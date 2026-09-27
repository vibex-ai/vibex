//! Browser panel preferences: the page a new tab opens and the engine the
//! address bar searches with.
//!
//! Both are a preset plus an optional custom value, and both are resolved here
//! rather than at the point of use: a custom value that cannot be used falls
//! back to the preset default, so a new tab is never a blank page because a URL
//! was typed wrong.

use serde::{Deserialize, Serialize};
use vibex_core::BrowserCaptureQuality;

/// Placeholder a custom search engine URL carries the keyword in.
///
/// One documented placeholder beats sniffing the value: a search URL that
/// silently lost its keyword would open the engine's home page and look like
/// the search did nothing.
pub const SEARCH_QUERY_PLACEHOLDER: &str = "{query}";

/// Longest accepted custom URL, matching the other bounded UI-state strings.
const MAX_URL_LENGTH: usize = 2_048;

/// Page a browser tab opens when it is created without an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserStartPage {
    #[default]
    Google,
    Bing,
    Baidu,
    /// Whatever `BrowserUiState::start_page_url` holds.
    Custom,
}

impl BrowserStartPage {
    /// The preset's own address, or `None` for [`Self::Custom`].
    pub const fn preset_url(self) -> Option<&'static str> {
        match self {
            Self::Google => Some("https://www.google.com/"),
            Self::Bing => Some("https://www.bing.com/"),
            Self::Baidu => Some("https://www.baidu.com/"),
            Self::Custom => None,
        }
    }

    /// The name this preset is stored and selected by.
    pub const fn storage_value(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Baidu => "baidu",
            Self::Custom => "custom",
        }
    }

    pub fn from_storage_value(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            "bing" => Some(Self::Bing),
            "baidu" => Some(Self::Baidu),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Engine the address bar searches with when the input is not an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSearchEngine {
    #[default]
    Google,
    Bing,
    Baidu,
    DuckDuckGo,
    /// Whatever `BrowserUiState::search_engine_url` holds.
    Custom,
}

impl BrowserSearchEngine {
    /// The preset's search URL, or `None` for [`Self::Custom`].
    pub const fn preset_url(self) -> Option<&'static str> {
        match self {
            Self::Google => Some("https://www.google.com/search?q={query}"),
            Self::Bing => Some("https://www.bing.com/search?q={query}"),
            Self::Baidu => Some("https://www.baidu.com/s?wd={query}"),
            Self::DuckDuckGo => Some("https://duckduckgo.com/?q={query}"),
            Self::Custom => None,
        }
    }

    /// The name this engine is stored and selected by.
    pub const fn storage_value(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Baidu => "baidu",
            Self::DuckDuckGo => "duck_duck_go",
            Self::Custom => "custom",
        }
    }

    pub fn from_storage_value(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            "bing" => Some(Self::Bing),
            "baidu" => Some(Self::Baidu),
            "duck_duck_go" => Some(Self::DuckDuckGo),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Persisted browser panel preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserUiState {
    #[serde(default)]
    pub start_page: BrowserStartPage,
    /// Used when `start_page` is [`BrowserStartPage::Custom`].
    #[serde(default)]
    pub start_page_url: String,
    #[serde(default)]
    pub search_engine: BrowserSearchEngine,
    /// Used when `search_engine` is [`BrowserSearchEngine::Custom`]. Carries
    /// [`SEARCH_QUERY_PLACEHOLDER`] where the keyword goes.
    #[serde(default)]
    pub search_engine_url: String,
    /// What the panel's HD toggle selects. JPEG 80 unless the reader asked for
    /// lossless frames.
    #[serde(default)]
    pub capture_quality: BrowserCaptureQuality,
    /// Whether the panel lets a page write a file to the runtime's download
    /// directory. Off by default: a download is a write to this machine and the
    /// reader has to ask for it.
    #[serde(default)]
    pub downloads_enabled: bool,
}

impl BrowserUiState {
    /// Trims and bounds the custom values.
    ///
    /// The selection itself is left alone: a reader who just picked "Custom"
    /// has not typed an address yet, and moving the selection back would make
    /// the field they are about to fill in unreachable.
    pub fn normalize(&mut self) {
        self.start_page_url = normalize_custom_url(std::mem::take(&mut self.start_page_url));
        self.search_engine_url = normalize_custom_url(std::mem::take(&mut self.search_engine_url));
    }

    /// The address a browser tab opens when nothing else names a page.
    ///
    /// A custom selection with an unusable value falls back to the default
    /// preset rather than to an empty address: a tab is never blank because a
    /// custom URL was typed wrong, and the settings card says what is missing.
    pub fn resolved_start_page(&self) -> &str {
        if let Some(url) = self.start_page.preset_url() {
            return url;
        }
        if browser_start_page_is_usable(&self.start_page_url) {
            return self.start_page_url.as_str();
        }
        default_start_page()
    }

    /// The search URL template the address bar fills in, `{query}` included.
    pub fn resolved_search_url(&self) -> &str {
        if let Some(url) = self.search_engine.preset_url() {
            return url;
        }
        if browser_search_url_is_usable(&self.search_engine_url) {
            return self.search_engine_url.as_str();
        }
        default_search_url()
    }

    /// Whether the custom start page is filled in, for the settings card.
    pub fn custom_start_page_is_valid(&self) -> bool {
        self.start_page != BrowserStartPage::Custom
            || browser_start_page_is_usable(&self.start_page_url)
    }

    /// Whether the custom search engine is filled in, for the settings card.
    pub fn custom_search_engine_is_valid(&self) -> bool {
        self.search_engine != BrowserSearchEngine::Custom
            || browser_search_url_is_usable(&self.search_engine_url)
    }
}

/// The address a fresh install opens, and the fallback for an unusable custom
/// value.
fn default_start_page() -> &'static str {
    BrowserStartPage::default()
        .preset_url()
        .expect("the default start page is a preset")
}

/// The engine a fresh install searches with, and the fallback for an unusable
/// custom value.
fn default_search_url() -> &'static str {
    BrowserSearchEngine::default()
        .preset_url()
        .expect("the default search engine is a preset")
}

/// Whether a typed start page can be navigated to.
pub fn browser_start_page_is_usable(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && absolute_address_is_usable(value)
}

/// Whether a typed search URL can carry a keyword.
pub fn browser_search_url_is_usable(value: &str) -> bool {
    let value = value.trim();
    value.contains(SEARCH_QUERY_PLACEHOLDER) && absolute_address_is_usable(value)
}

/// A URL a browser can be sent to: an absolute address with a host, or one of
/// the hostless schemes a page can legitimately be.
///
/// `Url::parse` on its own accepts `localhost:5173` as a URL whose scheme is
/// `localhost`, which is how an unfinished address would be taken as a start
/// page and fail to load.
fn absolute_address_is_usable(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    parsed.host().is_some()
        || matches!(
            parsed.scheme(),
            "about" | "data" | "blob" | "chrome" | "view-source" | "file"
        )
}

fn normalize_custom_url(value: String) -> String {
    let value = value.trim();
    value.chars().take(MAX_URL_LENGTH).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_opens_google_and_searches_with_google() {
        let state = BrowserUiState::default();
        assert_eq!(state.resolved_start_page(), "https://www.google.com/");
        assert_eq!(
            state.resolved_search_url(),
            "https://www.google.com/search?q={query}"
        );
    }

    #[test]
    fn every_preset_resolves_to_a_usable_value() {
        for start_page in [
            BrowserStartPage::Google,
            BrowserStartPage::Bing,
            BrowserStartPage::Baidu,
        ] {
            let url = start_page.preset_url().expect("a preset has an address");
            assert!(
                browser_start_page_is_usable(url),
                "{start_page:?} must resolve to an address the browser accepts"
            );
        }
        for search_engine in [
            BrowserSearchEngine::Google,
            BrowserSearchEngine::Bing,
            BrowserSearchEngine::Baidu,
            BrowserSearchEngine::DuckDuckGo,
        ] {
            let url = search_engine
                .preset_url()
                .expect("a preset has a search URL");
            assert!(
                browser_search_url_is_usable(url),
                "{search_engine:?} must carry the query placeholder"
            );
        }
    }

    #[test]
    fn a_custom_value_is_used_only_when_it_is_usable() {
        let mut state = BrowserUiState {
            start_page: BrowserStartPage::Custom,
            start_page_url: "https://example.com/home".to_string(),
            search_engine: BrowserSearchEngine::Custom,
            search_engine_url: "https://example.com/find?q={query}".to_string(),
            ..BrowserUiState::default()
        };
        state.normalize();
        assert_eq!(state.resolved_start_page(), "https://example.com/home");
        assert_eq!(
            state.resolved_search_url(),
            "https://example.com/find?q={query}"
        );
        assert!(state.custom_start_page_is_valid());
        assert!(state.custom_search_engine_is_valid());
    }

    #[test]
    fn an_unusable_custom_value_falls_back_to_the_default_instead_of_blanking_a_tab() {
        let mut state = BrowserUiState {
            start_page: BrowserStartPage::Custom,
            start_page_url: "   ".to_string(),
            search_engine: BrowserSearchEngine::Custom,
            // A search URL without the placeholder opens the engine's home page
            // and looks like the search silently did nothing.
            search_engine_url: "https://example.com/find".to_string(),
            ..BrowserUiState::default()
        };
        state.normalize();
        // The selection survives, because the reader is still editing it, but
        // what the panel uses is the default preset.
        assert_eq!(state.start_page, BrowserStartPage::Custom);
        assert_eq!(state.search_engine, BrowserSearchEngine::Custom);
        assert!(!state.custom_start_page_is_valid());
        assert!(!state.custom_search_engine_is_valid());
        assert_eq!(state.resolved_start_page(), "https://www.google.com/");
        assert_eq!(
            state.resolved_search_url(),
            "https://www.google.com/search?q={query}"
        );
    }

    #[test]
    fn a_custom_value_is_trimmed_and_bounded() {
        let mut state = BrowserUiState {
            start_page_url: format!("  https://example.com/{}  ", "a".repeat(4_000)),
            search_engine_url: " https://example.com/?q={query} ".to_string(),
            ..BrowserUiState::default()
        };
        state.normalize();
        assert!(state.start_page_url.starts_with("https://example.com/"));
        assert_eq!(state.start_page_url.len(), MAX_URL_LENGTH);
        assert_eq!(
            state.search_engine_url, "https://example.com/?q={query}",
            "the custom values are trimmed"
        );
    }

    /// `localhost:5173` parses as a URL whose scheme is `localhost`, which
    /// must not pass for a finished address.
    #[test]
    fn a_bare_host_is_not_an_absolute_address() {
        assert!(!browser_start_page_is_usable("localhost:5173"));
        assert!(browser_start_page_is_usable("http://localhost:5173"));
        assert!(browser_start_page_is_usable("about:blank"));
    }

    /// The select in the settings writes `storage_value`, and the file stores
    /// the serde tag: they have to be the same string.
    #[test]
    fn storage_values_match_the_serialized_names() {
        for start_page in [
            BrowserStartPage::Google,
            BrowserStartPage::Bing,
            BrowserStartPage::Baidu,
            BrowserStartPage::Custom,
        ] {
            assert_eq!(
                serde_json::to_value(start_page).unwrap(),
                serde_json::json!(start_page.storage_value())
            );
            assert_eq!(
                BrowserStartPage::from_storage_value(start_page.storage_value()),
                Some(start_page)
            );
        }
        for engine in [
            BrowserSearchEngine::Google,
            BrowserSearchEngine::Bing,
            BrowserSearchEngine::Baidu,
            BrowserSearchEngine::DuckDuckGo,
            BrowserSearchEngine::Custom,
        ] {
            assert_eq!(
                serde_json::to_value(engine).unwrap(),
                serde_json::json!(engine.storage_value())
            );
            assert_eq!(
                BrowserSearchEngine::from_storage_value(engine.storage_value()),
                Some(engine)
            );
        }
        assert_eq!(BrowserStartPage::from_storage_value("nope"), None);
        assert_eq!(BrowserSearchEngine::from_storage_value("nope"), None);
    }

    #[test]
    fn a_legacy_file_without_browser_preferences_reads_as_the_defaults() {
        let state: BrowserUiState = serde_json::from_str("{}").expect("an empty section reads");
        assert_eq!(state, BrowserUiState::default());
    }

    #[test]
    fn browser_preferences_round_trip_through_json() {
        let state = BrowserUiState {
            start_page: BrowserStartPage::Baidu,
            start_page_url: "https://example.com/".to_string(),
            search_engine: BrowserSearchEngine::DuckDuckGo,
            search_engine_url: "https://example.com/?q={query}".to_string(),
            // The HD toggle is part of the same file, so a round trip that
            // dropped it would silently reset the reader's choice.
            capture_quality: BrowserCaptureQuality::High,
            downloads_enabled: true,
        };
        let encoded = serde_json::to_value(&state).expect("the preferences serialize");
        assert_eq!(
            encoded,
            serde_json::json!({
                "startPage": "baidu",
                "startPageUrl": "https://example.com/",
                "searchEngine": "duck_duck_go",
                "searchEngineUrl": "https://example.com/?q={query}",
                "captureQuality": "high",
                "downloadsEnabled": true,
            })
        );
        let decoded: BrowserUiState =
            serde_json::from_value(encoded).expect("the preferences read back");
        assert_eq!(decoded, state);
    }
}
