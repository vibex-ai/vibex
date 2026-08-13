use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult};

pub const NATIVE_CONTENT_DIAGNOSTIC_SCHEMA: &str = "native-content-surface.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSurfaceKind {
    Text,
    Markdown,
    Image,
    Media,
    GitDiff,
    GitCommit,
    Terminal,
    Pdf,
    Office,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSurfaceOrigin {
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSurfacePhase {
    ReadyToLoad,
    Loading,
    Active,
    Inactive,
    HiddenForOverlay,
    Crashed,
    Error,
    Unsupported,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogicalSurfaceBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl LogicalSurfaceBounds {
    pub fn new(x: i32, y: i32, width: u32, height: u32, scale_factor: f32) -> VibexResult<Self> {
        if width == 0 || height == 0 {
            return Err(VibexError::validation(
                "content_surface_bounds_empty",
                "content surface bounds must have non-zero width and height",
            ));
        }
        if !scale_factor.is_finite() || scale_factor <= 0.0 || scale_factor > 16.0 {
            return Err(VibexError::validation(
                "content_surface_scale_invalid",
                "content surface scale factor is invalid",
            ));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
            scale_factor,
        })
    }

    pub fn physical_size(self) -> (u32, u32) {
        let physical = |logical: u32| {
            ((logical as f64 * self.scale_factor as f64).round()).clamp(1.0, u32::MAX as f64) as u32
        };
        (physical(self.width), physical(self.height))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationDisposition {
    Applied,
    IgnoredStale,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentResourceMetrics {
    pub resident_items: usize,
    pub resident_bytes: usize,
    pub budget_items: usize,
    pub budget_bytes: usize,
    pub evictions: u64,
}

impl ContentResourceMetrics {
    pub fn is_within_budget(self) -> bool {
        self.resident_items <= self.budget_items && self.resident_bytes <= self.budget_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSurfaceDiagnostics {
    pub schema_version: &'static str,
    pub kind: ContentSurfaceKind,
    pub origin: ContentSurfaceOrigin,
    pub phase: ContentSurfacePhase,
    pub activation_generation: u64,
    pub visible: bool,
    pub focused: bool,
    pub focus_return_pending: bool,
    pub overlay_depth: u16,
    pub logical_bounds: Option<LogicalSurfaceBounds>,
    pub backend: String,
    pub backend_revision: String,
    pub resources: ContentResourceMetrics,
    pub crash_count: u64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContentSurfaceLifecycle {
    kind: ContentSurfaceKind,
    origin: ContentSurfaceOrigin,
    phase: ContentSurfacePhase,
    activation_generation: u64,
    activation_requested: bool,
    content_loaded: bool,
    visible: bool,
    focused: bool,
    focus_return_pending: bool,
    overlay_depth: u16,
    bounds: Option<LogicalSurfaceBounds>,
    crash_count: u64,
    last_error_code: Option<String>,
}

impl ContentSurfaceLifecycle {
    pub fn restored(kind: ContentSurfaceKind, origin: ContentSurfaceOrigin) -> Self {
        Self {
            kind,
            origin,
            phase: ContentSurfacePhase::ReadyToLoad,
            activation_generation: 0,
            activation_requested: false,
            content_loaded: false,
            visible: false,
            focused: false,
            focus_return_pending: false,
            overlay_depth: 0,
            bounds: None,
            crash_count: 0,
            last_error_code: None,
        }
    }

    pub fn kind(&self) -> ContentSurfaceKind {
        self.kind
    }

    pub fn origin(&self) -> ContentSurfaceOrigin {
        self.origin
    }

    pub fn phase(&self) -> ContentSurfacePhase {
        self.phase
    }

    pub fn activation_generation(&self) -> u64 {
        self.activation_generation
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn focus_return_pending(&self) -> bool {
        self.focus_return_pending
    }

    pub fn bounds(&self) -> Option<LogicalSurfaceBounds> {
        self.bounds
    }

    pub fn activate(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if generation == 0 {
            return Err(VibexError::validation(
                "content_surface_generation_zero",
                "content surface activation generation must be non-zero",
            ));
        }
        if generation < self.activation_generation {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if self.phase == ContentSurfacePhase::Closed && generation == self.activation_generation {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.activation_generation = generation;
        self.activation_requested = true;
        if self.phase == ContentSurfacePhase::Closed {
            self.overlay_depth = 0;
            self.focused = false;
            self.focus_return_pending = false;
        }
        self.refresh_visibility();
        Ok(GenerationDisposition::Applied)
    }

    pub fn mark_unsupported(
        &mut self,
        generation: u64,
        error_code: &str,
    ) -> VibexResult<GenerationDisposition> {
        if generation == 0 {
            return Err(VibexError::validation(
                "content_surface_generation_zero",
                "content surface activation generation must be non-zero",
            ));
        }
        if generation < self.activation_generation {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if self.phase == ContentSurfacePhase::Closed && generation == self.activation_generation {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.activation_generation = generation;
        self.activation_requested = false;
        self.content_loaded = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        self.overlay_depth = 0;
        self.phase = ContentSurfacePhase::Unsupported;
        self.last_error_code = Some(stable_error_code(error_code));
        Ok(GenerationDisposition::Applied)
    }

    pub fn begin_load(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.content_loaded = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        self.phase = ContentSurfacePhase::Loading;
        self.last_error_code = None;
        Ok(GenerationDisposition::Applied)
    }

    pub fn finish_load(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.content_loaded = true;
        self.last_error_code = None;
        self.refresh_visibility();
        Ok(GenerationDisposition::Applied)
    }

    pub fn deactivate(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.activation_requested = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        if self.phase != ContentSurfacePhase::Closed {
            self.phase = if self.content_loaded {
                ContentSurfacePhase::Inactive
            } else {
                ContentSurfacePhase::ReadyToLoad
            };
        }
        Ok(GenerationDisposition::Applied)
    }

    pub fn set_bounds(
        &mut self,
        generation: u64,
        bounds: LogicalSurfaceBounds,
    ) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.bounds = Some(bounds);
        Ok(GenerationDisposition::Applied)
    }

    pub fn overlay_opened(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if self.focused {
            self.focus_return_pending = true;
        }
        self.focused = false;
        self.overlay_depth = self.overlay_depth.saturating_add(1);
        self.refresh_visibility();
        Ok(GenerationDisposition::Applied)
    }

    pub fn focus_entered(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        if !self.visible {
            return Err(VibexError::conflict(
                "content_surface_focus_unavailable",
                "content surface cannot receive focus while hidden or inactive",
            ));
        }
        self.focused = true;
        self.focus_return_pending = false;
        Ok(GenerationDisposition::Applied)
    }

    pub fn focus_left(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.focused = false;
        Ok(GenerationDisposition::Applied)
    }

    pub fn overlay_closed(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.overlay_depth = self.overlay_depth.saturating_sub(1);
        self.refresh_visibility();
        Ok(GenerationDisposition::Applied)
    }

    pub fn crashed(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.crash_count = self.crash_count.saturating_add(1);
        self.content_loaded = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        self.phase = ContentSurfacePhase::Crashed;
        Ok(GenerationDisposition::Applied)
    }

    pub fn failed(
        &mut self,
        generation: u64,
        error_code: &str,
    ) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.last_error_code = Some(stable_error_code(error_code));
        self.content_loaded = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        self.phase = ContentSurfacePhase::Error;
        Ok(GenerationDisposition::Applied)
    }

    pub fn close(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        if self.reject_stale(generation)? {
            return Ok(GenerationDisposition::IgnoredStale);
        }
        self.activation_requested = false;
        self.content_loaded = false;
        self.visible = false;
        self.focused = false;
        self.focus_return_pending = false;
        self.overlay_depth = 0;
        self.phase = ContentSurfacePhase::Closed;
        Ok(GenerationDisposition::Applied)
    }

    pub fn diagnostics(
        &self,
        backend: impl Into<String>,
        backend_revision: impl Into<String>,
        resources: ContentResourceMetrics,
    ) -> ContentSurfaceDiagnostics {
        ContentSurfaceDiagnostics {
            schema_version: NATIVE_CONTENT_DIAGNOSTIC_SCHEMA,
            kind: self.kind,
            origin: self.origin,
            phase: self.phase,
            activation_generation: self.activation_generation,
            visible: self.visible,
            focused: self.focused,
            focus_return_pending: self.focus_return_pending,
            overlay_depth: self.overlay_depth,
            logical_bounds: self.bounds,
            backend: backend.into(),
            backend_revision: backend_revision.into(),
            resources,
            crash_count: self.crash_count,
            last_error_code: self.last_error_code.clone(),
        }
    }

    fn reject_stale(&self, generation: u64) -> VibexResult<bool> {
        if generation == 0 {
            return Err(VibexError::validation(
                "content_surface_generation_zero",
                "content surface activation generation must be non-zero",
            ));
        }
        Ok(generation != self.activation_generation || self.phase == ContentSurfacePhase::Closed)
    }

    fn refresh_visibility(&mut self) {
        self.visible = self.activation_requested && self.content_loaded && self.overlay_depth == 0;
        self.phase = if !self.content_loaded {
            if self.phase == ContentSurfacePhase::Loading {
                ContentSurfacePhase::Loading
            } else {
                ContentSurfacePhase::ReadyToLoad
            }
        } else if !self.activation_requested {
            ContentSurfacePhase::Inactive
        } else if self.overlay_depth > 0 {
            ContentSurfacePhase::HiddenForOverlay
        } else {
            ContentSurfacePhase::Active
        };
    }
}

fn stable_error_code(value: &str) -> String {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        value.to_string()
    } else {
        "invalid_code".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> LogicalSurfaceBounds {
        LogicalSurfaceBounds::new(12, 20, 800, 500, 1.5).unwrap()
    }

    #[test]
    fn stale_generation_cannot_show_or_move_a_surface() {
        let mut lifecycle = ContentSurfaceLifecycle::restored(
            ContentSurfaceKind::Pdf,
            ContentSurfaceOrigin::Preview,
        );
        lifecycle.activate(4).unwrap();
        lifecycle.begin_load(4).unwrap();
        lifecycle.finish_load(4).unwrap();
        lifecycle.activate(5).unwrap();

        assert_eq!(
            lifecycle.set_bounds(4, bounds()).unwrap(),
            GenerationDisposition::IgnoredStale
        );
        assert_eq!(
            lifecycle.finish_load(4).unwrap(),
            GenerationDisposition::IgnoredStale
        );
        assert_eq!(lifecycle.activation_generation(), 5);
        assert_eq!(lifecycle.bounds(), None);
    }

    #[test]
    fn overlay_hides_and_restores_only_the_current_surface() {
        let mut lifecycle = ContentSurfaceLifecycle::restored(
            ContentSurfaceKind::Pdf,
            ContentSurfaceOrigin::Preview,
        );
        lifecycle.activate(1).unwrap();
        lifecycle.begin_load(1).unwrap();
        lifecycle.finish_load(1).unwrap();
        lifecycle.focus_entered(1).unwrap();
        assert!(lifecycle.visible());
        assert!(lifecycle.focused());

        lifecycle.overlay_opened(1).unwrap();
        assert!(!lifecycle.visible());
        assert!(!lifecycle.focused());
        assert!(lifecycle.focus_return_pending());
        assert_eq!(lifecycle.phase(), ContentSurfacePhase::HiddenForOverlay);
        lifecycle.activate(2).unwrap();
        lifecycle.overlay_closed(1).unwrap();
        assert!(!lifecycle.visible());
        lifecycle.overlay_closed(2).unwrap();
        assert!(lifecycle.visible());
        assert!(lifecycle.focus_return_pending());
        lifecycle.focus_entered(2).unwrap();
        assert!(lifecycle.focused());
        assert!(!lifecycle.focus_return_pending());
    }

    #[test]
    fn close_fences_same_generation_callbacks_until_a_new_activation() {
        let mut lifecycle = ContentSurfaceLifecycle::restored(
            ContentSurfaceKind::Pdf,
            ContentSurfaceOrigin::Preview,
        );
        lifecycle.activate(4).unwrap();
        lifecycle.begin_load(4).unwrap();
        lifecycle.finish_load(4).unwrap();
        lifecycle.focus_entered(4).unwrap();
        lifecycle.close(4).unwrap();

        assert_eq!(
            lifecycle.finish_load(4).unwrap(),
            GenerationDisposition::IgnoredStale
        );
        assert_eq!(
            lifecycle.set_bounds(4, bounds()).unwrap(),
            GenerationDisposition::IgnoredStale
        );
        assert_eq!(
            lifecycle.focus_entered(4).unwrap(),
            GenerationDisposition::IgnoredStale
        );
        assert_eq!(lifecycle.phase(), ContentSurfacePhase::Closed);
        assert!(!lifecycle.visible());
        assert!(!lifecycle.focused());

        assert_eq!(
            lifecycle.activate(5).unwrap(),
            GenerationDisposition::Applied
        );
        lifecycle.begin_load(5).unwrap();
        lifecycle.finish_load(5).unwrap();
        lifecycle.focus_entered(5).unwrap();
        assert_eq!(lifecycle.phase(), ContentSurfacePhase::Active);
        assert!(lifecycle.focused());
    }

    #[test]
    fn diagnostics_retain_codes_and_counts_but_no_content_fields() {
        let mut lifecycle = ContentSurfaceLifecycle::restored(
            ContentSurfaceKind::Pdf,
            ContentSurfaceOrigin::Preview,
        );
        lifecycle.activate(7).unwrap();
        lifecycle.failed(7, "PDF path: /home/private.pdf").unwrap();
        let diagnostic = lifecycle.diagnostics(
            "pdfium",
            "7881",
            ContentResourceMetrics {
                resident_items: 2,
                resident_bytes: 1024,
                budget_items: 8,
                budget_bytes: 4096,
                evictions: 1,
            },
        );
        let json = serde_json::to_string(&diagnostic).unwrap();
        assert!(json.contains("invalid_code"));
        assert!(!json.contains("private.pdf"));
        assert!(diagnostic.resources.is_within_budget());
    }

    #[test]
    fn physical_bounds_round_once_at_the_scale_boundary() {
        assert_eq!(bounds().physical_size(), (1200, 750));
    }
}
