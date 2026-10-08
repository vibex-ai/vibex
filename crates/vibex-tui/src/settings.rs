//! The settings surface: its rows, their kinds, and the mode machine.
//!
//! A settings screen with more than a handful of rows needs more than one
//! interaction: browsing, filtering, choosing among values, and typing one in.
//! Those are four modes of one surface, not four screens, so [`SettingsMode`]
//! is an explicit state machine and every key handler dispatches on it. That is
//! what keeps `Esc` honest — it means "undo the thing I am in the middle of",
//! and only closes the page when there is nothing in the middle.
//!
//! Values are applied by one function per row ([`App::apply_setting_value`]),
//! used by the picker's live preview, the committed choice and the reset
//! action alike. A preview is therefore never a different code path from a
//! commit, which is what makes "try it, `Esc` puts it back" true rather than
//! approximately true.

use vibex_desktop_model::ThemeSelection;

use crate::app::App;
use crate::locale::{Locale, Strings};
use crate::logo::MarkStyle;
use crate::theme::GlyphMode;

/// The theme id held by the slot for one appearance.
///
/// A theme is authored for one appearance, so a choice is a pair of slots
/// rather than one id: moving to light must not overwrite the dark palette the
/// reader picked, and moving back must find it again.
pub fn theme_slot(selection: &ThemeSelection, mode: vibex_ui::GpuiThemeMode) -> Option<&str> {
    match mode {
        vibex_ui::GpuiThemeMode::Dark => selection.dark(),
        vibex_ui::GpuiThemeMode::Light => selection.light(),
    }
}

/// Record a theme choice in the slot for one appearance.
pub fn select_theme(
    selection: &mut ThemeSelection,
    mode: vibex_ui::GpuiThemeMode,
    id: impl Into<String>,
) {
    match mode {
        vibex_ui::GpuiThemeMode::Dark => selection.select_dark(id),
        vibex_ui::GpuiThemeMode::Light => selection.select_light(id),
    }
}

/// One adjustable setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingRow {
    Mode,
    Theme,
    Icons,
    Mark,
    Motion,
    Transitions,
    Language,
    Keys,
    Workspace,
    Backend,
    Seat,
    Version,
}

/// How a setting's value is presented and changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// A closed set of values: `Enter` opens the chooser.
    Choice,
    /// Two states: `Enter` or `Space` flips it in place.
    Toggle,
    /// Free text: `Enter` opens an inline editor.
    Text,
    /// A command rather than a value.
    Action,
    /// Reported, never changed.
    ReadOnly,
}

/// The group a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Appearance,
    Language,
    Interface,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 3] = [
        SettingsSection::Appearance,
        SettingsSection::Language,
        SettingsSection::Interface,
    ];

    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            SettingsSection::Appearance => strings.settings_section_appearance(),
            SettingsSection::Language => strings.settings_section_language(),
            SettingsSection::Interface => strings.settings_section_interface(),
        }
    }
}

/// One row's static definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingDef {
    pub row: SettingRow,
    pub section: SettingsSection,
    pub kind: SettingKind,
}

/// Every setting, in display order.
pub const SETTINGS: &[SettingDef] = &[
    SettingDef {
        row: SettingRow::Mode,
        section: SettingsSection::Appearance,
        kind: SettingKind::Toggle,
    },
    SettingDef {
        row: SettingRow::Theme,
        section: SettingsSection::Appearance,
        kind: SettingKind::Choice,
    },
    SettingDef {
        row: SettingRow::Icons,
        section: SettingsSection::Appearance,
        kind: SettingKind::Toggle,
    },
    SettingDef {
        row: SettingRow::Mark,
        section: SettingsSection::Appearance,
        kind: SettingKind::Choice,
    },
    SettingDef {
        row: SettingRow::Motion,
        section: SettingsSection::Appearance,
        kind: SettingKind::Toggle,
    },
    SettingDef {
        row: SettingRow::Transitions,
        section: SettingsSection::Appearance,
        kind: SettingKind::Toggle,
    },
    SettingDef {
        row: SettingRow::Language,
        section: SettingsSection::Language,
        kind: SettingKind::Choice,
    },
    SettingDef {
        row: SettingRow::Workspace,
        section: SettingsSection::Interface,
        kind: SettingKind::Text,
    },
    SettingDef {
        row: SettingRow::Keys,
        section: SettingsSection::Interface,
        kind: SettingKind::Action,
    },
    SettingDef {
        row: SettingRow::Backend,
        section: SettingsSection::Interface,
        kind: SettingKind::ReadOnly,
    },
    SettingDef {
        row: SettingRow::Seat,
        section: SettingsSection::Interface,
        kind: SettingKind::ReadOnly,
    },
    SettingDef {
        row: SettingRow::Version,
        section: SettingsSection::Interface,
        kind: SettingKind::ReadOnly,
    },
];

/// The definition for one row.
pub fn definition(row: SettingRow) -> &'static SettingDef {
    SETTINGS
        .iter()
        .find(|definition| definition.row == row)
        .expect("every SettingRow has a definition")
}

/// Which interaction the settings surface is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SettingsMode {
    /// Moving over rows and activating them.
    #[default]
    Browse,
    /// Typing into the search bar.
    Filter,
    /// Choosing among a row's values, previewing as the cursor moves.
    Picking {
        row: SettingRow,
        selected: usize,
        /// The value to restore if the reader leaves with `Esc`.
        original: String,
    },
    /// Typing a row's value in place.
    Editing { row: SettingRow, buffer: String },
}

impl SettingsMode {
    pub const fn is_browse(&self) -> bool {
        matches!(self, SettingsMode::Browse)
    }

    /// The row a sub-mode belongs to, if any.
    pub const fn row(&self) -> Option<SettingRow> {
        match self {
            SettingsMode::Picking { row, .. } | SettingsMode::Editing { row, .. } => Some(*row),
            _ => None,
        }
    }
}

/// Everything the settings page renders from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    /// The reader's theme choice per appearance; an empty slot is the catalog's
    /// default for that appearance.
    pub themes: ThemeSelection,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    pub glyphs: GlyphMode,
    /// The character art the landing mark is drawn with.
    pub mark: MarkStyle,
    /// Whether the mark's own animation runs while the page waits.
    pub motion: bool,
    /// Whether a line scrambles when the text it shows changes.
    pub transitions: bool,
    /// Index into the visible rows.
    pub selected: usize,
    pub view: SettingsMode,
    pub filter: String,
}

/// One value a chooser offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChoice {
    /// The value written when the choice is taken.
    pub value: String,
    pub label: String,
    pub current: bool,
}

impl App {
    // ---- rows -----------------------------------------------------------

    /// The rows the filter leaves visible, in display order.
    pub fn visible_settings(&self) -> Vec<SettingRow> {
        let needles = self
            .settings
            .filter
            .split_whitespace()
            .map(|needle| needle.to_lowercase())
            .collect::<Vec<_>>();
        SETTINGS
            .iter()
            .map(|definition| definition.row)
            .filter(|row| {
                if needles.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {} {}",
                    self.setting_label(*row),
                    self.setting_value(*row),
                    self.setting_description(*row)
                )
                .to_lowercase();
                needles.iter().all(|needle| haystack.contains(needle))
            })
            .collect()
    }

    /// The row the selection currently points at.
    pub fn selected_setting(&self) -> Option<SettingRow> {
        self.visible_settings()
            .get(self.selection_for(crate::keymap::Scope::Settings))
            .copied()
            .or_else(|| self.visible_settings().first().copied())
    }

    /// The row a sub-mode belongs to, else the highlighted row.
    pub fn active_setting(&self) -> Option<SettingRow> {
        self.settings.view.row().or_else(|| self.selected_setting())
    }

    pub fn setting_label(&self, row: SettingRow) -> &'static str {
        let strings = self.strings;
        match row {
            SettingRow::Mode => strings.settings_mode(),
            SettingRow::Theme => strings.settings_theme(),
            SettingRow::Icons => strings.settings_icons(),
            SettingRow::Mark => strings.settings_mark(),
            SettingRow::Motion => strings.settings_motion(),
            SettingRow::Transitions => strings.settings_transitions(),
            SettingRow::Language => strings.settings_language(),
            SettingRow::Keys => strings.settings_keys(),
            SettingRow::Workspace => strings.session_workspace(),
            SettingRow::Backend => strings.settings_backend(),
            SettingRow::Seat => strings.settings_connection(),
            SettingRow::Version => strings.settings_version(),
        }
    }

    pub fn setting_description(&self, row: SettingRow) -> &'static str {
        let strings = self.strings;
        match row {
            SettingRow::Mode => strings.settings_mode_hint(),
            SettingRow::Theme => strings.settings_theme_hint(),
            SettingRow::Icons => strings.settings_icons_hint(),
            SettingRow::Mark => strings.settings_mark_hint(),
            SettingRow::Motion => strings.settings_motion_hint(),
            SettingRow::Transitions => strings.settings_transitions_hint(),
            SettingRow::Language => strings.settings_language_hint(),
            SettingRow::Keys => strings.settings_keys_hint(),
            SettingRow::Workspace => strings.settings_workspace_hint(),
            SettingRow::Backend => strings.settings_backend_hint(),
            SettingRow::Seat => strings.settings_seat_hint(),
            SettingRow::Version => strings.settings_version_hint(),
        }
    }

    /// The row's current value, as it is displayed.
    pub fn setting_value(&self, row: SettingRow) -> String {
        match row {
            SettingRow::Mode => match self.settings.mode {
                vibex_ui::GpuiThemeMode::Dark => self.strings.settings_mode_dark().to_string(),
                vibex_ui::GpuiThemeMode::Light => self.strings.settings_mode_light().to_string(),
            },
            SettingRow::Theme => self.theme.id.to_string(),
            SettingRow::Icons => match self.settings.glyphs {
                GlyphMode::Unicode => self.strings.settings_icons_unicode().to_string(),
                GlyphMode::Ascii => self.strings.settings_icons_ascii().to_string(),
            },
            SettingRow::Mark => match self.settings.mark {
                MarkStyle::Classic => self.strings.settings_mark_classic().to_string(),
                MarkStyle::Glitch => self.strings.settings_mark_glitch().to_string(),
            },
            SettingRow::Motion => self.toggle_label(self.settings.motion),
            SettingRow::Transitions => self.toggle_label(self.settings.transitions),
            SettingRow::Language => self.settings.locale.tag().to_string(),
            SettingRow::Keys => format!(
                "{} · {}",
                self.strings.settings_keys_reload(),
                self.keymap.bindings().len()
            ),
            SettingRow::Workspace => self
                .workspace_path
                .clone()
                .or_else(|| {
                    self.active_session()
                        .map(|session| session.workspace_root.clone())
                })
                .unwrap_or_else(|| self.strings.none().to_string()),
            SettingRow::Backend => self.capabilities.schema_version.clone(),
            SettingRow::Seat => self.seat.label(self.strings).to_string(),
            SettingRow::Version => env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    // ---- values ---------------------------------------------------------

    /// The values a choice row offers, with the current one marked.
    pub fn setting_choices(&self, row: SettingRow) -> Vec<SettingChoice> {
        match row {
            SettingRow::Mode => ["dark", "light"]
                .into_iter()
                .map(|value| SettingChoice {
                    value: value.to_string(),
                    label: match value {
                        "dark" => self.strings.settings_mode_dark().to_string(),
                        _ => self.strings.settings_mode_light().to_string(),
                    },
                    current: value
                        == match self.settings.mode {
                            vibex_ui::GpuiThemeMode::Dark => "dark",
                            vibex_ui::GpuiThemeMode::Light => "light",
                        },
                })
                .collect(),
            SettingRow::Theme => {
                let current = self.theme.id;
                vibex_ui::theme_catalog::themes_for(self.settings.mode)
                    .map(|theme| SettingChoice {
                        value: theme.id.to_string(),
                        label: theme.name.to_string(),
                        current: theme.id == current,
                    })
                    .collect()
            }
            SettingRow::Icons => ["unicode", "ascii"]
                .into_iter()
                .map(|value| SettingChoice {
                    value: value.to_string(),
                    label: if value == "unicode" {
                        self.strings.settings_icons_unicode().to_string()
                    } else {
                        self.strings.settings_icons_ascii().to_string()
                    },
                    current: value
                        == match self.settings.glyphs {
                            GlyphMode::Unicode => "unicode",
                            GlyphMode::Ascii => "ascii",
                        },
                })
                .collect(),
            SettingRow::Mark => MarkStyle::ALL
                .into_iter()
                .map(|style| SettingChoice {
                    value: style.id().to_string(),
                    label: match style {
                        MarkStyle::Classic => self.strings.settings_mark_classic().to_string(),
                        MarkStyle::Glitch => self.strings.settings_mark_glitch().to_string(),
                    },
                    current: style == self.settings.mark,
                })
                .collect(),
            SettingRow::Motion | SettingRow::Transitions => self.toggle_choices(row),
            SettingRow::Language => [Locale::En, Locale::ZhCn, Locale::ZhTw]
                .into_iter()
                .map(|locale| SettingChoice {
                    value: locale.tag().to_string(),
                    label: locale.tag().to_string(),
                    current: locale == self.settings.locale,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The value a choice row resets to.
    pub fn default_setting_value(&self, row: SettingRow) -> String {
        match row {
            SettingRow::Mode => "dark".to_string(),
            // The shipped default for the appearance on screen, not one fixed
            // id: resetting the theme in light mode must not store a dark one.
            SettingRow::Theme => {
                vibex_ui::theme_catalog::default_theme_id(self.settings.mode).to_string()
            }
            SettingRow::Icons => "unicode".to_string(),
            SettingRow::Mark => MarkStyle::default().id().to_string(),
            // Motion and the text transition are the shipped behaviour: a
            // reset puts them back on, which is what a reader who never opened
            // this page already has.
            SettingRow::Motion | SettingRow::Transitions => "on".to_string(),
            SettingRow::Language => Locale::En.tag().to_string(),
            SettingRow::Workspace => String::new(),
            _ => self.setting_value(row),
        }
    }

    /// The two labels a toggle row shows.
    fn toggle_label(&self, on: bool) -> String {
        if on {
            self.strings.settings_on().to_string()
        } else {
            self.strings.settings_off().to_string()
        }
    }

    /// The two values a toggle row offers.
    ///
    /// A toggle is a choice with two values rather than a kind of its own: the
    /// picker, the key that steps it and the reset all read the same list, so
    /// they cannot disagree about what the other state is called.
    fn toggle_choices(&self, row: SettingRow) -> Vec<SettingChoice> {
        let current = match row {
            SettingRow::Motion => self.settings.motion,
            _ => self.settings.transitions,
        };
        [("on", true), ("off", false)]
            .into_iter()
            .map(|(value, on)| SettingChoice {
                value: value.to_string(),
                label: self.toggle_label(on),
                current: on == current,
            })
            .collect()
    }

    /// The state a toggle value names, or `None` when it names neither.
    pub(crate) fn toggle_value(value: &str) -> Option<bool> {
        match value {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }
    }

    /// Apply one value to one row.
    ///
    /// Returns whether anything changed. This is the only writer: the chooser's
    /// preview, its commit, the inline editor and the reset all come through
    /// here, so they cannot disagree — and every change is written to the
    /// interface's own file, so the next run starts where this one left off.
    pub fn apply_setting_value(&mut self, row: SettingRow, value: &str) -> bool {
        let changed = self.apply_setting_value_inner(row, value);
        if changed {
            self.save_interface_preferences();
        }
        changed
    }

    fn apply_setting_value_inner(&mut self, row: SettingRow, value: &str) -> bool {
        match row {
            SettingRow::Mode => {
                let mode = match value {
                    "light" => vibex_ui::GpuiThemeMode::Light,
                    "dark" => vibex_ui::GpuiThemeMode::Dark,
                    _ => return false,
                };
                if mode == self.settings.mode {
                    return false;
                }
                self.settings.mode = mode;
                self.rebuild_appearance();
                true
            }
            SettingRow::Theme => {
                if value.is_empty()
                    || value == self.theme.id
                    || vibex_ui::theme_catalog::theme_for(value, self.settings.mode).is_none()
                {
                    return false;
                }
                select_theme(&mut self.settings.themes, self.settings.mode, value);
                self.rebuild_appearance();
                true
            }
            SettingRow::Icons => {
                let glyphs = match value {
                    "ascii" => GlyphMode::Ascii,
                    "unicode" => GlyphMode::Unicode,
                    _ => return false,
                };
                if glyphs == self.settings.glyphs {
                    return false;
                }
                self.settings.glyphs = glyphs;
                self.rebuild_appearance();
                true
            }
            SettingRow::Mark => {
                let Some(mark) = MarkStyle::from_id(value) else {
                    return false;
                };
                if mark == self.settings.mark {
                    return false;
                }
                self.settings.mark = mark;
                true
            }
            SettingRow::Motion => {
                let Some(on) = Self::toggle_value(value) else {
                    return false;
                };
                if on == self.settings.motion {
                    return false;
                }
                self.settings.motion = on;
                true
            }
            SettingRow::Transitions => {
                let Some(on) = Self::toggle_value(value) else {
                    return false;
                };
                if on == self.settings.transitions {
                    return false;
                }
                self.settings.transitions = on;
                // A transition in flight is a promise the effect will keep
                // drawing: switching the effect off mid-line stops it where it
                // is, and the line it was carrying is drawn as the text it
                // already is.
                if !on {
                    self.transitions.cancel();
                }
                true
            }
            SettingRow::Language => {
                let Some(locale) = [Locale::En, Locale::ZhCn, Locale::ZhTw]
                    .into_iter()
                    .find(|locale| locale.tag() == value)
                else {
                    return false;
                };
                if locale == self.settings.locale {
                    return false;
                }
                self.settings.locale = locale;
                self.strings = Strings::with_locale(locale);
                true
            }
            SettingRow::Workspace => {
                let path = (!value.trim().is_empty()).then(|| value.trim().to_string());
                if path == self.workspace_path && path == self.preferred_workspace {
                    return false;
                }
                self.workspace_path = path.clone();
                // The row is the *default* for new sessions, not this run's
                // choice of workspace: a path picked in the browser for one
                // session leaves the file alone, and only this row writes the
                // standing answer down.
                self.preferred_workspace = path;
                true
            }
            SettingRow::Keys | SettingRow::Backend | SettingRow::Seat | SettingRow::Version => {
                false
            }
        }
    }

    /// Re-resolve the theme and re-measure the transcript after a look change.
    pub fn rebuild_appearance(&mut self) {
        self.capability.glyphs = self.settings.glyphs;
        self.theme = crate::theme::TuiTheme::resolve(
            theme_slot(&self.settings.themes, self.settings.mode),
            self.settings.mode,
            self.capability,
        );
        // Only the look changed; the width is still whatever the renderer last
        // drew the transcript at.
        self.transcript.set_theme(&self.theme.clone());
    }

    // ---- the mode machine ----------------------------------------------

    /// Move the highlight, skipping nothing because only valued rows exist.
    pub fn move_setting_selection(&mut self, delta: i64) {
        let rows = self.visible_settings();
        if rows.is_empty() {
            return;
        }
        let scope = crate::keymap::Scope::Settings;
        let current = self.selection_for(scope) as i64;
        let next = (current + delta).clamp(0, rows.len() as i64 - 1) as usize;
        self.set_selection(scope, next);
    }

    /// Enter the settings filter.
    pub fn begin_settings_filter(&mut self) {
        self.settings.filter.clear();
        self.settings.view = SettingsMode::Filter;
        self.set_selection(crate::keymap::Scope::Settings, 0);
    }

    /// Leave the filter, keeping the query (the design keeps `Enter` sticky and
    /// clears on `Esc`).
    pub fn leave_settings_filter(&mut self, clear: bool) {
        if clear {
            self.settings.filter.clear();
        }
        self.settings.view = SettingsMode::Browse;
        self.set_selection(crate::keymap::Scope::Settings, 0);
    }

    /// Append to the filter query and re-clamp the selection.
    pub fn push_settings_filter(&mut self, character: char) {
        self.settings.filter.push(character);
        self.set_selection(crate::keymap::Scope::Settings, 0);
    }

    pub fn pop_settings_filter(&mut self) {
        self.settings.filter.pop();
        self.set_selection(crate::keymap::Scope::Settings, 0);
    }

    /// Open the chooser for `row`, remembering the value to restore.
    ///
    /// What is remembered is the row's *value*, not the words it is displayed
    /// as: `Esc` puts the old choice back by applying it, and a row whose value
    /// is a label — the appearance row says "Dark", the value is `dark` — would
    /// otherwise restore a spelling nothing reads and quietly keep the preview.
    pub fn begin_setting_pick(&mut self, row: SettingRow) -> bool {
        let choices = self.setting_choices(row);
        if choices.is_empty() {
            return false;
        }
        let selected = choices
            .iter()
            .position(|choice| choice.current)
            .unwrap_or(0);
        let original = choices
            .get(selected)
            .map(|choice| choice.value.clone())
            .unwrap_or_else(|| self.setting_value(row));
        self.settings.view = SettingsMode::Picking {
            row,
            selected,
            original,
        };
        true
    }

    /// Move the chooser's cursor, previewing the value as it lands.
    pub fn step_setting_pick(&mut self, delta: i64) {
        let SettingsMode::Picking {
            row,
            selected,
            original,
        } = self.settings.view.clone()
        else {
            return;
        };
        let choices = self.setting_choices(row);
        if choices.is_empty() {
            return;
        }
        let next = (selected as i64 + delta).rem_euclid(choices.len() as i64) as usize;
        self.settings.view = SettingsMode::Picking {
            row,
            selected: next,
            original,
        };
        let value = choices[next].value.clone();
        self.apply_setting_value(row, &value);
    }

    /// Commit the previewed value.
    pub fn commit_setting_pick(&mut self) -> bool {
        if matches!(self.settings.view, SettingsMode::Picking { .. }) {
            self.settings.view = SettingsMode::Browse;
            true
        } else {
            false
        }
    }

    /// Put the original value back and leave the chooser.
    pub fn cancel_setting_pick(&mut self) -> bool {
        let SettingsMode::Picking { row, original, .. } = std::mem::take(&mut self.settings.view)
        else {
            return false;
        };
        self.apply_setting_value(row, &original);
        self.settings.view = SettingsMode::Browse;
        true
    }

    /// Open the inline editor for a text row.
    pub fn begin_setting_edit(&mut self, row: SettingRow) -> bool {
        if definition(row).kind != SettingKind::Text {
            return false;
        }
        let buffer = match row {
            SettingRow::Workspace => self.workspace_path.clone().unwrap_or_default(),
            _ => self.setting_value(row),
        };
        self.settings.view = SettingsMode::Editing { row, buffer };
        true
    }

    /// Commit the inline editor's buffer.
    pub fn commit_setting_edit(&mut self) -> bool {
        let SettingsMode::Editing { row, buffer } = std::mem::take(&mut self.settings.view) else {
            return false;
        };
        self.apply_setting_value(row, &buffer);
        self.settings.view = SettingsMode::Browse;
        true
    }

    /// Discard the inline editor's buffer.
    pub fn cancel_setting_edit(&mut self) -> bool {
        let SettingsMode::Editing { .. } = std::mem::take(&mut self.settings.view) else {
            return false;
        };
        self.settings.view = SettingsMode::Browse;
        true
    }

    /// Reset one row to its default value.
    pub fn reset_setting(&mut self, row: SettingRow) -> bool {
        let value = self.default_setting_value(row);
        self.apply_setting_value(row, &value)
    }
}
