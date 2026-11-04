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

use crate::app::{
    App, ClipboardWanted, Effect, Focus, LiveState, Overlay, Page, PromptField, Toast,
};
use crate::config_center::{gateway_provider_label, gateway_requested_model_label};
use crate::keymap::Chord;
use crate::reduce::Outcome;
use crate::terminal::{TerminalGuard, with_terminal_restored};
use crate::worker::{AppMessage, ClipboardContent, Worker};

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
    /// The reader took the host's offer to own the runtime home.
    ///
    /// Ending the run is all this crate can do about it: becoming the authority
    /// means starting the runtime, which is the composition root's job.
    Handover,
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
    // The size the frame on the terminal was drawn at, which is what says that
    // the backend cleared the screen — and with it every picture — since the
    // frame before. `None` until the first frame: a screen nothing has been
    // drawn on has no pictures to forget, and this way the first frame is the
    // one that admits it knows nothing.
    let mut drawn_size: Option<(u16, u16)> = None;
    // Whether the Agent's blocking question is worth looking for again. It is
    // derived from the timeline, so the loop asks only after input or a worker
    // result could have changed what is pending — an idle client does not walk
    // its whole projection sixty times a second.
    let mut questions_due = true;
    // The clipboard question the terminal has not answered yet, if any. While
    // one is in flight nothing else may read the input queue.
    let mut clipboard_read: Option<TerminalClipboardRead> = None;

    // First frame: paint the skeleton before any I/O result arrives. The
    // client opens on the page a session is written on, so what the first
    // frame reads is what that page names — the workspace, the runtime
    // catalogue — plus the session list its background work is derived from.
    let initial = app.perform(crate::action::Intent::NewSession);
    dispatch_all(worker, &initial);
    app.sync_transcript();

    loop {
        // ---- the terminal's own clipboard --------------------------------
        match pump_terminal_clipboard(app, worker, guard, &mut clipboard_read)? {
            ClipboardPump::Quit => return Ok(ExitReason::UserQuit),
            ClipboardPump::Applied => dirty = true,
            ClipboardPump::Waiting => {}
        }

        // ---- input -------------------------------------------------------
        // A probe owns the input queue for as long as it is alive: the
        // terminal's answer and a keystroke are the same kind of byte on the
        // same descriptor, so reading keys here would read the answer as keys.
        if clipboard_read.is_none() {
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
                        if handle_key(app, worker, Some(guard), key)? {
                            return Ok(ExitReason::UserQuit);
                        }
                        dirty = true;
                        // A key can be what leaves the Composer, which is what
                        // lets a waiting card open.
                        questions_due = true;
                    }
                    Event::Mouse(mouse) => {
                        if handle_mouse(app, worker, Some(guard), mouse)? {
                            dirty = true;
                        }
                    }
                    Event::Resize(columns, rows) => {
                        app.resize(columns, rows);
                        // A sign-in terminal is a PTY with its own idea of how
                        // wide it is, and the login flow reflows when it is
                        // told. The panel changed size with the window, so the
                        // terminal behind it has to change with the panel.
                        // The local grid is resized here rather than waiting
                        // for the round trip: the next frame is drawn at the
                        // new size, and a grid still believing it is the old
                        // one would paint the wrong number of rows into it.
                        let (rows, columns) = app.auth_terminal_size();
                        if let Some(terminal) = app.auth_terminal.as_mut() {
                            terminal.resize(rows, columns);
                            let terminal_id = terminal.terminal_id.clone();
                            dispatch_all(
                                worker,
                                &crate::reduce::Outcome::effects(vec![
                                    Effect::ResizeAgentAuthTerminal {
                                        terminal_id,
                                        rows,
                                        columns,
                                    },
                                ]),
                            );
                        }
                        dirty = true;
                    }
                    // The notifier is told whether the reader is at the
                    // terminal; it decides whether a session in front of them
                    // still has to be announced. A terminal that never reports
                    // focus sends neither event, and the client keeps assuming
                    // the reader is there.
                    Event::FocusGained => app.set_terminal_focused(true),
                    Event::FocusLost => app.set_terminal_focused(false),
                    Event::Paste(text) => {
                        if app.search_composing() {
                            if let Some(search) = app.search.as_mut() {
                                let mut query = search.query.clone();
                                query.push_str(&text);
                                search.set_query(query);
                            }
                            app.refresh_search_matches();
                        } else {
                            // One route for every paste: a path to a picture is
                            // a request to attach it, anything else is draft
                            // text.
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
        }

        // ---- worker results ----------------------------------------------
        let mut handled = 0usize;
        let results_started = Instant::now();
        while handled < 64 && results_started.elapsed() < INPUT_BATCH_BUDGET {
            match messages.try_recv() {
                Ok(message) => {
                    handled += 1;
                    // A message is how a request arrives and how one is
                    // answered elsewhere.
                    questions_due = true;
                    match message {
                        // The host the client runs on has nothing on its
                        // clipboard. The reader's clipboard may still be the one
                        // the *terminal* holds — a client reached over `ssh` has
                        // no other way to see it — so the terminal is asked
                        // before the gesture is called empty.
                        AppMessage::Pasted {
                            ticket,
                            wanted,
                            content: crate::worker::ClipboardContent::Empty,
                        } if crate::terminal_clipboard::available()
                            && app.accepts_composer_ticket(&ticket) =>
                        {
                            clipboard_read = Some(TerminalClipboardRead {
                                ticket,
                                wanted,
                                probe: crate::terminal_clipboard::Probe::start(terminal_wanted(
                                    wanted,
                                )),
                            });
                        }
                        message => apply_message(app, worker, message)?,
                    }
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

        // ---- activity detail ----------------------------------------------
        // An open activity whose payload is only a summary is a read the
        // reader has already asked for. The transcript records it wherever a
        // block is opened or rebuilt — a refresh can put a summary back under
        // a block that stayed open — and this is where the client, which is
        // the only thing that may start I/O, actually starts it.
        for effect in app.take_timeline_detail_effects() {
            worker.dispatch(effect);
        }

        // ---- the projected send -------------------------------------------
        // A send stops being projected once the runtime's own copy of it is in
        // the timeline, or once it has waited longer than a send could take.
        if app.settle_pending_send() {
            app.sync_transcript();
            dirty = true;
        }
        // ---- the transcript's liveness -------------------------------------
        // A read can move the open session's state — a list answering an
        // invalidated sidebar, a tree answer naming a worker — and the rows of
        // a turn that never produced a final message stay streaming until the
        // projection is rebuilt with that state. Settled before the clock,
        // because the clock reads the transcript to decide whether a turn is
        // still running.
        if app.sync_transcript_liveness() {
            dirty = true;
        }
        // The running band follows the session's state, so it appears when the
        // turn starts and leaves when it ends, whatever path the state took to
        // get there.
        if app.sync_turn_clock() {
            dirty = true;
        }

        // ---- the banner row ----------------------------------------------
        // The banner is a condition rather than an event: refreshing it here
        // means a message that is no longer true withdraws itself.
        if app.refresh_banner() {
            dirty = true;
        }

        // ---- the Agent's blocking question --------------------------------
        // A question opens its own card the first time it appears, the way the
        // desktop's card is simply there in the timeline: the transcript has no
        // room for controls, so a reader who never looked at the count in the
        // band would otherwise have no way to answer the Agent at all. Asked
        // only after input or a message, because the answer is derived from the
        // timeline and an idle client must not walk it every pass.
        if questions_due {
            questions_due = false;
            if app.sync_pending_question() {
                dirty = true;
            }
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
        if app.page_owns_session() && app.transcript.advance_disclosures(Instant::now()) {
            dirty = true;
        }
        if dirty && last_frame.elapsed() >= FRAME_INTERVAL {
            // A frame drawn at a size the client has not drawn at yet begins
            // with the backend clearing the screen, and the clear takes every
            // picture on it: a terminal holds its own images, and kitty's own
            // rules say a clear must clear them. So the frame the new size
            // arrives on is the last moment the pictures this client believes
            // it has placed are worth believing — forgetting them *here*, on
            // the way into that frame rather than after it, is what makes the
            // frame itself send every mark again instead of leaving the rows
            // the clear emptied blank until they scroll away.
            if let Ok(size) = terminal.size() {
                let drawn = (size.width, size.height);
                if drawn_size != Some(drawn) {
                    app.forget_terminal_pictures();
                    drawn_size = Some(drawn);
                }
            }
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

        // ---- notifications ------------------------------------------------
        // Out-of-band escapes are written after the frame, never inside it: the
        // frame owns the buffer, and a control sequence injected between two of
        // its writes would land in the middle of a row. A queued notification
        // is reason enough to write even when nothing repainted.
        let control = app.take_terminal_control();
        if !control.is_empty() {
            crate::terminal::write_control(&control)?;
        }
        // A desktop popup is a process rather than a byte, and it leaves through
        // the same door for the same reason: the frame owns the buffer, and the
        // reader's own desktop is not the frame's business.
        for notification in app.take_system_notifications() {
            crate::notify::deliver_system(&notification);
        }

        if app.should_quit {
            return Ok(ExitReason::UserQuit);
        }
        if app.handover_requested {
            return Ok(ExitReason::Handover);
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
/// The bytes a key sends to a terminal, or `None` when the key is not the
/// terminal's to take.
///
/// The encoding is the one a terminal actually receives, because the program
/// inside is reading its own stdin through a PTY: a name, a run of text, or an
/// escape sequence. Anything the client keeps for itself — a global chord, an
/// unbound function key — returns `None` and falls through to the binding
/// table.
fn terminal_keys_bytes(key: KeyEvent) -> Option<String> {
    // A control chord is the terminal's when the terminal has a byte for it:
    // `Ctrl+C` interrupts the login, `Ctrl+D` ends its input, and a program
    // that offers those is a program the reader can drive.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(character) = key.code {
            let lower = character.to_ascii_lowercase();
            if lower.is_ascii_lowercase() {
                // Ctrl+A is 0x01 through Ctrl+Z is 0x1a, in alphabet order.
                return Some((((lower as u8) - b'a' + 1) as char).to_string());
            }
            if character == ' ' {
                return Some("\0".to_string());
            }
        }
        return None;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        // Alt+<char> is `Esc` followed by the character, which is exactly how
        // a terminal reports it.
        if let KeyCode::Char(character) = key.code {
            return Some(format!("\x1b{character}"));
        }
        return None;
    }
    match key.code {
        KeyCode::Char(character) => Some(character.to_string()),
        KeyCode::Enter => Some("\r".to_string()),
        KeyCode::Tab => Some("\t".to_string()),
        KeyCode::BackTab => Some("\x1b[Z".to_string()),
        KeyCode::Backspace => Some("\x7f".to_string()),
        KeyCode::Esc => Some("\x1b".to_string()),
        KeyCode::Up => Some("\x1b[A".to_string()),
        KeyCode::Down => Some("\x1b[B".to_string()),
        KeyCode::Right => Some("\x1b[C".to_string()),
        KeyCode::Left => Some("\x1b[D".to_string()),
        KeyCode::Home => Some("\x1b[H".to_string()),
        KeyCode::End => Some("\x1b[F".to_string()),
        KeyCode::PageUp => Some("\x1b[5~".to_string()),
        KeyCode::PageDown => Some("\x1b[6~".to_string()),
        KeyCode::Insert => Some("\x1b[2~".to_string()),
        KeyCode::Delete => Some("\x1b[3~".to_string()),
        // A function key or a media key is not something a login prompt reads.
        KeyCode::F(_)
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => None,
    }
}

/// Reduce one key press. `true` means the program should exit.
///
/// The terminal guard is what `$EDITOR` needs — handing the screen over and
/// taking it back — and it is optional because a press can also arrive from a
/// click on a key legend, where there is no editor to open and, in a test, no
/// terminal to hand over. Only that one branch reads it.
fn handle_key(
    app: &mut App,
    worker: &Worker,
    guard: Option<&mut TerminalGuard>,
    key: KeyEvent,
) -> BackendResult<bool> {
    let key = normalized_key(key);
    // Naming somewhere else to be is answered from anywhere the key is not
    // text: the key bar and the cheatsheet go on naming these chords while a
    // field or a modal owns the keyboard, so a reader who presses one is asking
    // to leave — and the modes below either ignore the key (a function key in a
    // filter) or would swallow it (a modal). `chord_is_input` is the same
    // question the bar asks before it names a key, so the two cannot disagree
    // about whether `F1` is Settings or nothing at all.
    let chord = Chord::from_event(key);
    if !app.chord_is_input(chord)
        && let Some(intent) = app.keymap.resolve(&app.active_scopes(), chord)
        && intent.leaves_the_surface()
    {
        let outcome = app.perform(intent);
        dispatch_all(worker, &outcome);
        return Ok(app.should_quit);
    }
    // A sign-in terminal owns the keyboard while it is attached: the program
    // inside is an interactive login, and a key that ran a client binding
    // instead would be a key the login never saw. `Esc` is the one exception —
    // it gives the panel back — and the global chords above have already had
    // their chance, so a reader can still leave the surface.
    if let Some(terminal_id) = app.auth_terminal_key_target() {
        // `Esc` gives the panel back rather than being sent: the program inside
        // reads it as "cancel the prompt", and a reader asking to leave the
        // surface would be answering a question they meant to walk away from.
        // The terminal is detached, not closed — the authority still owns the
        // process, and re-opening the panel attaches to it again.
        if key.code == KeyCode::Esc && !key.modifiers.contains(KeyModifiers::CONTROL) {
            app.detach_auth_terminal();
            return Ok(false);
        }
        if let Some(bytes) = terminal_keys_bytes(key) {
            dispatch_all(
                worker,
                &crate::reduce::Outcome::effects(vec![Effect::WriteAgentAuthTerminal {
                    terminal_id,
                    data: bytes,
                }]),
            );
            return Ok(false);
        }
    }
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
    // The configuration centre's filter is its own field: it narrows a table
    // on the page rather than the session list, and `Enter` asks the area to
    // read again, which is what makes a registry search a search.
    if app.config_center.filtering {
        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.push_config_filter(character);
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.pop_config_filter();
                return Ok(false);
            }
            KeyCode::Delete => {
                app.clear_config_filter();
                return Ok(false);
            }
            KeyCode::Enter => {
                app.config_center.filtering = false;
                let outcome = app.refresh_config_area();
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
        if let Overlay::Form { form } = &overlay
            && form.editing.is_some()
        {
            match key.code {
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.type_form_char(character);
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    app.form_backspace();
                    return Ok(false);
                }
                _ => {}
            }
        }
        if let Overlay::Schedule(form) = &overlay
            && form.value(form.field).is_some()
        {
            // The schedule editor is a form of text fields with one closed-set
            // row, so it takes characters the way the configuration form does:
            // out of the way of the keys that move and commit.
            match key.code {
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    let mut form = form.clone();
                    // A character the field has no room for is not an error to
                    // report: the field simply does not take it, the way a
                    // caret in a one-line input stops at the edge.
                    if form.push(character) {
                        form.error = None;
                        app.overlay = Some(Overlay::Schedule(form));
                    }
                    return Ok(false);
                }
                KeyCode::Backspace => {
                    let mut form = form.clone();
                    if form.backspace() {
                        form.error = None;
                    }
                    app.overlay = Some(Overlay::Schedule(form));
                    return Ok(false);
                }
                _ => {}
            }
        }
        if let Overlay::Prompt {
            field,
            value,
            title,
            target,
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
                        target,
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
                        target,
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
        // The elicitation form is a set of controls, and two of its keys are
        // also text: a character the cursor's field types, and a digit that
        // chooses an option when the field offers them. Both are answered
        // before the binding table — the same rule the configuration form and
        // the schedule editor live by — so a question mark in an answer is a
        // question mark and `1` on a list of options is the first one.
        if let Some(Overlay::Elicitation { .. }) = app.overlay {
            let typing = app.elicitation_field_takes_text_keys();
            match key.code {
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    match app.elicitation_key(character) {
                        crate::reduce::ElicitationKey::Text => {
                            app.elicitation_type_char(character);
                            return Ok(false);
                        }
                        crate::reduce::ElicitationKey::Option(index) => {
                            let outcome = app.select_elicitation_option(index);
                            dispatch_all(worker, &outcome);
                            return Ok(false);
                        }
                        crate::reduce::ElicitationKey::Elsewhere => {}
                    }
                }
                KeyCode::Backspace if typing => {
                    app.elicitation_backspace();
                    return Ok(false);
                }
                KeyCode::Delete if typing => {
                    app.elicitation_clear_field();
                    return Ok(false);
                }
                _ => {}
            }
        }
        // The directory picker is a place a reader walks: `←` climbs out of the
        // directory and `→` opens the highlighted row, the two arrows every
        // file manager answers to. `Space` takes the directory the picker is
        // showing — the workspace list's own "use this directory" key — which
        // the overlay table cannot carry because the shared overlay `Space`
        // toggles a value in the elicitation form. All three are answered
        // before the binding table, and only while this picker is the overlay.
        if let Some(Overlay::WorkspacePicker { .. }) = app.overlay {
            let intent = match key.code {
                KeyCode::Left => Some(crate::action::Intent::WorkspaceBrowseUp),
                KeyCode::Right => Some(crate::action::Intent::ConfirmOverlay),
                KeyCode::Char(' ') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(crate::action::Intent::WorkspaceBrowseSelect)
                }
                _ => None,
            };
            if let Some(intent) = intent {
                let outcome = app.perform(intent);
                dispatch_all(worker, &outcome);
                return Ok(false);
            }
        }
    } else if app.composer_takes_keys()
        && let Some(exit) = handle_composer_key(app, worker, key)?
    {
        return Ok(exit);
    }

    let scopes = app.active_scopes();
    let Some(intent) = app.keymap.resolve(&scopes, chord) else {
        // An unbound key must not be swallowed silently when it looks like an
        // action the user expected to work.
        return Ok(false);
    };

    // `$EDITOR` hands the terminal over, so it runs outside the reducer.
    if intent == crate::action::Intent::EditComposerExternally {
        // No legend advertises this chord, so a pointer press never reaches
        // here; without a terminal there is nothing to hand the editor.
        let Some(guard) = guard else {
            return Ok(false);
        };
        let body = app.composer.text().to_string();
        let edited = with_terminal_restored(guard, || {
            crate::terminal::edit_in_editor("Vibex draft", &body)
        })?;
        // The screen was given back and taken again, and the alternate screen
        // does not keep what was on it: the terminal dropped every picture the
        // frame had placed, exactly as it does when a resize clears it. What
        // the pools believe is placed is worth nothing from here, so the frame
        // that follows the editor sends every mark and the reader's list comes
        // back with its icons rather than with empty slots.
        app.forget_terminal_pictures();
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

fn dispatch_completions(app: &mut App, worker: &Worker) {
    if let Some((trigger, query)) = app.refresh_completion() {
        worker.dispatch(crate::app::Effect::DiscoverCompletions {
            ticket: app.composer_ticket(),
            trigger,
            query,
        });
    }
}

/// The tag a completion row wears for the kind of entry it is.
///
/// The same words the desktop popup prints beside the same rows, so the two
/// clients describe one discovery answer with one vocabulary; `ClientBuiltin`
/// is the one variant whose name the code and the reader disagree about.
fn completion_source_label(source_kind: vibex_core::AgentCommandSourceKind) -> &'static str {
    use vibex_core::AgentCommandSourceKind;
    match source_kind {
        AgentCommandSourceKind::Provider => "Provider",
        AgentCommandSourceKind::Prompt => "Prompt",
        AgentCommandSourceKind::Skill => "Skill",
        AgentCommandSourceKind::Reference => "Reference",
        AgentCommandSourceKind::ClientBuiltin => "Built-in",
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
            // An open `/` or `@` drawer owns `←`/`→` for its panels, which is
            // how the desktop popup answers the same keys. At the first panel
            // the press is not the drawer's, so the caret keeps moving inside
            // the query the reader is editing — and the drawer stays open.
            if app.step_completion_tab(false) {
                return Ok(Some(false));
            }
            app.composer.move_left();
            return Ok(Some(false));
        }
        KeyCode::Right if !ctrl => {
            if app.step_completion_tab(true) {
                return Ok(Some(false));
            }
            app.composer.move_right();
            return Ok(Some(false));
        }
        KeyCode::Up if !ctrl && !alt => {
            // The completion menu owns the arrow keys while it is open.
            if app.completion.is_some() {
                let outcome = app.perform(Intent::CompletionPrevious);
                dispatch_all(worker, &outcome);
            } else if !app.step_composer_history(true) {
                // Otherwise `Up` is the draft's own key. It used to hand an
                // empty editor to the transcript; the walk takes that press
                // instead, and `Tab` is what moves the keyboard between the
                // transcript and the composer.
                app.composer.move_up();
                app.refresh_completion();
            }
            return Ok(Some(false));
        }
        KeyCode::Down if !ctrl && !alt => {
            if app.completion.is_some() {
                let outcome = app.perform(Intent::CompletionNext);
                dispatch_all(worker, &outcome);
            } else if !app.step_composer_history(false) {
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
            // A history walk is left the same way the desktop leaves one: the
            // recalled message goes and the draft it displaced comes back. Only
            // once there is no walk to leave does `Esc` mean "out of here".
            if app.cancel_composer_history() {
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
        // The drawer is the thing on screen with the keyboard, so `Enter` takes
        // the row it offers into the draft — the same answer the desktop gives
        // the same popup, and what keeps choosing a command from sending one.
        // It sends only when there is no row to take.
        KeyCode::Enter if app.completion_offers_row() => {
            let outcome = app.perform(Intent::CompletionAccept);
            dispatch_all(worker, &outcome);
            return Ok(Some(false));
        }
        _ => {}
    }
    Ok(None)
}

/// The mouse is an enhancement, never the only path: every gesture here has a
/// keyboard equivalent, and the transcript stays readable if the terminal has no
/// mouse reporting at all.
fn handle_mouse(
    app: &mut App,
    worker: &Worker,
    guard: Option<&mut TerminalGuard>,
    mouse: MouseEvent,
) -> BackendResult<bool> {
    // A sign-in terminal that asked for the mouse owns it: the program drew a
    // list and asked the terminal to report clicks, which is exactly the mode
    // the emulator records. A click the client kept for itself would be a click
    // the list never saw, so this comes before every other region.
    if let Some((terminal_id, bytes)) = app.auth_terminal_mouse_event(mouse) {
        dispatch_all(
            worker,
            &crate::reduce::Outcome::effects(vec![Effect::WriteAgentAuthTerminal {
                terminal_id,
                data: bytes,
            }]),
        );
        return Ok(true);
    }
    Ok(match mouse.kind {
        MouseEventKind::ScrollUp => {
            // The switcher owns the wheel while it is up: it is the list on
            // screen, and scrolling the transcript behind a modal is not a
            // gesture anyone means.
            if app.runtime_picker_is_open() {
                let outcome = app.step_runtime_picker(-3);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            // On the session list the wheel moves the list: the transcript
            // behind it is not what the reader is looking at, and a list that
            // only answers the arrow keys is a list that cannot be scanned.
            if app.page == crate::app::Page::Sessions && app.overlay.is_none() {
                app.scroll_session_list(-3);
                return Ok(true);
            }
            if app.page == crate::app::Page::Management && app.overlay.is_none() {
                let outcome = app.scroll_config_area(-3, mouse.column, mouse.row);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            app.scroll_lines(-3);
            // The wheel is the other way to reach the top of the loaded
            // window; the controller ignores the ask when the history is
            // exhausted or two pages are already on their way.
            if let Some(effect) = app.prefetch_older_history() {
                worker.dispatch(effect);
            }
            true
        }
        MouseEventKind::ScrollDown => {
            if app.runtime_picker_is_open() {
                let outcome = app.step_runtime_picker(3);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            if app.page == crate::app::Page::Sessions && app.overlay.is_none() {
                app.scroll_session_list(3);
                return Ok(true);
            }
            if app.page == crate::app::Page::Management && app.overlay.is_none() {
                let outcome = app.scroll_config_area(3, mouse.column, mouse.row);
                dispatch_all(worker, &outcome);
                return Ok(true);
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
                return Ok(true);
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
                // The control goes with the modal it belonged to; the pointer
                // is over the cell, not over the affordance that has left.
                app.hovered_modal_close = false;
                return Ok(true);
            }
            // A command palette row runs the command under the pointer. The
            // palette is a menu, and the hover has already moved the cursor onto
            // the row the reader is pointing at, so the press is the choice
            // rather than a step towards one.
            if let Some(row) = app
                .regions
                .palette
                .as_ref()
                .and_then(|region| region.entry_at(mouse.column, mouse.row))
            {
                let outcome = app.select_palette_row(row, true);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            // The configuration centre's own controls come first: a tab, an
            // area and a record's action button are each a control, and a
            // click on one is the press rather than a step towards it.
            if app.overlay.is_none()
                && let Some(outcome) = app.click_config_control(mouse.column, mouse.row)
            {
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            // The devices page has its own strip and its own rows, which the
            // shared list region cannot describe: the pairing rows are ways to
            // pair rather than records, so they carry no scope.
            if app.overlay.is_none()
                && app.page == crate::app::Page::Devices
                && let Some(outcome) = app.click_device_control(mouse.column, mouse.row)
            {
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            // The completion drawer's panel names are controls, like the strips
            // above: a press on one switches the panel the same way `←`/`→` do.
            // A modal covers the drawer, so its names answer only when nothing
            // is drawn over them.
            if app.overlay.is_none()
                && let Some((_, tab)) = app
                    .regions
                    .completion_tabs
                    .iter()
                    .find(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row))
                    .copied()
                && app.select_completion_tab(tab)
            {
                return Ok(true);
            }
            // The switcher's rows select and activate exactly as the other
            // lists do, headings included: a heading is the control that folds
            // its group, and a double click is how the mouse presses it. A line
            // of air between two groups answers with no row, so a click on one
            // is a click on nothing rather than on the row under it.
            if let Some(region) = app.regions.runtime_picker.as_ref()
                && let Some(row) = region.row_at(mouse.column, mouse.row)
            {
                let repeat = app.double_click_at(mouse.column, mouse.row);
                let outcome = app.select_runtime_picker_row(row, repeat);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            // A `key label` legend is the key itself, so the press is put
            // through the keyboard's own dispatcher rather than a second
            // interpretation of what the legend meant. That is what keeps the
            // queue's `Alt+E` and a form's `Esc` behaving exactly as they do
            // from the keys — including the modes that answer a key before the
            // binding table does.
            if let Some(chord) = app
                .regions
                .key_hints
                .iter()
                .find(|hint| rect_contains(hint.rect, mouse.column, mouse.row))
                .map(|hint| hint.chord)
            {
                return handle_key(app, worker, guard, chord.event());
            }
            if app.overlay.is_some() {
                return Ok(false);
            }
            // A row's own control acts on the row it was drawn for, not on the
            // row the cursor happens to be on: the pointer chose the target,
            // and a delete button that removed a different session than the one
            // under it is the one mistake this row cannot make.
            if let Some(action) = app
                .regions
                .row_actions
                .iter()
                .find(|action| rect_contains(action.rect, mouse.column, mouse.row))
                .copied()
            {
                app.set_selection(crate::keymap::Scope::Sessions, action.row);
                let outcome = app.perform(action.intent);
                dispatch_all(worker, &outcome);
                return Ok(true);
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
                return Ok(true);
            }
            // A list row selects; a second click on the same row activates it.
            if let Some(region) = app.regions.list.clone()
                && let Some(index) = crate::app::list_row_at(&region, mouse.column, mouse.row)
            {
                let repeat = app.double_click_at(mouse.column, mouse.row);
                if region.scope == crate::keymap::Scope::Management {
                    let outcome = app.click_config_row(index, repeat);
                    dispatch_all(worker, &outcome);
                    return Ok(true);
                }
                app.set_selection(region.scope, index);
                // A heading is a control, not a row to open: it folds on the
                // first click, the way a tree does everywhere else. So does a
                // session that owns children — its branch is opened by the same
                // click that selects it, which is why no disclosure is drawn at
                // its right edge to aim at. A session with no branch under it
                // keeps the two-step contract, because opening one leaves the
                // page.
                let heading = region.scope == crate::keymap::Scope::Sessions
                    && app.sidebar_row_is_heading(index);
                let branch = region.scope == crate::keymap::Scope::Sessions
                    && app.sidebar_row_has_children(index);
                // The first click of a double press folds the branch; the second
                // is the open. Both are kept, so a session that owns children is
                // still reachable on the page the way every other session is.
                if branch && !repeat {
                    let outcome = crate::reduce::Outcome::effects(
                        app.toggle_child_branch_at(index).into_iter().collect(),
                    );
                    dispatch_all(worker, &outcome);
                    return Ok(true);
                }
                if repeat || heading {
                    let intent = match region.scope {
                        crate::keymap::Scope::Sessions => {
                            crate::action::Intent::OpenSelectedSession
                        }
                        _ => crate::action::Intent::ActivateSetting,
                    };
                    let outcome = app.perform(intent);
                    dispatch_all(worker, &outcome);
                }
                return Ok(true);
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
                return Ok(true);
            }
            // A dock row selects; a second click on the same row opens it, the
            // same contract the other lists use.
            if let Some(region) = app.regions.dock
                && rect_contains(region, mouse.column, mouse.row)
            {
                let line = usize::from(mouse.row - region.y);
                if let Some(row) = app.regions.dock_rows.get(line).copied() {
                    app.dock_selection = Some(row);
                    if app.double_click_at(mouse.column, mouse.row) {
                        app.activate_dock_row();
                    }
                }
                return Ok(true);
            }
            // The queue band selects a held message.
            if let Some(region) = app.regions.queue
                && rect_contains(region, mouse.column, mouse.row)
            {
                let line = usize::from(mouse.row - region.y);
                if let Some(row) = app.regions.queue_rows.get(line).copied() {
                    app.queue_selection = Some(row);
                    if app.double_click_at(mouse.column, mouse.row) {
                        app.edit_queued_message();
                    }
                }
                return Ok(true);
            }
            // The composer takes the keyboard from a click anywhere in its box,
            // and from a text row it also takes the caret: a click leaves the
            // selection empty, a drag fills it, and typing over it replaces it.
            if app.click_composer(mouse.column, mouse.row) {
                return Ok(true);
            }
            if app.page != Page::Agent {
                return Ok(false);
            }
            let Some((line, column)) = mouse_cell(app, mouse.column, mouse.row) else {
                return Ok(false);
            };
            // The fold's own rows put a turn's process away: the title above an
            // open section, the rule below it, and the single title row a shut
            // one draws. They are controls before they are rows: pressing one
            // toggles the work, and a drag that happens to start on it still
            // selects text, so the gesture only claims its own lines. The
            // pointer resolves the row it landed on rather than asking which
            // affordance `e` would have chosen.
            if let Some(index) = app.transcript.block_at_line(line)
                && app.transcript.fold_control_lines(index).contains(&line)
            {
                app.clear_text_selection();
                app.focus = Focus::Main;
                app.set_selection(crate::keymap::Scope::Agent, index);
                app.pin_transcript_view();
                let opened = app.transcript.toggle_fold_at(index);
                let effects = if opened {
                    // The act of opening can reveal an activity whose payload
                    // the summary page withheld, so the read is asked for here
                    // the way it is for a group that was just opened.
                    app.note_all_block_details();
                    app.take_timeline_detail_effects()
                } else {
                    Vec::new()
                };
                dispatch_all(worker, &Outcome::effects(effects));
                return Ok(true);
            }
            // The block's own row opens the block: the one its disclosure is
            // drawn on. A fold draws the turn's title over the work it opens,
            // so a block that opens an open section is not on its first line —
            // and the row the reader points at is the one that has to answer.
            if let Some(index) = app.transcript.block_at_line(line)
                && app.transcript.block_header_line(index) == Some(line)
                && app.transcript.block(index).is_some_and(|block| {
                    block.collapsible
                        && !block.is_hidden()
                        && crate::transcript::is_dense_row(block.kind)
                })
            {
                app.clear_text_selection();
                app.focus = Focus::Main;
                app.set_selection(crate::keymap::Scope::Agent, index);
                app.pin_transcript_view();
                let outcome = app.perform(crate::action::Intent::ToggleBlockExpanded);
                dispatch_all(worker, &outcome);
                return Ok(true);
            }
            let now = Instant::now();
            let double_click = app.last_click.is_some_and(|(at, last_line, last_column)| {
                now.duration_since(at) < DOUBLE_CLICK && last_line == line && last_column == column
            });
            if double_click {
                app.last_click = None;
                return Ok(app.select_word_at(line, column));
            }
            app.last_click = Some((now, line, column));
            app.clear_text_selection();
            app.begin_text_selection(line, column);
            true
        }
        MouseEventKind::Moved => {
            // The palette's list follows the pointer: hovering a row moves the
            // cursor onto it, which is the highlight `↑↓` paints, so the row
            // that is lit and the row `Enter` would run cannot disagree.
            if let Some(row) = app
                .regions
                .palette
                .as_ref()
                .and_then(|region| region.entry_at(mouse.column, mouse.row))
            {
                let outcome = app.select_palette_row(row, false);
                dispatch_all(worker, &outcome);
                return Ok(outcome.dirty);
            }
            // The modal's close affordance is a control before it is a click
            // target: the pointer resting on it is how the reader learns it can
            // be pressed.
            let close = app
                .regions
                .modal_close
                .is_some_and(|rect| rect_contains(rect, mouse.column, mouse.row));
            let hover = hover_target(app, mouse.column, mouse.row);
            // A hint is a control: the pointer resting on it is how the reader
            // learns it can be pressed.
            let hint = app
                .regions
                .hints
                .iter()
                .find(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row))
                .map(|(_, intent)| *intent);
            // A legend lights for the same reason a button does: the pointer
            // resting on it is how the reader learns it can be pressed.
            let key_hint = app
                .regions
                .key_hints
                .iter()
                .find(|hint| rect_contains(hint.rect, mouse.column, mouse.row))
                .map(|hint| hint.chord);
            // A control inside a list row answers the same way, and carries the
            // row it belongs to so the right one lights up.
            let row_action = app
                .regions
                .row_actions
                .iter()
                .find(|action| rect_contains(action.rect, mouse.column, mouse.row))
                .map(|action| (action.row, action.intent));
            if app.hover != hover
                || app.hovered_hint != hint
                || app.hovered_key_hint != key_hint
                || app.hovered_row_action != row_action
                || app.hovered_modal_close != close
            {
                app.hover = hover;
                app.hovered_hint = hint;
                app.hovered_key_hint = key_hint;
                app.hovered_row_action = row_action;
                app.hovered_modal_close = close;
                return Ok(true);
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.draft_selecting {
                return Ok(app.drag_draft_selection(mouse.column, mouse.row));
            }
            let Some((line, column)) = mouse_cell_clamped(app, mouse.column, mouse.row) else {
                return Ok(false);
            };
            app.extend_text_selection(line, column)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // A drag inside the draft copies what it selected; the highlight
            // stays so the reader can see what went to the clipboard.
            if app.draft_selecting {
                app.draft_selecting = false;
                let Some(text) = app.composer.selected_text() else {
                    return Ok(true);
                };
                let message = app.strings.copied().to_string();
                app.toast(Toast::success(message));
                worker.dispatch(crate::app::Effect::Clipboard { text });
                return Ok(true);
            }
            if !app.finish_text_selection() {
                return Ok(true);
            }
            let Some(text) = app.selected_text() else {
                return Ok(true);
            };
            let message = app.strings.copied().to_string();
            app.toast(Toast::success(message));
            worker.dispatch(crate::app::Effect::Clipboard { text });
            true
        }
        _ => false,
    })
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
    let line = usize::from(row.checked_sub(region.y)?);
    let message = *app.regions.queue_rows.get(line)?;
    rect_contains(region, column, row).then_some((crate::keymap::Scope::Agent, message))
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

/// A clipboard read the terminal itself has to answer.
///
/// The host this client runs on had nothing on its clipboard, so the reader's
/// clipboard can only be the one the terminal is holding — which is the whole
/// situation of a client reached over `ssh`. The terminal's answer arrives on
/// the input queue the event loop owns, so the loop reads it, and nothing else
/// may read that queue until it has.
struct TerminalClipboardRead {
    ticket: crate::app::ComposerTicket,
    wanted: ClipboardWanted,
    probe: crate::terminal_clipboard::Probe,
}

/// Advance a clipboard read the terminal is answering, if one is in flight.
///
/// The probe swallows what the queue holds while it waits — it has to, or the
/// terminal's own answer would be read as keystrokes — and `Ctrl+C` is the one
/// key that is handed back, so a reader waiting on a prompt they cannot see can
/// still leave.
fn pump_terminal_clipboard(
    app: &mut App,
    worker: &Worker,
    guard: &mut TerminalGuard,
    read: &mut Option<TerminalClipboardRead>,
) -> BackendResult<ClipboardPump> {
    use crate::terminal_clipboard::Progress;

    let Some(active) = read.as_mut() else {
        return Ok(ClipboardPump::Waiting);
    };
    match active.probe.pump() {
        Progress::Waiting => Ok(ClipboardPump::Waiting),
        Progress::Interrupted => {
            *read = None;
            let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            Ok(if handle_key(app, worker, Some(guard), ctrl_c)? {
                ClipboardPump::Quit
            } else {
                ClipboardPump::Waiting
            })
        }
        Progress::Answered(content) => {
            let active = read.take().expect("the read was in flight");
            apply_clipboard(
                app,
                &active.ticket,
                active.wanted,
                clipboard_content(content),
            );
            Ok(ClipboardPump::Applied)
        }
    }
}

/// What one pump of the terminal's clipboard produced.
enum ClipboardPump {
    /// The terminal has not finished.
    Waiting,
    /// The answer was applied, and the frame has something new to draw.
    Applied,
    /// The reader asked to leave.
    Quit,
}

/// What the terminal's own clipboard held, in the worker's vocabulary.
fn clipboard_content(content: crate::terminal_clipboard::TerminalClipboard) -> ClipboardContent {
    use crate::terminal_clipboard::TerminalClipboard;
    match content {
        TerminalClipboard::Image { mime_type, bytes } => {
            ClipboardContent::Image { mime_type, bytes }
        }
        TerminalClipboard::Text(text) => ClipboardContent::Text(text),
        TerminalClipboard::Refused => ClipboardContent::Refused,
        // A terminal that does not speak the protocol is a clipboard this
        // client cannot reach; it is not a refusal to report.
        TerminalClipboard::Empty | TerminalClipboard::Unsupported => ClipboardContent::Empty,
    }
}

/// The half of the terminal's clipboard a gesture asks it for.
fn terminal_wanted(wanted: ClipboardWanted) -> crate::terminal_clipboard::Wanted {
    match wanted {
        ClipboardWanted::Everything => crate::terminal_clipboard::Wanted::Everything,
        ClipboardWanted::Image => crate::terminal_clipboard::Wanted::Image,
    }
}

/// Apply what a clipboard read produced, whichever clipboard answered it.
///
/// Who asked decides what "nothing" means: a paste says so, the composer's
/// attach action opens the path prompt, because naming a file is still a way
/// to attach a picture.
fn apply_clipboard(
    app: &mut App,
    ticket: &crate::app::ComposerTicket,
    wanted: ClipboardWanted,
    content: ClipboardContent,
) {
    if !app.accepts_composer_ticket(ticket) {
        return;
    }
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
        // The clipboard is there and this client was not allowed to read it:
        // saying "nothing to paste" would send the reader looking in the wrong
        // place.
        ClipboardContent::Refused => {
            app.toast(Toast::info(app.strings.clipboard_refused().to_string()));
        }
        ClipboardContent::Empty => match wanted {
            ClipboardWanted::Image => {
                app.overlay = Some(crate::app::Overlay::Prompt {
                    title: app.strings.image_path_title().to_string(),
                    field: crate::app::PromptField::ImagePath,
                    value: String::new(),
                    target: None,
                });
                app.toast(Toast::info(app.strings.image_clipboard_empty().to_string()));
            }
            ClipboardWanted::Everything => {
                app.toast(Toast::info(app.strings.clipboard_empty().to_string()));
            }
        },
    }
}

fn apply_message(app: &mut App, worker: &Worker, message: AppMessage) -> BackendResult<()> {
    match message {
        AppMessage::Pasted {
            ticket,
            wanted,
            content,
        } => apply_clipboard(app, &ticket, wanted, content),
        AppMessage::Sessions(result) => {
            // Read before the list is stored: a row the reader named by writing
            // is recognized as already held, which is what tells a title the
            // authority supplies from a placeholder that has not been named yet.
            let result = app.reconcile_session_list(result);
            if let Err(error) = app.agent.apply_sessions(result) {
                app.toast(Toast::danger(error.message));
            }
            // A session the runtime no longer lists is a session the list can
            // no longer draw, so its last words are not worth keeping.
            app.retain_session_echoes();
            app.retain_pending_session_titles();
            app.retain_child_tree();
            // Fold the loaded ids into the reader's arrangement: their pins and
            // manual order survive a refresh, and new sessions are appended
            // rather than dropped from the order.
            app.reconcile_sidebar_arrangement();
            app.live = LiveState::Ready;
            if let Some(effect) = app.pending_session_list_refresh() {
                worker.dispatch(effect);
            }
            // A session that has just appeared may own children, and one that
            // has just changed may have changed a worker's state. The reads are
            // coalesced and bounded by the tree itself, so a burst of session
            // events does not become a burst of ownership reads.
            for effect in app.child_tree_effects() {
                worker.dispatch(effect);
            }
        }
        AppMessage::SessionTree {
            parent_session_id,
            append,
            result,
        } => match result {
            Ok(page) => {
                // Only the page the open session asked for describes the path
                // back to the session that owns it; a branch's ancestors are
                // its own and never this reader's way home.
                if let Some(open) = parent_session_id.clone()
                    && app.selected_session_id() == Some(&open)
                {
                    app.children.set_open_path(&open, page.ancestors.clone());
                }
                app.children.apply(
                    parent_session_id.as_ref(),
                    append,
                    vibex_core::unix_timestamp_ms(),
                    page,
                );
                // The answer carries metadata for every node and ancestor it
                // named, the open worker included. That row is this client's
                // only authoritative copy of a worker's state — the root list
                // omits it, and a remote authority announces it as a sidebar
                // invalidation rather than a session update — so the page's own
                // copy is settled from it here.
                app.sync_open_session_from_tree();
            }
            Err(error) => {
                if error.kind == vibex_backend::BackendErrorKind::Unsupported {
                    // This runtime has no ownership tree to read. Stop asking
                    // and keep drawing the list this client has always drawn.
                    app.children.mark_unsupported();
                } else {
                    app.children.fail(parent_session_id.as_ref());
                }
            }
        },
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
        AppMessage::TeamLoaded { ticket, result } => {
            if app.team.apply(&ticket, result)
                && app.team.has_pending_refresh()
                && app.dock_open
                && let Some(effect) = app.load_team(false)
            {
                worker.dispatch(effect);
            }
        }
        AppMessage::TeamMutated { session_id, result } => {
            app.team_busy = false;
            if app.team.session_id() == Some(&session_id) {
                if let Err(error) = result {
                    app.toast(Toast::danger(error.message));
                }
                if let Some(effect) = app.load_team(false) {
                    worker.dispatch(effect);
                }
            }
        }
        AppMessage::SessionOpened { ticket, result } => {
            let failure = result.as_ref().err().map(|error| error.message.clone());
            // The shared controller owns the projection: applying the snapshot
            // is what populates `selected_session_id` and the timeline model,
            // so live events stop being dropped as stale.
            if app.agent.apply_session_snapshot(&ticket, result) {
                // Details read for the projection this replaces can never be
                // applied; the rebuild below asks again for whatever the
                // reader still has open.
                app.prune_timeline_details();
                app.sync_transcript();
                match failure {
                    Some(message) => app.toast(Toast::danger(message)),
                    None => {
                        app.sync_transcript();
                        app.live = LiveState::Ready;
                        if app.dock_open
                            && let Some(effect) = app.load_team(false)
                        {
                            worker.dispatch(effect);
                        }
                    }
                }
            }
        }
        AppMessage::OlderTimeline { ticket, result } => {
            match app.agent.apply_timeline_before(&ticket, result) {
                Ok(true) => {
                    app.sync_transcript();
                    // The reader was near the top when this page was asked
                    // for, so they are reading into it now: keep the next one
                    // on its way until the history runs out or they scroll
                    // away.
                    if let Some(effect) = app.prefetch_older_history() {
                        worker.dispatch(effect);
                    }
                }
                Ok(false) => {}
                Err(error) => app.toast(Toast::warning(error.message)),
            }
        }
        AppMessage::TimelineItem { ticket, result } => {
            app.finish_timeline_detail(&ticket);
            match app.agent.apply_timeline_item(&ticket, result) {
                // The block now renders the output the summary withheld.
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
            // The read has answered, so the composing page stops saying it is
            // loading: a catalogue replaces it with the Agent it names, and an
            // empty or failed one leaves the runtime's own answer standing.
            app.runtime_options_reading = false;
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
        AppMessage::Mutation {
            key,
            record,
            result,
        } => {
            app.pending.remove(&key);
            // The write is over either way, so the row stops saying it is
            // working. What it was about comes back out with it: the answer
            // names the record and the verb, which is the half a reader who
            // looked away cannot reconstruct from the row alone.
            let pending = record
                .as_deref()
                .and_then(|record| app.config_center.finish_pending(record));
            match result {
                Ok(outcome) => {
                    if let Some(pending) = &pending {
                        let message = pending.message(app.strings, &outcome);
                        let tone = outcome.tone();
                        app.toast(match tone {
                            crate::config_center::ConfigTone::Bad => Toast::danger(message.clone()),
                            crate::config_center::ConfigTone::Warn => {
                                Toast::warning(message.clone())
                            }
                            _ => Toast::success(message.clone()),
                        });
                        // The toast is gone in seconds; the line under the
                        // table stays, so the reader who was reading something
                        // else when the answer landed can still see what it
                        // was about.
                        app.config_center.set_notice(message, tone);
                    }
                    // A form exists to make one write: it closes on the answer,
                    // leaving the record it produced on the table behind it. A
                    // refusal keeps it open, because the fix is in its fields.
                    if record.as_deref() == Some(crate::config_center::FORM_RECORD)
                        && matches!(app.overlay, Some(Overlay::Form { .. }))
                    {
                        app.overlay = None;
                    }
                    if key == "resolve_permission" {
                        let message = app.strings.approval_resolved().to_string();
                        app.toast(Toast::success(message));
                    }
                    if key == "update_entry" {
                        let message = app.strings.management_saved().to_string();
                        app.toast(Toast::success(message));
                    }
                    // The answer to a sign-in is what the panel remembers: a
                    // method that opened a browser or a terminal is still
                    // running, and the operation it named is what "cancel"
                    // would end. A sign-in that finished clears it, because
                    // there is nothing left to call off.
                    // A cancel is its own verb, and its answer is the panel's:
                    // the operation is over, so the line that offered to end it
                    // is gone and the sentence says so.
                    if key == "agent_auth_cancel"
                        && let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut()
                    {
                        view.pending_sign_in = None;
                        view.notice = Some((
                            app.strings.config_auth_cancelled().to_string(),
                            crate::config_center::ConfigTone::Muted,
                        ));
                    }
                    if key == "agent_auth_sign_in"
                        && let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut()
                    {
                        view.pending_sign_in = match &outcome {
                            crate::app::MutationOutcome::AuthWaiting {
                                operation: Some(operation),
                                ..
                            } => Some((**operation).clone()),
                            _ => None,
                        };
                    }
                    if key == "switch_runtime" {
                        // The switch is durable and asynchronous: the runtime
                        // reports `Switching…` until the new Agent is up, so the
                        // acknowledgement says that rather than claiming the
                        // session has already moved.
                        let message = app.strings.runtime_switching().to_string();
                        app.toast(Toast::info(message));
                    }
                    // A sign-in that opened a terminal is a sign-in this client
                    // can follow: attach to the PTY the authority created and
                    // draw it. The attach is issued before the re-read below so
                    // the frame pump is already listening when the first bytes
                    // land — a login prompt printed before the subscription
                    // would otherwise have to arrive twice.
                    if let crate::app::MutationOutcome::AuthWaiting {
                        terminal: Some(terminal),
                        ..
                    } = &outcome
                        && let Some(terminal_id) = terminal.terminal_id.clone()
                        && let Some(agent_id) = record
                            .as_deref()
                            .and_then(|record| vibex_core::AgentId::parse(record).ok())
                    {
                        dispatch_all(
                            worker,
                            &crate::reduce::Outcome::effects(vec![
                                Effect::AttachAgentAuthTerminal {
                                    agent_id,
                                    terminal_id,
                                    title: terminal.title.clone(),
                                },
                            ]),
                        );
                    }
                    // A write is only half the story: every record on screen
                    // is the one from before it. Re-read what the write
                    // changed, so what the reader just did is what they see
                    // rather than what a later `Ctrl+R` would have shown.
                    let outcome = app.refresh_after_mutation(&key);
                    dispatch_all(worker, &outcome);
                }
                Err(error) => {
                    // A failed mutation rolls the optimistic UI back and says
                    // why, rather than leaving the user guessing.
                    let message = match &pending {
                        Some(pending) => pending.failure(app.strings, &error.message),
                        None => format!("{}: {}", key, error.message),
                    };
                    app.toast(Toast::danger(message.clone()));
                    if pending.is_some() {
                        app.config_center
                            .set_notice(message, crate::config_center::ConfigTone::Bad);
                    }
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
                    dispatch_all(worker, &outcome);
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
                // logs, configuration, or the scrollback export. It lives on the
                // pairing page, which drops it the moment the reader leaves the
                // tab, and the row that asked for it stops spinning.
                app.device_center.pairing.busy = None;
                app.device_center.pairing.error = None;
                app.device_center.pairing.offer =
                    Some(crate::device_center::PairingOffer::from_response(&response));
            }
            Err(error) => {
                let method = app
                    .device_center
                    .pairing
                    .busy
                    .unwrap_or(crate::device_center::PairMethod::QrCode);
                app.device_center
                    .pairing
                    .fail(crate::device_center::PairingError {
                        method,
                        code: error.code.clone(),
                        message: error.message.clone(),
                    });
            }
        },
        AppMessage::PairingOfferStatus(result) => match result {
            Ok(summary) => {
                // A cancel answers with the summary the runtime now holds, and a
                // status poll answers with the one on screen. Only the second
                // can be news, and only when it says the device arrived.
                if app.device_center.pairing.apply_offer_status(&summary)
                    && app.device_center.pairing.offer_was_claimed()
                {
                    app.toast(Toast::success(app.strings.devices_offer_claimed()));
                }
            }
            Err(error) => {
                // The panel is not on screen any more — a cancel was the press
                // that asked — so the toast is the only place left to say the
                // runtime did not take it down.
                app.device_center.pairing.busy = None;
                app.toast(Toast::danger(error.message.clone()));
            }
        },
        AppMessage::PairingPublications(result) => {
            // The code travels rather than the message: the panel turns it into
            // what the reader can do about it, and a server sentence is not
            // that.
            let settled = match &result {
                Ok(rows) => Ok(rows.clone()),
                Err(error) => Err(error.code.as_str()),
            };
            app.device_center.publications.settle(settled);
        }
        AppMessage::LanPairing(result) => match result {
            Ok(window) => {
                app.device_center
                    .pairing
                    .settle(window.map(|window| *window));
            }
            Err(error) => {
                // The answer names what failed, so the panel can say which row
                // it was and how to recover; a panel the reader has already
                // closed has nowhere to say it, so the toast does.
                let method = app
                    .device_center
                    .pairing
                    .busy
                    .unwrap_or(crate::device_center::PairMethod::LocalNetwork);
                if app.device_center.lan_panel_open() {
                    app.device_center
                        .pairing
                        .fail(crate::device_center::PairingError {
                            method,
                            code: error.code.clone(),
                            message: error.message.clone(),
                        });
                } else {
                    app.device_center.pairing.busy = None;
                    app.toast(Toast::danger(error.message));
                }
            }
        },
        AppMessage::Audit(result) => match result {
            Ok(records) => {
                let body = crate::device_center::audit_report(&records, app.strings);
                app.management_data.audit = records;
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.devices_audit().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Gateway(result) => match result {
            Ok(gateway) => {
                app.management_data.gateway = Some(gateway);
            }
            Err(error) => {
                // A gateway that cannot be read is not a reason to hide the
                // Providers page: the direct Providers are still readable.
                app.management_data.gateway = None;
                if error.code != "remote_model_gateway_unavailable" {
                    app.toast(Toast::danger(error.message));
                }
            }
        },
        AppMessage::GatewayRequests(result) => match result {
            Ok(requests) => {
                let gateway = app.management_data.gateway.as_ref();
                let mut body = String::new();
                if requests.is_empty() {
                    body.push_str(app.strings.config_requests_empty());
                    body.push('\n');
                }
                for request in requests.iter().take(100) {
                    body.push_str(&format!(
                        "{}  {}  {} -> {}\n",
                        request.request_id.as_str(),
                        request.outcome.id(),
                        gateway_requested_model_label(gateway, &request.model_requested),
                        request.model_served.as_deref().unwrap_or("-"),
                    ));
                    body.push_str(&format!(
                        "  {} · {} → {} · {} tok ({} in / {} out / {} cached) · {} ms\n",
                        gateway_provider_label(
                            gateway,
                            request.model_provider_profile_id.as_str(),
                            &request.provider_name,
                        ),
                        request.wire_in,
                        request.wire_out,
                        request.total_tokens,
                        request.input_tokens,
                        request.output_tokens,
                        request.cached_read_tokens,
                        request.latency_ms.unwrap_or_default(),
                    ));
                    if let Some(key) = request.credential_name.as_deref() {
                        body.push_str(&format!("  key: {key}\n"));
                    }
                    if let Some(route) = request.route.as_ref() {
                        if route.skipped_cooling > 0 || route.skipped_over_limit > 0 {
                            body.push_str(&format!(
                                "  skipped: {} cooling, {} over limit\n",
                                route.skipped_cooling, route.skipped_over_limit
                            ));
                        }
                        for attempt in &route.failed_attempts {
                            body.push_str(&format!(
                                "  tried {}: {}\n",
                                attempt.credential_name.as_deref().unwrap_or("Key"),
                                attempt.failure.id()
                            ));
                        }
                    }
                    body.push('\n');
                }
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.config_action_requests().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Profiles(result) => match result {
            Ok(profiles) => {
                app.management_data.providers = profiles;
                app.management_data.mark_read(
                    crate::config_center::ConfigModule::Agent,
                    crate::config_center::ConfigSection::Providers,
                );
                app.live = LiveState::Ready;
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                app.management_data.mark_read_failed(
                    crate::config_center::ConfigModule::Agent,
                    crate::config_center::ConfigSection::Providers,
                    error.code,
                    error.message,
                );
            }
        },
        AppMessage::CatalogOfferedModels {
            provider_id,
            only,
            models,
        } => {
            match models {
                Ok(models) => app.fill_preset_models(&provider_id, only.as_deref(), models),
                // A catalogue read that fails leaves the form's model list as
                // it is: the save-time fetch still asks the endpoint, and the
                // reader can type an id.
                Err(error) => app.toast(Toast::danger(error.message)),
            }
        }
        AppMessage::ProviderTest(result) => match result {
            Ok(tested) => {
                let body = format!("{:?}\n\n{}\n{}", tested.status, tested.code, tested.message);
                app.overlay = Some(Overlay::TextView {
                    title: app.strings.management_probe().to_string(),
                    body,
                    scroll: 0,
                });
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::AgentAuthLogoutPreview(result) => match result {
            Ok(preview) => {
                app.management_data.agent_auth_logout = Some(*preview);
            }
            Err(error) => {
                app.toast(Toast::danger(error.message));
                if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
                    view.confirming_sign_out = false;
                }
            }
        },
        AppMessage::AgentAuthCatalog(result) => {
            let agent_id = match &result {
                Ok(catalog) => catalog.agent_id.as_str().to_string(),
                Err(_) => match &app.overlay {
                    Some(Overlay::AgentAuth { view }) => view.agent_id.clone(),
                    _ => String::new(),
                },
            };
            match result {
                Ok(mut catalog) => {
                    // The catalogue says what the ways in are; the contexts the
                    // authority already answered say whether one of them was
                    // used. Folding them in here is what keeps the panel from
                    // saying "Not checked" over an account the reader is signed
                    // into, and what lets the verbs that need an account appear
                    // without a second round trip.
                    crate::config_center::merge_agent_auth_context(
                        &mut catalog,
                        &app.management_data.agent_auth,
                    );
                    app.management_data.agent_auth_catalog = Some(*catalog);
                    // The lines are the catalogue's, so where the verb that
                    // opened the panel points is known only now.
                    let landed = app.management_data.agent_auth_catalog.as_ref();
                    let strings = app.strings;
                    if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
                        let rows = view.rows(landed, strings);
                        view.selected = view.want_line.index(&rows);
                        view.loading = false;
                        view.notice = None;
                    }
                }
                Err(error) => {
                    app.toast(Toast::danger(error.message.clone()));
                    if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
                        view.loading = false;
                        view.notice = Some((error.message, crate::config_center::ConfigTone::Bad));
                    }
                }
            }
            // The row wore a working marker while the panel opened; the answer
            // is what takes it off.
            app.config_center.finish_pending(&agent_id);
        }
        AppMessage::NativeExport(result) => match result {
            Ok(preview) => {
                if let Some(Overlay::NativeExport { view }) = app.overlay.as_mut() {
                    view.preview = Some(preview);
                    view.notice = None;
                }
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                if let Some(Overlay::NativeExport { view }) = app.overlay.as_mut() {
                    view.notice = Some((error.message, crate::config_center::ConfigTone::Bad));
                }
            }
        },
        AppMessage::NativeExportHistory(result) => match result {
            Ok(history) => {
                if let Some(Overlay::NativeExport { view }) = app.overlay.as_mut() {
                    view.history = history;
                }
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::SkillMarketPage(result) => match result {
            Ok(page) => {
                // A page is appended rather than replacing what is on screen,
                // and by identity: a registry that shifted under the reader
                // between two reads would otherwise show the same Skill twice.
                let known = app
                    .management_data
                    .skill_market
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect::<std::collections::HashSet<_>>();
                let added = page
                    .entries
                    .iter()
                    .filter(|entry| !known.contains(&entry.id))
                    .count();
                app.management_data.skill_market.extend(
                    page.entries
                        .into_iter()
                        .filter(|entry| !known.contains(&entry.id)),
                );
                let shown = app.management_data.skill_market.len();
                app.management_data.market_note = Some(crate::app::MarketNote {
                    matched: Some(shown),
                    exhausted: !page.has_more,
                    cursor: page.next_cursor.clone(),
                    error: None,
                });
                app.toast(Toast::success(
                    app.strings
                        .config_registry_more()
                        .replace("{count}", &added.to_string()),
                ));
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                if let Some(note) = app.management_data.market_note.as_mut() {
                    note.error = Some((error.code, error.message));
                }
            }
        },
        AppMessage::SkillMarketDocument(result) => match result {
            Ok(document) => {
                let entry_id = document.entry_id.clone();
                app.management_data.skill_preview = Some(document);
                if let Some(Overlay::SkillPreview { view }) = app.overlay.as_mut() {
                    view.loading = false;
                    view.scroll = 0;
                    view.notice = None;
                    // The answer names the entry it belongs to: a preview that
                    // arrived after the reader moved on is not theirs to read.
                    view.entry_id = entry_id;
                }
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                if let Some(Overlay::SkillPreview { view }) = app.overlay.as_mut() {
                    view.loading = false;
                    view.notice = Some((error.message, crate::config_center::ConfigTone::Bad));
                }
            }
        },
        AppMessage::NativeImport(result) => match result {
            Ok(preview) => {
                app.management_data.native_import = Some(preview);
                // The answer fills the picker the reader already has open, so
                // their place in it is kept rather than reset.
                app.overlay = Some(Overlay::NativeImport {
                    view: settled_native_import_view(&app.overlay),
                });
            }
            Err(error) => {
                app.toast(Toast::danger(error.message));
                if matches!(app.overlay, Some(Overlay::NativeImport { .. })) {
                    app.overlay = Some(Overlay::NativeImport {
                        view: settled_native_import_view(&app.overlay),
                    });
                }
            }
        },
        AppMessage::ProviderOrder(result) => match result {
            Ok(entries) => {
                // The read is per Agent and answers with that Agent's whole
                // arrangement, so it replaces those entries rather than adding
                // to them: two orders for one Agent is not an order.
                let agent_ids = entries
                    .iter()
                    .map(|entry| entry.agent_id.clone())
                    .collect::<Vec<_>>();
                app.management_data
                    .provider_order
                    .retain(|entry| !agent_ids.contains(&entry.agent_id));
                app.management_data.provider_order.extend(entries);
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::ProviderProfile(result) => {
            let pending = app.pending_provider_open.take();
            match result {
                Ok(profile) => {
                    let profile_id = profile.id.clone();
                    app.management_data.provider_detail = Some(profile);
                    // The reader asked for a surface before the read; the answer
                    // is what opens it.
                    if let Some((wanted, open)) = pending
                        && wanted == profile_id
                    {
                        let outcome = app.show_provider_open(&profile_id, open);
                        dispatch_all(worker, &outcome);
                    }
                }
                Err(error) => {
                    // A runtime that does not serve the full profile still gets
                    // an editor: it opens on what the summary carries, and the
                    // reader is told which part could not be read rather than
                    // finding a field mysteriously empty.
                    app.toast(Toast::warning(error.message));
                    if let Some((wanted, open)) = pending {
                        let outcome = app.show_provider_open(&wanted, open);
                        dispatch_all(worker, &outcome);
                    }
                }
            }
        }
        AppMessage::ProviderModels(result) => match result {
            Ok(models) => {
                // The catalogue is kept rather than printed: the Model editor
                // unions it with what the profile declares, so an entry the
                // profile does not offer yet becomes a row the reader can turn
                // on instead of a line in a report they have to retype.
                let count = models.len();
                app.management_data.provider_models = models;
                app.toast(Toast::success(format!(
                    "{count} {}",
                    app.strings.config_result_models()
                )));
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::AgentTools(result) => match result {
            Ok(preferences) => app.management_data.agent_tools = preferences,
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::Agents(result) => match result {
            Ok(agents) => {
                // What the authority is doing with an install the reader asked
                // for is on the record, not on the request: the phase the card
                // shows is read from here.
                if let Some(watch) = app.install_watch.as_mut()
                    && let Some(agent) = agents.iter().find(|agent| agent.id == watch.agent_id)
                {
                    watch.phase = Some(agent.managed_install.status);
                    watch.error = agent.managed_install.last_error_message.clone();
                    watch.version = agent.managed_install.available_version.clone();
                }
                app.management_data.agents = agents;
                app.management_data.mark_read(
                    crate::config_center::ConfigModule::Agent,
                    crate::config_center::ConfigSection::Agents,
                );
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
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                app.management_data.mark_read_failed(
                    crate::config_center::ConfigModule::Agent,
                    crate::config_center::ConfigSection::Agents,
                    error.code,
                    error.message,
                );
            }
        },
        AppMessage::AgentAuth { result, open } => match result {
            Ok(contexts) => {
                app.management_data.agent_auth = contexts;
                // The catalogue and the contexts answer one question between
                // them, and either read can move the account: a sign-out lands
                // here, and a panel left open must not go on claiming the
                // status the previous read earned.
                if let Some(catalog) = app.management_data.agent_auth_catalog.as_mut() {
                    crate::config_center::merge_agent_auth_context(
                        catalog,
                        &app.management_data.agent_auth,
                    );
                }
                if open {
                    let body =
                        app.management_data
                            .agent_auth
                            .iter()
                            .map(|context| {
                                let account = context.account_hint.clone().unwrap_or_else(|| {
                                    app.strings.config_result_signed_out().into()
                                });
                                format!("{}  ·  {account}", context.agent_id)
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                    app.overlay = Some(Overlay::TextView {
                        title: app.strings.management_login().to_string(),
                        body,
                        scroll: 0,
                    });
                }
            }
            Err(error) => app.toast(Toast::danger(error.message)),
        },
        AppMessage::AgentAuthTerminal {
            agent_id,
            terminal_id,
            title,
            result,
        } => match result {
            Ok(()) => {
                let opening = app.auth_terminal.is_none();
                // The terminal is the size of the panel it will be drawn in, not
                // the size the authority opened it at: a login flow that wraps
                // at a hundred columns draws badly in a seventy-column modal, and
                // the emulator reflowing is what keeps the two honest.
                let (rows, columns) = app.auth_terminal_size();
                app.auth_terminal = Some(crate::auth_terminal::AuthTerminal::new(
                    agent_id,
                    terminal_id,
                    title,
                    rows,
                    columns,
                ));
                if opening {
                    app.toast(Toast::info(
                        app.strings.config_auth_terminal_opened().to_string(),
                    ));
                }
            }
            // The panel says why it is not drawing a terminal rather than
            // leaving a blank box where the sign-in should be.
            Err(error) => {
                app.auth_terminal = None;
                if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
                    view.notice = Some((error.message, crate::config_center::ConfigTone::Warn));
                }
            }
        },
        AppMessage::AgentAuthTerminalFrame {
            terminal_id, bytes, ..
        } => {
            // A frame for a terminal this client is no longer attached to is
            // dropped: a second sign-in may already own the panel.
            if let Some(terminal) = app.auth_terminal.as_mut()
                && terminal.terminal_id == terminal_id
            {
                terminal.advance(&bytes);
                terminal.refresh();
            }
        }
        AppMessage::AgentAuthTerminalEnded { terminal_id } => {
            // The process is gone but the grid it left is not: the last frame
            // stays on screen so the reader can read what it said. What the
            // panel *does* change is the line that would otherwise leave the
            // reader typing into a terminal nobody is reading any more.
            if app
                .auth_terminal
                .as_ref()
                .is_some_and(|terminal| terminal.terminal_id == terminal_id)
                && let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut()
            {
                view.notice = Some((
                    app.strings.config_auth_terminal_ended().to_string(),
                    crate::config_center::ConfigTone::Muted,
                ));
            }
        }
        AppMessage::ConfigSources { module, mcp, skill } => {
            if module == crate::config_center::ConfigModule::Mcp {
                match mcp {
                    Ok(discoveries) => app.management_data.mcp_discoveries = discoveries,
                    Err(error) => app.toast(Toast::danger(error.message)),
                }
            }
            if let Some(skill) = skill {
                match skill {
                    Ok(discoveries) => app.management_data.skill_discoveries = discoveries,
                    Err(error) => app.toast(Toast::danger(error.message)),
                }
            }
        }
        AppMessage::Registry {
            module,
            mcp,
            skill,
            note,
        } => {
            if module == crate::config_center::ConfigModule::Mcp {
                match mcp {
                    Ok(entries) => app.management_data.mcp_market = entries,
                    Err(error) => app.toast(Toast::danger(error.message)),
                }
            } else if let Some(skill) = skill {
                match skill {
                    Ok(entries) => app.management_data.skill_market = entries,
                    Err(error) => app.toast(Toast::danger(error.message)),
                }
            }
            app.management_data.market_note = note;
            // The market is one area of two modules, so the answer is recorded
            // against the module that asked: the Skill catalog is not read just
            // because the MCP one was.
            app.management_data
                .mark_read(module, crate::config_center::ConfigSection::Catalog);
            // A registry that answered with fewer pages than the reader is on —
            // a search that matched less, a window that shrank — leaves the
            // page pointing past the end of its own answer, so it is put back
            // inside what came back rather than left showing nothing.
            app.clamp_registry_page();
        }
        AppMessage::ConfigReload => {
            let outcome = app.refresh_config_area();
            dispatch_all(worker, &outcome);
        }
        AppMessage::Mcp(result) => match result {
            Ok(servers) => {
                app.management_data.read_errors.remove("mcp/servers");
                app.management_data.mcp = servers;
                app.live = LiveState::Ready;
                app.management_data.mark_read(
                    crate::config_center::ConfigModule::Mcp,
                    crate::config_center::ConfigSection::Servers,
                );
            }
            // An area with a refusal in it is not an area that is still being
            // read, so the skeleton goes when the answer does — either way.
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                app.management_data.mark_read_failed(
                    crate::config_center::ConfigModule::Mcp,
                    crate::config_center::ConfigSection::Servers,
                    error.code,
                    error.message,
                );
            }
        },
        AppMessage::Skills(result) => match result {
            Ok(skills) => {
                app.management_data.skills = skills;
                app.live = LiveState::Ready;
                app.management_data.mark_read(
                    crate::config_center::ConfigModule::Skills,
                    crate::config_center::ConfigSection::Installed,
                );
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                app.management_data.mark_read_failed(
                    crate::config_center::ConfigModule::Skills,
                    crate::config_center::ConfigSection::Installed,
                    error.code,
                    error.message,
                );
            }
        },
        AppMessage::Prompts(result) => match result {
            Ok(prompts) => {
                app.management_data.prompts = prompts;
                app.live = LiveState::Ready;
                app.management_data.mark_read(
                    crate::config_center::ConfigModule::Prompts,
                    crate::config_center::ConfigSection::Prompts,
                );
            }
            Err(error) => {
                app.toast(Toast::danger(error.message.clone()));
                app.management_data.mark_read_failed(
                    crate::config_center::ConfigModule::Prompts,
                    crate::config_center::ConfigSection::Prompts,
                    error.code,
                    error.message,
                );
            }
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
                        // Each panel gets the list the authority resolved for it,
                        // which arrived whole in this one answer — the same
                        // division the desktop popup draws. The provider's
                        // ordering is already "most relevant first" inside each
                        // list, so nothing is re-sorted here.
                        let rows = |entries: &[vibex_core::AgentCommandEntry]| {
                            let mut rows = entries
                                .iter()
                                .map(|entry| {
                                    // A delegation target is described by the
                                    // account it runs as and the model it runs,
                                    // both written by the runtime; the account
                                    // is the one name this client says for
                                    // itself.
                                    let detail = entry.description.clone().unwrap_or_default();
                                    crate::composer::Completion {
                                        insert: entry.insertion_text.clone(),
                                        label: entry.label.clone(),
                                        detail: crate::transcript::runtime_attribution_line(
                                            app.strings,
                                            &detail,
                                        ),
                                        group: completion_source_label(entry.source_kind)
                                            .to_string(),
                                    }
                                })
                                .collect::<Vec<_>>();
                            rows.dedup_by(|left, right| left.insert == right.insert);
                            rows
                        };
                        match menu.trigger {
                            crate::composer::CompletionTrigger::Slash => {
                                menu.lists.commands = rows(&discovery.response.entries);
                                menu.lists.phrases = rows(&discovery.quick_phrases);
                            }
                            crate::composer::CompletionTrigger::At => {
                                menu.lists.files = rows(&discovery.response.entries);
                                menu.lists.delegation = rows(&discovery.delegation_entries);
                            }
                            crate::composer::CompletionTrigger::Dollar => {
                                menu.lists.commands = rows(&discovery.response.entries);
                            }
                        }
                        menu.loading = false;
                        // The reader may have typed past the row the drawer had
                        // highlighted while this answer travelled.
                        menu.reseat(menu.query.clone());
                        let rows = menu.items().len();
                        menu.selected = menu.selected.min(rows.saturating_sub(1));
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
            // Read for notification before the controller sees it: a background
            // session's failure is dropped as stale there, and it is exactly
            // the event the reader is not at the terminal to see.
            app.observe_notification_event(&event);
            if app.dock_open
                && !app.team_busy
                && let Some(effect) = app.load_team(false)
            {
                worker.dispatch(effect);
            }
            // A sidebar invalidated on the authority means the list this client
            // is drawing is out of date: the reader rearranged it somewhere
            // else, or another client created a session in it. A remote
            // authority announces a new session this way and through no other
            // event, so the sessions and their project headings are re-read
            // along with the arrangement — re-reading the arrangement alone
            // would redraw the same rows without the new one.
            //
            // A fan-out that lagged past the sidebar is the same news from the
            // other direction: the invalidation itself was the event that got
            // dropped, and the same reads answer both.
            let sidebar_invalidated = matches!(
                &event,
                vibex_backend::BackendEvent::ProjectionInvalidated(
                    vibex_backend::BackendProjection::Sidebar
                )
            ) || matches!(
                &event,
                vibex_backend::BackendEvent::Lagged { refetch, .. }
                    if refetch.projection == Some(vibex_backend::BackendProjection::Sidebar)
            );
            if sidebar_invalidated {
                for effect in app.sidebar_projection_effects() {
                    worker.dispatch(effect);
                }
            }
            // The unread mark and the last thing a session did are both the
            // client's own readings of the events it already receives: the
            // event says what arrived, the list says where the reader was when
            // it did, and what the session was last seen saying.
            if let vibex_backend::BackendEvent::Timeline(item) = &event {
                app.note_session_echo(&item.item);
                if app.note_activity(item) {
                    // The mark is the reader's, but the badge is shared: the
                    // Desktop is another window on this runtime, and the
                    // finished answer it did not receive here is just as new
                    // there. Sending the mark is what makes the two lists agree;
                    // a mark the authority already holds sends nothing.
                    if let Some(effect) = app.sidebar_mark_unread_effect(&item.session_id) {
                        worker.dispatch(effect);
                    }
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
            // The name this client recorded for a session it created stands in
            // while the authority still reports the manager's placeholder, so an
            // early update cannot put the default back on a named row.
            let event = app.reconcile_session_event(event);
            let session_update = matches!(&event, vibex_backend::BackendEvent::SessionUpdated(_));
            // A removal names sessions this client may still badge or hold an
            // echo for; the controller drops the shared rows, so the client's
            // own per-session state has to go with them or the maps keep a
            // line for a row no reader can ever open again.
            if let vibex_backend::BackendEvent::SessionsRemoved(session_ids) = &event {
                app.forget_removed_sessions(session_ids);
            }
            let decision = app.agent.apply_event(event);
            match decision {
                vibex_ui::AgentEventDecision::Applied => {
                    app.live = LiveState::Ready;
                    app.sync_transcript();
                }
                vibex_ui::AgentEventDecision::NeedsAuthoritativeRefetch if session_update => {
                    // A new session event can describe an owned worker. Only
                    // the authority's root list may add a navigation row;
                    // refreshing it must preserve the worker already open.
                    if let Some(effect) = app.refresh_session_list() {
                        worker.dispatch(effect);
                    }
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

/// Build a prompt overlay for the given field. Used by the composition root for
/// first-run flows such as a workspace path.
pub fn prompt_for(app: &mut App, title: String, field: PromptField) {
    app.overlay = Some(Overlay::Prompt {
        title,
        field,
        value: String::new(),
        target: None,
    });
}

/// The import picker with its scan finished, keeping the reader's place.
fn settled_native_import_view(overlay: &Option<Overlay>) -> crate::config_center::NativeImportView {
    match overlay {
        Some(Overlay::NativeImport { view }) => crate::config_center::NativeImportView {
            loading: false,
            selected: view.selected,
            notice: view.notice.clone(),
        },
        _ => crate::config_center::NativeImportView {
            loading: false,
            ..Default::default()
        },
    }
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
            app.composer_takes_keys(),
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
        assert!(!app.composer_takes_keys());
    }

    #[test]
    fn enter_takes_the_completion_row_instead_of_sending_the_draft() {
        // Regression: with the drawer open, `Enter` fell through to the binding
        // table and sent the draft, so the command the reader had just chosen
        // never reached the box — they watched a half-typed trigger word go out
        // instead. Taking the row is what the desktop composer answers the same
        // popup with.
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.resize(100, 30);
        app.perform(crate::action::Intent::NewSession);
        app.agent.state.selected_session_id = None;
        app.focus = crate::app::Focus::Composer;
        assert!(app.composer_takes_keys());
        assert_eq!(app.composer.text(), "");
        // The composer is the page's own target, so nothing has to be opened for
        // the draft to be ready to send.
        app.composer.set_text("testest /com");
        app.refresh_completion();
        {
            let menu = app.completion.as_mut().expect("a completion menu");
            menu.lists.commands = vec![crate::composer::Completion {
                insert: "/compact ".to_string(),
                label: "compact".to_string(),
                detail: "Compact the conversation".to_string(),
                group: "Command".to_string(),
            }];
            menu.loading = false;
            menu.reseat(menu.query.clone());
        }
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(
            app.composer.text(),
            "testest /compact ",
            "the chosen command did not land in the box"
        );
        assert!(app.pending_creations.is_empty(), "the draft was sent");
        assert!(app.completion.is_none());

        // A drawer with nothing to offer is not a reason to swallow `Enter`: the
        // key falls through to the binding table, which sends the draft.
        app.composer.set_text("testest /com");
        app.refresh_completion();
        let menu = app.completion.as_mut().expect("a completion menu");
        menu.lists.commands.clear();
        menu.loading = false;
        assert_eq!(
            handle_composer_key(
                &mut app,
                &worker,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .unwrap(),
            None,
            "an empty drawer took the key"
        );
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
            app.composer_takes_keys(),
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
    fn ctrl_l_reaches_the_session_list_from_the_transcript() {
        // The global digit that names the session list is taken by the composer
        // whenever a draft owns the keyboard, so the page binds the chord
        // itself. A reader whose hand is on the transcript is not typing in a
        // draft: without this binding there is no key at all that leaves the
        // session, which is the page the reader spends the session on.
        use crate::action::Intent;
        let mut app = isolation_app();
        app.navigate_to(crate::app::Page::Agent);
        app.focus = crate::app::Focus::Main;
        let chord = crate::keymap::Chord::ctrl('l');
        // The resolution `handle_key` performs, against the scopes the page
        // actually publishes.
        let intent = app
            .keymap
            .resolve(&app.active_scopes(), chord)
            .expect("Ctrl+L is unbound on the session page");
        assert_eq!(intent, Intent::GotoSessions);
        app.perform(intent);
        assert_eq!(
            app.page,
            crate::app::Page::Sessions,
            "Ctrl+L did not reach the session list"
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
        // The band lists the messages held for the session on screen, in the
        // window it scrolled to. The first drawn line here is the queue's
        // third message, so that is the row a pointer on it is over.
        app.regions.queue_rows.push(2);
        assert_eq!(hover_target(&app, 40, 2), None, "a row above the band");
        assert_eq!(
            hover_target(&app, 200, 2),
            None,
            "a column outside the band"
        );
        assert_eq!(
            hover_target(&app, 40, 30),
            Some((crate::keymap::Scope::Agent, 2)),
            "the pointer is over the message the band drew, not the first one"
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
                    member_count: 1,
                    auto_routes: false,
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
            tools: Default::default(),
            transport: vibex_core::SessionTransport::Acp,
            terminal: None,
        }
    }

    /// The same session before anything has named it: what the manager answers
    /// with for a creation that carried no title.
    fn unnamed_session(id: vibex_core::VibexSessionId, agent: &str) -> vibex_core::AgentSession {
        let mut session = isolation_session(id, agent);
        session.title = format!("{agent} session");
        session
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
        // The picker opens with every Agent but the one in use folded, so a test
        // that points at another group opens it first, the way a reader does.
        let agent_id = app.runtime_options.as_ref().expect("catalogue").options[index]
            .selection
            .agent_id
            .clone();
        app.fold_runtime_group(&agent_id, false);
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
    fn a_remote_sidebar_invalidation_re_reads_the_session_list() {
        // A remote authority publishes no session-update event: a session the
        // desktop has just created reaches this client as an invalidated
        // sidebar and nothing else. Re-reading the arrangement alone leaves the
        // new row out of the list, so the invalidation has to re-read the
        // sessions too.
        let (worker, mut messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        app.agent
            .apply_sessions(Ok(vec![isolation_session(
                vibex_core::VibexSessionId::new(),
                "codex",
            )]))
            .unwrap();
        assert!(!app.agent.state.sessions.is_loading());

        // A projection this client draws no session rows from is not a session
        // read: a management invalidation must not cost one.
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::ProjectionInvalidated(
                vibex_backend::BackendProjection::Management,
            )),
        )
        .unwrap();
        assert!(!app.agent.state.sessions.is_loading());

        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::ProjectionInvalidated(
                vibex_backend::BackendProjection::Sidebar,
            )),
        )
        .unwrap();
        assert!(
            app.agent.state.sessions.is_loading(),
            "the invalidated sidebar did not re-read the sessions it arranges"
        );
        // The reads the worker was asked for are the ones the invalidation
        // promised, so the wiring, not just the reducer, is what is asserted.
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let reads = observer.block_on(async {
            let mut reads = Vec::new();
            while reads.len() < 3 {
                match tokio::time::timeout(Duration::from_secs(5), messages.recv()).await {
                    Ok(Some(AppMessage::Sessions(_))) => reads.push("sessions"),
                    Ok(Some(AppMessage::Workspaces(_))) => reads.push("workspaces"),
                    Ok(Some(AppMessage::SidebarOrganization(_))) => reads.push("arrangement"),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            reads
        });
        for read in ["sessions", "workspaces", "arrangement"] {
            assert!(reads.contains(&read), "the invalidation never read {read}");
        }

        // A fan-out that lagged past the sidebar dropped the invalidation
        // itself, and the refetch it asks for names the same projection.
        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![isolation_session(
                vibex_core::VibexSessionId::new(),
                "codex",
            )])),
        )
        .unwrap();
        assert!(!app.agent.state.sessions.is_loading());
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::Lagged {
                stream: vibex_backend::BackendEventStream::Fanout,
                skipped: 4,
                refetch: vibex_backend::BackendRefetch {
                    session_id: None,
                    timeline: false,
                    runtime: false,
                    runtime_selection: false,
                    projection: Some(vibex_backend::BackendProjection::Sidebar),
                },
                observed_live: false,
            }),
        )
        .unwrap();
        assert!(
            app.agent.state.sessions.is_loading(),
            "a lag that named the sidebar left the list stale"
        );
    }

    #[test]
    fn a_finished_answer_elsewhere_reports_the_unread_mark_to_the_authority() {
        // The event loop, not just the reducer: a completion for a session the
        // reader is not looking at must reach the worker as a sidebar mutation,
        // or the Desktop window never learns to badge it. The write is observed
        // where every other sidebar write is — the answer that comes back.
        use vibex_core::{AgentMessagePayload, TimelinePayload};
        let (worker, mut messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        let elsewhere = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        app.agent
            .apply_sessions(Ok(vec![elsewhere.clone()]))
            .unwrap();
        // The reader is looking at some other session, so the answer lands in
        // the background.
        let open = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        app.agent.state.selected_session_id = Some(open.id.clone());
        app.agent.state.active_session.resolve(open.clone());

        let sequence = 1;
        let item = conversation_item(
            &elsewhere.id,
            sequence,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::Timeline(
                vibex_core::TimelineLiveEvent {
                    session_id: elsewhere.id.clone(),
                    sequence,
                    item,
                },
            )),
        )
        .unwrap();
        assert!(
            app.session_is_unread(&elsewhere.id),
            "the completion did not mark the background session unread"
        );

        // The mutation was dispatched to the worker: its answer — a refusal from
        // the disconnected backend — is the proof, and it names the mark.
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let reported = observer.block_on(async {
            loop {
                match tokio::time::timeout(Duration::from_secs(5), messages.recv()).await {
                    Ok(Some(AppMessage::SidebarOrganizationMutated(_))) => break true,
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break false,
                }
            }
        });
        assert!(
            reported,
            "the unread mark never reached the worker's sidebar mutation"
        );
    }

    #[test]
    fn a_sidebar_invalidation_settles_the_open_sessions_turn() {
        // The authority publishes no session update to a remote client, so a
        // turn it has settled reaches this one as nothing but an invalidated
        // sidebar. The list read that follows is the only copy of the state
        // the page gets, and without the fold it kept drawing the turn as
        // running — spinner, elapsed clock and "Steer" placeholder included —
        // until the reader re-opened the session by hand.
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        let mut running = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        running.state = vibex_core::AgentSessionState::Running;
        app.agent.apply_sessions(Ok(vec![running.clone()])).unwrap();
        app.agent.state.selected_session_id = Some(running.id.clone());
        app.agent.state.active_session.resolve(running.clone());
        app.navigate_to(Page::Agent);
        assert!(app.turn_reads_running());

        let mut settled = running.clone();
        settled.state = vibex_core::AgentSessionState::Idle;
        settled.updated_at_ms = running.updated_at_ms + 1;
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::ProjectionInvalidated(
                vibex_backend::BackendProjection::Sidebar,
            )),
        )
        .unwrap();
        assert!(app.agent.state.sessions.is_loading());
        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![settled.clone()])),
        )
        .unwrap();

        assert_eq!(app.active_session(), Some(&settled));
        assert!(!app.turn_reads_running());
        assert!(
            !app.is_animating(),
            "the settled page is still repainting for a turn that ended"
        );
    }

    #[test]
    fn a_running_turn_is_timed_from_its_own_start_not_from_the_readers_arrival() {
        // The line above the composer dates the turn, not the visit: opening a
        // session that is already working must show how long that work has
        // taken — the same number the desktop shows — rather than a clock that
        // started when the reader arrived.
        use vibex_core::{
            AgentMessageDeltaPayload, AgentMessagePhase, TimelinePayload, UserMessagePayload,
        };
        let mut app = isolation_app();
        let mut running = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        running.state = vibex_core::AgentSessionState::Running;
        app.agent.apply_sessions(Ok(vec![running.clone()])).unwrap();
        app.agent.state.selected_session_id = Some(running.id.clone());
        app.agent.state.active_session.resolve(running.clone());
        app.navigate_to(Page::Agent);

        // The turn began a minute and a half before this client opened it.
        let started_at_ms = vibex_core::unix_timestamp_ms() - 90_000;
        let mut prompt = conversation_item(
            &running.id,
            1,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "what is taking so long?".into(),
                ..Default::default()
            }),
        );
        prompt.timestamp_ms = started_at_ms;
        let mut answer = conversation_item(
            &running.id,
            2,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "still working".into(),
                chunk_index: 0,
                phase: Some(AgentMessagePhase::FinalAnswer),
            }),
        );
        answer.timestamp_ms = started_at_ms + 1_000;
        app.agent
            .state
            .timeline
            .replace_authoritative(running.id.clone(), vec![prompt, answer]);
        app.sync_transcript();

        let elapsed = app
            .turn_elapsed()
            .expect("a running turn has an elapsed time");
        assert!(
            elapsed >= Duration::from_secs(89),
            "the clock restarted when the reader arrived: {elapsed:?}"
        );
        assert!(
            elapsed <= Duration::from_secs(91),
            "the clock is not the turn's own: {elapsed:?}"
        );
    }

    #[test]
    fn a_list_read_that_settles_the_turn_stops_its_rows_from_streaming() {
        // The fold is the page's copy of the state, and the transcript is where
        // that state is drawn: a provider that streams its answer as deltas and
        // never sends a final message leaves its last row arriving as far as the
        // projection can tell. Folding the settled state in without rebuilding
        // the transcript left the turn line drawing "running" — spinner, elapsed
        // clock and all — over a turn the runtime had already finished.
        use vibex_core::{AgentMessageDeltaPayload, TimelinePayload};
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        let mut running = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        running.state = vibex_core::AgentSessionState::Running;
        app.agent.apply_sessions(Ok(vec![running.clone()])).unwrap();
        app.agent.state.selected_session_id = Some(running.id.clone());
        app.agent.state.active_session.resolve(running.clone());
        app.navigate_to(Page::Agent);
        app.agent.state.timeline.replace_authoritative(
            running.id.clone(),
            vec![conversation_item(
                &running.id,
                1,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "the answer".to_string(),
                    chunk_index: 0,
                    phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
                }),
            )],
        );
        app.sync_transcript();
        assert!(
            app.transcript_animating(),
            "the running turn's answer is not arriving"
        );
        let live = conversation_frame(&mut app, 100, 30);
        assert!(
            live.contains(app.strings.phase_generating()),
            "the running turn's phase is not on its own line:\n{live}"
        );

        let mut settled = running.clone();
        settled.state = vibex_core::AgentSessionState::Idle;
        settled.updated_at_ms = running.updated_at_ms + 1;
        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![settled.clone()])),
        )
        .unwrap();

        assert_eq!(app.active_session(), Some(&settled));
        // The read settles the page's copy of the state; the loop's own step is
        // what settles the projection drawn from it.
        assert!(
            app.sync_transcript_liveness(),
            "the loop did not notice the state move"
        );
        assert!(
            !app.sync_transcript_liveness(),
            "the loop rebuilt the transcript for a state it had already drawn"
        );
        assert!(
            !app.transcript_animating(),
            "a finished turn is still streaming"
        );
        let settled_frame = conversation_frame(&mut app, 100, 30);
        assert!(
            !settled_frame.contains(app.strings.running()),
            "the settled session still reads as running:\n{settled_frame}"
        );
        assert!(
            settled_frame.contains(app.strings.idle()),
            "the settled session does not read as idle:\n{settled_frame}"
        );
    }

    #[test]
    fn worker_session_events_refresh_roots_without_retargeting_the_open_worker() {
        use vibex_core::{AgentMessageDeltaPayload, TimelinePayload};
        let (worker, mut messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        app.dock_open = false;
        let root = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        let mut child = isolation_session(vibex_core::VibexSessionId::new(), "deepseek-harness");
        app.agent.apply_sessions(Ok(vec![root.clone()])).unwrap();
        let ticket = app
            .open_session_effects(child.id.clone())
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                crate::app::Effect::OpenSession { ticket, .. } => Some(ticket),
                _ => None,
            })
            .unwrap();
        let item = conversation_item(
            &child.id,
            1,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "Worker answer".to_string(),
                chunk_index: 0,
                phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
            }),
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionOpened {
                ticket,
                result: Ok(vibex_ui::AgentSessionSnapshot {
                    session: child.clone(),
                    timeline: vec![item.clone()],
                    runtime_selection: None,
                    timeline_has_older: false,
                }),
            },
        )
        .unwrap();
        app.composer.set_text("Keep this worker draft");
        let generation = app.agent.state.generation;

        for revision in [2, 3] {
            child.updated_at_ms = revision;
            child.title = format!("Worker revision {revision}");
            apply_message(
                &mut app,
                &worker,
                AppMessage::Event(vibex_backend::BackendEvent::SessionUpdated(child.clone())),
            )
            .unwrap();
            assert!(app.agent.state.sessions.is_loading());
            assert_eq!(
                app.agent.state.sessions.value.as_ref().unwrap(),
                &vec![root.clone()]
            );
            assert_eq!(app.agent.state.active_session.value.as_ref(), Some(&child));
            assert_eq!(app.agent.state.generation, generation);
            assert_eq!(app.live, LiveState::Ready);
        }

        let new_root = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::SessionUpdated(
                new_root.clone(),
            )),
        )
        .unwrap();

        async fn receive_only_root_refresh(
            messages: &mut tokio::sync::mpsc::UnboundedReceiver<AppMessage>,
        ) {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match messages.recv().await {
                        Some(AppMessage::Sessions(_)) => break,
                        Some(AppMessage::Notice { .. }) => {}
                        unexpected => {
                            panic!("expected only a root-list refresh, got {unexpected:?}")
                        }
                    }
                }
            })
            .await
            .expect("root-list refresh was dispatched");
            assert!(
                tokio::time::timeout(Duration::from_millis(50), messages.recv())
                    .await
                    .is_err(),
                "worker metadata events must coalesce while the root list is loading"
            );
        }

        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        observer.block_on(receive_only_root_refresh(&mut messages));
        // This request captured its snapshot before the new root appeared.
        // The pending invalidation must produce a fresh read after it returns.
        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![root.clone()])),
        )
        .unwrap();
        assert!(app.agent.state.sessions.is_loading());
        observer.block_on(receive_only_root_refresh(&mut messages));
        let roots = vec![root, new_root];
        apply_message(&mut app, &worker, AppMessage::Sessions(Ok(roots.clone()))).unwrap();
        assert!(!app.agent.state.sessions.is_loading());
        assert_eq!(app.agent.state.sessions.value, Some(roots));
        assert_eq!(
            app.agent.state.selected_session_id.as_ref(),
            Some(&child.id)
        );
        assert_eq!(app.agent.state.active_session.value.as_ref(), Some(&child));
        assert_eq!(app.agent.state.timeline.items, vec![item]);
        assert_eq!(app.agent.state.generation, generation);
        assert_eq!(app.composer.text(), "Keep this worker draft");
        assert_eq!(app.page, Page::Agent);
        assert_eq!(app.live, LiveState::Ready);
    }

    #[test]
    fn a_read_and_an_update_keep_the_name_the_reader_gave_its_new_session() {
        // The client names the row the moment it asks for the session, because
        // the authority has no name for it until the message reaches a turn.
        // Everything the authority says in the meantime is a placeholder, and
        // none of it may take the reader's own words off the row.
        let worker = isolation_worker();
        let mut app = isolation_app();
        let session_id = vibex_core::VibexSessionId::new();
        app.note_pending_session_title(&session_id, "  Fix   the flake  ", &[]);

        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![unnamed_session(session_id.clone(), "codex")])),
        )
        .unwrap();
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Fix the flake")
        );

        // The management of the session it just persisted arrives under the
        // same placeholder.
        apply_message(
            &mut app,
            &worker,
            AppMessage::Event(vibex_backend::BackendEvent::SessionUpdated(
                unnamed_session(session_id.clone(), "codex"),
            )),
        )
        .unwrap();
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Fix the flake"),
            "the placeholder took the row back"
        );

        // The authority's own title — the seed from that message, or one an
        // Agent supplied — wins, and from then on the row is the authority's.
        let mut named = unnamed_session(session_id.clone(), "codex");
        named.title = "Investigate the flake".to_string();
        apply_message(&mut app, &worker, AppMessage::Sessions(Ok(vec![named]))).unwrap();
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Investigate the flake")
        );

        apply_message(
            &mut app,
            &worker,
            AppMessage::Sessions(Ok(vec![unnamed_session(session_id.clone(), "codex")])),
        )
        .unwrap();
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("codex session"),
            "the name the reader's message gave is still standing over the authority's"
        );
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
    fn a_streamed_tool_call_stays_one_row_in_the_transcript() {
        use vibex_core::{TimelinePayload, ToolCallPayload, ToolCallStatus};
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.resize(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        let mut session = isolation_session(id.clone(), "deepseek-harness");
        session.state = vibex_core::AgentSessionState::Running;
        app.agent.state.selected_session_id = Some(id.clone());
        app.agent.state.active_session.resolve(session);
        app.agent
            .state
            .timeline
            .replace_authoritative(id.clone(), vec![]);

        // One tool call arrives as a run of updates: the same call id with a
        // growing payload, from the first fragment to the finished input.
        let inputs = [
            r#"{"arguments":""}"#,
            r#"{"arguments":"{\"command"}"#,
            r#"{"command":"ls -la"}"#,
        ];
        for (index, input) in inputs.into_iter().enumerate() {
            let sequence = index as i64 + 1;
            let item = conversation_item(
                &id,
                sequence,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "call-1".into(),
                    tool_name: "execute".into(),
                    status: if index + 1 == inputs.len() {
                        ToolCallStatus::Completed
                    } else {
                        ToolCallStatus::Progress
                    },
                    summary: "ls -la".into(),
                    input_summary: Some(input.into()),
                    output_summary: None,
                    raw_extension: None,
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
            assert_eq!(
                app.transcript.len(),
                1,
                "update {sequence} drew a second row for one tool call"
            );
        }

        // The block keeps the name of the update that opened the row.
        // The block keeps the name of the update that opened the row.
        assert_eq!(app.transcript.blocks()[0].id, "tool:call-1:1");
        let screen = conversation_frame(&mut app, 100, 30);
        assert!(screen.contains("ls -la"), "{screen}");
        assert!(
            !screen.contains("arguments"),
            "a half-parsed payload reached the screen: {screen}"
        );
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
            load: vibex_ui::TimelineLoadKind::Cold,
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

    /// The queue band's legends are controls, not decoration: a click on
    /// `Alt+E edit` does what pressing `Alt+E` does.
    #[test]
    fn a_click_on_a_queue_legend_presses_its_key() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let session = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        app.agent
            .apply_sessions(Ok(vec![session.clone()]))
            .expect("sessions apply");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(Page::Agent);
        app.enqueue_message("fix the flake".to_string());
        // The queue's legends are drawn only while its cursor is in it.
        app.queue_selection = Some(0);

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        // The legend under the pointer is the one for editing, and it is
        // published where the band drew it.
        let edit = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord == "alt+e".parse().unwrap())
            .map(|hint| hint.rect)
            .expect("the queue band publishes an edit legend");

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: edit.x,
                row: edit.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.composer.text(),
            "fix the flake",
            "the click did not take the held message back into the draft"
        );
        assert!(app.queued_messages.is_empty(), "the message is still held");
    }

    /// A legend the drafter could aim at but not use would be worse than none:
    /// every one the queue draws resolves to a key the table binds.
    #[test]
    fn every_queue_legend_names_a_bound_key() {
        for legend in ["Alt+↑↓", "Alt+E", "Alt+X", "Alt+J/K", "Alt+S", "Alt+Enter"] {
            let chord = crate::keymap::Chord::from_hint(legend)
                .unwrap_or_else(|| panic!("{legend} is not read as a key"));
            assert!(
                crate::keymap::DEFAULT_BINDINGS
                    .iter()
                    .any(|binding| binding.chord == chord),
                "{legend} is not bound to anything"
            );
        }
    }

    /// The dock prints its own key legends while its cursor owns the list, and
    /// they are presses too: the panel exists to act on the work in flight.
    #[test]
    fn a_click_on_a_dock_legend_presses_its_key() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let session = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        app.agent
            .apply_sessions(Ok(vec![session.clone()]))
            .expect("sessions apply");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(Page::Agent);
        app.enqueue_message("fix the flake".to_string());
        app.perform(crate::action::Intent::ToggleDock);
        assert!(app.dock_is_focused(), "the dock does not own its cursor");

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        // `Alt+H` hides the finished rows; the click has to reach the key, not
        // a second copy of what the key was thought to mean.
        let hide = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord == "alt+h".parse().unwrap())
            .map(|hint| hint.rect)
            .expect("the dock publishes a hide-done legend");

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hide.x,
                row: hide.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        // The reducer toggles the panel's own filter, which is the observable
        // effect of the key the legend names.
        assert!(
            app.dock_hide_done,
            "the click did not run the key the legend names"
        );
    }

    /// A modal's footer is a row of controls, not a caption: its `Esc` closes
    /// the box exactly as the key does.
    #[test]
    fn a_click_on_a_modal_footer_closes_it() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.navigate_to(Page::Agent);
        app.perform(crate::action::Intent::OpenCommandPalette);
        assert!(app.overlay.is_some(), "the palette did not open");

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        let close = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord.code == KeyCode::Esc)
            .map(|hint| hint.rect)
            .expect("the footer publishes its close legend");

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.overlay.is_none(),
            "the footer's Esc did not close the box"
        );
    }

    /// A footer that draws `↑↓` is two targets, not one: the arrow under the
    /// pointer is the direction its press takes.
    #[test]
    fn the_two_halves_of_an_arrow_legend_step_opposite_ways() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.navigate_to(Page::Agent);
        app.perform(crate::action::Intent::OpenCommandPalette);
        // Three commands, so a step down from the first is observable.
        app.overlay = Some(crate::app::Overlay::Palette {
            query: String::new(),
            selected: 0,
        });

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        let up = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord.code == KeyCode::Up)
            .map(|hint| hint.rect)
            .expect("the footer publishes the up half");
        let down = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord.code == KeyCode::Down)
            .map(|hint| hint.rect)
            .expect("the footer publishes the down half");
        assert_eq!(up.y, down.y, "the two halves are not on one row");
        assert_eq!(
            down.x,
            up.x + 1,
            "the halves are not the two cells of the glyph pair"
        );

        let selected = |app: &crate::App| match &app.overlay {
            Some(crate::app::Overlay::Palette { selected, .. }) => *selected,
            other => panic!("the palette is not open: {other:?}"),
        };
        let first = selected(&app);
        let click = |app: &mut crate::App, x: u16| {
            let _ = handle_mouse(
                app,
                &worker,
                None,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: up.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        };
        click(&mut app, down.x);
        assert_ne!(selected(&app), first, "the down half did not step down");
        click(&mut app, up.x);
        assert_eq!(
            selected(&app),
            first,
            "the up half did not step back the way it came"
        );
    }

    /// The settings footer names the keys of the mode it is in, and they are
    /// presses: the footer is the only place the mode says what it answers to.
    #[test]
    fn a_click_on_a_settings_footer_legend_presses_its_key() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.navigate_to(Page::Settings);
        // Browse mode's footer starts with `↑↓`, so a step down is observable.
        app.set_selection(crate::keymap::Scope::Settings, 0);

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        let down = app
            .regions
            .key_hints
            .iter()
            .find(|hint| hint.chord.code == KeyCode::Down)
            .map(|hint| hint.rect)
            .expect("the settings footer publishes its down legend");

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: down.x,
                row: down.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.selection_for(crate::keymap::Scope::Settings),
            1,
            "the click did not step the settings cursor"
        );
    }

    /// A click on a scrolled queue band selects the message the band drew, not
    /// the one whose index happens to match the line number.
    #[test]
    fn a_click_on_a_scrolled_queue_band_selects_the_row_under_the_pointer() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        let session = isolation_session(vibex_core::VibexSessionId::new(), "codex");
        app.agent
            .apply_sessions(Ok(vec![session.clone()]))
            .expect("sessions apply");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(Page::Agent);
        for index in 0..5 {
            app.enqueue_message(format!("held {index}"));
        }
        // The cursor on the last message scrolls the window, so the first rows
        // are off the top and the line numbers no longer match the indexes.
        app.queue_selection = Some(4);

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        let band = app.regions.queue.expect("the queue band has a rect");
        let rows = app.regions.queue_rows.clone();
        assert!(
            rows.len() < 5 && rows[0] > 0,
            "the fixture did not scroll: {rows:?} shows the whole queue from its first row"
        );
        let top_row = rows[0];

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: band.x + 2,
                row: band.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.queue_selection,
            Some(top_row),
            "the click did not land on the row the band drew at the top"
        );
    }

    /// The new-session page names the keys that change where the message will
    /// go, and those names are presses: the page offers no button for either.
    #[test]
    fn a_click_on_the_new_session_hints_switches_the_runtime() {
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.navigate_to(Page::NewSession);

        let mut screen =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        screen
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        // The runtime switcher is a modal this opens, so the observable answer
        // is the overlay rather than a field on the page.
        let switch = app
            .regions
            .hints
            .iter()
            .find(|(_, intent)| *intent == crate::action::Intent::SwitchAgentRuntime)
            .map(|(rect, _)| *rect)
            .expect("the page publishes its runtime key");

        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: switch.x,
                row: switch.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.overlay.is_some(),
            "the click on the runtime key did not open the switcher"
        );
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
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
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
            let _ = handle_mouse(
                &mut app,
                &worker,
                None,
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
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
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
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
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
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
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

    /// A row's rail is one line down the list: the mark beside a session's
    /// title and the mark beside what it last did stand in the same column,
    /// however deep the tree the row hangs in. Drawn before the row's own
    /// indent instead, a nested row's rail broke in two — the title's mark
    /// stood under the heading's children and the echo's at the page's edge.
    #[test]
    fn a_session_row_draws_one_rail_down_both_its_lines() {
        let mut app = test_app(100, 24);
        let project = vibex_core::ProjectId::new();
        let workspace = vibex_core::WorkspaceId::new();
        let sessions = (1..=2)
            .map(|index| vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse(format!("session_rail{index:04}"))
                    .expect("valid session id"),
                title: format!("railed {index}"),
                // One workspace, so the two are rows 1 and 2 under one heading.
                project_id: project.clone(),
                workspace_id: workspace.clone(),
                workspace_root: "/tmp/vibex-rail-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: index,
                updated_at_ms: index,
                last_message_at_ms: index,
                archived_at_ms: None,
                deleted_at_ms: None,
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
            })
            .collect::<Vec<_>>();
        app.agent
            .apply_sessions(Ok(sessions.clone()))
            .expect("apply");
        app.note_session_echo(&conversation_item(
            &sessions[0].id,
            1,
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "Read crates/vibex-tui/src/transcript.rs".to_string(),
                is_final: true,
            }),
        ));
        app.navigate_to(Page::Sessions);
        // The arrangement decides which row a session lands on, so the cursor
        // finds it rather than counting to it.
        let index = app
            .sidebar_rows()
            .iter()
            .position(|row| row.session_id.as_ref() == Some(&sessions[0].id))
            .expect("the session is not in the list");
        app.set_selection(crate::keymap::Scope::Sessions, index);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, &mut app))
            .expect("the session list draws");
        let buffer = terminal.backend().buffer();
        let line = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let echo_row = (0..buffer.area.height)
            .find(|row| line(*row).contains("transcript.rs"))
            .expect("the session's echo is not on screen");
        let title_row = echo_row
            .checked_sub(1)
            .expect("the echo is on the page's first line");
        assert!(
            line(title_row).contains("railed 1"),
            "the echo is not the second line of its own row:\n{}\n{}",
            line(title_row),
            line(echo_row)
        );
        // The row is the cursor's, so its rail is drawn rather than blanked.
        let bar = crate::glyphs::accent_bar(app.glyph_tier());
        let mark_column = |row: u16| -> Option<u16> {
            (0..buffer.area.width).find(|column| {
                buffer
                    .cell((*column, row))
                    .is_some_and(|cell| cell.symbol() == bar)
            })
        };
        let first_column = |row: u16| -> Option<u16> {
            (0..buffer.area.width).find(|column| {
                buffer
                    .cell((*column, row))
                    .is_some_and(|cell| cell.symbol() != " ")
            })
        };
        let text_column = |row: u16, after: u16| -> Option<u16> {
            (after + 1..buffer.area.width).find(|column| {
                buffer
                    .cell((*column, row))
                    .is_some_and(|cell| cell.symbol() != " ")
            })
        };
        let mark = mark_column(title_row).expect("the row drew no rail");
        // The row hangs under a heading, which is what the second line's indent
        // had to keep: without the nest there is nothing to get wrong. The
        // heading is found by the name the list gave it, so a sibling row drawn
        // above this one is not mistaken for it.
        let rows = app.sidebar_rows();
        let heading = rows[..index]
            .iter()
            .rev()
            .find(|row| row.kind != vibex_desktop_model::AgentSidebarRowKind::Session)
            .expect("the session hangs under no heading");
        let heading_row = (0..title_row)
            .find(|row| line(*row).contains(&heading.label))
            .expect("the row's heading is not on screen");
        assert_eq!(
            mark,
            first_column(heading_row).expect("the heading is blank") + 2,
            "the row is not indented under its heading:\n{}",
            line(title_row)
        );
        assert_eq!(
            Some(mark),
            mark_column(echo_row),
            "the row's two lines did not draw one rail:\n{}\n{}",
            line(title_row),
            line(echo_row)
        );
        // The echo opens in the column the title's own text took, which is what
        // drawing the rail after the indent buys: the row's whole lead on from
        // the mark — the rail, its gap, the Agent's two-cell mark and its gap.
        assert_eq!(
            text_column(echo_row, mark),
            Some(mark + crate::view::SESSION_ROW_LEAD as u16),
            "the echo does not open under its title:\n{}\n{}",
            line(title_row),
            line(echo_row)
        );
        // The band is the row, not the sentence: both of a row's lines end in
        // the same column, and the echo's band runs on past its own text rather
        // than stopping where the sentence happened to.
        let band = app.theme.roles.surface_highlight;
        let band_end = |row: u16| -> Option<u16> {
            (0..buffer.area.width).rfind(|column| {
                buffer
                    .cell((*column, row))
                    .is_some_and(|cell| cell.style().bg == Some(band))
            })
        };
        let echo_text_end = (0..buffer.area.width)
            .rfind(|column| {
                buffer
                    .cell((*column, echo_row))
                    .is_some_and(|cell| cell.symbol() != " ")
            })
            .expect("the echo has no text");
        assert_eq!(
            band_end(title_row),
            band_end(echo_row),
            "the row's two lines ended in different columns:\n{}\n{}",
            line(title_row),
            line(echo_row)
        );
        assert!(
            band_end(echo_row).is_some_and(|end| end > echo_text_end),
            "the echo's band stops at the sentence instead of the row:\n{}",
            line(echo_row)
        );
    }

    /// A session row's own controls act on the row they were drawn for rather
    /// than on the row the cursor happens to be on. The pointer chose the
    /// target, and a delete button that removed a different session than the
    /// one under it is the one mistake this pair cannot make.
    #[test]
    fn a_session_rows_controls_act_on_the_row_under_the_pointer() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.live = crate::app::LiveState::Ready;
        let project = vibex_core::ProjectId::new();
        let workspace = vibex_core::WorkspaceId::new();
        let sessions = (1..=2)
            .map(|index| vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse(format!("session_action{index:04}"))
                    .expect("valid session id"),
                title: format!("actionable {index}"),
                // One workspace, so the two are rows 1 and 2 under one heading.
                project_id: project.clone(),
                workspace_id: workspace.clone(),
                workspace_root: "/tmp/vibex-action-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: index,
                updated_at_ms: index,
                last_message_at_ms: index,
                archived_at_ms: None,
                deleted_at_ms: None,
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
            })
            .collect::<Vec<_>>();
        app.agent.apply_sessions(Ok(sessions)).expect("apply");
        app.navigate_to(Page::Sessions);
        // The cursor stays on the first session; the pointer is on the second.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        app.hover = Some((crate::keymap::Scope::Sessions, 2));
        conversation_frame(&mut app, 100, 30);

        let control = |app: &App, row: usize, intent: Intent| {
            app.regions
                .row_actions
                .iter()
                .find(|control| control.row == row && control.intent == intent)
                .map(|control| control.rect)
                .unwrap_or_else(|| panic!("row {row} published no {intent:?} control"))
        };
        // What the row under the pointer is called, read from the list itself:
        // the order the sessions are drawn in is the arrangement's, not this
        // test's.
        let pointed = app.sidebar_rows()[2].label.clone();
        let rename = control(&app, 2, Intent::BeginRenameSession);
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rename.x,
                row: rename.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.selection_for(crate::keymap::Scope::Sessions),
            2,
            "the control did not move the cursor to the row it was drawn for"
        );
        match app.overlay.clone() {
            Some(Overlay::Prompt {
                field: crate::app::PromptField::RenameSession,
                value,
                ..
            }) => assert_eq!(value, pointed, "the prompt names another session"),
            other => panic!("the rename control opened {other:?}"),
        }

        // The delete button raises the same guard its chord does, against the
        // row it was drawn for.
        app.perform(Intent::Back);
        conversation_frame(&mut app, 100, 30);
        let delete = control(&app, 2, Intent::DeleteSession);
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: delete.right() - 1,
                row: delete.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        match app.overlay.clone() {
            Some(Overlay::Confirm { confirm, .. }) => {
                assert_eq!(confirm, Intent::DeleteSession)
            }
            other => panic!("the delete control opened {other:?}"),
        }
    }

    /// The keyboard's focus on a row's controls is drawn where the pointer's is:
    /// the button `←`/`→` have stepped onto is lit, and the page's own action row
    /// wears the selection band while the cursor is on it.
    #[test]
    fn the_keyboard_lights_the_control_it_has_stepped_onto() {
        use crate::action::Intent;
        use ratatui::style::Modifier;
        let mut app = test_app(100, 30);
        app.agent
            .apply_sessions(Ok(vec![vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse("session_lit000001")
                    .expect("valid session id"),
                title: "lightable".to_string(),
                project_id: vibex_core::ProjectId::new(),
                workspace_id: vibex_core::WorkspaceId::new(),
                workspace_root: "/tmp/vibex-lit-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: 1,
                updated_at_ms: 1,
                last_message_at_ms: 1,
                archived_at_ms: None,
                deleted_at_ms: None,
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
            }]))
            .expect("apply");
        app.navigate_to(Page::Sessions);
        // Row 0 is the workspace heading; the session is under it.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let draw =
            |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                terminal
                    .draw(|frame| crate::view::render(frame, app))
                    .expect("the session list draws");
            };
        draw(&mut app, &mut terminal);
        let control = |app: &App, row: usize, intent: Intent| {
            app.regions
                .row_actions
                .iter()
                .find(|control| control.row == row && control.intent == intent)
                .map(|control| control.rect)
                .unwrap_or_else(|| panic!("row {row} published no {intent:?} control"))
        };
        let bold =
            |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, column: u16, row: u16| {
                terminal
                    .backend()
                    .buffer()
                    .cell((column, row))
                    .is_some_and(|cell| cell.style().add_modifier.contains(Modifier::BOLD))
            };
        // The cursor's row draws its cluster, but no button is the reader's yet.
        let pin = control(&app, 1, Intent::PinSession);
        assert!(
            !bold(&terminal, pin.x, pin.y),
            "a button nobody had reached was lit"
        );

        // `→` steps onto the cluster, and the button it reached is lit.
        app.perform(Intent::SessionControlNext);
        draw(&mut app, &mut terminal);
        let pin = control(&app, 1, Intent::PinSession);
        assert!(
            bold(&terminal, pin.x, pin.y),
            "the button the keyboard reached is not lit"
        );

        // `↑` from the first row puts the cursor on the page's action row: it
        // wears the selection band and `+ New session` is lit, because `Enter`
        // there starts a session.
        app.set_selection(crate::keymap::Scope::Sessions, 0);
        app.perform(Intent::SelectPrevious);
        draw(&mut app, &mut terminal);
        let buffer = terminal.backend().buffer();
        let line = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let action_row = (0..buffer.area.height)
            .find(|row| line(*row).contains("New session"))
            .expect("the action row is not on screen");
        let text = line(action_row);
        let label_column = text.find("New session").expect("the label is there") as u16;
        let band = app.theme.roles.surface_highlight;
        assert_eq!(
            buffer
                .cell((label_column, action_row))
                .and_then(|cell| cell.style().bg),
            Some(band),
            "the action row does not wear the selection band:\n{text}"
        );
        assert!(
            bold(&terminal, label_column, action_row),
            "the action row did not light its first control"
        );

        // `→` moves the light to the workspace control on the right.
        app.perform(Intent::SessionControlNext);
        draw(&mut app, &mut terminal);
        let buffer = terminal.backend().buffer();
        let line = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let text = line(action_row);
        let workspace_column = text
            .find("Workspace")
            .expect("the workspace control is there") as u16;
        assert!(
            bold(&terminal, workspace_column, action_row),
            "the workspace control is not lit:\n{text}"
        );
        assert!(
            !bold(&terminal, label_column, action_row),
            "both controls are lit at once:\n{text}"
        );
    }

    /// A row's controls describe the frame that drew them: a click after the
    /// list has left the screen lands on nothing, not on the button that used
    /// to be there.
    #[test]
    fn a_row_control_rect_does_not_outlive_the_frame_that_drew_it() {
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.agent
            .apply_sessions(Ok(vec![vibex_core::AgentSession {
                id: vibex_core::VibexSessionId::parse("session_action0001")
                    .expect("valid session id"),
                title: "actionable".to_string(),
                project_id: vibex_core::ProjectId::new(),
                workspace_id: vibex_core::WorkspaceId::new(),
                workspace_root: "/tmp/vibex-action-workspace".to_string(),
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
                state: vibex_core::AgentSessionState::Idle,
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: 1,
                updated_at_ms: 1,
                last_message_at_ms: 1,
                archived_at_ms: None,
                deleted_at_ms: None,
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
            }]))
            .expect("apply");
        app.navigate_to(Page::Sessions);
        // Row 0 is the workspace heading; the session is under it.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        conversation_frame(&mut app, 100, 30);
        let control = app
            .regions
            .row_actions
            .first()
            .copied()
            .expect("the selected row published no controls");

        app.navigate_to(Page::Agent);
        conversation_frame(&mut app, 100, 30);
        assert!(
            app.regions.row_actions.is_empty(),
            "a control outlived the frame that drew it"
        );
        let before = (
            app.selection_for(crate::keymap::Scope::Sessions),
            app.overlay.clone(),
        );
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: control.rect.x,
                row: control.rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            (
                app.selection_for(crate::keymap::Scope::Sessions),
                app.overlay.clone()
            ),
            before,
            "a click on a control nobody can see still ran"
        );
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
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
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
            let _ = handle_mouse(
                &mut app,
                &worker,
                None,
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

        // The affordance answers the pointer before it answers the click: the
        // pointer resting on it is how the reader learns it can be pressed.
        assert!(!app.hovered_modal_close, "the affordance started lit");
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.hovered_modal_close,
            "the close affordance ignored the pointer"
        );
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: close.x,
                row: close.y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            !app.hovered_modal_close,
            "the affordance stayed lit after the pointer left it"
        );
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );

        // The affordance answers while the modal is up.
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.overlay.is_none(), "the close affordance did nothing");
        assert!(
            !app.hovered_modal_close,
            "the hover outlived the affordance that drew it"
        );

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
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
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

    /// The palette's list is a menu the pointer can drive: the row under the
    /// pointer takes the highlight `↑↓` paints, and a click runs it. Both go
    /// through the cursor the keys move, so the row that is lit and the command
    /// that runs cannot disagree.
    ///
    /// The frame is tall because the sections read view-first: a session command
    /// is the last group down, and a palette window that only fits the first
    /// screenful cannot show the row this test points at. It is drawn on a
    /// session's own page for the same reason: the session section is offered
    /// only where a session is on screen.
    #[test]
    fn the_palette_rows_answer_the_pointer() {
        use crate::action::Intent;
        use crate::view::PaletteGroup;
        let worker = isolation_worker();
        let mut app = test_app(100, 60);
        let session = isolation_session(vibex_core::VibexSessionId::new(), "claude");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(Page::Agent);
        app.overlay = Some(Overlay::Palette {
            query: String::new(),
            selected: 0,
        });
        conversation_frame(&mut app, 100, 60);
        let region = app
            .regions
            .palette
            .clone()
            .expect("the drawn palette published no rows");
        let entries = app.palette_entries("");
        let command = entries
            .iter()
            .position(|listing| {
                listing.entry.intent == Intent::NewSession && listing.group == PaletteGroup::Session
            })
            .expect("no session command in the palette");
        let line = region
            .rows
            .iter()
            .position(|row| *row == Some(command))
            .expect("the session command was not drawn");
        let cell = (region.rect.x + 4, region.rect.y + line as u16);

        // The pointer resting on a row moves the cursor onto it, whichever row
        // that is: the highlight follows the pointer the way the arrows do.
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: cell.0,
                row: cell.1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.overlay,
            Some(Overlay::Palette {
                query: String::new(),
                selected: command
            }),
            "the pointer did not move the palette's cursor"
        );

        // A heading names a section rather than a row, so the pointer on it
        // moves nothing and the cursor never lands on a line `Enter` cannot run.
        let heading = region
            .rows
            .iter()
            .position(Option::is_none)
            .expect("no heading was drawn");
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: region.rect.x + 2,
                row: region.rect.y + heading as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.overlay,
            Some(Overlay::Palette {
                query: String::new(),
                selected: command
            }),
            "a heading took the cursor"
        );

        // A click runs the row it landed on: the palette is a menu, and the
        // pointer has already taken the highlight on it.
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: cell.0,
                row: cell.1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.overlay.is_none(), "the click left the palette open");
        assert_eq!(
            app.page,
            Page::NewSession,
            "the click did not run the command under it"
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
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
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
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
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
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 4);
        app.perform(Intent::SelectPrevious);
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 0);
        conversation_frame(&mut app, 100, 30);
        let area = app.regions.scrollback;
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 3,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        app.transcript
            .advance_disclosures(Instant::now() + crate::transcript::DISCLOSURE_DURATION);
        let opened = conversation_frame(&mut app, 100, 30);
        assert!(opened.contains("file-3.rs"), "{opened}");
        assert!(!opened.contains("contents of file"), "{opened}");
        app.perform(Intent::SelectNext);
        assert_eq!(app.selection_for(crate::keymap::Scope::Agent), 1);
        app.perform(Intent::ToggleBlockExpanded);
        app.transcript
            .advance_disclosures(Instant::now() + crate::transcript::DISCLOSURE_DURATION);
        let details = conversation_frame(&mut app, 100, 30);
        assert!(details.contains("contents of file 1"), "{details}");
        assert!(!details.contains("contents of file 3"), "{details}");
        app.sync_transcript();
        assert!(app.transcript.blocks()[0].expanded && app.transcript.blocks()[1].expanded);
        assert!(!app.transcript.blocks()[2].expanded && !app.transcript.blocks()[3].expanded);
        conversation_frame(&mut app, 100, 30);
        let area = app.regions.scrollback;
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 3,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        app.transcript
            .advance_disclosures(Instant::now() + crate::transcript::DISCLOSURE_DURATION);
        let collapsed = conversation_frame(&mut app, 100, 30);
        assert!(!collapsed.contains("contents of file"), "{collapsed}");
        assert!(
            app.transcript.blocks()[..4]
                .iter()
                .all(|block| !block.expanded)
        );
        app.focus = Focus::Composer;
        // This session has no user message to recall, so `Up` is the draft's
        // own key and the keyboard stays in the composer. The transcript is
        // reached with `Tab`, not by an empty draft.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.focus, Focus::Composer);
        let outcome = app.perform(Intent::FocusNext);
        dispatch_all(&worker, &outcome);
        assert_eq!(app.focus, Focus::Main);
    }

    /// A settled turn hides its process behind the answer's own title. Opening
    /// the turn puts the work back on screen under the turn's title, and the
    /// group that stands for the run is drawn below it. The row that opens the
    /// group is the group's own, not the block's first line — which is the
    /// turn's title, and folds the turn instead.
    #[test]
    fn a_click_opens_a_group_head_drawn_under_its_turns_title() {
        use vibex_core::{
            AgentMessagePayload, TimelinePayload, ToolCallPayload, ToolCallStatus,
            UserMessagePayload,
        };
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        let mut items = vec![conversation_item(
            &id,
            1,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "read the files".into(),
                ..Default::default()
            }),
        )];
        for sequence in 2..=4 {
            items.push(conversation_item(
                &id,
                sequence,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: format!("tool-{sequence}"),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: String::new(),
                    input_summary: Some(format!("src/file-{sequence}.rs")),
                    output_summary: Some(format!("contents of file {sequence}")),
                    raw_extension: None,
                }),
            ));
        }
        items.push(conversation_item(
            &id,
            5,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "All three files are read.".into(),
                is_final: true,
            }),
        ));
        app.agent.state.timeline.replace_authoritative(id, items);
        app.sync_transcript();
        app.focus = Focus::Main;

        // The settled turn asks for its process shut, so the answer owns the
        // control and the group is hidden behind it.
        let head = app
            .transcript
            .blocks()
            .iter()
            .position(|block| block.group.is_head())
            .expect("the three reads are one group");
        let answer = app
            .transcript
            .blocks()
            .iter()
            .position(|block| {
                block
                    .fold
                    .as_ref()
                    .is_some_and(crate::transcript::TurnFold::is_control)
            })
            .expect("the answer owns the control");
        conversation_frame(&mut app, 100, 30);
        let area = app.regions.scrollback;
        let control = app.transcript.fold_control_lines(answer)[0];
        let offset = app.transcript.scroll_offset();
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 3,
                row: area.y + u16::try_from(control - offset).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.transcript.blocks()[head]
                .fold
                .as_ref()
                .is_some_and(|fold| fold.expanded),
            "the answer's title did not open the turn"
        );

        // Open, the turn draws its title above the group's own row: two rows
        // for the title and the air under it. The row after them is the one
        // that carries the group's disclosure.
        let opened = conversation_frame(&mut app, 100, 30);
        assert!(opened.contains("Worked for"), "{opened}");
        let header = app.transcript.line_of_block(head) + 2;
        let _ = handle_mouse(
            &mut app,
            &worker,
            None,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 3,
                row: area.y + u16::try_from(header - offset).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
        );
        app.transcript
            .advance_disclosures(Instant::now() + crate::transcript::DISCLOSURE_DURATION);
        let group = conversation_frame(&mut app, 100, 30);
        assert!(
            app.transcript.blocks()[head].expanded,
            "the group's own row did not open it: {group}"
        );
        // The members are on screen, and the turn's title is above them once:
        // the head draws it for the run, and no member repeats it.
        assert!(group.contains("file-3.rs"), "{group}");
        assert_eq!(group.matches("Worked for").count(), 2, "{group}");
    }

    #[test]
    fn a_landed_history_page_keeps_pulling_while_the_reader_stays_near_the_top() {
        use vibex_core::{TimelinePage, TimelinePayload, UserMessagePayload};
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.resize(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        let history = |sequence: i64| {
            conversation_item(
                &id,
                sequence,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: format!("history {sequence}"),
                    ..Default::default()
                }),
            )
        };
        app.agent
            .state
            .timeline
            .replace_authoritative(id.clone(), (1..=12).map(history).collect::<Vec<_>>());
        app.agent.state.timeline_has_older = true;
        app.sync_transcript();
        app.transcript_band_rows = 10;
        app.scroll.follow = false;
        app.scroll.offset = 0;
        // The page the reader is reading was asked for from the top.
        let ticket = app.agent.begin_timeline_before().expect("a first page");

        let page = TimelinePage {
            session_id: id.clone(),
            items: vec![history(0)],
            start_sequence: Some(0),
            end_sequence: Some(0),
            has_older: true,
            has_newer: true,
            starts_at_turn_start: true,
            leading_turn_start: None,
            omitted: None,
        };
        apply_message(
            &mut app,
            &worker,
            AppMessage::OlderTimeline {
                ticket,
                result: Ok(page),
            },
        )
        .unwrap();

        // The reader has not moved off the head of the new page, so the next
        // one is already on its way: one more ask is admitted and the one after
        // that is refused at the controller's limit.
        assert!(app.load_older_history().is_some());
        assert!(app.load_older_history().is_none());
    }

    #[test]
    fn expanding_a_summarized_activity_reads_its_output() {
        use crate::action::Intent;
        use vibex_core::{
            AgentEventRawExtension, AgentEventRawOutput, AgentEventRawOutputMode, TimelinePayload,
            ToolCallPayload, ToolCallStatus,
        };
        let worker = isolation_worker();
        let mut app = isolation_app();
        app.resize(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        let payload = ToolCallPayload {
            tool_call_id: "call-1".into(),
            tool_name: "execute".into(),
            status: ToolCallStatus::Completed,
            summary: "ls -la".into(),
            input_summary: Some(r#"{"command":"ls -la"}"#.into()),
            output_summary: Some("terminal output".into()),
            raw_extension: None,
        };
        // A page carries the activity summarized, which is what makes opening
        // it a read rather than a redraw.
        let mut summary = conversation_item(&id, 1, TimelinePayload::ToolCall(payload.clone()));
        summary.payload_depth = vibex_core::TimelinePayloadDepth::Summary;
        app.agent
            .state
            .timeline
            .replace_authoritative(id.clone(), vec![summary.clone()]);
        app.sync_transcript();
        app.focus = Focus::Main;
        app.set_selection(crate::keymap::Scope::Agent, 0);

        let outcome = app.perform(Intent::ToggleBlockExpanded);
        let [crate::app::Effect::LoadTimelineItem { ticket }] = outcome.effects.as_slice() else {
            panic!("expected one detail read, got {:?}", outcome.effects);
        };
        assert_eq!(ticket.sequence, 1);
        // The summary is on screen while the read is out: opening an activity
        // never leaves it blank.
        let pending = conversation_frame(&mut app, 100, 30);
        assert!(pending.contains("ls -la"), "{pending}");

        // A rebuild while the read is on its way does not pay for it twice.
        app.sync_transcript();
        assert!(app.take_timeline_detail_effects().is_empty());

        let mut full = summary.clone();
        full.payload_depth = vibex_core::TimelinePayloadDepth::Detail;
        full.payload = TimelinePayload::ToolCall(ToolCallPayload {
            raw_extension: Some(AgentEventRawExtension::new(
                Vec::new(),
                Some(r#"{"command":"ls -la"}"#.into()),
                Some(
                    AgentEventRawOutput::new(
                        AgentEventRawOutputMode::Snapshot,
                        r#"{"formatted_output":"total 4\nsrc\n","exit_code":0}"#,
                    )
                    .0,
                ),
                Vec::new(),
                Default::default(),
                false,
            )),
            ..payload
        });
        apply_message(
            &mut app,
            &worker,
            AppMessage::TimelineItem {
                ticket: ticket.clone(),
                result: Ok(full),
            },
        )
        .unwrap();
        let loaded = conversation_frame(&mut app, 100, 30);
        assert!(loaded.contains("total 4"), "{loaded}");

        // The payload is complete now, so reopening the activity asks for
        // nothing.
        app.perform(Intent::ToggleBlockExpanded);
        assert!(app.perform(Intent::ToggleBlockExpanded).effects.is_empty());
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
            assert_eq!(app.history_log.entries().len(), 2);
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

    /// A click in the catalogue takes the row the frame drew under the pointer,
    /// and the lines that are not rows take nothing at all.
    ///
    /// The list is not all rows: a rule parts the pinned sections from the
    /// Agents, and a line of air parts one Agent's group from the next. A hit
    /// test that counted lines as rows would put the cursor on the Agent above
    /// the one the reader pointed at, and a click on the air would move it,
    /// which no list here does.
    #[test]
    fn a_click_on_the_catalogue_skips_the_lines_that_are_not_rows() {
        use crate::action::Intent;
        use crate::app::RuntimePickerView;
        use crate::runtime_picker::RuntimePickerRow;
        let worker = isolation_worker();
        let mut app = isolation_app();
        // The pinned row is what puts the rule on screen, and two open Agents
        // are what put air between two groups.
        let recent = app.runtime_options.as_ref().unwrap().options[0]
            .selection
            .clone();
        app.remember_runtime_selection(&recent);
        app.perform(Intent::NewSession);
        app.show_runtime_picker();
        app.fold_runtime_group(
            &vibex_core::AgentId::parse("deepseek-harness").unwrap(),
            false,
        );
        conversation_frame(&mut app, 120, 40);

        let region = app
            .regions
            .runtime_picker
            .as_ref()
            .expect("the picker published its rows")
            .clone();
        // The line the last Agent's heading landed on, and the lines the frame
        // drew no row on. Such a line only counts when the line above it is a
        // row, so the empty space below the list is not one.
        let rows = app.runtime_picker_rows();
        let heading = rows
            .iter()
            .rposition(|row| matches!(row, RuntimePickerRow::Agent { .. }))
            .expect("the catalogue has two Agents");
        let heading_line = region
            .rows
            .iter()
            .position(|row| *row == Some(heading))
            .expect("the heading is in the window");
        let gaps: Vec<usize> = region
            .rows
            .iter()
            .enumerate()
            .filter(|(line, row)| row.is_none() && *line > 0 && region.rows[line - 1].is_some())
            .map(|(line, _)| line)
            .collect();
        assert_eq!(
            gaps.len(),
            2,
            "the rule and the air are not both drawn: {gaps:?}"
        );

        let selected = |app: &App| match &app.overlay {
            Some(Overlay::RuntimePicker { selected, .. }) => *selected,
            other => panic!("the picker is not up: {other:?}"),
        };
        let click = |app: &mut App, line: usize| {
            let _ = handle_mouse(
                app,
                &worker,
                None,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: region.rect.x,
                    row: region.rect.y + u16::try_from(line).expect("a line in the window"),
                    modifiers: KeyModifiers::NONE,
                },
            );
        };

        click(&mut app, heading_line);
        assert_eq!(
            selected(&app),
            heading,
            "the click did not take the heading under the pointer"
        );
        for line in gaps {
            click(&mut app, line);
            assert_eq!(
                selected(&app),
                heading,
                "a click on the line {line} moved the cursor off the row it was on"
            );
        }
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: heading,
            }),
            "a click on a line that is not a row changed the view"
        );
    }

    #[test]
    fn a_landed_catalogue_ends_the_loading_line() {
        use crate::action::Intent;
        let worker = isolation_worker();
        let mut app = isolation_app();
        let catalog = app.runtime_options.take().unwrap();

        // The page has asked and the answer is still on its way: the Agent
        // setup line says it is loading rather than that the runtime is
        // unavailable.
        app.perform(Intent::NewSession);
        assert!(app.runtime_options_reading);
        assert_eq!(app.composer_runtime_labels().0, app.strings.loading());

        // The answer names the Agent the next creation will use, and the page
        // stops saying it is waiting.
        apply_message(&mut app, &worker, AppMessage::RuntimeOptions(Ok(catalog))).unwrap();
        assert!(!app.runtime_options_reading);
        assert_eq!(app.composer_runtime_labels().0, "codex");

        // A read that fails is not a read that is still coming: the page stops
        // loading and says the runtime is unavailable.
        app.runtime_options = None;
        app.perform(Intent::NewSession);
        assert!(app.runtime_options_reading);
        apply_message(
            &mut app,
            &worker,
            AppMessage::RuntimeOptions(Err(vibex_backend::BackendError::failed(
                "test",
                "catalogue failed",
            ))),
        )
        .unwrap();
        assert!(!app.runtime_options_reading);
        assert_eq!(
            app.composer_runtime_labels().0,
            app.strings.runtime_unavailable()
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
                wanted: crate::app::ClipboardWanted::Everything,
                content: crate::worker::ClipboardContent::Text("stale paste".into()),
            },
            AppMessage::Pasted {
                ticket: ticket.clone(),
                wanted: crate::app::ClipboardWanted::Image,
                content: crate::worker::ClipboardContent::Image {
                    mime_type: "image/png".into(),
                    bytes: vec![1, 2, 3],
                },
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
                wanted: crate::app::ClipboardWanted::Everything,
                content: crate::worker::ClipboardContent::Text(" accepted".into()),
            },
        )
        .unwrap();
        assert_eq!(app.composer.text(), "draft after edit accepted");
    }

    #[test]
    fn the_completion_drawer_is_painted_on_the_theme_surface() {
        // Regression: the drawer cleared its area and let the terminal paint it,
        // so the command list sat on a background of its own — a different
        // colour from the composer it is pulled out of, and the one surface a
        // theme cannot choose for itself.
        let mut app = isolation_app();
        app.resize(100, 30);
        app.perform(crate::action::Intent::NewSession);
        app.focus = crate::app::Focus::Composer;
        app.composer.set_text("testest /com");
        app.refresh_completion();
        {
            let menu = app.completion.as_mut().expect("a completion menu");
            menu.lists.commands = vec![crate::composer::Completion {
                insert: "/compact ".to_string(),
                label: "compact".to_string(),
                detail: "Compact the conversation".to_string(),
                group: "Command".to_string(),
            }];
            menu.loading = false;
            menu.reseat(menu.query.clone());
        }
        let surface = app.theme.roles.surface_raised;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, &mut app))
            .expect("the drawer draws");
        let buffer = terminal.backend().buffer();
        let row_text = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let drawer = (0..buffer.area.height)
            .find(|row| row_text(*row).contains("compact"))
            .expect("the drawer is not on screen");
        for column in [0, buffer.area.width / 2, buffer.area.width - 1] {
            assert_eq!(
                buffer.cell((column, drawer)).map(|cell| cell.style().bg),
                Some(Some(surface)),
                "column {column} of the drawer is not on the theme's surface"
            );
        }
    }

    #[test]
    fn the_drawer_names_its_panels_and_marks_the_one_showing() {
        // The panels are the desktop popup's, and a reader who cannot see which
        // one is lit has no way to know what `←`/`→` would change. The active
        // name wears the accent fill the other tab strips use, and the trigger's
        // own sigil closes the row.
        let mut app = isolation_app();
        app.resize(100, 30);
        app.perform(crate::action::Intent::NewSession);
        app.focus = crate::app::Focus::Composer;
        app.composer.set_text("testest /");
        app.refresh_completion();
        {
            let menu = app.completion.as_mut().expect("a completion menu");
            let row = |label: &str| crate::composer::Completion {
                insert: format!("{label} "),
                label: label.to_string(),
                detail: String::new(),
                group: "Command".to_string(),
            };
            menu.lists.commands = vec![row("/compact")];
            menu.lists.phrases = vec![row("/review")];
            menu.loading = false;
            menu.reseat(menu.query.clone());
        }
        let accent = app.theme.roles.accent_user;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, &mut app))
            .expect("the drawer draws");
        let buffer = terminal.backend().buffer();
        let row_text = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let header = (0..buffer.area.height)
            .find(|row| {
                let text = row_text(*row);
                text.contains("Commands") && text.contains("Quick phrases")
            })
            .expect("the panel header is not on screen");
        let header_text = row_text(header);
        assert!(header_text.contains('/'), "the trigger sigil is missing");
        // A terminal that cannot paint the accent keeps `BOLD` as the mark, so
        // that is what the test reads; where the palette can paint, the fill is
        // the accent the other tab strips use.
        let marked = |name: &str| {
            let column = header_text.find(name).expect("the panel name") as u16;
            let cell = buffer.cell((column, header)).expect("the panel's own cell");
            (
                cell.style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD),
                cell.style().bg,
            )
        };
        let (bold, fill) = marked("Commands");
        assert!(bold, "the panel showing wears no mark");
        if accent != ratatui::style::Color::Reset {
            assert_eq!(fill, Some(accent), "the panel showing wears no accent fill");
        }
        let (bold, _) = marked("Quick phrases");
        assert!(!bold, "a panel that is not showing wears the active mark");

        // Switching panels moves the mark with the rows.
        assert!(app.select_completion_tab(crate::composer::CompletionTab::Phrases));
        terminal
            .draw(|frame| crate::view::render(frame, &mut app))
            .expect("the drawer draws");
        let buffer = terminal.backend().buffer();
        let row_text = |row: u16| -> String {
            (0..buffer.area.width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol())
                        .unwrap_or(" ")
                })
                .collect()
        };
        let header = (0..buffer.area.height)
            .find(|row| {
                let text = row_text(*row);
                text.contains("Commands") && text.contains("Quick phrases")
            })
            .expect("the panel header is not on screen");
        let header_text = row_text(header);
        let marked = |name: &str| {
            let column = header_text.find(name).expect("the panel name") as u16;
            buffer.cell((column, header)).is_some_and(|cell| {
                cell.style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
            })
        };
        assert!(marked("Quick phrases"));
        assert!(!marked("Commands"));
        assert!(
            (0..buffer.area.height).any(|row| row_text(row).contains("/review")),
            "the panel's own rows are not drawn"
        );
    }

    #[test]
    fn the_completion_drawer_hides_the_transcript_it_covers() {
        // Regression: the drawer painted its surface with a style, and a style
        // does not erase the glyphs already in the cells it patches. The
        // transcript underneath stayed readable through every gap between the
        // rows, so the drawer looked like it was drawn on glass.
        let mut app = isolation_app();
        app.resize(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        app.agent.state.timeline.replace_authoritative(
            id.clone(),
            (1..=24)
                .map(|sequence| {
                    // Tall enough to overflow the band: a transcript that fits
                    // its rows is drawn from the top and never reaches the
                    // drawer, which hangs off the bottom of the band.
                    let text = format!("BLEEDMARKER {sequence} ").repeat(6);
                    let payload = if sequence % 2 == 0 {
                        vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                            text,
                            ..Default::default()
                        })
                    } else {
                        vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                            text,
                            is_final: true,
                        })
                    };
                    conversation_item(&id, sequence, payload)
                })
                .collect::<Vec<_>>(),
        );
        app.sync_transcript();
        app.focus = crate::app::Focus::Composer;

        let rows = |app: &mut App| -> Vec<String> {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| crate::view::render(frame, app))
                .expect("the frame draws");
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|row| {
                    (0..buffer.area.width)
                        .map(|column| {
                            buffer
                                .cell((column, row))
                                .map(|cell| cell.symbol())
                                .unwrap_or(" ")
                        })
                        .collect::<String>()
                })
                .collect()
        };

        // The drawer hangs off the composer band, which the drawer does not
        // move: the rows it covers are known before it is opened.
        let covered = rows(&mut app);
        let band = app
            .regions
            .composer_band
            .expect("the composer band is drawn");
        app.composer.set_text("/com");
        app.refresh_completion();
        {
            let menu = app.completion.as_mut().expect("a completion menu");
            menu.lists.commands = (0..5)
                .map(|index| crate::composer::Completion {
                    insert: format!("/command{index} "),
                    label: format!("command{index}"),
                    detail: "a command".to_string(),
                    group: "Command".to_string(),
                })
                .collect();
            menu.loading = false;
            menu.reseat(menu.query.clone());
        }
        let height = app
            .completion
            .as_ref()
            .expect("a completion menu")
            .visible()
            .len() as u16
            + 2;
        let covered_rows = ((band.y - height)..band.y).collect::<Vec<_>>();
        assert!(
            covered_rows
                .iter()
                .any(|row| covered[*row as usize].contains("BLEEDMARKER")),
            "the transcript is not behind the drawer at rows {covered_rows:?} (band {band:?})"
        );

        let open = rows(&mut app);
        for row in covered_rows {
            assert!(
                !open[row as usize].contains("BLEEDMARKER"),
                "the transcript shows through the drawer at row {row}: {:?}",
                open[row as usize]
            );
        }
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
    fn a_completion_request_names_the_workspace_the_draft_belongs_to() {
        // `@` lists the workspace's files, so the request has to say which
        // workspace — and the composing page has no session to read one from.
        // The directory that page will create the session in is the answer, and
        // the workspace records this client has read turn it into an id.
        let mut app = isolation_app();
        app.resize(100, 30);
        app.perform(crate::action::Intent::NewSession);
        let project_id = vibex_core::ProjectId::new();
        let workspace_id = vibex_core::WorkspaceId::new();
        app.workspace_rows = vec![vibex_backend::WorkspaceSummary {
            project: vibex_core::ProjectRecord {
                id: project_id.clone(),
                name: "repo".into(),
                root_path: "/repo".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            workspace: vibex_core::WorkspaceRecord {
                id: workspace_id.clone(),
                project_id,
                root_path: "/repo".into(),
                mode: vibex_core::WorkspaceMode::CurrentCheckout,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            git_branch: None,
        }];
        app.workspace_path = Some("/repo".to_string());
        assert_eq!(app.composer_workspace_id(), Some(workspace_id.clone()));
        assert_eq!(
            app.composer_ticket().workspace_id,
            Some(workspace_id),
            "the request the worker builds would name no workspace"
        );

        // A directory the runtime has no record for names no workspace rather
        // than the wrong one: a completion request without one answers without
        // files, which is a list that is empty rather than a list of another
        // project's.
        app.workspace_path = Some("/elsewhere".to_string());
        assert_eq!(app.composer_workspace_id(), None);
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
                payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
    fn a_collaborator_row_names_its_account_in_the_readers_language() {
        // The row's second line is the runtime's own description — the account
        // and the model — and the account is the one name this client says for
        // itself, so a reader of a locale does not read the runtime's English.
        let worker = isolation_worker();
        let mut app = test_app(120, 32);
        app.strings = crate::locale::Strings::for_locale(crate::locale::Locale::ZhCn);
        app.composer.set_text("@");
        app.refresh_completion();
        let ticket = app.composer_ticket();
        let discovery = vibex_core::AgentCommandDiscovery {
            response: vibex_core::AgentCommandDiscoverResponse {
                entries: vec![vibex_core::AgentCommandEntry {
                    id: "reference:agent:codex:work".to_string(),
                    trigger: vibex_core::AgentCommandTrigger::Mention,
                    source_kind: vibex_core::AgentCommandSourceKind::Reference,
                    label: "@Codex".to_string(),
                    description: Some("Agent account - you@example.com · gpt-5".to_string()),
                    insertion_text: "@Codex ".to_string(),
                    command_name: None,
                    provider_kind: None,
                    prompt_id: None,
                    skill_id: None,
                    reference_path: None,
                    selection_behavior: vibex_core::AgentCommandSelectionBehavior::Insert,
                    execution_behavior: vibex_core::AgentCommandExecutionBehavior::None,
                    destructive: false,
                    metadata: Vec::new(),
                }],
                diagnostics: Vec::new(),
            },
            slash_commands: true,
            skills: true,
            quick_phrases: Vec::new(),
            delegation_entries: Vec::new(),
        };
        apply_message(
            &mut app,
            &worker,
            AppMessage::Completions {
                ticket,
                result: Ok(Box::new(discovery)),
            },
        )
        .unwrap();
        let items = &app.completion.as_ref().expect("a completion menu").items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail, "Agent 账户 - you@example.com · gpt-5");
        // The reference is a workspace file's panel sibling: an `@` answer fills
        // the files and the delegation panels, not the commands one.
        assert_eq!(
            app.completion.as_ref().unwrap().active_tab(),
            crate::composer::CompletionTab::Files
        );
    }

    /// One discovery entry, as the authority would hand it over.
    fn discovered_entry(
        label: &str,
        source_kind: vibex_core::AgentCommandSourceKind,
    ) -> vibex_core::AgentCommandEntry {
        vibex_core::AgentCommandEntry {
            id: format!("test:{label}"),
            trigger: vibex_core::AgentCommandTrigger::Slash,
            source_kind,
            label: label.to_string(),
            description: Some(format!("{label} description")),
            insertion_text: format!("{label} "),
            command_name: None,
            provider_kind: None,
            prompt_id: None,
            skill_id: None,
            reference_path: None,
            selection_behavior: vibex_core::AgentCommandSelectionBehavior::Insert,
            execution_behavior: vibex_core::AgentCommandExecutionBehavior::None,
            destructive: false,
            metadata: Vec::new(),
        }
    }

    /// Answer the open drawer with the two `/` panels, as one discovery does.
    fn answer_slash_discovery(app: &mut App, worker: &Worker) {
        use vibex_core::AgentCommandSourceKind;
        let ticket = app.composer_ticket();
        let discovery = vibex_core::AgentCommandDiscovery {
            response: vibex_core::AgentCommandDiscoverResponse {
                entries: vec![discovered_entry(
                    "/compact",
                    AgentCommandSourceKind::Provider,
                )],
                diagnostics: Vec::new(),
            },
            slash_commands: true,
            skills: true,
            quick_phrases: vec![discovered_entry("/review", AgentCommandSourceKind::Prompt)],
            delegation_entries: Vec::new(),
        };
        apply_message(
            app,
            worker,
            AppMessage::Completions {
                ticket,
                result: Ok(Box::new(discovery)),
            },
        )
        .unwrap();
    }

    #[test]
    fn the_drawer_opens_the_desktops_panels_and_the_arrow_keys_switch_them() {
        // The `/` drawer is the desktop popup's: a panel of commands and a panel
        // of reusable Prompts, both from the one discovery answer, with `←`/`→`
        // moving between them. The panels used to be flattened into a single
        // list, so the reader could not reach the Prompts at all once the
        // commands filled the bound.
        let worker = isolation_worker();
        let mut app = test_app(120, 32);
        app.composer.set_text("/ab");
        app.refresh_completion();
        answer_slash_discovery(&mut app, &worker);

        let menu = app.completion.as_ref().expect("a completion menu");
        assert_eq!(menu.active_tab(), crate::composer::CompletionTab::Commands);
        assert_eq!(menu.items()[0].label, "/compact");

        // `Right` switches to the phrases panel, which the same answer filled.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        )
        .unwrap();
        let menu = app.completion.as_ref().expect("a completion menu");
        assert_eq!(menu.active_tab(), crate::composer::CompletionTab::Phrases);
        assert_eq!(menu.items()[0].label, "/review");
        // The key was the drawer's, so the caret never moved: the next character
        // lands at the end of the draft.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "/abX");

        // `Left` walks back to the first panel.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(
            app.completion.as_ref().unwrap().active_tab(),
            crate::composer::CompletionTab::Commands
        );
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "/abXY");

        // At the first panel `Left` is not the drawer's key: the caret keeps its
        // usual movement, which the next character proves by landing before the
        // one it stepped over.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        )
        .unwrap();
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "/abXZY");
    }

    #[test]
    fn a_skills_drawer_keeps_its_single_panel_and_its_plain_arrows() {
        // `$` has one Skills panel, so `←`/`→` stay the caret's keys there and
        // the drawer draws no header to switch.
        let worker = isolation_worker();
        let mut app = test_app(120, 32);
        app.composer.set_text("$ab");
        app.refresh_completion();
        let ticket = app.composer_ticket();
        let discovery = vibex_core::AgentCommandDiscovery {
            response: vibex_core::AgentCommandDiscoverResponse {
                entries: vec![discovered_entry(
                    "$deploy",
                    vibex_core::AgentCommandSourceKind::Skill,
                )],
                diagnostics: Vec::new(),
            },
            slash_commands: true,
            skills: true,
            quick_phrases: Vec::new(),
            delegation_entries: Vec::new(),
        };
        apply_message(
            &mut app,
            &worker,
            AppMessage::Completions {
                ticket,
                result: Ok(Box::new(discovery)),
            },
        )
        .unwrap();
        let menu = app.completion.as_ref().expect("a completion menu");
        assert_eq!(menu.active_tab(), crate::composer::CompletionTab::Commands);
        assert!(!menu.tabs_available());
        assert_eq!(menu.items()[0].label, "$deploy");

        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        )
        .unwrap();
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "$abX");
        assert_eq!(
            app.completion.as_ref().unwrap().active_tab(),
            crate::composer::CompletionTab::Commands
        );
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

    #[test]
    fn a_completed_write_stops_saying_it_is_working() {
        // The row wears "working" while the write is in flight. Only the
        // write's own answer can take it off again, and nothing else said the
        // write was over: the marker outlived the change it described.
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = test_app(120, 32);
        app.config_center.begin_pending(
            "mcp:filesystem",
            "Filesystem",
            crate::config_center::ConfigVerb::Delete,
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Mutation {
                key: "mcp_delete".to_string(),
                record: Some("mcp:filesystem".to_string()),
                result: Ok(crate::app::MutationOutcome::Plain),
            },
        )
        .unwrap();
        assert!(!app.config_center.is_pending("mcp:filesystem"));

        // A refusal is an answer too: the reader is told why, and the row is
        // not left spinning over a record that did not change.
        let mut app = test_app(120, 32);
        app.config_center.begin_pending(
            "mcp:filesystem",
            "Filesystem",
            crate::config_center::ConfigVerb::Delete,
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Mutation {
                key: "mcp_delete".to_string(),
                record: Some("mcp:filesystem".to_string()),
                result: Err(vibex_backend::BackendError::failed("test", "refused")),
            },
        )
        .unwrap();
        assert!(!app.config_center.is_pending("mcp:filesystem"));
        assert!(app.toast.is_some(), "the refusal was silent");
    }

    /// Mark one record as working, the way a verb does before it is sent.
    fn working(app: &mut App, key: &str, subject: &str, verb: crate::config_center::ConfigVerb) {
        assert!(
            app.config_center.begin_pending(key, subject, verb),
            "{key} was already working"
        );
    }

    /// Hand the client one write's answer, filed under `record`.
    fn answer(worker: &Worker, app: &mut App, record: &str, outcome: crate::app::MutationOutcome) {
        apply_message(
            app,
            worker,
            AppMessage::Mutation {
                key: "test".to_string(),
                record: Some(record.to_string()),
                result: Ok(outcome),
            },
        )
        .unwrap();
    }

    #[test]
    fn an_answer_names_the_record_and_the_verb_it_was_about() {
        // The reader pressed "Install" on the Codex row and looked away. The
        // answer has to say what happened to what: the effect key the write
        // travelled under is not a thing a reader has ever seen, and "done" on
        // its own leaves them asking whether the key registered at all.
        use crate::app::MutationOutcome as Outcome;
        use crate::config_center::ConfigVerb as Verb;
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = test_app(120, 32);
        working(&mut app, "codex", "Codex", Verb::Install);
        answer(
            &worker,
            &mut app,
            "codex",
            Outcome::Installed {
                version: Some("1.3.0".to_string()),
            },
        );

        let toast = app.toast.as_ref().expect("the install said nothing");
        assert_eq!(toast.text, "Codex: Installed · version 1.3.0");
        assert_eq!(toast.tone, crate::app::ToastTone::Success);
        // The toast expires; the line under the table does not, so the answer
        // outlives the seconds a reader might have been looking elsewhere.
        let (notice, _) = app.config_center.notice.as_ref().expect("no notice");
        assert_eq!(notice, "Codex: Installed · version 1.3.0");
        assert!(!app.config_center.is_pending("codex"));
    }

    #[test]
    fn a_check_for_updates_says_which_of_the_two_answers_it_found() {
        // "Check update" exists to tell "up to date" from "a version is
        // waiting". A toast that only said "checked" would leave the reader
        // exactly where they were before they pressed it.
        use crate::app::MutationOutcome as Outcome;
        use crate::config_center::ConfigVerb as Verb;
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();

        let mut app = test_app(120, 32);
        working(&mut app, "codex", "Codex", Verb::CheckUpdate);
        answer(&worker, &mut app, "codex", Outcome::UpToDate);
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("Codex: Update checked · up to date")
        );

        let mut app = test_app(120, 32);
        working(&mut app, "codex", "Codex", Verb::CheckUpdate);
        answer(
            &worker,
            &mut app,
            "codex",
            Outcome::UpdateAvailable {
                version: "1.3.0".to_string(),
            },
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("Codex: Update checked · version 1.3.0 available")
        );
    }

    #[test]
    fn an_agent_account_read_says_who_is_signed_in() {
        // Signing in is the runtime's to do; the client's half is to report
        // what the runtime found, so the verb does not look like it did
        // nothing on an Agent nobody has signed into yet.
        use crate::app::MutationOutcome as Outcome;
        use crate::config_center::ConfigVerb as Verb;
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();

        let mut app = test_app(120, 32);
        working(&mut app, "codex", "Codex", Verb::SignIn);
        answer(
            &worker,
            &mut app,
            "codex",
            Outcome::Auth {
                methods: 2,
                account: Some("bai@example.com".to_string()),
                authenticated: true,
            },
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("Codex: Sign-in methods read · bai@example.com")
        );

        // Nobody is signed in: how many ways in there are is the useful half,
        // because it says the verb found something to offer.
        let mut app = test_app(120, 32);
        working(&mut app, "codex", "Codex", Verb::SignIn);
        answer(
            &worker,
            &mut app,
            "codex",
            Outcome::Auth {
                methods: 2,
                account: None,
                authenticated: false,
            },
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("Codex: Sign-in methods read · not signed in · 2 sign-in methods")
        );
    }

    #[test]
    fn a_validation_reports_its_verdict_and_not_only_that_it_ran() {
        // Three answers, three tones. A warning is neither a clean pass nor a
        // failure, and drawing it as either is the client telling the reader
        // something the authority did not say.
        use crate::app::MutationOutcome as Outcome;
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();

        let cases = [
            (
                Outcome::Verdict {
                    passed: true,
                    warned: false,
                    message: "mcp_ok · reachable".to_string(),
                },
                crate::app::ToastTone::Success,
                "Filesystem: Validated · ok · mcp_ok · reachable",
            ),
            (
                Outcome::Verdict {
                    passed: true,
                    warned: true,
                    message: "mcp_slow · two seconds".to_string(),
                },
                crate::app::ToastTone::Warning,
                "Filesystem: Validated · warning · mcp_slow · two seconds",
            ),
            (
                Outcome::Verdict {
                    passed: false,
                    warned: false,
                    message: "mcp_bad · no command".to_string(),
                },
                crate::app::ToastTone::Danger,
                "Filesystem: Validated · failed · mcp_bad · no command",
            ),
        ];
        for (outcome, tone, expected) in cases {
            let mut app = test_app(120, 32);
            working(
                &mut app,
                "mcp:filesystem",
                "Filesystem",
                crate::config_center::ConfigVerb::Validate,
            );
            answer(&worker, &mut app, "mcp:filesystem", outcome.clone());
            let toast = app.toast.as_ref().expect("the validation said nothing");
            assert_eq!(toast.text, expected, "wrong sentence for {outcome:?}");
            assert_eq!(toast.tone, tone, "wrong tone for {outcome:?}");
        }
    }

    #[test]
    fn a_refusal_names_what_did_not_happen() {
        // "install_agent: connection lost" tells a reader which row the key was
        // on only if they remember the key. The record they pressed it on and
        // the verb they asked for are what the sentence is made of.
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = test_app(120, 32);
        working(
            &mut app,
            "codex",
            "Codex",
            crate::config_center::ConfigVerb::Install,
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Mutation {
                key: "install_agent".to_string(),
                record: Some("codex".to_string()),
                result: Err(vibex_backend::BackendError::failed("test", "refused")),
            },
        )
        .unwrap();

        let toast = app.toast.as_ref().expect("the refusal was silent");
        assert_eq!(toast.text, "Codex: Install · failed: refused");
        assert_eq!(toast.tone, crate::app::ToastTone::Danger);
        assert_eq!(
            app.config_center
                .notice
                .as_ref()
                .map(|(text, _)| text.as_str()),
            Some("Codex: Install · failed: refused")
        );
        assert!(!app.config_center.is_pending("codex"));
    }

    #[test]
    fn a_form_closes_on_its_answer_and_stays_open_on_a_refusal() {
        // A form exists to make one write, so the answer is what closes it: the
        // record it produced is on the table behind it. A refusal is not an
        // answer to that write — the fix is in the form's own fields.
        use crate::app::MutationOutcome as Outcome;
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();

        let mut app = test_app(120, 32);
        app.overlay = Some(Overlay::Form {
            form: crate::config_center::mcp_form(None, app.strings),
        });
        working(
            &mut app,
            crate::config_center::FORM_RECORD,
            "New server",
            crate::config_center::ConfigVerb::Create,
        );
        answer(
            &worker,
            &mut app,
            crate::config_center::FORM_RECORD,
            Outcome::Plain,
        );
        assert!(
            app.overlay.is_none(),
            "the form stayed open over the record it had just made"
        );

        let mut app = test_app(120, 32);
        app.overlay = Some(Overlay::Form {
            form: crate::config_center::mcp_form(None, app.strings),
        });
        working(
            &mut app,
            crate::config_center::FORM_RECORD,
            "New server",
            crate::config_center::ConfigVerb::Create,
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Mutation {
                key: "mcp_create".to_string(),
                record: Some(crate::config_center::FORM_RECORD.to_string()),
                result: Err(vibex_backend::BackendError::failed(
                    "test",
                    "name is required",
                )),
            },
        )
        .unwrap();
        assert!(
            matches!(app.overlay, Some(Overlay::Form { .. })),
            "a refusal closed the form the fix belongs in"
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some("New server: Create · failed: name is required")
        );
    }

    #[test]
    fn a_completed_write_asks_the_authority_to_say_what_changed() {
        // The write's answer carries an acknowledgement rather than the record
        // it produced, so the client has to read the area again. The read is
        // what the worker is handed here; its answer is what the next frame
        // draws instead of the record from before the write.
        let (worker, mut messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = test_app(120, 32);
        app.show_config_module(
            crate::config_center::ConfigModule::Agent,
            Some(crate::config_center::ConfigSection::Agents),
        );
        apply_message(
            &mut app,
            &worker,
            AppMessage::Mutation {
                key: "uninstall_agent".to_string(),
                record: None,
                result: Ok(crate::app::MutationOutcome::Plain),
            },
        )
        .unwrap();

        // The roster read is asked for even though the backend behind it has
        // nothing to say: the request, not its answer, is what was missing.
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let read = observer.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match messages.recv().await {
                        Some(AppMessage::Agents(_)) => break,
                        Some(_) => continue,
                        None => panic!("the worker stopped"),
                    }
                }
            })
            .await
        });
        assert!(
            read.is_ok(),
            "the completed write did not re-read the area it changed"
        );
    }

    /// One ownership node for the delegation tests below.
    fn ownership_node(
        id: &str,
        parent: Option<&str>,
        child_count: usize,
    ) -> vibex_core::SessionTreeNode {
        vibex_core::SessionTreeNode {
            session_ref: vibex_core::VibexUseRef::session(
                &vibex_core::VibexSessionId::parse(id).unwrap(),
            ),
            parent_session_ref: parent.map(|parent| {
                vibex_core::VibexUseRef::session(
                    &vibex_core::VibexSessionId::parse(parent).unwrap(),
                )
            }),
            title: id.to_string(),
            agent_id: None,
            agent_label: None,
            task_ref: None,
            task_title: None,
            task_phase: None,
            completion_policy: None,
            child_count,
            has_more_children: false,
            blocked_on: None,
            active_descendants: 0,
            blocked_descendants: 0,
            current_task_ref: None,
            updated_at_ms: 1,
        }
    }

    fn ownership_page(
        nodes: Vec<vibex_core::SessionTreeNode>,
        registry: Vec<vibex_core::AgentSession>,
    ) -> vibex_core::SessionTreePage {
        vibex_core::SessionTreePage {
            nodes,
            ancestors: Vec::new(),
            registry,
            next_cursor: None,
            has_more: false,
        }
    }

    /// The ownership tree is what makes a delegated session reachable at all:
    /// the root list omits it, so the tree names it, the page has to be able to
    /// say which session owns the open one, and both directions have to open
    /// the other session.
    #[test]
    fn the_ownership_tree_opens_the_main_session_and_its_workers() {
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        let main_id = vibex_core::VibexSessionId::parse("session_main00001").unwrap();
        let sub_id = vibex_core::VibexSessionId::parse("session_sub000001").unwrap();
        let mut main = isolation_session(main_id.clone(), "codex");
        main.title = "the main session".into();
        let mut sub = isolation_session(sub_id.clone(), "codex");
        sub.title = "the worker".into();
        app.agent.apply_sessions(Ok(vec![main.clone()])).unwrap();
        app.agent.state.selected_session_id = Some(sub_id.clone());

        // The sub-session's own page: no children of its own, and the path back
        // up to the session that delegated it.
        let mut page = ownership_page(Vec::new(), vec![main.clone(), sub.clone()]);
        page.ancestors = vec![ownership_node("session_main00001", None, 1)];
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionTree {
                parent_session_id: Some(sub_id.clone()),
                append: false,
                result: Ok(page),
            },
        )
        .unwrap();
        assert_eq!(app.parent_session_id(), Some(main_id.clone()));

        // The key that goes back up opens the session that delegated this one.
        let outcome = app.perform(crate::action::Intent::GotoParentSession);
        assert!(
            outcome.effects.iter().any(|effect| matches!(
                effect,
                crate::app::Effect::OpenSession { session_id, .. } if session_id == &main_id
            )),
            "the way back opened something else: {:?}",
            outcome.effects
        );

        // A root session has no way up, and its page answers none.
        app.agent.state.selected_session_id = Some(main_id.clone());
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionTree {
                parent_session_id: Some(main_id.clone()),
                append: false,
                result: Ok(ownership_page(Vec::new(), vec![main.clone()])),
            },
        )
        .unwrap();
        assert_eq!(app.parent_session_id(), None);

        // The main session's page names the worker it delegated, and the key
        // opens it.
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionTree {
                parent_session_id: Some(main_id.clone()),
                append: false,
                result: Ok(ownership_page(
                    vec![ownership_node(
                        "session_sub000001",
                        Some("session_main00001"),
                        0,
                    )],
                    vec![main.clone(), sub.clone()],
                )),
            },
        )
        .unwrap();
        assert_eq!(app.children.child_ids(main_id.as_str()).len(), 1);
        assert!(
            app.sidebar_rows()
                .iter()
                .any(|row| row.delegated && row.session_id.as_ref() == Some(&sub_id)),
            "the delegated session has no nested row"
        );
        let outcome = app.perform(crate::action::Intent::OpenChildSessions);
        assert!(
            outcome.effects.iter().any(|effect| matches!(
                effect,
                crate::app::Effect::OpenSession { session_id, .. } if session_id == &sub_id
            )),
            "the sub-session key opened something else: {:?}",
            outcome.effects
        );
    }

    /// A delegated session's state reaches its own page through the tree.
    ///
    /// A worker is absent from the root list, and a remote authority announces
    /// a settled turn as a sidebar invalidation rather than a session event, so
    /// the branch read that follows is the only authoritative row for the
    /// worker on screen. Without that fold the page keeps the state its
    /// snapshot carried, and a worker whose turn ended sits under the composer
    /// as running until the reader opens it again.
    #[test]
    fn the_ownership_tree_settles_the_open_workers_turn() {
        let (worker, _messages) =
            Worker::start(vibex_backend::DisconnectedBackend::facade()).unwrap();
        let mut app = isolation_app();
        let main_id = vibex_core::VibexSessionId::parse("session_main00001").unwrap();
        let sub_id = vibex_core::VibexSessionId::parse("session_sub000001").unwrap();
        let main = isolation_session(main_id, "codex");
        let mut running = isolation_session(sub_id.clone(), "codex");
        running.state = vibex_core::AgentSessionState::Running;
        app.agent.apply_sessions(Ok(vec![main.clone()])).unwrap();
        app.agent.state.selected_session_id = Some(sub_id.clone());
        app.agent.state.active_session.resolve(running.clone());

        let mut settled = running.clone();
        settled.state = vibex_core::AgentSessionState::Idle;
        settled.updated_at_ms = 2;
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionTree {
                parent_session_id: Some(sub_id.clone()),
                append: false,
                result: Ok(ownership_page(
                    Vec::new(),
                    vec![main.clone(), settled.clone()],
                )),
            },
        )
        .unwrap();
        assert_eq!(app.active_session(), Some(&settled));

        // The same branch read again with the state this page has already left
        // behind is not news, and the row the list would draw is untouched.
        apply_message(
            &mut app,
            &worker,
            AppMessage::SessionTree {
                parent_session_id: Some(sub_id),
                append: false,
                result: Ok(ownership_page(Vec::new(), vec![main, running])),
            },
        )
        .unwrap();
        assert_eq!(app.active_session(), Some(&settled));
    }

    #[test]
    fn up_recalls_the_sessions_earlier_messages_in_the_composer() {
        use vibex_core::{TimelinePayload, UserMessagePayload};
        let worker = isolation_worker();
        let mut app = test_app(100, 30);
        app.navigate_to(Page::Agent);
        let id = vibex_core::VibexSessionId::new();
        app.agent.state.selected_session_id = Some(id.clone());
        let items = ["first question", "second question"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                conversation_item(
                    &id,
                    index as i64 + 1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: text.into(),
                        ..Default::default()
                    }),
                )
            })
            .collect::<Vec<_>>();
        app.agent.state.timeline.replace_authoritative(id, items);
        app.sync_transcript();
        conversation_frame(&mut app, 100, 30);
        app.focus = Focus::Composer;

        // `Up` from an empty draft lands on the newest message, and again on
        // the one before it. The keyboard stays in the composer: this is what
        // the desktop does, and the transcript is `Tab`'s destination.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "second question");
        assert_eq!(app.focus, Focus::Composer);
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "first question");
        // The oldest message is sticky rather than wrapping to the newest.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "first question");

        // `Down` walks back toward the newest message and, past it, returns the
        // draft the walk started from.
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "second question");
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "");

        // `Esc` leaves a running walk and puts the preserved draft back. The
        // draft belongs to the composer, so the walk starts from it only once
        // the box is empty again.
        app.composer.clear();
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "second question");
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "");
        assert_eq!(app.focus, Focus::Composer, "Esc left the session instead");

        // A live draft does not start a walk, so `Up` keeps the caret's own
        // upward movement.
        app.composer.set_text("a\nhalf-written\ndraft");
        handle_composer_key(
            &mut app,
            &worker,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        )
        .unwrap();
        assert_eq!(app.composer.text(), "a\nhalf-written\ndraft");
        assert_ne!(app.composer.cursor(), app.composer.text().len());
    }
}
