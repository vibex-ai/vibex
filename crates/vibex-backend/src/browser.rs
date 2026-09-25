//! Platform-neutral embedded-browser seam consumed by shared GPUI controllers.
//!
//! This mirrors the terminal seam: clients read projections, mutate through
//! [`MutationRequest`] wrappers and watch pixels through a frame subscription.
//! They never hold a CDP connection — the embedded browser is a tool panel
//! owned by the authority runtime.

use std::fmt;

use serde::{Deserialize, Serialize};
use vibex_core::{
    BrowserActionRecord, BrowserAvailability, BrowserFrame, BrowserSession, BrowserSessionId,
    BrowserSessionSnapshot, BrowserTab, BrowserTabId, ErrorCategory, RedactedDiagnostic,
    VibexSessionId, WorkspaceId,
};

use crate::{
    BackendBound, BackendError, BackendErrorKind, BackendFuture, BackendResult, MutationRequest,
};

/// One screencast step for a tab.
///
/// `frame` is `None` when nothing new was captured and the batch exists only to
/// report `dropped_frames` or `reset_required`. `Eq` is not derived because the
/// frame metadata carries `f64` geometry.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserFrameBatch {
    pub tab_id: BrowserTabId,
    pub frame: Option<BrowserFrame>,
    pub next_sequence: u64,
    pub dropped_frames: u64,
    pub reset_required: bool,
}

impl fmt::Debug for BrowserFrameBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserFrameBatch")
            .field("tab_id", &self.tab_id)
            .field("has_frame", &self.frame.is_some())
            .field(
                "byte_len",
                &self.frame.as_ref().map_or(0, |frame| frame.bytes.len()),
            )
            .field("next_sequence", &self.next_sequence)
            .field("dropped_frames", &self.dropped_frames)
            .field("reset_required", &self.reset_required)
            .finish()
    }
}

/// Converts the wire payload into the runtime's input type.
///
/// The two types are deliberately separate: this one must stay `Serialize`
/// because a remote client sends it, while the runtime's carries no wire
/// contract.
pub fn payload_to_browser_input(payload: &BrowserInputPayload) -> vibex_browser::BrowserInput {
    use vibex_browser::BrowserInput;
    match payload {
        BrowserInputPayload::MouseMove { x, y, buttons } => BrowserInput::MouseMove {
            x: *x,
            y: *y,
            buttons: *buttons,
        },
        BrowserInputPayload::MouseDown {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => BrowserInput::MouseDown {
            x: *x,
            y: *y,
            button: button.clone(),
            click_count: *click_count,
            modifiers: *modifiers,
        },
        BrowserInputPayload::MouseUp {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => BrowserInput::MouseUp {
            x: *x,
            y: *y,
            button: button.clone(),
            click_count: *click_count,
            modifiers: *modifiers,
        },
        BrowserInputPayload::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => BrowserInput::Wheel {
            x: *x,
            y: *y,
            delta_x: *delta_x,
            delta_y: *delta_y,
        },
        BrowserInputPayload::Key {
            event_type,
            key,
            code,
            text,
            modifiers,
            windows_key_code,
        } => BrowserInput::Key {
            event_type: event_type.clone(),
            key: key.clone(),
            code: code.clone(),
            text: text.clone(),
            modifiers: *modifiers,
            windows_key_code: *windows_key_code,
        },
        BrowserInputPayload::InsertText { text } => BrowserInput::InsertText { text: text.clone() },
    }
}

pub trait BrowserFrameSubscription: BackendBound {
    fn next(&mut self) -> BackendFuture<'_, Option<BrowserFrameBatch>>;
}

pub trait BrowserBackend: BackendBound {
    /// Reports whether an embedded browser can be launched on the authority
    /// host, and which installation would be used.
    fn browser_availability(&self) -> BackendFuture<'_, BrowserAvailability>;

    fn list_browser_sessions(&self) -> BackendFuture<'_, Vec<BrowserSession>>;

    fn browser_session_snapshot(
        &self,
        session_id: BrowserSessionId,
    ) -> BackendFuture<'_, BrowserSessionSnapshot>;

    fn browser_ledger(
        &self,
        session_id: BrowserSessionId,
    ) -> BackendFuture<'_, Vec<BrowserActionRecord>>;

    /// Opens, or returns, the browser session for a workspace or agent session.
    ///
    /// The payload is a wire-safe stand-in: mapping it onto
    /// `vibex_browser::BrowserSessionKey` is the implementation's job, because
    /// that key is runtime state and deliberately not serializable.
    fn ensure_browser_session(
        &self,
        request: MutationRequest<BrowserSessionOpenRequest>,
    ) -> BackendFuture<'_, BrowserSessionId>;

    fn create_browser_tab(
        &self,
        request: MutationRequest<BrowserTabOpenRequest>,
    ) -> BackendFuture<'_, BrowserTab>;

    fn close_browser_tab(&self, request: MutationRequest<BrowserTabId>) -> BackendFuture<'_, ()>;

    fn select_browser_tab(
        &self,
        request: MutationRequest<BrowserTabSelection>,
    ) -> BackendFuture<'_, ()>;

    fn subscribe_browser_frames(
        &self,
        tab_id: BrowserTabId,
        next_sequence: u64,
    ) -> BackendResult<Box<dyn BrowserFrameSubscription>>;

    fn set_browser_viewport(
        &self,
        request: MutationRequest<BrowserViewportRequest>,
    ) -> BackendFuture<'_, ()>;

    fn send_browser_input(
        &self,
        request: MutationRequest<BrowserInputRequest>,
    ) -> BackendFuture<'_, ()>;

    fn resolve_browser_dialog(
        &self,
        request: MutationRequest<BrowserDialogResolution>,
    ) -> BackendFuture<'_, ()>;

    /// Stops the screencast for a tab without closing it. Idempotent: a tab that
    /// is already not casting is not an error.
    fn stop_browser_screencast(&self, tab_id: BrowserTabId) -> BackendFuture<'_, ()>;
}

/// Payload for [`BrowserBackend::ensure_browser_session`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSessionOpenRequest {
    pub workspace_id: Option<WorkspaceId>,
    pub agent_session_id: Option<VibexSessionId>,
}

/// Payload for [`BrowserBackend::create_browser_tab`]. `url` defaults to a blank
/// page when `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTabOpenRequest {
    pub session_id: BrowserSessionId,
    pub url: Option<String>,
}

/// Payload for [`BrowserBackend::select_browser_tab`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTabSelection {
    pub session_id: BrowserSessionId,
    pub tab_id: BrowserTabId,
}

/// Payload for [`BrowserBackend::set_browser_viewport`]. Sizes are CSS pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserViewportRequest {
    pub tab_id: BrowserTabId,
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
}

/// Payload for [`BrowserBackend::send_browser_input`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInputRequest {
    pub tab_id: BrowserTabId,
    pub input: BrowserInputPayload,
}

/// Panel input in viewport CSS pixels.
///
/// Mirrors `vibex_browser::BrowserInput` minus `Resize`, which has its own
/// method because it drives `Emulation.setDeviceMetricsOverride` rather than
/// delivering an input event.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserInputPayload {
    MouseMove {
        x: f64,
        y: f64,
        /// Bitmask of the buttons held during the move; Chrome needs it to see
        /// a drag rather than a series of hovers.
        #[serde(default)]
        buttons: i32,
    },
    MouseDown {
        x: f64,
        y: f64,
        button: String,
        #[serde(alias = "clickCount")]
        click_count: i32,
        modifiers: i32,
    },
    MouseUp {
        x: f64,
        y: f64,
        button: String,
        #[serde(alias = "clickCount")]
        click_count: i32,
        modifiers: i32,
    },
    Wheel {
        x: f64,
        y: f64,
        #[serde(alias = "deltaX")]
        delta_x: f64,
        #[serde(alias = "deltaY")]
        delta_y: f64,
    },
    /// A raw key event. `event_type` is `keyDown`, `keyUp` or `rawKeyDown`.
    Key {
        #[serde(alias = "eventType")]
        event_type: String,
        key: String,
        code: String,
        text: Option<String>,
        modifiers: i32,
        #[serde(alias = "windowsKeyCode")]
        windows_key_code: i32,
    },
    /// Committed IME or paste text.
    InsertText { text: String },
}

impl fmt::Debug for BrowserInputPayload {
    /// Never prints typed text: input is as sensitive as terminal input, so
    /// `insert_text` and the `key` text field report their length instead.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MouseMove { x, y, buttons } => formatter
                .debug_struct("BrowserInputPayload::MouseMove")
                .field("x", x)
                .field("y", y)
                .field("buttons", buttons)
                .finish(),
            Self::MouseDown {
                x,
                y,
                button,
                click_count,
                modifiers,
            } => formatter
                .debug_struct("BrowserInputPayload::MouseDown")
                .field("x", x)
                .field("y", y)
                .field("button", button)
                .field("click_count", click_count)
                .field("modifiers", modifiers)
                .finish(),
            Self::MouseUp {
                x,
                y,
                button,
                click_count,
                modifiers,
            } => formatter
                .debug_struct("BrowserInputPayload::MouseUp")
                .field("x", x)
                .field("y", y)
                .field("button", button)
                .field("click_count", click_count)
                .field("modifiers", modifiers)
                .finish(),
            Self::Wheel {
                x,
                y,
                delta_x,
                delta_y,
            } => formatter
                .debug_struct("BrowserInputPayload::Wheel")
                .field("x", x)
                .field("y", y)
                .field("delta_x", delta_x)
                .field("delta_y", delta_y)
                .finish(),
            Self::Key {
                event_type,
                key,
                code,
                text,
                modifiers,
                windows_key_code,
            } => formatter
                .debug_struct("BrowserInputPayload::Key")
                .field("event_type", event_type)
                .field("key", key)
                .field("code", code)
                .field("text_len", &text.as_ref().map(|text| text.chars().count()))
                .field("modifiers", modifiers)
                .field("windows_key_code", windows_key_code)
                .finish(),
            Self::InsertText { text } => formatter
                .debug_struct("BrowserInputPayload::InsertText")
                .field("text_len", &text.chars().count())
                .finish(),
        }
    }
}

/// Payload for [`BrowserBackend::resolve_browser_dialog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserDialogResolution {
    pub tab_id: BrowserTabId,
    pub accept: bool,
    /// Answer for a `prompt` dialog. Ignored for `alert`/`confirm`.
    pub prompt_text: Option<String>,
}

impl From<vibex_browser::BrowserError> for BackendError {
    fn from(error: vibex_browser::BrowserError) -> Self {
        let kind = match error.category {
            ErrorCategory::Capability => BackendErrorKind::Unsupported,
            ErrorCategory::Permission => BackendErrorKind::Permission,
            ErrorCategory::Conflict => BackendErrorKind::Conflict,
            ErrorCategory::Validation
            | ErrorCategory::Provider
            | ErrorCategory::Process
            | ErrorCategory::Storage
            | ErrorCategory::Remote => BackendErrorKind::Failed,
        };
        Self {
            kind,
            code: error.code,
            message: error.message,
            recovery_hint: error.recovery_hint,
            correlation_id: None,
            diagnostics: error
                .diagnostics
                .into_iter()
                .map(|(key, value)| RedactedDiagnostic { key, value })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use vibex_core::{BrowserFrameFormat, BrowserFrameMetadata};

    use super::*;

    fn frame_with(bytes: &[u8]) -> BrowserFrame {
        BrowserFrame {
            tab_id: BrowserTabId::new(),
            sequence: 4,
            format: BrowserFrameFormat::Jpeg,
            bytes: bytes.to_vec(),
            metadata: BrowserFrameMetadata::default(),
        }
    }

    #[test]
    fn browser_frame_batch_debug_exposes_metadata_without_frame_bytes() {
        let frame = frame_with(b"frame-secret-pixels");
        let batch = BrowserFrameBatch {
            tab_id: BrowserTabId::new(),
            frame: Some(frame),
            next_sequence: 5,
            dropped_frames: 2,
            reset_required: true,
        };
        let debug = format!("{batch:?}");

        assert!(debug.contains("has_frame: true"));
        assert!(debug.contains("byte_len: 19"));
        assert!(debug.contains("next_sequence: 5"));
        assert!(debug.contains("dropped_frames: 2"));
        assert!(debug.contains("reset_required: true"));
        assert!(!debug.contains("frame-secret-pixels"));

        let empty = BrowserFrameBatch {
            tab_id: BrowserTabId::new(),
            frame: None,
            next_sequence: 6,
            dropped_frames: 0,
            reset_required: false,
        };
        let debug = format!("{empty:?}");

        assert!(debug.contains("has_frame: false"));
        assert!(debug.contains("byte_len: 0"));
    }

    #[test]
    fn browser_input_debug_never_prints_typed_text() {
        let insert = BrowserInputPayload::InsertText {
            text: "input-secret".to_string(),
        };
        let debug = format!("{insert:?}");

        assert!(debug.contains("InsertText"));
        assert!(debug.contains("text_len: 12"));
        assert!(!debug.contains("input-secret"));

        let key = BrowserInputPayload::Key {
            event_type: "keyDown".to_string(),
            key: "a".to_string(),
            code: "KeyA".to_string(),
            text: Some("key-secret".to_string()),
            modifiers: 0,
            windows_key_code: 65,
        };
        let debug = format!("{key:?}");

        assert!(debug.contains("event_type: \"keyDown\""));
        assert!(debug.contains("text_len: Some(10)"));
        assert!(!debug.contains("key-secret"));
    }

    #[test]
    fn browser_input_payload_accepts_snake_case_and_camel_case_fields() {
        let canonical: BrowserInputPayload = serde_json::from_str(
            r#"{"kind":"key","event_type":"keyDown","key":"a","code":"KeyA","text":null,"modifiers":0,"windows_key_code":65}"#,
        )
        .expect("canonical payload decodes");
        let aliased: BrowserInputPayload = serde_json::from_str(
            r#"{"kind":"key","eventType":"keyDown","key":"a","code":"KeyA","text":null,"modifiers":0,"windowsKeyCode":65}"#,
        )
        .expect("camelCase payload decodes through aliases");
        assert_eq!(canonical, aliased);

        let encoded = serde_json::to_string(&canonical).expect("payload encodes");
        assert!(encoded.contains(r#""kind":"key""#));
        assert!(encoded.contains(r#""windows_key_code":65"#));

        let wheel: BrowserInputPayload =
            serde_json::from_str(r#"{"kind":"wheel","x":1.0,"y":2.0,"deltaX":3.0,"deltaY":4.0}"#)
                .expect("wheel aliases decode");
        assert_eq!(
            wheel,
            BrowserInputPayload::Wheel {
                x: 1.0,
                y: 2.0,
                delta_x: 3.0,
                delta_y: 4.0,
            }
        );
    }

    #[test]
    fn browser_error_maps_categories_and_keeps_diagnostics() {
        let cases = [
            (
                vibex_browser::BrowserError::validation("bad", "bad input"),
                BackendErrorKind::Failed,
            ),
            (
                vibex_browser::BrowserError::capability("missing", "no browser"),
                BackendErrorKind::Unsupported,
            ),
            (
                vibex_browser::BrowserError::permission("denied", "not allowed"),
                BackendErrorKind::Permission,
            ),
            (
                vibex_browser::BrowserError::process("dead", "process exited"),
                BackendErrorKind::Failed,
            ),
            (
                vibex_browser::BrowserError::storage("disk", "write failed"),
                BackendErrorKind::Failed,
            ),
            (
                vibex_browser::BrowserError::conflict("stale", "generation moved"),
                BackendErrorKind::Conflict,
            ),
        ];

        for (source, expected) in cases {
            let code = source.code.clone();
            let mapped = BackendError::from(source);
            assert_eq!(mapped.kind, expected);
            assert_eq!(mapped.code, code);
        }

        let mapped = BackendError::from(
            vibex_browser::BrowserError::process("dead", "process exited")
                .with_recovery_hint("relaunch the browser")
                .with_diagnostic("phase", "spawn"),
        );
        assert_eq!(
            mapped.recovery_hint.as_deref(),
            Some("relaunch the browser")
        );
        assert_eq!(mapped.diagnostics.len(), 1);
        assert_eq!(mapped.diagnostics[0].key, "phase");
        assert_eq!(mapped.diagnostics[0].value, "spawn");
    }
}
