//! The appearance settings theme pickers.
//!
//! Each appearance gets its own picker, because the two choices are
//! independent: a user can keep the product default for dark while running a
//! warm palette in light. A picker lists only the themes authored for its own
//! appearance, since adopting a palette built for the opposite one would paint
//! unreadable text.
//!
//! The control is a segmented strip of palette swatches rather than a dropdown:
//! the catalog is curated and small, and a swatch shows what the theme actually
//! looks like — surface, raised surface, and accent — without a preview pane.

use gpui::{AnyElement, App, ElementId, IntoElement, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    Selectable as _, Sizable as _,
    tab::{Tab, TabBar},
};
use vibex_desktop_model::ThemeSelection;
use vibex_ui::{GpuiThemeDefinition, GpuiThemeMode, resolve_selection, themes_for};

use crate::theme;

/// Swatch metrics. The strip sits inside the 28px settings control shell, so
/// each swatch is a small rounded chip with three color bands.
const SWATCH_WIDTH: f32 = 34.0;
const SWATCH_HEIGHT: f32 = 22.0;
const SWATCH_RADIUS: f32 = 5.0;
const SWATCH_BAND_WIDTH: f32 = 3.0;

/// The theme a picker for `mode` currently shows as selected.
pub fn selected_theme(
    selection: &ThemeSelection,
    mode: GpuiThemeMode,
) -> &'static GpuiThemeDefinition {
    resolve_selection(selection, mode)
}

/// One palette swatch: the theme's background, raised surface, and accent, in
/// the order they stack in the product.
fn swatch(theme_definition: &GpuiThemeDefinition) -> AnyElement {
    let token = |name: &str| theme::semantic_color_for(theme_definition, name);
    let background = token("background");
    let surface = token("card");
    let accent = token("sidebar-primary");
    let border = token("border");

    div()
        .w(px(SWATCH_WIDTH))
        .h(px(SWATCH_HEIGHT))
        .rounded(px(SWATCH_RADIUS))
        .overflow_hidden()
        .flex()
        .flex_row()
        .bg(background)
        .border_1()
        .border_color(border)
        .child(div().flex_1().h_full().bg(surface))
        .child(div().w(px(SWATCH_BAND_WIDTH)).h_full().bg(accent))
        .into_any_element()
}

/// A segmented picker over every theme authored for `mode`.
///
/// `on_select` receives the chosen theme id. The strip keeps the shared 28px
/// settings control shell metric so it lines up with the adjacent font and
/// window-scale rows.
///
/// Each swatch paints its own candidate's colors, so the control previews
/// every palette at once rather than only the active one.
pub fn theme_picker(
    id: &'static str,
    mode: GpuiThemeMode,
    selection: &ThemeSelection,
    on_select: impl Fn(&'static str, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let selected = selected_theme(selection, mode);
    let options: Vec<&'static GpuiThemeDefinition> = themes_for(mode).collect();
    let selected_index = options
        .iter()
        .position(|candidate| candidate.id == selected.id);

    let tabs: Vec<Tab> = options
        .iter()
        .map(|definition| {
            let definition = *definition;
            let on_select = on_select.clone();
            Tab::new()
                .child(swatch(definition))
                .selected(definition.id == selected.id)
                .on_click(move |_, window, cx| {
                    on_select(definition.id, window, cx);
                })
        })
        .collect();

    div()
        .id(ElementId::Name(SharedString::from(id)))
        .h(px(28.0))
        .flex()
        .items_center()
        .child(
            TabBar::new(id)
                .segmented()
                .small()
                .h(px(28.0))
                .when_some(selected_index, |bar, index| bar.selected_index(index))
                .children(tabs),
        )
        .into_any_element()
}

/// The description under a picker row: the selected theme's name, so the row
/// states the current choice in words as well as in color.
pub fn selected_theme_description(
    selection: &ThemeSelection,
    mode: GpuiThemeMode,
    fallback: &'static str,
) -> SharedString {
    let selected = selected_theme(selection, mode);
    let explicit = match mode {
        GpuiThemeMode::Light => selection.light(),
        GpuiThemeMode::Dark => selection.dark(),
    };
    if explicit.is_none() {
        return SharedString::from(format!("{} — {fallback}", selected.name));
    }
    SharedString::from(selected.name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_selection_reports_the_catalog_default() {
        let selection = ThemeSelection::default();
        assert_eq!(
            selected_theme(&selection, GpuiThemeMode::Light).id,
            "vibex-light"
        );
        assert_eq!(
            selected_theme(&selection, GpuiThemeMode::Dark).id,
            "vibex-dark"
        );
    }

    #[test]
    fn a_picker_never_offers_the_other_appearance() {
        for mode in GpuiThemeMode::ALL {
            for definition in themes_for(mode) {
                assert_eq!(
                    definition.mode, mode,
                    "{} must not appear in the {mode:?} picker",
                    definition.id
                );
            }
        }
    }

    #[test]
    fn an_explicit_choice_is_named_without_the_default_suffix() {
        let mut selection = ThemeSelection::default();
        selection.select_dark("nord");
        assert_eq!(
            selected_theme_description(&selection, GpuiThemeMode::Dark, "default"),
            "Nord"
        );
        // The untouched light slot still says it is the product default.
        assert_eq!(
            selected_theme_description(&selection, GpuiThemeMode::Light, "default"),
            "Vibex Light — default"
        );
    }
}
