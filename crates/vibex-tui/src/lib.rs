#![forbid(unsafe_code)]
//! Vibex TUI: the character-grid client for a `DesktopRuntime`.
//!
//! The crate is a *client*, and the dependency boundary is load-bearing:
//! it consumes the `vibex-backend` domain traits and never starts, owns or
//! reaches past the runtime. Seat selection (in-process authority versus a
//! remote runtime) belongs to the composition root that builds the
//! [`vibex_backend::BackendFacade`] this crate is handed.
//!
//! Layering, from the bottom up:
//!
//! ```text
//! console   `stderr` diversion while the interface owns the terminal
//! terminal  raw mode, restoration, OSC 52, $EDITOR hand-off
//! theme     design tokens → terminal colour with truecolor/256/16/none degradation
//! text      grapheme-correct measurement, wrapping with joiners, bidi order
//! locale    en / zh-CN / zh-TW product copy
//! attachment an attachment's place in the message, drawn back as a placeholder
//! keymap    one binding table per scope: dispatch + key bar + help
//! layout    the screen as a vertical stack of full-width bands
//! glyphs    the chrome glyph vocabulary, with per-terminal fallbacks
//! modal     the one chrome every popup is drawn through
//! onboarding what to do first, in order, until it is done
//! composer  the edit buffer, completion triggers and history
//! search    transcript search: regex with smart case
//! transcript block cache, incremental layout, viewport-only rendering
//! view      page shells, overlays, the key bar
//! app       navigation and overlay state
//! interface_prefs what the settings surface remembers between runs
//! runtime_prefs what the switcher remembers between runs
//! runtime_picker the switcher's catalogue, folded and filtered, and its staged run options
//! reduce    the pure intent → effect reducer
//! worker    the only place that performs I/O
//! run       the event loop
//! ```

pub mod action;
pub mod app;
pub mod attachment;
pub mod auto_continue;
pub mod composer;
pub mod console;
pub mod glyphs;
pub mod interface_prefs;
pub mod keymap;
pub mod layout;
pub mod locale;
pub mod logo;
pub mod markdown;
pub mod modal;
pub mod onboarding;
pub mod reduce;
pub mod run;
pub mod runtime_picker;
pub mod runtime_prefs;
pub mod search;
pub mod sessions;
pub mod settings;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod transcript;
pub mod view;
pub mod worker;

pub use app::{App, AppOptions, Effect, Page};
pub use locale::{Locale, Strings};
pub use run::{ExitReason, run_loop};
pub use theme::{ColorCapability, ColorMode, GlyphMode, TuiTheme};
pub use view::SeatKind;
pub use worker::{AppMessage, Worker};

use vibex_backend::{BackendError, BackendFacade, BackendResult};

use crate::interface_prefs::InterfacePreferences;

/// Everything the composition root must decide before the interface starts.
///
/// The three look-and-language fields are optional on purpose: `None` means the
/// composition root did not name a value, and what the last run remembered —
/// then detection — answers instead. A value set here (`--theme`, a future
/// `--lang`) outranks both.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub seat: SeatKind,
    pub theme_id: Option<String>,
    pub mode: Option<vibex_ui::GpuiThemeMode>,
    pub locale: Option<Locale>,
    pub capability: ColorCapability,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            seat: SeatKind::Remote,
            theme_id: None,
            mode: None,
            locale: None,
            capability: ColorCapability::detect(),
        }
    }
}

/// What the interface starts with, once every source has been asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiStartup {
    /// The theme this run was named, when the composition root or
    /// `VIBEX_THEME` named one. It seeds the appearance slot for the run
    /// without erasing what the other appearance remembers.
    pub theme_id: Option<String>,
    /// Everything the file remembered, carried through so the app can save it
    /// back beside the values this run resolved differently.
    pub remembered: InterfacePreferences,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    pub glyphs: GlyphMode,
}

impl TuiOptions {
    /// Resolve what the interface starts with.
    ///
    /// Precedence, highest first: what the composition root set, the
    /// environment (`VIBEX_THEME`, `VIBEX_TUI_ICONS`), what the last run
    /// remembered, and detection. The environment is this invocation's answer
    /// and the file is the reader's standing one, so the environment wins for
    /// the run; the reader's own standing choice still beats detection, which
    /// is only a guess about the terminal.
    ///
    /// The lookup is a parameter rather than `std::env` so the whole order can
    /// be asserted without a terminal.
    pub fn resolve(
        &self,
        remembered: InterfacePreferences,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> TuiStartup {
        let mode = self
            .mode
            .or_else(|| remembered.mode())
            .unwrap_or(vibex_ui::GpuiThemeMode::Dark);
        let theme_id = self
            .theme_id
            .clone()
            .or_else(|| named_theme(&mut lookup))
            .filter(|theme_id| !theme_id.trim().is_empty());
        let locale = self
            .locale
            .or_else(|| remembered.locale())
            .or_else(|| Strings::detect_from(&mut lookup).map(|strings| strings.locale))
            .unwrap_or(Locale::En);
        let glyphs = GlyphMode::explicit_from(&mut lookup)
            .or_else(|| remembered.glyphs())
            .unwrap_or(self.capability.glyphs);
        TuiStartup {
            theme_id,
            remembered,
            mode,
            locale,
            glyphs,
        }
    }
}

/// The theme id the environment names, if any.
fn named_theme(lookup: &mut impl FnMut(&str) -> Option<String>) -> Option<String> {
    lookup("VIBEX_THEME").filter(|value| !value.trim().is_empty())
}

/// Run the interface against an already-constructed facade.
///
/// Returns an error rather than drawing anything when stdin or stdout is not a
/// terminal, so a piped invocation produces a usable message and a non-zero
/// exit code instead of a half-rendered screen.
pub fn run(facade: BackendFacade, options: TuiOptions) -> BackendResult<ExitReason> {
    if !terminal::is_interactive_terminal() {
        return Err(BackendError::failed(
            "tui_not_a_terminal",
            terminal::non_interactive_help(),
        ));
    }
    let preferences_path = App::interface_preferences_path();
    let remembered = InterfacePreferences::load(preferences_path.as_deref());
    let startup = options.resolve(remembered, |key| std::env::var(key).ok());
    let (worker, mut messages) = Worker::start(facade.clone())?;
    let mut app = App::new(
        facade,
        AppOptions {
            seat: options.seat,
            capability: ColorCapability {
                mode: options.capability.mode,
                glyphs: startup.glyphs,
            },
            theme_id: startup.theme_id,
            mode: startup.mode,
            locale: startup.locale,
            remembered: startup.remembered,
            sidebar_path: App::sidebar_arrangement_path(),
            runtime_path: App::runtime_preferences_path(),
            preferences_path,
        },
    );
    let result = run::run_loop(&mut app, &worker, &mut messages);
    worker.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Options whose capability does not depend on the machine running the
    /// test; every other field is "the root named nothing".
    fn options() -> TuiOptions {
        TuiOptions {
            seat: SeatKind::Remote,
            theme_id: None,
            mode: None,
            locale: None,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        }
    }

    fn env_map<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl FnMut(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn a_remembered_choice_answers_when_the_root_names_nothing() {
        let remembered = InterfacePreferences {
            mode: Some("light".to_string()),
            icons: Some("ascii".to_string()),
            locale: Some("zh-TW".to_string()),
            ..InterfacePreferences::default()
        };

        let startup = options().resolve(
            remembered.clone(),
            env_map(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")]),
        );
        assert_eq!(startup.mode, vibex_ui::GpuiThemeMode::Light);
        assert_eq!(startup.glyphs, GlyphMode::Ascii);
        assert_eq!(startup.locale, Locale::ZhTw);
        assert_eq!(startup.theme_id, None, "the file owns the theme slots");
        assert_eq!(startup.remembered, remembered);
    }

    #[test]
    fn the_environment_outranks_the_file_for_this_run_without_rewriting_it() {
        let remembered = InterfacePreferences {
            icons: Some("unicode".to_string()),
            locale: Some("zh-CN".to_string()),
            ..InterfacePreferences::default()
        };

        let startup = options().resolve(
            remembered,
            env_map(&[
                ("VIBEX_THEME", "solar-light"),
                ("VIBEX_TUI_ICONS", "ascii"),
                ("LANG", "C"),
            ]),
        );
        assert_eq!(startup.theme_id.as_deref(), Some("solar-light"));
        assert_eq!(startup.glyphs, GlyphMode::Ascii);
        // The reader's standing language beats `LANG`: that is a guess about
        // the terminal, not a demand the way `VIBEX_TUI_ICONS` is.
        assert_eq!(startup.locale, Locale::ZhCn);
        assert_eq!(
            startup.remembered.icons.as_deref(),
            Some("unicode"),
            "the run's own choice is not written into the file it read"
        );
    }

    #[test]
    fn an_explicit_option_outranks_the_environment_and_the_file() {
        let remembered = InterfacePreferences {
            mode: Some("light".to_string()),
            icons: Some("unicode".to_string()),
            locale: Some("zh-CN".to_string()),
            ..InterfacePreferences::default()
        };
        let named = TuiOptions {
            theme_id: Some("vibex-dark".to_string()),
            mode: Some(vibex_ui::GpuiThemeMode::Dark),
            locale: Some(Locale::En),
            ..options()
        };

        let startup = named.resolve(
            remembered,
            env_map(&[
                ("VIBEX_THEME", "solar-light"),
                ("VIBEX_TUI_ICONS", "ascii"),
                ("LANG", "zh_TW.UTF-8"),
            ]),
        );
        assert_eq!(startup.theme_id.as_deref(), Some("vibex-dark"));
        assert_eq!(startup.mode, vibex_ui::GpuiThemeMode::Dark);
        assert_eq!(startup.locale, Locale::En);
        assert_eq!(startup.glyphs, GlyphMode::Ascii);
    }

    #[test]
    fn detection_answers_last_and_the_default_is_dark_english_unicode() {
        let startup = options().resolve(
            InterfacePreferences::default(),
            env_map(&[("LANG", "zh_CN.UTF-8"), ("LC_ALL", "zh_CN.UTF-8")]),
        );
        assert_eq!(startup.mode, vibex_ui::GpuiThemeMode::Dark);
        // The process locale is detection's answer, and there is nothing in the
        // file to outrank it.
        assert_eq!(startup.locale, Locale::ZhCn);
        // The capability the composition root detected is the last word on the
        // icon set: `resolve` does not detect a second time.
        assert_eq!(startup.glyphs, GlyphMode::Unicode);
        assert_eq!(startup.theme_id, None);
    }

    #[test]
    fn a_blank_named_theme_is_not_a_name() {
        let startup = options().resolve(
            InterfacePreferences::default(),
            env_map(&[("VIBEX_THEME", "   ")]),
        );
        assert_eq!(startup.theme_id, None);
    }
}
