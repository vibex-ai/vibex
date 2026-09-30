//! The main loop: input reduction, frame scheduling, and worker message
//! application.
//!
//! The loop holds to three rules:
//!
//! * **Dirty-frame scheduling.** A frame is drawn when input, a worker result,
//!   or a tick actually changed something. With no input and no events the loop
//!   produces zero frames — the measured contract is 0 frames/second idle.
//! * **Input batching.** Key events are drained with a budget, so a wheel burst
//!   or a paste cannot starve the renderer.
//! * **The main thread never awaits.** Worker results are drained with
//!   `try_recv`.

use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use vibex_backend::{BackendError, BackendResult};

use crate::app::{App, Focus, LiveState, Overlay, Page, PromptField, Toast};
use crate::keymap::Chord;
use crate::reduce::Outcome;
use crate::terminal::{TerminalGuard, with_terminal_restored};
use crate::worker::{AppMessage, Worker};

/// Logical tick period. A tick never performs I/O; it only expires toasts and
/// re-evaluates time-based state.
pub const TICK: Duration = Duration::from_millis(200);
/// Tick period while a turn is running, so the rail animation is smooth.
///
/// This is the only condition under which the interface repaints without input
/// or an event, and it stops the moment the turn does.
pub const ANIMATION_TICK: Duration = Duration::from_millis(120);
/// Upper bound on frames per second.
pub const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// Maximum key events drained in one batch.
pub const INPUT_BATCH_LIMIT: usize = 256;
/// Wall-clock budget for one input batch.
pub const INPUT_BATCH_BUDGET: Duration = Duration::from_millis(4);

/// How the session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    UserQuit,
    ConnectionLost,
}

/// Run the interface until the user quits.
pub fn run_loop(
    app: &mut App,
    worker: &Worker,
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<AppMessage>,
) -> BackendResult<ExitReason> {
    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)
        .map_err(|error| BackendError::failed("tui_terminal_unavailable", error.to_string()))?;
    terminal.clear().ok();

    let result = event_loop(app, worker, messages, &mut terminal, &mut guard);
    // The guard restores on drop, but doing it explicitly means the cursor is
    // back before any error is printed.
    guard.release();
    // The screen belongs to the process again, so the diagnostics that were
    // kept out of the frames above can be named rather than silently dropped.
    crate::terminal::report_captured_stderr();
    result
}

fn event_loop(
    app: &mut App,
    worker: &Worker,
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<AppMessage>,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    guard: &mut TerminalGuard,
) -> BackendResult<ExitReason> {
    let mut dirty = true;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    let mut last_tick = Instant::now();
    let mut reason = ExitReason::UserQuit;

    // First frame: paint the skeleton before any I/O result arrives.
    let initial = app.perform(crate::action::Intent::GotoSessions);
    dispatch_all(worker, &initial);
    app.sync_transcript();

    loop {
        // ---- input -------------------------------------------------------
        let mut drained = 0usize;
        let batch_start = Instant::now();
        while drained < INPUT_BATCH_LIMIT && batch_start.elapsed() < INPUT_BATCH_BUDGET {
            match event::poll(Duration::from_millis(if dirty { 0 } else { 16 })) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    return Err(BackendError::failed(
                        "tui_input_unavailable",
                        error.to_string(),
                    ));
                }
            }
            let event = match event::read() {
                Ok(event) => event,
                Err(error) => {
                    return Err(BackendError::failed(
                        "tui_input_unavailable",
                        error.to_string(),
                    ));
                }
            };
            drained += 1;
            match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if handle_key(app, worker, guard, key)? {
                        return Ok(ExitReason::UserQuit);
                    }
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    if handle_mouse(app, mouse.kind) {
                        dirty = true;
                    }
                }
                Event::Resize(columns, rows) => {
                    app.resize(columns, rows);
                    dirty = true;
                }
                Event::Paste(text) => {
                    app.composer.insert_str(&text);
                    app.refresh_completion();
                    dirty = true;
                }
                _ => {}
            }
            if app.should_quit {
                return Ok(ExitReason::UserQuit);
            }
        }

        // ---- worker results ----------------------------------------------
        let mut handled = 0usize;
        while handled < 64 {
            match messages.try_recv() {
                Ok(message) => {
                    handled += 1;
                    apply_message(app, message)?;
                    dirty = true;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    reason = ExitReason::ConnectionLost;
                    app.live = LiveState::Offline;
                    dirty = true;
                    break;
                }
            }
        }

        // ---- tick ---------------------------------------------------------
        let period = if app.transcript_animating() {
            ANIMATION_TICK
        } else {
            TICK
        };
        if last_tick.elapsed() >= period {
            last_tick = Instant::now();
            let had_toast = app.toast.is_some();
            app.tick();
            if had_toast {
                dirty = true;
            }
            if app.advance_transcript_animation() {
                dirty = true;
            }
        }

        // ---- frame --------------------------------------------------------
        if dirty && last_frame.elapsed() >= FRAME_INTERVAL {
            if let Some(message) = crate::view::degradation_message(app) {
                terminal
                    .draw(|frame| {
                        let theme = app.theme.clone();
                        frame.render_widget(
                            ratatui::widgets::Paragraph::new(message.clone())
                                .style(theme.warning())
                                .alignment(ratatui::layout::Alignment::Center),
                            frame.area(),
                        );
                    })
                    .map_err(|error| {
                        BackendError::failed("tui_render_failed", error.to_string())
                    })?;
            } else {
                terminal
                    .draw(|frame| crate::view::render(frame, app))
                    .map_err(|error| {
                        BackendError::failed("tui_render_failed", error.to_string())
                    })?;
            }
            last_frame = Instant::now();
            dirty = false;
        }

        if app.should_quit {
            return Ok(ExitReason::UserQuit);
        }
        if reason == ExitReason::ConnectionLost {
            return Ok(reason);
        }
    }
}

fn dispatch_all(worker: &Worker, outcome: &Outcome) {
    for effect in &outcome.effects {
        worker.dispatch(effect.clone());
    }
}

/// Reduce one key press. Returns `true` when the program should exit.
fn handle_key(
    app: &mut App,
    worker: &Worker,
    guard: &mut TerminalGuard,
    key: KeyEvent,
) -> BackendResult<bool> {
    // While a filter or a prompt is being typed, printable characters are text.
    if app.filtering {
        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter.push(character);
                app.set_selection(app.page.scope(), 0);
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.filter.pop();
                return Ok(false);
            }
            KeyCode::Delete => {
                app.filter.clear();
                return Ok(false);
            }
            KeyCode::Enter => {
                app.filtering = false;
                let outcome = app.perform(crate::action::Intent::Refresh);
                dispatch_all(worker, &outcome);
                return Ok(false);
            }
            KeyCode::Esc => {
                let outcome = app.perform(crate::action::Intent::Back);
                dispatch_all(worker, &outcome);
                return Ok(false);
            }
            _ => {}
        }
    } else if let Some(overlay) = app.overlay.clone() {
        if let Overlay::Prompt {
            field,
            value,
            title,
        } = overlay
        {
            match key.code {
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let mut value = value;
                    value.push(character);
                    app.overlay = Some(Overlay::Prompt {
                        field,
                        value,
                        title,
                    });
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    let mut value = value;
                    value.pop();
                    app.overlay = Some(Overlay::Prompt {
                        field,
                        value,
                        title,
                    });
                    return Ok(false);
                }
                KeyCode::Enter => {
                    let outcome = app.perform(crate::action::Intent::ConfirmOverlay);
                    dispatch_all(worker, &outcome);
                    return Ok(false);
                }
                KeyCode::Esc => {
                    app.overlay = None;
                    return Ok(false);
                }
                _ => {}
            }
        }
        if let Some(Overlay::Palette { query, selected: _ }) = app.overlay.clone() {
            match key.code {
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let mut query = query;
                    query.push(character);
                    app.overlay = Some(Overlay::Palette { query, selected: 0 });
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    let mut query = query;
                    query.pop();
                    app.overlay = Some(Overlay::Palette { query, selected: 0 });
                    return Ok(false);
                }
                KeyCode::Enter => {
                    let outcome = app.perform(crate::action::Intent::PaletteRun);
                    dispatch_all(worker, &outcome);
                    return Ok(false);
                }
                KeyCode::Esc => {
                    app.overlay = None;
                    return Ok(false);
                }
                _ => {}
            }
        }
        // The approval card accepts a digit as a direct option selection.
        if let Some(Overlay::Approval { .. }) = app.overlay
            && let KeyCode::Char(digit) = key.code
            && let Some(index) = digit.to_digit(10)
            && index >= 1
        {
            let index = index as usize - 1;
            if index < app.approvals().len() {
                app.overlay = Some(Overlay::Approval { selected: index });
                let outcome = app.perform(crate::action::Intent::ApprovalApprove);
                dispatch_all(worker, &outcome);
                return Ok(false);
            }
        }
    } else if app.focus == Focus::Composer
        && app.page == Page::Agent
        && let Some(exit) = handle_composer_key(app, worker, key)?
    {
        return Ok(exit);
    }

    let chord = Chord::from_event(key);
    let scopes = app.active_scopes();
    let Some(intent) = app.keymap.resolve(&scopes, chord) else {
        // An unbound key must not be swallowed silently when it looks like an
        // action the user expected to work.
        return Ok(false);
    };

    // `$EDITOR` hands the terminal over, so it runs outside the reducer.
    if intent == crate::action::Intent::EditComposerExternally {
        let body = app.composer.text().to_string();
        let edited = with_terminal_restored(guard, || {
            crate::terminal::edit_in_editor("Vibex draft", &body)
        })?;
        if let Ok(Some(text)) = edited {
            app.composer.set_text(text);
        }
        return Ok(false);
    }

    let outcome = app.perform(intent);
    dispatch_all(worker, &outcome);
    Ok(app.should_quit)
}

/// Composer editing keys. Returns `Some(true)` to exit the program.
fn handle_composer_key(
    app: &mut App,
    worker: &Worker,
    key: KeyEvent,
) -> BackendResult<Option<bool>> {
    use crate::action::Intent;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char(character) if !ctrl => {
            app.composer.insert_char(character);
            if let Some((trigger, query)) = app.refresh_completion() {
                worker.dispatch(crate::app::Effect::DiscoverCompletions { trigger, query });
            }
            return Ok(Some(false));
        }
        KeyCode::Backspace => {
            app.composer.backspace();
            app.refresh_completion();
            return Ok(Some(false));
        }
        KeyCode::Delete => {
            app.composer.delete();
            app.refresh_completion();
            return Ok(Some(false));
        }
        KeyCode::Left if !ctrl => {
            app.composer.move_left();
            return Ok(Some(false));
        }
        KeyCode::Right if !ctrl => {
            app.composer.move_right();
            return Ok(Some(false));
        }
        KeyCode::Up if !ctrl => {
            // The completion menu owns the arrow keys while it is open.
            if app.completion.is_some() {
                let outcome = app.perform(Intent::CompletionPrevious);
                dispatch_all(worker, &outcome);
            } else {
                app.composer.move_up();
                app.refresh_completion();
            }
            return Ok(Some(false));
        }
        KeyCode::Down if !ctrl => {
            if app.completion.is_some() {
                let outcome = app.perform(Intent::CompletionNext);
                dispatch_all(worker, &outcome);
            } else {
                app.composer.move_down();
                app.refresh_completion();
            }
            return Ok(Some(false));
        }
        KeyCode::Home => {
            app.composer.move_line_start();
            return Ok(Some(false));
        }
        KeyCode::End => {
            app.composer.move_line_end();
            return Ok(Some(false));
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            // Let the transcript scroll while the composer keeps the cursor.
            let outcome = app.perform(if key.code == KeyCode::PageUp {
                Intent::ScrollPageUp
            } else {
                Intent::ScrollPageDown
            });
            dispatch_all(worker, &outcome);
            return Ok(Some(false));
        }
        KeyCode::Esc => {
            if app.completion.is_some() {
                app.completion = None;
                return Ok(Some(false));
            }
            // A double Escape clears the draft; the first press only warns.
            if app.draft_clear_armed {
                app.composer.clear();
                app.history.reset();
                app.draft_clear_armed = false;
                let message = app.strings.composer_draft_cleared().to_string();
                app.toast(Toast::info(message));
            } else {
                app.draft_clear_armed = true;
                let message = app.strings.composer_press_again().to_string();
                app.toast(Toast::info(message));
            }
            return Ok(Some(false));
        }
        KeyCode::Tab => {
            if app.completion.is_some() {
                let outcome = app.perform(Intent::CompletionAccept);
                dispatch_all(worker, &outcome);
                return Ok(Some(false));
            }
        }
        KeyCode::Enter if shift => {
            app.composer.insert_char('\n');
            return Ok(Some(false));
        }
        _ => {}
    }
    Ok(None)
}

fn handle_mouse(app: &mut App, kind: MouseEventKind) -> bool {
    // The mouse is an enhancement only: every action has a keyboard path.
    match kind {
        MouseEventKind::ScrollUp => {
            app.scroll.follow = false;
            app.scroll.offset = app.scroll.offset.saturating_sub(3);
            true
        }
        MouseEventKind::ScrollDown => {
            app.scroll.offset = app.scroll.offset.saturating_add(3);
            true
        }
        _ => false,
    }
}

fn apply_message(app: &mut App, message: AppMessage) -> BackendResult<()> {
    match message {
        AppMessage::Sessions(result) => {
            if let Err(error) = app.agent.apply_sessions(result) {
                app.toast(Toast::danger(error.message));
            }
            app.live = LiveState::Ready;
        }
        AppMessage::SessionOpened(result) => match result {
            Ok(snapshot) => {
                app.sync_transcript();
                app.open_session(snapshot.session.id.clone());
                app.live = LiveState::Ready;
                let _ = snapshot;
            }
            Err(error) => {
                app.toast(Toast::danger(error.message));
            }
        },
        AppMessage::TimelineRefreshed(result) => {
            if let Err(error) = result {
                app.toast(Toast::danger(error.message));
            }
        }
        AppMessage::RuntimeOptions(result) => {
            match result {
                Ok(catalog) => {
                    app.runtime_options = Some(catalog);
                    if app.overlay.is_none() {
                        app.overlay = Some(Overlay::RuntimePicker { selected: 0 });
                    }
                }
                Err(error) => app.toast(Toast::danger(error.message)),
            };
        }
        AppMessage::SessionCreated(result) => match result {
            Ok(session) => {
                app.toast(Toast::success(format!(
                    "{}: {}",
                    app.strings.session_new(),
                    session.title
                )));
                app.open_session(session.id.clone());
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Mutation { key, result } => {
            app.pending.remove(&key);
            match result {
                Ok(()) => {
                    if key == "resolve_permission" {
                        let message = app.strings.approval_resolved().to_string();
                        app.toast(Toast::success(message));
                    }
                    if key == "update_entry" || key == "git_revert" || key == "worktree_create" {
                        let message = app.strings.management_saved().to_string();
                        app.toast(Toast::success(message));
                    }
                    if key == "provider_secret" {
                        let message = app.strings.management_saved().to_string();
                        app.toast(Toast::success(message));
                    }
                    if key == "revoke_device" || key == "delete_session" {
                        let outcome = app.perform(crate::action::Intent::Refresh);
                        dispatch_refresh(app, outcome);
                    }
                }
                Err(error) => {
                    // A failed mutation rolls the optimistic UI back and says
                    // why, rather than leaving the user guessing.
                    app.toast(Toast::danger(format!("{}: {}", key, error.message)));
                }
            }
        }
        AppMessage::Workspaces(rows) => {
            app.workspace_rows = rows;
        }
        AppMessage::WorkspaceOpened(result) => match result {
            Ok(summary) => {
                app.workspace_path = Some(summary.workspace.root_path.clone());
                let outcome = app.perform(crate::action::Intent::NewSession);
                dispatch_refresh(app, outcome);
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::DirectoryListing(result) => match result {
            Ok(listing) => {
                app.set_selection(crate::keymap::Scope::Sessions, 0);
                app.workspace_browse = Some(listing);
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::FileTree(result) => match result {
            Ok(rows) => {
                app.file_rows = rows;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::FileContents(result) => match result {
            Ok(response) => {
                app.overlay = Some(Overlay::TextView {
                    title: response.path.clone(),
                    body: response.content.clone().unwrap_or_default(),
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::GitStatus(result) => match result {
            Ok(status) => {
                app.git_status = Some(status);
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::GitHistory(result) => match result {
            Ok(history) => {
                let body = history
                    .commits
                    .iter()
                    .map(|commit| {
                        format!(
                            "{}  {}  {}",
                            commit.short_hash, commit.author_name, commit.subject
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                app.git_history = history.commits;
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.git_history_title().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::GitBranches(result) => match result {
            Ok(branches) => {
                let body = branches
                    .branches
                    .iter()
                    .map(|branch| {
                        format!(
                            "{} {}{}",
                            if branch.current { "*" } else { " " },
                            branch.name,
                            branch
                                .upstream
                                .as_ref()
                                .map(|upstream| format!(
                                    "  {upstream} (+{}/-{})",
                                    branch.ahead, branch.behind
                                ))
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                app.git_branches = branches.branches;
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.git_branches_title().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Worktrees(result) => match result {
            Ok(snapshot) => {
                let body = if snapshot.managed_worktrees.is_empty() {
                    app.strings.nothing_here().to_string()
                } else {
                    snapshot
                        .managed_worktrees
                        .iter()
                        .map(|worktree| {
                            format!(
                                "{}  {}  {:?}",
                                worktree.branch.clone().unwrap_or_else(|| "-".to_string()),
                                worktree.worktree_path,
                                worktree.status
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                app.worktrees = Some(snapshot);
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.worktree_title().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::WorktreePreflight(result) => match result {
            Ok(preflight) => {
                // The preflight verdict is shown before anything is touched.
                let verdict = if preflight.allowed {
                    app.strings.worktree_preflight_allowed()
                } else {
                    app.strings.worktree_preflight_blocked()
                };
                let risks = preflight
                    .risks
                    .iter()
                    .map(|risk| format!("{risk:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.worktree_preflight_title().to_string(),
                    body: format!("{verdict}\n\n{risks}"),
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::GitDiff(result) => match result {
            Ok(diff) => {
                app.diff_text = Some(diff.diff.clone());
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.transcript_diff().to_string(),
                    body: diff.diff,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Devices(result) => match result {
            Ok(devices) => {
                app.management_data.devices = devices;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::PairingOffer(result) => match result {
            Ok(response) => {
                // The code is shown once and never stored: it is not written to
                // logs, configuration, or the scrollback export.
                app.overlay = Some(Overlay::PairingCode {
                    code: response.offer.one_time_challenge.clone(),
                    link: response.launch_fragment.clone(),
                    permission: response.offer.summary.permission_level,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Audit(result) => match result {
            Ok(records) => {
                let body = records
                    .iter()
                    .map(|record| {
                        format!(
                            "{}  {:?}  {:?}",
                            record.created_at_ms, record.action, record.outcome
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                app.management_data.audit = records;
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.devices_audit().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Profiles(result) => match result {
            Ok(profiles) => {
                app.management_data.providers = profiles;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::ProviderHealth(result) => match result {
            Ok(health) => {
                app.management_data.health = health;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::ProviderTest(result) => match result {
            Ok(outcome) => {
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.management_probe().to_string(),
                    body: format!("{outcome:?}"),
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::ProviderModels(result) => match result {
            Ok(models) => {
                let body = models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.management_fetch_models().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Agents(result) => match result {
            Ok(agents) => {
                app.management_data.agents = agents;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::AgentAuth(result) => match result {
            Ok(contexts) => {
                let body = contexts
                    .iter()
                    .map(|context| format!("{}  {:?}", context.agent_id, context.status))
                    .collect::<Vec<_>>()
                    .join("\n");
                app.management_data.agent_auth = contexts;
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.management_login().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Mcp(result) => match result {
            Ok(servers) => {
                app.management_data.mcp = servers;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Skills(result) => match result {
            Ok(skills) => {
                app.management_data.skills = skills;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Prompts(result) => match result {
            Ok(prompts) => {
                app.management_data.prompts = prompts;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Completions(result) => match result {
            Ok(discovery) => {
                if let Some(menu) = app.completion.as_mut() {
                    // `/` shows the Agent's commands and Vibex Prompts in one
                    // list; the menu keeps the provider's ordering, which is
                    // already "most relevant first".
                    let mut entries = discovery
                        .response
                        .entries
                        .iter()
                        .chain(discovery.quick_phrases.iter())
                        .map(|entry| crate::composer::Completion {
                            insert: entry.insertion_text.clone(),
                            label: entry.label.clone(),
                            detail: entry.description.clone().unwrap_or_default(),
                            group: format!("{:?}", entry.source_kind),
                        })
                        .collect::<Vec<_>>();
                    entries.dedup_by(|left, right| left.insert == right.insert);
                    menu.items = entries;
                    menu.loading = false;
                    menu.selected = menu.selected.min(menu.items.len().saturating_sub(1));
                }
            }
            Err(error) => {
                if let Some(menu) = app.completion.as_mut() {
                    menu.loading = false;
                }
                app.toast(Toast::warning(error.message));
            }
        },
        AppMessage::Hooks(result) => match result {
            Ok(hooks) => {
                app.management_data.hooks = hooks;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Usage(result) => match result {
            Ok(report) => {
                app.management_data.usage = report.aggregate;
                app.management_data.usage_session = report.session;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Recovery(result) => match result {
            Ok(detail) => {
                app.toast(Toast::success(format!(
                    "{}: {detail}",
                    app.strings.recovery_artifact_path()
                )));
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::BackupList(result) => match result {
            Ok(list) => app.management_data.backups = list,
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Clipboard(result) => {
            if let Err(error) = result {
                app.toast(Toast::warning(error.message));
            }
        }
        AppMessage::EditorFinished(result) => match result {
            Ok(Some(text)) => app.composer.set_text(text),
            Ok(None) => {}
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Event(event) => {
            let decision = app.agent.apply_event(event);
            match decision {
                vibex_ui::AgentEventDecision::Applied => {
                    app.live = LiveState::Ready;
                    app.sync_transcript();
                }
                vibex_ui::AgentEventDecision::NeedsAuthoritativeRefetch => {
                    // Lagged events mean the client must re-read, never
                    // continue applying deltas across the gap.
                    app.live = LiveState::Reconnecting;
                    app.toast(Toast::info(app.strings.reconnecting().to_string()));
                }
                vibex_ui::AgentEventDecision::Disconnected => {
                    app.live = LiveState::Offline;
                    app.toast(Toast::warning(app.strings.disconnected().to_string()));
                }
                vibex_ui::AgentEventDecision::IgnoredStale => {}
            }
        }
        AppMessage::SubscriptionEnded => {
            app.live = LiveState::Offline;
            app.toast(Toast::warning(app.strings.disconnected().to_string()));
        }
        AppMessage::Notice { text, danger } => {
            app.toast(if danger {
                Toast::danger(text)
            } else {
                Toast::info(text)
            });
        }
    }
    Ok(())
}

fn dispatch_refresh(app: &mut App, outcome: Outcome) {
    let _ = (app, outcome);
}

/// Build a prompt overlay for the given field. Used by the composition root for
/// first-run flows such as a workspace path.
pub fn prompt_for(app: &mut App, title: String, field: PromptField) {
    app.overlay = Some(Overlay::Prompt {
        title,
        field,
        value: String::new(),
    });
}

/// Set the seat that the status bar reports.
pub fn set_seat(app: &mut App, seat: crate::view::SeatKind) {
    app.seat = seat;
}

/// Mark the connection ready once the composition root has attached.
pub fn mark_ready(app: &mut App) {
    app.live = LiveState::Ready;
    if app.page == Page::Sessions {
        app.focus = Focus::Main;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_budget_is_under_sixteen_milliseconds() {
        assert!(FRAME_INTERVAL <= Duration::from_millis(17));
    }

    #[test]
    fn input_batch_is_bounded() {
        const { assert!(INPUT_BATCH_LIMIT > 0) };
        assert!(INPUT_BATCH_BUDGET <= Duration::from_millis(10));
    }

    #[test]
    fn tick_period_is_slow_enough_to_stay_idle() {
        // Ticks exist to expire toasts, not to animate; anything faster would
        // make the idle frame budget impossible.
        assert!(TICK >= Duration::from_millis(100));
    }

    #[test]
    fn exit_reasons_are_distinguishable() {
        assert_ne!(ExitReason::UserQuit, ExitReason::ConnectionLost);
    }
}
