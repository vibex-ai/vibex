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

pub struct DirectoryPickerDialog {
    locale_mode: LocaleMode,
    /// Confirm-with-the-primary-action candidate; tracks the browse root.
    selected: Option<PathBuf>,
    /// The current browse root; `None` until the first listing resolves.
    browse_root: Option<PathBuf>,
    /// The directory the pending (or failed) browse targeted, for retry.
    retry_target: Option<PathBuf>,
    home: Option<PathBuf>,
    /// This machine's mounted volumes (best-effort; failures just hide rows).
    drives: Vec<PathBuf>,
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
        let mut dialog = Self {
            locale_mode,
            selected: None,
            browse_root: None,
            retry_target: None,
            home: user_home_directory(),
            drives: Vec::new(),
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
        dialog.load_drives(cx);
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

    /// Browses `target` (or the default root) and resets the selection to it.
    fn browse(&mut self, target: Option<PathBuf>, cx: &mut Context<Self>) {
        let target = target.unwrap_or_else(|| self.default_root());
        self.retry_target = Some(target.clone());
        self.phase = BrowsePhase::Loading;
        self.entries.clear();
        self.active = 0;
        self.browse_task = Some({
            let browse_target = target.clone();
            let runner =
                gpui_tokio::Tokio::spawn(cx, async move { list_directories(&browse_target) });
            cx.spawn(async move |this, cx| {
                let listing = runner.await.unwrap_or_else(|error| Err(error.to_string()));
                let _ = this.update(cx, |this, cx| {
                    match listing {
                        Ok(entries) => {
                            this.browse_root = Some(target);
                            this.selected = this.browse_root.clone();
                            this.entries = entries;
                            this.phase = BrowsePhase::Ready;
                        }
                        Err(error) => {
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
        if Path::new(&query).is_absolute() || query.starts_with('~') {
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
    fn descend_into_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let query = self.search_input.read(cx).value().trim().to_string();
        let query = expand_query(&query, self.home.as_deref());
        if query.is_empty() {
            return false;
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
        if let Some(parent) = self
            .browse_root
            .clone()
            .and_then(|current| current.parent().map(Path::to_path_buf))
        {
            self.browse(Some(parent), cx);
        }
    }

    fn goto_quick_location(&mut self, location: QuickLocation, cx: &mut Context<Self>) {
        let target = match &location {
            QuickLocation::Home => self.home.clone(),
            QuickLocation::Drive(ix) => self.drives.get(*ix).cloned(),
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
        let selected_label = self
            .selected
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| strings.loading.to_string());
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(foreground.opacity(0.65))
                    .child(selected_label),
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
        let input_color = crate::theme::semantic_color("input", is_dark);
        let ring = crate::theme::semantic_color("ring", is_dark);
        let danger = cx.theme().danger;
        let primary = cx.theme().primary;

        let search_focused = self.search_input.focus_handle(cx).is_focused(window);
        let rows = self.filtered_entries(cx);
        let loading = self.phase == BrowsePhase::Loading;
        let load_error = match &self.phase {
            BrowsePhase::Error(error) => Some(error.clone()),
            _ => None,
        };
        let query = self.search_input.read(cx).value().trim().to_string();
        let query_is_path =
            (Path::new(&query).is_absolute() || query.starts_with('~')) && !query.is_empty();

        let browse_root = self.browse_root.clone();
        let can_go_up = browse_root
            .as_ref()
            .and_then(|path| path.parent())
            .is_some();

        // Quick-location rail rows: home first, then mounted volumes.
        let quick_rows: Vec<(QuickLocation, SharedString)> = {
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
            if self.home.as_ref().is_some_and(|home| root == home) {
                Some(QuickLocation::Home)
            } else {
                self.drives
                    .iter()
                    .position(|drive| drive == root)
                    .map(QuickLocation::Drive)
            }
        });

        let breadcrumbs = browse_root
            .as_ref()
            .map(|root| breadcrumb_segments(root, self.home.as_deref(), strings.home))
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
                .py(px(4.0))
                .children(entries.into_iter().enumerate().map(|(ix, entry)| {
                    let is_active = ix == self.active;
                    let name: SharedString = entry.name.clone().into();
                    let path_label: SharedString = entry.path.to_string_lossy().into_owned().into();
                    let path = entry.path.clone();
                    h_flex()
                        .id(("directory-picker-row", ix))
                        .min_h(px(30.0))
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
                        .child(
                            div()
                                .ml_auto()
                                .flex_none()
                                .max_w(px(160.0))
                                .truncate()
                                .text_xs()
                                .text_color(muted.opacity(0.7))
                                .child(path_label),
                        )
                }))
                .into_any_element()
        };

        let rail = v_flex()
            .w(px(168.0))
            .flex_none()
            .border_l_1()
            .border_color(border.opacity(0.6))
            .px(px(8.0))
            .py(px(8.0))
            .gap(px(2.0))
            .child(
                div()
                    .px(px(8.0))
                    .pb(px(4.0))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted.opacity(0.8))
                    .child(strings.places),
            )
            .children(quick_rows.into_iter().map(|(location, label)| {
                let is_active = active_location.as_ref() == Some(&location);
                let icon = match location {
                    QuickLocation::Home => IconName::CircleUser,
                    QuickLocation::Drive(_) => IconName::HardDrive,
                };
                let row_key = match location {
                    QuickLocation::Home => 0usize,
                    QuickLocation::Drive(ix) => ix + 1,
                };
                h_flex()
                    .id(("directory-picker-location", row_key))
                    .min_h(px(28.0))
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
                            .px(px(4.0))
                            .rounded(px(4.0))
                            .text_color(foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child(crumb.label)
                            .into_any_element()
                    } else {
                        let path = crumb.path;
                        div()
                            .id(("directory-picker-crumb", ix))
                            .px(px(4.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|style| style.text_color(foreground))
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(if search_focused { ring } else { input_color })
                            .bg(muted_bg.opacity(0.5))
                            .child(
                                Input::new(&self.search_input)
                                    .small()
                                    .h_full()
                                    .appearance(false)
                                    .text_sm()
                                    .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                            ),
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
                // Breadcrumb path strip above the folder list.
                h_flex()
                    .flex_none()
                    .items_center()
                    .min_h(px(28.0))
                    .px_1()
                    .child(crumbs),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(v_flex().flex_1().min_w_0().min_h_0().child(list))
                    .child(rail),
            )
            .child(
                div()
                    .flex_none()
                    .pt_2()
                    .text_xs()
                    .text_color(muted.opacity(0.7))
                    .child(strings.hint),
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
    if let Some(fold) = folded {
        segments.push(Crumb {
            label: SharedString::from(home_label),
            path: fold,
        });
    }
    let mut accumulated = PathBuf::new();
    for component in visible.components() {
        let label = component.as_os_str().to_string_lossy().into_owned();
        if accumulated.as_os_str().is_empty() {
            accumulated = PathBuf::from(&label);
        } else {
            accumulated.push(&label);
        }
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
        use std::os::unix::fs::MetadataExt as _;
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
