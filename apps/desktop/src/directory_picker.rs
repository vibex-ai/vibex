//! In-app directory picker dialog for opening a project folder.
//!
//! Renders a folder browser entirely with GPUI: quick-location rail, search
//! box that doubles as a path field, breadcrumb navigation, and a keyboard-
//! navigable folder list. Picking a folder hands the resolved path back to
//! the workbench; the caller owns opening the workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use vibex_backend::BackendFacade;
use vibex_desktop_model::LocaleMode;

use crate::locale::{self, ResolvedLocale};

/// One browsable entry of the current directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
}

/// What the picker shows instead of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowsePhase {
    Loading,
    Ready,
    Error(String),
}

/// Quick-location rows of the side rail.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QuickLocation {
    Home,
    Drive(usize),
    /// One browse root of a paired authority.
    Root(usize),
}

/// Where a picker listing comes from.
///
/// A local authority and this machine share one filesystem, so the picker
/// browses it directly. A paired authority does not: its directories are the
/// only ones an Agent can run in, and they are listed over Remote v2 within
/// the browse roots the runtime was configured with.
#[derive(Clone)]
pub enum DirectoryBrowseTarget {
    Local,
    Authority(BackendFacade),
}

/// One resolved listing, whichever filesystem produced it.
struct DirectoryListing {
    /// The directory that produced `entries`.
    path: PathBuf,
    /// Parent directory, absent when `path` is a browse root.
    parent: Option<PathBuf>,
    entries: Vec<DirectoryEntry>,
    /// Browse roots of a paired authority; empty for a local listing.
    roots: Vec<PathBuf>,
}

impl DirectoryBrowseTarget {
    /// Whether listings come from a paired authority.
    pub(crate) fn is_authority(&self) -> bool {
        matches!(self, Self::Authority(_))
    }

    /// Lists `target`, or the authority's first browse root when it is `None`.
    /// Runs on the tokio runtime; keep it free of GPUI state.
    async fn list(&self, target: Option<PathBuf>) -> Result<DirectoryListing, String> {
        match self {
            Self::Local => {
                let target = target.unwrap_or_default();
                let entries = list_directories(&target)?;
                Ok(DirectoryListing {
                    parent: target.parent().map(Path::to_path_buf),
                    path: target,
                    entries,
                    roots: Vec::new(),
                })
            }
            Self::Authority(backend) => {
                let requested = target.map(|path| path.to_string_lossy().into_owned());
                let listing = backend
                    .workspace()
                    .browse_authority_directories(requested)
                    .await
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                Ok(DirectoryListing {
                    path: PathBuf::from(listing.path),
                    parent: listing.parent.map(PathBuf::from),
                    entries: listing
                        .entries
                        .into_iter()
                        .map(|entry| DirectoryEntry {
                            name: entry.name,
                            path: PathBuf::from(entry.path),
                        })
                        .collect(),
                    roots: listing.roots.into_iter().map(PathBuf::from).collect(),
                })
            }
        }
    }
}

#[derive(Clone, Copy)]
struct PickerText {
    search_placeholder: &'static str,
    go_up: &'static str,
    open_path: &'static str,
    places: &'static str,
    home: &'static str,
    choose_here: &'static str,
    cancel: &'static str,
    retry: &'static str,
    loading: &'static str,
    empty: &'static str,
    no_matches: &'static str,
    hint: &'static str,
}

fn text(locale: ResolvedLocale) -> PickerText {
    match locale {
        ResolvedLocale::En => PickerText {
            search_placeholder: "Filter folders, or type a path and press Enter",
            go_up: "Up",
            open_path: "Open this path",
            places: "Places",
            home: "Home",
            choose_here: "Open",
            cancel: "Cancel",
            retry: "Retry",
            loading: "Loading…",
            empty: "No folders here",
            no_matches: "No folders match",
            hint: "↑↓ Navigate · Enter Open · ⌘Enter Choose here",
        },
        ResolvedLocale::ZhCn => PickerText {
            search_placeholder: "筛选文件夹，或输入路径后按 Enter",
            go_up: "上一级",
            open_path: "打开该路径",
            places: "位置",
            home: "主目录",
            choose_here: "打开",
            cancel: "取消",
            retry: "重试",
            loading: "正在加载…",
            empty: "这里没有文件夹",
            no_matches: "没有匹配的文件夹",
            hint: "↑↓ 选择 · Enter 打开 · ⌘Enter 选定当前目录",
        },
        ResolvedLocale::ZhTw => PickerText {
            search_placeholder: "篩選資料夾，或輸入路徑後按 Enter",
            go_up: "上一層",
            open_path: "開啟該路徑",
            places: "位置",
            home: "主資料夾",
            choose_here: "開啟",
            cancel: "取消",
            retry: "重試",
            loading: "載入中…",
            empty: "這裡沒有資料夾",
            no_matches: "沒有符合的資料夾",
            hint: "↑↓ 選擇 · Enter 開啟 · ⌘Enter 選定目前目錄",
        },
    }
}

/// Callback invoked with the confirmed directory path. Returning `true` asks
/// the host to close the dialog (path accepted); `false` keeps it open.
pub type DirectoryPickHandler = Arc<dyn Fn(String, &mut Window, &mut App) -> bool + 'static>;

/// Row-id offset that keeps authority browse roots apart from local volumes in
/// the quick-location rail.
const ROOT_QUICK_LOCATION_ROW_OFFSET: usize = 1024;

pub struct DirectoryPickerDialog {
    locale_mode: LocaleMode,
    /// Filesystem the listings come from.
    source: DirectoryBrowseTarget,
    /// Confirm-with-the-primary-action candidate; tracks the browse root.
    selected: Option<PathBuf>,
    /// The current browse root; `None` until the first listing resolves.
    browse_root: Option<PathBuf>,
    /// Parent of the current browse root as reported by the source. `None`
    /// means the root is the top of what may be browsed, so "Up" is disabled
    /// instead of climbing out of the authority's browse roots.
    parent: Option<PathBuf>,
    /// The directory the pending (or failed) browse targeted, for retry.
    retry_target: Option<PathBuf>,
    home: Option<PathBuf>,
    /// This machine's mounted volumes (best-effort; failures just hide rows).
    drives: Vec<PathBuf>,
    /// Browse roots of a paired authority, offered as quick locations.
    roots: Vec<PathBuf>,
    entries: Vec<DirectoryEntry>,
    phase: BrowsePhase,
    search_input: Entity<InputState>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    browse_task: Option<Task<()>>,
    drives_task: Option<Task<()>>,
    focus_pending: bool,
    list_scroll: ScrollHandle,
    on_pick: DirectoryPickHandler,
    _search_events: gpui::Subscription,
}

impl DirectoryPickerDialog {
    pub fn new(
        locale_mode: LocaleMode,
        initial_dir: Option<PathBuf>,
        source: DirectoryBrowseTarget,
        on_pick: DirectoryPickHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let locale = locale::resolve_locale(locale_mode, locale::system_locale().as_deref());
        let placeholder = text(locale).search_placeholder;
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let search_events = cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
            // Only the filter reset is handled here. Enter and Escape are
            // deliberately left alone: the single-line field re-emits them
            // while propagating, and the dialog-level key handler below is
            // the single authority for those keys.
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let local = !source.is_authority();
        let mut dialog = Self {
            locale_mode,
            source,
            selected: None,
            browse_root: None,
            parent: None,
            retry_target: None,
            home: local.then(user_home_directory).flatten(),
            drives: Vec::new(),
            roots: Vec::new(),
            entries: Vec::new(),
            phase: BrowsePhase::Loading,
            search_input,
            active: 0,
            browse_task: None,
            drives_task: None,
            focus_pending: true,
            list_scroll: ScrollHandle::new(),
            on_pick,
            _search_events: search_events,
        };
        dialog.browse(initial_dir, cx);
        if local {
            dialog.load_drives(cx);
        }
        dialog
    }

    fn locale(&self) -> ResolvedLocale {
        locale::resolve_locale(self.locale_mode, locale::system_locale().as_deref())
    }

    /// Preferred starting root: the requested directory, else the user home,
    /// else the filesystem root.
    fn default_root(&self) -> PathBuf {
        self.home
            .clone()
            .or_else(|| Some(PathBuf::from("/")))
            .unwrap_or_default()
    }

    fn load_drives(&mut self, cx: &mut Context<Self>) {
        let runner = gpui_tokio::Tokio::spawn(cx, async move { detect_drives() });
        self.drives_task = Some(cx.spawn(async move |this, cx| {
            let drives = runner.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.drives = drives;
                cx.notify();
            });
        }));
    }

    /// Browses `target`. A local browse always resolves to a concrete
    /// directory; an authority browse lets the runtime answer with its first
    /// browse root when the caller has none to suggest.
    fn browse(&mut self, target: Option<PathBuf>, cx: &mut Context<Self>) {
        let target = match &self.source {
            DirectoryBrowseTarget::Local => Some(target.unwrap_or_else(|| self.default_root())),
            DirectoryBrowseTarget::Authority(_) => target,
        };
        self.retry_target = target.clone();
        self.phase = BrowsePhase::Loading;
        self.entries.clear();
        self.active = 0;
        self.browse_task = Some({
            let source = self.source.clone();
            let browse_target = target;
            let runner =
                gpui_tokio::Tokio::spawn(cx, async move { source.list(browse_target).await });
            cx.spawn(async move |this, cx| {
                let listing = runner.await.unwrap_or_else(|error| Err(error.to_string()));
                let _ = this.update(cx, |this, cx| {
                    match listing {
                        Ok(listing) => {
                            this.browse_root = Some(listing.path.clone());
                            this.selected = Some(listing.path);
                            this.parent = listing.parent;
                            if !listing.roots.is_empty() {
                                this.roots = listing.roots;
                            }
                            this.entries = listing.entries;
                            this.phase = BrowsePhase::Ready;
                        }
                        Err(error) => {
                            // An authority that never answered has no roots to
                            // offer yet, so Retry falls back to its first root
                            // instead of repeating the path that just failed.
                            if this.source.is_authority() && this.roots.is_empty() {
                                this.retry_target = None;
                            }
                            this.phase = BrowsePhase::Error(error);
                        }
                    }
                    cx.notify();
                });
            })
        });
        cx.notify();
    }

    /// Rows visible under the current query. A query that reads as a path
    /// filters nothing; it is a navigation request the Enter handler resolves
    /// instead.
    fn filtered_entries(&self, cx: &App) -> Vec<DirectoryEntry> {
        let query = self.search_input.read(cx).value().trim().to_string();
        if query_reads_as_path(&query, self.source.is_authority()) {
            return Vec::new();
        }
        if query.is_empty() {
            return self.entries.clone();
        }
        let query = query.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
    }

    /// Follows the search query as a path when it names a directory.
    ///
    /// A local browse can check the filesystem before asking for a listing; an
    /// authority browse cannot, so the query goes to the runtime, which either
    /// resolves it or explains why it will not.
    fn descend_into_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let query = self.search_input.read(cx).value().trim().to_string();
        let query = if self.source.is_authority() {
            query
        } else {
            expand_query(&query, self.home.as_deref())
        };
        if query.is_empty() {
            return false;
        }
        if self.source.is_authority() {
            self.clear_query(window, cx);
            self.browse(Some(PathBuf::from(query)), cx);
            return true;
        }
        let path = PathBuf::from(&query);
        if path.is_dir() {
            let path = path.canonicalize().unwrap_or(path);
            self.clear_query(window, cx);
            self.browse(Some(path), cx);
            return true;
        }
        false
    }

    /// Enter in the search field: descend into the highlighted row, else
    /// follow the query as a path.
    fn activate_focused_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.filtered_entries(cx);
        if let Some(entry) = rows.get(self.active) {
            let path = entry.path.clone();
            self.clear_query(window, cx);
            self.browse(Some(path), cx);
            return;
        }
        self.descend_into_query(window, cx);
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.clone() {
            self.browse(Some(parent), cx);
        }
    }

    fn goto_quick_location(&mut self, location: QuickLocation, cx: &mut Context<Self>) {
        let target = match &location {
            QuickLocation::Home => self.home.clone(),
            QuickLocation::Drive(ix) => self.drives.get(*ix).cloned(),
            QuickLocation::Root(ix) => self.roots.get(*ix).cloned(),
        };
        if let Some(target) = target {
            self.browse(Some(target), cx);
        }
    }

    fn move_active(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.filtered_entries(cx).len();
        if count == 0 {
            return;
        }
        let next = (self.active as isize + delta).clamp(0, count as isize - 1) as usize;
        if next != self.active {
            self.active = next;
            self.list_scroll.scroll_to_item(next);
            cx.notify();
        }
    }

    /// Primary action: confirm the directory currently browsed (the path the
    /// footer displays). Highlighted rows are only a navigation target —
    /// Enter descends into them. Returns `true` when the dialog should
    /// close.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.phase != BrowsePhase::Ready {
            return false;
        }
        let Some(target) = self.selected.clone() else {
            return false;
        };
        self.clear_query(window, cx);
        self.submit(target, window, cx)
    }

    fn submit(&mut self, target: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let handler = self.on_pick.clone();
        let path = target.to_string_lossy().into_owned();
        handler(path, window, cx)
    }

    pub fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let strings = text(self.locale());
        let is_dark = cx.theme().is_dark();
        let foreground = crate::theme::semantic_color("popover-foreground", is_dark);
        let muted = crate::theme::semantic_color("muted-foreground", is_dark);
        let primary = cx.theme().primary;
        let selected_label = self
            .selected
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| strings.loading.to_string());
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size(px(13.0))
                            .flex_none()
                            .text_color(primary),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(foreground.opacity(0.75))
                            .child(selected_label),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted.opacity(0.7))
                    .child(strings.hint),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("directory-picker-cancel")
                            .small()
                            .outline()
                            .label(strings.cancel)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("directory-picker-choose")
                            .small()
                            .primary()
                            .label(strings.choose_here)
                            .disabled(self.phase != BrowsePhase::Ready)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                if this.confirm(window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

impl gpui::Render for DirectoryPickerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_pending = false;
            let handle = self.search_input.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let strings = text(self.locale());
        let is_dark = cx.theme().is_dark();
        let foreground = crate::theme::semantic_color("popover-foreground", is_dark);
        let muted = crate::theme::semantic_color("muted-foreground", is_dark);
        let border = crate::theme::semantic_color("border", is_dark);
        let muted_bg = crate::theme::semantic_color("muted", is_dark);
        let danger = cx.theme().danger;
        let primary = cx.theme().primary;

        let rows = self.filtered_entries(cx);
        let loading = self.phase == BrowsePhase::Loading;
        let load_error = match &self.phase {
            BrowsePhase::Error(error) => Some(error.clone()),
            _ => None,
        };
        let query = self.search_input.read(cx).value().trim().to_string();
        let query_is_path = query_reads_as_path(&query, self.source.is_authority());

        let browse_root = self.browse_root.clone();
        let can_go_up = self.parent.is_some();

        // Quick-location rail rows: a local browse offers this machine's home
        // and mounted volumes; a paired authority offers the roots it allows
        // browsing, which is the only place its directories can be reached.
        let quick_rows: Vec<(QuickLocation, SharedString)> = if self.source.is_authority() {
            self.roots
                .iter()
                .enumerate()
                .map(|(ix, root)| {
                    (
                        QuickLocation::Root(ix),
                        SharedString::from(root.to_string_lossy().into_owned()),
                    )
                })
                .collect()
        } else {
            let mut rows = vec![(QuickLocation::Home, SharedString::from(strings.home))];
            rows.extend(self.drives.iter().enumerate().map(|(ix, drive)| {
                let label = drive
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned().into())
                    .unwrap_or_else(|| drive.to_string_lossy().into_owned().into());
                (QuickLocation::Drive(ix), label)
            }));
            rows
        };
        let active_location: Option<QuickLocation> = browse_root.as_ref().and_then(|root| {
            if self.source.is_authority() {
                self.roots
                    .iter()
                    .position(|candidate| candidate == root)
                    .map(QuickLocation::Root)
            } else if self.home.as_ref().is_some_and(|home| root == home) {
                Some(QuickLocation::Home)
            } else {
                self.drives
                    .iter()
                    .position(|drive| drive == root)
                    .map(QuickLocation::Drive)
            }
        });

        // Breadcrumbs fold the authority's browse root into one crumb, so the
        // trail can never offer a target above the boundary the runtime set.
        let breadcrumbs = browse_root
            .as_ref()
            .map(|root| {
                if self.source.is_authority() {
                    let base = self
                        .roots
                        .iter()
                        .filter(|candidate| root.starts_with(candidate))
                        .max_by_key(|candidate| candidate.components().count());
                    match base {
                        Some(base) => {
                            breadcrumb_segments(root, Some(base), &base.to_string_lossy())
                        }
                        None => breadcrumb_segments(root, None, strings.home),
                    }
                } else {
                    breadcrumb_segments(root, self.home.as_deref(), strings.home)
                }
            })
            .unwrap_or_default();
        let last_crumb = breadcrumbs.len().saturating_sub(1);

        let list: AnyElement = if loading {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .size(px(36.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .bg(muted_bg.opacity(0.4))
                        .child(Spinner::new()),
                )
                .child(div().text_sm().text_color(muted).child(strings.loading))
                .into_any_element()
        } else if let Some(error) = load_error {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(20.0))
                        .text_color(danger),
                )
                .child(
                    div()
                        .max_w(px(380.0))
                        .text_center()
                        .text_xs()
                        .text_color(muted)
                        .child(error),
                )
                .child(
                    Button::new("directory-picker-retry")
                        .small()
                        .outline()
                        .label(strings.retry)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let target = this.retry_target.clone();
                            this.browse(target, cx);
                        })),
                )
                .into_any_element()
        } else if rows.is_empty() {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .size(px(44.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(12.0))
                        .bg(muted_bg.opacity(0.4))
                        .child(
                            Icon::new(IconName::FolderOpen)
                                .size(px(20.0))
                                .text_color(muted),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(if query.is_empty() {
                            strings.empty
                        } else {
                            strings.no_matches
                        }),
                )
                .when(query_is_path, |view| {
                    view.child(
                        Button::new("directory-picker-go-path")
                            .small()
                            .outline()
                            .label(strings.open_path)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.descend_into_query(window, cx);
                            })),
                    )
                })
                .into_any_element()
        } else {
            let entries = rows;
            v_flex()
                .id("directory-picker-list")
                .flex_1()
                .min_h_0()
                .track_scroll(&self.list_scroll)
                .overflow_y_scroll()
                .vertical_scrollbar(&self.list_scroll)
                .gap(px(2.0))
                .pt(px(2.0))
                .pb(px(4.0))
                .children(entries.into_iter().enumerate().map(|(ix, entry)| {
                    let is_active = ix == self.active;
                    let name: SharedString = entry.name.clone().into();
                    let path = entry.path.clone();
                    h_flex()
                        .id(("directory-picker-row", ix))
                        .min_h(px(32.0))
                        .px_2()
                        .rounded(px(6.0))
                        .gap_2()
                        .text_sm()
                        .cursor_pointer()
                        .when(is_active, |row| {
                            row.bg(primary.opacity(0.14)).text_color(foreground)
                        })
                        .when(!is_active, |row| {
                            row.text_color(foreground.opacity(0.9))
                                .hover(|style| style.bg(muted_bg.opacity(0.6)))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.active = ix;
                            this.clear_query(window, cx);
                            this.browse(Some(path.clone()), cx);
                        }))
                        .child(
                            Icon::new(IconName::Folder)
                                .size(px(15.0))
                                .flex_none()
                                .text_color(if is_active { primary } else { muted }),
                        )
                        .child(div().min_w_0().truncate().child(name))
                }))
                .into_any_element()
        };

        let rail = v_flex()
            .w(px(172.0))
            .flex_none()
            .border_l_1()
            .border_color(border.opacity(0.6))
            .px(px(8.0))
            .pt(px(10.0))
            .pb(px(8.0))
            .gap(px(2.0))
            .child(
                div()
                    .px(px(8.0))
                    .pb(px(6.0))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted.opacity(0.7))
                    .child(strings.places),
            )
            .children(quick_rows.into_iter().map(|(location, label)| {
                let is_active = active_location.as_ref() == Some(&location);
                let icon = match location {
                    QuickLocation::Home => IconName::CircleUser,
                    QuickLocation::Drive(_) => IconName::HardDrive,
                    QuickLocation::Root(_) => IconName::Globe,
                };
                let row_key = match location {
                    QuickLocation::Home => 0usize,
                    QuickLocation::Drive(ix) => ix + 1,
                    // Local volumes and authority roots never share a rail;
                    // the offset keeps their row ids distinct anyway.
                    QuickLocation::Root(ix) => ix + ROOT_QUICK_LOCATION_ROW_OFFSET,
                };
                h_flex()
                    .id(("directory-picker-location", row_key))
                    .min_h(px(30.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .when(is_active, |row| {
                        row.bg(primary.opacity(0.14)).text_color(foreground)
                    })
                    .when(!is_active, |row| {
                        row.text_color(muted)
                            .hover(|style| style.bg(muted_bg.opacity(0.6)))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.goto_quick_location(location.clone(), cx);
                    }))
                    .child(
                        Icon::new(icon)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(if is_active { primary } else { muted }),
                    )
                    .child(div().min_w_0().truncate().child(label))
            }));

        let crumbs = h_flex()
            .flex_wrap()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap(px(2.0))
            .text_xs()
            .children(breadcrumbs.into_iter().enumerate().map(|(ix, crumb)| {
                let is_last = ix == last_crumb;
                h_flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_color(muted.opacity(0.6))
                            .child(SharedString::from("/")),
                    )
                    .child(if is_last {
                        div()
                            .px(px(5.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .bg(primary.opacity(0.10))
                            .text_color(foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child(crumb.label)
                            .into_any_element()
                    } else {
                        let path = crumb.path;
                        div()
                            .id(("directory-picker-crumb", ix))
                            .px(px(5.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|style| style.text_color(foreground).bg(muted_bg.opacity(0.6)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.browse(Some(path.clone()), cx);
                            }))
                            .child(crumb.label)
                            .into_any_element()
                    })
            }));

        v_flex()
            .size_full()
            .min_h_0()
            .id("directory-picker-card")
            // The dialog's own Enter/Escape bindings are disabled; this
            // handler is the single keyboard authority so a plain Enter in
            // the search field cannot both descend and confirm.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" if !event.keystroke.modifiers.modified() => this.move_active(-1, cx),
                    "down" if !event.keystroke.modifiers.modified() => this.move_active(1, cx),
                    "enter" if event.keystroke.modifiers.secondary() => {
                        if this.confirm(window, cx) {
                            window.close_dialog(cx);
                        }
                    }
                    "enter" => this.activate_focused_row(window, cx),
                    "escape" => window.close_dialog(cx),
                    _ => {}
                }
            }))
            .child(
                // Search bar: filter input + up button.
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .child(
                        // `Input` owns its border and focus ring, so the field
                        // only needs the flex slot.
                        Input::new(&self.search_input)
                            .small()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                    )
                    .child(
                        Button::new("directory-picker-up")
                            .small()
                            .outline()
                            .icon(IconName::ArrowUp)
                            .tooltip(SharedString::from(strings.go_up))
                            .disabled(loading || !can_go_up)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.go_up(cx);
                            })),
                    ),
            )
            .child(
                // Folder column (breadcrumbs above the list) beside the
                // full-height places rail.
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .min_h(px(28.0))
                                    .px_1()
                                    .child(crumbs),
                            )
                            .child(list),
                    )
                    .child(rail),
            )
    }
}

struct Crumb {
    label: SharedString,
    path: PathBuf,
}

/// Breadcrumb segments for `path`, folding everything up to `home` into a
/// single translated "Home" crumb when the path sits at or under it.
fn breadcrumb_segments(path: &Path, home: Option<&Path>, home_label: &str) -> Vec<Crumb> {
    let mut segments: Vec<Crumb> = Vec::new();
    let (folded, visible): (Option<PathBuf>, &Path) = match home {
        Some(home) if path.starts_with(home) => (
            Some(home.to_path_buf()),
            path.strip_prefix(home).unwrap_or_else(|_| Path::new("")),
        ),
        _ => (None, path),
    };
    if let Some(ref fold) = folded {
        segments.push(Crumb {
            label: SharedString::from(home_label),
            path: fold.clone(),
        });
    }
    // Accumulate from the folded base (or the path root) so every crumb
    // target stays absolute; segments below the home fold would otherwise
    // browse a relative path and fail to list.
    let mut accumulated = folded.unwrap_or_default();
    for component in visible.components() {
        accumulated.push(component.as_os_str());
        let label = accumulated
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| accumulated.to_string_lossy().into_owned());
        segments.push(Crumb {
            label: SharedString::from(label),
            path: accumulated.clone(),
        });
    }
    segments
}

/// Lists the subdirectories of `path`, hidden entries excluded, sorted
/// case-insensitively. Runs on the tokio runtime; keep it free of GPUI
/// state.
fn list_directories(path: &Path) -> Result<Vec<DirectoryEntry>, String> {
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // A symlinked directory is browsable; a broken link fails `metadata`
        // and is skipped rather than failing the listing.
        let is_dir = if file_type.is_symlink() {
            entry
                .path()
                .metadata()
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        } else {
            file_type.is_dir()
        };
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        entries.push(DirectoryEntry {
            name,
            path: entry.path(),
        });
    }
    entries.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

/// Best-effort detection of this machine's mounted volumes. Failures simply
/// leave the rail with the Home row. Runs on the tokio runtime; keep it free
/// of GPUI state.
fn detect_drives() -> Vec<PathBuf> {
    detect_drives_sync()
}

#[cfg(target_os = "linux")]
fn detect_drives_sync() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let Ok(content) = std::fs::read_to_string("/proc/self/mounts") else {
        return roots;
    };
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let (_device, mount_point, filesystem) = match (fields.next(), fields.next(), fields.next())
        {
            (Some(device), Some(point), Some(fs)) => (device, point, fs),
            _ => continue,
        };
        if !matches!(
            filesystem,
            "ext2"
                | "ext3"
                | "ext4"
                | "xfs"
                | "btrfs"
                | "zfs"
                | "f2fs"
                | "ntfs"
                | "vfat"
                | "exfat"
                | "apfs"
                | "fuseblk"
                | "exfat-fuse"
                | "ntfs-3g"
        ) {
            continue;
        }
        let path = PathBuf::from(decode_mount_path(mount_point));
        // Skip the root filesystem itself (the breadcrumb path and Home
        // already cover it) and anything unreadable.
        if path == Path::new("/") || !path.is_dir() {
            continue;
        }
        // Skip duplicates nested under a volume already listed (e.g. a bind
        // mount inside an external drive).
        if roots
            .iter()
            .any(|existing: &PathBuf| path.starts_with(existing))
        {
            continue;
        }
        roots.push(path);
    }
    roots.sort();
    roots
}

#[cfg(target_os = "macos")]
fn detect_drives_sync() -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt as _;

    let mut roots: Vec<PathBuf> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir("/Volumes") else {
        return roots;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            roots.push(path);
        }
    }
    // The boot volume appears under /Volumes but is the same filesystem as
    // "/", so the breadcrumb path already covers it.
    if let Ok(root_dev) = std::fs::metadata("/").map(|metadata| metadata.dev()) {
        roots.retain(|path| {
            std::fs::metadata(path)
                .map(|metadata| metadata.dev() != root_dev)
                .unwrap_or(false)
        });
    }
    roots.sort();
    roots
}

#[cfg(target_os = "windows")]
fn detect_drives_sync() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if root.is_dir() {
            roots.push(root);
        }
    }
    roots
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn detect_drives_sync() -> Vec<PathBuf> {
    Vec::new()
}

/// Decodes `/proc/self/mounts` octal escapes (`\040` space, `\011` tab,
/// `\134` backslash, generic `\nnn`).
#[cfg(target_os = "linux")]
fn decode_mount_path(raw: &str) -> String {
    let mut decoded = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(current) = chars.next() {
        if current != '\\' {
            decoded.push(current);
            continue;
        }
        let digits: Vec<u32> = (0..3)
            .filter_map(|_| chars.next().and_then(|digit| digit.to_digit(8)))
            .collect();
        if digits.len() == 3 {
            let code = digits[0] * 64 + digits[1] * 8 + digits[2];
            decoded.push(char::from_u32(code).unwrap_or('_'));
        } else {
            decoded.push('\\');
        }
    }
    decoded
}

/// Whether a query reads as a path to navigate to rather than a filter.
///
/// An authority reports POSIX paths, and a Windows client would not call
/// `/data/repo` absolute, so the leading separator decides there. `~` means
/// this machine's home and stays a path only for a local browse.
fn query_reads_as_path(query: &str, authority: bool) -> bool {
    if query.is_empty() {
        return false;
    }
    if authority {
        query.starts_with('/')
    } else {
        Path::new(query).is_absolute() || query.starts_with('~')
    }
}

/// Expands a leading `~` in a search query to the user home.
fn expand_query(query: &str, home: Option<&Path>) -> String {
    if query == "~" {
        return home
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    if let Some(rest) = query.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    query.to_string()
}

fn user_home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumb_segments_under_home_stay_absolute() {
        let home = Path::new("/home/ada");
        let crumbs = breadcrumb_segments(Path::new("/home/ada/projects/vibex"), Some(home), "Home");
        let targets: Vec<&Path> = crumbs.iter().map(|crumb| crumb.path.as_path()).collect();
        assert_eq!(
            targets,
            vec![
                Path::new("/home/ada"),
                Path::new("/home/ada/projects"),
                Path::new("/home/ada/projects/vibex"),
            ]
        );
        let labels: Vec<&str> = crumbs.iter().map(|crumb| crumb.label.as_ref()).collect();
        assert_eq!(labels, vec!["Home", "projects", "vibex"]);
    }

    #[test]
    fn breadcrumb_segments_outside_home_show_full_path() {
        let crumbs = breadcrumb_segments(
            Path::new("/media/backup"),
            Some(Path::new("/home/ada")),
            "Home",
        );
        assert!(crumbs.iter().all(|crumb| crumb.path.is_absolute()));
        // The leading filesystem-root crumb ("label /", path /) is present
        // and clickable on paths outside the home fold.
        assert_eq!(crumbs[0].label.as_ref(), "/");
        assert_eq!(crumbs[0].path, Path::new("/"));
        assert_eq!(
            crumbs
                .iter()
                .map(|crumb| crumb.label.as_ref())
                .collect::<Vec<_>>(),
            vec!["/", "media", "backup"]
        );
    }

    #[test]
    fn breadcrumb_segments_at_home_root_show_single_crumb() {
        let crumbs =
            breadcrumb_segments(Path::new("/home/ada"), Some(Path::new("/home/ada")), "Home");
        assert_eq!(crumbs.len(), 1);
        assert_eq!(crumbs[0].label.as_ref(), "Home");
        assert_eq!(crumbs[0].path, Path::new("/home/ada"));
    }

    #[test]
    fn expand_query_resolves_tilde_forms() {
        let home = Path::new("/home/ada");
        assert_eq!(expand_query("~", Some(home)), "/home/ada");
        assert_eq!(expand_query("~/docs", Some(home)), "/home/ada/docs");
        assert_eq!(expand_query("/opt", Some(home)), "/opt");
        assert_eq!(expand_query("~", None), "");
    }

    #[test]
    fn query_reads_as_path_follows_the_browsed_filesystem() {
        // Local: this machine's absolute paths and its home shorthand.
        assert!(query_reads_as_path("/opt/vibex", false));
        assert!(query_reads_as_path("~/docs", false));
        assert!(!query_reads_as_path("", false));
        assert!(!query_reads_as_path("vibex", false));
        // Authority: its POSIX paths stay paths even on a Windows client,
        // while `~` keeps meaning this machine's home and only filters there.
        assert!(query_reads_as_path("/data/repos", true));
        assert!(!query_reads_as_path("~/docs", true));
        assert!(!query_reads_as_path("", true));
        assert!(!query_reads_as_path("repos", true));
    }

    #[test]
    fn breadcrumb_segments_fold_an_authority_root_into_one_crumb() {
        let crumbs = breadcrumb_segments(
            Path::new("/data/repos/vibex"),
            Some(Path::new("/data/repos")),
            "/data/repos",
        );
        assert_eq!(
            crumbs
                .iter()
                .map(|crumb| crumb.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("/data/repos"), Path::new("/data/repos/vibex")]
        );
        let labels: Vec<&str> = crumbs.iter().map(|crumb| crumb.label.as_ref()).collect();
        assert_eq!(labels, vec!["/data/repos", "vibex"]);
    }

    #[test]
    fn breadcrumb_segments_at_an_authority_root_show_single_crumb() {
        let crumbs = breadcrumb_segments(Path::new("/data"), Some(Path::new("/data")), "/data");
        assert_eq!(crumbs.len(), 1);
        assert_eq!(crumbs[0].label.as_ref(), "/data");
    }
}
