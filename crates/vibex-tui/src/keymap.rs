//! Key chords, the per-scope binding tables, and the user remap file.
//!
//! One table per scope drives three things at once: key dispatch, the footer
//! key bar, and the `?` help panel. They cannot drift because they are the same
//! data.
//!
//! Two rules are load-bearing:
//!
//! * [`Binding::label`] controls **display only**. Dispatch never consults it,
//!   so a hidden alias still works and a disabled action can still explain
//!   itself instead of silently swallowing the key.
//! * Bindings are data, not `match` arms, so `~/.vibex/tui-keys.toml` can
//!   replace them without recompiling.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::locale::Strings;

/// A single key press with its relevant modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Chord {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    pub const fn ctrl(character: char) -> Self {
        Self {
            code: KeyCode::Char(character),
            modifiers: KeyModifiers::CONTROL,
        }
    }

    /// Normalise a raw terminal event.
    ///
    /// Shift is folded into the character itself for printable keys, because
    /// terminals disagree about whether `Shift+a` arrives as `A` or as
    /// `Shift+Char('a')`. Keeping one representation means a binding written as
    /// `shift+enter` does not also need a `S-enter` alias.
    pub fn from_event(event: KeyEvent) -> Self {
        let mut modifiers = event.modifiers;
        let code = match event.code {
            KeyCode::Char(character) if character.is_ascii_uppercase() => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::Char(character.to_ascii_lowercase())
            }
            other => other,
        };
        Self { code, modifiers }
    }

    /// The chord a key legend is naming.
    ///
    /// A hint is drawn as `key label`, and the key half is written for a human:
    /// `^S` is the terminal's shorthand for `Ctrl+S`, `Space` and `↑` are
    /// spelled out, and `J/K` names the pair a list walks with. A pointer
    /// pressing that legend should press the key it names, so the text is read
    /// back the way the reader reads it.
    ///
    /// A pair names one chord here, and it is the forward one: a caller that
    /// can do better — the modal footer splits `↑↓` into its two cells, so the
    /// arrow under the pointer picks the direction — handles the halves and
    /// never asks this function about the pair.
    ///
    /// `None` is text rather than a key — a footer that says `type` is
    /// describing an editor, not a chord — and such a legend is not clickable.
    pub fn from_hint(text: &str) -> Option<Chord> {
        let text = text.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("type") {
            return None;
        }
        // `^S` is the one shorthand that does not spell its modifier.
        if let Some(rest) = text.strip_prefix('^') {
            let mut characters = rest.chars();
            let character = characters.next()?;
            return characters
                .next()
                .is_none()
                .then(|| Chord::ctrl(character.to_ascii_lowercase()));
        }
        // Shift+Tab is BackTab in the wire vocabulary, whatever the text says.
        if text.eq_ignore_ascii_case("shift+tab") {
            return Some(Chord::plain(KeyCode::BackTab));
        }
        let mut modifiers = KeyModifiers::NONE;
        let mut code = None;
        for part in text.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" => modifiers |= KeyModifiers::ALT,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                // A bare `s` is the letter, never the `Shift` modifier the
                // chord parser also accepts as an abbreviation: `Alt+S` is the
                // schedule key on the queue band, and reading its last part as
                // a modifier would leave it with no key at all.
                part => {
                    code = Some(match part {
                        "↑↓" | "↓" => KeyCode::Down,
                        "↑" => KeyCode::Up,
                        "←→" | "→" => KeyCode::Right,
                        "←" => KeyCode::Left,
                        "j/k" => KeyCode::Char('j'),
                        "k/j" => KeyCode::Char('k'),
                        other => parse_code(other)?,
                    });
                }
            }
        }
        Some(Chord {
            code: code?,
            modifiers,
        })
    }

    /// The key press this chord names.
    ///
    /// A legend is pressed by sending the key it advertises through the same
    /// dispatcher the keyboard uses, which is what keeps a clickable hint from
    /// growing a second command path. [`Self::from_event`] normalises a live
    /// press into this form, so the round trip is exact for everything the
    /// tables hold.
    pub fn event(&self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }

    /// Compact display form, e.g. `Ctrl+P` or `Shift+Enter`.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl".to_string());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt".to_string());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift".to_string());
        }
        parts.push(match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(character) => character.to_ascii_uppercase().to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::BackTab => "Shift+Tab".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Delete => "Del".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::Insert => "Ins".to_string(),
            KeyCode::Null => "Null".to_string(),
            KeyCode::CapsLock => "CapsLock".to_string(),
            KeyCode::ScrollLock => "ScrollLock".to_string(),
            KeyCode::NumLock => "NumLock".to_string(),
            KeyCode::PrintScreen => "PrtSc".to_string(),
            KeyCode::Pause => "Pause".to_string(),
            KeyCode::Menu => "Menu".to_string(),
            KeyCode::KeypadBegin => "KeypadBegin".to_string(),
            KeyCode::Media(media) => format!("{media:?}"),
            KeyCode::Modifier(modifier) => format!("{modifier:?}"),
            KeyCode::F(number) => format!("F{number}"),
        });
        // `BackTab` already reads as Shift+Tab.
        if self.code == KeyCode::BackTab {
            return "Shift+Tab".to_string();
        }
        parts.join("+")
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display())
    }
}

impl FromStr for Chord {
    type Err = KeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut modifiers = KeyModifiers::NONE;
        let mut code: Option<KeyCode> = None;
        for part in value.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => modifiers |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" | "m" => modifiers |= KeyModifiers::ALT,
                "shift" | "s" => modifiers |= KeyModifiers::SHIFT,
                "super" | "cmd" | "win" => modifiers |= KeyModifiers::SUPER,
                other => {
                    if code.is_some() {
                        return Err(KeyParseError::new(value, "more than one key in chord"));
                    }
                    code = Some(
                        parse_code(other)
                            .ok_or_else(|| KeyParseError::new(value, "unknown key name"))?,
                    );
                }
            }
        }
        let Some(code) = code else {
            return Err(KeyParseError::new(value, "no key in chord"));
        };
        // Normalise uppercase letters the same way live events are normalised.
        let (code, modifiers) = match code {
            KeyCode::Char(character) if character.is_ascii_uppercase() => (
                KeyCode::Char(character.to_ascii_lowercase()),
                modifiers - KeyModifiers::SHIFT,
            ),
            other => (other, modifiers),
        };
        Ok(Self { code, modifiers })
    }
}

fn parse_code(name: &str) -> Option<KeyCode> {
    Some(match name {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        other => {
            if let Some(number) = other.strip_prefix('f')
                && let Ok(number) = number.parse::<u8>()
                && (1..=24).contains(&number)
            {
                return Some(KeyCode::F(number));
            }
            let mut characters = other.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            KeyCode::Char(character)
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParseError {
    pub value: String,
    pub reason: &'static str,
}

impl KeyParseError {
    fn new(value: &str, reason: &'static str) -> Self {
        Self {
            value: value.to_string(),
            reason,
        }
    }
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid key {:?}: {}", self.value, self.reason)
    }
}

impl std::error::Error for KeyParseError {}

/// Where a binding is active.
///
/// The group a scope is shown under in the shortcuts cheatsheet.
///
/// Categories exist so the overlay can be read rather than scrolled: a reader
/// looking for "how do I send" looks under Composer, not through forty rows of
/// management keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Global,
    Transcript,
    Composer,
    Modals,
    Workbench,
    Management,
    Panels,
}

impl Category {
    /// Every category, in the order the cheatsheet shows them.
    pub const ALL: [Category; 7] = [
        Category::Global,
        Category::Transcript,
        Category::Composer,
        Category::Modals,
        Category::Workbench,
        Category::Management,
        Category::Panels,
    ];

    /// Stable identifier used as the collapse key.
    pub const fn id(self) -> &'static str {
        match self {
            Category::Global => "global",
            Category::Transcript => "transcript",
            Category::Composer => "composer",
            Category::Modals => "modals",
            Category::Workbench => "workbench",
            Category::Management => "management",
            Category::Panels => "panels",
        }
    }
}

/// Scopes are checked from the most specific to [`Scope::Global`], so a page
/// binding always wins over a global one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Available everywhere, including inside overlays.
    Global,
    /// Active while the composer owns the keyboard.
    Composer,
    /// Active while a modal overlay (help, palette, confirm) is open.
    Overlay,
    /// Active while the runtime switcher is open, on top of `Overlay`.
    Runtime,
    Sessions,
    Agent,
    Management,
    Providers,
    Agents,
    Mcp,
    Skills,
    Prompts,
    Devices,
    Settings,
    Help,
}

impl Scope {
    /// The scope whose bindings are shown for a given page scope.
    pub const ALL: &'static [Scope] = &[
        Scope::Global,
        Scope::Composer,
        Scope::Overlay,
        Scope::Runtime,
        Scope::Sessions,
        Scope::Agent,
        Scope::Management,
        Scope::Providers,
        Scope::Agents,
        Scope::Mcp,
        Scope::Skills,
        Scope::Prompts,
        Scope::Devices,
        Scope::Settings,
        Scope::Help,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Composer => "composer",
            Scope::Overlay => "overlay",
            Scope::Runtime => "runtime",
            Scope::Sessions => "sessions",
            Scope::Agent => "agent",
            Scope::Management => "management",
            Scope::Agents => "agents",
            Scope::Providers => "providers",
            Scope::Mcp => "mcp",
            Scope::Skills => "skills",
            Scope::Prompts => "prompts",
            Scope::Devices => "devices",
            Scope::Settings => "settings",
            Scope::Help => "help",
        }
    }

    /// The category this scope's bindings are listed under.
    pub const fn category(self) -> Category {
        match self {
            Scope::Global => Category::Global,
            Scope::Agent => Category::Transcript,
            Scope::Composer => Category::Composer,
            Scope::Overlay | Scope::Runtime => Category::Modals,
            Scope::Sessions => Category::Workbench,
            Scope::Management
            | Scope::Providers
            | Scope::Agents
            | Scope::Mcp
            | Scope::Skills
            | Scope::Prompts => Category::Management,
            Scope::Devices | Scope::Settings | Scope::Help => Category::Panels,
        }
    }
}

/// One row of a binding table.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub scope: Scope,
    pub chord: Chord,
    pub intent: Intent,
    /// The locale key that names this row, or `None` for a hidden alias:
    /// dispatchable, never advertised.
    ///
    /// It is an accessor rather than a string so the row is drawn in whatever
    /// language the session resolved to. Which words a row uses is still the
    /// table's decision, not the renderer's, which is what lets `Search` on the
    /// runtime switcher be `Filter` on a list without either surface spelling
    /// it out.
    pub label: Option<fn(Strings) -> &'static str>,
}

const fn binding(
    scope: Scope,
    chord: Chord,
    intent: Intent,
    label: fn(Strings) -> &'static str,
) -> Binding {
    Binding {
        scope,
        chord,
        intent,
        label: Some(label),
    }
}

const fn alias(scope: Scope, chord: Chord, intent: Intent) -> Binding {
    Binding {
        scope,
        chord,
        intent,
        label: None,
    }
}

pub use crate::action::Intent;

/// The default binding tables.
///
/// Design notes that are easy to get wrong and are deliberate here:
/// * `Esc` never cancels a running turn — it only walks back one level, because
///   overloading it makes "close this overlay" and "stop the Agent"
///   indistinguishable. `Ctrl+C` owns interrupting.
/// * Vim motions are aliases, not the primary binding, so arrow keys work for
///   everyone and `hjkl` works for people who want it.
/// * Keys that raise or lower privilege (`a`/`d`/`A`) are single characters and
///   therefore always go through an explicit confirmation or a scoped surface.
/// * `Ctrl+C` is the one key that leaves: it interrupts, and with nothing left
///   to cancel its second press is the exit. No second chord quits, so the
///   answer to "how do I get out" is never a choice between two keys.
pub static DEFAULT_BINDINGS: &[Binding] = &[
    // ---- global ---------------------------------------------------------
    binding(
        Scope::Global,
        Chord::ctrl('p'),
        Intent::OpenCommandPalette,
        |strings| strings.row_command_palette(),
    ),
    alias(
        Scope::Global,
        Chord::plain(KeyCode::Char(':')),
        Intent::OpenCommandPalette,
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('?')),
        Intent::ToggleHelp,
        |strings| strings.row_contextual_help(),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::F(1)),
        Intent::OpenSettings,
        |strings| Intent::OpenSettings.label(strings),
    ),
    alias(Scope::Global, Chord::ctrl(','), Intent::OpenSettings),
    // `Quit` has no chord on purpose. `Ctrl+C` already owns leaving — the first
    // press interrupts, and with nothing left to cancel it asks on the status
    // band — so a second key for the same exit was only a way to reach it
    // without the question. The command palette still lists it, and
    // `tui-keys.toml` can still give it a chord back.
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Esc),
        Intent::Back,
        |strings| strings.row_back_close(),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Tab),
        Intent::FocusNext,
        |strings| Intent::FocusNext.label(strings),
    ),
    alias(
        Scope::Global,
        Chord::plain(KeyCode::BackTab),
        Intent::FocusPrevious,
    ),
    binding(
        Scope::Global,
        Chord::ctrl('r'),
        Intent::Refresh,
        |strings| Intent::Refresh.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::ctrl('c'),
        Intent::ContextualCancel,
        |strings| strings.row_cancel_interrupt(),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('1')),
        Intent::GotoSessions,
        |strings| Intent::GotoSessions.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('2')),
        Intent::GotoManagement,
        |strings| Intent::GotoManagement.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('4')),
        Intent::OpenSettings,
        |strings| Intent::OpenSettings.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('5')),
        Intent::ToggleHelp,
        |strings| Intent::ToggleHelp.label(strings),
    ),
    // ---- sessions -------------------------------------------------------
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    alias(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('k')),
        Intent::SelectPrevious,
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    alias(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('j')),
        Intent::SelectNext,
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Enter),
        Intent::OpenSelectedSession,
        |strings| Intent::OpenSelectedSession.label(strings),
    ),
    // The row's own buttons are reached with the arrows that move across it:
    // `→` steps onto the cluster at its first button, `←` at its last, and the
    // two then walk it. `Enter` presses whichever button the cursor reached,
    // and opens the row itself while the cursor is still only on the row.
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Left),
        Intent::SessionControlPrevious,
        |strings| Intent::SessionControlPrevious.label(strings),
    ),
    alias(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('h')),
        Intent::SessionControlPrevious,
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Right),
        Intent::SessionControlNext,
        |strings| Intent::SessionControlNext.label(strings),
    ),
    alias(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('l')),
        Intent::SessionControlNext,
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('n')),
        Intent::NewSession,
        |strings| strings.row_new_session(),
    ),
    // A heading's own verbs live behind one key, the way the desktop keeps them
    // behind its action button: a project and a folder each have four or five,
    // and none of them means anything on a session row.
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('m')),
        Intent::SidebarRowActions,
        |strings| strings.row_heading_actions(),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        |strings| Intent::BeginFilter.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('r')),
        Intent::BeginRenameSession,
        |strings| Intent::BeginRenameSession.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('f')),
        Intent::ForkSession,
        |strings| strings.row_fork_session(),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('a')),
        Intent::ArchiveSession,
        |strings| Intent::ArchiveSession.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('d')),
        Intent::DeleteSession,
        |strings| Intent::DeleteSession.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('e')),
        Intent::ToggleSessionCard,
        |strings| Intent::ToggleSessionCard.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('c')),
        Intent::CollapseSessionCards,
        |strings| Intent::CollapseSessionCards.label(strings),
    ),
    // A branch of delegated sub-sessions folds on one key, the way the heading
    // above it folds on one click; `z` is free here and reads as "fold".
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('z')),
        Intent::ToggleChildBranch,
        |strings| Intent::ToggleChildBranch.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('y')),
        Intent::CopySessionRow,
        |strings| Intent::CopySessionRow.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::ctrl('a'),
        Intent::ToggleShowArchived,
        |strings| strings.row_show_archived(),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('p')),
        Intent::PinSession,
        |strings| Intent::PinSession.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('t')),
        Intent::ToggleAutoContinue,
        |strings| Intent::ToggleAutoContinue.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::new(KeyCode::Up, KeyModifiers::ALT),
        Intent::MoveSessionUp,
        |strings| Intent::MoveSessionUp.label(strings),
    ),
    binding(
        Scope::Sessions,
        Chord::new(KeyCode::Down, KeyModifiers::ALT),
        Intent::MoveSessionDown,
        |strings| Intent::MoveSessionDown.label(strings),
    ),
    // ---- agent (transcript) ---------------------------------------------
    // The session list is a digit in the global table, which the composer
    // takes whenever it owns the keyboard, so the page that has both a
    // composer and a transcript needs the chord on the page itself: with the
    // reader's hand on the transcript rather than in the draft, `Ctrl+L` is
    // the same destination the composer already names.
    alias(Scope::Agent, Chord::ctrl('l'), Intent::GotoSessions),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::PageUp),
        Intent::ScrollPageUp,
        |strings| Intent::ScrollPageUp.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::PageDown),
        Intent::ScrollPageDown,
        |strings| Intent::ScrollPageDown.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('u'),
        Intent::ScrollHalfPageUp,
        |strings| strings.row_half_page_up(),
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('d'),
        Intent::ScrollHalfPageDown,
        |strings| strings.row_half_page_down(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::End),
        Intent::ScrollToBottom,
        |strings| strings.row_bottom_follow(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Home),
        Intent::ScrollToTop,
        |strings| Intent::ScrollToTop.label(strings),
    ),
    // `u` is free in this scope and reads as "up one page of history"; the
    // scroll keys around it are all taken already.
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('u')),
        Intent::LoadOlderHistory,
        |strings| Intent::LoadOlderHistory.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('e')),
        Intent::ToggleBlockExpanded,
        |strings| strings.row_expand_block(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(2)),
        Intent::ToggleAllBlocksExpanded,
        |strings| Intent::ToggleAllBlocksExpanded.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('e'),
        Intent::ToggleReasoningExpanded,
        |strings| strings.row_toggle_reasoning(),
    ),
    // The countdown to a continuation is only visible while the reader is in
    // the session, so the key that stops it has to work there too.
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('t')),
        Intent::ToggleAutoContinue,
        |strings| Intent::ToggleAutoContinue.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('y')),
        Intent::CopyBlockBody,
        |strings| strings.row_copy_block(),
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('y'),
        Intent::CopyBlockMetadata,
        |strings| strings.row_copy_metadata(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Enter),
        Intent::OpenBlockDetails,
        |strings| strings.row_block_details(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('i')),
        Intent::FocusComposer,
        |strings| strings.row_write_a_message(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(6)),
        Intent::ContinueTurn,
        |strings| strings.row_continue_turn(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginTranscriptSearch,
        |strings| Intent::BeginTranscriptSearch.label(strings),
    ),
    // The queue's own keys. `Alt` keeps them out of the way of the transcript,
    // and every one of them no-ops when nothing is queued.
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Up, KeyModifiers::ALT),
        Intent::QueueSelectPrevious,
        |strings| Intent::QueueSelectPrevious.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Down, KeyModifiers::ALT),
        Intent::QueueSelectNext,
        |strings| Intent::QueueSelectNext.label(strings),
    ),
    alias(
        Scope::Agent,
        Chord::new(KeyCode::Char('e'), KeyModifiers::ALT),
        Intent::QueueEditSelected,
    ),
    alias(
        Scope::Agent,
        Chord::new(KeyCode::Char('x'), KeyModifiers::ALT),
        Intent::QueueDeleteSelected,
    ),
    alias(
        Scope::Agent,
        Chord::new(KeyCode::Char('k'), KeyModifiers::ALT),
        Intent::QueueMoveUp,
    ),
    alias(
        Scope::Agent,
        Chord::new(KeyCode::Char('j'), KeyModifiers::ALT),
        Intent::QueueMoveDown,
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Enter, KeyModifiers::ALT),
        Intent::QueueSendNow,
        |strings| Intent::QueueSendNow.label(strings),
    ),
    // The clock button the Desktop puts beside the Composer. `Alt` keeps it out
    // of the way of the transcript, and it no-ops on a page with no draft.
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('s'), KeyModifiers::ALT),
        Intent::ScheduleMessage,
        |strings| strings.row_schedule_send(),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('n')),
        Intent::SearchNext,
        |strings| Intent::SearchNext.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('p')),
        Intent::SearchPrevious,
        |strings| Intent::SearchPrevious.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('d'), KeyModifiers::ALT),
        Intent::ToggleDock,
        |strings| Intent::ToggleDock.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('g'), KeyModifiers::ALT),
        Intent::DockActivate,
        |strings| Intent::DockActivate.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('h'), KeyModifiers::ALT),
        Intent::DockHideDone,
        |strings| Intent::DockHideDone.label(strings),
    ),
    // The ownership tree is walked with the arrows that mean "in" and "back":
    // into the sessions this one delegated, and back to the one that delegated
    // it. Neither chord is claimed by the composer, so both answer wherever the
    // keyboard is on the page.
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Right, KeyModifiers::ALT),
        Intent::OpenChildSessions,
        |strings| Intent::OpenChildSessions.label(strings),
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Left, KeyModifiers::ALT),
        Intent::GotoParentSession,
        |strings| Intent::GotoParentSession.label(strings),
    ),
    // ---- composer -------------------------------------------------------
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Enter),
        Intent::SubmitComposer,
        |strings| Intent::SubmitComposer.label(strings),
    ),
    // Two chords for the one action, and the second is not a convenience: a
    // terminal that does not implement the keyboard protocol sends `Shift+Enter`
    // as the same carriage return `Enter` sends, so the chord cannot arrive
    // there at all. `Ctrl+J` is a line feed — a byte of its own in every
    // terminal, and what a terminal's own key mapping is usually told to send
    // when the reader rebinds `Shift+Enter` outside the client.
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Enter, KeyModifiers::SHIFT),
        Intent::InsertNewline,
        |strings| Intent::InsertNewline.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('j'),
        Intent::InsertNewline,
        |strings| strings.row_newline_any_terminal(),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('m'),
        Intent::ToggleMultiline,
        |strings| strings.row_multiline_mode(),
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Up),
        Intent::ComposerHistoryPrevious,
        |strings| strings.row_previous_message(),
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Down),
        Intent::ComposerHistoryNext,
        |strings| strings.row_next_message(),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('o'),
        Intent::EditComposerExternally,
        |strings| strings.row_edit_in_editor(),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('b'),
        Intent::BackgroundRunningCommand,
        |strings| strings.row_send_to_background(),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('s'),
        Intent::SteerRunningTurn,
        |strings| strings.row_steer_running_turn(),
    ),
    // The completion drawer is walked with the arrow keys while it is open;
    // these two are the explicit chords, and they deliberately stay off
    // `Ctrl+N`/`Ctrl+P` so the global command palette keeps working while the
    // composer owns the keyboard. `←`/`→` switch the drawer's panels and are
    // read before this table, so they are not bindings of their own.
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Up, KeyModifiers::ALT),
        Intent::CompletionPrevious,
        |strings| strings.row_previous_completion(),
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Down, KeyModifiers::ALT),
        Intent::CompletionNext,
        |strings| strings.row_next_completion(),
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Tab),
        Intent::CompletionAccept,
        |strings| strings.row_accept_completion(),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('w'),
        Intent::DeleteWordBefore,
        |strings| Intent::DeleteWordBefore.label(strings),
    ),
    alias(
        Scope::Composer,
        Chord::new(KeyCode::Backspace, KeyModifiers::ALT),
        Intent::DeleteWordBackward,
    ),
    alias(
        Scope::Composer,
        Chord::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        Intent::DeleteWordBackward,
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('d'), KeyModifiers::ALT),
        Intent::DeleteWordAfter,
        |strings| Intent::DeleteWordAfter.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('k'),
        Intent::KillToLineEnd,
        |strings| Intent::KillToLineEnd.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('u'),
        Intent::KillToLineStart,
        |strings| Intent::KillToLineStart.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('y'),
        Intent::YankKill,
        |strings| Intent::YankKill.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('z'),
        Intent::ComposerUndo,
        |strings| Intent::ComposerUndo.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('z'), KeyModifiers::ALT),
        Intent::ComposerRedo,
        |strings| Intent::ComposerRedo.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('b'), KeyModifiers::ALT),
        Intent::ComposerWordLeft,
        |strings| Intent::ComposerWordLeft.label(strings),
    ),
    alias(
        Scope::Composer,
        Chord::new(KeyCode::Left, KeyModifiers::CONTROL),
        Intent::ComposerWordLeft,
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('f'), KeyModifiers::ALT),
        Intent::ComposerWordRight,
        |strings| Intent::ComposerWordRight.label(strings),
    ),
    alias(
        Scope::Composer,
        Chord::new(KeyCode::Right, KeyModifiers::CONTROL),
        Intent::ComposerWordRight,
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('a'),
        Intent::ComposerLineStart,
        |strings| Intent::ComposerLineStart.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('e'),
        Intent::ComposerLineEnd,
        |strings| Intent::ComposerLineEnd.label(strings),
    ),
    // `Ctrl+A` already owns line-start in this scope, so the whole-draft
    // selection gets the Alt layer, which is where the composer keeps its
    // larger edits (kill, redo, word motion).
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('i'), KeyModifiers::ALT),
        Intent::AttachImage,
        |strings| Intent::AttachImage.label(strings),
    ),
    // The reader's own paste gesture. A terminal in bracketed-paste mode never
    // sends this key — it pastes the text itself — so the binding only fires on
    // the terminals that forward it, where a clipboard holding an image would
    // otherwise paste nothing at all.
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        Intent::PasteClipboard,
        |strings| Intent::PasteClipboard.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('a'), KeyModifiers::ALT),
        Intent::SelectAllDraft,
        |strings| Intent::SelectAllDraft.label(strings),
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('c'), KeyModifiers::ALT),
        Intent::CopyDraftSelection,
        |strings| Intent::CopyDraftSelection.label(strings),
    ),
    // The global navigation keys are single characters, so a page whose
    // composer owns the keyboard cannot reach the session list with them —
    // typing `1` is the digit the reader meant. This is the same destination
    // on a chord the composer can afford to give up, and it is what the
    // composing page's top-right entry names.
    binding(
        Scope::Composer,
        Chord::ctrl('l'),
        Intent::GotoSessions,
        |strings| Intent::GotoSessions.label(strings),
    ),
    // ---- overlays -------------------------------------------------------
    // Climbing out of a directory is part of browsing it, so the picker keeps
    // the key the workspace list uses rather than inventing a second one.
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('u')),
        Intent::WorkspaceBrowseUp,
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Esc),
        Intent::CloseOverlay,
        |strings| Intent::CloseOverlay.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Enter),
        Intent::ConfirmOverlay,
        |strings| Intent::ConfirmOverlay.label(strings),
    ),
    // The configuration form's one write key. It is an overlay-wide chord
    // because `Ctrl+S` means "save" in every form this client will grow.
    binding(
        Scope::Overlay,
        Chord::ctrl('s'),
        Intent::ConfigFormSave,
        |strings| Intent::ConfigFormSave.label(strings),
    ),
    // Voiding a deadline is not deleting the message, so it is a keystroke of
    // its own rather than the overlay's destructive one.
    binding(
        Scope::Overlay,
        Chord::ctrl('x'),
        Intent::ScheduleClear,
        |strings| strings.row_clear_schedule(),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Tab),
        Intent::OverlayNextField,
        |strings| Intent::OverlayNextField.label(strings),
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::BackTab),
        Intent::OverlayPreviousField,
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    // `j`/`k` are the list keys every other surface in this client answers to,
    // and the switcher is a list: an overlay that types takes its characters
    // before the table, so these only ever reach an overlay that wants them.
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('k')),
        Intent::SelectPrevious,
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('j')),
        Intent::SelectNext,
    ),
    // Walking a long catalogue: the arrows move one row, these move a screen or
    // an end of it.
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::PageUp),
        Intent::ScrollPageUp,
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::PageDown),
        Intent::ScrollPageDown,
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Home),
        Intent::ScrollToTop,
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::End),
        Intent::ScrollToBottom,
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    // ---- the runtime switcher --------------------------------------------
    // A scope of its own because the overlay scope already spends `←`/`→` on
    // the elicitation form and `a` on approving an approval card, and a key
    // that means "fold this group" must not also mean "approve" depending on
    // which modal happens to be up.
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Left),
        Intent::OverlayFoldClosed,
        |strings| Intent::OverlayFoldClosed.label(strings),
    ),
    alias(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('h')),
        Intent::OverlayFoldClosed,
    ),
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Right),
        Intent::OverlayFoldOpen,
        |strings| Intent::OverlayFoldOpen.label(strings),
    ),
    alias(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('l')),
        Intent::OverlayFoldOpen,
    ),
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        |strings| strings.row_search(),
    ),
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('*')),
        Intent::StarRuntimeModel,
        |strings| Intent::StarRuntimeModel.label(strings),
    ),
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('a')),
        Intent::ManageRuntimeAccount,
        |strings| Intent::ManageRuntimeAccount.label(strings),
    ),
    binding(
        Scope::Runtime,
        Chord::plain(KeyCode::Char('r')),
        Intent::ResetRunOption,
        |strings| Intent::ResetRunOption.label(strings),
    ),
    // ---- the configuration centre ----------------------------------------
    // One scope for the whole centre: the module tabs, the area rail, the
    // record table and the detail pane are four zones of one surface, so the
    // keys that move between them are declared once rather than per page.
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('k')),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('j')),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('h')),
        Intent::ConfigPanePrevious,
        |strings| Intent::ConfigPanePrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('l')),
        Intent::ConfigPaneNext,
        |strings| Intent::ConfigPaneNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Left),
        Intent::ConfigPanePrevious,
        |strings| Intent::ConfigPanePrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Right),
        Intent::ConfigPaneNext,
        |strings| Intent::ConfigPaneNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Tab),
        Intent::ConfigSectionNext,
        |strings| Intent::ConfigSectionNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::BackTab),
        Intent::ConfigSectionPrevious,
        |strings| Intent::ConfigSectionPrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Enter),
        Intent::ConfigEntryActions,
        |strings| Intent::ConfigEntryActions.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleSelectedEntry,
        |strings| strings.row_enable_disable(),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedEntry,
        |strings| Intent::EditSelectedEntry.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('n')),
        Intent::ConfigEntryCreate,
        |strings| Intent::ConfigEntryCreate.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('d')),
        Intent::ConfigEntryDelete,
        |strings| Intent::ConfigEntryDelete.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('r')),
        Intent::ReloadManagement,
        |strings| Intent::ReloadManagement.label(strings),
    ),
    // The registry's own controls. `t` and `s` cycle the two narrowings a
    // registry offers — a transport family for MCP, a ranking for Skills — and
    // the page keys walk the pages of whichever one is on screen. They are
    // letters rather than chords because a reader browsing a catalogue presses
    // them repeatedly, and `,`/`.` are the pair the rest of this client already
    // uses for "the one before" and "the one after".
    // `c` copies the line a form is about to write, which is the one thing on
    // it the reader cannot select with the pointer. It is only the copy while
    // no field is being typed into: inside an edit it is the letter.
    // `Ctrl+Q` throws a half-written form away. It is behind a confirmation, so
    // it is a chord rather than a letter: no reader should lose a form to a key
    // they pressed while reaching for something else.
    binding(
        Scope::Overlay,
        Chord::ctrl('q'),
        Intent::ConfigFormDiscard,
        |strings| Intent::ConfigFormDiscard.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('c')),
        Intent::ConfigFormCopy,
        |strings| Intent::ConfigFormCopy.label(strings),
    ),
    // `m` opens the Models of the highlighted profile. It is the one thing in
    // the centre that is a list inside a record rather than a record of its own.
    // `i` reads what other CLI tools already hold. The Providers area is where
    // a profile comes from, so the verb lives on the same surface.
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('i')),
        Intent::ConfigImport,
        |strings| Intent::ConfigImport.label(strings),
    ),
    // `Ctrl+F` probes the machine for Agent CLIs that have not been added.
    binding(
        Scope::Management,
        Chord::ctrl('f'),
        Intent::ConfigDiscover,
        |strings| Intent::ConfigDiscover.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('x')),
        Intent::ConfigExport,
        |strings| Intent::ConfigExport.label(strings),
    ),
    // The export picker's own keys. They avoid the chords the approval and
    // elicitation cards already own — a chord dispatches one intent, so sharing
    // one would silently disable a surface — and they are letters rather than
    // chords because a reader cycling targets presses them repeatedly.
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('t')),
        Intent::ConfigExportSource,
        |strings| Intent::ConfigExportSource.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('m')),
        Intent::ConfigExportMode,
        |strings| Intent::ConfigExportMode.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('v')),
        Intent::ConfigExportPreview,
        |strings| Intent::ConfigExportPreview.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('w')),
        Intent::ConfigExportApply,
        |strings| Intent::ConfigExportApply.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('b')),
        Intent::ConfigExportRollback,
        |strings| Intent::ConfigExportRollback.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('r')),
        Intent::ConfigImportRefresh,
        |strings| Intent::ConfigImportRefresh.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('m')),
        Intent::ConfigModels,
        |strings| Intent::ConfigModels.label(strings),
    ),
    // The Model editor's own keys. They are not `Space`, `a` and `d`: those
    // chords already belong to the approval card and to the elicitation card,
    // and a chord dispatches one intent, so sharing them would silently disable
    // one of the two surfaces.
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('o')),
        Intent::ConfigModelToggle,
        |strings| Intent::ConfigModelToggle.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('i')),
        Intent::ConfigModelAdd,
        |strings| Intent::ConfigModelAdd.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('x')),
        Intent::ConfigModelRemove,
        |strings| Intent::ConfigModelRemove.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('f')),
        Intent::ConfigModelFetch,
        |strings| Intent::ConfigModelFetch.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('e')),
        Intent::ConfigModelEdit,
        |strings| Intent::ConfigModelEdit.label(strings),
    ),
    // The whole list at once. The desktop offers the same three (a select-all
    // beside these two); selecting and enabling are one press here because a
    // Model the profile does not hold has nothing to enable.
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('y')),
        Intent::ConfigModelEnableAll,
        |strings| Intent::ConfigModelEnableAll.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('z')),
        Intent::ConfigModelDisableAll,
        |strings| Intent::ConfigModelDisableAll.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('t')),
        Intent::ConfigRegistryFilter,
        |strings| Intent::ConfigRegistryFilter.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('s')),
        Intent::ConfigRegistrySort,
        |strings| Intent::ConfigRegistrySort.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char(',')),
        Intent::ConfigRegistryPagePrevious,
        |strings| Intent::ConfigRegistryPagePrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('.')),
        Intent::ConfigRegistryPageNext,
        |strings| Intent::ConfigRegistryPageNext.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('[')),
        Intent::ConfigModulePrevious,
        |strings| Intent::ConfigModulePrevious.label(strings),
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char(']')),
        Intent::ConfigModuleNext,
        |strings| Intent::ConfigModuleNext.label(strings),
    ),
    // ---- devices ---------------------------------------------------------
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    // The two halves of the page are one press apart, the way the
    // configuration centre's modules are: the tabs are a strip, not a pane the
    // reader has to walk into.
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Left),
        Intent::DevicesTabPrevious,
        |strings| Intent::DevicesTabPrevious.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Right),
        Intent::DevicesTabNext,
        |strings| Intent::DevicesTabNext.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Enter),
        Intent::ActivateDevicePairing,
        |strings| Intent::ActivateDevicePairing.label(strings),
    ),
    // The route the code carries is the one thing on this page the reader can
    // change without the runtime: the transport lives in the link, so moving it
    // is a re-encode rather than a new offer.
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('[')),
        Intent::PairingRoutePrevious,
        |strings| Intent::PairingRoutePrevious.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char(']')),
        Intent::PairingRouteNext,
        |strings| Intent::PairingRouteNext.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('a')),
        Intent::AllowLanPairing,
        |strings| Intent::AllowLanPairing.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('r')),
        Intent::RefuseLanPairing,
        |strings| Intent::RefuseLanPairing.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('x')),
        Intent::RevokeSelectedDevice,
        |strings| Intent::RevokeSelectedDevice.label(strings),
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('u')),
        Intent::OpenDeviceAudit,
        |strings| strings.row_audit_log(),
    ),
    // ---- settings --------------------------------------------------------
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Enter),
        Intent::ActivateSetting,
        |strings| Intent::ActivateSetting.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Left),
        Intent::SettingPrevious,
        |strings| Intent::SettingPrevious.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Right),
        Intent::SettingNext,
        |strings| Intent::SettingNext.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        |strings| Intent::BeginFilter.label(strings),
    ),
    alias(
        Scope::Settings,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ActivateSetting,
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Char('d')),
        Intent::ResetSetting,
        |strings| Intent::ResetSetting.label(strings),
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::F(9)),
        Intent::ReloadKeymap,
        |strings| strings.row_reload_key_bindings(),
    ),
    // ---- help ------------------------------------------------------------
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        |strings| Intent::SelectPrevious.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        |strings| Intent::SelectNext.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::PageUp),
        Intent::ScrollPageUp,
        |strings| Intent::ScrollPageUp.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::PageDown),
        Intent::ScrollPageDown,
        |strings| Intent::ScrollPageDown.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Home),
        Intent::ScrollToTop,
        |strings| Intent::ScrollToTop.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::End),
        Intent::ScrollToBottom,
        |strings| Intent::ScrollToBottom.label(strings),
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        |strings| strings.row_search_help(),
    ),
    // ---- shared intents that also need a visible home --------------------
    binding(
        Scope::Global,
        Chord::ctrl('t'),
        Intent::TakeOverHome,
        |strings| Intent::TakeOverHome.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::ctrl('b'),
        Intent::ToggleSidebar,
        |strings| Intent::ToggleSidebar.label(strings),
    ),
    alias(
        Scope::Global,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ShowDetails,
    ),
    binding(
        Scope::Global,
        Chord::ctrl('w'),
        Intent::SwitchWorkspace,
        |strings| Intent::SwitchWorkspace.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::ctrl('g'),
        Intent::SwitchAgentRuntime,
        |strings| Intent::SwitchAgentRuntime.label(strings),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::F(5)),
        Intent::ProbeAgentRuntime,
        |strings| strings.row_probe_runtimes(),
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Delete),
        Intent::ClearFilter,
        |strings| Intent::ClearFilter.label(strings),
    ),
    // ---- workspace browser -----------------------------------------------
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('b')),
        Intent::OpenWorkspaceBrowser,
        |strings| strings.row_browse_dirs(),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('u')),
        Intent::WorkspaceBrowseUp,
        |strings| strings.row_parent_dir(),
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char(' ')),
        Intent::WorkspaceBrowseSelect,
        |strings| strings.row_use_dir(),
    ),
    // ---- approval card and elicitation form ------------------------------
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('a')),
        Intent::ApprovalApprove,
        |strings| Intent::ApprovalApprove.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('d')),
        Intent::ApprovalDeny,
        |strings| Intent::ApprovalDeny.label(strings),
    ),
    // The elicitation form types into its answer fields, so `d` is a letter
    // there while one is open for typing. Refusing the question is a chord no
    // field can swallow, which is what keeps Decline reachable from every
    // field rather than only from the ones that are lists.
    alias(Scope::Overlay, Chord::ctrl('d'), Intent::ApprovalDeny),
    binding(
        Scope::Overlay,
        Chord::ctrl('a'),
        Intent::ApprovalAlways,
        |strings| Intent::ApprovalAlways.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('n')),
        Intent::ApprovalFocusNext,
        |strings| Intent::ApprovalFocusNext.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('s')),
        Intent::ElicitationSubmit,
        |strings| strings.row_submit_answers(),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Left),
        Intent::ElicitationFieldPrevious,
        |strings| strings.row_prev_question(),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Right),
        Intent::ElicitationFieldNext,
        |strings| strings.row_next_question(),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char(' ')),
        Intent::OverlayToggleValue,
        |strings| Intent::OverlayToggleValue.label(strings),
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('p')),
        Intent::PaletteRun,
        |strings| strings.row_run_command(),
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('g')),
        Intent::CompletionCancel,
    ),
    alias(
        Scope::Management,
        Chord::ctrl('r'),
        Intent::ReloadManagement,
    ),
];

/// The mutable binding set: defaults plus optional user overrides.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
    /// Intents whose chord the user replaced, kept so conflict reporting can
    /// name both sides.
    pub overrides: BTreeMap<Intent, Chord>,
    /// Problems found while loading the user file. Surfaced in settings rather
    /// than failing startup.
    pub warnings: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::built_in()
    }
}

impl Keymap {
    pub fn built_in() -> Self {
        Self {
            bindings: DEFAULT_BINDINGS.to_vec(),
            overrides: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The user key file location, honouring `VIBEX_HOME`.
    pub fn user_path() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_KEYS")
            && !explicit.trim().is_empty()
        {
            return Some(PathBuf::from(explicit));
        }
        let home = std::env::var("VIBEX_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .or_else(crate::user_home::default_vibex_home)?;
        Some(home.join("tui-keys.toml"))
    }

    /// Load overrides from `~/.vibex/tui-keys.toml`.
    ///
    /// A malformed file never prevents startup: problems become warnings and
    /// the affected binding keeps its default.
    pub fn load(path: &Path) -> Self {
        let mut keymap = Self::built_in();
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return keymap,
            Err(error) => {
                keymap.warnings.push(format!("{}: {error}", path.display()));
                return keymap;
            }
        };
        let (parsed, parse_warnings) = toml_lite::parse(&raw);
        keymap.warnings.extend(parse_warnings);
        for (name, value) in parsed {
            let Some(intent) = Intent::from_id(&name) else {
                keymap.warnings.push(format!("unknown action `{name}`"));
                continue;
            };
            let chord = match value.parse::<Chord>() {
                Ok(chord) => chord,
                Err(error) => {
                    keymap.warnings.push(format!("{name}: {error}"));
                    continue;
                }
            };
            keymap.rebind(intent, chord);
        }
        keymap
    }

    /// Replace every binding of `intent` with `chord`.
    pub fn rebind(&mut self, intent: Intent, chord: Chord) {
        let mut replaced = false;
        for binding in &mut self.bindings {
            if binding.intent == intent {
                if replaced {
                    // Collapse extra aliases so a remap really is one key.
                    binding.label = None;
                    binding.chord = chord;
                } else {
                    binding.chord = chord;
                    replaced = true;
                }
            }
        }
        if !replaced {
            self.bindings.push(Binding {
                scope: intent.default_scope(),
                chord,
                intent,
                label: Some(intent.label_fn()),
            });
        }
        self.overrides.insert(intent, chord);
        self.warnings
            .retain(|warning| !warning.starts_with("conflict"));
        self.report_conflicts();
    }

    fn report_conflicts(&mut self) {
        let mut seen = HashMap::<(Scope, Chord), Intent>::new();
        let mut conflicts = Vec::new();
        for binding in &self.bindings {
            if binding.label.is_none() {
                continue;
            }
            if let Some(existing) = seen.insert((binding.scope, binding.chord), binding.intent)
                && existing != binding.intent
            {
                conflicts.push(format!(
                    "conflict: {} ({}) is bound to both `{}` and `{}`",
                    binding.chord,
                    binding.scope.id(),
                    existing.id(),
                    binding.intent.id()
                ));
            }
        }
        self.warnings.extend(conflicts);
    }

    /// Resolve a key press against the ordered scope list.
    ///
    /// `scopes` is consulted front to back, so callers pass the most specific
    /// scope first. An overlay scope usually means only [`Scope::Overlay`] plus
    /// [`Scope::Global`] are eligible; that filtering is the caller's job.
    pub fn resolve(&self, scopes: &[Scope], chord: Chord) -> Option<Intent> {
        for scope in scopes {
            if let Some(binding) = self
                .bindings
                .iter()
                .find(|binding| binding.scope == *scope && binding.chord == chord)
            {
                return Some(binding.intent);
            }
        }
        None
    }

    /// Every binding that should be advertised for a scope, in table order.
    ///
    /// Table order is the order the bindings were declared in, which is *not*
    /// the order they answer in: `1` is the session list in [`Scope::Global`]
    /// and `Ctrl+L` is the same destination in [`Scope::Composer`]. A surface
    /// that names a key has to check each row against [`Self::resolve`] — which
    /// is what [`crate::app::App::advertised_keys`] does — or it will spell the
    /// row that lost.
    pub fn advertised(&self, scopes: &[Scope]) -> Vec<&Binding> {
        self.bindings
            .iter()
            .filter(|binding| binding.label.is_some() && scopes.contains(&binding.scope))
            .collect()
    }

    /// Chord to show for an intent, if it has one.
    pub fn chord_for(&self, intent: Intent) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|binding| binding.intent == intent)
            .map(|binding| binding.chord)
    }

    /// Chord to show for an intent in one scope, if that scope binds it.
    ///
    /// A single intent can be bound more than once — the session list is one
    /// key in the global table and another while the composer owns the
    /// keyboard — and a surface that names the chord has to name the one that
    /// will answer *there*, not the first one in the table.
    pub fn chord_for_in(&self, scope: Scope, intent: Intent) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|binding| binding.scope == scope && binding.intent == intent)
            .map(|binding| binding.chord)
    }

    /// Whether the user has moved this intent off its default chord.
    pub fn is_overridden(&self, intent: Intent) -> bool {
        self.overrides.contains_key(&intent)
    }

    /// The chord this intent is bound to out of the box, if it has one.
    pub fn default_chord(intent: Intent) -> Option<Chord> {
        DEFAULT_BINDINGS
            .iter()
            .find(|binding| binding.intent == intent)
            .map(|binding| binding.chord)
    }

    /// Put one intent back on its default chord.
    ///
    /// Rebuilding from the defaults and re-applying the remaining overrides is
    /// what restores aliases too: a rebind collapses them, and there is no
    /// information left in the live table to un-collapse them one by one.
    pub fn reset(&mut self, intent: Intent) {
        if self.overrides.remove(&intent).is_none() {
            return;
        }
        let remaining = self.overrides.clone();
        *self = Self::built_in();
        for (intent, chord) in remaining {
            self.rebind(intent, chord);
        }
    }

    /// Write the overrides to `path`, creating the directory when it is absent.
    ///
    /// Every failure is reported as a string rather than an error type: the
    /// caller shows it and keeps the in-memory keymap, so a read-only home
    /// directory costs a message, not the edit.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let overrides = self
            .overrides
            .iter()
            .map(|(intent, chord)| (intent.id().to_string(), chord.display()))
            .collect::<BTreeMap<_, _>>();
        std::fs::write(path, toml_lite::render(&overrides))
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Whether a chord is already taken in `scope` by a different intent.
    pub fn conflict(&self, scope: Scope, chord: Chord, intent: Intent) -> Option<Intent> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.scope == scope && binding.chord == chord && binding.intent != intent
            })
            .map(|binding| binding.intent)
    }
}

/// The subset of TOML the key file needs: `action = "chord"` lines.
///
/// Hand-rolled rather than pulling in a TOML parser: the file's grammar is one
/// assignment per line, and adding a dependency to the TUI's closure for that
/// would be out of proportion.
pub mod toml_lite {
    use std::collections::BTreeMap;

    /// Parse `action = "chord"` lines, collecting per-line problems instead of
    /// aborting. A typo in one binding must not cost the user the other twenty.
    pub fn parse(input: &str) -> (BTreeMap<String, String>, Vec<String>) {
        let mut out = BTreeMap::new();
        let mut warnings = Vec::new();
        for (index, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(format!("line {}: expected `action = \"chord\"`", index + 1));
                continue;
            };
            let key = key.trim().trim_matches('"').trim().to_string();
            let value = value
                .trim()
                .trim_matches(|character| character == '"' || character == '\'')
                .trim()
                .to_string();
            if key.is_empty() || value.is_empty() {
                warnings.push(format!("line {}: empty action or chord", index + 1));
                continue;
            }
            out.insert(key, value);
        }
        (out, warnings)
    }

    /// Serialise overrides back into the file the parser reads.
    ///
    /// The header is written every time so a file the editor created explains
    /// itself; the actions are sorted by the map, which keeps the diff of two
    /// saves to the line that actually changed.
    pub fn render(overrides: &BTreeMap<String, String>) -> String {
        let mut out = String::from(
            "# Vibex TUI key bindings.\n\
             # Each line moves one action off its default chord:\n\
             #   action_id = \"Ctrl+P\"\n\
             # Delete a line to go back to the default. F9 reloads this file.\n",
        );
        for (action, chord) in overrides {
            out.push_str(&format!("{action} = \"{chord}\"\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn chords_round_trip_through_their_text_form() {
        for chord in [
            Chord::ctrl('p'),
            Chord::plain(KeyCode::Enter),
            Chord::new(KeyCode::Enter, KeyModifiers::SHIFT),
            Chord::plain(KeyCode::F(1)),
            Chord::plain(KeyCode::PageUp),
            Chord::plain(KeyCode::Char('a')),
        ] {
            let text = chord.display();
            let parsed: Chord = text.parse().unwrap();
            assert_eq!(parsed, chord, "{text} did not round-trip");
        }
    }

    #[test]
    fn shift_is_folded_into_uppercase_characters() {
        let upper = Chord::from_event(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(upper.code, KeyCode::Char('a'));
        assert_eq!(upper.modifiers, KeyModifiers::NONE);
        // ...but Shift+Enter keeps its modifier, because there is no uppercase
        // Enter to fold it into.
        let enter = Chord::from_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(enter.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn every_intent_has_a_way_in_and_an_id() {
        // A chord or a palette command: the two producers a reader drives. An
        // intent with neither is one nobody can reach, which is how an action
        // ends up implemented and never run.
        for intent in Intent::ALL {
            let bound = DEFAULT_BINDINGS
                .iter()
                .any(|binding| binding.intent == *intent);
            let listed = crate::view::PALETTE
                .iter()
                .any(|entry| entry.intent == *intent);
            assert!(bound || listed, "{} has no way in", intent.id());
            assert_eq!(Intent::from_id(intent.id()), Some(*intent));
        }
    }

    #[test]
    fn every_advertised_row_is_named_in_every_language() {
        // The key bar, the cheatsheet and the key editor read a binding's name
        // through its locale accessor, so a row whose accessor answers with an
        // empty string would draw a chord with nothing beside it. The three
        // languages are written in one table and reached by name, which is what
        // makes a missing translation a compile error; this pins that every row
        // has a real name in each of them.
        for binding in DEFAULT_BINDINGS {
            let Some(label) = binding.label else { continue };
            for locale in [
                crate::locale::Locale::En,
                crate::locale::Locale::ZhCn,
                crate::locale::Locale::ZhTw,
            ] {
                let strings = Strings::for_locale(locale);
                let name = label(strings);
                assert!(
                    !name.is_empty(),
                    "{} {} has no name in {locale:?}",
                    binding.scope.id(),
                    binding.chord
                );
            }
        }
    }

    #[test]
    fn a_row_that_needs_its_own_words_keeps_them() {
        // Most rows borrow the action's name, but a handful are bound in two
        // scopes where the action means something different — `Ctrl+/` filters a
        // list and searches the runtime switcher. Those carry a row accessor of
        // their own, and it has to differ from the action's default or the
        // distinction the row draws is lost.
        let search = DEFAULT_BINDINGS
            .iter()
            .find(|binding| {
                binding.scope == Scope::Runtime && binding.intent == Intent::BeginFilter
            })
            .and_then(|binding| binding.label)
            .expect("the runtime switcher searches");
        let en = Strings::for_locale(crate::locale::Locale::En);
        assert_eq!(search(en), "Search");
        assert_ne!(search(en), Intent::BeginFilter.label(en));
    }

    #[test]
    fn advertised_bindings_are_unique_per_scope_and_chord() {
        let keymap = Keymap::built_in();
        let mut seen = HashMap::new();
        for binding in DEFAULT_BINDINGS {
            let Some(_) = binding.label else { continue };
            if let Some(previous) = seen.insert((binding.scope, binding.chord), binding.intent) {
                assert_eq!(
                    previous,
                    binding.intent,
                    "{} {} is advertised twice",
                    binding.scope.id(),
                    binding.chord
                );
            }
        }
        assert!(keymap.warnings.is_empty(), "{:?}", keymap.warnings);
    }

    /// The row controls' two actions answer on the letters beside them, not on
    /// a chord the reader has to be told: `r` renames and `d` deletes the row
    /// the cursor is on.
    #[test]
    fn the_session_list_renames_on_r_and_deletes_on_d() {
        let keymap = Keymap::built_in();
        assert_eq!(
            keymap.resolve(&[Scope::Sessions], Chord::plain(KeyCode::Char('r'))),
            Some(Intent::BeginRenameSession)
        );
        assert_eq!(
            keymap.resolve(&[Scope::Sessions], Chord::plain(KeyCode::Char('d'))),
            Some(Intent::DeleteSession)
        );
        // And the key the band advertises is the key that answers.
        assert_eq!(
            keymap.chord_for_in(Scope::Sessions, Intent::DeleteSession),
            Some(Chord::plain(KeyCode::Char('d')))
        );
    }

    #[test]
    fn hidden_aliases_still_resolve() {
        let keymap = Keymap::built_in();
        // `k` is an alias for SelectPrevious and must dispatch even though it
        // is not advertised.
        let resolved = keymap.resolve(&[Scope::Sessions], Chord::plain(KeyCode::Char('k')));
        assert_eq!(resolved, Some(Intent::SelectPrevious));
        let advertised = keymap.advertised(&[Scope::Sessions]);
        assert!(
            !advertised
                .iter()
                .any(|binding| binding.chord == Chord::plain(KeyCode::Char('k')))
        );
    }

    #[test]
    fn scope_order_decides_which_binding_wins() {
        let keymap = Keymap::built_in();
        // Enter is "send" in the composer and "open" in a list.
        assert_eq!(
            keymap.resolve(
                &[Scope::Composer, Scope::Agent],
                Chord::plain(KeyCode::Enter)
            ),
            Some(Intent::SubmitComposer)
        );
        assert_eq!(
            keymap.resolve(
                &[Scope::Sessions, Scope::Global],
                Chord::plain(KeyCode::Enter)
            ),
            Some(Intent::OpenSelectedSession)
        );
    }

    #[test]
    fn esc_never_interrupts_a_running_turn() {
        let keymap = Keymap::built_in();
        assert_eq!(
            keymap.resolve(&[Scope::Agent, Scope::Global], Chord::plain(KeyCode::Esc)),
            Some(Intent::Back)
        );
        assert_eq!(
            keymap.resolve(&[Scope::Agent, Scope::Global], Chord::ctrl('c')),
            Some(Intent::ContextualCancel)
        );
    }

    #[test]
    fn the_schedule_key_reaches_the_page_while_the_composer_types() {
        // The composer's scope is consulted before the page's, and it binds
        // `Alt` chords of its own: a schedule key the composer swallowed would
        // be one the reader could not press while writing the message they are
        // scheduling.
        let keymap = Keymap::built_in();
        let chord = Chord::new(KeyCode::Char('s'), KeyModifiers::ALT);
        assert_eq!(
            keymap.resolve(&[Scope::Composer, Scope::Agent, Scope::Global], chord),
            Some(Intent::ScheduleMessage)
        );
        // And voiding a deadline is the overlay's, so it only answers where the
        // editor that owns it is open.
        assert_eq!(
            keymap.resolve(&[Scope::Overlay, Scope::Global], Chord::ctrl('x')),
            Some(Intent::ScheduleClear)
        );
        assert_eq!(
            keymap.resolve(&[Scope::Agent, Scope::Global], Chord::ctrl('x')),
            None,
            "the overlay's key answered on a page"
        );
    }

    #[test]
    fn a_chord_is_named_for_the_scope_that_answers_it() {
        let keymap = Keymap::built_in();
        // The session list is `1` in the global table and `Ctrl+L` while the
        // composer owns the keyboard, so a surface that names the chord has to
        // say which of the two answers where it is drawn.
        assert_eq!(
            keymap.chord_for(Intent::GotoSessions),
            Some(Chord::plain(KeyCode::Char('1')))
        );
        assert_eq!(
            keymap.chord_for_in(Scope::Composer, Intent::GotoSessions),
            Some(Chord::ctrl('l'))
        );
        assert_eq!(
            keymap.chord_for_in(Scope::Sessions, Intent::GotoSessions),
            None,
            "the list's own scope does not rebind a global key"
        );
        // The page that owns a transcript answers it too, so the chord works
        // with the reader's hand outside the draft: the composer's copy of the
        // binding is only reachable while the composer has the keyboard.
        assert_eq!(
            keymap.resolve(&[Scope::Agent, Scope::Global], Chord::ctrl('l')),
            Some(Intent::GotoSessions)
        );
        // And it stays off the bar, where the global digit already names the
        // destination: `advertised` walks the table, and the digit is older.
        assert_eq!(
            keymap.chord_for(Intent::GotoSessions),
            Some(Chord::plain(KeyCode::Char('1')))
        );
    }

    #[test]
    fn rebinding_replaces_the_chord_and_reports_conflicts() {
        let mut keymap = Keymap::built_in();
        keymap.rebind(Intent::NewSession, Chord::plain(KeyCode::F(5)));
        assert_eq!(
            keymap.resolve(&[Scope::Sessions], Chord::plain(KeyCode::F(5))),
            Some(Intent::NewSession)
        );
        assert_eq!(
            keymap.resolve(&[Scope::Sessions], Chord::plain(KeyCode::Char('n'))),
            None
        );

        keymap.rebind(Intent::NewSession, Chord::plain(KeyCode::Char('r')));
        assert!(
            keymap
                .warnings
                .iter()
                .any(|warning| warning.starts_with("conflict")),
            "{:?}",
            keymap.warnings
        );
    }

    #[test]
    fn a_broken_user_file_yields_warnings_not_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-keys.toml");
        std::fs::write(
            &path,
            "# comment\nopen_settings = \"ctrl+,\"\nnot_an_action = \"ctrl+z\"\nbroken = \"ctrl+notakey\"\nthis is not toml\n",
        )
        .unwrap();
        let keymap = Keymap::load(&path);
        assert_eq!(
            keymap.resolve(&[Scope::Global], Chord::ctrl(',')),
            Some(Intent::OpenSettings)
        );
        assert_eq!(keymap.warnings.len(), 3, "{:?}", keymap.warnings);
    }

    #[test]
    fn saved_overrides_reload_from_the_written_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-keys.toml");
        let mut keymap = Keymap::built_in();
        keymap.rebind(Intent::OpenCommandPalette, Chord::ctrl('j'));
        keymap.save(&path).expect("the file is writable");

        // The file names the action, not the enum: it is the same grammar the
        // loader reads, and a human has to be able to edit it.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("command_palette = \"Ctrl+J\""),
            "{written}"
        );

        let reloaded = Keymap::load(&path);
        assert_eq!(
            reloaded.chord_for(Intent::OpenCommandPalette),
            Some(Chord::ctrl('j'))
        );
        assert!(reloaded.is_overridden(Intent::OpenCommandPalette));
        assert!(reloaded.warnings.is_empty(), "{:?}", reloaded.warnings);
    }

    #[test]
    fn resetting_an_override_restores_the_default_and_its_aliases() {
        let mut keymap = Keymap::built_in();
        keymap.rebind(Intent::OpenCommandPalette, Chord::ctrl('j'));
        assert!(keymap.is_overridden(Intent::OpenCommandPalette));
        // While the override is live the old chord is free for someone else.
        assert_eq!(
            keymap.conflict(Scope::Global, Chord::ctrl('p'), Intent::OpenSettings),
            None
        );

        keymap.reset(Intent::OpenCommandPalette);
        assert_eq!(
            keymap.conflict(Scope::Global, Chord::ctrl('p'), Intent::OpenSettings),
            Some(Intent::OpenCommandPalette)
        );
        assert!(!keymap.is_overridden(Intent::OpenCommandPalette));
        assert_eq!(
            keymap.chord_for(Intent::OpenCommandPalette),
            Some(Chord::ctrl('p'))
        );
        // The hidden `:` alias comes back with it, which a field-by-field
        // restore could not have done: a rebind had collapsed it.
        assert_eq!(
            keymap.resolve(&[Scope::Global], Chord::plain(KeyCode::Char(':'))),
            Some(Intent::OpenCommandPalette)
        );
        assert!(keymap.warnings.is_empty(), "{:?}", keymap.warnings);
    }

    #[test]
    fn a_missing_user_file_keeps_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let keymap = Keymap::load(&directory.path().join("absent.toml"));
        assert!(keymap.warnings.is_empty());
        assert_eq!(
            keymap.resolve(&[Scope::Global], Chord::ctrl('p')),
            Some(Intent::OpenCommandPalette)
        );
    }

    #[test]
    fn chord_parser_rejects_nonsense() {
        assert!("".parse::<Chord>().is_err());
        assert!("ctrl".parse::<Chord>().is_err());
        assert!("ctrl+a+b".parse::<Chord>().is_err());
        assert!("ctrl+notakey".parse::<Chord>().is_err());
    }
}
