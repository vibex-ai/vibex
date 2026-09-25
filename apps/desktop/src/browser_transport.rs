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

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use tokio::runtime::Handle;
use vibex_backend::{BackendError, BrowserBackend, BrowserInputPayload};
use vibex_browser::{
    BrowserFrameSubscription, BrowserInput, BrowserSelectMenu, BrowserService, BrowserServiceEvent,
    BrowserSessionKey,
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
    /// Acks are sent from a Tokio task, so the stream carries the runtime too:
    /// the pump awaits frames on GPUI's executor. See [`runtime_context`].
    runtime: Handle,
}

impl BrowserFrameStream {
    /// Waits for the next frame, or `None` when the stream is over.
    pub async fn next(&mut self) -> Option<BrowserFrame> {
        match self {
            Self::Local(stream) => {
                let LocalFrameStream {
                    subscription,
                    runtime,
                } = stream;
                runtime_context(runtime.clone(), subscription.next()).await
            }
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

    /// Moves the tab one entry back in its history.
    fn go_back(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;

    /// Moves the tab one entry forward in its history.
    fn go_forward(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;

    /// Highlights the element under a viewport point and returns its
    /// `backendNodeId`, or `None` when there is nothing there.
    fn highlight_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<i64>>;

    /// Removes the picker's highlight.
    fn clear_highlight(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;

    /// Describes the element under a viewport point for the inspector card.
    fn describe_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<vibex_browser::BrowserElementInspection>>;

    /// The page's current selection, for the panel's copy shortcut.
    ///
    /// Headless Chrome's clipboard is its own, so the panel reads the selection
    /// out and writes the system clipboard itself.
    fn selection_text(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, String>;

    /// A `<select>` under a viewport point, whose popup the panel has to draw.
    fn select_menu_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<BrowserSelectMenu>>;

    /// Applies an option the human picked from that menu.
    fn choose_select_option(
        &self,
        tab_id: &BrowserTabId,
        index: u32,
        value: &str,
    ) -> BrowserTransportFuture<'_, ()>;

    /// Opens the frame stream and turns the screencast on.
    fn subscribe_frames(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, BrowserFrameStream>;

    /// Stops the screencast. The target and the page state stay alive.
    fn stop_screencast(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()>;
}

/// Polls `future` with `handle`'s Tokio runtime context installed.
///
/// The panel drives the transport from GPUI's executor, and the browser service
/// is a Tokio citizen: it spawns the system Chrome, opens async pipes, arms
/// deadlines and calls `tokio::spawn` for frame acks. Polling any of that from a
/// thread with no Tokio context panics inside tokio — the reported failure was
/// `there is no reactor running, must be called from the context of a Tokio 1.x
/// runtime`, raised by tokio's pidfd reaper on the first click of the browser
/// entry.
///
/// Entering the context for the duration of each poll is the fix, rather than
/// spawning the future onto the runtime: a spawned task cannot borrow the
/// transport, and the panel relies on dropping a task to stop waiting for a
/// frame. The guard is dropped before `poll` returns, so it never crosses an
/// await point and the adapter stays `Send` whenever the future is.
fn runtime_context<F: Future>(handle: Handle, future: F) -> RuntimeContext<F> {
    RuntimeContext {
        handle,
        future: Box::pin(future),
    }
}

/// Future adapter built by [`runtime_context`].
struct RuntimeContext<F> {
    handle: Handle,
    /// Boxed so the adapter is `Unpin` without an unsafe projection.
    future: Pin<Box<F>>,
}

impl<F: Future> Future for RuntimeContext<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        // `Pin<Box<F>>` is always `Unpin`, so `get_mut` needs no `unsafe`.
        let this = self.get_mut();
        let _entered = this.handle.enter();
        this.future.as_mut().poll(cx)
    }
}

/// Browser transport backed by the in-process runtime.
pub struct LocalBrowserTransport {
    service: BrowserService,
    /// The runtime the browser service must be polled inside. See
    /// [`runtime_context`].
    runtime: Handle,
}

impl Clone for LocalBrowserTransport {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

impl LocalBrowserTransport {
    pub fn new(service: BrowserService, runtime: Handle) -> Self {
        Self { service, runtime }
    }

    pub fn service(&self) -> &BrowserService {
        &self.service
    }

    /// Polls a browser-service future inside the runtime.
    ///
    /// Every `BrowserTransport` method already goes through here. Operations the
    /// trait does not model — answering a page dialog is the only one today —
    /// must too, because the GPUI executor has no Tokio reactor.
    pub fn run<'a, T: 'a>(
        &'a self,
        future: impl Future<Output = T> + 'a,
    ) -> impl Future<Output = T> + 'a {
        runtime_context(self.runtime.clone(), future)
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
        let service = self.service.clone();
        Box::pin(self.run(async move { Ok(service.availability().await) }))
    }

    fn list_sessions(&self) -> BrowserTransportFuture<'_, Vec<BrowserSession>> {
        let service = self.service.clone();
        Box::pin(self.run(async move { Ok(service.sessions().await) }))
    }

    fn session_snapshot(
        &self,
        session_id: &BrowserSessionId,
    ) -> BrowserTransportFuture<'_, BrowserSessionSnapshot> {
        let session_id = session_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .session_snapshot(&session_id)
                .await
                .map_err(Into::into)
        }))
    }

    fn ensure_workspace_session(
        &self,
        workspace_id: &WorkspaceId,
    ) -> BrowserTransportFuture<'_, BrowserSessionId> {
        let workspace_id = workspace_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .ensure_session(
                    BrowserSessionKey::Workspace(workspace_id.clone()),
                    Some(workspace_id),
                )
                .await
                .map_err(Into::into)
        }))
    }

    fn create_tab(
        &self,
        session_id: &BrowserSessionId,
        url: Option<&str>,
    ) -> BrowserTransportFuture<'_, BrowserTab> {
        let session_id = session_id.clone();
        let url = url.map(str::to_string);
        let service = self.service.clone();
        Box::pin(self.run(async move {
            let tab_id = service
                .create_tab(&session_id, url.as_deref(), BrowserTabOwner::User)
                .await?;
            let snapshot = service.session_snapshot(&session_id).await?;
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
        }))
    }

    fn close_tab(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move { service.close_tab(&tab_id).await.map_err(Into::into) }))
    }

    fn select_tab(
        &self,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, ()> {
        let session_id = session_id.clone();
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .select_tab(&session_id, &tab_id)
                .await
                .map_err(Into::into)
        }))
    }

    fn set_viewport(
        &self,
        tab_id: &BrowserTabId,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .set_viewport(&tab_id, width, height, device_scale_factor)
                .await
                .map_err(Into::into)
        }))
    }

    fn dispatch_input(
        &self,
        tab_id: &BrowserTabId,
        input: BrowserInput,
    ) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .dispatch_input(&tab_id, input)
                .await
                .map_err(Into::into)
        }))
    }

    fn navigate(&self, tab_id: &BrowserTabId, url: &str) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let url = url.to_string();
        let service = self.service.clone();
        Box::pin(self.run(async move { service.navigate(&tab_id, &url).await.map_err(Into::into) }))
    }

    fn reload(&self, tab_id: &BrowserTabId, ignore_cache: bool) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .reload(&tab_id, ignore_cache)
                .await
                .map_err(Into::into)
        }))
    }

    fn go_back(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move { service.go_back(&tab_id).await.map_err(Into::into) }))
    }

    fn go_forward(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move { service.go_forward(&tab_id).await.map_err(Into::into) }))
    }

    fn highlight_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<i64>> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .highlight_at(&tab_id, x, y)
                .await
                .map_err(Into::into)
        }))
    }

    fn clear_highlight(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(
            self.run(async move { service.clear_highlight(&tab_id).await.map_err(Into::into) }),
        )
    }

    fn describe_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<vibex_browser::BrowserElementInspection>> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(
            self.run(async move { service.describe_at(&tab_id, x, y).await.map_err(Into::into) }),
        )
    }

    fn selection_text(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, String> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move { service.selection_text(&tab_id).await.map_err(Into::into) }))
    }

    fn select_menu_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserTransportFuture<'_, Option<BrowserSelectMenu>> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .select_menu_at(&tab_id, x, y)
                .await
                .map_err(Into::into)
        }))
    }

    fn choose_select_option(
        &self,
        tab_id: &BrowserTabId,
        index: u32,
        value: &str,
    ) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let value = value.to_string();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service
                .choose_select_option(&tab_id, index, &value)
                .await
                .map_err(Into::into)
        }))
    }

    fn subscribe_frames(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserTransportFuture<'_, BrowserFrameStream> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        let runtime = self.runtime.clone();
        Box::pin(self.run(async move {
            let subscription = service.subscribe_frames(&tab_id).await?;
            Ok(BrowserFrameStream::Local(LocalFrameStream {
                subscription,
                runtime,
            }))
        }))
    }

    fn stop_screencast(&self, tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        let tab_id = tab_id.clone();
        let service = self.service.clone();
        Box::pin(self.run(async move {
            service.stop_screencast(&tab_id).await;
            Ok(())
        }))
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

    fn go_back(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn go_forward(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn highlight_at(
        &self,
        _tab_id: &BrowserTabId,
        _x: f64,
        _y: f64,
    ) -> BrowserTransportFuture<'_, Option<i64>> {
        Box::pin(async move { Self::unavailable() })
    }

    fn clear_highlight(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, ()> {
        Box::pin(async move { Self::unavailable() })
    }

    fn describe_at(
        &self,
        _tab_id: &BrowserTabId,
        _x: f64,
        _y: f64,
    ) -> BrowserTransportFuture<'_, Option<vibex_browser::BrowserElementInspection>> {
        Box::pin(async move { Self::unavailable() })
    }

    fn selection_text(&self, _tab_id: &BrowserTabId) -> BrowserTransportFuture<'_, String> {
        Box::pin(async move { Self::unavailable() })
    }

    fn select_menu_at(
        &self,
        _tab_id: &BrowserTabId,
        _x: f64,
        _y: f64,
    ) -> BrowserTransportFuture<'_, Option<BrowserSelectMenu>> {
        Box::pin(async move { Self::unavailable() })
    }

    fn choose_select_option(
        &self,
        _tab_id: &BrowserTabId,
        _index: u32,
        _value: &str,
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
        BrowserInput::MouseMove { x, y, buttons } => BrowserInputPayload::MouseMove {
            x: *x,
            y: *y,
            buttons: *buttons,
        },
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

/// The tier an Agent session gets. Kept here so the panel and the runtime agree.
pub fn tool_tier_for_session(_session: Option<&VibexSessionId>) -> BrowserToolTier {
    BrowserToolTier::Fine
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Drives a future on the calling thread, which — like GPUI's executor, and
    /// unlike a Tokio worker — has no Tokio context installed.
    fn drive_outside_tokio<F: Future>(future: F) -> F::Output {
        struct ThreadWaker(std::thread::Thread);
        impl std::task::Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = std::task::Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut context = TaskContext::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                // Parks can return spuriously; the loop simply polls again.
                Poll::Pending => std::thread::park(),
            }
        }
    }

    /// Runs `browser_evaluate` through the transport and returns the outcome's
    /// text, for the live assertions that need to read the page.
    async fn evaluate(
        transport: &LocalBrowserTransport,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
        script: &str,
    ) -> String {
        let context = vibex_browser::BrowserToolContext {
            session_id: session_id.clone(),
            agent_session_id: None,
            workspace_id: None,
            authorized_roots: Vec::new(),
            tier: BrowserToolTier::Fine,
        };
        let outcome = transport
            .run(transport.service().call_tool(
                &context,
                "browser_evaluate",
                &serde_json::json!({ "tab_id": tab_id.as_str(), "script": script }),
            ))
            .await;
        assert!(
            !outcome.is_error,
            "browser_evaluate failed: {}",
            outcome.text
        );
        outcome.text
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a Tokio runtime for the test")
    }

    /// Stops the browser and removes the throwaway profile when the test ends —
    /// including the panicking path, so a failure cannot leave a Chrome process
    /// tree behind.
    struct BrowserCleanup {
        runtime: tokio::runtime::Runtime,
        service: BrowserService,
        home: std::path::PathBuf,
    }

    impl Drop for BrowserCleanup {
        fn drop(&mut self) {
            self.runtime.block_on(self.service.shutdown());
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    /// The context adapter is what keeps the panel's first click from panicking.
    ///
    /// Spawning a process is the operation that used to blow up: tokio opens a
    /// pidfd and registers it with the reactor, so outside a runtime it panics
    /// with "there is no reactor running". The timer covers the deadlines every
    /// CDP command carries.
    #[test]
    fn runtime_bound_work_runs_when_polled_from_a_foreign_executor() {
        let runtime = test_runtime();
        let outcome = drive_outside_tokio(runtime_context(runtime.handle().clone(), async {
            let mut command = if cfg!(windows) {
                let mut command = tokio::process::Command::new("cmd");
                command.args(["/C", "exit", "0"]);
                command
            } else {
                tokio::process::Command::new("true")
            };
            let status = command.status().await.expect("the probe process runs");
            let deadline_held = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_millis(1)).await;
            })
            .await
            .is_ok();
            status.success() && deadline_held
        }));
        assert!(
            outcome,
            "a runtime-bound future must complete when the adapter installs the context"
        );
        runtime.shutdown_background();
    }

    /// The crash the panel hit: `ensure_session` launches the system Chrome, and
    /// `create_tab` drives CDP. Polled from a thread with no Tokio context — the
    /// situation GPUI's executor creates — the whole chain must work, which is
    /// what the reported `there is no reactor running` panic was about.
    ///
    /// Machines without a system Chrome cannot prove anything here, and the
    /// feature is explicitly unavailable there, so the test steps aside.
    #[test]
    fn the_panel_can_open_a_browser_session_from_a_foreign_executor() {
        if vibex_browser::discovery::installation_by_id(None).is_none() {
            eprintln!("no system browser installed; skipping the browser transport test");
            return;
        }
        let runtime = test_runtime();
        let home = std::env::temp_dir().join(format!(
            "vibex-browser-transport-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let service = BrowserService::new(vibex_browser::BrowserServiceConfig::new(&home));
        let transport = LocalBrowserTransport::new(service.clone(), runtime.handle().clone());
        let workspace = WorkspaceId::new();
        let _cleanup = BrowserCleanup {
            runtime,
            service,
            home,
        };

        let outcome = drive_outside_tokio(async {
            let session = transport.ensure_workspace_session(&workspace).await?;
            let tab = transport.create_tab(&session, Some("about:blank")).await?;
            transport.set_viewport(&tab.tab_id, 800, 600, 1.0).await?;
            let mut frames = transport.subscribe_frames(&tab.tab_id).await?;
            // The pump awaits frames outside the transport, so the stream has to
            // install the context itself: an arriving frame acks from a Tokio
            // task. A blank page paints, so the first frame is what proves it.
            let frame = frames.next().await;
            assert!(
                frame.is_some(),
                "the first screencast frame should arrive for a painted page"
            );
            // The panel's clipboard path needs the page's own selection, which
            // headless Chrome never puts on the system clipboard.
            transport
                .run(transport.service().call_tool(
                    &vibex_browser::BrowserToolContext {
                        session_id: session.clone(),
                        agent_session_id: None,
                        workspace_id: None,
                        authorized_roots: Vec::new(),
                        tier: BrowserToolTier::Fine,
                    },
                    "browser_evaluate",
                    &serde_json::json!({
                        "tab_id": tab.tab_id.as_str(),
                        "script": "document.body.innerText = 'select me'; \
                                    const range = document.createRange(); \
                                    range.selectNodeContents(document.body); \
                                    window.getSelection().removeAllRanges(); \
                                    window.getSelection().addRange(range);",
                    }),
                ))
                .await;
            let selection = transport.selection_text(&tab.tab_id).await?;
            assert_eq!(selection.trim(), "select me", "the selection is readable");

            // A `<select>` popup is browser UI, so the panel draws its own; the
            // transport has to find the element and apply the choice.
            transport
                .run(transport.service().call_tool(
                    &vibex_browser::BrowserToolContext {
                        session_id: session.clone(),
                        agent_session_id: None,
                        workspace_id: None,
                        authorized_roots: Vec::new(),
                        tier: BrowserToolTier::Fine,
                    },
                    "browser_evaluate",
                    &serde_json::json!({
                        "tab_id": tab.tab_id.as_str(),
                        "script": "document.body.innerHTML = \
                            '<select style=\"position:absolute;left:20px;top:20px\">' + \
                            '<option value=\"a\">Alpha</option>' + \
                            '<option value=\"b\">Beta</option></select>';",
                    }),
                ))
                .await;
            let menu = transport.select_menu_at(&tab.tab_id, 30.0, 30.0).await?;
            let menu = menu.expect("the select under the point is found");
            let labels: Vec<String> = menu.options.iter().map(|o| o.label.clone()).collect();
            assert_eq!(labels, vec!["Alpha".to_string(), "Beta".to_string()]);
            transport
                .choose_select_option(&tab.tab_id, menu.index, "b")
                .await?;
            let after = transport
                .select_menu_at(&tab.tab_id, 30.0, 30.0)
                .await?
                .expect("the select is still there");
            assert_eq!(after.value, "b", "the choice reached the page");
            assert!(
                transport
                    .select_menu_at(&tab.tab_id, 700.0, 500.0)
                    .await?
                    .is_none(),
                "a point with no select is not a menu"
            );

            // A wheel in the DOM convention moves the page the way the sign
            // says: positive `deltaY` scrolls down. The panel flips GPUI's sign
            // before it reaches here, and this is what pins the flip.
            evaluate(
                &transport,
                &session,
                &tab.tab_id,
                "window.__events = []; \
                 addEventListener('wheel', (e) => window.__events.push('wheel:' + e.deltaY)); \
                 addEventListener('mousemove', (e) => window.__events.push('move:' + e.buttons)); \
                 document.body.innerHTML = '<div style=\"height:5000px\">tall</div>'; \
                 window.scrollTo(0, 0); 'ready'",
            )
            .await;
            transport
                .dispatch_input(
                    &tab.tab_id,
                    BrowserInput::Wheel {
                        x: 100.0,
                        y: 100.0,
                        delta_x: 0.0,
                        delta_y: 300.0,
                    },
                )
                .await?;
            // A move with the button held has to say so, or Chrome never treats
            // it as a drag: a scrollbar or a text selection would stay dead.
            for input in [
                BrowserInput::MouseDown {
                    x: 100.0,
                    y: 100.0,
                    button: "left".to_string(),
                    click_count: 1,
                    modifiers: 0,
                },
                BrowserInput::MouseMove {
                    x: 140.0,
                    y: 160.0,
                    buttons: 1,
                },
                BrowserInput::MouseUp {
                    x: 140.0,
                    y: 160.0,
                    button: "left".to_string(),
                    click_count: 1,
                    modifiers: 0,
                },
            ] {
                transport.dispatch_input(&tab.tab_id, input).await?;
            }
            std::thread::sleep(Duration::from_millis(300));
            // A human's wheel pauses the Agent on that tab, exactly as a click
            // does; reading the page back needs the hand-back first.
            transport
                .run(transport.service().resume_agent_operations(&session))
                .await;
            let seen = evaluate(
                &transport,
                &session,
                &tab.tab_id,
                "({ y: window.scrollY, events: window.__events })",
            )
            .await;
            assert!(
                seen.contains("wheel:300"),
                "a positive deltaY reaches the page: {seen}"
            );
            assert!(
                seen.contains("move:1"),
                "a held button travels with the move: {seen}"
            );
            assert!(
                seen.contains("\"y\":300"),
                "a positive deltaY scrolls the page down: {seen}"
            );
            // An HTTP auth challenge has no headless UI. Left unanswered it
            // suspends the request forever, so the runtime declines it and the
            // load finishes with a 401 the page can see.
            let auth = std::net::TcpListener::bind("127.0.0.1:0").expect("an auth server");
            let auth_port = auth.local_addr().expect("the auth address").port();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = auth.accept() {
                    let _ = std::io::Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"vibex\"\r\n                          Content-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            });
            transport
                .navigate(&tab.tab_id, &format!("http://127.0.0.1:{auth_port}/"))
                .await?;
            // The body is polled without a Tokio context on purpose, so the
            // wait blocks the test thread between polls rather than using a
            // Tokio timer: the browser keeps making progress on its own threads.
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let mut status = None;
            while std::time::Instant::now() < deadline {
                let snapshot = transport.session_snapshot(&session).await?;
                status = snapshot
                    .session
                    .tabs
                    .iter()
                    .find(|candidate| candidate.tab_id == tab.tab_id)
                    .map(|candidate| candidate.status);
                if status == Some(vibex_core::BrowserTabStatus::Ready) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(
                status,
                Some(vibex_core::BrowserTabStatus::Ready),
                "an auth challenge must be declined, not left hanging"
            );

            // A permission prompt has no headless UI either. The browser
            // declines it rather than waiting for an answer nobody can give.
            let geolocation = transport
                .run(transport.service().call_tool(
                    &vibex_browser::BrowserToolContext {
                        session_id: session.clone(),
                        agent_session_id: None,
                        workspace_id: None,
                        authorized_roots: Vec::new(),
                        tier: BrowserToolTier::Fine,
                    },
                    "browser_evaluate",
                    &serde_json::json!({
                        "tab_id": tab.tab_id.as_str(),
                        "script": "new Promise((resolve) => { \
                            const timer = setTimeout(() => resolve('pending'), 4000); \
                            navigator.geolocation.getCurrentPosition( \
                              () => { clearTimeout(timer); resolve('granted'); }, \
                              (error) => { clearTimeout(timer); resolve('denied:' + error.code); }, \
                              { timeout: 3000 }); \
                        })",
                    }),
                ))
                .await;
            let text = format!("{geolocation:?}");
            assert!(
                text.contains("denied"),
                "a permission prompt must be declined, not left pending: {text}"
            );

            transport.stop_screencast(&tab.tab_id).await?;
            transport.close_tab(&tab.tab_id).await?;
            Ok::<(), BrowserTransportError>(())
        });

        outcome.expect("the panel drives the browser service from a foreign executor");
    }

    #[test]
    fn input_round_trips_through_the_wire_payload() {
        let inputs = [
            BrowserInput::MouseMove {
                x: 1.0,
                y: 2.0,
                buttons: 1,
            },
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
