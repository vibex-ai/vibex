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
    App, Availability, ClipboardWanted, ComposerTarget, Effect, Focus, Overlay, Page,
    PendingCreation, PromptField, RunOption, RunOptionKey, RunOptionKind, RuntimePickerView, Toast,
    WorkspacePickerRow,
};
use crate::composer::{CompletionLists, CompletionMenu, CompletionTab, CompletionTrigger};
use crate::keymap::Scope;
use crate::runtime_picker::{PICKER_PAGE_ROWS, RuntimePickerRow};
use crate::schedule::ScheduleEditTarget;
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
    pub fn effects(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            dirty: true,
        }
    }

    pub fn quiet() -> Self {
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
        // Approaching the top is the asking gesture for older history;
        // `LoadOlderHistory` is the explicit one. The controller refuses once
        // two pages are in flight — and once the history is exhausted — so
        // holding a scroll key down cannot fire a request per repeat.
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

    /// Queue an older-page fetch when the reader is close to the top.
    fn request_older_history_at_top(&mut self, outcome: &mut Outcome) {
        if let Some(effect) = self.prefetch_older_history() {
            outcome.effects.push(effect);
            outcome.dirty = true;
        }
    }

    fn perform_intent(&mut self, intent: Intent) -> Outcome {
        // `ctrl+r` is the universal refresh key; inside the Key prompt it is
        // the one way a terminal can reveal what it is about to store.
        if matches!(intent, Intent::Refresh | Intent::ReloadManagement)
            && matches!(
                self.overlay,
                Some(Overlay::Prompt {
                    field: PromptField::ProviderSecret,
                    ..
                })
            )
        {
            self.reveal_provider_secret = !self.reveal_provider_secret;
            return Outcome {
                effects: Vec::new(),
                dirty: true,
            };
        }
        // An overlay swallows most intents; the overlay's own intents and the
        // global escape hatches still apply.
        if self.overlay.is_some() {
            return self.perform_overlay(intent);
        }
        if self.filtering {
            return self.perform_filter(intent);
        }

        // A collapsed paste under the caret is opened by `Enter` before
        // anything else looks at the key. It is an edit to the draft rather
        // than a send, so it is answered even where sending is not — a
        // disconnected seat still lets a reader read what they pasted — and it
        // is answered here rather than in the send, which is where the
        // availability of the runtime is decided.
        if matches!(intent, Intent::SubmitComposer) && self.composer.expand_paste_at_cursor() {
            self.refresh_completion();
            return Outcome::effects(vec![]);
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
            // A pairing panel is a surface inside the page, so `Esc` puts it
            // down before it becomes a walk back to the page the reader came
            // from.
            Intent::Back if self.device_panel_is_open() => self.close_device_panel(),
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
                // The centre opens on the Agent roster, which is a read: a page
                // that showed an empty table until the reader pressed `r` would
                // be a page that looks broken on arrival.
                Outcome::effects(self.config_section_effects())
            }
            Intent::ToggleSidebar => {
                self.toggle_sidebar_collapsed();
                Outcome::effects(vec![])
            }
            Intent::TakeOverHome => {
                // Only when the host says the home is free. The key exists on
                // every run, so pressing it while this client is the authority
                // or while the runtime is answering has to say why it does
                // nothing rather than ending the run for no reason.
                if self.handover_offered() {
                    self.handover_requested = true;
                } else {
                    let message = self.strings.toast_offline().to_string();
                    self.toast(Toast::info(message));
                }
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
            // `←`/`→` move across the row's own buttons rather than along the
            // list: the two axes do different jobs, and the buttons are the only
            // thing beside the list's cursor.
            Intent::SessionControlPrevious => self.step_session_control(-1),
            Intent::SessionControlNext => self.step_session_control(1),
            Intent::ScrollPageUp => {
                if self.page == Page::Management {
                    // The detail pane has no cursor: the page keys move it
                    // while the record keys move the row it describes.
                    return self.scroll_config_detail(-6);
                }
                if self.scrolls_session_list() {
                    let step = self.session_page_step();
                    self.scroll_session_list(-(step as i64));
                } else {
                    self.scroll_by(-1, true);
                }
                Outcome::effects(vec![])
            }
            Intent::ScrollPageDown => {
                if self.page == Page::Management {
                    return self.scroll_config_detail(6);
                }
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
                if self.page == Page::Management {
                    self.config_center.filtering = true;
                    self.config_center.filter.clear();
                    self.config_center.set_cursor(0);
                    return Outcome::effects(vec![]);
                }
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
                if self.page == Page::Management {
                    self.config_center.filter.clear();
                    self.config_center.filtering = false;
                    self.config_center.set_cursor(0);
                    return Outcome::effects(vec![]);
                }
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
                // The keyboard may have stepped onto one of the row's own
                // buttons; `Enter` then presses it rather than opening the row
                // behind it. A row the keyboard has not stepped into opens as it
                // always has.
                if self.page == Page::Sessions
                    && let Some(outcome) = self.activate_session_control()
                {
                    return outcome;
                }
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
                //
                // Opening a session is a request to work in it, so the keyboard
                // lands in the composer: `open_session` did that when it moved
                // the page, and there is no second "enter the workbench" step to
                // take.
                self.open_session_effects(session_id)
            }
            Intent::NewSession => {
                // The list shows rows, and its cursor is on one of them: `n`
                // starts a session in the project the reader is pointing at,
                // the way `r` and `d` act on the session they are pointing at.
                // A cursor on a session row, or a reader who is not on the
                // list at all, leaves the page's own workspace alone.
                self.aim_new_session_at_selected_heading();
                self.begin_new_session()
            }
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
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                // One gesture, two directions: a session that is already in the
                // archived list is restored, and nothing is destroyed either
                // way, so only archiving asks for confirmation.
                if self
                    .session_by_id(&session_id)
                    .is_some_and(|session| session.archived_at_ms.is_some())
                {
                    return Outcome::effects(vec![Effect::UnarchiveSession { session_id }]);
                }
                self.overlay = Some(Overlay::Confirm {
                    title: self.strings.session_archive().to_string(),
                    body: self.strings.session_confirm_archive().to_string(),
                    confirm: Intent::ArchiveSession,
                });
                self.set_selection(Scope::Overlay, 0);
                Outcome::effects(vec![])
            }
            Intent::UnarchiveSession => {
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::UnarchiveSession { session_id }])
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
            Intent::ToggleChildBranch => {
                // A row with no children has no branch: the key does nothing
                // rather than folding a heading the reader did not point at.
                let folded = self.toggle_child_branch();
                Outcome::effects(folded.into_iter().collect())
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
                } else {
                    // Opening a row is a reading gesture, so the view holds the
                    // frame it was on — the same thing a click on the row does.
                    // A running turn's new rows otherwise carry the section the
                    // reader just opened away under their eyes.
                    self.pin_transcript_view();
                    if self
                        .transcript
                        .block(index)
                        .and_then(|block| block.fold.as_ref())
                        .is_some_and(crate::transcript::TurnFold::is_control)
                    {
                        // Opening a turn's fold can reveal activities the
                        // summary page withheld, and it opens them all at once
                        // rather than the one the reader pointed at.
                        self.note_all_block_details();
                    } else {
                        // An activity opens onto output a summary page withheld,
                        // so the expand is what asks for the read.
                        self.note_block_detail(index);
                    }
                }
                Outcome::effects(self.take_timeline_detail_effects())
            }
            Intent::ToggleAllBlocksExpanded => {
                let expand = !self.transcript.all_expanded();
                self.transcript.toggle_all(expand);
                // "Show me everything" is a reading gesture too, and the whole
                // transcript just changed shape under the reader.
                self.pin_transcript_view();
                if let Some(index) = self
                    .transcript
                    .visible_block(self.selection_for(Scope::Agent))
                {
                    self.set_selection(Scope::Agent, index);
                }
                if expand {
                    self.note_all_block_details();
                }
                Outcome::effects(self.take_timeline_detail_effects())
            }
            Intent::ToggleReasoningExpanded => {
                self.transcript.toggle_reasoning();
                self.pin_transcript_view();
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
                // The one block whose details are not a read: a question the
                // Agent is blocked on opens the card that answers it, which is
                // also how a reader who put the form down with `Esc` asks for
                // it back.
                if self.selected_block_is_a_pending_question(true) {
                    self.open_elicitation();
                } else if self.selected_block_is_a_pending_question(false) {
                    self.open_approval();
                }
                if self.overlay.is_some() {
                    return Outcome::effects(vec![]);
                }
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
                    // Nothing of this client's is held, so the send the reader
                    // is asking for is the one another client is waiting on:
                    // the same instruction travels as a deadline of now, and
                    // the holder releases it through the ordinary send path.
                    return self.send_held_schedule_now();
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
                self.history_log.push(text.clone());
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
            Intent::ScheduleMessage => self.open_message_schedule(),
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
                    self.team_confirm_cancel = None;
                }
                let effects = if self.dock_open {
                    self.load_team(false).into_iter().collect()
                } else {
                    Vec::new()
                };
                Outcome::effects(effects)
            }
            Intent::DockActivate => {
                if let Some(outcome) = self.activate_child_dock_row() {
                    return outcome;
                }
                if let Some(outcome) = self.activate_team_dock_row() {
                    return outcome;
                }
                self.activate_dock_row();
                Outcome::effects(vec![])
            }
            Intent::GotoParentSession => {
                // The page the reader came from, one level up: a root session
                // has none, and saying so is better than a key that seems dead.
                let Some(parent) = self.parent_session_id() else {
                    let message = self.strings.session_no_parent().to_string();
                    self.toast(Toast::info(message));
                    return Outcome::quiet();
                };
                self.open_session_effects(parent)
            }
            Intent::OpenChildSessions => {
                let Some(open) = self.selected_session_id().cloned() else {
                    return Outcome::quiet();
                };
                match self.children.child_ids(open.as_str()).as_slice() {
                    // Nothing is known yet: the key becomes the read that finds
                    // out, and the next press has something to act on.
                    [] => {
                        if let Some(effect) = self.load_child_page(Some(open), false) {
                            let message = self.strings.dock_children_loading().to_string();
                            self.toast(Toast::info(message));
                            return Outcome::effects(vec![effect]);
                        }
                        let message = self.strings.dock_children_none().to_string();
                        self.toast(Toast::info(message));
                        Outcome::quiet()
                    }
                    // One worker is a destination, not a list to choose from.
                    [only] => self.open_session_effects(only.clone()),
                    // Several: the dock is where they are named, and the key
                    // lands the cursor on the first of them.
                    _ => {
                        self.dock_open = true;
                        self.focus_dock_children();
                        Outcome::effects(vec![])
                    }
                }
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
            Intent::ComposerHistoryPrevious | Intent::ComposerHistoryNext => {
                let previous = intent == Intent::ComposerHistoryPrevious;
                self.step_composer_history(previous);
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

            // ---- configuration centre --------------------------------------
            Intent::ConfigModulePrevious => {
                self.config_center.step_module(-1);
                Outcome::effects(self.config_section_effects())
            }
            Intent::ConfigModuleNext => {
                self.config_center.step_module(1);
                Outcome::effects(self.config_section_effects())
            }
            Intent::ConfigSectionPrevious => {
                self.config_center.step_section(-1);
                Outcome::effects(self.config_section_effects())
            }
            Intent::ConfigSectionNext => {
                self.config_center.step_section(1);
                Outcome::effects(self.config_section_effects())
            }
            Intent::ConfigPaneNext => {
                self.config_center.step_zone(1);
                Outcome::effects(vec![])
            }
            Intent::ConfigPanePrevious => {
                self.config_center.step_zone(-1);
                Outcome::effects(vec![])
            }
            // Every record-scoped verb answers for the pane the reader is in.
            // The centre has three panes and one cursor each, and a verb that
            // acts on "the row" needs the reader to be on one: a reader whose
            // focus is on the module tabs is not, and pressing `d` there is not
            // a request to delete whatever row the cursor happens to be on in a
            // pane they are not looking at.
            Intent::ConfigEntryActions if self.config_rows_focused() => {
                self.open_entry_actions()
            }
            Intent::SidebarRowActions => self.open_sidebar_row_actions(),
            Intent::ConfigEntryCreate if self.config_rows_focused() => {
                self.begin_entry_form(None)
            }
            Intent::ConfigEntryDelete if self.config_rows_focused() => {
                self.confirm_entry_delete()
            }
            // The three verbs every record shares. They mean the same thing in
            // every module, which is why they are keys; everything a *kind* of
            // record can also do lives in the action menu, where the row itself
            // decides what is offered.
            Intent::ToggleSelectedEntry if self.config_rows_focused() => {
                self.toggle_selected_entry()
            }
            Intent::EditSelectedEntry if self.config_rows_focused() => {
                let row = self.selected_config_row();
                self.begin_entry_form(row)
            }
            Intent::ConfigFormSave => Outcome::quiet(),
            // The form's own key, reaching here only when no form is open.
            Intent::ConfigFormCopy => Outcome::quiet(),
            // Confirmed: the form is dropped, and with it the edits it held.
            Intent::ConfigFormDiscard => {
                self.pending_form = None;
                self.overlay = None;
                Outcome::effects(vec![])
            }
            Intent::ConfigModels if self.config_rows_focused() => self.open_model_editor(),
            Intent::ConfigImport if self.config_rows_focused() => self.open_native_import(),
            Intent::ConfigExport if self.config_rows_focused() => self.open_native_export(),
            Intent::ConfigDiscover if self.config_rows_focused() => {
                self.discover_local_agents()
            }
            // A record verb pressed from another pane: the reader asked about
            // the pane they are in, and that pane has no records, so nothing
            // happens rather than something happening somewhere else.
            Intent::ToggleSelectedEntry
            | Intent::EditSelectedEntry
            | Intent::ConfigEntryActions
            | Intent::ConfigEntryCreate
            | Intent::ConfigEntryDelete
            | Intent::ConfigModels
            | Intent::ConfigImport
            | Intent::ConfigExport
            | Intent::ConfigDiscover => Outcome::quiet(),
            // The Model editor's own keys, reaching here only when no overlay is
            // open to receive them.
            Intent::ConfigModelToggle
            | Intent::ConfigModelAdd
            | Intent::ConfigModelRemove
            | Intent::ConfigModelFetch
            | Intent::ConfigModelEdit
            | Intent::ConfigModelEnableAll
            | Intent::ConfigModelDisableAll
            | Intent::ConfigImportRefresh
            | Intent::ConfigExportSource
            | Intent::ConfigExportMode
            | Intent::ConfigExportPreview
            | Intent::ConfigExportApply
            | Intent::ConfigExportRollback => Outcome::quiet(),
            Intent::ConfigRegistryFilter => self.step_registry_narrowing(RegistryStep::Transport),
            Intent::ConfigRegistrySort => self.step_registry_narrowing(RegistryStep::Sort),
            Intent::ConfigRegistryPagePrevious => self.step_registry_page(-1),
            Intent::ConfigRegistryPageNext => self.step_registry_page(1),
            Intent::GotoDevices => {
                self.navigate_to(Page::Devices);
                Outcome::effects(self.device_page_effects())
            }

            // ---- management ------------------------------------------------
            Intent::ReloadManagement => self.refresh_current_page(),
            Intent::ActivateDevicePairing => self.activate_device_pairing(),
            Intent::DevicesTabNext => self.step_device_tab(1),
            Intent::DevicesTabPrevious => self.step_device_tab(-1),
            Intent::PairingRouteNext => self.cycle_pairing_route(1),
            Intent::PairingRoutePrevious => self.cycle_pairing_route(-1),
            Intent::AllowLanPairing => self.decide_lan_pairing(true),
            Intent::RefuseLanPairing => self.decide_lan_pairing(false),
            Intent::RevokeSelectedDevice => self.begin_revoke_device(),
            Intent::OpenDeviceAudit => Outcome::effects(vec![Effect::ListAudit]),

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
            | Intent::ResetRunOption
            // The schedule editor's own key. With no editor open there is no
            // send time in front of the reader to void.
            | Intent::ScheduleClear => Outcome::quiet(),
        }
    }

    // -----------------------------------------------------------------
    // the configuration centre
    // -----------------------------------------------------------------

    /// The rows the centre's table is drawing right now.
    fn visible_config_rows(&self) -> Vec<crate::config_center::ConfigRow> {
        self.config_center
            .visible(&self.management_data, self.strings)
    }

    fn selected_config_row(&self) -> Option<crate::config_center::ConfigRow> {
        self.config_center
            .selected(&self.management_data, self.strings)
    }

    /// Read whatever the area being opened needs and does not already hold.
    ///
    /// The list areas every module owns are read on entry; the registry and
    /// the import areas are read on demand, because a search costs a network
    /// round trip and a reader who never opens the registry should never pay
    /// for one.
    pub fn config_section_effects(&mut self) -> Vec<Effect> {
        use crate::config_center::ConfigSection;
        match (self.config_center.module, self.config_center.section) {
            (_, ConfigSection::Agents) => vec![
                Effect::ListAgents,
                Effect::LoadAgentTools,
                // The account column is a read of its own: the Agent list says
                // what an Agent is, and the authorization contexts say whether
                // it is signed in. Without this the column had no source at all
                // and read "Not checked" for every Agent forever.
                Effect::ListAgentAuth {
                    agent_id: None,
                    open: false,
                },
            ],
            (_, ConfigSection::Providers) => {
                let mut effects = vec![Effect::ListProfiles, Effect::LoadGateway];
                // The arrangement is a record of its own, read per Agent: a
                // list that cannot say what order the reader chose would show
                // the authority's order and call it theirs.
                let mut agents = self
                    .management_data
                    .providers
                    .iter()
                    .map(|profile| profile.agent_id.clone())
                    .collect::<Vec<_>>();
                agents.dedup();
                effects.extend(
                    agents
                        .into_iter()
                        .map(|agent_id| Effect::LoadProviderOrder { agent_id }),
                );
                effects
            }
            (_, ConfigSection::Servers) => vec![Effect::ListMcp],
            (_, ConfigSection::Installed) => vec![Effect::ListSkills],
            (_, ConfigSection::Prompts) => vec![Effect::ListPrompts],
            (module, ConfigSection::Catalog) => vec![Effect::SearchRegistry {
                module,
                query: self.config_center.filter.clone(),
                registry: self.config_center.registry,
            }],
            (module, ConfigSection::Sources) => vec![Effect::DiscoverSources { module }],
        }
    }

    /// `t` and `s`: ask the registry a narrower question.
    ///
    /// Only the registry on screen has either control, so a key that means
    /// nothing here does nothing, rather than changing a setting the reader
    /// cannot see. A narrowing is a new question, so it is read again from its
    /// first page.
    fn step_registry_narrowing(&mut self, step: RegistryStep) -> Outcome {
        use crate::config_center::{ConfigModule, ConfigSection};
        if self.config_center.section != ConfigSection::Catalog {
            return Outcome::quiet();
        }
        let mcp = self.config_center.module == ConfigModule::Mcp;
        match step {
            RegistryStep::Transport if mcp => self.config_center.registry.cycle_transport(),
            RegistryStep::Sort if !mcp => self.config_center.registry.cycle_sort(),
            _ => return Outcome::quiet(),
        }
        self.config_center.set_cursor(0);
        Outcome::effects(self.config_section_effects())
    }

    /// `,` and `.`: show the page before or after this one.
    ///
    /// MCP is asked for one page at a time, so moving is a read; the Skill
    /// index hands over one window which is cut here, so moving is a redraw.
    fn step_registry_page(&mut self, delta: isize) -> Outcome {
        use crate::config_center::{ConfigModule, ConfigSection};
        if self.config_center.section != ConfigSection::Catalog {
            return Outcome::quiet();
        }
        let pages = self.config_center.registry_pages(&self.management_data);
        // Paging past the last page the client holds asks the registry for the
        // page after it, when the registry said there is one. The cursor is the
        // registry's, so the answer is a page it chose rather than a window the
        // client widened on its own.
        if delta > 0 && self.config_center.registry.page + 1 >= pages {
            let cursor = self
                .management_data
                .market_note
                .as_ref()
                .and_then(|note| note.cursor.clone());
            if let Some(cursor) = cursor {
                return Outcome::effects(vec![Effect::LoadMoreSkillMarket {
                    cursor,
                    query: self.config_center.filter.clone(),
                    registry: self.config_center.registry,
                }]);
            }
        }
        if !self.config_center.registry.step_page(delta, pages) {
            return Outcome::quiet();
        }
        // The cursor belongs to the page it was chosen on: the next page starts
        // at its own first row rather than at an index the last page left.
        self.config_center.set_cursor(0);
        if self.config_center.module == ConfigModule::Mcp {
            return Outcome::effects(self.config_section_effects());
        }
        Outcome::effects(vec![])
    }

    /// The action button of a project or folder row: everything that heading can
    /// do, built from the heading itself.
    ///
    /// A session row has no menu here: its verbs are the buttons it already
    /// wears while the reader is on it, and a menu of the same five would be a
    /// second way to reach one list.
    fn open_sidebar_row_actions(&mut self) -> Outcome {
        let index = self.selection_for(Scope::Sessions);
        let Some(row) = self.sidebar_rows().get(index).cloned() else {
            return Outcome::quiet();
        };
        if row.session_id.is_some() {
            return Outcome::quiet();
        }
        let actions =
            crate::sessions::SidebarRowAction::for_heading(row.kind, !row.project_id.is_empty());
        if actions.is_empty() {
            return Outcome::quiet();
        }
        self.overlay = Some(Overlay::SidebarRowActions(
            crate::sessions::SidebarRowActions {
                title: row.label.clone(),
                row_id: row.id.clone(),
                actions,
                collapsed: row.collapsed,
                selected: 0,
            },
        ));
        Outcome::effects(vec![])
    }

    fn perform_sidebar_row_actions(
        &mut self,
        intent: Intent,
        menu: crate::sessions::SidebarRowActions,
    ) -> Outcome {
        let last = menu.actions.len().saturating_sub(1);
        match intent {
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                self.overlay = Some(Overlay::SidebarRowActions(
                    crate::sessions::SidebarRowActions {
                        selected: menu.selected.saturating_sub(1),
                        ..menu
                    },
                ));
                Outcome::effects(vec![])
            }
            Intent::SelectNext | Intent::ScrollPageDown => {
                self.overlay = Some(Overlay::SidebarRowActions(
                    crate::sessions::SidebarRowActions {
                        selected: (menu.selected + 1).min(last),
                        ..menu
                    },
                ));
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => {
                let Some(action) = menu.actions.get(menu.selected).copied() else {
                    return Outcome::quiet();
                };
                self.overlay = None;
                self.run_sidebar_row_action(&menu.row_id, action)
            }
            _ => Outcome::quiet(),
        }
    }

    /// Run one heading verb against the heading it was drawn for.
    ///
    /// The heading is resolved by its own id rather than by the cursor's index:
    /// a read that lands while the menu is open must not make the choice act on
    /// whichever heading took its place.
    fn run_sidebar_row_action(
        &mut self,
        row_id: &str,
        action: crate::sessions::SidebarRowAction,
    ) -> Outcome {
        use crate::sessions::SidebarRowAction;
        let Some(row) = self.sidebar_rows().into_iter().find(|row| row.id == row_id) else {
            return Outcome::quiet();
        };
        match action {
            SidebarRowAction::NewSession => {
                // The composing page is where a session is created, and it
                // already knows what to do with a directory it is handed: the
                // heading's button is that page with the project's own checkout
                // already chosen, not a second way to make a session.
                if let Some(root) = self.project_workspace_root(&row.project_id, &row.workspace_id)
                {
                    self.workspace_path = Some(root);
                }
                self.begin_new_session()
            }
            SidebarRowAction::NewFolder => self.begin_folder_prompt(&row, false),
            SidebarRowAction::RenameFolder => self.begin_folder_prompt(&row, true),
            SidebarRowAction::ToggleCollapse => {
                // When the authority owns the arrangement, so does the fold: a
                // collapse the desktop would not see is not a collapse.
                if let Some(effect) = self.sidebar_collapse_effect(&row) {
                    return Outcome::effects(vec![effect]);
                }
                self.toggle_sidebar_collapsed_for(&row.project_id);
                Outcome::effects(vec![])
            }
            SidebarRowAction::DeleteProject | SidebarRowAction::DeleteFolder => {
                let project = action == SidebarRowAction::DeleteProject;
                self.pending_sidebar_action = Some((row.id.clone(), action));
                self.overlay = Some(Overlay::Confirm {
                    title: if project {
                        self.strings.sidebar_action_delete_project()
                    } else {
                        self.strings.sidebar_action_delete_folder()
                    }
                    .to_string(),
                    body: format!(
                        "{}\n\n{}",
                        row.label,
                        if project {
                            self.strings.sidebar_confirm_delete_project()
                        } else {
                            self.strings.sidebar_confirm_delete_folder()
                        }
                    ),
                    confirm: Intent::SidebarRowActions,
                });
                Outcome::effects(vec![])
            }
        }
    }

    /// Open the prompt that names a folder.
    ///
    /// One prompt for both verbs, because the reader's answer is the same kind
    /// of thing either way — a name — and the two differ only in which folder it
    /// lands on and whether that folder already exists.
    fn begin_folder_prompt(
        &mut self,
        row: &vibex_desktop_model::AgentSidebarRow,
        rename: bool,
    ) -> Outcome {
        let folder_id = row.id.strip_prefix("folder:").map(str::to_string);
        if rename && folder_id.is_none() {
            return Outcome::quiet();
        }
        self.pending_folder_prompt = Some(crate::app::FolderPromptTarget {
            folder_id: if rename { folder_id.clone() } else { None },
            project_id: (!row.project_id.is_empty()).then(|| row.project_id.clone()),
            workspace_id: (!row.workspace_id.is_empty()).then(|| row.workspace_id.clone()),
            // A new folder made from a folder heading goes *inside* it; one made
            // from a project heading has no parent.
            parent_folder_id: if rename { None } else { folder_id },
        });
        self.overlay = Some(Overlay::Prompt {
            title: if rename {
                self.strings.sidebar_action_rename_folder()
            } else {
                self.strings.sidebar_action_new_folder()
            }
            .to_string(),
            field: if rename {
                PromptField::RenameFolderName
            } else {
                PromptField::NewFolderName
            },
            value: String::new(),
            target: None,
        });
        Outcome::effects(vec![])
    }

    /// `Enter`: everything the highlighted record can do.
    fn open_entry_actions(&mut self) -> Outcome {
        let Some(row) = self.selected_config_row() else {
            return Outcome::quiet();
        };
        if row.actions.is_empty() {
            return Outcome::quiet();
        }
        // A health row is a report, not a record: its one action is a probe,
        // which the menu would charge two presses for.
        if row.actions.len() == 1 {
            return self.run_config_action(&row, row.actions[0]);
        }
        self.overlay = Some(Overlay::EntryActions {
            entry: crate::config_center::EntryActions {
                title: row.title.clone(),
                row_key: row.key.clone(),
                actions: row.actions.clone(),
                selected: 0,
            },
        });
        Outcome::effects(vec![])
    }

    fn perform_entry_actions(
        &mut self,
        intent: Intent,
        entry: crate::config_center::EntryActions,
    ) -> Outcome {
        let last = entry.actions.len().saturating_sub(1);
        match intent {
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                self.overlay = Some(Overlay::EntryActions {
                    entry: crate::config_center::EntryActions {
                        selected: entry.selected.saturating_sub(1),
                        ..entry
                    },
                });
                Outcome::effects(vec![])
            }
            Intent::SelectNext | Intent::ScrollPageDown => {
                self.overlay = Some(Overlay::EntryActions {
                    entry: crate::config_center::EntryActions {
                        selected: (entry.selected + 1).min(last),
                        ..entry
                    },
                });
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => {
                let Some(action) = entry.actions.get(entry.selected).copied() else {
                    return Outcome::quiet();
                };
                self.overlay = None;
                self.run_config_action_by_key(&entry.row_key, action)
            }
            _ => Outcome::quiet(),
        }
    }

    /// Run one action against the record it was drawn for.
    ///
    /// The record is resolved by key rather than by cursor position: a read
    /// that lands while the menu is open must not make the choice write to the
    /// record that took the cursor's place.
    fn run_config_action(
        &mut self,
        row: &crate::config_center::ConfigRow,
        action: crate::config_center::ConfigAction,
    ) -> Outcome {
        let key = row.key.clone();
        self.run_config_action_by_key(&key, action)
    }

    pub fn run_config_action_by_key(
        &mut self,
        key: &str,
        action: crate::config_center::ConfigAction,
    ) -> Outcome {
        // The two verbs that cannot be undone ask before they run. The answer
        // comes back through `perform_confirm`, which calls the write below
        // directly: re-entering this function would ask again for ever.
        if action.is_destructive() {
            self.pending_config_action = Some((key.to_string(), action));
            self.confirm_config_delete(key, Intent::ConfigEntryDelete);
            return Outcome::effects(vec![]);
        }
        // `Edit` is not a write but an editor: it opens the form on the record
        // the menu was opened for. It resolves through the row for the same
        // reason every other verb does — a read that lands while the menu is
        // open must not put the form on whichever record took the cursor's
        // place.
        if action == crate::config_center::ConfigAction::Edit {
            let row = self.config_row(key);
            return self.begin_entry_form(row);
        }
        self.exec_config_action(key, action)
    }

    /// The write one row's verb performs, once it has been decided on.
    fn exec_config_action(
        &mut self,
        key: &str,
        action: crate::config_center::ConfigAction,
    ) -> Outcome {
        use crate::config_center::{ConfigAction, ConfigSection};
        let section = self.config_center.section;
        match (section, action) {
            (ConfigSection::Agents, ConfigAction::Toggle) => {
                let Some(agent) = self
                    .management_data
                    .agents
                    .iter()
                    .find(|agent| agent.id.as_str() == key)
                else {
                    return Outcome::quiet();
                };
                // The row answers the press before the authority does; the
                // write re-reads the area, so a refusal is drawn back.
                let agent_id = agent.id.clone();
                let enabled = !agent.enabled;
                if let Some(agent) = self
                    .management_data
                    .agents
                    .iter_mut()
                    .find(|candidate| candidate.id.as_str() == key)
                {
                    agent.enabled = enabled;
                }
                self.begin_config_mutation(crate::app::ConfigMutation::ToggleAgent {
                    agent_id,
                    enabled,
                })
            }
            // An upgrade is the same call as an install: what differs is the
            // verb the reader pressed and the version the authority moves to.
            (ConfigSection::Agents, ConfigAction::Install | ConfigAction::Upgrade) => {
                let Some(agent) = self
                    .management_data
                    .agents
                    .iter()
                    .find(|agent| agent.id.as_str() == key)
                else {
                    return Outcome::quiet();
                };
                let agent_id = agent.id.clone();
                let subject = agent.label.clone();
                // The grant is checked before the watch starts: a press this
                // client cannot make is not one it should be waiting on.
                if let Some(outcome) = self.unavailable_outcome(BackendOperation::ManagementAgents)
                {
                    return outcome;
                }
                // A managed install runs on the authority and answers nothing
                // until it is done, so the client watches the record it will
                // change rather than the request it sent.
                self.watch_install(agent_id.clone(), subject.clone());
                self.guard_record(
                    BackendOperation::ManagementAgents,
                    key,
                    subject,
                    crate::config_center::ConfigVerb::Install,
                    Effect::InstallAgent { agent_id },
                )
            }
            (ConfigSection::Agents, ConfigAction::Rollback) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                if let Some(outcome) = self.unavailable_outcome(BackendOperation::ManagementAgents)
                {
                    return outcome;
                }
                self.watch_install(agent_id.clone(), self.config_row_subject(key));
                self.begin_config_mutation(crate::app::ConfigMutation::RollbackAgent { agent_id })
            }
            (ConfigSection::Agents, ConfigAction::CheckUpdate) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                self.begin_config_mutation(crate::app::ConfigMutation::CheckAgentUpdate {
                    agent_id,
                })
            }
            (ConfigSection::Agents, ConfigAction::RefreshSnapshot) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                self.begin_config_mutation(crate::app::ConfigMutation::RefreshAgentSnapshot {
                    agent_id,
                })
            }
            (ConfigSection::Agents, ConfigAction::Uninstall) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                let subject = self.config_row_subject(key);
                if let Some(outcome) = self.unavailable_outcome(BackendOperation::ManagementAgents)
                {
                    return outcome;
                }
                self.watch_install(agent_id.clone(), subject.clone());
                self.guard_record(
                    BackendOperation::ManagementAgents,
                    key,
                    subject,
                    crate::config_center::ConfigVerb::Uninstall,
                    Effect::UninstallAgent { agent_id },
                )
            }
            (ConfigSection::Agents, ConfigAction::AddToRoster) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                self.begin_config_mutation(crate::app::ConfigMutation::SetAgentAdded {
                    agent_id,
                    added: true,
                })
            }
            (ConfigSection::Agents, ConfigAction::RemoveFromRoster) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                self.begin_config_mutation(crate::app::ConfigMutation::SetAgentAdded {
                    agent_id,
                    added: false,
                })
            }
            (ConfigSection::Agents, ConfigAction::BrowserTools) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                let enabled = self
                    .management_data
                    .agent_tools
                    .enabled(key, vibex_core::AgentBuiltinTool::Browser);
                self.management_data.agent_tools.set_enabled(
                    key,
                    vibex_core::AgentBuiltinTool::Browser,
                    !enabled,
                );
                self.begin_config_mutation(crate::app::ConfigMutation::SetAgentTool {
                    agent_id,
                    tool: vibex_core::AgentBuiltinTool::Browser,
                    enabled: !enabled,
                })
            }
            (ConfigSection::Agents, ConfigAction::ComputerTools) => {
                let Some(agent_id) = self.agent_id(key) else {
                    return Outcome::quiet();
                };
                let enabled = self
                    .management_data
                    .agent_tools
                    .enabled(key, vibex_core::AgentBuiltinTool::Computer);
                self.management_data.agent_tools.set_enabled(
                    key,
                    vibex_core::AgentBuiltinTool::Computer,
                    !enabled,
                );
                self.begin_config_mutation(crate::app::ConfigMutation::SetAgentTool {
                    agent_id,
                    tool: vibex_core::AgentBuiltinTool::Computer,
                    enabled: !enabled,
                })
            }
            (ConfigSection::Agents, ConfigAction::MoveUp) => self.move_agent(key, -1),
            (ConfigSection::Agents, ConfigAction::MoveDown) => self.move_agent(key, 1),
            (ConfigSection::Agents, ConfigAction::Auth) => self.open_agent_auth(key),
            // Signing out is the account panel's own verb, not a second call
            // shape behind a different key: the panel is where the impact of
            // ending an account is read out and confirmed, and an Agent row that
            // signed out on one press would be the same destructive write with
            // none of that. The row opens the panel and lands the cursor on the
            // line it was pressed for.
            (ConfigSection::Agents, ConfigAction::Logout) => {
                self.open_agent_auth_at(key, crate::config_center::AgentAuthLine::SignOut)
            }
            // The gateway's own row is the only switch in this table. A direct
            // Provider is chosen rather than switched, so this arm is the row
            // it names and nothing else: the verbs a Provider row advertises do
            // not include Toggle.
            (ConfigSection::Providers, ConfigAction::Toggle) if key.starts_with("gateway-key:") => {
                let Some((profile, credential, enabled, _)) = self.gateway_key_target(key) else {
                    return Outcome::quiet();
                };
                self.begin_toggle(
                    key,
                    !enabled,
                    Effect::SetGatewayKey {
                        profile,
                        credential,
                        enabled: Some(!enabled),
                        preferred: None,
                    },
                )
            }
            (ConfigSection::Providers, ConfigAction::Prefer) if key.starts_with("gateway-key:") => {
                let Some((profile, credential, _, preferred)) = self.gateway_key_target(key) else {
                    return Outcome::quiet();
                };
                let subject = self.config_row_subject(key);
                self.begin_record_work(
                    key.to_string(),
                    subject,
                    crate::config_center::ConfigVerb::Activate,
                    vec![Effect::SetGatewayKey {
                        profile,
                        credential,
                        enabled: None,
                        preferred: Some(!preferred),
                    }],
                )
            }
            (ConfigSection::Providers, ConfigAction::Requests) if key == "gateway" => self
                .begin_record_work(
                    "gateway".to_string(),
                    self.config_row_subject("gateway"),
                    crate::config_center::ConfigVerb::RefreshSnapshot,
                    vec![Effect::LoadGatewayRequests],
                ),
            (ConfigSection::Providers, ConfigAction::TokenLimit) if key == "gateway" => {
                let value = self
                    .management_data
                    .gateway
                    .as_ref()
                    .and_then(|snapshot| snapshot.settings.policy.token_limit)
                    .map(|limit| limit.tokens.to_string())
                    .unwrap_or_default();
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.config_action_token_limit().to_string(),
                    field: PromptField::GatewayTokenLimit,
                    value,
                    target: Some("gateway".to_string()),
                });
                Outcome::effects(vec![])
            }
            (ConfigSection::Providers, ConfigAction::Toggle) if key == "gateway" => {
                let Some(snapshot) = self.management_data.gateway.as_ref() else {
                    return Outcome::quiet();
                };
                // "On" means it is listening; a degraded gateway is still
                // listening, so the press turns it off. Only a state that is
                // not serving at all is asked to be turned on.
                let enabled = !snapshot.runtime.status.is_listening();
                self.begin_toggle(key, enabled, Effect::SetGatewayEnabled { enabled })
            }
            (ConfigSection::Providers, ConfigAction::Activate) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                let subject = self.config_row_subject(key);
                // A default is either for the checkout this client is working in
                // or for everything; the reader is told which, and a client that
                // knows its workspace asks for that one.
                let scope = self.composer_workspace_id().map(|workspace_id| {
                    vibex_core::ProviderProfileDefaultScope {
                        kind: vibex_core::ProviderDefaultScopeKind::Workspace,
                        project_id: None,
                        workspace_id: Some(workspace_id),
                    }
                });
                self.guard_record(
                    BackendOperation::ManagementProfileSelect,
                    key,
                    subject,
                    crate::config_center::ConfigVerb::Activate,
                    Effect::SelectProfile { profile_id, scope },
                )
            }
            // A copy starts in the form rather than being written straight out:
            // this client reads profiles as summaries, and a summary has no
            // endpoint to copy. The reader is shown what the copy carries and
            // asked for the rest.
            (ConfigSection::Providers, ConfigAction::MoveUp) => self.move_provider(key, -1),
            (ConfigSection::Providers, ConfigAction::MoveDown) => self.move_provider(key, 1),
            (ConfigSection::Providers, ConfigAction::Duplicate) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                let Some(agent_id) = self.profile_agent_id(key) else {
                    return Outcome::quiet();
                };
                self.open_provider(agent_id, profile_id, crate::app::ProviderOpen::Duplicate)
            }
            (ConfigSection::Providers, ConfigAction::ClearSecret) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                // The same path the write takes: neither is gated on the
                // client's capability snapshot, and a runtime that refuses the
                // write answers for itself.
                let subject = self.config_row_subject(key);
                self.begin_record_work(
                    key.to_string(),
                    subject,
                    crate::config_center::ConfigVerb::Delete,
                    vec![Effect::ClearProviderSecret { profile_id }],
                )
            }
            (ConfigSection::Providers, ConfigAction::Secret) => {
                // The profile the key is for travels with the prompt: the list
                // can be re-read while the reader is typing, and the row the
                // cursor then points at is not the row they pressed.
                self.reveal_provider_secret = false;
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.config_action_secret().to_string(),
                    field: PromptField::ProviderSecret,
                    value: String::new(),
                    target: Some(key.to_string()),
                });
                Outcome::effects(vec![])
            }
            (ConfigSection::Providers, ConfigAction::Test) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                let subject = self.config_row_subject(key);
                self.guard_record(
                    BackendOperation::ManagementProviderProfileTest,
                    key,
                    subject,
                    crate::config_center::ConfigVerb::Test,
                    Effect::TestProviderProfile { profile_id },
                )
            }
            (ConfigSection::Providers, ConfigAction::FetchModels) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                let subject = self.config_row_subject(key);
                self.guard_record(
                    BackendOperation::ManagementProviderModelFetch,
                    key,
                    subject,
                    crate::config_center::ConfigVerb::FetchModels,
                    Effect::FetchProviderModels { profile_id },
                )
            }
            (ConfigSection::Providers, ConfigAction::Delete) => {
                let Some(profile_id) = self.provider_id(key) else {
                    return Outcome::quiet();
                };
                let Some(agent_id) = self.profile_agent_id(key) else {
                    return Outcome::quiet();
                };
                self.begin_config_mutation(crate::app::ConfigMutation::DeleteProviderProfile {
                    agent_id,
                    profile_id,
                })
            }
            (ConfigSection::Servers, ConfigAction::Toggle) => {
                let Some(server) = self.mcp_server(key) else {
                    return Outcome::quiet();
                };
                let enabled = server.status != vibex_core::McpServerStatus::Enabled;
                let effect = Effect::ToggleMcp {
                    server_id: server.id.clone(),
                    enabled,
                };
                self.begin_toggle(key, enabled, effect)
            }
            (ConfigSection::Servers, ConfigAction::Validate) => {
                let Some(server) = self.mcp_server(key) else {
                    return Outcome::quiet();
                };
                let server_id = server.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::ValidateMcpServer {
                    server_id,
                })
            }
            (ConfigSection::Servers, ConfigAction::Matrix)
            | (ConfigSection::Installed, ConfigAction::Matrix) => self.begin_matrix_form(key),
            (ConfigSection::Servers, ConfigAction::Delete) => {
                let Some(server) = self.mcp_server(key) else {
                    return Outcome::quiet();
                };
                let server_id = server.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::DeleteMcpServer {
                    server_id,
                })
            }
            (ConfigSection::Installed, ConfigAction::Toggle) => {
                let Some(skill) = self.skill(key) else {
                    return Outcome::quiet();
                };
                let enabled = skill.status != vibex_core::SkillStatus::Enabled;
                let effect = Effect::ToggleSkill {
                    skill_id: skill.id.clone(),
                    enabled,
                };
                self.begin_toggle(key, enabled, effect)
            }
            (ConfigSection::Installed, ConfigAction::Validate) => {
                let Some(skill) = self.skill(key) else {
                    return Outcome::quiet();
                };
                let skill_id = skill.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::ValidateSkill { skill_id })
            }
            (ConfigSection::Installed, ConfigAction::Delete) => {
                let Some(skill) = self.skill(key) else {
                    return Outcome::quiet();
                };
                let skill_id = skill.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::DeleteSkill { skill_id })
            }
            (ConfigSection::Prompts, ConfigAction::Toggle) => {
                let Some(prompt) = self.prompt(key) else {
                    return Outcome::quiet();
                };
                let enabled = prompt.status != vibex_core::PromptStatus::Enabled;
                let effect = Effect::TogglePrompt {
                    prompt_id: prompt.id.clone(),
                    enabled,
                };
                self.begin_toggle(key, enabled, effect)
            }
            (ConfigSection::Prompts, ConfigAction::Delete) => {
                let Some(prompt) = self.prompt(key) else {
                    return Outcome::quiet();
                };
                let prompt_id = prompt.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::DeletePrompt { prompt_id })
            }
            (ConfigSection::Prompts, ConfigAction::Validate) => {
                let Some(prompt) = self.prompt(key) else {
                    return Outcome::quiet();
                };
                let prompt_id = prompt.id.clone();
                self.begin_config_mutation(crate::app::ConfigMutation::ValidatePrompt { prompt_id })
            }
            (ConfigSection::Agents, ConfigAction::InstallDocs) => {
                let Some(agent) = self
                    .management_data
                    .agents
                    .iter()
                    .find(|agent| agent.id.as_str() == key)
                else {
                    return Outcome::quiet();
                };
                match vibex_core::agent_install_url(agent) {
                    Some(url) => Outcome::effects(vec![Effect::OpenUrl {
                        url: url.to_string(),
                    }]),
                    None => Outcome::quiet(),
                }
            }
            (ConfigSection::Catalog, ConfigAction::Install | ConfigAction::Reinstall) => {
                self.install_registry_entry(key)
            }
            (ConfigSection::Catalog, ConfigAction::OpenPage) => {
                let Some(entry) = self.mcp_market_entry(key) else {
                    return Outcome::quiet();
                };
                match crate::config_center::market_entry_url(&entry) {
                    Some(url) => Outcome::effects(vec![Effect::OpenUrl { url }]),
                    None => Outcome::quiet(),
                }
            }
            (ConfigSection::Catalog, ConfigAction::Validate) => self.validate_registry_entry(key),
            (ConfigSection::Sources, ConfigAction::Import) => self.import_discovery(key),
            _ => Outcome::quiet(),
        }
    }

    fn agent_id(&self, key: &str) -> Option<vibex_core::AgentId> {
        self.management_data
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == key)
            .map(|agent| agent.id.clone())
    }

    /// The (profile, credential, enabled, preferred) a gateway Key row names.
    fn gateway_key_target(
        &self,
        key: &str,
    ) -> Option<(
        vibex_core::ModelProviderProfileId,
        vibex_core::RequestId,
        bool,
        bool,
    )> {
        let rest = key.strip_prefix("gateway-key:")?;
        let (profile, credential) = rest.split_once(':')?;
        let profile = vibex_core::ModelProviderProfileId::parse(profile).ok()?;
        let credential = vibex_core::RequestId::parse(credential).ok()?;
        let snapshot = self.management_data.gateway.as_ref()?;
        let preferred = snapshot
            .settings
            .policy
            .preferred_keys
            .get(&profile)
            .is_some_and(|preferred| preferred == &credential);
        let key = snapshot
            .upstreams
            .iter()
            .find(|upstream| upstream.model_provider_profile_id == profile)?
            .keys
            .iter()
            .find(|key| key.credential_id == credential)?;
        Some((
            profile,
            credential,
            key.state != vibex_core::GatewayKeyState::Disabled,
            preferred,
        ))
    }

    fn provider_id(&self, key: &str) -> Option<vibex_core::ProviderProfileId> {
        self.management_data
            .providers
            .iter()
            .find(|profile| profile.id.as_str() == key)
            .map(|profile| profile.id.clone())
    }

    fn mcp_server(&self, key: &str) -> Option<&vibex_core::McpServer> {
        self.management_data
            .mcp
            .iter()
            .find(|server| server.id.as_str() == key)
    }

    fn skill(&self, key: &str) -> Option<&vibex_core::Skill> {
        self.management_data
            .skills
            .iter()
            .find(|skill| skill.id.as_str() == key)
    }

    fn prompt(&self, key: &str) -> Option<&vibex_core::Prompt> {
        self.management_data
            .prompts
            .iter()
            .find(|prompt| prompt.id.as_str() == key)
    }

    fn profile_agent_id(&self, key: &str) -> Option<vibex_core::AgentId> {
        self.management_data
            .providers
            .iter()
            .find(|profile| profile.id.as_str() == key)
            .map(|profile| profile.agent_id.clone())
    }

    /// `d`, and the delete row of the action menu.
    fn confirm_entry_delete(&mut self) -> Outcome {
        let Some(row) = self.selected_config_row() else {
            return Outcome::quiet();
        };
        let action = if row
            .actions
            .contains(&crate::config_center::ConfigAction::Delete)
        {
            crate::config_center::ConfigAction::Delete
        } else if row
            .actions
            .contains(&crate::config_center::ConfigAction::Uninstall)
        {
            crate::config_center::ConfigAction::Uninstall
        } else {
            return Outcome::quiet();
        };
        self.run_config_action(&row, action)
    }

    fn confirm_config_delete(&mut self, label: &str, confirm: Intent) {
        let uninstall = self
            .pending_config_action
            .as_ref()
            .is_some_and(|(_, action)| *action == crate::config_center::ConfigAction::Uninstall);
        let (title, explanation) = if uninstall {
            (
                self.strings.config_confirm_uninstall_title(),
                self.strings.config_confirm_uninstall_body(),
            )
        } else {
            (
                self.strings.config_confirm_delete_title(),
                self.strings.config_confirm_delete_body(),
            )
        };
        let body = if label.is_empty() {
            explanation.to_string()
        } else {
            format!("{label}\n\n{explanation}")
        };
        self.overlay = Some(Overlay::Confirm {
            title: title.to_string(),
            body,
            confirm,
        });
    }

    /// `e` / `n`: open the form for a record, or the blank one for a new one.
    fn begin_entry_form(&mut self, row: Option<crate::config_center::ConfigRow>) -> Outcome {
        use crate::config_center::{ConfigModule, ConfigSection};
        let strings = self.strings;
        let key = row.as_ref().map(|row| row.key.as_str());
        let form = match (self.config_center.module, self.config_center.section) {
            (ConfigModule::Mcp, ConfigSection::Servers) => {
                let server = key.and_then(|key| self.mcp_server(key)).cloned();
                Some(crate::config_center::mcp_form(server.as_ref(), strings))
            }
            (ConfigModule::Skills, ConfigSection::Installed) => {
                let skill = key.and_then(|key| self.skill(key)).cloned();
                Some(crate::config_center::skill_form(skill.as_ref(), strings))
            }
            (ConfigModule::Prompts, ConfigSection::Prompts) => {
                let prompt = key.and_then(|key| self.prompt(key)).cloned();
                Some(crate::config_center::prompt_form(prompt.as_ref(), strings))
            }
            // `n` on the Agents area registers an Agent rather than editing
            // one: the roster's own records are the machine's, and the only
            // Agent a reader can make is an ACP one they name themselves. A
            // selected row does not change that, so this branch ignores it.
            (ConfigModule::Agent, ConfigSection::Agents) => {
                Some(crate::config_center::custom_agent_form(strings))
            }
            (ConfigModule::Agent, ConfigSection::Providers) => {
                let profile = key
                    .and_then(|key| {
                        self.management_data
                            .providers
                            .iter()
                            .find(|profile| profile.id.as_str() == key)
                    })
                    .cloned();
                match profile {
                    Some(profile) => Some(crate::config_center::provider_form(
                        &profile,
                        self.provider_detail(profile.id.as_str()),
                        strings,
                    )),
                    // `n` on the Providers area has no record to open, so it
                    // opens the preset list first: a preset is the shortcut
                    // past having to know an address, a protocol and a model id,
                    // and the form after it is the same form either way.
                    None if key.is_none() => {
                        let agent_id = self
                            .management_data
                            .agents
                            .first()
                            .map(|agent| agent.id.as_str().to_string())
                            .unwrap_or_default();
                        if agent_id.is_empty() {
                            Some(self.provider_create_form(None, None, strings))
                        } else {
                            self.overlay = Some(Overlay::ProviderPresetPicker {
                                selected: 0,
                                agent_id,
                            });
                            None
                        }
                    }
                    None => None,
                }
            }
            _ => None,
        };
        match form {
            Some(form) => {
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
            None => Outcome::quiet(),
        }
    }

    /// The `Auth` verb on an Agent: open the account panel and read it.
    ///
    /// The panel is a list of lines rather than a set of keys: the ways in, and
    /// the things that can be done with the account the Agent holds, are one
    /// decision, and `Enter` on a line is the whole vocabulary.
    fn open_agent_auth(&mut self, key: &str) -> Outcome {
        self.open_agent_auth_at(key, crate::config_center::AgentAuthLine::First)
    }

    /// The same panel, with the cursor put on the line the verb was for.
    ///
    /// The line is chosen once the catalogue lands, because the lines are the
    /// catalogue's; until then the panel says it is reading. A verb pressed on
    /// the row and a verb pressed inside the panel must lead to the same place,
    /// or the row's key is a second, quieter way to do the same destructive
    /// thing — which is exactly what "sign out" without the impact readout was.
    fn open_agent_auth_at(
        &mut self,
        key: &str,
        line: crate::config_center::AgentAuthLine,
    ) -> Outcome {
        let Some(agent_id) = self.agent_id(key) else {
            return Outcome::quiet();
        };
        // The grant is checked before the panel opens: a surface that opened
        // and then refused would be a surface the reader cannot use.
        if let Some(outcome) = self.unavailable_outcome(BackendOperation::AgentAuthRead) {
            return outcome;
        }
        let subject = self.config_row_subject(key);
        self.overlay = Some(Overlay::AgentAuth {
            view: crate::config_center::AgentAuthView {
                agent_id: agent_id.as_str().to_string(),
                title: subject.clone(),
                selected: 0,
                loading: true,
                confirming_sign_out: false,
                notice: None,
                want_line: line,
                pending_sign_in: None,
            },
        });
        let verb = match line {
            crate::config_center::AgentAuthLine::SignOut => {
                crate::config_center::ConfigVerb::SignOut
            }
            crate::config_center::AgentAuthLine::First => crate::config_center::ConfigVerb::SignIn,
        };
        self.begin_record_work(
            key.to_string(),
            subject,
            verb,
            vec![Effect::LoadAgentAuthCatalog {
                agent_id,
                refresh: false,
            }],
        )
    }

    /// The keys of the account panel.
    fn perform_agent_auth(
        &mut self,
        intent: Intent,
        mut view: crate::config_center::AgentAuthView,
    ) -> Outcome {
        use crate::config_center::AgentAuthAction;
        let rows = view.rows(
            self.management_data.agent_auth_catalog.as_ref(),
            self.strings,
        );
        let last = rows.len().saturating_sub(1);
        let selected = view.selected.min(last);
        let Ok(agent_id) = parse_config_id::<vibex_core::AgentId>(&view.agent_id) else {
            return Outcome::quiet();
        };
        match intent {
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                view.selected = selected.saturating_sub(1);
                self.overlay = Some(Overlay::AgentAuth { view });
                Outcome::effects(vec![])
            }
            Intent::SelectNext | Intent::ScrollPageDown => {
                view.selected = (selected + 1).min(last);
                self.overlay = Some(Overlay::AgentAuth { view });
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => {
                let Some(row) = rows.get(selected) else {
                    return Outcome::quiet();
                };
                let Some(action) = row.action.clone() else {
                    // The line says why it cannot be used; pressing it repeats
                    // that rather than sending a request that would fail.
                    if let Some(hint) = row.detail.first() {
                        self.toast(Toast::info(hint.1.clone()));
                    }
                    return Outcome::quiet();
                };
                let agent_id_str = view.agent_id.clone();
                let confirming_sign_out = view.confirming_sign_out;
                let pending_sign_in = view.pending_sign_in.clone();
                self.overlay = Some(Overlay::AgentAuth { view });
                match action {
                    // A refresh is a write: it launches the Agent's own runtime
                    // to re-probe what it advertises. It is gated on the manage
                    // grant for that reason, and it is the *only* line that
                    // re-probes — opening the panel answers from the snapshot.
                    AgentAuthAction::RefreshMethods => {
                        if let Some(outcome) =
                            self.unavailable_outcome(BackendOperation::AgentAuthManage)
                        {
                            return outcome;
                        }
                        Outcome::effects(vec![Effect::LoadAgentAuthCatalog {
                            agent_id,
                            refresh: true,
                        }])
                    }
                    AgentAuthAction::Verify => {
                        Outcome::effects(vec![Effect::VerifyAgentAuth { agent_id }])
                    }
                    AgentAuthAction::RefreshModels => {
                        Outcome::effects(vec![Effect::RefreshAgentAuthModels { agent_id }])
                    }
                    // A method opens the form that collects what it needs; the
                    // environment variables are the only thing between the
                    // reader and the Agent's own runtime.
                    AgentAuthAction::Method {
                        method_id,
                        environment,
                        ..
                    } => {
                        let form = crate::config_center::agent_auth_method_form(
                            &agent_id_str,
                            &method_id,
                            &row.label,
                            &environment,
                            self.strings,
                        );
                        self.overlay = Some(Overlay::Form { form });
                        Outcome::effects(vec![])
                    }
                    AgentAuthAction::Credentials { url } => {
                        Outcome::effects(vec![Effect::OpenUrl { url }])
                    }
                    // Calling off a sign-in ends the operation the authority
                    // is holding and the terminal or browser it opened. The
                    // panel drops its memory of it either way: a cancel that
                    // arrived after the sign-in finished is not an error, and
                    // a panel still offering it afterwards would be offering
                    // to cancel something that is over.
                    AgentAuthAction::Cancel => {
                        let Some(operation) = pending_sign_in.clone() else {
                            return Outcome::quiet();
                        };
                        if let Some(Overlay::AgentAuth { view }) = self.overlay.as_mut() {
                            view.pending_sign_in = None;
                            view.confirming_sign_out = false;
                        }
                        Outcome::effects(vec![Effect::CancelAgentAuth {
                            agent_id,
                            operation_id: operation.operation_id,
                        }])
                    }
                    // Signing out is asked about first: a sign-out that also
                    // ends the reader's sessions is not a decision to make
                    // blind, and the authority refuses a count that no longer
                    // matches, which is what makes the preview a decision
                    // rather than a formality.
                    AgentAuthAction::SignOut => {
                        if !confirming_sign_out {
                            // The panel is already the overlay on screen, so the
                            // question is marked on it rather than on a copy.
                            if let Some(Overlay::AgentAuth { view }) = self.overlay.as_mut() {
                                view.confirming_sign_out = true;
                            }
                            return Outcome::effects(vec![Effect::PreviewAgentAuthLogout {
                                agent_id,
                            }]);
                        }
                        let Some(preview) = self.management_data.agent_auth_logout.clone() else {
                            return Outcome::quiet();
                        };
                        Outcome::effects(vec![Effect::ConfigMutation {
                            mutation: crate::app::ConfigMutation::LogoutAgentAuthContext {
                                auth_context_id: preview.context.id.as_str().to_string(),
                                expected_context_revision: preview.context.revision,
                                confirmed_affected_session_count: preview
                                    .affected_session_ids
                                    .len(),
                            },
                            record: preview.context.agent_id.as_str().to_string(),
                        }])
                    }
                }
            }
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                // Closing the panel does not cancel the sign-in: the operation
                // belongs to the authority, and a browser the reader is still
                // finishing in is not abandoned because they looked away. What
                // is given up is the client's view of it, which is also what
                // stops the keys going anywhere.
                let mut outcome = Outcome::effects(vec![]);
                outcome.dirty = true;
                outcome
            }
            _ => {
                self.overlay = Some(Overlay::AgentAuth { view });
                Outcome::quiet()
            }
        }
    }

    /// Move one Agent a place earlier or later among the ones it shares a
    /// group with.
    ///
    /// The order is the authority's — every client reads it — so the move is a
    /// swap of two records' places rather than a list this client keeps to
    /// itself. An Agent at either end of its own group has nowhere to go, and
    /// one at the edge of its group does not cross into the next: the groups
    /// are the roster's, not the reader's.
    fn move_agent(&mut self, key: &str, delta: isize) -> Outcome {
        let Some(agent) = self
            .management_data
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == key)
            .cloned()
        else {
            return Outcome::quiet();
        };
        let group = crate::config_center::agent_group(&agent);
        let mut siblings = self
            .management_data
            .agents
            .iter()
            .filter(|candidate| crate::config_center::agent_group(candidate) == group)
            .collect::<Vec<_>>();
        siblings.sort_by(|left, right| {
            left.order_index
                .cmp(&right.order_index)
                .then(left.id.as_str().cmp(right.id.as_str()))
        });
        let Some(index) = siblings
            .iter()
            .position(|candidate| candidate.id == agent.id)
        else {
            return Outcome::quiet();
        };
        let next = index.saturating_add_signed(delta);
        if next == index || next >= siblings.len() {
            return Outcome::quiet();
        }
        let other = siblings[next];
        self.begin_config_mutation(crate::app::ConfigMutation::SwapAgentOrder {
            first: agent.id.clone(),
            first_index: agent.order_index,
            second: other.id.clone(),
            second_index: other.order_index,
        })
    }

    /// Start watching what the authority does with a managed install.
    fn watch_install(&mut self, agent_id: vibex_core::AgentId, label: String) {
        self.install_watch = Some(crate::config_center::InstallWatch::new(agent_id, label));
    }

    /// `Ctrl+F`: probe the machine for Agent CLIs that have not been added.
    ///
    /// The pass is about the Agents area as a whole rather than about a record,
    /// so it is refused where it would do nothing — nothing left to probe — and
    /// otherwise runs as one effect.
    fn discover_local_agents(&mut self) -> Outcome {
        use crate::config_center::ConfigSection;
        if self.config_center.section != ConfigSection::Agents {
            return Outcome::quiet();
        }
        if !self.management_data.agents.iter().any(|agent| !agent.added) {
            self.toast(Toast::info(
                self.strings.config_discover_no_candidates().to_string(),
            ));
            return Outcome::quiet();
        }
        self.begin_record_work(
            "agents".to_string(),
            self.strings.config_action_discover().to_string(),
            crate::config_center::ConfigVerb::RefreshSnapshot,
            vec![Effect::DiscoverLocalAgents],
        )
    }

    /// `x`: write this profile into another tool's own settings.
    ///
    /// The export belongs to a record — it is one profile that gets written —
    /// so unlike the import it is offered on the profile the cursor is on.
    fn open_native_export(&mut self) -> Outcome {
        use crate::config_center::ConfigSection;
        if self.config_center.section != ConfigSection::Providers {
            return Outcome::quiet();
        }
        let Some(row) = self.selected_config_row() else {
            return Outcome::quiet();
        };
        let Some(profile) = self.provider(&row.key).cloned() else {
            return Outcome::quiet();
        };
        let profile_id = profile.id.clone();
        self.overlay = Some(Overlay::NativeExport {
            view: Box::new(crate::config_center::NativeExportView {
                agent_id: profile.agent_id.as_str().to_string(),
                profile_id: profile.id.as_str().to_string(),
                title: profile.display_name.clone(),
                source: vibex_core::ProviderNativeExportSource::AgentDefault,
                mode: vibex_core::ProviderNativeExportMode::ProviderProfile,
                preview: None,
                history: Vec::new(),
                selected: 0,
                notice: None,
            }),
        });
        Outcome::effects(vec![Effect::NativeExportHistory { profile_id }])
    }

    /// The keys of the export picker.
    fn perform_native_export(
        &mut self,
        intent: Intent,
        mut view: crate::config_center::NativeExportView,
    ) -> Outcome {
        use crate::config_center as cc;
        let Ok(profile_id) = parse_config_id::<vibex_core::ProviderProfileId>(&view.profile_id)
        else {
            return Outcome::quiet();
        };
        match intent {
            Intent::ConfigExportSource => {
                view.source = cc::cycle(&cc::native_export_sources(), view.source);
                // The plan was made for the old question, so it is dropped
                // rather than applied to a target it does not describe.
                view.preview = None;
                self.overlay = Some(Overlay::NativeExport {
                    view: Box::new(view),
                });
                Outcome::effects(vec![])
            }
            Intent::ConfigExportMode => {
                view.mode = cc::cycle(&cc::native_export_modes(), view.mode);
                view.preview = None;
                self.overlay = Some(Overlay::NativeExport {
                    view: Box::new(view),
                });
                Outcome::effects(vec![])
            }
            Intent::ConfigExportPreview => {
                let source = view.source;
                let mode = view.mode;
                self.overlay = Some(Overlay::NativeExport {
                    view: Box::new(view),
                });
                Outcome::effects(vec![Effect::PreviewNativeExport {
                    profile_id,
                    source,
                    mode,
                }])
            }
            // Applying names the plan the reader is looking at, so a press with
            // nothing previewed asks for the preview rather than writing a plan
            // the reader never saw.
            Intent::ConfigExportApply => match view
                .preview
                .as_ref()
                .map(|preview| preview.export_id.clone())
            {
                Some(export_id) => self.begin_record_work(
                    view.profile_id.clone(),
                    view.title.clone(),
                    cc::ConfigVerb::Save,
                    vec![Effect::ConfigMutation {
                        mutation: crate::app::ConfigMutation::ApplyNativeExport {
                            export_id: export_id.as_str().to_string(),
                        },
                        record: view.profile_id.clone(),
                    }],
                ),
                None => {
                    self.overlay = Some(Overlay::NativeExport {
                        view: Box::new(view),
                    });
                    Outcome::quiet()
                }
            },
            Intent::ConfigExportRollback => {
                let record = view.history.iter().find(|record| {
                    record.applied_at_ms.is_some() && record.rolled_back_at_ms.is_none()
                });
                let Some(record) = record else {
                    return Outcome::quiet();
                };
                let export_id = record.export_id.as_str().to_string();
                self.overlay = Some(Overlay::NativeExport {
                    view: Box::new(view),
                });
                Outcome::effects(vec![Effect::ConfigMutation {
                    mutation: crate::app::ConfigMutation::RollbackNativeExport { export_id },
                    record: self
                        .selected_config_row()
                        .map(|row| row.key)
                        .unwrap_or_default(),
                }])
            }
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                Outcome::effects(vec![])
            }
            _ => {
                self.overlay = Some(Overlay::NativeExport {
                    view: Box::new(view),
                });
                Outcome::quiet()
            }
        }
    }

    /// `i`: what other CLI tools on this machine already hold.
    ///
    /// The Providers area is where a profile comes from, so the verb lives
    /// there and the picker it opens is the area's, not a record's.
    fn open_native_import(&mut self) -> Outcome {
        use crate::config_center::ConfigSection;
        if self.config_center.section != ConfigSection::Providers {
            return Outcome::quiet();
        }
        self.overlay = Some(Overlay::NativeImport {
            view: crate::config_center::NativeImportView::default(),
        });
        Outcome::effects(vec![Effect::PreviewNativeImport {
            sources: crate::config_center::native_import_sources(),
        }])
    }

    /// The keys of the import picker.
    fn perform_native_import(
        &mut self,
        intent: Intent,
        mut view: crate::config_center::NativeImportView,
    ) -> Outcome {
        let rows = crate::config_center::native_import_rows(
            self.management_data.native_import.as_ref(),
            self.strings,
        );
        let last = rows.len().saturating_sub(1);
        let selected = view.selected.min(last);
        match intent {
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                view.selected = selected.saturating_sub(1);
                self.overlay = Some(Overlay::NativeImport { view });
                Outcome::effects(vec![])
            }
            Intent::SelectNext | Intent::ScrollPageDown => {
                view.selected = (selected + 1).min(last);
                self.overlay = Some(Overlay::NativeImport { view });
                Outcome::effects(vec![])
            }
            // `Enter` is the overlay's confirm, which is the same key the
            // config centre's other menus use to run what is highlighted.
            Intent::ConfirmOverlay => {
                let Some(row) = rows.get(selected) else {
                    return Outcome::quiet();
                };
                if !row.importable {
                    // The authority named why this one cannot be read; sending
                    // it anyway would turn a diagnostic into a failure.
                    if let Some((_, message)) = row.detail.last() {
                        self.toast(Toast::warning(message.clone()));
                    }
                    return Outcome::quiet();
                }
                self.overlay = Some(Overlay::NativeImport { view });
                Outcome::effects(vec![Effect::ConfigMutation {
                    mutation: crate::app::ConfigMutation::ImportNativeProfile {
                        item_id: row.item_id.clone(),
                        sources: crate::config_center::native_import_sources(),
                    },
                    record: row.item_id.clone(),
                }])
            }
            Intent::ConfigImportRefresh => {
                view.loading = true;
                view.notice = None;
                self.overlay = Some(Overlay::NativeImport { view });
                Outcome::effects(vec![Effect::PreviewNativeImport {
                    sources: crate::config_center::native_import_sources(),
                }])
            }
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                Outcome::effects(vec![])
            }
            _ => {
                self.overlay = Some(Overlay::NativeImport { view });
                Outcome::quiet()
            }
        }
    }

    /// `m`: the Models of the profile the cursor is on.
    ///
    /// A Model is not a record of its own — it belongs to a profile and has no
    /// life without one — so the list opens over the record rather than beside
    /// it, and every press writes through the profile it belongs to.
    fn open_model_editor(&mut self) -> Outcome {
        use crate::config_center::ConfigSection;
        if self.config_center.section != ConfigSection::Providers {
            return Outcome::quiet();
        }
        let Some(row) = self.selected_config_row() else {
            return Outcome::quiet();
        };
        let Some(profile) = self.provider(&row.key).cloned() else {
            return Outcome::quiet();
        };
        self.overlay = Some(Overlay::Models {
            view: crate::config_center::ModelListView {
                agent_id: profile.agent_id.as_str().to_string(),
                profile_id: profile.id.as_str().to_string(),
                title: profile.display_name.clone(),
                selected: 0,
                notice: None,
            },
        });
        Outcome::effects(vec![])
    }

    fn provider(&self, key: &str) -> Option<&vibex_core::ProviderProfileSummary> {
        self.management_data
            .providers
            .iter()
            .find(|profile| profile.id.as_str() == key)
    }

    /// The rows a Model editor draws, projected from the records it edits.
    fn model_rows(
        &self,
        view: &crate::config_center::ModelListView,
    ) -> Vec<crate::config_center::ModelRow> {
        match self.provider(&view.profile_id) {
            Some(profile) => crate::config_center::model_rows(
                profile,
                &self.management_data.provider_models,
                self.strings,
            ),
            None => Vec::new(),
        }
    }

    /// The keys of the Model editor.
    fn perform_models(
        &mut self,
        intent: Intent,
        mut view: crate::config_center::ModelListView,
    ) -> Outcome {
        let rows = self.model_rows(&view);
        let last = rows.len().saturating_sub(1);
        let selected = view.selected.min(last);
        match intent {
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                view.selected = selected.saturating_sub(1);
                view.notice = None;
                self.overlay = Some(Overlay::Models { view });
                Outcome::effects(vec![])
            }
            Intent::SelectNext | Intent::ScrollPageDown => {
                view.selected = (selected + 1).min(last);
                view.notice = None;
                self.overlay = Some(Overlay::Models { view });
                Outcome::effects(vec![])
            }
            // A Model the endpoint listed but the profile does not declare is
            // offered by this press rather than being a row that cannot be
            // acted on: the catalogue is a menu, not a report.
            // The whole list at once. Offering every Model the catalogue lists
            // is one press rather than one per row, because a catalogue is a
            // menu and a reader who wants all of it has already decided.
            Intent::ConfigModelEnableAll => {
                let catalogue = self.management_data.provider_models.clone();
                let models = self.models_with(&view, |models| {
                    for model in catalogue {
                        match models.iter_mut().find(|held| held.id == model.id) {
                            Some(held) => held.enabled = true,
                            None => {
                                let mut offered = model;
                                offered.enabled = true;
                                models.push(offered);
                            }
                        }
                    }
                });
                self.write_models(view, models)
            }
            // Disabling is about what the profile holds: a Model it does not
            // hold has nothing to switch off, and adding one to switch it off
            // would be the opposite of what was asked.
            Intent::ConfigModelDisableAll => {
                let models = self.models_with(&view, |models| {
                    for model in models.iter_mut() {
                        model.enabled = false;
                    }
                });
                self.write_models(view, models)
            }
            Intent::ConfigModelToggle => {
                let Some(row) = rows.get(selected) else {
                    return Outcome::quiet();
                };
                let models = self.models_with(&view, |models| {
                    match models.iter_mut().find(|model| model.id == row.id) {
                        Some(model) => model.enabled = !row.enabled,
                        None => models.push(vibex_core::ProviderConfiguredModel {
                            id: row.id.clone(),
                            display_name: None,
                            enabled: true,
                            wire_api: None,
                            capabilities: vibex_core::ProviderModelCapabilities::default(),
                        }),
                    }
                });
                self.write_models(view, models)
            }
            Intent::ConfigModelRemove => {
                let Some(row) = rows.get(selected) else {
                    return Outcome::quiet();
                };
                if !row.declared {
                    return Outcome::quiet();
                }
                let models =
                    self.models_with(&view, |models| models.retain(|model| model.id != row.id));
                self.write_models(view, models)
            }
            Intent::ConfigModelFetch => {
                let Ok(profile_id) = parse_config_id(&view.profile_id) else {
                    return Outcome::quiet();
                };
                self.overlay = Some(Overlay::Models { view });
                Outcome::effects(vec![Effect::FetchProviderModels { profile_id }])
            }
            // A Model the profile does not declare has no settings yet: there is
            // nothing to edit until the press that offers it makes it part of
            // the profile.
            Intent::ConfigModelEdit => {
                let Some(row) = rows.get(selected).filter(|row| row.declared) else {
                    return Outcome::quiet();
                };
                let Some(model) = self
                    .provider(&view.profile_id)
                    .and_then(|profile| {
                        profile
                            .configured_models
                            .iter()
                            .find(|model| model.id == row.id)
                    })
                    .cloned()
                else {
                    return Outcome::quiet();
                };
                let form = crate::config_center::provider_model_form(
                    &view.agent_id,
                    &view.profile_id,
                    &model,
                    self.strings,
                );
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
            Intent::ConfigModelAdd => {
                self.overlay = Some(Overlay::Prompt {
                    title: self.strings.config_form_add_model().to_string(),
                    field: PromptField::NewModelId,
                    value: String::new(),
                    target: Some(view.profile_id.clone()),
                });
                Outcome::effects(vec![])
            }
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                Outcome::effects(vec![])
            }
            _ => {
                self.overlay = Some(Overlay::Models { view });
                Outcome::quiet()
            }
        }
    }

    /// The profile's Model list with one change applied to it.
    fn models_with(
        &self,
        view: &crate::config_center::ModelListView,
        change: impl FnOnce(&mut Vec<vibex_core::ProviderConfiguredModel>),
    ) -> Vec<vibex_core::ProviderConfiguredModel> {
        let mut models = self
            .provider(&view.profile_id)
            .map(|profile| profile.configured_models.clone())
            .unwrap_or_default();
        change(&mut models);
        models
    }

    /// Write a profile's Model list through the profile it belongs to.
    fn write_models(
        &mut self,
        view: crate::config_center::ModelListView,
        models: Vec<vibex_core::ProviderConfiguredModel>,
    ) -> Outcome {
        let (Ok(agent_id), Ok(profile_id)) = (
            parse_config_id(&view.agent_id),
            parse_config_id(&view.profile_id),
        ) else {
            return Outcome::quiet();
        };
        let outcome =
            self.begin_config_mutation(crate::app::ConfigMutation::UpdateProviderProfile {
                agent_id,
                profile_id,
                display_name: None,
                base_url: None,
                default_model: None,
                configured_models: Some(models),
                provider_options: None,
            });
        // The editor stays where it was: a write is not a reason to take the
        // list away from the reader, and the re-read lands behind it.
        self.overlay = Some(Overlay::Models { view });
        outcome
    }

    /// Move one profile a place earlier or later among its Agent's profiles.
    ///
    /// The arrangement is the whole list, not one index: the authority is asked
    /// to hold this order, and an order that named only the moved profile would
    /// say nothing about the ones it moved past.
    fn move_provider(&mut self, key: &str, delta: isize) -> Outcome {
        let Some(profile) = self.provider(key).cloned() else {
            return Outcome::quiet();
        };
        let agent_id = profile.agent_id.clone();
        // The reader's order is the order on screen, which is what the rows
        // were sorted by; a profile the arrangement does not name sorts last.
        let mut siblings = self
            .management_data
            .providers
            .iter()
            .filter(|candidate| candidate.agent_id == agent_id)
            .collect::<Vec<_>>();
        siblings.sort_by_key(|candidate| {
            self.management_data
                .provider_order
                .iter()
                .find(|entry| entry.provider_profile_id == candidate.id)
                .map(|entry| entry.order_index)
                .unwrap_or(i64::MAX)
        });
        let Some(index) = siblings
            .iter()
            .position(|candidate| candidate.id == profile.id)
        else {
            return Outcome::quiet();
        };
        let next = index.saturating_add_signed(delta);
        if next == index || next >= siblings.len() {
            return Outcome::quiet();
        }
        let mut profiles = siblings
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        profiles.swap(index, next);
        self.begin_config_mutation(crate::app::ConfigMutation::SetProviderOrder {
            agent_id,
            profiles,
        })
    }

    /// Open one of a profile's surfaces, reading the profile first if this
    /// client has not read it.
    ///
    /// The list answers with a summary, so an editor opened straight from it
    /// would be editing a record whose endpoint it cannot see — and a field it
    /// cannot see is a field a save leaves empty. The read is a round trip, so
    /// what the reader asked for travels with it and the surface opens on the
    /// answer.
    fn open_provider(
        &mut self,
        agent_id: vibex_core::AgentId,
        profile_id: vibex_core::ProviderProfileId,
        open: crate::app::ProviderOpen,
    ) -> Outcome {
        if self.provider_detail(profile_id.as_str()).is_none() {
            self.pending_provider_open = Some((profile_id.clone(), open));
            return Outcome::effects(vec![Effect::LoadProviderProfile {
                agent_id,
                profile_id,
            }]);
        }
        self.show_provider_open(&profile_id, open)
    }

    /// Open the surface a profile read was for, from what this client holds.
    pub fn show_provider_open(
        &mut self,
        profile_id: &vibex_core::ProviderProfileId,
        open: crate::app::ProviderOpen,
    ) -> Outcome {
        use crate::app::ProviderOpen;
        let strings = self.strings;
        let detail = self.provider_detail(profile_id.as_str()).cloned();
        let summary = self
            .management_data
            .providers
            .iter()
            .find(|profile| profile.id == *profile_id)
            .cloned();
        let form = match open {
            ProviderOpen::Edit => summary.as_ref().map(|profile| {
                crate::config_center::provider_form(profile, detail.as_ref(), strings)
            }),
            ProviderOpen::Duplicate => {
                Some(self.provider_create_form(summary.as_ref(), detail.as_ref(), strings))
            }
            // Ordering acts on the list rather than on one record, and a
            // profile's Models are in the summary already.
            ProviderOpen::Order | ProviderOpen::Models => None,
        };
        match form {
            Some(form) => {
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
            None => Outcome::quiet(),
        }
    }

    /// The full configuration of one profile, when this client has read it.
    fn provider_detail(&self, key: &str) -> Option<&vibex_core::ProviderProfile> {
        self.management_data
            .provider_detail
            .as_ref()
            .filter(|detail| detail.id.as_str() == key)
    }

    /// The form that makes a profile, given the Agent it may belong to.
    ///
    /// The Agents offered are the ones the runtime says can hold a provider
    /// configuration, intersected with the ones this client has been told
    /// about: offering an Agent that cannot hold one would be a form whose save
    /// the authority refuses.
    fn provider_create_form(
        &self,
        source: Option<&vibex_core::ProviderProfileSummary>,
        detail: Option<&vibex_core::ProviderProfile>,
        strings: crate::locale::Strings,
    ) -> crate::config_center::ConfigForm {
        let configurable = vibex_core::model_provider_configurable_agent_ids().unwrap_or_default();
        let agents = self
            .management_data
            .agents
            .iter()
            .filter(|agent| configurable.contains(&agent.id))
            .cloned()
            .collect::<Vec<_>>();
        crate::config_center::provider_create_form(&agents, source, detail, strings)
    }

    /// The per-Agent matrix of one MCP server or Skill.
    /// Install one registry entry the reader chose.
    ///
    /// The entry is resolved here, where the page the registry answered is
    /// still in hand: the install request needs the entry's own launcher, and
    /// the row key alone would make the worker guess at it.
    fn install_registry_entry(&mut self, key: &str) -> Outcome {
        use crate::config_center::ConfigModule;
        if self.config_center.module == ConfigModule::Mcp {
            let Some(entry) = self.mcp_market_entry(key) else {
                return Outcome::quiet();
            };
            // Installing is a form rather than a press: the registry names the
            // variables it needs and never the reader's own values for them,
            // and it has no idea which Agents should receive the server.
            let form = crate::config_center::mcp_market_install_form(
                &entry,
                &self.management_data.agents,
                self.strings,
            );
            self.overlay = Some(Overlay::Form { form });
            return Outcome::effects(vec![]);
        }
        let Some(entry) = self
            .management_data
            .skill_market
            .iter()
            .find(|entry| entry.id == key)
            .cloned()
        else {
            return Outcome::quiet();
        };
        // A published Skill is a folder with instructions in it, so the
        // install shows what would be written before it asks who receives it:
        // a reader who has not seen the bundle is not in a position to agree
        // to it.
        self.overlay = Some(Overlay::SkillPreview {
            view: crate::config_center::SkillPreviewView {
                entry_id: entry.id.clone(),
                title: entry.name.clone(),
                scroll: 0,
                loading: true,
                notice: None,
            },
        });
        Outcome::effects(vec![Effect::PreviewSkillMarket {
            entry_id: entry.id.clone(),
            slug: entry.slug.clone(),
            owner_handle: entry.owner_handle.clone(),
            version: entry.version.clone(),
        }])
    }

    /// The keys of a Skill preview.
    fn perform_skill_preview(
        &mut self,
        intent: Intent,
        mut view: crate::config_center::SkillPreviewView,
    ) -> Outcome {
        let lines = crate::config_center::skill_preview_lines(
            self.management_data.skill_preview.as_deref(),
            self.strings,
        );
        let last = lines.len().saturating_sub(1);
        match intent {
            Intent::SelectNext | Intent::ScrollPageDown => {
                view.scroll = (view.scroll + 1).min(last);
                self.overlay = Some(Overlay::SkillPreview { view });
                Outcome::effects(vec![])
            }
            Intent::SelectPrevious | Intent::ScrollPageUp => {
                view.scroll = view.scroll.saturating_sub(1);
                self.overlay = Some(Overlay::SkillPreview { view });
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => {
                // A document the runtime would refuse to write is refused here,
                // with the size that makes it too large, rather than after the
                // reader has answered the rest of the questions.
                if let Some(document) = self.management_data.skill_preview.as_deref()
                    && document.too_large
                {
                    self.toast(Toast::warning(
                        self.strings
                            .config_preview_too_large()
                            .replace("{bytes}", &document.bytes.to_string()),
                    ));
                    return Outcome::quiet();
                }
                let Some(entry) = self
                    .management_data
                    .skill_market
                    .iter()
                    .find(|entry| entry.id == view.entry_id)
                    .cloned()
                else {
                    return Outcome::quiet();
                };
                // A Skill is stored once and enabled per Agent, so the install
                // asks who receives it before it writes anything.
                let form = crate::config_center::skill_market_install_form(
                    &entry,
                    &self.management_data.agents,
                    self.strings,
                );
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
            Intent::CloseOverlay | Intent::Back => {
                self.overlay = None;
                Outcome::effects(vec![])
            }
            _ => {
                self.overlay = Some(Overlay::SkillPreview { view });
                Outcome::quiet()
            }
        }
    }

    /// Check one registry entry's launcher before it is installed.
    ///
    /// The entry has no record to name, so the candidate the registry published
    /// travels with the request: the answer is about the entry the reader is
    /// looking at, and the reader finds out whether it would launch before
    /// anything is written rather than after.
    fn validate_registry_entry(&mut self, key: &str) -> Outcome {
        use crate::config_center::ConfigModule;
        if self.config_center.module != ConfigModule::Mcp {
            return Outcome::quiet();
        }
        let Some(entry) = self.mcp_market_entry(key) else {
            return Outcome::quiet();
        };
        self.begin_config_mutation(crate::app::ConfigMutation::ValidateMcpMarketEntry {
            entry_id: entry.id.clone(),
            candidate: Box::new(crate::config_center::mcp_market_candidate(&entry)),
        })
    }

    /// One entry of the registry page the centre asked for.
    fn mcp_market_entry(&self, key: &str) -> Option<vibex_core::McpMarketEntry> {
        self.management_data
            .mcp_market
            .iter()
            .find(|entry| entry.id == key)
            .cloned()
    }

    /// Import one discovered resource, carrying the candidate the runtime
    /// published for it.
    fn import_discovery(&mut self, key: &str) -> Outcome {
        use crate::config_center::ConfigModule;
        if self.config_center.module == ConfigModule::Mcp {
            let Some(selection) = self
                .management_data
                .mcp_discoveries
                .iter()
                .find(|discovery| discovery.discovery_id == key)
                .and_then(|discovery| {
                    discovery.candidate.clone().map(|candidate| {
                        vibex_core::McpServerImportSelection {
                            discovery_id: discovery.discovery_id.clone(),
                            source_agent_id: discovery.source_agent_id.clone(),
                            candidate,
                            enable_agent_ids: vec![discovery.source_agent_id.clone()],
                        }
                    })
                })
            else {
                return Outcome::quiet();
            };
            return self.begin_config_mutation(crate::app::ConfigMutation::ImportMcpServer {
                selection: Box::new(selection),
            });
        }
        let Some(selection) = self
            .management_data
            .skill_discoveries
            .iter()
            .find(|discovery| discovery.discovery_id == key)
            .map(|discovery| vibex_core::SkillImportSelection {
                discovery_id: discovery.discovery_id.clone(),
                source_agent_id: discovery.source_agent_id.clone(),
                source_path: discovery.source_path.clone(),
                display_name: discovery.display_name.clone(),
                command_name: discovery.command_name.clone(),
                description: discovery.description.clone(),
                content_preview: discovery.content_preview.clone(),
                body: discovery.body.clone(),
                enable_agent_ids: vec![discovery.source_agent_id.clone()],
            })
        else {
            return Outcome::quiet();
        };
        self.begin_config_mutation(crate::app::ConfigMutation::ImportSkill {
            selection: Box::new(selection),
        })
    }

    fn begin_matrix_form(&mut self, key: &str) -> Outcome {
        let strings = self.strings;
        let agents = self.management_data.agents.clone();
        let form = match self.config_center.section {
            crate::config_center::ConfigSection::Servers => self
                .mcp_server(key)
                .cloned()
                .map(|server| crate::config_center::mcp_matrix_form(&server, &agents, strings)),
            crate::config_center::ConfigSection::Installed => self
                .skill(key)
                .cloned()
                .map(|skill| crate::config_center::skill_matrix_form(&skill, &agents, strings)),
            _ => None,
        };
        match form {
            Some(form) => {
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
            None => Outcome::quiet(),
        }
    }

    /// A form key the reader is typing into, delivered one character at a time.
    pub fn type_form_char(&mut self, character: char) {
        let Some(Overlay::Form { form }) = self.overlay.as_mut() else {
            return;
        };
        let Some(key) = form.editing.clone() else {
            return;
        };
        if let Some(field) = form.field_mut(&key) {
            field.value.push(character);
        }
    }

    pub fn form_backspace(&mut self) {
        let Some(Overlay::Form { form }) = self.overlay.as_mut() else {
            return;
        };
        let Some(key) = form.editing.clone() else {
            return;
        };
        if let Some(field) = form.field_mut(&key) {
            field.value.pop();
        }
    }

    fn perform_form(&mut self, intent: Intent, form: crate::config_center::ConfigForm) -> Outcome {
        // While a field is being typed into, the field owns the keys, and the
        // ones that matter are "keep this" and "put it back".
        if form.editing.is_some() {
            let multiline = form.selected_field().is_some_and(|field| {
                matches!(field.kind, crate::config_center::FormFieldKind::Multiline)
            });
            return match intent {
                // `Enter` keeps a one-line field and takes a newline in a
                // multi-line one: a body is a document, and a key that ended
                // it would make the reader reach for the mouse to write a
                // second paragraph.
                Intent::ConfirmOverlay if multiline => {
                    self.type_form_char('\n');
                    Outcome::quiet()
                }
                Intent::ConfirmOverlay => {
                    self.commit_form_field(form);
                    Outcome::effects(vec![])
                }
                // `Tab` keeps the field and moves on, in every field.
                Intent::OverlayNextField => {
                    self.commit_form_field(form);
                    Outcome::effects(vec![])
                }
                Intent::OverlayPreviousField => {
                    self.commit_form_field(form);
                    self.step_form_field(-2);
                    Outcome::effects(vec![])
                }
                Intent::ConfigFormSave => self.submit_form(form),
                Intent::ScrollPageUp | Intent::ScrollPageDown => Outcome::quiet(),
                _ => Outcome::quiet(),
            };
        }
        match intent {
            Intent::OverlayNextField | Intent::SelectNext | Intent::ScrollPageDown => {
                self.step_form_field(1);
                Outcome::effects(vec![])
            }
            Intent::OverlayPreviousField | Intent::SelectPrevious | Intent::ScrollPageUp => {
                self.step_form_field(-1);
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => self.begin_form_field(form),
            // `←`/`→` are the value keys in every other form this client
            // draws — the elicitation card, the approval card — so they are
            // the value keys here too.
            Intent::ElicitationFieldPrevious => self.step_form_value(-1),
            Intent::ElicitationFieldNext => self.step_form_value(1),
            Intent::ConfigFormSave => self.submit_form(form),
            // A form that reads a command back has one thing on it the reader
            // cannot select, and a client that showed them a launcher and would
            // not hand it over is asking them to retype it.
            Intent::ConfigFormCopy => match form.preview.clone() {
                Some(preview) => {
                    self.toast(Toast::success(self.strings.copied().to_string()));
                    Outcome::effects(vec![Effect::Clipboard { text: preview }])
                }
                None => Outcome::quiet(),
            },
            _ => Outcome::quiet(),
        }
    }

    /// Mount the schedule editor on the Composer in front of the reader.
    ///
    /// The value the draft already carries is what the editor opens on, so a
    /// reader who came back to change the hour does not have to type the whole
    /// deadline again — and a draft with no schedule opens on the deadline
    /// someone who has not decided yet almost always means.
    ///
    /// An empty draft is nothing to put an hour on, and a reader pressing the
    /// clock there is looking at the send their session is already waiting for
    /// — which another client may be the one keeping. Editing that one is the
    /// same gesture: the value travels to whoever holds the clock, and this
    /// client arms nothing it cannot send.
    fn open_message_schedule(&mut self) -> Outcome {
        let Some(target) = self.composer_target.clone() else {
            return Outcome::quiet();
        };
        let now_ms = vibex_core::unix_timestamp_ms();
        let edit = if self.composer.is_empty() && self.draft_schedule().is_none() {
            self.held_schedule_target(&target)
                .unwrap_or(ScheduleEditTarget::Draft(target))
        } else {
            ScheduleEditTarget::Draft(target)
        };
        let initial = match &edit {
            ScheduleEditTarget::Draft(_) => self.draft_schedule(),
            ScheduleEditTarget::Held { session_id, .. } => self
                .authority_scheduled_send(session_id)
                .map(|send| crate::schedule::MessageSchedule::At(send.scheduled_at_ms)),
        };
        self.overlay = Some(Overlay::Schedule(
            crate::schedule::MessageScheduleForm::new(edit, initial, now_ms),
        ));
        Outcome::effects(vec![])
    }

    /// The send this client is not holding, when the Composer is sitting on the
    /// session that is waiting for one.
    ///
    /// The composing page has no session yet, so there is nothing published to
    /// edit there: a draft that has not been sent has no queue entry anywhere.
    fn held_schedule_target(&self, target: &ComposerTarget) -> Option<ScheduleEditTarget> {
        let ComposerTarget::Session(session_id) = target else {
            return None;
        };
        let send = self.authority_scheduled_send(session_id)?;
        Some(ScheduleEditTarget::Held {
            session_id: session_id.clone(),
            message_id: send.message_id,
        })
    }

    /// The schedule editor's own keys.
    ///
    /// A form is a small state machine: `Enter` on the mode row flips it,
    /// `Enter` on a text row commits the whole thing, and `←`/`→` are the value
    /// keys every other form in this client draws — which for a two-valued
    /// shape means the other one.
    fn perform_schedule(
        &mut self,
        intent: Intent,
        form: crate::schedule::MessageScheduleForm,
    ) -> Outcome {
        use crate::schedule::ScheduleField;
        let mut form = form;
        match intent {
            Intent::OverlayNextField | Intent::SelectNext | Intent::ScrollPageDown => {
                form.step_field(1);
                form.error = None;
                self.overlay = Some(Overlay::Schedule(form));
                Outcome::effects(vec![])
            }
            Intent::OverlayPreviousField | Intent::SelectPrevious | Intent::ScrollPageUp => {
                form.step_field(-1);
                form.error = None;
                self.overlay = Some(Overlay::Schedule(form));
                Outcome::effects(vec![])
            }
            // A closed-set row has no text to type, so `Enter` steps it; a text
            // row's `Enter` is the panel's own commit.
            Intent::ConfirmOverlay if form.field == ScheduleField::Mode => {
                form.toggle_mode();
                self.overlay = Some(Overlay::Schedule(form));
                Outcome::effects(vec![])
            }
            Intent::ConfirmOverlay => self.apply_schedule(form),
            Intent::ElicitationFieldPrevious
            | Intent::ElicitationFieldNext
            | Intent::OverlayToggleValue => {
                form.toggle_mode();
                self.overlay = Some(Overlay::Schedule(form));
                Outcome::effects(vec![])
            }
            Intent::ScheduleClear => self.clear_schedule(form),
            _ => Outcome::quiet(),
        }
    }

    /// Write the editor's value onto the draft it was opened for, or send it to
    /// whoever is holding the send it was opened for.
    ///
    /// A value the send path would refuse keeps the surface open with the
    /// reason, which is what lets the reader fix it rather than lose it. The
    /// target is re-checked because the reader may have navigated while the
    /// editor was up: a schedule written onto whatever happens to be in front
    /// of them now is worse than no schedule at all. A held send is re-checked
    /// the same way, against the send the authority last published — a message
    /// that has gone out since cannot be given a new hour.
    fn apply_schedule(&mut self, form: crate::schedule::MessageScheduleForm) -> Outcome {
        let now_ms = vibex_core::unix_timestamp_ms();
        let schedule = match form.validate(now_ms) {
            Ok(schedule) => schedule,
            Err(error) => {
                let mut form = form;
                form.error = Some(error);
                self.overlay = Some(Overlay::Schedule(form));
                return Outcome::effects(vec![]);
            }
        };
        match &form.target {
            ScheduleEditTarget::Draft(target) => {
                if self.composer_target.as_ref() != Some(target) {
                    self.overlay = None;
                    return Outcome::effects(vec![]);
                }
                self.set_draft_schedule(Some(schedule));
                self.overlay = None;
                self.toast(Toast::info(format!(
                    "{} · {}",
                    self.strings.schedule_set(),
                    crate::schedule::schedule_summary(schedule)
                )));
                Outcome::effects(vec![])
            }
            ScheduleEditTarget::Held {
                session_id,
                message_id,
            } => {
                let (session_id, message_id) = (session_id.clone(), *message_id);
                let at_ms = schedule
                    .deadline(now_ms)
                    .expect("a validated schedule names a deadline");
                self.overlay = None;
                let Some(effect) = self.set_held_schedule(session_id, message_id, Some(at_ms))
                else {
                    return Outcome::effects(vec![]);
                };
                self.toast(Toast::info(format!(
                    "{} · {}",
                    self.strings.schedule_set(),
                    crate::schedule::format_send_time(at_ms)
                )));
                Outcome::effects(vec![effect])
            }
        }
    }

    /// Void the draft's schedule — or the held send's. The message is kept:
    /// clearing a timer is not deleting what the reader wrote.
    fn clear_schedule(&mut self, form: crate::schedule::MessageScheduleForm) -> Outcome {
        // Nothing armed is nothing to void: the key that means "clear" must not
        // be the one that closes an editor the reader is still deciding in.
        if !form.clearable {
            return Outcome::quiet();
        }
        match &form.target {
            ScheduleEditTarget::Draft(target) => {
                if self.composer_target.as_ref() != Some(target) {
                    self.overlay = None;
                    return Outcome::effects(vec![]);
                }
                self.set_draft_schedule(None);
                self.overlay = None;
                self.toast(Toast::info(self.strings.schedule_cleared().to_string()));
                Outcome::effects(vec![])
            }
            ScheduleEditTarget::Held {
                session_id,
                message_id,
            } => {
                let (session_id, message_id) = (session_id.clone(), *message_id);
                self.overlay = None;
                let Some(effect) = self.set_held_schedule(session_id, message_id, None) else {
                    return Outcome::effects(vec![]);
                };
                self.toast(Toast::info(self.strings.schedule_cleared().to_string()));
                Outcome::effects(vec![effect])
            }
        }
    }

    /// The authority change that moves, or takes away, a send this client is
    /// not holding.
    ///
    /// `None` when the authority is not publishing that send any more: there is
    /// no clock here to change, and inventing a deadline for a message this
    /// client cannot see would only promise a send nobody has.
    fn set_held_schedule(
        &mut self,
        session_id: VibexSessionId,
        message_id: u64,
        scheduled_at_ms: Option<i64>,
    ) -> Option<crate::app::Effect> {
        let view = self.sidebar_organization()?;
        // The send is looked up in the same snapshot the value is written
        // against, so the reader's glance at the row and the change it makes
        // are about one message.
        let published = view
            .scheduled_sends
            .iter()
            .any(|send| send.session_id == session_id.as_str() && send.message_id == message_id);
        if !published {
            return None;
        }
        Some(crate::app::Effect::MutateSidebarOrganization {
            mutation: vibex_core::RemoteSidebarOrganizationMutation::SetScheduledSend {
                session_id: session_id.as_str().to_string(),
                message_id,
                scheduled_at_ms,
            },
            expected_revision: Some(view.revision),
        })
    }

    /// Ask the holder to send its waiting message now.
    ///
    /// "Now" is a deadline rather than a separate verb: the holder's ordinary
    /// dispatch is what sends a scheduled message, and a deadline that has
    /// arrived is exactly what it acts on. A message this client cannot see —
    /// nothing published, or the reader is on the composing page — says so
    /// instead of silently doing nothing.
    fn send_held_schedule_now(&mut self) -> Outcome {
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let Some(send) = self.authority_scheduled_send(&session_id) else {
            return Outcome::quiet();
        };
        let message_id = send.message_id;
        let at_ms = send.scheduled_at_ms;
        // The earlier of the two: a deadline that has already arrived stays
        // due, and one still ahead comes back to now.
        let at_ms = vibex_core::unix_timestamp_ms().min(at_ms);
        let Some(effect) = self.set_held_schedule(session_id, message_id, Some(at_ms)) else {
            return Outcome::quiet();
        };
        self.toast(Toast::info(self.strings.queue_released().to_string()));
        Outcome::effects(vec![effect])
    }

    fn step_form_field(&mut self, delta: isize) {
        if let Some(Overlay::Form { form }) = self.overlay.as_mut() {
            form.step_field(delta);
        }
    }

    fn step_form_value(&mut self, delta: isize) -> Outcome {
        let strings = self.strings;
        let Some(Overlay::Form { form }) = self.overlay.as_mut() else {
            return Outcome::quiet();
        };
        let index = form.selected;
        let stepped = match form.fields.get_mut(index) {
            Some(field) => field.step(delta),
            None => false,
        };
        // The Agent is a choice and the endpoint is pinned to it, so the pin
        // follows the choice: a field locked for the Agent the form opened on
        // must not stay locked after the reader moves to another one.
        if stepped
            && form
                .fields
                .get(index)
                .is_some_and(|field| field.key == "agent")
        {
            crate::config_center::reaim_base_url_field(form, strings);
        }
        Outcome::effects(vec![])
    }

    fn begin_form_field(&mut self, form: crate::config_center::ConfigForm) -> Outcome {
        let Some(field) = form.selected_field() else {
            return Outcome::quiet();
        };
        // A pinned value is the Agent's, not the reader's: the form says so on
        // the field's own line rather than opening an editor whose answer the
        // authority would refuse.
        if field.locked {
            if let Some(hint) = field.hint.clone() {
                self.toast(Toast::info(hint));
            }
            return Outcome::quiet();
        }
        // A closed set or a switch has no text to type: `Enter` moves it.
        if !matches!(
            field.kind,
            crate::config_center::FormFieldKind::Text
                | crate::config_center::FormFieldKind::Multiline
        ) {
            return self.step_form_value(1);
        }
        let key = field.key.clone();
        if let Some(Overlay::Form { form }) = self.overlay.as_mut() {
            form.editing = Some(key);
        }
        Outcome::effects(vec![])
    }

    fn commit_form_field(&mut self, form: crate::config_center::ConfigForm) {
        let _ = form;
        if let Some(Overlay::Form { form }) = self.overlay.as_mut() {
            form.editing = None;
            form.step_field(1);
        }
    }

    /// `Ctrl+S`: write the form.
    ///
    /// The write is filed under the form rather than under a row: the form is
    /// what the reader is looking at, and a record that does not exist until
    /// the write lands has no row to wear the marker. That is also why the
    /// answer closes the form — the record it produced is on the table behind
    /// it.
    fn submit_form(&mut self, mut form: crate::config_center::ConfigForm) -> Outcome {
        match self.form_mutation(&form) {
            Ok(mutation) => {
                form.error = None;
                let verb = mutation.verb();
                let subject = form.title.clone();
                self.overlay = Some(Overlay::Form { form });
                let effects = vec![Effect::ConfigMutation {
                    mutation,
                    record: crate::config_center::FORM_RECORD.to_string(),
                }];
                self.begin_record_work(
                    crate::config_center::FORM_RECORD.to_string(),
                    subject,
                    verb,
                    effects,
                )
            }
            Err(message) => {
                form.error = Some(message.to_string());
                self.overlay = Some(Overlay::Form { form });
                Outcome::effects(vec![])
            }
        }
    }

    /// Turn a filled-in form into the request it describes.
    fn form_mutation(
        &mut self,
        form: &crate::config_center::ConfigForm,
    ) -> Result<crate::app::ConfigMutation, &'static str> {
        use crate::app::ConfigMutation;
        use crate::config_center::FormPurpose;
        let strings = self.strings;
        // A sign-in form has no record to name and no name field; it is the
        // method's own variables and nothing else.
        if let FormPurpose::AgentAuthMethod { .. } = &form.purpose {
            return self.agent_auth_method_mutation(form);
        }
        let name = form.value("name").unwrap_or_default();
        match &form.purpose {
            FormPurpose::McpServer { server_id } => {
                if name.is_empty() {
                    return Err(strings.config_form_error_name());
                }
                let transport = match form.value("transport").as_deref() {
                    Some("http") => vibex_core::McpServerTransportKind::Http,
                    Some("sse") => vibex_core::McpServerTransportKind::Sse,
                    _ => vibex_core::McpServerTransportKind::Stdio,
                };
                let command = form.optional("command");
                let url = form.optional("url");
                if transport == vibex_core::McpServerTransportKind::Stdio && command.is_none() {
                    return Err(strings.config_form_error_stdio_command());
                }
                if transport != vibex_core::McpServerTransportKind::Stdio && url.is_none() {
                    return Err(strings.config_form_error_remote_url());
                }
                let args = form
                    .value("args")
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let env = parse_pairs(form.value("env").as_deref().unwrap_or_default(), '=')
                    .into_iter()
                    .map(|(name, value)| vibex_core::McpServerEnvEntry { name, value })
                    .collect::<Vec<_>>();
                let headers =
                    parse_pairs(form.value("headers").as_deref().unwrap_or_default(), ':')
                        .into_iter()
                        .map(|(name, value)| vibex_core::McpServerHeaderEntry { name, value })
                        .collect::<Vec<_>>();
                let tags = split_tags(form.value("tags").as_deref().unwrap_or_default());
                let status = match form.value("status").as_deref() {
                    Some("disabled") => vibex_core::McpServerStatus::Disabled,
                    _ => vibex_core::McpServerStatus::Enabled,
                };
                let scope = mcp_scope(&form.value("scope").unwrap_or_default());
                match server_id {
                    Some(id) => Ok(ConfigMutation::UpdateMcpServer(Box::new(
                        vibex_core::McpServerUpdateRequest {
                            mcp_server_id: parse_config_id(id)?,
                            display_name: Some(name),
                            transport_kind: Some(transport),
                            status: Some(status),
                            scope_kind: Some(scope),
                            project_id: None,
                            workspace_id: None,
                            command,
                            args: Some(args),
                            env: Some(env),
                            url,
                            headers: Some(headers),
                            description: form.optional("description"),
                            tags: Some(tags),
                        },
                    ))),
                    None => Ok(ConfigMutation::CreateMcpServer(Box::new(
                        vibex_core::McpServerCreateRequest {
                            display_name: name,
                            transport_kind: transport,
                            status,
                            scope_kind: scope,
                            project_id: None,
                            workspace_id: None,
                            command,
                            args,
                            env,
                            url,
                            headers,
                            description: form.optional("description"),
                            tags,
                            secret_references: Vec::new(),
                            provider_matrix: Vec::new(),
                        },
                    ))),
                }
            }
            FormPurpose::Skill { skill_id } => {
                if name.is_empty() {
                    return Err(strings.config_form_error_name());
                }
                let body = form.value("body").unwrap_or_default();
                let source_kind = match form.value("source_kind").as_deref() {
                    Some("local_folder") => vibex_core::SkillSourceKind::LocalFolder,
                    Some("git_repo") => vibex_core::SkillSourceKind::GitRepo,
                    _ => vibex_core::SkillSourceKind::Manual,
                };
                let status = match form.value("status").as_deref() {
                    Some("disabled") => vibex_core::SkillStatus::Disabled,
                    _ => vibex_core::SkillStatus::Enabled,
                };
                let scope = skill_scope(&form.value("scope").unwrap_or_default());
                let description = form.optional("description");
                let tags = split_tags(form.value("tags").as_deref().unwrap_or_default());
                let source_uri = form.optional("source_uri");
                let preview = Some(body.chars().take(2048).collect::<String>());
                match skill_id {
                    Some(id) => Ok(ConfigMutation::UpdateSkill(Box::new(
                        vibex_core::SkillUpdateRequest {
                            skill_id: parse_config_id(id)?,
                            display_name: Some(name),
                            source_kind: Some(source_kind),
                            status: Some(status),
                            scope_kind: Some(scope),
                            project_id: None,
                            workspace_id: None,
                            source_uri,
                            description,
                            tags: Some(tags),
                            content_preview: preview,
                            body: Some(body),
                        },
                    ))),
                    None => {
                        if body.trim().is_empty() {
                            return Err(strings.config_form_error_body());
                        }
                        Ok(ConfigMutation::CreateSkill(Box::new(
                            vibex_core::SkillCreateRequest {
                                display_name: name,
                                source_kind,
                                status,
                                scope_kind: scope,
                                project_id: None,
                                workspace_id: None,
                                source_uri,
                                description,
                                tags,
                                content_preview: preview,
                                body: Some(body),
                                provider_matrix: Vec::new(),
                            },
                        )))
                    }
                }
            }
            FormPurpose::Prompt { prompt_id } => {
                if name.is_empty() {
                    return Err(strings.config_form_error_name());
                }
                let body = form.value("body").unwrap_or_default();
                if body.trim().is_empty() {
                    return Err(strings.config_form_error_body());
                }
                let kind = match form.value("kind").as_deref() {
                    Some("slash_command") => vibex_core::PromptKind::SlashCommand,
                    Some("system_snippet") => vibex_core::PromptKind::SystemSnippet,
                    _ => vibex_core::PromptKind::ReusablePrompt,
                };
                let status = match form.value("status").as_deref() {
                    Some("disabled") => vibex_core::PromptStatus::Disabled,
                    Some("archived") => vibex_core::PromptStatus::Archived,
                    _ => vibex_core::PromptStatus::Enabled,
                };
                let scope = prompt_scope(&form.value("scope").unwrap_or_default());
                let description = form.optional("description");
                let tags = split_tags(form.value("tags").as_deref().unwrap_or_default());
                match prompt_id {
                    Some(id) => Ok(ConfigMutation::UpdatePrompt(Box::new(
                        vibex_core::PromptUpdateRequest {
                            prompt_id: parse_config_id(id)?,
                            display_name: Some(name),
                            kind: Some(kind),
                            status: Some(status),
                            scope_kind: Some(scope),
                            project_id: None,
                            workspace_id: None,
                            body: Some(body),
                            description,
                            tags: Some(tags),
                        },
                    ))),
                    None => Ok(ConfigMutation::CreatePrompt(Box::new(
                        vibex_core::PromptCreateRequest {
                            display_name: name,
                            kind,
                            status,
                            scope_kind: scope,
                            project_id: None,
                            workspace_id: None,
                            body,
                            description,
                            tags,
                        },
                    ))),
                }
            }
            FormPurpose::ProviderModel {
                agent_id,
                profile_id,
                model_id,
            } => {
                let Some(profile) = self
                    .management_data
                    .providers
                    .iter()
                    .find(|profile| profile.id.as_str() == profile_id.as_str())
                    .cloned()
                else {
                    return Err("unknown record");
                };
                let Some(index) = profile
                    .configured_models
                    .iter()
                    .position(|model| model.id == *model_id)
                else {
                    return Err("unknown record");
                };
                let context_tokens = declared_token_limit(form, "context_tokens", strings)?;
                let output_tokens = declared_token_limit(form, "output_tokens", strings)?;
                // The thinking-level declaration is parsed by the authority's own
                // reader, so the form accepts exactly what dispatch can consume
                // and says why when it cannot.
                let reasoning_efforts = match form.value("reasoning_efforts") {
                    Some(input) if !input.trim().is_empty() => {
                        // The reason is core's own sentence, which names the
                        // level it could not read rather than saying "invalid".
                        vibex_core::ProviderModelReasoningEfforts::parse(&input)
                            .map_err(|_| strings.config_form_error_reasoning_efforts())?
                    }
                    _ => None,
                };
                let mut models = profile.configured_models.clone();
                let model = &mut models[index];
                model.display_name = form.optional("display_name");
                model.wire_api = form
                    .value("wire_api")
                    .filter(|value| value != crate::config_center::INHERIT)
                    .and_then(|value| crate::config_center::wire_api_from_value(&value));
                model.capabilities.reasoning = match form.value("reasoning").as_deref() {
                    Some("reasoning") => Some(true),
                    Some("non_reasoning") => Some(false),
                    _ => None,
                };
                model.capabilities.context_tokens = context_tokens;
                model.capabilities.output_tokens = output_tokens;
                model.capabilities.reasoning_efforts = reasoning_efforts;
                Ok(ConfigMutation::UpdateProviderProfile {
                    agent_id: parse_config_id(agent_id)?,
                    profile_id: parse_config_id(profile_id)?,
                    display_name: None,
                    base_url: None,
                    default_model: None,
                    configured_models: Some(models),
                    provider_options: None,
                })
            }
            FormPurpose::NewCustomAgent => Ok(ConfigMutation::CreateCustomAgent(Box::new(
                crate::config_center::custom_agent_request(form, strings)?,
            ))),
            FormPurpose::SkillMarketInstall {
                entry_id,
                slug,
                owner_handle,
                version,
            } => {
                let agent_ids = form
                    .enabled_agents()
                    .iter()
                    .filter_map(|id| vibex_core::AgentId::parse(id.clone()).ok())
                    .collect::<Vec<_>>();
                if agent_ids.is_empty() {
                    // A Skill enabled for nobody is a Skill the reader cannot
                    // find afterwards, so the authority would refuse it anyway.
                    return Err("no-agent");
                }
                // Only the document for *this* entry travels: a preview left
                // over from another Skill would install the wrong bundle.
                let document = self
                    .management_data
                    .skill_preview
                    .as_ref()
                    .filter(|document| document.entry_id == *entry_id)
                    .cloned();
                Ok(ConfigMutation::InstallSkillMarketEntry {
                    entry_id: entry_id.clone(),
                    slug: slug.clone(),
                    owner_handle: owner_handle.clone(),
                    version: version.clone(),
                    agent_ids,
                    document,
                })
            }
            // Reached only through the sign-in hook above, which handles this
            // purpose before the record match.
            FormPurpose::AgentAuthMethod { .. } => Err("unknown record"),
            FormPurpose::NewProviderProfile { source } => {
                let Some(agent_id) = form
                    .value("agent")
                    .filter(|id| !id.is_empty())
                    .and_then(|id| parse_config_id(&id).ok())
                else {
                    return Err(strings.config_form_error_agent());
                };
                if name.is_empty() {
                    return Err(strings.config_form_error_name());
                }
                // A form pre-filled from a preset writes that preset's protocol
                // explicitly. Without this the projection falls back to the
                // first protocol that has a URL, so an Anthropic preset comes
                // back as Chat Completions the next time the editor opens.
                // Read once, then clear: the preset belongs to this one form,
                // and leaving it set would stamp its protocol onto the next
                // Provider the reader adds from a blank form.
                let preset = self
                    .management_data
                    .pending_provider_preset
                    .take()
                    .and_then(|id| vibex_core::provider_preset(&id));
                // The Models the reader listed are offered as they are written:
                // a wire protocol, a reasoning declaration and a token limit
                // are per-Model settings, and a new profile has none of them
                // until its Models are edited one at a time.
                let preset_wire_api = preset.as_ref().and_then(|preset| preset.default_wire_api());
                let configured_models = form
                    .value("models")
                    .unwrap_or_default()
                    .lines()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(|id| vibex_core::ProviderConfiguredModel {
                        id: id.to_string(),
                        display_name: None,
                        enabled: true,
                        wire_api: preset_wire_api,
                        capabilities: vibex_core::ProviderModelCapabilities::default(),
                    })
                    .collect::<Vec<_>>();
                // Start from the preset's options and let the form override
                // the address, so the per-protocol URLs a multi-protocol
                // preset carries survive.
                let mut provider_options = preset
                    .as_ref()
                    .map(|preset| preset.draft_profile(&agent_id).provider_options);
                if let Some(options) = provider_options.as_mut()
                    && let Some(url) = form.optional("base_url")
                    && let Some(wire_api) = preset_wire_api
                {
                    let key = wire_api.protocol_base_url_option_key();
                    options.entries.retain(|entry| entry.key != key);
                    options
                        .entries
                        .push(vibex_core::ProviderBindingMetadata { key, value: url });
                }
                Ok(ConfigMutation::CreateProviderProfile(Box::new(
                    vibex_core::AgentModelProviderProfileCreateRequest {
                        agent_id,
                        display_name: name,
                        account_alias: form.optional("account_alias"),
                        base_url: form.optional("base_url"),
                        default_model: form.optional("default_model"),
                        // A copy carries the parts the form has no field for: a
                        // copy that dropped them would not be one. The credential
                        // references never travel — a copy starts without a key
                        // rather than sharing one.
                        small_model: source
                            .as_ref()
                            .and_then(|source| source.small_model.clone()),
                        large_model: source
                            .as_ref()
                            .and_then(|source| source.large_model.clone()),
                        configured_models,
                        reasoning_effort: source
                            .as_ref()
                            .and_then(|source| source.reasoning_effort.clone()),
                        sandbox_defaults: source
                            .as_ref()
                            .map(|source| source.sandbox_defaults.clone()),
                        network_defaults: source
                            .as_ref()
                            .map(|source| source.network_defaults.clone()),
                        permission_defaults: source
                            .as_ref()
                            .map(|source| source.permission_defaults.clone()),
                        provider_options: provider_options.or_else(|| {
                            source
                                .as_ref()
                                .map(|source| source.provider_options.clone())
                        }),
                        secret_references: Vec::new(),
                    },
                )))
            }
            FormPurpose::ProviderProfile {
                agent_id,
                profile_id,
            } => {
                if name.is_empty() {
                    return Err(strings.config_form_error_name());
                }
                let profile_id: vibex_core::ProviderProfileId = parse_config_id(profile_id)?;
                let mut draft = crate::config_center::provider_options_draft(form, strings)?;
                // A profile pointed at a subscription gateway that routes by the
                // session sends the header that gateway reads, rather than
                // leaving the reader to find out from the gateway's answer.
                let mut noticed = false;
                if draft.wants_session_header(form.optional("base_url").as_deref()) {
                    let before = draft.headers.len();
                    draft.with_session_header();
                    noticed = draft.headers.len() != before;
                }
                // The options are a bag the profile shares with other writers,
                // so the form's three keys are replaced over what the profile
                // holds and every other entry is left where it was. When the
                // profile could not be read there is nothing to preserve, and
                // the form is what the reader has.
                let stored = self
                    .provider_detail(profile_id.as_str())
                    .map(|detail| detail.provider_options.clone())
                    .unwrap_or_else(vibex_core::ProviderOptions::empty);
                let provider_options = draft.apply(&stored);
                if noticed {
                    self.toast(Toast::info(
                        self.strings.config_result_session_header().to_string(),
                    ));
                }
                Ok(ConfigMutation::UpdateProviderProfile {
                    agent_id: parse_config_id(agent_id)?,
                    profile_id,
                    display_name: Some(name),
                    base_url: form.optional("base_url"),
                    default_model: form.optional("default_model"),
                    configured_models: None,
                    provider_options: Some(Box::new(provider_options)),
                })
            }
            FormPurpose::McpMatrix { server_id } => {
                let server_id: vibex_core::McpServerId = parse_config_id(server_id)?;
                let Some(server) = self
                    .management_data
                    .mcp
                    .iter()
                    .find(|server| server.id == server_id)
                else {
                    return Err("unknown record");
                };
                let enabled = form.enabled_agents();
                // The form draws every Agent the client knows about, so the
                // matrix is rebuilt over that set: an Agent the record had
                // never been assigned to is a switch the reader can turn on,
                // and a matrix built from the record's own entries would drop
                // that press on the floor. Entries the form did not draw —
                // an Agent that has since left the roster — keep what they
                // had rather than being swept away by an editor that never
                // showed them.
                let mut matrix = self
                    .management_data
                    .agents
                    .iter()
                    .map(|agent| {
                        let existing = server
                            .agent_matrix
                            .iter()
                            .find(|entry| entry.agent_id == agent.id);
                        vibex_core::McpServerAgentMatrix {
                            agent_id: agent.id.clone(),
                            enabled: enabled.iter().any(|id| id == agent.id.as_str()),
                            source_kind: existing
                                .map(|entry| entry.source_kind)
                                .unwrap_or(vibex_core::ResourceAgentMatrixSourceKind::Manual),
                            updated_at_ms: existing
                                .map(|entry| entry.updated_at_ms)
                                .unwrap_or_else(vibex_core::unix_timestamp_ms),
                        }
                    })
                    .collect::<Vec<_>>();
                matrix.extend(
                    server
                        .agent_matrix
                        .iter()
                        .filter(|entry| {
                            !self
                                .management_data
                                .agents
                                .iter()
                                .any(|agent| agent.id == entry.agent_id)
                        })
                        .cloned(),
                );
                Ok(ConfigMutation::SetMcpAgentMatrix { server_id, matrix })
            }
            FormPurpose::SkillMatrix { skill_id } => {
                let skill_id: vibex_core::SkillId = parse_config_id(skill_id)?;
                let Some(skill) = self
                    .management_data
                    .skills
                    .iter()
                    .find(|skill| skill.id == skill_id)
                else {
                    return Err("unknown record");
                };
                let enabled = form.enabled_agents();
                // Same rule as the MCP matrix: the form is the set, and an
                // Agent it drew can be switched on whether or not the record
                // already held an entry for it.
                let mut matrix = self
                    .management_data
                    .agents
                    .iter()
                    .map(|agent| {
                        let existing = skill
                            .agent_matrix
                            .iter()
                            .find(|entry| entry.agent_id == agent.id);
                        vibex_core::SkillAgentMatrix {
                            agent_id: agent.id.clone(),
                            enabled: enabled.iter().any(|id| id == agent.id.as_str()),
                            source_kind: existing
                                .map(|entry| entry.source_kind)
                                .unwrap_or(vibex_core::ResourceAgentMatrixSourceKind::Manual),
                            updated_at_ms: existing
                                .map(|entry| entry.updated_at_ms)
                                .unwrap_or_else(vibex_core::unix_timestamp_ms),
                        }
                    })
                    .collect::<Vec<_>>();
                matrix.extend(
                    skill
                        .agent_matrix
                        .iter()
                        .filter(|entry| {
                            !self
                                .management_data
                                .agents
                                .iter()
                                .any(|agent| agent.id == entry.agent_id)
                        })
                        .cloned(),
                );
                Ok(ConfigMutation::SetSkillAgentMatrix { skill_id, matrix })
            }
            FormPurpose::McpMarketInstall {
                entry_id,
                candidate,
            } => {
                // What the reader filled in is written onto the candidate the
                // registry published, so what is installed is the entry the
                // form showed rather than a second reading of a registry that
                // may have moved on since.
                let env = form
                    .fields
                    .iter()
                    .filter_map(|field| {
                        let name = field.key.strip_prefix("env:")?;
                        let value = field.optional()?;
                        Some(vibex_core::McpServerEnvEntry {
                            name: name.to_string(),
                            value,
                        })
                    })
                    .collect::<Vec<_>>();
                let mut candidate = (**candidate).clone();
                candidate.env = env.clone();
                let agent_ids = form
                    .enabled_agents()
                    .iter()
                    .filter_map(|id| parse_config_id(id).ok())
                    .collect();
                Ok(ConfigMutation::InstallMcpMarketEntry {
                    entry_id: entry_id.clone(),
                    candidate: Box::new(candidate),
                    env_values: env,
                    agent_ids,
                })
            }
        }
    }

    /// The write a sign-in form describes.
    fn agent_auth_method_mutation(
        &mut self,
        form: &crate::config_center::ConfigForm,
    ) -> Result<crate::app::ConfigMutation, &'static str> {
        let crate::config_center::FormPurpose::AgentAuthMethod {
            agent_id,
            method_id,
        } = &form.purpose
        else {
            return Err("unknown record");
        };
        let strings = self.strings;
        let Some(catalog) = self.management_data.agent_auth_catalog.as_ref() else {
            return Err("unknown record");
        };
        let Some(method) = catalog
            .methods
            .iter()
            .find(|method| method.id == *method_id)
        else {
            return Err("unknown record");
        };
        let values =
            crate::config_center::agent_auth_method_values(form, &method.environment, strings)?;
        Ok(crate::app::ConfigMutation::AuthenticateAgentMethod {
            agent_id: parse_config_id(agent_id)?,
            method_id: method_id.clone(),
            values,
        })
    }

    /// Mark a write in flight and ask the worker for it.
    ///
    /// The record's own row wears the marker, so the key is the row's and the
    /// sentence the answer will be made of is taken from it here — the title
    /// the reader sees, not the id the write travels under.
    fn begin_config_mutation(&mut self, mutation: crate::app::ConfigMutation) -> Outcome {
        let key = mutation.key();
        let subject = self.config_row_subject(&key);
        let verb = mutation.verb();
        let effects = vec![Effect::ConfigMutation {
            mutation,
            record: key.clone(),
        }];
        self.begin_record_work(key, subject, verb, effects)
    }

    /// Mark one record as working and hand back the effects that do the work.
    ///
    /// Every verb in the configuration centre goes through here, whether it is
    /// a write boxed as a [`crate::app::ConfigMutation`] or one of the reads
    /// and updates the authority already had. What matters to the reader is the
    /// same either way: the row says it is working while it works, and the
    /// answer comes back filed under the record it was about. A second press on
    /// a record that is already working is refused rather than queued, because
    /// two of the same call in flight is how a row ends up describing a write
    /// that finished before the one it is waiting on.
    fn begin_record_work(
        &mut self,
        key: String,
        subject: String,
        verb: crate::config_center::ConfigVerb,
        effects: Vec<Effect>,
    ) -> Outcome {
        if !self.config_center.begin_pending(key, subject, verb) {
            self.toast(Toast::warning(
                self.strings.config_action_blocked().to_string(),
            ));
            return Outcome::quiet();
        }
        Outcome::effects(effects)
    }

    /// The record a key names, as the table draws it.
    ///
    /// Every verb in the centre resolves through the key rather than through
    /// the cursor, so the record it acts on is the record the reader pressed
    /// on. A row that is no longer listed answers `None`, which is what lets a
    /// verb that needs the record itself — opening its editor — refuse rather
    /// than guess.
    fn config_row(&self, key: &str) -> Option<crate::config_center::ConfigRow> {
        self.config_center
            .rows(&self.management_data, self.strings)
            .into_iter()
            .find(|row| row.key == key)
    }

    /// The title of the record a key names, as the table draws it.
    ///
    /// The answer is a sentence about a record the reader can see, so it is
    /// read off the row rather than reconstructed from the identifier: a form's
    /// save names the form, and a record that is no longer listed falls back to
    /// the key, which is at least true.
    fn config_row_subject(&self, key: &str) -> String {
        self.config_row(key)
            .map(|row| row.title)
            .unwrap_or_else(|| key.to_string())
    }

    /// Mark a record's switch as working, naming the half of the switch it
    /// asked for.
    ///
    /// "Enabled" and "Disabled" are the same call to the authority and two
    /// different things to the reader, so the direction is what the row says.
    fn begin_toggle(&mut self, key: &str, enabled: bool, effect: Effect) -> Outcome {
        let verb = if enabled {
            crate::config_center::ConfigVerb::Enable
        } else {
            crate::config_center::ConfigVerb::Disable
        };
        // A switch is the one record write whose intent cannot be misread: the
        // reader is looking at a row that says "off" and asking for "on", and
        // the row answering immediately is the difference between a switch and
        // a request. Everything else waits for the authority, because a record
        // patched from a form would be this client's guess at what it wrote.
        self.flip_toggled_record(key, enabled);
        let subject = self.config_row_subject(key);
        self.begin_record_work(key.to_string(), subject, verb, vec![effect])
    }

    fn guard(&mut self, operation: BackendOperation, effect: Effect) -> Outcome {
        match self.unavailable_outcome(operation) {
            Some(outcome) => outcome,
            None => Outcome::effects(vec![effect]),
        }
    }

    /// Move the record a switch is about, before the authority answers.
    ///
    /// Nothing here survives a refusal: the write re-reads the area it touched,
    /// so a switch the authority declined is drawn back the way it was.
    fn flip_toggled_record(&mut self, key: &str, enabled: bool) {
        use crate::config_center::ConfigSection;
        match self.config_center.section {
            ConfigSection::Servers => {
                if let Some(server) = self
                    .management_data
                    .mcp
                    .iter_mut()
                    .find(|server| server.id.as_str() == key)
                {
                    server.status = if enabled {
                        vibex_core::McpServerStatus::Enabled
                    } else {
                        vibex_core::McpServerStatus::Disabled
                    };
                }
            }
            ConfigSection::Installed => {
                if let Some(skill) = self
                    .management_data
                    .skills
                    .iter_mut()
                    .find(|skill| skill.id.as_str() == key)
                {
                    skill.status = if enabled {
                        vibex_core::SkillStatus::Enabled
                    } else {
                        vibex_core::SkillStatus::Disabled
                    };
                }
            }
            ConfigSection::Prompts => {
                if let Some(prompt) = self
                    .management_data
                    .prompts
                    .iter_mut()
                    .find(|prompt| prompt.id.as_str() == key)
                {
                    prompt.status = if enabled {
                        vibex_core::PromptStatus::Enabled
                    } else {
                        vibex_core::PromptStatus::Disabled
                    };
                }
            }
            ConfigSection::Providers => {
                if key == "gateway"
                    && let Some(snapshot) = self.management_data.gateway.as_mut()
                {
                    // The port is not flipped with the status: only the
                    // authority knows which port comes back, and a client that
                    // guessed one would draw an address nobody is listening on.
                    snapshot.runtime.status = if enabled {
                        vibex_core::ModelGatewayStatus::Running
                    } else {
                        vibex_core::ModelGatewayStatus::Disabled
                    };
                    snapshot.runtime.port = None;
                    snapshot.runtime.notice = None;
                }
            }
            _ => {}
        }
    }

    /// [`Self::guard`] for one record's verb.
    ///
    /// The order matters: a backend that cannot run the operation answers
    /// before the row is marked, so the row never says it is working over a
    /// call that was never made.
    fn guard_record(
        &mut self,
        operation: BackendOperation,
        key: &str,
        subject: String,
        verb: crate::config_center::ConfigVerb,
        effect: Effect,
    ) -> Outcome {
        match self.unavailable_outcome(operation) {
            Some(outcome) => outcome,
            None => self.begin_record_work(key.to_string(), subject, verb, vec![effect]),
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
    /// Opens the new-Provider form pre-filled from a preset.
    ///
    /// The protocol is written into the form's own `wireApi` field rather than
    /// left to be inferred, so the Provider keeps the protocol the preset
    /// chose even after a save and reopen.
    fn open_provider_form_from_preset(
        &mut self,
        agent_id: &str,
        preset: &vibex_core::ProviderPreset,
    ) -> Outcome {
        let Ok(agent_id) = vibex_core::AgentId::parse(agent_id.to_string()) else {
            return Outcome::quiet();
        };
        // The preset fills the form's own fields rather than a "copy" of
        // another Profile: nothing is being duplicated, and the reader is
        // naming a new Provider. The protocol rides in the Provider options so
        // a save and reopen keeps the one the preset chose.
        let mut form = self.provider_create_form(None, None, self.strings);
        if let Some(field) = form.field_mut("name") {
            field.value = preset.display_name.clone();
        }
        if let Some(field) = form.field_mut("base_url")
            && let Some(url) = preset
                .default_wire_api()
                .and_then(|wire_api| preset.endpoint_for(wire_api))
        {
            field.value = url.to_string();
        }
        if let Some(field) = form.field_mut("default_model") {
            field.value = preset.models.first().cloned().unwrap_or_default();
        }
        if let Some(field) = form.field_mut("models") {
            field.value = preset.models.join("\n");
        }
        if let Some(field) = form.field_mut("agent") {
            field.value = agent_id.as_str().to_string();
        }
        // Remember which preset pre-filled the form so the save can write its
        // protocol explicitly instead of letting the projection infer one.
        self.management_data.pending_provider_preset = Some(preset.id.clone());
        self.toast(Toast::info(format!(
            "{} · {}",
            preset.display_name,
            preset
                .endpoint_hint
                .clone()
                .unwrap_or_else(|| preset.id.clone())
        )));
        self.overlay = Some(Overlay::Form { form });
        Outcome::effects(vec![])
    }

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
            // `Esc` means "undo the thing I am in the middle of" everywhere
            // else in this client, and a form has two things to be in the
            // middle of: first the field being typed into, then the form.
            Intent::CloseOverlay | Intent::Back
                if matches!(
                    &self.overlay,
                    Some(Overlay::Form { form }) if form.editing.is_some()
                ) =>
            {
                if let Some(Overlay::Form { form }) = self.overlay.as_mut() {
                    let key = form.editing.take();
                    if let Some(key) = key
                        && let Some(field) = form.field_mut(&key)
                        && !matches!(field.kind, crate::config_center::FormFieldKind::Multiline)
                    {
                        // A one-line field gives its draft up; a body keeps it,
                        // because a paragraph that vanished on `Esc` is a
                        // paragraph the reader has to write twice.
                        field.value = field.origin.clone();
                    }
                }
                return Outcome::effects(vec![]);
            }
            Intent::CloseOverlay | Intent::Back => {
                // The switcher's two views nest: the run options were opened
                // from the catalogue, and the value list from the run options,
                // so `Esc` steps back one level at a time rather than dropping
                // the reader out of the surface they are working in.
                if matches!(
                    self.overlay,
                    Some(Overlay::Form { .. })
                        | Some(Overlay::EntryActions { .. })
                        | Some(Overlay::Confirm { .. })
                ) {
                    self.pending_config_action = None;
                }
                // A form the reader has typed into is not thrown away on one
                // press: everything else in this client can be done again, and
                // what was typed cannot.
                if let Some(Overlay::Form { form }) = self.overlay.as_ref()
                    && form.dirty()
                {
                    let form = form.clone();
                    self.pending_form = Some(form);
                    self.overlay = Some(Overlay::Confirm {
                        title: self.strings.config_discard_title().to_string(),
                        body: self.strings.config_discard_body().to_string(),
                        confirm: Intent::ConfigFormDiscard,
                    });
                    return Outcome::effects(vec![]);
                }
                self.overlay = match self.overlay.take() {
                    // The account panel's sign-out question is one level inside
                    // it: `Esc` gives up the question before it gives up the
                    // panel, the way every other two-step in this client works.
                    Some(Overlay::AgentAuth { mut view }) if view.confirming_sign_out => {
                        view.confirming_sign_out = false;
                        Some(Overlay::AgentAuth { view })
                    }
                    // Backing out of the discard question returns the form
                    // rather than the surface behind it.
                    Some(Overlay::Confirm {
                        confirm: Intent::ConfigFormDiscard,
                        ..
                    }) => self.pending_form.take().map(|form| Overlay::Form { form }),
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
            // Naming somewhere else to be is leaving, and the key bar and the
            // cheatsheet go on naming these while a modal is up: an overlay
            // that swallowed them would be promising keys it does not answer.
            // Putting the surface down first is `Esc`'s job, so a reader who
            // asked for Settings with a modal open asked for exactly that.
            _ if intent.leaves_the_surface() => {
                self.overlay = None;
                return self.perform_intent(intent);
            }
            _ => {}
        }
        let Some(overlay) = self.overlay.clone() else {
            return Outcome::quiet();
        };
        match overlay {
            // The two configuration-centre overlays own their keys, because a
            // form is a small state machine of its own: what `Enter` means
            // depends on whether a field is being typed into.
            Overlay::Form { form } => self.perform_form(intent, form),
            Overlay::EntryActions { entry } => self.perform_entry_actions(intent, entry),
            Overlay::SidebarRowActions(menu) => self.perform_sidebar_row_actions(intent, menu),
            Overlay::Schedule(form) => self.perform_schedule(intent, form),
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
                target,
            } => match intent {
                Intent::ConfirmOverlay => {
                    self.overlay = None;
                    let mut outcome = self.submit_prompt(field, value, title, target);
                    outcome.dirty = true;
                    outcome
                }
                _ => Outcome::quiet(),
            },
            Overlay::Models { view } => self.perform_models(intent, view),
            Overlay::NativeImport { view } => self.perform_native_import(intent, view),
            Overlay::NativeExport { view } => self.perform_native_export(intent, *view),
            Overlay::SkillPreview { view } => self.perform_skill_preview(intent, view),
            Overlay::AgentAuth { view } => self.perform_agent_auth(intent, view),
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
            Overlay::Elicitation { .. } => match intent {
                // `Enter` is the card's one key: it confirms the answer on a
                // question tab and sends the form from the Confirm tab. `s`
                // keeps working as the desktop's Submit.
                Intent::ElicitationSubmit => self.submit_elicitation(),
                Intent::ConfirmOverlay => self.confirm_elicitation(),
                // The approval card's "deny" is the form's "decline": one verb
                // for refusing the Agent's request, whichever card asks.
                Intent::ApprovalDeny => self.decline_elicitation(),
                Intent::ElicitationFieldNext | Intent::OverlayNextField => {
                    self.step_elicitation_field(1)
                }
                Intent::ElicitationFieldPrevious | Intent::OverlayPreviousField => {
                    self.step_elicitation_field(-1)
                }
                Intent::SelectNext => self.step_elicitation_cursor(1),
                Intent::SelectPrevious => self.step_elicitation_cursor(-1),
                Intent::OverlayToggleValue => self.toggle_elicitation_choice(),
                _ => Outcome::quiet(),
            },
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
            Overlay::ProviderPresetPicker { selected, agent_id } => match intent {
                Intent::ConfirmOverlay | Intent::ApprovalApprove => {
                    let presets = vibex_core::provider_presets();
                    match presets.get(selected) {
                        Some(preset) => {
                            // The preset fills the form; it does not create
                            // anything, because the reader still has to name
                            // the Provider and supply a key.
                            self.overlay = None;
                            self.open_provider_form_from_preset(&agent_id, preset)
                        }
                        None => Outcome::quiet(),
                    }
                }
                Intent::SelectNext => {
                    let count = vibex_core::provider_presets().len();
                    self.overlay = Some(Overlay::ProviderPresetPicker {
                        selected: if count == 0 {
                            0
                        } else {
                            (selected + 1) % count
                        },
                        agent_id: agent_id.clone(),
                    });
                    Outcome::effects(vec![])
                }
                Intent::SelectPrevious => {
                    self.overlay = Some(Overlay::ProviderPresetPicker {
                        selected: selected.saturating_sub(1),
                        agent_id: agent_id.clone(),
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
                self.perform(listing.entry.intent)
            }
            // Both arrows wrap. The list is a ring rather than a shelf: the
            // reader holding `↓` on the last command arrives back at the first
            // one instead of stopping, and `↑` on the first goes to the last.
            // One rule for both directions is also what keeps a palette whose
            // length changes under a query from reading as a boundary.
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
                let count = self.palette_entries(&query).len();
                self.overlay = Some(Overlay::Palette {
                    query,
                    selected: if count == 0 {
                        0
                    } else {
                        (selected + count - 1) % count
                    },
                });
                Outcome::effects(vec![])
            }
            _ => Outcome::quiet(),
        }
    }

    fn perform_filter(&mut self, intent: Intent) -> Outcome {
        // The same rule the overlay follows: a destination named from the
        // filter is the reader leaving it, so the query is given up rather than
        // swallowing the key. Both filters are given up at once — only the page
        // that is showing has one — because the destination leaves the page
        // either way.
        if intent.leaves_the_surface() {
            self.filtering = false;
            self.filter.clear();
            self.config_center.filtering = false;
            self.config_center.filter.clear();
            return self.perform_intent(intent);
        }
        if self.page == Page::Management {
            match intent {
                Intent::Back | Intent::CloseOverlay | Intent::ContextualCancel => {
                    self.config_center.filtering = false;
                    self.config_center.filter.clear();
                    return Outcome::effects(vec![]);
                }
                Intent::ConfirmOverlay => {
                    self.config_center.filtering = false;
                    return Outcome::effects(self.config_section_effects());
                }
                _ => return Outcome::quiet(),
            }
        }
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
        if confirm == Intent::ConfigEntryDelete {
            let Some((key, action)) = self.pending_config_action.take() else {
                return Outcome::quiet();
            };
            // The write itself, not the entry point: the question has been
            // answered, and asking it again is what the entry point would do.
            return self.exec_config_action(&key, action);
        }
        if confirm == Intent::SidebarRowActions {
            let Some((row_id, action)) = self.pending_sidebar_action.take() else {
                return Outcome::quiet();
            };
            // The same rule as the record above: the menu that offered the verb
            // has been closed, so what runs is the write rather than the menu.
            let Some(row) = self.sidebar_rows().into_iter().find(|row| row.id == row_id) else {
                return Outcome::quiet();
            };
            return match action {
                crate::sessions::SidebarRowAction::DeleteProject => {
                    Outcome::effects(vec![Effect::DeleteProject {
                        project_id: row.project_id,
                    }])
                }
                crate::sessions::SidebarRowAction::DeleteFolder => {
                    let Some(folder_id) = row.id.strip_prefix("folder:").map(str::to_string) else {
                        return Outcome::quiet();
                    };
                    let Some(view) = self.projection.sidebar_organization.as_ref() else {
                        return Outcome::quiet();
                    };
                    Outcome::effects(vec![Effect::MutateSidebarOrganization {
                        mutation: vibex_core::RemoteSidebarOrganizationMutation::DeleteFolder {
                            folder_id,
                        },
                        expected_revision: Some(view.revision),
                    }])
                }
                _ => Outcome::quiet(),
            };
        }
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
            Intent::UnarchiveSession => {
                let Some(session_id) = self.list_session_target() else {
                    return Outcome::quiet();
                };
                Outcome::effects(vec![Effect::UnarchiveSession { session_id }])
            }
            Intent::RevokeSelectedDevice => {
                let Some(device) = self.selected_paired_device() else {
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

    fn submit_prompt(
        &mut self,
        field: PromptField,
        value: String,
        title: String,
        target: Option<String>,
    ) -> Outcome {
        let trimmed = value.trim().to_string();
        match field {
            PromptField::NewFolderName | PromptField::RenameFolderName => {
                // The heading travelled with the prompt, so the name lands on
                // the folder the reader was looking at rather than on whichever
                // heading the list has since put under the cursor.
                let Some(target) = self.pending_folder_prompt.take() else {
                    return Outcome::quiet();
                };
                if trimmed.is_empty() {
                    self.toast(Toast::warning(
                        self.strings.sidebar_folder_name_empty().to_string(),
                    ));
                    return Outcome::quiet();
                }
                let Some(view) = self.projection.sidebar_organization.as_ref() else {
                    // No authority owns the arrangement, so there is no tree for
                    // a folder to be filed in: saying so is better than a row
                    // that appears and is gone on the next read.
                    self.toast(Toast::warning(
                        self.strings.sidebar_row_actions().to_string(),
                    ));
                    return Outcome::quiet();
                };
                let mutation = match target.folder_id {
                    Some(folder_id) => {
                        vibex_core::RemoteSidebarOrganizationMutation::RenameFolder {
                            folder_id,
                            name: trimmed,
                        }
                    }
                    None => vibex_core::RemoteSidebarOrganizationMutation::CreateFolder {
                        name: trimmed,
                        project_id: target.project_id,
                        workspace_id: target.workspace_id,
                        parent_folder_id: target.parent_folder_id,
                    },
                };
                Outcome::effects(vec![Effect::MutateSidebarOrganization {
                    mutation,
                    expected_revision: Some(view.revision),
                }])
            }
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
            PromptField::NewModelId => {
                let Some(profile_id) = target.clone() else {
                    return Outcome::quiet();
                };
                if trimmed.is_empty() {
                    self.toast(Toast::warning(
                        self.strings.config_form_error_model().to_string(),
                    ));
                    return Outcome::quiet();
                }
                let Some(profile) = self.provider(&profile_id).cloned() else {
                    return Outcome::quiet();
                };
                let mut models = profile.configured_models.clone();
                if !models.iter().any(|model| model.id == trimmed) {
                    models.push(vibex_core::ProviderConfiguredModel {
                        id: trimmed,
                        display_name: None,
                        enabled: true,
                        wire_api: None,
                        capabilities: vibex_core::ProviderModelCapabilities::default(),
                    });
                }
                let view = crate::config_center::ModelListView {
                    agent_id: profile.agent_id.as_str().to_string(),
                    profile_id,
                    title: profile.display_name.clone(),
                    selected: 0,
                    notice: None,
                };
                self.write_models(view, models)
            }
            PromptField::ProviderSecret => {
                // The profile the reader pressed on, named by the prompt itself
                // rather than read back from the cursor: the list may have been
                // re-read while they typed, and the answer belongs to the record
                // the key was asked for.
                let Some(profile_id) = target.as_deref().and_then(|id| parse_config_id(id).ok())
                else {
                    return Outcome::quiet();
                };
                // The drafted secret is dropped as soon as it is submitted.
                Outcome::effects(vec![Effect::WriteProviderSecret {
                    profile_id,
                    secret: value,
                }])
            }
            PromptField::GatewayTokenLimit => {
                let window = self
                    .management_data
                    .gateway
                    .as_ref()
                    .and_then(|snapshot| snapshot.settings.policy.token_limit)
                    .map(|limit| limit.window)
                    .unwrap_or(vibex_core::GatewayTokenWindow::Day);
                let digits = trimmed.replace(['_', ','], "");
                let limit = if digits.is_empty() {
                    None
                } else {
                    match digits.parse::<u64>() {
                        Ok(tokens) if tokens > 0 => {
                            Some(vibex_core::GatewayTokenLimit { window, tokens })
                        }
                        _ => {
                            self.toast(Toast::warning(
                                self.strings.config_form_error_tokens().to_string(),
                            ));
                            return Outcome::quiet();
                        }
                    }
                };
                self.begin_record_work(
                    "gateway".to_string(),
                    self.config_row_subject("gateway"),
                    crate::config_center::ConfigVerb::Save,
                    vec![Effect::SetGatewayPolicy {
                        token_limit: Some(limit),
                    }],
                )
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
                let Some(device) = self.selected_paired_device() else {
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
        // opened Settings while writing a new session lands back in that draft —
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
        // A turn can only be stopped through the connection. While it is down
        // the state on screen is the last one the runtime reported, and a
        // session that was running when the link dropped still reads as
        // running — so the interrupt would be dispatched into a dead seat,
        // never change anything, and spend every future press the same way.
        // `Ctrl+C` would then have nothing it could ever do, and the reader
        // would have no way out of a client whose connection is already gone.
        // The press falls through to the quit question instead, which is the
        // one thing left that a disconnected client can still carry out.
        if self.live.is_live() && self.is_turn_running() {
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

    /// Whether the page in front of the reader is a session's own page with a
    /// turn `Ctrl+C` can stop.
    ///
    /// Only the page that draws the transcript can stop the turn it shows. The
    /// composing page has no session of its own, and the session list, settings
    /// and every other page keep the session behind them selected only for
    /// navigation: a reader pressing `Ctrl+C` there is not asking to stop
    /// something the page is not showing. Reading the session behind those
    /// pages is what left them unable to leave while a session was running.
    fn is_turn_running(&self) -> bool {
        self.page_shows_transcript()
            && self.active_session().is_some_and(|session| {
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
        if self.page == Page::Management {
            return self.move_config_selection(delta);
        }
        if self.page == Page::Devices {
            return self.move_device_selection(delta);
        }
        if self.page == Page::Sessions {
            return self.move_session_selection(delta);
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
                .is_some_and(|block| block.is_hidden())
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

    /// Move the session list's cursor, one row above the first for the page's
    /// own action row.
    ///
    /// That line is not one of `sidebar_rows`, so it has no index the list can
    /// count to; it is [`crate::app::SESSION_ACTION_ROW`], a step above row 0,
    /// and the two arrows walk onto it the way they walk onto any row. Landing
    /// on it puts the keyboard on its first control, so `Enter` starts a session
    /// without a second press.
    fn move_session_selection(&mut self, delta: i64) -> Outcome {
        let rows = self.sidebar_rows();
        if self.session_cursor_is_actions() {
            // The action row is the end of the list: there is nothing above it,
            // and down steps back onto the first row — when there is one.
            if delta > 0 && !rows.is_empty() {
                self.set_selection(Scope::Sessions, 0);
            }
            return Outcome::effects(vec![]);
        }
        let next = self.selection_for(Scope::Sessions) as i64 + delta;
        if next < 0 {
            self.set_selection(Scope::Sessions, crate::app::SESSION_ACTION_ROW);
            // The row opens on its first control, so `Enter` does what the row
            // says it does without the reader having to step right first.
            self.session_row_action = self
                .session_action_row_intents()
                .first()
                .map(|intent| (crate::app::SESSION_ACTION_ROW, *intent));
            return Outcome::effects(vec![]);
        }
        let next = next.min(rows.len().saturating_sub(1) as i64) as usize;
        self.set_selection(Scope::Sessions, next);
        // Walking onto a folded branch opens it, so the rows under it are there
        // to walk on to. It only ever opens: the cursor also lands here through
        // a click, and a branch that folded as soon as the cursor moved would
        // take the child row off the list while the reader was reaching for it.
        let opened = self.open_child_branch_at(next);
        Outcome::effects(opened.into_iter().collect())
    }

    /// Move the keyboard across the buttons of the row the session cursor is on.
    ///
    /// `→` steps into the cluster at its first button and then walks right,
    /// stopping at the last; `←` steps in at the last and walks left, and from
    /// the first button it steps back out onto the row. The page's action row
    /// has two controls and no list to walk, so there the two keys toggle
    /// between them — which is what its own line promises.
    fn step_session_control(&mut self, delta: i64) -> Outcome {
        if self.page != Page::Sessions {
            return Outcome::quiet();
        }
        let cursor = self.selection_for(Scope::Sessions);
        let intents = if cursor == crate::app::SESSION_ACTION_ROW {
            self.session_action_row_intents()
        } else {
            self.sidebar_row_button_intents(cursor)
        };
        if intents.is_empty() {
            return Outcome::quiet();
        }
        let current = self
            .session_row_action
            .filter(|(row, _)| *row == cursor)
            .and_then(|(_, intent)| intents.iter().position(|candidate| *candidate == intent));
        if cursor == crate::app::SESSION_ACTION_ROW {
            let at = current.unwrap_or(0) as i64;
            let next = (at + delta).rem_euclid(intents.len() as i64) as usize;
            self.session_row_action = Some((cursor, intents[next]));
            return Outcome::effects(vec![]);
        }
        let next = match (current, delta.is_negative()) {
            // Left from the first button steps back out onto the row, which is
            // the way out of the cluster without leaving the row.
            (Some(0), true) => {
                self.session_row_action = None;
                return Outcome::effects(vec![]);
            }
            (None, true) => intents.len() - 1,
            (None, false) => 0,
            (Some(index), _) => (index as i64 + delta).clamp(0, intents.len() as i64 - 1) as usize,
        };
        self.session_row_action = Some((cursor, intents[next]));
        Outcome::effects(vec![])
    }

    /// Press the button the session list's keyboard cursor has stepped onto.
    ///
    /// `Enter` on the page's own action row starts a session even though the
    /// cursor may have been moved off its control: the row's first control is
    /// what the row is for, and its line says so. Anywhere else `None` means the
    /// cursor is only on the row, and the caller opens it instead.
    fn activate_session_control(&mut self) -> Option<Outcome> {
        let cursor = self.selection_for(Scope::Sessions);
        let focused = self
            .session_row_action
            .filter(|(row, _)| *row == cursor)
            .map(|(_, intent)| intent);
        let intents = if cursor == crate::app::SESSION_ACTION_ROW {
            self.session_action_row_intents()
        } else {
            self.sidebar_row_button_intents(cursor)
        };
        let intent = match focused {
            Some(intent) if intents.contains(&intent) => intent,
            _ if cursor == crate::app::SESSION_ACTION_ROW => Intent::NewSession,
            _ => return None,
        };
        // The press is the gesture: the row keeps its cursor, but not the
        // button, so a second `Enter` cannot run the same verb again on
        // whichever row a refresh or a deletion has since put under it.
        self.session_row_action = None;
        Some(self.perform(intent))
    }

    /// Whether the cursor is on the records rather than on the panes that
    /// choose which records they are.
    ///
    /// A verb that acts on "the row" is about the row the reader is on, so it
    /// needs the reader to be on one. The moves already answer for the pane
    /// they are in; this is the same rule for the verbs that change something.
    fn config_rows_focused(&self) -> bool {
        self.config_center.zone == crate::config_center::ConfigZone::Rows
    }

    /// Move inside the configuration centre, where the same two keys mean
    /// three things depending on the pane the reader is in.
    fn move_config_selection(&mut self, delta: i64) -> Outcome {
        use crate::config_center::ConfigZone;
        match self.config_center.zone {
            ConfigZone::Modules => {
                self.config_center.step_module(delta as isize);
                Outcome::effects(self.config_section_effects())
            }
            ConfigZone::Rail => {
                self.config_center.step_section(delta as isize);
                Outcome::effects(self.config_section_effects())
            }
            ConfigZone::Rows => {
                let rows = self.visible_config_rows();
                self.config_center.reconcile(&rows);
                let before = self.config_center.cursor(rows.len());
                self.config_center.step_cursor(&rows, delta as isize);
                if self.config_center.cursor(rows.len()) == before && !rows.is_empty() {
                    self.config_center.pin(rows.get(before));
                }
                Outcome::effects(vec![])
            }
        }
    }

    /// Scroll the detail pane, which belongs to the records zone.
    ///
    /// It has no cursor of its own — it describes the row the cursor is on —
    /// so the page keys move it while the record keys move the cursor.
    fn scroll_config_detail(&mut self, delta: i64) -> Outcome {
        let row = self.config_center.detail_scroll as i64 + delta;
        self.config_center.detail_scroll = row.max(0) as u16;
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
        // Reading is the act that clears the mark, so the authority is told
        // before `open_session` clears this client's copy — the effect is read
        // from the arrangement's unread set, which is what the authority
        // published, not from the local set the open is about to empty.
        let mark_read = self.sidebar_mark_read_effect(&session_id);
        self.open_session(session_id.clone());
        self.sync_transcript();
        // The open session's own page is what names its children and the path
        // back to the session that owns it, so it is read as the page opens
        // rather than when the list is next refreshed.
        let child_read = self.load_child_page(Some(session_id.clone()), false);
        let mut effects = vec![Effect::OpenSession { session_id, ticket }];
        if let Some(mark_read) = mark_read {
            effects.push(mark_read);
        }
        if self.runtime_options.is_none() && self.runtime_catalog_available() {
            effects.push(self.read_runtime_catalogue());
        }
        effects.extend(child_read);
        effects.extend(self.child_tree_effects());
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
    /// Point the composing page at the directory of the heading the reader is
    /// on, when the session list is what has the cursor.
    ///
    /// A heading is a project or a folder, and both name somewhere to work: the
    /// page is where a session is created, and this is the one thing it needs
    /// to open the new session in the right checkout. A cursor on a session
    /// row says nothing about where the *next* session goes, so the page keeps
    /// whatever the reader last chose.
    fn aim_new_session_at_selected_heading(&mut self) {
        if self.page != Page::Sessions {
            return;
        }
        let index = self.selection_for(Scope::Sessions);
        let Some(row) = self.sidebar_rows().get(index).cloned() else {
            return;
        };
        if row.session_id.is_some() {
            return;
        }
        if let Some(root) = self.project_workspace_root(&row.project_id, &row.workspace_id) {
            self.workspace_path = Some(root);
        }
    }

    fn begin_new_session(&mut self) -> Outcome {
        self.navigate_to(Page::NewSession);
        if !self.new_draft_has_content_or_choices() && !self.failed_creations.is_empty() {
            self.restore_failed_creation();
        }
        self.focus = Focus::Composer;
        let mut effects = vec![Effect::ListWorkspaces];
        // The page names the Agent the session will be created with and offers
        // its run options, so the catalogue is read on the way in rather than
        // only when the switcher is opened — the page says it is loading for as
        // short a time as the read allows, and never spends it saying the
        // runtime is unavailable. It is a read; a failure leaves the page naming
        // the Agent it can.
        if self.runtime_options.is_none() && self.runtime_catalog_available() {
            effects.push(self.read_runtime_catalogue());
        }
        // The list of existing sessions is read once on the way in even though
        // this page draws none of it: the client's own background work — an
        // auto-continue countdown, an unread mark, whether a session's turn is
        // still running — is derived from it, and none of that may be wrong
        // because the reader happened to start on the prompt. Once it is read
        // the state is kept, so `n` from the list does not read it again.
        if self.agent.state.sessions.value.is_none()
            && let Some(effect) = self.refresh_session_list()
        {
            effects.push(effect);
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
                Outcome::effects(vec![self.read_runtime_catalogue()])
            } else {
                Outcome::quiet()
            };
        }
        let outgoing = self.composer.take_outgoing();
        self.completion = None;
        // The send time the page armed, resolved now that the message is really
        // being submitted: a countdown starts here, and a deadline passed while
        // the reader was still typing is refused with the draft put back rather
        // than turned into an immediate send.
        let scheduled_at_ms = match self.take_draft_schedule(vibex_core::unix_timestamp_ms()) {
            Ok(at_ms) => at_ms,
            Err(error) => {
                self.composer.set_draft(outgoing.text, outgoing.images);
                self.toast(Toast::warning(error.message(self.strings).to_string()));
                return Outcome::quiet();
            }
        };
        // Read through the page's own answer (`new_session_workspace`), so what
        // the page names is what the creation carries — including the directory
        // the client was started in when nothing has chosen one.
        let workspace_root = self.new_session_workspace();
        let request_id = self.new_draft_id.clone();
        // Select the reserved identity synchronously. There is no fetch until
        // the authority acknowledges creation, and no old timeline survives.
        self.agent.select_pending_session(request_id.clone());
        self.enter_creating_session(request_id.clone());
        // A session the reader gave a send time to is created at once and its
        // message waits in the queue, which is what puts the row in the session
        // list — counting down — before anything has been sent. An ordinary
        // creation sends the moment the authority acknowledges it, so its
        // message is projected here and now.
        let send_id = match scheduled_at_ms {
            Some(at_ms) => {
                self.enqueue_scheduled(
                    request_id.clone(),
                    outgoing.text.clone(),
                    outgoing.images.clone(),
                    Some(at_ms),
                );
                0
            }
            None => {
                let projected = self.wire_attachments(&outgoing.images);
                self.mark_send_dispatched(Some(&request_id), outgoing.text.clone(), projected)
            }
        };
        // The row is named by the message from the moment the session is asked
        // for. The authority has no name for it until that message reaches a
        // turn — which waits on the Agent being materialized — and the
        // placeholder it reports until then is not what the reader wrote.
        // Pictures travel too: a creation that carries only a screenshot is
        // named by its label, not left blank.
        self.note_pending_session_title(&request_id, &outgoing.text, &outgoing.images);
        self.pending_creations.insert(
            request_id.clone(),
            PendingCreation {
                outgoing,
                runtime: runtime.clone(),
                workspace_root: workspace_root.clone(),
                send_id,
                scheduled_at_ms,
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
            target: None,
        });
        Outcome::effects(vec![])
    }

    /// Open the highlighted pairing row, or close it.
    ///
    /// Opening asks the runtime for what the panel draws: an offer for the QR
    /// and the link, a window for the local network. A window that has run out
    /// is a row the reader presses to look again rather than to close — the
    /// panel it would close already says the window is gone.
    fn activate_device_pairing(&mut self) -> Outcome {
        use crate::device_center::{Activation, DeviceTab, Opening, PairMethod};
        if self.page != Page::Devices || self.device_center.tab != DeviceTab::Pair {
            return Outcome::quiet();
        }
        let Some(method) = self.selected_pair_method() else {
            return Outcome::quiet();
        };
        let operation = match method {
            PairMethod::LocalNetwork => BackendOperation::DeviceLanPairing,
            PairMethod::QrCode | PairMethod::CodeOrLink => BackendOperation::DevicePairing,
        };
        let availability = self.availability(operation);
        if availability != Availability::Available {
            let message = match availability {
                Availability::RequiresPermission => self.strings.permission_required_for(),
                _ => self.strings.toast_action_unavailable(),
            };
            self.toast(Toast::warning(message.to_string()));
            return Outcome::quiet();
        }
        // A panel with nothing live behind it is a panel the press reopens:
        // the window closed, the code expired, or the last answer was a
        // failure. The row stays open and asks the runtime again, which is what
        // the reader meant by pressing it a second time.
        let spent = self.device_center.open == Some(method)
            && self
                .device_center
                .pairing
                .spent(method, vibex_core::unix_timestamp_ms());
        let activation = if spent {
            self.device_center.open = Some(method);
            Activation::Opened(method.opening())
        } else {
            self.device_center.toggle(method)
        };
        match activation {
            Activation::Opened(Opening::Offer) => {
                self.device_center.pairing.busy = Some(method);
                self.device_center.pairing.error = None;
                Outcome::effects(vec![Effect::CreatePairingOffer])
            }
            Activation::Opened(Opening::Window) => self.start_lan_pairing(),
            Activation::Closed { release } => {
                Outcome::effects(self.device_release_effects(release))
            }
        }
    }

    /// Move the offer's route, which is what the code and the link carry.
    ///
    /// A local choice: nothing is asked of the runtime, because the transport is
    /// the client's to name and the fragment behind it does not change. The
    /// press does not dirty the frame when there is only one route to be on.
    fn cycle_pairing_route(&mut self, delta: i64) -> Outcome {
        let moved = self
            .device_center
            .pairing
            .offer
            .as_mut()
            .is_some_and(|offer| offer.cycle_route(delta));
        Outcome {
            effects: vec![],
            dirty: moved,
        }
    }

    /// Ask the runtime to advertise itself on the local network.
    fn start_lan_pairing(&mut self) -> Outcome {
        use crate::device_center::PairMethod;
        // The old window is given up rather than drawn until the answer lands:
        // it belonged to a window this one replaces.
        self.device_center.pairing.window = None;
        self.device_center.pairing.busy = Some(PairMethod::LocalNetwork);
        self.device_center.pairing.error = None;
        Outcome::effects(vec![Effect::StartLanPairing {
            // The same level the TUI has always offered: full control, which is
            // what a device paired from this client is granted. Choosing a
            // narrower level is a decision the page does not yet ask for.
            permission_level: vibex_core::RemoteDevicePermissionLevel::FullControl,
            ttl_ms: vibex_core::DEFAULT_PAIRING_TTL_MS,
        }])
    }

    /// Approve or refuse the device waiting on the open window.
    fn decide_lan_pairing(&mut self, approve: bool) -> Outcome {
        use crate::device_center::PairMethod;
        if !self.device_center.lan_panel_open() {
            return Outcome::quiet();
        }
        let Some(request_id) = self
            .device_center
            .pairing
            .pending()
            .map(|request| request.request_id.clone())
        else {
            return Outcome::quiet();
        };
        self.device_center.pairing.busy = Some(PairMethod::LocalNetwork);
        self.device_center.pairing.error = None;
        Outcome::effects(vec![Effect::DecideLanPairing {
            request_id,
            approve,
        }])
    }

    /// Close the open pairing panel.
    ///
    /// A window panel takes its advertisement down and an offer panel takes its
    /// code down: the panel is the only place either is drawn, so a runtime left
    /// discoverable — or a code left claimable — behind a closed panel would let
    /// in a device the reader never saw.
    fn close_device_panel(&mut self) -> Outcome {
        let Some(method) = self.device_center.open.take() else {
            return Outcome::quiet();
        };
        let release = self.device_center.pairing.release(method);
        Outcome::effects(self.device_release_effects(release))
    }

    /// Switch halves of the devices page.
    fn step_device_tab(&mut self, delta: i64) -> Outcome {
        if self.page != Page::Devices {
            return Outcome::quiet();
        }
        let tabs = self.device_tabs();
        let release = self.device_center.step_tab(delta, &tabs);
        let mut effects = self.device_release_effects(release);
        if self.device_center.tab == crate::device_center::DeviceTab::Publishing
            && self.device_publications_available()
        {
            // The half is a read, and arriving on it is the moment to make it:
            // a status page that waited for its own poll would show the previous
            // visit's answer first.
            effects.push(Effect::PairingPublications);
        }
        Outcome::effects(effects)
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
            self.offered_questions
                .insert(format!("permission:{}", self.approvals()[0].request_id));
            self.overlay = Some(Overlay::Approval { selected: 0 });
        }
    }

    /// Open the form on the question that has waited longest.
    fn open_elicitation(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        let Some(surface) = self.elicitations().into_iter().next() else {
            return;
        };
        self.show_elicitation(surface.request.id.as_str());
    }

    /// Open the form on one request, and remember that it has been offered.
    fn show_elicitation(&mut self, request_id: &str) {
        self.offered_questions
            .insert(format!("elicitation:{request_id}"));
        self.overlay = Some(Overlay::Elicitation {
            request: request_id.to_string(),
            field: 0,
            choice: 0,
        });
    }

    /// Put the Agent's blocking question in front of the reader, once.
    ///
    /// The desktop draws the approval card and the elicitation form inline in
    /// the timeline, so both are visible without a gesture. The transcript here
    /// is a block list with no controls in it, so the same card is a modal that
    /// opens itself the moment a request appears on the page the reader owns —
    /// the Composer's keyboard included: a question that waited for the reader
    /// to go looking for it is a question the Agent stays blocked on.
    ///
    /// Nothing of the reader's is lost to it. A draft being written stays in
    /// the Composer exactly as it was, and `Esc` puts the card down and hands
    /// the keyboard straight back to it. A question already offered — or put
    /// down — is left alone; `Enter` on its transcript row asks for it again.
    ///
    /// Returns whether the frame changed.
    pub fn sync_pending_question(&mut self) -> bool {
        // A request answered from another client takes its card with it. The
        // form cannot answer a question nobody is waiting on, and a modal with
        // nothing left to draw would swallow every key in front of it.
        match &self.overlay {
            Some(Overlay::Elicitation { .. }) if self.elicitation_form().is_none() => {
                self.overlay = None;
                return true;
            }
            Some(Overlay::Approval { .. }) if self.approvals().is_empty() => {
                self.overlay = None;
                return true;
            }
            _ => {}
        }
        if self.overlay.is_some() || !self.page_owns_session() {
            return false;
        }
        // Both projections are read once: they walk the timeline, and this runs
        // on the loop's own clock rather than on an event.
        let elicitations = self.elicitations();
        let approvals = self.approvals();
        // Whatever has been answered or withdrawn is forgotten, so a request id
        // the runtime reuses — or a card that comes back from the dead — is
        // offered again rather than silently swallowed.
        let live = elicitations
            .iter()
            .map(|surface| format!("elicitation:{}", surface.request.id))
            .chain(
                approvals
                    .iter()
                    .map(|approval| format!("permission:{}", approval.request_id)),
            )
            .collect::<std::collections::BTreeSet<_>>();
        self.offered_questions.retain(|key| live.contains(key));
        // A question the reader has already seen — or put down with `Esc` — is
        // not offered again; asking for it is the transcript row's job. The
        // first *fresh* one is the card that opens, which is how a second
        // question takes the place of one whose answer is still in flight.
        let fresh_elicitation = elicitations
            .iter()
            .map(|surface| surface.request.id.to_string())
            .find(|id| {
                !self
                    .offered_questions
                    .contains(&format!("elicitation:{id}"))
            });
        let fresh_approval = approvals
            .first()
            .map(|approval| format!("permission:{}", approval.request_id))
            .filter(|key| !self.offered_questions.contains(key));
        let before = self.overlay.clone();
        if let Some(request) = fresh_elicitation {
            self.show_elicitation(&request);
        } else if fresh_approval.is_some() {
            self.open_approval();
        }
        self.overlay != before
    }

    /// Whether the transcript cursor is on a question the Agent is blocked on.
    ///
    /// The transcript draws one row for a pending request, and that row is the
    /// only place the reader can see the question without the card being open.
    /// The kind matters: the row that answers an approval is not the one that
    /// answers a question, and a reader who put the cursor on one is asking for
    /// that one.
    fn selected_block_is_a_pending_question(&self, elicitation: bool) -> bool {
        use vibex_desktop_model::TimelineRowKind;
        let index = self.selection_for(Scope::Agent);
        self.transcript.block(index).is_some_and(|block| {
            block.pending_permission
                && if elicitation {
                    block.kind == TimelineRowKind::ElicitationRequest
                } else {
                    block.kind == TimelineRowKind::PermissionRequest
                }
        })
    }

    /// The question the open form is filling in, while it is still pending.
    fn elicitation_form_request(&self) -> Option<vibex_core::ElicitationRequest> {
        self.elicitation_form().map(|form| form.request)
    }

    /// The draft the open form is writing into.
    fn elicitation_form_draft(&self, request_id: &str) -> crate::reduce::ElicitationDraft {
        self.elicitation_drafts
            .get(request_id)
            .cloned()
            .unwrap_or_default()
    }

    /// How many options the field at `field` offers.
    ///
    /// A boolean is a switch rather than a list — `Space` flips it — so it
    /// counts as no choices, which is what lets the arrows walk past it.
    fn elicitation_choice_count(request: &vibex_core::ElicitationRequest, field: usize) -> usize {
        use vibex_core::ElicitationFieldKind;
        match request.fields.get(field).map(|field| &field.kind) {
            Some(ElicitationFieldKind::Text { options, .. }) => options.len(),
            Some(ElicitationFieldKind::MultiSelect { options, .. }) => options.len(),
            // A boolean is a two-row list rather than a switch: the card is
            // walked with the same keys whatever the question asks, and `Yes`
            // and `No` are the two answers it has.
            Some(ElicitationFieldKind::Boolean { .. }) => 2,
            _ => 0,
        }
    }

    /// The option values a field offers, in the order the Agent sent them.
    fn elicitation_choice_values(
        request: &vibex_core::ElicitationRequest,
        field: usize,
    ) -> Vec<String> {
        use vibex_core::ElicitationFieldKind;
        match request.fields.get(field).map(|field| &field.kind) {
            Some(ElicitationFieldKind::Text { options, .. })
            | Some(ElicitationFieldKind::MultiSelect { options, .. }) => {
                options.iter().map(|option| option.value.clone()).collect()
            }
            Some(ElicitationFieldKind::Boolean { .. }) => {
                vec!["true".to_string(), "false".to_string()]
            }
            _ => Vec::new(),
        }
    }

    /// Where the cursor lands when the reader walks into a field.
    ///
    /// A single-select opens on the option already chosen and a multi-select on
    /// the first one already ticked, so `Space` on arrival changes what the
    /// reader was looking at rather than something further down the list.
    fn elicitation_entry_choice(
        request: &vibex_core::ElicitationRequest,
        field: usize,
        draft: &crate::reduce::ElicitationDraft,
    ) -> usize {
        let values = Self::elicitation_choice_values(request, field);
        let Some(definition) = request.fields.get(field) else {
            return 0;
        };
        let chosen = match &definition.kind {
            vibex_core::ElicitationFieldKind::MultiSelect { .. } => {
                draft.multi(&definition.id).into_iter().next()
            }
            vibex_core::ElicitationFieldKind::Boolean { default } => draft
                .boolean(&definition.id)
                .or(*default)
                .map(|value| value.to_string()),
            _ => draft.text(&definition.id).map(str::to_string),
        };
        chosen
            .and_then(|value| values.iter().position(|option| *option == value))
            .unwrap_or(0)
    }

    /// Whether the active field is typed into rather than chosen from.
    fn elicitation_field_takes_text(
        request: &vibex_core::ElicitationRequest,
        field: usize,
    ) -> bool {
        use vibex_core::ElicitationFieldKind;
        match request.fields.get(field).map(|field| &field.kind) {
            // A text field constrained to options is a list, not a prompt: the
            // request's own validation refuses an answer outside them.
            Some(ElicitationFieldKind::Text { options, .. }) => options.is_empty(),
            Some(ElicitationFieldKind::Number { .. } | ElicitationFieldKind::Integer { .. }) => {
                true
            }
            _ => false,
        }
    }

    /// Whether the active field offers anything to move a cursor over.
    fn elicitation_field_is_a_choice(
        request: &vibex_core::ElicitationRequest,
        field: usize,
    ) -> bool {
        Self::elicitation_choice_count(request, field) > 0
    }

    /// The open form's question, cursor and draft, resolved together.
    ///
    /// One lookup rather than three, so a form whose request was answered
    /// elsewhere cannot be read as "field 2 of some other question".
    fn elicitation_cursor(
        &self,
    ) -> Option<(
        String,
        vibex_core::ElicitationRequest,
        usize,
        usize,
        crate::reduce::ElicitationDraft,
    )> {
        let form = self.elicitation_form()?;
        let id = form.request.id.to_string();
        let draft = self.elicitation_form_draft(&id);
        Some((id, form.request, form.field, form.choice, draft))
    }

    /// Move the option cursor inside the open question.
    ///
    /// `↑`/`↓` are the *select* keys here, not the way between questions: a
    /// card whose questions are tabs moves between them with `Tab` and `←`/`→`,
    /// and an arrow that also changed the tab would make the option list
    /// impossible to walk past its first entry. It wraps, so the last option
    /// leads back to the first rather than dead-ending.
    fn step_elicitation_cursor(&mut self, delta: i64) -> Outcome {
        let Some((id, request, field, choice, _)) = self.elicitation_cursor() else {
            return Outcome::quiet();
        };
        let choices = Self::elicitation_choice_count(&request, field);
        if choices == 0 {
            return Outcome::quiet();
        }
        let choice = if delta > 0 {
            (choice + 1) % choices
        } else {
            (choice + choices - 1) % choices
        };
        self.set_elicitation_cursor(id, field, choice)
    }

    /// Move between the card's tabs: the questions, then Confirm.
    ///
    /// The option cursor lands on whatever that question already answers with,
    /// so `Enter` on arrival confirms what the reader can see rather than the
    /// first row of a list they have not read.
    fn step_elicitation_field(&mut self, delta: i64) -> Outcome {
        let Some((id, request, field, _, draft)) = self.elicitation_cursor() else {
            return Outcome::quiet();
        };
        let tabs = request.fields.len() + 1;
        let next = if delta > 0 {
            (field + 1) % tabs
        } else {
            (field + tabs - 1) % tabs
        };
        let choice = Self::elicitation_entry_choice(&request, next, &draft);
        self.set_elicitation_cursor(id, next, choice)
    }

    /// Take the `Enter` of the open tab: confirm this answer, or submit.
    ///
    /// One key for the whole card. On a question it records the option under
    /// the cursor — the ticked set is already recorded, and an answer that is
    /// typed is already in the draft — and moves on to the next tab. On the
    /// Confirm tab it sends the answers back. A required question that has no
    /// answer stops the walk where it is and says so, because moving on would
    /// only hide the field the Agent is waiting for.
    fn confirm_elicitation(&mut self) -> Outcome {
        let Some(form) = self.elicitation_form() else {
            return Outcome::quiet();
        };
        if form.confirming() {
            return self.submit_elicitation();
        }
        let Some(definition) = form.question().cloned() else {
            return Outcome::quiet();
        };
        // A row under the cursor *is* an answer for the questions that are
        // chosen from: `Enter` is how the reader says "this one", so it is
        // recorded before the question is judged. A list of ticks is already
        // recorded — the key that ticks them is `Space` — and an answer that is
        // typed is already in the draft.
        match &definition.kind {
            vibex_core::ElicitationFieldKind::Text { options, .. } if !options.is_empty() => {
                self.toggle_elicitation_choice();
            }
            vibex_core::ElicitationFieldKind::Boolean { .. } => {
                self.toggle_elicitation_choice();
            }
            _ => {}
        }
        let draft = self.elicitation_form_draft(form.request.id.as_str());
        if draft.answer_for(&definition.id, &definition).is_none() && definition.required {
            self.toast(Toast::warning(
                self.strings.elicitation_answer_required().to_string(),
            ));
            return Outcome::quiet();
        }
        self.step_elicitation_field(1)
    }

    fn set_elicitation_cursor(&mut self, request: String, field: usize, choice: usize) -> Outcome {
        self.overlay = Some(Overlay::Elicitation {
            request,
            field,
            choice,
        });
        Outcome::effects(vec![])
    }

    /// Take the cursor's option: choose it outright, or tick it in a list.
    fn toggle_elicitation_choice(&mut self) -> Outcome {
        use vibex_core::ElicitationFieldKind;
        let Some((id, request, field, choice, _)) = self.elicitation_cursor() else {
            return Outcome::quiet();
        };
        let Some(definition) = request.fields.get(field) else {
            return Outcome::quiet();
        };
        let draft = self.elicitation_drafts.entry(id).or_default();
        match &definition.kind {
            ElicitationFieldKind::Text { options, .. } => {
                let Some(option) = options.get(choice) else {
                    return Outcome::quiet();
                };
                draft.set_text(definition.id.clone(), option.value.clone());
            }
            ElicitationFieldKind::MultiSelect { options, .. } => {
                let Some(option) = options.get(choice) else {
                    return Outcome::quiet();
                };
                draft.toggle_multi(definition.id.clone(), option.value.clone());
            }
            // `Yes` and `No` are the field's two rows, so the key takes the
            // row under the cursor rather than flipping whatever was there.
            ElicitationFieldKind::Boolean { .. } => {
                draft.set_boolean(definition.id.clone(), choice == 0);
            }
            _ => return Outcome::quiet(),
        }
        Outcome::effects(vec![])
    }

    /// Type into the active field.
    ///
    /// A terminal form field is edited at its end, exactly like the
    /// configuration form and the schedule editor: the value is one line, and
    /// the caret is wherever the last character put it.
    pub fn elicitation_type_char(&mut self, character: char) {
        let Some((id, request, field, _, _)) = self.elicitation_cursor() else {
            return;
        };
        if !Self::elicitation_field_takes_text(&request, field) {
            return;
        }
        let Some(definition) = request.fields.get(field) else {
            return;
        };
        let draft = self.elicitation_drafts.entry(id).or_default();
        let mut value = draft.text(&definition.id).unwrap_or_default().to_string();
        value.push(character);
        draft.set_text(definition.id.clone(), value);
    }

    pub fn elicitation_backspace(&mut self) {
        let Some((id, request, field, _, _)) = self.elicitation_cursor() else {
            return;
        };
        if !Self::elicitation_field_takes_text(&request, field) {
            return;
        }
        let Some(definition) = request.fields.get(field) else {
            return;
        };
        let draft = self.elicitation_drafts.entry(id).or_default();
        let mut value = draft.text(&definition.id).unwrap_or_default().to_string();
        value.pop();
        draft.set_text(definition.id.clone(), value);
    }

    /// Give the active field up, the way `Delete` clears every other one-line
    /// field in this client.
    pub fn elicitation_clear_field(&mut self) {
        let Some((id, request, field, _, _)) = self.elicitation_cursor() else {
            return;
        };
        let Some(definition) = request.fields.get(field) else {
            return;
        };
        let draft = self.elicitation_drafts.entry(id).or_default();
        draft.set_text(definition.id.clone(), String::new());
    }

    /// Whether a printable character belongs to the active field rather than to
    /// the binding table. The run loop asks before the table sees the key, so a
    /// question and a shortcut cannot both answer the same letter.
    pub fn elicitation_field_takes_keys(&self) -> bool {
        self.elicitation_form().is_some_and(|form| {
            Self::elicitation_field_takes_text(&form.request, form.field)
                || Self::elicitation_field_is_a_choice(&form.request, form.field)
        })
    }

    /// Whether the cursor's field offers options, so a digit picks one instead
    /// of being typed.
    pub fn elicitation_field_choices(&self) -> usize {
        self.elicitation_form()
            .map(|form| Self::elicitation_choice_count(&form.request, form.field))
            .unwrap_or(0)
    }

    /// Whether the cursor's field is typed into.
    pub fn elicitation_field_takes_text_keys(&self) -> bool {
        self.elicitation_form()
            .is_some_and(|form| Self::elicitation_field_takes_text(&form.request, form.field))
    }

    /// What one printable key means to the open elicitation form.
    ///
    /// The rule is the reducer's, not the run loop's, so it can be tested
    /// without a terminal: a character is an answer while the cursor is in a
    /// field that takes one, an option while the cursor is on a list of them,
    /// and the binding table's business otherwise — which is what lets `j` walk
    /// a list and `d` refuse the question from every field that is not typing.
    pub fn elicitation_key(&self, character: char) -> ElicitationKey {
        if self.elicitation_field_takes_text_keys() {
            return ElicitationKey::Text;
        }
        let choices = self.elicitation_field_choices();
        if choices == 0 {
            return ElicitationKey::Elsewhere;
        }
        match character.to_digit(10) {
            Some(digit) if digit >= 1 && (digit as usize) <= choices => {
                ElicitationKey::Option(digit as usize - 1)
            }
            _ => ElicitationKey::Elsewhere,
        }
    }

    /// Choose the option at `index` in the cursor's field.
    pub fn select_elicitation_option(&mut self, index: usize) -> Outcome {
        if let Some(Overlay::Elicitation { choice, .. }) = self.overlay.as_mut() {
            *choice = index;
        }
        self.toggle_elicitation_choice()
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

    /// Answer the question the form is on, with what has been filled in.
    ///
    /// The card only closes once the answers pass the request's own validation:
    /// a required field the reader skipped is a reason to stay where they are,
    /// not to drop the form and leave the Agent waiting on nothing.
    fn submit_elicitation(&mut self) -> Outcome {
        let Some(request) = self.elicitation_form_request() else {
            return Outcome::quiet();
        };
        let draft = self.elicitation_form_draft(request.id.as_str());
        self.resolve_elicitation_request(request, ElicitationResolutionAction::Accept, &draft)
    }

    /// Refuse the question the form is on.
    fn decline_elicitation(&mut self) -> Outcome {
        let Some(request) = self.elicitation_form_request() else {
            return Outcome::quiet();
        };
        self.resolve_elicitation_request(
            request,
            ElicitationResolutionAction::Decline,
            &crate::reduce::ElicitationDraft::default(),
        )
    }

    fn resolve_elicitation_request(
        &mut self,
        request: vibex_core::ElicitationRequest,
        action: ElicitationResolutionAction,
        draft: &crate::reduce::ElicitationDraft,
    ) -> Outcome {
        let Some(session_id) = self.selected_session_id().cloned() else {
            return Outcome::quiet();
        };
        let mut answers = std::collections::BTreeMap::new();
        if action == ElicitationResolutionAction::Accept {
            for definition in &request.fields {
                if let Some(answer) = draft.answer_for(&definition.id, definition) {
                    answers.insert(definition.id.clone(), answer);
                }
            }
        }
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: session_id.clone(),
            action,
            answers,
            responder_device_id: None,
            resolved_at_ms: vibex_core::unix_timestamp_ms(),
        };
        if let Err(error) = request.validate_resolution(&resolution) {
            self.toast(Toast::danger(error.message));
            return Outcome::quiet();
        }
        self.mark_pending(format!("elicitation:{}", request.id.as_str()));
        self.overlay = None;
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
        // A schedule is resolved at the moment the message is submitted: a
        // countdown starts here, and an absolute deadline the reader typed a
        // while ago is checked again rather than trusted. A value the clock has
        // walked past is refused with the draft put back, because sending it now
        // is the one thing a schedule promised not to do.
        let scheduled_at_ms = match self.take_draft_schedule(vibex_core::unix_timestamp_ms()) {
            Ok(at_ms) => at_ms,
            Err(error) => {
                self.composer.set_draft(text, images);
                self.toast(Toast::warning(error.message(self.strings).to_string()));
                return Outcome::quiet();
            }
        };
        // A message written while a turn is running is held rather than sent:
        // the runtime would have to interleave it with work already in flight.
        // A message with a send time is held for a different reason — the clock
        // is what it waits for — but it waits in the same queue, and the same
        // send path releases it.
        if scheduled_at_ms.is_some()
            || self.turn_reads_running()
            || self.session_is_uncreated(&session_id)
        {
            self.enqueue_scheduled(session_id.clone(), text, images, scheduled_at_ms);
            let message = match scheduled_at_ms {
                Some(at_ms) => format!(
                    "{} · {}",
                    self.strings.schedule_set(),
                    crate::schedule::format_send_time(at_ms)
                ),
                None => self.strings.queue_held().to_string(),
            };
            self.toast(Toast::info(message));
            return Outcome::effects(vec![]);
        }
        self.history_log.push(text.clone());
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
        self.history_log.push(text.clone());
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
        // The walk is over the rows on screen, not over the whole answer: a
        // candidate the query has filtered out is one arrow keys must step over,
        // or the reader watches the highlight disappear into rows nobody drew.
        let matches = menu.visible();
        if matches.is_empty() {
            return;
        }
        let position = matches
            .iter()
            .position(|index| *index == menu.selected)
            .unwrap_or(0);
        let next = (position as i64 + delta).rem_euclid(matches.len() as i64) as usize;
        menu.selected = matches[next];
    }

    /// `Left`/`Right` on an open `/` or `@` drawer moves between its panels.
    ///
    /// Returns whether the press changed the panel. At the edge in that
    /// direction the key is *not* consumed: the caret keeps its usual movement
    /// inside the `/query` being edited, which is also what the desktop
    /// composer answers the same popup with.
    pub fn step_completion_tab(&mut self, forward: bool) -> bool {
        self.completion
            .as_mut()
            .is_some_and(|menu| menu.step_tab(forward))
    }

    /// Switch to `tab` from a click on the drawer's header.
    ///
    /// A single-panel trigger has no header, so a click on a panel name it does
    /// not draw changes nothing.
    pub fn select_completion_tab(&mut self, tab: CompletionTab) -> bool {
        self.completion
            .as_mut()
            .is_some_and(|menu| menu.select_tab(tab))
    }

    fn accept_completion(&mut self) -> Outcome {
        let Some(menu) = self.completion.clone() else {
            return Outcome::quiet();
        };
        // Only a row the drawer is offering is a row `Tab` or `Enter` may take:
        // an answer for a query the reader has since typed past can leave the
        // selection on a candidate their text no longer matches, and inserting
        // it would put a command they never chose into the draft.
        let Some(item) = menu.selected_item() else {
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
                match self.completion.as_mut() {
                    Some(menu) if menu.trigger == trigger && menu.start == start => {
                        menu.reseat(query.clone());
                    }
                    _ => {
                        self.completion = Some(CompletionMenu {
                            trigger,
                            start,
                            query: query.clone(),
                            lists: CompletionLists::default(),
                            // The panels follow the trigger, so the drawer opens
                            // on the trigger's own first panel whatever the
                            // previous one was showing.
                            tab: CompletionTab::tabs(trigger)
                                .first()
                                .copied()
                                .unwrap_or_default(),
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

    /// Ask for the runtime catalogue, remembering that it is on its way.
    ///
    /// Every request goes through here so no path can ask without saying so:
    /// the composing page names the Agent the session will be created with, and
    /// a read that has not answered yet is not a catalogue that came back
    /// empty. Until the answer lands — either way — the page says it is
    /// loading rather than that the runtime is unavailable.
    fn read_runtime_catalogue(&mut self) -> Effect {
        self.runtime_options_reading = true;
        Effect::ListRuntimeOptions
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
            return Outcome::effects(vec![self.read_runtime_catalogue()]);
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
                    Some(RuntimePickerRow::Entry {
                        index, remembered, ..
                    }) => self.choose_runtime_entry(index, remembered),
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
            target: None,
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

    /// Choose one catalogue entry, and say whether how it runs is still a
    /// question.
    ///
    /// Choosing an Agent and saying how it runs are one errand: sending the
    /// reader back to the catalogue — or out of the picker and in again — just
    /// to reach the run options costs them a second visit to a surface they are
    /// already standing in. A *remembered* row is the whole answer already: it
    /// is one of the reader's own recent choices, so `Enter` takes the run
    /// options it carries and closes rather than asking them to read back what
    /// they asked for. `Tab` is what still opens them, for the reader who wants
    /// to change one. An entry with nothing to tune closes the picker too,
    /// because there is nothing left to ask.
    fn choose_runtime_entry(&mut self, index: usize, remembered: bool) -> Outcome {
        let Some(selection) = self.selection_for_catalog_entry(index) else {
            return Outcome::quiet();
        };
        self.runtime_picker.working = Some(selection.clone());
        // The group the reader just moved onto is the one that stays open: the
        // catalogue behind the run options answers "what am I on" the same way
        // it did when it opened.
        self.fold_runtime_picker_to_current();
        let mut outcome = self.apply_picker_selection(selection);
        if remembered || self.picker_run_options().is_empty() {
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
    /// it does what `Enter` does: the row names one of the reader's own recent
    /// choices, so the choice is applied — run options and all — and the picker
    /// closes.
    ///
    /// Answers `None` when the digit is not one of them, which is what lets the
    /// caller fall through to the binding table: a key the reviewer did not
    /// mean as a shortcut must not be swallowed by one.
    pub fn quick_pick_runtime(&mut self, digit: char) -> Option<Outcome> {
        let position = digit.to_digit(10)? as usize;
        if position == 0 {
            return None;
        }
        // The row carries both facts the choice needs, so they are read off it
        // together rather than one of them being assumed from the other.
        let (index, remembered) = self
            .runtime_picker_rows()
            .iter()
            .find_map(|row| match row {
                RuntimePickerRow::Entry {
                    index,
                    quick: Some(quick),
                    remembered,
                } if *quick == digit => Some((*index, *remembered)),
                _ => None,
            })?;
        Some(self.choose_runtime_entry(index, remembered))
    }

    /// What entering the session list reads: the sessions themselves, and the
    /// arrangement the authority draws its own sidebar from.
    ///
    /// They travel together because the list is the arrangement applied to the
    /// sessions; a client that fetched one without the other would draw a tree
    /// that is already stale on arrival.
    fn session_page_effects(&mut self) -> Vec<Effect> {
        let mut effects = self.refresh_session_list().into_iter().collect::<Vec<_>>();
        effects.extend([
            // Project rows are named by the runtime's project records, which is
            // what the desktop's sidebar shows; the session list alone would
            // spell the directory instead.
            Effect::ListWorkspaces,
            Effect::LoadSidebarOrganization,
        ]);
        effects
    }

    /// Re-read whatever the configuration centre is showing.
    ///
    /// Every write ends here, which is what makes "what changed is what is
    /// re-read" true for verbs nobody has written yet.
    pub fn refresh_config_area(&mut self) -> Outcome {
        Outcome::effects(self.config_section_effects())
    }

    fn refresh_current_page(&mut self) -> Outcome {
        match self.page {
            Page::NewSession | Page::Sessions => Outcome::effects(self.session_page_effects()),
            Page::Agent => Outcome::effects(self.refresh_timeline().into_iter().collect()),
            Page::Devices => Outcome::effects(self.device_page_effects()),
            Page::Providers => Outcome::effects(vec![Effect::ListProfiles, Effect::LoadGateway]),
            Page::Agents => Outcome::effects(vec![Effect::ListAgents]),
            Page::Mcp => Outcome::effects(vec![Effect::ListMcp]),
            Page::Skills => Outcome::effects(vec![Effect::ListSkills]),
            Page::Prompts => Outcome::effects(vec![Effect::ListPrompts]),
            Page::Management => Outcome::effects(self.config_section_effects()),
            Page::Settings | Page::Help => Outcome::effects(vec![]),
        }
    }

    /// Re-read what one write that has just landed changed.
    ///
    /// A write is answered with an acknowledgement, not with the record it
    /// produced: the authority is what decides what a record now looks like,
    /// and a client that patched its own copy would be guessing at the parts
    /// of the record the request did not carry. Every key the worker reports a
    /// completed write under therefore ends here, and the key decides what is
    /// read again — a reader should never have to press `Ctrl+R` to see the
    /// change they just made.
    ///
    /// The reads are the ones the surface on screen is drawn from, so a write
    /// that happens to be about something else costs nothing. Keys with
    /// nothing to re-read — a permission answer, a steer — fall through:
    /// their record arrives as an event rather than as a write's answer.
    pub fn refresh_after_mutation(&mut self, key: &str) -> Outcome {
        match key {
            // A session's title and its membership of the list are the list's
            // to draw, and the list is the sidebar of every page.
            "rename_session" | "archive_session" | "unarchive_session" | "delete_session" => {
                Outcome::effects(self.session_page_effects())
            }
            // A project is a heading the workspace listing draws, and every
            // session filed under it went with it: both reads are what makes the
            // write visible.
            "delete_project" => {
                let mut effects = vec![Effect::ListWorkspaces];
                effects.extend(self.session_page_effects());
                Outcome::effects(effects)
            }
            // Records the management surfaces draw. Which read answers one is
            // the area on screen: the same write is a row in whichever
            // collection the reader is looking at.
            "toggle_entry" | "install_agent" | "uninstall_agent" | "logout_agent"
            | "update_entry" | "rename_profile" | "select_profile" | "provider_secret" => {
                Outcome::effects(self.management_record_effects())
            }
            // An account is two reads: the contexts that say who is signed in,
            // and the catalogue that says with what. A sign-in or a sign-out
            // moves both, and the panel draws both — so answering the press
            // without re-reading is how the reader ends up looking at the
            // account as it was before the key they just pressed.
            "agent_auth_sign_in" | "agent_auth_sign_out" => {
                let mut effects = vec![Effect::ListAgentAuth {
                    agent_id: None,
                    open: false,
                }];
                if let Some(catalog) = self.management_data.agent_auth_catalog.as_ref() {
                    effects.push(Effect::LoadAgentAuthCatalog {
                        agent_id: catalog.agent_id.clone(),
                        refresh: false,
                    });
                }
                Outcome::effects(effects)
            }
            "revoke_device" => Outcome::effects(vec![Effect::ListDevices]),
            _ => Outcome::quiet(),
        }
    }

    /// The reads that redraw whichever management area is on screen.
    ///
    /// A record is only ever drawn by the area that shows it, so re-reading
    /// that area is what makes a write visible; the other areas read again when
    /// the reader walks to them.
    fn management_record_effects(&mut self) -> Vec<Effect> {
        match self.page {
            Page::Management => self.config_section_effects(),
            Page::Agents => vec![Effect::ListAgents],
            Page::Providers => vec![Effect::ListProfiles, Effect::LoadGateway],
            Page::Mcp => vec![Effect::ListMcp],
            Page::Skills => vec![Effect::ListSkills],
            Page::Prompts => vec![Effect::ListPrompts],
            Page::Devices => vec![Effect::ListDevices],
            _ => vec![],
        }
    }

    fn toggle_selected_entry(&mut self) -> Outcome {
        if self.page == Page::Management {
            return self.toggle_config_entry();
        }
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

    /// `Space` on the highlighted record: its on/off verb, when it has one.
    fn toggle_config_entry(&mut self) -> Outcome {
        use crate::config_center::ConfigAction;
        let Some(row) = self.selected_config_row() else {
            return Outcome::quiet();
        };
        for action in [ConfigAction::Toggle] {
            if row.actions.contains(&action) {
                return self.run_config_action(&row, action);
            }
        }
        Outcome::quiet()
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
            // The test row raises a real notification through every live
            // channel, so the reader can see what their terminal — or their
            // desktop — does with it. When nothing is live the answer says so
            // rather than leaving them to wonder whether it worked.
            crate::settings::SettingRow::NotifyTest => {
                if self.test_notification() {
                    self.toast(Toast::info(self.strings.settings_notify_test_sent()));
                } else {
                    self.toast(Toast::warning(self.strings.settings_notify_test_none()));
                }
                Outcome::quiet()
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
        // A toggle with no state to show — the desktop popup on a remote seat —
        // has nothing to reset either, and asking first would be a question
        // whose answer changes nothing.
        if matches!(
            crate::settings::definition(row).kind,
            crate::settings::SettingKind::Toggle
        ) && self.setting_choices(row).is_empty()
        {
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
            mentions: Vec::new(),
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

/// What one printable key means to the open elicitation form.
///
/// The form is the one surface where a letter is sometimes an answer and
/// sometimes a shortcut, so the decision is named rather than left to the key
/// handler: [`App::elicitation_key`] makes it, the run loop only carries it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationKey {
    /// The character is part of the answer being typed.
    Text,
    /// The character picks the option at this index.
    Option(usize),
    /// The form has nothing to do with it; the binding table decides.
    Elsewhere,
}

/// A locally-held draft of an elicitation form.
///
/// The answers live here rather than in the shared controller because the
/// controller models the *request*, not the in-progress edit; keeping the draft
/// next to the reducer is what lets the whole form be tested without a
/// terminal. Every keystroke is committed as it is typed — a terminal field is
/// one line edited at its end — so there is no separate live buffer that a
/// missed commit could strand the reader's words in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElicitationDraft {
    text: std::collections::BTreeMap<String, String>,
    boolean: std::collections::BTreeMap<String, bool>,
    multi: std::collections::BTreeMap<String, Vec<String>>,
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
    }

    /// Answer for one field, using the draft where the user typed something and
    /// the field's default otherwise.
    ///
    /// An empty string is not an answer: a required field left blank must fail
    /// the request's own validation rather than be sent as one, and an optional
    /// one left blank must be left out of the map entirely.
    pub fn answer_for(
        &self,
        field_id: &str,
        definition: &vibex_core::ElicitationField,
    ) -> Option<ElicitationAnswerValue> {
        use vibex_core::ElicitationFieldKind;
        match &definition.kind {
            ElicitationFieldKind::Text { default, .. } => {
                let value = self
                    .text(field_id)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| default.clone())?;
                Some(ElicitationAnswerValue::String(value))
            }
            ElicitationFieldKind::Number { default, .. } => {
                let value = self
                    .text(field_id)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| default.clone())?;
                Some(ElicitationAnswerValue::Number(value))
            }
            ElicitationFieldKind::Integer { default, .. } => {
                let value = self
                    .text(field_id)
                    .filter(|value| !value.is_empty())
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
}

/// Read an id back from the key a row published.
///
/// The key *is* the id the row was projected from, so a parse failure means
/// the form outlived the record it was opened on; the caller turns that into
/// "nothing happened" rather than writing to a guessed id.
fn parse_config_id<T: std::str::FromStr>(value: &str) -> Result<T, &'static str> {
    value.parse().map_err(|_| "unknown record")
}

/// A token limit as the form typed it: a whole number greater than zero, or
/// nothing at all.
///
/// A limit that is not a number is refused rather than dropped, because a
/// silent zero would tell the Agent the Model has no context at all.
fn declared_token_limit(
    form: &crate::config_center::ConfigForm,
    key: &str,
    strings: crate::locale::Strings,
) -> Result<Option<u32>, &'static str> {
    match form.optional(key) {
        None => Ok(None),
        Some(value) => match value.parse::<u32>() {
            Ok(tokens) if tokens > 0 => Ok(Some(tokens)),
            _ => Err(strings.config_form_error_tokens()),
        },
    }
}

/// Which narrowing of a registry one press asked for.
///
/// The two registries are narrowed differently — MCP by transport family, Skills
/// by ranking — so the key that cycles one says which control it is aiming at
/// rather than the reducer guessing from the module twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryStep {
    Transport,
    Sort,
}

/// `KEY=value` (or `Name: value`) per line, as the forms ask for them.
///
/// A line with no separator is skipped rather than guessed at: half an
/// environment variable is not a value, and inventing one would send a secret
/// to a server under a name nobody chose.
fn parse_pairs(text: &str, separator: char) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once(separator)?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// A comma-separated tag list.
fn split_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

fn mcp_scope(value: &str) -> vibex_core::McpServerScopeKind {
    match value {
        "global" => vibex_core::McpServerScopeKind::Global,
        "project" => vibex_core::McpServerScopeKind::Project,
        "workspace" => vibex_core::McpServerScopeKind::Workspace,
        _ => vibex_core::McpServerScopeKind::User,
    }
}

fn skill_scope(value: &str) -> vibex_core::SkillScopeKind {
    match value {
        "global" => vibex_core::SkillScopeKind::Global,
        "project" => vibex_core::SkillScopeKind::Project,
        "workspace" => vibex_core::SkillScopeKind::Workspace,
        _ => vibex_core::SkillScopeKind::User,
    }
}

fn prompt_scope(value: &str) -> vibex_core::PromptScopeKind {
    match value {
        "global" => vibex_core::PromptScopeKind::Global,
        "project" => vibex_core::PromptScopeKind::Project,
        "workspace" => vibex_core::PromptScopeKind::Workspace,
        _ => vibex_core::PromptScopeKind::User,
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

    /// The key that takes the host's offer exists on every run, so the offer
    /// itself is what decides whether it does anything.
    #[test]
    fn taking_the_home_answers_only_an_offer_that_stands() {
        let mut app =
            app_with_capabilities(vibex_backend::BackendCapabilitySnapshot::desktop_native_v1());
        assert!(!app.handover_offered());
        app.perform(Intent::TakeOverHome);
        assert!(
            !app.handover_requested,
            "an unoffered takeover must not end the run"
        );
        let refused = app.strings.toast_offline().to_string();
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.text.clone()),
            Some(refused)
        );

        let offer = crate::HandoverOffer::new();
        offer.set_available(true);
        app.handover = Some(offer.clone());
        assert!(app.handover_offered());
        app.perform(Intent::TakeOverHome);
        assert!(app.handover_requested, "the offer is the reader's to take");

        // Withdrawn by the host, the key goes quiet again — including for a
        // reader who already took it once.
        app.handover_requested = false;
        offer.set_available(false);
        assert!(!app.handover_offered());
    }

    /// The banner is the one line a disconnected reader always sees, so the
    /// offer has to be there rather than only where a blocked action complains.
    #[test]
    fn the_offer_replaces_the_disconnected_banner() {
        let mut app =
            app_with_capabilities(vibex_backend::BackendCapabilitySnapshot::desktop_native_v1());
        app.live = crate::app::LiveState::Offline;
        app.refresh_banner();
        let offline = app
            .banner
            .as_ref()
            .map(|banner| banner.text.clone())
            .expect("a disconnected seat carries a banner");
        assert_eq!(offline, app.strings.toast_offline());

        let offer = crate::HandoverOffer::new();
        offer.set_available(true);
        app.handover = Some(offer);
        app.refresh_banner();
        let offered = app
            .banner
            .as_ref()
            .map(|banner| banner.text.clone())
            .expect("the offer is a banner");
        assert_eq!(offered, app.strings.handover_offer());
        assert_ne!(offered, offline);
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
    fn opening_a_session_from_the_list_takes_the_keyboard_into_the_composer() {
        let mut app = capable_app();
        app.agent
            .apply_sessions(Ok(vec![openable_session("session_enter0001")]))
            .expect("sessions apply");
        app.navigate_to(Page::Sessions);
        // Row 0 is the workspace heading; the session is one below it.
        app.set_selection(crate::keymap::Scope::Sessions, 1);
        let outcome = app.perform(Intent::OpenSelectedSession);
        assert!(
            !outcome.effects.is_empty(),
            "opening the session issued no load: {outcome:?}"
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
        // writing a new session who opens the Settings page comes back to the
        // draft, with the keyboard where they left it — not to a session list
        // they never opened, which is where a fixed hierarchy sent them.
        let mut app = capable_app();
        assert_eq!(app.page, Page::NewSession);
        app.composer.insert_str("a first thought");

        app.perform(Intent::OpenSettings);
        assert_eq!(app.page, Page::Settings);

        let outcome = app.perform(Intent::Back);
        assert!(outcome.effects.is_empty());
        assert_eq!(
            app.page,
            Page::NewSession,
            "Esc left the page the Settings page was opened from"
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
        // The configuration centre is one surface with four modules, so a
        // module route is not a page change: `Back` leaves the centre in one
        // press rather than walking out through the module it was opened on.
        let mut app = capable_app();
        app.navigate_to(Page::Management);
        app.navigate_to(Page::Providers);
        assert_eq!(app.page, Page::Management);
        assert_eq!(
            app.config_center.module,
            crate::config_center::ConfigModule::Agent
        );
        assert_eq!(
            app.config_center.section,
            crate::config_center::ConfigSection::Providers
        );

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
            member_count: 1,
            auto_routes: false,
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
            member_count: 1,
            auto_routes: false,
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

    /// An app on `page`, over a session in `state`, with the connection in
    /// `live`.
    ///
    /// The session stays the open one even on the composing page, which is the
    /// state that page is in after a reader steps out of a session to write a
    /// new one: the page shows no session, and the open one is still there.
    fn app_over_session(
        page: Page,
        state: vibex_core::AgentSessionState,
        live: crate::app::LiveState,
    ) -> App {
        let mut app = capable_app();
        let mut session = openable_session("session_quit0001");
        session.state = state;
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(page);
        app.live = live;
        app
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

    /// The catalogue row one of the pinned recent entries is drawn on.
    fn recent_row(app: &App, position: usize) -> usize {
        app.runtime_picker_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row,
                    RuntimePickerRow::Entry {
                        remembered: true,
                        ..
                    }
                )
            })
            .map(|(row, _)| row)
            .nth(position)
            .expect("the remembered row is on the list")
    }

    /// A session on the catalogue's second entry, which ran at `High` once.
    fn app_with_a_recent_run() -> App {
        let mut app = app_with_run_options(Page::Agent);
        let mut remembered = app.runtime_options.as_ref().expect("catalogue").options[1]
            .selection
            .clone();
        remembered.reasoning_effort = Some("high".to_string());
        app.remember_runtime_selection(&remembered);
        app.show_runtime_picker();
        app.overlay = Some(Overlay::RuntimePicker {
            view: RuntimePickerView::Choices,
            selected: recent_row(&app, 0),
        });
        app
    }

    #[test]
    fn a_recent_row_is_chosen_with_the_run_options_it_remembers() {
        // A pinned recent row is the reader's own answer read back to them: the
        // Agent, the model and how it ran were chosen together, so `Enter` on it
        // takes the whole answer and closes. Walking them through their own note
        // to agree with it is the second visit this saves.
        let mut app = app_with_a_recent_run();
        let chosen = app.perform(Intent::ConfirmOverlay);
        let selection = switched(&chosen).expect("the recent row did not switch the session");
        assert_eq!(
            selection.reasoning_effort.as_deref(),
            Some("high"),
            "the row was chosen without what it remembered"
        );
        assert_eq!(
            app.overlay, None,
            "the picker asked again for an answer it already had"
        );
    }

    #[test]
    fn tab_on_a_recent_row_still_opens_its_run_options() {
        // `Enter` is a shortcut rather than a lock: the reader who does want to
        // change a remembered value reaches it exactly where it always was, and
        // the view opens on what the row carried rather than on the Agent's
        // default.
        let mut app = app_with_a_recent_run();
        let opened = app.perform(Intent::OverlayNextField);
        assert!(opened.effects.is_empty(), "{opened:?}");
        assert_eq!(
            app.overlay,
            Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Options,
                selected: 0,
            }),
            "`Tab` did not open the run options"
        );
        let options = app.picker_run_options();
        assert_eq!(options[0].explicit.as_deref(), Some("high"));
    }

    #[test]
    fn a_digit_takes_a_recent_row_the_way_enter_does() {
        // A digit is `Enter` on a row the reader cannot be bothered to walk to,
        // and the numbered rows are exactly the recent ones: the gesture is the
        // same, so the answer must be too.
        let mut app = app_with_a_recent_run();
        let chosen = app
            .quick_pick_runtime('1')
            .expect("the digit names no remembered row");
        let selection = switched(&chosen).expect("the digit did not switch the session");
        assert_eq!(selection.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(app.overlay, None, "the digit stopped to ask again");
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
            target: None,
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
    fn the_composing_page_says_it_is_loading_until_the_catalogue_lands() {
        // The read the page opens with is not instantaneous: the first frames
        // are painted before the answer is. "Unavailable" would be a claim
        // about the runtime rather than about this read, so the Agent setup
        // line says it is loading, and the answer — a catalogue, an empty one,
        // or a failure — is what replaces it.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        assert!(app.runtime_options.is_none());

        app.perform(Intent::NewSession);
        assert!(
            app.runtime_options_reading,
            "the page asked for the catalogue without recording the read"
        );
        assert_eq!(
            app.composer_runtime_labels().0,
            app.strings.loading(),
            "a read on its way was reported as an answer"
        );

        // The catalogue that lands names the entry a creation would use.
        app.runtime_options = Some(run_option_catalog());
        app.runtime_options_reading = false;
        assert_eq!(app.composer_runtime_labels().0, "claude");

        // A backend that cannot read a catalogue never asks, so a page with
        // none in hand still says the runtime is unavailable.
        let mut offline =
            app_with_capabilities(vibex_backend::BackendCapabilitySnapshot::disconnected_v1());
        offline.live = crate::app::LiveState::Ready;
        offline.perform(Intent::NewSession);
        assert!(!offline.runtime_options_reading);
        assert_eq!(
            offline.composer_runtime_labels().0,
            offline.strings.runtime_unavailable()
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
    fn a_destination_named_from_behind_a_modal_leaves_it() {
        // The cheatsheet goes on listing the global destinations while it is
        // open, so `F1` has to answer there. An overlay that held the keyboard
        // against them was promising a key it did not answer: the reader read
        // `F1 Settings` off the list, pressed it, and nothing happened.
        let mut app = capable_app();
        app.perform(Intent::ToggleHelp);
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })));

        app.perform(Intent::OpenSettings);
        assert_eq!(app.overlay, None, "the overlay stayed up");
        assert_eq!(app.page, Page::Settings);

        // The palette is a destination too, and a `Confirm` card is the one
        // modal a reader is most likely to be in front of when they change
        // their mind about where they wanted to be.
        app.perform(Intent::OpenCommandPalette);
        assert!(matches!(app.overlay, Some(Overlay::Palette { .. })));
        app.perform(Intent::GotoSessions);
        assert_eq!(app.overlay, None);
        assert_eq!(app.page, Page::Sessions);
    }

    #[test]
    fn a_destination_named_from_a_filter_gives_the_query_up() {
        // A filter is a text field with the same rule: the reader who names
        // somewhere else to be is done filtering, and the query is not a reason
        // to swallow the key.
        let mut app = capable_app();
        app.perform(Intent::GotoSessions);
        app.filtering = true;
        app.filter = "alpha".to_string();

        app.perform(Intent::OpenSettings);
        assert!(!app.filtering, "the filter stayed open");
        assert!(app.filter.is_empty(), "the query outlived the filter");
        assert_eq!(app.page, Page::Settings);
    }

    #[test]
    fn the_palette_wraps_at_both_ends() {
        // The list is a ring, not a shelf: `↓` on the last command answers with
        // the first, and `↑` on the first answers with the last. One rule for
        // both directions is what keeps an end from reading as a dead key.
        let mut app = capable_app();
        app.perform(Intent::OpenCommandPalette);
        let count = app.palette_entries("").len();
        assert!(count > 1, "the palette has nothing to walk");

        let selected = |app: &App| match app.overlay {
            Some(Overlay::Palette { selected, .. }) => selected,
            _ => panic!("the palette is not up"),
        };

        app.overlay = Some(Overlay::Palette {
            query: String::new(),
            selected: count - 1,
        });
        app.perform(Intent::SelectNext);
        assert_eq!(
            selected(&app),
            0,
            "the last command did not wrap to the first"
        );

        // The wrap back is the same move in the other direction, from the first
        // entry the previous press landed on.
        app.perform(Intent::SelectPrevious);
        assert_eq!(
            selected(&app),
            count - 1,
            "the first command did not wrap back to the last"
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
    fn a_running_turn_is_still_interrupted_by_a_live_client() {
        // The rule the disconnected case bends for: with the connection up,
        // `Ctrl+C` on the session's own page stops the turn the reader is
        // watching rather than asking them to leave.
        let mut app = app_over_session(
            Page::Agent,
            AgentSessionState::Running,
            crate::app::LiveState::Ready,
        );
        let outcome = app.perform(Intent::ContextualCancel);
        assert!(
            matches!(outcome.effects.as_slice(), [Effect::Interrupt { .. }]),
            "a live client did not interrupt the turn it is showing: {outcome:?}"
        );
        assert!(
            !app.should_quit,
            "the interrupt was taken for an instruction"
        );
        assert!(!app.quit_armed(), "stopping a turn asked about leaving");
    }

    #[test]
    fn a_disconnected_client_can_still_be_left() {
        // The turn on screen is the last thing the runtime reported, not
        // something this client can still stop: over a dead link the interrupt
        // changes nothing. Spending the press on it would leave `Ctrl+C` with
        // nothing it could ever do — the reader would be trapped in a client
        // whose connection is already gone, which is the one moment they most
        // need to leave it.
        for state in [AgentSessionState::Running, AgentSessionState::NeedsInput] {
            let mut app = app_over_session(Page::Agent, state, crate::app::LiveState::Offline);
            let first = app.perform(Intent::ContextualCancel);
            assert!(
                first.effects.is_empty(),
                "an interrupt was dispatched over a dead connection: {first:?}"
            );
            assert!(!app.should_quit, "one press was taken for an instruction");
            assert!(
                app.quit_armed(),
                "the disconnected page did not ask anything for a {state:?} session"
            );
            app.perform(Intent::ContextualCancel);
            assert!(
                app.should_quit,
                "the second press did not leave a disconnected client"
            );
        }
    }

    #[test]
    fn ctrl_c_on_the_composing_page_asks_to_leave_rather_than_interrupting() {
        // The session behind the page keeps its turn: the composing page has no
        // turn of its own, so a press there is not an instruction to stop
        // something the page is not showing. Reading the session behind it made
        // the page unable to leave while that session was running.
        let mut app = app_over_session(
            Page::NewSession,
            AgentSessionState::Running,
            crate::app::LiveState::Ready,
        );
        let first = app.perform(Intent::ContextualCancel);
        assert!(
            first.effects.is_empty(),
            "the composing page interrupted a turn it does not show: {first:?}"
        );
        assert!(!app.should_quit, "one press was taken for an instruction");
        assert!(app.quit_armed(), "the page did not ask anything");
        app.perform(Intent::ContextualCancel);
        assert!(app.should_quit, "the second press did not leave");
    }

    #[test]
    fn ctrl_c_off_the_session_page_asks_to_leave_rather_than_interrupting() {
        // The session behind every other page keeps running: the list, the
        // configuration centre and the panels are places to read or choose, not
        // a view of the turn, so a press there is not an instruction to stop
        // one. The composing page has its own test above; these are the pages
        // that still own the selected session for navigation only.
        for page in [
            Page::Sessions,
            Page::Management,
            Page::Providers,
            Page::Agents,
            Page::Mcp,
            Page::Skills,
            Page::Prompts,
            Page::Devices,
            Page::Settings,
            Page::Help,
        ] {
            let mut app = app_over_session(
                page,
                AgentSessionState::Running,
                crate::app::LiveState::Ready,
            );
            let first = app.perform(Intent::ContextualCancel);
            assert!(
                first.effects.is_empty(),
                "{page:?} interrupted a turn it does not show: {first:?}"
            );
            assert!(
                !app.should_quit,
                "{page:?}: one press was taken for an instruction"
            );
            assert!(app.quit_armed(), "{page:?}: the page did not ask to leave");
            app.perform(Intent::ContextualCancel);
            assert!(app.should_quit, "{page:?}: the second press did not leave");
        }
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
    fn archiving_an_archived_row_restores_it_without_asking() {
        // The gesture is a toggle: the row under the cursor picks the
        // direction, and only retiring a session asks for confirmation.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        let mut archived = openable_session("session_archived001");
        archived.archived_at_ms = Some(1);
        app.agent
            .apply_sessions(Ok(vec![archived.clone()]))
            .expect("sessions apply");
        app.navigate_to(Page::Sessions);
        let cursor = app
            .sidebar_rows()
            .iter()
            .position(|row| row.session_id.as_ref() == Some(&archived.id))
            .expect("the archived session has a row");
        app.set_selection(Scope::Sessions, cursor);

        let restored = app.perform(Intent::ArchiveSession);
        assert!(
            app.overlay.is_none(),
            "restoring destroys nothing, so it asks nothing: {:?}",
            app.overlay
        );
        let [Effect::UnarchiveSession { session_id }] = restored.effects.as_slice() else {
            panic!("expected one restore, got {restored:?}");
        };
        assert_eq!(session_id, &archived.id);
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
        app.sync_transcript();
        // A band shorter than the loaded window, so scrolling up parks at the
        // top instead of deciding the tail is the bottom.
        app.transcript_band_rows = 1;

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

        // Approaching the top admits one more page — the reader is about to
        // read into the first one and should not have to wait for the second —
        // and the controller's in-flight limit refuses the one after that, so
        // a position parked near the top cannot queue a request per keypress.
        app.scroll.follow = false;
        app.scroll.offset = 0;
        assert!(app.at_transcript_top());
        let prefetch = app.perform(Intent::ScrollHalfPageUp);
        let [Effect::LoadOlder { ticket: prefetched }] = prefetch.effects.as_slice() else {
            panic!("expected a prefetched page, got {:?}", prefetch.effects);
        };
        assert_eq!(prefetched.before_sequence, 3);
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
            starts_at_turn_start: true,
            leading_turn_start: None,
            omitted: None,
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
    fn approaching_the_top_prefetches_before_the_reader_asks() {
        let mut app = capable_app();
        let session_id = VibexSessionId::new();
        app.navigate_to(Page::Agent);
        app.agent.state.selected_session_id = Some(session_id.clone());
        app.agent.state.timeline.replace_authoritative(
            session_id.clone(),
            (1..=12)
                .map(|sequence| history_item(&session_id, sequence))
                .collect::<Vec<_>>(),
        );
        app.agent.state.timeline_has_older = true;
        app.sync_transcript();
        // A band shorter than the loaded window, so scrolling up inside it
        // parks the viewport instead of deciding the tail is the bottom.
        app.transcript_band_rows = 10;
        app.scroll.follow = false;
        // Inside the head start of a screen and a half, but not at the top.
        app.scroll.offset = 12;
        assert!(!app.at_transcript_top());

        let outcome = app.perform(Intent::ScrollPageUp);
        let [Effect::LoadOlder { ticket }] = outcome.effects.as_slice() else {
            panic!("expected a prefetch, got {:?}", outcome.effects);
        };
        assert_eq!(ticket.before_sequence, 1);

        // The second slot is the prefetch depth; the ask after that is refused
        // even though the reader has not moved.
        assert!(app.prefetch_older_history().is_some());
        assert!(app.prefetch_older_history().is_none());

        // Far from the top a scroll is just a scroll.
        app.scroll.offset = 400;
        assert!(app.prefetch_older_history().is_none());
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
            draft.answer_for("name", &field),
            Some(ElicitationAnswerValue::String("typed".into()))
        );
        draft.clear();
        assert_eq!(
            draft.answer_for("name", &field),
            Some(ElicitationAnswerValue::String("default".into()))
        );
    }

    #[test]
    fn elicitation_draft_leaves_a_blank_field_out_of_the_answers() {
        // A field the reader never touched is not an answer: an optional one
        // must be absent from the map rather than sent as an empty string, and
        // a required one must fail the request's own validation instead of
        // being answered with nothing.
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
        assert_eq!(draft.answer_for("q", &field), None);
        draft.set_text("q", "hello");
        assert_eq!(
            draft.answer_for("q", &field),
            Some(ElicitationAnswerValue::String("hello".into()))
        );
        // Typed and then rubbed out: blank again, not an empty answer.
        draft.set_text("q", "");
        assert_eq!(draft.answer_for("q", &field), None);
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
            draft.answer_for("flag", &field),
            Some(ElicitationAnswerValue::Boolean(true))
        );
        draft.set_boolean("flag", false);
        assert_eq!(
            draft.answer_for("flag", &field),
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
            worktree_lifecycle: std::collections::BTreeMap::new(),
            project_new_session_locations: std::collections::BTreeMap::new(),
            auto_continue_default_enabled: false,
            auto_continue_disabled_project_ids: Vec::new(),
            auto_continue_project_ids: Vec::new(),
            auto_continue_session_overrides: std::collections::BTreeMap::new(),
            auto_continue_session_ids: Vec::new(),
            auto_continue_paused_session_ids: Vec::new(),
            unread_session_ids: vec!["session_org000001".to_string()],
            queued_sends: Vec::new(),
            paused_queue_session_ids: Vec::new(),
            manual_queue_session_ids: Vec::new(),
            scheduled_sends: Vec::new(),
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

    /// The page's own action row is one step above the first list row: `↑` from
    /// the top reaches it and `↓` steps back onto the list.
    #[test]
    fn the_session_cursor_walks_onto_the_pages_action_row() {
        let mut app = arranged_app();
        app.set_selection(Scope::Sessions, 0);
        assert!(!app.session_cursor_is_actions());

        app.perform(Intent::SelectPrevious);
        assert!(
            app.session_cursor_is_actions(),
            "up from the first row did not reach the action row"
        );
        assert_eq!(
            app.session_row_action,
            Some((crate::app::SESSION_ACTION_ROW, Intent::NewSession)),
            "the action row did not open on its first control"
        );

        // `→` switches to the workspace control and `→` again comes back: with
        // two controls the two keys toggle, which is what the line promises.
        app.perform(Intent::SessionControlNext);
        assert_eq!(
            app.session_row_action,
            Some((crate::app::SESSION_ACTION_ROW, Intent::SwitchWorkspace))
        );
        app.perform(Intent::SessionControlNext);
        assert_eq!(
            app.session_row_action,
            Some((crate::app::SESSION_ACTION_ROW, Intent::NewSession))
        );
        app.perform(Intent::SessionControlPrevious);
        assert_eq!(
            app.session_row_action,
            Some((crate::app::SESSION_ACTION_ROW, Intent::SwitchWorkspace))
        );

        // Down steps back onto the first row and leaves the button behind.
        app.perform(Intent::SelectNext);
        assert_eq!(app.selection_for(Scope::Sessions), 0);
        assert_eq!(app.session_row_action, None);
        assert!(!app.session_cursor_is_actions());
    }

    /// `Enter` on the action row runs the control the cursor has reached: `+ New
    /// session` opens the composing page, and the workspace control opens the
    /// directory picker.
    #[test]
    fn enter_on_the_action_row_runs_the_control_it_names() {
        let mut app = arranged_app();
        app.set_selection(Scope::Sessions, 0);
        app.perform(Intent::SelectPrevious);
        app.perform(Intent::OpenSelectedSession);
        assert_eq!(
            app.page,
            Page::NewSession,
            "the first control is new session"
        );
        assert_eq!(
            app.session_row_action, None,
            "the press left the button armed"
        );

        app.navigate_to(Page::Sessions);
        app.set_selection(Scope::Sessions, 0);
        app.perform(Intent::SelectPrevious);
        app.perform(Intent::SessionControlNext);
        app.perform(Intent::OpenSelectedSession);
        assert!(
            matches!(app.overlay, Some(Overlay::WorkspacePicker { .. })),
            "the workspace control did not open the picker: {:?}",
            app.overlay
        );
        assert_eq!(
            app.session_row_action, None,
            "the press left the button armed"
        );
    }

    /// `→` steps onto a row's own buttons and walks them, `←` walks back and
    /// steps out onto the row, and `Enter` presses whatever the cursor reached.
    #[test]
    fn the_row_buttons_are_reached_with_the_arrows_and_pressed_with_enter() {
        let mut app = arranged_app();
        app.live = crate::app::LiveState::Ready;
        let rows = app.sidebar_rows();
        let session_row = rows
            .iter()
            .position(|row| row.session_id.is_some())
            .expect("the arrangement holds no session");
        app.set_selection(Scope::Sessions, session_row);
        let buttons = app.sidebar_row_button_intents(session_row);
        assert_eq!(buttons.first(), Some(&Intent::PinSession));
        assert_eq!(buttons.last(), Some(&Intent::DeleteSession));

        // `→` steps in at the first button, then walks right.
        app.perform(Intent::SessionControlNext);
        assert_eq!(
            app.session_row_action,
            Some((session_row, Intent::PinSession))
        );
        app.perform(Intent::SessionControlNext);
        assert_eq!(
            app.session_row_action,
            Some((session_row, Intent::ToggleAutoContinue))
        );
        // `←` walks back, and from the first button it steps out onto the row.
        app.perform(Intent::SessionControlPrevious);
        assert_eq!(
            app.session_row_action,
            Some((session_row, Intent::PinSession))
        );
        app.perform(Intent::SessionControlPrevious);
        assert_eq!(
            app.session_row_action, None,
            "left did not leave the cluster"
        );
        // `←` from the row steps in at the last button, so both keys are ways
        // into the cluster.
        app.perform(Intent::SessionControlPrevious);
        assert_eq!(
            app.session_row_action,
            Some((session_row, Intent::DeleteSession))
        );

        // `Enter` presses the button the cursor reached rather than opening the
        // row, and the button is not left armed for a second press.
        app.perform(Intent::SessionControlPrevious);
        app.perform(Intent::SessionControlPrevious);
        app.perform(Intent::SessionControlPrevious);
        assert_eq!(
            app.session_row_action,
            Some((session_row, Intent::BeginRenameSession))
        );
        let label = app.sidebar_rows()[session_row].label.clone();
        app.perform(Intent::OpenSelectedSession);
        match app.overlay.clone() {
            Some(Overlay::Prompt {
                field: crate::app::PromptField::RenameSession,
                value,
                ..
            }) => assert_eq!(value, label, "the rename prompt names another session"),
            other => panic!("the rename button opened {other:?}"),
        }
        assert_eq!(
            app.session_row_action, None,
            "the press left the button armed"
        );
        assert_eq!(
            app.page,
            Page::Sessions,
            "pressing a row button opened the row"
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
            queued_sends: Vec::new(),
            paused_queue_session_ids: Vec::new(),
            manual_queue_session_ids: Vec::new(),
            scheduled_sends: Vec::new(),
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
    fn opening_an_unread_session_tells_the_authority_it_was_read() {
        // Opening a session is what "read" means, and the mark the reader sees
        // comes from the authority. A client that only cleared its own copy
        // would leave the Desktop badging a session the reader has read, so the
        // open sends the read back before it clears anything locally.
        let mut app = arranged_app();
        let session_id = vibex_core::VibexSessionId::parse("session_org000001").unwrap();
        assert!(app.session_is_unread(&session_id));

        let outcome = app.open_session_effects(session_id.clone());
        let read = outcome.effects.iter().find_map(|effect| match effect {
            Effect::MutateSidebarOrganization { mutation, .. } => Some(mutation.clone()),
            _ => None,
        });
        assert_eq!(
            read,
            Some(
                vibex_core::RemoteSidebarOrganizationMutation::SetSessionsUnread {
                    session_ids: vec!["session_org000001".to_string()],
                    unread: false,
                }
            ),
            "opening the session did not tell the authority: {:?}",
            outcome.effects
        );
        assert!(
            !outcome.effects.iter().any(|effect| matches!(
                effect,
                Effect::MutateSidebarOrganization {
                    expected_revision: Some(_),
                    ..
                }
            )),
            "a read is not an edit of the tree and must not quote its revision"
        );
        assert!(
            !app.session_is_unread(&session_id),
            "the optimistic clear did not take"
        );
    }

    #[test]
    fn opening_a_session_the_authority_never_badged_sends_no_read() {
        // A session this client badged itself, or one already read elsewhere,
        // has nothing to clear on the authority: sending a mark anyway would be
        // a write per open for no change.
        let mut app = arranged_app();
        let mut snapshot = arranged_snapshot();
        snapshot.unread_session_ids = Vec::new();
        app.apply_sidebar_organization(&snapshot);
        assert!(
            app.sidebar_mark_read_effect(
                &vibex_core::VibexSessionId::parse("session_org000001").unwrap()
            )
            .is_none()
        );
    }

    #[test]
    fn a_finished_answer_elsewhere_tells_the_authority_it_is_unread() {
        // The other half of the read sync: a completion this client draws a dot
        // for is just as new in the Desktop window, so the mark travels back.
        // The authority's own set is the one that decides whether a write is
        // needed, so an already-badged session sends nothing.
        let app = arranged_app();
        let quiet = vibex_core::VibexSessionId::parse("session_org000002").unwrap();
        let badged = vibex_core::VibexSessionId::parse("session_org000001").unwrap();

        let reported = app.sidebar_mark_unread_effect(&quiet);
        let Some(Effect::MutateSidebarOrganization {
            mutation,
            expected_revision,
        }) = reported
        else {
            panic!("a completion the authority does not badge was not reported: {reported:?}");
        };
        assert_eq!(
            mutation,
            vibex_core::RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: vec!["session_org000002".to_string()],
                unread: true,
            }
        );
        assert_eq!(
            expected_revision, None,
            "a mark is not an edit of the tree and must not quote its revision"
        );
        assert!(
            app.sidebar_mark_unread_effect(&badged).is_none(),
            "a session the authority already badges must not queue a write per turn"
        );
    }

    #[test]
    fn a_session_the_desktop_read_stops_being_unread_here() {
        // The mark is one shared value, so it has to move in both directions and
        // in both seats. The client draws its own dot optimistically while its
        // write is on the wire, but when the Desktop opens the session the
        // authority drops the mark: the next snapshot is the news, and the row
        // here has to lose the dot with it. Replacing only the authority's copy
        // is what left the dot glowing on a session every other window had
        // already read.
        let mut app = arranged_app();
        let session_id = vibex_core::VibexSessionId::parse("session_org000001").unwrap();
        // A completion for a session nobody is looking at both badges the row
        // and queues the write back to the authority.
        assert!(app.note_activity(&final_answer_event(&session_id)));
        assert!(app.session_is_unread(&session_id));

        // The Desktop reads it, so the authority publishes the mark cleared.
        let mut read = arranged_snapshot();
        read.revision = 12;
        read.unread_session_ids = Vec::new();
        app.apply_sidebar_organization(&read);

        assert!(
            !app.session_is_unread(&session_id),
            "the desktop's read did not clear this client's dot"
        );
    }

    #[test]
    fn an_optimistic_mark_survives_a_read_that_never_held_it() {
        // The client marks a session unread the moment it sees a completion, and
        // the write that tells the authority is still on the wire when the next
        // snapshot of the *arrangement* arrives — one that simply never carried
        // that id. That is not the authority clearing the mark, so the
        // optimistic dot must stand until the authority answers the write.
        let mut app = arranged_app();
        let session_id = vibex_core::VibexSessionId::parse("session_org000002").unwrap();
        assert!(app.note_activity(&final_answer_event(&session_id)));
        assert!(app.session_is_unread(&session_id));

        // A snapshot that does not name the session either way: the write has
        // not landed yet, so the authority's copy of the marks is simply empty.
        let mut pending = arranged_snapshot();
        pending.revision = 12;
        pending.unread_session_ids = vec!["session_org000001".to_string()];
        app.apply_sidebar_organization(&pending);

        assert!(
            app.session_is_unread(&session_id),
            "a mark still on the wire was dropped by an unrelated read"
        );
    }

    /// A finished answer for a session, the shape `note_activity` reads as a
    /// completion.
    fn final_answer_event(session_id: &VibexSessionId) -> vibex_core::TimelineLiveEvent {
        vibex_core::TimelineLiveEvent {
            session_id: session_id.clone(),
            sequence: 7,
            item: vibex_core::TimelineItem {
                id: vibex_core::TimelineItemId::new(),
                session_id: session_id.clone(),
                sequence: 7,
                timestamp_ms: 7,
                source: vibex_core::TimelineSource::Agent,
                kind: vibex_core::TimelineItemKind::AgentMessage,
                correlation_id: None,
                provider_correlation_id: None,
                redaction_state: vibex_core::TimelineRedactionState::None,
                execution_attribution: None,
                payload_depth: vibex_core::TimelinePayloadDepth::Detail,
                payload: vibex_core::TimelinePayload::AgentMessage(
                    vibex_core::AgentMessagePayload {
                        text: "done".to_string(),
                        is_final: true,
                    },
                ),
            },
        }
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

    /// The read one effect represents, which is what a refresh is judged by.
    fn read_keys(outcome: &Outcome) -> Vec<&'static str> {
        outcome.effects.iter().map(|effect| effect.key()).collect()
    }

    #[test]
    fn a_completed_session_write_re_reads_the_list() {
        // The list is the sidebar of every page, so a session's own writes are
        // read back wherever the reader is. Without this a rename, an archive
        // or a delete stayed on screen as the row it used to be until the
        // reader pressed `Ctrl+R`.
        for key in [
            "rename_session",
            "archive_session",
            "unarchive_session",
            "delete_session",
        ] {
            let mut app = capable_app();
            // Deliberately not the list page: the sidebar is drawn from the
            // same list on the Agent page, and it must not go stale there.
            app.page = Page::Agent;
            let outcome = app.refresh_after_mutation(key);
            assert_eq!(
                read_keys(&outcome),
                vec!["sessions", "workspaces", "sidebar_organization"],
                "{key} did not re-read the list"
            );
        }
    }

    #[test]
    fn a_completed_management_write_re_reads_the_area_on_screen() {
        // A record is only ever drawn by the area that shows it, so the write
        // is followed by the read that area makes.
        let cases = [
            (
                crate::config_center::ConfigModule::Agent,
                crate::config_center::ConfigSection::Agents,
                "install_agent",
                "agents",
            ),
            (
                crate::config_center::ConfigModule::Agent,
                crate::config_center::ConfigSection::Providers,
                "select_profile",
                "profiles",
            ),
            (
                crate::config_center::ConfigModule::Mcp,
                crate::config_center::ConfigSection::Servers,
                "toggle_entry",
                "mcp",
            ),
            (
                crate::config_center::ConfigModule::Skills,
                crate::config_center::ConfigSection::Installed,
                "toggle_entry",
                "skills",
            ),
            (
                crate::config_center::ConfigModule::Prompts,
                crate::config_center::ConfigSection::Prompts,
                "toggle_entry",
                "prompts",
            ),
        ];
        for (module, section, key, read) in cases {
            let mut app = capable_app();
            app.show_config_module(module, Some(section));
            let outcome = app.refresh_after_mutation(key);
            assert!(
                read_keys(&outcome).contains(&read),
                "{key} on {} did not re-read {read}: {:?}",
                section.key(),
                read_keys(&outcome)
            );
        }

        // A device list is not a configuration record, and its write is
        // answered by the read its own page makes.
        let mut app = capable_app();
        app.navigate_to(Page::Devices);
        let outcome = app.refresh_after_mutation("revoke_device");
        assert_eq!(read_keys(&outcome), vec!["devices"]);
    }

    #[test]
    fn a_write_with_nothing_to_re_read_stays_quiet() {
        // An approval answer, a steer, an interrupt: the record they change
        // arrives as an event, so a read here would only ask the runtime to say
        // again what it is already saying.
        for key in [
            "resolve_permission",
            "resolve_elicitation",
            "interrupt",
            "continue_turn",
            "steer_message",
        ] {
            let mut app = capable_app();
            assert!(
                app.refresh_after_mutation(key).effects.is_empty(),
                "{key} asked for a read it does not need"
            );
        }
    }

    // ---- scheduling a send ---------------------------------------------

    /// The session-page app the schedule keys are exercised on.
    fn scheduled_session_app() -> App {
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        let session = openable_session("session_sched0001");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.navigate_to(Page::Agent);
        app
    }

    /// Apply a schedule through the editor, the way a reader does: open it,
    /// type a countdown, commit.
    fn arm_countdown(app: &mut App, minutes: &str, seconds: &str) {
        app.perform(Intent::ScheduleMessage);
        assert!(
            matches!(app.overlay, Some(Overlay::Schedule(_))),
            "the schedule editor did not open"
        );
        app.perform(Intent::OverlayToggleValue);
        {
            let Some(Overlay::Schedule(form)) = app.overlay.as_mut() else {
                unreachable!("the editor is open");
            };
            form.hours = "0".to_string();
            form.minutes = minutes.to_string();
            form.seconds = seconds.to_string();
        }
        app.perform(Intent::ConfirmOverlay);
    }

    #[test]
    fn the_schedule_key_opens_the_editor_on_the_draft() {
        let mut app = scheduled_session_app();
        app.composer.insert_str("later today");
        let opened = app.perform(Intent::ScheduleMessage);
        assert!(opened.effects.is_empty());
        let Some(Overlay::Schedule(form)) = app.overlay.clone() else {
            panic!("the editor did not open");
        };
        assert_eq!(
            form.target,
            ScheduleEditTarget::Draft(ComposerTarget::Session(
                vibex_core::VibexSessionId::parse("session_sched0001").expect("session id")
            ))
        );
        assert!(!form.clearable, "the draft had no schedule to clear");
        // Nothing is written until the reader confirms: opening an editor is
        // not a decision.
        assert_eq!(app.draft_schedule(), None);
    }

    #[test]
    fn a_refused_value_keeps_the_editor_open_with_its_reason() {
        let mut app = scheduled_session_app();
        app.perform(Intent::ScheduleMessage);
        {
            let Some(Overlay::Schedule(form)) = app.overlay.as_mut() else {
                unreachable!("the editor is open");
            };
            form.mode = crate::schedule::ScheduleMode::After;
            form.hours = "0".to_string();
            form.minutes = "0".to_string();
            form.seconds = "0".to_string();
        }
        app.perform(Intent::ConfirmOverlay);
        let Some(Overlay::Schedule(form)) = app.overlay.clone() else {
            panic!("a refused value closed the editor");
        };
        assert_eq!(
            form.error,
            Some(crate::schedule::ScheduleError::InvalidCountdown)
        );
        assert_eq!(app.draft_schedule(), None);
    }

    #[test]
    fn clearing_a_schedule_keeps_the_message() {
        let mut app = scheduled_session_app();
        app.composer.insert_str("later today");
        arm_countdown(&mut app, "5", "0");
        assert!(app.draft_schedule().is_some());

        app.perform(Intent::ScheduleMessage);
        let cleared = app.perform(Intent::ScheduleClear);
        assert!(cleared.effects.is_empty());
        assert_eq!(app.overlay, None);
        assert_eq!(app.draft_schedule(), None);
        assert_eq!(
            app.composer.text(),
            "later today",
            "clearing the timer threw the message away"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| toast.text == app.strings.schedule_cleared())
        );
    }

    #[test]
    fn a_scheduled_send_waits_in_the_queue_instead_of_going_out() {
        let mut app = scheduled_session_app();
        app.composer.insert_str("send me later");
        arm_countdown(&mut app, "5", "0");
        assert!(app.draft_schedule().is_some());

        let sent = app.perform(Intent::SubmitComposer);
        assert!(
            sent.effects.is_empty(),
            "a scheduled message was sent on the spot: {sent:?}"
        );
        let session_id = app.selected_session_id().cloned().expect("a session");
        assert_eq!(app.queued_messages.len(), 1);
        assert_eq!(app.queued_messages[0].session_id, session_id);
        assert_eq!(app.queued_messages[0].text, "send me later");
        assert!(
            app.queued_messages[0].scheduled_at_ms.is_some(),
            "the message was held with no deadline"
        );
        // The draft's schedule is spent by the submission that carried it.
        assert_eq!(app.draft_schedule(), None);
        assert!(app.drain_queue().is_empty(), "it left before its hour");
    }

    #[test]
    fn a_deadline_the_clock_walked_past_is_refused_with_the_draft_put_back() {
        let mut app = scheduled_session_app();
        app.composer.insert_str("too late now");
        app.set_draft_schedule(Some(crate::schedule::MessageSchedule::At(
            vibex_core::unix_timestamp_ms() - 1_000,
        )));
        let refused = app.perform(Intent::SubmitComposer);
        assert!(refused.effects.is_empty());
        assert_eq!(
            app.composer.text(),
            "too late now",
            "the refused send lost what the reader wrote"
        );
        assert!(app.queued_messages.is_empty());
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| toast.text == app.strings.schedule_error_not_future())
        );
    }

    #[test]
    fn a_scheduled_first_message_creates_the_session_and_waits_for_its_hour() {
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.perform(Intent::NewSession);
        app.composer.insert_str("the first thing");
        arm_countdown(&mut app, "5", "0");

        let created = app.perform(Intent::SubmitComposer);
        assert!(
            created
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::CreateSession { .. })),
            "the session was not created for the scheduled message: {created:?}"
        );
        // The reader lands in the session at once — that is what puts a row in
        // the list, counting down, before anything has been sent.
        assert!(app.page_shows_uncreated_session());
        let request_id = app.selected_session_id().cloned().expect("a session");
        assert_eq!(app.queued_messages.len(), 1);
        assert_eq!(app.queued_messages[0].session_id, request_id);
        assert!(app.queued_messages[0].scheduled_at_ms.is_some());
        assert!(
            app.pending_sends.is_empty(),
            "a scheduled first message was projected as a send"
        );

        // The authority acknowledges the creation, and the message still does
        // not go: its clock is what it is waiting for.
        assert!(
            app.pending_send_effect(request_id.clone()).is_none(),
            "the acknowledged creation sent the message anyway"
        );
        assert!(!app.page_session_is_being_created());
        assert!(app.drain_queue().is_empty(), "it left before its hour");
    }

    #[test]
    fn a_creation_that_failed_takes_its_scheduled_message_back_to_the_draft() {
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.perform(Intent::NewSession);
        app.composer.insert_str("the first thing");
        arm_countdown(&mut app, "5", "0");
        app.perform(Intent::SubmitComposer);
        let request_id = app.selected_session_id().cloned().expect("a session");
        assert_eq!(app.queued_messages.len(), 1);

        assert!(app.fail_creation(&request_id));
        assert!(
            app.queued_messages.is_empty(),
            "a message waited on a session that was never created"
        );
        assert_eq!(app.composer.text(), "the first thing");
        assert!(
            app.draft_schedule().is_some(),
            "the retry lost the send time the reader chose"
        );
    }

    #[test]
    fn the_first_message_names_the_session_it_creates() {
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        app.runtime_options = Some(run_option_catalog());
        app.perform(Intent::NewSession);
        app.composer.insert_str("  Fix   the flake  ");

        app.perform(Intent::SubmitComposer);
        let request_id = app.selected_session_id().cloned().expect("a session");
        // One line, as the list draws it: what the reader wrote is the name the
        // row wears until the authority has one of its own.
        assert_eq!(
            app.pending_session_titles
                .get(&request_id)
                .map(String::as_str),
            Some("Fix the flake")
        );

        // A creation that never became a session takes its name with it: the
        // retry reserves another identity, and nothing would look this one up.
        assert!(app.fail_creation(&request_id));
        assert!(!app.pending_session_titles.contains_key(&request_id));
    }

    #[test]
    fn a_schedule_is_written_onto_the_draft_that_armed_it() {
        // The reader armed a send time on one session, walked to another, and
        // committed there. The value belongs to the draft that opened the
        // editor, so it must not land on the page in front of them now.
        let mut app = capable_app();
        app.live = crate::app::LiveState::Ready;
        let first = openable_session("session_sched0001");
        let second = openable_session("session_sched0002");
        app.agent.state.selected_session_id = Some(first.id.clone());
        app.agent.state.active_session.resolve(first);
        app.navigate_to(Page::Agent);
        app.perform(Intent::ScheduleMessage);
        app.agent.state.selected_session_id = Some(second.id.clone());
        app.agent.state.active_session.resolve(second);
        app.navigate_to(Page::Agent);
        app.perform(Intent::ConfirmOverlay);
        assert_eq!(
            app.draft_schedule(),
            None,
            "the value followed the reader onto another session's draft"
        );
        assert_eq!(app.overlay, None);
    }
    /// A snapshot carrying one send another client is holding for a session.
    fn held_send_snapshot(
        session_id: &VibexSessionId,
        message_id: u64,
        text: &str,
        scheduled_at_ms: i64,
    ) -> vibex_core::RemoteSidebarOrganizationSnapshot {
        vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 7,
            scheduled_sends: vec![vibex_core::RemoteScheduledSend {
                session_id: session_id.as_str().to_string(),
                message_id,
                text: text.to_string(),
                attachments: Vec::new(),
                scheduled_at_ms,
            }],
            ..Default::default()
        }
    }

    fn send_now_change(
        effects: &[Effect],
    ) -> (&vibex_core::RemoteSidebarOrganizationMutation, &Option<i64>) {
        let Some(Effect::MutateSidebarOrganization { mutation, .. }) = effects.first() else {
            panic!("no authority change was sent: {effects:?}");
        };
        let vibex_core::RemoteSidebarOrganizationMutation::SetScheduledSend {
            scheduled_at_ms, ..
        } = mutation
        else {
            panic!("the wrong change was sent: {mutation:?}");
        };
        (mutation, scheduled_at_ms)
    }

    #[test]
    fn the_clock_edits_the_send_another_client_holds() {
        // An empty draft is nothing to put an hour on. The send the session is
        // already waiting for is what the reader came to move, and it belongs
        // to whoever keeps its clock — this client arms nothing it cannot send.
        let mut app = scheduled_session_app();
        let session_id = app
            .agent
            .state
            .selected_session_id
            .clone()
            .expect("a session is open");
        let at_ms = vibex_core::unix_timestamp_ms() + 600_000;
        app.apply_sidebar_organization(&held_send_snapshot(
            &session_id,
            4,
            "send this later",
            at_ms,
        ));

        app.perform(Intent::ScheduleMessage);
        let Some(Overlay::Schedule(form)) = app.overlay.clone() else {
            panic!("the editor did not open");
        };
        assert_eq!(
            form.target,
            ScheduleEditTarget::Held {
                session_id: session_id.clone(),
                message_id: 4
            }
        );
        assert!(form.clearable, "the held send has an hour to clear");
        assert_eq!(
            app.draft_schedule(),
            None,
            "the draft is not the message being edited"
        );

        // A countdown is measured from the moment it is committed, and it
        // travels to the holder instead of arming anything here.
        app.perform(Intent::OverlayToggleValue);
        {
            let Some(Overlay::Schedule(form)) = app.overlay.as_mut() else {
                unreachable!("the editor is open");
            };
            form.hours = "0".to_string();
            form.minutes = "5".to_string();
            form.seconds = "0".to_string();
        }
        let outcome = app.perform(Intent::ConfirmOverlay);
        let (mutation, at_ms) = send_now_change(&outcome.effects);
        let vibex_core::RemoteSidebarOrganizationMutation::SetScheduledSend {
            session_id: changed,
            message_id,
            ..
        } = mutation
        else {
            unreachable!("checked by the helper");
        };
        assert_eq!(changed, session_id.as_str());
        assert_eq!(*message_id, 4);
        assert!(
            at_ms.is_some_and(|at| at > vibex_core::unix_timestamp_ms()),
            "a new deadline in the future"
        );
        assert_eq!(app.draft_schedule(), None);
        assert_eq!(
            app.composer.text(),
            "",
            "the held message is not the composer's"
        );
    }

    #[test]
    fn clearing_the_clock_holds_the_message_and_keeps_it_queued() {
        let mut app = scheduled_session_app();
        let session_id = app
            .agent
            .state
            .selected_session_id
            .clone()
            .expect("a session is open");
        app.apply_sidebar_organization(&held_send_snapshot(
            &session_id,
            9,
            "send this later",
            vibex_core::unix_timestamp_ms() + 600_000,
        ));

        app.perform(Intent::ScheduleMessage);
        let outcome = app.perform(Intent::ScheduleClear);
        let (mutation, at_ms) = send_now_change(&outcome.effects);
        assert_eq!(
            at_ms, &None,
            "clearing is a message that is no longer scheduled, not a send"
        );
        let vibex_core::RemoteSidebarOrganizationMutation::SetScheduledSend { message_id, .. } =
            mutation
        else {
            unreachable!("checked by the helper");
        };
        assert_eq!(*message_id, 9);
        assert_eq!(app.overlay, None);
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| toast.text == app.strings.schedule_cleared())
        );
    }

    #[test]
    fn send_now_is_a_deadline_of_now_for_the_holders_message() {
        let mut app = scheduled_session_app();
        let session_id = app
            .agent
            .state
            .selected_session_id
            .clone()
            .expect("a session is open");
        app.apply_sidebar_organization(&held_send_snapshot(
            &session_id,
            11,
            "send this later",
            vibex_core::unix_timestamp_ms() + 3_600_000,
        ));

        let outcome = app.perform(Intent::QueueSendNow);
        let (mutation, at_ms) = send_now_change(&outcome.effects);
        let vibex_core::RemoteSidebarOrganizationMutation::SetScheduledSend { message_id, .. } =
            mutation
        else {
            unreachable!("checked by the helper");
        };
        assert_eq!(*message_id, 11);
        let now_ms = vibex_core::unix_timestamp_ms();
        assert!(
            at_ms.is_some_and(|at| at <= now_ms),
            "the holder sends it as soon as it looks"
        );
        assert!(
            app.queued_messages.is_empty(),
            "another client's message is not this client's queue"
        );
    }

    #[test]
    fn a_draft_with_words_in_it_keeps_the_clock() {
        // Regression: the held send is only what the clock edits when there is
        // nothing written to send instead.
        let mut app = scheduled_session_app();
        let session_id = app
            .agent
            .state
            .selected_session_id
            .clone()
            .expect("a session is open");
        app.apply_sidebar_organization(&held_send_snapshot(
            &session_id,
            4,
            "theirs",
            vibex_core::unix_timestamp_ms() + 600_000,
        ));
        app.composer.insert_str("mine");

        app.perform(Intent::ScheduleMessage);
        let Some(Overlay::Schedule(form)) = app.overlay.clone() else {
            panic!("the editor did not open");
        };
        assert_eq!(
            form.target,
            ScheduleEditTarget::Draft(ComposerTarget::Session(session_id))
        );
        assert!(!form.clearable, "the draft had no schedule to clear");
    }

    #[test]
    fn a_send_the_authority_no_longer_publishes_is_not_edited() {
        // The holder sent it (or dropped it) between the frame the reader
        // looked at and the commit: there is no clock here to change, and
        // answering with a change nobody holds would promise a send that is
        // already gone.
        let mut app = scheduled_session_app();
        let session_id = app
            .agent
            .state
            .selected_session_id
            .clone()
            .expect("a session is open");
        let at_ms = vibex_core::unix_timestamp_ms() + 600_000;
        app.apply_sidebar_organization(&held_send_snapshot(&session_id, 4, "theirs", at_ms));
        app.perform(Intent::ScheduleMessage);
        app.apply_sidebar_organization(&vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 8,
            queued_sends: Vec::new(),
            paused_queue_session_ids: Vec::new(),
            manual_queue_session_ids: Vec::new(),
            scheduled_sends: Vec::new(),
            ..Default::default()
        });

        let outcome = app.perform(Intent::ConfirmOverlay);
        assert!(
            outcome.effects.is_empty(),
            "a change was sent for a send that is gone: {:?}",
            outcome.effects
        );
        assert_eq!(app.overlay, None);
    }
}
