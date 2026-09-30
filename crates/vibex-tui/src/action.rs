//! Every user-triggered action the TUI can perform.
//!
//! The enum is the vocabulary shared by three producers — the keymap, the
//! command palette, and in-page widgets such as the approval card — and one
//! consumer, [`crate::app::App::perform`]. Keeping one vocabulary is what makes
//! "the key bar cannot drift from the handler" true by construction.
//!
//! Variants are grouped by domain. Each one declares the scope that owns it and
//! the help text shown for it, so a new action is a single edit rather than
//! four.

use crate::keymap::Scope;

macro_rules! intents {
    ($( $variant:ident => { scope: $scope:ident, id: $id:expr, label: $label:expr, help: $help:expr } ),* $(,)?) => {
        /// A user-triggered action.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Intent {
            $($variant),*
        }

        impl Intent {
            /// Every intent, in declaration order.
            pub const ALL: &'static [Intent] = &[ $( Intent::$variant ),* ];

            /// Stable identifier used by the key-remap file and by tests.
            pub const fn id(self) -> &'static str {
                match self { $( Intent::$variant => $id ),* }
            }

            /// Scope that owns the intent by default.
            pub const fn default_scope(self) -> Scope {
                match self { $( Intent::$variant => Scope::$scope ),* }
            }

            /// Short label shown in the key bar and the help panel.
            pub const fn default_label(self) -> &'static str {
                match self { $( Intent::$variant => $label ),* }
            }

            /// One-line explanation shown in `?` help.
            pub const fn help(self) -> &'static str {
                match self { $( Intent::$variant => $help ),* }
            }
        }
    };
}

intents! {
    // ---- global ---------------------------------------------------------
    OpenCommandPalette => { scope: Global, id: "command_palette", label: "Commands", help: "Fuzzy-search every page and action." },
    ToggleHelp => { scope: Global, id: "help", label: "Help", help: "Show the keys and concepts for whatever has focus right now." },
    OpenSettings => { scope: Global, id: "open_settings", label: "Settings", help: "Theme, language, icons, colour and key bindings." },
    RequestQuit => { scope: Global, id: "quit", label: "Quit", help: "Leave the TUI after a confirmation." },
    Back => { scope: Global, id: "back", label: "Back", help: "Close the overlay, then the panel, then the page. Never quits." },
    FocusNext => { scope: Global, id: "focus_next", label: "Next pane", help: "Move focus between sidebar, main area, details and composer." },
    FocusPrevious => { scope: Global, id: "focus_previous", label: "Previous pane", help: "Move focus backwards." },
    Refresh => { scope: Global, id: "refresh", label: "Refresh", help: "Re-read the current page from the runtime." },
    ContextualCancel => { scope: Global, id: "cancel", label: "Cancel", help: "Clear the input, else interrupt the running turn, else offer to quit." },
    GotoSessions => { scope: Global, id: "goto_sessions", label: "Sessions", help: "Jump to the session list." },
    GotoManagement => { scope: Global, id: "goto_management", label: "Management", help: "Jump to the management index." },
    GotoUsage => { scope: Global, id: "goto_usage", label: "Usage", help: "Jump to token usage." },
    ToggleSidebar => { scope: Global, id: "toggle_sidebar", label: "Sidebar", help: "Collapse or show the sidebar." },
    ReloadKeymap => { scope: Global, id: "reload_keymap", label: "Reload keys", help: "Re-read ~/.vibex/tui-keys.toml." },

    // ---- list motion shared by every page --------------------------------
    SelectPrevious => { scope: Global, id: "select_previous", label: "Previous", help: "Move the selection up." },
    SelectNext => { scope: Global, id: "select_next", label: "Next", help: "Move the selection down." },
    ScrollPageUp => { scope: Global, id: "scroll_page_up", label: "Page up", help: "Scroll one page up." },
    ScrollPageDown => { scope: Global, id: "scroll_page_down", label: "Page down", help: "Scroll one page down." },
    ScrollHalfPageUp => { scope: Global, id: "scroll_half_up", label: "Half up", help: "Scroll half a page up." },
    ScrollHalfPageDown => { scope: Global, id: "scroll_half_down", label: "Half down", help: "Scroll half a page down." },
    ScrollToTop => { scope: Global, id: "scroll_top", label: "Top", help: "Jump to the first item." },
    ScrollToBottom => { scope: Global, id: "scroll_bottom", label: "Bottom", help: "Jump to the last item and resume following." },
    ShowDetails => { scope: Global, id: "show_details", label: "Details", help: "Open the details panel for the selection." },

    // ---- sessions -------------------------------------------------------
    OpenSelectedSession => { scope: Sessions, id: "session_open", label: "Open", help: "Open the selected session." },
    EnterSession => { scope: Sessions, id: "session_enter", label: "Enter", help: "Move into the session's workbench." },
    NewSession => { scope: Sessions, id: "session_new", label: "New", help: "Create a session in a workspace you pick." },
    BeginRenameSession => { scope: Sessions, id: "session_rename", label: "Rename", help: "Rename the selected session." },
    ForkSession => { scope: Sessions, id: "session_fork", label: "Fork", help: "Copy the session up to a chosen point." },
    ArchiveSession => { scope: Sessions, id: "session_archive", label: "Archive", help: "Archive the selected session." },
    DeleteSession => { scope: Sessions, id: "session_delete", label: "Delete", help: "Permanently delete the selected session." },
    ToggleShowArchived => { scope: Sessions, id: "session_show_archived", label: "Archived", help: "Include archived sessions in the list." },
    ToggleSessionCard => { scope: Sessions, id: "session_card_toggle", label: "Details", help: "Open or close the selected session's detail card." },
    CollapseSessionCards => { scope: Sessions, id: "session_card_collapse", label: "Close cards", help: "Close every open session detail card." },
    CopySessionRow => { scope: Sessions, id: "session_copy", label: "Copy", help: "Copy the selected session's details to the clipboard." },
    SwitchWorkspace => { scope: Sessions, id: "workspace_switch", label: "Workspace", help: "Change the workspace used for new sessions." },
    OpenWorkspaceBrowser => { scope: Sessions, id: "workspace_browse", label: "Browse", help: "List directories on the authority host." },
    WorkspaceBrowseUp => { scope: Sessions, id: "workspace_up", label: "Parent", help: "Go to the parent directory." },
    WorkspaceBrowseSelect => { scope: Sessions, id: "workspace_select", label: "Use directory", help: "Choose the highlighted directory." },

    // ---- agent transcript -----------------------------------------------
    FocusComposer => { scope: Agent, id: "composer_focus", label: "Write", help: "Put the cursor in the message composer." },
    ContinueTurn => { scope: Agent, id: "turn_continue", label: "Continue", help: "Ask the Agent to continue an unfinished turn." },
    ToggleBlockExpanded => { scope: Agent, id: "block_toggle", label: "Expand", help: "Expand or collapse the selected transcript block." },
    ToggleAllBlocksExpanded => { scope: Agent, id: "block_toggle_all", label: "Expand all", help: "Expand or collapse every collapsible block." },
    ToggleReasoningExpanded => { scope: Agent, id: "reasoning_toggle", label: "Reasoning", help: "Show or hide the Agent's reasoning." },
    CopyBlockBody => { scope: Agent, id: "block_copy", label: "Copy", help: "Copy the selected block's body to the clipboard." },
    CopyBlockMetadata => { scope: Agent, id: "block_copy_meta", label: "Copy meta", help: "Copy the command, path or id attached to the block." },
    OpenBlockDetails => { scope: Agent, id: "block_details", label: "Details", help: "Open the diff, terminal output or full text for the block." },
    PreviousPanel => { scope: Agent, id: "panel_previous", label: "Prev panel", help: "Move to the previous workbench panel." },
    NextPanel => { scope: Agent, id: "panel_next", label: "Next panel", help: "Move to the next workbench panel." },
    OpenChanges => { scope: Agent, id: "open_changes", label: "Changes", help: "Open the Git workbench for this session." },
    OpenFiles => { scope: Agent, id: "open_files", label: "Files", help: "Open the file tree for this session." },
    SwitchAgentRuntime => { scope: Agent, id: "runtime_switch", label: "Runtime", help: "Choose the Agent runtime and model for this session." },
    ProbeAgentRuntime => { scope: Agent, id: "runtime_probe", label: "Probe", help: "Ask the runtime to re-discover available Agents." },
    BeginTranscriptSearch => { scope: Agent, id: "transcript_search", label: "Find", help: "Search the transcript with a regular expression." },
    LoadOlderHistory => { scope: Agent, id: "history_older", label: "Older history", help: "Fetch the page of history above the oldest loaded one." },
    QueueSelectPrevious => { scope: Agent, id: "queue_previous", label: "Queue up", help: "Move the queue cursor to the message above." },
    QueueSelectNext => { scope: Agent, id: "queue_next", label: "Queue down", help: "Move the queue cursor to the message below." },
    QueueEditSelected => { scope: Agent, id: "queue_edit", label: "Queue edit", help: "Pull the queued message back into the composer." },
    QueueDeleteSelected => { scope: Agent, id: "queue_delete", label: "Queue drop", help: "Remove the queued message." },
    QueueMoveUp => { scope: Agent, id: "queue_move_up", label: "Queue raise", help: "Send the queued message one turn earlier." },
    QueueMoveDown => { scope: Agent, id: "queue_move_down", label: "Queue lower", help: "Send the queued message one turn later." },
    QueueSendNow => { scope: Agent, id: "queue_send_now", label: "Send now", help: "Interrupt the turn and send the queued message immediately." },
    SearchNext => { scope: Agent, id: "search_next", label: "Next match", help: "Jump to the next match, wrapping at the end." },
    SearchPrevious => { scope: Agent, id: "search_previous", label: "Prev match", help: "Jump to the previous match, wrapping at the start." },

    // ---- composer -------------------------------------------------------
    SubmitComposer => { scope: Composer, id: "composer_submit", label: "Send", help: "Send the draft as a new turn." },
    InsertNewline => { scope: Composer, id: "composer_newline", label: "Newline", help: "Insert a line break without sending." },
    ToggleMultiline => { scope: Composer, id: "composer_multiline", label: "Multiline", help: "Toggle multi-line editing." },
    ComposerHistoryPrevious => { scope: Composer, id: "composer_history_prev", label: "Prev sent", help: "Recall the previous message you sent." },
    ComposerHistoryNext => { scope: Composer, id: "composer_history_next", label: "Next sent", help: "Move forward through sent messages." },
    EditComposerExternally => { scope: Composer, id: "composer_editor", label: "$EDITOR", help: "Edit the draft in your editor, then return." },
    BackgroundRunningCommand => { scope: Composer, id: "composer_background", label: "Background", help: "Move the running foreground command to the background." },
    SteerRunningTurn => { scope: Composer, id: "composer_steer", label: "Steer", help: "Inject the draft into the running turn without interrupting it." },
    CompletionNext => { scope: Composer, id: "completion_next", label: "Next match", help: "Highlight the next completion." },
    CompletionPrevious => { scope: Composer, id: "completion_previous", label: "Prev match", help: "Highlight the previous completion." },
    CompletionAccept => { scope: Composer, id: "completion_accept", label: "Accept", help: "Insert the highlighted completion." },
    CompletionCancel => { scope: Composer, id: "completion_cancel", label: "Dismiss", help: "Close the completion menu." },
    DeleteWordBefore => { scope: Composer, id: "composer_delete_word", label: "Delete word", help: "Delete the word before the cursor." },
    DeleteWordAfter => { scope: Composer, id: "composer_delete_word_after", label: "Kill word", help: "Delete the word after the cursor into the kill buffer." },
    DeleteWordBackward => { scope: Composer, id: "composer_delete_word_backward", label: "Kill word back", help: "Delete the word before the cursor into the kill buffer." },
    KillToLineEnd => { scope: Composer, id: "composer_kill_to_end", label: "Kill to end", help: "Cut from the cursor to the end of the line." },
    KillToLineStart => { scope: Composer, id: "composer_kill_to_start", label: "Kill to start", help: "Cut from the start of the line to the cursor." },
    YankKill => { scope: Composer, id: "composer_yank", label: "Yank", help: "Put the last cut text back at the cursor." },
    ComposerUndo => { scope: Composer, id: "composer_undo", label: "Undo", help: "Undo the last edit to the draft." },
    ComposerRedo => { scope: Composer, id: "composer_redo", label: "Redo", help: "Redo an undone edit." },
    ComposerWordLeft => { scope: Composer, id: "composer_word_left", label: "Word left", help: "Move the cursor to the start of the previous word." },
    ComposerWordRight => { scope: Composer, id: "composer_word_right", label: "Word right", help: "Move the cursor past the end of the next word." },
    ComposerLineStart => { scope: Composer, id: "composer_line_start", label: "Line start", help: "Move the cursor to the start of the line." },
    ComposerLineEnd => { scope: Composer, id: "composer_line_end", label: "Line end", help: "Move the cursor to the end of the line." },

    // ---- overlays -------------------------------------------------------
    CloseOverlay => { scope: Overlay, id: "overlay_close", label: "Close", help: "Dismiss this overlay without applying anything." },
    ConfirmOverlay => { scope: Overlay, id: "overlay_confirm", label: "Confirm", help: "Apply this overlay's value." },
    OverlayNextField => { scope: Overlay, id: "overlay_next_field", label: "Next field", help: "Move to the next form field." },
    OverlayPreviousField => { scope: Overlay, id: "overlay_prev_field", label: "Prev field", help: "Move to the previous form field." },
    OverlayToggleValue => { scope: Overlay, id: "overlay_toggle", label: "Toggle", help: "Flip a boolean field or a multi-select option." },
    PaletteRun => { scope: Overlay, id: "palette_run", label: "Run", help: "Run the highlighted command." },

    // ---- approvals ------------------------------------------------------
    ApprovalApprove => { scope: Overlay, id: "approval_approve", label: "Allow", help: "Approve the pending request." },
    ApprovalDeny => { scope: Overlay, id: "approval_deny", label: "Deny", help: "Refuse the pending request." },
    ApprovalAlways => { scope: Overlay, id: "approval_always", label: "Always", help: "Approve and remember for the rest of this session." },
    ApprovalFocusNext => { scope: Overlay, id: "approval_next", label: "Next option", help: "Move between the request's allowed responses." },
    ElicitationSubmit => { scope: Overlay, id: "elicitation_submit", label: "Submit", help: "Send the answers back to the Agent." },
    ElicitationFieldNext => { scope: Overlay, id: "elicitation_next", label: "Next field", help: "Move to the next question." },
    ElicitationFieldPrevious => { scope: Overlay, id: "elicitation_prev", label: "Prev field", help: "Move to the previous question." },

    // ---- files ----------------------------------------------------------
    OpenSelectedFile => { scope: Files, id: "file_open", label: "Open", help: "Show the file's contents read-only." },
    EditSelectedFile => { scope: Files, id: "file_edit", label: "$EDITOR", help: "Hand the file to your editor." },
    ToggleFileTreeExpanded => { scope: Files, id: "file_toggle", label: "Expand", help: "Expand or collapse the highlighted directory." },
    FileSearch => { scope: Files, id: "file_search", label: "Search", help: "Search file names in the workspace." },

    // ---- changes / git --------------------------------------------------
    GitStageSelected => { scope: Changes, id: "git_stage", label: "Stage", help: "Stage the highlighted path." },
    GitUnstageSelected => { scope: Changes, id: "git_unstage", label: "Unstage", help: "Unstage the highlighted path." },
    GitCommit => { scope: Changes, id: "git_commit", label: "Commit", help: "Commit the staged changes; the message opens in $EDITOR." },
    GitRevert => { scope: Changes, id: "git_revert", label: "Revert", help: "Discard the highlighted change after confirmation." },
    ShowDiff => { scope: Changes, id: "git_diff", label: "Diff", help: "Show the diff for the highlighted path." },
    GitHistory => { scope: Changes, id: "git_history", label: "History", help: "Browse recent commits." },
    GitBranches => { scope: Changes, id: "git_branches", label: "Branches", help: "List and switch branches." },
    WorktreeMenu => { scope: Changes, id: "worktree_menu", label: "Worktrees", help: "Create, archive, restore or discard a worktree." },
    WorktreeCreate => { scope: Changes, id: "worktree_create", label: "Create", help: "Create a worktree for the selected branch." },
    WorktreePreflight => { scope: Changes, id: "worktree_preflight", label: "Preflight", help: "Run the preflight check a worktree action requires." },

    // ---- terminal -------------------------------------------------------
    NewTerminal => { scope: Terminal, id: "terminal_new", label: "New", help: "Open a runtime terminal in this session." },
    CloseTerminal => { scope: Terminal, id: "terminal_close", label: "Close", help: "Close the current runtime terminal." },
    TerminalToggleFollow => { scope: Terminal, id: "terminal_follow", label: "Follow", help: "Follow or pause terminal output." },

    // ---- management -----------------------------------------------------
    OpenManagementSection => { scope: Management, id: "management_open", label: "Open", help: "Open the highlighted management section." },
    InstallOrUpdateAgent => { scope: Management, id: "management_agent_install", label: "Install", help: "Install, update or roll back the highlighted Agent." },
    UninstallAgent => { scope: Management, id: "management_agent_uninstall", label: "Uninstall", help: "Uninstall the highlighted Agent." },
    AgentAuthMenu => { scope: Management, id: "management_agent_auth", label: "Sign in", help: "Open the Agent's authentication options." },
    AgentAuthRefresh => { scope: Management, id: "management_agent_auth_refresh", label: "Re-probe", help: "Re-read the Agent's advertised authentication methods." },
    AgentLogout => { scope: Management, id: "management_agent_logout", label: "Sign out", help: "Release the stored credentials for this Agent." },
    ToggleSelectedEntry => { scope: Management, id: "management_toggle", label: "Toggle", help: "Enable or disable the highlighted entry." },
    EditSelectedEntry => { scope: Management, id: "management_edit", label: "Edit", help: "Edit the highlighted entry." },
    ReloadManagement => { scope: Management, id: "management_reload", label: "Reload", help: "Re-read this management section." },

    // ---- providers ------------------------------------------------------
    ActivateProviderProfile => { scope: Providers, id: "provider_activate", label: "Use", help: "Make the highlighted profile the active one." },
    EditProviderProfile => { scope: Providers, id: "provider_edit", label: "Rename", help: "Rename the highlighted profile." },
    EditProviderProjection => { scope: Providers, id: "provider_projection", label: "Settings", help: "Edit the profile's endpoint and model settings." },
    EditProviderSecret => { scope: Providers, id: "provider_secret", label: "Credential", help: "Write a new credential. Stored values are never shown." },
    TestProviderProfile => { scope: Providers, id: "provider_test", label: "Test", help: "Run a connectivity check against the profile." },
    FetchProviderModels => { scope: Providers, id: "provider_models", label: "Models", help: "Ask the provider which models it offers." },
    ProviderHealth => { scope: Providers, id: "provider_health", label: "Health", help: "Show the provider health report." },

    // ---- devices --------------------------------------------------------
    CreatePairingCode => { scope: Devices, id: "device_pair", label: "Pair", help: "Issue a one-time pairing code and link." },
    RevokeSelectedDevice => { scope: Devices, id: "device_revoke", label: "Revoke", help: "Revoke the highlighted device's access." },
    OpenDeviceAudit => { scope: Devices, id: "device_audit", label: "Audit", help: "Show the remote-protocol audit trail." },

    // ---- usage ----------------------------------------------------------
    UsageSessionScope => { scope: Usage, id: "usage_scope", label: "Scope", help: "Switch between this session and the aggregate." },

    // ---- recovery -------------------------------------------------------
    ActivateRecoveryAction => { scope: Recovery, id: "recovery_run", label: "Run", help: "Run the highlighted recovery action." },
    ExportDiagnostics => { scope: Recovery, id: "recovery_diagnostics", label: "Diagnostics", help: "Export a diagnostics bundle on the authority host." },
    CreateBackup => { scope: Recovery, id: "recovery_backup", label: "Back up", help: "Create a database backup on the authority host." },
    InspectBackup => { scope: Recovery, id: "recovery_inspect", label: "Inspect", help: "Read a backup's manifest without restoring it." },
    RestoreBackup => { scope: Recovery, id: "recovery_restore", label: "Restore", help: "Restore a backup. Destructive, and doubly confirmed." },

    // ---- settings -------------------------------------------------------
    ActivateSetting => { scope: Settings, id: "setting_activate", label: "Change", help: "Change the highlighted setting." },
    SettingPrevious => { scope: Settings, id: "setting_previous", label: "Previous value", help: "Step the setting backwards." },
    SettingNext => { scope: Settings, id: "setting_next", label: "Next value", help: "Step the setting forwards." },
    ResetSetting => { scope: Settings, id: "setting_reset", label: "Reset", help: "Reset the highlighted setting to its default after a confirmation." },

    // ---- filters (shared) ------------------------------------------------
    BeginFilter => { scope: Global, id: "filter_begin", label: "Filter", help: "Type to filter the current list." },
    ClearFilter => { scope: Global, id: "filter_clear", label: "Clear filter", help: "Remove the active filter." },
}

impl Intent {
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|intent| intent.id().eq_ignore_ascii_case(id.trim()))
    }

    /// Whether the intent changes state and therefore needs a live connection.
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Intent::SubmitComposer
                | Intent::ContinueTurn
                | Intent::ApprovalApprove
                | Intent::ApprovalDeny
                | Intent::ApprovalAlways
                | Intent::ElicitationSubmit
                | Intent::NewSession
                | Intent::BeginRenameSession
                | Intent::ForkSession
                | Intent::ArchiveSession
                | Intent::DeleteSession
                | Intent::GitStageSelected
                | Intent::GitUnstageSelected
                | Intent::GitCommit
                | Intent::GitRevert
                | Intent::CreatePairingCode
                | Intent::RevokeSelectedDevice
                | Intent::CreateBackup
                | Intent::RestoreBackup
                | Intent::ExportDiagnostics
                | Intent::InstallOrUpdateAgent
                | Intent::UninstallAgent
                | Intent::AgentLogout
                | Intent::ActivateProviderProfile
                | Intent::EditProviderSecret
                | Intent::EditProviderProjection
                | Intent::ToggleSelectedEntry
                | Intent::EditSelectedEntry
                | Intent::WorktreeCreate
                | Intent::SteerRunningTurn
                | Intent::BackgroundRunningCommand
                | Intent::NewTerminal
                | Intent::CloseTerminal
                | Intent::SwitchAgentRuntime
        )
    }

    /// Whether the intent is meaningful while the connection is down.
    pub const fn works_offline(self) -> bool {
        matches!(
            self,
            Intent::ToggleHelp
                | Intent::OpenSettings
                | Intent::OpenCommandPalette
                | Intent::Back
                | Intent::FocusNext
                | Intent::FocusPrevious
                | Intent::GotoSessions
                | Intent::GotoManagement
                | Intent::GotoUsage
                | Intent::SelectPrevious
                | Intent::SelectNext
                | Intent::ScrollPageUp
                | Intent::ScrollPageDown
                | Intent::ScrollHalfPageUp
                | Intent::ScrollHalfPageDown
                | Intent::ScrollToTop
                | Intent::ScrollToBottom
                | Intent::ToggleBlockExpanded
                | Intent::ToggleAllBlocksExpanded
                | Intent::ToggleReasoningExpanded
                | Intent::ToggleSessionCard
                | Intent::CollapseSessionCards
                | Intent::CopySessionRow
                | Intent::BeginTranscriptSearch
                | Intent::SearchNext
                | Intent::SearchPrevious
                | Intent::QueueSelectPrevious
                | Intent::QueueSelectNext
                | Intent::QueueEditSelected
                | Intent::QueueDeleteSelected
                | Intent::QueueMoveUp
                | Intent::QueueMoveDown
                | Intent::CopyBlockBody
                | Intent::CopyBlockMetadata
                | Intent::OpenBlockDetails
                | Intent::PreviousPanel
                | Intent::NextPanel
                | Intent::ToggleSidebar
                | Intent::ReloadKeymap
                | Intent::CloseOverlay
                | Intent::RequestQuit
                | Intent::ContextualCancel
                | Intent::FocusComposer
                | Intent::InsertNewline
                | Intent::ToggleMultiline
                | Intent::ComposerHistoryPrevious
                | Intent::ComposerHistoryNext
                | Intent::CompletionNext
                | Intent::CompletionPrevious
                | Intent::CompletionAccept
                | Intent::CompletionCancel
                | Intent::DeleteWordBefore
                | Intent::DeleteWordAfter
                | Intent::DeleteWordBackward
                | Intent::KillToLineEnd
                | Intent::KillToLineStart
                | Intent::YankKill
                | Intent::ComposerUndo
                | Intent::ComposerRedo
                | Intent::ComposerWordLeft
                | Intent::ComposerWordRight
                | Intent::ComposerLineStart
                | Intent::ComposerLineEnd
                | Intent::BeginFilter
                | Intent::ClearFilter
                | Intent::ActivateSetting
                | Intent::SettingPrevious
                | Intent::SettingNext
                | Intent::ResetSetting
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_unique() {
        let mut seen = BTreeSet::new();
        for intent in Intent::ALL {
            assert!(seen.insert(intent.id()), "duplicate id {}", intent.id());
        }
    }

    #[test]
    fn labels_and_help_are_present() {
        for intent in Intent::ALL {
            assert!(!intent.default_label().is_empty(), "{}", intent.id());
            assert!(!intent.help().is_empty(), "{}", intent.id());
            assert!(
                intent.help().ends_with('.'),
                "{} help should be a sentence",
                intent.id()
            );
        }
    }

    #[test]
    fn every_intent_round_trips_through_its_id() {
        for intent in Intent::ALL {
            assert_eq!(Intent::from_id(intent.id()), Some(*intent));
        }
        assert_eq!(Intent::from_id("  QUIT "), Some(Intent::RequestQuit));
        assert_eq!(Intent::from_id("nope"), None);
    }

    #[test]
    fn mutating_intents_never_claim_to_work_offline() {
        for intent in Intent::ALL {
            if intent.mutates() {
                assert!(
                    !intent.works_offline(),
                    "{} mutates but claims to work offline",
                    intent.id()
                );
            }
        }
    }
}
