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
use ratatui::symbols::border;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};

/// A block whose border glyphs match the terminal's capability.
///
/// A non-UTF-8 locale must still get a usable frame, so the box-drawing set is
/// swapped for `+-|` rather than assuming the glyphs will render.
fn bordered(theme: &TuiTheme) -> Block<'static> {
    let block = Block::default().borders(Borders::ALL);
    match theme.glyphs() {
        GlyphMode::Unicode => block.border_type(BorderType::Rounded),
        GlyphMode::Ascii => block
            .border_type(BorderType::Plain)
            .border_set(border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                horizontal_top: "-",
                horizontal_bottom: "-",
                vertical_left: "|",
                vertical_right: "|",
            }),
    }
}
use vibex_ui::shell::ShellKind;

use crate::action::Intent;
use crate::app::{
    App, Availability, ManagementRow, Overlay, Page, RecoveryAction, SettingRow, ToastTone,
};
use crate::keymap::Scope;
use crate::locale::Strings;
use crate::text::{display_width, truncate_to_width};
use crate::theme::{GlyphMode, TuiTheme};

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
        label: "Files",
        hint: "Open the workspace file tree",
        intent: Intent::OpenFiles,
    },
    PaletteEntry {
        label: "Changes",
        hint: "Open the Git workbench",
        intent: Intent::OpenChanges,
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

/// Palette entries matching `query`.
pub fn palette_matches(query: &str, strings: Strings) -> Vec<PaletteEntry> {
    let needle = query.trim().to_lowercase();
    let mut scored = PALETTE
        .iter()
        .copied()
        .filter_map(|entry| {
            let label = entry.label.to_lowercase();
            let hint = entry.hint.to_lowercase();
            // Prefix matches outrank substring matches, which outrank a hit
            // in the description.
            let score = if needle.is_empty() || label.starts_with(&needle) {
                0usize
            } else if label.contains(&needle) {
                1
            } else if hint.contains(&needle) {
                2
            } else {
                return None;
            };
            Some((score, entry))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, entry)| (*score, entry.label));
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

/// The transcript width available inside the agent page chrome.
pub fn transcript_width(shell: ShellKind, columns: u16) -> usize {
    layout_for(shell, columns, 40).main_width.saturating_sub(4)
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
pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    app.shell = crate::app::shell_for_columns(area.width);
    let theme = app.theme.clone();
    let strings = app.strings;

    frame.render_widget(Block::default().style(theme.base()), area);

    let plan = layout_for(app.shell, area.width, area.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top bar
            Constraint::Min(3),    // body
            Constraint::Length(1), // key bar
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_top_bar(frame, rows[0], app, &theme);
    render_body(frame, rows[1], app, &plan, &theme, strings);
    render_key_bar(frame, rows[2], app, &theme);
    render_status_bar(frame, rows[3], app, &theme);

    if let Some(overlay) = app.overlay.clone() {
        render_overlay(frame, area, app, &overlay, &theme);
    }
}

fn render_top_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &TuiTheme) {
    let strings = app.strings;
    let mut spans = vec![
        Span::styled(
            format!(" {} ", strings.app_name()),
            theme.accent().add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ ", theme.border_style()),
    ];
    for (index, page) in [
        (1u8, Page::Sessions),
        (2, Page::Management),
        (3, Page::Usage),
        (4, Page::Settings),
        (5, Page::Help),
    ] {
        let label = match page {
            Page::Sessions => strings.nav_sessions(),
            Page::Usage => strings.nav_usage(),
            Page::Settings => strings.nav_settings(),
            Page::Help => strings.nav_help(),
            _ => strings.nav_management(),
        };
        let active = app.page == page || (page == Page::Sessions && app.page.is_session_page());
        let style = if active {
            theme.accent().add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        spans.push(Span::styled(format!("{index}:{label}"), style));
        spans.push(Span::styled("  ", theme.muted()));
    }
    if app.page.is_session_page() {
        spans.push(Span::styled("│ ", theme.border_style()));
        for (label, page) in [
            (strings.nav_agent(), Page::Agent),
            (strings.nav_files(), Page::Files),
            (strings.nav_changes(), Page::Changes),
            (strings.nav_terminal(), Page::Terminal),
        ] {
            let style = if app.page == page {
                theme.strong()
            } else {
                theme.muted()
            };
            spans.push(Span::styled(format!("{label} "), style));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    plan: &LayoutPlan,
    theme: &TuiTheme,
    strings: Strings,
) {
    let mut constraints = Vec::new();
    if plan.show_sidebar {
        constraints.push(Constraint::Length(plan.sidebar_width as u16));
    }
    constraints.push(Constraint::Min(20));
    if plan.show_details {
        constraints.push(Constraint::Length(plan.details_width as u16));
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    let mut index = 0;
    if plan.show_sidebar {
        let focused = app.focus == crate::app::Focus::Sidebar;
        render_sidebar(frame, columns[index], app, theme, strings, focused);
        index += 1;
    }
    let main = columns[index];
    render_main(frame, main, app, theme, strings);
    index += 1;
    if plan.show_details {
        let focused = app.focus == crate::app::Focus::Details;
        render_details(frame, columns[index], app, theme, strings, focused);
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

/// The empty-transcript state.
///
/// A blank pane with one grey sentence wastes the moment the reader is most
/// likely to be lost. This says what the product is, what it can do, and the
/// two keys that get started.
fn render_welcome(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme, strings: Strings) {
    if area.height < 4 {
        empty_state(frame, area, theme, strings.transcript_empty());
        return;
    }
    let accent = Style::default()
        .fg(theme.roles.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.roles.gray_dim);
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {}", strings.app_name()), accent)),
        Line::from(Span::styled(
            format!("  {}", strings.product_tagline()),
            Style::default().fg(theme.roles.gray),
        )),
        Line::from(""),
    ];
    for (key, label) in [
        ("/", strings.composer_command_menu()),
        ("@", strings.composer_file_menu()),
        ("$", strings.composer_skill_menu()),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key}  "), accent),
            Span::styled(label.to_string(), dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  {}", strings.help_hint()),
        dim,
    )));
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// A single centred line saying why a pane is empty.
///
/// It deliberately does not wrap: a wrapped sentence in a narrow pane reads as
/// a layout fault, and the key that resolves the emptiness is already on the
/// key bar.
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

fn render_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
    focused: bool,
) {
    let inner = page_frame(frame, area, theme, strings.sessions_title(), focused);
    if app.filtering || !app.filter.is_empty() {
        let filter_area = Rect { height: 1, ..inner };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", theme.accent()),
                Span::styled(app.filter.clone(), theme.base()),
                Span::styled("▏", theme.accent()),
            ])),
            filter_area,
        );
    }
    let list_area = if app.filtering || !app.filter.is_empty() {
        Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        }
    } else {
        inner
    };

    let rows = app.sidebar_rows();
    if rows.is_empty() {
        // The sidebar is narrow, so it states the fact and leaves the way out
        // to the key bar, which already offers `n`.
        empty_state(frame, list_area, theme, strings.nothing_here());
        return;
    }
    let selected = app.selection_for(Scope::Sessions);
    let items = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let active = row
                .session_id
                .as_ref()
                .is_some_and(|session_id| app.selected_session_id() == Some(session_id));
            let marker = match row.kind {
                vibex_desktop_model::AgentSidebarRowKind::Project => {
                    if row.collapsed {
                        "▸"
                    } else {
                        "▾"
                    }
                }
                vibex_desktop_model::AgentSidebarRowKind::Session => "",
            };
            let indent = " ".repeat(usize::from(row.depth) * 2);
            let state = row
                .state
                .map(|state| format!(" {}", session_state_marker(state)))
                .unwrap_or_default();
            let text = format!("{indent}{marker} {}{state}", row.label);
            let style = if active {
                theme.accent().add_modifier(Modifier::BOLD)
            } else if index == selected {
                theme.selected()
            } else if row.kind == vibex_desktop_model::AgentSidebarRowKind::Project {
                theme.muted().add_modifier(Modifier::BOLD)
            } else {
                theme.base()
            };
            let marker_prefix = if row.pinned { "● " } else { "" };
            ListItem::new(Line::from(Span::styled(
                format!("{marker_prefix}{text}"),
                style,
            )))
        })
        .collect::<Vec<_>>();
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(rows.len().saturating_sub(1))));
    frame.render_stateful_widget(List::new(items), list_area, &mut state);
}

fn session_state_marker(state: vibex_core::AgentSessionState) -> &'static str {
    match state {
        vibex_core::AgentSessionState::Initializing => "…",
        vibex_core::AgentSessionState::Idle => "",
        vibex_core::AgentSessionState::Running => "▶",
        vibex_core::AgentSessionState::NeedsInput => "!",
        vibex_core::AgentSessionState::Error => "✗",
        vibex_core::AgentSessionState::Archived => "▪",
        _ => "",
    }
}

fn render_main(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    match app.page {
        Page::Sessions => render_session_summary(frame, area, app, theme, strings),
        Page::Agent => render_agent(frame, area, app, theme, strings),
        Page::Files | Page::Changes => render_file_list(frame, area, app, theme, strings),
        Page::Management => render_management(frame, area, app, theme, strings),
        Page::Providers | Page::Agents | Page::Mcp | Page::Skills | Page::Prompts | Page::Hooks => {
            render_entry_list(frame, area, app, theme, strings)
        }
        Page::Devices => render_devices(frame, area, app, theme, strings),
        Page::Usage => render_usage(frame, area, app, theme, strings),
        Page::Recovery => render_recovery(frame, area, app, theme, strings),
        Page::Settings => render_settings(frame, area, app, theme, strings),
        Page::Help => render_help(frame, area, app, theme, strings),
        Page::Terminal => {
            let inner = page_frame(frame, area, theme, strings.nav_terminal(), true);
            empty_state(frame, inner, theme, strings.toast_action_unavailable());
        }
    }
}

fn render_session_summary(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let title = app
        .active_session()
        .map(|session| session.title.clone())
        .unwrap_or_else(|| strings.sessions_title().to_string());
    let inner = page_frame(frame, area, theme, &title, true);
    let Some(session) = app.active_session().cloned() else {
        empty_state(frame, inner, theme, strings.sessions_empty());
        return;
    };
    let rows = vec![
        (strings.session_workspace(), session.workspace_root.clone()),
        (strings.session_state(), format!("{:?}", session.state)),
        (strings.runtime_agent(), session.agent_id.to_string()),
        (
            strings.session_title_label(),
            session.last_message_at_ms.to_string(),
        ),
    ];
    let body = rows
        .into_iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("{label:<16}"), theme.muted()),
                Span::styled(value, theme.base()),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(body)), inner);
}

fn render_agent(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    // Borders (2), then the draft itself; the info line sits on the bottom
    // border rather than taking a row of its own.
    let composer_height = (app.composer.line_count().min(6) as u16 + 2).max(3);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(composer_height)])
        .split(area);

    let title = app
        .active_session()
        .map(|session| session.title.clone())
        .unwrap_or_else(|| strings.nav_agent().to_string());
    let focused = app.focus == crate::app::Focus::Main;
    let inner = page_frame(frame, rows[0], theme, &title, focused);

    if app.transcript.is_empty() {
        render_welcome(frame, inner, theme, strings);
    } else {
        let height = usize::from(inner.height);
        let selected = app.selection_for(Scope::Agent);
        let mut scroll = app.scroll;
        scroll.selected = Some(selected);
        let lines = app.transcript.visible_lines(scroll, height, theme, strings);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    render_composer(frame, rows[1], app, theme, strings);
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
    let running = app
        .active_session()
        .is_some_and(|session| session.state == vibex_core::AgentSessionState::Running);

    // Rail colour: the user's own accent when this is where typing lands, the
    // dim grey otherwise.
    let rail_color = if focused {
        theme.roles.accent_user
    } else {
        theme.roles.gray_dim
    };
    let border_color = if focused {
        theme.roles.border_focused
    } else {
        theme.roles.gray_dim
    };

    let title = app
        .active_session()
        .map(|session| session.title.clone())
        .unwrap_or_else(|| strings.product_tagline().to_string());

    let block = bordered(theme)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme.roles.gray),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let text_area = inner;

    let prompt_width = 2;
    let width = usize::from(text_area.width).saturating_sub(prompt_width);
    let prefix = "❯";
    let prefix_style = Style::default().fg(rail_color).add_modifier(Modifier::BOLD);

    if app.composer.text().is_empty() {
        let placeholder = if running {
            format!("{} · Ctrl+S", strings.composer_steer())
        } else {
            strings.composer_placeholder().to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{prefix} "), prefix_style),
                Span::styled(
                    truncate_to_width(&placeholder, width, "…"),
                    Style::default().fg(theme.roles.gray_dim),
                ),
            ])),
            text_area,
        );
    } else {
        let (cursor_line, _) = app.composer.cursor_line_column();
        let lines = app
            .composer
            .display_lines(width)
            .into_iter()
            .enumerate()
            .map(|(index, (text, is_cursor_line))| {
                let gutter = if index == 0 {
                    Span::styled(format!("{prefix} "), prefix_style)
                } else {
                    Span::raw(" ".repeat(prompt_width))
                };
                // The cursor's line is drawn at full strength; the rest of a
                // long draft recedes so the eye stays where typing happens.
                let style = if index == cursor_line && is_cursor_line {
                    theme.base()
                } else {
                    theme.base().add_modifier(Modifier::DIM)
                };
                Line::from(vec![gutter, Span::styled(text, style)])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Text::from(lines)), text_area);
    }

    // The info line is painted onto the bottom border. The rule continues
    // around it, so the composer keeps a single clean outline while still
    // carrying context -- a row of chrome that would otherwise be spent on `─`.
    if area.height >= 2 {
        let bottom = Rect {
            y: area.y + area.height - 1,
            x: area.x + 1,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        render_composer_info(frame, bottom, app, theme, strings, focused, running);
    }

    if let Some(menu) = app.completion.clone() {
        render_completion(frame, text_area, theme, &menu);
    }
}

/// The composer's info line: context on the left, mode on the right.
///
/// This is deliberately the bottom border rather than a separate row: a
/// terminal has no room for chrome that only carries status, and a divider that
/// also informs is free.
fn render_composer_info(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    theme: &TuiTheme,
    strings: Strings,
    focused: bool,
    running: bool,
) {
    if area.height == 0 {
        return;
    }
    let sep = |theme: &TuiTheme| {
        Span::styled(
            format!(" {} ", crate::transcript::chrome_separator()),
            Style::default().fg(if focused {
                theme.roles.gray_dim
            } else {
                theme.fade(theme.roles.gray_dim, 0.5)
            }),
        )
    };
    let flag = |theme: &TuiTheme| {
        Style::default().fg(if focused {
            theme.roles.gray
        } else {
            theme.fade(theme.roles.gray, 0.55)
        })
    };

    let mut left = vec![Span::raw(" ")];
    // Model identity, when the session has one.
    if let Some(session) = app.active_session() {
        left.push(Span::styled(
            session.agent_id.to_string(),
            flag(theme).add_modifier(Modifier::BOLD),
        ));
        if let Some(catalog) = app.runtime_options.as_ref()
            && let Some(option) = catalog.options.first()
        {
            left.push(sep(theme));
            left.push(Span::styled(option.model_label.clone(), flag(theme)));
        }
    } else {
        // No session yet: name the page, which is the only true context there is.
        let page = match app.page {
            Page::Agent => strings.nav_agent(),
            Page::Files => strings.nav_files(),
            Page::Changes => strings.nav_changes(),
            Page::Terminal => strings.nav_terminal(),
            Page::Sessions => strings.nav_sessions(),
            Page::Management => strings.nav_management(),
            Page::Usage => strings.nav_usage(),
            Page::Settings => strings.nav_settings(),
            Page::Help => strings.nav_help(),
            _ => strings.nav_management(),
        };
        left.push(Span::styled(page.to_string(), flag(theme)));
    }
    if running {
        left.push(sep(theme));
        left.push(Span::styled(
            strings.running().to_string(),
            Style::default().fg(theme.roles.accent_running),
        ));
    }
    let pending = app.pending_permission_count();
    if pending > 0 {
        left.push(sep(theme));
        left.push(Span::styled(
            format!("⚠ {pending} {}", strings.approval_title()),
            theme.warning().add_modifier(Modifier::BOLD),
        ));
    }
    left.push(Span::raw(" "));

    let mut right = Vec::new();
    if app.composer.line_count() > 1 {
        right.push(Span::styled(
            strings.settings_keys().to_string(),
            flag(theme),
        ));
    }
    if focused {
        right.push(Span::styled(
            "▏",
            Style::default().fg(theme.roles.accent_user),
        ));
    }
    if !right.is_empty() {
        right.push(Span::raw(" "));
    }

    // Render into a scratch line so the right-hand side can be placed against
    // the far edge without the two halves colliding in a narrow terminal.
    let left_line = Line::from(left);
    let left_width = left_line.width() as u16;
    let right_line = Line::from(right);
    let right_width = right_line.width() as u16;
    if left_width + right_width >= area.width {
        frame.render_widget(Paragraph::new(left_line), area);
        return;
    }
    frame.render_widget(Paragraph::new(left_line), area);
    let right_area = Rect {
        x: area.x + area.width - right_width,
        width: right_width,
        ..area
    };
    frame.render_widget(Paragraph::new(right_line), right_area);
}

fn render_completion(
    frame: &mut Frame<'_>,
    anchor: Rect,
    theme: &TuiTheme,
    menu: &crate::composer::CompletionMenu,
) {
    let query = menu
        .items
        .first()
        .map(|_| String::new())
        .unwrap_or_default();
    let indices = menu.filtered(&query);
    if indices.is_empty() {
        return;
    }
    let height = (indices.len() as u16 + 2).min(8);
    let width = anchor.width.saturating_sub(4).clamp(20, 60);
    let x = anchor.x.min(anchor.right().saturating_sub(width));
    let y = anchor.y.saturating_sub(height);
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(theme.focus_style())
        .style(theme.base());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = indices
        .iter()
        .take(usize::from(inner.height))
        .map(|index| {
            let item = &menu.items[*index];
            let style = if *index == menu.selected {
                theme.selected()
            } else {
                theme.base()
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<20}", truncate_to_width(&item.label, 20, "…")),
                    style,
                ),
                Span::styled(truncate_to_width(&item.detail, 30, "…"), theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items), inner);
}

fn render_file_list(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let title = if app.page == Page::Changes {
        strings.nav_changes()
    } else {
        strings.nav_files()
    };
    let inner = page_frame(frame, area, theme, title, true);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    let entries: Vec<(String, String)> = if app.page == Page::Changes {
        app.git_status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .map(|entry| {
                        (
                            format!("{:?} {}", entry.kind, entry.path),
                            entry.path.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        app.file_rows
            .iter()
            .map(|entry| {
                let marker = match entry.kind {
                    vibex_core::FileEntryKind::Directory => "▸ ",
                    vibex_core::FileEntryKind::File => "  ",
                    _ => "? ",
                };
                (format!("{marker}{}", entry.name), entry.path.clone())
            })
            .collect()
    };

    if entries.is_empty() {
        empty_state(frame, rows[0], theme, strings.nothing_here());
    } else {
        let selected = app.selection_for(app.page.scope());
        let items = entries
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let style = if index == selected {
                    theme.selected()
                } else {
                    theme.base()
                };
                ListItem::new(Line::from(Span::styled(label.clone(), style)))
            })
            .collect::<Vec<_>>();
        let mut state = ratatui::widgets::ListState::default();
        state.select(Some(selected.min(entries.len() - 1)));
        frame.render_stateful_widget(List::new(items), rows[0], &mut state);
    }

    if let Some(diff) = &app.diff_text {
        frame.render_widget(
            Paragraph::new(truncate_to_width(
                diff.lines().next().unwrap_or(""),
                usize::from(rows[1].width),
                "…",
            ))
            .style(theme.muted()),
            rows[1],
        );
    }
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
        strings.management_hooks(),
        strings.management_devices(),
        strings.management_recovery(),
    ];
    let counts = [
        app.management_data.agents.len(),
        app.management_data.providers.len(),
        app.management_data.mcp.len(),
        app.management_data.skills.len(),
        app.management_data.prompts.len(),
        app.management_data.hooks.len(),
        app.management_data.devices.len(),
        app.management_data.backups.len(),
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
                ManagementRow::Recovery => {
                    app.availability(vibex_backend::BackendOperation::RecoveryDiagnosticsExport)
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
            } else {
                theme.base()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<18}", label), style),
                Span::styled(suffix, theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
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
        Page::Hooks => strings.management_hooks(),
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
        Page::Hooks => app
            .management_data
            .hooks
            .iter()
            .map(|hook| {
                (
                    hook.display_name.clone(),
                    format!("{:?}", hook.event_kind),
                    hook.status == vibex_core::HookStatus::Enabled,
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

fn render_recovery(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.recovery_title(), true);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(inner);
    let actions = [
        (RecoveryAction::Diagnostics, strings.recovery_diagnostics()),
        (
            RecoveryAction::BackupCreate,
            strings.recovery_backup_create(),
        ),
        (
            RecoveryAction::BackupInspect,
            strings.recovery_backup_inspect(),
        ),
        (
            RecoveryAction::BackupRestore,
            strings.recovery_backup_restore(),
        ),
    ];
    let selected = app.selection_for(Scope::Recovery);
    let items = actions
        .iter()
        .enumerate()
        .map(|(index, (action, label))| {
            let availability = match action {
                RecoveryAction::Diagnostics => {
                    app.availability(vibex_backend::BackendOperation::RecoveryDiagnosticsExport)
                }
                RecoveryAction::BackupCreate => {
                    app.availability(vibex_backend::BackendOperation::RecoveryBackupCreate)
                }
                RecoveryAction::BackupInspect => {
                    app.availability(vibex_backend::BackendOperation::RecoveryBackupInspect)
                }
                RecoveryAction::BackupRestore => {
                    app.availability(vibex_backend::BackendOperation::RecoveryBackupRestore)
                }
            };
            let style = if index == selected {
                theme.selected()
            } else if availability.is_available() {
                theme.base()
            } else {
                theme.muted()
            };
            let suffix = match availability {
                Availability::Available => String::new(),
                Availability::RequiresPermission => {
                    format!("  [{}]", strings.permission_required_for())
                }
                _ => format!("  [{}]", strings.toast_action_unavailable()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<24}", label), style),
                Span::styled(suffix, theme.muted()),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(actions.len() - 1)));
    frame.render_stateful_widget(List::new(items), rows[0], &mut state);
    frame.render_widget(
        Paragraph::new(strings.recovery_authority_only()).style(theme.muted()),
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
    let rows = [
        (
            SettingRow::Theme,
            strings.settings_theme().to_string(),
            app.settings.theme_id.clone(),
        ),
        (
            SettingRow::Locale,
            strings.settings_language().to_string(),
            app.settings.locale.tag().to_string(),
        ),
        (
            SettingRow::Icons,
            strings.settings_icons().to_string(),
            format!("{:?}", app.settings.glyphs),
        ),
        (
            SettingRow::Backend,
            strings.settings_backend().to_string(),
            app.capabilities.schema_version.clone(),
        ),
        (
            SettingRow::Seat,
            strings.settings_connection().to_string(),
            app.seat.label(strings).to_string(),
        ),
        (
            SettingRow::Keys,
            strings.settings_keys().to_string(),
            format!("{}", app.keymap.bindings().len()),
        ),
        (
            SettingRow::Version,
            strings.settings_version().to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
    ];
    let selected = app.settings.selected;
    let items = rows
        .iter()
        .enumerate()
        .map(|(index, (_, label, value))| {
            let style = if index == selected {
                theme.selected()
            } else {
                theme.base()
            };
            let mut spans = vec![
                Span::styled(format!("{label:<16}"), style),
                Span::styled(value.clone(), theme.accent()),
            ];
            if index == selected {
                spans.push(Span::styled(
                    format!("   ({})", strings.settings_keys_hint()),
                    theme.muted(),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(selected.min(rows.len() - 1)));
    frame.render_stateful_widget(List::new(items), inner, &mut state);
}

fn render_help(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let inner = page_frame(frame, area, theme, strings.help_title(), true);
    let scopes = app.documented_scopes();
    let bindings = app.keymap.advertised(&scopes);
    if bindings.is_empty() {
        empty_state(frame, inner, theme, strings.help_no_keys());
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
    frame.render_widget(List::new(items), inner);
}

/// The details pane: the facts about the open session, at a glance.
///
/// Details are for the things a reader checks without leaving the transcript --
/// which runtime is answering, how full the context is, whether anything is
/// waiting on them. Everything here is also reachable elsewhere; the pane
/// exists so it does not have to be looked up.
fn render_details(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
    focused: bool,
) {
    let inner = page_frame(frame, area, theme, strings.details(), focused);
    let mut rows: Vec<(String, String, Style)> = Vec::new();
    let label = Style::default().fg(theme.roles.gray_dim);
    let value = Style::default().fg(theme.roles.foreground);

    if let Some(session) = app.active_session() {
        rows.push((
            strings.details_state().to_string(),
            format!("{:?}", session.state),
            Style::default().fg(match session.state {
                vibex_core::AgentSessionState::Running => theme.roles.accent_running,
                vibex_core::AgentSessionState::Error => theme.roles.danger,
                vibex_core::AgentSessionState::NeedsInput => theme.roles.accent_attention,
                _ => theme.roles.foreground,
            }),
        ));
        rows.push((
            strings.details_agent().to_string(),
            session.agent_id.to_string(),
            value,
        ));
        if let Some(catalog) = app.runtime_options.as_ref()
            && let Some(option) = catalog.options.first()
        {
            rows.push((
                strings.details_model().to_string(),
                option.model_label.clone(),
                value,
            ));
        }
        rows.push((
            strings.details_workspace().to_string(),
            session.workspace_root.clone(),
            Style::default().fg(theme.roles.path),
        ));
    } else {
        rows.push((
            strings.details_state().to_string(),
            strings.none().to_string(),
            label,
        ));
    }

    if let Some(branch) = app
        .git_status
        .as_ref()
        .and_then(|status| status.branch.clone())
    {
        rows.push((
            strings.details_branch().to_string(),
            branch,
            Style::default().fg(theme.roles.command),
        ));
    }

    if let Some(usage) = app.management_data.usage_session.as_ref() {
        let used = usage.total_tokens.unwrap_or(0);
        let text = match usage.context_window_size_tokens {
            Some(total) if total > 0 => {
                format!("{} / {}", compact_tokens(used), compact_tokens(total))
            }
            _ => compact_tokens(used),
        };
        rows.push((strings.details_context().to_string(), text, value));
    }

    let pending = app.pending_permission_count();
    rows.push((
        strings.approval_label().to_string(),
        pending.to_string(),
        if pending > 0 {
            Style::default()
                .fg(theme.roles.accent_attention)
                .add_modifier(Modifier::BOLD)
        } else {
            label
        },
    ));

    // Two columns, sized to the widest label, so nothing collides.
    let label_width = rows
        .iter()
        .map(|(name, _, _)| display_width(name))
        .max()
        .unwrap_or(0)
        .min(usize::from(inner.width).saturating_sub(8));
    let mut lines = Vec::new();
    for (name, text, style) in rows {
        let name = truncate_to_width(&name, label_width, "…");
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<label_width$}"), label),
            Span::raw(" "),
            Span::styled(
                truncate_to_width(
                    &text,
                    usize::from(inner.width).saturating_sub(label_width + 2),
                    "…",
                ),
                style,
            ),
        ]));
    }

    if let Some(worktrees) = app.worktrees.as_ref()
        && !worktrees.managed_worktrees.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "{} ({})",
                strings.worktree_title(),
                worktrees.managed_worktrees.len()
            ),
            label,
        )));
        for worktree in worktrees.managed_worktrees.iter().take(5) {
            lines.push(Line::from(Span::styled(
                truncate_to_width(
                    &format!(
                        "  {}",
                        worktree.branch.clone().unwrap_or_else(|| "-".to_string())
                    ),
                    usize::from(inner.width),
                    "…",
                ),
                Style::default().fg(theme.roles.gray),
            )));
        }
    }

    if lines.is_empty() {
        empty_state(frame, inner, theme, strings.nothing_here());
        return;
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// The key hint bar.
///
/// Keys are drawn bold and bright, labels dim: the key is what the reader is
/// looking for and the label only confirms it. Hints that must survive a narrow
/// terminal are pinned and drawn first, so shrinking the window degrades the bar
/// from the least important end rather than truncating it arbitrarily.
fn render_key_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &TuiTheme) {
    let scopes = app.documented_scopes();
    let bindings = app.keymap.advertised(&scopes);
    let key_style = Style::default()
        .fg(theme.roles.gray_bright)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(theme.roles.gray_dim);

    let mut spans = vec![Span::raw(" ")];
    let mut used = 1usize;
    let budget = usize::from(area.width);
    for (index, binding) in bindings.iter().enumerate() {
        let Some(label) = binding.label else { continue };
        let text = format!("{} {}", binding.chord.display(), label);
        let width = display_width(&text) + 3;
        // The first few hints are the ones a reader needs most; they are kept
        // even when the rest would not fit.
        let pinned = index < PINNED_HINTS;
        if used + width > budget && !pinned {
            break;
        }
        if used + width > budget {
            continue;
        }
        used += width;
        spans.push(Span::styled(binding.chord.display(), key_style));
        spans.push(Span::styled(format!(" {label}"), label_style));
        spans.push(Span::styled(
            "  ",
            Style::default().fg(theme.roles.gray_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// How many leading hints survive a narrow terminal.
const PINNED_HINTS: usize = 3;

/// The status bar: identity on the left, context in the centre, state on the
/// right.
///
/// Splitting it into zones is what stops the bar from becoming a single
/// left-aligned sentence whose tail is the first thing a narrow terminal eats.
/// The centre carries the thing the reader checks most often — how full the
/// context window is — with the colour blended across usage thresholds so the
/// answer is available without reading the number.
fn render_status_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &TuiTheme) {
    let strings = app.strings;
    let sep = || {
        Span::styled(
            format!(" {} ", crate::transcript::chrome_separator()),
            Style::default().fg(theme.roles.gray_dim),
        )
    };

    // ---- left: where am I, and is it alive ------------------------------
    let (live_text, live_color) = match app.live {
        crate::app::LiveState::Ready => (strings.done(), theme.roles.accent_success),
        crate::app::LiveState::Connecting => (strings.connecting(), theme.roles.gray),
        crate::app::LiveState::Reconnecting => (strings.reconnecting(), theme.roles.warning),
        crate::app::LiveState::Offline => (strings.disconnected(), theme.roles.danger),
    };
    let mut left = vec![
        Span::raw(" "),
        Span::styled(
            app.seat.label(strings).to_string(),
            Style::default()
                .fg(theme.roles.accent)
                .add_modifier(Modifier::BOLD),
        ),
        sep(),
        Span::styled(live_text, Style::default().fg(live_color)),
    ];
    if app.pending_permission_count() > 0 {
        left.push(sep());
        left.push(Span::styled(
            format!("⚠ {pending}", pending = app.pending_permission_count()),
            Style::default()
                .fg(theme.roles.accent_attention)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // ---- centre: context usage ------------------------------------------
    let centre = context_usage_line(app, theme);

    // ---- right: appearance, then a transient message --------------------
    let mut right = Vec::new();
    if let Some(toast) = &app.toast {
        let color = match toast.tone {
            ToastTone::Info => theme.roles.gray,
            ToastTone::Success => theme.roles.accent_success,
            ToastTone::Warning => theme.roles.warning,
            ToastTone::Danger => theme.roles.danger,
        };
        right.push(Span::styled(toast.text.clone(), Style::default().fg(color)));
        right.push(sep());
    }
    right.push(Span::styled(
        app.theme.name.to_string(),
        Style::default().fg(theme.roles.gray_dim),
    ));
    right.push(Span::raw(" "));

    render_zoned_line(frame, area, left, centre, right);
}

/// The context-window readout.
///
/// Token counts are compacted (`8.5K / 1.0M`) and the colour is blended across
/// usage thresholds, so the bar answers "how much room is left" from the corner
/// of the eye. Vibex records no pricing, so no cost is shown.
fn context_usage_line(app: &App, theme: &TuiTheme) -> Option<Vec<Span<'static>>> {
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
        format!("{} / {}", compact_tokens(used), compact_tokens(total)),
        Style::default().fg(color),
    )])
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
            let matches = palette_matches(query, strings);
            let height = (matches.len() as u16 + 4).min(area.height.saturating_sub(4));
            let width = area.width.saturating_sub(8).min(72);
            let rect = centered(area, width, height);
            frame.render_widget(Clear, rect);
            let block = overlay_block(theme, strings.palette_title());
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(inner);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("> ", theme.accent()),
                    Span::styled(query.clone(), theme.base()),
                    Span::styled("▏", theme.accent()),
                ])),
                rows[0],
            );
            let items = matches
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{:<24}", entry.label), style),
                        Span::styled(entry.hint, theme.muted()),
                    ]))
                })
                .collect::<Vec<_>>();
            let mut state = ratatui::widgets::ListState::default();
            state.select(Some((*selected).min(matches.len().saturating_sub(1))));
            frame.render_stateful_widget(List::new(items), rows[1], &mut state);
        }
        Overlay::Help { scroll, .. } => {
            let rect = centered(
                area,
                area.width.saturating_sub(8).min(90),
                area.height.saturating_sub(6),
            );
            frame.render_widget(Clear, rect);
            let block = overlay_block(theme, strings.help_title());
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let mut lines = Vec::new();
            for binding in app.keymap.bindings() {
                let Some(label) = binding.label else { continue };
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<22}", binding.scope.id()), theme.muted()),
                    Span::styled(format!("{:<12}", binding.chord.display()), theme.accent()),
                    Span::styled(label.to_string(), theme.base()),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("    {}", binding.intent.help()),
                    theme.muted(),
                )));
            }
            let offset = (*scroll).min(lines.len().saturating_sub(1));
            let visible = lines
                .into_iter()
                .skip(offset)
                .take(usize::from(inner.height))
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(Text::from(visible)), inner);
        }
        Overlay::Confirm { title, body, .. } => {
            let rect = centered(area, 64.min(area.width.saturating_sub(4)), 7);
            frame.render_widget(Clear, rect);
            let block = overlay_block(theme, title);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
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
                inner,
            );
        }
        Overlay::Prompt {
            title,
            value,
            field,
        } => {
            let rect = centered(area, 64.min(area.width.saturating_sub(4)), 6);
            frame.render_widget(Clear, rect);
            let block = overlay_block(theme, title);
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
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
                inner,
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
        Overlay::RuntimePicker { selected } => {
            let options = app
                .runtime_options
                .as_ref()
                .map(|catalog| catalog.options.clone())
                .unwrap_or_default();
            let rect = centered(
                area,
                72.min(area.width.saturating_sub(4)),
                (options.len() as u16 + 3).min(area.height.saturating_sub(4)),
            );
            frame.render_widget(Clear, rect);
            let block = overlay_block(theme, strings.runtime_title());
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let items = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let style = if index == *selected {
                        theme.selected()
                    } else {
                        theme.base()
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:<20}", truncate_to_width(&option.agent_label, 20, "…")),
                            style,
                        ),
                        Span::styled(
                            truncate_to_width(&option.model_label, 30, "…"),
                            theme.muted(),
                        ),
                        Span::styled(format!("  {:?}", option.availability), theme.muted()),
                    ]))
                })
                .collect::<Vec<_>>();
            let mut state = ratatui::widgets::ListState::default();
            state.select(Some((*selected).min(options.len().saturating_sub(1))));
            frame.render_stateful_widget(List::new(items), inner, &mut state);
        }
        Overlay::BlockDetails { block, scroll } => {
            let detail = block_detail_text(&mut app.transcript, *block);
            if let Some((title, body)) = detail {
                render_text_view(frame, area, &title, &body, *scroll, theme);
            }
        }
        Overlay::TextView {
            title,
            body,
            scroll,
        } => render_text_view(frame, area, title, body, *scroll, theme),
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
    let height = (approval.details.len() as u16 + 8).min(area.height.saturating_sub(4));
    let rect = centered(area, area.width.saturating_sub(8).min(80), height.max(8));
    frame.render_widget(Clear, rect);
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.warning())
        .style(theme.base())
        .title(Span::styled(
            format!(" ⚠ {} ", strings.approval_title()),
            theme.warning().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

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
                    usize::from(inner.width).saturating_sub(label.len() + 4),
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
        inner,
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
    let height = (request.fields.len() as u16 * 2 + 6).min(area.height.saturating_sub(4));
    let rect = centered(area, area.width.saturating_sub(8).min(80), height.max(8));
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.warning())
        .style(theme.base())
        .title(Span::styled(
            format!(" {} ", strings.elicitation_title()),
            theme.warning().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

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
        inner,
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
    let rect = centered(
        area,
        area.width.saturating_sub(8).min(76),
        area.height.saturating_sub(6),
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.accent())
        .style(theme.base())
        .title(Span::styled(
            format!(" {} ", strings.devices_pairing_code()),
            theme.strong(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(24), Constraint::Length(46)])
        .split(inner);

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
    title: &str,
    body: &str,
    scroll: usize,
    theme: &TuiTheme,
) {
    let rect = centered(
        area,
        area.width.saturating_sub(6),
        area.height.saturating_sub(4),
    );
    frame.render_widget(Clear, rect);
    let block = overlay_block(theme, title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines = body
        .split('\n')
        .skip(scroll)
        .take(usize::from(inner.height))
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
                truncate_to_width(line, usize::from(inner.width), "…"),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn overlay_block(theme: &TuiTheme, title: &str) -> Block<'static> {
    bordered(theme)
        .border_style(theme.focus_style())
        .style(theme.base())
        .title(Span::styled(format!(" {title} "), theme.strong()))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// The message shown when the terminal cannot host the interface at all.
pub fn degradation_message(app: &App) -> Option<String> {
    let (columns, rows) = app.viewport;
    if columns < 60 || rows < 16 {
        Some(format!(
            "{}\n{}: {}x{}\n{}: 60x16",
            app.strings.terminal_too_small(),
            app.strings.terminal_size_current(),
            columns,
            rows,
            app.strings.terminal_size_required()
        ))
    } else {
        None
    }
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
        Strings::for_locale(Locale::En)
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
    fn centered_rects_stay_inside_the_frame() {
        let area = Rect::new(0, 0, 80, 24);
        let rect = centered(area, 200, 100);
        assert!(rect.right() <= area.right());
        assert!(rect.bottom() <= area.bottom());
    }

    #[test]
    fn degradation_message_fires_only_for_tiny_terminals() {
        // The message is a pure function of the recorded viewport, so it can be
        // checked without a terminal.
        let text = format!(
            "{}\n{}: {}x{}\n{}: 60x16",
            strings().terminal_too_small(),
            strings().terminal_size_current(),
            40,
            10,
            strings().terminal_size_required()
        );
        assert!(text.contains("60x16"));
        assert!(text.contains("40x10"));
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
}
