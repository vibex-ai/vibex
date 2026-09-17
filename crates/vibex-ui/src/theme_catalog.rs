//! Resolution over the generated multi-theme token catalog.
//!
//! [`crate::generated_tokens`] owns the data — every theme variant, its
//! semantic colors, and its syntax highlight block. This module owns the
//! policy on top of it: which variant an appearance uses, and what happens
//! when a persisted selection names a theme that is no longer in the catalog.
//!
//! The resolution rules are deliberately total. A theme id can come from a
//! settings file written by a newer build, from a user-edited document, or
//! from a removed palette, so an unknown id must degrade to the default for
//! that appearance rather than fail the frame.

use std::sync::OnceLock;

use vibex_desktop_model::ThemeSelection;

use crate::{
    DEFAULT_DARK_THEME_ID, DEFAULT_LIGHT_THEME_ID, GpuiColorToken, GpuiThemeDefinition,
    GpuiThemeMode, THEMES,
};

/// User themes compiled from theme files, installed once at startup.
///
/// `OnceLock` rather than a lock: the set is process-lifetime configuration
/// read on every paint, so it must hand out genuinely `'static` references
/// without locking or reference counting. Adding themes therefore happens
/// before the first frame; changing a file takes effect on the next launch.
static CUSTOM: OnceLock<Vec<GpuiThemeDefinition>> = OnceLock::new();

/// Why user themes could not be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomThemeInstallError {
    /// Themes were already installed.
    ///
    /// Installation is once-only so that references already handed to the
    /// renderer stay valid for the life of the process.
    AlreadyInstalled,
}

impl std::fmt::Display for CustomThemeInstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInstalled => {
                write!(formatter, "user themes were already installed")
            }
        }
    }
}

impl std::error::Error for CustomThemeInstallError {}

/// Install user themes for the rest of the process.
///
/// Ids that collide with a built-in shadow it, so a user can iterate on a
/// palette without renaming it. Installing twice is refused rather than
/// silently ignored, because the second set would never be observable.
pub fn install_custom_themes(
    themes: Vec<GpuiThemeDefinition>,
) -> Result<(), CustomThemeInstallError> {
    CUSTOM
        .set(themes)
        .map_err(|_| CustomThemeInstallError::AlreadyInstalled)
}

/// The installed user themes.
pub fn custom_themes() -> &'static [GpuiThemeDefinition] {
    CUSTOM.get().map(Vec::as_slice).unwrap_or(&[])
}

/// The full catalog, in an order that survives user themes being installed.
///
/// Built-ins keep their positions; a user theme that shadows one takes that
/// built-in's slot, and a genuinely new user theme appends at the end. Both
/// [`theme_index`] and [`theme_at`] walk this order, and the renderer caches a
/// position per appearance, so positions must not shift when the user theme set
/// changes — prepending them would silently repoint every cached built-in.
///
/// Keeping one entry per id also stops a shadowed built-in from appearing
/// beside its replacement in the picker.
fn all_themes() -> impl Iterator<Item = &'static GpuiThemeDefinition> {
    let custom = custom_themes();
    let builtins = THEMES.iter().map(move |builtin| {
        custom
            .iter()
            .find(|theme| theme.id == builtin.id)
            .unwrap_or(builtin)
    });
    let appended = custom
        .iter()
        .filter(move |theme| !THEMES.iter().any(|builtin| builtin.id == theme.id));
    builtins.chain(appended)
}

/// The default theme id for an appearance.
///
/// These ids are recorded in the token source, not derived here, so the
/// catalog and the source cannot disagree about which palette ships as the
/// product default.
pub fn default_theme_id(mode: GpuiThemeMode) -> &'static str {
    match mode {
        GpuiThemeMode::Light => DEFAULT_LIGHT_THEME_ID,
        GpuiThemeMode::Dark => DEFAULT_DARK_THEME_ID,
    }
}

/// Look up a theme by its stable id, ignoring which appearance it is for.
pub fn theme(id: &str) -> Option<&'static GpuiThemeDefinition> {
    all_themes().find(|theme| theme.id == id)
}

/// Look up a theme by id, requiring it to be authored for `mode`.
///
/// A light palette is not a valid dark selection: its surfaces are built for
/// the opposite appearance, so accepting it would paint unreadable text.
pub fn theme_for(id: &str, mode: GpuiThemeMode) -> Option<&'static GpuiThemeDefinition> {
    theme(id).filter(|theme| theme.mode == mode)
}

/// Every theme authored for `mode`, in authoring order.
pub fn themes_for(mode: GpuiThemeMode) -> impl Iterator<Item = &'static GpuiThemeDefinition> {
    all_themes().filter(move |theme| theme.mode == mode)
}

/// The catalog position of a theme, or `None` when it is absent or authored
/// for the other appearance.
///
/// Renderers that must read the active palette without an `App` handle cache
/// this position in a lock-free slot instead of the id string.
pub fn theme_index(id: &str, mode: GpuiThemeMode) -> Option<usize> {
    all_themes().position(|theme| theme.id == id && theme.mode == mode)
}

/// The theme at a catalog position.
pub fn theme_at(index: usize) -> Option<&'static GpuiThemeDefinition> {
    all_themes().nth(index)
}

/// The catalog default for `mode`.
///
/// Panics only if the generated catalog is internally inconsistent, which the
/// token generator rejects before it can emit Rust and
/// [`tests::defaults_resolve`] pins.
pub fn default_theme(mode: GpuiThemeMode) -> &'static GpuiThemeDefinition {
    let id = default_theme_id(mode);
    theme_for(id, mode)
        .unwrap_or_else(|| panic!("generated theme catalog is missing its {mode:?} default {id:?}"))
}

/// Resolve an explicit selection, falling back to the default for `mode`.
pub fn resolve_theme(id: Option<&str>, mode: GpuiThemeMode) -> &'static GpuiThemeDefinition {
    id.and_then(|id| theme_for(id, mode))
        .unwrap_or_else(|| default_theme(mode))
}

/// Resolve the theme a persisted selection picks for `mode`.
pub fn resolve_selection(
    selection: &ThemeSelection,
    mode: GpuiThemeMode,
) -> &'static GpuiThemeDefinition {
    let requested = match mode {
        GpuiThemeMode::Light => selection.light(),
        GpuiThemeMode::Dark => selection.dark(),
    };
    resolve_theme(requested, mode)
}

/// Read a semantic token out of one theme.
///
/// A missing token is a build-time contract between the token source and the
/// call site, so it panics rather than silently painting a wrong color.
pub fn semantic_token(theme: &GpuiThemeDefinition, name: &str) -> Option<GpuiColorToken> {
    theme
        .tokens
        .iter()
        .copied()
        .find(|token| token.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn defaults_resolve() {
        assert_eq!(default_theme(GpuiThemeMode::Light).id, "vibex-light");
        assert_eq!(default_theme(GpuiThemeMode::Dark).id, "vibex-dark");
        assert_eq!(default_theme_id(GpuiThemeMode::Light), "vibex-light");
        assert_eq!(default_theme_id(GpuiThemeMode::Dark), "vibex-dark");
    }

    #[test]
    fn both_appearances_offer_more_than_one_choice() {
        for mode in GpuiThemeMode::ALL {
            let count = themes_for(mode).count();
            assert!(count >= 2, "{mode:?} offers only {count} theme(s)");
        }
    }

    #[test]
    fn every_theme_has_a_unique_id_and_a_name() {
        let mut ids = BTreeSet::new();
        for theme in THEMES {
            assert!(ids.insert(theme.id), "duplicate theme id {}", theme.id);
            assert!(!theme.name.trim().is_empty(), "{} has no name", theme.id);
        }
    }

    #[test]
    fn every_theme_carries_the_full_shared_token_contract() {
        let reference: BTreeSet<&str> = default_theme(GpuiThemeMode::Dark)
            .tokens
            .iter()
            .map(|token| token.name)
            .collect();
        assert!(reference.len() >= 40);
        for theme in THEMES {
            let names: BTreeSet<&str> = theme.tokens.iter().map(|token| token.name).collect();
            assert_eq!(
                names, reference,
                "{} does not carry the shared semantic token contract",
                theme.id
            );
            assert!(
                !theme.highlight_json.is_empty(),
                "{} has no syntax highlight block",
                theme.id
            );
        }
    }

    #[test]
    fn unknown_and_mismatched_ids_fall_back_to_the_default() {
        assert_eq!(
            resolve_theme(Some("no-such-theme"), GpuiThemeMode::Dark).id,
            "vibex-dark"
        );
        assert_eq!(resolve_theme(None, GpuiThemeMode::Light).id, "vibex-light");
        // A real id authored for the other appearance must not be adopted.
        assert_eq!(
            resolve_theme(Some("vibex-light"), GpuiThemeMode::Dark).id,
            "vibex-dark"
        );
        assert_eq!(
            resolve_theme(Some("vibex-dark"), GpuiThemeMode::Light).id,
            "vibex-light"
        );
    }

    #[test]
    fn each_slot_of_a_selection_resolves_independently() {
        let mut selection = ThemeSelection::default();
        selection.select_light("gruvbox-light");
        selection.select_dark("nord");

        assert_eq!(
            resolve_selection(&selection, GpuiThemeMode::Light).id,
            "gruvbox-light"
        );
        assert_eq!(
            resolve_selection(&selection, GpuiThemeMode::Dark).id,
            "nord"
        );

        selection.select_light("solarized-light");
        assert_eq!(
            resolve_selection(&selection, GpuiThemeMode::Dark).id,
            "nord",
            "changing the light slot leaves the dark slot alone"
        );
    }

    #[test]
    fn catalog_positions_round_trip_and_reject_the_other_appearance() {
        for (index, theme) in THEMES.iter().enumerate() {
            assert_eq!(theme_index(theme.id, theme.mode), Some(index));
            assert_eq!(theme_at(index).map(|found| found.id), Some(theme.id));
            let opposite = match theme.mode {
                GpuiThemeMode::Light => GpuiThemeMode::Dark,
                GpuiThemeMode::Dark => GpuiThemeMode::Light,
            };
            assert_eq!(
                theme_index(theme.id, opposite),
                None,
                "{} must not resolve for {opposite:?}",
                theme.id
            );
        }
        assert!(theme_index("no-such-theme", GpuiThemeMode::Dark).is_none());
        assert!(theme_at(THEMES.len()).is_none());
    }

    #[test]
    fn every_theme_exposes_the_core_roles_with_valid_channels() {
        for theme in THEMES {
            for role in ["background", "foreground", "primary", "border"] {
                let token = semantic_token(theme, role)
                    .unwrap_or_else(|| panic!("{} is missing {role}", theme.id));
                assert!(token.rgb <= 0x00ff_ffff, "{} {role}", theme.id);
                assert!(
                    (0.0..=1.0).contains(&token.alpha),
                    "{} {role} alpha",
                    theme.id
                );
            }
            assert!(
                semantic_token(theme, "no-such-role").is_none(),
                "{} resolved a role it does not define",
                theme.id
            );
        }
    }

    #[test]
    fn authored_foregrounds_stay_readable_on_their_background() {
        fn luminance(token: GpuiColorToken) -> f32 {
            let channel = |shift: u32| {
                let value = ((token.rgb >> shift) & 0xff) as f32 / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        }
        fn ratio(theme: &GpuiThemeDefinition, foreground: &str, background: &str) -> f32 {
            let foreground = luminance(semantic_token(theme, foreground).unwrap());
            let background = luminance(semantic_token(theme, background).unwrap());
            (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
        }

        for theme in THEMES {
            // Opaque pairs only: an alpha role composites against whatever it
            // is painted over, which this check cannot know.
            for (foreground, background, minimum) in [
                ("foreground", "background", 4.5),
                ("muted-foreground", "background", 4.5),
                ("card-foreground", "card", 4.5),
                ("primary-foreground", "primary", 4.5),
            ] {
                let actual = ratio(theme, foreground, background);
                assert!(
                    actual >= minimum,
                    "{}: {foreground} on {background} is {actual:.2}:1, expected {minimum}:1",
                    theme.id
                );
            }
        }
    }
}
