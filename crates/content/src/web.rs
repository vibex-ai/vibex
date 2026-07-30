use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;
use url::Url;
use vibex_core::{VibexError, VibexResult};

use crate::{
    ContentResourceMetrics, ContentSurfaceKind, ContentSurfaceLifecycle, ContentSurfaceOrigin,
    GenerationDisposition, LogicalSurfaceBounds,
};

pub const WEB_PREVIEW_HISTORY_LIMIT: usize = 64;
pub const WEB_PREVIEW_URL_LIMIT: usize = 2_048;
pub const DEFAULT_BROWSER_SURFACE_LIMIT: usize = 3;
pub const DEFAULT_BROWSER_CACHE_BUDGET_BYTES: usize = 384 * 1024 * 1024;
pub const WEB_PREVIEW_UNSUPPORTED_CODE: &str = "web_preview_temporarily_unsupported";
pub const RIGHT_RAIL_WEB_UNSUPPORTED_CODE: &str = "right_rail_native_web_surface_unsupported";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedWebUrl(String);

impl NormalizedWebUrl {
    pub fn parse(input: &str) -> VibexResult<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(VibexError::validation(
                "web_preview_url_missing",
                "Web Preview URL is required",
            ));
        }
        if trimmed.len() > WEB_PREVIEW_URL_LIMIT || trimmed.chars().any(char::is_control) {
            return Err(VibexError::validation(
                "web_preview_url_too_long",
                "Web Preview URL is too long or contains control characters",
            ));
        }
        let candidate = if has_explicit_scheme(trimmed) {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        let url = Url::parse(&candidate).map_err(|_| {
            VibexError::validation("web_preview_url_invalid", "Web Preview URL is invalid")
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(VibexError::validation(
                "web_preview_scheme_unsupported",
                "Web Preview supports only HTTP and HTTPS URLs",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(VibexError::validation(
                "web_preview_credentials_unsupported",
                "Web Preview URLs cannot contain embedded credentials",
            ));
        }
        Ok(Self(url.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn has_explicit_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    let suffix = &value[colon + 1..];
    if !suffix.starts_with("//")
        && (scheme.contains('.')
            || scheme.eq_ignore_ascii_case("localhost")
            || suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'/'))
    {
        return false;
    }
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'A'..=b'Z' => true,
            b'0'..=b'9' | b'+' | b'-' | b'.' => index > 0,
            _ => false,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebHostVisibility {
    Show,
    Hide,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WebHostAction {
    Create {
        generation: u64,
        navigation_id: u64,
        url: String,
    },
    Navigate {
        generation: u64,
        navigation_id: u64,
        url: String,
    },
    SetVisibility(WebHostVisibility),
    SetBounds(LogicalSurfaceBounds),
    FocusSurface,
    FocusParent,
    Reload {
        generation: u64,
        navigation_id: u64,
    },
    Close,
    OpenExternal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebNavigationDisposition {
    Allow,
    OpenExternal,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBackgroundNetworkPolicy {
    SuspendWhenHidden,
    AllowUntilIdleEviction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfilePolicy {
    pub persistent: bool,
    pub cache_budget_bytes: usize,
    pub cookie_partitioned_by_workspace: bool,
    pub background_network: BrowserBackgroundNetworkPolicy,
    pub user_agent: Option<String>,
}

impl Default for BrowserProfilePolicy {
    fn default() -> Self {
        Self {
            persistent: false,
            cache_budget_bytes: 128 * 1024 * 1024,
            cookie_partitioned_by_workspace: true,
            background_network: BrowserBackgroundNetworkPolicy::SuspendWhenHidden,
            user_agent: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPreviewDiagnostics {
    pub backend: String,
    pub backend_revision: String,
    pub generation: u64,
    pub navigation_id: u64,
    pub history_entries: usize,
    pub explicit_load_completed: bool,
    pub page_process_crashes: u64,
    pub last_navigation_error_code: Option<String>,
}

pub struct WebPreviewController {
    lifecycle: ContentSurfaceLifecycle,
    restored_url: Option<NormalizedWebUrl>,
    current_url: Option<NormalizedWebUrl>,
    requested_url: Option<NormalizedWebUrl>,
    navigation_id: u64,
    history: VecDeque<NormalizedWebUrl>,
    history_index: Option<usize>,
    explicit_load_completed: bool,
    page_process_crashes: u64,
    last_navigation_error_code: Option<String>,
    profile_policy: BrowserProfilePolicy,
}

impl WebPreviewController {
    pub fn restored(url: Option<&str>, origin: ContentSurfaceOrigin) -> VibexResult<Self> {
        let restored_url = url
            .filter(|url| !url.trim().is_empty())
            .map(NormalizedWebUrl::parse)
            .transpose()?;
        Ok(Self {
            lifecycle: ContentSurfaceLifecycle::restored(ContentSurfaceKind::Web, origin),
            restored_url,
            current_url: None,
            requested_url: None,
            navigation_id: 0,
            history: VecDeque::new(),
            history_index: None,
            explicit_load_completed: false,
            page_process_crashes: 0,
            last_navigation_error_code: None,
            profile_policy: BrowserProfilePolicy::default(),
        })
    }

    pub fn lifecycle(&self) -> &ContentSurfaceLifecycle {
        &self.lifecycle
    }

    pub fn ready_url(&self) -> Option<&str> {
        self.restored_url.as_ref().map(NormalizedWebUrl::as_str)
    }

    pub fn current_url(&self) -> Option<&str> {
        self.current_url.as_ref().map(NormalizedWebUrl::as_str)
    }

    pub fn has_allocated_surface(&self) -> bool {
        self.requested_url.is_some() || self.explicit_load_completed
    }

    pub fn profile_policy(&self) -> &BrowserProfilePolicy {
        &self.profile_policy
    }

    pub fn set_profile_policy(&mut self, policy: BrowserProfilePolicy) -> VibexResult<()> {
        if policy.cache_budget_bytes == 0 || policy.cache_budget_bytes > 2 * 1024 * 1024 * 1024 {
            return Err(VibexError::validation(
                "web_preview_cache_budget_invalid",
                "Web Preview cache budget is invalid",
            ));
        }
        if policy
            .user_agent
            .as_ref()
            .is_some_and(|value| value.len() > 512 || value.chars().any(char::is_control))
        {
            return Err(VibexError::validation(
                "web_preview_user_agent_invalid",
                "Web Preview user agent is invalid",
            ));
        }
        let _ = policy;
        Err(web_preview_unsupported(self.lifecycle.origin()))
    }

    pub fn activate(&mut self, generation: u64) -> VibexResult<Vec<WebHostAction>> {
        self.last_navigation_error_code = Some(unsupported_code(self.lifecycle.origin()).into());
        if self
            .lifecycle
            .mark_unsupported(generation, unsupported_code(self.lifecycle.origin()))?
            == GenerationDisposition::IgnoredStale
        {
            return Ok(Vec::new());
        }
        Err(web_preview_unsupported(self.lifecycle.origin()))
    }

    pub fn explicit_load(&mut self, input: &str, generation: u64) -> VibexResult<WebHostAction> {
        if self.lifecycle.activation_generation() != generation {
            return Err(VibexError::conflict(
                "web_preview_activation_stale",
                "Web Preview activation changed before loading",
            ));
        }
        self.restored_url = Some(NormalizedWebUrl::parse(input)?);
        self.last_navigation_error_code = Some(unsupported_code(self.lifecycle.origin()).into());
        self.lifecycle
            .mark_unsupported(generation, unsupported_code(self.lifecycle.origin()))?;
        Err(web_preview_unsupported(self.lifecycle.origin()))
    }

    pub fn open_external(&self, input: &str) -> VibexResult<WebHostAction> {
        Ok(WebHostAction::OpenExternal(
            NormalizedWebUrl::parse(input)?.into_string(),
        ))
    }

    pub fn navigation_finished(
        &mut self,
        generation: u64,
        navigation_id: u64,
        final_url: &str,
    ) -> VibexResult<GenerationDisposition> {
        if navigation_id != self.navigation_id {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if generation != self.lifecycle.activation_generation() {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        let final_url = NormalizedWebUrl::parse(final_url)?;
        self.current_url = Some(final_url.clone());
        self.restored_url = Some(final_url.clone());
        self.requested_url = None;
        self.explicit_load_completed = true;
        self.push_history(final_url);
        self.lifecycle.finish_load(generation)
    }

    pub fn navigation_failed(
        &mut self,
        generation: u64,
        navigation_id: u64,
        error_code: &str,
    ) -> VibexResult<GenerationDisposition> {
        if navigation_id != self.navigation_id {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if generation != self.lifecycle.activation_generation() {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.requested_url = None;
        self.last_navigation_error_code = Some(stable_code(error_code));
        self.lifecycle.failed(generation, error_code)
    }

    pub fn decide_navigation(
        &self,
        url: &str,
        main_frame: bool,
        user_requested_new_window: bool,
    ) -> WebNavigationDisposition {
        let Ok(url) = Url::parse(url) else {
            return WebNavigationDisposition::Block;
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return WebNavigationDisposition::OpenExternal;
        }
        if !main_frame || user_requested_new_window {
            WebNavigationDisposition::OpenExternal
        } else {
            WebNavigationDisposition::Allow
        }
    }

    pub fn set_bounds(
        &mut self,
        generation: u64,
        bounds: LogicalSurfaceBounds,
    ) -> VibexResult<Option<WebHostAction>> {
        Ok(
            (self.lifecycle.set_bounds(generation, bounds)? == GenerationDisposition::Applied)
                .then_some(WebHostAction::SetBounds(bounds)),
        )
    }

    pub fn overlay_opened(&mut self, generation: u64) -> VibexResult<Vec<WebHostAction>> {
        self.lifecycle
            .mark_unsupported(generation, unsupported_code(self.lifecycle.origin()))?;
        Ok(Vec::new())
    }

    pub fn overlay_closed(&mut self, generation: u64) -> VibexResult<Vec<WebHostAction>> {
        self.lifecycle
            .mark_unsupported(generation, unsupported_code(self.lifecycle.origin()))?;
        Ok(Vec::new())
    }

    pub fn page_process_crashed(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        let disposition = self.lifecycle.crashed(generation)?;
        if disposition == GenerationDisposition::Applied {
            self.page_process_crashes = self.page_process_crashes.saturating_add(1);
        }
        Ok(disposition)
    }

    pub fn reload(&mut self, generation: u64) -> VibexResult<WebHostAction> {
        if generation != self.lifecycle.activation_generation() {
            return Err(VibexError::conflict(
                "web_preview_activation_stale",
                "Web Preview activation changed before reload",
            ));
        }
        Err(web_preview_unsupported(self.lifecycle.origin()))
    }

    pub fn deactivate(&mut self, generation: u64) -> VibexResult<Vec<WebHostAction>> {
        self.lifecycle
            .mark_unsupported(generation, unsupported_code(self.lifecycle.origin()))?;
        Ok(Vec::new())
    }

    pub fn close(&mut self, generation: u64) -> VibexResult<Vec<WebHostAction>> {
        if self.lifecycle.close(generation)? == GenerationDisposition::IgnoredStale {
            return Ok(Vec::new());
        }
        Ok(vec![WebHostAction::Close, WebHostAction::FocusParent])
    }

    pub fn back(&mut self, generation: u64) -> VibexResult<Option<WebHostAction>> {
        let _ = generation;
        Ok(None)
    }

    pub fn forward(&mut self, generation: u64) -> VibexResult<Option<WebHostAction>> {
        let _ = generation;
        Ok(None)
    }

    pub fn diagnostics(
        &self,
        backend: impl Into<String>,
        backend_revision: impl Into<String>,
    ) -> WebPreviewDiagnostics {
        WebPreviewDiagnostics {
            backend: backend.into(),
            backend_revision: backend_revision.into(),
            generation: self.lifecycle.activation_generation(),
            navigation_id: self.navigation_id,
            history_entries: self.history.len(),
            explicit_load_completed: self.explicit_load_completed,
            page_process_crashes: self.page_process_crashes,
            last_navigation_error_code: self.last_navigation_error_code.clone(),
        }
    }

    fn push_history(&mut self, url: NormalizedWebUrl) {
        if let Some(index) = self.history_index {
            self.history.truncate(index + 1);
        }
        if self.history.back() != Some(&url) {
            self.history.push_back(url);
        }
        while self.history.len() > WEB_PREVIEW_HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history_index = self.history.len().checked_sub(1);
    }
}

fn unsupported_code(origin: ContentSurfaceOrigin) -> &'static str {
    match origin {
        ContentSurfaceOrigin::Preview => WEB_PREVIEW_UNSUPPORTED_CODE,
        ContentSurfaceOrigin::RightRailWebPlugin => RIGHT_RAIL_WEB_UNSUPPORTED_CODE,
    }
}

fn web_preview_unsupported(origin: ContentSurfaceOrigin) -> VibexError {
    let (code, message) = match origin {
        ContentSurfaceOrigin::Preview => (
            WEB_PREVIEW_UNSUPPORTED_CODE,
            "embedded Web Preview is temporarily unavailable in GPUI",
        ),
        ContentSurfaceOrigin::RightRailWebPlugin => (
            RIGHT_RAIL_WEB_UNSUPPORTED_CODE,
            "right-rail Web plugins do not allocate native browser surfaces in GPUI v1",
        ),
    };
    VibexError::capability(code, message)
        .with_recovery_hint("Open the validated URL in the system browser")
}

fn stable_code(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        value.to_string()
    } else {
        "invalid_code".to_string()
    }
}

#[derive(Debug, Clone)]
struct BrowserSurfaceEntry {
    visible: bool,
    resident_bytes: usize,
    last_used: u64,
}

pub struct BrowserSurfacePool {
    max_surfaces: usize,
    max_resident_bytes: usize,
    clock: u64,
    entries: BTreeMap<String, BrowserSurfaceEntry>,
    evictions: u64,
}

impl BrowserSurfacePool {
    pub fn new(max_surfaces: usize, max_resident_bytes: usize) -> VibexResult<Self> {
        if max_surfaces == 0 || max_resident_bytes == 0 {
            return Err(VibexError::validation(
                "web_surface_pool_budget_invalid",
                "browser surface pool budgets must be non-zero",
            ));
        }
        Ok(Self {
            max_surfaces,
            max_resident_bytes,
            clock: 0,
            entries: BTreeMap::new(),
            evictions: 0,
        })
    }

    pub fn acquire(&mut self, key: impl Into<String>, resident_bytes: usize) -> Vec<String> {
        self.clock = self.clock.saturating_add(1);
        let key = key.into();
        self.entries.insert(
            key.clone(),
            BrowserSurfaceEntry {
                visible: true,
                resident_bytes,
                last_used: self.clock,
            },
        );
        self.evict_to_budget(Some(&key))
    }

    pub fn hide(&mut self, key: &str) {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.visible = false;
            entry.last_used = self.clock;
        }
    }

    pub fn show(&mut self, key: &str) {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.visible = true;
            entry.last_used = self.clock;
        }
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    pub fn metrics(&self) -> ContentResourceMetrics {
        ContentResourceMetrics {
            resident_items: self.entries.len(),
            resident_bytes: self
                .entries
                .values()
                .map(|entry| entry.resident_bytes)
                .sum(),
            budget_items: self.max_surfaces,
            budget_bytes: self.max_resident_bytes,
            evictions: self.evictions,
        }
    }

    fn evict_to_budget(&mut self, protected: Option<&str>) -> Vec<String> {
        let mut evicted = Vec::new();
        while !self.metrics().is_within_budget() {
            let candidate = self
                .entries
                .iter()
                .filter(|(key, entry)| {
                    !entry.visible && protected.is_none_or(|protected| key.as_str() != protected)
                })
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
                .or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(key, _)| {
                            protected.is_none_or(|protected| key.as_str() != protected)
                        })
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone())
                });
            let Some(candidate) = candidate else {
                break;
            };
            self.entries.remove(&candidate);
            self.evictions = self.evictions.saturating_add(1);
            evicted.push(candidate);
        }
        evicted
    }
}

impl Default for BrowserSurfacePool {
    fn default() -> Self {
        Self::new(
            DEFAULT_BROWSER_SURFACE_LIMIT,
            DEFAULT_BROWSER_CACHE_BUDGET_BYTES,
        )
        .expect("default browser surface budgets are valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_url_stays_ready_without_allocating_or_navigating() {
        let controller = WebPreviewController::restored(
            Some("example.com/path#private-fragment"),
            ContentSurfaceOrigin::Preview,
        )
        .unwrap();
        assert_eq!(
            controller.ready_url(),
            Some("https://example.com/path#private-fragment")
        );
        assert!(!controller.has_allocated_surface());
        assert_eq!(
            controller.lifecycle().phase(),
            crate::ContentSurfacePhase::ReadyToLoad
        );
    }

    #[test]
    fn activation_and_explicit_load_are_typed_unsupported_without_allocation() {
        let mut controller =
            WebPreviewController::restored(None, ContentSurfaceOrigin::Preview).unwrap();
        let error = controller.activate(10).unwrap_err();
        assert_eq!(error.code, WEB_PREVIEW_UNSUPPORTED_CODE);
        assert_eq!(
            controller.lifecycle().phase(),
            crate::ContentSurfacePhase::Unsupported
        );
        let error = controller.explicit_load("example.com", 10).unwrap_err();
        assert_eq!(error.code, WEB_PREVIEW_UNSUPPORTED_CODE);
        assert_eq!(controller.ready_url(), Some("https://example.com/"));
        assert!(!controller.has_allocated_surface());
        assert_eq!(
            controller.open_external("example.com").unwrap(),
            WebHostAction::OpenExternal("https://example.com/".into())
        );
    }

    #[test]
    fn url_policy_rejects_credentials_and_non_http_schemes() {
        assert_eq!(
            NormalizedWebUrl::parse("file:///tmp/private")
                .unwrap_err()
                .code,
            "web_preview_scheme_unsupported"
        );
        assert_eq!(
            NormalizedWebUrl::parse("https://user:secret@example.com")
                .unwrap_err()
                .code,
            "web_preview_credentials_unsupported"
        );
    }

    #[test]
    fn overlays_never_request_a_native_surface() {
        let mut controller =
            WebPreviewController::restored(None, ContentSurfaceOrigin::Preview).unwrap();
        assert_eq!(
            controller.activate(1).unwrap_err().code,
            WEB_PREVIEW_UNSUPPORTED_CODE
        );
        assert!(controller.overlay_opened(1).unwrap().is_empty());
        assert!(controller.overlay_closed(1).unwrap().is_empty());
        assert!(!controller.has_allocated_surface());
        assert_eq!(
            controller.lifecycle().phase(),
            crate::ContentSurfacePhase::Unsupported
        );
    }

    #[test]
    fn right_rail_boundary_fails_before_any_surface_action() {
        let mut controller = WebPreviewController::restored(
            Some("https://example.com"),
            ContentSurfaceOrigin::RightRailWebPlugin,
        )
        .unwrap();
        let error = controller.activate(1).unwrap_err();
        assert_eq!(error.code, "right_rail_native_web_surface_unsupported");
        assert!(!controller.has_allocated_surface());
    }

    #[test]
    fn surface_pool_evicts_hidden_lru_before_visible_content() {
        let mut pool = BrowserSurfacePool::new(2, 100).unwrap();
        assert!(pool.acquire("a", 40).is_empty());
        pool.hide("a");
        assert!(pool.acquire("b", 40).is_empty());
        let evicted = pool.acquire("c", 40);
        assert_eq!(evicted, vec!["a"]);
        assert!(pool.metrics().is_within_budget());
    }

    #[test]
    fn diagnostics_never_include_urls_or_profile_values() {
        let mut controller = WebPreviewController::restored(
            Some("https://secret.example/private"),
            ContentSurfaceOrigin::Preview,
        )
        .unwrap();
        assert!(controller.activate(1).is_err());
        assert!(
            controller
                .explicit_load("https://secret.example/private", 1)
                .is_err()
        );
        let json = serde_json::to_string(
            &controller.diagnostics("unsupported-no-allocation", "gpui-stage-1"),
        )
        .unwrap();
        assert!(!json.contains("secret.example"));
        assert!(json.contains(WEB_PREVIEW_UNSUPPORTED_CODE));
    }
}
