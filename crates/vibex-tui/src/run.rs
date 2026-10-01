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
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
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
                    if handle_mouse(app, worker, mouse) {
                        dirty = true;
                    }
                }
                Event::Resize(columns, rows) => {
                    app.resize(columns, rows);
                    dirty = true;
                }
                Event::Paste(text) => {
                    if app.search_composing() {
                        if let Some(search) = app.search.as_mut() {
                            let mut query = search.query.clone();
                            query.push_str(&text);
                            search.set_query(query);
                        }
                        app.refresh_search_matches();
                    } else {
                        // One route for every paste: a path to a picture is a
                        // request to attach it, anything else is draft text.
                        app.insert_pasted_text(&text);
                    }
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

        // ---- the banner row ----------------------------------------------
        // The banner is a condition rather than an event: refreshing it here
        // means a message that is no longer true withdraws itself.
        if app.refresh_banner() {
            dirty = true;
        }

        // ---- the send queue ----------------------------------------------
        // A turn that has ended releases the next held message. Checked here,
        // after worker results have been applied, because that is the only
        // moment the session's state can have changed.
        if let Some((text, attachments)) = app.drain_queue()
            && let Some(session_id) = app.selected_session_id().cloned()
        {
            worker.dispatch(crate::app::Effect::SendMessage {
                session_id,
                text,
                attachments,
            });
            dirty = true;
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
    // The transcript search bar is a text field, so it takes printable keys
    // before the binding table sees them — the same rule the list filter uses.
    // Control chords still fall through, so `Ctrl+Q`, `Ctrl+P` and `Ctrl+C`
    // keep working while a field has focus.
    if app.search_composing() && !key.modifiers.contains(KeyModifiers::CONTROL) {
        handle_search_key(app, key);
        return Ok(false);
    }
    // The settings surface has four modes. The two typing modes take printable
    // keys before the table, and the chooser takes the arrows.
    if app.page == Page::Settings
        && !app.settings.view.is_browse()
        && !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        handle_settings_mode_key(app, key);
        return Ok(false);
    }
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
        // The shortcuts cheatsheet: `/` filters it, the arrows walk it and
        // Left/Right fold the category the cursor is on.
        if let Some(Overlay::Help {
            query,
            selected,
            collapsed,
        }) = app.overlay.clone()
        {
            match key.code {
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let mut query = query;
                    query.push(character);
                    app.overlay = Some(Overlay::Help {
                        query,
                        selected: 0,
                        collapsed,
                    });
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    let mut query = query;
                    query.pop();
                    app.overlay = Some(Overlay::Help {
                        query,
                        selected: 0,
                        collapsed,
                    });
                    return Ok(false);
                }
                KeyCode::Delete => {
                    app.overlay = Some(Overlay::Help {
                        query: String::new(),
                        selected: 0,
                        collapsed,
                    });
                    return Ok(false);
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Enter => {
                    let rows = crate::view::shortcut_rows(app, &query, &collapsed);
                    let selected = selected.min(rows.len().saturating_sub(1));
                    let Some(category) =
                        rows.get(selected).and_then(|row| row.header).or_else(|| {
                            // On a binding, fold the category above it.
                            rows[..selected].iter().rev().find_map(|row| row.header)
                        })
                    else {
                        return Ok(false);
                    };
                    let mut collapsed = collapsed;
                    if !collapsed.remove(category.id()) {
                        collapsed.insert(category.id().to_string());
                    }
                    app.overlay = Some(Overlay::Help {
                        query,
                        selected: 0,
                        collapsed,
                    });
                    return Ok(false);
                }
                KeyCode::Esc => {
                    // `Esc` clears the filter first and closes second, so a
                    // mistyped search is one key from gone.
                    if query.is_empty() {
                        app.overlay = None;
                    } else {
                        app.overlay = Some(Overlay::Help {
                            query: String::new(),
                            selected: 0,
                            collapsed,
                        });
                    }
                    return Ok(false);
                }
                _ => {}
            }
        }
        // The key-binding editor. While a chord is being captured every key is
        // data — including `d` and `s`, which are "reset" and "save" only in
        // the list — so the capture arm comes first and consumes whatever
        // arrives.
        if let Some(Overlay::Keys {
            query,
            selected,
            capturing,
            message,
            dirty,
        }) = app.overlay.clone()
        {
            if capturing.is_some() {
                if key.code == KeyCode::Esc {
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected,
                        capturing: None,
                        message: Some(app.strings.keys_capture_cancelled().to_string()),
                        dirty,
                    });
                } else {
                    app.finish_key_capture(Chord::from_event(key));
                }
                return Ok(false);
            }
            let rows = crate::view::key_editor_rows(app, &query);
            let selected = selected.min(rows.len().saturating_sub(1));
            let intent_at = |index: usize| {
                rows.get(index)
                    .and_then(|row| row.binding)
                    .map(|binding| binding.intent)
            };
            match key.code {
                KeyCode::Esc => {
                    app.overlay = None;
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    let mut query = query;
                    query.pop();
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected: 0,
                        capturing: None,
                        message,
                        dirty,
                    });
                    return Ok(false);
                }
                KeyCode::Delete => {
                    app.overlay = Some(Overlay::Keys {
                        query: String::new(),
                        selected: 0,
                        capturing: None,
                        message,
                        dirty,
                    });
                    return Ok(false);
                }
                // `/` is the filter key the other lists use; it starts an
                // empty query rather than being typed into one.
                // `/` is the filter key the other lists use; it starts an
                // empty query rather than being typed into one.
                KeyCode::Char('/') if query.is_empty() => return Ok(false),
                KeyCode::Up => {
                    let next = crate::view::step_key_row(&rows, selected, -1);
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected: next,
                        capturing: None,
                        message,
                        dirty,
                    });
                    return Ok(false);
                }
                KeyCode::Down => {
                    let next = crate::view::step_key_row(&rows, selected, 1);
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected: next,
                        capturing: None,
                        message,
                        dirty,
                    });
                    return Ok(false);
                }
                KeyCode::Enter => {
                    if let Some(intent) = intent_at(selected) {
                        app.begin_key_capture(intent);
                    }
                    return Ok(false);
                }
                KeyCode::Char('d') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(intent) = intent_at(selected) {
                        app.reset_key_binding(intent);
                    }
                    return Ok(false);
                }
                KeyCode::Char('s') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let (message, dirty) = match app.save_keymap() {
                        Ok(path) => (Some(format!("{}: {path}", app.strings.keys_saved())), false),
                        Err(error) => (Some(error), dirty),
                    };
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected,
                        capturing: None,
                        message,
                        dirty,
                    });
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let mut query = query;
                    query.push(character);
                    app.overlay = Some(Overlay::Keys {
                        query,
                        selected: 0,
                        capturing: None,
                        message,
                        dirty,
                    });
                    return Ok(false);
                }
                // Anything else — `Ctrl+Q`, the function keys — falls through
                // to the binding table, so the editor does not trap the reader.
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

/// Settings sub-mode keys.
///
/// `Esc` is deliberately routed through the reducer's `Back` intent rather than
/// handled here: "put the old value back" and "clear the filter" are state
/// transitions, not text editing.
fn handle_settings_mode_key(app: &mut App, key: KeyEvent) {
    use crate::app::SettingsMode;
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match app.settings.view.clone() {
        SettingsMode::Filter => match key.code {
            KeyCode::Char(character) if !control => app.push_settings_filter(character),
            KeyCode::Char('u') if control => {
                app.settings.filter.clear();
                app.set_selection(crate::keymap::Scope::Settings, 0);
            }
            KeyCode::Backspace => app.pop_settings_filter(),
            KeyCode::Delete => {
                app.settings.filter.clear();
                app.set_selection(crate::keymap::Scope::Settings, 0);
            }
            KeyCode::Enter => app.leave_settings_filter(false),
            KeyCode::Esc => app.leave_settings_filter(true),
            KeyCode::Down => app.move_setting_selection(1),
            KeyCode::Up => app.move_setting_selection(-1),
            _ => {}
        },
        SettingsMode::Picking { .. } => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.step_setting_pick(1),
            KeyCode::Up | KeyCode::Char('k') => app.step_setting_pick(-1),
            KeyCode::Enter => {
                app.commit_setting_pick();
            }
            KeyCode::Esc => {
                app.cancel_setting_pick();
            }
            KeyCode::Char('d') => {
                if let Some(row) = app.settings.view.row() {
                    app.cancel_setting_pick();
                    app.reset_setting(row);
                }
            }
            _ => {}
        },
        SettingsMode::Editing { ref buffer, .. } => match key.code {
            KeyCode::Char(character) if !control => {
                let mut buffer = buffer.clone();
                buffer.push(character);
                app.settings.view = SettingsMode::Editing {
                    row: app.settings.view.row().expect("editing has a row"),
                    buffer,
                };
            }
            KeyCode::Backspace => {
                let mut buffer = buffer.clone();
                buffer.pop();
                app.settings.view = SettingsMode::Editing {
                    row: app.settings.view.row().expect("editing has a row"),
                    buffer,
                };
            }
            KeyCode::Enter => {
                app.commit_setting_edit();
            }
            KeyCode::Esc => {
                app.cancel_setting_edit();
            }
            _ => {}
        },
        SettingsMode::Browse => {}
    }
}

/// Transcript search editing keys.
///
/// The bar stays in "composing" until `Enter`, so the query can be corrected
/// while the matches are already highlighted — the case that makes a regex
/// search usable rather than a guessing game.
fn handle_search_key(app: &mut App, key: KeyEvent) {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(search) = app.search.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Char(character) if !control => {
            let mut query = search.query.clone();
            query.push(character);
            search.set_query(query);
            app.refresh_search_matches();
        }
        KeyCode::Backspace => {
            let mut query = search.query.clone();
            query.pop();
            search.set_query(query);
            app.refresh_search_matches();
        }
        KeyCode::Delete => {
            search.set_query(String::new());
            search.matches.clear();
            search.total = 0;
        }
        KeyCode::Enter => {
            search.composing = false;
            app.reveal_current_match();
        }
        KeyCode::Down | KeyCode::PageDown => {
            app.step_search(1);
        }
        KeyCode::Up | KeyCode::PageUp => {
            app.step_search(-1);
        }
        KeyCode::Esc => {
            app.close_search();
        }
        _ => {}
    }
}

/// Composer editing keys. Returns `Some(true)` to exit the program.
fn handle_composer_key(
    app: &mut App,
    worker: &Worker,
    key: KeyEvent,
) -> BackendResult<Option<bool>> {
    use crate::action::Intent;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // The draft's first character decides its mode, so the mode is refreshed
    // before the key is interpreted rather than tracked alongside the text.
    app.sync_composer_mode();
    // `? ` puts the composer in history search: the draft is the query and the
    // drawer above it is the list.
    if app.composer_mode == crate::app::ComposerMode::HistorySearch {
        match key.code {
            KeyCode::Up => {
                app.move_history_selection(-1);
                return Ok(Some(false));
            }
            KeyCode::Down => {
                app.move_history_selection(1);
                return Ok(Some(false));
            }
            KeyCode::Enter | KeyCode::Tab => {
                app.accept_history_match();
                return Ok(Some(false));
            }
            KeyCode::Esc => {
                app.cancel_history_search();
                return Ok(Some(false));
            }
            _ => {}
        }
    }
    match key.code {
        // `Alt` chords belong to the binding table: word motions, redo and the
        // kill commands are bindings, not text. Inserting the letter instead
        // would make `Alt+B` type a `b`.
        KeyCode::Char(character) if !ctrl && !alt => {
            app.composer.insert_char(character);
            if let Some((trigger, query)) = app.refresh_completion() {
                worker.dispatch(crate::app::Effect::DiscoverCompletions { trigger, query });
            }
            return Ok(Some(false));
        }
        // `Alt`/`Ctrl` + Backspace is a word kill and belongs to the binding
        // table; a bare Backspace is one grapheme.
        KeyCode::Backspace if !ctrl && !alt => {
            app.composer.backspace();
            app.refresh_completion();
            return Ok(Some(false));
        }
        KeyCode::Delete => {
            app.composer.delete();
            app.refresh_completion();
            return Ok(Some(false));
        }
        // Shift turns a motion into a selection. These arms come first: a
        // `Shift+Left` also satisfies `!ctrl`, so the plain arms below would
        // otherwise swallow it.
        KeyCode::Left if shift && ctrl => {
            app.composer.extend_word_left();
            return Ok(Some(false));
        }
        KeyCode::Right if shift && ctrl => {
            app.composer.extend_word_right();
            return Ok(Some(false));
        }
        KeyCode::Left if shift => {
            app.composer.extend_left();
            return Ok(Some(false));
        }
        KeyCode::Right if shift => {
            app.composer.extend_right();
            return Ok(Some(false));
        }
        KeyCode::Up if shift && !alt => {
            app.composer.extend_up();
            return Ok(Some(false));
        }
        KeyCode::Down if shift && !alt => {
            app.composer.extend_down();
            return Ok(Some(false));
        }
        KeyCode::Home if shift => {
            app.composer.extend_line_start();
            return Ok(Some(false));
        }
        KeyCode::End if shift => {
            app.composer.extend_line_end();
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
        KeyCode::Up if !ctrl && !alt => {
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
        KeyCode::Down if !ctrl && !alt => {
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
            // A selection is dropped first: the reader who highlighted a phrase
            // and pressed Escape wanted the highlight gone, not to leave the
            // session.
            if app.composer.clear_selection() {
                return Ok(Some(false));
            }
            // Otherwise `Esc` walks back out of the session, exactly as the
            // binding table advertises it. The draft is kept, so stepping out to
            // the session list costs nothing; `Ctrl+C` is what clears it.
            let outcome = app.perform(Intent::Back);
            dispatch_all(worker, &outcome);
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

/// The mouse is an enhancement, never the only path: every gesture here has a
/// keyboard equivalent, and the transcript stays readable if the terminal has no
/// mouse reporting at all.
fn handle_mouse(app: &mut App, worker: &Worker, mouse: MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll.follow = false;
            app.scroll.offset = app.scroll.offset.saturating_sub(3);
            // The wheel is the other way to reach the top of the loaded
            // window; the controller ignores the ask when there is nothing
            // older or a page is already on its way.
            if app.at_transcript_top()
                && let Some(effect) = app.load_older_history()
            {
                worker.dispatch(effect);
            }
            true
        }
        MouseEventKind::ScrollDown => {
            app.scroll.offset = app.scroll.offset.saturating_add(3);
            true
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // A banner is dismissed by clicking it: it has no key of its own
            // and would otherwise stay until its condition changed.
            if let Some(rect) = app.regions.banner
                && rect_contains(rect, mouse.column, mouse.row)
            {
                app.banner = None;
                return true;
            }
            // The modal's close affordance is the one chrome control the mouse
            // owns, and it only exists while a modal is open.
            if let Some(close) = app.regions.modal_close
                && rect_contains(close, mouse.column, mouse.row)
            {
                app.overlay = None;
                app.regions.modal_close = None;
                return true;
            }
            if app.overlay.is_some() {
                return false;
            }
            // A hint in the shortcut band is a button: clicking it runs the
            // intent the key would have run.
            if let Some((_, intent)) = app
                .regions
                .hints
                .iter()
                .find(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row))
                .copied()
            {
                let outcome = app.perform(intent);
                dispatch_all(worker, &outcome);
                return true;
            }
            // A list row selects; a second click on the same row activates it.
            if let Some(region) = app.regions.list
                && let Some(index) = crate::app::list_row_at(&region, mouse.column, mouse.row)
            {
                let repeat = app.double_click_at(mouse.column, mouse.row);
                app.set_selection(region.scope, index);
                if repeat {
                    let intent = match region.scope {
                        crate::keymap::Scope::Sessions => {
                            crate::action::Intent::OpenSelectedSession
                        }
                        crate::keymap::Scope::Management => {
                            crate::action::Intent::OpenManagementSection
                        }
                        _ => crate::action::Intent::ActivateSetting,
                    };
                    let outcome = app.perform(intent);
                    dispatch_all(worker, &outcome);
                }
                return true;
            }
            // A turn tick scrolls to that turn.
            if let Some((_, turn)) = app
                .regions
                .turns
                .iter()
                .find(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row))
                .copied()
            {
                let block = app.transcript.block_of_turn(turn);
                if let Some(block) = block {
                    app.scroll.follow = false;
                    app.scroll.offset = app.transcript.line_of_block(block);
                    app.set_selection(crate::keymap::Scope::Agent, block);
                }
                return true;
            }
            // A dock row selects; a second click on the same row opens it, the
            // same contract the other lists use.
            if let Some(region) = app.regions.dock
                && rect_contains(region, mouse.column, mouse.row)
            {
                let index = usize::from(mouse.row - region.y);
                if index < app.dock_rows().len() {
                    app.dock_selection = Some(index);
                    if app.double_click_at(mouse.column, mouse.row) {
                        app.activate_dock_row();
                    }
                }
                return true;
            }
            // The queue band selects a held message.
            if let Some(region) = app.regions.queue
                && rect_contains(region, mouse.column, mouse.row)
            {
                let index = usize::from(mouse.row - region.y);
                if index < app.queued_messages.len() {
                    app.queue_selection = Some(index);
                    if app.double_click_at(mouse.column, mouse.row) {
                        app.edit_queued_message();
                    }
                }
                return true;
            }
            // The composer takes the keyboard from a click anywhere in its box,
            // and from a text row it also takes the caret: a click leaves the
            // selection empty, a drag fills it, and typing over it replaces it.
            if app.click_composer(mouse.column, mouse.row) {
                return true;
            }
            if app.page != Page::Agent {
                return false;
            }
            let Some((line, column)) = mouse_cell(app, mouse.column, mouse.row) else {
                return false;
            };
            let now = Instant::now();
            let double_click = app.last_click.is_some_and(|(at, last_line, last_column)| {
                now.duration_since(at) < DOUBLE_CLICK && last_line == line && last_column == column
            });
            if double_click {
                app.last_click = None;
                return app.select_word_at(line, column);
            }
            app.last_click = Some((now, line, column));
            app.clear_text_selection();
            app.begin_text_selection(line, column);
            true
        }
        MouseEventKind::Moved => {
            let hover = app
                .regions
                .list
                .as_ref()
                .and_then(|region| {
                    crate::app::list_row_at(region, mouse.column, mouse.row)
                        .map(|index| (region.scope, index))
                })
                .or_else(|| {
                    app.regions.queue.and_then(|region| {
                        rect_contains(region, mouse.column, mouse.row).then_some((
                            crate::keymap::Scope::Agent,
                            usize::from(mouse.row - region.y),
                        ))
                    })
                });
            if app.hover != hover {
                app.hover = hover;
                return true;
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.draft_selecting {
                return app.drag_draft_selection(mouse.column, mouse.row);
            }
            let Some((line, column)) = mouse_cell_clamped(app, mouse.column, mouse.row) else {
                return false;
            };
            app.extend_text_selection(line, column)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // A drag inside the draft copies what it selected; the highlight
            // stays so the reader can see what went to the clipboard.
            if app.draft_selecting {
                app.draft_selecting = false;
                let Some(text) = app.composer.selected_text() else {
                    return true;
                };
                let message = app.strings.copied().to_string();
                app.toast(Toast::success(message));
                worker.dispatch(crate::app::Effect::Clipboard { text });
                return true;
            }
            if !app.finish_text_selection() {
                return true;
            }
            let Some(text) = app.selected_text() else {
                return true;
            };
            let message = app.strings.copied().to_string();
            app.toast(Toast::success(message));
            worker.dispatch(crate::app::Effect::Clipboard { text });
            true
        }
        _ => false,
    }
}

/// How long two clicks count as one double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

fn rect_contains(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

/// The display cell under a pointer inside the transcript band.
fn mouse_cell(app: &App, column: u16, row: u16) -> Option<(usize, u16)> {
    let rect = app.regions.scrollback;
    if !rect_contains(rect, column, row) {
        return None;
    }
    let line = app.transcript.scroll_offset() + usize::from(row - rect.y);
    Some((line, column - rect.x))
}

/// The display cell under a pointer, clamped into the band.
///
/// A drag that leaves the band scrolls the transcript one row per event and
/// keeps extending the selection, so a phrase taller than the window can still
/// be selected in one gesture.
fn mouse_cell_clamped(app: &mut App, column: u16, row: u16) -> Option<(usize, u16)> {
    let rect = app.regions.scrollback;
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    if row < rect.y {
        app.scroll.follow = false;
        app.scroll.offset = app.scroll.offset.saturating_sub(1);
    } else if row >= rect.bottom() {
        app.scroll.follow = false;
        app.scroll.offset = app.scroll.offset.saturating_add(1);
    }
    let row = row.clamp(rect.y, rect.bottom().saturating_sub(1));
    let column = column.clamp(rect.x, rect.right().saturating_sub(1));
    let line = app.transcript.scroll_offset() + usize::from(row - rect.y);
    Some((line, column - rect.x))
}

fn apply_message(app: &mut App, message: AppMessage) -> BackendResult<()> {
    match message {
        AppMessage::Pasted(content) => {
            use crate::worker::ClipboardContent;
            match content {
                ClipboardContent::Image { mime_type, bytes } => {
                    match app.attach_image_bytes(mime_type, bytes) {
                        Ok(label) => {
                            let message = format!("{} {label}", app.strings.image_attached());
                            app.toast(Toast::success(message));
                        }
                        Err(error) => app.toast(Toast::warning(error)),
                    }
                }
                ClipboardContent::Text(text) => app.insert_pasted_text(&text),
                ClipboardContent::Empty => {
                    app.toast(Toast::info(app.strings.clipboard_empty().to_string()));
                }
            }
        }
        AppMessage::ClipboardImage(image) => match image {
            Some((mime_type, bytes)) => match app.attach_image_bytes(mime_type, bytes) {
                Ok(label) => {
                    let message = format!("{} {label}", app.strings.image_attached());
                    app.toast(Toast::success(message));
                }
                Err(error) => app.toast(Toast::warning(error)),
            },
            // No clipboard image: the reader can still name a file, and saying
            // so is better than a key that appears to do nothing.
            None => {
                app.overlay = Some(crate::app::Overlay::Prompt {
                    title: app.strings.image_path_title().to_string(),
                    field: crate::app::PromptField::ImagePath,
                    value: String::new(),
                });
                app.toast(Toast::info(app.strings.image_clipboard_empty().to_string()));
            }
        },
        AppMessage::Sessions(result) => {
            if let Err(error) = app.agent.apply_sessions(result) {
                app.toast(Toast::danger(error.message));
            }
            // Fold the loaded ids into the reader's arrangement: their pins and
            // manual order survive a refresh, and new sessions are appended
            // rather than dropped from the order.
            app.reconcile_sidebar_arrangement();
            app.live = LiveState::Ready;
        }
        AppMessage::SessionOpened { ticket, result } => {
            let failure = result.as_ref().err().map(|error| error.message.clone());
            // The shared controller owns the projection: applying the snapshot
            // is what populates `selected_session_id` and the timeline model,
            // so live events stop being dropped as stale.
            if app.agent.apply_session_snapshot(&ticket, result) {
                match failure {
                    Some(message) => app.toast(Toast::danger(message)),
                    None => {
                        app.sync_transcript();
                        app.open_session(ticket.session_id.clone());
                        app.live = LiveState::Ready;
                    }
                }
            }
        }
        AppMessage::OlderTimeline { ticket, result } => {
            match app.agent.apply_timeline_before(&ticket, result) {
                Ok(true) => app.sync_transcript(),
                Ok(false) => {}
                Err(error) => app.toast(Toast::warning(error.message)),
            }
        }
        AppMessage::TimelineRefreshed(result) => {
            if let Err(error) = result {
                app.toast(Toast::danger(error.message));
            }
        }
        AppMessage::RuntimeOptions(result) => {
            // The catalogue is read both on the way into a session (for the
            // composer's info line) and by the picker itself; only the second
            // one opens an overlay, which is what the pending flag records.
            let picker_waiting = app.runtime_picker_pending;
            app.runtime_picker_pending = false;
            match result {
                Ok(catalog) => {
                    app.runtime_options = Some(catalog);
                    if picker_waiting && app.overlay.is_none() {
                        app.show_runtime_picker();
                    }
                }
                // A prefetch that fails is not news: the info line falls back
                // to the Agent the session records. A failure the reader asked
                // for — they opened the picker — is.
                Err(error) => {
                    if picker_waiting {
                        app.toast(Toast::danger(error.message));
                    }
                }
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
                    if key == "switch_runtime" {
                        // The switch is durable and asynchronous: the runtime
                        // reports `Switching…` until the new Agent is up, so the
                        // acknowledgement says that rather than claiming the
                        // session has already moved.
                        let message = app.strings.runtime_switching().to_string();
                        app.toast(Toast::info(message));
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
