//! Authority-agnostic browser transport for the desktop browser panel.
//!
//! The panel paints encoded screencast frames and forwards input, but never
//! owns the browser process or the CDP connection. In native mode those come
//! from the in-process runtime; with a paired runtime the same contract is
//! served through [`BrowserBackend`] over Remote v2.
//!
//! Remote browser frames are an explicit later phase. Rather than failing
//! silently — or, worse, showing a blank panel that looks broken — the remote
//! transport reports `remote_browser_unavailable` and the surface renders that
//! reason. The architecture baseline requires degrading with an explicit
//! message, not a silent failure.

use std::{future::Future, pin::Pin, sync::Arc};

use gpui::{BackgroundExecutor, Task};
use vibex_backend::{BackendError, BrowserBackend, BrowserInputPayload};
use vibex_browser::{
    BrowserFrameSubscription, BrowserInput, BrowserService, BrowserServiceEvent, BrowserSessionKey,
};
use vibex_core::{
    BrowserAvailability, BrowserFrame, BrowserSession, BrowserSessionId, BrowserSessionSnapshot,
    BrowserTab, BrowserTabId, BrowserTabOwner, BrowserToolTier, BrowserUnavailableReason,
    VibexError, VibexSessionId, WorkspaceId,
};

/// Failure reported by a [`BrowserTransport`] operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserTransportError {
    pub code: String,
    pub message: String,
}

impl BrowserTransportError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// True when the failure is "this runtime cannot show a browser", which the
    /// panel renders as a reason rather than as an error.
    pub fn is_unavailable(&self) -> bool {
        self.code.starts_with("browser_unavailable")
            || self.code == "remote_browser_unavailable"
            || self.code == "browser_not_running"
    }
}

impl From<VibexError> for BrowserTransportError {
    fn from(error: VibexError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<BackendError> for BrowserTransportError {
    fn from(error: BackendError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<vibex_browser::BrowserError> for BrowserTransportError {
    fn from(error: vibex_browser::BrowserError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

pub type BrowserTransportResult<T> = Result<T, BrowserTransportError>;

pub type BrowserTransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = BrowserTransportResult<T>> + Send + 'a>>;

/// A stream of encoded screencast frames for one tab.
///
/// Frames are latest-value: the transport hands back whatever the runtime most
/// recently encoded, and acknowledging a frame is what releases the browser to
/// encode the next one.
pub enum BrowserFrameStream {
    Local(LocalFrameStream),
    /// The remote transport has no frame channel yet. Yielding `None` once is
    /// the honest answer: it tells the panel there will never be a frame, so it
    /// can show the reason instead of spinning.
    Unavailable,
}

pub struct LocalFrameStream {
    subscription: BrowserFrameSubscription,
}

impl BrowserFrameStream {
    /// Waits for the next frame, or `None` when the stream is over.
    pub async fn next(&mut self) -> Option<BrowserFrame> {
        match self {
            Self::Local(stream) => stream.subscription.next().await,
            Self::Unavailable => None,
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        match self {
            Self::Local(stream) => stream.subscription.dropped_frames(),
            Self::Unavailable => 0,
        }
    }
}

/// Operations the desktop browser panel needs from whichever authority owns the
/// browser.
pub trait BrowserTransport: Send + Sync + 'static {
    /// Upcasts to `Any` so the panel can reach the concrete transport when it
    /// needs an operation the trait does not model (answering a dialog, for
    /// instance, which only the in-process service can do today).
    fn as_any(&self) -> &dyn std::any::Any;

    /// Whether a browser is usable, and if not, why.
    fn availability(&self) -> BrowserTransportFuture<'_, BrowserAvailability>;

    fn list_sessions(&self) -> BrowserTransportFuture<'_, Vec<BrowserSession>>;

    fn session_snapshot(
        &self,
        session_id: &BrowserSessionId,
    ) -> BrowserTransportFuture<'_, BrowserSessionSnapshot>;

    /// Returns the session backing a workspace's panel, creating it if needed.
    fn ensure_workspace_session(
        &self,
        workspace_id: &WorkspaceId,
    ) -> BrowserTransportFuture<'_, BrowserSessionId>;

    fn create_tab(
        &self,
        session_id: &BrowserSessionId,
        url: Option<&str>,
    ) -> BrowserTransportFuture<'_, BrowserTab>;

    fn close_tab(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;

    fn select_tab(
        &self,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, ()>;

    fn set_viewport(
        &self,
        tab_id: &BrowserTabId,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> BrowserTransportFuture<'_, ()>;

    fn dispatch_input(
        &self,
        tab_id: &BrowserTabId,
        input: BrowserInput,
    ) -> BrowserTransportFuture<'_, ()>;

    /// Navigates on behalf of the human driving the panel.
    fn navigate(&self, tab_id: &BrowserTabId, url: &str) -> BrowserTransportFuture<'_, ()>;

    fn reload(&self, tab_id: &BrowserTabId, ignore_cache: bool) -> BrowserTransportFuture<'_, ()>;

    /// Opens the frame stream and turns the screencast on.
    fn subscribe_frames(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, BrowserFrameStream>;

    /// Stops the screencast. The target and the page state stay alive.
    fn stop_screencast(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;
}

/// Browser transport backed by the in-process runtime.
pub struct LocalBrowserTransport {
    service: BrowserService,
    executor: BackgroundExecutor,
}

impl Clone for LocalBrowserTransport {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            executor: self.executor.clone(),
        }
    }
}

impl LocalBrowserTransport {
    pub fn new(service: BrowserService, executor: BackgroundExecutor) -> Self {
        Self { service, executor }
    }

    pub fn service(&self) -> &BrowserService {
        &self.service
    }

    /// Spawns a task on the surface's executor. Used by the frame pump, which
    /// must not run on the UI thread.
    pub fn spawn<F>(&self, future: F) -> Task<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.executor.spawn(future)
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<BrowserServiceEvent> {
        self.service.subscribe()
    }
}

impl BrowserTransport for LocalBrowserTransport {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn availability(&self) -> BrowserTransportFuture<'_, BrowserAvailability> {
        Box::pin(async move { Ok(self.service.availability().await) })
    }

    fn list_sessions(&self) -> BrowserTransportFuture<'_, Vec<BrowserSession>> {
        Box::pin(async move { Ok(self.service.sessions().await) })
    }

    fn session_snapshot(
        &self,
        session_id: &BrowserSessionId,
    ) -> BrowserTransportFuture<'_, BrowserSessionSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.service
                .session_snapshot(&session_id)
                .await
                .map_err(Into::into)
        })
    }

    fn ensure_workspace_session(
        &self,
        workspace_id: &WorkspaceId,
    ) -> BrowserTransportFuture<'_, BrowserSessionId> {
        let workspace_id = workspace_id.clone();
        Box::pin(async move {
            self.service
                .ensure_session(
                    BrowserSessionKey::Workspace(workspace_id.clone()),
                    Some(workspace_id),
                )
                .await
                .map_err(Into::into)
        })
    }

    fn create_tab(
        &self,
        session_id: &BrowserSessionId,
        url: Option<&str>,
    ) -> BrowserTransportFuture<'_, BrowserTab> {
        let session_id = session_id.clone();
        let url = url.map(str::to_string);
        Box::pin(async move {
            let tab_id = self
                .service
                .create_tab(&session_id, url.as_deref(), BrowserTabOwner::User)
                .await?;
            let snapshot = self.service.session_snapshot(&session_id).await?;
            snapshot
                .session
                .tabs
                .into_iter()
                .find(|tab| tab.tab_id == tab_id)
                .ok_or_else(|| {
                    BrowserTransportError::new(
                        "browser_tab_missing",
                        "the browser tab vanished immediately after it was created",
                    )
                })
        })
    }

    fn close_tab(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        Box::pin(async move { self.service.close_tab(&tab_id).await.map_err(Into::into) })
    }

    fn select_tab(
        &self,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, ()> {
        let session_id = session_id.clone();
        let tab_id = tab_id.clone();
        Box::pin(async move {
            self.service
                .select_tab(&session_id, &tab_id)
                .await
                .map_err(Into::into)
        })
    }

    fn set_viewport(
        &self,
        tab_id: &BrowserTabId,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        Box::pin(async move {
            self.service
                .set_viewport(&tab_id, width, height, device_scale_factor)
                .await
                .map_err(Into::into)
        })
    }

    fn dispatch_input(
        &self,
        tab_id: &BrowserTabId,
        input: BrowserInput,
    ) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        Box::pin(async move {
            self.service
                .dispatch_input(&tab_id, input)
                .await
                .map_err(Into::into)
        })
    }

    fn navigate(&self, tab_id: &BrowserTabId, url: &str) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let url = url.to_string();
        Box::pin(async move {
            self.service
                .navigate(&tab_id, &url)
                .await
                .map_err(Into::into)
        })
    }

    fn reload(&self, tab_id: &BrowserTabId, ignore_cache: bool) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        Box::pin(async move {
            self.service
                .reload(&tab_id, ignore_cache)
                .await
                .map_err(Into::into)
        })
    }

    fn subscribe_frames(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, BrowserFrameStream> {
        let tab_id = tab_id.clone();
        Box::pin(async move {
            let subscription = self.service.subscribe_frames(&tab_id).await?;
            Ok(BrowserFrameStream::Local(LocalFrameStream { subscription }))
        })
    }

    fn stop_screencast(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        Box::pin(async move {
            self.service.stop_screencast(&tab_id).await;
            Ok(())
        })
    }
}

/// Browser transport backed by a paired runtime.
///
/// Every operation currently reports `remote_browser_unavailable`. That is a
/// deliberate, explicit degradation: the frame and input hop over Remote v2 is a
/// later phase, and a panel that says so is more useful than one that waits
/// forever for frames.
#[derive(Clone)]
pub struct RemoteBrowserTransport {
    backend: Arc<dyn BrowserBackend>,
}

impl RemoteBrowserTransport {
    pub fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }

    fn unavailable<T>() -> BrowserTransportResult<T> {
        Err(BrowserTransportError::new(
            "remote_browser_unavailable",
            "The runtime this client is paired with serves browser frames over Remote v2, which \
             is not implemented yet. Agent browser tools still work; only the live panel is \
             unavailable here.",
        ))
    }
}

impl BrowserTransport for RemoteBrowserTransport {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn availability(&self) -> BrowserTransportFuture<'_, BrowserAvailability> {
        Box::pin(async move {
            Ok(BrowserAvailability::unavailable(
                BrowserUnavailableReason::RemoteRuntimeUnsupported,
                Some(
                    "The runtime this client is paired with is remote, and the browser frame \
                     transport over Remote v2 is not implemented yet."
                        .to_string(),
                ),
            ))
        })
    }

    fn list_sessions(&self) -> BrowserTransportFuture<'_, Vec<BrowserSession>> {
        Box::pin(async move {
            self.backend
                .list_browser_sessions()
                .await
                .map_err(Into::into)
        })
    }

    fn session_snapshot(
        &self,
        session_id: &BrowserSessionId,
    ) -> BrowserTransportFuture<'_, BrowserSessionSnapshot> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.backend
                .browser_session_snapshot(session_id)
                .await
                .map_err(Into::into)
        })
    }

    fn ensure_workspace_session(
        &self,
        _workspace_id: &WorkspaceId,
    ) -> BrowserTransportFuture<'_, BrowserSessionId> {
        Box::pin(async move { Self::unavailable() })
    }

    fn create_tab(
        &self,
        _session_id: &BrowserSessionId,
        _url: Option<&str>,
    ) -> BrowserTransportFuture<'_, BrowserTab> {
        Box::pin(async move { Self::unavailable() })
    }

    fn close_tab(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn select_tab(
        &self,
        _session_id: &BrowserSessionId,
        _tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn set_viewport(
        &self,
        _tab_id: &BrowserTabId,
        _width: u32,
        _height: u32,
        _device_scale_factor: f64,
    ) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn dispatch_input(
        &self,
        _tab_id: &BrowserTabId,
        _input: BrowserInput,
    ) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn navigate(&self, _tab_id: &BrowserTabId, _url: &str) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn reload(
        &self,
        _tab_id: &BrowserTabId,
        _ignore_cache: bool,
    ) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn subscribe_frames(
        &self,
        _tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, BrowserFrameStream> {
        Box::pin(async move { Ok(BrowserFrameStream::Unavailable) })
    }

    fn stop_screencast(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }
}

/// Converts a surface input event into the wire payload the backend carries.
pub fn input_to_payload(input: &BrowserInput) -> Option<BrowserInputPayload> {
    Some(match input {
        BrowserInput::MouseMove { x, y } => BrowserInputPayload::MouseMove { x: *x, y: *y },
        BrowserInput::MouseDown {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => BrowserInputPayload::MouseDown {
            x: *x,
            y: *y,
            button: button.clone(),
            click_count: *click_count,
            modifiers: *modifiers,
        },
        BrowserInput::MouseUp {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => BrowserInputPayload::MouseUp {
            x: *x,
            y: *y,
            button: button.clone(),
            click_count: *click_count,
            modifiers: *modifiers,
        },
        BrowserInput::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => BrowserInputPayload::Wheel {
            x: *x,
            y: *y,
            delta_x: *delta_x,
            delta_y: *delta_y,
        },
        BrowserInput::Key {
            event_type,
            key,
            code,
            text,
            modifiers,
            windows_key_code,
        } => BrowserInputPayload::Key {
            event_type: event_type.clone(),
            key: key.clone(),
            code: code.clone(),
            text: text.clone(),
            modifiers: *modifiers,
            windows_key_code: *windows_key_code,
        },
        BrowserInput::InsertText { text } => BrowserInputPayload::InsertText { text: text.clone() },
        // Resize travels through its own method so the viewport is debounced in
        // one place.
        BrowserInput::Resize { .. } => return None,
    })
}

/// Converts a wire payload back into a surface input event.
pub fn payload_to_input(payload: &BrowserInputPayload) -> BrowserInput {
    match payload {
        BrowserInputPayload::MouseMove { x, y } => BrowserInput::MouseMove { x: *x, y: *y },
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

/// The tier an Agent session gets. Kept here so the panel and the runtime agree.
pub fn tool_tier_for_session(_session: Option<&VibexSessionId>) -> BrowserToolTier {
    BrowserToolTier::Fine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_round_trips_through_the_wire_payload() {
        let inputs = [
            BrowserInput::MouseMove { x: 1.0, y: 2.0 },
            BrowserInput::MouseDown {
                x: 3.0,
                y: 4.0,
                button: "left".to_string(),
                click_count: 1,
                modifiers: 2,
            },
            BrowserInput::MouseUp {
                x: 5.0,
                y: 6.0,
                button: "right".to_string(),
                click_count: 2,
                modifiers: 8,
            },
            BrowserInput::Wheel {
                x: 7.0,
                y: 8.0,
                delta_x: -1.0,
                delta_y: 120.0,
            },
            BrowserInput::Key {
                event_type: "keyDown".to_string(),
                key: "a".to_string(),
                code: "KeyA".to_string(),
                text: Some("a".to_string()),
                modifiers: 0,
                windows_key_code: 65,
            },
            BrowserInput::InsertText {
                text: "hello".to_string(),
            },
        ];
        for input in inputs {
            let payload = input_to_payload(&input).expect("input should convert");
            let restored = payload_to_input(&payload);
            assert_eq!(format!("{restored:?}"), format!("{input:?}"));
        }
    }

    #[test]
    fn resize_does_not_travel_as_an_input_payload() {
        assert!(
            input_to_payload(&BrowserInput::Resize {
                width: 100,
                height: 100,
                device_scale_factor: 1.0,
            })
            .is_none()
        );
    }

    #[test]
    fn unavailable_errors_are_recognized_as_reasons_not_failures() {
        assert!(BrowserTransportError::new("remote_browser_unavailable", "x").is_unavailable());
        assert!(BrowserTransportError::new("browser_unavailable", "x").is_unavailable());
        assert!(!BrowserTransportError::new("browser_tab_not_found", "x").is_unavailable());
    }
}
