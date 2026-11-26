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

use nsh_base::button;
use nsh_core::{element::ImageSrc, prelude::*};

use crate::screens::session::{
    self, AttachmentItem, BrowseDirs, CommandEntry, CommandQuery, MentionQuery, RuntimePanel,
    RuntimePickerHost, RuntimeSummaryView,
};

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

/// What the page may do, and the state it shows.
#[derive(Clone)]
pub struct NewSessionHost {
    pub workspaces: Signal<Vec<WorkspaceChoice>>,
    /// The chosen workspace's id.
    pub workspace: Signal<Option<String>>,
    /// The Session settings picker shared with open sessions.
    pub runtime: RuntimePickerHost,
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
    /// The `/`, `@`, and `$` palettes shared with open sessions.
    pub commands: Signal<Vec<CommandEntry>>,
    pub mentions: Signal<Vec<CommandEntry>>,
    pub skills: Signal<Vec<CommandEntry>>,
    pub on_command_query: CommandQuery,
    pub on_mention_query: MentionQuery,
    pub on_skill_query: MentionQuery,
    pub on_arm_entry: Rc<dyn Fn(&str)>,
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
    Attachment,
}

/// The page under the conversation's header while a session is being
/// written: the blank canvas, the prompt card, and the sheets its choices
/// open.
pub fn new_session(host: NewSessionHost) -> impl IntoElement {
    let picker = signal(None::<Picker>);
    let location = signal(Location::Workspace);
    let runtime_panel = signal(RuntimePanel::Model);
    let error = host.error;
    let busy = host.busy;
    let workspace = host.workspace;
    let workspaces = host.workspaces;
    let runtime = host.runtime.clone();
    let runtime_summary = runtime.summary;
    let runtime_busy = runtime.busy;
    let on_open_runtime = runtime.on_open.clone();
    let on_open_runtime_hero = runtime.on_open.clone();
    let commands = host.commands;
    let mentions = host.mentions;
    let skills = host.skills;
    let on_command_query = host.on_command_query.clone();
    let on_mention_query = host.on_mention_query.clone();
    let on_skill_query = host.on_skill_query.clone();
    let on_arm_entry = host.on_arm_entry.clone();
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
    let location_icon = move || match location.get() {
        Location::Workspace => icon!("folder"),
        Location::Worktree => icon!("git-branch"),
        Location::Temporary => icon!("box"),
    };
    let location_label = move || {
        SharedString::from(match location.get() {
            Location::Temporary => {
                copy::text("Temporary session", "临时会话", "暫時工作階段").to_owned()
            }
            Location::Worktree => workspace_name()
                .map(|name| format!("{name} · Worktree"))
                .unwrap_or_else(|| {
                    copy::text("New worktree", "新建 Worktree", "新增 Worktree").to_owned()
                }),
            Location::Workspace => workspace_name().unwrap_or_else(|| {
                copy::text("Choose project", "选择项目", "選擇專案").to_owned()
            }),
        })
    };
    let prompt = host.prompt;
    let branch = host.worktree_branch;
    let attachments = host.attachments;
    let can_create = move || {
        !busy.get()
            && runtime_summary
                .with(|summary| summary.as_ref().is_some_and(|summary| summary.available))
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
        .child(canvas(
            runtime_summary,
            move || busy.get() || runtime_busy.get(),
            move || {
                dismiss_keyboard();
                runtime_panel.set(RuntimePanel::Model);
                on_open_runtime_hero();
            },
            location_icon,
            location_label,
            move || {
                dismiss_keyboard();
                picker.set(Some(Picker::Location));
            },
        ))
        .child(session::composer_palettes(
            prompt,
            commands,
            mentions,
            skills,
            on_arm_entry,
        ))
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
                        .child(compose::field(
                            prompt,
                            compose::placeholder(),
                            move |text: &str| {
                                session::trigger_composer_palettes(
                                    text,
                                    commands,
                                    mentions,
                                    skills,
                                    &on_command_query,
                                    &on_mention_query,
                                    &on_skill_query,
                                );
                            },
                        ))
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
                                        .overflow_hidden()
                                        .child(session::runtime_trigger(
                                            runtime_summary,
                                            move || busy.get() || runtime_busy.get(),
                                            move || {
                                                dismiss_keyboard();
                                                runtime_panel.set(RuntimePanel::Model);
                                                on_open_runtime();
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
        .child(session::runtime_sheet(runtime, runtime_panel))
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

/// Where the messages will be: the centered Vibex lockup, the slogan naming the
/// chosen Agent (tapping its logo opens Session settings), and the workspace
/// selector centered below.
fn canvas(
    runtime_summary: Signal<Option<RuntimeSummaryView>>,
    runtime_disabled: impl Fn() -> bool + 'static,
    on_open_runtime: impl Fn() + 'static,
    location_icon: impl Fn() -> ImageSrc + 'static,
    location_label: impl IntoReactive<SharedString>,
    on_choose_location: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .flex_1()
        .min_h(px(0.0))
        .overflow_hidden()
        .items_center()
        .justify_center()
        .px(px(theme::SPACING_XL))
        .gap(px(theme::SPACING_MD))
        .child(brand::wordmark_at(theme::font_wordmark()))
        .child(slogan_line(
            runtime_summary,
            runtime_disabled,
            on_open_runtime,
        ))
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .justify_center()
                .pt(px(theme::SPACING_SM))
                .child(
                    button()
                        .accessibility_label(copy::text(
                            "Choose workspace",
                            "选择工作区",
                            "選擇工作區",
                        ))
                        .on_press(on_choose_location)
                        .h(px(34.0))
                        .max_w(relative(1.0))
                        .min_w(px(0.0))
                        .flex_shrink(1.0)
                        .pl(px(12.0))
                        .pr(px(10.0))
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .rounded_full()
                        .bg(theme::background_card)
                        .active(|style| style.opacity(0.65))
                        .child(dynamic(move || {
                            svg(location_icon())
                                .size(px(14.0))
                                .flex_shrink_0()
                                .text_color(theme::text_secondary)
                        }))
                        .child(
                            text(location_label)
                                .min_w(px(0.0))
                                .flex_shrink(1.0)
                                .truncate()
                                .text_size(px(theme::font_caption()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::foreground),
                        )
                        .child(
                            svg(icon!("chevron-down"))
                                .size(px(11.0))
                                .flex_shrink_0()
                                .text_color(theme::text_muted),
                        ),
                ),
        )
}

fn slogan_parts() -> (&'static str, &'static str) {
    match copy::language() {
        copy::Language::SimplifiedChinese => ("和", "开始新的工作"),
        copy::Language::TraditionalChinese => ("和", "開始新的工作"),
        _ => ("Start something new with", ""),
    }
}

fn slogan_word(label: &'static str) -> impl IntoElement {
    text(label)
        .text_size(px(theme::font_title()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::foreground)
}

/// The new-session slogan carrying the selected Agent's brand mark.
///
/// Tapping the logo opens the unified Session settings sheet directly so the
/// reader can switch Agent, model, or options right from the hero.
fn slogan_line(
    runtime_summary: Signal<Option<RuntimeSummaryView>>,
    disabled: impl Fn() -> bool + 'static,
    on_open_runtime: impl Fn() + 'static,
) -> impl IntoElement {
    let (lead, trail) = slogan_parts();
    let mark_button = button()
        .test_id("new-session-agent-mark")
        .accessibility_label(copy::text("Open session settings", "打开会话设置", "開啟工作階段設定"))
        .on_press(on_open_runtime)
        .disabled(disabled)
        .size(px(38.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CARD))
        .active(|style| style.bg(theme::fill()).opacity(0.75))
        .disabled_style(|style| style.opacity(0.45))
        .child(dynamic(move || {
            let mark = runtime_summary.with(|summary| {
                summary.as_ref().map_or_else(
                    || icon!("sparkles"),
                    |summary| {
                        session::agent_mark_svg(
                            &summary.agent_id,
                            &summary.agent,
                            &summary.model_id,
                        )
                    },
                )
            });
            svg(mark)
                .size(px(28.0))
                .flex_shrink_0()
                .text_color(theme::foreground)
        }));
    let mut row = view()
        .w_full()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM));
    if !lead.is_empty() {
        row = row.child(slogan_word(lead));
    }
    row = row.child(mark_button);
    if !trail.is_empty() {
        row = row.child(slogan_word(trail));
    }
    row
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
    /// The directories the runtime offers as starting points, in its own
    /// order. The sheet lists them so a reader can jump between the places the
    /// host allows instead of climbing to them one folder at a time.
    pub roots: Signal<Vec<String>>,
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
        roots,
        busy,
        error,
        on_browse,
        on_open,
    } = host;
    // The path the reader is typing, and whether the field is showing: the
    // sheet opens on the list, and a reader who knows where they are going can
    // type it instead of walking there.
    let typed = signal(String::new());
    let typing = signal(false);
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
                                    // The path is the field: tap it and type a
                                    // directory the list would take too many
                                    // steps to reach.
                                    view()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(when(
                                            move || typing.get(),
                                            {
                                                let on_browse = on_browse.clone();
                                                move || {
                                                    let on_browse = on_browse.clone();
                                                    view()
                                                        .w_full()
                                                        .flex_row()
                                                        .items_center()
                                                        .gap(px(theme::SPACING_SM))
                                                        .child(
                                                            view().flex_1().min_w(px(0.0)).child(
                                                                field::text_field_well(
                                                                    typed,
                                                                    copy::text(
                                                                        "Type a folder path",
                                                                        "输入文件夹路径",
                                                                        "輸入資料夾路徑",
                                                                    ),
                                                                )
                                                                .test_id("project-path"),
                                                            ),
                                                        )
                                                        .child(surface::ghost_button(
                                                            icon!("check"),
                                                            copy::text("Open", "打开", "開啟"),
                                                            move || {
                                                                let path = typed
                                                                    .get_untracked()
                                                                    .trim()
                                                                    .to_owned();
                                                                if !path.is_empty() {
                                                                    on_browse(Some(&path));
                                                                    typing.set(false);
                                                                }
                                                            },
                                                        ))
                                                }
                                            },
                                        ))
                                        .child(when(
                                            move || !typing.get(),
                                            move || {
                                                // The path reads as a field
                                                // you can type into, so it is
                                                // one press away from being
                                                // exactly that.
                                                view()
                                                    .w_full()
                                                    .flex_row()
                                                    .items_center()
                                                    .active(|style| style.opacity(0.6))
                                                    .on_press(move || {
                                                        typed.set(
                                                            current
                                                                .get_untracked()
                                                                .unwrap_or_default(),
                                                        );
                                                        typing.set(true);
                                                    })
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
                                                        .w_full()
                                                        .truncate()
                                                        .text_size(px(theme::font_detail()))
                                                        .font_family("monospace")
                                                        .text_color(theme::foreground),
                                                    )
                                            },
                                        )),
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
                // The places the runtime offers, as the desktop's picker offers
                // its own: one tap to a root instead of climbing back to it.
                // They are the roots the authority allows, so a reader can see
                // where they are before walking there.
                view().w_full().px(px(theme::SPACING_MD)).pb(px(theme::SPACING_SM)).child(
                    when(
                        move || roots.with(Vec::len) > 1,
                        {
                            let on_browse = on_browse.clone();
                            move || {
                                let on_browse = on_browse.clone();
                                surface::card().child(each(
                                    move || roots.get(),
                                    |root| root.clone(),
                                    move |root, _| {
                                        let on_browse = on_browse.clone();
                                        surface::row(
                                            icon!("folder"),
                                            move || {
                                                SharedString::from(root.with(|path| {
                                                    path.rsplit('/')
                                                        .next()
                                                        .filter(|name| !name.is_empty())
                                                        .unwrap_or(path.as_str())
                                                        .to_owned()
                                                }))
                                            },
                                            move || {
                                                on_browse(Some(
                                                    &root.with_untracked(|path| path.clone()),
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
