//! The query surface the editor uses.

use std::collections::BTreeMap;

use vibex_core::{
    ModelCapabilitySource, ProviderModelCapabilities, ProviderModelReasoningEfforts,
    ProviderReasoningEffortLevel,
};

/// One model's catalogue entry.
///
/// Prices are absent by design: the product reports tokens, and a price from a
/// catalogue would be an estimate presented as a fact. The catalogue's `cost`
/// object is not even parsed, so a stored entry cannot acquire one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub display_name: Option<String>,
    /// The vendor's model family, such as `claude` or `gpt`.
    pub family: Option<String>,
    /// When the vendor published the model, as the catalogue spells it.
    pub release_date: Option<String>,
    pub context_tokens: Option<u32>,
    /// Prompt tokens accepted at once, when the catalogue publishes it apart
    /// from the context window.
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub reasoning: Option<bool>,
    /// The reasoning controls the model offers, kept in the catalogue's own
    /// vocabulary rather than translated into Agent levels.
    pub reasoning_options: Option<Vec<CatalogReasoningOption>>,
    pub image_input: Option<bool>,
    pub pdf_input: Option<bool>,
    /// The raw modality lists, so a surface can show every modality rather
    /// than only the two capabilities this crate projects.
    pub modalities: Option<CatalogModalities>,
    pub temperature: Option<bool>,
    pub tool_call: Option<bool>,
    pub structured_output: Option<bool>,
    /// The vendor-independent id the catalogue maps a resold or aliased model
    /// to.
    pub canonical_model_id: Option<String>,
    /// The vendor's lifecycle status, such as `beta` or `deprecated`.
    pub status: Option<String>,
    pub reasoning_efforts: Option<ProviderModelReasoningEfforts>,
}

impl CatalogModel {
    /// Whether a Provider should start out carrying this model.
    ///
    /// Three answers decide it, and each of them is data the catalogue
    /// publishes rather than a guess about the id:
    ///
    /// - `deprecated` is a retired model. A new Provider offering one would be
    ///   offering an id its vendor has already said it is winding down.
    /// - The model has to answer with text, because a Provider is configured
    ///   for an Agent session. Speech, image and video endpoints are skipped.
    /// - It has to announce tool calling; an Agent that cannot call a tool
    ///   cannot run, so such a model is not a candidate however good its chat
    ///   answers are.
    ///
    /// An entry that publishes no modality list keeps the benefit of the
    /// doubt: older and third-party catalogue entries omit it, and treating an
    /// omission as "not text" would hide real models.
    pub fn is_offered(&self) -> bool {
        if self
            .status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("deprecated"))
        {
            return false;
        }
        if let Some(modalities) = self.modalities.as_ref()
            && !modalities.output.is_empty()
            && !modalities.output.iter().any(|output| output == "text")
        {
            return false;
        }
        !matches!(self.tool_call, Some(false))
    }
}

/// The modality lists a catalogue model advertises.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CatalogModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// One reasoning control a model advertises.
///
/// The shapes mirror the catalogue's own tagged objects: `{"type":"toggle"}`,
/// `{"type":"effort","values":[...]}`, and
/// `{"type":"budget_tokens","min":N}`. They are not Agent effort levels; the
/// catalogue does not publish the Agent's vocabulary, so nothing is mapped.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CatalogReasoningOption {
    Toggle,
    Effort {
        values: Vec<String>,
    },
    BudgetTokens {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<u32>,
    },
}

/// One provider's entries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    pub id: String,
    pub display_name: String,
    /// The provider's OpenAI-compatible base URL, when the catalogue lists one.
    pub api: Option<String>,
    /// The provider's documentation URL.
    pub doc: Option<String>,
    /// Environment variables the provider's SDK reads.
    #[serde(default)]
    pub env: Vec<String>,
    /// The npm package the vendor's own SDK ships as.
    pub npm: Option<String>,
    pub models: BTreeMap<String, CatalogModel>,
}

impl CatalogProvider {
    pub fn model(&self, model_id: &str) -> Option<&CatalogModel> {
        self.models.get(model_id)
    }
}

/// The whole catalogue, indexed by provider id.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub providers: BTreeMap<String, CatalogProvider>,
}

impl Catalog {
    pub fn provider(&self, provider_id: &str) -> Option<&CatalogProvider> {
        self.providers.get(provider_id)
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&CatalogModel> {
        self.provider(provider_id)?.model(model_id)
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// How a match was made, so the editor can be more cautious about a guess.
    pub fn suggestion(&self, provider_id: &str, model_id: &str) -> Option<CapabilitySuggestion> {
        if let Some(model) = self.model(provider_id, model_id) {
            return Some(CapabilitySuggestion {
                model: model.clone(),
                source: SuggestSource::Exact,
            });
        }
        // A model id is sometimes spelled with a variant suffix (`-thinking`,
        // `:free`) that the catalogue does not carry. Falling back to the
        // longest catalogue id that prefixes it recovers those without ever
        // reading a number out of the id itself.
        let provider = self.provider(provider_id)?;
        let mut best: Option<(&String, &CatalogModel)> = None;
        for (candidate, model) in &provider.models {
            let matches = model_id.starts_with(candidate.as_str())
                && model_id
                    .as_bytes()
                    .get(candidate.len())
                    .is_some_and(|byte| matches!(byte, b'-' | b':' | b'/' | b'@'));
            if matches && best.is_none_or(|(existing, _)| candidate.len() > existing.len()) {
                best = Some((candidate, model));
            }
        }
        best.map(|(_, model)| CapabilitySuggestion {
            model: model.clone(),
            source: SuggestSource::Approximate,
        })
    }

    /// The vendor-independent id a model is known by, when the catalogue names
    /// one.
    ///
    /// This is the only cross-Provider identity the catalogue publishes, so it
    /// is what a "same model" suggestion groups by. An unknown model is
    /// `None`, never a guess built from the id's spelling.
    pub fn canonical_model_id(&self, provider_id: &str, model_id: &str) -> Option<String> {
        self.suggestion(provider_id, model_id)
            .and_then(|suggestion| suggestion.model.canonical_model_id)
    }

    /// The models a Provider can actually be offered, in catalogue order.
    ///
    /// This is the list a new Provider starts from, so it cannot be a vendor's
    /// inventory: the catalogue files speech, image and video endpoints under
    /// the same provider, and offering `whisper-large-v3` beside `gpt-5` would
    /// make every picker a catalogue dump. A model qualifies when it answers
    /// with text and announces tool calling, because that is what an Agent
    /// session needs; one the vendor marks `deprecated` is left out as well,
    /// since a retired id is not a model a Provider should start carrying.
    ///
    /// An unknown provider answers empty rather than defaulted, so a caller can
    /// tell "the catalogue knows nothing" from "the vendor serves nothing".
    pub fn offered_models(&self, provider_id: &str) -> Vec<&CatalogModel> {
        let Some(provider) = self.provider(provider_id) else {
            return Vec::new();
        };
        provider
            .models
            .values()
            .filter(|model| model.is_offered())
            .collect()
    }
}

/// How a suggestion was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestSource {
    /// The catalogue carries this exact provider and model id.
    Exact,
    /// The catalogue carries a longer id this one extends.
    Approximate,
}

impl SuggestSource {
    pub const fn is_approximate(self) -> bool {
        matches!(self, Self::Approximate)
    }
}

/// What the catalogue suggests for one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySuggestion {
    pub model: CatalogModel,
    pub source: SuggestSource,
}

impl CapabilitySuggestion {
    /// The fields this suggestion can fill, as `(field, value)` pairs.
    ///
    /// `reasoningEfforts` is included only when the catalogue publishes an
    /// `effort` control, and only for values the Agent vocabulary names. It is
    /// an explicit suggestion, not an applied capability: the catalogue does
    /// not publish the Agent's wire spelling, so `adopt_all` leaves the level
    /// table alone and the editor applies it through its own control.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();
        if let Some(context) = self.model.context_tokens {
            fields.push(("contextTokens", context.to_string()));
        }
        if let Some(output) = self.model.output_tokens {
            fields.push(("outputTokens", output.to_string()));
        }
        if let Some(levels) = self.reasoning_efforts_suggestion() {
            fields.push(("reasoningEfforts", levels));
        }
        for (field, value) in [
            ("reasoning", self.model.reasoning),
            ("imageInput", self.model.image_input),
            ("pdfInput", self.model.pdf_input),
            ("temperature", self.model.temperature),
        ] {
            if let Some(value) = value {
                fields.push((field, value.to_string()));
            }
        }
        fields
    }

    /// The catalogue's effort values as a compact thinking-level declaration.
    ///
    /// The catalogue publishes values such as `low`, `high`, or `max`; values
    /// outside the Agent's vocabulary are dropped rather than guessed, and the
    /// `toggle` / `budget_tokens` controls are not level tables and suggest
    /// nothing. The result is a suggestion the editor offers under the Thinking
    /// levels control, never an applied declaration.
    pub fn reasoning_efforts_suggestion(&self) -> Option<String> {
        let options = self.model.reasoning_options.as_ref()?;
        let mut levels = Vec::new();
        for option in options {
            let CatalogReasoningOption::Effort { values } = option else {
                continue;
            };
            for value in values {
                let Some(level) = ProviderReasoningEffortLevel::parse(value) else {
                    continue;
                };
                if level != ProviderReasoningEffortLevel::Off {
                    levels.push(level);
                }
            }
        }
        levels.sort();
        levels.dedup();
        if levels.is_empty() {
            return None;
        }
        Some(
            levels
                .iter()
                .map(|level| level.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// The capabilities a user gets by adopting every scalar suggested field.
    ///
    /// The thinking-level suggestion is deliberately absent: it is
    /// Agent-specific, and the editor applies it only through the Thinking
    /// levels control. The result carries `Catalog` provenance for each
    /// adopted field, so a later projection can tell it apart from a
    /// hand-typed value.
    pub fn adopt_all(&self) -> ProviderModelCapabilities {
        let mut capabilities = ProviderModelCapabilities::default();
        if let Some(context) = self.model.context_tokens {
            capabilities.context_tokens = Some(context);
            capabilities.set_source("contextTokens", ModelCapabilitySource::Catalog);
        }
        if let Some(output) = self.model.output_tokens {
            capabilities.output_tokens = Some(output);
            capabilities.set_source("outputTokens", ModelCapabilitySource::Catalog);
        }
        for (field, value) in [
            ("reasoning", self.model.reasoning),
            ("imageInput", self.model.image_input),
            ("pdfInput", self.model.pdf_input),
            ("temperature", self.model.temperature),
        ] {
            if let Some(value) = value {
                match field {
                    "reasoning" => capabilities.reasoning = Some(value),
                    "imageInput" => capabilities.image_input = Some(value),
                    "pdfInput" => capabilities.pdf_input = Some(value),
                    "temperature" => capabilities.temperature = Some(value),
                    _ => {}
                }
                capabilities.set_source(field, ModelCapabilitySource::Catalog);
            }
        }
        capabilities
    }
}

/// Builds the wire shape for tests.
#[cfg(test)]
pub(crate) fn wire_suggestion_for_test(
    suggestion: &CapabilitySuggestion,
) -> vibex_core::GatewayCapabilitySuggestion {
    vibex_core::GatewayCapabilitySuggestion {
        approximate: suggestion.source.is_approximate(),
        fields: suggestion
            .fields()
            .into_iter()
            .map(|(field, value)| vibex_core::GatewayCapabilityField {
                field: field.to_string(),
                value,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::parse_catalog;

    fn catalog() -> Catalog {
        parse_catalog(
            r#"{
                "deepseek": {"name":"DeepSeek","models":{
                    "deepseek-chat":{"limit":{"context":128000,"output":8192},"reasoning":false,"temperature":true},
                    "deepseek-reasoner":{"limit":{"context":64000},"reasoning":true,"reasoning_options":[{"type":"effort","values":["low","high","ultra"]}]}
                }}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn an_exact_match_is_marked_exact() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        assert_eq!(suggestion.source, SuggestSource::Exact);
        assert_eq!(suggestion.model.context_tokens, Some(128_000));
    }

    #[test]
    fn a_suffixed_id_falls_back_to_the_longest_prefix() {
        let suggestion = catalog()
            .suggestion("deepseek", "deepseek-chat-free")
            .unwrap();
        assert_eq!(suggestion.source, SuggestSource::Approximate);
        assert!(suggestion.source.is_approximate());
        assert_eq!(suggestion.model.context_tokens, Some(128_000));
        // A prefix that is not delimited does not match.
        assert!(
            catalog()
                .suggestion("deepseek", "deepseek-chatty")
                .is_none()
        );
        // An unknown provider suggests nothing at all.
        assert!(catalog().suggestion("nowhere", "deepseek-chat").is_none());
    }

    #[test]
    fn adopting_every_field_records_catalog_provenance() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        let capabilities = suggestion.adopt_all();
        assert_eq!(capabilities.context_tokens, Some(128_000));
        assert_eq!(capabilities.output_tokens, Some(8_192));
        assert_eq!(capabilities.reasoning, Some(false));
        assert_eq!(capabilities.temperature, Some(true));
        assert_eq!(
            capabilities.adopted_from_catalog().collect::<Vec<_>>(),
            vec!["contextTokens", "outputTokens", "reasoning", "temperature"]
        );
    }

    #[test]
    fn a_hand_typed_value_is_not_overwritten_and_loses_its_catalog_mark() {
        let suggestion = catalog().suggestion("deepseek", "deepseek-chat").unwrap();
        let mut capabilities = suggestion.adopt_all();
        // The user edits the context window themselves.
        capabilities.context_tokens = Some(64_000);
        capabilities.set_source("contextTokens", ModelCapabilitySource::Declared);
        assert!(
            !capabilities
                .adopted_from_catalog()
                .any(|field| field == "contextTokens")
        );
        assert_eq!(capabilities.context_tokens, Some(64_000));
        // A field with no suggestion is not adopted, and nothing else moves.
        let wire = wire_suggestion_for_test(&suggestion);
        let mut untouched = capabilities.clone();
        assert!(!wire.adopt(&mut untouched, "reasoningEfforts"));
        assert_eq!(untouched, capabilities);
    }

    #[test]
    fn reasoning_efforts_are_suggested_but_never_adopted_automatically() {
        let suggestion = catalog()
            .suggestion("deepseek", "deepseek-reasoner")
            .unwrap();
        assert_eq!(
            suggestion.reasoning_efforts_suggestion().as_deref(),
            Some("low, high"),
            "unknown catalogue levels are dropped, not guessed"
        );
        assert!(
            suggestion
                .fields()
                .iter()
                .any(|(field, value)| *field == "reasoningEfforts" && value == "low, high")
        );
        assert!(suggestion.model.reasoning_efforts.is_none());
        assert!(suggestion.adopt_all().reasoning_efforts.is_none());
    }

    #[test]
    fn catalog_reasoning_options_are_suggested_not_declared() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{
                "reasoning_options":[
                    {"type":"toggle"},
                    {"type":"effort","values":["low","high"]},
                    {"type":"budget_tokens","min":1024}
                ]
            }}}}"#,
        )
        .unwrap();
        let suggestion = catalog.suggestion("p", "m").unwrap();
        assert_eq!(
            suggestion.model.reasoning_options.as_ref().unwrap().len(),
            3
        );
        // The catalogue's controls stay catalogue data; the effort values are
        // an explicit suggestion, not the Agent's declared level table.
        assert_eq!(
            suggestion.reasoning_efforts_suggestion().as_deref(),
            Some("low, high")
        );
        assert!(suggestion.model.reasoning_efforts.is_none());
        assert!(suggestion.adopt_all().reasoning_efforts.is_none());
    }

    #[test]
    fn a_catalog_without_an_effort_option_suggests_no_levels() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{"reasoning_options":[
                {"type":"toggle"},
                {"type":"budget_tokens","min":1024}
            ]}}}}"#,
        )
        .unwrap();
        let suggestion = catalog.suggestion("p", "m").unwrap();
        assert!(suggestion.reasoning_efforts_suggestion().is_none());
        assert!(
            !suggestion
                .fields()
                .iter()
                .any(|(field, _)| *field == "reasoningEfforts")
        );
    }

    #[test]
    fn canonical_model_ids_come_from_the_catalogue_only() {
        let catalog =
            parse_catalog(r#"{"p":{"models":{"m":{"canonical_model_id":"vendor/m"}}}}"#).unwrap();
        assert_eq!(
            catalog.canonical_model_id("p", "m").as_deref(),
            Some("vendor/m")
        );
        // The suffix match that recovers `m-thinking` also carries its identity.
        assert_eq!(
            catalog.canonical_model_id("p", "m-thinking").as_deref(),
            Some("vendor/m")
        );
        assert_eq!(catalog.canonical_model_id("p", "mystery"), None);
        assert_eq!(catalog.canonical_model_id("unknown", "m"), None);
    }

    /// The offered list is what a new Provider starts from, so it must skip
    /// the entries a session cannot use and the ids a vendor retired.
    #[test]
    fn offered_models_keep_only_usable_conversation_entries() {
        let catalog = parse_catalog(
            r#"{"p":{"name":"P","models":{
                "chat":{"tool_call":true,"modalities":{"input":["text"],"output":["text"]}},
                "retired":{"tool_call":true,"status":"deprecated",
                           "modalities":{"input":["text"],"output":["text"]}},
                "speech":{"tool_call":false,"modalities":{"input":["text"],"output":["audio"]}},
                "image":{"tool_call":false,"modalities":{"input":["text"],"output":["image"]}},
                "no-tools":{"tool_call":false,"modalities":{"input":["text"],"output":["text"]}},
                "silent-about-tools":{"modalities":{"input":["text"],"output":["text"]}},
                "silent-about-modalities":{"tool_call":true}
            }}}"#,
        )
        .unwrap();
        let offered = catalog
            .offered_models("p")
            .into_iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        // An entry that publishes no modality list, or says nothing about
        // tools, keeps the benefit of the doubt: older and third-party entries
        // omit those fields, and treating an omission as "no" would hide real
        // conversation models.
        assert_eq!(
            offered,
            vec!["chat", "silent-about-modalities", "silent-about-tools"]
        );
        // An unknown provider has nothing to offer rather than a default.
        assert!(catalog.offered_models("unknown").is_empty());
    }

    #[test]
    fn new_catalog_metadata_does_not_widen_the_adopted_capabilities() {
        let catalog = parse_catalog(
            r#"{"p":{"models":{"m":{
                "family":"p","release_date":"2025-01-01","limit":{"input":1000},
                "tool_call":true,"structured_output":true,
                "status":"beta","canonical_model_id":"p/m",
                "modalities":{"input":["text","image"],"output":["text"]}
            }}}}"#,
        )
        .unwrap();
        let suggestion = catalog.suggestion("p", "m").unwrap();
        let capabilities = suggestion.adopt_all();
        // Only the declared capability fields are adopted.
        assert_eq!(capabilities.reasoning, None);
        assert_eq!(capabilities.image_input, Some(true));
        assert_eq!(capabilities.pdf_input, Some(false));
        assert_eq!(capabilities.context_tokens, None);
        assert_eq!(
            capabilities.adopted_from_catalog().collect::<Vec<_>>(),
            vec!["imageInput", "pdfInput"]
        );
    }
}
