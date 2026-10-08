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
    Animation, AnimationExt as _, AnyElement, App, Bounds, BoxShadow, Context, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, Keystroke, MouseButton, ParentElement as _, Pixels,
    Point, Render, RenderImage, Role, SharedString, Styled as _, Subscription, Task,
    UTF16Selection, Window, canvas, div, img, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::Button,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
    progress::Progress,
    tooltip::Tooltip,
    v_flex,
};
use image::Frame;
use vibex_browser::BrowserInput;
use vibex_core::{
    BrowserActionKind, BrowserActionRecord, BrowserAvailability, BrowserCaptureQuality,
    BrowserDialogRequest, BrowserExecutionSource, BrowserFrame, BrowserFrameMetadata,
    BrowserOperationStatus, BrowserSessionId, BrowserTab, BrowserTabId, BrowserTabOwner,
    BrowserTabStatus, BrowserUnavailableReason, unix_timestamp_ms,
};

use vibex_desktop_model::SEARCH_QUERY_PLACEHOLDER;

use crate::browser_transport::{
    BrowserFrameStream, BrowserTransport, BrowserTransportError, LocalBrowserTransport,
};
use crate::locale;

/// A short label for one ledger entry's action.
///
/// A kind this build does not know is shown as `other`: the enum is
/// unknown-safe so a newer runtime's entry can still be listed.
fn activity_kind(kind: BrowserActionKind) -> &'static str {
    match kind {
        BrowserActionKind::Launch => "launch",
        BrowserActionKind::Navigate => "navigate",
        BrowserActionKind::Observe => "observe",
        BrowserActionKind::Find => "find",
        BrowserActionKind::Click => "click",
        BrowserActionKind::Fill => "fill",
        BrowserActionKind::Press => "press",
        BrowserActionKind::Hover => "hover",
        BrowserActionKind::Scroll => "scroll",
        BrowserActionKind::SelectOption => "select",
        BrowserActionKind::Drag => "drag",
        BrowserActionKind::Upload => "upload",
        BrowserActionKind::Extract => "extract",
        BrowserActionKind::Screenshot => "screenshot",
        BrowserActionKind::Evaluate => "script",
        BrowserActionKind::WaitFor => "wait",
        BrowserActionKind::ListTabs => "tabs",
        BrowserActionKind::CreateTab => "new tab",
        BrowserActionKind::SelectTab => "switch",
        BrowserActionKind::CloseTab => "close tab",
        BrowserActionKind::PreviewOpen => "preview",
        BrowserActionKind::ConsoleMessages => "console",
        BrowserActionKind::NetworkRequests => "network",
        BrowserActionKind::HandleDialog => "dialog",
        BrowserActionKind::RequestHelp => "help",
        BrowserActionKind::SnapshotBaseline => "baseline",
        BrowserActionKind::CompareBaseline => "compare",
        BrowserActionKind::ElementToSource => "source",
        BrowserActionKind::Download => "download",
        BrowserActionKind::Unknown => "other",
    }
}

/// How long ago an entry happened, in the shortest honest form.
fn relative_time(at_ms: i64, now_ms: i64) -> String {
    let seconds = ((now_ms - at_ms).max(0) / 1_000) as u64;
    match seconds {
        0..=9 => "now".to_string(),
        10..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3_600),
    }
}

/// How long the panel waits after a resize before telling the browser.
///
/// Resizing the window fires continuously; `Emulation.setDeviceMetricsOverride`
/// re-lays-out the page, so it must not run per frame.
const VIEWPORT_DEBOUNCE: Duration = Duration::from_millis(160);
/// Diameter of the Agent's play/pause control.
const AGENT_CONTROL_SIZE: f32 = 58.0;
/// Distance the control keeps from the frame's corner.
const AGENT_CONTROL_MARGIN: f32 = 20.0;

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
    /// Alt+click resolved the element under the pointer to a source location.
    ///
    /// `path` is workspace-relative, the way the editor's find-and-reveal wants
    /// it; `approximate` says the framework could not give an exact line.
    SourceLocated {
        path: String,
        line: Option<u32>,
        approximate: bool,
    },
    /// The reader switched the encoder from the toolbar, so the choice should
    /// outlive this surface and reach the settings that own it.
    CaptureQualityChanged(BrowserCaptureQuality),
    /// The reader asked to choose the file a page's file chooser is waiting
    /// for. The host owns the file browser that answers it.
    FileChooserPickRequested { tab_id: BrowserTabId },
}

/// A GPUI surface for one browser tab.
/// Textures waiting for a window to release them.
///
/// Shared between the browser surfaces and the workbench: a surface that is
/// removed from the panel never paints again, so the paint that frees its atlas
/// tiles has to happen somewhere else.
pub type OrphanTextures = std::rc::Rc<std::cell::RefCell<Vec<Arc<RenderImage>>>>;

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
    /// Textures handed to the workbench because this entity is going away.
    ///
    /// A removed surface never paints again, so anything it still holds would
    /// keep its atlas tile for the life of the process. The workbench owns the
    /// paint that can release them, so the images are parked in a queue shared
    /// with it.
    orphans: Option<OrphanTextures>,
    frame_metadata: BrowserFrameMetadata,
    /// Panel-space bounds of the frame, used to convert pointer positions.
    frame_bounds: Option<Bounds<Pixels>>,
    /// Encoded pixel size of the current frame.
    frame_pixel_size: (f32, f32),
    /// The latest debounced request, in logical pixels. Cleared when cancelled.
    requested_viewport: Option<(u32, u32, u32)>,
    viewport_task: Option<Task<()>>,
    frame_task: Option<Task<()>>,
    /// The stop-screencast call a deactivation sent. It is cancelled when the
    /// tab is shown again, because a stop that arrives after the restart would
    /// leave the panel with no frames at all.
    stop_task: Option<Task<()>>,
    dialog: Option<BrowserDialogRequest>,
    prompt_input: String,
    file_chooser_pending: bool,
    /// Downloads this tab has announced, newest last.
    ///
    /// The panel is the only place a headless download can be watched: Chrome
    /// has no shelf of its own, so progress and the saved path are shown here.
    downloads: Vec<vibex_browser::BrowserDownload>,
    /// The timer that takes finished downloads off the screen.
    downloads_hide_task: Option<Task<()>>,
    /// A `<select>` under the pointer, whose popup headless Chrome never paints.
    ///
    /// Probed on hover rather than on click: the panel forwards the click it
    /// receives, and a click that arrived while the probe was in flight would
    /// reach the page after its own release.
    select_hint: Option<SelectHint>,
    /// The open fallback menu, anchored where the click landed.
    select_menu: Option<OpenSelectMenu>,
    /// True while the find bar is open. The page keeps its highlights until the
    /// bar closes, so the reader can step through hits and still see them.
    find_open: bool,
    /// What the reader typed into the find bar.
    find_input: Entity<InputState>,
    /// The match counter, as the page reported it: `current` of `total`.
    find_total: u32,
    find_current: u32,
    /// True while a search is in flight, so the bar can say so instead of
    /// showing a count that is about to change.
    find_pending: bool,
    /// The session's redacted operation ledger, newest last.
    ///
    /// This is what lets a human see what the Agent did without having watched
    /// the whole run; the entries carry no page content by construction.
    ledger: Vec<BrowserActionRecord>,
    /// True while the activity list is docked under the page.
    ledger_open: bool,
    /// Set while the ledger is being fetched, so the list can say so.
    ledger_pending: bool,
    ledger_error: Option<String>,
    /// Why the last Alt+click could not be mapped to a file, shown inline.
    source_notice: Option<String>,
    /// The page's icon, once the runtime has fetched and this side decoded it.
    favicon: Option<Arc<RenderImage>>,
    /// The URL the current icon came from, so a repaint does not refetch it.
    favicon_source: Option<String>,
    /// True while the runtime reports an active recording on this session.
    recording: bool,
    /// True while a hover probe is outstanding.
    select_probe_in_flight: bool,
    /// The cursor the page asked for at the last hovered point.
    ///
    /// The screencast carries no cursor, so without this the panel shows an
    /// arrow over every link, text field and resize handle alike.
    hover_cursor: gpui::CursorStyle,
    /// True while a cursor probe is outstanding, and when the last one ran.
    cursor_probe_in_flight: bool,
    cursor_probed_at: Option<std::time::Instant>,
    /// The frame's hitbox, shared with the paint that applies the cursor.
    frame_hitbox: std::rc::Rc<std::cell::RefCell<Option<gpui::Hitbox>>>,
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
    /// URL template the address bar searches with, `{query}` included.
    ///
    /// Pushed in by the owner, which is where the setting lives: the surface
    /// only turns a typed keyword into the address the configured engine
    /// expects.
    search_template: String,
    /// What the panel asks the encoder for. JPEG 80 by default; the toolbar's
    /// HD toggle switches to lossless PNG.
    capture_quality: BrowserCaptureQuality,
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
        let find_input = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder("Find in page")
        });
        // Enter steps forward, Shift+Enter back — the same contract as the
        // browser's own find bar. The page is searched as the reader types, so
        // there is nothing to submit besides the step.
        let find_subscription = cx.subscribe_in(
            &find_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.run_find(false, true, cx),
                InputEvent::PressEnter { shift, .. } => this.run_find(*shift, false, cx),
                _ => {
                    let _ = window;
                }
            },
        );
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
            orphans: None,
            frame_metadata: BrowserFrameMetadata::default(),
            frame_bounds: None,
            frame_pixel_size: (0.0, 0.0),
            requested_viewport: None,
            viewport_task: None,
            frame_task: None,
            stop_task: None,
            dialog: None,
            recording: false,
            prompt_input: String::new(),
            file_chooser_pending: false,
            downloads: Vec::new(),
            downloads_hide_task: None,
            select_hint: None,
            select_menu: None,
            hover_cursor: gpui::CursorStyle::Arrow,
            cursor_probe_in_flight: false,
            cursor_probed_at: None,
            frame_hitbox: std::rc::Rc::new(std::cell::RefCell::new(None)),
            find_open: false,
            find_input,
            find_total: 0,
            find_current: 0,
            find_pending: false,
            ledger: Vec::new(),
            ledger_open: false,
            ledger_pending: false,
            ledger_error: None,
            source_notice: None,
            favicon: None,
            favicon_source: None,
            select_probe_in_flight: false,
            execution_source: None,
            agent_paused: false,
            focus: cx.focus_handle(),
            marked_text: None,
            active: false,
            _subscriptions: vec![address_subscription, find_subscription],
            address_input,
            search_template: vibex_desktop_model::BrowserUiState::default()
                .resolved_search_url()
                .to_string(),
            capture_quality: BrowserCaptureQuality::default(),
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
        self.viewport_task = None;
        self.requested_viewport = None;
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

    /// The address the runtime reports for this tab.
    ///
    /// The owner persists it: the page's address is the only browser state that
    /// can outlive the runtime's tab, so it is what a restart reopens.
    pub fn page_url(&self) -> Option<String> {
        let url = self.tab.as_ref()?.url.trim();
        (!url.is_empty()).then(|| url.to_string())
    }

    /// The page's icon, for the preview tab.
    pub fn favicon(&self) -> Option<Arc<RenderImage>> {
        self.favicon.clone()
    }

    /// Who opened this tab, as the runtime reports it.
    ///
    /// The preview tab strip marks an Agent's tabs: a human watching a page
    /// move on its own should be able to tell, at a glance, that an Agent is
    /// driving rather than a stray click.
    pub fn tab_owner(&self) -> Option<BrowserTabOwner> {
        self.tab.as_ref().map(|tab| tab.owner)
    }

    /// The Agent session behind an Agent tab, if the runtime named one.
    ///
    /// The tab strip uses it to draw the operating Agent's own mark rather than
    /// a generic robot; the identity itself lives in the client, not in the
    /// browser session.
    pub fn agent_session_id(&self) -> Option<&vibex_core::VibexSessionId> {
        self.tab.as_ref()?.agent_session_id.as_ref()
    }

    /// Fetches the page's icon once per URL.
    ///
    /// The bytes come from the runtime, so a paired client shows the icon even
    /// though it cannot reach the site itself.
    fn ensure_favicon(&mut self, url: String, cx: &mut Context<Self>) {
        if url.trim().is_empty() || self.favicon_source.as_deref() == Some(url.as_str()) {
            return;
        }
        self.favicon_source = Some(url);
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(Some(favicon)) = transport.favicon(&tab_id).await else {
                return;
            };
            let decoded = cx
                .background_executor()
                .spawn(async move { decode_frame(&favicon.bytes) })
                .await;
            let Ok(decoded) = decoded else {
                return;
            };
            let _ = this.update(cx, |surface, cx| {
                let next = Arc::new(RenderImage::new(vec![Frame::new(decoded.image)]));
                // The atlas has no eviction: replacing an image without asking
                // the window to drop it leaves the old tile resident.
                if let Some(previous) = surface.favicon.replace(next) {
                    surface.pending_drop.push(previous);
                }
                if let Some(tab_id) = surface.tab_id.clone() {
                    cx.emit(BrowserSurfaceEvent::TabChanged { tab_id });
                }
                cx.notify();
            });
        })
        .detach();
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

    /// Points the address bar at the search engine the settings chose.
    ///
    /// Whatever a reader already typed stays put: the setting applies to the
    /// next search, not to a bar someone is in the middle of filling.
    pub fn set_search_template(&mut self, template: impl Into<String>, cx: &mut Context<Self>) {
        let template = template.into();
        if self.search_template == template {
            return;
        }
        self.search_template = template;
        cx.notify();
    }

    /// The engine the address bar would search with, for tests that pin the
    /// wiring from the settings to an open panel.
    #[cfg(test)]
    pub(crate) fn search_template(&self) -> &str {
        &self.search_template
    }

    /// Switches the encoder between JPEG 80 and lossless PNG.
    ///
    /// A live stream has to be restarted for the change to take effect, which
    /// is why this is not just a field write: the reader pressed the button
    /// because the picture was not good enough, and leaving the old encoder
    /// running would look like the button did nothing.
    pub fn set_capture_quality(&mut self, quality: BrowserCaptureQuality, cx: &mut Context<Self>) {
        if self.capture_quality == quality {
            return;
        }
        self.capture_quality = quality;
        if self.active && self.tab_id.is_some() && self.transport.is_some() {
            self.start_frame_pump(cx);
        }
        cx.notify();
    }

    /// The encoder mode in force, for tests.
    #[cfg(test)]
    pub(crate) fn capture_quality(&self) -> BrowserCaptureQuality {
        self.capture_quality
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
            self.requested_viewport = None;
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

    /// Opens or closes the activity list, fetching the ledger when it opens.
    pub fn toggle_ledger(&mut self, cx: &mut Context<Self>) {
        self.ledger_open = !self.ledger_open;
        if self.ledger_open {
            self.refresh_ledger(cx);
        }
        cx.notify();
    }

    /// Reads the session's ledger from the runtime.
    pub fn refresh_ledger(&mut self, cx: &mut Context<Self>) {
        let (Some(transport), Some(session_id)) = (self.transport.clone(), self.session_id.clone())
        else {
            return;
        };
        self.ledger_pending = true;
        self.ledger_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = transport.ledger(&session_id).await;
            let _ = this.update(cx, |surface, cx| {
                surface.ledger_pending = false;
                match result {
                    Ok(records) => surface.ledger = records,
                    Err(error) => surface.ledger_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Resolves the element under a viewport point to the file that drew it.
    ///
    /// Nothing happens silently: a page whose framework cannot answer shows the
    /// probe's own explanation, because "I clicked and nothing happened" is the
    /// one outcome the design forbids.
    pub fn locate_source(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = transport.element_source_at(&tab_id, x, y).await;
            let _ = this.update(cx, |surface, cx| {
                match result {
                    Ok(source) => {
                        let path =
                            vibex_browser::element_source::normalize_source_path(&source.path);
                        match path {
                            Some(path) => {
                                // A file without a line is still useful, but
                                // saying the line is missing beats pretending
                                // the caret landed where the element is.
                                surface.source_notice = source.approximate.then(|| {
                                    locale::text(
                                        "Opened the element's component file; this build does not \
                                         expose the exact line.",
                                        "已打开该元素所在的组件文件；此构建未暴露精确行号。",
                                        "已開啟該元素所在的元件檔案；此建置未暴露精確行號。",
                                    )
                                    .to_string()
                                });
                                cx.emit(BrowserSurfaceEvent::SourceLocated {
                                    path,
                                    line: source.line,
                                    approximate: source.approximate,
                                });
                            }
                            None => {
                                surface.source_notice =
                                    Some(source.detail.clone().unwrap_or_else(|| {
                                        "this element could not be mapped to a source file"
                                            .to_string()
                                    }));
                            }
                        }
                    }
                    Err(error) => surface.source_notice = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Appends one operation the runtime just reported.
    ///
    /// The runtime is the source of truth and the panel is a subscriber, so a
    /// record that arrives while the list is open appears without a refetch.
    pub fn receive_ledger_record(&mut self, record: BrowserActionRecord, cx: &mut Context<Self>) {
        if record.tab_id != self.tab_id.clone().unwrap_or_default() {
            return;
        }
        self.ledger.push(record);
        let excess = self
            .ledger
            .len()
            .saturating_sub(vibex_core::BROWSER_MAX_SESSION_LEDGER_ITEMS);
        if excess > 0 {
            self.ledger.drain(..excess);
        }
        if self.ledger_open {
            cx.notify();
        }
    }

    /// Points the surface at a different runtime tab.
    pub fn set_tab(&mut self, tab_id: Option<BrowserTabId>, cx: &mut Context<Self>) {
        if self.tab_id == tab_id {
            return;
        }
        self.tab_id = tab_id;
        self.ledger.clear();
        self.ledger_error = None;
        if let Some(previous) = self.frame_image.take() {
            self.pending_drop.push(previous);
        }
        self.dialog = None;
        self.file_chooser_pending = false;
        // The next tab's downloads are its own; a card left over from the last
        // one would report progress for a page that is gone.
        self.downloads.clear();
        self.downloads_hide_task = None;
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
                let was_driving = surface.agent_driving();
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
                surface.recording = snapshot.session.recording;
                surface.agent_paused = snapshot.session.execution_source
                    == BrowserExecutionSource::User
                    && snapshot.session.user_engaged;
                // The preview tab shows the page's title, whether it is still
                // loading, and whether the Agent is driving it, so a change to
                // any of those has to reach the owner.
                let driving_changed = surface.agent_driving() != was_driving;
                if (surface.tab != previous || driving_changed)
                    && let Some(tab_id) = surface.tab_id.clone()
                {
                    cx.emit(BrowserSurfaceEvent::TabChanged { tab_id });
                }
                if let Some(tab) = surface.tab.as_ref()
                    && tab.status == BrowserTabStatus::Ready
                {
                    surface.ensure_favicon(tab.url.clone(), cx);
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
        let quality = self.capture_quality;
        self.frame_task = None;
        self.phase = SurfacePhase::Connecting;
        self.frame_task = Some(cx.spawn(async move |this, cx| {
            // A stream that ends or fails while the panel still shows this tab
            // used to leave the last frame on screen for good: the picture
            // looked live and answered nothing. Re-subscribing is safe because
            // the runtime stops the screencast before starting it again, so a
            // transient failure now recovers on its own.
            const ATTEMPTS: usize = 3;
            for attempt in 0..ATTEMPTS {
                if attempt > 0 {
                    cx.background_executor()
                        .timer(Duration::from_millis(400 * attempt as u64))
                        .await;
                }
                let mut stream = match transport.subscribe_frames(&tab_id, quality).await {
                    Ok(stream) => stream,
                    Err(error) => {
                        let alive = this.update(cx, |surface, cx| {
                            surface.apply_transport_error(&error);
                            cx.notify();
                        });
                        if alive.is_err() {
                            return;
                        }
                        continue;
                    }
                };
                if matches!(stream, BrowserFrameStream::Unavailable) {
                    // This client has no frame channel at all; retrying cannot
                    // help.
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
                        break;
                    };
                    // JPEG decoding is a few milliseconds of CPU work; doing it
                    // on the UI thread would show up as dropped interactions at
                    // 30fps.
                    let decoded = cx
                        .background_executor()
                        .spawn({
                            let bytes = frame.bytes.clone();
                            async move { decode_frame(&bytes) }
                        })
                        .await;
                    let Ok(decoded) = decoded else {
                        continue;
                    };
                    let alive = this.update(cx, |surface, cx| {
                        surface.accept_frame(frame, decoded);
                        cx.notify();
                    });
                    if alive.is_err() {
                        return;
                    }
                }
                // The stream ended. Say so, then try once more unless the
                // surface is gone.
                let alive = this.update(cx, |surface, cx| {
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
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    fn accept_frame(&mut self, frame: BrowserFrame, image: DecodedFrame) {
        // Park the outgoing texture: `Window::drop_image` needs a window, which
        // only `render` has.
        if let Some(previous) = self.frame_image.take() {
            self.pending_drop.push(previous);
        }
        self.frame_image = Some(Arc::new(RenderImage::new(vec![Frame::new(image.image)])));
        self.frame_pixel_size = (image.width as f32, image.height as f32);
        self.frame_metadata = frame.metadata;
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
        // CDP lays out the page in CSS pixels. The encoder's physical-pixel
        // budget belongs to the runtime; clipping either viewport dimension
        // here changes its aspect ratio on tall, wide and HiDPI panels.
        let key = (
            logical_width,
            logical_height,
            (scale_factor * 1000.0) as u32,
        );
        if self.requested_viewport == Some(key) {
            return;
        }
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.requested_viewport = Some(key);
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
        // Encoded frames may be downsampled or rendered at a higher DPI. Their
        // pixel dimensions are not the page's input coordinate system.
        let (device_width, device_height) =
            if self.frame_metadata.device_width > 0.0 && self.frame_metadata.device_height > 0.0 {
                (
                    self.frame_metadata.device_width as f32,
                    self.frame_metadata.device_height as f32,
                )
            } else if self.frame_pixel_size.0 > 0.0 {
                self.frame_pixel_size
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
        // Find is the panel's: headless Chrome has no find bar of its own, so
        // forwarding the shortcut would do nothing at all.
        if is_find_shortcut(&event.keystroke) {
            self.open_find(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.find_open {
            self.close_find(cx);
            cx.stop_propagation();
            return;
        }
        // The browser's own keys are answered here: a page never sees F5,
        // Ctrl+R or Ctrl+L, and headless Chrome has nowhere to reload or focus.
        if browser_command(&event.keystroke) {
            match event.keystroke.key.as_str() {
                "f5" | "r" | "R" => self.reload(cx),
                "l" | "L" => self.focus_address_bar(window, cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        // Copy, cut and paste belong to the panel: headless Chrome has its own
        // clipboard, so forwarding the shortcut would copy into a buffer the
        // human can never reach.
        match clipboard_command(&event.keystroke) {
            Some(ClipboardCommand::Copy) => {
                self.copy_selection(cx);
                cx.stop_propagation();
                return;
            }
            Some(ClipboardCommand::Cut) => {
                self.cut_selection(&event.keystroke, cx);
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

    /// Asks the page what cursor the hovered element wants.
    ///
    /// Throttled rather than run per motion event: a pointer crossing the panel
    /// fires dozens of moves, and one probe per round trip is already finer than
    /// the eye. The previous cursor stays until an answer arrives, so a moving
    /// pointer does not flicker.
    fn probe_cursor(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        const CURSOR_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(120);
        if self.cursor_probe_in_flight {
            return;
        }
        if self
            .cursor_probed_at
            .is_some_and(|at| at.elapsed() < CURSOR_PROBE_INTERVAL)
        {
            return;
        }
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.cursor_probe_in_flight = true;
        self.cursor_probed_at = Some(std::time::Instant::now());
        cx.spawn(async move |this, cx| {
            let cursor = transport.cursor_at(&tab_id, x, y).await.ok();
            let _ = this.update(cx, |surface, cx| {
                surface.cursor_probe_in_flight = false;
                let Some(cursor) = cursor else {
                    return;
                };
                let style = cursor_style_for(&cursor);
                if surface.hover_cursor != style {
                    surface.hover_cursor = style;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks the page whether a `<select>` sits under the pointer.
    ///
    /// Fire and forget, and only one probe at a time: the answer is a hint for
    /// the next click, so a late one is simply dropped.
    fn probe_select_hint(&mut self, x: f64, y: f64, cx: &mut Context<Self>) {
        if self.select_probe_in_flight {
            return;
        }
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.select_probe_in_flight = true;
        cx.spawn(async move |this, cx| {
            let menu = transport.select_menu_at(&tab_id, x, y).await.ok().flatten();
            let _ = this.update(cx, |surface, cx| {
                surface.select_probe_in_flight = false;
                let next = menu.map(|menu| SelectHint { x, y, menu });
                // A hover that found nothing clears a hint that no longer holds.
                if surface.select_hint.is_some() || next.is_some() {
                    surface.select_hint = next;
                }
                let _ = cx;
            });
        })
        .detach();
    }

    /// Applies an option the human picked from the fallback menu.
    fn choose_select_option(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(menu) = self.select_menu.take() else {
            return;
        };
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.select_hint = None;
        cx.background_executor()
            .spawn(async move {
                let _ = transport
                    .choose_select_option(&tab_id, menu.index, &value)
                    .await;
            })
            .detach();
        cx.notify();
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

    /// Cuts the page's selection into the system clipboard.
    ///
    /// The page performs the cut — that is the only way its own edit history
    /// stays consistent — but it writes Chrome's clipboard, which the human
    /// cannot paste from. The selection is read into the system clipboard
    /// first, so the text is not lost between the two.
    fn cut_selection(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        let keystroke = keystroke.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(text) = transport.selection_text(&tab_id).await
                && !text.is_empty()
            {
                let _ = this.update(cx, |_surface, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                });
            }
            let _ = this.update(cx, |surface, cx| {
                if let Some(input) = key_input(&keystroke, "rawKeyDown") {
                    surface.dispatch(input, cx);
                }
            });
        })
        .detach();
    }

    /// Puts the caret in the address bar, the way Ctrl+L does in a browser.
    fn focus_address_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.address_input.read(cx).focus_handle(cx), cx);
        cx.notify();
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
        let url = normalize_address(&trimmed, &self.search_template);
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

    /// Opens the find bar and puts the caret in it.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = true;
        window.focus(&self.find_input.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Whether this panel holds the keyboard — the page, or a field of its own.
    ///
    /// The session's find shortcut is claimed before focus is consulted, so the
    /// panel has to be able to say that the chord belongs to it: with the caret
    /// in the page or in one of the toolbar fields, Ctrl+F is the page's find.
    pub(crate) fn owns_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focus.is_focused(window)
            || self
                .address_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self.find_input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Closes the find bar and takes its highlights off the page.
    ///
    /// The page is the one that has to forget: an attribute and an injected
    /// style sheet outlive the panel's own state otherwise.
    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if !self.find_open {
            return;
        }
        self.find_open = false;
        self.find_total = 0;
        self.find_current = 0;
        self.find_pending = false;
        if let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) {
            cx.background_executor()
                .spawn(async move { transport.clear_find_in_page(&tab_id).await })
                .detach();
        }
        cx.notify();
    }

    /// Searches the page for what the find bar holds.
    ///
    /// `restart` is a fresh search of the typed text; without it this is a step
    /// to the next (or previous) match. `backward` is Shift+Enter.
    fn run_find(&mut self, backward: bool, restart: bool, cx: &mut Context<Self>) {
        let query = self.find_input.read(cx).value().to_string();
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        if restart && query.is_empty() {
            // An emptied field is a cleared search, not a search for "".
            self.find_total = 0;
            self.find_current = 0;
            if let Some(tab_id) = self.tab_id.clone() {
                cx.background_executor()
                    .spawn(async move { transport.clear_find_in_page(&tab_id).await })
                    .detach();
            }
            let _ = tab_id;
            cx.notify();
            return;
        }
        self.find_pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = transport.find_in_page(&tab_id, &query, !backward).await;
            let _ = this.update(cx, |surface, cx| {
                surface.find_pending = false;
                match result {
                    Ok((total, current)) => {
                        surface.find_total = total;
                        surface.find_current = current;
                    }
                    Err(error) => {
                        surface.find_total = 0;
                        surface.find_current = 0;
                        surface.message = Some(error.message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The find bar, drawn under the toolbar.
    fn render_find_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.find_open {
            return None;
        }
        let counter = if self.find_pending {
            "…".to_string()
        } else if self.find_total == 0 {
            locale::text("No matches", "无匹配", "無相符").to_string()
        } else {
            format!("{} / {}", self.find_current, self.find_total)
        };
        Some(
            h_flex()
                .id("browser-find")
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
                        .child(Input::new(&self.find_input).small()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(counter),
                )
                .child(
                    Button::new("browser-find-previous")
                        .icon(Icon::new(IconName::ArrowUp))
                        .ghost()
                        .xsmall()
                        .tooltip(locale::text("Previous match", "上一个匹配", "上一個相符"))
                        .on_click(cx.listener(|this, _, _, cx| this.run_find(true, false, cx))),
                )
                .child(
                    Button::new("browser-find-next")
                        .icon(Icon::new(IconName::ArrowDown))
                        .ghost()
                        .xsmall()
                        .tooltip(locale::text("Next match", "下一个匹配", "下一個相符"))
                        .on_click(cx.listener(|this, _, _, cx| this.run_find(false, false, cx))),
                )
                .child(
                    Button::new("browser-find-close")
                        .icon(Icon::new(IconName::Close))
                        .ghost()
                        .xsmall()
                        .tooltip(locale::text("Close find", "关闭查找", "關閉尋找"))
                        .on_click(cx.listener(|this, _, _, cx| this.close_find(cx))),
                )
                .into_any_element(),
        )
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

    fn go_back(&mut self, cx: &mut Context<Self>) {
        self.navigate_history(false, cx);
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        self.navigate_history(true, cx);
    }

    /// Moves the tab through its history. The mouse's side buttons take the
    /// same path as the toolbar arrows.
    fn navigate_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (Some(transport), Some(tab_id)) = (self.transport.clone(), self.tab_id.clone()) else {
            return;
        };
        self.start_frame_pump(cx);
        cx.background_executor()
            .spawn(async move {
                let _ = if forward {
                    transport.go_forward(&tab_id).await
                } else {
                    transport.go_back(&tab_id).await
                };
            })
            .detach();
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

    /// Says what the panel is waiting for while it has no runtime tab yet.
    ///
    /// A surface restored from a saved layout spends a moment here while the
    /// runtime reopens the page, and "waiting for the browser to start" is the
    /// wrong sentence for that. A retry after a refusal has to clear that
    /// refusal too, or the panel would keep showing it while the browser starts.
    pub fn set_pending_message(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if self.tab_id.is_some() {
            return;
        }
        self.phase = SurfacePhase::Idle;
        self.message = Some(message.into());
        cx.notify();
    }

    /// Explains why no runtime tab stands behind this panel.
    ///
    /// A restored tab whose runtime cannot serve it must say so; the idle
    /// placeholder would leave it looking like a browser that never starts.
    pub fn set_unattached_reason(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if self.tab_id.is_some() {
            return;
        }
        self.phase = SurfacePhase::Unavailable;
        self.message = Some(message.into());
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

    /// Hands the page the file the reader chose for its file chooser.
    pub(crate) fn upload_file(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tab_id.clone() else {
            return;
        };
        if !self.file_chooser_pending {
            return;
        }
        self.file_chooser_pending = false;
        let Some(local) = self.local_browser() else {
            cx.notify();
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let _ = local
                    .run(local.service().resolve_file_chooser(&tab_id, &[path]))
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

    /// Pauses the Agent's page actions on this tab.
    ///
    /// The only human gesture that stops the Agent: touching the page does not,
    /// so the reader says so instead. The runtime refuses the Agent's next page
    /// action with `browser_operation_aborted` while it is paused.
    fn pause_agent(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tab_id.clone() else {
            return;
        };
        let Some(local) = self.local_browser() else {
            cx.notify();
            return;
        };
        self.agent_paused = true;
        cx.background_executor()
            .spawn(async move {
                let _ = local
                    .run(local.service().pause_agent_operations(&tab_id))
                    .await;
            })
            .detach();
        cx.notify();
    }

    /// Whether the Agent is driving this tab right now.
    ///
    /// The runtime is the authority: `execution_source` is read from the session
    /// snapshot, and a paused Agent reports the human as the source even though
    /// it will take the tab back the moment the reader steps away.
    pub(crate) fn agent_driving(&self) -> bool {
        self.execution_source == Some(BrowserExecutionSource::Agent) && !self.agent_paused
    }

    /// Whether the panel can offer the Agent's play/pause control at all.
    ///
    /// A paired runtime owns the Agent on the other machine; only the
    /// in-process service can pause or resume it.
    fn can_control_agent(&self) -> bool {
        self.local_browser().is_some() && (self.agent_paused || self.agent_driving())
    }

    /// Reflects a file chooser the page opened.
    pub fn show_file_chooser(&mut self, tab_id: &BrowserTabId, cx: &mut Context<Self>) {
        if self.tab_id.as_ref() != Some(tab_id) {
            return;
        }
        self.file_chooser_pending = true;
        cx.notify();
    }

    /// Reflects a download the runtime announced for this tab.
    ///
    /// The same guid updates one row as it moves, so progress does not grow the
    /// list one event at a time; the most recent few stay visible.
    pub fn show_download(
        &mut self,
        download: vibex_browser::BrowserDownload,
        cx: &mut Context<Self>,
    ) {
        if self.tab_id.as_ref() != Some(&download.tab_id) {
            return;
        }
        const MAX_DOWNLOADS_SHOWN: usize = 4;
        match self
            .downloads
            .iter_mut()
            .find(|shown| shown.guid == download.guid)
        {
            Some(shown) => *shown = download,
            None => self.downloads.push(download),
        }
        if self.downloads.len() > MAX_DOWNLOADS_SHOWN {
            let excess = self.downloads.len() - MAX_DOWNLOADS_SHOWN;
            self.downloads.drain(..excess);
        }
        self.schedule_downloads_hide(cx);
        cx.notify();
    }

    /// Takes the download card off the screen once nothing is still arriving.
    ///
    /// Chrome's own bubble disappears the same way: a progress bar is worth
    /// watching, and a list of files saved an hour ago is not worth owning a
    /// corner of the page.
    fn schedule_downloads_hide(&mut self, cx: &mut Context<Self>) {
        const DOWNLOAD_NOTICE_LINGER: Duration = Duration::from_secs(12);
        self.downloads_hide_task = None;
        if self.downloads.is_empty()
            || self
                .downloads
                .iter()
                .any(|download| download.state == vibex_browser::BrowserDownloadState::InProgress)
        {
            return;
        }
        self.downloads_hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DOWNLOAD_NOTICE_LINGER).await;
            let _ = this.update(cx, |surface, cx| {
                surface.downloads_hide_task = None;
                // Only what has finished goes: a download that started while
                // the timer ran is still worth its progress bar.
                surface.downloads.retain(|download| {
                    download.state == vibex_browser::BrowserDownloadState::InProgress
                });
                cx.notify();
            });
        }));
    }

    /// The download popup: what is arriving, and where it went.
    fn render_downloads(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.downloads.is_empty() {
            return None;
        }
        // The directory travels with every event, so the folder button works
        // while a download is still running or was refused and no file exists.
        let directory = self
            .downloads
            .last()
            .map(|download| download.directory.clone());
        let mut rows: Vec<AnyElement> = Vec::with_capacity(self.downloads.len());
        for (index, download) in self.downloads.iter().enumerate() {
            rows.push(render_download_row(index, download, cx));
        }
        Some(
            v_flex()
                .id("browser-downloads")
                .absolute()
                .right(px(12.0))
                .bottom(px(if self.can_control_agent() {
                    AGENT_CONTROL_MARGIN * 2.0 + AGENT_CONTROL_SIZE
                } else {
                    12.0
                }))
                .w(px(320.0))
                .gap_1()
                .p_2()
                .rounded_lg()
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
                .shadow_md()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(locale::text("Downloads", "下载", "下載")),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .when_some(directory, |this, directory| {
                                    this.child(
                                        Button::new("browser-downloads-folder")
                                            .icon(Icon::new(IconName::FolderClosed))
                                            .ghost()
                                            .xsmall()
                                            .tooltip(locale::text(
                                                "Open the downloads folder",
                                                "打开下载目录",
                                                "開啟下載資料夾",
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_downloads_folder(directory.clone(), cx);
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("browser-downloads-dismiss")
                                        .icon(Icon::new(IconName::Close))
                                        .ghost()
                                        .xsmall()
                                        .tooltip(locale::text(
                                            "Hide the download list",
                                            "隐藏下载列表",
                                            "隱藏下載列表",
                                        ))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.downloads.clear();
                                            this.downloads_hide_task = None;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// The Agent's play/pause control.
    ///
    /// A page the Agent is driving gets one obvious way to stop it: touching
    /// the page does not pause anything any more, so the reader needs a control
    /// that says so. The bloom behind the button is what makes it read as the
    /// Agent being live rather than as another toolbar button, and it is drawn
    /// from two blurred shadows so it fades outwards instead of ending on a
    /// hard ring.
    fn render_agent_control(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A page dialog or a file chooser is a modal moment; the control is
        // about the page behind it and would only float over the card.
        if !self.can_control_agent() || self.dialog.is_some() || self.file_chooser_pending {
            return None;
        }
        let paused = self.agent_paused;
        // Green means "the Agent is working"; once paused the button turns to
        // the accent that hands the page back.
        let accent = if paused {
            cx.theme().success
        } else {
            cx.theme().primary
        };
        let (icon, tooltip) = if paused {
            (
                IconName::Play,
                locale::text(
                    "Hand control back to the Agent",
                    "把控制权交还给 Agent",
                    "把控制權交還給 Agent",
                ),
            )
        } else {
            (
                IconName::Pause,
                locale::text(
                    "Pause the Agent on this tab",
                    "暂停 Agent 在此标签页的操作",
                    "暫停 Agent 在此分頁的操作",
                ),
            )
        };
        let label = if paused {
            locale::text("Agent paused", "Agent 已暂停", "Agent 已暫停")
        } else {
            locale::text("Agent is driving", "Agent 正在操作", "Agent 正在操作")
        };
        let button = div()
            .id("browser-agent-control")
            .size(px(AGENT_CONTROL_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(cx.theme().background)
            .border_1()
            .border_color(accent.opacity(0.6))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label(label)
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .child(Icon::new(icon).size(px(26.0)).text_color(accent))
            .on_click(cx.listener(move |this, _, _, cx| {
                if paused {
                    this.hand_back_to_agent(cx);
                } else {
                    this.pause_agent(cx);
                }
            }));
        Some(
            div()
                .absolute()
                .right(px(AGENT_CONTROL_MARGIN))
                .bottom(px(AGENT_CONTROL_MARGIN))
                .child(
                    button.with_animation(
                        "browser-agent-control-bloom",
                        Animation::new(Duration::from_millis(2_200))
                            .with_max_fps(30.0)
                            .repeat(),
                        move |this, delta| {
                            // The bloom breathes outward: the halo grows and
                            // fades over one cycle, and the base shadow keeps a
                            // soft edge between frames.
                            let spread = 1.0 + delta * 9.0;
                            let blur = 12.0 + delta * 20.0;
                            let alpha = 0.5 - delta * 0.32;
                            this.shadow(vec![
                                BoxShadow::new(px(0.0), px(0.0), accent.opacity(alpha))
                                    .blur_radius(px(blur))
                                    .spread_radius(px(spread)),
                                BoxShadow::new(px(0.0), px(0.0), accent.opacity(0.18))
                                    .blur_radius(px(28.0))
                                    .spread_radius(px(2.0)),
                            ])
                        },
                    ),
                )
                .into_any_element(),
        )
    }

    /// Opens the runtime's download directory in the system file manager.
    fn open_downloads_folder(&mut self, directory: std::path::PathBuf, cx: &mut Context<Self>) {
        // The directory is created on demand: a reader whose every download was
        // refused has none yet, and asking to see it is asking for it to exist.
        let opened = std::fs::create_dir_all(&directory)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                crate::platform::reveal_path_in_file_manager(&directory)
                    .map_err(|error| error.message)
            });
        match opened {
            Ok(()) => self.source_notice = None,
            Err(error) => {
                self.source_notice = Some(format!(
                    "{}{error}",
                    locale::text(
                        "Could not open the downloads folder: ",
                        "无法打开下载目录：",
                        "無法開啟下載資料夾：",
                    )
                ));
            }
        }
        cx.notify();
    }

    fn render_frame(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let focus = self.focus.clone();
        let input_entity = cx.entity();
        let menu_entity = cx.weak_entity();
        let can_go_back = self.tab.as_ref().is_some_and(|tab| tab.can_go_back);
        let can_go_forward = self.tab.as_ref().is_some_and(|tab| tab.can_go_forward);
        let prepaint_entity = input_entity.clone();
        let prepaint_hitbox = self.frame_hitbox.clone();
        let frame_hitbox = self.frame_hitbox.clone();
        let hover_cursor = self.hover_cursor;
        let active = self.active;
        let has_frame = self.frame_image.is_some();
        let phase_message = self.phase_message();
        // A stalled or failed pump used to be invisible: the last frame stayed
        // on screen, so a frozen page looked like a live one that ignored the
        // pointer. Say it out loud instead.
        let stalled =
            has_frame && matches!(self.phase, SurfacePhase::Failed | SurfacePhase::Unavailable);
        let image = self.frame_image.clone();
        div()
            .id("browser-frame")
            .relative()
            .size_full()
            .overflow_hidden()
            .when_some(image, |this, image| {
                // The viewport follows this panel's aspect ratio. Fill also
                // covers encoder rounding and the old frame during the resize
                // debounce, so the painted area stays aligned with input.
                this.child(
                    img(image)
                        .w_full()
                        .h_full()
                        .object_fit(gpui::ObjectFit::Fill),
                )
            })
            .when(!has_frame || stalled, |this| {
                this.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .when(stalled, |this| this.bg(cx.theme().background.opacity(0.85)))
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
                    // Alt+click asks which file rendered the element instead of
                    // clicking it: the page never sees the click, which is what
                    // makes it safe to use while looking for code.
                    if event.modifiers.alt {
                        this.locate_source(x, y, cx);
                        return;
                    }
                    // An open menu swallows the next click: that is how a popup
                    // closes, and letting it through would also click the page
                    // underneath.
                    if this.select_menu.take().is_some() {
                        cx.notify();
                        return;
                    }
                    let hinted = this
                        .select_hint
                        .as_ref()
                        .filter(|hint| select_hint_matches(hint, x, y))
                        .map(|hint| hint.menu.clone());
                    if let Some(menu) = hinted {
                        this.select_menu = Some(OpenSelectMenu {
                            anchor: event.position,
                            index: menu.index,
                            value: menu.value,
                            options: menu.options,
                        });
                        cx.notify();
                        return;
                    }
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
            // The mouse's side buttons are the same navigation as the toolbar
            // arrows, which is what they do in a real browser.
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.go_back(cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.go_forward(cx)),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                let Some((x, y)) = this.to_viewport_point(event.position) else {
                    return;
                };
                // The held button travels with the move: Chrome only starts a
                // drag — a scrollbar, a text selection, an HTML5 drop — when it
                // knows one is down.
                let buttons = if event.dragging() { 1 } else { 0 };
                this.dispatch(vibex_browser::BrowserInput::MouseMove { x, y, buttons }, cx);
                this.probe_select_hint(x, y, cx);
                this.probe_cursor(x, y, cx);
            }))
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                let Some((x, y)) = this.to_viewport_point(event.position) else {
                    return;
                };
                let (delta_x, delta_y) = wheel_delta_cdp(event.delta, WHEEL_LINE_HEIGHT);
                this.dispatch(
                    vibex_browser::BrowserInput::Wheel {
                        x,
                        y,
                        delta_x,
                        delta_y,
                    },
                    cx,
                );
            }))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        // The frame geometry has to come from this canvas, not
                        // from `ElementExt::on_prepaint`: that helper adds an
                        // absolutely positioned `size_full` child, which GPUI
                        // lays out after the in-flow content, so its origin is
                        // the *bottom* of the frame area. Every pointer position
                        // then subtracted that offset and fell outside the
                        // bounds, and the panel silently dropped all mouse
                        // input. This canvas is `inset_0` of the frame, so its
                        // own bounds are the frame's.
                        //
                        // The window's scale factor travels with the size: the
                        // page has to render at the density it is displayed at,
                        // or a HiDPI panel shows a page laid out at 1x.
                        let scale_factor = window.scale_factor();
                        // The hitbox is what the paint below attaches the
                        // page's cursor to; it has to be the frame's own
                        // bounds, or the cursor would change over the toolbar.
                        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                        *prepaint_hitbox.borrow_mut() = Some(hitbox);
                        prepaint_entity.update(cx, |this, cx| {
                            this.frame_bounds = Some(bounds);
                            this.schedule_viewport(
                                f32::from(bounds.size.width),
                                f32::from(bounds.size.height),
                                scale_factor,
                                cx,
                            );
                        });
                    },
                    move |bounds, _, window, cx| {
                        if let Some(hitbox) = frame_hitbox.borrow().as_ref() {
                            window.set_cursor_style(hover_cursor, hitbox);
                        }
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
            .context_menu(move |menu, _, _cx| {
                let back = menu_entity.clone();
                let forward = menu_entity.clone();
                let reload = menu_entity.clone();
                let copy = menu_entity.clone();
                let paste = menu_entity.clone();
                let select_all = menu_entity.clone();
                menu.min_w(px(208.0))
                    .max_w(px(208.0))
                    .item(
                        PopupMenuItem::new(locale::text("Back", "后退", "上一頁"))
                            .icon(IconName::ArrowLeft)
                            .disabled(!can_go_back)
                            .on_click(move |_, _, cx| {
                                let _ = back.update(cx, |this, cx| this.go_back(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Forward", "前进", "下一頁"))
                            .icon(IconName::ArrowRight)
                            .disabled(!can_go_forward)
                            .on_click(move |_, _, cx| {
                                let _ = forward.update(cx, |this, cx| this.go_forward(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Reload", "重新加载", "重新載入"))
                            .icon(IconName::RotateCw)
                            .on_click(move |_, _, cx| {
                                let _ = reload.update(cx, |this, cx| this.reload(cx));
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(locale::text("Copy", "复制", "複製"))
                            .icon(IconName::Copy)
                            .on_click(move |_, _, cx| {
                                let _ = copy.update(cx, |this, cx| this.copy_selection(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Paste", "粘贴", "貼上")).on_click(
                            move |_, _, cx| {
                                let _ = paste.update(cx, |this, cx| this.paste_clipboard(cx));
                            },
                        ),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Select all", "全选", "全選")).on_click(
                            move |_, _, cx| {
                                let _ = select_all.update(cx, |this, cx| this.select_all(cx));
                            },
                        ),
                    )
            })
            .into_any_element()
    }

    /// Selects everything in the page, the way the context menu's entry does.
    fn select_all(&mut self, cx: &mut Context<Self>) {
        for event_type in ["rawKeyDown", "keyUp"] {
            self.dispatch(
                BrowserInput::Key {
                    event_type: event_type.to_string(),
                    key: "a".to_string(),
                    code: "KeyA".to_string(),
                    text: None,
                    // CDP modifier bits: Control.
                    modifiers: 2,
                    windows_key_code: 65,
                },
                cx,
            );
        }
    }

    fn phase_message(&self) -> SharedString {
        match self.phase {
            SurfacePhase::Idle => self
                .message
                .clone()
                .unwrap_or_else(|| {
                    locale::text(
                        "Waiting for the browser to start.",
                        "正在等待浏览器启动。",
                        "正在等待瀏覽器啟動。",
                    )
                    .to_string()
                })
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
        // Chrome's three buttons: back and forward follow the tab's own history,
        // reload is always available.
        let can_go_back = self.tab.as_ref().is_some_and(|tab| tab.can_go_back);
        let can_go_forward = self.tab.as_ref().is_some_and(|tab| tab.can_go_forward);
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
                Button::new("browser-back")
                    .icon(Icon::new(IconName::ArrowLeft))
                    .ghost()
                    .xsmall()
                    .disabled(!can_go_back)
                    .tooltip(locale::text("Back", "后退", "上一頁"))
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                Button::new("browser-forward")
                    .icon(Icon::new(IconName::ArrowRight))
                    .ghost()
                    .xsmall()
                    .disabled(!can_go_forward)
                    .tooltip(locale::text("Forward", "前进", "下一頁"))
                    .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            )
            .child(
                Button::new("browser-reload")
                    .icon(Icon::new(IconName::RotateCw))
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
            .child(
                Button::new("browser-activity")
                    .icon(Icon::new(IconName::LayoutDashboard))
                    .ghost()
                    .xsmall()
                    .tooltip(locale::text(
                        "Show what the Agent did in this tab",
                        "查看 Agent 在此标签页的操作记录",
                        "查看 Agent 在此分頁的操作記錄",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_ledger(cx))),
            )
            .child({
                // The stream is JPEG 80 by default; text-heavy pages are the
                // reason the toggle exists. The label is the mode in force, so
                // the button says what the picture is doing rather than what
                // pressing it would do.
                let high = self.capture_quality == BrowserCaptureQuality::High;
                Button::new("browser-quality")
                    .label(if high { "HD" } else { "SD" })
                    .ghost()
                    .xsmall()
                    .toggled(high)
                    .tooltip(locale::text(
                        "Lossless PNG frames; larger and slower than the default",
                        "无损 PNG 画面；比默认更清晰，但更大更慢",
                        "無損 PNG 畫面；比預設更清晰，但更大更慢",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = if this.capture_quality == BrowserCaptureQuality::High {
                            BrowserCaptureQuality::Standard
                        } else {
                            BrowserCaptureQuality::High
                        };
                        this.set_capture_quality(next, cx);
                        cx.emit(BrowserSurfaceEvent::CaptureQualityChanged(next));
                    }))
            })
            .into_any_element()
    }

    /// The recording banner.
    ///
    /// An exported test needs the values typed into the page, so recording keeps
    /// them in memory while the audit ledger stays redacted. That trade is the
    /// human's to make, which means the panel has to say it is happening.
    fn render_recording(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The flag is the runtime's answer from the session snapshot, so it is
        // already false until a snapshot says otherwise.
        self.recording.then_some(())?;
        Some(
            h_flex()
                .id("browser-recording")
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
                            "Recording: values typed into this page are kept in memory so the \
                             exported test is usable. The audit ledger stays redacted.",
                            "录制中：输入到页面的内容会保存在内存中，以便导出可用的测试；审计记录仍然脱敏。",
                            "錄製中：輸入到頁面的內容會保存在記憶體中，以便匯出可用的測試；審計記錄仍然脫敏。",
                        )),
                )
                .into_any_element(),
        )
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
                            "The Agent's page actions are paused on this tab; it can still \
                             observe. Use the play button, or hand the tab back, when you are \
                             done.",
                            "Agent 在此标签页的页面操作已暂停（仍可观察）。完成后点击播放按钮或交还给 Agent 即可继续。",
                            "Agent 在此分頁的頁面操作已暫停（仍可觀察）。完成後點擊播放按鈕或交還給 Agent 即可繼續。",
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

    /// Whether the recording banner would render, for the test that pins it.
    #[cfg(test)]
    pub(crate) fn render_recording_banner_for_test(&self) -> bool {
        self.recording
    }

    /// Whether the find bar is open, for tests.
    #[cfg(test)]
    pub(crate) fn find_bar_open(&self) -> bool {
        self.find_open
    }

    /// Why an Alt+click could not be mapped, said where the human clicked.
    fn render_source_notice(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.source_notice.clone()?;
        Some(
            h_flex()
                .id("browser-source-notice")
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
                        .child(notice),
                )
                .child(
                    Button::new("browser-source-notice-close")
                        .label(locale::text("Dismiss", "忽略", "忽略"))
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.source_notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The docked activity list.
    ///
    /// It shows the same redacted ledger the runtime persists, so a human can
    /// answer "what did it just do?" without having watched the run.
    fn render_ledger(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.ledger_open {
            return None;
        }
        let now = unix_timestamp_ms();
        let mut rows = Vec::new();
        for record in self.ledger.iter().rev().take(40) {
            rows.push(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .w(px(64.0))
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(relative_time(record.at_ms, now)),
                    )
                    .child(
                        div()
                            .w(px(72.0))
                            .flex_none()
                            .text_xs()
                            .text_color(match record.status {
                                BrowserOperationStatus::Failed => cx.theme().danger,
                                _ => cx.theme().muted_foreground,
                            })
                            .child(activity_kind(record.kind)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().foreground)
                            .child(record.summary.clone()),
                    )
                    .when(
                        record.execution_source == BrowserExecutionSource::User,
                        |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(locale::text("you", "你", "你")),
                            )
                        },
                    )
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.ledger_pending {
                        locale::text("Loading…", "加载中…", "載入中…")
                    } else {
                        locale::text(
                            "Nothing recorded in this tab yet.",
                            "此标签页还没有操作记录。",
                            "此分頁還沒有操作記錄。",
                        )
                    })
                    .into_any_element(),
            );
        }
        if let Some(error) = self.ledger_error.clone() {
            rows.push(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error)
                    .into_any_element(),
            );
        }
        Some(
            v_flex()
                .id("browser-activity")
                .flex_none()
                .w_full()
                .max_h(px(180.0))
                .overflow_y_scroll()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(locale::text(
                                    "Agent activity in this tab (redacted)",
                                    "Agent 在此标签页的操作（已脱敏）",
                                    "Agent 在此分頁的操作（已脫敏）",
                                )),
                        )
                        .child(
                            Button::new("browser-activity-refresh")
                                .label(locale::text("Refresh", "刷新", "重新整理"))
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_ledger(cx))),
                        )
                        .child(
                            Button::new("browser-activity-close")
                                .label(locale::text("Close", "关闭", "關閉"))
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_ledger(cx))),
                        ),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// The fallback menu for a page `<select>`.
    ///
    /// Chrome draws the real popup in browser UI, which a screencast never
    /// carries: without this the click opened nothing the human could see.
    fn render_select_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.select_menu.as_ref()?;
        let selected = menu.value.clone();
        let mut list = v_flex()
            .id("browser-select-menu")
            .absolute()
            .left(menu.anchor.x)
            .top(menu.anchor.y)
            .w(px(260.0))
            .max_h(px(280.0))
            .overflow_y_scroll()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_md()
            .occlude();
        for option in menu.options.iter() {
            let value = option.value.clone();
            let is_selected = option.value == selected;
            list = list.child(
                div()
                    .id(SharedString::from(format!(
                        "browser-select-option-{}",
                        option.value
                    )))
                    .px_2()
                    .py_1()
                    .text_sm()
                    .truncate()
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(cx.theme().accent))
                    .hover(|this| this.bg(cx.theme().muted))
                    .child(option.label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_select_option(value.clone(), cx);
                    })),
            );
        }
        Some(list.into_any_element())
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
        // A paired runtime's browser has no local file picker this panel could
        // feed, so only the cancel affordance is offered there.
        let can_choose = self.local_browser().is_some();
        let tab_id = self.tab_id.clone();
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
                                    "A headless browser has no native file dialog. Choose the file \
                                     to attach it to the page, or cancel the request.",
                                    "无头浏览器没有原生文件对话框。选择文件后会附加到页面，也可以取消该请求。",
                                    "無頭瀏覽器沒有原生檔案對話框。選擇檔案後會附加到頁面，也可以取消該請求。",
                                )),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("browser-file-chooser-dismiss")
                                        .label(locale::text(
                                            "Cancel request",
                                            "取消请求",
                                            "取消請求",
                                        ))
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_file_chooser(cx);
                                        })),
                                )
                                .when(can_choose, |this| {
                                    this.child(
                                        Button::new("browser-file-chooser-pick")
                                            .label(locale::text(
                                                "Choose file",
                                                "选择文件",
                                                "選擇檔案",
                                            ))
                                            .small()
                                            .on_click(cx.listener(
                                                move |_, _, _, cx| {
                                                    let Some(tab_id) = tab_id.clone() else {
                                                        return;
                                                    };
                                                    cx.emit(
                                                        BrowserSurfaceEvent::FileChooserPickRequested {
                                                            tab_id,
                                                        },
                                                    );
                                                },
                                            )),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// A byte count the way a download list reads it.
///
/// Chrome reports exact bytes, and the reader wants the size, not the count:
/// `3.4 MB` is an answer, `3565158 B` is a puzzle.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// One row of the download popup.
///
/// The name is what the reader asked for; under it the row says where the
/// download stands — a bar while Chrome writes, the saved path once it is
/// done, or why nothing was written at all.
fn render_download_row(
    index: usize,
    download: &vibex_browser::BrowserDownload,
    cx: &App,
) -> AnyElement {
    use vibex_browser::BrowserDownloadState;
    let (icon, tone) = match download.state {
        BrowserDownloadState::InProgress => (IconName::File, cx.theme().muted_foreground),
        BrowserDownloadState::Completed => (IconName::CircleCheck, cx.theme().success),
        BrowserDownloadState::Canceled => (IconName::CircleX, cx.theme().muted_foreground),
        BrowserDownloadState::Blocked => (IconName::TriangleAlert, cx.theme().warning),
    };
    let detail: AnyElement = match download.state {
        BrowserDownloadState::InProgress => {
            // Chrome reports the total only once it has one; until then the bar
            // is busy rather than pretending the download is at zero.
            let known_total = download.total_bytes > 0;
            let percent = if known_total {
                (download.received_bytes as f32 / download.total_bytes as f32 * 100.0)
                    .clamp(0.0, 100.0)
            } else {
                0.0
            };
            let label = if known_total {
                format!(
                    "{} / {}",
                    format_bytes(download.received_bytes),
                    format_bytes(download.total_bytes)
                )
            } else {
                format_bytes(download.received_bytes)
            };
            v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    Progress::new(format!("browser-download-{index}"))
                        .value(percent)
                        .loading(!known_total)
                        .small(),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .into_any_element()
        }
        BrowserDownloadState::Completed => {
            let saved_to = locale::text("Saved to ", "已保存到 ", "已儲存至 ");
            div()
                .w_full()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(match download.path.as_ref() {
                    Some(path) => format!("{saved_to}{}", path.display()),
                    None => saved_to.to_string(),
                })
                .into_any_element()
        }
        BrowserDownloadState::Canceled => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(locale::text("Canceled", "已取消", "已取消"))
            .into_any_element(),
        BrowserDownloadState::Blocked => div()
            .text_xs()
            .text_color(cx.theme().warning)
            .child(locale::text(
                "Blocked. Turn on “Allow downloads” in Settings to save files.",
                "已被阻止。在设置中开启“允许下载”后才能保存文件。",
                "已被阻止。在設定中開啟「允許下載」後才能儲存檔案。",
            ))
            .into_any_element(),
    };
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap_2()
        .py_1()
        .child(Icon::new(icon).size(px(14.0)).text_color(tone))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(download.file_name.clone()),
                )
                .child(detail),
        )
        .into_any_element()
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
/// anyone. The search goes to `search_template`, the engine the settings chose.
pub fn normalize_address(input: &str, search_template: &str) -> String {
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
    search_url(search_template, trimmed)
}

/// Fills a search engine's `{query}` with a keyword.
///
/// The keyword is form-encoded, which is what a search box needs for spaces,
/// `&` and non-ASCII text. A template that lost its placeholder can only come
/// from a caller that bypassed the settings, so it still searches rather than
/// opening the engine's home page with the keyword dropped.
pub fn search_url(search_template: &str, query: &str) -> String {
    let encoded = url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    if search_template.contains(SEARCH_QUERY_PLACEHOLDER) {
        return search_template.replace(SEARCH_QUERY_PLACEHOLDER, &encoded);
    }
    let separator = if search_template.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{search_template}{separator}q={encoded}")
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
/// stay on the text path below, which is where their characters come from —
/// except Space and Enter, which are named keys that also carry a character.
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
/// A `<select>` found under the pointer, with where it was found.
struct SelectHint {
    x: f64,
    y: f64,
    menu: vibex_browser::BrowserSelectMenu,
}

/// A fallback menu the panel is showing for a page `<select>`.
struct OpenSelectMenu {
    anchor: gpui::Point<Pixels>,
    index: u32,
    value: String,
    options: Vec<vibex_browser::BrowserSelectOption>,
}

/// How close a click has to be to the probed point to trust the hint.
///
/// The page can move between the hover and the click; a stale hint only costs
/// the popup the panel was trying to replace.
const SELECT_HINT_TOLERANCE: f64 = 6.0;

/// How many pixels one wheel line is worth when the platform reports lines.
///
/// Roughly a line of text; Chrome's own wheel handling treats a notch as a few
/// lines, and the panel only has to feel like a browser.
const WHEEL_LINE_HEIGHT: Pixels = px(16.0);

/// Converts a GPUI wheel delta into the convention the DevTools protocol uses.
///
/// The two disagree on sign: GPUI reports a positive `y` when the user scrolls
/// *up* (its own list tests simulate scrolling up with `+100`), while
/// `Input.dispatchMouseEvent` follows the DOM, where a positive `deltaY`
/// scrolls the page *down*. Passing one to the other unchanged inverts the
/// wheel — which is exactly what the panel did.
fn wheel_delta_cdp(delta: gpui::ScrollDelta, line_height: Pixels) -> (f64, f64) {
    let delta = delta.pixel_delta(line_height);
    (-f64::from(delta.x), -f64::from(delta.y))
}

/// True when a click is close enough to a probed point to trust its hint.
fn select_hint_matches(hint: &SelectHint, x: f64, y: f64) -> bool {
    (hint.x - x).abs() <= SELECT_HINT_TOLERANCE && (hint.y - y).abs() <= SELECT_HINT_TOLERANCE
}

/// A clipboard shortcut the panel answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardCommand {
    Copy,
    Cut,
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
        "x" | "X" => Some(ClipboardCommand::Cut),
        "v" | "V" => Some(ClipboardCommand::Paste),
        _ => None,
    }
}

/// Whether a keystroke is a browser command the page must never see.
///
/// A real browser keeps F5, Ctrl+R and Ctrl+L for itself, so a page that binds
/// them never sees them either; forwarding them here would both do nothing and
/// hide the browser's own behaviour.
fn browser_command(keystroke: &Keystroke) -> bool {
    if keystroke.modifiers.alt || keystroke.modifiers.function {
        return false;
    }
    let command = keystroke.modifiers.control || keystroke.modifiers.platform;
    match keystroke.key.as_str() {
        "f5" => true,
        "r" | "R" | "l" | "L" => command && !keystroke.modifiers.shift,
        _ => false,
    }
}

/// Maps a CSS `cursor` keyword onto the native cursor closest to it.
///
/// Several CSS values have no platform equivalent — `zoom-in`, `help`,
/// `wait` — and those fall back to the arrow rather than guessing at something
/// the page did not ask for.
pub fn cursor_style_for(cursor: &str) -> gpui::CursorStyle {
    use gpui::CursorStyle as Style;
    match cursor.trim().to_ascii_lowercase().as_str() {
        "text" => Style::IBeam,
        "vertical-text" => Style::IBeamCursorForVerticalLayout,
        "crosshair" | "cell" => Style::Crosshair,
        "pointer" | "hand" => Style::PointingHand,
        "grab" | "all-scroll" => Style::OpenHand,
        "grabbing" => Style::ClosedHand,
        "not-allowed" | "no-drop" => Style::OperationNotAllowed,
        "col-resize" | "ew-resize" => Style::ResizeLeftRight,
        "row-resize" | "ns-resize" => Style::ResizeUpDown,
        "e-resize" => Style::ResizeRight,
        "w-resize" => Style::ResizeLeft,
        "n-resize" | "up-arrow" => Style::ResizeUp,
        "s-resize" | "down-arrow" => Style::ResizeDown,
        "nesw-resize" => Style::ResizeUpRightDownLeft,
        "nwse-resize" => Style::ResizeUpLeftDownRight,
        "context-menu" => Style::ContextualMenu,
        "alias" => Style::DragLink,
        "copy" => Style::DragCopy,
        _ => Style::Arrow,
    }
}

/// Whether a keystroke is the panel's find shortcut.
///
/// Control on Linux and Windows, Command on macOS. Shift is included because
/// "find previous" is a different key in a browser but the same one here: the
/// bar is opened either way, and the step is decided inside it.
fn is_find_shortcut(keystroke: &Keystroke) -> bool {
    if keystroke.modifiers.alt || keystroke.modifiers.function {
        return false;
    }
    if !(keystroke.modifiers.control || keystroke.modifiers.platform) {
        return false;
    }
    matches!(keystroke.key.as_str(), "f" | "F")
}

/// Maps one GPUI keystroke onto the CDP key event the page receives.
///
/// Named keys and shortcuts whose identity matters travel this path; plain
/// printable characters do not, because their text arrives through
/// `EntityInputHandler` and `Input.insertText`. Sending them here as well would
/// insert every character twice, and would push the raw letters of a CJK
/// composition into the page. This mirrors how the terminal forwards keys.
///
/// Space and Enter are the exception: they are named keys *and* characters. A
/// `rawKeyDown` for either produces no character at all — a space never reached
/// a text field, and Enter never broke a line in a textarea — so both travel as
/// `keyDown` with the text Chrome would have generated.
fn key_input(keystroke: &Keystroke, event_type: &str) -> Option<BrowserInput> {
    let identity = named_page_key(keystroke.key.as_str()).or_else(|| {
        let shortcut =
            keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform;
        shortcut.then(|| character_page_key(keystroke)).flatten()
    })?;
    let text = (event_type == "rawKeyDown")
        .then(|| page_key_text(keystroke))
        .flatten();
    Some(BrowserInput::Key {
        event_type: if text.is_some() {
            "keyDown".to_string()
        } else {
            event_type.to_string()
        },
        key: identity.key,
        code: identity.code.to_string(),
        // A key event with no character keeps `text` empty: the character
        // travels on the text path, and sending it here too would insert it
        // twice.
        text: text.map(str::to_string),
        modifiers: mouse_modifiers(keystroke.modifiers),
        windows_key_code: identity.virtual_key_code,
    })
}

/// The character a named key generates, or `None` when it generates none.
///
/// Control, Alt and Meta suppress the character — Ctrl+Enter is a shortcut, not
/// a newline — and only the keys that produce one carry text.
fn page_key_text(keystroke: &Keystroke) -> Option<&'static str> {
    if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
        return None;
    }
    match keystroke.key.as_str() {
        "space" => Some(" "),
        "enter" => Some("\r"),
        _ => None,
    }
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

impl BrowserSurface {
    /// Points this surface at the queue the workbench drains.
    pub fn set_orphan_textures(&mut self, orphans: OrphanTextures) {
        self.orphans = Some(orphans);
    }

    /// Hands every texture this surface still owns to the workbench.
    ///
    /// Called when the surface is removed from the panel: `Window::drop_image`
    /// is the only release, and the code that closes a tab has no window.
    pub fn retire_textures(&mut self) {
        let mut images = std::mem::take(&mut self.pending_drop);
        if let Some(frame) = self.frame_image.take() {
            images.push(frame);
        }
        if let Some(favicon) = self.favicon.take() {
            images.push(favicon);
        }
        if images.is_empty() {
            return;
        }
        match self.orphans.as_ref() {
            Some(orphans) => orphans.borrow_mut().append(&mut images),
            // Without a queue the images stay parked: dropping them here would
            // leave the atlas tiles behind with no way to release them.
            None => self.pending_drop.append(&mut images),
        }
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
        let find_bar = self.render_find_bar(cx);
        let takeover = self.render_takeover(cx);
        let recording = self.render_recording(cx);
        let source_notice = self.render_source_notice(cx);
        let ledger = self.render_ledger(cx);
        let frame = self.render_frame(cx);
        let select_menu = self.render_select_menu(cx);
        let dialog = self.render_dialog(cx);
        let file_chooser = self.render_file_chooser(cx);
        let downloads = self.render_downloads(cx);
        let agent_control = self.render_agent_control(cx);
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
            .when_some(find_bar, |this, bar| this.child(bar))
            .when_some(takeover, |this, takeover| this.child(takeover))
            .when_some(recording, |this, recording| this.child(recording))
            .when_some(source_notice, |this, notice| this.child(notice))
            .when_some(ledger, |this, ledger| this.child(ledger))
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
            .when_some(select_menu, |this, menu| this.child(menu))
            .when_some(dialog, |this, dialog| this.child(dialog))
            .when_some(file_chooser, |this, chooser| this.child(chooser))
            .when_some(downloads, |this, downloads| this.child(downloads))
            .when_some(agent_control, |this, control| this.child(control))
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

    /// The engine the settings chose, which every case below leans on.
    const GOOGLE: &str = "https://www.google.com/search?q={query}";

    #[test]
    fn a_bare_host_becomes_http() {
        assert_eq!(
            normalize_address("localhost:5173", GOOGLE),
            "http://localhost:5173"
        );
        assert_eq!(
            normalize_address("example.com", GOOGLE),
            "http://example.com"
        );
        assert_eq!(
            normalize_address("127.0.0.1:3000", GOOGLE),
            "http://127.0.0.1:3000"
        );
    }

    #[test]
    fn an_absolute_url_is_left_alone() {
        assert_eq!(
            normalize_address("https://example.com/a?b=1", GOOGLE),
            "https://example.com/a?b=1"
        );
        assert_eq!(normalize_address("about:blank", GOOGLE), "about:blank");
    }

    #[test]
    fn a_bare_word_becomes_a_search_on_the_configured_engine() {
        let url = normalize_address("hello world", GOOGLE);
        assert!(url.starts_with("https://www.google.com/search?q="));
        // Query encoding turns the space into `+` or `%20`; both decode to a
        // space on every engine.
        assert!(url.contains("hello+world") || url.contains("hello%20world"));

        // A different engine is one setting away, and the keyword still has to
        // be encoded: an unencoded `&` would become a second parameter.
        let baidu = normalize_address("a&b", "https://www.baidu.com/s?wd={query}");
        assert_eq!(baidu, "https://www.baidu.com/s?wd=a%26b");
    }

    /// A template that lost its placeholder can only reach here from a caller
    /// that bypassed the settings; the keyword must still be searched for.
    #[test]
    fn a_search_template_without_a_placeholder_still_carries_the_keyword() {
        assert_eq!(
            search_url("https://example.com/find", "hello world"),
            "https://example.com/find?q=hello+world"
        );
        assert_eq!(
            search_url("https://example.com/find?lang=en", "hi"),
            "https://example.com/find?lang=en&q=hi"
        );
    }

    #[test]
    fn empty_input_stays_blank() {
        assert_eq!(normalize_address("   ", GOOGLE), "about:blank");
    }

    #[test]
    fn a_wheel_delta_is_flipped_into_the_dom_convention() {
        // Scrolling up: GPUI says +100, the page must be told -100.
        let up = gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(100.0)));
        assert_eq!(wheel_delta_cdp(up, WHEEL_LINE_HEIGHT), (0.0, -100.0));
        // Scrolling down: GPUI says -50, the page must be told +50.
        let down = gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-50.0)));
        assert_eq!(wheel_delta_cdp(down, WHEEL_LINE_HEIGHT), (0.0, 50.0));
        // Horizontal follows the same rule.
        let left = gpui::ScrollDelta::Pixels(gpui::point(px(30.0), px(0.0)));
        assert_eq!(wheel_delta_cdp(left, WHEEL_LINE_HEIGHT), (-30.0, 0.0));
        // Lines are converted before the flip.
        let lines = gpui::ScrollDelta::Lines(gpui::point(0.0, 3.0));
        assert_eq!(wheel_delta_cdp(lines, px(16.0)), (0.0, -48.0));
    }

    #[test]
    fn a_select_hint_only_covers_the_click_it_was_probed_for() {
        let hint = SelectHint {
            x: 100.0,
            y: 50.0,
            menu: vibex_browser::BrowserSelectMenu {
                index: 0,
                value: "a".to_string(),
                options: Vec::new(),
            },
        };
        assert!(select_hint_matches(&hint, 100.0, 50.0));
        assert!(select_hint_matches(
            &hint,
            100.0 + SELECT_HINT_TOLERANCE,
            50.0
        ));
        // A click somewhere else must reach the page, not open a stale menu.
        assert!(!select_hint_matches(
            &hint,
            100.0 + SELECT_HINT_TOLERANCE + 1.0,
            50.0
        ));
        assert!(!select_hint_matches(&hint, 100.0, 200.0));
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
        // Cut is the panel's as well: the page performs the cut, but only after
        // the selection has reached the system clipboard.
        assert_eq!(
            clipboard_command(&keystroke("x", Some("x"), ctrl)),
            Some(ClipboardCommand::Cut)
        );
        // Paste-as-plain-text and AltGr combinations belong to the page.
        assert_eq!(clipboard_command(&keystroke("v", None, ctrl_shift)), None);
        assert_eq!(clipboard_command(&keystroke("v", None, ctrl_alt)), None);
        assert_eq!(clipboard_command(&keystroke("c", Some("c"), none)), None);
        assert_eq!(
            clipboard_command(&keystroke("x", Some("x"), ctrl_shift)),
            None
        );
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
    fn downloads_land_on_their_own_tab_and_update_one_row(cx: &mut gpui::TestAppContext) {
        use vibex_browser::{BrowserDownload, BrowserDownloadState};
        let (surface, mut cx) = test_surface(cx);
        let attached = BrowserTabId::new();
        let other = BrowserTabId::new();
        let download = |tab_id: &BrowserTabId,
                        guid: &str,
                        received: u64,
                        state: BrowserDownloadState| BrowserDownload {
            guid: guid.to_string(),
            tab_id: tab_id.clone(),
            file_name: "report.pdf".to_string(),
            url: "https://example.com/report.pdf".to_string(),
            received_bytes: received,
            total_bytes: 4096,
            state,
            path: None,
            directory: std::path::PathBuf::from("/runtime/downloads"),
        };

        surface.update(&mut cx, |surface, cx| {
            surface.tab_id = Some(attached.clone());
            // Another tab's download must not appear over this page.
            surface.show_download(
                download(&other, "g1", 0, BrowserDownloadState::InProgress),
                cx,
            );
            assert!(surface.downloads.is_empty());

            surface.show_download(
                download(&attached, "g1", 1024, BrowserDownloadState::InProgress),
                cx,
            );
            assert_eq!(surface.downloads.len(), 1);
            // The same guid is one download moving, not a new row per event.
            surface.show_download(
                download(&attached, "g1", 4096, BrowserDownloadState::Completed),
                cx,
            );
            assert_eq!(surface.downloads.len(), 1);
            assert_eq!(surface.downloads[0].state, BrowserDownloadState::Completed);
            assert_eq!(surface.downloads[0].received_bytes, 4096);
            // The folder button opens the directory the runtime named, which
            // every event carries even when no file exists yet.
            assert_eq!(
                surface
                    .downloads
                    .last()
                    .map(|download| download.directory.clone()),
                Some(std::path::PathBuf::from("/runtime/downloads"))
            );

            // A second download is a second row, and the card holds only the
            // most recent few.
            for index in 0..6 {
                surface.show_download(
                    download(
                        &attached,
                        &format!("g{index}"),
                        index,
                        BrowserDownloadState::InProgress,
                    ),
                    cx,
                );
            }
            assert!(surface.downloads.len() <= 4);
            assert_eq!(
                surface
                    .downloads
                    .last()
                    .map(|download| download.guid.as_str()),
                Some("g5")
            );
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

    /// A restored tab with no runtime tab behind it must say why, and must say
    /// what it is doing meanwhile: the idle placeholder is the state the
    /// "waiting for the browser to start" bug got stuck in.
    #[gpui::test]
    fn an_unattached_surface_explains_itself_instead_of_waiting(cx: &mut gpui::TestAppContext) {
        let (surface, mut cx) = test_surface(cx);
        surface.update(&mut cx, |surface, cx| {
            surface.set_pending_message("Reopening the browser tab…", cx);
            assert_eq!(
                surface.phase_message().as_ref(),
                "Reopening the browser tab…"
            );
            assert_eq!(surface.phase, SurfacePhase::Idle);
            assert!(surface.page_url().is_none());

            surface.set_unattached_reason("The embedded browser is unavailable.", cx);
            assert_eq!(surface.phase, SurfacePhase::Unavailable);
            assert_eq!(
                surface.phase_message().as_ref(),
                "The embedded browser is unavailable."
            );

            // A second attempt after a refusal says it is trying again rather
            // than leaving the refusal on screen.
            surface.set_pending_message("Reopening the browser tab…", cx);
            assert_eq!(surface.phase, SurfacePhase::Idle);
            assert_eq!(
                surface.phase_message().as_ref(),
                "Reopening the browser tab…"
            );

            // A surface that already owns a runtime tab keeps rendering that
            // tab: a late boundary must not take over a live page.
            surface.tab_id = Some(BrowserTabId::new());
            surface.set_unattached_reason("late reason", cx);
            assert_eq!(surface.phase, SurfacePhase::Idle);
            assert_eq!(
                surface.phase_message().as_ref(),
                "Reopening the browser tab…",
                "a boundary for a surface that already has a tab is ignored"
            );
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

    fn key_text(input: &vibex_browser::BrowserInput) -> Option<String> {
        let vibex_browser::BrowserInput::Key { text, .. } = input else {
            panic!("not a key input: {input:?}");
        };
        text.clone()
    }

    #[test]
    fn editing_keys_reach_the_page_with_their_virtual_key_codes() {
        let none = gpui::Modifiers::default();
        // Enter carries the newline; a `rawKeyDown` would insert nothing at
        // all, which is how a textarea lost every line break.
        let enter = key_input(&keystroke("enter", None, none), "rawKeyDown").unwrap();
        assert_eq!(
            key_fields(&enter),
            (
                "keyDown".to_string(),
                "Enter".to_string(),
                "Enter".to_string(),
                0,
                13
            )
        );
        assert_eq!(key_text(&enter).as_deref(), Some("\r"));
        let backspace = key_input(&keystroke("backspace", None, none), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&backspace).4, 8);
        assert_eq!(
            key_text(&backspace),
            None,
            "Backspace edits, it types nothing"
        );
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
    fn space_and_enter_carry_the_character_they_generate() {
        let none = gpui::Modifiers::default();
        let space = key_input(&keystroke("space", Some(" "), none), "rawKeyDown").unwrap();
        assert_eq!(
            key_fields(&space),
            (
                "keyDown".to_string(),
                " ".to_string(),
                "Space".to_string(),
                0,
                32
            )
        );
        assert_eq!(key_text(&space).as_deref(), Some(" "));

        // Shift only changes which character a key produces, not whether there
        // is one; the character of Shift+Space is still a space.
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let shifted = key_input(&keystroke("space", Some(" "), shift), "rawKeyDown").unwrap();
        assert_eq!(key_text(&shifted).as_deref(), Some(" "));
        assert_eq!(key_fields(&shifted).0, "keyDown");

        // A modifier makes it a shortcut: the page is told which key was
        // pressed, not that a character was typed.
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        let shortcut = key_input(&keystroke("space", None, control), "rawKeyDown").unwrap();
        assert_eq!(key_fields(&shortcut).0, "rawKeyDown");
        assert_eq!(key_text(&shortcut), None);

        // A key-up never carries text: only the press types.
        let released = key_input(&keystroke("space", Some(" "), none), "keyUp").unwrap();
        assert_eq!(key_fields(&released).0, "keyUp");
        assert_eq!(key_text(&released), None);
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
        // modifiers; Shift+Enter still carries the line break it generates.
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
        assert_eq!(key_text(&newline).as_deref(), Some("\r"));
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
        // Typing "a" or ";" must not produce a key event: the character is
        // committed through the input handler, and sending it here as well
        // would insert it twice (and leak raw keys from a CJK composition).
        // Space and Enter are the two that cannot take that path — their
        // characters are only produced by a `keyDown` carrying text.
        assert!(key_input(&keystroke("a", Some("a"), none), "rawKeyDown").is_none());
        assert!(key_input(&keystroke(";", Some(";"), none), "rawKeyDown").is_none());
        assert!(key_input(&keystroke("中", Some("中"), none), "rawKeyDown").is_none());
    }

    #[test]
    fn the_panel_answers_the_browser_commands_a_page_never_sees() {
        let none = gpui::Modifiers::default();
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        let shift = gpui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert!(browser_command(&keystroke("f5", None, none)));
        assert!(browser_command(&keystroke("r", Some("r"), control)));
        assert!(browser_command(&keystroke("l", Some("l"), control)));
        // A page's own F5 is not a thing: a real browser claims it either way.
        assert!(!browser_command(&keystroke("r", None, shift)));
        assert!(!browser_command(&keystroke("f", Some("f"), control)));
        assert!(!browser_command(&keystroke("r", None, none)));

        // Cut is the panel's too: the page performs it, but the selection has
        // to reach the system clipboard first or the text is lost.
        assert_eq!(
            clipboard_command(&keystroke("x", Some("x"), control)),
            Some(ClipboardCommand::Cut)
        );
        assert_eq!(
            clipboard_command(&keystroke("c", Some("c"), control)),
            Some(ClipboardCommand::Copy)
        );
        assert_eq!(clipboard_command(&keystroke("x", None, shift)), None);
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

    #[gpui::test]
    fn viewport_follows_the_frame_bounds_at_every_display_scale(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport::default());
        let viewports = transport.viewports.clone();
        let window = cx.add_window(|window, cx| {
            let mut surface = BrowserSurface::new("viewport".to_string(), window, cx);
            surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
            surface
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");

        for (width, height, scale) in [
            (900.0, 2100.0, 1.0),
            (3100.0, 900.0, 1.0),
            (800.0, 1300.0, 2.0),
            (1000.0, 1900.0, 1.5),
        ] {
            cx.simulate_window_resize(window.into(), size(px(width), px(height)));
            cx.simulate_scale_factor_change(scale);
            cx.run_until_parked();
            cx.executor().advance_clock(VIEWPORT_DEBOUNCE);
            cx.run_until_parked();
            let bounds = surface
                .read_with(&cx, |surface, _| surface.frame_bounds)
                .expect("the page area is laid out");
            assert_eq!(
                viewports.lock().unwrap().last().copied(),
                Some((
                    f32::from(bounds.size.width).round() as u32,
                    f32::from(bounds.size.height).round() as u32,
                    scale as f64,
                )),
                "the page must receive the whole logical viewport on a {width}x{height} display at {scale}x"
            );
        }
    }

    #[gpui::test]
    fn attaching_after_layout_still_sends_the_viewport(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            BrowserSurface::new("restored-viewport".to_string(), window, cx)
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        cx.run_until_parked();
        let transport = Arc::new(RecordingTransport::default());
        let viewports = transport.viewports.clone();
        surface.update(&mut cx, |surface, cx| {
            surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(VIEWPORT_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(viewports.lock().unwrap().len(), 1);
    }

    #[gpui::test]
    fn pointer_coordinates_follow_metadata_instead_of_encoded_resolution(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            BrowserSurface::new("pointer-viewport".to_string(), window, cx)
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, _| {
            surface.frame_bounds = Some(Bounds::new(
                Point::new(px(10.0), px(20.0)),
                size(px(800.0), px(1200.0)),
            ));
            surface.frame_metadata = BrowserFrameMetadata {
                device_width: 800.0,
                device_height: 1200.0,
                scroll_offset_y: 900.0,
                ..Default::default()
            };
            for frame_size in [(1600.0, 2400.0), (1067.0, 1600.0), (400.0, 600.0)] {
                surface.frame_pixel_size = frame_size;
                assert_eq!(
                    surface.to_viewport_point(Point::new(px(610.0), px(920.0))),
                    Some((600.0, 900.0)),
                    "encoder resolution {frame_size:?} must not move a click"
                );
            }
        });
    }

    // The toolbar's arrows are the same navigation as the picker's side buttons.
    #[gpui::test]
    fn the_toolbar_arrows_reach_the_runtime(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport::default());
        let moves = transport.history_moves.clone();
        let transport: Arc<dyn BrowserTransport> = transport;
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface = BrowserSurface::new("probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface
                })
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, cx| {
            surface.go_back(cx);
            surface.go_forward(cx);
        });
        cx.run_until_parked();
        // The two calls are independent background tasks, so only the set is
        // ordered.
        let recorded = moves.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2);
        assert!(recorded.contains(&false) && recorded.contains(&true));
    }

    /// Records what the surface forwards, so an input-path regression fails
    /// here instead of silently doing nothing in the panel.
    struct RecordingTransport {
        inputs: Arc<std::sync::Mutex<Vec<String>>>,
        viewports: Arc<std::sync::Mutex<Vec<(u32, u32, f64)>>>,
        snapshot: Arc<std::sync::Mutex<Option<vibex_core::BrowserSessionSnapshot>>>,
        /// The ledger the panel reads when the activity list is opened.
        ledger: Arc<std::sync::Mutex<Vec<vibex_core::BrowserActionRecord>>>,
        /// What Alt+click resolves to, when the test configures an answer.
        element_source: Arc<std::sync::Mutex<Option<vibex_core::BrowserElementSource>>>,
        /// The viewport points Alt+click asked about.
        source_calls: Arc<std::sync::Mutex<Vec<(f64, f64)>>>,
        /// The source locations the editor asked the browser to reveal.
        reveals: Arc<std::sync::Mutex<Vec<(String, u32)>>>,
        /// How many times the frame stream was asked for, and how many of the
        /// first attempts fail before one succeeds.
        subscribe_calls: Arc<std::sync::atomic::AtomicUsize>,
        /// The encoder mode each frame subscription asked for, in call order.
        subscribe_qualities: Arc<std::sync::Mutex<Vec<BrowserCaptureQuality>>>,
        subscribe_failures: Arc<std::sync::atomic::AtomicUsize>,
        /// History moves, `true` for forward.
        history_moves: Arc<std::sync::Mutex<Vec<bool>>>,
        /// Find-in-page searches as `(query, forward)`, in call order.
        find_calls: Arc<std::sync::Mutex<Vec<(String, bool)>>>,
        /// What `find_in_page` answers: `(total, current)`.
        find_answer: Arc<std::sync::Mutex<(u32, u32)>>,
        /// How many times the page was asked to drop its highlights.
        find_clears: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Default for RecordingTransport {
        fn default() -> Self {
            Self {
                inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                viewports: Arc::new(std::sync::Mutex::new(Vec::new())),
                snapshot: Arc::new(std::sync::Mutex::new(None)),
                ledger: Arc::new(std::sync::Mutex::new(Vec::new())),
                element_source: Arc::new(std::sync::Mutex::new(None)),
                source_calls: Arc::new(std::sync::Mutex::new(Vec::new())),
                reveals: Arc::new(std::sync::Mutex::new(Vec::new())),
                subscribe_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                subscribe_qualities: Arc::new(std::sync::Mutex::new(Vec::new())),
                subscribe_failures: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                history_moves: Arc::new(std::sync::Mutex::new(Vec::new())),
                find_calls: Arc::new(std::sync::Mutex::new(Vec::new())),
                find_answer: Arc::new(std::sync::Mutex::new((0, 0))),
                find_clears: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
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
        fn ledger(
            &self,
            _session_id: &BrowserSessionId,
        ) -> crate::browser_transport::BrowserTransportFuture<
            '_,
            Vec<vibex_core::BrowserActionRecord>,
        > {
            let ledger = self.ledger.lock().unwrap().clone();
            Box::pin(async move { Ok(ledger) })
        }
        fn set_downloads_enabled(
            &self,
            _enabled: bool,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn cursor_at(
            &self,
            _tab_id: &BrowserTabId,
            _x: f64,
            _y: f64,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, String> {
            Box::pin(async { Ok("auto".to_string()) })
        }
        fn find_in_page(
            &self,
            _tab_id: &BrowserTabId,
            query: &str,
            forward: bool,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, (u32, u32)> {
            self.find_calls
                .lock()
                .unwrap()
                .push((query.to_string(), forward));
            let answer = *self.find_answer.lock().unwrap();
            Box::pin(async move { Ok(answer) })
        }
        fn clear_find_in_page(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            self.find_clears
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn highlight_source(
            &self,
            _tab_id: &BrowserTabId,
            path: &str,
            line: u32,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, vibex_core::SourceElementMatch>
        {
            self.reveals.lock().unwrap().push((path.to_string(), line));
            Box::pin(async {
                Ok(vibex_core::SourceElementMatch {
                    found: true,
                    detail: None,
                })
            })
        }

        fn element_source_at(
            &self,
            _tab_id: &BrowserTabId,
            x: f64,
            y: f64,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, vibex_core::BrowserElementSource>
        {
            self.source_calls.lock().unwrap().push((x, y));
            Box::pin(async {
                let source = self.element_source.lock().unwrap().clone();
                source.ok_or_else(|| {
                    crate::browser_transport::BrowserTransportError::new(
                        "test",
                        "no element source",
                    )
                })
            })
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
            width: u32,
            height: u32,
            device_scale_factor: f64,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async move {
                self.viewports
                    .lock()
                    .unwrap()
                    .push((width, height, device_scale_factor));
                Ok(())
            })
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
        fn go_back(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            self.history_moves.lock().unwrap().push(false);
            Box::pin(async { Ok(()) })
        }
        fn go_forward(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            self.history_moves.lock().unwrap().push(true);
            Box::pin(async { Ok(()) })
        }
        fn favicon(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<
            '_,
            Option<vibex_browser::BrowserFavicon>,
        > {
            Box::pin(async { Ok(None) })
        }
        fn selection_text(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
        fn select_menu_at(
            &self,
            _tab_id: &BrowserTabId,
            _x: f64,
            _y: f64,
        ) -> crate::browser_transport::BrowserTransportFuture<
            '_,
            Option<vibex_browser::BrowserSelectMenu>,
        > {
            Box::pin(async { Ok(None) })
        }
        fn choose_select_option(
            &self,
            _tab_id: &BrowserTabId,
            _index: u32,
            _value: &str,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn subscribe_frames(
            &self,
            _tab_id: &BrowserTabId,
            quality: BrowserCaptureQuality,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, BrowserFrameStream> {
            self.subscribe_qualities.lock().unwrap().push(quality);
            self.subscribe_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let remaining_failures = self
                .subscribe_failures
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |failures| failures.checked_sub(1),
                )
                .is_ok();
            Box::pin(async move {
                if remaining_failures {
                    Err(BrowserTransportError::new(
                        "test",
                        "the frame stream failed once",
                    ))
                } else {
                    Ok(BrowserFrameStream::Unavailable)
                }
            })
        }
        fn stop_screencast(
            &self,
            _tab_id: &BrowserTabId,
        ) -> crate::browser_transport::BrowserTransportFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    // A stream that fails must be retried: leaving the last frame on screen
    // with no further attempts is what made a frozen tab look alive.
    #[gpui::test]
    fn a_failed_frame_stream_is_retried(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport {
            subscribe_failures: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
            ..Default::default()
        });
        let calls = transport.subscribe_calls.clone();
        let transport: Arc<dyn BrowserTransport> = transport;
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface = BrowserSurface::new("probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface.set_active(true, cx);
                    surface
                })
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");

        // The first attempt fails, the retry succeeds after its backoff — the
        // test clock advances so the timer fires without waiting.
        cx.run_until_parked();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first attempt happens immediately"
        );
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(600));
        cx.run_until_parked();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the failed stream is retried once"
        );
        // The second attempt reports that this client has no frame channel.
        cx.run_until_parked();
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.phase),
            SurfacePhase::Unavailable
        );
    }

    #[test]
    fn css_cursors_map_to_the_nearest_native_shape() {
        assert_eq!(cursor_style_for("pointer"), gpui::CursorStyle::PointingHand);
        assert_eq!(cursor_style_for(" Text "), gpui::CursorStyle::IBeam);
        assert_eq!(cursor_style_for("grab"), gpui::CursorStyle::OpenHand);
        assert_eq!(
            cursor_style_for("col-resize"),
            gpui::CursorStyle::ResizeLeftRight
        );
        // Values the platform cannot express fall back to the arrow instead of
        // guessing at a shape the page never asked for.
        for unknown in ["auto", "default", "zoom-in", "help", "wait", ""] {
            assert_eq!(
                cursor_style_for(unknown),
                gpui::CursorStyle::Arrow,
                "{unknown:?} has no native equivalent"
            );
        }
    }

    // Find in page: headless Chrome has no find bar, so the panel owns both the
    // shortcut and the search. The count has to come from the page — a client
    // that counted locally would disagree with the highlights on screen.
    #[gpui::test]
    fn the_find_bar_searches_the_page_and_clears_it_on_close(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport {
            find_answer: Arc::new(std::sync::Mutex::new((7, 3))),
            ..Default::default()
        });
        let calls = transport.find_calls.clone();
        let clears = transport.find_clears.clone();
        let transport: Arc<dyn BrowserTransport> = transport;
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface = BrowserSurface::new("probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface.set_active(true, cx);
                    surface
                })
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        cx.run_until_parked();

        surface.update_in(&mut cx, |surface, window, cx| {
            assert!(!surface.find_open);
            surface.open_find(window, cx);
            assert!(surface.find_open, "the shortcut opens the bar");
            surface.find_input.update(cx, |input, cx| {
                input.set_value("needle", window, cx);
                // `set_value` deliberately suppresses the change event, so the
                // subscription is driven the way typing drives it.
                cx.emit(InputEvent::Change);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec![("needle".to_string(), true)],
            "typing searches forward from the top"
        );
        surface.read_with(&cx, |surface, _| {
            assert_eq!((surface.find_total, surface.find_current), (7, 3));
        });

        // Shift+Enter is the previous step, and the query is what decides
        // whether this is a step or a new search.
        surface.update_in(&mut cx, |surface, _, cx| surface.run_find(true, false, cx));
        cx.run_until_parked();
        assert_eq!(
            calls.lock().unwrap().last().cloned(),
            Some(("needle".to_string(), false))
        );

        surface.update(&mut cx, |surface, cx| surface.close_find(cx));
        cx.run_until_parked();
        assert_eq!(
            clears.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "closing the bar takes the highlights off the page"
        );
        surface.read_with(&cx, |surface, _| {
            assert!(!surface.find_open);
            assert_eq!((surface.find_total, surface.find_current), (0, 0));
        });
    }

    // An emptied field is a cleared search, not a search for the empty string —
    // which would match everywhere and highlight the whole page.
    #[gpui::test]
    fn emptying_the_find_field_clears_the_page(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport {
            find_answer: Arc::new(std::sync::Mutex::new((7, 3))),
            ..Default::default()
        });
        let calls = transport.find_calls.clone();
        let clears = transport.find_clears.clone();
        let transport: Arc<dyn BrowserTransport> = transport;
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface = BrowserSurface::new("probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface.set_active(true, cx);
                    surface
                })
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        cx.run_until_parked();

        surface.update_in(&mut cx, |surface, window, cx| {
            surface.open_find(window, cx);
            surface.find_input.update(cx, |input, cx| {
                input.set_value("needle", window, cx);
                cx.emit(InputEvent::Change);
            });
        });
        cx.run_until_parked();
        let after_typing = calls.lock().unwrap().len();

        surface.update_in(&mut cx, |surface, window, cx| {
            surface.find_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                cx.emit(InputEvent::Change);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            calls.lock().unwrap().len(),
            after_typing,
            "an empty query never reaches the page"
        );
        assert!(
            clears.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the page is told to drop the highlights"
        );
        surface.read_with(&cx, |surface, _| {
            assert_eq!((surface.find_total, surface.find_current), (0, 0));
        });
    }

    // The HD toggle is only honest if the encoder really changes: a live JPEG
    // stream would keep painting over the choice, so the switch has to restart
    // the subscription with the new mode.
    #[gpui::test]
    fn switching_to_hd_restarts_the_stream_with_the_new_mode(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport = Arc::new(RecordingTransport::default());
        let qualities = transport.subscribe_qualities.clone();
        let calls = transport.subscribe_calls.clone();
        let transport: Arc<dyn BrowserTransport> = transport;
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface = BrowserSurface::new("probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface.set_active(true, cx);
                    surface
                })
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        cx.run_until_parked();

        assert_eq!(
            qualities.lock().unwrap().clone(),
            vec![BrowserCaptureQuality::Standard],
            "a panel starts on the small, fast encoder"
        );

        surface.update(&mut cx, |surface, cx| {
            surface.set_capture_quality(BrowserCaptureQuality::High, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            qualities.lock().unwrap().clone(),
            vec![BrowserCaptureQuality::Standard, BrowserCaptureQuality::High],
            "the new mode reaches the runtime"
        );
        surface.read_with(&cx, |surface, _| {
            assert_eq!(surface.capture_quality(), BrowserCaptureQuality::High);
        });

        // Pressing the mode already in force is not a restart: an idle flick of
        // the button must not tear the picture down and re-encode it.
        let before = calls.load(std::sync::atomic::Ordering::SeqCst);
        surface.update(&mut cx, |surface, cx| {
            surface.set_capture_quality(BrowserCaptureQuality::High, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            before,
            "setting the mode already in force changes nothing"
        );
    }

    // Regression: the frame geometry used to come from
    // `ElementExt::on_prepaint`, whose helper canvas is laid out after the
    // in-flow content. Its origin was the bottom edge of the frame area, so
    // every pointer position converted to a negative local coordinate and the
    // panel dropped all mouse input while still accepting IME text.
    fn snapshot_with(tabs: Vec<vibex_core::BrowserTab>) -> vibex_core::BrowserSessionSnapshot {
        snapshot_with_source(tabs, vibex_core::BrowserExecutionSource::User, true)
    }

    /// A snapshot whose driver is named, for the takeover-state tests.
    fn snapshot_with_source(
        tabs: Vec<vibex_core::BrowserTab>,
        execution_source: vibex_core::BrowserExecutionSource,
        user_engaged: bool,
    ) -> vibex_core::BrowserSessionSnapshot {
        vibex_core::BrowserSessionSnapshot {
            session: vibex_core::BrowserSession {
                session_id: BrowserSessionId::new(),
                workspace_id: None,
                tabs,
                active_tab_id: None,
                agent_tab_id: None,
                execution_source,
                user_engaged,
                recording: false,
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
        tab_owned_by(tab_id, title, status, vibex_core::BrowserTabOwner::User)
    }

    fn tab_owned_by(
        tab_id: &BrowserTabId,
        title: &str,
        status: BrowserTabStatus,
        owner: vibex_core::BrowserTabOwner,
    ) -> vibex_core::BrowserTab {
        vibex_core::BrowserTab {
            tab_id: tab_id.clone(),
            url: "https://example.com/".to_string(),
            title: title.to_string(),
            status,
            owner,
            agent_session_id: None,
            created_at_ms: 0,
            last_activity_at_ms: 0,
            generation: 1,
            can_go_back: false,
            can_go_forward: false,
        }
    }

    // An Agent's tab is marked in the strip, so the panel has to report who
    // opened it alongside the title.
    #[gpui::test]
    fn the_panel_reports_who_opened_the_tab(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let tab_id = BrowserTabId::new();
        let snapshot = Arc::new(std::sync::Mutex::new(Some(snapshot_with(vec![
            tab_owned_by(
                &tab_id,
                "Agent page",
                BrowserTabStatus::Ready,
                vibex_core::BrowserTabOwner::Agent,
            ),
        ]))));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            snapshot: snapshot.clone(),
            ..Default::default()
        });
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| BrowserSurface::new(tab_id.as_str().to_string(), window, cx))
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, cx| {
            surface.attach(transport, BrowserSessionId::new(), tab_id.clone(), cx);
            surface.refresh_tab(cx);
        });
        cx.run_until_parked();
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tab_owner()),
            Some(vibex_core::BrowserTabOwner::Agent)
        );

        // A tab the human opened is not marked.
        *snapshot.lock().unwrap() = Some(snapshot_with(vec![tab_owned_by(
            &tab_id,
            "Mine",
            BrowserTabStatus::Ready,
            vibex_core::BrowserTabOwner::User,
        )]));
        surface.update(&mut cx, |surface, cx| surface.refresh_tab(cx));
        cx.run_until_parked();
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tab_owner()),
            Some(vibex_core::BrowserTabOwner::User)
        );
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
            snapshot: snapshot.clone(),
            ..Default::default()
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

    /// The strip's green Agent mark follows the runtime's answer.
    ///
    /// Touching the page no longer pauses the Agent, so the tab strip is where
    /// a reader can see who is driving: green means the Agent, and the mark
    /// goes back to muted the moment it is paused.
    #[gpui::test]
    fn a_driving_agent_reaches_the_tab_strip(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let tab_id = BrowserTabId::new();
        let snapshot = Arc::new(std::sync::Mutex::new(Some(snapshot_with(vec![tab_with(
            &tab_id,
            "Example",
            BrowserTabStatus::Ready,
        )]))));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            snapshot: snapshot.clone(),
            ..Default::default()
        });
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| BrowserSurface::new(tab_id.as_str().to_string(), window, cx))
            })
            .expect("surface window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        let seen = Arc::new(std::sync::Mutex::new(0usize));
        let seen_events = seen.clone();
        let _subscription = surface.update(&mut cx, |_, cx| {
            cx.subscribe_self(move |_, event: &BrowserSurfaceEvent, _| {
                if matches!(event, BrowserSurfaceEvent::TabChanged { .. }) {
                    *seen_events.lock().unwrap() += 1;
                }
            })
        });
        surface.update(&mut cx, |surface, cx| {
            surface.attach(transport, BrowserSessionId::new(), tab_id.clone(), cx);
            surface.refresh_tab(cx);
        });
        cx.run_until_parked();
        assert!(
            !surface.read_with(&cx, |surface, _| surface.agent_driving()),
            "a user-driven tab is not marked green"
        );
        let before = *seen.lock().unwrap();

        *snapshot.lock().unwrap() = Some(snapshot_with_source(
            vec![tab_with(&tab_id, "Example", BrowserTabStatus::Ready)],
            vibex_core::BrowserExecutionSource::Agent,
            false,
        ));
        surface.update(&mut cx, |surface, cx| surface.refresh_tab(cx));
        cx.run_until_parked();
        assert!(surface.read_with(&cx, |surface, _| surface.agent_driving()));
        assert_eq!(
            *seen.lock().unwrap(),
            before + 1,
            "the strip hears that the Agent took over"
        );

        // Paused by the reader: the Agent still owns the tab, but it is not
        // driving, and the mark goes back to muted.
        *snapshot.lock().unwrap() = Some(snapshot_with_source(
            vec![tab_with(&tab_id, "Example", BrowserTabStatus::Ready)],
            vibex_core::BrowserExecutionSource::User,
            true,
        ));
        surface.update(&mut cx, |surface, cx| surface.refresh_tab(cx));
        cx.run_until_parked();
        assert!(!surface.read_with(&cx, |surface, _| surface.agent_driving()));
        assert!(surface.read_with(&cx, |surface, _| surface.agent_paused));
        assert_eq!(*seen.lock().unwrap(), before + 2);
    }

    #[test]
    fn activity_labels_cover_every_kind_and_stay_short() {
        // Every kind has a label; an unknown one from a newer runtime is
        // `other` rather than a panic or an empty cell.
        for kind in [
            BrowserActionKind::Launch,
            BrowserActionKind::Navigate,
            BrowserActionKind::Observe,
            BrowserActionKind::ElementToSource,
            BrowserActionKind::Unknown,
        ] {
            assert!(!activity_kind(kind).is_empty());
        }
        assert_eq!(activity_kind(BrowserActionKind::Unknown), "other");
    }

    #[test]
    fn relative_times_are_coarse_and_never_negative() {
        assert_eq!(relative_time(1_000, 1_500), "now");
        assert_eq!(relative_time(1_000, 30_000), "29s");
        assert_eq!(relative_time(1_000, 130_000), "2m");
        assert_eq!(relative_time(1_000, 7_300_000), "2h");
        // A clock that moved backwards must not print a negative age.
        assert_eq!(relative_time(2_000, 1_000), "now");
    }

    #[gpui::test]
    fn opening_the_activity_list_reads_the_ledger(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let record = vibex_core::BrowserActionRecord {
            id: "braction_probe".to_string(),
            session_id: BrowserSessionId::new(),
            tab_id: BrowserTabId::new(),
            kind: BrowserActionKind::Click,
            summary: "clicked `Submit`".to_string(),
            at_ms: 1_000,
            status: BrowserOperationStatus::Dispatched,
            domain: Some("example.com".to_string()),
            execution_source: BrowserExecutionSource::Agent,
        };
        let ledger = Arc::new(std::sync::Mutex::new(vec![record]));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            ledger: ledger.clone(),
            ..Default::default()
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface =
                        BrowserSurface::new("browser_tab_probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface
                })
            })
            .expect("browser probe window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, cx| surface.toggle_ledger(cx));
        cx.run_until_parked();

        let (open, pending, error, records) = surface.read_with(&cx, |surface, _| {
            (
                surface.ledger_open,
                surface.ledger_pending,
                surface.ledger_error.clone(),
                surface.ledger.len(),
            )
        });
        assert!(open, "the activity list opens");
        assert!(!pending, "the fetch finished");
        assert_eq!(error, None);
        assert_eq!(records, 1, "the runtime's ledger is what the panel shows");
    }

    #[gpui::test]
    fn an_active_recording_is_shown_in_the_panel(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let mut snapshot = snapshot_with(vec![]);
        snapshot.session.recording = true;
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            snapshot: Arc::new(std::sync::Mutex::new(Some(snapshot))),
            ..Default::default()
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface =
                        BrowserSurface::new("browser_tab_probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface
                })
            })
            .expect("browser probe window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        surface.update(&mut cx, |surface, cx| surface.refresh_tab(cx));
        cx.run_until_parked();

        // The banner the human must see: recording keeps raw form values.
        assert!(
            surface.read_with(&cx, |surface, _| surface.recording),
            "the snapshot's recording flag reaches the panel"
        );
        assert!(
            surface.read_with(&cx, |surface, _| surface.render_recording_banner_for_test()),
            "a recording shows a banner"
        );
    }

    #[gpui::test]
    fn alt_clicking_the_page_asks_which_file_drew_the_element(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            element_source: Arc::new(std::sync::Mutex::new(Some(
                vibex_core::BrowserElementSource {
                    path: "/src/App.tsx".to_string(),
                    line: Some(42),
                    column: Some(3),
                    component: Some("App".to_string()),
                    framework: "react".to_string(),
                    approximate: false,
                    detail: None,
                },
            ))),
            source_calls: calls.clone(),
            inputs: inputs.clone(),
            ..Default::default()
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

        cx.simulate_mouse_down(
            bounds.center(),
            MouseButton::Left,
            gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        cx.run_until_parked();

        // The click asked about the point under the pointer...
        let asked = calls.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "one lookup for one Alt+click: {asked:?}");
        assert!(asked[0].0 > 0.0 && asked[0].1 > 0.0, "viewport coordinates");
        // ...and the page never saw a click: Alt+click looks for code.
        assert!(
            inputs.lock().unwrap().is_empty(),
            "Alt+click must not reach the page"
        );
    }

    #[gpui::test]
    fn a_page_that_cannot_map_an_element_says_so(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            element_source: Arc::new(std::sync::Mutex::new(Some(
                vibex_core::BrowserElementSource {
                    path: String::new(),
                    line: None,
                    column: None,
                    component: None,
                    framework: "unknown".to_string(),
                    approximate: true,
                    detail: Some(
                        "this page is not a React, Vue or Svelte development build".to_string(),
                    ),
                },
            ))),
            ..Default::default()
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut surface =
                        BrowserSurface::new("browser_tab_probe".to_string(), window, cx);
                    surface.attach(transport, BrowserSessionId::new(), BrowserTabId::new(), cx);
                    surface
                })
            })
            .expect("browser probe window")
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let surface = window.root(&mut cx).expect("surface");
        cx.run_until_parked();

        surface.update(&mut cx, |surface, cx| surface.locate_source(40.0, 40.0, cx));
        cx.run_until_parked();

        let notice = surface.read_with(&cx, |surface, _| surface.source_notice.clone());
        assert_eq!(
            notice.as_deref(),
            Some("this page is not a React, Vue or Svelte development build"),
            "an unmappable element explains itself instead of doing nothing"
        );
    }

    #[gpui::test]
    fn a_click_on_the_page_reaches_the_transport(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport: Arc<dyn BrowserTransport> = Arc::new(RecordingTransport {
            inputs: inputs.clone(),
            ..Default::default()
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
