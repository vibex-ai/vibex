//! The appearance settings theme controls.
//!
//! Each appearance gets its own picker, because the two choices are
//! independent: a user can keep the product default for dark while running a
//! warm palette in light. A picker lists only the themes authored for its own
//! appearance, since adopting a palette built for the opposite one would paint
//! unreadable text.
//!
//! An appearance's picker is a dropdown whose rows — and whose closed trigger —
//! carry the palette swatch, so the catalog stays scannable without a wall of
//! chips inside the settings panel.
//!
//! The appearance control is a row of preview cards rather than a segmented
//! icon strip: each card paints a miniature workbench in the palette its mode
//! resolves to, so the choice is legible before it is made instead of only
//! after the window has repainted. The cards sit outside the settings panel and
//! share its width three ways, which is what makes the previews readable.

use gpui::{
    AnyElement, App, ClickEvent, Hsla, IntoElement, Role, SharedString,
    StatefulInteractiveElement as _, Window, div, prelude::*, px, relative,
};
use gpui_component::{
    IndexPath, StyledExt as _, h_flex, searchable_list::SearchableListItem, v_flex,
};
use vibex_desktop_model::{ThemeMode, ThemeSelection};
use vibex_ui::{GpuiThemeDefinition, GpuiThemeMode, resolve_selection, themes_for};

use crate::theme;

/// Swatch metrics for a dropdown row and for the closed trigger, which show the
/// theme's background, raised surface, and accent in the order they stack.
const SWATCH_WIDTH: f32 = 24.0;
const SWATCH_HEIGHT: f32 = 16.0;
const SWATCH_RADIUS: f32 = 4.0;
const SWATCH_BAND_WIDTH: f32 = 3.0;

/// The theme `mode` currently resolves to.
fn selected_theme(selection: &ThemeSelection, mode: GpuiThemeMode) -> &'static GpuiThemeDefinition {
    resolve_selection(selection, mode)
}

/// One palette swatch: the theme's background, raised surface, and accent.
fn swatch(theme_definition: &GpuiThemeDefinition) -> AnyElement {
    let token = |name: &str| theme::semantic_color_for(theme_definition, name);
    let background = token("background");
    let surface = token("card");
    let accent = token("sidebar-primary");
    let border = token("border");

    div()
        .w(px(SWATCH_WIDTH))
        .h(px(SWATCH_HEIGHT))
        .flex_none()
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

/// One theme in an appearance dropdown: the palette swatch and its name.
///
/// `title()` stays the plain name because that is what assistive technology
/// reads as the committed value; the swatch is presentation only.
#[derive(Clone)]
pub struct ThemeOption {
    definition: &'static GpuiThemeDefinition,
    id: String,
}

impl ThemeOption {
    fn new(definition: &'static GpuiThemeDefinition) -> Self {
        Self {
            definition,
            id: definition.id.to_string(),
        }
    }
}

impl SearchableListItem for ThemeOption {
    type Value = String;

    fn title(&self) -> SharedString {
        SharedString::from(self.definition.name)
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(theme_option_row(self.definition))
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        theme_option_row(self.definition)
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

/// A dropdown row: the palette swatch, then the theme's name.
fn theme_option_row(definition: &'static GpuiThemeDefinition) -> AnyElement {
    h_flex()
        .gap_2()
        .child(swatch(definition))
        .child(div().child(definition.name))
        .into_any_element()
}

/// Every theme authored for `mode`, as dropdown options.
pub fn theme_options(mode: GpuiThemeMode) -> Vec<ThemeOption> {
    themes_for(mode).map(ThemeOption::new).collect()
}

/// The catalog position of the theme `mode` currently resolves to, so the
/// dropdown states the active palette — including the catalog default an unset
/// slot falls back to.
pub fn selected_theme_index(mode: GpuiThemeMode, selection: &ThemeSelection) -> Option<IndexPath> {
    let selected = selected_theme(selection, mode);
    themes_for(mode)
        .position(|definition| definition.id == selected.id)
        .map(|row| IndexPath::default().row(row))
}

/// Card metrics for the appearance control.
///
/// The three cards share the settings page's width, so the height follows from
/// [`mode_card_height`]. The face metrics are authored for a card of
/// [`MODE_CARD_REFERENCE_HEIGHT`] and scale with it, which keeps the miniature
/// workbench in proportion at every window size.
const MODE_CARD_ASPECT: f32 = 1.65;
const MODE_CARD_GAP: f32 = 12.0;
const MODE_CARD_REFERENCE_HEIGHT: f32 = 64.0;
const MODE_CARD_MIN_HEIGHT: f32 = 56.0;
const MODE_CARD_RADIUS: f32 = 8.0;
const MODE_CARD_PADDING: f32 = 3.0;
const MODE_LABEL_GAP: f32 = 8.0;

/// The height one appearance card takes when the row is `available_width`
/// wide. Cards flex to a third of the row, so the height follows from that
/// width to hold the authored aspect ratio.
pub fn mode_card_height(available_width: f32) -> f32 {
    let card_width = ((available_width - 2.0 * MODE_CARD_GAP) / 3.0).max(1.0);
    (card_width / MODE_CARD_ASPECT).max(MODE_CARD_MIN_HEIGHT)
}

/// Metrics for one miniature workbench face.
#[derive(Clone, Copy)]
struct FaceMetrics {
    padding: f32,
    gap: f32,
    sidebar_width: f32,
    bar_height: f32,
    bar_gap: f32,
    bar_radius: f32,
    panel_padding: f32,
    panel_radius: f32,
}

impl FaceMetrics {
    fn scaled(self, scale: f32) -> Self {
        Self {
            padding: self.padding * scale,
            gap: self.gap * scale,
            sidebar_width: self.sidebar_width * scale,
            bar_height: self.bar_height * scale,
            bar_gap: self.bar_gap * scale,
            bar_radius: self.bar_radius * scale,
            panel_padding: self.panel_padding * scale,
            panel_radius: self.panel_radius * scale,
        }
    }
}

/// A face that fills a whole mode card.
const FULL_FACE: FaceMetrics = FaceMetrics {
    padding: 5.0,
    gap: 5.0,
    sidebar_width: 16.0,
    bar_height: 3.0,
    bar_gap: 3.0,
    bar_radius: 1.5,
    panel_padding: 5.0,
    panel_radius: 3.0,
};

/// A face at half card width, for the two halves of the system card.
const SPLIT_FACE: FaceMetrics = FaceMetrics {
    padding: 3.0,
    gap: 3.0,
    sidebar_width: 9.0,
    bar_height: 2.0,
    bar_gap: 2.0,
    bar_radius: 1.0,
    panel_padding: 3.0,
    panel_radius: 2.0,
};

/// One rounded line inside a preview face.
fn preview_bar(width: f32, metrics: FaceMetrics, color: Hsla) -> AnyElement {
    div()
        .w(relative(width))
        .h(px(metrics.bar_height))
        .flex_none()
        .rounded(px(metrics.bar_radius))
        .bg(color)
        .into_any_element()
}

/// A miniature workbench in `definition`'s palette: the navigation column
/// beside one raised surface.
fn preview_face(definition: &'static GpuiThemeDefinition, metrics: FaceMetrics) -> AnyElement {
    let token = |name: &str| theme::semantic_color_for(definition, name);
    let background = token("background");
    let card = token("card");
    let border = token("border");
    let line = token("muted-foreground");
    let sidebar_line = line.opacity(0.40);
    let content_line = line.opacity(0.55);

    div()
        .size_full()
        .flex()
        .flex_row()
        .gap(px(metrics.gap))
        .p(px(metrics.padding))
        .bg(background)
        .overflow_hidden()
        .child(
            div()
                .w(px(metrics.sidebar_width))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(metrics.bar_gap))
                .child(preview_bar(1.0, metrics, sidebar_line))
                .child(preview_bar(0.72, metrics, sidebar_line))
                .child(preview_bar(0.88, metrics, sidebar_line)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .gap(px(metrics.bar_gap))
                .p(px(metrics.panel_padding))
                .rounded(px(metrics.panel_radius))
                .border_1()
                .border_color(border)
                .bg(card)
                .overflow_hidden()
                .child(preview_bar(0.86, metrics, content_line))
                .child(preview_bar(1.0, metrics, content_line))
                .child(preview_bar(0.62, metrics, content_line)),
        )
        .into_any_element()
}

/// The palettes one mode card paints.
///
/// Light and dark paint their own slot. System paints both, because following
/// the platform means either appearance can be the one on screen.
#[derive(Clone, Copy)]
struct ModePalettes {
    primary: &'static GpuiThemeDefinition,
    secondary: Option<&'static GpuiThemeDefinition>,
}

fn mode_palettes(selection: &ThemeSelection, mode: ThemeMode) -> ModePalettes {
    match mode {
        ThemeMode::Light => ModePalettes {
            primary: resolve_selection(selection, GpuiThemeMode::Light),
            secondary: None,
        },
        ThemeMode::Dark => ModePalettes {
            primary: resolve_selection(selection, GpuiThemeMode::Dark),
            secondary: None,
        },
        ThemeMode::System => ModePalettes {
            primary: resolve_selection(selection, GpuiThemeMode::Light),
            secondary: Some(resolve_selection(selection, GpuiThemeMode::Dark)),
        },
    }
}

/// The preview a mode card paints: one face, or two when the mode follows the
/// system.
fn mode_preview(selection: &ThemeSelection, mode: ThemeMode, scale: f32) -> AnyElement {
    let palettes = mode_palettes(selection, mode);
    let Some(secondary) = palettes.secondary else {
        return preview_face(palettes.primary, FULL_FACE.scaled(scale));
    };
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(preview_face(palettes.primary, SPLIT_FACE.scaled(scale))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(preview_face(secondary, SPLIT_FACE.scaled(scale))),
        )
        .into_any_element()
}

/// The localized names painted under the three appearance cards.
#[derive(Clone, Copy)]
pub struct ThemeModeLabels {
    pub system: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
}

/// The modes the picker offers, in reading order.
fn mode_options(labels: ThemeModeLabels) -> [(&'static str, ThemeMode, &'static str); 3] {
    [
        ("theme-mode-system", ThemeMode::System, labels.system),
        ("theme-mode-light", ThemeMode::Light, labels.light),
        ("theme-mode-dark", ThemeMode::Dark, labels.dark),
    ]
}

/// One appearance card: the preview, then the mode name.
///
/// The card is a radio button rather than a plain click target so the current
/// choice is announced; the selected card keeps a primary border, and the
/// label below states the mode in words as well as in color.
fn mode_card(
    id: &'static str,
    label: &'static str,
    preview: AnyElement,
    selected: bool,
    is_dark: bool,
    height: f32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let border = theme::semantic_color("border", is_dark);
    let primary = theme::semantic_color("primary", is_dark);
    let ring = theme::semantic_color("ring", is_dark);
    let muted_foreground = theme::semantic_color("muted-foreground", is_dark);
    let hover_border = muted_foreground.opacity(0.45);

    v_flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(px(MODE_LABEL_GAP))
        .child(
            div()
                .id(id)
                .w_full()
                .h(px(height))
                .p(px(MODE_CARD_PADDING))
                .rounded(px(MODE_CARD_RADIUS))
                .border_1()
                .border_color(border)
                .when(selected, |this| this.border_2().border_color(primary))
                .when(!selected, |this| {
                    this.hover(move |style| style.border_color(hover_border))
                })
                .focusable()
                .tab_stop(true)
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_selected(selected)
                .focus_visible(move |style| style.border_color(ring))
                .on_click(on_click)
                .child(
                    div()
                        .size_full()
                        .rounded(px(MODE_CARD_RADIUS - MODE_CARD_PADDING))
                        .overflow_hidden()
                        .child(preview),
                ),
        )
        .child(
            div()
                .text_xs()
                .when(selected, |this| this.font_medium().text_color(primary))
                .when(!selected, |this| this.text_color(muted_foreground))
                .child(label),
        )
        .into_any_element()
}

/// The appearance control: one preview card per mode, sharing the row's width.
///
/// `available_width` is the settings page's content width; the cards take a
/// third of it each and derive their height from it. `on_select` receives the
/// chosen mode. The cards state the choice in words as well as in color, and
/// the system card paints both appearances so "follow the platform" is visible
/// before it is chosen.
pub fn theme_mode_picker(
    selection: &ThemeSelection,
    selected: ThemeMode,
    labels: ThemeModeLabels,
    is_dark: bool,
    available_width: f32,
    on_select: impl Fn(ThemeMode, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let height = mode_card_height(available_width);
    let scale = height / MODE_CARD_REFERENCE_HEIGHT;
    h_flex()
        .id("theme-mode-cards")
        .role(Role::RadioGroup)
        .w_full()
        .items_start()
        .gap(px(MODE_CARD_GAP))
        .children(mode_options(labels).into_iter().map(|(id, mode, label)| {
            let on_select = on_select.clone();
            mode_card(
                id,
                label,
                mode_preview(selection, mode, scale),
                mode == selected,
                is_dark,
                height,
                move |_, window, cx| on_select(mode, window, cx),
            )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext};

    /// Renders the appearance control on its own, so the card tree is laid out
    /// and painted without standing up the whole settings surface.
    struct ModePickerProbe {
        selection: ThemeSelection,
    }

    impl Render for ModePickerProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(600.0)).child(theme_mode_picker(
                &self.selection,
                ThemeMode::System,
                ThemeModeLabels {
                    system: "System",
                    light: "Light",
                    dark: "Dark",
                },
                false,
                600.0,
                |_, _, _| {},
            ))
        }
    }

    #[gpui::test]
    fn the_mode_picker_paints_every_card(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| ModePickerProbe {
            selection: ThemeSelection::default(),
        });
        for _ in 0..3 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }
    }

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
    fn a_dropdown_never_offers_the_other_appearance() {
        for mode in GpuiThemeMode::ALL {
            for option in theme_options(mode) {
                assert_eq!(
                    option.definition.mode, mode,
                    "{} must not appear in the {mode:?} dropdown",
                    option.id
                );
            }
        }
    }

    #[test]
    fn a_dropdown_selects_the_theme_its_slot_resolves_to() {
        let mut selection = ThemeSelection::default();
        selection.select_light("gruvbox-light");
        selection.select_dark("nord");

        for (mode, expected) in [
            (GpuiThemeMode::Light, "gruvbox-light"),
            (GpuiThemeMode::Dark, "nord"),
        ] {
            let index = selected_theme_index(mode, &selection).expect("a selected row");
            let options = theme_options(mode);
            assert_eq!(options[index.row].id, expected);
        }

        // An untouched slot still points at the catalog default.
        let unset = ThemeSelection::default();
        let index = selected_theme_index(GpuiThemeMode::Dark, &unset).expect("a selected row");
        assert_eq!(
            theme_options(GpuiThemeMode::Dark)[index.row].id,
            "vibex-dark"
        );
    }

    #[test]
    fn the_system_card_paints_both_appearances() {
        let mut selection = ThemeSelection::default();
        selection.select_light("gruvbox-light");
        selection.select_dark("nord");

        let system = mode_palettes(&selection, ThemeMode::System);
        assert_eq!(system.primary.id, "gruvbox-light");
        assert_eq!(system.secondary.map(|theme| theme.id), Some("nord"));

        for (mode, expected) in [
            (ThemeMode::Light, GpuiThemeMode::Light),
            (ThemeMode::Dark, GpuiThemeMode::Dark),
        ] {
            let palettes = mode_palettes(&selection, mode);
            assert!(palettes.secondary.is_none(), "{mode:?} paints one face");
            assert_eq!(palettes.primary.mode, expected);
        }
    }

    #[test]
    fn the_mode_picker_offers_each_mode_once() {
        let options = mode_options(ThemeModeLabels {
            system: "System",
            light: "Light",
            dark: "Dark",
        });
        assert_eq!(
            options.map(|(_, mode, _)| mode),
            [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark]
        );

        let mut ids: Vec<&str> = options.iter().map(|(id, _, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            options.len(),
            "each card needs its own element id"
        );
    }

    #[test]
    fn a_card_keeps_its_aspect_as_the_row_grows() {
        let narrow = mode_card_height(480.0);
        let wide = mode_card_height(960.0);
        assert!(wide > narrow, "a wider row gives taller cards");
        for width in [320.0, 480.0, 764.0, 1200.0] {
            let card_width = (width - 2.0 * MODE_CARD_GAP) / 3.0;
            let height = mode_card_height(width);
            if height > MODE_CARD_MIN_HEIGHT {
                assert!(
                    (card_width / height - MODE_CARD_ASPECT).abs() < 0.01,
                    "width {width} lost the authored aspect: {card_width} x {height}"
                );
            }
        }
        assert_eq!(mode_card_height(0.0), MODE_CARD_MIN_HEIGHT);
    }
}
