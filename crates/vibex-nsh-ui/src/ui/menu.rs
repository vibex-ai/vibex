//! The floating menu a long press raises.
//!
//! A row's commands are not on the row. Holding it dims the page and raises a
//! card beside the finger: what was pressed at its head, then one command a
//! line, the word on the leading side and its glyph on the trailing one. A
//! press anywhere else puts it away.
//!
//! The menu is a snapshot: its entries are decided when it is raised, since
//! nothing under a modal layer changes while it is up.

use std::rc::Rc;

use nsh_base::button;
use nsh_core::{AccessibilityRole, element::ImageSrc, prelude::*};

use crate::{copy, motion, theme};

/// How far from the press the card stands.
const MENU_OFFSET: f32 = 26.0;
/// The card's distance from the screen's edges.
const MENU_MARGIN: f32 = 16.0;
/// The line that names what the menu is about.
const MENU_HEADER_HEIGHT: f32 = 54.0;
/// How long the scrim takes to come up.
const MENU_FADE_MS: f32 = 140.0;

/// The side of the page a menu's card keeps to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuSide {
    /// Under a row of a list.
    #[default]
    Leading,
    /// Under a title in the middle of a bar.
    Center,
    /// Under a control at the end of a bar.
    Trailing,
}

/// One command of a menu.
#[derive(Clone)]
pub struct MenuEntry {
    icon: ImageSrc,
    label: SharedString,
    danger: bool,
    action: Rc<dyn Fn()>,
}

impl MenuEntry {
    pub fn new(
        icon: ImageSrc,
        label: impl Into<SharedString>,
        action: impl Fn() + 'static,
    ) -> Self {
        Self {
            icon,
            label: label.into(),
            danger: false,
            action: Rc::new(action),
        }
    }

    /// Marks the command as one that removes or discards.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

/// A raised menu: where it was asked for, what it is about and what it offers.
#[derive(Clone, Default)]
pub struct Menu {
    /// How far down the page the press landed.
    pub anchor: f32,
    pub side: MenuSide,
    /// The mark of what was pressed.
    pub mark: Option<ImageSrc>,
    /// Its name; empty for a menu that belongs to the page itself.
    pub title: SharedString,
    /// The line under the name: what kind of thing it is.
    pub caption: SharedString,
    pub entries: Vec<MenuEntry>,
    /// Hears the menu being put away without a command.
    dismissed: Option<Rc<dyn Fn()>>,
}

impl Menu {
    /// A menu under a press `anchor` points down the page.
    pub fn at(anchor: f32) -> Self {
        Self {
            anchor,
            ..Self::default()
        }
    }

    pub fn side(mut self, side: MenuSide) -> Self {
        self.side = side;
        self
    }

    /// Names what the menu is about.
    pub fn about(
        mut self,
        mark: Option<ImageSrc>,
        title: impl Into<SharedString>,
        caption: impl Into<SharedString>,
    ) -> Self {
        self.mark = mark;
        self.title = title.into();
        self.caption = caption.into();
        self
    }

    pub fn entry(mut self, entry: MenuEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Adds `entry` only where it applies.
    pub fn entry_if(self, applies: bool, entry: impl FnOnce() -> MenuEntry) -> Self {
        if applies { self.entry(entry()) } else { self }
    }

    /// Hears the menu being put away without a command.
    pub fn on_dismiss(mut self, dismissed: impl Fn() + 'static) -> Self {
        self.dismissed = Some(Rc::new(dismissed));
        self
    }
}

/// Puts the raised menu away, as a press beside it does. Whether there was
/// one, for the back key.
pub fn dismiss(menu: Signal<Option<Menu>>) -> bool {
    let Some(current) = menu.get_untracked() else {
        return false;
    };
    menu.set(None);
    if let Some(dismissed) = current.dismissed {
        dismissed();
    }
    true
}

/// Two menus are never the same menu: each is raised by its own press.
impl PartialEq for Menu {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

/// The layer menus float on. It lies over the page that owns `menu` and takes
/// no room in it while no menu is up.
pub fn menu_layer(menu: Signal<Option<Menu>>) -> impl IntoElement {
    dynamic(move || match menu.get() {
        Some(current) => layer(menu, current).into_element(),
        None => view().absolute().into_element(),
    })
}

fn layer(menu: Signal<Option<Menu>>, current: Menu) -> impl IntoElement {
    let Menu {
        anchor,
        side,
        mark,
        title,
        caption,
        entries,
        dismissed: _,
    } = current;
    let headed = !title.is_empty();
    // The rows, the line that names them, and the card's own edge.
    let content = entries.len() as f32 * theme::MENU_ROW_HEIGHT
        + if headed { MENU_HEADER_HEIGHT } else { 0.0 }
        + 2.0;
    // The room between the status bar and the home indicator.
    let bounds = || {
        let floor = safe_area().top.0 + MENU_MARGIN;
        let ceiling = window().height.0 - safe_area().bottom.0 - MENU_MARGIN;
        (floor, ceiling.max(floor + theme::MENU_ROW_HEIGHT))
    };
    let height = move || {
        let (floor, ceiling) = bounds();
        content.min(ceiling - floor)
    };
    // Under the press where there is room, over it where there is not.
    let top = move || {
        let (floor, ceiling) = bounds();
        let height = height();
        let below = anchor + MENU_OFFSET;
        let top = if below + height <= ceiling {
            below
        } else {
            anchor - MENU_OFFSET - height
        };
        px(top.clamp(floor, (ceiling - height).max(floor)))
    };
    let width = || (window().width.0 - MENU_MARGIN * 2.0).clamp(1.0, theme::MENU_WIDTH);
    let left = move || {
        let room = window().width.0;
        px(match side {
            MenuSide::Leading => MENU_MARGIN + theme::SPACING_SM,
            MenuSide::Center => (room - width()) / 2.0,
            MenuSide::Trailing => room - width() - MENU_MARGIN,
        }
        .max(MENU_MARGIN))
    };
    let reveal = animated_value(if motion::reduce_motion() { 1.0 } else { 0.0 });
    if !motion::reduce_motion() {
        let _ = reveal.timing(1.0, MENU_FADE_MS, Easing::EaseOut);
    }
    let rise = reveal.interpolate(&[0.0, 1.0], &[8.0, 0.0]);
    let rows = entries.into_iter().enumerate().map(move |(index, entry)| {
        let MenuEntry {
            icon,
            label,
            danger,
            action,
        } = entry;
        let ink: fn() -> Hsla = if danger {
            theme::accent_red
        } else {
            theme::foreground
        };
        let glyph: fn() -> Hsla = if danger {
            theme::accent_red
        } else {
            theme::text_secondary
        };
        button()
            .w_full()
            .h(px(theme::MENU_ROW_HEIGHT))
            .flex_shrink_0()
            .px(px(theme::SPACING_LG))
            .flex_row()
            .items_center()
            .gap(px(theme::SPACING_MD))
            .when(index > 0 || headed, |row| {
                row.border_t_1().border_t_color(theme::hairline)
            })
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_press(move || {
                menu.set(None);
                action();
            })
            .child(
                text(label)
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .text_color(ink),
            )
            .child(
                svg(icon)
                    .size(px(theme::ICON_MD))
                    .flex_shrink_0()
                    .text_color(glyph),
            )
    });
    view()
        .absolute()
        .inset(px(0.0))
        .child(
            view()
                .absolute()
                .inset(px(0.0))
                .bg(theme::menu_scrim)
                .bind_opacity(&reveal)
                .role(AccessibilityRole::Button)
                .label(copy::text("Dismiss menu", "关闭菜单", "關閉選單"))
                .focusable(true)
                .on_press(move || {
                    dismiss(menu);
                }),
        )
        .child(
            view()
                .absolute()
                .top(top)
                .left(left)
                .w(move || px(width()))
                .max_h(move || px(height()))
                .flex_col()
                .rounded(px(theme::RADIUS_GROUP))
                .bg(theme::block_surface)
                .border_1()
                .border_color(theme::hairline)
                .shadow_xl()
                .overflow_hidden()
                .bind_translate_y(&rise)
                // The card takes its own touches: a press between two rows
                // must not fall through to the scrim.
                .on_press(|| {})
                .children(headed.then(|| {
                    view()
                        .w_full()
                        .h(px(MENU_HEADER_HEIGHT))
                        .flex_shrink_0()
                        .px(px(theme::SPACING_LG))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_MD))
                        .children(mark.map(|mark| {
                            svg(mark)
                                .size(px(theme::ICON_MD))
                                .flex_shrink_0()
                                .text_color(theme::text_secondary)
                        }))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_col()
                                .child(
                                    text(title)
                                        .truncate()
                                        .text_size(px(theme::font_detail()))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::foreground),
                                )
                                .children((!caption.is_empty()).then(|| {
                                    text(caption)
                                        .truncate()
                                        .text_size(px(theme::font_hint()))
                                        .text_color(theme::text_muted)
                                })),
                        )
                }))
                .child(
                    scroll()
                        .flex_shrink(1.0)
                        .min_h(px(0.0))
                        .child(view().w_full().flex_col().children(rows)),
                ),
        )
}
