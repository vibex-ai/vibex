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

    // First frame: paint the skeleton before any I/O result arrives. The
    // client opens on the page a session is written on, so what the first
    // frame reads is what that page names — the workspace, the runtime
    // catalogue — plus the session list its background work is derived from.
    let initial = app.perform(crate::action::Intent::NewSession);
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
        let results_started = Instant::now();
        while handled < 64 && results_started.elapsed() < INPUT_BATCH_BUDGET {
            match messages.try_recv() {
                Ok(message) => {
                    handled += 1;
                    apply_message(app, worker, message)?;
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

        // ---- the projected send -------------------------------------------
        // A send stops being projected once the runtime's own copy of it is in
        // the timeline, or once it has waited longer than a send could take.
        if app.settle_pending_send() {
            app.sync_transcript();
            dirty = true;
        }
        // The turn clock follows the session's state, so the elapsed readout
        // starts when the turn does and stops when it ends, whatever path the
        // state took to get there.
        if app.sync_turn_clock() {
            dirty = true;
        }

        // ---- the banner row ----------------------------------------------
        // The banner is a condition rather than an event: refreshing it here
        // means a message that is no longer true withdraws itself.
        if app.refresh_banner() {
            dirty = true;
        }

        // ---- the send queue ----------------------------------------------
        // A turn that has ended releases the next held message — for *its* own
        // session, which is not necessarily the one on screen. Checked here,
        // after worker results have been applied, because that is the only
        // moment a session's state can have changed.
        for (session_id, text, attachments) in app.drain_queue() {
            // A message released into the session on screen needs no
            // announcement — it is about to appear in the transcript. One
            // released elsewhere is the only way the reader can learn it went.
            if Some(&session_id) != app.selected_session_id() {
                let who = app
                    .session_title(&session_id)
                    .unwrap_or_else(|| app.strings.session_untitled().to_string());
                app.toast(Toast::info(format!(
                    "{} · {}",
                    app.strings.queue_released(),
                    who
                )));
            }
            // A released message is projected exactly like a typed one: it was
            // written for this session a while ago, and it should appear the
            // moment it goes out rather than a round trip later.
            let send_id =
                app.mark_send_dispatched(Some(&session_id), text.clone(), attachments.clone());
            worker.dispatch(crate::app::Effect::SendMessage {
                session_id,
                send_id,
                correlation_id: app.pending_sends[&send_id].correlation_id.clone(),
                text,
                attachments,
            });
            dirty = true;
        }

        // ---- tick ---------------------------------------------------------
        let period = if app.chrome_animating() {
            ANIMATION_TICK
        } else {
            TICK
        };
        if last_tick.elapsed() >= period {
            last_tick = Instant::now();
            let outcome = app.tick();
            if outcome.dirty {
                dirty = true;
            }
            dispatch_all(worker, &outcome);
            if app.advance_transcript_animation() {
                dirty = true;
            }
        }

        // ---- frame --------------------------------------------------------
        if dirty && last_frame.elapsed() >= FRAME_INTERVAL {
            // Every size draws the interface itself. There is no minimum: the
            // layout gives back the bands around the transcript and the
            // composer as the terminal shrinks, so a small one still paints a
            // whole frame rather than a notice about being small.
            terminal
                .draw(|frame| crate::view::render(frame, app))
                .map_err(|error| BackendError::failed("tui_render_failed", error.to_string()))?;
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

/// Fold a `Shift` modifier into the character it produced.
///
/// The keyboard protocol reports a shifted key as its *unshifted* code plus a
/// `Shift` modifier, and the character the reader pressed travels only in the
/// alternate keycode — which is why the client asks for
/// [`crossterm::event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS`]. A
/// terminal that implements the protocol's first flag without its second
/// therefore sends `Shift+a` as `Char('a')` + `Shift`, which would type a
/// lowercase letter and match no binding at all.
///
/// Folding it at the one door every key comes through means the composer's text
/// path, the binding table and the rebinding overlay all keep reading the single
/// representation they already assume.
fn normalized_key(key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(character) = key.code
        && character.is_ascii_lowercase()
        && key.modifiers.contains(KeyModifiers::SHIFT)
    {
        return KeyEvent::new_with_kind_and_state(
            KeyCode::Char(character.to_ascii_uppercase()),
            key.modifiers - KeyModifiers::SHIFT,
            key.kind,
            key.state,
        );
    }
    key
}

/// Reduce one key press. Returns `true` when the program should exit.
fn handle_key(
    app: &mut App,
    worker: &Worker,
    guard: &mut TerminalGuard,
    key: KeyEvent,
) -> BackendResult<bool> {
    let key = normalized_key(key);
    // The transcript search bar is a text field, so it takes printable keys
    // before the binding table sees them — the same rule the list filter uses.
    // Control chords still fall through, so `Ctrl+P` and `Ctrl+C` keep working
    // while a field has focus.
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
    // The runtime switcher's own filter is a second text field, opened with `/`
    // on the catalogue. Arrows and the page keys still fall through to the
    // binding table, so a reader can keep walking the list while narrowing it.
    if app.runtime_picker_filtering() {
        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.push_runtime_picker_query(character);
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.pop_runtime_picker_query();
                return Ok(false);
            }
            KeyCode::Delete => {
                app.clear_runtime_picker_query();
                return Ok(false);
            }
            // The first `Esc` gives the query up; a second one, with nothing
            // left to give up, closes the switcher the way `Esc` always does.
            KeyCode::Esc if app.cancel_runtime_picker_filter() => return Ok(false),
            _ => {}
        }
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
                    // Through the reducer, not around it: a cancelled prompt
                    // may be holding state of its own (a run option's key) that
                    // only the reducer knows to drop.
                    let outcome = app.perform(crate::action::Intent::Back);
                    dispatch_all(worker, &outcome);
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
                // Anything else — the control chords, the function keys —
                // falls through to the binding table, so the editor does not
                // trap the reader.
                _ => {}
            }
        }
        // The runtime switcher's recent rows are numbered, and a digit chooses
        // one outright: a reader who wants what they were using a minute ago
        // should not have to walk a catalogue to find it. A digit that names no
        // row falls through, so nothing is swallowed by a shortcut that does not
        // apply.
        if let Some(Overlay::RuntimePicker {
            view: crate::app::RuntimePickerView::Choices,
            ..
        }) = app.overlay
            && !app.runtime_picker.filtering
            && let KeyCode::Char(digit) = key.code
            && let Some(outcome) = app.quick_pick_runtime(digit)
        {
            dispatch_all(worker, &outcome);
            return Ok(false);
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
    } else if composer_takes_keys(app)
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
/// Whether the composer's own text path handles this key before the binding
/// table does.
///
/// The page matters as much as the focus does. The page where a session is
/// written has no session yet — but it has a composer, and there a printable
/// key is *text*: without this the keys fell through to the Agent scope, where
/// typing did nothing and `/` meant "search the transcript" and left the page.
fn composer_takes_keys(app: &App) -> bool {
    app.focus == Focus::Composer && app.page.is_composing_page()
}

fn dispatch_completions(app: &mut App, worker: &Worker) {
    if let Some((trigger, query)) = app.refresh_completion() {
        worker.dispatch(crate::app::Effect::DiscoverCompletions {
            ticket: app.composer_ticket(),
            trigger,
            query,
        });
    }
}

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
            dispatch_completions(app, worker);
            return Ok(Some(false));
        }
        // `Alt`/`Ctrl` + Backspace is a word kill and belongs to the binding
        // table; a bare Backspace is one grapheme.
        KeyCode::Backspace if !ctrl && !alt => {
            app.composer.backspace();
            dispatch_completions(app, worker);
            return Ok(Some(false));
        }
        KeyCode::Delete => {
            app.composer.delete();
            dispatch_completions(app, worker);
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
            } else if app.composer.is_empty()
                && app.page == Page::Agent
                && !app.transcript.is_empty()
            {
                // An empty editor hands Up to the last visible transcript row.
                app.focus = Focus::Main;
                let index = app
                    .transcript
                    .blocks()
                    .iter()
                    .rposition(|block| block.group != crate::transcript::GroupRole::Member)
                    .unwrap_or(0);
                app.set_selection(crate::keymap::Scope::Agent, index);
                app.scroll.follow = false;
                app.scroll.offset = app.transcript.offset_of_block(index);
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
            // The switcher owns the wheel while it is up: it is the list on
            // screen, and scrolling the transcript behind a modal is not a
            // gesture anyone means.
            if app.runtime_picker_is_open() {
                let outcome = app.step_runtime_picker(-3);
                dispatch_all(worker, &outcome);
                return true;
            }
            // On the session list the wheel moves the list: the transcript
            // behind it is not what the reader is looking at, and a list that
            // only answers the arrow keys is a list that cannot be scanned.
            if app.page == crate::app::Page::Sessions && app.overlay.is_none() {
                app.scroll_session_list(-3);
                return true;
            }
            app.scroll_lines(-3);
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
            if app.runtime_picker_is_open() {
                let outcome = app.step_runtime_picker(3);
                dispatch_all(worker, &outcome);
                return true;
            }
            if app.page == crate::app::Page::Sessions && app.overlay.is_none() {
                app.scroll_session_list(3);
                return true;
            }
            app.scroll_lines(3);
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
                // The same way out as `Esc`, so a modal holding state of its
                // own — a run option's key — is left the way the reducer
                // expects rather than around it.
                let outcome = app.perform(crate::action::Intent::Back);
                dispatch_all(worker, &outcome);
                app.regions.modal_close = None;
                return true;
            }
            // The switcher's rows select and activate exactly as the other
            // lists do, headings included: a heading is the control that folds
            // its group, and a double click is how the mouse presses it.
            if let Some(region) = app.regions.runtime_picker
                && let Some(row) = region.row_at(mouse.column, mouse.row)
            {
                let repeat = app.double_click_at(mouse.column, mouse.row);
                let outcome = app.select_runtime_picker_row(row, repeat);
                dispatch_all(worker, &outcome);
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
            if let Some(region) = app.regions.list.clone()
                && let Some(index) = crate::app::list_row_at(&region, mouse.column, mouse.row)
            {
                let repeat = app.double_click_at(mouse.column, mouse.row);
                app.set_selection(region.scope, index);
                // A heading is a control, not a row to open: it folds on the
                // first click, the way a tree does everywhere else. A session
                // keeps the two-step contract, because opening one leaves the
                // page.
                let heading = region.scope == crate::keymap::Scope::Sessions
                    && app.sidebar_row_is_heading(index);
                if repeat || heading {
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
            if let Some(index) = app.transcript.block_at_line(line)
                && app.transcript.line_of_block(index) == line
                && app.transcript.block(index).is_some_and(|block| {
                    block.collapsible && crate::transcript::is_dense_row(block.kind)
                })
            {
                app.clear_text_selection();
                app.focus = Focus::Main;
                app.set_selection(crate::keymap::Scope::Agent, index);
                app.scroll.follow = false;
                app.scroll.offset = app.transcript.scroll_offset();
                let outcome = app.perform(crate::action::Intent::ToggleBlockExpanded);
                dispatch_all(worker, &outcome);
                return true;
            }
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
            let hover = hover_target(app, mouse.column, mouse.row);
            // A hint is a control: the pointer resting on it is how the reader
            // learns it can be pressed.
            let hint = app
                .regions
                .hints
                .iter()
                .find(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row))
                .map(|(_, intent)| *intent);
            if app.hover != hover || app.hovered_hint != hint {
                app.hover = hover;
                app.hovered_hint = hint;
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

/// The row a pointer is over, if it is over one.
///
/// A pointer that is inside no band is the common case — the terminal reports
/// motion for the whole window, and most of the window is not a list — so every
/// arm is a *guard* around its arithmetic rather than a computation the guard
/// only appears to protect.
fn hover_target(app: &App, column: u16, row: u16) -> Option<(crate::keymap::Scope, usize)> {
    if let Some((scope, index)) = app.regions.list.as_ref().and_then(|region| {
        crate::app::list_row_at(region, column, row).map(|index| (region.scope, index))
    }) {
        return Some((scope, index));
    }
    let region = app.regions.queue?;
    rect_contains(region, column, row)
        .then(|| (crate::keymap::Scope::Agent, usize::from(row - region.y)))
}

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

fn apply_message(app: &mut App, worker: &Worker, message: AppMessage) -> BackendResult<()> {
    match message {
        AppMessage::Pasted { ticket, content } => {
            if !app.accepts_composer_ticket(&ticket) {
                return Ok(());
            }
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
        AppMessage::ClipboardImage { ticket, image } => {
            if !app.accepts_composer_ticket(&ticket) {
                return Ok(());
            }
            match image {
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
            }
        }
        AppMessage::Sessions(result) => {
            if let Err(error) = app.agent.apply_sessions(result) {
                app.toast(Toast::danger(error.message));
            }
            // A session the runtime no longer lists is a session the list can
            // no longer draw, so its last words are not worth keeping.
            app.retain_session_echoes();
            // Fold the loaded ids into the reader's arrangement: their pins and
            // manual order survive a refresh, and new sessions are appended
            // rather than dropped from the order.
            app.reconcile_sidebar_arrangement();
            app.live = LiveState::Ready;
        }
        AppMessage::AutoContinueTurnStatus {
            session_id,
            updated_at_ms,
            ended_normally,
        } => {
            // The runtime could not answer; when the session is the one on
            // screen, this client's own copy of the timeline can. That is the
            // same fallback the Desktop makes when its probe fails.
            let ended_normally = ended_normally.or_else(|| {
                let timeline = &app.agent.state.timeline;
                (timeline.session_id.as_ref() == Some(&session_id))
                    .then(|| vibex_core::latest_timeline_turn_ended_normally(&timeline.items))
                    .flatten()
            });
            // An answer for a revision the session has left is dropped by the
            // state machine; nothing here has to judge it.
            app.auto_continue
                .note_status(&session_id, updated_at_ms, ended_normally);
            let effects = app.sync_auto_continue();
            for effect in effects {
                worker.dispatch(effect);
            }
        }
        AppMessage::SidebarOrganization(result) => {
            // A failure is not an interface failure: the list falls back to the
            // sessions' own order, which is what a client with no authority
            // arrangement has always drawn. Saying anything would only report a
            // capability the reader never asked for.
            if let Ok(snapshot) = result {
                app.apply_sidebar_organization(&snapshot);
            }
        }
        AppMessage::SidebarOrganizationMutated(result) => match result {
            Ok(snapshot) => {
                app.apply_sidebar_organization(&snapshot);
            }
            Err(error) => {
                // The change did not land -- a stale revision, a move the
                // authority will not make. Say so, then re-read the tree so the
                // list stops showing the arrangement the reader thought they
                // were editing.
                app.toast(Toast::warning(error.message));
                worker.dispatch(crate::app::Effect::LoadSidebarOrganization);
            }
        },
        AppMessage::SessionOpened { ticket, result } => {
            let failure = result.as_ref().err().map(|error| error.message.clone());
            // The shared controller owns the projection: applying the snapshot
            // is what populates `selected_session_id` and the timeline model,
            // so live events stop being dropped as stale.
            if app.agent.apply_session_snapshot(&ticket, result) {
                app.sync_transcript();
                match failure {
                    Some(message) => app.toast(Toast::danger(message)),
                    None => {
                        app.sync_transcript();
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
            let picker_waiting = app.runtime_picker_pending && app.runtime_picker_is_current();
            app.runtime_picker_pending = false;
            match result {
                Ok(catalog) => {
                    let picker_open = matches!(
                        app.overlay,
                        Some(
                            Overlay::RuntimePicker { .. }
                                | Overlay::RunOptionValues { .. }
                                | Overlay::Prompt {
                                    field: PromptField::RunOptionValue,
                                    ..
                                }
                        )
                    );
                    // Indices and run-option values refer to the catalogue the
                    // reader opened. An unrelated prefetch cannot reorder it.
                    if !picker_open {
                        app.runtime_options = Some(catalog);
                        app.reconcile_remembered_runtime();
                    }
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
        AppMessage::SessionCreated { request_id, result } => {
            if !app.pending_creations.contains_key(&request_id) {
                return Ok(());
            }
            match result {
                Ok(session) if session.id == request_id => {
                    app.record_created_session(&session);
                    // A reply owns its request, never navigation. Fetch only if
                    // this identity remains selected, and leave the page alone.
                    if app.selected_session_id() == Some(&request_id) {
                        if let Ok(ticket) = app.agent.begin_session_load(request_id.clone()) {
                            worker.dispatch(crate::app::Effect::OpenSession {
                                session_id: request_id.clone(),
                                ticket,
                            });
                        }
                        app.sync_transcript();
                    }
                    if let Some(effect) = app.pending_send_effect(request_id) {
                        worker.dispatch(effect);
                    }
                }
                Ok(_) => {
                    app.fail_creation(&request_id);
                    app.toast(Toast::danger(
                        "The backend returned a different session for this creation",
                    ));
                }
                Err(error) => {
                    app.fail_creation(&request_id);
                    app.toast(Toast::danger(error.message));
                }
            }
        }
        AppMessage::SessionForked { request_id, result } => {
            let Some(navigation_serial) = app.pending_forks.remove(&request_id) else {
                return Ok(());
            };
            match result {
                Ok(session) => {
                    app.record_created_session(&session);
                    if navigation_serial == app.navigation_serial {
                        dispatch_all(worker, &app.open_session_effects(session.id));
                    }
                }
                Err(error) => app.toast(Toast::danger(error.message)),
            }
        }
        AppMessage::MessageSent {
            session_id,
            send_id,
            result,
        } => {
            let correlation_id = app
                .pending_sends
                .get(&send_id)
                .filter(|pending| pending.session_id.as_ref() == Some(&session_id))
                .map(|pending| pending.correlation_id.clone());
            app.finish_send(&session_id, send_id);
            match result {
                Ok(items) => {
                    for item in items.iter().filter(|item| {
                        item.session_id == session_id
                            && correlation_id.is_some()
                            && item.correlation_id == correlation_id
                    }) {
                        app.confirm_pending_item(item);
                    }
                    app.sync_transcript();
                }
                Err(error) => {
                    app.abandon_send(&session_id, send_id);
                    app.sync_transcript();
                    app.toast(Toast::danger(error.message));
                }
            }
        }
        AppMessage::Mutation { key, result } => {
            app.pending.remove(&key);
            match result {
                Ok(()) => {
                    if key == "resolve_permission" {
                        let message = app.strings.approval_resolved().to_string();
                        app.toast(Toast::success(message));
                    }
                    if key == "update_entry" {
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
        AppMessage::WorkspaceOpened {
            draft_id,
            navigation_serial,
            result,
        } => {
            if app.new_draft_id != draft_id || app.navigation_serial != navigation_serial {
                return Ok(());
            }
            match result {
                Ok(summary) => {
                    app.workspace_path = Some(summary.workspace.root_path.clone());
                    let outcome = app.perform(crate::action::Intent::NewSession);
                    dispatch_refresh(app, outcome);
                }
                Err(error) => app.toast(Toast::danger(error.message)),
            }
        }
        AppMessage::DirectoryListing(result) => match result {
            Ok(listing) => {
                app.set_selection(crate::keymap::Scope::Sessions, 0);
                app.workspace_browse = Some(listing);
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
                // The switcher sends a reader here for one Agent's account, and
                // the list it needs only arrives now: without this the page
                // lands on whichever Agent happens to be first.
                if let Some(agent_id) = app.pending_agent_focus.take()
                    && let Some(index) = app
                        .management_data
                        .agents
                        .iter()
                        .position(|agent| agent.id == agent_id)
                {
                    app.set_selection(crate::keymap::Scope::Management, index);
                }
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
        AppMessage::Completions { ticket, result } => {
            if !app.accepts_composer_ticket(&ticket)
                || ticket.runtime.as_deref() != app.page_runtime_selection().as_ref()
            {
                return Ok(());
            }
            match result {
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
            }
        }
        AppMessage::Usage(result) => match result {
            Ok(report) => {
                app.management_data.usage = report.aggregate;
                app.management_data.usage_session = report.session;
                app.live = LiveState::Ready;
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Clipboard(result) => {
            if let Err(error) = result {
                app.toast(Toast::warning(error.message));
            }
        }
        AppMessage::EditorFinished { ticket, result } => match result {
            Ok(Some(text)) => {
                if ticket
                    .as_ref()
                    .is_some_and(|ticket| app.accepts_composer_ticket(ticket))
                {
                    app.composer.set_text(text);
                }
            }
            Ok(None) => {}
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Event(event) => {
            // A sidebar invalidated on the authority means the tree this client
            // is drawing is out of date: the reader rearranged it somewhere
            // else, or another client did.
            if matches!(
                &event,
                vibex_backend::BackendEvent::ProjectionInvalidated(
                    vibex_backend::BackendProjection::Sidebar
                )
            ) {
                worker.dispatch(crate::app::Effect::LoadSidebarOrganization);
            }
            // The unread mark and the last thing a session did are both the
            // client's own readings of the events it already receives: the
            // event says what arrived, the list says where the reader was when
            // it did, and what the session was last seen saying.
            if let vibex_backend::BackendEvent::Timeline(item) = &event {
                app.note_session_echo(&item.item);
                if app.note_activity(item) {
                    // Deliberately not `dirty`: the frame is repainted when the
                    // event itself is applied below.
                }
            }
            if let vibex_backend::BackendEvent::Timeline(event) = &event
                && event.session_id == event.item.session_id
                && event.sequence == event.item.sequence
            {
                app.confirm_pending_item(&event.item);
            }
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
                    if let Some(effect) = app.refresh_timeline() {
                        worker.dispatch(effect);
                    }
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
    // A message is how the client learns a turn moved — a session update, a
    // timeline item, a list refresh, a mutation's answer. Auto-continue decides
    // again after every one of them, which is what keeps its behaviour tied to
    // the session list rather than to a timer of its own.
    for effect in app.sync_auto_continue() {
        worker.dispatch(effect);
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

    /// An `App` with nothing behind it, sized like a terminal.
    fn test_app(columns: u16, rows: u16) -> App {
        let mut app = App::new(
            vibex_backend::DisconnectedBackend::facade(),
            crate::app::AppOptions {
                seat: crate::view::SeatKind::Remote,
                capability: crate::theme::ColorCapability {
                    mode: crate::theme::ColorMode::TrueColor,
                    glyphs: crate::theme::GlyphMode::Unicode,
                },
                theme_id: Some("vibex-dark".to_string()),
                mode: vibex_ui::GpuiThemeMode::Dark,
                locale: crate::locale::Locale::En,
                sidebar_path: None,
                runtime_path: None,
                preferences_path: None,
                ..Default::default()
            },
        );
        app.resize(columns, rows);
        app
    }

    #[test]
    fn the_composing_page_types_into_its_own_box() {
        // The composer's text path runs before the binding table, but only on a
        // page whose scope list starts with `Composer`. The page where a session
        // is written did not, so a printable key fell through to the *Agent*
        // scope: typing did nothing, and `/` — "search the transcript" there —
        // took the reader to a session they had not opened.
        let (worker, _messages) = Worker::start(vibex_backend::DisconnectedBackend::facade())
            .expect("a worker over a client with no backend");
        let mut app = test_app(100, 30);
        app.live = crate::app::LiveState::Ready;
        app.perform(crate::action::Intent::NewSession);
        assert_eq!(app.page, crate::app::Page::NewSession);
        assert!(
            composer_takes_keys(&app),
            "the page's composer does not own the keyboard it shows"
        );

        for character in ['h', 'i', '/'] {
            let handled = handle_composer_key(
                &mut app,
                &worker,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .expect("the key is handled");
            assert_eq!(handled, Some(false), "{character} was not consumed");
        }
        assert_eq!(
            app.composer.text(),
            "hi/",
            "the page did not type into its own composer"
        );
        assert_eq!(
            app.page,
            crate::app::Page::NewSession,
            "a printable key navigated away"
        );

        // A page with no composer keeps its own bindings: the composer is not
        // a scope that swallows keys everywhere.
        app.perform(crate::action::Intent::Back);
        assert_eq!(app.page, crate::app::Page::Sessions);
        app.focus = crate::app::Focus::Composer;
        assert!(!composer_takes_keys(&app));
    }

    #[test]
    fn the_newline_chords_break_the_line_instead_of_sending() {
        // `Shift+Enter` is only a key of its own once the terminal has been
        // asked for the keyboard protocol, and `Ctrl+J` is the chord that works
        // whether it was or not. Both have to reach the composer as a line
        // break: a newline chord that submits the draft is the bug this pair
        // exists to prevent.
        let (worker, _messages) = Worker::start(vibex_backend::DisconnectedBackend::facade())
            .expect("a worker over a client with no backend");
        let mut app = test_app(100, 30);
        app.live = crate::app::LiveState::Ready;
        app.perform(crate::action::Intent::NewSession);
        assert!(
            composer_takes_keys(&app),
            "the composer does not own its keys"
        );

        // What a protocol terminal sends for `Shift+Enter`, through the same
        // normalisation every key goes through.
        let key = normalized_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(
            handle_composer_key(&mut app, &worker, key).expect("the key is handled"),
            Some(false),
            "Shift+Enter was not consumed"
        );

        app.composer.insert_char('x');
        // What a terminal without the protocol sends for `Ctrl+J`: the binding
        // table, which is where a control chord is resolved.
        let chord = Chord::from_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(
            app.keymap.resolve(&[crate::keymap::Scope::Composer], chord),
            Some(crate::action::Intent::InsertNewline),
            "Ctrl+J is not the newline chord in the composer"
        );
        app.perform(crate::action::Intent::InsertNewline);

        assert_eq!(app.composer.text(), "\nx\n");
        assert_eq!(
            app.page,
            crate::app::Page::NewSession,
            "a newline chord submitted the draft"
        );
    }

    #[test]
    fn a_shifted_letter_is_text_not_a_modifier() {
        // A terminal that implements the protocol's disambiguation without its
        // alternate keys reports `Shift+a` as `Char('a')` + `Shift`. Reading the
        // character as it arrives would type a lowercase letter for an
        // uppercase key, so the modifier is folded in at the door.
        let folded = normalized_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT));
        assert_eq!(folded.code, KeyCode::Char('A'));
        assert_eq!(folded.modifiers, KeyModifiers::NONE);

        // The alternate-keycode form the protocol does send is already the
        // uppercase character, and is left alone.
        let reported = normalized_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
        assert_eq!(reported.code, KeyCode::Char('A'));
        assert_eq!(reported.modifiers, KeyModifiers::NONE);

        // Everything else keeps its modifiers: an unshifted letter, a chord the
        // protocol disambiguates (`Shift+Enter`), and a character the ASCII
        // fold cannot answer for.
        for key in [
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('é'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::SHIFT),
        ] {
            assert_eq!(normalized_key(key), key, "{key:?} was rewritten");
        }
    }

    #[test]
    fn a_pointer_outside_a_band_is_not_a_row_in_it() {
        // The terminal reports motion for the whole window, so most of it is
        // outside every list. The row arithmetic used to run whatever the
        // containment check said — `then_some` evaluates its argument — and a
        // pointer above the queue band subtracted past zero and killed the
        // client with an overflow.
        let mut app = test_app(120, 40);
        app.navigate_to(crate::app::Page::Agent);
        app.regions.queue = Some(ratatui::layout::Rect {
            x: 2,
            y: 30,
            width: 116,
            height: 1,
        });
        assert_eq!(hover_target(&app, 40, 2), None, "a row above the band");
        assert_eq!(
            hover_target(&app, 200, 2),
            None,
            "a column outside the band"
        );
        assert_eq!(
            hover_target(&app, 40, 30),
            Some((crate::keymap::Scope::Agent, 0))
        );
        assert_eq!(hover_target(&app, 40, 31), None, "a row below the band");
    }
    fn isolation_app() -> App {
        let facade = vibex_backend::DisconnectedBackend::facade();
        facade.replace_capabilities(vibex_backend::BackendCapabilitySnapshot::desktop_native_v1());
        let mut app = App::new(
            facade,
            crate::app::AppOptions {
                sidebar_path: None,
                runtime_path: None,
                preferences_path: None,
                ..Default::default()
            },
        );
        app.live = LiveState::Ready;
        app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
            revision: 1,
            agents: vec![],
            auth_sources: vec![],
            options: ["codex", "deepseek-harness"]
                .into_iter()
                .map(|agent| vibex_core::SessionRuntimeOption {
                    selection: vibex_core::SessionRuntimeSelection::provider(
                        vibex_core::AgentId::parse(agent).unwrap(),
                        vibex_core::ProviderProfileId::new(),
                        "test-model",
                    ),
                    agent_label: agent.into(),
                    auth_source_label: "test".into(),
                    model_label: "test-model".into(),
                    reasoning_efforts: vec![],
                    modes: vec![],
                    features: vec![],
                    availability: vibex_core::RuntimeOptionAvailability::Available,
                })
                .collect(),
        });
        app
    }

    fn isolation_session(id: vibex_core::VibexSessionId, agent: &str) -> vibex_core::AgentSession {
        vibex_core::AgentSession {
            id,
            title: agent.into(),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/test/project".into(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse(agent).unwrap(),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn isolation_create(app: &mut App, text: &str, agent: usize) -> vibex_core::VibexSessionId {
        app.perform(crate::action::Intent::NewSession);
        app.new_session_runtime = Some(
            app.runtime_options.as_ref().unwrap().options[agent]
                .selection
                .clone(),
        );
        app.composer.set_text(text);
        let outcome = app.perform(crate::action::Intent::SubmitComposer);
        outcome
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                crate::app::Effect::CreateSession { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap()
    }

    fn isolation_highlight_runtime(app: &mut App, index: usize) {
        let row = app
            .runtime_picker_rows()
            .iter()
            .position(|row| row.entry() == Some(index))
            .unwrap();
        app.select_runtime_picker_row(row, false);
    }

    fn isolation_worker() -> Worker {
        Worker::start(vibex_backend::DisconnectedBackend::facade())
            .unwrap()
            .0
    }

    fn conversation_item(
        session_id: &vibex_core::VibexSessionId,
        sequence: i64,
        payload: vibex_core::TimelinePayload,
    ) -> vibex_core::TimelineItem {
        vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: sequence,
            source: vibex_core::TimelineSource::Agent,
            kind: match payload {
                vibex_core::TimelinePayload::UserMessage(_) => {
                    vibex_core::TimelineItemKind::UserMessage
                }
                vibex_core::TimelinePayload::ToolCall(_) => vibex_core::TimelineItemKind::ToolCall,
                _ => vibex_core::TimelineItemKind::AgentMessageDelta,
            },
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        }
    }

    fn conversation_frame(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn live_callbacks_show_a_growing_answer_and_settle_without_reflow() {
        use vibex_core::{AgentMessageDeltaPayload, TimelinePayload};
        let worker = isolation_worker();
        for (width, height) in [(80, 24), (120, 40)] {
            let mut app = isolation_app();
            app.resize(width, height);
            app.navigate_to(Page::Agent);
            let id = vibex_core::VibexSessionId::new();
            let mut session = isolation_session(id.clone(), "codex");
            session.state = vibex_core::AgentSessionState::Running;
            app.agent.state.selected_session_id = Some(id.clone());
            app.agent.state.active_session.resolve(session.clone());
            app.agent
                .state
                .timeline
                .replace_authoritative(id.clone(), vec![]);
            for sequence in 1..=30 {
                let item = conversation_item(
                    &id,
                    sequence,
                    TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                        text_delta: format!(
                            "Paragraph {sequence}: 中文流式内容 **keeps arriving**.\n\n"
                        ),
                        chunk_index: sequence as u32 - 1,
                        phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
                    }),
                );
                apply_message(
                    &mut app,
                    &worker,
                    AppMessage::Event(vibex_backend::BackendEvent::Timeline(
                        vibex_core::TimelineLiveEvent {
                            session_id: id.clone(),
                            sequence,
                            item,
                        },
                    )),
                )
                .unwrap();
                let screen = conversation_frame(&mut app, width, height);
                assert!(
                    screen.contains(&format!("Paragraph {sequence}:")),
                    "latest delta did not reach the screen: {screen}"
                );
                assert_eq!(app.transcript.len(), 1);
            }
            let before = app
                .transcript
                .visible_lines(app.scroll, 1000, &app.theme, app.strings);
            session.state = vibex_core::AgentSessionState::Idle;
            session.updated_at_ms += 1;
            apply_message(
                &mut app,
                &worker,
                AppMessage::Event(vibex_backend::BackendEvent::SessionUpdated(session)),
            )
            .unwrap();
            let after = app
                .transcript
                .visible_lines(app.scroll, 1000, &app.theme, app.strings);
            assert_eq!(
                before, after,
                "completing a turn changed its Markdown layout"
            );
            assert!(!app.transcript_animating());
        }
    }

    #[test]
    fn a_gap_starts_one_refetch_and_streaming_resumes_after_the_snapshot() {
        use vibex_core::{AgentMessageDeltaPayload, TimelinePayload};
        let worker = isolation_worker();
        let mut app = isolation_app();
        let id = vibex_core::VibexSessionId::new();
        let mut session = isolation_session(id.clone(), "codex");
        session.state = vibex_core::AgentSessionState::Running;
        app.agent.state.selected_session_id = Some(id.clone());
        app.agent.state.active_session.resolve(session.clone());
        let item = |sequence, text: &str| {
            conversation_item(
                &id,
                sequence,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: text.into(),
                    chunk_index: sequence as u32 - 1,
                    phase: None,
                }),
            )
        };
        app.agent
            .state
            .timeline
            .replace_authoritative(id.clone(), vec![item(1, "one ")]);
        app.perform(crate::action::Intent::NewSession);
        app.composer.set_text("keep this draft");
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::Timeline(
                vibex_core::TimelineLiveEvent {
                    session_id: id.clone(),
                    sequence: 3,
                    item: item(3, "three "),
                },
            )),
        )
        .unwrap();
        assert_eq!(
            app.agent.state.timeline_status.phase,
            vibex_ui::AsyncPhase::Loading
        );
        assert!(
            app.refresh_timeline().is_none(),
            "a refetch is already in flight"
        );
        let ticket = vibex_ui::AgentSessionLoadTicket {
            generation: app.agent.state.generation,
            session_id: id.clone(),
            after_sequence: 0,
        };
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionOpened {
                ticket,
                result: Ok(vibex_ui::AgentSessionSnapshot {
                    session,
                    timeline: vec![item(1, "one "), item(2, "two "), item(3, "three ")],
                    runtime_selection: None,
                    timeline_has_older: false,
                }),
            },
        )
        .unwrap();
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::Timeline(
                vibex_core::TimelineLiveEvent {
                    session_id: id.clone(),
                    sequence: 4,
                    item: item(4, "four"),
                },
            )),
        )
        .unwrap();
        assert!(!app.agent.state.timeline.needs_authoritative_refetch);
        assert!(
            app.transcript
                .block(0)
                .unwrap()
                .body
                .contains("one two three four")
        );
        assert_eq!(app.page, Page::NewSession);
        assert_eq!(app.composer.text(), "keep this draft");
    }

    /// The wheel moves the session list's window and leaves the cursor where
    /// the reader put it: a list that answers only the arrow keys is a list
    /// that cannot be scanned.
    #[test]
    fn the_wheel_scrolls_the_session_list() {
        let worker = isolation_worker();
        let mut app = test_app(100, 24);
        let sessions = (1..=30)
            .map(|index| vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse(format!("session_wheel{index:04}"))
                    .expect("valid session id"),
                title: format!("wheelable {index}"),
                project_id: vibex_core::ProjectId::parse("project_wheel001")
                    .expect("valid project id"),
                // One workspace for all of them: a fresh id per session would
                // scatter them into headings whose order is random, and this
                // test is about the window, not about the arrangement.
                workspace_id: vibex_core::WorkspaceId::parse("workspace_wheel01")
                    .expect("valid workspace id"),
                workspace_root: "/tmp/vibex-wheel".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: index,
                updated_at_ms: index,
                last_message_at_ms: index,
                archived_at_ms: None,
                deleted_at_ms: None,
            })
            .collect::<Vec<_>>();
        app.agent.apply_sessions(Ok(sessions)).expect("apply");
        app.navigate_to(crate::app::Page::Sessions);
        let before_screen = conversation_frame(&mut app, 100, 24);
        assert!(
            before_screen.contains("wheelable"),
            "no session is drawn:\n{before_screen}"
        );
        let before = app.session_scroll;
        let selected = app.selection_for(crate::keymap::Scope::Sessions);
        for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollDown] {
            handle_mouse(
                &mut app,
                &worker,
                MouseEvent {
                    kind,
                    column: 10,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        let after_screen = conversation_frame(&mut app, 100, 24);
        assert!(
            app.session_scroll > before,
            "the wheel did not move the list's window"
        );
        assert_eq!(
            app.selection_for(crate::keymap::Scope::Sessions),
            selected,
            "the wheel moved the cursor"
        );
        assert_ne!(
            after_screen, before_screen,
            "the wheel moved nothing on screen"
        );
    }

    /// A click on a heading folds it, the way a tree folds everywhere else. A
    /// session keeps the two-step contract, because opening one leaves the
    /// page.
    #[test]
    fn a_single_click_folds_a_session_list_heading() {
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        let sessions = (1..=2)
            .map(|index| vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse(format!("session_click{index:04}"))
                    .expect("valid session id"),
                title: format!("clickable {index}"),
                project_id: vibex_core::ProjectId::parse("project_click0001")
                    .expect("valid project id"),
                workspace_id: vibex_core::WorkspaceId::new(),
                workspace_root: "/tmp/vibex-click-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: index,
                updated_at_ms: index,
                last_message_at_ms: index,
                archived_at_ms: None,
                deleted_at_ms: None,
            })
            .collect::<Vec<_>>();
        app.agent.apply_sessions(Ok(sessions)).expect("apply");
        app.navigate_to(crate::app::Page::Sessions);
        let screen = conversation_frame(&mut app, 100, 30);
        assert!(
            screen.contains("vibex-click-workspace") && screen.contains("clickable 1"),
            "the tree is not on screen:\n{screen}"
        );

        // Row 0 is the workspace heading; the two sessions follow it.
        let list = app.regions.list.clone().expect("the list is clickable");
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: list.rect.x + 3,
                row: list.rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let folded = conversation_frame(&mut app, 100, 30);
        assert!(
            folded.contains("vibex-click-workspace"),
            "the heading went with its members:\n{folded}"
        );
        assert!(
            !folded.contains("clickable 1") && !folded.contains("clickable 2"),
            "one click did not fold the heading:\n{folded}"
        );
        // The same click again opens it.
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: list.rect.x + 3,
                row: list.rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let opened = conversation_frame(&mut app, 100, 30);
        assert!(opened.contains("clickable 1"), "{opened}");
    }

    /// A list rect describes the frame that drew it, so it must not outlive
    /// that page: a click on the next page would otherwise land on rows that
    /// are no longer on screen and open the session under them.
    #[test]
    fn a_list_rect_does_not_outlive_the_page_that_drew_it() {
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        let sessions = (1..=2)
            .map(|index| vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse(format!("session_stale{index:04}"))
                    .expect("valid session id"),
                title: format!("stale {index}"),
                project_id: vibex_core::ProjectId::parse("project_stale0001")
                    .expect("valid project id"),
                workspace_id: vibex_core::WorkspaceId::new(),
                workspace_root: "/tmp/vibex-stale-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: index,
                updated_at_ms: index,
                last_message_at_ms: index,
                archived_at_ms: None,
                deleted_at_ms: None,
            })
            .collect::<Vec<_>>();
        app.agent.apply_sessions(Ok(sessions)).expect("apply");
        app.navigate_to(Page::Sessions);
        conversation_frame(&mut app, 100, 30);
        let list = app
            .regions
            .list
            .clone()
            .expect("the sessions page drew a list");

        // The page that drew the list is gone, so its rows went with it.
        app.navigate_to(Page::Agent);
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.list.is_none(),
            "the session list's rect outlived the page that drew it"
        );

        // A click where the list used to be is a transcript gesture, not a
        // second click on a row nobody is looking at: the session cursor does
        // not move, and no session is opened behind the reader's back.
        let before = (
            app.selection_for(crate::keymap::Scope::Sessions),
            app.selected_session_id().cloned(),
        );
        for _ in 0..2 {
            handle_mouse(
                &mut app,
                &worker,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: list.rect.x + 3,
                    row: list.rect.y + 2,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        assert_eq!(
            (
                app.selection_for(crate::keymap::Scope::Sessions),
                app.selected_session_id().cloned()
            ),
            before,
            "a click on the transcript moved the session list's cursor"
        );
    }

    /// The modal's close affordance is the one control a modal hands the mouse,
    /// and it is a control only while the modal is drawn: the frame publishes
    /// the rect, and a click where it used to be is not a click on it.
    #[test]
    fn a_modal_close_rect_dies_with_the_modal() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        app.overlay = Some(Overlay::Palette {
            query: String::new(),
            selected: 0,
        });
        conversation_frame(&mut app, 100, 30);
        let close = app
            .regions
            .modal_close
            .expect("the drawn modal published no close affordance");

        // The affordance answers while the modal is up.
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.overlay.is_none(), "the close affordance did nothing");

        // A modal also leaves by the keyboard, and its rect goes with it: a
        // frame that draws no modal publishes no close.
        app.overlay = Some(Overlay::Palette {
            query: String::new(),
            selected: 0,
        });
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.modal_close.is_some(),
            "the reopened modal published no close affordance"
        );
        app.perform(Intent::Back);
        assert!(app.overlay.is_none(), "the chord did not close the modal");
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.modal_close.is_none(),
            "the close rect outlived the modal that drew it"
        );

        // The next click at that cell is not a second close, because there is
        // nothing there to close.
        let page = app.page;
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.page, page,
            "a closed modal's rect still ran the close intent"
        );
    }

    /// The banner's row is a control only while the banner is up: once it is
    /// gone, a click there belongs to whatever the frame drew in its place.
    #[test]
    fn a_banner_rect_dies_with_the_banner() {
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        app.set_banner(crate::app::Banner::info("a notice"));
        conversation_frame(&mut app, 100, 30);
        let banner = app
            .regions
            .banner
            .expect("the drawn banner published no row");

        // The banner goes, and its row goes back to the transcript.
        app.banner = None;
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.banner.is_none(),
            "the banner's row outlived the banner"
        );
        app.clear_text_selection();
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: banner.x + 1,
                row: banner.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.text_selection.is_some(),
            "the click was swallowed by a banner that is gone"
        );
    }

    /// The composer's box belongs to the pages that draw one: a click where it
    /// used to be must not take the keyboard for a box nobody can see.
    #[test]
    fn a_composer_rect_dies_with_the_page_that_drew_it() {
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        conversation_frame(&mut app, 100, 30);
        let composer = app
            .regions
            .composer
            .expect("the session page drew no composer");

        // The settings page has no composer, so the box goes with the page.
        app.navigate_to(Page::Settings);
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.composer.is_none() && app.regions.composer_band.is_none(),
            "the composer's box outlived the page that drew it"
        );
        assert_eq!(app.focus, Focus::Main);

        // The click lands on the row the box used to start on, which no other
        // control the page published covers.
        let cell = (composer.x + 1, composer.y);
        assert!(
            !app.regions
                .hints
                .iter()
                .any(|(rect, _)| rect_contains(*rect, cell.0, cell.1)),
            "the test clicked a control the page drew"
        );
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: cell.0,
                row: cell.1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.focus,
            Focus::Main,
            "a composer that is gone took the keyboard"
        );
    }

    #[test]
    fn mouse_opens_a_group_and_keyboard_skips_its_folded_members() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        let mut items = (1..=3)
            .map(|sequence| {
                conversation_item(
                    &id,
                    sequence,
                    vibex_core::TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
                        tool_call_id: format!("tool-{sequence}"),
                        tool_name: "read".into(),
                        status: vibex_core::ToolCallStatus::Completed,
                        summary: String::new(),
                        input_summary: Some(format!("src/file-{sequence}.rs")),
                        output_summary: Some(format!("contents of file {sequence}")),
                        raw_extension: None,
                    }),
                )
            })
            .collect::<Vec<_>>();
        items.push(conversation_item(
            &id,
            4,
            vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
                text_delta: "The result is ready.".into(),
                chunk_index: 0,
                phase: None,
            }),
        ));
        app.agent.state.timeline.replace_authoritative(id, items);
        app.sync_transcript();
        conversation_frame(&mut app, 100, 30);
        app.focus = Focus::Main;
        app.set_selection(crate::keymap::Scope::Agent, 0);
        app.perform(Intent::SelectNext);
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 3);
        app.perform(Intent::SelectPrevious);
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 0);
        conversation_frame(&mut app, 100, 30);
        let area = app.regions.scrollback;
        handle_mouse(
            &mut app,
            &worker,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 3,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let opened = conversation_frame(&mut app, 100, 30);
        assert!(opened.contains("contents of file 3"));
        app.sync_transcript();
        assert!(
            app.transcript.blocks()[..3]
                .iter()
                .all(|block| block.expanded)
        );
        app.perform(Intent::ToggleAllBlocksExpanded);
        app.focus = Focus::Composer;
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.focus, Focus::Main);
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 3);
    }

    #[test]
    fn late_open_callback_cannot_leave_draft_or_retarget_picker() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        let id = vibex_core::VibexSessionId::new();
        let load = app.open_session_effects(id.clone());
        let ticket = load
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                crate::app::Effect::OpenSession { ticket, .. } => Some(ticket),
                _ => None,
            })
            .unwrap();
        app.perform(Intent::NewSession);
        app.show_runtime_picker();
        isolation_highlight_runtime(&mut app, 1);
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionOpened {
                ticket,
                result: Ok(vibex_ui::AgentSessionSnapshot {
                    session: isolation_session(id.clone(), "codex"),
                    timeline: vec![],
                    runtime_selection: None,
                    timeline_has_older: false,
                }),
            },
        )
        .unwrap();
        assert_eq!(app.page, crate::app::Page::NewSession);
        let outcome = app.perform(Intent::ConfirmOverlay);
        assert!(
            outcome.effects.is_empty(),
            "a draft choice switched an existing runtime"
        );
        assert_eq!(
            app.new_session_runtime.as_ref().unwrap().agent_id.as_str(),
            "deepseek-harness"
        );
        assert_eq!(app.active_session().unwrap().agent_id.as_str(), "codex");
        assert_eq!(app.selected_session_id(), Some(&id));
    }

    #[test]
    fn session_snapshot_and_failure_callbacks_preserve_global_navigation() {
        let worker = isolation_worker();
        for fail in [false, true] {
            let mut app = isolation_app();
            let id = vibex_core::VibexSessionId::new();
            let load = app.open_session_effects(id.clone());
            let ticket = load
                .effects
                .into_iter()
                .find_map(|effect| match effect {
                    crate::app::Effect::OpenSession { ticket, .. } => Some(ticket),
                    _ => None,
                })
                .unwrap();
            app.perform(crate::action::Intent::GotoSessions);
            let result = if fail {
                Err(vibex_backend::BackendError::failed("test", "load failed"))
            } else {
                Ok(vibex_ui::AgentSessionSnapshot {
                    session: isolation_session(id, "codex"),
                    timeline: vec![],
                    runtime_selection: None,
                    timeline_has_older: false,
                })
            };
            apply_message(
                &mut app,
                &worker,
                AppMessage::SessionOpened { ticket, result },
            )
            .unwrap();
            assert_eq!(app.page, crate::app::Page::Sessions);
            assert!(app.transcript.blocks().is_empty());
        }
    }

    #[test]
    fn creation_callbacks_are_correlated_and_duplicates_do_nothing() {
        let worker = isolation_worker();
        for reverse in [false, true] {
            let mut app = isolation_app();
            let a = isolation_create(&mut app, "first prompt", 0);
            let send_a = app.pending_creations[&a].send_id;
            let b = isolation_create(&mut app, "second prompt", 1);
            let send_b = app.pending_creations[&b].send_id;
            let order = if reverse {
                vec![b.clone(), a.clone()]
            } else {
                vec![a.clone(), b.clone()]
            };
            for request_id in order {
                let agent = if request_id == a {
                    "codex"
                } else {
                    "deepseek-harness"
                };
                let session = isolation_session(request_id.clone(), agent);
                apply_message(
                    &mut app,
                    &worker,
                    AppMessage::SessionCreated {
                        request_id: request_id.clone(),
                        result: Ok(session.clone()),
                    },
                )
                .unwrap();
                assert_eq!(app.selected_session_id(), Some(&b));
                let serial = app.navigation_serial;
                apply_message(
                    &mut app,
                    &worker,
                    AppMessage::SessionCreated {
                        request_id,
                        result: Ok(session),
                    },
                )
                .unwrap();
                assert_eq!(app.navigation_serial, serial);
            }
            assert!(app.pending_creations.is_empty());
            assert_eq!(app.pending_sends[&send_a].text, "first prompt");
            assert_eq!(app.pending_sends[&send_a].session_id.as_ref(), Some(&a));
            assert_eq!(app.pending_sends[&send_b].text, "second prompt");
            assert_eq!(app.pending_sends[&send_b].session_id.as_ref(), Some(&b));
            assert_eq!(app.history.entries().len(), 2);
        }
    }

    #[test]
    fn failed_creation_preserves_newer_draft_and_explicit_retry_gets_new_identity() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        let a = isolation_create(&mut app, "recover me", 0);
        app.perform(Intent::NewSession);
        app.composer.set_text("newer draft");
        app.new_session_runtime = Some(
            app.runtime_options.as_ref().unwrap().options[1]
                .selection
                .clone(),
        );
        app.workspace_path = Some("/different/project".into());
        let draft_b = app.new_draft_id.clone();
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionCreated {
                request_id: a.clone(),
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "failed creation",
                )),
            },
        )
        .unwrap();
        assert_eq!(app.composer.text(), "newer draft");
        assert_eq!(app.new_draft_id, draft_b);
        assert_eq!(app.workspace_path.as_deref(), Some("/different/project"));
        assert_eq!(app.failed_creations.len(), 1);
        let b = app.perform(Intent::SubmitComposer);
        assert!(!b.effects.is_empty());
        app.perform(Intent::NewSession);
        assert_eq!(app.composer.text(), "recover me");
        assert_eq!(
            app.new_session_runtime.as_ref().unwrap().agent_id.as_str(),
            "codex"
        );
        assert_ne!(app.new_draft_id, a);
        let retry = app.new_draft_id.clone();
        app.perform(Intent::SubmitComposer);
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionCreated {
                request_id: a.clone(),
                result: Ok(isolation_session(a, "codex")),
            },
        )
        .unwrap();
        assert!(app.pending_creations.contains_key(&retry));
    }

    #[test]
    fn fork_reply_cannot_consume_prompt_or_navigate_after_another_creation() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let fork = vibex_core::VibexSessionId::new();
        app.pending_forks
            .insert(fork.clone(), app.navigation_serial);
        let created = isolation_create(&mut app, "new prompt", 1);
        let forked_session = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionForked {
                request_id: fork,
                result: Ok(forked_session),
            },
        )
        .unwrap();
        assert!(app.pending_creations.contains_key(&created));
        assert_eq!(app.selected_session_id(), Some(&created));
        assert_eq!(app.pending_send_for_active().unwrap().text, "new prompt");
    }

    #[test]
    fn send_error_callback_only_removes_its_own_session_and_attempt() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let a = vibex_core::VibexSessionId::new();
        let old_send = app.mark_send_dispatched(Some(&a), "old".into(), vec![]);
        let new_send = app.mark_send_dispatched(Some(&a), "new".into(), vec![]);
        let b = vibex_core::VibexSessionId::new();
        app.open_session_effects(b.clone());
        let other_send = app.mark_send_dispatched(Some(&b), "other session".into(), vec![]);
        apply_message(
            &mut app,
            &worker,
            AppMessage::MessageSent {
                session_id: a.clone(),
                send_id: old_send,
                result: Err(vibex_backend::BackendError::failed("test", "send failed")),
            },
        )
        .unwrap();
        assert!(!app.pending_sends.contains_key(&old_send));
        assert!(app.pending_sends.contains_key(&new_send));
        assert!(app.pending_sends.contains_key(&other_send));
        assert_eq!(app.pending_send_for_active().unwrap().text, "other session");
        apply_message(
            &mut app,
            &worker,
            AppMessage::MessageSent {
                session_id: a,
                send_id: other_send,
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "mismatched callback",
                )),
            },
        )
        .unwrap();
        assert!(app.pending_sends.contains_key(&other_send));
    }

    #[test]
    fn wrong_created_identity_recovers_draft_without_sending() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let request_id = isolation_create(&mut app, "keep this", 1);
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionCreated {
                request_id,
                result: Ok(isolation_session(
                    vibex_core::VibexSessionId::new(),
                    "codex",
                )),
            },
        )
        .unwrap();
        assert!(app.pending_creations.is_empty());
        assert!(app.pending_sends.is_empty());
        assert_eq!(app.composer.text(), "keep this");
        assert_eq!(app.page, crate::app::Page::NewSession);
    }

    #[test]
    fn deferred_catalogue_neither_reopens_cancelled_picker_nor_reorders_open_choices() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        let mut catalog = app.runtime_options.take().unwrap();
        app.perform(Intent::NewSession);
        app.perform(Intent::SwitchAgentRuntime);
        app.perform(Intent::GotoSessions);
        apply_message(
            &mut app,
            &worker,
            AppMessage::RuntimeOptions(Ok(catalog.clone())),
        )
        .unwrap();
        assert!(app.overlay.is_none());
        app.perform(Intent::NewSession);
        app.show_runtime_picker();
        isolation_highlight_runtime(&mut app, 1);
        catalog.options.reverse();
        apply_message(&mut app, &worker, AppMessage::RuntimeOptions(Ok(catalog))).unwrap();
        app.perform(Intent::ConfirmOverlay);
        assert_eq!(
            app.new_session_runtime.as_ref().unwrap().agent_id.as_str(),
            "deepseek-harness"
        );
    }
    #[test]
    fn late_composer_callbacks_cannot_edit_another_draft() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.perform(Intent::NewSession);
        app.composer.set_text("first draft");
        let ticket = app.composer_ticket();
        isolation_create(&mut app, "submitted first", 0);
        app.perform(Intent::NewSession);
        app.composer.set_text("second draft");
        let messages = [
            AppMessage::Pasted {
                ticket: ticket.clone(),
                content: crate::worker::ClipboardContent::Text("stale paste".into()),
            },
            AppMessage::ClipboardImage {
                ticket: ticket.clone(),
                image: Some(("image/png".into(), vec![1, 2, 3])),
            },
            AppMessage::EditorFinished {
                ticket: Some(ticket.clone()),
                result: Ok(Some("stale external edit".into())),
            },
            AppMessage::Completions {
                ticket,
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "stale completion",
                )),
            },
        ];
        app.toast = None;
        for message in messages {
            apply_message(&mut app, &worker, message).unwrap();
        }
        assert_eq!(app.composer.text(), "second draft");
        assert_eq!(app.composer.image_count(), 0);
        assert!(app.toast.is_none());
        assert!(app.overlay.is_none());
    }

    #[test]
    fn composer_callback_requires_unchanged_input_even_with_same_owner() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.perform(crate::action::Intent::NewSession);
        app.composer.set_text("draft before edit");
        let ticket = app.composer_ticket();
        app.composer.set_text("draft after edit");
        apply_message(
            &mut app,
            &worker,
            AppMessage::EditorFinished {
                ticket: Some(ticket),
                result: Ok(Some("stale edited text".into())),
            },
        )
        .unwrap();
        assert_eq!(app.composer.text(), "draft after edit");
        let ticket = app.composer_ticket();
        apply_message(
            &mut app,
            &worker,
            AppMessage::Pasted {
                ticket,
                content: crate::worker::ClipboardContent::Text(" accepted".into()),
            },
        )
        .unwrap();
        assert_eq!(app.composer.text(), "draft after edit accepted");
    }

    #[test]
    fn workspace_callback_cannot_move_a_newer_draft_or_page() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.perform(Intent::NewSession);
        let draft_id = app.new_draft_id.clone();
        let navigation_serial = app.navigation_serial;
        app.perform(Intent::GotoSessions);
        app.perform(Intent::NewSession);
        app.workspace_path = Some("/newer/workspace".into());
        app.composer.set_text("newer work");
        let project_id = vibex_core::ProjectId::new();
        let summary = vibex_backend::WorkspaceSummary {
            project: vibex_core::ProjectRecord {
                id: project_id.clone(),
                name: "old".into(),
                root_path: "/old".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            workspace: vibex_core::WorkspaceRecord {
                id: vibex_core::WorkspaceId::new(),
                project_id,
                root_path: "/old".into(),
                mode: vibex_core::WorkspaceMode::CurrentCheckout,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            git_branch: None,
        };
        apply_message(
            &mut app,
            &worker,
            AppMessage::WorkspaceOpened {
                draft_id,
                navigation_serial,
                result: Ok(summary),
            },
        )
        .unwrap();
        assert_eq!(app.workspace_path.as_deref(), Some("/newer/workspace"));
        assert_eq!(app.composer.text(), "newer work");
    }

    #[test]
    fn slow_creation_and_send_expiry_never_release_followup_before_send_ack() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let id = isolation_create(&mut app, "first", 0);
        let send_id = app.pending_creations[&id].send_id;
        app.pending_sends.get_mut(&send_id).unwrap().submitted_at =
            std::time::Instant::now() - Duration::from_secs(100);
        app.enqueue(id.clone(), "followup".into(), vec![]);
        assert!(!app.settle_pending_send());
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionCreated {
                request_id: id.clone(),
                result: Ok(isolation_session(id.clone(), "codex")),
            },
        )
        .unwrap();
        assert!(app.pending_sends[&send_id].submitted_at.elapsed() < Duration::from_secs(1));
        assert!(app.drain_queue().is_empty());
        app.pending_sends.get_mut(&send_id).unwrap().submitted_at =
            std::time::Instant::now() - Duration::from_secs(100);
        assert!(app.settle_pending_send());
        assert!(app.pending_send_for_active().is_none());
        assert!(
            app.drain_queue().is_empty(),
            "display timeout released a still-running RPC"
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::MessageSent {
                session_id: id,
                send_id,
                result: Ok(vec![]),
            },
        )
        .unwrap();
        assert_eq!(app.drain_queue().len(), 1);
    }
    #[test]
    fn background_send_confirms_from_its_reply_or_event_without_opening_it() {
        let worker = isolation_worker();
        for via_event in [false, true] {
            let mut app = isolation_app();
            let background = vibex_core::VibexSessionId::new();
            let visible = vibex_core::VibexSessionId::new();
            app.open_session_effects(visible.clone());
            let send_id = app.mark_send_dispatched(Some(&background), "same words".into(), vec![]);
            let correlation_id = app.pending_sends[&send_id].correlation_id.clone();
            app.enqueue(background.clone(), "next".into(), vec![]);
            let item = vibex_core::TimelineItem {
                id: vibex_core::TimelineItemId::new(),
                session_id: background.clone(),
                sequence: 8,
                timestamp_ms: 1,
                source: vibex_core::TimelineSource::User,
                kind: vibex_core::TimelineItemKind::UserMessage,
                correlation_id: Some(correlation_id),
                provider_correlation_id: None,
                redaction_state: vibex_core::TimelineRedactionState::None,
                execution_attribution: None,
                payload: vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                    text: "same words".into(),
                    ..Default::default()
                }),
            };
            if via_event {
                apply_message(
                    &mut app,
                    &worker,
                    AppMessage::Event(vibex_backend::BackendEvent::Timeline(
                        vibex_core::TimelineLiveEvent {
                            session_id: background.clone(),
                            sequence: 8,
                            item: item.clone(),
                        },
                    )),
                )
                .unwrap();
                assert!(!app.pending_sends.contains_key(&send_id));
                assert!(
                    app.drain_queue().is_empty(),
                    "echo is not the RPC acknowledgement"
                );
            }
            apply_message(
                &mut app,
                &worker,
                AppMessage::MessageSent {
                    session_id: background,
                    send_id,
                    result: Ok(vec![item]),
                },
            )
            .unwrap();
            assert!(!app.pending_sends.contains_key(&send_id));
            assert_eq!(app.selected_session_id(), Some(&visible));
            assert_eq!(app.drain_queue().len(), 1);
        }
    }

    #[test]
    fn completion_from_another_agent_cannot_finish_current_menu() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.perform(crate::action::Intent::NewSession);
        app.composer.set_text("/a");
        app.refresh_completion();
        let ticket = app.composer_ticket();
        app.new_session_runtime = Some(
            app.runtime_options.as_ref().unwrap().options[1]
                .selection
                .clone(),
        );
        app.toast = None;
        apply_message(
            &mut app,
            &worker,
            AppMessage::Completions {
                ticket,
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "old agent discovery",
                )),
            },
        )
        .unwrap();
        assert!(app.toast.is_none());
        assert!(app.completion.as_ref().unwrap().loading);
    }
    #[test]
    fn remembered_default_and_late_catalogue_do_not_block_creation_recovery() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.perform(Intent::NewSession);
        app.show_runtime_picker();
        isolation_highlight_runtime(&mut app, 1);
        app.perform(Intent::ConfirmOverlay);
        app.composer.set_text("recover remembered agent");
        app.perform(Intent::SubmitComposer);
        let request_id = app.selected_session_id().unwrap().clone();
        let catalog = app.runtime_options.clone().unwrap();
        apply_message(&mut app, &worker, AppMessage::RuntimeOptions(Ok(catalog))).unwrap();
        assert!(
            app.new_session_runtime.is_none(),
            "a default became an explicit draft edit"
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionCreated {
                request_id,
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "creation failed",
                )),
            },
        )
        .unwrap();
        assert_eq!(app.page, crate::app::Page::NewSession);
        assert_eq!(app.composer.text(), "recover remembered agent");
        assert_eq!(
            app.page_runtime_selection().unwrap().agent_id.as_str(),
            "deepseek-harness"
        );
        app.perform(Intent::SubmitComposer);
        app.perform(Intent::NewSession);
        assert!(app.new_session_runtime.is_none());
        assert_eq!(
            app.page_runtime_selection().unwrap().agent_id.as_str(),
            "deepseek-harness"
        );
    }
}
