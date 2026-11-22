//! The new-session page: pick a workspace and a runtime, then start.
//!
//! The options come from the runtime itself (`list_workspaces` and
//! `list_runtime_options`): a workspace names the project it belongs to and
//! its root, and a runtime is one Agent/model/auth selection the runtime is
//! ready to run. The sheet only carries ids; the app's service owns the
//! records and the create call.

use std::rc::Rc;

use nsh_base::{SheetEvent, button, input, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::screens::session::{AttachmentItem, BrowseDirs};

type Action = Rc<dyn Fn()>;

/// One directory inside the authority's browse roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}
use crate::{
    copy, theme,
    ui::{action, brand, field, sheet as chrome},
};

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

/// What the page may do, and the state it shows.
#[derive(Clone)]
pub struct NewSessionHost {
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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Location {
    #[default]
    Workspace,
    Worktree,
    Temporary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Location,
    Runtime,
    Attachment,
}

/// The full-screen first-message composer and its choice sheets.
pub fn new_session(host: NewSessionHost) -> impl IntoElement {
    let picker = signal(None::<Picker>);
    let location = signal(Location::Workspace);
    let error = host.error;
    let busy = host.busy;
    let workspace = host.workspace;
    let workspaces = host.workspaces;
    let runtime = host.runtime;
    let runtimes = host.runtimes;
    let workspace_label = move || {
        if location.get() == Location::Temporary {
            return copy::text("Temporary session", "临时会话", "暫時工作階段").to_owned();
        }
        workspaces.with(|choices| {
            workspace
                .with(|selected| {
                    choices
                        .iter()
                        .find(|choice| selected.as_ref() == Some(&choice.id))
                })
                .map(|choice| choice.name.clone())
                .unwrap_or_else(|| copy::text("Choose project", "选择项目", "選擇專案").to_owned())
        })
    };
    let runtime_label = move || {
        runtimes.with(|choices| {
            runtime
                .with(|selected| {
                    choices
                        .iter()
                        .find(|choice| selected.as_ref() == Some(&choice.id))
                })
                .map(|choice| choice.title.clone())
                .unwrap_or_else(|| {
                    copy::text("Agent and model", "Agent 与模型", "Agent 與模型").to_owned()
                })
        })
    };
    let close = host.on_close.clone();
    let prompt = host.prompt;
    let branch = host.worktree_branch;
    let can_create = move || {
        !busy.get()
            && runtime.with(Option::is_some)
            && match location.get() {
                Location::Temporary => true,
                Location::Workspace => workspace.with(Option::is_some),
                Location::Worktree => {
                    workspace.with(Option::is_some) && branch.with(|name| !name.trim().is_empty())
                }
            }
    };
    view()
        .absolute()
        .inset(px(0.0))
        .flex_col()
        .bg(theme::background)
        .pt_safe_area()
        .pb_keyboard()
        .child(chrome::sheet_header(
            copy::text("New session", "新建会话", "新增工作階段"),
            move || {
                dismiss_keyboard();
                close();
            },
        ))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .justify_center()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(brand::wordmark())
                .child(
                    text(workspace_label)
                        .w_full()
                        .text_center()
                        .px(px(theme::SPACING_LG))
                        .truncate()
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            view()
                .w_full()
                .flex_shrink_0()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_LG))
                .pb(px(theme::SPACING_MD))
                .child(when(
                    move || error.with(Option::is_some),
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                    },
                ))
                .child(when(
                    move || location.get() == Location::Worktree,
                    move || {
                        view()
                            .gap(px(theme::SPACING_XS))
                            .child(field::text_field(
                                branch,
                                copy::text("Branch name", "分支名", "分支名稱"),
                            ))
                            .child(field::text_field(
                                host.base_ref,
                                copy::text(
                                    "Base ref (optional)",
                                    "基准引用（可选）",
                                    "基準參照（選填）",
                                ),
                            ))
                    },
                ))
                .child(
                    view()
                        .p(px(theme::SPACING_SM))
                        .gap(px(theme::SPACING_XS))
                        .rounded(px(theme::RADIUS_COMPOSER))
                        .border_1()
                        .border_color(theme::border_default)
                        .bg(theme::composer_surface)
                        .child(new_session_attachments(
                            host.attachments,
                            host.on_remove_attachment.clone(),
                        ))
                        .child(
                            input()
                                .value(move || prompt.get())
                                .on_change(move |value: &str| prompt.set(value.to_owned()))
                                .multiline(true)
                                .h(px(70.0))
                                .w_full()
                                .p(px(theme::SPACING_XS))
                                .placeholder(copy::text(
                                    "First message",
                                    "第一条消息",
                                    "第一則訊息",
                                ))
                                .placeholder_color(theme::muted_foreground)
                                .text_color(theme::foreground)
                                .text_size(px(theme::font_body())),
                        )
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .child(action::icon_button(
                                    icon!("plus"),
                                    copy::text("Attach a file", "添加附件", "新增附件"),
                                    move || {
                                        dismiss_keyboard();
                                        picker.set(Some(Picker::Attachment));
                                    },
                                ))
                                .child(
                                    button()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .h(px(theme::TOUCH_TARGET))
                                        .items_start()
                                        .justify_center()
                                        .on_press(move || {
                                            dismiss_keyboard();
                                            picker.set(Some(Picker::Runtime));
                                        })
                                        .child(
                                            text(runtime_label)
                                                .truncate()
                                                .w_full()
                                                .text_size(px(theme::font_caption()))
                                                .text_color(theme::foreground),
                                        ),
                                )
                                .child(
                                    action::icon_button(
                                        icon!("send"),
                                        copy::text("Create", "创建", "建立"),
                                        move || {
                                            dismiss_keyboard();
                                            match location.get_untracked() {
                                                Location::Workspace => (host.on_create)(),
                                                Location::Worktree => (host.on_create_worktree)(),
                                                Location::Temporary => (host.on_create_temporary)(),
                                            }
                                        },
                                    )
                                    .size(px(theme::COMPOSER_SEND_SIZE))
                                    .rounded_full()
                                    .bg(theme::action_primary)
                                    .disabled(move || !can_create())
                                    .disabled_style(|style| style.opacity(0.4)),
                                ),
                        ),
                )
                .child(
                    button()
                        .h(px(theme::TOUCH_TARGET))
                        .w_full()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .accessibility_label(copy::text(
                            "Choose workspace",
                            "选择工作区",
                            "選擇工作區",
                        ))
                        .on_press(move || {
                            dismiss_keyboard();
                            picker.set(Some(Picker::Location));
                        })
                        .child(
                            svg(icon!("folder"))
                                .size(px(theme::ICON_SM))
                                .text_color(theme::text_muted),
                        )
                        .child(
                            text(workspace_label)
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme::font_caption()))
                                .text_color(theme::text_secondary),
                        )
                        .child(
                            svg(icon!("chevron-down"))
                                .size(px(theme::ICON_SM))
                                .text_color(theme::text_muted),
                        ),
                ),
        )
        .child(when(
            move || keyboard_height().0 == 0.0,
            || view().h(safe_area().bottom),
        ))
        .child(choice_sheet(
            picker,
            Picker::Location,
            copy::text("Workspace", "工作区", "工作區"),
            view()
                .gap(px(theme::SPACING_XS))
                .child(choice_list(
                    workspaces,
                    workspace,
                    |choice| choice.id.clone(),
                    |choice| (choice.name.clone(), choice.detail.clone()),
                    move || {
                        location.set(Location::Workspace);
                        picker.set(None);
                    },
                ))
                .child(chrome::sheet_entry(
                    icon!("git-branch"),
                    copy::text("New worktree", "新建 Worktree", "新增 Worktree"),
                    false,
                    move || {
                        location.set(Location::Worktree);
                        picker.set(None);
                    },
                ))
                .child(chrome::sheet_entry(
                    icon!("box"),
                    copy::text("Temporary session", "临时会话", "暫時工作階段"),
                    false,
                    move || {
                        location.set(Location::Temporary);
                        picker.set(None);
                    },
                ))
                .child(field::text_field(
                    host.title,
                    copy::text("Title (optional)", "标题（可选）", "標題（選填）"),
                )),
        ))
        .child(choice_sheet(
            picker,
            Picker::Runtime,
            copy::text("Agent and model", "Agent 与模型", "Agent 與模型"),
            choice_list(
                runtimes,
                runtime,
                |choice| choice.id.clone(),
                |choice| (choice.title.clone(), choice.detail.clone()),
                move || picker.set(None),
            ),
        ))
        .child(choice_sheet(
            picker,
            Picker::Attachment,
            copy::text("Add attachment", "添加附件", "新增附件"),
            view()
                .child(chrome::sheet_entry(
                    icon!("image"),
                    copy::text("Image", "图片", "圖片"),
                    false,
                    move || {
                        picker.set(None);
                        (host.on_attach_image)();
                    },
                ))
                .child(chrome::sheet_entry(
                    icon!("file-plus"),
                    copy::text("File", "文件", "檔案"),
                    false,
                    move || {
                        picker.set(None);
                        (host.on_attach_file)();
                    },
                )),
        ))
}

fn choice_sheet(
    picker: Signal<Option<Picker>>,
    current: Picker,
    title: &'static str,
    content: impl IntoElement,
) -> impl IntoElement {
    chrome::bottom_sheet(
        move || picker.get() == Some(current),
        move || picker.set(None),
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.75))
            .min_h(px(0.0))
            .child(chrome::sheet_header(title, move || picker.set(None)))
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    view()
                        .w_full()
                        .px(px(theme::SPACING_MD))
                        .pt(px(theme::SPACING_SM))
                        .pb_safe_area_plus(px(theme::SPACING_LG))
                        .child(content),
                ),
            ),
    )
}

/// A vertical list of choices, each a row; the chosen one is marked.
fn choice_list<T>(
    items: Signal<Vec<T>>,
    selected: Signal<Option<String>>,
    id: impl Fn(&T) -> String + Clone + 'static,
    text_of: impl Fn(&T) -> (String, String) + Clone + 'static,
    on_select: impl Fn() + Copy + 'static,
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
            move |item, _| choice_row(item, selected, id.clone(), text_of.clone(), on_select),
        ))
}

fn choice_row<T>(
    item: ReadSignal<T>,
    selected: Signal<Option<String>>,
    id: impl Fn(&T) -> String + 'static,
    text_of: impl Fn(&T) -> (String, String) + 'static,
    on_select: impl Fn() + 'static,
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
        .on_press(move || {
            selected.set(Some(key.get_untracked()));
            on_select();
        })
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
}

#[derive(Clone)]
pub struct ProjectBrowserHost {
    pub open: Signal<bool>,
    pub dirs: Signal<Vec<DirectoryEntry>>,
    pub current: Signal<Option<String>>,
    pub parent: Signal<Option<String>>,
    pub busy: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    pub on_browse: BrowseDirs,
    pub on_open: ProjectOpen,
}

type ProjectOpen = Rc<dyn Fn(&str)>;

pub fn project_browser_sheet(host: ProjectBrowserHost) -> impl IntoElement {
    let ProjectBrowserHost {
        open,
        dirs,
        current,
        parent,
        busy,
        error,
        on_browse,
        on_open,
    } = host;
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
                        current.get().unwrap_or_else(|| {
                            copy::text("New project", "新建项目", "新增專案").to_owned()
                        })
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
                            .disabled(move || busy.get())
                        },
                    )
                })
                .child({
                    let browse_rows = on_browse.clone();
                    scroll()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(view().w_full().child(each(
                            move || dirs.get(),
                            |directory| directory.path.clone(),
                            move |directory, _| {
                                let on_browse = browse_rows.clone();
                                button()
                                    .on_press(move || {
                                        on_browse(Some(&directory.get_untracked().path))
                                    })
                                    .disabled(move || busy.get())
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
                        )))
                })
                .child(when(
                    move || busy.get(),
                    || {
                        text(copy::text("Loading…", "正在加载…", "正在載入…"))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::text_muted)
                    },
                ))
                .child(when(
                    move || error.with(Option::is_some),
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                    },
                ))
                .child({
                    action::primary(
                        copy::text("Open project", "打开项目", "開啟專案"),
                        icon!("folder-open"),
                        move || {
                            if let Some(path) = current.get_untracked() {
                                on_open(&path);
                            }
                        },
                    )
                    .w_full()
                    .disabled(move || busy.get() || current.with(Option::is_none))
                }),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}
