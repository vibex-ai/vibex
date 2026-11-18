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
                        .text_size(px(theme::FONT_HEADING))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(section_label(copy::text("Workspace", "工作区", "工作區")))
                .child(choice_list(
                    host.workspaces,
                    host.workspace,
                    |choice| choice.id.clone(),
                    |choice| (choice.name.clone(), choice.detail.clone()),
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
                            .text_size(px(theme::FONT_CAPTION))
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
        .text_size(px(theme::FONT_MICRO))
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
                .text_size(px(theme::FONT_DETAIL))
                .text_color(theme::foreground),
        )
        .child(
            text(move || value.get().1)
                .truncate()
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}
