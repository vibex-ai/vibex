//! Grok's blocking `_x.ai` host requests.
//!
//! Grok's native `ask_user_question` and `exit_plan_mode` tools do not run
//! through MCP or the standard permission channel: they issue a private
//! JSON-RPC **request** and block until the host answers. Replying
//! `method-not-found` is not a graceful downgrade here — the agent's own tool
//! call fails, and in the plan case the agent stays in plan mode with no way
//! out.
//!
//! Both map onto Vibex's existing elicitation surface, so they reuse the
//! pending-elicitation bookkeeping, the timeline card and the resolution API.
//! Only the wire encoding differs, which is why every pending elicitation
//! records the dialect that produced it.
//!
//! Wire formats (verified against observed shapes from real Grok runs, and
//! deliberately fail-soft: a malformed reply only makes Grok fall
//! back to its inert rendering, never worse than the pre-bridge behavior):
//!
//! - `_x.ai/ask_user_question` request: `{sessionId, toolCallId, questions:
//!   [{question, multiSelect, options: [{label, description}]}], mode}`.
//!   Response: `{outcome: "accepted", answers: {<question text>: string |
//!   string[]}, partial_answers: {}}`, or `{outcome: "skip_interview"}`.
//!   Answers are keyed by the **question text** because Grok's questions carry
//!   no id.
//! - `_x.ai/exit_plan_mode` request: `{sessionId, toolCallId, planContent}`.
//!   Response: `{outcome: "approved" | "keep_planning" | "abandoned",
//!   feedback}`. Anything that is neither `approved` nor `abandoned` keeps
//!   plan mode active, which is the correct behavior for a disconnect.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use vibex_core::{
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationOption,
    ElicitationRequest, ElicitationRequestStatus, ElicitationResolution,
    ElicitationResolutionAction, RequestId, VibexSessionId, unix_timestamp_ms,
};

const GROK_MAX_QUESTIONS: usize = 8;
const GROK_MAX_OPTIONS: usize = 24;
const GROK_MAX_TEXT_CHARS: usize = 2000;
const GROK_MIN_OPTIONS: usize = 2;

/// Field id of the single choice an `exit_plan_mode` approval carries.
pub(crate) const GROK_PLAN_DECISION_FIELD: &str = "outcome";

const GROK_PLAN_APPROVED: &str = "approved";
const GROK_PLAN_KEEP_PLANNING: &str = "keep_planning";
const GROK_PLAN_ABANDONED: &str = "abandoned";

/// Convert `_x.ai/ask_user_question` params into an elicitation form.
///
/// Each Grok question becomes one field. Single-select maps to a text field
/// constrained to its options, multi-select to a multi-select field. Returns
/// `None` for a payload with no usable question, so the caller can answer with
/// the skip outcome instead of parking a card the user cannot act on.
pub(crate) fn parse_grok_ask_user_question(
    params: &Value,
    request_id: RequestId,
    session_id: VibexSessionId,
) -> Option<ElicitationRequest> {
    let questions = params.get("questions").and_then(Value::as_array)?;
    let mut fields = Vec::new();
    for question in questions.iter().take(GROK_MAX_QUESTIONS) {
        let prompt = question
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let multi_select = question
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let options = grok_question_options(question.get("options"))?;
        let title = bounded(prompt, GROK_MAX_TEXT_CHARS);
        fields.push(ElicitationField {
            // Grok correlates answers by question text, so the field id is the
            // question itself rather than a synthetic key.
            id: title.clone(),
            title,
            description: None,
            required: true,
            kind: if multi_select {
                ElicitationFieldKind::MultiSelect {
                    options,
                    min_items: Some(1),
                    max_items: None,
                    default: Vec::new(),
                }
            } else {
                ElicitationFieldKind::Text {
                    min_length: None,
                    max_length: None,
                    pattern: None,
                    format: None,
                    default: None,
                    options,
                }
            },
        });
    }
    if fields.is_empty() {
        return None;
    }
    Some(ElicitationRequest {
        id: request_id.clone(),
        session_id,
        provider_request_id: Some(request_id.as_str().to_string()),
        tool_call_id: params
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_string),
        message: fields
            .first()
            .map(|field| field.title.clone())
            .unwrap_or_default(),
        title: Some("Grok needs an answer".to_string()),
        description: None,
        fields,
        status: ElicitationRequestStatus::Pending,
        requested_at_ms: unix_timestamp_ms(),
    })
}

fn grok_question_options(value: Option<&Value>) -> Option<Vec<ElicitationOption>> {
    let raw = value.and_then(Value::as_array)?;
    let mut options: Vec<ElicitationOption> = Vec::new();
    for option in raw {
        if options.len() == GROK_MAX_OPTIONS {
            break;
        }
        let label = option
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(label) = label else {
            continue;
        };
        let label = bounded(label, GROK_MAX_TEXT_CHARS);
        // The label is the selection identity on the wire, so a duplicate is
        // dropped rather than allowed to make the answer ambiguous.
        if options.iter().any(|existing| existing.value == label) {
            continue;
        }
        let description = option
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| bounded(value, GROK_MAX_TEXT_CHARS));
        options.push(ElicitationOption {
            value: label.clone(),
            title: label,
            description,
        });
    }
    (options.len() >= GROK_MIN_OPTIONS).then_some(options)
}

/// Encode a resolved ask-user-question card into Grok's response shape.
pub(crate) fn build_grok_ask_response(resolution: &ElicitationResolution) -> Value {
    if resolution.action != ElicitationResolutionAction::Accept {
        return grok_ask_skip_response();
    }
    let mut answers = serde_json::Map::new();
    for (question, answer) in &resolution.answers {
        let value = match answer {
            // Single-select answers stay bare strings: Grok's `StringOrVec`
            // accepts both, but a one-element array reads as multi-select.
            ElicitationAnswerValue::String(value) => Value::String(value.clone()),
            ElicitationAnswerValue::StringArray(values) => {
                Value::Array(values.iter().cloned().map(Value::String).collect())
            }
            ElicitationAnswerValue::Integer(value) => Value::String(value.to_string()),
            ElicitationAnswerValue::Number(value) => Value::String(value.clone()),
            ElicitationAnswerValue::Boolean(value) => Value::String(value.to_string()),
        };
        answers.insert(question.clone(), value);
    }
    if answers.is_empty() {
        return grok_ask_skip_response();
    }
    json!({
        "outcome": "accepted",
        "answers": Value::Object(answers),
        "partial_answers": Value::Object(serde_json::Map::new()),
    })
}

/// Reply for a declined card, or for one torn down before the user answered.
pub(crate) fn grok_ask_skip_response() -> Value {
    json!({ "outcome": "skip_interview" })
}

/// Convert `_x.ai/exit_plan_mode` params into a single-choice approval form.
pub(crate) fn parse_grok_exit_plan_mode(
    params: &Value,
    request_id: RequestId,
    session_id: VibexSessionId,
) -> Option<ElicitationRequest> {
    let plan = params
        .get("planContent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bounded(value, GROK_MAX_TEXT_CHARS))
        .unwrap_or_else(|| "Grok is ready to leave plan mode.".to_string());
    Some(ElicitationRequest {
        id: request_id.clone(),
        session_id,
        provider_request_id: Some(request_id.as_str().to_string()),
        tool_call_id: params
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_string),
        message: plan,
        title: Some("Approve Grok's plan".to_string()),
        description: None,
        fields: vec![ElicitationField {
            id: GROK_PLAN_DECISION_FIELD.to_string(),
            title: "Plan decision".to_string(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: Some(GROK_PLAN_APPROVED.to_string()),
                options: vec![
                    ElicitationOption {
                        value: GROK_PLAN_APPROVED.to_string(),
                        title: "Approve and start".to_string(),
                        description: None,
                    },
                    ElicitationOption {
                        value: GROK_PLAN_KEEP_PLANNING.to_string(),
                        title: "Keep planning".to_string(),
                        description: None,
                    },
                    ElicitationOption {
                        value: GROK_PLAN_ABANDONED.to_string(),
                        title: "Abandon the plan".to_string(),
                        description: None,
                    },
                ],
            },
        }],
        status: ElicitationRequestStatus::Pending,
        requested_at_ms: unix_timestamp_ms(),
    })
}

/// Encode a plan decision. Anything unrecognized keeps plan mode active, which
/// is the only safe default: `approved` and `abandoned` both leave plan mode.
pub(crate) fn build_grok_exit_plan_response(resolution: &ElicitationResolution) -> Value {
    if resolution.action != ElicitationResolutionAction::Accept {
        return grok_exit_plan_keep_planning_response();
    }
    let decision = match resolution.answers.get(GROK_PLAN_DECISION_FIELD) {
        Some(ElicitationAnswerValue::String(value)) => value.as_str(),
        _ => GROK_PLAN_KEEP_PLANNING,
    };
    let outcome = match decision {
        GROK_PLAN_APPROVED => GROK_PLAN_APPROVED,
        GROK_PLAN_ABANDONED => GROK_PLAN_ABANDONED,
        _ => GROK_PLAN_KEEP_PLANNING,
    };
    json!({ "outcome": outcome, "feedback": grok_plan_feedback(&resolution.answers) })
}

/// The reply for a connection torn down mid-approval. Mirrors Grok's own
/// "client disconnected" behavior: keep plan mode active so the approval is
/// re-surfaced instead of silently proceeding as if approved.
pub(crate) fn grok_exit_plan_keep_planning_response() -> Value {
    json!({ "outcome": GROK_PLAN_KEEP_PLANNING, "feedback": "" })
}

fn grok_plan_feedback(answers: &BTreeMap<String, ElicitationAnswerValue>) -> String {
    match answers.get("feedback") {
        Some(ElicitationAnswerValue::String(value)) => bounded(value, GROK_MAX_TEXT_CHARS),
        _ => String::new(),
    }
}

fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolution(
        action: ElicitationResolutionAction,
        answers: Vec<(&str, ElicitationAnswerValue)>,
    ) -> ElicitationResolution {
        ElicitationResolution {
            request_id: RequestId::new(),
            session_id: VibexSessionId::new(),
            action,
            answers: answers
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            responder_device_id: None,
            resolved_at_ms: 0,
        }
    }

    #[test]
    fn ask_user_question_becomes_one_field_per_question() {
        let request = parse_grok_ask_user_question(
            &json!({
                "sessionId": "s1",
                "toolCallId": "call-1",
                "questions": [
                    {
                        "question": "Which database?",
                        "options": [
                            {"label": "Postgres", "description": "relational"},
                            {"label": "SQLite"},
                            {"label": "Postgres"}
                        ]
                    },
                    {
                        "question": "Which features?",
                        "multiSelect": true,
                        "options": [{"label": "auth"}, {"label": "billing"}]
                    }
                ]
            }),
            RequestId::new(),
            VibexSessionId::new(),
        )
        .expect("payload has usable questions");

        assert_eq!(request.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(request.fields.len(), 2);
        // Grok keys answers by question text, so the field id must be it.
        assert_eq!(request.fields[0].id, "Which database?");
        match &request.fields[0].kind {
            ElicitationFieldKind::Text { options, .. } => {
                // The duplicate label is dropped, not allowed to make the
                // selection ambiguous.
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].value, "Postgres");
                assert_eq!(options[0].description.as_deref(), Some("relational"));
            }
            other => panic!("single select must be a text field: {other:?}"),
        }
        assert!(matches!(
            request.fields[1].kind,
            ElicitationFieldKind::MultiSelect { .. }
        ));
    }

    #[test]
    fn unusable_ask_payloads_are_rejected_so_the_agent_gets_the_skip_outcome() {
        let id = || (RequestId::new(), VibexSessionId::new());
        for params in [
            json!({}),
            json!({ "questions": [] }),
            // A single option cannot be a choice.
            json!({ "questions": [{ "question": "?", "options": [{"label": "only"}] }] }),
            json!({ "questions": [{ "question": "   ", "options": [] }] }),
        ] {
            let (request_id, session_id) = id();
            assert!(
                parse_grok_ask_user_question(&params, request_id, session_id).is_none(),
                "expected rejection for {params}"
            );
        }
        assert_eq!(
            grok_ask_skip_response(),
            json!({"outcome": "skip_interview"})
        );
    }

    #[test]
    fn ask_answers_encode_single_and_multi_select_distinctly() {
        let accepted = build_grok_ask_response(&resolution(
            ElicitationResolutionAction::Accept,
            vec![
                (
                    "Which database?",
                    ElicitationAnswerValue::String("Postgres".to_string()),
                ),
                (
                    "Which features?",
                    ElicitationAnswerValue::StringArray(vec![
                        "auth".to_string(),
                        "billing".to_string(),
                    ]),
                ),
            ],
        ));
        assert_eq!(accepted["outcome"], json!("accepted"));
        assert_eq!(accepted["answers"]["Which database?"], json!("Postgres"));
        assert_eq!(
            accepted["answers"]["Which features?"],
            json!(["auth", "billing"])
        );
        assert_eq!(accepted["partial_answers"], json!({}));

        for action in [
            ElicitationResolutionAction::Decline,
            ElicitationResolutionAction::Cancel,
        ] {
            assert_eq!(
                build_grok_ask_response(&resolution(action, Vec::new())),
                grok_ask_skip_response()
            );
        }
        // An accept with nothing selected must not claim an answer.
        assert_eq!(
            build_grok_ask_response(&resolution(ElicitationResolutionAction::Accept, Vec::new())),
            grok_ask_skip_response()
        );
    }

    #[test]
    fn exit_plan_mode_defaults_to_keeping_plan_mode_active() {
        let request = parse_grok_exit_plan_mode(
            &json!({"toolCallId": "call-2", "planContent": "1. do the thing"}),
            RequestId::new(),
            VibexSessionId::new(),
        )
        .expect("plan approvals are always actionable");
        assert_eq!(request.message, "1. do the thing");
        assert_eq!(request.fields.len(), 1);
        assert_eq!(request.fields[0].id, GROK_PLAN_DECISION_FIELD);

        let approved = build_grok_exit_plan_response(&resolution(
            ElicitationResolutionAction::Accept,
            vec![(
                GROK_PLAN_DECISION_FIELD,
                ElicitationAnswerValue::String("approved".to_string()),
            )],
        ));
        assert_eq!(approved["outcome"], json!("approved"));

        // Decline, cancel, an unknown decision and a missing answer all keep
        // plan mode active: only `approved` / `abandoned` leave it.
        for resolved in [
            resolution(ElicitationResolutionAction::Decline, Vec::new()),
            resolution(ElicitationResolutionAction::Cancel, Vec::new()),
            resolution(ElicitationResolutionAction::Accept, Vec::new()),
            resolution(
                ElicitationResolutionAction::Accept,
                vec![(
                    GROK_PLAN_DECISION_FIELD,
                    ElicitationAnswerValue::String("something-else".to_string()),
                )],
            ),
        ] {
            assert_eq!(
                build_grok_exit_plan_response(&resolved)["outcome"],
                json!("keep_planning")
            );
        }
        assert_eq!(
            grok_exit_plan_keep_planning_response()["outcome"],
            json!("keep_planning")
        );
    }
}
