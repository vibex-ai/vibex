use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AccessibleAction, AnyElement, AnyWindowHandle, App, ClipboardItem, Context, DragMoveEvent,
    Entity, FocusHandle, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement,
    KeyDownEvent, Keystroke, ListAlignment, ListHorizontalSizingBehavior, ListOffset, ListState,
    MouseButton, MouseDownEvent, Orientation, ParentElement as _, PathBuilder, Render, Role,
    ScrollHandle, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, Subscription, Task, UniformListScrollHandle, WeakEntity, Window, canvas, div, img,
    list, point, prelude::*, px, relative, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, IndexPath,
    Selectable as _, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonRounded, ButtonVariants as _},
    calendar::Date,
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel, v_resizable},
    scroll::ScrollableElement as _,
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
    v_flex,
};
use sha2::{Digest as _, Sha256};
use vibex_backend::{BackendError, BackendFacade, BackendOperation, MutationRequest};
use vibex_content::{
    ContentSurfaceKind, ContentSurfaceLifecycle, ContentSurfaceOrigin, LogicalSurfaceBounds,
};
use vibex_core::{
    FileEncoding, FileEntryKind, FileLineEnding, FileMutationRequest, FilePreviewKind,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest, GitBranchSummary, GitChange, GitChangeKind,
    GitCommitDetailRequest, GitCommitRequest, GitCommitSummary, GitDiffRequest, GitDiffResponse,
    GitHistoryAuthor, GitHistoryRequest, GitManagedWorktreeStatus, GitRemoteActionKind,
    GitRemoteActionRequest, GitRemoteSummary, GitStageRequest, GitStatusSummary,
    GitWorktreeArchiveRequest, GitWorktreeConflictFile, GitWorktreeConflictKind,
    GitWorktreeConflictResolveRequest, GitWorktreeConflictStageRequest, GitWorktreeConflictVersion,
    GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeMergePlan, GitWorktreeMergeRequest, GitWorktreeMergeStrategy,
    GitWorktreeOperationRecord, GitWorktreeOperationRequest, GitWorktreeOperationStatus,
    GitWorktreeReadinessRequest, GitWorktreeReadinessState, GitWorktreeRestoreRequest,
    GitWorktreeRisk, GitWorktreeRiskKind, RequestId, TerminalId, TerminalSession, TerminalStatus,
    VibexError, VibexResult, WorkspaceId, unix_timestamp_ms,
};
use vibex_desktop_model::{
    BoundedImageCache, ContentPreviewKind, EditorBufferAvailability, EditorBufferRegistry,
    EditorExternalState, EditorRecoverySnapshot, FileExplorerRow, FileIconKind, FileMutationKind,
    FileTreeLoadState, FileTreeProjection, GitCommitPatchRow, GitMutationKind,
    GitPathSelectionState, GitQueryKind, GitSelectionKey, GitTreeRow, GitTreeRowKind,
    GitWorkbenchMode, GitWorkbenchState, ImageCacheKey, PendingFileMutation,
    PreviewCloseDisposition, PreviewPane, PreviewSplitNode, PreviewSplitPosition, PreviewState,
    PreviewTab, PreviewTarget, UnifiedDiffLineKind, WorktreeLifecycleDisplayState,
    WorktreeLifecycleView, content_preview_kind, content_preview_kind_for_path,
    file_icon_descriptor, mutation_scope,
};
use vibex_desktop_runtime::{DesktopRuntime, GitHandle, validate_external_open_url};
use vibex_markdown::{
    MarkdownDocument, MarkdownInput, MarkdownSurface, MarkdownView, ResolvedResource, ResourceKind,
    ResourcePolicy, ResourceRole, code_font_weight, parse_markdown,
};

use crate::app::VibexWorkbench;
use crate::assets::{file_tree_asset_icon, open_tool_brand_icon};
use crate::locale;
use crate::office_surface::OfficeSurface;
use crate::pdf_surface::PdfSurface;
use crate::platform::{
    available_external_tools, open_external_url, open_native_terminal_for_path,
    open_path_with_default_app, open_path_with_external_tool, reveal_path_in_file_manager,
};
use crate::terminal_surface::TerminalSurface;

const FILE_ROW_HEIGHT: f32 = 28.0;
const FILE_TREE_INDENT: f32 = 20.0;
const FILE_TREE_GUIDE_OFFSET: f32 = 16.0;
const FILE_MANAGER_OPEN_TOOL_ID: &str = "file_manager";
const NATIVE_TERMINAL_OPEN_TOOL_ID: &str = "native_terminal";
const GIT_ROW_HEIGHT: f32 = 30.0;
const GIT_HISTORY_ROW_HEIGHT: f32 = 64.0;
const GIT_HISTORY_GRAPH_LANE_WIDTH: f32 = 12.0;
const GIT_HISTORY_GRAPH_MAX_LANES: usize = 8;
const GIT_HISTORY_DRAWER_MIN_HEIGHT: f32 = 52.0;
const GIT_HISTORY_DRAWER_CLOSE_THRESHOLD: f32 = 92.0;
const GIT_HISTORY_DRAWER_DEFAULT_HEIGHT: f32 = 260.0;
const GIT_HISTORY_DRAWER_KEYBOARD_STEP: f32 = 24.0;
const GIT_COMMIT_MESSAGE_HEIGHT: f32 = 80.0;
const GIT_COMMIT_TYPES: [&str; 11] = [
    "feat", "fix", "refactor", "test", "docs", "style", "perf", "chore", "build", "ci", "revert",
];
const DIFF_ROW_MIN_HEIGHT: f32 = 22.0;
const DIFF_LIST_OVERDRAW: f32 = 512.0;
const DIFF_LINE_VERTICAL_PADDING: f32 = 2.0;
const FILE_PREVIEW_MAX_BYTES: u64 = 8 * 1024 * 1024;
const IMAGE_SOURCE_MAX_BYTES: usize = 8 * 1024 * 1024;
const MARKDOWN_LOCAL_IMAGE_LIMIT: usize = 32;
const MARKDOWN_LOCAL_IMAGE_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const GIT_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);
const HIDDEN_WORKSPACE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const EDITOR_RECOVERY_PERSIST_DELAY: Duration = Duration::from_millis(200);
const FILE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(140);
const GIT_HISTORY_SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const FILE_SEARCH_RESULT_LIMIT: u32 = 200;
const WORKTREE_CONFLICT_RENDER_LIMIT: usize = 256;
pub const CODE_WORKBENCH_MAX_EAGER_ROWS: usize = 5_000;
pub const CODE_WORKBENCH_INITIAL_DIFF_ROWS: usize = 500;

#[derive(Debug)]
struct PatchListState {
    revision: String,
    list: ListState,
}

impl PatchListState {
    fn new(revision: String, row_count: usize, row_height: f32) -> Self {
        Self {
            revision,
            list: ListState::new(row_count, ListAlignment::Top, px(DIFF_LIST_OVERDRAW))
                .with_uniform_item_height(px(row_height)),
        }
    }

    fn reconcile(&mut self, revision: &str, row_count: usize, row_height: f32) {
        if self.revision != revision {
            *self = Self::new(revision.to_string(), row_count, row_height);
            return;
        }
        if self.list.item_count() == row_count {
            return;
        }

        let scroll_top = self.list.logical_scroll_top();
        self.list
            .reset_with_uniform_height(row_count, px(row_height));
        self.list.scroll_to(ListOffset {
            item_ix: scroll_top.item_ix.min(row_count),
            offset_in_item: scroll_top.offset_in_item,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightRailMode {
    Files,
    Git,
}

impl RightRailMode {
    fn title(self) -> &'static str {
        match self {
            Self::Files => locale::text("Files", "文件", "檔案"),
            Self::Git => "Git",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSearchMode {
    Name,
    Content,
}

impl FileSearchMode {
    fn toggled(self) -> Self {
        match self {
            Self::Name => Self::Content,
            Self::Content => Self::Name,
        }
    }

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

    fn title(self) -> &'static str {
        match self {
            Self::Name => locale::text("Name", "名称", "名稱"),
            Self::Content => locale::text("Content", "内容", "內容"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FileSearchOptions {
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSearchOption {
    CaseSensitive,
    WholeWord,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileNameSearchTreeNode {
    path: String,
    name: String,
    kind: FileEntryKind,
    matched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileNameSearchTreeRow {
    path: String,
    name: String,
    kind: FileEntryKind,
    depth: usize,
    matched: bool,
    has_children: bool,
    expanded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeWorkbenchFixtureKind {
    Files,
    Diff,
    Markdown,
}

pub(crate) struct CodeWorkbenchPersistedState {
    pub preview: PreviewState,
    pub recovery: Option<EditorRecoverySnapshot>,
    pub workspace_id: Option<String>,
    pub selected_file_path: Option<String>,
    pub selected_git_path: Option<String>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeWorkbenchEvent {
    LayoutChanged { fullscreen: bool },
}

#[derive(Debug, Clone)]
struct WorkbenchWorkspace {
    id: WorkspaceId,
    root: PathBuf,
    generation: u64,
}

#[derive(Debug, Clone)]
struct WorkspaceGenerationFence {
    workspace_id: WorkspaceId,
    generation: u64,
}

impl WorkspaceGenerationFence {
    fn capture(workspace: &WorkbenchWorkspace) -> Self {
        Self {
            workspace_id: workspace.id.clone(),
            generation: workspace.generation,
        }
    }

    fn matches(&self, workspace: Option<&WorkbenchWorkspace>) -> bool {
        workspace.is_some_and(|workspace| {
            workspace.id == self.workspace_id && workspace.generation == self.generation
        })
    }
}

#[derive(Clone)]
struct PendingWorkspace {
    runtime: Arc<DesktopRuntime>,
    id: WorkspaceId,
    root: PathBuf,
}

#[derive(Clone)]
struct EditorBinding {
    id: u64,
    input: Entity<InputState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSearchReveal {
    path: String,
    query: String,
    line: u32,
    match_start: Option<u32>,
    match_end: Option<u32>,
}

enum FilePresentation {
    Loading,
    Markdown {
        document: Arc<MarkdownDocument>,
        images: Arc<BTreeMap<String, Arc<Image>>>,
    },
    Image {
        image: Arc<Image>,
        cache_key: ImageCacheKey,
    },
    MediaExternalOnly,
    Pdf(Entity<PdfSurface>),
    Office(Entity<OfficeSurface>),
    Unsupported(String),
    Error {
        code: String,
        message: String,
    },
}

impl FilePresentation {
    fn error(error: VibexError) -> Self {
        Self::Error {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Clone)]
struct FileRowDrag {
    path: String,
    name: SharedString,
    kind: FileEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InlineFileAction {
    CreateFile { parent: String },
    CreateDirectory { parent: String },
    Rename { source: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileClipboardOperation {
    Cut,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileClipboardEntry {
    operation: FileClipboardOperation,
    path: String,
    name: String,
    kind: FileEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileContextMenuTarget {
    path: String,
    name: String,
    kind: FileEntryKind,
    target_directory: String,
    directory_error: bool,
}

impl Render for FileRowDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().drag_border)
            .bg(cx.theme().popover)
            .child(Icon::new(if self.kind == FileEntryKind::Directory {
                IconName::FolderOpen
            } else {
                IconName::File
            }))
            .child(self.name.clone())
    }
}

#[derive(Clone)]
struct PreviewTabDrag {
    tab_id: String,
    label: SharedString,
}

impl Render for PreviewTabDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().drag_border)
            .bg(cx.theme().popover)
            .child(Icon::new(IconName::File).small())
            .child(self.label.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHistoryBranchChoice {
    name: String,
    incoming: u32,
    outgoing: u32,
}

impl SearchableListItem for GitHistoryBranchChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        format!("{}  ↓{} ↑{}", self.name, self.incoming, self.outgoing).into()
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(div().min_w_0().flex_1().truncate().child(self.name.clone()))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1p5()
                    .font_family("monospace")
                    .text_xs()
                    .child(
                        div()
                            .text_color(cx.theme().info)
                            .child(format!("↓{}", self.incoming)),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().success)
                            .child(format!("↑{}", self.outgoing)),
                    ),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHistoryAuthorChoice {
    label: String,
    value: Option<String>,
}

impl SearchableListItem for GitHistoryAuthorChoice {
    type Value = Option<String>;

    fn title(&self) -> SharedString {
        self.label.clone().into()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct GitHistoryDrawerResizeState {
    start_window_y: f32,
    start_height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GitHistoryDrawerResizeDrag;

impl Render for GitHistoryDrawerResizeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitGraphEdge {
    from: usize,
    to: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitGraphRow {
    lane: usize,
    top_edges: Vec<GitGraphEdge>,
    bottom_edges: Vec<GitGraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitGraphLayout {
    rows: Vec<GitGraphRow>,
    lane_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewTabDropTarget {
    pane_id: String,
    tab_id: String,
    after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewPaneDropRegion {
    TabGroup,
    Content,
    Right,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewPaneDropTarget {
    pane_id: String,
    region: PreviewPaneDropRegion,
}

#[derive(Clone)]
enum GitTreeInteraction {
    Changes,
    Commit { hash: String, subject: String },
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum WorktreeLifecycleConfirmation {
    Merge {
        plan: GitWorktreeMergePlan,
        strategy: GitWorktreeMergeStrategy,
    },
    Archive {
        request: GitWorktreeArchiveRequest,
        preflight: GitWorktreeDestructivePreflight,
    },
    Restore {
        request: GitWorktreeRestoreRequest,
        preflight: GitWorktreeDestructivePreflight,
    },
    Discard {
        request: GitWorktreeDiscardRequest,
        preflight: GitWorktreeDestructivePreflight,
    },
    Continue(GitWorktreeOperationRequest),
    Abort(GitWorktreeOperationRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeRightRailRevision {
    file_tree: u64,
    git: u64,
    workspace_generation: u64,
    workspace_available: bool,
    tree_loading: bool,
    file_mutation_pending: bool,
    status_loading: bool,
    history_loading: bool,
    lifecycle_snapshot: Option<String>,
    lifecycle_confirmation: u64,
    lifecycle_loading: bool,
    lifecycle_action_pending: bool,
    lifecycle_mutations_available: bool,
    mode: RightRailMode,
    commit_type: String,
    amend_commit: bool,
    selected_git_path: Option<String>,
    error: Option<String>,
    note: Option<String>,
}

#[derive(Clone)]
struct CodeRightRailFileProjection {
    workspace_available: bool,
    workspace_root: Option<PathBuf>,
    rows: Arc<Vec<FileExplorerRow>>,
    expanded_paths: Arc<BTreeSet<String>>,
    selected_directory_path: Option<String>,
    root_name: String,
    loading: bool,
    mutation_pending: bool,
    scroll: UniformListScrollHandle,
}

impl CodeRightRailFileProjection {
    fn row_position(&self, path: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.path_chain.iter().any(|candidate| candidate == path))
    }

    fn is_expanded(&self, path: &str) -> bool {
        path.is_empty() || self.expanded_paths.contains(path)
    }
}

#[derive(Clone)]
struct CodeRightRailGitTreeRow {
    row: GitTreeRow,
    change: Option<GitChange>,
}

#[derive(Clone)]
struct CodeRightRailGitProjection {
    mode: GitWorkbenchMode,
    pending: bool,
    selected_count: usize,
    root_selection: GitPathSelectionState,
    selection_states: Arc<BTreeMap<String, GitPathSelectionState>>,
    has_directories: bool,
    all_directories_expanded: bool,
    change_count: usize,
    additions: u32,
    deletions: u32,
    change_rows: Arc<Vec<CodeRightRailGitTreeRow>>,
    history: Arc<Vec<GitCommitSummary>>,
    branches: Arc<Vec<GitBranchSummary>>,
    remotes: Arc<Vec<GitRemoteSummary>>,
    authors: Arc<Vec<GitHistoryAuthor>>,
    selected_branch: Option<String>,
    selected_author: Option<String>,
    history_query: Option<String>,
    history_authored_after_ms: Option<i64>,
    history_authored_before_ms: Option<i64>,
    selected_hash: Option<String>,
    selected_commit: Option<GitCommitSummary>,
    commit_detail_loaded: bool,
    commit_file_count: usize,
    commit_additions: u32,
    commit_deletions: u32,
    commit_rows: Arc<Vec<CodeRightRailGitTreeRow>>,
    status_loading: bool,
    history_loading: bool,
    scroll: UniformListScrollHandle,
}

impl CodeRightRailGitProjection {
    fn selection_state(&self, path: &str) -> GitPathSelectionState {
        self.selection_states
            .get(path)
            .copied()
            .unwrap_or(GitPathSelectionState::Unchecked)
    }
}

#[derive(Clone)]
struct CodeRightRailProjection {
    revision: CodeRightRailRevision,
    mode: RightRailMode,
    error: Option<String>,
    note: Option<String>,
    files: CodeRightRailFileProjection,
    git: CodeRightRailGitProjection,
    workspace_name: String,
    commit_message: Entity<InputState>,
    commit_type: String,
    amend_commit: bool,
    selected_git_path: Option<String>,
    lifecycle_view: Option<WorktreeLifecycleView>,
    lifecycle_confirmation: Option<WorktreeLifecycleConfirmation>,
    lifecycle_loading: bool,
    lifecycle_action_pending: bool,
    lifecycle_mutations_available: bool,
}

fn right_rail_git_projection(
    git: &GitWorkbenchState,
    status_loading: bool,
    history_loading: bool,
    scroll: UniformListScrollHandle,
) -> CodeRightRailGitProjection {
    let change_rows = (0..git.change_tree_row_count())
        .filter_map(|index| {
            git.change_tree_row(index)
                .map(|(row, change)| CodeRightRailGitTreeRow {
                    row: row.clone(),
                    change: change.cloned(),
                })
        })
        .collect::<Vec<_>>();
    let selection_states = right_rail_git_selection_states(git, &change_rows);
    let root_selection = selection_states
        .get("")
        .copied()
        .unwrap_or(GitPathSelectionState::Unchecked);
    let (change_count, additions, deletions) = git
        .status
        .as_ref()
        .map(|status| {
            let (additions, deletions) =
                status
                    .changes
                    .iter()
                    .fold((0_u32, 0_u32), |summary, change| {
                        (
                            summary.0.saturating_add(change.additions),
                            summary.1.saturating_add(change.deletions),
                        )
                    });
            (status.changes.len(), additions, deletions)
        })
        .unwrap_or_default();
    let (branches, remotes) = git
        .branches
        .as_ref()
        .map(|response| (response.branches.clone(), response.remotes.clone()))
        .unwrap_or_default();
    let selected_hash = git.selected_commit_hash.clone();
    let selected_commit = selected_hash.as_ref().and_then(|hash| {
        git.history
            .iter()
            .find(|commit| &commit.hash == hash)
            .cloned()
    });
    let selected_detail = selected_hash.as_ref().and_then(|hash| {
        git.commit_detail
            .as_ref()
            .filter(|detail| &detail.summary.hash == hash)
    });
    let (commit_file_count, commit_additions, commit_deletions) = selected_detail
        .map(|detail| {
            let (additions, deletions) =
                detail.files.iter().fold((0_u32, 0_u32), |summary, file| {
                    (
                        summary.0.saturating_add(file.additions),
                        summary.1.saturating_add(file.deletions),
                    )
                });
            (detail.files.len(), additions, deletions)
        })
        .unwrap_or_default();
    let commit_rows = (0..git.commit_tree_row_count())
        .filter_map(|index| {
            git.commit_tree_row(index)
                .map(|(row, change)| CodeRightRailGitTreeRow {
                    row: row.clone(),
                    change: change.cloned(),
                })
        })
        .collect::<Vec<_>>();

    CodeRightRailGitProjection {
        mode: git.mode,
        pending: git.pending_mutation.is_some(),
        selected_count: git.selected_path_count(),
        root_selection,
        selection_states: Arc::new(selection_states),
        has_directories: git.has_change_directories(),
        all_directories_expanded: git.all_change_directories_expanded(),
        change_count,
        additions,
        deletions,
        change_rows: Arc::new(change_rows),
        history: Arc::new(git.history.clone()),
        branches: Arc::new(branches),
        remotes: Arc::new(remotes),
        authors: Arc::new(git.history_authors.clone()),
        selected_branch: git.history_filter.ref_name.clone(),
        selected_author: git.history_filter.author.clone(),
        history_query: git.history_filter.query.clone(),
        history_authored_after_ms: git.history_filter.authored_after_ms,
        history_authored_before_ms: git.history_filter.authored_before_ms,
        selected_hash,
        selected_commit,
        commit_detail_loaded: selected_detail.is_some(),
        commit_file_count,
        commit_additions,
        commit_deletions,
        commit_rows: Arc::new(commit_rows),
        status_loading,
        history_loading,
        scroll,
    }
}

fn right_rail_git_selection_states(
    git: &GitWorkbenchState,
    rows: &[CodeRightRailGitTreeRow],
) -> BTreeMap<String, GitPathSelectionState> {
    let selected = git
        .selected_change_paths()
        .into_iter()
        .filter_map(|path| normalized_relative_path(&path))
        .collect::<BTreeSet<_>>();
    let available = rows
        .iter()
        .filter(|row| row.change.is_some())
        .filter_map(|row| normalized_relative_path(&row.row.path))
        .collect::<BTreeSet<_>>();
    let mut counts = BTreeMap::<String, (usize, usize)>::new();
    for path in available {
        let is_selected = selected.contains(&path);
        let mut current = path.as_str();
        loop {
            let count = counts.entry(current.to_string()).or_default();
            count.0 = count.0.saturating_add(1);
            count.1 = count.1.saturating_add(usize::from(is_selected));
            if current.is_empty() {
                break;
            }
            current = relative_parent_path(current);
        }
    }
    counts
        .into_iter()
        .map(|(path, (available, selected))| {
            let state = if selected == 0 {
                GitPathSelectionState::Unchecked
            } else if selected == available {
                GitPathSelectionState::Checked
            } else {
                GitPathSelectionState::Indeterminate
            };
            (path, state)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeLifecyclePrimaryAction {
    ReviewChanges,
    MarkReady,
    MergeBack,
    ReviewQueuedMerge,
    Restore,
}

fn worktree_lifecycle_primary_action(
    state: WorktreeLifecycleDisplayState,
) -> Option<WorktreeLifecyclePrimaryAction> {
    match state {
        WorktreeLifecycleDisplayState::Working => {
            Some(WorktreeLifecyclePrimaryAction::ReviewChanges)
        }
        WorktreeLifecycleDisplayState::Reviewing => Some(WorktreeLifecyclePrimaryAction::MarkReady),
        WorktreeLifecycleDisplayState::Ready => Some(WorktreeLifecyclePrimaryAction::MergeBack),
        WorktreeLifecycleDisplayState::Queued => {
            Some(WorktreeLifecyclePrimaryAction::ReviewQueuedMerge)
        }
        WorktreeLifecycleDisplayState::Archived => Some(WorktreeLifecyclePrimaryAction::Restore),
        WorktreeLifecycleDisplayState::Merging
        | WorktreeLifecycleDisplayState::NeedsResolution
        | WorktreeLifecycleDisplayState::Aborting
        | WorktreeLifecycleDisplayState::Archiving
        | WorktreeLifecycleDisplayState::Restoring
        | WorktreeLifecycleDisplayState::Discarding
        | WorktreeLifecycleDisplayState::Discarded
        | WorktreeLifecycleDisplayState::Failed
        | WorktreeLifecycleDisplayState::NeedsAttention => None,
    }
}

pub struct CodeWorkbench {
    parent: Option<WeakEntity<VibexWorkbench>>,
    runtime: Option<Arc<DesktopRuntime>>,
    backend: Option<BackendFacade>,
    workspace: Option<WorkbenchWorkspace>,
    pending_workspace: Option<PendingWorkspace>,
    workspace_generation: u64,
    restored_workspace_id: Option<String>,
    pub(crate) right_rail_mode: RightRailMode,
    pub(crate) file_tree: FileTreeProjection,
    pub(crate) git: GitWorkbenchState,
    pub(crate) preview: PreviewState,
    preview_panel_fullscreen: bool,
    preview_visible: bool,
    files_surface_visible: bool,
    git_surface_visible: bool,
    pub(crate) editors: EditorBufferRegistry,
    editor_bindings: BTreeMap<String, EditorBinding>,
    next_editor_binding_id: u64,
    editor_subscriptions: Vec<Subscription>,
    markdown_edit_paths: BTreeSet<String>,
    presentations: BTreeMap<String, FilePresentation>,
    image_cache: BoundedImageCache,
    lifecycles: BTreeMap<String, ContentSurfaceLifecycle>,
    activation_generation: u64,
    terminals: Vec<TerminalSession>,
    terminal_surfaces: BTreeMap<String, Entity<TerminalSurface>>,
    active_terminal_surface_ids: BTreeSet<String>,
    selected_file_path: Option<String>,
    selected_git_path: Option<String>,
    selected_terminal_id: Option<String>,
    pub(crate) commit_message: Entity<InputState>,
    pub(crate) commit_type: String,
    pub(crate) amend_commit: bool,
    commit_reset_window: Option<AnyWindowHandle>,
    pub(crate) error: Option<String>,
    pub(crate) note: Option<String>,
    tree_loading: bool,
    status_loading: bool,
    history_loading: bool,
    file_mutation_pending: bool,
    tree_tasks: BTreeMap<String, Task<()>>,
    tree_refreshing: bool,
    tree_refresh_task: Option<Task<()>>,
    workspace_poll_task: Option<Task<()>>,
    git_status_poll_task: Option<Task<()>>,
    status_task: Option<Task<()>>,
    history_task: Option<Task<()>>,
    branch_task: Option<Task<()>>,
    lifecycle_task: Option<Task<()>>,
    lifecycle_action_task: Option<Task<()>>,
    file_tasks: BTreeMap<String, Task<()>>,
    diff_tasks: BTreeMap<GitSelectionKey, Task<()>>,
    commit_detail_tasks: BTreeMap<String, Task<()>>,
    mutation_task: Option<Task<()>>,
    lifecycle_snapshot: Option<GitWorktreeLifecycleSnapshot>,
    lifecycle_confirmation: Option<WorktreeLifecycleConfirmation>,
    lifecycle_confirmation_revision: u64,
    lifecycle_loading: bool,
    lifecycle_reload_requested: bool,
    lifecycle_action_pending: bool,
    file_scroll: UniformListScrollHandle,
    git_scroll: UniformListScrollHandle,
    preview_diff_lists: BTreeMap<String, PatchListState>,
    preview_commit_lists: BTreeMap<String, PatchListState>,
    preview_commit_focus_requests: BTreeMap<String, u64>,
    git_preview_errors: BTreeMap<String, String>,
    preview_tab_scrolls: BTreeMap<String, ScrollHandle>,
    markdown_scrolls: BTreeMap<String, ScrollHandle>,
    preview_revealed_tab_ids: BTreeMap<String, String>,
    pending_file_search_reveal: Option<FileSearchReveal>,
    pending_file_search_directory_reveal: Option<String>,
    preview_tab_drop_target: Option<PreviewTabDropTarget>,
    preview_pane_drop_target: Option<PreviewPaneDropTarget>,
    restore_hydration_scheduled: bool,
    recovery_persist_generation: u64,
    recovery_persist_task: Option<Task<()>>,
    code_font_family: String,
    code_font_size: u16,
}

impl gpui::EventEmitter<CodeWorkbenchEvent> for CodeWorkbench {}

impl CodeWorkbench {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parent: WeakEntity<VibexWorkbench>,
        restored_workspace_id: Option<String>,
        preview: PreviewState,
        recovery: EditorRecoverySnapshot,
        selected_file_path: Option<String>,
        selected_git_path: Option<String>,
        selected_terminal_id: Option<String>,
        code_font_family: String,
        code_font_size: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_inner(
            Some(parent),
            restored_workspace_id,
            preview,
            recovery,
            selected_file_path,
            selected_git_path,
            selected_terminal_id,
            code_font_family,
            code_font_size,
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        parent: Option<WeakEntity<VibexWorkbench>>,
        restored_workspace_id: Option<String>,
        mut preview: PreviewState,
        recovery: EditorRecoverySnapshot,
        selected_file_path: Option<String>,
        selected_git_path: Option<String>,
        selected_terminal_id: Option<String>,
        code_font_family: String,
        code_font_size: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        preview.normalize();
        preview.fullscreen_tab_id = None;
        let mut editors = EditorBufferRegistry::default();
        editors.restore_recovery(recovery);
        let commit_message = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder(git_commit_placeholder("feat"))
        });
        Self {
            parent,
            runtime: None,
            backend: None,
            workspace: None,
            pending_workspace: None,
            workspace_generation: 0,
            restored_workspace_id,
            right_rail_mode: RightRailMode::Files,
            file_tree: FileTreeProjection::default(),
            git: GitWorkbenchState::default(),
            preview,
            preview_panel_fullscreen: false,
            preview_visible: false,
            files_surface_visible: false,
            git_surface_visible: false,
            editors,
            editor_bindings: BTreeMap::new(),
            next_editor_binding_id: 0,
            editor_subscriptions: Vec::new(),
            markdown_edit_paths: BTreeSet::new(),
            presentations: BTreeMap::new(),
            image_cache: BoundedImageCache::default(),
            lifecycles: BTreeMap::new(),
            activation_generation: 0,
            terminals: Vec::new(),
            terminal_surfaces: BTreeMap::new(),
            active_terminal_surface_ids: BTreeSet::new(),
            selected_file_path,
            selected_git_path,
            selected_terminal_id,
            commit_message,
            commit_type: "feat".to_string(),
            amend_commit: false,
            commit_reset_window: None,
            error: None,
            note: None,
            tree_loading: false,
            status_loading: false,
            history_loading: false,
            file_mutation_pending: false,
            tree_tasks: BTreeMap::new(),
            tree_refreshing: false,
            tree_refresh_task: None,
            workspace_poll_task: None,
            git_status_poll_task: None,
            status_task: None,
            history_task: None,
            branch_task: None,
            lifecycle_task: None,
            lifecycle_action_task: None,
            file_tasks: BTreeMap::new(),
            diff_tasks: BTreeMap::new(),
            commit_detail_tasks: BTreeMap::new(),
            mutation_task: None,
            lifecycle_snapshot: None,
            lifecycle_confirmation: None,
            lifecycle_confirmation_revision: 0,
            lifecycle_loading: false,
            lifecycle_reload_requested: false,
            lifecycle_action_pending: false,
            file_scroll: UniformListScrollHandle::new(),
            git_scroll: UniformListScrollHandle::new(),
            preview_diff_lists: BTreeMap::new(),
            preview_commit_lists: BTreeMap::new(),
            preview_commit_focus_requests: BTreeMap::new(),
            git_preview_errors: BTreeMap::new(),
            preview_tab_scrolls: BTreeMap::new(),
            markdown_scrolls: BTreeMap::new(),
            preview_revealed_tab_ids: BTreeMap::new(),
            pending_file_search_reveal: None,
            pending_file_search_directory_reveal: None,
            preview_tab_drop_target: None,
            preview_pane_drop_target: None,
            restore_hydration_scheduled: false,
            recovery_persist_generation: 0,
            recovery_persist_task: None,
            code_font_family,
            code_font_size,
        }
    }

    pub fn fixture(
        kind: CodeWorkbenchFixtureKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new_inner(
            None,
            None,
            PreviewState::default(),
            EditorRecoverySnapshot::default(),
            None,
            None,
            None,
            crate::platform::default_code_font_family().to_string(),
            13,
            window,
            cx,
        );
        let workspace_id = WorkspaceId::new();
        this.workspace_generation = 1;
        this.workspace = Some(WorkbenchWorkspace {
            id: workspace_id.clone(),
            root: PathBuf::from("."),
            generation: 1,
        });
        this.file_tree.reset_workspace(workspace_id.clone());
        this.file_tree.set_root_name("vibex");
        let generation = this.file_tree.begin_load("");
        let tree_entries = [
            ("src", None, FileEntryKind::Directory),
            ("src/lib.rs", Some("src"), FileEntryKind::File),
            ("src/app.rs", Some("src"), FileEntryKind::File),
            ("docs", None, FileEntryKind::Directory),
            ("docs/architecture.md", Some("docs"), FileEntryKind::File),
            ("assets", None, FileEntryKind::Directory),
            ("assets/workbench.png", Some("assets"), FileEntryKind::File),
            ("Cargo.toml", None, FileEntryKind::File),
            ("README.md", None, FileEntryKind::File),
        ]
        .into_iter()
        .map(|(path, parent_path, entry_kind)| FileTreeEntry {
            workspace_id: workspace_id.clone(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            parent_path: parent_path.map(str::to_string),
            kind: entry_kind,
            size_bytes: None,
            modified_at_ms: Some(1),
            hidden: false,
            ignored: false,
        })
        .collect();
        assert!(
            this.file_tree
                .apply_entries(&workspace_id, generation, "", tree_entries)
        );
        assert!(this.file_tree.toggle_expanded("src"));
        assert!(this.file_tree.toggle_expanded("docs"));

        let changes = vec![
            GitChange {
                path: "src/lib.rs".to_string(),
                original_path: None,
                kind: GitChangeKind::Modified,
                staged: false,
                unstaged: true,
                additions: 3,
                deletions: 2,
            },
            GitChange {
                path: "docs/architecture.md".to_string(),
                original_path: None,
                kind: GitChangeKind::Added,
                staged: true,
                unstaged: false,
                additions: 18,
                deletions: 0,
            },
        ];
        this.file_tree.set_git_changes(&changes);
        this.git.reset_workspace(workspace_id.clone());
        let status_ticket = this
            .git
            .begin_query(GitQueryKind::Status, "status")
            .expect("fixture workspace is selected");
        assert!(this.git.apply_status(
            &status_ticket,
            GitStatusSummary {
                workspace_id: workspace_id.clone(),
                repo_path: ".".to_string(),
                branch: Some("feature/workbench".to_string()),
                short_commit: Some("7f16abc".to_string()),
                detached: false,
                dirty: true,
                staged_count: 1,
                unstaged_count: 1,
                untracked_count: 0,
                changes,
                captured_at_ms: 1,
            },
        ));

        let markdown = include_str!("../../../crates/vibex-markdown/fixtures/advanced.md");
        this.editors.insert_read(FileReadResponse {
            workspace_id: workspace_id.clone(),
            path: "README.md".to_string(),
            name: "README.md".to_string(),
            preview_kind: FilePreviewKind::Markdown,
            content: Some(markdown.to_string()),
            size_bytes: markdown.len() as u64,
            modified_at_ms: Some(1),
            language: Some("markdown".to_string()),
            truncated: false,
            encoding: FileEncoding::Utf8,
            line_ending: FileLineEnding::Lf,
            content_revision: "fixture-readme-r1".to_string(),
        });
        let editor = this.ensure_editor_binding("README.md", window, cx);
        editor.update(cx, |input, cx| {
            input.set_highlighter("markdown", cx);
            input.set_value(markdown, window, cx);
        });
        let document = parse_file_markdown(markdown, "README.md");
        this.presentations.insert(
            "README.md".to_string(),
            FilePresentation::Markdown {
                document,
                images: Arc::default(),
            },
        );
        let readme_tab = this
            .preview
            .open(
                PreviewTarget::File {
                    path: "README.md".to_string(),
                },
                None,
                1,
            )
            .expect("fixture README target is valid");

        let patch = concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,8 +1,9 @@ pub fn visible_rows_for_the_current_workspace_before_treating_the_tab_as_unavailable() {\n",
            " use std::collections::BTreeMap;\n",
            "-const MAX_ROWS: usize = 20_000;\n",
            "+const MAX_ROWS: usize = 100_000;\n",
            "+const INITIAL_ROWS: usize = 500;\n",
            " \n",
            " pub fn visible_rows(start: usize) -> usize {\n",
            "-    start + MAX_ROWS\n",
            "+    start.min(MAX_ROWS)\n",
            " }\n",
        );
        let diff_ticket = this
            .git
            .begin_query(GitQueryKind::Diff, "src/lib.rs")
            .expect("fixture workspace is selected");
        assert!(this.git.apply_diff(
            &diff_ticket,
            GitDiffResponse {
                workspace_id,
                path: "src/lib.rs".to_string(),
                staged: false,
                diff: patch.to_string(),
                truncated: false,
            },
        ));

        this.git.set_mode(GitWorkbenchMode::Changes);
        match kind {
            CodeWorkbenchFixtureKind::Files | CodeWorkbenchFixtureKind::Markdown => {
                this.right_rail_mode = RightRailMode::Files;
                this.preview.focus(&readme_tab);
            }
            CodeWorkbenchFixtureKind::Diff => {
                this.right_rail_mode = RightRailMode::Git;
                let diff_tab = this
                    .preview
                    .open(
                        PreviewTarget::GitDiff {
                            path: "src/lib.rs".to_string(),
                            staged: false,
                        },
                        None,
                        2,
                    )
                    .expect("fixture diff target is valid");
                assert!(this.preview.split(
                    &diff_tab,
                    "preview-pane-main",
                    PreviewSplitPosition::Right,
                    "preview-fixture-diff",
                    "preview-fixture-split",
                ));
                this.preview.focus(&diff_tab);
            }
        }
        this.note = Some("Fixture workspace".to_string());
        this
    }

    pub fn fullscreen_active(&self) -> bool {
        self.preview_panel_fullscreen
    }

    fn right_rail_revision(&self) -> CodeRightRailRevision {
        CodeRightRailRevision {
            file_tree: self.file_tree.presentation_revision(),
            git: self.git.presentation_revision(),
            workspace_generation: self.workspace_generation,
            workspace_available: self.workspace.is_some(),
            tree_loading: self.tree_loading,
            file_mutation_pending: self.file_mutation_pending,
            status_loading: self.status_loading,
            history_loading: self.history_loading,
            lifecycle_snapshot: self
                .lifecycle_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revision.clone()),
            lifecycle_confirmation: self.lifecycle_confirmation_revision,
            lifecycle_loading: self.lifecycle_loading,
            lifecycle_action_pending: self.lifecycle_action_pending,
            lifecycle_mutations_available: self.backend.as_ref().is_some_and(|backend| {
                backend
                    .capabilities()
                    .git
                    .supports(BackendOperation::GitWorktreeLifecycleMutate)
            }),
            mode: self.right_rail_mode,
            commit_type: self.commit_type.clone(),
            amend_commit: self.amend_commit,
            selected_git_path: self.selected_git_path.clone(),
            error: self.error.clone(),
            note: self.note.clone(),
        }
    }

    fn right_rail_projection(&self) -> CodeRightRailProjection {
        let revision = self.right_rail_revision();
        let workspace_name = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.root.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Changes")
            .to_string();
        CodeRightRailProjection {
            revision,
            mode: self.right_rail_mode,
            error: self.error.clone(),
            note: self.note.clone(),
            files: CodeRightRailFileProjection {
                workspace_available: self.workspace.is_some(),
                workspace_root: self
                    .workspace
                    .as_ref()
                    .map(|workspace| workspace.root.clone()),
                rows: self.file_tree.visible_rows_snapshot(),
                expanded_paths: self.file_tree.expanded_paths_snapshot(),
                selected_directory_path: self
                    .file_tree
                    .selected_directory_path()
                    .map(str::to_string),
                root_name: self.file_tree.root_name().to_string(),
                loading: self.tree_loading,
                mutation_pending: self.file_mutation_pending,
                scroll: self.file_scroll.clone(),
            },
            git: right_rail_git_projection(
                &self.git,
                self.status_loading,
                self.history_loading,
                self.git_scroll.clone(),
            ),
            workspace_name,
            commit_message: self.commit_message.clone(),
            commit_type: self.commit_type.clone(),
            amend_commit: self.amend_commit,
            selected_git_path: self.selected_git_path.clone(),
            lifecycle_view: self.worktree_lifecycle_view(),
            lifecycle_confirmation: self.lifecycle_confirmation.clone(),
            lifecycle_loading: self.lifecycle_loading,
            lifecycle_action_pending: self.lifecycle_action_pending,
            lifecycle_mutations_available: self.backend.as_ref().is_some_and(|backend| {
                backend
                    .capabilities()
                    .git
                    .supports(BackendOperation::GitWorktreeLifecycleMutate)
            }),
        }
    }

    fn set_lifecycle_confirmation(&mut self, confirmation: Option<WorktreeLifecycleConfirmation>) {
        self.lifecycle_confirmation = confirmation;
        self.lifecycle_confirmation_revision = self
            .lifecycle_confirmation_revision
            .saturating_add(1)
            .max(1);
    }

    fn take_lifecycle_confirmation(&mut self) -> Option<WorktreeLifecycleConfirmation> {
        let confirmation = self.lifecycle_confirmation.take();
        if confirmation.is_some() {
            self.lifecycle_confirmation_revision = self
                .lifecycle_confirmation_revision
                .saturating_add(1)
                .max(1);
        }
        confirmation
    }

    fn set_preview_panel_fullscreen(&mut self, fullscreen: bool, cx: &mut Context<Self>) {
        if self.preview_panel_fullscreen == fullscreen {
            return;
        }
        self.preview_panel_fullscreen = fullscreen;
        cx.emit(CodeWorkbenchEvent::LayoutChanged { fullscreen });
    }

    pub(crate) fn set_preview_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.preview_visible == visible {
            return;
        }
        self.preview_visible = visible;
        self.sync_terminal_surface_activity(cx);
    }

    pub(crate) fn set_workspace_surface_visibility(
        &mut self,
        files_visible: bool,
        git_visible: bool,
        cx: &mut Context<Self>,
    ) {
        let files_became_visible = files_visible && !self.files_surface_visible;
        let git_became_visible = git_visible && !self.git_surface_visible;
        self.files_surface_visible = files_visible;
        self.git_surface_visible = git_visible;
        if files_became_visible {
            self.refresh_file_tree(cx);
        }
        if git_became_visible {
            self.refresh_git(cx);
        }
    }

    fn sync_terminal_surface_activity(&mut self, cx: &mut Context<Self>) {
        let desired = if self.preview_visible {
            self.preview
                .pane_ids()
                .into_iter()
                .filter_map(|pane_id| self.preview.active_tab_id(pane_id))
                .filter_map(|tab_id| self.preview.tabs.get(tab_id))
                .filter_map(|tab| match &tab.target {
                    PreviewTarget::Terminal { terminal_id }
                        if self.terminal_surfaces.contains_key(terminal_id) =>
                    {
                        Some(terminal_id.clone())
                    }
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        if desired == self.active_terminal_surface_ids {
            return;
        }

        let deactivated = self
            .active_terminal_surface_ids
            .difference(&desired)
            .filter_map(|terminal_id| self.terminal_surfaces.get(terminal_id).cloned())
            .collect::<Vec<_>>();
        let activated = desired
            .difference(&self.active_terminal_surface_ids)
            .filter_map(|terminal_id| self.terminal_surfaces.get(terminal_id).cloned())
            .collect::<Vec<_>>();
        self.active_terminal_surface_ids = desired;

        for surface in deactivated {
            surface.update(cx, |surface, cx| surface.set_active(false, cx));
        }
        for surface in activated {
            surface.update(cx, |surface, cx| surface.set_active(true, cx));
        }
    }

    pub(crate) fn persisted_state(&self) -> CodeWorkbenchPersistedState {
        self.persisted_state_with_recovery(None)
    }

    pub(crate) fn persisted_state_for_exit(&self) -> CodeWorkbenchPersistedState {
        self.persisted_state_with_recovery(Some(self.editors.recovery_snapshot()))
    }

    fn persisted_state_with_recovery(
        &self,
        recovery: Option<EditorRecoverySnapshot>,
    ) -> CodeWorkbenchPersistedState {
        let mut preview = self.preview.clone();
        // Fullscreen is a session-local view state in the Tauri workbench.
        preview.fullscreen_tab_id = None;
        preview.normalize();
        CodeWorkbenchPersistedState {
            preview,
            recovery,
            workspace_id: self
                .workspace
                .as_ref()
                .map(|workspace| workspace.id.as_str().to_string()),
            selected_file_path: self.selected_file_path.clone(),
            selected_git_path: self.selected_git_path.clone(),
            selected_terminal_id: self.selected_terminal_id.clone(),
        }
    }

    pub fn set_code_font(&mut self, family: String, size: u16, cx: &mut Context<Self>) {
        self.code_font_family = family;
        self.code_font_size = size.clamp(10, 24);
        self.preview_diff_lists.clear();
        self.preview_commit_lists.clear();
        cx.notify();
    }

    pub fn sync_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commit_placeholder = git_commit_placeholder(&self.commit_type);
        self.commit_message.update(cx, |input, cx| {
            input.set_placeholder(commit_placeholder, window, cx)
        });
        for binding in self.editor_bindings.values() {
            binding.input.update(cx, |input, cx| {
                input.set_placeholder(
                    locale::text("Loading file", "正在加载文件", "正在載入檔案"),
                    window,
                    cx,
                )
            });
        }
        cx.notify();
    }

    pub fn restore_persisted_state(
        &mut self,
        mut preview: PreviewState,
        recovery: EditorRecoverySnapshot,
        workspace_id: Option<String>,
        code_font_family: String,
        code_font_size: u16,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(
            self.workspace.is_none(),
            "persisted state must be restored before a runtime workspace is selected"
        );
        preview.normalize();
        preview.fullscreen_tab_id = None;
        let mut editors = EditorBufferRegistry::default();
        editors.restore_recovery(recovery);
        self.preview = preview;
        self.set_preview_panel_fullscreen(false, cx);
        self.preview_tab_scrolls.clear();
        self.markdown_scrolls.clear();
        self.preview_revealed_tab_ids.clear();
        self.pending_file_search_reveal = None;
        self.pending_file_search_directory_reveal = None;
        self.preview_diff_lists.clear();
        self.preview_commit_lists.clear();
        self.preview_commit_focus_requests.clear();
        self.git_preview_errors.clear();
        self.editors = editors;
        self.editor_bindings.clear();
        self.editor_subscriptions.clear();
        self.recovery_persist_generation = self.recovery_persist_generation.wrapping_add(1);
        self.recovery_persist_task = None;
        self.restored_workspace_id = workspace_id;
        self.restore_hydration_scheduled = false;
        self.code_font_family = code_font_family;
        self.code_font_size = code_font_size.clamp(10, 24);
        self.preview_diff_lists.clear();
        self.preview_commit_lists.clear();
        self.sync_terminal_surface_activity(cx);
        cx.notify();
    }

    pub fn sync_workspace(
        &mut self,
        runtime: Arc<DesktopRuntime>,
        workspace_id: WorkspaceId,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.id == workspace_id)
        {
            self.runtime = Some(runtime);
            return;
        }
        if self.editors.dirty_paths().next().is_some() && self.workspace.is_some() {
            self.pending_workspace = Some(PendingWorkspace {
                runtime,
                id: workspace_id,
                root,
            });
            self.error = Some(
                "Workspace switch is waiting because one or more editor buffers are dirty"
                    .to_string(),
            );
            cx.notify();
            return;
        }
        self.apply_workspace(runtime, workspace_id, root, cx);
    }

    pub(crate) fn set_backend(&mut self, backend: BackendFacade, cx: &mut Context<Self>) {
        self.backend = Some(backend);
        if self.workspace.is_some() {
            self.load_worktree_lifecycle(cx);
        }
    }

    pub(crate) fn clear_backend(&mut self, cx: &mut Context<Self>) {
        self.backend = None;
        self.lifecycle_task = None;
        self.lifecycle_action_task = None;
        self.lifecycle_loading = false;
        self.lifecycle_reload_requested = false;
        self.lifecycle_action_pending = false;
        self.set_lifecycle_confirmation(None);
        cx.notify();
    }

    pub(crate) fn discard_and_apply_pending_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_workspace.take() else {
            return;
        };
        self.editors = EditorBufferRegistry::default();
        self.apply_workspace(pending.runtime, pending.id, pending.root, cx);
    }

    fn apply_workspace(
        &mut self,
        runtime: Arc<DesktopRuntime>,
        workspace_id: WorkspaceId,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.persist(cx);
        let preserve_restored = self.workspace.is_none()
            && self.restored_workspace_id.as_deref() == Some(workspace_id.as_str());
        if !preserve_restored {
            self.preview = PreviewState::default();
            self.set_preview_panel_fullscreen(false, cx);
            self.editors = EditorBufferRegistry::default();
            self.editor_bindings.clear();
            self.editor_subscriptions.clear();
            self.selected_file_path = None;
            self.selected_git_path = None;
            self.selected_terminal_id = None;
        }
        self.preview_tab_scrolls.clear();
        self.markdown_scrolls.clear();
        self.preview_revealed_tab_ids.clear();
        self.pending_file_search_reveal = None;
        self.pending_file_search_directory_reveal = None;
        self.preview_diff_lists.clear();
        self.preview_commit_lists.clear();
        self.preview_commit_focus_requests.clear();
        self.git_preview_errors.clear();
        self.restored_workspace_id = None;
        self.workspace_generation = self.workspace_generation.saturating_add(1).max(1);
        self.workspace = Some(WorkbenchWorkspace {
            id: workspace_id.clone(),
            root,
            generation: self.workspace_generation,
        });
        self.terminals = runtime
            .list_terminals(&workspace_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|terminal| terminal.status == TerminalStatus::Running)
            .collect();
        self.reconcile_terminal_selection();
        self.terminal_surfaces.clear();
        self.active_terminal_surface_ids.clear();
        self.runtime = Some(runtime);
        self.pending_workspace = None;
        self.presentations.clear();
        self.markdown_edit_paths.clear();
        self.close_all_lifecycles();
        self.file_tasks.clear();
        self.tree_tasks.clear();
        self.tree_refreshing = false;
        self.tree_refresh_task = None;
        self.workspace_poll_task = None;
        self.git_status_poll_task = None;
        self.lifecycle_task = None;
        self.lifecycle_action_task = None;
        self.lifecycle_snapshot = None;
        self.set_lifecycle_confirmation(None);
        self.lifecycle_loading = false;
        self.lifecycle_reload_requested = false;
        self.lifecycle_action_pending = false;
        self.diff_tasks.clear();
        self.commit_detail_tasks.clear();
        self.file_tree.reset_workspace(workspace_id.clone());
        let root_name = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.root.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("workspace")
            .to_string();
        self.file_tree.set_root_name(root_name);
        self.git.reset_workspace(workspace_id);
        self.error = None;
        self.note = Some("Workspace files and Git state are loading".to_string());
        self.load_tree(cx);
        self.refresh_git(cx);
        self.start_workspace_polling(cx);
        self.persist(cx);
        self.persist_editor_recovery(cx);
        cx.notify();
    }

    fn reconcile_file_selection(&mut self) {
        if let Some(path) = self.selected_file_path.clone() {
            self.file_tree.select(&path, false, false);
        }
    }

    fn reconcile_git_selection(&mut self) {
        let Some(path) = self.selected_git_path.as_deref() else {
            return;
        };
        if self
            .git
            .status
            .as_ref()
            .is_some_and(|status| !status.changes.iter().any(|change| change.path == path))
        {
            self.selected_git_path = None;
        }
    }

    fn reconcile_terminal_selection(&mut self) {
        if self.selected_terminal_id.as_ref().is_some_and(|selected| {
            !self
                .terminals
                .iter()
                .any(|terminal| terminal.id.as_str() == selected)
        }) {
            self.selected_terminal_id = self
                .terminals
                .first()
                .map(|terminal| terminal.id.as_str().to_string());
        }
    }

    pub(crate) fn restore_navigation_selection(
        &mut self,
        selected_file_path: Option<String>,
        selected_git_path: Option<String>,
        selected_terminal_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_file_path = selected_file_path;
        self.selected_git_path = selected_git_path;
        self.selected_terminal_id = selected_terminal_id;
        self.reconcile_file_selection();
        self.reconcile_git_selection();
        self.reconcile_terminal_selection();
        if let Some(terminal_id) = self
            .selected_terminal_id
            .as_deref()
            .and_then(|terminal_id| TerminalId::parse(terminal_id).ok())
        {
            let tab_id = format!("terminal:{terminal_id}");
            if self.preview.tabs.contains_key(&tab_id)
                && self.ensure_terminal_surface(&terminal_id, window, cx)
            {
                self.preview.focus(&tab_id);
                self.activate_tab(&tab_id, cx);
            }
        }
        self.persist(cx);
        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        let state = self.persisted_state();
        self.send_persisted_state(state, cx);
    }

    fn persist_editor_recovery(&mut self, cx: &mut Context<Self>) {
        self.recovery_persist_generation = self.recovery_persist_generation.wrapping_add(1);
        let generation = self.recovery_persist_generation;
        let background = cx.background_executor().clone();
        self.recovery_persist_task = Some(cx.spawn(async move |entity, cx| {
            background.timer(EDITOR_RECOVERY_PERSIST_DELAY).await;
            let _ = entity.update(cx, |this, cx| {
                if this.recovery_persist_generation != generation {
                    return;
                }
                this.recovery_persist_task = None;
                let state = this.persisted_state_for_exit();
                this.send_persisted_state(state, cx);
            });
        }));
    }

    fn send_persisted_state(&self, state: CodeWorkbenchPersistedState, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.as_ref() {
            let parent = parent.clone();
            cx.defer(move |cx| {
                let _ = parent.update(cx, |parent, cx| {
                    parent.persist_code_workbench_state(state, cx)
                });
            });
        }
    }

    fn request_preview_panel(&self, cx: &mut Context<Self>) {
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| parent.reveal_code_preview(cx));
        });
    }

    fn request_close_preview_panel(&self, cx: &mut Context<Self>) {
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| parent.close_code_preview(cx));
        });
    }

    fn request_new_preview_terminal(
        &self,
        window_handle: gpui::AnyWindowHandle,
        pane_id: Option<String>,
        cwd_source: Option<(String, FileEntryKind)>,
        cx: &mut Context<Self>,
    ) {
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| {
                parent.create_preview_terminal(
                    window_handle,
                    pane_id.clone(),
                    cwd_source.clone(),
                    cx,
                )
            });
        });
    }

    fn close_preview_panel_if_empty(&mut self, cx: &mut Context<Self>) {
        if !self.preview.tabs.is_empty() {
            return;
        }
        self.set_preview_panel_fullscreen(false, cx);
        self.preview.set_fullscreen(None);
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| parent.close_code_preview(cx));
        });
    }

    fn schedule_restore_hydration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restore_hydration_scheduled || self.workspace.is_none() {
            return;
        }
        let paths = self
            .preview
            .tabs
            .values()
            .filter_map(|tab| match &tab.target {
                PreviewTarget::File { path }
                    if !self.editor_bindings.contains_key(path)
                        && !self.presentations.contains_key(path) =>
                {
                    Some(path.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let terminal_ids = self
            .preview
            .tabs
            .values()
            .filter_map(|tab| match &tab.target {
                PreviewTarget::Terminal { terminal_id }
                    if !self.terminal_surfaces.contains_key(terminal_id) =>
                {
                    TerminalId::parse(terminal_id).ok()
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let diff_keys = self
            .preview
            .tabs
            .values()
            .filter_map(|tab| match &tab.target {
                PreviewTarget::GitDiff { path, staged } => Some(GitSelectionKey {
                    path: path.clone(),
                    staged: *staged,
                }),
                _ => None,
            })
            .filter(|key| !self.git.diffs.contains_key(key) && !self.diff_tasks.contains_key(key))
            .collect::<BTreeSet<_>>();
        let commit_hashes = self
            .preview
            .tabs
            .values()
            .filter_map(|tab| match &tab.target {
                PreviewTarget::GitCommit { commit_hash, .. } => Some(commit_hash.clone()),
                _ => None,
            })
            .filter(|hash| {
                !self.git.commit_patch_ready(hash) && !self.commit_detail_tasks.contains_key(hash)
            })
            .collect::<BTreeSet<_>>();
        if paths.is_empty()
            && terminal_ids.is_empty()
            && diff_keys.is_empty()
            && commit_hashes.is_empty()
        {
            return;
        }
        self.restore_hydration_scheduled = true;
        let active_tab = self
            .preview
            .active_tab_id(&self.preview.focused_pane_id)
            .map(str::to_string);
        let entity = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.restore_hydration_scheduled = false;
                let selected_file_path = this.selected_file_path.clone();
                for path in paths {
                    if let Some(buffer) = this.editors.buffers.get(&path).cloned() {
                        let content = buffer.content;
                        let input = this.ensure_editor_binding(&path, window, cx);
                        input.update(cx, |input, cx| {
                            input.set_highlighter(language_for_path(&path), cx);
                            input.set_value(content.clone(), window, cx);
                        });
                        if content_preview_kind_for_path(&path) == ContentPreviewKind::Markdown {
                            this.start_markdown_parse(path, content, cx);
                        }
                    } else {
                        this.open_file(path, false, window, cx);
                    }
                }
                this.selected_file_path = selected_file_path;
                this.reconcile_file_selection();
                for terminal_id in terminal_ids {
                    this.ensure_terminal_surface(&terminal_id, window, cx);
                }
                for key in diff_keys {
                    this.load_diff(key, cx);
                }
                for hash in commit_hashes {
                    this.load_commit_detail(hash, cx);
                }
                if let Some(active_tab) = active_tab {
                    this.preview.focus(&active_tab);
                    this.activate_tab(&active_tab, cx);
                }
                cx.notify();
            });
        });
    }

    pub(crate) fn load_tree(&mut self, cx: &mut Context<Self>) {
        self.load_tree_path(String::new(), cx);
    }

    fn start_workspace_polling(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let file_tree_interval = Duration::from_millis(runtime.polling_policy().file_tree_ms);
        let background = cx.background_executor().clone();
        self.workspace_poll_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                let interval = entity
                    .read_with(cx, |this, _| {
                        if this.files_surface_visible {
                            file_tree_interval
                        } else {
                            HIDDEN_WORKSPACE_POLL_INTERVAL
                        }
                    })
                    .unwrap_or(HIDDEN_WORKSPACE_POLL_INTERVAL);
                background.timer(interval).await;
                let active = entity
                    .update(cx, |this, cx| {
                        if this.workspace.is_none() {
                            return false;
                        }
                        if this.files_surface_visible {
                            this.refresh_file_tree(cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        }));

        let background = cx.background_executor().clone();
        self.git_status_poll_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                let interval = entity
                    .read_with(cx, |this, _| {
                        if this.git_surface_visible {
                            GIT_STATUS_POLL_INTERVAL
                        } else {
                            HIDDEN_WORKSPACE_POLL_INTERVAL
                        }
                    })
                    .unwrap_or(HIDDEN_WORKSPACE_POLL_INTERVAL);
                background.timer(interval).await;
                let active = entity
                    .update(cx, |this, cx| {
                        if this.workspace.is_none() {
                            return false;
                        }
                        if this.git_surface_visible
                            && !this.status_loading
                            && !this.file_mutation_pending
                        {
                            this.load_git_status(cx);
                            this.load_worktree_lifecycle(cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        }));
    }

    fn refresh_file_tree(&mut self, cx: &mut Context<Self>) {
        if self.tree_loading
            || self.tree_refreshing
            || self.file_mutation_pending
            || !self.tree_tasks.is_empty()
        {
            return;
        }
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let mut paths = vec![String::new()];
        paths.extend(self.file_tree.expanded_directory_paths());
        let ticket = self.file_tree.begin_refresh();
        self.tree_refreshing = true;
        let request_workspace_id = workspace.id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            paths
                .into_iter()
                .map(|path| {
                    let request = FileTreeRequest {
                        workspace_id: request_workspace_id.clone(),
                        path: (!path.is_empty()).then_some(path.clone()),
                        max_depth: Some(if path.is_empty() { 4 } else { 8 }),
                        include_hidden: true,
                    };
                    let result = runtime.files().list_native_tree(&request);
                    (path, result)
                })
                .collect::<Vec<_>>()
        });
        self.tree_refresh_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if this.workspace.as_ref().map(|current| current.generation)
                    != Some(workspace.generation)
                {
                    return;
                }
                this.tree_refreshing = false;
                let Ok(results) = outcome else {
                    return;
                };
                let root_loaded = results
                    .iter()
                    .any(|(path, result)| path.is_empty() && result.is_ok());
                if !root_loaded {
                    return;
                }
                let mut entries = Vec::new();
                let mut failed_subtrees = Vec::new();
                for (path, result) in results {
                    match result {
                        Ok(path_entries) => entries.extend(path_entries),
                        Err(_) if !path.is_empty() => failed_subtrees.push(path),
                        Err(_) => {}
                    }
                }
                if this.file_tree.apply_refresh_entries(
                    &workspace.id,
                    ticket,
                    entries,
                    &failed_subtrees,
                ) {
                    this.reconcile_file_selection();
                    cx.notify();
                }
            });
        }));
    }

    fn load_tree_path(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let path = normalized_relative_path(&path).unwrap_or_default();
        let ticket = self.file_tree.begin_load(&path);
        let request = FileTreeRequest {
            workspace_id: workspace.id.clone(),
            path: (!path.is_empty()).then_some(path.clone()),
            max_depth: Some(if path.is_empty() { 4 } else { 8 }),
            include_hidden: true,
        };
        if path.is_empty() {
            self.tree_loading = true;
        }
        self.error = None;
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { runtime.files().list_native_tree(&request) },
            );
        let task_path = path.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.tree_tasks.remove(&task_path);
                if this.workspace.as_ref().map(|current| current.generation)
                    != Some(workspace.generation)
                {
                    return;
                }
                if task_path.is_empty() {
                    this.tree_loading = false;
                }
                match outcome {
                    Ok(Ok(entries)) => {
                        if this
                            .file_tree
                            .apply_entries(&workspace.id, ticket, &task_path, entries)
                        {
                            this.reconcile_file_selection();
                            this.continue_file_search_directory_reveal(cx);
                        }
                        this.note = None;
                    }
                    Ok(Err(error)) => {
                        this.file_tree.fail_load(ticket, &task_path, &error.code);
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.file_tree
                            .fail_load(ticket, &task_path, "file_tree_task_failed");
                        this.error = Some(format!("file tree task failed: {error}"));
                    }
                }
                cx.notify();
            });
        });
        self.tree_tasks.insert(path, task);
    }

    pub(crate) fn toggle_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.file_tree.toggle_expanded(&path) {
            return;
        }
        if self.file_tree.is_expanded(&path)
            && !matches!(
                self.file_tree.load_state(&path),
                FileTreeLoadState::Loaded | FileTreeLoadState::Loading
            )
        {
            self.load_tree_path(path, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_directory_chain(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let expanding = !self.file_tree.chain_is_expanded(&paths);
        if !self.file_tree.set_chain_expanded(&paths, expanding) {
            return;
        }
        if expanding {
            for path in paths {
                if path.is_empty()
                    || matches!(
                        self.file_tree.load_state(&path),
                        FileTreeLoadState::Loaded | FileTreeLoadState::Loading
                    )
                {
                    continue;
                }
                self.load_tree_path(path, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn select_directory_segment(
        &mut self,
        path: String,
        path_chain: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.file_tree.select_directory_segment(&path, &path_chain) {
            return;
        }
        if !path.is_empty()
            && !matches!(
                self.file_tree.load_state(&path),
                FileTreeLoadState::Loaded | FileTreeLoadState::Loading
            )
        {
            self.load_tree_path(path, cx);
        }
        cx.notify();
    }

    pub(crate) fn retry_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if matches!(self.file_tree.load_state(&path), FileTreeLoadState::Loading) {
            return;
        }
        self.load_tree_path(path, cx);
        cx.notify();
    }

    pub(crate) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        self.load_git_status(cx);
        self.load_worktree_lifecycle(cx);
        self.load_branches(cx);
        if self.git.mode == GitWorkbenchMode::History {
            self.load_history(false, cx);
        }
    }

    pub(crate) fn git_status(&self) -> Option<&GitStatusSummary> {
        self.git.status.as_ref()
    }

    pub(crate) fn workspace_is_active(&self, workspace_id: &WorkspaceId) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| &workspace.id == workspace_id)
    }

    fn load_git_status(&mut self, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let Some(ticket) = self.git.begin_query(GitQueryKind::Status, "status") else {
            return;
        };
        self.status_loading = true;
        let workspace_id = workspace.id.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { runtime.git().status(&workspace_id) });
        self.status_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.status_loading = false;
                match outcome {
                    Ok(Ok(status)) => {
                        if this.git.apply_status(&ticket, status.clone()) {
                            this.file_tree.set_git_changes(&status.changes);
                            this.reconcile_git_selection();
                            if this.git.mode == GitWorkbenchMode::History
                                && this.ensure_history_ref_filter()
                            {
                                this.load_history(false, cx);
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        this.git.fail_query(&ticket, &error.code);
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => this.error = Some(format!("Git status task failed: {error}")),
                }
                cx.notify();
            });
        }));
    }

    fn worktree_lifecycle_view(&self) -> Option<WorktreeLifecycleView> {
        let workspace = self.workspace.as_ref()?;
        WorktreeLifecycleView::from_snapshot(&workspace.id, self.lifecycle_snapshot.as_ref()?)
    }

    fn apply_worktree_lifecycle_snapshot(&mut self, snapshot: GitWorktreeLifecycleSnapshot) {
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        if snapshot.workspace_id != workspace.id {
            return;
        }
        let conflict_paths = WorktreeLifecycleView::from_snapshot(&workspace.id, &snapshot)
            .filter(WorktreeLifecycleView::owns_conflict_resolution)
            .and_then(|view| view.operation)
            .filter(|operation| {
                matches!(
                    operation.status,
                    GitWorktreeOperationStatus::NeedsResolution
                        | GitWorktreeOperationStatus::NeedsAttention
                )
            })
            .map(|operation| {
                operation
                    .detail
                    .conflicts
                    .into_iter()
                    .filter(|conflict| !conflict.resolved)
                    .map(|conflict| conflict.path)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.git.set_lifecycle_conflict_paths(conflict_paths);
        self.lifecycle_snapshot = Some(snapshot);
    }

    fn load_worktree_lifecycle(&mut self, cx: &mut Context<Self>) {
        if self.lifecycle_loading {
            self.lifecycle_reload_requested = true;
            return;
        }
        let (Some(backend), Some(workspace)) = (self.backend.clone(), self.workspace.clone())
        else {
            return;
        };
        if !backend
            .capabilities()
            .git
            .supports(BackendOperation::GitWorktreeRead)
        {
            self.lifecycle_snapshot = None;
            self.git.set_lifecycle_conflict_paths(Vec::new());
            return;
        }
        self.lifecycle_loading = true;
        let workspace_fence = WorkspaceGenerationFence::capture(&workspace);
        let workspace_id = workspace.id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.git().git_worktree_snapshot(workspace_id).await
        });
        self.lifecycle_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if !workspace_fence.matches(this.workspace.as_ref()) {
                    return;
                }
                this.lifecycle_loading = false;
                let reload = std::mem::take(&mut this.lifecycle_reload_requested);
                match outcome {
                    Ok(Ok(snapshot)) => this.apply_worktree_lifecycle_snapshot(snapshot),
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("Worktree lifecycle task failed: {error}"));
                    }
                }
                if reload {
                    this.load_worktree_lifecycle(cx);
                }
                cx.notify();
            });
        }));
    }

    fn lifecycle_mutation_backend(&mut self) -> Option<BackendFacade> {
        let backend = self.backend.clone()?;
        if !backend
            .capabilities()
            .git
            .supports(BackendOperation::GitWorktreeLifecycleMutate)
        {
            self.error = Some("worktree_lifecycle_mutation_unsupported".to_string());
            return None;
        }
        Some(backend)
    }

    fn request_parent_lifecycle_refresh(&self, cx: &mut Context<Self>) {
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| parent.refresh_workspace_contexts(cx));
        });
    }

    fn request_operation_target_focus(
        &self,
        operation: &GitWorktreeOperationRecord,
        cx: &mut Context<Self>,
    ) {
        if !matches!(
            operation.status,
            GitWorktreeOperationStatus::NeedsResolution
                | GitWorktreeOperationStatus::NeedsAttention
        ) {
            return;
        }
        let (Some(parent), Some(target_workspace_id)) =
            (self.parent.clone(), operation.target_workspace_id.clone())
        else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| {
                parent.focus_worktree_operation_target(target_workspace_id.clone(), cx)
            });
        });
    }

    fn run_worktree_operation<F, Fut>(
        &mut self,
        operation: F,
        retry_confirmation: Option<WorktreeLifecycleConfirmation>,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(BackendFacade) -> Fut + 'static,
        Fut: Future<Output = Result<GitWorktreeOperationRecord, BackendError>> + Send + 'static,
    {
        if self.lifecycle_action_pending {
            self.error = Some("Another Worktree lifecycle action is already running".to_string());
            cx.notify();
            return;
        }
        let Some(backend) = self.lifecycle_mutation_backend() else {
            cx.notify();
            return;
        };
        let Some(workspace_fence) = self
            .workspace
            .as_ref()
            .map(WorkspaceGenerationFence::capture)
        else {
            return;
        };
        self.lifecycle_action_pending = true;
        self.set_lifecycle_confirmation(None);
        self.error = None;
        self.note = Some("Worktree lifecycle action is running".to_string());
        let runner = gpui_tokio::Tokio::spawn(cx, operation(backend));
        self.lifecycle_action_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if !workspace_fence.matches(this.workspace.as_ref()) {
                    return;
                }
                this.lifecycle_action_pending = false;
                match outcome {
                    Ok(Ok(operation)) => {
                        this.note = Some("Worktree lifecycle action completed".to_string());
                        this.request_operation_target_focus(&operation, cx);
                        this.load_git_status(cx);
                        this.load_worktree_lifecycle(cx);
                        this.request_parent_lifecycle_refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.set_lifecycle_confirmation(
                            if worktree_plan_error_requires_refresh(&error.code) {
                                None
                            } else {
                                retry_confirmation.clone()
                            },
                        );
                        this.note = None;
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        this.load_worktree_lifecycle(cx);
                    }
                    Err(error) => {
                        this.set_lifecycle_confirmation(retry_confirmation.clone());
                        this.note = None;
                        this.error = Some(format!("Worktree lifecycle task failed: {error}"));
                        this.load_worktree_lifecycle(cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn run_lifecycle_confirmation_query<T, F, Fut, Build>(
        &mut self,
        operation: F,
        build: Build,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        F: FnOnce(BackendFacade) -> Fut + 'static,
        Fut: Future<Output = Result<T, BackendError>> + Send + 'static,
        Build: FnOnce(T) -> WorktreeLifecycleConfirmation + 'static,
    {
        if self.lifecycle_action_pending {
            return;
        }
        let Some(backend) = self.lifecycle_mutation_backend() else {
            cx.notify();
            return;
        };
        let Some(workspace_fence) = self
            .workspace
            .as_ref()
            .map(WorkspaceGenerationFence::capture)
        else {
            return;
        };
        self.lifecycle_action_pending = true;
        self.set_lifecycle_confirmation(None);
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, operation(backend));
        self.lifecycle_action_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if !workspace_fence.matches(this.workspace.as_ref()) {
                    return;
                }
                this.lifecycle_action_pending = false;
                match outcome {
                    Ok(Ok(value)) => {
                        this.set_lifecycle_confirmation(Some(build(value)));
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("Worktree lifecycle task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn set_worktree_readiness(
        &mut self,
        state: GitWorktreeReadinessState,
        cx: &mut Context<Self>,
    ) {
        if self.lifecycle_action_pending {
            return;
        }
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(backend) = self.lifecycle_mutation_backend() else {
            cx.notify();
            return;
        };
        let Some(workspace_fence) = self
            .workspace
            .as_ref()
            .map(WorkspaceGenerationFence::capture)
        else {
            return;
        };
        let expected = (state == GitWorktreeReadinessState::ReadyToMerge)
            .then_some(view.readiness)
            .flatten();
        let request = GitWorktreeReadinessRequest {
            workspace_id: view.workspace_id,
            state,
            expected_source_head: expected
                .as_ref()
                .map(|readiness| readiness.source_head.clone()),
            expected_dirty_fingerprint: expected
                .as_ref()
                .map(|readiness| readiness.dirty_fingerprint.clone()),
            checks: expected
                .map(|readiness| readiness.checks)
                .unwrap_or_default(),
        };
        self.lifecycle_action_pending = true;
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .git()
                .git_worktree_set_readiness(MutationRequest::new(request))
                .await
        });
        self.lifecycle_action_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if !workspace_fence.matches(this.workspace.as_ref()) {
                    return;
                }
                this.lifecycle_action_pending = false;
                match outcome {
                    Ok(Ok(_)) => {
                        this.note = Some("Worktree readiness updated".to_string());
                        this.load_worktree_lifecycle(cx);
                        this.request_parent_lifecycle_refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("Worktree readiness task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn request_worktree_merge_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let strategy = view
            .operation
            .as_ref()
            .and_then(|operation| operation.detail.merge_strategy)
            .filter(|strategy| *strategy != GitWorktreeMergeStrategy::Unknown)
            .unwrap_or_default();
        let Some(managed) = view.managed else {
            return;
        };
        let request = GitWorktreeMergeRequest {
            workspace_id: view.workspace_id,
            source_path: managed.worktree_path,
            target_workspace_id: managed.target_workspace_id,
            strategy: (strategy == GitWorktreeMergeStrategy::RebaseAndMerge).then_some(strategy),
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
        };
        self.run_lifecycle_confirmation_query(
            move |backend| async move { backend.git().git_worktree_merge_plan(request).await },
            move |plan| WorktreeLifecycleConfirmation::Merge { plan, strategy },
            cx,
        );
    }

    pub(crate) fn set_worktree_merge_strategy(
        &mut self,
        strategy: GitWorktreeMergeStrategy,
        cx: &mut Context<Self>,
    ) {
        let Some(WorktreeLifecycleConfirmation::Merge {
            strategy: selected, ..
        }) = self.lifecycle_confirmation.as_mut()
        else {
            return;
        };
        if matches!(
            strategy,
            GitWorktreeMergeStrategy::NoFfMerge | GitWorktreeMergeStrategy::RebaseAndMerge
        ) {
            *selected = strategy;
            cx.notify();
        }
    }

    pub(crate) fn request_worktree_archive_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(managed) = view.managed else {
            return;
        };
        let request = GitWorktreeArchiveRequest {
            workspace_id: view.workspace_id,
            worktree_path: managed.worktree_path,
            expected_head: None,
            preflight_revision: None,
        };
        let request_for_query = request.clone();
        self.run_lifecycle_confirmation_query(
            move |backend| async move {
                backend
                    .git()
                    .git_worktree_archive_preflight(request_for_query)
                    .await
            },
            move |preflight| {
                let mut request = request;
                request.expected_head = preflight.source_head.clone();
                request.preflight_revision = Some(preflight.revision.clone());
                WorktreeLifecycleConfirmation::Archive { request, preflight }
            },
            cx,
        );
    }

    pub(crate) fn request_worktree_restore_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(managed) = view.managed else {
            return;
        };
        let request = GitWorktreeRestoreRequest {
            workspace_id: view.workspace_id,
            worktree_id: managed.worktree_id,
            preflight_revision: None,
        };
        let request_for_query = request.clone();
        self.run_lifecycle_confirmation_query(
            move |backend| async move {
                backend
                    .git()
                    .git_worktree_restore_preflight(request_for_query)
                    .await
            },
            move |preflight| {
                let mut request = request;
                request.preflight_revision = Some(preflight.revision.clone());
                WorktreeLifecycleConfirmation::Restore { request, preflight }
            },
            cx,
        );
    }

    pub(crate) fn request_worktree_discard_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(managed) = view.managed else {
            return;
        };
        let force = self.git.status.as_ref().is_some_and(|status| status.dirty);
        let request = GitWorktreeDiscardRequest {
            workspace_id: view.workspace_id,
            worktree_path: managed.worktree_path,
            force,
            expected_head: None,
            preflight_revision: None,
        };
        let request_for_query = request.clone();
        self.run_lifecycle_confirmation_query(
            move |backend| async move {
                backend
                    .git()
                    .git_worktree_discard_preflight(request_for_query)
                    .await
            },
            move |preflight| {
                let mut request = request;
                request.expected_head = preflight.source_head.clone();
                request.preflight_revision = Some(preflight.revision.clone());
                WorktreeLifecycleConfirmation::Discard { request, preflight }
            },
            cx,
        );
    }

    pub(crate) fn request_worktree_abort_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(operation) = view.operation else {
            return;
        };
        self.set_lifecycle_confirmation(Some(WorktreeLifecycleConfirmation::Abort(
            GitWorktreeOperationRequest {
                operation_id: operation.operation_id,
                workspace_id: view.workspace_id,
            },
        )));
        cx.notify();
    }

    pub(crate) fn request_worktree_continue_confirmation(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(operation) = view.operation else {
            return;
        };
        self.set_lifecycle_confirmation(Some(WorktreeLifecycleConfirmation::Continue(
            GitWorktreeOperationRequest {
                operation_id: operation.operation_id,
                workspace_id: view.workspace_id,
            },
        )));
        cx.notify();
    }

    pub(crate) fn cancel_worktree_lifecycle_confirmation(&mut self, cx: &mut Context<Self>) {
        self.set_lifecycle_confirmation(None);
        cx.notify();
    }

    pub(crate) fn confirm_worktree_lifecycle_action(&mut self, cx: &mut Context<Self>) {
        let Some(confirmation) = self.take_lifecycle_confirmation() else {
            return;
        };
        match confirmation.clone() {
            WorktreeLifecycleConfirmation::Merge { plan, strategy } => {
                if !plan.preflight.allowed {
                    self.set_lifecycle_confirmation(Some(confirmation));
                    self.error = Some("worktree_preflight_blocked".to_string());
                    cx.notify();
                    return;
                }
                let request = GitWorktreeMergeRequest {
                    workspace_id: plan.source_workspace_id,
                    source_path: plan.source_path,
                    target_workspace_id: Some(plan.target_workspace_id),
                    strategy: Some(strategy),
                    expected_source_head: Some(plan.source_head),
                    expected_target_head: Some(plan.target_head),
                    preflight_revision: Some(plan.preflight.revision),
                };
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_merge(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
            WorktreeLifecycleConfirmation::Archive { request, preflight } => {
                if !preflight.allowed {
                    self.set_lifecycle_confirmation(Some(confirmation));
                    self.error = Some("worktree_preflight_blocked".to_string());
                    cx.notify();
                    return;
                }
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_archive(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
            WorktreeLifecycleConfirmation::Restore { request, preflight } => {
                if !preflight.allowed {
                    self.set_lifecycle_confirmation(Some(confirmation));
                    self.error = Some("worktree_preflight_blocked".to_string());
                    cx.notify();
                    return;
                }
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_restore(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
            WorktreeLifecycleConfirmation::Discard { request, preflight } => {
                if !preflight.allowed {
                    self.set_lifecycle_confirmation(Some(confirmation));
                    self.error = Some("worktree_preflight_blocked".to_string());
                    cx.notify();
                    return;
                }
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_discard(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
            WorktreeLifecycleConfirmation::Continue(request) => {
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_continue_merge(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
            WorktreeLifecycleConfirmation::Abort(request) => {
                self.run_worktree_operation(
                    move |backend| async move {
                        backend
                            .git()
                            .git_worktree_abort_merge(MutationRequest::new(request))
                            .await
                    },
                    Some(confirmation),
                    cx,
                );
            }
        }
    }

    pub(crate) fn resolve_worktree_conflict(
        &mut self,
        path: String,
        version: GitWorktreeConflictVersion,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(operation) = view.operation else {
            return;
        };
        let request = GitWorktreeConflictResolveRequest {
            operation_id: operation.operation_id,
            workspace_id: view.workspace_id,
            path,
            version,
        };
        self.run_worktree_operation(
            move |backend| async move {
                backend
                    .git()
                    .git_worktree_resolve_conflict(MutationRequest::new(request))
                    .await
            },
            None,
            cx,
        );
    }

    pub(crate) fn stage_worktree_conflict(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(operation) = view.operation else {
            return;
        };
        let request = GitWorktreeConflictStageRequest {
            operation_id: operation.operation_id,
            workspace_id: view.workspace_id,
            paths: vec![path],
        };
        self.run_worktree_operation(
            move |backend| async move {
                backend
                    .git()
                    .git_worktree_stage_conflicts(MutationRequest::new(request))
                    .await
            },
            None,
            cx,
        );
    }

    pub(crate) fn request_worktree_agent_assistance(&self, cx: &mut Context<Self>) {
        let Some(view) = self.worktree_lifecycle_view() else {
            return;
        };
        let Some(operation) = view.operation else {
            return;
        };
        let Some(parent) = self.parent.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = parent.update(cx, |parent, cx| {
                parent.assist_worktree_merge(operation.clone(), cx)
            });
        });
    }

    pub(crate) fn set_git_mode(&mut self, mode: GitWorkbenchMode, cx: &mut Context<Self>) {
        self.right_rail_mode = RightRailMode::Git;
        self.git.set_mode(mode);
        if mode == GitWorkbenchMode::History {
            if self.git.branches.is_none() {
                self.load_branches(cx);
            }
            let filter_changed = self.ensure_history_ref_filter();
            if filter_changed || self.git.history.is_empty() {
                self.load_history(false, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn set_history_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        if self.git.history_filter.ref_name.as_deref() == Some(branch.as_str()) {
            return;
        }
        let mut filter = self.git.history_filter.clone();
        filter.ref_name = Some(branch);
        self.git.set_history_filter(filter);
        self.load_history(false, cx);
    }

    pub(crate) fn set_history_author(&mut self, author: Option<String>, cx: &mut Context<Self>) {
        if self.git.history_filter.author == author {
            return;
        }
        let mut filter = self.git.history_filter.clone();
        filter.author = author;
        self.git.set_history_filter(filter);
        self.load_history(false, cx);
    }

    pub(crate) fn set_history_query(&mut self, query: Option<String>, cx: &mut Context<Self>) {
        let query = query.and_then(|query| {
            let query = query.trim().to_string();
            (!query.is_empty()).then_some(query)
        });
        if self.git.history_filter.query == query {
            return;
        }
        let mut filter = self.git.history_filter.clone();
        filter.query = query;
        self.git.set_history_filter(filter);
        self.load_history(false, cx);
    }

    pub(crate) fn set_history_date_range(
        &mut self,
        authored_after_ms: Option<i64>,
        authored_before_ms: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if self.git.history_filter.authored_after_ms == authored_after_ms
            && self.git.history_filter.authored_before_ms == authored_before_ms
        {
            return;
        }
        let mut filter = self.git.history_filter.clone();
        filter.authored_after_ms = authored_after_ms;
        filter.authored_before_ms = authored_before_ms;
        self.git.set_history_filter(filter);
        self.load_history(false, cx);
    }

    fn ensure_history_ref_filter(&mut self) -> bool {
        if self.git.history_filter.ref_name.is_some() {
            return false;
        }
        let branch = self
            .git
            .branches
            .as_ref()
            .and_then(|response| {
                response
                    .branches
                    .iter()
                    .find(|branch| branch.current)
                    .or_else(|| response.branches.first())
                    .map(|branch| branch.name.clone())
            })
            .or_else(|| {
                self.git
                    .status
                    .as_ref()
                    .and_then(|status| status.branch.clone())
            });
        let Some(branch) = branch else {
            return false;
        };
        let mut filter = self.git.history_filter.clone();
        filter.ref_name = Some(branch);
        self.git.set_history_filter(filter);
        true
    }

    pub(crate) fn load_history(&mut self, append: bool, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        if self.git.history_filter.ref_name.is_none() {
            return;
        }
        let before_commit = append
            .then(|| self.git.history.last().map(|commit| commit.hash.clone()))
            .flatten();
        let key = format!(
            "{}:{}:{}:{}:{}:{}",
            self.git
                .history_filter
                .ref_name
                .as_deref()
                .unwrap_or_default(),
            self.git
                .history_filter
                .author
                .as_deref()
                .unwrap_or_default(),
            self.git.history_filter.query.as_deref().unwrap_or_default(),
            self.git
                .history_filter
                .authored_after_ms
                .unwrap_or_default(),
            self.git
                .history_filter
                .authored_before_ms
                .unwrap_or_default(),
            before_commit.as_deref().unwrap_or_default()
        );
        let Some(ticket) = self.git.begin_query(GitQueryKind::History, key) else {
            return;
        };
        let request = GitHistoryRequest {
            workspace_id: workspace.id,
            limit: Some(60),
            before_commit,
            ref_name: self.git.history_filter.ref_name.clone(),
            author: self.git.history_filter.author.clone(),
            query: self.git.history_filter.query.clone(),
            authored_after_ms: self.git.history_filter.authored_after_ms,
            authored_before_ms: self.git.history_filter.authored_before_ms,
        };
        self.history_loading = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move { runtime.git().history(&request) });
        self.history_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.history_loading = false;
                match outcome {
                    Ok(Ok(history)) => {
                        this.git.apply_history(&ticket, history, append);
                    }
                    Ok(Err(error)) => {
                        this.git.fail_query(&ticket, &error.code);
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => this.error = Some(format!("Git history task failed: {error}")),
                }
                cx.notify();
            });
        }));
    }

    fn load_branches(&mut self, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let Some(ticket) = self.git.begin_query(GitQueryKind::Branches, "branches") else {
            return;
        };
        let workspace_id = workspace.id;
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { runtime.git().branch_list(&workspace_id) });
        self.branch_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(branches)) => {
                        if this.git.apply_branches(&ticket, branches)
                            && this.git.mode == GitWorkbenchMode::History
                            && this.ensure_history_ref_filter()
                        {
                            this.load_history(false, cx);
                        }
                    }
                    Ok(Err(error)) => {
                        this.git.fail_query(&ticket, &error.code);
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn open_file(
        &mut self,
        path: String,
        temporary: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = normalized_relative_path(&path) else {
            return;
        };
        if self
            .pending_file_search_reveal
            .as_ref()
            .is_some_and(|reveal| reveal.path != path)
        {
            self.pending_file_search_reveal = None;
        }
        self.selected_file_path = Some(path.clone());
        self.file_tree.clear_selected_directory();
        self.file_tree.select(&path, false, false);
        let tab_id = if temporary {
            self.preview
                .preview_file(path.clone(), None, unix_timestamp_ms())
        } else {
            self.preview.open(
                PreviewTarget::File { path: path.clone() },
                None,
                unix_timestamp_ms(),
            )
        };
        let Some(tab_id) = tab_id else {
            return;
        };
        self.activate_tab(&tab_id, cx);
        if self.presentations.contains_key(&path) || self.editor_bindings.contains_key(&path) {
            self.persist(cx);
            cx.notify();
            return;
        }
        match content_preview_kind_for_path(&path) {
            ContentPreviewKind::Pdf => self.open_pdf(path.clone(), window, cx),
            ContentPreviewKind::Office => self.open_office(path.clone(), cx),
            ContentPreviewKind::Image => self.load_image(path.clone(), window, cx),
            ContentPreviewKind::MediaExternalOnly => {
                self.presentations
                    .insert(path.clone(), FilePresentation::MediaExternalOnly);
            }
            ContentPreviewKind::UnsupportedBinary
            | ContentPreviewKind::Markdown
            | ContentPreviewKind::TextEditor => self.load_file(path.clone(), window, cx),
        }
        self.persist(cx);
        cx.notify();
    }

    fn open_file_search_result(
        &mut self,
        mut reveal: FileSearchReveal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = normalized_relative_path(&reveal.path) else {
            return;
        };
        reveal.path = path.clone();
        self.pending_file_search_reveal = Some(reveal);
        if content_preview_kind_for_path(&path) == ContentPreviewKind::Markdown {
            self.markdown_edit_paths.insert(path.clone());
        }
        self.request_preview_panel(cx);
        self.open_file(path.clone(), false, window, cx);
        self.schedule_file_search_reveal(path, window, cx);
    }

    fn schedule_file_search_reveal(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |this, window, cx| {
            if this
                .pending_file_search_reveal
                .as_ref()
                .is_some_and(|reveal| reveal.path == path)
            {
                this.apply_pending_file_search_reveal(window, cx);
            }
        });
    }

    fn apply_pending_file_search_reveal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(reveal) = self.pending_file_search_reveal.clone() else {
            return false;
        };
        let active_path = self
            .preview
            .active_tab_id(&self.preview.focused_pane_id)
            .and_then(|tab_id| self.preview.tabs.get(tab_id))
            .and_then(|tab| match &tab.target {
                PreviewTarget::File { path } => Some(path.as_str()),
                _ => None,
            });
        if active_path != Some(reveal.path.as_str()) {
            return false;
        }
        let Some(input) = self
            .editor_bindings
            .get(&reveal.path)
            .map(|binding| binding.input.clone())
        else {
            return false;
        };
        let content = input.read(cx).value().to_string();
        let match_range = reveal
            .match_start
            .zip(reveal.match_end)
            .map(|(start, end)| start as usize..end as usize);
        let Some(range) =
            file_search_reveal_range(&content, &reveal.query, reveal.line, match_range)
        else {
            self.pending_file_search_reveal = None;
            return false;
        };
        input.update(cx, |input, cx| {
            input.set_selected_range(range.clone(), cx);
            input.focus(window, cx);
        });
        let target_line = reveal.line;
        cx.on_next_frame(window, move |_, _, cx| {
            input.update(cx, |input, cx| center_input_line(input, target_line, cx));
        });
        self.pending_file_search_reveal = None;
        cx.notify();
        true
    }

    pub(crate) fn reveal_file_search_directory(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(path) = normalized_relative_path(&path) else {
            return;
        };
        self.pending_file_search_directory_reveal = Some(path);
        self.continue_file_search_directory_reveal(cx);
    }

    fn continue_file_search_directory_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.pending_file_search_directory_reveal.clone() else {
            return;
        };
        let path_chain = relative_directory_path_chain(&path);
        let loaded_chain = path_chain
            .iter()
            .take_while(|candidate| self.file_tree.contains_path(candidate))
            .cloned()
            .collect::<Vec<_>>();
        if self.file_tree.contains_path(&path) {
            self.file_tree.set_chain_expanded(&path_chain, true);
            self.file_tree.clear_selected_directory();
            self.file_tree.select(&path, false, false);
            if let Some(index) = self.file_tree.visible_row_position(&path) {
                self.file_scroll
                    .scroll_to_item(index, gpui::ScrollStrategy::Center);
            }
            if !matches!(
                self.file_tree.load_state(&path),
                FileTreeLoadState::Loaded | FileTreeLoadState::Loading
            ) {
                self.load_tree_path(path.clone(), cx);
            }
            self.pending_file_search_directory_reveal = None;
            self.persist(cx);
            cx.notify();
            return;
        }
        if !loaded_chain.is_empty() {
            self.file_tree.set_chain_expanded(&loaded_chain, true);
        }
        let base = loaded_chain.last().cloned().unwrap_or_default();
        match self.file_tree.load_state(&base) {
            FileTreeLoadState::Loaded => {
                self.pending_file_search_directory_reveal = None;
            }
            FileTreeLoadState::Loading => {}
            FileTreeLoadState::Unloaded | FileTreeLoadState::Error { .. } => {
                self.load_tree_path(base, cx);
            }
        }
        cx.notify();
    }

    pub fn register_terminal(&mut self, terminal: TerminalSession, cx: &mut Context<Self>) {
        self.terminals.retain(|item| item.id != terminal.id);
        self.terminals.push(terminal);
        cx.notify();
    }

    pub fn sync_terminals(
        &mut self,
        terminals: Vec<TerminalSession>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.terminals == terminals {
            return false;
        }
        let previous_selection = self.selected_terminal_id.clone();
        self.terminals = terminals;
        self.reconcile_terminal_selection();
        self.terminal_surfaces.retain(|terminal_id, _| {
            self.terminals
                .iter()
                .any(|terminal| terminal.id.as_str() == terminal_id)
        });
        self.sync_terminal_surface_activity(cx);
        if self.selected_terminal_id != previous_selection {
            self.persist(cx);
        }
        cx.notify();
        true
    }

    fn ensure_terminal_surface(
        &mut self,
        terminal_id: &TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.terminal_surfaces.contains_key(terminal_id.as_str()) {
            return true;
        }
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return false;
        };
        let session = self
            .terminals
            .iter()
            .find(|terminal| &terminal.id == terminal_id)
            .cloned()
            .or_else(|| {
                runtime
                    .list_terminals(&workspace.id)
                    .ok()?
                    .into_iter()
                    .find(|terminal| &terminal.id == terminal_id)
            });
        let Some(session) = session else {
            self.error = Some("Terminal session is no longer available".into());
            return false;
        };
        self.terminal_surfaces.insert(
            terminal_id.as_str().to_string(),
            cx.new(|cx| {
                TerminalSurface::from_preview_shared_session(
                    runtime.terminals().manager(),
                    workspace.root,
                    session,
                    window,
                    cx,
                )
            }),
        );
        self.sync_terminal_surface_activity(cx);
        true
    }

    pub fn open_terminal(
        &mut self,
        terminal_id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_terminal_in_pane(terminal_id, None, window, cx);
    }

    pub fn open_terminal_in_pane(
        &mut self,
        terminal_id: TerminalId,
        pane_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_terminal_surface(&terminal_id, window, cx) {
            cx.notify();
            return;
        }
        let id = self.preview.open(
            PreviewTarget::Terminal {
                terminal_id: terminal_id.as_str().to_string(),
            },
            pane_id.as_deref(),
            unix_timestamp_ms(),
        );
        if let Some(id) = id {
            self.selected_terminal_id = Some(terminal_id.as_str().to_string());
            self.activate_tab(&id, cx);
            self.persist(cx);
            self.request_preview_panel(cx);
            cx.notify();
        }
    }

    fn open_pdf(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.resolve_workspace_path(&path);
        match result {
            Ok(absolute) => {
                let surface = cx.new(|cx| {
                    PdfSurface::new(
                        vibex_content::PdfiumEngine::discover_library_path(),
                        Some(absolute),
                        None,
                        window,
                        cx,
                    )
                });
                self.presentations
                    .insert(path, FilePresentation::Pdf(surface));
            }
            Err(error) => {
                self.presentations
                    .insert(path, FilePresentation::error(error));
            }
        }
    }

    fn open_office(&mut self, path: String, cx: &mut Context<Self>) {
        let result = self.resolve_workspace_path(&path);
        match result {
            Ok(absolute) => {
                let surface = cx.new(|cx| OfficeSurface::new(Some(absolute), cx));
                self.presentations
                    .insert(path, FilePresentation::Office(surface));
            }
            Err(error) => {
                self.presentations
                    .insert(path, FilePresentation::error(error));
            }
        }
    }

    fn resolve_workspace_path(&self, path: &str) -> VibexResult<PathBuf> {
        let (Some(runtime), Some(workspace)) = (&self.runtime, &self.workspace) else {
            return Err(VibexError::validation(
                "workspace_not_selected",
                "select a workspace before opening a file",
            ));
        };
        runtime.files().resolve_existing_path(&workspace.id, path)
    }

    fn ensure_editor_binding(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(binding) = self.editor_bindings.get(path) {
            return binding.input.clone();
        }
        self.next_editor_binding_id = self.next_editor_binding_id.saturating_add(1).max(1);
        let binding_id = self.next_editor_binding_id;
        let language = language_for_path(path);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .folding(true)
                .replaceable(true)
                .placeholder(locale::text("Loading file", "正在加载文件", "正在載入檔案"))
        });
        let subscription = cx.subscribe_in(&input, window, move |this, _, event, _, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let binding = this
                .editor_bindings
                .iter()
                .find(|(_, binding)| binding.id == binding_id)
                .map(|(path, binding)| (path.clone(), binding.input.clone()));
            let Some((path, input)) = binding else {
                return;
            };
            let value = input.read(cx).value().to_string();
            if this
                .editors
                .buffers
                .get_mut(&path)
                .is_some_and(|buffer| buffer.update_content(value))
            {
                this.persist_editor_recovery(cx);
                cx.notify();
            }
        });
        self.editor_subscriptions.push(subscription);
        self.editor_bindings.insert(
            path.to_string(),
            EditorBinding {
                id: binding_id,
                input: input.clone(),
            },
        );
        input
    }

    fn open_web_external(&mut self, url: String, cx: &mut Context<Self>) {
        let outcome = validate_external_open_url(&url)
            .and_then(|validated| open_external_url(&validated.url));
        match outcome {
            Ok(()) => self.note = Some("Opened link in the system browser".into()),
            Err(error) => self.error = Some(format!("{}: {}", error.code, error.message)),
        }
        cx.notify();
    }

    fn open_markdown_resource(
        &mut self,
        resource: ResolvedResource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = resource.resolved else {
            return;
        };
        match resource.kind {
            ResourceKind::Workspace => self.open_file(target, false, window, cx),
            ResourceKind::Http => self.open_web_external(target, cx),
            ResourceKind::DataImage | ResourceKind::Fragment | ResourceKind::Blocked => {}
        }
    }

    fn start_markdown_parse(&mut self, path: String, source: String, cx: &mut Context<Self>) {
        let Some(workspace_generation) = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.generation)
        else {
            return;
        };
        if !matches!(
            self.presentations.get(&path),
            Some(FilePresentation::Markdown { .. })
        ) {
            self.presentations
                .insert(path.clone(), FilePresentation::Loading);
        }
        let parse_path = path.clone();
        let parse = cx.background_spawn(async move { parse_file_markdown(&source, &parse_path) });
        let task_path = path.clone();
        let task_key = format!("markdown-parse:{path}");
        let completion_key = task_key.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let document = parse.await;
            let _ =
                entity.update(cx, |this, cx| {
                    this.file_tasks.remove(&completion_key);
                    if this
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.generation)
                        != Some(workspace_generation)
                        || this.editors.buffers.get(&task_path).is_none_or(|buffer| {
                            buffer.content.as_str() != document.source.as_ref()
                        })
                    {
                        return;
                    }
                    let images = match this.presentations.get(&task_path) {
                        Some(FilePresentation::Markdown { images, .. }) => images.clone(),
                        _ => Arc::default(),
                    };
                    this.presentations.insert(
                        task_path.clone(),
                        FilePresentation::Markdown { document, images },
                    );
                    this.load_markdown_assets(task_path.clone(), cx);
                    cx.notify();
                });
        });
        self.file_tasks.insert(task_key, task);
    }

    fn load_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        self.presentations
            .insert(path.clone(), FilePresentation::Loading);
        let request = FileReadRequest {
            workspace_id: workspace.id.clone(),
            path: path.clone(),
            max_bytes: Some(FILE_PREVIEW_MAX_BYTES),
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move { runtime.files().read(&request) });
        let task_path = path.clone();
        let task = cx.spawn_in(window, async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update_in(cx, |this, window, cx| {
                this.file_tasks.remove(&task_path);
                if this.workspace.as_ref().map(|current| current.generation)
                    != Some(workspace.generation)
                {
                    return;
                }
                match outcome {
                    Ok(Ok(file)) => {
                        if file.path != task_path {
                            return;
                        }
                        let preview_kind = content_preview_kind(&file);
                        match preview_kind {
                            ContentPreviewKind::TextEditor | ContentPreviewKind::Markdown => {
                                let markdown = preview_kind == ContentPreviewKind::Markdown;
                                let content = file.content.clone().unwrap_or_default();
                                this.editors.insert_read(file);
                                let input = this.ensure_editor_binding(&task_path, window, cx);
                                input.update(cx, |input, cx| {
                                    input.set_highlighter(language_for_path(&task_path), cx);
                                    input.set_value(content.clone(), window, cx);
                                });
                                if markdown {
                                    this.start_markdown_parse(task_path.clone(), content, cx);
                                } else {
                                    this.presentations.remove(&task_path);
                                }
                            }
                            ContentPreviewKind::Image => {
                                this.load_image(task_path.clone(), window, cx);
                            }
                            ContentPreviewKind::Pdf => {
                                this.open_pdf(task_path.clone(), window, cx);
                            }
                            ContentPreviewKind::Office => {
                                this.open_office(task_path.clone(), cx);
                            }
                            ContentPreviewKind::MediaExternalOnly => {
                                this.presentations
                                    .insert(task_path.clone(), FilePresentation::MediaExternalOnly);
                            }
                            ContentPreviewKind::UnsupportedBinary => {
                                this.presentations.insert(
                                    task_path.clone(),
                                    FilePresentation::Unsupported(
                                        "Unsupported binary file".to_string(),
                                    ),
                                );
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        this.presentations
                            .insert(task_path.clone(), FilePresentation::error(error));
                    }
                    Err(error) => {
                        this.presentations.insert(
                            task_path.clone(),
                            FilePresentation::Error {
                                code: "file_load_task_failed".to_string(),
                                message: error.to_string(),
                            },
                        );
                    }
                }
                if this.editor_bindings.contains_key(&task_path)
                    && this
                        .pending_file_search_reveal
                        .as_ref()
                        .is_some_and(|reveal| reveal.path == task_path)
                {
                    this.schedule_file_search_reveal(task_path.clone(), window, cx);
                }
                this.persist(cx);
                cx.notify();
            });
        });
        self.file_tasks.insert(path, task);
    }

    fn load_markdown_assets(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let assets = match self.presentations.get(&path) {
            Some(FilePresentation::Markdown { document, .. }) => document
                .resources
                .iter()
                .filter(|asset| {
                    asset.kind == ResourceKind::Workspace && asset.role == ResourceRole::Image
                })
                .filter_map(|asset| Some((asset.source.clone(), asset.resolved.as_ref()?.clone())))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            _ => return,
        };
        if assets.is_empty() {
            return;
        }
        let workspace_id = workspace.id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let files = runtime.files();
            let mut resolved = Vec::new();
            let mut total_bytes = 0_usize;
            for (source, asset_path) in assets {
                if resolved.len() >= MARKDOWN_LOCAL_IMAGE_LIMIT {
                    break;
                }
                let Ok(bytes) =
                    files.read_bytes(&workspace_id, &asset_path, IMAGE_SOURCE_MAX_BYTES)
                else {
                    continue;
                };
                if total_bytes.saturating_add(bytes.len()) > MARKDOWN_LOCAL_IMAGE_TOTAL_BYTES {
                    continue;
                }
                let Some(mime) = image_mime_for_path(&asset_path) else {
                    continue;
                };
                total_bytes = total_bytes.saturating_add(bytes.len());
                resolved.push((source, mime.to_string(), bytes));
            }
            Ok::<_, VibexError>(resolved)
        });
        let task_path = path.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if this.workspace.as_ref().map(|current| current.generation)
                    != Some(workspace.generation)
                {
                    return;
                }
                if let Ok(Ok(assets)) = outcome
                    && let Some(FilePresentation::Markdown { images, .. }) =
                        this.presentations.get_mut(&task_path)
                {
                    let images = Arc::make_mut(images);
                    for (source, mime, bytes) in assets {
                        let Some(format) = ImageFormat::from_mime_type(&mime) else {
                            continue;
                        };
                        images.insert(source, Arc::new(Image::from_bytes(format, bytes)));
                    }
                }
                cx.notify();
            });
        });
        self.file_tasks.insert(format!("markdown:{path}"), task);
    }

    fn load_image(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        self.presentations
            .insert(path.clone(), FilePresentation::Loading);
        let workspace_id = workspace.id.clone();
        let request = FileReadRequest {
            workspace_id: workspace_id.clone(),
            path: path.clone(),
            max_bytes: Some(1),
        };
        let byte_path = path.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let files = runtime.files();
            let metadata = files.read(&request)?;
            let bytes = files.read_bytes(&workspace_id, &byte_path, IMAGE_SOURCE_MAX_BYTES)?;
            Ok::<_, VibexError>((metadata, bytes))
        });
        let task_path = path.clone();
        let task = cx.spawn_in(window, async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update_in(cx, |this, window, cx| {
                this.file_tasks.remove(&task_path);
                if this.workspace.as_ref().map(|current| current.generation)
                    != Some(workspace.generation)
                {
                    return;
                }
                match outcome {
                    Ok(Ok((metadata, bytes))) => {
                        let Some(format) = image_format_for_path(&task_path) else {
                            this.presentations.insert(
                                task_path.clone(),
                                FilePresentation::Unsupported("Unsupported image format".into()),
                            );
                            return;
                        };
                        let image = Arc::new(Image::from_bytes(format, bytes));
                        let Some(rendered) = image.clone().get_render_image(window, cx) else {
                            this.presentations.insert(
                                task_path.clone(),
                                FilePresentation::Error {
                                    code: "image_decode_failed".into(),
                                    message: "The image could not be decoded".into(),
                                },
                            );
                            return;
                        };
                        let size = rendered.size(0);
                        let width = u32::try_from(size.width.0).unwrap_or(u32::MAX);
                        let height = u32::try_from(size.height.0).unwrap_or(u32::MAX);
                        let decoded_bytes = rendered.as_bytes(0).map_or(0, |bytes| bytes.len());
                        let cache_key = ImageCacheKey {
                            path: task_path.clone(),
                            revision: metadata.content_revision,
                        };
                        match this.image_cache.insert(
                            cache_key.clone(),
                            width,
                            height,
                            decoded_bytes,
                        ) {
                            Ok(evicted) => {
                                let evicted_images = this
                                    .presentations
                                    .values()
                                    .filter_map(|presentation| match presentation {
                                        FilePresentation::Image { image, cache_key }
                                            if evicted.contains(cache_key) =>
                                        {
                                            Some(image.clone())
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>();
                                for evicted_image in evicted_images {
                                    evicted_image.remove_asset(cx);
                                }
                                this.presentations.retain(|_, presentation| {
                                    !matches!(presentation, FilePresentation::Image { cache_key, .. } if evicted.contains(cache_key))
                                });
                                this.presentations.insert(
                                    task_path.clone(),
                                    FilePresentation::Image { image, cache_key },
                                );
                            }
                            Err(_) => {
                                image.remove_asset(cx);
                                this.presentations.insert(
                                    task_path.clone(),
                                    FilePresentation::Error {
                                        code: "image_budget_exceeded".into(),
                                        message: "Image exceeds the native preview budget".into(),
                                    },
                                );
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        this.presentations
                            .insert(task_path.clone(), FilePresentation::error(error));
                    }
                    Err(error) => {
                        this.presentations.insert(
                            task_path.clone(),
                            FilePresentation::Error {
                                code: "image_load_task_failed".into(),
                                message: error.to_string(),
                            },
                        );
                    }
                }
                cx.notify();
            });
        });
        self.file_tasks.insert(path, task);
    }

    fn activate_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        self.activation_generation = self.activation_generation.saturating_add(1).max(1);
        let generation = self.activation_generation;
        for lifecycle in self.lifecycles.values_mut() {
            let current = lifecycle.activation_generation();
            if current > 0 {
                let _ = lifecycle.deactivate(current);
            }
        }
        let Some(tab) = self.preview.tabs.get(tab_id) else {
            return;
        };
        let kind = surface_kind(&tab.target);
        let lifecycle = self
            .lifecycles
            .entry(tab_id.to_string())
            .or_insert_with(|| {
                ContentSurfaceLifecycle::restored(kind, ContentSurfaceOrigin::Preview)
            });
        if lifecycle.activate(generation).is_ok() {
            let _ = lifecycle.begin_load(generation);
            let _ = lifecycle.finish_load(generation);
            let _ = lifecycle.focus_entered(generation);
        }
        self.sync_terminal_surface_activity(cx);
    }

    fn update_lifecycle_bounds(
        &mut self,
        tab_id: &str,
        bounds: gpui::Bounds<gpui::Pixels>,
        scale_factor: f32,
    ) {
        let Some(lifecycle) = self.lifecycles.get_mut(tab_id) else {
            return;
        };
        let generation = lifecycle.activation_generation();
        if generation == 0 || !lifecycle.visible() {
            return;
        }
        let logical = LogicalSurfaceBounds::new(
            f32::from(bounds.origin.x).round() as i32,
            f32::from(bounds.origin.y).round() as i32,
            u32::from(bounds.size.width).max(1),
            u32::from(bounds.size.height).max(1),
            scale_factor,
        );
        if let Ok(bounds) = logical
            && lifecycle.bounds() != Some(bounds)
        {
            let _ = lifecycle.set_bounds(generation, bounds);
        }
    }

    fn close_lifecycle(&mut self, tab_id: &str) {
        let Some(mut lifecycle) = self.lifecycles.remove(tab_id) else {
            return;
        };
        let generation = lifecycle.activation_generation();
        if generation > 0 {
            let _ = lifecycle.close(generation);
        }
    }

    fn close_all_lifecycles(&mut self) {
        let tab_ids = self.lifecycles.keys().cloned().collect::<Vec<_>>();
        for tab_id in tab_ids {
            self.close_lifecycle(&tab_id);
        }
    }

    fn cleanup_closed_tab(&mut self, tab_id: &str, force: bool) {
        self.close_lifecycle(tab_id);
        self.preview_diff_lists.remove(tab_id);
        self.preview_commit_lists.remove(tab_id);
        self.preview_commit_focus_requests.remove(tab_id);
        self.git_preview_errors.remove(tab_id);
        if let Some(terminal_id) = tab_id.strip_prefix("terminal:")
            && let Ok(terminal_id) = TerminalId::parse(terminal_id)
        {
            if let Some(runtime) = self.runtime.as_ref()
                && let Err(error) = runtime.kill_terminal(&terminal_id)
                && error.code != "terminal_not_found"
            {
                self.error = Some(format!(
                    "Terminal close failed: {}: {}",
                    error.code, error.message
                ));
            }
            self.terminals.retain(|terminal| terminal.id != terminal_id);
            self.terminal_surfaces.remove(terminal_id.as_str());
            if self.selected_terminal_id.as_deref() == Some(terminal_id.as_str()) {
                self.selected_terminal_id = self
                    .terminals
                    .first()
                    .map(|terminal| terminal.id.as_str().to_string());
            }
        }
        if let Some(path) = tab_id.strip_prefix("file:") {
            self.editors.close(path, force);
            if force || !self.editors.buffers.contains_key(path) {
                self.editor_bindings.remove(path);
                self.presentations.remove(path);
                self.markdown_edit_paths.remove(path);
                self.file_tasks.remove(path);
                self.file_tasks.remove(&format!("markdown:{path}"));
                self.file_tasks.remove(&format!("markdown-parse:{path}"));
                self.file_tasks.remove(&format!("save:{path}"));
            }
        }
    }

    fn cleanup_closed_tabs(&mut self, outcomes: &[(String, PreviewCloseDisposition)], force: bool) {
        for (tab_id, disposition) in outcomes {
            if *disposition == PreviewCloseDisposition::Closed {
                self.cleanup_closed_tab(tab_id, force);
            }
        }
    }

    pub(crate) fn focus_tab(&mut self, tab_id: String, cx: &mut Context<Self>) {
        if self.preview.focus(&tab_id) {
            if let Some(PreviewTarget::Terminal { terminal_id }) =
                self.preview.tabs.get(&tab_id).map(|tab| &tab.target)
            {
                self.selected_terminal_id = Some(terminal_id.clone());
            }
            self.activate_tab(&tab_id, cx);
            self.persist(cx);
            cx.notify();
        }
    }

    pub(crate) fn toggle_markdown_source(&mut self, path: String, cx: &mut Context<Self>) {
        if self.markdown_edit_paths.remove(&path) {
            if let Some(source) = self
                .editors
                .buffers
                .get(&path)
                .map(|buffer| buffer.content.clone())
            {
                self.start_markdown_parse(path, source, cx);
            }
        } else {
            self.markdown_edit_paths.insert(path);
        }
        cx.notify();
    }

    pub(crate) fn save_editor(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let Some(ticket) = self.editors.begin_save(&path) else {
            self.error = Some("The editor is not ready to save or has an external conflict".into());
            cx.notify();
            return;
        };
        let request_id = ticket.request_id;
        let request = ticket.into_request(workspace.id);
        let task_path = path.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move { runtime.files().write(&request) });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.file_tasks.remove(&format!("save:{task_path}"));
                let Some(buffer) = this.editors.buffers.get_mut(&task_path) else {
                    return;
                };
                match outcome {
                    Ok(Ok(file)) => {
                        buffer.finish_save(request_id, file);
                        this.note = Some(format!("Saved {task_path}"));
                    }
                    Ok(Err(error)) => {
                        buffer.fail_save(request_id, &error.code);
                        if error.code == "file_external_revision_changed" {
                            buffer.external = EditorExternalState::VerificationRequired;
                        }
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        buffer.fail_save(request_id, "file_save_task_failed");
                        this.error = Some(format!("file save task failed: {error}"));
                    }
                }
                this.persist(cx);
                this.persist_editor_recovery(cx);
                cx.notify();
            });
        });
        self.file_tasks.insert(format!("save:{path}"), task);
        cx.notify();
    }

    pub(crate) fn save_active_editor(&mut self, cx: &mut Context<Self>) {
        let tab_id = self.preview.active_tab_id(&self.preview.focused_pane_id);
        let Some(path) = tab_id
            .and_then(|tab_id| self.preview.tabs.get(tab_id))
            .and_then(|tab| match &tab.target {
                PreviewTarget::File { path } => Some(path.clone()),
                _ => None,
            })
        else {
            return;
        };
        if self
            .editors
            .buffers
            .get(&path)
            .is_some_and(|buffer| buffer.dirty)
        {
            self.save_editor(path, cx);
        }
    }

    fn protected_tab_ids(&self) -> BTreeSet<String> {
        self.editors
            .dirty_paths()
            .map(|path| format!("file:{path}"))
            .collect()
    }

    pub(crate) fn close_tab(&mut self, tab_id: String, force: bool, cx: &mut Context<Self>) {
        let disposition = self
            .preview
            .close_guarded(&tab_id, force, &self.protected_tab_ids());
        match disposition {
            PreviewCloseDisposition::Closed => {
                self.cleanup_closed_tab(&tab_id, force);
                self.sync_terminal_surface_activity(cx);
                self.persist(cx);
                self.persist_editor_recovery(cx);
                self.close_preview_panel_if_empty(cx);
            }
            PreviewCloseDisposition::Pinned => {
                self.error = Some("Unpin the tab before closing it".into())
            }
            PreviewCloseDisposition::Protected => {
                self.error = Some("Save or discard the dirty editor before closing it".into())
            }
            PreviewCloseDisposition::Missing => {}
        }
        cx.notify();
    }

    pub(crate) fn toggle_pin(&mut self, tab_id: String, cx: &mut Context<Self>) {
        if self.preview.toggle_pin(&tab_id) {
            self.persist(cx);
            cx.notify();
        }
    }

    pub(crate) fn close_other_tabs(
        &mut self,
        tab_id: String,
        pane_id: String,
        cx: &mut Context<Self>,
    ) {
        let outcomes = self
            .preview
            .close_other_tabs(&tab_id, &pane_id, &self.protected_tab_ids());
        self.cleanup_closed_tabs(&outcomes, false);
        if outcomes.iter().any(|(_, disposition)| {
            matches!(
                disposition,
                PreviewCloseDisposition::Pinned | PreviewCloseDisposition::Protected
            )
        }) {
            self.error = Some("Pinned or dirty tabs were kept open".into());
        }
        self.sync_terminal_surface_activity(cx);
        self.persist(cx);
        self.persist_editor_recovery(cx);
        self.close_preview_panel_if_empty(cx);
        cx.notify();
    }

    pub(crate) fn close_all_tabs(&mut self, pane_id: String, cx: &mut Context<Self>) {
        let outcomes = self
            .preview
            .close_all_tabs(&pane_id, &self.protected_tab_ids());
        self.cleanup_closed_tabs(&outcomes, false);
        if outcomes.iter().any(|(_, disposition)| {
            matches!(
                disposition,
                PreviewCloseDisposition::Pinned | PreviewCloseDisposition::Protected
            )
        }) {
            self.error = Some("Pinned or dirty tabs were kept open".into());
        }
        self.sync_terminal_surface_activity(cx);
        self.persist(cx);
        self.persist_editor_recovery(cx);
        self.close_preview_panel_if_empty(cx);
        cx.notify();
    }

    pub(crate) fn split_tab(
        &mut self,
        tab_id: String,
        pane_id: String,
        position: PreviewSplitPosition,
        cx: &mut Context<Self>,
    ) {
        self.split_tab_with_pruning(tab_id, pane_id, position, false, cx);
    }

    fn split_tab_with_pruning(
        &mut self,
        tab_id: String,
        pane_id: String,
        position: PreviewSplitPosition,
        prune_empty_panes: bool,
        cx: &mut Context<Self>,
    ) {
        let suffix = RequestId::new().as_str().to_string();
        let new_pane_id = format!("preview-pane-{suffix}");
        let new_split_id = format!("preview-split-{suffix}");
        let split = if prune_empty_panes {
            self.preview
                .split_pruned(&tab_id, &pane_id, position, &new_pane_id, &new_split_id)
        } else {
            self.preview
                .split(&tab_id, &pane_id, position, &new_pane_id, &new_split_id)
        };
        if split {
            self.sync_terminal_surface_activity(cx);
            self.persist(cx);
            cx.notify();
        }
    }

    pub(crate) fn toggle_fullscreen(&mut self, cx: &mut Context<Self>) {
        self.set_preview_panel_fullscreen(!self.preview_panel_fullscreen, cx);
        self.preview.set_fullscreen(None);
        self.persist(cx);
        cx.notify();
    }

    pub(crate) fn exit_fullscreen(&mut self, cx: &mut Context<Self>) {
        if self.preview_panel_fullscreen || self.preview.fullscreen_tab_id.is_some() {
            self.set_preview_panel_fullscreen(false, cx);
            self.preview.set_fullscreen(None);
            self.persist(cx);
            cx.notify();
        }
    }

    pub(crate) fn close_panel_tabs(&mut self, cx: &mut Context<Self>) {
        let tab_ids = self.preview.tabs.keys().cloned().collect::<Vec<_>>();
        for tab_id in tab_ids {
            let disposition = self.preview.close_guarded(&tab_id, true, &BTreeSet::new());
            if disposition == PreviewCloseDisposition::Closed {
                self.cleanup_closed_tab(&tab_id, true);
            }
        }
        self.set_preview_panel_fullscreen(false, cx);
        self.preview.set_fullscreen(None);
        self.preview.set_side_preview(None);
        self.sync_terminal_surface_activity(cx);
        self.persist(cx);
        self.persist_editor_recovery(cx);
        cx.notify();
    }

    fn reveal_file_in_right_rail(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(path) = normalized_relative_path(&path) else {
            return;
        };
        self.selected_file_path = Some(path.clone());
        self.file_tree.select(&path, false, false);
        self.persist(cx);
        if let Some(parent) = self.parent.clone() {
            cx.defer(move |cx| {
                let _ = parent.update(cx, |parent, cx| parent.reveal_file_in_right_rail(cx));
            });
        }
        cx.notify();
    }

    fn remap_editor_bindings(&mut self, source: &str, destination: &str) {
        let mut next = BTreeMap::new();
        for (path, binding) in std::mem::take(&mut self.editor_bindings) {
            next.insert(replace_path_prefix(&path, source, destination), binding);
        }
        self.editor_bindings = next;
        let mut presentations = BTreeMap::new();
        for (path, presentation) in std::mem::take(&mut self.presentations) {
            presentations.insert(
                replace_path_prefix(&path, source, destination),
                presentation,
            );
        }
        self.presentations = presentations;
        self.markdown_edit_paths = std::mem::take(&mut self.markdown_edit_paths)
            .into_iter()
            .map(|path| replace_path_prefix(&path, source, destination))
            .collect();
        self.markdown_scrolls = std::mem::take(&mut self.markdown_scrolls)
            .into_iter()
            .map(|(path, scroll)| (replace_path_prefix(&path, source, destination), scroll))
            .collect();
        self.lifecycles = std::mem::take(&mut self.lifecycles)
            .into_iter()
            .map(|(tab_id, lifecycle)| {
                (
                    remap_preview_tab_id(&tab_id, source, destination),
                    lifecycle,
                )
            })
            .collect();
    }

    pub(crate) fn create_file(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let request = FileWriteRequest {
            workspace_id: workspace.id,
            path,
            content: String::new(),
            create_if_missing: true,
            expected_revision: None,
            encoding: None,
            line_ending: None,
        };
        self.start_file_mutation(
            FileMutationKind::CreateFile,
            request.path.clone(),
            None,
            move || runtime.files().write(&request).map(|_| ()),
            cx,
        );
    }

    pub(crate) fn create_directory(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let request = FileMutationRequest {
            workspace_id: workspace.id,
            path,
            new_path: None,
            recursive: true,
            overwrite: false,
        };
        self.start_file_mutation(
            FileMutationKind::CreateDirectory,
            request.path.clone(),
            None,
            move || runtime.files().create_directory(&request).map(|_| ()),
            cx,
        );
    }

    pub(crate) fn copy_path(
        &mut self,
        source: String,
        destination: String,
        recursive: bool,
        cx: &mut Context<Self>,
    ) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let request = FileMutationRequest {
            workspace_id: workspace.id,
            path: source.clone(),
            new_path: Some(destination.clone()),
            recursive,
            overwrite: false,
        };
        self.start_file_mutation(
            FileMutationKind::Copy,
            source,
            Some(destination),
            move || runtime.files().copy(&request).map(|_| ()),
            cx,
        );
    }

    pub(crate) fn rename_path(
        &mut self,
        source: String,
        destination: String,
        cx: &mut Context<Self>,
    ) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let request = FileMutationRequest {
            workspace_id: workspace.id,
            path: source.clone(),
            new_path: Some(destination.clone()),
            recursive: true,
            overwrite: false,
        };
        let apply_source = source.clone();
        let apply_destination = destination.clone();
        self.start_file_mutation_with_apply(
            FileMutationKind::Rename,
            source,
            Some(destination),
            move || runtime.files().rename(&request).map(|_| ()),
            move |this| {
                this.file_tree.move_path(&apply_source, &apply_destination);
                this.preview.move_path(&apply_source, &apply_destination);
                this.editors.move_path(&apply_source, &apply_destination);
                this.git.move_path(&apply_source, &apply_destination);
                this.remap_editor_bindings(&apply_source, &apply_destination);
            },
            cx,
        );
    }

    pub(crate) fn delete_path(&mut self, path: String, cx: &mut Context<Self>) {
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let request = FileMutationRequest {
            workspace_id: workspace.id,
            path: path.clone(),
            new_path: None,
            recursive: true,
            overwrite: false,
        };
        let apply_path = path.clone();
        self.start_file_mutation_with_apply(
            FileMutationKind::Delete,
            path,
            None,
            move || runtime.files().delete(&request),
            move |this| {
                let protected = this.protected_tab_ids();
                let removed_tab_ids = this
                    .preview
                    .tabs
                    .iter()
                    .filter(|(tab_id, tab)| {
                        !protected.contains(*tab_id)
                            && preview_target_references_path(&tab.target, &apply_path)
                    })
                    .map(|(tab_id, _)| tab_id.clone())
                    .collect::<Vec<_>>();
                this.file_tree.delete_path(&apply_path);
                this.preview.delete_path_guarded(&apply_path, &protected);
                this.editors.delete_path(&apply_path);
                this.git.delete_path(&apply_path);
                for tab_id in removed_tab_ids {
                    this.cleanup_closed_tab(&tab_id, true);
                }
                this.presentations
                    .retain(|path, _| !path_is_equal_or_descendant(path, &apply_path));
                this.editor_bindings
                    .retain(|path, _| !path_is_equal_or_descendant(path, &apply_path));
            },
            cx,
        );
    }

    fn start_file_mutation<F>(
        &mut self,
        kind: FileMutationKind,
        source: String,
        destination: Option<String>,
        operation: F,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce() -> VibexResult<()> + Send + 'static,
    {
        self.start_file_mutation_with_apply(kind, source, destination, operation, |_| {}, cx);
    }

    fn start_file_mutation_with_apply<F, A>(
        &mut self,
        kind: FileMutationKind,
        source: String,
        destination: Option<String>,
        operation: F,
        apply: A,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce() -> VibexResult<()> + Send + 'static,
        A: FnOnce(&mut Self) + 'static,
    {
        if self.file_mutation_pending {
            self.error = Some("Another file mutation is already running".into());
            cx.notify();
            return;
        }
        let operation_id = RequestId::new().as_str().to_string();
        self.file_tree.invalidate_refresh();
        self.file_tree.set_pending(PendingFileMutation {
            operation_id: operation_id.clone(),
            kind,
            source_path: source,
            target_path: destination,
        });
        self.file_mutation_pending = true;
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move { operation() });
        self.mutation_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.file_mutation_pending = false;
                this.file_tree.finish_pending(&operation_id);
                match outcome {
                    Ok(Ok(())) => {
                        apply(this);
                        this.sync_terminal_surface_activity(cx);
                        this.note = Some("File operation completed".into());
                        this.load_tree(cx);
                        this.load_git_status(cx);
                        this.persist(cx);
                        this.persist_editor_recovery(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("file mutation task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn open_default_app(&mut self, path: String, cx: &mut Context<Self>) {
        self.open_external(path, None, false, cx);
    }

    pub(crate) fn open_native_terminal(&mut self, path: String, cx: &mut Context<Self>) {
        self.open_external(path, None, true, cx);
    }

    pub(crate) fn open_with_tool(&mut self, path: String, tool: String, cx: &mut Context<Self>) {
        self.open_external(path, Some(tool), false, cx);
    }

    pub(crate) fn reveal_in_file_manager(&mut self, path: String, cx: &mut Context<Self>) {
        let absolute = match self.resolve_workspace_path(&path) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("{}: {}", error.code, error.message));
                cx.notify();
                return;
            }
        };
        match reveal_path_in_file_manager(&absolute) {
            Ok(()) => self.note = Some(format!("Revealed {path}")),
            Err(error) => self.error = Some(format!("{}: {}", error.code, error.message)),
        }
        cx.notify();
    }

    fn open_external(
        &mut self,
        path: String,
        tool: Option<String>,
        terminal: bool,
        cx: &mut Context<Self>,
    ) {
        let absolute = match self.resolve_workspace_path(&path) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("{}: {}", error.code, error.message));
                cx.notify();
                return;
            }
        };
        let outcome = if terminal {
            open_native_terminal_for_path(&absolute)
        } else if let Some(tool) = tool {
            open_path_with_external_tool(&tool, &absolute)
        } else {
            open_path_with_default_app(&absolute)
        };
        match outcome {
            Ok(()) => self.note = Some(format!("Opened {path}")),
            Err(error) => self.error = Some(format!("{}: {}", error.code, error.message)),
        }
        cx.notify();
    }

    pub(crate) fn open_diff(&mut self, key: GitSelectionKey, cx: &mut Context<Self>) {
        if self.runtime.is_none() || self.workspace.is_none() {
            return;
        }
        let Some(path) = normalized_relative_path(&key.path) else {
            return;
        };
        let key = GitSelectionKey {
            path,
            staged: key.staged,
        };
        self.selected_git_path = Some(key.path.clone());
        let tab_id = self.preview.open(
            PreviewTarget::GitDiff {
                path: key.path.clone(),
                staged: key.staged,
            },
            None,
            unix_timestamp_ms(),
        );
        if let Some(tab_id) = tab_id {
            self.activate_tab(&tab_id, cx);
        }
        self.load_diff(key, cx);
        self.persist(cx);
        cx.notify();
    }

    fn load_diff(&mut self, key: GitSelectionKey, cx: &mut Context<Self>) {
        let Some(path) = normalized_relative_path(&key.path) else {
            return;
        };
        let key = GitSelectionKey {
            path,
            staged: key.staged,
        };
        if self.git.diffs.contains_key(&key) || self.diff_tasks.contains_key(&key) {
            return;
        }
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let tab_id = git_diff_tab_id(&key);
        self.git_preview_errors.remove(&tab_id);
        let query_key = format!("{}:{}", key.staged, key.path);
        let Some(ticket) = self.git.begin_query(GitQueryKind::Diff, query_key) else {
            return;
        };
        let request = GitDiffRequest {
            workspace_id: workspace.id,
            path: key.path.clone(),
            staged: key.staged,
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move { runtime.git().diff(&request) });
        let task_key = key.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.diff_tasks.remove(&task_key);
                match outcome {
                    Ok(Ok(diff)) => {
                        if this.git.apply_diff(&ticket, diff) {
                            this.git_preview_errors.remove(&tab_id);
                        }
                    }
                    Ok(Err(error)) => {
                        this.git.fail_query(&ticket, &error.code);
                        this.git_preview_errors
                            .insert(tab_id.clone(), format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        let message = format!("Git diff task failed: {error}");
                        this.git.fail_query(&ticket, "git_diff_task_failed");
                        this.git_preview_errors.insert(tab_id.clone(), message);
                    }
                }
                this.persist(cx);
                cx.notify();
            });
        });
        self.diff_tasks.insert(key, task);
    }

    pub(crate) fn open_commit(&mut self, hash: String, subject: String, cx: &mut Context<Self>) {
        if self.runtime.is_none() || self.workspace.is_none() {
            return;
        }
        let hash = hash.trim().to_string();
        if hash.is_empty() {
            return;
        }
        self.git.select_commit(hash.clone());
        let tab_id = self.preview.open(
            PreviewTarget::GitCommit {
                commit_hash: hash.clone(),
                subject: Some(subject),
                focus_path: None,
                focus_request_id: None,
            },
            None,
            unix_timestamp_ms(),
        );
        if let Some(tab_id) = tab_id {
            self.activate_tab(&tab_id, cx);
        }
        self.load_commit_detail(hash, cx);
        self.persist(cx);
        cx.notify();
    }

    fn load_commit_detail(&mut self, hash: String, cx: &mut Context<Self>) {
        let hash = hash.trim().to_string();
        if hash.is_empty() {
            return;
        }
        if self.git.commit_patch_ready(&hash) || self.commit_detail_tasks.contains_key(&hash) {
            return;
        }
        let (Some(runtime), Some(workspace)) = (self.runtime.clone(), self.workspace.clone())
        else {
            return;
        };
        let tab_id = git_commit_tab_id(&hash);
        self.git_preview_errors.remove(&tab_id);
        let Some(ticket) = self
            .git
            .begin_query(GitQueryKind::CommitDetail, hash.clone())
        else {
            return;
        };
        let request = GitCommitDetailRequest {
            workspace_id: workspace.id,
            commit_hash: hash.clone(),
            include_patch: true,
        };
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { runtime.git().commit_detail(&request) });
        let task_hash = hash.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.commit_detail_tasks.remove(&task_hash);
                match outcome {
                    Ok(Ok(detail)) => {
                        if this.git.apply_commit_detail(&ticket, detail) {
                            this.git_preview_errors.remove(&tab_id);
                        }
                    }
                    Ok(Err(error)) => {
                        this.git.fail_query(&ticket, &error.code);
                        this.git_preview_errors
                            .insert(tab_id.clone(), format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        let message = format!("Commit detail task failed: {error}");
                        this.git
                            .fail_query(&ticket, "git_commit_detail_task_failed");
                        this.git_preview_errors.insert(tab_id.clone(), message);
                    }
                }
                this.persist(cx);
                cx.notify();
            });
        });
        self.commit_detail_tasks.insert(hash, task);
    }

    pub(crate) fn open_commit_at_path(
        &mut self,
        hash: String,
        subject: String,
        path: String,
        cx: &mut Context<Self>,
    ) {
        if self.runtime.is_none() || self.workspace.is_none() {
            return;
        }
        let hash = hash.trim().to_string();
        if hash.is_empty() {
            return;
        }
        let opened_at_ms = unix_timestamp_ms();
        let focus_request_id = opened_at_ms.max(0) as u64;
        self.git.select_commit(hash.clone());
        let tab_id = self.preview.open(
            PreviewTarget::GitCommit {
                commit_hash: hash.clone(),
                subject: Some(subject),
                focus_path: Some(path),
                focus_request_id: Some(focus_request_id),
            },
            None,
            opened_at_ms,
        );
        if let Some(tab_id) = tab_id {
            self.activate_tab(&tab_id, cx);
        }
        self.load_commit_detail(hash, cx);
        self.persist(cx);
        cx.notify();
    }

    pub(crate) fn revert_selected(&mut self, cx: &mut Context<Self>) {
        let paths = self.git.selected_change_paths();
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        if paths.is_empty() {
            self.error = Some("Select one or more changes first".into());
            cx.notify();
            return;
        }
        let request = GitStageRequest {
            workspace_id: workspace.id,
            paths: paths.clone(),
        };
        self.run_git_mutation(
            mutation_scope(
                RequestId::new().as_str(),
                GitMutationKind::Revert,
                paths,
                None,
            ),
            move |git| git.revert(&request).map(Some),
            cx,
        );
    }

    pub(crate) fn commit(
        &mut self,
        push_after: bool,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let (Some(workspace), Some(_)) = (self.workspace.clone(), self.runtime.clone()) else {
            return;
        };
        let message = normalize_git_commit_message(
            &self.commit_type,
            self.commit_message.read(cx).value().as_ref(),
        );
        if message.is_empty() {
            self.error = Some("Commit message is required.".into());
            cx.notify();
            return;
        }
        let paths = self.git.selected_change_paths();
        if paths.is_empty() {
            self.error = Some("Select one or more changes first".into());
            cx.notify();
            return;
        }
        let amend = self.amend_commit;
        let request = GitCommitRequest {
            workspace_id: workspace.id.clone(),
            message,
            paths,
            amend,
            push_after,
        };
        let kind = if amend {
            GitMutationKind::Amend
        } else {
            GitMutationKind::Commit
        };
        let status_workspace = workspace.id;
        self.commit_reset_window = Some(window);
        self.run_git_mutation(
            mutation_scope(RequestId::new().as_str(), kind, request.paths.clone(), None),
            move |git| {
                git.commit(&request)?;
                git.status(&status_workspace).map(Some)
            },
            cx,
        );
    }

    pub(crate) fn remote_action(&mut self, kind: GitRemoteActionKind, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let request = GitRemoteActionRequest {
            workspace_id: workspace.id.clone(),
            kind,
            remote: None,
            branch: None,
        };
        let mutation_kind = match kind {
            GitRemoteActionKind::Fetch => GitMutationKind::Fetch,
            GitRemoteActionKind::Push => GitMutationKind::Push,
        };
        let status_workspace = workspace.id;
        self.run_git_mutation(
            mutation_scope(RequestId::new().as_str(), mutation_kind, Vec::new(), None),
            move |git| {
                let result = git.remote_action(&request)?;
                if let Some(status) = result.status_after {
                    Ok(Some(status))
                } else {
                    git.status(&status_workspace).map(Some)
                }
            },
            cx,
        );
    }

    fn run_git_mutation<F>(
        &mut self,
        scope: vibex_desktop_model::GitMutationScope,
        operation: F,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(GitHandle) -> VibexResult<Option<GitStatusSummary>> + Send + 'static,
    {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let reset_commit_form =
            matches!(scope.kind, GitMutationKind::Commit | GitMutationKind::Amend);
        self.error = None;
        self.note = None;
        if !self.git.begin_mutation(scope.clone()) {
            if reset_commit_form {
                self.commit_reset_window = None;
            }
            self.error = Some("Another Git mutation is already running".into());
            cx.notify();
            return;
        }
        let operation_id = scope.operation_id;
        let runner = gpui_tokio::Tokio::spawn(cx, async move { operation(runtime.git()) });
        self.mutation_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(status)) => {
                        this.git.finish_mutation(&operation_id, status.clone());
                        if reset_commit_form {
                            this.amend_commit = false;
                            if let Some(window_handle) = this.commit_reset_window.take() {
                                let input = this.commit_message.clone();
                                let _ = cx.update_window(window_handle, |_, window, cx| {
                                    input.update(cx, |input, cx| input.set_value("", window, cx));
                                });
                            }
                        }
                        if let Some(status) = status {
                            this.file_tree.set_git_changes(&status.changes);
                        }
                        this.note = Some("Git operation completed".into());
                        this.load_git_status(cx);
                        this.load_branches(cx);
                        if this.git.mode == GitWorkbenchMode::History {
                            this.load_history(false, cx);
                        }
                    }
                    Ok(Err(error)) => {
                        if reset_commit_form {
                            this.commit_reset_window = None;
                        }
                        this.git.fail_mutation(&operation_id, &error.code);
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        if reset_commit_form {
                            this.commit_reset_window = None;
                        }
                        this.git
                            .fail_mutation(&operation_id, "git_mutation_task_failed");
                        this.error = Some(format!("Git mutation task failed: {error}"));
                    }
                }
                this.persist(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_empty(&self, message: impl Into<SharedString>, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .text_center()
            .child(Icon::new(IconName::Inbox))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.into()),
            )
            .into_any_element()
    }

    fn render_preview_node(
        &mut self,
        node: PreviewSplitNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PreviewSplitNode::Pane { pane } => self.render_preview_pane(pane, window, cx),
            PreviewSplitNode::Split {
                id,
                direction,
                children,
                sizes,
            } => {
                let weak = cx.weak_entity();
                let resize_id = id.clone();
                let mut group = match direction {
                    vibex_desktop_model::SplitDirection::Horizontal => h_resizable(id),
                    vibex_desktop_model::SplitDirection::Vertical => v_resizable(id),
                }
                .on_resize(move |state, _, cx| {
                    let values = state
                        .read(cx)
                        .sizes()
                        .iter()
                        .map(|size| size.as_f32())
                        .collect::<Vec<_>>();
                    let total = values.iter().sum::<f32>();
                    if total > 0.0 {
                        let normalized = values.into_iter().map(|value| value / total).collect();
                        let _ = weak.update(cx, |this, cx| {
                            if this.preview.resize_split(&resize_id, normalized) {
                                this.persist(cx);
                                cx.notify();
                            }
                        });
                    }
                });
                for (index, child) in children.into_iter().enumerate() {
                    let ratio = sizes.get(index).copied().unwrap_or(0.5).clamp(0.05, 0.95);
                    group = group.child(
                        resizable_panel()
                            .size(px(600.0 * ratio))
                            .size_range(px(140.0)..gpui::Pixels::MAX)
                            .child(self.render_preview_node(child, window, cx)),
                    );
                }
                group.into_any_element()
            }
        }
    }

    fn render_preview_pane(
        &mut self,
        pane: PreviewPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = pane.active_tab_id.clone();
        let pane_id = pane.id.clone();
        let tabs = pane
            .tab_ids
            .iter()
            .filter_map(|id| self.preview.tabs.get(id).cloned())
            .collect::<Vec<_>>();
        let tab_scroll = self
            .preview_tab_scrolls
            .entry(pane_id.clone())
            .or_default()
            .clone();
        if let Some(active_tab_id) = active.as_ref() {
            if self.preview_revealed_tab_ids.get(&pane_id) != Some(active_tab_id)
                && let Some(index) = tabs.iter().position(|tab| &tab.id == active_tab_id)
            {
                tab_scroll.scroll_to_item(index);
                self.preview_revealed_tab_ids
                    .insert(pane_id.clone(), active_tab_id.clone());
            }
        } else {
            self.preview_revealed_tab_ids.remove(&pane_id);
        }
        let wheel_scroll = tab_scroll.clone();
        let mut tab_strip = h_flex();
        tab_strip.style().restrict_scroll_to_axis = Some(true);
        let drop_pane_id = pane_id.clone();
        let tab_strip_pane_id = pane_id.clone();
        let drag_pane_id = pane_id.clone();
        let new_terminal_entity = cx.weak_entity();
        let new_terminal_pane_id = pane_id.clone();
        let empty_terminal_entity = cx.weak_entity();
        let empty_terminal_pane_id = pane_id.clone();
        let terminal_available = self.workspace.is_some() && self.runtime.is_some();
        let tab_group_drop_active = cx.has_active_drag()
            && self
                .preview_pane_drop_target
                .as_ref()
                .is_some_and(|target| {
                    target.pane_id == pane_id && target.region == PreviewPaneDropRegion::TabGroup
                });
        let content_drop_region = cx
            .has_active_drag()
            .then_some(self.preview_pane_drop_target.as_ref())
            .flatten()
            .filter(|target| {
                target.pane_id == pane_id && target.region != PreviewPaneDropRegion::TabGroup
            })
            .map(|target| target.region);
        v_flex()
            .id(format!("preview-pane:{pane_id}"))
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(cx.theme().background)
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<PreviewTabDrag>, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let header_bottom = event.bounds.origin.y + px(36.0);
                    if event.event.position.y < header_bottom {
                        return;
                    }
                    let content_top = header_bottom;
                    let content_height = (event.bounds.size.height - px(36.0)).max(px(1.0));
                    let region = if event.event.position.y >= content_top + content_height * 0.5 {
                        PreviewPaneDropRegion::Bottom
                    } else if event.event.position.x
                        >= event.bounds.origin.x + event.bounds.size.width * 0.5
                    {
                        PreviewPaneDropRegion::Right
                    } else {
                        PreviewPaneDropRegion::Content
                    };
                    let next = PreviewPaneDropTarget {
                        pane_id: drag_pane_id.clone(),
                        region,
                    };
                    if this.preview_pane_drop_target != Some(next.clone()) {
                        this.preview_pane_drop_target = Some(next);
                        if region != PreviewPaneDropRegion::TabGroup {
                            this.preview_tab_drop_target = None;
                        }
                        cx.notify();
                    }
                },
            ))
            .on_drop(cx.listener(move |this, drag: &PreviewTabDrag, _, cx| {
                cx.stop_propagation();
                this.preview_tab_drop_target = None;
                let target = this
                    .preview_pane_drop_target
                    .take()
                    .filter(|target| target.pane_id == drop_pane_id);
                let Some(target) = target else {
                    cx.notify();
                    return;
                };
                match target.region {
                    PreviewPaneDropRegion::TabGroup | PreviewPaneDropRegion::Content => {
                        if this.preview.move_to_pane(&drag.tab_id, &drop_pane_id) {
                            this.sync_terminal_surface_activity(cx);
                            this.persist(cx);
                        }
                    }
                    PreviewPaneDropRegion::Right | PreviewPaneDropRegion::Bottom => {
                        let position = if target.region == PreviewPaneDropRegion::Right {
                            PreviewSplitPosition::Right
                        } else {
                            PreviewSplitPosition::Bottom
                        };
                        this.split_tab_with_pruning(
                            drag.tab_id.clone(),
                            drop_pane_id.clone(),
                            position,
                            true,
                            cx,
                        );
                    }
                }
                cx.notify();
            }))
            .child(
                tab_strip
                    .id(format!("preview-tab-strip:{pane_id}"))
                    .h(px(36.0))
                    .flex_none()
                    .min_w_0()
                    .overflow_x_scroll()
                    .overflow_y_hidden()
                    .track_scroll(&tab_scroll)
                    .on_scroll_wheel(cx.listener(
                        move |_, event: &ScrollWheelEvent, window, cx| {
                            let max_x = wheel_scroll.max_offset().x;
                            let delta = event.delta.pixel_delta(window.line_height());
                            if max_x > px(0.0) && delta.y.abs() > delta.x.abs() {
                                let offset = wheel_scroll.offset();
                                // GPUI applies delta.x before custom bubble listeners run.
                                let next_x =
                                    (offset.x - delta.x + delta.y).clamp(-max_x, px(0.0));
                                if next_x != offset.x {
                                    wheel_scroll.set_offset(point(next_x, offset.y));
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .border_b_1()
                    .border_color(if tab_group_drop_active {
                        cx.theme().primary.opacity(0.60)
                    } else {
                        cx.theme().border
                    })
                    .bg(if tab_group_drop_active {
                        cx.theme().primary.opacity(0.10)
                    } else {
                        cx.theme().muted.opacity(0.30)
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<PreviewTabDrag>, _, cx| {
                            if event.bounds.contains(&event.event.position) {
                                let next = PreviewPaneDropTarget {
                                    pane_id: tab_strip_pane_id.clone(),
                                    region: PreviewPaneDropRegion::TabGroup,
                                };
                                if this.preview_pane_drop_target != Some(next.clone())
                                    || this.preview_tab_drop_target.is_some()
                                {
                                    this.preview_pane_drop_target = Some(next);
                                    this.preview_tab_drop_target = None;
                                    cx.notify();
                                }
                            }
                        },
                    ))
                    .children(
                        tabs.into_iter().map(|tab| {
                            self.render_preview_tab(&pane_id, tab, active.as_deref(), cx)
                        }),
                    )
                    .child(
                        Button::new(format!("preview-pane-new:{pane_id}"))
                            .small()
                            .ghost()
                            .compact()
                            .rounded(ButtonRounded::None)
                            .h_full()
                            .w(px(36.0))
                            .flex_none()
                            .icon(IconName::Plus)
                            .text_color(cx.theme().muted_foreground)
                            .tooltip(locale::text("New preview tab", "新建预览标签", "新增預覽標籤"))
                            .dropdown_menu(move |menu, _, _| {
                                let terminal_entity = new_terminal_entity.clone();
                                let terminal_pane_id = new_terminal_pane_id.clone();
                                menu.min_w(px(176.0)).max_w(px(176.0)).item(
                                    PopupMenuItem::new(locale::text(
                                        "New terminal",
                                        "新建终端",
                                        "新增終端",
                                    ))
                                    .icon(IconName::SquareTerminal)
                                    .disabled(!terminal_available)
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = terminal_entity.update(cx, |this, cx| {
                                                this.request_new_preview_terminal(
                                                    window.window_handle(),
                                                    Some(terminal_pane_id.clone()),
                                                    None,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(match active {
                        Some(tab_id) => self.render_tab_content(&tab_id, window, cx),
                        None => v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .p_4()
                            .text_center()
                            .child(
                                v_flex()
                                    .max_w(px(320.0))
                                    .items_center()
                                    .gap_3()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        div()
                                            .size(px(48.0))
                                            .items_center()
                                            .justify_center()
                                            .rounded_full()
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .bg(cx.theme().background)
                                            .text_color(cx.theme().foreground)
                                            .child(
                                                Icon::new(IconName::PanelLeftOpen).size(px(20.0)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_medium()
                                            .text_color(cx.theme().foreground)
                                            .child(locale::text(
                                                "No preview tabs",
                                                "暂无预览标签",
                                                "暫無預覽標籤",
                                            )),
                                    )
                                    .child(div().line_height(gpui::relative(1.5)).child(locale::text(
                                        "Open a file or terminal here while keeping the Agent visible.",
                                        "在这里打开文件或终端，同时保留 Agent 对话。",
                                        "在這裡開啟檔案或終端機，同時保留 Agent 對話。",
                                    )))
                                    .child(
                                        h_flex()
                                            .flex_wrap()
                                            .justify_center()
                                            .gap_2()
                                            .child(
                                                Button::new(format!(
                                                    "preview-empty-new-terminal:{pane_id}"
                                                ))
                                                .outline()
                                                .icon(IconName::SquareTerminal)
                                                .label(locale::text(
                                                    "New terminal",
                                                    "新建终端",
                                                    "新增終端",
                                                ))
                                                .on_click(move |_, window, cx| {
                                                    let _ = empty_terminal_entity.update(
                                                        cx,
                                                        |this, cx| {
                                                            this.request_new_preview_terminal(
                                                                window.window_handle(),
                                                                Some(
                                                                    empty_terminal_pane_id.clone(),
                                                                ),
                                                                None,
                                                                cx,
                                                            )
                                                        },
                                                    );
                                                }),
                                            ),
                                    ),
                            )
                            .into_any_element(),
                    })
                    .when_some(content_drop_region, |this, region| {
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .when(region == PreviewPaneDropRegion::Right, |overlay| {
                                    overlay.child(
                                        div()
                                            .absolute()
                                            .top_2()
                                            .right_2()
                                            .bottom_2()
                                            .left(relative(0.5))
                                            .rounded(px(6.0))
                                            .border_1()
                                            .border_color(cx.theme().primary.opacity(0.45))
                                            .bg(cx.theme().primary.opacity(0.15)),
                                    )
                                })
                                .when(region == PreviewPaneDropRegion::Bottom, |overlay| {
                                    overlay.child(
                                        div()
                                            .absolute()
                                            .top(relative(0.5))
                                            .right_2()
                                            .bottom_2()
                                            .left_2()
                                            .rounded(px(6.0))
                                            .border_1()
                                            .border_color(cx.theme().primary.opacity(0.45))
                                            .bg(cx.theme().primary.opacity(0.15)),
                                    )
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_preview_tab(
        &mut self,
        pane_id: &str,
        tab: PreviewTab,
        active: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = match &tab.target {
            PreviewTarget::Terminal { terminal_id } => self
                .terminals
                .iter()
                .find(|terminal| terminal.id.as_str() == terminal_id)
                .map(|terminal| terminal.title.clone())
                .unwrap_or_else(|| {
                    locale::text("Terminal unavailable", "终端不可用", "終端不可用").to_string()
                }),
            target => tab_label(target),
        };
        let target_icon = preview_target_icon(&tab.target, cx);
        let file_path = match &tab.target {
            PreviewTarget::File { path } | PreviewTarget::GitDiff { path, .. } => {
                Some(path.clone())
            }
            _ => None,
        };
        let reveal_path = match &tab.target {
            PreviewTarget::File { path } => Some(path.clone()),
            _ => None,
        };
        let open_in_editor = matches!(tab.target, PreviewTarget::GitDiff { .. });
        let open_in_editor_available = file_path
            .as_deref()
            .is_some_and(|path| file_can_open_in_editor(path, FileEntryKind::File));
        let workspace_available = self.workspace.is_some();
        let terminal_available = workspace_available && self.runtime.is_some();
        let dirty = tab
            .id
            .strip_prefix("file:")
            .and_then(|path| self.editors.buffers.get(path))
            .is_some_and(|buffer| buffer.dirty);
        let select_id = tab.id.clone();
        let keyboard_select_id = tab.id.clone();
        let close_click_id = tab.id.clone();
        let close_key_id = tab.id.clone();
        let middle_close_id = tab.id.clone();
        let drag_id = tab.id.clone();
        let tab_id = tab.id.clone();
        let accessible_label = tab_accessible_label(&tab, dirty);
        let pinned = tab.pinned;
        let temporary = tab.temporary && matches!(tab.target, PreviewTarget::File { .. });
        let active_tab = active == Some(tab.id.as_str());
        let context_entity = cx.weak_entity();
        let pane_id = pane_id.to_string();
        let context_pane_id = pane_id.clone();
        let pane_tabs = pane_tab_ids(&self.preview.root, &pane_id).unwrap_or_default();
        let can_close_other_tabs = pane_tabs.iter().any(|candidate| {
            candidate != &tab.id
                && self
                    .preview
                    .tabs
                    .get(candidate)
                    .is_some_and(|candidate| !candidate.pinned)
        });
        let can_close_all_tabs = pane_tabs.iter().any(|candidate| {
            self.preview
                .tabs
                .get(candidate)
                .is_some_and(|candidate| !candidate.pinned)
        });
        let target_id = tab.id.clone();
        let target_pane = pane_id.clone();
        let target_status = preview_tab_visual_status(&tab.target, self.git.status.as_ref());
        let target_status_color = target_status.map(|status| preview_status_color(status, cx));
        let target_deleted = matches!(target_status, Some(PreviewTabVisualStatus::Deleted));
        let drag_entity = cx.weak_entity();
        let drag_payload = PreviewTabDrag {
            tab_id: drag_id,
            label: label.clone().into(),
        };
        h_flex()
            .id(format!("preview-tab:{}", tab.id))
            .relative()
            .h_full()
            .flex_none()
            .items_center()
            .gap_1p5()
            .px_2()
            .text_xs()
            .border_r_1()
            .border_color(cx.theme().border)
            .cursor_default()
            .focusable()
            .tab_stop(true)
            .role(Role::Button)
            .aria_label(accessible_label)
            .text_color(if active_tab {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .bg(if active_tab {
                cx.theme().background
            } else {
                cx.theme().transparent
            })
            .hover(|style| {
                style
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
            })
            .focus_visible(|style| {
                style.bg(cx.theme().background).shadow(vec![
                    gpui::BoxShadow::new(px(0.0), px(0.0), cx.theme().ring).spread_radius(px(2.0)),
                ])
            })
            .on_click(cx.listener(move |this, _, _, cx| this.focus_tab(select_id.clone(), cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                    this.focus_tab(keyboard_select_id.clone(), cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    this.close_tab(middle_close_id.clone(), true, cx);
                    cx.stop_propagation();
                }),
            )
            .on_drag(drag_payload, move |drag, _, _, cx| {
                let _ = drag_entity.update(cx, |this, cx| {
                    this.preview_tab_drop_target = None;
                    this.preview_pane_drop_target = None;
                    cx.notify();
                });
                cx.new(|_| drag.clone())
            })
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<PreviewTabDrag>, _, cx| {
                    let drag = event.drag(cx);
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let next = (drag.tab_id != target_id).then_some(PreviewTabDropTarget {
                        pane_id: target_pane.clone(),
                        tab_id: target_id.clone(),
                        after: event.event.position.x >= event.bounds.center().x,
                    });
                    if this.preview_tab_drop_target != next {
                        this.preview_tab_drop_target = next;
                        this.preview_pane_drop_target = Some(PreviewPaneDropTarget {
                            pane_id: target_pane.clone(),
                            region: PreviewPaneDropRegion::TabGroup,
                        });
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &PreviewTabDrag, _, cx| {
                cx.stop_propagation();
                this.preview_pane_drop_target = None;
                let target = this.preview_tab_drop_target.take();
                if let Some(target) = target {
                    let mut order = this
                        .preview
                        .pane_ids()
                        .into_iter()
                        .find(|id| *id == pane_id)
                        .and_then(|_| pane_tab_ids(&this.preview.root, &pane_id))
                        .unwrap_or_default();
                    order.retain(|id| id != &drag.tab_id);
                    if let Some(index) = order.iter().position(|id| id == &target.tab_id) {
                        order.insert(index + usize::from(target.after), drag.tab_id.clone());
                    }
                    this.preview.move_to_pane(&drag.tab_id, &pane_id);
                    this.preview.reorder_pane_tabs(&pane_id, &order);
                    this.sync_terminal_surface_activity(cx);
                    this.persist(cx);
                    cx.notify();
                }
            }))
            .child(target_icon)
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .when(temporary, |this| this.italic())
                    .when_some(target_status_color, |this, color| this.text_color(color))
                    .when(target_deleted, |this| this.line_through())
                    .child(label),
            )
            .when(pinned, |this| {
                this.child(
                    div()
                        .ml_1()
                        .size(px(16.0))
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .opacity(0.70)
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            Icon::default()
                                .path("icons/vibex/pin-filled.svg")
                                .size(px(12.0)),
                        ),
                )
            })
            .when(!pinned, |this| {
                this.child(
                    div()
                        .id(format!("close-preview-tab:{}", tab.id))
                        .ml_1()
                        .size(px(16.0))
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .opacity(0.60)
                        .cursor_default()
                        .focusable()
                        .tab_stop(true)
                        .role(Role::Button)
                        .aria_label(locale::text("Close tab", "关闭标签", "關閉標籤"))
                        .hover(|style| style.bg(cx.theme().muted).opacity(1.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab(close_click_id.clone(), false, cx);
                            cx.stop_propagation();
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                                this.close_tab(close_key_id.clone(), false, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            // Keep a close gesture from arming the tab drag handler.
                            cx.stop_propagation();
                        })
                        .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(Icon::new(IconName::Close).size(px(12.0))),
                )
            })
            .when(active_tab, |this| {
                this.child(
                    div()
                        .absolute()
                        .left(px(2.0))
                        .right(px(2.0))
                        .bottom_0()
                        .h(px(1.0))
                        .rounded_full()
                        .bg(cx.theme().primary.opacity(0.80)),
                )
            })
            .context_menu(move |menu, window, cx| {
                let focus_entity = context_entity.clone();
                let focus_id = tab_id.clone();
                let _ = focus_entity.update(cx, |this, cx| this.focus_tab(focus_id.clone(), cx));
                let pin_entity = context_entity.clone();
                let pin_id = tab_id.clone();
                let split_right_entity = context_entity.clone();
                let split_right_id = tab_id.clone();
                let split_right_pane = context_pane_id.clone();
                let split_down_entity = context_entity.clone();
                let split_down_id = tab_id.clone();
                let split_down_pane = context_pane_id.clone();
                let close_entity = context_entity.clone();
                let close_id = tab_id.clone();
                let close_other_entity = context_entity.clone();
                let close_other_id = tab_id.clone();
                let close_other_pane = context_pane_id.clone();
                let close_all_entity = context_entity.clone();
                let close_all_pane = context_pane_id.clone();
                let mut menu = menu
                    .min_w(px(208.0))
                    .max_w(px(208.0))
                    .item(
                        PopupMenuItem::new(locale::text("Close tab", "关闭标签", "關閉標籤"))
                            .icon(IconName::Close)
                            .disabled(pinned)
                            .on_click(move |_, _, cx| {
                                let _ = close_entity.update(cx, |this, cx| {
                                    this.close_tab(close_id.clone(), false, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text(
                            "Close other tabs",
                            "关闭其他标签",
                            "關閉其他標籤",
                        ))
                        .icon(Icon::default().path("icons/vibex/chevrons-right-left.svg"))
                        .disabled(!can_close_other_tabs)
                        .on_click(move |_, _, cx| {
                            let _ = close_other_entity.update(cx, |this, cx| {
                                this.close_other_tabs(
                                    close_other_id.clone(),
                                    close_other_pane.clone(),
                                    cx,
                                )
                            });
                        }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text(
                            "Close all tabs",
                            "关闭所有标签",
                            "關閉所有標籤",
                        ))
                        .icon(Icon::default().path("icons/vibex/chevrons-down-up.svg"))
                        .disabled(!can_close_all_tabs)
                        .on_click(move |_, _, cx| {
                            let _ = close_all_entity.update(cx, |this, cx| {
                                this.close_all_tabs(close_all_pane.clone(), cx)
                            });
                        }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(locale::text("Split right", "向右拆分", "向右分割"))
                            .icon(Icon::default().path("icons/vibex/chevrons-right.svg"))
                            .on_click(move |_, _, cx| {
                                let _ = split_right_entity.update(cx, |this, cx| {
                                    this.split_tab(
                                        split_right_id.clone(),
                                        split_right_pane.clone(),
                                        PreviewSplitPosition::Right,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Split down", "向下拆分", "向下分割"))
                            .icon(Icon::default().path("icons/vibex/chevrons-down-up.svg"))
                            .on_click(move |_, _, cx| {
                                let _ = split_down_entity.update(cx, |this, cx| {
                                    this.split_tab(
                                        split_down_id.clone(),
                                        split_down_pane.clone(),
                                        PreviewSplitPosition::Bottom,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(if pinned {
                            locale::text("Unpin tab", "取消固定标签", "取消固定標籤")
                        } else {
                            locale::text("Pin tab", "固定标签", "固定標籤")
                        })
                        .icon(Icon::default().path("icons/vibex/pin.svg"))
                        .on_click(move |_, _, cx| {
                            let _ = pin_entity
                                .update(cx, |this, cx| this.toggle_pin(pin_id.clone(), cx));
                        }),
                    );
                if let Some(path) = reveal_path.clone() {
                    let reveal_entity = context_entity.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::new(locale::text("Reveal in Files", "定位文件", "定位檔案"))
                            .icon(Icon::default().path("icons/vibex/files.svg"))
                            .on_click(move |_, _, cx| {
                                let _ = reveal_entity.update(cx, |this, cx| {
                                    this.reveal_file_in_right_rail(path.clone(), cx)
                                });
                            }),
                    );
                }
                let file_open_path = file_path.clone();
                if file_open_path.is_some() {
                    let submenu_entity = context_entity.clone();
                    let submenu_pane_id = context_pane_id.clone();
                    let can_open_in_file_system = file_open_path.is_some() && workspace_available;
                    let can_open_in_editor = open_in_editor_available && can_open_in_file_system;
                    menu =
                        menu.separator().submenu_with_icon(
                            Some(Icon::new(IconName::ExternalLink)),
                            locale::text("Open In", "Open In", "Open In"),
                            window,
                            cx,
                            move |mut submenu, _, _| {
                                submenu = submenu.min_w(px(208.0)).max_w(px(208.0));
                                if let Some(path) = file_open_path.clone() {
                                    if open_in_editor {
                                        let editor_entity = submenu_entity.clone();
                                        let editor_path = path.clone();
                                        submenu = submenu.item(
                                            PopupMenuItem::new(locale::text(
                                                "Editor",
                                                "编辑器",
                                                "編輯器",
                                            ))
                                            .icon(Icon::default().path("icons/vibex/pencil.svg"))
                                            .disabled(!can_open_in_editor)
                                            .on_click(move |_, window, cx| {
                                                let _ = editor_entity.update(cx, |this, cx| {
                                                    this.open_file(
                                                        editor_path.clone(),
                                                        false,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            }),
                                        );
                                    }
                                    let default_entity = submenu_entity.clone();
                                    let default_path = path.clone();
                                    let integrated_entity = submenu_entity.clone();
                                    let integrated_path = path.clone();
                                    let integrated_pane = submenu_pane_id.clone();
                                    let native_entity = submenu_entity.clone();
                                    let native_path = path.clone();
                                    submenu = submenu
                                        .item(
                                            PopupMenuItem::new(locale::text(
                                                "Default App",
                                                "默认应用",
                                                "預設應用程式",
                                            ))
                                            .icon(IconName::ExternalLink)
                                            .disabled(!can_open_in_file_system)
                                            .on_click(move |_, _, cx| {
                                                let _ = default_entity.update(cx, |this, cx| {
                                                    this.open_default_app(default_path.clone(), cx)
                                                });
                                            }),
                                        )
                                        .item(
                                            PopupMenuItem::new(locale::text(
                                                "Terminal", "终端", "終端",
                                            ))
                                            .icon(IconName::SquareTerminal)
                                            .disabled(
                                                !terminal_available || !can_open_in_file_system,
                                            )
                                            .on_click(move |_, window, cx| {
                                                let _ = integrated_entity.update(cx, |this, cx| {
                                                    this.request_new_preview_terminal(
                                                        window.window_handle(),
                                                        Some(integrated_pane.clone()),
                                                        Some((
                                                            integrated_path.clone(),
                                                            FileEntryKind::File,
                                                        )),
                                                        cx,
                                                    )
                                                });
                                            }),
                                        )
                                        .item(
                                            PopupMenuItem::new(locale::text(
                                                "Native Terminal",
                                                "本机终端",
                                                "本機終端",
                                            ))
                                            .icon(IconName::SquareTerminal)
                                            .disabled(!can_open_in_file_system)
                                            .on_click(move |_, _, cx| {
                                                let _ = native_entity.update(cx, |this, cx| {
                                                    this.open_native_terminal(
                                                        native_path.clone(),
                                                        cx,
                                                    )
                                                });
                                            }),
                                        )
                                        .separator()
                                        .item(open_tool_menu_section_label(locale::text(
                                            "Tools", "工具", "工具",
                                        )));
                                    let tools = available_external_tools();
                                    for tool in &tools {
                                        let tool_entity = submenu_entity.clone();
                                        let tool_path = path.clone();
                                        let tool_id = tool.id.to_string();
                                        submenu = submenu.item(
                                            open_tool_menu_item(tool.id, tool.label)
                                                .disabled(!can_open_in_file_system)
                                                .on_click(move |_, _, cx| {
                                                    let _ = tool_entity.update(cx, |this, cx| {
                                                        this.open_with_tool(
                                                            tool_path.clone(),
                                                            tool_id.clone(),
                                                            cx,
                                                        )
                                                    });
                                                }),
                                        );
                                    }
                                    if tools.is_empty() {
                                        submenu = submenu.item(
                                            PopupMenuItem::new(locale::text(
                                                "No installed IDE or project tool detected",
                                                "未探测到已安装的 IDE 或项目工具",
                                                "未探測到已安裝的 IDE 或專案工具",
                                            ))
                                            .disabled(true),
                                        );
                                    }
                                }
                                submenu
                            },
                        );
                }
                menu
            })
            .into_any_element()
    }

    fn render_tab_content(
        &mut self,
        tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tab) = self.preview.tabs.get(tab_id).cloned() else {
            return self.render_empty(
                locale::text(
                    "Preview target is unavailable",
                    "预览目标不可用",
                    "預覽目標無法使用",
                ),
                cx,
            );
        };
        let content = match tab.target {
            PreviewTarget::File { path } => self.render_file_content(path, window, cx),
            PreviewTarget::GitDiff { path, staged } => {
                self.render_diff_content(tab_id, GitSelectionKey { path, staged }, cx)
            }
            PreviewTarget::GitCommit {
                commit_hash,
                subject,
                focus_path,
                focus_request_id,
            } => self.render_commit_content(
                tab_id,
                commit_hash,
                subject,
                focus_path,
                focus_request_id,
                cx,
            ),
            PreviewTarget::Terminal { terminal_id } => self
                .terminal_surfaces
                .get(&terminal_id)
                .cloned()
                .map(|surface| {
                    surface
                        .cached(StyleRefinement::default().size_full())
                        .into_any_element()
                })
                .unwrap_or_else(|| {
                    self.render_native_boundary(
                        IconName::SquareTerminal,
                        locale::text("Terminal unavailable", "终端不可用", "終端不可用"),
                        match locale::current_locale() {
                            locale::ResolvedLocale::En => {
                                format!("Terminal {terminal_id} is no longer attached")
                            }
                            locale::ResolvedLocale::ZhCn => {
                                format!("终端 {terminal_id} 已断开连接")
                            }
                            locale::ResolvedLocale::ZhTw => {
                                format!("終端機 {terminal_id} 已中斷連線")
                            }
                        },
                        cx,
                    )
                }),
        };
        let entity = cx.entity().clone();
        let lifecycle_tab_id = tab_id.to_string();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(content)
            .child(
                canvas(
                    move |bounds, window, cx| {
                        entity.update(cx, |this, _| {
                            this.update_lifecycle_bounds(
                                &lifecycle_tab_id,
                                bounds,
                                window.scale_factor(),
                            )
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    fn render_file_content(
        &mut self,
        path: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let markdown = self.presentations.get(&path).and_then(|presentation| {
            if let FilePresentation::Markdown { document, images } = presentation {
                Some((document.clone(), images.clone()))
            } else {
                None
            }
        });
        if let Some((document, images)) = markdown
            && !self.markdown_edit_paths.contains(&path)
        {
            let edit_path = path.clone();
            let workspace_links = document
                .resources
                .iter()
                .filter(|asset| {
                    asset.kind == ResourceKind::Workspace && asset.role == ResourceRole::Link
                })
                .filter_map(|asset| asset.resolved.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(32)
                .collect::<Vec<_>>();
            let scroll = self
                .markdown_scrolls
                .entry(path.clone())
                .or_default()
                .clone();
            let view_scroll = scroll.clone();
            let markdown_entity = cx.weak_entity();
            let markdown_view =
                MarkdownView::from_document(format!("markdown-preview:{path}"), document)
                    .images(images)
                    .allow_http_images(true)
                    .scroll_handle(view_scroll)
                    .on_open_resource(move |resource, window, cx| {
                        let _ = markdown_entity.update(cx, |this, cx| {
                            this.open_markdown_resource(resource, window, cx)
                        });
                    });
            return v_flex()
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .h(px(34.0))
                        .flex_none()
                        .justify_between()
                        .px_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(div().text_xs().child(path.clone()))
                        .child(
                            Button::new(format!("edit-markdown:{path}"))
                                .small()
                                .ghost()
                                .compact()
                                .icon(IconName::Replace)
                                .tooltip(locale::text(
                                    "Edit Markdown source",
                                    "编辑 Markdown 源文件",
                                    "編輯 Markdown 原始檔",
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_markdown_source(edit_path.clone(), cx)
                                })),
                        ),
                )
                .when(!workspace_links.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .h(px(34.0))
                            .flex_none()
                            .gap_1()
                            .px_2()
                            .overflow_x_scrollbar()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .children(workspace_links.into_iter().map(|link_path| {
                                let open_path = link_path.clone();
                                Button::new(format!("open-markdown-link:{path}:{link_path}"))
                                    .small()
                                    .outline()
                                    .icon(IconName::File)
                                    .label(link_path)
                                    .tooltip(locale::text(
                                        "Open workspace link in Preview",
                                        "在预览中打开工作区链接",
                                        "在預覽中開啟工作區連結",
                                    ))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_file(open_path.clone(), false, window, cx)
                                    }))
                            })),
                    )
                })
                .child(
                    div()
                        .id(format!("markdown-scroll:{path}"))
                        .flex_1()
                        .min_h_0()
                        .track_scroll(&scroll)
                        .overflow_y_scrollbar()
                        .p_4()
                        .child(markdown_view),
                )
                .into_any_element();
        }
        if let Some(binding) = self.editor_bindings.get(&path).cloned() {
            let buffer = self.editors.buffers.get(&path).cloned();
            let save_path = path.clone();
            let markdown_path = path.clone();
            let editable = buffer.as_ref().is_some_and(|buffer| buffer.editable());
            let dirty = buffer.as_ref().is_some_and(|buffer| buffer.dirty);
            let status = buffer
                .as_ref()
                .map(editor_status)
                .unwrap_or_else(|| "Loading".to_string());
            return v_flex()
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .h(px(34.0))
                        .flex_none()
                        .justify_between()
                        .gap_2()
                        .px_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .min_w_0()
                                .text_ellipsis()
                                .text_xs()
                                .child(format!("{path} - {status}")),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .when(
                                    content_preview_kind_for_path(&path)
                                        == ContentPreviewKind::Markdown,
                                    |this| {
                                        this.child(
                                            Button::new(format!("preview-markdown:{path}"))
                                                .small()
                                                .ghost()
                                                .compact()
                                                .icon(IconName::File)
                                                .tooltip(locale::text(
                                                    "Render Markdown",
                                                    "渲染 Markdown",
                                                    "呈現 Markdown",
                                                ))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_markdown_source(
                                                        markdown_path.clone(),
                                                        cx,
                                                    )
                                                })),
                                        )
                                    },
                                )
                                .child(
                                    Button::new(format!("save-editor:{path}"))
                                        .small()
                                        .ghost()
                                        .compact()
                                        .icon(IconName::Check)
                                        .tooltip(locale::text("Save file", "保存文件", "儲存檔案"))
                                        .disabled(!editable || !dirty)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.save_editor(save_path.clone(), cx)
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .font_family(self.code_font_family.clone())
                        .text_size(px(f32::from(self.code_font_size)))
                        .font_weight(code_font_weight(cx))
                        .child(
                            Input::new(&binding.input)
                                .appearance(false)
                                .h_full()
                                .disabled(!editable),
                        ),
                )
                .into_any_element();
        }
        match self.presentations.get(&path) {
            Some(FilePresentation::Loading) => self.render_empty(
                locale::text("Loading file", "正在加载文件", "正在載入檔案"),
                cx,
            ),
            Some(FilePresentation::Image { image, .. }) => div()
                .size_full()
                .overflow_scrollbar()
                .p_3()
                .child(img(image.clone()).max_w_full().max_h_full())
                .into_any_element(),
            Some(FilePresentation::MediaExternalOnly) => self.render_native_boundary(
                IconName::ExternalLink,
                locale::text(
                    "Media opens externally",
                    "媒体将在外部打开",
                    "媒體將在外部開啟",
                ),
                locale::text(
                    "Native GPUI v1 preserves the current browser-media capability boundary",
                    "原生 GPUI v1 当前通过外部浏览器打开媒体",
                    "原生 GPUI v1 目前透過外部瀏覽器開啟媒體",
                ),
                cx,
            ),
            Some(FilePresentation::Pdf(surface)) => surface
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            Some(FilePresentation::Office(surface)) => surface
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            Some(FilePresentation::Unsupported(message)) => self.render_native_boundary(
                IconName::TriangleAlert,
                locale::text("Unsupported file", "不支持的文件", "不支援的檔案"),
                locale::localize_error_message(message),
                cx,
            ),
            Some(FilePresentation::Error { code, message }) => self.render_native_boundary(
                IconName::TriangleAlert,
                locale::text("File preview failed", "文件预览失败", "檔案預覽失敗"),
                locale::localize_error_message(&format!("{code}: {message}")),
                cx,
            ),
            Some(FilePresentation::Markdown { .. }) | None => self.render_empty(
                locale::text(
                    "File preview is not loaded",
                    "文件预览尚未加载",
                    "檔案預覽尚未載入",
                ),
                cx,
            ),
        }
    }

    fn render_diff_content(
        &mut self,
        tab_id: &str,
        key: GitSelectionKey,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview_error = self.git_preview_errors.get(tab_id).cloned();
        if let Some(error) = preview_error {
            return self.render_git_preview_error(&key.path, error, cx);
        }
        let Some((count, truncated, has_files, revision)) =
            self.git.diff_mut(&key).map(|document| {
                (
                    document.rows.len(),
                    document.truncated,
                    !document.files.is_empty(),
                    document.revision.clone(),
                )
            })
        else {
            return self.render_git_preview_loading(&key.path, cx);
        };
        let path = key.path.clone();
        let key_for_list = key.clone();
        let code_font_family = self.code_font_family.clone();
        let diff_row_height = diff_row_height(self.code_font_size);
        let list = if !has_files {
            self.render_empty(locale::text("No diff", "没有差异", "沒有差異"), cx)
        } else if count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .text_center()
                .child(div().text_sm().child(locale::text(
                    "No content changes in this file.",
                    "此文件没有内容变更。",
                    "此檔案沒有內容變更。",
                )))
                .into_any_element()
        } else {
            let list_state = self
                .preview_diff_lists
                .entry(tab_id.to_string())
                .or_insert_with(|| PatchListState::new(revision.clone(), count, diff_row_height));
            list_state.reconcile(&revision, count, diff_row_height);
            let list_state = list_state.list.clone();
            self.render_patch_list(
                format!("diff-rows:{}:{}", key.staged, key.path),
                list_state,
                move |this, index, _, cx| {
                    let Some(row) = this
                        .git
                        .diffs
                        .get_mut(&key_for_list)
                        .and_then(|document| document.rows.prepared_row(index))
                    else {
                        return div().w_full().h(px(diff_row_height)).into_any_element();
                    };
                    render_diff_row(row, &code_font_family, diff_row_height, cx)
                },
                cx,
            )
        };
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path.as_str())
            .to_string();
        let edit_path = path.clone();
        let edit_tab_id = tab_id.to_string();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .min_h(px(48.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.20))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(path.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new(format!("git-diff-edit:{edit_tab_id}"))
                                    .small()
                                    .ghost()
                                    .compact()
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .p_0()
                                    .tooltip(locale::text("Open file", "打开文件", "開啟檔案"))
                                    .child(Icon::default().path("icons/pencil.svg").size(px(14.0)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_file(edit_path.clone(), false, window, cx)
                                    })),
                            )
                            .child(preview_badge(
                                if key.staged {
                                    locale::text("staged", "已暂存", "已暫存")
                                } else {
                                    locale::text("unstaged", "未暂存", "未暫存")
                                },
                                cx,
                            ))
                            .when(truncated, |this| {
                                this.child(preview_destructive_badge("truncated", cx))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(render_truncated_alert(truncated, cx))
                    .child(div().relative().flex_1().min_h_0().child(list)),
            )
            .into_any_element()
    }

    fn render_commit_content(
        &mut self,
        tab_id: &str,
        hash: String,
        _subject: Option<String>,
        focus_path: Option<String>,
        focus_request_id: Option<u64>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview_error = self.git_preview_errors.get(tab_id).cloned();
        if let Some(error) = preview_error {
            return self.render_git_preview_error(&hash, error, cx);
        }
        let Some(detail) = self.git.commit_detail_for(&hash).cloned() else {
            return self.render_git_preview_loading(&hash, cx);
        };
        if !self.git.commit_patch_ready(&hash) {
            return self.render_git_preview_loading(&detail.summary.subject, cx);
        }

        let mut focused_row_index = None;
        if let Some(path) = focus_path.as_deref() {
            let request_id = focus_request_id.unwrap_or_default();
            if self.preview_commit_focus_requests.get(tab_id).copied() != Some(request_id) {
                focused_row_index = self.git.focus_commit_file(&hash, path);
                self.preview_commit_focus_requests
                    .insert(tab_id.to_string(), request_id);
            }
        }
        let row_count = self.git.commit_preview_row_count(&hash);
        let commit_revision = format!("commit:{hash}");
        let diff_row_height = diff_row_height(self.code_font_size);
        let list_state = self
            .preview_commit_lists
            .entry(tab_id.to_string())
            .or_insert_with(|| {
                PatchListState::new(commit_revision.clone(), row_count, diff_row_height)
            });
        list_state.reconcile(&commit_revision, row_count, diff_row_height);
        let list_state = list_state.list.clone();
        if let Some(row_index) = focused_row_index {
            list_state.scroll_to(ListOffset {
                item_ix: row_index,
                offset_in_item: px(0.0),
            });
        }
        let hash_for_list = hash.clone();
        let code_font_family = self.code_font_family.clone();
        let list = if row_count == 0 {
            self.render_empty(
                locale::text("Commit detail", "提交详情", "提交詳細資料"),
                cx,
            )
        } else {
            self.render_patch_list(
                format!("commit-rows:{hash}"),
                list_state,
                move |this, index, _, cx| {
                    this.git
                        .commit_preview_window(&hash_for_list, index, 1)
                        .into_iter()
                        .next()
                        .map(|row| {
                            render_commit_patch_row(
                                row,
                                hash_for_list.clone(),
                                code_font_family.clone(),
                                diff_row_height,
                                cx,
                            )
                        })
                        .unwrap_or_else(|| div().w_full().h(px(diff_row_height)).into_any_element())
                },
                cx,
            )
        };
        let patch_status = if detail.patch_truncated {
            locale::text("truncated", "已截断", "已截斷")
        } else {
            locale::text("loaded", "已加载", "已載入")
        };
        let patch_badge = match locale::current_locale() {
            locale::ResolvedLocale::En => {
                format!("{} files · patch {}", detail.files.len(), patch_status)
            }
            locale::ResolvedLocale::ZhCn => {
                format!("{} 个文件 · 补丁{}", detail.files.len(), patch_status)
            }
            locale::ResolvedLocale::ZhTw => {
                format!("{} 個檔案 · 補丁{}", detail.files.len(), patch_status)
            }
        };
        let body_lines = detail
            .body
            .as_deref()
            .filter(|body| !body.is_empty())
            .map(|body| body.split('\n').map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .flex_none()
                    .min_w_0()
                    .gap_1()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.20))
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(detail.summary.subject.clone()),
                            )
                            .child(preview_badge(patch_badge, cx)),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .children([
                                detail.summary.short_hash.clone(),
                                detail.summary.author_name.clone(),
                                git_commit_authored_at(detail.summary.authored_at_ms),
                            ]),
                    )
                    .when(!body_lines.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .mt_3()
                                .line_height(gpui::relative(1.5))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .children(body_lines.into_iter().map(|line| {
                                    div().whitespace_normal().child(if line.is_empty() {
                                        " ".to_string()
                                    } else {
                                        line
                                    })
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(render_truncated_alert(detail.patch_truncated, cx))
                    .child(div().relative().flex_1().min_h_0().child(list)),
            )
            .into_any_element()
    }

    fn render_patch_list<F>(
        &mut self,
        list_id: String,
        state: ListState,
        render_row: F,
        cx: &mut Context<Self>,
    ) -> AnyElement
    where
        F: Fn(&mut Self, usize, &mut Window, &mut Context<Self>) -> AnyElement + 'static,
    {
        div()
            .id(list_id)
            .size_full()
            .min_h_0()
            .child(
                list(
                    state,
                    cx.processor(move |this, index, window, cx| {
                        render_row(this, index, window, cx)
                    }),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn render_git_preview_loading(&self, title: &str, _cx: &Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .text_center()
            .child(Icon::new(IconName::LoaderCircle).size(px(16.0)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(title.to_string()),
            )
            .into_any_element()
    }

    fn render_git_preview_error(
        &self,
        title: &str,
        error: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                h_flex()
                    .w_full()
                    .max_w(px(576.0))
                    .items_start()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().danger.opacity(0.48))
                    .bg(cx.theme().danger.opacity(0.08))
                    .p_4()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(16.0))
                            .text_color(cx.theme().danger),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(title.to_string()))
                            .child(
                                div()
                                    .whitespace_normal()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(error),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_native_boundary(
        &self,
        icon: IconName,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        cx: &Context<Self>,
    ) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .text_center()
            .child(Icon::new(icon))
            .child(div().text_sm().font_medium().child(title.into()))
            .child(
                div()
                    .max_w(px(460.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.into()),
            )
            .into_any_element()
    }
}

impl Render for CodeWorkbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_restore_hydration(window, cx);
        if !cx.has_active_drag() {
            self.preview_tab_drop_target = None;
            self.preview_pane_drop_target = None;
        }
        let pane_ids = self
            .preview
            .pane_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        self.preview_tab_scrolls
            .retain(|pane_id, _| pane_ids.contains(pane_id));
        self.markdown_scrolls
            .retain(|path, _| self.presentations.contains_key(path));
        self.preview_revealed_tab_ids
            .retain(|pane_id, _| pane_ids.contains(pane_id));
        let tab_ids = self.preview.tabs.keys().cloned().collect::<BTreeSet<_>>();
        self.preview_diff_lists
            .retain(|tab_id, _| tab_ids.contains(tab_id));
        self.preview_commit_lists
            .retain(|tab_id, _| tab_ids.contains(tab_id));
        self.preview_commit_focus_requests
            .retain(|tab_id, _| tab_ids.contains(tab_id));
        self.git_preview_errors
            .retain(|tab_id, _| tab_ids.contains(tab_id));
        let is_fullscreen = self.preview_panel_fullscreen;
        let terminal_available = self.workspace.is_some() && self.runtime.is_some();
        let root = self.preview.root.clone();
        let side_preview = self.preview.side_preview_tab_id.clone();
        v_flex()
            .id("code-workbench-preview")
            .size_full()
            .min_w_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(44.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(locale::text("Preview", "预览", "預覽")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("preview-new-terminal")
                                    .small()
                                    .ghost()
                                    .compact()
                                    .size(px(28.0))
                                    .icon(IconName::SquareTerminal)
                                    .tooltip(locale::text(
                                        "New terminal",
                                        "新建终端",
                                        "新增終端",
                                    ))
                                    .disabled(!terminal_available)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.request_new_preview_terminal(
                                            window.window_handle(),
                                            None,
                                            None,
                                            cx,
                                        )
                                    })),
                            )
                            .child(div().mx_1().h(px(20.0)).w(px(1.0)).bg(cx.theme().border))
                            .child(
                                Button::new("toggle-preview-fullscreen")
                                    .small()
                                    .ghost()
                                    .compact()
                                    .size(px(28.0))
                                    .icon(if is_fullscreen {
                                        IconName::Minimize
                                    } else {
                                        IconName::Maximize
                                    })
                                    .tooltip(if is_fullscreen {
                                        locale::text(
                                            "Exit full screen",
                                            "退出全屏",
                                            "退出全螢幕",
                                        )
                                    } else {
                                        locale::text(
                                            "Full screen",
                                            "全屏",
                                            "全螢幕",
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_fullscreen(cx);
                                    })),
                            )
                            .child(
                                Button::new("close-preview-panel")
                                    .small()
                                    .ghost()
                                    .compact()
                                    .size(px(28.0))
                                    .icon(IconName::Close)
                                    .tooltip(locale::text(
                                        "Close preview panel",
                                        "关闭预览面板",
                                        "關閉預覽面板",
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request_close_preview_panel(cx)
                                    })),
                            ),
                    ),
            )
            .when_some(self.pending_workspace.clone(), |this, pending| {
                this.child(
                    h_flex()
                        .flex_none()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().warning.opacity(0.12))
                        .child(div().min_w_0().text_xs().child(
                            match locale::current_locale() {
                                locale::ResolvedLocale::En => format!(
                                    "Dirty buffers keep Preview on the current workspace; pending {}",
                                    pending.root.display()
                                ),
                                locale::ResolvedLocale::ZhCn => format!(
                                    "存在未保存的缓冲区，预览仍停留在当前工作区；等待切换到 {}",
                                    pending.root.display()
                                ),
                                locale::ResolvedLocale::ZhTw => format!(
                                    "存在未儲存的緩衝區，預覽仍停留在目前工作區；等待切換到 {}",
                                    pending.root.display()
                                ),
                            },
                        ))
                        .child(
                            Button::new("discard-dirty-switch-workspace")
                                .small()
                                .danger()
                                .label(locale::text(
                                    "Discard and switch",
                                    "放弃更改并切换",
                                    "捨棄變更並切換",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.discard_and_apply_pending_workspace(cx)
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(if is_fullscreen {
                        self.render_preview_node(root, window, cx)
                    } else if let Some(side_tab) = side_preview {
                        h_resizable("preview-side-layout")
                            .child(
                                resizable_panel()
                                    .size(px(420.0))
                                    .size_range(px(180.0)..gpui::Pixels::MAX)
                                    .child(self.render_preview_node(root, window, cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(320.0))
                                    .size_range(px(180.0)..gpui::Pixels::MAX)
                                    .child(self.render_tab_content(&side_tab, window, cx)),
                            )
                            .into_any_element()
                    } else {
                        self.render_preview_node(root, window, cx)
                    }),
            )
    }
}

pub struct CodeRightRail {
    workbench: Entity<CodeWorkbench>,
    projection: CodeRightRailProjection,
    file_search_input: Entity<InputState>,
    file_search_mode: FileSearchMode,
    file_search_options: FileSearchOptions,
    file_search_results: Vec<FileSearchResult>,
    file_search_collapsed_paths: BTreeSet<String>,
    file_search_loading: bool,
    file_search_error: Option<String>,
    file_search_generation: u64,
    file_search_task: Option<Task<()>>,
    history_search_input: Entity<InputState>,
    history_search_generation: u64,
    history_search_task: Option<Task<()>>,
    history_branch_select: Entity<SelectState<Vec<GitHistoryBranchChoice>>>,
    history_author_select: Entity<SelectState<Vec<GitHistoryAuthorChoice>>>,
    history_date_picker: Entity<DatePickerState>,
    history_drawer_height: f32,
    history_body_height: f32,
    history_drawer_resize: Option<GitHistoryDrawerResizeState>,
    inline_path_input: Entity<InputState>,
    inline_file_action: Option<InlineFileAction>,
    inline_file_error: Option<String>,
    file_tree_focus: FocusHandle,
    file_typeahead: String,
    file_clipboard: Option<FileClipboardEntry>,
    file_drag_path: Option<String>,
    file_drop_target_path: Option<String>,
    selected_open_tool_id: Option<String>,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    render_count: usize,
}

impl CodeRightRail {
    pub fn new(
        workbench: Entity<CodeWorkbench>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let file_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(locale::text(
                "Search workspace files",
                "搜索工作区文件",
                "搜尋工作區檔案",
            ))
        });
        let inline_path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(inline_path_placeholder(None)));
        let file_tree_focus = cx.focus_handle().tab_stop(true);
        let projection = workbench.read(cx).right_rail_projection();
        let history_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(locale::text(
                    "Search commits",
                    "搜索提交信息或代码",
                    "搜尋提交資訊或代碼",
                ))
                .default_value(projection.git.history_query.clone().unwrap_or_default())
        });
        let branch_choices = git_history_branch_choices(
            projection.git.branches.as_ref(),
            projection.git.remotes.as_ref(),
        );
        let branch_selected = projection
            .git
            .selected_branch
            .as_ref()
            .and_then(|selected| {
                branch_choices
                    .iter()
                    .position(|choice| &choice.name == selected)
                    .map(IndexPath::new)
            });
        let history_branch_select = cx.new(|cx| {
            SelectState::new(branch_choices, branch_selected, window, cx).searchable(true)
        });
        let author_choices = git_history_author_choices(projection.git.authors.as_ref());
        let selected_author_value = projection.git.selected_author.clone();
        let author_selected = author_choices
            .iter()
            .position(|choice| choice.value == selected_author_value)
            .map(IndexPath::new);
        let history_author_select = cx.new(|cx| {
            SelectState::new(author_choices, author_selected, window, cx).searchable(true)
        });
        let history_date_picker =
            cx.new(|cx| DatePickerState::range(window, cx).date_format("%Y-%m-%d"));
        let initial_date = git_history_filter_date(
            projection.git.history_authored_after_ms,
            projection.git.history_authored_before_ms,
        );
        history_date_picker.update(cx, |picker, cx| picker.set_date(initial_date, window, cx));
        let workbench_subscription =
            cx.observe_in(&workbench, window, |this, workbench, window, cx| {
                let previous_workspace_generation = this.projection.revision.workspace_generation;
                let next = {
                    let workbench = workbench.read(cx);
                    let revision = workbench.right_rail_revision();
                    if revision == this.projection.revision {
                        return;
                    }
                    workbench.right_rail_projection()
                };
                this.projection = next;
                if this.projection.revision.workspace_generation != previous_workspace_generation {
                    this.file_search_results.clear();
                    this.file_search_collapsed_paths.clear();
                    this.file_search_error = None;
                    this.schedule_file_search(cx);
                    this.history_search_generation = this.history_search_generation.wrapping_add(1);
                    this.history_search_task = None;
                    this.history_search_input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    this.history_drawer_height = GIT_HISTORY_DRAWER_DEFAULT_HEIGHT;
                }
                this.sync_history_filter_controls(window, cx);
                cx.notify();
            });
        let file_search_subscription = cx.subscribe_in(
            &file_search_input,
            window,
            |this, _, event, _, cx| match event {
                InputEvent::Change => this.schedule_file_search(cx),
                InputEvent::PressEnter { shift: false, .. } => this.start_file_search(cx),
                _ => {}
            },
        );
        let input_subscription = cx.subscribe_in(
            &inline_path_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } | InputEvent::Blur
                    if this.inline_file_action.is_some() =>
                {
                    this.submit_inline_file_action(window, cx);
                }
                _ => {}
            },
        );
        let history_search_subscription = cx.subscribe_in(
            &history_search_input,
            window,
            |this, _, event, _, cx| match event {
                InputEvent::Change => this.schedule_history_search(cx),
                InputEvent::PressEnter { shift: false, .. } => this.start_history_search(cx),
                _ => {}
            },
        );
        let history_branch_subscription = cx.subscribe(
            &history_branch_select,
            |this, _, event: &SelectEvent<Vec<GitHistoryBranchChoice>>, cx| {
                let SelectEvent::Confirm(branch) = event;
                let Some(branch) = branch.clone() else {
                    return;
                };
                this.update_workbench(cx, |workbench, cx| workbench.set_history_branch(branch, cx));
            },
        );
        let history_author_subscription = cx.subscribe(
            &history_author_select,
            |this, _, event: &SelectEvent<Vec<GitHistoryAuthorChoice>>, cx| {
                let SelectEvent::Confirm(author) = event;
                let author = author.clone().flatten();
                this.update_workbench(cx, |workbench, cx| workbench.set_history_author(author, cx));
            },
        );
        let history_date_subscription = cx.subscribe(
            &history_date_picker,
            |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(date) = event;
                let (after_ms, before_ms) = git_history_date_bounds(*date);
                this.update_workbench(cx, |workbench, cx| {
                    workbench.set_history_date_range(after_ms, before_ms, cx)
                });
            },
        );
        let file_tree_blur_subscription =
            cx.on_blur(&file_tree_focus, window, Self::on_file_tree_blur);
        Self {
            workbench,
            projection,
            file_search_input,
            file_search_mode: FileSearchMode::Name,
            file_search_options: FileSearchOptions::default(),
            file_search_results: Vec::new(),
            file_search_collapsed_paths: BTreeSet::new(),
            file_search_loading: false,
            file_search_error: None,
            file_search_generation: 0,
            file_search_task: None,
            history_search_input,
            history_search_generation: 0,
            history_search_task: None,
            history_branch_select,
            history_author_select,
            history_date_picker,
            history_drawer_height: GIT_HISTORY_DRAWER_DEFAULT_HEIGHT,
            history_body_height: GIT_HISTORY_DRAWER_DEFAULT_HEIGHT,
            history_drawer_resize: None,
            inline_path_input,
            inline_file_action: None,
            inline_file_error: None,
            file_tree_focus,
            file_typeahead: String::new(),
            file_clipboard: None,
            file_drag_path: None,
            file_drop_target_path: None,
            selected_open_tool_id: None,
            _subscriptions: vec![
                workbench_subscription,
                file_search_subscription,
                input_subscription,
                history_search_subscription,
                history_branch_subscription,
                history_author_subscription,
                history_date_subscription,
                file_tree_blur_subscription,
            ],
            #[cfg(test)]
            render_count: 0,
        }
    }

    pub(crate) fn set_mode(&mut self, mode: RightRailMode, cx: &mut Context<Self>) {
        if self.projection.mode == mode {
            return;
        }
        self.projection.mode = mode;
        self.projection.revision.mode = mode;
        cx.notify();
    }

    pub fn sync_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_search_input.update(cx, |input, cx| {
            input.set_placeholder(
                locale::text("Search workspace files", "搜索工作区文件", "搜尋工作區檔案"),
                window,
                cx,
            )
        });
        self.history_search_input.update(cx, |input, cx| {
            input.set_placeholder(
                locale::text("Search commits", "搜索提交信息或代码", "搜尋提交資訊或代碼"),
                window,
                cx,
            )
        });
        let placeholder = inline_path_placeholder(self.inline_file_action.as_ref());
        self.inline_path_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        cx.notify();
    }

    fn sync_history_filter_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch_choices = git_history_branch_choices(
            self.projection.git.branches.as_ref(),
            self.projection.git.remotes.as_ref(),
        );
        let selected_branch = self.projection.git.selected_branch.clone();
        self.history_branch_select.update(cx, |select, cx| {
            select.set_items(branch_choices, window, cx);
            if let Some(selected) = selected_branch.as_ref() {
                select.set_selected_value(selected, window, cx);
            } else {
                select.set_selected_index(None, window, cx);
            }
        });

        let author_choices = git_history_author_choices(self.projection.git.authors.as_ref());
        let selected_author = self.projection.git.selected_author.clone();
        self.history_author_select.update(cx, |select, cx| {
            select.set_items(author_choices, window, cx);
            select.set_selected_value(&selected_author, window, cx);
        });

        let date = git_history_filter_date(
            self.projection.git.history_authored_after_ms,
            self.projection.git.history_authored_before_ms,
        );
        self.history_date_picker.update(cx, |picker, cx| {
            if picker.date() != date {
                picker.set_date(date, window, cx);
            }
        });
    }

    fn schedule_history_search(&mut self, cx: &mut Context<Self>) {
        self.launch_history_search(GIT_HISTORY_SEARCH_DEBOUNCE, cx);
    }

    fn start_history_search(&mut self, cx: &mut Context<Self>) {
        self.launch_history_search(Duration::ZERO, cx);
    }

    fn launch_history_search(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.history_search_generation = self.history_search_generation.wrapping_add(1);
        let generation = self.history_search_generation;
        self.history_search_task = None;
        let query = self
            .history_search_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        self.history_search_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let _ = entity.update(cx, |this, cx| {
                if this.history_search_generation != generation {
                    return;
                }
                this.history_search_task = None;
                let query = (!query.is_empty()).then_some(query.clone());
                this.update_workbench(cx, |workbench, cx| workbench.set_history_query(query, cx));
            });
        }));
    }

    fn set_history_drawer_height(&mut self, height: f32, cx: &mut Context<Self>) {
        let max_height = self.history_body_height.max(GIT_HISTORY_DRAWER_MIN_HEIGHT);
        let height = height
            .round()
            .clamp(GIT_HISTORY_DRAWER_MIN_HEIGHT, max_height);
        if (height - self.history_drawer_height).abs() < f32::EPSILON {
            return;
        }
        self.history_drawer_height = height;
        cx.notify();
    }

    fn update_history_drawer_resize(&mut self, window_y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.history_drawer_resize else {
            return;
        };
        self.set_history_drawer_height(drag.start_height + drag.start_window_y - window_y, cx);
    }

    fn finish_history_drawer_resize(&mut self, cx: &mut Context<Self>) {
        if self.history_drawer_resize.take().is_none() {
            return;
        }
        if git_history_drawer_should_close(self.history_drawer_height) {
            self.update_workbench(cx, |workbench, cx| {
                workbench.git.clear_commit_selection();
                cx.notify();
            });
        }
        cx.notify();
    }

    fn record_history_body_height(&mut self, height: f32, cx: &mut Context<Self>) {
        if height.is_finite() && height > 0.0 {
            self.history_body_height = height;
            let drawer_height = self.history_drawer_height.min(height);
            if (drawer_height - self.history_drawer_height).abs() >= f32::EPSILON {
                self.history_drawer_height = drawer_height;
                cx.notify();
            }
        }
    }

    fn set_file_search_mode(&mut self, mode: FileSearchMode, cx: &mut Context<Self>) {
        if self.file_search_mode == mode {
            return;
        }
        self.file_search_mode = mode;
        self.file_search_results.clear();
        self.file_search_collapsed_paths.clear();
        self.file_search_error = None;
        self.schedule_file_search(cx);
        cx.notify();
    }

    fn toggle_file_search_mode(&mut self, cx: &mut Context<Self>) {
        self.set_file_search_mode(self.file_search_mode.toggled(), cx);
    }

    fn toggle_file_search_option(&mut self, option: FileSearchOption, cx: &mut Context<Self>) {
        match option {
            FileSearchOption::CaseSensitive => {
                self.file_search_options.case_sensitive = !self.file_search_options.case_sensitive;
            }
            FileSearchOption::WholeWord => {
                self.file_search_options.whole_word = !self.file_search_options.whole_word;
            }
            FileSearchOption::Regex => {
                self.file_search_options.regex = !self.file_search_options.regex;
            }
        }
        self.file_search_results.clear();
        self.file_search_collapsed_paths.clear();
        self.file_search_error = None;
        self.schedule_file_search(cx);
        cx.notify();
    }

    fn clear_file_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_search_generation = self.file_search_generation.wrapping_add(1);
        self.file_search_task = None;
        self.file_search_loading = false;
        self.file_search_error = None;
        self.file_search_results.clear();
        self.file_search_collapsed_paths.clear();
        self.file_search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn schedule_file_search(&mut self, cx: &mut Context<Self>) {
        self.launch_file_search(FILE_SEARCH_DEBOUNCE, cx);
    }

    fn start_file_search(&mut self, cx: &mut Context<Self>) {
        self.launch_file_search(Duration::ZERO, cx);
    }

    fn launch_file_search(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.file_search_generation = self.file_search_generation.wrapping_add(1);
        let generation = self.file_search_generation;
        self.file_search_task = None;
        self.file_search_collapsed_paths.clear();
        let query = self.file_search_input.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.file_search_loading = false;
            self.file_search_error = None;
            self.file_search_results.clear();
            cx.notify();
            return;
        }
        let (runtime, workspace) = {
            let workbench = self.workbench.read(cx);
            (workbench.runtime.clone(), workbench.workspace.clone())
        };
        let (Some(runtime), Some(workspace)) = (runtime, workspace) else {
            self.file_search_loading = false;
            self.file_search_results.clear();
            self.file_search_error = Some(
                locale::text(
                    "Select an Agent session before searching files",
                    "请先选择 Agent 会话再搜索文件",
                    "請先選擇 Agent 會話再搜尋檔案",
                )
                .to_string(),
            );
            cx.notify();
            return;
        };
        let mode = self.file_search_mode;
        let options = self.file_search_options;
        let request = FileSearchRequest {
            workspace_id: workspace.id,
            query,
            include_content: mode == FileSearchMode::Content,
            case_sensitive: mode == FileSearchMode::Content && options.case_sensitive,
            whole_word: mode == FileSearchMode::Content && options.whole_word,
            regex: mode == FileSearchMode::Content && options.regex,
            limit: Some(FILE_SEARCH_RESULT_LIMIT),
        };
        self.file_search_loading = true;
        self.file_search_error = None;
        self.file_search_results.clear();
        cx.notify();
        self.file_search_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let runner =
                gpui_tokio::Tokio::spawn(cx, async move { runtime.files().search(&request) });
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if this.file_search_generation != generation {
                    return;
                }
                this.file_search_task = None;
                this.file_search_loading = false;
                match outcome {
                    Ok(Ok(mut results)) => {
                        results.retain(|result| match mode {
                            FileSearchMode::Name => result.line.is_none(),
                            FileSearchMode::Content => result.line.is_some(),
                        });
                        this.file_search_results = results;
                        this.file_search_error = None;
                    }
                    Ok(Err(error)) => {
                        this.file_search_results.clear();
                        this.file_search_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.file_search_results.clear();
                        this.file_search_error = Some(format!("file search task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn toggle_file_name_search_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.file_search_collapsed_paths.insert(path.clone()) {
            self.file_search_collapsed_paths.remove(&path);
        }
        cx.notify();
    }

    fn activate_file_name_search_row(
        &mut self,
        path: String,
        kind: FileEntryKind,
        has_children: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind == FileEntryKind::Directory && has_children {
            self.toggle_file_name_search_directory(path, cx);
            return;
        }
        if kind == FileEntryKind::Directory {
            self.clear_file_search(window, cx);
            let _ = self.workbench.update(cx, |workbench, cx| {
                workbench.reveal_file_search_directory(path, cx);
            });
        } else {
            let _ = self.workbench.update(cx, |workbench, cx| {
                workbench.request_preview_panel(cx);
                workbench.open_file(path, false, window, cx);
            });
        }
    }

    fn begin_inline_file_action(
        &mut self,
        action: InlineFileAction,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = inline_path_placeholder(Some(&action));
        self.inline_file_action = Some(action);
        self.inline_file_error = None;
        let selection_end = initial.len();
        self.inline_path_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value(initial, window, cx);
            input.set_selected_range(0..selection_end, cx);
        });
        self.defer_inline_path_input_focus(window, cx);
        cx.notify();
    }

    fn defer_inline_path_input_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.inline_path_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
    }

    fn cancel_inline_file_action(&mut self, cx: &mut Context<Self>) {
        self.inline_file_action = None;
        self.inline_file_error = None;
        cx.notify();
    }

    fn submit_inline_file_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.inline_file_action.clone() else {
            return;
        };
        let name = self.inline_path_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.cancel_inline_file_action(cx);
            return;
        }
        if !valid_file_name(&name) {
            self.inline_file_error = Some(
                locale::text(
                    "Enter a file name without path separators",
                    "请输入不含路径分隔符的文件名",
                    "請輸入不含路徑分隔符的檔案名稱",
                )
                .to_string(),
            );
            self.defer_inline_path_input_focus(window, cx);
            cx.notify();
            return;
        }
        let destination = match &action {
            InlineFileAction::CreateFile { parent }
            | InlineFileAction::CreateDirectory { parent } => join_relative_path(parent, &name),
            InlineFileAction::Rename { source } => {
                join_relative_path(relative_parent_path(source), &name)
            }
        };
        let duplicate = self
            .workbench
            .read(cx)
            .file_tree
            .contains_path(&destination);
        let unchanged =
            matches!(&action, InlineFileAction::Rename { source } if source == &destination);
        if duplicate && !unchanged {
            self.inline_file_error = Some(match locale::current_locale() {
                locale::ResolvedLocale::En => format!("{destination} already exists"),
                locale::ResolvedLocale::ZhCn => format!("{destination} 已存在"),
                locale::ResolvedLocale::ZhTw => format!("{destination} 已存在"),
            });
            self.defer_inline_path_input_focus(window, cx);
            cx.notify();
            return;
        }
        if unchanged {
            self.cancel_inline_file_action(cx);
            return;
        }
        self.update_workbench(cx, move |workbench, cx| match action {
            InlineFileAction::CreateFile { .. } => workbench.create_file(destination, cx),
            InlineFileAction::CreateDirectory { .. } => workbench.create_directory(destination, cx),
            InlineFileAction::Rename { source } => workbench.rename_path(source, destination, cx),
        });
        self.inline_file_action = None;
        self.inline_file_error = None;
        cx.notify();
    }

    fn on_file_tree_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.file_typeahead.is_empty() {
            self.file_typeahead.clear();
            cx.notify();
        }
    }

    fn on_file_tree_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inline_file_action.is_some() {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" if !self.file_typeahead.is_empty() => self.file_typeahead.clear(),
            "backspace" if !self.file_typeahead.is_empty() => {
                self.file_typeahead.pop();
            }
            _ => {
                let Some(value) = event.keystroke.key_char.as_deref() else {
                    return;
                };
                if value == " " || value.chars().any(char::is_control) {
                    return;
                }
                self.file_typeahead.extend(
                    value
                        .chars()
                        .take(512usize.saturating_sub(self.file_typeahead.chars().count())),
                );
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn update_workbench(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut CodeWorkbench, &mut Context<CodeWorkbench>),
    ) {
        self.workbench.update(cx, update);
    }

    fn select_and_open_workspace_tool(&mut self, tool_id: String, cx: &mut Context<Self>) {
        self.selected_open_tool_id = Some(tool_id.clone());
        self.update_workbench(cx, move |workbench, cx| match tool_id.as_str() {
            FILE_MANAGER_OPEN_TOOL_ID => workbench.reveal_in_file_manager(String::new(), cx),
            NATIVE_TERMINAL_OPEN_TOOL_ID => workbench.open_native_terminal(String::new(), cx),
            _ => workbench.open_with_tool(String::new(), tool_id, cx),
        });
        cx.notify();
    }

    fn open_workspace_integrated_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_workbench(cx, |workbench, cx| {
            workbench.request_new_preview_terminal(window.window_handle(), None, None, cx)
        });
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        let parent = self.workbench.read(cx).parent.clone();
        if let Some(parent) = parent {
            let _ = parent.update(cx, |parent, cx| parent.close_right_rail(cx));
        }
    }

    fn confirm_delete(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let workbench = self.workbench.downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let workbench = workbench.clone();
            let path = path.clone();
            dialog
                .title(locale::text(
                    "Delete workspace path?",
                    "删除工作区路径？",
                    "刪除工作區路徑？",
                ))
                .child(match locale::current_locale() {
                    locale::ResolvedLocale::En => format!(
                        "Delete {path}? Dirty editor content remains recoverable and is not silently closed."
                    ),
                    locale::ResolvedLocale::ZhCn => format!(
                        "删除 {path}？未保存的编辑器内容仍可恢复，不会被静默关闭。"
                    ),
                    locale::ResolvedLocale::ZhTw => format!(
                        "刪除 {path}？未儲存的編輯器內容仍可復原，不會被靜默關閉。"
                    ),
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .child(
                                    Button::new("cancel-file-delete")
                                        .outline()
                                        .label(locale::text("Cancel", "取消", "取消")),
                                ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-file-delete")
                                    .danger()
                                    .label(locale::text("Delete", "删除", "刪除")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = workbench.update(cx, |workbench, cx| {
                        workbench.delete_path(path.clone(), cx)
                    });
                    true
                })
        });
    }

    fn confirm_git_action(
        &mut self,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        action: impl Fn(&mut CodeWorkbench, &mut Context<CodeWorkbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workbench = self.workbench.downgrade();
        let title = title.into();
        let message = message.into();
        let action = Rc::new(action);
        window.open_dialog(cx, move |dialog, _, _| {
            let workbench = workbench.clone();
            let action = action.clone();
            dialog
                .title(title.clone())
                .child(message.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().child(
                                Button::new("cancel-git-action")
                                    .outline()
                                    .label(locale::text("Cancel", "取消", "取消")),
                            ),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-git-action")
                                    .danger()
                                    .label(locale::text("Continue", "继续", "繼續")),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = workbench.update(cx, |workbench, cx| action(workbench, cx));
                    true
                })
        });
    }

    fn begin_inline_create(
        &mut self,
        parent: String,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let expand_parent = parent.clone();
        self.update_workbench(cx, |workbench, cx| {
            if !workbench.file_tree.is_expanded(&expand_parent) {
                workbench.toggle_directory(expand_parent.clone(), cx);
            }
        });
        let action = if directory {
            InlineFileAction::CreateDirectory { parent }
        } else {
            InlineFileAction::CreateFile { parent }
        };
        self.begin_inline_file_action(action, String::new(), window, cx);
    }

    fn set_file_clipboard(
        &mut self,
        operation: FileClipboardOperation,
        path: String,
        name: String,
        kind: FileEntryKind,
        cx: &mut Context<Self>,
    ) {
        if path.is_empty() {
            return;
        }
        self.file_clipboard = Some(FileClipboardEntry {
            operation,
            path,
            name,
            kind,
        });
        cx.notify();
    }

    fn paste_file_clipboard(&mut self, target_directory: String, cx: &mut Context<Self>) {
        let Some(clipboard) = self.file_clipboard.clone() else {
            return;
        };
        if clipboard.path == target_directory
            || path_is_equal_or_descendant(&target_directory, &clipboard.path)
        {
            return;
        }
        let destination = match clipboard.operation {
            FileClipboardOperation::Cut => join_relative_path(&target_directory, &clipboard.name),
            FileClipboardOperation::Copy => {
                let workbench = self.workbench.read(cx);
                unique_copy_destination(&target_directory, &clipboard.name, |candidate| {
                    workbench.file_tree.contains_path(candidate)
                })
            }
        };
        if destination == clipboard.path {
            return;
        }
        self.update_workbench(cx, |workbench, cx| match clipboard.operation {
            FileClipboardOperation::Cut => {
                workbench.rename_path(clipboard.path.clone(), destination.clone(), cx)
            }
            FileClipboardOperation::Copy => workbench.copy_path(
                clipboard.path.clone(),
                destination.clone(),
                clipboard.kind == FileEntryKind::Directory,
                cx,
            ),
        });
        if clipboard.operation == FileClipboardOperation::Cut {
            self.file_clipboard = None;
        }
        cx.notify();
    }

    fn begin_file_drag(&mut self, path: String, cx: &mut Context<Self>) {
        self.file_drag_path = Some(path);
        self.file_drop_target_path = None;
        cx.notify();
    }

    fn update_file_drop_target(
        &mut self,
        drag: &FileRowDrag,
        target_directory: String,
        cx: &mut Context<Self>,
    ) {
        let destination = join_relative_path(&target_directory, drag.name.as_ref());
        let valid = drag.path != target_directory
            && destination != drag.path
            && !path_is_equal_or_descendant(&target_directory, &drag.path)
            && !self
                .workbench
                .read(cx)
                .file_tree
                .contains_path(&destination);
        let next = valid.then_some(target_directory);
        if self.file_drop_target_path != next {
            self.file_drop_target_path = next;
            cx.notify();
        }
    }

    fn finish_file_drag(&mut self, cx: &mut Context<Self>) {
        let changed =
            self.file_drag_path.take().is_some() || self.file_drop_target_path.take().is_some();
        if changed {
            cx.notify();
        }
    }

    fn auto_scroll_file_tree(
        &mut self,
        event: &DragMoveEvent<FileRowDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let edge = px(40.0);
        let delta = if event.event.position.y < event.bounds.top() + edge {
            px(12.0)
        } else if event.event.position.y > event.bounds.bottom() - edge {
            px(-12.0)
        } else {
            return;
        };
        let scroll = self.workbench.read(cx).file_scroll.clone();
        let handle = scroll.0.borrow().base_handle.clone();
        let offset = handle.offset();
        handle.set_offset(point(offset.x, offset.y + delta));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn build_file_context_menu(
        menu: PopupMenu,
        target: FileContextMenuTarget,
        workspace_root: Option<PathBuf>,
        clipboard_available: bool,
        mutation_pending: bool,
        view: WeakEntity<Self>,
        controller: WeakEntity<CodeWorkbench>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let is_root = target.path.is_empty();
        let editor_available = file_can_open_in_editor(&target.path, target.kind);
        let tools = available_external_tools();
        let absolute_path = workspace_root.map(|root| {
            if is_root {
                root.to_string_lossy().to_string()
            } else {
                root.join(&target.path).to_string_lossy().to_string()
            }
        });

        let new_file_view = view.clone();
        let new_folder_view = view.clone();
        let cut_view = view.clone();
        let copy_view = view.clone();
        let paste_view = view.clone();
        let rename_view = view.clone();
        let delete_view = view;
        let new_file_parent = target.target_directory.clone();
        let new_folder_parent = target.target_directory.clone();
        let paste_directory = target.target_directory.clone();
        let cut_target = target.clone();
        let copy_target = target.clone();
        let rename_source = target.path.clone();
        let rename_name = target.name.clone();
        let delete_path = target.path.clone();
        let relative_path = target.path.clone();
        let file_name = target.name.clone();
        let delete_label = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Delete {}", target.name),
            locale::ResolvedLocale::ZhCn => format!("删除 {}", target.name),
            locale::ResolvedLocale::ZhTw => format!("刪除 {}", target.name),
        };
        let mut menu = menu
            .min_w(px(224.0))
            .max_w(px(224.0))
            .item(
                PopupMenuItem::new(locale::text("New File", "新建文件", "新建檔案"))
                    .icon(Icon::default().path("icons/vibex/file-plus.svg"))
                    .disabled(mutation_pending)
                    .on_click(move |_, window, cx| {
                        let _ = new_file_view.update(cx, |this, cx| {
                            this.begin_inline_create(new_file_parent.clone(), false, window, cx)
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(locale::text("New Folder", "新建文件夹", "新建資料夾"))
                    .icon(Icon::default().path("icons/vibex/folder-plus.svg"))
                    .disabled(mutation_pending)
                    .on_click(move |_, window, cx| {
                        let _ = new_folder_view.update(cx, |this, cx| {
                            this.begin_inline_create(new_folder_parent.clone(), true, window, cx)
                        });
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(locale::text("Cut", "剪切", "剪下"))
                    .icon(Icon::default().path("icons/vibex/scissors.svg"))
                    .disabled(is_root || mutation_pending)
                    .on_click(move |_, _, cx| {
                        let _ = cut_view.update(cx, |this, cx| {
                            this.set_file_clipboard(
                                FileClipboardOperation::Cut,
                                cut_target.path.clone(),
                                cut_target.name.clone(),
                                cut_target.kind,
                                cx,
                            )
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(locale::text("Copy", "复制", "複製"))
                    .icon(IconName::Copy)
                    .disabled(is_root)
                    .on_click(move |_, _, cx| {
                        let _ = copy_view.update(cx, |this, cx| {
                            this.set_file_clipboard(
                                FileClipboardOperation::Copy,
                                copy_target.path.clone(),
                                copy_target.name.clone(),
                                copy_target.kind,
                                cx,
                            )
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(locale::text("Paste", "粘贴", "貼上"))
                    .icon(Icon::default().path("icons/vibex/clipboard-paste.svg"))
                    .disabled(!clipboard_available || mutation_pending)
                    .on_click(move |_, _, cx| {
                        let _ = paste_view.update(cx, |this, cx| {
                            this.paste_file_clipboard(paste_directory.clone(), cx)
                        });
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(locale::text(
                    "Copy Relative Path",
                    "复制相对位置",
                    "複製相對位置",
                ))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(relative_path.clone()));
                }),
            )
            .item(
                PopupMenuItem::new(locale::text(
                    "Copy Absolute Path",
                    "复制绝对位置",
                    "複製絕對位置",
                ))
                .disabled(absolute_path.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(path) = absolute_path.clone() {
                        cx.write_to_clipboard(ClipboardItem::new_string(path));
                    }
                }),
            )
            .item(
                PopupMenuItem::new(locale::text("Copy File Name", "复制文件名", "複製檔案名"))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(file_name.clone()));
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(locale::text("Rename", "重命名", "重新命名"))
                    .disabled(is_root || mutation_pending)
                    .on_click(move |_, window, cx| {
                        let source = rename_source.clone();
                        let _ = rename_view.update(cx, |this, cx| {
                            this.begin_inline_file_action(
                                InlineFileAction::Rename {
                                    source: source.clone(),
                                },
                                rename_name.clone(),
                                window,
                                cx,
                            )
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(delete_label)
                    .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                    .disabled(is_root || mutation_pending)
                    .on_click(move |_, window, cx| {
                        let path = delete_path.clone();
                        let _ = delete_view
                            .update(cx, |this, cx| this.confirm_delete(path, window, cx));
                    }),
            )
            .separator();

        if target.directory_error {
            let retry_controller = controller.clone();
            let retry_path = target.path.clone();
            menu = menu.item(
                PopupMenuItem::new(locale::text(
                    "Retry loading directory",
                    "重新加载目录",
                    "重新載入目錄",
                ))
                .icon(IconName::Replace)
                .on_click(move |_, _, cx| {
                    let _ = retry_controller.update(cx, |workbench, cx| {
                        workbench.retry_directory(retry_path.clone(), cx)
                    });
                }),
            );
        }

        let editor_controller = controller.clone();
        let default_controller = controller.clone();
        let integrated_controller = controller.clone();
        let native_controller = controller.clone();
        let reveal_controller = controller.clone();
        let editor_path = target.path.clone();
        let default_path = target.path.clone();
        let integrated_path = target.path.clone();
        let native_path = target.path.clone();
        let reveal_path = target.path.clone();
        let submenu_controller = controller;
        let submenu_path = target.path;
        let target_kind = target.kind;
        menu.submenu_with_icon(
            Some(Icon::new(IconName::ExternalLink)),
            "Open In",
            window,
            cx,
            move |mut submenu, _, _| {
                submenu = submenu.min_w(px(224.0)).max_w(px(224.0));
                let editor_controller = editor_controller.clone();
                let default_controller = default_controller.clone();
                let integrated_controller = integrated_controller.clone();
                let native_controller = native_controller.clone();
                let reveal_controller = reveal_controller.clone();
                let editor_path = editor_path.clone();
                let default_path = default_path.clone();
                let integrated_path = integrated_path.clone();
                let native_path = native_path.clone();
                let reveal_path = reveal_path.clone();
                submenu = submenu
                    .item(
                        PopupMenuItem::new(locale::text("Editor", "编辑器", "編輯器"))
                            .icon(Icon::default().path("icons/vibex/file-text.svg"))
                            .disabled(!editor_available)
                            .on_click(move |_, window, cx| {
                                let _ = editor_controller.update(cx, |workbench, cx| {
                                    workbench.request_preview_panel(cx);
                                    workbench.open_file(editor_path.clone(), false, window, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Default App", "默认应用", "預設應用程式"))
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| {
                                let _ = default_controller.update(cx, |workbench, cx| {
                                    workbench.open_default_app(default_path.clone(), cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Terminal", "终端", "終端"))
                            .icon(IconName::SquareTerminal)
                            .on_click(move |_, window, cx| {
                                let _ = integrated_controller.update(cx, |workbench, cx| {
                                    workbench.request_new_preview_terminal(
                                        window.window_handle(),
                                        None,
                                        Some((integrated_path.clone(), target_kind)),
                                        cx,
                                    )
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text("Native Terminal", "本机终端", "本機終端"))
                            .icon(IconName::SquareTerminal)
                            .on_click(move |_, _, cx| {
                                let _ = native_controller.update(cx, |workbench, cx| {
                                    workbench.open_native_terminal(native_path.clone(), cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(locale::text(
                            "File Manager",
                            "文件管理器",
                            "檔案管理器",
                        ))
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            let _ = reveal_controller.update(cx, |workbench, cx| {
                                workbench.reveal_in_file_manager(reveal_path.clone(), cx)
                            });
                        }),
                    )
                    .separator()
                    .item(open_tool_menu_section_label(locale::text(
                        "Tools", "工具", "工具",
                    )));
                for tool in tools.clone() {
                    let tool_controller = submenu_controller.clone();
                    let tool_path = submenu_path.clone();
                    let tool_id = tool.id.to_string();
                    submenu = submenu.item(open_tool_menu_item(tool.id, tool.label).on_click(
                        move |_, _, cx| {
                            let _ = tool_controller.update(cx, |workbench, cx| {
                                workbench.open_with_tool(tool_path.clone(), tool_id.clone(), cx)
                            });
                        },
                    ));
                }
                if tools.is_empty() {
                    submenu = submenu.item(
                        PopupMenuItem::new(locale::text(
                            "No installed IDE or project tool detected",
                            "未探测到已安装的 IDE 或项目工具",
                            "未探測到已安裝的 IDE 或專案工具",
                        ))
                        .disabled(true),
                    );
                }
                submenu
            },
        )
    }

    fn render_file_search_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query_present = !self.file_search_input.read(cx).value().is_empty();
        let mode = self.file_search_mode;
        let options = self.file_search_options;
        v_flex()
            .w_full()
            .flex_none()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h_8()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().input)
                    .bg(cx.theme().background)
                    .px_2()
                    .child(
                        Icon::new(IconName::Search)
                            .xsmall()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div().min_w_0().flex_1().child(
                            Input::new(&self.file_search_input)
                                .small()
                                .appearance(false),
                        ),
                    )
                    .when(self.file_search_loading, |this| {
                        this.child(
                            Icon::new(IconName::Loader)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .when(query_present, |this| {
                        this.child(
                            Button::new("clear-file-search")
                                .xsmall()
                                .ghost()
                                .compact()
                                .icon(IconName::Close)
                                .tooltip(locale::text("Clear search", "清除搜索", "清除搜尋"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_file_search(window, cx)
                                })),
                        )
                    })
                    .when(mode == FileSearchMode::Content, |this| {
                        this.child(
                            Button::new("file-search-case-sensitive")
                                .xsmall()
                                .ghost()
                                .compact()
                                .label("Aa")
                                .selected(options.case_sensitive)
                                .tooltip(locale::text("Match case", "区分大小写", "區分大小寫"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_file_search_option(
                                        FileSearchOption::CaseSensitive,
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            Button::new("file-search-whole-word")
                                .xsmall()
                                .ghost()
                                .compact()
                                .label("ab")
                                .selected(options.whole_word)
                                .tooltip(locale::text(
                                    "Match whole word",
                                    "匹配整个词",
                                    "符合整個詞",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_file_search_option(FileSearchOption::WholeWord, cx)
                                })),
                        )
                        .child(
                            Button::new("file-search-regex")
                                .xsmall()
                                .ghost()
                                .compact()
                                .label(".*")
                                .selected(options.regex)
                                .tooltip(locale::text(
                                    "Use regular expression",
                                    "使用正则表达式",
                                    "使用正規表示式",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_file_search_option(FileSearchOption::Regex, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("toggle-file-search-mode")
                            .xsmall()
                            .ghost()
                            .compact()
                            .label(mode.title())
                            .tooltip(mode.toggle_label())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.toggle_file_search_mode(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_file_search_name_result(
        &mut self,
        index: usize,
        row: FileNameSearchTreeRow,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = row.path.clone();
        let kind = row.kind;
        let descriptor = file_icon_descriptor(&row.name, row.kind);
        let has_children = row.has_children;
        let expanded = row.expanded;
        let click_path = path.clone();
        let keyboard_path = path.clone();
        let disclosure = if has_children {
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(px(14.0))
            .text_color(cx.theme().muted_foreground)
            .into_any_element()
        } else {
            div().w(px(14.0)).flex_none().into_any_element()
        };
        h_flex()
            .id(format!("file-name-search-result:{index}:{}", row.path))
            .relative()
            .h(px(FILE_ROW_HEIGHT))
            .min_w_full()
            .flex_none()
            .items_center()
            .px_1()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .focusable()
            .tab_index(0)
            .role(Role::Button)
            .aria_label(row.path.clone())
            .when(has_children, |this| this.aria_expanded(expanded))
            .children(file_tree_guides(row.depth, cx))
            .child(div().w(px(row.depth as f32 * FILE_TREE_INDENT)).flex_none())
            .child(disclosure)
            .child(file_tree_icon(descriptor.kind, false, cx))
            .child(
                div()
                    .flex_none()
                    .ml_1p5()
                    .whitespace_nowrap()
                    .child(render_file_name_match(
                        &row.name,
                        if row.matched { query } else { "" },
                        cx.theme().sidebar_foreground,
                        cx,
                    )),
            )
            .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.45)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_file_name_search_row(
                    click_path.clone(),
                    kind,
                    has_children,
                    window,
                    cx,
                );
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                    this.activate_file_name_search_row(
                        keyboard_path.clone(),
                        kind,
                        has_children,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    fn render_file_search_content_header(
        &self,
        result: &FileSearchResult,
        count: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let descriptor = file_icon_descriptor(&result.name, result.kind);
        h_flex()
            .h_8()
            .min_w_full()
            .flex_none()
            .gap_2()
            .px_2()
            .pt_1()
            .child(file_tree_icon(descriptor.kind, false, cx))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_sm()
                    .font_medium()
                    .child(result.path.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .px_1p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    fn render_file_search_content_result(
        &mut self,
        index: usize,
        result: FileSearchResult,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = result.path.clone();
        let line = result.line.unwrap_or(1);
        let snippet = result.snippet.unwrap_or_default();
        let snippet_match_range = result
            .snippet_match_start
            .zip(result.snippet_match_end)
            .map(|(start, end)| start as usize..end as usize);
        let reveal_query = query.to_string();
        let match_start = result.match_start;
        let match_end = result.match_end;
        let controller = self.workbench.downgrade();
        h_flex()
            .id(format!("file-content-search-result:{index}:{path}:{line}"))
            .min_h(px(30.0))
            .min_w_full()
            .flex_none()
            .items_start()
            .gap_2()
            .pl(px(28.0))
            .pr_2()
            .py_1()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .aria_label(format!("{}:{line}", result.path))
            .child(
                div()
                    .w(px(32.0))
                    .flex_none()
                    .text_right()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(line.to_string()),
            )
            .child(div().flex_none().whitespace_nowrap().text_xs().child(
                render_highlighted_text_range(
                    &snippet,
                    snippet_match_range,
                    cx.theme().sidebar_foreground,
                    cx,
                ),
            ))
            .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.45)))
            .on_click(move |_, window, cx| {
                let _ = controller.update(cx, |workbench, cx| {
                    workbench.open_file_search_result(
                        FileSearchReveal {
                            path: path.clone(),
                            query: reveal_query.clone(),
                            line,
                            match_start,
                            match_end,
                        },
                        window,
                        cx,
                    );
                });
            })
            .into_any_element()
    }

    fn render_file_search_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.file_search_input.read(cx).value().trim().to_string();
        if self.file_search_loading {
            return rail_empty(locale::text("Searching", "正在搜索", "正在搜尋"), cx);
        }
        if let Some(error) = self.file_search_error.clone() {
            return rail_empty(locale::localize_error_message(&error), cx);
        }
        if self.file_search_results.is_empty() {
            return rail_empty(
                match self.file_search_mode {
                    FileSearchMode::Name => {
                        locale::text("No matching items", "没有匹配项", "沒有符合項目")
                    }
                    FileSearchMode::Content => {
                        locale::text("No matching files", "没有匹配的文件", "沒有匹配的檔案")
                    }
                },
                cx,
            );
        }
        let results = self.file_search_results.clone();
        let file_count = results
            .iter()
            .map(|result| result.path.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let summary = match (self.file_search_mode, locale::current_locale()) {
            (FileSearchMode::Name, locale::ResolvedLocale::En) => {
                format!("{} matching items", results.len())
            }
            (FileSearchMode::Name, locale::ResolvedLocale::ZhCn) => {
                format!("{} 个匹配项", results.len())
            }
            (FileSearchMode::Name, locale::ResolvedLocale::ZhTw) => {
                format!("{} 個符合項目", results.len())
            }
            (FileSearchMode::Content, locale::ResolvedLocale::En) => {
                format!("{} results in {} files", results.len(), file_count)
            }
            (FileSearchMode::Content, locale::ResolvedLocale::ZhCn) => {
                format!("{} 个结果，位于 {} 个文件中", results.len(), file_count)
            }
            (FileSearchMode::Content, locale::ResolvedLocale::ZhTw) => {
                format!("{} 個結果，位於 {} 個檔案中", results.len(), file_count)
            }
        };
        let mut rows = Vec::new();
        match self.file_search_mode {
            FileSearchMode::Name => {
                let tree_rows =
                    file_name_search_tree_rows(&results, &self.file_search_collapsed_paths);
                for (index, row) in tree_rows.into_iter().enumerate() {
                    rows.push(self.render_file_search_name_result(index, row, &query, cx));
                }
            }
            FileSearchMode::Content => {
                let mut counts = BTreeMap::new();
                for result in &results {
                    *counts.entry(result.path.clone()).or_insert(0_usize) += 1;
                }
                let mut previous_path = None;
                for (index, result) in results.into_iter().enumerate() {
                    if previous_path.as_deref() != Some(result.path.as_str()) {
                        rows.push(self.render_file_search_content_header(
                            &result,
                            counts.get(&result.path).copied().unwrap_or(1),
                            cx,
                        ));
                        previous_path = Some(result.path.clone());
                    }
                    rows.push(self.render_file_search_content_result(index, result, &query, cx));
                }
            }
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_scrollbar()
                    .px_1()
                    .py_1()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_files(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.projection.files.workspace_available {
            return rail_empty(
                locale::text(
                    "Select an Agent session to load workspace files",
                    "选择 Agent 会话以加载工作区文件",
                    "選擇 Agent 會話以載入工作區檔案",
                ),
                cx,
            );
        }
        let rows = self.projection.files.rows.clone();
        let row_count = rows.len();
        let loading = self.projection.files.loading;
        let mutation_pending = self.projection.files.mutation_pending;
        let create_insertion = self.inline_file_action.as_ref().and_then(|action| {
            let parent = match action {
                InlineFileAction::CreateFile { parent }
                | InlineFileAction::CreateDirectory { parent } => parent,
                InlineFileAction::Rename { .. } => return None,
            };
            let index = self.projection.files.row_position(parent)?;
            let depth = rows.get(index).map(|row| row.depth.saturating_add(1))?;
            Some((index.saturating_add(1), depth))
        });
        let item_count = row_count.saturating_add(usize::from(create_insertion.is_some()));
        let widest_row = rows
            .iter()
            .enumerate()
            .max_by_key(|(_, row)| file_tree_row_width_score(row));
        let mut measurement_index = widest_row.map(|(index, _)| index).unwrap_or_default();
        let measurement_score = widest_row
            .map(|(_, row)| file_tree_row_width_score(row))
            .unwrap_or_default();
        if let Some((insert_index, depth)) = create_insertion {
            if insert_index <= measurement_index {
                measurement_index = measurement_index.saturating_add(1);
            }
            let inline_score = depth.saturating_mul(20).saturating_add(280);
            if inline_score > measurement_score {
                measurement_index = insert_index;
            }
        }
        measurement_index = measurement_index.min(item_count.saturating_sub(1));
        let blank_context_view = cx.weak_entity();
        let typeahead = self.file_typeahead.clone();
        let inline_error = self.inline_file_error.clone();
        let workspace_root = self.projection.files.workspace_root.clone();
        let root_name = self.projection.files.root_name.clone();
        let scroll = self.projection.files.scroll.clone();
        let clipboard_available = self.file_clipboard.is_some();
        let root_controller = self.workbench.downgrade();
        let search_active = !self.file_search_input.read(cx).value().trim().is_empty();
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().sidebar.opacity(0.75))
            .child(self.render_file_search_controls(cx))
            .child(if search_active {
                self.render_file_search_results(cx)
            } else {
                div()
                    .id("code-workbench-file-tree")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .py_1()
                    .focusable()
                    .tab_index(0)
                    .track_focus(&self.file_tree_focus)
                    .on_key_down(cx.listener(Self::on_file_tree_key_down))
                    .on_drag_move(cx.listener(Self::auto_scroll_file_tree))
                    .child(if item_count == 0 && !loading {
                        rail_empty(
                            locale::text(
                                "No workspace files",
                                "工作区中没有文件",
                                "工作區中沒有檔案",
                            ),
                            cx,
                        )
                    } else {
                        uniform_list(
                            "code-workbench-file-rows",
                            item_count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                let range = bounded_uniform_range(
                                    range,
                                    item_count,
                                    CODE_WORKBENCH_MAX_EAGER_ROWS,
                                );
                                range
                                    .filter_map(|index| {
                                        if create_insertion
                                            .is_some_and(|(insert_index, _)| index == insert_index)
                                        {
                                            let depth = create_insertion
                                                .map(|(_, depth)| depth)
                                                .unwrap_or_default();
                                            return Some(
                                                this.render_inline_file_row(depth, None, cx),
                                            );
                                        }
                                        let row_index = if create_insertion
                                            .is_some_and(|(insert_index, _)| index > insert_index)
                                        {
                                            index.saturating_sub(1)
                                        } else {
                                            index
                                        };
                                        let row =
                                            this.projection.files.rows.get(row_index).cloned()?;
                                        Some(this.render_file_row(row, cx))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .with_width_from_item(Some(measurement_index))
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .track_scroll(&scroll)
                        .size_full()
                        .into_any_element()
                    })
                    .when(!typeahead.is_empty(), |this| {
                        let label = match locale::current_locale() {
                            locale::ResolvedLocale::En => format!("Matching \"{typeahead}\""),
                            locale::ResolvedLocale::ZhCn => format!("匹配“{typeahead}”"),
                            locale::ResolvedLocale::ZhTw => format!("匹配「{typeahead}」"),
                        };
                        this.child(
                            div()
                                .absolute()
                                .top_2()
                                .left_2()
                                .right_2()
                                .flex()
                                .justify_end()
                                .child(
                                    div()
                                        .min_w_0()
                                        .max_w_full()
                                        .whitespace_normal()
                                        .px_2()
                                        .py_1()
                                        .rounded(cx.theme().radius_lg)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .bg(cx.theme().popover)
                                        .text_xs()
                                        .font_medium()
                                        .text_right()
                                        .text_color(cx.theme().popover_foreground)
                                        .child(label),
                                ),
                        )
                    })
                    .when_some(inline_error, |this, error| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(40.0))
                                .right_2()
                                .max_w(px(240.0))
                                .px_2()
                                .py_1()
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(cx.theme().danger.opacity(0.35))
                                .bg(cx.theme().popover)
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    })
                    .context_menu(move |menu, window, cx| {
                        Self::build_file_context_menu(
                            menu,
                            FileContextMenuTarget {
                                path: String::new(),
                                name: root_name.clone(),
                                kind: FileEntryKind::Directory,
                                target_directory: String::new(),
                                directory_error: false,
                            },
                            workspace_root.clone(),
                            clipboard_available,
                            mutation_pending,
                            blank_context_view.clone(),
                            root_controller.clone(),
                            window,
                            cx,
                        )
                    })
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_inline_path_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("inline-file-tree-name-editor")
            .h(px(24.0))
            .w(px(224.0))
            .min_w_0()
            .flex_none()
            .child(
                Input::new(&self.inline_path_input)
                    .small()
                    .appearance(false),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_inline_file_action(cx);
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    fn render_inline_file_row(
        &mut self,
        depth: usize,
        rename_icon: Option<FileIconKind>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(action) = self.inline_file_action.clone() else {
            return div().h(px(FILE_ROW_HEIGHT)).into_any_element();
        };
        let icon_kind = rename_icon.unwrap_or({
            if matches!(action, InlineFileAction::CreateDirectory { .. }) {
                FileIconKind::Directory
            } else {
                FileIconKind::File
            }
        });
        h_flex()
            .id("inline-file-tree-editor")
            .relative()
            .h(px(FILE_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .items_center()
            .px_2()
            .children(file_tree_guides(depth, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .pl(px(depth as f32 * FILE_TREE_INDENT))
                    .child(file_tree_icon(icon_kind, false, cx))
                    .child(self.render_inline_path_editor(cx)),
            )
            .into_any_element()
    }

    fn render_file_row(&mut self, row: FileExplorerRow, cx: &mut Context<Self>) -> AnyElement {
        let path = row.path.clone();
        let row_kind = row.kind;
        let is_directory = row_kind == FileEntryKind::Directory;
        let is_root = path.is_empty();
        let path_chain = if row.path_chain.is_empty() {
            vec![path.clone()]
        } else {
            row.path_chain.clone()
        };
        let segments = if row.segments.is_empty() {
            vec![vibex_desktop_model::FileTreeSegment {
                path: path.clone(),
                name: row.name.clone(),
            }]
        } else {
            row.segments.clone()
        };
        let rename_source = self
            .inline_file_action
            .as_ref()
            .and_then(|action| match action {
                InlineFileAction::Rename { source } if path_chain.contains(source) => {
                    Some(source.clone())
                }
                _ => None,
            });
        let row_rename_active = rename_source.is_some();
        if row_rename_active && !is_directory {
            return self.render_inline_file_row(row.depth, Some(row.icon.kind), cx);
        }
        let directory_loading = row.load_state == FileTreeLoadState::Loading;
        let directory_error = matches!(row.load_state, FileTreeLoadState::Error { .. });
        let typeahead = self.file_typeahead.clone();
        let typeahead_active = segments
            .iter()
            .any(|segment| file_name_match_range(&segment.name, &typeahead).is_some());
        let selected_directory = self.projection.files.selected_directory_path.clone();
        let text_color = file_tree_text_color(&row, cx);
        let row_view = cx.weak_entity();
        let controller = self.workbench.downgrade();
        let click_controller = controller.clone();
        let keyboard_controller = controller.clone();
        let drop_controller = controller.clone();
        let focus = self.file_tree_focus.clone();
        let click_path = path.clone();
        let click_chain = path_chain.clone();
        let keyboard_path = path.clone();
        let keyboard_chain = path_chain.clone();
        let drop_directory = if is_directory {
            path.clone()
        } else {
            relative_parent_path(&path).to_string()
        };
        let drag_path = if is_directory && path_chain.len() > 1 {
            path_chain.first().cloned().unwrap_or_else(|| path.clone())
        } else {
            path.clone()
        };
        let drag_name = segments
            .iter()
            .find(|segment| segment.path == drag_path)
            .map(|segment| segment.name.clone())
            .unwrap_or_else(|| row.name.clone());
        let drag = FileRowDrag {
            path: drag_path,
            name: drag_name.into(),
            kind: row_kind,
        };
        let active_drag_path = cx
            .has_active_drag()
            .then(|| self.file_drag_path.clone())
            .flatten();
        let active_drop_target = cx
            .has_active_drag()
            .then(|| self.file_drop_target_path.clone())
            .flatten();
        let row_dragging = active_drag_path.as_deref() == Some(drag.path.as_str());
        let row_drop_active = is_directory && active_drop_target.as_deref() == Some(path.as_str());
        let row_drop_scope_active = active_drop_target.as_deref().is_some_and(|target| {
            !path_chain.iter().any(|candidate| candidate == target)
                && self.projection.files.is_expanded(target)
                && path_chain
                    .iter()
                    .any(|candidate| path_is_equal_or_descendant(candidate, target))
        });

        let mut names = Vec::new();
        if is_directory {
            let has_chain = segments.len() > 1;
            for (index, segment) in segments.iter().enumerate() {
                if index > 0 {
                    names.push(
                        div()
                            .mx(px(2.0))
                            .text_color(cx.theme().sidebar_foreground.opacity(0.4))
                            .child("/")
                            .into_any_element(),
                    );
                }
                if rename_source.as_deref() == Some(segment.path.as_str()) {
                    names.push(self.render_inline_path_editor(cx));
                    continue;
                }
                let segment_path = segment.path.clone();
                let segment_chain = path_chain.clone();
                let segment_controller = controller.clone();
                let segment_keyboard_controller = controller.clone();
                let segment_drop_controller = controller.clone();
                let segment_drag_view = row_view.clone();
                let segment_drop_view = row_view.clone();
                let segment_keyboard_path = segment.path.clone();
                let segment_keyboard_chain = path_chain.clone();
                let segment_drop_path = segment.path.clone();
                let segment_move_path = segment.path.clone();
                let segment_drag = FileRowDrag {
                    path: segment.path.clone(),
                    name: segment.name.clone().into(),
                    kind: FileEntryKind::Directory,
                };
                let segment_selected = selected_directory.as_deref() == Some(&segment.path);
                let segment_dragging = active_drag_path.as_deref() == Some(segment.path.as_str());
                let segment_drop_active =
                    active_drop_target.as_deref() == Some(segment.path.as_str());
                let mut element = div()
                    .id(format!("file-tree-segment:{}", segment.path))
                    .h(px(24.0))
                    .flex_none()
                    .items_center()
                    .mx(px(-2.0))
                    .px_1()
                    .rounded(cx.theme().radius)
                    .text_color(text_color)
                    .child(render_file_name_match(
                        &segment.name,
                        &typeahead,
                        text_color,
                        cx,
                    ));
                if has_chain {
                    element = element
                        .focusable()
                        .tab_index(0)
                        .when(segment_selected, |this| {
                            this.font_semibold()
                                .text_color(cx.theme().foreground)
                                .text_decoration_1()
                                .text_decoration_color(cx.theme().primary.opacity(0.7))
                        })
                        .when(segment_dragging, |this| this.opacity(0.55))
                        .when(segment_drop_active, |this| {
                            this.bg(cx.theme().primary.opacity(0.12))
                        })
                        .hover(|style| {
                            style
                                .text_color(cx.theme().foreground)
                                .text_decoration_1()
                                .text_decoration_color(cx.theme().primary.opacity(0.65))
                        })
                        .on_drag(segment_drag, move |drag, _, _, cx| {
                            cx.stop_propagation();
                            let _ = segment_drag_view
                                .update(cx, |this, cx| this.begin_file_drag(drag.path.clone(), cx));
                            cx.new(|_| drag.clone())
                        })
                        .on_drag_move(cx.listener(
                            move |this, event: &DragMoveEvent<FileRowDrag>, _, cx| {
                                let drag = event.drag(cx).clone();
                                this.update_file_drop_target(&drag, segment_move_path.clone(), cx);
                                cx.stop_propagation();
                            },
                        ))
                        .on_drop(move |drag: &FileRowDrag, _, cx| {
                            let _ =
                                segment_drop_view.update(cx, |this, cx| this.finish_file_drag(cx));
                            if drag.path == segment_drop_path
                                || path_is_equal_or_descendant(&segment_drop_path, &drag.path)
                            {
                                cx.stop_propagation();
                                return;
                            }
                            let destination =
                                join_relative_path(&segment_drop_path, drag.name.as_ref());
                            let _ = segment_drop_controller.update(cx, |workbench, cx| {
                                if !workbench.file_tree.contains_path(&destination) {
                                    workbench.rename_path(drag.path.clone(), destination, cx);
                                }
                            });
                            cx.stop_propagation();
                        })
                        .on_click(move |_, _, cx| {
                            let _ = segment_controller.update(cx, |workbench, cx| {
                                workbench.select_directory_segment(
                                    segment_path.clone(),
                                    segment_chain.clone(),
                                    cx,
                                )
                            });
                            cx.stop_propagation();
                        })
                        .on_key_down(move |event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key != "enter" && event.keystroke.key != "space" {
                                return;
                            }
                            let _ = segment_keyboard_controller.update(cx, |workbench, cx| {
                                workbench.select_directory_segment(
                                    segment_keyboard_path.clone(),
                                    segment_keyboard_chain.clone(),
                                    cx,
                                )
                            });
                            cx.stop_propagation();
                        });
                }
                names.push(element.into_any_element());
            }
        } else {
            names.push(render_file_name_match(
                &row.name, &typeahead, text_color, cx,
            ));
        }

        let context_path = path.clone();
        let context_name = row.name.clone();
        let context_target_directory = drop_directory.clone();
        let context_controller = controller.clone();
        let context_view = row_view.clone();
        let clipboard_available = self.file_clipboard.is_some();
        let mutation_pending = self.projection.files.mutation_pending;
        let workspace_root = self.projection.files.workspace_root.clone();
        let row_drag_view = row_view.clone();
        let row_drop_view = row_view.clone();
        let row_move_directory = drop_directory.clone();

        h_flex()
            .id(row.id.clone())
            .relative()
            .h(px(FILE_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .min_w_0()
            .items_center()
            .px_2()
            .border_1()
            .border_color(if row_drop_active {
                cx.theme().primary.opacity(0.40)
            } else if row.selected {
                cx.theme().primary.opacity(0.25)
            } else if typeahead_active {
                cx.theme().primary.opacity(0.30)
            } else {
                cx.theme().transparent
            })
            .bg(if row_drop_active {
                cx.theme().primary.opacity(0.12)
            } else if row.selected {
                cx.theme().primary.opacity(0.07)
            } else if row_drop_scope_active {
                cx.theme().primary.opacity(0.055)
            } else if typeahead_active {
                cx.theme().primary.opacity(0.09)
            } else {
                cx.theme().transparent
            })
            .when(
                !row_drop_active && !row.selected && !row_drop_scope_active && !typeahead_active,
                |this| this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.35))),
            )
            .when(row_dragging, |this| this.opacity(0.55))
            .focusable()
            .tab_index(0)
            .aria_label(row.accessible_name.clone())
            .children(file_tree_guides(row.depth, cx))
            .when(!is_root && !row_rename_active, |this| {
                this.on_drag(drag, move |drag, _, _, cx| {
                    let _ = row_drag_view
                        .update(cx, |this, cx| this.begin_file_drag(drag.path.clone(), cx));
                    cx.new(|_| drag.clone())
                })
            })
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<FileRowDrag>, _, cx| {
                    let drag = event.drag(cx).clone();
                    this.update_file_drop_target(&drag, row_move_directory.clone(), cx);
                }),
            )
            .on_drop(move |drag: &FileRowDrag, _, cx| {
                let _ = row_drop_view.update(cx, |this, cx| this.finish_file_drag(cx));
                if drag.path == drop_directory
                    || path_is_equal_or_descendant(&drop_directory, &drag.path)
                {
                    return;
                }
                let destination = join_relative_path(&drop_directory, drag.name.as_ref());
                if destination == drag.path {
                    return;
                }
                let _ = drop_controller.update(cx, |workbench, cx| {
                    if !workbench.file_tree.contains_path(&destination) {
                        workbench.rename_path(drag.path.clone(), destination, cx);
                    }
                });
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .pl(px(row.depth as f32 * FILE_TREE_INDENT))
                    .text_sm()
                    .font_medium()
                    .text_color(text_color)
                    .child(file_tree_icon(row.icon.kind, row.ignored, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .children(names),
                    )
                    .when(directory_loading, |this| {
                        this.child(
                            Icon::new(IconName::Loader)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .when(directory_error, |this| {
                        this.child(
                            Icon::new(IconName::TriangleAlert)
                                .xsmall()
                                .text_color(cx.theme().warning),
                        )
                    }),
            )
            .on_click(move |event, window, cx| {
                focus.focus(window, cx);
                if row_rename_active {
                    return;
                }
                let temporary = event.click_count() < 2;
                let _ = click_controller.update(cx, |workbench, cx| {
                    if is_directory {
                        workbench.toggle_directory_chain(click_chain.clone(), cx);
                    } else {
                        workbench.request_preview_panel(cx);
                        workbench.open_file(click_path.clone(), temporary, window, cx);
                    }
                    workbench.persist(cx);
                    cx.notify();
                });
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if row_rename_active
                    || (event.keystroke.key != "enter" && event.keystroke.key != "space")
                {
                    return;
                }
                let _ = keyboard_controller.update(cx, |workbench, cx| {
                    if is_directory {
                        workbench.toggle_directory_chain(keyboard_chain.clone(), cx);
                    } else {
                        workbench.request_preview_panel(cx);
                        workbench.open_file(keyboard_path.clone(), true, window, cx);
                    }
                    workbench.persist(cx);
                });
                cx.stop_propagation();
            })
            .context_menu(move |menu, window, cx| {
                Self::build_file_context_menu(
                    menu,
                    FileContextMenuTarget {
                        path: context_path.clone(),
                        name: context_name.clone(),
                        kind: row_kind,
                        target_directory: context_target_directory.clone(),
                        directory_error,
                    },
                    workspace_root.clone(),
                    clipboard_available,
                    mutation_pending,
                    context_view.clone(),
                    context_controller.clone(),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    fn render_worktree_lifecycle(
        &mut self,
        view: WorktreeLifecycleView,
        confirmation: Option<WorktreeLifecycleConfirmation>,
        loading: bool,
        pending: bool,
        mutations_available: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dirty = self
            .workbench
            .read(cx)
            .git
            .status
            .as_ref()
            .is_some_and(|status| status.dirty);
        let mut surface = v_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.75));

        if let Some(managed) = view.managed.clone() {
            let source_branch = managed
                .branch
                .as_deref()
                .unwrap_or_else(|| locale::text("Unknown branch", "未知分支", "未知分支"))
                .to_string();
            let target_branch = managed
                .target_branch
                .as_deref()
                .unwrap_or_else(|| locale::text("Unknown target", "未知目标", "未知目標"))
                .to_string();
            let state_label = worktree_lifecycle_state_label(view.state);
            let state_color = worktree_lifecycle_state_color(view.state, cx);
            let readiness_action = if mutations_available {
                match worktree_lifecycle_primary_action(view.state) {
                    Some(WorktreeLifecyclePrimaryAction::ReviewChanges) => Some(
                        Button::new("worktree-review-changes")
                            .small()
                            .outline()
                            .label(locale::text("Review changes", "检查改动", "檢查變更"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.set_worktree_readiness(
                                        GitWorktreeReadinessState::Reviewing,
                                        cx,
                                    )
                                })
                            }))
                            .into_any_element(),
                    ),
                    Some(WorktreeLifecyclePrimaryAction::MarkReady) => Some(
                        Button::new("worktree-mark-ready")
                            .small()
                            .primary()
                            .label(locale::text("Mark ready", "标记可合并", "標記可合併"))
                            .disabled(pending || dirty)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.set_worktree_readiness(
                                        GitWorktreeReadinessState::ReadyToMerge,
                                        cx,
                                    )
                                })
                            }))
                            .into_any_element(),
                    ),
                    Some(WorktreeLifecyclePrimaryAction::MergeBack) => {
                        let label = localized_merge_action_label(&target_branch);
                        Some(
                            Button::new("worktree-merge-back")
                                .small()
                                .primary()
                                .label(label)
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.request_worktree_merge_confirmation(cx)
                                    })
                                }))
                                .into_any_element(),
                        )
                    }
                    Some(WorktreeLifecyclePrimaryAction::ReviewQueuedMerge) => Some(
                        Button::new("worktree-review-queued-merge")
                            .small()
                            .outline()
                            .label(locale::text("Review merge", "检查合并", "檢查合併"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.request_worktree_merge_confirmation(cx)
                                })
                            }))
                            .into_any_element(),
                    ),
                    Some(WorktreeLifecyclePrimaryAction::Restore) => Some(
                        Button::new("worktree-restore")
                            .small()
                            .primary()
                            .label(locale::text(
                                "Restore Worktree",
                                "恢复 Worktree",
                                "還原 Worktree",
                            ))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.request_worktree_restore_confirmation(cx)
                                })
                            }))
                            .into_any_element(),
                    ),
                    None => None,
                }
            } else {
                None
            };
            let more_actions = (mutations_available
                && managed.status == GitManagedWorktreeStatus::Active
                && !view.target_owned)
                .then(|| {
                    let archive_workbench = self.workbench.downgrade();
                    let discard_workbench = self.workbench.downgrade();
                    Button::new("worktree-more-actions")
                        .small()
                        .ghost()
                        .compact()
                        .icon(IconName::ChevronDown)
                        .tooltip(locale::text(
                            "More Worktree actions",
                            "更多 Worktree 操作",
                            "更多 Worktree 操作",
                        ))
                        .disabled(pending)
                        .dropdown_menu(move |menu, _, _| {
                            let archive_workbench = archive_workbench.clone();
                            let discard_workbench = discard_workbench.clone();
                            menu.item(
                                PopupMenuItem::new(locale::text(
                                    "Archive Worktree",
                                    "归档 Worktree",
                                    "封存 Worktree",
                                ))
                                .on_click(move |_, _, cx| {
                                    let _ = archive_workbench.update(cx, |workbench, cx| {
                                        workbench.request_worktree_archive_confirmation(cx)
                                    });
                                }),
                            )
                            .item(
                                PopupMenuItem::new(locale::text(
                                    "Discard Worktree",
                                    "丢弃 Worktree",
                                    "捨棄 Worktree",
                                ))
                                .on_click(move |_, _, cx| {
                                    let _ = discard_workbench.update(cx, |workbench, cx| {
                                        workbench.request_worktree_discard_confirmation(cx)
                                    });
                                }),
                            )
                        })
                        .into_any_element()
                });
            surface = surface.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .bg(cx.theme().sidebar.opacity(0.72))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::default()
                                    .path("icons/vibex/git-branch.svg")
                                    .size(px(15.0))
                                    .text_color(state_color),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_semibold()
                                            .child(format!("{source_branch} -> {target_branch}")),
                                    )
                                    .child(
                                        h_flex()
                                            .flex_wrap()
                                            .gap_2()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(div().text_color(state_color).child(state_label))
                                            .child(if dirty {
                                                locale::text(
                                                    "Uncommitted changes",
                                                    "有未提交改动",
                                                    "有未提交變更",
                                                )
                                            } else {
                                                locale::text(
                                                    "Source committed",
                                                    "源改动已提交",
                                                    "來源變更已提交",
                                                )
                                            }),
                                    ),
                            )
                            .when(loading || pending, |this| {
                                this.child(Icon::new(IconName::LoaderCircle).size(px(14.0)))
                            }),
                    )
                    .when(
                        readiness_action.is_some() || more_actions.is_some(),
                        |this| {
                            this.child(
                                h_flex()
                                    .w_full()
                                    .flex_wrap()
                                    .justify_end()
                                    .gap_2()
                                    .children(readiness_action)
                                    .children(more_actions),
                            )
                        },
                    ),
            );
        }

        if view.owns_conflict_resolution()
            && let Some(operation) = view.operation.clone()
        {
            let source_branch = operation
                .branch
                .as_deref()
                .unwrap_or_else(|| locale::text("source", "源分支", "來源分支"));
            let target_branch = operation
                .detail
                .target_branch
                .as_deref()
                .unwrap_or_else(|| locale::text("target", "目标分支", "目標分支"));
            let unresolved = operation
                .detail
                .conflicts
                .iter()
                .filter(|conflict| !conflict.resolved)
                .count();
            let needs_resolution = operation.status == GitWorktreeOperationStatus::NeedsResolution;
            let rebase =
                operation.detail.merge_strategy == Some(GitWorktreeMergeStrategy::RebaseAndMerge);
            surface = surface.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .bg(if needs_resolution {
                        cx.theme().warning.opacity(0.10)
                    } else {
                        cx.theme().danger.opacity(0.10)
                    })
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(
                                Icon::default()
                                    .path("icons/vibex/shield-alert.svg")
                                    .size(px(15.0))
                                    .text_color(if needs_resolution {
                                        cx.theme().warning
                                    } else {
                                        cx.theme().danger
                                    }),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap(px(2.0))
                                    .child(
                                        div().min_w_0().whitespace_normal().font_semibold().child(
                                            if needs_resolution {
                                                localized_conflict_title(
                                                    source_branch,
                                                    target_branch,
                                                    rebase,
                                                )
                                            } else {
                                                localized_attention_title(
                                                    source_branch,
                                                    target_branch,
                                                )
                                            },
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(localized_conflict_count(unresolved)),
                                    ),
                            ),
                    )
                    .when(operation.detail.source_commits_after_start > 0, |this| {
                        this.child(
                            div()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(localized_source_delta(
                                    operation.detail.source_commits_after_start,
                                )),
                        )
                    })
                    .when_some(operation.detail.diagnostic.clone(), |this, diagnostic| {
                        this.child(
                            div()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(locale::localize_ui_message(&diagnostic.summary)),
                        )
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap_2()
                            .when(mutations_available, |this| {
                                this.child(
                                    Button::new("worktree-agent-assistance")
                                        .small()
                                        .outline()
                                        .icon(IconName::Bot)
                                        .label(locale::text(
                                            "Ask Agent",
                                            "让 Agent 协助",
                                            "讓 Agent 協助",
                                        ))
                                        .disabled(pending)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.update_workbench(cx, |workbench, cx| {
                                                workbench.request_worktree_agent_assistance(cx)
                                            })
                                        })),
                                )
                            })
                            .child(
                                Button::new("worktree-open-terminal")
                                    .small()
                                    .outline()
                                    .label(locale::text("Open terminal", "打开终端", "開啟終端機"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_workspace_integrated_terminal(window, cx)
                                    })),
                            )
                            .when(needs_resolution && mutations_available, |this| {
                                this.child(
                                    Button::new("worktree-abort-merge")
                                        .small()
                                        .danger()
                                        .label(if rebase {
                                            locale::text(
                                                "Abort rebase",
                                                "中止 Rebase",
                                                "中止 Rebase",
                                            )
                                        } else {
                                            locale::text("Abort merge", "中止合并", "中止合併")
                                        })
                                        .disabled(pending)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.update_workbench(cx, |workbench, cx| {
                                                workbench.request_worktree_abort_confirmation(cx)
                                            })
                                        })),
                                )
                                .child(
                                    Button::new("worktree-complete-merge")
                                        .small()
                                        .primary()
                                        .label(if rebase {
                                            locale::text(
                                                "Continue rebase",
                                                "继续 Rebase",
                                                "繼續 Rebase",
                                            )
                                        } else {
                                            locale::text("Complete merge", "完成合并", "完成合併")
                                        })
                                        .disabled(pending || unresolved > 0)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.update_workbench(cx, |workbench, cx| {
                                                workbench.request_worktree_continue_confirmation(cx)
                                            })
                                        })),
                                )
                            }),
                    ),
            );
        }

        if mutations_available && let Some(confirmation) = confirmation {
            surface = surface.child(self.render_worktree_confirmation(confirmation, pending, cx));
        }
        surface.into_any_element()
    }

    fn render_worktree_confirmation(
        &mut self,
        confirmation: WorktreeLifecycleConfirmation,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, summary, action_label, allowed, dangerous, risks) =
            worktree_confirmation_copy(&confirmation);
        let confirm_button = Button::new("confirm-worktree-lifecycle-action")
            .small()
            .label(action_label)
            .disabled(pending || !allowed);
        let confirm_button = if dangerous {
            confirm_button.danger()
        } else {
            confirm_button.primary()
        };
        let merge_strategy = match &confirmation {
            WorktreeLifecycleConfirmation::Merge { strategy, .. } => Some(*strategy),
            _ => None,
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.65))
            .px_3()
            .py_3()
            .bg(cx.theme().background.opacity(0.94))
            .child(div().font_semibold().text_sm().child(title))
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
            .when_some(merge_strategy, |this, strategy| {
                this.child(
                    h_flex()
                        .w_full()
                        .rounded(px(7.0))
                        .bg(cx.theme().muted.opacity(0.55))
                        .p(px(2.0))
                        .child(
                            Button::new("worktree-merge-strategy-merge")
                                .small()
                                .ghost()
                                .flex_1()
                                .justify_center()
                                .selected(strategy == GitWorktreeMergeStrategy::NoFfMerge)
                                .label(locale::text("Merge", "合并", "合併"))
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.set_worktree_merge_strategy(
                                            GitWorktreeMergeStrategy::NoFfMerge,
                                            cx,
                                        )
                                    })
                                })),
                        )
                        .child(
                            Button::new("worktree-merge-strategy-rebase")
                                .small()
                                .ghost()
                                .flex_1()
                                .justify_center()
                                .selected(strategy == GitWorktreeMergeStrategy::RebaseAndMerge)
                                .label(locale::text("Rebase", "Rebase", "Rebase"))
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.set_worktree_merge_strategy(
                                            GitWorktreeMergeStrategy::RebaseAndMerge,
                                            cx,
                                        )
                                    })
                                })),
                        ),
                )
            })
            .children(risks.into_iter().map(|risk| {
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_xs()
                    .text_color(if risk.blocking {
                        cx.theme().danger
                    } else {
                        cx.theme().warning
                    })
                    .child(format!(
                        "{}: {}",
                        worktree_risk_label(risk.kind),
                        risk.summary
                    ))
            }))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-worktree-lifecycle-action")
                            .small()
                            .outline()
                            .label(locale::text("Cancel", "取消", "取消"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.cancel_worktree_lifecycle_confirmation(cx)
                                })
                            })),
                    )
                    .child(confirm_button.on_click(cx.listener(|this, _, _, cx| {
                        this.update_workbench(cx, |workbench, cx| {
                            workbench.confirm_worktree_lifecycle_action(cx)
                        })
                    }))),
            )
            .into_any_element()
    }

    fn render_worktree_conflicts(
        &mut self,
        conflicts: Vec<GitWorktreeConflictFile>,
        source_branch: String,
        target_branch: String,
        mutations_available: bool,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = conflicts.len();
        let rows = conflicts
            .into_iter()
            .take(WORKTREE_CONFLICT_RENDER_LIMIT)
            .map(|conflict| {
                self.render_worktree_conflict_row(
                    conflict,
                    &source_branch,
                    &target_branch,
                    mutations_available,
                    pending,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        v_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.70))
            .child(
                h_flex()
                    .h(px(34.0))
                    .px_3()
                    .gap_2()
                    .bg(cx.theme().warning.opacity(0.08))
                    .child(
                        Icon::default()
                            .path("icons/vibex/shield-alert.svg")
                            .size(px(13.0))
                            .text_color(cx.theme().warning),
                    )
                    .child(div().font_semibold().text_xs().child(locale::text(
                        "Conflicts",
                        "冲突",
                        "衝突",
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(total.to_string()),
                    ),
            )
            .children(rows)
            .when(total > WORKTREE_CONFLICT_RENDER_LIMIT, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .whitespace_normal()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(localized_conflict_render_limit(
                            WORKTREE_CONFLICT_RENDER_LIMIT,
                            total,
                        )),
                )
            })
            .into_any_element()
    }

    fn render_worktree_conflict_row(
        &mut self,
        conflict: GitWorktreeConflictFile,
        source_branch: &str,
        target_branch: &str,
        mutations_available: bool,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = conflict.path.clone();
        let open_path = path.clone();
        let target_path = path.clone();
        let source_path = path.clone();
        let stage_path = path.clone();
        let target_label = localized_use_version_label(target_branch);
        let source_label = localized_use_version_label(source_branch);
        v_flex()
            .id(format!("worktree-conflict-row-{path}"))
            .w_full()
            .min_w_0()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.45))
            .px_3()
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .font_family("monospace")
                            .font_weight(code_font_weight(cx))
                            .font_semibold()
                            .text_color(cx.theme().warning)
                            .child("!"),
                    )
                    .child(
                        div()
                            .id(format!("open-worktree-conflict-{path}"))
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .truncate()
                            .font_family("monospace")
                            .font_weight(code_font_weight(cx))
                            .text_xs()
                            .child(path.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.open_diff(
                                        GitSelectionKey {
                                            path: open_path.clone(),
                                            staged: false,
                                        },
                                        cx,
                                    )
                                })
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(worktree_conflict_kind_label(conflict.kind, conflict.binary)),
                    ),
            )
            .when(mutations_available, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new(format!("use-target-conflict-{path}"))
                                .small()
                                .outline()
                                .label(target_label)
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.resolve_worktree_conflict(
                                            target_path.clone(),
                                            GitWorktreeConflictVersion::Target,
                                            cx,
                                        )
                                    })
                                })),
                        )
                        .child(
                            Button::new(format!("use-source-conflict-{path}"))
                                .small()
                                .outline()
                                .label(source_label)
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.resolve_worktree_conflict(
                                            source_path.clone(),
                                            GitWorktreeConflictVersion::Source,
                                            cx,
                                        )
                                    })
                                })),
                        )
                        .child(
                            Button::new(format!("stage-worktree-conflict-{path}"))
                                .small()
                                .primary()
                                .label(locale::text("Mark resolved", "标记已解决", "標記已解決"))
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.stage_worktree_conflict(stage_path.clone(), cx)
                                    })
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_git(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let git = &self.projection.git;
        let (
            workspace_available,
            mode,
            pending,
            status_loading,
            selected_count,
            root_selection,
            has_directories,
            all_directories_expanded,
            workspace_name,
            lifecycle_view,
            lifecycle_confirmation,
            lifecycle_loading,
            lifecycle_action_pending,
            lifecycle_mutations_available,
        ) = (
            self.projection.files.workspace_available,
            git.mode,
            git.pending,
            git.status_loading,
            git.selected_count,
            git.root_selection,
            git.has_directories,
            git.all_directories_expanded,
            self.projection.workspace_name.clone(),
            self.projection.lifecycle_view.clone(),
            self.projection.lifecycle_confirmation.clone(),
            self.projection.lifecycle_loading,
            self.projection.lifecycle_action_pending,
            self.projection.lifecycle_mutations_available,
        );
        if !workspace_available {
            return rail_empty_card(
                locale::text("No Git status", "没有 Git 状态", "沒有 Git 狀態"),
                locale::text(
                    "Select an Agent session to load Git",
                    "选择 Agent 会话以加载 Git",
                    "選擇 Agent 會話以載入 Git",
                ),
                cx,
            );
        }

        let changes_active = mode == GitWorkbenchMode::Changes;
        let history_active = mode == GitWorkbenchMode::History;
        let lifecycle_fenced = lifecycle_view.as_ref().is_some_and(|view| {
            view.target_owned
                && view.operation.as_ref().is_some_and(|operation| {
                    matches!(
                        operation.status,
                        GitWorktreeOperationStatus::NeedsResolution
                            | GitWorktreeOperationStatus::NeedsAttention
                    )
                })
        });
        let additions = git.additions;
        let deletions = git.deletions;
        let change_count = git.change_count;
        let history_search_input = self.history_search_input.clone();

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(40.0))
                    .flex_none()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .child(
                                Button::new("git-mode-changes")
                                    .ghost()
                                    .rounded_none()
                                    .h_full()
                                    .w_full()
                                    .font_semibold()
                                    .label(locale::text("Changes", "更改", "變更"))
                                    .text_color(if changes_active {
                                        cx.theme().sidebar_foreground
                                    } else {
                                        cx.theme().sidebar_foreground.opacity(0.55)
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_workbench(cx, |workbench, cx| {
                                            workbench.set_git_mode(GitWorkbenchMode::Changes, cx)
                                        })
                                    })),
                            )
                            .child(div().h(px(2.0)).w_full().bg(if changes_active {
                                cx.theme().sidebar_foreground.opacity(0.55)
                            } else {
                                cx.theme().transparent
                            })),
                    )
                    .child(
                        v_flex()
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .child(
                                Button::new("git-mode-history")
                                    .ghost()
                                    .rounded_none()
                                    .h_full()
                                    .w_full()
                                    .font_semibold()
                                    .label(locale::text("History", "历史", "歷史"))
                                    .text_color(if history_active {
                                        cx.theme().sidebar_foreground
                                    } else {
                                        cx.theme().sidebar_foreground.opacity(0.55)
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_workbench(cx, |workbench, cx| {
                                            workbench.set_git_mode(GitWorkbenchMode::History, cx)
                                        })
                                    })),
                            )
                            .child(div().h(px(2.0)).w_full().bg(if history_active {
                                cx.theme().sidebar_foreground.opacity(0.55)
                            } else {
                                cx.theme().transparent
                            })),
                    ),
            )
            .when(changes_active, |this| {
                this.when_some(lifecycle_view, |this, view| {
                    this.child(self.render_worktree_lifecycle(
                        view,
                        lifecycle_confirmation,
                        lifecycle_loading,
                        lifecycle_action_pending,
                        lifecycle_mutations_available,
                        cx,
                    ))
                })
            })
            .child(
                h_flex()
                    .h(px(40.0))
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(if changes_active {
                        cx.theme().border.opacity(0.45)
                    } else {
                        cx.theme().border.opacity(0.70)
                    })
                    .child(
                        Button::new("git-fetch")
                            .small()
                            .ghost()
                            .compact()
                            .w(px(20.0))
                            .h(px(20.0))
                            .p_0()
                            .icon(Icon::default().path("icons/vibex/download.svg"))
                            .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                            .tooltip(locale::text("Fetch", "获取", "擷取"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.remote_action(GitRemoteActionKind::Fetch, cx)
                                })
                            })),
                    )
                    .child(
                        Button::new("git-push")
                            .small()
                            .ghost()
                            .compact()
                            .w(px(20.0))
                            .h(px(20.0))
                            .p_0()
                            .icon(Icon::default().path("icons/vibex/upload.svg"))
                            .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                            .tooltip(locale::text("Push", "推送", "推送"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| {
                                    workbench.remote_action(GitRemoteActionKind::Push, cx)
                                })
                            })),
                    )
                    .child(
                        Button::new("refresh-git")
                            .small()
                            .ghost()
                            .compact()
                            .w(px(20.0))
                            .h(px(20.0))
                            .p_0()
                            .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                            .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                            .tooltip(locale::text("Refresh Git", "刷新 Git", "重新整理 Git"))
                            .loading(status_loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_workbench(cx, |workbench, cx| workbench.refresh_git(cx))
                            })),
                    )
                    .when(history_active, |this| {
                        this.child(
                            h_flex()
                                .h(px(28.0))
                                .min_w_0()
                                .ml_auto()
                                .flex_none()
                                .w(px(260.0))
                                .gap_1()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().input)
                                .bg(cx.theme().background)
                                .px_1p5()
                                .child(
                                    Icon::new(IconName::Search)
                                        .xsmall()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(div().min_w_0().flex_1().child(
                                    Input::new(&history_search_input).small().appearance(false),
                                ))
                                .child(
                                    Button::new("clear-git-history-search-toolbar")
                                        .xsmall()
                                        .ghost()
                                        .compact()
                                        .p_0()
                                        .icon(IconName::Close)
                                        .tooltip(locale::text(
                                            "Clear commit search",
                                            "清除提交搜索",
                                            "清除提交搜尋",
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.history_search_input.update(cx, |input, cx| {
                                                input.set_value("", window, cx)
                                            });
                                            this.start_history_search(cx);
                                        })),
                                ),
                        )
                    })
                    .when(changes_active, |this| {
                        this.child(
                            Button::new("revert-selected-toolbar")
                                .small()
                                .ghost()
                                .compact()
                                .w(px(20.0))
                                .h(px(20.0))
                                .p_0()
                                .icon(IconName::Undo2)
                                .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                                .tooltip(locale::text(
                                    "Rollback selected changes",
                                    "回滚所选更改",
                                    "回復所選變更",
                                ))
                                .disabled(
                                    pending
                                        || lifecycle_action_pending
                                        || lifecycle_fenced
                                        || selected_count == 0,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_git_action(
                                        locale::text(
                                            "Rollback selected changes?",
                                            "回滚所选更改？",
                                            "回復所選變更？",
                                        ),
                                        locale::text(
                                            "This discards the selected working tree changes.",
                                            "这将丢弃所选工作树更改。",
                                            "這將捨棄所選工作樹變更。",
                                        ),
                                        |workbench, cx| workbench.revert_selected(cx),
                                        window,
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            Button::new("show-git-history")
                                .small()
                                .ghost()
                                .compact()
                                .w(px(20.0))
                                .h(px(20.0))
                                .p_0()
                                .icon(IconName::Eye)
                                .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                                .tooltip(locale::text("Show history", "显示历史", "顯示歷史"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.set_git_mode(GitWorkbenchMode::History, cx)
                                    })
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .w(px(1.0))
                                .h(px(24.0))
                                .flex_none()
                                .bg(cx.theme().border.opacity(0.70)),
                        )
                        .child(
                            Button::new("toggle-all-git-directories")
                                .small()
                                .ghost()
                                .compact()
                                .w(px(20.0))
                                .h(px(20.0))
                                .p_0()
                                .icon(if all_directories_expanded {
                                    Icon::default().path("icons/vibex/chevrons-down-up.svg")
                                } else {
                                    Icon::new(IconName::ChevronsUpDown)
                                })
                                .text_color(cx.theme().sidebar_foreground.opacity(0.48))
                                .tooltip(if all_directories_expanded {
                                    locale::text(
                                        "Collapse all changes",
                                        "折叠所有更改",
                                        "摺疊所有變更",
                                    )
                                } else {
                                    locale::text(
                                        "Expand all changes",
                                        "展开所有更改",
                                        "展開所有變更",
                                    )
                                })
                                .disabled(!has_directories)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.git.toggle_all_change_directories();
                                        cx.notify();
                                    })
                                })),
                        )
                    }),
            )
            .when(changes_active, |this| {
                this.child(
                    h_flex()
                        .h(px(48.0))
                        .flex_none()
                        .min_w_0()
                        .gap_2()
                        .px_3()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.70))
                        .child(
                            Button::new("select-all-git-changes")
                                .small()
                                .ghost()
                                .compact()
                                .w(px(20.0))
                                .h(px(20.0))
                                .p_0()
                                .tooltip(locale::text(
                                    "Select all changes",
                                    "选择所有更改",
                                    "選擇所有變更",
                                ))
                                .child(git_selection_indicator(root_selection, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let selected = root_selection != GitPathSelectionState::Checked;
                                    this.update_workbench(cx, |workbench, cx| {
                                        workbench.git.select_path_prefix("", selected);
                                        cx.notify();
                                    })
                                })),
                        )
                        .child(
                            div().min_w_0().flex_1().child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_base()
                                            .font_medium()
                                            .child(workspace_name),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().sidebar_foreground.opacity(0.45))
                                            .child(match locale::current_locale() {
                                                locale::ResolvedLocale::En => {
                                                    format!("({change_count} files)")
                                                }
                                                locale::ResolvedLocale::ZhCn => {
                                                    format!("（{change_count} 个文件）")
                                                }
                                                locale::ResolvedLocale::ZhTw => {
                                                    format!("（{change_count} 個檔案）")
                                                }
                                            }),
                                    ),
                            ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_3()
                                .font_family("monospace")
                                .font_weight(code_font_weight(cx))
                                .text_xs()
                                .font_semibold()
                                .child(
                                    div()
                                        .text_color(if additions > 0 {
                                            cx.theme().success
                                        } else {
                                            cx.theme().sidebar_foreground.opacity(0.35)
                                        })
                                        .child(format!("+{additions}")),
                                )
                                .child(
                                    div()
                                        .text_color(if deletions > 0 {
                                            cx.theme().danger
                                        } else {
                                            cx.theme().sidebar_foreground.opacity(0.35)
                                        })
                                        .child(format!("-{deletions}")),
                                ),
                        ),
                )
            })
            .child(match mode {
                GitWorkbenchMode::Changes => self.render_git_changes(pending, cx),
                GitWorkbenchMode::History => self.render_git_history(cx),
            })
            .into_any_element()
    }

    fn render_git_changes(&mut self, pending: bool, cx: &mut Context<Self>) -> AnyElement {
        let change_row_count = self.projection.git.change_rows.len();
        let commit_message = self.projection.commit_message.clone();
        let commit_type = self.projection.commit_type.clone();
        let amend = self.projection.amend_commit;
        let selected_count = self.projection.git.selected_count;
        let scroll_handle = self.projection.git.scroll.clone();
        let conflict_context = self
            .projection
            .lifecycle_view
            .clone()
            .filter(|view| view.target_owned)
            .and_then(|view| view.operation)
            .filter(|operation| {
                matches!(
                    operation.status,
                    GitWorktreeOperationStatus::NeedsResolution
                        | GitWorktreeOperationStatus::NeedsAttention
                )
            })
            .map(|operation| {
                (
                    operation.detail.conflicts,
                    operation.branch.unwrap_or_else(|| "source".to_string()),
                    operation
                        .detail
                        .target_branch
                        .unwrap_or_else(|| "target".to_string()),
                )
            });
        let lifecycle_pending = self.projection.lifecycle_action_pending;
        let lifecycle_mutations_available = self.projection.lifecycle_mutations_available;
        let type_workbench = self.workbench.downgrade();
        let active_type = commit_type.clone();
        let push_workbench = self.workbench.downgrade();
        let has_conflicts = conflict_context
            .as_ref()
            .is_some_and(|(conflicts, _, _)| !conflicts.is_empty());
        let lifecycle_fenced = conflict_context.is_some();
        let action_pending = pending || lifecycle_pending || lifecycle_fenced;

        v_flex()
            .flex_1()
            .min_h_0()
            .when_some(conflict_context, |this, (conflicts, source, target)| {
                this.child(self.render_worktree_conflicts(
                    conflicts,
                    source,
                    target,
                    lifecycle_mutations_available,
                    lifecycle_pending,
                    cx,
                ))
            })
            .child(if change_row_count == 0 {
                rail_empty_card(
                    if has_conflicts {
                        locale::text("Other changes", "其他更改", "其他變更")
                    } else {
                        locale::text("clean", "干净", "乾淨")
                    },
                    if has_conflicts {
                        locale::text(
                            "No other changed files in this workspace.",
                            "此工作区没有其他更改的文件。",
                            "此工作區沒有其他變更的檔案。",
                        )
                    } else {
                        locale::text(
                            "No changed files in this workspace.",
                            "此工作区没有更改的文件。",
                            "此工作區沒有變更的檔案。",
                        )
                    },
                    cx,
                )
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .py_2()
                    .child(
                        uniform_list(
                            "git-change-rows",
                            change_row_count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                let range = bounded_uniform_range(
                                    range,
                                    change_row_count,
                                    CODE_WORKBENCH_MAX_EAGER_ROWS,
                                );
                                let rows = range
                                    .filter_map(|index| {
                                        this.projection.git.change_rows.get(index).cloned()
                                    })
                                    .collect::<Vec<_>>();
                                rows.into_iter()
                                    .map(|entry| {
                                        this.render_git_tree_row(
                                            entry.row,
                                            entry.change,
                                            GitTreeInteraction::Changes,
                                            cx,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll_handle)
                        .size_full(),
                    )
                    .into_any_element()
            })
            .child(
                v_flex()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar.opacity(0.95))
                    .p_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .mb_2()
                            .child(
                                Button::new("git-commit-type")
                                    .small()
                                    .outline()
                                    .w(px(112.0))
                                    .h(px(32.0))
                                    .label(commit_type)
                                    .dropdown_caret(true)
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for candidate in GIT_COMMIT_TYPES {
                                            let workbench = type_workbench.clone();
                                            let value = candidate.to_string();
                                            let checked = active_type == candidate;
                                            menu = menu.item(
                                                PopupMenuItem::new(candidate)
                                                    .checked(checked)
                                                    .on_click(move |_, window, cx| {
                                                        let value = value.clone();
                                                        let placeholder =
                                                            git_commit_placeholder(&value);
                                                        let _ = workbench.update(
                                                            cx,
                                                            |workbench, cx| {
                                                                workbench.commit_type =
                                                                    value.clone();
                                                                workbench.commit_message.update(
                                                                    cx,
                                                                    |input, cx| {
                                                                        input.set_placeholder(
                                                                            placeholder,
                                                                            window,
                                                                            cx,
                                                                        )
                                                                    },
                                                                );
                                                                cx.notify();
                                                            },
                                                        );
                                                    }),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .child(
                                Button::new("toggle-amend")
                                    .small()
                                    .ghost()
                                    .compact()
                                    .tooltip(locale::text(
                                        "Amend last commit",
                                        "修正上次提交",
                                        "修正上次提交",
                                    ))
                                    .child(git_selection_indicator(
                                        if amend {
                                            GitPathSelectionState::Checked
                                        } else {
                                            GitPathSelectionState::Unchecked
                                        },
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().sidebar_foreground.opacity(0.70))
                                            .child("amend"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_workbench(cx, |workbench, cx| {
                                            workbench.amend_commit = !workbench.amend_commit;
                                            cx.notify();
                                        })
                                    })),
                            ),
                    )
                    .child(
                        Input::new(&commit_message)
                            .small()
                            .h(px(GIT_COMMIT_MESSAGE_HEIGHT))
                            .w_full(),
                    )
                    .child(
                        h_flex()
                            .mt_3()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("rollback-selected")
                                    .outline()
                                    .h(px(36.0))
                                    .px_4()
                                    .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                                    .label(locale::text("Rollback", "回滚", "回復"))
                                    .disabled(action_pending || selected_count == 0)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_git_action(
                                            locale::text(
                                                "Rollback selected changes?",
                                                "回滚所选更改？",
                                                "回復所選變更？",
                                            ),
                                            locale::text(
                                                "This discards the selected working tree changes.",
                                                "这将丢弃所选工作树更改。",
                                                "這將捨棄所選工作樹變更。",
                                            ),
                                            |workbench, cx| workbench.revert_selected(cx),
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                h_flex()
                                    .h(px(36.0))
                                    .flex_none()
                                    .gap_0()
                                    .overflow_hidden()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().background.opacity(0.80))
                                    .child(
                                        Button::new("commit-changes")
                                            .ghost()
                                            .rounded_none()
                                            .h_full()
                                            .px_3()
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .child(locale::text("Commit", "提交", "提交"))
                                                    .child(
                                                        div()
                                                            .min_w(px(18.0))
                                                            .h(px(18.0))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .rounded(px(4.0))
                                                            .bg(cx.theme().primary.opacity(0.18))
                                                            .px_1()
                                                            .font_family("monospace")
                                                            .text_xs()
                                                            .text_color(cx.theme().foreground)
                                                            .child(selected_count.to_string()),
                                                    ),
                                            )
                                            .loading(pending || lifecycle_pending)
                                            .disabled(action_pending || selected_count == 0)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                let window_handle = window.window_handle();
                                                this.update_workbench(cx, |workbench, cx| {
                                                    workbench.commit(false, window_handle, cx)
                                                })
                                            })),
                                    )
                                    .child(
                                        div().w(px(1.0)).h_full().flex_none().bg(cx.theme().border),
                                    )
                                    .child(
                                        Button::new("commit-more-actions")
                                            .ghost()
                                            .rounded_none()
                                            .h_full()
                                            .w(px(36.0))
                                            .p_0()
                                            .icon(IconName::ChevronDown)
                                            .tooltip(locale::text(
                                                "More commit actions",
                                                "更多提交操作",
                                                "更多提交操作",
                                            ))
                                            .disabled(action_pending || selected_count == 0)
                                            .dropdown_menu(move |menu, _, _| {
                                                let workbench = push_workbench.clone();
                                                menu.item(
                                                    PopupMenuItem::new(locale::text(
                                                        "Commit and push",
                                                        "提交并推送",
                                                        "提交並推送",
                                                    ))
                                                    .on_click(move |_, window, cx| {
                                                        let window_handle = window.window_handle();
                                                        let _ = workbench.update(
                                                            cx,
                                                            |workbench, cx| {
                                                                workbench.commit(
                                                                    true,
                                                                    window_handle,
                                                                    cx,
                                                                )
                                                            },
                                                        );
                                                    }),
                                                )
                                            }),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_git_tree_row(
        &mut self,
        row: GitTreeRow,
        change: Option<GitChange>,
        interaction: GitTreeInteraction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selection = match interaction {
            GitTreeInteraction::Changes => Some(self.projection.git.selection_state(&row.path)),
            GitTreeInteraction::Commit { .. } => None,
        };
        let selected_path = self.projection.selected_git_path.as_deref() == Some(row.path.as_str());
        let path = row.path.clone();
        let path_chain = row
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect::<Vec<_>>();
        let row_label = row
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let selection_button = selection.map(|state| {
            let select_path = path.clone();
            let is_directory = row.kind == GitTreeRowKind::Directory;
            Button::new(format!("select-git-tree-row:{}", row.id))
                .small()
                .ghost()
                .compact()
                .w(px(20.0))
                .h(px(20.0))
                .p_0()
                .mr_1p5()
                .tooltip(match locale::current_locale() {
                    locale::ResolvedLocale::En => format!("Select {path}"),
                    locale::ResolvedLocale::ZhCn => format!("选择 {path}"),
                    locale::ResolvedLocale::ZhTw => format!("選擇 {path}"),
                })
                .child(git_selection_indicator(state, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let selected = state != GitPathSelectionState::Checked;
                    this.update_workbench(cx, |workbench, cx| {
                        if is_directory {
                            workbench.git.select_path_prefix(&select_path, selected);
                        } else {
                            workbench.git.select_path(&select_path, selected);
                        }
                        cx.notify();
                    })
                }))
                .into_any_element()
        });

        if row.kind == GitTreeRowKind::Directory {
            let toggle_chain = path_chain.clone();
            let toggle_interaction = interaction.clone();
            let keyboard_chain = path_chain.clone();
            let keyboard_interaction = interaction.clone();
            let directory_label = path.clone();
            return h_flex()
                .id(row.id)
                .relative()
                .h(px(GIT_ROW_HEIGHT))
                .w_full()
                .flex_none()
                .min_w_0()
                .items_center()
                .px_1()
                .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.42)))
                .children(file_tree_guides(row.depth, cx))
                .child(div().w(px(row.depth as f32 * FILE_TREE_INDENT)).flex_none())
                .when_some(selection_button, |this, selection| this.child(selection))
                .when(selection.is_none(), |this| {
                    this.child(div().w(px(14.0)).mr_1p5().flex_none())
                })
                .child(
                    h_flex()
                        .id(format!("toggle-git-directory:{path}"))
                        .min_w_0()
                        .flex_1()
                        .gap_1p5()
                        .cursor_pointer()
                        .focusable()
                        .tab_stop(true)
                        .role(Role::Button)
                        .aria_label(directory_label)
                        .focus_visible(|style| style.bg(cx.theme().sidebar_accent.opacity(0.55)))
                        .child(file_tree_icon(FileIconKind::Directory, false, cx))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_xs()
                                .font_medium()
                                .child(row_label),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let paths = toggle_chain.clone();
                            let interaction = toggle_interaction.clone();
                            this.update_workbench(cx, |workbench, cx| {
                                match interaction {
                                    GitTreeInteraction::Changes => {
                                        workbench.git.toggle_change_directories(&paths)
                                    }
                                    GitTreeInteraction::Commit { .. } => {
                                        workbench.git.toggle_commit_directories(&paths)
                                    }
                                }
                                cx.notify();
                            })
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key != "enter" && event.keystroke.key != "space" {
                                return;
                            }
                            let paths = keyboard_chain.clone();
                            let interaction = keyboard_interaction.clone();
                            this.update_workbench(cx, |workbench, cx| {
                                match interaction {
                                    GitTreeInteraction::Changes => {
                                        workbench.git.toggle_change_directories(&paths)
                                    }
                                    GitTreeInteraction::Commit { .. } => {
                                        workbench.git.toggle_commit_directories(&paths)
                                    }
                                }
                                cx.notify();
                            });
                            cx.stop_propagation();
                        })),
                )
                .into_any_element();
        }

        let Some(change) = change else {
            return div().into_any_element();
        };
        let file_name = row
            .segments
            .last()
            .map(|segment| segment.name.as_str())
            .unwrap_or(row.path.as_str())
            .to_string();
        let descriptor = file_icon_descriptor(&file_name, FileEntryKind::File);
        let open_path = row.path.clone();
        let open_interaction = interaction.clone();
        let keyboard_path = row.path.clone();
        let keyboard_interaction = interaction.clone();
        let keyboard_change = change.clone();
        let accessible_name = row.path.clone();
        let status_color = git_change_text_color(&change, cx);
        let deleted = change.kind == GitChangeKind::Deleted;

        h_flex()
            .id(row.id)
            .relative()
            .h(px(GIT_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .min_w_0()
            .items_center()
            .px_1()
            .border_1()
            .border_color(if selected_path {
                cx.theme().primary.opacity(0.28)
            } else {
                cx.theme().transparent
            })
            .bg(if selected_path {
                cx.theme().primary.opacity(0.07)
            } else {
                cx.theme().transparent
            })
            .when(!selected_path, |this| {
                this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.42)))
            })
            .children(file_tree_guides(row.depth, cx))
            .child(div().w(px(row.depth as f32 * FILE_TREE_INDENT)).flex_none())
            .when_some(selection_button, |this, selection| this.child(selection))
            .when(selection.is_none(), |this| {
                this.child(div().w(px(14.0)).mr_1p5().flex_none())
            })
            .child(
                h_flex()
                    .id(format!("open-git-tree-file:{}", row.path))
                    .min_w_0()
                    .flex_1()
                    .gap_1p5()
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .role(Role::Button)
                    .aria_label(accessible_name)
                    .focus_visible(|style| style.bg(cx.theme().sidebar_accent.opacity(0.55)))
                    .child(file_tree_icon(descriptor.kind, false, cx))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(status_color)
                            .when(deleted, |this| this.line_through())
                            .child(file_name),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .ml_2()
                            .font_family("monospace")
                            .font_weight(code_font_weight(cx))
                            .text_xs()
                            .child(
                                div()
                                    .text_color(if change.additions > 0 {
                                        cx.theme().success
                                    } else {
                                        cx.theme().sidebar_foreground.opacity(0.35)
                                    })
                                    .child(format!("+{}", change.additions)),
                            )
                            .child(
                                div()
                                    .text_color(if change.deletions > 0 {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().sidebar_foreground.opacity(0.35)
                                    })
                                    .child(format!("-{}", change.deletions)),
                            ),
                    )
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
                            .border_color(cx.theme().border.opacity(0.70))
                            .px_1()
                            .font_family("monospace")
                            .font_weight(code_font_weight(cx))
                            .text_xs()
                            .text_color(cx.theme().sidebar_foreground.opacity(0.50))
                            .child(git_change_label(change.kind)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let interaction = open_interaction.clone();
                        let path = open_path.clone();
                        this.update_workbench(cx, |workbench, cx| {
                            workbench.request_preview_panel(cx);
                            match interaction {
                                GitTreeInteraction::Changes => workbench.open_diff(
                                    GitSelectionKey {
                                        path,
                                        staged: change.staged,
                                    },
                                    cx,
                                ),
                                GitTreeInteraction::Commit { hash, subject } => {
                                    workbench.open_commit_at_path(hash, subject, path, cx)
                                }
                            }
                        })
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key != "enter" && event.keystroke.key != "space" {
                            return;
                        }
                        let interaction = keyboard_interaction.clone();
                        let path = keyboard_path.clone();
                        let change = keyboard_change.clone();
                        this.update_workbench(cx, |workbench, cx| {
                            workbench.request_preview_panel(cx);
                            match interaction {
                                GitTreeInteraction::Changes => workbench.open_diff(
                                    GitSelectionKey {
                                        path,
                                        staged: change.staged,
                                    },
                                    cx,
                                ),
                                GitTreeInteraction::Commit { hash, subject } => {
                                    workbench.open_commit_at_path(hash, subject, path, cx)
                                }
                            }
                        });
                        cx.stop_propagation();
                    })),
            )
            .into_any_element()
    }

    fn render_git_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let git = &self.projection.git;
        let history = git.history.clone();
        let history_row_count = history.len();
        let loading = git.history_loading;
        let selected_hash = git.selected_hash.clone();
        let selected_hash_for_rows = selected_hash.clone();
        let scroll_handle = git.scroll.clone();
        let graph = git_graph_layout(&history);
        let branch_select = Select::new(&self.history_branch_select)
            .small()
            .h(px(32.0))
            .w_full()
            .text_xs()
            .menu_width(px(320.0))
            .menu_max_h(px(360.0))
            .placeholder(locale::text("Branch", "分支", "分支"))
            .search_placeholder(locale::text("Search branches", "搜索分支", "搜尋分支"));
        let author_select = Select::new(&self.history_author_select)
            .small()
            .h(px(32.0))
            .w_full()
            .text_xs()
            .menu_width(px(320.0))
            .menu_max_h(px(360.0))
            .placeholder(locale::text("All Users", "所有用户", "所有使用者"))
            .search_placeholder(locale::text("Search users", "搜索用户", "搜尋使用者"));
        let date_picker = DatePicker::new(&self.history_date_picker)
            .small()
            .h(px(32.0))
            .w(px(220.0))
            .text_xs()
            .cleanable(true)
            .placeholder(locale::text("Any date", "任意日期", "任何日期"));
        let body_size_target = cx.weak_entity();

        let history_list = if history_row_count == 0 {
            if loading {
                rail_empty(
                    locale::text("Loading history", "正在加载历史", "正在載入歷史"),
                    cx,
                )
            } else {
                rail_empty_card(
                    locale::text("Recent history", "最近历史", "最近歷史"),
                    locale::text("No commits found.", "未找到提交。", "找不到提交。"),
                    cx,
                )
            }
        } else {
            div()
                .relative()
                .size_full()
                .min_h_0()
                .child(
                    uniform_list(
                        "git-history-rows",
                        history_row_count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let range = bounded_uniform_range(
                                range,
                                history_row_count,
                                CODE_WORKBENCH_MAX_EAGER_ROWS,
                            );
                            range
                                .filter_map(|index| {
                                    Some((
                                        history.get(index)?.clone(),
                                        graph.rows.get(index)?.clone(),
                                    ))
                                })
                                .map(|(commit, graph_row)| {
                                    this.render_git_history_row(
                                        commit,
                                        graph_row,
                                        graph.lane_count,
                                        selected_hash_for_rows.as_deref(),
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll_handle)
                    .size_full(),
                )
                .into_any_element()
        };
        let history_body = if let Some(hash) = selected_hash {
            let drawer_height = self.history_drawer_height;
            v_flex()
                .relative()
                .size_full()
                .min_h_0()
                .child(v_flex().flex_1().min_h_0().child(history_list))
                .child(
                    v_flex()
                        .h(px(drawer_height))
                        .min_h(px(GIT_HISTORY_DRAWER_MIN_HEIGHT))
                        .flex_none()
                        .child(self.render_git_history_drawer_resize_handle(cx))
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(self.render_git_commit_drawer(hash, cx)),
                        ),
                )
        } else {
            v_flex().size_full().min_h_0().child(history_list)
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .flex_none()
                    .h(px(42.0))
                    .gap_1p5()
                    .px_2()
                    .capture_any_mouse_down(cx.listener(|_, _, window, cx| {
                        // Select popovers are deferred overlays. Close the currently focused
                        // one during capture so the next filter opens on the same click.
                        let _ = window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                    }))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().min_w_0().flex_1().child(branch_select))
                    .child(div().min_w_0().flex_1().child(author_select))
                    .child(date_picker),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .on_prepaint(move |bounds, _, cx| {
                        let _ = body_size_target.update(cx, |this, cx| {
                            this.record_history_body_height(f32::from(bounds.size.height), cx)
                        });
                    })
                    .child(history_body),
            )
            .into_any_element()
    }

    fn render_git_history_drawer_resize_handle(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.history_drawer_resize.is_some();
        let increment_target = cx.weak_entity();
        let decrement_target = cx.weak_entity();
        h_flex()
            .id("git-history-drawer-resize")
            .role(Role::Splitter)
            .aria_label(locale::text(
                "Resize changed files drawer",
                "调整变更文件抽屉",
                "調整變更檔案抽屜",
            ))
            .aria_orientation(Orientation::Horizontal)
            .aria_numeric_value(self.history_drawer_height as f64)
            .aria_numeric_value_step(GIT_HISTORY_DRAWER_KEYBOARD_STEP as f64)
            .aria_min_numeric_value(GIT_HISTORY_DRAWER_MIN_HEIGHT as f64)
            .aria_max_numeric_value(
                self.history_body_height.max(GIT_HISTORY_DRAWER_MIN_HEIGHT) as f64
            )
            .focusable()
            .tab_index(0)
            .h(px(12.0))
            .w_full()
            .flex_none()
            .cursor_ns_resize()
            .items_center()
            .justify_center()
            .bg(if active {
                cx.theme().accent.opacity(0.24)
            } else {
                cx.theme().sidebar
            })
            .hover(|style| style.bg(cx.theme().accent.opacity(0.20)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let next = match event.keystroke.key.as_str() {
                    "up" => this.history_drawer_height + GIT_HISTORY_DRAWER_KEYBOARD_STEP,
                    "down" => this.history_drawer_height - GIT_HISTORY_DRAWER_KEYBOARD_STEP,
                    "home" => GIT_HISTORY_DRAWER_MIN_HEIGHT,
                    "end" => this.history_body_height,
                    _ => return,
                };
                this.set_history_drawer_height(next, cx);
                cx.stop_propagation();
            }))
            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                let _ = increment_target.update(cx, |this, cx| {
                    this.set_history_drawer_height(
                        this.history_drawer_height + GIT_HISTORY_DRAWER_KEYBOARD_STEP,
                        cx,
                    )
                });
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                let _ = decrement_target.update(cx, |this, cx| {
                    this.set_history_drawer_height(
                        this.history_drawer_height - GIT_HISTORY_DRAWER_KEYBOARD_STEP,
                        cx,
                    )
                });
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.history_drawer_resize = Some(GitHistoryDrawerResizeState {
                        start_window_y: f32::from(event.position.y),
                        start_height: this.history_drawer_height,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_drag(GitHistoryDrawerResizeDrag, |_, _, _, cx| {
                cx.new(|_| GitHistoryDrawerResizeDrag)
            })
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<GitHistoryDrawerResizeDrag>, _, cx| {
                    cx.stop_propagation();
                    this.update_history_drawer_resize(f32::from(event.event.position.y), cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_history_drawer_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_history_drawer_resize(cx)),
            )
            .child(
                div()
                    .h(px(4.0))
                    .w(px(42.0))
                    .rounded_full()
                    .bg(cx.theme().border),
            )
            .into_any_element()
    }

    fn render_git_history_row(
        &mut self,
        commit: vibex_core::GitCommitSummary,
        graph_row: GitGraphRow,
        graph_lane_count: usize,
        selected_hash: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = selected_hash == Some(commit.hash.as_str());
        let hash = commit.hash.clone();
        let subject = commit.subject.clone();
        let keyboard_hash = commit.hash.clone();
        let keyboard_subject = commit.subject.clone();
        let accessible_name = format!(
            "{}; {}; {}",
            commit.subject, commit.author_name, commit.short_hash
        );
        let click_workbench = self.workbench.downgrade();
        let keyboard_workbench = click_workbench.clone();
        let graph_width = graph_lane_count.clamp(1, GIT_HISTORY_GRAPH_MAX_LANES) as f32
            * GIT_HISTORY_GRAPH_LANE_WIDTH
            + 4.0;
        let graph_colors = [
            cx.theme().primary,
            cx.theme().info,
            cx.theme().success,
            cx.theme().warning,
            cx.theme().danger,
            cx.theme().accent_foreground,
            cx.theme().muted_foreground,
            cx.theme().foreground,
        ];
        let refs = commit.refs.iter().take(2).cloned().collect::<Vec<_>>();

        h_flex()
            .h(px(GIT_HISTORY_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .min_w_0()
            .pr_2()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let top = bounds.origin.y;
                        let bottom = bounds.origin.y + bounds.size.height;
                        let middle = bounds.origin.y + bounds.size.height / 2.0;
                        let lane_x = |lane: usize| {
                            // Keep the primary lane next to the commit card. Additional
                            // lanes fan out to the left, which keeps the graph balanced while
                            // making the main line easy to follow vertically.
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
                v_flex()
                    .id(format!("git-history-card:{}", commit.hash))
                    .h(px(56.0))
                    .min_w_0()
                    .flex_1()
                    .justify_center()
                    .gap_1()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if selected {
                        cx.theme().primary.opacity(0.52)
                    } else {
                        cx.theme().border.opacity(0.55)
                    })
                    .bg(if selected {
                        cx.theme().primary.opacity(0.10)
                    } else {
                        cx.theme().background.opacity(0.50)
                    })
                    .px_2()
                    .focusable()
                    .tab_index(0)
                    .aria_label(accessible_name)
                    .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.60)))
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_xs()
                                    .font_semibold()
                                    .child(commit.subject.clone()),
                            )
                            .children(refs.into_iter().map(|reference| {
                                div()
                                    .max_w(px(92.0))
                                    .flex_none()
                                    .truncate()
                                    .rounded(px(3.0))
                                    .bg(cx.theme().accent.opacity(0.55))
                                    .px_1()
                                    .text_xs()
                                    .text_color(cx.theme().accent_foreground)
                                    .child(reference)
                            })),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .child(git_history_relative_time(commit.authored_at_ms)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(commit.author_name),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_family("monospace")
                                    .font_weight(code_font_weight(cx))
                                    .child(commit.short_hash),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        let hash = hash.clone();
                        let subject = subject.clone();
                        let _ = click_workbench.update(cx, |workbench, cx| {
                            workbench.request_preview_panel(cx);
                            workbench.open_commit(hash, subject, cx);
                        });
                    })
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key != "enter" && event.keystroke.key != "space" {
                            return;
                        }
                        let hash = keyboard_hash.clone();
                        let subject = keyboard_subject.clone();
                        let _ = keyboard_workbench.update(cx, |workbench, cx| {
                            workbench.request_preview_panel(cx);
                            workbench.open_commit(hash, subject, cx);
                        });
                        cx.stop_propagation();
                    }),
            )
            .into_any_element()
    }

    fn render_git_commit_drawer(
        &mut self,
        selected_hash: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let git = &self.projection.git;
        let commit = git
            .selected_commit
            .clone()
            .filter(|commit| commit.hash == selected_hash);
        let detail_loaded = git.commit_detail_loaded
            && git.selected_hash.as_deref() == Some(selected_hash.as_str());
        let total_rows = git.commit_rows.len();
        let rows = git
            .commit_rows
            .iter()
            .take(200)
            .map(|entry| (entry.row.clone(), entry.change.clone()))
            .collect::<Vec<_>>();
        let subject = commit
            .as_ref()
            .map(|commit| commit.subject.clone())
            .unwrap_or_else(|| selected_hash.clone());
        let short_hash = commit
            .as_ref()
            .map(|commit| commit.short_hash.clone())
            .unwrap_or_default();
        let file_count = if detail_loaded {
            git.commit_file_count
        } else {
            0
        };
        let additions = if detail_loaded {
            git.commit_additions
        } else {
            0
        };
        let deletions = if detail_loaded {
            git.commit_deletions
        } else {
            0
        };
        let context = GitTreeInteraction::Commit {
            hash: selected_hash,
            subject: subject.clone(),
        };

        v_flex()
            .size_full()
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                v_flex()
                    .flex_none()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.72))
                    .child(
                        h_flex()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_2()
                                    .child(
                                        div().min_w_0().truncate().text_sm().font_semibold().child(
                                            locale::text("Changed files", "变更文件", "變更檔案"),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .rounded(px(3.0))
                                            .border_1()
                                            .border_color(cx.theme().border.opacity(0.70))
                                            .px_1p5()
                                            .py_0p5()
                                            .font_family("monospace")
                                            .font_weight(code_font_weight(cx))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(short_hash),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1p5()
                                    .font_family("monospace")
                                    .font_weight(code_font_weight(cx))
                                    .text_xs()
                                    .font_semibold()
                                    .child(
                                        div()
                                            .text_color(cx.theme().sidebar_foreground.opacity(0.45))
                                            .child(file_count.to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_color(if additions > 0 {
                                                cx.theme().success
                                            } else {
                                                cx.theme().sidebar_foreground.opacity(0.35)
                                            })
                                            .child(format!("+{additions}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(if deletions > 0 {
                                                cx.theme().danger
                                            } else {
                                                cx.theme().sidebar_foreground.opacity(0.35)
                                            })
                                            .child(format!("-{deletions}")),
                                    ),
                            )
                            .child(
                                Button::new("close-git-commit-drawer")
                                    .ml_1()
                                    .small()
                                    .ghost()
                                    .compact()
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .p_0()
                                    .icon(IconName::Close)
                                    .tooltip(locale::text(
                                        "Close changed files drawer",
                                        "关闭变更文件抽屉",
                                        "關閉變更檔案抽屜",
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_workbench(cx, |workbench, cx| {
                                            workbench.git.clear_commit_selection();
                                            cx.notify();
                                        })
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subject),
                    ),
            )
            .child(if !detail_loaded {
                rail_empty(
                    locale::text(
                        "Loading changed files",
                        "正在加载变更文件",
                        "正在載入變更檔案",
                    ),
                    cx,
                )
            } else if rows.is_empty() {
                rail_empty(
                    locale::text(
                        "No files changed in this commit.",
                        "此提交没有变更文件。",
                        "此提交沒有變更檔案。",
                    ),
                    cx,
                )
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px_2()
                    .py_2()
                    .children(rows.into_iter().map(|(row, change)| {
                        self.render_git_tree_row(row, change, context.clone(), cx)
                    }))
                    .when(total_rows > 200, |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(match locale::current_locale() {
                                    locale::ResolvedLocale::En => {
                                        format!("{} more files", total_rows - 200)
                                    }
                                    locale::ResolvedLocale::ZhCn => {
                                        format!("还有 {} 个文件", total_rows - 200)
                                    }
                                    locale::ResolvedLocale::ZhTw => {
                                        format!("還有 {} 個檔案", total_rows - 200)
                                    }
                                }),
                        )
                    })
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_file_header_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace_available = self.projection.files.workspace_available;
        let tools = available_external_tools();
        let selected_tool_id = self.selected_open_tool_id.as_deref().filter(|selected| {
            matches!(
                *selected,
                FILE_MANAGER_OPEN_TOOL_ID | NATIVE_TERMINAL_OPEN_TOOL_ID
            ) || tools.iter().any(|tool| tool.id == *selected)
        });
        let primary_icon = selected_tool_id
            .map(|tool_id| open_tool_element(tool_id, px(16.0), cx))
            .unwrap_or_else(|| {
                Icon::new(IconName::ExternalLink)
                    .size(px(16.0))
                    .into_any_element()
            });
        let primary_tool_id = selected_tool_id
            .unwrap_or(FILE_MANAGER_OPEN_TOOL_ID)
            .to_string();
        let primary_view = cx.weak_entity();
        let menu_view = cx.weak_entity();
        h_flex()
            .flex_none()
            .overflow_hidden()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border.opacity(0.70))
            .bg(cx.theme().sidebar.opacity(0.30))
            .child(
                Button::new("open-workspace-default")
                    .small()
                    .ghost()
                    .compact()
                    .rounded_none()
                    .w_6()
                    .px_0()
                    .child(primary_icon)
                    .tooltip(locale::text(
                        "Open workspace with selected tool",
                        "使用所选工具打开工作区",
                        "使用所選工具開啟工作區",
                    ))
                    .disabled(!workspace_available)
                    .on_click(move |_, _, cx| {
                        let _ = primary_view.update(cx, |this, cx| {
                            this.select_and_open_workspace_tool(primary_tool_id.clone(), cx)
                        });
                    }),
            )
            .child(
                Button::new("open-workspace-menu")
                    .small()
                    .ghost()
                    .compact()
                    .rounded_none()
                    .border_l_1()
                    .border_color(cx.theme().border.opacity(0.70))
                    .icon(IconName::ChevronDown)
                    .tooltip(locale::text(
                        "Open workspace with",
                        "选择工作区打开方式",
                        "選擇工作區開啟方式",
                    ))
                    .disabled(!workspace_available)
                    .dropdown_menu(move |menu, _, _| {
                        let file_manager_view = menu_view.clone();
                        let integrated_terminal_view = menu_view.clone();
                        let native_terminal_view = menu_view.clone();
                        let mut menu = menu
                            .min_w(px(224.0))
                            .max_w(px(224.0))
                            .item(open_tool_menu_section_label(locale::text(
                                "System", "系统", "系統",
                            )))
                            .item(
                                PopupMenuItem::new(locale::text(
                                    "File Manager",
                                    "文件管理器",
                                    "檔案管理器",
                                ))
                                .icon(IconName::FolderOpen)
                                .on_click(move |_, _, cx| {
                                    let _ = file_manager_view.update(cx, |this, cx| {
                                        this.select_and_open_workspace_tool(
                                            FILE_MANAGER_OPEN_TOOL_ID.to_string(),
                                            cx,
                                        )
                                    });
                                }),
                            )
                            .item(
                                PopupMenuItem::new(locale::text("Terminal", "终端", "終端"))
                                    .icon(IconName::SquareTerminal)
                                    .on_click(move |_, window, cx| {
                                        let _ = integrated_terminal_view.update(cx, |this, cx| {
                                            this.open_workspace_integrated_terminal(window, cx)
                                        });
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(locale::text(
                                    "Native Terminal",
                                    "本机终端",
                                    "本機終端",
                                ))
                                .icon(IconName::SquareTerminal)
                                .on_click(move |_, _, cx| {
                                    let _ = native_terminal_view.update(cx, |this, cx| {
                                        this.select_and_open_workspace_tool(
                                            NATIVE_TERMINAL_OPEN_TOOL_ID.to_string(),
                                            cx,
                                        )
                                    });
                                }),
                            )
                            .separator()
                            .item(open_tool_menu_section_label(locale::text(
                                "Tools", "工具", "工具",
                            )));
                        for tool in tools.clone() {
                            let tool_view = menu_view.clone();
                            let tool_id = tool.id.to_string();
                            menu = menu.item(open_tool_menu_item(tool.id, tool.label).on_click(
                                move |_, _, cx| {
                                    let _ = tool_view.update(cx, |this, cx| {
                                        this.select_and_open_workspace_tool(tool_id.clone(), cx)
                                    });
                                },
                            ));
                        }
                        if tools.is_empty() {
                            menu = menu.item(
                                PopupMenuItem::new(locale::text(
                                    "No installed IDE or project tool detected",
                                    "未探测到已安装的 IDE 或项目工具",
                                    "未探測到已安裝的 IDE 或專案工具",
                                ))
                                .disabled(true),
                            );
                        }
                        menu
                    }),
            )
            .into_any_element()
    }
}

impl Render for CodeRightRail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count = self.render_count.saturating_add(1);
        }
        let mode = self.projection.mode;
        let error = self.projection.error.clone();
        let note = self.projection.note.clone();
        v_flex()
            .id("code-workbench-right-rail")
            .size_full()
            .min_w_0()
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .h(px(48.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(mode.title()),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(mode == RightRailMode::Files, |this| {
                                this.child(self.render_file_header_actions(cx))
                            })
                            .child(
                                Button::new("close-right-rail")
                                    .ghost()
                                    .w_8()
                                    .px_0()
                                    .child(
                                        Icon::default()
                                            .path("icons/vibex/chevrons-right.svg")
                                            .size(px(20.0)),
                                    )
                                    .tooltip(locale::text("Close panel", "关闭面板", "關閉面板"))
                                    .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx))),
                            ),
                    ),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(locale::localize_error_message(&error)),
                )
            })
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(locale::localize_ui_message(&note)),
                )
            })
            .child(match mode {
                RightRailMode::Files => self.render_files(cx),
                RightRailMode::Git => self.render_git(cx),
            })
    }
}

pub struct CodeWorkbenchFixture {
    kind: CodeWorkbenchFixtureKind,
    workbench: Entity<CodeWorkbench>,
    right_rail: Entity<CodeRightRail>,
}

impl CodeWorkbenchFixture {
    pub fn new(
        kind: CodeWorkbenchFixtureKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workbench = cx.new(|cx| CodeWorkbench::fixture(kind, window, cx));
        let right_rail = cx.new(|cx| CodeRightRail::new(workbench.clone(), window, cx));
        Self {
            kind,
            workbench,
            right_rail,
        }
    }
}

impl Render for CodeWorkbenchFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = f32::from(window.viewport_size().width) < 760.0;
        let content = if compact {
            match self.kind {
                CodeWorkbenchFixtureKind::Files => div()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        self.right_rail
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    )
                    .into_any_element(),
                CodeWorkbenchFixtureKind::Diff | CodeWorkbenchFixtureKind::Markdown => div()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(self.workbench.clone())
                    .into_any_element(),
            }
        } else {
            h_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .min_h_0()
                        .child(self.workbench.clone()),
                )
                .child(
                    div()
                        .w(px(360.0))
                        .h_full()
                        .flex_none()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(
                            self.right_rail
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        ),
                )
                .into_any_element()
        };
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(content)
    }
}

fn rail_empty(message: impl Into<SharedString>, cx: &Context<CodeRightRail>) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .text_center()
        .child(Icon::new(IconName::Inbox))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message.into()),
        )
        .into_any_element()
}

// Mirrors the Tauri rail's EmptyPanel: a centered card with title + description.
fn rail_empty_card(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &Context<CodeRightRail>,
) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .items_center()
        .justify_center()
        .p_4()
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .max_w(px(384.0))
                .gap_1p5()
                .rounded(px(12.0))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .px_6()
                .py_6()
                .text_center()
                .child(div().font_semibold().child(title.into()))
                .child(
                    div()
                        .text_sm()
                        .line_height(gpui::relative(1.5))
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        .into_any_element()
}

fn file_tree_guides(depth: usize, cx: &Context<CodeRightRail>) -> Vec<AnyElement> {
    (0..depth)
        .map(|index| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(index as f32 * FILE_TREE_INDENT + FILE_TREE_GUIDE_OFFSET))
                .w(px(1.0))
                .bg(cx.theme().border.opacity(0.70))
                .into_any_element()
        })
        .collect()
}

fn file_tree_icon<T>(kind: FileIconKind, ignored: bool, cx: &Context<T>) -> AnyElement {
    file_tree_icon_sized(kind, ignored, px(16.0), cx)
}

fn file_tree_icon_sized<T>(
    kind: FileIconKind,
    ignored: bool,
    size: gpui::Pixels,
    cx: &Context<T>,
) -> AnyElement {
    let token = match kind {
        FileIconKind::Archive => "right-rail-file-icon-archive",
        FileIconKind::Audio => "right-rail-file-icon-audio",
        FileIconKind::Code | FileIconKind::Java | FileIconKind::Rust | FileIconKind::TypeScript => {
            "right-rail-file-icon-code"
        }
        FileIconKind::Config => "right-rail-file-icon-config",
        FileIconKind::Database | FileIconKind::Spreadsheet => "right-rail-file-icon-data",
        FileIconKind::Directory => "right-rail-file-icon-directory",
        FileIconKind::Font => "right-rail-file-icon-font",
        FileIconKind::Image | FileIconKind::Svg => "right-rail-file-icon-image",
        FileIconKind::Json => "right-rail-file-icon-json",
        FileIconKind::Lock => "right-rail-file-icon-lock",
        FileIconKind::Markdown => "right-rail-file-icon-markdown",
        FileIconKind::Markup => "right-rail-file-icon-markup",
        FileIconKind::JavaScript | FileIconKind::Script => "right-rail-file-icon-script",
        FileIconKind::Secret => "right-rail-file-icon-secret",
        FileIconKind::Style => "right-rail-file-icon-style",
        FileIconKind::Symlink => "right-rail-file-icon-symlink",
        FileIconKind::Video => "right-rail-file-icon-video",
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => "right-rail-file-icon-text",
    };
    let mut color = crate::theme::semantic_color(token, cx.theme().is_dark());
    if ignored {
        color = color.opacity(0.35);
    }
    match kind {
        FileIconKind::Directory => Icon::new(IconName::Folder)
            .small()
            .text_color(color)
            .into_any_element(),
        FileIconKind::Code | FileIconKind::Rust | FileIconKind::TypeScript => {
            file_tree_asset_icon("icons/vibex/file-code.svg", size, color)
        }
        FileIconKind::Java => file_tree_asset_icon("icons/vibex/coffee.svg", size, color),
        FileIconKind::JavaScript => file_tree_asset_icon("icons/vibex/file-code.svg", size, color),
        FileIconKind::Script => file_tree_asset_icon("icons/vibex/file-terminal.svg", size, color),
        FileIconKind::Json => file_tree_asset_icon("icons/vibex/file-braces.svg", size, color),
        FileIconKind::Markdown => {
            file_tree_asset_icon("icons/vibex/book-open-text.svg", size, color)
        }
        FileIconKind::Image => file_tree_asset_icon("icons/vibex/image.svg", size, color),
        FileIconKind::Svg => file_tree_asset_icon("icons/vibex/code-xml.svg", size, color),
        FileIconKind::Archive => file_tree_asset_icon("icons/vibex/file-archive.svg", size, color),
        FileIconKind::Database => file_tree_asset_icon("icons/vibex/database.svg", size, color),
        FileIconKind::Spreadsheet => {
            file_tree_asset_icon("icons/vibex/file-spreadsheet.svg", size, color)
        }
        FileIconKind::Style => file_tree_asset_icon("icons/vibex/hash.svg", size, color),
        FileIconKind::Markup => file_tree_asset_icon("icons/vibex/code-xml.svg", size, color),
        FileIconKind::Audio => file_tree_asset_icon("icons/vibex/audio-lines.svg", size, color),
        FileIconKind::Video => {
            file_tree_asset_icon("icons/vibex/file-video-camera.svg", size, color)
        }
        FileIconKind::Symlink => file_tree_asset_icon("icons/vibex/file-symlink.svg", size, color),
        FileIconKind::Config => file_tree_asset_icon("icons/vibex/file-cog.svg", size, color),
        FileIconKind::Lock => file_tree_asset_icon("icons/vibex/file-lock.svg", size, color),
        FileIconKind::Secret => file_tree_asset_icon("icons/vibex/file-key.svg", size, color),
        FileIconKind::Font => file_tree_asset_icon("icons/vibex/file-type.svg", size, color),
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => file_tree_asset_icon("icons/vibex/file-text.svg", size, color),
    }
}

fn file_tree_text_color(row: &FileExplorerRow, cx: &Context<CodeRightRail>) -> Hsla {
    if row.ignored {
        return cx.theme().sidebar_foreground.opacity(0.42);
    }
    match row.git.map(|git| git.signal) {
        Some(vibex_desktop_model::FileGitSignal::Added) => {
            crate::theme::semantic_color("right-rail-status-added", cx.theme().is_dark())
        }
        Some(vibex_desktop_model::FileGitSignal::Untracked) => {
            crate::theme::semantic_color("right-rail-status-untracked", cx.theme().is_dark())
        }
        Some(vibex_desktop_model::FileGitSignal::Ignored) => {
            cx.theme().sidebar_foreground.opacity(0.42)
        }
        Some(_) => crate::theme::semantic_color("right-rail-status-modified", cx.theme().is_dark()),
        None => cx.theme().sidebar_foreground,
    }
}

fn git_selection_indicator(
    state: GitPathSelectionState,
    cx: &Context<CodeRightRail>,
) -> AnyElement {
    let selected = state != GitPathSelectionState::Unchecked;
    div()
        .size(px(14.0))
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(if selected {
            cx.theme().accent_foreground.opacity(0.18)
        } else {
            cx.theme().input
        })
        .bg(if selected {
            cx.theme().accent
        } else {
            cx.theme().transparent
        })
        .text_color(cx.theme().accent_foreground.opacity(0.72))
        .when(state == GitPathSelectionState::Checked, |this| {
            this.child(Icon::new(IconName::Check).size(px(10.0)))
        })
        .when(state == GitPathSelectionState::Indeterminate, |this| {
            this.child(Icon::new(IconName::Minus).size(px(10.0)))
        })
        .into_any_element()
}

fn git_change_text_color(change: &GitChange, cx: &Context<CodeRightRail>) -> Hsla {
    match change.kind {
        GitChangeKind::Deleted => cx.theme().muted_foreground,
        GitChangeKind::Untracked => {
            crate::theme::semantic_color("right-rail-status-untracked", cx.theme().is_dark())
        }
        GitChangeKind::Added if !change.staged => {
            crate::theme::semantic_color("right-rail-status-untracked", cx.theme().is_dark())
        }
        GitChangeKind::Added => {
            crate::theme::semantic_color("right-rail-status-added", cx.theme().is_dark())
        }
        GitChangeKind::Modified
        | GitChangeKind::Renamed
        | GitChangeKind::Copied
        | GitChangeKind::TypeChanged => {
            crate::theme::semantic_color("right-rail-status-modified", cx.theme().is_dark())
        }
        GitChangeKind::Unmerged | GitChangeKind::Unknown => {
            crate::theme::semantic_color("right-rail-status-modified", cx.theme().is_dark())
        }
    }
}

fn git_change_label(kind: GitChangeKind) -> &'static str {
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

fn git_branch_is_remote(branch: &str, remote_names: &BTreeSet<&str>) -> bool {
    remote_names
        .iter()
        .any(|remote| branch == *remote || branch.starts_with(&format!("{remote}/")))
}

fn git_history_branch_choices(
    branches: &[GitBranchSummary],
    remotes: &[GitRemoteSummary],
) -> Vec<GitHistoryBranchChoice> {
    let remote_names = remotes
        .iter()
        .map(|remote| remote.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut choices = branches
        .iter()
        .map(|branch| GitHistoryBranchChoice {
            name: branch.name.clone(),
            // Local branches with an upstream already have the exact fetch/push
            // divergence. Remote or detached refs fall back to their divergence
            // from the current branch.
            incoming: if branch.upstream.is_some() || branch.current {
                branch.behind
            } else {
                branch.current_ahead
            },
            outgoing: if branch.upstream.is_some() || branch.current {
                branch.ahead
            } else {
                branch.current_behind
            },
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        let left_current = branches
            .iter()
            .find(|branch| branch.name == left.name)
            .is_some_and(|branch| branch.current);
        let right_current = branches
            .iter()
            .find(|branch| branch.name == right.name)
            .is_some_and(|branch| branch.current);
        let left_remote = git_branch_is_remote(&left.name, &remote_names);
        let right_remote = git_branch_is_remote(&right.name, &remote_names);
        right_current
            .cmp(&left_current)
            .then_with(|| left_remote.cmp(&right_remote))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    choices
}

fn git_history_author_choices(authors: &[GitHistoryAuthor]) -> Vec<GitHistoryAuthorChoice> {
    let mut choices = vec![GitHistoryAuthorChoice {
        label: locale::text("All Users", "所有用户", "所有使用者").to_string(),
        value: None,
    }];
    choices.extend(authors.iter().map(|author| GitHistoryAuthorChoice {
        label: format!("{} <{}>", author.name, author.email),
        value: Some(format!("{} <{}>", author.name, author.email)),
    }));
    choices
}

fn local_date_bounds(date: chrono::NaiveDate) -> (i64, i64) {
    use chrono::TimeZone as _;
    let start = date
        .and_hms_opt(0, 0, 0)
        .and_then(|value| chrono::Local.from_local_datetime(&value).single())
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|| {
            date.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        });
    let end_date = date.succ_opt().unwrap_or(date);
    let end = end_date
        .and_hms_opt(0, 0, 0)
        .and_then(|value| chrono::Local.from_local_datetime(&value).single())
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|| {
            end_date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        });
    (start, end)
}

fn git_history_filter_date(after_ms: Option<i64>, before_ms: Option<i64>) -> Date {
    let start = after_ms.and_then(|value| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
            .map(|value| value.with_timezone(&chrono::Local).date_naive())
    });
    let end = before_ms.and_then(|value| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value.saturating_sub(1))
            .map(|value| value.with_timezone(&chrono::Local).date_naive())
    });
    Date::Range(start, end)
}

fn git_history_date_bounds(date: Date) -> (Option<i64>, Option<i64>) {
    let start = date.start().map(local_date_bounds).map(|(start, _)| start);
    let end = date.end().map(local_date_bounds).map(|(_, end)| end);
    (start, end)
}

fn git_history_drawer_should_close(height: f32) -> bool {
    height <= GIT_HISTORY_DRAWER_CLOSE_THRESHOLD
}

fn git_history_relative_time(authored_at_ms: Option<i64>) -> String {
    git_history_relative_time_at(
        authored_at_ms,
        chrono::Local::now(),
        locale::current_locale(),
    )
}

fn git_history_relative_time_at(
    authored_at_ms: Option<i64>,
    now: chrono::DateTime<chrono::Local>,
    resolved_locale: locale::ResolvedLocale,
) -> String {
    let Some(timestamp) = authored_at_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.with_timezone(&chrono::Local))
    else {
        return match resolved_locale {
            locale::ResolvedLocale::En => "Unknown",
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => "未知",
        }
        .to_string();
    };
    let age = now.signed_duration_since(timestamp);
    let seconds = age.num_seconds().max(0);
    let day_delta = now
        .date_naive()
        .signed_duration_since(timestamp.date_naive())
        .num_days();
    if seconds < 60 {
        match resolved_locale {
            locale::ResolvedLocale::En => format!("{seconds}s ago"),
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
                format!("{seconds}秒前")
            }
        }
    } else if seconds < 3_600 {
        let minutes = seconds / 60;
        match resolved_locale {
            locale::ResolvedLocale::En => format!("{minutes}m ago"),
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
                format!("{minutes}分钟前")
            }
        }
    } else if seconds < 6 * 3_600 {
        let hours = seconds / 3_600;
        match resolved_locale {
            locale::ResolvedLocale::En => format!("{hours}h ago"),
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
                format!("{hours}小时前")
            }
        }
    } else if day_delta == 0 {
        let relative = match resolved_locale {
            locale::ResolvedLocale::En => "today",
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => "今天",
        };
        format!("{relative} {}", timestamp.format("%H:%M"))
    } else if day_delta == 1 {
        let relative = match resolved_locale {
            locale::ResolvedLocale::En => "yesterday",
            locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => "昨天",
        };
        format!("{relative} {}", timestamp.format("%H:%M"))
    } else {
        format!(
            "{} {}",
            git_history_date_label(timestamp, resolved_locale),
            timestamp.format("%H:%M")
        )
    }
}

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

fn git_history_date_label(
    timestamp: chrono::DateTime<chrono::Local>,
    resolved_locale: locale::ResolvedLocale,
) -> String {
    match resolved_locale {
        locale::ResolvedLocale::En => timestamp.format("%Y-%m-%d").to_string(),
        locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
            timestamp.format("%Y年%-m月%-d日").to_string()
        }
    }
}

fn git_commit_authored_at(authored_at_ms: Option<i64>) -> String {
    let Some(timestamp) = authored_at_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.with_timezone(&chrono::Local))
    else {
        return locale::text("Unknown", "未知", "未知").to_string();
    };
    match locale::current_locale() {
        locale::ResolvedLocale::En => timestamp.format("%b %-d, %Y, %H:%M").to_string(),
        locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => {
            timestamp.format("%Y年%-m月%-d日 %H:%M").to_string()
        }
    }
}

fn git_commit_placeholder(commit_type: &str) -> String {
    format!("{commit_type}: commit message")
}

fn normalize_git_commit_message(commit_type: &str, message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        return String::new();
    }
    let lower = message.to_ascii_lowercase();
    if GIT_COMMIT_TYPES.iter().any(|candidate| {
        lower
            .strip_prefix(candidate)
            .is_some_and(conventional_commit_suffix)
    }) {
        message.to_string()
    } else {
        format!("{commit_type}: {message}")
    }
}

fn conventional_commit_suffix(suffix: &str) -> bool {
    let suffix = if let Some(scoped) = suffix.strip_prefix('(') {
        let Some(end) = scoped.find(')') else {
            return false;
        };
        &scoped[end + 1..]
    } else {
        suffix
    };
    let suffix = suffix.strip_prefix('!').unwrap_or(suffix);
    suffix
        .strip_prefix(':')
        .is_some_and(|message| message.chars().next().is_some_and(char::is_whitespace))
}

fn file_tree_row_width_score(row: &FileExplorerRow) -> usize {
    let name_units = if row.segments.is_empty() {
        display_width_units(&row.name)
    } else {
        row.segments
            .iter()
            .map(|segment| display_width_units(&segment.name))
            .sum::<usize>()
            .saturating_add(row.segments.len().saturating_sub(1).saturating_mul(3))
    };
    row.depth
        .saturating_mul(FILE_TREE_INDENT as usize)
        .saturating_add(name_units.saturating_mul(8))
        .saturating_add(48)
}

fn display_width_units(value: &str) -> usize {
    value
        .chars()
        .map(|character| usize::from(!character.is_ascii()).saturating_add(1))
        .sum()
}

fn file_name_match_range(name: &str, query: &str) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let mut lowered = String::new();
    let mut source_ranges = Vec::new();
    let mut chars = name.char_indices().peekable();
    while let Some((start, character)) = chars.next() {
        let end = chars.peek().map(|(index, _)| *index).unwrap_or(name.len());
        for lowered_character in character.to_lowercase() {
            let lowered_start = lowered.len();
            lowered.push(lowered_character);
            source_ranges
                .extend((lowered_start..lowered.len()).map(|_| std::ops::Range { start, end }));
        }
    }
    let query = query.to_lowercase();
    let match_start = lowered.find(&query)?;
    let match_end = match_start.saturating_add(query.len());
    let source_start = source_ranges.get(match_start)?.start;
    let source_end = source_ranges.get(match_end.saturating_sub(1))?.end;
    Some(source_start..source_end)
}

fn file_search_reveal_range(
    content: &str,
    query: &str,
    line: u32,
    match_range: Option<std::ops::Range<usize>>,
) -> Option<std::ops::Range<usize>> {
    let target_line = line.saturating_sub(1) as usize;
    let mut line_start = 0_usize;
    for (index, segment) in content.split_inclusive('\n').enumerate() {
        if index == target_line {
            let line_text = segment.trim_end_matches(['\r', '\n']);
            let range = match_range
                .filter(|range| {
                    range.start < range.end
                        && range.end <= line_text.len()
                        && line_text.is_char_boundary(range.start)
                        && line_text.is_char_boundary(range.end)
                })
                .or_else(|| file_name_match_range(line_text, query))?;
            return Some(line_start + range.start..line_start + range.end);
        }
        line_start = line_start.saturating_add(segment.len());
    }
    None
}

fn center_input_line(input: &mut InputState, line: u32, cx: &mut Context<InputState>) {
    let (Some(visible_rows), Some(line_height)) = (input.visible_row_range(), input.line_height())
    else {
        return;
    };
    let target_row = line.saturating_sub(1) as usize;
    let top_row = target_row.saturating_sub(visible_rows.len().saturating_sub(1) / 2);
    let mut offset = input.scroll_offset();
    offset.y = -(line_height * top_row);
    input.set_scroll_offset(offset, cx);
}

fn open_tool_element(tool_id: &str, size: gpui::Pixels, cx: &gpui::App) -> AnyElement {
    if let Some(icon) = open_tool_brand_icon(tool_id, size, cx.theme().foreground) {
        return icon;
    }
    let icon = match tool_id {
        FILE_MANAGER_OPEN_TOOL_ID => Icon::new(IconName::FolderOpen),
        NATIVE_TERMINAL_OPEN_TOOL_ID => Icon::new(IconName::SquareTerminal),
        _ => Icon::default().path("icons/vibex/file-code.svg"),
    };
    icon.size(size).into_any_element()
}

fn open_tool_menu_section_label(label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        div()
            .w_full()
            .ml(px(-16.0))
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    })
    .disabled(true)
}

fn open_tool_menu_item(tool_id: &'static str, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex()
            .w_full()
            .ml(px(-16.0))
            .gap_2()
            .child(
                h_flex()
                    .size_4()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .child(open_tool_element(tool_id, px(16.0), cx)),
            )
            .child(label)
    })
}

fn render_file_name_match(
    name: &str,
    query: &str,
    color: Hsla,
    cx: &Context<CodeRightRail>,
) -> AnyElement {
    render_highlighted_text_range(name, file_name_match_range(name, query), color, cx)
}

fn render_highlighted_text_range(
    text: &str,
    range: Option<std::ops::Range<usize>>,
    color: Hsla,
    cx: &Context<CodeRightRail>,
) -> AnyElement {
    let Some(range) = range.filter(|range| {
        range.start < range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
    }) else {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(color)
            .child(text.to_string())
            .into_any_element();
    };
    h_flex()
        .flex_none()
        .whitespace_nowrap()
        .text_color(color)
        .child(text[..range.start].to_string())
        .child(
            div()
                .rounded(px(3.0))
                .border_1()
                .border_color(cx.theme().primary.opacity(0.25))
                .px(px(2.0))
                .bg(cx.theme().primary.opacity(0.25))
                .text_color(cx.theme().foreground)
                .child(text[range.clone()].to_string()),
        )
        .child(text[range.end..].to_string())
        .into_any_element()
}

fn tab_label(target: &PreviewTarget) -> String {
    match target {
        PreviewTarget::File { path } | PreviewTarget::GitDiff { path, .. } => Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_string(),
        PreviewTarget::Terminal { terminal_id } => {
            format!("{} {terminal_id}", locale::text("Terminal", "终端", "終端"))
        }
        PreviewTarget::GitCommit {
            commit_hash,
            subject,
            ..
        } => subject
            .clone()
            .unwrap_or_else(|| commit_hash.chars().take(8).collect()),
    }
}

fn git_diff_tab_id(key: &GitSelectionKey) -> String {
    format!(
        "git:{}:{}",
        if key.staged { "staged" } else { "unstaged" },
        key.path
    )
}

fn git_commit_tab_id(hash: &str) -> String {
    format!("git-commit:{hash}")
}

fn preview_target_icon(target: &PreviewTarget, cx: &Context<CodeWorkbench>) -> AnyElement {
    match target {
        PreviewTarget::File { path } => {
            let name = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path);
            let descriptor = file_icon_descriptor(name, FileEntryKind::File);
            file_tree_icon_sized(descriptor.kind, false, px(14.0), cx)
        }
        PreviewTarget::GitDiff { .. } => Icon::default()
            .path("icons/vibex/git-branch.svg")
            .size(px(14.0))
            .into_any_element(),
        PreviewTarget::GitCommit { .. } => Icon::default()
            .path("icons/vibex/hash.svg")
            .size(px(14.0))
            .into_any_element(),
        PreviewTarget::Terminal { .. } => Icon::new(IconName::SquareTerminal)
            .size(px(14.0))
            .into_any_element(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewTabVisualStatus {
    Added,
    Modified,
    Untracked,
    Deleted,
}

fn preview_tab_visual_status(
    target: &PreviewTarget,
    status: Option<&GitStatusSummary>,
) -> Option<PreviewTabVisualStatus> {
    let changes = &status?.changes;
    match target {
        PreviewTarget::File { path } => changes
            .iter()
            .filter(|change| preview_paths_match(&change.path, path))
            .map(file_preview_visual_status)
            .reduce(|current, next| {
                if current == next {
                    current
                } else {
                    PreviewTabVisualStatus::Modified
                }
            }),
        PreviewTarget::GitDiff { path, staged } => changes
            .iter()
            .find(|change| {
                preview_paths_match(&change.path, path)
                    && if *staged {
                        change.staged
                    } else {
                        change.unstaged || (!change.staged && !change.unstaged)
                    }
            })
            .map(git_preview_visual_status),
        PreviewTarget::GitCommit { .. } | PreviewTarget::Terminal { .. } => None,
    }
}

fn preview_paths_match(left: &str, right: &str) -> bool {
    normalized_relative_path(left) == normalized_relative_path(right)
}

fn file_preview_visual_status(change: &GitChange) -> PreviewTabVisualStatus {
    if change.kind == GitChangeKind::Untracked
        || (change.kind == GitChangeKind::Added && !change.staged)
    {
        PreviewTabVisualStatus::Untracked
    } else if change.kind == GitChangeKind::Added && change.staged {
        PreviewTabVisualStatus::Added
    } else {
        PreviewTabVisualStatus::Modified
    }
}

fn git_preview_visual_status(change: &GitChange) -> PreviewTabVisualStatus {
    if change.kind == GitChangeKind::Deleted {
        PreviewTabVisualStatus::Deleted
    } else {
        file_preview_visual_status(change)
    }
}

fn preview_status_color<T>(status: PreviewTabVisualStatus, cx: &Context<T>) -> Hsla {
    match status {
        PreviewTabVisualStatus::Added => {
            crate::theme::semantic_color("right-rail-status-added", cx.theme().is_dark())
        }
        PreviewTabVisualStatus::Untracked => {
            crate::theme::semantic_color("right-rail-status-untracked", cx.theme().is_dark())
        }
        PreviewTabVisualStatus::Modified => {
            crate::theme::semantic_color("right-rail-status-modified", cx.theme().is_dark())
        }
        PreviewTabVisualStatus::Deleted => cx.theme().muted_foreground,
    }
}

fn tab_accessible_label(tab: &PreviewTab, dirty: bool) -> String {
    format!(
        "{}{}{}{}",
        tab_label(&tab.target),
        if tab.pinned {
            locale::text(", pinned", "，已固定", "，已固定")
        } else {
            ""
        },
        if tab.temporary {
            locale::text(", temporary", "，临时", "，暫時")
        } else {
            ""
        },
        if dirty {
            locale::text(", dirty", "，未保存", "，未儲存")
        } else {
            ""
        }
    )
}

fn parse_file_markdown(source: &str, path: &str) -> Arc<MarkdownDocument> {
    let policy = ResourcePolicy::for_file(path);
    let mut digest = Sha256::new();
    digest.update(path.as_bytes());
    digest.update([0]);
    digest.update(source.as_bytes());
    let digest = digest.finalize();
    let revision = u64::from_le_bytes(digest[..8].try_into().unwrap_or_default());
    Arc::new(parse_markdown(
        MarkdownInput::new(source, policy.base_path(), revision)
            .surface(MarkdownSurface::FilePreview),
    ))
}

fn language_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
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
        Some("html") => "html",
        Some("sh" | "bash") => "bash",
        _ => "text",
    }
}

fn image_format_for_path(path: &str) -> Option<ImageFormat> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

fn image_mime_for_path(path: &str) -> Option<&'static str> {
    image_format_for_path(path).map(ImageFormat::mime_type)
}

fn surface_kind(target: &PreviewTarget) -> ContentSurfaceKind {
    match target {
        PreviewTarget::File { path } => match content_preview_kind_for_path(path) {
            ContentPreviewKind::TextEditor => ContentSurfaceKind::Text,
            ContentPreviewKind::Markdown => ContentSurfaceKind::Markdown,
            ContentPreviewKind::Image => ContentSurfaceKind::Image,
            ContentPreviewKind::MediaExternalOnly => ContentSurfaceKind::Media,
            ContentPreviewKind::Pdf => ContentSurfaceKind::Pdf,
            ContentPreviewKind::Office => ContentSurfaceKind::Office,
            ContentPreviewKind::UnsupportedBinary => ContentSurfaceKind::Text,
        },
        PreviewTarget::Terminal { .. } => ContentSurfaceKind::Terminal,
        PreviewTarget::GitDiff { .. } => ContentSurfaceKind::GitDiff,
        PreviewTarget::GitCommit { .. } => ContentSurfaceKind::GitCommit,
    }
}

fn editor_status(buffer: &vibex_desktop_model::EditorBufferModel) -> String {
    match (
        &buffer.availability,
        &buffer.external,
        buffer.pending_save.is_some(),
        buffer.dirty,
    ) {
        (EditorBufferAvailability::LargeFileReadOnly, _, _, _) => {
            locale::text("Large file - read only", "大文件 - 只读", "大型檔案 - 唯讀").into()
        }
        (EditorBufferAvailability::BinaryReadOnly, _, _, _) => locale::text(
            "Binary - read only",
            "二进制文件 - 只读",
            "二進位檔案 - 唯讀",
        )
        .into(),
        (EditorBufferAvailability::Missing, _, _, _) => locale::text(
            "Deleted - recovery buffer",
            "已删除 - 恢复缓冲区",
            "已刪除 - 復原緩衝區",
        )
        .into(),
        (_, EditorExternalState::Changed { .. }, _, _) => {
            locale::text("External conflict", "外部更改冲突", "外部變更衝突").into()
        }
        (_, EditorExternalState::VerificationRequired, _, _) => {
            locale::text("Verification required", "需要验证", "需要驗證").into()
        }
        (_, _, true, _) => locale::text("Saving", "正在保存", "正在儲存").into(),
        (_, _, _, true) => locale::text("Modified", "已修改", "已修改").into(),
        _ => locale::text("Saved", "已保存", "已儲存").into(),
    }
}

fn preview_badge(label: impl Into<SharedString>, cx: &Context<CodeWorkbench>) -> AnyElement {
    h_flex()
        .h(px(20.0))
        .flex_none()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.82))
        .px_2()
        .text_xs()
        .text_color(cx.theme().foreground)
        .child(label.into())
        .into_any_element()
}

fn preview_destructive_badge(
    label: impl Into<SharedString>,
    cx: &Context<CodeWorkbench>,
) -> AnyElement {
    h_flex()
        .h(px(20.0))
        .flex_none()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(cx.theme().danger.opacity(0.72))
        .bg(cx.theme().danger)
        .px_2()
        .text_xs()
        .text_color(cx.theme().danger_foreground)
        .child(label.into())
        .into_any_element()
}

fn render_truncated_alert(truncated: bool, cx: &Context<CodeWorkbench>) -> AnyElement {
    if !truncated {
        return div().h(px(0.0)).flex_none().into_any_element();
    }
    h_flex()
        .flex_none()
        .items_start()
        .gap_2()
        .m_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(cx.theme().warning.opacity(0.45))
        .bg(cx.theme().warning.opacity(0.08))
        .p_3()
        .child(
            Icon::new(IconName::TriangleAlert)
                .size(px(16.0))
                .text_color(cx.theme().warning),
        )
        .child(
            v_flex()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_medium().child("Patch truncated"))
                .child(
                    div()
                        .whitespace_normal()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("This preview shows the first bounded portion of the Git patch."),
                ),
        )
        .into_any_element()
}

fn render_commit_patch_row(
    row: GitCommitPatchRow,
    hash: String,
    code_font_family: String,
    diff_row_height: f32,
    cx: &Context<CodeWorkbench>,
) -> AnyElement {
    match row {
        GitCommitPatchRow::FileHeader {
            file_index,
            path,
            original_path,
            additions,
            deletions,
            collapsed,
        } => {
            let label = original_path
                .as_deref()
                .filter(|original| *original != path)
                .map(|original| format!("{path}  from {original}"))
                .unwrap_or_else(|| path.clone());
            let click_hash = hash.clone();
            let click_path = path.clone();
            let key_hash = hash.clone();
            let key_path = path.clone();
            h_flex()
                .id(format!("commit-file:{hash}:{file_index}"))
                .h(px(diff_row_height))
                .w_full()
                .flex_none()
                .min_w_0()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.55))
                .bg(cx.theme().muted.opacity(0.30))
                .cursor_pointer()
                .focusable()
                .tab_stop(true)
                .role(Role::Button)
                .aria_expanded(!collapsed)
                .aria_label(format!("{}; +{} -{}", path, additions, deletions))
                .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.65)))
                .child(
                    h_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_2()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(14.0))
                            .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_semibold()
                                .child(label),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .font_family(code_font_family)
                        .text_size(cx.theme().mono_font_size)
                        .font_weight(code_font_weight(cx))
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{additions}")),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("-{deletions}")),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.git.toggle_commit_file(&click_hash, &click_path) {
                        cx.notify();
                    }
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key != "enter" && event.keystroke.key != "space" {
                        return;
                    }
                    if this.git.toggle_commit_file(&key_hash, &key_path) {
                        cx.notify();
                    }
                    cx.stop_propagation();
                }))
                .into_any_element()
        }
        GitCommitPatchRow::Diff(row) => {
            render_diff_row(row, &code_font_family, diff_row_height, cx)
        }
        GitCommitPatchRow::Empty { file_index, path } => div()
            .id(format!("commit-file-empty:{hash}:{file_index}"))
            .h(px(diff_row_height))
            .flex_none()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.30))
            .bg(cx.theme().muted.opacity(0.10))
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!(
                "{} · {}",
                path,
                locale::text(
                    "No content changes in this file.",
                    "此文件没有内容变更。",
                    "此檔案沒有內容變更。",
                )
            ))
            .into_any_element(),
    }
}

fn render_diff_row(
    row: vibex_desktop_model::PreparedDiffRow,
    code_font_family: &str,
    diff_row_height: f32,
    cx: &Context<CodeWorkbench>,
) -> AnyElement {
    let background = match row.row.kind {
        UnifiedDiffLineKind::Add => cx.theme().success.opacity(0.10),
        UnifiedDiffLineKind::Delete => cx.theme().danger.opacity(0.10),
        UnifiedDiffLineKind::Hunk => cx.theme().info.opacity(0.10),
        UnifiedDiffLineKind::Meta => cx.theme().muted.opacity(0.30),
        UnifiedDiffLineKind::Context => cx.theme().background,
    };
    let foreground = match row.row.kind {
        UnifiedDiffLineKind::Add => cx.theme().success,
        UnifiedDiffLineKind::Delete => cx.theme().danger,
        UnifiedDiffLineKind::Hunk => cx.theme().info,
        UnifiedDiffLineKind::Meta => cx.theme().muted_foreground,
        UnifiedDiffLineKind::Context => cx.theme().foreground.opacity(0.85),
    };
    let prefix = match row.row.kind {
        UnifiedDiffLineKind::Add => "+",
        UnifiedDiffLineKind::Delete => "-",
        UnifiedDiffLineKind::Hunk => "",
        UnifiedDiffLineKind::Meta => " ",
        UnifiedDiffLineKind::Context => " ",
    };
    let content = if row.row.content.is_empty() {
        " ".to_string()
    } else {
        row.row.content
    };
    h_flex()
        .min_h(px(diff_row_height))
        .w_full()
        .flex_none()
        .min_w_0()
        .items_stretch()
        .bg(background)
        .font_family(code_font_family.to_string())
        .text_size(cx.theme().mono_font_size)
        .font_weight(code_font_weight(cx))
        .line_height(gpui::relative(1.5))
        .text_color(foreground)
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.30))
        .child(
            div()
                .w(px(64.0))
                .flex_none()
                .border_r_1()
                .border_color(cx.theme().border.opacity(0.30))
                .px_2()
                .py(px(DIFF_LINE_VERTICAL_PADDING))
                .text_right()
                .text_color(cx.theme().muted_foreground.opacity(0.70))
                .child(
                    row.row
                        .old_line
                        .map(|line| line.to_string())
                        .unwrap_or_default(),
                ),
        )
        .child(
            div()
                .w(px(64.0))
                .flex_none()
                .border_r_1()
                .border_color(cx.theme().border.opacity(0.30))
                .px_2()
                .py(px(DIFF_LINE_VERTICAL_PADDING))
                .text_right()
                .text_color(cx.theme().muted_foreground.opacity(0.70))
                .child(
                    row.row
                        .new_line
                        .map(|line| line.to_string())
                        .unwrap_or_default(),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .whitespace_normal()
                .px_3()
                .py(px(DIFF_LINE_VERTICAL_PADDING))
                .child(format!("{prefix}{content}")),
        )
        .into_any_element()
}

fn diff_row_height(code_font_size: u16) -> f32 {
    (f32::from(code_font_size.clamp(10, 24)) * 1.5 + DIFF_LINE_VERTICAL_PADDING * 2.0)
        .ceil()
        .max(DIFF_ROW_MIN_HEIGHT)
}

fn file_name_search_tree_rows(
    results: &[FileSearchResult],
    collapsed_paths: &BTreeSet<String>,
) -> Vec<FileNameSearchTreeRow> {
    let mut nodes = BTreeMap::<String, FileNameSearchTreeNode>::new();
    for result in results {
        let Some(path) = normalized_relative_path(&result.path) else {
            continue;
        };
        let segments = path.split('/').map(str::to_string).collect::<Vec<_>>();
        let mut current_path = String::new();
        for (index, segment) in segments.iter().enumerate() {
            if !current_path.is_empty() {
                current_path.push('/');
            }
            current_path.push_str(segment);
            let is_match = index + 1 == segments.len();
            let node =
                nodes
                    .entry(current_path.clone())
                    .or_insert_with(|| FileNameSearchTreeNode {
                        path: current_path.clone(),
                        name: segment.clone(),
                        kind: if is_match {
                            result.kind
                        } else {
                            FileEntryKind::Directory
                        },
                        matched: is_match,
                    });
            if is_match {
                node.name = if result.name.is_empty() {
                    segment.clone()
                } else {
                    result.name.clone()
                };
                node.kind = result.kind;
                node.matched = true;
            } else {
                node.kind = FileEntryKind::Directory;
            }
        }
    }

    let mut children = BTreeMap::<String, Vec<String>>::new();
    for path in nodes.keys() {
        children
            .entry(relative_parent_path(path).to_string())
            .or_default()
            .push(path.clone());
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|left, right| {
            let left = &nodes[left];
            let right = &nodes[right];
            let left_directory = left.kind == FileEntryKind::Directory;
            let right_directory = right.kind == FileEntryKind::Directory;
            right_directory
                .cmp(&left_directory)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.path.cmp(&right.path))
        });
    }

    let mut rows = Vec::with_capacity(nodes.len());
    append_file_name_search_tree_rows("", 0, &nodes, &children, collapsed_paths, &mut rows);
    rows
}

fn append_file_name_search_tree_rows(
    parent: &str,
    depth: usize,
    nodes: &BTreeMap<String, FileNameSearchTreeNode>,
    children: &BTreeMap<String, Vec<String>>,
    collapsed_paths: &BTreeSet<String>,
    rows: &mut Vec<FileNameSearchTreeRow>,
) {
    let Some(child_paths) = children.get(parent) else {
        return;
    };
    for child_path in child_paths {
        let node = &nodes[child_path];
        let has_children = children
            .get(child_path)
            .is_some_and(|children| !children.is_empty());
        let expanded = has_children && !collapsed_paths.contains(child_path);
        rows.push(FileNameSearchTreeRow {
            path: node.path.clone(),
            name: node.name.clone(),
            kind: node.kind,
            depth,
            matched: node.matched,
            has_children,
            expanded,
        });
        if expanded {
            append_file_name_search_tree_rows(
                child_path,
                depth.saturating_add(1),
                nodes,
                children,
                collapsed_paths,
                rows,
            );
        }
    }
}

fn normalized_relative_path(path: &str) -> Option<String> {
    let path = path
        .trim()
        .replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string();
    (!path.is_empty()).then_some(path)
}

fn relative_parent_path(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn relative_directory_path_chain(path: &str) -> Vec<String> {
    let mut current = String::new();
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(segment);
            current.clone()
        })
        .collect()
}

fn join_relative_path(parent: &str, name: &str) -> String {
    let parent = parent.trim_matches('/');
    let name = name.trim_matches('/');
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

fn inline_path_placeholder(action: Option<&InlineFileAction>) -> &'static str {
    match action {
        Some(InlineFileAction::CreateFile { .. }) => {
            locale::text("Type file name", "输入文件名", "輸入檔案名")
        }
        Some(InlineFileAction::CreateDirectory { .. }) => {
            locale::text("Type folder name", "输入文件夹名", "輸入資料夾名")
        }
        Some(InlineFileAction::Rename { .. }) => locale::text("New name", "新名称", "新名稱"),
        None => locale::text("File name", "文件名", "檔案名"),
    }
}

fn file_can_open_in_editor(path: &str, kind: FileEntryKind) -> bool {
    if kind != FileEntryKind::File {
        return false;
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    !matches!(
        extension.as_deref(),
        Some(
            "7z" | "a"
                | "aac"
                | "apk"
                | "app"
                | "avi"
                | "avif"
                | "bin"
                | "bmp"
                | "bz2"
                | "class"
                | "db"
                | "deb"
                | "dll"
                | "dmg"
                | "doc"
                | "docx"
                | "dylib"
                | "eot"
                | "exe"
                | "flac"
                | "gif"
                | "gz"
                | "heic"
                | "ico"
                | "iso"
                | "jar"
                | "jpeg"
                | "jpg"
                | "lib"
                | "m4a"
                | "mkv"
                | "mov"
                | "mp3"
                | "mp4"
                | "o"
                | "ods"
                | "otf"
                | "pdf"
                | "pfx"
                | "png"
                | "ppt"
                | "pptx"
                | "rar"
                | "rpm"
                | "so"
                | "sqlite"
                | "tar"
                | "tgz"
                | "tiff"
                | "ttf"
                | "wav"
                | "wasm"
                | "webm"
                | "webp"
                | "woff"
                | "woff2"
                | "xls"
                | "xlsx"
                | "xz"
                | "zip"
        )
    )
}

fn unique_copy_destination(parent: &str, name: &str, exists: impl Fn(&str) -> bool) -> String {
    let candidate = join_relative_path(parent, name);
    if !exists(&candidate) {
        return candidate;
    }
    let (stem, extension) = name
        .rfind('.')
        .filter(|index| *index > 0)
        .map(|index| (&name[..index], &name[index..]))
        .unwrap_or((name, ""));
    for index in 1..1000 {
        let suffix = if index == 1 {
            " copy".to_string()
        } else {
            format!(" copy {index}")
        };
        let candidate = join_relative_path(parent, &format!("{stem}{suffix}{extension}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    join_relative_path(parent, &format!("{stem} copy 1000{extension}"))
}

fn path_is_equal_or_descendant(candidate: &str, ancestor: &str) -> bool {
    candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn replace_path_prefix(path: &str, source: &str, destination: &str) -> String {
    if path == source {
        destination.to_string()
    } else {
        path.strip_prefix(source)
            .filter(|suffix| suffix.starts_with('/'))
            .map(|suffix| format!("{destination}{suffix}"))
            .unwrap_or_else(|| path.to_string())
    }
}

fn remap_preview_tab_id(tab_id: &str, source: &str, destination: &str) -> String {
    if let Some(path) = tab_id.strip_prefix("file:") {
        return format!("file:{}", replace_path_prefix(path, source, destination));
    }
    for prefix in ["git:staged:", "git:unstaged:"] {
        if let Some(path) = tab_id.strip_prefix(prefix) {
            return format!("{prefix}{}", replace_path_prefix(path, source, destination));
        }
    }
    tab_id.to_string()
}

fn preview_target_references_path(target: &PreviewTarget, path: &str) -> bool {
    match target {
        PreviewTarget::File { path: target } | PreviewTarget::GitDiff { path: target, .. } => {
            path_is_equal_or_descendant(target, path)
        }
        PreviewTarget::GitCommit { focus_path, .. } => focus_path
            .as_deref()
            .is_some_and(|target| path_is_equal_or_descendant(target, path)),
        PreviewTarget::Terminal { .. } => false,
    }
}

fn worktree_lifecycle_state_label(state: WorktreeLifecycleDisplayState) -> &'static str {
    match state {
        WorktreeLifecycleDisplayState::Working => locale::text("Working", "开发中", "開發中"),
        WorktreeLifecycleDisplayState::Reviewing => locale::text("Reviewing", "检查中", "檢查中"),
        WorktreeLifecycleDisplayState::Ready => locale::text("Ready", "可合并", "可合併"),
        WorktreeLifecycleDisplayState::Queued => locale::text("Queued", "排队中", "排隊中"),
        WorktreeLifecycleDisplayState::Merging => locale::text("Merging", "合并中", "合併中"),
        WorktreeLifecycleDisplayState::NeedsResolution => {
            locale::text("Needs resolution", "等待解决冲突", "等待解決衝突")
        }
        WorktreeLifecycleDisplayState::Aborting => locale::text("Aborting", "正在中止", "正在中止"),
        WorktreeLifecycleDisplayState::Archiving => locale::text("Archiving", "归档中", "封存中"),
        WorktreeLifecycleDisplayState::Archived => locale::text("Archived", "已归档", "已封存"),
        WorktreeLifecycleDisplayState::Restoring => locale::text("Restoring", "恢复中", "還原中"),
        WorktreeLifecycleDisplayState::Discarding => locale::text("Discarding", "丢弃中", "捨棄中"),
        WorktreeLifecycleDisplayState::Discarded => locale::text("Discarded", "已丢弃", "已捨棄"),
        WorktreeLifecycleDisplayState::Failed => locale::text("Failed", "失败", "失敗"),
        WorktreeLifecycleDisplayState::NeedsAttention => {
            locale::text("Needs attention", "需要处理", "需要處理")
        }
    }
}

fn worktree_lifecycle_state_color(
    state: WorktreeLifecycleDisplayState,
    cx: &Context<CodeRightRail>,
) -> Hsla {
    match state {
        WorktreeLifecycleDisplayState::Ready => cx.theme().success,
        WorktreeLifecycleDisplayState::Reviewing
        | WorktreeLifecycleDisplayState::Queued
        | WorktreeLifecycleDisplayState::Archiving
        | WorktreeLifecycleDisplayState::Restoring
        | WorktreeLifecycleDisplayState::Discarding => cx.theme().warning,
        WorktreeLifecycleDisplayState::Merging => cx.theme().primary,
        WorktreeLifecycleDisplayState::NeedsResolution
        | WorktreeLifecycleDisplayState::Aborting
        | WorktreeLifecycleDisplayState::Failed
        | WorktreeLifecycleDisplayState::NeedsAttention => cx.theme().danger,
        WorktreeLifecycleDisplayState::Working
        | WorktreeLifecycleDisplayState::Archived
        | WorktreeLifecycleDisplayState::Discarded => cx.theme().muted_foreground,
    }
}

fn localized_merge_action_label(target: &str) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("Merge to {}", bounded_ref_label(target)),
        locale::ResolvedLocale::ZhCn => format!("合并到 {}", bounded_ref_label(target)),
        locale::ResolvedLocale::ZhTw => format!("合併到 {}", bounded_ref_label(target)),
    }
}

fn localized_integration_action_label(target: &str, strategy: GitWorktreeMergeStrategy) -> String {
    if strategy == GitWorktreeMergeStrategy::RebaseAndMerge {
        return match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Rebase into {}", bounded_ref_label(target)),
            locale::ResolvedLocale::ZhCn => {
                format!("Rebase 到 {}", bounded_ref_label(target))
            }
            locale::ResolvedLocale::ZhTw => {
                format!("Rebase 到 {}", bounded_ref_label(target))
            }
        };
    }
    localized_merge_action_label(target)
}

fn merge_strategy_summary(strategy: GitWorktreeMergeStrategy) -> &'static str {
    match strategy {
        GitWorktreeMergeStrategy::NoFfMerge => locale::text(
            "Creates a merge commit.",
            "将创建 merge commit。",
            "將建立 merge commit。",
        ),
        GitWorktreeMergeStrategy::RebaseAndMerge => locale::text(
            "Rewrites the source commits onto the target, then fast-forwards without a merge commit.",
            "将源提交 rebase 到目标后快进，不产生 merge commit。",
            "將來源提交 rebase 到目標後快進，不產生 merge commit。",
        ),
        GitWorktreeMergeStrategy::Unknown => {
            locale::text("Unknown strategy.", "未知策略。", "未知策略。")
        }
    }
}

fn localized_conflict_title(source: &str, target: &str, rebase: bool) -> String {
    if rebase {
        return match locale::current_locale() {
            locale::ResolvedLocale::En => format!("Rebase paused: {source} -> {target}"),
            locale::ResolvedLocale::ZhCn => format!("Rebase 已暂停：{source} -> {target}"),
            locale::ResolvedLocale::ZhTw => format!("Rebase 已暫停：{source} -> {target}"),
        };
    }
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("Merge paused: {source} -> {target}"),
        locale::ResolvedLocale::ZhCn => format!("合并已暂停：{source} -> {target}"),
        locale::ResolvedLocale::ZhTw => format!("合併已暫停：{source} -> {target}"),
    }
}

fn localized_attention_title(source: &str, target: &str) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("Merge needs attention: {source} -> {target}"),
        locale::ResolvedLocale::ZhCn => format!("合并需要处理：{source} -> {target}"),
        locale::ResolvedLocale::ZhTw => format!("合併需要處理：{source} -> {target}"),
    }
}

fn localized_conflict_count(count: usize) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("{count} unresolved conflict(s)"),
        locale::ResolvedLocale::ZhCn => format!("{count} 个冲突未解决"),
        locale::ResolvedLocale::ZhTw => format!("{count} 個衝突未解決"),
    }
}

fn localized_source_delta(count: u32) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => {
            format!("{count} new source commit(s) are excluded from this merge")
        }
        locale::ResolvedLocale::ZhCn => format!("源分支新增 {count} 个提交，不包含在本次合并中"),
        locale::ResolvedLocale::ZhTw => format!("來源分支新增 {count} 個提交，不包含在本次合併中"),
    }
}

fn localized_conflict_render_limit(limit: usize, total: usize) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => {
            format!("Showing the first {limit} of {total} conflicts")
        }
        locale::ResolvedLocale::ZhCn => format!("显示 {total} 个冲突中的前 {limit} 个"),
        locale::ResolvedLocale::ZhTw => format!("顯示 {total} 個衝突中的前 {limit} 個"),
    }
}

fn localized_use_version_label(branch: &str) -> String {
    let branch = bounded_ref_label(branch);
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!("Use {branch}"),
        locale::ResolvedLocale::ZhCn => format!("使用 {branch}"),
        locale::ResolvedLocale::ZhTw => format!("使用 {branch}"),
    }
}

fn bounded_ref_label(value: &str) -> String {
    const MAX_CHARS: usize = 24;
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn worktree_conflict_kind_label(kind: GitWorktreeConflictKind, binary: bool) -> &'static str {
    if binary {
        return locale::text("Binary", "二进制", "二進位");
    }
    match kind {
        GitWorktreeConflictKind::BothModified => {
            locale::text("Both modified", "两边都修改", "兩邊都修改")
        }
        GitWorktreeConflictKind::BothAdded => {
            locale::text("Both added", "两边都新增", "兩邊都新增")
        }
        GitWorktreeConflictKind::DeletedBySource => {
            locale::text("Source deleted", "源分支删除", "來源分支刪除")
        }
        GitWorktreeConflictKind::DeletedByTarget => {
            locale::text("Target deleted", "目标分支删除", "目標分支刪除")
        }
        GitWorktreeConflictKind::Binary => locale::text("Binary", "二进制", "二進位"),
        GitWorktreeConflictKind::Other | GitWorktreeConflictKind::Unknown => {
            locale::text("Conflict", "冲突", "衝突")
        }
    }
}

fn worktree_confirmation_copy(
    confirmation: &WorktreeLifecycleConfirmation,
) -> (String, String, String, bool, bool, Vec<GitWorktreeRisk>) {
    match confirmation {
        WorktreeLifecycleConfirmation::Merge { plan, strategy } => {
            let summary = match locale::current_locale() {
                locale::ResolvedLocale::En => format!(
                    "{} -> {} at exact heads. {} commit(s), {} file(s), +{} / -{}. {} Agent(s) and {} terminal(s) remain independent. {}",
                    plan.source_branch,
                    plan.target_branch,
                    plan.summary.commit_count,
                    plan.summary.file_count,
                    plan.summary.additions,
                    plan.summary.deletions,
                    plan.running_consumers.agent_count,
                    plan.running_consumers.terminal_count,
                    merge_strategy_summary(*strategy),
                ),
                locale::ResolvedLocale::ZhCn => format!(
                    "{} -> {}，按固定提交集成。{} 个提交、{} 个文件、+{} / -{}。{} 个 Agent 和 {} 个终端保持独立运行。{}",
                    plan.source_branch,
                    plan.target_branch,
                    plan.summary.commit_count,
                    plan.summary.file_count,
                    plan.summary.additions,
                    plan.summary.deletions,
                    plan.running_consumers.agent_count,
                    plan.running_consumers.terminal_count,
                    merge_strategy_summary(*strategy),
                ),
                locale::ResolvedLocale::ZhTw => format!(
                    "{} -> {}，依固定提交整合。{} 個提交、{} 個檔案、+{} / -{}。{} 個 Agent 和 {} 個終端機保持獨立執行。{}",
                    plan.source_branch,
                    plan.target_branch,
                    plan.summary.commit_count,
                    plan.summary.file_count,
                    plan.summary.additions,
                    plan.summary.deletions,
                    plan.running_consumers.agent_count,
                    plan.running_consumers.terminal_count,
                    merge_strategy_summary(*strategy),
                ),
            };
            let title = if *strategy == GitWorktreeMergeStrategy::RebaseAndMerge {
                locale::text("Rebase Worktree", "Rebase Worktree", "Rebase Worktree")
            } else {
                locale::text("Merge Worktree", "合并 Worktree", "合併 Worktree")
            };
            (
                title.to_string(),
                summary,
                localized_integration_action_label(&plan.target_branch, *strategy),
                plan.preflight.allowed,
                false,
                plan.preflight.risks.clone(),
            )
        }
        WorktreeLifecycleConfirmation::Archive { preflight, .. } => (
            locale::text("Archive Worktree", "归档 Worktree", "封存 Worktree").to_string(),
            locale::text(
                "The Workspace, branch, Session history, and managed record are retained.",
                "将保留 Workspace、分支、Session 历史和托管记录。",
                "將保留 Workspace、分支、Session 歷史和受管理記錄。",
            )
            .to_string(),
            locale::text("Archive Worktree", "归档 Worktree", "封存 Worktree").to_string(),
            preflight.allowed,
            false,
            preflight.risks.clone(),
        ),
        WorktreeLifecycleConfirmation::Restore { preflight, .. } => (
            locale::text("Restore Worktree", "恢复 Worktree", "還原 Worktree").to_string(),
            locale::text(
                "Restore the original Workspace path, branch, and Session history.",
                "按原路径和分支恢复同一 Workspace 与 Session 历史。",
                "依原路徑和分支還原同一 Workspace 與 Session 歷史。",
            )
            .to_string(),
            locale::text("Restore Worktree", "恢复 Worktree", "還原 Worktree").to_string(),
            preflight.allowed,
            false,
            preflight.risks.clone(),
        ),
        WorktreeLifecycleConfirmation::Discard { preflight, .. } => (
            locale::text("Discard Worktree", "丢弃 Worktree", "捨棄 Worktree").to_string(),
            locale::text(
                "Remove the Worktree directory. The branch is not deleted; audit and Session history remain.",
                "移除 Worktree 目录。不会删除分支；审计和 Session 历史仍保留。",
                "移除 Worktree 目錄。不會刪除分支；稽核和 Session 歷史仍保留。",
            )
            .to_string(),
            locale::text("Discard Worktree", "丢弃 Worktree", "捨棄 Worktree").to_string(),
            preflight.allowed,
            true,
            preflight.risks.clone(),
        ),
        WorktreeLifecycleConfirmation::Continue(_) => (
            locale::text("Continue integration", "继续集成", "繼續整合").to_string(),
            locale::text(
                "Continue the active Git integration after revalidating its durable source and target state.",
                "重新校验持久化的源与目标状态后继续当前 Git 集成。",
                "重新驗證持久化的來源與目標狀態後繼續目前 Git 整合。",
            )
            .to_string(),
            locale::text("Continue integration", "继续集成", "繼續整合").to_string(),
            true,
            false,
            Vec::new(),
        ),
        WorktreeLifecycleConfirmation::Abort(_) => (
            locale::text("Abort integration", "中止集成", "中止整合").to_string(),
            locale::text(
                "Abort the active Git integration and restore the exact preflight source and target state.",
                "中止当前 Git 集成，并恢复预检时的精确源与目标状态。",
                "中止目前 Git 整合，並還原預檢時的精確來源與目標狀態。",
            )
            .to_string(),
            locale::text("Abort integration", "中止集成", "中止整合").to_string(),
            true,
            true,
            Vec::new(),
        ),
    }
}

fn worktree_risk_label(kind: GitWorktreeRiskKind) -> &'static str {
    match kind {
        GitWorktreeRiskKind::DirtySource => {
            locale::text("Source changes", "源目录改动", "來源目錄變更")
        }
        GitWorktreeRiskKind::DirtyTarget => {
            locale::text("Target changes", "目标目录改动", "目標目錄變更")
        }
        GitWorktreeRiskKind::SourceHeadChanged => {
            locale::text("Source changed", "源提交已变化", "來源提交已變更")
        }
        GitWorktreeRiskKind::TargetHeadChanged => {
            locale::text("Target changed", "目标提交已变化", "目標提交已變更")
        }
        GitWorktreeRiskKind::OwnershipMismatch => {
            locale::text("Ownership", "所有权不匹配", "所有權不符")
        }
        GitWorktreeRiskKind::ActiveOperation => {
            locale::text("Lifecycle operation", "生命周期操作", "生命週期操作")
        }
        GitWorktreeRiskKind::MissingGitRegistration => {
            locale::text("Git registration", "缺少 Git 注册", "缺少 Git 註冊")
        }
        GitWorktreeRiskKind::StaleReadiness => {
            locale::text("Readiness changed", "准备状态已变化", "準備狀態已變更")
        }
        GitWorktreeRiskKind::WrongTargetBranch => {
            locale::text("Target branch", "目标分支不匹配", "目標分支不符")
        }
        GitWorktreeRiskKind::ActiveGitOperation => {
            locale::text("Git operation", "存在 Git 操作", "存在 Git 操作")
        }
        GitWorktreeRiskKind::UnpushedCommits => {
            locale::text("Unpushed commits", "存在未推送提交", "存在未推送提交")
        }
        GitWorktreeRiskKind::RunningConsumers => {
            locale::text("Running activity", "仍有运行中任务", "仍有執行中工作")
        }
        GitWorktreeRiskKind::PathConflict => locale::text("Path conflict", "路径冲突", "路徑衝突"),
        GitWorktreeRiskKind::UnknownState | GitWorktreeRiskKind::Unknown => {
            locale::text("Unknown state", "状态未知", "狀態未知")
        }
    }
}

fn worktree_plan_error_requires_refresh(code: &str) -> bool {
    matches!(
        code,
        "worktree_preflight_stale"
            | "worktree_source_head_changed"
            | "worktree_target_head_changed"
            | "worktree_target_branch_changed"
            | "worktree_readiness_stale"
    )
}

fn bounded_uniform_range(
    range: std::ops::Range<usize>,
    total: usize,
    limit: usize,
) -> std::ops::Range<usize> {
    let start = range.start.min(total);
    let end = range.end.min(total).min(start.saturating_add(limit.max(1)));
    start..end
}

fn pane_tab_ids(node: &PreviewSplitNode, pane_id: &str) -> Option<Vec<String>> {
    match node {
        PreviewSplitNode::Pane { pane } => (pane.id == pane_id).then(|| pane.tab_ids.clone()),
        PreviewSplitNode::Split { children, .. } => children
            .iter()
            .find_map(|child| pane_tab_ids(child, pane_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_routes_and_language_names_are_stable() {
        assert_eq!(language_for_path("src/lib.rs"), "rust");
        assert_eq!(language_for_path("src/App.tsx"), "tsx");
        assert_eq!(image_format_for_path("asset.webp"), Some(ImageFormat::Webp));
        assert_eq!(image_format_for_path("asset.svg"), Some(ImageFormat::Svg));
        assert_eq!(
            surface_kind(&PreviewTarget::GitDiff {
                path: "a.rs".into(),
                staged: false,
            }),
            ContentSurfaceKind::GitDiff
        );
    }

    #[test]
    fn file_search_reveal_range_targets_the_requested_line_and_original_bytes() {
        let content = "other zed\r\n这里是 ZED 结果\r\nlast zed\r\n";
        let expected_start = content.find("ZED").unwrap();
        assert_eq!(
            file_search_reveal_range(content, "zed", 2, None),
            Some(expected_start..expected_start + "ZED".len())
        );
        assert_eq!(file_search_reveal_range(content, "missing", 2, None), None);
        assert_eq!(file_search_reveal_range(content, "zed", 8, None), None);
        assert_eq!(
            file_search_reveal_range(content, "ignored", 2, Some(10..13)),
            Some(expected_start..expected_start + 3)
        );
    }

    #[test]
    fn relative_directory_path_chain_builds_each_ancestor() {
        assert_eq!(
            relative_directory_path_chain("vendor/zed/crates"),
            ["vendor", "vendor/zed", "vendor/zed/crates"]
        );
    }

    #[test]
    fn file_search_mode_toggle_switches_between_names_and_content() {
        assert_eq!(FileSearchMode::Name.toggled(), FileSearchMode::Content);
        assert_eq!(FileSearchMode::Content.toggled(), FileSearchMode::Name);
    }

    #[test]
    fn file_name_search_results_project_to_a_collapsible_directory_first_tree() {
        let workspace_id = WorkspaceId::new();
        let result = |path: &str, name: &str, kind: FileEntryKind| FileSearchResult {
            workspace_id: workspace_id.clone(),
            path: path.to_string(),
            name: name.to_string(),
            kind,
            line: None,
            snippet: None,
            match_start: Some(0),
            match_end: u32::try_from(name.len()).ok(),
            matched_text: Some(name.to_string()),
            snippet_match_start: None,
            snippet_match_end: None,
        };
        let results = vec![
            result("README.md", "README.md", FileEntryKind::File),
            result("src/main.rs", "main.rs", FileEntryKind::File),
            result("src/tools/build.rs", "build.rs", FileEntryKind::File),
            result("src/tools", "tools", FileEntryKind::Directory),
        ];

        let rows = file_name_search_tree_rows(&results, &BTreeSet::new());
        assert_eq!(
            rows.iter()
                .map(|row| (row.path.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [
                ("src", 0),
                ("src/tools", 1),
                ("src/tools/build.rs", 2),
                ("src/main.rs", 1),
                ("README.md", 0),
            ]
        );
        assert!(!rows[0].matched);
        assert!(rows[0].has_children);
        assert!(rows[0].expanded);
        assert!(rows[1].matched);
        assert_eq!(rows[1].kind, FileEntryKind::Directory);

        let collapsed = BTreeSet::from(["src".to_string()]);
        let rows = file_name_search_tree_rows(&results, &collapsed);
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            ["src", "README.md"]
        );
        assert!(!rows[0].expanded);
    }

    #[test]
    fn every_system_right_rail_mode_has_a_reachable_panel_title() {
        assert_eq!(
            [RightRailMode::Files, RightRailMode::Git].map(RightRailMode::title),
            ["Files", "Git"]
        );
    }

    #[test]
    fn git_selection_indicator_uses_neutral_theme_states_and_a_centered_mixed_glyph() {
        let source = include_str!("code_workbench.rs");
        let indicator = source
            .split_once("fn git_selection_indicator(")
            .and_then(|(_, tail)| tail.split_once("\nfn git_change_text_color("))
            .map(|(body, _)| body)
            .unwrap();

        for theme_style in [
            "cx.theme().input",
            "cx.theme().accent",
            "cx.theme().accent_foreground",
            "cx.theme().transparent",
        ] {
            assert!(indicator.contains(theme_style));
        }
        assert!(indicator.contains("IconName::Minus"));
        assert!(!indicator.contains("h(px(1.5))"));
        assert!(!indicator.contains("gpui::white()"));
    }

    #[gpui::test]
    fn right_rail_cached_view_ignores_unrelated_workbench_notifications(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (fixture, cx) = cx.add_window_view(|window, cx| {
            CodeWorkbenchFixture::new(CodeWorkbenchFixtureKind::Files, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (workbench, right_rail) = fixture.read_with(cx, |fixture, _| {
            (fixture.workbench.clone(), fixture.right_rail.clone())
        });
        let initial = right_rail.read_with(cx, |right_rail, _| right_rail.render_count);

        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after_unrelated = right_rail.read_with(cx, |right_rail, _| right_rail.render_count);
        assert_eq!(after_unrelated, initial);

        right_rail.update(cx, |right_rail, cx| {
            right_rail.set_mode(RightRailMode::Git, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after_relevant = right_rail.read_with(cx, |right_rail, _| right_rail.render_count);
        assert!(after_relevant > after_unrelated);
    }

    #[test]
    fn workspace_generation_fence_requires_the_exact_workspace_generation() {
        let workspace_id = WorkspaceId::new();
        let workspace = WorkbenchWorkspace {
            id: workspace_id.clone(),
            root: PathBuf::from("/workspace/one"),
            generation: 7,
        };
        let fence = WorkspaceGenerationFence::capture(&workspace);

        assert!(fence.matches(Some(&workspace)));
        assert!(!fence.matches(None));
        assert!(!fence.matches(Some(&WorkbenchWorkspace {
            id: workspace_id,
            root: PathBuf::from("/workspace/one"),
            generation: 8,
        })));
        assert!(!fence.matches(Some(&WorkbenchWorkspace {
            id: WorkspaceId::new(),
            root: PathBuf::from("/workspace/two"),
            generation: 7,
        })));
    }

    #[test]
    fn worktree_lifecycle_renderer_keeps_conflicts_before_ordinary_changes_and_named_actions() {
        let source = include_str!("code_workbench.rs");
        let changes = source
            .split_once("    fn render_git_changes(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_git_tree_row("))
            .map(|(body, _)| body)
            .unwrap();
        assert!(
            changes.find("render_worktree_conflicts").unwrap()
                < changes.find(".child(if change_row_count == 0").unwrap()
        );
        for action in [
            "worktree-review-changes",
            "worktree-mark-ready",
            "worktree-merge-back",
            "worktree-review-queued-merge",
            "worktree-restore",
            "worktree-agent-assistance",
            "worktree-abort-merge",
            "worktree-complete-merge",
            "worktree-merge-strategy-merge",
            "worktree-merge-strategy-rebase",
            "use-target-conflict-",
            "use-source-conflict-",
            "stage-worktree-conflict-",
        ] {
            assert!(source.contains(action), "missing lifecycle action {action}");
        }
        let lifecycle = source
            .split_once("    fn render_worktree_lifecycle(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_worktree_confirmation("))
            .map(|(body, _)| body)
            .unwrap();
        assert!(
            lifecycle.contains("if mutations_available && let Some(confirmation) = confirmation")
        );
        let conflict_row = source
            .split_once("    fn render_worktree_conflict_row(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_git("))
            .map(|(body, _)| body)
            .unwrap();
        assert!(conflict_row.contains(".when(mutations_available, |this|"));
    }

    #[test]
    fn queued_worktree_keeps_a_fresh_merge_review_action() {
        assert_eq!(
            worktree_lifecycle_primary_action(WorktreeLifecycleDisplayState::Queued),
            Some(WorktreeLifecyclePrimaryAction::ReviewQueuedMerge)
        );
        assert_eq!(
            worktree_lifecycle_primary_action(WorktreeLifecycleDisplayState::Ready),
            Some(WorktreeLifecyclePrimaryAction::MergeBack)
        );
        for state in [
            WorktreeLifecycleDisplayState::Merging,
            WorktreeLifecycleDisplayState::NeedsResolution,
            WorktreeLifecycleDisplayState::NeedsAttention,
        ] {
            assert_eq!(worktree_lifecycle_primary_action(state), None);
        }
    }

    #[test]
    fn stale_worktree_plans_require_a_fresh_confirmation() {
        for code in [
            "worktree_preflight_stale",
            "worktree_source_head_changed",
            "worktree_target_head_changed",
            "worktree_target_branch_changed",
            "worktree_readiness_stale",
        ] {
            assert!(worktree_plan_error_requires_refresh(code));
        }
        assert!(!worktree_plan_error_requires_refresh("temporary_failure"));
    }

    #[test]
    fn conventional_commit_messages_match_the_tauri_prefix_contract() {
        assert_eq!(
            normalize_git_commit_message("feat", "add history"),
            "feat: add history"
        );
        assert_eq!(
            normalize_git_commit_message("fix", "refactor(git)!: keep existing prefix"),
            "refactor(git)!: keep existing prefix"
        );
        assert_eq!(normalize_git_commit_message("docs", "   "), "");
        assert_eq!(git_commit_placeholder("test"), "test: commit message");
    }

    #[test]
    fn git_graph_layout_tracks_linear_forks_and_merges() {
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

    #[test]
    fn git_history_branch_choices_use_upstream_and_current_divergence() {
        let branch = |name: &str,
                      current: bool,
                      upstream: Option<&str>,
                      ahead: u32,
                      behind: u32,
                      current_ahead: u32,
                      current_behind: u32| GitBranchSummary {
            name: name.into(),
            current,
            upstream: upstream.map(str::to_string),
            ahead,
            behind,
            current_ahead,
            current_behind,
            detached: false,
        };
        let choices = git_history_branch_choices(
            &[
                branch("main", true, Some("origin/main"), 3, 1, 0, 0),
                branch("feature", false, None, 0, 0, 4, 2),
            ],
            &[],
        );

        assert_eq!(choices[0].name, "main");
        assert_eq!((choices[0].incoming, choices[0].outgoing), (1, 3));
        assert_eq!(choices[1].name, "feature");
        assert_eq!((choices[1].incoming, choices[1].outgoing), (4, 2));
    }

    #[test]
    fn git_history_relative_times_cover_recent_and_calendar_dates() {
        use chrono::TimeZone as _;

        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 19, 12, 0, 0)
            .single()
            .unwrap();
        let label = |age: chrono::Duration| {
            git_history_relative_time_at(
                Some((now - age).timestamp_millis()),
                now,
                locale::ResolvedLocale::En,
            )
        };
        assert_eq!(label(chrono::Duration::seconds(12)), "12s ago");
        assert_eq!(label(chrono::Duration::minutes(5)), "5m ago");
        assert_eq!(label(chrono::Duration::hours(3)), "3h ago");
        assert!(label(chrono::Duration::hours(8)).starts_with("today "));
        assert!(label(chrono::Duration::hours(26)).starts_with("yesterday "));
        assert!(label(chrono::Duration::days(3)).contains("2026"));
    }

    #[test]
    fn git_history_date_ranges_round_trip_local_calendar_days() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap();
        let end = chrono::NaiveDate::from_ymd_opt(2026, 8, 19).unwrap();
        let (after_ms, before_ms) = git_history_date_bounds(Date::Range(Some(start), Some(end)));
        assert!(before_ms.unwrap() > after_ms.unwrap());
        assert_eq!(
            git_history_filter_date(after_ms, before_ms),
            Date::Range(Some(start), Some(end))
        );
    }

    #[test]
    fn a_short_git_history_drawer_is_treated_as_closed() {
        assert!(git_history_drawer_should_close(
            GIT_HISTORY_DRAWER_CLOSE_THRESHOLD
        ));
        assert!(!git_history_drawer_should_close(
            GIT_HISTORY_DRAWER_CLOSE_THRESHOLD + 1.0
        ));
    }

    #[test]
    fn preview_tab_statuses_match_tauri_file_and_diff_colors() {
        let change = |kind, staged, unstaged| GitChange {
            path: "./src\\main.rs".into(),
            original_path: None,
            kind,
            staged,
            unstaged,
            additions: 0,
            deletions: 0,
        };

        assert_eq!(
            file_preview_visual_status(&change(GitChangeKind::Added, false, true)),
            PreviewTabVisualStatus::Untracked
        );
        assert_eq!(
            file_preview_visual_status(&change(GitChangeKind::Added, true, false)),
            PreviewTabVisualStatus::Added
        );
        assert_eq!(
            git_preview_visual_status(&change(GitChangeKind::Deleted, true, false)),
            PreviewTabVisualStatus::Deleted
        );
        assert!(preview_paths_match("./src\\main.rs", "src/main.rs"));
    }

    #[test]
    fn path_prefix_updates_do_not_touch_similar_siblings() {
        assert_eq!(
            replace_path_prefix("src/a.rs", "src", "source"),
            "source/a.rs"
        );
        assert_eq!(
            replace_path_prefix("src2/a.rs", "src", "source"),
            "src2/a.rs"
        );
    }

    #[test]
    fn uniform_list_requests_are_bounded_without_shifting_deep_windows() {
        assert_eq!(
            bounded_uniform_range(40_000..50_000, 100_000, CODE_WORKBENCH_MAX_EAGER_ROWS),
            40_000..45_000
        );
        assert_eq!(
            bounded_uniform_range(19_500..20_000, 20_000, CODE_WORKBENCH_INITIAL_DIFF_ROWS),
            19_500..20_000
        );
        assert_eq!(bounded_uniform_range(50..80, 60, 500), 50..60);
    }

    #[test]
    fn wrapping_patch_list_reconciles_rows_and_revisions() {
        let mut state = PatchListState::new("revision-1".to_string(), 20, DIFF_ROW_MIN_HEIGHT);
        state.list.scroll_to(ListOffset {
            item_ix: 12,
            offset_in_item: px(4.0),
        });

        state.reconcile("revision-1", 8, DIFF_ROW_MIN_HEIGHT);
        assert_eq!(state.list.item_count(), 8);
        assert_eq!(state.list.logical_scroll_top().item_ix, 8);

        state.reconcile("revision-2", 4, DIFF_ROW_MIN_HEIGHT);
        assert_eq!(state.revision, "revision-2");
        assert_eq!(state.list.item_count(), 4);
        assert_eq!(state.list.logical_scroll_top().item_ix, 0);
    }

    #[test]
    fn diff_row_height_tracks_clamped_code_font_size() {
        assert_eq!(diff_row_height(10), DIFF_ROW_MIN_HEIGHT);
        assert_eq!(diff_row_height(13), 24.0);
        assert_eq!(diff_row_height(24), 40.0);
        assert_eq!(diff_row_height(100), diff_row_height(24));
    }

    #[test]
    fn file_typeahead_ranges_remain_valid_for_unicode_names() {
        let name = "文件.TS";
        let range = file_name_match_range(name, "件.t").unwrap();
        assert_eq!(&name[range], "件.T");

        let expanding_lowercase = "İstanbul.rs";
        let range = file_name_match_range(expanding_lowercase, "i").unwrap();
        assert_eq!(&expanding_lowercase[range], "İ");
    }

    #[test]
    fn inline_file_names_reject_paths_and_control_characters() {
        assert!(valid_file_name("main.rs"));
        assert!(valid_file_name(".env.local"));
        assert!(!valid_file_name(""));
        assert!(!valid_file_name(".."));
        assert!(!valid_file_name("src/main.rs"));
        assert!(!valid_file_name("src\\main.rs"));
        assert!(!valid_file_name("bad\nname"));
    }

    #[test]
    fn pasted_copies_follow_the_tauri_suffix_contract() {
        let occupied = BTreeSet::from([
            "docs/report.md".to_string(),
            "docs/report copy.md".to_string(),
        ]);
        assert_eq!(
            unique_copy_destination("docs", "report.md", |path| occupied.contains(path)),
            "docs/report copy 2.md"
        );
        assert_eq!(
            unique_copy_destination("docs", "archive.tar.gz", |path| path
                == "docs/archive.tar.gz"),
            "docs/archive.tar copy.gz"
        );
    }

    #[test]
    fn editor_open_rule_matches_the_tauri_binary_guard() {
        assert!(file_can_open_in_editor("src/main.rs", FileEntryKind::File));
        assert!(file_can_open_in_editor(
            "assets/logo.svg",
            FileEntryKind::File
        ));
        assert!(!file_can_open_in_editor(
            "assets/logo.png",
            FileEntryKind::File
        ));
        assert!(!file_can_open_in_editor("src", FileEntryKind::Directory));
    }

    #[test]
    fn workspace_polling_is_gated_by_real_surface_visibility() {
        let source = include_str!("code_workbench.rs");
        let polling = source
            .split_once("    fn start_workspace_polling(")
            .and_then(|(_, tail)| tail.split_once("\n    fn refresh_file_tree("))
            .map(|(body, _)| body)
            .expect("workspace polling should remain inspectable");
        assert!(polling.contains("if this.files_surface_visible"));
        assert!(polling.contains("if this.git_surface_visible"));
        assert!(polling.matches("HIDDEN_WORKSPACE_POLL_INTERVAL").count() >= 4);

        let visibility = source
            .split_once("    pub(crate) fn set_workspace_surface_visibility(")
            .and_then(|(_, tail)| tail.split_once("\n    fn sync_terminal_surface_activity("))
            .map(|(body, _)| body)
            .expect("workspace visibility setter should remain inspectable");
        assert!(visibility.contains("files_became_visible"));
        assert!(visibility.contains("git_became_visible"));
        assert!(visibility.contains("self.refresh_file_tree(cx);"));
        assert!(visibility.contains("self.refresh_git(cx);"));
    }
}
