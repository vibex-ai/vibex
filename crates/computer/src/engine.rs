//! The engine seam: what the runtime needs from a desktop-automation backend.
//!
//! The runtime never talks to a native automation API directly. It talks to
//! one **engine** through this trait, and the shipped engine is the signed
//! third-party driver the helper process wraps. Two properties of the seam are
//! load-bearing:
//!
//! * **The trait is Vibex's vocabulary, not the engine's.** Tool names, result
//!   shapes and delivery modes are defined here, so replacing the engine never
//!   changes what an Agent sees or what the approval cards say. The upstream
//!   driver's own tool surface is explicitly *not* passed through — it is not
//!   even stable between its own contract snapshot and its runtime registry.
//! * **Unknown tool facts stay unknown.** The engine reports
//!   [`EngineAppState::degraded`] and `degraded_reason` instead of an empty
//!   element list when it cannot read the tree, because "this window has no
//!   controls" and "the accessibility bridge is missing" demand opposite
//!   responses.

use async_trait::async_trait;

use vibex_core::{
    ComputerApplication, ComputerDeliveryMode, ComputerPermissionReport, ComputerPlatform,
    ComputerRect, ComputerScreenshot, ComputerUnverifiedReason,
};

use crate::error::ComputerResult;

/// One element of a tree the engine returned.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineElement {
    /// Engine-local index; the service turns it into a generation-scoped
    /// reference.
    pub index: usize,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub editable: bool,
    pub secure: bool,
    pub disabled: bool,
    pub bounds: Option<ComputerRect>,
}

/// A request for one application's accessibility state.
#[derive(Debug, Clone)]
pub struct EngineStateRequest {
    pub app_id: String,
    pub window_id: Option<String>,
    pub max_elements: usize,
    /// Include every element, not just the addressable ones.
    pub extended: bool,
    /// Ask for pixels as well. The engine may refuse on a platform where
    /// capture is not permitted.
    pub screenshot: bool,
}

/// One application's accessibility state.
#[derive(Debug, Clone)]
pub struct EngineAppState {
    pub app: ComputerApplication,
    pub window_id: Option<String>,
    pub window_title: Option<String>,
    /// Digest of the tree, used to detect that references went stale.
    pub tree_digest: String,
    pub elements: Vec<EngineElement>,
    pub truncated: bool,
    pub degraded: bool,
    pub degraded_reason: Option<String>,
    pub screenshot: Option<ComputerScreenshot>,
    /// The window rectangle in desktop coordinates, for the self-target guard.
    pub window_bounds: Option<ComputerRect>,
}

/// Where an input action should be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineDelivery {
    Background,
    Foreground,
}

impl EngineDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Foreground => "foreground",
        }
    }

    pub fn from_mode(mode: ComputerDeliveryMode) -> Self {
        match mode {
            ComputerDeliveryMode::Background => Self::Background,
            ComputerDeliveryMode::Foreground => Self::Foreground,
        }
    }
}

/// One click request.
#[derive(Debug, Clone)]
pub struct EngineClick {
    pub app_id: String,
    pub window_id: Option<String>,
    /// Element index when the caller had a fresh reference.
    pub element_index: Option<usize>,
    /// Desktop coordinates when the caller clicked a point.
    pub point: Option<(f64, f64)>,
    /// `left` or `right`.
    pub button: String,
    pub click_count: u32,
    pub delivery: EngineDelivery,
}

/// One text request.
#[derive(Debug, Clone)]
pub struct EngineTypeText {
    pub app_id: String,
    pub window_id: Option<String>,
    /// The text to type. Never logged, never audited.
    pub text: String,
    pub delivery: EngineDelivery,
}

/// One semantic value write.
#[derive(Debug, Clone)]
pub struct EngineSetValue {
    pub app_id: String,
    pub window_id: Option<String>,
    pub element_index: usize,
    /// The value to write. Never logged, never audited.
    pub value: String,
    pub delivery: EngineDelivery,
}

/// One key press.
#[derive(Debug, Clone)]
pub struct EnginePressKey {
    pub app_id: String,
    pub window_id: Option<String>,
    pub key: String,
    pub modifiers: Vec<String>,
    pub delivery: EngineDelivery,
}

/// One scroll request.
#[derive(Debug, Clone)]
pub struct EngineScroll {
    pub app_id: String,
    pub window_id: Option<String>,
    pub element_index: Option<usize>,
    pub point: Option<(f64, f64)>,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delivery: EngineDelivery,
}

/// What the engine says happened.
#[derive(Debug, Clone)]
pub struct EngineActionResult {
    /// The engine asserted the effect (an accessibility action with a
    /// post-check, or a state comparison the engine performed itself).
    pub asserted: bool,
    /// Why the effect is unproven when it is not asserted. `None` with
    /// `asserted == false` means the engine sent no metadata at all, which the
    /// class contract maps to
    /// [`ComputerUnverifiedReason::MissingMetadata`].
    pub unverified_reason: Option<ComputerUnverifiedReason>,
    /// A short engine-supplied detail, safe to show in the ledger.
    pub detail: Option<String>,
    /// Where the pointer ended up, when the engine knows.
    pub cursor: Option<(f64, f64)>,
    /// The tree digest after the action, when the engine re-read the tree.
    pub tree_digest_after: Option<String>,
}

impl EngineActionResult {
    pub fn asserted() -> Self {
        Self {
            asserted: true,
            unverified_reason: None,
            detail: None,
            cursor: None,
            tree_digest_after: None,
        }
    }

    pub fn unverified(reason: ComputerUnverifiedReason) -> Self {
        Self {
            asserted: false,
            unverified_reason: Some(reason),
            detail: None,
            cursor: None,
            tree_digest_after: None,
        }
    }
}

/// What the engine reports about itself and this machine.
#[derive(Debug, Clone)]
pub struct EngineProbe {
    /// The engine's own name and version.
    pub engine: Option<String>,
    pub platform: ComputerPlatform,
    pub permissions: ComputerPermissionReport,
    /// The engine's tool-surface fingerprint, pinned next to the engine version
    /// so an upgrade that changes the surface is visible.
    pub tool_surface: Option<String>,
    /// Per-capability degradations, passed through rather than flattened.
    pub degraded: Vec<String>,
    /// Reasons the engine is unusable here, in the runtime's vocabulary.
    pub unavailable_reason: Option<vibex_core::ComputerUnavailableReason>,
    pub detail: Option<String>,
}

/// The desktop-automation backend.
///
/// Every method is fallible and every failure carries a code. Implementations
/// must not translate a missing capability into an empty successful result:
/// that is the silent failure this whole module exists to prevent.
#[async_trait]
pub trait ComputerEngine: Send + Sync + 'static {
    /// Reports the engine's readiness. Must be read-only: probing never
    /// installs, never prompts and never starts an application.
    async fn probe(&self) -> ComputerResult<EngineProbe>;

    async fn list_apps(&self) -> ComputerResult<Vec<ComputerApplication>>;

    async fn get_app_state(&self, request: EngineStateRequest) -> ComputerResult<EngineAppState>;

    async fn click(&self, request: EngineClick) -> ComputerResult<EngineActionResult>;

    async fn type_text(&self, request: EngineTypeText) -> ComputerResult<EngineActionResult>;

    async fn set_value(&self, request: EngineSetValue) -> ComputerResult<EngineActionResult>;

    async fn press_key(&self, request: EnginePressKey) -> ComputerResult<EngineActionResult>;

    async fn scroll(&self, request: EngineScroll) -> ComputerResult<EngineActionResult>;

    /// Captures one window or the desktop without touching the tree.
    async fn screenshot(
        &self,
        app_id: Option<&str>,
        window_id: Option<&str>,
    ) -> ComputerResult<Option<ComputerScreenshot>>;

    /// Releases every mouse button and key the engine may be holding.
    ///
    /// This is the emergency-stop primitive: a key left down makes the whole
    /// machine unusable for its owner, which is the most severe secondary
    /// failure this feature can cause.
    async fn release_all_keys(&self) -> ComputerResult<()>;

    /// How long ago the human last produced input, when the engine can tell.
    async fn user_activity_age_ms(&self) -> ComputerResult<Option<i64>>;

    /// Starts an application. Used by the CLI and the panel; the policy for it
    /// lives in the service, not here.
    async fn launch_app(&self, app_id: &str) -> ComputerResult<ComputerApplication>;

    /// Terminates an application.
    async fn kill_app(&self, app_id: &str) -> ComputerResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_modes_map_onto_engine_delivery() {
        assert_eq!(
            EngineDelivery::from_mode(ComputerDeliveryMode::Background),
            EngineDelivery::Background
        );
        assert_eq!(
            EngineDelivery::from_mode(ComputerDeliveryMode::Foreground),
            EngineDelivery::Foreground
        );
        assert_eq!(EngineDelivery::Foreground.as_str(), "foreground");
    }

    #[test]
    fn an_action_result_without_metadata_is_not_asserted() {
        let result = EngineActionResult {
            asserted: false,
            unverified_reason: None,
            detail: None,
            cursor: None,
            tree_digest_after: None,
        };
        assert!(!result.asserted);
        assert!(result.unverified_reason.is_none());
        assert!(EngineActionResult::asserted().asserted);
    }
}
