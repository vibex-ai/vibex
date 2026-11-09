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
use crate::locale::Strings;

macro_rules! intents {
    ($( $variant:ident => { scope: $scope:ident, id: $id:expr, label: $label:ident, help: $help:ident } ),* $(,)?) => {
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

            /// Short label shown in the key bar and the help panel, in the
            /// language this session resolved to.
            ///
            /// The words live in the locale table rather than beside the action,
            /// so the three languages cannot drift and a new action is a compile
            /// error until all of them name it.
            pub fn label(self, strings: Strings) -> &'static str {
                match self { $( Intent::$variant => strings.$label() ),* }
            }

            /// One-line explanation shown in `?` help, in the reader's language.
            pub fn help(self, strings: Strings) -> &'static str {
                match self { $( Intent::$variant => strings.$help() ),* }
            }

            /// [`Self::label`] as a plain function pointer.
            ///
            /// A binding carries the way to name its row rather than the name,
            /// so the same action can be drawn in whichever language the
            /// session resolved to; a `fn` is what fits in a `const` table.
            pub const fn label_fn(self) -> fn(Strings) -> &'static str {
                match self { $( Intent::$variant => |strings| strings.$label() ),* }
            }
        }
    };
}

intents! {
    // ---- global ---------------------------------------------------------
    OpenCommandPalette => { scope: Global, id: "command_palette", label: label_command_palette, help: help_command_palette },
    ToggleHelp => { scope: Global, id: "help", label: label_help, help: help_help },
    OpenSettings => { scope: Global, id: "open_settings", label: label_open_settings, help: help_open_settings },
    RequestQuit => { scope: Global, id: "quit", label: label_quit, help: help_quit },
    Back => { scope: Global, id: "back", label: label_back, help: help_back },
    FocusNext => { scope: Global, id: "focus_next", label: label_focus_next, help: help_focus_next },
    FocusPrevious => { scope: Global, id: "focus_previous", label: label_focus_previous, help: help_focus_previous },
    Refresh => { scope: Global, id: "refresh", label: label_refresh, help: help_refresh },
    ContextualCancel => { scope: Global, id: "cancel", label: label_cancel, help: help_cancel },
    GotoSessions => { scope: Global, id: "goto_sessions", label: label_goto_sessions, help: help_goto_sessions },
    GotoManagement => { scope: Global, id: "goto_management", label: label_goto_management, help: help_goto_management },
    ToggleSidebar => { scope: Global, id: "toggle_sidebar", label: label_toggle_sidebar, help: help_toggle_sidebar },
    ReloadKeymap => { scope: Global, id: "reload_keymap", label: label_reload_keymap, help: help_reload_keymap },
    TakeOverHome => { scope: Global, id: "take_over_home", label: label_take_over_home, help: help_take_over_home },

    // ---- list motion shared by every page --------------------------------
    SelectPrevious => { scope: Global, id: "select_previous", label: label_select_previous, help: help_select_previous },
    SelectNext => { scope: Global, id: "select_next", label: label_select_next, help: help_select_next },
    ScrollPageUp => { scope: Global, id: "scroll_page_up", label: label_scroll_page_up, help: help_scroll_page_up },
    ScrollPageDown => { scope: Global, id: "scroll_page_down", label: label_scroll_page_down, help: help_scroll_page_down },
    ScrollHalfPageUp => { scope: Global, id: "scroll_half_up", label: label_scroll_half_up, help: help_scroll_half_up },
    ScrollHalfPageDown => { scope: Global, id: "scroll_half_down", label: label_scroll_half_down, help: help_scroll_half_down },
    ScrollToTop => { scope: Global, id: "scroll_top", label: label_scroll_top, help: help_scroll_top },
    ScrollToBottom => { scope: Global, id: "scroll_bottom", label: label_scroll_bottom, help: help_scroll_bottom },
    ShowDetails => { scope: Global, id: "show_details", label: label_show_details, help: help_show_details },

    // ---- sessions -------------------------------------------------------
    OpenSelectedSession => { scope: Sessions, id: "session_open", label: label_session_open, help: help_session_open },
    SessionControlPrevious => { scope: Sessions, id: "session_control_previous", label: label_session_control_previous, help: help_session_control_previous },
    SessionControlNext => { scope: Sessions, id: "session_control_next", label: label_session_control_next, help: help_session_control_next },
    NewSession => { scope: Sessions, id: "session_new", label: label_session_new, help: help_session_new },
    BeginRenameSession => { scope: Sessions, id: "session_rename", label: label_session_rename, help: help_session_rename },
    ForkSession => { scope: Sessions, id: "session_fork", label: label_session_fork, help: help_session_fork },
    ArchiveSession => { scope: Sessions, id: "session_archive", label: label_session_archive, help: help_session_archive },
    UnarchiveSession => { scope: Sessions, id: "session_unarchive", label: label_session_unarchive, help: help_session_unarchive },
    DeleteSession => { scope: Sessions, id: "session_delete", label: label_session_delete, help: help_session_delete },
    ToggleShowArchived => { scope: Sessions, id: "session_show_archived", label: label_session_show_archived, help: help_session_show_archived },
    ToggleSessionCard => { scope: Sessions, id: "session_card_toggle", label: label_session_card_toggle, help: help_session_card_toggle },
    CollapseSessionCards => { scope: Sessions, id: "session_card_collapse", label: label_session_card_collapse, help: help_session_card_collapse },
    SidebarRowActions => { scope: Sessions, id: "sidebar_row_actions", label: label_sidebar_row_actions, help: help_sidebar_row_actions },
    ToggleChildBranch => { scope: Sessions, id: "session_children_toggle", label: label_session_children_toggle, help: help_session_children_toggle },
    CopySessionRow => { scope: Sessions, id: "session_copy", label: label_session_copy, help: help_session_copy },
    PinSession => { scope: Sessions, id: "session_pin", label: label_session_pin, help: help_session_pin },
    ToggleAutoContinue => { scope: Sessions, id: "session_auto_continue", label: label_session_auto_continue, help: help_session_auto_continue },
    MoveSessionUp => { scope: Sessions, id: "session_move_up", label: label_session_move_up, help: help_session_move_up },
    MoveSessionDown => { scope: Sessions, id: "session_move_down", label: label_session_move_down, help: help_session_move_down },
    SwitchWorkspace => { scope: Sessions, id: "workspace_switch", label: label_workspace_switch, help: help_workspace_switch },
    OpenWorkspaceBrowser => { scope: Sessions, id: "workspace_browse", label: label_workspace_browse, help: help_workspace_browse },
    WorkspaceBrowseUp => { scope: Sessions, id: "workspace_up", label: label_workspace_up, help: help_workspace_up },
    WorkspaceBrowseSelect => { scope: Sessions, id: "workspace_select", label: label_workspace_select, help: help_workspace_select },

    // ---- agent transcript -----------------------------------------------
    FocusComposer => { scope: Agent, id: "composer_focus", label: label_composer_focus, help: help_composer_focus },
    ContinueTurn => { scope: Agent, id: "turn_continue", label: label_turn_continue, help: help_turn_continue },
    ToggleBlockExpanded => { scope: Agent, id: "block_toggle", label: label_block_toggle, help: help_block_toggle },
    ToggleAllBlocksExpanded => { scope: Agent, id: "block_toggle_all", label: label_block_toggle_all, help: help_block_toggle_all },
    ToggleReasoningExpanded => { scope: Agent, id: "reasoning_toggle", label: label_reasoning_toggle, help: help_reasoning_toggle },
    CopyBlockBody => { scope: Agent, id: "block_copy", label: label_block_copy, help: help_block_copy },
    CopyBlockMetadata => { scope: Agent, id: "block_copy_meta", label: label_block_copy_meta, help: help_block_copy_meta },
    OpenBlockDetails => { scope: Agent, id: "block_details", label: label_block_details, help: help_block_details },
    SwitchAgentRuntime => { scope: Agent, id: "runtime_switch", label: label_runtime_switch, help: help_runtime_switch },
    ProbeAgentRuntime => { scope: Agent, id: "runtime_probe", label: label_runtime_probe, help: help_runtime_probe },
    BeginTranscriptSearch => { scope: Agent, id: "transcript_search", label: label_transcript_search, help: help_transcript_search },
    ToggleDock => { scope: Agent, id: "dock_toggle", label: label_dock_toggle, help: help_dock_toggle },
    DockActivate => { scope: Agent, id: "dock_activate", label: label_dock_activate, help: help_dock_activate },
    OpenChildSessions => { scope: Agent, id: "session_children_open", label: label_session_children_open, help: help_session_children_open },
    GotoParentSession => { scope: Agent, id: "session_parent", label: label_session_parent, help: help_session_parent },
    DockHideDone => { scope: Agent, id: "dock_hide_done", label: label_dock_hide_done, help: help_dock_hide_done },
    LoadOlderHistory => { scope: Agent, id: "history_older", label: label_history_older, help: help_history_older },
    QueueSelectPrevious => { scope: Agent, id: "queue_previous", label: label_queue_previous, help: help_queue_previous },
    QueueSelectNext => { scope: Agent, id: "queue_next", label: label_queue_next, help: help_queue_next },
    QueueEditSelected => { scope: Agent, id: "queue_edit", label: label_queue_edit, help: help_queue_edit },
    QueueDeleteSelected => { scope: Agent, id: "queue_delete", label: label_queue_delete, help: help_queue_delete },
    QueueMoveUp => { scope: Agent, id: "queue_move_up", label: label_queue_move_up, help: help_queue_move_up },
    QueueMoveDown => { scope: Agent, id: "queue_move_down", label: label_queue_move_down, help: help_queue_move_down },
    QueueSendNow => { scope: Agent, id: "queue_send_now", label: label_queue_send_now, help: help_queue_send_now },
    ScheduleMessage => { scope: Agent, id: "message_schedule", label: label_message_schedule, help: help_message_schedule },
    SearchNext => { scope: Agent, id: "search_next", label: label_search_next, help: help_search_next },
    SearchPrevious => { scope: Agent, id: "search_previous", label: label_search_previous, help: help_search_previous },

    // ---- composer -------------------------------------------------------
    SubmitComposer => { scope: Composer, id: "composer_submit", label: label_composer_submit, help: help_composer_submit },
    InsertNewline => { scope: Composer, id: "composer_newline", label: label_composer_newline, help: help_composer_newline },
    ToggleMultiline => { scope: Composer, id: "composer_multiline", label: label_composer_multiline, help: help_composer_multiline },
    ComposerHistoryPrevious => { scope: Composer, id: "composer_history_prev", label: label_composer_history_prev, help: help_composer_history_prev },
    ComposerHistoryNext => { scope: Composer, id: "composer_history_next", label: label_composer_history_next, help: help_composer_history_next },
    EditComposerExternally => { scope: Composer, id: "composer_editor", label: label_composer_editor, help: help_composer_editor },
    BackgroundRunningCommand => { scope: Composer, id: "composer_background", label: label_composer_background, help: help_composer_background },
    SteerRunningTurn => { scope: Composer, id: "composer_steer", label: label_composer_steer, help: help_composer_steer },
    CompletionNext => { scope: Composer, id: "completion_next", label: label_completion_next, help: help_completion_next },
    CompletionPrevious => { scope: Composer, id: "completion_previous", label: label_completion_previous, help: help_completion_previous },
    CompletionAccept => { scope: Composer, id: "completion_accept", label: label_completion_accept, help: help_completion_accept },
    CompletionCancel => { scope: Composer, id: "completion_cancel", label: label_completion_cancel, help: help_completion_cancel },
    DeleteWordBefore => { scope: Composer, id: "composer_delete_word", label: label_composer_delete_word, help: help_composer_delete_word },
    DeleteWordAfter => { scope: Composer, id: "composer_delete_word_after", label: label_composer_delete_word_after, help: help_composer_delete_word_after },
    DeleteWordBackward => { scope: Composer, id: "composer_delete_word_backward", label: label_composer_delete_word_backward, help: help_composer_delete_word_backward },
    KillToLineEnd => { scope: Composer, id: "composer_kill_to_end", label: label_composer_kill_to_end, help: help_composer_kill_to_end },
    KillToLineStart => { scope: Composer, id: "composer_kill_to_start", label: label_composer_kill_to_start, help: help_composer_kill_to_start },
    YankKill => { scope: Composer, id: "composer_yank", label: label_composer_yank, help: help_composer_yank },
    ComposerUndo => { scope: Composer, id: "composer_undo", label: label_composer_undo, help: help_composer_undo },
    ComposerRedo => { scope: Composer, id: "composer_redo", label: label_composer_redo, help: help_composer_redo },
    ComposerWordLeft => { scope: Composer, id: "composer_word_left", label: label_composer_word_left, help: help_composer_word_left },
    ComposerWordRight => { scope: Composer, id: "composer_word_right", label: label_composer_word_right, help: help_composer_word_right },
    ComposerLineStart => { scope: Composer, id: "composer_line_start", label: label_composer_line_start, help: help_composer_line_start },
    ComposerLineEnd => { scope: Composer, id: "composer_line_end", label: label_composer_line_end, help: help_composer_line_end },
    AttachImage => { scope: Composer, id: "composer_image", label: label_composer_image, help: help_composer_image },
    PasteClipboard => { scope: Composer, id: "composer_paste", label: label_composer_paste, help: help_composer_paste },
    SelectAllDraft => { scope: Composer, id: "composer_select_all", label: label_composer_select_all, help: help_composer_select_all },
    CopyDraftSelection => { scope: Composer, id: "composer_copy_draft", label: label_composer_copy_draft, help: help_composer_copy_draft },

    // ---- overlays -------------------------------------------------------
    CloseOverlay => { scope: Overlay, id: "overlay_close", label: label_overlay_close, help: help_overlay_close },
    ConfirmOverlay => { scope: Overlay, id: "overlay_confirm", label: label_overlay_confirm, help: help_overlay_confirm },
    OverlayNextField => { scope: Overlay, id: "overlay_next_field", label: label_overlay_next_field, help: help_overlay_next_field },
    OverlayPreviousField => { scope: Overlay, id: "overlay_prev_field", label: label_overlay_prev_field, help: help_overlay_prev_field },
    OverlayToggleValue => { scope: Overlay, id: "overlay_toggle", label: label_overlay_toggle, help: help_overlay_toggle },
    ScheduleClear => { scope: Overlay, id: "schedule_clear", label: label_schedule_clear, help: help_schedule_clear },
    PaletteRun => { scope: Overlay, id: "palette_run", label: label_palette_run, help: help_palette_run },

    // ---- the runtime switcher -------------------------------------------
    // These live in the overlay scope rather than a switcher-only one because
    // every other overlay ignores what it does not use, and one shared scope is
    // what keeps the switcher's keys remappable from the same file.
    OverlayFoldOpen => { scope: Overlay, id: "overlay_fold_open", label: label_overlay_fold_open, help: help_overlay_fold_open },
    OverlayFoldClosed => { scope: Overlay, id: "overlay_fold_close", label: label_overlay_fold_close, help: help_overlay_fold_close },
    StarRuntimeModel => { scope: Overlay, id: "overlay_star_model", label: label_overlay_star_model, help: help_overlay_star_model },
    ManageRuntimeAccount => { scope: Overlay, id: "overlay_manage_account", label: label_overlay_manage_account, help: help_overlay_manage_account },
    ResetRunOption => { scope: Overlay, id: "overlay_reset_run_option", label: label_overlay_reset_run_option, help: help_overlay_reset_run_option },

    // ---- approvals ------------------------------------------------------
    ApprovalApprove => { scope: Overlay, id: "approval_approve", label: label_approval_approve, help: help_approval_approve },
    ApprovalDeny => { scope: Overlay, id: "approval_deny", label: label_approval_deny, help: help_approval_deny },
    ApprovalAlways => { scope: Overlay, id: "approval_always", label: label_approval_always, help: help_approval_always },
    ApprovalFocusNext => { scope: Overlay, id: "approval_next", label: label_approval_next, help: help_approval_next },
    ElicitationSubmit => { scope: Overlay, id: "elicitation_submit", label: label_elicitation_submit, help: help_elicitation_submit },
    ElicitationFieldNext => { scope: Overlay, id: "elicitation_next", label: label_elicitation_next, help: help_elicitation_next },
    ElicitationFieldPrevious => { scope: Overlay, id: "elicitation_prev", label: label_elicitation_prev, help: help_elicitation_prev },




    // ---- management -----------------------------------------------------
    ToggleSelectedEntry => { scope: Management, id: "management_toggle", label: label_management_toggle, help: help_management_toggle },
    EditSelectedEntry => { scope: Management, id: "management_edit", label: label_management_edit, help: help_management_edit },
    ReloadManagement => { scope: Management, id: "management_reload", label: label_management_reload, help: help_management_reload },

    // ---- configuration centre -------------------------------------------
    ConfigModulePrevious => { scope: Global, id: "config_module_previous", label: label_config_module_previous, help: help_config_module_previous },
    ConfigModuleNext => { scope: Global, id: "config_module_next", label: label_config_module_next, help: help_config_module_next },
    ConfigSectionPrevious => { scope: Management, id: "config_section_previous", label: label_config_section_previous, help: help_config_section_previous },
    ConfigSectionNext => { scope: Management, id: "config_section_next", label: label_config_section_next, help: help_config_section_next },
    ConfigPaneNext => { scope: Management, id: "config_pane_next", label: label_config_pane_next, help: help_config_pane_next },
    ConfigPanePrevious => { scope: Management, id: "config_pane_previous", label: label_config_pane_previous, help: help_config_pane_previous },
    ConfigEntryActions => { scope: Management, id: "config_entry_actions", label: label_config_entry_actions, help: help_config_entry_actions },
    ConfigEntryCreate => { scope: Management, id: "config_entry_create", label: label_config_entry_create, help: help_config_entry_create },
    ConfigEntryDelete => { scope: Management, id: "config_entry_delete", label: label_config_entry_delete, help: help_config_entry_delete },
    ConfigFormSave => { scope: Overlay, id: "config_form_save", label: label_config_form_save, help: help_config_form_save },
    ConfigFormCopy => { scope: Overlay, id: "config_form_copy", label: label_config_form_copy, help: help_config_form_copy },
    ConfigFormDiscard => { scope: Overlay, id: "config_form_discard", label: label_config_form_discard, help: help_config_form_discard },
    ConfigModels => { scope: Management, id: "config_models", label: label_config_models, help: help_config_models },
    ConfigImport => { scope: Management, id: "config_import", label: label_config_import, help: help_config_import },
    ConfigExport => { scope: Management, id: "config_export", label: label_config_export, help: help_config_export },
    ConfigDiscover => { scope: Management, id: "config_discover", label: label_config_discover, help: help_config_discover },
    ConfigModelToggle => { scope: Overlay, id: "config_model_toggle", label: label_config_model_toggle, help: help_config_model_toggle },
    ConfigModelAdd => { scope: Overlay, id: "config_model_add", label: label_config_model_add, help: help_config_model_add },
    ConfigModelRemove => { scope: Overlay, id: "config_model_remove", label: label_config_model_remove, help: help_config_model_remove },
    ConfigModelFetch => { scope: Overlay, id: "config_model_fetch", label: label_config_model_fetch, help: help_config_model_fetch },
    ConfigModelEdit => { scope: Overlay, id: "config_model_edit", label: label_config_model_edit, help: help_config_model_edit },
    ConfigModelEnableAll => { scope: Overlay, id: "config_model_enable_all", label: label_config_model_enable_all, help: help_config_model_enable_all },
    ConfigModelDisableAll => { scope: Overlay, id: "config_model_disable_all", label: label_config_model_disable_all, help: help_config_model_disable_all },
    ConfigImportRefresh => { scope: Overlay, id: "config_import_refresh", label: label_management_reload, help: help_config_import },
    ConfigExportSource => { scope: Overlay, id: "config_export_source", label: label_config_export, help: help_config_export },
    ConfigExportMode => { scope: Overlay, id: "config_export_mode", label: label_config_export, help: help_config_export },
    ConfigExportPreview => { scope: Overlay, id: "config_export_preview", label: label_config_export, help: help_config_export },
    ConfigExportApply => { scope: Overlay, id: "config_export_apply", label: label_config_export, help: help_config_export },
    ConfigExportRollback => { scope: Overlay, id: "config_export_rollback", label: label_config_export, help: help_config_export },
    ConfigRegistryFilter => { scope: Management, id: "config_registry_filter", label: label_config_registry_filter, help: help_config_registry_filter },
    ConfigRegistrySort => { scope: Management, id: "config_registry_sort", label: label_config_registry_sort, help: help_config_registry_sort },
    ConfigRegistryPagePrevious => { scope: Management, id: "config_registry_page_previous", label: label_config_registry_page_previous, help: help_config_registry_page_previous },
    ConfigRegistryPageNext => { scope: Management, id: "config_registry_page_next", label: label_config_registry_page_next, help: help_config_registry_page_next },
    GotoDevices => { scope: Global, id: "goto_devices", label: label_goto_devices, help: help_goto_devices },

    // ---- providers ------------------------------------------------------

    // ---- devices --------------------------------------------------------
    ActivateDevicePairing => { scope: Devices, id: "device_pair_activate", label: label_device_pair_activate, help: help_device_pair_activate },
    DevicesTabNext => { scope: Devices, id: "device_tab_next", label: label_device_tab_next, help: help_device_tab_next },
    DevicesTabPrevious => { scope: Devices, id: "device_tab_previous", label: label_device_tab_previous, help: help_device_tab_previous },
    PairingRouteNext => { scope: Devices, id: "device_route_next", label: label_device_route_next, help: help_device_route_next },
    PairingRoutePrevious => { scope: Devices, id: "device_route_previous", label: label_device_route_previous, help: help_device_route_previous },
    AllowLanPairing => { scope: Devices, id: "device_lan_allow", label: label_device_lan_allow, help: help_device_lan_allow },
    RefuseLanPairing => { scope: Devices, id: "device_lan_refuse", label: label_device_lan_refuse, help: help_device_lan_refuse },
    RevokeSelectedDevice => { scope: Devices, id: "device_revoke", label: label_device_revoke, help: help_device_revoke },
    OpenDeviceAudit => { scope: Devices, id: "device_audit", label: label_device_audit, help: help_device_audit },

    // ---- settings -------------------------------------------------------
    ActivateSetting => { scope: Settings, id: "setting_activate", label: label_setting_activate, help: help_setting_activate },
    SettingPrevious => { scope: Settings, id: "setting_previous", label: label_setting_previous, help: help_setting_previous },
    SettingNext => { scope: Settings, id: "setting_next", label: label_setting_next, help: help_setting_next },
    ResetSetting => { scope: Settings, id: "setting_reset", label: label_setting_reset, help: help_setting_reset },

    // ---- filters (shared) ------------------------------------------------
    BeginFilter => { scope: Global, id: "filter_begin", label: label_filter_begin, help: help_filter_begin },
    ClearFilter => { scope: Global, id: "filter_clear", label: label_filter_clear, help: help_filter_clear },
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
                | Intent::BeginRenameSession
                | Intent::ForkSession
                | Intent::ArchiveSession
                | Intent::UnarchiveSession
                | Intent::DeleteSession
                | Intent::ActivateDevicePairing
                | Intent::AllowLanPairing
                | Intent::RefuseLanPairing
                | Intent::RevokeSelectedDevice
                | Intent::ToggleSelectedEntry
                | Intent::EditSelectedEntry
                | Intent::SteerRunningTurn
                | Intent::BackgroundRunningCommand
                | Intent::SwitchAgentRuntime
        )
    }

    /// Whether the intent names somewhere else to be.
    ///
    /// These are the destinations the key bar and the cheatsheet keep
    /// advertising while a modal or a filter owns the keyboard, so a surface
    /// that swallowed them would be promising keys it does not answer: a modal
    /// puts itself down and runs one, exactly as `Esc` would have put it down.
    pub const fn leaves_the_surface(self) -> bool {
        matches!(
            self,
            Intent::OpenCommandPalette
                | Intent::ToggleHelp
                | Intent::OpenSettings
                | Intent::GotoSessions
                | Intent::GotoManagement
        )
    }

    /// Whether the intent is meaningful while the connection is down.
    ///
    /// `NewSession` is here rather than in [`Self::mutates`] because it only
    /// moves to the composing page and asks for reads: the page is the one the
    /// client opens on, so it has to be reachable in the first moments of a run
    /// when the seat is still connecting. Sending from it is what needs the
    /// runtime, and `SubmitComposer` says so.
    pub const fn works_offline(self) -> bool {
        matches!(
            self,
            Intent::ToggleHelp
                | Intent::OpenSettings
                | Intent::OpenCommandPalette
                | Intent::Back
                | Intent::FocusNext
                | Intent::FocusPrevious
                | Intent::NewSession
                | Intent::GotoSessions
                | Intent::GotoManagement
                | Intent::SelectPrevious
                | Intent::SelectNext
                | Intent::SessionControlPrevious
                | Intent::SessionControlNext
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
                | Intent::ScheduleMessage
                | Intent::CopyBlockBody
                | Intent::CopyBlockMetadata
                | Intent::OpenBlockDetails
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
                | Intent::SelectAllDraft
                | Intent::CopyDraftSelection
                | Intent::BeginFilter
                | Intent::ClearFilter
                | Intent::ActivateSetting
                | Intent::SettingPrevious
                | Intent::SettingNext
                | Intent::ResetSetting
                // Folding the catalogue, starring a model and resetting a run
                // option are all local: the switcher can be read and arranged
                // while the runtime is unreachable. Sending what a row was set
                // to does need the runtime, which is why `ManageRuntimeAccount`
                // is deliberately absent.
                | Intent::OverlayFoldOpen
                | Intent::OverlayFoldClosed
                | Intent::StarRuntimeModel
                | Intent::ResetRunOption
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
    fn labels_and_help_are_present_in_every_language() {
        // The three languages live in one locale table and are reached through
        // these accessors, so a missing translation is a compile error. What
        // this pins is that the copy is real: every action is named and
        // explained in each language, and the explanation is a sentence there
        // too — which is the rule the English-only version of this test kept.
        let languages = [
            crate::locale::Locale::En,
            crate::locale::Locale::ZhCn,
            crate::locale::Locale::ZhTw,
        ];
        for intent in Intent::ALL {
            for locale in languages {
                let strings = Strings::for_locale(locale);
                let label = intent.label(strings);
                let help = intent.help(strings);
                assert!(
                    !label.is_empty(),
                    "{} has no label in {locale:?}",
                    intent.id()
                );
                assert!(
                    !help.is_empty(),
                    "{} has no help in {locale:?}",
                    intent.id()
                );
                assert!(
                    help.ends_with('.') || help.ends_with('。'),
                    "{} help should be a sentence in {locale:?}: {help}",
                    intent.id()
                );
            }
        }
    }

    #[test]
    fn every_action_is_named_in_chinese() {
        // A translation that is still the English words is a translation nobody
        // wrote. Three actions are the same string in every language on
        // purpose: `Quit` is the key's own verb, `$EDITOR` names the reader's
        // editor, and `Runtime` names the Agent runtime the product calls by
        // that name in all three.
        let en = Strings::for_locale(crate::locale::Locale::En);
        let cn = Strings::for_locale(crate::locale::Locale::ZhCn);
        let tw = Strings::for_locale(crate::locale::Locale::ZhTw);
        let untranslated = Intent::ALL
            .iter()
            .filter(|intent| {
                !matches!(
                    intent,
                    Intent::RequestQuit
                        | Intent::EditComposerExternally
                        | Intent::SwitchAgentRuntime
                )
            })
            .filter(|intent| intent.label(cn) == intent.label(en))
            .filter(|intent| intent.label(tw) == intent.label(en))
            .map(|intent| intent.id())
            .collect::<Vec<_>>();
        assert!(
            untranslated.is_empty(),
            "these actions keep their English label in Chinese: {untranslated:?}"
        );
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
