//! Rendering: layout maths, page shells, overlays and the pure view helpers.
//!
//! Two structural rules are enforced here rather than trusted to discipline:
//!
//! * **Every page renders through [`page_frame`]**, which draws the bordered
//!   title, the always-visible key bar and the summary line. A page cannot
//!   hand-roll its own chrome.
//! * **The key bar is generated from the binding table**, so a hint can never
//!   describe a key that does nothing.

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use vibex_ui::shell::ShellKind;

use crate::action::Intent;
use crate::app::{App, Availability, BannerTone, ComposerMode, Overlay, Page, ToastTone};
use crate::keymap::{Chord, Scope};
use crate::layout::Bands;
use crate::locale::Strings;
use crate::modal::{self, ModalChrome, ModalHint, ModalLayout, ModalSizing};
use crate::runtime_picker::{RuntimePickerRow, runtime_option_auth_label};
use crate::text::{display_width, pad_to_width, take_width, truncate_to_width};
use crate::theme::TuiTheme;

/// A block whose border glyphs match the terminal's capability.
///
/// A non-UTF-8 locale must still get a usable frame, so the box-drawing set is
/// swapped for `+-|` rather than assuming the glyphs will render.
fn bordered(theme: &TuiTheme) -> Block<'static> {
    modal::border_block(theme)
}

/// Draft rows the composer grows to before it scrolls instead.
///
/// The band is `borders + a blank row + these rows`; capping it is what keeps a
/// pasted document from taking the whole screen away from the transcript.
const MAX_COMPOSER_DRAFT_ROWS: usize = 8;

/// Columns the composer spends on the edge that marks its writing surface.
///
/// The transcript draws the reader's own message behind the same cell, so the
/// width is asked for rather than repeated: the two surfaces are one object at
/// two moments, and an edge of two different widths would say otherwise.
const BOX_EDGE_COLUMNS: u16 = crate::transcript::BOX_EDGE_WIDTH as u16;

/// Which seat the client is attached by. Decided by the composition root, not
/// by the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatKind {
    /// This process owns the `DesktopRuntime`.
    Authority,
    /// This process reaches a runtime over loopback or the network.
    Remote,
}

impl SeatKind {
    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            SeatKind::Authority => strings.seat_authority(),
            SeatKind::Remote => strings.seat_remote(),
        }
    }
}

/// The narrowest a main pane may be before the sidebar gives way.
pub const MIN_MAIN_COLUMNS: usize = 44;

/// Resolved pane widths for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutPlan {
    pub sidebar_width: usize,
    pub main_width: usize,
    pub details_width: usize,
    pub show_sidebar: bool,
    pub show_details: bool,
}

/// Compute the pane layout for a shell kind and terminal size.
pub fn layout_for(shell: ShellKind, columns: u16, rows: u16) -> LayoutPlan {
    let columns = usize::from(columns);
    let rows = usize::from(rows);
    // Panes sit edge to edge; the separators are their own borders, so no gap
    // columns are reserved. Reserving them made the sidebar vanish at widths
    // that still had room for it.
    match shell {
        ShellKind::Wide => {
            let sidebar = (columns / 5).clamp(24, 40);
            let details = (columns / 4).clamp(28, 56);
            let main = columns.saturating_sub(sidebar + details).max(24);
            LayoutPlan {
                sidebar_width: sidebar,
                main_width: main,
                details_width: details,
                show_sidebar: true,
                show_details: columns >= 140,
            }
        }
        ShellKind::Medium => {
            let sidebar = 26usize.min(columns / 3);
            let main = columns.saturating_sub(sidebar).max(24);
            LayoutPlan {
                sidebar_width: sidebar,
                main_width: main,
                details_width: 0,
                show_sidebar: true,
                show_details: false,
            }
        }
        ShellKind::Compact => LayoutPlan {
            sidebar_width: 0,
            main_width: columns.max(8),
            details_width: 0,
            show_sidebar: false,
            show_details: false,
        },
    }
    .clamp_to(columns, rows)
}

impl LayoutPlan {
    fn clamp_to(mut self, columns: usize, _rows: usize) -> Self {
        // The details pane is the one that yields: it holds context, while the
        // sidebar is how the reader navigates at all.
        if self.show_details && self.sidebar_width + self.main_width + self.details_width > columns
        {
            self.show_details = false;
            self.details_width = 0;
            self.main_width = columns.saturating_sub(self.sidebar_width).max(24);
        }
        // The sidebar only goes when even a minimum main pane would not fit.
        if self.show_sidebar && self.sidebar_width + MIN_MAIN_COLUMNS > columns {
            self.show_sidebar = false;
            self.sidebar_width = 0;
            self.main_width = columns;
        }
        self
    }
}

/// Columns the palette spends on a command's name before its hint starts.
///
/// Wide enough for the longest English name with room to spare. It is a column
/// budget rather than a character count because a Chinese glyph takes two
/// columns, and the hints must line up in every language.
const PALETTE_NAME_WIDTH: usize = 24;

/// A palette entry: the intent it runs.
///
/// The name and the one-line explanation are read from the locale table
/// through [`PaletteEntry::label`] and [`PaletteEntry::hint`] rather than
/// stored here, so the list follows the reader's language the way the rest of
/// the interface does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    pub intent: Intent,
}

impl PaletteEntry {
    /// The name this command is listed under.
    pub fn label(self, strings: Strings) -> &'static str {
        palette_label(self.intent, strings)
    }

    /// The one-line explanation drawn beside the name.
    pub fn hint(self, strings: Strings) -> &'static str {
        palette_hint(self.intent, strings)
    }
}

/// The palette's name for one command.
///
/// Every entry in [`PALETTE`] is named here. A command the palette never lists
/// falls back to the key bar's own label, so a row can never be drawn blank.
fn palette_label(intent: Intent, strings: Strings) -> &'static str {
    match intent {
        Intent::GotoSessions => strings.palette_cmd_sessions(),
        Intent::GotoManagement => strings.palette_cmd_management(),
        Intent::OpenSettings => strings.palette_cmd_settings(),
        Intent::ToggleHelp => strings.palette_cmd_help(),
        Intent::NewSession => strings.palette_cmd_new_session(),
        Intent::BeginRenameSession => strings.palette_cmd_rename_session(),
        Intent::ForkSession => strings.palette_cmd_fork_session(),
        Intent::ArchiveSession => strings.palette_cmd_archive_session(),
        Intent::UnarchiveSession => strings.palette_cmd_restore_session(),
        Intent::DeleteSession => strings.palette_cmd_delete_session(),
        Intent::SwitchAgentRuntime => strings.palette_cmd_switch_runtime(),
        Intent::ProbeAgentRuntime => strings.palette_cmd_probe_runtimes(),
        Intent::GotoDevices => strings.palette_cmd_devices(),
        Intent::ReloadKeymap => strings.palette_cmd_reload_keys(),
        Intent::Refresh => strings.palette_cmd_refresh(),
        Intent::RequestQuit => strings.palette_cmd_quit(),
        _ => intent.label(strings),
    }
}

/// The palette's one-line explanation for one command.
fn palette_hint(intent: Intent, strings: Strings) -> &'static str {
    match intent {
        Intent::GotoSessions => strings.palette_hint_sessions(),
        Intent::GotoManagement => strings.palette_hint_management(),
        Intent::OpenSettings => strings.palette_hint_settings(),
        Intent::ToggleHelp => strings.palette_hint_help(),
        Intent::NewSession => strings.palette_hint_new_session(),
        Intent::BeginRenameSession => strings.palette_hint_rename_session(),
        Intent::ForkSession => strings.palette_hint_fork_session(),
        Intent::ArchiveSession => strings.palette_hint_archive_session(),
        Intent::UnarchiveSession => strings.palette_hint_restore_session(),
        Intent::DeleteSession => strings.palette_hint_delete_session(),
        Intent::SwitchAgentRuntime => strings.palette_hint_switch_runtime(),
        Intent::ProbeAgentRuntime => strings.palette_hint_probe_runtimes(),
        Intent::GotoDevices => strings.palette_hint_devices(),
        Intent::ReloadKeymap => strings.palette_hint_reload_keys(),
        Intent::Refresh => strings.palette_hint_refresh(),
        Intent::RequestQuit => strings.palette_hint_quit(),
        _ => intent.help(strings),
    }
}

/// Every command the palette can run.
pub const PALETTE: &[PaletteEntry] = &[
    PaletteEntry {
        intent: Intent::GotoSessions,
    },
    PaletteEntry {
        intent: Intent::GotoManagement,
    },
    PaletteEntry {
        intent: Intent::OpenSettings,
    },
    PaletteEntry {
        intent: Intent::ToggleHelp,
    },
    PaletteEntry {
        intent: Intent::NewSession,
    },
    PaletteEntry {
        intent: Intent::BeginRenameSession,
    },
    PaletteEntry {
        intent: Intent::ForkSession,
    },
    PaletteEntry {
        intent: Intent::ArchiveSession,
    },
    PaletteEntry {
        intent: Intent::UnarchiveSession,
    },
    PaletteEntry {
        intent: Intent::DeleteSession,
    },
    PaletteEntry {
        intent: Intent::SwitchAgentRuntime,
    },
    PaletteEntry {
        intent: Intent::ProbeAgentRuntime,
    },
    PaletteEntry {
        intent: Intent::GotoDevices,
    },
    PaletteEntry {
        intent: Intent::ReloadKeymap,
    },
    PaletteEntry {
        intent: Intent::Refresh,
    },
    PaletteEntry {
        intent: Intent::RequestQuit,
    },
];

/// The heading a palette command is filed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaletteGroup {
    /// Where the reader can go: the pages of the client, and the shape of the
    /// one in front of them.
    View,
    /// Pairing a device and managing the ones already paired. The two are one
    /// subject — a device arrives by pairing and is revoked from the same
    /// list — so they share a heading rather than sitting under the pages they
    /// happen to be reached from.
    Pairing,
    /// The client itself: the runtime a session runs on, the keys it answers
    /// to, the picture on screen.
    App,
    /// The session list's own verbs: creating, renaming, forking, archiving,
    /// deleting. They act on the row the list has selected, which is why the
    /// section is named for the list rather than for one conversation.
    Session,
    /// The transcript the reader is reading.
    Workbench,
}

impl PaletteGroup {
    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            PaletteGroup::View => strings.palette_group_view(),
            PaletteGroup::Pairing => strings.palette_group_pairing(),
            PaletteGroup::App => strings.palette_group_app(),
            PaletteGroup::Session => strings.palette_group_session(),
            PaletteGroup::Workbench => strings.palette_group_workbench(),
        }
    }
}

/// Which heading a command belongs under.
pub const fn palette_group(intent: Intent) -> PaletteGroup {
    match intent {
        Intent::GotoSessions
        | Intent::GotoManagement
        | Intent::OpenSettings
        | Intent::ToggleHelp
        | Intent::ToggleSidebar
        | Intent::FocusNext
        | Intent::FocusPrevious => PaletteGroup::View,
        Intent::GotoDevices | Intent::RevokeSelectedDevice | Intent::OpenDeviceAudit => {
            PaletteGroup::Pairing
        }
        Intent::NewSession
        | Intent::BeginRenameSession
        | Intent::ForkSession
        | Intent::ArchiveSession
        | Intent::UnarchiveSession
        | Intent::DeleteSession
        | Intent::SwitchWorkspace
        | Intent::OpenSelectedSession => PaletteGroup::Session,
        Intent::OpenBlockDetails
        | Intent::CopyBlockBody
        | Intent::CopyBlockMetadata
        | Intent::ToggleBlockExpanded
        | Intent::BeginTranscriptSearch => PaletteGroup::Workbench,
        // The client's own commands, and the configuration centre's record
        // verbs with them: they change how the client behaves rather than
        // which page is in front of the reader.
        _ => PaletteGroup::App,
    }
}

/// Fuzzy-score `needle` against `haystack`.
///
/// Subsequence matching, with the bonuses that make a two-word query work:
/// a hit at a word start beats a hit inside a word, and a contiguous run beats
/// a scattered one. `None` means "does not match at all", which is what keeps
/// a sheet of commands from surviving a query that has nothing to do with it.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    let mut score = 0i32;
    let mut haystack_chars = haystack.char_indices().peekable();
    let mut last_match: Option<usize> = None;
    for wanted in needle.chars() {
        let mut found = None;
        for (index, candidate) in haystack_chars.by_ref() {
            if candidate == wanted {
                found = Some(index);
                break;
            }
        }
        let index = found?;
        score += 1;
        match last_match {
            Some(previous) if previous + 1 == index => score += 4,
            _ => {}
        }
        let at_word_start = index == 0
            || haystack[..index]
                .chars()
                .next_back()
                .is_some_and(|character| character == ' ' || character == '-' || character == '_');
        if at_word_start {
            score += 6;
        }
        last_match = Some(index);
    }
    Some(score)
}

/// One palette command, together with the heading it is drawn under.
///
/// The grouping travels with the entry rather than being read back from its
/// position, so the frame can insert a heading whenever the group changes
/// without the list and the drawing disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteListing {
    pub group: PaletteGroup,
    pub entry: PaletteEntry,
}

/// Palette entries matching `query`, each with the heading it is drawn under.
///
/// A command is listed exactly once and always under its own heading: the list
/// a reader learns is the list they get, on the first open as much as on the
/// hundredth.
pub fn palette_listings(query: &str, strings: Strings) -> Vec<PaletteListing> {
    palette_matches(query, strings)
        .into_iter()
        .map(|entry| PaletteListing {
            group: palette_group(entry.intent),
            entry,
        })
        .collect()
}

/// Palette entries matching `query`.
///
/// The query is matched against the reader's own names and the English ones
/// alike: a reader who knows a command as `Settings` can find it in a Chinese
/// interface, and one who learned 用量 does not have to translate in their head.
pub fn palette_matches(query: &str, strings: Strings) -> Vec<PaletteEntry> {
    let english = Strings::for_locale(crate::locale::Locale::En);
    let needle = query.trim().to_lowercase();
    let mut scored = PALETTE
        .iter()
        .copied()
        .filter_map(|entry| {
            // The label is what the reader is typing at; the hint is what they
            // are reading, so a hit there scores lower but still counts.
            let label = fuzzy_score(entry.label(strings), &needle)
                .max(fuzzy_score(entry.label(english), &needle));
            let hint = if needle.is_empty() {
                None
            } else {
                fuzzy_score(entry.hint(strings), &needle)
                    .max(fuzzy_score(entry.hint(english), &needle))
            };
            let score = match (label, hint) {
                (Some(score), _) => score,
                (None, Some(score)) => score - 20,
                (None, None) => return None,
            };
            Some((score, entry))
        })
        .collect::<Vec<_>>();
    // Best first, then by group so the list reads in sections, then by name.
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| palette_group(left.1.intent).cmp(&palette_group(right.1.intent)))
            .then_with(|| left.1.label(strings).cmp(right.1.label(strings)))
    });
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// The transcript band's width at a terminal `columns` wide.
///
/// The agent view is one full-width stack, so this is the content area less
/// the outer padding and the rail's gutter — not a sidebar-adjusted pane
/// width. It mirrors [`crate::layout::compute`], and a test asserts the two
/// agree at several sizes so the two cannot drift.
pub fn transcript_width(_shell: ShellKind, columns: u16) -> usize {
    let columns = usize::from(columns);
    let hpad = usize::from(crate::layout::OUTER_HPAD.min(columns as u16 / 4));
    let content = columns.saturating_sub(hpad * 2);
    let gutter = if content >= usize::from(crate::layout::MIN_TRANSCRIPT_FOR_GUTTER) {
        usize::from(crate::layout::GUTTER_WIDTH)
    } else {
        0
    };
    content.saturating_sub(gutter)
}

/// Detail text for one transcript block, used by the details overlay.
pub fn block_detail_text(
    transcript: &mut crate::transcript::Transcript,
    index: usize,
) -> Option<(String, String)> {
    let block = transcript.block(index)?.clone();
    let title = format!(
        "{} · {}",
        crate::transcript::kind_label(block.kind, Strings::with_locale(crate::locale::Locale::En)),
        block.title
    );
    let body = transcript.block_text(index).unwrap_or_default();
    let meta = transcript.block_metadata(index).unwrap_or_default();
    Some((title, format!("{meta}\n\n{body}")))
}

/// Render one frame.
/// Draw one frame.
///
/// The screen is a vertical stack of full-width bands (see [`crate::layout`]).
/// Bands that are not needed this frame get a zero rect and their renderer is
/// skipped, so an idle session shows the transcript the whole height of the
/// screen rather than the top third of it.
pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    app.regions.begin_frame();
    // What the page is about to draw is what says whether a line changed, so
    // the transition is begun here — before the first frame that shows the new
    // text, rather than a tick after it.
    app.observe_composer_text();
    let area = frame.area();
    app.shell = crate::app::shell_for_columns(area.width);
    let theme = app.theme.clone();
    let strings = app.strings;

    frame.render_widget(Block::default().style(theme.base()), area);

    // The composer's height depends on the width the layout gives it: a draft
    // that is one logical line can still wrap to several display rows. The
    // first pass measures the prompt band, the second sizes it.
    let mut request = band_request(app);
    request.prompt = prompt_height(app, crate::layout::compute(area, request).prompt.width);
    let bands = crate::layout::compute(area, request);

    render_status_band(frame, bands.status, app, &theme, strings);
    if Bands::is_visible(bands.tasks) {
        render_tasks_band(frame, bands.tasks, app, &theme, strings);
    }
    if Bands::is_visible(bands.todo) {
        render_todo_band(frame, bands.todo, app, &theme, strings);
    }

    // The pairing QR is a picture the terminal holds rather than a frame it
    // drew, so the page is what says whether it belongs there: the panel places
    // it, and every frame that does not ask for it takes it down below.
    app.pairing_qr.begin_frame();
    // Same for the Agent marks in the session list and the switcher: the frame
    // starts with none wanted, the pages that draw rows ask for the ones they
    // show, and the settle at the end of this function takes down the rest.
    app.row_marks.begin_frame();
    match app.page {
        // The workbench pages share the scrollback band; they are views over the
        // same session, so they occupy the same part of the screen.
        Page::Agent => render_scrollback(frame, bands.scrollback, app, &theme, strings),
        Page::NewSession => render_new_session(frame, bands.scrollback, app, &theme, strings),
        Page::Sessions => render_session_view(frame, bands.scrollback, app, &theme, strings),
        Page::Management
        | Page::Providers
        | Page::Agents
        | Page::Mcp
        | Page::Skills
        | Page::Prompts
        | Page::Devices
        | Page::Settings
        | Page::Help => render_management_view(frame, bands.scrollback, app, &theme, strings),
    }
    app.pairing_qr.settle(&app.terminal_images);

    // The rail maps the transcript, so it belongs to the pages that show one.
    // A page writing a new session draws its own welcome in the same band, and
    // the session the reader left is still selected behind it: its rail is a
    // map of a conversation that is not on screen, so it leaves with it rather
    // than standing in the corner of a page that has no turns yet.
    if Bands::is_visible(bands.gutter) && app.page_shows_transcript() {
        render_gutter(frame, bands.gutter, app, &theme);
    }
    // Cleared like the dock: opening the dock folds this band away, and a stale
    // rect would let a click select a row that is no longer on screen.
    app.regions.queue = None;
    if Bands::is_visible(bands.queue) {
        render_queue_band(frame, bands.queue, app, &theme, strings);
    }
    if Bands::is_visible(bands.turn_status) {
        render_turn_status(frame, bands.turn_status, app, &theme, strings);
    }
    if Bands::is_visible(bands.banner) {
        app.regions.banner = Some(bands.banner);
        render_banner(frame, bands.banner, app, &theme, strings);
    }
    // The dock is the one band that comes and goes on a key, so its hit rect is
    // cleared rather than left pointing at rows that are no longer drawn.
    app.regions.dock = None;
    if Bands::is_visible(bands.dock) {
        render_dock_band(frame, bands.dock, app, &theme, strings);
    }
    if Bands::is_visible(bands.prompt) {
        render_prompt(frame, bands.prompt, app, &theme, strings);
    }
    render_shortcuts(frame, bands.shortcuts, app, &theme);

    // The chip under the caret explains itself in the room above the composer,
    // which is why this is drawn after the transcript and before any modal: the
    // panel belongs to the draft rather than to the page, and a modal that is
    // open is a different conversation. It is called on every frame rather than
    // only the ones that draw a panel, because a picture it put on the terminal
    // outlives the frame that drew it and has to be taken down from here.
    crate::preview::render(frame, composer_preview_area(area, &bands), app, &theme);

    if let Some(overlay) = app.overlay.clone() {
        render_overlay(frame, area, app, &overlay, &theme);
    }
    // The page asked for its marks as it drew, and the modal has published its
    // box by now, so this is the first moment the frame can tell a mark the box
    // covers from one it does not. An overlay that published no box is read as
    // the whole frame: the frame cannot say which marks it left alone, and
    // taking all of them down is the answer that cannot be wrong.
    let covered = match &app.overlay {
        // The question card is a band rather than a box: it took the composer's
        // rows, and the marks beside it are still on the page.
        Some(Overlay::Elicitation { .. }) => Some(bands.prompt),
        Some(_) => Some(app.regions.modal.unwrap_or(area)),
        None => None,
    };
    app.row_marks.place_asked(&app.terminal_images, covered);
    // Last, because the switcher places its own Agent marks from inside the
    // overlay: everything that could ask for one has asked by now, and a mark
    // nobody asked for is a layer the terminal is still holding.
    app.row_marks.settle(&app.terminal_images);
}

/// The rows a chip preview may use: everything above the composer's own stack.
///
/// The panel is anchored to the bottom of this area, so it grows upward from
/// the queue, turn line, banner and dock rather than covering them — those are
/// the rows that say what the turn is doing, and the draft's own band is the
/// one thing a preview must never hide.
fn composer_preview_area(area: Rect, bands: &Bands) -> Rect {
    let anchor = [
        bands.queue,
        bands.turn_status,
        bands.banner,
        bands.dock,
        bands.prompt,
    ]
    .into_iter()
    .filter(|band| Bands::is_visible(*band))
    .map(|band| band.y)
    .min()
    .unwrap_or(area.bottom());
    Rect {
        height: anchor.saturating_sub(area.y),
        ..area
    }
}

/// What the frame wants on screen, given the current state.
fn band_request(app: &App) -> crate::layout::BandRequest {
    // Everything from the turn line down belongs to the session view, and the
    // rule for what that is lives with the page ([`App::page_shows_transcript`])
    // rather than here: the rail down the right edge reads the same answer, and
    // two copies of it are how the composing page ended up wearing the session
    // behind it. On the session list the active session's plan and turn state
    // are not what the reader is looking at, and printing them above the list
    // is how a global page ends up wearing another page's chrome. The page
    // where a session is being written is the same case from the other side: it
    // is a page with a composer, not a session, so the session behind it —
    // which the reader is leaving — must not put its turn, plan or held queue
    // on it.
    let session_view = app.page_shows_transcript();
    // A send the runtime has not answered yet counts as running: the reader
    // pressed Enter, and the round trip must not read as "idle, message lost".
    let running = app.turn_reads_running();
    // Present whenever there is something to say about the turn: it is running,
    // it is waiting on the reader, or it is idle with a live session to report.
    let turn_status = u16::from(
        session_view
            && (running
                || app.is_animating()
                || app.page_approval_count() > 0
                || app.page_elicitation_count() > 0
                || app.active_session().is_some()
                || app.turn_started.is_some()),
    );
    // Only the open session's queue is drawn: the band is above *this*
    // composer, and a message held for another session is not this reader's to
    // see or act on here.
    let queued = app.queued_for_active().len() as u16;
    crate::layout::BandRequest {
        // The tasks row appears only when background work exists, so an idle
        // session spends no rows on it.
        tasks: if session_view && app.background_task_count() > 0 {
            1
        } else {
            0
        },
        // The dock already lists the plan and the held queue, so its sections
        // replace those bands while it is open rather than saying it twice.
        todo: if !session_view || app.dock_open {
            0
        } else {
            u16::from(app.todo_total_count() > 0)
        },
        queue: if !session_view || app.dock_open || queued == 0 {
            0
        } else {
            (queued + 1).min(5)
        },
        turn_status,
        banner: u16::from(app.banner.is_some()),
        dock: if session_view && app.dock_open {
            app.dock_height()
        } else {
            0
        },
        // The composer belongs to a session. On a page with no session context
        // there is nothing to send, so the band is not allocated and the
        // transcript gets its rows instead. Its height is left at zero here and
        // measured by [`prompt_height`] once the band's width is known.
        prompt: 0,
        // The card takes the composer's place, so it needs the same blank row
        // above it wherever it opens — including the pages that have no
        // composer of their own.
        prompt_gap: u16::from(
            app.page.is_session_page() || matches!(app.overlay, Some(Overlay::Elicitation { .. })),
        ),
        // The pages a reader writes on spend their last row on the draft rather
        // than on a legend: the composer's own line already names the runtime,
        // and every key the bar would advertise is one `?` away. The pages that
        // are navigated rather than written on keep it.
        shortcuts: u16::from(!app.page.is_session_page()),
        status_line: 0,
    }
}

/// The prompt band's height at `width` columns, borders included.
///
/// Two things live in this band and only one of them at a time: the draft, and
/// the question card that takes the draft's place while the Agent waits. The
/// draft is wrapped at the same width the renderer paints at, so a line wider
/// than the box grows the box rather than being clipped to its first row, and
/// the card asks for the rows its open tab needs.
/// [`crate::layout::compute`] squeezes both back down on a short terminal — the
/// transcript's minimum rows are kept first — and each scrolls under a shorter
/// box rather than losing its place.
fn prompt_height(app: &App, width: u16) -> u16 {
    if let Some(form) = app.elicitation_form() {
        return elicitation_card_height(app, &form, width);
    }
    if !app.page.is_session_page() {
        return 0;
    }
    // The draft's column, measured the way [`render_composer`] paints it: the
    // box's edge and padding column on each side, then the prompt arrow. The
    // two must agree or the band is sized for a wrap the painter does not
    // make — a two-row draft in a one-row box scrolls its first row away.
    let text_width = usize::from(width)
        .saturating_sub(usize::from(BOX_EDGE_COLUMNS + 2))
        .saturating_sub(crate::glyphs::PROMPT_ARROW_WIDTH);
    let rows = app
        .composer
        .display_row_count(text_width)
        .min(MAX_COMPOSER_DRAFT_ROWS);
    // One padding row on each side of the draft, followed by the context line.
    (rows as u16 + 3).max(4)
}

/// A one-line band summarising background work.
fn render_tasks_band(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let count = app.background_task_count();
    let text = format!(
        "{} {count} {}",
        crate::glyphs::diamond_dotted(app.glyph_tier()),
        strings.background_tasks()
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.roles.gray),
        ))),
        area,
    );
}

/// A one-line band listing the session's open steps.
fn render_todo_band(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let Some(progress) = app.todo_progress() else {
        return;
    };
    let heading = format!(
        "{} {}/{}",
        strings.dock_plan(),
        progress.done,
        progress.total
    );
    let label = progress.running.as_deref().unwrap_or(&progress.title);
    let available = usize::from(area.width).saturating_sub(display_width(&heading) + 3);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(heading, theme.muted()),
            Span::styled(
                format!(" · {}", truncate_to_width(label, available, "…")),
                theme.muted(),
            ),
        ])),
        area,
    );
}

/// The transcript band: full width, no frame.
///
/// A border around the transcript would spend two columns and two rows on a
/// rectangle the reader already knows the shape of. The blocks separate
/// themselves with a blank row, and the reader's own words carry a prompt mark,
/// so the band needs no frame and no rail.
fn render_scrollback(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if !Bands::is_visible(area) {
        return;
    }
    // The mouse layer maps a pointer back to a display line through this rect,
    // so it is published before any early return.
    app.regions.scrollback = area;
    // The transcript wraps to the band it is drawn in, and this is the only
    // place that knows how wide that is. Configuring it from a pane layout
    // instead is what left a wide terminal's right half empty.
    app.transcript.configure(usize::from(area.width), theme);
    // The live thought window's rail is a frame of the chrome animation, so the
    // clock is handed over here — the only place that draws the transcript, and
    // therefore the only place that knows a frame is being drawn at all.
    app.transcript.set_animation_phase(app.animation_phase());
    if app.transcript.is_empty() {
        render_welcome(frame, area, app, theme, strings);
        return;
    }
    // The search bar owns the last two rows of the transcript band while it is
    // open: a rule and the bar itself, exactly as the completion drawer owns
    // the space above the composer.
    let (content, search_bar) = if app.search.is_some() && area.height > 3 {
        (
            Rect {
                height: area.height.saturating_sub(2),
                ..area
            },
            Some(Rect {
                y: area.y + area.height.saturating_sub(2),
                height: 2,
                ..area
            }),
        )
    } else {
        (area, None)
    };
    let height = usize::from(content.height);
    let selected = app.selection_for(Scope::Agent);
    let mut scroll = app.scroll;
    scroll.selected = Some(selected);
    // A pinned prompt header takes its rows off the top before the transcript
    // is positioned. It only claims rows the body below has already let go of,
    // so the prompt is drawn once whether it hangs above the window or sits
    // inside it.
    let header = app.transcript.sticky_header(scroll, height, theme, strings);
    let (content, height) = match &header {
        Some(header) => {
            let reserved = header.reserved_rows().min(usize::from(content.height));
            let content = Rect {
                y: content.y + reserved as u16,
                height: content.height.saturating_sub(reserved as u16),
                ..content
            };
            (content, usize::from(content.height))
        }
        None => (content, height),
    };
    // The scroll ceiling is measured against the band the reader can actually
    // see, so the frame that drew it is the one that records it.
    app.transcript_band_rows = height;
    // A fold's title leads with the Agent's brand mark, and the cells that
    // picture is drawn over are reserved by the row itself — so the transcript
    // has to know whether the terminal can hold one before it lays the row out.
    app.transcript
        .set_pictures(app.terminal_images.supports_pixels());
    let pattern = app
        .search
        .as_ref()
        .and_then(|search| search.pattern.clone());
    let mut lines = app.transcript.visible_lines_highlighted(
        scroll,
        height,
        theme,
        strings,
        pattern.as_ref(),
        search_highlight_style(theme),
    );
    // Following the tail is a *position*, not an absence of one. Keeping the
    // state's offset on the row the frame starts at is what makes a scroll up
    // from the tail a step instead of a leap to the top of the session.
    if app.scroll.follow {
        app.scroll.offset = app.transcript.scroll_offset();
    }
    // The selection is painted last so it wins over a search highlight on the
    // same cells: the reader's most recent gesture is the one they mean.
    if let Some(selection) = app.text_selection {
        paint_selection(
            &mut lines,
            &selection,
            app.transcript.scroll_offset(),
            selection_style(theme),
        );
    }
    // The mouse layer maps a pointer to a display line through this rect, so it
    // is published once the header and the search bar have taken their rows.
    app.regions.scrollback = content;
    if let Some(header) = header {
        // The pinned header sits on a raised surface so the transcript moving
        // underneath it reads as a separate layer rather than as more prose.
        let header_area = Rect {
            height: header.lines.len() as u16,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Text::from(header.lines.clone()))
                .style(Style::default().bg(theme.roles.surface_raised)),
            header_area,
        );
    }
    // The Agent's mark on a fold's title, asked for where the row that reserved
    // its cells was drawn. A picture has no clip rect, so a mark is asked for
    // only while its row is in the window; the frame places it once the modal's
    // box is known, so a mark the box covers never reaches the terminal and one
    // beside the dialog is still the Agent's mark.
    for mark in app.transcript.window_marks() {
        if mark.column + crate::row_marks::MARK_COLS > content.width {
            continue;
        }
        let rect = Rect {
            x: content.x.saturating_add(mark.column),
            y: content
                .y
                .saturating_add(u16::try_from(mark.row).unwrap_or(u16::MAX)),
            width: crate::row_marks::MARK_COLS,
            height: crate::row_marks::MARK_ROWS,
        };
        app.row_marks.want(
            &app.terminal_images,
            &crate::row_marks::Mark::Agent(mark.identity.clone()),
            crate::theme::color_rgb(theme.roles.foreground),
            rect,
        );
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), content);
    if let Some(rows) = search_bar {
        render_search_bar(frame, rows, app, theme, strings);
    }
}

/// The inverted band a mouse selection is painted with.
fn selection_style(theme: &TuiTheme) -> Style {
    Style::default()
        .fg(theme.roles.background)
        .bg(theme.roles.foreground)
}

/// Paint the selected column range of every visible display line.
///
/// `first_line` is the display line the first visible row shows, so a selection
/// made in one frame still highlights correctly after the transcript scrolls.
/// Both ends are cells the reader pointed at, so the range is closed: the
/// character under the pointer at the end of a drag lights up with the rest of
/// what it went over.
fn paint_selection(
    lines: &mut [Line<'static>],
    selection: &crate::app::TextSelection,
    first_line: usize,
    style: Style,
) {
    let (start, end) = selection.ordered();
    for (offset, line) in lines.iter_mut().enumerate() {
        let display_line = first_line + offset;
        if display_line < start.0 || display_line > end.0 {
            continue;
        }
        let from = if display_line == start.0 {
            usize::from(start.1)
        } else {
            0
        };
        let to = if display_line == end.0 {
            usize::from(end.1) + 1
        } else {
            usize::MAX
        };
        let painted = crate::transcript::paint_columns(line.clone(), from, to, style);
        *line = painted;
    }
}

/// The inverted band a search match is painted with.
fn search_highlight_style(theme: &TuiTheme) -> Style {
    Style::default()
        .fg(theme.roles.background)
        .bg(theme.roles.accent_attention)
        .add_modifier(Modifier::BOLD)
}

/// The transcript search bar: a rule, the query with a caret, and the counter.
fn render_search_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let Some(search) = app.search.as_ref() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let rule = Rect { height: 1, ..area };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(rule.width)),
            Style::default().fg(theme.roles.gray_dim),
        ))),
        rule,
    );
    let bar = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    // The counter is measured first so it can never be pushed off the row by a
    // long query; the query is truncated to what is left.
    let counter = if search.error.is_some() {
        strings.search_bad_pattern().to_string()
    } else if search.is_empty() {
        String::new()
    } else if search.total == 0 {
        strings.search_no_matches().to_string()
    } else {
        format!("{}/{}", search.position(), search.total)
    };
    let counter_width = display_width(&counter);
    let label = truncate_to_width(strings.search_label(), usize::from(bar.width), "");
    let label_width = display_width(&label);
    let query_width = usize::from(bar.width)
        .saturating_sub(label_width + counter_width + 2)
        .max(4);
    let (visible, clipped) = search_query_window(&search.query, query_width);
    let caret = if search.composing { "▏" } else { "" };
    let mut spans = vec![
        Span::styled(label, Style::default().fg(theme.roles.gray)),
        Span::styled(visible, Style::default().fg(theme.roles.foreground)),
        Span::styled(caret.to_string(), theme.accent()),
    ];
    if clipped {
        spans.push(Span::styled("…", Style::default().fg(theme.roles.gray_dim)));
    }
    let left = Rect {
        width: usize::from(bar.width).saturating_sub(counter_width) as u16,
        ..bar
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
    if !counter.is_empty() {
        let style = if search.error.is_some() {
            theme.warning()
        } else {
            Style::default().fg(theme.roles.gray)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(counter, style))).alignment(Alignment::Right),
            bar,
        );
    }
}

/// The tail of `query` that fits in `width`, plus whether it was clipped.
fn search_query_window(query: &str, width: usize) -> (String, bool) {
    if display_width(query) <= width {
        return (query.to_string(), false);
    }
    let mut tail = String::new();
    let mut used = 0usize;
    for grapheme in query.graphemes(true).rev() {
        let grapheme_width = display_width(grapheme);
        if used + grapheme_width > width.saturating_sub(1) {
            break;
        }
        tail.insert_str(0, grapheme);
        used += grapheme_width;
    }
    (tail, true)
}

/// The full-screen session list.
///
/// Sessions are a navigation level, not a permanent column. As a view they get
/// the whole screen, and the screen is spent the way a reader scans a list of
/// work: where they are and what the sessions are doing on the first line, the
/// key that starts another on the second, then a heading per group and, under
/// it, a row carrying the mark, the title, who is answering, when it last spoke
/// and — on its own dim line — what it last said.
fn render_session_view(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let page = session_page(area);
    let list = app.sidebar_view();
    // The page's three bands, and the air between them: the header names the
    // list, the action row is the one button a session list needs, and the rows
    // are what the reader came for.
    //
    // The button belongs to the page's own chrome — it is what the title is
    // there for — so one row of air is what keeps it under the title without
    // reading as the next line of the same sentence. The list below it is a
    // different kind of thing, and one row is enough to stop the button from
    // reading as the list's first row.
    const ACTION_AIR_ABOVE: u16 = 1;
    const ACTION_AIR_BELOW: u16 = 1;
    let list_offset = 1 + ACTION_AIR_ABOVE + 1 + ACTION_AIR_BELOW;
    // A terminal too short for chrome and a row keeps the rows instead: three
    // rows of chrome over two rows of list can only show a heading, and a
    // heading the reader cannot open is worth less than the session under it.
    // The page's own action row goes with the rest of the chrome, so a cursor
    // that had walked onto it steps back onto the list: a line nobody drew
    // cannot be where the reader is.
    if page.height < list_offset + 2 {
        if app.session_cursor_is_actions() {
            app.set_selection(Scope::Sessions, 0);
        }
        render_session_rows(frame, page, &list, app, theme, strings);
        return;
    }
    let list_area = Rect {
        y: page.y + list_offset,
        height: page.height - list_offset,
        ..page
    };
    render_session_header(
        frame,
        Rect { height: 1, ..page },
        &list.states,
        app,
        theme,
        strings,
    );
    render_session_actions(
        frame,
        Rect {
            y: page.y + 1 + ACTION_AIR_ABOVE,
            height: 1,
            ..page
        },
        app,
        theme,
        strings,
    );
    render_session_rows(frame, list_area, &list, app, theme, strings);
}

/// The rect the session list draws in.
///
/// The list is the page, not a panel on it: there is no frame, so the rows and
/// their headings own the two columns and the two rows a border would have
/// taken. The margin is what keeps a title off the terminal's edge and lines
/// the page up with the status band above it.
fn session_page(area: Rect) -> Rect {
    const MARGIN: u16 = 1;
    let margin = MARGIN.min(area.width / 4);
    Rect {
        x: area.x + margin,
        width: area.width.saturating_sub(margin * 2),
        ..area
    }
}

/// The list's first line: which page this is, and how much work there is.
///
/// The status band above already carries the location, so the line does not
/// repeat it: it names the page, and the chips count what the list holds. The
/// filter takes the name's place while it is being typed — it is what the list
/// is answering, and the page has not stopped being the list.
fn render_session_header(
    frame: &mut Frame<'_>,
    area: Rect,
    states: &[(vibex_core::AgentSessionState, usize)],
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let chips = trim_spans(
        session_chip_spans(states, app, theme, strings),
        usize::from(area.width),
    );
    let chip_width = spans_width(&chips);
    // The chips describe the list, so they keep their columns and the name
    // gives way to them.
    let left_budget = usize::from(area.width)
        .saturating_sub(chip_width)
        .saturating_sub(2);
    let left = if app.filtering || !app.filter.is_empty() {
        let mut spans = vec![
            Span::styled("/ ", Style::default().fg(theme.roles.accent_user)),
            Span::styled(
                app.filter.clone(),
                Style::default().fg(theme.roles.foreground),
            ),
        ];
        if app.filtering {
            spans.push(Span::styled(
                "▏",
                Style::default().fg(theme.roles.accent_user),
            ));
        }
        spans
    } else {
        vec![Span::styled(
            strings.sessions_title().to_string(),
            theme.strong(),
        )]
    };

    frame.render_widget(
        Paragraph::new(Line::from(trim_spans(left, left_budget))),
        area,
    );
    if chips.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(chips)).alignment(Alignment::Right),
        area,
    );
}

/// The list's second line: the one action a session list needs, and the
/// control that answers the page — where the next session opens — with its key.
///
/// The controls are laid out from the right edge inwards and one that does not
/// fit is dropped whole rather than truncated: half a key is a key the reader
/// cannot use, and the action on the left is never the one that goes. Each is
/// published as a click target, and says so when the pointer is over it.
fn render_session_actions(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let new_session = format!("+ {}", strings.session_new());
    let new_session = truncate_to_width(&new_session, usize::from(area.width), "…");
    let label_width = display_width(&new_session);
    let mut candidates: Vec<(String, Intent)> = Vec::new();
    if let Some(chord) = app.keymap.chord_for(Intent::SwitchWorkspace) {
        candidates.push((
            format!(
                "[{} {}]",
                strings.session_workspace_label(),
                chord.display()
            ),
            Intent::SwitchWorkspace,
        ));
    }
    // Right to left: the last candidate keeps the right edge, and the ones
    // before it are placed to its left while they fit together.
    let mut chosen: Vec<&(String, Intent)> = Vec::new();
    let mut used = 0usize;
    for candidate in candidates.iter().rev() {
        let gap = if chosen.is_empty() { 0 } else { 2 };
        let width = display_width(&candidate.0);
        if label_width + 2 + used + gap + width > usize::from(area.width) {
            break;
        }
        used += gap + width;
        chosen.push(candidate);
    }
    chosen.reverse();
    // The button is a button: a click runs the same intent the key does, and
    // the pointer resting on it says so before the click does.
    app.regions.hints.push((
        Rect {
            width: label_width as u16,
            height: 1,
            ..area
        },
        Intent::NewSession,
    ));
    let hovered = app.hovered_hint;
    // The action row is a row like any other while the cursor is on it: it wears
    // the selection band, and the control `←`/`→` have stepped onto is lit the
    // way the pointer lights one. `+ New session` is lit while merely *on* the
    // row, because `Enter` starts a session there without a second press.
    let on_row = app.session_cursor_is_actions();
    let focused = app
        .session_row_action
        .filter(|(row, _)| *row == crate::app::SESSION_ACTION_ROW)
        .map(|(_, intent)| intent);
    let lit = |intent: Intent| hovered == Some(intent) || focused == Some(intent);
    let band = on_row.then_some(theme.roles.surface_highlight);
    let dressed = |style: Style| match band {
        Some(band) => style.bg(band),
        None => style,
    };
    let button_style = if lit(Intent::NewSession) {
        dressed(
            Style::default()
                .fg(theme.roles.foreground)
                .add_modifier(Modifier::BOLD),
        )
    } else if on_row {
        dressed(Style::default().fg(theme.roles.foreground))
    } else {
        Style::default().fg(theme.roles.text_secondary)
    };
    let padding = usize::from(area.width).saturating_sub(label_width + used);
    let mut spans = vec![
        Span::styled(new_session, button_style),
        Span::styled(" ".repeat(padding), dressed(Style::default())),
    ];
    let mut x = usize::from(area.x) + padding + label_width;
    for (index, (text, intent)) in chosen.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", dressed(Style::default())));
            x += 2;
        }
        let width = display_width(text);
        app.regions.hints.push((
            Rect {
                x: x as u16,
                y: area.y,
                width: width as u16,
                height: 1,
            },
            *intent,
        ));
        let style = if lit(*intent) {
            dressed(
                Style::default()
                    .fg(theme.roles.foreground)
                    .add_modifier(Modifier::BOLD),
            )
        } else if on_row {
            dressed(Style::default().fg(theme.roles.text_secondary))
        } else {
            theme.dimmed(theme.roles.gray)
        };
        spans.push(Span::styled(text.clone(), style));
        x += width;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// What the header says about the list: how many sessions are waiting on the
/// reader, still working, stopped badly and quiet.
///
/// The order is the order they have to be acted on, and a family with nothing
/// in it is left out rather than drawn as a zero the reader has to read past.
fn session_chip_spans(
    states: &[(vibex_core::AgentSessionState, usize)],
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) -> Vec<Span<'static>> {
    let tier = app.glyph_tier();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (state, count) in states {
        let (mark, colour, label) = session_chip(*state, tier, theme, strings);
        if !spans.is_empty() {
            spans.push(Span::styled(
                "  ",
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
        spans.push(Span::styled(mark, Style::default().fg(colour)));
        spans.push(Span::styled(
            format!(" {count} {label}"),
            Style::default().fg(theme.roles.gray),
        ));
    }
    spans
}

/// The mark, colour and word of one summary chip.
fn session_chip(
    state: vibex_core::AgentSessionState,
    tier: crate::glyphs::GlyphTier,
    theme: &TuiTheme,
    strings: Strings,
) -> (&'static str, ratatui::style::Color, &'static str) {
    match state {
        vibex_core::AgentSessionState::NeedsInput => (
            crate::glyphs::state_marker("waiting", tier),
            theme.roles.accent_attention,
            strings.session_chip_waiting(),
        ),
        vibex_core::AgentSessionState::Running => (
            crate::glyphs::state_marker("running", tier),
            theme.roles.accent_running,
            strings.session_chip_running(),
        ),
        vibex_core::AgentSessionState::Error => (
            crate::glyphs::state_marker("failed", tier),
            theme.roles.danger,
            strings.session_chip_failed(),
        ),
        _ => (
            crate::glyphs::diamond_hollow(tier),
            theme.roles.gray_dim,
            strings.session_chip_idle(),
        ),
    }
}

/// A control a list row draws at its own right edge — a session row's action
/// buttons, a heading's icon slot, a branch's disclosure.
///
/// The row does not know where it will land: the page lays every row out and
/// then draws a window of them, so a control is placed relative to the row —
/// its line and its column — and `render_session_rows` moves it onto the screen
/// once the window is known.
struct SessionRowControl {
    /// The line of the row the control sits on, counting from its first.
    line: usize,
    /// The control's first column, from the row's left edge.
    column: u16,
    /// How many cells the control's target covers.
    width: u16,
    intent: Intent,
}

/// One picture a list row wants on the terminal.
///
/// Like a control, a mark is collected while the row is laid out and placed
/// onto the screen once the window is known, because a row scrolled past must
/// not leave a picture behind. The row has already reserved the cells, so what
/// travels here is only what to draw and in what colour. The session list and
/// the runtime switcher both lay rows out this way, which is why the shape
/// carries no assumption about which list it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRowMark {
    /// The line of the row the mark sits on, counting from its first.
    line: usize,
    /// The mark's first column, from the row's left edge.
    column: u16,
    /// What to draw: an Agent's brand mark, or one of the list's own icons.
    mark: crate::row_marks::Mark,
    /// The colour a themed mark is drawn in. An Agent's mark ignores it when it
    /// carries the brand's own colours; a chrome icon is always drawn in it, so
    /// a delete button is the danger colour and a pin is the warning one.
    color: ratatui::style::Color,
}

/// The air a row keeps between its content and its right-hand controls, and
/// between the controls themselves.
///
/// A control's target is everything it draws, so a pointer does not have to
/// land on the character itself, and two targets never touch: the column
/// between them belongs to neither.
const ROW_CONTROL_AIR: usize = 1;
const ROW_CONTROL_GAP: usize = 1;

/// How far one level of the tree moves a row's content off the left edge.
///
/// Headings and the sessions under them are indented by the same rule, so a
/// folder's children hang under the folder and a project's rows under the
/// project: without it a folder's sessions start to the left of their own
/// heading and the nesting reads backwards.
pub const SESSION_ROW_INDENT: usize = 2;

/// The columns a row spends before its title: the selection bar with the air
/// after it, the icon slot every row opens with, and the air after that.
///
/// The slot is two cells wide because that is the shape a picture has to be:
/// two cells is the narrowest box that is still square-ish under the cell
/// aspect the rest of this client assumes. A row with no picture to draw still
/// spends the cells, so the titles of a list do not start in a different column
/// on a different terminal.
///
/// Published because it is a contract rather than a detail: it is where a row's
/// title opens, and a second line that opened anywhere else would break the
/// rail the two lines share.
pub const SESSION_ROW_LEAD: usize = 2 + crate::row_marks::MARK_COLS as usize + 1;

/// The columns a heading spends before its name: the icon slot, and the air
/// after it.
///
/// Shorter than a session row's lead by the selection bar, which only a row
/// that can be opened draws: a heading is folded, not entered, and the columns
/// the bar would have taken are the name's.
pub const HEADING_ROW_LEAD: usize = crate::row_marks::MARK_COLS as usize + 1;

/// The guide a delegated row draws in place of its selection bar: the leading
/// cells of its title line, and of the line under it.
///
/// The guide is the desktop's own, in cells rather than pixels. The elbow lands
/// in the row's indent column — the column the Agent mark of the session that
/// owns it is drawn in — and turns toward the row's own mark, which sits one
/// indent further in. Each level above that owns a column of its own, one indent
/// apart, holding a trunk while the session at that level still has a sibling to
/// reach.
///
/// The elbow keeps a cell of air between its corner and the row's mark. A rule
/// run right up to the mark is what the desktop can do and a terminal cannot: a
/// box-drawing corner fills its own cell to the edge, so the line would touch a
/// picture drawn in a cell of its own. The air is the whole difference, and it
/// is spent out of the same two columns as before, so the mark stays in the
/// column every other row's mark is in.
///
/// `shape` is read from the row outward, one entry per level, the row's own
/// level first — see [`crate::sessions::branch_shape`]. The second line mirrors
/// the first, with a trunk where the elbow turned and only while a sibling still
/// follows: a branch that has ended stops at its elbow instead of running on
/// into nothing, and the trunk of a branch that has not runs down past the row's
/// two lines to the next sibling.
///
/// Both runs are exactly as wide as the bar and its air — the two columns a root
/// row spends before its Agent slot — so a delegated row's mark lands in the
/// same column as every other row's.
fn branch_lead(shape: &[bool], indent: usize, tier: crate::glyphs::GlyphTier) -> (String, String) {
    if shape.is_empty() {
        // A delegated row with no session above it in the list has no column to
        // hang from. It is drawn as a plain row rather than as an elbow into
        // nothing — still spending the bar's two columns, so its mark lines up
        // with every other row's.
        let blank = " ".repeat(indent + 2);
        return (blank.clone(), blank);
    }
    let mut above = vec![" "; indent + 2];
    for (level, continues) in shape.iter().enumerate().skip(1) {
        let Some(column) = indent.checked_sub(level * SESSION_ROW_INDENT) else {
            break;
        };
        if *continues {
            above[column] = crate::glyphs::branch_trunk(tier);
        }
    }
    let carries = shape.first().is_some_and(|continues| *continues);
    let mut title = above.clone();
    title[indent] = if carries {
        crate::glyphs::branch_elbow(false, tier)
    } else {
        crate::glyphs::branch_elbow(true, tier)
    };
    // The cell past the elbow is left blank: that air is what keeps the branch
    // off the row's Agent mark, and it costs the guide none of its own columns.
    let mut below = above;
    // The row's own column carries the trunk on to its next sibling; the air
    // after it stays blank, because the row's Agent mark is what the elbow
    // turned toward.
    below[indent] = if carries {
        crate::glyphs::branch_trunk(tier)
    } else {
        " "
    };
    (title.concat(), below.concat())
}

/// The buttons a session row offers while the reader is on it, in the order the
/// desktop's own action cluster and menu list the same verbs.
///
/// The row the cursor is on and the row under the pointer both wear them, and
/// every other row gives their columns back to its title. They take the place
/// of the status column rather than sitting beside it, which is what the
/// desktop does with its own action cluster: two answers to "what is this" and
/// "act on this" in one column would be one too many.
///
/// The pin leads because the desktop's own cluster leads with it — it is the one
/// verb there that is about the row's *place* rather than about the session, and
/// a reader who knows the cluster on the desktop finds it in the same column
/// here.
const SESSION_ROW_ACTIONS: [(crate::sidebar_icons::Icon, Intent); 6] = [
    (crate::sidebar_icons::Icon::Pin, Intent::PinSession),
    (
        crate::sidebar_icons::Icon::Resume,
        Intent::ToggleAutoContinue,
    ),
    (
        crate::sidebar_icons::Icon::Rename,
        Intent::BeginRenameSession,
    ),
    (crate::sidebar_icons::Icon::Fork, Intent::ForkSession),
    (crate::sidebar_icons::Icon::Archive, Intent::ArchiveSession),
    (crate::sidebar_icons::Icon::Delete, Intent::DeleteSession),
];

/// The buttons a project or folder row offers, in the desktop's own order.
///
/// The desktop reveals these on hover and puts the fold chevron among them. A
/// terminal has no hover, so they are drawn always; and the chevron is not
/// repeated here, because the heading's own leading slot is already the control
/// that folds it — a second one a few columns away would be the same button
/// twice.
const HEADING_ROW_ACTIONS: [(crate::sidebar_icons::Icon, Intent); 2] = [
    (crate::sidebar_icons::Icon::More, Intent::SidebarRowActions),
    (crate::sidebar_icons::Icon::New, Intent::NewSession),
];

/// The columns one icon button occupies: the mark itself, with the gap to the
/// next button counted separately.
fn icon_button_width() -> usize {
    crate::row_marks::MARK_COLS as usize
}

/// The width of a run of icon buttons, gaps included.
fn icon_cluster_width(count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    count * icon_button_width() + (count - 1) * ROW_CONTROL_GAP
}

/// The list itself: a heading per group, and under each a row per session.
fn render_session_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    list: &crate::sessions::SessionListRows,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let rows = &list.rows;
    if rows.is_empty() {
        render_welcome(frame, area, app, theme, strings);
        return;
    }
    let selected = app.selection_for(Scope::Sessions);
    // The control the keyboard has stepped onto is lit the way the pointer's own
    // is: both say "this button is the one a press would run", and the keyboard
    // is the one that has to survive the pointer being nowhere near the row.
    let focused_action = app.session_row_action;
    let hovered_action = app.hovered_row_action;
    // The branch every delegated row hangs from. It is read from the list as a
    // whole — a trunk is drawn from the rows that follow, which no single row
    // can answer — and read once, before anything is painted, so the geometry
    // the mouse is hit-tested against is the geometry on screen.
    let branches = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if row.delegated {
                crate::sessions::branch_shape(rows, index)
            } else {
                Vec::new()
            }
        })
        .collect::<Vec<_>>();
    let now_ms = vibex_core::unix_timestamp_ms();
    // What each row's right end says while the reader is not on it: the hour it
    // last spoke, the state it is in, and the marks that qualify either. It is
    // built for every row before anything is drawn, because the widest answer
    // is what every row reserves — a title that grew when a session started
    // running would rewrite the list under the reader's hand, and the columns
    // of the list would stop lining up.
    let statuses = rows
        .iter()
        .map(|row| {
            let session = row
                .session_id
                .as_ref()
                .and_then(|session_id| app.session_by_id(session_id));
            session_status_column(row, session, app, theme, strings, now_ms)
        })
        .collect::<Vec<_>>();
    let status_column = statuses
        .iter()
        .map(|column| column.width)
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) / 3);
    // Every row is laid out, and the page draws a window of the result. The
    // list is taller than any terminal — a workspace can hold hundreds of
    // sessions and a row can carry a detail card — so the window is measured in
    // lines and moved by line, which is what the wheel and the page keys step
    // through.
    let mut all = Vec::new();
    let mut heights = Vec::with_capacity(rows.len());
    let mut starts = Vec::with_capacity(rows.len());
    // A row's own controls, in the coordinates of the laid-out list: the line
    // each landed on, the row it belongs to, and where it sits inside that row.
    // They are moved onto the screen once the window is known, so a button on a
    // row the reader has scrolled past is never published as a click target.
    let mut controls: Vec<(usize, usize, SessionRowControl)> = Vec::new();
    // Every picture the rows want, in the coordinates of the laid-out list.
    // Like the controls, they are moved onto the screen once the window is
    // known, so a mark on a row the reader has scrolled past is never placed.
    let mut wanted_marks: Vec<(usize, SessionRowMark)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        starts.push(all.len());
        let mut row_controls = Vec::new();
        let mut row_marks = Vec::new();
        let row_lines = session_list_item(
            row,
            branches.get(index).map(Vec::as_slice).unwrap_or_default(),
            index == selected,
            app.hover == Some((Scope::Sessions, index)),
            focused_action
                .filter(|(focused_row, _)| *focused_row == index)
                .or_else(|| hovered_action.filter(|(hovered_row, _)| *hovered_row == index))
                .map(|(_, intent)| intent),
            statuses.get(index),
            status_column,
            usize::from(area.width),
            app,
            theme,
            strings,
            &mut row_controls,
            &mut row_marks,
        );
        for control in row_controls {
            controls.push((starts[index] + control.line, index, control));
        }
        for mark in row_marks {
            wanted_marks.push((starts[index] + mark.line, mark));
        }
        heights.push(row_lines.len().max(1));
        all.extend(row_lines);
    }
    let window = usize::from(area.height);
    app.session_band_rows = window;
    app.session_band_width = usize::from(area.width);
    // The cursor's row keeps itself in sight until the reader takes the window
    // over; a row taller than the window shows its top, not its middle. A cursor
    // on the page's own action row is above every row, so what it keeps in sight
    // is the top of the list.
    let anchor = if selected == crate::app::SESSION_ACTION_ROW {
        0
    } else {
        selected
    };
    if !app.session_scroll_manual
        && let Some(top) = starts
            .get(anchor.min(rows.len().saturating_sub(1)))
            .copied()
    {
        let bottom = top + heights.get(anchor).copied().unwrap_or(1);
        if top < app.session_scroll {
            app.session_scroll = top;
        } else if bottom > app.session_scroll.saturating_add(window) {
            app.session_scroll = bottom.saturating_sub(window);
        }
    }
    // Clamp to the last page, then land on a row's first line: a window that
    // starts mid-row shows half a row at the top, which reads as a torn frame.
    let clamped = app.session_scroll.min(all.len().saturating_sub(window));
    let offset = starts
        .iter()
        .rev()
        .find(|start| **start <= clamped)
        .copied()
        .unwrap_or(0);
    app.session_scroll = offset;
    // The controls land where the rows they belong to were drawn. One on a line
    // the window has scrolled past — or one a terminal too narrow to hold it has
    // pushed out of the band — is not published at all: a click there would
    // otherwise act on a control nobody can see.
    for (line, row, control) in controls {
        let fits =
            usize::from(control.column) + usize::from(control.width) <= usize::from(area.width);
        let drawn = line
            .checked_sub(offset)
            .is_some_and(|screen_line| screen_line < window);
        if !fits || !drawn {
            continue;
        }
        app.regions.row_actions.push(crate::app::RowActionRegion {
            rect: Rect {
                x: area.x.saturating_add(control.column),
                y: area.y.saturating_add((line - offset) as u16),
                width: control.width,
                height: 1,
            },
            row,
            intent: control.intent,
        });
    }
    let visible = all
        .into_iter()
        .skip(offset)
        .take(window)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(visible)), area);
    // The pictures land where the rows they belong to were drawn. One on a line
    // the window has scrolled past, or one a terminal too narrow to hold it has
    // pushed out of the band, is not asked for at all: a picture has no clip
    // rect, so one left standing over the row below would be worse than the
    // character it replaced.
    //
    // A modal over the list does not take the list's marks with it. The frame
    // places what is asked for here once the modal has published its box, so
    // only the marks that box actually covers are dropped and the reader keeps
    // every icon the dialog leaves in the open. See [`crate::row_marks`].
    //
    // `app.row_marks` is settled at the end of the frame, so a mark nobody
    // wants any more is taken down rather than left over the list.
    for (line, mark) in &wanted_marks {
        let fits = mark.column + crate::row_marks::MARK_COLS <= area.width;
        let drawn = line
            .checked_sub(offset)
            .is_some_and(|screen_line| screen_line < window);
        if !fits || !drawn {
            continue;
        }
        let rect = Rect {
            x: area.x.saturating_add(mark.column),
            y: area.y.saturating_add((line - offset) as u16),
            width: crate::row_marks::MARK_COLS,
            height: crate::row_marks::MARK_ROWS,
        };
        app.row_marks.want(
            &app.terminal_images,
            &mark.mark,
            crate::theme::color_rgb(mark.color),
            rect,
        );
    }
    // The rows are not all one line tall, so the mouse needs to know which row
    // the drawn window starts on and how many of its lines are on screen.
    let first_row = starts
        .iter()
        .rposition(|start| *start <= offset)
        .unwrap_or(0);
    let mut drawn = 0usize;
    let mut visible_heights = Vec::new();
    for height in heights.iter().skip(first_row) {
        if drawn >= window {
            break;
        }
        let clipped = (*height).min(window - drawn);
        visible_heights.push(clipped.min(usize::from(u16::MAX)) as u16);
        drawn += clipped;
    }
    app.regions.list = Some(crate::app::ListRegion {
        rect: area,
        scope: Scope::Sessions,
        rows: rows.len(),
        first_line: 0,
        first_row,
        heights: visible_heights,
    });
}

/// One row of the list: its heading, its title line, its second line and any
/// detail card the reader has opened under it.
#[allow(clippy::too_many_arguments)]
fn session_list_item(
    row: &vibex_desktop_model::AgentSidebarRow,
    branch: &[bool],
    selected: bool,
    hovered: bool,
    lit_control: Option<Intent>,
    status: Option<&SessionStatusColumn>,
    status_column: usize,
    width: usize,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    controls: &mut Vec<SessionRowControl>,
    marks: &mut Vec<SessionRowMark>,
) -> Vec<Line<'static>> {
    let session = row
        .session_id
        .as_ref()
        .and_then(|session_id| app.session_by_id(session_id));
    if row.kind != vibex_desktop_model::AgentSidebarRowKind::Session {
        return vec![
            session_heading_line(
                row,
                selected,
                hovered,
                lit_control,
                width,
                app,
                theme,
                strings,
                controls,
                marks,
            ),
            Line::from(""),
        ];
    }
    let mut lines = session_row_lines(
        row,
        branch,
        session,
        selected,
        hovered,
        lit_control,
        status,
        status_column,
        width,
        app,
        theme,
        strings,
        controls,
        marks,
    );
    if let Some(session) = session
        && app.session_card_expanded(session.id.as_str())
    {
        lines.extend(session_card_lines(app, session, strings, theme, width));
    }
    // The row keeps a blank line under it: rows that touch are read as one
    // block, and a list of sessions is a list of separate pieces of work.
    lines.push(Line::from(""));
    lines
}

/// The column a row's icon slot opens at, from the row's left edge.
///
/// The slot is the second thing a row draws: the selection bar owns the first
/// column and the space after it, and the air past the slot is what keeps the
/// title off the mark.
fn row_icon_column(indent: usize) -> u16 {
    (indent + 2) as u16
}

/// Whether a chrome icon can be drawn as a picture here.
///
/// The terminal has to hold a picture at all, and it has to have a colour to
/// draw the icon in: every icon in the vocabulary is drawn in the row's own
/// colour, so a palette tier that resolves no RGB has none. Either failure
/// falls back to the character, which is the answer this branch exists to give.
fn icon_drawable(app: &App, color: ratatui::style::Color) -> bool {
    app.terminal_images.supports_pixels() && crate::theme::color_rgb(color).is_some()
}

/// The two-cell icon slot a row opens with: a picture where the terminal can
/// hold one, and the character the same vocabulary carries where it cannot.
///
/// Both renderings are the same two columns wide, so a row does not reflow when
/// the terminal's capabilities change. A picture is returned as a mark for the
/// page to place rather than drawn here, because only the page knows which line
/// the row finally landed on.
#[allow(clippy::too_many_arguments)]
fn row_icon_slot(
    icon: crate::sidebar_icons::Icon,
    drawable: bool,
    tier: crate::glyphs::GlyphTier,
    color: ratatui::style::Color,
    style: Style,
    line: usize,
    column: u16,
    marks: &mut Vec<SessionRowMark>,
) -> Vec<Span<'static>> {
    if drawable {
        // The cells the picture covers are painted blank so nothing shows
        // through around it: a picture sits above the text layer, not instead
        // of it. They keep the row's own style, because the wash a row wears
        // behind its mark — the selection band, the light under the pointer —
        // is painted by the text layer and shows around the icon.
        marks.push(SessionRowMark {
            line,
            column,
            mark: crate::row_marks::Mark::Chrome(icon),
            color,
        });
        return vec![Span::styled(
            " ".repeat(usize::from(crate::row_marks::MARK_COLS)),
            style,
        )];
    }
    vec![Span::styled(
        format!(
            "{:<width$}",
            icon.glyph(tier),
            width = usize::from(crate::row_marks::MARK_COLS)
        ),
        style,
    )]
}

/// A group's heading: what it is called, how much it holds, and a rule to the
/// edge of the list.
///
/// The rule is what turns a stack of rows into sections: without it a project
/// name is one more line of text, and the reader has to read it to find the
/// next one. The trailing mark is the "N running" the desktop keeps at the
/// row's far edge, and past it are the row's own buttons.
#[allow(clippy::too_many_arguments)]
fn session_heading_line(
    row: &vibex_desktop_model::AgentSidebarRow,
    selected: bool,
    hovered: bool,
    lit_control: Option<Intent>,
    width: usize,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    controls: &mut Vec<SessionRowControl>,
    marks: &mut Vec<SessionRowMark>,
) -> Line<'static> {
    use vibex_desktop_model::AgentSidebarRowKind;
    let tier = app.glyph_tier();
    let folder = row.kind == AgentSidebarRowKind::Folder;
    let label_style = if selected {
        Style::default().fg(theme.roles.accent_user)
    } else if hovered {
        Style::default().fg(theme.roles.foreground)
    } else {
        Style::default().fg(theme.roles.gray_bright)
    }
    .add_modifier(Modifier::BOLD);
    let blank = Style::default();
    let indent = usize::from(row.depth) * SESSION_ROW_INDENT;
    // The heading's icon slot is also the control that folds it, so a reader who
    // clicks the mark gets what the mark says. A folder wears the folder the
    // desktop draws — open or shut — and a project wears the disclosure alone,
    // because the terminal draws no project logo at all.
    let open = !row.collapsed;
    let icon = match (folder, open) {
        (true, true) => crate::sidebar_icons::Icon::FolderOpen,
        (true, false) => crate::sidebar_icons::Icon::FolderClosed,
        (false, true) => crate::sidebar_icons::Icon::Expand,
        (false, false) => crate::sidebar_icons::Icon::Collapse,
    };
    let icon_color = if selected {
        theme.roles.accent_user
    } else {
        theme.roles.gray_bright
    };
    let mut spans = vec![Span::raw(" ".repeat(indent))];
    spans.extend(row_icon_slot(
        icon,
        folder && icon_drawable(app, icon_color),
        tier,
        icon_color,
        Style::default().fg(icon_color),
        0,
        indent as u16,
        marks,
    ));
    // The slot is the heading's own fold control: a heading is folded rather
    // than entered, so the click that lands on the mark is the click that folds
    // what the mark says.
    controls.push(SessionRowControl {
        line: 0,
        column: indent as u16,
        width: crate::row_marks::MARK_COLS,
        intent: Intent::OpenSelectedSession,
    });
    spans.push(Span::styled(" ", blank));
    // What the row's right end holds, from the rule to the edge: the running
    // count the desktop keeps at the far edge, and the buttons. Both are
    // measured before the name is cut, so the name gives way rather than the
    // shape of the row.
    let buttons = heading_buttons(row);
    let running = heading_running_label(row, app, strings);
    let running_block = if running.is_empty() {
        0
    } else {
        // The running count keeps a column of air on each side: against the rule
        // it is punctuation, and against the buttons it is a different kind of
        // fact.
        display_width(&running) + 2
    };
    let right = running_block + icon_cluster_width(buttons.len()) + ROW_CONTROL_AIR;
    // The name gives way before the shape of the heading does: a heading whose
    // rule ran out is still a heading, but a heading without its name is not.
    let label_width = width
        .saturating_sub(indent)
        .saturating_sub(HEADING_ROW_LEAD)
        .saturating_sub(right)
        .saturating_sub(4)
        .max(1);
    let label = truncate_to_width(&row.label, label_width, "…");
    spans.push(Span::styled(label.clone(), label_style));
    let used = indent + HEADING_ROW_LEAD + display_width(&label);
    // One column of air, then the rule to the far end of the list in the dim
    // step: a rule is punctuation between sections, not a line of the page.
    //
    // It stops a column short of what follows it, which is what the trailing
    // block's air is: a rule that ran into the buttons would read as the row's
    // own edge, and the last dash would look like part of the button it touches.
    let rule_width = width.saturating_sub(used + 1 + right);
    if rule_width > 0 {
        spans.push(Span::styled(" ", blank));
        spans.push(Span::styled(
            crate::glyphs::heading_rule(tier).repeat(rule_width),
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    spans.push(Span::styled(" ".repeat(ROW_CONTROL_AIR), blank));
    // The trailing mark is the desktop's "N running", which it keeps at the
    // row's far edge and hides under the pointer. A terminal has no hover, so it
    // is drawn beside the badge instead of under the buttons.
    // Where the trailing block starts, and where each piece of it leaves the
    // cursor: the buttons publish the rect a click lands on, so a column that
    // did not follow the running count would be a button the pointer cannot hit.
    let mut column = width.saturating_sub(right - ROW_CONTROL_AIR);
    if !running.is_empty() {
        spans.push(Span::styled(
            format!(" {running} "),
            Style::default().fg(theme.roles.gray),
        ));
        column += running_block;
    }
    for (index, (icon, intent)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ".repeat(ROW_CONTROL_GAP), blank));
            column += ROW_CONTROL_GAP;
        }
        let lit = lit_control == Some(*intent);
        let color = if lit {
            theme.roles.accent_user
        } else {
            theme.roles.gray_bright
        };
        let style = if lit {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };
        let mut slot = row_icon_slot(
            *icon,
            icon_drawable(app, color),
            tier,
            color,
            style,
            0,
            column as u16,
            marks,
        );
        spans.append(&mut slot);
        controls.push(SessionRowControl {
            line: 0,
            column: column as u16,
            width: crate::row_marks::MARK_COLS,
            intent: *intent,
        });
        column += icon_button_width();
    }
    Line::from(spans)
}

/// The buttons a heading offers: the two the desktop reveals on hover, minus the
/// new-session button on a folder no project owns — because only such a folder
/// can hold a session.
fn heading_buttons(
    row: &vibex_desktop_model::AgentSidebarRow,
) -> Vec<(crate::sidebar_icons::Icon, Intent)> {
    let folder_without_scope =
        row.kind == vibex_desktop_model::AgentSidebarRowKind::Folder && row.project_id.is_empty();
    HEADING_ROW_ACTIONS
        .iter()
        .filter(|(icon, _)| !(folder_without_scope && *icon == crate::sidebar_icons::Icon::New))
        .copied()
        .collect()
}

/// The verbs a heading's buttons carry, in the order they are drawn.
///
/// The keyboard asks this rather than reading the icons, so the button it
/// reaches and the button on screen are one list.
pub(crate) fn heading_button_intents(row: &vibex_desktop_model::AgentSidebarRow) -> Vec<Intent> {
    heading_buttons(row)
        .into_iter()
        .map(|(_, intent)| intent)
        .collect()
}

/// How many of a project's sessions are running, in the desktop's own words.
///
/// A project's mark, not a folder's: the desktop keeps it on the project row,
/// and a folder's own sessions are a slice of the project's — the same number
/// printed twice, one of them about work the folder may not even hold. The
/// folder's badge beside the buttons already says how much the folder holds.
fn heading_running_label(
    row: &vibex_desktop_model::AgentSidebarRow,
    app: &App,
    strings: Strings,
) -> String {
    if row.kind != vibex_desktop_model::AgentSidebarRowKind::Project {
        return String::new();
    }
    let count = app.sidebar_running_count(&row.project_id, &row.workspace_id);
    if count == 0 {
        return String::new();
    }
    format!("{count} {}", strings.sidebar_running())
}

/// What a session row's right end says while the reader is not on it.
///
/// Built for every row before anything is drawn, because the widest answer is
/// what every row reserves. The order is the desktop's status column's, mark for
/// mark: the pause, then the state's own mark, then the hour or the word for a
/// state that has no hour, then the marks that qualify the row — something new,
/// a failure, and the pin that owns the trailing slot.
struct SessionStatusColumn {
    spans: Vec<Span<'static>>,
    width: usize,
    /// The icons the column drew, and the column each starts in, relative to
    /// the column's own left edge. Each has reserved two cells and drawn the
    /// character it falls back to; the row places the desktop's own picture over
    /// those cells where the terminal can hold one, which is what keeps a pin
    /// the shape the desktop draws rather than a star this client invented.
    icons: Vec<(usize, crate::sidebar_icons::Icon, ratatui::style::Color)>,
}

#[allow(clippy::too_many_arguments)]
fn session_status_column(
    row: &vibex_desktop_model::AgentSidebarRow,
    session: Option<&vibex_core::AgentSession>,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    now_ms: i64,
) -> SessionStatusColumn {
    use vibex_core::AgentSessionState;
    let tier = app.glyph_tier();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut icons: Vec<(usize, crate::sidebar_icons::Icon, ratatui::style::Color)> = Vec::new();
    let Some(session) = session else {
        return SessionStatusColumn {
            spans,
            width: 0,
            icons,
        };
    };
    let state = session.state;
    let awaiting = state == AgentSessionState::NeedsInput;
    // Only a *running* turn suppresses the hour. A session still starting up
    // keeps the ring — the Agent has been asked and has not answered — and says
    // `INIT` beside it, which is the desktop's own reading: the ring is the
    // state's mark and the word is the row's.
    let generating = state == AgentSessionState::Running;
    let failed = state == AgentSessionState::Error;
    let scheduled = app.next_scheduled_message_at(&session.id);
    let unread = app.session_is_unread(&session.id);
    let paused = app.auto_continue.is_paused(&session.id);
    fn push(spans: &mut Vec<Span<'static>>, text: String, color: ratatui::style::Color) {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    /// One of the desktop's icons, in the two cells a picture needs.
    ///
    /// The cells are reserved blank where the terminal can hold a picture: the
    /// mark is drawn *over* them, and a character left underneath would show
    /// through the transparent parts of the icon — a question mark behind the
    /// circled question mark. Where the terminal cannot, the character is the
    /// mark, and it is what the reader gets.
    fn push_icon(
        spans: &mut Vec<Span<'static>>,
        icons: &mut Vec<(usize, crate::sidebar_icons::Icon, ratatui::style::Color)>,
        icon: crate::sidebar_icons::Icon,
        tier: crate::glyphs::GlyphTier,
        color: ratatui::style::Color,
        drawable: bool,
    ) {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        let column = spans_width(spans);
        let text = if drawable {
            " ".repeat(usize::from(crate::row_marks::MARK_COLS))
        } else {
            format!(
                "{:<width$}",
                icon.glyph(tier),
                width = usize::from(crate::row_marks::MARK_COLS)
            )
        };
        icons.push((column, icon, color));
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    // The pause leads, the way the desktop's column leads with it: it is the one
    // mark that says the session's own automation is held back.
    if paused {
        push_icon(
            &mut spans,
            &mut icons,
            crate::sidebar_icons::Icon::Pause,
            tier,
            theme.roles.gray,
            icon_drawable(app, theme.roles.gray),
        );
    }
    // The state's own mark, in the desktop's own order. A session parked on the
    // reader answers them, so it outranks everything; then the clock of a
    // message waiting for its hour; then the mark the state itself carries. A
    // row with something unread, a failure, or nothing to report draws none of
    // them, which is what keeps the dots below unambiguous.
    if awaiting {
        push_icon(
            &mut spans,
            &mut icons,
            crate::sidebar_icons::Icon::Attention,
            tier,
            theme.roles.warning,
            icon_drawable(app, theme.roles.warning),
        );
    } else if scheduled.is_some() {
        push_icon(
            &mut spans,
            &mut icons,
            crate::sidebar_icons::Icon::Clock,
            tier,
            theme.roles.gray,
            icon_drawable(app, theme.roles.gray),
        );
    } else if !unread && !failed && state != AgentSessionState::Idle {
        // The ring turns only while the Agent is being asked to work, and it is
        // the one mark that moves: a session continuing itself wears it in the
        // success colour, which is how the desktop says auto-continue without a
        // word. A session that is *not* continuing itself wears the theme's
        // primary — white in the shipped dark themes — because that is the
        // desktop's own pairing: `theme().primary` while the reader has
        // auto-continue off, `theme().success` while it is on. The running rail
        // (`accent_running`) is a transcript hue, not the sidebar's mark, and a
        // blue ring beside the list's green one would say the two working
        // sessions differ in a way the desktop does not. A session that is over
        // — archived, or closed — wears the quiet dot instead, which is the mark
        // the desktop gives it. Which states turn the ring lives with the list
        // ([`crate::sessions::session_state_turns_ring`]), because the clock
        // that steps it has to ask the same question.
        if crate::sessions::session_state_turns_ring(state) {
            let frames = crate::glyphs::activity_ring_frames(tier);
            let phase = app.animation_phase / crate::glyphs::ACTIVITY_RING_TICKS_PER_FRAME;
            let frame = crate::glyphs::frame_at(frames, phase, 1).to_string();
            let color = if app.auto_continue.is_enabled(&session.id) {
                theme.roles.accent_success
            } else {
                theme.roles.accent
            };
            push(&mut spans, frame, color);
        } else if matches!(
            state,
            AgentSessionState::Archived | AgentSessionState::Closed
        ) {
            push(
                &mut spans,
                crate::glyphs::state_marker("idle", tier).to_string(),
                theme.roles.gray_dim,
            );
        }
    }
    // The countdown is this client's own: the desktop's ring can say *that* a
    // session will continue itself, and a cell can say when.
    if let Some(seconds) = app.auto_continue.countdown_seconds(&session.id) {
        push(
            &mut spans,
            format!("↻{seconds}"),
            theme.roles.accent_attention,
        );
    }
    // The hour, or the word for a state that has no hour worth reading. The
    // desktop gates the whole pair on the row being neither pinned, nor parked
    // on the reader, nor working: each of those has a mark of its own to the
    // right, and a time beside it would be a second answer to the same question.
    let chip = match state {
        AgentSessionState::Initializing => Some(strings.sidebar_state_initializing()),
        AgentSessionState::Archived => Some(strings.sidebar_state_archived()),
        _ => None,
    };
    if !row.pinned && !awaiting && !generating {
        if let Some(chip) = chip {
            push(&mut spans, chip.to_string(), theme.roles.gray);
        } else if !unread && !failed && !paused {
            push(
                &mut spans,
                session_time_label(session, scheduled, strings, now_ms),
                theme.roles.gray,
            );
        }
    }
    if unread {
        push(
            &mut spans,
            crate::glyphs::unread_marker(tier).to_string(),
            theme.roles.accent_user,
        );
    }
    if failed {
        push(
            &mut spans,
            crate::glyphs::state_marker("failed", tier).to_string(),
            theme.roles.danger,
        );
    }
    if row.pinned {
        push_icon(
            &mut spans,
            &mut icons,
            crate::sidebar_icons::Icon::Pin,
            tier,
            theme.roles.warning,
            icon_drawable(app, theme.roles.warning),
        );
    }
    let width = spans_width(&spans);
    SessionStatusColumn {
        spans,
        width,
        icons,
    }
}

/// When a session last said anything, or how long until its message goes, in
/// the desktop's own words.
///
/// The desktop reads a row's hour as a wall clock — `14:32` today, the word for
/// yesterday, a count of days, then weeks, and a date once it is older than a
/// month — so the list separates "this morning" from "last week" by the clock
/// the reader already knows rather than by an age they have to subtract. A
/// session with a message waiting reads as the time left to wait instead, which
/// is the one question that row is asking.
fn session_time_label(
    session: &vibex_core::AgentSession,
    scheduled_at_ms: Option<i64>,
    strings: Strings,
    now_ms: i64,
) -> String {
    if let Some(at_ms) = scheduled_at_ms {
        return crate::schedule::format_schedule_countdown(at_ms.saturating_sub(now_ms));
    }
    relative_time_label(session.last_message_at_ms, now_ms, strings)
}

/// A session's row: the Agent's mark, its title, what it last did, and the
/// column at its right end.
///
/// The left edge belongs to the Agent that answers for the row — the same mark
/// the desktop puts there — and the right end belongs to the hour, or to the
/// buttons while the reader is on the row. It is the desktop's layout, cell for
/// cell: a terminal has no room to invent a third arrangement.
#[allow(clippy::too_many_arguments)]
fn session_row_lines(
    row: &vibex_desktop_model::AgentSidebarRow,
    branch: &[bool],
    session: Option<&vibex_core::AgentSession>,
    selected: bool,
    hovered: bool,
    lit_control: Option<Intent>,
    status: Option<&SessionStatusColumn>,
    status_column: usize,
    width: usize,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    controls: &mut Vec<SessionRowControl>,
    marks: &mut Vec<SessionRowMark>,
) -> Vec<Line<'static>> {
    let tier = app.glyph_tier();
    // A selected or hovered row is a band rather than a coloured sentence: the
    // band covers the second line too, so the row reads as one object.
    let band = (selected || hovered).then_some(theme.roles.surface_highlight);
    let paint = |colour: ratatui::style::Color| match band {
        Some(band) => Style::default().fg(colour).bg(band),
        None => Style::default().fg(colour),
    };
    let title_style = if selected {
        paint(theme.roles.foreground).add_modifier(Modifier::BOLD)
    } else {
        paint(theme.roles.foreground)
    };
    let dim_style = paint(if selected {
        theme.roles.text_secondary
    } else {
        theme.roles.gray_dim
    });
    let blank_style = match band {
        Some(band) => Style::default().bg(band),
        None => Style::default(),
    };
    let bar_style = paint(theme.roles.accent_user).add_modifier(Modifier::BOLD);
    let guide_style = paint(theme.roles.border);
    let indent = usize::from(row.depth) * SESSION_ROW_INDENT;
    // The Agent's mark, in the slot every row opens with. A terminal that cannot
    // hold a picture, or an Agent with no brand mark, leaves the cells blank and
    // names the Agent after the title instead — the answer this branch exists to
    // give, and never a half-drawn one.
    let identity = session.map(|session| app.session_agent_identity(session));
    let mark_color = theme.roles.foreground;
    let drawable = app.terminal_images.supports_pixels()
        && crate::theme::color_rgb(mark_color).is_some()
        && identity
            .as_deref()
            .is_some_and(|identity| vibex_brands::agent_mark(identity).is_some());
    // The left edge: a delegated row draws the branch it hangs from, where a
    // root row draws the bar that says it is the reader's. A child never draws
    // the bar — the band is what says it is selected — so the guide owns the
    // columns the bar would have taken, and the elbow lands exactly where the
    // Agent mark of the session that owns it sits.
    //
    // Both runs are the two columns the bar and its air occupy, and the second
    // line draws the same one the first did: the rail down a selected row's left
    // edge is unbroken, and a delegated row's trunk reaches the line under its
    // title rather than stopping at it.
    let (lead, trail) = if row.delegated {
        branch_lead(branch, indent, tier)
    } else {
        let bar = if selected {
            crate::glyphs::accent_bar(tier)
        } else {
            " "
        };
        let run = format!("{}{bar} ", " ".repeat(indent));
        (run.clone(), run)
    };
    let lead_style = if row.delegated {
        guide_style
    } else {
        bar_style
    };
    let mut spans = vec![Span::styled(lead, lead_style)];
    if drawable && let Some(identity) = identity.as_deref() {
        marks.push(SessionRowMark {
            line: 0,
            column: row_icon_column(indent),
            mark: crate::row_marks::Mark::Agent(identity.to_string()),
            color: mark_color,
        });
    }
    spans.push(Span::styled(
        " ".repeat(usize::from(crate::row_marks::MARK_COLS)),
        blank_style,
    ));
    spans.push(Span::styled(" ", blank_style));
    // The buttons the row offers while the reader is on it, and the room they
    // need at the row's right end whether or not they are drawn. The trailing
    // button is the last to go when the terminal is too narrow to hold the five,
    // so delete is never the one that disappears.
    let buttons = session_row_buttons(width, indent);
    let action_block = if buttons.is_empty() {
        0
    } else {
        icon_cluster_width(buttons.len()) + ROW_CONTROL_AIR
    };
    let reserved = status_column.max(action_block);
    let text_width = width
        .saturating_sub(indent)
        .saturating_sub(SESSION_ROW_LEAD)
        .saturating_sub(reserved);
    // The name a session that exists only to wait for its scheduled message
    // comes from that message until the send actually leaves the queue, so the
    // manager's default `<agent> session` title never labels the wait.
    let label = session
        .map(|session| app.session_row_title(session))
        .unwrap_or_else(|| row.label.clone());
    let title = truncate_to_width(&label, text_width, "…");
    let mut text_used = display_width(&title);
    spans.push(Span::styled(title, title_style));
    // Who is answering, when the title has left room for it and the row has no
    // mark to say it with. Which Agent a session runs on is how a reader tells
    // two similarly named rows apart.
    if !drawable {
        let subtitle = session
            .map(|session| app.session_agent_label(session))
            .unwrap_or_default();
        if !subtitle.is_empty() && text_used + 3 + display_width(&subtitle) <= text_width {
            spans.push(Span::styled(" · ", dim_style));
            let subtitle = truncate_to_width(&subtitle, text_width - text_used - 3, "…");
            text_used += 3 + display_width(&subtitle);
            spans.push(Span::styled(subtitle, dim_style));
        }
    }
    spans.push(Span::styled(
        " ".repeat(text_width.saturating_sub(text_used)),
        blank_style,
    ));
    // A session that delegated work used to carry a disclosure after its title.
    // It is gone: the branch now opens from the row itself — the cursor onto it,
    // or a click — so the mark that only said "there is something under this"
    // was a control the reader had to aim at to do what the row already does.
    // The columns it reserved go back to the title.
    // The row's own buttons, while the row is the reader's: the selected row, or
    // the one the pointer is on. They take the status column's place at the
    // right edge, and the hour is not drawn under them: two answers to "when"
    // and "act on this" in one column would be one too many.
    let active = selected || hovered;
    if active && !buttons.is_empty() {
        let start = indent + SESSION_ROW_LEAD + text_width;
        let pad = reserved.saturating_sub(action_block) + ROW_CONTROL_AIR;
        spans.push(Span::styled(" ".repeat(pad), blank_style));
        let mut column = start + pad;
        for (index, (icon, intent)) in buttons.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(" ".repeat(ROW_CONTROL_GAP), blank_style));
                column += ROW_CONTROL_GAP;
            }
            // At rest the cluster is chrome, the way the desktop's own is: a
            // row of six coloured marks would shout at a reader who is only
            // reading the list. Under the pointer — or under the keyboard, once
            // `←`/`→` have stepped onto the cluster — the button takes the colour
            // of the verb it is: delete is the danger colour exactly when it is
            // about to be pressed, and that colour is also what the picture is
            // drawn in. The mark is never drawn *in* the wash behind it: a
            // picture has no ink to fall back on, and a button rastered in the
            // terminal's own background is a button that disappears.
            let lit = lit_control == Some(*intent);
            let colour = if lit {
                action_colour(*icon, theme, row.pinned)
            } else {
                resting_action_colour(*icon, theme, row.pinned)
            };
            let style = if lit {
                paint(colour).add_modifier(Modifier::BOLD)
            } else {
                paint(colour)
            };
            let mut slot = row_icon_slot(
                *icon,
                icon_drawable(app, colour),
                tier,
                colour,
                style,
                0,
                column as u16,
                marks,
            );
            spans.append(&mut slot);
            controls.push(SessionRowControl {
                line: 0,
                column: column as u16,
                width: crate::row_marks::MARK_COLS,
                intent: *intent,
            });
            column += icon_button_width();
        }
    } else if let Some(status) = status {
        let pad = reserved.saturating_sub(status.width);
        spans.push(Span::styled(" ".repeat(pad), blank_style));
        // The status column keeps the row's band, so a selected row is one
        // object rather than a band with a bare patch at its right end.
        spans.extend(
            status
                .spans
                .iter()
                .map(|span| Span::styled(span.content.clone(), span.style.patch(blank_style))),
        );
        // The marks the column drew as characters are the desktop's own icons
        // wherever the terminal can hold a picture, which is what keeps a pinned
        // row wearing the pin the desktop draws rather than a star.
        let base = indent + SESSION_ROW_LEAD + text_width + pad;
        for (offset, icon, color) in &status.icons {
            if !icon_drawable(app, *color) {
                continue;
            }
            marks.push(SessionRowMark {
                line: 0,
                column: (base + offset) as u16,
                mark: crate::row_marks::Mark::Chrome(*icon),
                color: *color,
            });
        }
    }
    let mut lines = vec![Line::from(spans)];
    // The second line is what the session last did, and nothing else: the
    // workspace is the heading the row already sits under, so a row with nothing
    // to report stays one line rather than repeating its directory.
    if let Some(session) = session {
        let pending = session.state == vibex_core::AgentSessionState::NeedsInput;
        // What the line says, and whether it is news the reader has to act on: a
        // session blocked on them says so even when this client has not seen it
        // say anything yet.
        let (text, actionable) = match app.session_echo(&session.id) {
            Some(echo) if pending => (format!("{}: {echo}", strings.pending()), true),
            Some(echo) => (echo.to_string(), false),
            None if pending => (strings.approval_waiting().to_string(), true),
            None => (String::new(), false),
        };
        if !text.is_empty() {
            let budget = width.saturating_sub(indent + SESSION_ROW_LEAD);
            let style = if actionable {
                paint(theme.roles.warning)
            } else {
                dim_style
            };
            // The mark under the title stands in the column the title's own mark
            // took: the same guide comes first, its elbow's column carried down
            // as a trunk where the row has a sibling left, and the text then
            // opens under the title it belongs to rather than under the mark.
            let text = truncate_to_width(&text, budget, "…");
            // What the echo does not reach keeps the band: the row is one
            // object, so its second line is as wide as its first rather than
            // ending where the sentence happened to.
            let tail = " ".repeat(budget.saturating_sub(display_width(&text)));
            lines.push(Line::from(vec![
                Span::styled(trail, lead_style),
                Span::styled(" ".repeat(SESSION_ROW_LEAD - 2), blank_style),
                Span::styled(text, style),
                Span::styled(tail, blank_style),
            ]));
        }
    }
    lines
}

/// The buttons a session row offers, dropping the leading ones a narrow
/// terminal cannot hold.
///
/// The title keeps two thirds of the row whatever else is drawn, so the cluster
/// is cut from the front: the verbs a reader is most likely to want on a narrow
/// screen are the ones nearest the title's end, and delete — the one that cannot
/// be undone — is the last to go.
fn session_row_buttons(width: usize, indent: usize) -> Vec<(crate::sidebar_icons::Icon, Intent)> {
    let available = width
        .saturating_sub(indent)
        .saturating_sub(SESSION_ROW_LEAD)
        .min(width / 3)
        .saturating_sub(ROW_CONTROL_AIR);
    let mut buttons = SESSION_ROW_ACTIONS.to_vec();
    while !buttons.is_empty() && icon_cluster_width(buttons.len()) > available {
        buttons.remove(0);
    }
    buttons
}

/// The verbs a session row's buttons carry at this width, in the order they are
/// drawn.
///
/// The keyboard asks this with the columns the frame last drew the list in, so
/// the button it reaches is the button on screen: a terminal too narrow to hold
/// the whole cluster hides the same leading verbs from both.
pub(crate) fn session_row_button_intents(width: usize, indent: usize) -> Vec<Intent> {
    session_row_buttons(width, indent)
        .into_iter()
        .map(|(_, intent)| intent)
        .collect()
}

/// The colour a session row's button takes while the pointer is on it.
///
/// Delete is the danger colour and archive is the attention one, which is the
/// distinction a reader has to be able to make without reading: everything else
/// acts on the session, and those two end it or put it away.
fn action_colour(
    icon: crate::sidebar_icons::Icon,
    theme: &TuiTheme,
    pinned: bool,
) -> ratatui::style::Color {
    match icon {
        crate::sidebar_icons::Icon::Delete => theme.roles.danger,
        crate::sidebar_icons::Icon::Archive => theme.roles.accent_attention,
        crate::sidebar_icons::Icon::Resume => theme.roles.accent_success,
        crate::sidebar_icons::Icon::Pin if pinned => theme.roles.warning,
        _ => theme.roles.accent_user,
    }
}

/// The colour a session row's button wears while the reader is elsewhere.
///
/// Chrome, with one exception the desktop makes too: a pinned session's pin
/// stays in the warning colour, because it is not a verb the reader is about to
/// run but the state the row is already in.
fn resting_action_colour(
    icon: crate::sidebar_icons::Icon,
    theme: &TuiTheme,
    pinned: bool,
) -> ratatui::style::Color {
    match icon {
        crate::sidebar_icons::Icon::Pin if pinned => theme.roles.warning,
        _ => theme.roles.gray_bright,
    }
}

/// The width in cells of a run of spans.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| display_width(&span.content)).sum()
}

/// Cut a run of spans to `budget` columns, dropping what does not fit.
///
/// The span that runs out of room is truncated rather than dropped, so a long
/// path keeps its head and says it was cut; anything after it is gone.
fn trim_spans(spans: Vec<Span<'static>>, budget: usize) -> Vec<Span<'static>> {
    let mut used = 0usize;
    let mut kept = Vec::new();
    for span in spans {
        let width = display_width(&span.content);
        if used + width <= budget {
            used += width;
            kept.push(span);
            continue;
        }
        let room = budget.saturating_sub(used);
        if room > 1 {
            kept.push(Span::styled(
                truncate_to_width(&span.content, room, "…"),
                span.style,
            ));
        }
        break;
    }
    kept
}

/// The mark beside a session in a list that is not the session list: a shape
/// per state family, so a monochrome terminal can tell a failed session from a
/// running one.
///
/// The session list itself no longer uses this: its rows carry the desktop's
/// own status column, where the state is a ring, a question mark, a clock or a
/// dot. The dock that lists a session's delegated workers is a menu rather than
/// a mirror of the desktop, and keeps the one-character shapes it was built
/// with.
fn session_state_mark(
    state: vibex_core::AgentSessionState,
    tier: crate::glyphs::GlyphTier,
) -> &'static str {
    match state {
        vibex_core::AgentSessionState::Running => crate::glyphs::state_marker("running", tier),
        vibex_core::AgentSessionState::NeedsInput => crate::glyphs::state_marker("waiting", tier),
        vibex_core::AgentSessionState::Error => crate::glyphs::state_marker("failed", tier),
        vibex_core::AgentSessionState::Archived => crate::glyphs::state_marker("archived", tier),
        vibex_core::AgentSessionState::Initializing => crate::glyphs::diamond_dotted(tier),
        // A legacy console has one `o`, and the unread mark is already wearing
        // it: an idle session keeps the dot its row used before rather than
        // saying "something new" with the same shape.
        vibex_core::AgentSessionState::Idle => match tier {
            crate::glyphs::GlyphTier::Full => crate::glyphs::diamond_hollow(tier),
            crate::glyphs::GlyphTier::Legacy => crate::glyphs::state_marker("idle", tier),
        },
        _ => crate::glyphs::diamond_hollow(tier),
    }
}

/// The colour of a state's mark, for the lists that draw one per state.
fn state_marker_colour(
    state: vibex_core::AgentSessionState,
    theme: &TuiTheme,
) -> ratatui::style::Color {
    match state {
        vibex_core::AgentSessionState::Running => theme.roles.accent_running,
        vibex_core::AgentSessionState::Error => theme.roles.danger,
        vibex_core::AgentSessionState::NeedsInput => theme.roles.accent_attention,
        _ => theme.roles.gray_dim,
    }
}

/// When a session last said anything, the way the desktop's list reads it.
///
/// Deliberately a clock rather than an age: today is the reader's own wall
/// clock, yesterday is the word for it, and only past that does the label count
/// — days, then weeks, then a date. A reader scanning the column is asking
/// "when", and `14:32` answers it without the arithmetic step `3h` asks for. The
/// comparison is by calendar date rather than by elapsed hours, which is what
/// makes `23:50` read as yesterday at `00:10`.
///
/// A timestamp the clock cannot resolve, or one in the future, reads as today:
/// a clock that moved backwards is not news, and a blank column would be worse
/// than the hour the runtime reported.
fn relative_time_label(at_ms: i64, now_ms: i64, strings: Strings) -> String {
    let local = |ms: i64| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .map(|timestamp| timestamp.with_timezone(&chrono::Local).naive_local())
    };
    let (Some(then), Some(now)) = (local(at_ms), local(now_ms)) else {
        return String::new();
    };
    let days = now.date().signed_duration_since(then.date()).num_days();
    if days <= 0 {
        return then.format("%H:%M").to_string();
    }
    if days == 1 {
        return strings.sidebar_yesterday().to_string();
    }
    if days < 7 {
        return format!("{days}{}", strings.time_days_ago());
    }
    if days < 30 {
        return format!("{}{}", days / 7, strings.time_weeks_ago());
    }
    then.format("%m/%d").to_string()
}

fn session_state_label(state: vibex_core::AgentSessionState, strings: Strings) -> &'static str {
    match state {
        vibex_core::AgentSessionState::Running => strings.running(),
        vibex_core::AgentSessionState::NeedsInput => strings.approval_title(),
        vibex_core::AgentSessionState::Error => strings.failed(),
        vibex_core::AgentSessionState::Archived => strings.archived(),
        vibex_core::AgentSessionState::Initializing => strings.connecting(),
        _ => strings.idle(),
    }
}

/// The fields of one session's detail card, in the order they are shown.
///
/// Only what the runtime actually publishes appears. A field the client cannot
/// answer is omitted rather than shown as a dash, because a card that says
/// "Model —" tells the reader nothing and costs a row.
fn session_card_fields(
    app: &App,
    session: &vibex_core::AgentSession,
    strings: Strings,
) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        (strings.session_card_id(), session.id.as_str().to_string()),
        (strings.session_workspace(), session.workspace_root.clone()),
        (
            strings.session_state(),
            session_state_label(vibex_ui::agent::display_state(session), strings).to_string(),
        ),
        (strings.session_card_agent(), session.agent_id.to_string()),
    ];
    // Where the session sits in the ownership tree, when the reader can act on
    // it: a child's card names the session it was delegated by, which is what
    // makes the card the second way back up.
    if let Some(parent) = app.parent_of_session(&session.id)
        && let Some(title) = app.session_display_title(&parent)
    {
        fields.push((strings.session_card_parent(), title));
    }
    // The model is only known for the session whose runtime selection has been
    // loaded, which is the open one.
    if app.selected_session_id() == Some(&session.id)
        && let Some(selection) = app.agent.state.runtime_selection.value.as_ref()
        && let Some(model) = selection.effective.model.model_id()
    {
        fields.push((strings.session_card_model(), model.to_string()));
    }
    fields.push((
        strings.session_card_created(),
        crate::text::format_utc_timestamp(session.created_at_ms),
    ));
    fields.push((
        strings.session_card_updated(),
        crate::text::format_utc_timestamp(session.updated_at_ms),
    ));
    if session.last_message_at_ms > 0 {
        fields.push((
            strings.session_card_last_message(),
            crate::text::format_utc_timestamp(session.last_message_at_ms),
        ));
    }
    if app.selected_session_id() == Some(&session.id) {
        let messages = app.transcript.len();
        if messages > 0 {
            fields.push((strings.session_card_messages(), messages.to_string()));
            fields.push((
                strings.session_card_turns(),
                app.transcript.turn_count().to_string(),
            ));
        }
    }
    fields
}

/// The rendered lines of one session's detail card.
fn session_card_lines(
    app: &App,
    session: &vibex_core::AgentSession,
    strings: Strings,
    theme: &TuiTheme,
    width: usize,
) -> Vec<Line<'static>> {
    let indent = width.min(120) / 12 + 4;
    let label_width = 12usize;
    let value_width = width.saturating_sub(indent + label_width + 2).max(8);
    let label_style = Style::default().fg(theme.roles.gray_dim);
    let value_style = Style::default().fg(theme.roles.gray);
    let bar_style = Style::default().fg(theme.roles.accent_user);
    session_card_fields(app, session, strings)
        .into_iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(" ".repeat(indent.saturating_sub(2)), Style::default()),
                Span::styled(
                    crate::glyphs::accent_bar(app.glyph_tier()).to_string(),
                    bar_style,
                ),
                Span::styled(" ", Style::default()),
                Span::styled(format!("{label:<label_width$}"), label_style),
                Span::styled(
                    truncate_to_width(&compact_path(&value, value_width), value_width, "…"),
                    value_style,
                ),
            ])
        })
        .collect()
}

/// The copyable text of one session's detail card.
pub fn session_card_text(app: &App, session: &vibex_core::AgentSession) -> String {
    let strings = Strings::for_locale(app.settings.locale);
    session_card_fields(app, session, strings)
        .into_iter()
        .map(|(label, value)| format!("{label:<12} {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_management_view(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    match app.page {
        // Every configuration route draws the one centre; which module and
        // area are open is the centre's own state, set when the route was
        // taken.
        Page::Management
        | Page::Providers
        | Page::Agents
        | Page::Mcp
        | Page::Skills
        | Page::Prompts => render_config_center(frame, area, app, theme, strings),
        Page::Devices => render_devices(frame, area, app, theme, strings),
        Page::Settings => render_settings(frame, area, app, theme, strings),
        Page::Help => render_help(frame, area, app, theme, strings),
        _ => {}
    }
}

/// The right gutter: one tick per turn.
///
/// A tick stands for a turn — one the reader opened — in conversation order, and
/// every turn the viewport shows is lit, so the count of the reader's
/// conversation and their place in it are the same object, and the rail is a map
/// of the session rather than of the buffer. The turn the viewport starts on
/// wears the heavier tick: the lit window says what the reader can see, and the
/// heavier one says where in that window they are. Nothing else is drawn there:
/// a scrollbar in the same columns would be counted as turns, which is exactly
/// the mistake the rail exists to prevent. When there are more turns than rows
/// the ticks window around the active one, because an unwindowed rail would fold
/// many turns onto a single row.
fn render_gutter(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    let turns = app.transcript.turn_count();
    let tier = app.glyph_tier();
    let rows = usize::from(area.height);
    if area.width == 0 || rows == 0 || turns == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows);

    // The window of turns on screen, and the turn it starts on. The marker is
    // the lit window's first tick whenever the viewport starts on a turn; a
    // window that opens on the runtime's own startup rows lights from the first
    // turn down and marks none of them.
    let lit = app.transcript.visible_turns();
    let active = app.transcript.active_turn();
    // Window the ticks when there are more turns than rows, keeping the active
    // turn visible: an unwindowed rail would fold many turns onto one row.
    let window_start = if turns <= rows {
        0
    } else {
        active
            .unwrap_or(0)
            .saturating_sub(rows / 2)
            .min(turns - rows)
    };
    for row in 0..rows {
        let turn = window_start + row;
        if turn >= turns {
            lines.push(Line::from(""));
            continue;
        }
        let is_active = active == Some(turn);
        let is_lit = lit.as_ref().is_some_and(|turns| turns.contains(&turn));
        let (glyph, color) = if is_active {
            (
                crate::glyphs::timeline_tick_active(tier),
                theme.roles.accent_user,
            )
        } else if is_lit {
            // A turn the reader can see, but not the one the viewport starts
            // on: the same light, the lighter mark.
            (crate::glyphs::timeline_tick(tier), theme.roles.accent_user)
        } else {
            (crate::glyphs::timeline_tick(tier), theme.roles.gray_dim)
        };
        app.regions.turns.push((
            Rect {
                y: area.y + row as u16,
                height: 1,
                ..area
            },
            turn,
        ));
        lines.push(Line::from(Span::styled(glyph, Style::default().fg(color))));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The turn-status row: what the Agent is doing, right now.
///
/// This is the band that makes the interface feel alive. It is not part of the
/// transcript because it must never scroll away, and it says the same things
/// the desktop's line above its composer says: what is happening, how long it
/// has been happening, how many tools it has called, and how fast it is going.
/// An idle-but-connected session says so, rather than showing nothing and
/// leaving the reader unsure whether the client is still there.
///
/// The running mark is the desktop sidebar's: a highlight walking the ring of
/// the Braille cell, one step per animation tick, in the theme's primary — or
/// green when the session continues itself. Idle is not animated at all, so a
/// glance at the band cannot mistake "nothing is happening" for progress.
///
/// The label beside the mark wears the light the desktop's live timeline rows
/// wear: a soft band crossing the words, one lap every 1200 ms, for as long as
/// the work is in flight ([`crate::shimmer`]). It is the same statement the
/// spinner makes, on the surface a reader actually reads — and it is drawn only
/// on the running arm, because a band waiting on the reader is not work.
fn render_turn_status(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if !Bands::is_visible(area) {
        return;
    }
    let tier = app.glyph_tier();
    let phase = app.animation_phase();

    let running = app.turn_reads_running();
    let approvals = app.page_approval_count();
    let questions = app.page_elicitation_count();
    // A turn that is still streaming into the transcript reads as running even
    // after the runtime has stopped calling the session running: the last line
    // of an answer arriving is the turn, and the band must not go idle under it.
    let live = running || app.transcript_animating();

    let (icon, label, color, in_flight) = if approvals + questions > 0 {
        (
            crate::glyphs::diamond_filled(tier),
            format!(
                "{approvals} {} · {questions} {}",
                strings.approval_label(),
                strings.elicitation_title()
            ),
            theme.roles.accent_attention,
            false,
        )
    } else if live {
        (
            crate::glyphs::frame_at(
                crate::glyphs::activity_ring_frames(tier),
                phase,
                crate::glyphs::ACTIVITY_RING_TICKS_PER_FRAME,
            ),
            turn_label(app, strings),
            running_mark_colour(app, theme),
            true,
        )
    } else {
        // Idle is a fact, not an animation: a plain mark in the dim grey, so a
        // glance at the band cannot mistake "nothing is happening" for progress.
        (
            if tier == crate::glyphs::GlyphTier::Full {
                "·"
            } else {
                "."
            },
            strings.idle().to_string(),
            theme.roles.gray_dim,
            false,
        )
    };

    // One sentence, in the desktop's order: what the Agent is doing, how long
    // it has taken, how many tools it has called, what it has spent and how
    // fast it is writing. A band that is not about a running turn — idle, or
    // waiting on the reader — says only what it is; the measurements belong to
    // work that is actually happening.
    //
    // The label is the light's own width: it is cut to the columns left after
    // the mark, so a lap always completes over what the reader can see rather
    // than spending half of itself off the edge.
    let prefix = format!("{icon} ");
    let label = truncate_to_width(
        &label,
        usize::from(area.width).saturating_sub(display_width(&prefix)),
        "…",
    );
    let mut line = vec![Span::styled(prefix, Style::default().fg(color))];
    line.extend(crate::shimmer::spans(
        &label,
        theme.roles.gray,
        theme,
        phase,
        in_flight && app.settings.motion,
    ));
    let measure = Style::default().fg(theme.roles.gray_dim);
    let measure_part = |line: &mut Vec<Span<'static>>, text: String| {
        line.push(Span::styled(" · ", measure));
        line.push(Span::styled(text, measure));
    };
    if live {
        if let Some(elapsed) = app.turn_elapsed() {
            measure_part(&mut line, crate::turn::format_worked_duration(elapsed));
        }
        let readout = app.turn_readout();
        if let Some(tool_calls) = readout
            .map(crate::turn::TurnReadout::tool_calls)
            .filter(|count| *count > 0)
        {
            measure_part(&mut line, format!("{tool_calls} {}", strings.tool_calls()));
        }
        if let Some(rate) = readout.and_then(crate::turn::TurnReadout::tokens_per_second) {
            measure_part(&mut line, format!("{rate:.1} t/s"));
        }
    }
    // A narrow terminal keeps what the Agent is doing and drops the measurement
    // rather than wrapping a band that is one row tall.
    truncate_spans(&mut line, usize::from(area.width));
    frame.render_widget(Paragraph::new(Line::from(line)), area);
}

/// The ink the running mark wears.
///
/// One session has one running mark, and the desktop's sidebar already answers
/// what colour it is: the theme's primary while the Agent works, and green when
/// the session will continue itself. The band is the same session's mark on
/// another surface, so it says the same thing — a reader who has both open must
/// not have to read the preference off the session list to know whether the
/// turn will pick itself up.
///
/// A page with no session of its own cannot be continuing anything, so it keeps
/// the primary. In the shipped dark themes that primary is white.
fn running_mark_colour(app: &App, theme: &TuiTheme) -> ratatui::style::Color {
    match app.page_session_id() {
        Some(session_id) if app.auto_continue.is_enabled(session_id) => theme.roles.success,
        _ => theme.roles.accent,
    }
}

/// What the turn line calls the running turn.
///
/// The runtime's own activity line names the specific work when it has one —
/// `read src/net/upload.rs` is more use than `Calling tool` — and the phase is
/// what is left when it does not: the desktop's own vocabulary, so a reader who
/// uses both surfaces reads the same words. A send the runtime has not answered
/// yet has neither, and "Running" is the honest answer for it.
fn turn_label(app: &App, strings: Strings) -> String {
    if let Some(activity) = app.current_activity() {
        return activity;
    }
    match app.turn_readout().and_then(crate::turn::TurnReadout::phase) {
        Some(phase) => phase_label(phase, strings).to_string(),
        None => strings.running().to_string(),
    }
}

/// The word the interface uses for a turn's phase.
fn phase_label(phase: crate::turn::TurnPhase, strings: Strings) -> &'static str {
    match phase {
        crate::turn::TurnPhase::Preparing => strings.phase_preparing(),
        crate::turn::TurnPhase::Thinking => strings.phase_thinking(),
        crate::turn::TurnPhase::CallingTool => strings.phase_calling_tool(),
        crate::turn::TurnPhase::Generating => strings.phase_generating(),
        crate::turn::TurnPhase::WaitingForApproval => strings.phase_waiting_approval(),
    }
}

/// Trim a styled line to `width` columns, ellipsising where it is cut.
///
/// The turn line is a sentence with its measurements on the end, and they are
/// what a narrow terminal can do without: what the Agent is doing is worth more
/// than how fast it is doing it. The cut is made here rather than by the
/// paragraph widget so the last span keeps its own colour instead of the whole
/// line being re-styled or wrapped onto a row the band does not have.
fn truncate_spans(spans: &mut Vec<Span<'static>>, width: usize) {
    let mut used = 0usize;
    for (index, span) in spans.iter().enumerate() {
        let span_width = display_width(span.content.as_ref());
        if used + span_width <= width {
            used += span_width;
            continue;
        }
        let trimmed = truncate_to_width(span.content.as_ref(), width - used, "…");
        let style = span.style;
        spans.truncate(index);
        if !trimmed.is_empty() {
            spans.push(Span::styled(trimmed, style));
        }
        return;
    }
}

/// A transient one-line message above the composer.
fn render_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    _strings: Strings,
) {
    let Some(banner) = app.banner.as_ref() else {
        return;
    };
    let color = match banner.tone {
        BannerTone::Info => theme.roles.gray,
        BannerTone::Warning => theme.roles.warning,
        BannerTone::Danger => theme.roles.danger,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&banner.text, usize::from(area.width), "…"),
            Style::default().fg(color),
        ))),
        area,
    );
}

/// The `Alt+X` legends a band prints along its header, split into presses.
///
/// A band's hint line is one localized string — `Alt+↑↓ pick · Alt+E edit · …` —
/// because it is a sentence, and a sentence translates as a whole. The pointer
/// still needs each legend on its own, so the drawn text is read back at the
/// separators the same locales use, and each segment is measured where it lands
/// on screen. Splitting the *rendered* string rather than the catalogue keeps
/// the two in step: a locale that reorders or reword its legends gets hit rects
/// on the words it actually drew.
///
/// Returns the spans to draw; the press rects go into the frame's regions. A
/// segment whose text is not a key at all — the `type` a form's footer names —
/// is drawn but not pressable, so nothing pretends it is.
fn key_hint_legend(
    app: &mut App,
    theme: &TuiTheme,
    area: Rect,
    row: u16,
    text: &str,
) -> Vec<Span<'static>> {
    let style = Style::default().fg(theme.roles.gray_dim);
    let mut spans = Vec::new();
    let mut column = area.x;
    for (index, segment) in text.split(" · ").enumerate() {
        if index > 0 {
            let separator = " · ";
            spans.push(Span::styled(separator, style));
            column += display_width(separator) as u16;
        }
        let width = display_width(segment) as u16;
        // The band may be narrower than the sentence; a legend drawn past the
        // edge is clipped, and a hit rect over cells that are not on screen
        // would answer a click the reader cannot see themselves making. A
        // segment that starts on the last cell still gets that one cell — the
        // first character of its key is the part the reader aims at.
        let visible = width.min(area.right().saturating_sub(column));
        let chord = Chord::from_hint(segment.split_whitespace().next().unwrap_or(""))
            .filter(|_| visible > 0);
        spans.push(Span::styled(
            segment.to_string(),
            match chord {
                Some(chord) if app.hovered_key_hint == Some(chord) => {
                    Style::default().fg(theme.roles.gray_bright)
                }
                _ => style,
            },
        ));
        if let Some(chord) = chord {
            app.regions.key_hints.push(crate::app::KeyHintRegion {
                rect: Rect {
                    x: column,
                    y: row,
                    width: visible,
                    height: 1,
                },
                chord,
            });
        }
        column += width;
    }
    spans
}

/// The queued-message band.
///
/// Queued prompts are held back until the running turn ends, so they are shown
/// where they will be sent from rather than buried in the transcript.
fn render_queue_band(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let mut header = vec![Span::styled(
        format!(
            "{} {} {}",
            crate::glyphs::diamond_dotted(app.glyph_tier()),
            app.queued_messages.len(),
            strings.composer_queue()
        ),
        Style::default().fg(theme.roles.gray_dim),
    )];
    // The keys are only worth listing while the queue has something to act on,
    // and only while the cursor is in it. Each legend is its own press target,
    // so `Alt+E` can be clicked the way it can be pressed.
    if app.queue_selection.is_some() {
        let lead = display_width(&header[0].content) as u16 + 2;
        let spans = key_hint_legend(
            app,
            theme,
            Rect {
                x: area.x + lead,
                width: area.width.saturating_sub(lead),
                ..area
            },
            area.y,
            strings.queue_hint(),
        );
        header.extend(spans);
    }
    app.regions.queue = Some(Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    });
    let mut lines = vec![Line::from(header)];
    let visible = usize::from(area.height).saturating_sub(1);
    // Scroll the window so the cursor stays visible: a queue can be longer than
    // the three rows the band allows.
    let held = app.queued_for_active();
    let selected = app.queue_selection.unwrap_or(0);
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    // What each row is waiting on, right-aligned in a column of its own: a
    // message held for a turn and a message held for the clock read the same in
    // the text, and the difference is exactly what the right edge is for. The
    // column is measured from the rows that will be drawn, so a queue of
    // unscheduled messages spends no width on a clock.
    let now_ms = vibex_core::unix_timestamp_ms();
    let schedules = held
        .iter()
        .map(|at| {
            let message = &app.queued_messages[*at];
            message
                .scheduled_at_ms
                .map(|at_ms| app.schedule_status_label(at_ms, &message.session_id, now_ms))
        })
        .collect::<Vec<_>>();
    let schedule_column = schedules
        .iter()
        .flatten()
        .map(|label| display_width(label))
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) / 3);
    for (row, at) in held.iter().enumerate().skip(offset).take(visible) {
        app.regions.queue_rows.push(row);
        let message = &app.queued_messages[*at];
        let active = app.queue_selection == Some(row);
        let prefix = if active { "▸ " } else { "  " };
        let number = format!("#{} ", row + 1);
        let schedule = schedules
            .get(row)
            .and_then(Option::as_deref)
            .map(|label| truncate_to_width(label, schedule_column, "…"));
        let width = usize::from(area.width);
        // The columns are laid out exactly rather than left to the paragraph
        // widget: the schedule is right-aligned, and a row whose message grew
        // would otherwise push the clock off the edge it belongs on.
        let lead = display_width(prefix) + display_width(&number);
        let reserved = schedule
            .as_ref()
            .map_or(0, |label| display_width(label) + 2);
        let text = truncate_to_width(
            message.text.lines().next().unwrap_or_default(),
            width.saturating_sub(lead + reserved),
            "…",
        );
        let mut spans = vec![
            Span::styled(
                prefix.to_string(),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(number, Style::default().fg(theme.roles.gray_dim)),
            Span::styled(
                text.clone(),
                if active {
                    theme.selected()
                } else {
                    Style::default().fg(theme.roles.gray)
                },
            ),
        ];
        if let Some(schedule) = schedule {
            let pad = width.saturating_sub(lead + display_width(&text) + display_width(&schedule));
            spans.push(Span::styled(" ".repeat(pad), Style::default()));
            spans.push(Span::styled(
                schedule,
                if active {
                    Style::default().fg(theme.roles.accent_attention)
                } else {
                    Style::default().fg(theme.roles.gray_dim)
                },
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The dock: agents, the plan and the held queue, in one panel above the
/// composer.
///
/// It is a band rather than an overlay because it answers a question the reader
/// asks *while* typing -- is anything still running? -- and an overlay would
/// make them leave the thing they are doing to find out.
fn render_dock_band(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let rows = app.dock_rows();
    let focused = app.dock_is_focused();
    app.regions.dock = Some(Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    });
    let mut header = vec![Span::styled(
        format!(
            "{} {}",
            crate::glyphs::diamond_dotted(app.glyph_tier()),
            strings.dock_title()
        ),
        Style::default()
            .fg(theme.roles.accent_user)
            .add_modifier(Modifier::BOLD),
    )];
    if focused {
        let lead = display_width(&header[0].content) as u16 + 2;
        let spans = key_hint_legend(
            app,
            theme,
            Rect {
                x: area.x + lead,
                width: area.width.saturating_sub(lead),
                ..area
            },
            area.y,
            strings.dock_hint(),
        );
        header.extend(spans);
    }
    let mut lines = vec![Line::from(header)];
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.dock_empty()),
            Style::default().fg(theme.roles.gray_dim),
        )));
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    }

    let visible = usize::from(area.height).saturating_sub(1);
    // The rows can shrink under the cursor as work finishes, so the selection
    // is clamped here rather than trusted.
    let selected = app.dock_selection.unwrap_or(0).min(rows.len() - 1);
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    // One row is given up to the "more" line when the rest does not fit, so the
    // number it reports counts what is really left out.
    let capacity = if rows.len().saturating_sub(offset) > visible {
        visible.saturating_sub(1)
    } else {
        visible
    };
    let hidden = rows.len().saturating_sub(offset + capacity);
    let shown = rows.iter().enumerate().skip(offset).take(capacity);
    for (index, row) in shown {
        app.regions.dock_rows.push(index);
        let active = focused && index == selected;
        let base = if active {
            theme.selected()
        } else {
            theme.base()
        };
        match row {
            crate::app::DockRow::Team { label, .. } => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}",
                        truncate_to_width(label, usize::from(area.width).saturating_sub(2), "…")
                    ),
                    base,
                )));
            }
            crate::app::DockRow::Header { section, count } => {
                let folded = app.dock_collapsed.contains(section);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "  {} ",
                            crate::glyphs::disclosure(!folded, app.glyph_tier())
                        ),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(
                        format!("{} {count}", section.label(strings)),
                        Style::default()
                            .fg(theme.roles.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            crate::app::DockRow::Agent {
                label,
                status,
                summary,
                ..
            } => {
                let (marker, tone) = dock_status_marker(*status, app.animation_phase(), theme, app);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", if active { "▸" } else { " " }),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(format!("{marker} "), Style::default().fg(tone)),
                    Span::styled(truncate_to_width(label, 18, "…"), base),
                    Span::styled(
                        format!(
                            "  {}",
                            truncate_to_width(
                                summary,
                                usize::from(area.width).saturating_sub(28),
                                "…"
                            )
                        ),
                        if active {
                            base
                        } else {
                            Style::default().fg(theme.roles.gray)
                        },
                    ),
                ]));
            }
            crate::app::DockRow::Child {
                title,
                state,
                session_id: _,
            } => {
                // A child row is read like a session row in the list: its own
                // state mark first, then the name it answers to. The reader is
                // choosing which session to move into, so what it is doing is
                // the fact that decides.
                let (marker, tone) = match state {
                    Some(state) => (
                        crate::view::session_state_mark(*state, app.glyph_tier()),
                        state_marker_colour(*state, theme),
                    ),
                    None => ("·", theme.roles.gray_dim),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", if active { "▸" } else { " " }),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(format!("{marker} "), Style::default().fg(tone)),
                    Span::styled(
                        truncate_to_width(title, usize::from(area.width).saturating_sub(8), "…"),
                        base,
                    ),
                ]));
            }
            crate::app::DockRow::Plan { title, status, .. } => {
                let marker = match status {
                    vibex_core::PlanStepStatus::Completed => {
                        crate::glyphs::check_mark(app.glyph_tier())
                    }
                    vibex_core::PlanStepStatus::Running => crate::glyphs::frame_at(
                        crate::glyphs::spinner_frames(app.glyph_tier()),
                        app.animation_phase(),
                        4,
                    ),
                    vibex_core::PlanStepStatus::Failed => crate::glyphs::ballot_x(app.glyph_tier()),
                    vibex_core::PlanStepStatus::Pending => "·",
                };
                let tone = match status {
                    vibex_core::PlanStepStatus::Completed => theme.roles.success,
                    vibex_core::PlanStepStatus::Running => theme.roles.accent_running,
                    vibex_core::PlanStepStatus::Failed => theme.roles.danger,
                    vibex_core::PlanStepStatus::Pending => theme.roles.gray_dim,
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", if active { "▸" } else { " " }),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(format!("{marker} "), Style::default().fg(tone)),
                    Span::styled(
                        truncate_to_width(title, usize::from(area.width).saturating_sub(8), "…"),
                        if active {
                            base
                        } else if *status == vibex_core::PlanStepStatus::Completed {
                            Style::default()
                                .fg(theme.roles.gray_dim)
                                .add_modifier(Modifier::CROSSED_OUT)
                        } else {
                            Style::default().fg(theme.roles.gray)
                        },
                    ),
                ]));
            }
            crate::app::DockRow::Queue { index, text } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", if active { "▸" } else { " " }),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(
                        format!("#{} ", index + 1),
                        Style::default().fg(theme.roles.gray_dim),
                    ),
                    Span::styled(
                        truncate_to_width(text, usize::from(area.width).saturating_sub(8), "…"),
                        base,
                    ),
                ]));
            }
        }
    }
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {} {}",
                crate::glyphs::chevron(false, app.glyph_tier()),
                hidden,
                strings.dock_more()
            ),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The spinner or mark for an agent's state, with its colour.
fn dock_status_marker(
    status: vibex_core::ToolCallStatus,
    phase: u32,
    theme: &TuiTheme,
    app: &App,
) -> (&'static str, ratatui::style::Color) {
    match status {
        vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress => (
            crate::glyphs::frame_at(crate::glyphs::spinner_frames(app.glyph_tier()), phase, 4),
            theme.roles.accent_running,
        ),
        vibex_core::ToolCallStatus::Completed => (
            crate::glyphs::check_mark(app.glyph_tier()),
            theme.roles.success,
        ),
        vibex_core::ToolCallStatus::Failed => (
            crate::glyphs::ballot_x(app.glyph_tier()),
            theme.roles.danger,
        ),
    }
}

/// The prompt band.
fn render_prompt(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if !Bands::is_visible(area) {
        return;
    }
    // The Agent's question is drawn where the draft would be: the reader is
    // already looking at this band, and a card that replaces the box they type
    // in needs no gesture to reach. It is the *pending* request that decides,
    // not the overlay: one answered elsewhere leaves the band to the draft
    // rather than blanking it for the frame before the card is taken down.
    if app.elicitation_form().is_some() {
        render_elicitation_card(frame, area, app, theme);
        return;
    }
    render_composer(frame, area, app, theme, strings);
}

/// The shortcut band at the very bottom.
fn render_shortcuts(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    if !Bands::is_visible(area) {
        return;
    }
    render_key_bar(frame, area, app, theme);
}

/// The top band: where the session is, and what state it is in.
///
/// The location sits on the left and the status segments are right-aligned as a
/// group, so the left column is stable while the group grows and shrinks. A
/// left-aligned list of everything would push the state off the edge exactly
/// when a narrow terminal makes it most worth reading. A session page's corner
/// holds the band's one control: the way to the sessions that already exist,
/// drawn last so it stays where the reader learned to find it.
fn render_status_band(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if !Bands::is_visible(area) {
        return;
    }
    let sep = Span::styled(" │ ", Style::default().fg(theme.roles.gray_dim));

    // ---- left: the location ---------------------------------------------
    let mut left = Vec::new();
    // The composing page names the directory the session it is writing will
    // open in — the one the reader picked, else the open session's — rather
    // than the session behind the page, which the reader is leaving.
    let location = if app.page_owns_session() {
        app.active_session()
            .map(|session| session.workspace_root.clone())
            .or_else(|| app.workspace_path.clone())
    } else {
        Some(app.new_session_workspace())
    }
    .unwrap_or_else(|| app.page_label(strings).to_string());
    left.push(Span::styled(
        compact_path(&location, usize::from(area.width) / 3),
        Style::default().fg(theme.roles.path),
    ));
    // A button on a session row is a picture, and a picture cannot say what it
    // does. The band is the one place a terminal has room for the sentence a
    // tooltip would carry, so the pointer resting on a button — or the keyboard
    // having stepped onto one — puts the verb and the key that would have run it
    // here, and the location comes back when the reader moves on. The key is what
    // the row's controls used to spell out, and it is the half a reader cannot
    // guess from the icon. The page's action row is left out — both of its
    // controls are words already — and so is every other page, which has no row
    // for the remembered button to belong to.
    let focused_control = app
        .session_row_action
        .filter(|(row, _)| app.page == Page::Sessions && *row != crate::app::SESSION_ACTION_ROW);
    if let Some((_, intent)) = app.hovered_row_action.or(focused_control) {
        let mut hint = vec![Span::styled(
            intent.label(strings).to_string(),
            Style::default()
                .fg(theme.roles.foreground)
                .add_modifier(Modifier::BOLD),
        )];
        if let Some(chord) = app.keymap.chord_for_in(Scope::Sessions, intent) {
            hint.push(Span::styled(
                format!(" {}", chord.display()),
                Style::default().fg(theme.roles.accent_user),
            ));
        }
        hint.push(Span::styled(
            format!("  {}", strings.sessions_title()),
            Style::default().fg(theme.roles.gray_dim),
        ));
        left = hint;
    }

    // ---- right: the question, while there is one -------------------------
    // The reader's first `Ctrl+C` takes the band rather than joining the end of
    // it. A question the reader cannot see is not a question, and the segments
    // would have to fit beside it to be worth anything: `render_zoned_line`
    // drops a right-hand group that does not fit whole, so a hint appended to
    // the mode, the connection and the way to the list would vanish on exactly
    // the narrow terminals where the reader has least room to guess. What it
    // displaces is all one key away, and a reader being asked whether to leave
    // is not on their way to any of it. A toast still wins: it is about
    // something that happened, and this is only about what a second press
    // would do.
    if app.quit_armed() && app.toast.is_none() {
        render_zoned_line(
            frame,
            area,
            left,
            None,
            vec![Span::styled(
                strings.quit_hint().to_string(),
                Style::default()
                    .fg(theme.roles.warning)
                    .add_modifier(Modifier::BOLD),
            )],
        );
        return;
    }

    // ---- right: the status segments --------------------------------------
    let mut right: Vec<Span<'static>> = Vec::new();
    let push_segment = |right: &mut Vec<Span<'static>>, span: Span<'static>| {
        if !right.is_empty() {
            right.push(sep.clone());
        }
        right.push(span);
    };

    let connection = match app.live {
        crate::app::LiveState::Ready => None,
        crate::app::LiveState::Connecting => Some((strings.connecting(), theme.roles.gray)),
        crate::app::LiveState::Reconnecting => Some((strings.reconnecting(), theme.roles.warning)),
        crate::app::LiveState::Offline => Some((strings.disconnected(), theme.roles.danger)),
    };
    if let Some((label, color)) = connection {
        push_segment(&mut right, Span::styled(label, Style::default().fg(color)));
    }

    let approvals = app.pending_permission_count();
    let questions = app.pending_elicitations();
    if approvals + questions > 0 {
        push_segment(
            &mut right,
            Span::styled(
                format!(
                    "{} {}",
                    crate::glyphs::diamond_filled(app.glyph_tier()),
                    approvals + questions
                ),
                Style::default()
                    .fg(theme.roles.accent_attention)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }

    if let Some(toast) = &app.toast {
        let color = match toast.tone {
            ToastTone::Info => theme.roles.gray,
            ToastTone::Success => theme.roles.accent_success,
            ToastTone::Warning => theme.roles.warning,
            ToastTone::Danger => theme.roles.danger,
        };
        push_segment(
            &mut right,
            Span::styled(toast.text.clone(), Style::default().fg(color)),
        );
    }

    push_segment(
        &mut right,
        Span::styled(
            app.seat.label(strings).to_string(),
            Style::default().fg(theme.roles.gray_dim),
        ),
    );

    // ---- the way back up, when this session was delegated -----------------
    // A sub-session is a page the reader arrived at *from* another session, so
    // the band names where it came from the way it names the list: a control
    // that runs the same intent the chord does. It yields to the segments
    // before it, and the Sessions entry after it yields in turn.
    let mut parent_entry: Option<(u16, u16)> = None;
    if app.page_owns_session()
        && let Some(parent_id) = app.parent_session_id()
    {
        let title = app
            .session_display_title(&parent_id)
            .unwrap_or_else(|| strings.parent_session().to_string());
        let chord = app
            .keymap
            .chord_for_in(Scope::Agent, Intent::GotoParentSession)
            .or_else(|| app.keymap.chord_for(Intent::GotoParentSession));
        let label = format!(
            "{} {}",
            crate::glyphs::chevron(true, app.glyph_tier()),
            title
        );
        let budget = usize::from(area.width) / 3;
        let label = truncate_to_width(&label, budget.max(8), "…");
        let mut width = display_width(&label) as u16;
        let chord = chord.map(|chord| format!(" {}", chord.display()));
        if let Some(chord) = &chord {
            width += display_width(chord) as u16;
        }
        let status_width = right.iter().map(Span::width).sum::<usize>() as u16;
        let separator = if right.is_empty() {
            0
        } else {
            sep.width() as u16
        };
        if status_width + separator + width + 1 < area.width {
            if !right.is_empty() {
                right.push(sep.clone());
            }
            right.push(Span::styled(
                label,
                Style::default()
                    .fg(theme.roles.accent_user)
                    .add_modifier(Modifier::BOLD),
            ));
            if let Some(chord) = chord {
                right.push(Span::styled(chord, theme.dimmed(theme.roles.gray_dim)));
            }
            parent_entry = Some((status_width + separator, width));
        }
    }

    // ---- the corner: the way to the sessions that already exist ----------
    // Both session pages carry it. On the prompt — where the client starts —
    // the list is the other half of that home; on a session's own page it is
    // how the reader leaves for another one. It is drawn last so it holds the
    // corner, and it is the only chrome here that is a control: the reader who
    // wants to reopen something rather than write something should not have to
    // know a key the draft has taken.
    let mut entry: Option<(u16, u16)> = None;
    if app.page.is_session_page() {
        let label = strings.sessions_title();
        // The chord that answers *here*: the composer owns the plain digits
        // the global binding uses, so while it has the keyboard the page names
        // its own chord rather than one that would type a digit.
        let chord = match app.focus {
            crate::app::Focus::Composer => app
                .keymap
                .chord_for_in(Scope::Composer, Intent::GotoSessions),
            _ => None,
        }
        .or_else(|| app.keymap.chord_for(Intent::GotoSessions));
        let status_width = right.iter().map(Span::width).sum::<usize>() as u16;
        let separator = if right.is_empty() {
            0
        } else {
            sep.width() as u16
        };
        let mut width = display_width(label) as u16;
        let chord = chord.map(|chord| format!(" {}", chord.display()));
        if let Some(chord) = &chord {
            width += display_width(chord) as u16;
        }
        // The entry is the group's only segment that can be given up: what
        // precedes it is the state of the session the reader is in, and
        // `render_zoned_line` drops a right-hand group that does not fit whole.
        // An entry that pushed the group over the edge would take the
        // connection, the approvals and the context readout with it on exactly
        // the narrow terminals where they are worth most, so it yields instead,
        // and the chord it names still answers where it is gone.
        if status_width + separator + width + 1 < area.width {
            if !right.is_empty() {
                right.push(sep.clone());
            }
            right.push(Span::styled(
                label.to_string(),
                Style::default()
                    .fg(theme.roles.accent_user)
                    .add_modifier(Modifier::BOLD),
            ));
            if let Some(chord) = chord {
                right.push(Span::styled(chord, theme.dimmed(theme.roles.gray_dim)));
            }
            entry = Some((status_width + separator, width));
        }
    }

    // The entry is a button: a click runs the same intent the chord would.
    // Its rect is the tail of the right-aligned group, so it is only published
    // when the group was drawn at all — `render_zoned_line` drops the group
    // rather than let it collide with the location on a narrow terminal.
    let right_width = right.iter().map(Span::width).sum::<usize>() as u16;
    if let Some((offset, width)) = entry
        && right_width + 1 < area.width
    {
        app.regions.hints.push((
            Rect {
                x: area.x + area.width - right_width + offset,
                y: area.y,
                width,
                height: 1,
            },
            Intent::GotoSessions,
        ));
    }
    // The parent crumb is the same kind of control: a click goes back up the
    // tree exactly as Alt+Left does.
    if let Some((offset, width)) = parent_entry
        && right_width + 1 < area.width
    {
        app.regions.hints.push((
            Rect {
                x: area.x + area.width - right_width + offset,
                y: area.y,
                width,
                height: 1,
            },
            Intent::GotoParentSession,
        ));
    }

    render_zoned_line(frame, area, left, None, right);
}

/// Shorten a path so the last components stay readable.
///
/// The tail of a path is what identifies it; `/home/dev/…/net/upload.rs` is
/// more useful than `/home/dev/code/peatboy/vibex-dev/vib…`. Before the head is
/// dropped, its folders fold to their initials — `/h/d/c/v/c/vibex-tui` still
/// says how deep the path is and which branch of it the reader is on, and the
/// final name is never touched.
pub fn compact_path(path: &str, budget: usize) -> String {
    if display_width(path) <= budget || budget < 8 {
        return path.to_string();
    }
    let abbreviated = abbreviate_components(path);
    if display_width(&abbreviated) <= budget {
        return abbreviated;
    }
    let suffix = take_width_from_end(path, budget.saturating_sub(2));
    format!("…/{suffix}")
}

/// Fold every component but the last to its first character.
///
/// `/home/dev/code/vibex/crates/vibex-tui` becomes `/h/d/c/v/c/vibex-tui`: the
/// ancestors still read as the path they are, and the component that names the
/// place — the directory a session works in — keeps its whole name, because a
/// row of initials alone would not say where the reader is.
fn abbreviate_components(path: &str) -> String {
    // A path with no separator at all (`C:`) has no ancestor to fold and is
    // left as it is rather than mangled.
    let Some(cut) = path.rfind(is_path_separator) else {
        return path.to_string();
    };
    let (head, tail) = path.split_at(cut + 1);
    let mut out = String::with_capacity(path.len());
    let mut start = 0usize;
    for (index, character) in head.char_indices() {
        if !is_path_separator(character) {
            continue;
        }
        out.push_str(fold_component(&head[start..index]));
        out.push(character);
        start = index + character.len_utf8();
    }
    out.push_str(tail);
    out
}

/// One ancestor, shortened to its initial.
///
/// A volume is not a folder name: `C:` stays whole, because `C` alone would
/// stop saying which drive the reader is on.
fn fold_component(component: &str) -> &str {
    if component.ends_with(':') {
        return component;
    }
    match component.char_indices().nth(1) {
        Some((index, _)) => &component[..index],
        None => component,
    }
}

/// Both separators, because the runtime's working directory may be a Windows
/// path and the status band must not print it whole just because of a `\`.
fn is_path_separator(character: char) -> bool {
    character == '/' || character == '\\'
}

/// The last `budget` columns of a path, starting at a component boundary.
fn take_width_from_end(path: &str, budget: usize) -> String {
    let mut width = 0usize;
    let mut start = path.len();
    for grapheme in path.graphemes(true).collect::<Vec<_>>().into_iter().rev() {
        let grapheme_width = display_width(grapheme);
        if width + grapheme_width > budget {
            break;
        }
        width += grapheme_width;
        start -= grapheme.len();
    }
    // Start at the next component boundary so a truncated name does not appear
    // as a fragment of its parent.
    match path[start..].find(is_path_separator) {
        Some(offset) if offset + 1 < path.len() - start => path[start + offset + 1..].to_string(),
        _ => path[start..].to_string(),
    }
}

/// The shared page shell: bordered title, body, and an empty-state message.
fn page_frame(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &TuiTheme,
    title: &str,
    focused: bool,
) -> Rect {
    let border_style = if focused {
        theme.focus_style()
    } else {
        theme.border_style()
    };
    let block = bordered(theme)
        .border_style(border_style)
        .title(Span::styled(format!(" {title} "), theme.strong()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// The first thing a new session shows.
///
/// An empty transcript is the moment the reader is most likely to be lost, so
/// this is a real surface rather than a grey sentence: what the client is, what
/// it can do, and the keys that get started. It is laid out as a hero on a wide
/// terminal and stacked on a narrow one, because the same block cannot be both.
/// The page a new session starts on: the mark, and the prompt under it.
///
/// A reader who asks for a new session wants to *write*, so the page hands them
/// the composer and shows what the message will be sent through — the Agent, the
/// model, the workspace — rather than asking them a question about it first. The
/// session itself is created by sending: its title comes from the message, which
/// is where a title comes from anyway.
fn render_new_session(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.height < 4 || area.width < 24 {
        empty_state(frame, area, theme, strings.session_new());
        return;
    }
    let style = app.settings.mark;
    let lit = app.mark_animates();
    let mark = crate::logo::rows(theme, style, app.animation_phase(), area.width, lit);
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Room for the mark, a blank row and the lines under it, or the mark is
    // dropped and the words speak for themselves.
    let mark_fits = !mark.is_empty() && area.height as usize >= mark.len() + 7;
    if mark_fits {
        lines.push(Line::from(""));
        let indent = usize::from(area.width).saturating_sub(crate::logo::width(
            style,
            theme.glyphs(),
            area.width,
        )) / 2;
        lines.extend(mark.into_iter().map(|line| {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            Line::from(spans)
        }));
    }
    lines.push(Line::from(""));
    // The page does not name itself. It is the prompt, and a reader who typed
    // the client's name to get here does not need telling which page the cursor
    // is in; the version is the one thing here that is not already obvious.
    lines.push(centred(
        area.width,
        vec![Span::styled(
            env!("CARGO_PKG_VERSION").to_string(),
            theme.dimmed(theme.roles.gray_dim),
        )],
    ));
    lines.push(Line::from(""));

    // The sentence the other clients lead their page with, in this client's
    // vocabulary: the reader's own language, the Agent's mark where the words
    // leave room for it, and the Agent's name where the terminal cannot hold a
    // picture. The split is [`Locale::new_session_slogan`], the same table the
    // phone and the desktop use, so the three pages say one thing in one order.
    let (agent, model) = app.drawn_runtime_parts();
    // The mark is a picture, so the transition cannot scramble it — but the two
    // cells it stands in can wear the line's own noise while the Agent is
    // changing, which is the same effect in the one vocabulary a cell has. The
    // picture is asked for on the frame the noise stops.
    let mark_noise = app.transitions.cells(
        crate::scramble::Slot::Runtime,
        usize::from(crate::row_marks::MARK_COLS),
        app.animation_phase(),
    );
    let sentence = new_session_slogan_row(
        area.width,
        &app.composer_agent_identity(),
        &agent,
        app.terminal_images.supports_pixels(),
        mark_noise.as_deref(),
        theme,
        strings,
    );
    let sentence_row = lines.len();
    lines.push(sentence.line);

    // What the message will be sent through, and where. Both are answers the
    // reader needs before writing, and both are one key away from changing —
    // which is what the transition is for: the line the reader just changed
    // arrives as itself rather than being swapped out under their eyes.
    let runtime = if model.is_empty() {
        agent
    } else {
        format!("{agent} · {model}")
    };
    let switches = [
        (
            Intent::SwitchAgentRuntime,
            strings.session_runtime_label(),
            runtime,
            strings.runtime_switch_hint(),
        ),
        (
            Intent::SwitchWorkspace,
            strings.session_workspace_label(),
            app.drawn_new_session_workspace(),
            strings.workspace_switch_hint(),
        ),
    ];
    // Each row ends in a key: `Ctrl+G` swaps the Agent, `Ctrl+W` the workspace.
    // The reader can see the chord, so it is a press, and the press is the key
    // — looked up by the intent the row is about rather than by re-reading the
    // translated sentence, because a locale may put the chord anywhere in it.
    let mut switch_hints = Vec::new();
    for (intent, label, value, hint) in switches {
        let line = setting_row(area.width, label, &value, theme, Some(hint));
        if let Some(chord) = app.keymap.chord_for(intent)
            && let Some(column) = legend_column(&line, &chord.display())
        {
            switch_hints.push((intent, lines.len(), column, chord));
        }
        lines.push(line);
    }

    // The draft's vocabulary is not repeated here: the empty composer under
    // this page spells out `/ commands @ files $ skills` itself, and the page
    // is read once while the box is read every time it is written in.

    // Centred as a block, the way the desktop's page is: the mark sits over the
    // prompt it belongs to rather than hanging at the top of a tall band.
    let body = lines.len().min(usize::from(area.height));
    let top = usize::from(area.height).saturating_sub(body) / 3;
    let region = Rect {
        y: area.y + top as u16,
        height: body as u16,
        ..area
    };
    for (intent, row, column, chord) in switch_hints {
        app.regions.hints.push((
            Rect {
                x: area.x + column,
                y: region.y + row as u16,
                width: display_width(&chord.display()) as u16,
                height: 1,
            },
            intent,
        ));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), region);
    // The Agent's mark in the sentence is a picture the terminal holds rather
    // than a cell this frame draws, so it is *asked* for once the row it belongs
    // to is known: the sentence is centred, and only the frame that placed it
    // can say which column the mark's two cells start at.
    if let Some(mark) = sentence.mark {
        app.row_marks.want(
            &app.terminal_images,
            &crate::row_marks::Mark::Agent(mark.identity),
            crate::theme::color_rgb(theme.roles.foreground),
            Rect {
                x: area.x.saturating_add(mark.column),
                y: region.y.saturating_add(sentence_row as u16),
                width: crate::row_marks::MARK_COLS,
                height: crate::row_marks::MARK_ROWS,
            },
        );
    }
}

/// One drawn row of the new-session sentence, and where its Agent mark goes.
struct NewSessionSloganRow {
    line: Line<'static>,
    mark: Option<NewSessionSloganMark>,
}

/// The Agent's mark in the new-session sentence, in the page's own cells.
struct NewSessionSloganMark {
    identity: String,
    /// The column the mark's two cells start at, from the page's left edge.
    column: u16,
}

/// The new-session sentence, centred, with a slot for the Agent's mark.
///
/// A terminal cannot draw a brand mark in a cell, so the sentence reserves the
/// two cells a picture needs and asks the frame to put one there. When there is
/// no picture to have — a terminal without an image protocol, or an Agent with
/// no brand mark — the Agent's own name fills the slot instead: the same fact,
/// said the way that terminal can say it, and never a blank column.
///
/// The order is the reader's language's, from [`Locale::new_session_slogan`]:
/// Chinese reads `和 [mark] 开始新的工作`, English reads `Start something new with
/// [mark]`. The line is one voice at the page's title weight, and only the mark
/// stands out of it.
fn new_session_slogan_row(
    width: u16,
    identity: &str,
    agent_name: &str,
    pictures: bool,
    mark_noise: Option<&str>,
    theme: &TuiTheme,
    strings: Strings,
) -> NewSessionSloganRow {
    let (lead, connector, trail) = strings.locale.new_session_slogan();
    let style = theme.strong();
    let mark = drawable_agent_mark(identity.to_string(), pictures, theme);
    // The slot is the picture's width when there is one, and the Agent's own
    // name when there is not: `None` here means the sentence has no mark to
    // reserve cells for, so the slot is the name and no picture is asked for. A
    // page with no Agent chosen yet has no identity to name either, and then the
    // slot is empty — the sentence is said alone rather than naming a runtime
    // the page is still loading.
    // While the change is still arriving the slot wears the transition's own
    // noise rather than blank cells: a picture that appeared on the first frame
    // of a cut would be the one thing on the line that did not move.
    let slot = match mark.as_ref() {
        Some(_) => mark_noise
            .map(str::to_string)
            .unwrap_or_else(|| " ".repeat(usize::from(crate::row_marks::MARK_COLS))),
        None if identity.is_empty() || agent_name.is_empty() => String::new(),
        None => agent_name.to_string(),
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut column = None;
    let push = |text: &str, spans: &mut Vec<Span<'static>>, used: &mut usize| {
        if text.is_empty() {
            return;
        }
        let piece = if spans.is_empty() {
            text.to_string()
        } else {
            format!(" {text}")
        };
        *used += display_width(&piece);
        spans.push(Span::styled(piece, style));
    };
    push(lead, &mut spans, &mut used);
    push(connector, &mut spans, &mut used);
    if !slot.is_empty() {
        if !spans.is_empty() {
            used += 1;
            spans.push(Span::raw(" "));
        }
        if mark.is_some() {
            column = Some(used);
        }
        used += display_width(&slot);
        spans.push(Span::raw(slot.clone()));
    }
    push(trail, &mut spans, &mut used);
    // The row is centred as a whole; the mark's column is the one the slot
    // leaves, shifted by that centring. A sentence wider than the page is not
    // centred at all — the terminal clips it at the right, which is where the
    // words after the mark are — so the column is measured from the left edge.
    let indent = usize::from(width).saturating_sub(used) / 2;
    let mut out = vec![Span::raw(" ".repeat(indent))];
    out.extend(spans);
    NewSessionSloganRow {
        line: Line::from(out),
        mark: mark.and_then(|identity| {
            // A mark asked for on a frame the slot is still wearing noise would
            // be placed over the noise, which is the one picture a transition
            // is supposed to be replacing. The settle frame is the one that
            // places it.
            let column = mark_noise.is_none().then_some(column).flatten()?;
            Some(NewSessionSloganMark {
                identity,
                column: (indent + column) as u16,
            })
        }),
    }
}

/// The width of the label column on the new-session page.
const SETTING_LABEL_COLUMN: usize = 9;

/// One `label  value  hint` row of the new-session page.
fn setting_row(
    width: u16,
    label: &str,
    value: &str,
    theme: &TuiTheme,
    hint: Option<&str>,
) -> Line<'static> {
    let hint = hint.map(|hint| format!("  {hint}")).unwrap_or_default();
    // A fixed label column, so the values of two rows line up whatever the
    // language says the labels are.
    let label_column = SETTING_LABEL_COLUMN;
    let padded = format!("{label:<label_column$} ");
    // The row is centred, so it must not be wider than the terminal: the value
    // takes what is left after the label and the hint.
    let budget = usize::from(width)
        .saturating_sub(padded.chars().count() + hint.chars().count() + 2)
        .max(8);
    // A path keeps its tail — the directory the session works in is the end of
    // it — and folds its ancestors to initials rather than losing them.
    let value = compact_path(value, budget);
    let used = padded.chars().count() + value.chars().count() + hint.chars().count() + 2;
    let indent = usize::from(width).saturating_sub(used) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(padded, theme.dimmed(theme.roles.gray_dim)),
        Span::styled(
            value.to_string(),
            Style::default().fg(theme.roles.foreground),
        ),
        Span::styled(hint, theme.dimmed(theme.roles.gray)),
    ])
}

/// The cell a chord sits at inside an already-composed line.
///
/// A setting row centres itself with a run of spaces, and the hint is the last
/// span on it, so the chord's column is the width of everything before it plus
/// wherever the chord starts inside the hint. Measuring the composed line is
/// what keeps the hit rect on the cells the reader sees, whatever the locale
/// put around the key.
fn legend_column(line: &Line<'static>, chord: &str) -> Option<u16> {
    let mut before_hint = 0usize;
    for span in line.spans.iter().rev().skip(1) {
        before_hint += display_width(span.content.as_ref());
    }
    let hint = line.spans.last()?.content.as_ref();
    let offset = hint.find(chord)?;
    Some((before_hint + display_width(&hint[..offset])) as u16)
}

/// One centred line of already-styled spans.
fn centred(width: u16, spans: Vec<Span<'static>>) -> Line<'static> {
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    let indent = usize::from(width).saturating_sub(used) / 2;
    let mut out = vec![Span::raw(" ".repeat(indent))];
    out.extend(spans);
    Line::from(out)
}

fn render_welcome(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    // A terminal-mode session keeps its conversation in the Agent's own
    // interface, so this page has no transcript to draw. Saying so is the honest
    // answer: an empty transcript would read as "nothing has happened yet" for a
    // session that may be deep into a turn.
    if app
        .page_session_id()
        .and_then(|session_id| app.session_by_id(session_id))
        .is_some_and(vibex_ui::agent::is_terminal_session)
    {
        empty_state(frame, area, theme, strings.transcript_terminal_session());
        return;
    }
    if area.height < 4 {
        empty_state(frame, area, theme, strings.transcript_empty());
        return;
    }
    // A session whose first message is scheduled has nothing in its transcript
    // yet and nothing running: what the page has to say is when the message it
    // was opened for goes out. The Desktop draws the same countdown in place of
    // its own empty state, so a reader who opens such a session is told what it
    // is waiting for rather than that it is empty.
    if let Some(at_ms) = app
        .page_session_id()
        .and_then(|session_id| app.next_scheduled_message_at(session_id))
    {
        render_scheduled_empty_timeline(frame, area, app, at_ms, theme, strings);
        return;
    }
    // While the first-run steps are unfinished the guide is the content, in
    // order, with the next step to take called out. It belongs to the landing
    // surface: once a session is open the transcript is what matters, and the
    // remaining step is named by that surface's own empty state.
    if app.page == Page::Sessions && !app.onboarding_complete() && area.height >= 14 {
        render_onboarding(frame, area, app, theme, strings);
        return;
    }
    let wide = area.width >= 90;
    let height = area.height.min(if wide { 14 } else { 12 });
    let top = area.y + area.height.saturating_sub(height) / 2;
    let region = Rect {
        y: top,
        height,
        ..area
    };

    let accent = Style::default()
        .fg(theme.roles.accent)
        .add_modifier(Modifier::BOLD);
    let title = Style::default().fg(theme.roles.foreground);
    let dim = Style::default().fg(theme.roles.gray_dim);
    let body = Style::default().fg(theme.roles.gray);

    let mut lines = vec![
        Line::from(Span::styled(format!("  {}", strings.app_name()), accent)),
        Line::from(Span::styled(
            format!("  {}", strings.product_tagline()),
            body,
        )),
        Line::from(""),
    ];

    // An empty page says what is empty; the menu below says what fills it.
    if app.page == Page::Sessions {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.sessions_empty()),
            title,
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.transcript_empty()),
            title,
        )));
    }
    lines.push(Line::from(""));

    // The menu. Each row is a key and what it does, which is more useful than a
    // list of feature names: the reader leaves knowing a key.
    for (key, label) in [
        ("n", strings.session_new()),
        ("/", strings.composer_command_menu()),
        ("@", strings.composer_file_menu()),
        ("?", strings.help_title()),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:<3}"), accent),
            Span::styled(label.to_string(), dim),
        ]));
    }

    if region.height as usize > lines.len() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.help_hint()),
            dim,
        )));
    }

    // On a wide terminal the menu sits beside the wordmark rather than under it,
    // so the block reads as a card instead of a wall of left-aligned lines.
    if wide && region.height >= 8 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(28),
                Constraint::Min(30),
            ])
            .split(region);
        let mut left = lines.clone();
        left.truncate(3);
        frame.render_widget(Paragraph::new(Text::from(left)), columns[1]);
        frame.render_widget(Paragraph::new(Text::from(lines.split_off(3))), columns[2]);
        return;
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), region);
}

/// The first-run guide: the steps, in order, with the next one called out.
fn render_onboarding(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let steps = app.onboarding();
    let accent = Style::default()
        .fg(theme.roles.accent)
        .add_modifier(Modifier::BOLD);
    let done_style = Style::default().fg(theme.roles.accent_success);
    let current_style = Style::default()
        .fg(theme.roles.foreground)
        .add_modifier(Modifier::BOLD);
    let future_style = Style::default().fg(theme.roles.gray_dim);
    let detail_style = Style::default().fg(theme.roles.gray);

    let check = crate::glyphs::check_mark(app.glyph_tier());
    let arrow = crate::glyphs::prompt_arrow(app.glyph_tier());
    let bullet = crate::glyphs::diamond_hollow(app.glyph_tier());

    let mut lines = vec![
        Line::from(Span::styled(
            format!("  {} · {}", strings.app_name(), strings.onboarding_title()),
            accent,
        )),
        Line::from(Span::styled(
            format!("  {}", strings.sessions_empty()),
            Style::default().fg(theme.roles.foreground),
        )),
        Line::from(""),
    ];
    for progress in &steps {
        let (marker, style) = if progress.done {
            (check, done_style)
        } else if progress.current {
            (arrow, current_style)
        } else {
            (bullet, future_style)
        };
        let key = progress.step.key();
        let label = progress.step.label(strings);
        let mut spans = vec![
            Span::styled(format!("  {marker} "), style),
            Span::styled(label.to_string(), style),
        ];
        if progress.done {
            spans.push(Span::styled(
                format!("  {}", strings.onboarding_done()),
                done_style,
            ));
        } else if !key.is_empty() {
            spans.push(Span::styled(
                format!("   {key}"),
                Style::default()
                    .fg(theme.roles.accent_user)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
        // Only the step being taken explains itself; four explanations at once
        // would be a manual rather than a guide.
        if progress.current {
            lines.push(Line::from(Span::styled(
                format!("      {}", progress.step.detail(strings)),
                detail_style,
            )));
        }
    }
    lines.push(Line::from(""));
    // The keys a first-time reader needs, in the same shape the plain welcome
    // uses. Optional: on a short terminal the steps matter more, so the menu
    // and then the hint are the first things dropped.
    let budget = usize::from(area.height);
    if lines.len() + 6 <= budget {
        for (key, label) in [
            ("n", strings.session_new()),
            ("/", strings.composer_command_menu()),
            ("@", strings.composer_file_menu()),
            ("?", strings.help_title()),
        ] {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<3}"), accent),
                Span::styled(label.to_string(), detail_style),
            ]));
        }
    }
    if lines.len() + 2 <= budget {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.help_hint()),
            future_style,
        )));
    }
    // Centre what was actually built, so a clipped row is never the last step.
    let height = (lines.len() as u16).min(area.height);
    let region = Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    };
    frame.render_widget(Paragraph::new(Text::from(lines)), region);
}

fn empty_state(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme, message: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text = truncate_to_width(message, usize::from(area.width), "…");
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(theme.roles.gray_dim))
            .alignment(Alignment::Center),
        area,
    );
}

/// What a session opened for a scheduled message says until it goes out.
///
/// The same three answers the Desktop gives — that a message is waiting, how
/// long is left, and the hour it will leave at — because a transcript with
/// nothing in it and a countdown in front of it is otherwise indistinguishable
/// from a client that dropped the message.
fn render_scheduled_empty_timeline(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    at_ms: i64,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let now_ms = vibex_core::unix_timestamp_ms();
    let remaining = at_ms.saturating_sub(now_ms);
    let height = area.height.min(8);
    let region = Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "  {} {}",
                crate::glyphs::schedule_marker(app.glyph_tier()),
                strings.schedule_message()
            ),
            Style::default().fg(theme.roles.gray_dim),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", crate::schedule::format_remaining(remaining)),
            Style::default()
                .fg(theme.roles.accent_attention)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  {}", crate::schedule::format_send_time(at_ms)),
            Style::default().fg(theme.roles.gray),
        )),
        Line::from(""),
    ];
    // What is waiting, in the reader's own words. A countdown alone says that
    // something will happen; the message is the part only they can recognise,
    // and the part that tells them whether it is the one they meant to arm —
    // which matters most when another client is the one holding it.
    if let Some((text, _)) = app
        .page_session_id()
        .and_then(|session_id| app.earliest_scheduled_send(session_id))
    {
        let first_line = text.lines().next().unwrap_or_default();
        let width = usize::from(area.width).saturating_sub(4);
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_to_width(first_line, width, "…")),
            Style::default().fg(theme.roles.foreground),
        )));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.schedule_note()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    lines.truncate(usize::from(region.height));
    frame.render_widget(Paragraph::new(Text::from(lines)), region);
}

/// The empty composer's hint: one sigil per completion drawer, named in the
/// reader's own language.
///
/// The words come from the same copy the drawers title themselves with, so the
/// hint cannot describe a menu under a name the menu does not use.
fn composer_vocabulary(strings: Strings) -> String {
    format!(
        "/ {} @ {} $ {}",
        strings.composer_command_menu(),
        strings.composer_file_menu(),
        strings.composer_skill_menu()
    )
}

/// The edge down the left of the composer: the cell that marks the writing
/// surface as the reader's own.
///
/// One column, the height of the surface, in the colour the transcript draws
/// the reader's own message with. The box and the message are one object at two
/// moments, and the edge is what says so before a word is read.
fn render_composer_edge(frame: &mut Frame<'_>, input: Rect, theme: &TuiTheme) {
    if input.width == 0 || input.height == 0 {
        return;
    }
    let edge = Rect {
        width: BOX_EDGE_COLUMNS.min(input.width),
        ..input
    };
    let rows = (0..input.height)
        .map(|_| Line::from(crate::transcript::box_edge(theme)))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(rows)), edge);
}

/// Draw the composer.
///
/// The composer is the one place the user types, so it gets the interface's
/// strongest affordances:
///
/// ```text
/// ┃ ❯ the draft so far
/// <agent> · <account> · <model> · <mode>
/// ```
///
/// The writing surface is the reader's own, so it wears the same green edge on
/// its left as the message they last sent. The line under it is an info line —
/// the runtime a message will go through and any warning — which is where a
/// terminal UI can show context without spending a row on it. The prompt mark
/// fades toward the canvas when the composer does not have focus, so "where will
/// my keystrokes go" is answerable at a glance rather than by reading.
fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let focused = app.focus == crate::app::Focus::Composer;
    let running = app.turn_reads_running();

    let rail_color = if focused {
        theme.roles.accent_user
    } else {
        theme.roles.gray
    };
    // A quiet writing surface with padding; context lives below it, so neither
    // the title nor runtime labels compete with the draft's caret.
    let input = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.roles.surface_raised)),
        input,
    );
    render_composer_edge(frame, input, theme);
    // The edge, then the padding cell the draft is inset by: the same two cells
    // the reader's own message is drawn behind in the transcript.
    let text_area = Rect {
        x: input.x + BOX_EDGE_COLUMNS + 1,
        y: input.y + 1,
        width: input.width.saturating_sub(BOX_EDGE_COLUMNS + 2),
        height: input.height.saturating_sub(2),
    };
    if text_area.height == 0 {
        return;
    }

    let prompt_width = crate::glyphs::PROMPT_ARROW_WIDTH;
    let width = usize::from(text_area.width).saturating_sub(prompt_width);
    let caret = app.composer.cursor_cell(width);
    let tier = app.glyph_tier();
    // The prefix says what the draft will do: `❯` sends to the Agent, `!` runs a
    // shell command, `?` searches history.
    let (prefix, prefix_color) = match app.composer_mode {
        ComposerMode::Normal => (crate::glyphs::prompt_arrow(tier), rail_color),
        ComposerMode::Shell => ("! ", theme.roles.command),
        ComposerMode::HistorySearch => ("? ", theme.roles.gray),
    };
    let prefix_style = Style::default()
        .fg(prefix_color)
        .add_modifier(Modifier::BOLD);

    // A click anywhere in the box puts the cursor there, empty or not; the
    // border rows only take focus, since there is no cell to place a caret on.
    app.regions.composer = Some(text_area);
    app.regions.composer_band = Some(area);
    // Display rows the draft has scrolled past the top of the box, which is what
    // maps a click on a visible row back to the row the draft wrapped to.
    let mut scroll = 0usize;
    app.regions.composer_scroll = 0;
    if app.composer.text().is_empty() {
        // The placeholder explains the mode rather than the product: the mode is
        // the thing the reader cannot guess from an empty box. In the ordinary
        // mode the box is also where the draft's vocabulary is spelled out —
        // the one place a reader looks when it is empty.
        let placeholder = match app.composer_mode {
            ComposerMode::Shell => strings.mode_shell().to_string(),
            ComposerMode::HistorySearch => strings.composer_history().to_string(),
            ComposerMode::Normal if running => {
                format!("{} · Ctrl+S", strings.composer_steer())
            }
            ComposerMode::Normal => composer_vocabulary(strings),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled(
                    truncate_to_width(&placeholder, width, "…"),
                    Style::default().fg(theme.roles.gray),
                ),
            ])),
            text_area,
        );
    } else {
        let chips = app.composer.chip_ranges();
        let selection = app.composer.selection();
        let rows = app.composer.display_rows(width);
        let visible = usize::from(text_area.height).max(1);
        // A draft taller than the box scrolls under it, anchored to the caret:
        // the row being typed on is always the row that is shown.
        scroll = (usize::from(caret.0) + 1).saturating_sub(visible);
        app.regions.composer_scroll = u16::try_from(scroll).unwrap_or(u16::MAX);
        let lines = rows
            .into_iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .map(|(index, row)| {
                // The draft's first row carries the prompt mark; when it scrolls
                // away the visible rows are continuations and keep the indent.
                let gutter = if index == 0 {
                    Span::styled(prefix.to_string(), prefix_style)
                } else {
                    Span::raw(" ".repeat(prompt_width))
                };
                let style = theme.base().bg(theme.roles.surface_raised);
                let mut spans = vec![gutter];
                spans.extend(composer_line_spans(
                    &row.text,
                    style,
                    theme,
                    row.source_start,
                    &chips,
                    selection,
                ));
                Line::from(spans)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Text::from(lines)), text_area);
    }

    // The caret is the terminal's own cursor, placed on the cell the next
    // character will occupy. A composer that only highlights itself leaves the
    // reader guessing where typing will land.
    if focused {
        let row = usize::from(caret.0).saturating_sub(scroll);
        let x = text_area.x + prompt_width as u16 + caret.1;
        let y = text_area.y + u16::try_from(row).unwrap_or(u16::MAX);
        if x < text_area.right() && y < text_area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }

    // Runtime context sits below the padded writing surface, on the draft's own
    // column: the edge and the padding above it are the box, and a caption
    // belongs under what it describes.
    if area.height >= 2 {
        let bottom = Rect {
            y: area.y + area.height - 1,
            x: area.x + BOX_EDGE_COLUMNS + 1,
            width: area.width.saturating_sub(BOX_EDGE_COLUMNS + 2),
            height: 1,
        };
        render_composer_info(frame, bottom, app, theme, strings);
    }

    if let Some(menu) = app.completion.clone() {
        render_completion(frame, area, app, theme, strings, &menu);
    } else if app.composer_mode == ComposerMode::HistorySearch {
        render_history_search(frame, area, app, theme, strings);
    }
}

/// The history search drawer, drawn above the composer like the completion
/// drawer and with the same two-rule chrome.
///
/// It is the same shape on purpose: `? ` in the draft and `/`/`@` completions
/// are all "a list attached to the prompt", and giving them different chrome
/// would invent a distinction the reader does not have.
fn render_history_search(
    frame: &mut Frame<'_>,
    anchor: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    const MAX_ROWS: usize = 8;
    let matches = app.history_matches();
    let rows = matches.len().clamp(1, MAX_ROWS);
    let height = rows as u16 + 2;
    if anchor.y < height || anchor.width < 8 {
        return;
    }
    let area = Rect {
        x: anchor.x,
        y: anchor.y - height,
        width: anchor.width,
        height,
    };
    // Same plane and the same two widgets as the completion drawer above: the
    // two are the same chrome, and this one covers the transcript behind it for
    // the same reason.
    let surface = Style::default().bg(theme.roles.surface_raised);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(surface), area);
    let rule_style = Style::default().fg(theme.roles.gray_dim);
    // Top rule, titled and with the match count right-aligned on it.
    let title = format!(" {} ", strings.composer_history());
    let count = format!(" {} ", matches.len());
    let used = display_width(&title) + display_width(&count);
    let mut top = vec![Span::styled(title, Style::default().fg(theme.roles.gray))];
    if usize::from(area.width) > used + 2 {
        top.push(Span::styled(
            "─".repeat(usize::from(area.width) - used),
            rule_style,
        ));
        top.push(Span::styled(count, Style::default().fg(theme.roles.gray)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(top)).style(surface),
        Rect { height: 1, ..area },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            rule_style,
        )))
        .style(surface),
        Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        },
    );

    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    if matches.is_empty() {
        empty_state(frame, body, theme, strings.composer_history_empty());
        return;
    }
    // The selection is kept in view by scrolling the window rather than by an
    // offset of its own: the list is short and the cursor is the anchor.
    let selected = app.history_selection.min(matches.len() - 1);
    let offset = selected.saturating_sub(rows.saturating_sub(1));
    let lines = matches
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
        .map(|(index, (_, text))| {
            let active = index == selected;
            let first_line = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            let mut spans = vec![Span::styled(
                if active { "❯ " } else { "  " },
                Style::default().fg(theme.roles.accent_user),
            )];
            if active {
                spans.push(Span::styled(
                    truncate_to_width(
                        first_line.trim(),
                        usize::from(body.width).saturating_sub(2),
                        "…",
                    ),
                    Style::default()
                        .fg(theme.roles.foreground)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    truncate_to_width(
                        first_line.trim(),
                        usize::from(body.width).saturating_sub(2),
                        "…",
                    ),
                    Style::default().fg(theme.roles.gray),
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)).style(surface), body);
}

/// The composer's info line, against the right edge.
///
/// This is deliberately the bottom border rather than a separate row: a
/// terminal has no room for chrome that only carries status, and a divider that
/// also informs is free. It sits on the *right* because that is where a reader
/// looks for it and where it costs the draft nothing: the left of the line is
/// the rule the eye follows into the box, and the runtime a message will go
/// through belongs beside the box's far corner, not on top of the text it is
/// about.
///
/// The runtime is named in full — the Agent, the account it will run on and the
/// model — because an entry the reader picked in the picker is one of those
/// three names, and a line that answered with two of them would leave the
/// reader to remember which Provider Profile they chose. The Agent is drawn as
/// its brand mark where the terminal can hold a picture and named where it
/// cannot: the same fact, said the way that terminal can say it.
fn render_composer_info(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.height == 0 {
        return;
    }
    let (agent, model) = app.drawn_runtime_parts();
    // The identity is read from the page rather than from the drawn name, which
    // is mid-transition noise on the frames after a switch: a mark matched
    // against that would blink out exactly when the reader changed Agent.
    let mark = composer_agent_mark(app, area, theme);
    // The mark's cells are painted blank rather than left untouched, so nothing
    // shows through around it: a picture sits above the text layer, not instead
    // of it. A terminal that cannot hold one, and an Agent without a mark, keep
    // the name — which is the other way of saying the same thing, never a blank
    // column.
    let mut left = match &mark {
        Some(_) => vec![Span::raw(
            " ".repeat(usize::from(crate::row_marks::MARK_COLS)),
        )],
        None => vec![Span::styled(agent, theme.muted())],
    };
    // The account the message will run on sits between the Agent and the model,
    // which is the order the picker names an entry in: an Agent reached through
    // a Provider Profile is a different runtime from the same Agent on its own
    // account, and the line that says what Enter will do has to say which.
    let account = app.drawn_runtime_account();
    if !account.is_empty() {
        left.push(Span::styled(format!(" · {account}"), theme.muted()));
    }
    if !model.is_empty() {
        left.push(Span::styled(format!(" · {model}"), theme.base()));
    }
    for option in app.composer_run_option_labels() {
        left.push(Span::styled(format!(" · {option}"), theme.muted()));
    }
    if let Some(seconds) = app
        .page_session_id()
        .and_then(|id| app.auto_continue.countdown_seconds(id))
    {
        left.push(Span::styled(format!(" · ↻{seconds}"), theme.warning()));
    }
    // The Composer's own clock, where the info line has room for it: a draft
    // with a send time says so beside the runtime it will go through, because
    // Enter is otherwise about to do something the reader did not ask for.
    if let Some(schedule) = app.draft_schedule() {
        left.push(Span::styled(
            format!(
                " · {} {}",
                crate::glyphs::schedule_marker(app.glyph_tier()),
                crate::schedule::schedule_summary(schedule)
            ),
            theme.warning(),
        ));
    }
    let images = app.composer.image_count();
    if images > 0 {
        left.push(Span::styled(
            format!(" · {images} {}", strings.image_count()),
            theme.muted(),
        ));
    }
    render_zoned_line(frame, area, left, None, Vec::new());
    // The mark goes where the Agent's name would have started, which is what
    // "instead of the name" means for a picture: the same place, drawn, not
    // written. It is *asked* for rather than placed, because the page draws
    // before the frame knows what a modal covers and a picture cannot be covered
    // once the terminal holds it.
    if let Some(identity) = mark {
        app.row_marks.want(
            &app.terminal_images,
            &crate::row_marks::Mark::Agent(identity),
            crate::theme::color_rgb(theme.roles.foreground),
            Rect {
                width: crate::row_marks::MARK_COLS,
                height: crate::row_marks::MARK_ROWS,
                ..area
            },
        );
    }
}

/// The Agent mark the composer's info line wants drawn, when there is one.
///
/// `None` is the caller's signal to name the Agent instead, which is the answer
/// a terminal that cannot hold a picture, an Agent with no mark, and a line too
/// narrow to seat one all get.
fn composer_agent_mark(app: &App, area: Rect, theme: &TuiTheme) -> Option<String> {
    if area.width < crate::row_marks::MARK_COLS {
        return None;
    }
    drawable_agent_mark(
        app.composer_agent_identity(),
        app.terminal_images.supports_pixels(),
        theme,
    )
}

/// The completion popup, drawn directly above the composer.
///
/// Two full-width rules with no corners and no side borders, rather than a box.
/// A box would need four more glyphs, would sit inset from the composer it
/// belongs to, and would read as a separate window; two rules read as a drawer
/// pulled out of the prompt. The count on the top rule says how much is behind
/// it, which a border title cannot.
///
/// A trigger with more than one panel — `/` and `@` — opens with a header row
/// naming them under that rule, and the drawer stays up while the panel the
/// reader chose has nothing to offer: a drawer that closed on an empty panel
/// would take the panels with it, and `←` could never reach the list that does
/// match.
fn render_completion(
    frame: &mut Frame<'_>,
    anchor: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
    menu: &crate::composer::CompletionMenu,
) {
    let indices = menu.visible();
    let tabbed = menu.tabs_available();
    if indices.is_empty() && !tabbed {
        // No "no matches" state for a trigger with one panel: the drawer simply
        // closes, which is what the reader expects when the query stops matching
        // anything.
        return;
    }
    let tier = app.glyph_tier();
    let rows = indices.len().min(crate::composer::MAX_VISIBLE_COMPLETIONS);
    let header = usize::from(tabbed);
    let body_rows = rows.max(usize::from(tabbed));
    let height = (header + body_rows + 2) as u16;
    if anchor.y < height {
        return;
    }
    let area = Rect {
        x: anchor.x,
        y: anchor.y - height,
        width: anchor.width,
        height,
    };
    // The drawer is a surface of its own, on the same plane as the composer it
    // is pulled out of. It is cleared *and* painted: the style alone would leave
    // every glyph the rows do not cover in place — the transcript behind the
    // drawer stayed readable through the gaps between them — and `Clear` alone
    // would leave the cells on the terminal's own background, the one colour a
    // themed interface cannot choose.
    let surface = Style::default().bg(theme.roles.surface_raised);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(surface), area);

    let rule_style = Style::default().fg(theme.roles.gray_dim);

    // Top rule, with the match count right-aligned on it.
    let mut top = vec![Span::styled(
        "─".repeat(usize::from(area.width)),
        rule_style,
    )];
    let count = format!(" {}/{} ", menu.selected + 1, indices.len());
    let count_width = display_width(&count);
    if !indices.is_empty() && usize::from(area.width) > count_width + 4 {
        let cut = usize::from(area.width) - count_width;
        top = vec![
            Span::styled("─".repeat(cut), rule_style),
            Span::styled(count, Style::default().fg(theme.roles.gray)),
        ];
    }
    frame.render_widget(
        Paragraph::new(Line::from(top)).style(surface),
        Rect { height: 1, ..area },
    );
    // Bottom rule, which is also the composer's top edge.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            rule_style,
        )))
        .style(surface),
        Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        },
    );

    let body = Rect {
        y: area.y + 1 + header as u16,
        height: body_rows as u16,
        ..area
    };
    if tabbed {
        render_completion_tabs(
            frame,
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
            app,
            theme,
            strings,
            menu,
            surface,
        );
    }
    if indices.is_empty() {
        let message = if menu.loading {
            strings.loading()
        } else if menu.active_tab() == crate::composer::CompletionTab::Phrases
            && menu.query.trim().is_empty()
        {
            strings.composer_phrases_empty()
        } else {
            strings.composer_no_matches()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {message}"),
                Style::default().fg(theme.roles.gray_dim),
            )))
            .style(surface),
            body,
        );
        return;
    }
    let lines = indices
        .iter()
        .take(rows)
        .map(|index| completion_row(menu, *index, usize::from(body.width), theme, tier))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)).style(surface), body);
}

/// The drawer's panel header: one name per panel the trigger opens.
///
/// The active name is filled with the accent the way the devices page and the
/// configuration centre mark their own strips, and the trigger's sigil sits on
/// the right naming which drawer this is. Each name is published as a click
/// region, so the pointer runs the same switch `←`/`→` do.
fn render_completion_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
    menu: &crate::composer::CompletionMenu,
    surface: Style,
) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0u16;
    for tab in menu.tabs() {
        let active = *tab == menu.active_tab();
        let label = format!(" {} ", completion_tab_label(*tab, strings));
        let width = display_width(&label) as u16;
        if used + width > area.width {
            break;
        }
        let style = if active {
            Style::default()
                .fg(theme.roles.background)
                .bg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        app.regions.completion_tabs.push((
            Rect {
                x: area.x + used,
                y: area.y,
                width,
                height: 1,
            },
            *tab,
        ));
        spans.push(Span::styled(label, style));
        used += width;
    }
    let sigil = format!("{}  ", menu.trigger.character());
    let sigil_width = display_width(&sigil) as u16;
    if used + sigil_width < area.width {
        spans.push(Span::raw(
            " ".repeat(usize::from(area.width - used - sigil_width)),
        ));
        spans.push(Span::styled(
            sigil,
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(surface), area);
}

/// One panel's name, in the reader's language.
fn completion_tab_label(tab: crate::composer::CompletionTab, strings: Strings) -> &'static str {
    use crate::composer::CompletionTab;
    match tab {
        CompletionTab::Commands => strings.composer_command_menu(),
        CompletionTab::Phrases => strings.composer_tab_phrases(),
        CompletionTab::Files => strings.composer_tab_files(),
        CompletionTab::Delegation => strings.composer_tab_delegation(),
    }
}

/// One completion row.
///
/// The selection marker is the two-column prefix the composer uses for its own
/// prompt arrow, so the highlighted row lines up with the text being typed and
/// the eye does not have to jump columns.
fn completion_row(
    menu: &crate::composer::CompletionMenu,
    index: usize,
    width: usize,
    theme: &TuiTheme,
    tier: crate::glyphs::GlyphTier,
) -> Line<'static> {
    let Some(item) = menu.items().get(index) else {
        return Line::from("");
    };
    let selected = index == menu.selected;
    let marker = if selected {
        crate::glyphs::prompt_arrow(tier)
    } else {
        "  "
    };
    let marker_style = Style::default().fg(if selected {
        theme.roles.accent_user
    } else {
        theme.roles.gray_dim
    });
    let label_style = if selected {
        Style::default()
            .fg(theme.roles.foreground)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.roles.gray_bright)
    };
    let detail_style = Style::default().fg(theme.roles.gray_dim);
    let group_style = Style::default().fg(theme.roles.accent_system);

    // The group tag trails the label so a mixed list stays scannable, and the
    // description takes whatever is left rather than a fixed column: a fixed
    // column wastes most of the row on a narrow terminal.
    let tag = format!(" [{}]", item.group);
    let budget = width.saturating_sub(crate::glyphs::PROMPT_ARROW_WIDTH + display_width(&tag) + 4);
    let label = truncate_to_width(&item.label, budget.min(40), "…");
    let used = crate::glyphs::PROMPT_ARROW_WIDTH + display_width(&label) + display_width(&tag) + 2;
    let detail_budget = width.saturating_sub(used);
    let detail = truncate_to_width(&item.detail, detail_budget, "…");

    let mut spans = vec![
        Span::styled(marker.to_string(), marker_style),
        Span::styled(label, label_style),
        Span::styled(tag, group_style),
    ];
    if !detail.is_empty() {
        spans.push(Span::styled("  ", detail_style));
        spans.push(Span::styled(detail, detail_style));
    }
    Line::from(spans)
}

/// The configuration centre: modules, areas, records and the detail pane.
///
/// Three bands rather than one, because the centre is a hierarchy and a flat
/// list cannot show one: the tab strip is the module, the rail is the function
/// area inside it, and the records pane holds both the table and the detail
/// under it. Every band is drawn from the same cursor the keys move, so the
/// pane that is lit and the pane a key acts on cannot disagree.
///
/// Which pane is lit is drawn three times over, because one cue is a cue the
/// reader misses: the focused pane's own edge takes the accent colour, its
/// selected row wears the focus bar, and the footer names it. `←`/`→` walk the
/// three and wrap.
fn render_config_center(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.width < 40 || area.height < 8 {
        empty_state(frame, area, theme, strings.config_title());
        return;
    }
    let rows = app.config_center.visible(&app.management_data, strings);
    app.config_center.reconcile(&rows);
    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);
    render_config_tabs(frame, bands[0], app, theme, strings);
    // The rule under the tabs is the tab strip's own bottom edge: it takes the
    // accent colour while the modules own the keyboard, so the strip reads as
    // the pane that is lit rather than as page chrome.
    let modules_focused = app.config_center.zone == crate::config_center::ConfigZone::Modules;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if modules_focused { "━" } else { "─" }.repeat(usize::from(bands[1].width)),
            if modules_focused {
                Style::default()
                    .fg(theme.roles.accent_user)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.roles.gray_dim)
            },
        ))),
        bands[1],
    );
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(rail_width(area.width)),
            Constraint::Length(1),
            Constraint::Min(20),
        ])
        .split(bands[2]);
    render_config_rail(frame, body[0], app, theme, strings);
    // A rule between the rail and the records, so the two panes read as two
    // panes rather than as one wide table with a stray column. It is the
    // rail's own right edge, so it takes the accent colour — and a heavier
    // glyph — while the areas own the keyboard.
    let rail_focused = app.config_center.zone == crate::config_center::ConfigZone::Rail;
    let rule = (body[1].y..body[1].bottom())
        .map(|_| {
            Line::from(Span::styled(
                if rail_focused { "┃" } else { "│" },
                if rail_focused {
                    Style::default()
                        .fg(theme.roles.accent_user)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.roles.gray_dim)
                },
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(rule), body[1]);
    render_config_records(frame, body[2], app, &rows, theme, strings);
    render_config_footer(frame, bands[3], app, &rows, theme, strings);
}

/// How wide the area rail is: a quarter of the terminal, within reason.
fn rail_width(width: u16) -> u16 {
    (width / 4).clamp(16, 26)
}

/// The module strip: four tabs, drawn as pills, clickable and switchable with
/// `[`/`]` or `←`/`→` from the module zone.
fn render_config_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let focused = app.config_center.zone == crate::config_center::ConfigZone::Modules;
    // The focus bar in the strip's first column, the same mark every pane
    // draws on its own leading edge.
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        if focused { "▌" } else { " " },
        Style::default().fg(theme.roles.accent_user),
    )];
    let mut used = 1u16;
    for module in crate::config_center::ConfigModule::ALL {
        let active = module == app.config_center.module;
        let label = format!(" {} ", module.label(strings));
        let width = display_width(&label) as u16;
        if used + width > area.width {
            break;
        }
        let style = if active {
            // The current module is a filled chip even when the strip is not
            // focused: it says where the reader is, which is worth knowing
            // whatever owns the keyboard.
            Style::default()
                .fg(theme.roles.background)
                .bg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD)
        } else if focused {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        app.regions.config_tabs.push((
            Rect {
                x: area.x + used,
                y: area.y,
                width,
                height: 1,
            },
            module,
        ));
        spans.push(Span::styled(label, style));
        spans.push(Span::styled(" ", Style::default()));
        used += width + 1;
    }
    // What sits on the right is the area's record count: the tab strip names
    // where the reader is, and the number tells them whether it is worth
    // walking to before they walk.
    let count = app.config_center.section.label(strings).to_string();
    let count_label = format!("{} · {}  ", count, app.page_row_count());
    let count_width = display_width(&count_label) as u16;
    if used + count_width + 2 < area.width {
        let pad = area.width - used - count_width - 1;
        spans.push(Span::raw(" ".repeat(usize::from(pad))));
        spans.push(Span::styled(
            count_label,
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The area rail: the module's function areas, with their record counts.
fn render_config_rail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let sections = app.config_center.module.sections().to_vec();
    let counts = app.config_center.counts(&app.management_data, strings);
    let focused = app.config_center.zone == crate::config_center::ConfigZone::Rail;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(sections.len() + 2);
    // The heading is the rail's title, so it brightens with the pane: a rail
    // that is lit only on its rows is a rail whose selection can be missed.
    lines.push(Line::from(Span::styled(
        format!(" {}", strings.config_section_label().to_uppercase()),
        if focused {
            Style::default()
                .fg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.roles.gray_dim)
                .add_modifier(Modifier::BOLD)
        },
    )));
    for (index, section) in sections.iter().enumerate() {
        let active = *section == app.config_center.section;
        let count = counts.get(index).copied().unwrap_or(0);
        let label = section.label(strings);
        let count_label = format!("{count} ");
        // The focus bar is the leading column; the pointer follows it, so the
        // two marks read as one row rather than as two.
        let bar = if focused { "▌" } else { " " };
        let marker = if active { "▸" } else { " " };
        let body = format!("{marker} {label}");
        let width = usize::from(area.width).saturating_sub(3);
        let body = truncate_to_width(
            &body,
            width.saturating_sub(display_width(&count_label)),
            "…",
        );
        let style = if active {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if focused {
            Style::default().fg(theme.roles.foreground)
        } else {
            Style::default().fg(theme.roles.text_secondary)
        };
        let mut spans = vec![
            Span::styled(
                bar.to_string(),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(body, style),
        ];
        let used = 1 + display_width(&spans[1].content);
        if used + display_width(&count_label) < usize::from(area.width) {
            spans.push(Span::styled(
                " ".repeat(usize::from(area.width) - used - display_width(&count_label)),
                style,
            ));
        }
        spans.push(Span::styled(
            count_label,
            if active {
                style
            } else {
                Style::default().fg(theme.roles.gray_dim)
            },
        ));
        lines.push(Line::from(spans));
        app.regions.config_sections.push((
            Rect {
                x: area.x,
                y: area.y + lines.len() as u16 - 1,
                width: area.width,
                height: 1,
            },
            *section,
        ));
    }
    // The rail's own hint, under the list, when the terminal is tall enough
    // for it to be an explanation rather than a squeeze. It is wrapped rather
    // than truncated: a hint cut in half explains nothing.
    if area.height as usize > lines.len() + 2 {
        let width = usize::from(area.width).saturating_sub(2);
        lines.push(Line::from(""));
        for line in textwrap(app.config_center.section.hint(strings), width)
            .into_iter()
            .take(3)
        {
            lines.push(Line::from(Span::styled(
                format!(" {line}"),
                Style::default().fg(theme.roles.gray_dim),
            )));
        }
    }
    let style = if focused {
        Style::default()
    } else {
        Style::default().fg(theme.roles.text_secondary)
    };
    frame.render_widget(Paragraph::new(lines).style(style), area);
}

/// The record table and the detail pane under it.
fn render_config_records(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    rows: &[crate::config_center::ConfigRow],
    theme: &TuiTheme,
    strings: Strings,
) {
    let detail_rows = if area.height >= 18 {
        9
    } else if area.height >= 12 {
        6
    } else {
        0
    };
    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(detail_rows)])
        .split(area);
    render_config_table(frame, bands[0], app, rows, theme, strings);
    if detail_rows > 0 {
        // The table's own bottom edge: it takes the accent while the records
        // own the keyboard, which is what makes the table-and-detail column
        // read as one lit pane.
        let focused = app.config_center.zone == crate::config_center::ConfigZone::Rows;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if focused { "━" } else { "─" }.repeat(usize::from(bands[1].width)),
                if focused {
                    Style::default()
                        .fg(theme.roles.accent_user)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.roles.gray_dim)
                },
            ))),
            Rect {
                height: 1,
                ..bands[1]
            },
        );
        let detail = Rect {
            y: bands[1].y + 1,
            height: bands[1].height.saturating_sub(1),
            ..bands[1]
        };
        render_config_detail(frame, detail, app, rows, theme, strings);
    }
}

fn render_config_table(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    rows: &[crate::config_center::ConfigRow],
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.height < 2 {
        return;
    }
    let focused = app.config_center.zone == crate::config_center::ConfigZone::Rows;
    // A filter line takes the first row when one is being typed or is set, so
    // the reader can see what is hiding rows rather than wondering.
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.config_center.filtering || !app.config_center.filter.is_empty() {
        let caret = if app.config_center.filtering {
            "▏"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", strings.config_search()),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                app.config_center.filter.clone(),
                Style::default().fg(theme.roles.foreground),
            ),
            Span::styled(caret.to_string(), theme.accent()),
            Span::styled(
                if app.config_center.filter.is_empty() {
                    format!("  {}", strings.config_search_hint())
                } else {
                    String::new()
                },
                Style::default().fg(theme.roles.gray_dim),
            ),
        ]));
    }
    // The heading row is the columns, measured against the same widths the
    // cells use so a heading sits over its own column. A narrow terminal drops
    // whole columns rather than squeezing every one of them past readability;
    // the status column is never among the dropped.
    let all_columns =
        crate::config_center::columns(app.config_center.module, app.config_center.section, strings);
    let visible_columns =
        visible_column_indices(app.config_center.section, area.width, all_columns.len());
    let columns = visible_columns
        .iter()
        .filter_map(|index| all_columns.get(*index).copied())
        .collect::<Vec<_>>();
    let layout = column_widths(area.width, &columns);
    if lines.len() < usize::from(area.height) {
        // The heading style is shared by every column, including the two the
        // table's data columns do not name: a heading row that starts halfway
        // across the table reads as a table missing its first columns.
        let heading_style = Style::default()
            .fg(if focused {
                theme.roles.text_secondary
            } else {
                theme.roles.gray_dim
            })
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
            // The focus bar, the status mark and a space — the same prefix the
            // cells carry, so every heading starts where its cells start.
            Span::styled(" ".repeat(TABLE_PREFIX), Style::default()),
            // The record's name column is a column like any other: it is named
            // so the row of headings covers the whole table rather than
            // beginning at the first fact column.
            Span::styled(
                config_cell(strings.config_col_name(), layout.title),
                heading_style,
            ),
            // The status column holds the chip, and is named for it.
            Span::styled(
                config_cell(strings.config_col_status(), CHIP_COLUMN),
                heading_style,
            ),
        ];
        for (index, column) in columns.iter().enumerate() {
            let width = layout.cell(index);
            spans.push(Span::styled(config_cell(column, width), heading_style));
        }
        lines.push(Line::from(spans));
    }
    // The registry's own line: the question the page was asked with, and how
    // much of the registry it covers. It sits above the columns because it is
    // about the whole page rather than about any record on it, and every
    // control on it is published so the pointer asks the same question the key
    // does.
    if let Some(spans) = render_registry_bar(area, app, rows.len(), theme, strings) {
        lines.push(Line::from(spans));
    }
    // A read that refused is the state the area is in, so it is drawn where the
    // area is rather than toasted once: the reader who looked away would
    // otherwise find an empty table and no reason for it, and the key that asks
    // again is named rather than left to be guessed.
    if let Some((code, message)) = app
        .management_data
        .read_error(app.config_center.module, app.config_center.section)
    {
        lines.push(Line::from(vec![
            Span::styled(" ".repeat(TABLE_PREFIX), Style::default()),
            Span::styled(
                format!(" {} ", strings.config_read_failed()),
                chip_style(crate::config_center::ConfigTone::Bad, theme)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{code} · {message}"),
                Style::default().fg(theme.roles.gray_bright),
            ),
            Span::styled(
                format!("  {}", strings.config_read_retry()),
                Style::default().fg(theme.roles.gray_dim),
            ),
        ]));
    }
    // The install card. A managed install is one the reader cannot see happen:
    // it runs on the authority, and without this the row would simply change
    // under them a while later with nothing in between.
    if let Some(watch) = app.install_watch.as_ref().filter(|watch| watch.running()) {
        lines.push(Line::from(vec![
            Span::styled(" ".repeat(TABLE_PREFIX), Style::default()),
            Span::styled(
                format!(
                    " {} ",
                    crate::glyphs::frame_at(
                        crate::glyphs::spinner_frames(app.glyph_tier()),
                        app.animation_phase(),
                        crate::glyphs::SPINNER_TICKS_PER_FRAME,
                    )
                ),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                strings
                    .config_install_watching()
                    .replace("{agent}", &watch.label),
                Style::default()
                    .fg(theme.roles.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {}", watch.phase_label(strings)),
                Style::default().fg(theme.roles.text_secondary),
            ),
            Span::styled(
                watch
                    .version
                    .as_ref()
                    .map(|version| format!(" · v{version}"))
                    .unwrap_or_default(),
                Style::default().fg(theme.roles.gray_bright),
            ),
        ]));
    }
    let body_top = lines.len() as u16;
    if rows.is_empty() {
        // An empty list and a list nobody has read yet look the same on screen
        // and are not the same thing: one is an area with nothing in it, and
        // the other is an area that has not been asked. The second draws the
        // shape of the rows it is waiting for, so the reader can tell "still
        // coming" from "there is nothing here".
        if app
            .management_data
            .has_read(app.config_center.module, app.config_center.section)
        {
            lines.push(Line::from(Span::styled(
                format!(
                    "{}  {}",
                    " ".repeat(TABLE_PREFIX),
                    if app.config_center.filter.trim().is_empty() {
                        strings.config_empty()
                    } else {
                        // The way out of a filter nobody remembers setting is
                        // the one thing an empty table owes the reader.
                        strings.config_hint_no_matches()
                    }
                ),
                Style::default().fg(theme.roles.gray_dim),
            )));
        } else {
            let dim = Style::default().fg(theme.roles.gray_dim);
            for index in 0..SKELETON_ROWS {
                // The first line carries the word; the rest are the shape of
                // the rows that are coming, at the widths the table uses.
                let mut spans = vec![Span::styled(" ".repeat(TABLE_PREFIX), dim)];
                for width in [24usize, 14, 12, 10] {
                    spans.push(Span::styled("░".repeat(width.min(24)), dim));
                    spans.push(Span::raw("  "));
                }
                if index == 0 {
                    spans.pop();
                    spans.push(Span::styled(
                        strings.config_reading().to_string(),
                        dim.add_modifier(Modifier::ITALIC),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    let selected = app.config_center.cursor(rows.len());
    let window = area.height.saturating_sub(body_top) as usize;
    let first = selected.saturating_sub(window.saturating_sub(1) / 2);
    for (offset, row) in rows.iter().skip(first).take(window.max(1)).enumerate() {
        let index = first + offset;
        let is_selected = index == selected;
        let hovered = app.hover == Some((crate::keymap::Scope::Management, index));
        let base = if is_selected && focused {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if is_selected || hovered {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface)
        } else if row.enabled {
            Style::default().fg(theme.roles.foreground)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        let marker = if row.enabled { "●" } else { "○" };
        let working = app.config_center.is_pending(&row.key);
        let mut spans = vec![
            // The focus bar marks the row the keys act on: accent while the
            // records pane owns the keyboard, a plain space while it does not.
            // The row keeps its selection background either way, so the cursor
            // never disappears — it stops being lit.
            Span::styled(
                if focused && is_selected { "▌" } else { " " },
                Style::default().fg(theme.roles.accent_user),
            ),
            // A working row rotates in place of its status mark, so a write in
            // flight is visible on every row rather than only on the one the
            // action bar happens to be describing.
            Span::styled(
                if working {
                    format!(
                        " {}",
                        crate::glyphs::frame_at(
                            crate::glyphs::spinner_frames(app.glyph_tier()),
                            app.animation_phase(),
                            crate::glyphs::SPINNER_TICKS_PER_FRAME,
                        )
                    )
                } else {
                    format!(" {marker}")
                },
                if working {
                    Style::default()
                        .fg(theme.roles.warning)
                        .add_modifier(Modifier::BOLD)
                } else if row.enabled {
                    Style::default().fg(theme.roles.success)
                } else {
                    Style::default().fg(theme.roles.gray_dim)
                },
            ),
            Span::styled(config_cell(&row.title, layout.title), base),
            // The status column is always emitted, chip or not: a row that
            // skipped it would slide its cells under the wrong headings.
            Span::styled(
                config_cell(&row.chip, CHIP_COLUMN),
                chip_style(row.tone, theme).patch(base),
            ),
        ];
        for (position, column) in visible_columns.iter().enumerate() {
            let Some(cell) = row.cells.get(*column) else {
                continue;
            };
            let width = layout.cell(position);
            spans.push(Span::styled(config_cell(cell, width), base));
        }
        lines.push(Line::from(spans));
    }
    app.regions.list = Some(crate::app::ListRegion {
        rect: Rect {
            y: area.y + body_top,
            height: area.height.saturating_sub(body_top),
            ..area
        },
        scope: crate::keymap::Scope::Management,
        rows: rows.len(),
        first_line: 0,
        first_row: first,
        heights: Vec::new(),
    });
    frame.render_widget(Paragraph::new(lines), area);
}

/// The registry bar: which question the page was asked with, and how much of the
/// registry it covers.
///
/// Drawn only where it means something — the installed areas have no page and no
/// catalogue behind them — and returns the row it drew so the table can count it
/// as one of its own lines.
fn render_registry_bar(
    area: Rect,
    app: &mut App,
    shown: usize,
    theme: &TuiTheme,
    strings: Strings,
) -> Option<Vec<Span<'static>>> {
    use crate::config_center::{ConfigModule, ConfigSection, RegistryControl};
    if app.config_center.section != ConfigSection::Catalog {
        return None;
    }
    let registry = app.config_center.registry;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = area.x;
    // One chip per narrowing: it reads as the value it is on, and a press moves
    // it on, which is the same thing `t` and `s` do.
    let (name, value, control) = match app.config_center.module {
        ConfigModule::Mcp => (
            crate::action::Intent::ConfigRegistryFilter.label(strings),
            registry
                .transport
                .map(|transport| transport.label(strings))
                .unwrap_or(strings.config_registry_transport_all()),
            RegistryControl::Transport,
        ),
        _ => (
            crate::action::Intent::ConfigRegistrySort.label(strings),
            registry.sort.label(strings),
            RegistryControl::Sort,
        ),
    };
    push_registry_segment(&mut spans, &mut x, area.y, app, name, None, false, theme);
    push_registry_segment(
        &mut spans,
        &mut x,
        area.y,
        app,
        value,
        Some(control),
        true,
        theme,
    );
    let pages = app.config_center.registry_pages(&app.management_data);
    push_registry_segment(
        &mut spans,
        &mut x,
        area.y,
        app,
        "‹",
        Some(RegistryControl::PagePrevious),
        registry.page > 0,
        theme,
    );
    push_registry_segment(
        &mut spans,
        &mut x,
        area.y,
        app,
        &format!("{}/{}", registry.page + 1, pages),
        None,
        false,
        theme,
    );
    push_registry_segment(
        &mut spans,
        &mut x,
        area.y,
        app,
        "›",
        Some(RegistryControl::PageNext),
        registry.page + 1 < pages,
        theme,
    );
    if let Some(note) = registry_note(app, shown, strings) {
        let room = usize::from(area.width.saturating_sub(x.saturating_sub(area.x)));
        let text = truncate_to_width(&note, room.saturating_sub(3), "…");
        if !text.is_empty() {
            spans.push(Span::styled("   ", Style::default()));
            spans.push(Span::styled(
                text,
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
    }
    Some(spans)
}

/// One control or label on the registry bar, drawn where it landed and — when it
/// is a control — published so a press on it reaches the same handler the key
/// does.
#[allow(clippy::too_many_arguments)]
fn push_registry_segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    y: u16,
    app: &mut App,
    text: &str,
    control: Option<crate::config_center::RegistryControl>,
    lit: bool,
    theme: &TuiTheme,
) {
    let label = format!(" {text} ");
    let width = display_width(&label) as u16;
    if let Some(control) = control {
        app.regions.config_registry.push((
            Rect {
                x: *x,
                y,
                width,
                height: 1,
            },
            control,
        ));
    }
    let style = if lit {
        Style::default()
            .fg(theme.roles.foreground)
            .bg(theme.roles.surface_highlight)
    } else {
        Style::default().fg(theme.roles.gray_bright)
    };
    spans.push(Span::styled(label, style));
    spans.push(Span::raw(" "));
    *x = x.saturating_add(width + 1);
}

/// What the registry said about the page the table is showing.
///
/// It is drawn from the numbers rather than stored as a sentence so the three
/// languages can spell it, and what is on screen is the rows themselves: a page
/// the client cut is described by what it cut rather than by what it fetched.
/// How many placeholder rows an area that has not answered yet draws.
///
/// It is the number of rows the desktop's skeleton cards stand in for, so the
/// list does not jump when the answer lands.
const SKELETON_ROWS: usize = 6;

/// How far the detail pane's own lines are indented.
const DETAIL_PREFIX: usize = 2;

fn registry_note(app: &App, shown: usize, strings: Strings) -> Option<String> {
    let note = app.management_data.market_note.as_ref()?;
    // A registry that refused is the state the list is in, so it is drawn where
    // the list is rather than toasted once and forgotten: the reader who looked
    // away would otherwise find an empty table and no reason for it.
    if let Some((code, message)) = &note.error {
        return Some(format!(
            "{} · {code} · {message}",
            strings.config_registry_failed()
        ));
    }
    let mut line = match note.matched {
        Some(matched) => format!(
            "{} {shown} {} {matched} {}",
            strings.config_registry_shown(),
            strings.config_registry_of(),
            strings.config_registry_entries()
        ),
        None => format!(
            "{} {shown} {}",
            strings.config_registry_shown(),
            strings.config_registry_entries()
        ),
    };
    // A page the registry has more of says so once: the next page is reached
    // with the table's own pager, so repeating that here would name two ways to
    // read the same answer.
    if !note.exhausted {
        line.push_str(" · ");
        line.push_str(strings.config_registry_indexing());
    }
    Some(line)
}

/// The columns the table lays out, in display columns.
///
/// `headings` is what the header draws and `cells` is what each row draws;
/// both are the same list, and one width is measured per column, so a heading
/// sits over the cells it names.
struct TableLayout {
    title: usize,
    /// One width per entry of [`crate::config_center::columns`].
    cells: Vec<usize>,
}

impl TableLayout {
    fn cell(&self, index: usize) -> usize {
        self.cells.get(index).copied().unwrap_or(self.title)
    }
}

/// One cell of the table: truncated to its column and padded to it.
///
/// Padding counts display columns, not characters, because `format!("{:<n$}")`
/// pads by `char` — a CJK label is two columns wide and would leave the cells
/// under it one column short each, which is exactly the drift a table cannot
/// have.
fn config_cell(text: &str, width: usize) -> String {
    pad_to_width(&truncate_to_width(text, width, "…"), width)
}

/// The width of the status column, which sits between the title and the cells.
const CHIP_COLUMN: usize = 12;

/// What the table's leading columns cost: the focus bar, the status mark and
/// the space before the title. The status chip is a column of its own, counted
/// separately in [`column_widths`].
const TABLE_PREFIX: usize = 3;

/// Share the row out: the title takes a third of what is left, and every named
/// column takes an equal share of the rest, so no column is squeezed to the
/// point where its heading is the only thing that fits.
/// Which declared columns a Provider table can still show at this width.
///
/// The design drops `AGENTS` below 100 columns and everything except `STATE`
/// below 80. Other sections keep their columns: their headings are already the
/// shortest name for the fact, and dropping one would hide the page's subject
/// rather than a detail.
pub(crate) fn visible_column_indices(
    section: crate::config_center::ConfigSection,
    width: u16,
    total: usize,
) -> Vec<usize> {
    use crate::config_center::ConfigSection;
    if section != ConfigSection::Providers {
        return (0..total).collect();
    }
    if width < 80 {
        // Upstreams / Keys / State / Agents: keep the state column.
        return (2 < total).then_some(2).into_iter().collect();
    }
    if width < 100 {
        return (0..total.saturating_sub(1)).collect();
    }
    (0..total).collect()
}

fn column_widths(width: u16, columns: &[&'static str]) -> TableLayout {
    let total = usize::from(width);
    let available = total.saturating_sub(TABLE_PREFIX + CHIP_COLUMN);
    if columns.is_empty() {
        return TableLayout {
            title: available,
            cells: Vec::new(),
        };
    }
    // The title names the record, so it keeps the widest fixed share; the rest
    // is split evenly. Both have a floor, because a column narrower than its
    // own heading is a column the reader cannot read.
    let title = (available / 3).clamp(12, 36);
    let each = available.saturating_sub(title) / columns.len();
    let each = each.max(8);
    TableLayout {
        title,
        cells: vec![each; columns.len()],
    }
}

fn chip_style(tone: crate::config_center::ConfigTone, theme: &TuiTheme) -> Style {
    use crate::config_center::ConfigTone;
    let color = match tone {
        ConfigTone::Ok => theme.roles.success,
        ConfigTone::Warn => theme.roles.warning,
        ConfigTone::Bad => theme.roles.danger,
        ConfigTone::Muted => theme.roles.gray,
    };
    Style::default().fg(color)
}

/// The detail pane: what the highlighted record is, in full.
fn render_config_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    rows: &[crate::config_center::ConfigRow],
    theme: &TuiTheme,
    strings: Strings,
) {
    let Some(row) = rows.get(app.config_center.cursor(rows.len())) else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", strings.config_no_selection()),
                Style::default().fg(theme.roles.gray_dim),
            ))),
            area,
        );
        return;
    };
    // The detail belongs to the records pane, so its title carries the same
    // focus bar: the pane is lit across both halves or not at all.
    let focused = app.config_center.zone == crate::config_center::ConfigZone::Rows;
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            if focused { "▌" } else { " " },
            Style::default().fg(theme.roles.accent_user),
        ),
        Span::styled(
            row.title.clone(),
            Style::default()
                .fg(theme.roles.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", strings.config_detail()),
            Style::default().fg(theme.roles.gray_dim),
        ),
    ]));
    // The badge the row wears, on the pane too: the table and the pane are one
    // record read two ways, and a reader whose eyes are in the pane should not
    // have to look back at the row to find out what state it is in.
    if !row.chip.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ".repeat(DETAIL_PREFIX), Style::default()),
            Span::styled(
                format!(" {} ", row.chip),
                chip_style(row.tone, theme).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    if let Some(summary) = &row.summary {
        for line in textwrap(summary, usize::from(area.width).saturating_sub(4)) {
            lines.push(Line::from(Span::styled(
                format!("   {line}"),
                Style::default().fg(theme.roles.text_secondary),
            )));
        }
    }
    let label_width = 14;
    for (label, value) in &row.detail {
        let mut parts = value.lines();
        let first = parts.next().unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "   {:<label_width$}",
                    truncate_to_width(label, label_width, "…")
                ),
                Style::default().fg(theme.roles.gray_dim),
            ),
            Span::styled(
                truncate_to_width(
                    first,
                    usize::from(area.width).saturating_sub(label_width + 4),
                    "…",
                ),
                Style::default().fg(theme.roles.text_secondary),
            ),
        ]));
        for line in parts.take(4) {
            lines.push(Line::from(Span::styled(
                format!(
                    "   {:<label_width$}{}",
                    "",
                    truncate_to_width(
                        line,
                        usize::from(area.width).saturating_sub(label_width + 4),
                        "…"
                    )
                ),
                Style::default().fg(theme.roles.text_secondary),
            )));
        }
    }
    let scroll = usize::from(app.config_center.detail_scroll).min(lines.len().saturating_sub(1));
    let visible = lines
        .into_iter()
        .skip(scroll)
        .take(usize::from(area.height))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), area);
}

/// The footer: which pane owns the keyboard, what the highlighted record can
/// do as buttons the mouse can press, and whatever the last write had to say.
fn render_config_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    rows: &[crate::config_center::ConfigRow],
    theme: &TuiTheme,
    strings: Strings,
) {
    let row = rows.get(app.config_center.cursor(rows.len()));
    // The focused pane is named here as well as lit above: the reader who has
    // lost track of where the keys are going reads it rather than deducing it
    // from a colour.
    let focused = app.config_center.zone;
    let badge = format!(" {} · {} ", strings.config_focus(), focused.label(strings));
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        badge.clone(),
        Style::default()
            .fg(theme.roles.background)
            .bg(theme.roles.accent_user)
            .add_modifier(Modifier::BOLD),
    )];
    let mut used = display_width(&badge) as u16;
    // Cloned rather than borrowed: the buttons below are written into the app's
    // own regions, which a live borrow of the pending map would forbid.
    let pending = row.and_then(|row| app.config_center.pending.get(&row.key).cloned());
    // A record whose write is in flight says what it is doing where its verbs
    // were. The buttons are the one thing on this line that would do nothing
    // while it works, and the phrase is the answer to the question the reader
    // is actually asking — whether their key landed at all.
    if let Some(pending) = &pending {
        let label = format!(
            " {} {} ",
            crate::glyphs::frame_at(
                crate::glyphs::spinner_frames(app.glyph_tier()),
                app.animation_phase(),
                crate::glyphs::SPINNER_TICKS_PER_FRAME,
            ),
            pending.verb.running(strings),
        );
        let width = display_width(&label) as u16;
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme.roles.warning)
                .add_modifier(Modifier::BOLD),
        ));
        used += width;
    }
    // What the last write answered, before the verbs rather than after them:
    // the answer is the whole reason the reader looks at this line, and a
    // notice squeezed in behind eight buttons is cut down to "Codex: Install…",
    // which says nothing the row did not already say. The verbs are still one
    // `Enter` away when they are the ones that do not fit.
    if let Some((notice, tone)) = &app.config_center.notice {
        let text = truncate_to_width(
            notice,
            usize::from(area.width).saturating_sub(usize::from(used) + 2),
            "…",
        );
        if !text.is_empty() {
            spans.push(Span::styled("  ", Style::default()));
            let width = display_width(&text) as u16;
            spans.push(Span::styled(text, chip_style(*tone, theme)));
            used += width + 2;
        }
    }
    if pending.is_none()
        && let Some(row) = row
    {
        for action in &row.actions {
            let label = format!(" {} ", action.label(strings));
            let width = display_width(&label) as u16;
            if used + width + 1 > area.width {
                break;
            }
            app.regions.config_buttons.push((
                Rect {
                    x: area.x + used,
                    y: area.y,
                    width,
                    height: 1,
                },
                *action,
            ));
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(theme.roles.gray_bright)
                    .bg(theme.roles.surface),
            ));
            spans.push(Span::raw(" "));
            used += width + 1;
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Wrap one line of prose to `width` columns, on word boundaries where it can.
fn textwrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if display_width(&current) + 1 + display_width(word) <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The pairing link, cut into the rows it will be drawn on.
///
/// [`textwrap`] breaks at whitespace and a link has none: a URL of a kilobyte
/// comes back from it as one piece, which the paragraph then wraps into a dozen
/// rows nobody counted. These rows are counted — the QR picture is placed by
/// them and the link is given whatever the picture leaves — so the link is
/// measured rather than word-wrapped.
fn wrap_link(url: &str, width: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = url;
    while !rest.is_empty() && width > 0 {
        let (piece, tail) = take_width(rest, width);
        if piece.is_empty() {
            break;
        }
        pieces.push(piece);
        rest = tail;
    }
    pieces
}

/// The devices page: pairing on one half, the trust store on the other.
///
/// Pairing a device and checking that it arrived are one job — the reader opens
/// this page to let something in, and the list of what is already in is how
/// they check that it worked — so the two share a page and are separated by a
/// tab strip rather than by a palette verb.
///
/// The pairing half lists the ways *this* runtime can be paired and nothing
/// else. A phone always scans; a phone on the same network can also be found
/// when the runtime can advertise itself; a second desktop can only be handed a
/// code or a link by a headless runtime, because a desktop shell pairs phones
/// and does not accept another desktop as a client. A row that cannot work is
/// absent rather than disabled: a page that lists a verb the runtime will
/// refuse teaches the reader to distrust the page.
fn render_devices(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.management_devices(), true);
    if inner.width < 24 || inner.height < 4 {
        empty_state(frame, inner, theme, strings.devices_pair_unavailable());
        return;
    }
    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    render_device_tabs(frame, bands[0], app, theme, strings);
    // The rule under the strip is the strip's own bottom edge, in the
    // configuration centre's language: the tabs are a strip rather than a pane,
    // so the rule belongs to them.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(bands[1].width)),
            Style::default().fg(theme.roles.gray_dim),
        ))),
        bands[1],
    );
    match app.device_center.tab {
        crate::device_center::DeviceTab::Pair => {
            render_pair_rows(frame, bands[2], app, theme, strings);
        }
        crate::device_center::DeviceTab::Devices => {
            render_paired_devices(frame, bands[2], app, theme, strings);
        }
        crate::device_center::DeviceTab::Publishing => {
            render_publications(frame, bands[2], app, theme, strings);
        }
    }
    render_device_footer(frame, bands[3], app, theme, strings);
}

/// The page's halves, drawn as the configuration centre draws its modules.
///
/// What sits on the right is the runtime this page pairs and what kind it is.
/// The kind is why a row is missing, so it is worth naming before the reader
/// wonders where the desktop row went.
fn render_device_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0u16;
    for tab in app.device_tabs() {
        let active = tab == app.device_center.tab;
        let label = format!(" {} ", tab.label(strings));
        let width = display_width(&label) as u16;
        if used + width > area.width {
            break;
        }
        let style = if active {
            Style::default()
                .fg(theme.roles.background)
                .bg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        app.regions.device_tabs.push((
            Rect {
                x: area.x + used,
                y: area.y,
                width,
                height: 1,
            },
            tab,
        ));
        spans.push(Span::styled(label, style));
        spans.push(Span::styled(" ", Style::default()));
        used += width + 1;
    }
    let kind = runtime_kind_label(app.runtime_kind(), strings);
    let kind_label = format!("{kind}  ");
    let kind_width = display_width(&kind_label) as u16;
    if used + kind_width + 2 < area.width {
        spans.push(Span::raw(
            " ".repeat(usize::from(area.width - used - kind_width - 1)),
        ));
        spans.push(Span::styled(
            kind_label,
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The runtime's kind, as the tab strip names it.
fn runtime_kind_label(kind: vibex_core::RemoteServerKind, strings: Strings) -> &'static str {
    match kind {
        vibex_core::RemoteServerKind::Desktop => strings.devices_runtime_desktop(),
        vibex_core::RemoteServerKind::Headless => strings.devices_runtime_headless(),
        vibex_core::RemoteServerKind::Unknown => strings.devices_runtime_unknown(),
    }
}

/// The pairing half: one row per way to pair, with the open row's panel under
/// it.
fn render_pair_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let methods = app.device_pair_methods();
    if methods.is_empty() {
        // "This runtime cannot pair one" and "your grant does not allow it" are
        // different answers, and only one of them is the reader's to fix. A
        // third is the one worth naming most: the runtime can pair, and there is
        // no published route for it to pair over — which is a thing the reader
        // can go and switch on.
        let message = match app.availability(vibex_backend::BackendOperation::DevicePairing) {
            Availability::RequiresPermission => strings.permission_required_for(),
            _ if app.device_publications_available()
                && app.device_center.publications.loaded
                && !app.device_center.publications.any_reachable() =>
            {
                // Two sentences, because the state and the way out of it are
                // both the answer: this is the page a reader reaches when
                // pairing does not work, and "no route is published" without
                // "turn one on" is a diagnosis with no treatment.
                strings.devices_pair_no_route()
            }
            _ => strings.devices_pair_unavailable(),
        };
        empty_state(frame, area, theme, message);
        return;
    }
    let selected = app.device_cursor();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(usize, usize)> = Vec::new();
    for (index, method) in methods.iter().enumerate() {
        let open = app.device_center.open == Some(*method);
        rows.push((lines.len(), index));
        lines.push(pair_row_line(
            *method,
            index == selected,
            open,
            area.width,
            app.glyph_tier(),
            theme,
            strings,
        ));
        if open {
            push_pair_panel(&mut lines, *method, app, area, theme, strings);
        }
    }
    for (offset, index) in rows {
        if offset >= usize::from(area.height) {
            continue;
        }
        app.regions.device_rows.push((
            Rect {
                x: area.x,
                y: area.y + offset as u16,
                width: area.width,
                height: 1,
            },
            index,
        ));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

/// One line, cut to the width the band can draw.
///
/// A line that fits is returned whole; one that does not ends in an ellipsis,
/// which is what keeps a row from becoming two.
fn truncate_line(line: Line<'static>, width: u16) -> Line<'static> {
    let width = usize::from(width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        if used >= width {
            break;
        }
        let text = truncate_to_width(&span.content, width - used, "…");
        used += display_width(&text);
        spans.push(Span::styled(text, span.style));
    }
    Line::from(spans)
}

/// One way to pair: the reader's action, and who can take it.
fn pair_row_line(
    method: crate::device_center::PairMethod,
    selected: bool,
    open: bool,
    width: u16,
    tier: crate::glyphs::GlyphTier,
    theme: &TuiTheme,
    strings: Strings,
) -> Line<'static> {
    let scope = method.scope(strings);
    let label_width = usize::from(width)
        .saturating_sub(6 + display_width(scope))
        .max(8);
    let label = truncate_to_width(method.label(strings), label_width, "…");
    Line::from(vec![
        // The bar marks the row the keyboard is on; the others are indented by
        // its width so the labels line up.
        Span::styled(
            if selected {
                format!("{} ", crate::glyphs::accent_bar(tier))
            } else {
                "  ".to_string()
            },
            Style::default().fg(theme.roles.accent_user),
        ),
        Span::styled(
            format!("{} ", crate::glyphs::disclosure(open, tier)),
            Style::default().fg(theme.roles.gray_dim),
        ),
        Span::styled(
            format!("{label:<label_width$}"),
            if selected {
                theme.selected()
            } else {
                theme.base()
            },
        ),
        Span::styled(
            format!("  {scope}"),
            Style::default().fg(theme.roles.gray_dim),
        ),
    ])
}

/// The panel under an open pairing row: what the reader does with the way they
/// opened.
fn push_pair_panel(
    lines: &mut Vec<Line<'static>>,
    method: crate::device_center::PairMethod,
    app: &mut App,
    area: Rect,
    theme: &TuiTheme,
    strings: Strings,
) {
    use crate::device_center::PairMethod;
    let pairing = &app.device_center.pairing;
    let indent = "    ";
    if pairing.busy == Some(method) {
        let frames = crate::glyphs::spinner_frames(app.glyph_tier());
        let frame = crate::glyphs::frame_at(frames, app.animation_phase(), 2);
        lines.push(Line::from(Span::styled(
            format!("{indent}{frame} {}", strings.devices_pair_working()),
            theme.muted(),
        )));
        return;
    }
    if let Some(error) = pairing
        .error
        .as_ref()
        .filter(|error| error.method == method)
    {
        lines.push(Line::from(Span::styled(
            format!("{indent}{}", pairing_error_copy(strings, error)),
            theme.danger(),
        )));
        return;
    }
    match method {
        PairMethod::QrCode | PairMethod::CodeOrLink => {
            push_offer_panel(lines, method, app, area, theme, strings);
        }
        PairMethod::LocalNetwork => push_window_panel(lines, app, theme, strings),
    }
}

/// The offer, drawn as a QR code for a phone and as a link for a desktop.
fn push_offer_panel(
    lines: &mut Vec<Line<'static>>,
    method: crate::device_center::PairMethod,
    app: &mut App,
    area: Rect,
    theme: &TuiTheme,
    strings: Strings,
) {
    use crate::device_center::PairMethod;
    let indent = "    ";
    let text_width = usize::from(area.width).saturating_sub(indent.len()).max(8);
    let now_ms = vibex_core::unix_timestamp_ms();
    if app.device_center.pairing.offer_was_claimed() {
        // A claimed code is not an expired one: the reader's device is in, and
        // the panel says so rather than counting down to a deadline that no
        // longer means anything.
        lines.push(Line::from(Span::styled(
            format!("{indent}{}", strings.devices_offer_claimed()),
            theme.success(),
        )));
        return;
    }
    let Some(offer) = app.device_center.pairing.live_offer(now_ms) else {
        // A code the runtime has stopped accepting is not a code: the row says
        // so, and the next press mints a new one.
        let message = if app.device_center.pairing.offer.is_some() {
            strings.devices_offer_expired()
        } else {
            strings.devices_pair_hint()
        };
        lines.push(Line::from(Span::styled(
            format!("{indent}{message}"),
            theme.muted(),
        )));
        return;
    };
    // What the reader has to hand decides which half of the panel matters: a
    // phone reads the code, a desktop pastes the link.
    let hint = if method == PairMethod::QrCode {
        strings.devices_qr_hint()
    } else {
        strings.devices_link_hint()
    };
    lines.push(Line::from(Span::styled(
        format!("{indent}{hint}"),
        theme.accent(),
    )));
    push_offer_route(lines, offer, area, theme, strings);
    // The link is what the code encodes and what a desktop pastes, so it is
    // wrapped once here: the picture decides how many of its lines are drawn.
    let url = offer.url();
    let link = wrap_link(&url, text_width);
    if method == PairMethod::QrCode {
        // A terminal that draws pictures is handed the code itself. The
        // half-block code below needs a terminal some 173 columns wide, which
        // is not a terminal anyone has, so this is the row's real answer.
        let drawn = app.terminal_images.supports_pixels()
            && push_qr_picture(lines, &mut app.pairing_qr, &app.terminal_images, area, &url);
        if !drawn {
            // The QR is the whole point of this row, so it is drawn first and
            // the link below it is the fallback. A terminal that cannot hold it
            // says so rather than drawing a code no camera could read: a code
            // that is cut off is worse than no code, because it looks
            // scannable.
            //
            // The hint above, the code line, the link and its label, the
            // closing hint: everything the QR must leave room for.
            let reserved = 4 + link.len();
            let room = usize::from(area.height)
                .saturating_sub(lines.len())
                .saturating_sub(reserved);
            let blocks = qr_blocks(&url, text_width, room, app.glyph_tier());
            match blocks {
                Some(blocks) => {
                    for block in blocks {
                        lines.push(Line::from(Span::styled(
                            format!("{indent}{block}"),
                            theme.base(),
                        )));
                    }
                }
                None => {
                    // Two different reasons, two different answers: a terminal
                    // whose font has no half blocks cannot draw a code at any
                    // size, and one that is merely small can be widened. A
                    // terminal that draws pictures is only ever here because
                    // the panel had no room for one.
                    let message = if app.glyph_tier() == crate::glyphs::GlyphTier::Full
                        || app.terminal_images.supports_pixels()
                    {
                        strings.devices_qr_no_room()
                    } else {
                        strings.devices_qr_no_glyphs()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{indent}{message}"),
                        theme.warning(),
                    )));
                }
            }
        }
    }
    let permission = match offer.permission {
        vibex_core::RemoteDevicePermissionLevel::ReadOnly => strings.permission_read_only(),
        vibex_core::RemoteDevicePermissionLevel::ApproveOnly => strings.permission_approve_only(),
        vibex_core::RemoteDevicePermissionLevel::FullControl => strings.permission_full_control(),
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("{indent}{}: ", strings.devices_pairing_code()),
            theme.muted(),
        ),
        Span::styled(
            offer.code.clone(),
            theme.strong().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}: ", strings.devices_permission()),
            theme.muted(),
        ),
        Span::styled(permission.to_string(), theme.accent()),
    ]));
    lines.push(Line::from(Span::styled(
        format!("{indent}{}:", strings.devices_link()),
        theme.muted(),
    )));
    // As much of the link as the room left: a picture can take most of the
    // panel, and one line of the link is the floor — a reader who cannot scan
    // the code is the one who needs it. What is cut says so, and the full link
    // is on the row that exists for it.
    let room = usize::from(area.height).saturating_sub(lines.len() + 1);
    let shown = link.len().min(room.max(1));
    for (index, piece) in link.iter().enumerate().take(shown) {
        let cut = index + 1 == shown && shown < link.len();
        let text = if cut {
            truncate_to_width(piece, text_width.saturating_sub(1), "…")
        } else {
            piece.clone()
        };
        lines.push(Line::from(Span::styled(
            format!("{indent}{text}"),
            theme.muted(),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("{indent}{}", strings.devices_code_hint()),
        theme.warning(),
    )));
}

/// The route the code and the link below it carry.
///
/// One line, in a stable column, because the reader's question is "can the phone
/// in my hand reach the address this code names" — a question the transport word
/// and the origin answer together. With a single published route there is
/// nothing to choose, so the line names it and the choice key stays out of the
/// bar; with several, the position is shown so the reader knows the `]` has
/// somewhere to go.
fn push_offer_route(
    lines: &mut Vec<Line<'static>>,
    offer: &crate::device_center::PairingOffer,
    area: Rect,
    theme: &TuiTheme,
    strings: Strings,
) {
    let indent = "    ";
    let Some(route) = offer.selected_route() else {
        // No route published and an offer in hand is a contradiction the client
        // does not try to resolve: the runtime is the side that decides whether
        // a code can be claimed, so the link is drawn and its origin is not
        // invented.
        return;
    };
    let transport_width = usize::from(area.width) / 4;
    let transport = crate::device_center::pairing_transport_name(route.transport);
    let label = format!("{}: ", strings.devices_route());
    // The origin is what is left after the label and the transport, and one
    // column of it is still worth drawing: a route the reader cannot read is a
    // route they cannot judge.
    let origin_width = usize::from(area.width)
        .saturating_sub(indent.len() + display_width(&label) + transport_width + 6)
        .max(1);
    lines.push(Line::from(vec![
        Span::styled(format!("{indent}{label}"), theme.muted()),
        Span::styled(format!("{:<transport_width$}", transport), theme.base()),
        Span::styled(
            truncate_to_width(&route.origin, origin_width, "…"),
            Style::default().fg(theme.roles.gray_dim),
        ),
    ]));
    if !offer.route_is_chosen() {
        return;
    }
    let position = format!(
        "{} {}/{}",
        strings.devices_route_choice(),
        offer.selected + 1,
        offer.route_count()
    );
    lines.push(Line::from(Span::styled(
        format!("{indent}{position}"),
        theme.muted(),
    )));
}

/// The publishing half: how this runtime can be reached, and what is wrong with
/// the ways that are not up.
///
/// Every row is a statement rather than a verb. Switching one of these on moves
/// a Tailscale Serve config, binds a port, or enrolls a relay, and the desktop
/// Remote Access page is where a reader can see the certificate, the port, and
/// the ownership that decision turns on. A terminal that offered the switch
/// without those facts would be offering a verb it cannot explain.
fn render_publications(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if !app.device_publications_available() {
        empty_state(
            frame,
            area,
            theme,
            strings.devices_publication_unavailable(),
        );
        return;
    }
    let publications = &app.device_center.publications;
    if let Some(code) = publications.error.clone() {
        let message = if code == "remote_permission_denied" {
            strings.permission_required_for()
        } else {
            strings.devices_publication_unavailable()
        };
        empty_state(frame, area, theme, message);
        return;
    }
    if !publications.loaded {
        empty_state(frame, area, theme, strings.loading());
        return;
    }
    if publications.rows.is_empty() {
        // Two lines: the state, and the one thing the reader can do about it.
        // The empty page is the case this whole read exists for, so it is the
        // one that must not be a bare assertion.
        let lines = vec![
            Line::from(Span::styled(
                strings.devices_publication_empty().to_string(),
                theme.muted(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("    {}", strings.devices_publication_empty_hint()),
                theme.warning(),
            )),
        ];
        frame.render_widget(
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let selected = app.device_cursor();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(usize, usize)> = Vec::new();
    for (index, row) in publications.rows.iter().enumerate() {
        rows.push((lines.len(), index));
        lines.push(publication_row_line(
            row,
            index == selected,
            area.width,
            app.glyph_tier(),
            theme,
            strings,
        ));
    }
    if let Some(row) = publications.row(selected) {
        lines.push(Line::from(""));
        for line in publication_detail(row, theme, strings) {
            lines.push(line);
        }
        lines.push(Line::from(Span::styled(
            format!("    {}", strings.devices_publications_hint()),
            Style::default()
                .fg(theme.roles.gray_dim)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    for (offset, index) in rows {
        if offset >= usize::from(area.height) {
            continue;
        }
        app.regions.device_rows.push((
            Rect {
                x: area.x,
                y: area.y + offset as u16,
                width: area.width,
                height: 1,
            },
            index,
        ));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

/// One publishing method: the reader's name for it, and where it stands.
fn publication_row_line(
    row: &vibex_core::RemotePairingPublicationSummary,
    selected: bool,
    width: u16,
    tier: crate::glyphs::GlyphTier,
    theme: &TuiTheme,
    strings: Strings,
) -> Line<'static> {
    let method = publication_method_label(row.method, strings).to_string();
    let state = publication_state_label(row, strings).to_string();
    let state_width = usize::from(width) / 3;
    let label_width = usize::from(width).saturating_sub(6 + state_width).max(8);
    Line::from(vec![
        Span::styled(
            if selected {
                format!("{} ", crate::glyphs::accent_bar(tier))
            } else {
                "  ".to_string()
            },
            Style::default().fg(theme.roles.accent_user),
        ),
        Span::styled(
            format!(
                "{:<label_width$}",
                truncate_to_width(&method, label_width, "…")
            ),
            if selected {
                theme.selected()
            } else {
                theme.base()
            },
        ),
        Span::styled(
            format!(
                "  {:<state_width$}",
                truncate_to_width(&state, state_width, "…")
            ),
            publication_state_style(row, theme),
        ),
    ])
}

/// What the highlighted method is, in the runtime's own words.
///
/// A method that is off because nobody enabled it and one that is off because it
/// failed are different facts, and only one of them is the reader's to act on.
fn publication_detail(
    row: &vibex_core::RemotePairingPublicationSummary,
    theme: &TuiTheme,
    strings: Strings,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(origin) = row.origin.as_ref() {
        lines.push(Line::from(vec![
            Span::styled(format!("    {}: ", strings.devices_link()), theme.muted()),
            Span::styled(origin.clone(), theme.base()),
        ]));
    }
    if let Some(code) = row.error_code.as_ref() {
        lines.push(Line::from(Span::styled(
            format!("    {code}"),
            theme.danger(),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("    {}", publication_recovery_copy(row, strings)),
        publication_state_style(row, theme),
    )));
    lines
}

/// The name the desktop Remote Access page uses for one publishing method.
fn publication_method_label(
    method: vibex_core::RemotePairingPublicationMethod,
    strings: Strings,
) -> &'static str {
    use vibex_core::RemotePairingPublicationMethod as Method;
    match method {
        Method::TailscaleServe => strings.devices_publication_method_tailscale(),
        Method::Direct => strings.devices_publication_method_direct(),
        Method::SelfHostedRelay => strings.devices_publication_method_relay(),
        Method::Unknown => strings.devices_publication_method_unknown(),
    }
}

/// Where one method stands, and whether the operator asked for it.
///
/// "Off" and "off, and nobody switched it on" are different sentences: the
/// first is a route that failed to come up, the second is one the reader can
/// decide to want.
fn publication_state_label(
    row: &vibex_core::RemotePairingPublicationSummary,
    strings: Strings,
) -> &'static str {
    use vibex_core::RemotePairingPublicationState as State;
    match row.state {
        State::Disabled if !row.desired_enabled => strings.devices_publication_never_enabled(),
        State::Disabled => strings.devices_publication_state_disabled(),
        State::Checking => strings.devices_publication_state_checking(),
        State::ConfirmationNeeded => strings.devices_publication_state_confirmation_needed(),
        State::Enabling => strings.devices_publication_state_enabling(),
        State::Online => strings.devices_publication_state_online(),
        State::Degraded => strings.devices_publication_state_degraded(),
        State::RepairRequired => strings.devices_publication_state_repair_required(),
        State::Conflict => strings.devices_publication_state_conflict(),
        State::Stopping => strings.devices_publication_state_stopping(),
        State::Error => strings.devices_publication_state_error(),
        State::Unknown => strings.devices_publication_state_unknown(),
    }
}

fn publication_state_style(
    row: &vibex_core::RemotePairingPublicationSummary,
    theme: &TuiTheme,
) -> Style {
    use vibex_core::RemotePairingPublicationState as State;
    match row.state {
        State::Online => theme.success(),
        State::Degraded => theme.warning(),
        State::ConfirmationNeeded | State::RepairRequired | State::Conflict | State::Error => {
            theme.danger()
        }
        _ => Style::default().fg(theme.roles.gray_dim),
    }
}

/// What an operator has to do about a method that is not up.
///
/// The client cannot do any of it, so the sentence names the action and where it
/// happens rather than offering a key that would do nothing.
fn publication_recovery_copy(
    row: &vibex_core::RemotePairingPublicationSummary,
    strings: Strings,
) -> &'static str {
    use vibex_core::RemotePairingPublicationRecovery as Recovery;
    match row.recovery_action {
        Recovery::None if row.state.is_reachable() => strings.devices_publication_hint_ok(),
        Recovery::None => strings.devices_publications_hint(),
        Recovery::ConfirmPort => strings.devices_publication_recovery_confirm_port(),
        Recovery::RepairRoute => strings.devices_publication_recovery_repair_route(),
        Recovery::Configure => strings.devices_publication_recovery_configure(),
        Recovery::Retry => strings.devices_publication_recovery_retry(),
        Recovery::ManualCommand => strings.devices_publication_recovery_manual(),
        Recovery::RePair => strings.devices_publication_recovery_re_pair(),
        Recovery::Unknown => strings.devices_publication_recovery_unknown(),
    }
}

/// The rows a picture of the code is worth drawing in at all.
///
/// Below this the picture is a smear a camera cannot read, and saying the
/// terminal is too small is the honest answer.
const QR_MIN_ROWS: usize = 8;

/// The code as a picture, when the panel has room for one worth scanning.
///
/// `false` means no picture was placed — no room for one, or a link no code can
/// be built for — and the caller falls back to the half-block code, or to the
/// sentence that says why there is none.
///
/// A cell is about twice as tall as it is wide, so a square code takes two
/// columns per row; the rows it reserves are left blank, and the picture is
/// drawn over them by the terminal rather than by this frame.
fn push_qr_picture(
    lines: &mut Vec<Line<'static>>,
    qr: &mut crate::pairing_qr::PairingQr,
    support: &crate::terminal_image::ImageSupport,
    area: Rect,
    url: &str,
) -> bool {
    // The hint above, the code line, the link's label, one line of the link at
    // least, and the closing hint: what the picture has to leave room for.
    const FIXED: usize = 4;
    // The picture is placed by the count of the rows above it, so a row that
    // would wrap has to be cut instead: the panel's own rows are short, and one
    // that became two would put the code over the text. Only these rows are
    // cut — the copy under the code still wraps as far as it needs to.
    for line in lines.iter_mut() {
        *line = truncate_line(std::mem::replace(line, Line::from("")), area.width);
    }
    let room = usize::from(area.height).saturating_sub(lines.len() + FIXED);
    let widest = usize::from(area.width).saturating_sub(2);
    let rows = room.min(widest / 2);
    if rows < QR_MIN_ROWS {
        return false;
    }
    let cols = (rows * 2).min(widest) as u16;
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(cols)) / 2,
        y: area.y + lines.len() as u16,
        width: cols,
        height: rows as u16,
    };
    qr.show(support, url, rect);
    if !qr.is_holding() {
        return false;
    }
    for _ in 0..rows {
        lines.push(Line::from(""));
    }
    true
}

/// The advertisement, and whoever has answered it so far.
fn push_window_panel(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let indent = "    ";
    let pairing = &app.device_center.pairing;
    let now_ms = vibex_core::unix_timestamp_ms();
    let live = pairing.window.as_ref().filter(|_| !pairing.expired(now_ms));
    let Some(window) = live else {
        // No window, or one the runtime has taken down: both are the reader
        // being told there is nothing to answer, and the next press starts a
        // new one.
        lines.push(Line::from(Span::styled(
            format!(
                "{indent}{} — {}",
                strings.devices_lan_expired(),
                strings.devices_lan_restart()
            ),
            theme.warning(),
        )));
        return;
    };
    let remaining = (window.discovery.expires_at_ms - now_ms)
        .div_euclid(1_000)
        .max(0);
    lines.push(Line::from(vec![
        Span::styled(
            format!("{indent}{}", strings.devices_lan_advertising()),
            theme.accent(),
        ),
        Span::styled(
            format!("   {} {remaining}s", strings.devices_lan_closes_in()),
            theme.muted(),
        ),
    ]));
    match pairing.pending() {
        None => lines.push(Line::from(Span::styled(
            format!("{indent}{}", strings.devices_lan_waiting()),
            theme.muted(),
        ))),
        Some(request) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{indent}{}: ", strings.devices_lan_request()),
                    theme.muted(),
                ),
                Span::styled(request.display_name.clone(), theme.strong()),
                Span::styled(
                    format!(" · {}", request.device_fingerprint),
                    theme.dimmed(theme.roles.gray),
                ),
                Span::styled(
                    format!("   {}: ", strings.devices_pairing_code()),
                    theme.muted(),
                ),
                Span::styled(
                    request.verification_code.clone(),
                    theme
                        .strong()
                        .add_modifier(Modifier::BOLD)
                        .fg(theme.roles.accent_user),
                ),
            ]));
            if pairing.waiting() > 1 {
                lines.push(Line::from(Span::styled(
                    format!("{indent}{}", strings.devices_lan_more()),
                    theme.muted(),
                )));
            }
        }
    }
}

/// The QR code for one link, or nothing when the terminal cannot hold it.
///
/// A QR code has to be square in modules, and one character cell is about twice
/// as tall as it is wide, so the half-block renderer puts two module rows in
/// every text row. A link this long needs a large terminal; one that is too
/// small is told so rather than shown a code that cannot be read.
fn qr_blocks(
    url: &str,
    width: usize,
    height: usize,
    tier: crate::glyphs::GlyphTier,
) -> Option<Vec<String>> {
    if tier != crate::glyphs::GlyphTier::Full || width == 0 || height == 0 {
        return None;
    }
    let code = QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L).ok()?;
    let image = code.render::<Dense1x2>().quiet_zone(true).build();
    let blocks: Vec<String> = image.lines().map(str::to_string).collect();
    let block_width = blocks.iter().map(|block| display_width(block)).max()?;
    if block_width > width || blocks.len() > height {
        return None;
    }
    Some(blocks)
}

/// What a failed pairing action means for the reader, and what to do next.
///
/// The runtime's own sentence is the fallback: the codes worth naming are the
/// ones with a recovery the reader can act on, and inventing copy for the rest
/// would be guessing at a cause the runtime already stated.
fn pairing_error_copy(strings: Strings, error: &crate::device_center::PairingError) -> String {
    let recovery = match error.code.as_str() {
        "remote_lan_direct_unavailable" | "remote_lan_tls_incompatible" => {
            Some(strings.devices_error_direct_unavailable())
        }
        "remote_lan_pairing_unavailable" => Some(strings.devices_error_lan_unavailable()),
        "remote_lan_pairing_window_unavailable" | "remote_lan_pairing_request_unavailable" => {
            Some(strings.devices_error_request_settled())
        }
        "remote_permission_denied" => Some(strings.devices_error_permission()),
        _ => None,
    };
    match recovery {
        Some(copy) => format!("{copy} ({})", error.message),
        None => error.message.clone(),
    }
}

/// The trust store: the devices this runtime has paired.
fn render_paired_devices(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    // A backend that does not offer device management says so rather than
    // drawing an empty list: "no devices" and "this runtime cannot pair one"
    // are different answers, and only one of them is the reader's to fix.
    if app.availability(vibex_backend::BackendOperation::DeviceList) != Availability::Available {
        let message = match app.availability(vibex_backend::BackendOperation::DeviceList) {
            Availability::RequiresPermission => strings.permission_required_for(),
            _ => strings.toast_action_unavailable(),
        };
        empty_state(frame, area, theme, message);
        return;
    }
    if app.management_data.devices.is_empty() {
        empty_state(frame, area, theme, strings.devices_empty());
        return;
    }
    let selected = app.device_cursor();
    let items = app
        .management_data
        .devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            let style = if index == selected {
                theme.selected()
            } else {
                theme.base()
            };
            let permission = match device.permission_level {
                vibex_core::RemoteDevicePermissionLevel::ReadOnly => strings.permission_read_only(),
                vibex_core::RemoteDevicePermissionLevel::ApproveOnly => {
                    strings.permission_approve_only()
                }
                vibex_core::RemoteDevicePermissionLevel::FullControl => {
                    strings.permission_full_control()
                }
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<24}", truncate_to_width(&device.display_name, 24, "…")),
                    style,
                ),
                Span::styled(format!("{permission:<14}"), theme.muted()),
                Span::styled(format!("{:?}", device.status), theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
    for (index, _) in app.management_data.devices.iter().enumerate() {
        if index >= usize::from(area.height) {
            break;
        }
        app.regions.device_rows.push((
            Rect {
                x: area.x,
                y: area.y + index as u16,
                width: area.width,
                height: 1,
            },
            index,
        ));
    }
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(app.management_data.devices.len() - 1)));
    frame.render_stateful_widget(List::new(items), area, &mut state);
}

/// The line under the two halves: what the open half counts, or what to press.
fn render_device_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let text = match app.device_center.tab {
        crate::device_center::DeviceTab::Pair => strings.devices_pair_hint().to_string(),
        crate::device_center::DeviceTab::Devices => format!(
            "{}: {}",
            strings.devices_audit(),
            app.management_data.audit.len()
        ),
        // The publishing half has no verb of its own, so the line says where
        // the verb lives rather than naming a key that does nothing.
        crate::device_center::DeviceTab::Publishing => {
            strings.devices_publications_hint().to_string()
        }
    };
    frame.render_widget(Paragraph::new(text).style(theme.muted()), area);
}

fn render_settings(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.nav_settings(), true);
    // Row 0 is the search bar (always present, so the filter is discoverable
    // before it is used), row 1 a rule, the rest the grouped list, and the last
    // row explains whichever mode is active.
    if inner.height < 3 {
        empty_state(frame, inner, theme, strings.settings_no_matches());
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    render_settings_filter(frame, rows[0], app, theme, strings);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(rows[1].width)),
            Style::default().fg(theme.roles.gray_dim),
        ))),
        rows[1],
    );
    render_settings_rows(frame, rows[2], app, theme, strings);
    render_settings_footer(frame, rows[3], app, theme, strings);
}

/// The settings filter bar.
fn render_settings_filter(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let focused = matches!(app.settings.view, crate::app::SettingsMode::Filter);
    let label = strings.settings_filter_label();
    let caret = if focused { "▏" } else { "" };
    let mut spans = vec![
        Span::styled(
            label,
            Style::default().fg(if focused {
                theme.roles.accent_user
            } else {
                theme.roles.gray
            }),
        ),
        Span::styled(
            app.settings.filter.clone(),
            Style::default().fg(theme.roles.foreground),
        ),
        Span::styled(caret.to_string(), theme.accent()),
    ];
    if app.settings.filter.is_empty() && !focused {
        spans.push(Span::styled(
            strings.filter(),
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The grouped, filtered setting rows.
fn render_settings_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let visible = app.visible_settings();
    if visible.is_empty() {
        empty_state(frame, area, theme, strings.settings_no_matches());
        return;
    }
    let selected_index = app
        .selection_for(Scope::Settings)
        .min(visible.len().saturating_sub(1));
    let selected_row = visible[selected_index];
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut section = None;
    for (index, row) in visible.iter().enumerate() {
        let definition = crate::settings::definition(*row);
        if section != Some(definition.section) {
            if section.is_some() {
                lines.push(Line::from(""));
            }
            section = Some(definition.section);
            let label = definition.section.label(strings);
            let used = display_width(label) + 2;
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(theme.roles.gray)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "─".repeat(usize::from(area.width).saturating_sub(used)),
                    Style::default().fg(theme.roles.gray_dim),
                ),
            ]));
        }
        let selected = index == selected_index;
        let label_style = if selected {
            Style::default()
                .fg(theme.roles.foreground)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.roles.foreground)
        };
        let value = if app.settings.view.row() == Some(*row) {
            match &app.settings.view {
                crate::app::SettingsMode::Editing { buffer, .. } => format!("{buffer}▏"),
                _ => app.setting_value(*row),
            }
        } else {
            app.setting_value(*row)
        };
        let chevron = match crate::settings::definition(*row).kind {
            crate::settings::SettingKind::Choice | crate::settings::SettingKind::Text => "›",
            _ => " ",
        };
        let used = 2 + display_width(app.setting_label(*row));
        let marker = if selected { "▸" } else { " " };
        // The value sits in a stable column so the eye can run down it. A label
        // too long for that column keeps a single space instead of being
        // truncated.
        let value_column = (usize::from(area.width) / 2).clamp(20, 48);
        let mut spans = vec![
            Span::styled(format!("{marker} "), theme.accent()),
            Span::styled(app.setting_label(*row).to_string(), label_style),
        ];
        if used + 2 <= value_column {
            spans.push(Span::styled(
                " ".repeat(value_column - used),
                Style::default(),
            ));
        } else {
            spans.push(Span::styled(" ", Style::default()));
        }
        spans.push(Span::styled(
            value,
            Style::default().fg(if selected {
                theme.roles.accent_user
            } else {
                theme.roles.gray
            }),
        ));
        spans.push(Span::styled(
            format!(" {chevron}"),
            Style::default().fg(theme.roles.gray_dim),
        ));
        lines.push(Line::from(spans));
    }

    // The chooser replaces the list: it is the row's value being decided, and
    // showing both would be two cursors on one screen.
    if let crate::app::SettingsMode::Picking { row, selected, .. } = &app.settings.view {
        let choices = app.setting_choices(*row);
        let mut chooser = vec![Line::from(vec![
            Span::styled("▸ ", theme.accent()),
            Span::styled(
                app.setting_label(*row).to_string(),
                Style::default()
                    .fg(theme.roles.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        for (index, choice) in choices.iter().enumerate() {
            let active = index == *selected;
            chooser.push(Line::from(vec![
                Span::styled(
                    if active { "  ▸ " } else { "    " },
                    Style::default().fg(theme.roles.accent_user),
                ),
                Span::styled(
                    choice.label.clone(),
                    if active {
                        theme.selected()
                    } else {
                        Style::default().fg(theme.roles.foreground)
                    },
                ),
                Span::styled(
                    if choice.current { "  ●" } else { "" },
                    Style::default().fg(theme.roles.accent_success),
                ),
            ]));
        }
        chooser.push(Line::from(""));
        chooser.push(Line::from(Span::styled(
            strings.settings_pick_hint().to_string(),
            Style::default().fg(theme.roles.gray_dim),
        )));
        frame.render_widget(Paragraph::new(Text::from(chooser)), area);
        return;
    }

    // Keep the highlighted row on screen without a scroll offset of its own:
    // a settings list is short, and the selected row is the anchor.
    let height = usize::from(area.height);
    let offset = if lines.len() > height {
        let selected_line = lines
            .iter()
            .position(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content.starts_with('▸'))
            })
            .unwrap_or(0);
        selected_line.saturating_sub(height.saturating_sub(2))
    } else {
        0
    };
    let visible_lines = lines
        .into_iter()
        .skip(offset)
        .take(height)
        .collect::<Vec<_>>();
    // The description of the highlighted row is the last thing shown, because
    // it explains the choice rather than the list.
    let mut text_lines = visible_lines;
    if matches!(app.settings.view, crate::app::SettingsMode::Browse)
        && text_lines.len() + 2 <= height
        && !app.setting_description(selected_row).is_empty()
    {
        text_lines.push(Line::from(""));
        text_lines.push(Line::from(Span::styled(
            format!("  {}", app.setting_description(selected_row)),
            Style::default()
                .fg(theme.roles.gray)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    frame.render_widget(Paragraph::new(Text::from(text_lines)), area);
}

/// The one-line footer that names the keys of the active mode.
fn render_settings_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let hints: Vec<(&str, &str)> = match &app.settings.view {
        crate::app::SettingsMode::Browse => vec![
            ("↑↓", strings.hint_nav()),
            ("Enter", strings.hint_edit()),
            ("Space", strings.hint_toggle()),
            ("/", strings.filter()),
            ("d", strings.hint_reset()),
            ("Esc", strings.close()),
        ],
        crate::app::SettingsMode::Filter => vec![
            ("type", strings.filter()),
            ("Enter", strings.hint_commit()),
            ("Esc", strings.hint_clear()),
        ],
        crate::app::SettingsMode::Picking { .. } => vec![
            ("↑↓", strings.hint_nav()),
            ("Enter", strings.hint_select()),
            ("Esc", strings.hint_revert()),
        ],
        crate::app::SettingsMode::Editing { .. } => vec![
            ("type", strings.hint_edit()),
            ("Enter", strings.hint_commit()),
            ("Esc", strings.cancel()),
        ],
    };
    let rule = Span::styled(
        format!(" {} ", crate::glyphs::accent_bar(app.glyph_tier())),
        Style::default().fg(theme.roles.accent_user),
    );
    let mut spans = vec![rule];
    // The footer's keys are controls like every other legend: a reader who can
    // see `Enter edit` can click it. Each pair is published where it lands, so
    // the press goes through the same dispatcher the key does.
    let mut column = area.x + display_width(&spans[0].content) as u16;
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            let separator = "  │  ";
            spans.push(Span::styled(
                separator,
                Style::default().fg(theme.roles.gray_dim),
            ));
            column += display_width(separator) as u16;
        }
        let text = format!("{key} {label}");
        let width = display_width(&text) as u16;
        let chord = Chord::from_hint(key).filter(|_| column < area.right());
        if let Some(chord) = chord {
            let hovered = app.hovered_key_hint == Some(chord);
            app.regions.key_hints.push(crate::app::KeyHintRegion {
                rect: Rect {
                    x: column,
                    y: area.y,
                    width: width.min(area.right() - column),
                    height: 1,
                },
                chord,
            });
            spans.push(Span::styled(
                format!("{key} "),
                Style::default()
                    .fg(if hovered {
                        theme.roles.accent_user
                    } else {
                        theme.roles.gray_bright
                    })
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("{key} "),
                Style::default()
                    .fg(theme.roles.gray_bright)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            label.to_string(),
            Style::default().fg(theme.roles.gray_dim),
        ));
        column += width;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_help(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.help_title(), true);
    // The filter is a line of the page, like every other list, so what is being
    // typed is visible next to what it matched.
    let list_area = if app.filtering || !app.filter.is_empty() {
        let filter_area = Rect { height: 1, ..inner };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme.roles.accent_user)),
                Span::styled(
                    app.filter.clone(),
                    Style::default().fg(theme.roles.foreground),
                ),
                Span::styled(
                    if app.filtering { "▏" } else { "" },
                    Style::default().fg(theme.roles.accent_user),
                ),
            ])),
            filter_area,
        );
        Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        }
    } else {
        inner
    };
    let needle = app.filter.trim().to_lowercase();
    // Help is generated from the binding tables, so filtering it is filtering
    // the same data that dispatches the keys — and it names the key that
    // answers here rather than the first row of the table that shares the
    // intent.
    let bindings = app
        .advertised_keys()
        .into_iter()
        .map(|(_, binding)| binding)
        .filter(|binding| {
            needle.is_empty()
                || binding.chord.display().to_lowercase().contains(&needle)
                || app.binding_label(binding).to_lowercase().contains(&needle)
                || binding.intent.id().contains(&needle)
                || binding
                    .intent
                    .help(app.strings)
                    .to_lowercase()
                    .contains(&needle)
        })
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        empty_state(frame, list_area, theme, strings.help_no_keys());
        return;
    }
    let items = bindings
        .iter()
        .map(|binding| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<14}", binding.chord.display()), theme.accent()),
                Span::styled(app.binding_label(binding).to_string(), theme.base()),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items), list_area);
}

/// Split one composer line into spans, lifting a collapsed paste out of it.
///
/// A chip is a single object visually as well as in the buffer: the brackets
/// are dim and the label is coloured, so a draft with a log in it reads as
/// "a log is attached" rather than "the draft begins with a strange sentence".
/// One display row of the draft, styled in runs.
///
/// The row arrives as plain text plus where it starts in the buffer, so chip
/// labels and the selection can be recognised by byte offset rather than by
/// scanning for marker strings. Three styles can apply to one grapheme and they
/// have a fixed precedence: the selection wins over a chip, and a chip wins
/// over the surrounding prose — a highlight the reader made must never be
/// hidden by chrome.
fn composer_line_spans(
    text: &str,
    style: Style,
    theme: &TuiTheme,
    source_start: usize,
    chips: &[(usize, usize)],
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let chip_style = Style::default()
        .fg(theme.roles.accent_attention)
        .add_modifier(Modifier::BOLD);
    let selection_style = selection_style(theme);
    let style_for = |offset: usize| -> Style {
        let absolute = source_start + offset;
        if selection.is_some_and(|(start, end)| absolute >= start && absolute < end) {
            selection_style
        } else if chips
            .iter()
            .any(|(start, end)| absolute >= *start && absolute < *end)
        {
            chip_style
        } else {
            style
        }
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = style;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_style = style_for(offset);
        if grapheme_style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = grapheme_style;
        run.push_str(grapheme);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), style));
    }
    spans
}

/// One row of the key-binding editor: a scope heading or one binding.
pub struct KeyRow<'a> {
    pub header: Option<Scope>,
    pub binding: Option<&'a crate::keymap::Binding>,
}

/// The rows the editor shows, grouped by scope in declaration order.
///
/// The query matches the chord, the action label, its identifier and its help
/// text, so "ctrl" and "queue" are both useful searches. A heading appears only
/// when something under it matched.
pub fn key_editor_rows<'a>(app: &'a App, query: &str) -> Vec<KeyRow<'a>> {
    let needle = query.trim().to_lowercase();
    let matches = |binding: &crate::keymap::Binding| {
        if needle.is_empty() {
            return true;
        }
        binding.chord.display().to_lowercase().contains(&needle)
            || app.binding_label(binding).to_lowercase().contains(&needle)
            || binding.intent.id().contains(&needle)
            || binding
                .intent
                .help(app.strings)
                .to_lowercase()
                .contains(&needle)
            || binding.scope.id().contains(&needle)
    };
    let mut rows = Vec::new();
    for scope in Scope::ALL {
        let bindings = app
            .keymap
            .bindings()
            .iter()
            .filter(|binding| binding.scope == *scope && binding.label.is_some())
            .filter(|binding| matches(binding))
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            continue;
        }
        rows.push(KeyRow {
            header: Some(*scope),
            binding: None,
        });
        rows.extend(bindings.into_iter().map(|binding| KeyRow {
            header: None,
            binding: Some(binding),
        }));
    }
    rows
}

/// The next row in `delta` direction that is a binding rather than a heading.
///
/// A heading is a label, not a target: `Enter` on one would have nothing to
/// rebind, so the cursor steps over it.
pub fn step_key_row(rows: &[KeyRow<'_>], from: usize, delta: isize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let mut index = from.min(rows.len() - 1) as isize;
    loop {
        index += delta;
        if index < 0 || index as usize >= rows.len() {
            return from.min(rows.len() - 1);
        }
        if rows[index as usize].binding.is_some() {
            return index as usize;
        }
    }
}

/// One row of the shortcuts cheatsheet.
pub struct ShortcutRow<'a> {
    /// A category header, when the row is not a binding.
    pub header: Option<crate::keymap::Category>,
    pub binding: Option<&'a crate::keymap::Binding>,
}

/// The rows the cheatsheet shows: category headers, then the bindings that are
/// not folded away and that match the query.
pub fn shortcut_rows<'a>(
    app: &'a App,
    query: &str,
    collapsed: &std::collections::BTreeSet<String>,
) -> Vec<ShortcutRow<'a>> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    for category in crate::keymap::Category::ALL {
        let mut bindings = app
            .keymap
            .bindings()
            .iter()
            .filter(|binding| binding.label.is_some() && binding.scope.category() == category)
            // A verb with no subject is not a shortcut the reader can use: the
            // cheatsheet names what this client can do *now*, so the ownership
            // rows appear with the sessions that have something to own.
            .filter(|binding| app.ownership_key_applies(binding.intent))
            .filter(|binding| {
                needle.is_empty()
                    || binding.chord.display().to_lowercase().contains(&needle)
                    || app.binding_label(binding).to_lowercase().contains(&needle)
                    || binding.intent.id().contains(&needle)
                    || binding
                        .intent
                        .help(app.strings)
                        .to_lowercase()
                        .contains(&needle)
                    || binding.scope.id().contains(&needle)
            })
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            continue;
        }
        // A search opens the categories it matched: folding would hide the
        // result the reader just asked for.
        let folded = collapsed.contains(category.id()) && needle.is_empty();
        rows.push(ShortcutRow {
            header: Some(category),
            binding: None,
        });
        if !folded {
            bindings.sort_by_key(|binding| binding.chord.display());
            rows.extend(bindings.into_iter().map(|binding| ShortcutRow {
                header: None,
                binding: Some(binding),
            }));
        }
    }
    rows
}

/// The shortcuts cheatsheet inside the help modal.
#[allow(clippy::too_many_arguments)]
fn render_shortcut_cheatsheet(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    query: &str,
    selected: usize,
    collapsed: &std::collections::BTreeSet<String>,
) {
    let rows = shortcut_rows(app, query, collapsed);
    if rows.is_empty() {
        empty_state(frame, area, theme, strings.help_no_keys());
        return;
    }
    let selected = selected.min(rows.len().saturating_sub(1));
    let height = usize::from(area.height);
    let offset = selected.saturating_sub(height.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(offset).take(height) {
        let active = index == selected;
        match row.header {
            Some(category) => {
                let folded = collapsed.contains(category.id()) && query.trim().is_empty();
                let marker = crate::glyphs::disclosure(!folded, app.glyph_tier());
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{marker} "),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(
                        category_label(category, strings).to_string(),
                        Style::default()
                            .fg(theme.roles.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            None => {
                let Some(binding) = row.binding else { continue };
                let label = app.binding_label(binding);
                let style = if active {
                    theme.selected()
                } else {
                    theme.base()
                };
                lines.push(Line::from(vec![
                    Span::styled(if active { "  ▸ " } else { "    " }, style),
                    Span::styled(format!("{:<14}", binding.chord.display()), style),
                    Span::styled(label.to_string(), style),
                    Span::styled(
                        format!("   {}", binding.scope.id()),
                        Style::default().fg(theme.roles.gray_dim),
                    ),
                ]));
                // The selected binding explains itself, so the overlay is a
                // teacher rather than a list of chords.
                if active {
                    lines.push(Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(
                            binding.intent.help(strings).to_string(),
                            Style::default()
                                .fg(theme.roles.gray)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The key-binding editor: a grouped list whose selected row can be rebound in
/// place.
///
/// The editor writes the same file the loader reads, and refuses a chord that
/// another action already owns rather than silently shadowing it — a duplicate
/// binding is invisible at runtime, so it must be impossible to create one by
/// accident here.
/// What the editor is showing, bundled so the renderer stays a function of one
/// state value rather than of five positional flags.
struct KeysEditorView<'a> {
    query: &'a str,
    selected: usize,
    capturing: Option<Intent>,
    message: Option<&'a str>,
    dirty: bool,
}

fn render_keys_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    view: KeysEditorView<'_>,
) {
    let KeysEditorView {
        query,
        selected,
        capturing,
        message,
        dirty,
    } = view;
    let rows = key_editor_rows(app, query);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.help_no_keys()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    let selected = selected.min(rows.len().saturating_sub(1));
    let height = usize::from(area.height).saturating_sub(1);
    let offset = selected.saturating_sub(height.saturating_sub(1));
    for (index, row) in rows.iter().enumerate().skip(offset).take(height) {
        let active = index == selected;
        match row.header {
            Some(scope) => lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    scope.id().to_uppercase(),
                    Style::default()
                        .fg(theme.roles.gray_dim)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            None => {
                let Some(binding) = row.binding else { continue };
                let label = app.binding_label(binding);
                let style = if active {
                    theme.selected()
                } else {
                    theme.base()
                };
                // While capturing, the chord column is the prompt: it is where
                // the new keys will appear, so the reader's eye is already
                // there.
                let chord = if capturing == Some(binding.intent) {
                    format!("{:<14}", strings.keys_press())
                } else {
                    format!("{:<14}", binding.chord.display())
                };
                let mut spans = vec![
                    Span::styled(if active { "  ▸ " } else { "    " }, style),
                    Span::styled(chord, style),
                    Span::styled(label.to_string(), style),
                ];
                if app.keymap.is_overridden(binding.intent) {
                    spans.push(Span::styled(
                        format!("  {}", crate::glyphs::diamond_dotted(app.glyph_tier())),
                        Style::default().fg(theme.roles.accent_attention),
                    ));
                }
                spans.push(Span::styled(
                    format!("   {}", binding.intent.id()),
                    Style::default().fg(theme.roles.gray_dim),
                ));
                lines.push(Line::from(spans));
            }
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
    // The last row of the content area reports the filter, the last attempt or
    // the unsaved state, in that order of urgency.
    let footer = Rect {
        y: area.bottom().saturating_sub(1),
        height: 1,
        ..area
    };
    let (text, tone) = if let Some(message) = message {
        (message.to_string(), theme.roles.danger)
    } else if capturing.is_some() {
        (
            strings.keys_capture_hint().to_string(),
            theme.roles.accent_attention,
        )
    } else if !query.trim().is_empty() {
        (format!("/{}", query.trim()), theme.roles.foreground)
    } else if dirty {
        (
            strings.keys_unsaved().to_string(),
            theme.roles.accent_attention,
        )
    } else {
        (strings.keys_saved_hint().to_string(), theme.roles.gray_dim)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&text, usize::from(footer.width), "…"),
            Style::default().fg(tone),
        ))),
        footer,
    );
}

/// Human label for a cheatsheet category.
fn category_label(category: crate::keymap::Category, strings: Strings) -> &'static str {
    match category {
        crate::keymap::Category::Global => strings.help_category_global(),
        crate::keymap::Category::Transcript => strings.help_category_transcript(),
        crate::keymap::Category::Composer => strings.help_category_composer(),
        crate::keymap::Category::Modals => strings.help_category_modals(),
        crate::keymap::Category::Workbench => strings.help_category_workbench(),
        crate::keymap::Category::Management => strings.help_category_management(),
        crate::keymap::Category::Panels => strings.help_category_panels(),
    }
}

/// The key hint bar.
///
/// Keys are drawn bold and bright, labels dim: the key is what the reader is
/// looking for and the label only confirms it. Hints that must survive a narrow
/// terminal are pinned and drawn first, so shrinking the window degrades the bar
/// from the least important end rather than truncating it arbitrarily.
fn render_key_bar(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    let strings = app.strings;
    let mut labels: Vec<(Scope, Intent, &'static str)> = Vec::new();
    if app.page.is_session_page() && app.overlay.is_none() {
        let composing = app.focus == crate::app::Focus::Composer || app.page_is_composing();
        if composing {
            labels.push((
                app.page.scope(),
                Intent::SubmitComposer,
                if app.turn_reads_running() {
                    strings.composer_queue()
                } else {
                    strings.composer_send()
                },
            ));
            labels.push((
                app.page.scope(),
                Intent::InsertNewline,
                strings.composer_newline(),
            ));
        } else {
            labels.push((
                app.page.scope(),
                Intent::ToggleBlockExpanded,
                strings.transcript_expand(),
            ));
            labels.push((
                app.page.scope(),
                Intent::OpenBlockDetails,
                strings.transcript_details(),
            ));
            labels.push((
                app.page.scope(),
                Intent::FocusComposer,
                strings.composer_send(),
            ));
        }
        if app.turn_reads_running() {
            labels.push((
                Scope::Global,
                Intent::ContextualCancel,
                if app.composer.is_empty() {
                    strings.session_stop()
                } else {
                    strings.hint_clear()
                },
            ));
        }
        labels.push((
            app.page.scope(),
            Intent::SwitchAgentRuntime,
            strings.runtime_title(),
        ));
        labels.push((
            Scope::Global,
            Intent::FocusNext,
            strings.transcript_browse(),
        ));
    }
    // The table fills in the rest, and the whole list is read back through the
    // keys that answer here: a hint whose intent has no chord on this surface
    // — `Tab` means "previous pane" on the transcript and "accept completion"
    // in the composer, and `?` is help on the list and a question mark in a
    // draft — is dropped rather than spelled wrong.
    // How many of the bar's rows the page's own scope may take. A page with
    // twenty verbs would otherwise fill the bar and push the global chrome off
    // it — including the palette, which is how a reader finds the verbs that
    // did not fit.
    // How much of the bar the global chrome keeps. The page's own keys lead,
    // but a page with twenty verbs would take the whole row and leave the
    // reader without the palette — which is how they reach the verbs that did
    // not fit.
    const GLOBAL_SHARE: u16 = 3;
    let mut reserved = 0u16;
    for (scope, binding) in app.advertised_keys() {
        if scope != Scope::Global {
            continue;
        }
        if binding.label.is_some() {
            let label = app.binding_label(binding);
            reserved +=
                (display_width(&binding.chord.display()) + 1 + display_width(label) + 5) as u16;
        }
        if reserved >= area.width / GLOBAL_SHARE {
            reserved = reserved.min(area.width / GLOBAL_SHARE);
            break;
        }
    }
    for (scope, binding) in app.advertised_keys() {
        if binding.label.is_some()
            && !labels
                .iter()
                .any(|(_, hinted, _)| *hinted == binding.intent)
        {
            labels.push((scope, binding.intent, app.binding_label(binding)));
        }
    }
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1usize;
    for (scope, intent, label) in labels {
        let Some(chord) = app.advertised_chord(intent) else {
            continue;
        };
        let key = chord.display();
        let separator = if used == 1 { "" } else { "  ·  " };
        let width = display_width(&key) + 1 + display_width(label);
        // The page's own rows stop at the line the global chrome keeps; the
        // global rows may use the whole bar.
        let ceiling = if scope == Scope::Global {
            area.width
        } else {
            area.width.saturating_sub(reserved)
        };
        if used + display_width(separator) + width > usize::from(ceiling) {
            continue;
        }
        spans.push(Span::styled(separator, theme.dimmed(theme.roles.gray_dim)));
        used += display_width(separator);
        app.regions.hints.push((
            Rect {
                x: area.x + used as u16,
                y: area.y,
                width: width as u16,
                height: 1,
            },
            intent,
        ));
        spans.push(Span::styled(key, theme.muted()));
        spans.push(Span::styled(
            format!(" {label}"),
            theme.dimmed(theme.roles.gray_dim),
        ));
        used += width;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Paint a left/centre/right line, dropping the centre before the sides.
fn render_zoned_line(
    frame: &mut Frame<'_>,
    area: Rect,
    left: Vec<Span<'static>>,
    centre: Option<Vec<Span<'static>>>,
    right: Vec<Span<'static>>,
) {
    if area.height == 0 {
        return;
    }
    let left_line = Line::from(left);
    let left_width = left_line.width() as u16;
    let right_line = Line::from(right);
    let right_width = right_line.width() as u16;
    frame.render_widget(Paragraph::new(left_line), area);
    if right_width + 1 < area.width {
        let right_area = Rect {
            x: area.x + area.width - right_width,
            width: right_width,
            ..area
        };
        frame.render_widget(Paragraph::new(right_line), right_area);
    }
    let Some(centre) = centre else { return };
    let centre_line = Line::from(centre);
    let centre_width = centre_line.width() as u16;
    let gutter = 2u16;
    if left_width + centre_width + right_width + gutter * 2 > area.width {
        // Not enough room for all three; the sides are load-bearing, so the
        // centre yields rather than colliding.
        return;
    }
    let centre_area = Rect {
        x: area.x + (area.width.saturating_sub(centre_width)) / 2,
        width: centre_width,
        ..area
    };
    frame.render_widget(Paragraph::new(centre_line), centre_area);
}

/// Draw one modal and publish what the frame has to know about it.
///
/// Two things are published. The close affordance belongs to the frame that
/// draws it: the rect is republished every frame the modal is up, and cleared
/// with the frame once it is gone. Nothing remembers it across frames. The
/// box itself is published for the picture layer, which cannot be covered by
/// the backdrop and so has to be told what the box took — see
/// [`crate::row_marks::RowMarks::place_asked`].
fn render_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    chrome: &ModalChrome<'_>,
    theme: &TuiTheme,
    app: &mut App,
) -> Option<ModalLayout> {
    let layout = modal::render_modal(frame, area, chrome, theme);
    // A box too small to draw fills the whole area with its own notice, so that
    // is the box the frame took; a box that was drawn is the popup's own rect.
    app.regions.modal = Some(layout.as_ref().map_or(area, |layout| layout.area));
    app.regions.modal_close = layout.as_ref().and_then(|layout| layout.close);
    // A footer hint is a key printed on a control the pointer can see, so it is
    // pressed the way its key is. The chrome reports the cells because it is
    // the only code that knows where a hint wrapped to.
    if let Some(layout) = layout.as_ref() {
        for (rect, key) in &layout.hints {
            if let Some(chord) = Chord::from_hint(key) {
                app.regions
                    .key_hints
                    .push(crate::app::KeyHintRegion { rect: *rect, chord });
            }
        }
    }
    layout
}

/// The form overlay: one record's fields, edited in place.
///
/// Every field is on one screen rather than behind a step per field, because a
/// reader changing an MCP server's transport usually changes its URL in the
/// same breath, and a wizard makes them walk the whole form twice.
fn render_config_form(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    form: &crate::config_center::ConfigForm,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let saving = app
        .config_center
        .pending
        .contains_key(crate::config_center::FORM_RECORD);
    let chrome = modal_chrome(
        app,
        &form.title,
        ModalSizing::new(70, 96, 48, 2, 2, 1, 2),
        vec![
            ModalHint::new("Tab", strings.config_form_hint_move()),
            ModalHint::new("←→", strings.config_form_hint_change()),
            ModalHint::new("Enter", strings.config_form_hint_edit()),
            ModalHint::new("^S", strings.config_form_hint_save()),
            ModalHint::new("Esc", strings.config_form_hint_cancel()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let content = layout.content;
    let footer = layout.footer;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let label_width = 18usize;
    // What the form is about to write, read back before it is written. It is
    // the one line the fields do not carry — a registry entry is a launcher
    // before it is a record — so it is shown rather than asked for.
    if let Some(preview) = &form.preview {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                truncate_to_width(preview, usize::from(content.width).saturating_sub(4), "…"),
                Style::default().fg(theme.roles.text_secondary),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  c {}", strings.config_form_copy()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    for (index, field) in form.fields.iter().enumerate() {
        let selected = index == form.selected;
        let editing = form.editing.as_deref() == Some(field.key.as_str());
        let label_style = if selected {
            Style::default()
                .fg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        let marker = if selected { "▸" } else { " " };
        let changed = if field.changed() { "•" } else { " " };
        let value = field.displayed();
        let value_style = if field.locked {
            // A pinned value is not the reader's to change, so it is drawn as
            // something read rather than something written.
            Style::default().fg(theme.roles.gray_bright)
        } else if editing {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
        } else if field.changed() {
            Style::default().fg(theme.roles.warning)
        } else {
            Style::default().fg(theme.roles.foreground)
        };
        let chevron = match field.kind {
            _ if field.locked => "locked",
            crate::config_center::FormFieldKind::Choice { .. } => "‹ ›",
            crate::config_center::FormFieldKind::Toggle => "on/off",
            _ => "",
        };
        let multi = matches!(field.kind, crate::config_center::FormFieldKind::Multiline);
        let shown = if field.secret {
            "•".repeat(value.chars().count())
        } else if multi {
            value.lines().next().unwrap_or_default().to_string()
        } else {
            value.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{changed} "),
                Style::default().fg(theme.roles.gray_dim),
            ),
            Span::styled(
                format!(
                    "{:<label_width$}",
                    truncate_to_width(&field.label, label_width, "…")
                ),
                label_style,
            ),
            Span::styled(
                truncate_to_width(
                    &shown,
                    usize::from(content.width).saturating_sub(label_width + 10),
                    "…",
                ),
                value_style,
            ),
            Span::styled(
                if editing {
                    format!("▏{chevron}")
                } else {
                    chevron.to_string()
                },
                Style::default().fg(theme.roles.accent_user),
            ),
        ]));
        // A multi-line value is worth its own box: the reader is writing
        // instructions an Agent will follow, and one truncated line is not
        // enough to see what they wrote.
        if multi && (selected || editing) {
            let body_width = usize::from(content.width).saturating_sub(label_width + 4);
            for line in field.value.lines().skip(1).take(4) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{:<label_width$}  {}",
                        "",
                        truncate_to_width(line, body_width, "…")
                    ),
                    Style::default().fg(theme.roles.text_secondary),
                )));
            }
        }
        if let Some(hint) = &field.hint
            && selected
        {
            lines.push(Line::from(Span::styled(
                format!("  {:<label_width$}{}", "", hint),
                Style::default().fg(theme.roles.gray_dim),
            )));
        }
    }
    if let Some(error) = &form.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {error}"),
            Style::default()
                .fg(theme.roles.danger)
                .add_modifier(Modifier::BOLD),
        )));
    }
    let scroll = usize::from(form.scroll);
    let visible = lines
        .into_iter()
        .skip(scroll)
        .take(usize::from(content.height))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), content);
    if let Some(footer) = footer {
        // The footer carries the one state the fields cannot show: a write on
        // its way, or an unsaved change the reader would lose by leaving.
        let hint = if saving {
            strings.config_working()
        } else if form.dirty() {
            strings.config_form_unsaved()
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {hint}"),
                Style::default().fg(theme.roles.gray_dim),
            ))),
            footer,
        );
    }
}

/// The send-time editor: the Desktop's clock popover, on the character grid.
///
/// Two shapes in one panel, the way the Desktop's segmented control has them. A
/// terminal has no calendar grid, so the absolute shape is one text field in
/// the layout it is read back in — said beside the field, because a deadline
/// typed in the wrong layout is refused and the reader has to be able to see
/// why before they are told. The countdown shape is three small numbers, and
/// the note under both is the Desktop's own sentence about what a schedule
/// promises: it sends by itself, it waits for a running turn, and it needs the
/// client to still be open.
fn render_message_schedule(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    form: &crate::schedule::MessageScheduleForm,
    theme: &TuiTheme,
) {
    use crate::schedule::{ScheduleField, ScheduleMode};
    let strings = app.strings;
    // The keys the panel names are the keys it answers: a draft with no send
    // time has nothing to void, so the clear is not offered until there is
    // something to clear.
    let mut hints = vec![
        ModalHint::new("Tab", strings.schedule_hint_move()),
        ModalHint::new("←→", strings.schedule_hint_switch()),
        ModalHint::new("Enter", strings.schedule_hint_apply()),
    ];
    if form.clearable {
        hints.push(ModalHint::new("^X", strings.schedule_clear()));
    }
    hints.push(ModalHint::new("Esc", strings.close()));
    let chrome = modal_chrome(
        app,
        strings.schedule_title(),
        ModalSizing::new(52, 72, 44, 4, 2, 1, 0),
        hints,
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let content = layout.content;
    let label_width = 12usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    // The shape row: a closed set with two values, drawn the way every other
    // closed set in this client is drawn.
    let mode_selected = form.field == ScheduleField::Mode;
    lines.push(Line::from(vec![
        Span::styled(
            if mode_selected { "▸ " } else { "  " },
            Style::default().fg(theme.roles.accent_user),
        ),
        Span::styled(
            format!(
                "{:<label_width$}",
                truncate_to_width(strings.schedule_field_mode(), label_width, "…")
            ),
            if mode_selected {
                Style::default()
                    .fg(theme.roles.accent_user)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.roles.gray)
            },
        ),
        Span::styled(
            format!("‹ {} ›", form.mode.label(strings)),
            Style::default().fg(if mode_selected {
                theme.roles.foreground
            } else {
                theme.roles.text_secondary
            }),
        ),
    ]));
    // Then the fields the shape actually has. A field the shape does not use is
    // not drawn greyed: it does not exist until the shape is changed, and a row
    // that could not be acted on would only be something to skip past.
    let fields = match form.mode {
        ScheduleMode::At => vec![(ScheduleField::When, strings.schedule_field_when())],
        ScheduleMode::After => vec![
            (ScheduleField::Hours, strings.schedule_field_hours()),
            (ScheduleField::Minutes, strings.schedule_field_minutes()),
            (ScheduleField::Seconds, strings.schedule_field_seconds()),
        ],
    };
    for (field, label) in fields {
        let selected = form.field == field;
        let value = form.value(field).cloned().unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                format!(
                    "{:<label_width$}",
                    truncate_to_width(label, label_width, "…")
                ),
                if selected {
                    Style::default()
                        .fg(theme.roles.accent_user)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.roles.gray)
                },
            ),
            Span::styled(
                truncate_to_width(
                    &value,
                    usize::from(content.width).saturating_sub(label_width + 8),
                    "…",
                ),
                if selected {
                    Style::default()
                        .fg(theme.roles.foreground)
                        .bg(theme.roles.surface_highlight)
                } else {
                    Style::default().fg(theme.roles.foreground)
                },
            ),
            Span::styled(
                if selected { "▏" } else { "" },
                Style::default().fg(theme.roles.accent_user),
            ),
        ]));
    }
    lines.push(Line::from(""));
    let hint = match form.mode {
        ScheduleMode::At => strings.schedule_when_hint(),
        ScheduleMode::After => strings.schedule_countdown_hint(),
    };
    for row in wrap_hint(hint, usize::from(content.width).saturating_sub(2)) {
        lines.push(Line::from(Span::styled(
            format!("  {row}"),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    if let Some(error) = form.error {
        lines.push(Line::from(Span::styled(
            format!("  {}", error.message(strings)),
            Style::default()
                .fg(theme.roles.danger)
                .add_modifier(Modifier::BOLD),
        )));
    }
    for row in wrap_hint(
        strings.schedule_note(),
        usize::from(content.width).saturating_sub(2),
    ) {
        lines.push(Line::from(Span::styled(
            format!("  {row}"),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    let visible = lines
        .into_iter()
        .take(usize::from(content.height))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), content);
}

/// Cut one sentence into the rows a modal's width can hold, on word
/// boundaries. The modal is a fixed size, so a line that overflowed would be
/// silently clipped rather than wrapped by the paragraph widget.
fn wrap_hint(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        if row.is_empty() {
            row.push_str(word);
        } else if display_width(&row) + 1 + display_width(word) <= width {
            row.push(' ');
            row.push_str(word);
        } else {
            rows.push(std::mem::take(&mut row));
            row.push_str(word);
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// The action menu for one record.
fn render_entry_actions(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    entry: &crate::config_center::EntryActions,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let chrome = modal_chrome(
        app,
        &entry.title,
        ModalSizing::new(40, 56, 36, 4, 2, 1, 0),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.hint_run()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, action) in entry.actions.iter().enumerate() {
        let selected = index == entry.selected;
        let label = action.label(strings);
        let style = if selected {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if action.is_destructive() {
            Style::default().fg(theme.roles.danger)
        } else {
            Style::default().fg(theme.roles.text_secondary)
        };
        let marker = if selected { "▸" } else { " " };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker} {:<width$}",
                label,
                width = usize::from(layout.content.width).saturating_sub(2)
            ),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
}

/// One registry Skill, read before it is installed.
///
/// The preview draws what the install would write: the bundle, the files the
/// registry refused to send, and the instructions themselves. A reader who
/// cannot see the bundle is not in a position to agree to it.
fn render_skill_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &crate::config_center::SkillPreviewView,
    theme: &TuiTheme,
) {
    use crate::config_center as cc;
    let strings = app.strings;
    let title = format!("{} · {}", strings.config_preview_title(), view.title);
    let chrome = modal_chrome(
        app,
        &title,
        ModalSizing::new(72, 96, 44, 2, 2, 1, 1),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.config_action_install()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let document = app.management_data.skill_preview.as_deref();
    let lines = cc::skill_preview_lines(document, strings);
    let mut drawn: Vec<Line<'static>> = Vec::new();
    drawn.push(Line::from(Span::styled(
        format!("  {}", strings.config_preview_hint()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    if let Some(banner) = &lines.banner {
        drawn.push(Line::from(Span::styled(
            format!("  {banner} "),
            chip_style(cc::ConfigTone::Bad, theme).add_modifier(Modifier::BOLD),
        )));
    }
    if lines.is_empty() && document.is_none() {
        drawn.push(Line::from(Span::styled(
            format!(
                "  {}",
                if view.loading {
                    strings.config_preview_loading()
                } else {
                    strings.config_preview_empty()
                }
            ),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    if let Some(bundle) = &lines.bundle {
        drawn.push(Line::from(Span::styled(
            format!("  {bundle}"),
            Style::default().fg(theme.roles.text_secondary),
        )));
    }
    if !lines.files.is_empty() || !lines.skipped.is_empty() {
        drawn.push(Line::from(Span::styled(
            format!("  {}", strings.config_preview_files()),
            Style::default()
                .fg(theme.roles.text_secondary)
                .add_modifier(Modifier::BOLD),
        )));
    }
    for file in &lines.files {
        drawn.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                format!(" {:<10}", strings.config_state_available()),
                chip_style(cc::ConfigTone::Ok, theme),
            ),
            Span::styled(file.clone(), Style::default().fg(theme.roles.foreground)),
        ]));
    }
    for skipped in &lines.skipped {
        drawn.push(Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                format!(" {:<10}", strings.config_preview_skipped()),
                chip_style(cc::ConfigTone::Warn, theme),
            ),
            Span::styled(
                skipped.clone(),
                Style::default().fg(theme.roles.gray_bright),
            ),
        ]));
    }
    if !lines.body.is_empty() {
        drawn.push(Line::from(Span::styled(
            format!("  {}", strings.config_preview_body()),
            Style::default()
                .fg(theme.roles.text_secondary)
                .add_modifier(Modifier::BOLD),
        )));
        for line in &lines.body {
            drawn.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(theme.roles.foreground),
            )));
        }
    }
    // The window is the body of the modal; the reader scrolls the whole thing
    // rather than the instructions alone, because the file list is what they
    // are agreeing to first.
    let window = usize::from(layout.content.height).saturating_sub(1);
    let first = view.scroll.min(drawn.len().saturating_sub(1));
    let visible = drawn
        .into_iter()
        .skip(first)
        .take(window.max(1))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), layout.content);
}

/// One Agent's account: the ways in, and what can be done with what it holds.
///
/// The status is the first line because it is the question the reader came
/// with; everything under it is a line `Enter` can run.
fn render_agent_auth(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &crate::config_center::AgentAuthView,
    theme: &TuiTheme,
) {
    use crate::config_center as cc;
    let strings = app.strings;
    let title = format!("{} · {}", strings.config_auth_title(), view.title);
    let chrome = modal_chrome(
        app,
        &title,
        ModalSizing::new(64, 96, 40, 2, 2, 1, 1),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.hint_run()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let catalog = app.management_data.agent_auth_catalog.as_ref();
    let (status, tone) = cc::agent_auth_status(catalog, strings);
    // A sign-in terminal takes the panel over. The account lines are still the
    // truth about the account, but while a login flow owns the PTY the reader's
    // eyes — and keys — belong to it, and drawing both would put a list of
    // verbs beside a prompt that answers them.
    if app.auth_terminal.is_some() {
        render_agent_auth_terminal(frame, layout.content, app, theme);
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            format!(" {status} "),
            chip_style(tone, theme).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.config_auth_hint()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    let rows = view.rows(catalog, strings);
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                if view.loading {
                    strings.config_auth_loading()
                } else {
                    strings.config_auth_no_methods()
                }
            ),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    let window = usize::from(layout.content.height).saturating_sub(3);
    let selected = view.selected.min(rows.len().saturating_sub(1));
    let first = selected.saturating_sub(window.saturating_sub(1) / 2);
    for (offset, row) in rows.iter().skip(first).take(window.max(1)).enumerate() {
        let index = first + offset;
        let is_selected = index == selected;
        let usable = row.action.is_some();
        let base = if is_selected {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if usable {
            Style::default().fg(theme.roles.foreground)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        let mut spans = vec![
            Span::styled(
                if is_selected { "▌" } else { " " },
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                format!(" {:<22}", row.chip),
                chip_style(row.tone, theme).patch(base),
            ),
            Span::styled(config_cell(&row.label, 26), base),
        ];
        if let Some((label, value)) = row.detail.first() {
            spans.push(Span::styled(
                if label.is_empty() {
                    value.clone()
                } else {
                    format!("{label} · {value}")
                },
                Style::default().fg(theme.roles.gray_bright),
            ));
        }
        if let Some(hint) = &row.hint {
            spans.push(Span::styled(
                format!("  {hint}"),
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
        lines.push(Line::from(spans));
    }
    // The sign-out question is drawn where the reader's eyes already are, with
    // the count the authority would hold them to.
    if view.confirming_sign_out {
        let count = app
            .management_data
            .agent_auth_logout
            .as_ref()
            .map(|preview| preview.affected_session_ids.len())
            .unwrap_or(0);
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                strings
                    .config_auth_sign_out_impact()
                    .replace("{count}", &count.to_string())
            ),
            chip_style(crate::config_center::ConfigTone::Warn, theme),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.config_auth_sign_out_confirm()),
            Style::default().fg(theme.roles.gray_bright),
        )));
    }
    // A sign-in the authority is still running says what the client can do
    // about it: the cancel line above is the answer, and this is the sentence
    // that makes it read as an answer rather than a stray verb.
    if view.pending_sign_in.is_some() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.config_auth_waiting_hint()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    if let Some((notice, tone)) = &view.notice {
        lines.push(Line::from(Span::styled(
            format!("  {notice}"),
            chip_style(*tone, theme),
        )));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
}

/// The live grid of a sign-in terminal.
///
/// The terminal's own colours are used where the emulator reports them, because
/// the program inside chose them: a login flow that draws its selected provider
/// in reverse video is telling the reader something, and repainting it in the
/// client's palette would erase that. An unknown or default colour falls back to
/// the panel's foreground on its own background, which is what makes an
/// unstyled program readable in either appearance.
fn render_agent_auth_terminal(
    frame: &mut Frame<'_>,
    content: Rect,
    app: &mut App,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let Some(terminal) = app.auth_terminal.as_ref() else {
        return;
    };
    let Some(snapshot) = terminal.snapshot() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", strings.config_auth_terminal_reading()),
                Style::default().fg(theme.roles.gray_dim),
            ))),
            content,
        );
        return;
    };
    let title = terminal.title.clone();
    // The header names the terminal and says what the keys do; the grid starts
    // under it, and the footer is the same hint the panel would have drawn.
    let header = Rect {
        height: content.height.min(1),
        ..content
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("  {} ", strings.config_auth_method_terminal()),
                chip_style(crate::config_center::ConfigTone::Ok, theme),
            ),
            Span::styled(
                title,
                Style::default()
                    .fg(theme.roles.gray_bright)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        header,
    );
    let footer_rows = u16::from(content.height > 3);
    let grid = Rect {
        y: content.y + 1,
        height: content.height.saturating_sub(1 + footer_rows),
        ..content
    };
    let visible_rows = usize::from(grid.height).min(usize::from(snapshot.rows));
    let visible_columns = usize::from(grid.width).min(usize::from(snapshot.columns));
    let mut rows: Vec<Line<'static>> = Vec::with_capacity(visible_rows);
    for row in 0..visible_rows as u16 {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(visible_columns);
        for column in 0..visible_columns as u16 {
            // A cell's own glyph is what the program drew. A wide character
            // occupies two cells and its second is a spacer the emulator marks,
            // so the spacer is skipped rather than drawn as a second glyph.
            let Some(cell) = snapshot
                .cells
                .iter()
                .find(|cell| cell.row == row && cell.column == column)
            else {
                spans.push(Span::raw(" "));
                continue;
            };
            if cell.wide_spacer {
                continue;
            }
            let text = if cell.text.is_empty() {
                " ".to_string()
            } else {
                cell.text.clone()
            };
            spans.push(Span::styled(text, terminal_cell_style(cell, theme)));
        }
        rows.push(Line::from(spans));
    }
    // Blank rows the program has not written to still belong to the grid: the
    // panel is a fixed box, and leaving the page behind it visible would make
    // the terminal look like it had holes.
    while rows.len() < usize::from(grid.height) {
        rows.push(Line::from(""));
    }
    app.regions.agent_auth_terminal = Some(grid);
    frame.render_widget(Paragraph::new(rows), grid);
    if footer_rows == 1 {
        let footer = Rect {
            y: grid.bottom(),
            height: 1,
            ..content
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", strings.config_auth_terminal_hint()),
                Style::default().fg(theme.roles.gray_dim),
            ))),
            footer,
        );
    }
}

/// One terminal cell, in the colour the program inside asked for.
fn terminal_cell_style(cell: &vibex_terminal_ui::TerminalCellSnapshot, theme: &TuiTheme) -> Style {
    use vibex_terminal_ui::TerminalCellColor;
    let foreground = match cell.foreground {
        TerminalCellColor::Named { index } => named_terminal_color(index),
        TerminalCellColor::Indexed { index } => indexed_terminal_color(index),
        TerminalCellColor::Rgb { red, green, blue } => Some(Color::Rgb(red, green, blue)),
    };
    let foreground = foreground.unwrap_or(theme.roles.foreground);
    let background = match cell.background {
        TerminalCellColor::Named { index } => named_terminal_color(index),
        TerminalCellColor::Indexed { index } => indexed_terminal_color(index),
        TerminalCellColor::Rgb { red, green, blue } => Some(Color::Rgb(red, green, blue)),
    }
    .unwrap_or(theme.roles.surface_raised);
    // Inverse is the one attribute a program cannot be drawn without: a
    // selected row and a plain one would otherwise look the same.
    let (foreground, background) = if cell.inverse {
        (background, foreground)
    } else {
        (foreground, background)
    };
    let mut style = Style::default().fg(foreground).bg(background);
    if cell.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.strikeout {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if cell.hidden {
        // A hidden cell is invisible but still occupies its position, which is
        // how a password field draws its dots.
        style = style.add_modifier(Modifier::HIDDEN);
    }
    style
}

/// The sixteen ANSI slots, in the order a terminal numbers them.
///
/// `None` for a slot outside the sixteen: a program that indexed `default` is
/// asking for the terminal's own colour rather than a palette entry, and the
/// caller decides what that means for the surface it is on.
fn named_terminal_color(index: u16) -> Option<Color> {
    let palette: [Color; 16] = [
        Color::Rgb(0, 0, 0),
        Color::Rgb(205, 49, 49),
        Color::Rgb(13, 188, 121),
        Color::Rgb(229, 229, 16),
        Color::Rgb(36, 114, 200),
        Color::Rgb(188, 63, 188),
        Color::Rgb(17, 168, 205),
        Color::Rgb(229, 229, 229),
        Color::Rgb(102, 102, 102),
        Color::Rgb(241, 76, 76),
        Color::Rgb(35, 209, 139),
        Color::Rgb(245, 245, 67),
        Color::Rgb(59, 142, 234),
        Color::Rgb(214, 112, 214),
        Color::Rgb(41, 184, 219),
        Color::Rgb(255, 255, 255),
    ];
    palette.get(usize::from(index)).copied()
}

/// A 256-colour index, resolved the way every terminal resolves it: the first
/// sixteen are the named slots, the next two hundred and sixteen are a 6×6×6
/// cube, and the last twenty-four are a grey ramp.
fn indexed_terminal_color(index: u8) -> Option<Color> {
    match index {
        0..=15 => named_terminal_color(u16::from(index)),
        16..=231 => {
            let cube = index - 16;
            let step = |value: u8| -> u8 { if value == 0 { 0 } else { 55 + value * 40 } };
            Some(Color::Rgb(
                step(cube / 36),
                step((cube % 36) / 6),
                step(cube % 6),
            ))
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            Some(Color::Rgb(level, level, level))
        }
    }
}

/// One profile, offered to another tool's own settings.
///
/// The picker shows the question — which target, which contents — and then the
/// plan that question produced: the files it would write and the redacted
/// difference in each. Apply names the plan on screen rather than re-deriving
/// one, so what is written is what was read.
fn render_native_export(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &crate::config_center::NativeExportView,
    theme: &TuiTheme,
) {
    use crate::config_center as cc;
    let strings = app.strings;
    let title = format!("{} · {}", strings.config_export_title(), view.title);
    let chrome = modal_chrome(
        app,
        &title,
        ModalSizing::new(70, 96, 44, 2, 2, 1, 1),
        vec![
            ModalHint::new("t", strings.config_export_source_agent()),
            ModalHint::new("m", strings.config_field_kind()),
            ModalHint::new("v", strings.config_field_preview()),
            ModalHint::new("w", strings.config_action_save()),
            ModalHint::new("b", strings.config_action_rollback()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.config_export_hint()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("{}: ", cc::native_export_source_label(view.source, strings)),
            Style::default()
                .fg(theme.roles.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            cc::native_export_mode_label(view.mode, strings).to_string(),
            Style::default().fg(theme.roles.text_secondary),
        ),
    ]));
    let files = cc::native_export_file_rows(view.preview.as_deref(), strings);
    if files.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.config_export_no_preview()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    for file in &files {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!(" {:<12}", file.chip), chip_style(file.tone, theme)),
            Span::styled(
                truncate_to_width(&file.label, 44, "…"),
                Style::default().fg(theme.roles.foreground),
            ),
        ]));
        for line in file.diff.lines().take(6) {
            lines.push(Line::from(Span::styled(
                format!("      {line}"),
                Style::default().fg(theme.roles.gray_bright),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.config_export_history()),
        Style::default()
            .fg(theme.roles.text_secondary)
            .add_modifier(Modifier::BOLD),
    )));
    let history = cc::native_export_record_rows(&view.history, strings);
    if history.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.config_export_no_history()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    for record in &history {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{:<14}", record.status),
                Style::default().fg(if record.rollbackable {
                    theme.roles.warning
                } else {
                    theme.roles.gray_dim
                }),
            ),
            Span::styled(
                format!("{} · {}", record.source, record.mode),
                Style::default().fg(theme.roles.gray_bright),
            ),
        ]));
    }
    if let Some((notice, tone)) = &view.notice {
        lines.push(Line::from(Span::styled(
            format!("  {notice}"),
            chip_style(*tone, theme),
        )));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
}

/// What other CLI tools hold, offered to import.
///
/// The picker is the area's rather than a record's: a reader looks for settings
/// to import before there is a profile to hang them on.
fn render_native_import(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &crate::config_center::NativeImportView,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let chrome = modal_chrome(
        app,
        strings.config_import_title(),
        ModalSizing::new(64, 96, 40, 2, 2, 1, 1),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.config_action_import()),
            ModalHint::new("r", strings.config_action_refresh()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let rows = crate::config_center::native_import_rows(
        app.management_data.native_import.as_ref(),
        strings,
    );
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.config_import_hint()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                if view.loading {
                    strings.config_import_loading()
                } else {
                    strings.config_import_empty()
                }
            ),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    let window = usize::from(layout.content.height).saturating_sub(1);
    let selected = view.selected.min(rows.len().saturating_sub(1));
    let first = selected.saturating_sub(window.saturating_sub(1) / 2);
    for (offset, row) in rows.iter().skip(first).take(window.max(1)).enumerate() {
        let index = first + offset;
        let is_selected = index == selected;
        let base = if is_selected {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if row.importable {
            Style::default().fg(theme.roles.foreground)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        let mut spans = vec![
            Span::styled(
                if is_selected { "▌" } else { " " },
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                format!(" {:<13}", row.chip),
                chip_style(row.tone, theme).patch(base),
            ),
            Span::styled(config_cell(&row.label, 26), base),
            Span::styled(
                format!("{:<10}", row.source),
                Style::default().fg(theme.roles.gray_bright),
            ),
            Span::styled(
                row.agent.clone(),
                Style::default().fg(theme.roles.gray_bright),
            ),
        ];
        spans.push(Span::raw("  "));
        if let Some((_, value)) = row.detail.first() {
            spans.push(Span::styled(
                value.clone(),
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
}

/// The Models one profile offers.
///
/// The list is the one thing in the centre that is a part of a record rather
/// than a record, so it is drawn over the table rather than in it: what the
/// reader is editing is the profile underneath, and every press writes through
/// that profile.
fn render_models(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &crate::config_center::ModelListView,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let title = format!("{} · {}", strings.config_models_title(), view.title);
    let chrome = modal_chrome(
        app,
        &title,
        ModalSizing::new(60, 96, 40, 2, 2, 1, 1),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("o", strings.config_action_toggle()),
            ModalHint::new("f", strings.config_action_fetch_models()),
            ModalHint::new("i", strings.config_action_new()),
            ModalHint::new("x", strings.config_action_delete()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let rows = match app
        .management_data
        .providers
        .iter()
        .find(|profile| profile.id.as_str() == view.profile_id)
    {
        Some(profile) => {
            crate::config_center::model_rows(profile, &app.management_data.provider_models, strings)
        }
        None => Vec::new(),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.config_models_hint()),
        Style::default().fg(theme.roles.gray_dim),
    )));
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", strings.config_models_empty()),
            Style::default().fg(theme.roles.gray_dim),
        )));
    }
    let window = usize::from(layout.content.height).saturating_sub(1);
    let selected = view.selected.min(rows.len().saturating_sub(1));
    let first = selected.saturating_sub(window.saturating_sub(1) / 2);
    for (offset, row) in rows.iter().skip(first).take(window.max(1)).enumerate() {
        let index = first + offset;
        let is_selected = index == selected;
        let base = if is_selected {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if row.declared {
            Style::default().fg(theme.roles.foreground)
        } else {
            Style::default().fg(theme.roles.gray)
        };
        let (chip, tone) = row.chip(strings);
        let mut spans = vec![
            Span::styled(
                if is_selected { "▌" } else { " " },
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                format!(" {:<14}", chip),
                chip_style(tone, theme).patch(base),
            ),
            Span::styled(config_cell(&row.label, 28), base),
        ];
        for (label, value) in &row.detail {
            spans.push(Span::styled(
                format!("{label} {value}  "),
                Style::default().fg(theme.roles.gray_bright),
            ));
        }
        if !row.declared {
            spans.push(Span::styled(
                strings.config_models_undeclared().to_string(),
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
    if let (Some((notice, tone)), Some(footer)) = (&view.notice, layout.footer) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {notice}"),
                chip_style(*tone, theme),
            ))),
            footer,
        );
    }
}

/// The action menu for one project or folder.
///
/// Built from the heading rather than from a fixed list, so a verb a heading
/// cannot perform is never offered — the same reason the desktop builds its own
/// menu from the row. A destructive verb is drawn in the danger colour, because
/// the menu is the last moment the reader can see what they are choosing.
fn render_sidebar_row_actions(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    menu: &crate::sessions::SidebarRowActions,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let chrome = modal_chrome(
        app,
        &menu.title,
        ModalSizing::new(40, 56, 36, 4, 2, 1, 0),
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.hint_run()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, action) in menu.actions.iter().enumerate() {
        let selected = index == menu.selected;
        // The fold verb is the one whose label depends on the row rather than
        // on the verb: the reader is being offered the state they are not in.
        let label = if action.is_fold() && menu.collapsed {
            strings.sidebar_action_expand()
        } else {
            action.label(strings)
        };
        let style = if selected {
            Style::default()
                .fg(theme.roles.foreground)
                .bg(theme.roles.surface_highlight)
                .add_modifier(Modifier::BOLD)
        } else if action.is_destructive() {
            Style::default().fg(theme.roles.danger)
        } else {
            Style::default().fg(theme.roles.text_secondary)
        };
        let marker = if selected { "▸" } else { " " };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker} {:<width$}",
                label,
                width = usize::from(layout.content.width).saturating_sub(2)
            ),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), layout.content);
}

fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    overlay: &Overlay,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    match overlay {
        Overlay::Form { form } => {
            render_config_form(frame, area, app, form, theme);
        }
        Overlay::Schedule(form) => {
            render_message_schedule(frame, area, app, form, theme);
        }
        Overlay::EntryActions { entry } => {
            render_entry_actions(frame, area, app, entry, theme);
        }
        Overlay::SidebarRowActions(menu) => {
            render_sidebar_row_actions(frame, area, app, menu, theme);
        }
        Overlay::Palette { query, selected } => {
            let matches = app.palette_entries(query);
            let chrome = modal_chrome(
                app,
                strings.palette_title(),
                ModalSizing::palette(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("Enter", strings.hint_run()),
                    ModalHint::new("Esc", strings.close()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(layout.content);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("> ", theme.accent()),
                    Span::styled(query.clone(), theme.base()),
                    Span::styled("▏", theme.accent()),
                ])),
                rows[0],
            );
            // Grouped so the list reads in sections; the selection index is
            // over the flat entry list, so a heading is skipped by the cursor
            // rather than counted as a row. Which entry each drawn line shows
            // is recorded beside the line as it is built, so the frame can
            // publish where every row landed without walking the list a second
            // time — the pointer hit-tests the same lines the reader sees.
            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut row_of_line: Vec<Option<usize>> = Vec::new();
            let mut last_group = None;
            let mut selected_line = 0usize;
            for (index, listing) in matches.iter().enumerate() {
                if last_group != Some(listing.group) {
                    last_group = Some(listing.group);
                    lines.push(Line::from(Span::styled(
                        listing.group.label(strings).to_string(),
                        Style::default()
                            .fg(theme.roles.gray)
                            .add_modifier(Modifier::BOLD),
                    )));
                    row_of_line.push(None);
                }
                let selected_here = index == *selected;
                if selected_here {
                    selected_line = lines.len();
                }
                let style = if selected_here {
                    theme.selected()
                } else {
                    theme.base()
                };
                lines.push(Line::from(vec![
                    Span::styled(if selected_here { "▸ " } else { "  " }, style),
                    // Padded by columns, not characters: a Chinese name is two
                    // columns a glyph, and the hints have to line up under one
                    // another in every language.
                    Span::styled(
                        pad_to_width(listing.entry.label(strings), PALETTE_NAME_WIDTH),
                        style,
                    ),
                    Span::styled(listing.entry.hint(strings), theme.muted()),
                ]));
                row_of_line.push(Some(index));
            }
            let height = usize::from(rows[1].height);
            // Keep the selection on screen: the list can be longer than the
            // drawer, and the cursor is the anchor.
            let offset = selected_line.saturating_sub(height.saturating_sub(1));
            let visible = lines
                .into_iter()
                .skip(offset)
                .take(height)
                .collect::<Vec<_>>();
            let visible_rows = row_of_line
                .into_iter()
                .skip(offset)
                .take(height)
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(Text::from(visible)), rows[1]);
            app.regions.palette = Some(crate::app::PaletteRegion {
                rect: rows[1],
                rows: visible_rows,
            });
        }
        Overlay::Help {
            query,
            selected,
            collapsed,
        } => {
            let chrome = modal_chrome(
                app,
                strings.help_title(),
                ModalSizing::large(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("/", strings.search()),
                    ModalHint::new("←→", strings.hint_toggle()),
                    ModalHint::new("Esc", strings.close()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            render_shortcut_cheatsheet(
                frame,
                layout.content,
                app,
                theme,
                strings,
                query,
                *selected,
                collapsed,
            );
        }
        Overlay::Keys {
            query,
            selected,
            capturing,
            message,
            dirty,
        } => {
            let title = if *dirty {
                format!("{} {}", strings.settings_keys(), "●")
            } else {
                strings.settings_keys().to_string()
            };
            let chrome = modal_chrome(
                app,
                &title,
                ModalSizing::large(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("Enter", strings.keys_rebind()),
                    ModalHint::new("d", strings.keys_default()),
                    ModalHint::new("s", strings.keys_save()),
                    ModalHint::new("Esc", strings.close()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            render_keys_editor(
                frame,
                layout.content,
                app,
                theme,
                strings,
                KeysEditorView {
                    query,
                    selected: *selected,
                    capturing: *capturing,
                    message: message.as_deref(),
                    dirty: *dirty,
                },
            );
        }
        Overlay::Confirm { title, body, .. } => {
            let chrome = modal_chrome(
                app,
                title,
                ModalSizing::prompt(),
                vec![
                    ModalHint::new("Enter", strings.confirm()),
                    ModalHint::new("Esc", strings.cancel()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from(Span::styled(body.clone(), theme.base())),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!("[{}] ", strings.confirm()),
                            theme.accent().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("[{}] ", strings.cancel()), theme.muted()),
                    ]),
                ]))
                .wrap(Wrap { trim: true }),
                layout.content,
            );
        }
        Overlay::Prompt {
            title,
            value,
            field,
            ..
        } => {
            let chrome = modal_chrome(
                app,
                title,
                ModalSizing::prompt(),
                vec![
                    ModalHint::new("Enter", strings.confirm()),
                    ModalHint::new("Esc", strings.cancel()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            let masked = matches!(field, crate::app::PromptField::ProviderSecret)
                && !app.reveal_provider_secret;
            let shown = if masked {
                "•".repeat(value.chars().count())
            } else {
                value.clone()
            };
            let mut lines = vec![Line::from(Span::styled(shown, theme.base()))];
            if matches!(field, crate::app::PromptField::ProviderSecret) {
                if masked {
                    lines.push(Line::from(Span::styled(
                        strings.management_secret_write_only(),
                        theme.muted(),
                    )));
                }
                lines.push(Line::from(Span::styled(
                    if masked {
                        strings.management_secret_reveal_hint()
                    } else {
                        strings.management_secret_hide_hint()
                    },
                    theme.muted(),
                )));
            }
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                layout.content,
            );
        }
        Overlay::Models { view } => render_models(frame, area, app, view, theme),
        Overlay::NativeImport { view } => render_native_import(frame, area, app, view, theme),
        Overlay::NativeExport { view } => render_native_export(frame, area, app, view, theme),
        Overlay::SkillPreview { view } => render_skill_preview(frame, area, app, view, theme),
        Overlay::AgentAuth { view } => render_agent_auth(frame, area, app, view, theme),
        Overlay::Approval { selected } => render_approval(frame, area, app, *selected, theme),
        // The question card is not a box over the page: it is drawn in the
        // composer's band, which the frame paints before it gets here.
        Overlay::Elicitation { .. } => {}
        Overlay::RuntimePicker { view, selected } => {
            render_runtime_picker(frame, area, app, *view, *selected, theme);
        }
        Overlay::RunOptionValues {
            selected, option, ..
        } => {
            let chrome = modal_chrome(
                app,
                &option.label,
                ModalSizing::picker(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("Enter", strings.hint_select()),
                    ModalHint::new("Esc", strings.hint_back()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            // A long-winded option explains itself in a caption above the
            // values, so the rows stay one line each.
            let mut content = layout.content;
            if let Some(description) = option.description.as_deref()
                && content.height > 1
            {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        truncate_to_width(description, usize::from(content.width), "…"),
                        theme.dimmed(theme.roles.gray),
                    ))),
                    Rect {
                        height: 1,
                        ..content
                    },
                );
                content = Rect {
                    y: content.y + 1,
                    height: content.height - 1,
                    ..content
                };
            }
            let choices = app.run_option_choices(option);
            // The marker and the "Current" badge own the rest of the row; a
            // label takes what is left rather than a width guessed in advance.
            let label_width = usize::from(content.width)
                .saturating_sub(2 + strings.runtime_current().len() + 2)
                .max(8);
            let items = choices
                .iter()
                .enumerate()
                .map(|(index, (value, label))| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    let current = option.is_selected_value(value.as_deref());
                    let mut spans = vec![Span::styled(
                        if current { "● " } else { "  " }.to_string(),
                        Style::default().fg(theme.roles.accent_user),
                    )];
                    spans.push(Span::styled(
                        truncate_to_width(label, label_width, "…"),
                        if current {
                            style.add_modifier(Modifier::BOLD)
                        } else {
                            style
                        },
                    ));
                    if current {
                        spans.push(Span::styled(
                            format!("  {}", strings.runtime_current()),
                            Style::default().fg(theme.roles.accent_user),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect::<Vec<_>>();
            let mut state = ratatui::widgets::ListState::default();
            state.select(Some((*selected).min(items.len().saturating_sub(1))));
            frame.render_stateful_widget(List::new(items), content, &mut state);
        }
        Overlay::ProviderPresetPicker { selected, .. } => {
            let presets = vibex_core::provider_presets();
            let chrome = modal_chrome(
                app,
                strings.config_preset_picker_title(),
                ModalSizing::palette(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("Enter", strings.hint_select()),
                    ModalHint::new("Esc", strings.close()),
                ],
            );
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            let items = presets
                .iter()
                .enumerate()
                .map(|(index, preset)| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    // The second column says what the preset saves the reader
                    // from looking up: a plan's price, its address, or that it
                    // needs no key.
                    let detail = if let Some(note) = preset.plan_note.as_deref() {
                        note.to_string()
                    } else if preset.no_key {
                        strings.config_preset_no_key().to_string()
                    } else {
                        preset
                            .endpoint_hint
                            .clone()
                            .or_else(|| preset.website.clone())
                            .unwrap_or_else(|| {
                                preset
                                    .default_wire_api()
                                    .map(|wire_api| wire_api.wire_protocol_id().to_string())
                                    .unwrap_or_default()
                            })
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{:<24}", preset.display_name), style),
                        Span::styled(detail, Style::default().fg(theme.roles.gray)),
                    ]))
                })
                .collect::<Vec<_>>();
            let mut state = ratatui::widgets::ListState::default();
            state.select(Some((*selected).min(items.len().saturating_sub(1))));
            frame.render_stateful_widget(List::new(items), layout.content, &mut state);
        }
        Overlay::WorkspacePicker { selected } => {
            let path = app
                .workspace_browse
                .as_ref()
                .map(|listing| listing.path.clone())
                .unwrap_or_default();
            let title = format!(
                "{}  {}",
                strings.session_workspace_label(),
                compact_path(&path, 40)
            );
            let rows = app.workspace_picker_rows();
            let mut hints = vec![
                ModalHint::new("↑↓", strings.hint_nav()),
                // Enter opens the row under the cursor, so the footer says so
                // rather than promising a choice the key no longer makes.
                ModalHint::new("Enter", strings.hint_open()),
            ];
            // The key that takes a directory is named only once there is one
            // to take, and it says which one it would take: the folder under
            // the cursor, or — on the `..` row, where the cursor names a step
            // rather than a folder — the directory the picker is showing.
            if !path.is_empty() {
                let label = match rows.get(*selected) {
                    Some(crate::app::WorkspacePickerRow::Entry { .. }) => {
                        strings.workspace_use_selected()
                    }
                    _ => strings.workspace_use_this(),
                };
                hints.push(ModalHint::new("Space", label));
            }
            // The way up is named only where there is somewhere to go: a footer
            // that offers a step the listing cannot take is the same lie as an
            // empty directory with no way out of it.
            if rows
                .iter()
                .any(|row| matches!(row, crate::app::WorkspacePickerRow::Parent { .. }))
            {
                hints.push(ModalHint::new("←", strings.workspace_parent()));
            }
            hints.push(ModalHint::new("Esc", strings.close()));
            let chrome = modal_chrome(app, &title, ModalSizing::picker(), hints);
            let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
                return;
            };
            let items = rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    // The way up is drawn where a reader looks for it, and
                    // dimmed while unselected so it reads as the step the
                    // listing carries rather than a directory inside it.
                    let (label, style) = match row {
                        crate::app::WorkspacePickerRow::Parent { .. } => (
                            "..".to_string(),
                            if index == *selected {
                                style
                            } else {
                                theme.dimmed(theme.roles.gray_dim)
                            },
                        ),
                        crate::app::WorkspacePickerRow::Entry { name, .. } => {
                            (truncate_to_width(name, 44, "…"), style)
                        }
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if index == *selected { "❯ " } else { "  " }.to_string(),
                            Style::default().fg(theme.roles.accent_user),
                        ),
                        Span::styled(label, style),
                    ]))
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                // An empty directory is a fact, not an error: say so where the
                // rows would have been.
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("  {}", strings.workspace_empty()),
                        theme.dimmed(theme.roles.gray_dim),
                    ))),
                    layout.content,
                );
                return;
            }
            let mut state = ratatui::widgets::ListState::default();
            state.select(Some((*selected).min(items.len() - 1)));
            frame.render_stateful_widget(List::new(items), layout.content, &mut state);
        }
        Overlay::BlockDetails { block, scroll } => {
            let detail = block_detail_text(&mut app.transcript, *block);
            if let Some((title, body)) = detail {
                render_text_view(frame, area, app, &title, &body, *scroll, theme);
            }
        }
        Overlay::TextView {
            title,
            body,
            scroll,
        } => render_text_view(frame, area, app, title, body, *scroll, theme),
    }
}

fn render_approval(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    selected: usize,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let approvals = app.approvals();
    let Some(approval) = approvals.get(selected) else {
        return;
    };
    // The risk taxonomy is the provider's; the terminal only maps it onto how
    // loudly the card should read.
    let risk = match approval.risk_category {
        vibex_core::PermissionRiskCategory::Command
        | vibex_core::PermissionRiskCategory::CustomTool => strings.risk_medium(),
        vibex_core::PermissionRiskCategory::FileReadSensitive => strings.risk_low(),
        vibex_core::PermissionRiskCategory::FileWrite => strings.risk_medium(),
        vibex_core::PermissionRiskCategory::FileDeleteOrMove
        | vibex_core::PermissionRiskCategory::GitDestructive => strings.risk_critical(),
        vibex_core::PermissionRiskCategory::Network => strings.risk_high(),
        vibex_core::PermissionRiskCategory::ProviderConfigExport => strings.risk_high(),
    };
    let title = format!(
        "{0} {1}",
        crate::glyphs::diamond_filled(app.glyph_tier()),
        strings.approval_title()
    );
    let chrome = modal_chrome(
        app,
        &title,
        ModalSizing::card(),
        vec![
            ModalHint::new("a", strings.approval_allow()),
            ModalHint::new("d", strings.approval_deny()),
            ModalHint::new("Ctrl+A", strings.approval_always()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(approval.title.clone(), theme.strong())),
        Line::from(vec![
            Span::styled(format!("{}: ", strings.approval_risk()), theme.muted()),
            Span::styled(risk, theme.warning()),
        ]),
        Line::from(""),
    ];
    for (label, value) in approval.details.iter().take(6) {
        lines.push(Line::from(vec![
            Span::styled(format!("{label}: "), theme.muted()),
            Span::styled(
                truncate_to_width(
                    value,
                    usize::from(layout.content.width).saturating_sub(label.len() + 4),
                    "…",
                ),
                theme.base(),
            ),
        ]));
    }
    lines.push(Line::from(""));
    // The response options are provider-advertised, so the card never invents
    // an allow/deny pair the Agent would reject.
    let options = approval
        .response_options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let key = index + 1;
            Span::styled(
                format!("[{key}] {}  ", option.label),
                theme.accent().add_modifier(Modifier::BOLD),
            )
        })
        .collect::<Vec<_>>();
    lines.push(Line::from(options));
    if approval.pending {
        lines.push(Line::from(Span::styled(
            strings.approval_pending(),
            theme.muted(),
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }),
        layout.content,
    );
}

/// The question card, drawn where the composer is.
///
/// The desktop puts this card inline in the timeline; the terminal puts it in
/// the composer's band, because that is where the reader is already looking and
/// already typing, and a question should not have to be found before it can be
/// answered. It wears the composer's own panel — the same rail, the same raised
/// surface — so it reads as the draft's place rather than as a window over the
/// page.
///
/// Each question is a tab. `Tab` and `←`/`→` walk the tabs, `↑`/`↓` walk the
/// open question's options, `Enter` confirms the answer and moves on, and the
/// last tab sends the form. The footer names the keys the open tab actually
/// answers to, so it never promises a toggle to a question that is typed into.
fn render_elicitation_card(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    let Some(form) = app.elicitation_form() else {
        return;
    };
    let empty = crate::reduce::ElicitationDraft::default();
    let draft = app
        .elicitation_draft(form.request.id.as_str())
        .unwrap_or(&empty);
    let card = elicitation_card(app, &form, draft, theme, area.width);

    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.roles.surface_raised)),
        area,
    );
    render_composer_edge(frame, area, theme);
    let text = Rect {
        x: area.x.saturating_add(BOX_EDGE_COLUMNS + 1),
        width: area.width.saturating_sub(BOX_EDGE_COLUMNS + 2),
        ..area
    };
    if text.width == 0 || text.height == 0 {
        return;
    }

    // One row of padding above the first row, like the draft's box: the panel
    // never touches its own rail.
    let width = usize::from(text.width);
    let top = usize::from(text.height > 2);
    // Every measurement is in *display* rows, not lines: a question or an
    // option wider than the band wraps, and a window that counted lines would
    // push the row the cursor is on out of the box it promised to show.
    let height = usize::from(text.height).saturating_sub(top);
    let header_rows = card_rows(&card.header, width).min(height);
    let footer_rows = card_rows(&card.footer, width).min(height.saturating_sub(header_rows));
    // The question and the tabs stay put; the options are what scrolls when the
    // band is shorter than the question, because they are the part the reader
    // is moving through and the title is the part that says what they are on.
    let body_rows = height.saturating_sub(header_rows + footer_rows);
    let cursor_row = card_rows(&card.body[..card.cursor.min(card.body.len())], width);
    let scroll = cursor_row
        .saturating_add(1)
        .saturating_sub(body_rows.max(1))
        .min(card_rows(&card.body, width).saturating_sub(body_rows));

    render_card_rows(frame, text, top, header_rows, &card.header, 0, theme);
    render_card_rows(
        frame,
        text,
        top + header_rows,
        body_rows,
        &card.body,
        scroll,
        theme,
    );
    let footer_top = top + height.saturating_sub(footer_rows);
    render_card_rows(frame, text, footer_top, footer_rows, &card.footer, 0, theme);
    // The footer names the keys the card answers to, and a card that takes the
    // composer's place would otherwise be the one panel whose printed keys do
    // nothing when clicked. A footer that wrapped has no one row per key, so
    // only the unwrapped form is published; the keys still work.
    if footer_rows == card.footer.len() {
        for (line, column, width, key) in card.footer_keys {
            let Some(chord) = Chord::from_hint(&key) else {
                continue;
            };
            app.regions.key_hints.push(crate::app::KeyHintRegion {
                rect: Rect {
                    x: text.x + column,
                    y: text.y + (footer_top + line) as u16,
                    width,
                    height: 1,
                },
                chord,
            });
        }
    }
}

/// How many display rows these lines take at `width` columns.
fn card_rows(lines: &[Line<'static>], width: usize) -> usize {
    lines
        .iter()
        .map(|line| {
            crate::text::wrap_text(&line.to_string(), width)
                .len()
                .max(1)
        })
        .sum()
}

/// Draw `lines` into `rows` display rows at `offset`, scrolled by `scroll`.
fn render_card_rows(
    frame: &mut Frame<'_>,
    text: Rect,
    offset: usize,
    rows: usize,
    lines: &[Line<'static>],
    scroll: usize,
    theme: &TuiTheme,
) {
    if rows == 0 || lines.is_empty() {
        return;
    }
    let area = Rect {
        y: text.y.saturating_add(offset as u16),
        height: (rows as u16).min(text.height.saturating_sub(offset as u16)),
        ..text
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines.to_vec()))
            .style(theme.base().bg(theme.roles.surface_raised))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        area,
    );
}

/// The card's rows, split so the renderer can scroll the middle and the layout
/// can measure the whole.
/// One key in a card's footer: the line it is on, the column it starts at, its
/// width, and the text it was drawn with.
///
/// The four travel together because a footer is translated and may wrap: a
/// caller that had only the key's text would have to find it in the finished
/// line, which is a guess about what the translation put around it.
type FooterKey = (usize, u16, u16, String);

struct ElicitationCard {
    /// The tab strip and the open question.
    header: Vec<Line<'static>>,
    /// The options, the answer field or the recorded answers.
    body: Vec<Line<'static>>,
    /// The body row the option cursor is on, so the window can follow it.
    cursor: usize,
    /// The key legend, with the column of each key inside its line.
    ///
    /// The footer is the card's only place to say what it answers to, and a
    /// pointer aimed at `Enter` there means the same thing the key does. The
    /// columns ride with the lines rather than being recovered by guessing
    /// which span is a key: a label may contain a middot or a space, and a
    /// footer is translated.
    footer: Vec<Line<'static>>,
    footer_keys: Vec<FooterKey>,
}

/// How many option rows the card shows before the list starts scrolling.
///
/// A question can carry two dozen options; the transcript behind the card is
/// worth more than the ones nobody is looking at, so the rest wait below the
/// window and the cursor brings them in.
const MAX_CARD_BODY_ROWS: usize = 10;

/// The card's natural height at `width` columns, panel included.
///
/// The layout squeezes this like every other band — the transcript keeps its
/// rows first — and the renderer scrolls the option list under whatever is
/// left, so this is what the card would like rather than what it gets.
fn elicitation_card_height(app: &App, form: &crate::app::ElicitationForm, width: u16) -> u16 {
    let empty = crate::reduce::ElicitationDraft::default();
    let draft = app
        .elicitation_draft(form.request.id.as_str())
        .unwrap_or(&empty);
    let theme = &app.theme;
    let card = elicitation_card(app, form, draft, theme, width);
    let text_width = usize::from(width).saturating_sub(usize::from(BOX_EDGE_COLUMNS) + 2);
    let body = card_rows(&card.body, text_width).min(MAX_CARD_BODY_ROWS);
    // One padding row above the first row and one below the last, like the
    // draft's box: the panel never touches its own rail.
    (card_rows(&card.header, text_width) + body + card_rows(&card.footer, text_width) + 2).max(6)
        as u16
}

/// Build the card: tabs, question, options and legend.
fn elicitation_card(
    app: &App,
    form: &crate::app::ElicitationForm,
    draft: &crate::reduce::ElicitationDraft,
    theme: &TuiTheme,
    width: u16,
) -> ElicitationCard {
    use vibex_core::ElicitationFieldKind;
    let strings = app.strings;
    let tier = app.glyph_tier();
    let text_width = usize::from(width).saturating_sub(usize::from(BOX_EDGE_COLUMNS) + 2);
    let mut header = vec![Line::from(elicitation_tabs(form, draft, strings, theme))];
    header.push(Line::from(""));
    if form.confirming() {
        header.push(Line::from(Span::styled(
            strings.elicitation_confirm().to_string(),
            theme.strong(),
        )));
    } else if let Some(question) = form.question() {
        let required = if question.required {
            format!("  *{}", strings.elicitation_required())
        } else {
            String::new()
        };
        header.push(Line::from(vec![
            Span::styled(question.title.clone(), theme.strong()),
            Span::styled(required, theme.warning()),
        ]));
        if let Some(description) = question
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
        {
            header.push(Line::from(Span::styled(
                description.to_string(),
                theme.muted(),
            )));
        }
    }
    header.push(Line::from(""));

    let mut body: Vec<Line<'static>> = Vec::new();
    let mut cursor = 0usize;
    if form.confirming() {
        for question in &form.request.fields {
            let answer = draft.answer_for(&question.id, question);
            let missing = answer.is_none() && question.required;
            let value = match answer {
                Some(answer) => answer_label(question, &answer, strings),
                None => strings.elicitation_unanswered().to_string(),
            };
            body.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{}  ",
                        truncate_to_width(&question.title, text_width.saturating_sub(12), "…")
                    ),
                    theme.muted(),
                ),
                Span::styled(
                    value,
                    if missing {
                        theme.warning()
                    } else {
                        theme.base()
                    },
                ),
            ]));
        }
    } else if let Some(question) = form.question() {
        match &question.kind {
            ElicitationFieldKind::Text { options, .. } if !options.is_empty() => {
                let chosen = draft.text(&question.id);
                for (index, option) in options.iter().enumerate() {
                    if index == form.choice {
                        cursor = body.len();
                    }
                    body.push(elicitation_option_line(
                        index,
                        &option.title,
                        chosen == Some(option.value.as_str()),
                        false,
                        index == form.choice,
                        tier,
                        theme,
                    ));
                    body.extend(option_description(option.description.as_deref(), theme));
                }
            }
            ElicitationFieldKind::MultiSelect { options, .. } => {
                let selected = draft.multi(&question.id);
                for (index, option) in options.iter().enumerate() {
                    if index == form.choice {
                        cursor = body.len();
                    }
                    body.push(elicitation_option_line(
                        index,
                        &option.title,
                        selected.contains(&option.value),
                        true,
                        index == form.choice,
                        tier,
                        theme,
                    ));
                    body.extend(option_description(option.description.as_deref(), theme));
                }
            }
            ElicitationFieldKind::Boolean { default } => {
                let value = draft.boolean(&question.id).or(*default);
                for (index, label) in [strings.elicitation_yes(), strings.elicitation_no()]
                    .into_iter()
                    .enumerate()
                {
                    if index == form.choice {
                        cursor = body.len();
                    }
                    body.push(elicitation_option_line(
                        index,
                        label,
                        value == Some(index == 0),
                        false,
                        index == form.choice,
                        tier,
                        theme,
                    ));
                }
            }
            ElicitationFieldKind::Text { .. }
            | ElicitationFieldKind::Number { .. }
            | ElicitationFieldKind::Integer { .. } => {
                // A terminal field is one line edited at its end: the value and
                // the caret that follows it are the whole control, and an empty
                // one says what it takes rather than being a blank row.
                let value = draft.text(&question.id).unwrap_or_default();
                let shown = if value.is_empty() {
                    format!("({})", field_kind_label(question, strings))
                } else {
                    value.to_string()
                };
                body.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", crate::glyphs::prompt_arrow(tier)),
                        Style::default().fg(theme.roles.accent_user),
                    ),
                    Span::styled(shown, theme.base()),
                    Span::styled(crate::glyphs::caret(tier).to_string(), theme.accent()),
                ]));
            }
            ElicitationFieldKind::Unsupported { schema_type } => {
                body.push(Line::from(Span::styled(
                    format!("{}: {schema_type}", strings.elicitation_unsupported()),
                    theme.warning(),
                )));
            }
        }
    }

    let (footer_lines, footer_keys) = elicitation_hints(form, strings, theme);
    ElicitationCard {
        header,
        body,
        cursor,
        footer: footer_lines,
        footer_keys,
    }
}

/// The tab strip: one tab per question, then the Confirm tab.
///
/// A question that already has an answer is drawn in the ordinary ink rather
/// than the muted one, so a reader walking the tabs can see what is left
/// without opening each of them.
fn elicitation_tabs(
    form: &crate::app::ElicitationForm,
    draft: &crate::reduce::ElicitationDraft,
    strings: Strings,
    theme: &TuiTheme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, question) in form.request.fields.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let answered = draft.answer_for(&question.id, question).is_some();
        let style = if index == form.field {
            theme.selected()
        } else if answered {
            theme.base()
        } else {
            theme.muted()
        };
        spans.push(Span::styled(
            format!(" {} ", truncate_to_width(&question.title, 24, "…")),
            style,
        ));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!(" {} ", strings.elicitation_confirm()),
        if form.confirming() {
            theme.selected()
        } else {
            theme.muted()
        },
    ));
    spans
}

/// One option row: its number, its label, and the marks that say whether it is
/// the answer and whether the cursor is on it.
///
/// The number is the key that picks the row outright, so it is drawn rather
/// than left to the reader to count. The cursor is reverse video across the
/// whole row, which is what survives a terminal with no colour.
fn elicitation_option_line(
    index: usize,
    label: &str,
    chosen: bool,
    ticking: bool,
    cursor: bool,
    tier: crate::glyphs::GlyphTier,
    theme: &TuiTheme,
) -> Line<'static> {
    let row = if cursor {
        theme.selected()
    } else {
        theme.base()
    };
    let mut spans = vec![
        Span::styled(format!("{}. ", index + 1), theme.muted()),
        Span::styled(label.to_string(), row),
    ];
    if ticking {
        spans.insert(
            0,
            Span::styled(format!("{} ", crate::glyphs::checkbox(chosen, tier)), row),
        );
    } else if chosen {
        spans.push(Span::styled(
            format!(" {}", crate::glyphs::check_mark(tier)),
            theme.success(),
        ));
    }
    Line::from(spans)
}

/// The description under an option, indented to the label it belongs to.
fn option_description(description: Option<&str>, theme: &TuiTheme) -> Option<Line<'static>> {
    let description = description.map(str::trim).filter(|text| !text.is_empty())?;
    Some(Line::from(Span::styled(
        format!("   {description}"),
        theme.dimmed(theme.roles.gray_dim),
    )))
}

/// The key legend under the card, in the keys the open tab answers to.
/// The card's key legend, and where each key sits inside it.
///
/// The key's text rides with its column so a pointer press lands on the cells
/// the reader is reading: the footer is composed here, and re-finding a key in
/// the finished text would guess at a translation that happens to contain the
/// same character.
fn elicitation_hints(
    form: &crate::app::ElicitationForm,
    strings: Strings,
    theme: &TuiTheme,
) -> (Vec<Line<'static>>, Vec<FooterKey>) {
    let mut hints = vec![("Tab".to_string(), strings.hint_switch().to_string())];
    if form.confirming() {
        hints.push((
            "Enter".to_string(),
            strings.elicitation_submit().to_string(),
        ));
    } else {
        let ticking = form.question().is_some_and(|question| {
            matches!(
                question.kind,
                vibex_core::ElicitationFieldKind::MultiSelect { .. }
            )
        });
        if !app_question_takes_text(form) {
            hints.push(("↑↓".to_string(), strings.hint_select().to_string()));
        }
        if ticking {
            hints.push(("Space".to_string(), strings.hint_toggle().to_string()));
        }
        hints.push((
            "Enter".to_string(),
            strings.elicitation_confirm().to_string(),
        ));
    }
    hints.push((
        "Ctrl+D".to_string(),
        strings.elicitation_decline().to_string(),
    ));
    hints.push(("Esc".to_string(), strings.elicitation_dismiss().to_string()));

    let mut spans = vec![Span::raw(" ")];
    let mut keys = Vec::new();
    let mut column = 1u16;
    for (index, (key, label)) in hints.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", theme.dimmed(theme.roles.gray_dim)));
            column += 3;
        }
        let width = display_width(&key) as u16;
        keys.push((1, column, width, key.clone()));
        spans.push(Span::styled(key, theme.muted()));
        spans.push(Span::styled(
            format!(" {label}"),
            theme.dimmed(theme.roles.gray_dim),
        ));
        column += width + 1 + display_width(&label) as u16;
    }
    (vec![Line::from(""), Line::from(spans)], keys)
}

/// Whether the open question is typed into rather than chosen from.
fn app_question_takes_text(form: &crate::app::ElicitationForm) -> bool {
    use vibex_core::ElicitationFieldKind;
    match form.question().map(|question| &question.kind) {
        Some(ElicitationFieldKind::Text { options, .. }) => options.is_empty(),
        Some(ElicitationFieldKind::Number { .. } | ElicitationFieldKind::Integer { .. }) => true,
        _ => false,
    }
}

/// One recorded answer, as the Confirm tab reads it back.
///
/// An answer that names an option is read back as the option's label: the
/// Confirm tab is for the reader to check what they chose, and the value on the
/// wire is not what they saw.
fn answer_label(
    question: &vibex_core::ElicitationField,
    answer: &vibex_core::ElicitationAnswerValue,
    strings: Strings,
) -> String {
    use vibex_core::ElicitationFieldKind;
    let options = match &question.kind {
        ElicitationFieldKind::Text { options, .. }
        | ElicitationFieldKind::MultiSelect { options, .. } => options.as_slice(),
        _ => &[],
    };
    let label = |value: &str| {
        options
            .iter()
            .find(|option| option.value == value)
            .map(|option| option.title.clone())
            .unwrap_or_else(|| value.to_string())
    };
    match answer {
        vibex_core::ElicitationAnswerValue::String(value) => label(value),
        vibex_core::ElicitationAnswerValue::StringArray(values) => values
            .iter()
            .map(|value| label(value))
            .collect::<Vec<_>>()
            .join(", "),
        other => answer_text(other, strings),
    }
}

fn answer_text(answer: &vibex_core::ElicitationAnswerValue, strings: Strings) -> String {
    match answer {
        vibex_core::ElicitationAnswerValue::String(value) => value.clone(),
        vibex_core::ElicitationAnswerValue::Integer(value) => value.to_string(),
        vibex_core::ElicitationAnswerValue::Number(value) => value.clone(),
        vibex_core::ElicitationAnswerValue::Boolean(value) => {
            if *value {
                strings.elicitation_yes().to_string()
            } else {
                strings.elicitation_no().to_string()
            }
        }
        vibex_core::ElicitationAnswerValue::StringArray(values) => values.join(", "),
    }
}

fn field_kind_label(definition: &vibex_core::ElicitationField, strings: Strings) -> &'static str {
    use vibex_core::ElicitationFieldKind;
    match definition.kind {
        ElicitationFieldKind::Text { .. } => strings.elicitation_field_text(),
        ElicitationFieldKind::Number { .. } => strings.elicitation_field_number(),
        ElicitationFieldKind::Integer { .. } => strings.elicitation_field_integer(),
        ElicitationFieldKind::Boolean { .. } => strings.elicitation_field_boolean(),
        ElicitationFieldKind::MultiSelect { .. } => strings.elicitation_field_multi(),
        _ => strings.elicitation_unsupported(),
    }
}

fn render_text_view(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    title: &str,
    body: &str,
    scroll: usize,
    theme: &TuiTheme,
) {
    // The modal chrome's hints are the only copy this view needs, and the app
    // already carries the reader's own table.
    let strings = app.strings;
    let chrome = modal_chrome(
        app,
        title,
        ModalSizing::document(),
        vec![
            ModalHint::new("↑↓", strings.hint_scroll()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    let lines = body
        .split('\n')
        .skip(scroll)
        .take(usize::from(layout.content.height))
        .map(|line| {
            let style = if line.starts_with('+') && !line.starts_with("+++") {
                theme.success()
            } else if line.starts_with('-') && !line.starts_with("---") {
                theme.danger()
            } else if line.starts_with("@@") {
                theme.accent()
            } else {
                theme.base()
            };
            Line::from(Span::styled(
                truncate_to_width(line, usize::from(layout.content.width), "…"),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), layout.content);
}

/// The Agent setup picker.
///
/// Two views of one question. The catalogue is a tree — Agent headings with
/// their accounts and models under them, and the pinned recent and starred
/// entries above the lot — because a flat list of every model on the machine
/// says nothing about which Agent is which. The run options are the other side
/// of `Tab`: how the entry the reader chose runs.
fn render_runtime_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: crate::app::RuntimePickerView,
    selected: usize,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let show_choices = view == crate::app::RuntimePickerView::Choices;
    let run_options = app.picker_run_options();
    // The title names the surface's two halves: the catalogue chooses an Agent,
    // and the second view says how it runs. What the choice *moves* is said by
    // the rows instead — the entry in use is marked, and the run options name
    // the Agent they belong to — so the title does not have to guess which
    // session the reader came from.
    let title = if show_choices {
        format!(
            "{} · {}",
            strings.runtime_title(),
            strings.runtime_choose_agent()
        )
    } else {
        format!(
            "{} · {}",
            strings.runtime_title(),
            strings.runtime_run_options()
        )
    };
    let hints = if show_choices {
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("←→", strings.runtime_fold()),
            ModalHint::new("/", strings.runtime_search()),
            ModalHint::new("Tab", strings.runtime_run_options()),
            ModalHint::new("Enter", strings.hint_select()),
            ModalHint::new("Esc", strings.close()),
        ]
    } else {
        // No apply key: a run option is sent the moment it is set, so the
        // footer promises only what the rows do.
        vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.hint_select()),
            ModalHint::new("r", strings.runtime_reset()),
            ModalHint::new("Tab", strings.runtime_title()),
            ModalHint::new("Esc", strings.hint_back()),
        ]
    };
    let chrome = modal_chrome(app, &title, ModalSizing::picker(), hints);
    let Some(layout) = render_modal(frame, area, &chrome, theme, app) else {
        return;
    };
    if show_choices {
        render_runtime_catalogue(frame, layout.content, app, selected, theme);
    } else {
        render_runtime_options(frame, layout.content, app, selected, &run_options, theme);
    }
}

/// The catalogue: a filter line, the tree, and a preview of what the entry under
/// the cursor would run with.
fn render_runtime_catalogue(
    frame: &mut Frame<'_>,
    content: Rect,
    app: &mut App,
    selected: usize,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let query = app.runtime_picker.query.clone();
    let filtering = app.runtime_picker.filtering;
    let rows = app.runtime_picker_rows();
    let options = app
        .runtime_options
        .as_ref()
        .map(|catalog| catalog.options.clone())
        .unwrap_or_default();
    let mut content = content;
    if content.height == 0 {
        return;
    }
    // The filter has its own line whenever it is being typed or has something in
    // it, so a catalogue that is shorter than it looks says why.
    if filtering || !query.is_empty() {
        let line = if filtering {
            Line::from(vec![
                Span::styled("/ ", theme.accent()),
                Span::styled(
                    truncate_to_width(&query, usize::from(content.width.saturating_sub(3)), "…"),
                    theme.base(),
                ),
                Span::styled("▏", theme.accent()),
            ])
        } else {
            Line::from(Span::styled(
                truncate_to_width(&format!("/ {query}"), usize::from(content.width), "…"),
                theme.dimmed(theme.roles.gray),
            ))
        };
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                height: 1,
                ..content
            },
        );
        content = Rect {
            y: content.y + 1,
            height: content.height - 1,
            ..content
        };
    }
    if content.height == 0 {
        return;
    }
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_to_width(strings.runtime_no_match(), usize::from(content.width), "…"),
                theme.dimmed(theme.roles.gray),
            ))),
            content,
        );
        app.runtime_picker.scroll = 0;
        app.runtime_picker.page_rows = usize::from(content.height);
        return;
    }
    // The preview line is the inspector this surface has: what the highlighted
    // entry was last run with, one `Tab` away from being changed. It is only
    // drawn when there is room for a list above it, and it keeps a line of air
    // under it: the line describes the list, and the keys below are the modal's
    // rather than its own, so the two must not read as one block.
    let preview = content.height >= 6;
    if preview {
        content.height -= 2;
    }
    let width = usize::from(content.width);
    // The list is laid out as *lines* rather than as rows, because two of the
    // things it draws are not rows: a line of air parts one Agent's group from
    // the next, and a rule parts the pinned sections from the Agents. Each line
    // therefore carries the catalogue row it shows — nothing, for the air and
    // the rule — which is what the window, the page keys and the pointer count
    // in. The table is built in the same pass, so a line can never be pushed
    // without saying what it is.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len() + 2);
    let mut row_of_line: Vec<Option<usize>> = Vec::with_capacity(rows.len() + 2);
    let mut line_of_row: Vec<usize> = Vec::with_capacity(rows.len());
    // Every Agent mark the lines want, in the coordinates of the laid-out list.
    // Like the session list's, a mark is moved onto the screen once the window is
    // known, so one on a line the reader has scrolled past is never placed.
    let mut wanted_marks: Vec<(usize, SessionRowMark)> = Vec::new();
    // The account column, measured once from the catalogue rather than from each
    // row: the widest account the list has to show, capped so one long Provider
    // Profile cannot spend the room the models need. Every group row pads its
    // account to it, which is what makes the model beside it start at the same
    // offset all the way down — the ragged column this replaces started it
    // wherever the account happened to end.
    let auth_column =
        runtime_auth_column(&strings, &options, width.saturating_sub(RUNTIME_ENTRY_LEAD));
    let mut in_group = false;
    let mut in_section = false;
    for (index, row) in rows.iter().enumerate() {
        match row {
            RuntimePickerRow::Section(section) => {
                // Air above a pinned section: two sections are two lists, and
                // their titles are where one ends and the next begins.
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                    row_of_line.push(None);
                }
                line_of_row.push(lines.len());
                in_group = false;
                in_section = true;
                // Flush with the list's left edge, where the rule under it and
                // the Agents' disclosures are: the title names the section rather
                // than sitting in one of its rows, and the rows keep the indent
                // their own markers give them.
                lines.push(Line::from(Span::styled(
                    truncate_to_width(section.label(&strings), width, "…"),
                    theme.dimmed(theme.roles.gray).add_modifier(Modifier::BOLD),
                )));
                row_of_line.push(Some(index));
            }
            RuntimePickerRow::Agent { .. } => {
                // The pinned sections and the Agents are the two halves of this
                // list, and a rule is what says so: the air between two Agents
                // says a group ended, not that the shortcuts did. The rule is
                // drawn between two rows, so it shows no row of its own.
                if in_section {
                    in_section = false;
                    lines.push(runtime_picker_rule(width, app.glyph_tier(), theme));
                    row_of_line.push(None);
                } else if !lines.is_empty() {
                    lines.push(Line::from(""));
                    row_of_line.push(None);
                }
                let at = lines.len();
                line_of_row.push(at);
                in_group = true;
                let (line, identity) =
                    runtime_picker_heading_line(app, row, index == selected, width, theme);
                if let Some(identity) = identity {
                    // The heading's own two-cell disclosure is the lead the mark
                    // sits after.
                    wanted_marks.push((
                        at,
                        SessionRowMark {
                            line: 0,
                            column: 2,
                            mark: crate::row_marks::Mark::Agent(identity),
                            color: theme.roles.foreground,
                        },
                    ));
                }
                lines.push(line);
                row_of_line.push(Some(index));
            }
            RuntimePickerRow::Entry { .. } => {
                let at = lines.len();
                line_of_row.push(at);
                let (line, mark) = runtime_picker_entry_line(
                    app,
                    &options,
                    row,
                    index == selected,
                    in_group,
                    width,
                    auth_column,
                    theme,
                );
                if let Some((column, identity)) = mark {
                    wanted_marks.push((
                        at,
                        SessionRowMark {
                            line: 0,
                            column,
                            mark: crate::row_marks::Mark::Agent(identity),
                            color: theme.roles.foreground,
                        },
                    ));
                }
                lines.push(line);
                row_of_line.push(Some(index));
            }
        }
    }
    // Keep the cursor on screen with the least movement: the window only moves
    // when the cursor leaves it, which is what makes `↑↓` feel like a list
    // rather than like a camera. The offset is recorded because the page keys
    // and the scroll bar both need to know where the window ended up.
    let selected = selected.min(rows.len().saturating_sub(1));
    let height = usize::from(content.height);
    let selected_line = line_of_row.get(selected).copied().unwrap_or(0);
    let mut offset = app
        .runtime_picker
        .scroll
        .min(lines.len().saturating_sub(height.min(lines.len())));
    if selected_line < offset {
        offset = selected_line;
    }
    if selected_line >= offset + height {
        offset = (selected_line + 1).saturating_sub(height);
    }
    app.runtime_picker.scroll = offset;
    // A page is what the reader can see, and the air between two groups is not
    // a row they can land on: the page key counts the rows the window drew.
    let drawn_rows = row_of_line
        .iter()
        .skip(offset)
        .take(height)
        .filter(|row| row.is_some())
        .count();
    app.runtime_picker.page_rows = drawn_rows.max(1);
    app.regions.runtime_picker = Some(crate::app::RuntimePickerRegion {
        rect: content,
        rows: row_of_line
            .iter()
            .skip(offset)
            .take(height)
            .copied()
            .collect(),
    });

    // The scroll bar is one column of the list rather than a widget: a reader
    // who cannot see how long the catalogue is cannot tell a short list from a
    // clipped one.
    let total = lines.len();
    let bar = total > height && content.width > 4;
    let list_area = if bar {
        Rect {
            width: content.width - 1,
            ..content
        }
    } else {
        content
    };
    frame.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .skip(offset)
                .take(height)
                .collect::<Vec<_>>(),
        )),
        list_area,
    );
    // The Agent marks land on the lines the window actually drew. Like the
    // session list's, a mark on a line scrolled past, or one a terminal too
    // narrow to hold it has pushed out of the list, is not placed at all rather
    // than left standing over the row below.
    for (line, mark) in &wanted_marks {
        let Some(screen_line) = line.checked_sub(offset) else {
            continue;
        };
        if screen_line >= height {
            continue;
        }
        let column = list_area.x.saturating_add(mark.column);
        if column + crate::row_marks::MARK_COLS > list_area.right() {
            continue;
        }
        let rect = Rect {
            x: column,
            y: list_area.y.saturating_add(screen_line as u16),
            width: crate::row_marks::MARK_COLS,
            height: crate::row_marks::MARK_ROWS,
        };
        let _ = app.row_marks.show(
            &app.terminal_images,
            &mark.mark,
            crate::theme::color_rgb(mark.color),
            rect,
        );
    }
    if bar {
        render_scroll_indicator(frame, content, offset, total, theme);
    }
    if preview {
        let entry = rows.get(selected).and_then(|row| row.entry());
        let body = runtime_picker_preview(
            app,
            entry.and_then(|index| options.get(index)),
            usize::from(content.width),
            theme,
        );
        frame.render_widget(
            Paragraph::new(body),
            Rect {
                y: content.y + content.height,
                height: 1,
                ..content
            },
        );
    }
}

/// The rule that parts the pinned sections from the Agents below them.
///
/// The two halves of the list answer different questions — "what have I used"
/// and "what is there" — and the air that parts two Agents' groups cannot say
/// where one half ends: it says a group ended. A rule from the list's left edge
/// to its right is what a reader reads as the end of a section, and it is drawn
/// in the dim step for the reason every other rule here is: punctuation between
/// sections, not a line of the page.
fn runtime_picker_rule(
    width: usize,
    tier: crate::glyphs::GlyphTier,
    theme: &TuiTheme,
) -> Line<'static> {
    Line::from(Span::styled(
        crate::glyphs::heading_rule(tier).repeat(width),
        theme.dimmed(theme.roles.gray_dim),
    ))
}

/// One Agent's heading: what it holds, and whether it is the one in use.
///
/// The heading is the one row in the catalogue that is *about* an Agent rather
/// than about a way to run it, so it is where the Agent's mark belongs: a
/// column of marks is how a reader finds the Agent they came for without
/// reading every group title. The two cells the mark covers are reserved here
/// and reported back, because only the caller knows which line of the window
/// the heading landed on.
fn runtime_picker_heading_line(
    app: &App,
    row: &RuntimePickerRow,
    selected: bool,
    width: usize,
    theme: &TuiTheme,
) -> (Line<'static>, Option<String>) {
    let strings = app.strings;
    let RuntimePickerRow::Agent {
        agent_id,
        label,
        count,
        folded,
        current,
        unavailable,
    } = row
    else {
        return (Line::from(""), None);
    };
    // A mark is only drawn when the terminal can hold a picture and the Agent
    // has one; the heading's own text is what every other case gets, which is
    // the state this pointer is `None` in.
    let mark_width = usize::from(crate::row_marks::MARK_COLS);
    let mark = agent_mark_identity(app, agent_id, label, theme);
    let lead = if *folded { "▸ " } else { "▾ " };
    let mut spans = vec![Span::styled(
        lead.to_string(),
        theme.dimmed(theme.roles.gray),
    )];
    if mark.is_some() {
        // The cells the mark covers are painted blank so nothing shows through
        // around it: a picture sits above the text layer, not instead of it.
        spans.push(Span::styled(" ".repeat(mark_width), theme.base()));
        spans.push(Span::styled(" ", theme.base()));
    }
    let indent = if mark.is_some() { mark_width + 1 } else { 0 };
    spans.push(Span::styled(
        truncate_to_width(label, width.saturating_sub(2 + indent), "…"),
        theme.base().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(format!(" · {count}"), theme.muted()));
    if *current {
        spans.push(Span::styled(
            format!("  {}", strings.runtime_current()),
            Style::default().fg(theme.roles.accent_user),
        ));
    }
    if *unavailable > 0 {
        spans.push(Span::styled(
            format!("  {} {unavailable}", strings.runtime_unavailable()),
            theme.dimmed(theme.roles.gray_dim),
        ));
    }
    (selected_line(spans, selected, width, theme), mark)
}

/// The Agent mark an identity wants drawn, when there is one to want.
///
/// Two conditions, and neither is the caller's to answer: the terminal has to be
/// able to hold a picture, and the identity has to name an Agent with a mark.
/// `None` is the caller's signal to keep the Agent's name, which is always
/// known — a mark that resolved to four pixels of half block would be a worse
/// answer than the word it replaced, and a themed mark with no theme colour has
/// nothing to draw in.
fn drawable_agent_mark(identity: String, pictures: bool, theme: &TuiTheme) -> Option<String> {
    if !pictures {
        return None;
    }
    vibex_brands::agent_mark(&identity)
        .filter(|mark| {
            !mark.uses_current_color || crate::theme::color_rgb(theme.roles.foreground).is_some()
        })
        .map(|_| identity)
}

/// The Agent mark a catalogue row wants drawn, when there is one to want.
fn agent_mark_identity(
    app: &App,
    agent_id: &vibex_core::AgentId,
    label: &str,
    theme: &TuiTheme,
) -> Option<String> {
    drawable_agent_mark(
        app.agent_identity(agent_id, label),
        app.terminal_images.supports_pixels(),
        theme,
    )
}

/// One entry: its account and model, and what it says about itself.
///
/// Reports the Agent mark the row reserved cells for, and the column it starts
/// at: a pinned row says which Agent it came from with the mark rather than with
/// the name, the way the heading it no longer sits under would have.
///
/// `auth_column` is the account column the list settled on, so that every row of
/// a group starts its model at the same offset.
#[allow(clippy::too_many_arguments)]
fn runtime_picker_entry_line(
    app: &App,
    options: &[vibex_core::SessionRuntimeOption],
    row: &RuntimePickerRow,
    selected: bool,
    in_group: bool,
    width: usize,
    auth_column: usize,
    theme: &TuiTheme,
) -> (Line<'static>, Option<(u16, String)>) {
    let strings = app.strings;
    let RuntimePickerRow::Entry { index, quick, .. } = row else {
        return (Line::from(""), None);
    };
    let Some(option) = options.get(*index) else {
        return (Line::from(""), None);
    };
    let current = app.runtime_option_is_current(option);
    let status = if current {
        Some(strings.runtime_current().to_string())
    } else {
        app.runtime_entry_status(option)
            .label(&strings)
            .map(str::to_string)
    };
    let marker = match quick {
        Some(digit) => digit.to_string(),
        None if current => "●".to_string(),
        None if app.runtime_prefs.is_favorite(option) => "★".to_string(),
        None => " ".to_string(),
    };
    let indent = if in_group { 4 } else { 2 };
    // A pinned row names an entry out of its group, so it has to carry what the
    // heading above it would otherwise say: the Agent it belongs to — as the
    // heading says it, which is the mark when the terminal can hold one. Its run
    // options are not drawn here — three names and a settings summary do not fit
    // one row, and the reader who wants them is one `Tab` from the line under
    // the list, which the highlighted row already fills in.
    let mark = if in_group {
        None
    } else {
        agent_mark_identity(app, &option.selection.agent_id, &option.agent_label, theme)
    };
    // The cells the mark covers, plus the column that keeps it off the name
    // beside it. Counted from the text's own lead, so the two ends of the row
    // cannot drift apart.
    let mark_width = if mark.is_some() {
        usize::from(crate::row_marks::MARK_COLS) + 1
    } else {
        0
    };
    let status_width = status.as_deref().map(display_width).unwrap_or_default();
    let status_gap = if status_width > 0 { 2 } else { 0 };
    let available = width.saturating_sub(indent + 2 + mark_width + status_width + status_gap);
    // The model is what a reader scans for, so it takes what the account leaves
    // and the account truncates first: "Agent account/…" names the provenance,
    // but two rows of the same model on two accounts are still two rows.
    //
    // The account is a *column*: padded to the width the list measured for it,
    // so the model starts at the same offset on every row of a group. The model
    // is the row's last cell and keeps its ragged right edge — a list of names
    // that all stopped at the same column would be a narrower list for no one.
    let auth_label = runtime_option_auth_label(&strings, option);
    let body = if in_group || mark.is_some() {
        // The Agent has already been said — by the heading above a group's row,
        // by the mark on a pinned one — so the row spends its columns on the two
        // names that tell two entries of one Agent apart.
        let model_width = available.saturating_sub(auth_column + RUNTIME_COLUMN_SEPARATOR);
        format!(
            "{} · {}",
            fixed_column(&auth_label, auth_column),
            truncate_to_width(&option.model_label, model_width, "…"),
        )
    } else {
        let (agent_width, auth_width, model_width) = pinned_columns(available);
        format!(
            "{} · {} · {}",
            fixed_column(&option.agent_label, agent_width),
            fixed_column(&auth_label, auth_width),
            truncate_to_width(&option.model_label, model_width, "…"),
        )
    };
    let mut spans = vec![
        Span::styled(" ".repeat(indent), theme.base()),
        Span::styled(
            format!("{marker} "),
            if current || quick.is_some() {
                Style::default().fg(theme.roles.accent_user)
            } else {
                theme.dimmed(theme.roles.gray)
            },
        ),
    ];
    if mark.is_some() {
        // The cells the mark covers are painted blank so nothing shows through
        // around it: a picture sits above the text layer, not instead of it.
        spans.push(Span::styled(
            " ".repeat(usize::from(crate::row_marks::MARK_COLS)),
            theme.base(),
        ));
        spans.push(Span::styled(" ", theme.base()));
    }
    spans.push(Span::styled(
        pad_to_width(&body, available),
        if current { theme.base() } else { theme.muted() },
    ));
    if let Some(status) = status {
        spans.push(Span::styled(" ".repeat(status_gap), theme.base()));
        spans.push(Span::styled(
            status,
            if current {
                Style::default().fg(theme.roles.accent_user)
            } else {
                theme.dimmed(theme.roles.gray)
            },
        ));
    }
    // The mark sits after the row's own lead — the indent and the marker's two
    // cells — which is where the blank cells above were reserved.
    let mark = mark.map(|identity| ((indent + 2) as u16, identity));
    (selected_line(spans, selected, width, theme), mark)
}

/// The preview of the highlighted entry: how it would run, and where that is
/// changed. Drawn under the list rather than in it, so it cannot be mistaken for
/// a row, and centred: it answers for the row the cursor is on, and the middle
/// of the width under the list is where a caption of the list sits.
fn runtime_picker_preview(
    app: &App,
    option: Option<&vibex_core::SessionRuntimeOption>,
    width: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    let strings = app.strings;
    // A heading has no entry of its own, so the preview answers for the entry the
    // picker is on — which is exactly what `Tab` would show from there.
    let selection = match option {
        Some(option) => app.runtime_prefs.with_remembered_options(option),
        None => match app.picker_selection() {
            Some(selection) => selection,
            None => return Line::from(""),
        },
    };
    if app.run_options_for(&selection).is_empty() {
        return Line::from("");
    }
    let body = app
        .run_option_values(&selection)
        .unwrap_or_else(|| strings.runtime_default().to_string());
    // The values name themselves, and the words that introduced them cost the
    // row the room the last value needs on a narrow terminal.
    let text = truncate_to_width(
        &format!("{body} · {}", strings.runtime_summary_hint()),
        width,
        "…",
    );
    let air = width.saturating_sub(display_width(&text)) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(air)),
        Span::styled(text, theme.dimmed(theme.roles.gray)),
    ])
}

/// The run options of the entry the picker is on, each row showing what is in
/// effect: a value is sent as it is chosen, so there is no staged copy to mark.
fn render_runtime_options(
    frame: &mut Frame<'_>,
    content: Rect,
    app: &mut App,
    selected: usize,
    run_options: &[crate::app::RunOption],
    theme: &TuiTheme,
) {
    let strings = app.strings;
    // The caption names the entry the rows belong to: the reader may have picked
    // it a moment ago, and the page's own selection catches up a round trip
    // later. It carries the same facts the catalogue's own rows carry — whose
    // account serves the entry, and which model it is — with the Agent as the
    // mark those rows wear: "DeepSeek Harness · bai · model" is one name too
    // many under a mark that already says the first, and the account is the fact
    // the catalogue cannot leave out and this line was.
    let (agent, model) = app.picker_runtime_labels();
    let option = app
        .picker_selection()
        .and_then(|selection| app.runtime_option_for(&selection))
        .cloned();
    let auth = option
        .as_ref()
        .map(|option| runtime_option_auth_label(&strings, option))
        .unwrap_or_default();
    let mark = option.as_ref().and_then(|option| {
        agent_mark_identity(app, &option.selection.agent_id, &option.agent_label, theme)
    });
    let mut content = content;
    // Two rows: the caption, and the air under it. What the rows below are *of*
    // has to be read before them, and a caption that touches them reads as their
    // first row.
    if content.height > 2 {
        let facts = [auth, model]
            .into_iter()
            .filter(|fact| !fact.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let caption = if mark.is_some() || agent.is_empty() {
            facts
        } else if facts.is_empty() {
            agent
        } else {
            format!("{agent} · {facts}")
        };
        let mut spans = Vec::new();
        let mut lead = 0usize;
        if mark.is_some() {
            // The cells the mark covers are painted blank so nothing shows
            // through around it: a picture sits above the text layer, not
            // instead of it. One column of air parts it from the caption.
            spans.push(Span::styled(
                " ".repeat(usize::from(crate::row_marks::MARK_COLS)),
                theme.base(),
            ));
            spans.push(Span::styled(" ", theme.base()));
            lead = usize::from(crate::row_marks::MARK_COLS) + 1;
        }
        spans.push(Span::styled(
            truncate_to_width(
                &caption,
                usize::from(content.width).saturating_sub(lead),
                "…",
            ),
            theme.dimmed(theme.roles.gray),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                height: 1,
                ..content
            },
        );
        // The mark goes where the Agent's name would have started, which is what
        // "instead of the text" means for a picture: the same place, drawn, not
        // written.
        if let Some(identity) = mark {
            let rect = Rect {
                x: content.x,
                y: content.y,
                width: crate::row_marks::MARK_COLS,
                height: crate::row_marks::MARK_ROWS,
            };
            let color = crate::theme::color_rgb(theme.roles.foreground);
            let _ = app.row_marks.show(
                &app.terminal_images,
                &crate::row_marks::Mark::Agent(identity),
                color,
                rect,
            );
        }
        // The air under the caption: it is what the rows below are *of*, and the
        // two must not read as one list.
        content = Rect {
            y: content.y + 2,
            height: content.height - 2,
            ..content
        };
    }
    let width = usize::from(content.width);
    let mut lines = Vec::with_capacity(run_options.len());
    for (index, option) in run_options.iter().enumerate() {
        let set = option.is_explicit();
        let value = option.resolved_label(strings.runtime_default());
        let value_width = width.saturating_sub(2 + 18);
        let spans = vec![
            Span::styled(
                if set { "● " } else { "  " }.to_string(),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                // One column short, so a label exactly as wide as the column
                // still keeps its space before the value.
                format!("{:<18}", truncate_to_width(&option.label, 17, "…")),
                theme.base(),
            ),
            Span::styled(
                pad_to_width(&truncate_to_width(&value, value_width, "…"), value_width),
                if set { theme.base() } else { theme.muted() },
            ),
        ];
        lines.push(selected_line(spans, index == selected, width, theme));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)),
        Rect {
            height: content
                .height
                .min(u16::try_from(run_options.len()).unwrap_or(u16::MAX)),
            ..content
        },
    );
}

/// Finish one row: highlight it across the full width when it is the cursor's.
fn selected_line(
    spans: Vec<Span<'static>>,
    selected: bool,
    width: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    if !selected {
        return Line::from(spans);
    }
    let used = spans
        .iter()
        .map(|span| display_width(&span.content))
        .sum::<usize>();
    let mut spans = spans;
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        theme.selected(),
    ));
    Line::from(spans).style(theme.selected())
}

/// The columns one of a group's rows spends before its names: the indent under
/// its Agent's heading, and the two-cell marker.
const RUNTIME_ENTRY_LEAD: usize = 6;

/// The columns the " · " between two names of a catalogue row takes.
const RUNTIME_COLUMN_SEPARATOR: usize = 3;

/// Split the width left for "Agent · account · model" between the three names.
///
/// Three names in one row is a squeeze, so the model keeps the largest share and
/// the account the smallest: two rows of the same model on two accounts are
/// still two rows, but the model is what a reader scans for and the account is
/// what they check second.
fn pinned_columns(available: usize) -> (usize, usize, usize) {
    const SEPARATORS: usize = 6;
    if available <= SEPARATORS {
        return (0, 0, available);
    }
    let body = available - SEPARATORS;
    let model = (body * 4 / 9).max(6).min(body);
    let rest = body - model;
    let agent = (rest * 3 / 5).max(4).min(rest);
    (agent, rest - agent, model)
}

/// One cell of a catalogue row: the value cut to its column and the column
/// filled either way.
///
/// A row that only cut its value would leave the column beside it starting
/// wherever that value happened to end — which is what makes a list of accounts
/// read as a sentence rather than as a column.
fn fixed_column(value: &str, width: usize) -> String {
    pad_to_width(&truncate_to_width(value, width, "…"), width)
}

/// How many columns the catalogue's account names are given.
///
/// As wide as the widest account the list has to show, so one width serves every
/// row of it, and capped: the account is what a reader checks second, and a
/// Provider Profile with a paragraph for a name must not spend the room the
/// models are read in.
fn runtime_auth_column(
    strings: &Strings,
    options: &[vibex_core::SessionRuntimeOption],
    names: usize,
) -> usize {
    /// The widest an account column may grow, in columns.
    const CAP: usize = 18;
    /// The room the model keeps beside even the longest account.
    const MODEL_FLOOR: usize = 12;
    let widest = options
        .iter()
        .map(|option| display_width(&runtime_option_auth_label(strings, option)))
        .max()
        .unwrap_or_default();
    widest
        .min(CAP)
        .min(names.saturating_sub(RUNTIME_COLUMN_SEPARATOR + MODEL_FLOOR))
}

/// The scroll bar beside a list: a track with a thumb that says where the window
/// is, so a long catalogue is legible as long rather than as clipped.
fn render_scroll_indicator(
    frame: &mut Frame<'_>,
    area: Rect,
    offset: usize,
    total: usize,
    theme: &TuiTheme,
) {
    let bar = Rect {
        x: area.x + area.width - 1,
        width: 1,
        ..area
    };
    let track = usize::from(bar.height);
    if track == 0 || total <= track {
        return;
    }
    let thumb = (track * track / total).max(1).min(track);
    let travel = track - thumb;
    // The thumb's position, rounded to the nearest row; a catalogue with
    // nothing left to scroll has no travel to divide by.
    let span = total - track;
    let start = (offset * travel + span / 2)
        .checked_div(span)
        .unwrap_or(0)
        .min(travel);
    let lines = (0..track)
        .map(|row| {
            let style = if row >= start && row < start + thumb {
                Style::default().fg(theme.roles.accent_user)
            } else {
                theme.dimmed(theme.roles.gray_dim)
            };
            Line::from(Span::styled("│", style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), bar);
}

/// Build a modal's chrome, giving the margins back on a compact terminal.
fn modal_chrome<'a>(
    app: &App,
    title: &'a str,
    sizing: ModalSizing,
    hints: Vec<ModalHint>,
) -> ModalChrome<'a> {
    let sizing = if app.is_compact() {
        sizing.compact()
    } else {
        sizing
    };
    ModalChrome::new(title, sizing)
        .hints(hints)
        .close_hovered(app.hovered_modal_close)
}

/// Labels used by the status line when the seat failed to attach.
pub fn locked_seat_help(strings: Strings) -> String {
    format!(
        "{}\n\n{}",
        strings.runtime_locked_title(),
        strings.runtime_locked_body()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::GlyphTier;
    use crate::locale::Locale;

    fn strings() -> Strings {
        Strings::for_locale(crate::locale::Locale::En)
    }

    /// The elicitation card's footer is a row of keys, and each one is
    /// published on the cells it was drawn in: the card takes the composer's
    /// place, and its printed keys used to be the only inert ones on screen.
    #[test]
    fn the_elicitation_footer_names_the_cells_of_its_keys() {
        let strings = strings();
        let theme = TuiTheme::resolve(
            Some("vibex-dark"),
            vibex_ui::GpuiThemeMode::Dark,
            crate::theme::ColorCapability {
                mode: crate::theme::ColorMode::TrueColor,
                glyphs: crate::theme::GlyphMode::Unicode,
            },
        );
        let form = crate::app::ElicitationForm {
            request: vibex_core::ElicitationRequest {
                id: vibex_core::RequestId::new(),
                session_id: vibex_core::VibexSessionId::new(),
                provider_request_id: None,
                tool_call_id: None,
                message: "Which one?".into(),
                title: None,
                description: None,
                fields: vec![vibex_core::ElicitationField {
                    id: "choice".into(),
                    title: "Choice".into(),
                    description: None,
                    required: true,
                    kind: vibex_core::ElicitationFieldKind::Text {
                        min_length: None,
                        max_length: None,
                        pattern: None,
                        format: None,
                        default: None,
                        options: vec![
                            vibex_core::ElicitationOption {
                                value: "a".into(),
                                title: "A".into(),
                                description: None,
                            },
                            vibex_core::ElicitationOption {
                                value: "b".into(),
                                title: "B".into(),
                                description: None,
                            },
                        ],
                    },
                }],
                status: vibex_core::ElicitationRequestStatus::Pending,
                requested_at_ms: 0,
            },
            field: 0,
            choice: 0,
        };
        let (lines, keys) = elicitation_hints(&form, strings, &theme);
        assert!(!keys.is_empty(), "the footer names no keys");
        let text = lines
            .last()
            .expect("the footer has its legend row")
            .to_string();
        for (line, column, width, key) in &keys {
            assert_eq!(*line, 1, "every key is on the legend row");
            // The columns are the cells the key occupies in the drawn line.
            let cell = text
                .chars()
                .skip(*column as usize)
                .take(*width as usize)
                .collect::<String>();
            assert!(
                Chord::from_hint(&cell).is_some(),
                "{cell:?} at column {column} is not a key, and {key} was reported there"
            );
        }
    }

    /// A narrow Provider table drops whole columns but never the state one.
    #[test]
    fn narrow_provider_tables_drop_columns_and_keep_state() {
        use crate::config_center::ConfigSection;
        assert_eq!(
            visible_column_indices(ConfigSection::Providers, 120, 4),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            visible_column_indices(ConfigSection::Providers, 99, 4),
            vec![0, 1, 2],
            "below 100 columns the Agent column goes first"
        );
        assert_eq!(
            visible_column_indices(ConfigSection::Providers, 79, 4),
            vec![2],
            "below 80 columns only the state column survives"
        );
        assert_eq!(
            visible_column_indices(ConfigSection::Agents, 60, 5),
            vec![0, 1, 2, 3, 4],
            "other sections keep the columns their headings name"
        );
    }

    /// One session, for the row builders that need something to describe.
    fn a_session(id: &str, title: &str) -> vibex_core::AgentSession {
        vibex_core::AgentSession {
            id: vibex_core::VibexSessionId::parse(id).expect("valid session id"),
            title: title.to_string(),
            project_id: vibex_core::ProjectId::parse("project_view0001").expect("valid project id"),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/repo/view".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 10,
            updated_at_ms: 20,
            last_message_at_ms: 0,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
            transport: vibex_core::SessionTransport::Acp,
            terminal: None,
        }
    }

    #[test]
    fn layout_never_exceeds_the_terminal() {
        for columns in [40u16, 60, 88, 119, 120, 160, 240] {
            let plan = layout_for(crate::app::shell_for_columns(columns), columns, 40);
            let used = usize::from(columns);
            let total = usize::from(plan.show_sidebar) * plan.sidebar_width
                + plan.main_width
                + usize::from(plan.show_details) * plan.details_width;
            assert!(
                total <= used,
                "{columns} columns produced a {total}-column layout"
            );
            assert!(plan.main_width >= 8);
        }
    }

    #[test]
    fn compact_layout_hides_the_side_panes() {
        let plan = layout_for(ShellKind::Compact, 70, 24);
        assert!(!plan.show_sidebar);
        assert!(!plan.show_details);
        assert_eq!(plan.main_width, 70);
    }

    #[test]
    fn wide_layout_is_the_only_one_with_a_details_pane() {
        let wide = layout_for(ShellKind::Wide, 160, 40);
        let medium = layout_for(ShellKind::Medium, 100, 40);
        assert!(wide.show_details);
        assert!(!medium.show_details);
    }

    #[test]
    fn palette_ranks_prefix_matches_first() {
        let matches = palette_matches("he", strings());
        assert!(!matches.is_empty());
        assert!(matches[0].label(strings()).to_lowercase().starts_with("he"));
    }

    #[test]
    fn palette_is_empty_for_nonsense() {
        assert!(palette_matches("zzzzzz", strings()).is_empty());
    }

    #[test]
    fn palette_finds_destinations_by_localised_name() {
        let chinese = palette_matches("设置", Strings::for_locale(Locale::ZhCn));
        assert!(
            chinese
                .iter()
                .any(|entry| entry.intent == Intent::OpenSettings),
            "localised palette lookup failed"
        );
    }

    #[test]
    fn palette_names_follow_the_reader_s_language() {
        let english = Strings::for_locale(Locale::En);
        let chinese = Strings::for_locale(Locale::ZhCn);
        let traditional = Strings::for_locale(Locale::ZhTw);
        // An English name still finds its command in a Chinese interface, and
        // a Chinese one does not have to be translated in the reader's head.
        assert!(
            palette_matches("settings", chinese)
                .iter()
                .any(|entry| entry.intent == Intent::OpenSettings),
            "an English name stopped finding its command"
        );
        for entry in PALETTE {
            assert!(!entry.label(english).is_empty());
            assert!(!entry.hint(english).is_empty());
            // Every command carries its own copy rather than falling back to
            // the key bar's English label.
            assert_ne!(
                entry.label(chinese),
                entry.label(english),
                "{:?} has no zh-CN name",
                entry.intent
            );
            assert_ne!(
                entry.label(traditional),
                entry.label(english),
                "{:?} has no zh-TW name",
                entry.intent
            );
        }
    }

    #[test]
    fn palette_does_not_duplicate_destinations() {
        let matches = palette_matches("", strings());
        let mut seen = std::collections::BTreeSet::new();
        for entry in &matches {
            assert!(seen.insert(entry.intent), "duplicate {:?}", entry.intent);
        }
    }

    #[test]
    fn every_palette_entry_maps_to_a_real_intent() {
        for entry in PALETTE {
            // The intent must round-trip through its stable id.
            assert_eq!(Intent::from_id(entry.intent.id()), Some(entry.intent));
        }
    }

    #[test]
    fn modal_rects_stay_inside_the_frame() {
        let area = Rect::new(0, 0, 80, 24);
        let rect = modal::dimensions(area, ModalSizing::document());
        assert!(rect.right() <= area.right());
        assert!(rect.bottom() <= area.bottom());
    }

    #[test]
    fn locked_seat_help_offers_every_way_out() {
        let help = locked_seat_help(strings());
        // The switch that serves this client is a General setting now, not a
        // Remote Access method, so the help has to send the reader there.
        assert!(help.contains("Settings → General"));
        assert!(help.contains("Vibex TUI"));
        assert!(help.contains("vibex connect"));
    }

    #[test]
    fn transcript_width_leaves_room_for_the_frame() {
        let width = transcript_width(ShellKind::Wide, 160);
        assert!(width > 40 && width < 160);
    }

    #[test]
    fn a_long_path_folds_its_folders_to_initials_before_dropping_its_head() {
        // The tail — the directory the session works in — is the part that
        // identifies the path, so it stays whole while its ancestors fold to one
        // letter each. `/h/d/c/v/c/vibex-tui` still says how deep the reader is.
        assert_eq!(
            compact_path("/home/dev/code/vibex/crates/vibex-tui", 24),
            "/h/d/c/v/c/vibex-tui"
        );
        // A path that already fits is never touched.
        assert_eq!(compact_path("/tmp/work", 24), "/tmp/work");
        // Cheap to compute even when it does not help: the caller's budget is
        // what decides, not the path's shape.
        assert_eq!(compact_path("vibex-tui", 4), "vibex-tui");
    }

    #[test]
    fn a_path_too_long_even_for_initials_falls_back_to_its_tail() {
        let compact = compact_path("/home/dev/code/vibex/crates/vibex-tui", 12);
        assert!(display_width(&compact) <= 12, "{compact}");
        assert!(compact.starts_with('…'), "{compact}");
        assert!(compact.ends_with("vibex-tui"), "{compact}");
    }

    #[test]
    fn a_windows_path_folds_on_its_own_separator() {
        // The runtime's working directory may be a Windows path, and the status
        // band must not print it whole just because the separator is a `\`. The
        // volume is not a folder, so its colon survives the fold.
        assert_eq!(
            compact_path(r"C:\Users\dev\code\vibex\crates\vibex-tui", 24),
            r"C:\U\d\c\v\c\vibex-tui"
        );
    }

    #[test]
    fn transcript_width_is_the_band_the_frame_actually_lays_out() {
        // The helper and the composition must agree, or a block measures itself
        // against a width other than the one it is drawn in.
        for (columns, rows) in [(60u16, 24u16), (100, 30), (160, 40), (240, 50)] {
            let bands = crate::layout::compute(
                Rect::new(0, 0, columns, rows),
                crate::layout::BandRequest {
                    turn_status: 1,
                    prompt: 4,
                    prompt_gap: 1,
                    shortcuts: 1,
                    ..crate::layout::BandRequest::default()
                },
            );
            assert_eq!(
                transcript_width(ShellKind::Wide, columns),
                usize::from(bands.scrollback.width),
                "at {columns} columns the helper and the band disagree"
            );
        }
    }

    /// The prompt band must wrap the draft at the width the composer paints in.
    ///
    /// The band is sized from `prompt_height`, which measures the draft itself,
    /// and the composer then paints into the rect the band produced. When the
    /// two disagreed by the padding columns, a draft that wrapped in the
    /// painter did not wrap in the measurer: the band stayed one row short, the
    /// renderer scrolled to keep the caret visible, and the *first* row of a
    /// two-row draft was pushed out of a box that had no room for it. Typing
    /// on until the band grew brought it back — the reader's report was a line
    /// that vanished and then reappeared.
    #[test]
    fn the_prompt_band_wraps_the_draft_at_the_width_the_composer_paints() {
        // The composer's own inset, spelled the way `render_composer` does it:
        // one edge column, one padding column, on each side of the text.
        fn painted_width(band: u16) -> usize {
            usize::from(band)
                .saturating_sub(usize::from(BOX_EDGE_COLUMNS + 2))
                .saturating_sub(crate::glyphs::PROMPT_ARROW_WIDTH)
        }
        for columns in [40u16, 60, 80, 100, 120, 160] {
            // A draft of exactly the painted width wraps to one row; one more
            // character makes it two. The band has to grow on the same input.
            let painted = painted_width(columns);
            let draft = "x".repeat(painted + 1);
            let mut app = App::new(
                vibex_backend::DisconnectedBackend::facade(),
                crate::app::AppOptions::default(),
            );
            app.terminal_images = crate::terminal_image::ImageSupport::forced(
                crate::terminal_image::ImageProtocol::None,
            );
            app.resize(columns, 40);
            app.navigate_to(Page::Agent);
            app.composer.set_text(draft.clone());

            let bands = crate::layout::compute(
                Rect::new(0, 0, columns, 40),
                crate::layout::BandRequest {
                    prompt: prompt_height(&app, columns),
                    prompt_gap: 1,
                    ..crate::layout::BandRequest::default()
                },
            );
            let band = bands.prompt;
            // The rows the painter would draw, against the rows the band left
            // inside its padding and context line.
            let rows = app.composer.display_row_count(painted_width(band.width));
            let room = usize::from(band.height).saturating_sub(3);
            assert_eq!(
                rows,
                room,
                "at {columns} columns the band has {room} draft rows for a draft \
                 that wraps to {rows} in the {}-column painter",
                painted_width(band.width)
            );
        }
    }

    /// Type a two-line draft a character at a time and never lose a line.
    ///
    /// The band is measured from the draft, then the draft is painted into the
    /// rect that measurement produced. Typing one character can change both at
    /// once — a wrap starts a new display row *and* grows the band — so the
    /// two have to be checked on every keystroke, not on the settled text.
    #[test]
    fn every_keystroke_of_a_two_line_draft_keeps_both_lines_on_screen() {
        for columns in [40u16, 61, 80, 100, 137] {
            let mut app = App::new(
                vibex_backend::DisconnectedBackend::facade(),
                crate::app::AppOptions::default(),
            );
            app.terminal_images = crate::terminal_image::ImageSupport::forced(
                crate::terminal_image::ImageProtocol::None,
            );
            app.resize(columns, 40);
            app.navigate_to(Page::Agent);
            app.composer.set_text(String::new());

            // The first line is long enough to wrap, which is where the band
            // and the painter disagree about how many rows the draft takes.
            let first = "a".repeat(usize::from(columns).saturating_mul(2));
            let mut typed = String::new();
            for character in first
                .chars()
                .chain(std::iter::once('\n'))
                .chain("bc".chars())
            {
                typed.push(character);
                app.composer.set_text(typed.clone());
                app.composer.move_to_end();

                let backend = ratatui::backend::TestBackend::new(columns, 40);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal
                    .draw(|frame| render(frame, &mut app))
                    .expect("frame draws");
                let buffer = terminal.backend().buffer().clone();
                let screen = (0..40)
                    .map(|row| {
                        (0..columns)
                            .map(|column| {
                                buffer
                                    .cell((column, row))
                                    .map(|cell| cell.symbol().to_string())
                                    .unwrap_or_default()
                            })
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                // The caret is on the last row the draft occupies, so that row
                // is on screen by construction. Every line before it has to be
                // too, or the box is showing a suffix of the draft.
                let painted = app
                    .regions
                    .composer
                    .expect("the composer publishes its rows");
                let painted_width =
                    usize::from(painted.width).saturating_sub(crate::glyphs::PROMPT_ARROW_WIDTH);
                let total = app.composer.display_row_count(painted_width);
                let visible = usize::from(painted.height).max(1);
                assert_eq!(
                    app.regions.composer_scroll,
                    0,
                    "at {columns} columns, after typing {} characters, a \
                     {total}-row draft was scrolled in a {visible}-row box\n{screen}",
                    typed.chars().count(),
                );
            }
        }
    }

    /// The session list reads a row's hour the way the desktop's sidebar does.
    ///
    /// Deliberately a clock rather than an age: the column answers "when", and
    /// a reader who has to subtract an age to get there has been given an
    /// arithmetic problem instead of an answer. Past a month even the day stops
    /// mattering and the date is what the row says.
    #[test]
    fn the_list_reads_a_session_hour_the_way_the_desktop_does() {
        let strings = Strings::for_locale(Locale::En);
        let now = vibex_core::unix_timestamp_ms();
        const DAY: i64 = 24 * 60 * 60 * 1_000;
        // `now` itself rather than an offset from it: the label compares
        // calendar dates, so "ninety minutes ago" is yesterday for the first
        // hour and a half of the day, and the assertions below would fail at
        // midnight rather than on a defect.
        let today = relative_time_label(now, now, strings);
        assert_eq!(today.len(), 5, "today is not a wall clock: {today:?}");
        assert_eq!(&today[2..3], ":", "today is not a wall clock: {today:?}");
        assert_eq!(
            relative_time_label(now - DAY, now, strings),
            strings.sidebar_yesterday(),
            "yesterday is not named"
        );
        assert_eq!(
            relative_time_label(now - 3 * DAY, now, strings),
            format!("3{}", strings.time_days_ago())
        );
        assert_eq!(
            relative_time_label(now - 10 * DAY, now, strings),
            format!("1{}", strings.time_weeks_ago())
        );
        let old = relative_time_label(now - 40 * DAY, now, strings);
        assert_eq!(old.len(), 5, "an old session is not a date: {old:?}");
        assert_eq!(&old[2..3], "/", "an old session is not a date: {old:?}");
        // A clock that moved backwards is not news, and a blank column would be
        // worse than the hour the runtime reported.
        assert_eq!(
            relative_time_label(now + DAY, now, strings),
            relative_time_label(now, now, strings),
            "a future timestamp is not read as today"
        );
    }

    /// The trailing column of a session row is the desktop's, mark for mark:
    /// the ring of a session still working, the question mark of one parked on
    /// the reader, the clock of one waiting on a send, the dot of one with
    /// something new, and the hour for everything quiet.
    #[test]
    fn a_session_row_says_its_state_in_the_desktop_s_own_marks() {
        use vibex_core::AgentSessionState;
        let strings = Strings::for_locale(Locale::En);
        let theme = TuiTheme::resolve(
            Some("vibex-dark"),
            vibex_ui::GpuiThemeMode::Dark,
            crate::theme::ColorCapability {
                mode: crate::theme::ColorMode::TrueColor,
                glyphs: crate::theme::GlyphMode::Unicode,
            },
        );
        let mut app = App::new(
            vibex_backend::DisconnectedBackend::facade(),
            crate::app::AppOptions::default(),
        );
        // No picture protocol, whatever terminal the runner happens to be in: a
        // console draws the character, and a test that let the environment
        // decide would pass or fail on where it was run.
        app.terminal_images =
            crate::terminal_image::ImageSupport::forced(crate::terminal_image::ImageProtocol::None);
        let now = vibex_core::unix_timestamp_ms();
        let mut session = a_session("session_status0001", "quiet");
        session.last_message_at_ms = now;
        /// The row the column is built for.
        fn a_row(
            session: &vibex_core::AgentSession,
            state: AgentSessionState,
        ) -> vibex_desktop_model::AgentSidebarRow {
            vibex_desktop_model::AgentSidebarRow {
                id: format!("session:{}", session.id.as_str()),
                kind: vibex_desktop_model::AgentSidebarRowKind::Session,
                project_id: session.project_id.as_str().to_string(),
                workspace_id: session.workspace_id.as_str().to_string(),
                session_id: Some(session.id.clone()),
                label: session.title.clone(),
                depth: 1,
                pinned: false,
                selected: false,
                collapsed: false,
                state: Some(state),
                delegated: false,
                child_count: 0,
                parent_id: None,
            }
        }
        /// The status column one state draws, as text.
        fn status_text(
            app: &App,
            theme: &TuiTheme,
            strings: Strings,
            session: &vibex_core::AgentSession,
            state: AgentSessionState,
            pinned: bool,
            now: i64,
        ) -> String {
            let mut described = session.clone();
            described.state = state;
            let mut row = a_row(session, state);
            row.pinned = pinned;
            let column = session_status_column(&row, Some(&described), app, theme, strings, now);
            column
                .spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        }
        macro_rules! text_of {
            ($state:expr) => {
                status_text(&app, &theme, strings, &session, $state, false, now)
            };
        }
        // A quiet session says when it last spoke, and nothing else.
        let idle = text_of!(AgentSessionState::Idle);
        assert_eq!(idle.len(), 5, "a quiet row is not its hour: {idle:?}");
        // A working one turns the ring instead, because the hour of a session
        // in the middle of a turn is not the question its row is asking.
        let running = text_of!(AgentSessionState::Running);
        assert!(
            crate::glyphs::activity_ring_frames(GlyphTier::Full).contains(&running.as_str()),
            "a working row does not turn the ring: {running:?}"
        );
        // A session parked on the reader asks, and the question outranks the
        // clock. The desktop's own icons reserve the two cells a picture needs,
        // so the character is followed by the air the row would have drawn the
        // mark over.
        let slot = |icon: crate::sidebar_icons::Icon| {
            format!(
                "{:<width$}",
                icon.glyph(GlyphTier::Full),
                width = usize::from(crate::row_marks::MARK_COLS)
            )
        };
        assert_eq!(
            text_of!(AgentSessionState::NeedsInput),
            slot(crate::sidebar_icons::Icon::Attention)
        );
        // A failure is the danger mark, and the row keeps no hour beside it.
        assert_eq!(
            text_of!(AgentSessionState::Error),
            crate::glyphs::state_marker("failed", GlyphTier::Full).to_string()
        );
        // A session still starting up wears the ring and says `INIT` beside it:
        // the Agent has been asked and has not answered, and the word is what
        // separates that from a turn that is genuinely under way.
        assert_eq!(
            text_of!(AgentSessionState::Initializing),
            format!(
                "{} {}",
                crate::glyphs::activity_ring_frames(GlyphTier::Full)[0],
                strings.sidebar_state_initializing()
            ),
            "a starting session does not say so"
        );
        // Something new is a dot beside whatever the row already said, and the
        // pin owns the column's trailing slot.
        app.unread_sessions.insert(session.id.as_str().to_string());
        let unread = text_of!(AgentSessionState::Idle);
        assert!(
            unread.ends_with(crate::glyphs::unread_marker(GlyphTier::Full)),
            "an unread row is not dotted: {unread:?}"
        );
        app.unread_sessions.clear();
        let pinned_text = status_text(
            &app,
            &theme,
            strings,
            &session,
            AgentSessionState::Idle,
            true,
            now,
        );
        assert_eq!(
            pinned_text,
            slot(crate::sidebar_icons::Icon::Pin),
            "a pinned row does not end in its pin: {pinned_text:?}"
        );
    }

    /// The branch a delegated row hangs from is drawn in the same columns for
    /// every terminal: an elbow under the owning session's Agent mark, air
    /// between that elbow and the row's own mark, and a trunk per level that
    /// still has a sibling to reach.
    #[test]
    fn a_delegated_row_draws_its_branch_in_cells_of_one_width() {
        let full = GlyphTier::Full;
        // A row one level down: the elbow lands in its indent column and the
        // cell past it is air, so the branch stops short of the row's mark.
        let one = SESSION_ROW_INDENT;
        let (title, below) = branch_lead(&[false], one, full);
        assert_eq!(title, "  └ ", "{title:?}");
        // The last child of a branch stops there: the second line carries no
        // trunk, because nothing follows.
        assert_eq!(below, "    ", "{below:?}");
        // A row with a sibling left runs its trunk past both lines.
        let (title, below) = branch_lead(&[true], one, full);
        assert_eq!(title, "  ├ ", "{title:?}");
        assert_eq!(below, "  │ ", "{below:?}");
        // A helper under that worker: the owner's own column stands outside the
        // worker's elbow, carrying the owner's branch on to the worker's
        // siblings, while the elbow lands under the worker's mark.
        let indent = SESSION_ROW_INDENT * 3;
        let (title, below) = branch_lead(&[false, true], indent, full);
        assert_eq!(title, "    │ └ ", "{title:?}");
        assert_eq!(below, "    │   ", "{below:?}");
        // Both runs are as wide as the lead a root row spends, whatever the
        // depth: the Agent marks of a list line up.
        for shape in [vec![false], vec![true], vec![false, true], vec![true, true]] {
            let depth = shape.len() as u8 + 1;
            let indent = usize::from(depth) * SESSION_ROW_INDENT;
            let (title, below) = branch_lead(&shape, indent, full);
            assert_eq!(title.chars().count(), indent + 2, "{title:?}");
            assert_eq!(below.chars().count(), indent + 2, "{below:?}");
        }
        // The legacy tier falls back to characters a console font has.
        let legacy = GlyphTier::Legacy;
        let (title, _) = branch_lead(&[false], one, legacy);
        assert_eq!(title, "  ` ", "{title:?}");
    }
}
