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
    SteerAgentMessageRequest, WorkspaceMode,
};

use crate::action::Intent;
use crate::app::{
    App, Availability, Effect, Focus, ManagementRow, Overlay, Page, PromptField, RecoveryAction,
    Toast,
};
use crate::composer::{CompletionMenu, CompletionTrigger};
use crate::keymap::Scope;
use crate::view::block_detail_text;

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
            Intent::RequestQuit => self.confirm_quit(),
            Intent::Back => self.go_back(),
            Intent::FocusNext => {
                self.focus = self.focus.next();
                if self.focus == Focus::Composer {
                    self.page = Page::Agent;
                }
                Outcome::effects(vec![])
            }
            Intent::FocusPrevious => {
                self.focus = self.focus.previous();
                Outcome::effects(vec![])
            }
            Intent::Refresh => self.refresh_current_page(),
            Intent::ContextualCancel => self.contextual_cancel(),
            Intent::GotoSessions => {
                self.select_global(vibex_ui::shell::GlobalDestination::Sessions);
                Outcome::effects(vec![Effect::ListSessions {
                    include_archived: self.show_archived,
                }])
            }
            Intent::GotoManagement => {
                self.select_global(vibex_ui::shell::GlobalDestination::Management);
                Outcome::effects(vec![])
            }
            Intent::GotoUsage => {
                self.page = Page::Usage;
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
                self.scroll_by(-1, true);
                Outcome::effects(vec![])
            }
            Intent::ScrollPageDown => {
                self.scroll_by(1, true);
                Outcome::effects(vec![])
            }
            Intent::ScrollHalfPageUp => {
                self.scroll_by(-1, false);
                Outcome::effects(vec![])
            }
            Intent::ScrollHalfPageDown => {
                self.scroll_by(1, false);
                Outcome::effects(vec![])
            }
            Intent::ScrollToTop => {
                self.scroll.follow = false;
                self.scroll.offset = 0;
                Outcome::effects(vec![])
            }
            Intent::ScrollToBottom => {
                self.scroll.follow = true;
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
                let Some(row) = rows.get(index) else {
                    return Outcome::quiet();
                };
                let Some(session_id) = row.session_id.clone() else {
                    // A project header toggles instead of opening.
                    self.toggle_sidebar_collapsed_for(&row.project_id);
                    return Outcome::effects(vec![]);
                };
                // The controller issues the load ticket before the fetch, so
                // the snapshot is applied in the generation it was requested
                // for and live events for the session stop being stale.
                let ticket = match self.agent.begin_session_load(session_id.clone()) {
                    Ok(ticket) => ticket,
                    Err(error) => {
                        self.toast(Toast::danger(error.message));
                        return Outcome::quiet();
                    }
                };
                self.open_session(session_id.clone());
                // The composer's info line names the Agent and model the
                // session is on, and the switcher needs the same catalogue, so
                // it is fetched on the way in rather than only when the picker
                // is opened. It is a read; a failure leaves the line naming the
                // Agent alone.
                let mut effects = vec![Effect::OpenSession { session_id, ticket }];
                if self.runtime_options.is_none() && self.runtime_catalog_available() {
                    effects.push(Effect::ListRuntimeOptions);
                }
                Outcome::effects(effects)
            }
            Intent::EnterSession => {
                let outcome = self.perform(Intent::OpenSelectedSession);
                if !outcome.effects.is_empty() {
                    // Landing on the session view is a request to work in it, so
                    // the caret goes back into the composer: navigating set the
                    // focus to the page itself.
                    self.select_session_destination(vibex_ui::shell::SessionDestination::Agent);
                    self.focus = Focus::Composer;
                }
                outcome
            }
            Intent::NewSession => self.begin_new_session(),
            Intent::BeginRenameSession => self.begin_rename_session(),
            Intent::ForkSession => {
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                let message = format!(
                    "{} — {}",
                    self.strings.session_fork(),
                    self.strings.confirm()
                );
                self.toast(Toast::info(message));
                Outcome::effects(vec![Effect::ForkSession { session_id }])
            }
            Intent::ArchiveSession => {
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.session_archive().to_string(),
                    body: self.strings.session_confirm_archive().to_string(),
                    confirm: Intent::ArchiveSession,
                });
                self.set_selection(Scope::Overlay, 0);
                let _ = session_id;
                Outcome::effects(vec![])
            }
            Intent::DeleteSession => {
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.session_delete().to_string(),
                    body: self.strings.session_confirm_delete().to_string(),
                    confirm: Intent::DeleteSession,
                });
                self.set_selection(Scope::Overlay, 0);
                let _ = session_id;
                Outcome::effects(vec![])
            }
            Intent::ToggleShowArchived => {
                self.show_archived = !self.show_archived;
                Outcome::effects(vec![Effect::ListSessions {
                    include_archived: self.show_archived,
                }])
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
                if self.toggle_session_pin() {
                    let message = match self.selected_sidebar_row() {
                        Some(row) if row.pinned => self.strings.sidebar_pinned(),
                        _ => self.strings.sidebar_unpinned(),
                    };
                    self.toast(Toast::success(message));
                }
                Outcome::effects(vec![])
            }
            // Up the screen is a smaller row index.
            Intent::MoveSessionUp => self.move_session(-1),
            Intent::MoveSessionDown => self.move_session(1),
            Intent::ToggleSidebarGrouping => {
                let grouped = self.toggle_sidebar_grouping();
                let message = if grouped {
                    self.strings.sidebar_grouped()
                } else {
                    self.strings.sidebar_flat()
                };
                self.toast(Toast::info(message));
                Outcome::effects(vec![])
            }
            Intent::SwitchWorkspace => {
                self.page = Page::Sessions;
                self.toast(Toast::info(self.strings.workspace_pick().to_string()));
                Outcome::effects(vec![Effect::ListWorkspaces])
            }
            Intent::OpenWorkspaceBrowser => {
                self.toast(Toast::info(self.strings.workspace_browse().to_string()));
                Outcome::effects(vec![Effect::BrowseDirectories { path: None }])
            }
            Intent::WorkspaceBrowseUp => {
                let parent = self
                    .workspace_browse
                    .as_ref()
                    .and_then(|listing| listing.parent.clone());
                let Some(parent) = parent else {
                    self.toast(Toast::warning(self.strings.workspace_empty().to_string()));
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::BrowseDirectories { path: Some(parent) }])
            }
            Intent::WorkspaceBrowseSelect => {
                let Some(listing) = self.workspace_browse.as_ref() else {
                    return Outcome::quiet();
                };
                let index = self.selection_for(Scope::Sessions);
                let Some(entry) = listing.entries.get(index) else {
                    return Outcome::quiet();
                };
                let path = entry.path.clone();
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.session_new().to_string(),
                    field: PromptField::NewSessionTitle,
                    value: String::new(),
                });
                self.workspace_path = Some(path);
                Outcome::effects(vec![])
            }

            // ---- agent transcript ----------------------------------------
            Intent::FocusComposer => {
                self.page = Page::Agent;
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
                Outcome::effects(vec![])
            }
            Intent::ToggleReasoningExpanded => {
                // Reasoning blocks are the collapsible ones the desktop folds
                // by default; toggling them all is the whole-body equivalent.
                for index in 0..self.transcript.len() {
                    let is_reasoning = self.transcript.block(index).is_some_and(|block| {
                        block.kind == vibex_desktop_model::TimelineRowKind::Reasoning
                    });
                    if is_reasoning {
                        self.transcript.toggle_block(index);
                    }
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
            Intent::PreviousPanel => {
                self.cycle_session_destination(false);
                Outcome::effects(vec![])
            }
            Intent::NextPanel => {
                self.cycle_session_destination(true);
                Outcome::effects(vec![])
            }
            Intent::OpenChanges => {
                self.select_session_destination(vibex_ui::shell::SessionDestination::Changes);
                self.load_changes()
            }
            Intent::OpenFiles => {
                self.select_session_destination(vibex_ui::shell::SessionDestination::Files);
                self.load_files()
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
                effects.push(Effect::SendMessage {
                    session_id,
                    text,
                    attachments,
                });
                Outcome::effects(effects)
            }
            Intent::AttachImage => Outcome::effects(vec![Effect::ReadClipboardImage]),
            Intent::PasteClipboard => Outcome::effects(vec![Effect::ReadClipboard]),
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
                self.page = Page::Agent;
                if !self.begin_search() {
                    self.toast(Toast::info(self.strings.transcript_empty()));
                }
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
                    title: self.strings.composer_placeholder().to_string(),
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
                self.composer.delete_word_before();
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

            // ---- files / changes -------------------------------------------
            Intent::OpenSelectedFile => {
                let index = self.selection_for(Scope::Files);
                match self.file_rows.get(index).cloned() {
                    Some(entry) if entry.kind == vibex_core::FileEntryKind::File => {
                        let workspace_id = self.active_workspace_id();
                        match workspace_id {
                            Some(workspace_id) => Outcome::effects(vec![Effect::ReadFile {
                                workspace_id,
                                path: entry.path,
                            }]),
                            None => Outcome::quiet(),
                        }
                    }
                    _ => Outcome::quiet(),
                }
            }
            Intent::EditSelectedFile => {
                let index = self.selection_for(Scope::Files);
                match self.file_rows.get(index).cloned() {
                    Some(entry) if entry.kind == vibex_core::FileEntryKind::File => {
                        Outcome::effects(vec![Effect::EditExternally {
                            title: entry.name.clone(),
                            body: entry.path.clone(),
                        }])
                    }
                    _ => Outcome::quiet(),
                }
            }
            Intent::ToggleFileTreeExpanded => Outcome::effects(vec![]),
            Intent::FileSearch => {
                self.filtering = true;
                Outcome::effects(vec![])
            }
            Intent::ShowDiff => {
                let index = self.selection_for(Scope::Changes);
                let (Some(status), Some(workspace_id)) =
                    (self.git_status.as_ref(), self.active_workspace_id())
                else {
                    return Outcome::quiet();
                };
                let Some(entry) = status.changes.get(index) else {
                    return Outcome::quiet();
                };
                let path = entry.path.clone();
                Outcome::effects(vec![Effect::LoadGitDiff { workspace_id, path }])
            }
            Intent::GitStageSelected => self.git_stage(true),
            Intent::GitUnstageSelected => self.git_stage(false),
            Intent::GitCommit => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.transcript_git().to_string(),
                    field: PromptField::CommitMessage,
                    value: String::new(),
                });
                Outcome::effects(vec![])
            }
            Intent::GitRevert => {
                let index = self.selection_for(Scope::Changes);
                let Some(status) = self.git_status.as_ref() else {
                    return Outcome::quiet();
                };
                let Some(entry) = status.changes.get(index) else {
                    return Outcome::quiet();
                };
                let path = entry.path.clone();
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.git_revert_title().to_string(),
                    body: format!("{path}\n\n{}", self.strings.git_revert_warning()),
                    confirm: Intent::GitRevert,
                });
                Outcome::effects(vec![])
            }
            Intent::GitHistory => match self.active_workspace_id() {
                Some(workspace_id) => {
                    Outcome::effects(vec![Effect::LoadGitHistory { workspace_id }])
                }
                None => Outcome::quiet(),
            },
            Intent::GitBranches => match self.active_workspace_id() {
                Some(workspace_id) => {
                    Outcome::effects(vec![Effect::LoadGitBranches { workspace_id }])
                }
                None => Outcome::quiet(),
            },
            Intent::WorktreeMenu => match self.active_workspace_id() {
                Some(workspace_id) => {
                    Outcome::effects(vec![Effect::LoadWorktrees { workspace_id }])
                }
                None => Outcome::quiet(),
            },
            Intent::WorktreePreflight => {
                let Some(workspace_id) = self.active_workspace_id() else {
                    return Outcome::quiet();
                };
                let Some(path) = self.selected_worktree_path() else {
                    self.toast(Toast::warning(self.strings.nothing_here().to_string()));
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::WorktreePreflight { workspace_id, path }])
            }
            Intent::WorktreeCreate => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.worktree_create_title().to_string(),
                    field: PromptField::WorktreeBranch,
                    value: String::new(),
                });
                Outcome::effects(vec![])
            }

            // ---- terminal ---------------------------------------------------
            Intent::NewTerminal | Intent::CloseTerminal | Intent::TerminalToggleFollow => {
                // The embedded terminal pane is an M4 feature; the terminal
                // page explains that rather than pretending to work.
                self.toast(Toast::info(
                    self.strings.toast_action_unavailable().to_string(),
                ));
                Outcome::quiet()
            }

            // ---- usage -------------------------------------------------------
            Intent::UsageSessionScope => {
                self.usage_scope_session = !self.usage_scope_session;
                Outcome::effects(vec![Effect::LoadUsage])
            }

            // ---- recovery ----------------------------------------------------
            Intent::ActivateRecoveryAction => {
                let index = self.selection_for(Scope::Recovery);
                match RecoveryAction::ALL.get(index) {
                    Some(RecoveryAction::Diagnostics) => self.perform(Intent::ExportDiagnostics),
                    Some(RecoveryAction::BackupCreate) => self.perform(Intent::CreateBackup),
                    Some(RecoveryAction::BackupInspect) => self.perform(Intent::InspectBackup),
                    Some(RecoveryAction::BackupRestore) => self.perform(Intent::RestoreBackup),
                    None => Outcome::quiet(),
                }
            }
            Intent::ExportDiagnostics => self.guard(
                BackendOperation::RecoveryDiagnosticsExport,
                Effect::ExportDiagnostics,
            ),
            Intent::CreateBackup => {
                self.guard(BackendOperation::RecoveryBackupCreate, Effect::CreateBackup)
            }
            Intent::InspectBackup => self.guard(
                BackendOperation::RecoveryBackupInspect,
                Effect::InspectBackup,
            ),
            Intent::RestoreBackup => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.recovery_backup_restore().to_string(),
                    field: PromptField::RestoreBackupId,
                    value: String::new(),
                });
                Outcome::effects(vec![])
            }

            // ---- settings ----------------------------------------------------
            Intent::ActivateSetting => self.activate_setting(),
            Intent::SettingPrevious => self.step_setting(-1),
            Intent::SettingNext => self.step_setting(1),
            Intent::ResetSetting => self.begin_reset_setting(),
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

    fn perform_overlay(&mut self, intent: Intent) -> Outcome {
        // Global escape hatches still work with an overlay open.
        match intent {
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                return Outcome::effects(vec![]);
            }
            Intent::RequestQuit => return self.confirm_quit(),
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
            Overlay::RuntimePicker { selected } => match intent {
                Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                    self.overlay = None;
                    self.apply_runtime_selection(selected)
                }
                Intent::SelectNext => {
                    let count = self.runtime_option_count();
                    self.overlay = Some(Overlay::RuntimePicker {
                        selected: (selected + 1) % count.max(1),
                    });
                    Outcome::effects(vec![])
                }
                Intent::SelectPrevious => {
                    self.overlay = Some(Overlay::RuntimePicker {
                        selected: selected.saturating_sub(1),
                    });
                    Outcome::effects(vec![])
                }
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
                let Some(entry) = matches.get(selected).copied() else {
                    return Outcome::quiet();
                };
                self.overlay = None;
                self.remember_command(entry.intent);
                self.perform(entry.intent)
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
                Outcome::effects(vec![Effect::ListSessions {
                    include_archived: self.show_archived,
                }])
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
                let Some(session_id) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::DeleteSession { session_id }])
            }
            Intent::ArchiveSession => {
                let Some(session_id) = self.selected_session_id().cloned() else {
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
            Intent::GitRevert => {
                let index = self.selection_for(Scope::Changes);
                let (Some(status), Some(workspace_id)) =
                    (self.git_status.as_ref(), self.active_workspace_id())
                else {
                    return Outcome::quiet();
                };
                let Some(entry) = status.changes.get(index) else {
                    return Outcome::quiet();
                };
                let path = entry.path.clone();
                self.guard(
                    BackendOperation::GitRevert,
                    Effect::GitRevert { workspace_id, path },
                )
            }
            Intent::RequestQuit => {
                self.should_quit = true;
                Outcome::effects(vec![])
            }
            other => self.perform(other),
        }
    }

    fn submit_prompt(&mut self, field: PromptField, value: String, title: String) -> Outcome {
        let trimmed = value.trim().to_string();
        match field {
            PromptField::RenameSession => {
                let Some(session_id) = self.selected_session_id().cloned() else {
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
            PromptField::NewSessionTitle => {
                let workspace_root = self
                    .workspace_path
                    .clone()
                    .or_else(|| {
                        self.active_session()
                            .map(|session| session.workspace_root.clone())
                    })
                    .unwrap_or_default();
                Outcome::effects(vec![Effect::CreateSession {
                    workspace_root,
                    title: (!trimmed.is_empty()).then_some(trimmed),
                }])
            }
            PromptField::WorkspacePath => {
                Outcome::effects(vec![Effect::OpenWorkspace { root_path: trimmed }])
            }
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
            PromptField::CommitMessage => {
                let Some(workspace_id) = self.active_workspace_id() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::GitCommit {
                    workspace_id,
                    message: trimmed,
                }])
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
            PromptField::McpServerName
            | PromptField::SkillName
            | PromptField::PromptName
            | PromptField::HookName => {
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
                    _ => self.management_data.hooks.get(index).map(|hook| {
                        crate::app::ManagementEntryEdit::Hook {
                            hook_id: hook.id.clone(),
                            display_name: trimmed,
                        }
                    }),
                };
                let _ = title;
                match entry {
                    Some(entry) => Outcome::effects(vec![Effect::UpdateEntry { entry }]),
                    None => Outcome::quiet(),
                }
            }
            PromptField::WorktreeBranch => {
                if trimmed.is_empty() {
                    return Outcome::quiet();
                }
                match self.active_workspace_id() {
                    Some(workspace_id) => Outcome::effects(vec![Effect::WorktreeCreate {
                        workspace_id,
                        branch_name: trimmed,
                    }]),
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
            PromptField::RestoreBackupId => {
                if trimmed.is_empty() {
                    return Outcome::quiet();
                }
                self.guard(
                    BackendOperation::RecoveryBackupRestore,
                    Effect::RestoreBackup { backup_id: trimmed },
                )
            }
        }
    }

    // ---- focused helpers --------------------------------------------------

    fn confirm_quit(&mut self) -> Outcome {
        self.overlay = Some(Overlay::Confirm {
            title: self.strings.close().to_string(),
            body: self.strings.help_hint().to_string(),
            confirm: Intent::RequestQuit,
        });
        Outcome::effects(vec![])
    }

    fn go_back(&mut self) -> Outcome {
        if self.overlay.is_some() {
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
        // it must not be a room with no door: `Esc` from a session returns to
        // the session list rather than only moving focus off the draft. `Tab`
        // is what walks the panes, and the draft is left where it was.
        if self.focus == Focus::Composer && !self.page.is_session_page() {
            self.focus = Focus::Main;
            return Outcome::effects(vec![]);
        }
        if self.page.is_session_page() && self.page != Page::Agent {
            self.select_session_destination(vibex_ui::shell::SessionDestination::Agent);
            return Outcome::effects(vec![]);
        }
        if self.page != Page::Sessions {
            self.select_global(vibex_ui::shell::GlobalDestination::Sessions);
        }
        Outcome::effects(vec![])
    }

    fn contextual_cancel(&mut self) -> Outcome {
        if self.overlay.is_some() {
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
            return Outcome::effects(vec![Effect::Interrupt { session_id }]);
        }
        self.confirm_quit()
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
        let scope = if self.page == Page::Agent {
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
        let next = (current + delta).clamp(0, count as i64 - 1) as usize;
        self.set_selection(scope, next);
        if scope == Scope::Agent {
            self.scroll.follow = false;
            let offset = self.transcript.offset_of_block(next);
            self.scroll.offset = offset;
        }
        Outcome::effects(vec![])
    }

    fn scroll_by(&mut self, direction: i64, page: bool) {
        let step = if page {
            usize::from(self.viewport.1).saturating_sub(4).max(1)
        } else {
            (usize::from(self.viewport.1) / 2).max(1)
        };
        let current = self.scroll.offset as i64;
        self.scroll.follow = false;
        self.scroll.offset = (current + direction * step as i64).max(0) as usize;
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

    fn cycle_session_destination(&mut self, forward: bool) {
        use vibex_ui::shell::SessionDestination;
        let order = [
            SessionDestination::Agent,
            SessionDestination::Files,
            SessionDestination::Changes,
            SessionDestination::Terminal,
        ];
        let current = order
            .iter()
            .position(|candidate| *candidate == self.navigation.session)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % order.len()
        } else {
            (current + order.len() - 1) % order.len()
        };
        self.select_session_destination(order[next]);
    }

    /// Path of the highlighted managed worktree, when the page has one.
    pub fn selected_worktree_path(&self) -> Option<String> {
        let snapshot = self.worktrees.as_ref()?;
        let index = self.selection_for(Scope::Changes);
        snapshot
            .managed_worktrees
            .get(index)
            .map(|worktree| worktree.worktree_path.clone())
    }

    fn begin_new_session(&mut self) -> Outcome {
        self.overlay = Some(Overlay::Prompt {
            title: self.strings.session_new().to_string(),
            field: PromptField::NewSessionTitle,
            value: String::new(),
        });
        self.workspace_path = None;
        Outcome::effects(vec![Effect::ListWorkspaces])
    }

    fn begin_rename_session(&mut self) -> Outcome {
        let Some(session) = self.active_session() else {
            return Outcome::quiet();
        };
        let title = session.title.clone();
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
        let (text, images) = self.composer.take_with_attachments();
        self.completion = None;
        // A message written while a turn is running is held rather than sent:
        // the runtime would have to interleave it with work already in flight.
        if self.session_running() {
            self.enqueue(text, images);
            self.toast(Toast::info(self.strings.queue_held().to_string()));
            return Outcome::effects(vec![]);
        }
        self.history.push(text.clone());
        self.scroll.follow = true;
        let attachments = images
            .iter()
            .map(crate::composer::message_attachment)
            .collect();
        Outcome::effects(vec![Effect::SendMessage {
            session_id,
            text,
            attachments,
        }])
    }

    fn steer_composer(&mut self) -> Outcome {
        if self.composer.is_empty() {
            self.toast(Toast::warning(self.strings.composer_empty().to_string()));
            return Outcome::quiet();
        }
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let (text, images) = self.composer.take_with_attachments();
        self.history.push(text.clone());
        // Remote seats have no steering RPC, so the worker falls back to
        // interrupt + resend and says so.
        let fallback = self.seat != crate::view::SeatKind::Authority;
        if fallback {
            self.toast(Toast::info(
                self.strings.composer_steer_unavailable().to_string(),
            ));
        }
        let attachments = images
            .iter()
            .map(crate::composer::message_attachment)
            .collect();
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
    /// on the first row rather than on nothing.
    pub fn show_runtime_picker(&mut self) {
        let selected = self.current_runtime_option_index().unwrap_or(0);
        self.overlay = Some(Overlay::RuntimePicker { selected });
    }

    fn runtime_option_count(&self) -> usize {
        self.runtime_options
            .as_ref()
            .map(|catalog| catalog.options.len())
            .unwrap_or(0)
    }

    fn apply_runtime_selection(&mut self, index: usize) -> Outcome {
        let Some(catalog) = self.runtime_options.as_ref() else {
            return Outcome::quiet();
        };
        let Some(option) = catalog.options.get(index) else {
            return Outcome::quiet();
        };
        if option.availability != vibex_core::RuntimeOptionAvailability::Available {
            let message = self.strings.runtime_unavailable().to_string();
            self.toast(Toast::warning(message));
            return Outcome::quiet();
        }
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        self.guard(
            BackendOperation::AgentSwitchRuntime,
            Effect::SwitchRuntime {
                session_id,
                selection: option.selection.clone(),
            },
        )
    }

    fn refresh_current_page(&mut self) -> Outcome {
        match self.page {
            Page::Sessions => Outcome::effects(vec![Effect::ListSessions {
                include_archived: self.show_archived,
            }]),
            Page::Agent => Outcome::effects(vec![Effect::RefreshTimeline]),
            Page::Devices => Outcome::effects(vec![Effect::ListDevices]),
            Page::Providers => Outcome::effects(vec![Effect::ListProfiles]),
            Page::Agents => Outcome::effects(vec![Effect::ListAgents]),
            Page::Mcp => Outcome::effects(vec![Effect::ListMcp]),
            Page::Skills => Outcome::effects(vec![Effect::ListSkills]),
            Page::Prompts => Outcome::effects(vec![Effect::ListPrompts]),
            Page::Hooks => Outcome::effects(vec![Effect::ListHooks]),
            Page::Usage => Outcome::effects(vec![Effect::LoadUsage]),
            Page::Files => self.load_files(),
            Page::Changes => self.load_changes(),
            Page::Management | Page::Recovery | Page::Settings | Page::Help | Page::Terminal => {
                Outcome::effects(vec![])
            }
        }
    }

    fn load_files(&mut self) -> Outcome {
        match self.active_workspace_id() {
            Some(workspace_id) => Outcome::effects(vec![Effect::LoadFileTree { workspace_id }]),
            None => Outcome::quiet(),
        }
    }

    fn load_changes(&mut self) -> Outcome {
        match self.active_workspace_id() {
            Some(workspace_id) => Outcome::effects(vec![Effect::LoadGitStatus { workspace_id }]),
            None => Outcome::quiet(),
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
            Page::Hooks => match self.management_data.hooks.get(index) {
                Some(hook) => Outcome::effects(vec![Effect::ToggleHook {
                    hook_id: hook.id.clone(),
                    enabled: hook.status != vibex_core::HookStatus::Enabled,
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
            Page::Hooks => PromptField::HookName,
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

    fn git_stage(&mut self, stage: bool) -> Outcome {
        let index = self.selection_for(Scope::Changes);
        let Some(workspace_id) = self.active_workspace_id() else {
            return Outcome::quiet();
        };
        let Some(status) = self.git_status.as_ref() else {
            return Outcome::quiet();
        };
        let Some(entry) = status.changes.get(index) else {
            return Outcome::quiet();
        };
        let path = entry.path.clone();
        let operation = if stage {
            BackendOperation::GitStage
        } else {
            BackendOperation::GitUnstage
        };
        self.guard(
            operation,
            Effect::GitStage {
                workspace_id,
                path,
                stage,
            },
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
            through_sequence: i64::MAX,
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
        let backend = std::sync::Arc::new(vibex_backend::DisconnectedBackend);
        let facade = vibex_backend::BackendFacade::new(
            vibex_backend::BackendCapabilitySnapshot::desktop_native_v1(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
        );
        let mut app = App::new(facade, crate::app::AppOptions::default());
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
        assert_eq!(app.overlay, Some(Overlay::RuntimePicker { selected: 1 }));
    }

    #[test]
    fn escape_from_a_session_returns_to_the_list_with_the_draft_kept() {
        // The composer is where the keyboard lands when a session opens, so
        // `Esc` has to be able to leave the page from it: a reader who could
        // not get back to the session list was stuck in the session.
        let mut app = capable_app();
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
        assert_eq!(app.overlay, Some(Overlay::RuntimePicker { selected: 1 }));
        assert!(app.runtime_option_is_current(&app.runtime_options.as_ref().unwrap().options[1]));

        // Choosing the unavailable entry refuses instead of issuing a switch
        // the runtime would reject.
        let refused = app.perform(Intent::ConfirmOverlay);
        assert!(refused.effects.is_empty());

        // Choosing the available one asks for exactly that selection.
        app.overlay = Some(Overlay::RuntimePicker { selected: 0 });
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
            crate::app::ManagementEntryEdit::Hook {
                hook_id: vibex_core::HookId::new(),
                display_name: "d".into(),
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
        let workspace_id = vibex_core::WorkspaceId::new();
        let keys = [
            Effect::GitStage {
                workspace_id: workspace_id.clone(),
                path: "a".into(),
                stage: true,
            }
            .key(),
            Effect::GitCommit {
                workspace_id: workspace_id.clone(),
                message: "m".into(),
            }
            .key(),
            Effect::GitRevert {
                workspace_id: workspace_id.clone(),
                path: "a".into(),
            }
            .key(),
            Effect::WorktreeCreate {
                workspace_id: workspace_id.clone(),
                branch_name: "b".into(),
            }
            .key(),
            Effect::WorktreePreflight {
                workspace_id: workspace_id.clone(),
                path: "a".into(),
            }
            .key(),
            Effect::LoadGitHistory { workspace_id }.key(),
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
}
