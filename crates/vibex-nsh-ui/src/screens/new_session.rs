//! The new-session sheet: pick a workspace and a runtime, then start.
//!
//! The options come from the runtime itself (`list_workspaces` and
//! `list_runtime_options`): a workspace names the project it belongs to and
//! its root, and a runtime is one Agent/model/auth selection the runtime is
//! ready to run. The sheet only carries ids; the app's service owns the
//! records and the create call.

use std::rc::Rc;

use nsh_base::{SheetEvent, button, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::screens::session::{AttachmentItem, BrowseDirs};

type Action = Rc<dyn Fn()>;

/// One directory inside the authority's browse roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}
use crate::{copy, theme, ui::action};

/// A workspace the new session can live in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceChoice {
    pub id: String,
    pub name: String,
    pub detail: String,
}

/// A runtime the new session can run on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeChoice {
    pub id: String,
    pub title: String,
    pub detail: String,
}

/// What the sheet may do, and the state it shows.
#[derive(Clone)]
pub struct NewSessionHost {
    pub open: Signal<bool>,
    pub workspaces: Signal<Vec<WorkspaceChoice>>,
    pub runtimes: Signal<Vec<RuntimeChoice>>,
    /// The chosen workspace's id.
    pub workspace: Signal<Option<String>>,
    /// The chosen runtime's id.
    pub runtime: Signal<Option<String>>,
    pub busy: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    pub on_close: Rc<dyn Fn()>,
    pub on_create: Rc<dyn Fn()>,
    pub on_create_temporary: Rc<dyn Fn()>,
    /// A path to open as a project before choosing it.
    pub project_path: Signal<String>,
    pub on_open_project: Rc<dyn Fn()>,
    pub project_dirs: Signal<Vec<DirectoryEntry>>,
    pub browser_open: Signal<bool>,
    pub project_dir_path: Signal<Option<String>>,
    pub project_dir_parent: Signal<Option<String>>,
    pub on_browse_project_dirs: BrowseDirs,
    /// The session's optional title, first message and its attachments.
    pub title: Signal<String>,
    pub prompt: Signal<String>,
    pub attachments: Signal<Vec<AttachmentItem>>,
    pub on_attach_image: Action,
    pub on_attach_file: Action,
    pub on_remove_attachment: Rc<dyn Fn(&str)>,
    /// A branch name and optional base ref for a new worktree.
    pub worktree_branch: Signal<String>,
    pub base_ref: Signal<String>,
    pub on_create_worktree: Rc<dyn Fn()>,
}

/// The sheet.
pub fn new_session(host: NewSessionHost) -> impl IntoElement {
    let open = host.open;
    let error = host.error;
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .child(
            view()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("New session", "新建会话", "新增工作階段"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(crate::ui::field::text_field(
                    host.title,
                    copy::text("Title (optional)", "标题（可选）", "標題（選填）"),
                ))
                .child(crate::ui::field::text_field(
                    host.prompt,
                    copy::text(
                        "First message (optional)",
                        "第一条消息（可选）",
                        "第一則訊息（選填）",
                    ),
                ))
                .child(new_session_attachments(
                    host.attachments,
                    host.on_attach_image.clone(),
                    host.on_attach_file.clone(),
                    host.on_remove_attachment.clone(),
                ))
                .child(section_label(copy::text("Workspace", "工作区", "工作區")))
                .child(choice_list(
                    host.workspaces,
                    host.workspace,
                    |choice| choice.id.clone(),
                    |choice| (choice.name.clone(), choice.detail.clone()),
                ))
                .child(section_label(copy::text(
                    "New worktree",
                    "新建 Worktree",
                    "新增 Worktree",
                )))
                .child(
                    view()
                        .w_full()
                        .flex_row()
                        .items_end()
                        .gap(px(theme::SPACING_SM))
                        .child(crate::ui::field::text_field(
                            host.worktree_branch,
                            copy::text("Branch name", "分支名", "分支名稱"),
                        ))
                        .child({
                            let on_create_worktree = host.on_create_worktree.clone();
                            action::secondary(
                                copy::text("Create worktree", "创建 Worktree", "建立 Worktree"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                move || on_create_worktree(),
                            )
                        }),
                )
                .child(crate::ui::field::text_field(
                    host.base_ref,
                    copy::text(
                        "Base ref (optional)",
                        "基准引用（可选）",
                        "基準參照（選填）",
                    ),
                ))
                .child(section_label(copy::text(
                    "Agent and model",
                    "Agent 与模型",
                    "Agent 與模型",
                )))
                .child(choice_list(
                    host.runtimes,
                    host.runtime,
                    |choice| choice.id.clone(),
                    |choice| (choice.title.clone(), choice.detail.clone()),
                ))
                .child(when(
                    move || error.get().is_some(),
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                    },
                ))
                .child({
                    let on_create = host.on_create.clone();
                    let busy = host.busy;
                    let workspace = host.workspace;
                    let runtime = host.runtime;
                    action::primary(
                        copy::text("Create", "创建", "建立"),
                        icon(include_str!("../../assets/icons/circle-plus.svg")),
                        move || on_create(),
                    )
                    .w_full()
                    .disabled(move || {
                        busy.get()
                            || workspace.with(|workspace| workspace.is_none())
                            || runtime.with(|runtime| runtime.is_none())
                    })
                    .disabled_style(|style| style.opacity(0.4))
                }),
        )
}

fn section_label(label: &'static str) -> impl IntoElement {
    text(label)
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted)
}

/// A vertical list of choices, each a row; the chosen one is marked.
fn choice_list<T>(
    items: Signal<Vec<T>>,
    selected: Signal<Option<String>>,
    id: impl Fn(&T) -> String + Clone + 'static,
    text_of: impl Fn(&T) -> (String, String) + Clone + 'static,
) -> impl IntoElement
where
    T: Clone + PartialEq + 'static,
{
    let key_id = id.clone();
    view()
        .flex_col()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .child(each(
            move || items.get(),
            move |item| key_id(item),
            move |item, _| choice_row(item, selected, id.clone(), text_of.clone()),
        ))
}

fn choice_row<T>(
    item: ReadSignal<T>,
    selected: Signal<Option<String>>,
    id: impl Fn(&T) -> String + 'static,
    text_of: impl Fn(&T) -> (String, String) + 'static,
) -> impl IntoElement
where
    T: Clone + PartialEq + 'static,
{
    let value = computed(move || item.with(|item| text_of(item)));
    let key = computed(move || item.with(|item| id(item)));
    button()
        .w_full()
        .min_h(px(48.0))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(move || {
            if selected.with(|selected| selected.as_deref() == Some(key.get().as_str())) {
                theme::foreground()
            } else {
                theme::border_subtle()
            }
        })
        .bg(move || {
            if selected.with(|selected| selected.as_deref() == Some(key.get().as_str())) {
                theme::background_dim()
            } else {
                theme::background_card()
            }
        })
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_XS))
        .flex_col()
        .items_start()
        .on_press(move || selected.set(Some(key.get_untracked())))
        .child(
            text(move || value.get().0)
                .truncate()
                .text_size(px(theme::font_detail()))
                .text_color(theme::foreground),
        )
        .child(
            text(move || value.get().1)
                .truncate()
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

fn new_session_attachments(
    attachments: Signal<Vec<AttachmentItem>>,
    on_attach_image: Action,
    on_attach_file: Action,
    on_remove: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(
            move || !attachments.with(|items| items.is_empty()),
            move || {
                let on_remove = on_remove.clone();
                view()
                    .w_full()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(theme::SPACING_XS))
                    .child(each(
                        move || attachments.get(),
                        |attachment| attachment.id.clone(),
                        move |attachment, _| {
                            let on_remove = on_remove.clone();
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_XS))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .bg(theme::background_dim)
                                .px(px(theme::SPACING_XS))
                                .py(px(2.0))
                                .child(
                                    text(move || attachment.with(|item| item.label.clone()))
                                        .text_size(px(theme::font_micro()))
                                        .text_color(theme::text_secondary),
                                )
                                .child(
                                    button()
                                        .on_press(move || on_remove(&attachment.get_untracked().id))
                                        .hit_slop(px(8.0))
                                        .child(
                                            text("×")
                                                .text_size(px(theme::font_caption()))
                                                .text_color(theme::text_muted),
                                        ),
                                )
                        },
                    ))
            },
        ))
        .child(
            view()
                .flex_row()
                .gap(px(theme::SPACING_XS))
                .child(action::secondary(
                    copy::text("Image", "图片", "圖片"),
                    icon(include_str!("../../assets/icons/image.svg")),
                    false,
                    move || on_attach_image(),
                ))
                .child(action::secondary(
                    copy::text("File", "文件", "檔案"),
                    icon(include_str!("../../assets/icons/paperclip.svg")),
                    false,
                    move || on_attach_file(),
                )),
        )
}

pub fn project_browser_sheet(
    open: Signal<bool>,
    dirs: Signal<Vec<DirectoryEntry>>,
    current: Signal<Option<String>>,
    parent: Signal<Option<String>>,
    on_browse: BrowseDirs,
    project_path: Signal<String>,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .child(
            view()
                .w_full()
                .max_h(px(520.0))
                .gap(px(theme::SPACING_XS))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(move || {
                        current
                            .get()
                            .unwrap_or_else(|| copy::text("Folders", "文件夹", "資料夾").to_owned())
                    })
                    .truncate()
                    .text_size(px(theme::font_heading()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::foreground),
                )
                .child({
                    let on_browse = on_browse.clone();
                    when(
                        move || parent.get().is_some(),
                        move || {
                            let on_browse = on_browse.clone();
                            action::secondary(
                                copy::text("Parent folder", "上级文件夹", "上層資料夾"),
                                icon(include_str!("../../assets/icons/chevron-left.svg")),
                                false,
                                move || {
                                    if let Some(path) = parent.get_untracked() {
                                        on_browse(Some(&path));
                                    }
                                },
                            )
                            .w_full()
                        },
                    )
                })
                .child({
                    let browse_rows = on_browse.clone();
                    each(
                        move || dirs.get(),
                        |directory| directory.path.clone(),
                        move |directory, _| {
                            let on_browse = browse_rows.clone();
                            button()
                                .on_press(move || on_browse(Some(&directory.get_untracked().path)))
                                .w_full()
                                .min_h(px(40.0))
                                .px(px(theme::SPACING_SM))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_XS))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .bg(theme::background_dim)
                                .child(
                                    svg(icon(include_str!("../../assets/icons/folder.svg")))
                                        .w(px(theme::ICON_SM))
                                        .h(px(theme::ICON_SM))
                                        .text_color(theme::text_muted),
                                )
                                .child(
                                    text(move || directory.get().name)
                                        .flex_1()
                                        .truncate()
                                        .text_size(px(theme::font_body()))
                                        .text_color(theme::foreground),
                                )
                        },
                    )
                })
                .child({
                    let on_browse = on_browse.clone();
                    action::primary(
                        copy::text("Use this folder", "使用此文件夹", "使用此資料夾"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || {
                            if let Some(path) = current.get_untracked() {
                                project_path.set(path);
                                open.set(false);
                                let _ = &on_browse;
                            }
                        },
                    )
                    .w_full()
                }),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}
