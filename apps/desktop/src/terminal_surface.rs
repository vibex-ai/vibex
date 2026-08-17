use std::{
    collections::BTreeSet,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, FontWeight, Hsla, IntoElement, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, Subscription, Task, UTF16Selection, WeakEntity, Window, actions, canvas, div,
    point, prelude::*, px, rgb, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
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
use vibex_markdown::code_font_weight;
use vibex_terminal::{
    TerminalCellColor, TerminalCellSnapshot, TerminalCursorShape, TerminalFrameSnapshot,
    TerminalGridPoint, TerminalManager,
};

const TERMINAL_BASE_FONT_SIZE: f32 = 13.0;
const TERMINAL_BASE_CELL_WIDTH: f32 = 8.0;
const TERMINAL_BASE_CELL_HEIGHT: f32 = 16.0;
const TERMINAL_MIN_FONT_SIZE: f32 = 10.0;
const TERMINAL_MAX_FONT_SIZE: f32 = 24.0;
const TERMINAL_HORIZONTAL_PADDING: f32 = 12.0;
const TERMINAL_VERTICAL_PADDING: f32 = 12.0;
// Tauri preview terminals use `p-2`, while the Composer surface has another
// compact wrapper around its grid. Keep those insets distinct so a preview
// terminal does not gain standalone chrome or lose the expected content edge.
const PREVIEW_TERMINAL_PADDING: f32 = 8.0;
const COMPOSER_TERMINAL_PADDING: f32 = 4.0;
const TERMINAL_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(16);
const TERMINAL_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINAL_IDLE_POLL_THRESHOLD: u16 = 4;
const TERMINAL_RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);
const TERMINAL_CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
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
}

impl TerminalSurfaceMode {
    fn shows_chrome(self) -> bool {
        self == Self::Standalone
    }

    fn horizontal_padding(self) -> f32 {
        match self {
            Self::Standalone => TERMINAL_HORIZONTAL_PADDING,
            Self::Preview => PREVIEW_TERMINAL_PADDING,
            Self::Composer => COMPOSER_TERMINAL_PADDING,
        }
    }

    fn vertical_padding(self) -> f32 {
        match self {
            Self::Standalone => TERMINAL_VERTICAL_PADDING,
            Self::Preview => PREVIEW_TERMINAL_PADDING,
            Self::Composer => COMPOSER_TERMINAL_PADDING,
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
        }
    }
}

#[derive(Clone)]
struct TerminalPollWork {
    terminal_id: TerminalId,
    next_sequence: i64,
    manager: TerminalManager,
    backend: SharedTerminalBackend,
}

struct TerminalPollResult {
    terminal_id: TerminalId,
    session: Option<TerminalSession>,
    result: Result<Option<(TerminalSyncOutcome, TerminalFrameSnapshot)>, String>,
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

pub struct TerminalSurface {
    manager: TerminalManager,
    workspace_root: PathBuf,
    workspace_id: WorkspaceId,
    tabs: Vec<TerminalTab>,
    active_tab: Option<usize>,
    search_input: Entity<InputState>,
    search_open: bool,
    search_matches: Vec<vibex_terminal::TerminalSearchMatch>,
    active_search_match: usize,
    marked_text: Option<String>,
    selection_anchor: Option<TerminalGridPoint>,
    selection_text: Option<String>,
    active_hyperlink: Option<String>,
    focus: FocusHandle,
    cell_metrics: TerminalCellMetrics,
    resize: TerminalResizeCoordinator,
    resize_task: Option<Task<()>>,
    last_grid_bounds: Option<(String, u32, u32)>,
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
}

impl TerminalSurface {
    pub fn new(
        physical_marker_shortcut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let manager = new_ui_terminal_manager();
        let workspace_root = std::env::var_os("VIBEX_NATIVE_CONTENT_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let workspace_id = WorkspaceId::new();
        let initial = manager.create(
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
            manager,
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
        let workspace_id = session.workspace_id.clone();
        Self::build(
            manager,
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

    #[allow(dead_code)]
    pub fn from_preview_shared_session(
        manager: TerminalManager,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            manager,
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

    #[allow(dead_code)]
    pub fn from_embedded_shared_session(
        manager: TerminalManager,
        workspace_root: PathBuf,
        session: TerminalSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = session.workspace_id.clone();
        Self::build(
            manager,
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
        manager: TerminalManager,
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
            this.send_focus(true);
            window.invalidate_character_coordinates();
            cx.notify();
        });
        let focus_out = cx.on_focus_out(&focus, window, |this, _, window, cx| {
            this.send_focus(false);
            if this.marked_text.take().is_some() {
                window.invalidate_character_coordinates();
                cx.notify();
            }
        });
        let cell_metrics = terminal_cell_metrics(f32::from(cx.theme().mono_font_size), mode);
        let resize = TerminalResizeCoordinator::new(cell_metrics);
        let owned_terminal_ids = if owns_initial_sessions {
            tabs.iter()
                .map(|tab| tab.session.id.as_str().to_string())
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut this = Self {
            manager,
            workspace_root,
            workspace_id,
            tabs,
            active_tab,
            search_input,
            search_open: false,
            search_matches: Vec::new(),
            active_search_match: 0,
            marked_text: None,
            selection_anchor: None,
            selection_text: None,
            active_hyperlink: None,
            focus,
            cell_metrics,
            resize,
            resize_task: None,
            last_grid_bounds: None,
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
            _subscriptions: vec![focus_in, focus_out],
            #[cfg(test)]
            input_log: Vec::new(),
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
                    this.foreground_active.then(|| this.poll_work())
                }) else {
                    break;
                };
                let Some(work) = work else {
                    break;
                };
                if work.is_empty() {
                    background.timer(TERMINAL_IDLE_POLL_INTERVAL).await;
                    continue;
                }
                let results = background
                    .spawn(async move { work.into_iter().map(run_poll_work).collect::<Vec<_>>() })
                    .await;
                let Ok(interval) = entity.update(cx, |this, cx| {
                    if !this.foreground_active {
                        return None;
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
                manager: self.manager.clone(),
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
        for result in results {
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
                    let update = tab.frame.apply(&frame);
                    if update.full_repaint {
                        tab.full_repaints = tab.full_repaints.saturating_add(1);
                    } else {
                        tab.partial_repaints = tab.partial_repaints.saturating_add(1);
                    }
                    tab.changed_rows = tab
                        .changed_rows
                        .saturating_add(update.changed_rows.len() as u64);
                    if outcome.gap_detected {
                        tab.sequence_gaps = tab.sequence_gaps.saturating_add(1);
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
        }
        if changed {
            self.refresh_marker_observation();
            if self.search_open {
                self.refresh_search(cx);
            }
            cx.notify();
        }
        changed
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
        let cell_metrics = terminal_cell_metrics(f32::from(cx.theme().mono_font_size), self.mode);
        if self.cell_metrics != cell_metrics {
            self.cell_metrics = cell_metrics;
            self.resize = TerminalResizeCoordinator::new(cell_metrics);
            self.resize_task = None;
            self.last_grid_bounds = None;
        }
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
        match self.manager.resize(&request) {
            Ok(session) => {
                if let Some(tab) = self
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
                self.note = format!("{} rows x {} columns", request.rows, request.cols);
                self.last_error_code = None;
            }
            Err(error) => {
                self.note = format!("Resize failed: {}", error.message);
                self.last_error_code = Some("terminal_resize_failed");
            }
        }
        cx.notify();
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

    fn write_active(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<(), ()> {
        let Some((terminal_id, _)) = self.active_runtime() else {
            self.note = "No active terminal".into();
            self.last_error_code = Some("terminal_not_found");
            return Err(());
        };
        #[cfg(test)]
        self.input_log.push(bytes.to_vec());
        self.manager
            .write_bytes(&terminal_id, bytes)
            .map_err(|error| {
                self.note = format!("Terminal write failed: {}", error.message);
                self.last_error_code = Some("terminal_write_failed");
            })?;
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

    fn send_focus(&mut self, focused: bool) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let sequence = backend
            .lock()
            .ok()
            .and_then(|backend| backend.encode_focus(focused).map(<[u8]>::to_vec));
        if let Some(sequence) = sequence {
            let _ = self.manager.write_bytes(&terminal_id, &sequence);
        }
    }

    fn new_tab(&mut self, shell: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.tabs.len().saturating_add(1);
        match self.manager.create(
            &self.workspace_root,
            TerminalCreateRequest {
                workspace_id: self.workspace_id.clone(),
                title: Some(format!("Terminal {number}")),
                shell,
                cwd: Some(self.workspace_root.display().to_string()),
                rows: 20,
                cols: 100,
            },
        ) {
            Ok(session) => {
                self.owned_terminal_ids
                    .insert(session.id.as_str().to_string());
                self.tabs.push(TerminalTab::new(session));
                self.active_tab = Some(self.tabs.len() - 1);
                self.clear_interaction_state();
                self.focus.focus(window, cx);
                self.last_error_code = None;
            }
            Err(error) => {
                self.note = format!("New terminal failed: {}", error.message);
                self.last_error_code = Some("terminal_create_failed");
            }
        }
        cx.notify();
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
        let manager = self.manager.clone();
        let terminal_id = tab.session.id;
        cx.background_spawn(async move {
            let _ = manager.kill(&terminal_id);
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
        let _ = self.manager.kill(&session.id);
        match self.manager.restore(&self.workspace_root, session) {
            Ok(session) => {
                self.tabs[index] = TerminalTab::new(session);
                self.note = "Terminal restarted with the same Vibex id".into();
                self.last_error_code = None;
            }
            Err(error) => {
                self.note = format!("Restart failed: {}", error.message);
                self.last_error_code = Some("terminal_restart_failed");
            }
        }
        self.clear_interaction_state();
        cx.notify();
    }

    fn switch_shell(&mut self, shell: String, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            return;
        };
        let terminal_id = self.tabs[index].session.id.clone();
        match self
            .manager
            .switch_shell(&TerminalSwitchShellRequest { terminal_id, shell })
        {
            Ok(session) => {
                self.tabs[index] = TerminalTab::new(session);
                self.note = "Shell switched without creating a second terminal domain".into();
                self.last_error_code = None;
            }
            Err(error) => {
                self.note = format!("Shell switch failed: {}", error.message);
                self.last_error_code = Some("terminal_shell_switch_failed");
            }
        }
        self.clear_interaction_state();
        cx.notify();
    }

    fn clear_interaction_state(&mut self) {
        self.marked_text = None;
        self.selection_anchor = None;
        self.selection_text = None;
        self.active_hyperlink = None;
        self.search_matches.clear();
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
            self.active_search_match = 0;
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_string();
        self.search_matches = self
            .active_runtime()
            .and_then(|(_, backend)| {
                backend
                    .lock()
                    .ok()
                    .map(|backend| backend.find_visible(&query, false))
            })
            .unwrap_or_default();
        self.active_search_match = self
            .active_search_match
            .min(self.search_matches.len().saturating_sub(1));
        self.apply_active_search_match(cx);
    }

    fn select_next_search_match(&mut self, cx: &mut Context<Self>) {
        if !self.search_matches.is_empty() {
            self.active_search_match = (self.active_search_match + 1) % self.search_matches.len();
            self.apply_active_search_match(cx);
        }
    }

    fn apply_active_search_match(&mut self, cx: &mut Context<Self>) {
        let Some(search_match) = self.search_matches.get(self.active_search_match).copied() else {
            return;
        };
        self.select_range(search_match.start, search_match.end, cx);
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
            );
        } else {
            self.selection_anchor = Some(point);
            self.select_range(point, point, cx);
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
                );
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
            );
        } else if self.selection_anchor == Some(point) {
            self.active_hyperlink = hyperlink;
        }
        self.selection_anchor = None;
        cx.notify();
    }

    fn send_mouse(
        &mut self,
        kind: TerminalMouseKind,
        button: u8,
        point: TerminalGridPoint,
        modifiers: TerminalModifiers,
    ) {
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let bytes = backend.lock().ok().and_then(|backend| {
            backend.encode_mouse(TerminalMouseEvent {
                kind,
                button,
                row: point.row,
                column: point.column,
                modifiers,
            })
        });
        if let Some(bytes) = bytes {
            let _ = self.manager.write_bytes(&terminal_id, &bytes);
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let cell_metrics = terminal_cell_metrics(f32::from(cx.theme().mono_font_size), self.mode);
        let y = match event.delta {
            ScrollDelta::Lines(point) => point.y,
            ScrollDelta::Pixels(point) => f32::from(point.y) / cell_metrics.cell_height,
        };
        if y == 0.0 {
            return;
        }
        let modes = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.frame.modes());
        if modes.is_some_and(|modes| modes.mouse_reporting) && !event.modifiers.shift {
            let kind = if y > 0.0 {
                TerminalMouseKind::WheelUp
            } else {
                TerminalMouseKind::WheelDown
            };
            self.send_mouse(
                kind,
                0,
                TerminalGridPoint { row: 0, column: 0 },
                terminal_modifiers(event.modifiers),
            );
            return;
        }
        let Some((terminal_id, backend)) = self.active_runtime() else {
            return;
        };
        let frame = backend.lock().ok().map(|mut backend| {
            backend.scroll(if y > 0.0 { 3 } else { -3 });
            backend.frame()
        });
        if let Some(frame) = frame {
            self.apply_local_frame(&terminal_id, frame);
            cx.notify();
        }
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
        h_flex()
            .h(px(34.0))
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let status = terminal_status_label(tab.session.status);
                Button::new(format!("terminal-tab-{index}"))
                    .small()
                    .ghost()
                    .icon(IconName::SquareTerminal)
                    .label(format!("{} · {status}", tab.session.title))
                    .selected(active == Some(index))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)),
                    )
            }))
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
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Terminal is unavailable")
                .into_any_element();
        };
        let resize_entity = cx.weak_entity();
        let input_entity = cx.entity();
        let input_focus = self.focus.clone();
        let rows = tab.frame.rows();
        let columns = tab.frame.columns();
        let cursor = tab.frame.cursor();
        let marked_text = self.marked_text.clone();
        let horizontal_padding = self.horizontal_padding();
        let vertical_padding = self.vertical_padding();
        let cell_metrics = terminal_cell_metrics(f32::from(cx.theme().mono_font_size), self.mode);
        let mut cells = vec![None; usize::from(rows).saturating_mul(usize::from(columns))];
        for cell in tab.frame.cells() {
            let index = usize::from(cell.row)
                .saturating_mul(usize::from(columns))
                .saturating_add(usize::from(cell.column));
            if let Some(slot) = cells.get_mut(index) {
                *slot = Some(cell.clone());
            }
        }
        v_flex()
            .id("terminal-grid")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().background)
            .px(px(horizontal_padding))
            .py(px(vertical_padding))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .font_weight(code_font_weight(cx))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                }),
            )
            .children((0..rows).map(|row| {
                h_flex()
                    .h(px(cell_metrics.cell_height))
                    .flex_none()
                    .overflow_hidden()
                    .children((0..columns).map(|column| {
                        let point = TerminalGridPoint { row, column };
                        let index = usize::from(row)
                            .saturating_mul(usize::from(columns))
                            .saturating_add(usize::from(column));
                        self.render_cell(
                            point,
                            cells.get(index).cloned().flatten(),
                            cursor,
                            cell_metrics,
                            cx,
                        )
                    }))
            }))
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
                let _ =
                    resize_entity.update(cx, |this, cx| this.schedule_resize(width, height, cx));
            })
            .into_any_element()
    }

    fn render_cell(
        &self,
        point: TerminalGridPoint,
        cell: Option<TerminalCellSnapshot>,
        cursor: Option<vibex_terminal::TerminalCursorSnapshot>,
        cell_metrics: TerminalCellMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = cell.unwrap_or_else(|| empty_cell(point));
        if cell.wide_spacer {
            return div()
                .w_0()
                .h(px(cell_metrics.cell_height))
                .flex_none()
                .into_any_element();
        }
        let cursor = cursor.filter(|cursor| {
            self.marked_text.is_none()
                && self.cursor_visible
                && cursor.row == point.row
                && cursor.column == point.column
        });
        let default_foreground = cx.theme().foreground;
        let default_background = cx.theme().background;
        let mut foreground = terminal_color(
            cell.foreground,
            default_foreground,
            default_background,
            default_foreground,
        );
        let mut background = terminal_color(
            cell.background,
            default_foreground,
            default_background,
            default_background,
        );
        if cell.dim {
            foreground = foreground.opacity(0.68);
        }
        if cell.selected {
            background = default_foreground.opacity(0.28);
        }
        if cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Block) {
            background = default_foreground;
            foreground = default_background;
        }
        let hyperlink = cell.hyperlink.clone();
        div()
            .id(format!("terminal-cell-{}-{}", point.row, point.column))
            .w(px(if cell.wide {
                cell_metrics.cell_width * 2.0
            } else {
                cell_metrics.cell_width
            }))
            .h(px(cell_metrics.cell_height))
            .flex_none()
            .overflow_hidden()
            .bg(background)
            .text_color(foreground)
            .when(cell.bold, |cell| cell.font_weight(FontWeight::BOLD))
            .when(cell.italic, |cell| cell.italic())
            .when(cell.underline || hyperlink.is_some(), |cell| {
                cell.underline()
            })
            .when(cell.strikeout, |cell| cell.line_through())
            .when(cell.hidden, |cell| cell.invisible())
            .when(
                cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Beam),
                |cell| cell.border_l_1().border_color(default_foreground),
            )
            .when(
                cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Underline),
                |cell| cell.border_b_1().border_color(default_foreground),
            )
            .when(
                cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::HollowBlock),
                |cell| cell.border_1().border_color(default_foreground),
            )
            .child(if cell.text.is_empty() {
                " ".into()
            } else {
                cell.text.clone()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    this.on_cell_mouse_down(point, event, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, event, _, cx| this.on_cell_mouse_move(point, event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| {
                    this.on_cell_mouse_up(point, hyperlink.clone(), event, cx)
                }),
            )
            .into_any_element()
    }
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
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self
            .active_tab
            .and_then(|index| self.tabs.get(index))?
            .frame
            .cursor()?;
        let start = range_utf16.start as f32;
        let width = range_utf16.end.saturating_sub(range_utf16.start).max(1) as f32;
        let cell_metrics = terminal_cell_metrics(f32::from(cx.theme().mono_font_size), self.mode);
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
            .bg(cx.theme().background)
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
        let manager = self.manager.clone();
        let _ = std::thread::spawn(move || {
            for terminal_id in terminal_ids {
                let _ = manager.kill(&terminal_id);
            }
        });
    }
}

fn run_poll_work(work: TerminalPollWork) -> TerminalPollResult {
    let snapshot = match work
        .manager
        .raw_snapshot_from(&work.terminal_id, work.next_sequence)
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return TerminalPollResult {
                terminal_id: work.terminal_id,
                session: None,
                result: Err(format!("Terminal snapshot failed: {}", error.message)),
            };
        }
    };
    let session = snapshot.session.clone();
    let result = work
        .backend
        .lock()
        .map_err(|_| "Terminal parser state lock failed".to_string())
        .and_then(|mut backend| {
            backend
                .sync(&snapshot)
                .map_err(|error| format!("Terminal sync failed: {}", error.message))
                .map(|outcome| {
                    (outcome.ingested_chunks > 0 || outcome.rebuilt)
                        .then(|| (outcome, backend.frame()))
                })
        });
    TerminalPollResult {
        terminal_id: work.terminal_id,
        session: Some(session),
        result,
    }
}

fn empty_cell(point: TerminalGridPoint) -> TerminalCellSnapshot {
    TerminalCellSnapshot {
        row: point.row,
        column: point.column,
        text: " ".into(),
        foreground: TerminalCellColor::Named { index: 256 },
        background: TerminalCellColor::Named { index: 257 },
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

fn terminal_cell_metrics(code_font_size: f32, mode: TerminalSurfaceMode) -> TerminalCellMetrics {
    let code_font_size = if code_font_size.is_finite() {
        code_font_size.clamp(TERMINAL_MIN_FONT_SIZE, TERMINAL_MAX_FONT_SIZE)
    } else {
        TERMINAL_BASE_FONT_SIZE
    };
    let scale = code_font_size / TERMINAL_BASE_FONT_SIZE;
    TerminalCellMetrics {
        cell_width: TERMINAL_BASE_CELL_WIDTH * scale,
        cell_height: TERMINAL_BASE_CELL_HEIGHT * scale,
        horizontal_padding: mode.horizontal_padding(),
        vertical_padding: mode.vertical_padding(),
    }
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
    Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
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

    #[test]
    fn terminal_cell_metrics_follow_clamped_code_font_size() {
        let default = terminal_cell_metrics(13.0, TerminalSurfaceMode::Standalone);
        assert_eq!(default.cell_width, 8.0);
        assert_eq!(default.cell_height, 16.0);

        let minimum = terminal_cell_metrics(1.0, TerminalSurfaceMode::Preview);
        assert_eq!(minimum.cell_width, 80.0 / 13.0);
        assert_eq!(minimum.cell_height, 160.0 / 13.0);
        assert_eq!(minimum.horizontal_padding, PREVIEW_TERMINAL_PADDING);

        let maximum = terminal_cell_metrics(100.0, TerminalSurfaceMode::Composer);
        assert_eq!(maximum.cell_width, 192.0 / 13.0);
        assert_eq!(maximum.cell_height, 384.0 / 13.0);
        assert_eq!(maximum.vertical_padding, COMPOSER_TERMINAL_PADDING);

        assert_eq!(
            terminal_cell_metrics(f32::NAN, TerminalSurfaceMode::Standalone),
            default
        );
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
        surface.update(&mut cx, |surface, _| {
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
        assert_eq!(
            marked_bounds.size.height,
            px(terminal_cell_metrics(13.0, TerminalSurfaceMode::Standalone).cell_height)
        );

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
}
