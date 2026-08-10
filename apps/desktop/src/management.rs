//! GPUI management center.
//!
//! This module is intentionally a view/controller layer.  It owns section
//! navigation, input entities, loading generations, and redacted projections;
//! durable records and side effects stay behind `DesktopRuntime::management`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AccessibleAction, Anchor, Animation, AnimationExt as _, AnyElement, App, Context,
    DragMoveEvent, Empty, Entity, EventEmitter, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, Orientation, Render, Role, SharedString, StatefulInteractiveElement as _,
    Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, Theme, WindowExt as _,
    animation::ease_out_cubic,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    switch::Switch,
    tooltip::Tooltip,
    v_flex,
};
use vibex_core::{
    AgentAuthCatalog, AgentAuthEnvironmentUpdateRequest, AgentAuthEnvironmentValue,
    AgentAuthMethodKind, AgentAuthStatus, AgentAuthenticateRequest, AgentId, AgentListRequest,
    AgentSnapshotEntry, AgentUpdateConfigRequest, AutomationGraphCreateRequest, AutomationGraphId,
    AutomationGraphListRequest, AutomationGraphStatus, AutomationRun, AutomationRunCancelRequest,
    AutomationRunId, AutomationRunListRequest, AutomationRunResumeRequest,
    AutomationRunStartRequest, AutomationRunStatus, AutomationRunStep,
    AutomationRunStepListRequest, AutomationRunTrigger, ProviderKind, ScheduledTask,
    ScheduledTaskAttentionListRequest, ScheduledTaskAuditListRequest, ScheduledTaskCreateRequest,
    ScheduledTaskId, ScheduledTaskIntervalSchedule, ScheduledTaskListRequest, ScheduledTaskRun,
    ScheduledTaskRunListRequest, ScheduledTaskSchedule, TerminalAuthActionDescriptor, VibexError,
    VibexResult, WorkspaceMode, unix_timestamp_ms,
};
use vibex_desktop_model::{
    AutomationGraphDraft, ManagementNavigation, ManagementSection, PairingContextProjection,
    ProviderCenterSnapshot, RecoveryOperationState, RedactedDiagnosticProjection,
};
use vibex_desktop_runtime::{
    DesktopRuntime, ManagementHandle, RuntimeOptionProbeResult, validate_external_open_url,
};
use vibex_markdown::code_font_weight;
use vibex_ui::{AgentProviderBindingEditorState, ProjectionCredentialSurface};

use crate::assets::agent_brand_icon;
use crate::gpui_ext::button_with_aria_label;
use crate::locale::{self, ResolvedLocale};
use crate::remote_access_pairing::open_remote_access_pairing;
use crate::terminal_surface::TerminalSurface;

const MANAGEMENT_SIDEBAR_WIDTH: f32 = 368.0;
const MANAGEMENT_HEADER_HEIGHT: f32 = 48.0;
const MANAGEMENT_WIDE_BREAKPOINT: f32 = 1024.0;
const MANAGEMENT_COMPACT_SIDEBAR_DEFAULT_HEIGHT: f32 = 360.0;
const MANAGEMENT_COMPACT_SIDEBAR_MIN_HEIGHT: f32 = 192.0;
const MANAGEMENT_COMPACT_SIDEBAR_MAX_HEIGHT: f32 = 560.0;
const MANAGEMENT_COMPACT_MAIN_MIN_HEIGHT: f32 = 192.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_HEIGHT: f32 = 12.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_WIDTH: f32 = 48.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_HOVER_WIDTH: f32 = 80.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_THICKNESS: f32 = 3.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_HOVER_THICKNESS: f32 = 5.0;
const MANAGEMENT_COMPACT_RESIZE_HANDLE_ANIMATION_MS: u64 = 140;
const AGENT_AUTH_TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MANAGEMENT_HOST_TITLE_BAR_HEIGHT: f32 = 44.0;
const MANAGEMENT_COMPACT_RESIZE_STEP: f32 = 16.0;
const MANAGEMENT_DETAIL_ACTION_HEIGHT: f32 = 42.0;
const MANAGEMENT_PROVIDER_ROW_ACTION_SIZE: f32 = 40.0;
const PROVIDER_OPTION_WEBSITE_URL: &str = "ccSwitchWebsiteUrl";
const PROVIDER_OPTION_CC_SWITCH_DB_PATH: &str = "ccSwitchDbPath";
const PROVIDER_OPTION_CC_SWITCH_PROVIDER_ID: &str = "ccSwitchProviderId";
const PROVIDER_OPTION_CC_SWITCH_APP_TYPE: &str = "ccSwitchAppType";
const PROVIDER_OPTION_NATIVE_SOURCE: &str = "nativeSource";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagementEvent {
    AgentRegistryChanged,
}

struct ManagementCenterFeedbackNotification;

#[derive(Debug, Clone, Copy, PartialEq)]
struct ManagementSidebarResizeDragState {
    start_window_y: f32,
    start_height: f32,
    min_height: f32,
    max_height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagementSidebarResizeDrag;

impl Render for ManagementSidebarResizeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagementImportKind {
    Mcp,
    Skill,
}

struct ManagementImportDialog {
    center: Entity<ManagementCenter>,
    kind: ManagementImportKind,
    _center_subscription: Subscription,
}

impl ManagementImportDialog {
    fn new(
        center: Entity<ManagementCenter>,
        kind: ManagementImportKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let center_subscription = cx.observe(&center, |_, _, cx| cx.notify());
        Self {
            center,
            kind,
            _center_subscription: center_subscription,
        }
    }
}

struct ManagementProfileDialog {
    center: Entity<ManagementCenter>,
    _center_subscription: Subscription,
}

impl ManagementProfileDialog {
    fn new(center: Entity<ManagementCenter>, cx: &mut Context<Self>) -> Self {
        let center_subscription = cx.observe(&center, |_, _, cx| cx.notify());
        Self {
            center,
            _center_subscription: center_subscription,
        }
    }
}

#[derive(Clone, Copy)]
struct ManagementCopy {
    title: &'static str,
    agents: &'static str,
    mcp: &'static str,
    skills: &'static str,
    advanced: &'static str,
    search_agents: &'static str,
    search_mcp: &'static str,
    search_skills: &'static str,
    add_configuration: &'static str,
    import_configuration: &'static str,
    provider_configuration: &'static str,
    no_agents: &'static str,
    no_agents_description: &'static str,
    no_profiles: &'static str,
    no_profiles_description: &'static str,
    import_mcp: &'static str,
    import_skill: &'static str,
}

fn management_copy() -> ManagementCopy {
    match locale::current_locale() {
        ResolvedLocale::En => ManagementCopy {
            title: "Config Center",
            agents: "Agent",
            mcp: "MCP",
            skills: "Skills",
            advanced: "Advanced",
            search_agents: "Search Agent",
            search_mcp: "Search MCP",
            search_skills: "Search Skills",
            add_configuration: "Add config",
            import_configuration: "Import existing config",
            provider_configuration: "Model provider configuration",
            no_agents: "No Agent added",
            no_agents_description: "Add an Agent from the list.",
            no_profiles: "No model provider configuration",
            no_profiles_description: "Add or import a configuration for the selected Agent.",
            import_mcp: "Import Existing MCP",
            import_skill: "Import Existing Skills",
        },
        ResolvedLocale::ZhCn => ManagementCopy {
            title: "配置中心",
            agents: "Agent",
            mcp: "MCP",
            skills: "技能",
            advanced: "高级",
            search_agents: "搜索 Agent",
            search_mcp: "搜索 MCP",
            search_skills: "搜索技能",
            add_configuration: "添加配置",
            import_configuration: "导入已有配置",
            provider_configuration: "模型供应商配置",
            no_agents: "尚未添加 Agent",
            no_agents_description: "从列表中添加一个 Agent。",
            no_profiles: "暂无模型供应商配置",
            no_profiles_description: "为当前 Agent 添加或导入配置。",
            import_mcp: "导入已有 MCP",
            import_skill: "导入已有技能",
        },
        ResolvedLocale::ZhTw => ManagementCopy {
            title: "配置中心",
            agents: "Agent",
            mcp: "MCP",
            skills: "技能",
            advanced: "進階",
            search_agents: "搜尋 Agent",
            search_mcp: "搜尋 MCP",
            search_skills: "搜尋技能",
            add_configuration: "新增配置",
            import_configuration: "匯入既有配置",
            provider_configuration: "模型供應商配置",
            no_agents: "尚未新增 Agent",
            no_agents_description: "從列表中新增一個 Agent。",
            no_profiles: "暫無模型供應商配置",
            no_profiles_description: "為目前 Agent 新增或匯入配置。",
            import_mcp: "匯入已有 MCP",
            import_skill: "匯入已有技能",
        },
    }
}

#[derive(Clone)]
struct ManagementSnapshot {
    center: ProviderCenterSnapshot,
    provider_profiles: Vec<vibex_core::ProviderProfile>,
    model_provider_agent_ids: BTreeSet<String>,
    acp_configs: Vec<(String, vibex_core::AcpProviderConfig)>,
    native_import_preview: Option<vibex_core::ProviderNativeImportPreview>,
    agent_profile_states: Vec<AgentProviderProfileState>,
    provider_display_order: BTreeMap<String, i64>,
    projection_states: Vec<AgentProviderProjectionState>,
    health_summaries: Vec<vibex_core::ProviderHealthSummary>,
    capability_summaries: Vec<vibex_core::ProviderCapabilitySummary>,
    usage_summaries: Vec<vibex_core::ProviderUsageSummary>,
    native_exports: Vec<vibex_core::ProviderNativeExportRecordSummary>,
    device_count: usize,
    revoked_device_count: usize,
    audit_count: usize,
    scheduled_runs: Vec<ScheduledTaskRun>,
    scheduled_attention: Vec<vibex_core::ScheduledTaskAttentionSummary>,
    scheduled_audit: Vec<vibex_core::ScheduledTaskAuditRecord>,
    automation_runs: Vec<AutomationRun>,
    automation_steps: Vec<AutomationRunStep>,
    devices: Vec<vibex_core::RemoteDeviceDetail>,
}

#[derive(Clone)]
struct AgentProviderProfileState {
    agent_id: String,
    profile_id: String,
    is_default: bool,
}

#[derive(Clone)]
struct ProviderDisplayOrderDrag {
    agent_id: String,
    profile_id: String,
    label: SharedString,
}

impl Render for ProviderDisplayOrderDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_3()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().drag_border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .child(
                Icon::default()
                    .path("icons/vibex/grip-vertical.svg")
                    .size(px(14.0)),
            )
            .child(self.label.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderDisplayOrderDropTarget {
    profile_id: String,
    after: bool,
}

#[derive(Clone)]
struct AgentProviderProjectionState {
    agent_id: String,
    legacy_profile_id: Option<String>,
    capability: vibex_core::AgentProviderProjectionCapability,
    preview: Option<vibex_core::AgentProviderProjectionPreview>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagementMutation {
    ProfileCreate,
    ProfileUpdate(String),
    ProfileDelete(String),
    AcpConfig(String),
    ProviderProbe(String),
    AgentAuth(String),
    ProviderPreview(String),
    ProviderDisplayOrder(String),
    AgentToggle(String),
    AgentInstall(String),
    AgentUpdateCheck(String),
    AgentUninstall(String),
    AgentDiscovery,
    McpAction(String),
    SkillAction(String),
    PromptAction(String),
    HookAction(String),
    ScheduledPause(String),
    ScheduledResume(String),
    ScheduledRun(String),
    ScheduledUpdate(String),
    ScheduledDelete(String),
    AutomationPause(String),
    AutomationResume(String),
    AutomationArchive(String),
    AutomationRun(String),
    AutomationResumeRun(String),
    AutomationSave,
    AutomationCreate,
    ScheduledCreate,
    RemoteRevoke(String),
    AutomationCancel(String),
    DiagnosticsExport,
    BackupCreate,
    BackupInspect,
    BackupRestore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentAuthTerminalState {
    Running,
    Succeeded,
    Failed,
}

enum AgentAuthTerminalCompletion {
    Authenticated {
        catalog: Option<AgentAuthCatalog>,
        refresh_error: Option<VibexError>,
    },
    AuthenticationRequired(VibexError),
}

impl ManagementMutation {
    fn key(&self) -> String {
        match self {
            Self::ProfileCreate => "profile:create".into(),
            Self::ProfileUpdate(id) => format!("profile:update:{id}"),
            Self::ProfileDelete(id) => format!("profile:delete:{id}"),
            Self::AcpConfig(id) => format!("provider:acp-config:{id}"),
            Self::ProviderProbe(id) => format!("provider:probe:{id}"),
            Self::AgentAuth(id) => format!("agent:auth:{id}"),
            Self::ProviderPreview(id) => format!("provider:preview:{id}"),
            Self::ProviderDisplayOrder(id) => format!("provider:display-order:{id}"),
            Self::AgentToggle(id) => format!("agent:toggle:{id}"),
            Self::AgentInstall(id) => format!("agent:install:{id}"),
            Self::AgentUpdateCheck(id) => format!("agent:update-check:{id}"),
            Self::AgentUninstall(id) => format!("agent:uninstall:{id}"),
            Self::AgentDiscovery => "agent:discover".into(),
            Self::McpAction(id) => format!("mcp:{id}"),
            Self::SkillAction(id) => format!("skill:{id}"),
            Self::PromptAction(id) => format!("prompt:{id}"),
            Self::HookAction(id) => format!("hook:{id}"),
            Self::ScheduledPause(id) => format!("scheduled:pause:{id}"),
            Self::ScheduledResume(id) => format!("scheduled:resume:{id}"),
            Self::ScheduledRun(id) => format!("scheduled:run:{id}"),
            Self::ScheduledUpdate(id) => format!("scheduled:update:{id}"),
            Self::ScheduledDelete(id) => format!("scheduled:delete:{id}"),
            Self::AutomationPause(id) => format!("automation:pause:{id}"),
            Self::AutomationResume(id) => format!("automation:resume:{id}"),
            Self::AutomationArchive(id) => format!("automation:archive:{id}"),
            Self::AutomationRun(id) => format!("automation:run:{id}"),
            Self::AutomationResumeRun(id) => format!("automation:resume-run:{id}"),
            Self::AutomationSave => "automation:save".into(),
            Self::AutomationCreate => "automation:create".into(),
            Self::ScheduledCreate => "scheduled:create".into(),
            Self::RemoteRevoke(id) => format!("remote:revoke:{id}"),
            Self::AutomationCancel(id) => format!("automation:cancel:{id}"),
            Self::DiagnosticsExport => "diagnostics:export".into(),
            Self::BackupCreate => "backup:create".into(),
            Self::BackupInspect => "backup:inspect".into(),
            Self::BackupRestore => "backup:restore".into(),
        }
    }

    fn concurrent_agent_id(&self) -> Option<&str> {
        match self {
            Self::AgentToggle(action) => Some(
                action
                    .rsplit_once(':')
                    .map_or(action.as_str(), |(_, agent_id)| agent_id),
            ),
            Self::AgentInstall(agent_id)
            | Self::AgentUpdateCheck(agent_id)
            | Self::AgentUninstall(agent_id)
            | Self::ProviderDisplayOrder(agent_id) => Some(agent_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
enum ManagedDeleteTarget {
    Agent { id: String, label: String },
    Provider { id: String, label: String },
    Mcp { id: String, label: String },
    Skill { id: String, label: String },
    Prompt { id: String, label: String },
    Hook { id: String, label: String },
}

impl ManagedDeleteTarget {
    fn label(&self) -> &str {
        match self {
            Self::Agent { label, .. }
            | Self::Provider { label, .. }
            | Self::Mcp { label, .. }
            | Self::Skill { label, .. }
            | Self::Prompt { label, .. }
            | Self::Hook { label, .. } => label,
        }
    }
}

pub struct ManagementCenter {
    runtime: Option<Arc<DesktopRuntime>>,
    navigation: ManagementNavigation,
    snapshot: ProviderCenterSnapshot,
    provider_profiles: Vec<vibex_core::ProviderProfile>,
    model_provider_agent_ids: BTreeSet<String>,
    acp_configs: Vec<(String, vibex_core::AcpProviderConfig)>,
    native_import_preview: Option<vibex_core::ProviderNativeImportPreview>,
    agent_profile_states: Vec<AgentProviderProfileState>,
    provider_display_order: BTreeMap<String, i64>,
    projection_states: Vec<AgentProviderProjectionState>,
    projection_editor: AgentProviderBindingEditorState,
    health_summaries: Vec<vibex_core::ProviderHealthSummary>,
    capability_summaries: Vec<vibex_core::ProviderCapabilitySummary>,
    usage_summaries: Vec<vibex_core::ProviderUsageSummary>,
    native_exports: Vec<vibex_core::ProviderNativeExportRecordSummary>,
    native_export_source: vibex_core::ProviderNativeExportSource,
    native_export_mode: vibex_core::ProviderNativeExportMode,
    native_export_preview: Option<vibex_core::ProviderNativeExportPreview>,
    graph_draft: AutomationGraphDraft,
    pairing: PairingContextProjection,
    pairing_workspace_id: Option<vibex_core::WorkspaceId>,
    diagnostics: RedactedDiagnosticProjection,
    recovery: RecoveryOperationState,
    device_count: usize,
    revoked_device_count: usize,
    audit_count: usize,
    scheduled_runs: Vec<ScheduledTaskRun>,
    scheduled_attention: Vec<vibex_core::ScheduledTaskAttentionSummary>,
    scheduled_audit: Vec<vibex_core::ScheduledTaskAuditRecord>,
    automation_runs: Vec<AutomationRun>,
    automation_steps: Vec<AutomationRunStep>,
    devices: Vec<vibex_core::RemoteDeviceDetail>,
    loading: bool,
    mutation: Option<ManagementMutation>,
    agent_mutations: BTreeMap<String, ManagementMutation>,
    error: Option<String>,
    notice: Option<String>,
    generation: u64,
    refresh_task: Option<Task<()>>,
    mutation_task: Option<Task<()>>,
    agent_mutation_tasks: BTreeMap<String, Task<()>>,
    agent_auth_task: Option<Task<()>>,
    agent_auth_terminal_monitor_task: Option<Task<()>>,
    discover_agents_after_refresh: bool,
    mcp_import_open: bool,
    skill_import_open: bool,
    mcp_discovery: Option<vibex_core::McpServerDiscoveryResponse>,
    skill_discovery: Option<vibex_core::SkillDiscoveryResponse>,
    mcp_validation: Option<(String, String, bool)>,
    skill_validation: Option<(String, String, bool)>,
    selected_agent_id: Option<String>,
    selected_provider_profile_id: Option<String>,
    agent_auth_scope: Option<(String, Option<String>)>,
    agent_auth_catalog: Option<AgentAuthCatalog>,
    agent_auth_loading: bool,
    agent_auth_error: Option<String>,
    agent_auth_generation: u64,
    agent_auth_inputs: BTreeMap<String, Entity<InputState>>,
    agent_auth_clear_values: BTreeSet<String>,
    agent_auth_terminal: Option<TerminalAuthActionDescriptor>,
    agent_auth_terminal_surface: Option<(String, Entity<TerminalSurface>)>,
    agent_auth_terminal_state: Option<AgentAuthTerminalState>,
    provider_display_order_drop_target: Option<ProviderDisplayOrderDropTarget>,
    selected_mcp_id: Option<String>,
    selected_skill_id: Option<String>,
    selected_scheduled_task_id: Option<String>,
    compact_sidebar_height: f32,
    compact_sidebar_resize_hovered: bool,
    compact_sidebar_resize_drag: Option<ManagementSidebarResizeDragState>,
    profile_editor_open: bool,
    editing_profile_id: Option<String>,
    profile_secret_touched: bool,
    profile_secret_loading: bool,
    profile_configured_models: Vec<vibex_core::ProviderConfiguredModel>,
    profile_model_edit_index: Option<usize>,
    profile_model_edit_wire_api: Option<vibex_core::ProviderModelWireApi>,
    profile_provider_options: vibex_core::ProviderOptions,
    selected_acp_profile_id: Option<String>,
    acp_config_draft: Option<vibex_core::AcpProviderConfig>,
    agent_search: Entity<InputState>,
    mcp_search: Entity<InputState>,
    skill_search: Entity<InputState>,
    profile_name: Entity<InputState>,
    profile_note: Entity<InputState>,
    profile_website_url: Entity<InputState>,
    profile_base_url: Entity<InputState>,
    profile_protocol_base_urls: Vec<(vibex_core::ProviderModelWireApi, Entity<InputState>)>,
    profile_model_draft: Entity<InputState>,
    profile_model_edit_id: Entity<InputState>,
    profile_model_edit_name: Entity<InputState>,
    profile_api_key: Entity<InputState>,
    acp_command: Entity<InputState>,
    acp_args: Entity<InputState>,
    acp_cwd: Entity<InputState>,
    scheduled_title: Entity<InputState>,
    scheduled_prompt: Entity<InputState>,
    automation_title: Entity<InputState>,
    automation_description: Entity<InputState>,
    prompt_name: Entity<InputState>,
    prompt_body: Entity<InputState>,
    hook_name: Entity<InputState>,
    hook_command: Entity<InputState>,
    backup_path: Entity<InputState>,
    restore_target: Entity<InputState>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<ManagementEvent> for ManagementCenter {}

impl ManagementCenter {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let copy = management_copy();
        let agent_search = cx.new(|cx| InputState::new(window, cx).placeholder(copy.search_agents));
        let mcp_search = cx.new(|cx| InputState::new(window, cx).placeholder(copy.search_mcp));
        let skill_search = cx.new(|cx| InputState::new(window, cx).placeholder(copy.search_skills));
        let profile_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Profile name",
                "配置名称",
                "配置名稱",
            ))
        });
        let profile_note = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text("Note", "备注", "備註"))
        });
        let profile_website_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://provider.example"));
        let profile_base_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://provider.example/v1"));
        let profile_model_draft = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Model id",
                "模型 ID",
                "模型 ID",
            ))
        });
        let profile_model_edit_id = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Model id",
                "模型 ID",
                "模型 ID",
            ))
        });
        let profile_model_edit_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Display name",
                "显示名称",
                "顯示名稱",
            ))
        });
        let profile_api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("API Key")
                .masked(true)
        });
        let acp_command = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "ACP command",
                "ACP 命令",
                "ACP 命令",
            ))
        });
        let acp_args = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Arguments separated by spaces",
                "以空格分隔参数",
                "以空格分隔參數",
            ))
        });
        let acp_cwd = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Working directory template",
                "工作目录模板",
                "工作目錄範本",
            ))
        });
        let scheduled_title = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Scheduled task title",
                "定时任务标题",
                "排程任務標題",
            ))
        });
        let scheduled_prompt = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Prompt to run",
                "要执行的提示词",
                "要執行的提示詞",
            ))
        });
        let automation_title = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Automation graph title",
                "自动化图标题",
                "自動化圖標題",
            ))
        });
        let automation_description = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Graph description (optional)",
                "图描述（可选）",
                "圖描述（選填）",
            ))
        });
        let prompt_name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(management_locale_text(
                    "Review command",
                    "检查命令",
                    "檢查命令",
                ))
                .placeholder(management_locale_text(
                    "Prompt name",
                    "提示词名称",
                    "提示詞名稱",
                ))
        });
        let prompt_body = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(management_locale_text(
                    "Review this workspace.",
                    "检查此工作区。",
                    "檢查此工作區。",
                ))
                .placeholder(management_locale_text(
                    "Prompt body",
                    "提示词内容",
                    "提示詞內容",
                ))
        });
        let hook_name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(management_locale_text(
                    "Permission notifier",
                    "权限通知器",
                    "權限通知器",
                ))
                .placeholder(management_locale_text(
                    "Hook name",
                    "Hook 名称",
                    "Hook 名稱",
                ))
        });
        let hook_command = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("vibex notify permission")
                .placeholder(management_locale_text(
                    "Redacted command preview",
                    "已脱敏的命令预览",
                    "已遮罩的命令預覽",
                ))
        });
        let backup_path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "Backup directory",
                "备份目录",
                "備份目錄",
            ))
        });
        let restore_target = cx.new(|cx| {
            InputState::new(window, cx).placeholder(management_locale_text(
                "New restore database path",
                "新的恢复数据库路径",
                "新的復原資料庫路徑",
            ))
        });
        let subscriptions = vec![
            cx.subscribe(&agent_search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&mcp_search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&skill_search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&profile_name, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_note, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_website_url, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_base_url, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_model_draft, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_model_edit_id, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_model_edit_name, |this, _, _: &InputEvent, cx| {
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&profile_api_key, |this, _, _: &InputEvent, cx| {
                this.profile_secret_touched = true;
                let clear = this.profile_api_key.read(cx).value().trim().is_empty();
                this.projection_editor.set_secret_intent(true, clear);
                this.projection_editor.mark_draft_changed();
                this.navigation.mark_dirty(ManagementSection::Agents, true);
                cx.notify();
            }),
            cx.subscribe(&acp_command, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&acp_args, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&acp_cwd, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&scheduled_title, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Scheduled, true);
                cx.notify();
            }),
            cx.subscribe(&scheduled_prompt, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Scheduled, true);
                cx.notify();
            }),
            cx.subscribe(&automation_title, |this, _, _: &InputEvent, cx| {
                this.graph_draft.title = this.automation_title.read(cx).value().to_string();
                this.graph_draft.dirty = true;
                this.navigation
                    .mark_dirty(ManagementSection::Automation, true);
                cx.notify();
            }),
            cx.subscribe(&automation_description, |this, _, _: &InputEvent, cx| {
                this.graph_draft.description =
                    this.automation_description.read(cx).value().to_string();
                this.graph_draft.dirty = true;
                this.navigation
                    .mark_dirty(ManagementSection::Automation, true);
                cx.notify();
            }),
            cx.subscribe(&prompt_name, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&prompt_body, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&hook_name, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&hook_command, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Advanced, true);
                cx.notify();
            }),
            cx.subscribe(&backup_path, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Recovery, true);
                cx.notify();
            }),
            cx.subscribe(&restore_target, |this, _, _: &InputEvent, cx| {
                this.navigation
                    .mark_dirty(ManagementSection::Recovery, true);
                cx.notify();
            }),
        ];
        Self {
            runtime: None,
            navigation: ManagementNavigation::default(),
            snapshot: ProviderCenterSnapshot::default(),
            provider_profiles: Vec::new(),
            model_provider_agent_ids: BTreeSet::new(),
            acp_configs: Vec::new(),
            native_import_preview: None,
            agent_profile_states: Vec::new(),
            provider_display_order: BTreeMap::new(),
            projection_states: Vec::new(),
            projection_editor: AgentProviderBindingEditorState::default(),
            health_summaries: Vec::new(),
            capability_summaries: Vec::new(),
            usage_summaries: Vec::new(),
            native_exports: Vec::new(),
            native_export_source: vibex_core::ProviderNativeExportSource::Codex,
            native_export_mode: vibex_core::ProviderNativeExportMode::ProviderProfile,
            native_export_preview: None,
            graph_draft: AutomationGraphDraft::empty(),
            pairing: PairingContextProjection::new(None, None, "current_checkout"),
            pairing_workspace_id: None,
            diagnostics: RedactedDiagnosticProjection::default(),
            recovery: RecoveryOperationState::default(),
            device_count: 0,
            revoked_device_count: 0,
            audit_count: 0,
            scheduled_runs: Vec::new(),
            scheduled_attention: Vec::new(),
            scheduled_audit: Vec::new(),
            automation_runs: Vec::new(),
            automation_steps: Vec::new(),
            devices: Vec::new(),
            loading: false,
            mutation: None,
            agent_mutations: BTreeMap::new(),
            error: None,
            notice: None,
            generation: 0,
            refresh_task: None,
            mutation_task: None,
            agent_mutation_tasks: BTreeMap::new(),
            agent_auth_task: None,
            agent_auth_terminal_monitor_task: None,
            discover_agents_after_refresh: false,
            mcp_import_open: false,
            skill_import_open: false,
            mcp_discovery: None,
            skill_discovery: None,
            mcp_validation: None,
            skill_validation: None,
            provider_display_order_drop_target: None,
            selected_agent_id: None,
            selected_provider_profile_id: None,
            agent_auth_scope: None,
            agent_auth_catalog: None,
            agent_auth_loading: false,
            agent_auth_error: None,
            agent_auth_generation: 0,
            agent_auth_inputs: BTreeMap::new(),
            agent_auth_clear_values: BTreeSet::new(),
            agent_auth_terminal: None,
            agent_auth_terminal_surface: None,
            agent_auth_terminal_state: None,
            selected_mcp_id: None,
            selected_skill_id: None,
            selected_scheduled_task_id: None,
            compact_sidebar_height: MANAGEMENT_COMPACT_SIDEBAR_DEFAULT_HEIGHT,
            compact_sidebar_resize_hovered: false,
            compact_sidebar_resize_drag: None,
            profile_editor_open: false,
            editing_profile_id: None,
            profile_secret_touched: false,
            profile_secret_loading: false,
            profile_configured_models: Vec::new(),
            profile_model_edit_index: None,
            profile_model_edit_wire_api: None,
            profile_provider_options: vibex_core::ProviderOptions::empty(),
            selected_acp_profile_id: None,
            acp_config_draft: None,
            agent_search,
            mcp_search,
            skill_search,
            profile_name,
            profile_note,
            profile_website_url,
            profile_base_url,
            profile_protocol_base_urls: Vec::new(),
            profile_model_draft,
            profile_model_edit_id,
            profile_model_edit_name,
            profile_api_key,
            acp_command,
            acp_args,
            acp_cwd,
            scheduled_title,
            scheduled_prompt,
            automation_title,
            automation_description,
            prompt_name,
            prompt_body,
            hook_name,
            hook_command,
            backup_path,
            restore_target,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_runtime(&mut self, runtime: Arc<DesktopRuntime>, cx: &mut Context<Self>) {
        if self
            .runtime
            .as_ref()
            .is_some_and(|current| !Arc::ptr_eq(current, &runtime))
        {
            self.agent_auth_generation = self.agent_auth_generation.saturating_add(1);
            self.clear_agent_auth_terminal();
            self.agent_auth_scope = None;
            self.agent_auth_catalog = None;
            self.agent_auth_error = None;
            self.agent_auth_inputs.clear();
            self.agent_auth_clear_values.clear();
        }
        self.runtime = Some(runtime);
        self.error = None;
        self.notice = None;
        self.refresh(cx);
    }

    pub fn sync_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let copy = management_copy();
        self.agent_search.update(cx, |input, cx| {
            input.set_placeholder(copy.search_agents, window, cx)
        });
        self.mcp_search.update(cx, |input, cx| {
            input.set_placeholder(copy.search_mcp, window, cx)
        });
        self.skill_search.update(cx, |input, cx| {
            input.set_placeholder(copy.search_skills, window, cx)
        });
        let placeholders = [
            (&self.profile_name, ("Profile name", "配置名称", "配置名稱")),
            (&self.profile_note, ("Note", "备注", "備註")),
            (
                &self.profile_model_draft,
                ("Model id", "模型 ID", "模型 ID"),
            ),
            (
                &self.profile_model_edit_id,
                ("Model id", "模型 ID", "模型 ID"),
            ),
            (
                &self.profile_model_edit_name,
                ("Display name", "显示名称", "顯示名稱"),
            ),
            (&self.acp_command, ("ACP command", "ACP 命令", "ACP 命令")),
            (
                &self.acp_args,
                (
                    "Arguments separated by spaces",
                    "以空格分隔参数",
                    "以空格分隔參數",
                ),
            ),
            (
                &self.acp_cwd,
                ("Working directory template", "工作目录模板", "工作目錄範本"),
            ),
            (
                &self.scheduled_title,
                ("Scheduled task title", "定时任务标题", "排程任務標題"),
            ),
            (
                &self.scheduled_prompt,
                ("Prompt to run", "要执行的提示词", "要執行的提示詞"),
            ),
            (
                &self.automation_title,
                ("Automation graph title", "自动化图标题", "自動化圖標題"),
            ),
            (
                &self.automation_description,
                (
                    "Graph description (optional)",
                    "图描述（可选）",
                    "圖描述（選填）",
                ),
            ),
            (
                &self.prompt_name,
                ("Prompt name", "提示词名称", "提示詞名稱"),
            ),
            (
                &self.prompt_body,
                ("Prompt body", "提示词内容", "提示詞內容"),
            ),
            (&self.hook_name, ("Hook name", "Hook 名称", "Hook 名稱")),
            (
                &self.hook_command,
                (
                    "Redacted command preview",
                    "已脱敏的命令预览",
                    "已遮罩的命令預覽",
                ),
            ),
            (
                &self.backup_path,
                ("Backup directory", "备份目录", "備份目錄"),
            ),
            (
                &self.restore_target,
                (
                    "New restore database path",
                    "新的恢复数据库路径",
                    "新的復原資料庫路徑",
                ),
            ),
        ];
        for (input, (en, zh_cn, zh_tw)) in placeholders {
            input.update(cx, |input, cx| {
                input.set_placeholder(locale::text(en, zh_cn, zh_tw), window, cx)
            });
        }
        cx.notify();
    }

    pub fn clear_runtime(&mut self, cx: &mut Context<Self>) {
        self.clear_agent_auth_terminal();
        self.runtime = None;
        self.loading = false;
        self.agent_auth_generation = self.agent_auth_generation.saturating_add(1);
        self.agent_auth_loading = false;
        self.agent_auth_scope = None;
        self.agent_auth_catalog = None;
        self.agent_auth_error = None;
        self.agent_auth_inputs.clear();
        self.agent_auth_clear_values.clear();
        self.discover_agents_after_refresh = false;
        self.mcp_import_open = false;
        self.skill_import_open = false;
        self.mcp_discovery = None;
        self.skill_discovery = None;
        self.native_export_preview = None;
        self.native_import_preview = None;
        self.notice = Some(
            management_locale_text(
                "Management runtime is not connected",
                "配置中心运行时未连接",
                "配置中心執行階段未連線",
            )
            .to_string(),
        );
        cx.notify();
    }

    pub fn request_local_agent_discovery(&mut self, cx: &mut Context<Self>) {
        self.navigation.active = ManagementSection::Agents;
        self.discover_agents_after_refresh = true;
        if !self.loading {
            self.discover_agents_after_refresh = false;
            self.discover_local_agents(cx);
        } else {
            cx.notify();
        }
    }

    pub fn set_pairing_context(
        &mut self,
        workspace: Option<String>,
        workspace_id: Option<vibex_core::WorkspaceId>,
        session_id: Option<String>,
        mode: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let default_scope_changed = self.pairing_workspace_id != workspace_id;
        self.pairing = PairingContextProjection::new(workspace, session_id, mode);
        self.pairing_workspace_id = workspace_id;
        if default_scope_changed && self.runtime.is_some() {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    fn current_workspace_context(&self) -> Option<(String, WorkspaceMode)> {
        if let Some(workspace) = self
            .pairing
            .workspace
            .as_ref()
            .filter(|workspace| !workspace.trim().is_empty())
        {
            let mode = if self.pairing.mode == "vibex_worktree" {
                WorkspaceMode::VibexWorktree
            } else {
                WorkspaceMode::CurrentCheckout
            };
            return Some((workspace.clone(), mode));
        }
        self.snapshot
            .graphs
            .first()
            .map(|graph| (graph.workspace_root.clone(), graph.workspace_mode))
            .or_else(|| {
                self.snapshot
                    .scheduled
                    .first()
                    .map(|task| (task.workspace_root.clone(), task.workspace_mode))
            })
            .filter(|(workspace, _)| !workspace.trim().is_empty())
    }

    fn select_management_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        if self.selected_agent_id.as_deref() == Some(agent_id.as_str()) {
            return;
        }
        self.selected_agent_id = Some(agent_id);
        self.selected_provider_profile_id = None;
        self.selected_acp_profile_id = None;
        self.acp_config_draft = None;
        self.native_export_preview = None;
        self.sync_projection_editor();
        self.load_agent_auth(false, cx);
        cx.notify();
    }

    fn select_provider_profile(&mut self, profile_id: String, cx: &mut Context<Self>) {
        if self.selected_provider_profile_id.as_deref() == Some(profile_id.as_str()) {
            return;
        }
        self.selected_provider_profile_id = Some(profile_id);
        self.selected_acp_profile_id = None;
        self.acp_config_draft = None;
        self.native_export_preview = None;
        self.sync_projection_editor();
        self.load_agent_auth(false, cx);
        cx.notify();
    }

    fn current_agent_auth_scope(&self) -> Option<(String, Option<String>)> {
        let agent_id = self.selected_agent_id.as_ref()?;
        self.snapshot
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == agent_id && agent.added && agent.enabled)?;
        Some((agent_id.clone(), self.selected_provider_profile_id.clone()))
    }

    fn load_agent_auth(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(scope) = self.current_agent_auth_scope() else {
            self.agent_auth_generation = self.agent_auth_generation.saturating_add(1);
            self.agent_auth_scope = None;
            self.agent_auth_catalog = None;
            self.agent_auth_loading = false;
            self.agent_auth_error = None;
            self.agent_auth_inputs.clear();
            self.agent_auth_clear_values.clear();
            self.clear_agent_auth_terminal();
            cx.notify();
            return;
        };
        if self.agent_auth_scope.as_ref() == Some(&scope)
            && self.agent_auth_terminal_state == Some(AgentAuthTerminalState::Running)
        {
            return;
        }
        if !force
            && self.agent_auth_scope.as_ref() == Some(&scope)
            && (self.agent_auth_loading || self.agent_auth_catalog.is_some())
        {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let Ok(agent_id) = AgentId::parse(scope.0.clone()) else {
            return;
        };
        let provider_profile_id = match scope.1.as_ref() {
            Some(profile_id) => match vibex_core::ProviderProfileId::parse(profile_id.clone()) {
                Ok(profile_id) => Some(profile_id),
                Err(error) => {
                    self.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                    cx.notify();
                    return;
                }
            },
            None => None,
        };
        let scope_changed = self.agent_auth_scope.as_ref() != Some(&scope);
        self.agent_auth_generation = self.agent_auth_generation.saturating_add(1);
        let generation = self.agent_auth_generation;
        self.agent_auth_scope = Some(scope.clone());
        self.agent_auth_loading = true;
        self.agent_auth_error = None;
        if scope_changed {
            self.agent_auth_catalog = None;
            self.agent_auth_inputs.clear();
            self.agent_auth_clear_values.clear();
            self.clear_agent_auth_terminal();
        }
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            if force {
                runtime
                    .agent()
                    .refresh_auth_methods(agent_id, provider_profile_id)
                    .await
            } else {
                runtime
                    .agent()
                    .list_auth_methods(agent_id, provider_profile_id)
                    .await
            }
        });
        self.agent_auth_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if this.agent_auth_generation != generation
                    || this.agent_auth_scope.as_ref() != Some(&scope)
                {
                    return;
                }
                this.agent_auth_loading = false;
                match outcome {
                    Ok(Ok(catalog)) => {
                        this.agent_auth_catalog = Some(catalog);
                        this.agent_auth_error = None;
                        this.agent_auth_inputs.clear();
                        this.agent_auth_clear_values.clear();
                    }
                    Ok(Err(error)) => {
                        this.agent_auth_catalog = None;
                        this.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.agent_auth_catalog = None;
                        this.agent_auth_error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Authentication discovery failed",
                                "认证方式发现失败",
                                "驗證方式探索失敗",
                            )
                        ));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn ensure_agent_auth_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let specs = self
            .agent_auth_catalog
            .as_ref()
            .into_iter()
            .flat_map(|catalog| catalog.methods.iter())
            .filter(|method| method.kind == AgentAuthMethodKind::Environment)
            .flat_map(|method| {
                method.environment.iter().map(move |variable| {
                    (
                        agent_auth_input_key(&method.id, &variable.name),
                        variable.secret,
                        variable.configured,
                    )
                })
            })
            .collect::<Vec<_>>();
        let active_keys = specs
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect::<BTreeSet<_>>();
        self.agent_auth_inputs
            .retain(|key, _| active_keys.contains(key));
        self.agent_auth_clear_values
            .retain(|key| active_keys.contains(key));
        for (key, secret, configured) in specs {
            if self.agent_auth_inputs.contains_key(&key) {
                continue;
            }
            let placeholder = if configured {
                management_locale_text(
                    "Configured - leave blank to keep",
                    "已配置，留空则保持不变",
                    "已設定，留空則保持不變",
                )
            } else {
                management_locale_text("Enter value", "输入凭据", "輸入憑證")
            };
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(secret)
            });
            let input_for_subscription = input.clone();
            let key_for_subscription = key.clone();
            self._subscriptions
                .push(cx.subscribe(&input, move |this, _, _: &InputEvent, cx| {
                    if !input_for_subscription.read(cx).value().trim().is_empty() {
                        this.agent_auth_clear_values.remove(&key_for_subscription);
                    }
                    cx.notify();
                }));
            self.agent_auth_inputs.insert(key, input);
        }
    }

    fn toggle_agent_auth_clear(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.agent_auth_clear_values.remove(&key) {
            self.agent_auth_clear_values.insert(key);
        }
        cx.notify();
    }

    fn authenticate_agent(&mut self, method_id: String, cx: &mut Context<Self>) {
        if self.mutation.is_some() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let Some(scope) = self.current_agent_auth_scope() else {
            return;
        };
        let Some(method) = self
            .agent_auth_catalog
            .as_ref()
            .and_then(|catalog| catalog.methods.iter().find(|method| method.id == method_id))
            .cloned()
        else {
            return;
        };
        let Ok(agent_id) = AgentId::parse(scope.0.clone()) else {
            return;
        };
        let provider_profile_id = scope
            .1
            .as_ref()
            .and_then(|profile_id| vibex_core::ProviderProfileId::parse(profile_id.clone()).ok());
        if method.kind == AgentAuthMethodKind::Environment && provider_profile_id.is_none() {
            self.agent_auth_error = Some(
                management_error_text(
                    "Select or create a Provider configuration before saving environment credentials",
                    "请先选择或创建模型供应商配置，再保存环境凭据",
                    "請先選擇或建立模型供應商設定，再儲存環境憑證",
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let values = method
            .environment
            .iter()
            .filter_map(|variable| {
                let key = agent_auth_input_key(&method.id, &variable.name);
                let value = self
                    .agent_auth_inputs
                    .get(&key)
                    .map(|input| input.read(cx).value().trim().to_string())
                    .filter(|value| !value.is_empty());
                let clear = self.agent_auth_clear_values.contains(&key);
                if variable.configured && value.is_none() && !clear {
                    return None;
                }
                Some(AgentAuthEnvironmentValue {
                    name: variable.name.clone(),
                    value,
                    secret: variable.secret,
                    optional: variable.optional,
                    clear,
                })
            })
            .collect::<Vec<_>>();
        let required_credential_cleared = values.iter().any(|value| value.clear && !value.optional);
        let generation = self.agent_auth_generation;
        self.mutation = Some(ManagementMutation::AgentAuth(method.id.clone()));
        self.agent_auth_error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let method_id_for_request = method.id.clone();
        let method_id_for_callback = method.id.clone();
        let scope_for_callback = scope.clone();
        let runtime_for_callback = runtime.clone();
        let agent_id_for_monitor = agent_id.clone();
        let provider_profile_id_for_monitor = provider_profile_id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            if method.kind == AgentAuthMethodKind::Environment && !values.is_empty() {
                runtime
                    .management()
                    .providers()
                    .management()
                    .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                        agent_id: agent_id.clone(),
                        provider_profile_id: provider_profile_id
                            .clone()
                            .expect("environment authentication profile was validated"),
                        method_id: method_id_for_request.clone(),
                        values,
                    })?;
            }
            if required_credential_cleared {
                let mut catalog = runtime
                    .agent()
                    .refresh_auth_methods(agent_id, provider_profile_id)
                    .await?;
                catalog.status = AgentAuthStatus::AuthenticationRequired;
                return Ok::<_, VibexError>((catalog, None, true));
            }
            let result = runtime
                .agent()
                .authenticate(AgentAuthenticateRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: provider_profile_id.clone(),
                    method_id: method_id_for_request,
                })
                .await?;
            let mut catalog = runtime
                .agent()
                .refresh_auth_methods(agent_id, provider_profile_id)
                .await?;
            catalog.status = if result.terminal.is_some() {
                AgentAuthStatus::Unknown
            } else {
                AgentAuthStatus::Authenticated
            };
            Ok::<_, VibexError>((catalog, result.terminal, false))
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                let operation_is_current = agent_auth_scope_matches(
                    this.agent_auth_generation,
                    this.agent_auth_scope.as_ref(),
                    generation,
                    &scope_for_callback,
                ) && matches!(
                    this.mutation,
                    Some(ManagementMutation::AgentAuth(ref action))
                        if action == &method_id_for_callback
                );
                if !operation_is_current {
                    if matches!(
                        this.mutation,
                        Some(ManagementMutation::AgentAuth(ref action))
                            if action == &method_id_for_callback
                    ) {
                        this.mutation = None;
                    }
                    if let Ok(Ok((_, Some(terminal), _))) = &outcome
                        && let Some(terminal_id) = terminal.terminal_id.as_ref()
                    {
                        let _ = runtime_for_callback
                            .terminals()
                            .manager()
                            .kill(terminal_id);
                    }
                    cx.notify();
                    return;
                }
                this.mutation = None;
                match outcome {
                    Ok(Ok((catalog, terminal, credential_removed))) => {
                        let terminal_started = terminal
                            .as_ref()
                            .is_some_and(|terminal| terminal.terminal_id.is_some());
                        this.agent_auth_catalog = Some(catalog);
                        this.agent_auth_error = None;
                        this.agent_auth_inputs.clear();
                        this.agent_auth_clear_values.clear();
                        if let Some(terminal) = terminal {
                            let terminal_id = terminal.terminal_id.clone();
                            this.clear_agent_auth_terminal();
                            this.agent_auth_terminal = Some(terminal);
                            if let Some(terminal_id) = terminal_id {
                                this.start_agent_auth_terminal_monitor(
                                    runtime_for_callback.clone(),
                                    scope_for_callback.clone(),
                                    generation,
                                    agent_id_for_monitor.clone(),
                                    provider_profile_id_for_monitor.clone(),
                                    terminal_id,
                                    active_locale,
                                    cx,
                                );
                            } else {
                                if let Some(catalog) = this.agent_auth_catalog.as_mut() {
                                    catalog.status = AgentAuthStatus::AuthenticationRequired;
                                }
                                this.agent_auth_terminal_state =
                                    Some(AgentAuthTerminalState::Failed);
                                this.agent_auth_error = Some(
                                    "agent_terminal_auth_terminal_missing: Interactive authentication terminal was not created"
                                        .to_string(),
                                );
                            }
                        }
                        this.notice = this.agent_auth_error.is_none().then(|| {
                            if credential_removed {
                                management_locale_text_for(
                                    active_locale,
                                    "Saved credential removed; sign-in is required",
                                    "已移除保存的凭据，需要重新登录",
                                    "已移除儲存的憑證，需要重新登入",
                                )
                            } else if terminal_started {
                                management_locale_text_for(
                                    active_locale,
                                    "Interactive sign-in terminal opened",
                                    "交互式登录终端已打开",
                                    "互動式登入終端已開啟",
                                )
                            } else {
                                management_locale_text_for(
                                    active_locale,
                                    "Agent authentication completed",
                                    "Agent 认证已完成",
                                    "Agent 驗證已完成",
                                )
                            }
                            .to_string()
                        });
                    }
                    Ok(Err(error)) => {
                        this.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.agent_auth_error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Authentication action failed",
                                "认证操作失败",
                                "驗證操作失敗",
                            )
                        ));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn logout_agent(&mut self, cx: &mut Context<Self>) {
        if self.mutation.is_some() {
            return;
        }
        let (Some(runtime), Some(scope)) = (self.runtime.clone(), self.current_agent_auth_scope())
        else {
            return;
        };
        let Ok(agent_id) = AgentId::parse(scope.0.clone()) else {
            return;
        };
        let provider_profile_id = scope
            .1
            .as_ref()
            .and_then(|profile_id| vibex_core::ProviderProfileId::parse(profile_id.clone()).ok());
        let generation = self.agent_auth_generation;
        self.mutation = Some(ManagementMutation::AgentAuth("logout".to_string()));
        self.agent_auth_error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let scope_for_callback = scope.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .agent()
                .logout(vibex_core::AgentLogoutRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: provider_profile_id.clone(),
                })
                .await?;
            let mut catalog = runtime
                .agent()
                .refresh_auth_methods(agent_id, provider_profile_id)
                .await?;
            catalog.status = AgentAuthStatus::AuthenticationRequired;
            Ok::<_, VibexError>(catalog)
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                let operation_is_current = agent_auth_scope_matches(
                    this.agent_auth_generation,
                    this.agent_auth_scope.as_ref(),
                    generation,
                    &scope_for_callback,
                ) && matches!(
                    this.mutation,
                    Some(ManagementMutation::AgentAuth(ref action)) if action == "logout"
                );
                if !operation_is_current {
                    if matches!(
                        this.mutation,
                        Some(ManagementMutation::AgentAuth(ref action)) if action == "logout"
                    ) {
                        this.mutation = None;
                    }
                    cx.notify();
                    return;
                }
                this.mutation = None;
                match outcome {
                    Ok(Ok(catalog)) => {
                        this.agent_auth_catalog = Some(catalog);
                        this.clear_agent_auth_terminal();
                        this.notice = Some(
                            management_locale_text_for(
                                active_locale,
                                "Agent signed out",
                                "Agent 已退出登录",
                                "Agent 已登出",
                            )
                            .to_string(),
                        );
                    }
                    Ok(Err(error)) => {
                        this.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.agent_auth_error = Some(format!(
                            "{}: {error}",
                            management_error_text("Logout failed", "退出登录失败", "登出失敗",)
                        ));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn clear_agent_auth_terminal(&mut self) {
        self.agent_auth_terminal_monitor_task = None;
        if let (Some(runtime), Some(terminal_id)) = (
            self.runtime.as_ref(),
            self.agent_auth_terminal
                .as_ref()
                .and_then(|terminal| terminal.terminal_id.as_ref()),
        ) {
            let _ = runtime.terminals().manager().kill(terminal_id);
        }
        self.agent_auth_terminal = None;
        self.agent_auth_terminal_surface = None;
        self.agent_auth_terminal_state = None;
    }

    fn close_agent_auth_terminal(&mut self, cx: &mut Context<Self>) {
        self.clear_agent_auth_terminal();
        cx.notify();
    }

    fn ensure_agent_auth_terminal_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal_id) = self
            .agent_auth_terminal
            .as_ref()
            .and_then(|terminal| terminal.terminal_id.clone())
        else {
            self.agent_auth_terminal_surface = None;
            return;
        };
        if self
            .agent_auth_terminal_surface
            .as_ref()
            .is_some_and(|(id, _)| id == terminal_id.as_str())
        {
            return;
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let manager = runtime.terminals().manager();
        let Ok(snapshot) = manager.snapshot(&terminal_id) else {
            return;
        };
        let workspace_root = PathBuf::from(&snapshot.session.cwd);
        let surface = cx.new(|cx| {
            TerminalSurface::from_shared_session(
                manager,
                workspace_root,
                snapshot.session,
                window,
                cx,
            )
        });
        surface.update(cx, |surface, cx| surface.set_active(true, cx));
        self.agent_auth_terminal_surface = Some((terminal_id.as_str().to_string(), surface));
    }

    #[allow(clippy::too_many_arguments)]
    fn start_agent_auth_terminal_monitor(
        &mut self,
        runtime: Arc<DesktopRuntime>,
        scope: (String, Option<String>),
        generation: u64,
        agent_id: AgentId,
        provider_profile_id: Option<vibex_core::ProviderProfileId>,
        terminal_id: vibex_core::TerminalId,
        active_locale: ResolvedLocale,
        cx: &mut Context<Self>,
    ) {
        self.agent_auth_terminal_state = Some(AgentAuthTerminalState::Running);
        let manager = runtime.terminals().manager();
        let terminal_id_for_runner = terminal_id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let exit_status = loop {
                if let Some(status) = manager.process_exit_status(&terminal_id_for_runner)? {
                    break status;
                }
                tokio::time::sleep(AGENT_AUTH_TERMINAL_POLL_INTERVAL).await;
            };
            if let Some(error) = terminal_auth_exit_error(&exit_status) {
                return Ok::<_, VibexError>(AgentAuthTerminalCompletion::AuthenticationRequired(
                    error,
                ));
            }
            let (catalog, refresh_error) = match runtime
                .agent()
                .refresh_auth_methods(agent_id, provider_profile_id)
                .await
            {
                Ok(mut catalog) => {
                    catalog.status = AgentAuthStatus::Authenticated;
                    (Some(catalog), None)
                }
                Err(error) => (None, Some(error)),
            };
            Ok(AgentAuthTerminalCompletion::Authenticated {
                catalog,
                refresh_error,
            })
        });
        let entity = cx.weak_entity();
        self.agent_auth_terminal_monitor_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if !agent_auth_scope_matches(
                    this.agent_auth_generation,
                    this.agent_auth_scope.as_ref(),
                    generation,
                    &scope,
                ) || this
                    .agent_auth_terminal
                    .as_ref()
                    .and_then(|terminal| terminal.terminal_id.as_ref())
                    != Some(&terminal_id)
                {
                    return;
                }
                match outcome {
                    Ok(Ok(AgentAuthTerminalCompletion::Authenticated {
                        catalog,
                        refresh_error,
                    })) => {
                        if let Some(catalog) = catalog {
                            this.agent_auth_catalog = Some(catalog);
                        } else if let Some(catalog) = this.agent_auth_catalog.as_mut() {
                            catalog.status = AgentAuthStatus::Authenticated;
                        }
                        this.agent_auth_terminal_state = Some(AgentAuthTerminalState::Succeeded);
                        this.agent_auth_error =
                            refresh_error.map(|error| format!("{}: {}", error.code, error.message));
                        this.notice = Some(
                            management_locale_text_for(
                                active_locale,
                                "Interactive Agent sign-in completed",
                                "Agent 交互式登录已完成",
                                "Agent 互動式登入已完成",
                            )
                            .to_string(),
                        );
                    }
                    Ok(Ok(AgentAuthTerminalCompletion::AuthenticationRequired(error)))
                    | Ok(Err(error)) => {
                        if let Some(catalog) = this.agent_auth_catalog.as_mut() {
                            catalog.status = AgentAuthStatus::AuthenticationRequired;
                        }
                        this.agent_auth_terminal_state = Some(AgentAuthTerminalState::Failed);
                        this.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                        this.notice = None;
                    }
                    Err(error) => {
                        if let Some(catalog) = this.agent_auth_catalog.as_mut() {
                            catalog.status = AgentAuthStatus::AuthenticationRequired;
                        }
                        this.agent_auth_terminal_state = Some(AgentAuthTerminalState::Failed);
                        this.agent_auth_error =
                            Some(format!("agent_terminal_auth_monitor_failed: {error}"));
                        this.notice = None;
                    }
                }
                cx.notify();
            });
        }));
    }

    fn sync_projection_editor(&mut self) {
        let selected_profile_id = self.selected_provider_profile_id.as_deref();
        let selected_agent_id = self.selected_agent_id.as_deref();
        let projection = selected_profile_id
            .and_then(|profile_id| {
                self.projection_states
                    .iter()
                    .find(|projection| projection.legacy_profile_id.as_deref() == Some(profile_id))
            })
            .or_else(|| {
                selected_agent_id.and_then(|agent_id| {
                    self.projection_states
                        .iter()
                        .find(|projection| projection.agent_id == agent_id)
                })
            });
        if let Some(projection) = projection {
            self.projection_editor
                .replace_capability(projection.capability.clone());
            self.projection_editor.preview = projection.preview.clone();
        } else {
            self.projection_editor.capability = None;
            self.projection_editor.preview = None;
        }
    }

    pub fn active_section(&self) -> ManagementSection {
        self.navigation.active
    }

    pub fn select_section(
        &mut self,
        section: ManagementSection,
        discard_dirty: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let section = match section {
            ManagementSection::ModelProviders => ManagementSection::Agents,
            ManagementSection::PromptsHooks
            | ManagementSection::Scheduled
            | ManagementSection::Automation
            | ManagementSection::Relay
            | ManagementSection::Recovery => ManagementSection::Advanced,
            section => section,
        };
        if !self.navigation.switch(section, discard_dirty) {
            self.notice = Some(
                management_locale_text(
                    "Unsaved changes are still present; confirm discard first",
                    "仍有未保存的修改，请先确认是否放弃",
                    "仍有未儲存的修改，請先確認是否放棄",
                )
                .into(),
            );
            cx.notify();
            return false;
        }
        self.error = None;
        cx.notify();
        true
    }

    fn request_section_switch(
        &mut self,
        section: ManagementSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if section == self.navigation.active {
            return;
        }
        if !self.navigation.is_dirty(self.navigation.active) {
            self.select_section(section, false, cx);
            return;
        }
        let entity = cx.weak_entity();
        let current = management_secondary_label(self.navigation.active);
        let title = match locale::current_locale() {
            ResolvedLocale::En => format!("Discard unsaved {current} changes?"),
            ResolvedLocale::ZhCn => format!("放弃未保存的{current}修改？"),
            ResolvedLocale::ZhTw => format!("放棄未儲存的{current}修改？"),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            dialog
                .title(title.clone())
                .child(management_locale_text(
                    "Pending backend actions and errors remain visible; only the local form draft is discarded.",
                    "后台操作和错误信息仍会保留，仅放弃当前页面的本地表单草稿。",
                    "後台操作與錯誤資訊仍會保留，僅放棄目前頁面的本機表單草稿。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(
                            gpui_component::dialog::DialogClose::new().child(
                                Button::new("cancel-management-navigation")
                                    .outline()
                                    .label(management_locale_text(
                                        "Keep editing",
                                        "继续编辑",
                                        "繼續編輯",
                                    )),
                            ),
                        )
                        .child(
                            gpui_component::dialog::DialogAction::new().child(
                                Button::new("confirm-management-navigation")
                                    .danger()
                                    .label(management_locale_text(
                                        "Discard and switch",
                                        "放弃并切换",
                                        "放棄並切換",
                                    )),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.select_section(section, true, cx);
                    });
                    true
                })
        });
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        let default_scope = management_provider_default_scope(self.pairing_workspace_id.clone());
        let entity = cx.weak_entity();
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { load_snapshot(runtime, default_scope).await },
            );
        self.refresh_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.loading = false;
                match outcome {
                    Ok(Ok(snapshot)) => this.apply_snapshot(snapshot, cx),
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message))
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Config center refresh failed",
                                "配置中心刷新失败",
                                "配置中心重新整理失敗",
                            )
                        ))
                    }
                }
                cx.notify();
            });
        }));
    }

    fn apply_snapshot(&mut self, snapshot: ManagementSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot.center;
        self.provider_profiles = snapshot.provider_profiles;
        self.provider_display_order = snapshot.provider_display_order;
        self.model_provider_agent_ids = snapshot.model_provider_agent_ids;
        self.acp_configs = snapshot.acp_configs;
        self.native_import_preview = snapshot.native_import_preview;
        self.agent_profile_states = snapshot.agent_profile_states;
        self.projection_states = snapshot.projection_states;
        if !self.snapshot.agents.iter().any(|agent| {
            self.selected_agent_id.as_deref() == Some(agent.id.as_str())
                && (agent.added || agent.managed_install.managed)
        }) {
            self.selected_agent_id = self
                .snapshot
                .agents
                .iter()
                .find(|agent| agent.added)
                .map(|agent| agent.id.as_str().to_string());
        }
        let selected_agent_id = self.selected_agent_id.as_deref();
        let selected_profile_is_valid =
            self.selected_provider_profile_id
                .as_deref()
                .is_some_and(|selected_profile_id| {
                    self.snapshot.profiles.iter().any(|profile| {
                        Some(profile.agent_id.as_str()) == selected_agent_id
                            && profile.id == selected_profile_id
                    })
                });
        if !selected_profile_is_valid {
            let previous_profile_id = self.selected_provider_profile_id.take();
            self.selected_provider_profile_id = selected_agent_id.and_then(|agent_id| {
                self.agent_profile_states
                    .iter()
                    .find(|state| state.agent_id == agent_id && state.is_default)
                    .map(|state| state.profile_id.clone())
                    .or_else(|| {
                        self.snapshot
                            .profiles
                            .iter()
                            .find(|profile| profile.agent_id == agent_id)
                            .map(|profile| profile.id.clone())
                    })
            });
            if previous_profile_id != self.selected_provider_profile_id {
                self.selected_acp_profile_id = None;
                self.acp_config_draft = None;
                self.native_export_preview = None;
            }
        }
        if !self
            .snapshot
            .mcp_servers
            .iter()
            .any(|server| self.selected_mcp_id.as_deref() == Some(server.id.as_str()))
        {
            self.selected_mcp_id = self
                .snapshot
                .mcp_servers
                .first()
                .map(|server| server.id.as_str().to_string());
        }
        if !self
            .snapshot
            .skills
            .iter()
            .any(|skill| self.selected_skill_id.as_deref() == Some(skill.id.as_str()))
        {
            self.selected_skill_id = self
                .snapshot
                .skills
                .first()
                .map(|skill| skill.id.as_str().to_string());
        }
        if !self
            .snapshot
            .scheduled
            .iter()
            .any(|task| self.selected_scheduled_task_id.as_deref() == Some(task.id.as_str()))
        {
            self.selected_scheduled_task_id = None;
        }
        self.health_summaries = snapshot.health_summaries;
        self.capability_summaries = snapshot.capability_summaries;
        self.usage_summaries = snapshot.usage_summaries;
        self.native_exports = snapshot.native_exports;
        self.device_count = snapshot.device_count;
        self.revoked_device_count = snapshot.revoked_device_count;
        self.audit_count = snapshot.audit_count;
        self.scheduled_runs = snapshot.scheduled_runs;
        self.scheduled_attention = snapshot.scheduled_attention;
        self.scheduled_audit = snapshot.scheduled_audit;
        self.automation_runs = snapshot.automation_runs;
        self.automation_steps = snapshot.automation_steps;
        self.devices = snapshot.devices;
        self.sync_projection_editor();
        self.load_agent_auth(false, cx);
        if self.graph_draft.graph_id.is_none() {
            if let Some(graph) = self.snapshot.graphs.first() {
                self.graph_draft = AutomationGraphDraft::from_graph(graph);
            }
        } else if !self.graph_draft.dirty
            && let Some(graph) = self
                .snapshot
                .graphs
                .iter()
                .find(|graph| self.graph_draft.graph_id.as_deref() == Some(graph.id.as_str()))
        {
            self.graph_draft = AutomationGraphDraft::from_graph(graph);
        }
        if std::mem::take(&mut self.discover_agents_after_refresh) {
            self.discover_local_agents(cx);
        }
    }

    fn export_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.mutation.is_some() {
            self.notice = Some(
                management_locale_text(
                    "Another management action is still pending",
                    "另一项配置操作仍在处理中",
                    "另一項配置操作仍在處理中",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some(
                management_error_text(
                    "Management runtime is not connected",
                    "配置中心运行时未连接",
                    "配置中心執行階段未連線",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let mutation = ManagementMutation::DiagnosticsExport;
        let key = mutation.key();
        self.mutation = Some(mutation);
        self.error = None;
        self.diagnostics.status = "exporting".into();
        self.diagnostics.error_code = None;
        let entity = cx.weak_entity();
        let management = runtime.management();
        let management_for_runner = management.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let destination = management_for_runner.diagnostics_destination();
            management_for_runner
                .diagnostics()
                .export_to_path(vibex_core::DiagnosticBundleRequest::default(), &destination)?;
            Ok::<_, VibexError>("diagnostics exported with redaction verification".to_string())
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(message)) => {
                        this.diagnostics.status = "succeeded".into();
                        this.diagnostics.redaction_verified = true;
                        this.diagnostics.destination =
                            Some(management.diagnostics_destination().display().to_string());
                        this.notice = Some(format!("{key}: {message}"));
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.diagnostics.status = "error".into();
                        this.diagnostics.error_code = Some(error.code.clone());
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.diagnostics.status = "error".into();
                        this.diagnostics.error_code = Some("diagnostics_export_task_failed".into());
                        this.error = Some(format!("management action failed: {error}"));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn open_profile_creator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_projection_editor();
        self.projection_editor.preview = None;
        self.profile_name.update(cx, |state, cx| {
            state.set_value(
                management_locale_text(
                    "Local model provider config",
                    "本地模型供应商配置",
                    "本機模型供應商設定",
                ),
                window,
                cx,
            )
        });
        self.profile_note
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_website_url
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_base_url
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_model_draft
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_model_edit_id
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_model_edit_name
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_api_key.update(cx, |state, cx| {
            state.set_masked(true, window, cx);
            state.set_value("", window, cx);
        });
        self.editing_profile_id = None;
        self.projection_editor.draft_revision = 0;
        self.profile_secret_touched = false;
        self.projection_editor.set_secret_intent(false, false);
        self.profile_secret_loading = false;
        self.profile_configured_models.clear();
        self.profile_model_edit_index = None;
        self.profile_model_edit_wire_api = None;
        self.profile_provider_options = vibex_core::ProviderOptions::empty();
        self.rebuild_profile_protocol_base_urls(window, cx);
        self.profile_editor_open = true;
        self.error = None;
        self.navigation.mark_dirty(ManagementSection::Agents, false);
        self.present_profile_editor_dialog(window, cx);
        cx.notify();
    }

    fn open_profile_editor(
        &mut self,
        profile: vibex_desktop_model::ProviderProfileProjection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_provider_profile(profile.id.clone(), cx);
        let full_profile = self
            .provider_profiles
            .iter()
            .find(|candidate| candidate.id.as_str() == profile.id)
            .cloned();
        let website_url = full_profile
            .as_ref()
            .and_then(|profile| {
                profile
                    .provider_options
                    .entries
                    .iter()
                    .find(|entry| entry.key == PROVIDER_OPTION_WEBSITE_URL)
            })
            .map(|entry| entry.value.clone())
            .unwrap_or_default();
        self.profile_name.update(cx, |state, cx| {
            state.set_value(profile.display_name.clone(), window, cx)
        });
        self.profile_note.update(cx, |state, cx| {
            state.set_value(
                profile.account_alias.clone().unwrap_or_default(),
                window,
                cx,
            )
        });
        self.profile_website_url
            .update(cx, |state, cx| state.set_value(website_url, window, cx));
        self.profile_base_url.update(cx, |state, cx| {
            state.set_value(profile.base_url.clone().unwrap_or_default(), window, cx)
        });
        self.profile_model_draft
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_model_edit_id
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_model_edit_name
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_api_key.update(cx, |state, cx| {
            state.set_masked(true, window, cx);
            state.set_value("", window, cx);
        });
        self.profile_configured_models = full_profile
            .as_ref()
            .map(|profile| profile.configured_models.clone())
            .unwrap_or_default();
        self.profile_model_edit_index = None;
        self.profile_model_edit_wire_api = None;
        self.profile_provider_options = full_profile
            .map(|profile| profile.provider_options)
            .unwrap_or_else(vibex_core::ProviderOptions::empty);
        self.rebuild_profile_protocol_base_urls(window, cx);
        self.editing_profile_id = Some(profile.id.clone());
        self.projection_editor.draft_revision = 0;
        self.profile_secret_touched = false;
        self.projection_editor.set_secret_intent(false, false);
        self.profile_secret_loading = false;
        self.profile_editor_open = true;
        self.error = None;
        self.navigation.mark_dirty(ManagementSection::Agents, false);
        self.present_profile_editor_dialog(window, cx);
        cx.notify();
    }

    fn close_profile_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_profile_editor_state(window, cx);
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
        }
    }

    fn reset_profile_editor_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_api_key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.profile_editor_open = false;
        self.editing_profile_id = None;
        self.projection_editor.draft_revision = 0;
        self.profile_secret_touched = false;
        self.projection_editor.set_secret_intent(false, false);
        self.profile_secret_loading = false;
        self.profile_configured_models.clear();
        self.profile_model_edit_index = None;
        self.profile_model_edit_wire_api = None;
        self.profile_provider_options = vibex_core::ProviderOptions::empty();
        self.profile_protocol_base_urls.clear();
        self.navigation.mark_dirty(ManagementSection::Agents, false);
        cx.notify();
    }

    fn rebuild_profile_protocol_base_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_protocol_base_urls.clear();
        for wire_api in self.projection_editor.supported_wire_apis() {
            let option_key = wire_api.protocol_base_url_option_key();
            let value = provider_option_value(&self.profile_provider_options, &option_key)
                .unwrap_or_default()
                .to_string();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder("https://provider.example")
            });
            self._subscriptions
                .push(cx.subscribe(&input, |this, _, _: &InputEvent, cx| {
                    this.projection_editor.mark_draft_changed();
                    this.navigation.mark_dirty(ManagementSection::Agents, true);
                    cx.notify();
                }));
            self.profile_protocol_base_urls.push((wire_api, input));
        }
    }

    fn present_profile_editor_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let center = cx.entity();
        let dialog_center = center.clone();
        let dialog_content = cx.new(|cx| ManagementProfileDialog::new(center.clone(), cx));
        let dialog_width = (f32::from(window.viewport_size().width) - 32.0).clamp(320.0, 544.0);
        let dialog_height = (f32::from(window.viewport_size().height) - 32.0).clamp(280.0, 544.0);
        let title = if self.editing_profile_id.is_some() {
            management_locale_text("Update", "更新", "更新")
        } else {
            management_locale_text("Create", "创建", "建立")
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let dialog_center = dialog_center.clone();
            dialog
                .title(title)
                .w(px(dialog_width))
                .max_w(px(dialog_width))
                .h(px(dialog_height))
                .child(dialog_content.clone())
                .on_close(move |_, window, cx| {
                    dialog_center.update(cx, |center, cx| {
                        center.reset_profile_editor_state(window, cx);
                    });
                })
        });
    }

    fn add_profile_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model_id = self.profile_model_draft.read(cx).value().trim().to_string();
        if model_id.is_empty() {
            return;
        }
        merge_provider_models(
            &mut self.profile_configured_models,
            vec![vibex_core::ProviderConfiguredModel {
                id: model_id,
                display_name: None,
                enabled: true,
                wire_api: None,
            }],
        );
        self.profile_model_draft
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.navigation.mark_dirty(ManagementSection::Agents, true);
        cx.notify();
    }

    fn toggle_profile_model(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        if let Some(model) = self.profile_configured_models.get_mut(index) {
            model.enabled = enabled;
            self.navigation.mark_dirty(ManagementSection::Agents, true);
            cx.notify();
        }
    }

    fn remove_profile_model(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.profile_configured_models.len() {
            self.profile_configured_models.remove(index);
            self.profile_model_edit_index = match self.profile_model_edit_index {
                Some(edit_index) if edit_index == index => {
                    self.profile_model_edit_wire_api = None;
                    None
                }
                Some(edit_index) if edit_index > index => Some(edit_index - 1),
                current => current,
            };
            self.navigation.mark_dirty(ManagementSection::Agents, true);
            cx.notify();
        }
    }

    fn open_profile_model_editor(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.profile_configured_models.get(index).cloned() else {
            return;
        };
        let was_dirty = self.navigation.is_dirty(ManagementSection::Agents);
        self.profile_model_edit_wire_api = model.wire_api;
        self.profile_model_edit_id
            .update(cx, |state, cx| state.set_value(model.id, window, cx));
        self.profile_model_edit_name.update(cx, |state, cx| {
            state.set_value(model.display_name.unwrap_or_default(), window, cx)
        });
        self.profile_model_edit_index = Some(index);
        self.navigation
            .mark_dirty(ManagementSection::Agents, was_dirty);
        cx.notify();
    }

    fn close_profile_model_editor(&mut self, cx: &mut Context<Self>) {
        self.profile_model_edit_index = None;
        self.profile_model_edit_wire_api = None;
        cx.notify();
    }

    fn save_profile_model_editor(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.profile_model_edit_index else {
            return;
        };
        let model_id = self
            .profile_model_edit_id
            .read(cx)
            .value()
            .trim()
            .to_string();
        if model_id.is_empty() {
            self.error = Some(
                management_error_text(
                    "Model id is required",
                    "模型 ID 不能为空",
                    "模型 ID 不能為空",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let display_name = self
            .profile_model_edit_name
            .read(cx)
            .value()
            .trim()
            .to_string();
        let Some(model) = self.profile_configured_models.get_mut(index) else {
            self.profile_model_edit_index = None;
            self.profile_model_edit_wire_api = None;
            cx.notify();
            return;
        };
        model.id = model_id;
        model.display_name = (!display_name.is_empty()).then_some(display_name);
        model.wire_api = self.profile_model_edit_wire_api;
        self.profile_model_edit_index = None;
        self.profile_model_edit_wire_api = None;
        self.navigation.mark_dirty(ManagementSection::Agents, true);
        cx.notify();
    }

    fn set_profile_model_wire_api(
        &mut self,
        index: usize,
        wire_api: Option<vibex_core::ProviderModelWireApi>,
        cx: &mut Context<Self>,
    ) {
        if wire_api.is_some_and(|wire_api| !self.projection_editor.accepts_wire_api(wire_api)) {
            self.error = Some(
                "agent_model_interface_unsupported: model interface is not supported by the selected Agent projection descriptor"
                    .to_string(),
            );
            cx.notify();
            return;
        }
        if self.profile_model_edit_index == Some(index) {
            self.profile_model_edit_wire_api = wire_api;
            self.navigation.mark_dirty(ManagementSection::Agents, true);
            cx.notify();
        }
    }

    fn duplicate_provider_profile(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let Some(profile) = self
            .provider_profiles
            .iter()
            .find(|profile| profile.id.as_str() == profile_id)
            .cloned()
        else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        let copy_name = match active_locale {
            ResolvedLocale::En => format!("{} copy", profile.display_name),
            ResolvedLocale::ZhCn => format!("{} 副本", profile.display_name),
            ResolvedLocale::ZhTw => format!("{} 副本", profile.display_name),
        };
        self.begin_simple_task(ManagementMutation::ProfileCreate, cx, async move {
            let agent_id = profile.agent_id.clone();
            let created = runtime
                .management()
                .providers()
                .management()
                .create_agent_model_provider_profile(
                    vibex_core::AgentModelProviderProfileCreateRequest {
                        agent_id: agent_id.clone(),
                        display_name: copy_name,
                        account_alias: profile.account_alias,
                        base_url: profile.base_url,
                        default_model: profile.default_model,
                        small_model: profile.small_model,
                        large_model: profile.large_model,
                        configured_models: profile.configured_models,
                        reasoning_effort: profile.reasoning_effort,
                        sandbox_defaults: Some(profile.sandbox_defaults),
                        network_defaults: Some(profile.network_defaults),
                        permission_defaults: Some(profile.permission_defaults),
                        provider_options: Some(profile.provider_options),
                        secret_references: Vec::new(),
                    },
                )?;
            let message = match active_locale {
                ResolvedLocale::En => {
                    format!("Duplicated provider configuration {}", created.display_name)
                }
                ResolvedLocale::ZhCn => {
                    format!("已复制供应商配置 {}", created.display_name)
                }
                ResolvedLocale::ZhTw => {
                    format!("已複製供應商配置 {}", created.display_name)
                }
            };
            Ok(message)
        });
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.snapshot.agents.iter().find(|agent| {
            agent.added && self.selected_agent_id.as_deref() == Some(agent.id.as_str())
        }) else {
            self.error = Some(
                management_error_text(
                    "Select an Agent before creating a provider profile",
                    "请先选择 Agent，再创建供应商配置",
                    "請先選擇 Agent，再建立供應商配置",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let name = self.profile_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some(
                management_error_text(
                    "Provider profile name is required",
                    "供应商配置名称不能为空",
                    "供應商配置名稱不能為空",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let note = self.profile_note.read(cx).value().trim().to_string();
        let website_url = self.profile_website_url.read(cx).value().trim().to_string();
        let base_url = self.profile_base_url.read(cx).value().trim().to_string();
        let api_key = self.profile_api_key.read(cx).value().trim().to_string();
        let configured_models = normalized_provider_models(&self.profile_configured_models);
        let default_model = configured_models
            .iter()
            .find(|model| model.enabled)
            .map(|model| model.id.clone());
        let mut provider_options = with_provider_option(
            self.profile_provider_options.clone(),
            PROVIDER_OPTION_WEBSITE_URL,
            (!website_url.is_empty()).then_some(website_url),
        );
        for (wire_api, input) in &self.profile_protocol_base_urls {
            let value = input.read(cx).value().trim().to_string();
            provider_options = with_provider_option(
                provider_options,
                &wire_api.protocol_base_url_option_key(),
                (!value.is_empty()).then_some(value),
            );
        }
        let editing_profile_id = match self
            .editing_profile_id
            .as_deref()
            .map(vibex_core::ProviderProfileId::parse)
            .transpose()
        {
            Ok(id) => id,
            Err(_) => {
                self.error = Some(
                    management_error_text(
                        "Provider profile identity is invalid",
                        "供应商配置标识无效",
                        "供應商配置識別碼無效",
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
        };
        let agent_id = agent.id.clone();
        let secret_touched = self.profile_secret_touched
            && self.projection_editor.credential_surface() == ProjectionCredentialSurface::ApiKey;
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some(
                management_error_text(
                    "Management runtime is not connected",
                    "配置中心运行时未连接",
                    "配置中心執行階段未連線",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        self.mutation = Some(
            editing_profile_id
                .as_ref()
                .map(|id| ManagementMutation::ProfileUpdate(id.as_str().to_string()))
                .unwrap_or(ManagementMutation::ProfileCreate),
        );
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let providers = runtime.management().providers().management();
            let updating = editing_profile_id.is_some();
            let profile = if let Some(provider_profile_id) = editing_profile_id {
                providers.update_agent_model_provider_profile(
                    vibex_core::AgentModelProviderProfileUpdateRequest {
                        agent_id: agent_id.clone(),
                        provider_profile_id,
                        display_name: Some(name),
                        status: None,
                        account_alias: (!note.is_empty()).then_some(note),
                        base_url: (!base_url.is_empty()).then_some(base_url),
                        default_model: default_model.clone(),
                        small_model: None,
                        large_model: None,
                        configured_models: Some(configured_models.clone()),
                        reasoning_effort: None,
                        sandbox_defaults: None,
                        network_defaults: None,
                        permission_defaults: None,
                        provider_options: Some(provider_options.clone()),
                    },
                )?
            } else {
                providers.create_agent_model_provider_profile(
                    vibex_core::AgentModelProviderProfileCreateRequest {
                        agent_id: agent_id.clone(),
                        display_name: name,
                        account_alias: (!note.is_empty()).then_some(note),
                        base_url: (!base_url.is_empty()).then_some(base_url),
                        default_model,
                        small_model: None,
                        large_model: None,
                        configured_models,
                        reasoning_effort: None,
                        sandbox_defaults: None,
                        network_defaults: None,
                        permission_defaults: None,
                        provider_options: Some(provider_options),
                        secret_references: Vec::new(),
                    },
                )?
            };
            let saved_profile_id = profile.id.as_str().to_string();
            if (!updating && !api_key.is_empty()) || (updating && secret_touched) {
                let clear = api_key.is_empty();
                providers.update_agent_model_provider_profile_secret_value(
                    vibex_core::AgentModelProviderProfileSecretValueUpdateRequest {
                        agent_id: agent_id.clone(),
                        provider_profile_id: profile.id.clone(),
                        value: (!clear).then_some(api_key),
                        clear,
                    },
                )?;
            }
            let message = if updating {
                management_locale_text_for(
                    active_locale,
                    "Provider configuration updated",
                    "供应商配置已更新",
                    "供應商配置已更新",
                )
                .to_string()
            } else {
                management_locale_text_for(
                    active_locale,
                    "Provider configuration created",
                    "供应商配置已创建",
                    "供應商配置已建立",
                )
                .to_string()
            };
            Ok::<_, VibexError>((message, saved_profile_id))
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok((message, saved_profile_id))) => {
                        this.navigation.mark_dirty(ManagementSection::Agents, false);
                        this.profile_editor_open = false;
                        this.editing_profile_id = None;
                        this.selected_provider_profile_id = Some(saved_profile_id);
                        this.profile_secret_touched = false;
                        this.projection_editor.set_secret_intent(false, false);
                        this.notice = Some(message);
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Provider configuration save failed",
                                "供应商配置保存失败",
                                "供應商配置儲存失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn run_provider_health_probe(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderProbe("health".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .run_health_probes(vibex_core::ProviderRunHealthProbesRequest {
                        provider_profile_ids: None,
                        probe_kinds: None,
                    })
                    .map(|result| match active_locale {
                        ResolvedLocale::En => {
                            format!("Completed {} health probe(s)", result.results.len())
                        }
                        ResolvedLocale::ZhCn => {
                            format!("已完成 {} 项健康检查", result.results.len())
                        }
                        ResolvedLocale::ZhTw => {
                            format!("已完成 {} 項健康檢查", result.results.len())
                        }
                    })
            },
        );
    }

    fn run_provider_capability_probe(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderProbe("capability".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .run_capability_probes(vibex_core::ProviderRunCapabilityProbesRequest {
                        provider_profile_ids: None,
                        force_refresh: true,
                    })
                    .map(|result| match active_locale {
                        ResolvedLocale::En => {
                            format!("Completed {} capability probe(s)", result.results.len())
                        }
                        ResolvedLocale::ZhCn => {
                            format!("已完成 {} 项能力检查", result.results.len())
                        }
                        ResolvedLocale::ZhTw => {
                            format!("已完成 {} 項能力檢查", result.results.len())
                        }
                    })
            },
        );
    }

    fn test_provider_profile(
        &mut self,
        profile_id: String,
        agent_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Ok(provider_profile_id), Ok(agent_id), Some(runtime)) = (
            vibex_core::ProviderProfileId::parse(profile_id.clone()),
            AgentId::parse(agent_id),
            self.runtime.clone(),
        ) else {
            self.error = Some(
                management_error_text(
                    "Provider profile identity is invalid",
                    "供应商配置标识无效",
                    "供應商配置識別碼無效",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderProbe(profile_id),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .test_agent_model_provider_profile(
                        vibex_core::AgentModelProviderProfileTestRequest {
                            agent_id,
                            provider_profile_id,
                        },
                    )
                    .map(|result| {
                        format!(
                            "{}: {}",
                            management_provider_test_status_label(result.status, active_locale),
                            result.message
                        )
                    })
            },
        );
    }

    fn fetch_provider_models(
        &mut self,
        profile_id: String,
        agent_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Ok(provider_profile_id), Ok(agent_id), Some(runtime)) = (
            vibex_core::ProviderProfileId::parse(profile_id.clone()),
            AgentId::parse(agent_id),
            self.runtime.clone(),
        ) else {
            self.error = Some(
                management_error_text(
                    "Provider profile identity is invalid",
                    "供应商配置标识无效",
                    "供應商配置識別碼無效",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        self.mutation = Some(ManagementMutation::ProviderProbe(format!(
            "models:{profile_id}"
        )));
        self.error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .fetch_agent_model_provider_profile_models(
                    vibex_core::AgentModelProviderProfileFetchModelsRequest {
                        agent_id,
                        provider_profile_id,
                    },
                )
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(result)) => {
                        let count = result.models.len();
                        merge_provider_models(&mut this.profile_configured_models, result.models);
                        this.profile_model_edit_index = None;
                        this.profile_model_edit_wire_api = None;
                        this.navigation.mark_dirty(ManagementSection::Agents, true);
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => format!("Fetched {count} model(s)"),
                            ResolvedLocale::ZhCn => format!("已拉取 {count} 个模型"),
                            ResolvedLocale::ZhTw => format!("已擷取 {count} 個模型"),
                        });
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Model detection failed",
                                "模型探测失败",
                                "模型探測失敗",
                            )
                        ));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn confirm_managed_delete(
        &mut self,
        target: ManagedDeleteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let label = target.label().to_string();
        let active_locale = locale::current_locale();
        let uninstalling_agent = matches!(target, ManagedDeleteTarget::Agent { .. });
        let title = match (active_locale, uninstalling_agent) {
            (ResolvedLocale::En, true) => format!("Uninstall {label}?"),
            (ResolvedLocale::ZhCn, true) => format!("卸载“{label}”？"),
            (ResolvedLocale::ZhTw, true) => format!("解除安裝「{label}」？"),
            (ResolvedLocale::En, false) => format!("Delete {label}?"),
            (ResolvedLocale::ZhCn, false) => format!("删除“{label}”？"),
            (ResolvedLocale::ZhTw, false) => format!("刪除「{label}」？"),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let target = target.clone();
            dialog
                .title(title.clone())
                .child(if uninstalling_agent {
                    management_locale_text_for(
                        active_locale,
                        "The managed Agent runtime and its cached versions will be removed.",
                        "托管的 Agent 运行时及其缓存版本将被移除。",
                        "託管的 Agent 執行環境及其快取版本將被移除。",
                    )
                } else {
                    management_locale_text_for(
                        active_locale,
                        "This durable action is soft-deleted where supported and remains auditable.",
                        "此操作会持久化；支持软删除的数据仍会保留审计记录。",
                        "此操作會持久化；支援軟刪除的資料仍會保留稽核記錄。",
                    )
                })
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(gpui_component::dialog::DialogClose::new().child(
                            Button::new("cancel-managed-delete").outline().label(
                                management_locale_text_for(active_locale, "Cancel", "取消", "取消"),
                            ),
                        ))
                        .child(gpui_component::dialog::DialogAction::new().child(
                            Button::new("confirm-managed-delete").danger().label(if uninstalling_agent {
                                management_locale_text_for(
                                    active_locale,
                                    "Uninstall",
                                    "卸载",
                                    "解除安裝",
                                )
                            } else {
                                management_locale_text_for(active_locale, "Delete", "删除", "刪除")
                            }),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        let Some(runtime) = this.runtime.clone() else {
                            return;
                        };
                        let providers = runtime.management().providers().management();
                        match target.clone() {
                            ManagedDeleteTarget::Agent { id, .. } => {
                                this.uninstall_managed_agent(id, cx);
                            }
                            ManagedDeleteTarget::Provider { id, .. } => {
                                if let Ok(provider_profile_id) =
                                    vibex_core::ProviderProfileId::parse(id.clone())
                                {
                                    this.begin_simple_task(
                                        ManagementMutation::ProfileDelete(id),
                                        cx,
                                        async move {
                                            providers.delete_profile(
                                                vibex_core::ProviderProfileDeleteRequest {
                                                    provider_profile_id,
                                                },
                                            )?;
                                            Ok(management_locale_text_for(
                                                active_locale,
                                                "Provider configuration deleted",
                                                "供应商配置已删除",
                                                "供應商配置已刪除",
                                            )
                                            .into())
                                        },
                                    );
                                }
                            }
                            ManagedDeleteTarget::Mcp { id, .. } => {
                                if let Ok(mcp_server_id) =
                                    vibex_core::McpServerId::parse(id.clone())
                                {
                                    this.begin_simple_task(
                                        ManagementMutation::McpAction(format!("delete:{id}")),
                                        cx,
                                        async move {
                                            providers.delete_mcp_server(
                                                vibex_core::McpServerDeleteRequest {
                                                    mcp_server_id,
                                                },
                                            )?;
                                            Ok(management_locale_text_for(
                                                active_locale,
                                                "MCP server deleted",
                                                "MCP 服务已删除",
                                                "MCP 服務已刪除",
                                            )
                                            .into())
                                        },
                                    );
                                }
                            }
                            ManagedDeleteTarget::Skill { id, .. } => {
                                if let Ok(skill_id) = vibex_core::SkillId::parse(id.clone()) {
                                    this.begin_simple_task(
                                        ManagementMutation::SkillAction(format!("delete:{id}")),
                                        cx,
                                        async move {
                                            providers.delete_skill(
                                                vibex_core::SkillDeleteRequest { skill_id },
                                            )?;
                                            Ok(management_locale_text_for(
                                                active_locale,
                                                "Skill deleted",
                                                "技能已删除",
                                                "技能已刪除",
                                            )
                                            .into())
                                        },
                                    );
                                }
                            }
                            ManagedDeleteTarget::Prompt { id, .. } => {
                                if let Ok(prompt_id) = vibex_core::PromptId::parse(id.clone()) {
                                    this.begin_simple_task(
                                        ManagementMutation::PromptAction(format!("delete:{id}")),
                                        cx,
                                        async move {
                                            providers.delete_prompt(
                                                vibex_core::PromptDeleteRequest { prompt_id },
                                            )?;
                                            Ok(management_locale_text_for(
                                                active_locale,
                                                "Prompt deleted",
                                                "提示词已删除",
                                                "提示詞已刪除",
                                            )
                                            .into())
                                        },
                                    );
                                }
                            }
                            ManagedDeleteTarget::Hook { id, .. } => {
                                if let Ok(hook_id) = vibex_core::HookId::parse(id.clone()) {
                                    this.begin_simple_task(
                                        ManagementMutation::HookAction(format!("delete:{id}")),
                                        cx,
                                        async move {
                                            providers.delete_hook(
                                                vibex_core::HookDeleteRequest { hook_id },
                                            )?;
                                            Ok(management_locale_text_for(
                                                active_locale,
                                                "Hook deleted",
                                                "Hook 已删除",
                                                "Hook 已刪除",
                                            )
                                            .into())
                                        },
                                    );
                                }
                            }
                        }
                    });
                    true
                })
        });
    }

    fn toggle_agent(&mut self, agent_id: String, enabled: bool, cx: &mut Context<Self>) {
        let Ok(agent_id) = AgentId::parse(agent_id.clone()) else {
            self.error = Some(
                management_error_text("Invalid Agent id", "Agent 标识无效", "Agent 識別碼無效")
                    .into(),
            );
            cx.notify();
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AgentToggle(agent_id.as_str().to_string()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .update_agent_config(AgentUpdateConfigRequest {
                        agent_id: agent_id.clone(),
                        added: None,
                        enabled: Some(enabled),
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    })?;
                Ok(management_locale_text_for(
                    active_locale,
                    "Agent updated",
                    "Agent 已更新",
                    "Agent 已更新",
                )
                .to_string())
            },
        );
    }

    fn set_agent_added(&mut self, agent_id: String, added: bool, cx: &mut Context<Self>) {
        let managed = self
            .snapshot
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == agent_id)
            .is_some_and(|agent| agent.managed_install.managed);
        if managed && added {
            self.install_managed_agent(agent_id, false, cx);
            return;
        }
        if managed && !added {
            self.uninstall_managed_agent(agent_id, cx);
            return;
        }
        let Ok(parsed_agent_id) = AgentId::parse(agent_id.clone()) else {
            self.error = Some(
                management_error_text("Invalid Agent id", "Agent 标识无效", "Agent 識別碼無效")
                    .into(),
            );
            cx.notify();
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        if added {
            self.selected_agent_id = Some(agent_id.clone());
        }
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AgentToggle(format!(
                "{}:{agent_id}",
                if added { "add" } else { "remove" }
            )),
            cx,
            async move {
                let providers = runtime.management().providers().management();
                providers.update_agent_config(AgentUpdateConfigRequest {
                    agent_id: parsed_agent_id.clone(),
                    added: Some(added),
                    enabled: Some(false),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                })?;
                if !added {
                    runtime.agent().delete_auth_catalog(&parsed_agent_id)?;
                    return Ok(management_locale_text_for(
                        active_locale,
                        "Agent removed",
                        "Agent 已移除",
                        "Agent 已移除",
                    )
                    .to_string());
                }
                let refreshed =
                    providers.refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                        agent_id: parsed_agent_id.clone(),
                        cwd_scope: None,
                    })?;
                if refreshed.agent.installed {
                    providers.update_agent_config(AgentUpdateConfigRequest {
                        agent_id: parsed_agent_id.clone(),
                        added: None,
                        enabled: Some(true),
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    })?;
                    let message = management_locale_text_for(
                        active_locale,
                        "Agent added and enabled",
                        "Agent 已添加并启用",
                        "Agent 已新增並啟用",
                    )
                    .to_string();
                    let refresh = runtime
                        .agent()
                        .runtime_catalog()
                        .probe_agent(&parsed_agent_id)
                        .await;
                    Ok(management_append_runtime_option_probe(
                        message,
                        refresh,
                        active_locale,
                    ))
                } else {
                    Ok(management_locale_text_for(
                        active_locale,
                        "Agent added; install it and probe again to enable it",
                        "Agent 已添加；安装后再次检测即可启用",
                        "Agent 已新增；安裝後再次檢測即可啟用",
                    )
                    .to_string())
                }
            },
        );
    }

    fn install_managed_agent(&mut self, agent_id: String, upgrading: bool, cx: &mut Context<Self>) {
        let Ok(parsed_agent_id) = AgentId::parse(agent_id.clone()) else {
            self.error = Some(
                management_error_text("Invalid Agent id", "Agent 标识无效", "Agent 識別碼無效")
                    .into(),
            );
            cx.notify();
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.selected_agent_id = Some(agent_id.clone());
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::AgentInstall(agent_id), cx, async move {
            runtime
                .agent()
                .install_managed_agent(parsed_agent_id.clone())
                .await?;
            let providers = runtime.management().providers().management();
            providers.update_agent_config(AgentUpdateConfigRequest {
                agent_id: parsed_agent_id.clone(),
                added: if upgrading { None } else { Some(true) },
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })?;
            providers.refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                agent_id: parsed_agent_id.clone(),
                cwd_scope: None,
            })?;
            let runtime_probe = runtime
                .agent()
                .runtime_catalog()
                .probe_agent(&parsed_agent_id)
                .await;
            let auth_probe = runtime
                .agent()
                .refresh_auth_methods(parsed_agent_id, None)
                .await;
            let mut message = management_locale_text_for(
                active_locale,
                if upgrading {
                    "Agent upgraded and enabled"
                } else {
                    "Agent installed and enabled"
                },
                if upgrading {
                    "Agent 已升级并启用"
                } else {
                    "Agent 已安装并启用"
                },
                if upgrading {
                    "Agent 已升級並啟用"
                } else {
                    "Agent 已安裝並啟用"
                },
            )
            .to_string();
            message = management_append_runtime_option_probe(message, runtime_probe, active_locale);
            match auth_probe {
                Ok(catalog) => {
                    let count = catalog.methods.len();
                    message.push_str(&match active_locale {
                        ResolvedLocale::En => {
                            format!("; {count} authentication method(s) detected")
                        }
                        ResolvedLocale::ZhCn => format!("；已检测到 {count} 种认证方式"),
                        ResolvedLocale::ZhTw => format!("；已偵測到 {count} 種驗證方式"),
                    });
                }
                Err(error) => {
                    message.push_str(&match active_locale {
                        ResolvedLocale::En => {
                            format!("; authentication detection pending ({})", error.code)
                        }
                        ResolvedLocale::ZhCn => {
                            format!("；认证方式待重新检测（{}）", error.code)
                        }
                        ResolvedLocale::ZhTw => {
                            format!("；驗證方式待重新偵測（{}）", error.code)
                        }
                    });
                }
            }
            Ok(message)
        });
    }

    fn check_managed_agent_update(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let Ok(parsed_agent_id) = AgentId::parse(agent_id.clone()) else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AgentUpdateCheck(agent_id),
            cx,
            async move {
                let state = runtime
                    .agent()
                    .check_managed_agent_update(parsed_agent_id)
                    .await?;
                Ok(
                    if state.status == vibex_core::AgentManagedInstallStatus::UpdateAvailable {
                        management_locale_text_for(
                            active_locale,
                            "Agent update available",
                            "Agent 有可用更新",
                            "Agent 有可用更新",
                        )
                        .to_string()
                    } else {
                        management_locale_text_for(
                            active_locale,
                            "Agent is up to date",
                            "Agent 已是最新版本",
                            "Agent 已是最新版本",
                        )
                        .to_string()
                    },
                )
            },
        );
    }

    fn uninstall_managed_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let Ok(parsed_agent_id) = AgentId::parse(agent_id.clone()) else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AgentUninstall(agent_id),
            cx,
            async move {
                runtime
                    .agent()
                    .uninstall_managed_agent(parsed_agent_id)
                    .await?;
                Ok(management_locale_text_for(
                    active_locale,
                    "Agent uninstalled",
                    "Agent 已卸载",
                    "Agent 已解除安裝",
                )
                .to_string())
            },
        );
    }

    fn discover_local_agents(&mut self, cx: &mut Context<Self>) {
        let candidates = self
            .snapshot
            .agents
            .iter()
            .filter(|agent| !agent.added)
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.notice = Some(
                management_locale_text(
                    "No additional local Agent CLI candidates are available",
                    "没有更多可探测的本地 Agent CLI",
                    "沒有更多可探測的本機 Agent CLI",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::AgentDiscovery, cx, async move {
            let providers = runtime.management().providers().management();
            let runtime_catalog = runtime.agent().runtime_catalog();
            let mut discovered_count = 0_usize;
            let mut option_refresh = RuntimeOptionProbeResult::default();
            let mut option_refresh_errors = 0_usize;
            for agent_id in candidates {
                providers.update_agent_config(AgentUpdateConfigRequest {
                    agent_id: agent_id.clone(),
                    added: Some(true),
                    enabled: Some(false),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                })?;
                let refreshed =
                    providers.refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                        agent_id: agent_id.clone(),
                        cwd_scope: None,
                    })?;
                if refreshed.agent.installed {
                    providers.update_agent_config(AgentUpdateConfigRequest {
                        agent_id: agent_id.clone(),
                        added: None,
                        enabled: Some(true),
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    })?;
                    discovered_count = discovered_count.saturating_add(1);
                    match runtime_catalog.probe_agent(&agent_id).await {
                        Ok(result) => {
                            option_refresh
                                .probed_agent_ids
                                .extend(result.probed_agent_ids);
                            option_refresh
                                .failed_agent_ids
                                .extend(result.failed_agent_ids);
                            option_refresh
                                .cached_agent_ids
                                .extend(result.cached_agent_ids);
                        }
                        Err(_) => {
                            option_refresh_errors = option_refresh_errors.saturating_add(1);
                        }
                    }
                } else {
                    providers.update_agent_config(AgentUpdateConfigRequest {
                        agent_id: agent_id.clone(),
                        added: Some(false),
                        enabled: Some(false),
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    })?;
                }
            }
            let message = match (active_locale, discovered_count) {
                (ResolvedLocale::En, 0) => "No installed local Agent CLI was found".into(),
                (ResolvedLocale::ZhCn, 0) => "未发现已安装的本地 Agent CLI".into(),
                (ResolvedLocale::ZhTw, 0) => "未發現已安裝的本機 Agent CLI".into(),
                (ResolvedLocale::En, count) => {
                    format!("Found and enabled {count} local Agent CLI(s)")
                }
                (ResolvedLocale::ZhCn, count) => {
                    format!("已发现并启用 {count} 个本地 Agent CLI")
                }
                (ResolvedLocale::ZhTw, count) => {
                    format!("已發現並啟用 {count} 個本機 Agent CLI")
                }
            };
            let message =
                management_append_runtime_option_probe(message, Ok(option_refresh), active_locale);
            if option_refresh_errors == 0 {
                Ok(message)
            } else {
                Ok(format!(
                    "{message}; {}",
                    match active_locale {
                        ResolvedLocale::En => format!(
                            "runtime option probing failed for {option_refresh_errors} Agent(s)"
                        ),
                        ResolvedLocale::ZhCn =>
                            format!("{option_refresh_errors} 个 Agent 的运行选项探测失败"),
                        ResolvedLocale::ZhTw =>
                            format!("{option_refresh_errors} 個 Agent 的執行選項探測失敗"),
                    }
                ))
            }
        });
    }

    fn probe_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let install_url = self
            .snapshot
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == agent_id)
            .and_then(agent_install_url)
            .map(str::to_string);
        let (Ok(agent_id), Some(runtime)) =
            (AgentId::parse(agent_id.clone()), self.runtime.clone())
        else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AgentToggle(format!("probe:{agent_id}")),
            cx,
            async move {
                let providers = runtime.management().providers().management();
                let response =
                    providers.refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                        agent_id,
                        cwd_scope: None,
                    })?;
                if response.agent.installed && !response.agent.enabled {
                    providers.update_agent_config(AgentUpdateConfigRequest {
                        agent_id: response.agent.id.clone(),
                        added: None,
                        enabled: Some(true),
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    })?;
                }
                if response.agent.installed {
                    let message = match active_locale {
                        ResolvedLocale::En => {
                            format!("{} was detected and enabled", response.agent.label)
                        }
                        ResolvedLocale::ZhCn => {
                            format!("已检测到并启用 {}", response.agent.label)
                        }
                        ResolvedLocale::ZhTw => {
                            format!("已檢測到並啟用 {}", response.agent.label)
                        }
                    };
                    let refresh = runtime
                        .agent()
                        .runtime_catalog()
                        .probe_agent(&response.agent.id)
                        .await;
                    return Ok(management_append_runtime_option_probe(
                        message,
                        refresh,
                        active_locale,
                    ));
                }
                if let Some(install_url) = install_url {
                    let validated = validate_external_open_url(&install_url)?;
                    crate::platform::open_external_url(&validated.url)?;
                    return Ok(match active_locale {
                        ResolvedLocale::En => format!(
                            "{} is not installed; the installation page was opened",
                            response.agent.label
                        ),
                        ResolvedLocale::ZhCn => {
                            format!("尚未安装 {}，已打开安装页面", response.agent.label)
                        }
                        ResolvedLocale::ZhTw => {
                            format!("尚未安裝 {}，已開啟安裝頁面", response.agent.label)
                        }
                    });
                }
                Ok(match active_locale {
                    ResolvedLocale::En => {
                        format!("{} was not found on this device", response.agent.label)
                    }
                    ResolvedLocale::ZhCn => {
                        format!("未在此设备上检测到 {}", response.agent.label)
                    }
                    ResolvedLocale::ZhTw => {
                        format!("未在此裝置上檢測到 {}", response.agent.label)
                    }
                })
            },
        );
    }

    fn set_default_provider_profile(
        &mut self,
        profile_id: String,
        agent_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Ok(provider_profile_id), Ok(agent_id), Some(runtime)) = (
            vibex_core::ProviderProfileId::parse(profile_id.clone()),
            AgentId::parse(agent_id),
            self.runtime.clone(),
        ) else {
            return;
        };
        let active_locale = locale::current_locale();
        let scope = management_provider_default_scope(self.pairing_workspace_id.clone());
        let scope_kind = scope.kind;
        self.begin_simple_task(
            ManagementMutation::ProviderPreview(format!("default:{profile_id}")),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .set_agent_model_provider_default(
                        vibex_core::AgentModelProviderSetDefaultRequest {
                            scope,
                            agent_id,
                            provider_profile_id,
                        },
                    )
                    .map(|_| management_default_updated_message(active_locale, scope_kind).into())
            },
        );
    }

    fn reorder_provider_profiles(
        &mut self,
        moving_id: &str,
        target_id: &str,
        after: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(agent_id) = self.selected_agent_id.clone() else {
            return;
        };
        let mut current_ids = self
            .snapshot
            .profiles
            .iter()
            .filter(|profile| profile.agent_id == agent_id)
            .map(|profile| profile.id.as_str().to_string())
            .collect::<Vec<_>>();
        current_ids.sort_by_key(|profile_id| {
            (
                self.provider_display_order
                    .get(profile_id.as_str())
                    .is_none(),
                self.provider_display_order
                    .get(profile_id.as_str())
                    .copied()
                    .unwrap_or(i64::MAX),
            )
        });
        let ordered_ids = reordered_provider_profile_ids(&current_ids, moving_id, target_id, after);
        if ordered_ids == current_ids {
            return;
        }
        let Ok(parsed_agent_id) = AgentId::parse(agent_id.clone()) else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        let entries = ordered_ids
            .into_iter()
            .map(
                |provider_profile_id| vibex_core::AgentModelProviderDisplayOrderSetEntry {
                    provider_profile_id: vibex_core::ProviderProfileId::parse(provider_profile_id)
                        .expect("provider profile ids originate from the loaded snapshot"),
                },
            )
            .collect::<Vec<_>>();
        self.begin_simple_task(
            ManagementMutation::ProviderDisplayOrder(agent_id),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .set_agent_model_provider_display_order(
                        vibex_core::AgentModelProviderDisplayOrderSetRequest {
                            agent_id: parsed_agent_id,
                            entries,
                        },
                    )?;
                Ok(management_locale_text_for(
                    active_locale,
                    "Provider order updated",
                    "模型供应商顺序已更新",
                    "模型供應商順序已更新",
                )
                .to_string())
            },
        );
    }

    fn begin_simple_task<F>(
        &mut self,
        mutation: ManagementMutation,
        cx: &mut Context<Self>,
        work: F,
    ) where
        F: std::future::Future<Output = VibexResult<String>> + Send + 'static,
    {
        let concurrent_agent_id = mutation.concurrent_agent_id().map(str::to_string);
        if let Some(agent_id) = concurrent_agent_id.as_deref() {
            if self.agent_mutations.contains_key(agent_id) {
                self.notice = Some(
                    management_locale_text(
                        "Another action for this Agent is still pending",
                        "此 Agent 的另一项操作仍在处理中",
                        "此 Agent 的另一項操作仍在處理中",
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
            self.agent_mutations
                .insert(agent_id.to_string(), mutation.clone());
        } else {
            if self.mutation.is_some() {
                self.notice = Some(
                    management_locale_text(
                        "Another management action is still pending",
                        "另一项配置操作仍在处理中",
                        "另一項配置操作仍在處理中",
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
            self.mutation = Some(mutation.clone());
        }
        let completed_mutation = mutation;
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, work);
        let task = cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if let Some(agent_id) = completed_mutation.concurrent_agent_id() {
                    this.agent_mutations.remove(agent_id);
                } else {
                    this.mutation = None;
                }
                let agent_registry_changed = matches!(
                    &completed_mutation,
                    ManagementMutation::AgentToggle(_)
                        | ManagementMutation::AgentInstall(_)
                        | ManagementMutation::AgentUpdateCheck(_)
                        | ManagementMutation::AgentUninstall(_)
                        | ManagementMutation::AgentDiscovery
                );
                match outcome {
                    Ok(Ok(message)) => {
                        match &completed_mutation {
                            ManagementMutation::PromptAction(action)
                            | ManagementMutation::HookAction(action)
                                if action.starts_with("create") =>
                            {
                                this.navigation
                                    .mark_dirty(ManagementSection::Advanced, false);
                            }
                            ManagementMutation::AcpConfig(_) => {
                                this.navigation
                                    .mark_dirty(ManagementSection::Advanced, false);
                                this.selected_acp_profile_id = None;
                                this.acp_config_draft = None;
                            }
                            ManagementMutation::ProviderPreview(action)
                                if matches!(
                                    action.as_str(),
                                    "native-export-apply" | "native-export-rollback"
                                ) =>
                            {
                                this.native_export_preview = None;
                            }
                            ManagementMutation::ScheduledUpdate(_) => {
                                this.navigation
                                    .mark_dirty(ManagementSection::Scheduled, false);
                            }
                            ManagementMutation::BackupInspect
                            | ManagementMutation::BackupRestore
                            | ManagementMutation::BackupCreate => {
                                this.navigation
                                    .mark_dirty(ManagementSection::Recovery, false);
                                this.recovery.phase = "succeeded".into();
                                this.recovery.progress_percent = 100;
                                this.recovery.error_code = None;
                            }
                            _ => {}
                        }
                        this.notice = Some(message);
                        this.refresh(cx);
                        if agent_registry_changed {
                            cx.emit(ManagementEvent::AgentRegistryChanged);
                        }
                    }
                    Ok(Err(error)) => {
                        if matches!(
                            &completed_mutation,
                            ManagementMutation::BackupInspect
                                | ManagementMutation::BackupRestore
                                | ManagementMutation::BackupCreate
                        ) {
                            this.recovery.phase = "error".into();
                            this.recovery.error_code = Some(error.code.clone());
                        }
                        let message = format!("{}: {}", error.code, error.message);
                        if agent_registry_changed {
                            this.refresh(cx);
                            cx.emit(ManagementEvent::AgentRegistryChanged);
                        }
                        this.error = Some(message);
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Config center action failed",
                                "配置中心操作失败",
                                "配置中心操作失敗",
                            )
                        ));
                        if agent_registry_changed {
                            this.refresh(cx);
                            cx.emit(ManagementEvent::AgentRegistryChanged);
                        }
                        cx.notify();
                    }
                }
            });
        });
        if let Some(agent_id) = concurrent_agent_id {
            self.agent_mutation_tasks
                .retain(|_, existing| !existing.is_ready());
            self.agent_mutation_tasks.insert(agent_id, task);
        } else {
            self.mutation_task = Some(task);
        }
    }

    fn present_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent_auth_error = self.agent_auth_error.take();
        let notice = self.notice.take();
        let notification = self
            .error
            .take()
            .map(|error| Notification::error(locale::localize_error_message(&error)))
            .or_else(|| {
                agent_auth_error
                    .map(|error| Notification::error(locale::localize_error_message(&error)))
            })
            .or_else(|| {
                notice.map(|notice| Notification::info(locale::localize_ui_message(&notice)))
            });
        let Some(notification) = notification else {
            return;
        };

        window.defer(cx, move |window, cx| {
            Theme::global_mut(cx).notification.placement = Anchor::TopCenter;
            window.push_notification(
                notification
                    .id::<ManagementCenterFeedbackNotification>()
                    .autohide(true)
                    .on_click(|_, _, _| {}),
                cx,
            );
        });
    }

    fn open_mcp_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_import_dialog(ManagementImportKind::Mcp, window, cx);
    }

    fn open_import_dialog(
        &mut self,
        kind: ManagementImportKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mutation.is_some() {
            return;
        }
        match kind {
            ManagementImportKind::Mcp => {
                self.mcp_import_open = true;
                self.mcp_discovery = None;
            }
            ManagementImportKind::Skill => {
                self.skill_import_open = true;
                self.skill_discovery = None;
            }
        }
        let center = cx.entity();
        let dialog_center = center.clone();
        let dialog_content = cx.new(|cx| ManagementImportDialog::new(center.clone(), kind, cx));
        let dialog_width = (f32::from(window.viewport_size().width) - 32.0).clamp(320.0, 672.0);
        let dialog_height = (f32::from(window.viewport_size().height) - 32.0).clamp(280.0, 608.0);
        let title = match kind {
            ManagementImportKind::Mcp => {
                management_locale_text("Native MCP import", "原生 MCP 导入", "原生 MCP 匯入")
            }
            ManagementImportKind::Skill => {
                management_locale_text("Native Skill import", "原生技能导入", "原生技能匯入")
            }
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let dialog_center = dialog_center.clone();
            dialog
                .title(title)
                .w(px(dialog_width))
                .max_w(px(dialog_width))
                .h(px(dialog_height))
                .child(dialog_content.clone())
                .on_close(move |_, _, cx| {
                    dialog_center.update(cx, |center, cx| {
                        match kind {
                            ManagementImportKind::Mcp => {
                                center.mcp_import_open = false;
                                center.mcp_discovery = None;
                            }
                            ManagementImportKind::Skill => {
                                center.skill_import_open = false;
                                center.skill_discovery = None;
                            }
                        }
                        cx.notify();
                    });
                })
        });
        match kind {
            ManagementImportKind::Mcp => self.discover_mcp_servers(cx),
            ManagementImportKind::Skill => self.discover_skills(cx),
        }
    }

    fn discover_mcp_servers(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.mutation = Some(ManagementMutation::McpAction("discover".into()));
        self.error = None;
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .discover_mcp_sources(vibex_core::McpServerDiscoverRequest {
                    source_agent_id: None,
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(discovery)) => {
                        this.notice = Some(match locale::current_locale() {
                            ResolvedLocale::En => format!(
                                "Discovered {} MCP candidate(s)",
                                discovery.discoveries.len()
                            ),
                            ResolvedLocale::ZhCn => {
                                format!("发现 {} 个 MCP 候选项", discovery.discoveries.len())
                            }
                            ResolvedLocale::ZhTw => {
                                format!("發現 {} 個 MCP 候選項", discovery.discoveries.len())
                            }
                        });
                        this.mcp_discovery = Some(discovery);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "MCP discovery failed",
                                "MCP 探测失败",
                                "MCP 探測失敗",
                            )
                        ));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn import_mcp_discovery(&mut self, discovery_id: String, cx: &mut Context<Self>) {
        let Some(item) = self
            .mcp_discovery
            .as_ref()
            .and_then(|response| {
                response
                    .discoveries
                    .iter()
                    .find(|item| item.discovery_id == discovery_id)
            })
            .cloned()
        else {
            return;
        };
        let Some(selection) = mcp_import_selection_from_discovery(item) else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let mutation = ManagementMutation::McpAction(format!("import:{discovery_id}"));
        self.mutation = Some(mutation);
        self.error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .import_mcp_servers(vibex_core::McpServerImportRequest {
                    selections: vec![selection],
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(result)) => {
                        if let Some(imported) = result.imported.first() {
                            this.selected_mcp_id = Some(imported.id.as_str().to_string());
                        }
                        this.mcp_import_open = false;
                        this.mcp_discovery = None;
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => format!(
                                "Imported {} MCP server(s), updated {}",
                                result.created_count, result.updated_count
                            ),
                            ResolvedLocale::ZhCn => format!(
                                "已导入 {} 个 MCP 服务，更新 {} 个",
                                result.created_count, result.updated_count
                            ),
                            ResolvedLocale::ZhTw => format!(
                                "已匯入 {} 個 MCP 服務，更新 {} 個",
                                result.created_count, result.updated_count
                            ),
                        });
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "MCP import failed",
                                "MCP 导入失败",
                                "MCP 匯入失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn validate_mcp_server(&mut self, id: String, cx: &mut Context<Self>) {
        let (Ok(mcp_server_id), Some(runtime)) = (
            vibex_core::McpServerId::parse(id.clone()),
            self.runtime.clone(),
        ) else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        self.mutation = Some(ManagementMutation::McpAction(format!("validate:{id}")));
        self.error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let validated_id = id;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .validate_mcp_server(vibex_core::McpServerValidateRequest {
                    mcp_server_id: Some(mcp_server_id),
                    candidate: None,
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(result)) => {
                        let failed = result.status == vibex_core::McpServerValidationStatus::Fail;
                        let message = format!(
                            "{}: {}",
                            management_mcp_validation_status_label(result.status, active_locale),
                            result.message
                        );
                        this.mcp_validation = Some((validated_id, message.clone(), failed));
                        this.notice = Some(message);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("MCP validation task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn set_mcp_agent_matrix(
        &mut self,
        id: String,
        agent_id: AgentId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(matrix) = self
            .snapshot
            .mcp_servers
            .iter()
            .find(|server| server.id.as_str() == id)
            .map(|server| {
                updated_mcp_agent_matrix(server.agent_matrix.clone(), agent_id.clone(), enabled)
            })
        else {
            self.error = Some(
                management_error_text(
                    "MCP server was not found",
                    "未找到 MCP 服务",
                    "找不到 MCP 服務",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let (Ok(mcp_server_id), Some(runtime)) = (
            vibex_core::McpServerId::parse(id.clone()),
            self.runtime.clone(),
        ) else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::McpAction(format!("agent-matrix:{id}")),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .set_mcp_server_agent_matrix(vibex_core::McpServerSetAgentMatrixRequest {
                        mcp_server_id,
                        agent_matrix: matrix,
                    })
                    .map(|_| {
                        format!(
                            "{}: {}",
                            management_locale_text_for(
                                active_locale,
                                "Agent enablement",
                                "Agent 启用范围",
                                "Agent 啟用範圍",
                            ),
                            management_locale_text_for(
                                active_locale,
                                if enabled { "Enabled" } else { "Disabled" },
                                if enabled { "已启用" } else { "已停用" },
                                if enabled { "已啟用" } else { "已停用" },
                            )
                        )
                    })
            },
        );
    }

    fn open_skill_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_import_dialog(ManagementImportKind::Skill, window, cx);
    }

    fn discover_skills(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let workspace_id = self.pairing_workspace_id.clone().or_else(|| {
            self.snapshot
                .graphs
                .iter()
                .find_map(|graph| graph.workspace_id.clone())
                .or_else(|| {
                    self.snapshot
                        .scheduled
                        .iter()
                        .find_map(|task| task.workspace_id.clone())
                })
        });
        self.mutation = Some(ManagementMutation::SkillAction("discover".into()));
        self.error = None;
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .discover_skill_sources(vibex_core::SkillDiscoverRequest {
                    source_agent_id: None,
                    workspace_id,
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(discovery)) => {
                        this.notice = Some(match locale::current_locale() {
                            ResolvedLocale::En => format!(
                                "Discovered {} Skill candidate(s)",
                                discovery.discoveries.len()
                            ),
                            ResolvedLocale::ZhCn => {
                                format!("发现 {} 个技能候选项", discovery.discoveries.len())
                            }
                            ResolvedLocale::ZhTw => {
                                format!("發現 {} 個技能候選項", discovery.discoveries.len())
                            }
                        });
                        this.skill_discovery = Some(discovery);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Skill discovery failed",
                                "技能探测失败",
                                "技能探測失敗",
                            )
                        ));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn import_skill_discovery(&mut self, discovery_id: String, cx: &mut Context<Self>) {
        let Some(item) = self
            .skill_discovery
            .as_ref()
            .and_then(|response| {
                response
                    .discoveries
                    .iter()
                    .find(|item| item.discovery_id == discovery_id)
            })
            .cloned()
        else {
            return;
        };
        let Some(selection) = skill_import_selection_from_discovery(item) else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.mutation = Some(ManagementMutation::SkillAction(format!(
            "import:{discovery_id}"
        )));
        self.error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime.management().providers().management().import_skills(
                vibex_core::SkillImportRequest {
                    selections: vec![selection],
                },
            )
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(result)) => {
                        if let Some(imported) = result.imported.first() {
                            this.selected_skill_id = Some(imported.id.as_str().to_string());
                        }
                        this.skill_import_open = false;
                        this.skill_discovery = None;
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => format!(
                                "Imported {} Skill(s), updated {}",
                                result.created_count, result.updated_count
                            ),
                            ResolvedLocale::ZhCn => format!(
                                "已导入 {} 个技能，更新 {} 个",
                                result.created_count, result.updated_count
                            ),
                            ResolvedLocale::ZhTw => format!(
                                "已匯入 {} 個技能，更新 {} 個",
                                result.created_count, result.updated_count
                            ),
                        });
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Skill import failed",
                                "技能导入失败",
                                "技能匯入失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn validate_skill(&mut self, id: String, cx: &mut Context<Self>) {
        let (Ok(skill_id), Some(runtime)) =
            (vibex_core::SkillId::parse(id.clone()), self.runtime.clone())
        else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        self.mutation = Some(ManagementMutation::SkillAction(format!("validate:{id}")));
        self.error = None;
        let active_locale = locale::current_locale();
        let entity = cx.weak_entity();
        let validated_id = id;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .validate_skill(vibex_core::SkillValidateRequest {
                    skill_id: Some(skill_id),
                    candidate: None,
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(result)) => {
                        let failed = result.status == vibex_core::SkillValidationStatus::Fail;
                        let message = format!(
                            "{}: {}",
                            management_skill_validation_status_label(result.status, active_locale),
                            result.message
                        );
                        this.skill_validation = Some((validated_id, message.clone(), failed));
                        this.notice = Some(message);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.error = Some(format!("Skill validation task failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn set_skill_agent_matrix(
        &mut self,
        id: String,
        agent_id: AgentId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(matrix) = self
            .snapshot
            .skills
            .iter()
            .find(|skill| skill.id.as_str() == id)
            .map(|skill| {
                updated_skill_agent_matrix(skill.agent_matrix.clone(), agent_id.clone(), enabled)
            })
        else {
            self.error = Some(
                management_error_text("Skill was not found", "未找到技能", "找不到技能").into(),
            );
            cx.notify();
            return;
        };
        let (Ok(skill_id), Some(runtime)) =
            (vibex_core::SkillId::parse(id.clone()), self.runtime.clone())
        else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::SkillAction(format!("agent-matrix:{id}")),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .set_skill_agent_matrix(vibex_core::SkillSetAgentMatrixRequest {
                        skill_id,
                        agent_matrix: matrix,
                    })
                    .map(|_| {
                        format!(
                            "{}: {}",
                            management_locale_text_for(
                                active_locale,
                                "Agent enablement",
                                "Agent 启用范围",
                                "Agent 啟用範圍",
                            ),
                            management_locale_text_for(
                                active_locale,
                                if enabled { "Enabled" } else { "Disabled" },
                                if enabled { "已启用" } else { "已停用" },
                                if enabled { "已啟用" } else { "已停用" },
                            )
                        )
                    })
            },
        );
    }

    fn create_prompt(&mut self, cx: &mut Context<Self>) {
        let active_locale = locale::current_locale();
        let display_name = self.prompt_name.read(cx).value().trim().to_string();
        let display_name = if display_name.is_empty() {
            management_locale_text_for(
                active_locale,
                "Reusable prompt",
                "可复用提示词",
                "可重用提示詞",
            )
            .to_string()
        } else {
            display_name
        };
        let body = self.prompt_body.read(cx).value().trim().to_string();
        let body = if body.is_empty() {
            management_locale_text_for(
                active_locale,
                "Review this workspace.",
                "检查此工作区。",
                "檢查此工作區。",
            )
            .to_string()
        } else {
            body
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.begin_simple_task(
            ManagementMutation::PromptAction("create".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .create_prompt(vibex_core::PromptCreateRequest {
                        display_name,
                        kind: vibex_core::PromptKind::ReusablePrompt,
                        status: vibex_core::PromptStatus::Enabled,
                        scope_kind: vibex_core::PromptScopeKind::User,
                        project_id: None,
                        workspace_id: None,
                        body,
                        description: None,
                        tags: Vec::new(),
                    })
                    .map(|prompt| match active_locale {
                        ResolvedLocale::En => format!("Created Prompt {}", prompt.display_name),
                        ResolvedLocale::ZhCn => format!("已创建提示词 {}", prompt.display_name),
                        ResolvedLocale::ZhTw => format!("已建立提示詞 {}", prompt.display_name),
                    })
            },
        );
    }

    fn create_hook(&mut self, cx: &mut Context<Self>) {
        let active_locale = locale::current_locale();
        let display_name = self.hook_name.read(cx).value().trim().to_string();
        let display_name = if display_name.is_empty() {
            management_locale_text_for(active_locale, "Vibex hook", "Vibex Hook", "Vibex Hook")
                .to_string()
        } else {
            display_name
        };
        let command_preview = self.hook_command.read(cx).value().trim().to_string();
        let provider_kind = self
            .selected_management_provider_profile()
            .map(|profile| profile.kind)
            .unwrap_or(ProviderKind::Acp);
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.begin_simple_task(
            ManagementMutation::HookAction("create".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .create_hook(vibex_core::HookCreateRequest {
                        display_name,
                        provider_kind,
                        event_kind: vibex_core::HookEventKind::PermissionRequest,
                        status: vibex_core::HookStatus::Draft,
                        command_preview: (!command_preview.is_empty()).then_some(command_preview),
                        managed_marker: None,
                        description: None,
                    })
                    .map(|hook| match active_locale {
                        ResolvedLocale::En => format!("Created Hook {}", hook.display_name),
                        ResolvedLocale::ZhCn => format!("已创建 Hook {}", hook.display_name),
                        ResolvedLocale::ZhTw => format!("已建立 Hook {}", hook.display_name),
                    })
            },
        );
    }

    fn select_graph(&mut self, graph_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(graph) = self
            .snapshot
            .graphs
            .iter()
            .find(|graph| graph.id.as_str() == graph_id)
        {
            self.automation_title.update(cx, |input, cx| {
                input.set_value(graph.title.clone(), window, cx)
            });
            self.automation_description.update(cx, |input, cx| {
                input.set_value(graph.description.clone().unwrap_or_default(), window, cx)
            });
            self.graph_draft = AutomationGraphDraft::from_graph(graph);
            self.navigation
                .mark_dirty(ManagementSection::Automation, false);
            cx.notify();
        }
    }

    fn request_graph_selection(
        &mut self,
        graph_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.graph_draft.graph_id.as_deref() == Some(graph_id.as_str()) {
            return;
        }
        if !self.graph_draft.dirty {
            self.select_graph(graph_id, window, cx);
            return;
        }
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let graph_id = graph_id.clone();
            dialog
                .title(management_locale_text_for(
                    active_locale,
                    "Discard unsaved graph changes?",
                    "放弃未保存的自动化图修改？",
                    "放棄未儲存的自動化圖修改？",
                ))
                .child(management_locale_text_for(
                    active_locale,
                    "The current graph draft will be replaced by the selected authoritative revision.",
                    "当前自动化图草稿将被所选的权威版本替换。",
                    "目前自動化圖草稿將被所選的權威版本取代。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(
                            gpui_component::dialog::DialogClose::new().child(
                                Button::new("cancel-graph-switch")
                                    .outline()
                                    .label(management_locale_text_for(
                                        active_locale,
                                        "Keep editing",
                                        "继续编辑",
                                        "繼續編輯",
                                    )),
                            ),
                        )
                        .child(
                            gpui_component::dialog::DialogAction::new().child(
                                Button::new("confirm-graph-switch")
                                    .danger()
                                    .label(management_locale_text_for(
                                        active_locale,
                                        "Discard and switch",
                                        "放弃并切换",
                                        "放棄並切換",
                                    )),
                            ),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.select_graph(graph_id.clone(), window, cx)
                    });
                    true
                })
        });
    }

    fn select_graph_node(&mut self, node_id: String, cx: &mut Context<Self>) {
        if !self.graph_draft.selected_node_ids.remove(&node_id) {
            self.graph_draft.selected_node_ids.insert(node_id);
        }
        cx.notify();
    }

    fn move_selected_graph_node(&mut self, dx: i32, dy: i32, cx: &mut Context<Self>) {
        let selected = self
            .graph_draft
            .selected_node_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in selected {
            if let Some(node) = self.graph_draft.nodes.iter().find(|node| node.id == id) {
                let _ = self.graph_draft.move_node(
                    &id,
                    vibex_desktop_model::GraphPosition {
                        x: node.position.x.saturating_add(dx),
                        y: node.position.y.saturating_add(dy),
                    },
                );
            }
        }
        cx.notify();
    }

    fn delete_selected_graph_nodes(&mut self, cx: &mut Context<Self>) {
        if self.graph_draft.delete_selection() {
            self.navigation
                .mark_dirty(ManagementSection::Automation, true);
        }
        cx.notify();
    }

    fn add_graph_node(&mut self, cx: &mut Context<Self>) {
        let id = vibex_core::AutomationNodeId::new();
        if self.graph_draft.add_node(
            id,
            vibex_core::AutomationNodeKind::AgentPrompt,
            "Agent prompt",
            vibex_core::AutomationNodeConfig::AgentPrompt(
                vibex_core::AutomationAgentPromptConfig {
                    prompt_template: "Review the current workspace".into(),
                    provider_kind: Some(ProviderKind::Acp),
                    provider_profile_id: None,
                    safety: None,
                    workspace_root: None,
                    workspace_mode: Some(WorkspaceMode::CurrentCheckout),
                },
            ),
        ) {
            self.navigation
                .mark_dirty(ManagementSection::Automation, true);
        }
        cx.notify();
    }

    fn connect_selected_graph_nodes(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .graph_draft
            .selected_node_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if selected.len() == 2 {
            let result = self.graph_draft.connect(
                &selected[0],
                &selected[1],
                vibex_core::AutomationEdgeCondition {
                    kind: vibex_core::AutomationEdgeConditionKind::OnSuccess,
                    expression: None,
                },
            );
            if let Err(issue) = result {
                self.error = Some(issue.message);
            } else {
                self.navigation
                    .mark_dirty(ManagementSection::Automation, true);
            }
        } else {
            self.error = Some(
                management_error_text(
                    "Select exactly two nodes to create an edge",
                    "请选择两个节点以创建连线",
                    "請選擇兩個節點以建立連線",
                )
                .into(),
            );
        }
        cx.notify();
    }

    fn save_graph_definition(&mut self, cx: &mut Context<Self>) {
        if self.mutation.is_some() {
            self.notice = Some(
                management_locale_text(
                    "Another automation action is still pending",
                    "另一项自动化操作仍在处理中",
                    "另一項自動化操作仍在處理中",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let request = match self.graph_draft.to_definition_request() {
            Ok(request) => request,
            Err(issues) => {
                self.error = Some(
                    issues
                        .into_iter()
                        .map(|issue| issue.message)
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                cx.notify();
                return;
            }
        };
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some(
                management_error_text(
                    "Management runtime is not connected",
                    "配置中心运行时未连接",
                    "配置中心執行階段未連線",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let active_locale = locale::current_locale();
        self.mutation = Some(ManagementMutation::AutomationSave);
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .automation()
                .replace_definition(request)
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(graph)) => {
                        this.graph_draft = AutomationGraphDraft::from_graph(&graph);
                        this.navigation
                            .mark_dirty(ManagementSection::Automation, false);
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => {
                                format!("Saved graph at revision {}", graph.version)
                            }
                            ResolvedLocale::ZhCn => {
                                format!("自动化图已保存为版本 {}", graph.version)
                            }
                            ResolvedLocale::ZhTw => {
                                format!("自動化圖已儲存為版本 {}", graph.version)
                            }
                        });
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        if error.code == "automation_graph_version_conflict" {
                            // Keep the user's draft and surface a deliberate reload choice.
                            this.graph_draft.preserve_after_conflict(None);
                            this.error = Some(management_error_text(
                                "Graph changed elsewhere. Draft preserved; reload before retrying.",
                                "自动化图已在其他位置发生更改。草稿已保留，请重新加载后重试。",
                                "自動化圖已在其他位置發生變更。草稿已保留，請重新載入後重試。",
                            ).into());
                        } else {
                            this.error = Some(format!("{}: {}", error.code, error.message));
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Graph save failed",
                                "自动化图保存失败",
                                "自動化圖儲存失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn create_graph_from_draft(&mut self, cx: &mut Context<Self>) {
        if self.mutation.is_some() {
            self.notice = Some(
                management_locale_text(
                    "Another automation action is still pending",
                    "另一项自动化操作仍在处理中",
                    "另一項自動化操作仍在處理中",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        if self.graph_draft.title.trim().is_empty() || self.graph_draft.nodes.is_empty() {
            self.error = Some(
                management_error_text(
                    "A graph title and at least one node are required",
                    "自动化图标题不能为空，且至少需要一个节点",
                    "自動化圖標題不能為空，且至少需要一個節點",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let nodes = self
            .graph_draft
            .nodes
            .iter()
            .map(|node| vibex_core::AutomationNodeCreateRequest {
                id: vibex_core::AutomationNodeId::parse(node.id.clone()).ok(),
                kind: node.kind,
                title: node.title.clone(),
                config: node.config.clone(),
                position: Some(vibex_core::AutomationNodePosition {
                    x: node.position.x,
                    y: node.position.y,
                }),
            })
            .collect();
        let edges = self
            .graph_draft
            .edges
            .iter()
            .filter_map(|edge| {
                Some(vibex_core::AutomationEdgeCreateRequest {
                    source_node_id: vibex_core::AutomationNodeId::parse(
                        edge.source_node_id.clone(),
                    )
                    .ok()?,
                    target_node_id: vibex_core::AutomationNodeId::parse(
                        edge.target_node_id.clone(),
                    )
                    .ok()?,
                    condition: edge.condition.clone(),
                })
            })
            .collect();
        let Some((workspace_root, workspace_mode)) = self.current_workspace_context() else {
            self.error = Some(
                management_error_text(
                    "Open a workspace session before creating an automation graph",
                    "请先打开一个工作区会话，再创建自动化图",
                    "請先開啟一個工作區工作階段，再建立自動化圖",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let request = AutomationGraphCreateRequest {
            title: self.graph_draft.title.trim().to_string(),
            description: (!self.graph_draft.description.trim().is_empty())
                .then(|| self.graph_draft.description.trim().to_string()),
            project_id: None,
            workspace_id: None,
            workspace_root,
            workspace_mode,
            provider_kind: Some(ProviderKind::Acp),
            provider_profile_id: None,
            trigger: vibex_core::AutomationGraphTrigger::Manual,
            nodes,
            edges,
        };
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some(
                management_error_text(
                    "Management runtime is not connected",
                    "配置中心运行时未连接",
                    "配置中心執行階段未連線",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let active_locale = locale::current_locale();
        self.mutation = Some(ManagementMutation::AutomationCreate);
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime.management().automation().create(request)
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(graph)) => {
                        this.graph_draft = AutomationGraphDraft::from_graph(&graph);
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => format!("Created graph {}", graph.title),
                            ResolvedLocale::ZhCn => format!("已创建自动化图 {}", graph.title),
                            ResolvedLocale::ZhTw => format!("已建立自動化圖 {}", graph.title),
                        });
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Graph creation failed",
                                "自动化图创建失败",
                                "自動化圖建立失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn resume_automation_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        let (Ok(run_id), Some(runtime)) =
            (AutomationRunId::parse(run_id.clone()), self.runtime.clone())
        else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AutomationResumeRun(run_id.as_str().to_string()),
            cx,
            async move {
                runtime
                    .management()
                    .automation()
                    .resume_run(AutomationRunResumeRequest {
                        run_id,
                        now_ms: Some(unix_timestamp_ms()),
                    })
                    .await
                    .map(|run| match active_locale {
                        ResolvedLocale::En => format!("Automation run is {:?}", run.status),
                        ResolvedLocale::ZhCn => format!("自动化运行状态：{:?}", run.status),
                        ResolvedLocale::ZhTw => format!("自動化執行狀態：{:?}", run.status),
                    })
            },
        );
    }

    fn cancel_automation_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        let (Ok(run_id), Some(runtime)) =
            (AutomationRunId::parse(run_id.clone()), self.runtime.clone())
        else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::AutomationCancel(run_id.as_str().to_string()),
            cx,
            async move {
                runtime
                    .management()
                    .automation()
                    .cancel_run(AutomationRunCancelRequest {
                        run_id,
                        now_ms: Some(unix_timestamp_ms()),
                        reason: Some("canceled from GPUI management".into()),
                    })
                    .map(|run| match active_locale {
                        ResolvedLocale::En => format!("Automation run is {:?}", run.status),
                        ResolvedLocale::ZhCn => format!("自动化运行状态：{:?}", run.status),
                        ResolvedLocale::ZhTw => format!("自動化執行狀態：{:?}", run.status),
                    })
            },
        );
    }

    fn confirm_automation_archive(
        &mut self,
        graph_id: String,
        graph_title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let graph_id = graph_id.clone();
            let title = match active_locale {
                ResolvedLocale::En => format!("Archive automation graph {graph_title}?"),
                ResolvedLocale::ZhCn => format!("归档自动化图“{graph_title}”？"),
                ResolvedLocale::ZhTw => format!("封存自動化圖「{graph_title}」？"),
            };
            dialog
                .title(title)
                .child(management_locale_text_for(
                    active_locale,
                    "The graph will stop appearing in active lists; run history remains available.",
                    "归档后，该图不会再出现在活动列表中，运行历史仍会保留。",
                    "封存後，該圖不會再出現在活動清單中，執行歷史仍會保留。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(gpui_component::dialog::DialogClose::new().child(
                            Button::new("cancel-automation-archive").outline().label(
                                management_locale_text_for(active_locale, "Cancel", "取消", "取消"),
                            ),
                        ))
                        .child(gpui_component::dialog::DialogAction::new().child(
                            Button::new("confirm-automation-archive").danger().label(
                                management_locale_text_for(
                                    active_locale,
                                    "Archive",
                                    "归档",
                                    "封存",
                                ),
                            ),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        if let (Some(runtime), Ok(id)) = (
                            this.runtime.clone(),
                            AutomationGraphId::parse(graph_id.clone()),
                        ) {
                            this.begin_simple_task(
                                ManagementMutation::AutomationArchive(graph_id.clone()),
                                cx,
                                async move {
                                    runtime.management().automation().archive(&id).map(|_| {
                                        management_locale_text_for(
                                            active_locale,
                                            "Automation graph archived",
                                            "自动化图已归档",
                                            "自動化圖已封存",
                                        )
                                        .to_string()
                                    })
                                },
                            );
                        }
                    });
                    true
                })
        });
    }

    fn preview_native_import(
        &mut self,
        import: bool,
        target_agent_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let target_agent_id = match target_agent_id.map(AgentId::parse).transpose() {
            Ok(agent_id) => agent_id,
            Err(_) => {
                self.error = Some(
                    management_error_text("Invalid Agent id", "Agent 标识无效", "Agent 識別碼無效")
                        .into(),
                );
                cx.notify();
                return;
            }
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let existing_profiles = self.provider_profiles.clone();
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderPreview(
                if import {
                    "native-import"
                } else {
                    "native-scan"
                }
                .into(),
            ),
            cx,
            async move {
                let providers = runtime.management().providers().management();
                let request = vibex_core::ProviderNativeImportPreviewRequest {
                    sources: vec![
                        vibex_core::ProviderNativeImportSource::Codex,
                        vibex_core::ProviderNativeImportSource::Claude,
                        vibex_core::ProviderNativeImportSource::CcSwitch,
                    ],
                };
                let preview = providers.preview_native_import(request.clone())?;
                if !import {
                    return Ok(match active_locale {
                        ResolvedLocale::En => format!(
                            "Native preview: {} item(s), {} file(s)",
                            preview.items.len(),
                            preview.files.len()
                        ),
                        ResolvedLocale::ZhCn => format!(
                            "原生配置预览：{} 个项目、{} 个文件",
                            preview.items.len(),
                            preview.files.len()
                        ),
                        ResolvedLocale::ZhTw => format!(
                            "原生配置預覽：{} 個項目、{} 個檔案",
                            preview.items.len(),
                            preview.files.len()
                        ),
                    });
                }
                if let Some(target_agent_id) = target_agent_id.as_ref() {
                    let import_item_ids = pending_cc_switch_import_item_ids(
                        &preview,
                        &existing_profiles,
                        target_agent_id,
                    );
                    if import_item_ids.is_empty() {
                        return Err(VibexError::validation(
                            "provider_native_import_no_candidate",
                            "no importable cc-switch Provider record was found for this Agent",
                        ));
                    }
                    let mut imported_count = 0usize;
                    let mut missing_secret_count = 0usize;
                    for import_item_id in import_item_ids {
                        let result = providers.create_profile_from_import(
                            vibex_core::ProviderNativeImportCreateRequest {
                                preview_request: request.clone(),
                                import_item_id,
                            },
                        )?;
                        imported_count += 1;
                        if result.diagnostics.iter().any(|diagnostic| {
                            diagnostic.code
                                == "provider_native_import_cc_switch_secret_keychain_unavailable"
                        }) {
                            missing_secret_count += 1;
                        }
                    }
                    return Ok(match active_locale {
                        ResolvedLocale::En if missing_secret_count > 0 => format!(
                            "Imported {imported_count} cc-switch configuration(s); {missing_secret_count} require API Key setup"
                        ),
                        ResolvedLocale::En => {
                            format!("Imported {imported_count} cc-switch configuration(s)")
                        }
                        ResolvedLocale::ZhCn if missing_secret_count > 0 => format!(
                            "已导入 {imported_count} 个 cc-switch 配置，其中 {missing_secret_count} 个需要补充 API Key"
                        ),
                        ResolvedLocale::ZhCn => {
                            format!("已导入 {imported_count} 个 cc-switch 配置")
                        }
                        ResolvedLocale::ZhTw if missing_secret_count > 0 => format!(
                            "已匯入 {imported_count} 個 cc-switch 配置，其中 {missing_secret_count} 個需要補充 API Key"
                        ),
                        ResolvedLocale::ZhTw => {
                            format!("已匯入 {imported_count} 個 cc-switch 配置")
                        }
                    });
                }
                let item = preview
                    .items
                    .iter()
                    .find(|item| native_import_status_is_eligible(item.status))
                    .ok_or_else(|| {
                        VibexError::validation(
                            "provider_native_import_no_candidate",
                            "no importable native Provider record was found",
                        )
                    })?;
                providers
                    .create_profile_from_import(vibex_core::ProviderNativeImportCreateRequest {
                        preview_request: request,
                        import_item_id: item.import_item_id.clone(),
                    })
                    .map(|result| match active_locale {
                        ResolvedLocale::En => {
                            format!("Imported native profile {}", result.profile.display_name)
                        }
                        ResolvedLocale::ZhCn => {
                            format!("已导入原生配置 {}", result.profile.display_name)
                        }
                        ResolvedLocale::ZhTw => {
                            format!("已匯入原生配置 {}", result.profile.display_name)
                        }
                    })
            },
        );
    }

    fn preview_native_export(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let (Ok(provider_profile_id), Some(runtime)) = (
            vibex_core::ProviderProfileId::parse(profile_id.clone()),
            self.runtime.clone(),
        ) else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        let source = self.native_export_source;
        let mode = self.native_export_mode;
        let active_locale = locale::current_locale();
        self.mutation = Some(ManagementMutation::ProviderPreview(format!(
            "native-export:{profile_id}"
        )));
        self.native_export_preview = None;
        self.error = None;
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .management()
                .providers()
                .management()
                .preview_native_export(vibex_core::ProviderNativeExportPreviewRequest {
                    provider_profile_id,
                    source,
                    mode,
                    persist: true,
                })
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(preview)) => {
                        let selected_profile_id = this
                            .selected_management_provider_profile()
                            .map(|profile| profile.id.as_str().to_string());
                        if native_export_preview_matches(
                            &preview,
                            selected_profile_id.as_deref(),
                            this.native_export_source,
                            this.native_export_mode,
                        ) {
                            this.notice = Some(match active_locale {
                                ResolvedLocale::En => format!(
                                    "Export preview {}: {} file(s), {} diagnostic(s)",
                                    preview.export_id,
                                    preview.files.len(),
                                    preview.diagnostics.len()
                                ),
                                ResolvedLocale::ZhCn => format!(
                                    "导出预览 {}：{} 个文件、{} 条诊断",
                                    preview.export_id,
                                    preview.files.len(),
                                    preview.diagnostics.len()
                                ),
                                ResolvedLocale::ZhTw => format!(
                                    "匯出預覽 {}：{} 個檔案、{} 條診斷",
                                    preview.export_id,
                                    preview.files.len(),
                                    preview.diagnostics.len()
                                ),
                            });
                            this.native_export_preview = Some(preview);
                        }
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Native export preview failed",
                                "原生配置导出预览失败",
                                "原生配置匯出預覽失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn apply_native_export(&mut self, export_id: String, cx: &mut Context<Self>) {
        let (Ok(export_id), Some(runtime)) = (
            vibex_core::RequestId::parse(export_id),
            self.runtime.clone(),
        ) else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderPreview("native-export-apply".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .apply_native_export(vibex_core::ProviderNativeExportApplyRequest { export_id })
                    .map(|result| match active_locale {
                        ResolvedLocale::En => format!("Native export {:?}", result.status),
                        ResolvedLocale::ZhCn => format!("原生配置导出：{:?}", result.status),
                        ResolvedLocale::ZhTw => format!("原生配置匯出：{:?}", result.status),
                    })
            },
        );
    }

    fn rollback_native_export(&mut self, export_id: String, cx: &mut Context<Self>) {
        let (Ok(export_id), Some(runtime)) = (
            vibex_core::RequestId::parse(export_id),
            self.runtime.clone(),
        ) else {
            return;
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(
            ManagementMutation::ProviderPreview("native-export-rollback".into()),
            cx,
            async move {
                runtime
                    .management()
                    .providers()
                    .management()
                    .rollback_native_export(vibex_core::ProviderNativeExportRollbackRequest {
                        export_id,
                    })
                    .map(|result| match active_locale {
                        ResolvedLocale::En => format!("Native rollback {:?}", result.status),
                        ResolvedLocale::ZhCn => format!("原生配置回滚：{:?}", result.status),
                        ResolvedLocale::ZhTw => format!("原生配置回復：{:?}", result.status),
                    })
            },
        );
    }

    fn set_compact_sidebar_height(
        &mut self,
        height: f32,
        min_height: f32,
        max_height: f32,
        cx: &mut Context<Self>,
    ) {
        let height = height.round().clamp(min_height, max_height);
        if (height - self.compact_sidebar_height).abs() < f32::EPSILON {
            return;
        }
        self.compact_sidebar_height = height;
        cx.notify();
    }

    fn update_compact_sidebar_resize(&mut self, window_y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.compact_sidebar_resize_drag else {
            return;
        };
        self.set_compact_sidebar_height(
            drag.start_height + window_y - drag.start_window_y,
            drag.min_height,
            drag.max_height,
            cx,
        );
    }

    fn finish_compact_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.compact_sidebar_resize_drag.take().is_some() {
            cx.notify();
        }
    }

    fn render_compact_sidebar_resize_handle(
        &mut self,
        min_height: f32,
        max_height: f32,
        displayed_height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resize_active = self.compact_sidebar_resize_drag.is_some();
        let resize_highlighted = resize_active || self.compact_sidebar_resize_hovered;
        let increment_target = cx.weak_entity();
        let decrement_target = cx.weak_entity();
        let handle = div()
            .h(px(MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_THICKNESS))
            .w(px(MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_WIDTH))
            .rounded_full()
            .bg(cx.theme().border);
        let handle = if resize_highlighted {
            let idle_color = cx.theme().border;
            let highlighted_color = cx.theme().drag_border.opacity(0.92);
            handle
                .with_animation(
                    "management-compact-sidebar-resize-highlight",
                    Animation::new(Duration::from_millis(
                        MANAGEMENT_COMPACT_RESIZE_HANDLE_ANIMATION_MS,
                    ))
                    .with_easing(ease_out_cubic),
                    move |handle, delta| {
                        let width = MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_WIDTH
                            + (MANAGEMENT_COMPACT_RESIZE_HANDLE_HOVER_WIDTH
                                - MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_WIDTH)
                                * delta;
                        let thickness = MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_THICKNESS
                            + (MANAGEMENT_COMPACT_RESIZE_HANDLE_HOVER_THICKNESS
                                - MANAGEMENT_COMPACT_RESIZE_HANDLE_IDLE_THICKNESS)
                                * delta;
                        handle.w(px(width)).h(px(thickness)).bg(gpui::Hsla {
                            h: idle_color.h + (highlighted_color.h - idle_color.h) * delta,
                            s: idle_color.s + (highlighted_color.s - idle_color.s) * delta,
                            l: idle_color.l + (highlighted_color.l - idle_color.l) * delta,
                            a: idle_color.a + (highlighted_color.a - idle_color.a) * delta,
                        })
                    },
                )
                .into_any_element()
        } else {
            handle.into_any_element()
        };
        h_flex()
            .id("management-compact-sidebar-resize")
            .role(Role::Splitter)
            .aria_label(management_locale_text(
                "Resize configuration sidebar",
                "调整配置侧栏高度",
                "調整配置側欄高度",
            ))
            .aria_orientation(Orientation::Horizontal)
            .aria_numeric_value(displayed_height as f64)
            .aria_numeric_value_step(MANAGEMENT_COMPACT_RESIZE_STEP as f64)
            .aria_min_numeric_value(min_height as f64)
            .aria_max_numeric_value(max_height as f64)
            .focusable()
            .tab_index(0)
            .h(px(MANAGEMENT_COMPACT_RESIZE_HANDLE_HEIGHT))
            .w_full()
            .flex_none()
            .cursor_ns_resize()
            .items_center()
            .justify_center()
            .border_t_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(if resize_active {
                cx.theme().accent.opacity(0.30)
            } else {
                cx.theme().background
            })
            .hover(|style| style.bg(cx.theme().accent.opacity(0.24)))
            .on_hover(cx.listener(|this, hovered, _, cx| {
                if this.compact_sidebar_resize_hovered != *hovered {
                    this.compact_sidebar_resize_hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let next = match event.keystroke.key.as_str() {
                    "up" => this.compact_sidebar_height - MANAGEMENT_COMPACT_RESIZE_STEP,
                    "down" => this.compact_sidebar_height + MANAGEMENT_COMPACT_RESIZE_STEP,
                    "home" => min_height,
                    "end" => max_height,
                    _ => return,
                };
                this.set_compact_sidebar_height(next, min_height, max_height, cx);
                cx.stop_propagation();
            }))
            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                let _ = increment_target.update(cx, |this, cx| {
                    this.set_compact_sidebar_height(
                        this.compact_sidebar_height + MANAGEMENT_COMPACT_RESIZE_STEP,
                        min_height,
                        max_height,
                        cx,
                    )
                });
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                let _ = decrement_target.update(cx, |this, cx| {
                    this.set_compact_sidebar_height(
                        this.compact_sidebar_height - MANAGEMENT_COMPACT_RESIZE_STEP,
                        min_height,
                        max_height,
                        cx,
                    )
                });
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.compact_sidebar_resize_drag = Some(ManagementSidebarResizeDragState {
                        start_window_y: f32::from(event.position.y),
                        start_height: displayed_height,
                        min_height,
                        max_height,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_drag(ManagementSidebarResizeDrag, |_, _, _, cx| {
                cx.new(|_| ManagementSidebarResizeDrag)
            })
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<ManagementSidebarResizeDrag>, _, cx| {
                    cx.stop_propagation();
                    this.update_compact_sidebar_resize(f32::from(event.event.position.y), cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_compact_sidebar_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_compact_sidebar_resize(cx)),
            )
            .child(handle)
            .into_any_element()
    }

    fn render_nav(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let copy = management_copy();
        let active = management_primary_section(self.navigation.active);
        let items = [
            (ManagementSection::Agents, copy.agents, IconName::Bot),
            (ManagementSection::Mcp, copy.mcp, IconName::Network),
            (ManagementSection::Skills, copy.skills, IconName::BookOpen),
            (
                ManagementSection::Advanced,
                copy.advanced,
                IconName::Settings2,
            ),
        ];
        let mut nav = h_flex()
            .id("management-section-nav")
            .w_full()
            .h(px(42.0))
            .flex_none()
            .items_center()
            .gap_1()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.20))
            .p_1();
        for (section, label, icon) in items {
            nav = nav.child(
                Button::new(SharedString::from(format!(
                    "management-primary-nav-{}",
                    section.key()
                )))
                .small()
                .ghost()
                .flex_1()
                .h(px(32.0))
                .px_1()
                .rounded(gpui_component::button::ButtonRounded::Size(px(6.0)))
                .selected(section == active)
                .icon(icon)
                .label(label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.request_section_switch(section, window, cx);
                })),
            );
        }
        nav.into_any_element()
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let copy = management_copy();
        h_flex()
            .w_full()
            .h(px(MANAGEMENT_HEADER_HEIGHT))
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .px_3()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .child(Icon::new(IconName::Settings2).size(px(16.0)))
                    .child(div().truncate().text_sm().font_medium().child(copy.title)),
            )
            .into_any_element()
    }

    fn render_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.navigation.active {
            ManagementSection::Agents | ManagementSection::ModelProviders => {
                self.render_providers(window, cx)
            }
            ManagementSection::Mcp => self.render_mcp(cx),
            ManagementSection::Skills => self.render_skills(cx),
            ManagementSection::PromptsHooks => {
                self.render_prompts_hooks(f32::from(window.viewport_size().width) >= 1536.0, cx)
            }
            ManagementSection::Advanced => self.render_advanced(window, cx),
            ManagementSection::Scheduled => self.render_scheduled(window, cx),
            ManagementSection::Automation => self.render_automation(cx),
            ManagementSection::Relay => self.render_relay(window, cx),
            ManagementSection::Recovery => self.render_recovery(cx),
        }
    }

    fn render_context_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match management_primary_section(self.navigation.active) {
            ManagementSection::Agents => self.render_agents(cx),
            ManagementSection::Mcp => self.render_mcp_sidebar(cx),
            ManagementSection::Skills => self.render_skills_sidebar(cx),
            ManagementSection::Advanced => self.render_advanced_sidebar(window, cx),
            _ => unreachable!("primary management section must be normalized"),
        }
    }

    fn render_agents(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.agent_search.read(cx).value().trim().to_lowercase();
        let mut agents = self
            .snapshot
            .agents
            .iter()
            .filter(|agent| management_agent_matches_search(agent, &query))
            .cloned()
            .collect::<Vec<_>>();
        agents.sort_by_cached_key(management_agent_sort_key);
        let mut agent_rows = v_flex().w_full().gap(px(6.0));
        if agents.is_empty() {
            agent_rows = agent_rows.child(compact_empty_state(
                management_no_matching_agents_title(),
                management_no_matching_agents_description(),
                cx,
            ));
        }
        for agent in agents {
            let id = agent.id.as_str().to_string();
            let mutation = self.agent_mutations.get(&id);
            let pending = mutation.is_some();
            let row_select_id = id.clone();
            let keyboard_select_id = id.clone();
            let toggle_id = id.clone();
            let probe_id = id.clone();
            let add_id = id.clone();
            let added = agent.added;
            let managed_installing = matches!(
                mutation,
                Some(ManagementMutation::AgentInstall(active_id)) if active_id == &id
            );
            let selected = (added || agent.managed_install.managed || managed_installing)
                && self.selected_agent_id.as_deref() == Some(id.as_str());
            let enabled = agent.enabled;
            let show_install_prompt = added
                && !agent.managed_install.managed
                && agent.install_status == vibex_core::AgentInstallStatus::Missing;
            let status_missing = added
                && (show_install_prompt
                    || (agent.enabled
                        && agent.runtime_status != vibex_core::AgentRuntimeStatus::Ready));
            let status_label = management_agent_status_label(&agent);
            let status_tooltip = SharedString::from(status_label);
            let status_indicator = div()
                .id(SharedString::from(format!("management-agent-status-{id}")))
                .role(Role::Image)
                .aria_label(status_label)
                .size(px(16.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(status_missing, |indicator| {
                    indicator
                        .text_sm()
                        .font_bold()
                        .text_color(cx.theme().danger)
                        .child("!")
                })
                .when(!status_missing, |indicator| {
                    indicator.child(div().size(px(6.0)).rounded(px(3.0)).bg(
                        if added && agent.enabled {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground.opacity(0.55)
                        },
                    ))
                })
                .tooltip(move |window, cx| Tooltip::new(status_tooltip.clone()).build(window, cx));
            let profile_count = self
                .snapshot
                .profiles
                .iter()
                .filter(|profile| profile.agent_id == id)
                .count();
            let model_provider_configuration_supported =
                self.model_provider_agent_ids.contains(&id);
            let row =
                v_flex()
                    .id(SharedString::from(format!("management-agent-row-{id}")))
                    .aria_label(agent.label.clone())
                    .relative()
                    .w_full()
                    .gap_1()
                    .overflow_hidden()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(if selected { 0.0 } else { 0.65 }))
                    .bg(if selected {
                        cx.theme().primary.opacity(0.08)
                    } else {
                        cx.theme().background.opacity(0.70)
                    })
                    .px_2()
                    .py_2()
                    .hover(|style| {
                        style.bg(if selected {
                            cx.theme().primary.opacity(0.08)
                        } else {
                            cx.theme().accent
                        })
                    })
                    .when(added, |row| {
                        row.role(Role::Button)
                            .focusable()
                            .tab_index(0)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_management_agent(row_select_id.clone(), cx);
                            }))
                            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "enter" || event.keystroke.key == "space"
                                {
                                    this.select_management_agent(keyboard_select_id.clone(), cx);
                                    cx.stop_propagation();
                                }
                            }))
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_1()
                            .child(management_agent_glyph(
                                agent.id.as_str(),
                                &agent.label,
                                selected,
                                cx,
                            ))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(28.0))
                                    .px_1()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_medium()
                                            .child(agent.label.clone()),
                                    )
                                    .child(status_indicator)
                                    .child(div().flex_1()),
                            )
                            .when(show_install_prompt, |actions| {
                                actions.child(
                                    Button::new(SharedString::from(format!(
                                        "management-agent-probe-{probe_id}"
                                    )))
                                    .small()
                                    .outline()
                                    .h(px(34.0))
                                    .px_3()
                                    .icon(IconName::ExternalLink)
                                    .label(management_install_label())
                                    .loading(matches!(
                                        mutation,
                                        Some(ManagementMutation::AgentToggle(action))
                                            if action == &format!("probe:{probe_id}")
                                    ))
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.probe_agent(probe_id.clone(), cx)
                                    })),
                                )
                            })
                            .when(added && !show_install_prompt, |actions| {
                                actions.child(
                                    Switch::new(SharedString::from(format!(
                                        "management-agent-toggle-{toggle_id}"
                                    )))
                                    .small()
                                    .checked(enabled)
                                    .disabled(pending)
                                    .tooltip(management_agent_toggle_selector_label())
                                    .on_click(cx.listener(move |this, checked, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_agent(toggle_id.clone(), *checked, cx)
                                    })),
                                )
                            })
                            .when(!added, |actions| {
                                actions.child(button_with_aria_label(
                                    Button::new(SharedString::from(format!(
                                        "management-agent-add-{add_id}"
                                    )))
                                    .small()
                                    .outline()
                                    .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                                    .icon(IconName::Plus)
                                    .tooltip(management_add_label())
                                    .loading(
                                        managed_installing
                                            || matches!(
                                                mutation,
                                                Some(ManagementMutation::AgentToggle(action))
                                                    if action == &format!("add:{add_id}")
                                            ),
                                    )
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.set_agent_added(add_id.clone(), true, cx)
                                    })),
                                    management_add_label(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .pl(px(40.0))
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if added && model_provider_configuration_supported {
                                management_profile_count(profile_count)
                            } else {
                                status_label.to_string()
                            }),
                    );
            agent_rows = agent_rows.child(row);
        }

        v_flex()
            .size_full()
            .min_h_0()
            .gap(px(10.0))
            .child(management_search_input(&self.agent_search, cx))
            .child(
                div()
                    .id("management-agent-list-scroll")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .pr_1()
                    .child(agent_rows),
            )
            .into_any_element()
    }

    fn render_mcp_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let copy = management_copy();
        let query = self.mcp_search.read(cx).value().trim().to_lowercase();
        let servers = self
            .snapshot
            .mcp_servers
            .iter()
            .filter(|server| {
                query.is_empty()
                    || format!(
                        "{} {} {} {:?} {:?} {} {} {}",
                        server.display_name,
                        server.id.as_str(),
                        server.description.as_deref().unwrap_or_default(),
                        server.transport_kind,
                        server.scope_kind,
                        server.command.as_deref().unwrap_or_default(),
                        server.url.as_deref().unwrap_or_default(),
                        server.tags.join(" ")
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let resource_count = servers.len();
        let mut rows = v_flex()
            .w_full()
            .gap(px(6.0))
            .child(management_resource_sidebar_header(
                management_mcp_resources_title(),
                resource_count,
                cx,
            ));
        if servers.is_empty() {
            rows = rows.child(compact_empty_state(
                management_no_mcp_title(),
                management_no_mcp_description(),
                cx,
            ));
        }
        for server in servers {
            let id = server.id.as_str().to_string();
            let select_id = id.clone();
            let selected = self.selected_mcp_id.as_deref() == Some(id.as_str());
            let enabled_count = server
                .agent_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .count();
            let title = server.display_name;
            let subtitle = format!(
                "{} · {}",
                server.transport_kind,
                management_mcp_scope_label(server.scope_kind)
            );
            let status = management_resource_status_key(
                server.status == vibex_core::McpServerStatus::Enabled,
            );
            let enabled_count_label = management_enabled_agent_count(enabled_count);
            let accessible_label = format!("{title}, {status}, {subtitle}, {enabled_count_label}");
            rows = rows.child(button_with_aria_label(
                Button::new(SharedString::from(format!(
                    "management-mcp-select-{select_id}"
                )))
                .small()
                .ghost()
                .w_full()
                .h(px(72.0))
                .justify_start()
                .rounded(px(8.0))
                .border_1()
                .border_color(if selected {
                    cx.theme().ring.opacity(0.60)
                } else {
                    cx.theme().border.opacity(0.70)
                })
                .bg(if selected {
                    cx.theme().accent.opacity(0.35)
                } else {
                    cx.theme().background.opacity(0.70)
                })
                .px(px(10.0))
                .py_2()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(management_resource_sidebar_glyph(
                            "icons/vibex/boxes.svg",
                            cx,
                        ))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap(px(2.0))
                                .child(
                                    h_flex()
                                        .min_w_0()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_sm()
                                                .font_medium()
                                                .child(title),
                                        )
                                        .child(management_status_badge(status.to_string(), cx)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(enabled_count_label),
                                ),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_mcp_id = Some(select_id.clone());
                    cx.notify();
                })),
                accessible_label,
            ));
        }
        v_flex()
            .size_full()
            .min_h_0()
            .gap(px(10.0))
            .child(
                Button::new("management-mcp-import-sidebar")
                    .small()
                    .secondary()
                    .w_full()
                    .h(px(32.0))
                    .justify_start()
                    .icon(Icon::default().path("icons/vibex/import.svg"))
                    .label(copy.import_mcp)
                    .on_click(cx.listener(|this, _, window, cx| this.open_mcp_import(window, cx))),
            )
            .child(management_search_input(&self.mcp_search, cx))
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .pr_1()
                    .child(rows),
            )
            .into_any_element()
    }

    fn render_skills_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let copy = management_copy();
        let query = self.skill_search.read(cx).value().trim().to_lowercase();
        let skills = self
            .snapshot
            .skills
            .iter()
            .filter(|skill| {
                query.is_empty()
                    || format!(
                        "{} {} {} {:?} {:?} {} {}",
                        skill.display_name,
                        skill.id.as_str(),
                        skill.description.as_deref().unwrap_or_default(),
                        skill.source_kind,
                        skill.scope_kind,
                        skill.source_uri.as_deref().unwrap_or_default(),
                        skill.tags.join(" ")
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let resource_count = skills.len();
        let mut rows = v_flex()
            .w_full()
            .gap(px(6.0))
            .child(management_resource_sidebar_header(
                management_skills_title(),
                resource_count,
                cx,
            ));
        if skills.is_empty() {
            rows = rows.child(compact_empty_state(
                management_no_skills_title(),
                management_no_skills_description(),
                cx,
            ));
        }
        for skill in skills {
            let id = skill.id.as_str().to_string();
            let select_id = id.clone();
            let selected = self.selected_skill_id.as_deref() == Some(id.as_str());
            let enabled_count = skill
                .agent_matrix
                .iter()
                .filter(|entry| entry.enabled)
                .count();
            let title = skill.display_name;
            let subtitle = format!(
                "{} · {}",
                management_skill_source_kind_label(skill.source_kind),
                management_skill_scope_label(skill.scope_kind)
            );
            let status =
                management_resource_status_key(skill.status == vibex_core::SkillStatus::Enabled);
            let enabled_count_label = management_enabled_agent_count(enabled_count);
            let accessible_label = format!("{title}, {status}, {subtitle}, {enabled_count_label}");
            rows = rows.child(button_with_aria_label(
                Button::new(SharedString::from(format!(
                    "management-skill-select-{select_id}"
                )))
                .small()
                .ghost()
                .w_full()
                .h(px(72.0))
                .justify_start()
                .rounded(px(8.0))
                .border_1()
                .border_color(if selected {
                    cx.theme().ring.opacity(0.60)
                } else {
                    cx.theme().border.opacity(0.70)
                })
                .bg(if selected {
                    cx.theme().accent.opacity(0.35)
                } else {
                    cx.theme().background.opacity(0.70)
                })
                .px(px(10.0))
                .py_2()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(management_resource_sidebar_glyph(
                            "icons/vibex/library.svg",
                            cx,
                        ))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap(px(2.0))
                                .child(
                                    h_flex()
                                        .min_w_0()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_sm()
                                                .font_medium()
                                                .child(title),
                                        )
                                        .child(management_status_badge(status.to_string(), cx)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(enabled_count_label),
                                ),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_skill_id = Some(select_id.clone());
                    cx.notify();
                })),
                accessible_label,
            ));
        }
        v_flex()
            .size_full()
            .min_h_0()
            .gap(px(10.0))
            .child(
                Button::new("management-skill-import-sidebar")
                    .small()
                    .secondary()
                    .w_full()
                    .h(px(32.0))
                    .justify_start()
                    .icon(Icon::default().path("icons/vibex/import.svg"))
                    .label(copy.import_skill)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_skill_import(window, cx)),
                    ),
            )
            .child(management_search_input(&self.skill_search, cx))
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .pr_1()
                    .child(rows),
            )
            .into_any_element()
    }

    fn render_advanced_sidebar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = [
            (
                management_locale_text("Native", "原生", "原生"),
                management_locale_text("Native compatibility sync", "原生兼容同步", "原生相容同步"),
                IconName::Replace,
            ),
            (
                management_locale_text("Health", "健康检查", "健康檢查"),
                management_locale_text(
                    "Agent probes and capability checks",
                    "Agent 探测和能力检查",
                    "Agent 探測與能力檢查",
                ),
                IconName::CircleCheck,
            ),
            (
                management_locale_text("Prompts", "提示词", "提示詞"),
                management_locale_text(
                    "Reusable prompt library",
                    "可复用提示词库",
                    "可重用提示詞庫",
                ),
                IconName::BookOpen,
            ),
            (
                "Hooks",
                management_locale_text("Advanced command hooks", "高级命令 Hook", "進階命令 Hook"),
                IconName::Settings2,
            ),
        ];
        let mut sidebar = v_flex().w_full().gap_2();
        for (title, subtitle, icon) in rows {
            sidebar = sidebar.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .px_3()
                    .py_2()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(icon).size(px(16.0)))
                            .child(div().truncate().text_sm().font_medium().child(title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    ),
            );
        }
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .pr_1()
                    .child(sidebar),
            )
            .into_any_element()
    }

    fn render_profile_model_section(
        &mut self,
        selected_agent_id: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending =
            self.mutation.is_some() || self.agent_mutations.contains_key(&selected_agent_id);
        let wire_api_choices = self.projection_editor.wire_api_choices();
        let shows_wire_api = self
            .projection_editor
            .shows(vibex_core::AgentProjectionFormControl::WireProtocol);
        let fetching_models = self.editing_profile_id.as_ref().is_some_and(|profile_id| {
            matches!(
                &self.mutation,
                Some(ManagementMutation::ProviderProbe(action))
                    if action == &format!("models:{profile_id}")
            )
        });
        let mut model_rows = v_flex().w_full().gap_2();
        for (index, model) in self
            .profile_configured_models
            .clone()
            .into_iter()
            .enumerate()
        {
            let enabled = model.enabled;
            let edit_index = index;
            model_rows = model_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.65))
                    .bg(cx.theme().background.opacity(0.70))
                    .px_2()
                    .py_2()
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_medium()
                                    .child(model.display_name.unwrap_or_else(|| model.id.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match model.wire_api {
                                        Some(wire_api) => format!(
                                            "{} · {}",
                                            model.id,
                                            provider_wire_api_label(wire_api)
                                        ),
                                        None => format!(
                                            "{} · {}",
                                            model.id,
                                            management_locale_text(
                                                "Inherit provider default",
                                                "继承供应商默认值",
                                                "繼承供應商預設值",
                                            )
                                        ),
                                    }),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("provider-model-edit-{index}")))
                            .xsmall()
                            .ghost()
                            .compact()
                            .icon(IconName::Settings2)
                            .tooltip(management_locale_text("Edit model", "编辑模型", "編輯模型"))
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_profile_model_editor(edit_index, window, cx)
                            })),
                    )
                    .child(
                        Switch::new(SharedString::from(format!(
                            "provider-model-enabled-{index}"
                        )))
                        .small()
                        .checked(enabled)
                        .disabled(pending)
                        .tooltip(management_enabled_label(enabled))
                        .on_click(cx.listener(
                            move |this, checked, _, cx| {
                                this.toggle_profile_model(index, *checked, cx)
                            },
                        )),
                    )
                    .child(
                        Button::new(SharedString::from(format!("provider-model-delete-{index}")))
                            .xsmall()
                            .ghost()
                            .compact()
                            .icon(IconName::Delete)
                            .tooltip(management_locale_text(
                                "Delete model",
                                "删除模型",
                                "刪除模型",
                            ))
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_profile_model(index, cx)
                            })),
                    ),
            );
        }

        let model_editor = self
            .profile_model_edit_index
            .filter(|index| self.profile_configured_models.get(*index).is_some())
            .map(|index| (index, self.profile_model_edit_wire_api))
            .map(|(index, wire_api)| {
                let mut wire_controls = h_flex().w_full().flex_wrap().gap_1();
                let candidates = std::iter::once(None)
                    .chain(wire_api_choices.iter().copied().map(Some))
                    .collect::<Vec<_>>();
                for candidate in candidates {
                    let label = candidate.map_or_else(
                        || management_locale_text("Inherit", "继承", "繼承").to_string(),
                        |wire_api| {
                            let support = self
                                .projection_editor
                                .wire_api_integration_kind(wire_api)
                                .map(provider_interface_integration_label)
                                .unwrap_or_else(|| {
                                    management_locale_text("Unsupported", "不支持", "不支援")
                                });
                            format!("{} · {support}", provider_wire_api_label(wire_api))
                        },
                    );
                    wire_controls = wire_controls.child(
                        Button::new(SharedString::from(format!(
                            "provider-model-wire-{index}-{candidate:?}"
                        )))
                        .small()
                        .ghost()
                        .selected(wire_api == candidate)
                        .label(label)
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_profile_model_wire_api(index, candidate, cx)
                        })),
                    );
                }
                v_flex()
                    .w_full()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(cx.theme().border.opacity(0.70))
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(management_locale_text("Edit model", "编辑模型", "編輯模型")),
                    )
                    .child(management_input_field(
                        management_locale_text("Model ID", "模型 ID", "模型 ID"),
                        &self.profile_model_edit_id,
                        false,
                        cx,
                    ))
                    .child(management_input_field(
                        management_locale_text("Display name", "显示名称", "顯示名稱"),
                        &self.profile_model_edit_name,
                        false,
                        cx,
                    ))
                    .when(shows_wire_api, |editor| {
                        editor
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(management_locale_text(
                                        "Model API protocol",
                                        "模型接口协议",
                                        "模型介面協定",
                                    )),
                            )
                            .child(wire_controls)
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("provider-model-edit-cancel")
                                    .small()
                                    .ghost()
                                    .label(management_cancel_label())
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_profile_model_editor(cx)
                                    })),
                            )
                            .child(
                                Button::new("provider-model-edit-save")
                                    .small()
                                    .secondary()
                                    .label(management_locale_text(
                                        "Apply model changes",
                                        "应用模型修改",
                                        "套用模型修改",
                                    ))
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_profile_model_editor(cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            });

        v_flex()
            .w_full()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border.opacity(0.70))
            .bg(cx.theme().muted.opacity(0.25))
            .p_3()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(management_locale_text("Models", "模型", "模型")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.profile_configured_models.is_empty() {
                                        management_locale_text("No models", "没有模型", "沒有模型")
                                            .to_string()
                                    } else {
                                        management_model_count(self.profile_configured_models.len())
                                    }),
                            ),
                    )
                    .when_some(self.editing_profile_id.clone(), |header, profile_id| {
                        let agent_id = selected_agent_id.clone();
                        header.child(
                            Button::new("provider-editor-fetch-models")
                                .small()
                                .outline()
                                .icon(IconName::Search)
                                .label(if fetching_models {
                                    management_locale_text("Fetching...", "获取中...", "取得中...")
                                } else {
                                    management_fetch_models_label()
                                })
                                .loading(fetching_models)
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.fetch_provider_models(
                                        profile_id.clone(),
                                        agent_id.clone(),
                                        cx,
                                    )
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&self.profile_model_draft).small().w_full()),
                    )
                    .child(
                        Button::new("provider-model-add")
                            .small()
                            .secondary()
                            .label(management_add_label())
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_profile_model(window, cx)
                            })),
                    ),
            )
            .child(if self.profile_configured_models.is_empty() {
                compact_empty_state(
                    management_locale_text(
                        "No configured models",
                        "暂无已配置模型",
                        "暫無已配置模型",
                    ),
                    management_locale_text(
                        "Fetch models or add a model ID manually.",
                        "可获取模型或手动添加模型 ID。",
                        "可取得模型或手動新增模型 ID。",
                    ),
                    cx,
                )
            } else {
                model_rows.into_any_element()
            })
            .when_some(model_editor, |models, editor| models.child(editor))
            .into_any_element()
    }

    fn render_projection_credential_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let surface = self.projection_editor.credential_surface();
        if surface == ProjectionCredentialSurface::ApiKey {
            return v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child("API Key"),
                        )
                        .when_some(
                            self.projection_editor
                                .capability
                                .as_ref()
                                .map(|capability| format!("{:?}", capability.auth_state)),
                            |row, status| row.child(management_status_badge(status, cx)),
                        ),
                )
                .child(
                    Input::new(&self.profile_api_key)
                        .small()
                        .w_full()
                        .mask_toggle()
                        .disabled(self.profile_secret_loading),
                )
                .into_any_element();
        }

        let (title, detail) = match surface {
            ProjectionCredentialSurface::OAuth => (
                "OAuth",
                management_locale_text(
                    "Agent or host authentication status",
                    "Agent 或主机认证状态",
                    "Agent 或主機驗證狀態",
                ),
            ),
            ProjectionCredentialSurface::Cloud => (
                management_locale_text("Cloud credential", "云凭证", "雲端憑證"),
                management_locale_text(
                    "Cloud profile and credential references",
                    "云端配置与凭证引用",
                    "雲端設定與憑證引用",
                ),
            ),
            ProjectionCredentialSurface::AgentManaged => (
                management_locale_text("Agent account", "Agent 账号", "Agent 帳號"),
                management_locale_text(
                    "Authentication is managed by the Agent",
                    "认证由 Agent 管理",
                    "驗證由 Agent 管理",
                ),
            ),
            ProjectionCredentialSurface::Local => (
                management_locale_text("Local runtime", "本地运行时", "本機執行階段"),
                management_locale_text(
                    "No remote credential is projected",
                    "不会投影远程凭证",
                    "不會投影遠端憑證",
                ),
            ),
            ProjectionCredentialSurface::ServiceMarketplace => (
                management_locale_text("Service marketplace", "服务市场", "服務市集"),
                management_locale_text(
                    "Authentication is owned by the service",
                    "认证由服务管理",
                    "驗證由服務管理",
                ),
            ),
            ProjectionCredentialSurface::Unsupported => (
                management_locale_text("Automatic credential", "自动凭证", "自動憑證"),
                management_locale_text(
                    "Unsupported or unverified for this compatible runtime",
                    "当前兼容运行时不支持或尚未验证",
                    "目前相容執行階段不支援或尚未驗證",
                ),
            ),
            ProjectionCredentialSurface::ApiKey => unreachable!(),
        };
        v_flex()
            .w_full()
            .gap_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.20))
            .px_3()
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_medium().child(title))
                    .when_some(
                        self.projection_editor
                            .capability
                            .as_ref()
                            .map(|capability| format!("{:?}", capability.auth_state)),
                        |row, status| row.child(management_status_badge(status, cx)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
            .into_any_element()
    }

    fn render_profile_editor_dialog(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.profile_editor_open {
            return div().size_full().into_any_element();
        }
        let selected_agent_id = self.selected_agent_id.clone().unwrap_or_default();
        let pending =
            self.mutation.is_some() || self.agent_mutations.contains_key(&selected_agent_id);
        let updating = self.editing_profile_id.is_some();
        let saving = matches!(
            self.mutation,
            Some(ManagementMutation::ProfileCreate) | Some(ManagementMutation::ProfileUpdate(_))
        );
        let shows_endpoint = self
            .projection_editor
            .shows(vibex_core::AgentProjectionFormControl::Endpoint);
        let shows_model = self
            .projection_editor
            .shows(vibex_core::AgentProjectionFormControl::Model);
        let shows_api_key =
            self.projection_editor.credential_surface() == ProjectionCredentialSurface::ApiKey;
        let credential_control = self.render_projection_credential_control(cx);
        let model_section =
            shows_model.then(|| self.render_profile_model_section(selected_agent_id, cx));
        let protocol_endpoints = shows_endpoint.then(|| {
            let mut section = v_flex().w_full().gap_2();
            for (wire_api, input) in &self.profile_protocol_base_urls {
                section = section.child(management_input_field(
                    provider_protocol_url_override_label(*wire_api),
                    input,
                    false,
                    cx,
                ));
            }
            section.into_any_element()
        });
        let mut form = v_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "Create, update, duplicate, delete, or set defaults.",
                        "创建、更新、复制、删除或设置默认项。",
                        "建立、更新、複製、刪除或設定預設項。",
                    )),
            )
            .child(management_input_field(
                management_locale_text("Provider name", "供应商名称", "供應商名稱"),
                &self.profile_name,
                false,
                cx,
            ))
            .child(management_input_field(
                management_locale_text("Note", "备注", "備註"),
                &self.profile_note,
                false,
                cx,
            ))
            .child(management_input_field(
                management_locale_text("Website URL", "官网链接", "官網連結"),
                &self.profile_website_url,
                false,
                cx,
            ))
            .child(credential_control)
            .when(shows_endpoint, |form| {
                form.child(management_input_field(
                    management_locale_text(
                        "Default API request URL",
                        "默认 API 请求地址",
                        "預設 API 請求位址",
                    ),
                    &self.profile_base_url,
                    false,
                    cx,
                ))
            })
            .when_some(protocol_endpoints, |form, endpoints| form.child(endpoints))
            .when_some(model_section, |form, section| form.child(section));
        if shows_api_key && self.profile_secret_loading {
            form = form.child(status_line(
                management_locale_text(
                    "Loading saved API Key...",
                    "正在加载已保存的 API Key...",
                    "正在載入已儲存的 API Key...",
                )
                .to_string(),
                false,
                cx,
            ));
        }
        if shows_api_key && updating && !self.profile_secret_touched {
            form = form.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "Saved Secret remains unchanged until this field is edited.",
                        "只有编辑此字段后才会修改已保存的密钥。",
                        "只有編輯此欄位後才會修改已儲存的金鑰。",
                    )),
            );
        }
        if let Some(error) = self.error.clone() {
            form = form.child(status_line(
                locale::localize_error_message(&error),
                true,
                cx,
            ));
        }

        v_flex()
            .size_full()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .pr_1()
                    .child(form),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .justify_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .child(
                        Button::new("provider-profile-save")
                            .small()
                            .when(updating, |button| button.secondary())
                            .when(!updating, |button| button.primary())
                            .label(if updating {
                                management_locale_text("Update", "更新", "更新")
                            } else {
                                management_locale_text("Create", "创建", "建立")
                            })
                            .loading(saving)
                            .disabled(pending || self.profile_secret_loading)
                            .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
                    )
                    .child(
                        Button::new("provider-profile-close")
                            .small()
                            .outline()
                            .label(management_locale_text(
                                "Close editor",
                                "关闭编辑器",
                                "關閉編輯器",
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_profile_editor(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn open_agent_auth_link(&mut self, url: String, cx: &mut Context<Self>) {
        match validate_external_open_url(&url)
            .and_then(|validated| crate::platform::open_external_url(&validated.url))
        {
            Ok(()) => {}
            Err(error) => {
                self.agent_auth_error = Some(format!("{}: {}", error.code, error.message));
                cx.notify();
            }
        }
    }

    fn render_agent_authentication(
        &mut self,
        window: &mut Window,
        provider_configuration: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_agent_auth_inputs(window, cx);
        self.ensure_agent_auth_terminal_surface(window, cx);
        let pending = self.mutation.is_some()
            || self
                .selected_agent_id
                .as_deref()
                .is_some_and(|agent_id| self.agent_mutations.contains_key(agent_id))
            || self.agent_auth_terminal_state == Some(AgentAuthTerminalState::Running);
        let selected_profile_label = self
            .selected_management_provider_profile()
            .map(|profile| profile.display_name.clone());
        let auth_available = self.runtime.is_some() && self.current_agent_auth_scope().is_some();
        let catalog = self.agent_auth_catalog.clone();
        let status = if !auth_available {
            management_locale_text("Agent disabled", "Agent 已停用", "Agent 已停用")
        } else if let Some(catalog) = catalog.as_ref() {
            match catalog.status {
                AgentAuthStatus::Authenticated => {
                    management_locale_text("Signed in", "已登录", "已登入")
                }
                AgentAuthStatus::AuthenticationRequired => {
                    management_locale_text("Sign-in required", "需要登录", "需要登入")
                }
                AgentAuthStatus::Unknown => {
                    management_locale_text("Not verified", "尚未验证", "尚未驗證")
                }
            }
        } else if !self.agent_auth_loading {
            management_locale_text("Unavailable", "暂不可用", "暫不可用")
        } else {
            management_locale_text("Discovering", "正在发现", "正在探索")
        };
        let supports_logout = catalog
            .as_ref()
            .is_some_and(|catalog| catalog.supports_logout);
        let mut content = v_flex().w_full().min_w_0().gap_3().child(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(management_status_badge(status.to_string(), cx))
                        .when_some(selected_profile_label, |header, label| {
                            header.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {label}",
                                        management_locale_text(
                                            "Credential profile",
                                            "凭据配置",
                                            "憑證設定",
                                        )
                                    )),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(management_detail_icon_action(
                            Button::new("agent-auth-refresh")
                                .small()
                                .outline()
                                .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                                .loading(self.agent_auth_loading)
                                .disabled(pending || self.agent_auth_loading || !auth_available)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.load_agent_auth(true, cx)),
                                ),
                            management_locale_text(
                                "Refresh methods",
                                "刷新认证方式",
                                "重新整理驗證方式",
                            ),
                        ))
                        .when(supports_logout, |actions| {
                            actions.child(management_detail_icon_action(
                                Button::new("agent-auth-logout")
                                    .small()
                                    .danger()
                                    .icon(IconName::ExternalLink)
                                    .loading(matches!(
                                        self.mutation,
                                        Some(ManagementMutation::AgentAuth(ref action))
                                            if action == "logout"
                                    ))
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, _, cx| this.logout_agent(cx))),
                                management_locale_text("Sign out", "退出登录", "登出"),
                            ))
                        }),
                ),
        );
        content = content.when_some(provider_configuration, |content, configuration| {
            content.child(configuration)
        });

        if !auth_available {
            content = content.child(status_line(
                management_locale_text(
                    "Enable this Agent to access its sign-in methods",
                    "启用此 Agent 后可使用其登录方式",
                    "啟用此 Agent 後可使用其登入方式",
                )
                .to_string(),
                false,
                cx,
            ));
        } else if self.agent_auth_loading && catalog.is_none() {
            content = content.child(status_line(
                management_locale_text(
                    "Reading authentication methods reported by the Agent...",
                    "正在读取 Agent 上报的认证方式...",
                    "正在讀取 Agent 回報的驗證方式...",
                )
                .to_string(),
                false,
                cx,
            ));
        }
        if let Some(catalog) = catalog {
            if catalog.methods.is_empty() {
                content = content.child(compact_empty_state(
                    management_locale_text(
                        "No sign-in method reported",
                        "Agent 未上报认证方式",
                        "Agent 未回報驗證方式",
                    ),
                    management_locale_text(
                        "This Agent may use credentials already configured by its own CLI.",
                        "此 Agent 可能使用其 CLI 中已有的登录状态。",
                        "此 Agent 可能使用其 CLI 中既有的登入狀態。",
                    ),
                    cx,
                ));
            }
            for method in catalog.methods {
                let method_loading = matches!(
                    self.mutation,
                    Some(ManagementMutation::AgentAuth(ref action)) if action == &method.id
                );
                let action_label = match method.kind {
                    AgentAuthMethodKind::Agent => management_locale_text("Sign in", "登录", "登入"),
                    AgentAuthMethodKind::Environment => {
                        management_locale_text("Save and sign in", "保存并登录", "儲存並登入")
                    }
                    AgentAuthMethodKind::Terminal => management_locale_text(
                        "Open sign-in terminal",
                        "打开登录终端",
                        "開啟登入終端",
                    ),
                };
                let action_icon = match method.kind {
                    AgentAuthMethodKind::Agent => Icon::new(IconName::ArrowRight),
                    AgentAuthMethodKind::Environment => Icon::new(IconName::Check),
                    AgentAuthMethodKind::Terminal => Icon::new(IconName::ArrowRight),
                };
                let submit_disabled = pending
                    || (method.kind == AgentAuthMethodKind::Environment
                        && self.selected_provider_profile_id.is_none());
                let submit_method_id = method.id.clone();
                let mut method_content =
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
                        .border_t_1()
                        .border_color(cx.theme().border.opacity(0.75))
                        .pt_3()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_start()
                                .justify_between()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .flex_1()
                                        .gap_1()
                                        .child(div().text_sm().font_semibold().child(method.name))
                                        .when_some(method.description, |title, description| {
                                            title.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(description),
                                            )
                                        }),
                                )
                                .child(management_detail_icon_action(
                                    Button::new(SharedString::from(format!(
                                        "agent-auth-submit-{}",
                                        method.id
                                    )))
                                    .small()
                                    .primary()
                                    .icon(action_icon)
                                    .loading(method_loading)
                                    .disabled(submit_disabled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.authenticate_agent(submit_method_id.clone(), cx)
                                    })),
                                    action_label,
                                )),
                        );

                for variable in method.environment {
                    let key = agent_auth_input_key(&method.id, &variable.name);
                    let Some(input) = self.agent_auth_inputs.get(&key) else {
                        continue;
                    };
                    let label = variable
                        .label
                        .as_deref()
                        .map(|label| format!("{label} · {}", variable.name))
                        .unwrap_or_else(|| variable.name.clone());
                    let input_element = if variable.secret {
                        Input::new(input)
                            .small()
                            .w_full()
                            .mask_toggle()
                            .into_any_element()
                    } else {
                        Input::new(input).small().w_full().into_any_element()
                    };
                    let clear_key = key.clone();
                    let clearing = self.agent_auth_clear_values.contains(&key);
                    let clear_label = if clearing {
                        management_locale_text("Keep saved value", "保留已保存值", "保留已儲存值")
                    } else {
                        management_locale_text("Clear saved value", "清除已保存值", "清除已儲存值")
                    };
                    method_content = method_content.child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .min_w_0()
                                            .flex_1()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_xs()
                                                    .font_medium()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(label),
                                            )
                                            .when(variable.optional, |row| {
                                                row.child(management_status_badge(
                                                    management_locale_text(
                                                        "Optional", "可选", "選填",
                                                    )
                                                    .to_string(),
                                                    cx,
                                                ))
                                            })
                                            .when(variable.configured && !clearing, |row| {
                                                row.child(management_status_badge(
                                                    management_locale_text(
                                                        "Configured",
                                                        "已配置",
                                                        "已設定",
                                                    )
                                                    .to_string(),
                                                    cx,
                                                ))
                                            }),
                                    )
                                    .when(variable.configured, |row| {
                                        row.child(management_detail_icon_action(
                                            Button::new(SharedString::from(format!(
                                                "agent-auth-clear-{key}"
                                            )))
                                            .small()
                                            .outline()
                                            .icon(if clearing {
                                                IconName::Undo2
                                            } else {
                                                IconName::Delete
                                            })
                                            .disabled(pending)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_agent_auth_clear(clear_key.clone(), cx)
                                            })),
                                            clear_label,
                                        ))
                                    }),
                            )
                            .child(input_element),
                    );
                }

                if let Some(link) = method.credential_link {
                    let open_link = link.clone();
                    method_content = method_content.child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .child(management_detail_icon_action(
                                Button::new(SharedString::from(format!(
                                    "agent-auth-credential-link-{}",
                                    method.id
                                )))
                                .small()
                                .link()
                                .icon(IconName::ExternalLink)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.open_agent_auth_link(open_link.clone(), cx)
                                    },
                                )),
                                management_locale_text("Get credentials", "获取凭据", "取得憑證"),
                            )),
                    );
                }
                content = content.child(method_content);
            }
        }

        if let Some((_, terminal)) = self.agent_auth_terminal_surface.as_ref() {
            let terminal_status = match self.agent_auth_terminal_state {
                Some(AgentAuthTerminalState::Running) => {
                    management_locale_text("Sign-in in progress", "正在登录", "正在登入")
                }
                Some(AgentAuthTerminalState::Succeeded) => {
                    management_locale_text("Sign-in completed", "登录已完成", "登入已完成")
                }
                Some(AgentAuthTerminalState::Failed) => {
                    management_locale_text("Sign-in failed", "登录失败", "登入失敗")
                }
                None => management_locale_text("Sign-in terminal", "登录终端", "登入終端"),
            };
            content =
                content
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(management_status_badge(terminal_status.to_string(), cx))
                            .child(management_detail_icon_action(
                                Button::new("agent-auth-terminal-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_agent_auth_terminal(cx)
                                    })),
                                management_locale_text(
                                    "Close sign-in terminal",
                                    "关闭登录终端",
                                    "關閉登入終端",
                                ),
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(management_locale_text(
                                "Agent CLI authentication session",
                                "Agent CLI 认证会话",
                                "Agent CLI 驗證工作階段",
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(320.0))
                            .min_h(px(220.0))
                            .overflow_hidden()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(terminal.clone()),
                    );
        }

        management_card_with_icon(
            management_locale_text("Authentication", "登录与认证", "登入與驗證"),
            management_locale_text(
                "Agent sign-in and model provider credentials",
                "Agent 登录与模型供应商凭据",
                "Agent 登入與模型供應商憑證",
            ),
            "icons/vibex/shield-alert.svg",
            content.into_any_element(),
            cx,
        )
    }

    fn render_agent_install_loading(
        &self,
        _agent: &AgentSnapshotEntry,
        upgrading: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = if upgrading {
            management_locale_text("Upgrading Agent", "正在升级 Agent", "正在升級 Agent")
        } else {
            management_locale_text("Downloading Agent", "正在下载 Agent", "正在下載 Agent")
        };
        let description = if upgrading {
            management_locale_text(
                "The verified runtime is being prepared before the new version is enabled.",
                "正在校验并准备新运行时，完成后才会启用新版本。",
                "正在驗證並準備新執行環境，完成後才會啟用新版本。",
            )
        } else {
            management_locale_text(
                "The Agent is downloaded and verified before it becomes available.",
                "Agent 会先下载并校验，完成后才会正式可用。",
                "Agent 會先下載並驗證，完成後才會正式可用。",
            )
        };
        management_card_with_icon(
            title,
            management_locale_text(
                "Verified ACP Registry runtime",
                "ACP Registry 托管运行时",
                "ACP Registry 託管執行環境",
            ),
            "icons/vibex/download.svg",
            v_flex()
                .w_full()
                .items_center()
                .justify_center()
                .gap_3()
                .py(px(44.0))
                .child(
                    Button::new("management-agent-install-loading")
                        .large()
                        .ghost()
                        .loading(true)
                        .disabled(true)
                        .label(title),
                )
                .child(
                    div()
                        .max_w(px(460.0))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(description),
                )
                .into_any_element(),
            cx,
        )
    }

    fn render_agent_installation_card(
        &mut self,
        agent: &AgentSnapshotEntry,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &agent.managed_install;
        if !state.managed {
            let content = v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(div().text_sm().font_medium().child(management_locale_text(
                            "External CLI",
                            "外部 CLI",
                            "外部 CLI",
                        )))
                        .child(management_status_badge(
                            management_locale_text("Managed by user", "由用户管理", "由使用者管理")
                                .to_string(),
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(management_locale_text(
                            "Vibex uses the CLI already available on PATH for this Agent.",
                            "Vibex 使用此 Agent 在 PATH 中已有的 CLI。",
                            "Vibex 使用此 Agent 在 PATH 中已有的 CLI。",
                        )),
                );
            return management_card_with_icon(
                management_locale_text("Agent installation", "Agent 安装", "Agent 安裝"),
                management_locale_text(
                    "This Agent is not distributed through the verified ACP Registry.",
                    "此 Agent 暂未提供可校验的 ACP Registry 分发包。",
                    "此 Agent 暫未提供可驗證的 ACP Registry 分發包。",
                ),
                "icons/vibex/download.svg",
                content.into_any_element(),
                cx,
            );
        }

        let id = agent.id.as_str().to_string();
        let mutation = self.agent_mutations.get(&id);
        let healthy_installation = state.has_usable_installation();
        let needs_install = !healthy_installation
            && matches!(
                state.status,
                vibex_core::AgentManagedInstallStatus::NotInstalled
                    | vibex_core::AgentManagedInstallStatus::Failed
            );
        let checking = healthy_installation
            && matches!(
                mutation,
                Some(ManagementMutation::AgentUpdateCheck(active_id)) if active_id == &id
            );
        let upgrading = matches!(
            mutation,
            Some(ManagementMutation::AgentInstall(active_id)) if active_id == &id
        );
        let uninstalling = matches!(
            mutation,
            Some(ManagementMutation::AgentUninstall(active_id)) if active_id == &id
        );
        let update_available =
            state.status == vibex_core::AgentManagedInstallStatus::UpdateAvailable;
        let mut content = v_flex().w_full().gap_2();
        content = content.child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .child(management_status_badge(
                    management_managed_install_status_label(state).to_string(),
                    cx,
                ))
                .when_some(state.installed_version.clone(), |row, version| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("v{version}")),
                    )
                }),
        );
        if let Some(version) = state.available_version.as_deref()
            && state.installed_version.as_deref() != Some(version)
        {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}: v{version}",
                        management_locale_text("Available", "可用版本", "可用版本")
                    )),
            );
        }
        if let Some(error) = state.last_error_message.as_deref() {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.to_string()),
            );
        }
        let pending = mutation.is_some();
        let mut actions = h_flex().w_full().justify_end().gap_2();
        if needs_install {
            actions = actions.child(management_detail_icon_action(
                Button::new(SharedString::from(format!("management-agent-install-{id}")))
                    .small()
                    .primary()
                    .icon(IconName::ArrowDown)
                    .loading(upgrading)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.install_managed_agent(id.clone(), false, cx)
                    })),
                management_locale_text("Install", "安装", "安裝"),
            ));
        } else if healthy_installation {
            actions = actions.child(management_detail_icon_action(
                Button::new(SharedString::from(format!("management-agent-check-{id}")))
                    .small()
                    .outline()
                    .icon(IconName::Search)
                    .loading(checking)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.check_managed_agent_update(id.clone(), cx)
                    })),
                management_locale_text("Check for updates", "检查更新", "檢查更新"),
            ));
        }
        if update_available && healthy_installation {
            actions = actions.child(management_detail_icon_action(
                Button::new(SharedString::from(format!("management-agent-upgrade-{id}")))
                    .small()
                    .primary()
                    .icon(IconName::ArrowUp)
                    .loading(upgrading)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.install_managed_agent(id.clone(), true, cx)
                    })),
                management_locale_text("Upgrade", "升级", "升級"),
            ));
        }
        if agent.added || state.installed_version.is_some() {
            let uninstall_label = agent.label.clone();
            actions = actions.child(management_detail_icon_action(
                Button::new(SharedString::from(format!(
                    "management-agent-uninstall-{id}"
                )))
                .small()
                .danger()
                .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                .loading(uninstalling)
                .disabled(pending)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.confirm_managed_delete(
                            ManagedDeleteTarget::Agent {
                                id: id.clone(),
                                label: uninstall_label.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                })),
                management_locale_text("Uninstall", "卸载", "解除安裝"),
            ));
        }
        content = content.child(actions);
        management_card_with_icon(
            management_locale_text("Agent installation", "Agent 安装", "Agent 安裝"),
            management_locale_text(
                "Vibex-managed runtime",
                "Vibex 托管运行时",
                "Vibex 託管執行環境",
            ),
            "icons/vibex/download.svg",
            content.into_any_element(),
            cx,
        )
    }

    fn render_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let copy = management_copy();
        let selected_agent = self
            .snapshot
            .agents
            .iter()
            .find(|agent| self.selected_agent_id.as_deref() == Some(agent.id.as_str()));
        let Some(selected_agent) = selected_agent.cloned() else {
            return detail_empty_state(copy.no_agents, copy.no_agents_description, cx);
        };
        let agent_header = management_agent_detail_header(&selected_agent, cx);
        let selected_id = selected_agent.id.as_str().to_string();
        if matches!(
            self.agent_mutations.get(&selected_id),
            Some(ManagementMutation::AgentInstall(active_id)) if active_id == &selected_id
        ) {
            return v_flex()
                .w_full()
                .gap_4()
                .child(agent_header)
                .child(self.render_agent_install_loading(
                    &selected_agent,
                    matches!(
                        selected_agent.managed_install.status,
                        vibex_core::AgentManagedInstallStatus::UpdateAvailable
                            | vibex_core::AgentManagedInstallStatus::Upgrading
                    ),
                    cx,
                ))
                .into_any_element();
        }
        if !selected_agent.added {
            if selected_agent.managed_install.managed {
                return v_flex()
                    .w_full()
                    .gap_4()
                    .child(agent_header)
                    .child(self.render_agent_installation_card(&selected_agent, window, cx))
                    .into_any_element();
            }
            return detail_empty_state(copy.no_agents, copy.no_agents_description, cx);
        }
        let selected_agent_id = selected_agent.id.as_str().to_string();
        if !self.model_provider_agent_ids.contains(&selected_agent_id) {
            return v_flex()
                .w_full()
                .min_w_0()
                .gap_4()
                .child(agent_header)
                .child(self.render_agent_installation_card(&selected_agent, window, cx))
                .child(self.render_agent_authentication(window, None, cx))
                .into_any_element();
        }
        let mut profiles = self
            .snapshot
            .profiles
            .iter()
            .filter(|profile| profile.agent_id == selected_agent_id)
            .cloned()
            .collect::<Vec<_>>();
        profiles.sort_by_key(|profile| {
            (
                self.provider_display_order
                    .get(profile.id.as_str())
                    .is_none(),
                self.provider_display_order
                    .get(profile.id.as_str())
                    .copied()
                    .unwrap_or(i64::MAX),
            )
        });
        let selected_profile_id = self
            .selected_management_provider_profile()
            .map(|profile| profile.id.as_str().to_string());
        let pending =
            self.mutation.is_some() || self.agent_mutations.contains_key(&selected_agent_id);
        let native_importing = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderPreview(action)) if action == "native-import"
        );
        let cc_switch_import_candidate_count = self.native_import_preview.as_ref().map(|preview| {
            pending_cc_switch_import_item_ids(preview, &self.provider_profiles, &selected_agent.id)
                .len()
        });
        let mut profile_rows = v_flex().w_full().gap_2();
        for profile in profiles.clone() {
            let id = profile.id.clone();
            let hover_group: SharedString = format!("provider-row-hover-{id}").into();
            let select_id = id.clone();
            let agent_id = profile.agent_id.clone();
            let edit_profile = profile.clone();
            let test_id = id.clone();
            let test_agent = agent_id.clone();
            let duplicate_id = id.clone();
            let default_id = id.clone();
            let default_agent = agent_id.clone();
            let delete_id = id.clone();
            let delete_label = profile.display_name.clone();
            let profile_state = self
                .agent_profile_states
                .iter()
                .find(|state| state.agent_id == agent_id && state.profile_id == id);
            let is_default = profile_state.is_some_and(|state| state.is_default);
            let active = selected_profile_id.as_deref() == Some(id.as_str());
            let address = profile
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(management_unconfigured_label);
            let testing = matches!(
                &self.mutation,
                Some(ManagementMutation::ProviderProbe(active_id)) if active_id == &id
            );
            let deleting = matches!(
                &self.mutation,
                Some(ManagementMutation::ProfileDelete(active_id)) if active_id == &id
            );
            let selectable = h_flex()
                .id(SharedString::from(format!("provider-select-{id}")))
                .flex_1()
                .min_w_0()
                .min_h(px(72.0))
                .items_center()
                .gap_3()
                .cursor_pointer()
                .px_3()
                .py(px(12.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_provider_profile(select_id.clone(), cx);
                }))
                .child(management_profile_glyph(
                    profile.kind,
                    &profile.display_name,
                    is_default,
                    cx,
                ))
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_semibold()
                                        .child(profile.display_name.clone()),
                                )
                                .when(is_default, |header| {
                                    header.child(management_status_badge(
                                        management_locale_text("Default", "默认", "預設")
                                            .to_string(),
                                        cx,
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} · {}",
                                    address,
                                    management_model_count(profile.configured_model_count)
                                )),
                        ),
                );
            let mut actions = h_flex()
                .flex_none()
                .items_center()
                .justify_end()
                .gap_1()
                .pr_3()
                .invisible()
                .group_hover(&hover_group, |style| style.visible());
            if !is_default {
                actions = actions.child(button_with_aria_label(
                    Button::new(SharedString::from(format!("provider-default-{default_id}")))
                        .xsmall()
                        .secondary()
                        .compact()
                        .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                        .icon(IconName::Check)
                        .tooltip(management_set_default_label())
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_default_provider_profile(
                                default_id.clone(),
                                default_agent.clone(),
                                cx,
                            )
                        })),
                    management_set_default_label(),
                ));
            }
            actions = actions
                .child(button_with_aria_label(
                    Button::new(SharedString::from(format!("provider-edit-{id}")))
                        .xsmall()
                        .secondary()
                        .compact()
                        .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                        .icon(
                            Icon::default()
                                .path("icons/vibex/pencil.svg")
                                .size(px(18.0)),
                        )
                        .disabled(pending)
                        .tooltip(management_locale_text(
                            "Edit configuration",
                            "编辑配置",
                            "編輯配置",
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_profile_editor(edit_profile.clone(), window, cx)
                        })),
                    management_locale_text("Edit configuration", "编辑配置", "編輯配置"),
                ))
                .child(button_with_aria_label(
                    Button::new(SharedString::from(format!(
                        "provider-duplicate-{duplicate_id}"
                    )))
                    .xsmall()
                    .secondary()
                    .compact()
                    .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                    .icon(IconName::Copy)
                    .disabled(pending)
                    .tooltip(management_locale_text(
                        "Duplicate configuration",
                        "复制配置",
                        "複製配置",
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.duplicate_provider_profile(duplicate_id.clone(), cx)
                    })),
                    management_locale_text("Duplicate configuration", "复制配置", "複製配置"),
                ))
                .child(button_with_aria_label(
                    Button::new(SharedString::from(format!("provider-test-{test_id}")))
                        .xsmall()
                        .secondary()
                        .compact()
                        .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                        .icon(
                            Icon::default()
                                .path("icons/vibex/activity.svg")
                                .size(px(18.0)),
                        )
                        .loading(testing)
                        .disabled(pending)
                        .tooltip(if testing {
                            management_locale_text("Testing...", "正在测试...", "正在測試...")
                        } else {
                            management_test_label()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.test_provider_profile(test_id.clone(), test_agent.clone(), cx)
                        })),
                    management_test_label(),
                ))
                .child(button_with_aria_label(
                    Button::new(SharedString::from(format!("provider-delete-{delete_id}")))
                        .xsmall()
                        .danger()
                        .compact()
                        .size(px(MANAGEMENT_PROVIDER_ROW_ACTION_SIZE))
                        .icon(IconName::Delete)
                        .loading(deleting)
                        .disabled(pending)
                        .tooltip(management_delete_profile_label())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_managed_delete(
                                ManagedDeleteTarget::Provider {
                                    id: delete_id.clone(),
                                    label: delete_label.clone(),
                                },
                                window,
                                cx,
                            )
                        })),
                    management_delete_profile_label(),
                ));
            let drag_payload = ProviderDisplayOrderDrag {
                agent_id: selected_agent_id.clone(),
                profile_id: id.clone(),
                label: profile.display_name.clone().into(),
            };
            let drag_entity = cx.weak_entity();
            let drag_tooltip = management_locale_text(
                "Drag to reorder providers",
                "拖动调整供应商顺序",
                "拖動調整供應商順序",
            );
            let drag_handle = div()
                .id(SharedString::from(format!("provider-drag-handle-{id}")))
                .flex_none()
                .w(px(36.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_grab()
                .tooltip(move |window, cx| Tooltip::new(drag_tooltip).build(window, cx))
                .child(
                    Icon::default()
                        .path("icons/vibex/grip-vertical.svg")
                        .size(px(18.0))
                        .text_color(cx.theme().muted_foreground.opacity(0.65)),
                )
                .on_drag(drag_payload, move |drag, _, _, cx| {
                    cx.stop_propagation();
                    let _ = drag_entity.update(cx, |this, cx| {
                        this.provider_display_order_drop_target = None;
                        cx.notify();
                    });
                    cx.new(|_| drag.clone())
                });
            let active_drop_after =
                self.provider_display_order_drop_target
                    .as_ref()
                    .and_then(|target| {
                        (cx.has_active_drag() && target.profile_id == id).then_some(target.after)
                    });
            let move_agent_id = selected_agent_id.clone();
            let move_profile_id = id.clone();
            let drop_agent_id = selected_agent_id.clone();
            let drop_profile_id = id.clone();
            profile_rows = profile_rows.child(
                div()
                    .id(SharedString::from(format!("provider-row-{id}")))
                    .group(hover_group)
                    .relative()
                    .flex()
                    .items_center()
                    .w_full()
                    .min_w_0()
                    .min_h(px(72.0))
                    .overflow_hidden()
                    .rounded(px(14.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(if active { 0.0 } else { 0.65 }))
                    .bg(if active {
                        cx.theme().primary.opacity(0.08)
                    } else {
                        cx.theme().background.opacity(0.90)
                    })
                    .hover(|style| {
                        style.bg(if active {
                            cx.theme().primary.opacity(0.08)
                        } else {
                            cx.theme().accent
                        })
                    })
                    .when_some(active_drop_after, |this, after| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .h(px(2.0))
                                .bg(cx.theme().drag_border)
                                .map(|line| if after { line.bottom_0() } else { line.top_0() }),
                        )
                    })
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<ProviderDisplayOrderDrag>, _, cx| {
                            let drag = event.drag(cx);
                            let next = (event.bounds.contains(&event.event.position)
                                && drag.agent_id == move_agent_id
                                && drag.profile_id != move_profile_id)
                                .then_some(ProviderDisplayOrderDropTarget {
                                    profile_id: move_profile_id.clone(),
                                    after: event.event.position.y >= event.bounds.center().y,
                                });
                            if this.provider_display_order_drop_target != next {
                                this.provider_display_order_drop_target = next;
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(
                        cx.listener(move |this, drag: &ProviderDisplayOrderDrag, _, cx| {
                            let target = this.provider_display_order_drop_target.take();
                            if let Some(target) = target.filter(|target| {
                                target.profile_id == drop_profile_id
                                    && drag.agent_id == drop_agent_id
                            }) {
                                this.reorder_provider_profiles(
                                    &drag.profile_id,
                                    &target.profile_id,
                                    target.after,
                                    cx,
                                );
                            } else {
                                cx.notify();
                            }
                        }),
                    )
                    .child(drag_handle)
                    .child(selectable)
                    .child(actions),
            );
        }

        let installation = self.render_agent_installation_card(&selected_agent, window, cx);
        let provider_configuration = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.75))
            .pt_4()
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(copy.provider_configuration),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(management_locale_text(
                                "Credentials used to connect this Agent to model services",
                                "用于连接此 Agent 与模型服务的凭据配置",
                                "用於連接此 Agent 與模型服務的憑證設定",
                            )),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(management_profile_count(profiles.len())),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .child(management_detail_icon_action(
                                Button::new("provider-import-existing")
                                    .small()
                                    .secondary()
                                    .icon(Icon::default().path("icons/vibex/import.svg"))
                                    .loading(native_importing)
                                    .disabled(
                                        pending
                                            || cc_switch_import_candidate_count.is_none()
                                            || cc_switch_import_candidate_count == Some(0),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let agent_id = this.selected_agent_id.clone();
                                        this.preview_native_import(true, agent_id, cx)
                                    })),
                                copy.import_configuration,
                            ))
                            .child(management_detail_icon_action(
                                Button::new("provider-add-configuration")
                                    .small()
                                    .primary()
                                    .icon(IconName::Plus)
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_profile_creator(window, cx);
                                    })),
                                copy.add_configuration,
                            )),
                    ),
            )
            .child(if profiles.is_empty() {
                compact_empty_state(copy.no_profiles, copy.no_profiles_description, cx)
            } else {
                profile_rows.into_any_element()
            });
        let authentication = self.render_agent_authentication(
            window,
            Some(provider_configuration.into_any_element()),
            cx,
        );

        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(agent_header)
            .child(installation)
            .child(authentication)
            .into_any_element()
    }

    fn render_mcp(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let servers = self
            .snapshot
            .mcp_servers
            .iter()
            .filter(|server| self.selected_mcp_id.as_deref() == Some(server.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let matrix_agents = self
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.added)
            .cloned()
            .collect::<Vec<_>>();
        let mut rows = v_flex().gap_2();
        for server in servers.clone() {
            let id = server.id.as_str().to_string();
            let validate_id = id.clone();
            let delete_id = id.clone();
            let delete_label = server.display_name.clone();
            let enabled = server.status == vibex_core::McpServerStatus::Enabled;
            let source = format!(
                "{} · {}",
                server.transport_kind,
                server
                    .command
                    .as_deref()
                    .or(server.url.as_deref())
                    .map(str::to_string)
                    .unwrap_or_else(management_unconfigured_label)
            );
            let description = server
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| management_mcp_description().to_string());
            let validating = matches!(
                &self.mutation,
                Some(ManagementMutation::McpAction(action))
                    if action == &format!("validate:{id}")
            );
            let deleting = matches!(
                &self.mutation,
                Some(ManagementMutation::McpAction(action))
                    if action == &format!("delete:{id}")
            );
            let validation = self
                .mcp_validation
                .as_ref()
                .filter(|(resource_id, _, _)| resource_id == &id)
                .map(|(_, message, failed)| (message.clone(), *failed));
            let mut agent_matrix_rows = v_flex().w_full().gap_1();
            for agent in matrix_agents.clone() {
                let agent_id = agent.id.clone();
                let server_id = id.clone();
                let matrix_entry = server
                    .agent_matrix
                    .iter()
                    .find(|entry| entry.agent_id == agent_id);
                let enabled_for_agent = matrix_entry.is_some_and(|entry| entry.enabled);
                let matrix_source = matrix_entry
                    .map(|entry| management_resource_matrix_source_label(entry.source_kind))
                    .unwrap_or("manual");
                agent_matrix_rows = agent_matrix_rows.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.60))
                        .px_2()
                        .py_2()
                        .child(management_agent_glyph(
                            agent.id.as_str(),
                            &agent.label,
                            false,
                            cx,
                        ))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_1()
                                .child(div().truncate().text_sm().font_medium().child(agent.label))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} · {matrix_source}", agent.id)),
                                ),
                        )
                        .child(
                            Switch::new(SharedString::from(format!(
                                "mcp-agent-matrix-{server_id}-{}",
                                agent_id.as_str()
                            )))
                            .small()
                            .checked(enabled_for_agent)
                            .disabled(pending)
                            .on_click(cx.listener(
                                move |this, checked, _, cx| {
                                    this.set_mcp_agent_matrix(
                                        server_id.clone(),
                                        agent_id.clone(),
                                        *checked,
                                        cx,
                                    )
                                },
                            )),
                        ),
                );
            }
            rows = rows.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.75))
                    .bg(cx.theme().background.opacity(0.75))
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(server.display_name.clone()),
                            )
                            .child(management_status_badge(
                                management_enabled_label(enabled).to_string(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(description),
                    )
                    .child(key_value(
                        management_locale_text("Source", "来源", "來源"),
                        &source,
                        cx,
                    ))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "mcp-validate-{validate_id}"
                                )))
                                .small()
                                .outline()
                                .label(management_validate_label())
                                .loading(validating)
                                .disabled(pending)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.validate_mcp_server(validate_id.clone(), cx)
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!("mcp-delete-{delete_id}")))
                                    .small()
                                    .danger()
                                    .icon(IconName::Delete)
                                    .loading(deleting)
                                    .tooltip(management_delete_mcp_label())
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_managed_delete(
                                            ManagedDeleteTarget::Mcp {
                                                id: delete_id.clone(),
                                                label: delete_label.clone(),
                                            },
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .when_some(validation, |card, (message, failed)| {
                        card.child(status_line(
                            if failed {
                                locale::localize_error_message(&message)
                            } else {
                                locale::localize_ui_message(&message)
                            },
                            failed,
                            cx,
                        ))
                    })
                    .when(!matrix_agents.is_empty(), |card| {
                        card.child(
                            v_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .child(management_agent_enablement_label()),
                                )
                                .child(agent_matrix_rows),
                        )
                    }),
            );
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(if servers.is_empty() {
                compact_empty_state(
                    management_no_mcp_selection_title(),
                    management_no_mcp_selection_description(),
                    cx,
                )
            } else {
                rows.into_any_element()
            })
            .into_any_element()
    }

    fn render_skills(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let skills = self
            .snapshot
            .skills
            .iter()
            .filter(|skill| self.selected_skill_id.as_deref() == Some(skill.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let matrix_agents = self
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.added)
            .cloned()
            .collect::<Vec<_>>();
        let mut rows = v_flex().gap_2();
        for skill in skills.clone() {
            let id = skill.id.as_str().to_string();
            let validate_id = id.clone();
            let delete_id = id.clone();
            let delete_label = skill.display_name.clone();
            let enabled = skill.status == vibex_core::SkillStatus::Enabled;
            let source = skill
                .source_uri
                .as_deref()
                .map(str::trim)
                .filter(|source| !source.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| management_skill_source_kind_label(skill.source_kind).into());
            let description = skill
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| management_skills_description().to_string());
            let validating = matches!(
                &self.mutation,
                Some(ManagementMutation::SkillAction(action))
                    if action == &format!("validate:{id}")
            );
            let deleting = matches!(
                &self.mutation,
                Some(ManagementMutation::SkillAction(action))
                    if action == &format!("delete:{id}")
            );
            let validation = self
                .skill_validation
                .as_ref()
                .filter(|(resource_id, _, _)| resource_id == &id)
                .map(|(_, message, failed)| (message.clone(), *failed));
            let mut agent_matrix_rows = v_flex().w_full().gap_1();
            for agent in matrix_agents.clone() {
                let agent_id = agent.id.clone();
                let skill_id = id.clone();
                let matrix_entry = skill
                    .agent_matrix
                    .iter()
                    .find(|entry| entry.agent_id == agent_id);
                let enabled_for_agent = matrix_entry.is_some_and(|entry| entry.enabled);
                let matrix_source = matrix_entry
                    .map(|entry| management_resource_matrix_source_label(entry.source_kind))
                    .unwrap_or("manual");
                agent_matrix_rows = agent_matrix_rows.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.60))
                        .px_2()
                        .py_2()
                        .child(management_agent_glyph(
                            agent.id.as_str(),
                            &agent.label,
                            false,
                            cx,
                        ))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_1()
                                .child(div().truncate().text_sm().font_medium().child(agent.label))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} · {matrix_source}", agent.id)),
                                ),
                        )
                        .child(
                            Switch::new(SharedString::from(format!(
                                "skill-agent-matrix-{skill_id}-{}",
                                agent_id.as_str()
                            )))
                            .small()
                            .checked(enabled_for_agent)
                            .disabled(pending)
                            .on_click(cx.listener(
                                move |this, checked, _, cx| {
                                    this.set_skill_agent_matrix(
                                        skill_id.clone(),
                                        agent_id.clone(),
                                        *checked,
                                        cx,
                                    )
                                },
                            )),
                        ),
                );
            }
            rows =
                rows.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.75))
                        .bg(cx.theme().background.opacity(0.75))
                        .p_3()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .font_semibold()
                                        .child(skill.display_name.clone()),
                                )
                                .child(management_status_badge(
                                    management_enabled_label(enabled).to_string(),
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        )
                        .child(key_value(
                            management_locale_text("Source", "来源", "來源"),
                            &source,
                            cx,
                        ))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "skill-validate-{validate_id}"
                                    )))
                                    .small()
                                    .outline()
                                    .label(management_validate_label())
                                    .loading(validating)
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.validate_skill(validate_id.clone(), cx)
                                    })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "skill-delete-{delete_id}"
                                    )))
                                    .small()
                                    .danger()
                                    .icon(IconName::Delete)
                                    .loading(deleting)
                                    .tooltip(management_delete_skill_label())
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_managed_delete(
                                            ManagedDeleteTarget::Skill {
                                                id: delete_id.clone(),
                                                label: delete_label.clone(),
                                            },
                                            window,
                                            cx,
                                        )
                                    })),
                                ),
                        )
                        .when_some(validation, |card, (message, failed)| {
                            card.child(status_line(
                                if failed {
                                    locale::localize_error_message(&message)
                                } else {
                                    locale::localize_ui_message(&message)
                                },
                                failed,
                                cx,
                            ))
                        })
                        .when(!matrix_agents.is_empty(), |card| {
                            card.child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_semibold()
                                            .child(management_agent_enablement_label()),
                                    )
                                    .child(agent_matrix_rows),
                            )
                        }),
                );
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(if skills.is_empty() {
                compact_empty_state(
                    management_no_skill_selection_title(),
                    management_no_skill_selection_description(),
                    cx,
                )
            } else {
                rows.into_any_element()
            })
            .into_any_element()
    }

    fn render_prompts_hooks(&mut self, extra_wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let prompts = self.snapshot.prompts.clone();
        let hooks = self.snapshot.hooks.clone();
        let pending = self.mutation.is_some();
        let mut prompt_rows = v_flex().gap_3();
        for prompt in prompts.clone() {
            let id = prompt.id.as_str().to_string();
            let delete_id = id.clone();
            let delete_label = prompt.display_name.clone();
            let deleting = matches!(
                &self.mutation,
                Some(ManagementMutation::PromptAction(action))
                    if action == &format!("delete:{id}")
            );
            prompt_rows = prompt_rows.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .font_medium()
                                    .child(prompt.display_name),
                            )
                            .child(management_status_badge(
                                management_prompt_status_label(prompt.status).to_string(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                management_prompt_kind_key(prompt.kind),
                                management_prompt_scope_key(prompt.scope_kind)
                            )),
                    )
                    .child(
                        h_flex().child(
                            Button::new(SharedString::from(format!("prompt-delete-{delete_id}")))
                                .small()
                                .danger()
                                .icon(IconName::Delete)
                                .label(management_locale_text("Delete", "删除", "刪除"))
                                .loading(deleting)
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_managed_delete(
                                        ManagedDeleteTarget::Prompt {
                                            id: delete_id.clone(),
                                            label: delete_label.clone(),
                                        },
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                    ),
            );
        }
        let mut hook_rows = v_flex().gap_3();
        for hook in hooks.clone() {
            let id = hook.id.as_str().to_string();
            let delete_id = id.clone();
            let delete_label = hook.display_name.clone();
            let deleting = matches!(
                &self.mutation,
                Some(ManagementMutation::HookAction(action))
                    if action == &format!("delete:{id}")
            );
            hook_rows = hook_rows.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .font_medium()
                                    .child(hook.display_name),
                            )
                            .child(management_status_badge(
                                management_hook_status_label(hook.status).to_string(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                hook.provider_kind,
                                management_hook_event_kind_key(hook.event_kind)
                            )),
                    )
                    .child(
                        h_flex().child(
                            Button::new(SharedString::from(format!("hook-delete-{delete_id}")))
                                .small()
                                .danger()
                                .icon(IconName::Delete)
                                .label(management_locale_text("Delete", "删除", "刪除"))
                                .loading(deleting)
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_managed_delete(
                                        ManagedDeleteTarget::Hook {
                                            id: delete_id.clone(),
                                            label: delete_label.clone(),
                                        },
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                    ),
            );
        }
        let prompt_card = management_card(
            management_locale_text("Prompts", "提示词", "提示詞"),
            management_locale_text(
                "Reusable prompts shared with supported Agent runtimes.",
                "管理可供支持的 Agent 运行时复用的提示词。",
                "管理可供支援的 Agent 執行階段重用的提示詞。",
            ),
            v_flex()
                .w_full()
                .gap_3()
                .child(management_input_field(
                    management_locale_text("Name", "名称", "名稱"),
                    &self.prompt_name,
                    false,
                    cx,
                ))
                .child(management_input_field(
                    management_locale_text("Prompt body", "提示词内容", "提示詞內容"),
                    &self.prompt_body,
                    false,
                    cx,
                ))
                .child(
                    Button::new("prompt-create")
                        .small()
                        .primary()
                        .label(management_locale_text(
                            "Create Prompt",
                            "创建提示词",
                            "建立提示詞",
                        ))
                        .loading(matches!(
                            self.mutation,
                            Some(ManagementMutation::PromptAction(ref action)) if action == "create"
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.create_prompt(cx))),
                )
                .child(if prompts.is_empty() {
                    compact_empty_state(
                        management_locale_text("No Prompts", "暂无提示词", "暫無提示詞"),
                        management_locale_text(
                            "Create a reusable Prompt",
                            "创建一个可复用提示词",
                            "建立一個可重用提示詞",
                        ),
                        cx,
                    )
                } else {
                    prompt_rows.into_any_element()
                })
                .into_any_element(),
            cx,
        );
        let hook_card = management_card(
            "Hooks",
            management_locale_text(
                "Preview managed Hook installation before enabling it.",
                "启用前预览托管 Hook 的安装内容。",
                "啟用前預覽託管 Hook 的安裝內容。",
            ),
            v_flex()
                .w_full()
                .gap_3()
                .child(management_input_field(
                    management_locale_text("Name", "名称", "名稱"),
                    &self.hook_name,
                    false,
                    cx,
                ))
                .child(management_input_field(
                    management_locale_text(
                        "Hook command preview",
                        "Hook 命令预览",
                        "Hook 命令預覽",
                    ),
                    &self.hook_command,
                    false,
                    cx,
                ))
                .child(
                    Button::new("hook-create")
                        .small()
                        .primary()
                        .label(management_locale_text(
                            "Create Hook",
                            "创建 Hook",
                            "建立 Hook",
                        ))
                        .loading(matches!(
                            self.mutation,
                            Some(ManagementMutation::HookAction(ref action)) if action == "create"
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.create_hook(cx))),
                )
                .child(if hooks.is_empty() {
                    compact_empty_state(
                        management_locale_text("No Hooks", "暂无 Hooks", "暫無 Hooks"),
                        management_locale_text(
                            "Create a preview-only managed Hook",
                            "创建一个仅预览的托管 Hook",
                            "建立一個僅預覽的託管 Hook",
                        ),
                        cx,
                    )
                } else {
                    hook_rows.into_any_element()
                })
                .into_any_element(),
            cx,
        );
        if extra_wide {
            h_flex()
                .w_full()
                .items_start()
                .gap_4()
                .child(div().min_w_0().flex_1().child(prompt_card))
                .child(div().min_w_0().flex_1().child(hook_card))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .gap_4()
                .child(prompt_card)
                .child(hook_card)
                .into_any_element()
        }
    }

    fn selected_management_provider_profile(&self) -> Option<vibex_core::ProviderProfile> {
        let agent_id = self.selected_agent_id.as_deref()?;
        let profile_ids = self
            .provider_profiles
            .iter()
            .filter(|profile| {
                profile.agent_id.as_str() == agent_id && profile.deleted_at_ms.is_none()
            })
            .map(|profile| profile.id.as_str().to_string())
            .collect::<Vec<_>>();
        let preferred_id = self
            .selected_provider_profile_id
            .as_ref()
            .filter(|id| profile_ids.contains(id))
            .cloned()
            .or_else(|| {
                self.agent_profile_states
                    .iter()
                    .find(|state| state.agent_id == agent_id && state.is_default)
                    .map(|state| state.profile_id.clone())
            })
            .or_else(|| profile_ids.first().cloned())?;
        self.provider_profiles
            .iter()
            .find(|profile| profile.id.as_str() == preferred_id)
            .cloned()
    }

    fn prepare_acp_config_editor(
        &mut self,
        profile_id: String,
        config: vibex_core::AcpProviderConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_acp_profile_id.as_deref() == Some(profile_id.as_str()) {
            return;
        }
        self.acp_command.update(cx, |state, cx| {
            state.set_value(config.command.clone(), window, cx)
        });
        self.acp_args.update(cx, |state, cx| {
            state.set_value(config.args.join(" "), window, cx)
        });
        self.acp_cwd.update(cx, |state, cx| {
            state.set_value(config.cwd_template.clone().unwrap_or_default(), window, cx)
        });
        self.selected_acp_profile_id = Some(profile_id);
        self.acp_config_draft = Some(config);
        self.navigation
            .mark_dirty(ManagementSection::Advanced, false);
    }

    fn set_acp_process_strategy(
        &mut self,
        strategy: vibex_core::AcpProcessStrategy,
        cx: &mut Context<Self>,
    ) {
        if let Some(config) = self.acp_config_draft.as_mut() {
            config.process_strategy = strategy;
            self.navigation
                .mark_dirty(ManagementSection::Advanced, true);
            cx.notify();
        }
    }

    fn set_acp_terminal_tools(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(config) = self.acp_config_draft.as_mut() {
            config.terminal_tools = enabled;
            self.navigation
                .mark_dirty(ManagementSection::Advanced, true);
            cx.notify();
        }
    }

    fn set_acp_terminal_auth(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(config) = self.acp_config_draft.as_mut() {
            config.terminal_auth = enabled;
            self.navigation
                .mark_dirty(ManagementSection::Advanced, true);
            cx.notify();
        }
    }

    fn save_acp_config(&mut self, cx: &mut Context<Self>) {
        let (Some(profile_id), Some(mut config), Some(runtime)) = (
            self.selected_acp_profile_id.clone(),
            self.acp_config_draft.clone(),
            self.runtime.clone(),
        ) else {
            return;
        };
        let Ok(provider_profile_id) = vibex_core::ProviderProfileId::parse(profile_id.clone())
        else {
            return;
        };
        let command = self.acp_command.read(cx).value().trim().to_string();
        if command.is_empty() {
            self.error = Some(
                management_error_text(
                    "ACP command is required",
                    "ACP 命令不能为空",
                    "ACP 命令不能為空",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let args = self.acp_args.read(cx).value().to_string();
        let cwd_template = self.acp_cwd.read(cx).value().to_string();
        config = acp_config_with_editor_fields(config, command, &args, &cwd_template);
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::AcpConfig(profile_id), cx, async move {
            let profile = runtime
                .management()
                .providers()
                .management()
                .update_acp_profile_config(vibex_core::AcpProviderProfileUpdateRequest {
                    provider_profile_id,
                    config,
                })?;
            let message = match active_locale {
                ResolvedLocale::En => {
                    format!("Updated ACP configuration {}", profile.display_name)
                }
                ResolvedLocale::ZhCn => {
                    format!("已更新 ACP 配置 {}", profile.display_name)
                }
                ResolvedLocale::ZhTw => {
                    format!("已更新 ACP 配置 {}", profile.display_name)
                }
            };
            Ok(message)
        });
    }

    fn render_acp_config_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected_profile = self.selected_management_provider_profile();
        let Some(profile) = selected_profile else {
            return management_card(
                management_locale_text("ACP configuration", "ACP 配置", "ACP 配置"),
                management_locale_text(
                    "Select an Agent provider configuration first.",
                    "请先选择 Agent 的供应商配置。",
                    "請先選擇 Agent 的供應商配置。",
                ),
                detail_empty_state(
                    management_locale_text(
                        "No provider configuration selected",
                        "未选择供应商配置",
                        "未選擇供應商配置",
                    ),
                    management_locale_text(
                        "Add or select a configuration in the Agent section.",
                        "请在 Agent 页面添加或选择配置。",
                        "請在 Agent 頁面新增或選擇配置。",
                    ),
                    cx,
                ),
                cx,
            );
        };
        if profile.kind != ProviderKind::Acp {
            return management_card(
                management_locale_text("ACP configuration", "ACP 配置", "ACP 配置"),
                management_locale_text(
                    "ACP runtime settings apply only to ACP profiles.",
                    "ACP 运行时设置仅适用于 ACP 配置。",
                    "ACP 執行階段設定僅適用於 ACP 配置。",
                ),
                detail_empty_state(
                    management_locale_text(
                        "Select an ACP profile",
                        "请选择 ACP 配置",
                        "請選擇 ACP 配置",
                    ),
                    management_locale_text(
                        "The currently selected profile is not an ACP profile.",
                        "当前选择的供应商配置不是 ACP 类型。",
                        "目前選擇的供應商配置不是 ACP 類型。",
                    ),
                    cx,
                ),
                cx,
            );
        }
        let Some(config) = self
            .acp_configs
            .iter()
            .find(|(profile_id, _)| profile_id == profile.id.as_str())
            .map(|(_, config)| config.clone())
        else {
            return management_card(
                management_locale_text("ACP configuration", "ACP 配置", "ACP 配置"),
                management_locale_text(
                    "ACP configuration could not be loaded.",
                    "无法加载 ACP 配置。",
                    "無法載入 ACP 配置。",
                ),
                compact_empty_state(
                    management_locale_text("Configuration unavailable", "配置不可用", "配置不可用"),
                    management_locale_text(
                        "Refresh the config center and try again.",
                        "请刷新配置中心后重试。",
                        "請重新整理配置中心後重試。",
                    ),
                    cx,
                ),
                cx,
            );
        };
        self.prepare_acp_config_editor(profile.id.as_str().to_string(), config, window, cx);
        let draft = self.acp_config_draft.clone().unwrap_or_else(|| {
            self.acp_configs
                .iter()
                .find(|(profile_id, _)| profile_id == profile.id.as_str())
                .map(|(_, config)| config.clone())
                .expect("ACP config was checked above")
        });
        let pending = self.mutation.is_some();
        let saving = matches!(self.mutation, Some(ManagementMutation::AcpConfig(_)));
        let capability = self
            .capability_summaries
            .iter()
            .find(|summary| summary.profile.id == profile.id)
            .cloned();
        let process_pool_fallback = capability.as_ref().and_then(|capability| {
            capability
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.key == "processPoolFallback")
                .map(|diagnostic| diagnostic.value.clone())
        });
        let process_strategy = draft.process_strategy;
        let terminal_tools = draft.terminal_tools;
        let terminal_auth = draft.terminal_auth;
        let env_summary = draft
            .env
            .iter()
            .map(|entry| format!("{}: {}", entry.key, entry.redacted_hint))
            .collect::<Vec<_>>();
        management_card(
            management_locale_text("ACP configuration", "ACP 配置", "ACP 配置"),
            management_locale_text(
                "Command, process strategy, terminal tools, and detected capabilities.",
                "管理命令、进程策略、终端工具和已探测能力。",
                "管理命令、程序策略、終端工具與已探測能力。",
            ),
            v_flex()
                .w_full()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(div().text_sm().font_medium().child(profile.display_name))
                        .when_some(capability.clone(), |row, capability| {
                            row.child(management_status_badge(
                                if capability.fresh {
                                    management_locale_text("Fresh", "能力最新", "能力最新")
                                } else {
                                    management_locale_text("Stale", "能力过期", "能力過期")
                                }
                                .to_string(),
                                cx,
                            ))
                        }),
                )
                .child(management_input_field(
                    management_locale_text("Command", "命令", "命令"),
                    &self.acp_command,
                    false,
                    cx,
                ))
                .child(management_input_field(
                    management_locale_text("Arguments", "参数", "參數"),
                    &self.acp_args,
                    false,
                    cx,
                ))
                .child(management_input_field(
                    management_locale_text(
                        "Working directory template",
                        "工作目录模板",
                        "工作目錄範本",
                    ),
                    &self.acp_cwd,
                    false,
                    cx,
                ))
                .child(
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child(management_locale_text(
                                    "Process strategy",
                                    "进程策略",
                                    "程序策略",
                                )),
                        )
                        .child(
                            h_flex().w_full().flex_wrap().gap_1().children(
                                [
                                    (
                                        vibex_core::AcpProcessStrategy::PerSession,
                                        management_locale_text(
                                            "Per session",
                                            "每会话",
                                            "每工作階段",
                                        ),
                                    ),
                                    (
                                        vibex_core::AcpProcessStrategy::PerProfilePool,
                                        management_locale_text(
                                            "Profile pool",
                                            "配置进程池",
                                            "配置程序池",
                                        ),
                                    ),
                                    (
                                        vibex_core::AcpProcessStrategy::Auto,
                                        management_locale_text("Automatic", "自动", "自動"),
                                    ),
                                ]
                                .into_iter()
                                .map(|(strategy, label)| {
                                    Button::new(SharedString::from(format!(
                                        "acp-strategy-{strategy:?}"
                                    )))
                                    .small()
                                    .ghost()
                                    .selected(process_strategy == strategy)
                                    .label(label)
                                    .disabled(pending)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_acp_process_strategy(strategy, cx)
                                    }))
                                }),
                            ),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            h_flex()
                                .min_w(px(220.0))
                                .flex_1()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(cx.theme().border.opacity(0.70))
                                .bg(cx.theme().muted.opacity(0.20))
                                .px_3()
                                .py_2()
                                .child(div().text_sm().font_medium().child(management_locale_text(
                                    "Terminal tools",
                                    "终端工具",
                                    "終端工具",
                                )))
                                .child(
                                    Switch::new("acp-terminal-tools")
                                        .small()
                                        .checked(terminal_tools)
                                        .disabled(pending)
                                        .on_click(cx.listener(|this, enabled, _, cx| {
                                            this.set_acp_terminal_tools(*enabled, cx)
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .min_w(px(220.0))
                                .flex_1()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(cx.theme().border.opacity(0.70))
                                .bg(cx.theme().muted.opacity(0.20))
                                .px_3()
                                .py_2()
                                .child(div().text_sm().font_medium().child(management_locale_text(
                                    "Terminal authorization",
                                    "终端授权",
                                    "終端授權",
                                )))
                                .child(
                                    Switch::new("acp-terminal-auth")
                                        .small()
                                        .checked(terminal_auth)
                                        .disabled(pending)
                                        .on_click(cx.listener(|this, enabled, _, cx| {
                                            this.set_acp_terminal_auth(*enabled, cx)
                                        })),
                                ),
                        ),
                )
                .child(key_value(
                    management_locale_text("Models", "模型", "模型"),
                    &management_join_list(&draft.models),
                    cx,
                ))
                .child(key_value(
                    management_locale_text("Modes", "模式", "模式"),
                    &management_join_list(&draft.modes),
                    cx,
                ))
                .child(key_value(
                    management_locale_text("Features", "能力", "能力"),
                    &management_join_list(&draft.features),
                    cx,
                ))
                .child(key_value(
                    management_locale_text("Disabled tools", "已禁用工具", "已停用工具"),
                    &management_join_list(&draft.disabled_tools),
                    cx,
                ))
                .child(key_value(
                    management_locale_text("Environment", "环境变量", "環境變數"),
                    &management_join_list(&env_summary),
                    cx,
                ))
                .when_some(capability, |card, capability| {
                    card.child(key_value(
                        management_locale_text("Capability source", "能力来源", "能力來源"),
                        &format!("{:?} · {}", capability.status, capability.capability_source),
                        cx,
                    ))
                })
                .when_some(process_pool_fallback, |card, fallback| {
                    card.child(status_line(
                        match locale::current_locale() {
                            ResolvedLocale::En => format!("Process pool fallback: {fallback}"),
                            ResolvedLocale::ZhCn => format!("进程池回退：{fallback}"),
                            ResolvedLocale::ZhTw => format!("程序池回退：{fallback}"),
                        },
                        false,
                        cx,
                    ))
                })
                .child(
                    Button::new("acp-config-save")
                        .small()
                        .primary()
                        .label(management_locale_text(
                            "Update ACP config",
                            "更新 ACP 配置",
                            "更新 ACP 配置",
                        ))
                        .loading(saving)
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.save_acp_config(cx))),
                )
                .into_any_element(),
            cx,
        )
    }

    fn render_native_export_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let selected_profile = self.selected_management_provider_profile();
        let selected_profile_id = selected_profile
            .as_ref()
            .map(|profile| profile.id.as_str().to_string());
        let pending = self.mutation.is_some();
        let previewing = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderPreview(action))
                if action.starts_with("native-export:")
        );
        let source = self.native_export_source;
        let mode = self.native_export_mode;

        let source_controls = h_flex()
            .w_full()
            .gap_1()
            .child(
                Button::new("native-export-source-codex")
                    .small()
                    .ghost()
                    .flex_1()
                    .selected(source == vibex_core::ProviderNativeExportSource::Codex)
                    .label("Codex")
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.native_export_source
                            != vibex_core::ProviderNativeExportSource::Codex
                        {
                            this.native_export_source =
                                vibex_core::ProviderNativeExportSource::Codex;
                            this.native_export_preview = None;
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("native-export-source-claude")
                    .small()
                    .ghost()
                    .flex_1()
                    .selected(source == vibex_core::ProviderNativeExportSource::Claude)
                    .label("Claude")
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.native_export_source
                            != vibex_core::ProviderNativeExportSource::Claude
                        {
                            this.native_export_source =
                                vibex_core::ProviderNativeExportSource::Claude;
                            this.native_export_preview = None;
                            cx.notify();
                        }
                    })),
            );

        let mut mode_controls = h_flex().w_full().flex_wrap().gap_1();
        for (candidate, label) in [
            (
                vibex_core::ProviderNativeExportMode::ProviderProfile,
                management_locale_text("Provider profile", "供应商配置", "供應商配置"),
            ),
            (
                vibex_core::ProviderNativeExportMode::Combined,
                management_locale_text("Combined", "组合", "組合"),
            ),
            (vibex_core::ProviderNativeExportMode::Mcp, "MCP"),
            (
                vibex_core::ProviderNativeExportMode::Skills,
                management_locale_text("Skills", "技能", "技能"),
            ),
            (
                vibex_core::ProviderNativeExportMode::Prompts,
                management_locale_text("Prompts", "提示词", "提示詞"),
            ),
        ] {
            mode_controls = mode_controls.child(
                Button::new(SharedString::from(format!(
                    "native-export-mode-{candidate:?}"
                )))
                .small()
                .ghost()
                .selected(mode == candidate)
                .label(label)
                .disabled(pending)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.native_export_mode != candidate {
                        this.native_export_mode = candidate;
                        this.native_export_preview = None;
                        cx.notify();
                    }
                })),
            );
        }

        let active_export_preview = self.native_export_preview.clone().filter(|preview| {
            native_export_preview_matches(preview, selected_profile_id.as_deref(), source, mode)
        });
        let mut preview_rows = v_flex().w_full().gap_2();
        if let Some(preview) = active_export_preview.clone() {
            preview_rows = preview_rows.child(stat_line(
                management_locale_text("Preview", "预览", "預覽"),
                format!("{:?} · {:?}", preview.source, preview.mode),
                cx,
            ));
            if preview.files.is_empty() {
                preview_rows = preview_rows.child(compact_empty_state(
                    management_locale_text("No changes", "没有变更", "沒有變更"),
                    management_locale_text(
                        "The selected export does not change any native file.",
                        "当前导出不会修改任何原生配置文件。",
                        "目前匯出不會修改任何原生配置檔案。",
                    ),
                    cx,
                ));
            }
            for file in preview.files {
                let preview_text = if !file.redacted_diff.trim().is_empty() {
                    file.redacted_diff
                } else if !file.redacted_after.trim().is_empty() {
                    file.redacted_after
                } else {
                    management_locale_text("No changes", "没有变更", "沒有變更").to_string()
                };
                preview_rows = preview_rows.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.72))
                        .p_3()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .font_medium()
                                        .child(file.target_path),
                                )
                                .child(management_status_badge(format!("{:?}", file.status), cx)),
                        )
                        .child(
                            div()
                                .w_full()
                                .max_h(px(128.0))
                                .overflow_y_scrollbar()
                                .rounded(px(6.0))
                                .bg(cx.theme().muted.opacity(0.45))
                                .p_2()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .font_weight(code_font_weight(cx))
                                .child(preview_text),
                        ),
                );
            }
        } else {
            preview_rows = preview_rows.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "No export preview yet",
                        "暂无导出预览",
                        "暫無匯出預覽",
                    )),
            );
        }

        let preview_export_id = active_export_preview
            .as_ref()
            .map(|preview| preview.export_id.as_str().to_string());
        let apply_export_id = preview_export_id.clone();
        let rollback_export_id = preview_export_id.clone();
        let preview_profile_id = selected_profile_id.clone();
        let actions = h_flex()
            .w_full()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("native-export-preview-current")
                    .small()
                    .primary()
                    .icon(IconName::ArrowDown)
                    .label(management_locale_text(
                        "Preview export",
                        "预览导出",
                        "預覽匯出",
                    ))
                    .loading(previewing)
                    .disabled(pending || preview_profile_id.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(profile_id) = preview_profile_id.clone() {
                            this.preview_native_export(profile_id, cx);
                        }
                    })),
            )
            .child(
                Button::new("native-export-apply-current")
                    .small()
                    .secondary()
                    .label(management_locale_text("Apply", "应用", "套用"))
                    .disabled(pending || apply_export_id.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(export_id) = apply_export_id.clone() {
                            this.apply_native_export(export_id, cx);
                        }
                    })),
            )
            .child(
                Button::new("native-export-rollback-current")
                    .small()
                    .outline()
                    .label(management_locale_text("Rollback", "回滚", "回復"))
                    .disabled(pending || rollback_export_id.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(export_id) = rollback_export_id.clone() {
                            this.rollback_native_export(export_id, cx);
                        }
                    })),
            );

        let mut history_rows = v_flex().w_full().gap_2();
        let records = selected_profile_id
            .as_deref()
            .map(|profile_id| {
                self.native_exports
                    .iter()
                    .filter(|record| record.provider_profile_id.as_str() == profile_id)
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if records.is_empty() {
            history_rows = history_rows.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "No persisted export records",
                        "暂无已保存的导出记录",
                        "暫無已儲存的匯出記錄",
                    )),
            );
        }
        for record in records {
            let rollback_id = record.export_id.as_str().to_string();
            history_rows = history_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.70))
                    .p_2()
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(div().text_xs().child(format!(
                                "{:?} · {:?} · {}",
                                record.source, record.mode, record.status
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match locale::current_locale() {
                                        ResolvedLocale::En => format!(
                                            "{} file(s) · {} blocked",
                                            record.file_count, record.blocked_count
                                        ),
                                        ResolvedLocale::ZhCn => format!(
                                            "{} 个文件 · {} 个被阻止",
                                            record.file_count, record.blocked_count
                                        ),
                                        ResolvedLocale::ZhTw => format!(
                                            "{} 個檔案 · {} 個被阻止",
                                            record.file_count, record.blocked_count
                                        ),
                                    }),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "native-export-history-rollback-{rollback_id}"
                        )))
                        .small()
                        .outline()
                        .label(management_locale_text(
                            "Rollback record",
                            "回滚记录",
                            "回復記錄",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.rollback_native_export(rollback_id.clone(), cx)
                        })),
                    ),
            );
        }

        management_card(
            management_locale_text("Native export", "原生配置导出", "原生配置匯出"),
            management_locale_text(
                "Preview redacted file changes before writing Agent-native configuration.",
                "写入 Agent 原生配置前先检查脱敏文件变更。",
                "寫入 Agent 原生配置前先檢查遮罩檔案變更。",
            ),
            v_flex()
                .w_full()
                .gap_3()
                .when_some(selected_profile, |content, profile| {
                    content.child(stat_line(
                        management_locale_text("Profile", "配置", "配置"),
                        profile.display_name,
                        cx,
                    ))
                })
                .child(source_controls)
                .child(mode_controls)
                .child(actions)
                .child(preview_rows)
                .child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().border.opacity(0.70))
                        .text_sm()
                        .font_semibold()
                        .child(management_locale_text(
                            "Export history",
                            "导出历史",
                            "匯出歷史",
                        )),
                )
                .child(history_rows)
                .into_any_element(),
            cx,
        )
    }

    fn render_advanced(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let extra_wide = f32::from(window.viewport_size().width) >= 1536.0;
        let acp_card = self.render_acp_config_card(window, cx);
        let native_export_card = self.render_native_export_card(cx);
        let prompts_hooks = self.render_prompts_hooks(extra_wide, cx);
        let pending = self.mutation.is_some();
        let mut health_rows = v_flex().w_full().gap_1();
        for summary in self.health_summaries.clone() {
            health_rows = health_rows.child(stat_line(
                summary.profile.display_name,
                management_health_status_label(Some(summary.overall_status)).to_string(),
                cx,
            ));
        }
        if self.health_summaries.is_empty() {
            health_rows = health_rows.child(compact_empty_state(
                management_locale_text("No health results", "暂无健康检查结果", "暫無健康檢查結果"),
                management_locale_text(
                    "Run a health probe to populate this list.",
                    "运行健康检查后将在此显示结果。",
                    "執行健康檢查後將在此顯示結果。",
                ),
                cx,
            ));
        }
        let health_card = management_card(
            management_locale_text("Health", "健康检查", "健康檢查"),
            management_locale_text(
                "Provider connectivity and authentication status.",
                "供应商连接和认证状态。",
                "供應商連線與驗證狀態。",
            ),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("advanced-health-probe")
                        .small()
                        .secondary()
                        .label(management_health_probe_label())
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.run_provider_health_probe(cx))),
                )
                .child(health_rows)
                .into_any_element(),
            cx,
        );
        let mut capability_rows = v_flex().w_full().gap_1();
        for summary in self.capability_summaries.clone() {
            capability_rows = capability_rows.child(stat_line(
                summary.profile.display_name,
                format!("{:?} · {}", summary.status, summary.capability_source),
                cx,
            ));
        }
        if self.capability_summaries.is_empty() {
            capability_rows = capability_rows.child(compact_empty_state(
                management_locale_text(
                    "No capability results",
                    "暂无能力检查结果",
                    "暫無能力檢查結果",
                ),
                management_locale_text(
                    "Run capability detection to populate this list.",
                    "运行能力检查后将在此显示结果。",
                    "執行能力檢查後將在此顯示結果。",
                ),
                cx,
            ));
        }
        let capability_card = management_card(
            management_locale_text("Capabilities", "能力", "能力"),
            management_locale_text(
                "Detected runtime features and their source.",
                "已探测的运行时能力及其来源。",
                "已探測的執行階段能力及其來源。",
            ),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("advanced-capability-probe")
                        .small()
                        .secondary()
                        .label(management_capability_probe_label())
                        .disabled(pending)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.run_provider_capability_probe(cx)),
                        ),
                )
                .child(capability_rows)
                .into_any_element(),
            cx,
        );
        let selected_agent_id = self.selected_agent_id.clone();
        let mut compatibility_rows = v_flex().w_full().gap_1();
        let mut compatibility_count = 0usize;
        for profile in self.provider_profiles.iter().filter(|profile| {
            selected_agent_id.as_deref() == Some(profile.agent_id.as_str())
                && profile.deleted_at_ms.is_none()
        }) {
            compatibility_count = compatibility_count.saturating_add(1);
            compatibility_rows = compatibility_rows.child(stat_line(
                profile.display_name.clone(),
                format!(
                    "{} · {}",
                    profile.kind,
                    management_profile_status_label(profile.status)
                ),
                cx,
            ));
        }
        if compatibility_count == 0 {
            compatibility_rows = compatibility_rows.child(compact_empty_state(
                management_locale_text("No provider profiles", "暂无供应商配置", "暫無供應商配置"),
                management_locale_text(
                    "Add a configuration from the Agent section.",
                    "请从 Agent 页面添加配置。",
                    "請從 Agent 頁面新增配置。",
                ),
                cx,
            ));
        }
        let compatibility_card = management_card(
            management_locale_text("Compatibility profiles", "兼容配置", "相容配置"),
            management_locale_text(
                "Profiles available to the selected Agent.",
                "当前 Agent 可用的供应商配置。",
                "目前 Agent 可用的供應商配置。",
            ),
            compatibility_rows.into_any_element(),
            cx,
        );
        let top_cards = if extra_wide {
            h_flex()
                .w_full()
                .items_start()
                .gap_4()
                .child(div().min_w_0().flex_1().child(acp_card))
                .child(div().min_w_0().flex_1().child(native_export_card))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .gap_4()
                .child(acp_card)
                .child(native_export_card)
                .into_any_element()
        };
        let status_cards = if extra_wide {
            h_flex()
                .w_full()
                .items_start()
                .gap_4()
                .child(div().min_w_0().flex_1().child(health_card))
                .child(div().min_w_0().flex_1().child(capability_card))
                .child(div().min_w_0().flex_1().child(compatibility_card))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .gap_4()
                .child(health_card)
                .child(capability_card)
                .child(compatibility_card)
                .into_any_element()
        };
        section_layout(
            management_locale_text("Advanced", "高级", "進階"),
            management_locale_text(
                "ACP runtime configuration, native compatibility, probes, Prompts, and Hooks",
                "管理 ACP 运行时配置、原生兼容、检查、提示词和 Hooks",
                "管理 ACP 執行階段配置、原生相容、檢查、提示詞與 Hooks",
            ),
            cx,
        )
        .child(status_line(
            management_locale_text(
                "Advanced changes affect native Agent compatibility. Review export previews before applying them.",
                "高级设置会影响原生 Agent 兼容性；应用前请先检查导出预览。",
                "進階設定會影響原生 Agent 相容性；套用前請先檢查匯出預覽。",
            )
            .to_string(),
            false,
            cx,
        ))
        .child(top_cards)
        .child(status_cards)
        .child(prompts_hooks)
        .into_any_element()
    }

    fn clear_scheduled_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_scheduled_task_id = None;
        self.scheduled_title
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.scheduled_prompt
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.navigation
            .mark_dirty(ManagementSection::Scheduled, false);
        cx.notify();
    }

    fn select_scheduled_task(
        &mut self,
        task: ScheduledTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_scheduled_task_id = Some(task.id.as_str().to_string());
        self.scheduled_title
            .update(cx, |state, cx| state.set_value(task.title, window, cx));
        self.scheduled_prompt
            .update(cx, |state, cx| state.set_value(task.prompt, window, cx));
        self.navigation
            .mark_dirty(ManagementSection::Scheduled, false);
        cx.notify();
    }

    fn render_scheduled(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tasks = self.snapshot.scheduled.clone();
        let selected_task = self
            .selected_scheduled_task_id
            .as_deref()
            .and_then(|id| tasks.iter().find(|task| task.id.as_str() == id).cloned());
        let editing = selected_task.is_some();
        let pending = self.mutation.is_some();
        section_layout(
            management_locale_text("Scheduled", "定时任务", "排程任務"),
            management_locale_text(
                "Interval/one-shot tasks, recovery, attention, audit, and guarded delete",
                "管理周期/单次任务、恢复、待处理项、审计及安全删除",
                "管理週期/單次任務、復原、待處理項、稽核及安全刪除",
            ),
            cx,
        )
        .child(
            v_flex()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .pb_3()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .child(management_locale_text(
                            if editing {
                                "Edit interval task"
                            } else {
                                "Create interval task"
                            },
                            if editing {
                                "编辑周期任务"
                            } else {
                                "创建周期任务"
                            },
                            if editing {
                                "編輯週期任務"
                            } else {
                                "建立週期任務"
                            },
                        )),
                )
                .child(management_input_field(
                    management_locale_text("Task title", "任务标题", "任務標題"),
                    &self.scheduled_title,
                    false,
                    cx,
                ))
                .child(management_input_field(
                    management_locale_text("Prompt", "提示词", "提示詞"),
                    &self.scheduled_prompt,
                    false,
                    cx,
                ))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .child(if let Some(task) = selected_task {
                            Button::new("scheduled-save-selected")
                                .small()
                                .primary()
                                .label(management_locale_text(
                                    "Save changes",
                                    "保存修改",
                                    "儲存修改",
                                ))
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_selected_scheduled_task(task.clone(), cx)
                                }))
                        } else {
                            Button::new("scheduled-create")
                                .small()
                                .primary()
                                .label(management_locale_text(
                                    "Create task",
                                    "创建任务",
                                    "建立任務",
                                ))
                                .disabled(pending)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.create_scheduled_task(cx)),
                                )
                        })
                        .when(editing, |actions| {
                            actions.child(
                                Button::new("scheduled-new")
                                    .small()
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label(management_locale_text(
                                        "New task",
                                        "新建任务",
                                        "新增任務",
                                    ))
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.clear_scheduled_editor(window, cx)
                                    })),
                            )
                        }),
                ),
        )
        .child(stat_line(
            management_locale_text("Tasks", "任务", "任務"),
            tasks.len().to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Run history", "运行历史", "執行歷史"),
            self.scheduled_runs.len().to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Attention", "待处理", "待處理"),
            self.scheduled_attention.len().to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Audit", "审计", "稽核"),
            self.scheduled_audit.len().to_string(),
            cx,
        ))
        .child(if tasks.is_empty() {
            empty_state(
                management_locale_text("No scheduled tasks", "暂无定时任务", "暫無排程任務"),
                management_locale_text(
                    "Create a task above or refresh the list",
                    "在上方创建任务或刷新列表",
                    "在上方建立任務或重新整理清單",
                ),
                cx,
            )
        } else {
            v_flex()
                .gap_1()
                .children(
                    tasks
                        .into_iter()
                        .map(|task| self.render_scheduled_row(task, cx)),
                )
                .into_any_element()
        })
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text(
                    "Run history",
                    "运行历史",
                    "執行歷史",
                )),
        )
        .child(if self.scheduled_runs.is_empty() {
            empty_state(
                management_locale_text("No scheduled runs", "暂无运行记录", "暫無執行記錄"),
                management_locale_text(
                    "The scheduler will add authoritative run records",
                    "调度器会在任务运行后写入权威记录",
                    "排程器會在任務執行後寫入權威記錄",
                ),
                cx,
            )
        } else {
            v_flex()
                .gap_1()
                .children(self.scheduled_runs.clone().into_iter().map(|run| {
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .justify_between()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .py_1()
                        .child(
                            div().text_xs().child(format!(
                                "{} · {:?} · {:?}",
                                run.id, run.status, run.trigger
                            )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(run.error_code.unwrap_or_else(|| {
                                    management_locale_text("No error", "无错误", "無錯誤").into()
                                })),
                        )
                }))
                .into_any_element()
        })
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text(
                    "Attention and recovery",
                    "待处理与恢复",
                    "待處理與復原",
                )),
        )
        .child(if self.scheduled_attention.is_empty() {
            empty_state(
                management_locale_text("No attention items", "暂无待处理项", "暫無待處理項"),
                management_locale_text(
                    "Failed, permission, and stale-run recovery will appear here",
                    "失败、权限和过期运行的恢复项会显示在这里",
                    "失敗、權限與過期執行的復原項目會顯示在這裡",
                ),
                cx,
            )
        } else {
            v_flex()
                .gap_1()
                .children(self.scheduled_attention.clone().into_iter().map(|item| {
                    div().text_xs().child(format!(
                        "{} · {:?} · {}",
                        item.task_title,
                        item.attention_kind,
                        item.error_code.unwrap_or_else(|| {
                            management_locale_text("Retry available", "可重试", "可重試").into()
                        })
                    ))
                }))
                .into_any_element()
        })
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text("Audit", "审计", "稽核")),
        )
        .child(if self.scheduled_audit.is_empty() {
            empty_state(
                management_locale_text("No audit records", "暂无审计记录", "暫無稽核記錄"),
                management_locale_text(
                    "Scheduler transitions remain durable and queryable",
                    "调度器状态变化会持久保存并可查询",
                    "排程器狀態變化會持久儲存並可查詢",
                ),
                cx,
            )
        } else {
            v_flex()
                .gap_1()
                .children(self.scheduled_audit.clone().into_iter().map(|record| {
                    div()
                        .text_xs()
                        .child(format!("{} · {:?}", record.task_id, record.outcome))
                }))
                .into_any_element()
        })
        .into_any_element()
    }

    fn update_selected_scheduled_task(&mut self, task: ScheduledTask, cx: &mut Context<Self>) {
        let title = self.scheduled_title.read(cx).value().trim().to_string();
        let prompt = self.scheduled_prompt.read(cx).value().trim().to_string();
        if title.is_empty() || prompt.is_empty() {
            self.error = Some(
                management_error_text(
                    "Scheduled task title and prompt are required",
                    "定时任务标题和提示词不能为空",
                    "排程任務標題與提示詞不能為空",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let mutation = ManagementMutation::ScheduledUpdate(task.id.as_str().to_string());
        let active_locale = locale::current_locale();
        self.begin_simple_task(mutation, cx, async move {
            runtime
                .management()
                .scheduled()
                .update(vibex_core::ScheduledTaskUpdateRequest {
                    id: task.id,
                    title: Some(title),
                    prompt: Some(prompt),
                    project_id: None,
                    clear_project_id: false,
                    workspace_id: None,
                    clear_workspace_id: false,
                    workspace_root: None,
                    workspace_mode: None,
                    provider_kind: None,
                    provider_profile_id: None,
                    clear_provider_profile_id: false,
                    schedule: None,
                    safety: None,
                    next_run_at_ms: None,
                    clear_next_run_at_ms: false,
                })
                .map(|task| match active_locale {
                    ResolvedLocale::En => format!("Updated scheduled task {}", task.title),
                    ResolvedLocale::ZhCn => format!("已更新定时任务 {}", task.title),
                    ResolvedLocale::ZhTw => format!("已更新排程任務 {}", task.title),
                })
        });
    }

    fn create_scheduled_task(&mut self, cx: &mut Context<Self>) {
        let title = self.scheduled_title.read(cx).value().trim().to_string();
        let prompt = self.scheduled_prompt.read(cx).value().trim().to_string();
        if title.is_empty() || prompt.is_empty() {
            self.error = Some(
                management_error_text(
                    "Scheduled task title and prompt are required",
                    "定时任务标题和提示词不能为空",
                    "排程任務標題與提示詞不能為空",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let Some((workspace_root, workspace_mode)) = self.current_workspace_context() else {
            self.error = Some(
                management_error_text(
                    "Open a workspace session before creating a scheduled task",
                    "请先打开一个工作区会话，再创建定时任务",
                    "請先開啟一個工作區工作階段，再建立排程任務",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        let now = unix_timestamp_ms();
        let request = ScheduledTaskCreateRequest {
            title,
            prompt,
            project_id: None,
            workspace_id: None,
            workspace_root,
            workspace_mode,
            provider_kind: ProviderKind::Acp,
            provider_profile_id: None,
            schedule: ScheduledTaskSchedule::Interval(ScheduledTaskIntervalSchedule {
                every_seconds: 3_600,
                start_at_ms: now,
                end_at_ms: None,
            }),
            safety: None,
            next_run_at_ms: Some(now),
        };
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some(
                management_error_text(
                    "Management runtime is not connected",
                    "配置中心运行时未连接",
                    "配置中心執行階段未連線",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        if self.mutation.is_some() {
            self.notice = Some(
                management_locale_text(
                    "Another management action is still pending",
                    "另一项配置操作仍在处理中",
                    "另一項配置操作仍在處理中",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let active_locale = locale::current_locale();
        self.mutation = Some(ManagementMutation::ScheduledCreate);
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime.management().scheduled().create(request)
        });
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(Ok(task)) => {
                        this.navigation
                            .mark_dirty(ManagementSection::Scheduled, false);
                        this.selected_scheduled_task_id = Some(task.id.as_str().to_string());
                        this.notice = Some(match active_locale {
                            ResolvedLocale::En => {
                                format!("Created scheduled task {}", task.title)
                            }
                            ResolvedLocale::ZhCn => {
                                format!("已创建定时任务 {}", task.title)
                            }
                            ResolvedLocale::ZhTw => {
                                format!("已建立排程任務 {}", task.title)
                            }
                        });
                        this.refresh(cx);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {error}",
                            management_error_text(
                                "Scheduled task creation failed",
                                "定时任务创建失败",
                                "排程任務建立失敗",
                            )
                        ));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn render_scheduled_row(&mut self, task: ScheduledTask, cx: &mut Context<Self>) -> AnyElement {
        let id = task.id.as_str().to_string();
        let selected = self.selected_scheduled_task_id.as_deref() == Some(id.as_str());
        let select_task = task.clone();
        let task_title = task.title.clone();
        let task_schedule = format!("{:?}", task.schedule);
        let active = task.status == vibex_core::ScheduledTaskStatus::Active;
        let any_pending = self.mutation.is_some();
        let active_locale = locale::current_locale();
        let mutation_pending = self.mutation.as_ref().is_some_and(|mutation| matches!(mutation, ManagementMutation::ScheduledPause(value) | ManagementMutation::ScheduledResume(value) | ManagementMutation::ScheduledDelete(value) if value == &id));
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded(px(8.0))
            .border_1()
            .border_color(if selected {
                cx.theme().ring.opacity(0.60)
            } else {
                cx.theme().border.opacity(0.70)
            })
            .bg(if selected {
                cx.theme().accent.opacity(0.35)
            } else {
                cx.theme().background.opacity(0.70)
            })
            .px_2()
            .py_2()
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        Button::new(SharedString::from(format!("scheduled-select-{id}")))
                            .small()
                            .ghost()
                            .w_full()
                            .min_w_0()
                            .h(px(28.0))
                            .justify_start()
                            .px_1()
                            .selected(selected)
                            .label(task_title)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_scheduled_task(select_task.clone(), window, cx)
                            })),
                    )
                    .child(
                        div()
                            .pl_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                if active {
                                    management_locale_text("Active", "运行中", "執行中")
                                } else {
                                    management_locale_text("Paused", "已暂停", "已暫停")
                                },
                                task_schedule
                            )),
                    ),
            )
            .child({
                let toggle_id = id.clone();
                let delete_id = id.clone();
                h_flex()
                    .gap_1()
                    .child({
                        let run_id = id.clone();
                        Button::new(SharedString::from(format!("scheduled-run-{run_id}")))
                            .small()
                            .outline()
                            .label(management_locale_text("Run", "运行", "執行"))
                            .disabled(any_pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let (Some(runtime), Ok(task_id)) =
                                    (this.runtime.clone(), ScheduledTaskId::parse(run_id.clone()))
                                {
                                    this.begin_simple_task(
                                        ManagementMutation::ScheduledRun(run_id.clone()),
                                        cx,
                                        async move {
                                            let claimed = management_locale_text_for(
                                                active_locale,
                                                "Scheduled run claimed",
                                                "定时任务运行已领取",
                                                "排程任務執行已領取",
                                            )
                                            .to_string();
                                            runtime
                                                .management()
                                                .scheduled()
                                                .claim_due(&task_id, unix_timestamp_ms())?
                                                .map(|_| claimed)
                                                .ok_or_else(|| {
                                                    VibexError::conflict(
                                                        "scheduled_task_not_due",
                                                        "task is not due yet",
                                                    )
                                                })
                                        },
                                    );
                                }
                            }))
                    })
                    .child(
                        Button::new(SharedString::from(format!("scheduled-toggle-{toggle_id}")))
                            .small()
                            .ghost()
                            .label(if active {
                                management_locale_text("Pause", "暂停", "暫停")
                            } else {
                                management_locale_text("Resume", "恢复", "繼續")
                            })
                            .loading(mutation_pending)
                            .disabled(any_pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let parsed = ScheduledTaskId::parse(toggle_id.clone()).ok();
                                if let Some(parsed) = parsed {
                                    let runtime = this.runtime.clone();
                                    if let Some(runtime) = runtime {
                                        let mutation = if active {
                                            ManagementMutation::ScheduledPause(
                                                parsed.as_str().into(),
                                            )
                                        } else {
                                            ManagementMutation::ScheduledResume(
                                                parsed.as_str().into(),
                                            )
                                        };
                                        this.begin_simple_task(mutation, cx, async move {
                                            let handle = runtime.management().scheduled();
                                            let task = if active {
                                                handle.pause(&parsed)?
                                            } else {
                                                handle.resume(&parsed)?
                                            };
                                            Ok(task.title.to_string())
                                        });
                                    }
                                }
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("scheduled-delete-{delete_id}")))
                            .small()
                            .danger()
                            .icon(IconName::Delete)
                            .tooltip(management_locale_text(
                                "Delete scheduled task (confirmation required)",
                                "删除定时任务（需要确认）",
                                "刪除排程任務（需要確認）",
                            ))
                            .disabled(any_pending)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_scheduled_delete(delete_id.clone(), window, cx);
                            })),
                    )
            })
            .into_any_element()
    }

    fn confirm_scheduled_delete(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let id = id.clone();
            dialog
                .title(management_locale_text_for(
                    active_locale,
                    "Delete scheduled task?",
                    "删除定时任务？",
                    "刪除排程任務？",
                ))
                .child(management_locale_text_for(
                    active_locale,
                    "This is a durable destructive action. Existing run history remains auditable.",
                    "此删除操作会持久化，现有运行历史仍保留审计记录。",
                    "此刪除操作會持久化，現有執行歷史仍保留稽核記錄。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(gpui_component::dialog::DialogClose::new().child(
                            Button::new("cancel-scheduled-delete").outline().label(
                                management_locale_text_for(active_locale, "Cancel", "取消", "取消"),
                            ),
                        ))
                        .child(gpui_component::dialog::DialogAction::new().child(
                            Button::new("confirm-scheduled-delete").danger().label(
                                management_locale_text_for(active_locale, "Delete", "删除", "刪除"),
                            ),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        if let (Some(runtime), Ok(task_id)) =
                            (this.runtime.clone(), ScheduledTaskId::parse(id.clone()))
                        {
                            this.begin_simple_task(
                                ManagementMutation::ScheduledDelete(id.clone()),
                                cx,
                                async move {
                                    runtime.management().scheduled().delete(&task_id).map(|_| {
                                        management_locale_text_for(
                                            active_locale,
                                            "Scheduled task deleted",
                                            "定时任务已删除",
                                            "排程任務已刪除",
                                        )
                                        .to_string()
                                    })
                                },
                            );
                        }
                    });
                    true
                })
        });
    }

    fn render_automation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let graphs = self.snapshot.graphs.clone();
        let draft = self.graph_draft.clone();
        let issues = draft.validate();
        let pending = self.mutation.is_some();
        let mut graph_rows = v_flex().gap_1();
        for graph in graphs.clone() {
            let id = graph.id.as_str().to_string();
            let select_id = id.clone();
            graph_rows = graph_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .py_2()
                    .child(
                        Button::new(SharedString::from(format!("automation-select-{select_id}")))
                            .small()
                            .ghost()
                            .label(graph.title.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_graph_selection(select_id.clone(), window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(match locale::current_locale() {
                                ResolvedLocale::En => format!(
                                    "{:?} · v{} · {} node(s)",
                                    graph.status,
                                    graph.version,
                                    graph.nodes.len()
                                ),
                                ResolvedLocale::ZhCn => format!(
                                    "{:?} · v{} · {} 个节点",
                                    graph.status,
                                    graph.version,
                                    graph.nodes.len()
                                ),
                                ResolvedLocale::ZhTw => format!(
                                    "{:?} · v{} · {} 個節點",
                                    graph.status,
                                    graph.version,
                                    graph.nodes.len()
                                ),
                            }),
                    ),
            );
        }
        let mut node_rows = v_flex().gap_1();
        for node in self.graph_draft.nodes.clone() {
            let node_id = node.id.clone();
            let selected = self.graph_draft.selected_node_ids.contains(&node.id);
            node_rows = node_rows.child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .py_1()
                    .child(
                        Button::new(SharedString::from(format!("automation-node-{node_id}")))
                            .small()
                            .ghost()
                            .selected(selected)
                            .label(format!("{} · {:?}", node.title, node.kind))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_graph_node(node_id.clone(), cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("({}, {})", node.position.x, node.position.y)),
                    ),
            );
        }
        let mut run_rows = v_flex().gap_1();
        for run in self.automation_runs.clone() {
            let run_id = run.id.as_str().to_string();
            let resume_id = run_id.clone();
            let cancel_id = run_id.clone();
            let can_resume = matches!(
                run.status,
                AutomationRunStatus::WaitingForApproval
                    | AutomationRunStatus::Failed
                    | AutomationRunStatus::Recovered
            );
            let can_cancel = matches!(
                run.status,
                AutomationRunStatus::Queued
                    | AutomationRunStatus::Running
                    | AutomationRunStatus::WaitingForApproval
            );
            let steps = self
                .automation_steps
                .iter()
                .filter(|step| step.run_id == run.id)
                .cloned()
                .collect::<Vec<_>>();
            let mut actions = h_flex().flex_wrap().gap_1();
            if can_resume {
                actions = actions.child(
                    Button::new(SharedString::from(format!("automation-resume-{resume_id}")))
                        .small()
                        .outline()
                        .label(management_locale_text("Resume", "恢复", "繼續"))
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resume_automation_run(resume_id.clone(), cx)
                        })),
                );
            }
            if can_cancel {
                actions = actions.child(
                    Button::new(SharedString::from(format!("automation-cancel-{cancel_id}")))
                        .small()
                        .danger()
                        .label(management_cancel_label())
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cancel_automation_run(cancel_id.clone(), cx)
                        })),
                );
            }
            let step_summary = if steps.is_empty() {
                management_locale_text("No steps loaded", "尚未加载步骤", "尚未載入步驟")
                    .to_string()
            } else {
                steps
                    .iter()
                    .map(|step| format!("{}:{:?}", step.node_id, step.status))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            run_rows = run_rows.child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .py_2()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().text_xs().child(format!(
                                        "{} {} · {:?}",
                                        management_locale_text("Run", "运行", "執行",),
                                        run.id,
                                        run.status
                                    )))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                if let Some(error_code) = run.error_code.as_deref()
                                                {
                                                    let message = run
                                                        .error_message
                                                        .as_deref()
                                                        .unwrap_or("The operation failed");
                                                    format!(
                                                        "{} {}",
                                                        management_locale_text(
                                                            "Error", "错误", "錯誤",
                                                        ),
                                                        locale::localize_error_message(&format!(
                                                            "{error_code}: {message}"
                                                        ))
                                                    )
                                                } else {
                                                    step_summary
                                                },
                                            ),
                                    ),
                            )
                            .child(actions),
                    ),
            );
        }
        let mut detail = v_flex().gap_2();
        detail = detail.child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("automation-zoom-out")
                        .small()
                        .ghost()
                        .icon(IconName::Minus)
                        .tooltip(management_locale_text("Zoom out", "缩小", "縮小"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.graph_draft.zoom_by(-10);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .child(format!("{}%", draft.viewport.zoom_percent)),
                )
                .child(
                    Button::new("automation-zoom-in")
                        .small()
                        .ghost()
                        .icon(IconName::Plus)
                        .tooltip(management_locale_text("Zoom in", "放大", "放大"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.graph_draft.zoom_by(10);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("automation-pan")
                        .small()
                        .ghost()
                        .label(management_locale_text("Pan", "平移", "平移"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.graph_draft.pan_by(24, 0);
                            cx.notify();
                        })),
                ),
        );
        if draft.graph_id.is_none() {
            detail = detail
                .child(empty_state(
                    management_locale_text(
                        "Select a graph",
                        "选择一个自动化图",
                        "選擇一個自動化圖",
                    ),
                    management_locale_text(
                        "Graph drafts remain in memory until explicitly saved",
                        "自动化图草稿会保留在内存中，直到明确保存",
                        "自動化圖草稿會保留在記憶體中，直到明確儲存",
                    ),
                    cx,
                ))
                .child(
                    Button::new("automation-create")
                        .small()
                        .primary()
                        .label(management_locale_text(
                            "Create graph",
                            "创建自动化图",
                            "建立自動化圖",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.create_graph_from_draft(cx))),
                )
                .child(
                    Button::new("automation-add-node-empty")
                        .small()
                        .outline()
                        .label(management_locale_text(
                            "Add first node",
                            "添加首个节点",
                            "新增首個節點",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| this.add_graph_node(cx))),
                );
        } else {
            detail = detail
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(match locale::current_locale() {
                            ResolvedLocale::En => format!("Draft: {}", draft.title),
                            ResolvedLocale::ZhCn => format!("草稿：{}", draft.title),
                            ResolvedLocale::ZhTw => format!("草稿：{}", draft.title),
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(match locale::current_locale() {
                            ResolvedLocale::En => format!(
                                "{} node(s), {} edge(s), base revision {:?}",
                                draft.nodes.len(),
                                draft.edges.len(),
                                draft.base_version
                            ),
                            ResolvedLocale::ZhCn => format!(
                                "{} 个节点、{} 条连线，基础版本 {:?}",
                                draft.nodes.len(),
                                draft.edges.len(),
                                draft.base_version
                            ),
                            ResolvedLocale::ZhTw => format!(
                                "{} 個節點、{} 條連線，基礎版本 {:?}",
                                draft.nodes.len(),
                                draft.edges.len(),
                                draft.base_version
                            ),
                        }),
                )
                .child(stat_line(
                    management_locale_text("Draft state", "草稿状态", "草稿狀態"),
                    if draft.dirty {
                        management_locale_text("Unsaved", "未保存", "未儲存")
                    } else {
                        management_locale_text("Saved", "已保存", "已儲存")
                    },
                    cx,
                ))
                .child(if issues.is_empty() {
                    status_line(
                        management_locale_text(
                            "Graph validation passed",
                            "自动化图验证通过",
                            "自動化圖驗證通過",
                        )
                        .into(),
                        false,
                        cx,
                    )
                } else {
                    status_line(
                        match locale::current_locale() {
                            ResolvedLocale::En => format!(
                                "{} validation issue(s): {}",
                                issues.len(),
                                issues
                                    .iter()
                                    .map(|issue| issue.code)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            ResolvedLocale::ZhCn => format!(
                                "{} 个验证问题：{}",
                                issues.len(),
                                issues
                                    .iter()
                                    .map(|issue| issue.code)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            ResolvedLocale::ZhTw => format!(
                                "{} 個驗證問題：{}",
                                issues.len(),
                                issues
                                    .iter()
                                    .map(|issue| issue.code)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        },
                        true,
                        cx,
                    )
                });
            detail = detail
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(management_locale_text(
                            "Canvas draft",
                            "画布草稿",
                            "畫布草稿",
                        )),
                )
                .child(node_rows)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Button::new("automation-add-node")
                                .small()
                                .outline()
                                .label(management_locale_text("Add node", "添加节点", "新增節點"))
                                .on_click(cx.listener(|this, _, _, cx| this.add_graph_node(cx))),
                        )
                        .child(
                            Button::new("automation-connect-nodes")
                                .small()
                                .outline()
                                .label(management_locale_text(
                                    "Connect selected",
                                    "连接所选节点",
                                    "連接所選節點",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connect_selected_graph_nodes(cx)
                                })),
                        )
                        .child(
                            Button::new("automation-move-left")
                                .small()
                                .ghost()
                                .icon(IconName::ChevronLeft)
                                .tooltip(management_locale_text(
                                    "Move selected node left",
                                    "左移所选节点",
                                    "左移所選節點",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.move_selected_graph_node(-16, 0, cx)
                                })),
                        )
                        .child(
                            Button::new("automation-move-right")
                                .small()
                                .ghost()
                                .icon(IconName::ChevronRight)
                                .tooltip(management_locale_text(
                                    "Move selected node right",
                                    "右移所选节点",
                                    "右移所選節點",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.move_selected_graph_node(16, 0, cx)
                                })),
                        )
                        .child(
                            Button::new("automation-delete-selection")
                                .small()
                                .danger()
                                .icon(IconName::Delete)
                                .tooltip(management_locale_text(
                                    "Delete selected nodes",
                                    "删除所选节点",
                                    "刪除所選節點",
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_selected_graph_nodes(cx)
                                })),
                        ),
                );
            if let Some(graph) = graphs
                .iter()
                .find(|graph| Some(graph.id.as_str()) == draft.graph_id.as_deref())
            {
                let id = graph.id.as_str().to_string();
                let run_id = id.clone();
                let archive_id = id.clone();
                let archive_title = graph.title.clone();
                let paused = graph.status == AutomationGraphStatus::Paused;
                let runnable = graph.status == AutomationGraphStatus::Active;
                detail = detail.child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Button::new("automation-save-definition")
                                .small()
                                .primary()
                                .label(management_locale_text(
                                    "Save definition",
                                    "保存定义",
                                    "儲存定義",
                                ))
                                .disabled(pending)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.save_graph_definition(cx)),
                                ),
                        )
                        .child(
                            Button::new("automation-lifecycle")
                                .small()
                                .outline()
                                .label(if paused {
                                    management_locale_text("Resume", "恢复", "繼續")
                                } else {
                                    management_locale_text("Pause", "暂停", "暫停")
                                })
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let (Some(runtime), Ok(graph_id)) =
                                        (this.runtime.clone(), AutomationGraphId::parse(id.clone()))
                                    {
                                        let mutation = if paused {
                                            ManagementMutation::AutomationResume(id.clone())
                                        } else {
                                            ManagementMutation::AutomationPause(id.clone())
                                        };
                                        this.begin_simple_task(mutation, cx, async move {
                                            let graph = if paused {
                                                runtime
                                                    .management()
                                                    .automation()
                                                    .resume(&graph_id)?
                                            } else {
                                                runtime
                                                    .management()
                                                    .automation()
                                                    .pause(&graph_id)?
                                            };
                                            Ok(graph.title)
                                        });
                                    }
                                })),
                        )
                        .child(
                            Button::new("automation-run")
                                .small()
                                .primary()
                                .label(management_locale_text("Run", "运行", "執行"))
                                .disabled(pending || !runnable)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if runnable
                                        && let (Some(runtime), Ok(graph_id)) = (
                                            this.runtime.clone(),
                                            AutomationGraphId::parse(run_id.clone()),
                                        )
                                    {
                                        let active_locale = locale::current_locale();
                                        this.begin_simple_task(
                                            ManagementMutation::AutomationRun(run_id.clone()),
                                            cx,
                                            async move {
                                                runtime
                                                    .management()
                                                    .automation()
                                                    .start_run(AutomationRunStartRequest {
                                                        graph_id,
                                                        trigger: AutomationRunTrigger::Manual,
                                                        scheduled_task_id: None,
                                                        now_ms: None,
                                                    })
                                                    .await
                                                    .map(|run| match active_locale {
                                                        ResolvedLocale::En => {
                                                            format!("Run {} started", run.id)
                                                        }
                                                        ResolvedLocale::ZhCn => {
                                                            format!("运行 {} 已启动", run.id)
                                                        }
                                                        ResolvedLocale::ZhTw => {
                                                            format!("執行 {} 已啟動", run.id)
                                                        }
                                                    })
                                            },
                                        );
                                    }
                                })),
                        )
                        .child(
                            Button::new("automation-archive")
                                .small()
                                .danger()
                                .label(management_locale_text("Archive", "归档", "封存"))
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_automation_archive(
                                        archive_id.clone(),
                                        archive_title.clone(),
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                );
                if !runnable {
                    detail = detail.child(status_line(
                        match locale::current_locale() {
                            ResolvedLocale::En => {
                                format!("Graph status {:?} cannot start a new run", graph.status)
                            }
                            ResolvedLocale::ZhCn => {
                                format!("自动化图状态 {:?} 无法启动新运行", graph.status)
                            }
                            ResolvedLocale::ZhTw => {
                                format!("自動化圖狀態 {:?} 無法啟動新執行", graph.status)
                            }
                        },
                        false,
                        cx,
                    ));
                }
            }
        }
        section_layout(
            management_locale_text("Automation", "自动化", "自動化"),
            management_locale_text(
                "Draft graph editing, CAS saves, lifecycle, runs, and steps",
                "管理自动化图草稿、CAS 保存、生命周期、运行与步骤",
                "管理自動化圖草稿、CAS 儲存、生命週期、執行與步驟",
            ),
            cx,
        )
        .child(management_input_field(
            management_locale_text("Graph title", "自动化图标题", "自動化圖標題"),
            &self.automation_title,
            false,
            cx,
        ))
        .child(management_input_field(
            management_locale_text("Description", "描述", "描述"),
            &self.automation_description,
            false,
            cx,
        ))
        .child(stat_line(
            management_locale_text("Graphs", "自动化图", "自動化圖"),
            graphs.len().to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Run history", "运行历史", "執行歷史"),
            self.automation_runs.len().to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Visible steps", "可见步骤", "可見步驟"),
            self.automation_steps.len().to_string(),
            cx,
        ))
        .child(graph_rows)
        .child(detail)
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text(
                    "Run history",
                    "运行历史",
                    "執行歷史",
                )),
        )
        .child(if self.automation_runs.is_empty() {
            empty_state(
                management_locale_text("No automation runs", "暂无自动化运行", "暫無自動化執行"),
                management_locale_text(
                    "Run history is populated by the authoritative runner",
                    "权威运行器会在执行后写入运行历史",
                    "權威執行器會在執行後寫入執行歷史",
                ),
                cx,
            )
        } else {
            run_rows.into_any_element()
        })
        .into_any_element()
    }

    fn render_relay(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let mut device_rows = v_flex().w_full().gap_1();
        if self.devices.is_empty() {
            device_rows = device_rows.child(compact_empty_state(
                management_locale_text("No paired devices", "暂无配对设备", "暫無配對裝置"),
                management_locale_text(
                    "Paired devices will appear here with their permissions and status.",
                    "设备配对后会在此显示权限与状态。",
                    "裝置配對後會在此顯示權限與狀態。",
                ),
                cx,
            ));
        }
        for device in self.devices.clone() {
            let revoked = device.status == vibex_core::RemoteDeviceStatus::Revoked;
            let detail = management_remote_device_detail(device.status, device.permission_level);
            let revoke_id = device.device_id.as_str().to_string();
            device_rows = device_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.70))
                    .px_3()
                    .py_2()
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(device.display_name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(detail),
                            ),
                    )
                    .when(!revoked, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("revoke-device-{revoke_id}")))
                                .small()
                                .danger()
                                .label(management_locale_text("Revoke", "撤销", "撤銷"))
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_revoke_device(revoke_id.clone(), window, cx)
                                })),
                        )
                    }),
            );
        }
        section_layout(
            management_locale_text(
                "Remote access and devices",
                "远程访问与设备",
                "遠端存取與裝置",
            ),
            management_locale_text(
                "Configure remote methods in one place, then manage trusted devices and audit",
                "集中配置远程连接方式，并管理受信任设备及审计",
                "集中配置遠端連線方式，並管理受信任裝置及稽核",
            ),
            cx,
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .rounded(px(8.0))
                .border_1()
                .border_color(cx.theme().border.opacity(0.70))
                .px_3()
                .py_3()
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().font_medium().child(management_locale_text(
                            "Remote access methods",
                            "远程连接方式",
                            "遠端連線方式",
                        )))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(management_locale_text(
                                    "Tailscale Serve, self-managed Direct, and self-hosted Relay",
                                    "Tailscale Serve、自管 Direct 与自建 Relay",
                                    "Tailscale Serve、自管 Direct 與自建 Relay",
                                )),
                        ),
                )
                .child(
                    Button::new("manage-remote-access")
                        .primary()
                        .icon(IconName::Network)
                        .label(management_locale_text(
                            "Manage remote access",
                            "管理远程访问",
                            "管理遠端存取",
                        ))
                        .disabled(self.runtime.is_none())
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(runtime) = this.runtime.clone() {
                                open_remote_access_pairing(runtime, window, cx);
                            }
                        })),
                ),
        )
        .child(stat_line(
            management_locale_text("Trusted devices", "受信任设备", "受信任裝置"),
            self.device_count.to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Revoked devices", "已撤销设备", "已撤銷裝置"),
            self.revoked_device_count.to_string(),
            cx,
        ))
        .child(stat_line(
            management_locale_text("Audit records", "审计记录", "稽核記錄"),
            self.audit_count.to_string(),
            cx,
        ))
        .child(device_rows)
        .into_any_element()
    }

    fn confirm_revoke_device(
        &mut self,
        device_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let device_id = device_id.clone();
            dialog
                .title(management_locale_text_for(
                    active_locale,
                    "Revoke remote device?",
                    "撤销远程设备？",
                    "撤銷遠端裝置？",
                ))
                .child(management_locale_text_for(
                    active_locale,
                    "The device will lose access immediately. This action is audited.",
                    "该设备会立即失去访问权限，此操作将写入审计记录。",
                    "該裝置會立即失去存取權限，此操作將寫入稽核記錄。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(gpui_component::dialog::DialogClose::new().child(
                            Button::new("cancel-device-revoke").outline().label(
                                management_locale_text_for(active_locale, "Cancel", "取消", "取消"),
                            ),
                        ))
                        .child(gpui_component::dialog::DialogAction::new().child(
                            Button::new("confirm-device-revoke").danger().label(
                                management_locale_text_for(active_locale, "Revoke", "撤销", "撤銷"),
                            ),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        if let (Some(runtime), Ok(id)) = (
                            this.runtime.clone(),
                            vibex_core::DeviceId::parse(device_id.clone()),
                        ) {
                            this.begin_simple_task(
                                ManagementMutation::RemoteRevoke(device_id.clone()),
                                cx,
                                async move {
                                    runtime
                                        .management()
                                        .remote()
                                        .revoke_device(vibex_core::RemoteRevokeDeviceRequest {
                                            device_id: id,
                                            reason: Some("revoked from GPUI management".into()),
                                        })
                                        .map(|_| {
                                            management_locale_text_for(
                                                active_locale,
                                                "Remote device revoked",
                                                "远程设备已撤销",
                                                "遠端裝置已撤銷",
                                            )
                                            .to_string()
                                        })
                                },
                            );
                        }
                    });
                    true
                })
        });
    }

    fn begin_backup_create(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let raw_path = self.backup_path.read(cx).value().trim().to_string();
        let backup_dir = if raw_path.is_empty() {
            runtime
                .management()
                .backup_destination(unix_timestamp_ms().to_string())
        } else {
            PathBuf::from(raw_path)
        };
        self.recovery = RecoveryOperationState {
            operation: "backup_create".into(),
            phase: "copying".into(),
            progress_percent: 20,
            destination: Some(backup_dir.display().to_string()),
            rollback_available: false,
            error_code: None,
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::BackupCreate, cx, async move {
            runtime
                .management()
                .backup()
                .create(backup_dir)
                .map(|result| match active_locale {
                    ResolvedLocale::En => {
                        format!("Backup created at {}", result.backup_dir.display())
                    }
                    ResolvedLocale::ZhCn => {
                        format!("备份已创建：{}", result.backup_dir.display())
                    }
                    ResolvedLocale::ZhTw => {
                        format!("備份已建立：{}", result.backup_dir.display())
                    }
                })
        });
    }

    fn begin_backup_inspect(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let raw_path = self.backup_path.read(cx).value().trim().to_string();
        let backup_dir = if raw_path.is_empty() {
            runtime.management().backup_destination("manual")
        } else {
            PathBuf::from(raw_path)
        };
        self.recovery = RecoveryOperationState {
            operation: "backup_inspect".into(),
            phase: "validating".into(),
            progress_percent: 10,
            destination: Some(backup_dir.display().to_string()),
            rollback_available: false,
            error_code: None,
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::BackupInspect, cx, async move {
            runtime
                .management()
                .backup()
                .inspect(&backup_dir)
                .map(|inspection| match active_locale {
                    ResolvedLocale::En => format!(
                        "Backup verified: schema {}, {:?}",
                        inspection.database_schema_version, inspection.migration_compatibility
                    ),
                    ResolvedLocale::ZhCn => format!(
                        "备份验证通过：数据库结构版本 {}，{:?}",
                        inspection.database_schema_version, inspection.migration_compatibility
                    ),
                    ResolvedLocale::ZhTw => format!(
                        "備份驗證通過：資料庫結構版本 {}，{:?}",
                        inspection.database_schema_version, inspection.migration_compatibility
                    ),
                })
        });
    }

    fn begin_backup_restore(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let raw_backup = self.backup_path.read(cx).value().trim().to_string();
        let raw_target = self.restore_target.read(cx).value().trim().to_string();
        let backup_dir = if raw_backup.is_empty() {
            runtime.management().backup_destination("manual")
        } else {
            PathBuf::from(raw_backup)
        };
        let target_db_path = if raw_target.is_empty() {
            runtime
                .management()
                .backup()
                .database_path()
                .with_file_name("management-restored.db")
        } else {
            PathBuf::from(raw_target)
        };
        self.recovery = RecoveryOperationState {
            operation: "backup_restore".into(),
            phase: "restoring".into(),
            progress_percent: 20,
            destination: Some(target_db_path.display().to_string()),
            rollback_available: true,
            error_code: None,
        };
        let active_locale = locale::current_locale();
        self.begin_simple_task(ManagementMutation::BackupRestore, cx, async move {
            runtime
                .management()
                .backup()
                .restore(backup_dir, target_db_path)
                .map(|result| match active_locale {
                    ResolvedLocale::En => format!("Backup restored: {:?}", result.status),
                    ResolvedLocale::ZhCn => format!("备份已恢复：{:?}", result.status),
                    ResolvedLocale::ZhTw => format!("備份已復原：{:?}", result.status),
                })
        });
    }

    fn confirm_backup_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            dialog
                .title(management_locale_text_for(
                    active_locale,
                    "Restore backup to a new database?",
                    "将备份恢复到新数据库？",
                    "將備份復原到新資料庫？",
                ))
                .child(management_locale_text_for(
                    active_locale,
                    "The target must be empty. Existing databases are never overwritten.",
                    "目标路径必须为空，现有数据库不会被覆盖。",
                    "目標路徑必須為空，現有資料庫不會被覆寫。",
                ))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(gpui_component::dialog::DialogClose::new().child(
                            Button::new("cancel-backup-restore").outline().label(
                                management_locale_text_for(active_locale, "Cancel", "取消", "取消"),
                            ),
                        ))
                        .child(gpui_component::dialog::DialogAction::new().child(
                            Button::new("confirm-backup-restore").danger().label(
                                management_locale_text_for(
                                    active_locale,
                                    "Restore",
                                    "恢复",
                                    "復原",
                                ),
                            ),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| this.begin_backup_restore(cx));
                    true
                })
        });
    }

    fn render_recovery(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let diagnostics_status = management_operation_state_label(&self.diagnostics.status);
        let recovery_phase = management_operation_state_label(&self.recovery.phase);
        let pending = self.mutation.is_some();
        section_layout(
            management_locale_text("Recovery", "诊断与恢复", "診斷與復原"),
            management_locale_text(
                "Selection context, redacted diagnostics, and backup/restore",
                "查看选择上下文、脱敏诊断以及备份与恢复",
                "查看選擇內容、遮罩診斷以及備份與復原",
            ),
            cx,
        )
        .child(
            div()
                .text_sm()
                .font_semibold()
                .child(management_locale_text(
                    "Selection context",
                    "选择上下文",
                    "選擇內容",
                )),
        )
        .child(key_value(
            management_locale_text("Workspace", "工作区", "工作區"),
            self.pairing
                .workspace
                .as_deref()
                .unwrap_or(management_locale_text("None selected", "未选择", "未選擇")),
            cx,
        ))
        .child(key_value(
            management_locale_text("Session", "会话", "工作階段"),
            self.pairing
                .session_id
                .as_deref()
                .unwrap_or(management_locale_text("None selected", "未选择", "未選擇")),
            cx,
        ))
        .child(key_value(
            management_locale_text("Mode", "模式", "模式"),
            &self.pairing.mode,
            cx,
        ))
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text("Diagnostics", "诊断", "診斷")),
        )
        .child(key_value(
            management_locale_text("Status", "状态", "狀態"),
            &diagnostics_status,
            cx,
        ))
        .child(key_value(
            management_locale_text("Redaction", "脱敏验证", "遮罩驗證"),
            if self.diagnostics.redaction_verified {
                management_locale_text("Verified", "已验证", "已驗證")
            } else {
                management_locale_text("Failed", "失败", "失敗")
            },
            cx,
        ))
        .child(key_value(
            management_locale_text("Destination", "导出位置", "匯出位置"),
            self.diagnostics
                .destination
                .as_deref()
                .unwrap_or(management_locale_text(
                    "Not exported",
                    "尚未导出",
                    "尚未匯出",
                )),
            cx,
        ))
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(
                    Button::new("diagnostics-export")
                        .small()
                        .primary()
                        .label(management_locale_text(
                            "Export diagnostics",
                            "导出诊断",
                            "匯出診斷",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.export_diagnostics(cx))),
                )
                .child(
                    Button::new("diagnostics-retry")
                        .small()
                        .outline()
                        .label(management_locale_text(
                            "Retry export",
                            "重试导出",
                            "重試匯出",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.export_diagnostics(cx))),
                )
                .child(
                    Button::new("backup-create")
                        .small()
                        .outline()
                        .label(management_locale_text(
                            "Create backup",
                            "创建备份",
                            "建立備份",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.begin_backup_create(cx))),
                ),
        )
        .child(
            div()
                .mt_3()
                .text_sm()
                .font_semibold()
                .child(management_locale_text(
                    "Backup and restore",
                    "备份与恢复",
                    "備份與復原",
                )),
        )
        .child(management_input_field(
            management_locale_text("Backup directory", "备份目录", "備份目錄"),
            &self.backup_path,
            false,
            cx,
        ))
        .child(management_input_field(
            management_locale_text(
                "New restore database path",
                "新的恢复数据库路径",
                "新的復原資料庫路徑",
            ),
            &self.restore_target,
            false,
            cx,
        ))
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(
                    Button::new("backup-inspect")
                        .small()
                        .outline()
                        .label(management_locale_text(
                            "Inspect backup",
                            "检查备份",
                            "檢查備份",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, _, cx| this.begin_backup_inspect(cx))),
                )
                .child(
                    Button::new("backup-restore")
                        .small()
                        .danger()
                        .label(management_locale_text(
                            "Restore to new path",
                            "恢复到新路径",
                            "復原到新路徑",
                        ))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_backup_restore(window, cx)
                        })),
                ),
        )
        .child(key_value(
            management_locale_text("Recovery phase", "恢复阶段", "復原階段"),
            &recovery_phase,
            cx,
        ))
        .child(key_value(
            management_locale_text("Recovery progress", "恢复进度", "復原進度"),
            &format!("{}%", self.recovery.progress_percent),
            cx,
        ))
        .child(key_value(
            management_locale_text("Recovery destination", "恢复位置", "復原位置"),
            self.recovery
                .destination
                .as_deref()
                .unwrap_or(management_locale_text("None", "无", "無")),
            cx,
        ))
        .when_some(self.recovery.error_code.clone(), |this, code| {
            this.child(status_line(
                match locale::current_locale() {
                    ResolvedLocale::En => format!("Recovery error: {code}"),
                    ResolvedLocale::ZhCn => format!("恢复错误：{code}"),
                    ResolvedLocale::ZhTw => format!("復原錯誤：{code}"),
                },
                true,
                cx,
            ))
        })
        .into_any_element()
    }
}

impl Render for ManagementProfileDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.center.read(cx).profile_editor_open {
            cx.defer_in(window, |_, window, cx| {
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                }
            });
            return div().size_full().into_any_element();
        }
        self.center
            .update(cx, |center, cx| center.render_profile_editor_dialog(cx))
    }
}

impl Render for ManagementImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open, discovering, mutation, candidates) = {
            let center = self.center.read(cx);
            match self.kind {
                ManagementImportKind::Mcp => (
                    center.mcp_import_open,
                    matches!(
                        &center.mutation,
                        Some(ManagementMutation::McpAction(action)) if action == "discover"
                    ),
                    center.mutation.clone(),
                    center
                        .mcp_discovery
                        .as_ref()
                        .map(|response| {
                            response
                                .discoveries
                                .iter()
                                .map(|item| {
                                    (
                                        item.discovery_id.clone(),
                                        item.candidate
                                            .as_ref()
                                            .map(|candidate| candidate.display_name.clone())
                                            .unwrap_or_else(|| item.import_key.clone()),
                                        item.source_path.clone(),
                                        item.status,
                                        item.status
                                            == vibex_core::ResourceDiscoveryStatus::Importable
                                            && item.candidate.is_some(),
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                ),
                ManagementImportKind::Skill => (
                    center.skill_import_open,
                    matches!(
                        &center.mutation,
                        Some(ManagementMutation::SkillAction(action)) if action == "discover"
                    ),
                    center.mutation.clone(),
                    center
                        .skill_discovery
                        .as_ref()
                        .map(|response| {
                            response
                                .discoveries
                                .iter()
                                .map(|item| {
                                    (
                                        item.discovery_id.clone(),
                                        item.display_name.clone(),
                                        item.source_path.clone(),
                                        item.status,
                                        item.status
                                            == vibex_core::ResourceDiscoveryStatus::Importable,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                ),
            }
        };
        if !open {
            cx.defer_in(window, |_, window, cx| {
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                }
            });
            return div().size_full();
        }

        let pending = mutation.is_some();
        let mut rows = v_flex().w_full().gap_2();
        if discovering {
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.70))
                    .bg(cx.theme().muted.opacity(0.25))
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::LoaderCircle).size(px(16.0)))
                    .child(management_locale_text(
                        "Detecting configurations from installed Agents...",
                        "正在从已安装的 Agent 中探测配置...",
                        "正在從已安裝的 Agent 中探測配置...",
                    )),
            );
        } else if candidates.is_empty() {
            rows = rows.child(
                div()
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_4()
                    .text_sm()
                    .font_medium()
                    .child(management_locale_text(
                        "No import candidates",
                        "没有可导入的候选项",
                        "沒有可匯入的候選項",
                    )),
            );
        }
        if !discovering {
            for (id, title, subtitle, status, importable) in candidates {
                let action = format!("import:{id}");
                let importing = match (&self.kind, &mutation) {
                    (ManagementImportKind::Mcp, Some(ManagementMutation::McpAction(active))) => {
                        active == &action
                    }
                    (
                        ManagementImportKind::Skill,
                        Some(ManagementMutation::SkillAction(active)),
                    ) => active == &action,
                    _ => false,
                };
                let import_id = id.clone();
                let kind = self.kind;
                let icon = match kind {
                    ManagementImportKind::Mcp => IconName::Network,
                    ManagementImportKind::Skill => IconName::BookOpen,
                };
                rows = rows.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.70))
                        .p_3()
                        .child(Icon::new(icon).size(px(16.0)))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .child(div().truncate().text_sm().font_medium().child(title))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                ),
                        )
                        .child(management_status_badge(
                            management_resource_discovery_status_label(status).to_string(),
                            cx,
                        ))
                        .child(
                            Button::new(SharedString::from(format!(
                                "management-import-candidate-{id}"
                            )))
                            .small()
                            .outline()
                            .label(management_locale_text("Import", "导入", "匯入"))
                            .loading(importing)
                            .disabled(pending || !importable)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let center = this.center.clone();
                                    center.update(cx, |center, cx| match kind {
                                        ManagementImportKind::Mcp => {
                                            center.import_mcp_discovery(import_id.clone(), cx)
                                        }
                                        ManagementImportKind::Skill => {
                                            center.import_skill_discovery(import_id.clone(), cx)
                                        }
                                    });
                                },
                            )),
                        ),
                );
            }
        }

        let kind = self.kind;
        v_flex()
            .size_full()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .pr_1()
                    .child(rows),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .justify_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .child(
                        Button::new("management-import-rescan")
                            .small()
                            .outline()
                            .icon(IconName::Search)
                            .label(if discovering {
                                management_locale_text("Detecting...", "正在探测...", "正在探測...")
                            } else {
                                management_locale_text("Detect", "探测", "探測")
                            })
                            .loading(discovering)
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let center = this.center.clone();
                                center.update(cx, |center, cx| match kind {
                                    ManagementImportKind::Mcp => {
                                        center.mcp_discovery = None;
                                        center.discover_mcp_servers(cx);
                                    }
                                    ManagementImportKind::Skill => {
                                        center.skill_discovery = None;
                                        center.discover_skills(cx);
                                    }
                                });
                            })),
                    )
                    .child(
                        Button::new("management-import-close")
                            .small()
                            .secondary()
                            .label(management_locale_text("Close", "关闭", "關閉"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let center = this.center.clone();
                                let kind = this.kind;
                                center.update(cx, |center, cx| {
                                    match kind {
                                        ManagementImportKind::Mcp => {
                                            center.mcp_import_open = false;
                                            center.mcp_discovery = None;
                                        }
                                        ManagementImportKind::Skill => {
                                            center.skill_import_open = false;
                                            center.skill_discovery = None;
                                        }
                                    }
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            })),
                    ),
            )
    }
}

impl Render for ManagementCenter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.present_feedback(window, cx);
        let viewport = window.viewport_size();
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let wide = management_uses_wide_layout(viewport_width);
        let (compact_min_height, compact_max_height) =
            management_compact_sidebar_height_limits(viewport_height);
        let compact_sidebar_height = self
            .compact_sidebar_height
            .clamp(compact_min_height, compact_max_height);
        let header = self.render_header(cx);
        let nav = self.render_nav(cx);
        let context_sidebar = self.render_context_sidebar(window, cx);
        let content = self.render_content(window, cx);
        let sidebar = v_flex()
            .min_h_0()
            .flex_none()
            .overflow_hidden()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .when(wide, |sidebar| {
                sidebar
                    .w(px(MANAGEMENT_SIDEBAR_WIDTH))
                    .h_full()
                    .border_r_1()
            })
            .when(!wide, |sidebar| {
                sidebar.w_full().h(px(compact_sidebar_height))
            })
            .child(header)
            .child(
                v_flex()
                    .min_h_0()
                    .flex_1()
                    .gap(px(10.0))
                    .p_3()
                    .child(nav)
                    .child(
                        div()
                            .min_h_0()
                            .flex_1()
                            .overflow_hidden()
                            .child(context_sidebar),
                    ),
            );
        let main = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scrollbar()
            .when(viewport_width < 640.0, |main| main.p_3())
            .when(viewport_width >= 640.0, |main| main.p_4())
            .child(content);
        let layout = if wide {
            h_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(sidebar)
                .child(main)
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(sidebar)
                .child(self.render_compact_sidebar_resize_handle(
                    compact_min_height,
                    compact_max_height,
                    compact_sidebar_height,
                    cx,
                ))
                .child(main)
                .into_any_element()
        };
        div()
            .id("management-center")
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(self.compact_sidebar_resize_drag.is_some(), |root| {
                root.cursor_ns_resize()
            })
            .child(layout)
    }
}

fn management_compact_sidebar_height_limits(viewport_height: f32) -> (f32, f32) {
    let available = viewport_height
        - MANAGEMENT_HOST_TITLE_BAR_HEIGHT
        - MANAGEMENT_COMPACT_RESIZE_HANDLE_HEIGHT
        - MANAGEMENT_COMPACT_MAIN_MIN_HEIGHT;
    let max_height = available.clamp(
        MANAGEMENT_COMPACT_SIDEBAR_MIN_HEIGHT,
        MANAGEMENT_COMPACT_SIDEBAR_MAX_HEIGHT,
    );
    (MANAGEMENT_COMPACT_SIDEBAR_MIN_HEIGHT, max_height)
}

fn management_uses_wide_layout(viewport_width: f32) -> bool {
    viewport_width >= MANAGEMENT_WIDE_BREAKPOINT
}

fn management_primary_section(section: ManagementSection) -> ManagementSection {
    match section {
        ManagementSection::Agents | ManagementSection::ModelProviders => ManagementSection::Agents,
        ManagementSection::Mcp => ManagementSection::Mcp,
        ManagementSection::Skills => ManagementSection::Skills,
        ManagementSection::PromptsHooks
        | ManagementSection::Advanced
        | ManagementSection::Scheduled
        | ManagementSection::Automation
        | ManagementSection::Relay
        | ManagementSection::Recovery => ManagementSection::Advanced,
    }
}

fn reordered_provider_profile_ids(
    profile_ids: &[String],
    moving_id: &str,
    target_id: &str,
    after: bool,
) -> Vec<String> {
    let mut ids = profile_ids.to_vec();
    if moving_id == target_id {
        return ids;
    }
    let Some(moving_index) = ids.iter().position(|id| id == moving_id) else {
        return ids;
    };
    let moving = ids.remove(moving_index);
    let Some(target_index) = ids.iter().position(|id| id == target_id) else {
        ids.insert(moving_index.min(ids.len()), moving);
        return ids;
    };
    ids.insert(target_index + usize::from(after), moving);
    ids
}

fn management_provider_default_scope(
    workspace_id: Option<vibex_core::WorkspaceId>,
) -> vibex_core::ProviderProfileDefaultScope {
    if let Some(workspace_id) = workspace_id {
        vibex_core::ProviderProfileDefaultScope {
            kind: vibex_core::ProviderDefaultScopeKind::Workspace,
            project_id: None,
            workspace_id: Some(workspace_id),
        }
    } else {
        vibex_core::ProviderProfileDefaultScope {
            kind: vibex_core::ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        }
    }
}

fn management_default_updated_message(
    active_locale: ResolvedLocale,
    scope_kind: vibex_core::ProviderDefaultScopeKind,
) -> &'static str {
    match scope_kind {
        vibex_core::ProviderDefaultScopeKind::Workspace => management_locale_text_for(
            active_locale,
            "Workspace default Provider configuration updated",
            "工作区默认供应商配置已更新",
            "工作區預設供應商配置已更新",
        ),
        vibex_core::ProviderDefaultScopeKind::Project => management_locale_text_for(
            active_locale,
            "Project default Provider configuration updated",
            "项目默认供应商配置已更新",
            "專案預設供應商配置已更新",
        ),
        vibex_core::ProviderDefaultScopeKind::Global => management_locale_text_for(
            active_locale,
            "Global default Provider configuration updated",
            "全局默认供应商配置已更新",
            "全域預設供應商配置已更新",
        ),
    }
}

fn native_export_preview_matches(
    preview: &vibex_core::ProviderNativeExportPreview,
    selected_profile_id: Option<&str>,
    source: vibex_core::ProviderNativeExportSource,
    mode: vibex_core::ProviderNativeExportMode,
) -> bool {
    selected_profile_id == Some(preview.provider_profile_id.as_str())
        && preview.source == source
        && preview.mode == mode
}

fn agent_provider_profile_states(
    agent_id: &str,
    profiles: impl IntoIterator<Item = vibex_core::ProviderProfile>,
    default_profile_id: Option<&vibex_core::ProviderProfileId>,
) -> Vec<AgentProviderProfileState> {
    profiles
        .into_iter()
        .map(|profile| AgentProviderProfileState {
            agent_id: agent_id.to_string(),
            profile_id: profile.id.as_str().to_string(),
            is_default: default_profile_id == Some(&profile.id),
        })
        .collect()
}

fn agent_auth_input_key(method_id: &str, variable_name: &str) -> String {
    format!("{}:{method_id}{variable_name}", method_id.len())
}

fn agent_auth_scope_matches(
    current_generation: u64,
    current_scope: Option<&(String, Option<String>)>,
    expected_generation: u64,
    expected_scope: &(String, Option<String>),
) -> bool {
    current_generation == expected_generation && current_scope == Some(expected_scope)
}

fn terminal_auth_exit_error(
    status: &vibex_terminal::TerminalProcessExitStatus,
) -> Option<VibexError> {
    if status.exit_code == Some(0) && status.signal.is_none() {
        return None;
    }
    let mut error = VibexError::process(
        "agent_terminal_auth_failed",
        "Interactive Agent authentication did not complete successfully",
    );
    if let Some(exit_code) = status.exit_code {
        error = error.with_diagnostic("exitCode", exit_code.to_string());
    }
    if let Some(signal) = status.signal.as_deref() {
        error = error.with_diagnostic("signal", signal);
    }
    Some(error)
}

fn management_locale_text(
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
) -> &'static str {
    management_locale_text_for(locale::current_locale(), en, zh_cn, zh_tw)
}

fn management_error_text(
    en: &'static str,
    _zh_cn: &'static str,
    _zh_tw: &'static str,
) -> &'static str {
    // Error state keeps a canonical key so an already-visible error can follow locale changes.
    en
}

fn management_locale_text_for(
    locale: ResolvedLocale,
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
) -> &'static str {
    match locale {
        ResolvedLocale::En => en,
        ResolvedLocale::ZhCn => zh_cn,
        ResolvedLocale::ZhTw => zh_tw,
    }
}

fn management_secondary_label(section: ManagementSection) -> &'static str {
    match section {
        ManagementSection::Advanced => {
            management_locale_text("Native & Plugins", "原生配置与插件", "原生配置與外掛")
        }
        ManagementSection::PromptsHooks => {
            management_locale_text("Prompts & Hooks", "提示词与 Hooks", "提示詞與 Hooks")
        }
        ManagementSection::Scheduled => management_locale_text("Scheduled", "定时任务", "排程任務"),
        ManagementSection::Automation => management_locale_text("Automation", "自动化", "自動化"),
        ManagementSection::Relay => management_locale_text("Relay", "中继与设备", "中繼與裝置"),
        ManagementSection::Recovery => {
            management_locale_text("Recovery", "诊断与恢复", "診斷與復原")
        }
        _ => section.label(),
    }
}

fn management_agent_matches_search(agent: &AgentSnapshotEntry, query: &str) -> bool {
    query.is_empty()
        || format!(
            "{} {} {} {:?}",
            agent.label,
            agent.id.as_str(),
            agent.description.as_deref().unwrap_or_default(),
            agent.runtime_kind
        )
        .to_lowercase()
        .contains(query)
}

fn management_agent_sort_key(agent: &AgentSnapshotEntry) -> (u8, String, String) {
    let group = if !agent.added {
        2
    } else if agent.enabled {
        0
    } else {
        1
    };
    (
        group,
        agent.label.to_lowercase(),
        agent.id.as_str().to_string(),
    )
}

fn agent_install_url(agent: &AgentSnapshotEntry) -> Option<&str> {
    if let Some(install_url) = agent
        .params
        .get("installUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|install_url| !install_url.trim().is_empty())
    {
        return Some(install_url);
    }

    let identity = format!(
        "{} {} {} {}",
        agent.id,
        agent.label,
        agent
            .command
            .as_ref()
            .map(|command| command.command.as_str())
            .unwrap_or_default(),
        agent.description.as_deref().unwrap_or_default(),
    )
    .to_ascii_lowercase();
    let compact = identity
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let tokens = identity
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let has_token = |candidates: &[&str]| {
        candidates
            .iter()
            .any(|candidate| tokens.contains(candidate))
    };

    if compact.contains("opencode") || compact.contains("openaiopencode") {
        Some("https://opencode.ai/docs/")
    } else if compact.contains("codebuddy") {
        Some("https://www.codebuddy.ai/cli")
    } else if compact.contains("githubcopilot") || has_token(&["copilot"]) {
        Some("https://github.com/features/copilot/cli")
    } else if has_token(&["qwen", "tongyi", "dashscope"]) || compact.contains("qwencode") {
        Some("https://qwen.ai/qwencode")
    } else if has_token(&["gemini"]) || compact.contains("geminicli") {
        Some("https://geminicli.com/")
    } else if has_token(&["goose"]) {
        Some("https://goose-docs.ai/docs/getting-started/installation")
    } else if has_token(&["qoder"]) {
        Some("https://qoder.com/zh/cli")
    } else if has_token(&["kiro"]) {
        Some("https://kiro.dev/cli/")
    } else if has_token(&["cline"]) {
        Some("https://cline.bot/cli")
    } else if has_token(&["auggie", "augment", "augmentcode"]) {
        Some("https://www.augmentcode.com/product/cli")
    } else if has_token(&["amp", "ampcode", "sourcegraph"]) {
        Some("https://ampcode.com/manual")
    } else if has_token(&["claude", "anthropic"]) || compact.contains("claudecode") {
        Some("https://code.claude.com/docs/quickstart")
    } else if has_token(&["codex", "openai", "chatgpt"]) {
        Some("https://developers.openai.com/codex/cli")
    } else {
        None
    }
}

fn mcp_import_selection_from_discovery(
    item: vibex_core::McpServerDiscovery,
) -> Option<vibex_core::McpServerImportSelection> {
    if item.status != vibex_core::ResourceDiscoveryStatus::Importable {
        return None;
    }
    let candidate = item.candidate?;
    let source_agent_id = item.source_agent_id;
    Some(vibex_core::McpServerImportSelection {
        discovery_id: item.discovery_id,
        enable_agent_ids: vec![source_agent_id.clone()],
        source_agent_id,
        candidate,
    })
}

fn skill_import_selection_from_discovery(
    item: vibex_core::SkillDiscovery,
) -> Option<vibex_core::SkillImportSelection> {
    if item.status != vibex_core::ResourceDiscoveryStatus::Importable {
        return None;
    }
    let source_agent_id = item.source_agent_id;
    Some(vibex_core::SkillImportSelection {
        discovery_id: item.discovery_id,
        enable_agent_ids: vec![source_agent_id.clone()],
        source_agent_id,
        source_path: item.source_path,
        display_name: item.display_name,
        command_name: item.command_name,
        description: item.description,
        content_preview: item.content_preview,
    })
}

fn acp_config_with_editor_fields(
    mut config: vibex_core::AcpProviderConfig,
    command: String,
    args: &str,
    cwd_template: &str,
) -> vibex_core::AcpProviderConfig {
    config.command = command;
    config.args = args.split_whitespace().map(str::to_string).collect();
    config.cwd_template =
        (!cwd_template.trim().is_empty()).then(|| cwd_template.trim().to_string());
    config
}

fn updated_mcp_agent_matrix(
    mut matrix: Vec<vibex_core::McpServerAgentMatrix>,
    agent_id: AgentId,
    enabled: bool,
) -> Vec<vibex_core::McpServerAgentMatrix> {
    let updated_at_ms = unix_timestamp_ms();
    if let Some(entry) = matrix.iter_mut().find(|entry| entry.agent_id == agent_id) {
        entry.enabled = enabled;
        entry.updated_at_ms = updated_at_ms;
    } else {
        matrix.push(vibex_core::McpServerAgentMatrix {
            agent_id,
            enabled,
            source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
            updated_at_ms,
        });
    }
    matrix
}

fn updated_skill_agent_matrix(
    mut matrix: Vec<vibex_core::SkillAgentMatrix>,
    agent_id: AgentId,
    enabled: bool,
) -> Vec<vibex_core::SkillAgentMatrix> {
    let updated_at_ms = unix_timestamp_ms();
    if let Some(entry) = matrix.iter_mut().find(|entry| entry.agent_id == agent_id) {
        entry.enabled = enabled;
        entry.updated_at_ms = updated_at_ms;
    } else {
        matrix.push(vibex_core::SkillAgentMatrix {
            agent_id,
            enabled,
            source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
            updated_at_ms,
        });
    }
    matrix
}

fn normalized_provider_models(
    models: &[vibex_core::ProviderConfiguredModel],
) -> Vec<vibex_core::ProviderConfiguredModel> {
    let mut normalized = Vec::new();
    for model in models {
        let id = model.id.trim();
        if id.is_empty()
            || normalized
                .iter()
                .any(|item: &vibex_core::ProviderConfiguredModel| item.id == id)
        {
            continue;
        }
        normalized.push(vibex_core::ProviderConfiguredModel {
            id: id.to_string(),
            display_name: model
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            enabled: model.enabled,
            wire_api: model.wire_api,
        });
    }
    normalized
}

fn merge_provider_models(
    current: &mut Vec<vibex_core::ProviderConfiguredModel>,
    incoming: Vec<vibex_core::ProviderConfiguredModel>,
) {
    let mut merged = normalized_provider_models(current);
    for model in normalized_provider_models(&incoming) {
        if let Some(existing) = merged.iter_mut().find(|existing| existing.id == model.id) {
            if existing.display_name.is_none() {
                existing.display_name = model.display_name;
            }
            if existing.wire_api.is_none() {
                existing.wire_api = model.wire_api;
            }
        } else {
            merged.push(model);
        }
    }
    *current = merged;
}

fn with_provider_option(
    mut options: vibex_core::ProviderOptions,
    key: &str,
    value: Option<String>,
) -> vibex_core::ProviderOptions {
    options.entries.retain(|entry| entry.key != key);
    if let Some(value) = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        options.entries.push(vibex_core::ProviderBindingMetadata {
            key: key.to_string(),
            value,
        });
    }
    options
}

fn native_import_status_is_eligible(status: vibex_core::ProviderNativeImportItemStatus) -> bool {
    matches!(
        status,
        vibex_core::ProviderNativeImportItemStatus::Importable
            | vibex_core::ProviderNativeImportItemStatus::NeedsSecretSetup
            | vibex_core::ProviderNativeImportItemStatus::Partial
    )
}

fn provider_option_value<'a>(
    options: &'a vibex_core::ProviderOptions,
    key: &str,
) -> Option<&'a str> {
    options
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.trim())
        .filter(|value| !value.is_empty())
}

fn cc_switch_import_identity(options: &vibex_core::ProviderOptions) -> Option<String> {
    let db_path = provider_option_value(options, PROVIDER_OPTION_CC_SWITCH_DB_PATH)?;
    let provider_id = provider_option_value(options, PROVIDER_OPTION_CC_SWITCH_PROVIDER_ID)?;
    let app_type = provider_option_value(options, PROVIDER_OPTION_CC_SWITCH_APP_TYPE)?;
    Some(format!("{db_path}\0{provider_id}\0{app_type}"))
}

fn pending_cc_switch_import_item_ids(
    preview: &vibex_core::ProviderNativeImportPreview,
    existing_profiles: &[vibex_core::ProviderProfile],
    target_agent_id: &AgentId,
) -> Vec<vibex_core::RequestId> {
    let imported_identities = existing_profiles
        .iter()
        .filter(|profile| &profile.agent_id == target_agent_id)
        .filter_map(|profile| cc_switch_import_identity(&profile.provider_options))
        .collect::<HashSet<_>>();
    let mut seen_item_ids = HashSet::new();

    preview
        .items
        .iter()
        .filter(|item| item.agent_id.as_ref() == Some(target_agent_id))
        .filter(|item| {
            item.source == vibex_core::ProviderNativeImportSource::CcSwitch
                || provider_option_value(&item.provider_options, PROVIDER_OPTION_NATIVE_SOURCE)
                    == Some("cc-switch")
        })
        .filter(|item| native_import_status_is_eligible(item.status))
        .filter(|item| {
            cc_switch_import_identity(&item.provider_options)
                .is_none_or(|identity| !imported_identities.contains(&identity))
        })
        .filter(|item| seen_item_ids.insert(item.import_item_id.as_str().to_string()))
        .map(|item| item.import_item_id.clone())
        .collect()
}

fn management_agent_icon(identity: &str, label: &str, active: bool, cx: &App) -> AnyElement {
    let identity = format!("{identity} {label}").to_ascii_lowercase();
    agent_brand_icon(
        &identity,
        px(28.0),
        Some(
            cx.theme()
                .foreground
                .opacity(if active { 0.90 } else { 0.72 }),
        ),
    )
}

fn management_agent_glyph(identity: &str, label: &str, active: bool, cx: &App) -> AnyElement {
    div()
        .size(px(36.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(management_agent_icon(identity, label, active, cx))
        .into_any_element()
}

fn management_agent_detail_header(agent: &AgentSnapshotEntry, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_3()
        .px_1()
        .py_1()
        .child(
            div()
                .size(px(42.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .border_1()
                .border_color(cx.theme().border.opacity(0.75))
                .bg(cx.theme().muted.opacity(0.25))
                .child(management_agent_icon(
                    agent.id.as_str(),
                    &agent.label,
                    true,
                    cx,
                )),
        )
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .gap(px(2.0))
                .child(
                    div()
                        .truncate()
                        .text_base()
                        .font_semibold()
                        .child(agent.label.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(agent.description.clone().unwrap_or_else(|| {
                            management_locale_text(
                                "Agent settings and runtime configuration",
                                "Agent 设置与运行配置",
                                "Agent 設定與執行配置",
                            )
                            .to_string()
                        })),
                ),
        )
        .child(management_status_badge(
            management_agent_status_label(agent).to_string(),
            cx,
        ))
        .into_any_element()
}

fn management_profile_glyph(
    kind: ProviderKind,
    label: &str,
    is_default: bool,
    cx: &App,
) -> AnyElement {
    let identity = format!("{kind} {label}");
    div()
        .size(px(42.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .border_1()
        .border_color(if is_default {
            cx.theme().primary.opacity(0.25)
        } else {
            cx.theme().border.opacity(0.70)
        })
        .bg(if is_default {
            cx.theme().primary.opacity(0.08)
        } else {
            cx.theme().muted.opacity(0.35)
        })
        .child(agent_brand_icon(
            &identity,
            px(26.0),
            Some(if is_default {
                cx.theme().primary
            } else {
                cx.theme().foreground.opacity(0.78)
            }),
        ))
        .into_any_element()
}

fn management_status_badge(label: String, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .rounded(px(4.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.70))
        .bg(cx.theme().muted.opacity(0.25))
        .px_1p5()
        .py(px(1.0))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn provider_wire_api_label(wire_api: vibex_core::ProviderModelWireApi) -> &'static str {
    match wire_api {
        vibex_core::ProviderModelWireApi::OpenaiResponses => "OpenAI Responses",
        vibex_core::ProviderModelWireApi::OpenaiChatCompletions => "Chat Completions",
        vibex_core::ProviderModelWireApi::AnthropicMessages => "Anthropic Messages",
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi => "Google Generative AI",
        vibex_core::ProviderModelWireApi::AwsBedrockConverse => "AWS Bedrock Converse",
    }
}

fn provider_interface_integration_label(
    kind: vibex_core::AgentModelInterfaceIntegrationKind,
) -> &'static str {
    match kind {
        vibex_core::AgentModelInterfaceIntegrationKind::Direct => {
            management_locale_text("Direct", "直接支持", "直接支援")
        }
        vibex_core::AgentModelInterfaceIntegrationKind::Bridged => {
            management_locale_text("Bridged", "协议桥接", "協定橋接")
        }
    }
}

fn provider_protocol_url_override_label(wire_api: vibex_core::ProviderModelWireApi) -> String {
    let protocol = provider_wire_api_label(wire_api);
    match locale::current_locale() {
        ResolvedLocale::En => format!("{protocol} URL override"),
        ResolvedLocale::ZhCn => format!("{protocol} URL 覆盖"),
        ResolvedLocale::ZhTw => format!("{protocol} URL 覆寫"),
    }
}

fn management_resource_sidebar_header(title: &'static str, count: usize, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .px_1()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(title)
        .child(management_status_badge(count.to_string(), cx))
        .into_any_element()
}

fn management_resource_sidebar_glyph(path: &'static str, cx: &App) -> AnyElement {
    div()
        .size(px(36.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.70))
        .bg(cx.theme().muted.opacity(0.35))
        .child(
            Icon::default()
                .path(path)
                .size(px(16.0))
                .text_color(cx.theme().muted_foreground),
        )
        .into_any_element()
}

fn management_resource_discovery_status_label(
    status: vibex_core::ResourceDiscoveryStatus,
) -> &'static str {
    match status {
        vibex_core::ResourceDiscoveryStatus::Importable => {
            management_locale_text("Importable", "可导入", "可匯入")
        }
        vibex_core::ResourceDiscoveryStatus::AlreadyImported => {
            management_locale_text("Already imported", "已导入", "已匯入")
        }
        vibex_core::ResourceDiscoveryStatus::Unsupported => {
            management_locale_text("Unsupported", "不支持", "不支援")
        }
        vibex_core::ResourceDiscoveryStatus::Error => {
            management_locale_text("Error", "错误", "錯誤")
        }
    }
}

fn management_search_input(
    state: &Entity<InputState>,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    div()
        .flex()
        .h(px(36.0))
        .w_full()
        .flex_none()
        .items_center()
        .rounded(px(6.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.70))
        .bg(cx.theme().muted.opacity(0.20))
        .child(
            Input::new(state)
                .small()
                .h_full()
                .w_full()
                .appearance(false)
                .prefix(
                    Icon::new(IconName::Search)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .into_any_element()
}

fn management_input_field(
    label: impl Into<SharedString>,
    state: &Entity<InputState>,
    masked: bool,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    let input = Input::new(state).small().w_full();
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(if masked { input.mask_toggle() } else { input })
        .into_any_element()
}

fn compact_empty_state(
    title: &'static str,
    description: &'static str,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.70))
        .bg(cx.theme().background.opacity(0.60))
        .p_3()
        .child(div().text_sm().font_medium().child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .into_any_element()
}

fn detail_empty_state(
    title: &'static str,
    description: &'static str,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    v_flex()
        .w_full()
        .min_h(px(180.0))
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().font_semibold().child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .into_any_element()
}

fn management_profile_count(count: usize) -> String {
    match locale::current_locale() {
        ResolvedLocale::En => format!("{count} configuration(s)"),
        ResolvedLocale::ZhCn => format!("{count} 个配置"),
        ResolvedLocale::ZhTw => format!("{count} 個配置"),
    }
}

fn management_enabled_agent_count(count: usize) -> String {
    match locale::current_locale() {
        ResolvedLocale::En => format!("{count} enabled agents"),
        ResolvedLocale::ZhCn => format!("{count} 个已启用 Agent"),
        ResolvedLocale::ZhTw => format!("{count} 個已啟用 Agent"),
    }
}

fn management_mcp_resources_title() -> &'static str {
    management_locale_text("MCP RESOURCES", "MCP 资源", "MCP 資源")
}

fn management_skills_title() -> &'static str {
    management_locale_text("SKILLS", "技能", "技能")
}

fn management_resource_status_key(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn management_agent_enablement_label() -> &'static str {
    management_locale_text("Agent enablement", "Agent 启用范围", "Agent 啟用範圍")
}

fn management_agent_toggle_selector_label() -> &'static str {
    management_locale_text(
        "Toggle conversation selector availability",
        "切换对话选择器可用性",
        "切換對話選擇器可用性",
    )
}

fn management_install_label() -> &'static str {
    management_locale_text("Install", "安装", "安裝")
}

fn management_agent_status_label(agent: &AgentSnapshotEntry) -> &'static str {
    match agent.managed_install.status {
        vibex_core::AgentManagedInstallStatus::Installing
        | vibex_core::AgentManagedInstallStatus::Upgrading => {
            return management_locale_text("Downloading", "下载中", "下載中");
        }
        vibex_core::AgentManagedInstallStatus::UpdateAvailable => {
            return management_locale_text("Update available", "有可用更新", "有可用更新");
        }
        vibex_core::AgentManagedInstallStatus::Failed => {
            return management_locale_text("Install failed", "安装失败", "安裝失敗");
        }
        vibex_core::AgentManagedInstallStatus::Uninstalling => {
            return management_locale_text("Uninstalling", "卸载中", "解除安裝中");
        }
        vibex_core::AgentManagedInstallStatus::External
        | vibex_core::AgentManagedInstallStatus::NotInstalled
        | vibex_core::AgentManagedInstallStatus::Installed => {}
    }
    if !agent.added {
        return management_locale_text("Not added", "未添加", "未新增");
    }
    if !agent.installed {
        return management_locale_text("Not installed", "未安装", "未安裝");
    }
    if !agent.enabled {
        return management_locale_text("Disabled", "已停用", "已停用");
    }
    match agent.runtime_status {
        vibex_core::AgentRuntimeStatus::Ready => {
            management_locale_text("Available", "可用", "可用")
        }
        vibex_core::AgentRuntimeStatus::ProbeFailed => {
            management_locale_text("Probe failed", "检测失败", "檢測失敗")
        }
        vibex_core::AgentRuntimeStatus::Unavailable => {
            management_locale_text("Unavailable", "不可用", "不可用")
        }
        vibex_core::AgentRuntimeStatus::Disabled => {
            management_locale_text("Disabled", "已停用", "已停用")
        }
        vibex_core::AgentRuntimeStatus::Unknown => {
            management_locale_text("Not checked", "尚未检测", "尚未檢測")
        }
    }
}

fn management_managed_install_status_label(
    state: &vibex_core::AgentManagedInstallState,
) -> &'static str {
    match state.status {
        vibex_core::AgentManagedInstallStatus::External => {
            management_locale_text("External CLI", "外部 CLI", "外部 CLI")
        }
        vibex_core::AgentManagedInstallStatus::NotInstalled => {
            management_locale_text("Not installed", "未安装", "未安裝")
        }
        vibex_core::AgentManagedInstallStatus::Installing => {
            management_locale_text("Downloading", "下载中", "下載中")
        }
        vibex_core::AgentManagedInstallStatus::Installed => {
            management_locale_text("Installed", "已安装", "已安裝")
        }
        vibex_core::AgentManagedInstallStatus::UpdateAvailable => {
            management_locale_text("Update available", "有可用更新", "有可用更新")
        }
        vibex_core::AgentManagedInstallStatus::Upgrading => {
            management_locale_text("Upgrading", "升级中", "升級中")
        }
        vibex_core::AgentManagedInstallStatus::Failed => {
            management_locale_text("Install failed", "安装失败", "安裝失敗")
        }
        vibex_core::AgentManagedInstallStatus::Uninstalling => {
            management_locale_text("Uninstalling", "卸载中", "解除安裝中")
        }
    }
}

fn management_add_label() -> &'static str {
    management_locale_text("Add", "添加", "新增")
}

fn management_no_matching_agents_title() -> &'static str {
    management_locale_text("No matching Agents", "没有匹配的 Agent", "沒有符合的 Agent")
}

fn management_no_matching_agents_description() -> &'static str {
    management_locale_text(
        "Try another name or Agent id.",
        "请尝试其他名称或 Agent ID。",
        "請嘗試其他名稱或 Agent ID。",
    )
}

fn management_no_mcp_title() -> &'static str {
    management_locale_text("No MCP servers", "暂无 MCP 服务", "暫無 MCP 服務")
}

fn management_no_mcp_description() -> &'static str {
    management_locale_text(
        "Import or add an MCP server.",
        "导入或添加一个 MCP 服务。",
        "匯入或新增一個 MCP 服務。",
    )
}

fn management_no_skills_title() -> &'static str {
    management_locale_text("No Skills", "暂无技能", "暫無技能")
}

fn management_no_skills_description() -> &'static str {
    management_locale_text(
        "Import or add a reusable Skill.",
        "导入或添加一个可复用技能。",
        "匯入或新增一個可重用技能。",
    )
}

fn management_unconfigured_label() -> String {
    management_locale_text("Not configured", "未配置", "未配置").to_string()
}

fn management_enabled_label(enabled: bool) -> &'static str {
    if enabled {
        management_locale_text("Enabled", "已启用", "已啟用")
    } else {
        management_locale_text("Disabled", "已停用", "已停用")
    }
}

fn management_validate_label() -> &'static str {
    management_locale_text("Validate", "验证", "驗證")
}

fn management_delete_mcp_label() -> &'static str {
    management_locale_text("Delete MCP server", "删除 MCP 服务", "刪除 MCP 服務")
}

fn management_delete_skill_label() -> &'static str {
    management_locale_text("Delete Skill", "删除技能", "刪除技能")
}

fn management_profile_status_label(status: vibex_core::ProviderProfileStatus) -> &'static str {
    management_enabled_label(status == vibex_core::ProviderProfileStatus::Enabled)
}

fn management_health_status_label(
    status: Option<vibex_core::ProviderHealthStatus>,
) -> &'static str {
    match status.unwrap_or(vibex_core::ProviderHealthStatus::Unknown) {
        vibex_core::ProviderHealthStatus::Unknown => {
            management_locale_text("Unknown", "未知", "未知")
        }
        vibex_core::ProviderHealthStatus::Pass => management_locale_text("Healthy", "正常", "正常"),
        vibex_core::ProviderHealthStatus::Warn => management_locale_text("Warning", "警告", "警告"),
        vibex_core::ProviderHealthStatus::Fail => management_locale_text("Failed", "失败", "失敗"),
        vibex_core::ProviderHealthStatus::Skipped => {
            management_locale_text("Skipped", "已跳过", "已略過")
        }
        vibex_core::ProviderHealthStatus::Unsupported => {
            management_locale_text("Unsupported", "不支持", "不支援")
        }
    }
}

fn management_provider_test_status_label(
    status: vibex_core::AgentModelProviderTestStatus,
    active_locale: ResolvedLocale,
) -> &'static str {
    match status {
        vibex_core::AgentModelProviderTestStatus::Pass => {
            management_locale_text_for(active_locale, "Passed", "通过", "通過")
        }
        vibex_core::AgentModelProviderTestStatus::Warn => {
            management_locale_text_for(active_locale, "Warning", "警告", "警告")
        }
        vibex_core::AgentModelProviderTestStatus::Fail => {
            management_locale_text_for(active_locale, "Failed", "失败", "失敗")
        }
        vibex_core::AgentModelProviderTestStatus::Unsupported => {
            management_locale_text_for(active_locale, "Unsupported", "不支持", "不支援")
        }
    }
}

fn management_mcp_validation_status_label(
    status: vibex_core::McpServerValidationStatus,
    active_locale: ResolvedLocale,
) -> &'static str {
    match status {
        vibex_core::McpServerValidationStatus::Pass => {
            management_locale_text_for(active_locale, "Passed", "通过", "通過")
        }
        vibex_core::McpServerValidationStatus::Warn => {
            management_locale_text_for(active_locale, "Warning", "警告", "警告")
        }
        vibex_core::McpServerValidationStatus::Fail => {
            management_locale_text_for(active_locale, "Failed", "失败", "失敗")
        }
    }
}

fn management_skill_validation_status_label(
    status: vibex_core::SkillValidationStatus,
    active_locale: ResolvedLocale,
) -> &'static str {
    match status {
        vibex_core::SkillValidationStatus::Pass => {
            management_locale_text_for(active_locale, "Passed", "通过", "通過")
        }
        vibex_core::SkillValidationStatus::Warn => {
            management_locale_text_for(active_locale, "Warning", "警告", "警告")
        }
        vibex_core::SkillValidationStatus::Fail => {
            management_locale_text_for(active_locale, "Failed", "失败", "失敗")
        }
    }
}

fn management_prompt_status_label(status: vibex_core::PromptStatus) -> &'static str {
    match status {
        vibex_core::PromptStatus::Enabled => management_enabled_label(true),
        vibex_core::PromptStatus::Disabled => management_enabled_label(false),
        vibex_core::PromptStatus::Archived => {
            management_locale_text("Archived", "已归档", "已封存")
        }
    }
}

fn management_prompt_kind_key(kind: vibex_core::PromptKind) -> &'static str {
    match kind {
        vibex_core::PromptKind::ReusablePrompt => "reusable_prompt",
        vibex_core::PromptKind::SlashCommand => "slash_command",
        vibex_core::PromptKind::SystemSnippet => "system_snippet",
    }
}

fn management_prompt_scope_key(scope: vibex_core::PromptScopeKind) -> &'static str {
    match scope {
        vibex_core::PromptScopeKind::Global => "global",
        vibex_core::PromptScopeKind::User => "user",
        vibex_core::PromptScopeKind::Project => "project",
        vibex_core::PromptScopeKind::Workspace => "workspace",
    }
}

fn management_hook_status_label(status: vibex_core::HookStatus) -> &'static str {
    match status {
        vibex_core::HookStatus::Draft => management_locale_text("Draft", "草稿", "草稿"),
        vibex_core::HookStatus::Enabled => management_enabled_label(true),
        vibex_core::HookStatus::Disabled => management_enabled_label(false),
    }
}

fn management_hook_event_kind_key(kind: vibex_core::HookEventKind) -> &'static str {
    match kind {
        vibex_core::HookEventKind::TerminalActivity => "terminal_activity",
        vibex_core::HookEventKind::SessionStart => "session_start",
        vibex_core::HookEventKind::SessionStop => "session_stop",
        vibex_core::HookEventKind::PermissionRequest => "permission_request",
    }
}

fn management_operation_state_label(value: &str) -> String {
    let label = match value.trim().to_ascii_lowercase().as_str() {
        "idle" => management_locale_text("Idle", "空闲", "閒置"),
        "exporting" => management_locale_text("Exporting", "正在导出", "正在匯出"),
        "copying" => management_locale_text("Copying", "正在复制", "正在複製"),
        "validating" => management_locale_text("Validating", "正在验证", "正在驗證"),
        "restoring" => management_locale_text("Restoring", "正在恢复", "正在復原"),
        "succeeded" => management_locale_text("Succeeded", "已完成", "已完成"),
        "error" | "failed" => management_locale_text("Failed", "失败", "失敗"),
        _ => return value.to_string(),
    };
    label.to_string()
}

fn management_remote_device_detail(
    status: vibex_core::RemoteDeviceStatus,
    permission: vibex_core::RemoteDevicePermissionLevel,
) -> String {
    let status = match status {
        vibex_core::RemoteDeviceStatus::Pending => {
            management_locale_text("Pending", "待确认", "待確認")
        }
        vibex_core::RemoteDeviceStatus::Active => {
            management_locale_text("Active", "已启用", "已啟用")
        }
        vibex_core::RemoteDeviceStatus::Revoked => {
            management_locale_text("Revoked", "已撤销", "已撤銷")
        }
    };
    let permission = match permission {
        vibex_core::RemoteDevicePermissionLevel::ReadOnly => {
            management_locale_text("Read only", "只读", "唯讀")
        }
        vibex_core::RemoteDevicePermissionLevel::ApproveOnly => {
            management_locale_text("Approval only", "仅审批", "僅審批")
        }
        vibex_core::RemoteDevicePermissionLevel::FullControl => {
            management_locale_text("Full control", "完全控制", "完全控制")
        }
    };
    format!("{status} · {permission}")
}

fn management_model_count(count: usize) -> String {
    match locale::current_locale() {
        ResolvedLocale::En => format!("{count} model(s)"),
        ResolvedLocale::ZhCn => format!("{count} 个模型"),
        ResolvedLocale::ZhTw => format!("{count} 個模型"),
    }
}

fn management_skill_source_kind_label(source: vibex_core::SkillSourceKind) -> &'static str {
    match source {
        vibex_core::SkillSourceKind::Manual => "manual",
        vibex_core::SkillSourceKind::GitRepo => "git_repo",
        vibex_core::SkillSourceKind::LocalFolder => "local_folder",
        vibex_core::SkillSourceKind::Marketplace => "marketplace",
    }
}

fn management_mcp_scope_label(scope: vibex_core::McpServerScopeKind) -> &'static str {
    match scope {
        vibex_core::McpServerScopeKind::Global => "global",
        vibex_core::McpServerScopeKind::User => "user",
        vibex_core::McpServerScopeKind::Project => "project",
        vibex_core::McpServerScopeKind::Workspace => "workspace",
    }
}

fn management_skill_scope_label(scope: vibex_core::SkillScopeKind) -> &'static str {
    match scope {
        vibex_core::SkillScopeKind::Global => "global",
        vibex_core::SkillScopeKind::User => "user",
        vibex_core::SkillScopeKind::Project => "project",
        vibex_core::SkillScopeKind::Workspace => "workspace",
    }
}

fn management_resource_matrix_source_label(
    source: vibex_core::ResourceAgentMatrixSourceKind,
) -> &'static str {
    match source {
        vibex_core::ResourceAgentMatrixSourceKind::Manual => "manual",
        vibex_core::ResourceAgentMatrixSourceKind::NativeImport => "native_import",
        vibex_core::ResourceAgentMatrixSourceKind::LegacyBackfill => "legacy_backfill",
    }
}

fn management_test_label() -> &'static str {
    management_locale_text("Test connection", "测试连接", "測試連線")
}

fn management_fetch_models_label() -> &'static str {
    management_locale_text("Fetch models", "拉取模型", "擷取模型")
}

fn management_set_default_label() -> &'static str {
    management_locale_text("Set default", "设为默认", "設為預設")
}

fn management_delete_profile_label() -> &'static str {
    management_locale_text("Delete configuration", "删除配置", "刪除配置")
}

fn management_cancel_label() -> &'static str {
    management_locale_text("Cancel", "取消", "取消")
}

fn management_health_probe_label() -> &'static str {
    management_locale_text("Health check", "健康检查", "健康檢查")
}

fn management_capability_probe_label() -> &'static str {
    management_locale_text("Capability check", "能力检查", "能力檢查")
}

fn management_runtime_option_probe_message(
    result: &RuntimeOptionProbeResult,
    active_locale: ResolvedLocale,
) -> String {
    let detected = result.probed_agent_ids.len();
    let failed = result.failed_agent_ids.len();
    let cached = result.cached_agent_ids.len();
    match (active_locale, detected, failed, cached) {
        (ResolvedLocale::En, 0, 0, 0) => "Agent runtime option probe was not started".into(),
        (ResolvedLocale::ZhCn, 0, 0, 0) => "未启动 Agent 运行选项探测".into(),
        (ResolvedLocale::ZhTw, 0, 0, 0) => "未啟動 Agent 執行選項探測".into(),
        (ResolvedLocale::En, detected, 0, 0) => {
            format!("Detected and cached runtime options for {detected} Agent(s)")
        }
        (ResolvedLocale::ZhCn, detected, 0, 0) => {
            format!("已探测并缓存 {detected} 个 Agent 的运行选项")
        }
        (ResolvedLocale::ZhTw, detected, 0, 0) => {
            format!("已探測並快取 {detected} 個 Agent 的執行選項")
        }
        (ResolvedLocale::En, 0, 0, cached) => {
            format!("Using cached runtime options for {cached} Agent(s)")
        }
        (ResolvedLocale::ZhCn, 0, 0, cached) => {
            format!("正在使用 {cached} 个 Agent 的运行选项缓存")
        }
        (ResolvedLocale::ZhTw, 0, 0, cached) => {
            format!("正在使用 {cached} 個 Agent 的執行選項快取")
        }
        (ResolvedLocale::En, 0, failed, 0) => {
            format!("Runtime option probing failed for {failed} Agent(s)")
        }
        (ResolvedLocale::ZhCn, 0, failed, 0) => {
            format!("{failed} 个 Agent 的运行选项探测失败")
        }
        (ResolvedLocale::ZhTw, 0, failed, 0) => {
            format!("{failed} 個 Agent 的執行選項探測失敗")
        }
        (ResolvedLocale::En, detected, failed, cached) => {
            format!("Agent runtime options: {detected} detected, {cached} cached, {failed} failed")
        }
        (ResolvedLocale::ZhCn, detected, failed, cached) => {
            format!("Agent 运行选项：{detected} 个已探测，{cached} 个已缓存，{failed} 个失败")
        }
        (ResolvedLocale::ZhTw, detected, failed, cached) => {
            format!("Agent 執行選項：{detected} 個已探測，{cached} 個已快取，{failed} 個失敗")
        }
    }
}

fn management_append_runtime_option_probe(
    message: String,
    result: Result<RuntimeOptionProbeResult, VibexError>,
    active_locale: ResolvedLocale,
) -> String {
    match result {
        Ok(result)
            if result.probed_agent_ids.is_empty()
                && result.failed_agent_ids.is_empty()
                && result.cached_agent_ids.is_empty() =>
        {
            message
        }
        Ok(result) => format!(
            "{message}; {}",
            management_runtime_option_probe_message(&result, active_locale)
        ),
        Err(error) => format!(
            "{message}; {} ({})",
            management_locale_text_for(
                active_locale,
                "runtime option probe failed",
                "运行选项探测失败",
                "執行選項探測失敗",
            ),
            error.code
        ),
    }
}

fn management_mcp_description() -> &'static str {
    management_locale_text(
        "Managed servers, validation, and Agent enablement.",
        "管理服务、验证状态及 Agent 启用范围。",
        "管理服務、驗證狀態及 Agent 啟用範圍。",
    )
}

fn management_skills_description() -> &'static str {
    management_locale_text(
        "Reusable Skills, discovery, and Agent enablement.",
        "管理可复用技能、发现来源及 Agent 启用范围。",
        "管理可重用技能、探索來源及 Agent 啟用範圍。",
    )
}

fn management_no_mcp_selection_title() -> &'static str {
    management_locale_text(
        "No MCP server selected",
        "未选择 MCP 服务器",
        "未選擇 MCP 伺服器",
    )
}

fn management_no_mcp_selection_description() -> &'static str {
    management_locale_text(
        "Select or import a server to manage Agent enablement.",
        "选择或导入服务器以管理 Agent 启用状态。",
        "選擇或匯入伺服器以管理 Agent 啟用狀態。",
    )
}

fn management_no_skill_selection_title() -> &'static str {
    management_locale_text("No Skill selected", "未选择技能", "未選擇技能")
}

fn management_no_skill_selection_description() -> &'static str {
    management_locale_text(
        "Select or import a Skill to manage Agent enablement.",
        "选择或导入技能以管理 Agent 启用状态。",
        "選擇或匯入技能以管理 Agent 啟用狀態。",
    )
}

async fn load_snapshot(
    runtime: Arc<DesktopRuntime>,
    default_scope: vibex_core::ProviderProfileDefaultScope,
) -> VibexResult<ManagementSnapshot> {
    let management: ManagementHandle = runtime.management();
    let provider = management.providers().management();
    // Config Center refresh is the explicit, bounded slow path for installed
    // versioned Agent CLIs. Ordinary Agent catalog reads remain process-free.
    provider.refresh_detected_agent_versions()?;
    let agents = provider.list_agents(AgentListRequest {
        include_disabled: true,
    })?;
    let catalog = provider.list_agent_catalog()?;
    let model_provider_agent_ids = vibex_core::model_provider_configurable_agent_ids()?
        .into_iter()
        .map(|agent_id| agent_id.as_str().to_string())
        .collect();
    let profiles = provider.list_profiles()?;
    let native_import_preview = provider
        .preview_native_import(vibex_core::ProviderNativeImportPreviewRequest {
            sources: vec![
                vibex_core::ProviderNativeImportSource::Codex,
                vibex_core::ProviderNativeImportSource::Claude,
                vibex_core::ProviderNativeImportSource::CcSwitch,
            ],
        })
        .ok();
    let acp_configs = profiles
        .iter()
        .filter(|profile| profile.kind == ProviderKind::Acp)
        .filter_map(|profile| {
            provider
                .get_acp_profile_config(profile.id.clone())
                .ok()
                .map(|config| (profile.id.as_str().to_string(), config))
        })
        .collect::<Vec<_>>();
    let mut agent_profile_states = Vec::new();
    let mut provider_display_order = BTreeMap::new();
    let mut projection_states = Vec::new();
    let projection_workspace_key = default_scope
        .workspace_id
        .as_ref()
        .map(|workspace_id| workspace_id.as_str().to_string())
        .or_else(|| {
            default_scope
                .project_id
                .as_ref()
                .map(|project_id| project_id.as_str().to_string())
        })
        .unwrap_or_else(|| "management-global".to_string());
    for agent in agents.agents.iter().filter(|agent| agent.added) {
        let response = provider.list_agent_model_provider_profiles(
            vibex_core::AgentModelProviderProfileListRequest {
                agent_id: agent.id.clone(),
                include_disabled: true,
            },
        )?;
        let scoped_default = provider.get_agent_model_provider_default(
            vibex_core::AgentModelProviderDefaultRequest {
                scope: default_scope.clone(),
                agent_id: agent.id.clone(),
            },
        )?;
        provider_display_order.extend(response.profiles.iter().filter_map(|item| {
            item.display_order_index
                .map(|order_index| (item.profile.id.as_str().to_string(), order_index))
        }));
        agent_profile_states.extend(agent_provider_profile_states(
            agent.id.as_str(),
            response.profiles.into_iter().map(|item| item.profile),
            scoped_default.provider_profile_id.as_ref(),
        ));

        let runtimes = provider.list_agent_runtime_profiles(&agent.id)?;
        let bindings = provider.list_agent_model_provider_bindings(
            vibex_core::AgentModelProviderBindingListRequest {
                agent_id: Some(agent.id.clone()),
                model_provider_profile_id: None,
            },
        )?;
        for runtime_profile in runtimes {
            let runtime_bindings = bindings
                .iter()
                .filter(|binding| binding.runtime_profile_id == runtime_profile.id)
                .collect::<Vec<_>>();
            if runtime_bindings.is_empty() {
                let capability = provider.agent_provider_projection_capability(
                    vibex_core::AgentProviderProjectionCapabilityRequest {
                        runtime_profile_id: runtime_profile.id.clone(),
                        binding_id: None,
                    },
                )?;
                projection_states.push(AgentProviderProjectionState {
                    agent_id: agent.id.as_str().to_string(),
                    legacy_profile_id: None,
                    capability,
                    preview: None,
                });
                continue;
            }
            for binding in runtime_bindings {
                let capability = provider.agent_provider_projection_capability(
                    vibex_core::AgentProviderProjectionCapabilityRequest {
                        runtime_profile_id: runtime_profile.id.clone(),
                        binding_id: Some(binding.id.clone()),
                    },
                )?;
                let preview = provider
                    .preview_agent_provider_projection(
                        vibex_core::AgentProviderProjectionPreviewRequest {
                            binding_id: binding.id.clone(),
                            workspace_key: projection_workspace_key.clone(),
                        },
                    )
                    .ok();
                projection_states.push(AgentProviderProjectionState {
                    agent_id: agent.id.as_str().to_string(),
                    legacy_profile_id: binding
                        .legacy_provider_profile_id
                        .as_ref()
                        .map(|profile_id| profile_id.as_str().to_string()),
                    capability,
                    preview,
                });
            }
        }
    }
    let mcp_servers = provider.list_mcp_servers()?;
    let skills = provider.list_skills()?;
    let prompts = provider.list_prompts()?;
    let hooks = provider.list_hooks()?;
    let health_summaries = provider.list_health_summaries()?;
    let capability_summaries = provider.list_capability_summaries()?;
    let usage_summaries = provider.list_usage_summaries(vibex_core::ProviderUsageListRequest {
        provider_profile_ids: None,
        include_empty: true,
    })?;
    let native_exports =
        provider.list_native_exports(vibex_core::ProviderNativeExportListRequest {
            provider_profile_id: None,
            limit: Some(50),
        })?;
    let scheduled = management.scheduled().list(ScheduledTaskListRequest {
        workspace_id: None,
        status: None,
        include_deleted: false,
        limit: Some(100),
    })?;
    let scheduled_runs = management
        .scheduled()
        .list_runs(ScheduledTaskRunListRequest {
            task_id: None,
            session_id: None,
            status: None,
            limit: Some(100),
        })?;
    let scheduled_attention =
        management
            .scheduled()
            .list_attention(ScheduledTaskAttentionListRequest {
                workspace_id: None,
                limit: Some(100),
            })?;
    let scheduled_audit = management
        .scheduled()
        .list_audit(ScheduledTaskAuditListRequest {
            workspace_id: None,
            status: None,
            limit: Some(100),
        })?;
    let graphs = management.automation().list(AutomationGraphListRequest {
        workspace_id: None,
        status: None,
        include_deleted: false,
        limit: Some(100),
    })?;
    let selected_graph_id = graphs.first().map(|graph| graph.id.clone());
    let automation_runs = management
        .automation()
        .list_runs(AutomationRunListRequest {
            graph_id: selected_graph_id,
            status: None,
            limit: Some(100),
        })?;
    let automation_steps = management
        .automation()
        .list_steps(AutomationRunStepListRequest {
            run_id: None,
            node_id: None,
            status: None,
            limit: Some(500),
        })?;
    let devices = management.remote().list_devices()?;
    let revoked_device_count = devices
        .iter()
        .filter(|device| device.status == vibex_core::RemoteDeviceStatus::Revoked)
        .count();
    let audit_count = management
        .remote()
        .list_audit(vibex_core::RemoteAuditListRequest {
            device_id: None,
            limit: Some(100),
        })?
        .len();
    Ok(ManagementSnapshot {
        center: ProviderCenterSnapshot {
            agents: agents.agents,
            catalog: Some(catalog),
            profiles: profiles
                .iter()
                .map(vibex_desktop_model::ProviderProfileProjection::from_profile)
                .collect(),
            mcp_servers,
            skills,
            prompts,
            hooks,
            scheduled,
            graphs,
        },
        provider_profiles: profiles,
        model_provider_agent_ids,
        acp_configs,
        native_import_preview,
        agent_profile_states,
        provider_display_order,
        projection_states,
        health_summaries,
        capability_summaries,
        usage_summaries,
        native_exports,
        device_count: devices.len().saturating_sub(revoked_device_count),
        revoked_device_count,
        audit_count,
        scheduled_runs,
        scheduled_attention,
        scheduled_audit,
        automation_runs,
        automation_steps,
        devices,
    })
}

fn section_layout(
    title: &'static str,
    description: &'static str,
    cx: &mut Context<ManagementCenter>,
) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(div().text_lg().font_semibold().child(title))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

fn management_card(
    title: &'static str,
    description: &'static str,
    content: AnyElement,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    management_card_inner(title, description, None, content, cx)
}

fn management_card_with_icon(
    title: &'static str,
    description: &'static str,
    icon_path: &'static str,
    content: AnyElement,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    management_card_inner(title, description, Some(icon_path), content, cx)
}

fn management_detail_icon_action(button: Button, label: &'static str) -> impl IntoElement {
    let label = SharedString::from(label);
    button_with_aria_label(
        button
            .size(px(MANAGEMENT_DETAIL_ACTION_HEIGHT))
            .tooltip(label.clone()),
        label,
    )
}

fn management_card_inner(
    title: &'static str,
    description: &'static str,
    icon_path: Option<&'static str>,
    content: AnyElement,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background.opacity(0.75))
        .p_4()
        .child(if let Some(icon_path) = icon_path {
            management_module_heading(title, description, icon_path, cx)
        } else {
            v_flex()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                )
                .into_any_element()
        })
        .child(content)
        .into_any_element()
}

fn management_module_heading(
    title: &'static str,
    description: &'static str,
    icon_path: &'static str,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(42.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .bg(cx.theme().accent.opacity(0.45))
                .child(
                    Icon::default()
                        .path(icon_path)
                        .size(px(22.0))
                        .text_color(cx.theme().foreground.opacity(0.82)),
                ),
        )
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .into_any_element()
}

fn management_join_list(values: &[String]) -> String {
    if values.is_empty() {
        management_locale_text("Not available", "暂无", "暫無").to_string()
    } else {
        values.join(", ")
    }
}

fn stat_line(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    h_flex()
        .w_full()
        .justify_between()
        .border_b_1()
        .border_color(cx.theme().border)
        .py_2()
        .child(div().text_sm().child(label.into()))
        .child(div().text_sm().font_semibold().child(value.into()))
        .into_any_element()
}

fn key_value(label: &'static str, value: &str, cx: &mut Context<ManagementCenter>) -> AnyElement {
    stat_line(label, value.to_string(), cx)
}

fn status_line(message: String, error: bool, cx: &mut Context<ManagementCenter>) -> AnyElement {
    div()
        .w_full()
        .rounded_sm()
        .bg(if error {
            cx.theme().danger
        } else {
            cx.theme().secondary
        })
        .text_color(if error {
            cx.theme().danger_foreground
        } else {
            cx.theme().secondary_foreground
        })
        .px_3()
        .py_2()
        .text_xs()
        .child(message)
        .into_any_element()
}

fn empty_state(
    title: &'static str,
    description: &'static str,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    v_flex()
        .w_full()
        .items_center()
        .gap_1()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_sm()
        .p_4()
        .child(div().text_sm().font_semibold().child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mutations_are_keyed_by_their_target_agent() {
        for (mutation, expected) in [
            (ManagementMutation::AgentToggle("codex".into()), "codex"),
            (
                ManagementMutation::AgentToggle("add:claude".into()),
                "claude",
            ),
            (
                ManagementMutation::AgentToggle("probe:opencode".into()),
                "opencode",
            ),
            (ManagementMutation::AgentInstall("pi".into()), "pi"),
            (
                ManagementMutation::AgentUpdateCheck("gemini".into()),
                "gemini",
            ),
            (ManagementMutation::AgentUninstall("kiro".into()), "kiro"),
        ] {
            assert_eq!(mutation.concurrent_agent_id(), Some(expected));
        }
        assert_eq!(
            ManagementMutation::AgentDiscovery.concurrent_agent_id(),
            None
        );
        assert_eq!(
            ManagementMutation::ProfileCreate.concurrent_agent_id(),
            None
        );

        let mut pending = BTreeMap::new();
        pending.insert(
            "codex".to_string(),
            ManagementMutation::AgentInstall("codex".into()),
        );
        assert!(pending.contains_key("codex"));
        assert!(!pending.contains_key("claude"));
    }

    fn provider_options(entries: &[(&str, &str)]) -> vibex_core::ProviderOptions {
        vibex_core::ProviderOptions {
            schema_version: 1,
            entries: entries
                .iter()
                .map(|(key, value)| vibex_core::ProviderBindingMetadata {
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                })
                .collect(),
        }
    }

    fn native_import_item(
        id: &str,
        source: vibex_core::ProviderNativeImportSource,
        agent_id: &AgentId,
        status: vibex_core::ProviderNativeImportItemStatus,
        options: vibex_core::ProviderOptions,
    ) -> vibex_core::ProviderNativeImportItem {
        vibex_core::ProviderNativeImportItem {
            import_item_id: vibex_core::RequestId::parse(id).expect("valid import item id"),
            source,
            provider_kind: ProviderKind::Codex,
            agent_id: Some(agent_id.clone()),
            display_name: id.to_string(),
            account_alias: None,
            base_url: None,
            default_model: None,
            small_model: None,
            large_model: None,
            reasoning_effort: None,
            provider_options: options,
            secret_references: Vec::new(),
            status,
            redacted_fields: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn management_feedback_uses_a_top_centered_click_dismissable_autohide_notification() {
        let source = include_str!("management.rs");
        let presentation = source
            .split_once("    fn present_feedback(")
            .and_then(|(_, tail)| tail.split_once("\n    fn open_mcp_import("))
            .map(|(body, _)| body)
            .expect("management feedback presenter should remain inspectable");

        assert!(
            presentation
                .contains("Theme::global_mut(cx).notification.placement = Anchor::TopCenter;")
        );
        assert!(presentation.contains("Notification::info("));
        assert!(presentation.contains("Notification::error("));
        assert!(presentation.contains("let agent_auth_error = self.agent_auth_error.take();"));
        assert!(presentation.contains(".id::<ManagementCenterFeedbackNotification>()"));
        assert!(presentation.contains(".autohide(true)"));
        assert!(presentation.contains(".on_click(|_, _, _| {})"));

        let renderer = source
            .split_once("impl Render for ManagementCenter {")
            .and_then(|(_, tail)| tail.split_once("\nfn management_compact_sidebar_height_limits("))
            .map(|(body, _)| body)
            .expect("management renderer should remain inspectable");
        assert!(renderer.contains("self.present_feedback(window, cx);"));
        assert!(!renderer.contains(".bottom_4()"));
    }

    #[test]
    fn management_sections_collapse_into_tauri_primary_navigation() {
        assert_eq!(
            management_primary_section(ManagementSection::ModelProviders),
            ManagementSection::Agents
        );
        assert_eq!(
            management_primary_section(ManagementSection::Mcp),
            ManagementSection::Mcp
        );
        assert_eq!(
            management_primary_section(ManagementSection::Skills),
            ManagementSection::Skills
        );
        for section in [
            ManagementSection::PromptsHooks,
            ManagementSection::Advanced,
            ManagementSection::Scheduled,
            ManagementSection::Automation,
            ManagementSection::Relay,
            ManagementSection::Recovery,
        ] {
            assert_eq!(
                management_primary_section(section),
                ManagementSection::Advanced
            );
        }
    }

    #[test]
    fn compact_management_sidebar_preserves_a_reachable_main_panel() {
        assert_eq!(
            management_compact_sidebar_height_limits(620.0),
            (192.0, 372.0)
        );
        assert_eq!(
            management_compact_sidebar_height_limits(1_000.0),
            (192.0, 560.0)
        );
        let (minimum, maximum) = management_compact_sidebar_height_limits(400.0);
        assert_eq!(minimum, 192.0);
        assert_eq!(maximum, 192.0);
    }

    #[test]
    fn management_keeps_side_by_side_layout_at_common_desktop_widths() {
        assert!(!management_uses_wide_layout(1_023.0));
        assert!(management_uses_wide_layout(1_024.0));
        assert!(management_uses_wide_layout(1_270.0));
    }

    #[test]
    fn provider_default_scope_tracks_workspace_context() {
        let global = management_provider_default_scope(None);
        assert_eq!(global.kind, vibex_core::ProviderDefaultScopeKind::Global);
        assert!(global.project_id.is_none());
        assert!(global.workspace_id.is_none());

        let workspace_id =
            vibex_core::WorkspaceId::parse("workspace_management").expect("valid workspace id");
        let workspace = management_provider_default_scope(Some(workspace_id.clone()));
        assert_eq!(
            workspace.kind,
            vibex_core::ProviderDefaultScopeKind::Workspace
        );
        assert!(workspace.project_id.is_none());
        assert_eq!(workspace.workspace_id, Some(workspace_id));
    }

    #[test]
    fn scoped_provider_default_overrides_global_list_marker() {
        let global_profile = vibex_core::ProviderProfile::local_default(ProviderKind::Codex);
        let mut workspace_profile = global_profile.clone();
        workspace_profile.id = vibex_core::ProviderProfileId::parse("provider_workspace")
            .expect("valid Provider profile id");
        workspace_profile.display_name = "Workspace Provider".into();

        let states = agent_provider_profile_states(
            global_profile.agent_id.as_str(),
            vec![global_profile.clone(), workspace_profile.clone()],
            Some(&workspace_profile.id),
        );
        assert_eq!(states.len(), 2);
        assert!(states.iter().any(|state| {
            state.profile_id == workspace_profile.id.as_str() && state.is_default
        }));
        assert!(
            states.iter().any(|state| {
                state.profile_id == global_profile.id.as_str() && !state.is_default
            })
        );
    }

    #[test]
    fn native_export_preview_requires_matching_selection_source_and_mode() {
        let preview = vibex_core::ProviderNativeExportPreview {
            export_id: vibex_core::RequestId::parse("request_export")
                .expect("valid export request id"),
            provider_profile_id: vibex_core::ProviderProfileId::parse("provider_export")
                .expect("valid Provider profile id"),
            source: vibex_core::ProviderNativeExportSource::Codex,
            mode: vibex_core::ProviderNativeExportMode::ProviderProfile,
            files: Vec::new(),
            diagnostics: Vec::new(),
            created_at_ms: 1,
        };

        assert!(native_export_preview_matches(
            &preview,
            Some("provider_export"),
            vibex_core::ProviderNativeExportSource::Codex,
            vibex_core::ProviderNativeExportMode::ProviderProfile,
        ));
        assert!(!native_export_preview_matches(
            &preview,
            Some("provider_other"),
            vibex_core::ProviderNativeExportSource::Codex,
            vibex_core::ProviderNativeExportMode::ProviderProfile,
        ));
        assert!(!native_export_preview_matches(
            &preview,
            Some("provider_export"),
            vibex_core::ProviderNativeExportSource::Claude,
            vibex_core::ProviderNativeExportMode::ProviderProfile,
        ));
        assert!(!native_export_preview_matches(
            &preview,
            Some("provider_export"),
            vibex_core::ProviderNativeExportSource::Codex,
            vibex_core::ProviderNativeExportMode::Combined,
        ));
    }

    #[test]
    fn resource_agent_matrix_updates_preserve_other_assignments() {
        let codex = AgentId::parse("codex").expect("valid Agent id");
        let claude = AgentId::parse("claude").expect("valid Agent id");
        let mcp = updated_mcp_agent_matrix(
            vec![vibex_core::McpServerAgentMatrix {
                agent_id: claude.clone(),
                enabled: true,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::NativeImport,
                updated_at_ms: 1,
            }],
            codex.clone(),
            true,
        );
        assert_eq!(mcp.len(), 2);
        assert!(
            mcp.iter()
                .any(|entry| entry.agent_id == claude && entry.enabled)
        );
        assert!(
            mcp.iter()
                .any(|entry| entry.agent_id == codex && entry.enabled)
        );
        let mcp = updated_mcp_agent_matrix(mcp, claude.clone(), false);
        assert!(mcp.iter().any(|entry| {
            entry.agent_id == claude
                && !entry.enabled
                && entry.source_kind == vibex_core::ResourceAgentMatrixSourceKind::NativeImport
        }));

        let claude = AgentId::parse("claude").expect("valid Agent id");
        let skill = updated_skill_agent_matrix(
            vec![vibex_core::SkillAgentMatrix {
                agent_id: claude.clone(),
                enabled: true,
                source_kind: vibex_core::ResourceAgentMatrixSourceKind::NativeImport,
                updated_at_ms: 1,
            }],
            AgentId::parse("codex").expect("valid Agent id"),
            true,
        );
        assert_eq!(skill.len(), 2);
        let skill = updated_skill_agent_matrix(skill, claude.clone(), false);
        assert!(skill.iter().any(|entry| {
            entry.agent_id == claude
                && !entry.enabled
                && entry.source_kind == vibex_core::ResourceAgentMatrixSourceKind::NativeImport
        }));
    }

    #[test]
    fn native_import_rejects_parse_blocked_candidates() {
        assert!(native_import_status_is_eligible(
            vibex_core::ProviderNativeImportItemStatus::Importable
        ));
        assert!(native_import_status_is_eligible(
            vibex_core::ProviderNativeImportItemStatus::NeedsSecretSetup
        ));
        assert!(native_import_status_is_eligible(
            vibex_core::ProviderNativeImportItemStatus::Partial
        ));
        assert!(!native_import_status_is_eligible(
            vibex_core::ProviderNativeImportItemStatus::BlockedByParseError
        ));
    }

    #[test]
    fn cc_switch_import_candidates_match_agent_dedupe_and_skip_imported_profiles() {
        let codex = AgentId::parse("codex").expect("valid Agent id");
        let claude = AgentId::parse("claude").expect("valid Agent id");
        let imported_identity = provider_options(&[
            (PROVIDER_OPTION_CC_SWITCH_DB_PATH, "/tmp/cc-switch.db"),
            (PROVIDER_OPTION_CC_SWITCH_PROVIDER_ID, "already-imported"),
            (PROVIDER_OPTION_CC_SWITCH_APP_TYPE, "codex"),
        ]);
        let mut imported_profile = vibex_core::ProviderProfile::local_default(ProviderKind::Codex);
        imported_profile.provider_options = imported_identity.clone();

        let pending_options = provider_options(&[
            (PROVIDER_OPTION_CC_SWITCH_DB_PATH, "/tmp/cc-switch.db"),
            (PROVIDER_OPTION_CC_SWITCH_PROVIDER_ID, "pending"),
            (PROVIDER_OPTION_CC_SWITCH_APP_TYPE, "codex"),
        ]);
        let fallback_source_options = provider_options(&[
            (PROVIDER_OPTION_NATIVE_SOURCE, "cc-switch"),
            (PROVIDER_OPTION_CC_SWITCH_DB_PATH, "/tmp/cc-switch.db"),
            (PROVIDER_OPTION_CC_SWITCH_PROVIDER_ID, "fallback-source"),
            (PROVIDER_OPTION_CC_SWITCH_APP_TYPE, "codex"),
        ]);
        let preview = vibex_core::ProviderNativeImportPreview {
            preview_id: vibex_core::RequestId::parse("request_preview").expect("valid preview id"),
            sources: vec![vibex_core::ProviderNativeImportSource::CcSwitch],
            files: Vec::new(),
            items: vec![
                native_import_item(
                    "request_imported",
                    vibex_core::ProviderNativeImportSource::CcSwitch,
                    &codex,
                    vibex_core::ProviderNativeImportItemStatus::Importable,
                    imported_identity,
                ),
                native_import_item(
                    "request_pending",
                    vibex_core::ProviderNativeImportSource::CcSwitch,
                    &codex,
                    vibex_core::ProviderNativeImportItemStatus::NeedsSecretSetup,
                    pending_options.clone(),
                ),
                native_import_item(
                    "request_pending",
                    vibex_core::ProviderNativeImportSource::CcSwitch,
                    &codex,
                    vibex_core::ProviderNativeImportItemStatus::Partial,
                    pending_options.clone(),
                ),
                native_import_item(
                    "request_blocked",
                    vibex_core::ProviderNativeImportSource::CcSwitch,
                    &codex,
                    vibex_core::ProviderNativeImportItemStatus::BlockedByParseError,
                    pending_options.clone(),
                ),
                native_import_item(
                    "request_other_agent",
                    vibex_core::ProviderNativeImportSource::CcSwitch,
                    &claude,
                    vibex_core::ProviderNativeImportItemStatus::Importable,
                    pending_options,
                ),
                native_import_item(
                    "request_fallback_source",
                    vibex_core::ProviderNativeImportSource::Codex,
                    &codex,
                    vibex_core::ProviderNativeImportItemStatus::Importable,
                    fallback_source_options,
                ),
            ],
            diagnostics: Vec::new(),
            created_at_ms: 1,
        };

        let ids = pending_cc_switch_import_item_ids(&preview, &[imported_profile], &codex)
            .into_iter()
            .map(|id| id.into_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["request_pending", "request_fallback_source"]);
    }

    #[test]
    fn configured_models_normalize_and_merge_detected_metadata() {
        let mut models = vec![
            vibex_core::ProviderConfiguredModel {
                id: "  gpt-5  ".into(),
                display_name: Some("  Primary  ".into()),
                enabled: true,
                wire_api: None,
            },
            vibex_core::ProviderConfiguredModel {
                id: "gpt-5".into(),
                display_name: Some("Duplicate".into()),
                enabled: false,
                wire_api: Some(vibex_core::ProviderModelWireApi::OpenaiResponses),
            },
            vibex_core::ProviderConfiguredModel {
                id: "   ".into(),
                display_name: None,
                enabled: true,
                wire_api: None,
            },
        ];
        assert_eq!(normalized_provider_models(&models).len(), 1);

        models[0].display_name = None;
        merge_provider_models(
            &mut models,
            vec![
                vibex_core::ProviderConfiguredModel {
                    id: "gpt-5".into(),
                    display_name: Some("Detected GPT-5".into()),
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::OpenaiResponses),
                },
                vibex_core::ProviderConfiguredModel {
                    id: "claude-sonnet".into(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                },
            ],
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5");
        assert_eq!(models[0].display_name.as_deref(), Some("Detected GPT-5"));
        assert_eq!(
            models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::OpenaiResponses)
        );
        assert_eq!(models[1].id, "claude-sonnet");
    }

    #[test]
    fn resource_discovery_imports_only_one_candidate_and_enables_its_source_agent() {
        let codex = AgentId::parse("codex").expect("valid Agent id");
        let mcp = mcp_import_selection_from_discovery(vibex_core::McpServerDiscovery {
            discovery_id: "mcp-candidate".into(),
            source_agent_id: codex.clone(),
            source_path: "/tmp/mcp.json".into(),
            import_key: "mcp-key".into(),
            status: vibex_core::ResourceDiscoveryStatus::Importable,
            candidate: Some(vibex_core::McpServerCreateRequest {
                display_name: "Filesystem".into(),
                transport_kind: vibex_core::McpServerTransportKind::Stdio,
                status: vibex_core::McpServerStatus::Enabled,
                scope_kind: vibex_core::McpServerScopeKind::User,
                project_id: None,
                workspace_id: None,
                command: Some("mcp-filesystem".into()),
                args: Vec::new(),
                url: None,
                description: None,
                tags: Vec::new(),
                secret_references: Vec::new(),
                provider_matrix: Vec::new(),
            }),
            existing_mcp_server_id: None,
            diagnostics: Vec::new(),
        })
        .expect("importable MCP candidate");
        assert_eq!(mcp.discovery_id, "mcp-candidate");
        assert_eq!(mcp.enable_agent_ids, vec![codex.clone()]);

        let skill = skill_import_selection_from_discovery(vibex_core::SkillDiscovery {
            discovery_id: "skill-candidate".into(),
            source_agent_id: codex.clone(),
            source_path: "/tmp/SKILL.md".into(),
            import_key: "skill-key".into(),
            status: vibex_core::ResourceDiscoveryStatus::Importable,
            display_name: "Review".into(),
            command_name: "review".into(),
            description: Some("Review changes".into()),
            content_preview: Some("preview".into()),
            existing_skill_id: None,
            diagnostics: Vec::new(),
        })
        .expect("importable Skill candidate");
        assert_eq!(skill.discovery_id, "skill-candidate");
        assert_eq!(skill.enable_agent_ids, vec![codex]);
    }

    #[test]
    fn acp_editor_fields_preserve_capabilities_and_runtime_policy() {
        let original = vibex_core::AcpProviderConfig {
            command: "old-command".into(),
            args: vec!["old-arg".into()],
            env: vec![vibex_core::AcpProviderEnvReference {
                key: "TOKEN".into(),
                source: vibex_core::AcpProviderEnvSource::SecretReference,
                value: None,
                secret_lookup_key: Some("provider-token".into()),
                redacted_hint: "configured".into(),
            }],
            cwd_template: Some("/old".into()),
            process_strategy: vibex_core::AcpProcessStrategy::PerProfilePool,
            terminal_tools: true,
            terminal_auth: true,
            models: vec!["model-a".into()],
            modes: vec!["code".into()],
            features: vec!["mcp".into()],
            disabled_tools: vec!["dangerous-tool".into()],
        };
        let updated = acp_config_with_editor_fields(
            original.clone(),
            "new-command".into(),
            "--stdio  --verbose",
            "  /workspace  ",
        );
        assert_eq!(updated.command, "new-command");
        assert_eq!(updated.args, vec!["--stdio", "--verbose"]);
        assert_eq!(updated.cwd_template.as_deref(), Some("/workspace"));
        assert_eq!(updated.env, original.env);
        assert_eq!(updated.models, original.models);
        assert_eq!(updated.modes, original.modes);
        assert_eq!(updated.features, original.features);
        assert_eq!(updated.disabled_tools, original.disabled_tools);
        assert_eq!(updated.process_strategy, original.process_strategy);
        assert_eq!(updated.terminal_tools, original.terminal_tools);
        assert_eq!(updated.terminal_auth, original.terminal_auth);
    }

    #[test]
    fn provider_profile_saves_do_not_probe_agent_runtime_options() {
        let source = include_str!("management.rs");
        let duplicate = source
            .split_once("    fn duplicate_provider_profile(")
            .and_then(|(_, tail)| tail.split_once("\n    fn save_profile("))
            .map(|(body, _)| body)
            .expect("duplicate profile handler should remain inspectable");
        let save = source
            .split_once("    fn save_profile(")
            .and_then(|(_, tail)| tail.split_once("\n    fn run_provider_health_probe("))
            .map(|(body, _)| body)
            .expect("profile save handler should remain inspectable");
        let acp = source
            .split_once("    fn save_acp_config(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_acp_config_card("))
            .map(|(body, _)| body)
            .expect("ACP config save handler should remain inspectable");

        assert!(!duplicate.contains(".probe_agent("));
        assert!(!save.contains(".probe_agent("));
        assert!(!acp.contains(".probe_agent("));
    }

    #[test]
    fn agent_setup_and_managed_install_probe_options_but_ordinary_toggle_does_not() {
        let source = include_str!("management.rs");
        let toggle = source
            .split_once("    fn toggle_agent(")
            .and_then(|(_, tail)| tail.split_once("\n    fn set_agent_added("))
            .map(|(body, _)| body)
            .expect("Agent toggle handler should remain inspectable");
        let add = source
            .split_once("    fn set_agent_added(")
            .and_then(|(_, tail)| tail.split_once("\n    fn install_managed_agent("))
            .map(|(body, _)| body)
            .expect("Agent add handler should remain inspectable");
        let managed_install = source
            .split_once("    fn install_managed_agent(")
            .and_then(|(_, tail)| tail.split_once("\n    fn check_managed_agent_update("))
            .map(|(body, _)| body)
            .expect("managed Agent install handler should remain inspectable");
        let discover = source
            .split_once("    fn discover_local_agents(")
            .and_then(|(_, tail)| tail.split_once("\n    fn probe_agent("))
            .map(|(body, _)| body)
            .expect("Agent discovery handler should remain inspectable");
        let detect_after_install = source
            .split_once("    fn probe_agent(")
            .and_then(|(_, tail)| tail.split_once("\n    fn set_default_provider_profile("))
            .map(|(body, _)| body)
            .expect("Agent install detection handler should remain inspectable");

        assert!(!toggle.contains(".probe_agent("));
        assert_eq!(add.matches(".probe_agent(").count(), 1);
        assert_eq!(managed_install.matches(".probe_agent(").count(), 1);
        assert!(managed_install.contains(".refresh_auth_methods("));
        let selection = source
            .split_once("fn apply_snapshot(")
            .and_then(|(_, tail)| tail.split_once("\n    fn export_diagnostics("))
            .map(|(body, _)| body)
            .expect("management snapshot application should remain inspectable");
        assert!(selection.contains("agent.managed_install.managed"));
        assert_eq!(discover.matches(".probe_agent(").count(), 1);
        assert_eq!(detect_after_install.matches(".probe_agent(").count(), 1);
    }

    #[test]
    fn agent_settings_expose_authentication_without_internal_runtime_panels() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");

        assert!(render.contains("render_agent_authentication(window, None, cx)"));
        assert!(!render.contains("render_runtime_verification_card"));
        assert!(!render.contains("runtime_options_card"));
        assert!(!render.contains("render_projection_contract"));
    }

    #[test]
    fn agent_settings_show_model_provider_configuration_only_when_supported() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");
        let capability_gate = render
            .find(".model_provider_agent_ids")
            .expect("Provider rendering must consult the shared capability set");
        let profile_projection = render
            .find("let mut profiles = self")
            .expect("supported Agents should still render Provider profiles");

        assert!(capability_gate < profile_projection);
        assert!(render.contains("render_agent_installation_card(&selected_agent, window, cx)"));
        assert!(render.contains("render_agent_authentication(window, None, cx)"));

        let sidebar = source
            .split_once("    fn render_agents(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp_sidebar("))
            .map(|(body, _)| body)
            .expect("Agent sidebar renderer should remain inspectable");
        assert!(sidebar.contains("model_provider_configuration_supported"));
        assert!(sidebar.contains("added && model_provider_configuration_supported"));
    }

    #[test]
    fn agent_details_reveal_consistent_provider_actions_on_hover() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");

        assert!(render.contains("management_agent_detail_header"));
        assert!(render.contains("icons/vibex/grip-vertical.svg"));
        assert!(render.contains("management_model_count"));
        assert!(render.contains("cx.theme().primary.opacity(0.08)"));
        assert!(render.contains("cx.theme().accent"));
        assert!(!render.contains("cx.theme().primary.opacity(0.11)"));
        assert!(!render.contains(".shadow_sm()"));
        assert!(!render.contains("cx.theme().ring.opacity(0.50)"));
        assert!(render.contains(".group(hover_group)"));
        assert!(render.contains(".invisible()"));
        assert!(render.contains(".group_hover(&hover_group, |style| style.visible())"));
        for action in [
            "provider-edit-",
            "provider-duplicate-",
            "provider-test-",
            "provider-delete-",
        ] {
            assert!(
                render.contains(action),
                "missing visible provider action: {action}"
            );
        }
        for action in [
            "provider-default-",
            "provider-edit-",
            "provider-duplicate-",
            "provider-test-",
        ] {
            let action_body = render
                .split_once(action)
                .map(|(_, tail)| tail)
                .expect("Provider action should remain inspectable");
            assert!(
                action_body
                    .find(".secondary()")
                    .is_some_and(|index| index < 360),
                "non-destructive Provider action must use the shared secondary background: {action}"
            );
        }

        let glyph = source
            .split_once("fn management_profile_glyph(")
            .and_then(|(_, tail)| tail.split_once("\nfn management_status_badge("))
            .map(|(body, _)| body)
            .expect("Provider glyph should remain inspectable");
        assert!(glyph.contains("agent_brand_icon("));
    }

    #[test]
    fn agent_details_merge_provider_credentials_into_authentication() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");
        let authentication = source
            .split_once("    fn render_agent_authentication(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_agent_install_loading("))
            .map(|(body, _)| body)
            .expect("Authentication renderer should remain inspectable");

        assert!(render.contains("let provider_configuration = v_flex()"));
        assert!(!render.contains("icons/vibex/database.svg"));
        assert!(render.contains("Some(provider_configuration.into_any_element())"));
        assert!(authentication.contains("provider_configuration: Option<AnyElement>"));
        assert!(authentication.contains("content.when_some(provider_configuration"));
        assert!(authentication.contains("icons/vibex/shield-alert.svg"));
    }

    #[test]
    fn agent_authentication_actions_share_method_header_row_without_kind_badges() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_agent_authentication(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_agent_install_loading("))
            .map(|(body, _)| body)
            .expect("Authentication renderer should remain inspectable");
        assert!(!render.contains("let kind_label = match method.kind"));
        assert!(!render.contains("management_status_badge(kind_label"));
        let header = render
            .split_once("let mut method_content =")
            .and_then(|(_, tail)| tail.split_once("for variable in method.environment"))
            .map(|(body, _)| body)
            .expect("Authentication method header should remain inspectable");
        assert!(header.contains("agent-auth-submit-"));
        assert!(header.contains("management_detail_icon_action("));
        assert!(!render.contains("justify_end().child(management_detail_icon_action(\n                            Button::new(SharedString::from(format!(\n                                \"agent-auth-submit-"));
    }

    #[test]
    fn provider_rows_expose_a_drag_handle_and_persist_reordered_ids() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");
        assert!(render.contains("ProviderDisplayOrderDrag"));
        assert!(render.contains("provider-drag-handle-"));
        assert!(render.contains(".cursor_grab()"));
        assert!(render.contains(".on_drag_move(cx.listener("));
        assert!(render.contains(".on_drop("));
        assert!(render.contains("reorder_provider_profiles("));
        assert!(render.contains("provider_display_order_drop_target"));

        let helper = source
            .split_once("fn reordered_provider_profile_ids(")
            .and_then(|(_, tail)| tail.split_once("\nfn management_provider_default_scope("))
            .map(|(body, _)| body)
            .expect("Provider reorder helper should remain inspectable");
        assert!(helper.contains("ids.remove(moving_index)"));
        assert!(helper.contains("ids.insert(target_index + usize::from(after), moving)"));
    }

    #[test]
    fn provider_reorder_helper_moves_before_and_after_target() {
        let ids = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            reordered_provider_profile_ids(&ids, "c", "a", false),
            ["c", "a", "b"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            reordered_provider_profile_ids(&ids, "a", "b", true),
            ["b", "a", "c"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(reordered_provider_profile_ids(&ids, "b", "b", false), ids);
    }

    #[test]
    fn agent_detail_modules_use_accessible_icon_actions() {
        let source = include_str!("management.rs");
        let authentication = source
            .split_once("    fn render_agent_authentication(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_agent_install_loading("))
            .map(|(body, _)| body)
            .expect("Authentication renderer should remain inspectable");
        let install = source
            .split_once("    fn render_agent_installation_card(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_providers("))
            .map(|(body, _)| body)
            .expect("Installation renderer should remain inspectable");
        let render = source
            .split_once("    fn render_providers(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp("))
            .map(|(body, _)| body)
            .expect("Provider renderer should remain inspectable");
        let action_helper = source
            .split_once("fn management_detail_icon_action(")
            .and_then(|(_, tail)| tail.split_once("\nfn management_card_inner("))
            .map(|(body, _)| body)
            .expect("Detail icon action helper should remain inspectable");

        assert!(install.contains("management_card_with_icon("));
        assert!(install.contains("icons/vibex/download.svg"));
        assert!(install.contains("icons/vibex/trash-2.svg"));
        assert!(render.contains("MANAGEMENT_PROVIDER_ROW_ACTION_SIZE"));
        assert!(render.contains(".icon(IconName::Plus)"));
        assert!(
            authentication
                .contains("AgentAuthMethodKind::Terminal => Icon::new(IconName::ArrowRight)")
        );
        assert!(action_helper.contains(".size(px(MANAGEMENT_DETAIL_ACTION_HEIGHT))"));
        assert!(action_helper.contains(".tooltip(label.clone())"));
        assert!(action_helper.contains("button_with_aria_label("));
        for (name, body, minimum_actions) in [
            ("authentication", authentication, 6),
            ("installation", install, 4),
            ("Provider configuration", render, 2),
        ] {
            assert!(
                !body.contains(".label("),
                "{name} actions must remain icon-only"
            );
            assert!(
                body.matches("management_detail_icon_action(").count() >= minimum_actions,
                "{name} actions must use the accessible icon helper"
            );
        }
    }

    #[test]
    fn agent_authentication_surface_covers_dynamic_methods_and_terminal_flow() {
        let source = include_str!("management.rs");
        let render = source
            .split_once("    fn render_agent_authentication(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_providers("))
            .map(|(body, _)| body)
            .expect("Agent authentication renderer should remain inspectable");

        assert!(!render.contains("if let Some(error) = self.agent_auth_error.clone()"));

        for expected in [
            "for method in catalog.methods",
            "for variable in method.environment",
            "AgentAuthMethodKind::Agent",
            "AgentAuthMethodKind::Environment",
            "AgentAuthMethodKind::Terminal",
            "agent-auth-logout",
            "agent_auth_terminal_surface",
            ".mask_toggle()",
            "Clear saved value",
        ] {
            assert!(render.contains(expected), "missing auth flow: {expected}");
        }
    }

    #[test]
    fn agent_auth_input_keys_are_unambiguous() {
        assert_ne!(
            agent_auth_input_key("ab", "c"),
            agent_auth_input_key("a", "bc")
        );
    }

    #[test]
    fn agent_auth_async_results_are_fenced_by_generation_and_scope() {
        let scope = ("opencode".to_string(), Some("profile-a".to_string()));
        assert!(agent_auth_scope_matches(4, Some(&scope), 4, &scope));
        assert!(!agent_auth_scope_matches(5, Some(&scope), 4, &scope));
        assert!(!agent_auth_scope_matches(
            4,
            Some(&("opencode".to_string(), Some("profile-b".to_string()))),
            4,
            &scope,
        ));
        assert!(!agent_auth_scope_matches(4, None, 4, &scope));
    }

    #[test]
    fn terminal_auth_exit_status_distinguishes_success_failure_and_signal() {
        assert!(
            terminal_auth_exit_error(&vibex_terminal::TerminalProcessExitStatus {
                exit_code: Some(0),
                signal: None,
            })
            .is_none()
        );
        let nonzero = terminal_auth_exit_error(&vibex_terminal::TerminalProcessExitStatus {
            exit_code: Some(7),
            signal: None,
        })
        .expect("nonzero exit must fail authentication");
        assert_eq!(nonzero.code, "agent_terminal_auth_failed");
        let signaled = terminal_auth_exit_error(&vibex_terminal::TerminalProcessExitStatus {
            exit_code: None,
            signal: Some("SIGTERM".to_string()),
        })
        .expect("signal exit must fail authentication");
        assert_eq!(signaled.code, "agent_terminal_auth_failed");
    }

    #[test]
    fn config_center_snapshot_refreshes_versioned_agents_before_projection_load() {
        let source = include_str!("management.rs");
        let load_snapshot = source
            .split_once("async fn load_snapshot(")
            .and_then(|(_, tail)| tail.split_once("\nfn "))
            .map(|(body, _)| body)
            .expect("management snapshot loader should remain inspectable");
        let version_refresh = load_snapshot
            .find("provider.refresh_detected_agent_versions()?")
            .expect("Config Center snapshot must refresh detected Agent versions");
        let agent_list = load_snapshot
            .find("let agents = provider.list_agents(AgentListRequest")
            .expect("Config Center snapshot must load Agent snapshots");
        assert!(
            version_refresh < agent_list,
            "versioned Agent identity must be refreshed before capability state is loaded"
        );
    }

    #[test]
    fn agent_sidebar_keeps_hidden_scrollbars_and_full_row_selection() {
        let source = include_str!("management.rs");
        let render_agents = source
            .split_once("    fn render_agents(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_mcp_sidebar("))
            .map(|(body, _)| body)
            .expect("Agent sidebar renderer should remain inspectable");

        assert!(render_agents.contains(".id(\"management-agent-list-scroll\")"));
        assert!(!render_agents.contains(".overflow_y_scrollbar()"));
        assert!(render_agents.contains("management-agent-row-{id}"));
        assert!(render_agents.contains("this.select_management_agent(row_select_id.clone(), cx);"));
        assert!(!render_agents.contains("management-agent-select-"));
        assert!(render_agents.contains("agents.sort_by_cached_key(management_agent_sort_key);"));
        assert!(render_agents.contains("management-agent-add-{add_id}"));
        assert!(!render_agents.contains("management-agent-remove-"));
        assert!(!render_agents.contains("management-agent-catalog-"));
        assert!(render_agents.contains("cx.theme().primary.opacity(0.08)"));
        assert!(render_agents.contains("cx.theme().accent"));
        assert!(!render_agents.contains("cx.theme().primary.opacity(0.11)"));
        assert!(!render_agents.contains(".shadow_sm()"));
        assert!(!render_agents.contains(".w(px(3.0))"));
        assert!(!render_agents.contains("cx.theme().ring.opacity(0.60)"));
        assert!(render_agents.matches("cx.stop_propagation();").count() >= 4);
    }

    #[test]
    fn agent_sidebar_orders_enabled_disabled_then_unadded_by_name() {
        let definitions = vibex_core::builtin_agent_definitions();
        assert!(definitions.len() >= 5);
        let mut agents = definitions
            .iter()
            .take(5)
            .map(|definition| AgentSnapshotEntry::from_definition(definition, None, None))
            .collect::<Vec<_>>();
        for (agent, (label, added, enabled)) in agents.iter_mut().zip([
            ("Zulu", true, true),
            ("alpha", true, true),
            ("Delta", true, false),
            ("beta", true, false),
            ("Aardvark", false, false),
        ]) {
            agent.label = label.to_string();
            agent.added = added;
            agent.enabled = enabled;
        }

        agents.sort_by_cached_key(management_agent_sort_key);

        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "Zulu", "beta", "Delta", "Aardvark"]
        );
    }

    #[test]
    fn agent_install_links_match_tauri_agent_identities() {
        let snapshots = vibex_core::builtin_agent_definitions()
            .iter()
            .map(|definition| AgentSnapshotEntry::from_definition(definition, None, None))
            .collect::<Vec<_>>();
        let codex = snapshots
            .iter()
            .find(|agent| agent.id.as_str() == "codex")
            .expect("Codex is present in the built-in catalog");
        assert_eq!(
            agent_install_url(codex),
            Some("https://developers.openai.com/codex/cli")
        );
        let gemini = snapshots
            .iter()
            .find(|agent| agent.id.as_str() == "gemini")
            .expect("Gemini is present in the built-in catalog");
        assert_eq!(agent_install_url(gemini), Some("https://geminicli.com"));
        let cursor = snapshots
            .iter()
            .find(|agent| agent.id.as_str() == "cursor")
            .expect("Cursor is present in the built-in catalog");
        assert_eq!(
            agent_install_url(cursor),
            Some("https://docs.cursor.com/en/cli/overview")
        );
    }
}
