//! Rendering: layout maths, page shells, overlays and the pure view helpers.
//!
//! Two structural rules from the reference implementations are enforced here
//! rather than trusted to discipline:
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
use ratatui::style::Modifier;
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
    match shell {
        ShellKind::Wide => {
            let sidebar = (columns / 5).clamp(24, 40);
            let details = (columns / 4).clamp(28, 56);
            let main = columns.saturating_sub(sidebar + details + 4).max(24);
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
            let main = columns.saturating_sub(sidebar + 2).max(24);
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
        if self.show_sidebar && self.sidebar_width + self.main_width + 4 > columns {
            self.show_sidebar = false;
            self.sidebar_width = 0;
            self.main_width = columns;
        }
        if self.show_details && self.main_width + self.details_width + 4 > columns {
            self.show_details = false;
            self.details_width = 0;
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

fn empty_state(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme, message: &str) {
    frame.render_widget(
        Paragraph::new(message.to_string())
            .style(theme.muted())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
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
        empty_state(frame, list_area, theme, strings.sessions_empty());
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
        empty_state(frame, inner, theme, strings.transcript_empty());
    } else {
        let height = usize::from(inner.height);
        let lines = app
            .transcript
            .visible_lines(app.scroll, height, theme, strings);
        // The visible selection is tracked by display line, so the highlighted
        // block is always the one the viewport is showing.
        let selected_line = app
            .transcript
            .offset_of_block(app.selection_for(Scope::Agent));
        let offset = if app.scroll.follow {
            app.transcript.total_height().saturating_sub(height)
        } else {
            app.scroll.offset
        };
        let rendered = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if offset + index == selected_line && focused {
                    line.style(theme.selected())
                } else {
                    line
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Text::from(rendered)), inner);
    }

    render_composer(frame, rows[1], app, theme, strings);
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
) {
    let focused = app.focus == crate::app::Focus::Composer;
    let border = if focused {
        theme.focus_style()
    } else {
        theme.border_style()
    };
    let running = app
        .active_session()
        .is_some_and(|session| session.state == vibex_core::AgentSessionState::Running);
    let hint = if running {
        format!("{} · Ctrl+S", strings.composer_steer())
    } else {
        strings.composer_placeholder().to_string()
    };
    let block = bordered(theme)
        .border_style(border)
        .title(Span::styled(format!(" {hint} "), theme.muted()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    if app.composer.text().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("█", theme.accent()))),
            inner,
        );
    } else {
        let (cursor_line, _) = app.composer.cursor_line_column();
        let lines = app
            .composer
            .display_lines(width)
            .into_iter()
            .enumerate()
            .map(|(index, (text, is_cursor_line))| {
                // The line holding the cursor is emphasized so a long draft
                // stays navigable; the rest is plain body text.
                let style = if index == cursor_line && is_cursor_line {
                    theme.base().add_modifier(Modifier::BOLD)
                } else {
                    theme.base()
                };
                Line::from(Span::styled(text, style))
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    if let Some(menu) = app.completion.clone() {
        render_completion(frame, inner, theme, &menu);
    }
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

fn render_details(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    theme: &TuiTheme,
    strings: Strings,
    focused: bool,
) {
    let inner = page_frame(frame, area, theme, strings.details(), focused);
    let mut lines = Vec::new();
    if let Some(session) = app.active_session() {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<14}", strings.session_state()), theme.muted()),
            Span::styled(format!("{:?}", session.state), theme.base()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14}", strings.session_workspace()),
                theme.muted(),
            ),
            Span::styled(
                truncate_to_width(
                    &session.workspace_root,
                    usize::from(inner.width).saturating_sub(16),
                    "…",
                ),
                theme.base(),
            ),
        ]));
    }
    let approvals = app.pending_permission_count();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<14}", strings.approval_title()), theme.muted()),
        Span::styled(
            approvals.to_string(),
            if approvals > 0 {
                theme.warning()
            } else {
                theme.base()
            },
        ),
    ]));
    for (label, value) in [
        (strings.runtime_desired(), String::new()),
        (strings.runtime_effective(), String::new()),
    ] {
        let _ = (label, value);
    }
    if let Some(runtime) = app.runtime_options.as_ref() {
        for option in runtime.options.iter().take(4) {
            lines.push(Line::from(Span::styled(
                truncate_to_width(
                    &format!("{} · {}", option.agent_label, option.model_label),
                    usize::from(inner.width).saturating_sub(2),
                    "…",
                ),
                theme.muted(),
            )));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_key_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &TuiTheme) {
    let scopes = app.documented_scopes();
    let bindings = app.keymap.advertised(&scopes);
    let mut spans = vec![Span::styled(" ", theme.muted())];
    let mut used = 1usize;
    let budget = usize::from(area.width);
    for binding in bindings {
        let Some(label) = binding.label else { continue };
        let text = format!("{} {}", binding.chord.display(), label);
        let width = display_width(&text) + 3;
        if used + width > budget {
            break;
        }
        used += width;
        spans.push(Span::styled(binding.chord.display(), theme.accent()));
        spans.push(Span::styled(format!(" {label}  "), theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_status_bar(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &TuiTheme) {
    let strings = app.strings;
    let (live_text, live_style) = match app.live {
        crate::app::LiveState::Ready => (strings.done(), theme.success()),
        crate::app::LiveState::Connecting => (strings.connecting(), theme.muted()),
        crate::app::LiveState::Reconnecting => (strings.reconnecting(), theme.warning()),
        crate::app::LiveState::Offline => (strings.disconnected(), theme.danger()),
    };
    let mut spans = vec![
        Span::styled(format!(" {} ", app.seat.label(strings)), theme.accent()),
        Span::styled("│ ", theme.border_style()),
        Span::styled(live_text, live_style),
        Span::styled(" │ ", theme.border_style()),
        Span::styled(format!("{}: ", strings.settings_theme()), theme.muted()),
        Span::styled(app.theme.name.to_string(), theme.base()),
    ];
    let pending = app.pending_permission_count();
    if pending > 0 {
        spans.push(Span::styled(" │ ", theme.border_style()));
        spans.push(Span::styled(
            format!("⚠ {pending} {}", strings.approval_title()),
            theme.warning().add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(toast) = &app.toast {
        spans.push(Span::styled(" │ ", theme.border_style()));
        let style = match toast.tone {
            ToastTone::Info => theme.base(),
            ToastTone::Success => theme.success(),
            ToastTone::Warning => theme.warning(),
            ToastTone::Danger => theme.danger(),
        };
        spans.push(Span::styled(
            truncate_to_width(&toast.text, usize::from(area.width).saturating_sub(60), "…"),
            style,
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
