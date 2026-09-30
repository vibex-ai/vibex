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
//! reduce    the pure intent → effect reducer
//! worker    the only place that performs I/O
//! run       the event loop
//! ```

pub mod action;
pub mod app;
pub mod composer;
pub mod console;
pub mod glyphs;
pub mod keymap;
pub mod layout;
pub mod locale;
pub mod markdown;
pub mod modal;
pub mod onboarding;
pub mod reduce;
pub mod run;
pub mod search;
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

/// Everything the composition root must decide before the interface starts.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub seat: SeatKind,
    pub theme_id: Option<String>,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    pub capability: ColorCapability,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            seat: SeatKind::Remote,
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Strings::detect().locale,
            capability: ColorCapability::detect(),
        }
    }
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
    let (worker, mut messages) = Worker::start(facade.clone())?;
    let mut app = App::new(
        facade,
        AppOptions {
            seat: options.seat,
            capability: options.capability,
            theme_id: options.theme_id,
            mode: options.mode,
            locale: options.locale,
            sidebar_path: App::sidebar_arrangement_path(),
        },
    );
    let result = run::run_loop(&mut app, &worker, &mut messages);
    worker.shutdown();
    result
}
