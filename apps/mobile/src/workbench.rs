use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Bounds, ClipboardItem, Context, DispatchPhase, Entity, FontWeight, Image, ImageFormat,
    IntoElement, LongPressEvent, MouseButton, MouseUpEvent, ObjectFit, ParentElement as _,
    PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollEvent, ScrollHandle, Styled as _, Task,
    TouchDragEvent, TouchPhase, WeakEntity, Window, canvas, div, img, point, prelude::*, px, rgb,
    svg,
};
use vibex_backend::{
    BackendError, BackendOperation, BackendResult, GitBackend as _, MutationRequest,
    TerminalBackend as _,
};
use vibex_core::{
    FileEntryKind, FilePreviewKind, FileSearchRequest, GitBranchSummary, GitChange, GitChangeKind,
    GitCommitDetail, GitCommitDetailRequest, GitCommitFileChange, GitCommitSummary,
    GitDiffResponse, GitHistoryRequest, GitManagedWorktreeRecord, GitRemoteActionKind,
    GitRemoteActionRequest, GitStageRequest, GitWorktreeConflictStageRequest,
    GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeMergePlan, GitWorktreeMergeRequest, GitWorktreeOperationRecord,
    GitWorktreeOperationRequest, GitWorktreeOperationStatus, GitWorktreeReadinessRequest,
    GitWorktreeReadinessState, RemoteActionClass, TerminalCreateRequest, TerminalId,
    TerminalSnapshot, TerminalStatus, VibexSessionId, WorkspaceId,
};
use vibex_desktop_model::{
    FileGitSignal, FileIconDescriptor, FileIconKind, GitPathSelectionState, GitQueryKind,
    GitTreeRow, GitTreeRowKind, GitWorkbenchMode, file_icon_descriptor,
};
use vibex_remote_client::WebRemoteBackend;
use vibex_terminal_ui::{
    TerminalCellColor, TerminalCellSnapshot, TerminalCursorShape, TerminalGridPoint,
    TerminalSearchMatch,
};
use vibex_ui::terminal::{TERMINAL_DEFAULT_COLS, TERMINAL_DEFAULT_ROWS};
use vibex_ui::{
    AsyncPhase, FileWorkflowController, GitWorkflowController, ShellKind, TerminalInput,
    TerminalKey, TerminalKeyModifiers, TerminalWorkflowCapabilities, TerminalWorkflowController,
};

use crate::bottom_sheet::{
    BottomSheet, SheetKey, SheetPresentation, SheetPull, bottom_sheet_shell,
};
use crate::locale;
use crate::markdown;
use crate::platform::WindowInsetsExt as _;
use crate::theme;
use crate::toast::{Hint, Tone};
use gpui_component::ActiveTheme as _;
use gpui_component::Sizable as _;
use gpui_component::Size;
use gpui_component::input::{
    Editor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_component::spinner::Spinner;

const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(600);
/// Cell metrics copied from the desktop terminal surface so both clients
/// render the same grid geometry at the base 13px mono font size.
/// How much of a long diff is shown at once, and how much more each "show
/// more" adds.
const GIT_DIFF_PAGE_LINES: usize = 800;
/// How many hits a file search may return. The desktop asks for the same
/// number, and the shared workflow clamps anything above its own ceiling, so a
/// query answered here lists exactly what the desktop would list.
const FILE_SEARCH_RESULT_LIMIT: u32 = 200;
/// The pause after the last keystroke before the file search runs.
///
/// A phone keyboard commits a word one character at a time, and a request per
/// character would flood the wire with queries nobody asked for. The pause is
/// what makes the field feel live without reading as a search per letter; the
/// desktop debounces its own field for the same reason.
const FILE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
/// The same pause for the commit query. It is a little longer because the
/// authority scans history for it rather than walking the file tree.
const GIT_HISTORY_SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// How many lines of a commit's patch the detail screen paints. A commit that
/// changes more than this is read in the desktop's own preview pane, which
/// pages the same way; the phone draws the head of it rather than a scroll
/// that would take a minute to move through.
const GIT_COMMIT_PATCH_PAGE_LINES: usize = 2000;
/// The terminal grid keeps absolute pixels on purpose: the PTY is sized to a
/// row/column count, so a cell that grew with the text-size preference would
/// desynchronize the rendered grid from the shell's belief about its size.
/// These mirror the desktop terminal surface's metrics.
const TERMINAL_FONT_SIZE: f32 = 13.0;
const TERMINAL_CELL_WIDTH: f32 = 8.0;
const TERMINAL_CELL_HEIGHT: f32 = 18.0;
const TERMINAL_HORIZONTAL_PADDING: f32 = 6.0;
const TERMINAL_VERTICAL_PADDING: f32 = 4.0;
/// Auto-fit bounds: the PTY is resized to fill the surface, clamped so extreme
/// layouts can never request a degenerate grid.
const TERMINAL_MIN_ROWS: u16 = 4;
const TERMINAL_MAX_ROWS: u16 = 100;
const TERMINAL_MIN_COLS: u16 = 20;
const TERMINAL_MAX_COLS: u16 = 240;
/// Height of the floating copy bar a touch selection raises, and the band at
/// the top of the grid it occupies. A long press inside the band belongs to
/// the bar, not to the row underneath it.
const TERMINAL_SELECTION_BAR_HEIGHT: f32 = 40.0;
const TERMINAL_SELECTION_BAR_BAND: f32 = TERMINAL_SELECTION_BAR_HEIGHT + theme::SPACING_SM * 2.0;
/// The selection's drag handles: the dot a reader sees, and the radius a touch
/// has to land inside to take hold of it. The touch radius is deliberately
/// larger than the dot, so the handle is a comfortable target even though it
/// has to sit beside the text it is selecting.
const TERMINAL_HANDLE_SIZE: f32 = 14.0;
const TERMINAL_HANDLE_TOUCH_RADIUS: f32 = 26.0;

/// The rail's own header: one row of surface tabs and the way out. The drawer
/// that hosts the rail reserves no header of its own, so this row is the whole
/// chrome between the session and the surface. The drawer's own old header was
/// this tall, so the rail now carries a tab strip, a count, and the close
/// control for the price the title row used to cost on its own.
///
/// The band is a chrome strip, not a content row: it sits a step under the
/// height that a touch target wants, because the reader passes through it on
/// the way to the surface rather than working in it. The rows below pay for
/// that step with the space they get back.
const WORKBENCH_HEADER_HEIGHT: f32 = 44.0;
/// A surface tab inside that header. The tab is the plate the reader sees; the
/// header's own height is what makes it a target, so the plate stays close to
/// its label instead of carrying a second touch minimum inside the row.
const WORKBENCH_TAB_HEIGHT: f32 = 32.0;
/// The contextual row under the header: a branch, a search field, a refresh.
const WORKBENCH_CONTEXT_HEIGHT: f32 = 44.0;
/// The file screen's own header. It is a step taller than the rail's chrome
/// strip because it stacks the file's name over the directory that holds it,
/// and it is the one header on the rail that carries a label rather than a tab.
const FILE_HEADER_HEIGHT: f32 = 48.0;
/// The row that carries a segmented switch and the actions that belong to the
/// segment it is on.
const WORKBENCH_FILTER_HEIGHT: f32 = 44.0;
/// A control drawn inside one of those rows: the row's own height less its
/// padding, so the four pixels above and below it stay ink rather than hitbox.
const WORKBENCH_CONTROL_SIZE: f32 = 36.0;
/// A secondary control (a chip that names a filter) inside one of those rows.
const WORKBENCH_CHIP_HEIGHT: f32 = 32.0;
/// How far a tree row's fill is inset from the rail's edges. The fill is a
/// rounded card, so it must not touch the border it sits under.
const TREE_ROW_INSET: f32 = 4.0;
/// A row in one of the rail's trees.
///
/// A tree is read by scanning, so its rows are sized by their pitch rather
/// than by the touch minimum: a full 44 on every row spent a phone's screen on
/// the gaps between the names. It stays a comfortable target at this height —
/// the row is a name, not a form field — and it is the density the desktop
/// tree and the readers' own file browsers use.
const TREE_ROW_HEIGHT: f32 = 34.0;
/// The gap between two controls in a toolbar row. Two of these are already a
/// margin; a scale step each is what pushed the search field into a sliver.
const TOOLBAR_GAP: f32 = 2.0;
/// The corner of a surface tab's plate. Squarer than a control: the plate sits
/// inside a strip that has corners of its own, and a soft one there reads as a
/// card resting on the row rather than a tab cut into it.
const TAB_RADIUS: f32 = 5.0;

/// The two ends of a touch selection over the terminal grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TerminalSelection {
    anchor: TerminalGridPoint,
    head: TerminalGridPoint,
}

impl TerminalSelection {
    /// The selection with its ends in reading order, which is what the two
    /// drag handles are placed and hit-tested against.
    fn ends(self) -> (TerminalGridPoint, TerminalGridPoint) {
        let key = |point: TerminalGridPoint| (point.row, point.column);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// Which end of the selection a drag is moving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TerminalSelectionHandle {
    Start,
    End,
}

/// A drag this client is forwarding to an application that asked for mouse
/// reporting, driven by a sideways pan.
#[derive(Clone, Copy, Debug)]
struct TerminalMouseDrag {
    /// Where the finger went down. A pan reports this same point with every
    /// step, so it is both the press's cell and the origin the travelled
    /// distance is measured from.
    origin: Point<Pixels>,
    /// Horizontal distance reported since the press, in pixels.
    travelled: f32,
    /// The cell of the last report, so a step that stays inside one cell is
    /// not sent again.
    last: TerminalGridPoint,
}

/// How the rail reports that its own close control was pressed. The drawer it
/// lives in belongs to the app, so the panel holds the app's handler rather
/// than the animation state.
type CloseHandler = Rc<dyn Fn(&MouseUpEvent, &mut Window, &mut gpui::App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchSurface {
    Files,
    Git,
    Terminal,
}

/// Which side of a unified diff a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffLineKind {
    Context,
    Added,
    Removed,
    Hunk,
}

/// One painted row of a unified diff: what it is, which line it is, and the
/// text without its leading marker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDiffRow {
    kind: DiffLineKind,
    line: Option<u32>,
    content: String,
}

impl GitDiffRow {
    /// The marker a row carries in front of its text, which is what makes a
    /// copied line readable and what tells an empty added line from an empty
    /// context one.
    fn prefix(&self) -> &'static str {
        match self.kind {
            DiffLineKind::Added => "+",
            DiffLineKind::Removed => "-",
            DiffLineKind::Hunk => "",
            DiffLineKind::Context => " ",
        }
    }
}

/// One file's part of a patch: the path it changes and the rows that change it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitPatchFile {
    path: String,
    rows: Vec<GitDiffRow>,
}

/// Reads a patch into one group of rows per file.
///
/// The `diff --git`, `index`, `---` and `+++` lines are read but never painted:
/// they describe the patch's own envelope, and the desktop's patch view starts
/// at the first hunk. What they are good for is telling one file's part of the
/// patch from the next, which is what lets the commit screen draw a file's
/// changes under its own bar.
///
/// One walk serves the working-tree diff sheet and a commit's patch, so the two
/// surfaces cannot disagree about what an addition looks like.
fn parse_patch_files(patch: &str) -> Vec<GitPatchFile> {
    let mut files: Vec<GitPatchFile> = Vec::new();
    let mut path: Option<String> = None;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut in_hunk = false;

    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            // A file's part begins. `rest` is `a/<old> b/<new>`, and the `+++`
            // line below is the authoritative name when the two differ — a
            // rename — so this is only the fallback. The new group starts on
            // the name *this* line carries: the group being closed already has
            // its own, and borrowing it for the next file is how every file's
            // bar after the first ends up labelled with the one before it.
            let target = diff_git_target(rest);
            files.push(GitPatchFile {
                path: target.clone().unwrap_or_default(),
                rows: Vec::new(),
            });
            path = target;
            in_hunk = false;
            continue;
        }
        if raw.starts_with("@@") {
            new_line = hunk_new_start(raw).unwrap_or(new_line);
            old_line = hunk_old_start(raw).unwrap_or(old_line);
            in_hunk = true;
            push_patch_row(
                &mut files,
                &path,
                GitDiffRow {
                    kind: DiffLineKind::Hunk,
                    line: None,
                    content: raw.to_string(),
                },
            );
            continue;
        }
        if !in_hunk {
            if let Some(target) = raw.strip_prefix("+++ ") {
                // The destination side, which is where the change lands. It
                // overrides the `diff --git` line rather than only filling it
                // in, because a rename makes the two disagree.
                let target = clean_patch_path(target);
                path = Some(target.clone());
                if let Some(file) = files.last_mut() {
                    file.path = target;
                }
            }
            // Everything else before the first hunk is envelope.
            continue;
        }
        let row = if let Some(rest) = raw.strip_prefix('+') {
            let line = new_line;
            new_line += 1;
            GitDiffRow {
                kind: DiffLineKind::Added,
                line: Some(line),
                content: rest.to_string(),
            }
        } else if let Some(rest) = raw.strip_prefix('-') {
            let line = old_line;
            old_line += 1;
            GitDiffRow {
                kind: DiffLineKind::Removed,
                line: Some(line),
                content: rest.to_string(),
            }
        } else if let Some(rest) = raw.strip_prefix(' ') {
            let line = new_line;
            old_line += 1;
            new_line += 1;
            GitDiffRow {
                kind: DiffLineKind::Context,
                line: Some(line),
                content: rest.to_string(),
            }
        } else if raw.is_empty() {
            // A blank line inside a hunk is an empty context line.
            let line = new_line;
            old_line += 1;
            new_line += 1;
            GitDiffRow {
                kind: DiffLineKind::Context,
                line: Some(line),
                content: String::new(),
            }
        } else {
            // `\ No newline at end of file` and anything else the envelope grew
            // has no line of its own to stand on.
            continue;
        };
        push_patch_row(&mut files, &path, row);
    }

    // A diff with no `diff --git` header — which is what a single file's diff
    // looks like — still opens one group for the rows it has.
    if files.iter().all(|file| file.rows.is_empty()) {
        let rows = files.into_iter().flat_map(|file| file.rows).collect();
        return vec![GitPatchFile {
            path: path.unwrap_or_default(),
            rows,
        }];
    }
    files
}

/// Appends a row to the group it belongs to, opening one for a diff that began
/// without a `diff --git` header.
fn push_patch_row(files: &mut Vec<GitPatchFile>, path: &Option<String>, row: GitDiffRow) {
    if files.is_empty() {
        files.push(GitPatchFile {
            path: path.clone().unwrap_or_default(),
            rows: Vec::new(),
        });
    }
    if let Some(file) = files.last_mut() {
        file.rows.push(row);
        if file.path.is_empty()
            && let Some(path) = path.clone()
        {
            file.path = path;
        }
    }
}

/// The destination path in a `diff --git a/<old> b/<new>` line.
fn diff_git_target(rest: &str) -> Option<String> {
    rest.rsplit_once(" b/")
        .map(|(_, target)| target.to_string())
        .or_else(|| rest.split_whitespace().last().map(clean_patch_path))
}

/// Strips the `a/` or `b/` prefix and the `/dev/null` marker from a patch path.
fn clean_patch_path(value: &str) -> String {
    let value = value.trim();
    if value == "/dev/null" {
        return String::new();
    }
    value
        .strip_prefix("b/")
        .or_else(|| value.strip_prefix("a/"))
        .unwrap_or(value)
        .to_string()
}

/// Every row of a patch, in order, capped at `limit`; the second half of the
/// pair is how many lines were left over.
fn parse_unified_diff(patch: &str, limit: usize) -> (Vec<GitDiffRow>, usize) {
    let rows = parse_patch_files(patch)
        .into_iter()
        .flat_map(|file| file.rows)
        .take(limit)
        .collect::<Vec<_>>();
    let remaining = patch.lines().count().saturating_sub(limit);
    (rows, remaining)
}

/// The `-` side's starting line in a `@@ -a,b +c,d @@` header.
fn hunk_old_start(header: &str) -> Option<u32> {
    header
        .split_whitespace()
        .find(|part| part.starts_with('-'))
        .and_then(|part| part.trim_start_matches('-').split(',').next())
        .and_then(|value| value.parse().ok())
}

/// The `+` side's starting line in a `@@ -a,b +c,d @@` header.
fn hunk_new_start(header: &str) -> Option<u32> {
    header
        .split_whitespace()
        .find(|part| part.starts_with('+'))
        .and_then(|part| part.trim_start_matches('+').split(',').next())
        .and_then(|value| value.parse().ok())
}

/// The wash, the ink, and the marker of one diff row, in the desktop's own
/// tones: an addition is a green-tinted band, a deletion a red one, a hunk
/// header a blue one, and a context line the pane's own background.
fn diff_row_palette(kind: DiffLineKind) -> (gpui::Hsla, gpui::Hsla) {
    match kind {
        DiffLineKind::Added => (
            theme::accent_success().opacity(0.10),
            theme::accent_success(),
        ),
        DiffLineKind::Removed => (theme::accent_red().opacity(0.10), theme::accent_red()),
        DiffLineKind::Hunk => (theme::accent_blue().opacity(0.10), theme::accent_blue()),
        DiffLineKind::Context => (theme::workbench_bg(), theme::text_primary().opacity(0.85)),
    }
}

/// The rail that marks a changed row, drawn from the pane's left edge so an
/// addition or a deletion reads before the text does.
fn diff_change_rail(kind: DiffLineKind) -> gpui::Div {
    let color = match kind {
        DiffLineKind::Added => theme::accent_success(),
        DiffLineKind::Removed => theme::accent_red(),
        _ => theme::text_primary().opacity(0.0),
    };
    div().w(px(3.0)).flex_none().bg(color)
}

/// One diff row, at the desktop's own spacing: the change rail, the line
/// number, and the text behind its marker.
fn render_diff_row(row: GitDiffRow) -> gpui::AnyElement {
    let (background, foreground) = diff_row_palette(row.kind);
    let text = if row.content.is_empty() {
        row.prefix().to_string()
    } else {
        format!("{}{}", row.prefix(), row.content)
    };
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_stretch()
        .bg(background)
        .border_b_1()
        .border_color(theme::border_subtle())
        .font_family(theme::MONO_FONT_FAMILY)
        .text_size(theme::font_micro())
        .text_color(foreground)
        .child(diff_change_rail(row.kind))
        .child(
            div()
                .w(px(36.0))
                .flex_none()
                .px_1()
                .py(px(2.0))
                .text_right()
                .text_color(theme::text_muted().opacity(0.70))
                .child(row.line.map(|line| line.to_string()).unwrap_or_default()),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .px_2()
                .py(px(2.0))
                .whitespace_normal()
                .child(text),
        )
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobileFileSearchMode {
    Name,
    Content,
}

impl MobileFileSearchMode {
    /// The word the control reads, and the mode a tap switches away from.
    fn label(self) -> &'static str {
        match self {
            Self::Name => locale::text("Name", "名称", "名稱"),
            Self::Content => locale::text("Content", "内容", "內容"),
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Name => Self::Content,
            Self::Content => Self::Name,
        }
    }

    /// What a tap will do, which is what the control announces to a reader who
    /// cannot see the word it is showing.
    fn toggle_label(self) -> &'static str {
        match self {
            Self::Name => locale::text(
                "Switch to content search",
                "切换为内容搜索",
                "切換為內容搜尋",
            ),
            Self::Content => {
                locale::text("Switch to name search", "切换为名称搜索", "切換為名稱搜尋")
            }
        }
    }
}

impl WorkbenchSurface {
    pub const ALL: [Self; 3] = [Self::Files, Self::Git, Self::Terminal];

    /// The surface's name as an identifier: the element id a test finds its tab
    /// by, and the word the strip is drawn from.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            // The surface is the working tree, not the tool that reads it: the
            // tab names what the reader finds there, the same word the desktop
            // puts over the same list.
            Self::Git => "Changes",
            Self::Terminal => "Terminal",
        }
    }

    pub fn localized_label(self) -> &'static str {
        match self {
            Self::Git => locale::text("Changes", "变更", "變更"),
            _ => locale::common(self.label()),
        }
    }
}

/// The independent readers whose failures the rail announces.
///
/// A success from one must not clear the record of another's failure: the
/// terminal poll and the file tree settle at different rates, and a shared slot
/// let a healthy Git read make the next identical terminal failure speak again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FailureSource {
    Files,
    Git,
    Terminal,
}

pub struct MobileWorkbench {
    backend: Arc<WebRemoteBackend>,
    workspace_id: WorkspaceId,
    surface: WorkbenchSurface,
    files: FileWorkflowController,
    git: GitWorkflowController,
    terminal: TerminalWorkflowController,
    file_search_input: Option<Entity<InputState>>,
    /// Installed on first paint so the keyboard's action key submits a search.
    file_search_subscription: Option<gpui::Subscription>,
    /// The pause after the last keystroke before a typed query runs. Held so
    /// the next keystroke can drop the timer it supersedes.
    file_search_debounce_task: Option<Task<()>>,
    /// Which file search request an answer belongs to. A reply to an earlier
    /// query must not paint over the answer to the query in the field.
    file_search_request_generation: u64,
    file_search_mode: MobileFileSearchMode,
    file_editor_input: Option<Entity<EditorState>>,
    /// A chosen file takes over the whole Files surface as its own screen
    /// with a back button, instead of rendering inline under the tree.
    file_screen_open: bool,
    /// The commit message, as the multi-line field the desktop commits from.
    ///
    /// A commit message is a subject and a body. A one-line field can only ever
    /// hold the first, and it swallowed the second without saying so.
    git_commit_input: Option<Entity<TextareaState>>,
    git_history_query_input: Option<Entity<InputState>>,
    /// Installed with the field so the keyboard's action key runs the query.
    git_history_query_subscription: Option<gpui::Subscription>,
    /// The pause after the last keystroke before the commit query runs.
    git_history_query_debounce_task: Option<Task<()>>,
    /// The focused sink the software keyboard writes into.
    ///
    /// A hardware keyboard can hand a focused element raw key events, but an
    /// Android IME only ever commits text into a real text field. Rendering an
    /// invisible field behind the grid is what makes the terminal typeable with
    /// the phone's own keyboard: every committed character is forwarded to the
    /// PTY and the field is emptied again.
    ///
    /// It is a *multi-line* field, which is what makes Enter work. The host's
    /// IME target is a multi-line editor, so the keyboard's Enter key commits a
    /// newline rather than an editor action; a single-line field strips
    /// newlines as it normalises, so the key never reached the grid at all.
    terminal_input: Option<Entity<TextareaState>>,
    /// Subscription to the sink's change events, installed on first paint.
    terminal_sink_subscription: Option<gpui::Subscription>,
    /// What the sink held after the last forwarded edit, so the next change can
    /// be reduced to the characters the user actually typed or deleted.
    terminal_sink_previous: String,
    /// Whether Ctrl is latched from the accessory bar (⌃ on a phone keyboard
    /// has no physical key to hold).
    terminal_alt_latched: bool,
    /// Set while the grid is scrolled away from the live bottom, so the
    /// "jump to latest" affordance only appears when it can do something.
    terminal_scrolled_back: bool,
    // Programmatic edits need a window and the async paths below have none,
    // so a clear is recorded here and applied on the next paint.
    file_search_clear_pending: bool,
    git_commit_clear_pending: bool,
    git_history_query_clear_pending: bool,
    terminal_clear_pending: bool,
    /// File content waiting to be written into the editor. Loading a file
    /// happens in an async task with no window.
    pending_file_editor_content: Option<String>,
    /// The path of the buffer the editor is holding, which is not always the
    /// selected path: a read that returned nothing must not claim the screen.
    file_editor_path: Option<String>,
    /// A line to put the caret on once the file it belongs to has loaded. A
    /// content-search result names a line, and opening the file without going
    /// there leaves the reader to find it again by hand.
    pending_file_reveal: Option<(String, usize)>,
    /// The image a `FilePreviewKind::Image` file turned out to be, once its
    /// bytes have come back from the workspace.
    file_image: Option<Arc<Image>>,
    /// The path whose bytes are being fetched, so a slow read cannot paint
    /// over the file the reader moved on to.
    file_image_loading: Option<String>,
    /// The rendered Markdown view, kept across frames so a preview does not
    /// rebuild its text layout on every paint. Keyed by path and revision.
    file_markdown_view: Option<(String, Entity<markdown::MarkdownView>)>,
    /// How the content search is run. The desktop offers the same three
    /// switches; a phone keeps them off until asked for.
    /// Where this workbench is pointed, as one line: the project, the worktree
    /// and its branch. The rail follows the open session, so this is a
    /// read-out rather than a switcher — the desktop is where a worktree is
    /// chosen.
    context_label: Option<String>,
    file_search_case_sensitive: bool,
    file_search_whole_word: bool,
    file_search_regex: bool,
    /// The editor's own find bar. The kit input already highlights matches and
    /// reveals the active one with padding, so the phone only has to own the
    /// field and the two step buttons.
    file_find_open: bool,
    file_find_input: Option<Entity<InputState>>,
    file_find_subscription: Option<gpui::Subscription>,
    /// The query the editor was last told about, so a keystroke that did not
    /// change it costs nothing.
    file_find_query: String,
    /// The go-to-line prompt, which is a number and a confirm.
    file_goto_open: bool,
    file_goto_input: Option<Entity<InputState>>,
    git_diff: Option<GitDiffResponse>,
    /// The scroll handle for the diff sheet, so a freshly opened diff starts at
    /// its first line instead of wherever the tree behind it was scrolled to.
    git_diff_scroll: ScrollHandle,
    /// The worktree lifecycle: what the authority reports about this
    /// workspace's worktree, the merge plan being confirmed, and the
    /// destructive action waiting for its confirmation.
    worktree_lifecycle: Option<GitWorktreeLifecycleSnapshot>,
    worktree_lifecycle_loading: bool,
    worktree_merge_plan: Option<GitWorktreeMergePlan>,
    worktree_confirmation: Option<WorktreeConfirmation>,
    /// How many lines of the open diff are shown. A long patch is read a page
    /// at a time rather than cut off, which is what the hard limit used to do.
    git_diff_limit: usize,
    /// Paths awaiting confirmation before a rollback. The desktop confirms this
    /// too; the phone used to discard local edits on a single tap.
    git_revert_confirmation: Option<Vec<String>>,
    /// One commit opened from the Commits list takes over the Git surface as
    /// its own full-screen view with a back button.
    git_commit_detail: Option<GitCommitDetail>,
    git_commit_detail_loading: bool,
    /// Whether the open commit's message body is unfolded. One flag serves the
    /// one commit the screen can show, and it resets with every new one.
    git_commit_body_expanded: bool,
    /// The files of the open commit whose patch rows are folded away. A file
    /// list is read by folding the parts the reader is not on, and the set
    /// resets with every commit for the same reason the body does.
    git_commit_collapsed_files: BTreeSet<String>,
    git_commit_confirmation: bool,
    /// Rewrite the previous commit instead of adding one.
    git_commit_amend: bool,
    /// Push the branch once the commit lands.
    git_commit_push_after: bool,
    git_history_loading: bool,
    git_history_request_generation: u64,
    /// Every branch the repository has, read on demand for the filter sheet.
    /// The desktop gets the same list from `git_branch_list`, and without it
    /// the phone was stuck reading history from whichever branch it was on.
    git_branches: Option<Vec<GitBranchSummary>>,
    git_branches_loading: bool,
    git_branch_sheet_open: bool,
    /// The sheets that rise from the foot of the screen: their rise, their
    /// fall, and the finger pulling one back down.
    sheet: BottomSheet,
    /// The task that clears a sheet once its fall has landed. Held so the
    /// timer is not dropped before it fires.
    sheet_task: Option<Task<()>>,
    /// The author filter's sheet. The list comes from the history the model
    /// already holds, so opening it costs nothing.
    git_author_sheet_open: bool,
    /// The date filter's sheet.
    git_date_sheet_open: bool,
    /// Next chunk sequence to feed into the render model; 0 forces a rebuild
    /// from the retained snapshot window.
    terminal_render_sequence: i64,
    terminal_close_confirmation: Option<TerminalId>,
    /// The tab the reader was last on.
    ///
    /// Coming back to the terminal should land on the one they were using
    /// rather than on whichever session the list happens to start with, so the
    /// attachment is remembered here and the refresh puts it back.
    terminal_last_selected: Option<TerminalId>,
    /// Set when the reader enters the Terminal surface, and consumed by the
    /// first session list that comes back empty: a phone that opens the
    /// terminal on a workspace with none should have one, not an empty pane
    /// with a button in it.
    terminal_auto_create: bool,
    /// Rows/cols this client last requested, so repeated fits are no-ops.
    terminal_fit_size: Option<(u16, u16)>,
    /// The first auto-fit failure, kept so the paint-time fit reports it once
    /// instead of re-notifying on every frame.
    terminal_fit_error: Option<String>,
    terminal_poll_generation: u64,
    /// The geometry the terminal grid was last painted at, in window
    /// coordinates. A touch point is turned into a cell against it, which is
    /// what makes a long press select the place the finger is actually on.
    terminal_grid_bounds: Option<Bounds<Pixels>>,
    /// The row count the painted frame holds, with the pane height those rows
    /// were painted to fill. The frame only changes size when the PTY resize
    /// comes back, so this is what tells a pane that grew ahead of its rows
    /// from one that grew with them.
    terminal_grid_rows: Option<(u16, f32)>,
    /// The anchor and live head of a touch selection, in grid points. Its
    /// presence is what raises the copy bar.
    terminal_selection: Option<TerminalSelection>,
    /// The text the current selection holds, kept for Copy so the clipboard
    /// write does not have to reach back into the render model.
    terminal_selection_text: Option<String>,
    /// The end of the selection a touch is currently dragging, once it landed
    /// on one of the two handles.
    terminal_selection_handle: Option<TerminalSelectionHandle>,
    /// The application drag a sideways pan is driving, if one is in progress.
    terminal_mouse_drag: Option<TerminalMouseDrag>,
    /// The origin of the pan whose drag has already been released. A fling
    /// repeats its own gesture's origin, so this is what tells the momentum
    /// that follows a release apart from a new drag.
    terminal_mouse_drag_released: Option<Point<Pixels>>,
    /// In-terminal find. Matches are painted by reusing the emulator's own
    /// selection, exactly as the desktop surface highlights them.
    terminal_search_open: bool,
    terminal_search_input: Option<Entity<InputState>>,
    terminal_search_subscription: Option<gpui::Subscription>,
    terminal_search_matches: Vec<TerminalSearchMatch>,
    terminal_active_search_match: usize,
    /// Which remote action, if any, the authority is busy with.
    ///
    /// A push or a pull spends seconds on the wire, so the button that started
    /// it turns into the spinner and the other two go quiet until it lands.
    /// The row used to dim all three and show nothing moving, which read as a
    /// frozen panel rather than work in progress.
    git_remote_action: Option<GitRemoteActionKind>,
    busy: bool,
    /// Hints waiting for a paint that has the window the toast layer needs.
    ///
    /// A rail reports every completed action and every failure as a light hint
    /// on the window's notification layer, never as a strip or a card inside a
    /// panel: a phone has one narrow column, and a fixed line pushes the
    /// content under it around and outlives the state it described. Most
    /// producers here run inside a task that has only `cx`, so the hint waits
    /// here until [`Self::present_pending_hints`] takes it.
    pending_hints: Vec<Hint>,
    /// The last failure each reader announced as a hint.
    ///
    /// A failed read is a light hint like every other result, but the terminal
    /// poll repeats every fraction of a second and the Git poll every few
    /// seconds; a hint per tick would fill the phone's screen with the same
    /// card. Keeping one message per source is what makes the announcement once
    /// per distinct failure; a read that recovers clears its own entry.
    last_failures: BTreeMap<FailureSource, String>,
    /// The last Git status failure announced, kept apart from `last_failures`.
    ///
    /// The two polls are independent and would otherwise clear each other's
    /// slot, so a Git failure repeated under a healthy terminal would be
    /// announced again on every poll.
    git_status_error: Option<String>,
    /// The rail's own way out. The drawer belongs to the app, so the header
    /// reports the request instead of animating the panel itself; the app
    /// installs this when it creates the workbench.
    close_handler: Option<CloseHandler>,
    tasks: Vec<Task<()>>,
}

impl MobileWorkbench {
    pub fn new(
        backend: Arc<WebRemoteBackend>,
        workspace_id: WorkspaceId,
        _session_id: Option<VibexSessionId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let capabilities = backend.capability_snapshot();
        let mut files = FileWorkflowController::new(backend.clone(), capabilities.file.clone());
        files.select_workspace(workspace_id.clone());
        let mut git = GitWorkflowController::new(backend.clone(), capabilities.git.clone());
        git.select_workspace(workspace_id.clone());
        let terminal = TerminalWorkflowController::new(
            backend.clone(),
            TerminalWorkflowCapabilities::from_backend(&capabilities),
        );
        let mut workbench = Self {
            backend,
            workspace_id,
            surface: WorkbenchSurface::Files,
            files,
            git,
            terminal,
            file_search_input: None,
            file_search_subscription: None,
            file_search_debounce_task: None,
            file_search_request_generation: 0,
            file_search_mode: MobileFileSearchMode::Name,
            file_editor_input: None,
            file_screen_open: false,
            git_commit_input: None,
            git_history_query_input: None,
            git_history_query_subscription: None,
            git_history_query_debounce_task: None,
            terminal_input: None,
            terminal_sink_subscription: None,
            terminal_sink_previous: String::new(),
            terminal_alt_latched: false,
            terminal_scrolled_back: false,
            file_search_clear_pending: false,
            git_commit_clear_pending: false,
            git_history_query_clear_pending: false,
            terminal_clear_pending: false,
            pending_file_editor_content: None,
            file_editor_path: None,
            pending_file_reveal: None,
            file_image: None,
            file_image_loading: None,
            file_markdown_view: None,
            context_label: None,
            file_search_case_sensitive: false,
            file_search_whole_word: false,
            file_search_regex: false,
            file_find_open: false,
            file_find_input: None,
            file_find_subscription: None,
            file_find_query: String::new(),
            file_goto_open: false,
            file_goto_input: None,
            git_diff: None,
            git_diff_scroll: ScrollHandle::new(),
            worktree_lifecycle: None,
            worktree_lifecycle_loading: false,
            worktree_merge_plan: None,
            worktree_confirmation: None,
            git_diff_limit: GIT_DIFF_PAGE_LINES,
            git_revert_confirmation: None,
            git_status_error: None,
            git_commit_detail: None,
            git_commit_detail_loading: false,
            git_commit_body_expanded: false,
            git_commit_collapsed_files: BTreeSet::new(),
            git_commit_confirmation: false,
            git_commit_amend: false,
            git_commit_push_after: false,
            git_history_loading: false,
            git_history_request_generation: 0,
            git_branches: None,
            git_branches_loading: false,
            git_branch_sheet_open: false,
            git_author_sheet_open: false,
            git_date_sheet_open: false,
            sheet: BottomSheet::default(),
            sheet_task: None,
            terminal_render_sequence: 0,
            terminal_close_confirmation: None,
            terminal_last_selected: None,
            terminal_auto_create: false,
            terminal_fit_size: None,
            terminal_fit_error: None,
            terminal_poll_generation: 0,
            terminal_grid_bounds: None,
            terminal_grid_rows: None,
            terminal_selection: None,
            terminal_selection_text: None,
            terminal_selection_handle: None,
            terminal_mouse_drag: None,
            terminal_mouse_drag_released: None,
            terminal_search_open: false,
            terminal_search_input: None,
            terminal_search_subscription: None,
            terminal_search_matches: Vec::new(),
            terminal_active_search_match: 0,
            git_remote_action: None,
            busy: false,
            pending_hints: Vec::new(),
            last_failures: BTreeMap::new(),
            close_handler: None,
            tasks: Vec::new(),
        };
        workbench.refresh_all(cx);
        workbench
    }

    /// Installs the app's way out of the rail, drawn by this view's header.
    ///
    /// Called once when the app creates the workbench. Without it the header
    /// still renders its close control, so a surface built outside the app
    /// shell (a test, or a future embedding) never panics on a missing owner.
    pub fn set_close_handler(
        &mut self,
        handler: impl Fn(&MouseUpEvent, &mut Window, &mut gpui::App) + 'static,
    ) {
        self.close_handler = Some(Rc::new(handler));
    }

    pub fn set_surface(&mut self, surface: WorkbenchSurface, cx: &mut Context<Self>) {
        if surface != WorkbenchSurface::Files {
            // The file's own sheet belongs to the surface that opens it, so it
            // comes down with it instead of rising over the next one.
            self.close_file_goto(cx);
        }
        if self.surface == WorkbenchSurface::Terminal && surface != WorkbenchSurface::Terminal {
            self.stop_terminal_poll();
            // The grid goes with the surface; a selection it was showing has
            // nothing left to be copied from.
            self.dismiss_terminal_selection(cx);
        }
        self.sync_capabilities();
        self.surface = surface;
        match surface {
            WorkbenchSurface::Files => self.refresh_files(cx),
            WorkbenchSurface::Git => {
                self.refresh_git(cx);
                self.refresh_worktree_lifecycle(cx);
            }
            // Entering the terminal is a request to work in one. If no live
            // session comes back, the refresh below opens one; the flag makes
            // that a one-shot answer to this visit rather than a session created
            // behind the reader's back on every poll.
            WorkbenchSurface::Terminal => {
                self.terminal_auto_create = true;
                self.refresh_terminals(cx)
            }
        }
        cx.notify();
    }

    /// Sets the one-line description of what this rail is pointed at.
    pub fn set_context_label(&mut self, label: Option<String>) {
        if self.context_label != label {
            self.context_label = label;
        }
    }

    pub fn set_workspace(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        // Another workspace is another file system, so the viewer's buffer goes
        // with the old one rather than showing a file from a tree it has left.
        self.workspace_id = workspace_id.clone();
        self.files.select_workspace(workspace_id.clone());
        self.git.select_workspace(workspace_id);
        self.file_editor_path = None;
        self.file_screen_open = false;
        self.git_diff = None;
        self.git_revert_confirmation = None;
        self.git_status_error = None;
        self.last_failures.clear();
        self.git_commit_detail = None;
        self.git_commit_detail_loading = false;
        self.terminal_render_sequence = 0;
        self.terminal_fit_size = None;
        self.terminal_fit_error = None;
        self.terminal_close_confirmation = None;
        self.reset_terminal_text_state();
        self.git_history_loading = false;
        self.git_history_request_generation = self.git_history_request_generation.wrapping_add(1);
        self.git_history_query_clear_pending = true;
        // A query scheduled against the old workspace must not run against the
        // new one, and an answer still coming back for it has no workspace.
        self.file_search_debounce_task = None;
        self.file_search_request_generation = self.file_search_request_generation.wrapping_add(1);
        self.git_history_query_debounce_task = None;
        self.stop_terminal_poll();
        self.refresh_all(cx);
    }

    pub fn suspend(&mut self) {
        self.stop_terminal_poll();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.refresh_all(cx);
    }

    /// Which surface the rail is showing. The app header reads it so tapping
    /// the rail button returns to the tab the user left it on.
    pub fn surface(&self) -> WorkbenchSurface {
        self.surface
    }

    /// Changed files plus live terminals: enough for one badge on the rail's
    /// entry point without a second trip into the panel to find out.
    pub fn right_rail_badge(&self) -> usize {
        self.git
            .state
            .model
            .change_row_count()
            .saturating_add(self.terminal_is_running() as usize)
    }

    pub fn terminal_is_running(&self) -> bool {
        self.terminal
            .state
            .sessions
            .iter()
            .any(|session| session.status == TerminalStatus::Running)
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.sync_capabilities();
        self.refresh_files(cx);
        self.refresh_git(cx);
        self.refresh_terminals(cx);
    }

    fn sync_capabilities(&mut self) {
        let capabilities = self.backend.capability_snapshot();
        self.files.set_capabilities(capabilities.file.clone());
        self.git.set_capabilities(capabilities.git.clone());
        self.terminal
            .set_capabilities(TerminalWorkflowCapabilities::from_backend(&capabilities));
    }

    fn refresh_files(&mut self, cx: &mut Context<Self>) {
        self.load_file_tree_path(String::new(), cx);
    }

    fn load_file_tree_path(&mut self, path: String, cx: &mut Context<Self>) {
        let ticket = match self.files.begin_tree_load(&path) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.load_tree(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_tree_load(&ticket, outcome);
                this.hint_failure(FailureSource::Files, failure);
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn select_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.open_file_now(path, cx);
    }

    fn open_file_now(&mut self, path: String, cx: &mut Context<Self>) {
        if self
            .pending_file_reveal
            .as_ref()
            .is_some_and(|(pending, _)| pending != &path)
        {
            self.pending_file_reveal = None;
        }
        let ticket = match self.files.begin_open_file(path) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        // The bytes a preview needs are not the editor's business, and for an
        // image the read carries no text at all, so the fetch is started
        // alongside the read rather than after it.
        let preview_kind = file_preview_kind_for_path(&ticket.path);
        if preview_kind == FilePreviewKind::Image {
            self.start_file_image_load(ticket.path.clone(), cx);
        }
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.read_file(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_file_read(&ticket, outcome);
                this.hint_failure(FailureSource::Files, failure);
                if let Some(content) = this.files.state.view().editor_content {
                    this.file_editor_path = Some(ticket.path.clone());
                    this.pending_file_editor_content = Some(content.clone());
                    this.file_screen_open = true;
                    if preview_kind == FilePreviewKind::Markdown {
                        let revision = this
                            .files
                            .state
                            .view()
                            .active_file
                            .as_ref()
                            .map(|file| file.content_revision.clone())
                            .unwrap_or_default();
                        let key = format!("{}:{revision}", ticket.path);
                        this.file_markdown_view = Some((
                            key,
                            cx.new(|cx| {
                                markdown::render(Arc::from(content.as_str()), 0, false, cx)
                            }),
                        ));
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Fetches an image file's bytes so the viewer can show the picture itself.
    ///
    /// The desktop reads images off its own disk; a phone has to ask the
    /// authority for them, which is the same route an attachment preview takes.
    fn start_file_image_load(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(format) = image_format_for_path(&path) else {
            return;
        };
        let backend = self.backend.clone();
        let workspace_id = self.workspace_id.clone();
        let requested = path.clone();
        self.file_image = None;
        self.file_image_loading = Some(path.clone());
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.download_file(workspace_id, path).await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                // A read that came back after the reader opened another file is
                // dropped rather than painted over the file they are on.
                if this.file_image_loading.as_deref() != Some(requested.as_str()) {
                    return;
                }
                this.file_image_loading = None;
                match outcome {
                    Ok(bytes) => {
                        // The viewer shows an image as itself the moment
                        // its bytes are here; nothing else has to be switched.
                        this.file_image = Some(Arc::new(Image::from_bytes(format, bytes)));
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Pushes the find query into the editor, which highlights the matches.
    fn apply_file_find_query(&mut self, cx: &mut Context<Self>) {
        let query = input_value(&self.file_find_input, cx).to_string();
        if query == self.file_find_query {
            return;
        }
        self.file_find_query = query.clone();
        let Some(editor) = self.file_editor_input.clone() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            if query.trim().is_empty() {
                editor.close_search(cx);
            } else {
                editor.set_search_query(query, true, cx);
            }
        });
        cx.notify();
    }

    fn open_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_find_open = true;
        if let Some(find) = self.file_find_input.clone() {
            find.update(cx, |find, cx| find.focus(window, cx));
        }
        crate::platform::show_keyboard();
        cx.notify();
    }

    fn close_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_find_open = false;
        self.file_find_query.clear();
        if let Some(find) = self.file_find_input.clone() {
            find.update(cx, |find, cx| find.set_value("", window, cx));
        }
        if let Some(editor) = self.file_editor_input.clone() {
            editor.update(cx, |editor, cx| editor.close_search(cx));
        }
        cx.notify();
    }

    /// Steps to the next or previous match.
    ///
    /// The editor reveals the match itself, with the padding a reader needs to
    /// see what surrounds it, so nothing here has to know where it landed.
    fn step_file_find_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.file_editor_input.clone() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            if forward {
                editor.next_search_match(cx);
            } else {
                editor.previous_search_match(cx);
            }
        });
        cx.notify();
    }

    fn open_file_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_goto_open = true;
        if let Some(input) = self.file_goto_input.clone() {
            input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
        }
        crate::platform::show_keyboard();
        cx.notify();
    }

    /// Brings down the go-to-line sheet.
    ///
    /// It is drawn by the Files surface, so a reader who leaves that surface
    /// must not find it waiting on the page they moved to.
    fn close_file_goto(&mut self, cx: &mut Context<Self>) {
        if self.file_goto_open {
            self.close_sheet(SheetKey::FileGoto, cx);
        }
    }

    /// Puts the caret on a line the reader named.
    ///
    /// The line is selected rather than merely pointed at: a caret between two
    /// identical lines is hard to find on a phone, and a selection is not.
    fn confirm_file_goto(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line_number) = input_value(&self.file_goto_input, cx)
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|line| *line > 0)
        else {
            self.hint(
                Tone::Warning,
                locale::text("Enter a line number", "请输入行号", "請輸入行號"),
            );
            return;
        };
        let Some(editor) = self.file_editor_input.clone() else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        let Some((start, end)) = file_line_range(&text, line_number) else {
            self.hint(
                Tone::Warning,
                locale::text(
                    "That line is past the end of the file",
                    "该行超出了文件末尾",
                    "該行超出了檔案結尾",
                ),
            );
            return;
        };
        // The editor does not reveal a programmatic selection, so the scroll is
        // set from the line's own height. A wrapped line makes it approximate,
        // which is why the line itself is what gets selected.
        let line_height = editor.read(cx).line_height().map(f32::from).unwrap_or(18.0);
        let offset_y = -((line_number.saturating_sub(2)) as f32) * line_height;
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(start..end, cx);
            editor.set_scroll_offset(point(px(0.0), px(offset_y)), cx);
            editor.focus(window, cx);
        });
        self.file_goto_open = false;
        crate::platform::hide_keyboard();
        cx.notify();
    }

    fn activate_file_row(
        &mut self,
        path: String,
        path_chain: Vec<String>,
        kind: FileEntryKind,
        cx: &mut Context<Self>,
    ) {
        if kind == FileEntryKind::Directory {
            let was_expanded = self.files.state.tree.chain_is_expanded(&path_chain);
            if self
                .files
                .state
                .tree
                .set_chain_expanded(&path_chain, !was_expanded)
                && !was_expanded
            {
                self.load_file_tree_path(path, cx);
            } else {
                cx.notify();
            }
        } else {
            self.select_file(path, cx);
        }
    }

    /// Collapses every directory the tree has open.
    ///
    /// Always complete, and always local: the paths are the ones the tree is
    /// already holding open.
    fn collapse_file_tree(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let expanded = self.files.state.tree.expanded_directory_paths();
        if expanded.is_empty() {
            return;
        }
        if self.files.state.tree.set_chain_expanded(&expanded, false) {
            cx.notify();
        }
    }

    /// Runs the typed file query once the reader pauses.
    ///
    /// A phone keyboard has no search button the reader can be asked to press,
    /// so waiting for Enter left the panel answering the empty query with "No
    /// matching items" while the reader was still typing. The field is the
    /// search: every keystroke supersedes the previous timer, and the panel
    /// enters its searching state at once so the answer on screen is never
    /// mistaken for an answer to the words now in the field.
    fn schedule_file_search(&mut self, cx: &mut Context<Self>) {
        // Whatever request is in flight was asked a different question.
        self.file_search_request_generation = self.file_search_request_generation.wrapping_add(1);
        let query = input_value(&self.file_search_input, cx).trim().to_string();
        if query.is_empty() {
            self.file_search_debounce_task = None;
            self.files.state.search.clear();
            cx.notify();
            return;
        }
        // Loading starts with the keystroke rather than with the request: the
        // debounce is the beginning of the search, and the rows already on
        // screen belong to older words.
        self.files.state.search.begin();
        cx.notify();
        self.file_search_debounce_task =
            Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                cx.background_executor().timer(FILE_SEARCH_DEBOUNCE).await;
                let _ = entity.update(cx, |this, cx| {
                    this.file_search_debounce_task = None;
                    this.start_file_search(cx);
                });
            }));
    }

    fn start_file_search(&mut self, cx: &mut Context<Self>) {
        // Enter, or the pause landing, runs the query now; the timer that would
        // have run it later goes with this call.
        self.file_search_debounce_task = None;
        self.file_search_request_generation = self.file_search_request_generation.wrapping_add(1);
        let request_generation = self.file_search_request_generation;
        let query = input_value(&self.file_search_input, cx).trim().to_string();
        if query.is_empty() {
            self.files.state.search.clear();
            cx.notify();
            return;
        }
        if let Err(error) = self.files.begin_search() {
            // The keystroke already put the panel into its searching state.
            // Without an answer for it, the panel would spin forever, so the
            // failure takes the spinner's place.
            self.files.state.search.reject(error.clone());
            self.fail(error);
            cx.notify();
            return;
        }
        let generation = self.files.state.generation;
        let mode = self.file_search_mode;
        // The three switches belong to the content search alone. Passing them
        // in name mode let a query like "foo" be narrowed to whole words or
        // matched case-sensitively because the reader had left those toggles on
        // in the other mode, which quietly answered a question nobody asked.
        // The desktop gates them the same way.
        let content = mode == MobileFileSearchMode::Content;
        let request = FileSearchRequest {
            workspace_id: self.workspace_id.clone(),
            query,
            include_content: content,
            case_sensitive: content && self.file_search_case_sensitive,
            whole_word: content && self.file_search_whole_word,
            regex: content && self.file_search_regex,
            // The shared workflow clamps this to its own ceiling, which is the
            // limit the desktop passes too.
            limit: Some(FILE_SEARCH_RESULT_LIMIT),
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.search_files(request));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                // The field has moved on since this query left, so its answer
                // is not the one the reader is waiting for.
                if request_generation != this.file_search_request_generation {
                    return;
                }
                // The backend answers one list for both modes: name matches
                // carry no line and content matches always do. Without this
                // split a name search listed the same file once per line that
                // contained the word, which is what made the phone's results
                // disagree with the desktop's for the same query.
                this.files.apply_search(
                    generation,
                    outcome.map(|mut results| {
                        results.retain(|result| match mode {
                            MobileFileSearchMode::Name => result.line.is_none(),
                            MobileFileSearchMode::Content => result.line.is_some(),
                        });
                        results
                    }),
                );
                this.hint_failure(FailureSource::Files, failure);
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Opens a search result on the line it matched.
    fn reveal_file_search_result(&mut self, path: String, line: u32, cx: &mut Context<Self>) {
        let line = line as usize;
        if line > 0 {
            self.pending_file_reveal = Some((path.clone(), line));
        }
        self.select_file(path, cx);
    }

    /// Puts the caret on a line that was waiting for its file to load.
    ///
    /// Called from the paint that writes the buffer, because the selection can
    /// only be set on content the editor already holds.
    fn apply_pending_file_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, line)) = self.pending_file_reveal.clone() else {
            return;
        };
        if self.file_editor_path.as_deref() != Some(path.as_str()) {
            // The file that was asked for never opened; the request goes with
            // it rather than landing on whatever is in the buffer instead.
            if !self.files.state.active_file.is_loading() {
                self.pending_file_reveal = None;
            }
            return;
        }
        let Some(input) = self.file_editor_input.clone() else {
            return;
        };
        self.pending_file_reveal = None;
        let text = input.read(cx).value().to_string();
        let Some((start, end)) = file_line_range(&text, line) else {
            return;
        };
        let line_height = input.read(cx).line_height().map(f32::from).unwrap_or(18.0);
        let offset_y = -((line.saturating_sub(2)) as f32) * line_height;
        input.update(cx, |input, cx| {
            input.set_selected_range(start..end, cx);
            input.set_scroll_offset(point(px(0.0), px(offset_y)), cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Switches what the file search reads, in place.
    ///
    /// A control that reads its own value and swaps it on a tap: the mode is a
    /// two-state setting, and a list to choose between two states is a panel
    /// the reader has to dismiss.
    fn toggle_file_search_mode(&mut self, cx: &mut Context<Self>) {
        self.choose_file_search_mode(self.file_search_mode.toggled(), cx);
    }

    /// Sets what the file search reads.
    ///
    /// Switching to the mode already in force is a no-op, and picking the other
    /// one re-runs the query: the reader changed what the same words mean, so
    /// the list has to answer the question they now think they asked.
    fn choose_file_search_mode(&mut self, mode: MobileFileSearchMode, cx: &mut Context<Self>) {
        if self.file_search_mode == mode {
            return;
        }
        self.file_search_mode = mode;
        if !input_value(&self.file_search_input, cx).trim().is_empty() {
            self.start_file_search(cx);
        } else {
            cx.notify();
        }
    }

    fn clear_file_search(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A timer still waiting for the pause would ask the authority a query
        // the reader has already taken back.
        self.file_search_debounce_task = None;
        self.file_search_request_generation = self.file_search_request_generation.wrapping_add(1);
        if let Some(input) = self.file_search_input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.files.state.search.clear();
        cx.notify();
    }

    /// Queues a light hint for the window's notification layer.
    ///
    /// Nothing is painted from this queue: a hint takes no line in the rail's
    /// layout, so a message cannot leave a strip behind on a surface it no
    /// longer describes. It is handed over once per paint, where the window the
    /// layer needs always exists.
    fn hint(&mut self, tone: Tone, message: impl Into<String>) {
        let message = message.into();
        // Several producers can report one failure before the next frame hands
        // the queue over — the terminal list, the create and the fit all say
        // the same thing when the backend is offline. The toast layer keys a
        // card by its message, so coalescing here only spares it the burst.
        if self
            .pending_hints
            .iter()
            .any(|hint| hint.tone == tone && hint.message == message)
        {
            return;
        }
        self.pending_hints.push(Hint::new(tone, message));
    }

    /// Announces a failure the reader asked for and did not get.
    fn fail(&mut self, error: BackendError) {
        self.hint(Tone::Error, error.message);
    }

    /// Announces one of the repeated failures — the terminal poll, the Git
    /// status poll — at most once while it stays the same.
    ///
    /// These producers run on a timer, and the same broken read would otherwise
    /// push the same card on every tick. The message held for `source` is what
    /// makes the announcement once per distinct failure; a read that recovers
    /// clears its own slot so the next failure is announced again, without the
    /// other sources' records.
    fn hint_failure(&mut self, source: FailureSource, error: Option<BackendError>) {
        let message = error.map(|error| error.message);
        if message.as_ref() == self.last_failures.get(&source) {
            return;
        }
        match &message {
            Some(message) => {
                self.last_failures.insert(source, message.clone());
            }
            None => {
                self.last_failures.remove(&source);
            }
        }
        if let Some(message) = message {
            self.hint(Tone::Error, message);
        }
    }

    /// Hands every queued hint to the window's notification layer.
    ///
    /// The layer lives on the kit's `Root`, which every window this app opens
    /// carries, so a hint can only be pushed from a paint. The queue is taken
    /// whole, oldest first, so a burst of producers — a save that failed while
    /// a terminal refused a resize — still lands in the order it happened.
    fn present_pending_hints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hints = std::mem::take(&mut self.pending_hints);
        for hint in hints {
            crate::toast::push(window, cx, &hint, None);
        }
    }

    /// Returns from the full-screen file view to the file tree. The viewer
    /// keeps its loaded content so re-opening the file is instant.
    fn close_file_screen(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.file_screen_open = false;
        // The go-to-line sheet has nothing to point at once the buffer is off
        // screen, and it is drawn by the screen that is closing.
        self.close_file_goto(cx);
        cx.notify();
    }

    /// Opens `path` in the Files surface, taking the surface over with the
    /// viewer. Used by the timeline, where a file the agent wrote is a link.
    pub fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.surface = WorkbenchSurface::Files;
        self.open_file_now(path, cx);
    }

    /// Reads what the authority knows about this workspace's worktree.
    ///
    /// The desktop's Git panel carries a lifecycle card for a managed
    /// worktree; a phone that starts a session in one had no way to finish
    /// with it, so the worktree could be created and never merged back.
    fn refresh_worktree_lifecycle(&mut self, cx: &mut Context<Self>) {
        if !self
            .git
            .capabilities()
            .supports(BackendOperation::GitWorktreeLifecycleMutate)
            && !self
                .git
                .capabilities()
                .supports(BackendOperation::GitWorktreeRead)
        {
            return;
        }
        let backend = self.backend.clone();
        let workspace_id = self.workspace_id.clone();
        self.worktree_lifecycle_loading = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.git_worktree_snapshot(workspace_id).await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.worktree_lifecycle_loading = false;
                match outcome {
                    Ok(snapshot) => this.worktree_lifecycle = Some(snapshot),
                    // A workspace that is not a managed worktree is not an
                    // error: the card is simply not drawn.
                    Err(_) => this.worktree_lifecycle = None,
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// The managed worktree this rail is pointed at, when it is one.
    fn managed_worktree(&self) -> Option<&GitManagedWorktreeRecord> {
        let snapshot = self.worktree_lifecycle.as_ref()?;
        snapshot.managed_worktrees.iter().find(|worktree| {
            worktree
                .workspace_id
                .as_ref()
                .is_some_and(|id| id == &self.workspace_id)
        })
    }

    /// The merge or archive operation this worktree is in the middle of.
    fn active_worktree_operation(&self) -> Option<&GitWorktreeOperationRecord> {
        let snapshot = self.worktree_lifecycle.as_ref()?;
        let workspace_id = self.workspace_id.as_str();
        snapshot.operations.iter().find(|operation| {
            operation.status == GitWorktreeOperationStatus::Running
                || operation.status == GitWorktreeOperationStatus::Queued
                || (operation.status == GitWorktreeOperationStatus::Failed
                    && operation.source_workspace_id.as_ref().map(|id| id.as_str())
                        == Some(workspace_id))
        })
    }

    /// Marks the worktree ready to merge, or takes that back.
    fn set_worktree_readiness(&mut self, ready: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.managed_worktree() else {
            return;
        };
        let request = GitWorktreeReadinessRequest {
            workspace_id: self.workspace_id.clone(),
            state: if ready {
                GitWorktreeReadinessState::ReadyToMerge
            } else {
                GitWorktreeReadinessState::Working
            },
            expected_source_head: worktree.head.clone(),
            expected_dirty_fingerprint: None,
            checks: Vec::new(),
        };
        let backend = self.backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .git_worktree_set_readiness(MutationRequest::new(request))
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(_) => {
                        this.hint(
                            Tone::Success,
                            locale::text(
                                "Worktree marked",
                                "Worktree 状态已更新",
                                "Worktree 狀態已更新",
                            ),
                        );
                        this.refresh_worktree_lifecycle(cx);
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Asks what a merge would do, and shows the plan before running it.
    fn plan_worktree_merge(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.managed_worktree() else {
            return;
        };
        let Some(target_workspace_id) = worktree.target_workspace_id.clone() else {
            self.fail(BackendError::failed(
                "worktree_target_missing",
                locale::text(
                    "This worktree has no workspace to merge into.",
                    "该 Worktree 没有可合并的目标工作区。",
                    "該 Worktree 沒有可合併的目標工作區。",
                ),
            ));
            cx.notify();
            return;
        };
        let request = GitWorktreeMergeRequest {
            workspace_id: self.workspace_id.clone(),
            source_path: worktree.worktree_path.clone(),
            target_workspace_id: Some(target_workspace_id),
            strategy: None,
            expected_source_head: worktree.head.clone(),
            expected_target_head: None,
            preflight_revision: None,
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { backend.git_worktree_merge_plan(request).await },
            );
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(plan) => {
                        this.worktree_confirmation =
                            Some(WorktreeConfirmation::Merge(Box::new(plan)));
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Runs the merge the reader confirmed.
    fn confirm_worktree_merge(&mut self, cx: &mut Context<Self>) {
        let Some(WorktreeConfirmation::Merge(plan)) = self.worktree_confirmation.take() else {
            return;
        };
        let request = GitWorktreeMergeRequest {
            workspace_id: plan.source_workspace_id.clone(),
            source_path: plan.source_path.clone(),
            target_workspace_id: Some(plan.target_workspace_id.clone()),
            strategy: None,
            expected_source_head: Some(plan.source_head.clone()),
            expected_target_head: Some(plan.target_head.clone()),
            preflight_revision: Some(plan.preflight.revision.clone()),
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .git_worktree_merge(MutationRequest::new(request))
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.hint(
                            Tone::Success,
                            locale::text("Merge started", "已开始合并", "已開始合併"),
                        );
                        this.refresh_worktree_lifecycle(cx);
                        this.refresh_git(cx);
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Abandons the merge that is in progress.
    fn abort_worktree_merge(&mut self, cx: &mut Context<Self>) {
        let Some(operation_id) = self
            .active_worktree_operation()
            .map(|operation| operation.operation_id.clone())
        else {
            return;
        };
        self.run_worktree_operation_action(WorktreeOperationAction::Abort, operation_id, cx);
    }

    /// Marks the conflicted paths resolved, which is what a merge needs to go
    /// on. A phone cannot edit a three-way merge by hand, so the reader fixes
    /// the files in the editor and then says so here.
    fn stage_worktree_conflicts(&mut self, cx: &mut Context<Self>) {
        let Some(operation_id) = self
            .active_worktree_operation()
            .map(|operation| operation.operation_id.clone())
        else {
            return;
        };
        let paths = self
            .active_worktree_operation()
            .map(|operation| {
                operation
                    .detail
                    .conflicts
                    .iter()
                    .filter(|conflict| !conflict.resolved)
                    .map(|conflict| conflict.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if paths.is_empty() {
            self.hint(
                Tone::Warning,
                locale::text(
                    "No conflicted files are listed",
                    "没有列出冲突文件",
                    "沒有列出衝突檔案",
                ),
            );
            cx.notify();
            return;
        }
        let request = GitWorktreeConflictStageRequest {
            operation_id: operation_id.clone(),
            workspace_id: self.workspace_id.clone(),
            paths,
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .git_worktree_stage_conflicts(MutationRequest::new(request))
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.hint(
                            Tone::Success,
                            locale::text(
                                "Conflicts staged",
                                "冲突已标记为已解决",
                                "衝突已標記為已解決",
                            ),
                        );
                        this.refresh_worktree_lifecycle(cx);
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Carries the merge on once its conflicts are resolved.
    fn continue_worktree_merge(&mut self, cx: &mut Context<Self>) {
        let Some(operation_id) = self
            .active_worktree_operation()
            .map(|operation| operation.operation_id.clone())
        else {
            return;
        };
        self.run_worktree_operation_action(WorktreeOperationAction::Continue, operation_id, cx);
    }

    /// Runs one of the two operations that act on a merge already in flight.
    fn run_worktree_operation_action(
        &mut self,
        action: WorktreeOperationAction,
        operation_id: vibex_core::RequestId,
        cx: &mut Context<Self>,
    ) {
        let request = GitWorktreeOperationRequest {
            operation_id,
            workspace_id: self.workspace_id.clone(),
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            match action {
                WorktreeOperationAction::Abort => {
                    backend
                        .git_worktree_abort_merge(MutationRequest::new(request))
                        .await
                }
                WorktreeOperationAction::Continue => {
                    backend
                        .git_worktree_continue_merge(MutationRequest::new(request))
                        .await
                }
            }
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.hint(
                            Tone::Success,
                            match action {
                                WorktreeOperationAction::Abort => {
                                    locale::text("Merge aborted", "已中止合并", "已中止合併")
                                }
                                WorktreeOperationAction::Continue => {
                                    locale::text("Merge continued", "已继续合并", "已繼續合併")
                                }
                            },
                        );
                        this.refresh_worktree_lifecycle(cx);
                        this.refresh_git(cx);
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Asks what discarding would cost, then asks the reader to confirm it.
    fn plan_worktree_discard(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.managed_worktree() else {
            return;
        };
        let request = GitWorktreeDiscardRequest {
            workspace_id: self.workspace_id.clone(),
            worktree_path: worktree.worktree_path.clone(),
            force: false,
            delete_branch: false,
            expected_head: worktree.head.clone(),
            preflight_revision: None,
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.git_worktree_discard_preflight(request).await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(preflight) => {
                        this.worktree_confirmation =
                            Some(WorktreeConfirmation::Discard(Box::new(preflight)));
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Discards the worktree the reader confirmed.
    fn confirm_worktree_discard(&mut self, cx: &mut Context<Self>) {
        let Some(WorktreeConfirmation::Discard(preflight)) = self.worktree_confirmation.take()
        else {
            return;
        };
        let request = GitWorktreeDiscardRequest {
            workspace_id: self.workspace_id.clone(),
            worktree_path: self
                .managed_worktree()
                .map(|worktree| worktree.worktree_path.clone())
                .unwrap_or_default(),
            force: false,
            delete_branch: false,
            expected_head: preflight.source_head.clone(),
            preflight_revision: Some(preflight.revision.clone()),
        };
        let backend = self.backend.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .git_worktree_discard(MutationRequest::new(request))
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.hint(
                            Tone::Success,
                            locale::text(
                                "Worktree discarded",
                                "Worktree 已丢弃",
                                "Worktree 已丟棄",
                            ),
                        );
                        this.refresh_worktree_lifecycle(cx);
                        this.refresh_git(cx);
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn cancel_worktree_confirmation(&mut self, cx: &mut Context<Self>) {
        if self.worktree_confirmation.is_some() {
            self.close_sheet(SheetKey::WorktreeConfirmation, cx);
        }
    }

    /// A tap on a sheet's scrim: what the finger was doing when it landed is
    /// what decides whether it dismisses.
    ///
    /// The lift that ends a pull can land on the scrim itself once the panel
    /// has moved out from under the finger, and that is not a tap anyone made.
    fn tap_sheet_scrim(&mut self, key: SheetKey, cx: &mut Context<Self>) {
        if self.sheet.pull_just_released(key) {
            return;
        }
        self.close_sheet(key, cx);
    }

    /// Drives one sheet from the platform's touch pan.
    ///
    /// The listener belongs to the panel the finger is on, so a page showing
    /// two sheets never moves the wrong one.
    fn pull_sheet(
        &mut self,
        key: SheetKey,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pan) = crate::bottom_sheet::sheet_pan(event) else {
            return;
        };
        match self
            .sheet
            .pull(key, pan, crate::bottom_sheet::sheet_size(window))
        {
            SheetPull::Ignored => {}
            SheetPull::Pulling | SheetPull::Restored => {
                // The panel owns the gesture now: the list inside the sheet
                // must not scroll along with the finger that is moving it.
                cx.stop_propagation();
                cx.notify();
            }
            SheetPull::Dismissed(key) => {
                cx.stop_propagation();
                self.lower_sheet(key, cx);
            }
        }
    }

    /// Asks a sheet to close, and lets its panel leave the screen.
    fn close_sheet(&mut self, key: SheetKey, cx: &mut Context<Self>) {
        if self.sheet.is_leaving(key) {
            // The reader asked a second time: the panel stops falling and is
            // simply gone, rather than making them wait out the animation.
            self.land_sheet(key, cx);
            return;
        }
        self.lower_sheet(key, cx);
    }

    /// Starts the panel's fall. The sheet's own flag stays set until the panel
    /// has landed, which is what keeps its content in the panel as it travels.
    fn lower_sheet(&mut self, key: SheetKey, cx: &mut Context<Self>) {
        let flight = self.sheet.lower(key);
        if flight.is_zero() {
            self.land_sheet(key, cx);
            return;
        }
        self.sheet_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            cx.background_executor().timer(flight).await;
            let _ = entity.update(cx, |this, cx| this.land_sheet(key, cx));
        }));
        cx.notify();
    }

    /// Forgets the sheet whose fall has landed.
    ///
    /// This is where closing a sheet actually ends: the panel has left the
    /// screen, so the page's own flag — the thing that says the sheet is up —
    /// can go. [`BottomSheet::land`] answers `false` for a landing that has
    /// nothing left to clear, which is what keeps a second close from touching
    /// a page's state twice.
    fn land_sheet(&mut self, key: SheetKey, cx: &mut Context<Self>) {
        if !self.sheet.land(key) {
            return;
        }
        match key {
            SheetKey::FileGoto => self.file_goto_open = false,
            SheetKey::GitBranches => self.git_branch_sheet_open = false,
            SheetKey::GitFilter => {
                self.git_author_sheet_open = false;
                self.git_date_sheet_open = false;
            }
            SheetKey::WorktreeConfirmation => self.worktree_confirmation = None,
            SheetKey::GitRevert => self.git_revert_confirmation = None,
            SheetKey::GitDiff => self.git_diff = None,
            // The workspace owns the rest.
            SheetKey::PairingFallback
            | SheetKey::SidebarRowMenu
            | SheetKey::SidebarRootMenu
            | SheetKey::FolderArchiveMenu
            | SheetKey::SidebarRescheduleMenu
            | SheetKey::BatchDelete
            | SheetKey::WorktreePaths
            | SheetKey::GroupMembers
            | SheetKey::ProjectRemove
            | SheetKey::GoalObjective
            | SheetKey::HostActions
            | SheetKey::ComposerHistory
            | SheetKey::ComposerAttach
            | SheetKey::NewSessionPicker
            | SheetKey::RuntimeOptions
            | SheetKey::RuntimeOption
            // The question panel belongs to the conversation, not to the
            // workbench page: the page that opened it clears it.
            | SheetKey::AgentQuestion
            | SheetKey::SettingsPalette => {}
        }
        cx.notify();
    }

    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let ticket = match self.git.begin_status_load() {
            Ok(ticket) => ticket,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.load_status(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let failed = outcome.is_err();
                this.git.apply_status(&ticket, outcome);
                // A read that failed is a light hint, not a card inside the
                // Changes list. The poll repeats it every few seconds, so the
                // message is raised only when the failure changes: a reader who
                // is not looking at the phone does not need it said again.
                let failure = failed
                    .then(|| {
                        this.git
                            .state
                            .last_error
                            .as_ref()
                            .map(|error| error.message.clone())
                    })
                    .flatten()
                    .filter(|_| this.git.state.model.status.is_none());
                if failure != this.git_status_error {
                    this.git_status_error = failure.clone();
                    this.hint(
                        Tone::Error,
                        failure.unwrap_or_else(|| {
                            locale::common("Could not read the repository").to_string()
                        }),
                    );
                }
                if let Some(status) = this.git.state.model.status.as_ref() {
                    this.files.state.tree.set_git_changes(&status.changes);
                }
                if this.git.state.model.mode == GitWorkbenchMode::History {
                    this.refresh_git_history(false, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        // The branch list travels with the status it belongs to: the rail's
        // Changes tab reads how far the branch is from its upstream from the
        // same refresh that reads the working tree, so the badge is never a
        // step behind the panel under it. The desktop's rail loads the two
        // together for the same reason.
        self.refresh_git_sync(cx);
    }

    fn can_mutate_git(&self) -> bool {
        self.backend
            .permits_remote_action(RemoteActionClass::MutateGit)
    }

    fn set_git_mode(&mut self, mode: GitWorkbenchMode, cx: &mut Context<Self>) {
        self.git.state.model.set_mode(mode);
        if mode == GitWorkbenchMode::History {
            self.refresh_git_history(false, cx);
        }
        cx.notify();
    }

    /// Reads the repository's branches, for the history filter's sheet.
    ///
    /// Read fresh each time the sheet opens: branches appear when a worktree is
    /// created or a fetch lands, and a cached list would quietly go stale.
    fn load_git_branches(&mut self, cx: &mut Context<Self>) {
        self.read_git_branches(true, cx);
    }

    /// Reads the branches the rail's own sync badge counts.
    ///
    /// The badge is a hint beside a tab, not a surface the reader opened, so a
    /// list that fails to load leaves it empty rather than raising the rail's
    /// error banner over a panel nobody asked for.
    fn refresh_git_sync(&mut self, cx: &mut Context<Self>) {
        self.read_git_branches(false, cx);
    }

    fn read_git_branches(&mut self, report_error: bool, cx: &mut Context<Self>) {
        if !self
            .git
            .capabilities()
            .supports(BackendOperation::GitBranchList)
        {
            return;
        }
        self.git_branches_loading = true;
        let backend = self.backend.clone();
        let workspace_id = self.workspace_id.clone();
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { backend.git_branch_list(workspace_id).await },
            );
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git_branches_loading = false;
                match outcome {
                    Ok(branches) => {
                        this.git_branches = Some(branches.branches);
                    }
                    Err(error) => {
                        if report_error {
                            this.fail(error);
                        }
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_git_branch_sheet(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git_branch_sheet_open = true;
        self.load_git_branches(cx);
        cx.notify();
    }

    fn close_git_branch_sheet(&mut self, cx: &mut Context<Self>) {
        if self.git_branch_sheet_open {
            self.close_sheet(SheetKey::GitBranches, cx);
        }
    }

    /// Points the history filter at one branch, or at every branch at once.
    ///
    /// `None` is the whole repository: the desktop's branch select offers the
    /// same "all" case, and on a phone it is the way to see a commit that
    /// landed on another branch without walking over to the desktop.
    fn set_git_history_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        let mut filter = self.git.state.model.history_filter.clone();
        if filter.ref_name == branch {
            self.close_sheet(SheetKey::GitBranches, cx);
            return;
        }
        filter.ref_name = branch;
        self.git.state.model.set_history_filter(filter);
        self.close_sheet(SheetKey::GitBranches, cx);
        self.refresh_git_history(false, cx);
        cx.notify();
    }

    fn refresh_git_history(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.git_history_loading && !append {
            self.git_history_request_generation =
                self.git_history_request_generation.wrapping_add(1);
        }
        // The filter is whatever the reader last chose. It used to be filled
        // with the current branch on every refresh, which made the branch chip
        // unchangeable: picking "all branches" was overwritten on the next
        // load.
        let filter = self.git.state.model.history_filter.clone();
        let before_commit = append
            .then(|| {
                self.git
                    .state
                    .model
                    .history
                    .last()
                    .map(|commit| commit.hash.clone())
            })
            .flatten();
        let key = format!(
            "{}:{}:{}:{}:{}:{}",
            filter.ref_name.as_deref().unwrap_or_default(),
            filter.author.as_deref().unwrap_or_default(),
            filter.query.as_deref().unwrap_or_default(),
            filter.authored_after_ms.unwrap_or_default(),
            filter.authored_before_ms.unwrap_or_default(),
            before_commit.as_deref().unwrap_or_default(),
        );
        let Some(ticket) = self.git.state.model.begin_query(GitQueryKind::History, key) else {
            return;
        };
        let request = GitHistoryRequest {
            workspace_id: self.workspace_id.clone(),
            limit: Some(60),
            before_commit,
            ref_name: filter.ref_name,
            author: filter.author,
            query: filter.query,
            authored_after_ms: filter.authored_after_ms,
            authored_before_ms: filter.authored_before_ms,
        };
        self.git_history_loading = true;
        self.git_history_request_generation = self.git_history_request_generation.wrapping_add(1);
        let generation = self.git_history_request_generation;
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_history(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if generation != this.git_history_request_generation {
                    return;
                }
                this.git_history_loading = false;
                match outcome {
                    Ok(history) => {
                        if !this.git.state.model.apply_history(&ticket, history, append) {
                            return;
                        }
                    }
                    Err(error) => {
                        this.git.state.model.fail_query(&ticket, &error.code);
                        this.fail(error);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Create the kit input states on first paint and land any clear that was
    /// requested from a path without a window.
    ///
    /// `InputState::new` and every programmatic edit take `&mut Window`, but the
    /// workbench is built and mutated from async paths that have none. Painting
    /// always has one, so the fields are born here and deferred clears apply here.
    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_search_input.is_none() {
            self.file_search_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Search files",
                    "搜索文件",
                    "搜尋檔案",
                ))
            }));
        }
        if self.file_search_subscription.is_none()
            && let Some(search) = self.file_search_input.clone()
        {
            self.file_search_subscription = Some(cx.subscribe_in(
                &search,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 _: &mut Window,
                 cx: &mut Context<Self>| {
                    match event {
                        // The field is the search. Waiting for Enter made the
                        // panel answer the empty query with "No matching items"
                        // while the reader was still typing, because a phone
                        // keyboard never sent the key it was waiting for.
                        InputEvent::Change => this.schedule_file_search(cx),
                        InputEvent::PressEnter { .. } => this.start_file_search(cx),
                        _ => {}
                    }
                },
            ));
        }
        if self.file_find_input.is_none() {
            self.file_find_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Find in file",
                    "在文件中查找",
                    "在檔案中尋找",
                ))
            }));
        }
        if self.file_find_subscription.is_none()
            && let Some(find) = self.file_find_input.clone()
        {
            self.file_find_subscription = Some(cx.subscribe_in(
                &find,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 _: &mut Window,
                 cx: &mut Context<Self>| {
                    if matches!(event, InputEvent::Change) {
                        this.apply_file_find_query(cx);
                    }
                },
            ));
        }
        if self.file_goto_input.is_none() {
            self.file_goto_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text("Line number", "行号", "行號"))
            }));
        }
        if self.git_commit_input.is_none() {
            self.git_commit_input = Some(cx.new(|cx| {
                // Three rows, the desktop's own height for the same field: one
                // for the subject and two to show that a body has somewhere to
                // go.
                TextareaState::new(window, cx)
                    .rows(3)
                    .placeholder(locale::text("Commit message", "提交消息", "提交訊息"))
            }));
        }
        if self.git_history_query_input.is_none() {
            self.git_history_query_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Search commit information or code",
                    "搜索提交信息或代码",
                    "搜尋提交資訊或代碼",
                ))
            }));
            if let Some(input) = self.git_history_query_input.clone() {
                self.git_history_query_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     _: &mut Window,
                     cx: &mut Context<Self>| {
                        match event {
                            // Typing is the query: the soft keyboard's action
                            // key is not a button the reader can be asked to
                            // press, and waiting for Enter meant the commit
                            // search never ran at all.
                            InputEvent::Change => this.schedule_git_history_search(cx),
                            InputEvent::PressEnter { .. } => this.start_git_history_search(cx),
                            _ => {}
                        }
                    },
                ));
            }
        }
        if self.file_editor_input.is_none() {
            self.file_editor_input = Some(cx.new(|cx| {
                // The code-editor kind rather than a plain textarea: it carries
                // the line-number gutter, the indent guides, and the syntax
                // highlight a source file is read with. The language is set
                // from the file's path before its text lands.
                //
                // Its read-only flag is set on the element that draws it, not
                // here: see [`file_editor_element`].
                EditorState::new(window, cx)
                    .folding(false)
                    .placeholder(locale::text("File content", "文件内容", "檔案內容"))
            }));
        }
        let sink_created = self.terminal_input.is_none();
        if sink_created {
            self.terminal_input = Some(cx.new(|cx| TextareaState::new(window, cx).rows(1)));
        }
        // The sink starts, and stays, on its seed (see [`TERMINAL_SINK_SEED`]),
        // with the caret after it: every key the reader presses has text in
        // front of it to be measured against. `terminal_clear_pending` is the
        // paths without a window asking for the same thing at the next paint.
        //
        // This is not a "value differs from the seed" check: an IME's
        // composition is a value that differs from it and that must survive the
        // paint, and the change handler already re-seeds after every commit.
        if sink_created || std::mem::take(&mut self.terminal_clear_pending) {
            self.reseed_terminal_sink(window, cx);
        }
        if let Some(sink) = self.terminal_input.clone()
            && self.terminal_sink_subscription.is_none()
        {
            // Every committed character is forwarded to the PTY as it lands,
            // and the field is put back on its seed in the same breath, so the
            // next key is measured against the document it will actually edit.
            // Waiting for a paint to refill it meant a second Backspace arrived
            // at an empty field, which the keyboard reads as an edit of nothing:
            // that is how a held Backspace deleted one character and stopped.
            self.terminal_sink_subscription = Some(cx.subscribe_in(
                &sink,
                window,
                |this: &mut Self,
                 _: &Entity<TextareaState>,
                 event: &InputEvent,
                 window: &mut Window,
                 cx: &mut Context<Self>| {
                    match event {
                        InputEvent::Change => {
                            let next = textarea_value(&this.terminal_input, cx).to_string();
                            this.forward_terminal_sink(&next, cx);
                            // `set_value` is silent, so this cannot re-enter
                            // the event it is answering.
                            this.reseed_terminal_sink(window, cx);
                        }
                        // The kit inserts the newline into the field silently
                        // and announces the key here, so the field is put back
                        // on its seed after the Enter: the newline left behind
                        // would otherwise be forwarded again, as an Enter, with
                        // the next character typed.
                        InputEvent::PressEnter { .. } => {
                            this.send_terminal_value(
                                TerminalInput::Key(
                                    TerminalKey::Enter,
                                    TerminalKeyModifiers::default(),
                                ),
                                false,
                                cx,
                            );
                            this.reseed_terminal_sink(window, cx);
                        }
                        _ => {}
                    }
                },
            ));
        }
        if self.terminal_search_input.is_none() {
            self.terminal_search_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Find in terminal",
                    "在终端中查找",
                    "在終端機中尋找",
                ))
            }));
            if let Some(input) = self.terminal_search_input.clone() {
                self.terminal_search_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     _: &mut Window,
                     cx: &mut Context<Self>| {
                        match event {
                            InputEvent::Change => this.refresh_terminal_search(cx),
                            InputEvent::PressEnter { .. } => {
                                this.select_next_terminal_search_match(cx)
                            }
                            _ => {}
                        }
                    },
                ));
            }
        }

        if let Some(content) = self.pending_file_editor_content.take()
            && let Some(input) = self.file_editor_input.clone()
        {
            // The language is chosen by the path, so it is set before the text
            // lands: the highlight is parsed from the buffer the reader sees,
            // and a buffer that arrives first is parsed again on the next
            // frame.
            let path = self.file_editor_path.clone().unwrap_or_default();
            let language = file_language_for_path(&path);
            input.update(cx, |input, cx| {
                input.set_highlighter(language, cx);
                input.set_value(content, window, cx);
            });
        }
        // After the buffer write, and before the clears borrow the fields the
        // reveal also reads.
        self.apply_pending_file_reveal(window, cx);

        let clears = [
            (&mut self.file_search_clear_pending, &self.file_search_input),
            (
                &mut self.git_history_query_clear_pending,
                &self.git_history_query_input,
            ),
        ];
        for (pending, input) in clears {
            if !*pending {
                continue;
            }
            *pending = false;
            if let Some(input) = input.clone() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        if std::mem::take(&mut self.git_commit_clear_pending)
            && let Some(input) = self.git_commit_input.clone()
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Runs the commit query once the reader pauses.
    ///
    /// The commit search used to wait for the keyboard's action key, which the
    /// soft keyboard never sent: the field took the words and no query left the
    /// phone. Typing now schedules the query, and the pause after the last
    /// keystroke is what spends the request.
    fn schedule_git_history_search(&mut self, cx: &mut Context<Self>) {
        self.git_history_query_debounce_task =
            Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                cx.background_executor()
                    .timer(GIT_HISTORY_SEARCH_DEBOUNCE)
                    .await;
                let _ = entity.update(cx, |this, cx| {
                    this.git_history_query_debounce_task = None;
                    this.start_git_history_search(cx);
                });
            }));
    }

    /// Runs the commit-history query as it now stands.
    ///
    /// Reached from the pause after a keystroke, and from the keyboard's own
    /// action key where the platform sends one: the field is the control, and a
    /// second framed glyph beside it would only repeat what the field does.
    fn start_git_history_search(&mut self, cx: &mut Context<Self>) {
        // A timer still waiting for its pause is superseded by this run.
        self.git_history_query_debounce_task = None;
        let query = input_value(&self.git_history_query_input, cx)
            .trim()
            .to_string();
        let query = (!query.is_empty()).then_some(query);
        // The same words do not ask the authority again: Enter after the pause
        // has already answered them, and re-running would empty the list the
        // reader is looking at.
        if self.git.state.model.history_filter.query == query {
            return;
        }
        let mut filter = self.git.state.model.history_filter.clone();
        filter.query = query;
        self.git.state.model.set_history_filter(filter);
        self.refresh_git_history(false, cx);
    }

    fn clear_git_history_search(
        &mut self,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.git_history_query_input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        // A timer still waiting for its pause would run the query the reader
        // has just taken back.
        self.git_history_query_debounce_task = None;
        let mut filter = self.git.state.model.history_filter.clone();
        filter.query = None;
        self.git.state.model.set_history_filter(filter);
        self.refresh_git_history(false, cx);
    }

    fn open_git_author_sheet(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git_author_sheet_open = true;
        cx.notify();
    }

    fn close_git_author_sheet(&mut self, cx: &mut Context<Self>) {
        if self.git_author_sheet_open {
            self.close_sheet(SheetKey::GitFilter, cx);
        }
    }

    /// Points the history filter at one author, or at everyone.
    ///
    /// A list rather than the cycle this used to be: the reader picked a name
    /// by tapping through the authors one at a time, which is no way to reach
    /// the fifth of nine.
    fn set_git_history_author(&mut self, author: Option<String>, cx: &mut Context<Self>) {
        let mut filter = self.git.state.model.history_filter.clone();
        if filter.author == author {
            self.close_sheet(SheetKey::GitFilter, cx);
            return;
        }
        filter.author = author;
        self.git.state.model.set_history_filter(filter);
        self.close_sheet(SheetKey::GitFilter, cx);
        self.refresh_git_history(false, cx);
        cx.notify();
    }

    fn open_git_date_sheet(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git_date_sheet_open = true;
        cx.notify();
    }

    fn close_git_date_sheet(&mut self, cx: &mut Context<Self>) {
        if self.git_date_sheet_open {
            self.close_sheet(SheetKey::GitFilter, cx);
        }
    }

    /// Points the history filter at a window of time.
    ///
    /// Presets rather than a calendar: a phone date picker is three taps deep
    /// for a question that is almost always "recently".
    fn set_git_history_window(
        &mut self,
        window: Option<MobileHistoryWindow>,
        cx: &mut Context<Self>,
    ) {
        let mut filter = self.git.state.model.history_filter.clone();
        let (after, before) = match window {
            Some(window) => window.bounds(vibex_core::unix_timestamp_ms()),
            None => (None, None),
        };
        filter.authored_after_ms = after;
        filter.authored_before_ms = before;
        self.git.state.model.set_history_filter(filter);
        self.close_sheet(SheetKey::GitFilter, cx);
        self.refresh_git_history(false, cx);
        cx.notify();
    }

    fn load_more_git_history(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.git_history_loading && self.git.state.model.history_has_more {
            self.refresh_git_history(true, cx);
        }
    }

    fn run_git_remote_action(&mut self, kind: GitRemoteActionKind, cx: &mut Context<Self>) {
        if self.busy
            || !self
                .git
                .capabilities()
                .supports(BackendOperation::GitRemoteAction)
            || !self.can_mutate_git()
        {
            return;
        }
        let request = GitRemoteActionRequest {
            workspace_id: self.workspace_id.clone(),
            kind,
            remote: None,
            branch: None,
        };
        self.busy = true;
        self.git_remote_action = Some(kind);
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_remote_action(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                this.git_remote_action = None;
                match outcome {
                    Ok(result) => {
                        // Reconcile through the normal ticketed status path so
                        // the shared tree and selection model stay authoritative.
                        this.refresh_git(cx);
                        // The desktop answers a push or a pull with the action's
                        // own name; a fetch is bookkeeping it does not announce.
                        // The authority's own summary is the sentence for
                        // anything else, so a runtime that reports more than the
                        // desktop does is still heard.
                        let notice = match kind {
                            GitRemoteActionKind::Pull => {
                                Some(locale::text("Pull completed", "拉取已完成", "拉取已完成"))
                            }
                            GitRemoteActionKind::Push => {
                                Some(locale::text("Push completed", "推送已完成", "推送已完成"))
                            }
                            GitRemoteActionKind::Fetch => None,
                        };
                        this.hint(
                            Tone::Success,
                            notice.map(str::to_string).unwrap_or(result.summary),
                        );
                    }
                    Err(error) => {
                        // A failed push or pull names the action it failed at,
                        // the way the desktop's own failure hint does.
                        let label = match kind {
                            GitRemoteActionKind::Fetch => {
                                Some(locale::text("Fetch failed", "获取失败", "取得失敗"))
                            }
                            GitRemoteActionKind::Pull => {
                                Some(locale::text("Pull failed", "拉取失败", "拉取失敗"))
                            }
                            GitRemoteActionKind::Push => {
                                Some(locale::text("Push failed", "推送失败", "推送失敗"))
                            }
                        };
                        match label {
                            Some(label) => {
                                this.hint(Tone::Error, format!("{label}: {}", error.message))
                            }
                            None => this.fail(error),
                        }
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Asks before discarding local edits. The confirmation names the file
    /// count, because a rollback is the one Git action here that destroys work
    /// the user cannot recover from the phone.
    fn revert_selected_git(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || !self
                .git
                .capabilities()
                .supports(BackendOperation::GitRevert)
            || !self.can_mutate_git()
        {
            return;
        }
        let paths = self.git.state.model.selected_change_paths();
        if paths.is_empty() {
            return;
        }
        self.git_revert_confirmation = Some(paths);
        cx.notify();
    }

    fn cancel_revert_git(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git_revert_confirmation = None;
        cx.notify();
    }

    fn confirm_revert_git(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(paths) = self.git_revert_confirmation.take() else {
            return;
        };
        if self.busy
            || !self
                .git
                .capabilities()
                .supports(BackendOperation::GitRevert)
            || !self.can_mutate_git()
        {
            return;
        }
        let request = GitStageRequest {
            workspace_id: self.workspace_id.clone(),
            paths,
        };
        self.busy = true;
        let backend = self.backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move { backend.git_revert(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.refresh_git(cx);
                        this.hint(
                            Tone::Success,
                            locale::common("Selected changes rolled back"),
                        );
                    }
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_git_diff(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        self.git_diff_limit = GIT_DIFF_PAGE_LINES;
        let ticket = match self.git.begin_diff_load(path, staged) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.load_diff(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let display = outcome.as_ref().ok().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_diff(&ticket, outcome);
                let opened = display.is_some();
                this.git_diff = display;
                if opened {
                    this.git_diff_scroll.set_offset(gpui::Point::default());
                }
                this.hint_failure(FailureSource::Git, failure);
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Opens the full-screen commit view and loads the commit detail (files
    /// and patch) from the desktop.
    fn open_git_commit(&mut self, commit_hash: String, cx: &mut Context<Self>) {
        self.git.state.model.select_commit(commit_hash.clone());
        self.git_commit_detail = None;
        self.git_commit_detail_loading = true;
        // The message body starts folded, as it does on the desktop: the header
        // already carries the subject, and the diff is what the screen is for.
        // Every file's patch starts open, so the commit reads the way
        // `git show` prints it until the reader folds something.
        self.git_commit_body_expanded = false;
        self.git_commit_collapsed_files.clear();
        let request = GitCommitDetailRequest {
            workspace_id: self.workspace_id.clone(),
            commit_hash,
            include_patch: true,
        };
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_commit_detail(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git_commit_detail_loading = false;
                match outcome {
                    Ok(detail) => this.git_commit_detail = Some(detail),
                    Err(error) => this.fail(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Returns from the full-screen commit view to the Commits list.
    fn close_git_commit_screen(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git_commit_detail = None;
        self.git_commit_detail_loading = false;
        cx.notify();
    }

    /// Folds or unfolds one file's part of the open commit's patch.
    fn toggle_git_commit_file(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.git_commit_collapsed_files.remove(path) {
            self.git_commit_collapsed_files.insert(path.to_string());
        }
        cx.notify();
    }

    fn request_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git_commit_amend = false;
        self.git_commit_push_after = false;
        if !self
            .git
            .capabilities()
            .supports(BackendOperation::GitCommit)
            || !self.can_mutate_git()
        {
            return;
        }
        let message = textarea_value(&self.git_commit_input, cx)
            .trim()
            .to_string();
        let paths = self.git.state.model.selected_change_paths();
        if paths.is_empty() {
            self.fail(BackendError::failed(
                "git_paths_empty",
                locale::text(
                    "Select at least one change first.",
                    "请先选择至少一项更改。",
                    "請先選擇至少一項變更。",
                ),
            ));
            cx.notify();
            return;
        }
        match self.git.request_commit_confirmation(message, paths) {
            Ok(_) => {
                self.git_commit_confirmation = true;
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    fn cancel_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git.cancel_commit();
        self.git_commit_confirmation = false;
        self.git_commit_amend = false;
        self.git_commit_push_after = false;
        cx.notify();
    }

    /// Rewrites the previous commit rather than adding one.
    fn toggle_git_commit_amend(&mut self, cx: &mut Context<Self>) {
        let amend = !self.git_commit_amend;
        match self.git.set_commit_amend(amend) {
            Ok(()) => {
                self.git_commit_amend = amend;
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    /// Pushes the branch once the commit lands.
    fn toggle_git_commit_push_after(&mut self, cx: &mut Context<Self>) {
        let push = !self.git_commit_push_after;
        match self.git.set_commit_push_after(push) {
            Ok(()) => {
                self.git_commit_push_after = push;
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    fn confirm_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let operation = self
            .git
            .confirm_commit()
            .and_then(|()| self.git.begin_confirmed_commit());
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        self.git_commit_confirmation = false;
        self.git_commit_amend = false;
        self.git_commit_push_after = false;
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.run_commit(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_commit(&operation, outcome);
                this.busy = false;
                match failure {
                    Some(error) => this.fail(error),
                    None => {
                        this.git_commit_clear_pending = true;
                        this.hint(Tone::Success, locale::common("Commit created on desktop"));
                        this.refresh_git(cx);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Opens the Agent's own terminal for a terminal-mode session.
    ///
    /// The session's PTY is not a workspace tab, so it never appears in the
    /// terminal list and the reader cannot reach it by browsing. Selecting the
    /// session is the gesture that opens it: the snapshot already names the
    /// terminal, so the rail attaches directly rather than listing.
    pub fn open_session_terminal(
        &mut self,
        session: &vibex_core::AgentSession,
        cx: &mut Context<Self>,
    ) {
        let Some(facts) = session.terminal.as_deref() else {
            return;
        };
        let Some(terminal_id) = facts.terminal_id.clone() else {
            return;
        };
        if self
            .terminal
            .state
            .active_session
            .as_ref()
            .is_some_and(|active| active.id == terminal_id)
        {
            return;
        }
        // The snapshot carries the identity the grid needs, and the terminal's
        // own metadata is answered by `terminal_snapshot` on attach.
        let now = vibex_core::unix_timestamp_ms();
        let terminal = vibex_core::TerminalSession {
            id: terminal_id.clone(),
            workspace_id: session.workspace_id.clone(),
            title: session.title.clone(),
            shell: session.agent_id.as_str().to_string(),
            cwd: session.workspace_root.clone(),
            rows: TERMINAL_DEFAULT_ROWS,
            cols: TERMINAL_DEFAULT_COLS,
            status: vibex_core::TerminalStatus::Running,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        self.stop_terminal_poll();
        match self.terminal.attach_session_terminal(terminal) {
            Ok(()) => {
                self.terminal_fit_error = None;
                self.terminal_render_sequence = 0;
                self.terminal_fit_size = None;
                self.terminal_last_selected = Some(terminal_id.clone());
                self.start_terminal_poll(terminal_id, cx);
            }
            Err(error) => self.fail(error),
        }
        cx.notify();
    }

    fn refresh_terminals(&mut self, cx: &mut Context<Self>) {
        self.stop_terminal_poll();
        // `begin_refresh` clears the attached session, so remember which one
        // the user was looking at and put it back when the list returns. A
        // refresh that dropped the attachment left the panel on "Select or
        // create a terminal" until the user found the tab strip.
        let previous = self
            .terminal
            .state
            .active_session
            .as_ref()
            .map(|session| session.id.clone())
            .or_else(|| self.terminal_last_selected.clone());
        let ticket = match self.terminal.begin_refresh(self.workspace_id.clone()) {
            Ok(ticket) => ticket,
            Err(error) => {
                // A refresh is issued whenever the rail is entered and again
                // whenever it resumes, so a backend that is offline says the
                // same thing several times in a row. `hint_failure` keeps that
                // to one card per distinct failure.
                self.hint_failure(FailureSource::Terminal, Some(error));
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.load_sessions(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_refresh(&ticket, outcome);
                this.hint_failure(FailureSource::Terminal, failure);
                if this.terminal.state.active_session.is_none() {
                    // The tab the reader left first, then the head of the live
                    // list. A terminal that has exited or been closed is not a
                    // place to put them: the desktop drops those from the same
                    // list, and attaching to one would paint a dead grid.
                    let next = previous
                        .filter(|id| {
                            this.terminal
                                .state
                                .sessions
                                .iter()
                                .any(|session| &session.id == id && is_live_terminal(session))
                        })
                        .or_else(|| {
                            this.terminal
                                .state
                                .sessions
                                .iter()
                                .find(|session| is_live_terminal(session))
                                .map(|session| session.id.clone())
                        });
                    match next {
                        Some(terminal_id) => {
                            this.terminal_auto_create = false;
                            this.attach_terminal(terminal_id, cx);
                        }
                        // No live terminal and the reader is standing on the
                        // terminal: open one, once per visit.
                        None => {
                            if std::mem::take(&mut this.terminal_auto_create) {
                                this.open_terminal_when_idle(cx);
                            }
                        }
                    }
                } else {
                    // A refresh that found something has answered the question
                    // the auto-create was holding open.
                    this.terminal_auto_create = false;
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn create_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.create_terminal_now(cx);
    }

    /// Opens a terminal if the reader is on the terminal surface and none is
    /// running.
    ///
    /// Called where the list may have just lost its last live session, so the
    /// strip the reader is looking at is never empty: an empty terminal pane
    /// offers a button, and a phone has no room for a screen whose only content
    /// is a control.
    fn open_terminal_when_idle(&mut self, cx: &mut Context<Self>) {
        if self.surface != WorkbenchSurface::Terminal
            || self.terminal_is_running()
            || self.busy
            || !self
                .terminal
                .capabilities
                .supports(BackendOperation::TerminalCreate)
        {
            return;
        }
        self.create_terminal_now(cx);
    }

    /// Opens a terminal session and attaches to it. The window the button has
    /// is not needed for either, which is what lets the surface's own
    /// empty-list path open the first terminal without one.
    fn create_terminal_now(&mut self, cx: &mut Context<Self>) {
        let request = MutationRequest::new(TerminalCreateRequest {
            workspace_id: self.workspace_id.clone(),
            title: Some(locale::text("Mobile terminal", "移动端终端", "行動端終端機").to_string()),
            shell: None,
            cwd: None,
            rows: 24,
            cols: 80,
        });
        let operation = match self.terminal.begin_create(request) {
            Ok(operation) => operation,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_create(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let created_id = outcome.as_ref().ok().map(|session| session.id.clone());
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_create(&operation, outcome);
                this.busy = false;
                if let Some(error) = failure {
                    this.fail(error);
                }
                if let Some(terminal_id) = created_id {
                    this.attach_terminal(terminal_id, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn attach_terminal(&mut self, terminal_id: TerminalId, cx: &mut Context<Self>) {
        self.terminal_close_confirmation = None;
        self.reset_terminal_text_state();
        // A keystroke half-typed into the previous terminal is not text for the
        // next one: the field is emptied before it can be diffed against a
        // buffer that belongs to a session the reader has left.
        self.terminal_clear_pending = true;
        match self.terminal.attach(terminal_id.clone()) {
            Ok(()) => {
                self.terminal_fit_error = None;
                self.terminal_render_sequence = 0;
                self.terminal_fit_size = None;
                // Remembered so the next visit — and the next refresh of the
                // list — comes back to the tab the reader left.
                self.terminal_last_selected = Some(terminal_id.clone());
                self.start_terminal_poll(terminal_id, cx);
            }
            Err(error) => {
                self.fail(error);
                cx.notify();
            }
        }
    }

    fn stop_terminal_poll(&mut self) {
        self.terminal_poll_generation = self.terminal_poll_generation.saturating_add(1).max(1);
    }

    fn start_terminal_poll(&mut self, terminal_id: TerminalId, cx: &mut Context<Self>) {
        self.stop_terminal_poll();
        let generation = self.terminal_poll_generation;
        self.poll_terminal_snapshot(terminal_id, generation, cx);
    }

    fn poll_terminal_snapshot(
        &mut self,
        terminal_id: TerminalId,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if generation != self.terminal_poll_generation {
            return;
        }
        let backend = self.backend.clone();
        let requested_terminal_id = terminal_id.clone();
        let background = cx.background_executor().clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.terminal_snapshot(requested_terminal_id).await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let continue_polling = entity
                .update(cx, |this, cx| {
                    let current = generation == this.terminal_poll_generation
                        && this.surface == WorkbenchSurface::Terminal
                        && this
                            .terminal
                            .state
                            .active_session
                            .as_ref()
                            .is_some_and(|session| session.id == terminal_id);
                    if !current {
                        return false;
                    }
                    match outcome {
                        Ok(snapshot) => {
                            this.apply_terminal_snapshot(snapshot);
                            this.hint_failure(FailureSource::Terminal, None);
                        }
                        Err(error) => this.hint_failure(FailureSource::Terminal, Some(error)),
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !continue_polling {
                return;
            }
            background.timer(TERMINAL_POLL_INTERVAL).await;
            let _ = entity.update(cx, |this, cx| {
                if generation == this.terminal_poll_generation
                    && this.surface == WorkbenchSurface::Terminal
                    && this
                        .terminal
                        .state
                        .active_session
                        .as_ref()
                        .is_some_and(|session| session.id == terminal_id)
                {
                    this.poll_terminal_snapshot(terminal_id.clone(), generation, cx);
                }
            });
        });
        task.detach();
    }

    /// Feeds a polled snapshot into the shared render model. Chunks already
    /// applied are skipped by sequence; a sequence reset (snapshot evicted,
    /// resized, or terminal reattached) rebuilds the emulator from the
    /// retained window, mirroring the desktop raw-buffer rebuild rules.
    fn apply_terminal_snapshot(&mut self, snapshot: TerminalSnapshot) {
        let render = self
            .terminal
            .state
            .render
            .get_or_insert_with(|| vibex_ui::TerminalRenderModel::new(1, 1));
        terminal_feed_snapshot(render, &mut self.terminal_render_sequence, &snapshot);
        // A rebuild drops the emulator's own selection with the grid it pointed
        // into, so a live touch selection is re-applied to what is left.
        if let Some(selection) = self.terminal_selection {
            self.terminal_selection_text = render
                .select_text(selection.anchor, selection.head)
                .ok()
                .or_else(|| self.terminal_selection_text.clone());
        }
    }

    /// The room the keyboard left below the terminal grid, in logical pixels.
    ///
    /// The grid hangs from the bottom of a pane that grows the instant the
    /// keyboard closes, while the rows that fill it only exist once the runtime
    /// has answered `TerminalResize` and the resized frame has been polled back.
    /// Between those two moments a pane that grew without its rows drags the
    /// whole terminal down by the keyboard's height and springs back when they
    /// arrive. The frame's own row count is what says whether the rows are
    /// current: until it moves, the room the keyboard gave up is held open below
    /// the grid instead, and the rows never leave the place the reader was
    /// looking at.
    fn hold_terminal_grid(&mut self, window: &Window) -> f32 {
        let rows = self
            .terminal
            .state
            .render
            .as_ref()
            .map(|render| render.frame.rows);
        let Some(rows) = rows else {
            self.terminal_grid_rows = None;
            return 0.0;
        };
        // The pane's height up to the chrome that never moves with a keyboard:
        // whatever resized the window, this is what the rows have to fill.
        let insets = window.safe_insets();
        let height = f32::from(window.viewport_size().height) - insets.top - insets.bottom;
        terminal_grid_hold(rows, height, &mut self.terminal_grid_rows)
    }

    /// Auto-fits the PTY to the available surface. The desktop grid geometry
    /// (8x18px cells at 13px) determines how many rows/columns fit, and the
    /// shared render model is resized in lock-step so the frame matches.
    fn fit_terminal_to(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let cols = ((((width - TERMINAL_HORIZONTAL_PADDING * 2.0) / TERMINAL_CELL_WIDTH).floor()
            as i32)
            .clamp(i32::from(TERMINAL_MIN_COLS), i32::from(TERMINAL_MAX_COLS)))
            as u16;
        let rows = ((((height - TERMINAL_VERTICAL_PADDING * 2.0) / TERMINAL_CELL_HEIGHT).floor()
            as i32)
            .clamp(i32::from(TERMINAL_MIN_ROWS), i32::from(TERMINAL_MAX_ROWS)))
            as u16;
        let Some(session) = self.terminal.state.active_session.as_ref() else {
            return;
        };
        if session.rows == rows && session.cols == cols {
            return;
        }
        if self.terminal_fit_size == Some((rows, cols)) || self.busy {
            return;
        }
        self.terminal_fit_size = Some((rows, cols));
        let terminal_id = session.id.clone();
        let operation = match self.terminal.begin_resize(rows, cols) {
            Ok(operation) => operation,
            Err(error) => {
                // This runs from the grid's paint closure. Recording the error
                // and notifying here would schedule the next frame from inside
                // this one and the surface would repaint forever on a runtime
                // that does not implement `TerminalResize`; the fit is simply
                // marked unavailable and reported once.
                self.terminal_fit_size = Some((rows, cols));
                if self.terminal_fit_error.is_none() {
                    self.terminal_fit_error = Some(error.message.clone());
                    self.hint(Tone::Error, error.message);
                }
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_resize(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let current = this
                    .terminal
                    .state
                    .active_session
                    .as_ref()
                    .is_some_and(|session| session.id == terminal_id);
                if !current {
                    return;
                }
                let failure = outcome.as_ref().err().cloned();
                let resized = this.terminal.apply_resize(&operation, outcome);
                if resized {
                    if let Some(session) = this.terminal.state.active_session.as_ref()
                        && let Some(render) = this.terminal.state.render.as_mut()
                    {
                        render.resize(session.rows, session.cols);
                    }
                    this.terminal_render_sequence = 0;
                    this.start_terminal_poll(terminal_id, cx);
                    if this.terminal_search_open {
                        // The resize rebuilt the emulator, so the matches found
                        // against the old grid point at cells that are gone.
                        this.refresh_terminal_search(cx);
                    }
                }
                this.hint_failure(FailureSource::Terminal, failure);
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Puts the sink back on its seed and re-bases the diff with it.
    ///
    /// The field is the terminal's keyboard, not its buffer: every keystroke is
    /// forwarded and the field goes back to the one character the next key is
    /// measured against. `set_value` is the kit's silent write — it replaces
    /// the document without emitting `InputEvent::Change` — so this cannot
    /// answer the change that called it, and the caret goes to the end because
    /// that is the only place a key can be typed or deleted at.
    fn reseed_terminal_sink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sink) = self.terminal_input.clone() {
            sink.update(cx, |sink, cx| {
                sink.set_value(TERMINAL_SINK_SEED, window, cx);
                let caret = TERMINAL_SINK_SEED.len();
                sink.set_selected_range(caret..caret, cx);
            });
        }
        self.terminal_sink_previous = TERMINAL_SINK_SEED.to_string();
    }

    /// The sink's value changed: forward only what the user just typed or
    /// deleted.
    ///
    /// `next` is the value the change fired for. Reading the field again from
    /// the caller would turn a later keystroke into this one's text.
    fn forward_terminal_sink(&mut self, next: &str, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.terminal_sink_previous, next.to_string());
        match terminal_sink_edit(&previous, next) {
            TerminalSinkEdit::Typed(typed) => self.send_terminal_text(&typed, cx),
            // A deletion (backspace, or a selection replaced by nothing) is a
            // shorter value; the PTY is told one DEL per removed character.
            TerminalSinkEdit::Deleted(deletes) => {
                self.send_terminal_value(TerminalInput::Text("\u{7f}".repeat(deletes)), false, cx)
            }
            // Anything else (an IME rewriting the whole buffer) is not a
            // keystroke the PTY can be told about; dropping it beats
            // duplicating input.
            TerminalSinkEdit::Ignored => {}
        }
    }

    /// Forwards committed text, turning the newlines in it back into Enter.
    ///
    /// The phone's keyboard does not send an Enter *key*: the host's IME target
    /// is a multi-line editor, so its Return key commits a newline character
    /// into the field instead. Reading that character as the key it stands for
    /// is what makes a command typed on a phone actually run — without it the
    /// line reached the shell with no terminator, so the reader saw their
    /// command echoed and then nothing at all.
    fn send_terminal_text(&mut self, typed: &str, cx: &mut Context<Self>) {
        for (after_return, segment) in terminal_text_segments(typed) {
            if after_return {
                self.send_terminal_value(
                    TerminalInput::Key(TerminalKey::Enter, TerminalKeyModifiers::default()),
                    false,
                    cx,
                );
            }
            if !segment.is_empty() {
                let input = TerminalInput::Text(self.apply_terminal_latches(&segment));
                self.send_terminal_value(input, false, cx);
            }
        }
    }

    /// Applies the accessory bar's latched modifiers to text typed on the
    /// keyboard. The shared encoder applies the Ctrl latch to key events only,
    /// and a soft keyboard produces text, so the byte has to be built here —
    /// otherwise the highlighted ⌃ chip would be decoration.
    fn apply_terminal_latches(&mut self, typed: &str) -> String {
        let alt = std::mem::take(&mut self.terminal_alt_latched);
        let control = std::mem::take(&mut self.terminal.state.control_latched);
        let mut out = String::new();
        if alt {
            out.push('\u{1b}');
        }
        if control {
            for character in typed.chars() {
                let byte = character as u32;
                if byte < 0x80 && character.is_ascii_alphabetic() {
                    out.push(char::from_u32(byte & 0x1f).unwrap_or(character));
                } else if character == ' ' {
                    out.push('\0');
                } else {
                    out.push(character);
                }
            }
        } else {
            out.push_str(typed);
        }
        out
    }

    /// Tapping the grid puts the software keyboard on the terminal.
    fn focus_terminal(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A tap on the grid is the user leaving selection mode; the recognizer
        // reports a touch as a tap only when it never became a long press, so
        // this can never cut a selection drag short.
        self.dismiss_terminal_selection(cx);
        self.show_terminal_keyboard(window, cx);
        self.click_terminal_cell(event.position, cx);
    }

    /// A tap belongs to the application when it asked for mouse reporting.
    ///
    /// tmux takes one to pick a pane, vim to move the cursor, htop to click a
    /// row. The desktop keeps a tap local with Shift; a phone has no such key,
    /// so here the application wins the tap and a long press is what selects
    /// text locally instead.
    fn click_terminal_cell(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(point) = self.terminal_cell_at(position) else {
            return;
        };
        let Some(render) = self.terminal.state.render.as_ref() else {
            return;
        };
        if render.frame.modes.mouse_reporting {
            // An application that asked for the mouse owns the tap, exactly as
            // it does on the desktop where Shift is what takes it back.
            if let Some(input) = terminal_mouse_tap(point, render.frame.modes.sgr_mouse) {
                self.send_terminal_value(input, false, cx);
            }
            return;
        }
        // Nothing is claiming taps, so a link under the finger is the reader's.
        let link = render
            .frame
            .cells
            .iter()
            .find(|cell| cell.row == point.row && cell.column == point.column)
            .and_then(|cell| cell.hyperlink.clone());
        if let Some(link) = link
            && crate::platform::open_external_url(&link)
        {
            self.hint(
                Tone::Info,
                locale::text(
                    "Opened in the browser",
                    "已在浏览器中打开",
                    "已在瀏覽器中開啟",
                ),
            );
        }
    }

    /// Focuses the terminal's key sink and raises the software keyboard.
    fn show_terminal_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sink) = self.terminal_input.clone() {
            sink.update(cx, |sink, cx| sink.focus(window, cx));
        }
        crate::platform::show_keyboard();
        cx.notify();
    }

    fn send_terminal_key(&mut self, key: TerminalKey, cx: &mut Context<Self>) {
        self.send_terminal_value(
            TerminalInput::Key(key, TerminalKeyModifiers::default()),
            false,
            cx,
        );
    }

    /// Scroll-back for the terminal grid. Wheel deltas move the shared render
    /// model's viewport; a downward scroll past the history stops at the live
    /// bottom, mirroring the desktop surface.
    fn scroll_terminal(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (lines, columns) = match event.delta {
            ScrollDelta::Lines(point) => (point.y * 3.0, point.x * 3.0),
            ScrollDelta::Pixels(point) => (
                f32::from(point.y) / TERMINAL_CELL_HEIGHT,
                f32::from(point.x) / TERMINAL_CELL_WIDTH,
            ),
        };
        if lines == 0.0 && columns == 0.0 {
            return;
        }
        // An application that turned on mouse reporting (vim, less, htop,
        // tmux) owns the wheel: the gesture has to reach the PTY as a wheel
        // event instead of moving this client's own viewport, which the
        // application would immediately repaint away.
        let mouse_reporting = self
            .terminal
            .state
            .render
            .as_ref()
            .is_some_and(|render| render.frame.modes.mouse_reporting);
        if mouse_reporting && lines == 0.0 {
            // A pan locks to one axis, so a gesture with no vertical distance
            // is a sideways drag. The grid has no use for that axis — it never
            // scrolls sideways — so it is spent on the application's own drag.
            // The vertical axis keeps its wheel, because that is what scrolls
            // a pager or a copy-mode buffer and the phone has no second
            // gesture to spare.
            self.drag_terminal_mouse(event, cx);
            return;
        }
        if lines == 0.0 {
            return;
        }
        cx.stop_propagation();
        if mouse_reporting {
            let (row, column) = self
                .terminal
                .state
                .render
                .as_ref()
                .map(|render| {
                    (
                        render.frame.cursor.map(|cursor| cursor.row).unwrap_or(0),
                        render.frame.cursor.map(|cursor| cursor.column).unwrap_or(0),
                    )
                })
                .unwrap_or((0, 0));
            let button = if lines > 0.0 { 65 } else { 64 };
            let sequence = format!("\u{1b}[<{};{};{}M", button, column + 1, row + 1);
            self.send_terminal_value(TerminalInput::Text(sequence), false, cx);
            return;
        }
        let Some(render) = self.terminal.state.render.as_mut() else {
            return;
        };
        if render.frame.display_offset == 0 && lines < 0.0 {
            return;
        }
        render.scroll(lines as i32);
        self.terminal_scrolled_back = render.frame.display_offset != 0;
        cx.notify();
    }

    /// Forwards a sideways pan to the application as the drag it is: a press on
    /// the first step, a move whenever the finger reaches another cell, and a
    /// release when it lifts.
    ///
    /// A pan reports the position it started at together with the distance
    /// moved since the previous step, never the finger's live position, so the
    /// cell under the finger is the start point plus the distance travelled.
    fn drag_terminal_mouse(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let ScrollDelta::Pixels(delta) = event.delta else {
            // Touch pans are always reported in pixels; a line-based delta
            // comes from a wheel, which has no drag to give.
            return;
        };
        let Some(sgr) = self
            .terminal
            .state
            .render
            .as_ref()
            .map(|render| render.frame.modes.sgr_mouse)
        else {
            return;
        };
        let origin = event.position;
        if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            let Some(drag) = self.terminal_mouse_drag.take() else {
                return;
            };
            // The fling that follows a release repeats this same origin, so
            // remembering it is what keeps momentum from reading as a second
            // drag. A new gesture starts somewhere else.
            self.terminal_mouse_drag_released = Some(origin);
            if let Some(input) = terminal_mouse_report(TerminalMouseReport::Release, drag.last, sgr)
            {
                self.send_terminal_value(input, false, cx);
            }
            return;
        }
        if self.terminal_mouse_drag_released == Some(origin) {
            // Momentum from the gesture that has already been released.
            return;
        }
        // A drag starts where the finger first went down, and only if that was
        // over the grid: a press on the key row or on the copy bar is not the
        // application's.
        let starting = match self.terminal_mouse_drag {
            Some(drag) if drag.origin == origin => None,
            _ => {
                let Some(bounds) = self.terminal_grid_bounds else {
                    return;
                };
                if !bounds.contains(&origin) {
                    return;
                }
                let Some(point) = self.terminal_cell_at(origin) else {
                    return;
                };
                Some(point)
            }
        };
        if let Some(point) = starting {
            self.terminal_mouse_drag = Some(TerminalMouseDrag {
                origin,
                // The step that starts the drag carries the distance the
                // finger travelled before the pan was recognized, so the first
                // move lands where the finger actually is.
                travelled: f32::from(delta.x),
                last: point,
            });
            self.terminal_mouse_drag_released = None;
            if let Some(input) = terminal_mouse_report(TerminalMouseReport::Press, point, sgr) {
                self.send_terminal_value(input, false, cx);
            }
            cx.stop_propagation();
            return;
        }
        let Some(drag) = self.terminal_mouse_drag else {
            return;
        };
        let travelled = drag.travelled + f32::from(delta.x);
        let point = Point::new(origin.x + px(travelled), origin.y);
        let Some(point) = self.terminal_cell_at(point) else {
            return;
        };
        if point == drag.last {
            return;
        }
        self.terminal_mouse_drag = Some(TerminalMouseDrag {
            travelled,
            last: point,
            ..drag
        });
        if let Some(input) = terminal_mouse_report(TerminalMouseReport::Move, point, sgr) {
            self.send_terminal_value(input, false, cx);
        }
        cx.stop_propagation();
    }

    fn scroll_terminal_to_bottom(&mut self, cx: &mut Context<Self>) {
        if let Some(render) = self.terminal.state.render.as_mut()
            && render.frame.display_offset != 0
        {
            render.scroll_to_bottom();
            self.terminal_scrolled_back = false;
            cx.notify();
        }
    }

    /// The cell a window point falls in, using the geometry the grid was last
    /// painted at. A point past an edge clamps to the nearest cell, so a finger
    /// that drags off the grid keeps extending the selection instead of losing
    /// it.
    fn terminal_cell_at(&self, position: Point<Pixels>) -> Option<TerminalGridPoint> {
        let bounds = self.terminal_grid_bounds?;
        let render = self.terminal.state.render.as_ref()?;
        terminal_cell_at_point(bounds, render.frame.rows, render.frame.columns, position)
    }

    /// The run of non-blank cells around `point` on its own row. A long press
    /// selects the whole run, which is how Android selects a word.
    fn terminal_word_at(&self, point: TerminalGridPoint) -> (TerminalGridPoint, TerminalGridPoint) {
        let Some(render) = self.terminal.state.render.as_ref() else {
            return (point, point);
        };
        let mut blanks = vec![true; usize::from(render.frame.columns)];
        for cell in &render.frame.cells {
            if cell.row != point.row {
                continue;
            }
            if let Some(blank) = blanks.get_mut(usize::from(cell.column)) {
                *blank = cell.text.trim().is_empty();
            }
        }
        let (start, end) = terminal_word_range(&blanks, point.column);
        (
            TerminalGridPoint {
                row: point.row,
                column: start,
            },
            TerminalGridPoint {
                row: point.row,
                column: end,
            },
        )
    }

    /// The two handles a selection can be adjusted by, so they can be painted
    /// and hit-tested against the same geometry.
    fn terminal_selection_handles(&self) -> Option<(Point<Pixels>, Point<Pixels>)> {
        let bounds = self.terminal_grid_bounds?;
        let render = self.terminal.state.render.as_ref()?;
        let (start, end) = self.terminal_selection?.ends();
        Some(terminal_handle_points(
            bounds,
            render.frame.rows,
            start,
            end,
        ))
    }

    /// Moves the end of the selection a touch took hold of.
    fn on_terminal_handle_drag(&mut self, event: &TouchDragEvent, cx: &mut Context<Self>) {
        match event.phase {
            TouchPhase::Started => {
                // The bar sits over the top rows while a selection is up, and
                // a press there is meant for the bar even when it lands on a
                // handle the bar happens to be covering.
                if self.terminal_grid_bounds.is_some_and(|bounds| {
                    event.position.y < bounds.top() + px(TERMINAL_SELECTION_BAR_BAND)
                }) {
                    return;
                }
                let Some((start_point, end_point)) = self.terminal_selection_handles() else {
                    return;
                };
                let Some(handle) = terminal_handle_at_point(
                    start_point,
                    end_point,
                    event.position,
                    TERMINAL_HANDLE_TOUCH_RADIUS,
                ) else {
                    return;
                };
                // Claiming the touch here is what makes the drag a control
                // drag rather than a pan: nothing else in the grid has to know
                // a handle was touched, and the surface never sees a tap that
                // would dismiss the selection.
                self.terminal_selection_handle = Some(handle);
                cx.stop_propagation();
            }
            TouchPhase::Moved => {
                let Some(handle) = self.terminal_selection_handle else {
                    return;
                };
                let Some(selection) = self.terminal_selection else {
                    return;
                };
                let Some(point) = self.terminal_cell_at(event.position) else {
                    return;
                };
                let (start, end) = selection.ends();
                let (anchor, head) = match handle {
                    TerminalSelectionHandle::Start => (point, end),
                    TerminalSelectionHandle::End => (start, point),
                };
                if (anchor, head) != (selection.anchor, selection.head) {
                    self.select_terminal_cells(anchor, head, cx);
                }
                cx.stop_propagation();
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.terminal_selection_handle.take().is_some() {
                    cx.stop_propagation();
                }
            }
        }
    }

    /// Drops everything that pointed into the previous terminal's grid: the
    /// selection, its text, the find query's matches, and the geometry they
    /// were resolved against.
    fn reset_terminal_text_state(&mut self) {
        self.terminal_grid_bounds = None;
        self.terminal_grid_rows = None;
        self.terminal_selection = None;
        self.terminal_selection_text = None;
        self.terminal_selection_handle = None;
        self.terminal_mouse_drag = None;
        self.terminal_mouse_drag_released = None;
        self.terminal_search_matches.clear();
        self.terminal_active_search_match = 0;
    }

    /// Points the shared render model's selection at `anchor..head` and keeps
    /// the text it produced. The model rebuilds its frame from the selection,
    /// so the next paint carries the per-cell highlight.
    fn select_terminal_cells(
        &mut self,
        anchor: TerminalGridPoint,
        head: TerminalGridPoint,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .terminal
            .state
            .render
            .as_mut()
            .and_then(|render| render.select_text(anchor, head).ok());
        self.terminal_selection_text = text;
        self.terminal_selection = Some(TerminalSelection { anchor, head });
        cx.notify();
    }

    /// Drops the touch selection and the highlight it painted.
    fn dismiss_terminal_selection(&mut self, cx: &mut Context<Self>) {
        self.terminal_selection_handle = None;
        if self.terminal_selection.take().is_none() {
            return;
        }
        self.terminal_selection_text = None;
        if let Some(render) = self.terminal.state.render.as_mut() {
            render.clear_selection();
        }
        cx.notify();
    }

    /// A long press starts a selection; the recognizer keeps reporting the same
    /// touch through `Moved`, which is what turns the press into a drag over
    /// the grid. A plain pan never reaches this: it stays a scroll gesture.
    fn on_terminal_long_press(&mut self, event: &LongPressEvent, cx: &mut Context<Self>) {
        match event.phase {
            TouchPhase::Started => {
                // The bar sits over the top rows while a selection is up; a
                // press there is meant for the bar.
                if self.terminal_selection.is_some()
                    && self.terminal_grid_bounds.is_some_and(|bounds| {
                        event.position.y < bounds.top() + px(TERMINAL_SELECTION_BAR_BAND)
                    })
                {
                    return;
                }
                let Some(point) = self.terminal_cell_at(event.position) else {
                    return;
                };
                let (start, end) = self.terminal_word_at(point);
                self.select_terminal_cells(start, end, cx);
                cx.stop_propagation();
            }
            TouchPhase::Moved => {
                let Some(selection) = self.terminal_selection else {
                    return;
                };
                let Some(point) = self.terminal_cell_at(event.position) else {
                    return;
                };
                if point != selection.head {
                    self.select_terminal_cells(selection.anchor, point, cx);
                }
                cx.stop_propagation();
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.terminal_selection.is_some() {
                    cx.stop_propagation();
                }
            }
        }
    }

    fn copy_terminal_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal_selection_text.take()
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.hint(Tone::Success, locale::text("Copied", "已复制", "已複製"));
        }
        self.dismiss_terminal_selection(cx);
    }

    fn toggle_terminal_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_search_open = !self.terminal_search_open;
        self.terminal_selection = None;
        self.terminal_selection_text = None;
        if self.terminal_search_open {
            if let Some(input) = self.terminal_search_input.clone() {
                // The field opens empty every time: the previous query belonged
                // to the grid the reader was looking at before.
                input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
            }
            crate::platform::show_keyboard();
            self.refresh_terminal_search(cx);
        } else {
            self.terminal_search_matches.clear();
            self.terminal_active_search_match = 0;
            if let Some(render) = self.terminal.state.render.as_mut() {
                render.clear_selection();
            }
        }
        cx.notify();
    }

    /// Re-runs the query over the visible grid. The emulator only searches what
    /// is on screen, which is the same scope the desktop surface gives it.
    fn refresh_terminal_search(&mut self, cx: &mut Context<Self>) {
        let query = input_value(&self.terminal_search_input, cx).to_string();
        let query = query.trim();
        self.terminal_search_matches = self
            .terminal
            .state
            .render
            .as_ref()
            .filter(|_| !query.is_empty())
            .map(|render| render.find_visible(query, false))
            .unwrap_or_default();
        self.terminal_active_search_match = self
            .terminal_active_search_match
            .min(self.terminal_search_matches.len().saturating_sub(1));
        if self.terminal_search_matches.is_empty() {
            if let Some(render) = self.terminal.state.render.as_mut() {
                render.clear_selection();
            }
        } else {
            self.apply_terminal_search_match(cx);
            return;
        }
        cx.notify();
    }

    fn select_next_terminal_search_match(&mut self, cx: &mut Context<Self>) {
        if self.terminal_search_matches.is_empty() {
            return;
        }
        self.terminal_active_search_match =
            (self.terminal_active_search_match + 1) % self.terminal_search_matches.len();
        self.apply_terminal_search_match(cx);
    }

    fn apply_terminal_search_match(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self
            .terminal_search_matches
            .get(self.terminal_active_search_match)
            .copied()
        else {
            return;
        };
        if let Some(render) = self.terminal.state.render.as_mut() {
            let _ = render.select_text(found.start, found.end);
        }
        cx.notify();
    }

    fn send_terminal_value(
        &mut self,
        input: TerminalInput,
        clear_input: bool,
        cx: &mut Context<Self>,
    ) {
        let operation = match self.terminal.begin_send_input(input) {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                cx.notify();
                return;
            }
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_input(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_input(&operation, outcome);
                this.busy = false;
                if let Some(error) = failure {
                    this.hint_failure(FailureSource::Terminal, Some(error));
                } else if clear_input {
                    this.terminal_clear_pending = true;
                }
                if let Some(terminal_id) = this
                    .terminal
                    .state
                    .active_session
                    .as_ref()
                    .map(|session| session.id.clone())
                {
                    this.start_terminal_poll(terminal_id, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Asks before closing the terminal a tab's own close control names.
    ///
    /// The strip draws one close control per tab, so the tab the reader
    /// pressed is attached first: the confirmation that follows asks about the
    /// terminal they pointed at, and the close itself runs on the active
    /// session the way every other terminal action does.
    fn request_close_terminal_id(&mut self, terminal_id: TerminalId, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.attach_terminal(terminal_id, cx);
        self.terminal_close_confirmation = self
            .terminal
            .state
            .active_session
            .as_ref()
            .map(|session| session.id.clone());
        cx.notify();
    }

    fn cancel_close_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal_close_confirmation = None;
        cx.notify();
    }

    fn confirm_close_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(confirmed_id) = self.terminal_close_confirmation.take() else {
            return;
        };
        if !self
            .terminal
            .state
            .active_session
            .as_ref()
            .is_some_and(|session| session.id == confirmed_id)
        {
            self.fail(BackendError::conflict(
                "mobile_terminal_close_target_changed",
                locale::text(
                    "The selected terminal changed before close confirmation.",
                    "确认关闭前，所选终端已发生变化。",
                    "確認關閉前，所選終端機已變更。",
                ),
            ));
            cx.notify();
            return;
        }
        let operation = match self.terminal.begin_close() {
            Ok(operation) => operation,
            Err(error) => {
                self.fail(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_close(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failure = outcome.as_ref().err().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_close(&operation, outcome);
                this.busy = false;
                this.terminal.state.render = None;
                this.terminal_render_sequence = 0;
                // The tab that just went may have been the last one. An empty
                // strip is not a state a reader can leave the terminal on, so
                // the next terminal opens in its place — the same answer the
                // surface gives when it is entered with nothing running.
                this.terminal_auto_create = true;
                match failure {
                    None => {
                        this.stop_terminal_poll();
                        this.open_terminal_when_idle(cx);
                    }
                    Some(error) => this.hint_failure(FailureSource::Terminal, Some(error)),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// The Files surface: the tree, or the file the tree opened.
    ///
    /// Nothing on this surface writes. The rail browses the workspace and the
    /// desktop is what edits it, so there is no unsaved buffer to guard, no
    /// prompt over the list, and no action bar under the file.
    fn render_files(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let body = if self.file_screen_open {
            self.render_file_screen(window, cx)
        } else {
            self.render_file_tree(cx)
        };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().relative().child(body))
            .into_any_element()
    }

    /// The file's own screen: a flat header that names the file and carries its
    /// two navigation actions, the buffer filling everything under it, and one
    /// A viewer with a back control at the top and the file filling everything
    /// under it.
    ///
    /// The screen is drawn as a page rather than as a card with a card inside
    /// it. A reader who opened a file came for the file, and the rail does not
    /// write to the workspace, so there is no save bar under the buffer and no
    /// framed control beside it: every rounded box here is a line of source
    /// they cannot see.
    fn render_file_screen(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.files.state.view();
        // The buffer's own path, not the row that happens to be selected: while
        // a read is in flight those differ, and the header would announce the
        // new file over the old file's content.
        let path = self
            .file_editor_path
            .clone()
            .or_else(|| view.selected_path.clone())
            .unwrap_or_default();
        let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let icon = file_icon_descriptor(&file_name, FileEntryKind::File);
        // The file's own name is already the header's headline; the directory
        // under it is what the tree ellipsises away, and it is dropped for a
        // file that sits at the root rather than repeated as the same word.
        let parent = path.rsplit_once('/').map(|(parent, _)| parent.to_string());
        // Markdown is shown as Markdown and an image as itself, because a phone
        // reads those and the source is not what they are for. Every other kind
        // is text, and text is shown as text.
        //
        // There is no switch between the two faces. The phone's buffer is
        // read-only — the desktop is what writes the workspace — so the source
        // face of a Markdown file was an edit mode with the editing taken out of
        // it: a control that only ever changed how a document nobody could
        // change was presented.
        //
        // Find and go-to-line are the exception, and they are not a switch: a
        // rendered document has no lines to step through, so either control
        // shows the source for as long as it is open and the render comes back
        // when it is dismissed. Without that, giving Markdown up to its render
        // would have taken its search away with it.
        let preview_kind = file_preview_kind_for_path(&path);
        let showing_preview = match preview_kind {
            FilePreviewKind::Markdown => !(self.file_find_open || self.file_goto_open),
            FilePreviewKind::Image => self.file_image.is_some(),
            _ => false,
        };
        // The plate a code buffer is read on is the appearance's authored
        // `editor.background`, which is what the desktop editor paints and what
        // the syntax palette beside it was tuned against. The kit's own editor
        // would paint it too, but it would round the corners of a surface that
        // is meant to run edge to edge.
        let editor_bg = cx
            .theme()
            .highlight_theme
            .style
            .editor_background
            .unwrap_or_else(theme::bg_card);
        let body = if showing_preview {
            match preview_kind {
                FilePreviewKind::Markdown => {
                    match self
                        .file_markdown_view
                        .as_ref()
                        .map(|(_, view)| view.clone())
                    {
                        Some(entity) => div()
                            .id("file-screen-markdown")
                            .flex_1()
                            .min_h_0()
                            .bg(theme::bg_card())
                            .overflow_y_scroll()
                            .p_3()
                            .child(entity)
                            .into_any_element(),
                        None => div().flex_1().into_any_element(),
                    }
                }
                FilePreviewKind::Image => match self.file_image.clone() {
                    Some(image) => div()
                        .id("file-screen-image")
                        .flex_1()
                        .min_h_0()
                        .bg(theme::bg_card())
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .p_2()
                        .child(
                            img(image)
                                .max_w_full()
                                .max_h_full()
                                .object_fit(ObjectFit::Contain),
                        )
                        .into_any_element(),
                    None => div().flex_1().into_any_element(),
                },
                _ => div().flex_1().into_any_element(),
            }
        } else {
            div()
                .id("file-screen-buffer")
                .flex_1()
                .min_h_0()
                .bg(editor_bg)
                .overflow_hidden()
                // A code buffer in the interface font reads as prose; the
                // desktop editor is monospace and so is this one.
                .font_family(theme::MONO_FONT_FAMILY)
                .text_size(theme::code_font_size())
                // No tap-to-raise-keyboard: opening a file is a request to read
                // it, and an IME over the source is not what a reader scrolling
                // a file asked for.
                .child(file_editor_element(self.file_editor_input.as_ref()))
                .into_any_element()
        };

        div()
            .size_full()
            // The go-to-line sheet is drawn here, by the screen whose header
            // opens it. It used to be drawn at the foot of the Git surface's
            // tree, so tapping the glyph set the sheet's flag and painted
            // nothing until the reader left Files — at which point it appeared
            // over a surface that had never asked for it.
            .relative()
            .flex()
            .flex_col()
            .bg(theme::workbench_panel_bg())
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(FILE_HEADER_HEIGHT))
                    .pl(px(TOOLBAR_GAP))
                    .pr(px(theme::SPACING_XS))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap(px(TOOLBAR_GAP))
                    .child(
                        div()
                            .id("file-screen-back")
                            .size(px(WORKBENCH_CONTROL_SIZE))
                            .flex_shrink_0()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::close_file_screen))
                            .child(
                                svg()
                                    .path("icons/chevron-left.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    )
                    .child(mobile_file_tree_icon(icon, false, false))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(theme::font_body())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(file_name),
                            )
                            .when_some(parent, |stack, parent| {
                                stack.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::text_muted())
                                        .child(parent),
                                )
                            }),
                    )
                    // Find and go-to-line are the two things a long file
                    // needs on a phone, where scrolling to a place by hand is
                    // the only alternative. Every kind that has text offers
                    // them, Markdown included: on a rendered Markdown file they
                    // bring the source forward while they are open.
                    .when(preview_kind != FilePreviewKind::Image, |header| {
                        header
                            .child(
                                icon_button("file-find", "icons/search.svg", "Find in file")
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            if this.file_find_open {
                                                this.close_file_find(window, cx);
                                            } else {
                                                this.open_file_find(window, cx);
                                            }
                                        }),
                                    ),
                            )
                            .child(
                                icon_button("file-goto-line", "icons/hash.svg", "Go to line")
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.open_file_goto(window, cx)
                                        }),
                                    ),
                            )
                    }),
            )
            .when(self.file_find_open && !showing_preview, |screen| {
                screen.child(self.render_file_find_bar(cx))
            })
            .child(body)
            .when_some(
                self.sheet.presentation(
                    SheetKey::FileGoto,
                    self.file_goto_open,
                    crate::bottom_sheet::sheet_size(window),
                ),
                |screen, presentation| screen.child(self.render_file_goto_prompt(presentation, cx)),
            )
            .into_any_element()
    }

    /// The editor's find bar: the query, how many matches it has, and the two
    /// steps through them.
    ///
    /// The match count comes from the editor's own search session, so the
    /// number on screen is the number of highlighted matches rather than a
    /// second count that could disagree with them.
    fn render_file_find_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (matched, current) = self
            .file_editor_input
            .as_ref()
            .map(|editor| {
                let session = editor.read(cx).search_session();
                (session.matcher.len(), session.matcher.current_match_index())
            })
            .unwrap_or((0, 0));
        let has_query = !self.file_find_query.trim().is_empty();
        div()
            .id("file-find-bar")
            .debug_selector(|| "file-find-bar".to_string())
            .flex_shrink_0()
            .h(px(WORKBENCH_CONTEXT_HEIGHT))
            .px(px(theme::SPACING_XS))
            .border_b_1()
            .border_color(theme::border_subtle())
            .bg(theme::bg_card())
            .flex()
            .items_center()
            .gap(px(TOOLBAR_GAP))
            .child(
                div()
                    .h(px(WORKBENCH_CONTROL_SIZE))
                    .min_w_0()
                    .flex_1()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::workbench_panel_bg())
                    .px(px(theme::SPACING_SM))
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .child(
                        svg()
                            .path("icons/search.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_muted()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .on_mouse_down(MouseButton::Left, crate::app::resume_keyboard_on_tap)
                            .child(input_element(self.file_find_input.as_ref())),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(theme::font_micro())
                    .text_color(if has_query && matched == 0 {
                        theme::accent_red()
                    } else {
                        theme::text_muted()
                    })
                    .child(if has_query {
                        format!("{}/{}", current.min(matched), matched)
                    } else {
                        String::new()
                    }),
            )
            .child(
                icon_button(
                    "file-find-previous",
                    "icons/chevron-down.svg",
                    "Previous match",
                )
                .when(has_query && matched > 0, |button| {
                    button.on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.step_file_find_match(false, cx)),
                    )
                })
                .when(!has_query || matched == 0, |button| button.opacity(0.4)),
            )
            .child(
                icon_button("file-find-next", "icons/chevron-down.svg", "Next match")
                    .when(has_query && matched > 0, |button| {
                        button.on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.step_file_find_match(true, cx)),
                        )
                    })
                    .when(!has_query || matched == 0, |button| button.opacity(0.4)),
            )
            .child(
                icon_button("file-find-close", "icons/x.svg", "Close find").on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_file_find(window, cx)),
                ),
            )
            .into_any_element()
    }

    /// The go-to-line prompt: a number, and a way to confirm it.
    fn render_file_goto_prompt(
        &self,
        presentation: SheetPresentation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let body = div()
            .min_h_0()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(theme::font_title())
                    .text_color(theme::text_primary())
                    .child(locale::text("Go to line", "跳转到行", "跳轉到行")),
            )
            .child(
                div()
                    .h(px(theme::TOUCH_TARGET))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::workbench_panel_bg())
                    .px_2()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, crate::app::resume_keyboard_on_tap)
                    .child(input_element(self.file_goto_input.as_ref())),
            )
            .child(
                action_button(
                    "file-goto-confirm",
                    locale::common("Go"),
                    "icons/arrow-to-top.svg",
                )
                .on_mouse_up(MouseButton::Left, cx.listener(Self::confirm_file_goto)),
            );
        bottom_sheet_shell("file-goto-prompt", presentation)
            .on_dismiss(cx.listener(|this, _, _, cx| this.tap_sheet_scrim(SheetKey::FileGoto, cx)))
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::FileGoto, event, window, cx)
            }))
            .content(body)
            .into_any_element()
    }

    /// The branch list the history filter picks from.
    ///
    /// A sheet rather than a select: a repository can carry dozens of branches
    /// and a phone list scrolls, where a popup would have to be a full screen
    /// anyway. The current branch is marked, and "All branches" is the way back
    /// to the whole repository.
    fn render_git_branch_sheet(
        &self,
        presentation: SheetPresentation,
        body_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self.git.state.model.history_filter.ref_name.clone();
        let loading = self.git_branches_loading && self.git_branches.is_none();
        let mut rows: Vec<(Option<String>, String, Option<String>)> =
            vec![(None, locale::common("All branches").to_string(), None)];
        for branch in self.git_branches.iter().flatten() {
            let mut detail = Vec::new();
            if branch.current {
                detail.push(locale::common("Current").to_string());
            }
            if branch.ahead > 0 {
                detail.push(format!("↑{}", branch.ahead));
            }
            if branch.behind > 0 {
                detail.push(format!("↓{}", branch.behind));
            }
            if branch.detached {
                detail.push(locale::common("Detached").to_string());
            }
            rows.push((
                Some(branch.name.clone()),
                branch.name.clone(),
                (!detail.is_empty()).then(|| detail.join(" · ")),
            ));
        }
        let sheet = div()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(theme::font_title())
                            .text_color(theme::text_primary())
                            .child(locale::text("History branch", "历史分支", "歷史分支")),
                    )
                    .child(
                        div()
                            .id("git-branch-sheet-close")
                            .size(px(theme::TOUCH_TARGET))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.close_git_branch_sheet(cx)),
                            )
                            .child(
                                svg()
                                    .path("icons/x.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    ),
            )
            .child(
                div()
                    .id("git-branch-list")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&body_scroll)
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .flex()
                    .flex_col()
                    .p_2()
                    .when(loading, |list| {
                        list.child(
                            div()
                                .p_3()
                                .text_size(theme::font_body())
                                .text_color(theme::text_muted())
                                .child(locale::common("Loading")),
                        )
                    })
                    .children(rows.into_iter().enumerate().map(
                        |(index, (value, label, detail))| {
                            let chosen = value == selected;
                            let target = value.clone();
                            div()
                                .id(("git-branch-row", index))
                                .w_full()
                                .min_h(px(theme::TOUCH_TARGET))
                                .px_3()
                                .py_1()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.set_git_history_branch(target.clone(), cx)
                                    }),
                                )
                                .child(
                                    svg()
                                        .path("icons/git-branch.svg")
                                        .size(px(theme::ICON_SM))
                                        .flex_shrink_0()
                                        .text_color(if chosen {
                                            theme::primary()
                                        } else {
                                            theme::text_muted()
                                        }),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(theme::font_body())
                                        .text_color(if chosen {
                                            theme::primary()
                                        } else {
                                            theme::text_primary()
                                        })
                                        .child(label),
                                )
                                .when_some(detail, |row, detail| {
                                    row.child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(theme::font_micro())
                                            .text_color(theme::text_muted())
                                            .child(detail),
                                    )
                                })
                                .when(chosen, |row| {
                                    row.child(
                                        svg()
                                            .path("icons/check.svg")
                                            .size(px(theme::ICON_SM))
                                            .flex_shrink_0()
                                            .text_color(theme::primary()),
                                    )
                                })
                        },
                    )),
            );
        bottom_sheet_shell("mobile-git-branch-sheet", presentation)
            .on_dismiss(
                cx.listener(|this, _, _, cx| this.tap_sheet_scrim(SheetKey::GitBranches, cx)),
            )
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::GitBranches, event, window, cx)
            }))
            .content(sheet)
            .into_any_element()
    }

    /// The worktree lifecycle card: what this worktree is, and what can be
    /// done with it.
    ///
    /// Only drawn for a managed worktree. A phone could start a session in one
    /// and never finish with it, which made the worktree a one-way door.
    fn render_worktree_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let worktree = self.managed_worktree()?;
        let readiness = self
            .worktree_lifecycle
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .readiness
                    .iter()
                    .find(|record| record.workspace_id == self.workspace_id)
            })
            .map(|record| record.state)
            .unwrap_or_default();
        let branch = worktree
            .branch
            .clone()
            .unwrap_or_else(|| locale::text("detached", "游离分支", "游離分支").to_string());
        let ready = readiness == GitWorktreeReadinessState::ReadyToMerge;
        let operation = self.active_worktree_operation();
        let conflicts = operation
            .map(|operation| {
                operation
                    .detail
                    .conflicts
                    .iter()
                    .filter(|conflict| !conflict.resolved)
                    .count()
            })
            .unwrap_or(0);
        let merging = operation.is_some();
        let can_mutate = self
            .git
            .capabilities()
            .supports(BackendOperation::GitWorktreeLifecycleMutate);
        Some(
            div()
                .id("mobile-worktree-card")
                .debug_selector(|| "mobile-worktree-card".to_string())
                .flex_shrink_0()
                .mx_2()
                .mt_2()
                .rounded(px(theme::RADIUS_CARD))
                .border_1()
                .border_color(theme::border_default())
                .bg(theme::bg_card())
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path("icons/git-branch.svg")
                                .size(px(theme::ICON_SM))
                                .flex_shrink_0()
                                .text_color(theme::accent_purple()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(theme::font_caption())
                                .text_color(theme::text_primary())
                                .child(branch),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(theme::font_micro())
                                .text_color(if ready {
                                    theme::accent_green()
                                } else {
                                    theme::text_muted()
                                })
                                .child(worktree_readiness_label(readiness)),
                        ),
                )
                .when(conflicts > 0, |card| {
                    card.child(
                        div()
                            .text_size(theme::font_micro())
                            .text_color(theme::accent_yellow())
                            .child(
                                locale::text(
                                    "{} conflicted files",
                                    "{} 个文件冲突",
                                    "{} 個檔案衝突",
                                )
                                .replace("{}", &conflicts.to_string()),
                            ),
                    )
                })
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .flex_wrap()
                        .gap(px(theme::SPACING_XS))
                        .when(!merging && can_mutate, |actions| {
                            actions
                                .child(
                                    chip_action(
                                        "worktree-readiness",
                                        if ready {
                                            locale::text("Not ready", "标记未就绪", "標記未就緒")
                                        } else {
                                            locale::text("Mark ready", "标记就绪", "標記就緒")
                                        },
                                        !self.busy,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.set_worktree_readiness(!ready, cx)
                                        }),
                                    ),
                                )
                                .child(
                                    chip_action(
                                        "worktree-merge",
                                        locale::text("Merge back", "合并回去", "合併回去"),
                                        !self.busy,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.plan_worktree_merge(cx)),
                                    ),
                                )
                                .child(
                                    chip_action(
                                        "worktree-discard",
                                        locale::text("Discard", "丢弃", "丟棄"),
                                        !self.busy,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.plan_worktree_discard(cx)
                                        }),
                                    ),
                                )
                        })
                        // A merge in flight is finished or abandoned here; a
                        // phone cannot edit a three-way merge, so the files are
                        // fixed in the editor and marked resolved from here.
                        .when(merging && can_mutate, |actions| {
                            actions
                                .when(conflicts > 0, |actions| {
                                    actions.child(
                                        chip_action(
                                            "worktree-stage-conflicts",
                                            locale::text(
                                                "Mark resolved",
                                                "标记已解决",
                                                "標記已解決",
                                            ),
                                            !self.busy,
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.stage_worktree_conflicts(cx)
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    chip_action(
                                        "worktree-continue",
                                        locale::text("Continue", "继续", "繼續"),
                                        !self.busy,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.continue_worktree_merge(cx)
                                        }),
                                    ),
                                )
                                .child(
                                    chip_action(
                                        "worktree-abort",
                                        locale::text("Abort merge", "中止合并", "中止合併"),
                                        !self.busy,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.abort_worktree_merge(cx)),
                                    ),
                                )
                        }),
                )
                .into_any_element(),
        )
    }

    /// The plan or preflight a lifecycle action is waiting on.
    fn render_worktree_confirmation(
        &self,
        presentation: SheetPresentation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(confirmation) = self.worktree_confirmation.as_ref() else {
            return div().into_any_element();
        };
        let (title, detail, confirm_label) = match confirmation {
            WorktreeConfirmation::Merge(plan) => (
                locale::text(
                    "Merge this worktree back?",
                    "把这个 Worktree 合并回去？",
                    "把這個 Worktree 合併回去？",
                ),
                format!(
                    "{} {} · +{} −{}",
                    plan.source_branch,
                    locale::text("into", "合并到", "合併到"),
                    plan.summary.additions,
                    plan.summary.deletions
                ),
                locale::common("Merge"),
            ),
            WorktreeConfirmation::Discard(preflight) => (
                locale::text(
                    "Discard this worktree?",
                    "丢弃这个 Worktree？",
                    "丟棄這個 Worktree？",
                ),
                if preflight.risks.is_empty() {
                    locale::text(
                        "The worktree is removed; its branch is kept.",
                        "Worktree 会被移除，分支保留。",
                        "Worktree 會被移除，分支保留。",
                    )
                    .to_string()
                } else {
                    format!(
                        "{} {}",
                        preflight.risks.len(),
                        locale::text("risks reported", "项风险", "項風險")
                    )
                },
                locale::common("Discard"),
            ),
        };
        let merge = matches!(confirmation, WorktreeConfirmation::Merge(_));
        let body = div()
            .min_h_0()
            .p(px(theme::SPACING_LG))
            .flex()
            .flex_col()
            .gap(px(theme::SPACING_SM))
            .child(
                div()
                    .text_size(theme::font_heading())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(title),
            )
            .child(
                div()
                    .text_size(theme::font_caption())
                    .text_color(theme::text_muted())
                    .child(detail),
            )
            .child(
                div()
                    .mt(px(theme::SPACING_SM))
                    .flex()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        div()
                            .id("worktree-confirm-cancel")
                            .flex_1()
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .text_size(theme::font_body())
                            .text_color(theme::text_secondary())
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cancel_worktree_confirmation(cx)),
                            )
                            .child(crate::app::sheet_action_content(
                                "icons/x.svg",
                                locale::common("Cancel"),
                                theme::text_secondary(),
                            )),
                    )
                    .child(
                        div()
                            .id("worktree-confirm-action")
                            .flex_1()
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(if merge {
                                theme::primary()
                            } else {
                                theme::accent_red()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.opacity(0.85))
                            .text_size(theme::font_body())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if merge {
                                theme::primary_foreground()
                            } else {
                                theme::bg_primary()
                            })
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if merge {
                                        this.confirm_worktree_merge(cx)
                                    } else {
                                        this.confirm_worktree_discard(cx)
                                    }
                                }),
                            )
                            .child(crate::app::sheet_action_content(
                                if merge {
                                    "icons/git-branch.svg"
                                } else {
                                    "icons/trash-2.svg"
                                },
                                confirm_label,
                                if merge {
                                    theme::primary_foreground()
                                } else {
                                    theme::bg_primary()
                                },
                            )),
                    ),
            );
        bottom_sheet_shell("mobile-worktree-confirmation", presentation)
            .on_dismiss(cx.listener(|this, _, _, cx| {
                this.tap_sheet_scrim(SheetKey::WorktreeConfirmation, cx)
            }))
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::WorktreeConfirmation, event, window, cx)
            }))
            .content(body)
            .into_any_element()
    }

    /// The author list the history filter picks from.
    fn render_git_author_sheet(
        &self,
        presentation: SheetPresentation,
        body_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self.git.state.model.history_filter.author.clone();
        let mut rows: Vec<(Option<String>, String)> =
            vec![(None, locale::common("All authors").to_string())];
        for author in &self.git.state.model.history_authors {
            let label = if author.name.trim().is_empty() {
                author.email.clone()
            } else {
                author.name.clone()
            };
            rows.push((Some(author.email.clone()), label));
        }
        self.render_choice_sheet(
            ChoiceSheet {
                key: SheetKey::GitFilter,
                debug_id: "git-author-sheet",
                title: locale::text("History author", "历史作者", "歷史作者"),
                rows,
                selected,
                choose: |this, value, cx| this.set_git_history_author(value, cx),
            },
            presentation,
            body_scroll,
            cx,
        )
    }

    /// The date presets the history filter picks from.
    fn render_git_date_sheet(
        &self,
        presentation: SheetPresentation,
        body_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now = vibex_core::unix_timestamp_ms();
        let selected = MobileHistoryWindow::from_bounds(
            self.git.state.model.history_filter.authored_after_ms,
            self.git.state.model.history_filter.authored_before_ms,
            now,
        );
        let mut rows: Vec<(Option<MobileHistoryWindow>, String)> =
            vec![(None, locale::common("Any date").to_string())];
        rows.extend(
            MobileHistoryWindow::ALL
                .into_iter()
                .map(|window| (Some(window), window.label().to_string())),
        );
        self.render_choice_sheet(
            ChoiceSheet {
                key: SheetKey::GitFilter,
                debug_id: "git-date-sheet",
                title: locale::text("History date", "历史日期", "歷史日期"),
                rows,
                selected,
                choose: |this, value, cx| this.set_git_history_window(value, cx),
            },
            presentation,
            body_scroll,
            cx,
        )
    }

    /// One list sheet, shared by every rail control that offers a choice.
    ///
    /// The history filters and the file search's mode differ only in what a row
    /// carries, and a phone can only show one shape of list anyway. The sheet's
    /// own key travels with the description so the panel that opened it — not
    /// the first one to be written — is the one that closes it.
    fn render_choice_sheet<T: Clone + PartialEq + 'static>(
        &self,
        choice: ChoiceSheet<T>,
        presentation: SheetPresentation,
        body_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ChoiceSheet {
            key,
            debug_id,
            title,
            rows,
            selected,
            choose,
        } = choice;
        let sheet = div()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(theme::font_title())
                            .text_color(theme::text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .id("choice-sheet-close")
                            .size(px(theme::TOUCH_TARGET))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.close_sheet(key, cx)),
                            )
                            .child(
                                svg()
                                    .path("icons/x.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    ),
            )
            .child(
                div()
                    .id("choice-sheet-list")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&body_scroll)
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .flex()
                    .flex_col()
                    .p_2()
                    .children(rows.into_iter().enumerate().map(|(index, (value, label))| {
                        let chosen = value == selected;
                        let target = value.clone();
                        div()
                            .id(("choice-sheet-row", index))
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_3()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| choose(this, target.clone(), cx)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(theme::font_body())
                                    .text_color(if chosen {
                                        theme::primary()
                                    } else {
                                        theme::text_primary()
                                    })
                                    .child(label),
                            )
                            .when(chosen, |row| {
                                row.child(
                                    svg()
                                        .path("icons/check.svg")
                                        .size(px(theme::ICON_SM))
                                        .flex_shrink_0()
                                        .text_color(theme::primary()),
                                )
                            })
                    })),
            );
        bottom_sheet_shell(debug_id, presentation)
            .on_dismiss(cx.listener(move |this, _, _, cx| this.tap_sheet_scrim(key, cx)))
            .on_pan(
                cx.listener(move |this, event, window, cx| this.pull_sheet(key, event, window, cx)),
            )
            .content(sheet)
            .into_any_element()
    }

    /// One of the content search's switches.
    ///
    /// The desktop folds these into the search field's own frame. A phone has
    /// no room for that, so they get the strip's tab treatment instead — the
    /// same dim plate and square corner the surface tabs wear — rather than a
    /// row of outlined pills, which read as a second wall of buttons standing
    /// in front of the field they belong to.
    fn render_file_search_option(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        toggle: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .flex_shrink_0()
            .h(px(WORKBENCH_TAB_HEIGHT))
            .px(px(theme::SPACING_SM))
            .rounded(px(TAB_RADIUS))
            .when(on, |chip| chip.bg(theme::rail_tab_bg()))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .text_size(theme::font_caption())
            .text_color(if on {
                theme::text_primary()
            } else {
                theme::text_muted()
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| toggle(this, cx)),
            )
            .child(label)
            .into_any_element()
    }

    /// The file tree panel (search bar + rows + search results).
    fn render_file_tree(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.files.state.view();
        let rows = view.rows.clone();
        let search = view.search.clone();
        let query_present = !input_value(&self.file_search_input, cx).trim().is_empty();
        let search_loading = self.files.state.search.is_loading();
        let search_failed = self.files.state.search.phase == AsyncPhase::Failed;
        let search_has_results = !search.is_empty();
        // An answer is only what the panel reports once a request has landed:
        // while it waits, for the pause or for the authority, the search is
        // still in progress and neither the old rows nor "No matching items"
        // describe the words in the field.
        let search_settled = !search_loading && !search_failed;

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(WORKBENCH_CONTEXT_HEIGHT))
                    .px(px(theme::SPACING_XS))
                    .overflow_hidden()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap(px(TOOLBAR_GAP))
                    .child(
                        search_field(self.file_search_input.as_ref())
                            .when(search_loading, |bar| {
                                bar.child(
                                    svg()
                                        .path("icons/loader-circle.svg")
                                        .size(px(theme::ICON_SM))
                                        .flex_shrink_0()
                                        .text_color(theme::text_primary()),
                                )
                            })
                            .when(query_present, |bar| {
                                bar.child(
                                    inline_icon_button(
                                        "clear-file-search",
                                        "icons/x.svg",
                                        "Clear search",
                                    )
                                    // The glyph was drawn without a handler, so
                                    // the only way out of a query was to delete
                                    // it a character at a time.
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::clear_file_search),
                                    ),
                                )
                            }),
                    )
                    // One control that swaps its own value: what a query reads
                    // is a two-state setting, so a tap flips it rather than
                    // opening a list to pick the other state from. The word it
                    // shows is the mode in force — two labeled halves spent the
                    // row on a choice the reader was not making, and each read
                    // as a button in the way of the field.
                    .child(
                        div()
                            .id("file-search-mode")
                            .flex_shrink_0()
                            .h(px(WORKBENCH_CONTROL_SIZE))
                            .px(px(theme::SPACING_SM))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .aria_label(self.file_search_mode.toggle_label())
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.toggle_file_search_mode(cx)),
                            )
                            .child(
                                div()
                                    .text_size(theme::font_caption())
                                    .text_color(theme::text_secondary())
                                    .child(self.file_search_mode.label()),
                            ),
                    )
                    .child(
                        toolbar_icon_button("refresh-files", "icons/refresh.svg", "Refresh")
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.refresh_files(cx);
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        toolbar_icon_button(
                            "collapse-files",
                            "icons/chevrons-down-up.svg",
                            "Collapse folders",
                        )
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::collapse_file_tree)),
                    ),
            )
            // The content search's switches sit under the field they change,
            // and only in the mode that has them.
            .when(
                self.file_search_mode == MobileFileSearchMode::Content,
                |panel| {
                    panel.child(
                        div()
                            .id("file-search-options")
                            .flex_shrink_0()
                            .px(px(theme::SPACING_SM))
                            .pb(px(theme::SPACING_XS))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .child(self.render_file_search_option(
                                "file-search-case",
                                locale::text("Aa", "区分大小写", "區分大小寫"),
                                self.file_search_case_sensitive,
                                |this, cx| {
                                    this.file_search_case_sensitive =
                                        !this.file_search_case_sensitive;
                                    this.start_file_search(cx);
                                },
                                cx,
                            ))
                            .child(self.render_file_search_option(
                                "file-search-word",
                                locale::text("Word", "整词", "整詞"),
                                self.file_search_whole_word,
                                |this, cx| {
                                    this.file_search_whole_word = !this.file_search_whole_word;
                                    this.start_file_search(cx);
                                },
                                cx,
                            ))
                            .child(self.render_file_search_option(
                                "file-search-regex",
                                locale::text("Regex", "正则", "正則"),
                                self.file_search_regex,
                                |this, cx| {
                                    this.file_search_regex = !this.file_search_regex;
                                    this.start_file_search(cx);
                                },
                                cx,
                            )),
                    )
                },
            )
            // The tree starts at the top of the panel. The bar that used to sit
            // here named the workspace root and repeated the selected row's
            // path back to the reader; both are facts the tree already shows,
            // and on a phone the row they cost is a row of files.
            .child(
                div()
                    .id("mobile-files-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    // A drawer swipe is a horizontal pan; without this the
                    // pan (and the fling that follows it) would be applied to
                    // the vertical offset and the tree would scroll itself.
                    .restrict_scroll_to_axis()
                    .when(!query_present, |body| {
                        body.children(
                            rows.into_iter()
                                .map(|row| self.render_file_tree_row(row, cx)),
                        )
                    })
                    .when(query_present && search_loading, |body| {
                        body.child(empty_state(
                            "icons/loader-circle.svg",
                            locale::common("Searching"),
                        ))
                    })
                    .when(query_present && !search_loading && search_failed, |body| {
                        body.child(empty_state(
                            "icons/triangle-alert.svg",
                            locale::text("Search failed", "搜索失败", "搜尋失敗"),
                        ))
                    })
                    .when(
                        query_present && search_settled && !search_has_results,
                        |body| {
                            body.child(empty_state(
                                "icons/search.svg",
                                locale::common(
                                    if self.file_search_mode == MobileFileSearchMode::Name {
                                        "No matching items"
                                    } else {
                                        "No matching files"
                                    },
                                ),
                            ))
                        },
                    )
                    .when(
                        query_present && search_has_results && search_settled,
                        |body| {
                            body.child(
                                div()
                                    .h(px(30.0))
                                    .px(px(theme::SPACING_MD))
                                    .flex()
                                    .items_center()
                                    .border_t_1()
                                    .border_color(theme::border_subtle())
                                    .text_size(theme::font_micro())
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{} {}",
                                        search.len(),
                                        locale::common("results")
                                    )),
                            )
                            .children(search.into_iter().map(|result| {
                                let path = result.path.clone();
                                let line = result.line.unwrap_or_default();
                                let select_path = path.clone();
                                div()
                                    .id(format!("file-search-result:{path}:{line}"))
                                    .min_h(px(theme::TOUCH_TARGET))
                                    .mx(px(TREE_ROW_INSET))
                                    .px(px(theme::SPACING_XS))
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .flex()
                                    .items_center()
                                    .gap(px(theme::SPACING_SM))
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.reveal_file_search_result(
                                                select_path.clone(),
                                                line,
                                                cx,
                                            )
                                        }),
                                    )
                                    .child(mobile_file_tree_icon(
                                        file_icon_descriptor(&path, FileEntryKind::File),
                                        false,
                                        false,
                                    ))
                                    .child({
                                        let display_path = if line > 0 {
                                            format!("{path}:{line}")
                                        } else {
                                            path.clone()
                                        };
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(theme::font_caption())
                                            .text_color(theme::text_secondary())
                                            .child(display_path)
                                    })
                                    .when_some(result.snippet, |row, snippet| {
                                        row.child(
                                            div()
                                                .max_w(px(120.0))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .text_size(theme::font_micro())
                                                .text_color(theme::text_muted())
                                                .child(snippet),
                                        )
                                    })
                                    .into_any_element()
                            }))
                        },
                    ),
            )
            .into_any_element()
    }

    fn render_file_tree_row(
        &self,
        row: vibex_desktop_model::FileExplorerRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = row.path.clone();
        let kind = row.kind;
        let path_chain = row.path_chain.clone();
        let is_directory = kind == FileEntryKind::Directory;
        let text_color = file_tree_row_text_color(&row);
        let label = if is_directory && !row.segments.is_empty() {
            row.segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        } else {
            row.name.clone()
        };
        let status = row.git.map(|git| git.signal);
        let loading = matches!(
            row.load_state,
            vibex_desktop_model::FileTreeLoadState::Loading
        );
        div()
            .id(format!("file:{}", row.id))
            .relative()
            // A tree row is read at the pitch of the list around it, not at the
            // touch minimum: the row is a name to scan, and the desktop tree
            // and the readers' own file browsers all run shorter than a form
            // field. It is a floor rather than a height, so a reader who takes
            // the text-size ladder up a step keeps a row that fits its label.
            .min_h(px(TREE_ROW_HEIGHT))
            .mx(px(TREE_ROW_INSET))
            .px(px(theme::SPACING_XS))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(6.0))
            .when(row.selected, |item| item.bg(theme::sidebar_selected_bg()))
            .when(!row.selected, |item| {
                item.hover(|style| style.bg(theme::row_pressed_bg()))
            })
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .children(file_tree_guides_mobile(row.depth))
            .child(div().w(px(row.depth as f32 * 20.0)).flex_none())
            .child(
                div()
                    .w(px(16.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_directory {
                        svg()
                            .path(if row.expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(12.0))
                            .text_color(theme::text_muted())
                    } else {
                        svg()
                            .path("icons/chevron-right.svg")
                            .size(px(12.0))
                            .text_color(theme::workbench_bg())
                    }),
            )
            .child(mobile_file_tree_icon(row.icon, row.ignored, row.expanded))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(theme::font_body())
                    .text_color(text_color)
                    .child(label),
            )
            .when(loading, |item| {
                item.child(
                    svg()
                        .path("icons/loader-circle.svg")
                        .size(px(theme::ICON_SM))
                        .text_color(theme::text_primary()),
                )
            })
            .when_some(status, |item, signal| {
                item.child(
                    div()
                        .w(px(16.0))
                        .flex_none()
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_size(theme::font_micro())
                        .text_color(file_git_signal_color(signal))
                        .child(signal.short_label()),
                )
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.activate_file_row(path.clone(), path_chain.clone(), kind, cx)
                }),
            )
            .into_any_element()
    }

    fn render_git(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.git_commit_detail.is_some() || self.git_commit_detail_loading {
            return self.render_git_commit_screen(cx);
        }
        self.render_git_panel(window, cx)
    }

    fn render_git_panel(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mode = self.git.state.model.mode;
        // A sheet's own flag stays set while its panel is falling, so each gate
        // asks the sheet where that panel is rather than only whether the page
        // still wants it. The frames are read here, before the element tree is
        // built, because the state behind them is the panel's.
        let sheet_size = crate::bottom_sheet::sheet_size(window);
        let revert_sheet = self.sheet.presentation(
            SheetKey::GitRevert,
            self.git_revert_confirmation.is_some(),
            sheet_size,
        );
        let branch_sheet = self.sheet.presentation(
            SheetKey::GitBranches,
            self.git_branch_sheet_open,
            sheet_size,
        );
        let branch_scroll = self.sheet.body_scroll(SheetKey::GitBranches);
        let filter_sheet = self.sheet.presentation(
            SheetKey::GitFilter,
            self.git_author_sheet_open || self.git_date_sheet_open,
            sheet_size,
        );
        let filter_scroll = self.sheet.body_scroll(SheetKey::GitFilter);
        let worktree_sheet = self.sheet.presentation(
            SheetKey::WorktreeConfirmation,
            self.worktree_confirmation.is_some(),
            sheet_size,
        );
        let diff_sheet =
            self.sheet
                .presentation(SheetKey::GitDiff, self.git_diff.is_some(), sheet_size);
        let diff_scroll = self.sheet.body_scroll(SheetKey::GitDiff);
        let changes_active = mode == GitWorkbenchMode::Changes;
        let history_active = mode == GitWorkbenchMode::History;
        let status = self.git.state.model.status.clone();
        let status_loading = status.is_none();
        let additions = status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .map(|change| change.additions)
                    .sum::<u32>()
            })
            .unwrap_or_default();
        let deletions = status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .map(|change| change.deletions)
                    .sum::<u32>()
            })
            .unwrap_or_default();
        let change_count = status.as_ref().map_or(0, |status| status.changes.len());
        let can_commit = self
            .git
            .capabilities()
            .supports(BackendOperation::GitCommit)
            && self.can_mutate_git();
        // Revert and the remote actions are separate capabilities. Gating all
        // three on `GitStatus` showed a live button on a runtime that would
        // fail the mutation when it was pressed.
        let can_remote = self
            .git
            .capabilities()
            .supports(BackendOperation::GitRemoteAction)
            && self.can_mutate_git();
        let can_revert = self
            .git
            .capabilities()
            .supports(BackendOperation::GitRevert)
            && self.can_mutate_git();
        let has_selected_paths = self.git.state.model.selected_path_count() > 0;
        let can_revert = can_revert && has_selected_paths;

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            // ── where the working tree stands ────────────────────────────────
            // The branch and its totals are one thought, so they share the row
            // above the list instead of a band of their own; the history view
            // spends the same row on the query that filters it.
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(WORKBENCH_CONTEXT_HEIGHT))
                    .px(px(theme::SPACING_SM))
                    .overflow_hidden()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .when(changes_active, |row| {
                        row.child(
                            svg()
                                .path("icons/git-branch.svg")
                                .size(px(theme::ICON_SM))
                                .flex_shrink_0()
                                .text_color(theme::text_muted()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(theme::font_detail())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text_primary())
                                .child(
                                    status
                                        .as_ref()
                                        .and_then(|status| status.branch.clone())
                                        .unwrap_or_else(|| {
                                            if status_loading {
                                                locale::common("Loading…").to_string()
                                            } else {
                                                locale::common("No repository").to_string()
                                            }
                                        }),
                                ),
                        )
                        .when(change_count > 0, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .font_family(theme::MONO_FONT_FAMILY)
                                    .text_size(theme::font_micro())
                                    .child(
                                        div()
                                            .text_color(if additions > 0 {
                                                theme::accent_green()
                                            } else {
                                                theme::text_muted()
                                            })
                                            .child(format!("+{additions}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(if deletions > 0 {
                                                theme::accent_red()
                                            } else {
                                                theme::text_muted()
                                            })
                                            .child(format!("-{deletions}")),
                                    ),
                            )
                        })
                    })
                    .when(history_active, |row| {
                        row.child(
                            search_field(self.git_history_query_input.as_ref())
                                // The query runs after the pause and then on the
                                // wire; the field says so rather than looking
                                // unchanged while the list is still the old one.
                                .when(self.git_history_loading, |bar| {
                                    bar.child(
                                        svg()
                                            .path("icons/loader-circle.svg")
                                            .size(px(theme::ICON_SM))
                                            .flex_shrink_0()
                                            .text_color(theme::text_primary()),
                                    )
                                })
                                .when(
                                    !input_value(&self.git_history_query_input, cx)
                                        .trim()
                                        .is_empty(),
                                    |bar| {
                                        bar.child(
                                            inline_icon_button(
                                                "clear-git-history-search",
                                                "icons/x.svg",
                                                "Clear commit search",
                                            )
                                            // Drawn without a handler, like the
                                            // file search's own clear glyph: a
                                            // reader who mistyped a hash had to
                                            // erase it by hand to get back to
                                            // the unfiltered list.
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::clear_git_history_search),
                                            ),
                                        )
                                    },
                                ),
                        )
                    })
                    // Fetch before pull: the phone has no terminal habit to
                    // fall back on, and "what is on the remote" is its own
                    // question that a pull would answer by merging.
                    //
                    // The row carries no refresh control of its own: the
                    // desktop's own Changes header has none either, and the
                    // fetch glyph beside these two already reads as one. A
                    // reader who wants the status re-read has Fetch.
                    .child({
                        let running = self.git_remote_action == Some(GitRemoteActionKind::Fetch);
                        remote_action_button("git-fetch", "icons/refresh.svg", "Fetch", running)
                            .when(can_remote && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.run_git_remote_action(GitRemoteActionKind::Fetch, cx)
                                    }),
                                )
                            })
                            .when(!can_remote || self.busy, |button| button.opacity(0.55))
                    })
                    .child({
                        let running = self.git_remote_action == Some(GitRemoteActionKind::Pull);
                        remote_action_button("git-pull", "icons/download.svg", "Pull", running)
                            .when(can_remote && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.run_git_remote_action(GitRemoteActionKind::Pull, cx)
                                    }),
                                )
                            })
                            .when(!can_remote || self.busy, |button| button.opacity(0.55))
                    })
                    .child({
                        let running = self.git_remote_action == Some(GitRemoteActionKind::Push);
                        remote_action_button("git-push", "icons/upload.svg", "Push", running)
                            .when(can_remote && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.run_git_remote_action(GitRemoteActionKind::Push, cx)
                                    }),
                                )
                            })
                            .when(!can_remote || self.busy, |button| button.opacity(0.55))
                    }),
            )
            // ── what the list below is showing ───────────────────────────────
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(WORKBENCH_FILTER_HEIGHT))
                    .px(px(theme::SPACING_SM))
                    .overflow_hidden()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap(px(TOOLBAR_GAP))
                    .child(
                        div()
                            .id("git-mode")
                            .flex_shrink_0()
                            .h(px(WORKBENCH_CONTROL_SIZE))
                            .flex()
                            .items_center()
                            .child(
                                rail_switch_segment(
                                    "git-mode:changes",
                                    locale::text("Changes", "更改", "變更"),
                                    changes_active,
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.set_git_mode(GitWorkbenchMode::Changes, cx)
                                    }),
                                ),
                            )
                            .child(
                                rail_switch_segment(
                                    "git-mode:history",
                                    // The desktop names this tab 提交, not the
                                    // English "Commits".
                                    locale::text("Commits", "提交", "提交"),
                                    history_active,
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.set_git_mode(GitWorkbenchMode::History, cx)
                                    }),
                                ),
                            ),
                    )
                    .child(div().flex_1())
                    .when(changes_active, |row| {
                        // How many paths are selected is answered by the commit
                        // panel at the foot of the surface, which is where the
                        // number is actually acted on; repeating it here would
                        // squeeze the row on a narrow phone for nothing.
                        row.child(
                            toolbar_icon_button(
                                "select-all-git",
                                "icons/list-checks.svg",
                                "Select all",
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    let selected = this.git.state.model.path_selection_state("")
                                        != GitPathSelectionState::Checked;
                                    this.git.state.model.select_path_prefix("", selected);
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            toolbar_icon_button(
                                "git-revert",
                                "icons/undo.svg",
                                "Rollback selected",
                            )
                            .when(!self.busy && can_revert, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::revert_selected_git),
                                )
                            })
                            .when(!can_revert, |button| button.opacity(0.55)),
                        )
                        .child(
                            toolbar_icon_button(
                                "git-toggle-directories",
                                "icons/chevrons-down-up.svg",
                                "Expand or collapse all",
                            )
                            .when(
                                self.git.state.model.has_change_directories(),
                                |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.git.state.model.toggle_all_change_directories();
                                            cx.notify();
                                        }),
                                    )
                                },
                            ),
                        )
                    })
                    .when(history_active, |row| {
                        row.child(
                            chip_action(
                                "git-history-branch",
                                self.git
                                    .state
                                    .model
                                    .history_filter
                                    .ref_name
                                    .as_deref()
                                    .map(|branch| short_branch_label(branch, 18))
                                    .unwrap_or_else(|| locale::common("All branches").to_string()),
                                true,
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::open_git_branch_sheet),
                            ),
                        )
                        .child(
                            chip_action(
                                "git-history-author",
                                self.git
                                    .state
                                    .model
                                    .history_filter
                                    .author
                                    .as_deref()
                                    .unwrap_or(locale::common("All authors"))
                                    .to_string(),
                                true,
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::open_git_author_sheet),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .id("mobile-git-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .when(changes_active, |body| {
                        let row_count = self.git.state.model.change_tree_row_count();
                        body.when(row_count == 0, |body| {
                            body.child(empty_state(
                                "icons/git-branch.svg",
                                if status_loading {
                                    locale::common("Loading repository status...")
                                } else {
                                    locale::common("Working tree is clean")
                                },
                            ))
                        })
                        .children((0..row_count).filter_map(|index| {
                            self.git
                                .state
                                .model
                                .change_tree_row(index)
                                .map(|(row, change)| {
                                    self.render_git_tree_row_mobile(
                                        row.clone(),
                                        change.cloned(),
                                        cx,
                                    )
                                })
                        }))
                    })
                    .when(history_active, |body| {
                        self.render_git_history_body(body, cx)
                    }),
            )
            .when(changes_active, |root| {
                root.child(self.render_git_commit_panel(can_commit, has_selected_paths, cx))
            })
            .when_some(
                self.git_revert_confirmation.clone(),
                |root, paths| match revert_sheet {
                    Some(presentation) => {
                        root.child(self.render_git_revert_confirmation(&paths, presentation, cx))
                    }
                    None => root,
                },
            )
            // The diff is a sheet, not another row at the end of a long tree:
            // it opens where the eye already is, scrolls on its own, and has a
            // visible way out.
            .when_some(self.git_diff.clone(), |root, diff| match diff_sheet {
                Some(presentation) => {
                    root.child(self.render_git_diff(diff, presentation, diff_scroll.clone(), cx))
                }
                None => root,
            })
            .when_some(self.render_worktree_card(cx), |root, card| root.child(card))
            .when(self.git_branch_sheet_open, |root| match branch_sheet {
                Some(presentation) => root.child(self.render_git_branch_sheet(
                    presentation,
                    branch_scroll.clone(),
                    cx,
                )),
                None => root,
            })
            .when(
                self.git_author_sheet_open || self.git_date_sheet_open,
                |root| match filter_sheet {
                    Some(presentation) => root.child(if self.git_author_sheet_open {
                        self.render_git_author_sheet(presentation, filter_scroll.clone(), cx)
                    } else {
                        self.render_git_date_sheet(presentation, filter_scroll.clone(), cx)
                    }),
                    None => root,
                },
            )
            .when_some(worktree_sheet, |root, presentation| {
                root.child(self.render_worktree_confirmation(presentation, cx))
            })
            .into_any_element()
    }

    /// The rollback guard. Naming every path would overflow the sheet, so it
    /// reports the count and lists the first few.
    fn render_git_revert_confirmation(
        &self,
        paths: &[String],
        presentation: SheetPresentation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let listed = paths.iter().take(4).cloned().collect::<Vec<_>>().join("\n");
        let body = div()
            .min_h_0()
            .p(px(theme::SPACING_LG))
            .flex()
            .flex_col()
            .gap(px(theme::SPACING_SM))
            .child(
                div()
                    .text_size(theme::font_heading())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(locale::text(
                        "Roll back selected changes?",
                        "回滚所选更改？",
                        "回滾所選變更？",
                    )),
            )
            .child(
                div()
                    .text_size(theme::font_caption())
                    .text_color(theme::text_muted())
                    .child(locale::text(
                        "These files go back to the committed version and local edits are lost.",
                        "这些文件会回到已提交版本，本地修改将丢失。",
                        "這些檔案會回到已提交版本，本機修改將遺失。",
                    )),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT_FAMILY)
                    .text_size(theme::font_micro())
                    .text_color(theme::text_secondary())
                    .child(listed),
            )
            .when(paths.len() > 4, |sheet| {
                sheet.child(
                    div()
                        .text_size(theme::font_micro())
                        .text_color(theme::text_muted())
                        .child(
                            locale::text("and more", "以及更多", "以及更多").to_string()
                                + &format!(" ({} total)", paths.len()),
                        ),
                )
            })
            .child(
                div()
                    .mt(px(theme::SPACING_SM))
                    .flex()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        action_button("git-revert-cancel", locale::common("Cancel"), "icons/x.svg")
                            .flex_1()
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::cancel_revert_git)),
                    )
                    .child(
                        action_button(
                            "git-revert-confirm",
                            locale::text("Roll back", "回滚", "回滾"),
                            "icons/rotate-ccw.svg",
                        )
                        .flex_1()
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::confirm_revert_git)),
                    ),
            );
        bottom_sheet_shell("mobile-git-revert-confirmation", presentation)
            .on_dismiss(cx.listener(|this, _, _, cx| this.tap_sheet_scrim(SheetKey::GitRevert, cx)))
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::GitRevert, event, window, cx)
            }))
            .content(body)
            .into_any_element()
    }

    fn render_git_tree_row_mobile(
        &self,
        row: GitTreeRow,
        change: Option<GitChange>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_directory = row.kind == GitTreeRowKind::Directory;
        let path = row.path.clone();
        let path_chain = row
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect::<Vec<_>>();
        let label = row
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let selection = self.git.state.model.path_selection_state(&path);
        let row_id = row.id.clone();
        let change_staged = change
            .as_ref()
            .is_some_and(|change| change.staged && !change.unstaged);
        let text_color = change
            .as_ref()
            .map(git_change_text_color_mobile)
            .unwrap_or_else(theme::text_primary);
        let file_name = row
            .segments
            .last()
            .map(|segment| segment.name.as_str())
            .unwrap_or(row.path.as_str())
            .to_string();
        let icon = if is_directory {
            file_icon_descriptor("", FileEntryKind::Directory)
        } else {
            file_icon_descriptor(&file_name, FileEntryKind::File)
        };
        div()
            .id(format!("git-tree:{row_id}"))
            .relative()
            .min_h(px(TREE_ROW_HEIGHT))
            .mx(px(TREE_ROW_INSET))
            .px(px(theme::SPACING_XS))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(6.0))
            // A checked row wears no wash of its own. The checkbox is what
            // carries the state, exactly as it does on the desktop, where a
            // selected path is drawn by the indicator and nothing else: a
            // second highlight down the row repeated the same fact in a louder
            // voice and turned a list of changes into a list of bars.
            .hover(|style| style.bg(theme::row_pressed_bg()))
            .children(file_tree_guides_mobile(row.depth))
            .child(div().w(px(row.depth as f32 * 20.0)).flex_none())
            .child({
                let select_path = path.clone();
                div()
                    .id(format!("git-select:{row_id}"))
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            let selected = selection != GitPathSelectionState::Checked;
                            if is_directory {
                                this.git
                                    .state
                                    .model
                                    .select_path_prefix(&select_path, selected);
                            } else {
                                this.git.state.model.select_path(&select_path, selected);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(git_selection_indicator_mobile(selection))
            })
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_directory {
                        svg()
                            .path(if row.expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(12.0))
                            .text_color(theme::text_muted())
                    } else {
                        svg()
                            .path("icons/chevron-right.svg")
                            .size(px(12.0))
                            .text_color(theme::workbench_bg())
                    }),
            )
            .child(mobile_file_tree_icon(icon, false, row.expanded))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(theme::font_body())
                    .text_color(text_color)
                    .when(
                        change
                            .as_ref()
                            .is_some_and(|change| change.kind == GitChangeKind::Deleted),
                        |item| item.line_through(),
                    )
                    .child(if label.is_empty() {
                        path.clone()
                    } else {
                        label
                    }),
            )
            .when_some(change.clone(), |item, change| {
                item.child(change_status_pill(
                    change.kind,
                    git_change_text_color_mobile(&change),
                ))
                .child(
                    // The two counts share one column pair so a list of changes
                    // reads as a table: the eye compares magnitudes down the
                    // right edge instead of hunting for them after each name.
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_size(theme::font_micro())
                        .child(
                            div()
                                .min_w(px(28.0))
                                .text_color(if change.additions > 0 {
                                    theme::accent_green()
                                } else {
                                    theme::text_muted()
                                })
                                .child(format!("+{}", change.additions)),
                        )
                        .child(
                            div()
                                .min_w(px(28.0))
                                .text_color(if change.deletions > 0 {
                                    theme::accent_red()
                                } else {
                                    theme::text_muted()
                                })
                                .child(format!("-{}", change.deletions)),
                        ),
                )
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if is_directory {
                        this.git.state.model.toggle_change_directories(&path_chain);
                    } else if change.is_some() {
                        this.open_git_diff(path.clone(), change_staged, cx);
                    }
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// Full-screen commit view: header with a back button, commit metadata,
    /// the changed-file list, and the full patch body.
    fn render_git_commit_screen(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let detail = self.git_commit_detail.clone();
        let loading = self.git_commit_detail_loading;
        let subject = detail
            .as_ref()
            .map(|detail| detail.summary.subject.clone())
            .unwrap_or_default();
        let short_hash = detail
            .as_ref()
            .map(|detail| detail.summary.short_hash.clone())
            .unwrap_or_default();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::workbench_panel_bg())
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(48.0))
                    .px_1()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("git-commit-back")
                            .size(px(theme::TOUCH_TARGET))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::close_git_commit_screen),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron-left.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(theme::font_body())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(subject),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(theme::font_micro())
                                    .font_family(theme::MONO_FONT_FAMILY)
                                    .text_color(theme::text_muted())
                                    .child(short_hash),
                            ),
                    ),
            )
            .child(
                div()
                    .id("git-commit-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .when(loading, |body| {
                        body.child(empty_state(
                            "icons/loader-circle.svg",
                            locale::common("Loading"),
                        ))
                    })
                    .when_some(detail.clone(), |body, detail| {
                        let summary = &detail.summary;
                        let stats = detail.files.iter().fold(
                            (0u32, 0u32, 0usize),
                            |(add, del, count), file| {
                                (add + file.additions, del + file.deletions, count + 1)
                            },
                        );
                        body.child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::text_muted())
                                        .child(history_relative_time(summary.authored_at_ms))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .child(summary.author_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .font_family(theme::MONO_FONT_FAMILY)
                                                .text_color(theme::accent_green())
                                                .child(format!("+{}", stats.0)),
                                        )
                                        .child(
                                            div()
                                                .font_family(theme::MONO_FONT_FAMILY)
                                                .text_color(theme::accent_red())
                                                .child(format!("-{}", stats.1)),
                                        ),
                                )
                                .when_some(detail.body.clone(), |meta, commit_body| {
                                    // The message body is a disclosure, and it
                                    // starts folded: `git show` repeats the
                                    // subject as the first line, and the header
                                    // above already draws it. The desktop folds
                                    // the same lines behind the same switch.
                                    let body_lines =
                                        commit_body_content_lines(&commit_body, &summary.subject);
                                    meta.when(!body_lines.is_empty(), |meta| {
                                        meta.child(
                                            div()
                                                .id("git-commit-body")
                                                .pt_1()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .cursor_pointer()
                                                .aria_label(locale::text(
                                                    "Commit message",
                                                    "提交信息",
                                                    "提交資訊",
                                                ))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, _, cx| {
                                                        this.git_commit_body_expanded =
                                                            !this.git_commit_body_expanded;
                                                        cx.notify();
                                                    }),
                                                )
                                                .when(self.git_commit_body_expanded, |body| {
                                                    body.child(
                                                        div()
                                                            .flex()
                                                            .flex_col()
                                                            .text_size(theme::font_caption())
                                                            .whitespace_normal()
                                                            .text_color(theme::text_secondary())
                                                            .children(body_lines.into_iter().map(
                                                                |line| {
                                                                    div().child(
                                                                        if line.is_empty() {
                                                                            " ".to_string()
                                                                        } else {
                                                                            line
                                                                        },
                                                                    )
                                                                },
                                                            )),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .text_size(theme::font_caption())
                                                        .text_color(theme::primary())
                                                        .child(
                                                            // The disclosure's own glyph, drawn at
                                                            // full contrast. Its colour is set on
                                                            // the glyph rather than inherited from
                                                            // the label beside it: a chevron that
                                                            // borrowed the primary tone and faded
                                                            // into it read as an empty box.
                                                            svg()
                                                                .path(
                                                                    if self.git_commit_body_expanded
                                                                    {
                                                                        "icons/chevron-down.svg"
                                                                    } else {
                                                                        "icons/chevron-right.svg"
                                                                    },
                                                                )
                                                                .size(px(14.0))
                                                                .flex_none()
                                                                .text_color(theme::text_primary()),
                                                        )
                                                        .child(if self.git_commit_body_expanded {
                                                            locale::text("Collapse", "收起", "收起")
                                                        } else {
                                                            locale::text("Expand", "展开", "展開")
                                                        }),
                                                ),
                                        )
                                    })
                                })
                                .children(summary.refs.iter().take(4).cloned().map(|reference| {
                                    div()
                                        .mr_1()
                                        .max_w(px(120.0))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .rounded(px(3.0))
                                        .bg(theme::accent_blue().opacity(0.18))
                                        .px_1()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::accent_blue())
                                        .child(reference)
                                })),
                        )
                        .child(
                            div()
                                .h(px(30.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_t_1()
                                .border_b_1()
                                .border_color(theme::border_subtle())
                                .text_size(theme::font_micro())
                                .text_color(theme::text_muted())
                                .child(locale::text("Files", "文件", "檔案"))
                                .child(format!("{}", stats.2)),
                        )
                        .children(
                            detail
                                .files
                                .iter()
                                .map(|file| self.render_git_commit_file_row(file)),
                        )
                        .when_some(detail.patch.clone(), |body, patch| {
                            body.child(
                                div()
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .border_t_1()
                                    .border_color(theme::border_subtle())
                                    .text_size(theme::font_micro())
                                    .text_color(theme::text_muted())
                                    .child(locale::text("Patch", "补丁", "補丁")),
                            )
                            .child(self.render_git_patch_body(&patch, cx))
                            .when(detail.patch_truncated, |body| {
                                body.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::accent_yellow())
                                        .child(locale::text(
                                            "The patch is truncated.",
                                            "补丁内容已截断。",
                                            "補丁內容已截斷。",
                                        )),
                                )
                            })
                        })
                        .when(detail.patch.is_none(), |body| {
                            body.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_size(theme::font_micro())
                                    .text_color(theme::text_muted())
                                    .child(locale::text(
                                        "This commit has no patch content.",
                                        "该提交没有补丁内容。",
                                        "該提交沒有補丁內容。",
                                    )),
                            )
                        })
                    }),
            )
            .into_any_element()
    }

    /// One changed file in a commit, using the desktop right-rail colors and
    /// A/D/M/R/C/U labels.
    fn render_git_commit_file_row(&self, file: &GitCommitFileChange) -> gpui::AnyElement {
        let color = git_change_text_color_mobile(&GitChange {
            path: file.path.clone(),
            original_path: file.original_path.clone(),
            kind: file.kind,
            staged: false,
            unstaged: true,
            additions: file.additions,
            deletions: file.deletions,
        });
        div()
            .min_h(px(theme::TOUCH_TARGET))
            .px_3()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .h(px(18.0))
                    .min_w(px(22.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(theme::border_default())
                    .px_1()
                    .font_family(theme::MONO_FONT_FAMILY)
                    .text_size(theme::font_micro())
                    .text_color(color)
                    .child(git_change_label_mobile(file.kind)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(theme::font_caption())
                    .text_color(theme::text_secondary())
                    .when_some(file.original_path.clone(), |row, original| {
                        row.child(format!("{original} → {}", file.path))
                    })
                    .when(file.original_path.is_none(), |row| {
                        row.child(file.path.clone())
                    }),
            )
            .child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .font_family(theme::MONO_FONT_FAMILY)
                    .text_size(theme::font_micro())
                    .text_color(if file.additions > 0 {
                        theme::accent_green()
                    } else {
                        theme::text_muted()
                    })
                    .child(format!("+{}", file.additions)),
            )
            .child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .font_family(theme::MONO_FONT_FAMILY)
                    .text_size(theme::font_micro())
                    .text_color(if file.deletions > 0 {
                        theme::accent_red()
                    } else {
                        theme::text_muted()
                    })
                    .child(format!("-{}", file.deletions)),
            )
            .into_any_element()
    }

    /// A commit's patch, drawn through the same rows the working-tree diff
    /// sheet uses and folded under one bar per file.
    ///
    /// The desktop renders a commit's diff and a working-tree diff with one row
    /// projection, so the two surfaces cannot disagree about what an addition
    /// looks like. It groups them the same way too: each file's changes sit
    /// under a bar carrying its path, and a tap on that bar folds them away. A
    /// commit that touches twenty files is read by folding the ones the reader
    /// is not on, which is the only way a phone shows a patch of that size.
    fn render_git_patch_body(&self, patch: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut painted = 0usize;
        let files = parse_patch_files(patch);
        div()
            .flex()
            .flex_col()
            .children(files.into_iter().filter_map(|file| {
                if painted >= GIT_COMMIT_PATCH_PAGE_LINES {
                    return None;
                }
                let shown = file
                    .rows
                    .len()
                    .min(GIT_COMMIT_PATCH_PAGE_LINES.saturating_sub(painted));
                painted += shown;
                let collapsed = self.git_commit_collapsed_files.contains(&file.path);
                let toggle_path = file.path.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            // The file's own bar: the disclosure and the path.
                            // It is the row the desktop draws above every file's
                            // part of the patch, so a reader can tell where one
                            // file ends and the next begins without reading a
                            // hunk header.
                            div()
                                .id(format!("commit-file:{}", file.path))
                                .flex_none()
                                .h(px(30.0))
                                .w_full()
                                .min_w_0()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(theme::border_subtle())
                                .bg(theme::workbench_panel_bg())
                                .cursor_pointer()
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.toggle_git_commit_file(&toggle_path, cx)
                                    }),
                                )
                                .child(
                                    svg()
                                        .path(if collapsed {
                                            "icons/chevron-right.svg"
                                        } else {
                                            "icons/chevron-down.svg"
                                        })
                                        .size(px(12.0))
                                        .flex_none()
                                        .text_color(theme::text_muted()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(theme::font_caption())
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::text_primary())
                                        .child(if file.path.is_empty() {
                                            locale::common("Patch").to_string()
                                        } else {
                                            file.path.clone()
                                        }),
                                ),
                        )
                        .when(!collapsed, |body| {
                            body.children(file.rows.into_iter().take(shown).map(render_diff_row))
                        })
                        .into_any_element(),
                )
            }))
            .into_any_element()
    }

    /// The diff sheet: a header with the path and a way out, then the patch
    /// with the old/new line numbers the desktop shows. Long lines wrap with a
    /// hanging indent so a continuation cannot be mistaken for a new line.
    fn render_git_diff(
        &self,
        diff: GitDiffResponse,
        presentation: SheetPresentation,
        body_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let limit = self.git_diff_limit.max(GIT_DIFF_PAGE_LINES);
        // The same reduction a commit's patch goes through, so the working-tree
        // sheet and the commit detail draw one diff the same way.
        let (lines, remaining) = parse_unified_diff(&diff.diff, limit);
        let path = diff.path.clone();
        let sheet = div()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::DRAWER_HEADER_HEIGHT))
                    .flex_shrink_0()
                    .px(px(theme::SPACING_MD))
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        svg()
                            .path("icons/file-code.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::accent_blue()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(theme::font_caption())
                            .text_color(theme::text_primary())
                            .child(path),
                    )
                    // Which side of the index this is, which the header used to
                    // leave to the reader to infer from the contents.
                    .child(
                        div()
                            .flex_shrink_0()
                            .px_2()
                            .py(px(2.0))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::bg_card_dim())
                            .text_size(theme::font_micro())
                            .text_color(if diff.staged {
                                theme::accent_green()
                            } else {
                                theme::text_secondary()
                            })
                            .child(if diff.staged {
                                locale::text("Staged", "已暂存", "已暫存")
                            } else {
                                locale::text("Unstaged", "未暂存", "未暫存")
                            }),
                    )
                    .child(
                        icon_button("git-diff-close", "icons/x.svg", "Close").on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.close_sheet(SheetKey::GitDiff, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .id("mobile-git-diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&body_scroll)
                    .overflow_y_scroll()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .flex()
                    .flex_col()
                    .children(lines.into_iter().map(render_diff_row)),
            )
            // A long patch is paged rather than cut off: the rest of it is one
            // tap away instead of being a trip to the desktop.
            .when(remaining > 0, |sheet| {
                sheet.child(
                    div()
                        .id("git-diff-more")
                        .flex_shrink_0()
                        .min_h(px(theme::TOUCH_TARGET))
                        .px(px(theme::SPACING_MD))
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .cursor_pointer()
                        .active(|style| style.bg(theme::row_pressed_bg()))
                        .text_size(theme::font_caption())
                        .text_color(theme::primary())
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.git_diff_limit =
                                    this.git_diff_limit.saturating_add(GIT_DIFF_PAGE_LINES);
                                cx.notify();
                            }),
                        )
                        .child(locale::text("Show more lines", "显示更多行", "顯示更多行"))
                        .child(
                            div()
                                .text_size(theme::font_micro())
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "{} {}",
                                    remaining,
                                    locale::text("left", "行未显示", "行未顯示")
                                )),
                        ),
                )
            });
        bottom_sheet_shell("mobile-git-diff-sheet", presentation)
            .on_dismiss(cx.listener(|this, _, _, cx| this.tap_sheet_scrim(SheetKey::GitDiff, cx)))
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::GitDiff, event, window, cx)
            }))
            .content(sheet)
            .into_any_element()
    }

    /// The bottom of the Changes surface: the message and the one action it
    /// exists for.
    ///
    /// The button is filled rather than outlined because it is the point of the
    /// whole panel; the line above it names what the action will take, which is
    /// the difference between a disabled button and a user who knows why.
    fn render_git_commit_panel(
        &self,
        can_commit: bool,
        has_selected_paths: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let action_label = if !can_commit {
            locale::common("Read only")
        } else if self.busy {
            locale::common("Working...")
        } else {
            locale::common("Commit")
        };
        let action_enabled = can_commit && !self.busy && has_selected_paths;
        div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme::border_subtle())
            .bg(theme::workbench_panel_bg())
            .p(px(theme::SPACING_SM))
            .flex()
            .flex_col()
            .gap(px(theme::SPACING_SM))
            .child(commit_message_shell(self.git_commit_input.as_ref()))
            .when(self.git_commit_confirmation, |panel| {
                panel
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .child(
                                chip_action(
                                    "toggle-commit-amend",
                                    locale::text(
                                        "Amend last commit",
                                        "修正上次提交",
                                        "修正上次提交",
                                    ),
                                    true,
                                )
                                .when(self.git_commit_amend, |button| {
                                    button.border_color(theme::primary())
                                })
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.toggle_git_commit_amend(cx)),
                                ),
                            )
                            .child(
                                chip_action(
                                    "toggle-commit-push",
                                    locale::text("Push after", "提交后推送", "提交後推送"),
                                    true,
                                )
                                .when(self.git_commit_push_after, |button| {
                                    button.border_color(theme::primary())
                                })
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.toggle_git_commit_push_after(cx)
                                    }),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .p(px(theme::SPACING_MD))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::accent_yellow().opacity(0.14))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .text_size(theme::font_caption())
                            .text_color(theme::text_secondary())
                            .child(if self.git_commit_amend {
                                locale::text(
                                    "Rewrite the last commit?",
                                    "重写上一次提交？",
                                    "重寫上一次提交？",
                                )
                            } else {
                                locale::text(
                                    "Commit staged changes?",
                                    "提交已暂存的更改？",
                                    "提交已暫存的變更？",
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                chip_action("cancel-commit", locale::common("Cancel"), true)
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::cancel_git_commit),
                                    ),
                            )
                            .child(
                                chip_action("confirm-commit", locale::common("Commit"), can_commit)
                                    .when(can_commit, |button| {
                                        button.on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::confirm_git_commit),
                                        )
                                    }),
                            ),
                    )
            })
            .when(!self.git_commit_confirmation, |panel| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(theme::font_micro())
                                .text_color(if has_selected_paths {
                                    theme::text_secondary()
                                } else {
                                    theme::text_muted()
                                })
                                .child(if !can_commit {
                                    locale::common("This runtime cannot commit").to_string()
                                } else if has_selected_paths {
                                    locale::text(
                                        "{} selected for this commit",
                                        "已选 {} 项待提交",
                                        "已選 {} 項待提交",
                                    )
                                    .replace(
                                        "{}",
                                        &self.git.state.model.selected_path_count().to_string(),
                                    )
                                } else {
                                    locale::common("Select changes to commit").to_string()
                                }),
                        )
                        .child(
                            // The desktop's own commit control: a 36px plate
                            // carrying the verb and the number of paths it will
                            // take. The phone drew a full-height bar stretched
                            // across the foot of the panel, which read as the
                            // screen's primary action rather than as the last
                            // step of the list above it.
                            commit_button(
                                action_label,
                                self.git.state.model.selected_path_count(),
                                action_enabled,
                            )
                            .when(action_enabled, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::request_git_commit),
                                )
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_git_history_body(
        &self,
        body: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let count = self.git.state.model.history_row_count();
        // The graph is the branch's shape, so it is laid out over the whole
        // loaded window at once: a lane only means something relative to the
        // commits above and below it, and a row drawn one at a time had no way
        // to know either. The algorithm is the desktop's own, run over the same
        // `parents` the backend returns.
        let commits = (0..count)
            .filter_map(|index| self.git.state.model.history_row(index).cloned())
            .collect::<Vec<_>>();
        let graph = git_graph_layout(&commits);
        body.when(count == 0, |body| {
            body.child(empty_state(
                "icons/message-square.svg",
                if self.git_history_loading {
                    locale::common("Loading history")
                } else {
                    locale::common("No commits found")
                },
            ))
        })
        .children(commits.into_iter().enumerate().map(|(index, commit)| {
            let row = graph
                .rows
                .get(index)
                .cloned()
                .unwrap_or_else(GitGraphRow::default);
            self.render_git_history_row(commit, row, graph.lane_count, cx)
        }))
        .when(self.git.state.model.history_has_more, |body| {
            body.child(
                div()
                    .p(px(theme::SPACING_MD))
                    .flex()
                    .justify_center()
                    .child(
                        chip_action(
                            "load-more-git-history",
                            if self.git_history_loading {
                                locale::common("Loading...")
                            } else {
                                locale::common("Load more")
                            },
                            true,
                        )
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::load_more_git_history)),
                    ),
            )
        })
    }

    fn render_git_history_row(
        &self,
        commit: GitCommitSummary,
        graph_row: GitGraphRow,
        graph_lane_count: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let hash = commit.hash.clone();
        let selected = self.git.state.model.selected_commit_hash.as_deref() == Some(hash.as_str());
        // The lane column is as wide as the graph needs, and the commits' own
        // lanes are mirrored so the branch the reader is on sits beside the
        // card: the desktop fans extra lanes out to the left for the same
        // reason, which keeps the main line easy to follow downwards.
        let graph_width = graph_lane_count.clamp(1, GIT_HISTORY_GRAPH_MAX_LANES) as f32
            * GIT_HISTORY_GRAPH_LANE_WIDTH
            + 4.0;
        let graph_colors = git_graph_colors();
        div()
            .id(format!("git-history:{}", commit.hash))
            // A definite height, not a minimum. The graph's canvas fills its
            // row, and a percentage height inside an auto-height box resolves to
            // nothing: the column had collapsed to the height of its own dot,
            // which left the lanes drawn as a row of stubs. The desktop pins the
            // same row to the same height for the same reason.
            .h(px(GIT_HISTORY_ROW_HEIGHT))
            .flex_none()
            .mx(px(TREE_ROW_INSET))
            .px(px(theme::SPACING_XS))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_SM))
            .cursor_pointer()
            .when(selected, |item| item.bg(theme::sidebar_selected_bg()))
            .when(!selected, |item| {
                item.hover(|style| style.bg(theme::row_pressed_bg()))
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let top = bounds.origin.y;
                        let bottom = bounds.origin.y + bounds.size.height;
                        let middle = bounds.origin.y + bounds.size.height / 2.0;
                        let lane_x = |lane: usize| {
                            let visual_lane =
                                graph_lane_count.saturating_sub(1).saturating_sub(lane);
                            bounds.origin.x
                                + px(2.0 + visual_lane as f32 * GIT_HISTORY_GRAPH_LANE_WIDTH)
                                + px(GIT_HISTORY_GRAPH_LANE_WIDTH / 2.0)
                        };
                        for edge in &graph_row.top_edges {
                            let from = lane_x(edge.from);
                            let to = lane_x(edge.to);
                            let mut path = PathBuilder::stroke(px(1.5));
                            path.move_to(point(from, top));
                            if edge.from == edge.to {
                                path.line_to(point(to, middle));
                            } else {
                                path.cubic_bezier_to(
                                    point(to, middle),
                                    point(from, top + bounds.size.height / 6.0),
                                    point(to, middle - bounds.size.height / 6.0),
                                );
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, graph_colors[edge.to % graph_colors.len()]);
                            }
                        }
                        for edge in &graph_row.bottom_edges {
                            let from = lane_x(edge.from);
                            let to = lane_x(edge.to);
                            let mut path =
                                PathBuilder::stroke(px(if edge.from == graph_row.lane {
                                    1.8
                                } else {
                                    1.5
                                }));
                            path.move_to(point(from, middle));
                            if edge.from == edge.to {
                                path.line_to(point(to, bottom));
                            } else {
                                path.cubic_bezier_to(
                                    point(to, bottom),
                                    point(from, middle + bounds.size.height / 6.0),
                                    point(to, bottom - bounds.size.height / 6.0),
                                );
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, graph_colors[edge.to % graph_colors.len()]);
                            }
                        }
                        // The node is a filled disc, one step larger on the
                        // commit the reader opened.
                        let radius = px(if selected { 4.5 } else { 3.5 });
                        let center = point(lane_x(graph_row.lane), middle);
                        let mut node = PathBuilder::fill();
                        node.move_to(point(center.x + radius, center.y));
                        node.arc_to(
                            point(radius, radius),
                            px(0.0),
                            false,
                            true,
                            point(center.x - radius, center.y),
                        );
                        node.arc_to(
                            point(radius, radius),
                            px(0.0),
                            false,
                            true,
                            point(center.x + radius, center.y),
                        );
                        node.close();
                        if let Ok(path) = node.build() {
                            window.paint_path(
                                path,
                                graph_colors[graph_row.lane % graph_colors.len()],
                            );
                        }
                    },
                )
                .h_full()
                .w(px(graph_width))
                .flex_none()
                .into_any_element(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(theme::font_body())
                                    .text_color(theme::text_primary())
                                    .child(commit.subject.clone()),
                            )
                            .children(commit.refs.iter().take(2).cloned().map(|reference| {
                                div()
                                    .max_w(px(88.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .rounded(px(3.0))
                                    .bg(theme::accent_blue().opacity(0.18))
                                    .px_1()
                                    .text_size(theme::font_micro())
                                    .text_color(theme::accent_blue())
                                    .child(reference)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(theme::font_micro())
                            .text_color(theme::text_muted())
                            .child(history_relative_time(commit.authored_at_ms))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(commit.author_name),
                            )
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT_FAMILY)
                                    .child(commit.short_hash),
                            ),
                    ),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.open_git_commit(hash.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// Terminal surface: session chips, an auto-fitted cell grid rendered by
    /// the shared TerminalRenderModel, and a modifier bar that sits between
    /// the phone keyboard and the PTY.
    ///
    /// The grid is the input: tapping it focuses an off-screen one-line field
    /// so the platform IME has somewhere to commit text, and every committed
    /// character is forwarded to the PTY immediately (see
    /// [`Self::forward_terminal_sink`]). There is deliberately no "type here,
    /// then press Send" row — a terminal that needs a second send gesture is
    /// not a terminal.
    fn render_terminal(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        // The room a keyboard that just closed left behind, which the grid
        // keeps holding open until the rows that fill it arrive.
        let hold = self.hold_terminal_grid(window);
        let view = self.terminal.state.view(ShellKind::Compact);
        // Only the terminals that are still running are tabs, which is the rule
        // the desktop's own list keeps. A closed session stays in the
        // authority's list — the screen it left is the record of what ran in it
        // — so a strip drawn from the whole list kept a tab for a terminal the
        // reader had already closed.
        let sessions = self
            .terminal
            .state
            .sessions
            .iter()
            .filter(|session| is_live_terminal(session))
            .cloned()
            .collect::<Vec<_>>();
        let active = self.terminal.state.active_session.clone();
        let can_create = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalCreate);
        let can_input = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalInput);
        let can_close = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalClose);
        let control_latched = self.terminal.state.control_latched;
        let alt_latched = self.terminal_alt_latched;

        div()
            .size_full()
            .flex()
            .flex_col()
            // ── in-terminal find ─────────────────────────────────────────────
            .when(self.terminal_search_open, |terminal| {
                terminal.child(self.render_terminal_search(cx))
            })
            // ── terminal tabs ────────────────────────────────────────────────
            //
            // The surface used to open with a status row above this one: a dot,
            // the session's title, its grid size, and five framed actions. On a
            // phone that row was a third of the surface spent on facts the tab
            // strip already tells and actions the tabs can carry themselves.
            // The tabs are now the whole chrome: each names its terminal, each
            // closes its own, and the new-terminal control sits at the row's
            // right edge where it cannot be scrolled away.
            //
            // The row is drawn whether or not there is a tab in it. A surface
            // that hid its own new-terminal control while its list was empty
            // would be a screen with no way out of being empty.
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .pl(px(theme::SPACING_MD))
                    .pr(px(theme::SPACING_XS))
                    .py(px(theme::SPACING_SM))
                    .child(
                        div()
                            .id("mobile-terminal-sessions")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .children(sessions.iter().enumerate().map(|(index, item)| {
                                let terminal_id = item.id.clone();
                                let close_id = item.id.clone();
                                let selected =
                                    active.as_ref().is_some_and(|active| active.id == item.id);
                                div()
                                    .id(format!("terminal-chip:{}", item.id))
                                    .flex_shrink_0()
                                    .h(px(WORKBENCH_CHIP_HEIGHT))
                                    .rounded_full()
                                    .bg(if selected {
                                        theme::accent_blue().opacity(0.16)
                                    } else {
                                        theme::workbench_panel_bg()
                                    })
                                    .pl(px(theme::SPACING_MD))
                                    .pr(px(theme::SPACING_XS))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.attach_terminal(terminal_id.clone(), cx)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .size(px(6.0))
                                            .flex_shrink_0()
                                            .rounded_full()
                                            .bg(terminal_status_color(item.status)),
                                    )
                                    .child(
                                        // Tabs are numbered in the order the
                                        // strip shows them: "终端 1", "终端
                                        // 2", and so on. A reader counting
                                        // tabs on a phone needs the number
                                        // more than the shell's own name for
                                        // itself, which is the same word on
                                        // every tab.
                                        div()
                                            .max_w(px(140.0))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(theme::font_caption())
                                            .text_color(if selected {
                                                theme::text_primary()
                                            } else {
                                                theme::text_secondary()
                                            })
                                            .child(terminal_tab_label(index)),
                                    )
                                    .child(
                                        inline_icon_button(
                                            format!("terminal-close:{}", item.id),
                                            "icons/x.svg",
                                            "Close terminal",
                                        )
                                        // The tab underneath opens the terminal
                                        // it names, and opening one clears the
                                        // close confirmation on the way in. A
                                        // close that let the tap through was
                                        // therefore undone in the same
                                        // gesture: the tab lit up and nothing
                                        // else happened. The glyph keeps the
                                        // press to itself.
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .when(
                                            can_close && !self.busy,
                                            |button| {
                                                button.on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.request_close_terminal_id(
                                                            close_id.clone(),
                                                            cx,
                                                        )
                                                    }),
                                                )
                                            },
                                        ),
                                    )
                            })),
                    )
                    .when(active.is_some(), |row| {
                        row.child(
                            toolbar_icon_button("search-terminal", "icons/search.svg", "Find")
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_terminal_search(window, cx)
                                    }),
                                ),
                        )
                    })
                    .child(
                        toolbar_icon_button("create-terminal", "icons/plus.svg", "New terminal")
                            .when(can_create && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::create_terminal),
                                )
                            })
                            .when(!can_create || self.busy, |button| button.opacity(0.55)),
                    ),
            )
            .when_some(self.terminal_close_confirmation.clone(), |terminal, _| {
                terminal.child(
                    div()
                        .flex_shrink_0()
                        .m(px(theme::SPACING_SM))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .bg(theme::accent_yellow().opacity(0.14))
                        .p(px(theme::SPACING_MD))
                        .flex()
                        .flex_col()
                        .gap(px(theme::SPACING_SM))
                        .text_size(theme::font_caption())
                        .text_color(theme::text_secondary())
                        .child(locale::text(
                            "Close this terminal and stop its running process?",
                            "关闭此终端并停止正在运行的进程？",
                            "關閉此終端機並停止正在執行的程序？",
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(theme::SPACING_SM))
                                .child(
                                    chip_action(
                                        "cancel-terminal-close",
                                        locale::common("Cancel"),
                                        true,
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::cancel_close_terminal),
                                    ),
                                )
                                .child(
                                    chip_action(
                                        "confirm-terminal-close",
                                        locale::text("Close terminal", "关闭终端", "關閉終端機"),
                                        can_close,
                                    )
                                    .when(
                                        can_close,
                                        |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::confirm_close_terminal),
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
            })
            // The grid sits in a wrapper so the room the keyboard gave up can be
            // held open below it without the surface itself growing: the surface
            // is what the rows are measured and painted against, and a fit is
            // asked for the pane, not for the hold (see [`Self::render_terminal_grid`]).
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pb(px(hold))
                    .flex()
                    .flex_col()
                    .child(self.render_terminal_grid(hold, cx)),
            )
            // ── modifier bar ─────────────────────────────────────────────────
            .when_some(active.clone(), |terminal, _| {
                terminal.child(
                    div()
                        .id("mobile-terminal-keys-scroll")
                        .flex_shrink_0()
                        .h(px(52.0))
                        .px(px(theme::SPACING_SM))
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .flex()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .children(view.key_bar.into_iter().map(|action| {
                            let key = action.key;
                            let latched = key == TerminalKey::Control && control_latched;
                            terminal_modifier_chip(
                                format!("terminal-key:{:?}", key),
                                action.label,
                                latched,
                                can_input,
                                cx.listener(move |this, _, _, cx| this.send_terminal_key(key, cx)),
                            )
                        }))
                        .child(terminal_modifier_chip(
                            "terminal-key:alt",
                            "Alt",
                            alt_latched,
                            can_input,
                            cx.listener(|this, _, _, cx| {
                                this.terminal_alt_latched = !this.terminal_alt_latched;
                                cx.notify();
                            }),
                        ))
                        .child(terminal_modifier_chip(
                            "terminal-key:paste",
                            locale::common("Paste"),
                            false,
                            can_input,
                            cx.listener(|this, _, _, cx| {
                                if let Some(text) =
                                    cx.read_from_clipboard().and_then(|item| item.text())
                                {
                                    this.send_terminal_value(
                                        TerminalInput::Text(text.replace('\n', "\r")),
                                        false,
                                        cx,
                                    );
                                }
                            }),
                        ))
                        .child(terminal_modifier_chip(
                            "terminal-key:keyboard",
                            locale::text("Keyboard", "键盘", "鍵盤"),
                            false,
                            true,
                            cx.listener(|this, _, window, cx| {
                                this.show_terminal_keyboard(window, cx)
                            }),
                        )),
                )
            })
            .into_any_element()
    }

    /// The in-terminal find bar. Matches are highlighted by the shared render
    /// model's own selection, so the grid needs no second highlight path; this
    /// row only carries the query, the position, and the way forward.
    fn render_terminal_search(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let total = self.terminal_search_matches.len();
        let position = if total == 0 {
            0
        } else {
            self.terminal_active_search_match + 1
        };
        let empty = self.terminal_search_matches.is_empty();
        div()
            .id("mobile-terminal-search")
            .flex_shrink_0()
            .h(px(WORKBENCH_CONTEXT_HEIGHT))
            .px(px(theme::SPACING_XS))
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(theme::SPACING_XS))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(search_field(self.terminal_search_input.as_ref()))
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::MONO_FONT_FAMILY)
                    .text_size(theme::font_micro())
                    .text_color(theme::text_muted())
                    .child(format!("{position}/{total}")),
            )
            .child(
                toolbar_icon_button(
                    "terminal-search-next",
                    "icons/chevron-down.svg",
                    "Next match",
                )
                .when(empty, |button| button.opacity(0.45))
                .when(!empty, |button| {
                    button.on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.select_next_terminal_search_match(cx)),
                    )
                }),
            )
            .child(
                toolbar_icon_button("terminal-search-close", "icons/x.svg", "Close").on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.toggle_terminal_search(window, cx)),
                ),
            )
            .into_any_element()
    }

    /// The floating bar a touch selection raises. It sits over the top rows so
    /// it is always clear of the software keyboard, and only offers what the
    /// phone can actually do with the selection: copy it, or leave.
    fn render_terminal_selection_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .absolute()
            .top(px(theme::SPACING_SM))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("terminal-selection-bar")
                    .h(px(TERMINAL_SELECTION_BAR_HEIGHT))
                    .px(px(theme::SPACING_XS))
                    .rounded_full()
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_popover())
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    // A press on the bar is never a press on the grid.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(terminal_selection_action(
                        "terminal-selection-copy",
                        "icons/copy.svg",
                        locale::common("Copy"),
                        cx.listener(|this, _, _, cx| this.copy_terminal_selection(cx)),
                    ))
                    .child(
                        div()
                            .w(px(1.0))
                            .h(px(theme::SPACING_XL))
                            .bg(theme::border_subtle()),
                    )
                    .child(terminal_selection_action(
                        "terminal-selection-close",
                        "icons/x.svg",
                        locale::common("Close"),
                        cx.listener(|this, _, _, cx| this.dismiss_terminal_selection(cx)),
                    )),
            )
            .into_any_element()
    }

    /// The two dots a reader drags to adjust a selection, one at each end.
    ///
    /// They are painted over the grid but carry no hitbox of their own: the
    /// surface's touch listener is what claims the drag, so a handle never has
    /// to become an interactive element (and a tap passing through it still
    /// behaves like a tap on the grid).
    fn render_terminal_selection_handles(&self) -> Option<gpui::AnyElement> {
        let (start, end) = self.terminal_selection_handles()?;
        let bounds = self.terminal_grid_bounds?;
        let handle = |point: Point<Pixels>| {
            div()
                .absolute()
                .left(px(f32::from(point.x)
                    - f32::from(bounds.left())
                    - TERMINAL_HANDLE_SIZE / 2.0))
                .top(px(f32::from(point.y)
                    - f32::from(bounds.top())
                    - TERMINAL_HANDLE_SIZE / 2.0))
                .size(px(TERMINAL_HANDLE_SIZE))
                .rounded_full()
                .bg(theme::accent())
                .border_2()
                .border_color(theme::bg_card())
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .child(handle(start))
                .child(handle(end))
                .into_any_element(),
        )
    }

    /// The cell grid. The surface reports its bounds on paint so the PTY is
    /// auto-fitted to fill the screen, and the shared render model frame is
    /// rendered cell-by-cell with the desktop palette and cursor treatment.
    ///
    /// `hold` is the room the pane keeps below the surface while a keyboard that
    /// just closed is still waiting for its rows; the fit is asked for the pane
    /// including it, because that is the height the rows will have to fill.
    fn render_terminal_grid(&self, hold: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let weak_entity = cx.weak_entity();
        let frame = self
            .terminal
            .state
            .render
            .as_ref()
            .map(|render| render.frame.clone());
        let has_session = self.terminal.state.active_session.is_some();
        let has_frame = frame.is_some();
        div()
            .id("mobile-terminal-surface")
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .bg(theme::workbench_bg())
            // The surface does not take focus. It used to, and the platform
            // delivers committed text only to the element that holds it: a tap
            // on the grid focused the surface, GPUI registered no input handler
            // for it, and every character the keyboard sent was dropped. The
            // sink below is the terminal's one focus target, and
            // `focus_terminal` hands it the tap.
            .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_terminal))
            .on_scroll(cx.listener(Self::scroll_terminal))
            // The IME sink: invisible, unfocusable by tap, but the destination
            // the platform keyboard commits into while the grid holds the
            // user's attention.
            .child(terminal_sink_element(self.terminal_input.as_ref()))
            .when(
                self.terminal.state.active_session.is_some() && !has_frame,
                |surface| {
                    surface.child(centered_pane_state(
                        "icons/loader-circle.svg",
                        locale::text("Waiting for output…", "正在等待输出…", "正在等待輸出…"),
                    ))
                },
            )
            .when(has_frame && self.terminal_scrolled_back, |surface| {
                surface.child(
                    div()
                        .id("terminal-jump-to-latest")
                        .absolute()
                        .right(px(theme::SPACING_MD))
                        .bottom(px(theme::SPACING_MD))
                        .h(px(36.0))
                        .px_3()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border_default())
                        .bg(theme::bg_card())
                        .flex()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .text_size(theme::font_caption())
                        .text_color(theme::text_secondary())
                        .cursor_pointer()
                        .active(|style| style.bg(theme::row_pressed_bg()))
                        // The pill floats over the grid: a tap on it is not a
                        // tap on the cell underneath.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.terminal_scrolled_back = false;
                                this.scroll_terminal_to_bottom(cx);
                            }),
                        )
                        .child(
                            svg()
                                .path("icons/arrow-to-top.svg")
                                .size(px(theme::ICON_SM))
                                .text_color(theme::text_secondary()),
                        )
                        .child(locale::text("Latest", "最新", "最新")),
                )
            })
            .child({
                let prepaint_entity = weak_entity.clone();
                canvas(
                    move |bounds, _, cx| {
                        let width = f32::from(bounds.size.width);
                        // The grid's own height plus the room a pending resize
                        // is still holding open: the rows are what the pane will
                        // have to fill once they arrive.
                        let height = f32::from(bounds.size.height) + hold;
                        let _ = prepaint_entity.update(cx, |this, cx| {
                            // The grid's own geometry, kept for turning a touch
                            // point into the cell under it.
                            this.terminal_grid_bounds = Some(bounds);
                            this.fit_terminal_to(width, height, cx);
                        });
                    },
                    {
                        move |bounds, _, window, _: &mut gpui::App| {
                            // A long press is the one touch the grid has to
                            // claim from the platform: the recognizer keeps
                            // reporting that same touch through `Moved`, which
                            // is the drag that extends the selection. A plain
                            // pan stays a scroll gesture and is untouched.
                            window.on_mouse_event({
                                let entity = weak_entity.clone();
                                move |event: &LongPressEvent,
                                      phase: DispatchPhase,
                                      _: &mut Window,
                                      cx: &mut gpui::App| {
                                    if phase != DispatchPhase::Bubble
                                        || !bounds.contains(&event.position)
                                    {
                                        return;
                                    }
                                    let _ = entity.update(cx, |this, cx| {
                                        this.on_terminal_long_press(event, cx)
                                    });
                                }
                            });
                            // A touch that lands on one of the selection's
                            // handles is claimed before the recognizer can
                            // resolve it as a pan, which is what lets the
                            // handle be dragged while the grid keeps its
                            // scroll and long-press gestures everywhere else.
                            // Only the landing has to be inside the grid: a
                            // drag that leaves it keeps moving the end it
                            // took hold of.
                            window.on_mouse_event({
                                let entity = weak_entity.clone();
                                move |event: &TouchDragEvent,
                                      phase: DispatchPhase,
                                      _: &mut Window,
                                      cx: &mut gpui::App| {
                                    if phase != DispatchPhase::Bubble
                                        || (event.phase == TouchPhase::Started
                                            && !bounds.contains(&event.position))
                                    {
                                        return;
                                    }
                                    let _ = entity.update(cx, |this, cx| {
                                        this.on_terminal_handle_drag(event, cx)
                                    });
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_full()
            })
            .when_some(frame, |surface, frame| {
                let rows = frame.rows;
                let columns = frame.columns;
                let cursor = frame.cursor;
                let mut cells = vec![None; usize::from(rows).saturating_mul(usize::from(columns))];
                for cell in frame.cells {
                    let index = usize::from(cell.row)
                        .saturating_mul(usize::from(columns))
                        .saturating_add(usize::from(cell.column));
                    if let Some(slot) = cells.get_mut(index) {
                        *slot = Some(cell);
                    }
                }
                surface.child(
                    div()
                        .id("mobile-terminal-grid")
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .pt(px(TERMINAL_VERTICAL_PADDING))
                        .pb(px(TERMINAL_VERTICAL_PADDING))
                        .font_family(theme::MONO_FONT_FAMILY)
                        .text_size(px(TERMINAL_FONT_SIZE))
                        .line_height(px(TERMINAL_CELL_HEIGHT))
                        .children((0..rows).map(|row| {
                            div()
                                .h(px(TERMINAL_CELL_HEIGHT))
                                .flex_none()
                                .flex()
                                .overflow_hidden()
                                .children((0..columns).map(|column| {
                                    let index = usize::from(row)
                                        .saturating_mul(usize::from(columns))
                                        .saturating_add(usize::from(column));
                                    self.render_terminal_cell(
                                        TerminalGridPoint { row, column },
                                        cells.get(index).cloned().flatten(),
                                        cursor,
                                    )
                                }))
                        })),
                )
            })
            .when(!has_frame, |surface| {
                surface.child(centered_pane_state(
                    "icons/square-terminal.svg",
                    if has_session {
                        locale::text("No output yet", "暂无输出", "暫無輸出")
                    } else {
                        locale::text(
                            "Select or create a terminal",
                            "选择或新建一个终端",
                            "選擇或新建一個終端機",
                        )
                    },
                ))
            })
            .when(self.terminal_selection.is_some(), |surface| {
                surface
                    .children(self.render_terminal_selection_handles())
                    .child(self.render_terminal_selection_bar(cx))
            })
            .into_any_element()
    }

    /// One terminal cell, using the desktop palette: indexed/RGB colors, wide
    /// glyph spacing, attribute styling, and the host cursor shape.
    fn render_terminal_cell(
        &self,
        point: TerminalGridPoint,
        cell: Option<TerminalCellSnapshot>,
        cursor: Option<vibex_terminal_ui::TerminalCursorSnapshot>,
    ) -> gpui::AnyElement {
        let cell = cell.unwrap_or_else(|| empty_terminal_cell(point));
        if cell.wide_spacer {
            return div()
                .w_0()
                .h(px(TERMINAL_CELL_HEIGHT))
                .flex_none()
                .into_any_element();
        }
        let at_cursor = cursor
            .as_ref()
            .is_some_and(|cursor| cursor.row == point.row && cursor.column == point.column);
        let default_foreground = theme::text_primary();
        let default_background = theme::workbench_bg();
        let mut foreground = terminal_cell_color(
            cell.foreground,
            default_foreground,
            default_background,
            default_foreground,
        );
        let mut background = terminal_cell_color(
            cell.background,
            default_foreground,
            default_background,
            default_background,
        );
        if cell.dim {
            foreground = foreground.opacity(0.68);
        }
        // The emulator owns the selection, so the highlight is painted from the
        // cell flags the frame carries — the same tone the desktop surface uses.
        if cell.selected {
            background = default_foreground.opacity(0.28);
        }
        if at_cursor && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Block) {
            background = default_foreground;
            foreground = default_background;
        }
        div()
            .w(px(if cell.wide {
                TERMINAL_CELL_WIDTH * 2.0
            } else {
                TERMINAL_CELL_WIDTH
            }))
            .h(px(TERMINAL_CELL_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .bg(background)
            .text_color(foreground)
            .when(cell.bold, |cell| cell.font_weight(FontWeight::BOLD))
            .when(cell.italic, |cell| cell.italic())
            .when(cell.underline || cell.hyperlink.is_some(), |cell| {
                cell.underline()
            })
            .when(cell.strikeout, |cell| cell.line_through())
            .when(cell.hidden, |cell| cell.invisible())
            .when(
                at_cursor && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Beam),
                |cell| cell.border_l_1().border_color(default_foreground),
            )
            .when(
                at_cursor
                    && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Underline),
                |cell| cell.border_b_1().border_color(default_foreground),
            )
            .when(
                at_cursor
                    && cursor
                        .is_some_and(|cursor| cursor.shape == TerminalCursorShape::HollowBlock),
                |cell| cell.border_1().border_color(default_foreground),
            )
            .child(if cell.text.is_empty() {
                " ".to_string()
            } else {
                cell.text.clone()
            })
            .into_any_element()
    }

    /// The rail's header: the surface switch and the way out, on one row.
    ///
    /// Three framed buttons read as three commands competing for the eye and
    /// cost a whole second row above the surface. A tab is a place, so the
    /// selected one carries the only fill in the row and the rest recede. The
    /// working tree's tab also carries how far its branch runs from its
    /// upstream, in the two badges the desktop paints over the same list; a
    /// running terminal is a state its own panel already shows, so the strip
    /// leaves it uncounted.
    fn render_header(&self, surface: WorkbenchSurface, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex_shrink_0()
            .h(px(WORKBENCH_HEADER_HEIGHT))
            .pl(px(theme::SPACING_XS))
            .pr(px(TOOLBAR_GAP))
            .border_b_1()
            .border_color(theme::border_subtle())
            .flex()
            .items_center()
            .gap(px(TOOLBAR_GAP))
            .child(
                // A plain row rather than a horizontal scroller: the tabs fit a
                // phone at every text size the platform offers, and a scroller
                // on this axis would compete with the swipe that closes the
                // drawer.
                div()
                    .id("mobile-workbench-tabs")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(TOOLBAR_GAP))
                    .children(WorkbenchSurface::ALL.into_iter().map(|candidate| {
                        let selected = candidate == surface;
                        div()
                            .id(format!("workbench-tab:{}", candidate.label()))
                            .h(px(WORKBENCH_TAB_HEIGHT))
                            .flex_shrink_0()
                            .px(px(theme::SPACING_SM))
                            // The plate a tab lights up with is the dimmest
                            // wash in the palette and the squarest corner the
                            // rail draws: navigation is not a card, and a tab
                            // that reads as one competes with the surface under
                            // it.
                            .rounded(px(TAB_RADIUS))
                            .when(selected, |tab| tab.bg(theme::rail_tab_bg()))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            // The surface the rail is on is navigation text a
                            // reader looks at all session, so it takes the body
                            // step rather than the caption floor the old strip
                            // sat on.
                            .text_size(theme::font_body())
                            .text_color(if selected {
                                theme::text_primary()
                            } else {
                                theme::text_muted()
                            })
                            .cursor_pointer()
                            .aria_label(candidate.localized_label())
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.set_surface(candidate, cx)),
                            )
                            .child(candidate.localized_label())
                            // Only the working tree carries a count, and it is
                            // the one the desktop's own Changes tab carries:
                            // how far the branch has drifted from its upstream.
                            // A running terminal is a state the panel shows in
                            // full, so the strip leaves it uncounted.
                            .when(candidate == WorkbenchSurface::Git, |tab| {
                                let branches = self.git_branches.as_deref().unwrap_or_default();
                                let (behind, ahead) = git_sync_counts(branches);
                                tab.when(behind > 0, |tab| {
                                    tab.child(sync_badge(behind, "↓", theme::accent_blue()))
                                })
                                .when(ahead > 0, |tab| {
                                    tab.child(sync_badge(ahead, "↑", theme::accent_success()))
                                })
                            })
                    })),
            )
            .child(self.render_close_control())
            .into_any_element()
    }

    /// The one way out a thumb can see: the rail has no hardware back key
    /// rooted in it, and an edge swipe is not an affordance the phone
    /// announces. The drawer belongs to the app, so this reports the request
    /// through the handler installed when the workbench was created.
    fn render_close_control(&self) -> gpui::AnyElement {
        let handler = self.close_handler.clone();
        div()
            .id("close-right-rail")
            .size(px(WORKBENCH_CONTROL_SIZE))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(theme::RADIUS_CONTROL))
            .cursor_pointer()
            .aria_label(locale::text("Close", "关闭", "關閉"))
            .active(|style| style.bg(theme::row_pressed_bg()))
            .when_some(handler, |button, handler| {
                button.on_mouse_up(MouseButton::Left, move |event, window, cx| {
                    handler(event, window, cx)
                })
            })
            .child(
                svg()
                    .path("icons/x.svg")
                    .size(px(theme::ICON_MD))
                    .text_color(theme::text_secondary()),
            )
            .into_any_element()
    }
}

impl Render for MobileWorkbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        // Every result and failure the rail produced since the last frame goes
        // to the window's hint layer. Nothing about it takes a line in this
        // element tree, which is what keeps a message from leaving a strip on a
        // surface it no longer describes.
        self.present_pending_hints(window, cx);
        let surface = self.surface;
        div()
            .size_full()
            .bg(theme::sidebar_bg())
            .flex()
            .flex_col()
            .child(self.render_header(surface, cx))
            .child(div().flex_1().min_h_0().child(match surface {
                WorkbenchSurface::Files => self.render_files(window, cx),
                WorkbenchSurface::Git => self.render_git(window, cx),
                WorkbenchSurface::Terminal => self.render_terminal(window, cx),
            }))
    }
}

/// One choice a rail control offers, as one value.
///
/// They travel together — the sheet's own key, the rows, the one in force, the
/// name a test finds the panel by, and what choosing a row does — so they are
/// handed over as one thing rather than as five arguments that always appear
/// together.
struct ChoiceSheet<T> {
    key: SheetKey,
    debug_id: &'static str,
    title: &'static str,
    rows: Vec<(T, String)>,
    selected: T,
    choose: fn(&mut MobileWorkbench, T, &mut Context<MobileWorkbench>),
}

/// The frame around the commit message.
///
/// A field is a surface, not a box: on a phone the border around an empty
/// rectangle is ink that says nothing, so the fill alone marks where the text
/// goes and the row it sits in supplies the inset. Three rows tall and a step
/// above the caption size, because this is the one thing on the panel the
/// reader actually types and a commit message is read back before it is sent.
fn commit_message_shell(input: Option<&Entity<TextareaState>>) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::workbench_panel_bg())
        .px_2()
        .py_1()
        .text_size(theme::font_detail())
        .on_mouse_down(MouseButton::Left, crate::app::resume_keyboard_on_tap)
        .child(match input {
            Some(input) => Textarea::new(input).appearance(false).into_any_element(),
            None => div().into_any_element(),
        })
}

/// The kit editor for the buffer created on first paint.
///
/// The editor draws its own gutter and text surface and the panel around it
/// draws no frame, so the file's pixels are the file's own: a source buffer
/// that sits inside a card has a margin the reader pays for on every line.
///
/// It is read-only, and the flag is set here rather than on the state. The
/// element pushes its own `readonly` field into the state on every paint, so a
/// state that was told once to reject edits had that instruction overwritten on
/// the next frame — which is why a reader could still type into a file the rail
/// only browses. Read-only still answers a caret, a selection and a copy, so a
/// line can be taken out of a file; it cannot be changed.
fn file_editor_element(input: Option<&Entity<EditorState>>) -> gpui::AnyElement {
    match input {
        Some(input) => Editor::new(input)
            .appearance(false)
            .bordered(false)
            .readonly(true)
            .size_full()
            .into_any_element(),
        None => div().into_any_element(),
    }
}

/// The terminal's IME sink.
///
/// It is the one text field with no tap hook of its own, and deliberately so:
/// it is invisible and a single pixel across, so no tap can reach it, and its
/// focus is handed over by the grid's own tap — `focus_terminal` focuses the
/// sink and asks the platform for the keyboard in the same gesture. A frame
/// around it that re-requested the keyboard on a tap would be a hook for a
/// gesture the reader cannot make.
///
/// It always holds one space in front of the caret (see [`TERMINAL_SINK_SEED`]).
/// The keyboard computes a deletion from the document it is bound to, so a
/// field with nothing in it turns every Backspace into an edit of nothing — the
/// change never reaches this app and the shell never hears the key. Every
/// keystroke is read from the difference between what the field held and what
/// it holds, and the field then goes straight back to that one space.
fn terminal_sink_element(input: Option<&Entity<TextareaState>>) -> gpui::AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(1.0))
        .h(px(1.0))
        .opacity(0.0)
        .child(match input {
            Some(input) => Textarea::new(input).appearance(false).into_any_element(),
            None => div().into_any_element(),
        })
        .into_any_element()
}

/// What the terminal's sink holds before the caret: one space nothing types
/// into.
///
/// The keyboard computes a deletion from the document it is bound to, so a
/// Backspace on an empty field is an edit of nothing: the kit has no character
/// to remove, no change is emitted, and the key never leaves the phone. One
/// character is the whole scratch document that fixes it, and one is the most
/// a word-delete can then take — a longer run of spaces would let a keyboard
/// that deletes words turn one key into a burst of deletes the reader never
/// asked for.
const TERMINAL_SINK_SEED: &str = " ";

/// What one edit to the terminal's sink asks the PTY to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalSinkEdit {
    /// Text was appended after what the sink held.
    Typed(String),
    /// That many characters were removed from the end.
    Deleted(usize),
    /// The edit is not a keystroke the PTY can be told about.
    Ignored,
}

/// Reads one edit of the sink as the PTY input it stands for.
///
/// The sink is a scratch document, so the reader's key is the *difference*
/// between what it held and what it holds: a longer value that starts with the
/// old one is text typed, a shorter one that the old value starts with is a
/// deletion, and anything else is an IME rewriting the document, which no
/// keystroke can describe. The cap keeps one paste from becoming an unbounded
/// write.
fn terminal_sink_edit(previous: &str, next: &str) -> TerminalSinkEdit {
    if next == previous {
        return TerminalSinkEdit::Ignored;
    }
    if let Some(typed) = next.strip_prefix(previous) {
        return if typed.is_empty() {
            TerminalSinkEdit::Ignored
        } else {
            TerminalSinkEdit::Typed(typed.to_string())
        };
    }
    if let Some(remaining) = previous.strip_prefix(next) {
        let deletes = remaining.chars().count().min(64);
        return if deletes == 0 {
            TerminalSinkEdit::Ignored
        } else {
            TerminalSinkEdit::Deleted(deletes)
        };
    }
    TerminalSinkEdit::Ignored
}

/// The text of a lazily-created kit input, or empty before the first paint.
fn input_value(input: &Option<Entity<InputState>>, cx: &gpui::App) -> gpui::SharedString {
    input
        .as_ref()
        .map(|input| input.read(cx).value())
        .unwrap_or_default()
}

/// The text of a lazily-created multi-line field, or empty before the first
/// paint.
fn textarea_value(input: &Option<Entity<TextareaState>>, cx: &gpui::App) -> gpui::SharedString {
    input
        .as_ref()
        .map(|input| input.read(cx).value())
        .unwrap_or_default()
}

/// The kit input for a field that is created on first paint. The container
/// already draws the frame, so the input contributes only its text surface.
fn input_element(input: Option<&Entity<InputState>>) -> gpui::AnyElement {
    match input {
        Some(input) => Input::new(input).appearance(false).into_any_element(),
        None => div().into_any_element(),
    }
}

/// The field half of one of the rail's search rows.
///
/// The row *is* the field. It used to be a rounded plate inset in the row, so
/// each surface drew a second rectangle inside the one its own hairline already
/// drew, and the bar read as an object resting on the row rather than as the
/// row itself. A search bar on a phone is a full-width band under the header:
/// it runs from the surface's edge to the surface's edge, and the glyph and the
/// state controls sit at the same insets every other row of the rail uses.
///
/// The caller keeps its own trailing controls — a spinner while the query is
/// running, a clear glyph while there is something to clear, the position of a
/// match — because those belong to the query rather than to the field.
fn search_field(input: Option<&Entity<InputState>>) -> gpui::Div {
    div()
        // The row has a definite height, so the field is as tall as the band it
        // fills: the whole row answers a tap, not a plate inside it.
        .h_full()
        .min_w_0()
        .flex_1()
        .pl(px(theme::SPACING_SM))
        .pr(px(theme::SPACING_XS))
        .flex()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .on_mouse_down(MouseButton::Left, crate::app::resume_keyboard_on_tap)
        .child(
            svg()
                .path("icons/search.svg")
                .size(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(theme::text_muted()),
        )
        .child(div().min_w_0().flex_1().child(input_element(input)))
}

/// How far the current branch stands from its upstream, as `(behind, ahead)`.
///
/// The desktop's own reading of the same list, and nothing is counted for a
/// branch with no upstream: a local-only branch has not drifted from anything,
/// and a badge that said otherwise would be inventing a remote.
fn git_sync_counts(branches: &[GitBranchSummary]) -> (u32, u32) {
    branches
        .iter()
        .find(|branch| branch.current && branch.upstream.is_some())
        .map(|branch| (branch.behind, branch.ahead))
        .unwrap_or_default()
}

/// The count a rail tab carries beside its name.
///
/// Geometry and tone are the desktop's own sync badge: a plate of the count's
/// colour at a twelfth, the count on it, and one corner small enough to read as
/// a label rather than a button. The arrows are the plain vertical pair the
/// branch sheet already uses, not the desktop's diagonal one: on the platform's
/// monospace face the diagonal arrows fall through to the emoji font and paint
/// as a coloured glyph, while these two stay text in every face on the phone.
fn sync_badge(count: u32, arrow: &'static str, color: gpui::Hsla) -> gpui::AnyElement {
    div()
        .h(px(18.0))
        .min_w(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .bg(color.opacity(0.12))
        .font_family(theme::MONO_FONT_FAMILY)
        .text_size(theme::font_micro())
        .text_color(color)
        .child(format!("{arrow} {count}"))
        .into_any_element()
}

/// A remote action in the Changes header, drawn as its glyph or — while the
/// authority is busy with it — as the spinner that took its place.
///
/// The desktop answers a press on Push or Pull with a control that spins for
/// as long as the work takes. The phone previously dimmed all three buttons and
/// showed nothing moving, so a push that had not landed looked like a panel
/// that had stopped responding.
fn remote_action_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: &'static str,
    running: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(WORKBENCH_CONTROL_SIZE))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(locale::common(label))
        .when(running, |button| {
            button.child(
                Spinner::new()
                    .with_size(Size::Small)
                    .color(theme::text_secondary()),
            )
        })
        .when(!running, |button| {
            button
                .cursor_pointer()
                .active(|style| style.bg(theme::row_pressed_bg()))
                .child(
                    svg()
                        .path(path)
                        .size(px(theme::ICON_SM))
                        .text_color(theme::text_secondary()),
                )
        })
}

/// A glyph action that sits in a bar beside a label rather than in a field of
/// its own.
///
/// The bar it is drawn in is the touch target — the control only owns the
/// square its glyph needs — so the box is one step under the platform minimum
/// instead of a second full-height square in every row.
fn icon_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(WORKBENCH_CONTROL_SIZE))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(locale::common(label))
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            svg()
                .path(path)
                .size(px(theme::ICON_SM))
                .text_color(theme::text_secondary()),
        )
}

/// A glyph action inside a rail toolbar or header.
///
/// The row it sits in is already a touch-height band, so the control only has
/// to own its own width: a second 44px square inside every row is what made the
/// old chrome read as a wall of boxes. The caller still gets a comfortable
/// target because the row's height is the hitbox's height.
fn toolbar_icon_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(WORKBENCH_CONTROL_SIZE))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(locale::common(label))
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            svg()
                .path(path)
                .size(px(theme::ICON_SM))
                .text_color(theme::text_secondary()),
        )
}

/// The clear glyph that lives inside a toolbar field's own frame.
fn inline_icon_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.0))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_PILL))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(locale::common(label))
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            svg()
                .path(path)
                .size(px(14.0))
                .text_color(theme::text_muted()),
        )
}

/// One half of a two-word switch in a rail toolbar.
///
/// The control is two labels and nothing else: the rail already spends its
/// frame on the header, the toolbar band, and the list, and a plate drawn
/// around two words made the switcher the heaviest object in the row it is
/// supposed to sort. The half that is current carries the reading color and
/// the other half is a label a tap promotes, so the state is legible without a
/// track to explain it.
fn rail_switch_segment(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h_full()
        .flex_shrink_0()
        .px(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .text_size(theme::font_caption())
        .text_color(if selected {
            theme::text_primary()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(label)
}

/// A secondary action drawn as a filled pill: lighter than a framed button and
/// still a target a thumb finds without reading it.
fn chip_action(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(WORKBENCH_CHIP_HEIGHT))
        .px(px(theme::SPACING_MD))
        .rounded(px(theme::RADIUS_PILL))
        .bg(theme::workbench_panel_bg())
        .flex()
        .items_center()
        .justify_center()
        .text_size(theme::font_caption())
        .text_color(theme::text_secondary())
        .when(enabled, |chip| {
            chip.cursor_pointer()
                .active(|style| style.bg(theme::row_pressed_bg()))
        })
        .when(!enabled, |chip| chip.opacity(0.55))
        .child(label.into())
}

/// The commit action: the one filled control on the Changes surface, drawn at
/// the desktop's own weight.
///
/// It carries the count of paths the commit will take in a small plate beside
/// the verb, exactly as the desktop's does, so the reader can see what the
/// press costs without reading the line above it. It stays a control rather
/// than a bar: the desktop's is 36px tall and only as wide as its own label.
fn commit_button(
    label: impl Into<gpui::SharedString>,
    count: usize,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("request-commit")
        .flex_shrink_0()
        .h(px(WORKBENCH_CONTROL_SIZE))
        .px_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(if enabled {
            theme::primary()
        } else {
            theme::bg_card_dim()
        })
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .text_size(theme::font_caption())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if enabled {
            theme::primary_foreground()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button.cursor_pointer().active(|style| style.opacity(0.85))
        })
        .child(label.into())
        .child(
            // The count plate the desktop draws: one box the eye finds without
            // reading the sentence it belongs to.
            div()
                .min_w(px(18.0))
                .h(px(18.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(theme::primary_foreground().opacity(0.18))
                .px_1()
                .font_family(theme::MONO_FONT_FAMILY)
                .text_size(theme::font_micro())
                .text_color(theme::primary_foreground())
                .child(count.to_string()),
        )
}

/// What a change is, as a tinted pill rather than a framed box: the status is
/// colour-coded already, and a border around it added a second edge to every
/// row in the tree.
fn change_status_pill(kind: GitChangeKind, color: gpui::Hsla) -> gpui::AnyElement {
    div()
        .flex_shrink_0()
        .h(px(18.0))
        .min_w(px(22.0))
        .px(px(6.0))
        .rounded(px(theme::RADIUS_PILL))
        .bg(color.opacity(0.18))
        .flex()
        .items_center()
        .justify_center()
        .font_family(theme::MONO_FONT_FAMILY)
        .text_size(theme::font_micro())
        .text_color(color)
        .child(git_change_label_mobile(kind))
        .into_any_element()
}

/// The one thing a surface says when it has nothing to show.
///
/// A sentence in the top-left corner of an empty pane reads as a mistake; the
/// pane is a place, so the message is centred in it and carries the surface's
/// own glyph, which is what tells the reader they are looking at the right
/// place with nothing in it rather than at the wrong place.
fn empty_state(icon: &'static str, label: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .py_8()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM))
        .child(
            svg()
                .path(icon)
                .size(px(24.0))
                .text_color(theme::text_muted()),
        )
        .child(
            div()
                .px(px(theme::SPACING_LG))
                .text_size(theme::font_caption())
                .text_align(gpui::TextAlign::Center)
                .text_color(theme::text_muted())
                .child(label.into()),
        )
}

/// The same message for a pane with no flow of its own.
///
/// The terminal grid is an absolutely positioned surface, so a message inside
/// it has to claim the whole pane before it can be centred in it; a terminal
/// that has never produced a frame would otherwise leave the words in a corner
/// of an empty black rectangle.
fn centered_pane_state(icon: &'static str, label: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(empty_state(icon, label))
}

/// One key of the terminal modifier bar: a real 44px target, a latched
/// appearance for ⌃/⌥, and no handler at all when the runtime cannot accept
/// input (so a disabled chip never flashes a pressed state that does nothing).
fn terminal_modifier_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    latched: bool,
    enabled: bool,
    handler: impl Fn(&gpui::MouseUpEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(theme::TOUCH_TARGET))
        .min_w(px(theme::TOUCH_TARGET))
        .px_2()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(if latched {
            theme::accent_blue()
        } else {
            theme::border_subtle()
        })
        .bg(if latched {
            theme::accent_blue().opacity(0.16)
        } else {
            theme::bg_card()
        })
        .flex()
        .items_center()
        .justify_center()
        .text_size(theme::font_caption())
        .text_color(if latched {
            theme::accent_blue()
        } else {
            theme::text_secondary()
        })
        .when(enabled, |chip| {
            chip.cursor_pointer()
                .active(|style| style.bg(theme::row_pressed_bg()))
                .on_mouse_up(MouseButton::Left, handler)
        })
        .when(!enabled, |chip| chip.opacity(0.45))
        .child(label.into())
}

/// One action of the terminal selection bar: an icon and a word, inside a
/// target that still clears the touch minimum on its own.
fn terminal_selection_action(
    id: impl Into<gpui::ElementId>,
    icon: &'static str,
    label: &'static str,
    handler: impl Fn(&gpui::MouseUpEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(32.0))
        .min_w(px(64.0))
        .px_2()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_XS))
        .aria_label(label)
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_mouse_up(MouseButton::Left, handler)
        .child(
            svg()
                .path(icon)
                .size(px(theme::ICON_SM))
                .text_color(theme::text_secondary()),
        )
        .child(
            div()
                .text_size(theme::font_caption())
                .text_color(theme::text_primary())
                .child(label),
        )
}

/// The guide rails a tree row draws down its ancestor columns.
///
/// The offsets are measured against the rail's edge, so the row's own inset is
/// subtracted here: a guide has to line up with the chevron above it no matter
/// how far the row's fill is pulled in from the edge.
fn file_tree_guides_mobile(depth: usize) -> Vec<gpui::AnyElement> {
    (0..depth)
        .map(|index| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(index as f32 * 20.0 + 16.0 - TREE_ROW_INSET))
                .w(px(1.0))
                // The same hairline the desktop tree hangs its levels from.
                .bg(theme::border_default().opacity(0.70))
                .into_any_element()
        })
        .collect()
}

/// What a path is, for the purposes of showing it rather than editing it.
///
/// The authority sends a `preview_kind` with every read, but the phone has to
/// know before the answer arrives: an image's bytes are fetched on a different
/// route from its text, and the fetch is started alongside the read.
fn file_preview_kind_for_path(path: &str) -> FilePreviewKind {
    let extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("md" | "markdown" | "mdx") => FilePreviewKind::Markdown,
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif") => {
            FilePreviewKind::Image
        }
        Some("svg") => FilePreviewKind::Image,
        _ => FilePreviewKind::Text,
    }
}

/// The grammar a path is read with, named the way the editor's registry names
/// its languages.
///
/// It mirrors the desktop editor's own table, so the same file is highlighted
/// the same way on both clients; an unknown extension is plain text rather than
/// a guess, because a wrong grammar colors a file into nonsense.
fn file_language_for_path(path: &str) -> &'static str {
    let extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("rs") => "rust",
        Some("ts") => "typescript",
        Some("tsx") => "tsx",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("jsx") => "jsx",
        Some("json" | "jsonl") => "json",
        Some("md" | "mdx" | "markdown") => "markdown",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("py") => "python",
        Some("go") => "go",
        Some("java") => "java",
        Some("css") => "css",
        Some("html" | "htm") => "html",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("sql") => "sql",
        Some("zig") => "zig",
        Some("lua") => "lua",
        Some("kt" | "kts") => "kotlin",
        Some("swift") => "swift",
        Some("rb") => "ruby",
        Some("php") => "php",
        Some("c" | "h") => "c",
        Some("cpp" | "cc" | "hpp" | "cxx") => "cpp",
        Some("cs") => "csharp",
        Some("diff" | "patch") => "diff",
        _ => "text",
    }
}

/// The decoder an image path needs, or `None` when the extension names a
/// format this build cannot draw.
pub(crate) fn image_format_for_path(path: &str) -> Option<ImageFormat> {
    let extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase())?;
    match extension.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// The byte range of one 1-based line, or `None` when the file has no such
/// line. The trailing newline is left out so the selection ends with the text.
fn file_line_range(text: &str, line_number: usize) -> Option<(usize, usize)> {
    if line_number == 0 {
        return None;
    }
    let mut start = 0usize;
    let mut current = 1usize;
    for (index, character) in text.char_indices() {
        if current == line_number && character == '\n' {
            return Some((start, index));
        }
        if character == '\n' {
            current += 1;
            start = index + 1;
        }
    }
    // The last line has no newline after it.
    (current == line_number && start <= text.len()).then_some((start, text.len()))
}

/// What a worktree's readiness state reads as.
fn worktree_readiness_label(state: GitWorktreeReadinessState) -> &'static str {
    match state {
        GitWorktreeReadinessState::Working => locale::text("Working", "开发中", "開發中"),
        GitWorktreeReadinessState::Reviewing => locale::text("In review", "审阅中", "審閱中"),
        GitWorktreeReadinessState::ReadyToMerge => locale::text("Ready", "已就绪", "已就緒"),
        GitWorktreeReadinessState::MergeQueued => locale::text("Queued", "排队中", "排隊中"),
        GitWorktreeReadinessState::MergeRunning => locale::text("Merging", "合并中", "合併中"),
        GitWorktreeReadinessState::Unknown => locale::text("Worktree", "Worktree", "Worktree"),
    }
}

/// The two things that can be done to a merge already in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeOperationAction {
    Abort,
    Continue,
}

/// A lifecycle action waiting for the reader to confirm it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorktreeConfirmation {
    /// The plan a merge would run, shown before it runs.
    Merge(Box<GitWorktreeMergePlan>),
    /// Discarding the worktree, with the preflight's own risks.
    Discard(Box<GitWorktreeDestructivePreflight>),
}

/// The windows of time the history filter offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobileHistoryWindow {
    Today,
    Week,
    Month,
}

impl MobileHistoryWindow {
    const ALL: [Self; 3] = [Self::Today, Self::Week, Self::Month];

    fn label(self) -> &'static str {
        match self {
            Self::Today => locale::text("Today", "今天", "今天"),
            Self::Week => locale::text("Last 7 days", "最近 7 天", "最近 7 天"),
            Self::Month => locale::text("Last 30 days", "最近 30 天", "最近 30 天"),
        }
    }

    fn days(self) -> i64 {
        match self {
            Self::Today => 1,
            Self::Week => 7,
            Self::Month => 30,
        }
    }

    /// The filter bounds this window stands for, given the current time.
    fn bounds(self, now_ms: i64) -> (Option<i64>, Option<i64>) {
        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        (
            Some(now_ms.saturating_sub(self.days() * DAY_MS)),
            Some(now_ms),
        )
    }

    /// The preset a pair of bounds came from, when it is one of them.
    fn from_bounds(after: Option<i64>, before: Option<i64>, now_ms: i64) -> Option<Self> {
        let after = after?;
        before?;
        Self::ALL
            .into_iter()
            .find(|window| window.bounds(now_ms).0 == Some(after))
    }
}

/// A branch name clipped to what a chip can show.
fn short_branch_label(branch: &str, max_chars: usize) -> String {
    if branch.chars().count() <= max_chars {
        return branch.to_string();
    }
    let head: String = branch.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

fn mobile_file_tree_icon(
    icon: FileIconDescriptor,
    ignored: bool,
    expanded: bool,
) -> gpui::AnyElement {
    let path = match icon.kind {
        FileIconKind::Directory => {
            return svg()
                .path(if expanded {
                    "icons/folder-open.svg"
                } else {
                    "icons/folder.svg"
                })
                .size(px(theme::ICON_SM))
                .text_color(mobile_file_icon_color(icon.kind, ignored))
                .into_any_element();
        }
        FileIconKind::Code
        | FileIconKind::Rust
        | FileIconKind::TypeScript
        | FileIconKind::JavaScript => "icons/file-code.svg",
        FileIconKind::Java => "icons/coffee.svg",
        FileIconKind::Json => "icons/file-braces.svg",
        FileIconKind::Archive => "icons/file-archive.svg",
        FileIconKind::Spreadsheet => "icons/file-spreadsheet.svg",
        FileIconKind::Audio => "icons/audio-lines.svg",
        FileIconKind::Video => "icons/file-video-camera.svg",
        FileIconKind::Symlink => "icons/file-symlink.svg",
        FileIconKind::Config => "icons/file-cog.svg",
        FileIconKind::Lock => "icons/file-lock.svg",
        FileIconKind::Secret => "icons/file-key.svg",
        FileIconKind::Font => "icons/file-type.svg",
        FileIconKind::Markdown => "icons/book-open-text.svg",
        FileIconKind::Image => "icons/image.svg",
        FileIconKind::Svg | FileIconKind::Markup => "icons/code-xml.svg",
        FileIconKind::Database => "icons/database.svg",
        FileIconKind::Style => "icons/hash.svg",
        FileIconKind::Script => "icons/file-terminal.svg",
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => "icons/file-text.svg",
    };
    svg()
        .path(path)
        .size(px(theme::ICON_SM))
        .text_color(mobile_file_icon_color(icon.kind, ignored))
        .into_any_element()
}

fn mobile_file_icon_color(kind: FileIconKind, ignored: bool) -> gpui::Hsla {
    let color = match kind {
        FileIconKind::Directory => rgb(0x85899d).into(),
        FileIconKind::Code
        | FileIconKind::Java
        | FileIconKind::Rust
        | FileIconKind::TypeScript
        | FileIconKind::Markdown
        | FileIconKind::Image
        | FileIconKind::Svg => theme::accent_blue(),
        FileIconKind::JavaScript | FileIconKind::Script => theme::accent_yellow(),
        FileIconKind::Json => theme::accent_purple(),
        FileIconKind::Archive | FileIconKind::Config => rgb(0xf0a050).into(),
        FileIconKind::Database | FileIconKind::Spreadsheet => theme::accent_green(),
        FileIconKind::Style => rgb(0x5ed2d9).into(),
        FileIconKind::Markup => rgb(0xf0a050).into(),
        FileIconKind::Audio => rgb(0xd08ad8).into(),
        FileIconKind::Video => rgb(0xf08fc4).into(),
        FileIconKind::Symlink => rgb(0xb091f2).into(),
        FileIconKind::Lock | FileIconKind::Secret => theme::accent_yellow(),
        FileIconKind::Font => rgb(0xd08ad8).into(),
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => theme::text_secondary(),
    };
    if ignored { color.opacity(0.35) } else { color }
}

/// The color a tree row's name carries when git has something to say about it.
///
/// The signals resolve through the same `right-rail-status-*` tokens the
/// desktop tree paints with, so a modified file is the same blue on both
/// clients and the label the row already shows keeps matching its own color.
fn file_tree_row_text_color(row: &vibex_desktop_model::FileExplorerRow) -> gpui::Hsla {
    if row.ignored {
        return theme::text_muted();
    }
    match row.git.map(|git| git.signal) {
        Some(FileGitSignal::Added) => theme::status_added(),
        Some(FileGitSignal::Untracked) => theme::status_untracked(),
        Some(FileGitSignal::Ignored) => theme::text_muted(),
        Some(_) => theme::status_modified(),
        None => theme::text_primary(),
    }
}

fn file_git_signal_color(signal: FileGitSignal) -> gpui::Hsla {
    match signal {
        FileGitSignal::Added => theme::status_added(),
        FileGitSignal::Untracked => theme::status_untracked(),
        FileGitSignal::Modified
        | FileGitSignal::Deleted
        | FileGitSignal::Renamed
        | FileGitSignal::Copied
        | FileGitSignal::Conflicted => theme::status_modified(),
        FileGitSignal::Ignored => theme::text_muted(),
    }
}

/// Same geometry and tones as the desktop `git_selection_indicator`.
/// The checkbox a changed path carries in the Changes tree.
///
/// It is the only thing that reports whether a path is in the commit, because
/// the row itself carries no wash: the desktop draws a selected path exactly
/// this way. An empty box is a hairline outline on the panel; a checked one is
/// a solid plate with the mark on it.
///
/// The mark's colour is set on the glyph itself rather than inherited from the
/// plate. A one-pixel stroke at 10px had to survive being scaled down from a
/// 24px viewBox, and at the alpha the desktop's own indicator uses it faded
/// into the plate; a phone is read at arm's length under whatever light the
/// room has, so the check is drawn at full contrast and a step larger.
fn git_selection_indicator_mobile(state: GitPathSelectionState) -> gpui::AnyElement {
    let selected = state != GitPathSelectionState::Unchecked;
    let mut selected_border = theme::accent_foreground();
    selected_border.a = 0.18;
    let marker_color = theme::accent_foreground();
    div()
        .size(px(16.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(if selected {
            selected_border
        } else {
            theme::border_input()
        })
        .bg(if selected {
            theme::accent()
        } else {
            theme::workbench_bg()
        })
        .when(state == GitPathSelectionState::Checked, |item| {
            item.child(
                svg()
                    .path("icons/check.svg")
                    .size(px(12.0))
                    .flex_none()
                    .text_color(marker_color),
            )
        })
        .when(state == GitPathSelectionState::Indeterminate, |item| {
            item.child(
                svg()
                    .path("icons/minus.svg")
                    .size(px(12.0))
                    .flex_none()
                    .text_color(marker_color),
            )
        })
        .into_any_element()
}

fn file_icon_kind_for_path(path: &str) -> FileIconKind {
    file_icon_descriptor(path, FileEntryKind::File).kind
}

fn git_change_text_color_mobile(change: &GitChange) -> gpui::Hsla {
    match change.kind {
        GitChangeKind::Deleted => theme::text_muted(),
        GitChangeKind::Untracked => theme::status_untracked(),
        GitChangeKind::Added if !change.staged => theme::status_untracked(),
        GitChangeKind::Added => theme::status_added(),
        GitChangeKind::Modified
        | GitChangeKind::Renamed
        | GitChangeKind::Copied
        | GitChangeKind::TypeChanged
        | GitChangeKind::Unmerged
        | GitChangeKind::Unknown => theme::status_modified(),
    }
}

fn git_change_label_mobile(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "A",
        GitChangeKind::Deleted => "D",
        GitChangeKind::Renamed => "R",
        GitChangeKind::Copied => "C",
        GitChangeKind::Untracked => "U",
        GitChangeKind::Unmerged => "!",
        GitChangeKind::Modified | GitChangeKind::TypeChanged | GitChangeKind::Unknown => "M",
    }
}

/// One curve of the commit graph, drawn from the lane it leaves to the lane it
/// arrives at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GitGraphEdge {
    from: usize,
    to: usize,
}

/// One commit's place in the graph: the lane its node sits on, the curves that
/// arrive from the commit above it, and the curves that leave towards the one
/// below.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GitGraphRow {
    lane: usize,
    top_edges: Vec<GitGraphEdge>,
    bottom_edges: Vec<GitGraphEdge>,
}

/// The whole graph for a loaded window of history.
#[derive(Debug, Clone, Default)]
struct GitGraphLayout {
    rows: Vec<GitGraphRow>,
    lane_count: usize,
}

/// How many lanes the graph may draw before it is capped.
///
/// A repository with hundreds of live branches would otherwise spend the whole
/// rail on curves. The desktop caps it at the same number and so does the
/// column width, which is why a deep branch is drawn as a busier graph rather
/// than as a wider one.
const GIT_HISTORY_GRAPH_MAX_LANES: usize = 8;
/// The width one lane occupies, and the height one commit row occupies.
const GIT_HISTORY_GRAPH_LANE_WIDTH: f32 = 12.0;
const GIT_HISTORY_ROW_HEIGHT: f32 = 64.0;

/// The lane colours, in the desktop's own order.
///
/// Two clients reading one branch should not have to relearn which line is
/// which, so the palette is the same list of semantic tones in the same
/// sequence.
fn git_graph_colors() -> [gpui::Hsla; 8] {
    [
        theme::primary(),
        theme::accent_blue(),
        theme::accent_success(),
        theme::accent_yellow(),
        theme::accent_red(),
        theme::accent_foreground(),
        theme::text_muted(),
        theme::text_primary(),
    ]
}

/// Assigns every commit in `commits` a lane, and every parent link a curve.
///
/// This is the desktop's own walk. Lanes are a list of the commit hashes the
/// graph is still waiting to place, ordered from the newest branch down. A
/// commit takes the lane already holding its hash, or the head of the list if
/// nothing is waiting for it; its parents are then inserted just under it, so
/// a merge fans out where the two histories meet and a linear history keeps
/// one lane for its whole length.
fn git_graph_layout(commits: &[GitCommitSummary]) -> GitGraphLayout {
    let mut lanes: Vec<String> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    let mut lane_count = 1;
    for commit in commits {
        let top_lanes = lanes.clone();
        let mut middle_lanes = top_lanes.clone();
        let lane = middle_lanes
            .iter()
            .position(|hash| hash == &commit.hash)
            .filter(|lane| *lane < GIT_HISTORY_GRAPH_MAX_LANES)
            .unwrap_or_else(|| {
                middle_lanes.retain(|hash| hash != &commit.hash);
                middle_lanes.insert(0, commit.hash.clone());
                0
            });
        middle_lanes.truncate(GIT_HISTORY_GRAPH_MAX_LANES);

        let top_edges = top_lanes
            .iter()
            .enumerate()
            .filter(|(from, _)| *from < GIT_HISTORY_GRAPH_MAX_LANES)
            .filter_map(|(from, hash)| {
                middle_lanes
                    .iter()
                    .position(|candidate| candidate == hash)
                    .map(|to| GitGraphEdge { from, to })
            })
            .collect::<Vec<_>>();

        let mut next = middle_lanes.clone();
        next.remove(lane);
        let mut inserted_parents = 0;
        for parent in &commit.parents {
            if next.iter().any(|candidate| candidate == parent) {
                continue;
            }
            let insert_at = lane.saturating_add(inserted_parents).min(next.len());
            next.insert(insert_at, parent.clone());
            inserted_parents += 1;
        }
        next.truncate(GIT_HISTORY_GRAPH_MAX_LANES);

        let mut bottom_edges = middle_lanes
            .iter()
            .enumerate()
            .filter(|(from, _)| *from != lane)
            .filter_map(|(from, hash)| {
                next.iter()
                    .position(|candidate| candidate == hash)
                    .map(|to| GitGraphEdge { from, to })
            })
            .collect::<Vec<_>>();
        bottom_edges.extend(commit.parents.iter().filter_map(|parent| {
            next.iter()
                .position(|candidate| candidate == parent)
                .map(|to| GitGraphEdge { from: lane, to })
        }));
        bottom_edges.dedup();

        lane_count = lane_count
            .max(top_lanes.len())
            .max(middle_lanes.len())
            .max(next.len());
        rows.push(GitGraphRow {
            lane,
            top_edges,
            bottom_edges,
        });
        lanes = next;
    }
    GitGraphLayout { rows, lane_count }
}

/// The lines of a commit message body, without the subject `git show` repeats.
///
/// `git show --pretty=%B` returns the whole message, so its first line is the
/// subject the header already draws, and the blank line that separates the two
/// is what would otherwise open the folded body on an empty row.
fn commit_body_content_lines(body: &str, subject: &str) -> Vec<String> {
    let subject = subject.trim();
    body.split('\n')
        .map(str::to_string)
        .skip_while(|line| line.trim() == subject)
        .skip_while(|line| line.trim().is_empty())
        .collect()
}

fn history_relative_time(authored_at_ms: Option<i64>) -> String {
    let Some(timestamp) = authored_at_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.with_timezone(&chrono::Local))
    else {
        return locale::common("Unknown").to_string();
    };
    let age = chrono::Local::now().signed_duration_since(timestamp);
    let seconds = age.num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        timestamp.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// A secondary button on a sheet or a full-screen card's action bar. It keeps
/// its frame because it stands alone rather than inside a toolbar row, and a
/// lone word with no edge reads as a label instead of a control.
///
/// Every one leads with the glyph its command wears on the desktop, so a
/// confirmation reads as the same action whichever shell drew it.
fn action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    icon: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let color = theme::text_secondary();
    div()
        .id(id)
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_LG))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::workbench_panel_bg())
        .flex()
        .items_center()
        .justify_center()
        .text_size(theme::font_detail())
        .text_color(color)
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .child(
                    svg()
                        .path(icon)
                        .size(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(color),
                )
                .child(locale::common(label)),
        )
}

/// The cell a point falls in, given the geometry the grid was painted at.
///
/// Rows are laid out bottom-aligned under a fixed vertical padding, so the
/// row is measured from the grid's bottom edge. Points outside the grid clamp
/// to the nearest cell, which keeps a drag alive when the finger leaves the
/// surface instead of dropping the selection.
fn terminal_cell_at_point(
    bounds: Bounds<Pixels>,
    rows: u16,
    columns: u16,
    position: Point<Pixels>,
) -> Option<TerminalGridPoint> {
    if rows == 0 || columns == 0 {
        return None;
    }
    let content_bottom = f32::from(bounds.bottom()) - TERMINAL_VERTICAL_PADDING;
    let rows_from_bottom =
        ((content_bottom - f32::from(position.y)) / TERMINAL_CELL_HEIGHT).floor();
    let row = (f32::from(rows) - 1.0 - rows_from_bottom).clamp(0.0, f32::from(rows) - 1.0) as u16;
    let column = ((f32::from(position.x) - f32::from(bounds.left())) / TERMINAL_CELL_WIDTH)
        .floor()
        .clamp(0.0, f32::from(columns) - 1.0) as u16;
    Some(TerminalGridPoint { row, column })
}

/// The centre of a cell, using the same geometry `terminal_cell_at_point`
/// reads back: rows are laid out bottom-aligned above the grid's bottom
/// padding, and columns start at the grid's left edge.
fn terminal_cell_center(
    bounds: Bounds<Pixels>,
    rows: u16,
    point: TerminalGridPoint,
) -> Point<Pixels> {
    let content_bottom = f32::from(bounds.bottom()) - TERMINAL_VERTICAL_PADDING;
    let y = content_bottom - (f32::from(rows) - f32::from(point.row) - 0.5) * TERMINAL_CELL_HEIGHT;
    let x = f32::from(bounds.left()) + (f32::from(point.column) + 0.5) * TERMINAL_CELL_WIDTH;
    Point::new(px(x), px(y))
}

/// Where the two drag handles sit: the start of the first selected cell and
/// the end of the last, both on the selection's own line.
fn terminal_handle_points(
    bounds: Bounds<Pixels>,
    rows: u16,
    start: TerminalGridPoint,
    end: TerminalGridPoint,
) -> (Point<Pixels>, Point<Pixels>) {
    let start_center = terminal_cell_center(bounds, rows, start);
    let end_center = terminal_cell_center(bounds, rows, end);
    (
        Point::new(
            start_center.x - px(TERMINAL_CELL_WIDTH / 2.0),
            start_center.y,
        ),
        Point::new(end_center.x + px(TERMINAL_CELL_WIDTH / 2.0), end_center.y),
    )
}

/// Which handle a touch landed on, if any. The nearer of the two wins, so the
/// handles can be picked apart even when a selection is only one cell wide.
fn terminal_handle_at_point(
    start: Point<Pixels>,
    end: Point<Pixels>,
    position: Point<Pixels>,
    radius: f32,
) -> Option<TerminalSelectionHandle> {
    let distance = |handle: Point<Pixels>| {
        let dx = f32::from(position.x) - f32::from(handle.x);
        let dy = f32::from(position.y) - f32::from(handle.y);
        (dx * dx + dy * dy).sqrt()
    };
    let (start_distance, end_distance) = (distance(start), distance(end));
    if start_distance > radius && end_distance > radius {
        return None;
    }
    // A tie goes to the end handle, which is the one a reader reaches for
    // after selecting forwards.
    if start_distance < end_distance {
        Some(TerminalSelectionHandle::Start)
    } else {
        Some(TerminalSelectionHandle::End)
    }
}

/// The inclusive column range of the non-blank run `column` belongs to. A
/// blank cell selects itself, so a long press on empty space still raises the
/// copy bar instead of doing nothing.
fn terminal_word_range(row: &[bool], column: u16) -> (u16, u16) {
    let Some(last) = row.len().checked_sub(1) else {
        return (column, column);
    };
    let index = usize::from(column).min(last);
    let blank = |index: usize| row.get(index).copied().unwrap_or(true);
    if blank(index) {
        return (column, column);
    }
    let mut start = index;
    while start > 0 && !blank(start - 1) {
        start -= 1;
    }
    let mut end = index;
    while end < last && !blank(end + 1) {
        end += 1;
    }
    (start as u16, end as u16)
}

/// Feeds one polled snapshot into the render model. Chunks already applied
/// are skipped by sequence; a dims change, a sequence rewind (terminal
/// reattached), or a gap (retained ring evicted past the next expected chunk)
/// rebuilds the emulator from the retained window, mirroring the desktop
/// raw-buffer rebuild rules. Returns true when any bytes were applied.
fn terminal_feed_snapshot(
    render: &mut vibex_ui::TerminalRenderModel,
    applied_next_sequence: &mut i64,
    snapshot: &TerminalSnapshot,
) -> bool {
    let dims_changed =
        render.frame.rows != snapshot.session.rows || render.frame.columns != snapshot.session.cols;
    let rewound = snapshot.next_sequence < *applied_next_sequence;
    let gap = snapshot
        .chunks
        .first()
        .is_some_and(|chunk| chunk.sequence > *applied_next_sequence);
    let applied_any = *applied_next_sequence > 0;
    if dims_changed || rewound || gap || !applied_any {
        render.reset(snapshot.session.rows, snapshot.session.cols);
        *applied_next_sequence = 0;
    }
    let mut applied = false;
    for chunk in &snapshot.chunks {
        if chunk.sequence < *applied_next_sequence {
            continue;
        }
        render.apply(chunk.data.as_bytes());
        *applied_next_sequence = chunk.sequence + 1;
        applied = true;
    }
    applied
}

/// How much room the pane is holding for a painted terminal frame, given the
/// rows that frame holds and the height the pane now has.
///
/// `fitted` carries the rows and the pane height they were last seen at. A frame
/// whose row count has not moved was fitted for the height it recorded, so
/// whatever the pane has gained since then is room the rows cannot fill yet and
/// the grid must keep holding open below itself. A pane that lost height instead
/// — the keyboard opening, a rotation, a smaller window — holds nothing: there
/// the rows follow the surface, and the frame's own resize catches up with it.
fn terminal_grid_hold(rows: u16, height: f32, fitted: &mut Option<(u16, f32)>) -> f32 {
    let fit = match *fitted {
        Some((painted, height)) if painted == rows => height,
        _ => {
            *fitted = Some((rows, height));
            height
        }
    };
    (height - fit).max(0.0)
}

/// What a stretch of committed text stands for, in order: each part of the
/// text, and whether a Return came before it.
///
/// The phone's keyboard does not send an Enter *key*. The host's IME target is
/// a multi-line editor, so its Return key commits a newline into the field
/// instead, and the field hands it over as ordinary text. Reading that newline
/// as the key it stands for is what makes a command typed on a phone run: the
/// shell received the line with no terminator and waited, so the reader saw
/// their command echoed and then nothing. A CRLF pair is one Return.
fn terminal_text_segments(typed: &str) -> Vec<(bool, String)> {
    let normalized = typed.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split('\n')
        .enumerate()
        .filter(|(index, segment)| *index > 0 || !segment.is_empty())
        .map(|(index, segment)| (index > 0, segment.to_string()))
        .collect()
}

/// Status colors follow the desktop right-rail treatment: green for live
/// sessions, red for ended ones, yellow for a stale host connection.
/// Whether a terminal is one a tab strip shows and a reader can type into.
///
/// A session that has exited or been closed is kept by the authority — its
/// output is the record of what ran — but it is not a place to work, and the
/// desktop's own list drops it for the same reason.
fn is_live_terminal(session: &vibex_core::TerminalSession) -> bool {
    session.status == TerminalStatus::Running
}

fn terminal_status_color(status: TerminalStatus) -> gpui::Hsla {
    match status {
        TerminalStatus::Running => theme::accent_green(),
        TerminalStatus::Exited | TerminalStatus::Killed => theme::accent_red(),
        TerminalStatus::Stale => theme::accent_yellow(),
    }
}

/// What a terminal tab is called: its place in the strip, one-based.
///
/// The authority names every session this client asks for the same thing, so
/// the session's own title is the same word on every tab and says nothing about
/// which one the reader is looking at. The number is what a reader counts by,
/// and the desktop numbers its preview terminals the same way.
fn terminal_tab_label(index: usize) -> gpui::SharedString {
    let number = index.saturating_add(1);
    locale::text("Terminal {}", "终端 {}", "終端機 {}")
        .replace("{}", &number.to_string())
        .into()
}

/// The blank cell used wherever the host frame has no explicit cell, matching
/// the desktop surface defaults (default foreground on default background).
fn empty_terminal_cell(point: TerminalGridPoint) -> TerminalCellSnapshot {
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

/// One report of a mouse gesture, as an application that asked for mouse
/// reporting understands it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TerminalMouseReport {
    Press,
    Release,
    /// A step of a drag. The button bit stays set, which is what tells the
    /// application the finger is still down.
    Move,
}

/// One mouse report in the encoding the application negotiated.
///
/// SGR carries the cell in decimal and names the kind in the final byte, with
/// `m` for a release; the older X10 form packs the kind and the cell into
/// three bytes that reach past ASCII. Both mirror the desktop's shared
/// encoder, including the X10 form's 223-column limit, past which there is no
/// way to name the cell at all.
fn terminal_mouse_report(
    kind: TerminalMouseReport,
    point: TerminalGridPoint,
    sgr: bool,
) -> Option<TerminalInput> {
    let code: u8 = match kind {
        TerminalMouseReport::Press => 0,
        TerminalMouseReport::Release => 3,
        // A drag is reported with the button still held: button 0 plus the
        // motion bit.
        TerminalMouseReport::Move => 32,
    };
    let column = u32::from(point.column) + 1;
    let row = u32::from(point.row) + 1;
    if sgr {
        let final_byte = if kind == TerminalMouseReport::Release {
            'm'
        } else {
            'M'
        };
        return Some(TerminalInput::Text(format!(
            "\u{1b}[<{code};{column};{row}{final_byte}"
        )));
    }
    if column > 223 || row > 223 {
        return None;
    }
    Some(TerminalInput::Bytes(vec![
        0x1b,
        b'[',
        b'M',
        code + 32,
        column as u8 + 32,
        row as u8 + 32,
    ]))
}

/// The press and the release one tap sends, as the single input the write path
/// carries so the two can never be reordered against each other.
fn terminal_mouse_tap(point: TerminalGridPoint, sgr: bool) -> Option<TerminalInput> {
    let press = terminal_mouse_report(TerminalMouseReport::Press, point, sgr)?;
    let release = terminal_mouse_report(TerminalMouseReport::Release, point, sgr)?;
    Some(match (press, release) {
        (TerminalInput::Text(mut press), TerminalInput::Text(release)) => {
            press.push_str(&release);
            TerminalInput::Text(press)
        }
        (TerminalInput::Bytes(mut press), TerminalInput::Bytes(release)) => {
            press.extend(release);
            TerminalInput::Bytes(press)
        }
        // Both reports of one tap are asked for in the same encoding, so this
        // arm is unreachable; dropping the tap beats sending half of it.
        _ => return None,
    })
}

/// Resolves a terminal cell color against the current theme, mirroring the
/// desktop `terminal_color` mapping (RGB passthrough, 256-color palette, and
/// the emulator's named defaults).
fn terminal_cell_color(
    color: TerminalCellColor,
    default_foreground: gpui::Hsla,
    default_background: gpui::Hsla,
    fallback: gpui::Hsla,
) -> gpui::Hsla {
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

fn indexed_terminal_color(index: u8) -> gpui::Hsla {
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

fn flatten_join<T>(outcome: Result<BackendResult<T>, gpui_tokio::JoinError>) -> BackendResult<T> {
    outcome.unwrap_or_else(|_| Err(background_task_error()))
}

fn background_task_error() -> BackendError {
    BackendError::failed(
        "mobile_workbench_task_failed",
        locale::text(
            "A mobile workspace task stopped unexpectedly.",
            "移动端工作区任务意外停止。",
            "行動端工作區工作意外停止。",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{TerminalOutputChunk, TerminalSession};

    fn fixture_terminal_session(rows: u16, cols: u16) -> TerminalSession {
        TerminalSession {
            id: TerminalId::new(),
            workspace_id: WorkspaceId::new(),
            title: "Fixture".into(),
            shell: "/bin/sh".into(),
            cwd: "/fixture".into(),
            rows,
            cols,
            status: TerminalStatus::Running,
            created_at_ms: 1,
            updated_at_ms: 1,
            closed_at_ms: None,
        }
    }

    /// Assembles the visible screen text row by row for assertions.
    fn terminal_visible_text(render: &vibex_ui::TerminalRenderModel) -> String {
        let frame = &render.frame;
        let mut rows = vec![String::new(); usize::from(frame.rows)];
        for cell in &frame.cells {
            let Some(row) = rows.get_mut(usize::from(cell.row)) else {
                continue;
            };
            while row.chars().count() < usize::from(cell.column) {
                row.push(' ');
            }
            row.push_str(&cell.text);
        }
        rows.iter()
            .map(|row| row.trim_end().to_string())
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn terminal_feed_snapshot_applies_incrementally_and_rebuilds_on_gaps() {
        let terminal_id = TerminalId::new();
        let session = fixture_terminal_session(4, 12);
        let chunk = |sequence: i64, data: &str| TerminalOutputChunk {
            terminal_id: terminal_id.clone(),
            sequence,
            data: data.to_string(),
            timestamp_ms: 0,
        };
        let mut render = vibex_ui::TerminalRenderModel::new(4, 12);
        let mut applied = 0;

        // The first snapshot rebuilds from the retained window.
        let first = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "hello"), chunk(2, " world")],
            next_sequence: 3,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &first));
        assert_eq!(applied, 3);
        assert_eq!(terminal_visible_text(&render), "hello world");

        // No new output applies nothing and keeps the screen intact.
        let idle = TerminalSnapshot {
            session: session.clone(),
            chunks: first.chunks.clone(),
            next_sequence: 3,
        };
        assert!(!terminal_feed_snapshot(&mut render, &mut applied, &idle));
        assert_eq!(terminal_visible_text(&render), "hello world");

        // An incremental chunk extends the screen instead of duplicating it.
        let incremental = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "hello"), chunk(2, " world"), chunk(3, "\r\nnext")],
            next_sequence: 4,
        };
        assert!(terminal_feed_snapshot(
            &mut render,
            &mut applied,
            &incremental
        ));
        assert_eq!(applied, 4);
        assert_eq!(terminal_visible_text(&render), "hello world\nnext");

        // A sequence rewind (reattached terminal) rebuilds from scratch.
        let rewound = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "fresh")],
            next_sequence: 2,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &rewound));
        assert_eq!(applied, 2);
        assert_eq!(terminal_visible_text(&render), "fresh");

        // A gap (ring eviction past the next expected chunk) rebuilds from the
        // retained window only.
        let gapped = TerminalSnapshot {
            session,
            chunks: vec![chunk(9, "tail")],
            next_sequence: 10,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &gapped));
        assert_eq!(applied, 10);
        assert_eq!(terminal_visible_text(&render), "tail");
    }

    /// The commit graph's lanes have to come from the commits' own parents, or
    /// the left column is decoration. These are the desktop's own expectations
    /// run against this client's copy of the walk: a straight history stays on
    /// one lane, a merge fans out to the branch it merged, and a merge wider
    /// than the cap is drawn truncated rather than off the edge.
    #[test]
    fn the_commit_graph_tracks_linear_forks_and_merges() {
        let commit = |hash: &str, parents: &[&str]| GitCommitSummary {
            hash: hash.to_string(),
            short_hash: hash.to_string(),
            parents: parents.iter().map(|parent| (*parent).to_string()).collect(),
            author_name: "Ada".into(),
            author_email: "ada@example.test".into(),
            authored_at_ms: None,
            subject: hash.to_string(),
            refs: Vec::new(),
        };

        let linear =
            git_graph_layout(&[commit("a", &["b"]), commit("b", &["c"]), commit("c", &[])]);
        assert_eq!(linear.lane_count, 1);
        assert!(linear.rows[0].top_edges.is_empty());
        assert_eq!(
            linear.rows[0].bottom_edges,
            [GitGraphEdge { from: 0, to: 0 }]
        );
        assert_eq!(linear.rows[1].top_edges, [GitGraphEdge { from: 0, to: 0 }]);

        let merged = git_graph_layout(&[
            commit("merge", &["left", "right"]),
            commit("left", &["base"]),
            commit("right", &["base"]),
            commit("base", &[]),
        ]);
        assert_eq!(merged.lane_count, 2);
        assert_eq!(
            merged.rows[0].bottom_edges,
            [
                GitGraphEdge { from: 0, to: 0 },
                GitGraphEdge { from: 0, to: 1 },
            ]
        );
        assert_eq!(merged.rows[1].lane, 0);
        assert_eq!(merged.rows[2].lane, 1);
        assert!(
            merged.rows[2]
                .bottom_edges
                .contains(&GitGraphEdge { from: 1, to: 0 })
        );
        assert_eq!(merged.rows[3].lane, 0);

        let wide_merge = std::iter::once(commit(
            "wide",
            &["p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9"],
        ))
        .chain((0..10).map(|index| commit(&format!("p{index}"), &[])))
        .collect::<Vec<_>>();
        let capped = git_graph_layout(&wide_merge);
        assert_eq!(capped.lane_count, GIT_HISTORY_GRAPH_MAX_LANES);
        assert!(capped.rows.iter().all(|row| {
            row.lane < GIT_HISTORY_GRAPH_MAX_LANES
                && row.top_edges.iter().all(|edge| {
                    edge.from < GIT_HISTORY_GRAPH_MAX_LANES && edge.to < GIT_HISTORY_GRAPH_MAX_LANES
                })
                && row.bottom_edges.iter().all(|edge| {
                    edge.from < GIT_HISTORY_GRAPH_MAX_LANES && edge.to < GIT_HISTORY_GRAPH_MAX_LANES
                })
        }));
    }

    /// A patch is read once and painted twice — the working-tree sheet and a
    /// commit's patch — so the parse has to give both the same answer: the hunk
    /// header restarts the gutter, the marker is stripped from the text and
    /// carried separately, the envelope lines are dropped, and each file's rows
    /// stay together so the commit screen can fold them under their own bar.
    #[test]
    fn a_unified_diff_parses_into_numbered_rows_per_file() {
        let patch = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
diff --git a/docs/guide.md b/docs/guide.md
index 3333333..4444444 100644
--- a/docs/guide.md
+++ b/docs/guide.md
@@ -1,1 +1,1 @@
-old
+new
";
        let files = parse_patch_files(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[1].path, "docs/guide.md");

        // The envelope is read and dropped: a file's first row is its hunk
        // header, never a `diff --git` or an `index` line.
        let rows = &files[0].rows;
        assert_eq!(rows[0].kind, DiffLineKind::Hunk);
        assert!(rows[0].line.is_none());
        assert_eq!(rows.len(), 5);

        // The hunk starts at 10 on the new side, so the first context line is
        // line 10; the deletion takes its number from the old side.
        assert_eq!(rows[1].kind, DiffLineKind::Context);
        assert_eq!(rows[1].line, Some(10));
        assert_eq!(rows[1].content, "let a = 1;");

        assert_eq!(rows[2].kind, DiffLineKind::Removed);
        assert_eq!(rows[2].line, Some(11));
        assert_eq!(rows[2].content, "let b = 2;");

        assert_eq!(rows[3].kind, DiffLineKind::Added);
        assert_eq!(rows[3].line, Some(11));
        assert_eq!(rows[3].content, "let b = 3;");

        assert_eq!(rows[4].kind, DiffLineKind::Added);
        assert_eq!(rows[4].line, Some(12));

        // Every file's gutter restarts at its own hunk header.
        let second = &files[1].rows;
        assert_eq!(second[0].kind, DiffLineKind::Hunk);
        assert_eq!(second[1].kind, DiffLineKind::Removed);
        assert_eq!(second[1].line, Some(1));
        assert_eq!(second[2].kind, DiffLineKind::Added);
        assert_eq!(second[2].line, Some(1));

        // The marker is what a reader copies, so it is drawn rather than kept
        // in the text.
        assert_eq!(rows[3].prefix(), "+");
        assert_eq!(rows[2].prefix(), "-");
        assert_eq!(rows[1].prefix(), " ");

        // The flat walk is the same rows in the same order.
        let (flat, remaining) = parse_unified_diff(patch, 100);
        assert_eq!(remaining, 0);
        assert_eq!(
            flat.len(),
            files.iter().map(|file| file.rows.len()).sum::<usize>()
        );

        // A single file's diff arrives without a `diff --git` header of its
        // own, and still opens a group.
        let bare = parse_patch_files("@@ -1,1 +1,1 @@\n-a\n+b\n");
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].rows.len(), 3);

        // A page that runs out reports how much is left rather than dropping it.
        // The patch is sixteen lines, five of which fit on this page.
        let (short, remaining) = parse_unified_diff(patch, 5);
        assert_eq!(short.len(), 5);
        assert_eq!(remaining, 11);
    }

    /// A tap inside an application that asked for mouse reporting has to arrive
    /// in the encoding that application negotiated, and the older form has to
    /// give up rather than send a cell it cannot name.
    #[test]
    fn a_terminal_tap_is_encoded_the_way_the_application_asked_for() {
        let point = TerminalGridPoint { row: 4, column: 9 };

        assert_eq!(
            terminal_mouse_tap(point, true),
            Some(TerminalInput::Text(
                "\u{1b}[<0;10;5M\u{1b}[<3;10;5m".to_string()
            ))
        );
        // The legacy form has no decimal escape, so its press and release are
        // three bytes each: the button code packed with the cell, both past
        // ASCII for anything but the first few columns.
        assert_eq!(
            terminal_mouse_tap(point, false),
            Some(TerminalInput::Bytes(vec![
                0x1b,
                b'[',
                b'M',
                32,
                10 + 32,
                5 + 32,
                0x1b,
                b'[',
                b'M',
                3 + 32,
                10 + 32,
                5 + 32,
            ]))
        );

        // The X10 report packs the cell into one byte, so it stops at 223.
        let beyond = TerminalGridPoint {
            row: 0,
            column: 223,
        };
        assert!(terminal_mouse_tap(beyond, false).is_none());
        assert!(terminal_mouse_tap(beyond, true).is_some());
    }

    /// A drag reports the button as still held, which is what the motion bit
    /// in the button code means.
    #[test]
    fn a_terminal_drag_step_reports_the_button_as_held() {
        let point = TerminalGridPoint { row: 0, column: 0 };

        assert_eq!(
            terminal_mouse_report(TerminalMouseReport::Move, point, true),
            Some(TerminalInput::Text("\u{1b}[<32;1;1M".to_string()))
        );
        assert_eq!(
            terminal_mouse_report(TerminalMouseReport::Move, point, false),
            Some(TerminalInput::Bytes(vec![
                0x1b,
                b'[',
                b'M',
                32 + 32,
                1 + 32,
                1 + 32,
            ]))
        );
        assert_eq!(
            terminal_mouse_report(TerminalMouseReport::Release, point, true),
            Some(TerminalInput::Text("\u{1b}[<3;1;1m".to_string()))
        );
    }

    /// Deleting must reach the shell from any state the sink can be in.
    ///
    /// A Backspace is the sink getting shorter, and the diff is what turns
    /// that into DEL bytes. It has to work with the seed the field is put back
    /// on between keystrokes, with an IME's committed run, and with a partial
    /// delete after one — the states the field passes through, rather than
    /// only the empty one it used to be emptied to.
    #[test]
    fn a_backspace_reaches_the_shell_from_the_sink_seed() {
        // The seed is one character, so the first key deletes one.
        assert_eq!(
            terminal_sink_edit(TERMINAL_SINK_SEED, ""),
            TerminalSinkEdit::Deleted(1)
        );
        // Typing extends the seed and is forwarded as the character typed.
        assert_eq!(
            terminal_sink_edit(TERMINAL_SINK_SEED, " l"),
            TerminalSinkEdit::Typed("l".to_string())
        );
        // A committed run, then the Backspace that eats into it.
        assert_eq!(
            terminal_sink_edit(" ls", " l"),
            TerminalSinkEdit::Deleted(1)
        );
        assert_eq!(terminal_sink_edit(" ls", " "), TerminalSinkEdit::Deleted(2));
        // A selection replaced by nothing deletes every character it covered.
        assert_eq!(
            terminal_sink_edit("git status", "git "),
            TerminalSinkEdit::Deleted(6)
        );
        // An edit that is neither an append nor a truncation — an IME
        // rewriting the buffer — is dropped rather than guessed at.
        assert_eq!(terminal_sink_edit("abc", "abd"), TerminalSinkEdit::Ignored);
        assert_eq!(terminal_sink_edit("abc", "abc"), TerminalSinkEdit::Ignored);
    }

    /// A phone's Return key arrives as a newline in the sink, not as a key
    /// event, so the split has to read it back as the Enter it stands for.
    #[test]
    fn a_committed_newline_stands_for_the_return_key() {
        // A command and its Return, which is one text run and one Enter.
        assert_eq!(
            terminal_text_segments("ls\n"),
            [(false, "ls".to_string()), (true, String::new())]
        );
        // The Return on its own: what the sink holds when the reader presses
        // Enter with the field already emptied by the last forwarded character.
        assert_eq!(terminal_text_segments("\n"), [(true, String::new())]);
        // A CRLF pair is one Return, not two.
        assert_eq!(terminal_text_segments("\r\n"), [(true, String::new())]);
        assert_eq!(
            terminal_text_segments("a\r\nb"),
            [(false, "a".to_string()), (true, "b".to_string())]
        );
        // A bare CR is a Return on its own.
        assert_eq!(
            terminal_text_segments("a\rb"),
            [(false, "a".to_string()), (true, "b".to_string())]
        );
        // A run of Returns is a run of Enters.
        assert_eq!(
            terminal_text_segments("\n\n"),
            [(true, String::new()), (true, String::new())]
        );
        // Ordinary text is left alone.
        assert_eq!(
            terminal_text_segments("git status"),
            [(false, "git status".to_string())]
        );
        // Nothing typed is nothing sent.
        assert!(terminal_text_segments("").is_empty());
    }

    /// A terminal is a tab while it is running, and stops being one the moment
    /// it is not: the authority keeps a closed session so its output can still
    /// be read, and a strip drawn from that list kept a tab for a terminal the
    /// reader had already closed.
    #[test]
    fn only_a_running_terminal_is_a_tab() {
        let session = |status| TerminalSession {
            status,
            ..fixture_terminal_session(24, 80)
        };
        assert!(is_live_terminal(&session(TerminalStatus::Running)));
        assert!(!is_live_terminal(&session(TerminalStatus::Exited)));
        assert!(!is_live_terminal(&session(TerminalStatus::Killed)));
        assert!(!is_live_terminal(&session(TerminalStatus::Stale)));
    }

    /// The selection's handles sit on the first and last selected cells, in
    /// reading order, and a touch takes hold of the nearer one.
    #[test]
    fn terminal_handles_follow_the_selection_and_the_nearer_touch() {
        use gpui::{point, size};

        let bounds = Bounds::new(point(px(6.0), px(100.0)), size(px(80.0), px(200.0)));
        let start = TerminalGridPoint { row: 9, column: 0 };
        let end = TerminalGridPoint { row: 8, column: 3 };
        let (start_point, end_point) = terminal_handle_points(bounds, 10, start, end);

        // The last row is the one above the grid's bottom padding, so its
        // centre is half a cell up from there.
        assert_eq!(
            f32::from(start_point.y),
            300.0 - TERMINAL_VERTICAL_PADDING - TERMINAL_CELL_HEIGHT / 2.0
        );
        assert_eq!(f32::from(start_point.x), 6.0);
        assert_eq!(f32::from(end_point.x), 6.0 + 4.0 * TERMINAL_CELL_WIDTH);

        let radius = TERMINAL_HANDLE_TOUCH_RADIUS;
        assert_eq!(
            terminal_handle_at_point(start_point, end_point, start_point, radius),
            Some(TerminalSelectionHandle::Start)
        );
        assert_eq!(
            terminal_handle_at_point(start_point, end_point, end_point, radius),
            Some(TerminalSelectionHandle::End)
        );
        // A touch between the two handles is nearer neither, and the end
        // handle wins the tie. A touch past both is not a handle at all. Both
        // handles share a line here so the tie is exactly a tie.
        let (line_start, line_end) = terminal_handle_points(
            bounds,
            10,
            TerminalGridPoint { row: 8, column: 0 },
            TerminalGridPoint { row: 8, column: 3 },
        );
        let between = point(
            px((f32::from(line_start.x) + f32::from(line_end.x)) / 2.0),
            line_start.y,
        );
        assert_eq!(
            terminal_handle_at_point(line_start, line_end, between, radius),
            Some(TerminalSelectionHandle::End)
        );
        let far = point(line_end.x + px(radius + 1.0), line_end.y);
        assert_eq!(
            terminal_handle_at_point(line_start, line_end, far, radius),
            None
        );
    }

    /// The selection's ends are ordered, so the handles are placed on the
    /// first and last cell no matter which way the reader selected.
    #[test]
    fn terminal_selection_ends_read_in_order() {
        let first = TerminalGridPoint { row: 1, column: 2 };
        let last = TerminalGridPoint { row: 1, column: 7 };

        assert_eq!(
            TerminalSelection {
                anchor: first,
                head: last
            }
            .ends(),
            (first, last)
        );
        assert_eq!(
            TerminalSelection {
                anchor: last,
                head: first
            }
            .ends(),
            (first, last)
        );
    }

    #[test]
    fn terminal_cell_colors_follow_desktop_named_defaults() {
        let foreground = rgb(0xfafafa).into();
        let background = rgb(0x09090b).into();
        assert_eq!(
            terminal_cell_color(
                TerminalCellColor::Named { index: 256 },
                foreground,
                background,
                foreground,
            ),
            foreground
        );
        assert_eq!(
            terminal_cell_color(
                TerminalCellColor::Named { index: 257 },
                foreground,
                background,
                foreground,
            ),
            background
        );
        assert_ne!(
            terminal_cell_color(
                TerminalCellColor::Indexed { index: 1 },
                foreground,
                background,
                foreground,
            ),
            foreground
        );
    }

    /// Rows are bottom-aligned under the grid's vertical padding, so a point's
    /// row is measured from that edge, and a point outside the grid clamps to
    /// the nearest cell instead of dropping the selection.
    #[test]
    fn terminal_cell_at_point_reads_the_painted_geometry() {
        use gpui::{point, size};

        let bounds = Bounds::new(point(px(6.0), px(100.0)), size(px(80.0), px(200.0)));
        let cell = |x: f32, y: f32| {
            terminal_cell_at_point(bounds, 10, 10, point(px(x), px(y))).expect("the grid has cells")
        };

        // The bottom padding is not a row: the last row ends above it.
        assert_eq!(cell(6.0, 295.0), TerminalGridPoint { row: 9, column: 0 });
        assert_eq!(cell(6.0, 278.0), TerminalGridPoint { row: 8, column: 0 });
        // A point past the top is the first row, not a negative one.
        assert_eq!(cell(6.0, 100.0), TerminalGridPoint { row: 0, column: 0 });
        // Columns advance by the cell width and clamp at both ends.
        assert_eq!(cell(13.0, 295.0).column, 0);
        assert_eq!(cell(14.0, 295.0).column, 1);
        assert_eq!(cell(0.0, 295.0).column, 0);
        assert_eq!(cell(500.0, 295.0).column, 9);

        assert!(terminal_cell_at_point(bounds, 0, 10, point(px(6.0), px(295.0))).is_none());
    }

    /// A keyboard that closes gives the pane its room back before the runtime
    /// has resized the PTY, and a pane that grew ahead of its rows is what used
    /// to drag the whole terminal down and spring it back. The frame's row count
    /// is the only thing that says when the rows are current, so the room is
    /// held open below the grid until it moves.
    #[test]
    fn terminal_grid_holds_the_keyboard_s_room_until_the_rows_arrive() {
        let mut fitted = None;
        // Sixteen rows and the grid's padding fill the 296px pane the keyboard
        // leaves, so there is nothing to hold.
        assert_eq!(terminal_grid_hold(16, 296.0, &mut fitted), 0.0);
        // The keyboard closes and the pane grows to 576px, but the rows that
        // fill it are still a PTY round trip away: the 280px it gained are held
        // open below the grid instead of dragging it down.
        assert_eq!(terminal_grid_hold(16, 576.0, &mut fitted), 280.0);
        // The resized frame lands at the pane it now fills (thirty-one rows),
        // and the hold is given back in the same frame.
        assert_eq!(terminal_grid_hold(31, 576.0, &mut fitted), 0.0);
        // Opening the keyboard takes room away rather than giving it: the rows
        // follow the surface down, and the frame's own resize catches up with
        // it, exactly as it did before the hold existed.
        assert_eq!(terminal_grid_hold(31, 296.0, &mut fitted), 0.0);
        assert_eq!(terminal_grid_hold(16, 296.0, &mut fitted), 0.0);
        // A pane that shrinks for another reason — a rotation, a smaller window
        // — never holds room it does not have.
        assert_eq!(terminal_grid_hold(16, 200.0, &mut fitted), 0.0);

        // A runtime that never answers the resize leaves the rows where the
        // reader was reading them instead of dropping them by the keyboard.
        let mut unanswered = None;
        assert_eq!(terminal_grid_hold(16, 296.0, &mut unanswered), 0.0);
        assert_eq!(terminal_grid_hold(16, 576.0, &mut unanswered), 280.0);
    }

    /// A long press selects the non-blank run around the cell it landed on, and
    /// a blank cell selects only itself.
    #[test]
    fn terminal_word_range_selects_the_run_around_a_cell() {
        // `true` is a blank cell: three runs, the last of them against the edge.
        let row = [
            true, false, false, false, true, false, false, true, false, false,
        ];

        assert_eq!(terminal_word_range(&row, 2), (1, 3));
        assert_eq!(terminal_word_range(&row, 1), (1, 3));
        assert_eq!(terminal_word_range(&row, 3), (1, 3));
        assert_eq!(terminal_word_range(&row, 0), (0, 0));
        assert_eq!(terminal_word_range(&row, 5), (5, 6));
        assert_eq!(terminal_word_range(&row, 9), (8, 9));
        // A point past the row's own width clamps into it rather than panicking.
        assert_eq!(terminal_word_range(&row, 40), (8, 9));
        assert_eq!(terminal_word_range(&[], 3), (3, 3));
    }

    #[test]
    fn a_history_window_round_trips_through_the_bounds_it_sets() {
        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let now = 1_700_000_000_000i64;
        for window in MobileHistoryWindow::ALL {
            let (after, before) = window.bounds(now);
            assert_eq!(before, Some(now));
            assert_eq!(after, Some(now - window.days() * DAY_MS));
            assert_eq!(
                MobileHistoryWindow::from_bounds(after, before, now),
                Some(window)
            );
        }
        // No bounds at all is "any date", which is not a preset.
        assert_eq!(MobileHistoryWindow::from_bounds(None, None, now), None);
        // A window that is not one of the presets keeps its own bounds and
        // simply reads as unset in the chip.
        assert_eq!(
            MobileHistoryWindow::from_bounds(Some(now - 3 * DAY_MS), Some(now), now),
            None
        );
    }

    #[test]
    fn a_line_number_resolves_to_the_line_the_reader_counted() {
        let text = "one\ntwo\nthree";
        assert_eq!(file_line_range(text, 1), Some((0, 3)));
        assert_eq!(file_line_range(text, 2), Some((4, 7)));
        // The last line has no newline to end it.
        assert_eq!(file_line_range(text, 3), Some((8, 13)));
        assert_eq!(file_line_range(text, 4), None);
        assert_eq!(file_line_range(text, 0), None);
        // An empty file still has a first line to land on.
        assert_eq!(file_line_range("", 1), Some((0, 0)));
        assert_eq!(file_line_range("", 2), None);
        // A blank line between two others is a line, not a gap.
        assert_eq!(file_line_range("a\n\nb", 2), Some((2, 2)));
    }

    #[test]
    fn a_file_preview_kind_follows_the_extension_it_was_opened_with() {
        assert_eq!(
            file_preview_kind_for_path("docs/README.md"),
            FilePreviewKind::Markdown
        );
        assert_eq!(
            file_preview_kind_for_path("notes/deep/plan.MARKDOWN"),
            FilePreviewKind::Markdown
        );
        assert_eq!(
            file_preview_kind_for_path("shots/screen.png"),
            FilePreviewKind::Image
        );
        assert_eq!(
            file_preview_kind_for_path("icons/logo.svg"),
            FilePreviewKind::Image
        );
        // No extension, or one the viewer has no second face for.
        assert_eq!(
            file_preview_kind_for_path("Makefile"),
            FilePreviewKind::Text
        );
        assert_eq!(
            file_preview_kind_for_path("src/main.rs"),
            FilePreviewKind::Text
        );
        // A dot in a directory name must not be read as the file's extension.
        assert_eq!(
            file_preview_kind_for_path("v1.2/notes"),
            FilePreviewKind::Text
        );
    }

    #[test]
    fn only_formats_this_build_can_draw_get_a_decoder() {
        assert_eq!(image_format_for_path("a/b.png"), Some(ImageFormat::Png));
        assert_eq!(image_format_for_path("a/b.JPG"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_for_path("a/b.jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format_for_path("a/b.gif"), Some(ImageFormat::Gif));
        assert_eq!(image_format_for_path("a/b.webp"), Some(ImageFormat::Webp));
        assert_eq!(image_format_for_path("a/b.svg"), Some(ImageFormat::Svg));
        assert_eq!(image_format_for_path("a/b.tiff"), None);
        assert_eq!(image_format_for_path("a/b"), None);
    }

    #[test]
    fn a_branch_label_is_clipped_rather_than_overflowing_its_chip() {
        assert_eq!(short_branch_label("main", 18), "main");
        assert_eq!(
            short_branch_label("feature/a-very-long-branch-name", 10),
            "feature/a…"
        );
        // Multi-byte names are counted in characters, not bytes.
        assert_eq!(short_branch_label("分支名称很长很长很长", 5), "分支名称…");
    }

    #[test]
    fn mobile_workbench_file_and_git_mappings_follow_desktop_labels() {
        // The control shows its own value and swaps it on a tap, so the pair it
        // flips between is the contract between what the row offers and what
        // the query can mean.
        assert_eq!(
            MobileFileSearchMode::Name.toggled(),
            MobileFileSearchMode::Content
        );
        assert_eq!(
            MobileFileSearchMode::Content.toggled(),
            MobileFileSearchMode::Name,
            "the toggle is its own inverse: two states, one tap apart"
        );
        assert_ne!(
            MobileFileSearchMode::Name.toggle_label(),
            MobileFileSearchMode::Content.toggle_label(),
            "a tap that announces the same thing in both states says nothing"
        );
        assert_ne!(
            MobileFileSearchMode::Name.label(),
            MobileFileSearchMode::Content.label()
        );
        assert_eq!(git_change_label_mobile(GitChangeKind::Added), "A");
        assert_eq!(git_change_label_mobile(GitChangeKind::Deleted), "D");
        assert_eq!(git_change_label_mobile(GitChangeKind::Untracked), "U");
        assert_eq!(git_change_label_mobile(GitChangeKind::Unmerged), "!");
        assert_eq!(file_icon_kind_for_path("src/main.rs"), FileIconKind::Rust);
        assert_eq!(file_icon_kind_for_path("README.md"), FileIconKind::Markdown);
        assert_eq!(file_icon_kind_for_path("config.toml"), FileIconKind::Config);
    }

    /// The viewer's grammar table is the desktop editor's, name for name.
    ///
    /// The two clients read the same file, so the same buffer has to be
    /// highlighted by the same parser; a name the registry does not know would
    /// quietly render a source file as plain text on the phone only.
    #[test]
    fn the_file_viewer_names_languages_the_way_the_desktop_does() {
        assert_eq!(file_language_for_path("crates/core/src/lib.rs"), "rust");
        assert_eq!(
            file_language_for_path("apps/mobile/src/app.ts"),
            "typescript"
        );
        assert_eq!(file_language_for_path("src/App.tsx"), "tsx");
        assert_eq!(file_language_for_path("scripts/build.mjs"), "javascript");
        assert_eq!(file_language_for_path("package.json"), "json");
        assert_eq!(file_language_for_path("README.md"), "markdown");
        assert_eq!(file_language_for_path("Cargo.toml"), "toml");
        assert_eq!(file_language_for_path(".github/workflows/ci.yml"), "yaml");
        assert_eq!(file_language_for_path("tools/run.sh"), "bash");
        // Everything else is plain text rather than a guess: a wrong grammar
        // colors a file into nonsense, and an unknown extension is not a hint.
        assert_eq!(file_language_for_path("LICENSE"), "text");
        assert_eq!(file_language_for_path("assets/logo.bin"), "text");
    }

    /// The rail owns its whole header, and exactly one control closes it.
    ///
    /// The drawer used to draw a second header above the rail's own tabs. It
    /// spent a phone's row on repeating the title already selected below it,
    /// and it left two `close-right-rail` ids in the tree, which is how the
    /// duplicate survived review: both looked like the one way out.
    #[test]
    fn the_right_rail_draws_its_own_header() {
        let drawer = include_str!("app.rs")
            .split_once("fn render_workbench_drawer(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_drawer("))
            .map(|(body, _)| body)
            .expect("the workbench drawer should remain inspectable");
        assert!(
            !drawer.contains("close-right-rail"),
            "the drawer must not draw a second header over the rail's own"
        );
        assert!(
            !drawer.contains("DRAWER_HEADER_HEIGHT"),
            "the drawer must not reserve a header row of its own"
        );

        let workbench = include_str!("workbench.rs");
        assert!(workbench.contains("\"close-right-rail\""));
        assert!(workbench.contains("fn render_header("));
    }

    /// A sheet is drawn by the surface whose control opens it.
    ///
    /// The go-to-line sheet lived at the foot of the Git surface's render, so
    /// its glyph in the file header set the flag and painted nothing; the
    /// sheet only appeared once the reader switched surfaces, over a page that
    /// had never asked for it. Both halves are asserted here — who draws it,
    /// and who takes it down — because either one alone leaves the sheet
    /// reachable from a surface it does not belong to.
    #[test]
    fn the_files_surface_draws_and_drops_its_own_sheet() {
        let source = include_str!("workbench.rs");
        let body = |name: &str| {
            let start = ["\n    fn ", "\n    pub fn "]
                .iter()
                .filter_map(|prefix| source.find(&format!("{prefix}{name}(")))
                .min()
                .unwrap_or_else(|| panic!("{name} should remain inspectable"));
            let tail = &source[start + 1..];
            let end = ["\n    fn ", "\n    pub fn "]
                .iter()
                .filter_map(|prefix| tail.find(prefix))
                .min()
                .unwrap_or(tail.len());
            &tail[..end]
        };

        assert!(
            body("render_file_screen").contains("render_file_goto_prompt")
                && body("render_file_screen").contains("SheetKey::FileGoto"),
            "the file screen draws the prompt its own header opens"
        );
        assert!(
            !body("render_git_panel").contains("SheetKey::FileGoto"),
            "the Git surface must not carry the file viewer's sheet"
        );
        assert!(
            body("close_file_goto").contains("SheetKey::FileGoto"),
            "the sheet belongs to the surface and is dropped with it"
        );
        assert!(
            body("close_file_screen").contains("close_file_goto"),
            "leaving the file takes its sheet down with it"
        );
        assert!(
            body("set_surface").contains("close_file_goto"),
            "a surface the sheet does not belong to must not inherit it"
        );
    }

    /// The mode control swaps its own value; it does not open a list.
    ///
    /// The mode was two labeled halves in the toolbar, which put a choice the
    /// reader was not making in the row beside the field they were typing in.
    /// It is now the value in force, and a tap flips it — with no panel to
    /// dismiss and no second tap to make.
    #[test]
    fn a_tap_on_the_search_mode_swaps_it_in_place() {
        let source = include_str!("workbench.rs");
        let toolbar = source
            .split_once("fn render_file_tree(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_file_tree_row("))
            .map(|(body, _)| body)
            .expect("the file tree panel should remain inspectable");
        assert!(
            toolbar.contains("toggle_file_search_mode"),
            "the control in the toolbar flips the mode itself"
        );
        assert!(
            toolbar.contains("self.file_search_mode.label()"),
            "and reads the mode it is in"
        );
        assert!(
            !toolbar.contains("file_search_mode_sheet_open")
                && !toolbar.contains("SheetKey::FileSearchMode"),
            "no sheet is raised for a two-state setting"
        );
    }

    /// Both rail searches run from the field itself.
    ///
    /// They used to wait for `PressEnter`, which a phone's soft keyboard does
    /// not send: the file panel answered the empty query with "No matching
    /// items" while the reader typed, and the commit field never ran a query at
    /// all. Typing is now the query, once the pause after the last keystroke
    /// has passed.
    #[test]
    fn both_rail_searches_run_as_the_reader_types() {
        let source = include_str!("workbench.rs");
        let between = |start: &str, end: &str| {
            source
                .split_once(start)
                .and_then(|(_, tail)| tail.split_once(end))
                .map(|(body, _)| body)
                .unwrap_or_else(|| panic!("{start} .. {end} should remain inspectable"))
        };

        let file_subscription = between(
            "self.file_search_subscription = Some(cx.subscribe_in(",
            "if self.file_find_input.is_none()",
        );
        assert!(
            file_subscription.contains("InputEvent::Change => this.schedule_file_search(cx)"),
            "the file field searches while the reader types"
        );
        assert!(
            file_subscription.contains("InputEvent::PressEnter { .. } =>"),
            "and the action key still runs the query at once"
        );

        let history_subscription = between(
            "self.git_history_query_subscription = Some(cx.subscribe_in(",
            "if self.file_editor_input.is_none()",
        );
        assert!(
            history_subscription
                .contains("InputEvent::Change => this.schedule_git_history_search(cx)"),
            "the commit field searches while the reader types"
        );

        let file_schedule = between("fn schedule_file_search(", "fn start_file_search(");
        assert!(
            file_schedule.contains("FILE_SEARCH_DEBOUNCE") && file_schedule.contains("timer("),
            "a pause, not every letter, spends a file query"
        );
        assert!(
            file_schedule.contains(".search.begin()"),
            "the panel says it is searching from the first keystroke"
        );

        let history_schedule = between(
            "fn schedule_git_history_search(",
            "fn start_git_history_search(",
        );
        assert!(
            history_schedule.contains("GIT_HISTORY_SEARCH_DEBOUNCE")
                && history_schedule.contains("timer("),
            "a pause, not every letter, spends a commit query"
        );

        let file_apply = between("fn start_file_search(", "fn reveal_file_search_result(");
        assert!(
            file_apply.contains("request_generation != this.file_search_request_generation"),
            "a reply for words the field no longer holds does not paint over it"
        );
    }

    /// A query with no answer yet is not an empty result.
    ///
    /// "No matching items" is the answer to a finished search. While the field
    /// is still waiting — for the pause, or for the authority — the panel shows
    /// the search in progress instead.
    #[test]
    fn an_unanswered_file_query_is_not_reported_as_empty() {
        let source = include_str!("workbench.rs");
        let panel = source
            .split_once("fn render_file_tree(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_file_tree_row("))
            .map(|(body, _)| body)
            .expect("the file tree panel should remain inspectable");
        assert!(
            panel.contains("let search_settled = !search_loading && !search_failed"),
            "an answer is reported only once the search has settled"
        );
        assert!(
            panel.contains("search_settled && !search_has_results"),
            "the empty state is only for a finished, successful search"
        );
        assert!(
            panel.contains("search_has_results && search_settled"),
            "and rows from an older query wait for the new answer"
        );
    }

    /// A close control that reports to nobody is a dead button.
    ///
    /// The rail cannot animate the drawer it lives in, so the app hands it a
    /// handler when it creates the workbench; without the hand-off the header
    /// still paints and taps do nothing.
    #[test]
    fn the_app_gives_the_rail_a_way_to_close_itself() {
        let ensure = include_str!("app.rs")
            .split_once("fn ensure_workbench(")
            .and_then(|(_, tail)| tail.split_once("\n    /// Closes the right rail"))
            .map(|(body, _)| body)
            .expect("workbench creation should remain inspectable");
        assert!(
            ensure.contains("set_close_handler("),
            "the workbench has to learn how to close the drawer it was created in"
        );
    }

    #[test]
    fn visible_surfaces_exclude_provider_and_runtime_entries() {
        assert_eq!(
            WorkbenchSurface::ALL,
            [
                WorkbenchSurface::Files,
                WorkbenchSurface::Git,
                WorkbenchSurface::Terminal,
            ]
        );
        assert_eq!(theme::workbench_bg(), theme::sidebar_bg());
    }

    /// The strip counts one thing: how far the branch has drifted.
    ///
    /// The tab names the surface by what the reader finds there, and only the
    /// working tree carries a count — the same two the desktop paints over the
    /// same list. A running terminal is a state its panel already shows, and a
    /// tab that counted sessions spent the row on a number nobody acts on.
    #[test]
    fn the_surface_strip_counts_only_the_branchs_drift() {
        let source = include_str!("workbench.rs");
        let header = source
            .split_once("fn render_header(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_close_control("))
            .map(|(body, _)| body)
            .expect("the rail header should remain inspectable");
        assert!(
            header.contains("git_sync_counts") && header.contains("WorkbenchSurface::Git"),
            "the working tree's tab is the one that carries the branch's drift"
        );
        assert!(
            !header.contains("running_terminals") && !header.contains("change_row_count"),
            "no other tab counts anything"
        );
        assert_eq!(WorkbenchSurface::Git.label(), "Changes");
        assert_eq!(WorkbenchSurface::Terminal.label(), "Terminal");
    }

    /// A branch with no upstream has not drifted from anything.
    #[test]
    fn the_sync_badge_counts_only_a_branch_that_has_an_upstream() {
        let branch = |current: bool, upstream: bool, ahead: u32, behind: u32| GitBranchSummary {
            name: "main".to_string(),
            current,
            upstream: upstream.then(|| "origin/main".to_string()),
            ahead,
            behind,
            current_ahead: 0,
            current_behind: 0,
            detached: false,
        };
        assert_eq!(
            git_sync_counts(&[branch(true, true, 3, 8)]),
            (8, 3),
            "behind first, then ahead, the order the two badges are drawn in"
        );
        assert_eq!(git_sync_counts(&[]), (0, 0));
        assert_eq!(
            git_sync_counts(&[branch(true, false, 5, 5)]),
            (0, 0),
            "a local-only branch has no remote to be ahead of"
        );
        assert_eq!(
            git_sync_counts(&[branch(false, true, 5, 5)]),
            (0, 0),
            "and a branch the reader is not on is not the one on screen"
        );
    }

    /// The rail's two counts are drawn with text arrows, not emoji.
    ///
    /// The platform monospace face has no diagonal arrows, so `↙` and `↗`
    /// fell through to the emoji font and the badge painted a coloured glyph
    /// where the desktop paints a text arrow. The vertical arrows are the ones
    /// the branch sheet already uses and no face on the phone turns them into
    /// emoji.
    #[test]
    fn the_sync_badge_draws_text_arrows() {
        let source = include_str!("workbench.rs");
        assert!(
            source.contains("sync_badge(behind, \"↓\"")
                && source.contains("sync_badge(ahead, \"↑\""),
            "the badge asks for the plain arrows"
        );
        assert!(
            !source.contains("\"↙\"") && !source.contains("\"↗\""),
            "the diagonal arrows are the emoji fallback this fix removes"
        );
    }
}
