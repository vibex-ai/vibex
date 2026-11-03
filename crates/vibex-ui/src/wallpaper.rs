//! Letting a user wallpaper show through the theme's surfaces.
//!
//! The desktop paints a user image behind the workbench and then paints the
//! theme over it, so an opaque palette would hide the image completely. While a
//! wallpaper is behind them, the roles that are *containers* take an alpha, in
//! two tiers: the page and the chrome that frames it, then the panels and plates
//! that sit on the page.
//!
//! Three rules keep the result predictable:
//!
//! - Only containers fade. Ink, lines, status colors, borders, selection plates,
//!   and the hairline an input is outlined with keep the authored color, so text
//!   over a wallpaper is exactly as legible as text over the page, a selected
//!   row never turns into a hole, and a field keeps its edge.
//! - The fade *sets* an alpha rather than multiplying one, so the pass is
//!   idempotent: a client that maps a role and a framework bridge that maps the
//!   same role land on the same color.
//! - The tiers are tuned per appearance rather than shared. A dark appearance
//!   veils with a near-black page color, so the image keeps its detail through
//!   it; the light appearance's veil is white, which at the same alpha reads as
//!   milk over a bright picture and leaves the wallpaper looking like a faded
//!   print. Light therefore fades less at every tier, and the reader's own
//!   dimming control is what buys contrast when a picture needs it. A light
//!   tier must stay below the dark one at the same level — that is the
//!   regression this tuning exists to prevent.
//!
//! A light appearance also fades the plates that a dark one keeps authored —
//! cards, the composer, the right rail. There the page, the cards, and the
//! plates are all the same white, so a plate that kept its color would sit on
//! the wallpaper as a flat rectangle with no depth at all; a dark appearance
//! separates them by tone already, and keeping its plates solid is what its
//! tuning was built around.
//!
//! The legacy component tokens are left alone here, but the framework realigns
//! them to their colors on any edit that changes a color, so the fade reaches
//! them anyway. The client restores the one the framework paints a window's own
//! base plate from, after the appearance pass has closed: a translucent base
//! plate would let the desktop behind the window show through it. Tooltips,
//! hints, and notifications read their plate from a token as well, which is
//! what keeps them readable over any image.

use gpui::Hsla;
use gpui_component::Theme;

/// The alphas a wallpaper's surfaces take, per appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallpaperSurfaces {
    /// The page and the chrome that frames it: the window background, the
    /// sidebar, the title bar, the status bar, an accordion's body.
    pub base: f32,
    /// The panels and plates that sit on the page: group boxes, popovers, lists,
    /// tables, tab bars, and the secondary/muted/scrollbar plates.
    ///
    /// The page's alpha plus a step, because a plate paints on top of the page
    /// and the two compound: what reads as glass at one layer reads as a wash at
    /// two.
    pub raised: f32,
}

impl WallpaperSurfaces {
    /// The tiers a dark appearance paints with.
    pub const DARK: Self = Self {
        base: 0.55,
        raised: 0.72,
    };

    /// The tiers a light appearance paints with.
    ///
    /// Lower than the dark appearance at both tiers, not higher. The veil is
    /// mixed from the page color, and a white veil at the dark tuning reads as
    /// a milky sheet over a bright picture: the image keeps its shape but loses
    /// the color and the contrast that made it worth setting. What the light
    /// page needs is not more coverage but the reader's veil, which is the
    /// control that exists for a picture that turns out too bright.
    ///
    /// The plates still sit a step above the page. They are the same white as
    /// it, so they need that one step of separation to read as a surface rather
    /// than as a hole — but one step, not a lid.
    pub const LIGHT: Self = Self {
        base: 0.36,
        raised: 0.56,
    };

    pub fn for_appearance(is_dark: bool) -> Self {
        if is_dark { Self::DARK } else { Self::LIGHT }
    }
}

/// Which tier a container role belongs to, if it is a container at all.
///
/// The role is named the way each layer names it: the shared catalog uses
/// kebab-case (`table-head`), the framework's color fields use snake_case
/// (`table_head`), and both spellings have to resolve to one policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Base,
    Raised,
    /// A plate a light appearance fades and a dark one leaves authored.
    RaisedLight,
}

fn tier(role: &str) -> Option<Tier> {
    match role {
        // The page itself and the chrome that frames it.
        "background" | "right-rail-surface" | "right_rail_surface" | "accordion" | "sidebar"
        | "status-bar" | "status_bar" | "title-bar" | "title_bar" => Some(Tier::Base),
        // Panels and plates that sit on the page.
        "group-box"
        | "group_box"
        | "popover"
        | "list"
        | "list-head"
        | "list_head"
        | "table"
        | "table-head"
        | "table_head"
        | "table-foot"
        | "table_foot"
        | "tab-bar"
        | "tab_bar"
        | "tab-bar-segmented"
        | "tab_bar_segmented"
        | "secondary"
        | "muted"
        | "scrollbar"
        | "scrollbar-thumb"
        | "scrollbar_thumb"
        | "scrollbar-thumb-hover"
        | "scrollbar_thumb_hover" => Some(Tier::Raised),
        "card" | "composer-surface" | "composer_surface" | "composer-queue" | "composer_queue" => {
            Some(Tier::RaisedLight)
        }
        _ => None,
    }
}

/// The alpha `role` takes while a wallpaper is behind the surfaces, or `None`
/// when the role is ink, a line, or a status color and keeps its own.
///
/// Read at paint time by a client whose surfaces are painted from the shared
/// catalog rather than from the framework's theme, so a card and a panel over
/// the same wallpaper fade by the same rule.
pub fn wallpaper_surface_alpha(role: &str, is_dark: bool) -> Option<f32> {
    let surfaces = WallpaperSurfaces::for_appearance(is_dark);
    match tier(role)? {
        Tier::Base => Some(surfaces.base),
        Tier::Raised => Some(surfaces.raised),
        Tier::RaisedLight if !is_dark => Some(surfaces.raised),
        Tier::RaisedLight => None,
    }
}

/// `color` with the alpha a wallpaper surface takes, or unchanged for a role
/// that is not a container.
pub fn wallpaper_surface(role: &str, color: Hsla, is_dark: bool) -> Hsla {
    match wallpaper_surface_alpha(role, is_dark) {
        Some(alpha) => color.alpha(alpha),
        None => color,
    }
}

/// Fades the framework theme's container roles so a wallpaper behind them shows
/// through.
///
/// Called by the appearance pass after the component palette is filled in, and
/// only while a wallpaper is actually painted. Applying it to a theme twice is
/// the same as applying it once.
pub fn let_wallpaper_through(theme: &mut Theme, is_dark: bool) {
    let surfaces = WallpaperSurfaces::for_appearance(is_dark);

    macro_rules! fade_to {
        ($alpha:expr; $($field:ident),* $(,)?) => {
            $(
                theme.colors.$field = theme.colors.$field.alpha($alpha);
            )*
        };
    }

    fade_to!(
        surfaces.base;
        background,
        accordion,
        sidebar,
        status_bar,
        title_bar,
    );

    fade_to!(
        surfaces.raised;
        group_box,
        popover,
        list,
        list_head,
        table,
        table_head,
        table_foot,
        tab_bar,
        tab_bar_segmented,
        secondary,
        muted,
        scrollbar,
        scrollbar_thumb,
        scrollbar_thumb_hover,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::white;

    /// A theme whose container roles are all the same opaque color, so each
    /// assertion reads the alpha the pass chose and nothing else.
    fn opaque_theme() -> Theme {
        let mut theme = Theme::default();
        for color in [
            &mut theme.colors.background,
            &mut theme.colors.accordion,
            &mut theme.colors.sidebar,
            &mut theme.colors.status_bar,
            &mut theme.colors.title_bar,
            &mut theme.colors.group_box,
            &mut theme.colors.popover,
            &mut theme.colors.list,
            &mut theme.colors.list_head,
            &mut theme.colors.table,
            &mut theme.colors.table_head,
            &mut theme.colors.table_foot,
            &mut theme.colors.tab_bar,
            &mut theme.colors.tab_bar_segmented,
            &mut theme.colors.secondary,
            &mut theme.colors.muted,
            &mut theme.colors.scrollbar,
            &mut theme.colors.scrollbar_thumb,
            &mut theme.colors.scrollbar_thumb_hover,
        ] {
            *color = white();
        }
        theme
    }

    #[test]
    fn containers_fade_and_ink_keeps_the_authored_color() {
        for (is_dark, surfaces) in [
            (true, WallpaperSurfaces::DARK),
            (false, WallpaperSurfaces::LIGHT),
        ] {
            let mut theme = opaque_theme();
            let foreground = theme.colors.foreground;
            let border = theme.colors.border;
            let primary = theme.colors.primary;
            let list_active = theme.colors.list_active;
            let input = theme.colors.input;
            let text = theme.colors.group_box_foreground;

            let_wallpaper_through(&mut theme, is_dark);

            assert_eq!(theme.colors.background.a, surfaces.base);
            assert_eq!(theme.colors.sidebar.a, surfaces.base);
            assert_eq!(theme.colors.title_bar.a, surfaces.base);
            assert_eq!(theme.colors.group_box.a, surfaces.raised);
            assert_eq!(theme.colors.popover.a, surfaces.raised);
            assert_eq!(theme.colors.scrollbar.a, surfaces.raised);
            assert_eq!(theme.colors.scrollbar_thumb.a, surfaces.raised);

            // Ink, lines, accents, a selected row's plate, and the hairline a
            // field is outlined with are not containers.
            assert_eq!(theme.colors.foreground, foreground);
            assert_eq!(theme.colors.border, border);
            assert_eq!(theme.colors.primary, primary);
            assert_eq!(theme.colors.list_active, list_active);
            assert_eq!(theme.colors.input, input, "a field keeps its edge");
            assert_eq!(theme.colors.group_box_foreground, text);
        }
    }

    /// A light appearance fades the plates that a dark one keeps authored: in
    /// light they are the same white as the page, so a solid one would sit on
    /// the wallpaper as a flat rectangle.
    #[test]
    fn a_light_appearance_fades_the_plates_a_dark_one_keeps() {
        for role in ["card", "composer-surface", "composer-queue"] {
            let color = white();
            assert_eq!(
                wallpaper_surface(role, color, true),
                color,
                "{role} stays authored in a dark appearance"
            );
            assert_eq!(
                wallpaper_surface(role, color, false).a,
                WallpaperSurfaces::LIGHT.raised,
                "{role} fades in a light appearance"
            );
        }
    }

    /// The light tuning has to stay under the dark one at every tier.
    ///
    /// The veil is mixed from the page color, so the same alpha is a soft
    /// neutral dim in a dark appearance and a milky white sheet in a light one:
    /// at the dark tuning a bright picture loses its color and its contrast and
    /// stops reading as a wallpaper. This is a rule rather than a value because
    /// the light alphas are the ones most tempting to raise, and raising them
    /// past the dark ones is what the regression looked like.
    #[test]
    fn a_light_appearance_fades_less_than_a_dark_one() {
        const {
            assert!(
                WallpaperSurfaces::LIGHT.base < WallpaperSurfaces::DARK.base,
                "the light page must stay clearer than the dark one"
            );
            assert!(
                WallpaperSurfaces::LIGHT.raised < WallpaperSurfaces::DARK.raised,
                "the light plates must stay clearer than the dark ones"
            );
        }
        // Still frosted rather than transparent: the page keeps enough of the
        // veil for ink to read, and a plate stays a step above the page it sits
        // on so the two never collapse into one surface.
        for surfaces in [WallpaperSurfaces::DARK, WallpaperSurfaces::LIGHT] {
            assert!(surfaces.base > 0.0, "the page is never fully clear");
            assert!(
                surfaces.raised > surfaces.base,
                "a plate sits above the page it is painted on"
            );
        }
    }

    #[test]
    fn only_containers_take_a_wallpaper_alpha() {
        for role in [
            "foreground",
            "muted-foreground",
            "border",
            "sidebar-border",
            "input",
            "ring",
            "primary",
            "destructive",
            "select",
            "list-active",
            "sidebar-accent",
            "chart-2",
        ] {
            assert_eq!(
                wallpaper_surface(role, white(), true),
                white(),
                "{role} is not a container"
            );
            assert_eq!(wallpaper_surface(role, white(), false), white());
        }
        for role in ["background", "sidebar", "table-head", "table_head"] {
            assert!(wallpaper_surface_alpha(role, true).is_some());
            assert!(wallpaper_surface_alpha(role, false).is_some());
        }
    }

    /// The legacy tokens are what the framework paints a window's base plate and
    /// every tooltip from. They stay authored: a translucent base plate would
    /// let whatever is behind the window show through it.
    #[test]
    fn the_legacy_component_tokens_stay_authored() {
        let mut theme = opaque_theme();
        let background = theme.tokens.background.color;
        let popover = theme.tokens.popover.color;

        let_wallpaper_through(&mut theme, true);

        assert_eq!(theme.tokens.background.color, background);
        assert_eq!(theme.tokens.popover.color, popover);
    }

    /// A client maps a role and the framework bridge maps the same role, so the
    /// pass can run twice over one theme. Setting the alpha rather than
    /// multiplying it is what makes that safe.
    #[test]
    fn fading_twice_lands_on_the_same_color() {
        for is_dark in [true, false] {
            let mut once = opaque_theme();
            let_wallpaper_through(&mut once, is_dark);
            let mut twice = once.clone();
            let_wallpaper_through(&mut twice, is_dark);

            assert_eq!(twice.colors.background, once.colors.background);
            assert_eq!(twice.colors.group_box, once.colors.group_box);
            assert_eq!(twice.colors.scrollbar_thumb, once.colors.scrollbar_thumb);
        }
    }
}
