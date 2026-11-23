//! A session being written: the conversation page before its first message.
//!
//! A new session has no page of its own. It takes the transcript's place under
//! the conversation's header: a greeting and a few ways to begin where the
//! messages will be, and the prompt card where it always is, with the two
//! things a first message also has to say on its control row: where the
//! session runs, and what runs it.
//!
//! The options come from the runtime itself (`list_workspaces` and
//! `list_runtime_options`): a workspace names the project it belongs to and
//! its root, and a runtime is one Agent/model/auth selection the runtime is
//! ready to run. The page only carries ids; the app's service owns the
//! records and the create call.

use std::rc::Rc;

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
    ui::{action, brand, compose, field, sheet as chrome, surface},
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

/// One way to begin: what it is for, and the message it starts.
struct Starter {
    icon: ImageSrc,
    title: &'static str,
    caption: &'static str,
    prompt: &'static str,
}

fn starters() -> [Starter; 3] {
    [
        Starter {
            icon: icon!("book-open-text"),
            title: copy::text("Tour this project", "了解这个项目", "了解這個專案"),
            caption: copy::text(
                "How it is laid out and how to run it",
                "梳理代码结构与运行方式",
                "梳理程式碼結構與執行方式",
            ),
            prompt: copy::text(
                "Give me a tour of this project: what it does, how the code is laid out, and how to build, run and test it.",
                "带我了解这个项目：它是做什么的、代码如何组织，以及如何构建、运行和测试。",
                "帶我了解這個專案：它是做什麼的、程式碼如何組織，以及如何建置、執行和測試。",
            ),
        },
        Starter {
            icon: icon!("list-checks"),
            title: copy::text("Review my changes", "审查当前改动", "審查目前變更"),
            caption: copy::text(
                "Find problems in the working tree",
                "检查工作区里尚未提交的修改",
                "檢查工作區裡尚未提交的修改",
            ),
            prompt: copy::text(
                "Review the uncommitted changes in the working tree. Point out bugs, risky edits and anything that needs a test.",
                "审查工作区中尚未提交的改动，指出其中的缺陷、有风险的修改，以及需要补充测试的地方。",
                "審查工作區中尚未提交的變更，指出其中的缺陷、有風險的修改，以及需要補充測試的地方。",
            ),
        },
        Starter {
            icon: icon!("wrench"),
            title: copy::text("Fix a problem", "修复一个问题", "修復一個問題"),
            caption: copy::text(
                "Say what goes wrong and trace the cause",
                "描述现象，定位原因并修复",
                "描述現象，定位原因並修復",
            ),
            prompt: copy::text(
                "Something is not working as expected. Here is what happens: ",
                "有个地方的行为不符合预期，现象是：",
                "有個地方的行為不符合預期，現象是：",
            ),
        },
    ]
}

/// The page under the conversation's header while a session is being
/// written: the blank canvas, the prompt card, and the sheets its choices
/// open.
pub fn new_session(host: NewSessionHost) -> impl IntoElement {
    let picker = signal(None::<Picker>);
    let location = signal(Location::Workspace);
    let input = element_ref();
    let error = host.error;
    let busy = host.busy;
    let workspace = host.workspace;
    let workspaces = host.workspaces;
    let runtime = host.runtime;
    let runtimes = host.runtimes;
    let workspace_name = move || {
        workspaces.with(|choices| {
            workspace
                .with(|selected| {
                    choices
                        .iter()
                        .find(|choice| selected.as_ref() == Some(&choice.id))
                })
                .map(|choice| choice.name.clone())
        })
    };
    let location_label = move || {
        SharedString::from(match location.get() {
            Location::Temporary => {
                copy::text("Temporary session", "临时会话", "暫時工作階段").to_owned()
            }
            _ => workspace_name().unwrap_or_else(|| {
                copy::text("Choose project", "选择项目", "選擇專案").to_owned()
            }),
        })
    };
    let runtime_label = move || {
        SharedString::from(runtimes.with(|choices| {
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
        }))
    };
    let prompt = host.prompt;
    let branch = host.worktree_branch;
    let attachments = host.attachments;
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
    let create = {
        let host = host.clone();
        move || {
            dismiss_keyboard();
            match location.get_untracked() {
                Location::Workspace => (host.on_create)(),
                Location::Worktree => (host.on_create_worktree)(),
                Location::Temporary => (host.on_create_temporary)(),
            }
        }
    };
    let on_remove_attachment = host.on_remove_attachment.clone();
    view()
        .flex_1()
        .min_h(px(0.0))
        .flex_col()
        .pb_keyboard()
        .child(canvas(prompt, input))
        .child(
            view()
                .w_full()
                .flex_shrink_0()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_MD))
                .pb(px(theme::SPACING_SM))
                .child(when(
                    move || error.with(Option::is_some),
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .px(px(theme::SPACING_SM))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                    },
                ))
                .child(when(
                    move || location.get() == Location::Worktree,
                    move || {
                        // A new worktree is a branch of the chosen project:
                        // its name is asked for where the message is written.
                        surface::card()
                            .p(px(theme::SPACING_SM))
                            .gap(px(theme::SPACING_SM))
                            .child(field::text_field_well(
                                branch,
                                copy::text("Branch name", "分支名", "分支名稱"),
                            ))
                            .child(field::text_field_well(
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
                    compose::card()
                        .child(when(
                            move || !attachments.with(Vec::is_empty),
                            move || {
                                let on_remove = on_remove_attachment.clone();
                                compose::chips().child(each(
                                    move || attachments.get(),
                                    |attachment| attachment.id.clone(),
                                    move |attachment, _| {
                                        let on_remove = on_remove.clone();
                                        compose::chip(
                                            move || {
                                                SharedString::from(
                                                    attachment.with(|item| item.label.clone()),
                                                )
                                            },
                                            move || {
                                                on_remove(
                                                    &attachment.with_untracked(|item| {
                                                        item.id.clone()
                                                    }),
                                                )
                                            },
                                        )
                                    },
                                ))
                            },
                        ))
                        .child(
                            compose::field(
                                prompt,
                                copy::text("First message", "第一条消息", "第一則訊息"),
                                |_: &str| {},
                            )
                            .element_ref(&input),
                        )
                        .child(
                            compose::controls()
                                .child(compose::add_button(
                                    copy::text("Attach a file", "添加附件", "新增附件"),
                                    move || {
                                        dismiss_keyboard();
                                        picker.set(Some(Picker::Attachment));
                                    },
                                ))
                                .child(
                                    view()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex_row()
                                        .items_center()
                                        .gap(px(theme::SPACING_XS))
                                        .overflow_hidden()
                                        .child(
                                            compose::pill(
                                                icon!("folder"),
                                                location_label,
                                                move || {
                                                    dismiss_keyboard();
                                                    picker.set(Some(Picker::Location));
                                                },
                                            )
                                            .accessibility_label(copy::text(
                                                "Choose workspace",
                                                "选择工作区",
                                                "選擇工作區",
                                            )),
                                        )
                                        .child(compose::pill(
                                            icon!("sparkles"),
                                            runtime_label,
                                            move || {
                                                dismiss_keyboard();
                                                picker.set(Some(Picker::Runtime));
                                            },
                                        )),
                                )
                                .child(compose::send_button(
                                    icon!("send"),
                                    copy::text("Create", "创建", "建立"),
                                    can_create,
                                    create,
                                )),
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
                .w_full()
                .gap(px(theme::SPACING_MD))
                .child(when(
                    move || !workspaces.with(Vec::is_empty),
                    move || {
                        chrome::sheet_card().child(choice_list(
                            workspaces,
                            icon!("folder"),
                            |choice: &WorkspaceChoice| choice.id.clone(),
                            |choice: &WorkspaceChoice| (choice.name.clone(), choice.detail.clone()),
                            move |id| {
                                location.get() == Location::Workspace
                                    && workspace.with(|selected| selected.as_deref() == Some(id))
                            },
                            move |id| {
                                workspace.set(Some(id.to_owned()));
                                location.set(Location::Workspace);
                                picker.set(None);
                            },
                        ))
                    },
                ))
                .child(
                    chrome::sheet_card()
                        .child(
                            surface::row_captioned(
                                icon!("git-branch"),
                                copy::text("New worktree", "新建 Worktree", "新增 Worktree"),
                                copy::text(
                                    "A new branch of the chosen project",
                                    "基于所选项目新开一个分支",
                                    "基於所選專案新開一個分支",
                                ),
                                move || {
                                    location.set(Location::Worktree);
                                    picker.set(None);
                                },
                            )
                            .child(chosen_mark(move || location.get() == Location::Worktree)),
                        )
                        .child(surface::row_divider())
                        .child(
                            surface::row_captioned(
                                icon!("box"),
                                copy::text("Temporary session", "临时会话", "暫時工作階段"),
                                copy::text(
                                    "A scratch directory, outside any project",
                                    "使用临时目录，不属于任何项目",
                                    "使用暫存目錄，不屬於任何專案",
                                ),
                                move || {
                                    location.set(Location::Temporary);
                                    picker.set(None);
                                },
                            )
                            .child(chosen_mark(move || location.get() == Location::Temporary)),
                        ),
                )
                .child(
                    view()
                        .w_full()
                        .gap(px(theme::SPACING_SM))
                        .child(surface::group_label(copy::text(
                            "Session title",
                            "会话标题",
                            "工作階段標題",
                        )))
                        .child(field::text_field_well(
                            host.title,
                            copy::text("Title (optional)", "标题（可选）", "標題（選填）"),
                        )),
                ),
        ))
        .child(choice_sheet(
            picker,
            Picker::Runtime,
            copy::text("Agent and model", "Agent 与模型", "Agent 與模型"),
            chrome::sheet_card().child(choice_list(
                runtimes,
                icon!("sparkles"),
                |choice: &RuntimeChoice| choice.id.clone(),
                |choice: &RuntimeChoice| (choice.title.clone(), choice.detail.clone()),
                move |id| runtime.with(|selected| selected.as_deref() == Some(id)),
                move |id| {
                    runtime.set(Some(id.to_owned()));
                    picker.set(None);
                },
            )),
        ))
        .child(choice_sheet(
            picker,
            Picker::Attachment,
            copy::text("Add attachment", "添加附件", "新增附件"),
            chrome::sheet_card()
                .child(surface::row_captioned(
                    icon!("image"),
                    copy::text("Image", "图片", "圖片"),
                    copy::text(
                        "Scaled down before it is sent",
                        "发送前会先缩放",
                        "傳送前會先縮放",
                    ),
                    move || {
                        picker.set(None);
                        (host.on_attach_image)();
                    },
                ))
                .child(surface::row_divider())
                .child(surface::row_captioned(
                    icon!("file-plus"),
                    copy::text("File", "文件", "檔案"),
                    copy::text(
                        "Sent as it is, up to 8 MB",
                        "原样发送，最大 8 MB",
                        "原樣傳送，最大 8 MB",
                    ),
                    move || {
                        picker.set(None);
                        (host.on_attach_file)();
                    },
                )),
        ))
}

/// Where the messages will be: a greeting, and a few ways to begin. The ways
/// to begin give way to the keyboard, the greeting stays.
fn canvas(prompt: Signal<String>, input: ElementRef) -> impl IntoElement {
    view()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .justify_center()
        .px(px(theme::SPACING_XL))
        .gap(px(theme::SPACING_SM))
        .child(
            view()
                .size(px(44.0))
                .mb(px(theme::SPACING_SM))
                .rounded(px(theme::RADIUS_BUTTON))
                .bg(theme::fill)
                .items_center()
                .justify_center()
                .child(brand::mark(22.0)),
        )
        .child(
            text(copy::text(
                "What are we working on?",
                "这次要做点什么？",
                "這次要做點什麼？",
            ))
            .text_size(px(theme::font_greeting()))
            .font_weight(FontWeight::BOLD)
            .text_color(theme::foreground),
        )
        .child(
            text(copy::text(
                "Describe the task, or begin with one of these.",
                "描述要做的事，或者从下面选一个开始。",
                "描述要做的事，或者從下面選一個開始。",
            ))
            .text_size(px(theme::font_detail()))
            .line_height(theme::line_height_for(theme::font_detail()))
            .text_color(theme::text_secondary),
        )
        .child(when(
            move || keyboard_height().0 == 0.0,
            move || {
                view()
                    .w_full()
                    .pt(px(theme::SPACING_LG))
                    .gap(px(theme::SPACING_SM))
                    .children(starters().map(|starter| {
                        let Starter {
                            icon,
                            title,
                            caption,
                            prompt: begin,
                        } = starter;
                        surface::card().child(
                            surface::row_captioned(icon, title, caption, move || {
                                prompt.set(begin.to_owned());
                                input.focus();
                            })
                            .min_h(px(60.0)),
                        )
                    }))
            },
        ))
}

/// The mark a chosen row of a sheet wears at its end.
fn chosen_mark(chosen: impl Fn() -> bool + 'static) -> impl IntoElement {
    when(chosen, || {
        svg(icon!("check"))
            .size(px(theme::ICON_MD))
            .flex_shrink_0()
            .text_color(theme::foreground)
    })
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
                scroll()
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .child(chrome::sheet_body().child(content)),
            ),
    )
}

/// The rows of one choice: a title, a line of detail, and a mark on the one
/// that is chosen.
fn choice_list<T>(
    items: Signal<Vec<T>>,
    icon: ImageSrc,
    id: impl Fn(&T) -> String + Clone + 'static,
    text_of: impl Fn(&T) -> (String, String) + Clone + 'static,
    chosen: impl Fn(&str) -> bool + Clone + 'static,
    on_select: impl Fn(&str) + Clone + 'static,
) -> impl IntoElement
where
    T: Clone + PartialEq + 'static,
{
    let key_id = id.clone();
    view().flex_col().w_full().child(each(
        move || items.get(),
        move |item| key_id(item),
        move |item, _| {
            let value = {
                let text_of = text_of.clone();
                computed(move || item.with(|item| text_of(item)))
            };
            let key = {
                let id = id.clone();
                computed(move || item.with(|item| id(item)))
            };
            let chosen = chosen.clone();
            let on_select = on_select.clone();
            surface::row_captioned(
                icon.clone(),
                move || SharedString::from(value.get().0),
                move || SharedString::from(value.get().1),
                move || on_select(&key.get_untracked()),
            )
            .border_b_1()
            .border_b_color(theme::hairline)
            .child(chosen_mark(move || chosen(&key.get())))
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

/// The directory browser a new project is opened from: where it stands, the
/// way up, the directories below, and the action that opens the one shown.
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
    chrome::bottom_sheet(
        move || open.get(),
        move || open.set(false),
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.8))
            .min_h(px(0.0))
            .child(chrome::sheet_header(
                copy::text("New project", "新建项目", "新增專案"),
                move || open.set(false),
            ))
            .child(
                view()
                    .w_full()
                    .px(px(theme::SPACING_MD))
                    .pb(px(theme::SPACING_MD))
                    .child(
                        surface::card().child(
                            view()
                                .w_full()
                                .min_h(px(theme::ROW_HEIGHT))
                                .px(px(surface::ROW_INSET))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_MD))
                                .child(surface::tile(icon!("folder-open")))
                                .child(
                                    text(move || {
                                        current.get().unwrap_or_else(|| {
                                            copy::text(
                                                "Choose a folder",
                                                "选择一个文件夹",
                                                "選擇一個資料夾",
                                            )
                                            .to_owned()
                                        })
                                    })
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .font_family("monospace")
                                    .text_color(theme::foreground),
                                )
                                .child({
                                    let on_browse = on_browse.clone();
                                    when(
                                        move || parent.with(Option::is_some),
                                        move || {
                                            let on_browse = on_browse.clone();
                                            surface::ghost_button(
                                                icon!("arrow-to-top"),
                                                copy::text(
                                                    "Parent folder",
                                                    "上级文件夹",
                                                    "上層資料夾",
                                                ),
                                                move || {
                                                    if let Some(path) = parent.get_untracked() {
                                                        on_browse(Some(&path));
                                                    }
                                                },
                                            )
                                            .disabled(move || busy.get())
                                        },
                                    )
                                }),
                        ),
                    ),
            )
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    view()
                        .w_full()
                        .px(px(theme::SPACING_MD))
                        .child(when(
                            move || !dirs.with(Vec::is_empty),
                            {
                                let on_browse = on_browse.clone();
                                move || {
                                    let on_browse = on_browse.clone();
                                    surface::card().child(each(
                                        move || dirs.get(),
                                        |directory| directory.path.clone(),
                                        move |directory, _| {
                                            let on_browse = on_browse.clone();
                                            surface::row(
                                                icon!("folder"),
                                                move || {
                                                    SharedString::from(
                                                        directory.with(|entry| entry.name.clone()),
                                                    )
                                                },
                                                move || {
                                                    on_browse(Some(
                                                        &directory
                                                            .with_untracked(|entry| entry.path.clone()),
                                                    ))
                                                },
                                            )
                                            .border_b_1()
                                            .border_b_color(theme::hairline)
                                            .disabled(move || busy.get())
                                            .child(surface::chevron())
                                        },
                                    ))
                                }
                            },
                        ))
                        .child(when(
                            move || busy.get(),
                            || {
                                text(copy::text("Loading…", "正在加载…", "正在載入…"))
                                    .p(px(theme::SPACING_SM))
                                    .text_size(px(theme::font_caption()))
                                    .text_color(theme::text_muted)
                            },
                        ))
                        .child(when(
                            move || error.with(Option::is_some),
                            move || {
                                text(move || error.get().unwrap_or_default())
                                    .p(px(theme::SPACING_SM))
                                    .text_size(px(theme::font_caption()))
                                    .text_color(theme::accent_red)
                            },
                        )),
                ),
            )
            .child(
                chrome::sheet_body().pt(px(theme::SPACING_MD)).child(
                    action::solid(copy::text("Open project", "打开项目", "開啟專案"), move || {
                        if let Some(path) = current.get_untracked() {
                            on_open(&path);
                        }
                    })
                    .w_full()
                    .disabled(move || busy.get() || current.with(Option::is_none)),
                ),
            ),
    )
}
