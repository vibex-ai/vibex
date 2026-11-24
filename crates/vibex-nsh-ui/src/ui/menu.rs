//! The floating menu a long press raises.
//!
//! A row's commands are not on the row. Holding it lifts the row out of the
//! page, blurs what lies around it and stands a card against the row's near
//! edge: one command a line, the word on the leading side and its glyph on the
//! trailing one. A press anywhere else puts it away.
//!
//! The menu is a snapshot: its entries are decided when it is raised, since
//! nothing under a modal layer changes while it is up.

use std::rc::Rc;

use nsh_base::button;
use nsh_core::{AccessibilityRole, element::ImageSrc, prelude::*};

use crate::{copy, motion, theme};

/// How far from the press a menu with no row stands.
const MENU_OFFSET: f32 = 26.0;
/// The card's distance from the screen's edges.
const MENU_MARGIN: f32 = 16.0;
/// The inset a card keeps when it answers a row of the page.
const MENU_ATTACH_INSET: f32 = 12.0;
/// The gap between the card and the row it answers.
const MENU_ATTACH_GAP: f32 = 8.0;
/// How much a lifted row grows past its place in the list.
const LIFTED_ROW_GROWTH: f32 = 2.0;
/// How far the page behind a menu is blurred.
const MENU_BLUR: f32 = 14.0;
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

/// The row a long press lifted: where it stands, and what it says.
#[derive(Clone)]
pub struct MenuRow {
    /// How far down the page the row's top edge is.
    pub top: f32,
    /// The row's height.
    pub height: f32,
    /// The mark the row leads with.
    pub mark: Option<ImageSrc>,
    /// Its title.
    pub title: SharedString,
    /// What trails the title: when it was last touched, how many it holds.
    pub trailing: SharedString,
}

/// A raised menu: where it was asked for and what it offers.
#[derive(Clone, Default)]
pub struct Menu {
    /// How far down the page the press landed.
    pub anchor: f32,
    pub side: MenuSide,
    /// The row that raised the menu, when a long press did.
    pub row: Option<MenuRow>,
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

    /// Lifts `row` out of the page and stands the card against its near edge.
    pub fn from_row(mut self, row: MenuRow) -> Self {
        self.row = Some(row);
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
        row,
        entries,
        dismissed: _,
    } = current;
    // The rows and the card's own edge.
    let content = entries.len() as f32 * theme::MENU_ROW_HEIGHT + 2.0;
    // The room between the status bar and the home indicator.
    let bounds = || {
        let floor = safe_area().top.0 + MENU_MARGIN;
        let ceiling = window().height.0 - safe_area().bottom.0 - MENU_MARGIN;
        (floor, ceiling.max(floor + theme::MENU_ROW_HEIGHT))
    };
    let attached = row.is_some();
    let row_for_top = row.clone();
    let height = move || {
        let (floor, ceiling) = bounds();
        content.min(ceiling - floor)
    };
    // A lifted row puts the card against its near edge: under the row while
    // the room below it holds the card, over it when it does not. A menu with
    // no row follows the press instead.
    let top = move || {
        let (floor, ceiling) = bounds();
        let height = height();
        if let Some(row) = row_for_top.as_ref() {
            let card_top = row.top - LIFTED_ROW_GROWTH;
            let card_bottom = row.top + row.height + LIFTED_ROW_GROWTH;
            let below = card_bottom + MENU_ATTACH_GAP;
            if below + height <= ceiling {
                return px(below);
            }
            let above = card_top - MENU_ATTACH_GAP - height;
            return px(above.min((ceiling - height).max(floor)).max(floor));
        }
        let below = anchor + MENU_OFFSET;
        let top = if below + height <= ceiling {
            below
        } else {
            anchor - MENU_OFFSET - height
        };
        px(top.clamp(floor, (ceiling - height).max(floor)))
    };
    let width = move || {
        if attached {
            return (window().width.0 - MENU_ATTACH_INSET * 2.0).max(1.0);
        }
        (window().width.0 - MENU_MARGIN * 2.0).clamp(1.0, theme::MENU_WIDTH)
    };
    let left = move || {
        if attached {
            return MENU_ATTACH_INSET;
        }
        match side {
            MenuSide::Leading => MENU_MARGIN + theme::SPACING_SM,
            MenuSide::Center => (window().width.0 - width()) / 2.0,
            MenuSide::Trailing => window().width.0 - width() - MENU_MARGIN,
        }
        .max(MENU_MARGIN)
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
            .when(index > 0, |row| {
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
    // The row itself, lifted over the scrim: the page behind it is blurred,
    // and this is what the reader pressed, a shade larger than its place.
    let lifted_card = row
        .as_ref()
        .map(|row| lifted_row(row, row.top - LIFTED_ROW_GROWTH).bind_opacity(&reveal));
    view()
        .absolute()
        .inset(px(0.0))
        .child(
            view()
                .absolute()
                .inset(px(0.0))
                .bg(theme::menu_scrim)
                .backdrop_blur(px(MENU_BLUR))
                .bind_opacity(&reveal)
                .role(AccessibilityRole::Button)
                .label(copy::text("Dismiss menu", "关闭菜单", "關閉選單"))
                .focusable(true)
                .on_press(move || {
                    dismiss(menu);
                }),
        )
        .children(lifted_card)
        .child(
            view()
                .absolute()
                .top(top)
                .left(move || px(left()))
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
                .child(
                    scroll()
                        .flex_shrink(1.0)
                        .min_h(px(0.0))
                        .child(view().w_full().flex_col().children(rows)),
                ),
        )
}

/// The card a lifted row is drawn on: its mark, title and trailing detail,
/// a shade larger than its place in the list, standing at `top`.
///
/// The menu draws the row it was raised from with it; a drag keeps drawing
/// it under the finger as the row travels.
pub fn lifted_row(row: &MenuRow, top: f32) -> View {
    let mut card = view()
        .absolute()
        .top(px(top))
        .left(px(MENU_ATTACH_INSET))
        .w(move || px((window().width.0 - MENU_ATTACH_INSET * 2.0).max(1.0)))
        .h(px(row.height + LIFTED_ROW_GROWTH * 2.0))
        .rounded(px(theme::RADIUS_GROUP))
        .bg(theme::block_surface)
        .shadow_lg()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .px(px(theme::SPACING_LG))
        // The card is the row's picture, not a control: a press on it
        // dismisses the menu like a press beside it.
        .pointer_events(PointerEvents::None);
    if let Some(mark) = row.mark.clone() {
        card = card.child(
            svg(mark)
                .size(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(theme::text_secondary),
        );
    }
    card.child(
        text(row.title.clone())
            .flex_1()
            .min_w(px(0.0))
            .truncate()
            .text_size(px(theme::font_body()))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::foreground),
    )
    .children((!row.trailing.is_empty()).then(|| {
        text(row.trailing.clone())
            .flex_shrink_0()
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted)
    }))
}
