//! A read-only Git surface over the workspace.
//!
//! The runtime owns the repository; this lists its status (`git_status`) and
//! shows a file's diff (`git_diff`). Staging, committing and worktrees stay on
//! the desktop for now.

use std::rc::Rc;

use nsh_base::button;
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::action};

/// Opens a path's diff, staged or not.
type OpenDiff = Rc<dyn Fn(&str, bool)>;

/// One changed path in the status list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeRow {
    pub path: String,
    /// The one-letter kind ("M", "A", "D", "R", "?").
    pub status: String,
    pub staged: bool,
    pub additions: u32,
    pub deletions: u32,
}

/// An opened diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffPreview {
    pub title: String,
    pub body: String,
    pub truncated: bool,
}

/// What the surface may do, and the state it shows.
#[derive(Clone)]
pub struct GitHost {
    pub open: Signal<bool>,
    pub branch: Signal<Option<SharedString>>,
    pub changes: Signal<Vec<GitChangeRow>>,
    pub diff: Signal<Option<GitDiffPreview>>,
    pub error: Signal<Option<SharedString>>,
    /// Opens a path's diff; the second argument says whether it is the staged
    /// or the working-tree version.
    pub on_open_diff: OpenDiff,
    pub on_back: Rc<dyn Fn()>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The Git surface.
pub fn git(host: GitHost) -> impl IntoElement {
    let branch = host.branch;
    let diff = host.diff;
    let changes = host.changes;
    let error = host.error;
    let on_refresh = host.on_refresh.clone();
    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .child(
            view()
                .w_full()
                .pt_safe_area()
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    view()
                        .w_full()
                        .h(px(48.0))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .px(px(theme::SPACING_SM))
                        .child({
                            let on_back = host.on_back.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/chevron-left.svg")),
                                copy::text("Back", "返回", "返回"),
                                move || on_back(),
                            )
                        })
                        .child(
                            text(move || match branch.get() {
                                Some(branch) => branch.to_string(),
                                None => copy::text("Changes", "变更", "變更").to_owned(),
                            })
                            .flex_1()
                            .truncate()
                            .text_size(px(theme::FONT_HEADING))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        )
                        .child({
                            let on_refresh = on_refresh.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/refresh.svg")),
                                copy::text("Refresh", "刷新", "重新整理"),
                                move || on_refresh(),
                            )
                        })
                        .child({
                            let on_close = host.on_close.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/x.svg")),
                                copy::text("Close", "关闭", "關閉"),
                                move || on_close(),
                            )
                        }),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::accent_red)
            },
        ))
        .child(when(move || diff.get().is_some(), move || diff_view(diff)))
        .child(when(
            move || diff.get().is_none(),
            move || change_list(changes, host.on_open_diff.clone()),
        ))
}

fn change_list(changes: Signal<Vec<GitChangeRow>>, on_open_diff: OpenDiff) -> impl IntoElement {
    view().flex_1().min_h(px(0.0)).child(
        virtual_list(
            move || changes.with(|changes| changes.len()),
            move |at: Position| change_row(at, changes, on_open_diff.clone()),
        )
        .keys(move || {
            changes.with(|changes| {
                changes
                    .iter()
                    .map(|change| format!("{}:{}", change.path, change.staged))
                    .collect::<Vec<_>>()
            })
        })
        .estimated_item_size(px(48.0))
        .flex_1(),
    )
}

fn change_row(
    at: Position,
    changes: Signal<Vec<GitChangeRow>>,
    on_open_diff: OpenDiff,
) -> impl IntoElement {
    let change = computed(move || changes.with(|changes| changes.get(at.index()).cloned()));
    button()
        .w_full()
        .min_h(px(48.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_LG))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .active(|style| style.opacity(0.8))
        .on_press(move || {
            let Some(change) = change.get_untracked() else {
                return;
            };
            on_open_diff(&change.path, change.staged);
        })
        .child(
            text(move || {
                change
                    .with(|change| change.as_ref().map(|change| change.status.clone()))
                    .unwrap_or_default()
            })
            .w(px(16.0))
            .text_size(px(theme::FONT_MICRO))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(move || {
                change
                    .with(|change| change.as_ref().map(|change| status_color(&change.status)))
                    .unwrap_or_else(theme::text_muted)
            }),
        )
        .child(
            text(move || {
                change
                    .with(|change| change.as_ref().map(|change| change.path.clone()))
                    .unwrap_or_default()
            })
            .flex_1()
            .truncate()
            .text_size(px(theme::FONT_DETAIL))
            .text_color(theme::foreground),
        )
        .child(
            text(move || {
                change.with(|change| {
                    change.as_ref().map_or(String::new(), |change| {
                        format!("+{} −{}", change.additions, change.deletions)
                    })
                })
            })
            .text_size(px(theme::FONT_MICRO))
            .text_color(theme::text_muted),
        )
        .child(when(
            move || change.with(|change| change.as_ref().is_some_and(|change| change.staged)),
            || {
                text("●")
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(theme::color_token("chart-1"))
            },
        ))
}

fn diff_view(diff: Signal<Option<GitDiffPreview>>) -> impl IntoElement {
    let body = computed(move || {
        diff.with(|diff| {
            diff.as_ref()
                .map(|diff| diff.body.clone())
                .unwrap_or_default()
        })
    });
    let title = computed(move || {
        diff.with(|diff| {
            diff.as_ref()
                .map(|diff| diff.title.clone())
                .unwrap_or_default()
        })
    });
    let truncated =
        computed(move || diff.with(|diff| diff.as_ref().is_some_and(|diff| diff.truncated)));
    view()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(
            text(move || title.get())
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_MICRO))
                .line_height(theme::line_height_for(theme::FONT_MICRO))
                .text_color(theme::foreground),
        )
        .child(when(
            move || truncated.get(),
            || {
                text(copy::text(
                    "The diff is larger than the preview limit.",
                    "差异超过预览上限。",
                    "差異超過預覽上限。",
                ))
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted)
            },
        ))
}

fn status_color(status: &str) -> Hsla {
    match status {
        "A" | "?" => theme::color_token("chart-1"),
        "D" => theme::accent_red(),
        "M" | "R" => theme::color_token("warning"),
        _ => theme::text_muted(),
    }
}

fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}
