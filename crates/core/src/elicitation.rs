use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, RequestId, VibexSessionId};
use crate::{VibexError, VibexResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationRequestStatus {
    Pending,
    Accepted,
    Declined,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationResolutionAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ElicitationStringFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationOption {
    pub value: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ElicitationFieldKind {
    Text {
        min_length: Option<u32>,
        max_length: Option<u32>,
        pattern: Option<String>,
        format: Option<ElicitationStringFormat>,
        default: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ElicitationOption>,
    },
    Number {
        minimum: Option<String>,
        maximum: Option<String>,
        default: Option<String>,
    },
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
        default: Option<i64>,
    },
    Boolean {
        default: Option<bool>,
    },
    MultiSelect {
        options: Vec<ElicitationOption>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        default: Vec<String>,
    },
    Unsupported {
        schema_type: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationField {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: ElicitationFieldKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ElicitationAnswerValue {
    String(String),
    Integer(i64),
    /// Canonical finite decimal representation. ACP conversion parses this at
    /// the adapter boundary so the durable core model can retain `Eq`.
    Number(String),
    Boolean(bool),
    StringArray(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationRequest {
    pub id: RequestId,
    pub session_id: VibexSessionId,
    pub provider_request_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub message: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<ElicitationField>,
    pub status: ElicitationRequestStatus,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationResolution {
    pub request_id: RequestId,
    pub session_id: VibexSessionId,
    pub action: ElicitationResolutionAction,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, ElicitationAnswerValue>,
    pub responder_device_id: Option<DeviceId>,
    pub resolved_at_ms: i64,
}

impl ElicitationRequest {
    pub fn validate_resolution(&self, resolution: &ElicitationResolution) -> VibexResult<()> {
        if resolution.request_id != self.id || resolution.session_id != self.session_id {
            return Err(VibexError::validation(
                "elicitation_resolution_target_mismatch",
                "elicitation resolution must match the target session and request id",
            ));
        }
        if self.status != ElicitationRequestStatus::Pending {
            return Err(VibexError::validation(
                "elicitation_request_not_pending",
                "elicitation request is no longer pending",
            ));
        }
        if resolution.action != ElicitationResolutionAction::Accept {
            if !resolution.answers.is_empty() {
                return Err(VibexError::validation(
                    "elicitation_non_accept_answers",
                    "declined or cancelled elicitations cannot include answers",
                ));
            }
            return Ok(());
        }

        let fields = self
            .fields
            .iter()
            .map(|field| (field.id.as_str(), field))
            .collect::<BTreeMap<_, _>>();
        for answer_id in resolution.answers.keys() {
            if !fields.contains_key(answer_id.as_str()) {
                return Err(field_error(answer_id, "answer targets an unknown field"));
            }
        }
        for field in &self.fields {
            let answer = resolution.answers.get(&field.id);
            if answer.is_none() && field.required {
                return Err(field_error(&field.id, "required field is missing"));
            }
            if let Some(answer) = answer {
                validate_answer(field, answer)?;
            }
        }
        Ok(())
    }
}

fn validate_answer(field: &ElicitationField, answer: &ElicitationAnswerValue) -> VibexResult<()> {
    match (&field.kind, answer) {
        (
            ElicitationFieldKind::Text {
                min_length,
                max_length,
                pattern,
                options,
                ..
            },
            ElicitationAnswerValue::String(value),
        ) => {
            if pattern.is_some() {
                return Err(field_error(
                    &field.id,
                    "pattern-constrained text fields are not supported",
                ));
            }
            let length = value.chars().count() as u64;
            if min_length.is_some_and(|minimum| length < u64::from(minimum)) {
                return Err(field_error(
                    &field.id,
                    "answer is shorter than the minimum length",
                ));
            }
            if max_length.is_some_and(|maximum| length > u64::from(maximum)) {
                return Err(field_error(&field.id, "answer exceeds the maximum length"));
            }
            if !options.is_empty() && !options.iter().any(|option| option.value == *value) {
                return Err(field_error(&field.id, "answer is not an available option"));
            }
        }
        (
            ElicitationFieldKind::Number {
                minimum, maximum, ..
            },
            ElicitationAnswerValue::Number(value),
        ) => {
            let parsed = finite_number(value)
                .ok_or_else(|| field_error(&field.id, "answer is not a finite number"))?;
            if minimum
                .as_deref()
                .and_then(finite_number)
                .is_some_and(|minimum| parsed < minimum)
            {
                return Err(field_error(&field.id, "answer is below the minimum"));
            }
            if maximum
                .as_deref()
                .and_then(finite_number)
                .is_some_and(|maximum| parsed > maximum)
            {
                return Err(field_error(&field.id, "answer exceeds the maximum"));
            }
        }
        (
            ElicitationFieldKind::Integer {
                minimum, maximum, ..
            },
            ElicitationAnswerValue::Integer(value),
        ) => {
            if minimum.is_some_and(|minimum| *value < minimum) {
                return Err(field_error(&field.id, "answer is below the minimum"));
            }
            if maximum.is_some_and(|maximum| *value > maximum) {
                return Err(field_error(&field.id, "answer exceeds the maximum"));
            }
        }
        (ElicitationFieldKind::Boolean { .. }, ElicitationAnswerValue::Boolean(_)) => {}
        (
            ElicitationFieldKind::MultiSelect {
                options,
                min_items,
                max_items,
                ..
            },
            ElicitationAnswerValue::StringArray(values),
        ) => {
            let unique = values.iter().collect::<BTreeSet<_>>();
            if unique.len() != values.len() {
                return Err(field_error(&field.id, "answer contains duplicate options"));
            }
            if min_items.is_some_and(|minimum| (values.len() as u64) < minimum) {
                return Err(field_error(
                    &field.id,
                    "answer has too few selected options",
                ));
            }
            if max_items.is_some_and(|maximum| values.len() as u64 > maximum) {
                return Err(field_error(
                    &field.id,
                    "answer has too many selected options",
                ));
            }
            if values
                .iter()
                .any(|value| !options.iter().any(|option| option.value == *value))
            {
                return Err(field_error(
                    &field.id,
                    "answer contains an unavailable option",
                ));
            }
        }
        (ElicitationFieldKind::Unsupported { .. }, _) => {
            return Err(field_error(&field.id, "field type is not supported"));
        }
        _ => {
            return Err(field_error(
                &field.id,
                "answer type does not match the field",
            ));
        }
    }
    Ok(())
}

fn finite_number(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn field_error(field_id: &str, message: &str) -> VibexError {
    VibexError::validation("elicitation_answer_invalid", message)
        .with_diagnostic("fieldId", field_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_field(field: ElicitationField) -> ElicitationRequest {
        ElicitationRequest {
            id: RequestId::new(),
            session_id: VibexSessionId::new(),
            provider_request_id: None,
            tool_call_id: None,
            message: "Provide input".into(),
            title: None,
            description: None,
            fields: vec![field],
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: 1,
        }
    }

    #[test]
    fn validates_required_and_option_answers() {
        let request = request_with_field(ElicitationField {
            id: "choice".into(),
            title: "Choice".into(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: vec![ElicitationOption {
                    value: "a".into(),
                    title: "A".into(),
                    description: None,
                }],
            },
        });
        let mut resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers: BTreeMap::new(),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        assert!(request.validate_resolution(&resolution).is_err());
        resolution
            .answers
            .insert("choice".into(), ElicitationAnswerValue::String("a".into()));
        assert!(request.validate_resolution(&resolution).is_ok());
        resolution
            .answers
            .insert("choice".into(), ElicitationAnswerValue::String("b".into()));
        assert!(request.validate_resolution(&resolution).is_err());
    }

    #[test]
    fn rejects_mismatched_targets_and_non_accept_answers() {
        let request = request_with_field(ElicitationField {
            id: "name".into(),
            title: "Name".into(),
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
        });
        let mut resolution = ElicitationResolution {
            request_id: RequestId::new(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Decline,
            answers: BTreeMap::new(),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        assert_eq!(
            request.validate_resolution(&resolution).unwrap_err().code,
            "elicitation_resolution_target_mismatch"
        );

        resolution.request_id = request.id.clone();
        resolution
            .answers
            .insert("name".into(), ElicitationAnswerValue::String("Ada".into()));
        assert_eq!(
            request.validate_resolution(&resolution).unwrap_err().code,
            "elicitation_non_accept_answers"
        );
    }

    #[test]
    fn validates_numeric_and_multi_select_constraints() {
        let request = ElicitationRequest {
            fields: vec![
                ElicitationField {
                    id: "rating".into(),
                    title: "Rating".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::Number {
                        minimum: Some("1".into()),
                        maximum: Some("5".into()),
                        default: None,
                    },
                },
                ElicitationField {
                    id: "tags".into(),
                    title: "Tags".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::MultiSelect {
                        options: vec![
                            ElicitationOption {
                                value: "rust".into(),
                                title: "Rust".into(),
                                description: None,
                            },
                            ElicitationOption {
                                value: "ui".into(),
                                title: "UI".into(),
                                description: None,
                            },
                        ],
                        min_items: Some(1),
                        max_items: Some(2),
                        default: Vec::new(),
                    },
                },
            ],
            ..request_with_field(ElicitationField {
                id: "unused".into(),
                title: "Unused".into(),
                description: None,
                required: false,
                kind: ElicitationFieldKind::Boolean { default: None },
            })
        };
        let mut resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers: BTreeMap::from([
                (
                    "rating".into(),
                    ElicitationAnswerValue::Number("4.5".into()),
                ),
                (
                    "tags".into(),
                    ElicitationAnswerValue::StringArray(vec!["rust".into()]),
                ),
            ]),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        assert!(request.validate_resolution(&resolution).is_ok());

        resolution.answers.insert(
            "rating".into(),
            ElicitationAnswerValue::Number("NaN".into()),
        );
        assert_eq!(
            request.validate_resolution(&resolution).unwrap_err().code,
            "elicitation_answer_invalid"
        );
        resolution
            .answers
            .insert("rating".into(), ElicitationAnswerValue::Number("4".into()));
        resolution.answers.insert(
            "tags".into(),
            ElicitationAnswerValue::StringArray(vec!["rust".into(), "rust".into()]),
        );
        assert_eq!(
            request.validate_resolution(&resolution).unwrap_err().code,
            "elicitation_answer_invalid"
        );
    }

    #[test]
    fn rejects_pattern_constrained_text_without_assuming_regex_dialect() {
        let request = request_with_field(ElicitationField {
            id: "code".into(),
            title: "Code".into(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: Some("^[A-Z]+$".into()),
                format: None,
                default: None,
                options: Vec::new(),
            },
        });
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers: BTreeMap::from([(
                "code".into(),
                ElicitationAnswerValue::String("ABC".into()),
            )]),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        assert_eq!(
            request.validate_resolution(&resolution).unwrap_err().code,
            "elicitation_answer_invalid"
        );
    }
}
