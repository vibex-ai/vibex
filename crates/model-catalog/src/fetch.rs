//! Parsing one catalogue document.
//!
//! The upstream shape is a map of provider id to its models, and each model's
//! metadata is optional and loosely typed. Every field is read defensively: a
//! value of the wrong type is dropped rather than failing the whole document,
//! because a catalogue is advisory and one malformed entry must not cost the
//! user every other suggestion.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::suggest::{
    Catalog, CatalogModalities, CatalogModel, CatalogProvider, CatalogReasoningOption,
};

/// Why a catalogue could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogFetchError {
    #[error("the catalogue could not be reached")]
    Unreachable,
    #[error("the catalogue was not valid JSON")]
    InvalidJson,
    #[error("the catalogue had an unexpected shape")]
    InvalidShape,
}

/// The endpoint the catalogue is read from.
pub const CATALOG_URL: &str = "https://models.dev/api.json";

pub fn catalog_url() -> &'static str {
    CATALOG_URL
}

/// Parses one catalogue document into the shape the editor queries.
///
/// The document is a map of provider id to a provider object with a `models`
/// map. Anything else is an invalid shape; a single unusable model inside a
/// valid provider is skipped.
pub fn parse_catalog(body: &str) -> Result<Catalog, CatalogFetchError> {
    let value: Value = serde_json::from_str(body).map_err(|_| CatalogFetchError::InvalidJson)?;
    let providers = value.as_object().ok_or(CatalogFetchError::InvalidShape)?;
    let mut catalog = Catalog::default();
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        let provider_name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id.as_str());
        let mut entries = BTreeMap::new();
        for (model_id, model) in models {
            if let Some(entry) = read_model(model_id, model) {
                entries.insert(model_id.clone(), entry);
            }
        }
        if !entries.is_empty() {
            catalog.providers.insert(
                provider_id.clone(),
                CatalogProvider {
                    id: provider_id.clone(),
                    display_name: provider_name.to_string(),
                    api: string_field(provider, "api"),
                    doc: string_field(provider, "doc"),
                    env: string_list(provider.get("env")),
                    npm: string_field(provider, "npm"),
                    models: entries,
                },
            );
        }
    }
    Ok(catalog)
}

/// Reads an optional string field, dropping a value of any other type.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reads an optional token count, dropping a non-number or one that does not
/// fit the stored width.
fn token_count(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|tokens| u32::try_from(tokens).ok())
}

/// Reads a list of strings, dropping entries of any other type.
fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the `reasoning_options` array, dropping entries it cannot understand.
///
/// Each entry is kept in the catalogue's own tagged shape. A missing `min` on
/// a budget entry is retained as `None` rather than dropping the declaration,
/// because the control is still advertised; a non-numeric `min` is dropped.
fn read_reasoning_options(value: Option<&Value>) -> Option<Vec<CatalogReasoningOption>> {
    let options = value?.as_array()?;
    Some(options.iter().filter_map(read_reasoning_option).collect())
}

/// Reads one reasoning option, returning `None` for an unknown or malformed
/// entry so its siblings survive.
fn read_reasoning_option(value: &Value) -> Option<CatalogReasoningOption> {
    match value.get("type").and_then(Value::as_str)? {
        "toggle" => Some(CatalogReasoningOption::Toggle),
        "effort" => {
            let values = string_list(value.get("values"));
            // An effort control without any level says nothing a selector can
            // offer, so it is dropped rather than stored empty.
            (!values.is_empty()).then_some(CatalogReasoningOption::Effort { values })
        }
        "budget_tokens" => Some(CatalogReasoningOption::BudgetTokens {
            min: token_count(value.get("min")),
        }),
        _ => None,
    }
}

/// Reads one model entry, dropping fields that are missing or mistyped.
fn read_model(model_id: &str, model: &Value) -> Option<CatalogModel> {
    if !model.is_object() {
        return None;
    }
    let limit = model.get("limit").unwrap_or(&Value::Null);
    let modalities = model.get("modalities");
    let input_modalities = modalities.and_then(|modalities| modalities.get("input"));
    let output_modalities = modalities.and_then(|modalities| modalities.get("output"));
    Some(CatalogModel {
        id: model_id.to_string(),
        display_name: string_field(model, "name"),
        family: string_field(model, "family"),
        release_date: string_field(model, "release_date"),
        context_tokens: token_count(limit.get("context")),
        input_tokens: token_count(limit.get("input")),
        output_tokens: token_count(limit.get("output")),
        reasoning: model.get("reasoning").and_then(Value::as_bool),
        reasoning_options: read_reasoning_options(model.get("reasoning_options")),
        image_input: input_modalities
            .and_then(Value::as_array)
            .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("image"))),
        pdf_input: input_modalities
            .and_then(Value::as_array)
            .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("pdf"))),
        modalities: modalities
            .is_some_and(Value::is_object)
            .then(|| CatalogModalities {
                input: string_list(input_modalities),
                output: string_list(output_modalities),
            }),
        temperature: model.get("temperature").and_then(Value::as_bool),
        tool_call: model.get("tool_call").and_then(Value::as_bool),
        structured_output: model.get("structured_output").and_then(Value::as_bool),
        canonical_model_id: string_field(model, "canonical_model_id"),
        status: string_field(model, "status"),
        // `reasoning_options` is an advisory control list, not a declaration:
        // the editor may offer its known effort values, but the Agent's wire
        // spelling remains the user's explicit choice.
        reasoning_efforts: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> &'static str {
        r#"{
            "deepseek": {
                "id": "deepseek",
                "name": "DeepSeek",
                "api": "https://api.deepseek.com/v1",
                "doc": "https://api-docs.deepseek.com",
                "env": ["DEEPSEEK_API_KEY", 7],
                "npm": "@ai-sdk/deepseek",
                "models": {
                    "deepseek-chat": {
                        "name": "DeepSeek Chat",
                        "family": "deepseek",
                        "release_date": "2024-12-26",
                        "limit": {"context": 128000, "input": 64000, "output": 8192},
                        "reasoning": false,
                        "reasoning_options": [
                            {"type": "toggle"},
                            {"type": "effort", "values": ["low", "high"]},
                            {"type": "budget_tokens", "min": 1024}
                        ],
                        "tool_call": true,
                        "structured_output": true,
                        "temperature": true,
                        "status": "beta",
                        "canonical_model_id": "deepseek/deepseek-chat",
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "cost": {"input": 0.27, "output": 1.1}
                    },
                    "deepseek-reasoner": {
                        "limit": {"context": 64000},
                        "reasoning": true
                    },
                    "broken": "not an object"
                }
            },
            "empty": {"name": "Empty", "models": {}},
            "shapeless": "nope"
        }"#
    }

    #[test]
    fn a_document_is_read_into_providers_and_models() {
        let catalog = parse_catalog(document()).unwrap();
        assert_eq!(catalog.providers.len(), 1, "empty providers are dropped");
        let deepseek = catalog.provider("deepseek").unwrap();
        assert_eq!(deepseek.display_name, "DeepSeek");
        assert_eq!(deepseek.api.as_deref(), Some("https://api.deepseek.com/v1"));
        assert_eq!(
            deepseek.doc.as_deref(),
            Some("https://api-docs.deepseek.com")
        );
        assert_eq!(
            deepseek.env,
            vec!["DEEPSEEK_API_KEY".to_string()],
            "a non-string env entry is dropped"
        );
        assert_eq!(deepseek.npm.as_deref(), Some("@ai-sdk/deepseek"));
        assert_eq!(deepseek.models.len(), 2, "a malformed model is skipped");
        let chat = deepseek.model("deepseek-chat").unwrap();
        assert_eq!(chat.family.as_deref(), Some("deepseek"));
        assert_eq!(chat.release_date.as_deref(), Some("2024-12-26"));
        assert_eq!(chat.context_tokens, Some(128_000));
        assert_eq!(chat.input_tokens, Some(64_000));
        assert_eq!(chat.output_tokens, Some(8_192));
        assert_eq!(chat.reasoning, Some(false));
        assert_eq!(
            chat.reasoning_options,
            Some(vec![
                CatalogReasoningOption::Toggle,
                CatalogReasoningOption::Effort {
                    values: vec!["low".to_string(), "high".to_string()]
                },
                CatalogReasoningOption::BudgetTokens { min: Some(1_024) },
            ])
        );
        assert_eq!(chat.image_input, Some(true));
        assert_eq!(chat.pdf_input, Some(false));
        assert_eq!(
            chat.modalities,
            Some(CatalogModalities {
                input: vec!["text".to_string(), "image".to_string()],
                output: vec!["text".to_string()],
            })
        );
        assert_eq!(chat.temperature, Some(true));
        assert_eq!(chat.tool_call, Some(true));
        assert_eq!(chat.structured_output, Some(true));
        assert_eq!(
            chat.canonical_model_id.as_deref(),
            Some("deepseek/deepseek-chat")
        );
        assert_eq!(chat.status.as_deref(), Some("beta"));
        assert_eq!(
            deepseek.model("deepseek-reasoner").unwrap().context_tokens,
            Some(64_000)
        );
    }

    #[test]
    fn money_in_the_document_is_never_read_into_the_catalog() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{"cost":{"input":0.27,"output":1.1},"limit":{"context":1000}}}}}"#,
        )
        .unwrap();
        let encoded = serde_json::to_string(&catalog).unwrap().to_lowercase();
        for forbidden in ["cost", "price", "currency", "amount", "usd", "micros"] {
            assert!(
                !encoded.contains(forbidden),
                "a parsed catalog must not carry `{forbidden}`: {encoded}"
            );
        }
    }

    #[test]
    fn a_mistyped_field_is_dropped_rather_than_failing_the_document() {
        let catalog = parse_catalog(
            r#"{"p":{
                "api":7,"doc":[],"env":"KEY","npm":{},
                "models":{"m":{
                    "limit":{"context":"huge","input":-1,"output":123},
                    "reasoning":"yes",
                    "family":7,
                    "release_date":42,
                    "reasoning_options":{"type":"toggle"},
                    "modalities":{"input":"image","output":[1,"text"]},
                    "tool_call":"yes",
                    "structured_output":1,
                    "canonical_model_id":[],
                    "status":9
                }}
            }}"#,
        )
        .unwrap();
        let provider = catalog.provider("p").unwrap();
        assert_eq!(provider.api, None);
        assert_eq!(provider.doc, None);
        assert!(provider.env.is_empty(), "a non-array env is dropped");
        assert_eq!(provider.npm, None);
        let model = provider.model("m").unwrap();
        // A context that is not a number is absent, not zero.
        assert_eq!(model.context_tokens, None);
        assert_eq!(model.input_tokens, None);
        assert_eq!(model.output_tokens, Some(123));
        assert_eq!(model.reasoning, None);
        assert_eq!(model.family, None);
        assert_eq!(model.release_date, None);
        assert_eq!(model.reasoning_options, None);
        // A non-array modality list does not become an empty capability: the
        // field is dropped, exactly as before.
        assert_eq!(model.image_input, None);
        assert_eq!(
            model.modalities,
            Some(CatalogModalities {
                input: Vec::new(),
                output: vec!["text".to_string()],
            })
        );
        assert_eq!(model.tool_call, None);
        assert_eq!(model.structured_output, None);
        assert_eq!(model.canonical_model_id, None);
        assert_eq!(model.status, None);
    }

    #[test]
    fn reasoning_options_keep_their_wire_shape_and_drop_bad_entries() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{"reasoning_options":[
                {"type":"toggle"},
                {"type":"effort","values":["low",3,"high"]},
                {"type":"effort"},
                {"type":"budget_tokens","min":"1024"},
                {"type":"budget_tokens"},
                {"type":"unknown"},
                7
            ]}}}}"#,
        )
        .unwrap();
        let options = catalog
            .provider("p")
            .unwrap()
            .model("m")
            .unwrap()
            .reasoning_options
            .as_ref()
            .unwrap();
        assert_eq!(
            options,
            &vec![
                CatalogReasoningOption::Toggle,
                CatalogReasoningOption::Effort {
                    values: vec!["low".to_string(), "high".to_string()]
                },
                // The control is advertised even when its bound is mistyped or
                // absent; only the bound is dropped.
                CatalogReasoningOption::BudgetTokens { min: None },
                CatalogReasoningOption::BudgetTokens { min: None },
            ]
        );
        assert_eq!(
            serde_json::to_value(options).unwrap(),
            serde_json::json!([
                {"type": "toggle"},
                {"type": "effort", "values": ["low", "high"]},
                {"type": "budget_tokens"},
                {"type": "budget_tokens"}
            ])
        );
    }

    #[test]
    fn an_unusable_document_is_an_error_and_never_a_panic() {
        assert_eq!(
            parse_catalog("{").unwrap_err(),
            CatalogFetchError::InvalidJson
        );
        assert_eq!(
            parse_catalog("[]").unwrap_err(),
            CatalogFetchError::InvalidShape
        );
        assert_eq!(
            parse_catalog("null").unwrap_err(),
            CatalogFetchError::InvalidShape
        );
    }

    #[test]
    fn a_catalog_overflow_is_ignored_rather_than_truncated() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{"limit":{"context":99999999999999999999,"input":99999999999999999999}}}}}"#,
        )
        .unwrap();
        let model = catalog.provider("p").unwrap().model("m").unwrap();
        assert_eq!(model.context_tokens, None);
        assert_eq!(model.input_tokens, None);
    }
}
