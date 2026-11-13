//! The market sources and the catalog that merges them.
//!
//! Three sources feed one list: a signed static index built from a catalog
//! repository, GitHub topic discovery for repositories nobody reviewed, and the
//! catalogs the reader added themselves. [`MarketCatalog`] fetches them, merges
//! them and caches the result.

pub mod cache;
pub mod catalog;
pub mod fetch;
pub mod github;
pub mod index;
pub mod merge;

pub use cache::MarketCache;
pub use catalog::{
    GITHUB_TTL, INDEX_MAX_AGE, INDEX_TTL, MarketCatalog, MarketConfig, UserSourceConfig,
    UserSourceKind,
};
pub use fetch::{MARKET_REQUEST_TIMEOUT, MAX_MARKET_REDIRECTS, MarketHttpClient, MarketUrlPolicy};
pub use github::{DEFAULT_TOPIC, GITHUB_API_BASE, GITHUB_RAW_BASE, GithubTopicSource};
pub use index::{
    INDEX_SCHEMA, MAX_INDEX_BYTES, MAX_REVOCATIONS_BYTES, PluginIndexEntry, PluginIndexFile,
    PluginRevocations, PublisherKey, REVOCATIONS_SCHEMA, RevokedPlugin, decode_verifying_key,
    encode_verifying_key, sign_document, sign_package, verify_package_signature, verify_signature,
};
pub use merge::{DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, merge_entries, search_entries};
