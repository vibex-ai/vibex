//! Simple elicitation forms answered from chat.
//!
//! The Agent can stop and ask a question with a form. Most of those forms are
//! free text or several fields at once, and building a chat-side form for them
//! produces half-answers; those still send a notice that points at Vibex.
//!
//! Two shapes are simple enough to answer with one tap or one short line: a
//! boolean, and a text field restricted to a fixed list of options. This module
//! decides which shape a request has, renders it, and turns a tap back into a
//! resolution the core validator accepts.

use std::collections::BTreeMap;

use vibex_core::{
    ElicitationAnswerValue, ElicitationFieldKind, ElicitationRequest, ElicitationRequestStatus,
    ElicitationResolution, ElicitationResolutionAction,
};

use crate::i18n::ImLocale;

/// What a chat can do with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SimpleForm {
    /// One boolean field: yes / no.
    Boolean { field_id: String },
    /// One text field limited to a fixed option list.
    Choice {
        field_id: String,
        options: Vec<(String, String)>,
    },
}

impl SimpleForm {
    /// The fields a tap can fill.
    ///
    /// A request with more than one field is not simple: answering half of it
    /// in chat and the rest later is exactly the half-formed answer the design
    /// rules out. Every field must be either the single boolean or the single
    /// choice, and nothing may be `Unsupported`.
    pub(crate) fn of(request: &ElicitationRequest) -> Option<Self> {
        if request.status != ElicitationRequestStatus::Pending {
            return None;
        }
        let [field] = request.fields.as_slice() else {
            return None;
        };
        match &field.kind {
            ElicitationFieldKind::Boolean { .. } => Some(Self::Boolean {
                field_id: field.id.clone(),
            }),
            ElicitationFieldKind::Text { options, .. } if !options.is_empty() => {
                Some(Self::Choice {
                    field_id: field.id.clone(),
                    options: options
                        .iter()
                        .map(|option| (option.value.clone(), option.title.clone()))
                        .collect(),
                })
            }
            _ => None,
        }
    }
}

/// One tappable answer on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElicitationChoice {
    pub label: String,
    /// Serialized into the action payload.
    pub value: String,
}

/// A rendered question plus its answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ElicitationPrompt {
    pub text: String,
    pub choices: Vec<ElicitationChoice>,
}

/// Renders the question for a chat card.
pub(crate) fn prompt(
    request: &ElicitationRequest,
    form: &SimpleForm,
    locale: ImLocale,
) -> ElicitationPrompt {
    let mut text = match locale {
        ImLocale::En => "The Agent has a question:",
        ImLocale::ZhCn => "Agent 有一个问题：",
        ImLocale::ZhTw => "Agent 有一個問題：",
    }
    .to_string();
    text.push('\n');
    if let Some(title) = request
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
    {
        text.push_str(title.trim());
        text.push('\n');
    }
    text.push_str(request.message.trim());
    let choices = match form {
        SimpleForm::Boolean { .. } => vec![
            ElicitationChoice {
                label: match locale {
                    ImLocale::En => "Yes".to_string(),
                    ImLocale::ZhCn => "是".to_string(),
                    ImLocale::ZhTw => "是".to_string(),
                },
                value: "true".to_string(),
            },
            ElicitationChoice {
                label: match locale {
                    ImLocale::En => "No".to_string(),
                    ImLocale::ZhCn => "否".to_string(),
                    ImLocale::ZhTw => "否".to_string(),
                },
                value: "false".to_string(),
            },
        ],
        SimpleForm::Choice { options, .. } => options
            .iter()
            .map(|(value, title)| ElicitationChoice {
                label: title.clone(),
                value: value.clone(),
            })
            .collect(),
    };
    ElicitationPrompt { text, choices }
}

/// Builds the resolution for a tapped answer.
///
/// Declining and cancelling are the two answers that need no field at all, so
/// they are the same shape whatever the form was.
pub(crate) fn resolution_for_choice(
    request: &ElicitationRequest,
    form: &SimpleForm,
    value: &str,
) -> Option<ElicitationResolution> {
    let answer = match form {
        SimpleForm::Boolean { field_id } => {
            let parsed = match value {
                "true" | "yes" => true,
                "false" | "no" => false,
                _ => return None,
            };
            (field_id.clone(), ElicitationAnswerValue::Boolean(parsed))
        }
        SimpleForm::Choice { field_id, options } => {
            if !options.iter().any(|(candidate, _)| candidate == value) {
                return None;
            }
            (
                field_id.clone(),
                ElicitationAnswerValue::String(value.to_string()),
            )
        }
    };
    let mut answers = BTreeMap::new();
    answers.insert(answer.0, answer.1);
    Some(resolution(
        request,
        ElicitationResolutionAction::Accept,
        answers,
    ))
}

fn resolution(
    request: &ElicitationRequest,
    action: ElicitationResolutionAction,
    answers: BTreeMap<String, ElicitationAnswerValue>,
) -> ElicitationResolution {
    ElicitationResolution {
        request_id: request.id.clone(),
        session_id: request.session_id.clone(),
        action,
        answers,
        responder_device_id: None,
        resolved_at_ms: vibex_core::unix_timestamp_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{ElicitationField, ElicitationOption, RequestId, VibexSessionId};

    fn request(fields: Vec<ElicitationField>) -> ElicitationRequest {
        ElicitationRequest {
            id: RequestId::new(),
            session_id: VibexSessionId::new(),
            provider_request_id: None,
            tool_call_id: None,
            message: "Continue?".to_string(),
            title: Some("Confirm".to_string()),
            description: None,
            fields,
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: 1,
        }
    }

    fn boolean_field() -> ElicitationField {
        ElicitationField {
            id: "confirm".to_string(),
            title: "Confirm".to_string(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Boolean { default: None },
        }
    }

    fn choice_field() -> ElicitationField {
        ElicitationField {
            id: "target".to_string(),
            title: "Target".to_string(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: vec![
                    ElicitationOption {
                        value: "staging".to_string(),
                        title: "Staging".to_string(),
                        description: None,
                    },
                    ElicitationOption {
                        value: "production".to_string(),
                        title: "Production".to_string(),
                        description: None,
                    },
                ],
            },
        }
    }

    #[test]
    fn a_single_boolean_is_answerable_in_chat() {
        let request = request(vec![boolean_field()]);
        let form = SimpleForm::of(&request).expect("simple");
        let prompt = prompt(&request, &form, ImLocale::En);
        assert!(prompt.text.contains("Continue?"));
        assert_eq!(prompt.choices.len(), 2);
        let resolution = resolution_for_choice(&request, &form, "true").expect("yes");
        assert_eq!(resolution.action, ElicitationResolutionAction::Accept);
        assert_eq!(
            resolution.answers.get("confirm"),
            Some(&ElicitationAnswerValue::Boolean(true))
        );
        request.validate_resolution(&resolution).expect("valid");
    }

    #[test]
    fn a_single_choice_field_is_answerable_in_chat() {
        let request = request(vec![choice_field()]);
        let form = SimpleForm::of(&request).expect("simple");
        let prompt = prompt(&request, &form, ImLocale::ZhCn);
        assert_eq!(prompt.choices.len(), 2);
        assert!(prompt.text.contains("Agent 有一个问题"));
        let resolution = resolution_for_choice(&request, &form, "production").expect("choice");
        resolution_answers(&request, &resolution);
        assert_eq!(
            resolution.answers.get("target"),
            Some(&ElicitationAnswerValue::String("production".to_string()))
        );
        // A value that was not offered is refused rather than passed through.
        assert!(resolution_for_choice(&request, &form, "elsewhere").is_none());
    }

    #[test]
    fn multi_field_and_free_text_forms_stay_in_vibex() {
        assert!(SimpleForm::of(&request(vec![boolean_field(), choice_field()])).is_none());
        let free_text = ElicitationField {
            id: "notes".to_string(),
            title: "Notes".to_string(),
            description: None,
            required: false,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: Vec::new(),
            },
        };
        assert!(SimpleForm::of(&request(vec![free_text])).is_none());
        let numbers = ElicitationField {
            id: "count".to_string(),
            title: "Count".to_string(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Integer {
                minimum: None,
                maximum: None,
                default: None,
            },
        };
        assert!(SimpleForm::of(&request(vec![numbers])).is_none());
    }

    #[test]
    fn declining_needs_no_answers_and_validates() {
        use vibex_core::ElicitationResolutionAction;

        let request = request(vec![boolean_field()]);
        let resolution = resolution_for_choice(
            &request,
            &SimpleForm::Boolean {
                field_id: "confirm".to_string(),
            },
            "false",
        )
        .expect("no");
        assert_eq!(resolution.answers.len(), 1);
        // The core validator accepts a bare decline as well; the chat's own
        // path always carries the field answer, which is the stricter shape.
        let declined = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Decline,
            answers: BTreeMap::new(),
            responder_device_id: None,
            resolved_at_ms: 1,
        };
        request.validate_resolution(&declined).expect("valid");
    }

    fn resolution_answers(request: &ElicitationRequest, resolution: &ElicitationResolution) {
        request.validate_resolution(resolution).expect("valid");
    }
}
