//! `App::perform`: the pure intent → effect reducer.
//!
//! Nothing here touches the terminal, the network, or the filesystem. Every
//! asynchronous consequence is expressed as an [`Effect`] value, which is the
//! property that lets the whole interaction model be tested by driving intents
//! and asserting on state.

use vibex_backend::{BackendOperation, MutationRequest};
use vibex_core::{
    AgentSessionState, ContinueAgentTurnRequest, CreateAgentSessionRequest, ElicitationAnswerValue,
    ElicitationResolution, ElicitationResolutionAction, ForkAgentSessionRequest,
    PermissionResolution, PermissionResponseKind, RenameAgentSessionRequest, RequestId,
    ResolveElicitationRequest, ResolvePermissionRequest, SendAgentMessageRequest,
    SteerAgentMessageRequest, VibexSessionId, WorkspaceMode,
};

use crate::action::Intent;
use crate::app::{
    App, Availability, ClipboardWanted, ComposerTarget, Effect, Focus, ManagementRow, Overlay,
    Page, PendingCreation, PromptField, RunOption, RunOptionKey, RunOptionKind, RuntimePickerView,
    Toast, WorkspacePickerRow,
};
use crate::composer::{CompletionMenu, CompletionTrigger};
use crate::keymap::Scope;
use crate::runtime_picker::{PICKER_PAGE_ROWS, RuntimePickerRow};
use crate::view::block_detail_text;
use vibex_core::SessionRuntimeSelection;

/// What an intent produced, for tests and for the event loop.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    /// Set when the intent changed something the renderer must repaint.
    pub dirty: bool,
}

impl Outcome {
    fn effects(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            dirty: true,
        }
    }

    fn quiet() -> Self {
        Self::default()
    }
}

/// The first response option matching `response`, falling back to the first
/// option the request actually advertised.
fn response_for(
    options: &[vibex_core::PermissionResponseOption],
    response: PermissionResponseKind,
) -> Option<PermissionResponseKind> {
    options
        .iter()
        .find(|option| option.response == response)
        .or_else(|| options.first())
        .map(|option| option.response)
}

impl App {
    /// Apply one intent.
    pub fn perform(&mut self, intent: Intent) -> Outcome {
        let mut outcome = self.perform_intent(intent);
        // Scrolling up to the very top is the asking gesture for older
        // history; `LoadOlderHistory` is the explicit one. The controller
        // refuses while a page is in flight, so holding a scroll key down
        // cannot fire a request per repeat.
        if matches!(
            intent,
            Intent::ScrollPageUp
                | Intent::ScrollHalfPageUp
                | Intent::ScrollToTop
                | Intent::SelectPrevious
        ) {
            self.request_older_history_at_top(&mut outcome);
        }
        outcome
    }

    /// Queue an older-page fetch when the transcript is parked at its top.
    fn request_older_history_at_top(&mut self, outcome: &mut Outcome) {
        // While the dock owns the list keys, `k` is moving its cursor, not
        // scrolling the transcript it happens to be sitting above.
        if self.dock_is_focused() || !self.at_transcript_top() {
            return;
        }
        if let Some(effect) = self.load_older_history() {
            outcome.effects.push(effect);
            outcome.dirty = true;
        }
    }

    fn perform_intent(&mut self, intent: Intent) -> Outcome {
        // An overlay swallows most intents; the overlay's own intents and the
        // global escape hatches still apply.
        if self.overlay.is_some() {
            return self.perform_overlay(intent);
        }
        if self.filtering {
            return self.perform_filter(intent);
        }

        if let Some(reason) = self.unavailable_reason(intent) {
            self.toast(Toast::warning(reason));
            return Outcome::quiet();
        }

        match intent {
            // ---- global -------------------------------------------------
            Intent::OpenCommandPalette => {
                self.overlay = Some(Overlay::Palette {
                    query: String::new(),
                    selected: 0,
                });
                Outcome::effects(vec![])
            }
            Intent::ToggleHelp => {
                self.overlay = Some(Overlay::Help {
                    query: String::new(),
                    selected: 0,
                    collapsed: std::collections::BTreeSet::new(),
                });
                Outcome::effects(vec![])
            }
            Intent::OpenSettings => {
                self.select_global(vibex_ui::shell::GlobalDestination::Settings);
                Outcome::effects(vec![])
            }
            // The palette's Quit command. It leaves without asking: the chord
            // that used to raise a modal is gone, and a command the reader
            // typed and chose is already the deliberate version of the press
            // `Ctrl+C` has to ask about.
            Intent::RequestQuit => {
                self.should_quit = true;
                Outcome::effects(vec![])
            }
            Intent::Back => self.go_back(),
            Intent::FocusNext => {
                self.focus = if self.page.is_session_page() {
                    if self.focus == Focus::Composer {
                        Focus::Main
                    } else {
                        Focus::Composer
                    }
                } else {
                    self.focus.next()
                };
                if self.focus == Focus::Composer && self.page != Page::NewSession {
                    self.navigate_to(Page::Agent);
                }
                Outcome::effects(vec![])
            }
            Intent::FocusPrevious => {
                self.focus = if self.page.is_session_page() {
                    if self.focus == Focus::Composer {
                        Focus::Main
                    } else {
                        Focus::Composer
                    }
                } else {
                    self.focus.previous()
                };
                Outcome::effects(vec![])
            }
            Intent::Refresh => self.refresh_current_page(),
            Intent::ContextualCancel => self.contextual_cancel(),
            Intent::GotoSessions => {
                self.select_global(vibex_ui::shell::GlobalDestination::Sessions);
                Outcome::effects(self.session_page_effects())
            }
            Intent::GotoManagement => {
                self.select_global(vibex_ui::shell::GlobalDestination::Management);
                Outcome::effects(vec![])
            }
            Intent::GotoUsage => {
                self.navigate_to(Page::Usage);
                self.navigation.level = vibex_ui::shell::NavigationLevel::Global;
                Outcome::effects(vec![Effect::LoadUsage])
            }
            Intent::ToggleSidebar => {
                self.toggle_sidebar_collapsed();
                Outcome::effects(vec![])
            }
            Intent::ReloadKeymap => {
                match crate::keymap::Keymap::user_path() {
                    Some(path) => {
                        self.keymap = crate::keymap::Keymap::load(&path);
                        if self.keymap.warnings.is_empty() {
                            let message = crate::locale::Strings::with_locale(self.settings.locale)
                                .settings_keymap_reloaded()
                                .to_string();
                            self.toast(Toast::success(message));
                        } else {
                            let message = crate::locale::Strings::with_locale(self.settings.locale)
                                .settings_keymap_error()
                                .to_string();
                            self.toast(Toast::warning(format!(
                                "{message}: {}",
                                self.keymap.warnings.join("; ")
                            )));
                        }
                    }
                    None => self.toast(Toast::warning("no home directory for tui-keys.toml")),
                }
                Outcome::effects(vec![])
            }

            // ---- selection motion ----------------------------------------
            // The dock's cursor is a second cursor on the agent page; while it
            // is up, the list keys belong to it.
            Intent::SelectPrevious if self.dock_is_focused() => {
                self.step_dock_selection(-1);
                Outcome::effects(vec![])
            }
            Intent::SelectNext if self.dock_is_focused() => {
                self.step_dock_selection(1);
                Outcome::effects(vec![])
            }
            Intent::SelectPrevious => self.move_selection(-1),
            Intent::SelectNext => self.move_selection(1),
            Intent::ScrollPageUp => {
                if self.scrolls_session_list() {
                    let step = self.session_page_step();
                    self.scroll_session_list(-(step as i64));
                } else {
                    self.scroll_by(-1, true);
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollPageDown => {
                if self.scrolls_session_list() {
                    let step = self.session_page_step();
                    self.scroll_session_list(step as i64);
                } else {
                    self.scroll_by(1, true);
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollHalfPageUp => {
                if self.scrolls_session_list() {
                    let step = self.session_page_step() / 2;
                    self.scroll_session_list(-(step.max(1) as i64));
                } else {
                    self.scroll_by(-1, false);
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollHalfPageDown => {
                if self.scrolls_session_list() {
                    let step = self.session_page_step() / 2;
                    self.scroll_session_list(step.max(1) as i64);
                } else {
                    self.scroll_by(1, false);
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollToTop => {
                if self.scrolls_session_list() {
                    self.session_list_home();
                } else {
                    self.scroll.follow = false;
                    self.scroll.offset = 0;
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollToBottom => {
                if self.scrolls_session_list() {
                    self.session_list_end();
                } else {
                    self.scroll.follow = true;
                }
                Outcome::effects(vec![])
            }
            Intent::LoadOlderHistory => {
                // An explicit ask, so it works even when the transcript has
                // not been scrolled: the loaded page still lands above the
                // viewport and the reader keeps their place.
                match self.load_older_history() {
                    Some(effect) => Outcome::effects(vec![effect]),
                    None => Outcome::quiet(),
                }
            }
            Intent::ShowDetails => {
                self.focus = Focus::Details;
                Outcome::effects(vec![])
            }
            Intent::BeginFilter => {
                if self.page == Page::Settings {
                    // The settings filter is the surface's own mode, not the
                    // list filter the other pages share.
                    self.begin_settings_filter();
                    return Outcome::effects(vec![]);
                }
                self.filtering = true;
                self.filter.clear();
                Outcome::effects(vec![])
            }
            Intent::ClearFilter => {
                if self.page == Page::Settings {
                    self.leave_settings_filter(true);
                    return Outcome::effects(vec![]);
                }
                self.filter.clear();
                self.filtering = false;
                Outcome::effects(vec![])
            }

            // ---- sessions -------------------------------------------------
            Intent::OpenSelectedSession => {
                let rows = self.sidebar_rows();
                let index = self.selection_for(Scope::Sessions);
                let Some(row) = rows.get(index).cloned() else {
                    return Outcome::quiet();
                };
                let Some(session_id) = row.session_id.clone() else {
                    // A heading -- a project, or a folder the reader made --
                    // toggles instead of opening. When the authority owns the
                    // arrangement, so does the toggle: a collapse the desktop
                    // would not see is not a collapse.
                    if let Some(effect) = self.sidebar_collapse_effect(&row) {
                        return Outcome::effects(vec![effect]);
                    }
                    self.toggle_sidebar_collapsed_for(&row.project_id);
                    return Outcome::effects(vec![]);
                };
                // The controller issues the load ticket before the fetch, so
                // the snapshot is applied in the generation it was requested
                // for and live events for the session stop being stale.
                self.open_session_effects(session_id)
            }
            Intent::EnterSession => {
                let outcome = self.perform(Intent::OpenSelectedSession);
                if !outcome.effects.is_empty() {
                    // Landing on the session view is a request to work in it, so
                    // the caret goes back into the composer: navigating set the
                    // focus to the page itself.
                    self.select_session_destination();
                    self.focus = Focus::Composer;
                }
                outcome
            }
            Intent::NewSession => self.begin_new_session(),
            Intent::BeginRenameSession => self.begin_rename_session(),
            Intent::ForkSession => {
                // The row the cursor is on, not the session the client has open
                // behind the list: the list shows rows, so its keys act on the
                // row they point at.
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                let message = format!(
                    "{} — {}",
                    self.strings.session_fork(),
                    self.strings.confirm()
                );
                self.toast(Toast::info(message));
                let request_id = VibexSessionId::new();
                self.pending_forks
                    .insert(request_id.clone(), self.navigation_serial);
                Outcome::effects(vec![Effect::ForkSession {
                    request_id,
                    session_id,
                }])
            }
            Intent::ArchiveSession => {
                if self.list_session_target().is_none() {
                    return Outcome::quiet();
                }
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.session_archive().to_string(),
                    body: self.strings.session_confirm_archive().to_string(),
                    confirm: Intent::ArchiveSession,
                });
                self.set_selection(Scope::Overlay, 0);
                Outcome::effects(vec![])
            }
            Intent::DeleteSession => {
                if self.list_session_target().is_none() {
                    return Outcome::quiet();
                }
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.session_delete().to_string(),
                    body: self.strings.session_confirm_delete().to_string(),
                    confirm: Intent::DeleteSession,
                });
                self.set_selection(Scope::Overlay, 0);
                Outcome::effects(vec![])
            }
            Intent::ToggleShowArchived => {
                self.show_archived = !self.show_archived;
                Outcome::effects(self.session_page_effects())
            }
            Intent::ToggleSessionCard => {
                let Some(session_id) = self.selected_session_row_id() else {
                    return Outcome::quiet();
                };
                self.toggle_session_card(session_id.as_str());
                Outcome::effects(vec![])
            }
            Intent::CollapseSessionCards => {
                let open = self.close_session_cards();
                if open == 0 {
                    let message = self.strings.session_cards_none();
                    self.toast(Toast::info(message));
                }
                Outcome::effects(vec![])
            }
            Intent::CopySessionRow => {
                let Some(session_id) = self.selected_session_row_id() else {
                    return Outcome::quiet();
                };
                let Some(session) = self.session_by_id(&session_id) else {
                    return Outcome::quiet();
                };
                let text = crate::view::session_card_text(self, session);
                let message = self.strings.copied();
                self.toast(Toast::success(message));
                Outcome::effects(vec![Effect::Clipboard { text }])
            }
            Intent::PinSession => {
                let selected = self.selected_sidebar_row();
                if let Some(row) = selected.as_ref()
                    && let Some(effect) = self.sidebar_pin_effect(row)
                {
                    let message = if row.pinned {
                        self.strings.sidebar_unpinned()
                    } else {
                        self.strings.sidebar_pinned()
                    };
                    self.toast(Toast::success(message));
                    return Outcome::effects(vec![effect]);
                }
                if self.toggle_session_pin() {
                    let message = match self.selected_sidebar_row() {
                        Some(row) if row.pinned => self.strings.sidebar_pinned(),
                        _ => self.strings.sidebar_unpinned(),
                    };
                    self.toast(Toast::success(message));
                }
                Outcome::effects(vec![])
            }
            Intent::ToggleAutoContinue => {
                // On the list the key acts on the row under the cursor; inside a
                // session it acts on the session being read, which is where the
                // countdown is visible.
                let session_id = if self.page == Page::Sessions {
                    let Some(row) = self.selected_sidebar_row() else {
                        return Outcome::quiet();
                    };
                    // A heading owns no turn to continue. The Desktop's
                    // project-level default is its own menu.
                    let Some(session_id) = row.session_id.clone() else {
                        return Outcome::quiet();
                    };
                    session_id
                } else {
                    let Some(session_id) = self.selected_session_id().cloned() else {
                        return Outcome::quiet();
                    };
                    session_id
                };
                let now_ms = vibex_core::unix_timestamp_ms();
                // One key, four states, each of them a control the Desktop
                // has: stop a countdown, resume a suspension, switch off,
                // switch on.
                let message = if self.auto_continue.is_counting_down(&session_id) {
                    self.auto_continue.pause(&session_id, now_ms);
                    self.strings.auto_continue_paused()
                } else if self.auto_continue.is_paused(&session_id) {
                    self.auto_continue.resume(&session_id);
                    self.strings.auto_continue_resumed()
                } else if self.auto_continue.is_enabled(&session_id) {
                    self.auto_continue.set_enabled(&session_id, false);
                    self.strings.auto_continue_disabled()
                } else {
                    self.auto_continue.set_enabled(&session_id, true);
                    self.strings.auto_continue_enabled()
                };
                let enabled = self.auto_continue.is_enabled(&session_id);
                self.toast(Toast::success(message.to_string()));
                // The preference is the authority's when one owns the tree: it
                // travels in the sidebar arrangement, so the desktop shows it
                // and the next session here adopts it.
                match self.sidebar_auto_continue_effect(&session_id, enabled) {
                    Some(effect) => Outcome::effects(vec![effect]),
                    None => Outcome::quiet(),
                }
            }
            // Up the screen is a smaller row index.
            Intent::MoveSessionUp => self.move_session(-1),
            Intent::MoveSessionDown => self.move_session(1),
            Intent::SwitchWorkspace => {
                // The key is on the composing page's own line, so it has to do
                // what that line says: open the picker over the runtime's
                // listing. Listing workspaces into state nothing drew was a
                // gesture with no result — and it moved the reader off the page
                // they were writing on.
                self.overlay = Some(Overlay::WorkspacePicker { selected: 0 });
                Outcome::effects(vec![Effect::BrowseDirectories {
                    path: self.workspace_picker_start(),
                }])
            }
            Intent::OpenWorkspaceBrowser => {
                self.overlay = Some(Overlay::WorkspacePicker { selected: 0 });
                // The listing is what the picker draws, and the browse root is
                // where a reader who has never been anywhere should start.
                Outcome::effects(vec![Effect::BrowseDirectories {
                    path: self.workspace_picker_start(),
                }])
            }
            Intent::WorkspaceBrowseUp => self.workspace_browse_parent(),
            Intent::WorkspaceBrowseSelect => {
                // The same gesture the picker answers to: "use this directory"
                // names the one the listing is in, not whichever row a cursor
                // somewhere else happens to be on.
                match self.workspace_picker_here() {
                    Some(path) => self.use_workspace_directory(path),
                    None => Outcome::quiet(),
                }
            }

            // ---- agent transcript ----------------------------------------
            Intent::FocusComposer => {
                if self.page != Page::NewSession {
                    self.navigate_to(Page::Agent);
                }
                self.focus = Focus::Composer;
                Outcome::effects(vec![])
            }
            Intent::ContinueTurn => {
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                if !self.session_can_continue() {
                    self.toast(Toast::warning(
                        self.strings.toast_action_unavailable().to_string(),
                    ));
                    return Outcome::quiet();
                }
                // A manual continuation is a reactivation: it resumes a
                // suspension and settles the turn, exactly as the Desktop's own
                // Continue button does.
                if let Some(updated_at_ms) = self
                    .session_by_id(&session_id)
                    .map(|session| session.updated_at_ms)
                {
                    self.auto_continue
                        .note_continued(&session_id, updated_at_ms);
                }
                self.auto_continue.resume(&session_id);
                Outcome::effects(vec![Effect::ContinueTurn { session_id }])
            }
            Intent::ToggleBlockExpanded => {
                let index = self.selection_for(Scope::Agent);
                if !self.transcript.toggle_block(index) {
                    self.toast(Toast::info(
                        self.strings.toast_action_unavailable().to_string(),
                    ));
                }
                Outcome::effects(vec![])
            }
            Intent::ToggleAllBlocksExpanded => {
                let expand = !self.transcript.all_expanded();
                self.transcript.toggle_all(expand);
                if let Some(index) = self
                    .transcript
                    .visible_block(self.selection_for(Scope::Agent))
                {
                    self.set_selection(Scope::Agent, index);
                }
                Outcome::effects(vec![])
            }
            Intent::ToggleReasoningExpanded => {
                self.transcript.toggle_reasoning();
                if let Some(index) = self
                    .transcript
                    .visible_block(self.selection_for(Scope::Agent))
                {
                    self.set_selection(Scope::Agent, index);
                }
                Outcome::effects(vec![])
            }
            Intent::CopyBlockBody => {
                // A live selection is what the reader last pointed at, so `y`
                // copies that rather than surprising them with the whole block.
                if self.text_selection.is_some()
                    && let Some(text) = self.selected_text()
                {
                    let message = self.strings.copied().to_string();
                    self.toast(Toast::success(message));
                    return Outcome::effects(vec![Effect::Clipboard { text }]);
                }
                let index = self.selection_for(Scope::Agent);
                match self.transcript.block_text(index) {
                    Some(text) => {
                        let message = self.strings.copied().to_string();
                        self.toast(Toast::success(message));
                        Outcome::effects(vec![Effect::Clipboard { text }])
                    }
                    None => Outcome::quiet(),
                }
            }
            Intent::CopyBlockMetadata => {
                let index = self.selection_for(Scope::Agent);
                match self.transcript.block_metadata(index) {
                    Some(text) => {
                        let message = self.strings.copied().to_string();
                        self.toast(Toast::success(message));
                        Outcome::effects(vec![Effect::Clipboard { text }])
                    }
                    None => Outcome::quiet(),
                }
            }
            Intent::OpenBlockDetails => {
                let index = self.selection_for(Scope::Agent);
                match block_detail_text(&mut self.transcript, index) {
                    Some((title, body)) => {
                        self.overlay = Some(Overlay::TextView {
                            title,
                            body,
                            scroll: 0,
                        });
                        Outcome::effects(vec![])
                    }
                    None => Outcome::quiet(),
                }
            }
            Intent::SwitchAgentRuntime => self.open_runtime_picker(),
            Intent::ProbeAgentRuntime => match self.active_session() {
                Some(session) => {
                    let request = vibex_core::AgentRuntimeOptionProbeRequest {
                        agent_id: session.agent_id.clone(),
                    };
                    Outcome::effects(vec![Effect::ProbeAgentRuntime { request }])
                }
                None => Outcome::quiet(),
            },
            Intent::QueueSelectPrevious => {
                self.move_queue_selection(-1);
                Outcome::effects(vec![])
            }
            Intent::QueueSelectNext => {
                self.move_queue_selection(1);
                Outcome::effects(vec![])
            }
            Intent::QueueEditSelected => {
                if self.edit_queued_message() {
                    self.toast(Toast::info(self.strings.queue_editing().to_string()));
                }
                Outcome::effects(vec![])
            }
            Intent::QueueDeleteSelected => {
                self.delete_queued_message();
                Outcome::effects(vec![])
            }
            Intent::QueueMoveUp => {
                self.move_queued_message(-1);
                Outcome::effects(vec![])
            }
            Intent::QueueMoveDown => {
                self.move_queued_message(1);
                Outcome::effects(vec![])
            }
            Intent::QueueSendNow => {
                if self
                    .selected_session_id()
                    .is_some_and(|id| self.session_is_uncreated(id))
                {
                    return Outcome::quiet();
                }
                let Some((text, attachments)) = self.take_queued_message() else {
                    return Outcome::quiet();
                };
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                // Sending now means the running turn is interrupted first: the
                // alternative is a message that claims to be immediate and is
                // not.
                let mut effects = Vec::new();
                if self.session_running() {
                    effects.push(Effect::Interrupt {
                        session_id: session_id.clone(),
                    });
                }
                self.history.push(text.clone());
                // The message is the reactivation that lifts a suspension.
                self.auto_continue.resume(&session_id);
                let send_id =
                    self.mark_send_dispatched(Some(&session_id), text.clone(), attachments.clone());
                effects.push(Effect::SendMessage {
                    session_id,
                    send_id,
                    correlation_id: self.pending_sends[&send_id].correlation_id.clone(),
                    text,
                    attachments,
                });
                Outcome::effects(effects)
            }
            Intent::AttachImage => Outcome::effects(vec![Effect::ReadClipboard {
                ticket: self.composer_ticket(),
                wanted: ClipboardWanted::Image,
            }]),
            Intent::PasteClipboard => Outcome::effects(vec![Effect::ReadClipboard {
                ticket: self.composer_ticket(),
                wanted: ClipboardWanted::Everything,
            }]),
            Intent::ToggleDock => {
                self.dock_open = !self.dock_open;
                if self.dock_open {
                    // Opening the panel puts the cursor in it, because that is
                    // the only reason to open it: something is running and the
                    // reader wants to act on it.
                    self.dock_selection = Some(0);
                    self.step_dock_selection(1);
                    if self.dock_rows().is_empty() {
                        self.dock_selection = Some(0);
                    }
                } else {
                    self.dock_selection = None;
                }
                Outcome::effects(vec![])
            }
            Intent::DockActivate => {
                self.activate_dock_row();
                Outcome::effects(vec![])
            }
            Intent::DockHideDone => {
                let hidden = self.toggle_dock_hide_done();
                let message = if hidden {
                    self.strings.dock_hidden_done()
                } else {
                    self.strings.dock_shown_done()
                };
                self.toast(Toast::info(message));
                // The row the cursor was on may have just disappeared.
                let last = self.dock_rows().len().saturating_sub(1);
                self.dock_selection = self.dock_selection.map(|index| index.min(last));
                Outcome::effects(vec![])
            }
            Intent::BeginTranscriptSearch => {
                // The page follows the search, not the other way round: a
                // transcript with nothing in it has nothing to search, and
                // moving there first is how `/` used to leave the page a reader
                // was writing on for a session they had not opened.
                if !self.begin_search() {
                    self.toast(Toast::info(self.strings.transcript_empty()));
                    return Outcome::effects(vec![]);
                }
                self.navigate_to(Page::Agent);
                Outcome::effects(vec![])
            }
            Intent::SearchNext => {
                self.step_search(1);
                Outcome::effects(vec![])
            }
            Intent::SearchPrevious => {
                self.step_search(-1);
                Outcome::effects(vec![])
            }

            // ---- composer -------------------------------------------------
            Intent::SubmitComposer => self.submit_composer(),
            Intent::InsertNewline => {
                self.composer.insert_char('\n');
                Outcome::effects(vec![])
            }
            Intent::ToggleMultiline => {
                self.composer.insert_char('\n');
                Outcome::effects(vec![])
            }
            Intent::ComposerHistoryPrevious => {
                let current = self.composer.text().to_string();
                if let Some(entry) = self.history.previous(&current) {
                    self.composer.set_text(entry);
                }
                Outcome::effects(vec![])
            }
            Intent::ComposerHistoryNext => {
                if let Some(entry) = self.history.next() {
                    self.composer.set_text(entry);
                }
                Outcome::effects(vec![])
            }
            Intent::EditComposerExternally => {
                let body = self.composer.text().to_string();
                Outcome::effects(vec![Effect::EditExternally {
                    ticket: Some(self.composer_ticket()),
                    title: self.strings.composer_editor_title().to_string(),
                    body,
                }])
            }
            Intent::BackgroundRunningCommand => {
                self.toast(Toast::info(
                    self.strings.toast_action_unavailable().to_string(),
                ));
                Outcome::quiet()
            }
            Intent::SteerRunningTurn => self.steer_composer(),
            Intent::CompletionNext => {
                self.move_completion(1);
                Outcome::effects(vec![])
            }
            Intent::CompletionPrevious => {
                self.move_completion(-1);
                Outcome::effects(vec![])
            }
            Intent::CompletionAccept => self.accept_completion(),
            Intent::CompletionCancel => {
                self.completion = None;
                Outcome::effects(vec![])
            }
            Intent::DeleteWordBefore => {
                // The page where a session is written names `Ctrl+W` as the way
                // to change where it will work, and this binding — the shell's
                // word kill — is what answers that key first. The kill keeps the
                // key while there is a word to kill; with nothing to kill the
                // key does what the page says, the same way an empty editor
                // hands `Up` to the transcript instead of the history.
                if !self.composer.delete_word_before()
                    && self.page == Page::NewSession
                    && self.focus == Focus::Composer
                {
                    return self.perform(Intent::SwitchWorkspace);
                }
                Outcome::effects(vec![])
            }
            Intent::DeleteWordAfter => {
                self.composer.delete_word_after();
                Outcome::effects(vec![])
            }
            Intent::DeleteWordBackward => {
                self.composer.delete_word_backward();
                Outcome::effects(vec![])
            }
            Intent::KillToLineEnd => {
                self.composer.kill_to_line_end();
                Outcome::effects(vec![])
            }
            Intent::KillToLineStart => {
                self.composer.kill_to_line_start();
                Outcome::effects(vec![])
            }
            Intent::YankKill => {
                if !self.composer.yank() {
                    let message = self.strings.composer_nothing_to_yank();
                    self.toast(Toast::info(message));
                }
                Outcome::effects(vec![])
            }
            Intent::ComposerUndo => {
                if !self.composer.undo() {
                    let message = self.strings.composer_nothing_to_undo();
                    self.toast(Toast::info(message));
                }
                self.refresh_completion();
                Outcome::effects(vec![])
            }
            Intent::ComposerRedo => {
                if !self.composer.redo() {
                    let message = self.strings.composer_nothing_to_redo();
                    self.toast(Toast::info(message));
                }
                self.refresh_completion();
                Outcome::effects(vec![])
            }
            Intent::ComposerWordLeft => {
                self.composer.move_word_left();
                Outcome::effects(vec![])
            }
            Intent::ComposerWordRight => {
                self.composer.move_word_right();
                Outcome::effects(vec![])
            }
            Intent::ComposerLineStart => {
                self.composer.move_line_start();
                Outcome::effects(vec![])
            }
            Intent::ComposerLineEnd => {
                self.composer.move_line_end();
                Outcome::effects(vec![])
            }
            Intent::SelectAllDraft => {
                self.composer.select_all();
                Outcome::effects(vec![])
            }
            Intent::CopyDraftSelection => {
                let Some(text) = self.composer.selected_text() else {
                    let message = self.strings.composer_nothing_selected();
                    self.toast(Toast::info(message));
                    return Outcome::quiet();
                };
                let message = self.strings.copied().to_string();
                self.toast(Toast::success(message));
                Outcome::effects(vec![Effect::Clipboard { text }])
            }

            // ---- approval and elicitation ---------------------------------
            Intent::ApprovalApprove | Intent::ApprovalDeny | Intent::ApprovalAlways => {
                self.open_approval();
                self.perform_overlay(intent)
            }
            Intent::ElicitationSubmit => {
                self.open_elicitation();
                Outcome::effects(vec![])
            }
            Intent::ApprovalFocusNext => {
                self.open_approval();
                self.perform_overlay(intent)
            }
            Intent::ElicitationFieldNext => {
                self.open_elicitation();
                self.perform_overlay(intent)
            }
            Intent::ElicitationFieldPrevious => {
                self.open_elicitation();
                self.perform_overlay(intent)
            }
            Intent::OverlayToggleValue => {
                self.open_elicitation();
                Outcome::effects(vec![])
            }
            Intent::CloseOverlay
            | Intent::ConfirmOverlay
            | Intent::PaletteRun
            | Intent::OverlayNextField
            | Intent::OverlayPreviousField => Outcome::quiet(),

            // ---- management ------------------------------------------------
            Intent::OpenManagementSection => {
                let index = self.selection_for(Scope::Management);
                let Some(row) = ManagementRow::ALL.get(index) else {
                    return Outcome::quiet();
                };
                self.navigate_to(row.page());
                self.refresh_current_page()
            }
            Intent::ReloadManagement => self.refresh_current_page(),
            Intent::ToggleSelectedEntry => self.toggle_selected_entry(),
            Intent::EditSelectedEntry => self.begin_edit_entry(),
            Intent::InstallOrUpdateAgent => self.install_selected_agent(false),
            Intent::UninstallAgent => self.install_selected_agent(true),
            Intent::AgentAuthMenu => self.list_agent_auth(),
            Intent::AgentAuthRefresh => self.list_agent_auth(),
            Intent::AgentLogout => self.logout_selected_agent(),
            Intent::ActivateProviderProfile => self.activate_provider_profile(),
            Intent::EditProviderProfile => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.management_providers().to_string(),
                    field: PromptField::ProviderEndpoint,
                    value: String::new(),
                });
                Outcome::effects(vec![])
            }
            Intent::EditProviderProjection => {
                self.toast(Toast::info(
                    self.strings.management_form_essentials().to_string(),
                ));
                Outcome::quiet()
            }
            Intent::EditProviderSecret => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.management_providers().to_string(),
                    field: PromptField::ProviderSecret,
                    value: String::new(),
                });
                Outcome::effects(vec![])
            }
            Intent::TestProviderProfile => self.test_provider_profile(),
            Intent::FetchProviderModels => self.fetch_provider_models(),
            Intent::ProviderHealth => Outcome::effects(vec![Effect::ListHealth]),
            Intent::CreatePairingCode => {
                let availability = self.availability(BackendOperation::DevicePairing);
                match availability {
                    Availability::Available => Outcome::effects(vec![Effect::CreatePairingOffer]),
                    Availability::RequiresPermission => {
                        self.toast(Toast::warning(
                            self.strings.permission_required_for().to_string(),
                        ));
                        Outcome::quiet()
                    }
                    _ => {
                        self.toast(Toast::warning(
                            self.strings.toast_action_unavailable().to_string(),
                        ));
                        Outcome::quiet()
                    }
                }
            }
            Intent::RevokeSelectedDevice => self.begin_revoke_device(),
            Intent::OpenDeviceAudit => Outcome::effects(vec![Effect::ListAudit]),

            // ---- usage -------------------------------------------------------
            Intent::UsageSessionScope => {
                self.usage_scope_session = !self.usage_scope_session;
                Outcome::effects(vec![Effect::LoadUsage])
            }

            // ---- settings ----------------------------------------------------
            Intent::ActivateSetting => self.activate_setting(),
            Intent::SettingPrevious => self.step_setting(-1),
            Intent::SettingNext => self.step_setting(1),
            Intent::ResetSetting => self.begin_reset_setting(),

            // ---- the runtime switcher ----------------------------------------
            // These are the switcher's own keys. Reaching the reducer with no
            // overlay open means the switcher is not up, and there is nothing
            // for them to act on: `Ctrl+G` is what puts it there.
            Intent::OverlayFoldOpen
            | Intent::OverlayFoldClosed
            | Intent::StarRuntimeModel
            | Intent::ManageRuntimeAccount
            | Intent::ResetRunOption => Outcome::quiet(),
        }
    }

    fn guard(&mut self, operation: BackendOperation, effect: Effect) -> Outcome {
        match self.unavailable_outcome(operation) {
            Some(outcome) => outcome,
            None => Outcome::effects(vec![effect]),
        }
    }

    /// The toast an operation earns when the backend cannot run it.
    ///
    /// `None` means "go ahead". Callers that have work to do before building an
    /// effect — opening a picker, reading a catalogue — ask this first, so an
    /// unavailable action explains itself instead of opening a surface that
    /// cannot do anything.
    fn unavailable_outcome(&mut self, operation: BackendOperation) -> Option<Outcome> {
        match self.availability(operation) {
            Availability::Available => None,
            Availability::RequiresPermission => {
                self.toast(Toast::warning(
                    self.strings.permission_required_for().to_string(),
                ));
                Some(Outcome::quiet())
            }
            Availability::Offline => {
                self.toast(Toast::warning(self.strings.toast_offline().to_string()));
                Some(Outcome::quiet())
            }
            Availability::Unsupported => {
                self.toast(Toast::warning(
                    self.strings.toast_action_unavailable().to_string(),
                ));
                Some(Outcome::quiet())
            }
        }
    }

    // ---- overlay dispatch -----------------------------------------------

    /// Ask for a listing of `path`, staying inside the picker.
    ///
    /// Walking the tree is part of browsing it, so this answers with the
    /// listing to fetch rather than closing anything. It is the effect rather
    /// than a `perform` of an intent because the picker's own handler calls it:
    /// re-entering the overlay is how the way up used to recurse until the
    /// stack ran out.
    fn workspace_browse(&mut self, path: String) -> Outcome {
        // The cursor goes back to the top with the listing it was on: a row
        // index into the directory being left would name a different directory
        // in the new one, or none at all.
        if let Some(Overlay::WorkspacePicker { selected }) = self.overlay.as_mut() {
            *selected = 0;
        }
        Outcome::effects(vec![Effect::BrowseDirectories { path: Some(path) }])
    }

    /// Ask for the directory above the one being listed, if there is one.
    fn workspace_browse_parent(&mut self) -> Outcome {
        let parent = self
            .workspace_browse
            .as_ref()
            .and_then(|listing| listing.parent.clone());
        let Some(parent) = parent else {
            // Not an error: the reader is at the top of what can be browsed —
            // the filesystem root, or the browse roots a paired authority
            // allows — and saying so is the honest answer to the key.
            self.toast(Toast::warning(self.strings.workspace_at_root().to_string()));
            return Outcome::quiet();
        };
        self.workspace_browse(parent)
    }

    /// The directory the picker is showing — the path in its title.
    fn workspace_picker_here(&self) -> Option<String> {
        self.workspace_browse
            .as_ref()
            .map(|listing| listing.path.clone())
    }

    /// The directory "use this directory" would take, given the drawn row the
    /// cursor is on.
    ///
    /// A folder under the cursor is the answer: the reader pointed at it, and
    /// the whole listing is a menu of folders. The `..` row is a step rather
    /// than one of those folders — and an empty directory has nothing else to
    /// offer — so there the answer is the directory being shown, which is what
    /// makes the picker's own directory choosable at all.
    fn workspace_picker_target(&self, row: usize) -> Option<String> {
        match self.workspace_picker_rows().get(row) {
            Some(WorkspacePickerRow::Entry { path, .. }) => Some(path.clone()),
            _ => self.workspace_picker_here(),
        }
    }

    /// Make `path` where the next session works, closing the picker over it.
    fn use_workspace_directory(&mut self, path: String) -> Outcome {
        let composing = self.page == Page::NewSession;
        self.workspace_path = Some(path);
        self.overlay = None;
        // The picker answers the page that opened it: a reader writing a new
        // session stays there with the directory chosen, and a reader who
        // opened it from the session list stays there too.
        if composing {
            self.page = Page::NewSession;
            self.focus = Focus::Composer;
        }
        Outcome::effects(vec![])
    }

    fn perform_overlay(&mut self, intent: Intent) -> Outcome {
        // Global escape hatches still work with an overlay open.
        match intent {
            Intent::CloseOverlay | Intent::Back => {
                // The switcher's two views nest: the run options were opened
                // from the catalogue, and the value list from the run options,
                // so `Esc` steps back one level at a time rather than dropping
                // the reader out of the surface they are working in.
                self.overlay = match self.overlay.take() {
                    Some(Overlay::RunOptionValues { row, .. }) => Some(Overlay::RuntimePicker {
                        view: RuntimePickerView::Options,
                        selected: row,
                    }),
                    Some(Overlay::RuntimePicker {
                        view: RuntimePickerView::Options,
                        ..
                    }) => {
                        // Leaving the run options leaves what was changed on
                        // them: every row was sent as it was set, so there is
                        // nothing staged to drop and nothing to warn about.
                        Some(Overlay::RuntimePicker {
                            view: RuntimePickerView::Choices,
                            selected: self.runtime_picker_current_row(),
                        })
                    }
                    Some(Overlay::RuntimePicker {
                        view: RuntimePickerView::Choices,
                        ..
                    }) => {
                        // Closing the catalogue also drops the search: the next
                        // `Ctrl+G` is a fresh question, and a filter left over
                        // from the last one would answer it with a shorter list
                        // than the reader asked for.
                        self.runtime_picker.filtering = false;
                        self.runtime_picker.query.clear();
                        None
                    }
                    // A cancelled text option takes its key with it, so the
                    // next prompt cannot submit a value for this one.
                    Some(Overlay::Prompt {
                        field: PromptField::RunOptionValue,
                        ..
                    }) => {
                        self.run_option_prompt = None;
                        None
                    }
                    _ => None,
                };
                return Outcome::effects(vec![]);
            }
            // A quit asked for from behind an overlay still leaves, so a
            // rebind in `tui-keys.toml` cannot be trapped by one.
            Intent::RequestQuit => {
                self.should_quit = true;
                return Outcome::effects(vec![]);
            }
            _ => {}
        }
        let Some(overlay) = self.overlay.clone() else {
            return Outcome::quiet();
        };
        match overlay {
            Overlay::Palette { query, selected } => self.perform_palette(intent, query, selected),
            // The editor owns its keys directly, including the captured chord;
            // only the overlay-wide intents reach the reducer, and the two that
            // mean something here are handled above.
            Overlay::Keys { .. } => Outcome::quiet(),
            Overlay::Help {
                query,
                selected,
                collapsed,
            } => match intent {
                // The cheatsheet is navigated row by row; the renderer keeps the
                // selection on screen, so there is no scroll offset to keep.
                Intent::ScrollPageUp | Intent::SelectPrevious => {
                    self.overlay = Some(Overlay::Help {
                        query,
                        selected: selected.saturating_sub(1),
                        collapsed,
                    });
                    Outcome::effects(vec![])
                }
                Intent::ScrollPageDown | Intent::SelectNext => {
                    self.overlay = Some(Overlay::Help {
                        query,
                        selected: selected.saturating_add(1),
                        collapsed,
                    });
                    Outcome::effects(vec![])
                }
                Intent::ScrollToTop => {
                    self.overlay = Some(Overlay::Help {
                        query,
                        selected: 0,
                        collapsed,
                    });
                    Outcome::effects(vec![])
                }
                Intent::ScrollToBottom => {
                    self.set_overlay_scroll(usize::MAX);
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
            Overlay::Confirm { confirm, .. } => match intent {
                Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                    self.overlay = None;
                    let mut outcome = self.perform_confirm(confirm);
                    outcome.dirty = true;
                    outcome
                }
                _ => Outcome::quiet(),
            },
            Overlay::Prompt {
                field,
                value,
                title,
            } => match intent {
                Intent::ConfirmOverlay => {
                    self.overlay = None;
                    let mut outcome = self.submit_prompt(field, value, title);
                    outcome.dirty = true;
                    outcome
                }
                _ => Outcome::quiet(),
            },
            Overlay::Approval { selected } => match intent {
                Intent::ApprovalApprove
                | Intent::ApprovalDeny
                | Intent::ApprovalAlways
                | Intent::ConfirmOverlay => {
                    let kind = match intent {
                        Intent::ApprovalDeny => PermissionResponseKind::Deny,
                        Intent::ApprovalAlways => PermissionResponseKind::AlwaysAllowForSession,
                        _ => PermissionResponseKind::Approve,
                    };
                    self.overlay = None;
                    self.resolve_approval(kind, selected)
                }
                Intent::ApprovalFocusNext | Intent::OverlayNextField => {
                    let count = self.approvals().len();
                    if count == 0 {
                        return Outcome::quiet();
                    }
                    self.overlay = Some(Overlay::Approval {
                        selected: (selected + 1) % count,
                    });
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
            Overlay::Elicitation { field } => match intent {
                Intent::ElicitationSubmit | Intent::ConfirmOverlay => {
                    self.overlay = None;
                    self.submit_elicitation(field)
                }
                Intent::ElicitationFieldNext | Intent::OverlayNextField => {
                    let count = self.current_elicitation_fields();
                    if count == 0 {
                        return Outcome::quiet();
                    }
                    self.overlay = Some(Overlay::Elicitation {
                        field: (field + 1) % count,
                    });
                    Outcome::effects(vec![])
                }
                Intent::ElicitationFieldPrevious | Intent::OverlayPreviousField => {
                    let count = self.current_elicitation_fields();
                    if count == 0 {
                        return Outcome::quiet();
                    }
                    self.overlay = Some(Overlay::Elicitation {
                        field: field.saturating_sub(1),
                    });
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
            Overlay::PairingCode { .. } => Outcome::quiet(),
            Overlay::RuntimePicker { view, selected } => {
                self.perform_runtime_picker(intent, view, selected)
            }
            Overlay::RunOptionValues {
                row,
                selected,
                option,
            } => match intent {
                Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                    let choices = self.run_option_choices(&option);
                    let Some((value, _)) = choices.get(selected) else {
                        return Outcome::quiet();
                    };
                    // The value is sent as it is chosen: the row the reader
                    // comes back to shows what the session is on, not what it
                    // would be on if they remembered to apply it.
                    let outcome = self.apply_run_option(&option.key, value.clone());
                    self.overlay = Some(Overlay::RuntimePicker {
                        view: RuntimePickerView::Options,
                        selected: row,
                    });
                    self.runtime_picker.option_row = row;
                    outcome
                }
                Intent::SelectNext => {
                    let count = self.run_option_choices(&option).len();
                    self.overlay = Some(Overlay::RunOptionValues {
                        row,
                        selected: (selected + 1) % count.max(1),
                        option,
                    });
                    Outcome::effects(vec![])
                }
                Intent::SelectPrevious => {
                    let count = self.run_option_choices(&option).len();
                    self.overlay = Some(Overlay::RunOptionValues {
                        row,
                        selected: (selected + count.max(1) - 1) % count.max(1),
                        option,
                    });
                    Outcome::effects(vec![])
                }
                // A value list is a list: a long one is walked with the same
                // keys as every other list in the client rather than only with
                // the arrow keys it happens to fit on screen.
                Intent::ScrollPageUp
                | Intent::ScrollPageDown
                | Intent::ScrollToTop
                | Intent::ScrollToBottom => {
                    let count = self.run_option_choices(&option).len();
                    let selected = match intent {
                        Intent::ScrollPageUp => selected.saturating_sub(PICKER_PAGE_ROWS),
                        Intent::ScrollPageDown => selected
                            .saturating_add(PICKER_PAGE_ROWS)
                            .min(count.saturating_sub(1)),
                        Intent::ScrollToTop => 0,
                        _ => count.saturating_sub(1),
                    };
                    self.overlay = Some(Overlay::RunOptionValues {
                        row,
                        selected,
                        option,
                    });
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
            Overlay::WorkspacePicker { selected } => match intent {
                // Enter *opens* the highlighted row rather than choosing it:
                // the reader walks the tree the way a file manager does, and
                // the `..` row is the way back up. Choosing the directory the
                // picker is in is its own gesture (`Space`), exactly as the
                // desktop's picker opens a row and confirms the folder it is
                // showing.
                Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                    match self.workspace_picker_rows().get(selected).cloned() {
                        Some(WorkspacePickerRow::Parent { .. }) => self.workspace_browse_parent(),
                        Some(WorkspacePickerRow::Entry { path, .. }) => self.workspace_browse(path),
                        None => Outcome::quiet(),
                    }
                }
                // The folder under the cursor becomes where the next session
                // works, and the picker closes over the choice. On the `..` row
                // there is no such folder, so the directory being shown is the
                // answer rather than nothing.
                Intent::WorkspaceBrowseSelect => match self.workspace_picker_target(selected) {
                    Some(path) => self.use_workspace_directory(path),
                    None => Outcome::quiet(),
                },
                Intent::SelectNext => {
                    // Every drawn row is walked, `..` included: a reader who
                    // only ever presses Down has to be able to reach it.
                    let count = self.workspace_picker_rows().len();
                    self.overlay = Some(Overlay::WorkspacePicker {
                        selected: if count == 0 {
                            0
                        } else {
                            (selected + 1) % count
                        },
                    });
                    Outcome::effects(vec![])
                }
                Intent::SelectPrevious => {
                    self.overlay = Some(Overlay::WorkspacePicker {
                        selected: selected.saturating_sub(1),
                    });
                    Outcome::effects(vec![])
                }
                // Climbing out of a directory is part of browsing it, so it
                // stays inside the picker rather than closing it.
                Intent::WorkspaceBrowseUp => self.workspace_browse_parent(),
                _ => Outcome::quiet(),
            },
            Overlay::BlockDetails { scroll, .. } => match intent {
                Intent::ScrollPageDown | Intent::SelectNext => {
                    self.set_overlay_scroll(scroll + 10);
                    Outcome::effects(vec![])
                }
                Intent::ScrollPageUp | Intent::SelectPrevious => {
                    self.set_overlay_scroll(scroll.saturating_sub(10));
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
            Overlay::TextView {
                title,
                body,
                scroll,
            } => match intent {
                Intent::ScrollPageDown | Intent::SelectNext => {
                    self.overlay = Some(Overlay::TextView {
                        title,
                        body,
                        scroll: scroll + 10,
                    });
                    Outcome::effects(vec![])
                }
                Intent::ScrollPageUp | Intent::SelectPrevious => {
                    self.overlay = Some(Overlay::TextView {
                        title,
                        body,
                        scroll: scroll.saturating_sub(10),
                    });
                    Outcome::effects(vec![])
                }
                Intent::ScrollToTop => {
                    self.overlay = Some(Overlay::TextView {
                        title,
                        body,
                        scroll: 0,
                    });
                    Outcome::effects(vec![])
                }
                _ => Outcome::quiet(),
            },
        }
    }

    fn perform_palette(&mut self, intent: Intent, query: String, selected: usize) -> Outcome {
        match intent {
            Intent::ConfirmOverlay | Intent::PaletteRun => {
                let matches = self.palette_entries(&query);
                let Some(listing) = matches.get(selected).copied() else {
                    return Outcome::quiet();
                };
                self.overlay = None;
                self.remember_command(listing.entry.intent);
                self.perform(listing.entry.intent)
            }
            Intent::SelectNext => {
                let count = self.palette_entries(&query).len();
                self.overlay = Some(Overlay::Palette {
                    query,
                    selected: if count == 0 {
                        0
                    } else {
                        (selected + 1) % count
                    },
                });
                Outcome::effects(vec![])
            }
            Intent::SelectPrevious => {
                self.overlay = Some(Overlay::Palette {
                    query,
                    selected: selected.saturating_sub(1),
                });
                Outcome::effects(vec![])
            }
            _ => Outcome::quiet(),
        }
    }

    fn perform_filter(&mut self, intent: Intent) -> Outcome {
        match intent {
            Intent::Back | Intent::CloseOverlay | Intent::ContextualCancel => {
                self.filtering = false;
                self.filter.clear();
                Outcome::effects(self.session_page_effects())
            }
            Intent::ConfirmOverlay => {
                self.filtering = false;
                Outcome::effects(vec![])
            }
            _ => Outcome::quiet(),
        }
    }

    fn perform_confirm(&mut self, confirm: Intent) -> Outcome {
        match confirm {
            Intent::ResetSetting => {
                let Some(row) = self.selected_setting() else {
                    return Outcome::quiet();
                };
                let label = self.setting_label(row);
                if self.reset_setting(row) {
                    let message = format!("{}: {}", label, self.strings.settings_reset_done());
                    self.toast(Toast::success(message));
                }
                Outcome::effects(vec![])
            }
            Intent::DeleteSession => {
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::DeleteSession { session_id }])
            }
            Intent::ArchiveSession => {
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::ArchiveSession { session_id }])
            }
            Intent::RevokeSelectedDevice => {
                let index = self.selection_for(Scope::Devices);
                let Some(device) = self.management_data.devices.get(index) else {
                    return Outcome::quiet();
                };
                let device_id = device.device_id.clone();
                Outcome::effects(vec![Effect::RevokeDevice {
                    device_id,
                    reason: None,
                }])
            }
            other => self.perform(other),
        }
    }

    fn submit_prompt(&mut self, field: PromptField, value: String, title: String) -> Outcome {
        let trimmed = value.trim().to_string();
        match field {
            PromptField::RenameSession => {
                // The prompt was opened on a list row, and the cursor cannot
                // move while it is up: the row it pointed at is what is renamed.
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                if trimmed.is_empty() {
                    self.toast(Toast::warning(
                        self.strings.session_title_label().to_string(),
                    ));
                    return Outcome::quiet();
                }
                Outcome::effects(vec![Effect::RenameSession {
                    session_id,
                    title: trimmed,
                }])
            }
            PromptField::WorkspacePath => Outcome::effects(vec![Effect::OpenWorkspace {
                draft_id: self.new_draft_id.clone(),
                navigation_serial: self.navigation_serial,
                root_path: trimmed,
            }]),
            PromptField::ImagePath => {
                match self.attach_image_path(&trimmed) {
                    Ok(label) => {
                        let message = format!("{} {label}", self.strings.image_attached());
                        self.toast(Toast::success(message));
                    }
                    Err(error) => self.toast(Toast::warning(error)),
                }
                Outcome::effects(vec![])
            }
            PromptField::ProviderSecret => {
                let index = self.selection_for(Scope::Providers);
                let Some(profile) = self.management_data.providers.get(index) else {
                    return Outcome::quiet();
                };
                let profile_id = profile.id.clone();
                // The drafted secret is dropped as soon as it is submitted.
                Outcome::effects(vec![Effect::WriteProviderSecret {
                    profile_id,
                    secret: value,
                }])
            }
            PromptField::ProviderEndpoint => {
                let index = self.selection_for(Scope::Providers);
                let Some(profile) = self.management_data.providers.get(index) else {
                    return Outcome::quiet();
                };
                let profile_id = profile.id.clone();
                Outcome::effects(vec![Effect::RenameProfile {
                    profile_id,
                    name: trimmed,
                }])
            }
            PromptField::McpServerName | PromptField::SkillName | PromptField::PromptName => {
                if trimmed.is_empty() {
                    return Outcome::quiet();
                }
                let index = self.selection_for(Scope::Management);
                let entry = match field {
                    PromptField::McpServerName => {
                        self.management_data.mcp.get(index).map(|server| {
                            crate::app::ManagementEntryEdit::Mcp {
                                server_id: server.id.clone(),
                                display_name: trimmed,
                            }
                        })
                    }
                    PromptField::SkillName => self.management_data.skills.get(index).map(|skill| {
                        crate::app::ManagementEntryEdit::Skill {
                            skill_id: skill.id.clone(),
                            display_name: trimmed,
                        }
                    }),
                    PromptField::PromptName => {
                        self.management_data.prompts.get(index).map(|prompt| {
                            crate::app::ManagementEntryEdit::Prompt {
                                prompt_id: prompt.id.clone(),
                                display_name: trimmed,
                            }
                        })
                    }
                    _ => None,
                };
                let _ = title;
                match entry {
                    Some(entry) => Outcome::effects(vec![Effect::UpdateEntry { entry }]),
                    None => Outcome::quiet(),
                }
            }
            PromptField::DeviceRevokeReason => {
                let index = self.selection_for(Scope::Devices);
                let Some(device) = self.management_data.devices.get(index) else {
                    return Outcome::quiet();
                };
                let device_id = device.device_id.clone();
                Outcome::effects(vec![Effect::RevokeDevice {
                    device_id,
                    reason: (!trimmed.is_empty()).then_some(trimmed),
                }])
            }
            PromptField::RunOptionValue => {
                // The key rode along with the prompt; taking it here is what
                // makes a stale submission impossible.
                let Some(key) = self.run_option_prompt.take() else {
                    return Outcome::quiet();
                };
                if trimmed.is_empty() {
                    self.toast(Toast::warning(
                        self.strings.runtime_option_required().to_string(),
                    ));
                    return Outcome::quiet();
                }
                // Sent like every other row of the view it returns to: a typed
                // option is one of the switcher's rows, and the reader came back
                // to it to see the value in effect.
                let outcome = self.apply_run_option(&key, Some(trimmed.to_string()));
                self.overlay = Some(Overlay::RuntimePicker {
                    view: RuntimePickerView::Options,
                    selected: self.runtime_picker.option_row,
                });
                outcome
            }
        }
    }

    // ---- focused helpers --------------------------------------------------

    fn go_back(&mut self) -> Outcome {
        if self.overlay.is_some() {
            self.cancel_runtime_picker();
            self.overlay = None;
            return Outcome::effects(vec![]);
        }
        // The dock is the innermost panel above the composer, so it folds away
        // before the page around it does.
        if self.dock_open {
            self.dock_open = false;
            self.dock_selection = None;
            return Outcome::effects(vec![]);
        }
        // A copied selection stays highlighted until the reader dismisses it,
        // and `Esc` is that dismissal before it means anything else.
        if self.clear_text_selection() {
            return Outcome::effects(vec![]);
        }
        // The search bar is the next innermost surface on the agent page.
        if self.close_search() {
            return Outcome::effects(vec![]);
        }
        // A settings sub-mode is undone before the page is left: `Esc` in the
        // chooser puts the old value back rather than closing the screen.
        if self.page == Page::Settings && !self.settings.view.is_browse() {
            if self.cancel_setting_pick() || self.cancel_setting_edit() {
                return Outcome::effects(vec![]);
            }
            self.leave_settings_filter(true);
            return Outcome::effects(vec![]);
        }
        if self.filtering {
            self.filtering = false;
            self.filter.clear();
            return Outcome::effects(vec![]);
        }
        // The composer is where the keyboard lands when a session is opened, so
        // it must not be a room with no door: `Esc` from the composer moves the
        // keyboard off the draft rather than only being swallowed by it. `Tab`
        // is what walks the panes, and the draft is left where it was. A
        // session's own page skips this hop, so leaving one costs one press.
        if self.focus == Focus::Composer && !self.page.is_session_page() {
            self.focus = Focus::Main;
            return Outcome::effects(vec![]);
        }
        // Pages are a history, not a hierarchy: `Esc` returns to the page this
        // one was opened from, in the order the reader opened them. A reader who
        // opened Usage while writing a new session lands back in that draft —
        // never on a session list they never opened.
        if self.navigate_back() {
            return Outcome::effects(vec![]);
        }
        // Nothing was opened before this page, so the two halves of where the
        // client starts still step between each other; any other page falls back
        // to the list, which is the page every session is opened from.
        match self.page {
            Page::Sessions => {
                self.navigate_to(Page::NewSession);
                self.focus = Focus::Composer;
            }
            Page::NewSession => {
                self.navigate_to(Page::Sessions);
                self.focus = Focus::Main;
            }
            _ => self.select_global(vibex_ui::shell::GlobalDestination::Sessions),
        }
        Outcome::effects(vec![])
    }

    fn contextual_cancel(&mut self) -> Outcome {
        // Every `Ctrl+C` spends the arming first, whatever it goes on to do.
        // The second press has to be a press that had nothing else to cancel,
        // or a reader who cleared a draft and then pressed again would quit
        // without ever having been asked.
        let armed = self.spend_quit();
        if self.overlay.is_some() {
            self.cancel_runtime_picker();
            self.overlay = None;
            return Outcome::effects(vec![]);
        }
        if !self.composer.is_empty() {
            self.composer.clear();
            self.history.reset();
            let message = self.strings.composer_draft_cleared().to_string();
            self.toast(Toast::info(message));
            return Outcome::effects(vec![]);
        }
        if self.is_turn_running() {
            let Some(session_id) = self.selected_session_id().cloned() else {
                return Outcome::quiet();
            };
            // Stopping a turn stops the continuation that was about to follow
            // it: continuing a turn the reader just cancelled is the one thing
            // auto-continue must never do.
            let now_ms = vibex_core::unix_timestamp_ms();
            self.auto_continue.pause(&session_id, now_ms);
            return Outcome::effects(vec![Effect::Interrupt { session_id }]);
        }
        // Nothing left to cancel, so the reader is asking to leave. Twice: a
        // single press lands here by accident often enough — a `Ctrl+C` aimed
        // at a turn that had just finished — that quitting on it would be the
        // client taking a misfire for an instruction. The first press answers
        // with the hint on the status band and the second one leaves.
        if armed {
            self.should_quit = true;
            return Outcome::effects(vec![]);
        }
        self.arm_quit();
        Outcome::effects(vec![])
    }

    fn is_turn_running(&self) -> bool {
        self.active_session().is_some_and(|session| {
            matches!(
                session.state,
                AgentSessionState::Running | AgentSessionState::NeedsInput
            )
        })
    }

    fn session_can_continue(&self) -> bool {
        self.active_session().is_some_and(|session| {
            !matches!(
                session.state,
                AgentSessionState::Running | AgentSessionState::Archived
            )
        })
    }

    fn move_selection(&mut self, delta: i64) -> Outcome {
        let scope = if self.page.is_composing_page() {
            Scope::Agent
        } else if self.overlay.is_some() {
            Scope::Overlay
        } else {
            self.page.scope()
        };
        if self.page == Page::Settings {
            self.move_setting_selection(delta);
            return Outcome::effects(vec![]);
        }
        let count = self.page_row_count();
        if count == 0 {
            return Outcome::quiet();
        }
        let current = self.selection_for(scope) as i64;
        let mut next = (current + delta).clamp(0, count as i64 - 1) as usize;
        if scope == Scope::Agent {
            while self
                .transcript
                .block(next)
                .is_some_and(|block| block.group.is_hidden())
            {
                let candidate = next as i64 + delta.signum();
                if candidate < 0 || candidate >= count as i64 {
                    return Outcome::quiet();
                }
                next = candidate as usize;
            }
        }
        self.set_selection(scope, next);
        if scope == Scope::Agent {
            self.scroll.follow = false;
            let offset = self.transcript.offset_of_block(next);
            self.scroll.offset = offset;
        }
        Outcome::effects(vec![])
    }

    /// Whether the page in front of the reader is the session list, whose
    /// window the scroll keys move rather than the transcript's behind it.
    fn scrolls_session_list(&self) -> bool {
        self.page == Page::Sessions && self.overlay.is_none()
    }

    /// How far a page key moves the session list.
    ///
    /// A page leaves a margin of overlap, the way the transcript's does: the
    /// reader keeps the row they were reading instead of hunting for it.
    fn session_page_step(&self) -> usize {
        self.session_band_rows.saturating_sub(2).max(1)
    }

    fn scroll_by(&mut self, direction: i64, page: bool) {
        // A page leaves a margin of overlap so the reader keeps their place;
        // half a screen is the smallest step that reads as a scroll rather than
        // a jump.
        let step = if page {
            self.transcript_band_rows.saturating_sub(4).max(1)
        } else {
            (self.transcript_band_rows / 2).max(1)
        };
        self.scroll_lines(direction * step as i64);
    }

    fn set_overlay_scroll(&mut self, value: usize) {
        self.overlay = match self.overlay.clone() {
            Some(Overlay::Help {
                query, collapsed, ..
            }) => Some(Overlay::Help {
                query,
                selected: value,
                collapsed,
            }),
            Some(Overlay::BlockDetails { block, .. }) => Some(Overlay::BlockDetails {
                block,
                scroll: value,
            }),
            Some(Overlay::TextView { title, body, .. }) => Some(Overlay::TextView {
                title,
                body,
                scroll: value,
            }),
            other => other,
        };
    }

    /// The effects that open a session: the snapshot, which carries the
    /// session's own runtime selection, and the catalogue the composer names it
    /// from.
    ///
    /// One place owns it because two do the same thing: the reader opening a
    /// session from the list, and the answer to a creation landing in the
    /// session it made. Without the second, a session created on one Agent was
    /// described by the catalogue's first entry — the composer named another
    /// Agent, and the switcher offered to move the session to it.
    pub fn open_session_effects(&mut self, session_id: VibexSessionId) -> Outcome {
        // The controller issues the load ticket before the fetch, so the
        // snapshot is applied in the generation it was requested for and live
        // events for the session stop being stale.
        let ticket = match self.agent.begin_session_load(session_id.clone()) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.toast(Toast::danger(error.message));
                return Outcome::quiet();
            }
        };
        self.open_session(session_id.clone());
        self.sync_transcript();
        let mut effects = vec![Effect::OpenSession { session_id, ticket }];
        if self.runtime_options.is_none() && self.runtime_catalog_available() {
            effects.push(Effect::ListRuntimeOptions);
        }
        Outcome::effects(effects)
    }

    /// Land on the page a new session starts on.
    ///
    /// A page rather than a dialog: the reader asked to start writing, and the
    /// first thing worth showing them is the prompt they will write in with the
    /// runtime it will be sent through named beside it. No session exists yet —
    /// the send creates it — so this only moves the reader to the page, and the
    /// workspace list comes along because the page names the directory.
    ///
    /// The page keeps the choices already made for it — the Agent and the
    /// directory — because those belong to the page, not to the session behind
    /// it: a reader who picked a directory from the list picked it for the
    /// session they are about to write, and dropping it here is how a choice
    /// went nowhere. A creation consumes both (`enter_creating_session`), so the
    /// page a reader returns to afterwards names the directory of the session
    /// they are in rather than one chosen for a session that already exists.
    fn begin_new_session(&mut self) -> Outcome {
        self.navigate_to(Page::NewSession);
        if !self.new_draft_has_content_or_choices() && !self.failed_creations.is_empty() {
            self.restore_failed_creation();
        }
        self.focus = Focus::Composer;
        let mut effects = vec![Effect::ListWorkspaces];
        // The page names the Agent the session will be created with and offers
        // its run options, so the catalogue is read on the way in rather than
        // only when the switcher is opened — otherwise the page spends its
        // first moments saying the runtime is unavailable. It is a read; a
        // failure leaves the page naming the Agent it can.
        if self.runtime_options.is_none() && self.runtime_catalog_available() {
            effects.push(Effect::ListRuntimeOptions);
        }
        // The list of existing sessions is read once on the way in even though
        // this page draws none of it: the client's own background work — an
        // auto-continue countdown, an unread mark, whether a session's turn is
        // still running — is derived from it, and none of that may be wrong
        // because the reader happened to start on the prompt. Once it is read
        // the state is kept, so `n` from the list does not read it again.
        if self.agent.state.sessions.value.is_none() {
            effects.push(Effect::ListSessions {
                include_archived: self.show_archived,
            });
        }
        Outcome::effects(effects)
    }

    /// Ask for a session and keep the draft that will open it.
    ///
    /// The title is not asked for: it comes from the message, which is where a
    /// title comes from anyway, and a reader who has just written a paragraph
    /// should not then be asked to name it. The message waits in
    /// [`App::pending_creations`] until the authority acknowledges its reserved
    /// session identity. Another request cannot take its message or choices.
    ///
    /// The Agent is not asked for either, at this point: a session is created
    /// *with* one, so a page that cannot name one reads the catalogue rather
    /// than handing the choice to the runtime, which would land the session on
    /// an Agent the reader never saw. A backend that publishes no catalogue at
    /// all keeps the runtime's own fallback — there is nothing here to name.
    fn create_session_from_draft(&mut self) -> Outcome {
        if self.composer.is_empty() {
            self.toast(Toast::warning(self.strings.composer_empty().to_string()));
            return Outcome::quiet();
        }
        // Read before the page is left: the selection the page names is what the
        // session is created with, and `enter_creating_session` is what clears
        // the view of the session it came from.
        let runtime = self.page_runtime_selection();
        if runtime.is_none() && self.runtime_catalog_available() {
            let message = if self.runtime_options.is_none() {
                // The catalogue is on its way: the draft waits for the Agent
                // that will answer it.
                self.strings.runtime_catalogue_reading()
            } else {
                // A catalogue that publishes nothing is not going to answer.
                self.strings.runtime_no_agents()
            };
            self.toast(Toast::warning(message.to_string()));
            return if self.runtime_options.is_none() {
                Outcome::effects(vec![Effect::ListRuntimeOptions])
            } else {
                Outcome::quiet()
            };
        }
        let outgoing = self.composer.take_outgoing();
        self.completion = None;
        // Read through the page's own answer (`new_session_workspace`), so what
        // the page names is what the creation carries — including the directory
        // the client was started in when nothing has chosen one.
        let workspace_root = self.new_session_workspace();
        let request_id = self.new_draft_id.clone();
        // Select the reserved identity synchronously. There is no fetch until
        // the authority acknowledges creation, and no old timeline survives.
        self.agent.select_pending_session(request_id.clone());
        self.enter_creating_session(request_id.clone());
        let projected = self.wire_attachments(&outgoing.images);
        let send_id =
            self.mark_send_dispatched(Some(&request_id), outgoing.text.clone(), projected);
        self.pending_creations.insert(
            request_id.clone(),
            PendingCreation {
                outgoing,
                runtime: runtime.clone(),
                workspace_root: workspace_root.clone(),
                send_id,
            },
        );
        Outcome::effects(vec![Effect::CreateSession {
            request_id,
            workspace_root,
            title: None,
            runtime,
        }])
    }

    fn begin_rename_session(&mut self) -> Outcome {
        // The row the cursor is on: the reader asked to rename the session they
        // pointed at, which is not necessarily the one open behind the list.
        let Some(session_id) = self.list_session_target() else {
            return Outcome::quiet();
        };
        let title = self
            .session_title(&session_id)
            .unwrap_or_else(|| self.strings.session_untitled().to_string());
        self.overlay = Some(Overlay::Prompt {
            title: self.strings.session_rename().to_string(),
            field: PromptField::RenameSession,
            value: title,
        });
        Outcome::effects(vec![])
    }

    fn begin_revoke_device(&mut self) -> Outcome {
        self.overlay = Some(Overlay::Confirm {
            title: self.strings.devices_revoke().to_string(),
            body: self.strings.devices_confirm_revoke().to_string(),
            confirm: Intent::RevokeSelectedDevice,
        });
        Outcome::effects(vec![])
    }

    fn open_approval(&mut self) {
        if self.overlay.is_none() && !self.approvals().is_empty() {
            self.overlay = Some(Overlay::Approval { selected: 0 });
        }
    }

    fn open_elicitation(&mut self) {
        if self.overlay.is_none() && !self.elicitations().is_empty() {
            self.overlay = Some(Overlay::Elicitation { field: 0 });
        }
    }

    fn resolve_approval(&mut self, requested: PermissionResponseKind, index: usize) -> Outcome {
        let approvals = self.approvals();
        let Some(approval) = approvals.get(index) else {
            return Outcome::quiet();
        };
        let Some(response) = response_for(&approval.response_options, requested) else {
            return Outcome::quiet();
        };
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let request_id = approval.request_id.clone();
        let resolution = PermissionResolution {
            request_id: request_id.clone(),
            session_id: session_id.clone(),
            response,
            responder_device_id: None,
            provider_resolution_id: None,
            note: None,
            resolved_at_ms: vibex_core::unix_timestamp_ms(),
        };
        // Optimistically grey the card out; a failure rolls it back with an
        // error toast from the worker.
        self.mark_pending(format!("permission:{}", request_id.as_str()));
        Outcome::effects(vec![Effect::ResolvePermission {
            session_id,
            request_id,
            resolution,
        }])
    }

    fn current_elicitation_fields(&self) -> usize {
        self.elicitations()
            .first()
            .map(|surface| surface.request.fields.len())
            .unwrap_or(0)
    }

    fn submit_elicitation(&mut self, field: usize) -> Outcome {
        let elicitations = self.elicitations();
        let Some(surface) = elicitations.first() else {
            return Outcome::quiet();
        };
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let request = surface.request.clone();
        let draft = self.elicitation_draft.clone();
        let mut answers = std::collections::BTreeMap::new();
        for (index, definition) in request.fields.iter().enumerate() {
            if let Some(answer) = draft.answer_for(&definition.id, index, definition, field) {
                answers.insert(definition.id.clone(), answer);
            }
        }
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers,
            responder_device_id: None,
            resolved_at_ms: vibex_core::unix_timestamp_ms(),
        };
        if let Err(error) = request.validate_resolution(&resolution) {
            self.toast(Toast::danger(error.message));
            return Outcome::quiet();
        }
        self.mark_pending(format!("elicitation:{}", request.id.as_str()));
        Outcome::effects(vec![Effect::ResolveElicitation {
            session_id,
            request_id: request.id.clone(),
            resolution,
        }])
    }

    fn mark_pending(&mut self, key: String) {
        self.pending.insert(key, ());
    }

    fn submit_composer(&mut self) -> Outcome {
        if self.composer.is_empty() {
            self.toast(Toast::warning(self.strings.composer_empty().to_string()));
            return Outcome::quiet();
        }
        // The composing page has no session yet — asking it for one is what the
        // send *does* — so it is answered before the session checks, not after.
        if self.page == Page::NewSession {
            return self.create_session_from_draft();
        }
        let Some(session_id) = self.selected_session_id().cloned() else {
            self.toast(Toast::warning(self.strings.sessions_empty().to_string()));
            return Outcome::quiet();
        };
        if !self.supports(BackendOperation::AgentSendMessage) {
            self.toast(Toast::warning(
                self.strings.toast_action_unavailable().to_string(),
            ));
            return Outcome::quiet();
        }
        let outgoing = self.composer.take_outgoing();
        let (text, images) = (outgoing.text, outgoing.images);
        self.completion = None;
        // A message written while a turn is running is held rather than sent:
        // the runtime would have to interleave it with work already in flight.
        if self.turn_reads_running() || self.session_is_uncreated(&session_id) {
            self.enqueue(session_id.clone(), text, images);
            self.toast(Toast::info(self.strings.queue_held().to_string()));
            return Outcome::effects(vec![]);
        }
        self.history.push(text.clone());
        self.scroll.follow = true;
        let attachments = self.wire_attachments(&images);
        // The runtime owns the timeline, so its copy of this message is a round
        // trip away. Until it lands the send is projected locally — a reader who
        // pressed Enter must not be left wondering whether it worked.
        let send_id =
            self.mark_send_dispatched(Some(&session_id), text.clone(), attachments.clone());
        // Sending is a reactivation: whatever was suspended is wanted again.
        self.auto_continue.resume(&session_id);
        Outcome::effects(vec![Effect::SendMessage {
            session_id,
            send_id,
            correlation_id: self.pending_sends[&send_id].correlation_id.clone(),
            text,
            attachments,
        }])
    }

    fn steer_composer(&mut self) -> Outcome {
        if self.composer.is_empty() {
            self.toast(Toast::warning(self.strings.composer_empty().to_string()));
            return Outcome::quiet();
        }
        // The composing page has no turn to interject into — the message it
        // holds is what creates one — and the session behind it is not the
        // reader's target: steering that one would deliver the draft to
        // whatever they were looking at before, which is exactly the Agent they
        // did not choose. The draft stays in the box for `Enter` to send.
        if !self.page_owns_session() {
            self.toast(Toast::warning(
                self.strings.composer_steer_no_session().to_string(),
            ));
            return Outcome::quiet();
        }
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let outgoing = self.composer.take_outgoing();
        let (text, images) = (outgoing.text, outgoing.images);
        self.history.push(text.clone());
        // Remote seats have no steering RPC, so the worker falls back to
        // interrupt + resend and says so.
        let fallback = self.seat != crate::view::SeatKind::Authority;
        if fallback {
            self.toast(Toast::info(
                self.strings.composer_steer_unavailable().to_string(),
            ));
        }
        let attachments = self.wire_attachments(&images);
        Outcome::effects(vec![Effect::SteerMessage {
            session_id,
            text,
            attachments,
            fallback_to_resend: fallback,
        }])
    }

    fn move_completion(&mut self, delta: i64) {
        let Some(menu) = self.completion.as_mut() else {
            return;
        };
        let count = menu.items.len();
        if count == 0 {
            return;
        }
        let current = menu.selected as i64;
        menu.selected = (current + delta).rem_euclid(count as i64) as usize;
    }

    fn accept_completion(&mut self) -> Outcome {
        let Some(menu) = self.completion.clone() else {
            return Outcome::quiet();
        };
        let Some(item) = menu.items.get(menu.selected) else {
            self.completion = None;
            return Outcome::quiet();
        };
        let insertion = item.insert.clone();
        self.composer.replace_trigger_word(menu.start, &insertion);
        self.completion = None;
        Outcome::effects(vec![])
    }

    /// Recompute the completion menu after an edit.
    pub fn refresh_completion(&mut self) -> Option<(CompletionTrigger, String)> {
        match self.composer.active_trigger() {
            Some((trigger, start, query)) => {
                let end = self.composer.cursor();
                match self.completion.as_mut() {
                    Some(menu) if menu.trigger == trigger && menu.start == start => {
                        menu.end = end;
                        let filtered = menu.filtered(&query);
                        if let Some(first) = filtered.first() {
                            menu.selected = menu.selected.min(menu.items.len() - 1);
                            let _ = first;
                        }
                    }
                    _ => {
                        self.completion = Some(CompletionMenu {
                            trigger,
                            start,
                            end,
                            items: Vec::new(),
                            selected: 0,
                            loading: true,
                        });
                    }
                }
                Some((trigger, query))
            }
            None => {
                self.completion = None;
                None
            }
        }
    }

    /// Whether the runtime catalogue can be read at all.
    fn runtime_catalog_available(&self) -> bool {
        self.availability(BackendOperation::AgentSwitchRuntime)
            .is_available()
    }

    fn open_runtime_picker(&mut self) -> Outcome {
        // A backend that cannot move a session between runtimes gets an
        // explanation rather than an overlay whose Enter does nothing.
        if let Some(outcome) = self.unavailable_outcome(BackendOperation::AgentSwitchRuntime) {
            return outcome;
        }
        // A session the authority has not created yet has no runtime to move:
        // the one its creation carries is already on its way. The key bar
        // advertises `Ctrl+G` here all the same, so the reader who presses it
        // in that window is told which state they are waiting for instead of
        // watching the panel fail to appear.
        if self.page_shows_uncreated_session() {
            let message = if self.page_session_is_being_created() {
                self.strings.runtime_session_creating()
            } else {
                self.strings.runtime_session_uncreated()
            };
            self.toast(Toast::warning(message.to_string()));
            return Outcome::quiet();
        }
        self.runtime_picker_target = Some(self.runtime_target());
        if self.runtime_options.is_none() {
            // Opening the picker is what asked for the catalogue; the message
            // that carries it is what opens the overlay.
            self.runtime_picker_pending = true;
            return Outcome::effects(vec![Effect::ListRuntimeOptions]);
        }
        self.show_runtime_picker();
        Outcome::effects(vec![])
    }

    /// Open the picker on the choice the session is already on.
    ///
    /// It answers "what am I on" before it asks "what do you want", and a
    /// session with no matching entry (a catalogue that moved under it) opens
    /// on the first row rather than on nothing. The search from a previous
    /// visit is dropped: `Ctrl+G` is a fresh question, and a filter left over
    /// from the last one would answer it with a shorter list than the reader
    /// asked for.
    pub fn show_runtime_picker(&mut self) {
        self.runtime_picker.query.clear();
        self.runtime_picker.filtering = false;
        self.runtime_picker.working = None;
        // The picker opens as a list of Agents: the group the reader is already
        // on is the one that is open.
        self.fold_runtime_picker_to_current();
        self.show_runtime_picker_view(RuntimePickerView::Choices);
    }

    /// Put one view of the switcher on screen.
    ///
    /// The catalogue opens on the entry the picker is on, unfolded so the row is
    /// really there; the run options open where the reader left them, because a
    /// reader who came back to a setting came back to *that* setting. A view
    /// with nothing in it says so rather than showing an empty box or swallowing
    /// the key.
    pub fn show_runtime_picker_view(&mut self, view: RuntimePickerView) -> Outcome {
        if self.runtime_picker_target.is_none() {
            self.runtime_picker_target = Some(self.runtime_target());
        }
        if !self.runtime_picker_is_current() {
            return Outcome::quiet();
        }
        let selected = match view {
            RuntimePickerView::Choices => self.runtime_picker_current_row(),
            RuntimePickerView::Options => {
                let count = self.picker_run_options().len();
                if count == 0 {
                    self.toast(Toast::warning(
                        self.strings.runtime_no_run_options().to_string(),
                    ));
                    return Outcome::quiet();
                }
                self.begin_runtime_edit(None);
                self.runtime_picker.option_row.min(count - 1)
            }
        };
        self.overlay = Some(Overlay::RuntimePicker { view, selected });
        Outcome::effects(vec![])
    }

    /// The catalogue row the switcher opens on: the entry the picker is on,
    /// wherever the pinned sections and the folded groups have put it.
    ///
    /// The group is unfolded first: a folded group hides the entry the reader is
    /// on, and a picker that opens on the heading above it makes the reader open
    /// the group to see what they are already using.
    fn runtime_picker_current_row(&mut self) -> usize {
        let Some(index) = self.picker_runtime_option_index() else {
            return self.first_runtime_picker_entry_row();
        };
        if let Some(agent_id) = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.get(index))
            .map(|option| option.selection.agent_id.clone())
        {
            self.runtime_picker.folded.remove(&agent_id);
        }
        self.runtime_picker_rows()
            .iter()
            .position(|row| row.entry() == Some(index))
            .unwrap_or_else(|| self.first_runtime_picker_entry_row())
    }

    /// The first row a catalogue cursor may rest on: a heading has nothing to
    /// choose, so an empty catalogue of headers still opens on an entry.
    fn first_runtime_picker_entry_row(&self) -> usize {
        self.runtime_picker_rows()
            .iter()
            .position(|row| !row.is_heading())
            .unwrap_or(0)
    }

    /// One key of the runtime switcher.
    ///
    /// The two views answer different questions and so take different keys: the
    /// catalogue walks a tree of headings and entries, and the run options stage
    /// values onto the entry the page is on.
    fn perform_runtime_picker(
        &mut self,
        intent: Intent,
        view: RuntimePickerView,
        selected: usize,
    ) -> Outcome {
        match view {
            RuntimePickerView::Choices => self.perform_runtime_catalogue(intent, selected),
            RuntimePickerView::Options => self.perform_runtime_options(intent, selected),
        }
    }

    fn perform_runtime_catalogue(&mut self, intent: Intent, selected: usize) -> Outcome {
        match intent {
            Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                match self.runtime_picker_rows().get(selected).cloned() {
                    Some(RuntimePickerRow::Entry { index, .. }) => self.choose_runtime_entry(index),
                    // A heading is a control rather than a choice, and the one
                    // thing it controls is whether its entries are on screen.
                    Some(RuntimePickerRow::Agent {
                        agent_id, folded, ..
                    }) => {
                        self.fold_runtime_group(&agent_id, !folded);
                        Outcome::effects(vec![])
                    }
                    _ => Outcome::quiet(),
                }
            }
            Intent::SelectNext => self.step_runtime_cursor(1),
            Intent::SelectPrevious => self.step_runtime_cursor(-1),
            Intent::ScrollPageUp => {
                let page = self.runtime_picker_page_rows();
                self.step_runtime_cursor(-page)
            }
            Intent::ScrollPageDown => {
                let page = self.runtime_picker_page_rows();
                self.step_runtime_cursor(page)
            }
            Intent::ScrollToTop => self.set_runtime_cursor(0),
            Intent::ScrollToBottom => {
                let last = self.runtime_picker_rows().len().saturating_sub(1);
                self.set_runtime_cursor(last)
            }
            Intent::BeginFilter => {
                self.runtime_picker.filtering = true;
                Outcome::effects(vec![])
            }
            Intent::OverlayFoldOpen => self.fold_runtime_row(selected, false),
            Intent::OverlayFoldClosed => self.fold_runtime_row(selected, true),
            Intent::StarRuntimeModel => self.star_runtime_row(selected),
            Intent::ManageRuntimeAccount => self.open_runtime_account(selected),
            // `Tab` is the two halves of the switcher: the catalogue, and the
            // run options of the entry the reader has picked. They are views
            // rather than one list because the catalogue is as long as the
            // machine has models.
            Intent::OverlayNextField => {
                // The run options belong to the row under the cursor — the
                // preview line under the list already says so — so opening them
                // takes that row as their subject rather than the entry the page
                // happens to be on. A row that cannot be chosen is refused
                // exactly as `Enter` refuses it.
                match self.runtime_picker_edit_seed(selected) {
                    None => Outcome::quiet(),
                    Some(seed) => {
                        self.begin_runtime_edit(seed);
                        self.show_runtime_picker_view(RuntimePickerView::Options)
                    }
                }
            }
            _ => Outcome::quiet(),
        }
    }

    fn perform_runtime_options(&mut self, intent: Intent, selected: usize) -> Outcome {
        match intent {
            Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                let Some(option) = self.picker_run_options().into_iter().nth(selected) else {
                    return Outcome::quiet();
                };
                match option.kind {
                    RunOptionKind::Text => self.open_run_option_prompt(option, selected),
                    RunOptionKind::Choice | RunOptionKind::Toggle => {
                        // The cursor starts on the value in effect, which on a
                        // fresh selection is the Agent's own default.
                        let choices = self.run_option_choices(&option);
                        let at = choices
                            .iter()
                            .position(|(value, _)| option.is_selected_value(value.as_deref()))
                            .unwrap_or(0);
                        self.overlay = Some(Overlay::RunOptionValues {
                            row: selected,
                            selected: at,
                            option,
                        });
                        Outcome::effects(vec![])
                    }
                }
            }
            Intent::ResetRunOption => self.reset_run_option_row(selected),
            Intent::SelectNext => self.step_runtime_option_cursor(1),
            Intent::SelectPrevious => self.step_runtime_option_cursor(-1),
            Intent::ScrollPageUp => self.step_runtime_option_cursor(-(PICKER_PAGE_ROWS as isize)),
            Intent::ScrollPageDown => self.step_runtime_option_cursor(PICKER_PAGE_ROWS as isize),
            Intent::ScrollToTop => self.set_runtime_option_cursor(0),
            Intent::ScrollToBottom => {
                let last = self.picker_run_options().len().saturating_sub(1);
                self.set_runtime_option_cursor(last)
            }
            // `Tab` toggles the two views in both directions: the footer names
            // the one it moves to, and a key that only ever went one way is how
            // a reader ends up pressing `Esc` to get back.
            Intent::OverlayNextField | Intent::OverlayPreviousField => {
                self.show_runtime_picker_view(RuntimePickerView::Choices)
            }
            _ => Outcome::quiet(),
        }
    }

    /// Move the switcher's cursor without a key.
    ///
    /// The wheel and the pointer land on the same rows the arrows do, through
    /// the same reducer: a mouse gesture that took a different path is how a
    /// cursor ends up somewhere a key cannot reach.
    pub fn step_runtime_picker(&mut self, delta: isize) -> Outcome {
        match self.overlay {
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                ..
            }) => self.step_runtime_cursor(delta),
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                ..
            }) => self.step_runtime_option_cursor(delta),
            _ => Outcome::quiet(),
        }
    }

    /// Put the switcher's cursor on one row, opting to activate it.
    ///
    /// Clicking is select-then-activate, the contract every other list in this
    /// client keeps: the first click moves the cursor so the keys that follow
    /// act where the reader pointed, and a second click on the same row does
    /// what `Enter` would.
    pub fn select_runtime_picker_row(&mut self, row: usize, activate: bool) -> Outcome {
        let Some(Overlay::RuntimePicker { view, .. }) = self.overlay.clone() else {
            return Outcome::quiet();
        };
        self.overlay = Some(Overlay::RuntimePicker {
            view,
            selected: row,
        });
        if view == RuntimePickerView::Options {
            self.runtime_picker.option_row = row;
        }
        if activate {
            return self.perform_runtime_picker(Intent::ConfirmOverlay, view, row);
        }
        Outcome::effects(vec![])
    }

    /// Put the palette's cursor on one of its rows, opting to run it.
    ///
    /// The pointer and the arrows move the same cursor through the same
    /// reducer: a mouse path of its own is how the row that is lit and the
    /// command that runs drift apart. Hovering selects, and a click runs — the
    /// palette is a menu, and a row the pointer has already taken the highlight
    /// on answers the press that lands on it; asking for a second click would
    /// make the first one look broken.
    pub fn select_palette_row(&mut self, row: usize, activate: bool) -> Outcome {
        let Some(Overlay::Palette { query, selected }) = self.overlay.clone() else {
            return Outcome::quiet();
        };
        // A frame can outlive the list it published — the query changed under
        // the pointer, or the list scrolled — so the row is clamped to what the
        // palette holds now rather than trusted.
        let count = self.palette_entries(&query).len();
        if count == 0 {
            return Outcome::quiet();
        }
        let row = row.min(count - 1);
        if row != selected {
            self.overlay = Some(Overlay::Palette {
                query,
                selected: row,
            });
        }
        if activate {
            return self.perform(Intent::ConfirmOverlay);
        }
        if row == selected {
            Outcome::quiet()
        } else {
            Outcome::effects(vec![])
        }
    }

    /// How far a page key moves the catalogue: the rows the last frame drew,
    /// or a readable guess when the picker has not been on screen yet.
    fn runtime_picker_page_rows(&self) -> isize {
        match self.runtime_picker.page_rows {
            0 => PICKER_PAGE_ROWS as isize,
            rows => rows as isize,
        }
    }

    /// The row the switcher's cursor is on, whichever view is up.
    fn runtime_picker_selected(&self) -> usize {
        match self.overlay {
            Some(Overlay::RuntimePicker { selected, .. }) => selected,
            _ => 0,
        }
    }

    /// Move the catalogue cursor by `delta` rows, wrapping at both ends.
    ///
    /// Wrapping both ways is deliberate: the two directions had different rules
    /// once, and a list that wraps downwards but stops at the top reads as a bug
    /// rather than as a boundary.
    fn step_runtime_cursor(&mut self, delta: isize) -> Outcome {
        let count = self.runtime_picker_rows().len();
        if count == 0 {
            return Outcome::quiet();
        }
        let next = (self.runtime_picker_selected() as isize + delta).rem_euclid(count as isize);
        self.set_runtime_cursor(next as usize)
    }

    fn set_runtime_cursor(&mut self, row: usize) -> Outcome {
        if let Some(Overlay::RuntimePicker { view, .. }) = self.overlay.clone() {
            self.overlay = Some(Overlay::RuntimePicker {
                view,
                selected: row,
            });
        }
        Outcome::effects(vec![])
    }

    /// Move the run-option cursor by `delta` rows, wrapping at both ends.
    fn step_runtime_option_cursor(&mut self, delta: isize) -> Outcome {
        let count = self.picker_run_options().len();
        if count == 0 {
            return Outcome::quiet();
        }
        let next = (self.runtime_picker.option_row as isize + delta).rem_euclid(count as isize);
        self.set_runtime_option_cursor(next as usize)
    }

    /// Move the run-option cursor and remember where it went, so `Tab` back into
    /// the view lands on the row the reader was working on.
    fn set_runtime_option_cursor(&mut self, row: usize) -> Outcome {
        self.runtime_picker.option_row = row;
        self.set_runtime_cursor(row)
    }

    /// Fold or unfold one Agent's group.
    ///
    /// The catalogue's one control: a group is a heading and the entries under
    /// it, and folding one is what lets a reader keep the Agents they are not
    /// choosing from out of the way.
    pub fn fold_runtime_group(&mut self, agent_id: &vibex_core::AgentId, folded: bool) {
        if folded {
            self.runtime_picker.folded.insert(agent_id.clone());
        } else {
            self.runtime_picker.folded.remove(agent_id);
        }
    }

    /// `←`/`→` on a catalogue row: fold it, unfold it, or step in and out.
    fn fold_runtime_row(&mut self, selected: usize, close: bool) -> Outcome {
        let rows = self.runtime_picker_rows();
        match rows.get(selected) {
            Some(RuntimePickerRow::Agent {
                agent_id, folded, ..
            }) => {
                let agent_id = agent_id.clone();
                let folded = *folded;
                if close {
                    self.fold_runtime_group(&agent_id, true);
                    Outcome::effects(vec![])
                } else if folded {
                    self.fold_runtime_group(&agent_id, false);
                    Outcome::effects(vec![])
                } else {
                    // Already open: `→` steps into the group rather than doing
                    // nothing, which is what it means in every other tree.
                    self.step_runtime_cursor(1)
                }
            }
            // `←` on an entry steps out to the Agent heading it belongs under.
            Some(RuntimePickerRow::Entry { .. }) if close => {
                match rows[..selected]
                    .iter()
                    .rposition(|row| matches!(row, RuntimePickerRow::Agent { .. }))
                {
                    Some(heading) => self.set_runtime_cursor(heading),
                    None => Outcome::quiet(),
                }
            }
            _ => Outcome::quiet(),
        }
    }

    /// Star or unstar the highlighted model.
    ///
    /// The pinned section is drawn above the groups, so starring moves the row
    /// the reader was on. The cursor follows the *entry* rather than the index
    /// it happened to sit at, which is what makes the key usable twice in a row.
    fn star_runtime_row(&mut self, selected: usize) -> Outcome {
        let Some(index) = self.runtime_picker_entry(selected) else {
            return Outcome::quiet();
        };
        let Some(option) = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.get(index))
            .cloned()
        else {
            return Outcome::quiet();
        };
        let starred = self.runtime_prefs.toggle_favorite(&option);
        self.runtime_prefs.save(self.runtime_path.as_deref());
        let row = self
            .runtime_picker_rows()
            .iter()
            .position(|row| row.entry() == Some(index))
            .unwrap_or(0);
        self.set_runtime_cursor(row);
        let marker = if starred { "★" } else { "☆" };
        let message = format!("{marker} {} · {}", option.agent_label, option.model_label);
        self.toast(Toast::success(message));
        Outcome::effects(vec![])
    }

    /// Send the reader to the highlighted Agent's account in the management
    /// pages.
    ///
    /// The catalogue can say that an account needs attention; it cannot sign in.
    /// The Agents page owns that, and the Agent waits on the app for the list to
    /// arrive so the page lands on the row the reader asked about instead of on
    /// whichever Agent happened to be first.
    fn open_runtime_account(&mut self, selected: usize) -> Outcome {
        let Some(option) = self.runtime_picker_highlighted_entry(selected) else {
            return Outcome::quiet();
        };
        let agent_id = option.selection.agent_id.clone();
        let message = format!(
            "{}: {}",
            self.strings.runtime_manage_account(),
            option.agent_label
        );
        self.overlay = None;
        self.pending_agent_focus = Some(agent_id);
        self.navigate_to(Page::Agents);
        self.toast(Toast::info(message));
        Outcome::effects(vec![Effect::ListAgents])
    }

    /// The selection the run options open on when the reader asks for them from
    /// the catalogue row `row`.
    ///
    /// The options belong to the row under the cursor: its Agent, account and
    /// model, run the way it ran last time. A heading is its entries seen at
    /// once, so it answers with the Agent's own model — the one the reader last
    /// used with it, or the first it still publishes. A row that cannot be
    /// chosen is refused exactly as `Enter` refuses it: an outer `None` leaves
    /// the view where it is, and an inner one means the row names nothing to
    /// edit, so the page's own selection is what the options open on.
    fn runtime_picker_edit_seed(&mut self, row: usize) -> Option<Option<SessionRuntimeSelection>> {
        if !self.runtime_picker_is_current() {
            return None;
        }
        let highlighted = self.runtime_picker_rows().get(row).cloned();
        if let Some(RuntimePickerRow::Agent { agent_id, .. }) = highlighted {
            let selection = self.runtime_options.as_ref().and_then(|catalog| {
                self.runtime_prefs
                    .preferred(catalog, Some(&agent_id))
                    .or_else(|| {
                        catalog
                            .options
                            .iter()
                            .find(|option| {
                                option.availability
                                    == vibex_core::RuntimeOptionAvailability::Available
                                    && option.selection.agent_id == agent_id
                            })
                            .map(|option| self.runtime_prefs.with_remembered_options(option))
                    })
            });
            let Some(selection) = selection else {
                self.toast(Toast::warning(
                    self.strings.runtime_unavailable().to_string(),
                ));
                return None;
            };
            return Some(Some(selection));
        }
        let Some(option) = self.runtime_picker_highlighted_entry(row) else {
            // No catalogue row to take — a pinned section's title: the view
            // still turns, and answers with the page's own entry.
            return Some(None);
        };
        if option.availability != vibex_core::RuntimeOptionAvailability::Available {
            self.toast(Toast::warning(
                self.strings.runtime_unavailable().to_string(),
            ));
            return None;
        }
        // How this entry ran last time comes with it: the reader picked the
        // Agent and the model, and the thinking depth they set on it is part of
        // that answer rather than a second question.
        Some(Some(self.runtime_prefs.with_remembered_options(&option)))
    }

    /// Ask for a free-text run option's value.
    ///
    /// The open prompt carries its field but not the option it belongs to, so
    /// the key waits on the app for the one submission the prompt can make. The
    /// row rides along too, so the answer lands back on the row that asked.
    fn open_run_option_prompt(&mut self, option: RunOption, row: usize) -> Outcome {
        self.run_option_prompt = Some(option.key.clone());
        self.runtime_picker.option_row = row;
        let title = option.label.clone();
        let value = option
            .explicit
            .clone()
            .or_else(|| option.resolved.as_ref().map(|value| value.value.clone()))
            .unwrap_or_default();
        self.overlay = Some(Overlay::Prompt {
            title,
            field: PromptField::RunOptionValue,
            value,
        });
        Outcome::effects(vec![])
    }

    /// Put a selection under the run options.
    ///
    /// The seed is what the reader pointed at — the row they chose, or the entry
    /// the catalogue cursor is on. `None` keeps whatever the options are already
    /// editing (a row that names no entry is not a reason to drop the entry the
    /// reader picked), or takes the page's own selection when they are opening
    /// on it for the first time.
    fn begin_runtime_edit(&mut self, seed: Option<SessionRuntimeSelection>) {
        if !self.runtime_picker_is_current() {
            return;
        }
        match seed {
            Some(selection) => self.runtime_picker.working = Some(selection),
            None => {
                if self.runtime_picker.working.is_none() {
                    self.runtime_picker.working = self.page_runtime_selection();
                }
            }
        }
    }

    /// Apply one run option to the selection being edited, at once.
    ///
    /// There is no staged copy to send later: a run option is a setting on the
    /// Agent the reader is looking at, and one that only lived in the client
    /// would be a lie about the session it names. So a row that changes is a row
    /// that is sent — one switch per change, and never one for a gesture the
    /// reader did not make. The catalogue is the authority on what a value may
    /// be: one it no longer publishes is refused here rather than travelling to
    /// the runtime as a switch it would reject. `None` clears the override,
    /// which is what the value list's `Default` row means.
    fn apply_run_option(&mut self, key: &RunOptionKey, value: Option<String>) -> Outcome {
        if !self.runtime_picker_is_current() {
            return Outcome::quiet();
        }
        let Some(selection) = self.picker_selection() else {
            return Outcome::quiet();
        };
        let Some(option) = self
            .picker_run_options()
            .into_iter()
            .find(|option| &option.key == key)
        else {
            return Outcome::quiet();
        };
        let accepted = match (key, value.as_deref()) {
            (_, None) => true,
            (RunOptionKey::Feature(id), Some(value)) => self
                .runtime_option_for(&selection)
                .and_then(|entry| entry.features.iter().find(|feature| &feature.id == id))
                .is_some_and(|feature| feature.accepts_value(value)),
            (_, Some(value)) => option
                .values
                .iter()
                .any(|candidate| candidate.value == value),
        };
        if !accepted {
            return Outcome::quiet();
        }
        let mut working = selection.clone();
        match key {
            RunOptionKey::ReasoningEffort => working.reasoning_effort = value,
            RunOptionKey::Mode => working.mode_id = value,
            RunOptionKey::Feature(id) => match value {
                Some(value) => {
                    working.config_values.insert(id.clone(), value);
                }
                None => {
                    working.config_values.remove(id);
                }
            },
        }
        self.runtime_picker.working = Some(working.clone());
        self.apply_picker_selection(working)
    }

    /// Send the selection the picker is on to the thing it belongs to.
    ///
    /// A page showing a session moves *that* session; a page showing none — the
    /// session list, the management pages, the page where a session is being
    /// written — has nothing to move, so the choice becomes the next session's.
    /// The page decides, never "is a session selected?", because the client
    /// keeps a session selected behind every one of those pages and moving it is
    /// exactly what the reader did not ask for.
    fn apply_picker_selection(&mut self, selection: SessionRuntimeSelection) -> Outcome {
        self.remember_runtime_selection(&selection);
        self.completion = None;
        if matches!(self.runtime_picker_target, Some(ComposerTarget::Draft(_))) {
            self.new_session_runtime = Some(selection);
            return Outcome::effects(vec![]);
        }
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        self.guard(
            BackendOperation::AgentSwitchRuntime,
            Effect::SwitchRuntime {
                session_id,
                selection,
            },
        )
    }

    /// Choose one catalogue entry and put how it runs in front of the reader.
    ///
    /// Choosing an Agent and saying how it runs are one errand: sending the
    /// reader back to the catalogue — or out of the picker and in again — just
    /// to reach the run options costs them a second visit to a surface they are
    /// already standing in. An entry with nothing to tune closes the picker,
    /// because there is nothing left to ask.
    fn choose_runtime_entry(&mut self, index: usize) -> Outcome {
        let Some(selection) = self.selection_for_catalog_entry(index) else {
            return Outcome::quiet();
        };
        self.runtime_picker.working = Some(selection.clone());
        // The group the reader just moved onto is the one that stays open: the
        // catalogue behind the run options answers "what am I on" the same way
        // it did when it opened.
        self.fold_runtime_picker_to_current();
        let mut outcome = self.apply_picker_selection(selection);
        if self.picker_run_options().is_empty() {
            self.overlay = None;
            return outcome;
        }
        let view = self.show_runtime_picker_view(RuntimePickerView::Options);
        outcome.effects.extend(view.effects);
        outcome
    }

    /// The selection one catalogue entry stands for, when it can be chosen.
    ///
    /// How this entry ran last time comes back with it, and it is written down
    /// again: the entry the reader just chose is now the one a new session
    /// starts from.
    fn selection_for_catalog_entry(&mut self, index: usize) -> Option<SessionRuntimeSelection> {
        if !self.runtime_picker_is_current() {
            return None;
        }
        let option = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.get(index))
            .cloned()?;
        if option.availability != vibex_core::RuntimeOptionAvailability::Available {
            self.toast(Toast::warning(
                self.strings.runtime_unavailable().to_string(),
            ));
            return None;
        }
        Some(self.runtime_prefs.with_remembered_options(&option))
    }

    /// Put the highlighted run option back on the Agent's own default.
    fn reset_run_option_row(&mut self, selected: usize) -> Outcome {
        let Some(option) = self.picker_run_options().into_iter().nth(selected) else {
            return Outcome::quiet();
        };
        // Nothing is overridden, so there is nothing to put back: the row is
        // already showing the Agent's own value, and saying "Default" over it
        // would be noise rather than an answer.
        if option.explicit.is_none() {
            return Outcome::quiet();
        }
        let message = format!("{}: {}", option.label, self.strings.runtime_default());
        self.toast(Toast::info(message));
        self.apply_run_option(&option.key, None)
    }

    /// Choose one of the pinned recent rows by its number.
    ///
    /// A digit is `Enter` on a row the reader cannot be bothered to walk to, so
    /// it does what `Enter` does: the entry is chosen, and its run options are
    /// what the picker shows next.
    ///
    /// Answers `None` when the digit is not one of them, which is what lets the
    /// caller fall through to the binding table: a key the reviewer did not
    /// mean as a shortcut must not be swallowed by one.
    pub fn quick_pick_runtime(&mut self, digit: char) -> Option<Outcome> {
        let position = digit.to_digit(10)? as usize;
        if position == 0 {
            return None;
        }
        let rows = self.runtime_picker_rows();
        let row = rows.iter().position(|row| {
            matches!(row, RuntimePickerRow::Entry { quick: Some(quick), .. } if *quick == digit)
        })?;
        let index = rows[row].entry()?;
        Some(self.choose_runtime_entry(index))
    }

    /// What entering the session list reads: the sessions themselves, and the
    /// arrangement the authority draws its own sidebar from.
    ///
    /// They travel together because the list is the arrangement applied to the
    /// sessions; a client that fetched one without the other would draw a tree
    /// that is already stale on arrival.
    fn session_page_effects(&self) -> Vec<Effect> {
        vec![
            Effect::ListSessions {
                include_archived: self.show_archived,
            },
            // Project rows are named by the runtime's project records, which is
            // what the desktop's sidebar shows; the session list alone would
            // spell the directory instead.
            Effect::ListWorkspaces,
            Effect::LoadSidebarOrganization,
        ]
    }

    fn refresh_current_page(&mut self) -> Outcome {
        match self.page {
            Page::NewSession | Page::Sessions => Outcome::effects(self.session_page_effects()),
            Page::Agent => Outcome::effects(self.refresh_timeline().into_iter().collect()),
            Page::Devices => Outcome::effects(vec![Effect::ListDevices]),
            Page::Providers => Outcome::effects(vec![Effect::ListProfiles]),
            Page::Agents => Outcome::effects(vec![Effect::ListAgents]),
            Page::Mcp => Outcome::effects(vec![Effect::ListMcp]),
            Page::Skills => Outcome::effects(vec![Effect::ListSkills]),
            Page::Prompts => Outcome::effects(vec![Effect::ListPrompts]),
            Page::Usage => Outcome::effects(vec![Effect::LoadUsage]),
            Page::Management | Page::Settings | Page::Help => Outcome::effects(vec![]),
        }
    }

    fn toggle_selected_entry(&mut self) -> Outcome {
        let index = self.selection_for(self.page.scope());
        match self.page {
            Page::Mcp => match self.management_data.mcp.get(index) {
                Some(server) => Outcome::effects(vec![Effect::ToggleMcp {
                    server_id: server.id.clone(),
                    enabled: server.status != vibex_core::McpServerStatus::Enabled,
                }]),
                None => Outcome::quiet(),
            },
            Page::Skills => match self.management_data.skills.get(index) {
                Some(skill) => Outcome::effects(vec![Effect::ToggleSkill {
                    skill_id: skill.id.clone(),
                    enabled: skill.status != vibex_core::SkillStatus::Enabled,
                }]),
                None => Outcome::quiet(),
            },
            Page::Prompts => match self.management_data.prompts.get(index) {
                Some(prompt) => Outcome::effects(vec![Effect::TogglePrompt {
                    prompt_id: prompt.id.clone(),
                    enabled: prompt.status != vibex_core::PromptStatus::Enabled,
                }]),
                None => Outcome::quiet(),
            },
            _ => Outcome::quiet(),
        }
    }

    fn begin_edit_entry(&mut self) -> Outcome {
        let field = match self.page {
            Page::Mcp => PromptField::McpServerName,
            Page::Skills => PromptField::SkillName,
            Page::Prompts => PromptField::PromptName,
            _ => return Outcome::quiet(),
        };
        self.overlay = Some(Overlay::Prompt {
            title: self.strings.management_mcp().to_string(),
            field,
            value: String::new(),
        });
        Outcome::effects(vec![])
    }

    fn install_selected_agent(&mut self, uninstall: bool) -> Outcome {
        let index = self.selection_for(Scope::Management);
        let Some(agent) = self.management_data.agents.get(index) else {
            return Outcome::quiet();
        };
        let agent_id = agent.id.clone();
        if uninstall {
            return self.guard(
                BackendOperation::ManagementAgents,
                Effect::UninstallAgent { agent_id },
            );
        }
        self.guard(
            BackendOperation::ManagementAgents,
            Effect::InstallAgent { agent_id },
        )
    }

    fn list_agent_auth(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Management);
        let Some(agent) = self.management_data.agents.get(index) else {
            return Outcome::quiet();
        };
        let agent_id = agent.id.clone();
        self.guard(
            BackendOperation::AgentAuthRead,
            Effect::ListAgentAuth { agent_id },
        )
    }

    fn logout_selected_agent(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Management);
        let Some(agent) = self.management_data.agents.get(index) else {
            return Outcome::quiet();
        };
        let agent_id = agent.id.clone();
        self.guard(
            BackendOperation::AgentAuthManage,
            Effect::LogoutAgent { agent_id },
        )
    }

    fn activate_provider_profile(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Providers);
        let Some(profile) = self.management_data.providers.get(index) else {
            return Outcome::quiet();
        };
        let profile_id = profile.id.clone();
        self.guard(
            BackendOperation::ManagementProfileSelect,
            Effect::SelectProfile { profile_id },
        )
    }

    fn test_provider_profile(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Providers);
        let Some(profile) = self.management_data.providers.get(index) else {
            return Outcome::quiet();
        };
        let profile_id = profile.id.clone();
        self.guard(
            BackendOperation::ManagementProviderProfileTest,
            Effect::TestProviderProfile { profile_id },
        )
    }

    fn fetch_provider_models(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Providers);
        let Some(profile) = self.management_data.providers.get(index) else {
            return Outcome::quiet();
        };
        let profile_id = profile.id.clone();
        self.guard(
            BackendOperation::ManagementProviderModelFetch,
            Effect::FetchProviderModels { profile_id },
        )
    }

    /// `Enter` on the settings page: what it does depends on the row's kind.
    fn activate_setting(&mut self) -> Outcome {
        let Some(row) = self.selected_setting() else {
            return Outcome::quiet();
        };
        match crate::settings::definition(row).kind {
            crate::settings::SettingKind::Choice => {
                self.begin_setting_pick(row);
                Outcome::effects(vec![])
            }
            crate::settings::SettingKind::Toggle => {
                self.step_setting(1);
                Outcome::effects(vec![])
            }
            crate::settings::SettingKind::Text => {
                self.begin_setting_edit(row);
                Outcome::effects(vec![])
            }
            crate::settings::SettingKind::Action => self.open_setting_action(row),
            crate::settings::SettingKind::ReadOnly => Outcome::quiet(),
        }
    }

    /// Move the selected session through the list, one place per press.
    ///
    /// `delta` is in row-index space, so `-1` is up the screen.
    fn move_session(&mut self, delta: isize) -> Outcome {
        // The authority owns the order when an arrangement is loaded; moving
        // the local copy instead would show the reader a list nobody else has.
        match self.sidebar_move(delta) {
            Some(crate::app::SidebarMove::Remote(effect)) => {
                return Outcome::effects(vec![*effect]);
            }
            Some(crate::app::SidebarMove::Blocked) => {
                self.toast(Toast::warning(self.strings.sidebar_pinned_first()));
                return Outcome::quiet();
            }
            Some(crate::app::SidebarMove::AtEdge) => return Outcome::quiet(),
            None => {}
        }
        match self.move_session_row(delta) {
            Some(true) => Outcome::effects(vec![]),
            Some(false) => {
                self.toast(Toast::warning(self.strings.sidebar_pinned_first()));
                Outcome::quiet()
            }
            None => Outcome::quiet(),
        }
    }

    /// Run the action a settings row owns.
    fn open_setting_action(&mut self, row: crate::settings::SettingRow) -> Outcome {
        match row {
            // The bindings row opens the editor rather than only re-reading the
            // file: the reload is one key inside it, and a hint that names a
            // path is not an interface.
            crate::settings::SettingRow::Keys => {
                self.overlay = Some(Overlay::Keys {
                    query: String::new(),
                    selected: 0,
                    capturing: None,
                    message: None,
                    dirty: false,
                });
                Outcome::effects(vec![])
            }
            _ => self.perform(Intent::ReloadKeymap),
        }
    }

    /// Step a row's value without opening its chooser.
    ///
    /// A toggle flips; a choice cycles through its values, applying each one.
    /// The chooser is the deliberate path, this is the quick one.
    fn step_setting(&mut self, delta: i64) -> Outcome {
        let Some(row) = self.selected_setting() else {
            return Outcome::quiet();
        };
        match crate::settings::definition(row).kind {
            crate::settings::SettingKind::Choice | crate::settings::SettingKind::Toggle => {
                let choices = self.setting_choices(row);
                if choices.is_empty() {
                    return Outcome::quiet();
                }
                let current = choices
                    .iter()
                    .position(|choice| choice.current)
                    .unwrap_or(0) as i64;
                let next = (current + delta).rem_euclid(choices.len() as i64) as usize;
                let value = choices[next].value.clone();
                self.apply_setting_value(row, &value);
                Outcome::effects(vec![])
            }
            crate::settings::SettingKind::Action => self.open_setting_action(row),
            crate::settings::SettingKind::Text | crate::settings::SettingKind::ReadOnly => {
                Outcome::quiet()
            }
        }
    }

    /// `d`: ask before resetting a row, and carry the row through the question.
    fn begin_reset_setting(&mut self) -> Outcome {
        let Some(row) = self.selected_setting() else {
            return Outcome::quiet();
        };
        if matches!(
            crate::settings::definition(row).kind,
            crate::settings::SettingKind::ReadOnly | crate::settings::SettingKind::Action
        ) {
            return Outcome::quiet();
        }
        self.overlay = Some(Overlay::Confirm {
            title: self.strings.settings_reset_title().to_string(),
            body: format!(
                "{}: {}",
                self.setting_label(row),
                self.strings.settings_reset_confirm()
            ),
            confirm: Intent::ResetSetting,
        });
        Outcome::effects(vec![])
    }
}

/// Build the request payloads the worker needs for the agent mutations.
///
/// Kept here so the worker stays a thin transport layer and the payload shape
/// lives next to the reducer that decided to send it.
pub mod payloads {
    use super::*;

    pub fn create_session(
        workspace_root: String,
        title: Option<String>,
        runtime: vibex_core::SessionRuntimeSelection,
    ) -> MutationRequest<CreateAgentSessionRequest> {
        MutationRequest::new(CreateAgentSessionRequest {
            runtime,
            workspace_root,
            workspace_mode: WorkspaceMode::CurrentCheckout,
            title,
            safety: None,
            session_id: None,
            defer_runtime_materialization: false,
        })
    }

    pub fn send_message(
        session_id: vibex_core::VibexSessionId,
        text: String,
        attachments: Vec<vibex_core::MessageAttachment>,
        desired_runtime: vibex_core::SessionRuntimeSelection,
    ) -> MutationRequest<SendAgentMessageRequest> {
        MutationRequest::new(SendAgentMessageRequest {
            session_id,
            message_idempotency_key: RequestId::new().as_str().to_string(),
            // The submission is refused when the message's reasoning effort
            // disagrees with the selection it travels with, so the selection is
            // the single source for both: a message must never be the reason a
            // session's runtime configuration changes.
            reasoning_effort: desired_runtime.reasoning_effort.clone(),
            desired_runtime,
            text,
            attachments,
            correlation_id: None,
            delivery: vibex_core::UserMessageDelivery::Prompt,
            prompt_context: None,
            provenance: vibex_core::MessageProvenance::HumanInput,
        })
    }

    pub fn continue_turn(
        session_id: vibex_core::VibexSessionId,
    ) -> MutationRequest<ContinueAgentTurnRequest> {
        MutationRequest::new(ContinueAgentTurnRequest {
            session_id,
            correlation_id: None,
        })
    }

    pub fn rename_session(
        session_id: vibex_core::VibexSessionId,
        title: String,
    ) -> MutationRequest<RenameAgentSessionRequest> {
        MutationRequest::new(RenameAgentSessionRequest { session_id, title })
    }

    pub fn fork_session(
        session_id: vibex_core::VibexSessionId,
    ) -> MutationRequest<ForkAgentSessionRequest> {
        MutationRequest::new(ForkAgentSessionRequest {
            source_session_id: session_id,
            through_sequence: ForkAgentSessionRequest::AT_TIP,
            expected_source_end_sequence: None,
        })
    }

    pub fn steer_message(
        session_id: vibex_core::VibexSessionId,
        text: String,
        attachments: Vec<vibex_core::MessageAttachment>,
    ) -> MutationRequest<SteerAgentMessageRequest> {
        MutationRequest::new(SteerAgentMessageRequest {
            session_id,
            text,
            attachments,
            correlation_id: None,
        })
    }

    pub fn resolve_permission(
        session_id: vibex_core::VibexSessionId,
        request_id: RequestId,
        resolution: PermissionResolution,
    ) -> MutationRequest<ResolvePermissionRequest> {
        MutationRequest::new(ResolvePermissionRequest {
            session_id,
            request_id,
            resolution,
        })
    }

    pub fn resolve_elicitation(
        session_id: vibex_core::VibexSessionId,
        request_id: RequestId,
        resolution: ElicitationResolution,
    ) -> MutationRequest<ResolveElicitationRequest> {
        MutationRequest::new(ResolveElicitationRequest {
            session_id,
            request_id,
            resolution,
        })
    }

    pub fn answer_string(value: impl Into<String>) -> ElicitationAnswerValue {
        ElicitationAnswerValue::String(value.into())
    }
}

/// A locally-held draft of an elicitation form.
///
/// The answers live here rather than in the shared controller because the
/// controller models the *request*, not the in-progress edit; keeping the draft
/// next to the reducer is what lets the whole form be tested without a
/// terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElicitationDraft {
    text: std::collections::BTreeMap<String, String>,
    boolean: std::collections::BTreeMap<String, bool>,
    multi: std::collections::BTreeMap<String, Vec<String>>,
    active_text: String,
}

impl ElicitationDraft {
    pub fn set_text(&mut self, field_id: impl Into<String>, value: impl Into<String>) {
        self.text.insert(field_id.into(), value.into());
    }

    pub fn set_boolean(&mut self, field_id: impl Into<String>, value: bool) {
        self.boolean.insert(field_id.into(), value);
    }

    pub fn toggle_multi(&mut self, field_id: impl Into<String>, value: impl Into<String>) {
        let value = value.into();
        let entry = self.multi.entry(field_id.into()).or_default();
        if let Some(position) = entry.iter().position(|existing| *existing == value) {
            entry.remove(position);
        } else {
            entry.push(value);
        }
    }

    pub fn text(&self, field_id: &str) -> Option<&str> {
        self.text.get(field_id).map(String::as_str)
    }

    pub fn boolean(&self, field_id: &str) -> Option<bool> {
        self.boolean.get(field_id).copied()
    }

    pub fn multi(&self, field_id: &str) -> Vec<String> {
        self.multi.get(field_id).cloned().unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.boolean.clear();
        self.multi.clear();
        self.active_text.clear();
    }

    /// Answer for one field, using the draft where the user typed something and
    /// the field's default otherwise.
    pub fn answer_for(
        &self,
        field_id: &str,
        index: usize,
        definition: &vibex_core::ElicitationField,
        active: usize,
    ) -> Option<ElicitationAnswerValue> {
        use vibex_core::ElicitationFieldKind;
        match &definition.kind {
            ElicitationFieldKind::Text { default, .. } => {
                let value = self
                    .text(field_id)
                    .map(str::to_string)
                    .or_else(|| {
                        // The field being edited reads from the live editor.
                        (index == active)
                            .then(|| self.active_text.clone())
                            .filter(|value| !value.is_empty())
                    })
                    .or_else(|| default.clone())?;
                Some(ElicitationAnswerValue::String(value))
            }
            ElicitationFieldKind::Number { default, .. } => {
                let value = self
                    .text(field_id)
                    .map(str::to_string)
                    .or_else(|| default.clone())?;
                Some(ElicitationAnswerValue::Number(value))
            }
            ElicitationFieldKind::Integer { default, .. } => {
                let value = self
                    .text(field_id)
                    .and_then(|value| value.parse::<i64>().ok())
                    .or(*default)?;
                Some(ElicitationAnswerValue::Integer(value))
            }
            ElicitationFieldKind::Boolean { default } => {
                let value = self.boolean(field_id).or(*default)?;
                Some(ElicitationAnswerValue::Boolean(value))
            }
            ElicitationFieldKind::MultiSelect { .. } => {
                let values = self.multi(field_id);
                (!values.is_empty()).then_some(ElicitationAnswerValue::StringArray(values))
            }
            _ => None,
        }
    }

    /// Bind the live text editor to `field_id`.
    pub fn begin_text(&mut self, field_id: &str) {
        self.active_text = self.text.get(field_id).cloned().unwrap_or_default();
    }

    pub fn active_text_mut(&mut self) -> &mut String {
        &mut self.active_text
    }

    /// Flush the live editor back into the stored answer.
    pub fn commit_text(&mut self, field_id: &str) {
        let value = std::mem::take(&mut self.active_text);
        self.text.insert(field_id.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::VibexSessionId;

    /// An app whose Agent capabilities are advertised, which the disconnected
    /// facade is not: the reducer must be able to issue an older-page request
    /// for the trigger under test.
    fn capable_app() -> App {
        app_with_capabilities(vibex_backend::BackendCapabilitySnapshot::desktop_native_v1())
    }

    /// An app over one capability snapshot, which is what decides whether the
    /// picker reads this machine or asks the authority.
    fn app_with_capabilities(snapshot: vibex_backend::BackendCapabilitySnapshot) -> App {
        let backend = std::sync::Arc::new(vibex_backend::DisconnectedBackend);
        let facade = vibex_backend::BackendFacade::new(
            snapshot,
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
        );
        let mut app = App::new(
            facade,
            crate::app::AppOptions {
                sidebar_path: None,
                runtime_path: None,
                preferences_path: None,
                ..Default::default()
            },
        );
        app.resize(100, 30);
        app
    }

    fn history_item(session_id: &VibexSessionId, sequence: i64) -> vibex_core::TimelineItem {
        vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: 1_000 + sequence,
            source: vibex_core::TimelineSource::User,
            kind: vibex_core::TimelineItemKind::UserMessage,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload: vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: format!("message {sequence}"),
                attachments: Vec::new(),
                ..Default::default()
            }),
        }
    }

    /// A session the list can open, with every field the wire requires.
    fn openable_session(id: &str) -> vibex_core::AgentSession {
        vibex_core::AgentSession {
            id: VibexSessionId::parse(id).expect("valid id"),
            title: "a session".to_string(),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/tmp/vibex-reduce-workspace".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1_759_237_920_000,
            updated_at_ms: 1_759_251_200_000,
            last_message_at_ms: 1_759_251_200_000,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    #[test]
    fn a_sent_message_carries_its_selection_and_nothing_else() {
        // The submission is refused unless the message's reasoning effort agrees
        // with the selection it travels with, and the selection is what decides
        // which Agent the turn runs on. So the payload must not disagree with
        // the session's own selection, and it must not carry a setting the
        // session never chose.
        let mut selection = vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse("codex").expect("agent id"),
            vibex_core::ProviderProfileId::new(),
            "gpt-5",
        );
        selection.reasoning_effort = Some("high".to_string());
        let request = super::payloads::send_message(
            VibexSessionId::new(),
            "hello".to_string(),
            Vec::new(),
            selection.clone(),
        );
        assert_eq!(request.payload.desired_runtime, selection);
        assert_eq!(
            request.payload.reasoning_effort, selection.reasoning_effort,
            "the message disagrees with the runtime selection it carries"
        );
    }

    #[test]
    fn entering_a_session_from_the_list_takes_the_keyboard_into_the_composer() {
        let mut app = capable_app();
        app.agent
            .apply_sessions(Ok(vec![openable_session("session_enter0001")]))
            .expect("sessions apply");
        app.navigate_to(Page::Sessions);
        // Row 0 is the workspace heading; the session is one below it.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        let outcome = app.perform(Intent::EnterSession);
        assert!(
            !outcome.effects.is_empty(),
            "entering the session issued no load: {outcome:?}"
        );
        assert_eq!(app.page, Page::Agent);
        assert_eq!(
            app.focus,
            crate::app::Focus::Composer,
            "the navigation reset the focus out of the composer"
        );
        // The composer's info line names the Agent and model, which needs the
        // catalogue: it is read on the way in, not only when the picker opens.
        assert!(
            outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListRuntimeOptions)),
            "entering the session did not read the runtime catalogue: {outcome:?}"
        );
        assert!(
            !app.runtime_picker_pending,
            "a catalogue read for the info line must not open the picker"
        );
    }

    #[test]
    fn the_switcher_reads_the_catalogue_then_opens_on_the_current_choice() {
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        // The reader is *in* a session: the picker moves the session its page
        // shows, so the choice it opens on is that session's.
        app.agent.state.selected_session_id = Some(VibexSessionId::new());
        app.navigate_to(Page::Agent);
        assert!(app.runtime_options.is_none());

        // Nothing is loaded yet, so opening the switcher asks for the catalogue
        // and waits: a modal with no rows would be a dead end.
        let requested = app.perform(Intent::SwitchAgentRuntime);
        assert_eq!(app.overlay, None);
        assert!(app.runtime_picker_pending);
        assert!(matches!(
            requested.effects.as_slice(),
            [Effect::ListRuntimeOptions]
        ));

        // When it lands, the picker opens on the session's own entry.
        app.runtime_options = Some(runtime_catalog());
        let desired = app.runtime_options.as_ref().unwrap().options[1]
            .selection
            .clone();
        app.agent
            .state
            .runtime_selection
            .resolve(vibex_core::AgentSessionRuntimeSelectionState {
                desired: desired.clone(),
                effective: desired,
                status: vibex_core::SessionRuntimeSelectionStatus::Ready,
                session_revision: 2,
                selection_revision: 3,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            });
        app.show_runtime_picker();
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: entry_row(&app, 1)
            })
        );
    }

    #[test]
    fn escape_from_a_session_returns_to_the_list_with_the_draft_kept() {
        // The composer is where the keyboard lands when a session opens, so
        // `Esc` has to be able to leave the page from it: a reader who could
        // not get back to the session list was stuck in the session. The list
        // is the page the session was opened from, which is what `Esc` returns
        // to, so the reader walks in the way they really do.
        let mut app = capable_app();
        app.navigate_to(Page::Sessions);
        app.navigate_to(Page::Agent);
        app.focus = crate::app::Focus::Composer;
        app.composer.set_text("an unsent draft");

        let outcome = app.perform(Intent::Back);
        assert!(outcome.effects.is_empty());
        assert_eq!(app.page, Page::Sessions);
        assert_eq!(
            app.composer.text(),
            "an unsent draft",
            "leaving the session threw the draft away"
        );
    }

    #[test]
    fn escape_walks_back_through_the_pages_that_were_opened() {
        // The pages a reader opened are a history, not a hierarchy: a reader
        // writing a new session who opens the Usage page comes back to the
        // draft, with the keyboard where they left it — not to a session list
        // they never opened, which is where a fixed hierarchy sent them.
        let mut app = capable_app();
        assert_eq!(app.page, Page::NewSession);
        app.composer.insert_str("a first thought");

        app.perform(Intent::GotoUsage);
        assert_eq!(app.page, Page::Usage);

        let outcome = app.perform(Intent::Back);
        assert!(outcome.effects.is_empty());
        assert_eq!(
            app.page,
            Page::NewSession,
            "Esc left the page the Usage page was opened from"
        );
        assert_eq!(app.focus, crate::app::Focus::Composer);
        assert_eq!(app.composer.text(), "a first thought");

        // The step after that is the page the draft page itself came from, and
        // the opening page's pair still steps between the two of them.
        app.perform(Intent::Back);
        assert_eq!(app.page, Page::Sessions);
    }

    #[test]
    fn a_panel_returns_to_the_page_that_opened_it() {
        // The management sections are pages of their own, so the way back from
        // one is the page that listed it rather than the session list.
        let mut app = capable_app();
        app.navigate_to(Page::Management);
        app.navigate_to(Page::Providers);

        app.perform(Intent::Back);
        assert_eq!(app.page, Page::Management);
        app.perform(Intent::Back);
        assert_eq!(app.page, Page::NewSession);
    }

    /// A runtime catalogue with two Agents, the second one unavailable.
    fn runtime_catalog() -> vibex_core::SessionRuntimeOptionCatalog {
        let option = |agent: &str, model: &str, availability| vibex_core::SessionRuntimeOption {
            selection: vibex_core::SessionRuntimeSelection::provider(
                vibex_core::AgentId::parse(agent).expect("agent id"),
                vibex_core::ProviderProfileId::new(),
                model,
            ),
            agent_label: agent.to_string(),
            auth_source_label: "bal".to_string(),
            model_label: model.to_string(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability,
        };
        vibex_core::SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: vec![
                option(
                    "claude",
                    "claude-sonnet",
                    vibex_core::RuntimeOptionAvailability::Available,
                ),
                option(
                    "codex",
                    "gpt-5",
                    vibex_core::RuntimeOptionAvailability::RequiresConfiguration,
                ),
            ],
        }
    }

    #[test]
    fn the_runtime_picker_opens_on_the_choice_the_session_is_on() {
        let mut app = capable_app();
        // A switch is a mutation, so the reducer only offers the picker while
        // the connection is up — which is the state an open session is in.
        app.live = crate::app::LiveState::Ready;
        let session_id = VibexSessionId::new();
        app.agent.state.selected_session_id = Some(session_id.clone());
        // The reader is on that session's own page: the picker moves what the
        // page shows, so this is the page it opens on.
        app.navigate_to(Page::Agent);
        app.runtime_options = Some(runtime_catalog());
        // The session is on the *second* entry, which is also unavailable: the
        // picker must still start there and say so rather than silently
        // offering to switch.
        app.agent
            .state
            .runtime_selection
            .resolve(vibex_core::AgentSessionRuntimeSelectionState {
                desired: app.runtime_options.as_ref().unwrap().options[1]
                    .selection
                    .clone(),
                effective: app.runtime_options.as_ref().unwrap().options[1]
                    .selection
                    .clone(),
                status: vibex_core::SessionRuntimeSelectionStatus::Ready,
                session_revision: 4,
                selection_revision: 7,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            });

        let outcome = app.perform(Intent::SwitchAgentRuntime);
        assert!(
            outcome.effects.is_empty(),
            "the catalogue was already loaded"
        );
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: entry_row(&app, 1)
            })
        );
        assert!(app.runtime_option_is_current(&app.runtime_options.as_ref().unwrap().options[1]));

        // Choosing the unavailable entry refuses instead of issuing a switch
        // the runtime would reject.
        let refused = app.perform(Intent::ConfirmOverlay);
        assert!(refused.effects.is_empty());

        // Choosing the available one asks for exactly that selection. Its group
        // is folded — the reader is not on that Agent — so the test opens it the
        // way `→` does.
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 0),
        });
        let switched = app.perform(Intent::ConfirmOverlay);
        let [
            Effect::SwitchRuntime {
                session_id: target,
                selection,
            },
        ] = switched.effects.as_slice()
        else {
            panic!("expected one runtime switch, got {switched:?}");
        };
        assert_eq!(target, &session_id);
        assert_eq!(
            selection,
            &app.runtime_options.as_ref().unwrap().options[0].selection
        );
    }

    /// A catalogue whose second entry publishes run options: a thinking ladder,
    /// two conversation modes, a switch and a free-text field.
    fn run_option_catalog() -> vibex_core::SessionRuntimeOptionCatalog {
        let value = |value: &str, label: &str| vibex_core::SessionConfigValue {
            value: value.to_string(),
            label: (!label.is_empty()).then(|| label.to_string()),
        };
        let option = |agent: &str, model: &str| vibex_core::SessionRuntimeOption {
            selection: vibex_core::SessionRuntimeSelection::provider(
                vibex_core::AgentId::parse(agent).expect("agent id"),
                vibex_core::ProviderProfileId::new(),
                model,
            ),
            agent_label: agent.to_string(),
            auth_source_label: "bal".to_string(),
            model_label: model.to_string(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability: vibex_core::RuntimeOptionAvailability::Available,
        };
        let mut agent = option("codex", "gpt-5");
        agent.reasoning_efforts = vec![value("low", "Low"), value("high", "High")];
        agent.modes = vec![value("plan", "Plan"), value("pair", "Pair")];
        agent.features = vec![
            vibex_core::SessionRuntimeFeature {
                id: "web_search".to_string(),
                label: "Web search".to_string(),
                description: None,
                kind: vibex_core::SessionRuntimeFeatureKind::Toggle,
                current_value: Some(value("true", "")),
                default_value: Some(value("true", "")),
                values: Vec::new(),
            },
            vibex_core::SessionRuntimeFeature {
                id: "notes".to_string(),
                label: "Notes".to_string(),
                description: Some("Free text the Agent reads".to_string()),
                kind: vibex_core::SessionRuntimeFeatureKind::String,
                current_value: None,
                default_value: None,
                values: Vec::new(),
            },
        ];
        vibex_core::SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: vec![option("claude", "claude-sonnet"), agent],
        }
    }

    /// Put the open session on a selection, as the runtime reports it.
    fn session_on(app: &mut App, desired: vibex_core::SessionRuntimeSelection) {
        app.live = crate::app::LiveState::Ready;
        let mut session = openable_session("session_enter0001");
        session.agent_id = desired.agent_id.clone();
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.agent
            .state
            .runtime_selection
            .resolve(vibex_core::AgentSessionRuntimeSelectionState {
                desired: desired.clone(),
                effective: desired,
                status: vibex_core::SessionRuntimeSelectionStatus::Ready,
                session_revision: 1,
                selection_revision: 1,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            });
    }

    /// An app whose loaded catalogue publishes run options, with the page
    /// either on the second entry (a session) or holding it (the composing
    /// page).
    fn app_with_run_options(page: Page) -> App {
        let mut app = capable_app();
        app.navigate_to(page);
        let catalog = run_option_catalog();
        let desired = catalog.options[1].selection.clone();
        app.runtime_options = Some(catalog);
        if page == Page::NewSession {
            app.live = crate::app::LiveState::Ready;
            app.new_session_runtime = Some(desired);
        } else {
            session_on(&mut app, desired);
        }
        app.runtime_picker_target = Some(app.runtime_target());
        app
    }

    /// The one runtime switch an outcome asked for, if it asked for one.
    fn switched(outcome: &Outcome) -> Option<&vibex_core::SessionRuntimeSelection> {
        outcome.effects.iter().find_map(|effect| match effect {
            Effect::SwitchRuntime { selection, .. } => Some(selection),
            _ => None,
        })
    }

    #[test]
    fn the_switcher_says_why_it_cannot_move_a_session_that_does_not_exist_yet() {
        // The flow that made `Ctrl+G` look broken: `n`, write, Enter. The page
        // moves into the new session at once, while the authority is still
        // creating it — and the key bar advertises the switcher the whole time.
        // There is no runtime to move yet, so the key has to say which state it
        // is waiting for rather than opening nothing at all.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.perform(Intent::NewSession);
        app.composer.insert_str("a new thing");
        let created = app.perform(Intent::SubmitComposer);
        assert!(
            created
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::CreateSession { .. })),
            "the creation was not asked for: {created:?}"
        );
        assert!(
            app.page_shows_uncreated_session(),
            "the page did not move into the creation"
        );
        assert!(app.page_session_is_being_created());

        app.toast = None;
        let refused = app.perform(Intent::SwitchAgentRuntime);
        assert!(
            refused.effects.is_empty(),
            "a session that does not exist was moved: {refused:?}"
        );
        assert_eq!(
            app.overlay, None,
            "a picker opened for a session that does not exist yet"
        );
        let toast = app.toast.as_ref().expect("the refusal was silent");
        assert_eq!(
            toast.text,
            app.strings.runtime_session_creating(),
            "the refusal named another reason"
        );

        // The authority answered: the same key opens the switcher again.
        app.pending_creations.clear();
        assert!(!app.page_shows_uncreated_session());
        app.toast = None;
        app.perform(Intent::SwitchAgentRuntime);
        assert!(
            matches!(app.overlay, Some(Overlay::RuntimePicker { .. })),
            "the switcher stayed closed after the session existed: {:?}",
            app.overlay
        );
    }

    #[test]
    fn a_creation_that_failed_says_so_instead_of_swallowing_the_key() {
        // The other half of the same window: the authority refused, and the
        // page still shows the identity that never became a session.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.perform(Intent::NewSession);
        app.composer.insert_str("a new thing");
        app.perform(Intent::SubmitComposer);
        // A follow-up typed while the creation was in flight keeps the page on
        // the identity that never became a session: there is nothing to
        // restore it *to*, so it stays where the reader is looking.
        app.composer.insert_str("and then some");
        let request_id = app.selected_session_id().cloned().expect("an identity");
        assert!(app.fail_creation(&request_id));
        assert!(app.page_shows_uncreated_session());
        assert!(!app.page_session_is_being_created());

        app.toast = None;
        let refused = app.perform(Intent::SwitchAgentRuntime);
        assert!(refused.effects.is_empty(), "{refused:?}");
        assert_eq!(app.overlay, None);
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some(app.strings.runtime_session_uncreated()),
            "the refusal named another reason"
        );
    }

    /// The catalogue row one entry is drawn on.
    ///
    /// The list is a tree — an Agent heading above its entries, and the pinned
    /// recent and starred rows above those — so an entry's row is no longer its
    /// index in the catalogue. Tests ask for the row the way the renderer finds
    /// it rather than counting headings by hand.
    fn entry_row(app: &App, index: usize) -> usize {
        app.runtime_picker_rows()
            .iter()
            .position(|row| row.entry() == Some(index))
            .expect("the entry is on the list")
    }

    /// The catalogue row one entry is drawn on, with its Agent's group open.
    ///
    /// The picker opens as a list of Agents — every group but the one in use is
    /// folded — so a test that reaches for an entry somewhere else opens its
    /// group first, which is what a reader does with `→`.
    fn open_entry_row(app: &mut App, index: usize) -> usize {
        let agent_id = app.runtime_options.as_ref().expect("catalogue").options[index]
            .selection
            .agent_id
            .clone();
        app.fold_runtime_group(&agent_id, false);
        entry_row(app, index)
    }

    /// Set one run option, the way the switcher does: the row is sent as it is
    /// chosen, so the outcome is the switch itself.
    fn stage_and_apply(app: &mut App, key: RunOptionKey, value: Option<&str>) -> Outcome {
        app.show_runtime_picker_view(RuntimePickerView::Options);
        app.apply_run_option(&key, value.map(str::to_string))
    }

    #[test]
    fn the_switcher_publishes_run_options_as_a_view_of_their_own() {
        // A catalogue is as long as the machine has models, so the run options
        // cannot be appended under it: they are the switcher's second view, and
        // `Tab` is what moves between them.
        let mut app = app_with_run_options(Page::Agent);
        // One heading per Agent on top of the entries: the catalogue row count
        // is not the catalogue's length any more.
        assert_eq!(app.runtime_picker_row_count(RuntimePickerView::Choices), 4);
        assert_eq!(app.runtime_picker_row_count(RuntimePickerView::Options), 4);

        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: entry_row(&app, 1),
        });
        let outcome = app.perform(Intent::OverlayNextField);
        assert!(outcome.effects.is_empty(), "{outcome:?}");
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 0,
            })
        );
        // And back, on the entry the page is on rather than on row 0.
        app.perform(Intent::OverlayPreviousField);
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: entry_row(&app, 1),
            })
        );

        let options = app.run_options();
        assert_eq!(
            options.iter().map(|o| o.key.clone()).collect::<Vec<_>>(),
            vec![
                RunOptionKey::ReasoningEffort,
                RunOptionKey::Mode,
                RunOptionKey::Feature("web_search".to_string()),
                RunOptionKey::Feature("notes".to_string()),
            ]
        );
        assert_eq!(options[0].label, "Thinking depth");
        assert_eq!(options[0].explicit, None);
        assert_eq!(options[0].resolved_label("Default"), "Default");
        assert_eq!(options[1].label, "Conversation mode");
        // A switch reads as a state, not as the wire's `true`, and it is the
        // Agent's published value rather than an override the reader made.
        assert_eq!(options[2].kind, RunOptionKind::Toggle);
        assert_eq!(options[2].resolved_label("Default"), "On");
        assert_eq!(options[2].explicit, None);
        assert_eq!(options[3].kind, RunOptionKind::Text);
        assert_eq!(
            options[3].description.as_deref(),
            Some("Free text the Agent reads")
        );
    }

    #[test]
    fn an_agent_with_nothing_to_tune_says_so_rather_than_showing_an_empty_view() {
        let mut app = capable_app();
        app.navigate_to(Page::Agent);
        app.live = crate::app::LiveState::Ready;
        // The first entry publishes no run options at all.
        let catalog = run_option_catalog();
        let desired = catalog.options[0].selection.clone();
        app.runtime_options = Some(catalog);
        session_on(&mut app, desired);
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: 0,
        });

        let outcome = app.perform(Intent::OverlayNextField);
        assert!(outcome.effects.is_empty());
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: 0,
            }),
            "an empty run-option view was shown"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| toast.text == app.strings.runtime_no_run_options()),
            "the key was swallowed without saying why"
        );
    }

    #[test]
    fn a_session_gets_the_run_option_it_chooses_as_a_runtime_switch() {
        let mut app = app_with_run_options(Page::Agent);
        let agent_id = app.runtime_options.as_ref().expect("catalogue").options[1]
            .selection
            .agent_id
            .clone();

        // Thinking depth is the first row of the run-option view.
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 0,
        });
        let opened = app.perform(Intent::ConfirmOverlay);
        assert!(
            opened.effects.is_empty(),
            "opening a value list issued work: {opened:?}"
        );
        let Some(Overlay::RunOptionValues {
            row: 0,
            selected: 0,
            option,
        }) = app.overlay.clone()
        else {
            panic!("the value list did not open: {:?}", app.overlay);
        };
        assert_eq!(option.key, RunOptionKey::ReasoningEffort);
        // Nothing is overridden, so the list starts on the Agent's own default.
        assert_eq!(
            app.run_option_choices(&option)
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            vec!["Default", "Low", "High"]
        );

        // Down to `High`, which is one past `Low`. Choosing it sends the switch
        // and comes back to the row it belongs to rather than closing the
        // switcher: the reader is tuning one Agent, and every row they tune
        // belongs to the same answer.
        app.perform(Intent::SelectNext);
        app.perform(Intent::SelectNext);
        let chosen = app.perform(Intent::ConfirmOverlay);
        let selection = switched(&chosen).expect("the choice did not switch the session");
        assert_eq!(selection.reasoning_effort.as_deref(), Some("high"));
        // The rest of the selection travels untouched: a run option is a
        // setting on the Agent, not a different Agent.
        assert_eq!(selection.agent_id, agent_id);
        assert_eq!(selection.mode_id, None);
        assert!(selection.config_values.is_empty());
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 0,
            })
        );
        // The row the reader came back to shows the value in effect, so a second
        // look at it is the truth about the session rather than about the client.
        assert_eq!(
            app.runtime_picker
                .working
                .as_ref()
                .and_then(|working| working.reasoning_effort.as_deref()),
            Some("high")
        );
    }

    #[test]
    fn the_default_row_clears_a_run_option_the_session_is_on() {
        let mut app = capable_app();
        app.navigate_to(Page::Agent);
        let catalog = run_option_catalog();
        let mut desired = catalog.options[1].selection.clone();
        desired.reasoning_effort = Some("high".to_string());
        app.runtime_options = Some(catalog);
        session_on(&mut app, desired);

        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 0,
        });
        app.perform(Intent::ConfirmOverlay);
        // The list opens on the value in effect, so `Default` is two steps up.
        let Some(Overlay::RunOptionValues { selected: 2, .. }) = app.overlay.clone() else {
            panic!("the value list did not open on `High`: {:?}", app.overlay);
        };
        app.perform(Intent::SelectPrevious);
        app.perform(Intent::SelectPrevious);
        let cleared = app.perform(Intent::ConfirmOverlay);
        let selection =
            switched(&cleared).expect("clearing the override did not switch the session");
        assert_eq!(selection.reasoning_effort, None);
    }

    #[test]
    fn a_toggle_run_option_is_offered_as_on_and_off() {
        let mut app = app_with_run_options(Page::Agent);

        // Web search is the third run option.
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 2,
        });
        app.perform(Intent::ConfirmOverlay);
        let Some(Overlay::RunOptionValues { option, .. }) = app.overlay.clone() else {
            panic!("the toggle did not open a value list: {:?}", app.overlay);
        };
        assert_eq!(
            app.run_option_choices(&option),
            vec![
                (None, "Default".to_string()),
                (Some("true".to_string()), "On".to_string()),
                (Some("false".to_string()), "Off".to_string()),
            ]
        );
        // `On` is already in effect through the Agent's own value, so the
        // reader's next step is the explicit `Off`.
        app.perform(Intent::SelectNext);
        app.perform(Intent::SelectNext);
        let chosen = app.perform(Intent::ConfirmOverlay);
        let selection = switched(&chosen).expect("the toggle did not switch the session");
        assert_eq!(
            selection
                .config_values
                .get("web_search")
                .map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn a_free_text_run_option_is_applied_through_the_prompt() {
        let mut app = app_with_run_options(Page::Agent);

        // Notes is the fourth run option, and it has no value list: it asks for
        // one.
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 3,
        });
        let opened = app.perform(Intent::ConfirmOverlay);
        assert!(opened.effects.is_empty());
        assert_eq!(
            app.run_option_prompt,
            Some(RunOptionKey::Feature("notes".to_string()))
        );
        assert!(matches!(
            app.overlay,
            Some(Overlay::Prompt {
                field: PromptField::RunOptionValue,
                ..
            })
        ));
        // An empty answer is refused rather than sent as a blank setting.
        let empty = app.perform(Intent::ConfirmOverlay);
        assert!(empty.effects.is_empty(), "{empty:?}");
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| toast.text == app.strings.runtime_option_required()),
            "an empty text value was refused without saying why"
        );
        // And cancelling takes the key with it, so a later prompt cannot submit
        // a value for this option.
        app.overlay = Some(Overlay::Prompt {
            title: "Notes".to_string(),
            field: PromptField::RunOptionValue,
            value: String::new(),
        });
        app.perform(Intent::Back);
        assert_eq!(app.run_option_prompt, None);
    }

    #[test]
    fn escape_from_a_value_list_returns_to_the_switcher_row() {
        let mut app = app_with_run_options(Page::Agent);

        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 1,
        });
        app.perform(Intent::ConfirmOverlay);
        assert!(matches!(
            app.overlay,
            Some(Overlay::RunOptionValues { row: 1, .. })
        ));
        app.perform(Intent::Back);
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 1,
            })
        );
        // The run-option view steps back into the catalogue, on the entry the
        // session is on rather than on nothing.
        app.perform(Intent::Back);
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: entry_row(&app, 1),
            })
        );
        // And the catalogue still closes.
        app.perform(Intent::Back);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn the_composing_page_keeps_a_run_option_for_the_session_it_creates() {
        let mut app = app_with_run_options(Page::NewSession);
        let agent_id = app.runtime_options.as_ref().expect("catalogue").options[1]
            .selection
            .agent_id
            .clone();

        // Conversation mode is the second run option. The modes are listed in
        // the catalogue's own order — `Pair` before `Plan` — after the default
        // row.
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Options,
            selected: 1,
        });
        app.perform(Intent::ConfirmOverlay);
        let Some(Overlay::RunOptionValues { option, .. }) = app.overlay.clone() else {
            panic!("the mode list did not open: {:?}", app.overlay);
        };
        assert_eq!(
            app.run_option_choices(&option)
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            vec!["Default", "Pair", "Plan"]
        );
        app.perform(Intent::SelectNext);
        app.perform(Intent::SelectNext);
        let outcome = app.perform(Intent::ConfirmOverlay);
        assert!(
            outcome.effects.is_empty(),
            "a page with no session issued work: {outcome:?}"
        );
        let chosen = app
            .new_session_runtime
            .as_ref()
            .expect("the page kept no runtime");
        assert_eq!(chosen.mode_id.as_deref(), Some("plan"));
        assert_eq!(chosen.agent_id, agent_id);
    }

    /// The one runtime a creation asked for.
    fn created_runtime(outcome: &Outcome) -> Option<vibex_core::SessionRuntimeSelection> {
        outcome.effects.iter().find_map(|effect| match effect {
            Effect::CreateSession { runtime, .. } => runtime.clone(),
            _ => None,
        })
    }

    #[test]
    fn the_composing_page_reads_the_catalogue_on_the_way_in() {
        // The page names the Agent the session will be created with and offers
        // its run options, so the catalogue is read when the page opens rather
        // than only when the switcher is asked for: the page otherwise spends
        // its first moments saying the runtime is unavailable.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        assert!(app.runtime_options.is_none());
        let opened = app.perform(Intent::NewSession);
        assert!(
            opened
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListRuntimeOptions)),
            "the page did not read the catalogue: {opened:?}"
        );

        // A catalogue already in hand is not read again.
        app.runtime_options = Some(run_option_catalog());
        let reopened = app.perform(Intent::NewSession);
        assert!(
            !reopened
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListRuntimeOptions)),
            "the page read a catalogue it already had: {reopened:?}"
        );
    }

    #[test]
    fn the_composing_page_names_the_runtime_it_will_create_with() {
        // The page answers for itself. A reader who left a session to write a
        // new one is shown the entry the creation will carry — with nothing
        // chosen on the page, the catalogue's first available entry — and never
        // the Agent of the session behind the page, which is what promised an
        // Agent the message would not go through.
        let mut app = app_with_run_options(Page::Agent);
        assert_eq!(app.composer_runtime_labels().0, "codex");

        app.navigate_to(Page::NewSession);
        assert_eq!(
            app.composer_runtime_labels(),
            ("claude".to_string(), "claude-sonnet".to_string()),
            "the page named the session behind it"
        );
        // The picker's cursor is on the same entry, so "current" answers the
        // question the page asked.
        assert_eq!(app.current_runtime_option_index(), Some(0));
        assert!(app.runtime_option_is_current(&app.runtime_options.as_ref().unwrap().options[0]));
        assert!(!app.runtime_option_is_current(&app.runtime_options.as_ref().unwrap().options[1]));

        app.composer.insert_str("a new thing");
        let created = created_runtime(&app.perform(Intent::SubmitComposer))
            .expect("the page named a runtime and created with none");
        assert_eq!(
            created.model.model_id(),
            Some("claude-sonnet"),
            "the creation did not carry what the page named"
        );
    }

    #[test]
    fn a_run_option_set_on_the_page_keeps_the_agent_it_belongs_to() {
        // Tuning the choice must not change *whose* choice it is: the page
        // still names the Agent the reader picked, and the session is created
        // with that Agent and the value.
        let mut app = app_with_run_options(Page::NewSession);
        let agent_id = app.runtime_options.as_ref().expect("catalogue").options[1]
            .selection
            .agent_id
            .clone();

        // The page is on the entry the reader picked, and the picker says so.
        assert_eq!(app.current_runtime_option_index(), Some(1));
        let applied = stage_and_apply(&mut app, RunOptionKey::ReasoningEffort, Some("high"));
        assert!(
            applied.effects.is_empty(),
            "a page with no session issued work: {applied:?}"
        );

        let chosen = app
            .new_session_runtime
            .as_ref()
            .expect("no choice was kept");
        assert_eq!(chosen.agent_id, agent_id);
        assert_eq!(chosen.reasoning_effort.as_deref(), Some("high"));
        // The tuned choice is still the entry it came from: the page names it,
        // and the picker marks it rather than falling back to another Agent.
        assert_eq!(
            app.composer_runtime_labels(),
            ("codex".to_string(), "gpt-5".to_string())
        );
        assert_eq!(app.current_runtime_option_index(), Some(1));

        // The switcher stays up after an apply — the reader may want to tune
        // the next row too — so leaving it is what puts the keyboard back in
        // the composer.
        app.perform(Intent::Back);
        app.perform(Intent::Back);
        assert_eq!(app.overlay, None);
        app.composer.insert_str("write something");
        let created = created_runtime(&app.perform(Intent::SubmitComposer))
            .expect("the page named a runtime and created with none");
        assert_eq!(created.agent_id, agent_id);
        assert_eq!(created.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn ctrl_w_on_the_new_session_page_changes_the_workspace() {
        // The page names the key that changes where the session will work, so
        // the key has to do that. It did not: the composer's own binding
        // answered `Ctrl+W` first — the shell's word kill — and a reader who
        // pressed the key the page advertised got nothing, or lost a word.
        // Resolved the way the event loop resolves it, scopes included.
        use crate::keymap::{Chord, Keymap};
        let mut app = capable_app();
        app.keymap = Keymap::built_in();
        app.live = crate::app::LiveState::Ready;
        app.perform(Intent::NewSession);
        assert_eq!(
            app.focus,
            Focus::Composer,
            "the page hands over the composer"
        );

        let chord = Chord::ctrl('w');
        let intent = app
            .keymap
            .resolve(&app.active_scopes(), chord)
            .expect("Ctrl+W is bound");
        let outcome = app.perform(intent);
        assert!(
            matches!(app.overlay, Some(Overlay::WorkspacePicker { .. })),
            "Ctrl+W did not open the workspace picker on the page that advertises it: {:?}",
            app.overlay
        );
        assert!(
            outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::BrowseDirectories { .. })),
            "the listing was not asked for: {outcome:?}"
        );
    }

    #[test]
    fn ctrl_w_still_kills_a_word_while_the_new_session_draft_has_one() {
        // The other half of the rule: the page's key only takes over where the
        // kill would have done nothing. While the reader is writing, `Ctrl+W`
        // is the shell's word kill it is everywhere else — whitespace-delimited,
        // so a path goes in one press.
        use crate::keymap::{Chord, Keymap};
        let mut app = capable_app();
        app.keymap = Keymap::built_in();
        app.live = crate::app::LiveState::Ready;
        app.perform(Intent::NewSession);
        app.composer.insert_str("look at src/net.rs");

        let intent = app
            .keymap
            .resolve(&app.active_scopes(), Chord::ctrl('w'))
            .expect("Ctrl+W is bound");
        assert_eq!(intent, Intent::DeleteWordBefore);
        app.perform(intent);
        assert_eq!(app.composer.text(), "look at ");
        assert_eq!(
            app.overlay, None,
            "a draft with text in it opened the picker instead of killing a word"
        );
    }

    #[test]
    fn the_workspace_picker_climbs_to_the_parent_directory() {
        // The picker opens on the directory the page names, and a reader who
        // wants a directory somewhere else on the machine has to be able to
        // walk out of it. The parent is a drawn row — the `..` a reader looks
        // for — and both the row and the key answer from inside the picker,
        // which is what the key did not do: it re-entered its own handler until
        // the stack ran out.
        let mut app = capable_app();
        app.workspace_path = Some("/home/peatboy/vibex-dev".to_string());
        app.perform(Intent::SwitchWorkspace);
        app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
            roots: vec![],
            path: "/home/peatboy/vibex-dev".to_string(),
            parent: Some("/home/peatboy".to_string()),
            entries: vec![vibex_core::RemoteWorkspaceDirectoryEntry {
                name: "vibex".to_string(),
                path: "/home/peatboy/vibex-dev/vibex".to_string(),
            }],
        });
        assert!(
            matches!(
                app.workspace_picker_rows().first(),
                Some(WorkspacePickerRow::Parent { path }) if path == "/home/peatboy"
            ),
            "the way up is not the first row: {:?}",
            app.workspace_picker_rows()
        );

        // `Enter` on that row climbs, and the picker stays open on the parent.
        let climbed = app.perform(Intent::ConfirmOverlay);
        assert!(
            climbed.effects.iter().any(|effect| matches!(
                effect,
                Effect::BrowseDirectories { path: Some(path) } if path == "/home/peatboy"
            )),
            "the `..` row did not ask for the parent directory: {climbed:?}"
        );
        assert!(
            matches!(app.overlay, Some(Overlay::WorkspacePicker { .. })),
            "climbing closed the picker: {:?}",
            app.overlay
        );

        // The key the footer names is the same step.
        let keyed = app.perform(Intent::WorkspaceBrowseUp);
        assert!(
            keyed.effects.iter().any(|effect| matches!(
                effect,
                Effect::BrowseDirectories { path: Some(path) } if path == "/home/peatboy"
            )),
            "the key did not ask for the parent directory: {keyed:?}"
        );

        // A directory is a door, not an answer: `Enter` on the entry behind
        // the `..` row walks into it and leaves the picker open on it.
        app.overlay = Some(Overlay::WorkspacePicker { selected: 1 });
        let opened = app.perform(Intent::ConfirmOverlay);
        assert!(
            opened.effects.iter().any(|effect| matches!(
                effect,
                Effect::BrowseDirectories { path: Some(path) }
                    if path == "/home/peatboy/vibex-dev/vibex"
            )),
            "the highlighted directory was not opened: {opened:?}"
        );
        assert!(
            matches!(app.overlay, Some(Overlay::WorkspacePicker { .. })),
            "opening a directory closed the picker: {:?}",
            app.overlay
        );

        // "Use directory" is the choice, and it takes the folder the reader
        // pointed at: the cursor is on `vibex`, so that is where the session
        // works — not the directory the picker happens to be showing.
        app.overlay = Some(Overlay::WorkspacePicker { selected: 1 });
        app.perform(Intent::WorkspaceBrowseSelect);
        assert!(app.overlay.is_none(), "the choice left the picker open");
        assert_eq!(
            app.workspace_path.as_deref(),
            Some("/home/peatboy/vibex-dev/vibex"),
            "the highlighted folder was not taken"
        );

        // On the `..` row the cursor names a step rather than a folder, so the
        // key falls back to the directory being shown — which is also all an
        // empty listing has to offer.
        app.overlay = Some(Overlay::WorkspacePicker { selected: 0 });
        app.workspace_path = None;
        app.perform(Intent::WorkspaceBrowseSelect);
        assert!(app.overlay.is_none(), "the choice left the picker open");
        assert_eq!(
            app.workspace_path.as_deref(),
            Some("/home/peatboy/vibex-dev"),
            "the directory being shown was not taken"
        );
    }

    #[test]
    fn the_workspace_picker_says_when_nothing_is_above_it() {
        // A filesystem root — and the top of a paired authority's browse roots,
        // which withholds a parent the same way — has no row above it and no
        // listing to ask for. The reader is told that instead of being handed a
        // directory that does not exist, and the drawn rows stay aligned with
        // the listing they name.
        let mut app = capable_app();
        app.overlay = Some(Overlay::WorkspacePicker { selected: 0 });
        app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
            roots: vec![],
            path: "/".to_string(),
            parent: None,
            entries: vec![vibex_core::RemoteWorkspaceDirectoryEntry {
                name: "home".to_string(),
                path: "/home".to_string(),
            }],
        });
        assert!(
            app.workspace_picker_rows()
                .iter()
                .all(|row| matches!(row, WorkspacePickerRow::Entry { .. })),
            "a root grew a row above it: {:?}",
            app.workspace_picker_rows()
        );
        let climbed = app.perform(Intent::WorkspaceBrowseUp);
        assert!(
            climbed.effects.is_empty(),
            "a root was asked for a listing above it: {climbed:?}"
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some(app.strings.workspace_at_root()),
            "the reader was not told why nothing happened"
        );

        // And the first row is the first directory, not a step that is not
        // there: entering it opens `/home`, and the key that takes a directory
        // takes that same folder when the cursor has not moved.
        let before = app.workspace_path.clone();
        let opened = app.perform(Intent::ConfirmOverlay);
        assert!(
            opened.effects.iter().any(|effect| matches!(
                effect,
                Effect::BrowseDirectories { path: Some(path) } if path == "/home"
            )),
            "the highlighted directory was not opened: {opened:?}"
        );
        assert_eq!(app.workspace_path, before, "opening a directory chose one");
        app.perform(Intent::WorkspaceBrowseSelect);
        assert_eq!(
            app.workspace_path.as_deref(),
            Some("/home"),
            "the folder under the cursor was not taken at a root"
        );
    }

    #[test]
    fn the_workspace_picker_opens_where_the_page_is_when_it_reads_this_machine() {
        // The native seat browses *this* machine — it does not report
        // `WorkspaceBrowseDirectories` — so the reader opens the picker on the
        // directory the page already names instead of walking down from home.
        // A backend that can browse the authority names its own roots, and a
        // path proposed from here could fall outside them.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut app = capable_app();
        assert!(
            !app.supports(vibex_backend::BackendOperation::WorkspaceBrowseDirectories),
            "the native snapshot claims to browse the authority"
        );
        app.workspace_path = Some(directory.path().to_string_lossy().into_owned());
        assert_eq!(
            app.workspace_picker_start().as_deref(),
            Some(directory.path().to_string_lossy().as_ref())
        );
        let opened = app.perform(Intent::SwitchWorkspace);
        assert!(
            opened.effects.iter().any(|effect| matches!(
                effect,
                Effect::BrowseDirectories { path: Some(path) }
                    if path == &directory.path().to_string_lossy()
            )),
            "the picker did not open on the page's own directory: {opened:?}"
        );

        // A directory that is not there is not proposed: the backend would
        // answer with an error instead of a listing.
        app.workspace_path = Some("/nonexistent/vibex-workspace".to_string());
        assert_eq!(app.workspace_picker_start(), None);

        // The authority's directories are the authority's to name.
        let mut snapshot = vibex_backend::BackendCapabilitySnapshot::desktop_native_v1();
        snapshot
            .workspace
            .operations
            .insert(vibex_backend::BackendOperation::WorkspaceBrowseDirectories);
        let mut remote = app_with_capabilities(snapshot);
        remote.workspace_path = Some(directory.path().to_string_lossy().into_owned());
        assert_eq!(
            remote.workspace_picker_start(),
            None,
            "a paired authority was told where to start its own listing"
        );
    }

    #[test]
    fn a_choice_made_away_from_a_session_does_not_move_one() {
        // The runtime picker is one global key, so what it moves has to be the
        // session the page is *showing* — never one the reader cannot see. A
        // choice made from the session list is a choice for the next session;
        // the session the client still has selected is left alone. This is the
        // one that used to turn an existing codex session into the Agent the
        // reader picked for the session they were about to write.
        let mut app = capable_app();
        let catalog = run_option_catalog();
        let claude = catalog.options[0].selection.clone();
        let codex = catalog.options[1].selection.clone();
        app.runtime_options = Some(catalog);
        app.new_session_runtime = None;
        session_on(&mut app, claude.clone());
        app.navigate_to(Page::Sessions);

        // The picker opens on the page's own answer rather than on the Agent of
        // the session it does not show.
        app.show_runtime_picker();
        assert_eq!(app.current_runtime_option_index(), Some(0));
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 1),
        });
        let chosen = app.perform(Intent::ConfirmOverlay);
        assert!(
            switched(&chosen).is_none(),
            "a choice made off a session moved one: {chosen:?}"
        );
        assert!(
            chosen.effects.is_empty(),
            "a choice made off a session issued work: {chosen:?}"
        );
        assert_eq!(
            app.new_session_runtime
                .as_ref()
                .map(|selection| selection.agent_id.clone()),
            Some(codex.agent_id.clone()),
            "the choice did not become the next session's"
        );
        assert_eq!(
            app.session_runtime_selection()
                .map(|selection| selection.agent_id.clone()),
            Some(claude.agent_id),
            "the session behind the page was moved"
        );

        // A run option is the same choice seen from the other side: off a
        // session it tunes the next session's Agent, not that session's — and
        // the row sends it the moment it is set, without a session to move.
        app.show_runtime_picker_view(RuntimePickerView::Options);
        let tuned = app.apply_run_option(&RunOptionKey::ReasoningEffort, Some("high".to_string()));
        assert!(
            switched(&tuned).is_none(),
            "a run option off a session moved one: {tuned:?}"
        );
        assert_eq!(
            app.new_session_runtime
                .as_ref()
                .and_then(|selection| selection.reasoning_effort.as_deref()),
            Some("high")
        );
    }

    #[test]
    fn writing_a_new_session_leaves_the_open_one_where_it_was() {
        // The reported flow, end to end: a codex session is open and the reader
        // goes back to the list, picks deepseek-harness for the session they are
        // about to write, presses `n` and sends. The new session is
        // deepseek-harness and the codex session is still codex — the two
        // choices never reach each other.
        let mut app = capable_app();
        let mut catalog = run_option_catalog();
        let mut deepseek = catalog.options[0].clone();
        deepseek.selection.agent_id = vibex_core::AgentId::parse("deepseek-harness").expect("id");
        deepseek.agent_label = "DeepSeek Harness".to_string();
        deepseek.model_label = "deepseek-v4.1-flash".to_string();
        catalog.options.push(deepseek);
        let codex = catalog.options[1].selection.clone();
        let picked = catalog.options[2].selection.clone();
        app.runtime_options = Some(catalog);
        app.new_session_runtime = None;
        session_on(&mut app, codex.clone());
        app.navigate_to(Page::Sessions);

        // The reader chooses the next session's Agent while looking at the list.
        app.show_runtime_picker();
        assert_eq!(app.current_runtime_option_index(), Some(0));
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 2),
        });
        let chosen = app.perform(Intent::ConfirmOverlay);
        assert!(switched(&chosen).is_none(), "{chosen:?}");
        assert_eq!(app.composer_runtime_labels().0, "DeepSeek Harness");

        // `n`, write, send: the creation carries what the page named.
        app.perform(Intent::NewSession);
        assert_eq!(app.composer_runtime_labels().0, "DeepSeek Harness");
        app.composer.insert_str("a deepseek thing");
        let created = created_runtime(&app.perform(Intent::SubmitComposer))
            .expect("the page named a runtime and created with none");
        assert_eq!(created.agent_id, picked.agent_id);

        // The new identity cannot inherit the old session's runtime state.
        assert!(app.session_runtime_selection().is_none());
        assert_eq!(
            app.page_runtime_selection().unwrap().agent_id,
            picked.agent_id
        );
    }

    #[test]
    fn a_directory_chosen_off_the_page_belongs_to_the_next_session() {
        // The workspace key is global like the runtime one, and the same rule
        // applies: a directory chosen while the page shows no session is the
        // next session's, so `n` keeps the reader's choice instead of dropping
        // it on the way in. A creation consumes it, so the page a reader comes
        // back to names the session's own directory rather than one chosen for a
        // session that already exists.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.navigate_to(Page::Sessions);
        // What the picker stores when a directory is chosen from the list.
        app.workspace_path = Some("/home/peatboy/notes".to_string());

        app.perform(Intent::NewSession);
        assert_eq!(
            app.new_session_workspace(),
            "/home/peatboy/notes",
            "the page dropped the directory the reader chose for it"
        );

        app.composer.insert_str("write here");
        let created = app.perform(Intent::SubmitComposer);
        let Some(Effect::CreateSession { workspace_root, .. }) = created
            .effects
            .iter()
            .find(|effect| matches!(effect, Effect::CreateSession { .. }))
        else {
            panic!("no session was asked for: {created:?}");
        };
        assert_eq!(workspace_root, "/home/peatboy/notes");
        assert!(
            app.workspace_path.is_none(),
            "the page kept a directory the session took"
        );
    }

    #[test]
    fn the_landing_page_reads_the_sessions_it_does_not_draw() {
        // The prompt draws no list, but the client's background work — an
        // auto-continue countdown, an unread mark, whether a session's turn is
        // still running — is derived from one. It is read on the way in, and
        // only once: `n` from the list must not read it a second time.
        let mut app = capable_app();
        let landing = app.perform(Intent::NewSession);
        assert!(
            landing
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListSessions { .. })),
            "the landing page did not read the sessions: {:?}",
            landing.effects
        );
        assert!(
            landing
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListWorkspaces)),
            "the landing page did not read the workspaces: {:?}",
            landing.effects
        );

        app.agent
            .apply_sessions(Ok(vec![openable_session("session_landing01")]))
            .expect("sessions apply");
        let again = app.perform(Intent::NewSession);
        assert!(
            !again
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListSessions { .. })),
            "the list was read again with the state already in hand: {:?}",
            again.effects
        );
    }

    #[test]
    fn a_list_key_acts_on_the_row_the_cursor_is_on() {
        // The list shows rows, and the session the client has open behind it is
        // not always the one the reader pointed at. Archive, delete, fork and
        // rename answer for the row under the cursor, so a list key can never
        // change a session nobody selected.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        let open = openable_session("session_row0000001");
        let mut other = openable_session("session_row0000002");
        other.title = "the other session".to_string();
        app.agent
            .apply_sessions(Ok(vec![open.clone(), other.clone()]))
            .expect("sessions apply");
        app.navigate_to(Page::Sessions);
        // The open session is one row; the cursor is on the other.
        app.agent.state.selected_session_id = Some(open.id.clone());
        app.agent.state.active_session.resolve(open.clone());
        let cursor = app
            .sidebar_rows()
            .iter()
            .position(|row| row.session_id.as_ref() == Some(&other.id))
            .expect("the other session has a row");
        app.set_selection(Scope::Sessions, cursor);

        let forked = app.perform(Intent::ForkSession);
        let [Effect::ForkSession { session_id, .. }] = forked.effects.as_slice() else {
            panic!("expected one fork, got {forked:?}");
        };
        assert_eq!(
            session_id, &other.id,
            "the fork took the session behind the list"
        );

        app.perform(Intent::ArchiveSession);
        let archived = app.perform(Intent::ConfirmOverlay);
        let [Effect::ArchiveSession { session_id }] = archived.effects.as_slice() else {
            panic!("expected one archive, got {archived:?}");
        };
        assert_eq!(
            session_id, &other.id,
            "the archive took the session behind the list"
        );

        app.perform(Intent::BeginRenameSession);
        let Some(Overlay::Prompt { value, .. }) = app.overlay.clone() else {
            panic!("the rename prompt did not open: {:?}", app.overlay);
        };
        assert_eq!(value, "the other session");
        assert_eq!(
            app.selected_session_id(),
            Some(&open.id),
            "the list key moved the reader's session"
        );
    }

    #[test]
    fn a_choice_on_a_session_moves_only_that_session() {
        // And the other direction: a choice made on a session's own page moves
        // that session and nothing else. It is not carried into the page where
        // the next session is written, which answers with its own choice — so
        // the two can never inherit each other's Agent.
        let mut app = capable_app();
        let catalog = run_option_catalog();
        let claude = catalog.options[0].selection.clone();
        let codex = catalog.options[1].selection.clone();
        app.runtime_options = Some(catalog);
        app.new_session_runtime = None;
        session_on(&mut app, claude.clone());
        let session_id = app.selected_session_id().cloned().expect("a session");
        app.navigate_to(Page::Agent);

        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 1),
        });
        let chosen = app.perform(Intent::ConfirmOverlay);
        let [
            Effect::SwitchRuntime {
                session_id: target,
                selection,
            },
        ] = chosen.effects.as_slice()
        else {
            panic!("expected one runtime switch, got {chosen:?}");
        };
        assert_eq!(target, &session_id);
        assert_eq!(selection.agent_id, codex.agent_id);
        assert!(
            app.new_session_runtime.is_none(),
            "the session's move became the next session's choice"
        );

        // The next draft derives the remembered preference without becoming
        // an explicit draft edit or changing the existing session's state.
        // The picker is left before the next session is written — choosing an
        // entry keeps it open on the run options — and the page the reader
        // writes on then answers with the remembered preference.
        app.perform(Intent::Back);
        app.perform(Intent::Back);
        assert!(app.overlay.is_none(), "the picker stayed open");
        app.perform(Intent::NewSession);
        assert!(app.new_session_runtime.is_none());
        assert_eq!(app.composer_runtime_labels().0, "codex");
        assert_eq!(
            app.page_runtime_selection()
                .map(|selection| selection.agent_id),
            Some(codex.agent_id),
        );
        assert_eq!(
            app.session_runtime_selection().unwrap().agent_id,
            claude.agent_id
        );
    }

    #[test]
    fn a_send_waits_for_the_agent_it_will_be_created_with() {
        // A session is created *with* an Agent, so a page that cannot name one
        // reads the catalogue instead of handing the choice to the runtime —
        // which would land the session on an Agent the reader never saw. The
        // draft stays in the box for the `Enter` that follows.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.perform(Intent::NewSession);
        app.composer.insert_str("who answers this?");
        let waiting = app.perform(Intent::SubmitComposer);
        assert!(
            waiting
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListRuntimeOptions)),
            "the page did not read the catalogue: {waiting:?}"
        );
        assert!(
            created_runtime(&waiting).is_none(),
            "a session was created before an Agent could be named"
        );
        assert_eq!(app.composer.text(), "who answers this?");
        assert!(app.pending_creations.is_empty());

        // A catalogue that publishes nothing is not going to answer: the page
        // says so rather than creating with a runtime the reader never chose.
        app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: Vec::new(),
        });
        let refused = app.perform(Intent::SubmitComposer);
        assert!(refused.effects.is_empty(), "{refused:?}");
        assert_eq!(app.composer.text(), "who answers this?");
    }

    #[test]
    fn the_composing_page_and_its_creation_name_the_same_entry() {
        // A catalogue with nothing available is the one case where the page has
        // no usable entry to name. It still answers for itself rather than for
        // the session behind it: it names the entry a creation with no choice
        // falls back to — the worker's own rule — so the page and the session
        // it makes can never name two different Agents.
        let mut app = capable_app();
        let catalog = run_option_catalog();
        let unavailable = catalog.options[0].clone();
        let mut option = unavailable.clone();
        option.availability = vibex_core::RuntimeOptionAvailability::RequiresConfiguration;
        app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: vec![option.clone()],
        });
        session_on(&mut app, catalog.options[1].selection.clone());
        app.perform(Intent::NewSession);

        assert_eq!(
            app.composer_runtime_labels().0,
            option.agent_label,
            "the page did not name the entry a creation would use"
        );
        app.composer.insert_str("a new thing");
        let created = created_runtime(&app.perform(Intent::SubmitComposer))
            .expect("the page named a runtime and created with none");
        assert_eq!(created.agent_id, option.selection.agent_id);
        assert_eq!(created.model, option.selection.model);
    }

    #[test]
    fn a_new_session_after_a_codex_row_is_the_agent_the_page_picked() {
        // The reader is in a codex session and the list holds its row, so the
        // reader presses `n`, picks another Agent on the page and sends. What
        // is created must be the Agent on the page, never the one behind it.
        let mut app = capable_app();
        let catalog = run_option_catalog();
        let codex = catalog.options[1].selection.clone();
        let claude = catalog.options[0].selection.clone();
        app.runtime_options = Some(catalog);
        app.new_session_runtime = None;
        session_on(&mut app, codex);

        app.perform(Intent::NewSession);
        // The reader opens the switcher and takes the first row, which is the
        // Agent that is not the one behind the page.
        app.show_runtime_picker();
        app.perform(Intent::ConfirmOverlay);
        assert_eq!(
            app.new_session_runtime.as_ref().map(|s| s.agent_id.clone()),
            Some(claude.agent_id.clone())
        );
        assert_eq!(app.composer_runtime_labels().0, "claude");

        app.composer.insert_str("a new thing");
        let created = created_runtime(&app.perform(Intent::SubmitComposer))
            .expect("the page named a runtime and created with none");
        assert_eq!(
            created.agent_id, claude.agent_id,
            "the creation used the Agent behind the page"
        );
    }

    #[test]
    fn the_options_view_belongs_to_the_row_the_reader_picked() {
        // `Tab` opens the run options of the row the cursor is on — the preview
        // line under the list already says so — rather than of the entry the
        // page happened to start on. Opening a view is not a decision, though:
        // the page takes the row when one of its options is set, or when the row
        // is chosen outright.
        let mut app = app_with_run_options(Page::NewSession);
        app.new_session_runtime = None;
        app.show_runtime_picker();
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                selected: entry_row(&app, 0),
            }),
            "the catalogue did not open on the page's own entry"
        );
        // Every other group is folded, so the row below the open one is the next
        // Agent's heading.
        assert!(
            app.runtime_picker
                .folded
                .contains(&vibex_core::AgentId::parse("codex").expect("agent id"))
        );

        app.perform(Intent::SelectNext);
        let outcome = app.perform(Intent::OverlayNextField);
        assert!(outcome.effects.is_empty(), "{outcome:?}");
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 0,
            }),
            "the row's run options did not open"
        );
        assert_eq!(
            app.picker_runtime_labels().0,
            "codex",
            "the options belong to the entry the page started on"
        );
        assert!(
            app.new_session_runtime.is_none(),
            "opening a view was taken for a choice"
        );

        // Setting a row is what takes it, and the Agent comes with the setting:
        // a thinking depth belongs to the Agent it was set on.
        let tuned = app.apply_run_option(&RunOptionKey::ReasoningEffort, Some("high".to_string()));
        assert!(tuned.effects.is_empty(), "{tuned:?}");
        assert_eq!(
            app.new_session_runtime
                .as_ref()
                .map(|selection| selection.agent_id.clone()),
            Some(vibex_core::AgentId::parse("codex").expect("agent id")),
            "the page did not take the Agent whose option was set"
        );
        assert_eq!(app.composer_runtime_labels().0, "codex");
    }

    #[test]
    fn a_run_option_the_catalogue_stopped_publishing_is_not_sent() {
        let mut app = app_with_run_options(Page::Agent);
        app.show_runtime_picker_view(RuntimePickerView::Options);

        // A stale value list (the catalogue moved under it) must not move the
        // session onto a setting no Agent advertises.
        let refused = app.apply_run_option(
            &RunOptionKey::ReasoningEffort,
            Some("nonexistent".to_string()),
        );
        assert!(
            refused.effects.is_empty(),
            "a value no Agent publishes was sent: {refused:?}"
        );
        assert_eq!(
            app.session_runtime_selection()
                .and_then(|selection| selection.reasoning_effort.clone()),
            None
        );
        // A value the Agent does publish is sent as it is set.
        let accepted =
            app.apply_run_option(&RunOptionKey::ReasoningEffort, Some("high".to_string()));
        assert_eq!(
            switched(&accepted).and_then(|selection| selection.reasoning_effort.clone()),
            Some("high".to_string())
        );
    }

    #[test]
    fn reaching_the_top_of_the_transcript_asks_for_older_history_once() {
        let mut app = capable_app();
        let session_id = VibexSessionId::new();
        app.navigate_to(Page::Agent);
        app.agent.state.selected_session_id = Some(session_id.clone());
        app.agent.state.timeline.replace_authoritative(
            session_id.clone(),
            [history_item(&session_id, 3), history_item(&session_id, 4)],
        );
        app.agent.state.timeline_has_older = true;

        // No scrolling intent, no request.
        assert!(app.perform(Intent::ScrollPageDown).effects.is_empty());

        // The explicit key asks for the page below the oldest loaded item even
        // while the transcript is still following the tail.
        let explicit = app.perform(Intent::LoadOlderHistory);
        let [Effect::LoadOlder { ticket }] = explicit.effects.as_slice() else {
            panic!(
                "expected one older-page request, got {:?}",
                explicit.effects
            );
        };
        assert_eq!(ticket.session_id, session_id);
        assert_eq!(ticket.before_sequence, 3);

        // Scrolling to the top while that request is in flight must not stack
        // a second one.
        app.scroll.follow = false;
        app.scroll.offset = 0;
        assert!(app.at_transcript_top());
        assert!(app.perform(Intent::ScrollHalfPageUp).effects.is_empty());

        // An empty page means the history is exhausted, so the trigger stops.
        let ticket = ticket.clone();
        let page = vibex_core::TimelinePage {
            session_id: session_id.clone(),
            items: Vec::new(),
            start_sequence: None,
            end_sequence: None,
            has_older: false,
            has_newer: true,
        };
        assert!(
            app.agent
                .apply_timeline_before(&ticket, Ok(page))
                .expect("an empty page is not an error")
        );
        assert!(!app.at_transcript_top());
        assert!(app.perform(Intent::ScrollHalfPageUp).effects.is_empty());
    }

    #[test]
    fn every_response_kind_resolves_to_an_advertised_option() {
        let options = vec![vibex_core::PermissionResponseOption {
            option_id: "o1".into(),
            label: "Allow".into(),
            response: PermissionResponseKind::Approve,
        }];
        // Deny is not advertised, so it falls back to the only real option
        // rather than inventing one the provider would reject.
        assert_eq!(
            response_for(&options, PermissionResponseKind::Deny),
            Some(PermissionResponseKind::Approve)
        );
        assert_eq!(response_for(&[], PermissionResponseKind::Approve), None);
    }

    #[test]
    fn elicitation_draft_prefers_typed_values_over_defaults() {
        let field = vibex_core::ElicitationField {
            id: "name".into(),
            title: "Name".into(),
            description: None,
            required: true,
            kind: vibex_core::ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: Some("default".into()),
                options: Vec::new(),
            },
        };
        let mut draft = ElicitationDraft::default();
        draft.set_text("name", "typed");
        assert_eq!(
            draft.answer_for("name", 0, &field, 0),
            Some(ElicitationAnswerValue::String("typed".into()))
        );
        draft.clear();
        assert_eq!(
            draft.answer_for("name", 0, &field, 0),
            Some(ElicitationAnswerValue::String("default".into()))
        );
    }

    #[test]
    fn elicitation_draft_reads_the_live_editor_for_the_active_field() {
        let field = vibex_core::ElicitationField {
            id: "q".into(),
            title: "Q".into(),
            description: None,
            required: true,
            kind: vibex_core::ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: Vec::new(),
            },
        };
        let mut draft = ElicitationDraft::default();
        draft.begin_text("q");
        draft.active_text_mut().push_str("hello");
        assert_eq!(
            draft.answer_for("q", 0, &field, 0),
            Some(ElicitationAnswerValue::String("hello".into()))
        );
        // Not the active field, and nothing committed: no answer.
        assert_eq!(draft.answer_for("q", 1, &field, 0), None);
        draft.commit_text("q");
        assert_eq!(
            draft.answer_for("q", 1, &field, 0),
            Some(ElicitationAnswerValue::String("hello".into()))
        );
    }

    #[test]
    fn entry_edits_carry_the_row_id_so_the_worker_does_not_guess() {
        // The effect must name the entry it edits: re-deriving "the third row"
        // in the worker would race a list refresh.
        let edits = [
            crate::app::ManagementEntryEdit::Mcp {
                server_id: vibex_core::McpServerId::new(),
                display_name: "a".into(),
            },
            crate::app::ManagementEntryEdit::Skill {
                skill_id: vibex_core::SkillId::new(),
                display_name: "b".into(),
            },
            crate::app::ManagementEntryEdit::Prompt {
                prompt_id: vibex_core::PromptId::new(),
                display_name: "c".into(),
            },
        ];
        for edit in edits {
            let effect = Effect::UpdateEntry {
                entry: edit.clone(),
            };
            assert_eq!(effect.key(), "update_entry");
        }
    }

    #[test]
    fn mutating_effects_are_distinguishable_by_key() {
        // The pending map is keyed by this string, so two different mutations
        // sharing a key would clear each other's spinner.
        let session_id = VibexSessionId::new();
        let keys = [
            Effect::RenameSession {
                session_id: session_id.clone(),
                title: "t".into(),
            }
            .key(),
            Effect::ArchiveSession {
                session_id: session_id.clone(),
            }
            .key(),
            Effect::DeleteSession {
                session_id: session_id.clone(),
            }
            .key(),
            Effect::ForkSession {
                request_id: VibexSessionId::new(),
                session_id: session_id.clone(),
            }
            .key(),
            Effect::ContinueTurn {
                session_id: session_id.clone(),
            }
            .key(),
            Effect::Interrupt {
                session_id: session_id.clone(),
            }
            .key(),
            Effect::LoadUsage.key(),
        ];
        let unique = keys.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
    }

    #[test]
    fn multi_select_toggles_membership() {
        let mut draft = ElicitationDraft::default();
        draft.toggle_multi("f", "a");
        draft.toggle_multi("f", "b");
        assert_eq!(draft.multi("f"), vec!["a".to_string(), "b".to_string()]);
        draft.toggle_multi("f", "a");
        assert_eq!(draft.multi("f"), vec!["b".to_string()]);
    }

    #[test]
    fn boolean_defaults_are_used_when_untouched() {
        let field = vibex_core::ElicitationField {
            id: "flag".into(),
            title: "Flag".into(),
            description: None,
            required: false,
            kind: vibex_core::ElicitationFieldKind::Boolean {
                default: Some(true),
            },
        };
        let mut draft = ElicitationDraft::default();
        assert_eq!(
            draft.answer_for("flag", 0, &field, 0),
            Some(ElicitationAnswerValue::Boolean(true))
        );
        draft.set_boolean("flag", false);
        assert_eq!(
            draft.answer_for("flag", 0, &field, 0),
            Some(ElicitationAnswerValue::Boolean(false))
        );
    }

    /// Two sessions in one project, so the list has one heading and two rows.
    fn two_sessions() -> (vibex_core::AgentSession, vibex_core::AgentSession) {
        let mut first = openable_session("session_org000001");
        first.title = "first".to_string();
        first.last_message_at_ms = 1_759_251_200_000;
        let mut second = openable_session("session_org000002");
        second.title = "second".to_string();
        second.project_id = first.project_id.clone();
        second.workspace_id = first.workspace_id.clone();
        second.workspace_root = first.workspace_root.clone();
        second.last_message_at_ms = 1_759_251_100_000;
        (first, second)
    }

    /// What a desktop publishes when it has pinned the second session and put
    /// the first one after it by hand.
    fn arranged_snapshot() -> vibex_core::RemoteSidebarOrganizationSnapshot {
        vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 11,
            folders: Vec::new(),
            groups: Vec::new(),
            placements: Vec::new(),
            collapsed_folder_ids: Vec::new(),
            collapsed_group_ids: Vec::new(),
            collapsed_project_ids: Vec::new(),
            collapsed_workspace_ids: Vec::new(),
            pinned_session_ids: vec!["session_org000002".to_string()],
            session_order: vec![
                "session_org000001".to_string(),
                "session_org000002".to_string(),
            ],
            session_order_anchored_at_ms: i64::MAX,
            hierarchy_mode: vibex_core::RemoteSidebarHierarchyMode::Compact,
            project_order: Vec::new(),
            workspace_order: std::collections::BTreeMap::new(),
            project_appearances: std::collections::BTreeMap::new(),
            worktree_titles: std::collections::BTreeMap::new(),
            project_new_session_locations: std::collections::BTreeMap::new(),
            auto_continue_project_ids: Vec::new(),
            auto_continue_session_overrides: std::collections::BTreeMap::new(),
            auto_continue_session_ids: Vec::new(),
            auto_continue_paused_session_ids: Vec::new(),
            unread_session_ids: vec!["session_org000001".to_string()],
        }
    }

    fn arranged_app_with(snapshot: &vibex_core::RemoteSidebarOrganizationSnapshot) -> App {
        let mut app = capable_app();
        let (first, second) = two_sessions();
        app.agent
            .apply_sessions(Ok(vec![first, second]))
            .expect("sessions apply");
        app.page = Page::Sessions;
        assert!(
            app.apply_sidebar_organization(snapshot),
            "the arrangement was not adopted"
        );
        app
    }

    fn arranged_app() -> App {
        arranged_app_with(&arranged_snapshot())
    }

    #[test]
    fn the_authority_arrangement_is_the_order_the_list_draws() {
        let app = arranged_app();
        let rows = app.sidebar_rows();
        // Pinned above the manual order, and the unread mark comes from the
        // authority rather than from this client's own bookkeeping.
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            vec!["vibex-reduce-workspace", "second", "first"],
            "the list did not follow the arrangement: {rows:#?}"
        );
        assert!(rows[1].pinned, "the pin did not reach the row");
        assert!(
            app.session_is_unread(&vibex_core::VibexSessionId::parse("session_org000001").unwrap())
        );
    }

    #[test]
    fn an_arrangement_that_arranges_nothing_leaves_the_local_order_alone() {
        let mut app = capable_app();
        let (first, second) = two_sessions();
        app.agent
            .apply_sessions(Ok(vec![first, second]))
            .expect("sessions apply");
        let empty = vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 0,
            ..arranged_snapshot()
        };
        let empty = vibex_core::RemoteSidebarOrganizationSnapshot {
            pinned_session_ids: Vec::new(),
            session_order: Vec::new(),
            unread_session_ids: Vec::new(),
            ..empty
        };
        app.apply_sidebar_organization(&empty);
        assert!(
            app.sidebar_organization().is_none(),
            "an empty arrangement replaced the fallback"
        );
        // No pin, and recency still decides: the fallback list is unchanged.
        assert!(
            app.sidebar_rows().iter().all(|row| !row.pinned),
            "a pin appeared from nowhere"
        );
    }

    #[test]
    fn a_pin_is_asked_of_the_authority_that_owns_the_list() {
        let mut app = arranged_app();
        app.set_selection(crate::keymap::Scope::Sessions, 2);
        let outcome = app.perform(Intent::PinSession);
        let [
            Effect::MutateSidebarOrganization {
                mutation,
                expected_revision,
            },
        ] = outcome.effects.as_slice()
        else {
            panic!("expected one sidebar mutation, got {outcome:?}");
        };
        assert_eq!(
            mutation,
            &vibex_core::RemoteSidebarOrganizationMutation::SetSessionPinned {
                session_id: "session_org000001".to_string(),
                pinned: true,
            }
        );
        assert_eq!(
            *expected_revision,
            Some(11),
            "the change must name its tree"
        );
        // The local arrangement is not what the reader is looking at, so it is
        // not what the pin edits.
        assert!(app.projection.sidebar.pinned_ids.is_empty());
    }

    #[test]
    fn the_auto_continue_key_switches_the_session_on_the_authority() {
        let mut app = arranged_app();
        // Row 2 is the first (unpinned) session in the arrangement.
        app.set_selection(crate::keymap::Scope::Sessions, 2);
        let outcome = app.perform(Intent::ToggleAutoContinue);
        let [Effect::MutateSidebarOrganization { mutation, .. }] = outcome.effects.as_slice()
        else {
            panic!("expected one sidebar mutation, got {outcome:?}");
        };
        assert_eq!(
            mutation,
            &vibex_core::RemoteSidebarOrganizationMutation::SetSessionAutoContinue {
                session_id: "session_org000001".to_string(),
                enabled: true,
            }
        );
        assert!(
            app.auto_continue
                .is_enabled(&vibex_core::VibexSessionId::parse("session_org000001").unwrap())
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some(app.strings.auto_continue_enabled())
        );

        // The same key switches it off again.
        let outcome = app.perform(Intent::ToggleAutoContinue);
        let [Effect::MutateSidebarOrganization { mutation, .. }] = outcome.effects.as_slice()
        else {
            panic!("expected one sidebar mutation, got {outcome:?}");
        };
        assert_eq!(
            mutation,
            &vibex_core::RemoteSidebarOrganizationMutation::SetSessionAutoContinue {
                session_id: "session_org000001".to_string(),
                enabled: false,
            }
        );
        assert!(
            !app.auto_continue
                .is_enabled(&vibex_core::VibexSessionId::parse("session_org000001").unwrap())
        );
    }

    #[test]
    fn a_session_left_without_an_answer_probes_then_counts_down() {
        let mut app = arranged_app();
        let session_id = vibex_core::VibexSessionId::parse("session_org000001").unwrap();
        let updated_at_ms = app
            .session_by_id(&session_id)
            .expect("the session is loaded")
            .updated_at_ms;
        // The authority has this session switched on, which is where the client
        // learns it from.
        let mut snapshot = arranged_snapshot();
        snapshot.auto_continue_session_ids = vec!["session_org000001".to_string()];
        app.apply_sidebar_organization(&snapshot);
        assert!(app.auto_continue.is_enabled(&session_id));

        // The list cannot say whether the last turn ended normally, so the
        // first move is a question to the runtime.
        let effects = app.sync_auto_continue();
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::ProbeAutoContinue {
                    session_id: probed,
                    updated_at_ms: at_ms,
                }] if probed == &session_id && *at_ms == updated_at_ms
            ),
            "the session was not probed: {effects:?}"
        );
        // Asked once per revision, not once per message.
        assert!(app.sync_auto_continue().is_empty());

        // The answer is a turn that stopped without an answer: the countdown
        // starts, and the reader can see it on the row.
        app.auto_continue
            .note_status(&session_id, updated_at_ms, Some(false));
        assert!(app.sync_auto_continue().is_empty());
        assert_eq!(app.auto_continue.countdown_seconds(&session_id), Some(5));

        // An answer that says the turn ended normally is not continued.
        let mut app = arranged_app();
        app.apply_sidebar_organization(&snapshot);
        app.sync_auto_continue();
        app.auto_continue
            .note_status(&session_id, updated_at_ms, Some(true));
        app.sync_auto_continue();
        assert_eq!(app.auto_continue.countdown_seconds(&session_id), None);
    }

    #[test]
    fn stopping_a_turn_stops_the_continuation_that_would_follow_it() {
        let mut app = arranged_app();
        let session_id = vibex_core::VibexSessionId::parse("session_org000001").unwrap();
        let updated_at_ms = app.session_by_id(&session_id).unwrap().updated_at_ms;
        let mut snapshot = arranged_snapshot();
        snapshot.auto_continue_session_ids = vec!["session_org000001".to_string()];
        app.apply_sidebar_organization(&snapshot);
        app.auto_continue
            .note_status(&session_id, updated_at_ms, Some(false));
        app.sync_auto_continue();
        assert!(app.auto_continue.countdown_seconds(&session_id).is_some());

        // The reader stops the session: the pending continuation goes with it.
        app.auto_continue
            .pause(&session_id, vibex_core::unix_timestamp_ms());
        assert_eq!(app.auto_continue.countdown_seconds(&session_id), None);
        assert!(
            !app.sync_auto_continue()
                .iter()
                .any(|effect| matches!(effect, Effect::ContinueTurn { .. }))
        );
    }

    #[test]
    fn a_collapsed_heading_toggles_on_the_authority() {
        let mut app = arranged_app();
        app.set_selection(crate::keymap::Scope::Sessions, 0);
        let outcome = app.perform(Intent::OpenSelectedSession);
        let [Effect::MutateSidebarOrganization { mutation, .. }] = outcome.effects.as_slice()
        else {
            panic!("expected one sidebar mutation, got {outcome:?}");
        };
        assert!(
            matches!(
                mutation,
                vibex_core::RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                    collapsed: true,
                    ..
                }
            ),
            "a heading did not ask the authority to close: {mutation:?}"
        );
    }

    #[test]
    fn a_move_asks_the_authority_for_the_new_order() {
        let mut snapshot = arranged_snapshot();
        snapshot.pinned_session_ids.clear();
        let mut app = arranged_app_with(&snapshot);
        // The manual order is what the list draws, so the first session is the
        // one under the cursor and the second is its neighbour.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        let outcome = app.perform(Intent::MoveSessionDown);
        let [Effect::MutateSidebarOrganization { mutation, .. }] = outcome.effects.as_slice()
        else {
            panic!("expected one sidebar mutation, got {outcome:?}");
        };
        let vibex_core::RemoteSidebarOrganizationMutation::MoveItems {
            items,
            anchor,
            position,
            ..
        } = mutation
        else {
            panic!("a reorder must move the session: {mutation:?}");
        };
        assert_eq!(items[0].id, "session_org000001");
        assert_eq!(anchor.as_ref().expect("an anchor").id, "session_org000002");
        assert_eq!(position, &vibex_core::RemoteSidebarDropPosition::After);
    }

    #[test]
    fn a_move_across_the_pinned_band_says_so_instead_of_asking() {
        let mut app = arranged_app();
        // The pinned session leads the band; the row under it is not pinned,
        // and no arrangement can put one above the other.
        app.set_selection(crate::keymap::Scope::Sessions, 2);
        let outcome = app.perform(Intent::MoveSessionUp);
        assert!(
            outcome.effects.is_empty(),
            "an impossible move was sent anyway: {outcome:?}"
        );
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.as_str()),
            Some(app.strings.sidebar_pinned_first())
        );
    }
    #[test]
    fn the_picker_opens_as_a_list_of_agents_with_only_the_one_in_use_open() {
        // The catalogue is as long as the machine has models, so it opens folded
        // to the Agent the reader is on: the group they are already using is the
        // one that is open, and `→` opens another. A reader who opened the
        // surface to see *which* Agent they are on should not have to walk past
        // every model of every other one to find out.
        let mut app = app_with_run_options(Page::Agent);
        let claude = vibex_core::AgentId::parse("claude").expect("agent id");
        app.show_runtime_picker();
        assert_eq!(
            app.runtime_picker
                .folded
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![claude.clone()],
            "the picker did not open on the Agent in use"
        );
        let rows = app.runtime_picker_rows();
        assert!(
            rows.iter().any(|row| row.entry() == Some(1)),
            "the Agent in use was folded away: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.entry() == Some(0)),
            "another Agent's models were drawn: {rows:?}"
        );

        // The folded heading is a row like any other, and `→` on it opens the
        // group rather than stepping over it.
        let heading = rows
            .iter()
            .position(|row| matches!(row, RuntimePickerRow::Agent { .. }))
            .expect("a heading");
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: heading,
        });
        app.perform(Intent::OverlayFoldOpen);
        assert!(app.runtime_picker.folded.is_empty());
        // `Enter` on the same heading folds it back: a heading is its entries
        // seen at once, so the key that opens it is the key that closes it.
        app.perform(Intent::ConfirmOverlay);
        assert!(app.runtime_picker.folded.contains(&claude));
    }

    #[test]
    fn choosing_an_entry_offers_its_run_options_rather_than_closing() {
        // Choosing an Agent and saying how it runs are one errand: `Enter` on an
        // entry that publishes run options leaves the picker on them, so the
        // reader does not have to leave the surface and come back to reach
        // `Tab`.
        let mut app = app_with_run_options(Page::Agent);
        let codex = vibex_core::AgentId::parse("codex").expect("agent id");
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 1),
        });
        let chosen = app.perform(Intent::ConfirmOverlay);
        assert_eq!(
            switched(&chosen).map(|selection| selection.agent_id.clone()),
            Some(codex.clone()),
            "the entry was not chosen: {chosen:?}"
        );
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 0,
            }),
            "the picker did not open the chosen entry's run options"
        );
        assert!(
            !app.picker_run_options().is_empty(),
            "the run options belong to another entry"
        );

        // An entry with nothing to tune closes the picker instead: there is
        // nothing left to ask, and staying open would be a dead end.
        app.perform(Intent::Back);
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: open_entry_row(&mut app, 0),
        });
        let plain = app.perform(Intent::ConfirmOverlay);
        assert!(switched(&plain).is_some(), "{plain:?}");
        assert_eq!(
            app.overlay, None,
            "an entry with nothing to tune stayed open"
        );
    }

    #[test]
    fn the_recent_section_offers_at_most_six_entries() {
        // The pinned rows are a shortcut past a long catalogue — and what the
        // digits choose from — so they are whatever fits in one glance, not
        // every model the reader has ever run.
        let mut app = app_with_run_options(Page::Agent);
        let mut catalog = app.runtime_options.clone().expect("catalogue");
        let mut extras = Vec::new();
        for index in 0..(crate::runtime_picker::RECENT_LIMIT + 2) {
            let mut extra = catalog.options[1].clone();
            let model = format!("gpt-5-{index}");
            extra.model_label = model.clone();
            extra.selection.model = vibex_core::RuntimeModelSelection::explicit(model);
            extras.push(extra.clone());
            catalog.options.push(extra);
        }
        app.runtime_options = Some(catalog);
        for option in &extras {
            app.remember_runtime_selection(&option.selection);
        }

        app.show_runtime_picker();
        let rows = app.runtime_picker_rows();
        let section = rows
            .iter()
            .position(|row| {
                matches!(
                    row,
                    RuntimePickerRow::Section(crate::runtime_picker::RuntimePickerSection::Recent)
                )
            })
            .expect("the recent section");
        let recent = rows[section + 1..]
            .iter()
            .take_while(|row| matches!(row, RuntimePickerRow::Entry { .. }))
            .count();
        assert_eq!(
            recent,
            crate::runtime_picker::RECENT_LIMIT,
            "the recent section is not a glance: {rows:?}"
        );
        // Most recent first, so the row the digits call `1` is the last entry
        // the reader used.
        let newest = extras.last().expect("an entry").model_label.clone();
        assert_eq!(
            rows[section + 1].entry(),
            Some(catalog_index_of(&app, &newest)),
            "the recent section is not in recency order"
        );
    }

    /// Where one model's entry sits in the loaded catalogue.
    fn catalog_index_of(app: &App, model: &str) -> usize {
        app.runtime_options
            .as_ref()
            .expect("catalogue")
            .options
            .iter()
            .position(|option| option.model_label == model)
            .expect("the entry is in the catalogue")
    }

    #[test]
    fn a_run_option_belongs_to_the_visit_that_set_it() {
        // A run option is a setting on the session the reader was looking at,
        // and the picker keeps the selection it is editing for exactly as long
        // as it is up. Leaving it forgets the edit — the page's own selection is
        // what the next visit starts from — so one session's thinking depth
        // never becomes another's.
        let mut app = app_with_run_options(Page::Agent);
        let runtime = app.agent.state.runtime_selection.value.clone().unwrap();
        app.show_runtime_picker_view(RuntimePickerView::Options);
        let tuned = app.apply_run_option(&RunOptionKey::ReasoningEffort, Some("high".into()));
        assert_eq!(
            switched(&tuned).and_then(|selection| selection.reasoning_effort.clone()),
            Some("high".to_string())
        );
        app.cancel_runtime_picker();
        assert!(
            app.runtime_picker.working.is_none(),
            "the picker kept editing a surface that is gone"
        );

        // A session opened afterwards answers with its own selection, not with
        // the one the last visit was editing.
        let mut other = openable_session("session_same0002");
        other.agent_id = runtime.desired.agent_id.clone();
        app.open_session_effects(other.id.clone());
        app.agent.state.active_session.resolve(other);
        app.agent.state.runtime_selection.resolve(runtime);
        app.show_runtime_picker_view(RuntimePickerView::Options);
        assert_eq!(
            app.runtime_picker
                .working
                .as_ref()
                .and_then(|working| working.reasoning_effort.clone()),
            None
        );
    }
}
