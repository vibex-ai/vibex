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
    Sessions,
    Agent,
    Files,
    Changes,
    Terminal,
    Management,
    Providers,
    Agents,
    Mcp,
    Skills,
    Prompts,
    Hooks,
    Devices,
    Usage,
    Recovery,
    Settings,
    Help,
}

impl Scope {
    /// The scope whose bindings are shown for a given page scope.
    pub const ALL: &'static [Scope] = &[
        Scope::Global,
        Scope::Composer,
        Scope::Overlay,
        Scope::Sessions,
        Scope::Agent,
        Scope::Files,
        Scope::Changes,
        Scope::Terminal,
        Scope::Management,
        Scope::Providers,
        Scope::Agents,
        Scope::Mcp,
        Scope::Skills,
        Scope::Prompts,
        Scope::Hooks,
        Scope::Devices,
        Scope::Usage,
        Scope::Recovery,
        Scope::Settings,
        Scope::Help,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Composer => "composer",
            Scope::Overlay => "overlay",
            Scope::Sessions => "sessions",
            Scope::Agent => "agent",
            Scope::Files => "files",
            Scope::Changes => "changes",
            Scope::Terminal => "terminal",
            Scope::Management => "management",
            Scope::Agents => "agents",
            Scope::Providers => "providers",
            Scope::Mcp => "mcp",
            Scope::Skills => "skills",
            Scope::Prompts => "prompts",
            Scope::Hooks => "hooks",
            Scope::Devices => "devices",
            Scope::Usage => "usage",
            Scope::Recovery => "recovery",
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
            Scope::Overlay => Category::Modals,
            Scope::Sessions | Scope::Files | Scope::Changes | Scope::Terminal => {
                Category::Workbench
            }
            Scope::Management
            | Scope::Providers
            | Scope::Agents
            | Scope::Mcp
            | Scope::Skills
            | Scope::Prompts
            | Scope::Hooks => Category::Management,
            Scope::Devices | Scope::Usage | Scope::Recovery | Scope::Settings | Scope::Help => {
                Category::Panels
            }
        }
    }
}

/// One row of a binding table.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub scope: Scope,
    pub chord: Chord,
    pub intent: Intent,
    /// Help text. `None` marks a hidden alias: dispatchable, never advertised.
    pub label: Option<&'static str>,
}

const fn binding(scope: Scope, chord: Chord, intent: Intent, label: &'static str) -> Binding {
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
pub static DEFAULT_BINDINGS: &[Binding] = &[
    // ---- global ---------------------------------------------------------
    binding(
        Scope::Global,
        Chord::ctrl('p'),
        Intent::OpenCommandPalette,
        "Command palette",
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
        "Contextual help",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::F(1)),
        Intent::OpenSettings,
        "Settings",
    ),
    alias(Scope::Global, Chord::ctrl(','), Intent::OpenSettings),
    binding(Scope::Global, Chord::ctrl('q'), Intent::RequestQuit, "Quit"),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Esc),
        Intent::Back,
        "Back / close",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Tab),
        Intent::FocusNext,
        "Next pane",
    ),
    alias(
        Scope::Global,
        Chord::plain(KeyCode::BackTab),
        Intent::FocusPrevious,
    ),
    binding(Scope::Global, Chord::ctrl('r'), Intent::Refresh, "Refresh"),
    binding(
        Scope::Global,
        Chord::ctrl('c'),
        Intent::ContextualCancel,
        "Cancel / interrupt",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('1')),
        Intent::GotoSessions,
        "Sessions",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('2')),
        Intent::GotoManagement,
        "Management",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('3')),
        Intent::GotoUsage,
        "Usage",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('4')),
        Intent::OpenSettings,
        "Settings",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Char('5')),
        Intent::ToggleHelp,
        "Help",
    ),
    // ---- sessions -------------------------------------------------------
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
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
        "Next",
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
        "Open session",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('n')),
        Intent::NewSession,
        "New session",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        "Filter",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('r')),
        Intent::BeginRenameSession,
        "Rename",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('f')),
        Intent::ForkSession,
        "Fork session",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('a')),
        Intent::ArchiveSession,
        "Archive",
    ),
    binding(
        Scope::Sessions,
        Chord::ctrl('x'),
        Intent::DeleteSession,
        "Delete",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('e')),
        Intent::ToggleSessionCard,
        "Details",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('c')),
        Intent::CollapseSessionCards,
        "Close cards",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('y')),
        Intent::CopySessionRow,
        "Copy",
    ),
    binding(
        Scope::Sessions,
        Chord::ctrl('a'),
        Intent::ToggleShowArchived,
        "Show archived",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Right),
        Intent::EnterSession,
        "Open workspace",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('p')),
        Intent::PinSession,
        "Pin",
    ),
    binding(
        Scope::Sessions,
        Chord::new(KeyCode::Up, KeyModifiers::ALT),
        Intent::MoveSessionUp,
        "Move up",
    ),
    binding(
        Scope::Sessions,
        Chord::new(KeyCode::Down, KeyModifiers::ALT),
        Intent::MoveSessionDown,
        "Move down",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('g')),
        Intent::ToggleSidebarGrouping,
        "Grouping",
    ),
    // ---- agent (transcript) ---------------------------------------------
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::PageUp),
        Intent::ScrollPageUp,
        "Page up",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::PageDown),
        Intent::ScrollPageDown,
        "Page down",
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('u'),
        Intent::ScrollHalfPageUp,
        "Half page up",
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('d'),
        Intent::ScrollHalfPageDown,
        "Half page down",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::End),
        Intent::ScrollToBottom,
        "Bottom (follow)",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Home),
        Intent::ScrollToTop,
        "Top",
    ),
    // `u` is free in this scope and reads as "up one page of history"; the
    // scroll keys around it are all taken already.
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('u')),
        Intent::LoadOlderHistory,
        "Older history",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('e')),
        Intent::ToggleBlockExpanded,
        "Expand block",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(2)),
        Intent::ToggleAllBlocksExpanded,
        "Expand all",
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('e'),
        Intent::ToggleReasoningExpanded,
        "Toggle reasoning",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('y')),
        Intent::CopyBlockBody,
        "Copy block",
    ),
    binding(
        Scope::Agent,
        Chord::ctrl('y'),
        Intent::CopyBlockMetadata,
        "Copy metadata",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Enter),
        Intent::OpenBlockDetails,
        "Block details",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('i')),
        Intent::FocusComposer,
        "Write a message",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(6)),
        Intent::ContinueTurn,
        "Continue turn",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Left),
        Intent::PreviousPanel,
        "Previous panel",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Right),
        Intent::NextPanel,
        "Next panel",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginTranscriptSearch,
        "Find",
    ),
    // The queue's own keys. `Alt` keeps them out of the way of the transcript,
    // and every one of them no-ops when nothing is queued.
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Up, KeyModifiers::ALT),
        Intent::QueueSelectPrevious,
        "Queue up",
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Down, KeyModifiers::ALT),
        Intent::QueueSelectNext,
        "Queue down",
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
        "Send now",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('n')),
        Intent::SearchNext,
        "Next match",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::Char('p')),
        Intent::SearchPrevious,
        "Prev match",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(3)),
        Intent::OpenChanges,
        "Changes",
    ),
    binding(
        Scope::Agent,
        Chord::plain(KeyCode::F(4)),
        Intent::OpenFiles,
        "Files",
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('d'), KeyModifiers::ALT),
        Intent::ToggleDock,
        "Dock",
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('g'), KeyModifiers::ALT),
        Intent::DockActivate,
        "Dock open",
    ),
    binding(
        Scope::Agent,
        Chord::new(KeyCode::Char('h'), KeyModifiers::ALT),
        Intent::DockHideDone,
        "Hide done",
    ),
    // ---- composer -------------------------------------------------------
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Enter),
        Intent::SubmitComposer,
        "Send",
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Enter, KeyModifiers::SHIFT),
        Intent::InsertNewline,
        "Newline",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('m'),
        Intent::ToggleMultiline,
        "Multiline mode",
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Up),
        Intent::ComposerHistoryPrevious,
        "Previous message",
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Down),
        Intent::ComposerHistoryNext,
        "Next message",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('o'),
        Intent::EditComposerExternally,
        "Edit in $EDITOR",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('b'),
        Intent::BackgroundRunningCommand,
        "Send to background",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('s'),
        Intent::SteerRunningTurn,
        "Steer running turn",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('n'),
        Intent::CompletionNext,
        "Next completion",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('p'),
        Intent::CompletionPrevious,
        "Previous completion",
    ),
    binding(
        Scope::Composer,
        Chord::plain(KeyCode::Tab),
        Intent::CompletionAccept,
        "Accept completion",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('w'),
        Intent::DeleteWordBefore,
        "Delete word",
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
        "Kill word",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('k'),
        Intent::KillToLineEnd,
        "Kill to end",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('u'),
        Intent::KillToLineStart,
        "Kill to start",
    ),
    binding(Scope::Composer, Chord::ctrl('y'), Intent::YankKill, "Yank"),
    binding(
        Scope::Composer,
        Chord::ctrl('z'),
        Intent::ComposerUndo,
        "Undo",
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('z'), KeyModifiers::ALT),
        Intent::ComposerRedo,
        "Redo",
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('b'), KeyModifiers::ALT),
        Intent::ComposerWordLeft,
        "Word left",
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
        "Word right",
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
        "Line start",
    ),
    binding(
        Scope::Composer,
        Chord::ctrl('e'),
        Intent::ComposerLineEnd,
        "Line end",
    ),
    // `Ctrl+A` already owns line-start in this scope, so the whole-draft
    // selection gets the Alt layer, which is where the composer keeps its
    // larger edits (kill, redo, word motion).
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('i'), KeyModifiers::ALT),
        Intent::AttachImage,
        "Image",
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('a'), KeyModifiers::ALT),
        Intent::SelectAllDraft,
        "Select all",
    ),
    binding(
        Scope::Composer,
        Chord::new(KeyCode::Char('c'), KeyModifiers::ALT),
        Intent::CopyDraftSelection,
        "Copy draft",
    ),
    // ---- overlays -------------------------------------------------------
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Esc),
        Intent::CloseOverlay,
        "Close",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Enter),
        Intent::ConfirmOverlay,
        "Confirm",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Tab),
        Intent::OverlayNextField,
        "Next field",
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
        "Previous",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    // ---- deviceless pages ------------------------------------------------
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Enter),
        Intent::OpenSelectedFile,
        "Open file",
    ),
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedFile,
        "Edit in $EDITOR",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('a')),
        Intent::GitStageSelected,
        "Stage",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('u')),
        Intent::GitUnstageSelected,
        "Unstage",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('c')),
        Intent::GitCommit,
        "Commit",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('d')),
        Intent::ShowDiff,
        "Diff",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('w')),
        Intent::WorktreeMenu,
        "Worktrees",
    ),
    binding(
        Scope::Terminal,
        Chord::plain(KeyCode::Char('n')),
        Intent::NewTerminal,
        "New terminal",
    ),
    binding(
        Scope::Terminal,
        Chord::plain(KeyCode::Char('x')),
        Intent::CloseTerminal,
        "Close terminal",
    ),
    // ---- management ------------------------------------------------------
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Enter),
        Intent::OpenManagementSection,
        "Open",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Enter),
        Intent::ActivateProviderProfile,
        "Use profile",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditProviderProfile,
        "Edit profile",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Char('s')),
        Intent::EditProviderSecret,
        "Set credential",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Char('t')),
        Intent::TestProviderProfile,
        "Test connection",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Char('m')),
        Intent::FetchProviderModels,
        "Fetch models",
    ),
    binding(
        Scope::Providers,
        Chord::plain(KeyCode::Char('p')),
        Intent::EditProviderProjection,
        "Edit settings",
    ),
    binding(
        Scope::Mcp,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Mcp,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Mcp,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleSelectedEntry,
        "Enable / disable",
    ),
    binding(
        Scope::Mcp,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedEntry,
        "Edit",
    ),
    binding(
        Scope::Skills,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Skills,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Skills,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleSelectedEntry,
        "Enable / disable",
    ),
    binding(
        Scope::Skills,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedEntry,
        "Edit",
    ),
    binding(
        Scope::Prompts,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Prompts,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Prompts,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleSelectedEntry,
        "Enable / disable",
    ),
    binding(
        Scope::Prompts,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedEntry,
        "Edit",
    ),
    binding(
        Scope::Hooks,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Hooks,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Hooks,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleSelectedEntry,
        "Enable / disable",
    ),
    binding(
        Scope::Hooks,
        Chord::plain(KeyCode::Char('e')),
        Intent::EditSelectedEntry,
        "Edit",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('a')),
        Intent::InstallOrUpdateAgent,
        "Install / update",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('l')),
        Intent::AgentAuthMenu,
        "Sign in",
    ),
    // ---- devices ---------------------------------------------------------
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('p')),
        Intent::CreatePairingCode,
        "Pair a device",
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('x')),
        Intent::RevokeSelectedDevice,
        "Revoke",
    ),
    binding(
        Scope::Devices,
        Chord::plain(KeyCode::Char('u')),
        Intent::OpenDeviceAudit,
        "Audit log",
    ),
    // ---- usage -----------------------------------------------------------
    binding(
        Scope::Usage,
        Chord::plain(KeyCode::Char('r')),
        Intent::Refresh,
        "Refresh",
    ),
    // ---- recovery --------------------------------------------------------
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Enter),
        Intent::ActivateRecoveryAction,
        "Run",
    ),
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Char('d')),
        Intent::ExportDiagnostics,
        "Export diagnostics",
    ),
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Char('b')),
        Intent::CreateBackup,
        "Create backup",
    ),
    binding(
        Scope::Recovery,
        Chord::plain(KeyCode::Char('i')),
        Intent::InspectBackup,
        "Inspect backup",
    ),
    binding(
        Scope::Recovery,
        Chord::ctrl('r'),
        Intent::RestoreBackup,
        "Restore backup",
    ),
    // ---- settings --------------------------------------------------------
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Enter),
        Intent::ActivateSetting,
        "Change",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Left),
        Intent::SettingPrevious,
        "Previous value",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Right),
        Intent::SettingNext,
        "Next value",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        "Filter",
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
        "Reset",
    ),
    binding(
        Scope::Settings,
        Chord::plain(KeyCode::F(9)),
        Intent::ReloadKeymap,
        "Reload key bindings",
    ),
    // ---- help ------------------------------------------------------------
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Up),
        Intent::SelectPrevious,
        "Previous",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Down),
        Intent::SelectNext,
        "Next",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::PageUp),
        Intent::ScrollPageUp,
        "Page up",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::PageDown),
        Intent::ScrollPageDown,
        "Page down",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Home),
        Intent::ScrollToTop,
        "Top",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::End),
        Intent::ScrollToBottom,
        "Bottom",
    ),
    binding(
        Scope::Help,
        Chord::plain(KeyCode::Char('/')),
        Intent::BeginFilter,
        "Search help",
    ),
    // ---- shared intents that also need a visible home --------------------
    binding(
        Scope::Global,
        Chord::ctrl('b'),
        Intent::ToggleSidebar,
        "Sidebar",
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
        "Workspace",
    ),
    binding(
        Scope::Global,
        Chord::ctrl('g'),
        Intent::SwitchAgentRuntime,
        "Runtime",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::F(5)),
        Intent::ProbeAgentRuntime,
        "Probe runtimes",
    ),
    binding(
        Scope::Global,
        Chord::plain(KeyCode::Delete),
        Intent::ClearFilter,
        "Clear filter",
    ),
    // ---- workspace browser -----------------------------------------------
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('b')),
        Intent::OpenWorkspaceBrowser,
        "Browse dirs",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char('u')),
        Intent::WorkspaceBrowseUp,
        "Parent dir",
    ),
    binding(
        Scope::Sessions,
        Chord::plain(KeyCode::Char(' ')),
        Intent::WorkspaceBrowseSelect,
        "Use dir",
    ),
    // ---- approval card and elicitation form ------------------------------
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('a')),
        Intent::ApprovalApprove,
        "Allow",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('d')),
        Intent::ApprovalDeny,
        "Deny",
    ),
    binding(
        Scope::Overlay,
        Chord::ctrl('a'),
        Intent::ApprovalAlways,
        "Always",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('n')),
        Intent::ApprovalFocusNext,
        "Next option",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('s')),
        Intent::ElicitationSubmit,
        "Submit answers",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Left),
        Intent::ElicitationFieldPrevious,
        "Prev question",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Right),
        Intent::ElicitationFieldNext,
        "Next question",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char(' ')),
        Intent::OverlayToggleValue,
        "Toggle",
    ),
    binding(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('p')),
        Intent::PaletteRun,
        "Run command",
    ),
    alias(
        Scope::Overlay,
        Chord::plain(KeyCode::Char('g')),
        Intent::CompletionCancel,
    ),
    // ---- files / changes extras ------------------------------------------
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Char(' ')),
        Intent::ToggleFileTreeExpanded,
        "Expand dir",
    ),
    binding(
        Scope::Files,
        Chord::plain(KeyCode::Char('/')),
        Intent::FileSearch,
        "Search files",
    ),
    binding(
        Scope::Changes,
        Chord::ctrl('x'),
        Intent::GitRevert,
        "Revert",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('l')),
        Intent::GitHistory,
        "History",
    ),
    binding(
        Scope::Changes,
        Chord::plain(KeyCode::Char('b')),
        Intent::GitBranches,
        "Branches",
    ),
    alias(
        Scope::Changes,
        Chord::plain(KeyCode::Char('p')),
        Intent::WorktreePreflight,
    ),
    binding(
        Scope::Terminal,
        Chord::plain(KeyCode::Char('f')),
        Intent::TerminalToggleFollow,
        "Follow output",
    ),
    // ---- management extras -----------------------------------------------
    binding(
        Scope::Management,
        Chord::ctrl('x'),
        Intent::UninstallAgent,
        "Uninstall",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('p')),
        Intent::AgentAuthRefresh,
        "Re-probe auth",
    ),
    binding(
        Scope::Management,
        Chord::plain(KeyCode::Char('o')),
        Intent::AgentLogout,
        "Sign out",
    ),
    alias(
        Scope::Management,
        Chord::ctrl('r'),
        Intent::ReloadManagement,
    ),
    alias(
        Scope::Providers,
        Chord::plain(KeyCode::Char('h')),
        Intent::ProviderHealth,
    ),
    alias(
        Scope::Changes,
        Chord::plain(KeyCode::Char('n')),
        Intent::WorktreeCreate,
    ),
    alias(
        Scope::Usage,
        Chord::plain(KeyCode::Char('s')),
        Intent::UsageSessionScope,
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
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| PathBuf::from(value).join(".vibex"))
            })?;
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
                label: Some(intent.default_label()),
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
    fn every_intent_has_a_binding_and_an_id() {
        for intent in Intent::ALL {
            assert!(
                DEFAULT_BINDINGS
                    .iter()
                    .any(|binding| binding.intent == *intent),
                "{} has no default binding",
                intent.id()
            );
            assert_eq!(Intent::from_id(intent.id()), Some(*intent));
        }
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
