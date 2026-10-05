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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use vibex_ui::shell::ShellKind;

use crate::action::Intent;
use crate::app::{
    App, Availability, BannerTone, ComposerMode, ManagementRow, Overlay, Page, ToastTone,
};
use crate::keymap::Scope;
use crate::layout::Bands;
use crate::locale::Strings;
use crate::modal::{self, ModalChrome, ModalHint, ModalSizing};
use crate::runtime_picker::RuntimePickerRow;
use crate::text::{display_width, pad_to_width, truncate_to_width};
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

/// A palette entry: a label plus the intent it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    pub label: &'static str,
    pub hint: &'static str,
    pub intent: Intent,
}

/// Every command the palette can run.
pub const PALETTE: &[PaletteEntry] = &[
    PaletteEntry {
        label: "Sessions",
        hint: "Open the session list",
        intent: Intent::GotoSessions,
    },
    PaletteEntry {
        label: "Management",
        hint: "Open the management index",
        intent: Intent::GotoManagement,
    },
    PaletteEntry {
        label: "Usage",
        hint: "Show token usage",
        intent: Intent::GotoUsage,
    },
    PaletteEntry {
        label: "Settings",
        hint: "Theme, language, keys",
        intent: Intent::OpenSettings,
    },
    PaletteEntry {
        label: "Help",
        hint: "Contextual key help",
        intent: Intent::ToggleHelp,
    },
    PaletteEntry {
        label: "New session",
        hint: "Create a session",
        intent: Intent::NewSession,
    },
    PaletteEntry {
        label: "Rename session",
        hint: "Rename the open session",
        intent: Intent::BeginRenameSession,
    },
    PaletteEntry {
        label: "Fork session",
        hint: "Copy the session",
        intent: Intent::ForkSession,
    },
    PaletteEntry {
        label: "Archive session",
        hint: "Archive the open session",
        intent: Intent::ArchiveSession,
    },
    PaletteEntry {
        label: "Delete session",
        hint: "Delete the open session",
        intent: Intent::DeleteSession,
    },
    PaletteEntry {
        label: "Switch runtime",
        hint: "Pick the Agent runtime and model",
        intent: Intent::SwitchAgentRuntime,
    },
    PaletteEntry {
        label: "Probe runtimes",
        hint: "Re-discover available Agents",
        intent: Intent::ProbeAgentRuntime,
    },
    PaletteEntry {
        label: "Devices",
        hint: "Pair and revoke devices",
        intent: Intent::OpenManagementSection,
    },
    PaletteEntry {
        label: "Pair a device",
        hint: "Issue a one-time pairing code",
        intent: Intent::CreatePairingCode,
    },
    PaletteEntry {
        label: "Reload key bindings",
        hint: "Re-read tui-keys.toml",
        intent: Intent::ReloadKeymap,
    },
    PaletteEntry {
        label: "Refresh",
        hint: "Re-read the current page",
        intent: Intent::Refresh,
    },
    PaletteEntry {
        label: "Quit",
        hint: "Leave the TUI",
        intent: Intent::RequestQuit,
    },
];

/// The heading a palette command is filed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PaletteGroup {
    Recent,
    Session,
    Workbench,
    Management,
    Device,
    View,
    App,
}

impl PaletteGroup {
    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            PaletteGroup::Recent => strings.palette_group_recent(),
            PaletteGroup::Session => strings.palette_group_session(),
            PaletteGroup::Workbench => strings.palette_group_workbench(),
            PaletteGroup::Management => strings.palette_group_management(),
            PaletteGroup::Device => strings.palette_group_device(),
            PaletteGroup::View => strings.palette_group_view(),
            PaletteGroup::App => strings.palette_group_app(),
        }
    }
}

/// Which heading a command belongs under.
pub const fn palette_group(intent: Intent) -> PaletteGroup {
    match intent {
        Intent::NewSession
        | Intent::BeginRenameSession
        | Intent::ForkSession
        | Intent::ArchiveSession
        | Intent::DeleteSession
        | Intent::SwitchWorkspace
        | Intent::OpenSelectedSession
        | Intent::EnterSession => PaletteGroup::Session,
        Intent::OpenBlockDetails
        | Intent::CopyBlockBody
        | Intent::CopyBlockMetadata
        | Intent::ToggleBlockExpanded
        | Intent::BeginTranscriptSearch => PaletteGroup::Workbench,
        Intent::OpenManagementSection
        | Intent::InstallOrUpdateAgent
        | Intent::UninstallAgent
        | Intent::AgentAuthMenu
        | Intent::ToggleSelectedEntry
        | Intent::EditSelectedEntry
        | Intent::ReloadManagement
        | Intent::ProviderHealth => PaletteGroup::Management,
        Intent::CreatePairingCode | Intent::RevokeSelectedDevice | Intent::OpenDeviceAudit => {
            PaletteGroup::Device
        }
        Intent::GotoSessions
        | Intent::GotoManagement
        | Intent::GotoUsage
        | Intent::OpenSettings
        | Intent::ToggleHelp
        | Intent::ToggleSidebar
        | Intent::FocusNext
        | Intent::FocusPrevious => PaletteGroup::View,
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

/// Palette entries matching `query`, best first, with `recent` boosted.
pub fn palette_matches_recent(
    query: &str,
    strings: Strings,
    recent: &[String],
) -> Vec<PaletteEntry> {
    let needle = query.trim().to_lowercase();
    let mut entries = palette_matches(&needle, strings);
    if needle.is_empty() && !recent.is_empty() {
        // With nothing typed the palette opens on what was just used, which is
        // the whole point of remembering it.
        let mut recent_entries = recent
            .iter()
            .filter_map(|id| {
                let intent = Intent::from_id(id)?;
                PALETTE.iter().find(|entry| entry.intent == intent).copied()
            })
            .collect::<Vec<_>>();
        recent_entries.dedup_by_key(|entry| entry.intent);
        let seen = recent_entries
            .iter()
            .map(|entry| entry.intent)
            .collect::<Vec<_>>();
        recent_entries.extend(
            entries
                .into_iter()
                .filter(|entry| !seen.contains(&entry.intent)),
        );
        return recent_entries;
    }
    if !needle.is_empty() {
        // Recent use is a tie-break, not a filter: a command the reader has
        // used before rises among equally good matches.
        entries.sort_by_key(|entry| {
            let rank = recent
                .iter()
                .position(|id| id == entry.intent.id())
                .unwrap_or(usize::MAX);
            (
                palette_group(entry.intent),
                usize::from(rank == usize::MAX),
                rank,
                entry.label,
            )
        });
    }
    entries
}

/// Palette entries matching `query`.
pub fn palette_matches(query: &str, strings: Strings) -> Vec<PaletteEntry> {
    let needle = query.trim().to_lowercase();
    let mut scored = PALETTE
        .iter()
        .copied()
        .filter_map(|entry| {
            // The label is what the reader is typing at; the hint is what they
            // are reading, so a hit there scores lower but still counts.
            let label = fuzzy_score(entry.label, &needle);
            let hint = if needle.is_empty() {
                None
            } else {
                fuzzy_score(entry.hint, &needle)
            };
            let score = match (label, hint) {
                (Some(score), _) => score,
                (None, Some(score)) => score - 20,
                (None, None) => return None,
            };
            Some((score, entry))
        })
        .collect::<Vec<_>>();
    // Best first, then by group so the list reads in sections, then by label.
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| palette_group(left.1.intent).cmp(&palette_group(right.1.intent)))
            .then_with(|| left.1.label.cmp(right.1.label))
    });
    let mut entries = scored
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();
    // The palette also offers the localised destination names, so a user who
    // reads Chinese can find 用量 without knowing the English label.
    for (intent, label) in [
        (Intent::GotoSessions, strings.nav_sessions()),
        (Intent::GotoManagement, strings.nav_management()),
        (Intent::GotoUsage, strings.nav_usage()),
        (Intent::OpenSettings, strings.nav_settings()),
        (Intent::ToggleHelp, strings.nav_help()),
    ] {
        if (needle.is_empty() || label.to_lowercase().contains(&needle))
            && let Some(entry) = PALETTE.iter().find(|entry| entry.intent == intent)
            && !entries.iter().any(|candidate| candidate.intent == intent)
        {
            entries.push(*entry);
        }
    }
    entries
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
    let area = frame.area();
    app.shell = crate::app::shell_for_columns(area.width);
    let theme = app.theme.clone();
    let strings = app.strings;

    frame.render_widget(Block::default().style(theme.base()), area);

    // The composer's height depends on the width the layout gives it: a draft
    // that is one logical line can still wrap to several display rows. The
    // first pass measures the prompt band, the second sizes it.
    let mut request = band_request(app);
    request.prompt = composer_height(app, crate::layout::compute(area, request).prompt.width);
    let bands = crate::layout::compute(area, request);

    render_status_band(frame, bands.status, app, &theme, strings);
    if Bands::is_visible(bands.tasks) {
        render_tasks_band(frame, bands.tasks, app, &theme, strings);
    }
    if Bands::is_visible(bands.todo) {
        render_todo_band(frame, bands.todo, app, &theme, strings);
    }

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
        | Page::Usage
        | Page::Settings
        | Page::Help => render_management_view(frame, bands.scrollback, app, &theme, strings),
    }

    // The rail maps the transcript, so it belongs to the pages that show one.
    if Bands::is_visible(bands.gutter) && app.page.is_session_page() {
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

    if let Some(overlay) = app.overlay.clone() {
        render_overlay(frame, area, app, &overlay, &theme);
    }
}

/// What the frame wants on screen, given the current state.
fn band_request(app: &App) -> crate::layout::BandRequest {
    // Everything from the turn line down belongs to the session view. On the
    // session list the active session's plan and turn state are not what the
    // reader is looking at, and printing them above the list is how a global
    // page ends up wearing another page's chrome. The page where a session is
    // being written is the same case from the other side: it is a page with a
    // composer, not a session, so the session behind it — which the reader is
    // leaving — must not put its turn, plan or held queue on it.
    let session_view = app.page.is_session_page() && app.page_owns_session();
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
        // measured by [`composer_height`] once the band's width is known.
        prompt: 0,
        prompt_gap: u16::from(app.page.is_session_page()),
        // The pages a reader writes on spend their last row on the draft rather
        // than on a legend: the composer's own line already names the runtime,
        // and every key the bar would advertise is one `?` away. The pages that
        // are navigated rather than written on keep it.
        shortcuts: u16::from(!app.page.is_session_page()),
        status_line: 0,
    }
}

/// The composer band's height at `width` columns, borders included.
///
/// The draft is wrapped at the same width the renderer paints at, so a line
/// wider than the box grows the box rather than being clipped to its first row.
/// [`crate::layout::compute`] squeezes this back down on a short terminal —
/// the transcript's minimum rows are kept first — and the composer scrolls
/// under a shorter box instead of losing the caret.
fn composer_height(app: &App, width: u16) -> u16 {
    if !app.page.is_session_page() {
        return 0;
    }
    // The block's two border columns, then the prompt arrow's two.
    let text_width = usize::from(width)
        .saturating_sub(2)
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
    // is positioned. The header decision is made against a conservatively
    // small viewport so it cannot oscillate between two frames.
    let header = app.transcript.sticky_header(
        scroll,
        height.saturating_sub(
            crate::transcript::MAX_STICKY_ROWS + crate::transcript::STICKY_GAP_ROWS,
        ),
        theme,
        strings,
    );
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
            usize::from(end.1)
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
    // A terminal too short for chrome and a row keeps the rows instead: two
    // rows of chrome over two rows of list can only show a heading, and a
    // heading the reader cannot open is worth less than the session under it.
    if page.height < 5 {
        render_session_rows(frame, page, &list, app, theme, strings);
        return;
    }
    let list_area = Rect {
        y: page.y + 2,
        height: page.height - 2,
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
            y: page.y + 1,
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
/// controls that answer the page — where the next session opens, and how the
/// list is grouped — with their keys.
///
/// They are laid out from the right edge inwards and one that does not fit is
/// dropped whole rather than truncated: half a key is a key the reader cannot
/// use, and the action on the left is never the one that goes.
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
    if let Some(chord) = app.keymap.chord_for(Intent::ToggleSidebarGrouping) {
        let mode = if app.sidebar_grouped {
            strings.sidebar_grouped()
        } else {
            strings.sidebar_flat()
        };
        candidates.push((
            format!("{mode}  {}", chord.display()),
            Intent::ToggleSidebarGrouping,
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
    // The button is a button: a click runs the same intent the key does.
    app.regions.hints.push((
        Rect {
            width: label_width as u16,
            height: 1,
            ..area
        },
        Intent::NewSession,
    ));
    let padding = usize::from(area.width).saturating_sub(label_width + used);
    let mut spans = vec![
        Span::styled(new_session, Style::default().fg(theme.roles.text_secondary)),
        Span::raw(" ".repeat(padding)),
    ];
    let mut x = usize::from(area.x) + padding + label_width;
    for (index, (text, intent)) in chosen.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
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
        spans.push(Span::styled(text.clone(), theme.dimmed(theme.roles.gray)));
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
    let sessions = app.agent.state.sessions.value.clone().unwrap_or_default();
    let now_ms = vibex_core::unix_timestamp_ms();
    // When the session last said anything, right-aligned: the reader scanning
    // the list is looking for what changed, and a wall of identical rows says
    // nothing about that. The column is measured from what will be drawn, so a
    // list of sessions from today spends no width on days.
    let time_labels = rows
        .iter()
        .map(|row| {
            row.session_id
                .as_ref()
                .and_then(|session_id| {
                    sessions
                        .iter()
                        .find(|session| &session.id == session_id)
                        .map(|session| session.last_message_at_ms)
                })
                .map(|at_ms| relative_time_label(at_ms, now_ms, strings))
        })
        .collect::<Vec<_>>();
    let time_column = time_labels
        .iter()
        .flatten()
        .map(|label| display_width(label))
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) / 4);
    let mut items = Vec::with_capacity(rows.len());
    let mut heights = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let lines = session_list_item(
            row,
            list.counts.get(index).copied().unwrap_or(0),
            index == selected,
            app.hover == Some((Scope::Sessions, index)),
            time_labels.get(index).and_then(Option::as_ref),
            time_column,
            usize::from(area.width),
            &sessions,
            app,
            theme,
            strings,
        );
        heights.push(lines.len().min(usize::from(u16::MAX)) as u16);
        items.push(ListItem::new(Text::from(lines)));
    }
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(rows.len().saturating_sub(1))));
    frame.render_stateful_widget(List::new(items), area, &mut state);
    // The rows are not all one line tall, so the mouse needs to know which row
    // the drawn window starts on and how tall each row in it is.
    let first_row = state.offset().min(rows.len());
    let mut drawn = 0usize;
    let mut visible = Vec::new();
    for height in heights.iter().skip(first_row) {
        if drawn >= usize::from(area.height) {
            break;
        }
        let height = usize::from((*height).max(1));
        visible.push(height as u16);
        drawn += height;
    }
    app.regions.list = Some(crate::app::ListRegion {
        rect: area,
        scope: Scope::Sessions,
        rows: rows.len(),
        first_line: 0,
        first_row,
        heights: visible,
    });
}

/// One row of the list: its heading, its title line, its second line and any
/// detail card the reader has opened under it.
#[allow(clippy::too_many_arguments)]
fn session_list_item(
    row: &vibex_desktop_model::AgentSidebarRow,
    count: usize,
    selected: bool,
    hovered: bool,
    time: Option<&String>,
    time_column: usize,
    width: usize,
    sessions: &[vibex_core::AgentSession],
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) -> Vec<Line<'static>> {
    let session = row
        .session_id
        .as_ref()
        .and_then(|session_id| sessions.iter().find(|session| &session.id == session_id));
    if row.kind != vibex_desktop_model::AgentSidebarRowKind::Session {
        return vec![session_heading_line(
            row, count, selected, hovered, width, app, theme,
        )];
    }
    let mut lines = session_row_lines(
        row,
        session,
        selected,
        hovered,
        time,
        time_column,
        width,
        app,
        theme,
        strings,
    );
    if let Some(session) = session
        && app.session_card_expanded(session.id.as_str())
    {
        lines.extend(session_card_lines(app, session, strings, theme, width));
    }
    lines
}

/// A group's heading: what it is called, how much it holds, and a rule to the
/// edge of the list.
///
/// The rule is what turns a stack of rows into sections: without it a project
/// name is one more line of text, and the reader has to read it to find the
/// next one.
fn session_heading_line(
    row: &vibex_desktop_model::AgentSidebarRow,
    count: usize,
    selected: bool,
    hovered: bool,
    width: usize,
    app: &App,
    theme: &TuiTheme,
) -> Line<'static> {
    let tier = app.glyph_tier();
    let disclosure = crate::glyphs::disclosure(!row.collapsed, tier);
    let label_style = if selected {
        Style::default().fg(theme.roles.accent_user)
    } else if hovered {
        Style::default().fg(theme.roles.foreground)
    } else {
        Style::default().fg(theme.roles.gray_bright)
    }
    .add_modifier(Modifier::BOLD);
    let count_style = Style::default().fg(theme.roles.gray_dim);
    let indent = usize::from(row.depth) * 2;
    let count_label = if count > 0 {
        format!(" {count}")
    } else {
        String::new()
    };
    // The name gives way before the shape of the heading does: a heading whose
    // rule ran out is still a heading, but a heading without its name is not.
    let label_width = width
        .saturating_sub(indent)
        .saturating_sub(2)
        .saturating_sub(display_width(&count_label))
        .saturating_sub(3)
        .max(1);
    let label = truncate_to_width(&row.label, label_width, "…");
    let used = indent + 2 + display_width(&label) + display_width(&count_label);
    let mut spans = vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(format!("{disclosure} "), label_style),
        Span::styled(label, label_style),
    ];
    if !count_label.is_empty() {
        spans.push(Span::styled(count_label, count_style));
    }
    // One column of air between the count and the rule, then the rule to the
    // edge of the list, so the eye follows it to the next heading.
    let rule_width = width.saturating_sub(used + 1);
    if rule_width > 0 {
        spans.push(Span::styled(" ", count_style));
        spans.push(Span::styled(
            crate::glyphs::heading_rule(tier).repeat(rule_width),
            Style::default().fg(theme.roles.border),
        ));
    }
    Line::from(spans)
}

/// A session's row: the title line and, under it, what the session last did.
#[allow(clippy::too_many_arguments)]
fn session_row_lines(
    row: &vibex_desktop_model::AgentSidebarRow,
    session: Option<&vibex_core::AgentSession>,
    selected: bool,
    hovered: bool,
    time: Option<&String>,
    time_column: usize,
    width: usize,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) -> Vec<Line<'static>> {
    let tier = app.glyph_tier();
    // A selected or hovered row is a band rather than a coloured sentence: the
    // band covers the second line too, so the row reads as one object.
    let band = (selected || hovered).then_some(theme.roles.surface_highlight);
    let paint = |colour: ratatui::style::Color| match band {
        Some(band) => Style::default().fg(colour).bg(band),
        None => Style::default().fg(colour),
    };
    let title_style = paint(theme.roles.foreground);
    let dim_style = paint(if selected {
        theme.roles.text_secondary
    } else {
        theme.roles.gray_dim
    });
    let blank_style = match band {
        Some(band) => Style::default().bg(band),
        None => Style::default(),
    };
    let bar_style = Style::default()
        .fg(theme.roles.accent_user)
        .add_modifier(Modifier::BOLD);
    let bar_style = match band {
        Some(band) => bar_style.bg(band),
        None => bar_style,
    };
    let bar = if selected {
        crate::glyphs::accent_bar(tier)
    } else {
        " "
    };
    let state_mark = session.map(|session| {
        (
            session_state_mark(session.state, tier),
            state_marker_colour(session.state, theme),
        )
    });
    let icon_style = match (band, state_mark) {
        (Some(band), Some((_, colour))) => Style::default().fg(colour).bg(band),
        (None, Some((_, colour))) => Style::default().fg(colour),
        _ => title_style,
    };
    let icon = state_mark.map(|(mark, _)| mark).unwrap_or(" ");
    let badges = session_row_badges(row, session, selected, app, theme);
    let badge_width = spans_width(&badges);
    let badge_block = if badges.is_empty() {
        0
    } else {
        badge_width + 2
    };
    // The age column ends one cell short of the edge, so the numbers are not
    // glued to the frame.
    let meta_width = if time_column > 0 { time_column + 2 } else { 0 };
    let text_width = width
        .saturating_sub(4)
        .saturating_sub(meta_width)
        .saturating_sub(badge_block);
    let mut spans = vec![
        Span::styled(format!("{bar} "), bar_style),
        Span::styled(format!("{icon} "), icon_style),
    ];
    let title = truncate_to_width(&row.label, text_width, "…");
    let mut text_used = display_width(&title);
    spans.push(Span::styled(title, title_style));
    // Who is answering, when the title has left room for it: which Agent a
    // session runs on is how a reader tells two similarly named rows apart.
    let subtitle = session
        .map(|session| app.session_agent_label(session))
        .unwrap_or_default();
    if !subtitle.is_empty() && text_used + 3 + display_width(&subtitle) <= text_width {
        spans.push(Span::styled(" · ", dim_style));
        let subtitle = truncate_to_width(&subtitle, text_width - text_used - 3, "…");
        text_used += 3 + display_width(&subtitle);
        spans.push(Span::styled(subtitle, dim_style));
    }
    spans.push(Span::styled(
        " ".repeat(text_width.saturating_sub(text_used)),
        blank_style,
    ));
    if !badges.is_empty() {
        spans.push(Span::styled("  ", blank_style));
        spans.extend(badges);
    }
    if let Some(time) = time.filter(|_| time_column > 0) {
        let padding = time_column.saturating_sub(display_width(time));
        spans.push(Span::styled(
            format!(" {}{time}", " ".repeat(padding)),
            dim_style,
        ));
    }
    let mut lines = vec![Line::from(spans)];
    // The second line: what the session last did, or where it works when this
    // client has not seen it do anything yet. A session waiting on the reader
    // says so first — it is the one thing on the row that has to be acted on.
    if let Some(session) = session {
        let pending = session.state == vibex_core::AgentSessionState::NeedsInput;
        let prefix = if pending {
            format!("{}: ", strings.pending())
        } else {
            String::new()
        };
        let budget = width
            .saturating_sub(4)
            .saturating_sub(display_width(&prefix));
        let mut spans = vec![Span::styled(format!("{bar}   "), bar_style)];
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, paint(theme.roles.warning)));
        }
        let secondary = match app.session_echo(&session.id) {
            Some(echo) => truncate_to_width(echo, budget, "…"),
            None => {
                let root = compact_path(&session.workspace_root, budget);
                truncate_to_width(&root, budget, "…")
            }
        };
        spans.push(Span::styled(secondary, dim_style));
        lines.push(Line::from(spans));
    }
    lines
}

/// The marks a session row carries: pinned, unread, and whether the session is
/// going to continue itself.
fn session_row_badges(
    row: &vibex_desktop_model::AgentSidebarRow,
    session: Option<&vibex_core::AgentSession>,
    selected: bool,
    app: &App,
    theme: &TuiTheme,
) -> Vec<Span<'static>> {
    let tier = app.glyph_tier();
    let resting = |colour: ratatui::style::Color| Style::default().fg(colour);
    let mut spans = Vec::new();
    if row.pinned {
        spans.push(Span::styled(
            format!("{} ", crate::glyphs::pin_marker(tier)),
            resting(theme.roles.gray_bright),
        ));
    }
    if session.is_some_and(|session| app.session_is_unread(&session.id)) {
        spans.push(Span::styled(
            format!("{} ", crate::glyphs::unread_marker(tier)),
            resting(if selected {
                theme.roles.text_secondary
            } else {
                theme.roles.accent_attention
            }),
        ));
    }
    if let Some(session) = session
        && let Some(label) = auto_continue_label(app, &session.id)
    {
        let counting = app.auto_continue.countdown_seconds(&session.id).is_some();
        spans.push(Span::styled(
            format!("{label} "),
            resting(if selected {
                theme.roles.text_secondary
            } else if counting {
                theme.roles.accent_attention
            } else {
                theme.roles.accent_success
            }),
        ));
    }
    spans
}

/// The mark beside a session in the list: a shape per state family, so a
/// monochrome terminal can tell a failed session from a running one.
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

/// The auto-continue mark for one row: `↻` when the session will continue
/// itself, `↻3` while it counts down, and nothing when it will not.
fn auto_continue_label(app: &App, session_id: &vibex_core::VibexSessionId) -> Option<String> {
    if let Some(seconds) = app.auto_continue.countdown_seconds(session_id) {
        return Some(format!("↻{seconds}"));
    }
    app.auto_continue
        .is_enabled(session_id)
        .then(|| "↻".to_string())
}

/// The colour of a state's mark.
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

/// How long ago a session last said anything, the way a list of them reads.
///
/// Deliberately coarse: the list separates "just now" from "a while ago", and a
/// timestamp to the second would be a column of noise. Negative ages are clamped
/// rather than printed, because a clock that moved backwards is not news.
fn relative_time_label(at_ms: i64, now_ms: i64, strings: Strings) -> String {
    let elapsed_seconds = ((now_ms - at_ms) / 1_000).max(0);
    if elapsed_seconds < 60 {
        return strings.time_just_now().to_string();
    }
    let minutes = elapsed_seconds / 60;
    if minutes < 60 {
        return format!("{minutes}{}", strings.time_minutes());
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}{}", strings.time_hours());
    }
    format!("{}{}", hours / 24, strings.time_days())
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
            session_state_label(session.state, strings).to_string(),
        ),
        (strings.session_card_agent(), session.agent_id.to_string()),
    ];
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
        Page::Management => render_management(frame, area, app, theme, strings),
        Page::Providers | Page::Agents | Page::Mcp | Page::Skills | Page::Prompts => {
            render_entry_list(frame, area, app, theme, strings)
        }
        Page::Devices => render_devices(frame, area, app, theme, strings),
        Page::Usage => render_usage(frame, area, app, theme, strings),
        Page::Settings => render_settings(frame, area, app, theme, strings),
        Page::Help => render_help(frame, area, app, theme, strings),
        _ => {}
    }
}

/// The right gutter: one tick per turn.
///
/// A tick stands for a turn — one the reader opened — in conversation order, and
/// the turn the viewport starts on wears the heavier one, so the count of the
/// reader's conversation and their place in it are the same object, and the rail
/// is a map of the session rather than of the buffer. Nothing else is drawn
/// there: a scrollbar in the same columns would be counted as turns, which is
/// exactly the mistake the rail exists to prevent. When there are more turns
/// than rows the ticks window around the active one, because an unwindowed rail
/// would fold many turns onto a single row.
fn render_gutter(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &TuiTheme) {
    let turns = app.transcript.turn_count();
    let tier = app.glyph_tier();
    let rows = usize::from(area.height);
    if area.width == 0 || rows == 0 || turns == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows);

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
        app.regions.turns.push((
            Rect {
                y: area.y + row as u16,
                height: 1,
                ..area
            },
            turn,
        ));
        lines.push(Line::from(Span::styled(
            if is_active {
                crate::glyphs::timeline_tick_active(tier)
            } else {
                crate::glyphs::timeline_tick(tier)
            },
            Style::default().fg(if is_active {
                theme.roles.accent_user
            } else {
                theme.roles.gray_dim
            }),
        )));
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

    let (icon, label, color) = if approvals + questions > 0 {
        (
            crate::glyphs::diamond_filled(tier),
            format!(
                "{approvals} {} · {questions} {}",
                strings.approval_label(),
                strings.elicitation_title()
            ),
            theme.roles.accent_attention,
        )
    } else if live {
        (
            crate::glyphs::frame_at(
                crate::glyphs::spinner_frames(tier),
                phase,
                crate::glyphs::SPINNER_TICKS_PER_FRAME,
            ),
            turn_label(app, strings),
            theme.roles.accent_running,
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
        )
    };

    // One sentence, in the desktop's order: what the Agent is doing, how long
    // it has taken, how many tools it has called, what it has spent and how
    // fast it is writing. A band that is not about a running turn — idle, or
    // waiting on the reader — says only what it is; the measurements belong to
    // work that is actually happening.
    let mut line = vec![
        Span::styled(format!("{icon} "), Style::default().fg(color)),
        Span::styled(label, Style::default().fg(theme.roles.gray)),
    ];
    let measure = Style::default().fg(theme.roles.gray_dim);
    let measure_part = |line: &mut Vec<Span<'static>>, text: String| {
        line.push(Span::styled(" · ", measure));
        line.push(Span::styled(text, measure));
    };
    if live {
        if let Some(elapsed) = app.turn_elapsed() {
            measure_part(&mut line, format_turn_timer(elapsed));
        }
        let readout = app.turn_readout();
        if let Some(tool_calls) = readout
            .map(crate::turn::TurnReadout::tool_calls)
            .filter(|count| *count > 0)
        {
            measure_part(&mut line, format!("{tool_calls} {}", strings.tool_calls()));
        }
        if let Some(tokens) = app.turn_tokens() {
            measure_part(
                &mut line,
                format!(
                    "{}{}",
                    crate::glyphs::token_arrow(tier),
                    compact_tokens(tokens)
                ),
            );
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

/// Format an elapsed duration the way a person reads it.
pub fn format_turn_timer(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
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
    // and only while the cursor is in it.
    if app.queue_selection.is_some() {
        header.push(Span::styled(
            format!("  {}", strings.queue_hint()),
            Style::default().fg(theme.roles.gray_dim),
        ));
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
    for (row, at) in held.iter().enumerate().skip(offset).take(visible) {
        let message = &app.queued_messages[*at];
        let active = app.queue_selection == Some(row);
        let prefix = if active { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                prefix.to_string(),
                Style::default().fg(theme.roles.accent_user),
            ),
            Span::styled(
                format!("#{} ", row + 1),
                Style::default().fg(theme.roles.gray_dim),
            ),
            Span::styled(
                truncate_to_width(
                    message.text.lines().next().unwrap_or_default(),
                    usize::from(area.width).saturating_sub(6),
                    "…",
                ),
                if active {
                    theme.selected()
                } else {
                    Style::default().fg(theme.roles.gray)
                },
            ),
        ]));
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
        header.push(Span::styled(
            format!("  {}", strings.dock_hint()),
            Style::default().fg(theme.roles.gray_dim),
        ));
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
        let active = focused && index == selected;
        let base = if active {
            theme.selected()
        } else {
            theme.base()
        };
        match row {
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
/// when a narrow terminal makes it most worth reading. The prompt's corner
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

    if let Some(context) = context_usage_spans(app, theme) {
        push_segment(&mut right, Span::raw(""));
        right.pop();
        if !right.is_empty() {
            right.push(sep.clone());
        }
        right.extend(context);
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

    // ---- the corner: the way to the sessions that already exist ----------
    // The page a session is written on is where the client starts, and the
    // list is the other half of that home. It is drawn last so it holds the
    // corner, and it is the only chrome here that is a control: the reader who
    // wants to reopen something rather than write something should not have to
    // know a key the draft has taken.
    let mut entry: Option<(u16, u16)> = None;
    if app.page == Page::NewSession {
        let label = strings.nav_sessions();
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
        if !right.is_empty() {
            right.push(sep.clone());
        }
        let offset = right.iter().map(Span::width).sum::<usize>() as u16;
        right.push(Span::styled(
            label.to_string(),
            Style::default()
                .fg(theme.roles.accent_user)
                .add_modifier(Modifier::BOLD),
        ));
        let mut width = display_width(label) as u16;
        if let Some(chord) = chord {
            let chord = format!(" {}", chord.display());
            width += display_width(&chord) as u16;
            right.push(Span::styled(chord, theme.dimmed(theme.roles.gray_dim)));
        }
        entry = Some((offset, width));
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

/// The context-window readout, or nothing when there is no measurement.
///
/// Vibex records tokens and not prices, so this is the only quantitative status
/// the interface can honestly show.
fn context_usage_spans(app: &App, theme: &TuiTheme) -> Option<Vec<Span<'static>>> {
    // The numbers are a session's own: the composing page has none, and what was
    // read for the session behind it is not this page's context.
    if !app.page_owns_session() {
        return None;
    }
    let usage = app.management_data.usage_session.as_ref()?;
    let used = usage.total_tokens.unwrap_or(0);
    let total = usage.context_window_size_tokens?;
    if total == 0 {
        return None;
    }
    let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
    let color = if ratio >= 0.9 {
        theme.roles.danger
    } else if ratio >= 0.75 {
        theme.roles.warning
    } else if ratio >= 0.5 {
        theme.roles.accent_user
    } else {
        theme.roles.gray
    };
    Some(vec![Span::styled(
        format!(
            "{} {}/{}",
            crate::glyphs::token_arrow(app.glyph_tier()),
            compact_tokens(used),
            compact_tokens(total)
        ),
        Style::default().fg(color),
    )])
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
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.height < 4 || area.width < 24 {
        empty_state(frame, area, theme, strings.session_new());
        return;
    }
    let lit = app.composing_page_shines();
    let mark = crate::logo::rows(theme, app.animation_phase(), area.width, lit);
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Room for the mark, a blank row and the lines under it, or the mark is
    // dropped and the words speak for themselves.
    let mark_fits = !mark.is_empty() && area.height as usize >= mark.len() + 7;
    if mark_fits {
        lines.push(Line::from(""));
        let indent = usize::from(area.width)
            .saturating_sub(crate::logo::width(theme.glyphs(), area.width))
            / 2;
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

    // What the message will be sent through, and where. Both are answers the
    // reader needs before writing, and both are one key away from changing.
    let (agent, model) = app.composer_runtime_labels();
    let runtime = if model.is_empty() {
        agent
    } else {
        format!("{agent} · {model}")
    };
    lines.push(setting_row(
        area.width,
        strings.session_runtime_label(),
        &runtime,
        theme,
        Some(strings.runtime_switch_hint()),
    ));
    // A path is longer than any terminal: the tail is the part that says where
    // the session will work, so that is the part kept whole while the folders
    // above it fold to their initials.
    lines.push(setting_row(
        area.width,
        strings.session_workspace_label(),
        &app.new_session_workspace(),
        theme,
        Some(strings.workspace_switch_hint()),
    ));

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
    frame.render_widget(Paragraph::new(Text::from(lines)), region);
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
    if area.height < 4 {
        empty_state(frame, area, theme, strings.transcript_empty());
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

/// Draw the composer.
///
/// The composer is the one place the user types, so it gets the interface's
/// strongest affordances:
///
/// ```text
/// ╭─ <session title> ─────────────────────────────╮
/// │ ❯ the draft so far                             │
/// ╰─ <agent> · <model> · <mode>          multiline╯
/// ```
///
/// The top border carries the session, and the bottom border is an info line —
/// model, mode and any warning — which is where a terminal UI can show context
/// without spending a row on it. Both fade toward the canvas when the composer
/// does not have focus, so "where will my keystrokes go" is answerable at a
/// glance rather than by reading.
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
    let text_area = Rect {
        x: input.x + 1,
        y: input.y + 1,
        width: input.width.saturating_sub(2),
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

    // Runtime context sits below the padded writing surface.
    if area.height >= 2 {
        let bottom = Rect {
            y: area.y + area.height - 1,
            x: area.x + 1,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        render_composer_info(frame, bottom, app, theme, strings);
    }

    if let Some(menu) = app.completion.clone() {
        render_completion(frame, area, app, theme, &menu);
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
    frame.render_widget(Clear, area);
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
    frame.render_widget(Paragraph::new(Line::from(top)), Rect { height: 1, ..area });
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            rule_style,
        ))),
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
    frame.render_widget(Paragraph::new(Text::from(lines)), body);
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
fn render_composer_info(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
) {
    if area.height == 0 {
        return;
    }
    let (agent, model) = app.composer_runtime_labels();
    let mut left = vec![Span::styled(agent, theme.muted())];
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
    let images = app.composer.image_count();
    if images > 0 {
        left.push(Span::styled(
            format!(" · {images} {}", strings.image_count()),
            theme.muted(),
        ));
    }
    render_zoned_line(frame, area, left, None, Vec::new());
}

/// The completion popup, drawn directly above the composer.
///
/// Two full-width rules with no corners and no side borders, rather than a box.
/// A box would need four more glyphs, would sit inset from the composer it
/// belongs to, and would read as a separate window; two rules read as a drawer
/// pulled out of the prompt. The count on the top rule says how much is behind
/// it, which a border title cannot.
fn render_completion(
    frame: &mut Frame<'_>,
    anchor: Rect,
    app: &App,
    theme: &TuiTheme,
    menu: &crate::composer::CompletionMenu,
) {
    let indices = menu.visible();
    if indices.is_empty() {
        // No "no matches" state: the drawer simply closes, which is what the
        // reader expects when the query stops matching anything.
        return;
    }
    let tier = app.glyph_tier();
    let rows = indices.len().min(crate::composer::MAX_VISIBLE_COMPLETIONS);
    let height = rows as u16 + 2;
    if anchor.y < height {
        return;
    }
    let area = Rect {
        x: anchor.x,
        y: anchor.y - height,
        width: anchor.width,
        height,
    };
    frame.render_widget(Clear, area);

    let rule_style = Style::default().fg(theme.roles.gray_dim);
    let title = match menu.trigger {
        crate::composer::CompletionTrigger::Slash => strings_group(menu, 0),
        crate::composer::CompletionTrigger::At => "".to_string(),
        crate::composer::CompletionTrigger::Dollar => "".to_string(),
    };
    let _ = title;

    // Top rule, with the match count right-aligned on it.
    let mut top = vec![Span::styled(
        "─".repeat(usize::from(area.width)),
        rule_style,
    )];
    let count = format!(" {}/{} ", menu.selected + 1, indices.len());
    let count_width = display_width(&count);
    if usize::from(area.width) > count_width + 4 {
        let cut = usize::from(area.width) - count_width;
        top = vec![
            Span::styled("─".repeat(cut), rule_style),
            Span::styled(count, Style::default().fg(theme.roles.gray)),
        ];
    }
    frame.render_widget(Paragraph::new(Line::from(top)), Rect { height: 1, ..area });
    // Bottom rule, which is also the composer's top edge.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            rule_style,
        ))),
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
    let lines = indices
        .iter()
        .take(rows)
        .map(|index| {
            completion_row(
                menu,
                *index,
                usize::from(body.width),
                theme,
                tier,
                strings_of(app),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), body);
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
    strings: Strings,
) -> Line<'static> {
    let Some(item) = menu.items.get(index) else {
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
    let _ = strings;
    Line::from(spans)
}

fn strings_group(_menu: &crate::composer::CompletionMenu, _index: usize) -> String {
    String::new()
}

fn strings_of(_app: &App) -> Strings {
    Strings::for_locale(crate::locale::Locale::En)
}

fn render_management(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.nav_management(), true);
    let rows = ManagementRow::ALL;
    let labels = [
        strings.management_agents(),
        strings.management_providers(),
        strings.management_mcp(),
        strings.management_skills(),
        strings.management_prompts(),
        strings.management_devices(),
    ];
    let counts = [
        app.management_data.agents.len(),
        app.management_data.providers.len(),
        app.management_data.mcp.len(),
        app.management_data.skills.len(),
        app.management_data.prompts.len(),
        app.management_data.devices.len(),
    ];
    let selected = app.selection_for(Scope::Management);
    let items = rows
        .iter()
        .zip(labels.iter().zip(counts.iter()))
        .enumerate()
        .map(|(index, (row, (label, count)))| {
            let availability = match row {
                ManagementRow::Devices => {
                    app.availability(vibex_backend::BackendOperation::DeviceList)
                }
                _ => Availability::Available,
            };
            let suffix = match availability {
                Availability::RequiresPermission => {
                    format!("  [{}]", strings.permission_required_for())
                }
                Availability::Unsupported => format!("  [{}]", strings.toast_action_unavailable()),
                _ => format!("  ({count})"),
            };
            let style = if index == selected {
                theme.selected()
            } else if app.hover == Some((Scope::Management, index)) {
                theme.base().bg(theme.roles.surface_highlight)
            } else {
                theme.base()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<18}", label), style),
                Span::styled(suffix, theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
    app.regions.list = Some(crate::app::ListRegion {
        rect: inner,
        scope: Scope::Management,
        rows: rows.len(),
        first_line: 0,
        first_row: 0,
        heights: Vec::new(),
    });
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(rows.len() - 1)));
    frame.render_stateful_widget(List::new(items), inner, &mut state);
}

fn render_entry_list(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let title = match app.page {
        Page::Providers => strings.management_providers(),
        Page::Agents => strings.management_agents(),
        Page::Mcp => strings.management_mcp(),
        Page::Skills => strings.management_skills(),
        Page::Prompts => strings.management_prompts(),
        _ => strings.management_providers(),
    };
    let inner = page_frame(frame, area, theme, title, true);
    let entries: Vec<(String, String, bool)> = match app.page {
        Page::Providers => app
            .management_data
            .providers
            .iter()
            .map(|profile| {
                (
                    profile.display_name.clone(),
                    format!(
                        "{} · {:?}",
                        profile.default_model.clone().unwrap_or_default(),
                        profile.status
                    ),
                    profile.status == vibex_core::ProviderProfileStatus::Enabled,
                )
            })
            .collect(),
        Page::Agents => app
            .management_data
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.label.clone(),
                    format!("{:?} · {:?}", agent.install_status, agent.runtime_status),
                    agent.enabled,
                )
            })
            .collect(),
        Page::Mcp => app
            .management_data
            .mcp
            .iter()
            .map(|server| {
                (
                    server.display_name.clone(),
                    format!("{:?}", server.transport_kind),
                    server.status == vibex_core::McpServerStatus::Enabled,
                )
            })
            .collect(),
        Page::Skills => app
            .management_data
            .skills
            .iter()
            .map(|skill| {
                (
                    skill.display_name.clone(),
                    skill.description.clone().unwrap_or_default(),
                    skill.status == vibex_core::SkillStatus::Enabled,
                )
            })
            .collect(),
        Page::Prompts => app
            .management_data
            .prompts
            .iter()
            .map(|prompt| {
                (
                    prompt.display_name.clone(),
                    prompt.description.clone().unwrap_or_default(),
                    prompt.status == vibex_core::PromptStatus::Enabled,
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    if entries.is_empty() {
        empty_state(frame, inner, theme, strings.nothing_here());
        return;
    }
    let selected = app.selection_for(app.page.scope());
    let items = entries
        .iter()
        .enumerate()
        .map(|(index, (label, detail, enabled))| {
            let style = if index == selected {
                theme.selected()
            } else if *enabled {
                theme.base()
            } else {
                theme.muted()
            };
            let marker = if *enabled { "●" } else { "○" };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{marker} {:<20}", truncate_to_width(label, 20, "…")),
                    style,
                ),
                Span::styled(truncate_to_width(detail, 40, "…"), theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
    app.regions.list = Some(crate::app::ListRegion {
        rect: inner,
        scope: app.page.scope(),
        rows: entries.len(),
        first_line: 0,
        first_row: 0,
        heights: Vec::new(),
    });
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(entries.len() - 1)));
    frame.render_stateful_widget(List::new(items), inner, &mut state);
}

fn render_devices(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.devices_title(), true);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);
    if app.management_data.devices.is_empty() {
        empty_state(frame, rows[0], theme, strings.devices_empty());
    } else {
        let selected = app.selection_for(Scope::Devices);
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
                    vibex_core::RemoteDevicePermissionLevel::ReadOnly => {
                        strings.permission_read_only()
                    }
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
        let mut state = ratatui::widgets::ListState::default();
        state.select(Some(selected.min(app.management_data.devices.len() - 1)));
        frame.render_stateful_widget(List::new(items), rows[0], &mut state);
    }
    let audit_hint = format!(
        "{}: {}",
        strings.devices_audit(),
        app.management_data.audit.len()
    );
    frame.render_widget(Paragraph::new(audit_hint).style(theme.muted()), rows[1]);
}

fn render_usage(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.usage_title(), true);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(inner);

    let mut lines = Vec::new();
    if let Some(snapshot) = &app.management_data.usage_session {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<16}", strings.usage_input_tokens()),
                theme.muted(),
            ),
            Span::styled(snapshot.input_tokens.unwrap_or(0).to_string(), theme.base()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<16}", strings.usage_output_tokens()),
                theme.muted(),
            ),
            Span::styled(
                snapshot.output_tokens.unwrap_or(0).to_string(),
                theme.base(),
            ),
        ]));
        if let Some(cached) = snapshot.cached_read_tokens {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<16}", strings.usage_cached_tokens()),
                    theme.muted(),
                ),
                Span::styled(cached.to_string(), theme.base()),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<16}", strings.usage_total_tokens()),
                theme.muted(),
            ),
            Span::styled(
                snapshot.total_tokens.unwrap_or(0).to_string(),
                theme.strong(),
            ),
        ]));
        if let Some(window) = snapshot.context_window_size_tokens {
            let total = snapshot.total_tokens.unwrap_or(0);
            let ratio = if window > 0 {
                total as f64 / window as f64
            } else {
                0.0
            };
            let filled = (ratio.clamp(0.0, 1.0) * 20.0).round() as usize;
            lines.push(Line::from(vec![
                Span::styled(format!("{:<16}", strings.usage_context()), theme.muted()),
                Span::styled(
                    format!("[{}{}]", "█".repeat(filled), "░".repeat(20 - filled)),
                    theme.accent(),
                ),
                Span::styled(format!(" {total}/{window}"), theme.muted()),
            ]));
        }
    }
    if let Some(statistics) = &app.management_data.usage {
        lines.push(Line::from(""));
        for row in statistics.dimension_rows.iter().take(20) {
            let label = row.label.clone();
            let values = format!(
                "requests={} total={}",
                row.aggregate.requests,
                row.aggregate.total_tokens.value.unwrap_or(0)
            );
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<28}", truncate_to_width(&label, 28, "…")),
                    theme.base(),
                ),
                Span::styled(truncate_to_width(&values, 40, "…"), theme.muted()),
            ]));
        }
    }
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(strings.nothing_here()).style(theme.muted()),
            rows[0],
        );
    } else {
        frame.render_widget(Paragraph::new(Text::from(lines)), rows[0]);
    }
    frame.render_widget(
        Paragraph::new(strings.usage_no_cost_hint())
            .style(theme.muted())
            .wrap(Wrap { trim: true }),
        rows[1],
    );
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
    app: &App,
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
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                "  │  ",
                Style::default().fg(theme.roles.gray_dim),
            ));
        }
        spans.push(Span::styled(
            format!("{key} "),
            Style::default()
                .fg(theme.roles.gray_bright)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            label.to_string(),
            Style::default().fg(theme.roles.gray_dim),
        ));
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
    let scopes = app.documented_scopes();
    let needle = app.filter.trim().to_lowercase();
    // Help is generated from the binding tables, so filtering it is filtering
    // the same data that dispatches the keys.
    let bindings = app
        .keymap
        .advertised(&scopes)
        .into_iter()
        .filter(|binding| {
            needle.is_empty()
                || binding.chord.display().to_lowercase().contains(&needle)
                || binding
                    .label
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&needle)
                || binding.intent.id().contains(&needle)
                || binding.intent.help().to_lowercase().contains(&needle)
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
                Span::styled(binding.label.unwrap_or_default().to_string(), theme.base()),
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
            || binding
                .label
                .unwrap_or_default()
                .to_lowercase()
                .contains(&needle)
            || binding.intent.id().contains(&needle)
            || binding.intent.help().to_lowercase().contains(&needle)
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
            .filter(|binding| {
                needle.is_empty()
                    || binding.chord.display().to_lowercase().contains(&needle)
                    || binding
                        .label
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
                    || binding.intent.id().contains(&needle)
                    || binding.intent.help().to_lowercase().contains(&needle)
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
                let label = binding.label.unwrap_or_default();
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
                            binding.intent.help().to_string(),
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
                let label = binding.label.unwrap_or_default();
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
    let scopes = app.documented_scopes();
    let strings = app.strings;
    let mut hints = Vec::new();
    if app.page.is_session_page() && app.overlay.is_none() {
        let composing = app.focus == crate::app::Focus::Composer || app.page_is_composing();
        if composing {
            hints.push((
                Intent::SubmitComposer,
                if app.turn_reads_running() {
                    strings.composer_queue()
                } else {
                    strings.composer_send()
                },
            ));
            hints.push((Intent::InsertNewline, strings.composer_newline()));
        } else {
            hints.push((Intent::ToggleBlockExpanded, strings.transcript_expand()));
            hints.push((Intent::OpenBlockDetails, strings.transcript_details()));
            hints.push((Intent::FocusComposer, strings.composer_send()));
        }
        if app.turn_reads_running() {
            hints.push((
                Intent::ContextualCancel,
                if app.composer.is_empty() {
                    strings.session_stop()
                } else {
                    strings.hint_clear()
                },
            ));
        }
        hints.push((Intent::SwitchAgentRuntime, strings.runtime_title()));
        hints.push((Intent::FocusNext, strings.transcript_browse()));
    }
    for binding in app.keymap.advertised(&scopes) {
        if let Some(label) = binding.label
            && !hints.iter().any(|(intent, _)| *intent == binding.intent)
        {
            hints.push((binding.intent, label));
        }
    }
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1usize;
    for (intent, label) in hints {
        let Some(chord) = app.keymap.chord_for(intent) else {
            continue;
        };
        let key = chord.display();
        let separator = if used == 1 { "" } else { "  ·  " };
        let width = display_width(&key) + 1 + display_width(label);
        if used + display_width(separator) + width > usize::from(area.width) {
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

/// Compact a token count to at most four characters.
///
/// A status bar cannot afford eight digits for a number nobody reads exactly;
/// the precise figure lives on the usage page.
pub fn compact_tokens(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=9_999 => format!("{:.1}K", value as f64 / 1_000.0),
        10_000..=999_999 => format!("{}K", value / 1_000),
        1_000_000..=9_999_999 => format!("{:.1}M", value as f64 / 1_000_000.0),
        _ => format!("{}M", value / 1_000_000),
    }
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

fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    overlay: &Overlay,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    match overlay {
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
            // over the flat entry list, so a header is skipped by the cursor
            // rather than counted as a row.
            let recent_count = if query.trim().is_empty() {
                app.palette_recent_count()
            } else {
                0
            };
            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut last_group = None;
            for (index, entry) in matches.iter().enumerate() {
                let group = if index < recent_count {
                    PaletteGroup::Recent
                } else {
                    palette_group(entry.intent)
                };
                if last_group != Some(group) {
                    last_group = Some(group);
                    lines.push(Line::from(Span::styled(
                        group.label(strings).to_string(),
                        Style::default()
                            .fg(theme.roles.gray)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                let style = if index == *selected {
                    theme.selected()
                } else {
                    theme.base()
                };
                lines.push(Line::from(vec![
                    Span::styled(if index == *selected { "▸ " } else { "  " }, style),
                    Span::styled(format!("{:<24}", entry.label), style),
                    Span::styled(entry.hint, theme.muted()),
                ]));
            }
            let height = usize::from(rows[1].height);
            // Keep the selection on screen: the list can be longer than the
            // drawer, and the cursor is the anchor.
            let selected_line = {
                let mut line = 0usize;
                let mut group = None;
                for (index, entry) in matches.iter().enumerate() {
                    let entry_group = if index < recent_count {
                        PaletteGroup::Recent
                    } else {
                        palette_group(entry.intent)
                    };
                    if group != Some(entry_group) {
                        group = Some(entry_group);
                        line += 1;
                    }
                    if index == *selected {
                        break;
                    }
                    line += 1;
                }
                line
            };
            let offset = selected_line.saturating_sub(height.saturating_sub(1));
            let visible = lines
                .into_iter()
                .skip(offset)
                .take(height)
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(Text::from(visible)), rows[1]);
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
                return;
            };
            let masked = matches!(field, crate::app::PromptField::ProviderSecret);
            let shown = if masked {
                "•".repeat(value.chars().count())
            } else {
                value.clone()
            };
            let mut lines = vec![Line::from(Span::styled(shown, theme.base()))];
            if masked {
                lines.push(Line::from(Span::styled(
                    strings.management_secret_write_only(),
                    theme.muted(),
                )));
            }
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                layout.content,
            );
        }
        Overlay::Approval { selected } => render_approval(frame, area, app, *selected, theme),
        Overlay::Elicitation { field } => render_elicitation(frame, area, app, *field, theme),
        Overlay::PairingCode {
            code,
            link,
            permission,
        } => {
            render_pairing(frame, area, app, code, link, *permission, theme);
        }
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
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
        Overlay::WorkspacePicker { selected } => {
            let listing = app.workspace_browse.clone();
            let path = listing
                .as_ref()
                .map(|listing| listing.path.clone())
                .unwrap_or_default();
            let title = format!(
                "{}  {}",
                strings.session_workspace_label(),
                compact_path(&path, 40)
            );
            let chrome = modal_chrome(
                app,
                &title,
                ModalSizing::picker(),
                vec![
                    ModalHint::new("↑↓", strings.hint_nav()),
                    ModalHint::new("Enter", strings.hint_select()),
                    ModalHint::new("Esc", strings.close()),
                ],
            );
            let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
                return;
            };
            let entries = listing
                .as_ref()
                .map(|listing| listing.entries.clone())
                .unwrap_or_default();
            let items = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if index == *selected { "❯ " } else { "  " }.to_string(),
                            Style::default().fg(theme.roles.accent_user),
                        ),
                        Span::styled(truncate_to_width(&entry.name, 44, "…"), style),
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
    let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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

fn render_elicitation(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    field: usize,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let elicitations = app.elicitations();
    let Some(surface) = elicitations.first() else {
        return;
    };
    let request = surface.request.clone();
    let chrome = modal_chrome(
        app,
        strings.elicitation_title(),
        ModalSizing::card(),
        vec![
            ModalHint::new("Tab", strings.hint_next()),
            ModalHint::new("Shift+Tab", strings.hint_previous()),
            ModalHint::new("Enter", strings.elicitation_submit()),
            ModalHint::new("Esc", strings.close()),
        ],
    );
    let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
        return;
    };

    let mut lines = Vec::new();
    if let Some(title) = request.title.as_ref().filter(|title| !title.is_empty()) {
        lines.push(Line::from(Span::styled(title.clone(), theme.strong())));
        lines.push(Line::from(""));
    }
    for (index, definition) in request.fields.iter().enumerate() {
        let active = index == field;
        let marker = if active { "▸ " } else { "  " };
        let required = if definition.required {
            format!(" *{}", strings.elicitation_required())
        } else {
            String::new()
        };
        let style = if active {
            theme.accent()
        } else {
            theme.muted()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{}", definition.title), style),
            Span::styled(required, theme.warning()),
        ]));
        let answer = summarize_answer(&request, definition, &app.elicitation_draft);
        let answer_style = if answer.is_empty() {
            theme.muted()
        } else {
            theme.base()
        };
        lines.push(Line::from(vec![
            Span::styled("    ", theme.muted()),
            Span::styled(
                if answer.is_empty() {
                    format!("({})", field_kind_label(definition, strings))
                } else {
                    answer
                },
                answer_style,
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("[{}]", strings.elicitation_submit()),
        theme.accent().add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }),
        layout.content,
    );
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

fn summarize_answer(
    request: &vibex_core::ElicitationRequest,
    definition: &vibex_core::ElicitationField,
    draft: &crate::reduce::ElicitationDraft,
) -> String {
    let index = request
        .fields
        .iter()
        .position(|field| field.id == definition.id)
        .unwrap_or(0);
    match draft.answer_for(&definition.id, index, definition, index) {
        Some(vibex_core::ElicitationAnswerValue::String(value)) => value,
        Some(vibex_core::ElicitationAnswerValue::Integer(value)) => value.to_string(),
        Some(vibex_core::ElicitationAnswerValue::Number(value)) => value,
        Some(vibex_core::ElicitationAnswerValue::Boolean(value)) => {
            if value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Some(vibex_core::ElicitationAnswerValue::StringArray(values)) => values.join(", "),
        None => String::new(),
    }
}

fn render_pairing(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    code: &str,
    link: &str,
    permission: vibex_core::RemoteDevicePermissionLevel,
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let chrome = modal_chrome(
        app,
        strings.devices_pairing_code(),
        ModalSizing::picker(),
        vec![ModalHint::new("Esc", strings.close())],
    );
    let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(24), Constraint::Length(46)])
        .split(layout.content);

    let permission_label = match permission {
        vibex_core::RemoteDevicePermissionLevel::ReadOnly => strings.permission_read_only(),
        vibex_core::RemoteDevicePermissionLevel::ApproveOnly => strings.permission_approve_only(),
        vibex_core::RemoteDevicePermissionLevel::FullControl => strings.permission_full_control(),
    };
    let lines = vec![
        Line::from(Span::styled(
            code.to_string(),
            theme.strong().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("{}: ", strings.devices_permission()), theme.muted()),
            Span::styled(permission_label, theme.accent()),
        ]),
        Line::from(""),
        Line::from(Span::styled(link.to_string(), theme.muted())),
        Line::from(""),
        Line::from(Span::styled(strings.devices_code_hint(), theme.warning())),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true }),
        rows[0],
    );

    // The QR is rendered from the link, which already carries the TLS pin.
    if let Ok(qr) = QrCode::with_error_correction_level(link.as_bytes(), qrcode::EcLevel::L) {
        let image = qr.render::<Dense1x2>().quiet_zone(false).build();
        frame.render_widget(Paragraph::new(image).style(theme.base()), rows[1]);
    }
}

fn render_text_view(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    title: &str,
    body: &str,
    scroll: usize,
    theme: &TuiTheme,
) {
    let chrome = modal_chrome(
        app,
        title,
        ModalSizing::document(),
        vec![
            ModalHint::new("↑↓", strings_of(app).hint_scroll()),
            ModalHint::new("Esc", strings_of(app).close()),
        ],
    );
    let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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

/// Build a modal's chrome, giving the margins back on a compact terminal.
/// The runtime switcher.
///
/// Two views of one question. The catalogue is a tree — Agent headings with
/// their accounts and models under them, and the pinned recent and starred
/// entries above the lot — because a flat list of every model on the machine
/// says nothing about which Agent is which. The run options are the other side
/// of `Tab`: how the entry under the cursor runs.
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
    // The title names what the choice moves. A page showing no session — the
    // list, or the page where a session is being written — has nothing to move,
    // so the choice is the next session's, and saying so here is what keeps the
    // reader from expecting the session behind the page to change.
    let title = if show_choices {
        match app.page_shows_session() {
            true => strings.runtime_title().to_string(),
            false => format!(
                "{} · {}",
                strings.runtime_title(),
                strings.runtime_next_session()
            ),
        }
    } else {
        // The staged count rides in the title: the footer's hints are static
        // words, and the one number a reader wants while editing is how much is
        // waiting to be sent.
        match app
            .runtime_picker
            .draft
            .as_ref()
            .filter(|draft| draft.is_dirty())
        {
            Some(draft) => format!(
                "{} · {} ({})",
                strings.runtime_title(),
                strings.runtime_run_options(),
                draft.change_count()
            ),
            None => format!(
                "{} · {}",
                strings.runtime_title(),
                strings.runtime_run_options()
            ),
        }
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
        let mut hints = vec![
            ModalHint::new("↑↓", strings.hint_nav()),
            ModalHint::new("Enter", strings.hint_select()),
            ModalHint::new("r", strings.runtime_reset()),
        ];
        hints.push(ModalHint::new("Ctrl+S", strings.runtime_apply()));
        hints.push(ModalHint::new("Tab", strings.runtime_title()));
        // Leaving the run options with something staged drops it, which is what
        // the hint has to say: "back" would read as "keep them".
        let dirty = app
            .runtime_picker
            .draft
            .as_ref()
            .is_some_and(|draft| draft.is_dirty());
        hints.push(ModalHint::new(
            "Esc",
            if dirty {
                strings.cancel()
            } else {
                strings.hint_back()
            },
        ));
        hints
    };
    let chrome = modal_chrome(app, &title, ModalSizing::picker(), hints);
    let Some(layout) = modal::render_modal(frame, area, &chrome, theme) else {
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
    // drawn when there is room for a list above it.
    let preview = content.height >= 6;
    if preview {
        content.height -= 1;
    }
    let width = usize::from(content.width);
    let mut lines = Vec::with_capacity(rows.len());
    let mut in_group = false;
    for (index, row) in rows.iter().enumerate() {
        match row {
            RuntimePickerRow::Section(section) => {
                in_group = false;
                lines.push(Line::from(Span::styled(
                    truncate_to_width(&format!("  {}", section.label(&strings)), width, "…"),
                    theme.dimmed(theme.roles.gray).add_modifier(Modifier::BOLD),
                )));
            }
            RuntimePickerRow::Agent { .. } => {
                in_group = true;
                lines.push(runtime_picker_heading_line(
                    app,
                    row,
                    index == selected,
                    width,
                    theme,
                ));
            }
            RuntimePickerRow::Entry { .. } => lines.push(runtime_picker_entry_line(
                app,
                &options,
                row,
                index == selected,
                in_group,
                width,
                theme,
            )),
        }
    }
    // Keep the cursor on screen with the least movement: the window only moves
    // when the cursor leaves it, which is what makes `↑↓` feel like a list
    // rather than like a camera. The offset is recorded because the page keys
    // and the scroll bar both need to know where the window ended up.
    let selected = selected.min(rows.len().saturating_sub(1));
    let height = usize::from(content.height);
    let mut offset = app
        .runtime_picker
        .scroll
        .min(rows.len().saturating_sub(height.min(rows.len())));
    if selected < offset {
        offset = selected;
    }
    if selected >= offset + height {
        offset = (selected + 1).saturating_sub(height);
    }
    app.runtime_picker.scroll = offset;
    app.runtime_picker.page_rows = height.max(1);
    app.regions.runtime_picker = Some(crate::app::RuntimePickerRegion {
        rect: content,
        offset,
        rows: rows.len(),
    });

    // The scroll bar is one column of the list rather than a widget: a reader
    // who cannot see how long the catalogue is cannot tell a short list from a
    // clipped one.
    let bar = rows.len() > height && content.width > 4;
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
    if bar {
        render_scroll_indicator(frame, content, offset, rows.len(), theme);
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

/// One Agent's heading: what it holds, and whether it is the one in use.
fn runtime_picker_heading_line(
    app: &App,
    row: &RuntimePickerRow,
    selected: bool,
    width: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    let strings = app.strings;
    let RuntimePickerRow::Agent {
        label,
        count,
        folded,
        current,
        unavailable,
        ..
    } = row
    else {
        return Line::from("");
    };
    let mut spans = vec![
        Span::styled(
            if *folded { "▸ " } else { "▾ " }.to_string(),
            theme.dimmed(theme.roles.gray),
        ),
        Span::styled(
            truncate_to_width(label, width.saturating_sub(2), "…"),
            theme.base().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {count}"), theme.muted()),
    ];
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
    selected_line(spans, selected, width, theme)
}

/// One entry: its account and model, and what it says about itself.
fn runtime_picker_entry_line(
    app: &App,
    options: &[vibex_core::SessionRuntimeOption],
    row: &RuntimePickerRow,
    selected: bool,
    in_group: bool,
    width: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    let strings = app.strings;
    let RuntimePickerRow::Entry { index, quick } = row else {
        return Line::from("");
    };
    let Some(option) = options.get(*index) else {
        return Line::from("");
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
    let status_width = status.as_deref().map(display_width).unwrap_or_default();
    let gap = if status_width > 0 { 2 } else { 0 };
    let available = width.saturating_sub(indent + 2 + status_width + gap);
    // The model is what a reader scans for, so it takes the larger share and the
    // account truncates first: "Default CLI account/…" names the provenance, but
    // two rows of the same model on two accounts are still two rows.
    let (auth_width, model_width) = runtime_columns(available);
    let body = format!(
        "{} · {}",
        truncate_to_width(&option.auth_source_label, auth_width, "…"),
        truncate_to_width(&option.model_label, model_width, "…"),
    );
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
        Span::styled(
            pad_to_width(&body, available),
            if current { theme.base() } else { theme.muted() },
        ),
    ];
    if let Some(status) = status {
        spans.push(Span::styled(" ".repeat(gap), theme.base()));
        spans.push(Span::styled(
            status,
            if current {
                Style::default().fg(theme.roles.accent_user)
            } else {
                theme.dimmed(theme.roles.gray)
            },
        ));
    }
    selected_line(spans, selected, width, theme)
}

/// The preview of the highlighted entry: how it would run, and where that is
/// changed. Drawn under the list rather than in it, so it cannot be mistaken for
/// a row.
fn runtime_picker_preview(
    app: &App,
    option: Option<&vibex_core::SessionRuntimeOption>,
    width: usize,
    theme: &TuiTheme,
) -> Line<'static> {
    let strings = app.strings;
    // A heading has no entry of its own, so the preview answers for the page's
    // selection — which is exactly what `Tab` would show from there.
    let run_options = match option {
        Some(option) => app.run_options_for(&app.runtime_prefs.with_remembered_options(option)),
        None => app.picker_run_options(),
    };
    if run_options.is_empty() {
        return Line::from("");
    }
    let labels = run_options
        .iter()
        .filter(|option| option.is_explicit())
        .map(|option| {
            format!(
                "{} {}",
                option.label,
                option.resolved_label(strings.runtime_default())
            )
        })
        .collect::<Vec<_>>();
    let body = if labels.is_empty() {
        strings.runtime_default().to_string()
    } else {
        labels.join(" · ")
    };
    let text = format!(
        "{}: {body} · {}",
        strings.runtime_run_options(),
        strings.runtime_summary_hint()
    );
    Line::from(Span::styled(
        truncate_to_width(&text, width, "…"),
        theme.dimmed(theme.roles.gray),
    ))
}

/// The run options of the entry the page is on, with what the reader has staged
/// marked as staged.
fn render_runtime_options(
    frame: &mut Frame<'_>,
    content: Rect,
    app: &mut App,
    selected: usize,
    run_options: &[crate::app::RunOption],
    theme: &TuiTheme,
) {
    let strings = app.strings;
    let (agent, model) = app.composer_runtime_labels();
    let mut content = content;
    if content.height > 1 {
        let caption = if model.is_empty() {
            agent
        } else {
            format!("{agent} · {model}")
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_to_width(&caption, usize::from(content.width), "…"),
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
    let width = usize::from(content.width);
    let mut lines = Vec::with_capacity(run_options.len());
    for (index, option) in run_options.iter().enumerate() {
        let staged = app
            .runtime_picker
            .draft
            .as_ref()
            .is_some_and(|draft| draft.changed(&option.key));
        let set = option.is_explicit();
        let value = option.resolved_label(strings.runtime_default());
        let tag = if staged {
            strings.runtime_modified()
        } else {
            ""
        };
        let tag_width = display_width(tag);
        let gap = if tag_width > 0 { 2 } else { 0 };
        let value_width = width.saturating_sub(2 + 18 + tag_width + gap);
        let mut spans = vec![
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
        if tag_width > 0 {
            spans.push(Span::styled(" ".repeat(gap), theme.base()));
            spans.push(Span::styled(
                tag.to_string(),
                Style::default().fg(theme.roles.accent_user),
            ));
        }
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

/// Split the width left for "account · model" between the two names.
fn runtime_columns(available: usize) -> (usize, usize) {
    const SEPARATOR: usize = 3;
    if available <= SEPARATOR {
        return (0, available);
    }
    let body = available - SEPARATOR;
    let model = (body * 3 / 5).max(4).min(body);
    (body - model, model)
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
    ModalChrome::new(title, sizing).hints(hints)
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
    use crate::locale::Locale;

    fn strings() -> Strings {
        Strings::for_locale(crate::locale::Locale::En)
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
        assert!(matches[0].label.to_lowercase().starts_with("he"));
    }

    #[test]
    fn palette_is_empty_for_nonsense() {
        assert!(palette_matches("zzzzzz", strings()).is_empty());
    }

    #[test]
    fn palette_finds_destinations_by_localised_name() {
        let chinese = palette_matches("用量", Strings::for_locale(Locale::ZhCn));
        assert!(
            chinese
                .iter()
                .any(|entry| entry.intent == Intent::GotoUsage),
            "localised palette lookup failed"
        );
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
            assert!(!entry.label.is_empty());
            assert!(!entry.hint.is_empty());
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
        assert!(help.contains("Remote Access"));
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
}
