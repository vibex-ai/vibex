//! The GPUI surface that paints an embedded-browser tab.
//!
//! The surface is a renderer and an input router, nothing more. The runtime owns
//! the browser process, the CDP connection and the page; what arrives here is an
//! encoded JPEG per frame and what leaves is viewport-relative input.
//!
//! Three details are load-bearing:
//!
//! * **Every replacement frame drops the previous texture.** `RenderImage::new`
//!   mints a fresh `ImageId` and the sprite atlas has no eviction, so a
//!   per-frame image without a matching `Window::drop_image` grows GPU memory
//!   without bound. This is the first per-frame image path in the product, so
//!   there is no existing precedent to copy — the drop is explicit here.
//! * **Frames use a latest-value channel.** There is no queue to fall behind and
//!   no decoded-frame backlog; a frame that arrives while the UI is busy simply
//!   replaces the one waiting.
//! * **Input coordinates are converted through the frame metadata.** The runtime
//!   forwards viewport CSS pixels, so the panel only scales by the ratio between
//!   the drawn size and the frame's reported device size — and must *not* add
//!   the scroll offset, which would make every click drift.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement,
    Keystroke, MouseButton, ParentElement as _, Pixels, Point, Render, RenderImage, SharedString,
    Styled as _, Subscription, Task, UTF16Selection, Window, canvas, div, img, prelude::*, px,
    size,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::Button,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use image::Frame;
use vibex_browser::BrowserInput;
use vibex_core::{
    BrowserAvailability, BrowserDialogRequest, BrowserExecutionSource, BrowserFrame,
    BrowserFrameMetadata, BrowserSessionId, BrowserTab, BrowserTabId, BrowserTabStatus,
    BrowserUnavailableReason,
};

use crate::browser_transport::{
    BrowserFrameStream, BrowserTransport, BrowserTransportError, LocalBrowserTransport,
};
use crate::locale;

/// How long the panel waits after a resize before telling the browser.
///
/// Resizing the window fires continuously; `Emulation.setDeviceMetricsOverride`
/// re-lays-out the page, so it must not run per frame.
const VIEWPORT_DEBOUNCE: Duration = Duration::from_millis(160);
/// Longest frame edge the panel will ask the browser to encode.
const MAX_ENCODE_WIDTH: u32 = 2560;
const MAX_ENCODE_HEIGHT: u32 = 1600;

/// Which page the surface is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfacePhase {
    /// Waiting for the transport to hand over a tab.
    Idle,
    /// The tab exists and the screencast is starting.
    Connecting,
    /// Frames are arriving.
    Live,
    /// The runtime cannot serve this panel, with a reason to show.
    Unavailable,
    /// Something failed; the message is shown verbatim.
    Failed,
}

/// Events the surface raises to its owner.
#[derive(Debug, Clone)]
pub enum BrowserSurfaceEvent {
    /// The tab's title or URL changed, so the preview tab label should refresh.
    TabChanged { tab_id: BrowserTabId },
    /// A JavaScript dialog is blocking the page and needs a human.
    DialogOpened(BrowserDialogRequest),
}

/// A GPUI surface for one browser tab.
pub struct BrowserSurface {
    transport: Option<Arc<dyn BrowserTransport>>,
    tab_id: Option<BrowserTabId>,
    session_id: Option<BrowserSessionId>,
    tab: Option<BrowserTab>,
    availability: Option<BrowserAvailability>,
    phase: SurfacePhase,
    message: Option<String>,
    /// The frame currently painted. Replaced wholesale on each new frame.
    frame_image: Option<Arc<RenderImage>>,
    /// Textures that must be released on the next paint.
    ///
    /// `Window::drop_image` needs a `&mut Window`, which only `render` has, so
    /// the previous frame is parked here and released at the top of the next
    /// paint instead of leaking.
    pending_drop: Vec<Arc<RenderImage>>,
    frame_metadata: BrowserFrameMetadata,
    frame_sequence: u64,
    dropped_frames: u64,
    /// Panel-space bounds of the frame, used to convert pointer positions.
    frame_bounds: Option<Bounds<Pixels>>,
    /// Encoded pixel size of the current frame.
    frame_pixel_size: (f32, f32),
    last_applied_viewport: Option<(u32, u32, u32)>,
    viewport_task: Option<Task<()>>,
    frame_task: Option<Task<()>>,
    /// The stop-screencast call a deactivation sent. It is cancelled when the
    /// tab is shown again, because a stop that arrives after the restart would
    /// leave the panel with no frames at all.
    stop_task: Option<Task<()>>,
    dialog: Option<BrowserDialogRequest>,
    prompt_input: String,
    file_chooser_pending: bool,
    /// Who the runtime says is driving this tab.
    execution_source: Option<BrowserExecutionSource>,
    /// True while a human's own input has paused the Agent on this tab, so the
    /// panel can offer the hand-back and says why Agent tools fail meanwhile.
    agent_paused: bool,
    focus: FocusHandle,
    marked_text: Option<String>,
    active: bool,
    _subscriptions: Vec<Subscription>,
    /// The address bar. Editable, because a human taking over needs to be able
    /// to go somewhere the Agent did not.
    address_input: Entity<InputState>,
    #[cfg(test)]
    pub(crate) input_log: Vec<String>,
}

impl EventEmitter<BrowserSurfaceEvent> for BrowserSurface {}

impl BrowserSurface {
    /// Creates a surface for one preview tab. The tab is attached later.
    pub fn new(_browser_tab_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address_input = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder("Enter a URL")
        });
        // Enter in the address bar navigates. The subscription lives on the
        // surface so a keyboard-only reader never has to reach for the mouse.
        let address_subscription = cx.subscribe_in(
            &address_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_address(cx);
                    window.invalidate_character_coordinates();
                }
            },
        );
        Self {
            transport: None,
            tab_id: None,
            session_id: None,
            tab: None,
            availability: None,
            phase: SurfacePhase::Idle,
            message: None,
            frame_image: None,
            pending_drop: Vec::new(),
            frame_metadata: BrowserFrameMetadata::default(),
            frame_sequence: 0,
            dropped_frames: 0,
            frame_bounds: None,
            frame_pixel_size: (0.0, 0.0),
            last_applied_viewport: None,
            viewport_task: None,
            frame_task: None,
            stop_task: None,
            dialog: None,
            prompt_input: String::new(),
            file_chooser_pending: false,
            execution_source: None,
            agent_paused: false,
            focus: cx.focus_handle(),
            marked_text: None,
            active: false,
            _subscriptions: vec![address_subscription],
            address_input,
            #[cfg(test)]
            input_log: Vec::new(),
        }
    }

    /// Attaches the transport and the runtime-side tab.
    pub fn attach(
        &mut self,
        transport: Arc<dyn BrowserTransport>,
        session_id: BrowserSessionId,
        tab_id: BrowserTabId,
        cx: &mut Context<Self>,
    ) {
        if self.tab_id.as_ref() == Some(&tab_id) && self.transport.is_some() {
            return;
        }
        self.transport = Some(transport);
        self.session_id = Some(session_id);
        self.tab_id = Some(tab_id);
        self.phase = SurfacePhase::Connecting;
        self.message = None;
        if self.active {
            self.start_frame_pump(cx);
        }
        cx.notify();
    }

    /// The runtime-side tab this surface renders.
    pub fn tab_id(&self) -> Option<&BrowserTabId> {
        self.tab_id.as_ref()
    }

    pub fn phase_is_live(&self) -> bool {
        self.phase == SurfacePhase::Live
    }

    /// The page title the runtime reports, falling back to the URL while a
    /// fresh page has not reported one yet.
    pub fn page_title(&self) -> Option<String> {
        let tab = self.tab.as_ref()?;
        if !tab.title.trim().is_empty() {
            return Some(tab.title.clone());
        }
        (!tab.url.trim().is_empty()).then(|| tab.url.clone())
    }

    /// True while the panel is showing this tab's frames.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// True while the page is still loading.
    pub fn is_loading(&self) -> bool {
        self.tab
            .as_ref()
            .is_some_and(|tab| tab.status == BrowserTabStatus::Loading)
    }

    /// Starts or stops the frame pump.
    ///
    /// Switching away from the tab stops the screencast but keeps the target
    /// alive, so page state survives and coming back is instant.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            // A stop that is still on its way would cancel the screencast the
            // pump is about to start.
            self.stop_task = None;
            self.start_frame_pump(cx);
        } else {
            self.frame_task = None;
            self.viewport_task = None;
            let transport = self.transport.clone();
            let tab_id = self.tab_id.clone();
            if let (Some(transport), Some(tab_id)) = (transport, tab_id) {
                self.stop_task = Some(cx.background_executor().spawn(async move {
                    let _ = transport.stop_screencast(&tab_id).await;
                }));
            }
        }
        cx.notify();
    }

    /// Points the surface at a different runtime tab.
    pub fn set_tab(&mut self, tab_id: Option<BrowserTabId>, cx: &mut Context<Self>) {
        if self.tab_id == tab_id {
            return;
        }
        self.tab_id = tab_id;
        self.frame_image = None;
        self.frame_sequence = 0;
        self.dropped_frames = 0;
        self.dialog = None;
        self.file_chooser_pending = false;
        self.phase = if self.tab_id.is_some() {
            SurfacePhase::Connecting
        } else {
            SurfacePhase::Idle
        };
        if self.active {
            self.start_frame_pump(cx);
        }
        cx.notify();
    }

    /// Refreshes the address bar and the tab status from the runtime.
    pub fn refresh_tab(&mut self, cx: &mut Context<Self>) {
        let (Some(transport), Some(session_id)) = (self.transport.clone(), self.session_id.clone())
        else {
            return;
        };
        let tab_id = self.tab_id.clone();
        cx.spawn(async move |this, cx| {
            let Ok(snapshot) = transport.session_snapshot(&session_id).await else {
                return;
            };
            let _ = this.update(cx, |surface, cx| {
                let previous = surface.tab.clone();
                if let Some(tab_id) = &tab_id {
                    surface.tab = snapshot
                        .session
                        .tabs
                        .iter()
                        .find(|tab| &tab.tab_id == tab_id)
                        .cloned();
                }
                surface.availability = Some(snapshot.availability);
                // Who is driving the tab decides whether the panel offers the
                // Agent its control back; the runtime is the authority on that,
                // so it is read here rather than tracked locally.
                surface.execution_source = Some(snapshot.session.execution_source);
                surface.agent_paused = snapshot.session.execution_source
                    == BrowserExecutionSource::User
                    && snapshot.session.user_engaged;
                // The preview tab shows the page's title and whether it is
                // still loading, so a change has to reach the owner.
                if surface.tab != previous
                    && let Some(tab_id) = surface.tab_id.clone()
                {
                    cx.emit(BrowserSurfaceEvent::TabChanged { tab_id });
                }
                if let Some(tab) = &surface.tab
                    && tab.status == BrowserTabStatus::Crashed
                {
                    surface.phase = SurfacePhase::Failed;
                    surface.message = Some(
                        locale::text(
                            "The page crashed. Reload to try again.",
                            "页面已崩溃，请重新加载。",
                            "頁面已崩潰，請重新載入。",
                        )
                        .to_string(),
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_frame_pump(&mut self, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.frame_task = None;
        self.phase = SurfacePhase::Connecting;
        self.frame_task = Some(cx.spawn(async move |this, cx| {
            let mut stream = match transport.subscribe_frames(&tab_id).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = this.update(cx, |surface, cx| {
                        surface.apply_transport_error(&error);
                        cx.notify();
                    });
                    return;
                }
            };
            if matches!(stream, BrowserFrameStream::Unavailable) {
                let _ = this.update(cx, |surface, cx| {
                    surface.phase = SurfacePhase::Unavailable;
                    surface.message = Some(
                        locale::text(
                            "This client cannot show the live page. Agent browser tools still work.",
                            "此客户端无法显示实时页面，Agent 浏览器工具仍可使用。",
                            "此客戶端無法顯示即時頁面，Agent 瀏覽器工具仍可使用。",
                        )
                        .to_string(),
                    );
                    cx.notify();
                });
                return;
            }
            loop {
                let Some(frame) = stream.next().await else {
                    let _ = this.update(cx, |surface, cx| {
                        if surface.phase == SurfacePhase::Live {
                            surface.phase = SurfacePhase::Failed;
                            surface.message = Some(
                                locale::text(
                                    "The browser stopped sending frames.",
                                    "浏览器已停止发送画面。",
                                    "瀏覽器已停止傳送畫面。",
                                )
                                .to_string(),
                            );
                        }
                        cx.notify();
                    });
                    return;
                };
                // JPEG decoding is a few milliseconds of CPU work; doing it on
                // the UI thread would show up as dropped interactions at 30fps.
                let decoded = cx
                    .background_executor()
                    .spawn({
                        let bytes = frame.bytes.clone();
                        async move { decode_frame(&bytes) }
                    })
                    .await;
                let dropped = stream.dropped_frames();
                let Ok(decoded) = decoded else {
                    continue;
                };
                let alive = this.update(cx, |surface, cx| {
                    surface.accept_frame(frame, decoded, dropped);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    fn accept_frame(&mut self, frame: BrowserFrame, image: DecodedFrame, dropped: u64) {
        // Park the outgoing texture: `Window::drop_image` needs a window, which
        // only `render` has.
        if let Some(previous) = self.frame_image.take() {
            self.pending_drop.push(previous);
        }
        self.frame_image = Some(Arc::new(RenderImage::new(vec![Frame::new(image.image)])));
        self.frame_pixel_size = (image.width as f32, image.height as f32);
        self.frame_metadata = frame.metadata;
        self.frame_sequence = frame.sequence;
        self.dropped_frames = dropped;
        self.phase = SurfacePhase::Live;
        self.message = None;
    }

    fn apply_transport_error(&mut self, error: &BrowserTransportError) {
        if error.is_unavailable() {
            self.phase = SurfacePhase::Unavailable;
        } else {
            self.phase = SurfacePhase::Failed;
        }
        self.message = Some(error.message.clone());
    }

    /// Schedules a viewport update, coalescing a burst of resizes into one.
    fn schedule_viewport(
        &mut self,
        width: f32,
        height: f32,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) {
        let logical_width = width.max(1.0).round() as u32;
        let logical_height = height.max(1.0).round() as u32;
        let physical_width = ((logical_width as f32) * scale_factor).round() as u32;
        let physical_height = ((logical_height as f32) * scale_factor).round() as u32;
        let key = (
            physical_width.min(MAX_ENCODE_WIDTH),
            physical_height.min(MAX_ENCODE_HEIGHT),
            (scale_factor * 1000.0) as u32,
        );
        if self.last_applied_viewport == Some(key) {
            return;
        }
        self.last_applied_viewport = Some(key);
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.viewport_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VIEWPORT_DEBOUNCE).await;
            if let Err(error) = transport
                .set_viewport(&tab_id, key.0, key.1, scale_factor as f64)
                .await
            {
                let _ = this.update(cx, |surface, _| surface.apply_transport_error(&error));
            }
        }));
    }

    /// Converts a panel-space point into viewport CSS pixels.
    fn to_viewport_point(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let bounds = self.frame_bounds?;
        let (device_width, device_height) = if self.frame_pixel_size.0 > 0.0 {
            self.frame_pixel_size
        } else if self.frame_metadata.device_width > 0.0 {
            (
                self.frame_metadata.device_width as f32,
                self.frame_metadata.device_height as f32,
            )
        } else {
            return None;
        };
        let display_width = f32::from(bounds.size.width).max(1.0);
        let display_height = f32::from(bounds.size.height).max(1.0);
        let local_x = f32::from(position.x - bounds.origin.x);
        let local_y = f32::from(position.y - bounds.origin.y);
        if local_x < 0.0 || local_y < 0.0 || local_x > display_width || local_y > display_height {
            return None;
        }
        let scale_x = device_width as f64 / display_width as f64;
        let scale_y = device_height as f64 / display_height as f64;
        let page_scale = if self.frame_metadata.page_scale_factor > 0.0 {
            self.frame_metadata.page_scale_factor
        } else {
            1.0
        };
        Some((
            local_x as f64 * scale_x / page_scale,
            local_y as f64 * scale_y / page_scale,
        ))
    }

    fn dispatch(&mut self, input: vibex_browser::BrowserInput, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        #[cfg(test)]
        self.input_log.push(format!("{input:?}"));
        cx.background_executor()
            .spawn(async move {
                let _ = transport.dispatch_input(&tab_id, input).await;
            })
            .detach();
    }

    /// Forwards an editing or navigation key to the page.
    ///
    /// Text never comes through here: printable characters are committed by the
    /// input handler, which is what keeps IME composition intact. What this adds
    /// is everything a page reads from `keydown` — Enter submitting a form,
    /// Backspace editing a field, Tab moving focus, the arrows scrolling, and
    /// shortcuts such as Ctrl+A.
    fn on_page_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // AltGr and friends produce characters; they belong to the text path.
        // A composition owns the keyboard while it is open: Backspace and the
        // arrows are editing the preedit, not the page.
        if !self.focus.is_focused(window)
            || event.prefer_character_input
            || self.marked_text.is_some()
        {
            return;
        }
        // Copy and paste belong to the panel: headless Chrome has its own
        // clipboard, so forwarding the shortcut would copy into a buffer the
        // human can never reach.
        match clipboard_command(&event.keystroke) {
            Some(ClipboardCommand::Copy) => {
                self.copy_selection(cx);
                cx.stop_propagation();
                return;
            }
            Some(ClipboardCommand::Paste) => {
                self.paste_clipboard(cx);
                cx.stop_propagation();
                return;
            }
            None => {}
        }
        let Some(input) = key_input(&event.keystroke, "rawKeyDown") else {
            return;
        };
        self.dispatch(input, cx);
        cx.stop_propagation();
    }

    /// Copies the page's selection into the system clipboard.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(text) = transport.selection_text(&tab_id).await else {
                return;
            };
            if text.is_empty() {
                // A copy with nothing selected is not an error; the page's own
                // handler was not going to produce anything either.
                return;
            }
            let _ = this.update(cx, |_surface, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            });
        })
        .detach();
    }

    /// Sends the system clipboard's text to the page as typed input.
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.dispatch(BrowserInput::InsertText { text }, cx);
    }

    fn on_page_key_up(
        &mut self,
        event: &gpui::KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.is_focused(window) {
            return;
        }
        let Some(input) = key_input(&event.keystroke, "keyUp") else {
            return;
        };
        self.dispatch(input, cx);
        cx.stop_propagation();
    }

    fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        // Composition is drawn by the panel and only the committed string is
        // inserted, which is what makes CJK input work over a screencast.
        self.dispatch(
            vibex_browser::BrowserInput::InsertText {
                text: text.to_string(),
            },
            cx,
        );
    }

    /// Navigates to whatever the address bar holds.
    fn submit_address(&mut self, cx: &mut Context<Self>) {
        let trimmed = self.address_input.read(cx).value().trim().to_string();
        if trimmed.is_empty() {
            cx.notify();
            return;
        }
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        let url = normalize_address(&trimmed);
        self.phase = SurfacePhase::Connecting;
        cx.spawn(async move |this, cx| {
            let result = transport.navigate(&tab_id, &url).await;
            let _ = this.update(cx, |surface, cx| {
                if let Err(error) = result {
                    surface.apply_transport_error(&error);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps the address bar in step with the page while the reader is not
    /// editing it.
    ///
    /// Runs during paint because it needs a window: focusing the field is what
    /// decides whether the page URL may overwrite what is typed there.
    fn sync_address_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab.as_ref() else {
            return;
        };
        let url = tab.url.clone();
        let input = self.address_input.clone();
        if input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        if input.read(cx).value() != url {
            input.update(cx, |state, cx| {
                state.set_value(url, window, cx);
            });
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.start_frame_pump(cx);
        cx.background_executor()
            .spawn(async move {
                let _ = transport.reload(&tab_id, true).await;
            })
            .detach();
    }

    fn resolve_dialog(&mut self, accept: bool, cx: &mut Context<Self>) {
        let (Some(tab_id), Some(dialog)) = (self.tab_id.clone(), self.dialog.clone()) else {
            return;
        };
        let prompt_text = (dialog.dialog_type == "prompt").then(|| self.prompt_input.clone());
        self.dialog = None;
        self.prompt_input.clear();
        // A human answering a dialog they can see is a local operation: there is
        // no remote browser transport yet, so the local service is the only
        // authority that can answer. The call still goes through the transport's
        // runtime seam — the service drives CDP with Tokio deadlines.
        let Some(local) = self.local_browser() else {
            cx.notify();
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let _ = local
                    .run(
                        local
                            .service()
                            .handle_dialog(&tab_id, accept, prompt_text.as_deref()),
                    )
                    .await;
            })
            .detach();
        cx.notify();
    }

    /// The in-process authority, when this panel is driving one.
    ///
    /// Operations the `BrowserTransport` trait does not model — answering a
    /// dialog, releasing a file chooser, handing control back — only exist on
    /// the local service. A paired runtime has no equivalent yet, and the panel
    /// hides the affordance rather than offering one that cannot work.
    fn local_browser(&self) -> Option<LocalBrowserTransport> {
        self.transport.as_ref().and_then(|transport| {
            transport
                .as_any()
                .downcast_ref::<LocalBrowserTransport>()
                .cloned()
        })
    }

    /// Reflects a dialog the runtime reported, so the panel can answer it.
    pub fn show_dialog(&mut self, dialog: BrowserDialogRequest, cx: &mut Context<Self>) {
        if self.tab_id.as_ref() != Some(&dialog.tab_id) {
            return;
        }
        self.dialog = Some(dialog);
        cx.notify();
    }

    /// Clears a dialog card the runtime reports as already answered.
    ///
    /// The page can also unblock itself — a `beforeunload` dialog goes away
    /// when the navigation it was guarding is abandoned — and a card that
    /// outlives its dialog would answer a question nobody is asking.
    pub fn dismiss_dialog(&mut self, tab_id: &BrowserTabId, cx: &mut Context<Self>) {
        if self.tab_id.as_ref() != Some(tab_id) {
            return;
        }
        self.dialog = None;
        self.prompt_input.clear();
        cx.notify();
    }

    /// Reflects an availability change the runtime reported.
    pub fn set_availability(&mut self, availability: BrowserAvailability, cx: &mut Context<Self>) {
        self.availability = Some(availability);
        cx.notify();
    }

    /// Releases the page's file chooser without choosing anything.
    ///
    /// Cancelling only in the panel would leave the page waiting for a file
    /// selection it can never receive, so the runtime is told as well.
    fn cancel_file_chooser(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tab_id.clone() else {
            return;
        };
        self.file_chooser_pending = false;
        let Some(local) = self.local_browser() else {
            cx.notify();
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let _ = local
                    .run(local.service().resolve_file_chooser(&tab_id, &[]))
                    .await;
            })
            .detach();
        cx.notify();
    }

    /// Gives the Agent its control back after a human took over.
    ///
    /// The page may have moved on while the human was driving, so the Agent is
    /// expected to observe again; the panel says so instead of pretending the
    /// hand-back is invisible.
    fn hand_back_to_agent(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(local) = self.local_browser() else {
            cx.notify();
            return;
        };
        self.agent_paused = false;
        cx.background_executor()
            .spawn(async move {
                local
                    .run(local.service().resume_agent_operations(&session_id))
                    .await;
            })
            .detach();
        cx.notify();
    }

    /// Reflects a file chooser the page opened.
    pub fn show_file_chooser(&mut self, tab_id: &BrowserTabId, cx: &mut Context<Self>) {
        if self.tab_id.as_ref() != Some(tab_id) {
            return;
        }
        self.file_chooser_pending = true;
        cx.notify();
    }

    fn render_frame(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let focus = self.focus.clone();
        let input_entity = cx.entity();
        let prepaint_entity = input_entity.clone();
        let active = self.active;
        let has_frame = self.frame_image.is_some();
        let phase_message = self.phase_message();
        // A stalled or failed pump used to be invisible: the last frame stayed
        // on screen, so a frozen page looked like a live one that ignored the
        // pointer. Say it out loud instead.
        let stalled = has_frame
            && matches!(
                self.phase,
                SurfacePhase::Failed | SurfacePhase::Unavailable | SurfacePhase::Crashed
            );
        let stalled_message = stalled.then(|| phase_message.clone());
        let image = self.frame_image.clone();
        div()
            .id("browser-frame")
            .relative()
            .size_full()
            .overflow_hidden()
            .when_some(image, |this, image| {
                // The frame is already sized to the panel by the runtime's
                // viewport override, so it is stretched rather than letterboxed.
                this.child(img(image).w_full().h_full())
            })
            .when(!has_frame || stalled, |this| {
                this.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .when(stalled, |this| {
                            this.bg(cx.theme().background.opacity(0.85))
                        })
                        .child(
                            Icon::new(IconName::Globe)
                                .size(px(28.0))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .max_w(px(420.0))
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(phase_message),
                        ),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    let Some((x, y)) = this.to_viewport_point(event.position) else {
                        return;
                    };
                    this.dispatch(
                        vibex_browser::BrowserInput::MouseDown {
                            x,
                            y,
                            button: "left".to_string(),
                            click_count: event.click_count as i32,
                            modifiers: mouse_modifiers(event.modifiers),
                        },
                        cx,
                    );
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    let Some((x, y)) = this.to_viewport_point(event.position) else {
                        return;
                    };
                    this.dispatch(
                        vibex_browser::BrowserInput::MouseUp {
                            x,
                            y,
                            button: "left".to_string(),
                            click_count: event.click_count as i32,
                            modifiers: mouse_modifiers(event.modifiers),
                        },
                        cx,
                    );
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                let Some((x, y)) = this.to_viewport_point(event.position) else {
                    return;
                };
                this.dispatch(vibex_browser::BrowserInput::MouseMove { x, y }, cx);
            }))
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                let Some((x, y)) = this.to_viewport_point(event.position) else {
                    return;
                };
                let delta = event.delta.pixel_delta(px(16.0));
                this.dispatch(
                    vibex_browser::BrowserInput::Wheel {
                        x,
                        y,
                        delta_x: f32::from(delta.x) as f64,
                        delta_y: f32::from(delta.y) as f64,
                    },
                    cx,
                );
            }))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        // The frame geometry has to come from this canvas, not
                        // from `ElementExt::on_prepaint`: that helper adds an
                        // absolutely positioned `size_full` child, which GPUI
                        // lays out after the in-flow content, so its origin is
                        // the *bottom* of the frame area. Every pointer position
                        // then subtracted that offset and fell outside the
                        // bounds, and the panel silently dropped all mouse
                        // input. This canvas is `inset_0` of the frame, so its
                        // own bounds are the frame's.
                        prepaint_entity.update(cx, |this, cx| {
                            this.frame_bounds = Some(bounds);
                            this.schedule_viewport(
                                f32::from(bounds.size.width),
                                f32::from(bounds.size.height),
                                1.0,
                                cx,
                            );
                        });
                    },
                    move |bounds, _, window, cx| {
                        if active {
                            window.handle_input(
                                &focus,
                                ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    fn phase_message(&self) -> SharedString {
        match self.phase {
            SurfacePhase::Idle => locale::text(
                "Waiting for the browser to start.",
                "正在等待浏览器启动。",
                "正在等待瀏覽器啟動。",
            )
            .into(),
            SurfacePhase::Connecting => {
                locale::text("Loading the page…", "正在加载页面…", "正在載入頁面…").into()
            }
            SurfacePhase::Live => locale::text(
                "Waiting for the next frame…",
                "正在等待下一帧…",
                "正在等待下一幀…",
            )
            .into(),
            SurfacePhase::Unavailable => self
                .message
                .clone()
                .unwrap_or_else(|| {
                    locale::text(
                        "The embedded browser is unavailable.",
                        "内嵌浏览器不可用。",
                        "內嵌瀏覽器無法使用。",
                    )
                    .to_string()
                })
                .into(),
            SurfacePhase::Failed => self
                .message
                .clone()
                .unwrap_or_else(|| {
                    locale::text(
                        "The embedded browser stopped.",
                        "内嵌浏览器已停止。",
                        "內嵌瀏覽器已停止。",
                    )
                    .to_string()
                })
                .into(),
        }
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // The field follows the page until the reader takes it over, which is
        // how every browser address bar behaves.
        let page_url = self
            .tab
            .as_ref()
            .map(|tab| tab.url.clone())
            .unwrap_or_default();
        let typed = self.address_input.read(cx).value().to_string();
        let display_address = if typed.trim().is_empty() {
            page_url
        } else {
            typed
        };
        let display_address = if display_address.chars().count() > 120 {
            format!("{}…", display_address.chars().take(120).collect::<String>())
        } else {
            display_address
        };
        let status = self
            .tab
            .as_ref()
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let _ = display_address;
        h_flex()
            .id("browser-toolbar")
            .flex_none()
            .w_full()
            .h(px(34.0))
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                Button::new("browser-reload")
                    .icon(Icon::new(IconName::Redo))
                    .ghost()
                    .xsmall()
                    .tooltip(locale::text("Reload", "重新加载", "重新載入"))
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.address_input).small()),
            )
            .when(!status.is_empty(), |this| {
                this.child(
                    div()
                        .max_w(px(200.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(status),
                )
            })
            .when(self.dropped_frames > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("-{}", self.dropped_frames)),
                )
            })
            .into_any_element()
    }

    /// The takeover banner.
    ///
    /// A human's own input pauses the Agent on this tab; the banner is where
    /// that is admitted, because the Agent's next action will fail with
    /// `browser_operation_aborted` and nothing else in the panel explains why.
    /// The hand-back is offered only where a local service can perform it.
    fn render_takeover(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.agent_paused {
            return None;
        }
        let can_hand_back = self.local_browser().is_some();
        Some(
            h_flex()
                .id("browser-takeover")
                .flex_none()
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(locale::text(
                            "You are driving this tab; the Agent's page actions are paused. It can \
                             still observe.",
                            "你正在操作此标签页，Agent 的页面操作已暂停（仍可观察）。",
                            "你正在操作此分頁，Agent 的頁面操作已暫停（仍可觀察）。",
                        )),
                )
                .when(can_hand_back, |this| {
                    this.child(
                        Button::new("browser-hand-back")
                            .label(locale::text(
                                "Hand back to Agent",
                                "交还给 Agent",
                                "交還給 Agent",
                            ))
                            .ghost()
                            .xsmall()
                            .tooltip(locale::text(
                                "The Agent must observe the page again before acting.",
                                "Agent 需要重新观察页面后才能继续操作。",
                                "Agent 需要重新觀察頁面後才能繼續操作。",
                            ))
                            .on_click(cx.listener(|this, _, _, cx| this.hand_back_to_agent(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.clone()?;
        let is_prompt = dialog.dialog_type == "prompt";
        let mut card = v_flex()
            .id("browser-dialog")
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(420.0))
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .shadow_lg()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                        match dialog.dialog_type.as_str() {
                            "confirm" => locale::text(
                                "The page is asking for confirmation",
                                "页面正在请求确认",
                                "頁面正在請求確認",
                            ),
                            "prompt" => locale::text(
                                "The page is asking for input",
                                "页面正在请求输入",
                                "頁面正在請求輸入",
                            ),
                            "beforeunload" => locale::text(
                                "Leave this page?",
                                "要离开此页面吗？",
                                "要離開此頁面嗎？",
                            ),
                            _ => locale::text(
                                "The page is showing an alert",
                                "页面正在显示提示",
                                "頁面正在顯示提示",
                            ),
                        },
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(dialog.message.clone()),
                    ),
            );
        if is_prompt {
            let prompt = if self.prompt_input.is_empty() {
                locale::text("(type to answer)", "（输入以应答）", "（輸入以應答）").to_string()
            } else {
                self.prompt_input.clone()
            };
            card = card.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .child(prompt),
            );
        }
        Some(
            card.child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("browser-dialog-dismiss")
                            .label(locale::text("Dismiss", "取消", "取消"))
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.resolve_dialog(false, cx))),
                    )
                    .child(
                        Button::new("browser-dialog-accept")
                            .label(locale::text("Accept", "确定", "確定"))
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.resolve_dialog(true, cx))),
                    ),
            )
            .into_any_element(),
        )
    }

    fn render_file_chooser(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.file_chooser_pending {
            return None;
        }
        Some(
            v_flex()
                .id("browser-file-chooser")
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .w(px(420.0))
                        .gap_3()
                        .p_4()
                        .rounded_lg()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                            locale::text(
                                "The page wants to open a file",
                                "页面想要打开文件",
                                "頁面想要開啟檔案",
                            ),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(locale::text(
                                    "A headless browser has no native file dialog. An Agent can \
                                     attach files with the browser_upload tool, or you can cancel \
                                     this request.",
                                    "无头浏览器没有原生文件对话框。Agent 可以使用 browser_upload 工具附加文件，或者你也可以取消该请求。",
                                    "無頭瀏覽器沒有原生檔案對話框。Agent 可以使用 browser_upload 工具附加檔案，或者你也可以取消該請求。",
                                )),
                        )
                        .child(
                            h_flex().w_full().justify_end().child(
                                Button::new("browser-file-chooser-dismiss")
                                    .label(locale::text("Cancel request", "取消请求", "取消請求"))
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_file_chooser(cx);
                                    })),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// A decoded frame ready to upload to the GPU.
pub(crate) struct DecodedFrame {
    pub image: image::RgbaImage,
    pub width: u32,
    pub height: u32,
}

/// Decodes an encoded screencast frame into the BGRA layout `RenderImage` wants.
///
/// `RenderImage` stores BGRA while the `image` crate produces RGBA, so the red
/// and blue channels are swapped here rather than at paint time.
fn decode_frame(bytes: &[u8]) -> Result<DecodedFrame, String> {
    let decoded = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let (width, height) = (decoded.width(), decoded.height());
    let mut rgba = decoded.to_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(DecodedFrame {
        image: rgba,
        width,
        height,
    })
}

/// Turns whatever the user typed into an absolute URL.
///
/// A bare host becomes `http://host`, and a bare word becomes a search — the
/// same rule every browser address bar uses, so the panel does not surprise
/// anyone.
pub fn normalize_address(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "about:blank".to_string();
    }
    // An explicit `scheme://` is taken as written. `Url::parse` alone is not
    // enough: it accepts `localhost:5173` as a URL whose scheme is `localhost`,
    // which would stop a bare host from ever being completed.
    if let Ok(parsed) = url::Url::parse(trimmed) {
        // A host-bearing URL is taken as written, and so is a scheme that has
        // no host at all (`about:`, `data:`, `blob:`): those are complete
        // documents rather than something to complete.
        if parsed.host().is_some() || trimmed.contains("://") {
            return trimmed.to_string();
        }
        if matches!(
            parsed.scheme(),
            "about" | "data" | "blob" | "chrome" | "view-source"
        ) {
            return trimmed.to_string();
        }
    }
    let looks_like_host = (trimmed.contains('.') && !trimmed.contains(' '))
        || trimmed.starts_with("localhost")
        || trimmed.starts_with("127.0.0.1")
        || trimmed.starts_with("[::1]");
    if looks_like_host && url::Url::parse(&format!("http://{trimmed}")).is_ok() {
        return format!("http://{trimmed}");
    }
    // Anything else is a search. `query_pairs_mut` does the percent-encoding,
    // which is exactly the form-encoding a search box needs.
    let mut search = url::Url::parse("https://duckduckgo.com/")
        .expect("the search base URL is a valid absolute URL");
    search.query_pairs_mut().append_pair("q", trimmed);
    search.to_string()
}

/// One key identity in the terms the page understands.
struct PageKey {
    /// DOM `KeyboardEvent.key`.
    key: String,
    /// DOM `KeyboardEvent.code`.
    code: &'static str,
    /// The virtual key code Chrome uses for editing commands and shortcuts.
    virtual_key_code: i32,
}

/// Identifies a named editing or navigation key.
///
/// Only keys whose *identity* the page needs are listed. Printable characters
/// stay on the text path below, which is where their characters come from.
fn named_page_key(key: &str) -> Option<PageKey> {
    let (key_name, code, virtual_key_code) = match key {
        "enter" => ("Enter", "Enter", 13),
        "tab" => ("Tab", "Tab", 9),
        "backspace" => ("Backspace", "Backspace", 8),
        "escape" => ("Escape", "Escape", 27),
        "space" => (" ", "Space", 32),
        "delete" => ("Delete", "Delete", 46),
        "insert" => ("Insert", "Insert", 45),
        "home" => ("Home", "Home", 36),
        "end" => ("End", "End", 35),
        "pageup" => ("PageUp", "PageUp", 33),
        "pagedown" => ("PageDown", "PageDown", 34),
        "up" => ("ArrowUp", "ArrowUp", 38),
        "down" => ("ArrowDown", "ArrowDown", 40),
        "left" => ("ArrowLeft", "ArrowLeft", 37),
        "right" => ("ArrowRight", "ArrowRight", 39),
        "shift" => ("Shift", "ShiftLeft", 16),
        "control" => ("Control", "ControlLeft", 17),
        "alt" => ("Alt", "AltLeft", 18),
        "platform" => ("Meta", "MetaLeft", 91),
        "capslock" => ("CapsLock", "CapsLock", 20),
        "contextmenu" => ("ContextMenu", "ContextMenu", 93),
        function if function.len() > 1 && function.starts_with('f') => {
            let number = function[1..].parse::<i32>().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            let name = match number {
                1 => "F1",
                2 => "F2",
                3 => "F3",
                4 => "F4",
                5 => "F5",
                6 => "F6",
                7 => "F7",
                8 => "F8",
                9 => "F9",
                10 => "F10",
                11 => "F11",
                _ => "F12",
            };
            (name, name, 111 + number)
        }
        _ => return None,
    };
    Some(PageKey {
        key: key_name.to_string(),
        code,
        virtual_key_code,
    })
}

/// Identifies a single-character key, keyed by the character printed on it.
///
/// `dom_key` is what the page sees, so Shift+1 reports `!` while `code` and the
/// virtual key code stay those of the `1` key. The character itself is not sent:
/// it arrives through `Input.insertText`.
fn character_page_key(keystroke: &Keystroke) -> Option<PageKey> {
    let mut characters = keystroke.key.chars();
    let printed = characters.next()?;
    if characters.next().is_some() {
        return None;
    }
    let (code, virtual_key_code) = match printed {
        'a'..='z' => (
            match printed {
                'a' => "KeyA",
                'b' => "KeyB",
                'c' => "KeyC",
                'd' => "KeyD",
                'e' => "KeyE",
                'f' => "KeyF",
                'g' => "KeyG",
                'h' => "KeyH",
                'i' => "KeyI",
                'j' => "KeyJ",
                'k' => "KeyK",
                'l' => "KeyL",
                'm' => "KeyM",
                'n' => "KeyN",
                'o' => "KeyO",
                'p' => "KeyP",
                'q' => "KeyQ",
                'r' => "KeyR",
                's' => "KeyS",
                't' => "KeyT",
                'u' => "KeyU",
                'v' => "KeyV",
                'w' => "KeyW",
                'x' => "KeyX",
                'y' => "KeyY",
                _ => "KeyZ",
            },
            i32::from(printed.to_ascii_uppercase() as u8),
        ),
        '0'..='9' => (
            match printed {
                '0' => "Digit0",
                '1' => "Digit1",
                '2' => "Digit2",
                '3' => "Digit3",
                '4' => "Digit4",
                '5' => "Digit5",
                '6' => "Digit6",
                '7' => "Digit7",
                '8' => "Digit8",
                _ => "Digit9",
            },
            i32::from(printed as u8),
        ),
        ';' => ("Semicolon", 186),
        '=' => ("Equal", 187),
        ',' => ("Comma", 188),
        '-' => ("Minus", 189),
        '.' => ("Period", 190),
        '/' => ("Slash", 191),
        '`' => ("Backquote", 192),
        '[' => ("BracketLeft", 219),
        '\\' => ("Backslash", 220),
        ']' => ("BracketRight", 221),
        '\'' => ("Quote", 222),
        _ => return None,
    };
    Some(PageKey {
        key: keystroke
            .key_char
            .clone()
            .unwrap_or_else(|| printed.to_string()),
        code,
        virtual_key_code,
    })
}

/// Maps one GPUI keystroke onto the CDP key event the page receives.
///
/// Named keys and shortcuts whose identity matters travel this path; plain
/// printable characters do not, because their text arrives through
/// `EntityInputHandler` and `Input.insertText`. Sending them here as well would
/// insert every character twice, and would push the raw letters of a CJK
/// composition into the page. This mirrors how the terminal forwards keys.
/// A clipboard shortcut the panel answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardCommand {
    Copy,
    Paste,
}

/// The clipboard shortcut a keystroke means, if any.
///
/// Control on Linux and Windows, Command on macOS — GPUI reports the latter as
/// `modifiers.platform`. Shift or Alt makes it a different shortcut that the
/// page owns, so those are left alone.
fn clipboard_command(keystroke: &Keystroke) -> Option<ClipboardCommand> {
    if keystroke.modifiers.alt
        || keystroke.modifiers.shift
        || keystroke.modifiers.function
        || !(keystroke.modifiers.control || keystroke.modifiers.platform)
    {
        return None;
    }
    match keystroke.key.as_str() {
        "c" | "C" => Some(ClipboardCommand::Copy),
        "v" | "V" => Some(ClipboardCommand::Paste),
        _ => None,
    }
}

fn key_input(keystroke: &Keystroke, event_type: &str) -> Option<BrowserInput> {
    let identity = named_page_key(keystroke.key.as_str()).or_else(|| {
        let shortcut =
            keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform;
        shortcut.then(|| character_page_key(keystroke)).flatten()
    })?;
    Some(BrowserInput::Key {
        event_type: event_type.to_string(),
        key: identity.key,
        code: identity.code.to_string(),
        // The character travels on the text path, so the key event must not
        // carry it or the page would receive it twice.
        text: None,
        modifiers: mouse_modifiers(keystroke.modifiers),
        windows_key_code: identity.virtual_key_code,
    })
}

fn mouse_modifiers(modifiers: gpui::Modifiers) -> i32 {
    // CDP modifier bits: Alt 1, Control 2, Meta 4, Shift 8.
    let mut value = 0;
    if modifiers.alt {
        value |= 1;
    }
    if modifiers.control {
        value |= 2;
    }
    if modifiers.platform {
        value |= 4;
    }
    if modifiers.shift {
        value |= 8;
    }
    value
}

impl Focusable for BrowserSurface {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for BrowserSurface {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            window.invalidate_character_coordinates();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let composition_cleared = self.marked_text.take().is_some();
        if composition_cleared {
            window.invalidate_character_coordinates();
        }
        if self.dialog.is_some() {
            // A `prompt` dialog is answered by the panel, so keystrokes go into
            // the card instead of the page.
            self.prompt_input.push_str(text);
            cx.notify();
            return;
        }
        self.commit_text(text, cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        new_text: &str,
        _: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The composition popup is anchored at the frame's origin rather than
        // at a text caret: the page owns the caret and the panel cannot see it.
        let width = (range_utf16.end.saturating_sub(range_utf16.start).max(1)) as f32 * 12.0;
        Some(Bounds::new(
            element_bounds.origin,
            size(px(width), px(18.0)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.active && self.transport.is_some()
    }
}

impl Render for BrowserSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Release every parked texture. Without this the sprite atlas grows by
        // one entry per frame forever.
        for stale in self.pending_drop.drain(..) {
            let _ = window.drop_image(stale);
        }
        self.sync_address_field(window, cx);
        let toolbar = self.render_toolbar(cx);
        let takeover = self.render_takeover(cx);
        let frame = self.render_frame(cx);
        let dialog = self.render_dialog(cx);
        let file_chooser = self.render_file_chooser(cx);
        v_flex()
            .id("browser-surface")
            .track_focus(&self.focus)
            .key_context("VibexBrowser")
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(toolbar)
            .when_some(takeover, |this, takeover| this.child(takeover))
            .when(self.marked_text.is_some(), |this| {
                // The in-progress IME composition is drawn by the panel: the
                // page never sees uncommitted text.
                let marked = self.marked_text.clone().unwrap_or_default();
                this.child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().foreground)
                        .child(marked),
                )
            })
            .child(div().relative().flex_1().min_h_0().child(frame))
            .when_some(dialog, |this, dialog| this.child(dialog))
            .when_some(file_chooser, |this, chooser| this.child(chooser))
            .on_key_down(cx.listener(Self::on_page_key_down))
            .on_key_up(cx.listener(Self::on_page_key_up))
    }
}

/// A short, human-readable summary of why the panel shows nothing.
pub fn unavailable_reason_text(reason: BrowserUnavailableReason) -> SharedString {
    match reason {
        BrowserUnavailableReason::BrowserMissing => locale::text(
            "No Chromium-based browser was found on the machine running the Vibex runtime.",
            "在运行 Vibex runtime 的机器上未找到 Chromium 内核浏览器。",
            "在執行 Vibex runtime 的機器上未找到 Chromium 核心瀏覽器。",
        )
        .into(),
        BrowserUnavailableReason::RemoteRuntimeUnsupported => locale::text(
            "The paired runtime is remote; the live browser view over Remote v2 is not implemented \
             yet.",
            "已配对的 runtime 位于远程；基于 Remote v2 的实时浏览器画面尚未实现。",
            "已配對的 runtime 位於遠端；基於 Remote v2 的即時瀏覽器畫面尚未實作。",
        )
        .into(),
        BrowserUnavailableReason::RemoteDebuggingDisabled => locale::text(
            "The browser refused remote debugging. Chrome 136 and later require a non-default \
             profile, and an enterprise policy can disable it entirely.",
            "浏览器拒绝了远程调试。Chrome 136 及更高版本要求使用非默认配置文件，企业策略也可能完全禁用它。",
            "瀏覽器拒絕了遠端偵錯。Chrome 136 及更高版本要求使用非預設設定檔，企業原則也可能完全停用它。",
        )
        .into(),
        BrowserUnavailableReason::DisclaimerPending => locale::text(
            "Review and accept the embedded browser risk notice to enable the panel.",
            "请阅读并接受内嵌浏览器风险提示以启用该面板。",
            "請閱讀並接受內嵌瀏覽器風險提示以啟用該面板。",
        )
        .into(),
        BrowserUnavailableReason::FeatureDisabled => locale::text(
            "The embedded browser is disabled for this runtime.",
            "该 runtime 已禁用内嵌浏览器。",
            "該 runtime 已停用內嵌瀏覽器。",
        )
        .into(),
        BrowserUnavailableReason::PlatformUnsupported => locale::text(
            "The embedded browser is not supported on the runtime's platform.",
            "该 runtime 所在平台不支持内嵌浏览器。",
            "該 runtime 所在平台不支援內嵌瀏覽器。",
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, VisualTestContext};

    #[test]
    fn a_bare_host_becomes_http() {
        assert_eq!(normalize_address("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_address("example.com"), "http://example.com");
        assert_eq!(normalize_address("127.0.0.1:3000"), "http://127.0.0.1:3000");
    }

    #[test]
    fn an_absolute_url_is_left_alone() {
        assert_eq!(
            normalize_address("https://example.com/a?b=1"),
            "https://example.com/a?b=1"
        );
        assert_eq!(normalize_address("about:blank"), "about:blank");
    }

    #[test]
    fn a_bare_word_becomes_a_search() {
        let url = normalize_address("hello world");
        assert!(url.starts_with("https://duckduckgo.com/?q="));
        // Query encoding turns the space into `+`, which is what a search box
        // needs; both `+` and `%20` decode to a space.
        assert!(url.contains("hello+world") || url.contains("hello%20world"));
    }

    #[test]
    fn empty_input_stays_blank() {
        assert_eq!(normalize_address("   "), "about:blank");
    }

    #[test]
    fn clipboard_shortcuts_are_the_panels_own() {
        let ctrl = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        let command = gpui::Modifiers {
            platform: true,
            ..Default::default()
        };
        let ctrl_shift = gpui::Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        let ctrl_alt = gpui::Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        };
        let none = gpui::Modifiers::default();

        assert_eq!(
            clipboard_command(&keystroke("c", Some("c"), ctrl)),
            Some(ClipboardCommand::Copy)
        );
        assert_eq!(
            clipboard_command(&keystroke("v", Some("v"), command)),
            Some(ClipboardCommand::Paste)
        );
        // Paste-as-plain-text and AltGr combinations belong to the page.
        assert_eq!(clipboard_command(&keystroke("v", None, ctrl_shift)), None);
        assert_eq!(clipboard_command(&keystroke("v", None, ctrl_alt)), None);
        assert_eq!(clipboard_command(&keystroke("c", Some("c"), none)), None);
        assert_eq!(clipboard_command(&keystroke("x", Some("x"), ctrl)), None);
    }

    #[test]
    fn modifier_bits_match_the_devtools_protocol() {
        let modifiers = gpui::Modifiers {
            alt: true,
            control: true,
            platform: false,
            shift: true,
            function: false,
        };
        assert_eq!(mouse_modifiers(modifiers), 1 | 2 | 8);
    }

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: gpui::Modifiers) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            key_char: key_char.map(str::to_string),
            modifiers,
        }
    }

    /// Opens a bare surface for the entity-level tests.
    fn test_surface(cx: &mut gpui::TestAppContext) -> (Entity<BrowserSurface>, VisualTestContext) {
        cx.update(gpui_component::init);
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    cx.new(|cx| BrowserSurface::new("browser:test".to_string(), window, cx))
                })
            })
            .expect("the browser test window should open");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window
            .root(&mut cx)
            .expect("the browser test surface should exist");
        (surface, cx)
    }

    #[gpui::test]
    fn a_dialog_card_stays_with_its_own_tab(cx: &mut gpui::TestAppContext) {
        let (surface, mut cx) = test_surface(cx);
        let attached = BrowserTabId::new();
        let other = BrowserTabId::new();
        let dialog = |tab_id: &BrowserTabId| BrowserDialogRequest {
            tab_id: tab_id.clone(),
            dialog_type: "alert".to_string(),
            message: "hello".to_string(),
            default_prompt: None,
            at_ms: 0,
        };

        surface.update(&mut cx, |surface, cx| {
            surface.tab_id = Some(attached.clone());
            surface.show_dialog(dialog(&other), cx);
            assert!(
                surface.dialog.is_none(),
                "a dialog for another tab must not take over this card"
            );
            surface.show_dialog(dialog(&attached), cx);
            assert!(surface.dialog.is_some());
            surface.dismiss_dialog(&other, cx);
            assert!(
                surface.dialog.is_some(),
                "another tab's dismissal must not clear this card"
            );
            surface.dismiss_dialog(&attached, cx);
            assert!(surface.dialog.is_none());
        });
    }

    #[gpui::test]
    fn availability_and_takeover_state_come_from_the_runtime(cx: &mut gpui::TestAppContext) {
        let (surface, mut cx) = test_surface(cx);
        surface.update(&mut cx, |surface, cx| {
            assert!(!surface.agent_paused, "a fresh surface is not taken over");
            surface.set_availability(
                BrowserAvailability::unavailable(
                    BrowserUnavailableReason::BrowserMissing,
                    Some("no browser".to_string()),
                ),
                cx,
            );
            assert!(surface.availability.is_some());
        });
    }

    fn key_fields(input: &vibex_browser::BrowserInput) -> (String, String, String, i32, i32) {
        let vibex_browser::BrowserInput::Key {
            event_type,
            key,
            code,
            modifiers,
            windows_key_code,
            ..
        } = input
        else {
            panic!("not a key input: {input:?}");
        };
        (
            event_type.clone(),
            key.clone(),
            code.clone(),
            modifiers.to_owned(),
            *windows_key_code,
        )
    }

    #[test]
    fn editing_keys_reach_the_page_with_their_virtual_key_codes() {
        let none = gpui::Modifiers::default();
        let enter = key_input(&keystroke("enter", None, none), "rawKeyDown").unwrap();
        assert_eq!(
            key_fields(&enter),
            (
                "rawKeyDown".to_string(),
                "Enter".to_string(),
                "Enter".to_string(),
                0,
                13
            )
        );
        let backspace = key_input(&keystroke("backspace", None, none), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&backspace).4, 8);
        let delete = key_input(&keystroke("delete", None, none), "keyUp").unwrap();
        assert_eq!(key_fields(&delete).0, "keyUp");
        let arrow = key_input(&keystroke("left", None, none), "rawKeyDown").unwrap();
        assert_eq!(
            key_fields(&arrow),
            (
                "rawKeyDown".to_string(),
                "ArrowLeft".to_string(),
                "ArrowLeft".to_string(),
                0,
                37
            )
        );
        let function = key_input(&keystroke("f5", None, none), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&function).4, 116);
    }

    #[test]
    fn shortcuts_carry_their_modifiers() {
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        // Ctrl+A has no character of its own, so `key` names the key.
        let select_all = key_input(&keystroke("a", None, control), "rawKeyDown").unwrap();
        assert_eq!(
            key_fields(&select_all),
            (
                "rawKeyDown".to_string(),
                "a".to_string(),
                "KeyA".to_string(),
                2,
                65
            )
        );
        // Alt+arrow and Shift+Enter are named keys, so they travel with their
        // modifiers rather than through the text path.
        let alt_left = gpui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let back = key_input(&keystroke("left", None, alt_left), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&back).3, 1);
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let newline = key_input(&keystroke("enter", None, shift), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&newline).3, 8);
        // A shifted printable is a character, not a shortcut: it stays on the
        // text path, but its identity is still the key it was typed on.
        assert!(key_input(&keystroke("1", Some("!"), shift), "rawKeyDown").is_none());
        let bang = character_page_key(&keystroke("1", Some("!"), shift)).unwrap();
        assert_eq!(
            (bang.key.as_str(), bang.code, bang.virtual_key_code),
            ("!", "Digit1", 49)
        );
    }

    #[test]
    fn printable_keys_stay_on_the_text_path() {
        let none = gpui::Modifiers::default();
        // Typing "a" or a space must not produce a key event: the character is
        // committed through the input handler, and sending it here as well
        // would insert it twice (and leak raw keys from a CJK composition).
        assert!(key_input(&keystroke("a", Some("a"), none), "rawKeyDown").is_none());
        assert!(key_input(&keystroke(";", Some(";"), none), "rawKeyDown").is_none());
        assert!(key_input(&keystroke("中", Some("中"), none), "rawKeyDown").is_none());
    }

    #[test]
    fn decoding_swaps_red_and_blue_for_the_bgra_atlas() {
        // A single red pixel must come back with the red channel last, because
        // `RenderImage` is BGRA.
        let mut source = image::RgbaImage::new(1, 1);
        source.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        source
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let decoded = decode_frame(encoded.get_ref()).unwrap();
        assert_eq!(decoded.width, 1);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.image.get_pixel(0, 0).0, [0, 0, 255, 255]);
    }

    #[test]
    fn undecodable_bytes_produce_an_error_rather_than_a_panic() {
        assert!(decode_frame(b"not an image").is_err());
    }

    #[test]
    fn point_conversion_scales_between_the_panel_and_the_viewport() {
        let surface = SurfaceGeometry {
            frame_bounds: Some(Bounds::new(
                Point::new(px(10.0), px(20.0)),
                size(px(500.0), px(250.0)),
            )),
            frame_pixel_size: (1000.0, 500.0),
            frame_metadata: BrowserFrameMetadata::default(),
        };
        let (x, y) = surface
            .to_viewport_point(Point::new(px(260.0), px(145.0)))
            .unwrap();
        assert_eq!((x, y), (500.0, 250.0));
    }

    #[test]
    fn point_conversion_ignores_the_scroll_offset() {
        let surface = SurfaceGeometry {
            frame_bounds: Some(Bounds::new(
                Point::new(px(0.0), px(0.0)),
                size(px(100.0), px(100.0)),
            )),
            frame_pixel_size: (100.0, 100.0),
            frame_metadata: BrowserFrameMetadata {
                scroll_offset_y: 900.0,
                ..BrowserFrameMetadata::default()
            },
        };
        let (x, y) = surface
            .to_viewport_point(Point::new(px(10.0), px(20.0)))
            .unwrap();
        // Adding the scroll offset here is the classic highlight-drift bug.
        assert_eq!((x, y), (10.0, 20.0));
    }

    #[test]
    fn points_outside_the_frame_are_ignored() {
        let surface = SurfaceGeometry {
            frame_bounds: Some(Bounds::new(
                Point::new(px(0.0), px(0.0)),
                size(px(100.0), px(100.0)),
            )),
            frame_pixel_size: (100.0, 100.0),
            frame_metadata: BrowserFrameMetadata::default(),
        };
        assert!(
            surface
                .to_viewport_point(Point::new(px(500.0), px(500.0)))
                .is_none()
        );
    }

    #[test]
    fn conversion_needs_a_frame_to_scale_against() {
        let surface = SurfaceGeometry {
            frame_bounds: Some(Bounds::new(
                Point::new(px(0.0), px(0.0)),
                size(px(100.0), px(100.0)),
            )),
            frame_pixel_size: (0.0, 0.0),
            frame_metadata: BrowserFrameMetadata::default(),
        };
        assert!(
            surface
                .to_viewport_point(Point::new(px(1.0), px(1.0)))
                .is_none()
        );
    }

    #[test]
    fn unavailable_reasons_all_have_operator_facing_copy() {
        for reason in [
            BrowserUnavailableReason::BrowserMissing,
            BrowserUnavailableReason::RemoteRuntimeUnsupported,
            BrowserUnavailableReason::RemoteDebuggingDisabled,
            BrowserUnavailableReason::DisclaimerPending,
            BrowserUnavailableReason::FeatureDisabled,
            BrowserUnavailableReason::PlatformUnsupported,
        ] {
            assert!(!unavailable_reason_text(reason).is_empty());
        }
    }

    /// Mirrors the surface's coordinate math without needing a GPUI context.
    struct SurfaceGeometry {
        frame_bounds: Option<Bounds<Pixels>>,
        frame_pixel_size: (f32, f32),
        frame_metadata: BrowserFrameMetadata,
    }

    impl SurfaceGeometry {
        fn to_viewport_point(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
            let bounds = self.frame_bounds?;
            let (device_width, device_height) = if self.frame_pixel_size.0 > 0.0 {
                self.frame_pixel_size
            } else {
                return None;
            };
            let display_width = f32::from(bounds.size.width).max(1.0);
            let display_height = f32::from(bounds.size.height).max(1.0);
            let local_x = f32::from(position.x - bounds.origin.x);
            let local_y = f32::from(position.y - bounds.origin.y);
            if local_x < 0.0 || local_y < 0.0 || local_x > display_width || local_y > display_height
            {
                return None;
            }
            let scale_x = device_width as f64 / display_width as f64;
            let scale_y = device_height as f64 / display_height as f64;
            let page_scale = if self.frame_metadata.page_scale_factor > 0.0 {
                self.frame_metadata.page_scale_factor
            } else {
                1.0
            };
            Some((
                local_x as f64 * scale_x / page_scale,
                local_y as f64 * scale_y / page_scale,
            ))
        }
    }

    #[test]
    fn the_surface_geometry_helper_matches_the_surface_rules() {
        // Keeps the mirrored helper honest: if the surface's rule changes, this
        // test fails and the mirror has to change with it.
        let surface = SurfaceGeometry {
            frame_bounds: Some(Bounds::new(
                Point::new(px(0.0), px(0.0)),
                size(px(50.0), px(50.0)),
            )),
            frame_pixel_size: (100.0, 100.0),
            frame_metadata: BrowserFrameMetadata::default(),
        };
        assert_eq!(
            surface.to_viewport_point(Point::new(px(25.0), px(25.0))),
            Some((50.0, 50.0))
        );
    }

    /// Records what the surface forwards, so an input-path regression fails
    /// here instead of silently doing nothing in the panel.
    struct RecordingTransport {
        inputs: Arc<std::sync::Mutex<Vec<String>>>,
        snapshot: Arc<std::sync::Mutex<Option<vibex_core::BrowserSessionSnapshot>>>,
    }

    impl BrowserTransport for RecordingTransport {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn availability(
            &self,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, BrowserAvailability> {
            Box::pin(async {
                Ok(BrowserAvailability::unavailable(
                    BrowserUnavailableReason::BrowserMissing,
                    None,
                ))
            })
        }
        fn list_sessions(
            &self,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, Vec<vibex_core::BrowserSession>>
        {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn session_snapshot(
            &self,
            _session_id: &BrowserSessionId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, vibex_core::BrowserSessionSnapshot>
        {
            let snapshot = self.snapshot.lock().unwrap().clone();
            Box::pin(async move {
                snapshot.ok_or_else(|| {
                    BrowserTransportError::new("probe", "no snapshot configured for this test")
                })
            })
        }
        fn ensure_workspace_session(
            &self,
            _workspace_id: &vibex_core::WorkspaceId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, BrowserSessionId> {
            Box::pin(async { Ok(BrowserSessionId::new()) })
        }
        fn create_tab(
            &self,
            _session_id: &BrowserSessionId,
            _url: Option<&str>,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, BrowserTab> {
            Box::pin(async { Err(BrowserTransportError::new("probe", "not used by this test")) })
        }
        fn close_tab(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn select_tab(
            &self,
            _session_id: &BrowserSessionId,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn set_viewport(
            &self,
            _tab_id: &BrowserTabId,
            _width: u32,
            _height: u32,
            _device_scale_factor: f64,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn dispatch_input(
            &self,
            _tab_id: &BrowserTabId,
            input: vibex_browser::BrowserInput,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            let inputs = self.inputs.clone();
            Box::pin(async move {
                inputs.lock().unwrap().push(format!("{input:?}"));
                Ok(())
            })
        }
        fn navigate(
            &self,
            _tab_id: &BrowserTabId,
            _url: &str,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn reload(
            &self,
            _tab_id: &BrowserTabId,
            _ignore_cache: bool,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn selection_text(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
        fn subscribe_frames(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, BrowserFrameStream> {
            Box::pin(async { Ok(BrowserFrameStream::Unavailable) })
        }
        fn stop_screencast(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    // Regression: the frame geometry used to come from
    // `ElementExt::on_prepaint`, whose helper canvas is laid out after the
    // in-flow content. Its origin was the bottom edge of the frame area, so
    // every pointer position converted to a negative local coordinate and the
    // panel dropped all mouse input while still accepting IME text.
    fn snapshot_with(tabs: Vec<vibex_core::BrowserTab>) -> vibex_core::BrowserSessionSnapshot {
        vibex_core::BrowserSessionSnapshot {
            session: vibex_core::BrowserSession {
                session_id: BrowserSessionId::new(),
                workspace_id: None,
                tabs,
                active_tab_id: None,
                agent_tab_id: None,
                execution_source: vibex_core::BrowserExecutionSource::User,
                user_engaged: true,
                created_at_ms: 0,
                last_activity_at_ms: 0,
            },
            ledger: Vec::new(),
            availability: BrowserAvailability::unavailable(
                BrowserUnavailableReason::BrowserMissing,
                None,
            ),
        }
    }

    fn tab_with(
        tab_id: &BrowserTabId,
        title: &str,
        status: BrowserTabStatus,
    ) -> vibex_core::BrowserTab {
        vibex_core::BrowserTab {
            tab_id: tab_id.clone(),
            url: "https://example.com/".to_string(),
            title: title.to_string(),
            status,
            owner: vibex_core::BrowserTabOwner::User,
            agent_session_id: None,
            created_at_ms: 0,
            last_activity_at_ms: 0,
            generation: 1,
        }
    }

    // The preview tab shows the page's title, and a spinner while it loads, so
    // the surface has to tell its owner when either changes.
    #[gpui::test]
    fn the_owner_learns_the_title_and_the_loading_state(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let tab_id = BrowserTabId::new();
        let snapshot = Arc::new(std::sync::Mutex::new(Some(snapshot_with(vec![tab_with(
            &tab_id,
            "Example",
            BrowserTabStatus::Loading,
        )]))));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
            snapshot: snapshot.clone(),
        });
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| BrowserSurface::new(tab_id.as_str().to_string(), window, cx))
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_events = seen.clone();
        let _subscription = surface.update(&mut cx, |_, cx| {
            cx.subscribe_self(move |_, event: &BrowserSurfaceEvent, _| {
                if let BrowserSurfaceEvent::TabChanged { tab_id } = event {
                    seen_events
                        .lock()
                        .unwrap()
                        .push(tab_id.as_str().to_string());
                }
            })
        });
        surface.update(&mut cx, |surface, cx| {
            surface.attach(transport, BrowserSessionId::new(), tab_id.clone(), cx);
            surface.refresh_tab(cx);
        });
        cx.run_until_parked();
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.page_title()),
            Some("Example".to_string())
        );
        assert!(surface.read_with(&cx, |surface, _| surface.is_loading()));

        *snapshot.lock().unwrap() = Some(snapshot_with(vec![tab_with(
            &tab_id,
            "Loaded",
            BrowserTabStatus::Ready,
        )]));
        surface.update(&mut cx, |surface, cx| surface.refresh_tab(cx));
        cx.run_until_parked();
        assert!(!surface.read_with(&cx, |surface, _| surface.is_loading()));
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.page_title()),
            Some("Loaded".to_string())
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "a change has to reach the preview tab"
        );
    }

    #[gpui::test]
    fn a_click_on_the_page_reaches_the_transport(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            inputs: inputs.clone(),
            snapshot: Arc::new(std::sync::Mutex::new(None)),
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface =
                        BrowserSurface::new("browser_tab_probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface.set_active(true, cx);
                    surface
                })
            })
            .expect("browser probe window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, cx| {
            surface.frame_pixel_size = (1000.0, 600.0);
            cx.notify();
        });
        cx.run_until_parked();

        let bounds = surface
            .read_with(&cx, |surface, _| surface.frame_bounds)
            .expect("the frame is laid out");
        let frame_size = surface.read_with(&cx, |surface, _| surface.frame_pixel_size);
        // A click in the middle of the visible frame area must arrive as a
        // positive viewport coordinate, which is only true when the frame's
        // origin was captured instead of its bottom edge.
        cx.simulate_mouse_down(
            bounds.center(),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();

        let recorded = inputs.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            1,
            "expected one dispatched input: {recorded:?}"
        );
        let entry = &recorded[0];
        assert!(entry.contains("MouseDown"), "not a press: {entry}");
        let (x, y) = parse_mouse_down_point(entry);
        assert!(
            x > 0.0 && x < f64::from(frame_size.0) && y > 0.0 && y < f64::from(frame_size.1),
            "the press landed outside the viewport: ({x}, {y}) in {frame_size:?}"
        );
    }

    /// Reads the `x`/`y` out of the `Debug` rendering of a mouse-down input.
    fn parse_mouse_down_point(entry: &str) -> (f64, f64) {
        let number_after = |key: &str| {
            entry
                .split(key)
                .nth(1)
                .and_then(|rest| {
                    rest.trim_start()
                        .split(|character: char| {
                            !(character.is_ascii_digit() || character == '.' || character == '-')
                        })
                        .next()
                })
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or_else(|| panic!("no `{key}` in {entry}"))
        };
        (number_after("x: "), number_after("y: "))
    }
}
