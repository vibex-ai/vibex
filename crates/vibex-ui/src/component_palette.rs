//! Completes gpui-component's palette from the shared semantic catalog.
//!
//! gpui-component ships its own theme and only paints what an application
//! assigns to it. The appearance pass in each client maps the product's core
//! roles — surfaces, text, borders, and the hover washes — and everything it
//! leaves alone keeps the framework's stock neutral palette. That is why a
//! window could switch to a tinted palette while its switches, segmented tab
//! bars, outline buttons, scrollbars, skeletons, and text selections still
//! painted the framework's greys.
//!
//! [`apply_component_palette`] closes the gap. It runs after the client's core
//! mapping and fills in every remaining framework color from the same theme:
//! directly where the catalog owns a matching role, and derived from the
//! theme's own surface, foreground, or accent where it does not. The clients
//! then publish the result to the framework's base layer
//! (`Theme::sync_base`), which owns scrollbars, resize handles, and the text
//! view defaults.
//!
//! The split is explicit: [`CORE_TOKENS`] is what a client must have mapped
//! before calling in, [`COMPONENT_TOKENS`] is what this module owns, and the
//! two together cover every color token the framework defines — a framework
//! upgrade that adds a token fails `tests::the_bridge_covers_every_framework_token`
//! instead of silently painting a stock color.

use gpui::{Hsla, Rgba, rgb};
use gpui_component::Theme;

use crate::{GpuiThemeDefinition, semantic_token};

/// Color tokens the clients map themselves before calling
/// [`apply_component_palette`].
///
/// These carry the product's own tuning (per-platform washes, high-contrast
/// overrides), so the bridge reads them back out of the theme instead of
/// re-deriving them. Each client maps the roles it uses — the phone runs a
/// smaller set than the desktop shell — so this is the union, and every token
/// the bridge reads back has to be among them.
pub const CORE_TOKENS: &[&str] = &[
    "accent",
    "accent_foreground",
    "background",
    "border",
    "button_active",
    "button_hover",
    "foreground",
    "input",
    "list_hover",
    "muted",
    "muted_foreground",
    "overlay",
    "popover",
    "popover_foreground",
    "primary",
    "primary_active",
    "primary_foreground",
    "primary_hover",
    "ring",
    "secondary",
    "secondary_foreground",
    "secondary_hover",
    "sidebar",
    "sidebar_accent",
    "sidebar_accent_foreground",
    "sidebar_border",
    "sidebar_foreground",
    "sidebar_primary",
    "sidebar_primary_foreground",
    "table_hover",
    "title_bar",
    "title_bar_border",
];

/// Color tokens this bridge assigns from the active theme.
pub const COMPONENT_TOKENS: &[&str] = &[
    "accordion",
    "blue",
    "blue_light",
    "button",
    "button_danger",
    "button_danger_active",
    "button_danger_foreground",
    "button_danger_hover",
    "button_foreground",
    "button_info",
    "button_info_active",
    "button_info_foreground",
    "button_info_hover",
    "button_primary",
    "button_primary_active",
    "button_primary_foreground",
    "button_primary_hover",
    "button_secondary",
    "button_secondary_active",
    "button_secondary_foreground",
    "button_secondary_hover",
    "button_success",
    "button_success_active",
    "button_success_foreground",
    "button_success_hover",
    "button_warning",
    "button_warning_active",
    "button_warning_foreground",
    "button_warning_hover",
    "caret",
    "chart_1",
    "chart_2",
    "chart_3",
    "chart_4",
    "chart_5",
    "chart_bearish",
    "chart_bullish",
    "cyan",
    "cyan_light",
    "danger",
    "danger_active",
    "danger_foreground",
    "danger_hover",
    "description_list_label",
    "description_list_label_foreground",
    "drag_border",
    "drop_target",
    "green",
    "green_light",
    "group_box",
    "group_box_foreground",
    "info",
    "info_active",
    "info_foreground",
    "info_hover",
    "link",
    "link_active",
    "link_hover",
    "list",
    "list_active",
    "list_active_border",
    "list_even",
    "list_head",
    "magenta",
    "magenta_light",
    "progress_bar",
    "red",
    "red_light",
    "scrollbar",
    "scrollbar_thumb",
    "scrollbar_thumb_hover",
    "secondary_active",
    "selection",
    "skeleton",
    "slider_bar",
    "slider_thumb",
    "status_bar",
    "status_bar_border",
    "success",
    "success_active",
    "success_foreground",
    "success_hover",
    "switch",
    "switch_thumb",
    "tab",
    "tab_active",
    "tab_active_foreground",
    "tab_bar",
    "tab_bar_segmented",
    "tab_foreground",
    "table",
    "table_active",
    "table_active_border",
    "table_even",
    "table_foot",
    "table_foot_foreground",
    "table_head",
    "table_head_foreground",
    "table_row_border",
    "warning",
    "warning_active",
    "warning_foreground",
    "warning_hover",
    "window_border",
    "yellow",
    "yellow_light",
];

/// How far a status plate moves toward the theme foreground when hovered or
/// pressed — the same convention the clients use for `primary_hover`.
const STATUS_HOVER: f32 = 0.08;
const STATUS_ACTIVE: f32 = 0.16;

/// Alpha of the soft plate a filled semantic button paints over its surface.
///
/// The framework's own defaults shape Danger, Warning, Success, and Info as a
/// tinted plate carrying the semantic color as text, not as a solid block with
/// a contrasting ink. Keeping that shape means a themed dialog confirm button
/// reads the same way it always has, only in the palette's own red.
const SOFT_PLATE: f32 = 0.16;
const SOFT_PLATE_HOVER: f32 = 0.26;
const SOFT_PLATE_ACTIVE: f32 = 0.36;

/// Alpha of a text selection highlight. Translucent, so the selected glyphs
/// keep the contrast they have on the surface underneath.
const SELECTION_ALPHA: f32 = 0.30;

/// Alpha of the drag and drop accents.
const DROP_TARGET_ALPHA: f32 = 0.18;

/// How far a base swatch's `_light` variant moves toward the surface. The kit
/// paints those as soft chips, which means darker on a dark theme.
const LIGHT_SWATCH: f32 = 0.50;

/// Fill in every gpui-component color the product palette does not map.
///
/// Call this after the client has mapped its core roles (see [`CORE_TOKENS`]) —
/// including the high-contrast overrides, so the derived colors follow them —
/// and before publishing the theme to the framework's base layer.
pub fn apply_component_palette(theme: &mut Theme, definition: &GpuiThemeDefinition) {
    let role = |name: &str| semantic_color(definition, name);

    // Read back what the client mapped: its washes and high-contrast overrides
    // are the product's tuning, and the derived colors must follow them.
    let background = theme.background;
    let foreground = theme.foreground;
    let primary = theme.primary;
    let primary_foreground = theme.primary_foreground;
    let secondary = theme.secondary;
    let secondary_foreground = theme.secondary_foreground;
    let muted = theme.muted;
    let muted_foreground = theme.muted_foreground;
    let accent = theme.accent;
    let border = theme.border;
    let ring = theme.ring;
    let sidebar = theme.sidebar;
    let sidebar_border = theme.sidebar_border;
    let button_hover = theme.button_hover;
    let button_active = theme.button_active;
    let primary_hover = theme.primary_hover;
    let primary_active = theme.primary_active;

    // Roles the appearance pass never maps. `success` and `info` take the
    // catalog's green and blue accents; `warning` owns an authored ink.
    let card = role("card");
    let card_foreground = role("card-foreground");
    let destructive = role("destructive");
    let warning = role("warning");
    let warning_foreground = role("warning-foreground");
    let success = role("chart-2");
    let info = role("chart-category-1");
    let magenta = role("chart-category-8");
    let cyan = role("chart-category-2");

    // Status plates and their readable inks. Danger, Success, and Info have no
    // authored ink, so they take the pole that contrasts most with the plate —
    // white or black always clears 4.5:1 on any plate, which is the same rule
    // the user-theme compiler applies to unpinned foregrounds.
    let danger_foreground = ink_on(destructive);
    let success_foreground = ink_on(success);
    let info_foreground = ink_on(info);
    let danger_hover = mix(destructive, foreground, STATUS_HOVER);
    let danger_active = mix(destructive, foreground, STATUS_ACTIVE);
    let warning_hover = mix(warning, foreground, STATUS_HOVER);
    let warning_active = mix(warning, foreground, STATUS_ACTIVE);
    let success_hover = mix(success, foreground, STATUS_HOVER);
    let success_active = mix(success, foreground, STATUS_ACTIVE);
    let info_hover = mix(info, foreground, STATUS_HOVER);
    let info_active = mix(info, foreground, STATUS_ACTIVE);

    // The soft plates a filled semantic button paints.
    let danger_plate = destructive.alpha(SOFT_PLATE);
    let warning_plate = warning.alpha(SOFT_PLATE);
    let success_plate = success.alpha(SOFT_PLATE);
    let info_plate = info.alpha(SOFT_PLATE);

    // `Theme::colors` rather than the `Deref` shorthand: the framework's own
    // `Theme` fields shadow a few color names (`list` is a `ListSettings`).
    macro_rules! assign {
        ($($field:ident = $value:expr;)*) => {
            $(
                let value = $value;
                theme.colors.$field = value;
                theme.tokens.$field = value.into();
            )*
        };
    }

    assign! {
        // Containers and secondary chrome.
        accordion = background;
        group_box = card;
        group_box_foreground = card_foreground;
        status_bar = sidebar;
        status_bar_border = sidebar_border;
        window_border = border;
        description_list_label = muted;
        description_list_label_foreground = muted_foreground;
        skeleton = muted;
        progress_bar = primary;
        slider_bar = primary;
        slider_thumb = primary_foreground;
        scrollbar = background;
        scrollbar_thumb = border;
        scrollbar_thumb_hover = muted_foreground;
        caret = primary;
        selection = primary.alpha(SELECTION_ALPHA);
        drag_border = ring;
        drop_target = ring.alpha(DROP_TARGET_ALPHA);

        // Lists and tables.
        list = card;
        list_active = accent;
        list_active_border = ring;
        list_even = muted;
        list_head = muted;
        table = card;
        table_active = accent;
        table_active_border = ring;
        table_even = muted;
        table_foot = muted;
        table_foot_foreground = muted_foreground;
        table_head = muted;
        table_head_foreground = muted_foreground;
        table_row_border = border;

        // Tabs. The segmented track is the muted plate and the selected pill is
        // the page surface, which is what keeps the two distinguishable when a
        // palette authors `secondary` and `muted` identically.
        tab = background;
        tab_bar = background;
        tab_bar_segmented = secondary;
        tab_active = background;
        tab_active_foreground = foreground;
        tab_foreground = muted_foreground;

        // Switch. The framework fades the track and keeps the thumb on the
        // surface color, so the thumb stays readable against `primary` when
        // checked and against `muted` when not.
        switch = muted;
        switch_thumb = background;

        // Buttons. Default and Secondary are the product's neutral plates;
        // Primary is the solid accent; the semantic variants are tinted plates
        // carrying the semantic color as text.
        button = secondary;
        button_foreground = secondary_foreground;
        button_secondary = secondary;
        button_secondary_foreground = secondary_foreground;
        button_secondary_hover = button_hover;
        button_secondary_active = button_active;
        secondary_active = button_active;
        button_primary = primary;
        button_primary_foreground = primary_foreground;
        button_primary_hover = primary_hover;
        button_primary_active = primary_active;
        danger = destructive;
        danger_foreground = danger_foreground;
        danger_hover = danger_hover;
        danger_active = danger_active;
        button_danger = danger_plate;
        button_danger_foreground = destructive;
        button_danger_hover = destructive.alpha(SOFT_PLATE_HOVER);
        button_danger_active = destructive.alpha(SOFT_PLATE_ACTIVE);
        warning = warning;
        warning_foreground = warning_foreground;
        warning_hover = warning_hover;
        warning_active = warning_active;
        button_warning = warning_plate;
        button_warning_foreground = warning;
        button_warning_hover = warning.alpha(SOFT_PLATE_HOVER);
        button_warning_active = warning.alpha(SOFT_PLATE_ACTIVE);
        success = success;
        success_foreground = success_foreground;
        success_hover = success_hover;
        success_active = success_active;
        button_success = success_plate;
        button_success_foreground = success;
        button_success_hover = success.alpha(SOFT_PLATE_HOVER);
        button_success_active = success.alpha(SOFT_PLATE_ACTIVE);
        info = info;
        info_foreground = info_foreground;
        info_hover = info_hover;
        info_active = info_active;
        button_info = info_plate;
        button_info_foreground = info;
        button_info_hover = info.alpha(SOFT_PLATE_HOVER);
        button_info_active = info.alpha(SOFT_PLATE_ACTIVE);

        // Links take the palette's blue accent, so they stay distinguishable
        // from body text in every theme.
        link = info;
        link_hover = mix(info, foreground, 0.15);
        link_active = mix(info, foreground, 0.30);

        // Charts.
        chart_1 = role("chart-1");
        chart_2 = role("chart-2");
        chart_3 = role("chart-3");
        chart_4 = role("chart-4");
        chart_5 = role("chart-5");
        chart_bullish = success;
        chart_bearish = destructive;

        // Base swatches the kit paints outside charts (badges, labels, the
        // color picker) plus their soft variants.
        red = destructive;
        red_light = mix(destructive, background, LIGHT_SWATCH);
        green = success;
        green_light = mix(success, background, LIGHT_SWATCH);
        blue = info;
        blue_light = mix(info, background, LIGHT_SWATCH);
        yellow = warning;
        yellow_light = mix(warning, background, LIGHT_SWATCH);
        magenta = magenta;
        magenta_light = mix(magenta, background, LIGHT_SWATCH);
        cyan = cyan;
        cyan_light = mix(cyan, background, LIGHT_SWATCH);
    }
}

/// Read one semantic role out of a theme variant.
///
/// A missing role is a build-time contract between the token source and this
/// module, so it panics rather than painting a wrong color.
fn semantic_color(definition: &GpuiThemeDefinition, name: &str) -> Hsla {
    let token = semantic_token(definition, name)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    Hsla {
        a: token.alpha,
        ..rgb(token.rgb).into()
    }
}

/// The pole — white or black — that contrasts most with `plate`.
///
/// Mirrors the user-theme compiler's `best_on`, so a role this bridge derives
/// and a role a theme file derives land on the same ink.
fn ink_on(plate: Hsla) -> Hsla {
    if contrast(gpui::white(), plate) >= contrast(gpui::black(), plate) {
        gpui::white()
    } else {
        gpui::black()
    }
}

/// WCAG contrast ratio between two opaque colors.
fn contrast(first: Hsla, second: Hsla) -> f32 {
    let (first, second) = (luminance(first), luminance(second));
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

/// WCAG relative luminance of a color's opaque part.
fn luminance(color: Hsla) -> f32 {
    let color = Rgba::from(Hsla { a: 1.0, ..color });
    let channel = |value: f32| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// Blend `from` toward `to` by `amount`, premultiplied so a translucent
/// endpoint keeps its own weight.
fn mix(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return from;
    }
    if amount >= 1.0 {
        return to;
    }
    let (from, to) = (Rgba::from(from), Rgba::from(to));
    let alpha = from.a + (to.a - from.a) * amount;
    if alpha <= f32::EPSILON {
        return Hsla::from(Rgba { a: 0.0, ..to });
    }
    let lerp = |first: f32, second: f32| first + (second - first) * amount;
    Hsla::from(Rgba {
        r: lerp(from.r * from.a, to.r * to.a) / alpha,
        g: lerp(from.g * from.a, to.g * to.a) / alpha,
        b: lerp(from.b * from.a, to.b * to.a) / alpha,
        a: alpha,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui_component::ThemeColor;

    use super::*;
    use crate::{GpuiThemeMode, THEMES};

    /// Map the tokens a client owns, so the bridge is exercised on the same
    /// input it sees in the applications.
    fn core_themed(definition: &GpuiThemeDefinition) -> Theme {
        let dark = definition.mode == GpuiThemeMode::Dark;
        let stock = if dark {
            ThemeColor::dark()
        } else {
            ThemeColor::light()
        };
        let mut theme = Theme::from(stock.as_ref());
        let role = |name: &str| semantic_color(definition, name);
        let (background, foreground) = (role("background"), role("foreground"));
        let wash =
            |pole: f32, alpha: f32| gpui::hsla(background.h, background.s * 0.35, pole, alpha);
        let (hover, active) = if dark {
            (wash(0.92, 0.11), wash(0.92, 0.16))
        } else {
            (wash(0.10, 0.11), wash(0.10, 0.16))
        };
        macro_rules! assign {
            ($($field:ident = $value:expr;)*) => {
                $(
                    let value = $value;
                    theme.colors.$field = value;
                    theme.tokens.$field = value.into();
                )*
            };
        }
        assign! {
            background = background;
            foreground = foreground;
            primary = role("primary");
            primary_foreground = role("primary-foreground");
            secondary = role("secondary");
            secondary_foreground = role("secondary-foreground");
            muted = role("muted");
            muted_foreground = role("muted-foreground");
            accent = role("accent");
            border = role("border");
            ring = role("ring");
            sidebar = role("sidebar");
            sidebar_border = role("sidebar-border");
            list_hover = hover;
            table_hover = hover;
            button_hover = mix(role("secondary"), hover, 0.5);
            button_active = mix(role("secondary"), active, 0.5);
            primary_hover = mix(role("primary"), foreground, 0.08);
            primary_active = mix(role("primary"), foreground, 0.16);
        }
        theme
    }

    fn token_values(theme: &Theme) -> serde_json::Map<String, serde_json::Value> {
        serde_json::to_value(theme.tokens)
            .expect("theme tokens serialize")
            .as_object()
            .expect("theme tokens serialize to an object")
            .clone()
    }

    #[test]
    fn the_bridge_covers_every_framework_token() {
        let framework: BTreeSet<String> = token_values(&Theme::default()).keys().cloned().collect();
        let covered: BTreeSet<&str> = CORE_TOKENS
            .iter()
            .chain(COMPONENT_TOKENS.iter())
            .copied()
            .collect();
        assert_eq!(
            covered.len(),
            CORE_TOKENS.len() + COMPONENT_TOKENS.len(),
            "a token is claimed by both the clients and the bridge"
        );
        let uncovered: Vec<&String> = framework
            .iter()
            .filter(|name| !covered.contains(name.as_str()))
            .collect();
        assert!(
            uncovered.is_empty(),
            "gpui-component added color tokens the bridge does not map: {uncovered:?}"
        );
        let unknown: Vec<&&str> = covered
            .iter()
            .filter(|name| !framework.contains(**name))
            .collect();
        assert!(
            unknown.is_empty(),
            "the bridge maps tokens gpui-component no longer defines: {unknown:?}"
        );
    }

    #[test]
    fn component_tokens_follow_the_active_theme() {
        for definition in THEMES {
            let mut theme = core_themed(definition);
            apply_component_palette(&mut theme, definition);
            let role = |name: &str| semantic_color(definition, name);
            let expected = [
                ("switch", role("muted")),
                ("switch_thumb", role("background")),
                ("tab_bar_segmented", role("secondary")),
                ("tab_active", role("background")),
                ("tab_foreground", role("muted-foreground")),
                ("button", role("secondary")),
                ("button_primary", role("primary")),
                ("danger", role("destructive")),
                ("button_danger_foreground", role("destructive")),
                ("success", role("chart-2")),
                ("info", role("chart-category-1")),
                ("link", role("chart-category-1")),
                ("list", role("card")),
                ("list_active", role("accent")),
                ("table_head", role("muted")),
                ("table_row_border", role("border")),
                ("scrollbar_thumb", role("border")),
                ("caret", role("primary")),
                ("skeleton", role("muted")),
                ("progress_bar", role("primary")),
                ("slider_thumb", role("primary-foreground")),
                ("group_box", role("card")),
                ("status_bar", role("sidebar")),
                ("chart_bearish", role("destructive")),
                ("red", role("destructive")),
                ("blue", role("chart-category-1")),
                ("magenta", role("chart-category-8")),
            ];
            for (name, value) in expected {
                let actual = token_values(&theme)[name]["color"].clone();
                assert_eq!(
                    actual,
                    serde_json::to_value(value).expect("color serializes"),
                    "{}: {name} does not follow the theme",
                    definition.id
                );
            }
            assert_eq!(
                theme.selection.a, SELECTION_ALPHA,
                "{}: selection must stay translucent",
                definition.id
            );
        }
    }

    #[test]
    fn semantic_ink_stays_readable_on_its_plate() {
        for definition in THEMES {
            let mut theme = core_themed(definition);
            apply_component_palette(&mut theme, definition);
            for (plate, ink, minimum) in [
                (theme.danger, theme.danger_foreground, 4.5),
                (theme.warning, theme.warning_foreground, 4.5),
                (theme.success, theme.success_foreground, 4.5),
                (theme.info, theme.info_foreground, 4.5),
            ] {
                let actual = contrast(ink, plate);
                assert!(
                    actual >= minimum,
                    "{}: ink on plate is {actual:.2}:1, expected {minimum}:1",
                    definition.id
                );
            }
        }
    }
}
