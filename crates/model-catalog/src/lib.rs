//! A maintained catalogue of model metadata.
//!
//! The Provider spec forbids *inferring* a capability from a model id, an
//! endpoint, or a vendor. It does not forbid *data*. A catalogue is data: a
//! maintained table that says "this exact model has this context window", the
//! same kind of statement a user makes when they type it by hand.
//!
//! Three rules follow from that distinction and shape this crate:
//!
//! 1. A catalogue value is a **suggestion**, never an applied default. The user
//!    adopts it per field, and the adoption is recorded as provenance.
//! 2. A hand-typed value always wins. Nothing here overwrites a declaration.
//! 3. Every failure is silent. A catalogue that cannot be fetched, parsed, or
//!    read changes no capability and blocks nothing; the editor simply shows no
//!    suggestions.
//!
//! Prices are deliberately not modelled. The product reports tokens, not money.

mod cache;
mod fetch;
mod suggest;

pub use cache::{CatalogCache, CatalogState};
pub use fetch::{CatalogFetchError, catalog_url, parse_catalog};
pub use suggest::{
    CapabilitySuggestion, Catalog, CatalogModalities, CatalogModel, CatalogProvider,
    CatalogReasoningOption, SuggestSource,
};

/// How long a fetched catalogue is trusted before it is refreshed.
pub const CATALOG_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// The catalogue's own schema version, so a stored cache can be invalidated.
///
/// Bumped when the stored shape gains fields a reader expects, so an older
/// cache is discarded and refetched rather than served without them.
pub const CATALOG_SCHEMA_VERSION: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    /// No catalogue contract may carry a money field.
    ///
    /// The product reports tokens; a price is an estimate that misleads more
    /// than it informs, and an existing contract test already forbids it in
    /// usage. This keeps the catalogue from reintroducing it by the side door.
    #[test]
    fn a_catalog_model_carries_no_money_field() {
        let model = CatalogModel {
            id: "deepseek-chat".to_string(),
            display_name: Some("DeepSeek Chat".to_string()),
            family: Some("deepseek".to_string()),
            release_date: Some("2024-12-26".to_string()),
            context_tokens: Some(128_000),
            input_tokens: Some(64_000),
            output_tokens: Some(8_192),
            reasoning: Some(true),
            reasoning_options: Some(vec![CatalogReasoningOption::Toggle]),
            image_input: Some(false),
            pdf_input: Some(false),
            modalities: Some(CatalogModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            }),
            temperature: Some(true),
            tool_call: Some(true),
            structured_output: Some(true),
            canonical_model_id: Some("deepseek/deepseek-chat".to_string()),
            status: Some("beta".to_string()),
            reasoning_efforts: None,
        };
        let encoded = serde_json::to_string(&model).unwrap().to_lowercase();
        for forbidden in ["cost", "price", "currency", "amount", "usd", "micros"] {
            assert!(
                !encoded.contains(forbidden),
                "a catalog model must not carry `{forbidden}`: {encoded}"
            );
        }
    }

    /// The bundled asset has to answer for every preset that names a
    /// catalogue entry.
    ///
    /// The asset is what a first launch and an offline machine have instead of
    /// the network, and a Provider's model list now comes from it. A preset
    /// whose entry is missing would open an empty picker with no way to fill
    /// it, so the mismatch fails here rather than in a user's editor.
    #[test]
    fn the_bundled_catalog_covers_every_preset_entry() {
        let catalog = parse_catalog(include_str!("../assets/models-dev-bundled.json"))
            .expect("the bundled catalogue parses");
        for preset in vibex_core::provider_presets() {
            let Some(catalog_id) = preset.catalog_id.as_deref() else {
                continue;
            };
            assert!(
                catalog.provider(catalog_id).is_some(),
                "{} names `{catalog_id}`, which the bundled catalogue does not carry",
                preset.id
            );
        }
    }

    /// A `cost` object in the upstream document is not read into the catalog,
    /// so serialising the parsed catalog cannot leak money either.
    #[test]
    fn a_parsed_catalog_never_carries_money() {
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
}
