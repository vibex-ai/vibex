use std::{
    collections::BTreeSet,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::theme;
use crate::typography::code_font_weight;
use gpui::{
    AnyElement, App, BackgroundExecutor, BorderStyle, Bounds, ClipboardItem, ContentMask, Context,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Font, FontFallbacks,
    FontFeatures, FontStyle, FontWeight, Hsla, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, ShapedLine, Subscription, Task, TextAlign, TextRun, UTF16Selection,
    WeakEntity, Window, actions, canvas, div, fill, font, outline, point, prelude::*, px, rgb,
    size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Selectable as _,
    Sizable as _,
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    input::{Input, InputEvent, InputState},
    tab::{Tab, TabBar},
    v_flex,
};
use vibex_content::{
    TerminalCellMetrics, TerminalFrameCache, TerminalKey, TerminalModifiers, TerminalMouseEvent,
    TerminalMouseKind, TerminalResizeCoordinator, TerminalSurfaceBackend, TerminalSyncOutcome,
    new_ui_terminal_manager,
};
use vibex_core::{
    TerminalCreateRequest, TerminalId, TerminalSession, TerminalStatus, TerminalSwitchShellRequest,
    WorkspaceId,
};
use vibex_desktop_runtime::validate_external_open_url;
use vibex_terminal::{
    TerminalCellColor, TerminalCellSnapshot, TerminalColorQuery, TerminalCursorShape,
    TerminalCursorSnapshot, TerminalFrameSnapshot, TerminalGridPoint, TerminalHostSignals,
    TerminalManager, TerminalScrollbackMatch,
};

use crate::terminal_box_drawing::{self, BoxStroke};
use crate::terminal_links::{self, RowLink};

use crate::motion;
use crate::terminal_transport::{
    LocalTerminalTransport, TerminalTransport, TerminalTransportError, TerminalTransportFuture,
    TerminalTransportResult,
};

/// The code font's line box, as a multiple of the font size.
///
/// The cell is as tall as the font's own line box plus a little breathing room,
/// not a constant from a 13px design: the terminal cell and the fontSize the
/// shaper is given have to agree, or the glyphs drift inside their cells at
/// every zoom level.
const TERMINAL_LINE_HEIGHT_RATIO: f32 = 1.38;
const TERMINAL_MIN_FONT_SIZE: f32 = 10.0;
const TERMINAL_MAX_FONT_SIZE: f32 = 24.0;
const TERMINAL_HORIZONTAL_PADDING: f32 = 12.0;
const TERMINAL_VERTICAL_PADDING: f32 = 12.0;
/// Minimum perceptual distance between ink and the cell it sits on. Below this
/// a program's colour is washed out to the cell's opposite until it clears.
const TERMINAL_MINIMUM_CONTRAST: f32 = 0.35;
/// How far a dim cell's ink is pushed toward the cell background.
const TERMINAL_DIM_OPACITY: f32 = 0.68;
/// Extra ink a selected cell's background takes from the default foreground.
const TERMINAL_SELECTION_OPACITY: f32 = 0.28;
// Cells are divided into 8 columns and 24 lines, the least common multiple of
// the 8-way splits of the eighth blocks and the 3-way splits of the sextants.
/// How many shaped rows the grid keeps between frames.
///
/// Enough for several screens of scrollback at a typical window height, small
/// enough that a long-running terminal cannot pin every row it ever showed.
const GLYPH_CACHE_LIMIT: usize = 4_096;
const TERMINAL_SUBCELL_COLUMNS: f32 = 8.0;
const TERMINAL_SUBCELL_LINES: f32 = 24.0;

// Tauri preview terminals use `p-2`, while the Composer surface has another
// compact wrapper around its grid. Keep those insets distinct so a preview
// terminal does not gain standalone chrome or lose the expected content edge.
const PREVIEW_TERMINAL_PADDING: f32 = 8.0;
const COMPOSER_TERMINAL_PADDING: f32 = 4.0;
/// The session page's inset inside its rounded window.
///
/// The page keeps a gutter of its own tone *around* the window; this is the
/// breathing room *inside* it. Without it the first column sits on the window's
/// edge and the corner arcs fall across glyphs, which reads as a clipped
/// rectangle rather than a window the terminal sits in.
const SESSION_TERMINAL_PADDING: f32 = 8.0;
const TERMINAL_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(16);
const TERMINAL_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINAL_IDLE_POLL_THRESHOLD: u16 = 4;
const TERMINAL_RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);
const TERMINAL_CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// Width of the scrollbar overlay painted inside the grid's right edge.
const TERMINAL_SCROLLBAR_WIDTH: f32 = 6.0;
/// The horizontal inset that keeps the scrollbar off the grid's edge.
const TERMINAL_SCROLLBAR_MARGIN: f32 = 2.0;
/// The pointer band that begins a scrollbar drag. Wider than the bar itself so
/// a six-pixel thumb is not a precision task.
const TERMINAL_SCROLLBAR_HIT_WIDTH: f32 = 12.0;
/// Shortest thumb, in logical pixels, so a deep scrollback still shows a
/// grabbable handle.
const TERMINAL_SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// How much of the cell background an inactive search match takes.
const TERMINAL_SEARCH_MATCH_OPACITY: f32 = 0.22;
/// The ink a hovered detected link underlines with.
const TERMINAL_LINK_UNDERLINE_OPACITY: f32 = 0.8;
/// How often the scrollback is rescanned while the search bar is open.
///
/// A full-buffer scan is cheap once, not once per output poll: an Agent that
/// streams a long transcript would otherwise rescan ten thousand rows at the
/// frame rate.
const TERMINAL_SEARCH_REFRESH_INTERVAL: Duration = Duration::from_millis(120);
const PHYSICAL_MARKER_COMMAND: &str = "printf vibex-native-content-ok";
const TERMINAL_KEY_CONTEXT: &str = "VibexTerminal";

actions!(vibex_terminal, [SendTab, SendBacktab]);

pub fn bind_terminal_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", SendBacktab, Some(TERMINAL_KEY_CONTEXT)),
    ]);
}

type SharedTerminalBackend = Arc<Mutex<TerminalSurfaceBackend>>;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalSurfaceMode {
    Standalone,
    Preview,
    Composer,
    /// The session page's own surface.
    ///
    /// A terminal-mode session gives the whole column to the Agent CLI, so this
    /// mode draws no tabs, no toolbar, and the smallest padding the grid can
    /// take while staying readable.
    Session,
}

impl TerminalSurfaceMode {
    fn shows_chrome(self) -> bool {
        self == Self::Standalone
    }

    /// Whether the surface paints the page tone behind its cells.
    ///
    /// A session page embeds the grid in a rounded window of its own, so the
    /// surface must stay transparent: an opaque fill here is a rectangle, and
    /// its corners would square off the window it was given.
    fn paints_surface_background(self) -> bool {
        self != Self::Session
    }

    fn horizontal_padding(self) -> f32 {
        match self {
            Self::Standalone => TERMINAL_HORIZONTAL_PADDING,
            Self::Preview => PREVIEW_TERMINAL_PADDING,
            Self::Composer => COMPOSER_TERMINAL_PADDING,
            Self::Session => SESSION_TERMINAL_PADDING,
        }
    }

    fn vertical_padding(self) -> f32 {
        match self {
            Self::Standalone => TERMINAL_VERTICAL_PADDING,
            Self::Preview => PREVIEW_TERMINAL_PADDING,
            Self::Composer => COMPOSER_TERMINAL_PADDING,
            Self::Session => SESSION_TERMINAL_PADDING,
        }
    }
}

struct TerminalTab {
    session: TerminalSession,
    backend: SharedTerminalBackend,
    frame: TerminalFrameCache,
    full_repaints: u64,
    partial_repaints: u64,
    changed_rows: u64,
    sequence_gaps: u64,
    /// The title the program published with `OSC 0`/`OSC 2`.
    ///
    /// Kept beside the session rather than written into it: the runtime owns
    /// the session record and its periodic update would overwrite a title the
    /// program set.
    osc_title: Option<String>,
}

impl TerminalTab {
    fn new(session: TerminalSession) -> Self {
        let mut backend = TerminalSurfaceBackend::new(session.rows, session.cols);
        let _ = backend.lifecycle_mut().activate(1);
        let initial = backend.frame();
        Self {
            session,
            backend: Arc::new(Mutex::new(backend)),
            frame: TerminalFrameCache::new(&initial),
            full_repaints: 1,
            partial_repaints: 0,
            changed_rows: u64::from(initial.rows),
            sequence_gaps: 0,
            osc_title: None,
        }
    }
}

#[derive(Clone)]
struct TerminalPollWork {
    terminal_id: TerminalId,
    next_sequence: i64,
    transport: Arc<dyn TerminalTransport>,
    backend: SharedTerminalBackend,
}

struct TerminalPollResult {
    terminal_id: TerminalId,
    session: Option<TerminalSession>,
    result: Result<Option<(TerminalSyncOutcome, TerminalFrameSnapshot)>, String>,
    /// Bytes the emulator owes the program, carried out of the blocking poll so
    /// the UI thread can write them back with the transport.
    pty_replies: Vec<u8>,
}

fn terminal_poll_interval(idle_poll_count: u16) -> Duration {
    if idle_poll_count >= TERMINAL_IDLE_POLL_THRESHOLD {
        TERMINAL_IDLE_POLL_INTERVAL
    } else {
        TERMINAL_ACTIVE_POLL_INTERVAL
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPhysicalObservation {
    pub tab_count: usize,
    pub pty_created: bool,
    pub active_running: bool,
    pub command_submitted: bool,
    pub command_marker_observed: bool,
    pub non_blank_cells: usize,
    pub styled_cells: usize,
    pub cursor_present: bool,
    pub history_lines: usize,
    pub rows: u16,
    pub columns: u16,
    pub ingested_bytes: u64,
    pub full_repaints: u64,
    pub partial_repaints: u64,
    pub sequence_gaps: u64,
    pub selection_copy_available: bool,
    pub search_match_count: usize,
    pub ime_commits: u64,
    pub last_error_code: Option<&'static str>,
}

/// A row's shaped pieces, reused across frames while the row is unchanged.
#[derive(Clone, Default)]
struct TerminalRowLayout {
    runs: Vec<TerminalTextRun>,
    rects: Vec<TerminalRect>,
    strokes: Vec<BoxStroke>,
}

/// The shaped rows from the last prepaint, and the inputs they were shaped from.
#[derive(Default)]
struct RowLayoutCache {
    /// Hash of everything that changes how *every* row is shaped. A different
    /// epoch means the cached rows describe another font, theme or tab.
    epoch: u64,
    rows: Vec<Arc<TerminalRowLayout>>,
}

/// The scrollbar's track and thumb in window space.
#[derive(Clone, Copy)]
struct TerminalScrollbarGeometry {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
}

pub struct TerminalSurface {
    transport: Arc<dyn TerminalTransport>,
    background: BackgroundExecutor,
    workspace_root: PathBuf,
    workspace_id: WorkspaceId,
    tabs: Vec<TerminalTab>,
    active_tab: Option<usize>,
    search_input: Entity<InputState>,
    search_open: bool,
    search_matches: Vec<TerminalScrollbackMatch>,
    /// When the buffer was last rescanned for the open search bar.
    last_search_refresh: Option<Instant>,
    active_search_match: usize,
    marked_text: Option<String>,
    selection_anchor: Option<TerminalGridPoint>,
    selection_text: Option<String>,
    active_hyperlink: Option<String>,
    /// The detected link under the pointer: its row and the columns it covers.
    ///
    /// Kept so the paint pass can underline it. `OSC 8` links already carry
    /// their own underline through the cell style; a detected one has no style
    /// until the pointer says it is interesting.
    hovered_link: Option<(u16, RowLink)>,
    /// Per-row link scan for the row the pointer was last over, keyed by the
    /// frame's byte count so output invalidates it.
    row_links: std::cell::RefCell<Option<(u16, u64, u16, Vec<RowLink>)>>,
    /// Whether the scrollbar thumb is being dragged.
    scrollbar_drag: bool,
    focus: FocusHandle,
    cell_metrics: TerminalCellMetrics,
    /// Fractional scroll not yet spent as a whole line.
    ///
    /// A trackpad reports many sub-row deltas per gesture. Rounding each one
    /// independently turned a single flick into dozens of three-line jumps and
    /// could also step backwards; the remainder is carried instead.
    scroll_remainder: f32,
    /// Shaped rows keyed by the text and style they were shaped from.
    ///
    /// A terminal repaints on every output poll and every cursor blink, but a
    /// row that did not change must not be shaped again: the layout costs more
    /// than the paint, and a screen of prose is the same handful of strings
    /// frame after frame.
    glyph_cache: std::cell::RefCell<std::collections::HashMap<u64, ShapedLine>>,
    /// Rows whose shaped layout must be rebuilt before the next paint.
    ///
    /// A poll reports exactly the rows it damaged; shaping only those is what
    /// keeps a busy terminal's frame from re-laying out the whole screen.
    dirty_rows: std::cell::RefCell<BTreeSet<u16>>,
    /// Shaped rows from the last prepaint, and the epoch they belong to.
    row_layouts: std::cell::RefCell<RowLayoutCache>,
    /// Bumped by every local scroll.
    ///
    /// A poll that started before the scroll carries the older viewport; without
    /// this floor it would paint the pre-scroll rows back over the ones the
    /// reader just moved to, which is what made the content look duplicated.
    scroll_generation: u64,
    resize: TerminalResizeCoordinator,
    resize_task: Option<Task<()>>,
    /// Bounds of the painted grid from the last canvas prepaint — the area the
    /// cells start at — used to turn pointer positions into grid points and to
    /// place the scrollbar over the cells it scrolls.
    grid_bounds: Option<Bounds<Pixels>>,
    last_grid_bounds: Option<(String, u32, u32)>,
    /// The grid box the terminal should be sized to right now. It follows every
    /// prepaint, including when the debounced request for that box is already
    /// pending, so a resize response can be checked against what is on screen.
    desired_grid_size: Option<(String, f32, f32)>,
    poll_task: Option<Task<()>>,
    blink_task: Option<Task<()>>,
    foreground_active: bool,
    idle_poll_count: u16,
    cursor_visible: bool,
    note: String,
    physical_marker_shortcut: bool,
    command_submitted: bool,
    command_marker_observed: bool,
    ime_commits: u64,
    last_error_code: Option<&'static str>,
    mode: TerminalSurfaceMode,
    owned_terminal_ids: BTreeSet<String>,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    input_log: Vec<Vec<u8>>,
    /// Mouse reports the surface handed to the program, in the order it sent
    /// them. Tests read this instead of the PTY so a wheel's cell can be
    /// asserted without a live program to echo it back.
    #[cfg(test)]
    mouse_log: Vec<TerminalMouseEvent>,
}

impl TerminalSurface {
    pub fn new(
        physical_marker_shortcut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transport = LocalTerminalTransport::new(new_ui_terminal_manager());
        let workspace_root = std::env::var_os("VIBEX_NATIVE_CONTENT_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let workspace_id = WorkspaceId::new();
        let initial = transport.create_now(
            &workspace_root,
            TerminalCreateRequest {
                workspace_id: workspace_id.clone(),
                title: Some("Terminal 1".into()),
                shell: None,
                cwd: Some(workspace_root.display().to_string()),
                rows: 20,
                cols: 100,
            },
        );
        let (tabs, active_tab, note, last_error_code) = match initial {
            Ok(session) => (
                vec![TerminalTab::new(session)],
                Some(0),
                "Live PTY connected through bounded raw-byte snapshots".into(),
                None,
            ),
            Err(error) => (
                Vec::new(),
                None,
                format!("Terminal unavailable: {}", error.message),
                Some("terminal_create_failed"),
            ),
        };
        Self::build(
            Arc::new(transport),
            workspace_root,
            workspace_id,
            tabs,
            active_tab,
            note,
            last_error_code,
            physical_marker_shortcut,
            TerminalSurfaceMode::Standalone,
            true,
            window,
            cx,
        )
    }

    #[allow(dead_code)]
    pub fn from_shared_session(
        manager: TerminalManager,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_shared_session_with_transport(
            Arc::new(LocalTerminalTransport::new(manager)),
            workspace_root,
            session,
            window,
            cx,
        )
    }

    #[allow(dead_code)]
    pub fn from_preview_shared_session(
        manager: TerminalManager,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_preview_shared_session_with_transport(
            Arc::new(LocalTerminalTransport::new(manager)),
            workspace_root,
            session,
            window,
            cx,
        )
    }

    #[allow(dead_code)]
    pub fn from_embedded_shared_session(
        manager: TerminalManager,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_embedded_shared_session_with_transport(
            Arc::new(LocalTerminalTransport::new(manager)),
            workspace_root,
            session,
            window,
            cx,
        )
    }

    /// Attaches an existing session through an explicit transport.
    ///
    /// The `from_*_shared_session` constructors keep the native manager; this
    /// one lets a remote authority serve the same surface.
    #[allow(dead_code)]
    pub fn from_shared_session_with_transport(
        transport: Arc<dyn TerminalTransport>,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            transport,
            workspace_root,
            workspace_id,
            vec![TerminalTab::new(session)],
            Some(0),
            "Attached to the shared workspace PTY through bounded raw-byte snapshots".into(),
            None,
            false,
            TerminalSurfaceMode::Standalone,
            false,
            window,
            cx,
        )
    }

    /// Attaches an existing Preview session through an explicit transport.
    #[allow(dead_code)]
    pub fn from_preview_shared_session_with_transport(
        transport: Arc<dyn TerminalTransport>,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            transport,
            workspace_root,
            workspace_id,
            vec![TerminalTab::new(session)],
            Some(0),
            "Attached to the Preview terminal".into(),
            None,
            false,
            TerminalSurfaceMode::Preview,
            false,
            window,
            cx,
        )
    }

    /// Attaches a terminal-mode session's PTY to the session page.
    ///
    /// The surface never owns the process: closing the session is what stops
    /// the Agent, and a surface dropped because the reader changed pages must
    /// leave it running. That is why `owns_initial_sessions` is false here.
    pub fn from_session_shared_session_with_transport(
        transport: Arc<dyn TerminalTransport>,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            transport,
            workspace_root,
            workspace_id,
            vec![TerminalTab::new(session)],
            Some(0),
            "Attached to this session's terminal".into(),
            None,
            false,
            TerminalSurfaceMode::Session,
            false,
            window,
            cx,
        )
    }

    /// Attaches an existing Composer session through an explicit transport.
    #[allow(dead_code)]
    pub fn from_embedded_shared_session_with_transport(
        transport: Arc<dyn TerminalTransport>,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            transport,
            workspace_root,
            workspace_id,
            vec![TerminalTab::new(session)],
            Some(0),
            "Attached to the Composer terminal".into(),
            None,
            false,
            TerminalSurfaceMode::Composer,
            false,
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        transport: Arc<dyn TerminalTransport>,
        workspace_root: PathBuf,
        workspace_id: WorkspaceId,
        tabs: Vec<TerminalTab>,
        active_tab: Option<usize>,
        note: String,
        last_error_code: Option<&'static str>,
        physical_marker_shortcut: bool,
        mode: TerminalSurfaceMode,
        owns_initial_sessions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder("Search terminal")
        });
        let focus = cx.focus_handle();
        let focus_in = cx.on_focus_in(&focus, window, |this, window, cx| {
            this.send_focus(true, cx);
            window.invalidate_character_coordinates();
            cx.notify();
        });
        let focus_out = cx.on_focus_out(&focus, window, |this, _, window, cx| {
            this.send_focus(false, cx);
            if this.marked_text.take().is_some() {
                window.invalidate_character_coordinates();
                cx.notify();
            }
        });
        let cell_metrics = measured_terminal_cell_metrics(cx.text_system().as_ref(), cx, mode);
        let resize = TerminalResizeCoordinator::new(cell_metrics);
        let owned_terminal_ids = if owns_initial_sessions {
            tabs.iter()
                .map(|tab| tab.session.id.as_str().to_string())
                .collect()
        } else {
            BTreeSet::new()
        };
        // A blinking cursor is a 500ms notify loop per terminal. An inactive
        // window cannot show it, so the loop is dropped while the window is
        // away and restarted (from a lit cursor) when it comes back — but only
        // while the appearance preference asks an inactive window to stop
        // animating: with the preference off (the default) the cursor keeps
        // blinking in the background, and only the platform's own throttling
        // applies.
        let window_activation = cx.observe_window_activation(window, |this, window, cx| {
            let window_active = window.is_window_active();
            if window_active {
                this.cursor_visible = true;
                this.start_cursor_blink(cx);
            } else if motion::pauses_while_inactive(!window_active) {
                this.blink_task = None;
            }
        });
        let mut this = Self {
            transport,
            background: cx.background_executor().clone(),
            workspace_root,
            workspace_id,
            tabs,
            active_tab,
            search_input,
            search_open: false,
            search_matches: Vec::new(),
            last_search_refresh: None,
            active_search_match: 0,
            marked_text: None,
            selection_anchor: None,
            selection_text: None,
            active_hyperlink: None,
            hovered_link: None,
            row_links: std::cell::RefCell::new(None),
            scrollbar_drag: false,
            focus,
            cell_metrics,
            scroll_remainder: 0.0,
            glyph_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            dirty_rows: std::cell::RefCell::new(BTreeSet::new()),
            row_layouts: std::cell::RefCell::new(RowLayoutCache::default()),
            scroll_generation: 0,
            resize,
            resize_task: None,
            grid_bounds: None,
            last_grid_bounds: None,
            desired_grid_size: None,
            poll_task: None,
            blink_task: None,
            foreground_active: mode == TerminalSurfaceMode::Standalone,
            idle_poll_count: 0,
            cursor_visible: true,
            note,
            physical_marker_shortcut,
            command_submitted: false,
            command_marker_observed: false,
            ime_commits: 0,
            last_error_code,
            mode,
            owned_terminal_ids,
            _subscriptions: vec![focus_in, focus_out, window_activation],
            #[cfg(test)]
            input_log: Vec::new(),
            #[cfg(test)]
            mouse_log: Vec::new(),
        };
        this.install_search_subscription(window, cx);
        if this.foreground_active {
            this.start_polling(cx);
            this.start_cursor_blink(cx);
        }
        if this.foreground_active {
            let focus = this.focus.clone();
            cx.on_next_frame(window, move |_, window, cx| {
                focus.focus(window, cx);
            });
        }
        this
    }

    pub fn physical_submit_marker(&mut self, cx: &mut Context<Self>) {
        if self.command_submitted {
            return;
        }
        let mut bytes = self
            .encode_active_paste(PHYSICAL_MARKER_COMMAND)
            .unwrap_or_default();
        bytes.extend(
            self.encode_active_key(TerminalKey::Enter, TerminalModifiers::default())
                .unwrap_or_else(|| b"\r".to_vec()),
        );
        if self.write_active(&bytes, cx).is_ok() {
            self.command_submitted = true;
            cx.notify();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    #[allow(dead_code)]
    pub(crate) fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.foreground_active == active {
            return;
        }
        self.foreground_active = active;
        self.idle_poll_count = 0;
        if active {
            self.start_polling(cx);
            self.start_cursor_blink(cx);
        } else {
            self.poll_task = None;
            self.blink_task = None;
            if !self.cursor_visible {
                self.cursor_visible = true;
                cx.notify();
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn replace_embedded_session(
        &mut self,
        session: TerminalSession,
        cx: &mut Context<Self>,
    ) {
        if self.mode != TerminalSurfaceMode::Composer {
            return;
        }
        self.poll_task = None;
        self.tabs = vec![TerminalTab::new(session)];
        self.active_tab = Some(0);
        self.last_grid_bounds = None;
        self.note = "Terminal shell switched".into();
        self.last_error_code = None;
        self.clear_interaction_state();
        self.start_polling(cx);
        cx.notify();
    }

    pub fn physical_observation(&self) -> TerminalPhysicalObservation {
        let active = self.active_tab.and_then(|index| self.tabs.get(index));
        TerminalPhysicalObservation {
            tab_count: self.tabs.len(),
            pty_created: !self.tabs.is_empty(),
            active_running: active.is_some_and(|tab| tab.session.status == TerminalStatus::Running),
            command_submitted: self.command_submitted,
            command_marker_observed: self.command_marker_observed,
            non_blank_cells: active.map_or(0, |tab| {
                tab.frame
                    .cells()
                    .filter(|cell| !cell.text.trim().is_empty() && !cell.hidden)
                    .count()
            }),
            styled_cells: active.map_or(0, |tab| {
                tab.frame
                    .cells()
                    .filter(|cell| {
                        cell.bold
                            || cell.italic
                            || cell.underline
                            || cell.inverse
                            || cell.hyperlink.is_some()
                            || !matches!(cell.foreground, TerminalCellColor::Named { index: 256 })
                    })
                    .count()
            }),
            cursor_present: active.and_then(|tab| tab.frame.cursor()).is_some(),
            history_lines: active.map_or(0, |tab| tab.frame.history_lines()),
            rows: active.map_or(0, |tab| tab.frame.rows()),
            columns: active.map_or(0, |tab| tab.frame.columns()),
            ingested_bytes: active.map_or(0, |tab| tab.frame.ingested_bytes()),
            full_repaints: active.map_or(0, |tab| tab.full_repaints),
            partial_repaints: active.map_or(0, |tab| tab.partial_repaints),
            sequence_gaps: active.map_or(0, |tab| tab.sequence_gaps),
            selection_copy_available: self.selection_text.is_some(),
            search_match_count: self.search_matches.len(),
            ime_commits: self.ime_commits,
            last_error_code: self.last_error_code,
        }
    }

    fn install_search_subscription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.subscribe_in(
            &self.search_input,
            window,
            |this, _, event, _, cx| match event {
                InputEvent::Change => this.refresh_search(cx),
                InputEvent::PressEnter { .. } => this.select_next_search_match(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        ));
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        if self.poll_task.is_some() || !self.foreground_active {
            return;
        }
        let background = cx.background_executor().clone();
        self.poll_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            background.timer(TERMINAL_ACTIVE_POLL_INTERVAL).await;
            loop {
                let Ok(work) = entity.update(cx, |this, _| {
                    this.foreground_active
                        .then(|| (this.poll_work(), this.scroll_generation))
                }) else {
                    break;
                };
                let Some((work, scroll_generation)) = work else {
                    break;
                };
                if work.is_empty() {
                    background.timer(TERMINAL_IDLE_POLL_INTERVAL).await;
                    continue;
                }
                let results = background
                    .spawn(async move {
                        let mut results = Vec::with_capacity(work.len());
                        for item in work {
                            results.push(run_poll_work(item).await);
                        }
                        results
                    })
                    .await;
                let Ok(interval) = entity.update(cx, |this, cx| {
                    if !this.foreground_active {
                        return None;
                    }
                    // The reader scrolled while this poll was in flight, so the
                    // frames it carries describe a viewport that is no longer on
                    // screen. Applying them would scroll the terminal back and
                    // then forward again.
                    //
                    // The frames cannot simply be dropped, though: polling a
                    // terminal consumes its damage, so the rows this pass would
                    // have repainted are never reported again. The next frame
                    // would then carry only the newer damage, and the rows the
                    // dropped pass covered would keep their pre-scroll contents
                    // underneath the scrolled ones — which is exactly the
                    // duplicated block this guards against. So the result is
                    // still applied, and the cache is forced through a full
                    // repaint to pick up everything the dropped viewport owned.
                    let scrolled = this.scroll_generation != scroll_generation;
                    if scrolled {
                        for result in &results {
                            if let Some(tab) = this
                                .tabs
                                .iter_mut()
                                .find(|tab| tab.session.id == result.terminal_id)
                            {
                                tab.frame.force_full_repaint();
                            }
                        }
                    }
                    let changed = this.apply_poll_results(results, cx);
                    if changed {
                        this.idle_poll_count = 0;
                    } else {
                        this.idle_poll_count = this.idle_poll_count.saturating_add(1);
                    }
                    Some(terminal_poll_interval(this.idle_poll_count))
                }) else {
                    break;
                };
                let Some(interval) = interval else {
                    break;
                };
                background.timer(interval).await;
            }
        }));
    }

    fn start_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if self.blink_task.is_some() || !self.foreground_active {
            return;
        }
        let background = cx.background_executor().clone();
        self.blink_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                background.timer(TERMINAL_CURSOR_BLINK_INTERVAL).await;
                if entity
                    .update(cx, |this, cx| {
                        if !this.foreground_active {
                            return;
                        }
                        let blinking = this
                            .active_tab
                            .and_then(|index| this.tabs.get(index))
                            .and_then(|tab| tab.frame.cursor())
                            .is_some_and(|cursor| cursor.blinking);
                        let visible = !blinking || !this.cursor_visible;
                        if visible != this.cursor_visible {
                            this.cursor_visible = visible;
                            // The block cursor inverts the cell it covers, so
                            // blinking changes that row's ink without any
                            // output having arrived.
                            if let Some(cursor) = this
                                .active_tab
                                .and_then(|index| this.tabs.get(index))
                                .and_then(|tab| tab.frame.cursor())
                            {
                                this.mark_rows_dirty([cursor.row]);
                            }
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn poll_work(&self) -> Vec<TerminalPollWork> {
        self.tabs
            .iter()
            .filter(|tab| tab.session.status == TerminalStatus::Running)
            .map(|tab| TerminalPollWork {
                terminal_id: tab.session.id.clone(),
                next_sequence: tab
                    .backend
                    .lock()
                    .map(|backend| backend.next_sequence())
                    .unwrap_or(1),
                transport: self.transport.clone(),
                backend: tab.backend.clone(),
            })
            .collect()
    }

    fn apply_poll_results(
        &mut self,
        results: Vec<TerminalPollResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut changed = false;
        // Replies are written before the tab borrow below so the transport call
        // does not have to borrow `self` immutably while a tab is borrowed
        // mutably.
        let replies = results
            .iter()
            .filter(|result| !result.pty_replies.is_empty())
            .map(|result| (result.terminal_id.clone(), result.pty_replies.clone()))
            .collect::<Vec<_>>();
        for (terminal_id, bytes) in replies {
            // The emulator answered a query the program is waiting on; the
            // transport owns the write side, so the reply leaves through it
            // rather than being dropped on the floor.
            self.send_terminal_bytes(terminal_id, bytes, cx);
        }
        // Host-addressed requests leave the tab loop as data: answering one
        // needs the clipboard or the theme, neither of which the loop can
        // borrow while a tab is mutably held.
        let mut hosts = Vec::new();
        let mut dirty_rows = BTreeSet::new();
        for result in results {
            let host = match &result.result {
                Ok(Some((outcome, _))) => outcome.host.clone(),
                _ => TerminalHostSignals::default(),
            };
            let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|tab| tab.session.id == result.terminal_id)
            else {
                continue;
            };
            if let Some(session) = result.session {
                changed |= tab.session.status != session.status || tab.session != session;
                tab.session = session;
            }
            match result.result {
                Ok(Some((outcome, frame))) => {
                    let previous_cursor = tab.frame.cursor();
                    let update = tab.frame.apply(&frame);
                    let cursor = tab.frame.cursor();
                    if update.full_repaint {
                        tab.full_repaints = tab.full_repaints.saturating_add(1);
                        dirty_rows.extend(0..frame.rows);
                    } else {
                        tab.partial_repaints = tab.partial_repaints.saturating_add(1);
                        dirty_rows.extend(update.changed_rows.iter().copied());
                    }
                    // The block cursor inverts a cell's ink, so the row it left
                    // and the row it entered were reshaped by the move even when
                    // no output touched them.
                    dirty_rows.extend(
                        [previous_cursor, cursor]
                            .into_iter()
                            .flatten()
                            .map(|cursor| cursor.row),
                    );
                    tab.changed_rows = tab
                        .changed_rows
                        .saturating_add(update.changed_rows.len() as u64);
                    if outcome.gap_detected {
                        tab.sequence_gaps = tab.sequence_gaps.saturating_add(1);
                    }
                    if let Some(title) = &host.title {
                        tab.osc_title = if title.is_empty() {
                            None
                        } else {
                            Some(title.clone())
                        };
                    }
                    changed = true;
                }
                Ok(None) => {}
                Err(message) => {
                    if self.note != message || self.last_error_code != Some("terminal_poll_failed")
                    {
                        self.note = message;
                        self.last_error_code = Some("terminal_poll_failed");
                        changed = true;
                    }
                }
            }
            if !host.is_empty() {
                hosts.push((result.terminal_id, host));
            }
        }
        self.dirty_rows.borrow_mut().extend(dirty_rows);
        for (terminal_id, host) in hosts {
            self.answer_host_signals(&terminal_id, host, cx);
        }
        if changed {
            self.refresh_marker_observation();
            self.refresh_search_if_due(cx);
            cx.notify();
        }
        changed
    }

    /// Acts on the requests the program addressed to the host.
    ///
    /// These are plain data out of the emulator, so the clipboard read and the
    /// colour answers happen here, where the UI's clipboard and theme live.
    fn answer_host_signals(
        &mut self,
        terminal_id: &TerminalId,
        host: TerminalHostSignals,
        cx: &mut Context<Self>,
    ) {
        if let Some(encoded) = host.clipboard_store
            && let Ok(bytes) = BASE64_STANDARD.decode(encoded.as_bytes())
            && let Ok(text) = String::from_utf8(bytes)
            && text.len() <= 1024 * 1024
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.note = "Program set the clipboard".into();
        }
        if let Some(selection) = host.clipboard_read {
            let text = cx
                .read_from_clipboard()
                .and_then(|clipboard| clipboard.text())
                .unwrap_or_default();
            let encoded = BASE64_STANDARD.encode(text);
            let reply = format!("\x1b]52;{selection};{encoded}\x1b\\");
            self.send_terminal_bytes(terminal_id.clone(), reply.into_bytes(), cx);
        }
        if !host.color_queries.is_empty() {
            let mut reply = String::new();
            for query in host.color_queries {
                match query {
                    TerminalColorQuery::Palette { index } => {
                        let rgb = terminal_hsla_to_xterm(indexed_terminal_color(index));
                        reply.push_str(&format!("\x1b]4;{index};rgb:{rgb}\x1b\\"));
                    }
                    TerminalColorQuery::Foreground => {
                        let rgb = terminal_hsla_to_xterm(cx.theme().foreground);
                        reply.push_str(&format!("\x1b]10;rgb:{rgb}\x1b\\"));
                    }
                    TerminalColorQuery::Background => {
                        let rgb = terminal_hsla_to_xterm(cx.theme().background);
                        reply.push_str(&format!("\x1b]11;rgb:{rgb}\x1b\\"));
                    }
                    TerminalColorQuery::Cursor => {
                        let rgb = terminal_hsla_to_xterm(cx.theme().caret);
                        reply.push_str(&format!("\x1b]12;rgb:{rgb}\x1b\\"));
                    }
                }
            }
            if !reply.is_empty() {
                self.send_terminal_bytes(terminal_id.clone(), reply.into_bytes(), cx);
            }
        }
    }

    fn refresh_marker_observation(&mut self) {
        if self.command_marker_observed || !self.command_submitted {
            return;
        }
        self.command_marker_observed = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| {
                let mut text = String::new();
                for cell in tab.frame.cells() {
                    text.push_str(&cell.text);
                }
                text.contains("vibex-native-content-ok")
            });
    }

    fn schedule_resize(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let terminal_id = tab.session.id.clone();
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return;
        }
        let cell_metrics = measured_terminal_cell_metrics(cx.text_system().as_ref(), cx, self.mode);
        if self.cell_metrics != cell_metrics {
            self.cell_metrics = cell_metrics;
            self.resize = TerminalResizeCoordinator::new(cell_metrics);
            self.resize_task = None;
            self.last_grid_bounds = None;
        }
        // Remember the box even when the scheduler below has already recorded
        // it: a response to an older request must be able to tell that the
        // grid has moved on and ask for this one instead.
        self.desired_grid_size = Some((terminal_id.as_str().to_string(), width, height));
        let bounds = (
            terminal_id.as_str().to_string(),
            width.round().max(1.0) as u32,
            height.round().max(1.0) as u32,
        );
        if self.last_grid_bounds.as_ref() == Some(&bounds) {
            return;
        }
        if self
            .last_grid_bounds
            .as_ref()
            .is_some_and(|(previous_id, _, _)| previous_id != terminal_id.as_str())
        {
            self.resize = TerminalResizeCoordinator::new(self.cell_metrics);
        }
        self.last_grid_bounds = Some(bounds);
        let _ = self.resize.observe(terminal_id.clone(), width, height);
        let background = cx.background_executor().clone();
        self.resize_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            background.timer(TERMINAL_RESIZE_DEBOUNCE).await;
            let _ = entity.update(cx, |this, cx| {
                this.resize_task = None;
                this.commit_resize(terminal_id, width, height, cx)
            });
        }));
    }

    fn commit_resize(
        &mut self,
        terminal_id: TerminalId,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) {
        let Ok(Some(request)) = self.resize.observe(terminal_id.clone(), width, height) else {
            return;
        };
        let transport = self.transport.clone();
        let resize_request = request.clone();
        let future = Box::pin(async move { transport.resize_terminal(&resize_request).await });
        self.run_transport_operation(
            future,
            move |this, result, cx| {
                match result {
                    Ok(session) => {
                        if let Some(tab) = this
                            .tabs
                            .iter_mut()
                            .find(|tab| tab.session.id == terminal_id)
                        {
                            tab.session = session;
                            if let Ok(mut backend) = tab.backend.lock() {
                                backend.resize(request.rows, request.cols);
                                let frame = backend.frame();
                                tab.frame.force_full_repaint();
                                tab.frame.apply(&frame);
                            }
                        }
                        this.mark_all_rows_dirty(request.rows);
                        this.note = format!("{} rows x {} columns", request.rows, request.cols);
                        this.last_error_code = None;
                        this.reconcile_resize_after_response(
                            &terminal_id,
                            request.rows,
                            request.cols,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.note = format!("Resize failed: {}", error.message);
                        this.last_error_code = Some("terminal_resize_failed");
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Re-checks the terminal against the grid after a resize response.
    ///
    /// A response can describe a size the surface has already left behind: the
    /// terminal panel animates through small heights while it closes, and a
    /// request sent during that transit can be answered after the panel has
    /// opened again. Applying that response leaves the PTY one row tall while
    /// the grid is full, and the debounced scheduler will not send the current
    /// size because it already scheduled that box. Ask for the box on screen
    /// instead.
    fn reconcile_resize_after_response(
        &mut self,
        terminal_id: &TerminalId,
        rows: u16,
        cols: u16,
        cx: &mut Context<Self>,
    ) {
        let Some((desired_terminal, width, height)) = self.desired_grid_size.clone() else {
            return;
        };
        if desired_terminal != terminal_id.as_str() {
            return;
        }
        let Ok((desired_rows, desired_cols)) = self.cell_metrics.dimensions(width, height) else {
            return;
        };
        if (desired_rows, desired_cols) == (rows, cols) {
            return;
        }
        // The coordinator recorded the stale request as the last one sent, so
        // a plain commit would be suppressed as a duplicate. Start it over and
        // take both observations a resize needs in one step. The pending
        // debounce belongs to whichever box was current when it was scheduled,
        // so it is obsolete once this request goes out.
        self.resize_task = None;
        self.last_grid_bounds = Some((
            desired_terminal,
            width.round().max(1.0) as u32,
            height.round().max(1.0) as u32,
        ));
        self.resize = TerminalResizeCoordinator::new(self.cell_metrics);
        let _ = self.resize.observe(terminal_id.clone(), width, height);
        self.commit_resize(terminal_id.clone(), width, height, cx);
    }

    fn active_runtime(&self) -> Option<(TerminalId, SharedTerminalBackend)> {
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| (tab.session.id.clone(), tab.backend.clone()))
    }

    fn horizontal_padding(&self) -> f32 {
        self.mode.horizontal_padding()
    }

    fn vertical_padding(&self) -> f32 {
        self.mode.vertical_padding()
    }

    /// Runs a transport operation and applies its result to the surface.
    ///
    /// The operation runs on the foreground executor so transport calls stay
    /// ordered with the input that triggered them, and the state transition
    /// happens once the authority answers.
    fn run_transport_operation<T, A>(
        &mut self,
        future: TerminalTransportFuture<'static, T>,
        apply: A,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        A: FnOnce(&mut Self, TerminalTransportResult<T>, &mut Context<Self>) + 'static,
    {
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| apply(this, result, cx));
        })
        .detach();
    }

    /// Sends focus or mouse bytes, which have always ignored write failures.
    fn send_terminal_bytes(
        &mut self,
        terminal_id: TerminalId,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let transport = self.transport.clone();
        cx.spawn(async move |_: WeakEntity<Self>, _| {
            let _ = transport.write_bytes(&terminal_id, &bytes).await;
        })
        .detach();
    }

    fn record_terminal_write_failure(&mut self, error: TerminalTransportError) {
        self.note = format!("Terminal write failed: {}", error.message);
        self.last_error_code = Some("terminal_write_failed");
    }

    fn write_active(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<(), ()> {
        let Some((terminal_id, _)) = self.active_runtime() else {
            self.note = "No active terminal".into();
            self.last_error_code = Some("terminal_not_found");
            return Err(());
        };
        #[cfg(test)]
        self.input_log.push(bytes.to_vec());
        let transport = self.transport.clone();
        let bytes = bytes.to_vec();
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            if let Err(error) = transport.write_bytes(&terminal_id, &bytes).await {
                let _ = entity.update(cx, |this, cx| {
                    this.record_terminal_write_failure(error);
                    cx.notify();
                });
            }
        })
        .detach();
        self.wake_polling_after_input(cx);
        Ok(())
    }

    fn wake_polling_after_input(&mut self, cx: &mut Context<Self>) {
        if !self.foreground_active || self.idle_poll_count < TERMINAL_IDLE_POLL_THRESHOLD {
            return;
        }
        self.idle_poll_count = 0;
        self.poll_task = None;
        self.start_polling(cx);
    }

    fn encode_active_key(&self, key: TerminalKey, modifiers: TerminalModifiers) -> Option<Vec<u8>> {
        let (_, backend) = self.active_runtime()?;
        backend.lock().ok()?.encode_key(key, modifiers)
    }

    fn encode_active_paste(&self, text: &str) -> Option<Vec<u8>> {
        let (_, backend) = self.active_runtime()?;
        Some(backend.lock().ok()?.encode_paste(text))
    }

    fn commit_text(&mut self, value: &str, cx: &mut Context<Self>) -> bool {
        let Some((_, backend)) = self.active_runtime() else {
            return false;
        };
        let bytes = backend
            .lock()
            .ok()
            .map(|backend| backend.encode_text(value));
        let Some(bytes) = bytes else {
            return false;
        };
        match self.write_active(&bytes, cx) {
            Ok(()) => {
                if !value.is_ascii() {
                    self.ime_commits = self.ime_commits.saturating_add(1);
                }
                self.last_error_code.take().is_some()
            }
            Err(()) => true,
        }
    }

    fn send_key(&mut self, key: TerminalKey, modifiers: TerminalModifiers, cx: &mut Context<Self>) {
        if let Some(bytes) = self.encode_active_key(key, modifiers)
            && self.write_active(&bytes, cx).is_err()
        {
            cx.notify();
        }
    }

    fn send_tab(&mut self, _: &SendTab, _: &mut Window, cx: &mut Context<Self>) {
        self.send_key(TerminalKey::Tab, TerminalModifiers::default(), cx);
    }

    fn send_backtab(&mut self, _: &SendBacktab, _: &mut Window, cx: &mut Context<Self>) {
        self.send_key(
            TerminalKey::Tab,
            TerminalModifiers {
                shift: true,
                ..TerminalModifiers::default()
            },
            cx,
        );
    }

    fn send_focus(&mut self, focused: bool, cx: &mut Context<Self>) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let sequence = backend
            .lock()
            .ok()
            .and_then(|backend| backend.encode_focus(focused).map(<[u8]>::to_vec));
        if let Some(sequence) = sequence {
            self.send_terminal_bytes(terminal_id, sequence, cx);
        }
    }

    fn new_tab(&mut self, shell: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.tabs.len().saturating_add(1);
        let transport = self.transport.clone();
        let workspace_root = self.workspace_root.clone();
        let request = TerminalCreateRequest {
            workspace_id: self.workspace_id.clone(),
            title: Some(format!("Terminal {number}")),
            shell,
            cwd: Some(self.workspace_root.display().to_string()),
            rows: 20,
            cols: 100,
        };
        let focus = self.focus.clone();
        let window_handle = window.window_handle();
        let future =
            Box::pin(async move { transport.create_terminal(&workspace_root, request).await });
        self.run_transport_operation(
            future,
            move |this, result, cx| {
                match result {
                    Ok(session) => {
                        this.owned_terminal_ids
                            .insert(session.id.as_str().to_string());
                        this.tabs.push(TerminalTab::new(session));
                        this.active_tab = Some(this.tabs.len() - 1);
                        this.clear_interaction_state();
                        this.last_error_code = None;
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            focus.focus(window, cx);
                        });
                    }
                    Err(error) => {
                        this.note = format!("New terminal failed: {}", error.message);
                        this.last_error_code = Some("terminal_create_failed");
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active_tab = Some(index);
        self.clear_interaction_state();
        self.last_grid_bounds = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            return;
        };
        let tab = self.tabs.remove(index);
        self.owned_terminal_ids.remove(tab.session.id.as_str());
        let transport = self.transport.clone();
        let terminal_id = tab.session.id;
        cx.background_spawn(async move {
            let _ = transport.close_terminal(&terminal_id).await;
        })
        .detach();
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(index.min(self.tabs.len() - 1))
        };
        self.clear_interaction_state();
        if self.tabs.is_empty() {
            self.new_tab(None, window, cx);
        } else {
            self.last_grid_bounds = None;
            cx.notify();
        }
    }

    fn restart_active(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            return;
        };
        let session = self.tabs[index].session.clone();
        let transport = self.transport.clone();
        let workspace_root = self.workspace_root.clone();
        let future = Box::pin(async move {
            let _ = transport.close_terminal(&session.id).await;
            transport.restore_terminal(&workspace_root, session).await
        });
        self.run_transport_operation(
            future,
            move |this, result, cx| {
                match result {
                    Ok(session) => {
                        if let Some(tab) = this.tabs.get_mut(index) {
                            *tab = TerminalTab::new(session);
                        }
                        this.note = "Terminal restarted with the same Vibex id".into();
                        this.last_error_code = None;
                    }
                    Err(error) => {
                        this.note = format!("Restart failed: {}", error.message);
                        this.last_error_code = Some("terminal_restart_failed");
                    }
                }
                this.clear_interaction_state();
                cx.notify();
            },
            cx,
        );
    }

    fn switch_shell(&mut self, shell: String, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            return;
        };
        let request = TerminalSwitchShellRequest {
            terminal_id: self.tabs[index].session.id.clone(),
            shell,
        };
        let transport = self.transport.clone();
        let future = Box::pin(async move { transport.switch_shell(request).await });
        self.run_transport_operation(
            future,
            move |this, result, cx| {
                match result {
                    Ok(session) => {
                        if let Some(tab) = this.tabs.get_mut(index) {
                            *tab = TerminalTab::new(session);
                        }
                        this.note =
                            "Shell switched without creating a second terminal domain".into();
                        this.last_error_code = None;
                    }
                    Err(error) => {
                        this.note = format!("Shell switch failed: {}", error.message);
                        this.last_error_code = Some("terminal_shell_switch_failed");
                    }
                }
                this.clear_interaction_state();
                cx.notify();
            },
            cx,
        );
    }

    fn clear_interaction_state(&mut self) {
        self.marked_text = None;
        self.selection_anchor = None;
        self.selection_text = None;
        self.active_hyperlink = None;
        self.hovered_link = None;
        self.row_links.borrow_mut().take();
        self.search_matches.clear();
        self.last_search_refresh = None;
        self.active_search_match = 0;
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = !self.search_open;
        if self.search_open {
            self.marked_text = None;
            window.invalidate_character_coordinates();
            self.search_input
                .update(cx, |input, cx| input.focus(window, cx));
            self.refresh_search(cx);
        } else {
            self.search_matches.clear();
            self.last_search_refresh = None;
            self.active_search_match = 0;
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Rescans every retained line, scrollback included.
    fn rescan_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        let matches = self
            .active_runtime()
            .and_then(|(_, backend)| {
                backend
                    .lock()
                    .ok()
                    .map(|backend| backend.search_scrollback(&query, false))
            })
            .unwrap_or_default();
        self.search_matches = matches;
        self.last_search_refresh = Some(Instant::now());
        self.active_search_match = self
            .active_search_match
            .min(self.search_matches.len().saturating_sub(1));
    }

    /// Rescans and moves the viewport to the active match.
    ///
    /// A query change (or the bar opening) calls this; the poll path uses
    /// [`Self::refresh_search_if_due`], which only keeps the count current so
    /// streaming output cannot pull the reader back to a match.
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        self.rescan_search(cx);
        self.apply_active_search_match(cx);
    }

    /// Rescans the buffer for the open bar, at most every
    /// [`TERMINAL_SEARCH_REFRESH_INTERVAL`].
    fn refresh_search_if_due(&mut self, cx: &mut Context<Self>) {
        if !self.search_open {
            return;
        }
        if self
            .last_search_refresh
            .is_some_and(|last| last.elapsed() < TERMINAL_SEARCH_REFRESH_INTERVAL)
        {
            return;
        }
        self.rescan_search(cx);
    }

    fn select_next_search_match(&mut self, cx: &mut Context<Self>) {
        if !self.search_matches.is_empty() {
            self.active_search_match = (self.active_search_match + 1) % self.search_matches.len();
            self.apply_active_search_match(cx);
        }
    }

    fn select_previous_search_match(&mut self, cx: &mut Context<Self>) {
        if !self.search_matches.is_empty() {
            self.active_search_match = self
                .active_search_match
                .checked_sub(1)
                .unwrap_or(self.search_matches.len() - 1);
            self.apply_active_search_match(cx);
        }
    }

    /// Scrolls to the active match's row and selects it.
    ///
    /// A match above the viewport used to be selected at a row that was not on
    /// screen, so the bar counted it while nothing moved. The row is converted
    /// through the display offset the match was found against, then the
    /// viewport is scrolled so the match sits a third of the way down — a
    /// little context above it instead of against the top edge.
    fn apply_active_search_match(&mut self, cx: &mut Context<Self>) {
        let Some(search_match) = self.search_matches.get(self.active_search_match).copied() else {
            return;
        };
        let Some((_, backend)) = self.active_runtime() else {
            return;
        };
        let located = {
            let Ok(mut backend) = backend.lock() else {
                return;
            };
            let history = backend.history_lines();
            let rows = usize::from(backend.rows());
            let offset = backend.display_offset();
            let line = search_match.line as i64;
            let history = history as i64;
            let current_row = line - history + offset as i64;
            if (0..rows as i64).contains(&current_row) {
                Some((current_row as u16, offset))
            } else {
                let desired_row = (rows / 3).min(rows.saturating_sub(1)) as i64;
                let target_offset = (history - line + desired_row).clamp(0, history) as usize;
                backend.scroll_to_offset(target_offset);
                let viewport_row = (line - history + target_offset as i64).max(0) as u16;
                Some((viewport_row, target_offset))
            }
        };
        let Some((row, _)) = located else {
            return;
        };
        if row >= self.active_rows() {
            return;
        }
        self.select_range(
            TerminalGridPoint {
                row,
                column: search_match.start_column,
            },
            TerminalGridPoint {
                row,
                column: search_match.end_column,
            },
            cx,
        );
        // A scroll the search performed is newer than any poll in flight, just
        // like a wheel scroll: the cache has to pick up the whole viewport.
        self.scroll_generation = self.scroll_generation.wrapping_add(1);
    }

    fn active_rows(&self) -> u16 {
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.frame.rows())
            .unwrap_or(1)
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text.clone() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.note = "Selection copied".into();
            cx.notify();
        }
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|clipboard| clipboard.text())
        else {
            self.note = "Clipboard does not contain text".into();
            cx.notify();
            return;
        };
        if let Some(bytes) = self.encode_active_paste(&text) {
            let _ = self.write_active(&bytes, cx);
            self.note = "Clipboard text sent using terminal paste mode".into();
            cx.notify();
        }
    }

    fn select_range(
        &mut self,
        start: TerminalGridPoint,
        end: TerminalGridPoint,
        cx: &mut Context<Self>,
    ) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let frame_and_text = backend.lock().ok().and_then(|mut backend| {
            backend
                .select_text(start, end)
                .ok()
                .map(|text| (backend.frame(), text))
        });
        if let Some((frame, text)) = frame_and_text {
            self.selection_text = Some(text);
            self.apply_local_frame(&terminal_id, frame);
            cx.notify();
        }
    }

    fn apply_local_frame(&mut self, terminal_id: &TerminalId, frame: TerminalFrameSnapshot) {
        if let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| &tab.session.id == terminal_id)
        {
            tab.frame.force_full_repaint();
            let update = tab.frame.apply(&frame);
            tab.full_repaints = tab
                .full_repaints
                .saturating_add(u64::from(update.full_repaint));
            tab.changed_rows = tab
                .changed_rows
                .saturating_add(update.changed_rows.len() as u64);
        }
        self.mark_all_rows_dirty(frame.rows);
    }

    /// Marks rows whose shaped layout is stale.
    fn mark_rows_dirty(&mut self, rows: impl IntoIterator<Item = u16>) {
        self.dirty_rows.borrow_mut().extend(rows);
    }

    fn mark_all_rows_dirty(&mut self, rows: u16) {
        self.dirty_rows.borrow_mut().extend(0..rows);
    }

    fn on_cell_mouse_down(
        &mut self,
        point: TerminalGridPoint,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        let modes = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.frame.modes());
        if modes.is_some_and(|modes| modes.mouse_reporting) && !event.modifiers.shift {
            self.send_mouse(
                TerminalMouseKind::Press,
                mouse_button_code(event.button),
                point,
                terminal_modifiers(event.modifiers),
                cx,
            );
            return;
        }
        // Selection gestures: a double click takes the word under the pointer,
        // a triple click its whole line. Both defer to the emulator's own word
        // and line boundaries so the highlight and the copied text agree.
        match event.click_count {
            2 => {
                self.select_word(point, cx);
                return;
            }
            3 => {
                self.select_line(point, cx);
                return;
            }
            _ => {}
        }
        self.selection_anchor = Some(point);
        self.select_range(point, point, cx);
    }

    fn select_word(&mut self, point: TerminalGridPoint, cx: &mut Context<Self>) {
        self.select_with(point, cx, TerminalSurfaceBackend::select_word_at);
    }

    fn select_line(&mut self, point: TerminalGridPoint, cx: &mut Context<Self>) {
        self.select_with(point, cx, TerminalSurfaceBackend::select_line_at);
    }

    /// Runs one of the emulator's word/line selections and caches its frame.
    fn select_with(
        &mut self,
        point: TerminalGridPoint,
        cx: &mut Context<Self>,
        select: fn(
            &mut TerminalSurfaceBackend,
            TerminalGridPoint,
        ) -> vibex_core::VibexResult<String>,
    ) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let selected = backend.lock().ok().and_then(|mut backend| {
            select(&mut backend, point)
                .ok()
                .map(|text| (backend.frame(), text))
        });
        if let Some((frame, text)) = selected {
            self.selection_anchor = Some(point);
            self.selection_text = Some(text);
            self.apply_local_frame(&terminal_id, frame);
            cx.notify();
        }
    }

    fn on_cell_mouse_move(
        &mut self,
        point: TerminalGridPoint,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if event.dragging() {
            if let Some(anchor) = self.selection_anchor {
                self.select_range(anchor, point, cx);
            } else {
                self.send_mouse(
                    TerminalMouseKind::Move,
                    0,
                    point,
                    terminal_modifiers(event.modifiers),
                    cx,
                );
            }
        } else {
            let link = self.link_at(point);
            if self.hovered_link.as_ref().map(|(row, link)| (*row, link))
                != link.as_ref().map(|link| (point.row, link))
            {
                self.hovered_link = link.map(|link| (point.row, link));
                cx.notify();
            }
        }
    }

    fn on_cell_mouse_up(
        &mut self,
        point: TerminalGridPoint,
        hyperlink: Option<String>,
        event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        let modes = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.frame.modes());
        if modes.is_some_and(|modes| modes.mouse_reporting) && !event.modifiers.shift {
            self.send_mouse(
                TerminalMouseKind::Release,
                mouse_button_code(event.button),
                point,
                terminal_modifiers(event.modifiers),
                cx,
            );
        } else {
            // A cell's own `OSC 8` hyperlink wins over a URL detected in its
            // text: the program chose that target explicitly.
            let link = hyperlink.or_else(|| self.link_at(point).map(|link| link.url));
            if event.modifiers.secondary() && link.is_some() {
                self.active_hyperlink = link;
                self.open_hyperlink(cx);
            } else if self.selection_anchor == Some(point) {
                self.active_hyperlink = link;
            }
        }
        self.selection_anchor = None;
        cx.notify();
    }

    /// The detected link under a cell, if the row has one.
    fn link_at(&self, point: TerminalGridPoint) -> Option<RowLink> {
        let tab = self.active_tab.and_then(|index| self.tabs.get(index))?;
        let bytes = tab.frame.ingested_bytes();
        let columns = tab.frame.columns();
        let mut cached = self.row_links.borrow_mut();
        let stale = cached
            .as_ref()
            .is_none_or(|(row, cache_bytes, cache_columns, _)| {
                *row != point.row || *cache_bytes != bytes || *cache_columns != columns
            });
        if stale {
            let mut cells = Vec::new();
            for column in 0..tab.frame.columns() {
                let Some(cell) = tab.frame.cell(TerminalGridPoint {
                    row: point.row,
                    column,
                }) else {
                    continue;
                };
                if cell.wide_spacer || cell.text.is_empty() {
                    continue;
                }
                cells.push((column, cell.text.clone()));
            }
            *cached = Some((
                point.row,
                bytes,
                columns,
                terminal_links::find_links(&cells),
            ));
        }
        let (_, _, _, links) = cached.as_ref()?;
        terminal_links::link_at(links, point.column).cloned()
    }

    fn send_mouse(
        &mut self,
        kind: TerminalMouseKind,
        button: u8,
        point: TerminalGridPoint,
        modifiers: TerminalModifiers,
        cx: &mut Context<Self>,
    ) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let event = TerminalMouseEvent {
            kind,
            button,
            row: point.row,
            column: point.column,
            modifiers,
        };
        let pixel_mouse = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.frame.modes().sgr_pixels_mouse);
        let (cell_width, cell_height) =
            (self.cell_metrics.cell_width, self.cell_metrics.cell_height);
        let bytes = backend.lock().ok().and_then(|backend| {
            if pixel_mouse {
                backend.encode_mouse_pixels(event, cell_width, cell_height)
            } else {
                backend.encode_mouse(event)
            }
        });
        if let Some(bytes) = bytes {
            #[cfg(test)]
            self.mouse_log.push(event);
            self.send_terminal_bytes(terminal_id, bytes, cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = self.scroll_delta_lines(event);
        if lines == 0.0 {
            return;
        }
        let modes = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.frame.modes());
        if modes.is_some_and(|modes| modes.mouse_reporting) && !event.modifiers.shift {
            let kind = if lines > 0.0 {
                TerminalMouseKind::WheelUp
            } else {
                TerminalMouseKind::WheelDown
            };
            // The cell under the pointer, not the grid's origin: a full-screen
            // program hit-tests the wheel like any other mouse event, so it
            // scrolls only the region under the pointer and ignores a wheel
            // reported at the top-left corner. Sending every wheel as (0, 0) is
            // what made an Agent's transcript refuse to move.
            let point = self
                .cell_at_position(event.position)
                .unwrap_or_else(|| self.scroll_report_cell());
            self.send_mouse(kind, 0, point, terminal_modifiers(event.modifiers), cx);
            return;
        }
        // A full-screen program owns its screen and has no scrollback here. The
        // wheel is a scroll gesture the program never sees, so it becomes the
        // arrow keys the program already handles — the behaviour every terminal
        // calls alternate scroll.
        if modes.is_some_and(|modes| modes.alternate_screen) {
            let whole = self.accumulate_scroll_lines(lines);
            if whole == 0 {
                return;
            }
            let key = if whole > 0 {
                TerminalKey::Up
            } else {
                TerminalKey::Down
            };
            for _ in 0..whole.unsigned_abs().min(64) {
                self.send_key(key, TerminalModifiers::default(), cx);
            }
            return;
        }
        // Whole lines only: the terminal's display offset is integral, so a
        // fractional remainder is carried into the next event rather than
        // rounded away or multiplied into a fixed jump.
        let whole_lines = self.accumulate_scroll_lines(lines);
        if whole_lines == 0 {
            return;
        }
        self.scroll_lines(whole_lines, cx);
    }

    /// The scroll delta of one wheel event, in terminal lines.
    ///
    /// A line-granularity event already says how many lines it means; a pixel
    /// event is divided by the measured cell height so a trackpad's sub-row
    /// deltas add up to the number of rows actually travelled.
    fn scroll_delta_lines(&self, event: &ScrollWheelEvent) -> f32 {
        match event.delta {
            ScrollDelta::Lines(point) => point.y,
            ScrollDelta::Pixels(point) => {
                f32::from(point.y) / self.cell_metrics.cell_height.max(1.0)
            }
        }
    }

    /// Turn a fractional line delta into the whole lines to scroll now.
    fn accumulate_scroll_lines(&mut self, lines: f32) -> i32 {
        accumulate_scroll_lines(&mut self.scroll_remainder, lines)
    }

    /// Scrolls the active tab's viewport by whole lines and paints the result.
    fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) -> bool {
        if lines == 0 {
            return false;
        }
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return false;
        };
        let frame = backend.lock().ok().map(|mut backend| {
            backend.scroll(lines);
            backend.frame()
        });
        let Some(frame) = frame else {
            return false;
        };
        // A local scroll is newer than anything a poll in flight can carry, so
        // the cache is forced through a full repaint and the next poll result
        // is checked against this floor before it is applied.
        self.apply_local_frame(&terminal_id, frame);
        self.scroll_generation = self.scroll_generation.wrapping_add(1);
        cx.notify();
        true
    }

    /// Whether the active tab has scrollback the reader can reach.
    fn can_scroll_back(&self) -> bool {
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.frame.history_lines() > 0 && !tab.frame.modes().alternate_screen)
    }

    /// Scrolls by viewport pages; a positive count moves back into history,
    /// matching the sign a wheel's upward delta already uses.
    fn scroll_pages(&mut self, pages: i32, cx: &mut Context<Self>) {
        let rows = self.active_rows();
        let lines = pages.saturating_mul(i32::from(rows.saturating_sub(1)));
        self.scroll_lines(lines, cx);
    }

    fn scroll_to_history_edge(&mut self, top: bool, cx: &mut Context<Self>) {
        let offset = if top {
            self.active_tab
                .and_then(|index| self.tabs.get(index))
                .map(|tab| tab.frame.history_lines())
                .unwrap_or(0)
        } else {
            0
        };
        self.scroll_to_offset(offset, cx);
    }

    /// Scrolls the viewport so `offset` lines sit above it.
    fn scroll_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) -> bool {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return false;
        };
        let frame = backend.lock().ok().map(|mut backend| {
            backend.scroll_to_offset(offset);
            backend.frame()
        });
        let Some(frame) = frame else {
            return false;
        };
        self.apply_local_frame(&terminal_id, frame);
        self.scroll_generation = self.scroll_generation.wrapping_add(1);
        cx.notify();
        true
    }

    /// The scrollbar's track and thumb in window space, or `None` when there is
    /// no scrollback to show.
    fn scrollbar_geometry(&self) -> Option<TerminalScrollbarGeometry> {
        self.scrollbar_geometry_for(self.grid_bounds?)
    }

    fn scrollbar_geometry_for(&self, bounds: Bounds<Pixels>) -> Option<TerminalScrollbarGeometry> {
        let tab = self.active_tab.and_then(|index| self.tabs.get(index))?;
        let history = tab.frame.history_lines();
        if history == 0 || tab.frame.modes().alternate_screen {
            return None;
        }
        let track_height = f32::from(bounds.size.height);
        if track_height <= 0.0 || !track_height.is_finite() {
            return None;
        }
        let rows = f32::from(tab.frame.rows()).max(1.0);
        let thumb_height = (track_height * rows / (rows + history as f32))
            .max(TERMINAL_SCROLLBAR_MIN_THUMB.min(track_height));
        let travel = (track_height - thumb_height).max(0.0);
        let offset = tab.frame.display_offset().min(history);
        let progress = if history == 0 {
            1.0
        } else {
            1.0 - offset as f32 / history as f32
        };
        let track = Bounds::new(
            point(
                bounds.origin.x + bounds.size.width
                    - px(TERMINAL_SCROLLBAR_WIDTH + TERMINAL_SCROLLBAR_MARGIN),
                bounds.origin.y,
            ),
            size(px(TERMINAL_SCROLLBAR_WIDTH), bounds.size.height),
        );
        let thumb = Bounds::new(
            point(track.origin.x, track.origin.y + px(travel * progress)),
            size(px(TERMINAL_SCROLLBAR_WIDTH), px(thumb_height)),
        );
        Some(TerminalScrollbarGeometry { track, thumb })
    }

    fn scrollbar_hit(&self, position: Point<Pixels>) -> Option<TerminalScrollbarGeometry> {
        let geometry = self.scrollbar_geometry()?;
        let x = f32::from(position.x);
        let y = f32::from(position.y);
        let hit_left = f32::from(geometry.track.origin.x)
            - (TERMINAL_SCROLLBAR_HIT_WIDTH - TERMINAL_SCROLLBAR_WIDTH);
        let hit_right = f32::from(geometry.track.origin.x) + TERMINAL_SCROLLBAR_WIDTH;
        let top = f32::from(geometry.track.origin.y);
        let bottom = top + f32::from(geometry.track.size.height);
        (x >= hit_left && x <= hit_right && y >= top && y <= bottom).then_some(geometry)
    }

    /// Starts a scrollbar drag if `position` is in its band.
    fn begin_scrollbar_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(geometry) = self.scrollbar_hit(position) else {
            return false;
        };
        self.scrollbar_drag = true;
        let inside_thumb = position.y >= geometry.thumb.origin.y
            && position.y <= geometry.thumb.origin.y + geometry.thumb.size.height;
        if !inside_thumb {
            self.drag_scrollbar_to(position, geometry, cx);
        }
        true
    }

    fn drag_scrollbar(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if !self.scrollbar_drag {
            return false;
        }
        let Some(geometry) = self.scrollbar_geometry() else {
            self.scrollbar_drag = false;
            return false;
        };
        self.drag_scrollbar_to(position, geometry, cx);
        true
    }

    fn drag_scrollbar_to(
        &mut self,
        position: Point<Pixels>,
        geometry: TerminalScrollbarGeometry,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let history = tab.frame.history_lines();
        let track_height = f32::from(geometry.track.size.height);
        let thumb_height = f32::from(geometry.thumb.size.height);
        let travel = (track_height - thumb_height).max(0.0);
        if travel <= 0.0 || history == 0 {
            return;
        }
        let relative =
            (f32::from(position.y) - f32::from(geometry.track.origin.y) - thumb_height / 2.0)
                .clamp(0.0, travel);
        let offset = ((1.0 - relative / travel) * history as f32).round() as usize;
        self.scroll_to_offset(offset, cx);
    }

    fn end_scrollbar_drag(&mut self) {
        self.scrollbar_drag = false;
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if self.physical_marker_shortcut
            && !modifiers.modified()
            && key == "t"
            && !self.command_submitted
        {
            self.physical_submit_marker(cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("c") {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("v") {
            self.paste_clipboard(cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("f") {
            self.toggle_search(window, cx);
            cx.stop_propagation();
            return;
        }
        // Shift+PageUp/PageDown and Shift+Home/End reach the terminal's own
        // scrollback. They are only claimed when there is scrollback to reach,
        // so a program that wants the modified chord still receives it on a
        // live screen or in the alternate screen.
        if modifiers.shift && !modifiers.control && !modifiers.alt && self.can_scroll_back() {
            let handled = match key {
                "pageup" => {
                    self.scroll_pages(1, cx);
                    true
                }
                "pagedown" => {
                    self.scroll_pages(-1, cx);
                    true
                }
                "home" => {
                    self.scroll_to_history_edge(true, cx);
                    true
                }
                "end" => {
                    self.scroll_to_history_edge(false, cx);
                    true
                }
                _ => false,
            };
            if handled {
                cx.stop_propagation();
                return;
            }
        }
        let terminal_modifiers = terminal_modifiers(modifiers);
        let mapped = match key {
            "enter" => Some(TerminalKey::Enter),
            "backspace" => Some(TerminalKey::Backspace),
            "tab" => Some(TerminalKey::Tab),
            "escape" => Some(TerminalKey::Escape),
            "up" => Some(TerminalKey::Up),
            "down" => Some(TerminalKey::Down),
            "left" => Some(TerminalKey::Left),
            "right" => Some(TerminalKey::Right),
            "home" => Some(TerminalKey::Home),
            "end" => Some(TerminalKey::End),
            "pageup" => Some(TerminalKey::PageUp),
            "pagedown" => Some(TerminalKey::PageDown),
            "insert" => Some(TerminalKey::Insert),
            "delete" => Some(TerminalKey::Delete),
            value if value.len() > 1 && value.starts_with('f') => {
                value[1..].parse::<u8>().ok().map(TerminalKey::Function)
            }
            _ => None,
        };
        if let Some(key) = mapped {
            self.send_key(key, terminal_modifiers, cx);
            cx.stop_propagation();
        } else if (modifiers.control || modifiers.alt)
            && !event.prefer_character_input
            && let Some(character) = event
                .keystroke
                .key_char
                .as_ref()
                .and_then(|value| value.chars().next())
                .or_else(|| {
                    let mut characters = key.chars();
                    let character = characters.next()?;
                    characters.next().is_none().then_some(character)
                })
        {
            self.send_key(TerminalKey::Text(character), terminal_modifiers, cx);
            cx.stop_propagation();
        }
    }

    fn open_hyperlink(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self.active_hyperlink.clone() else {
            return;
        };
        let url = match validate_external_open_url(&url) {
            Ok(validated) => validated.url,
            Err(error) => {
                self.note = format!("Link rejected: {}", error.message);
                self.last_error_code = Some("terminal_hyperlink_invalid");
                cx.notify();
                return;
            }
        };
        match spawn_external_url(&url) {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.note = "External link open requested".into();
                self.last_error_code = None;
            }
            Err(error) => {
                self.note = format!("External link open failed: {error}");
                self.last_error_code = Some("terminal_hyperlink_open_failed");
            }
        }
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_selection = self.selection_text.is_some();
        let has_link = self.active_hyperlink.is_some();
        h_flex()
            .min_h(px(38.0))
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.mode.shows_chrome(), |toolbar| {
                toolbar
                    .child(
                        Button::new("terminal-new-tab")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("New terminal")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_tab(None, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("terminal-restart")
                            .small()
                            .ghost()
                            .icon(IconName::Replace)
                            .tooltip("Restart terminal")
                            .disabled(self.active_tab.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.restart_active(cx))),
                    )
                    .child(
                        Button::new("terminal-close")
                            .small()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Close terminal")
                            .disabled(self.active_tab.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.close_active_tab(window, cx)
                                }),
                            ),
                    )
            })
            .child(
                Button::new("terminal-search")
                    .small()
                    .ghost()
                    .icon(IconName::Search)
                    .tooltip("Search terminal")
                    .selected(self.search_open)
                    .disabled(self.active_tab.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))),
            )
            .child(
                Button::new("terminal-copy")
                    .small()
                    .ghost()
                    .label("Copy")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_selection(cx))),
            )
            .child(
                Button::new("terminal-paste")
                    .small()
                    .ghost()
                    .label("Paste")
                    .disabled(self.active_tab.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.paste_clipboard(cx))),
            )
            .when(has_link, |toolbar| {
                toolbar.child(
                    Button::new("terminal-open-link")
                        .small()
                        .ghost()
                        .icon(IconName::ExternalLink)
                        .label("Open link")
                        .on_click(cx.listener(|this, _, _, cx| this.open_hyperlink(cx))),
                )
            })
            .child(div().flex_1())
            .children(available_shells().into_iter().map(|(label, path)| {
                Button::new(format!("terminal-shell-{label}"))
                    .small()
                    .ghost()
                    .label(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.switch_shell(path.clone(), cx)),
                    )
            }))
            .into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_tab;
        // The folder bar draws the selected tab's shape; the plate behind it is
        // what the shape rises out of. Every tab strip in the app takes the same
        // plate, so the editor and the terminal read as one kind of strip.
        h_flex()
            .id("terminal-tab-strip")
            .h(px(36.0))
            .flex_none()
            .min_w_0()
            .bg(theme::tab_strip_plate(cx.theme()))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                TabBar::new("terminal-tab-bar")
                    .folder()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .bg(cx.theme().transparent)
                    .when_some(active, |bar, index| bar.selected_index(index))
                    .children(self.tabs.iter().map(|tab| {
                        let status = terminal_status_label(tab.session.status);
                        let title = tab
                            .osc_title
                            .clone()
                            .unwrap_or_else(|| tab.session.title.clone());
                        // `Tab` renders `icon` *instead of* `label` when both are
                        // set, so the session title rides the children slot and
                        // `aria_label` carries the accessible name.
                        Tab::new()
                            .aria_label(title.clone())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(Icon::new(IconName::SquareTerminal).small())
                                    .child(title),
                            )
                            .suffix(div().pl_1().text_xs().child(status))
                    }))
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        this.select_tab(*index, window, cx)
                    })),
            )
            .into_any_element()
    }

    fn render_search(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().child(Input::new(&self.search_input).small()))
            .child(
                div()
                    .w(px(72.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.search_matches.is_empty() {
                        "0 matches".into()
                    } else {
                        format!(
                            "{} / {}",
                            self.active_search_match + 1,
                            self.search_matches.len()
                        )
                    }),
            )
            .child(
                Button::new("terminal-search-previous")
                    .small()
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .tooltip("Previous match")
                    .disabled(self.search_matches.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.select_previous_search_match(cx))),
            )
            .child(
                Button::new("terminal-search-next")
                    .small()
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .tooltip("Next match")
                    .disabled(self.search_matches.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.select_next_search_match(cx))),
            )
            .into_any_element()
    }

    fn render_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.active_tab.and_then(|index| self.tabs.get(index)) else {
            return Empty::new()
                .header(
                    EmptyHeader::new()
                        .description(EmptyDescription::new().child("Terminal is unavailable")),
                )
                .into_any_element();
        };
        let resize_entity = cx.weak_entity();
        let input_entity = cx.entity();
        let prepaint_entity = cx.entity();
        let paint_entity = cx.entity();
        let input_focus = self.focus.clone();
        let cursor = tab.frame.cursor();
        let marked_text = self.marked_text.clone();
        let horizontal_padding = self.horizontal_padding();
        let vertical_padding = self.vertical_padding();
        let cell_metrics = measured_terminal_cell_metrics(cx.text_system().as_ref(), cx, self.mode);
        let terminal_opacity = crate::typography::terminal_opacity(cx);
        v_flex()
            .id("terminal-grid")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .when(self.mode.paints_surface_background(), |grid| {
                grid.bg(cx.theme().background.opacity(terminal_opacity))
            })
            .px(px(horizontal_padding))
            .py(px(vertical_padding))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .line_height(px(cell_metrics.cell_height))
            .font_weight(code_font_weight(cx))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // The scrollbar overlays the last cells, so it has to claim
                    // the press before the cell under it selects text.
                    if this.begin_scrollbar_drag(event.position, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    this.end_scrollbar_drag();
                    this.focus.focus(window, cx);
                    if let Some(point) = this.cell_at_position(event.position) {
                        this.on_cell_mouse_down(point, event, window, cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.drag_scrollbar(event.position, cx) {
                    return;
                }
                match this.cell_at_position(event.position) {
                    Some(point) => this.on_cell_mouse_move(point, event, cx),
                    None => {
                        if this.hovered_link.take().is_some() {
                            cx.notify();
                        }
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if this.scrollbar_drag {
                        this.end_scrollbar_drag();
                        return;
                    }
                    let Some(point) = this.cell_at_position(event.position) else {
                        this.selection_anchor = None;
                        return;
                    };
                    let hyperlink = this
                        .active_tab
                        .and_then(|index| this.tabs.get(index))
                        .and_then(|tab| tab.frame.cell(point))
                        .and_then(|cell| cell.hyperlink.clone());
                    this.on_cell_mouse_up(point, hyperlink, event, cx);
                }),
            )
            // The whole grid is one element: cells are painted by hand instead of
            // becoming a div, three mouse listeners and a text node each. Rebuilding
            // thousands of elements per frame was what dropped the workbench frame
            // rate whenever a terminal was on screen.
            //
            // The canvas fills the pane rather than being pinned to the current
            // row and column count. Pinning it meant the painted grid kept its
            // old size for as long as the resize round trip took, so dragging a
            // window left the terminal behind; filling the pane lets the grid
            // follow the pointer and lets prepaint derive the new size from the
            // box that is on screen right now.
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let layout =
                            shape_terminal_grid(&prepaint_entity, cell_metrics, window, cx);
                        // The canvas is the content box, which is where the first
                        // cell starts. Recording its bounds here — rather than
                        // from the padding-box listener below — keeps pointer
                        // mapping and the scrollbar in the same space as the
                        // painted cells.
                        let _ = prepaint_entity.update(cx, |surface, _| {
                            surface.grid_bounds = Some(bounds);
                        });
                        layout
                    },
                    move |bounds, layout, window, cx| {
                        paint_terminal_grid(bounds, &layout, &paint_entity, window, cx)
                    },
                )
                .size_full(),
            )
            .when_some(cursor.zip(marked_text), |grid, (cursor, marked_text)| {
                grid.child(
                    div()
                        .absolute()
                        .left(px(
                            horizontal_padding + f32::from(cursor.column) * cell_metrics.cell_width
                        ))
                        .top(px(
                            vertical_padding + f32::from(cursor.row) * cell_metrics.cell_height
                        ))
                        .min_w(px(cell_metrics.cell_width))
                        .h(px(cell_metrics.cell_height))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .bg(cx.theme().background)
                        .text_color(cx.theme().foreground)
                        .underline()
                        .child(marked_text),
                )
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &input_focus,
                            ElementInputHandler::new(bounds, input_entity),
                            cx,
                        );
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_prepaint(move |bounds, _, cx| {
                let width = f32::from(bounds.size.width);
                let height = f32::from(bounds.size.height);
                let _ = resize_entity.update(cx, |this, cx| {
                    this.schedule_resize(width, height, cx);
                });
            })
            .into_any_element()
    }

    /// The cell a wheel report falls back to when the pointer is outside them.
    ///
    /// The pointer can sit in the grid's own inset, or past the last row of a
    /// program that has not painted its full height. The middle of the grid is
    /// the safe guess: a program's top-left corner is usually its header, and a
    /// wheel reported there moves nothing, which is the bug this avoids.
    fn scroll_report_cell(&self) -> TerminalGridPoint {
        let (rows, columns) = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| (tab.frame.rows(), tab.frame.columns()))
            .unwrap_or((1, 1));
        TerminalGridPoint {
            row: rows.saturating_sub(1) / 2,
            column: columns.saturating_sub(1) / 2,
        }
    }

    /// The grid point under a window-space position, or `None` outside the cells.
    ///
    /// `grid_bounds` is the grid's content box, which is where the cells start,
    /// so pointer positions need no padding adjustment. Cell geometry comes from
    /// the last prepaint, so a click that arrives before the grid has ever been
    /// painted is ignored rather than guessed.
    fn cell_at_position(&self, position: Point<Pixels>) -> Option<TerminalGridPoint> {
        let bounds = self.grid_bounds?;
        let tab = self.active_tab.and_then(|index| self.tabs.get(index))?;
        let metrics = self.cell_metrics;
        if !metrics.cell_width.is_finite()
            || !metrics.cell_height.is_finite()
            || metrics.cell_width <= 0.0
            || metrics.cell_height <= 0.0
        {
            return None;
        }
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let column = (x / metrics.cell_width).floor();
        let row = (y / metrics.cell_height).floor();
        if column >= f32::from(tab.frame.columns()) || row >= f32::from(tab.frame.rows()) {
            return None;
        }
        Some(TerminalGridPoint {
            row: row.max(0.0) as u16,
            column: column.max(0.0) as u16,
        })
    }
}

/// A run of adjacent cells that share one style, shaped as a single line.
///
/// The shaper lays out a whole run at once, so ligatures, combining marks and
/// zero-width emoji sequences keep the relative positions the font gave them.
/// Splitting a row into runs only where the style actually changes is what keeps
/// the shaped text small: a screen of prose is a handful of runs, not 80 cells.
#[derive(Clone)]
struct TerminalTextRun {
    line: ShapedLine,
    /// Byte offset of the run's first column inside `line.text`.
    column: u16,
    /// Columns the run occupies; wide cells count twice.
    columns: u16,
    /// Set when the run is a single symbol glyph fitted to its cell's outline
    /// rather than sitting on the text baseline.
    symbol: Option<SymbolFit>,
}

/// How a symbol glyph was scaled and where its line box sits.
#[derive(Clone, Copy)]
struct SymbolFit {
    line_height: f32,
}

/// Everything the grid's paint pass needs from its prepaint pass.
struct TerminalGridLayout {
    rows: Vec<Arc<TerminalRowLayout>>,
    /// Cell geometry measured from the actual font, not from a design constant.
    metrics: TerminalCellMetrics,
}

impl TerminalGridLayout {
    fn empty(metrics: TerminalCellMetrics) -> Self {
        Self {
            rows: Vec::new(),
            metrics,
        }
    }
}

/// A run of adjacent cells painted as the same filled subcell rectangle.
#[derive(Clone, Copy)]
struct TerminalRect {
    row: u16,
    column: u16,
    /// How many adjacent cells this run covers.
    columns: u16,
    /// The filled area in subcell units, within a cell.
    fill: TerminalSubcellFill,
    color: Hsla,
    /// `1.0` for a solid block; the shade ramp uses a fraction of the cell.
    opacity: f32,
    /// Whether this rectangle may extend into the next cell when it repeats
    /// there.
    ///
    /// Blocks are edge-to-edge and should merge into one quad. A Braille dot is
    /// isolated inside its cell, so merging would bridge the gap between two
    /// neighbouring dots.
    merge_adjacent: bool,
}

/// A filled area inside one cell, in subcell units: `(x, y, columns, lines)`.
type TerminalSubcellFill = (u16, u16, u16, u16);

/// The rectangles a painted character fills, and the ink fraction they carry.
struct TerminalCellGeometry {
    fills: Vec<TerminalSubcellFill>,
    opacity: f32,
    /// Whether a repeated fill may be merged into the next cell's rectangle.
    merge_adjacent: bool,
}

/// Geometry for a cell that must be painted rather than shaped.
///
/// Returns the subcell rectangles the character fills and the fraction of the
/// ink they carry. A solid block is one rectangle at full opacity; the shade
/// ramp is one cell-covering rectangle at reduced opacity, which keeps a shaded
/// region seamless instead of showing the gaps a stipple font leaves between
/// cells. Quadrants and sextants return one rectangle per filled subcell, and
/// Braille patterns one small rectangle per raised dot.
///
/// An empty result means the character is ordinary text. A Braille blank returns
/// `Some` with no fills: it is one cell wide but draws nothing, and it must not
/// fall through to the shaper as ordinary text.
fn cell_geometry(cell: Option<&TerminalCellSnapshot>) -> Option<TerminalCellGeometry> {
    let ch = cell?.text.chars().next()?;
    if let Some(opacity) = shade_char_opacity(ch) {
        return Some(TerminalCellGeometry {
            fills: vec![(0, 0, 8, 24)],
            opacity,
            merge_adjacent: true,
        });
    }
    if let Some(fill) = block_char_to_rect(ch) {
        return Some(TerminalCellGeometry {
            fills: vec![fill],
            opacity: 1.0,
            merge_adjacent: true,
        });
    }
    if let Some(bits) = quadrant_char_to_filled_bits(ch) {
        // Each filled quadrant of the 2x2 split is half the cell wide and half
        // the cell tall; empty quadrants contribute nothing.
        let quadrants = [(0u16, 0u16), (4, 0), (0, 12), (4, 12)];
        let fills = quadrants
            .into_iter()
            .enumerate()
            .filter(|(index, _)| bits & (1 << index) != 0)
            .map(|(_, (x, y))| (x, y, 4, 12))
            .collect();
        return Some(TerminalCellGeometry {
            fills,
            opacity: 1.0,
            merge_adjacent: true,
        });
    }
    if let Some(bits) = sextant_char_to_filled_bits(ch) {
        // A sextant splits the cell into three rows of two; each filled subcell
        // is half the cell wide and a third of it tall.
        let sextants = [(0u16, 0u16), (4, 0), (0, 8), (4, 8), (0, 16), (4, 16)];
        let fills = sextants
            .into_iter()
            .enumerate()
            .filter(|(index, _)| bits & (1 << index) != 0)
            .map(|(_, (x, y))| (x, y, 4, 8))
            .collect();
        return Some(TerminalCellGeometry {
            fills,
            opacity: 1.0,
            merge_adjacent: true,
        });
    }
    if let Some(bits) = braille_char_to_dots(ch) {
        return Some(TerminalCellGeometry {
            fills: braille_dot_fills(bits),
            opacity: 1.0,
            // Every dot is separated from its neighbour by a gutter, and the
            // gutter is part of the pattern; merging adjacent cells would turn
            // a row of dots into a continuous bar.
            merge_adjacent: false,
        });
    }
    None
}

/// The parts of a terminal cell that decide how it is painted.
#[derive(Clone, Copy, PartialEq)]
struct TerminalCellStyle {
    foreground: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

impl TerminalCellStyle {
    fn font(&self, base: &Font) -> Font {
        let mut font = base.clone();
        if self.bold {
            font.weight = FontWeight::BOLD;
        }
        if self.italic {
            font.style = FontStyle::Italic;
        }
        font
    }
}

/// Whether the cell has no ink worth shaping: blank, a wide-cell spacer, a
/// hidden cell, or a code point a shaped line may not hold.
fn cell_is_blank(cell: &TerminalCellSnapshot) -> bool {
    cell.wide_spacer
        || cell.hidden
        || cell.text.trim_end().is_empty()
        || cell.text.chars().all(|ch| ch.is_control())
}

/// Resolve the foreground and background of one cell, including the cursor and
/// selection treatments the element tree used to apply as styles.
fn terminal_cell_colors(
    cell: Option<&TerminalCellSnapshot>,
    cursor: Option<TerminalCursorSnapshot>,
    point: TerminalGridPoint,
    default_foreground: Hsla,
    default_background: Hsla,
) -> (Hsla, Hsla) {
    let mut foreground = default_foreground;
    let mut background = default_background;
    if let Some(cell) = cell {
        foreground = terminal_color(
            cell.foreground,
            default_foreground,
            default_background,
            default_foreground,
        );
        background = terminal_color(
            cell.background,
            default_foreground,
            default_background,
            default_background,
        );
        // A program that picked its own true colour or its own 256-colour entry
        // chose that ink deliberately; only the theme's own ANSI slots and the
        // two default sentinels are adjusted for legibility.
        if !cell.inverse
            && is_theme_chosen_color(cell.foreground)
            && !cell.text.chars().all(is_decorative_character)
        {
            foreground = ensure_minimum_contrast(foreground, background);
        }
        if cell.dim {
            foreground = foreground.opacity(TERMINAL_DIM_OPACITY);
        }
        if cell.selected {
            background = default_foreground.opacity(TERMINAL_SELECTION_OPACITY);
        }
    }
    if cursor.is_some_and(|cursor| {
        cursor.row == point.row
            && cursor.column == point.column
            && cursor.shape == TerminalCursorShape::Block
    }) {
        background = default_foreground;
        foreground = default_background;
    }
    (foreground, background)
}

/// Whether a colour came from the theme rather than from the program.
fn is_theme_chosen_color(color: TerminalCellColor) -> bool {
    match color {
        TerminalCellColor::Named { .. } => true,
        TerminalCellColor::Indexed { index } => index < 16,
        TerminalCellColor::Rgb { .. } => false,
    }
}

/// Lift `foreground` toward the cell's opposite until it clears the minimum
/// contrast against `background`.
///
/// Terminal programs assume the palette their theme defines. A light-on-light
/// pair from a mismatched theme is unreadable, so the ink is walked toward the
/// background's opposite — never the background, which would erase the glyph.
fn ensure_minimum_contrast(foreground: Hsla, background: Hsla) -> Hsla {
    if contrast_ratio(foreground, background) >= TERMINAL_MINIMUM_CONTRAST {
        return foreground;
    }
    let toward_white = relative_luminance(background) < 0.5;
    let mut best = foreground;
    let mut best_ratio = contrast_ratio(foreground, background);
    for step in 1..=10 {
        let amount = step as f32 / 10.0;
        let candidate = if toward_white {
            foreground.blend(Hsla::white().opacity(amount))
        } else {
            foreground.blend(Hsla::black().opacity(amount))
        };
        let ratio = contrast_ratio(candidate, background);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
        if ratio >= TERMINAL_MINIMUM_CONTRAST {
            return candidate;
        }
    }
    best
}

/// Perceived brightness of a colour, 0.0 (black) to 1.0 (white).
///
/// The coefficients are the usual perceptual weights, so a saturated blue reads
/// as dark and a saturated yellow as light — which is what decides whether ink
/// is lifted toward white or toward black.
fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b
}

/// Perceived distance between two colours, 0.0 to 1.0.
fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
    (relative_luminance(foreground) - relative_luminance(background)).abs()
}

/// The subcell rectangle a block, quadrant or sextant character fills, in
/// `(column, line, columns, lines)` units of the cell grid.
///
/// These characters are drawn as rectangles instead of being shaped: their whole
/// point is to cover their cell edge to edge, and a stipple or a half block from
/// a vector font never does that consistently across fonts. `None` means the
/// character is ordinary text.
fn block_char_to_rect(ch: char) -> Option<TerminalSubcellFill> {
    let codepoint = ch as u32;
    Some(match codepoint {
        // ▀ upper half
        0x2580 => (0, 0, 8, 12),
        // ▁▂▃▄▅▆▇█ lower blocks of 1..=8 eighths
        0x2581..=0x2588 => {
            let eighths = (codepoint - 0x2580) as u16;
            (0, 24 - eighths * 3, 8, eighths * 3)
        }
        // ▉▊▋▌▍▎▏ left blocks of 7..=1 eighths
        0x2589..=0x258F => (0, 0, (0x2590 - codepoint) as u16, 24),
        // ▐ right half
        0x2590 => (4, 0, 4, 24),
        // ▔ upper eighth
        0x2594 => (0, 0, 8, 3),
        // ▕ right eighth
        0x2595 => (7, 0, 1, 24),
        _ => return None,
    })
}

/// The filled quadrants of a quadrant character, bit `row * 2 + column`.
fn quadrant_char_to_filled_bits(ch: char) -> Option<u8> {
    Some(match ch {
        '▘' => 0b0001,
        '▝' => 0b0010,
        '▖' => 0b0100,
        '▗' => 0b1000,
        '▚' => 0b1001,
        '▞' => 0b0110,
        '▛' => 0b0111,
        '▜' => 0b1011,
        '▙' => 0b1101,
        '▟' => 0b1110,
        _ => return None,
    })
}

/// The filled subcells of a sextant character, bit `row * 2 + column`.
///
/// U+1FB00..=U+1FB3B enumerate the 2x3 combinations that have no Block Elements
/// character of their own, so the four that do exist are skipped here.
fn sextant_char_to_filled_bits(ch: char) -> Option<u8> {
    let offset = (ch as u32).checked_sub(0x1FB00)?;
    if offset > 0x3B {
        return None;
    }
    Some((offset + 1 + u32::from(offset >= 20) + u32::from(offset >= 40)) as u8)
}

/// The raised dots of a Braille Pattern character.
///
/// U+2800..=U+28FF are not glyphs with outlines but a 2x4 dot matrix whose bit
/// set is the character itself: bit 0 is dot 1 (top left), bit 3 is dot 4 (top
/// right), bit 6 is dot 7 (bottom left) and bit 7 is dot 8 (bottom right).
/// `None` for every other character, which keeps ordinary text out of
/// [`braille_dot_fills`].
fn braille_char_to_dots(ch: char) -> Option<u8> {
    let offset = (ch as u32).checked_sub(0x2800)?;
    (offset <= 0xFF).then_some(offset as u8)
}

/// The rectangles of one Braille cell's raised dots, in subcell units.
///
/// The 2x4 dots sit on a fixed lattice, so a run of `⣿` reads as a dot matrix
/// at any font size instead of a solid block. Each dot is 2x3 units, leaving a
/// gutter of the same size on each side.
fn braille_dot_fills(bits: u8) -> Vec<TerminalSubcellFill> {
    const DOTS: [(u16, u16); 8] = [
        (1, 2),  // dot 1, top left
        (1, 8),  // dot 2
        (1, 14), // dot 3
        (5, 2),  // dot 4, top right
        (5, 8),  // dot 5
        (5, 14), // dot 6
        (1, 20), // dot 7, bottom left
        (5, 20), // dot 8, bottom right
    ];
    const DOT_WIDTH: u16 = 2;
    const DOT_HEIGHT: u16 = 3;
    DOTS.into_iter()
        .enumerate()
        .filter(|(index, _)| bits & (1u8 << index) != 0)
        .map(|(_, (x, y))| (x, y, DOT_WIDTH, DOT_HEIGHT))
        .collect()
}

/// The fill a shade character stands in for, as a fraction of the cell.
///
/// Fonts draw `░▒▓` as stipple patterns that leave gaps between cells; painting
/// the shade as a translucent fill keeps a shaded region seamless.
fn shade_char_opacity(ch: char) -> Option<f32> {
    match ch {
        '░' => Some(0.25),
        '▒' => Some(0.5),
        '▓' => Some(0.75),
        _ => None,
    }
}

/// Whether a character is a box, block, Braille pattern or geometric connector
/// whose colour has to match its neighbours exactly to keep a border or a
/// picture continuous.
fn is_decorative_character(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x257F   // Box Drawing
            | 0x2580..=0x259F // Block Elements
            | 0x25A0..=0x25FF // Geometric Shapes
            | 0x2800..=0x28FF // Braille Patterns
            | 0x1FB00..=0x1FB3B // Legacy Computing sextants
            | 0xE0B0..=0xE0D7 // Powerline separators
    )
}

/// The grid's metrics for the current theme and window scale.
fn measured_terminal_cell_metrics(
    text_system: &gpui::TextSystem,
    cx: &App,
    mode: TerminalSurfaceMode,
) -> TerminalCellMetrics {
    terminal_cell_metrics(
        text_system,
        &terminal_font(cx),
        f32::from(cx.theme().mono_font_size),
        mode,
    )
}

/// The metrics of the code font, measured from the font itself.
///
/// The cell width is the advance of `m`, so every column is exactly one glyph
/// wide for the font actually in use. A fixed width from a 13px design would
/// leave the columns and the glyphs in two different coordinate systems.
fn terminal_cell_metrics(
    text_system: &gpui::TextSystem,
    base: &Font,
    font_size: f32,
    mode: TerminalSurfaceMode,
) -> TerminalCellMetrics {
    let font_size = if font_size.is_finite() {
        font_size.clamp(TERMINAL_MIN_FONT_SIZE, TERMINAL_MAX_FONT_SIZE)
    } else {
        TERMINAL_MIN_FONT_SIZE
    };
    let font_pixels = px(font_size);
    let font_id = text_system.resolve_font(base);
    let cell_width = text_system
        .advance(font_id, font_pixels, 'm')
        .map(|advance| f32::from(advance.width))
        .unwrap_or(font_size * 0.6)
        .max(1.0);
    TerminalCellMetrics {
        cell_width,
        cell_height: font_size * TERMINAL_LINE_HEIGHT_RATIO,
        horizontal_padding: mode.horizontal_padding(),
        vertical_padding: mode.vertical_padding(),
    }
}

/// The font the grid is shaped and measured with.
///
/// The theme's code family and weight, the ligature preference the reader chose,
/// and the bundled symbol face plus the platform's fallbacks: a glyph the code
/// font lacks has to land somewhere the grid can still measure, and the
/// private-use area an Agent draws from is only covered by the bundled face.
fn terminal_font(cx: &App) -> Font {
    let mut font = font(cx.theme().mono_font_family.clone());
    font.weight = code_font_weight(cx);
    if !crate::typography::terminal_ligatures(cx) {
        font.features = FontFeatures::disable_ligatures();
    }
    let mut fallbacks = vec![crate::assets::BUNDLED_SYMBOL_FAMILY.to_string()];
    fallbacks.extend(
        crate::platform::platform_font_fallbacks()
            .iter()
            .filter(|name| *name != &cx.theme().mono_font_family.as_ref())
            .filter(|name| *name != &crate::assets::BUNDLED_SYMBOL_FAMILY)
            .map(|name| (*name).to_string()),
    );
    font.fallbacks = Some(FontFallbacks::from_fonts(fallbacks));
    font
}

/// The bundled symbol family, at the code font's weight.
///
/// Symbol cells are shaped with this family explicitly rather than relying on
/// fallback: the glyph has to come from a font whose outline can be measured
/// and scaled to the cell, and a fallback pick is not guaranteed to be it.
fn terminal_symbol_font(cx: &App) -> Font {
    let mut font = font(crate::assets::BUNDLED_SYMBOL_FAMILY);
    font.weight = code_font_weight(cx);
    font
}

/// A run of cells with one style, being accumulated during prepaint.
struct PendingRun {
    style: TerminalCellStyle,
    start_column: u16,
    end_column: u16,
    text: String,
    /// Whether every cell in the run is one column wide.
    ///
    /// Only a narrow run may have its glyphs forced onto a fixed pitch. A wide
    /// glyph already covers two cells, and forcing it to one would overlap it
    /// with its neighbour, so a run holding one keeps the font's own advance.
    all_narrow: bool,
}

impl PendingRun {
    fn new(style: TerminalCellStyle, column: u16) -> Self {
        Self {
            style,
            start_column: column,
            end_column: column,
            text: String::new(),
            all_narrow: true,
        }
    }

    fn push_cell(&mut self, column: u16, text: &str, wide: bool) {
        self.text.extend(text.chars().filter(|ch| !ch.is_control()));
        self.end_column = column;
        self.all_narrow &= !wide;
    }

    fn columns(&self) -> u16 {
        self.end_column.saturating_sub(self.start_column) + 1
    }
}

/// Shape every row of the grid once per frame.
///
/// Each row is cut into runs of adjacent cells that share a style, and each run
/// is shaped as one line with the cell width forced, so the shaper's own advance
/// cannot drift the columns away from the grid. Blank cells start no run, which
/// keeps a screen of prose down to a handful of shaped lines instead of one per
/// cell.
fn shape_terminal_grid(
    entity: &Entity<TerminalSurface>,
    metrics: TerminalCellMetrics,
    window: &mut Window,
    cx: &mut App,
) -> TerminalGridLayout {
    let font_size = f32::from(cx.theme().mono_font_size);
    let base_font = terminal_font(cx);
    let symbol_font = terminal_symbol_font(cx);
    let text_system = window.text_system().clone();
    let surface = entity.read(cx);
    let Some(tab) = surface.active_tab.and_then(|index| surface.tabs.get(index)) else {
        return TerminalGridLayout::empty(metrics);
    };
    let frame = &tab.frame;
    let rows = frame.rows();
    let columns = frame.columns();
    let default_foreground = cx.theme().foreground;
    let default_background = cx.theme().background;
    let cursor = frame
        .cursor()
        .filter(|_| surface.marked_text.is_none() && surface.cursor_visible);
    let epoch = terminal_layout_epoch(surface, tab, metrics, cx);

    // The cache lives on the surface, behind a cell so the prepaint pass — which
    // only has a shared read of the entity — can still reuse what it shaped last
    // frame.
    let mut cache = surface.row_layouts.borrow_mut();
    let mut glyph_cache = surface.glyph_cache.borrow_mut();
    let rebuild_all = cache.epoch != epoch || cache.rows.len() != usize::from(rows);
    let dirty = if rebuild_all {
        cache.epoch = epoch;
        cache.rows.clear();
        surface.dirty_rows.borrow_mut().clear();
        (0..rows).collect::<BTreeSet<_>>()
    } else {
        std::mem::take(&mut *surface.dirty_rows.borrow_mut())
    };
    for row in dirty {
        if row >= rows {
            continue;
        }
        let shaped = shape_terminal_row(
            row,
            frame,
            columns,
            cursor,
            default_foreground,
            default_background,
            &base_font,
            &symbol_font,
            font_size,
            metrics,
            text_system.as_ref(),
            &mut glyph_cache,
            window,
        );
        if cache.rows.len() <= usize::from(row) {
            cache
                .rows
                .resize_with(usize::from(row) + 1, Default::default);
        }
        cache.rows[usize::from(row)] = Arc::new(shaped);
    }
    cache.rows.resize_with(usize::from(rows), Default::default);
    TerminalGridLayout {
        rows: cache.rows.clone(),
        metrics,
    }
}

/// Everything that changes how every row is shaped.
///
/// Damage says which rows changed; this says whether *any* cached row still
/// means what it meant. The cursor is deliberately not part of the epoch: it
/// moves often, and the poll path marks the row it left and the row it entered
/// as damaged, which is cheaper than reshaping the screen twice a second.
fn terminal_layout_epoch(
    surface: &TerminalSurface,
    tab: &TerminalTab,
    metrics: TerminalCellMetrics,
    cx: &App,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tab.session.id.as_str().hash(&mut hasher);
    tab.frame.rows().hash(&mut hasher);
    tab.frame.columns().hash(&mut hasher);
    hash_terminal_color(cx.theme().foreground, &mut hasher);
    hash_terminal_color(cx.theme().background, &mut hasher);
    cx.theme().mono_font_family.hash(&mut hasher);
    f32::from(cx.theme().mono_font_size)
        .to_bits()
        .hash(&mut hasher);
    code_font_weight(cx).0.to_bits().hash(&mut hasher);
    crate::typography::terminal_ligatures(cx).hash(&mut hasher);
    metrics.cell_width.to_bits().hash(&mut hasher);
    metrics.cell_height.to_bits().hash(&mut hasher);
    metrics.horizontal_padding.to_bits().hash(&mut hasher);
    metrics.vertical_padding.to_bits().hash(&mut hasher);
    surface.marked_text.hash(&mut hasher);
    hasher.finish()
}

fn hash_terminal_color(color: Hsla, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    let rgb = color.to_rgb();
    rgb.r.to_bits().hash(hasher);
    rgb.g.to_bits().hash(hasher);
    rgb.b.to_bits().hash(hasher);
    rgb.a.to_bits().hash(hasher);
}

/// Shapes one painted row into runs, filled rectangles and box strokes.
#[allow(clippy::too_many_arguments)]
fn shape_terminal_row(
    row: u16,
    frame: &TerminalFrameCache,
    columns: u16,
    cursor: Option<TerminalCursorSnapshot>,
    default_foreground: Hsla,
    default_background: Hsla,
    base_font: &Font,
    symbol_font: &Font,
    font_size: f32,
    metrics: TerminalCellMetrics,
    text_system: &gpui::TextSystem,
    glyph_cache: &mut std::collections::HashMap<u64, ShapedLine>,
    window: &mut Window,
) -> TerminalRowLayout {
    let mut layout = TerminalRowLayout::default();
    let mut pending: Option<PendingRun> = None;
    for column in 0..columns {
        let point = TerminalGridPoint { row, column };
        let cell = frame.cell(point);
        // A blank cell carries no ink. It also ends the current run, because a
        // run's shaped text must hold adjacent columns only.
        let (style, text, wide) = match cell.filter(|cell| !cell_is_blank(cell)) {
            Some(cell) => {
                let (foreground, _) = terminal_cell_colors(
                    Some(cell),
                    cursor,
                    point,
                    default_foreground,
                    default_background,
                );
                (
                    Some(TerminalCellStyle {
                        foreground,
                        bold: cell.bold,
                        italic: cell.italic,
                        underline: cell.underline || cell.hyperlink.is_some(),
                        strikeout: cell.strikeout,
                    }),
                    cell.text.as_str(),
                    cell.wide || cell.wide_spacer,
                )
            }
            None => (None, "", false),
        };
        let foreground = style.map(|style| style.foreground).unwrap_or_default();

        // A Box Drawing character is painted from its arms, not shaped. Its
        // whole point is to cover its cell edge to edge and to meet its
        // neighbour exactly; a font's own glyph does neither consistently.
        if let Some(character) = single_character(text)
            && let Some(arms) = terminal_box_drawing::parse(character)
        {
            flush_pending_run(
                &mut layout.runs,
                pending.take(),
                base_font,
                font_size,
                metrics,
                glyph_cache,
                window,
            );
            if let Some(last) = layout.strokes.last_mut()
                && last.continues(row, column, arms, foreground)
            {
                last.extend();
            } else {
                layout
                    .strokes
                    .push(BoxStroke::new(row, column, arms, foreground));
            }
            continue;
        }

        // A private-use symbol is fitted to the cell's outline and centred in
        // it. Shaped as ordinary text it would sit on the baseline at whatever
        // size the font chose, which is how an Agent's icon ends up either a
        // speck or a glyph too tall for its row.
        if let Some(character) = single_character(text)
            && is_terminal_symbol(character)
            && let Some(fitted) = fitted_symbol_font_size(
                text_system,
                symbol_font,
                font_size,
                metrics.cell_width,
                metrics.cell_height,
                character,
            )
        {
            flush_pending_run(
                &mut layout.runs,
                pending.take(),
                base_font,
                font_size,
                metrics,
                glyph_cache,
                window,
            );
            let text_run = TextRun {
                len: text.len(),
                font: symbol_font.clone(),
                color: foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = shape_cached_line(
                glyph_cache,
                &text_run,
                text.to_string(),
                fitted,
                None,
                window,
            );
            layout.runs.push(TerminalTextRun {
                line,
                column,
                columns: if wide { 2 } else { 1 },
                symbol: Some(SymbolFit {
                    line_height: fitted * TERMINAL_LINE_HEIGHT_RATIO,
                }),
            });
            continue;
        }

        // Blocks, quadrants, sextants, Braille dots and the shade ramp are drawn
        // as rectangles rather than shaped: their whole point is to cover their
        // cell edge to edge, and a glyph from a vector font never does that the
        // same way twice. Recording them as geometry is what keeps a border
        // continuous, a logo made of half blocks legible and a Braille pattern
        // readable even when no installed font covers it.
        if let Some(geometry) = cell_geometry(cell) {
            let opacity = geometry.opacity;
            let merge_adjacent = geometry.merge_adjacent;
            flush_pending_run(
                &mut layout.runs,
                pending.take(),
                base_font,
                font_size,
                metrics,
                glyph_cache,
                window,
            );
            for fill in geometry.fills {
                // Adjacent edge-to-edge fills of the same colour merge, so a
                // line of full blocks is one quad rather than one per cell.
                if let Some(last) = layout.rects.last_mut()
                    && last.fill == fill
                    && last.color == foreground
                    && last.opacity == opacity
                    && last.merge_adjacent
                    && merge_adjacent
                    && last.column + 1 == column
                {
                    last.columns = last.columns.saturating_add(1);
                    continue;
                }
                layout.rects.push(TerminalRect {
                    row,
                    column,
                    columns: 1,
                    fill,
                    color: foreground,
                    opacity,
                    merge_adjacent,
                });
            }
            continue;
        }

        match (style, pending.as_mut()) {
            (Some(style), Some(run)) if run.style == style => {
                run.push_cell(column, text, wide);
            }
            (Some(style), _) => {
                flush_pending_run(
                    &mut layout.runs,
                    pending.take(),
                    base_font,
                    font_size,
                    metrics,
                    glyph_cache,
                    window,
                );
                let mut run = PendingRun::new(style, column);
                run.push_cell(column, text, wide);
                pending = Some(run);
            }
            (None, Some(_)) => {
                flush_pending_run(
                    &mut layout.runs,
                    pending.take(),
                    base_font,
                    font_size,
                    metrics,
                    glyph_cache,
                    window,
                );
            }
            (None, None) => {}
        }
    }
    flush_pending_run(
        &mut layout.runs,
        pending.take(),
        base_font,
        font_size,
        metrics,
        glyph_cache,
        window,
    );
    layout
}

/// The character a cell paints, when it is exactly one.
fn single_character(text: &str) -> Option<char> {
    let mut characters = text.chars();
    let character = characters.next()?;
    if characters.next().is_some() || character.is_control() {
        return None;
    }
    Some(character)
}

/// Whether a code point is in the private-use area Nerd Fonts covers.
///
/// The same ranges [`crate::assets::BUNDLED_SYMBOL_FAMILY`] draws: the three
/// private-use planes plus the handful of powerline marks Nerd Fonts keeps
/// outside them.
fn is_terminal_symbol(character: char) -> bool {
    matches!(
        character as u32,
        0x23FB..=0x23FE
            | 0x2B58
            | 0x2665
            | 0x26A1
            | 0xE000..=0xF8FF
            | 0xF0000..=0xFFFFD
            | 0x100000..=0x10FFFD
    )
}

/// The font size at which a symbol's ink fills most of its cell.
///
/// The size is measured from the glyph's typographic bounds at the code font's
/// size, then scaled so the longer dimension covers [`SYMBOL_CELL_FILL`] of the
/// cell. Clamping keeps a pathological glyph from becoming either invisible or
/// a row-breaking block.
fn fitted_symbol_font_size(
    text_system: &gpui::TextSystem,
    font: &Font,
    base_size: f32,
    cell_width: f32,
    cell_height: f32,
    character: char,
) -> Option<f32> {
    const SYMBOL_CELL_FILL: f32 = 0.92;
    let font_id = text_system.resolve_font(font);
    let bounds = text_system
        .typographic_bounds(font_id, px(base_size), character)
        .ok()?;
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale =
        (cell_width * SYMBOL_CELL_FILL / width).min(cell_height * SYMBOL_CELL_FILL / height);
    Some((base_size * scale).clamp(base_size * 0.5, base_size * 1.4))
}

/// Shape one accumulated run and record the columns it covers.
fn flush_pending_run(
    runs: &mut Vec<TerminalTextRun>,
    run: Option<PendingRun>,
    base_font: &Font,
    font_size: f32,
    metrics: TerminalCellMetrics,
    cache: &mut std::collections::HashMap<u64, ShapedLine>,
    window: &mut Window,
) {
    let Some(run) = run else {
        return;
    };
    if run.text.is_empty() {
        return;
    }
    let text_run = TextRun {
        len: run.text.len(),
        font: run.style.font(base_font),
        color: run.style.foreground,
        background_color: None,
        underline: run.style.underline.then(|| gpui::UnderlineStyle {
            thickness: px(1.0),
            color: Some(run.style.foreground),
            wavy: false,
        }),
        strikethrough: run.style.strikeout.then(|| gpui::StrikethroughStyle {
            thickness: px(1.0),
            color: Some(run.style.foreground),
        }),
    };
    let columns = run.columns();
    // A narrow run is forced onto the cell pitch, which is what keeps a font
    // whose advance differs from the measured cell from drifting. A run holding
    // a wide glyph keeps the font's own advances: the wide glyph already spans
    // two cells, and forcing every glyph to one cell would overlap it with the
    // text after it.
    let force_width = run.all_narrow.then_some(metrics.cell_width);
    let line = shape_cached_line(cache, &text_run, run.text, font_size, force_width, window);
    runs.push(TerminalTextRun {
        line,
        column: run.start_column,
        columns,
        symbol: None,
    });
}

/// Spend a fractional line delta as whole lines, carrying the remainder.
///
/// A trackpad reports many sub-row deltas per gesture. Rounding each event on
/// its own threw away slow gestures entirely and turned fast ones into a burst
/// of fixed jumps; carrying the remainder makes the movement add up to the
/// distance the fingers actually travelled.
fn accumulate_scroll_lines(remainder: &mut f32, lines: f32) -> i32 {
    if !lines.is_finite() {
        return 0;
    }
    *remainder += lines;
    let whole = remainder.trunc();
    if whole == 0.0 {
        return 0;
    }
    *remainder -= whole;
    whole as i32
}

/// Shape one run, reusing the previous result when nothing about it changed.
///
/// A terminal repaints on every output poll and every cursor blink, but a row
/// that did not change must not be shaped again: the layout costs more than the
/// paint. A row's text and style are what decide its layout, so the key covers
/// both — the same bytes in a different colour or weight must reshape.
fn shape_cached_line(
    cache: &mut std::collections::HashMap<u64, ShapedLine>,
    text_run: &TextRun,
    text: String,
    font_size: f32,
    force_width: Option<f32>,
    window: &mut Window,
) -> ShapedLine {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&text, &mut hasher);
    std::hash::Hash::hash(&text_run.font, &mut hasher);
    std::hash::Hash::hash(&text_run.color, &mut hasher);
    std::hash::Hash::hash(&text_run.underline, &mut hasher);
    std::hash::Hash::hash(&text_run.strikethrough, &mut hasher);
    std::hash::Hash::hash(&font_size.to_bits(), &mut hasher);
    std::hash::Hash::hash(&force_width.map(f32::to_bits), &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    if let Some(line) = cache.get(&key) {
        return line.clone();
    }
    let line = window.text_system().shape_line(
        text.into(),
        px(font_size),
        std::slice::from_ref(text_run),
        force_width.map(px),
    );
    // Rows that leave the screen would otherwise pin their shaped lines
    // forever; a few screens' worth covers scrolling without letting the cache
    // grow without bound.
    if cache.len() > GLYPH_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, line.clone());
    line
}

fn cell_bounds(
    origin: Point<Pixels>,
    cell: TerminalGridPoint,
    metrics: TerminalCellMetrics,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x + px(f32::from(cell.column) * metrics.cell_width),
            origin.y + px(f32::from(cell.row) * metrics.cell_height),
        ),
        size(px(metrics.cell_width), px(metrics.cell_height)),
    )
}

/// Paint every cell of the grid: backgrounds, glyphs, decorations and the cursor.
///
/// The whole pass runs inside the grid's own content mask, so a grid that is
/// momentarily larger than the pane it sits in — during a resize, or while an
/// old frame is still on screen — can never draw over the chrome around it.
fn paint_terminal_grid(
    bounds: Bounds<Pixels>,
    layout: &TerminalGridLayout,
    entity: &Entity<TerminalSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        paint_terminal_grid_masked(bounds, layout, entity, window, cx)
    });
}

fn paint_terminal_grid_masked(
    bounds: Bounds<Pixels>,
    layout: &TerminalGridLayout,
    entity: &Entity<TerminalSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    let metrics = layout.metrics;
    let default_foreground = cx.theme().foreground;
    let default_background = cx.theme().background;
    let cursor = paint_terminal_backgrounds(
        bounds,
        layout,
        entity,
        default_foreground,
        default_background,
        window,
        cx,
    );

    // Block, quadrant and sextant characters are filled rectangles, not glyphs:
    // painting them here keeps a border continuous and a half-block logo legible
    // whatever the font draws for those code points.
    paint_terminal_rects(bounds, layout, window);
    // Box Drawing characters are lines, joined across cells at their edges.
    paint_terminal_strokes(bounds, layout, window);

    // Text next, one shaped run at a time. The shaper laid each run out from its
    // first column with the cell width forced, so painting it at that column is
    // enough: every following glyph lands on its own column already. This is
    // outside the entity borrow above, because painting a shaped line needs
    // `&mut App`.
    let scale = window.scale_factor();
    for (row, row_layout) in layout.rows.iter().enumerate() {
        let row_top = snap_terminal_pixels(
            bounds.origin.y + px(row as f32 * metrics.cell_height),
            scale,
        );
        for text_run in &row_layout.runs {
            let cell_x = snap_terminal_pixels(
                bounds.origin.x + px(f32::from(text_run.column) * metrics.cell_width),
                scale,
            );
            let width = Some(px(f32::from(text_run.columns) * metrics.cell_width));
            if let Some(symbol) = text_run.symbol {
                // The fitted glyph is centred in its cell rather than aligned
                // to the text baseline: icons have no baseline relationship
                // with the letters beside them.
                let line_height = px(symbol.line_height);
                let origin = point(
                    cell_x,
                    row_top + px((metrics.cell_height - symbol.line_height) / 2.0),
                );
                let _ =
                    text_run
                        .line
                        .paint(origin, line_height, TextAlign::Center, width, window, cx);
            } else {
                let _ = text_run.line.paint(
                    point(cell_x, row_top),
                    px(metrics.cell_height),
                    TextAlign::Left,
                    width,
                    window,
                    cx,
                );
            }
        }
    }

    paint_terminal_scrollbar(bounds, entity, window, cx);

    let Some(cursor) = cursor else {
        return;
    };
    let cursor_bounds = snap_terminal_rect(
        cell_bounds(
            bounds.origin,
            TerminalGridPoint {
                row: cursor.row,
                column: cursor.column,
            },
            metrics,
        ),
        scale,
    );
    match cursor.shape {
        // A block cursor is painted by inverting the cell it sits on, which the
        // background pass already did.
        TerminalCursorShape::Block | TerminalCursorShape::Hidden => {}
        TerminalCursorShape::Beam => {
            window.paint_quad(fill(
                Bounds::new(
                    cursor_bounds.origin,
                    size(px(1.0), cursor_bounds.size.height),
                ),
                default_foreground,
            ));
        }
        TerminalCursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        cursor_bounds.origin.x,
                        cursor_bounds.origin.y + cursor_bounds.size.height - px(1.0),
                    ),
                    size(cursor_bounds.size.width, px(1.0)),
                ),
                default_foreground,
            ));
        }
        TerminalCursorShape::HollowBlock => {
            window.paint_quad(outline(
                cursor_bounds,
                default_foreground,
                BorderStyle::default(),
            ));
        }
    }
}

/// Paint the block, quadrant, sextant and Braille rectangles recorded during
/// prepaint.
///
/// The subcell grid is 8 columns by 24 lines, the least common multiple of the
/// eighth-block splits and the sextant splits, so every rectangle lands on a
/// whole subcell without accumulating rounding error across a row.
fn paint_terminal_rects(bounds: Bounds<Pixels>, layout: &TerminalGridLayout, window: &mut Window) {
    let metrics = layout.metrics;
    let scale = window.scale_factor();
    let subcell_width = metrics.cell_width / TERMINAL_SUBCELL_COLUMNS;
    let subcell_height = metrics.cell_height / TERMINAL_SUBCELL_LINES;
    for row in &layout.rows {
        for rect in &row.rects {
            let (x, y, columns, lines) = rect.fill;
            let origin = point(
                bounds.origin.x
                    + px(f32::from(rect.column) * metrics.cell_width)
                    + px(f32::from(x) * subcell_width),
                bounds.origin.y
                    + px(f32::from(rect.row) * metrics.cell_height)
                    + px(f32::from(y) * subcell_height),
            );
            let size = size(
                px((f32::from(columns)
                    + f32::from(rect.columns.saturating_sub(1)) * TERMINAL_SUBCELL_COLUMNS)
                    * subcell_width),
                px(f32::from(lines) * subcell_height),
            );
            if size.width <= px(0.0) || size.height <= px(0.0) {
                continue;
            }
            window.paint_quad(fill(
                snap_terminal_rect(Bounds::new(origin, size), scale),
                rect.color.opacity(rect.opacity),
            ));
        }
    }
}

/// Paint the scrollbar overlay and its thumb.
///
/// The bar lives inside the grid rather than beside it: the grid's own width is
/// the terminal's width, so taking a gutter for chrome would make every program
/// reflow when the history grew past a screen.
fn paint_terminal_scrollbar(
    bounds: Bounds<Pixels>,
    entity: &Entity<TerminalSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    let surface = entity.read(cx);
    let Some(geometry) = surface.scrollbar_geometry_for(bounds) else {
        return;
    };
    let scale = window.scale_factor();
    window.paint_quad(fill(
        snap_terminal_rect(geometry.track, scale),
        cx.theme().foreground.opacity(0.06),
    ));
    let opacity = if surface.scrollbar_drag { 0.55 } else { 0.26 };
    window.paint_quad(fill(
        snap_terminal_rect(geometry.thumb, scale),
        cx.theme().foreground.opacity(opacity),
    ));
}

/// Paint the Box Drawing runs recorded during prepaint.
fn paint_terminal_strokes(
    bounds: Bounds<Pixels>,
    layout: &TerminalGridLayout,
    window: &mut Window,
) {
    let metrics = layout.metrics;
    for row in &layout.rows {
        if row.strokes.is_empty() {
            continue;
        }
        terminal_box_drawing::paint(
            bounds,
            metrics.cell_width,
            metrics.cell_height,
            &row.strokes,
            window,
        );
    }
}

/// Snaps one coordinate to the device pixel grid.
///
/// A cell boundary that lands on a fraction of a physical pixel is what leaves
/// a hairline gap between two background runs, or a border that is one pixel
/// thick in one row and two in the next.
fn snap_terminal_pixels(value: Pixels, scale: f32) -> Pixels {
    if !scale.is_finite() || scale <= 0.0 {
        return value;
    }
    px((f32::from(value) * scale).round() / scale)
}

fn snap_terminal_rect(bounds: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    let origin = point(
        snap_terminal_pixels(bounds.origin.x, scale),
        snap_terminal_pixels(bounds.origin.y, scale),
    );
    let corner = point(
        snap_terminal_pixels(bounds.origin.x + bounds.size.width, scale),
        snap_terminal_pixels(bounds.origin.y + bounds.size.height, scale),
    );
    Bounds::from_corners(origin, corner)
}

/// Paint every cell's background and return the cursor the text pass still owes.
///
/// Adjacent cells sharing a background are painted as one quad: a full-screen
/// TUI in colour used to emit thousands of quads per frame, one per cell, and a
/// run of the same colour is the common shape. Cells on the default background
/// need no quad at all — the grid's own fill is that colour already.
fn paint_terminal_backgrounds(
    bounds: Bounds<Pixels>,
    layout: &TerminalGridLayout,
    entity: &Entity<TerminalSurface>,
    default_foreground: Hsla,
    default_background: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> Option<TerminalCursorSnapshot> {
    let metrics = layout.metrics;
    let surface = entity.read(cx);
    let tab = surface
        .active_tab
        .and_then(|index| surface.tabs.get(index))?;
    let frame = &tab.frame;
    let rows = frame.rows();
    let columns = frame.columns();
    let cursor = frame
        .cursor()
        .filter(|_| surface.marked_text.is_none() && surface.cursor_visible);
    let scale = window.scale_factor();
    for row in 0..rows {
        let mut run_start: Option<(u16, Hsla)> = None;
        for column in 0..=columns {
            let background = (column < columns).then(|| {
                let point = TerminalGridPoint { row, column };
                terminal_cell_colors(
                    frame.cell(point),
                    cursor,
                    point,
                    default_foreground,
                    default_background,
                )
                .1
            });
            match (run_start, background) {
                (Some((start, color)), Some(background)) if background == color => {
                    run_start = Some((start, color));
                }
                (Some((start, color)), next) => {
                    let first = TerminalGridPoint { row, column: start };
                    let mut run_bounds = cell_bounds(bounds.origin, first, metrics);
                    run_bounds.size.width =
                        px(f32::from(column.saturating_sub(start)) * metrics.cell_width);
                    window.paint_quad(fill(snap_terminal_rect(run_bounds, scale), color));
                    run_start = match next {
                        Some(background) if background != default_background => {
                            Some((column, background))
                        }
                        _ => None,
                    };
                }
                (None, Some(background)) if background != default_background => {
                    run_start = Some((column, background));
                }
                (None, _) => {}
            }
        }
    }

    // Every other match on screen, not just the one the bar navigated to. The
    // active match is the emulator's own selection, which the background pass
    // above already painted; overlapping a second wash on it would only muddy
    // the colour.
    let history = frame.history_lines() as i64;
    let offset = frame.display_offset() as i64;
    for (index, search_match) in surface.search_matches.iter().enumerate() {
        if index == surface.active_search_match {
            continue;
        }
        let row = search_match.line as i64 - history + offset;
        if row < 0 || row >= i64::from(rows) {
            continue;
        }
        let start = search_match.start_column;
        let end = search_match.end_column.saturating_add(1);
        if end <= start {
            continue;
        }
        let highlight = Bounds::new(
            point(
                bounds.origin.x + px(f32::from(start) * metrics.cell_width),
                bounds.origin.y + px(row as f32 * metrics.cell_height),
            ),
            size(
                px(f32::from(end - start) * metrics.cell_width),
                px(metrics.cell_height),
            ),
        );
        window.paint_quad(fill(
            snap_terminal_rect(highlight, scale),
            cx.theme().yellow.opacity(TERMINAL_SEARCH_MATCH_OPACITY),
        ));
    }

    // A detected link only exists while the pointer is on it, so the underline
    // is painted here rather than shaped into the cell.
    if let Some((row, link)) = &surface.hovered_link
        && *row < rows
        && link.columns.end > link.columns.start
    {
        let underline = Bounds::new(
            point(
                bounds.origin.x + px(f32::from(link.columns.start) * metrics.cell_width),
                bounds.origin.y
                    + px(f32::from(*row) * metrics.cell_height + metrics.cell_height - 1.0),
            ),
            size(
                px(f32::from(link.columns.end - link.columns.start) * metrics.cell_width),
                px(1.0),
            ),
        );
        window.paint_quad(fill(
            snap_terminal_rect(underline, scale),
            cx.theme().accent.opacity(TERMINAL_LINK_UNDERLINE_OPACITY),
        ));
    }
    cursor
}

impl EntityInputHandler for TerminalSurface {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
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

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
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
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let composition_cleared = self.marked_text.take().is_some();
        if composition_cleared {
            window.invalidate_character_coordinates();
        }
        if self.commit_text(text, cx) || composition_cleared {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self
            .active_tab
            .and_then(|index| self.tabs.get(index))?
            .frame
            .cursor()?;
        let start = range_utf16.start as f32;
        let width = range_utf16.end.saturating_sub(range_utf16.start).max(1) as f32;
        // The grid's own metrics, so the IME candidate window lands on the cell
        // the cursor is actually painted in rather than on a design constant.
        let cell_metrics = self.cell_metrics;
        Some(Bounds::new(
            point(
                element_bounds.left()
                    + px(self.horizontal_padding()
                        + (f32::from(cursor.column) + start) * cell_metrics.cell_width),
                element_bounds.top()
                    + px(self.vertical_padding() + f32::from(cursor.row) * cell_metrics.cell_height),
            ),
            size(
                px(width * cell_metrics.cell_width),
                px(cell_metrics.cell_height),
            ),
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
        self.active_tab
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| tab.session.status == TerminalStatus::Running)
    }
}

impl gpui::Focusable for TerminalSurface {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("terminal-surface")
            .track_focus(&self.focus)
            .key_context(TERMINAL_KEY_CONTEXT)
            .on_action(cx.listener(Self::send_tab))
            .on_action(cx.listener(Self::send_backtab))
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .when(self.mode.paints_surface_background(), |surface| {
                surface.bg(cx.theme().background)
            })
            .text_color(cx.theme().foreground)
            .when(self.mode.shows_chrome(), |surface| {
                surface
                    .child(self.render_tabs(cx))
                    .child(self.render_toolbar(cx))
            })
            .when(self.search_open, |surface| {
                surface.child(self.render_search(cx))
            })
            .child(self.render_grid(cx))
    }
}

impl Drop for TerminalSurface {
    fn drop(&mut self) {
        let terminal_ids = self
            .tabs
            .iter()
            .filter(|tab| self.owned_terminal_ids.contains(tab.session.id.as_str()))
            .map(|tab| tab.session.id.clone())
            .collect::<Vec<_>>();
        if terminal_ids.is_empty() {
            return;
        }
        let transport = self.transport.clone();
        self.background
            .spawn(async move {
                for terminal_id in terminal_ids {
                    let _ = transport.close_terminal(&terminal_id).await;
                }
            })
            .detach();
    }
}

async fn run_poll_work(work: TerminalPollWork) -> TerminalPollResult {
    let snapshot = match work
        .transport
        .poll_terminal(&work.terminal_id, work.next_sequence)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return TerminalPollResult {
                terminal_id: work.terminal_id,
                session: None,
                result: Err(format!("Terminal snapshot failed: {}", error.message)),
                pty_replies: Vec::new(),
            };
        }
    };
    let session = snapshot.session.clone();
    let mut pty_replies = Vec::new();
    let result = work
        .backend
        .lock()
        .map_err(|_| "Terminal parser state lock failed".to_string())
        .and_then(|mut backend| {
            backend
                .sync(&snapshot)
                .map_err(|error| format!("Terminal sync failed: {}", error.message))
                .map(|outcome| {
                    // Cloned, not moved: the same outcome still has to reach the
                    // frame gate below.
                    pty_replies.clone_from(&outcome.pty_replies);
                    (outcome.ingested_chunks > 0 || outcome.rebuilt)
                        .then(|| (outcome, backend.frame()))
                })
        });
    TerminalPollResult {
        terminal_id: work.terminal_id,
        session: Some(session),
        result,
        pty_replies,
    }
}

/// Format a colour the way an xterm colour query expects it.
///
/// `rgb:rrrr/gggg/bbbb` — four hex digits per channel, which is what every
/// terminal that answers `OSC 10`/`OSC 11` writes and what a program that parses
/// the reply assumes.
fn terminal_hsla_to_xterm(color: Hsla) -> String {
    let rgb = color.to_rgb();
    let channel = |value: f32| {
        let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("{byte:02x}{byte:02x}")
    };
    format!("{}/{}/{}", channel(rgb.r), channel(rgb.g), channel(rgb.b))
}

fn terminal_color(
    color: TerminalCellColor,
    default_foreground: Hsla,
    default_background: Hsla,
    fallback: Hsla,
) -> Hsla {
    match color {
        TerminalCellColor::Rgb { red, green, blue } => {
            rgb((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)).into()
        }
        TerminalCellColor::Indexed { index } => indexed_terminal_color(index),
        TerminalCellColor::Named { index } if index < 16 => indexed_terminal_color(index as u8),
        TerminalCellColor::Named { index: 256 } => default_foreground,
        TerminalCellColor::Named { index: 257 } => default_background,
        TerminalCellColor::Named { index: 258 } => default_foreground,
        TerminalCellColor::Named { index } if (259..=266).contains(&index) => {
            indexed_terminal_color((index - 259) as u8).opacity(0.68)
        }
        TerminalCellColor::Named { index: 267 } => default_foreground,
        TerminalCellColor::Named { index: 268 } => default_foreground.opacity(0.68),
        TerminalCellColor::Named { .. } => fallback,
    }
}

fn indexed_terminal_color(index: u8) -> Hsla {
    const ANSI: [u32; 16] = [
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
        0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ];
    let value = if index < 16 {
        ANSI[usize::from(index)]
    } else if index < 232 {
        let offset = index - 16;
        let levels = [0u32, 95, 135, 175, 215, 255];
        let red = levels[usize::from(offset / 36)];
        let green = levels[usize::from((offset % 36) / 6)];
        let blue = levels[usize::from(offset % 6)];
        (red << 16) | (green << 8) | blue
    } else {
        let gray = 8 + u32::from(index - 232) * 10;
        (gray << 16) | (gray << 8) | gray
    };
    rgb(value).into()
}

fn terminal_status_label(status: TerminalStatus) -> &'static str {
    match status {
        TerminalStatus::Running => "running",
        TerminalStatus::Exited => "exited",
        TerminalStatus::Killed => "killed",
        TerminalStatus::Stale => "stale",
    }
}

fn mouse_button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => 0,
    }
}

fn terminal_modifiers(value: gpui::Modifiers) -> TerminalModifiers {
    TerminalModifiers {
        shift: value.shift,
        alt: value.alt,
        control: value.control,
    }
}

pub(crate) fn available_shells() -> Vec<(String, String)> {
    #[cfg(target_os = "windows")]
    let candidates = vec![("PowerShell", "powershell.exe"), ("Command", "cmd.exe")];
    #[cfg(not(target_os = "windows"))]
    let candidates = vec![
        ("bash", "/bin/bash"),
        ("zsh", "/bin/zsh"),
        ("fish", "/usr/bin/fish"),
    ];
    candidates
        .into_iter()
        .filter(|(_, path)| cfg!(target_os = "windows") || Path::new(path).exists())
        .map(|(label, path)| (label.to_string(), path.to_string()))
        .collect()
}

#[cfg(target_os = "linux")]
fn spawn_external_url(url: &str) -> std::io::Result<std::process::Child> {
    Command::new("xdg-open").arg(url).spawn()
}

#[cfg(target_os = "macos")]
fn spawn_external_url(url: &str) -> std::io::Result<std::process::Child> {
    Command::new("open").arg(url).spawn()
}

#[cfg(target_os = "windows")]
fn spawn_external_url(url: &str) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt as _;

    // The GUI app must not make rundll32 flash a console window.
    let mut command = Command::new("rundll32.exe");
    command
        .args(["url.dll,FileProtocolHandler", url])
        .creation_flags(0x0800_0000);
    command.spawn()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_external_url(_: &str) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "external URL open is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::{cell::Cell, rc::Rc};

    /// A one-cell snapshot for the pure geometry tests.
    fn test_cell(text: &str) -> TerminalCellSnapshot {
        TerminalCellSnapshot {
            row: 0,
            column: 0,
            text: text.to_string(),
            foreground: TerminalCellColor::Named { index: 7 },
            background: TerminalCellColor::Named { index: 0 },
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            hidden: false,
            strikeout: false,
            wide: false,
            wide_spacer: false,
            selected: false,
            hyperlink: None,
        }
    }

    /// Opens a terminal surface whose frame already holds styled output:
    /// `underlined` is underlined, `red` sits on the ANSI red background and the
    /// cursor rests one column past the end of the text.
    fn styled_terminal_surface(
        cx: &mut TestAppContext,
    ) -> (Entity<TerminalSurface>, VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(bind_terminal_keys);
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| TerminalSurface::new(false, window, cx))
            })
            .expect("terminal test window should open")
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window
            .root(&mut cx)
            .expect("terminal test surface should exist");
        surface.update(&mut cx, |surface, _| {
            let tab = surface.tabs.first_mut().expect("terminal tab");
            let session = tab.session.clone();
            let data = b"\x1b[4munderlined\x1b[0m plain \x1b[41mred\x1b[0m".to_vec();
            let snapshot = vibex_terminal::TerminalRawSnapshot {
                session,
                retained_bytes: data.len(),
                chunks: vec![vibex_terminal::TerminalRawOutputChunk { sequence: 1, data }],
                next_sequence: 2,
                dropped_chunks: 0,
            };
            let mut backend = tab.backend.lock().expect("terminal backend");
            backend.sync(&snapshot).expect("terminal sync");
            let frame = backend.frame();
            drop(backend);
            tab.frame.force_full_repaint();
            tab.frame.apply(&frame);
            surface.mark_all_rows_dirty(frame.rows);
        });
        // A manually synced frame is the fixture's source of truth. The live
        // PTY poll would restart the raw sequence at 1 and rebuild the
        // emulator, wiping the fixture between the sync and the assertions.
        surface.update(&mut cx, |surface, _| {
            surface.foreground_active = false;
            surface.poll_task = None;
            surface.blink_task = None;
        });
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.focus_input(window, cx));
            let _ = window.draw(cx);
        });
        (surface, cx)
    }

    /// A run of narrow cells is forced onto the cell pitch; a run holding a wide
    /// glyph is not, because a wide glyph already spans two cells and forcing
    /// every glyph to one cell would overlap it with the text after it.
    #[gpui::test]
    fn only_narrow_runs_are_forced_onto_the_cell_pitch(_cx: &mut TestAppContext) {
        let mut narrow = PendingRun::new(
            TerminalCellStyle {
                foreground: rgb(0xffffff).into(),
                bold: false,
                italic: false,
                underline: false,
                strikeout: false,
            },
            0,
        );
        narrow.push_cell(0, "a", false);
        narrow.push_cell(1, "b", false);
        assert!(narrow.all_narrow);
        assert_eq!(narrow.columns(), 2);

        narrow.push_cell(2, "中", true);
        assert!(
            !narrow.all_narrow,
            "a wide cell must disarm the fixed pitch for the whole run"
        );
        assert_eq!(narrow.columns(), 3);
    }

    #[gpui::test]
    fn grid_shapes_runs_that_start_at_their_own_columns(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let layout = cx.update(|window, cx| {
            let metrics = measured_terminal_cell_metrics(
                cx.text_system().as_ref(),
                cx,
                TerminalSurfaceMode::Standalone,
            );
            shape_terminal_grid(&surface, metrics, window, cx)
        });
        let row = &layout.rows[0];
        // Three styles on one line: underlined, plain, then red-on-ANSI-red.
        // Blank cells between them start no run, so the line becomes three runs
        // and each one begins at the column its first glyph belongs to.
        assert_eq!(
            row.runs.len(),
            3,
            "one run per style change: {:?}",
            row.runs
                .iter()
                .map(|run| (run.column, run.columns, run.line.text.to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(row.runs[0].column, 0);
        assert_eq!(row.runs[0].line.text.as_ref(), "underlined");
        assert_eq!(row.runs[1].column, 11);
        assert_eq!(row.runs[1].line.text.as_ref(), "plain");
        assert_eq!(row.runs[2].column, 17);
        assert_eq!(row.runs[2].line.text.as_ref(), "red");
        assert!(
            layout.rows[1..]
                .iter()
                .all(|row| row.runs.is_empty() && row.rects.is_empty() && row.strokes.is_empty())
        );
    }

    /// A run is shaped with the cell width forced, so a font whose advance does
    /// not match the cell cannot drift the later columns away from the grid.
    #[gpui::test]
    fn shaped_runs_keep_the_measured_cell_width(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let (layout, cell_width) = cx.update(|window, cx| {
            let metrics = measured_terminal_cell_metrics(
                cx.text_system().as_ref(),
                cx,
                TerminalSurfaceMode::Standalone,
            );
            let layout = shape_terminal_grid(&surface, metrics, window, cx);
            let width = layout.metrics.cell_width;
            (layout, width)
        });
        assert!(cell_width > 0.0);
        let run = &layout.rows[0].runs[2];
        assert_eq!(run.columns, 3, "three cells in the run");
        // The shaped advances are the cell width, whatever the font's own
        // advance is: this is what force_width buys.
        let mut seen = Vec::new();
        for glyph in run.line.runs.iter().flat_map(|run| run.glyphs.iter()) {
            seen.push(f32::from(glyph.position.x));
        }
        seen.sort_by(f32::total_cmp);
        for (index, x) in seen.iter().enumerate() {
            let expected = index as f32 * cell_width;
            assert!(
                (x - expected).abs() < 0.51,
                "glyph {index} sits at {x}, expected about {expected}"
            );
        }
        assert_eq!(
            layout.metrics.cell_height,
            f32::from(cx.update(|_, cx| cx.theme().mono_font_size)) * TERMINAL_LINE_HEIGHT_RATIO
        );
    }

    #[gpui::test]
    fn grid_paints_backgrounds_at_cell_geometry(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let (grid_origin, metrics, foreground, background) =
            surface.read_with(&cx, |surface, cx| {
                (
                    surface.grid_bounds.expect("grid bounds").origin,
                    surface.cell_metrics,
                    cx.theme().foreground,
                    cx.theme().background,
                )
            });
        let scale = cx.update(|window, _| window.scale_factor());
        let origin = point(f32::from(grid_origin.x), f32::from(grid_origin.y));
        let cell_width = metrics.cell_width;
        let cell_height = metrics.cell_height;
        // Painted quads snap to the device pixel grid, so a quad edge can land
        // up to one device pixel away from its exact logical position.
        let snap = 1.0 / scale.max(1.0);
        let near = |a: f32, b: f32| (a - b).abs() <= snap + 0.01;
        let quads = cx.update(|window, _| window.painted_quads());

        let solid = |color: Hsla| {
            quads
                .iter()
                .filter(move |quad| quad.background.as_solid() == Some(color))
                .map(|quad| {
                    (
                        quad.bounds.origin.x.as_f32() / scale,
                        quad.bounds.origin.y.as_f32() / scale,
                        quad.bounds.size.width.as_f32() / scale,
                        quad.bounds.size.height.as_f32() / scale,
                    )
                })
                .collect::<Vec<_>>()
        };

        // Three adjacent cells of ANSI red background in the first row are one
        // merged quad: adjacent cells sharing a background paint exactly once.
        let red = solid(indexed_terminal_color(1));
        assert_eq!(red.len(), 1, "one quad for a run of red cells: {red:?}");
        let (x, y, width, height) = red[0];
        assert!(
            near(width, 3.0 * cell_width),
            "run width: {width}, cell width: {cell_width}"
        );
        assert!(near(height, cell_height), "run height: {height}");
        assert!(near(y, origin.y), "red cell should sit in row 0");
        let expected_x = origin.x + 17.0 * cell_width;
        assert!(
            near(x, expected_x),
            "red run should start at column 17: {x} vs {expected_x}"
        );

        // A cell on the default background paints no quad: the grid's own fill
        // stands in for it, which is one fewer quad per frame. The window's own
        // background is a different quad entirely, so the check looks for a
        // cell-sized one.
        let cell_sized_background = solid(background)
            .into_iter()
            .filter(|(_, _, _, height)| near(*height, cell_height))
            .collect::<Vec<_>>();
        assert!(
            cell_sized_background.is_empty(),
            "default-background cells must not paint a quad: {cell_sized_background:?}"
        );

        // The block cursor inverts the cell after the text.
        let cursor = solid(foreground)
            .into_iter()
            .find(|(x, y, width, height)| {
                near(*width, cell_width)
                    && near(*height, cell_height)
                    && near(*y, origin.y)
                    && near(*x, origin.x + 20.0 * cell_width)
            })
            .unwrap_or_else(|| panic!("block cursor quad missing"));
        assert!(cursor.2 > 0.0);
        assert_ne!(foreground, background);
    }

    /// A program that reports its own mouse state is told where the wheel
    /// turned, not just that it did.
    ///
    /// A full-screen CLI hit-tests the wheel against its own layout: the
    /// transcript scrolls only while the pointer is over the transcript, and a
    /// wheel reported at the grid's top-left corner lands on the header and
    /// moves nothing. Sending every wheel as cell (0, 0) is what made an Agent's
    /// scrollback appear frozen.
    #[gpui::test]
    fn a_mouse_reporting_terminal_hears_the_wheel_at_the_pointer(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        // Turn mouse reporting on the way a full-screen program does.
        surface.update(&mut cx, |surface, _| {
            let tab = surface.tabs.first_mut().expect("terminal tab");
            let session = tab.session.clone();
            let data = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h".to_vec();
            // The helper already synced sequence 1, so the mode change arrives
            // as the next chunk and is ingested rather than rejected as stale.
            let snapshot = vibex_terminal::TerminalRawSnapshot {
                session,
                retained_bytes: data.len(),
                chunks: vec![vibex_terminal::TerminalRawOutputChunk { sequence: 2, data }],
                next_sequence: 3,
                dropped_chunks: 0,
            };
            let mut backend = tab.backend.lock().expect("terminal backend");
            backend.sync(&snapshot).expect("terminal sync");
            let frame = backend.frame();
            drop(backend);
            tab.frame.force_full_repaint();
            tab.frame.apply(&frame);
            surface.mouse_log.clear();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            surface.read_with(&cx, |surface, _| surface.tabs[0]
                .frame
                .modes()
                .mouse_reporting),
            "the program has to be reporting the mouse for this to mean anything"
        );

        let (origin, metrics) = surface.read_with(&cx, |surface, _| {
            (
                surface.grid_bounds.expect("grid bounds").origin,
                surface.cell_metrics,
            )
        });
        let inside = |column: u16, row: u16| {
            point(
                origin.x + px((f32::from(column) + 0.5) * metrics.cell_width),
                origin.y + px((f32::from(row) + 0.5) * metrics.cell_height),
            )
        };
        let wheel = |cx: &mut VisualTestContext, position, lines: f32| {
            cx.simulate_event(gpui::ScrollWheelEvent {
                position,
                delta: ScrollDelta::Lines(point(0.0, lines)),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            cx.run_until_parked();
        };

        wheel(&mut cx, inside(3, 2), 3.0);
        wheel(&mut cx, inside(20, 7), -3.0);

        let log = surface.read_with(&cx, |surface, _| surface.mouse_log.clone());
        assert_eq!(
            log.iter()
                .map(|event| (event.kind, event.row, event.column))
                .collect::<Vec<_>>(),
            vec![
                (TerminalMouseKind::WheelUp, 2, 3),
                (TerminalMouseKind::WheelDown, 7, 20),
            ],
            "each wheel report carries the cell the pointer was over: {log:?}"
        );

        // A wheel in the grid's own inset has no cell under it. The fallback is
        // the middle of the grid, not the origin: a program's top-left corner is
        // its header, and a report there scrolls nothing.
        wheel(&mut cx, point(origin.x - px(1.0), origin.y - px(1.0)), 3.0);
        let (rows, columns) = surface.read_with(&cx, |surface, _| {
            (
                surface.tabs[0].frame.rows(),
                surface.tabs[0].frame.columns(),
            )
        });
        let fallback = surface.read_with(&cx, |surface, _| surface.mouse_log.clone());
        let last = fallback.last().expect("the inset wheel still reports");
        assert_eq!(
            (last.kind, last.row, last.column),
            (
                TerminalMouseKind::WheelUp,
                rows.saturating_sub(1) / 2,
                columns.saturating_sub(1) / 2
            ),
            "a wheel outside the cells lands in the middle of the grid: {last:?}"
        );
    }

    #[gpui::test]
    fn grid_maps_pointer_positions_to_cells(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let (origin, metrics) = surface.read_with(&cx, |surface, _| {
            (
                surface.grid_bounds.expect("grid bounds").origin,
                surface.cell_metrics,
            )
        });
        let inside = |column: u16, row: u16| {
            point(
                origin.x + px((f32::from(column) + 0.5) * metrics.cell_width),
                origin.y + px((f32::from(row) + 0.5) * metrics.cell_height),
            )
        };
        let anchor =
            |cx: &VisualTestContext| surface.read_with(cx, |surface, _| surface.selection_anchor);
        let press = |cx: &mut VisualTestContext, position| {
            cx.simulate_mouse_down(position, MouseButton::Left, gpui::Modifiers::default());
        };

        // Anywhere inside a cell selects that cell, not its neighbour.
        for (column, row) in [(0u16, 0u16), (2, 1), (17, 0), (99, 19)] {
            press(&mut cx, inside(column, row));
            assert_eq!(
                anchor(&cx),
                Some(TerminalGridPoint { row, column }),
                "a press inside cell ({row}, {column}) should select it"
            );
        }

        // Dragging across a row selects the cells the pointer travelled over.
        press(&mut cx, inside(0, 0));
        cx.simulate_mouse_move(inside(9, 0), MouseButton::Left, gpui::Modifiers::default());
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.selection_text.clone()),
            Some("underlined".to_string())
        );

        // Presses outside the cells are ignored instead of clamped onto the grid.
        cx.simulate_mouse_up(inside(9, 0), MouseButton::Left, gpui::Modifiers::default());
        press(
            &mut cx,
            point(
                origin.x - px(metrics.cell_width),
                origin.y + px(metrics.cell_height),
            ),
        );
        assert_eq!(anchor(&cx), None);
    }

    #[gpui::test]
    fn a_stale_resize_response_is_reconciled_with_the_visible_grid(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_terminal_keys);
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| TerminalSurface::new(false, window, cx))
            })
            .expect("terminal test window should open")
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window
            .root(&mut cx)
            .expect("terminal test surface should exist");

        let (terminal_id, expected) = surface.read_with(&cx, |surface, _| {
            (
                surface
                    .tabs
                    .first()
                    .expect("terminal tab")
                    .session
                    .id
                    .clone(),
                surface
                    .cell_metrics
                    .dimensions(816.0, 416.0)
                    .expect("grid dimensions"),
            )
        });

        // The panel is open at its full height, but the response still
        // describes the one row the closing transition asked for.
        surface.update(&mut cx, |surface, cx| {
            surface.desired_grid_size = Some((terminal_id.as_str().to_string(), 816.0, 416.0));
            surface.reconcile_resize_after_response(&terminal_id, 1, 20, cx);
        });
        cx.run_until_parked();

        let session = surface.read_with(&cx, |surface, _| {
            surface.tabs.first().expect("terminal tab").session.clone()
        });
        assert_eq!(
            (session.rows, session.cols),
            expected,
            "the terminal should be resized back to the grid, not left at the stale response"
        );
    }

    #[test]
    fn indexed_palette_covers_ansi_cube_and_grayscale() {
        assert_ne!(indexed_terminal_color(0), indexed_terminal_color(15));
        assert_ne!(indexed_terminal_color(16), indexed_terminal_color(231));
        assert_ne!(indexed_terminal_color(232), indexed_terminal_color(255));
    }

    #[test]
    fn named_terminal_defaults_remain_distinct_when_inverse_swaps_them() {
        let foreground: Hsla = rgb(0xfafafa).into();
        let background: Hsla = rgb(0x09090b).into();

        assert_eq!(
            terminal_color(
                TerminalCellColor::Named { index: 256 },
                foreground,
                background,
                foreground,
            ),
            foreground
        );
        assert_eq!(
            terminal_color(
                TerminalCellColor::Named { index: 257 },
                foreground,
                background,
                foreground,
            ),
            background
        );
    }

    #[test]
    fn preview_terminal_uses_tauri_inset_without_duplicate_chrome() {
        assert!(!TerminalSurfaceMode::Preview.shows_chrome());
        assert_eq!(TerminalSurfaceMode::Preview.horizontal_padding(), 8.0);
        assert_eq!(TerminalSurfaceMode::Preview.vertical_padding(), 8.0);

        assert!(!TerminalSurfaceMode::Composer.shows_chrome());
        assert_eq!(TerminalSurfaceMode::Composer.horizontal_padding(), 4.0);
        assert!(TerminalSurfaceMode::Standalone.shows_chrome());
    }

    /// A terminal-mode session gives the whole column to the Agent, so its
    /// surface draws no tabs and no toolbar, and keeps an inset of its own.
    ///
    /// The inset is what makes the rounded window read as a frame: the cells
    /// stop short of the edge on all four sides, so the corner arcs fall on the
    /// window's fill instead of on a glyph.
    #[test]
    fn a_session_terminal_draws_no_chrome() {
        assert!(!TerminalSurfaceMode::Session.shows_chrome());
        assert_eq!(
            TerminalSurfaceMode::Session.horizontal_padding(),
            SESSION_TERMINAL_PADDING
        );
        assert_eq!(
            TerminalSurfaceMode::Session.vertical_padding(),
            SESSION_TERMINAL_PADDING
        );
        // The framed window needs more room than the composer's inline surface:
        // a 4px inset still puts the corner arcs across the first and last
        // cells.
        assert_ne!(SESSION_TERMINAL_PADDING, COMPOSER_TERMINAL_PADDING);
    }

    /// The session page's surface stays transparent.
    ///
    /// The page embeds the grid in a rounded window, and the window's fill is
    /// what the grid's default cells stand on. Painting the page tone here too
    /// would draw an opaque rectangle over the window's rounded corners.
    #[test]
    fn only_the_session_surface_leaves_its_background_to_the_page() {
        assert!(!TerminalSurfaceMode::Session.paints_surface_background());
        assert!(TerminalSurfaceMode::Standalone.paints_surface_background());
        assert!(TerminalSurfaceMode::Preview.paints_surface_background());
        assert!(TerminalSurfaceMode::Composer.paints_surface_background());
    }

    #[gpui::test]
    fn terminal_cell_metrics_clamp_the_font_and_keep_the_insets(cx: &mut TestAppContext) {
        let base = font("Test Mono");
        let metrics = |cx: &mut TestAppContext, size: f32, mode| {
            let text_system = cx.update(|cx| cx.text_system().clone());
            terminal_cell_metrics(text_system.as_ref(), &base, size, mode)
        };

        for (size, mode) in [
            (13.0, TerminalSurfaceMode::Standalone),
            (1.0, TerminalSurfaceMode::Preview),
            (100.0, TerminalSurfaceMode::Composer),
            (f32::NAN, TerminalSurfaceMode::Session),
        ] {
            let measured = metrics(cx, size, mode);
            // The cell is measured, so it is positive and its height is the
            // clamped font size times the line box. A NaN font size lands on the
            // minimum rather than producing a zero-sized grid.
            assert!(measured.cell_width > 0.0, "width for {size}: {measured:?}");
            let clamped = if size.is_finite() {
                size.clamp(TERMINAL_MIN_FONT_SIZE, TERMINAL_MAX_FONT_SIZE)
            } else {
                TERMINAL_MIN_FONT_SIZE
            };
            assert!(
                (measured.cell_height - clamped * TERMINAL_LINE_HEIGHT_RATIO).abs() < 0.01,
                "height for {size}: {measured:?}"
            );
            assert_eq!(measured.horizontal_padding, mode.horizontal_padding());
            assert_eq!(measured.vertical_padding, mode.vertical_padding());
        }

        // Two sizes scale the cell by their ratio, because both are measured
        // from the same font.
        let small = metrics(cx, 11.0, TerminalSurfaceMode::Standalone);
        let large = metrics(cx, 22.0, TerminalSurfaceMode::Standalone);
        assert!((large.cell_width / small.cell_width - 2.0).abs() < 0.05);
        assert!((large.cell_height / small.cell_height - 2.0).abs() < 0.05);
    }

    #[test]
    fn scroll_deltas_accumulate_into_whole_lines() {
        // A trackpad reports sub-row deltas; carrying the remainder is what
        // turns a slow gesture into an actual scroll instead of rounding every
        // event to zero, and a fast one into exactly the lines it travelled.
        let mut remainder = 0.0;

        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.4), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.4), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.4), 1);
        assert!(remainder < 0.25);

        // Truncation is toward zero, so a reversal spends the remainder it
        // already had instead of jumping a whole line early.
        assert_eq!(accumulate_scroll_lines(&mut remainder, -1.0), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, -1.0), -1);
        assert_eq!(accumulate_scroll_lines(&mut remainder, -0.5), -1);
        assert_eq!(accumulate_scroll_lines(&mut remainder, f32::NAN), 0);
        // A gesture that reverses direction spends the remainder it had.
        let mut remainder = 0.75;
        assert_eq!(accumulate_scroll_lines(&mut remainder, -0.5), 0);
        assert!(remainder > 0.0 && remainder < 0.3);
    }

    #[test]
    fn block_and_shade_characters_paint_as_rectangles() {
        // Half blocks and the shade ramp are geometry, not ink: they have to
        // cover their cell edge to edge whatever the font draws for them.
        assert_eq!(block_char_to_rect('▀'), Some((0, 0, 8, 12)));
        assert_eq!(block_char_to_rect('█'), Some((0, 0, 8, 24)));
        assert_eq!(block_char_to_rect('▌'), Some((0, 0, 4, 24)));
        assert_eq!(block_char_to_rect('▐'), Some((4, 0, 4, 24)));
        assert_eq!(block_char_to_rect('▁'), Some((0, 21, 8, 3)));
        assert_eq!(block_char_to_rect('a'), None);

        assert_eq!(quadrant_char_to_filled_bits('▛'), Some(0b0111));
        assert_eq!(quadrant_char_to_filled_bits('▞'), Some(0b0110));
        assert_eq!(quadrant_char_to_filled_bits('x'), None);

        assert_eq!(sextant_char_to_filled_bits('\u{1FB00}'), Some(1));
        // The all-filled sextant is `█`, which Block Elements already owns, so
        // the range stops one short of it.
        assert_eq!(sextant_char_to_filled_bits('\u{1FB3B}'), Some(0b111110));
        assert_eq!(sextant_char_to_filled_bits('x'), None);

        assert_eq!(shade_char_opacity('░'), Some(0.25));
        assert_eq!(shade_char_opacity('▒'), Some(0.5));
        assert_eq!(shade_char_opacity('▓'), Some(0.75));
        assert_eq!(shade_char_opacity('█'), None);

        // Connectors keep their exact colour so a border stays continuous.
        assert!(is_decorative_character('─'));
        assert!(is_decorative_character('█'));
        assert!(is_decorative_character('▶'));
        assert!(is_decorative_character('\u{E0B0}'));
        assert!(!is_decorative_character('a'));
    }

    #[test]
    fn braille_patterns_paint_as_dot_matrices() {
        // The code point itself is the dot mask; U+2800 is a blank cell and
        // must stay geometry so it paints nothing instead of a missing glyph.
        assert_eq!(braille_char_to_dots('⠀'), Some(0));
        assert_eq!(braille_char_to_dots('⠁'), Some(0b0000_0001));
        assert_eq!(braille_char_to_dots('⣿'), Some(0xFF));
        assert_eq!(braille_char_to_dots('⣴'), Some(('⣴' as u32 - 0x2800) as u8));
        assert_eq!(braille_char_to_dots('\u{27FF}'), None);
        assert_eq!(braille_char_to_dots('\u{2900}'), None);
        assert_eq!(braille_char_to_dots('a'), None);

        assert!(braille_dot_fills(0).is_empty());

        let all = braille_dot_fills(0xFF);
        assert_eq!(all.len(), 8);
        for &(column, line, width, height) in &all {
            assert_eq!((width, height), (2, 3));
            assert!(column + width <= 8, "dot leaves the cell: {column}");
            assert!(line + height <= 24, "dot leaves the cell: {line}");
        }
        let positions = all
            .iter()
            .map(|&(column, line, _, _)| (column, line))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(positions.len(), 8, "dots must not share a cell slot");

        // Unicode order: dots 1-3 run down the left, 4-6 down the right, then
        // 7 and 8 along the bottom.
        assert_eq!(braille_dot_fills(0b0000_0001), vec![(1, 2, 2, 3)]);
        assert_eq!(braille_dot_fills(0b0000_1000), vec![(5, 2, 2, 3)]);
        assert_eq!(braille_dot_fills(0b0100_0000), vec![(1, 20, 2, 3)]);
        assert_eq!(braille_dot_fills(0b1000_0000), vec![(5, 20, 2, 3)]);

        // Braille art is decorative: its ink keeps the program's exact colour
        // instead of being lifted for contrast.
        assert!(is_decorative_character('⣿'));
        assert!(!is_decorative_character('\u{27FF}'));

        // Geometry, not text: the cell returns its dots directly, and a blank
        // Braille cell stays geometry so it paints nothing.
        let filled = cell_geometry(Some(&test_cell("⣿"))).expect("braille is geometry");
        assert_eq!(filled.fills.len(), 8);
        assert!(!filled.merge_adjacent, "dots must not merge across cells");

        let blank = cell_geometry(Some(&test_cell("⠀"))).expect("blank braille is geometry");
        assert!(blank.fills.is_empty());

        assert!(
            cell_geometry(Some(&test_cell("█")))
                .expect("block is geometry")
                .merge_adjacent
        );
        assert!(cell_geometry(Some(&test_cell("a"))).is_none());
    }

    #[test]
    fn low_contrast_ink_is_lifted_until_it_reads() {
        let background: Hsla = rgb(0x101014).into();
        // A theme that hands out a near-background ink is corrected.
        let washed: Hsla = rgb(0x14141a).into();
        assert!(contrast_ratio(washed, background) < TERMINAL_MINIMUM_CONTRAST);
        let lifted = ensure_minimum_contrast(washed, background);
        assert!(
            contrast_ratio(lifted, background) >= TERMINAL_MINIMUM_CONTRAST,
            "lifted ink should clear the floor"
        );

        // Ink that already reads is left exactly as the program chose it.
        let readable: Hsla = rgb(0xfafafa).into();
        assert_eq!(ensure_minimum_contrast(readable, background), readable);

        // On a light background the correction walks the other way.
        let light: Hsla = rgb(0xfdfdfd).into();
        let faint: Hsla = rgb(0xf4f4f6).into();
        let darkened = ensure_minimum_contrast(faint, light);
        assert!(relative_luminance(darkened) < relative_luminance(faint));
    }

    #[test]
    fn only_theme_chosen_colours_are_contrast_adjusted() {
        // ANSI slots 0..=15 come from the theme and may clash with it.
        assert!(is_theme_chosen_color(TerminalCellColor::Named { index: 1 }));
        assert!(is_theme_chosen_color(TerminalCellColor::Indexed {
            index: 9
        }));
        // A program's own 256-colour pick and its true colour do not.
        assert!(!is_theme_chosen_color(TerminalCellColor::Indexed {
            index: 16
        }));
        assert!(!is_theme_chosen_color(TerminalCellColor::Rgb {
            red: 1,
            green: 2,
            blue: 3
        }));
    }

    #[test]
    fn terminal_polling_backs_off_after_consecutive_idle_frames() {
        assert_eq!(terminal_poll_interval(0), TERMINAL_ACTIVE_POLL_INTERVAL);
        assert_eq!(terminal_poll_interval(3), TERMINAL_ACTIVE_POLL_INTERVAL);
        assert_eq!(terminal_poll_interval(4), TERMINAL_IDLE_POLL_INTERVAL);
        assert_eq!(
            terminal_poll_interval(u16::MAX),
            TERMINAL_IDLE_POLL_INTERVAL
        );
    }

    #[test]
    fn shell_candidates_never_include_missing_paths_on_unix() {
        #[cfg(not(target_os = "windows"))]
        assert!(
            available_shells()
                .iter()
                .all(|(_, path)| Path::new(path).exists())
        );
    }

    #[gpui::test]
    fn native_input_handler_routes_text_keys_and_ime(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_terminal_keys);
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| TerminalSurface::new(false, window, cx))
            })
            .expect("terminal test window should open")
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let surface = window
            .root(&mut cx)
            .expect("terminal test surface should exist");

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.focus_input(window, cx));
            let _ = window.draw(cx);
        });
        surface.update(&mut cx, |surface, _| surface.input_log.clear());
        let input_notifications = Rc::new(Cell::new(0));
        cx.update(|_, cx| {
            let input_notifications = input_notifications.clone();
            cx.observe(&surface, move |_, _| {
                input_notifications.set(input_notifications.get() + 1);
            })
            .detach();
        });

        let expected_keys = surface.read_with(&cx, |surface, _| {
            [
                (TerminalKey::Enter, TerminalModifiers::default()),
                (TerminalKey::Backspace, TerminalModifiers::default()),
                (TerminalKey::Tab, TerminalModifiers::default()),
                (
                    TerminalKey::Tab,
                    TerminalModifiers {
                        shift: true,
                        ..TerminalModifiers::default()
                    },
                ),
                (TerminalKey::Left, TerminalModifiers::default()),
                (TerminalKey::Right, TerminalModifiers::default()),
                (TerminalKey::Up, TerminalModifiers::default()),
                (TerminalKey::Down, TerminalModifiers::default()),
                (TerminalKey::Home, TerminalModifiers::default()),
                (TerminalKey::End, TerminalModifiers::default()),
                (
                    TerminalKey::Text('c'),
                    TerminalModifiers {
                        control: true,
                        ..TerminalModifiers::default()
                    },
                ),
                (
                    TerminalKey::Text('x'),
                    TerminalModifiers {
                        alt: true,
                        ..TerminalModifiers::default()
                    },
                ),
            ]
            .into_iter()
            .map(|(key, modifiers)| {
                surface
                    .encode_active_key(key, modifiers)
                    .expect("test key should encode")
            })
            .collect::<Vec<_>>()
        });

        let burst = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
        surface.update(&mut cx, |surface, cx| {
            // This test is about the poll loop restarting after input, so the
            // fixture's quiescence is lifted here.
            surface.foreground_active = true;
            surface.start_polling(cx);
            surface.idle_poll_count = TERMINAL_IDLE_POLL_THRESHOLD;
        });
        cx.simulate_input(burst);
        cx.simulate_keystrokes(
            "enter backspace tab shift-tab left right up down home end ctrl-c alt-x",
        );

        let mut expected = burst.bytes().map(|byte| vec![byte]).collect::<Vec<_>>();
        expected.extend(expected_keys);
        let actual = surface.read_with(&cx, |surface, _| surface.input_log.clone());
        assert_eq!(actual, expected);
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.idle_poll_count),
            0,
            "input after idle backoff should restore low-latency polling"
        );
        assert!(surface.read_with(&cx, |surface, _| surface.poll_task.is_some()));
        assert!(cx.update(|window, cx| surface.read(cx).focus.is_focused(window)));
        assert_eq!(
            input_notifications.get(),
            0,
            "successful terminal input should wait for PTY output before repainting"
        );

        surface.update(&mut cx, |surface, _| surface.input_log.clear());
        let marked_bounds = cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                EntityInputHandler::replace_and_mark_text_in_range(
                    surface,
                    None,
                    "中文",
                    Some(2..2),
                    window,
                    cx,
                );
                assert_eq!(
                    EntityInputHandler::marked_text_range(surface, window, cx),
                    Some(0..2)
                );
                EntityInputHandler::bounds_for_range(
                    surface,
                    0..0,
                    Bounds::new(point(px(100.0), px(200.0)), size(px(800.0), px(400.0))),
                    window,
                    cx,
                )
            })
        });
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.input_log.clone()),
            Vec::<Vec<u8>>::new()
        );
        let marked_bounds = marked_bounds.expect("IME bounds should follow the terminal cursor");
        let cell_height = surface.read_with(&cx, |surface, _| surface.cell_metrics.cell_height);
        assert_eq!(marked_bounds.size.height, px(cell_height));

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                EntityInputHandler::replace_text_in_range(surface, None, "中文", window, cx);
            });
        });
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.input_log.clone()),
            vec!["中文".as_bytes().to_vec()]
        );
        assert!(surface.read_with(&cx, |surface, _| surface.marked_text.is_none()));
    }

    /// Appends one raw chunk after the helper's initial sync and applies it the
    /// way a poll would.
    fn append_terminal_output(
        surface: &Entity<TerminalSurface>,
        cx: &mut VisualTestContext,
        sequence: i64,
        data: &[u8],
    ) {
        surface.update(cx, |surface, _| {
            let tab = surface.tabs.first_mut().expect("terminal tab");
            let snapshot = vibex_terminal::TerminalRawSnapshot {
                session: tab.session.clone(),
                retained_bytes: data.len(),
                chunks: vec![vibex_terminal::TerminalRawOutputChunk {
                    sequence,
                    data: data.to_vec(),
                }],
                next_sequence: sequence + 1,
                dropped_chunks: 0,
            };
            let mut backend = tab.backend.lock().expect("terminal backend");
            backend.sync(&snapshot).expect("terminal sync");
            let frame = backend.frame();
            drop(backend);
            tab.frame.apply(&frame);
            surface.mark_all_rows_dirty(frame.rows);
        });
    }

    fn draw_terminal(surface: &Entity<TerminalSurface>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            surface.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        });
    }

    fn many_lines(count: usize) -> String {
        let mut data = String::new();
        for index in 0..count {
            data.push_str(&format!("line-{index:03}\r\n"));
        }
        data
    }

    #[gpui::test]
    fn shift_page_keys_move_through_the_scrollback(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        append_terminal_output(&surface, &mut cx, 2, many_lines(80).as_bytes());
        draw_terminal(&surface, &mut cx);
        assert!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.history_lines()) > 0,
            "the fixture has to produce scrollback for this to mean anything"
        );
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()),
            0
        );

        cx.simulate_keystrokes("shift-pageup");
        cx.run_until_parked();
        assert!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()) > 0,
            "Shift+PageUp must reach the scrollback, not the program"
        );

        cx.simulate_keystrokes("shift-pagedown");
        cx.run_until_parked();
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()),
            0,
            "Shift+PageDown returns to the live screen"
        );
    }

    #[gpui::test]
    fn a_full_screen_program_hears_the_wheel_as_arrow_keys(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        append_terminal_output(&surface, &mut cx, 2, b"\x1b[?1049h");
        draw_terminal(&surface, &mut cx);
        assert!(surface.read_with(&cx, |surface, _| {
            surface.tabs[0].frame.modes().alternate_screen
        }));
        surface.update(&mut cx, |surface, _| surface.input_log.clear());

        let origin = surface.read_with(&cx, |surface, _| {
            surface.grid_bounds.expect("grid bounds").origin
        });
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(origin.x + px(20.0), origin.y + px(20.0)),
            delta: ScrollDelta::Lines(point(0.0, 3.0)),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        let sent = surface.read_with(&cx, |surface, _| surface.input_log.clone());
        assert_eq!(
            sent,
            vec![b"\x1b[A".to_vec(); 3],
            "each wheel notch becomes the arrows the full-screen program handles"
        );
    }

    #[gpui::test]
    fn the_scrollbar_thumb_moves_the_viewport(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        append_terminal_output(&surface, &mut cx, 2, many_lines(200).as_bytes());
        draw_terminal(&surface, &mut cx);
        let history = surface.read_with(&cx, |surface, _| surface.tabs[0].frame.history_lines());
        assert!(history > 0);
        let geometry = surface
            .read_with(&cx, |surface, _| surface.scrollbar_geometry())
            .expect("a terminal with scrollback draws a scrollbar");

        // The top of the track is the oldest retained line.
        let top = point(
            geometry.track.origin.x + px(1.0),
            geometry.track.origin.y + px(1.0),
        );
        surface.update(&mut cx, |surface, cx| {
            assert!(surface.begin_scrollbar_drag(top, cx));
        });
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()),
            history,
            "clicking the top of the track scrolls to the oldest line"
        );

        // The bottom returns to the live screen.
        let bottom = point(
            geometry.track.origin.x + px(1.0),
            geometry.track.origin.y + geometry.track.size.height - px(1.0),
        );
        surface.update(&mut cx, |surface, cx| {
            let _ = surface.drag_scrollbar(bottom, cx);
            surface.end_scrollbar_drag();
        });
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()),
            0
        );
    }

    #[gpui::test]
    fn double_and_triple_clicks_select_a_word_and_a_line(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let click = |count: usize| MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(0.0), px(0.0)),
            modifiers: Default::default(),
            click_count: count,
            first_mouse: false,
        };

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.on_cell_mouse_down(
                    TerminalGridPoint { row: 0, column: 2 },
                    &click(2),
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.selection_text.clone()),
            Some("underlined".to_string()),
            "a double click takes the word the emulator's word boundaries name"
        );

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.on_cell_mouse_down(
                    TerminalGridPoint { row: 0, column: 15 },
                    &click(3),
                    window,
                    cx,
                );
            });
        });
        let line = surface
            .read_with(&cx, |surface, _| surface.selection_text.clone())
            .expect("a triple click selects the row");
        assert!(
            line.contains("underlined"),
            "the whole line is selected: {line:?}"
        );
        assert!(line.contains("red"), "the whole line is selected: {line:?}");
    }

    #[gpui::test]
    fn a_plain_url_in_a_row_is_detected_under_the_pointer(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        append_terminal_output(
            &surface,
            &mut cx,
            2,
            b"\r\nsee https://example.com/docs now",
        );
        draw_terminal(&surface, &mut cx);

        let link = surface.read_with(&cx, |surface, _| {
            surface.link_at(TerminalGridPoint { row: 1, column: 9 })
        });
        let link = link.expect("the URL in the row should be detected");
        assert_eq!(link.url, "https://example.com/docs");
        assert_eq!(link.columns, 4..28);
        // A cell just past the URL is not part of it.
        assert!(
            surface
                .read_with(&cx, |surface, _| surface
                    .link_at(TerminalGridPoint { row: 1, column: 30 }))
                .is_none()
        );
    }

    #[gpui::test]
    fn search_navigates_to_a_match_above_the_viewport(cx: &mut TestAppContext) {
        let (surface, mut cx) = styled_terminal_surface(cx);
        let mut data = b"needle at the top\r\n".to_vec();
        data.extend(many_lines(80).as_bytes());
        append_terminal_output(&surface, &mut cx, 2, &data);
        draw_terminal(&surface, &mut cx);

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.search_open = true;
                surface
                    .search_input
                    .update(cx, |input, cx| input.set_value("needle", window, cx));
                surface.refresh_search(cx);
            });
        });

        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.search_matches.len()),
            1,
            "the scan has to reach the oldest retained line"
        );
        assert!(
            surface.read_with(&cx, |surface, _| surface.tabs[0].frame.display_offset()) > 0,
            "locating the match scrolls it into view"
        );
        assert_eq!(
            surface.read_with(&cx, |surface, _| surface.selection_text.clone()),
            Some("needle".to_string())
        );
    }
}
