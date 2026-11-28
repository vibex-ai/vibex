//! Cross-instance session, group, folder, and project cloning.
//!
//! Clones a bounded, single-project set of ACP sessions — together with any
//! selected sidebar folder subtree, session groups, placements, and optional
//! missing Provider / Model configurations — onto another connected runtime
//! authority without starting ACP subprocesses on the target instance.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollbarAxis,
    v_flex,
};
use vibex_backend::{
    BackendError, BackendErrorKind, BackendFacade, MutationRequest, NativeBackend, WorkspaceSummary,
};
use vibex_core::{
    AddModelProviderCredentialRequest, AgentId, AgentListRequest, AgentModelProviderBindRequest,
    AgentModelProviderBinding, AgentModelProviderBindingListRequest, AgentRefreshSnapshotRequest,
    AgentRuntimeOptionProbeRequest, AgentSession, AgentSnapshotEntry, AgentUpdateConfigRequest,
    CLONED_PREFERRED_RUNTIME_SELECTION_DIAGNOSTIC, ExportedCloneSession, ImportCloneSessionEntry,
    MAX_CLONE_SESSIONS_PER_BUNDLE, ModelProviderCatalogEntry, ModelProviderCredentialSecretRequest,
    ModelProviderHeaderValue, ModelProviderProfile, ModelProviderProfileCreateRequest,
    ModelProviderProfileId, ModelProviderProfileStatus, ModelProviderProfileUpdateRequest,
    OpenWorkspaceRequest, ProjectId, ProjectRecord, ProviderProfileId, ProviderProfileSummary,
    RemoteSidebarDropPosition, RemoteSidebarItemKind, RemoteSidebarItemRef,
    RemoteSidebarOrganizationMutation, RuntimeAuthSource, RuntimeModelSelection,
    SessionCloneBundle, SessionCloneExportRequest, SessionCloneImportRequest,
    SessionRuntimeSelection, VibexSessionId, WorkspaceId, WorkspaceMode, WorkspaceRecord,
    unix_timestamp_ms,
};
use vibex_desktop_model::{
    LocaleMode, SessionGroupUiState, SidebarAuthorityArrangement, SidebarFolderUiState,
    SidebarOrganizationItem, SidebarOrganizationPlacement, SidebarOrganizationState,
    SidebarUiState,
};
use vibex_desktop_runtime::DesktopRuntime;

use super::{ComposerTarget, VibexWorkbench, remote_authority_key, sidebar_icon};
use crate::assets::agent_brand_icon;
use crate::directory_picker::{
    DirectoryBrowseTarget, DirectoryFavoritesHandler, DirectoryPickHandler, DirectoryPickerDialog,
};
use crate::gpui_ext::{PinnedScrollbar as _, ScrollGutter as _};
use crate::locale;
use crate::remote_client::{DesktopRemoteCredential, LOCAL_RUNTIME_ID, RegisteredRuntime};
use crate::spinner::Spinner;
use crate::ssh_connection::{
    LaunchConfig, SshCommand, SshConnectionProfile, SshMacOsLifecycle, TunnelProcess, TunnelSpec,
    select_local_port, supervisor,
};
use crate::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CloneSourceRequest {
    Project(ProjectId),
    Folder(String),
    Group(String),
    Sessions(Vec<VibexSessionId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloneSourceKind {
    Project,
    Folder,
    Group,
    Sessions,
}

#[derive(Debug, Clone)]
pub(super) struct CollectedCloneScope {
    pub kind: CloneSourceKind,
    pub display_label: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub source_project_id: ProjectId,
    pub source_project_name: String,
    pub source_workspace_root: String,
    pub source_workspace_mode: WorkspaceMode,
    pub session_ids: Vec<VibexSessionId>,
    /// Folder IDs in topological (parent-before-child) order.
    pub folders: Vec<(String, SidebarFolderUiState)>,
    /// Root folder when cloning a single folder subtree (`CloneSourceRequest::Folder`).
    pub root_folder_id: Option<String>,
    pub groups: Vec<(String, SessionGroupUiState)>,
    pub placements: Vec<SidebarOrganizationPlacement>,
    pub pinned_session_ids: BTreeSet<String>,
}

#[derive(Clone)]
pub(super) struct CloneTargetInstance {
    pub id: String,
    pub label: String,
    pub authority_key: String,
    pub kind: CloneTargetKind,
}

#[derive(Clone)]
pub(super) enum CloneTargetKind {
    Local {
        runtime: Arc<DesktopRuntime>,
    },
    Remote {
        registered: Box<RegisteredRuntime>,
        ssh_profile: Option<Box<SshConnectionProfile>>,
        reuse_active_tunnel: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetWorkspaceChoice {
    Existing {
        workspace_id: WorkspaceId,
        project_id: ProjectId,
        project_name: String,
        root_path: String,
        mode: WorkspaceMode,
    },
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MissingProviderAction {
    /// Target has no matching `ModelProviderProfile`; create it (+ optional API key) and bind Agents.
    CreateProvider {
        source_model_provider_id: Option<ModelProviderProfileId>,
        source_legacy_profile_id: ProviderProfileId,
        display_name: String,
        base_url: Option<String>,
        models_to_configure: Vec<String>,
        agents_to_bind: Vec<AgentId>,
        has_api_key: bool,
    },
    /// Target has a matching `ModelProviderProfile`, but needs missing model IDs and/or Agent bindings.
    UpdateProvider {
        target_model_provider_id: ModelProviderProfileId,
        display_name: String,
        missing_models: Vec<String>,
        missing_agent_bindings: Vec<AgentId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentReadinessGapKind {
    Missing { managed_installable: bool },
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TargetAgentWarning {
    pub agent_id: AgentId,
    pub agent_label: String,
    pub session_count: usize,
    pub gap: AgentReadinessGapKind,
}

struct LoadedSourceCloneData {
    source_workspace_root: String,
    exported_sessions: Vec<ExportedCloneSession>,
    source_model_providers: Vec<ModelProviderProfile>,
    source_bindings: Vec<AgentModelProviderBinding>,
    source_legacy_profiles: Vec<ProviderProfileSummary>,
}

struct EphemeralTargetLease {
    remote: Option<Arc<vibex_remote_client::WebRemoteBackend>>,
    /// Ephemeral SSH tunnel owned by this lease when cloning to an inactive SSH host.
    _tunnel: Option<TunnelProcess>,
}

impl Drop for EphemeralTargetLease {
    fn drop(&mut self) {
        if let Some(remote) = self.remote.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = remote.disconnect().await;
            });
        }
    }
}

struct ConnectedTargetState {
    instance: CloneTargetInstance,
    backend: BackendFacade,
    /// Keeps any ephemeral remote WebSocket + SSH tunnel alive while the dialog
    /// or an active clone task uses this target, and cleans them up on drop.
    ephemeral_lease: Option<Arc<EphemeralTargetLease>>,
    workspaces: Vec<WorkspaceSummary>,
    agents: Vec<AgentSnapshotEntry>,
    model_providers: Vec<ModelProviderProfile>,
    bindings: Vec<AgentModelProviderBinding>,
    legacy_profiles: Vec<ProviderProfileSummary>,
}

#[derive(Debug, Clone)]
struct CloneCompletionSummary {
    target_runtime_id: String,
    target_label: String,
    target_workspace_root: String,
    imported_session_count: usize,
    restorable_native_count: usize,
    cloned_provider_count: usize,
    first_target_session_id: Option<VibexSessionId>,
    agent_warnings: Vec<TargetAgentWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CloneDialogPhase {
    LoadingSource,
    ConnectingTarget,
    Ready,
    Cloning,
    Completed,
    Error(String),
}

pub(super) fn collect_clone_scope(
    request: &CloneSourceRequest,
    sessions: &[AgentSession],
    workspaces: &[(ProjectRecord, WorkspaceRecord)],
    organization: &SidebarOrganizationState,
    pinned_ids: &BTreeSet<String>,
) -> Result<CollectedCloneScope, String> {
    let acp_sessions_by_id: HashMap<&str, &AgentSession> = sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none() && !session.is_terminal())
        .map(|session| (session.id.as_str(), session))
        .collect();

    let lookup_project = |project_id: &ProjectId| -> (String, String, WorkspaceMode) {
        let mut fallback_name = project_id.as_str().to_string();
        let mut root_path = String::new();
        let mut mode = WorkspaceMode::CurrentCheckout;
        for (project, workspace) in workspaces {
            if &project.id == project_id {
                fallback_name = project.name.clone();
                if root_path.is_empty() || workspace.mode == WorkspaceMode::CurrentCheckout {
                    root_path = workspace.root_path.clone();
                    mode = workspace.mode;
                }
            }
        }
        (fallback_name, root_path, mode)
    };

    match request {
        CloneSourceRequest::Project(project_id) => {
            let mut session_ids: Vec<VibexSessionId> = sessions
                .iter()
                .filter(|session| {
                    session.deleted_at_ms.is_none()
                        && !session.is_terminal()
                        && &session.project_id == project_id
                })
                .map(|session| session.id.clone())
                .collect();
            session_ids.dedup();
            if session_ids.is_empty() {
                return Err(locale::text(
                    "This project has no Agent sessions to clone.",
                    "该项目下没有可克隆的 Agent 会话。",
                    "此專案下沒有可複製的 Agent 會話。",
                )
                .to_string());
            }
            let (project_name, root_path, mode) = lookup_project(project_id);
            let included_session_keys: BTreeSet<&str> =
                session_ids.iter().map(VibexSessionId::as_str).collect();

            let project_folder_ids: BTreeSet<String> = organization
                .folders
                .iter()
                .filter(|(_, folder)| folder.project_id.as_deref() == Some(project_id.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            let ordered_folders = topological_folders(&project_folder_ids, organization);

            let groups: Vec<(String, SessionGroupUiState)> = organization
                .groups
                .iter()
                .filter(|(_, group)| {
                    group.project_id == project_id.as_str()
                        && group
                            .member_session_ids
                            .iter()
                            .any(|member| included_session_keys.contains(member.as_str()))
                })
                .map(|(id, group)| (id.clone(), group.clone()))
                .collect();
            let included_group_ids: BTreeSet<&str> =
                groups.iter().map(|(id, _)| id.as_str()).collect();

            let placements = collect_relevant_placements(
                organization,
                &project_folder_ids,
                &included_group_ids,
                &included_session_keys,
            );
            let pinned_session_ids = collect_relevant_pins(pinned_ids, &included_session_keys);

            Ok(CollectedCloneScope {
                kind: CloneSourceKind::Project,
                display_label: project_name.clone(),
                source_project_id: project_id.clone(),
                source_project_name: project_name,
                source_workspace_root: root_path,
                source_workspace_mode: mode,
                session_ids,
                folders: ordered_folders,
                root_folder_id: None,
                groups,
                placements,
                pinned_session_ids,
            })
        }
        CloneSourceRequest::Folder(folder_id) => {
            let Some(root_folder) = organization.folders.get(folder_id) else {
                return Err(locale::text(
                    "The selected folder no longer exists.",
                    "所选文件夹已不存在。",
                    "所選資料夾已不存在。",
                )
                .to_string());
            };
            let Some(project_id_str) = root_folder.project_id.as_deref() else {
                return Err(locale::text(
                    "Only project-scoped folders can be cloned; each clone must stay within a single project.",
                    "仅支持克隆项目内的文件夹；每次克隆只能在同一个项目中进行。",
                    "僅支援複製專案內的資料夾；每次複製只能在同一個專案中進行。",
                )
                .to_string());
            };
            let Ok(project_id) = ProjectId::parse(project_id_str) else {
                return Err(locale::text(
                    "Invalid project identifier on the selected folder.",
                    "所选文件夹关联的项目标识无效。",
                    "所選資料夾關聯的專案標識無效。",
                )
                .to_string());
            };
            let subtree_folder_ids = collect_folder_subtree_ids(folder_id, organization);
            let ordered_folders = topological_folders(&subtree_folder_ids, organization);

            let mut included_group_ids = BTreeSet::new();
            let mut session_ids_ordered = Vec::new();
            let mut seen_sessions = BTreeSet::new();

            for placement in &organization.placements {
                let Some(parent_id) = placement.parent_folder_id.as_deref() else {
                    continue;
                };
                if !subtree_folder_ids.contains(parent_id) {
                    continue;
                }
                match &placement.item {
                    SidebarOrganizationItem::Session(sid) => {
                        if let Some(session) = acp_sessions_by_id.get(sid.as_str())
                            && session.project_id == project_id
                            && seen_sessions.insert(sid.clone())
                        {
                            session_ids_ordered.push(session.id.clone());
                        }
                    }
                    SidebarOrganizationItem::Group(gid) => {
                        if let Some(group) = organization.groups.get(gid)
                            && group.project_id == project_id.as_str()
                        {
                            included_group_ids.insert(gid.clone());
                            for member in &group.member_session_ids {
                                if let Some(session) = acp_sessions_by_id.get(member.as_str())
                                    && session.project_id == project_id
                                    && seen_sessions.insert(member.clone())
                                {
                                    session_ids_ordered.push(session.id.clone());
                                }
                            }
                        }
                    }
                    SidebarOrganizationItem::Folder(_) | SidebarOrganizationItem::Project(_) => {}
                }
            }

            if session_ids_ordered.is_empty() {
                return Err(locale::text(
                    "This folder contains no Agent sessions to clone.",
                    "该文件夹下没有可克隆的 Agent 会话。",
                    "此資料夾下沒有可複製的 Agent 會話。",
                )
                .to_string());
            }

            let (project_name, root_path, mode) = lookup_project(&project_id);
            let included_session_keys: BTreeSet<&str> = session_ids_ordered
                .iter()
                .map(VibexSessionId::as_str)
                .collect();
            let groups: Vec<(String, SessionGroupUiState)> = included_group_ids
                .iter()
                .filter_map(|gid| {
                    organization
                        .groups
                        .get(gid)
                        .map(|group| (gid.clone(), group.clone()))
                })
                .collect();
            let group_key_refs: BTreeSet<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
            let placements = collect_relevant_placements(
                organization,
                &subtree_folder_ids,
                &group_key_refs,
                &included_session_keys,
            );
            let pinned_session_ids = collect_relevant_pins(pinned_ids, &included_session_keys);

            Ok(CollectedCloneScope {
                kind: CloneSourceKind::Folder,
                display_label: root_folder.name.clone(),
                source_project_id: project_id,
                source_project_name: project_name,
                source_workspace_root: root_path,
                source_workspace_mode: mode,
                session_ids: session_ids_ordered,
                folders: ordered_folders,
                root_folder_id: Some(folder_id.clone()),
                groups,
                placements,
                pinned_session_ids,
            })
        }
        CloneSourceRequest::Group(group_id) => {
            let Some(group) = organization.groups.get(group_id) else {
                return Err(locale::text(
                    "The selected session group no longer exists.",
                    "所选会话组已不存在。",
                    "所選會話組已不存在。",
                )
                .to_string());
            };
            let Ok(project_id) = ProjectId::parse(group.project_id.as_str()) else {
                return Err(locale::text(
                    "Invalid project identifier on the selected session group.",
                    "所选会话组关联的项目标识无效。",
                    "所選會話組關聯的專案標識無效。",
                )
                .to_string());
            };
            let mut session_ids = Vec::new();
            let mut seen = BTreeSet::new();
            for member in &group.member_session_ids {
                if let Some(session) = acp_sessions_by_id.get(member.as_str()) {
                    if session.project_id != project_id {
                        return Err(locale::text(
                            "Each clone operation can only include sessions from a single project.",
                            "每次克隆只能在同一个项目中进行。",
                            "每次複製只能在同一個專案中進行。",
                        )
                        .to_string());
                    }
                    if seen.insert(member.clone()) {
                        session_ids.push(session.id.clone());
                    }
                }
            }
            if session_ids.is_empty() {
                return Err(locale::text(
                    "This session group has no Agent sessions to clone.",
                    "该会话组中没有可克隆的 Agent 会话。",
                    "此會話組中沒有可複製的 Agent 會話。",
                )
                .to_string());
            }
            let (project_name, root_path, mode) = lookup_project(&project_id);
            let included_session_keys: BTreeSet<&str> =
                session_ids.iter().map(VibexSessionId::as_str).collect();
            let pinned_session_ids = collect_relevant_pins(pinned_ids, &included_session_keys);

            Ok(CollectedCloneScope {
                kind: CloneSourceKind::Group,
                display_label: group.name.clone(),
                source_project_id: project_id,
                source_project_name: project_name,
                source_workspace_root: root_path,
                source_workspace_mode: mode,
                session_ids,
                folders: Vec::new(),
                root_folder_id: None,
                groups: vec![(group_id.clone(), group.clone())],
                placements: vec![SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Group(group_id.clone()),
                    parent_folder_id: None,
                }],
                pinned_session_ids,
            })
        }
        CloneSourceRequest::Sessions(raw_ids) => {
            let mut session_ids = Vec::new();
            let mut seen = BTreeSet::new();
            let mut project_id: Option<ProjectId> = None;
            let mut first_title: Option<String> = None;

            for raw_id in raw_ids {
                let Some(session) = acp_sessions_by_id.get(raw_id.as_str()) else {
                    continue;
                };
                match &project_id {
                    None => {
                        project_id = Some(session.project_id.clone());
                        first_title = Some(session.title.clone());
                    }
                    Some(existing) if existing != &session.project_id => {
                        return Err(locale::text(
                            "Selected sessions span multiple projects. Each clone must stay within a single project.",
                            "所选会话跨越了多个项目，每次克隆只能在同一个项目中进行。",
                            "所選會話跨越了多個專案，每次複製只能在同一個專案中進行。",
                        )
                        .to_string());
                    }
                    Some(_) => {}
                }
                if seen.insert(session.id.as_str().to_string()) {
                    session_ids.push(session.id.clone());
                }
            }

            let Some(project_id) = project_id else {
                return Err(locale::text(
                    "Select at least one Agent session to clone.",
                    "请至少选择一个可克隆的 Agent 会话。",
                    "請至少選擇一個可複製的 Agent 會話。",
                )
                .to_string());
            };
            let (project_name, root_path, mode) = lookup_project(&project_id);
            let display_label = if session_ids.len() == 1 {
                first_title.unwrap_or_else(|| project_name.clone())
            } else {
                format!(
                    "{} {}",
                    session_ids.len(),
                    locale::text("sessions", "个会话", "個會話")
                )
            };
            let included_session_keys: BTreeSet<&str> =
                session_ids.iter().map(VibexSessionId::as_str).collect();
            let pinned_session_ids = collect_relevant_pins(pinned_ids, &included_session_keys);

            Ok(CollectedCloneScope {
                kind: CloneSourceKind::Sessions,
                display_label,
                source_project_id: project_id,
                source_project_name: project_name,
                source_workspace_root: root_path,
                source_workspace_mode: mode,
                session_ids,
                folders: Vec::new(),
                root_folder_id: None,
                groups: Vec::new(),
                placements: Vec::new(),
                pinned_session_ids,
            })
        }
    }
}

fn collect_folder_subtree_ids(
    root_folder_id: &str,
    organization: &SidebarOrganizationState,
) -> BTreeSet<String> {
    let mut subtree = BTreeSet::new();
    subtree.insert(root_folder_id.to_string());
    loop {
        let mut grew = false;
        for placement in &organization.placements {
            if let SidebarOrganizationItem::Folder(child_id) = &placement.item
                && let Some(parent_id) = placement.parent_folder_id.as_deref()
                && subtree.contains(parent_id)
                && organization.folders.contains_key(child_id)
                && subtree.insert(child_id.clone())
            {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    subtree
}

fn topological_folders(
    folder_ids: &BTreeSet<String>,
    organization: &SidebarOrganizationState,
) -> Vec<(String, SidebarFolderUiState)> {
    let parent_by_folder: HashMap<&str, Option<&str>> = organization
        .placements
        .iter()
        .filter_map(|placement| match &placement.item {
            SidebarOrganizationItem::Folder(id) if folder_ids.contains(id) => {
                Some((id.as_str(), placement.parent_folder_id.as_deref()))
            }
            _ => None,
        })
        .collect();

    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::with_capacity(folder_ids.len());
    while ordered.len() < folder_ids.len() {
        let mut progressed = false;
        for id in folder_ids {
            if emitted.contains(id) {
                continue;
            }
            let parent_ready = parent_by_folder
                .get(id.as_str())
                .copied()
                .flatten()
                .is_none_or(|parent| !folder_ids.contains(parent) || emitted.contains(parent));
            if parent_ready {
                if let Some(folder) = organization.folders.get(id) {
                    ordered.push((id.clone(), folder.clone()));
                }
                emitted.insert(id.clone());
                progressed = true;
            }
        }
        if !progressed {
            for id in folder_ids {
                if emitted.insert(id.clone())
                    && let Some(folder) = organization.folders.get(id)
                {
                    ordered.push((id.clone(), folder.clone()));
                }
            }
            break;
        }
    }
    ordered
}

fn collect_relevant_placements(
    organization: &SidebarOrganizationState,
    folder_ids: &BTreeSet<String>,
    group_ids: &BTreeSet<&str>,
    session_ids: &BTreeSet<&str>,
) -> Vec<SidebarOrganizationPlacement> {
    organization
        .placements
        .iter()
        .filter_map(|placement| {
            let keep = match &placement.item {
                SidebarOrganizationItem::Folder(id) => folder_ids.contains(id),
                SidebarOrganizationItem::Group(id) => group_ids.contains(id.as_str()),
                SidebarOrganizationItem::Session(id) => session_ids.contains(id.as_str()),
                SidebarOrganizationItem::Project(_) => false,
            };
            if !keep {
                return None;
            }
            let parent_folder_id = placement
                .parent_folder_id
                .as_ref()
                .filter(|parent| folder_ids.contains(*parent))
                .cloned();
            Some(SidebarOrganizationPlacement {
                item: placement.item.clone(),
                parent_folder_id,
            })
        })
        .collect()
}

fn collect_relevant_pins(
    pinned_ids: &BTreeSet<String>,
    session_ids: &BTreeSet<&str>,
) -> BTreeSet<String> {
    pinned_ids
        .iter()
        .filter(|id| session_ids.contains(id.as_str()))
        .cloned()
        .collect()
}

pub(super) struct ClonedSidebarApply<'a> {
    pub authority_key: &'a str,
    pub scope: &'a CollectedCloneScope,
    pub target_project_id: &'a ProjectId,
    pub target_workspace_id: &'a WorkspaceId,
    pub session_id_map: &'a HashMap<String, String>,
    pub mirrored_ids: &'a RemoteMirroredIds,
    pub now_ms: i64,
}

#[derive(Default)]
pub(super) struct RemoteMirroredIds {
    pub folder_ids: HashMap<String, String>,
    pub group_ids: HashMap<String, String>,
}

async fn mirror_cloned_organization_to_remote_sidebar(
    target_backend: &BackendFacade,
    scope: &CollectedCloneScope,
    target_project_id: &ProjectId,
    target_workspace_id: &WorkspaceId,
    session_id_map: &HashMap<String, String>,
) -> RemoteMirroredIds {
    let mut mirrored = RemoteMirroredIds::default();
    let has_organization = !scope.folders.is_empty()
        || !scope.groups.is_empty()
        || !scope.placements.is_empty()
        || !scope.pinned_session_ids.is_empty();
    if !has_organization {
        return mirrored;
    }

    // Headless `vibex-server` instances do not host a Desktop sidebar bridge and
    // return `unsupported` here immediately; Desktop-hosted remote authorities
    // return their live snapshot so both shells share identical folder/group IDs.
    let Ok(mut snapshot) = target_backend.sidebar().sidebar_organization().await else {
        return mirrored;
    };

    let parent_by_old_item: HashMap<(u8, &str), Option<&str>> = scope
        .placements
        .iter()
        .map(|placement| {
            let key = match &placement.item {
                SidebarOrganizationItem::Folder(id) => (0u8, id.as_str()),
                SidebarOrganizationItem::Group(id) => (1u8, id.as_str()),
                SidebarOrganizationItem::Session(id) => (2u8, id.as_str()),
                SidebarOrganizationItem::Project(id) => (3u8, id.as_str()),
            };
            (key, placement.parent_folder_id.as_deref())
        })
        .collect();

    for (old_folder_id, folder) in &scope.folders {
        let parent_folder_id = if scope.root_folder_id.as_deref() == Some(old_folder_id.as_str()) {
            None
        } else {
            parent_by_old_item
                .get(&(0u8, old_folder_id.as_str()))
                .copied()
                .flatten()
                .and_then(|old_parent| mirrored.folder_ids.get(old_parent).cloned())
        };
        let workspace_id = folder
            .workspace_id
            .as_ref()
            .map(|_| target_workspace_id.as_str().to_string());
        let before_ids: BTreeSet<String> = snapshot.folders.iter().map(|f| f.id.clone()).collect();
        if let Ok(next_snap) = target_backend
            .sidebar()
            .mutate_sidebar_organization(
                RemoteSidebarOrganizationMutation::CreateFolder {
                    name: folder.name.clone(),
                    project_id: Some(target_project_id.as_str().to_string()),
                    workspace_id,
                    parent_folder_id,
                },
                None,
            )
            .await
        {
            if let Some(created) = next_snap
                .folders
                .iter()
                .find(|f| !before_ids.contains(&f.id))
            {
                let created_id = created.id.clone();
                mirrored
                    .folder_ids
                    .insert(old_folder_id.clone(), created_id.clone());
                snapshot = next_snap;
                if let Some(days) = folder.auto_archive_after_days
                    && let Ok(updated_snap) = target_backend
                        .sidebar()
                        .mutate_sidebar_organization(
                            RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                                folder_id: created_id,
                                days: Some(days),
                            },
                            None,
                        )
                        .await
                {
                    snapshot = updated_snap;
                }
            } else {
                snapshot = next_snap;
            }
        }
    }

    let session_id_btree: BTreeMap<String, String> = session_id_map
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (old_group_id, group) in &scope.groups {
        let Some(remapped_group) = group.remap_sessions(
            target_project_id.as_str(),
            target_workspace_id.as_str(),
            &session_id_btree,
        ) else {
            continue;
        };
        let parent_folder_id = parent_by_old_item
            .get(&(1u8, old_group_id.as_str()))
            .copied()
            .flatten()
            .and_then(|old_parent| mirrored.folder_ids.get(old_parent).cloned());
        let before_ids: BTreeSet<String> = snapshot.groups.iter().map(|g| g.id.clone()).collect();
        if let Ok(next_snap) = target_backend
            .sidebar()
            .mutate_sidebar_organization(
                RemoteSidebarOrganizationMutation::CreateGroup {
                    name: remapped_group.name.clone(),
                    project_id: target_project_id.as_str().to_string(),
                    workspace_id: target_workspace_id.as_str().to_string(),
                    member_session_ids: remapped_group.member_session_ids.clone(),
                    parent_folder_id,
                },
                None,
            )
            .await
        {
            if let Some(created) = next_snap
                .groups
                .iter()
                .find(|g| !before_ids.contains(&g.id))
            {
                let created_id = created.id.clone();
                mirrored
                    .group_ids
                    .insert(old_group_id.clone(), created_id.clone());
                snapshot = next_snap;
                if remapped_group.pinned
                    && let Ok(updated_snap) = target_backend
                        .sidebar()
                        .mutate_sidebar_organization(
                            RemoteSidebarOrganizationMutation::SetGroupPinned {
                                group_id: created_id.clone(),
                                pinned: true,
                            },
                            None,
                        )
                        .await
                {
                    snapshot = updated_snap;
                }
                if let Some(enabled) = remapped_group.auto_continue
                    && let Ok(updated_snap) = target_backend
                        .sidebar()
                        .mutate_sidebar_organization(
                            RemoteSidebarOrganizationMutation::SetGroupAutoContinue {
                                group_id: created_id,
                                enabled,
                            },
                            None,
                        )
                        .await
                {
                    snapshot = updated_snap;
                }
            } else {
                snapshot = next_snap;
            }
        }
    }

    for placement in &scope.placements {
        if let SidebarOrganizationItem::Session(old_sid) = &placement.item
            && let Some(new_sid) = session_id_map.get(old_sid)
            && let Some(old_parent) = placement.parent_folder_id.as_ref()
            && let Some(new_parent) = mirrored.folder_ids.get(old_parent)
        {
            let _ = target_backend
                .sidebar()
                .mutate_sidebar_organization(
                    RemoteSidebarOrganizationMutation::MoveItems {
                        items: vec![RemoteSidebarItemRef {
                            kind: RemoteSidebarItemKind::Session,
                            id: new_sid.clone(),
                        }],
                        anchor: Some(RemoteSidebarItemRef {
                            kind: RemoteSidebarItemKind::Folder,
                            id: new_parent.clone(),
                        }),
                        position: RemoteSidebarDropPosition::Into,
                        project_id: Some(target_project_id.as_str().to_string()),
                    },
                    None,
                )
                .await;
        }
    }

    for old_pinned in &scope.pinned_session_ids {
        if let Some(new_session_id) = session_id_map.get(old_pinned) {
            let _ = target_backend
                .sidebar()
                .mutate_sidebar_organization(
                    RemoteSidebarOrganizationMutation::SetSessionPinned {
                        session_id: new_session_id.clone(),
                        pinned: true,
                    },
                    None,
                )
                .await;
        }
    }

    mirrored
}

pub(super) fn merge_cloned_organization_into_arrangement(
    arrangement: &mut SidebarAuthorityArrangement,
    scope: &CollectedCloneScope,
    target_project_id: &ProjectId,
    target_workspace_id: &WorkspaceId,
    session_id_map: &HashMap<String, String>,
    mirrored_ids: &RemoteMirroredIds,
    now_ms: i64,
) -> BTreeSet<String> {
    let mut folder_id_map: HashMap<String, String> = HashMap::new();
    for (idx, (old_folder_id, folder)) in scope.folders.iter().enumerate() {
        let new_folder_id = mirrored_ids
            .folder_ids
            .get(old_folder_id)
            .cloned()
            .unwrap_or_else(|| format!("folder_clone_{now_ms}_{idx}"));
        folder_id_map.insert(old_folder_id.clone(), new_folder_id.clone());
        let workspace_id = folder
            .workspace_id
            .as_ref()
            .map(|_| target_workspace_id.as_str().to_string());
        arrangement.organization.folders.insert(
            new_folder_id,
            SidebarFolderUiState {
                name: folder.name.clone(),
                project_id: Some(target_project_id.as_str().to_string()),
                workspace_id,
                auto_archive_after_days: folder.auto_archive_after_days,
            },
        );
    }

    let session_id_btree: BTreeMap<String, String> = session_id_map
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut group_id_map: HashMap<String, String> = HashMap::new();
    let mut created_group_ids = BTreeSet::new();
    for (idx, (old_group_id, group)) in scope.groups.iter().enumerate() {
        let Some(remapped_group) = group.remap_sessions(
            target_project_id.as_str(),
            target_workspace_id.as_str(),
            &session_id_btree,
        ) else {
            continue;
        };
        let new_group_id = mirrored_ids
            .group_ids
            .get(old_group_id)
            .cloned()
            .unwrap_or_else(|| format!("group_clone_{now_ms}_{idx}"));
        group_id_map.insert(old_group_id.clone(), new_group_id.clone());
        created_group_ids.insert(new_group_id.clone());
        arrangement
            .organization
            .groups
            .insert(new_group_id, remapped_group);
    }

    for placement in &scope.placements {
        let new_parent = match &placement.item {
            SidebarOrganizationItem::Folder(fid)
                if scope.root_folder_id.as_deref() == Some(fid.as_str()) =>
            {
                None
            }
            _ => placement
                .parent_folder_id
                .as_ref()
                .and_then(|old_parent| folder_id_map.get(old_parent).cloned()),
        };
        let new_item = match &placement.item {
            SidebarOrganizationItem::Folder(old_id) => folder_id_map
                .get(old_id)
                .cloned()
                .map(SidebarOrganizationItem::Folder),
            SidebarOrganizationItem::Group(old_id) => group_id_map
                .get(old_id)
                .cloned()
                .map(SidebarOrganizationItem::Group),
            SidebarOrganizationItem::Session(old_id) => session_id_map
                .get(old_id)
                .cloned()
                .map(SidebarOrganizationItem::Session),
            SidebarOrganizationItem::Project(_) => None,
        };
        if let Some(item) = new_item {
            arrangement
                .organization
                .placements
                .push(SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: new_parent,
                });
        }
    }

    for old_pinned in &scope.pinned_session_ids {
        if let Some(new_session_id) = session_id_map.get(old_pinned) {
            arrangement
                .pinned_session_ids
                .insert(new_session_id.clone());
        }
    }
    for old_sid in scope.session_ids.iter().rev() {
        if let Some(new_sid) = session_id_map.get(old_sid.as_str())
            && !arrangement
                .session_order
                .iter()
                .any(|existing| existing == new_sid)
        {
            arrangement.session_order.insert(0, new_sid.clone());
        }
    }
    arrangement
        .collapsed_project_ids
        .remove(target_project_id.as_str());
    if !arrangement
        .project_order
        .iter()
        .any(|id| id == target_project_id.as_str())
    {
        arrangement
            .project_order
            .insert(0, target_project_id.as_str().to_string());
    }
    arrangement.organization.normalize();
    created_group_ids.retain(|id| arrangement.organization.groups.contains_key(id));
    created_group_ids
}

fn normalize_url_key(url: Option<&str>) -> String {
    url.unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn provider_match_key(display_name: &str, base_url: Option<&str>) -> (String, String) {
    (
        display_name.trim().to_ascii_lowercase(),
        normalize_url_key(base_url),
    )
}

fn find_matching_target_model_provider<'a>(
    display_name: &str,
    base_url: Option<&str>,
    target_providers: &'a [ModelProviderProfile],
) -> Option<&'a ModelProviderProfile> {
    let exact_key = provider_match_key(display_name, base_url);
    target_providers
        .iter()
        .filter(|profile| profile.deleted_at_ms.is_none())
        .find(|profile| {
            let url = profile.primary_api_endpoint().map(|ep| ep.url.as_str());
            provider_match_key(&profile.display_name, url) == exact_key
        })
        .or_else(|| {
            let name_lower = display_name.trim().to_ascii_lowercase();
            target_providers
                .iter()
                .filter(|profile| profile.deleted_at_ms.is_none())
                .find(|profile| profile.display_name.trim().to_ascii_lowercase() == name_lower)
        })
}

pub(super) fn compute_missing_provider_actions(
    exported_sessions: &[ExportedCloneSession],
    source_model_providers: &[ModelProviderProfile],
    source_bindings: &[AgentModelProviderBinding],
    source_legacy_profiles: &[ProviderProfileSummary],
    target_model_providers: &[ModelProviderProfile],
    target_bindings: &[AgentModelProviderBinding],
    target_legacy_profiles: &[ProviderProfileSummary],
) -> Vec<MissingProviderAction> {
    #[derive(Default)]
    struct SourceProviderRequirement {
        source_model_provider: Option<ModelProviderProfile>,
        display_name: String,
        base_url: Option<String>,
        models: BTreeSet<String>,
        agents: BTreeSet<AgentId>,
        sample_legacy_id: Option<ProviderProfileId>,
    }

    let mut requirements: BTreeMap<String, SourceProviderRequirement> = BTreeMap::new();

    for session in exported_sessions {
        let Some(selection) = session.runtime_selection.as_ref() else {
            continue;
        };
        let RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } = &selection.auth_source
        else {
            continue;
        };

        let source_binding = source_bindings.iter().find(|binding| {
            binding.legacy_provider_profile_id.as_ref() == Some(provider_profile_id)
        });
        let source_mp = source_binding
            .and_then(|binding| binding.model_provider_profile_id.as_ref())
            .and_then(|mp_id| {
                source_model_providers
                    .iter()
                    .find(|profile| &profile.id == mp_id && profile.deleted_at_ms.is_none())
            })
            .or_else(|| {
                source_model_providers.iter().find(|profile| {
                    profile.legacy_provider_profile_id.as_ref() == Some(provider_profile_id)
                        && profile.deleted_at_ms.is_none()
                })
            });

        let req_key = source_mp
            .map(|mp| format!("mp:{}", mp.id.as_str()))
            .unwrap_or_else(|| format!("legacy:{}", provider_profile_id.as_str()));

        let entry = requirements.entry(req_key).or_default();
        if entry.sample_legacy_id.is_none() {
            entry.sample_legacy_id = Some(provider_profile_id.clone());
        }
        if let Some(mp) = source_mp {
            entry.source_model_provider = Some(mp.clone());
            entry.display_name = mp.display_name.clone();
            entry.base_url = mp.primary_api_endpoint().map(|ep| ep.url.clone());
            for model in &mp.configured_models {
                if model.enabled {
                    entry.models.insert(model.id.clone());
                }
            }
        } else if let Some(hint) = session.provider_hint.as_ref() {
            entry.display_name = hint.display_name.clone();
            entry.base_url = hint.base_url.clone();
            entry.models.extend(hint.configured_models.iter().cloned());
        } else if let Some(summary) = source_legacy_profiles
            .iter()
            .find(|summary| &summary.id == provider_profile_id)
        {
            entry.display_name = summary.display_name.clone();
            for model in &summary.configured_models {
                entry.models.insert(model.id.clone());
            }
        } else {
            entry.display_name = provider_profile_id.as_str().to_string();
        }

        if let RuntimeModelSelection::Explicit { model_id } = &selection.model
            && !model_id.trim().is_empty()
        {
            entry.models.insert(model_id.clone());
        }
        entry.agents.insert(selection.agent_id.clone());
    }

    let mut actions = Vec::new();
    for req in requirements.into_values() {
        let Some(sample_legacy_id) = req.sample_legacy_id else {
            continue;
        };
        if let Some(target_mp) = find_matching_target_model_provider(
            &req.display_name,
            req.base_url.as_deref(),
            target_model_providers,
        ) {
            let existing_models: BTreeSet<&str> = target_mp
                .configured_models
                .iter()
                .filter(|m| m.enabled)
                .map(|m| m.id.as_str())
                .collect();
            let missing_models: Vec<String> = req
                .models
                .iter()
                .filter(|m| !existing_models.contains(m.as_str()))
                .cloned()
                .collect();
            let missing_agent_bindings: Vec<AgentId> = req
                .agents
                .iter()
                .filter(|agent_id| {
                    !target_bindings.iter().any(|binding| {
                        &binding.agent_id == *agent_id
                            && binding.model_provider_profile_id.as_ref() == Some(&target_mp.id)
                            && binding.legacy_provider_profile_id.is_some()
                    })
                })
                .cloned()
                .collect();
            if !missing_models.is_empty() || !missing_agent_bindings.is_empty() {
                actions.push(MissingProviderAction::UpdateProvider {
                    target_model_provider_id: target_mp.id.clone(),
                    display_name: target_mp.display_name.clone(),
                    missing_models,
                    missing_agent_bindings,
                });
            }
        } else {
            let legacy_matched_for_all = req.agents.iter().all(|agent_id| {
                target_legacy_profiles.iter().any(|summary| {
                    &summary.agent_id == agent_id
                        && summary
                            .display_name
                            .trim()
                            .eq_ignore_ascii_case(req.display_name.trim())
                })
            });
            if legacy_matched_for_all {
                continue;
            }
            let has_api_key = req
                .source_model_provider
                .as_ref()
                .is_some_and(|mp| !mp.credentials.is_empty());
            actions.push(MissingProviderAction::CreateProvider {
                source_model_provider_id: req
                    .source_model_provider
                    .as_ref()
                    .map(|mp| mp.id.clone()),
                source_legacy_profile_id: sample_legacy_id,
                display_name: req.display_name,
                base_url: req.base_url,
                models_to_configure: req.models.into_iter().collect(),
                agents_to_bind: req.agents.into_iter().collect(),
                has_api_key,
            });
        }
    }
    actions
}

pub(super) fn compute_target_agent_warnings(
    exported_sessions: &[ExportedCloneSession],
    target_agents: &[AgentSnapshotEntry],
) -> Vec<TargetAgentWarning> {
    let mut counts: BTreeMap<AgentId, usize> = BTreeMap::new();
    for session in exported_sessions {
        *counts.entry(session.agent_id.clone()).or_insert(0) += 1;
    }

    let mut warnings = Vec::new();
    for (agent_id, session_count) in counts {
        match target_agents.iter().find(|entry| entry.id == agent_id) {
            None => warnings.push(TargetAgentWarning {
                agent_label: agent_id.as_str().to_string(),
                agent_id,
                session_count,
                gap: AgentReadinessGapKind::Missing {
                    managed_installable: false,
                },
            }),
            Some(entry) if !entry.installed => warnings.push(TargetAgentWarning {
                agent_label: entry.label.clone(),
                agent_id,
                session_count,
                gap: AgentReadinessGapKind::Missing {
                    managed_installable: entry.managed_install.managed,
                },
            }),
            Some(entry) if !entry.enabled => warnings.push(TargetAgentWarning {
                agent_label: entry.label.clone(),
                agent_id,
                session_count,
                gap: AgentReadinessGapKind::Disabled,
            }),
            Some(_) => {}
        }
    }
    warnings
}

fn rewrite_url_port(server_url: &str, new_port: u16) -> String {
    let Some((scheme, rest)) = server_url.split_once("://") else {
        return server_url.to_string();
    };
    let (authority, path_tail) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let host = if let Some(bracket_end) = authority.find(']') {
        &authority[..=bracket_end]
    } else if let Some((host_part, _)) = authority.rsplit_once(':') {
        host_part
    } else {
        authority
    };
    format!("{scheme}://{host}:{new_port}{path_tail}")
}

fn allow_unsupported_or_permission<T>(
    result: Result<T, BackendError>,
    fallback: T,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(err)
            if matches!(
                err.kind,
                BackendErrorKind::Unsupported | BackendErrorKind::Permission
            ) =>
        {
            Ok(fallback)
        }
        Err(err) => Err(format!("{}: {}", err.code, err.message)),
    }
}

async fn connect_target_instance(
    instance: CloneTargetInstance,
) -> Result<ConnectedTargetState, String> {
    let (backend, ephemeral_lease) = match &instance.kind {
        CloneTargetKind::Local { runtime } => {
            let facade = Arc::new(NativeBackend::new(runtime.clone())).facade();
            (facade, None)
        }
        CloneTargetKind::Remote {
            registered,
            ssh_profile,
            reuse_active_tunnel,
        } => {
            let mut credential: DesktopRemoteCredential = registered.credential.clone();
            let mut tunnel_guard: Option<TunnelProcess> = None;
            if let Some(profile) = ssh_profile
                && !*reuse_active_tunnel
            {
                let program = profile
                    .ssh_client_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("ssh"));
                let destination = profile.ssh_destination();
                let command = SshCommand::for_destination(
                    program,
                    &destination,
                    profile.ssh_config_path.as_deref(),
                )
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
                let mut launch = LaunchConfig::new(
                    &profile.remote_home,
                    &profile.remote_install_root,
                    &profile.workspace_roots,
                    profile.remote_port,
                )
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
                if profile.macos_lifecycle == SshMacOsLifecycle::LaunchAgent {
                    launch = launch.with_launch_agent(
                        crate::ssh_connection::launchd::label_for_profile(&profile.id),
                    );
                }
                let launch = launch.with_runtime_home(profile.runtime_home_path());
                let start_command = command.clone();
                let start_launch = launch.clone();
                tokio::task::spawn_blocking(move || {
                    if let Some(label) = start_launch.launch_agent_label.as_deref() {
                        crate::ssh_connection::launchd::ensure_started(
                            &start_command,
                            &start_launch,
                            label,
                        )
                        .map(|_| ())
                        .map_err(|err| format!("{}: {}", err.code, err.message))
                    } else {
                        supervisor::ensure_started(&start_command, &start_launch)
                            .map(|_| ())
                            .map_err(|err| format!("{}: {}", err.code, err.message))
                    }
                })
                .await
                .map_err(|err| format!("ssh_start_task_failed: {err}"))??;
                let local_port = select_local_port(0, 1)
                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
                let spec = TunnelSpec::new(
                    command.program,
                    destination,
                    profile.ssh_config_path.clone(),
                    local_port,
                    profile.remote_port,
                )
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
                let mut tunnel = TunnelProcess::spawn(&spec)
                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !crate::ssh_connection::wsl::localhost_forwarded(local_port) {
                    if let Ok(Some(status)) = tunnel.try_wait() {
                        return Err(format!(
                            "ssh_tunnel_exited: SSH tunnel exited before forwarding became ready ({status})"
                        ));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "ssh_tunnel_timeout: SSH tunnel on 127.0.0.1:{local_port} did not become ready within 10s"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                credential.record.server_url =
                    rewrite_url_port(&credential.record.server_url, local_port);
                tunnel_guard = Some(tunnel);
            }
            let web_backend = credential
                .backend()
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
            web_backend
                .connect()
                .await
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
            let facade = web_backend.facade();
            let lease = Arc::new(EphemeralTargetLease {
                remote: Some(web_backend),
                _tunnel: tunnel_guard,
            });
            (facade, Some(lease))
        }
    };

    let workspaces = backend
        .workspace()
        .list_workspaces()
        .await
        .map_err(|err| format!("{}: {}", err.code, err.message))?;
    let agents = allow_unsupported_or_permission(
        backend
            .management()
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .await
            .map(|resp| resp.agents),
        Vec::new(),
    )?;
    let model_providers = allow_unsupported_or_permission(
        backend.management().list_model_provider_profiles().await,
        Vec::new(),
    )?;
    let bindings = allow_unsupported_or_permission(
        backend
            .management()
            .list_agent_model_provider_bindings(AgentModelProviderBindingListRequest {
                agent_id: None,
                model_provider_profile_id: None,
            })
            .await,
        Vec::new(),
    )?;
    let legacy_profiles =
        allow_unsupported_or_permission(backend.management().list_profiles().await, Vec::new())?;

    Ok(ConnectedTargetState {
        instance,
        backend,
        ephemeral_lease,
        workspaces,
        agents,
        model_providers,
        bindings,
        legacy_profiles,
    })
}

async fn load_source_clone_data(
    source_backend: BackendFacade,
    scope: CollectedCloneScope,
) -> Result<LoadedSourceCloneData, String> {
    let mut exported_sessions = Vec::with_capacity(scope.session_ids.len());
    let mut source_workspace_root = scope.source_workspace_root.clone();
    for chunk in scope.session_ids.chunks(MAX_CLONE_SESSIONS_PER_BUNDLE) {
        let bundle: SessionCloneBundle = source_backend
            .agent()
            .export_session_clone_bundle(SessionCloneExportRequest {
                session_ids: chunk.to_vec(),
                include_native_artifacts: true,
            })
            .await
            .map_err(|err| format!("{}: {}", err.code, err.message))?;
        if source_workspace_root.is_empty() && !bundle.source_workspace_root.is_empty() {
            source_workspace_root = bundle.source_workspace_root;
        }
        exported_sessions.extend(bundle.sessions);
    }

    let source_model_providers = allow_unsupported_or_permission(
        source_backend
            .management()
            .list_model_provider_profiles()
            .await,
        Vec::new(),
    )?;
    let source_bindings = allow_unsupported_or_permission(
        source_backend
            .management()
            .list_agent_model_provider_bindings(AgentModelProviderBindingListRequest {
                agent_id: None,
                model_provider_profile_id: None,
            })
            .await,
        Vec::new(),
    )?;
    let source_legacy_profiles = allow_unsupported_or_permission(
        source_backend.management().list_profiles().await,
        Vec::new(),
    )?;

    Ok(LoadedSourceCloneData {
        source_workspace_root,
        exported_sessions,
        source_model_providers,
        source_bindings,
        source_legacy_profiles,
    })
}

async fn map_and_sync_providers_for_clone(
    source_backend: &BackendFacade,
    source_data: &LoadedSourceCloneData,
    target_backend: &BackendFacade,
    target_state: &ConnectedTargetState,
    clone_missing_providers: bool,
) -> Result<
    (
        HashMap<(AgentId, ProviderProfileId), ProviderProfileId>,
        usize,
    ),
    String,
> {
    let mut target_model_providers = target_state.model_providers.clone();
    let mut target_bindings = target_state.bindings.clone();
    let target_legacy_profiles = target_state.legacy_profiles.clone();
    let mut synced_provider_count = 0usize;

    if clone_missing_providers {
        let actions = compute_missing_provider_actions(
            &source_data.exported_sessions,
            &source_data.source_model_providers,
            &source_data.source_bindings,
            &source_data.source_legacy_profiles,
            &target_model_providers,
            &target_bindings,
            &target_legacy_profiles,
        );

        for action in actions {
            match action {
                MissingProviderAction::CreateProvider {
                    source_model_provider_id,
                    source_legacy_profile_id: _,
                    display_name,
                    base_url,
                    models_to_configure,
                    agents_to_bind,
                    has_api_key,
                } => {
                    let source_mp = source_model_provider_id.as_ref().and_then(|id| {
                        source_data
                            .source_model_providers
                            .iter()
                            .find(|mp| &mp.id == id)
                    });
                    let create_req = if let Some(mp) = source_mp {
                        let mut configured_models = mp.configured_models.clone();
                        for model_id in &models_to_configure {
                            if !configured_models.iter().any(|m| &m.id == model_id) {
                                configured_models.push(ModelProviderCatalogEntry {
                                    id: model_id.clone(),
                                    display_name: None,
                                    enabled: true,
                                    metadata: Vec::new(),
                                    capabilities: Default::default(),
                                });
                            }
                        }
                        ModelProviderProfileCreateRequest {
                            display_name: mp.display_name.clone(),
                            vendor_hint: mp.vendor_hint.clone(),
                            endpoints: mp.endpoints.clone(),
                            proxy_policy: mp.proxy_policy.clone(),
                            credentials: Vec::new(),
                            configured_models,
                            default_model_id: mp.default_model_id.clone(),
                            headers: mp
                                .headers
                                .iter()
                                .filter(|header| {
                                    !matches!(
                                        header.value,
                                        ModelProviderHeaderValue::SecretReference(_)
                                    )
                                })
                                .cloned()
                                .collect(),
                            metadata: mp.metadata.clone(),
                            status: ModelProviderProfileStatus::Enabled,
                        }
                    } else {
                        let endpoints = base_url
                            .filter(|u| !u.trim().is_empty())
                            .map(|url| {
                                vec![vibex_core::ModelProviderEndpoint {
                                    id: "primary".to_string(),
                                    kind: vibex_core::ModelProviderEndpointKind::Api,
                                    url,
                                    wire_protocol_id: None,
                                }]
                            })
                            .unwrap_or_default();
                        let configured_models: Vec<ModelProviderCatalogEntry> = models_to_configure
                            .iter()
                            .map(|id| ModelProviderCatalogEntry {
                                id: id.clone(),
                                display_name: None,
                                enabled: true,
                                metadata: Vec::new(),
                                capabilities: Default::default(),
                            })
                            .collect();
                        let default_model_id = configured_models.first().map(|m| m.id.clone());
                        ModelProviderProfileCreateRequest {
                            display_name,
                            vendor_hint: None,
                            endpoints,
                            proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                            credentials: Vec::new(),
                            configured_models,
                            default_model_id,
                            headers: Vec::new(),
                            metadata: Default::default(),
                            status: ModelProviderProfileStatus::Enabled,
                        }
                    };

                    let created = target_backend
                        .management()
                        .create_model_provider_profile(MutationRequest::new(create_req))
                        .await
                        .map_err(|err| format!("{}: {}", err.code, err.message))?;

                    if has_api_key && let Some(mp) = source_mp {
                        for cred in &mp.credentials {
                            let secret_resp = source_backend
                                .management()
                                .get_model_provider_credential_secret(
                                    ModelProviderCredentialSecretRequest {
                                        model_provider_profile_id: mp.id.clone(),
                                        credential_id: cred.id.clone(),
                                    },
                                )
                                .await
                                .map_err(|err| format!("{}: {}", err.code, err.message))?;
                            if let Some(secret_value) =
                                secret_resp.value.filter(|v| !v.trim().is_empty())
                            {
                                target_backend
                                    .management()
                                    .add_model_provider_credential(MutationRequest::new(
                                        AddModelProviderCredentialRequest {
                                            model_provider_profile_id: created.id.clone(),
                                            display_name: (!cred.display_name.trim().is_empty())
                                                .then(|| cred.display_name.clone()),
                                            value: secret_value,
                                        },
                                    ))
                                    .await
                                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
                            }
                        }
                    }

                    for agent_id in &agents_to_bind {
                        target_backend
                            .management()
                            .bind_agent_model_provider(MutationRequest::new(
                                AgentModelProviderBindRequest {
                                    provider_profile_id: ProviderProfileId::new(),
                                    model_provider_profile_id: Some(created.id.clone()),
                                    agent_id: agent_id.clone(),
                                    make_default: false,
                                },
                            ))
                            .await
                            .map_err(|err| format!("{}: {}", err.code, err.message))?;
                    }
                    synced_provider_count += 1;
                }
                MissingProviderAction::UpdateProvider {
                    target_model_provider_id,
                    display_name: _,
                    missing_models,
                    missing_agent_bindings,
                } => {
                    if !missing_models.is_empty()
                        && let Some(existing) = target_model_providers
                            .iter()
                            .find(|mp| mp.id == target_model_provider_id)
                            .cloned()
                    {
                        let mut updated = existing;
                        let expected_revision = updated.revision;
                        for model_id in &missing_models {
                            if !updated.configured_models.iter().any(|m| &m.id == model_id) {
                                updated.configured_models.push(ModelProviderCatalogEntry {
                                    id: model_id.clone(),
                                    display_name: None,
                                    enabled: true,
                                    metadata: Vec::new(),
                                    capabilities: Default::default(),
                                });
                            }
                        }
                        target_backend
                            .management()
                            .update_model_provider_profile(MutationRequest::new(
                                ModelProviderProfileUpdateRequest {
                                    profile: updated,
                                    expected_revision,
                                },
                            ))
                            .await
                            .map_err(|err| format!("{}: {}", err.code, err.message))?;
                    }
                    for agent_id in &missing_agent_bindings {
                        target_backend
                            .management()
                            .bind_agent_model_provider(MutationRequest::new(
                                AgentModelProviderBindRequest {
                                    provider_profile_id: ProviderProfileId::new(),
                                    model_provider_profile_id: Some(
                                        target_model_provider_id.clone(),
                                    ),
                                    agent_id: agent_id.clone(),
                                    make_default: false,
                                },
                            ))
                            .await
                            .map_err(|err| format!("{}: {}", err.code, err.message))?;
                    }
                    synced_provider_count += 1;
                }
            }
        }

        if synced_provider_count > 0 {
            target_model_providers = target_backend
                .management()
                .list_model_provider_profiles()
                .await
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
            target_bindings = target_backend
                .management()
                .list_agent_model_provider_bindings(AgentModelProviderBindingListRequest {
                    agent_id: None,
                    model_provider_profile_id: None,
                })
                .await
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
        }
    }

    let refreshed_legacy = if synced_provider_count > 0 {
        allow_unsupported_or_permission(
            target_backend.management().list_profiles().await,
            target_legacy_profiles,
        )?
    } else {
        target_legacy_profiles
    };

    let mut provider_id_map: HashMap<(AgentId, ProviderProfileId), ProviderProfileId> =
        HashMap::new();
    for session in &source_data.exported_sessions {
        let Some(selection) = session.runtime_selection.as_ref() else {
            continue;
        };
        let RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } = &selection.auth_source
        else {
            continue;
        };
        let key = (selection.agent_id.clone(), provider_profile_id.clone());
        if provider_id_map.contains_key(&key) {
            continue;
        }

        let source_binding = source_data
            .source_bindings
            .iter()
            .find(|b| b.legacy_provider_profile_id.as_ref() == Some(provider_profile_id));
        let source_mp = source_binding
            .and_then(|b| b.model_provider_profile_id.as_ref())
            .and_then(|mp_id| {
                source_data
                    .source_model_providers
                    .iter()
                    .find(|mp| &mp.id == mp_id && mp.deleted_at_ms.is_none())
            });

        let display_name = source_mp
            .map(|mp| mp.display_name.as_str())
            .or_else(|| {
                session
                    .provider_hint
                    .as_ref()
                    .map(|h| h.display_name.as_str())
            })
            .or_else(|| {
                source_data
                    .source_legacy_profiles
                    .iter()
                    .find(|p| &p.id == provider_profile_id)
                    .map(|p| p.display_name.as_str())
            });
        let base_url = source_mp
            .and_then(|mp| mp.primary_api_endpoint().map(|ep| ep.url.as_str()))
            .or_else(|| {
                session
                    .provider_hint
                    .as_ref()
                    .and_then(|h| h.base_url.as_deref())
            });

        if let Some(name) = display_name {
            if let Some(target_mp) =
                find_matching_target_model_provider(name, base_url, &target_model_providers)
                && let Some(target_binding) = target_bindings.iter().find(|b| {
                    b.agent_id == selection.agent_id
                        && b.model_provider_profile_id.as_ref() == Some(&target_mp.id)
                        && b.legacy_provider_profile_id.is_some()
                })
                && let Some(target_legacy_id) = target_binding.legacy_provider_profile_id.clone()
            {
                provider_id_map.insert(key, target_legacy_id);
                continue;
            }
            if let Some(legacy_summary) = refreshed_legacy.iter().find(|p| {
                p.agent_id == selection.agent_id
                    && p.display_name.trim().eq_ignore_ascii_case(name.trim())
            }) {
                provider_id_map.insert(key, legacy_summary.id.clone());
            }
        }
    }

    Ok((provider_id_map, synced_provider_count))
}

fn map_session_runtime_selection_for_target(
    session: &ExportedCloneSession,
    provider_id_map: &HashMap<(AgentId, ProviderProfileId), ProviderProfileId>,
) -> Option<SessionRuntimeSelection> {
    let selection = session.runtime_selection.as_ref()?;
    match &selection.auth_source {
        RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } => {
            let target_id = provider_id_map
                .get(&(selection.agent_id.clone(), provider_profile_id.clone()))?
                .clone();
            let mut mapped = selection.clone();
            mapped.auth_source = RuntimeAuthSource::ProviderProfile {
                provider_profile_id: target_id,
            };
            Some(mapped)
        }
        RuntimeAuthSource::AgentAccount { .. } => Some(selection.clone()),
        RuntimeAuthSource::ModelRoute { .. } => None,
    }
}

pub(super) struct SessionCloneDialog {
    workbench: WeakEntity<VibexWorkbench>,
    locale_mode: LocaleMode,
    source_backend: BackendFacade,
    source_label: String,
    scope: CollectedCloneScope,
    targets: Vec<CloneTargetInstance>,
    selected_target_index: usize,
    phase: CloneDialogPhase,
    source_data: Option<Arc<LoadedSourceCloneData>>,
    connected_target: Option<ConnectedTargetState>,
    workspace_choice: TargetWorkspaceChoice,
    directory_input: Entity<InputState>,
    missing_provider_actions: Vec<MissingProviderAction>,
    clone_missing_providers: bool,
    agent_warnings: Vec<TargetAgentWarning>,
    completion: Option<CloneCompletionSummary>,
    body_scroll: ScrollHandle,
    active_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(super) struct SessionCloneDialogInit {
    pub workbench: WeakEntity<VibexWorkbench>,
    pub locale_mode: LocaleMode,
    pub source_backend: BackendFacade,
    pub source_label: String,
    pub scope: CollectedCloneScope,
    pub targets: Vec<CloneTargetInstance>,
}

impl SessionCloneDialog {
    pub(super) fn new(
        init: SessionCloneDialogInit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let SessionCloneDialogInit {
            workbench,
            locale_mode,
            source_backend,
            source_label,
            scope,
            targets,
        } = init;
        let initial_dir = scope.source_workspace_root.clone();
        let directory_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder(locale::text(
                "/path/to/workspace",
                "/目标工作区目录路径",
                "/目標工作區目錄路徑",
            ));
            input.set_value(initial_dir, window, cx);
            input
        });
        let sub = cx.subscribe_in(&directory_input, window, |_, _, _: &InputEvent, _, cx| {
            cx.notify();
        });

        let mut dialog = Self {
            workbench,
            locale_mode,
            source_backend,
            source_label,
            scope,
            targets,
            selected_target_index: 0,
            phase: CloneDialogPhase::LoadingSource,
            source_data: None,
            connected_target: None,
            workspace_choice: TargetWorkspaceChoice::Directory,
            directory_input,
            missing_provider_actions: Vec::new(),
            clone_missing_providers: true,
            agent_warnings: Vec::new(),
            completion: None,
            body_scroll: ScrollHandle::new(),
            active_task: None,
            _subscriptions: vec![sub],
        };
        dialog.start_initial_load(window, cx);
        dialog
    }

    fn start_initial_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source_backend = self.source_backend.clone();
        let scope = self.scope.clone();
        let first_target = self.targets.first().cloned();
        self.phase = CloneDialogPhase::LoadingSource;

        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let source_data = Arc::new(load_source_clone_data(source_backend, scope).await?);
            let target_state = match first_target {
                Some(target) => Some(connect_target_instance(target).await?),
                None => None,
            };
            Ok::<_, String>((source_data, target_state))
        });

        self.active_task = Some(cx.spawn_in(
            window,
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext| {
                let outcome = runner.await;
                let _ = entity.update_in(cx, |this, window, cx| {
                    match outcome {
                        Ok(Ok((source_data, target_state))) => {
                            if !source_data.source_workspace_root.is_empty() {
                                this.directory_input.update(cx, |input, cx| {
                                    if input.value().trim().is_empty() {
                                        input.set_value(
                                            source_data.source_workspace_root.clone(),
                                            window,
                                            cx,
                                        );
                                    }
                                });
                            }
                            this.source_data = Some(source_data);
                            if let Some(target) = target_state {
                                this.install_connected_target(target, window, cx);
                            } else {
                                this.phase = CloneDialogPhase::Error(
                                    locale::text(
                                        "No target runtime instance is available.",
                                        "没有可用的目标实例。",
                                        "沒有可用的目標執行個體。",
                                    )
                                    .to_string(),
                                );
                            }
                        }
                        Ok(Err(error)) => {
                            this.phase = CloneDialogPhase::Error(error);
                        }
                        Err(error) => {
                            this.phase = CloneDialogPhase::Error(error.to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn select_target(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.targets.len() || index == self.selected_target_index {
            return;
        }
        self.selected_target_index = index;
        self.connected_target = None;
        self.missing_provider_actions.clear();
        self.agent_warnings.clear();
        self.phase = CloneDialogPhase::ConnectingTarget;
        let target = self.targets[index].clone();

        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { connect_target_instance(target).await });
        self.active_task = Some(cx.spawn_in(
            window,
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext| {
                let outcome = runner.await;
                let _ = entity.update_in(cx, |this, window, cx| {
                    match outcome {
                        Ok(Ok(target_state)) => {
                            this.install_connected_target(target_state, window, cx);
                        }
                        Ok(Err(error)) => {
                            this.phase = CloneDialogPhase::Error(error);
                        }
                        Err(error) => {
                            this.phase = CloneDialogPhase::Error(error.to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
        cx.notify();
    }

    fn install_connected_target(
        &mut self,
        target: ConnectedTargetState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_root = self
            .source_data
            .as_ref()
            .map(|d| d.source_workspace_root.as_str())
            .unwrap_or(self.scope.source_workspace_root.as_str());
        let source_name = self.scope.source_project_name.trim();

        let matched_workspace = target
            .workspaces
            .iter()
            .find(|summary| !source_root.is_empty() && summary.workspace.root_path == source_root)
            .or_else(|| {
                target.workspaces.iter().find(|summary| {
                    !source_name.is_empty()
                        && summary
                            .project
                            .name
                            .trim()
                            .eq_ignore_ascii_case(source_name)
                })
            })
            .or_else(|| target.workspaces.first());

        if let Some(summary) = matched_workspace {
            self.workspace_choice = TargetWorkspaceChoice::Existing {
                workspace_id: summary.workspace.id.clone(),
                project_id: summary.project.id.clone(),
                project_name: summary.project.name.clone(),
                root_path: summary.workspace.root_path.clone(),
                mode: summary.workspace.mode,
            };
            self.directory_input.update(cx, |input, cx| {
                input.set_value(summary.workspace.root_path.clone(), window, cx);
            });
        } else {
            self.workspace_choice = TargetWorkspaceChoice::Directory;
        }

        if let Some(source_data) = self.source_data.as_ref() {
            self.missing_provider_actions = compute_missing_provider_actions(
                &source_data.exported_sessions,
                &source_data.source_model_providers,
                &source_data.source_bindings,
                &source_data.source_legacy_profiles,
                &target.model_providers,
                &target.bindings,
                &target.legacy_profiles,
            );
            self.agent_warnings =
                compute_target_agent_warnings(&source_data.exported_sessions, &target.agents);
        }

        self.clone_missing_providers = !self.missing_provider_actions.is_empty();
        self.connected_target = Some(target);
        self.phase = CloneDialogPhase::Ready;
    }

    fn resolved_target_directory(&self, cx: &App) -> Option<(String, WorkspaceMode)> {
        match &self.workspace_choice {
            TargetWorkspaceChoice::Existing {
                root_path, mode, ..
            } => {
                let trimmed = root_path.trim();
                (!trimmed.is_empty()).then(|| (trimmed.to_string(), *mode))
            }
            TargetWorkspaceChoice::Directory => {
                let value = self.directory_input.read(cx).value();
                let trimmed = value.trim();
                (!trimmed.is_empty())
                    .then(|| (trimmed.to_string(), self.scope.source_workspace_mode))
            }
        }
    }

    fn open_target_directory_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connected) = self.connected_target.as_ref() else {
            return;
        };
        let browse_source = match &connected.instance.kind {
            CloneTargetKind::Local { .. } => DirectoryBrowseTarget::Local,
            CloneTargetKind::Remote { .. } => {
                DirectoryBrowseTarget::Authority(connected.backend.clone())
            }
        };
        let current_val = self.directory_input.read(cx).value().trim().to_string();
        let initial_dir = (!current_val.is_empty()).then(|| PathBuf::from(current_val));
        let locale_mode = self.locale_mode;
        let dialog_weak = cx.weak_entity();
        let on_pick: DirectoryPickHandler = Arc::new(move |picked: String, window, cx| {
            dialog_weak
                .update(cx, |this, cx| {
                    this.workspace_choice = TargetWorkspaceChoice::Directory;
                    this.directory_input.update(cx, |input, cx| {
                        input.set_value(picked, window, cx);
                    });
                    cx.notify();
                })
                .is_ok()
        });
        let on_favorites_change: DirectoryFavoritesHandler = Arc::new(|_, _| {});
        let picker_view: Entity<DirectoryPickerDialog> = cx.new(|cx| {
            DirectoryPickerDialog::new(
                locale_mode,
                initial_dir,
                Vec::new(),
                browse_source,
                on_pick,
                on_favorites_change,
                window,
                cx,
            )
        });
        let title = locale::text(
            "Choose target workspace directory",
            "选择目标工作区目录",
            "選擇目標工作區目錄",
        );
        let viewport = window.viewport_size();
        let dialog_width = (f32::from(viewport.width) - 48.0).clamp(420.0, 640.0);
        let dialog_height = (f32::from(viewport.height) - 48.0).clamp(1.0, 520.0);
        let picker_weak = picker_view.downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let is_dark = cx.theme().is_dark();
            let popover = theme::semantic_color("popover", is_dark);
            let popover_foreground = theme::semantic_color("popover-foreground", is_dark);
            let footer = picker_weak
                .update(cx, |picker, cx| picker.render_footer(cx).into_any_element())
                .unwrap_or_else(|_| div().into_any_element());
            let content_view = picker_view.clone();
            dialog
                .title(title)
                .w(px(dialog_width))
                .max_w(px(dialog_width))
                .h(px(dialog_height))
                .rounded(px(14.0))
                .bg(popover)
                .text_color(popover_foreground)
                .border_color(popover_foreground.opacity(0.10))
                .keyboard(false)
                .overlay(true)
                .overlay_closable(true)
                .content(move |content, _, _| {
                    content
                        .min_h_0()
                        .overflow_hidden()
                        .pt(px(4.0))
                        .child(content_view.clone())
                })
                .footer(footer)
        });
    }

    fn execute_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != CloneDialogPhase::Ready {
            return;
        }
        let (Some(source_data), Some(connected)) =
            (self.source_data.clone(), self.connected_target.as_ref())
        else {
            return;
        };
        let Some((target_root, target_mode)) = self.resolved_target_directory(cx) else {
            return;
        };
        let open_new_directory = matches!(self.workspace_choice, TargetWorkspaceChoice::Directory);
        let source_backend = self.source_backend.clone();
        let target_backend = connected.backend.clone();
        let target_instance = connected.instance.clone();
        let target_snapshot = ConnectedTargetState {
            instance: connected.instance.clone(),
            backend: connected.backend.clone(),
            ephemeral_lease: connected.ephemeral_lease.clone(),
            workspaces: connected.workspaces.clone(),
            agents: connected.agents.clone(),
            model_providers: connected.model_providers.clone(),
            bindings: connected.bindings.clone(),
            legacy_profiles: connected.legacy_profiles.clone(),
        };
        let clone_missing_providers =
            self.clone_missing_providers && !self.missing_provider_actions.is_empty();
        let scope = self.scope.clone();
        let agent_warnings = self.agent_warnings.clone();
        let workbench = self.workbench.clone();

        self.phase = CloneDialogPhase::Cloning;
        cx.notify();

        let scope_for_runner = scope.clone();
        let target_is_remote = target_instance.id != LOCAL_RUNTIME_ID;
        let attempt_id = vibex_core::RequestId::new().to_string();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let _ephemeral_lease = target_snapshot.ephemeral_lease.clone();
            let confirmed_root = if open_new_directory {
                let opened = target_backend
                    .workspace()
                    .open_workspace(
                        MutationRequest::new(OpenWorkspaceRequest {
                            root_path: target_root.clone(),
                            mode: Some(target_mode),
                        })
                        .with_idempotency_key(format!("clone-{attempt_id}-open")),
                    )
                    .await
                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
                opened.workspace.root_path
            } else {
                target_root
            };

            let (provider_id_map, cloned_provider_count) = map_and_sync_providers_for_clone(
                &source_backend,
                &source_data,
                &target_backend,
                &target_snapshot,
                clone_missing_providers,
            )
            .await?;

            let entries: Vec<ImportCloneSessionEntry> = source_data
                .exported_sessions
                .iter()
                .cloned()
                .map(|session| {
                    let mapped_runtime_selection =
                        map_session_runtime_selection_for_target(&session, &provider_id_map);
                    ImportCloneSessionEntry {
                        session,
                        mapped_runtime_selection,
                    }
                })
                .collect();

            let mut target_project_id: Option<ProjectId> = None;
            let mut target_workspace_id: Option<WorkspaceId> = None;
            let mut session_id_map: HashMap<String, String> = HashMap::new();
            let mut first_target_session_id: Option<VibexSessionId> = None;
            let mut imported_session_count = 0usize;
            let mut restorable_native_count = 0usize;

            for (chunk_idx, chunk) in entries.chunks(MAX_CLONE_SESSIONS_PER_BUNDLE).enumerate() {
                let result = target_backend
                    .agent()
                    .import_session_clone_bundle(
                        MutationRequest::new(SessionCloneImportRequest {
                            source_workspace_root: source_data.source_workspace_root.clone(),
                            target_workspace_root: confirmed_root.clone(),
                            workspace_mode: target_mode,
                            sessions: chunk.to_vec(),
                        })
                        .with_idempotency_key(format!("clone-{attempt_id}-chunk-{chunk_idx}")),
                    )
                    .await
                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
                target_project_id = Some(result.target_project_id);
                target_workspace_id = Some(result.target_workspace_id);
                for mapping in result.imported_sessions {
                    if first_target_session_id.is_none() {
                        first_target_session_id = Some(mapping.target_session.id.clone());
                    }
                    if mapping.native_state_restorable {
                        restorable_native_count += 1;
                    }
                    imported_session_count += 1;
                    session_id_map.insert(
                        mapping.source_session_id.as_str().to_string(),
                        mapping.target_session.id.as_str().to_string(),
                    );
                }
            }

            let mirrored_ids = if target_is_remote
                && let (Some(pid), Some(wid)) = (&target_project_id, &target_workspace_id)
            {
                mirror_cloned_organization_to_remote_sidebar(
                    &target_backend,
                    &scope_for_runner,
                    pid,
                    wid,
                    &session_id_map,
                )
                .await
            } else {
                RemoteMirroredIds::default()
            };

            Ok::<_, String>((
                target_project_id,
                target_workspace_id,
                confirmed_root,
                session_id_map,
                mirrored_ids,
                first_target_session_id,
                imported_session_count,
                restorable_native_count,
                cloned_provider_count,
            ))
        });

        self.active_task = Some(cx.spawn_in(
            window,
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext| {
                let outcome = runner.await;
                let _ = entity.update_in(cx, |this, _, cx| {
                    match outcome {
                        Ok(Ok((
                            Some(target_project_id),
                            Some(target_workspace_id),
                            confirmed_root,
                            session_id_map,
                            mirrored_ids,
                            first_target_session_id,
                            imported_session_count,
                            restorable_native_count,
                            cloned_provider_count,
                        ))) => {
                            let authority_key = target_instance.authority_key.clone();
                            let now_ms = unix_timestamp_ms();
                            let _ = workbench.update(cx, |wb, cx| {
                                wb.apply_cloned_organization_to_sidebar(ClonedSidebarApply {
                                    authority_key: &authority_key,
                                    scope: &scope,
                                    target_project_id: &target_project_id,
                                    target_workspace_id: &target_workspace_id,
                                    session_id_map: &session_id_map,
                                    mirrored_ids: &mirrored_ids,
                                    now_ms,
                                });
                                cx.notify();
                            });

                            this.completion = Some(CloneCompletionSummary {
                                target_runtime_id: target_instance.id.clone(),
                                target_label: target_instance.label.clone(),
                                target_workspace_root: confirmed_root,
                                imported_session_count,
                                restorable_native_count,
                                cloned_provider_count,
                                first_target_session_id,
                                agent_warnings,
                            });
                            this.phase = CloneDialogPhase::Completed;
                        }
                        Ok(Ok(_)) => {
                            this.phase = CloneDialogPhase::Error(
                                locale::text(
                                    "Clone completed without returning a target workspace.",
                                    "克隆未返回有效的目标工作区。",
                                    "複製未傳回有效的目標工作區。",
                                )
                                .to_string(),
                            );
                        }
                        Ok(Err(error)) => {
                            this.phase = CloneDialogPhase::Error(error);
                        }
                        Err(error) => {
                            this.phase = CloneDialogPhase::Error(error.to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    pub(super) fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let can_submit = self.phase == CloneDialogPhase::Ready
            && self.connected_target.is_some()
            && self.resolved_target_directory(cx).is_some();

        match &self.phase {
            CloneDialogPhase::Completed => {
                let completion = self.completion.clone();
                let workbench = self.workbench.clone();
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().text_color(muted).child(locale::text(
                        "Sessions are ready on the target instance.",
                        "会话已就绪，可前往目标实例继续。",
                        "會話已就緒，可前往目標執行個體繼續。",
                    )))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("session-clone-close-done")
                                    .small()
                                    .ghost()
                                    .label(locale::text("Close", "关闭", "關閉"))
                                    .on_click(|_, window, cx| {
                                        window.close_dialog(cx);
                                    }),
                            )
                            .when_some(completion, |row, summary| {
                                let target_id = summary.target_runtime_id.clone();
                                let first_session = summary.first_target_session_id.clone();
                                let switch_label = format!(
                                    "{} {}",
                                    locale::text("Switch to", "切换到", "切換到"),
                                    summary.target_label
                                );
                                row.child(
                                    Button::new("session-clone-switch-open")
                                        .small()
                                        .primary()
                                        .label(switch_label)
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = workbench.update(cx, |wb, cx| {
                                                wb.switch_to_cloned_runtime_and_reveal(
                                                    target_id.clone(),
                                                    first_session.clone(),
                                                    cx,
                                                );
                                            });
                                        }),
                                )
                            }),
                    )
            }
            _ => {
                let cloning = self.phase == CloneDialogPhase::Cloning;
                let session_count = self.scope.session_ids.len();
                let status_label = match &self.phase {
                    CloneDialogPhase::LoadingSource => locale::text(
                        "Reading source sessions…",
                        "正在读取源会话数据…",
                        "正在讀取來源會話資料…",
                    )
                    .to_string(),
                    CloneDialogPhase::ConnectingTarget => locale::text(
                        "Connecting to target instance…",
                        "正在连接目标实例…",
                        "正在連線目標執行個體…",
                    )
                    .to_string(),
                    CloneDialogPhase::Cloning => locale::text(
                        "Cloning sessions & configurations…",
                        "正在克隆会话与配置…",
                        "正在複製會話與設定…",
                    )
                    .to_string(),
                    _ => format!(
                        "{} {}",
                        session_count,
                        locale::text(
                            "Agent session(s) in 1 project",
                            "个同项目 Agent 会话",
                            "個同專案 Agent 會話",
                        )
                    ),
                };

                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().text_color(muted).child(status_label))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("session-clone-cancel")
                                    .small()
                                    .ghost()
                                    .disabled(cloning)
                                    .label(locale::text("Cancel", "取消", "取消"))
                                    .on_click(|_, window, cx| {
                                        window.close_dialog(cx);
                                    }),
                            )
                            .child(
                                Button::new("session-clone-confirm")
                                    .small()
                                    .primary()
                                    .loading(cloning)
                                    .disabled(!can_submit)
                                    .label(locale::text("Start Clone", "开始克隆", "開始複製"))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.execute_clone(window, cx);
                                    })),
                            ),
                    )
            }
        }
    }

    fn render_source_summary_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let card_bg = cx.theme().secondary.opacity(0.35);

        let kind_badge = match self.scope.kind {
            CloneSourceKind::Project => locale::text("Project", "项目", "專案"),
            CloneSourceKind::Folder => locale::text("Folder", "文件夹", "資料夾"),
            CloneSourceKind::Group => locale::text("Session Group", "会话组", "會話組"),
            CloneSourceKind::Sessions => locale::text("Session", "会话", "會話"),
        };

        let mut counters = vec![format!(
            "{} {}",
            self.scope.session_ids.len(),
            locale::text("sessions", "个会话", "個會話")
        )];
        if !self.scope.groups.is_empty() {
            counters.push(format!(
                "{} {}",
                self.scope.groups.len(),
                locale::text("groups", "个会话组", "個會話組")
            ));
        }
        if !self.scope.folders.is_empty() {
            counters.push(format!(
                "{} {}",
                self.scope.folders.len(),
                locale::text("folders", "个文件夹", "個資料夾")
            ));
        }

        v_flex()
            .w_full()
            .gap_1p5()
            .rounded(px(10.0))
            .border_1()
            .border_color(border.opacity(0.80))
            .bg(card_bg)
            .p_3()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(px(5.0))
                                    .bg(cx.theme().primary.opacity(0.12))
                                    .text_color(cx.theme().primary)
                                    .text_xs()
                                    .child(kind_badge),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(fg)
                                    .truncate()
                                    .child(self.scope.display_label.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .flex_none()
                            .child(counters.join(" · ")),
                    ),
            )
            .child(div().text_xs().text_color(muted).truncate().child(format!(
                "{}: {} · {}: {}",
                locale::text("From", "源实例", "來源執行個體"),
                self.source_label,
                locale::text("Project", "所属项目", "所屬專案"),
                self.scope.source_project_name
            )))
            .into_any_element()
    }

    fn render_target_selector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let selected_label = self
            .targets
            .get(self.selected_target_index)
            .map(|t| t.label.clone())
            .unwrap_or_default();
        let targets_snapshot: Vec<(usize, String)> = self
            .targets
            .iter()
            .enumerate()
            .map(|(idx, t)| (idx, t.label.clone()))
            .collect();
        let selected_idx = self.selected_target_index;
        let dialog_entity = cx.weak_entity();
        let disabled = self.phase == CloneDialogPhase::Cloning;

        v_flex()
            .w_full()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(fg)
                    .child(locale::text(
                        "1. Target Instance",
                        "1. 目标设备实例",
                        "1. 目標裝置執行個體",
                    )),
            )
            .child(
                Button::new("session-clone-target-dropdown")
                    .small()
                    .secondary()
                    .w_full()
                    .justify_between()
                    .disabled(disabled)
                    .icon(sidebar_icon("icons/vibex/monitor.svg"))
                    .label(selected_label)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (idx, label) in targets_snapshot.clone() {
                            let pick_entity = dialog_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(idx == selected_idx)
                                    .on_click(move |_, window, cx| {
                                        let _ = pick_entity.update(cx, |this, cx| {
                                            this.select_target(idx, window, cx);
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(locale::text(
                        "Cloned sessions preserve conversation history and prompt cache prefixes on the selected instance.",
                        "克隆后完整保留会话历史与原生上下文状态，复用大模型 Prompt 命中缓存。",
                        "複製後完整保留會話歷史與原生上下文狀態，重用大模型 Prompt 命中快取。",
                    )),
            )
            .into_any_element()
    }

    fn render_workspace_confirmation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let disabled = self.phase != CloneDialogPhase::Ready;

        let existing_workspaces: Vec<WorkspaceSummary> = self
            .connected_target
            .as_ref()
            .map(|t| t.workspaces.clone())
            .unwrap_or_default();
        let is_existing_mode = matches!(
            self.workspace_choice,
            TargetWorkspaceChoice::Existing { .. }
        );
        let current_existing_label = match &self.workspace_choice {
            TargetWorkspaceChoice::Existing {
                project_name,
                root_path,
                ..
            } => format!("{project_name} ({root_path})"),
            TargetWorkspaceChoice::Directory => locale::text(
                "Choose an existing project…",
                "选择已有项目…",
                "選擇現有專案…",
            )
            .to_string(),
        };
        let selected_workspace_id = match &self.workspace_choice {
            TargetWorkspaceChoice::Existing { workspace_id, .. } => Some(workspace_id.clone()),
            TargetWorkspaceChoice::Directory => None,
        };
        let confirmed_dir = self
            .resolved_target_directory(cx)
            .map(|(path, _)| path)
            .unwrap_or_default();

        let first_existing = existing_workspaces.first().cloned();
        let dialog_entity = cx.weak_entity();

        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(fg)
                    .child(locale::text(
                        "2. Target Project / Workspace Directory",
                        "2. 目标项目与工作区目录确认",
                        "2. 目標專案與工作區目錄確認",
                    )),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("session-clone-ws-mode-existing")
                            .xsmall()
                            .when(is_existing_mode, |b| b.primary())
                            .when(!is_existing_mode, |b| b.secondary())
                            .disabled(disabled || existing_workspaces.is_empty())
                            .label(locale::text(
                                "Existing Project",
                                "选择已有项目",
                                "選擇現有專案",
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(summary) = first_existing.as_ref() {
                                    this.workspace_choice = TargetWorkspaceChoice::Existing {
                                        workspace_id: summary.workspace.id.clone(),
                                        project_id: summary.project.id.clone(),
                                        project_name: summary.project.name.clone(),
                                        root_path: summary.workspace.root_path.clone(),
                                        mode: summary.workspace.mode,
                                    };
                                    this.directory_input.update(cx, |input, cx| {
                                        input.set_value(
                                            summary.workspace.root_path.clone(),
                                            window,
                                            cx,
                                        );
                                    });
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("session-clone-ws-mode-dir")
                            .xsmall()
                            .when(!is_existing_mode, |b| b.primary())
                            .when(is_existing_mode, |b| b.secondary())
                            .disabled(disabled)
                            .label(locale::text(
                                "Choose Directory",
                                "指定工作区目录",
                                "指定工作區目錄",
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.workspace_choice = TargetWorkspaceChoice::Directory;
                                cx.notify();
                            })),
                    ),
            )
            .when(is_existing_mode, |col| {
                let items = existing_workspaces.clone();
                let selected_ws = selected_workspace_id.clone();
                col.child(
                    Button::new("session-clone-existing-project-dropdown")
                        .small()
                        .secondary()
                        .w_full()
                        .justify_between()
                        .disabled(disabled)
                        .label(current_existing_label)
                        .dropdown_menu(move |mut menu, _, _| {
                            for summary in items.clone() {
                                let pick_entity = dialog_entity.clone();
                                let ws_id = summary.workspace.id.clone();
                                let proj_id = summary.project.id.clone();
                                let proj_name = summary.project.name.clone();
                                let root_path = summary.workspace.root_path.clone();
                                let mode = summary.workspace.mode;
                                let label = format!("{proj_name} ({root_path})");
                                let checked = selected_ws.as_ref() == Some(&ws_id);
                                menu =
                                    menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                        move |_, window, cx| {
                                            let _ = pick_entity.update(cx, |this, cx| {
                                                this.workspace_choice =
                                                    TargetWorkspaceChoice::Existing {
                                                        workspace_id: ws_id.clone(),
                                                        project_id: proj_id.clone(),
                                                        project_name: proj_name.clone(),
                                                        root_path: root_path.clone(),
                                                        mode,
                                                    };
                                                this.directory_input.update(cx, |input, cx| {
                                                    input.set_value(root_path.clone(), window, cx);
                                                });
                                                cx.notify();
                                            });
                                        },
                                    ));
                            }
                            menu
                        }),
                )
            })
            .when(!is_existing_mode, |col| {
                col.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.directory_input).small()),
                        )
                        .child(
                            Button::new("session-clone-browse-target-dir")
                                .small()
                                .secondary()
                                .disabled(disabled)
                                .icon(IconName::FolderOpen)
                                .label(locale::text("Browse…", "浏览…", "瀏覽…"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_target_directory_picker(window, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(border.opacity(0.65))
                    .px_2p5()
                    .py_1p5()
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(if confirmed_dir.is_empty() {
                        locale::text(
                            "Please select or enter a target workspace directory.",
                            "请选择已有项目或输入目标实例上的工作区目录。",
                            "請選擇現有專案或輸入目標執行個體上的工作區目錄。",
                        )
                        .to_string()
                    } else {
                        format!(
                            "{}: {confirmed_dir}",
                            locale::text(
                                "Confirmed workspace directory",
                                "已确认工作区目录",
                                "已確認工作區目錄",
                            )
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_provider_diff_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.missing_provider_actions.is_empty() {
            return None;
        }
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let checked = self.clone_missing_providers;
        let disabled = self.phase != CloneDialogPhase::Ready;

        let rows: Vec<AnyElement> = self
            .missing_provider_actions
            .iter()
            .map(|action| match action {
                MissingProviderAction::CreateProvider {
                    display_name,
                    models_to_configure,
                    has_api_key,
                    ..
                } => {
                    let detail = format!(
                        "{} · {} {}{}",
                        locale::text("New Provider", "新增供应商", "新增供應商"),
                        models_to_configure.len(),
                        locale::text("model(s)", "个模型", "個模型"),
                        if *has_api_key {
                            locale::text(" · includes API Key", " · 含 API Key", " · 含 API Key")
                        } else {
                            ""
                        }
                    );
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .font_medium()
                                .text_color(fg)
                                .child(display_name.clone()),
                        )
                        .child(div().text_color(muted).child(detail))
                        .into_any_element()
                }
                MissingProviderAction::UpdateProvider {
                    display_name,
                    missing_models,
                    missing_agent_bindings,
                    ..
                } => {
                    let detail = format!(
                        "+{} {} · +{} {}",
                        missing_models.len(),
                        locale::text("model(s)", "个模型", "個模型"),
                        missing_agent_bindings.len(),
                        locale::text("Agent binding(s)", "项 Agent 绑定", "項 Agent 綁定"),
                    );
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .font_medium()
                                .text_color(fg)
                                .child(display_name.clone()),
                        )
                        .child(div().text_color(muted).child(detail))
                        .into_any_element()
                }
            })
            .collect();

        Some(
            v_flex()
                .w_full()
                .gap_2()
                .rounded(px(10.0))
                .border_1()
                .border_color(border.opacity(0.80))
                .p_3()
                .child(
                    Checkbox::new("session-clone-sync-providers")
                        .checked(checked)
                        .disabled(disabled)
                        .label(locale::text(
                            "Also clone missing Provider & Model configurations to target instance",
                            "同时将目标实例缺失的供应商与模型配置（含密钥）克隆过去",
                            "同時將目標執行個體缺失的供應商與模型設定（含金鑰）複製過去",
                        ))
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.clone_missing_providers = *checked;
                            cx.notify();
                        })),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_agent_warnings_section(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.agent_warnings.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .agent_warnings
            .iter()
            .map(|warning| {
                let state_text = match warning.gap {
                    AgentReadinessGapKind::Missing { .. } => {
                        locale::text("not installed", "未安装", "未安裝")
                    }
                    AgentReadinessGapKind::Disabled => {
                        locale::text("not enabled", "未启用", "未啟用")
                    }
                };
                format!(
                    "{} ({state_text} · {} {})",
                    warning.agent_label,
                    warning.session_count,
                    locale::text("session(s)", "个会话", "個會話")
                )
            })
            .collect();

        let message = format!(
            "{}: {}. {}",
            locale::text(
                "Target instance has unready Agent(s)",
                "目标实例尚未启用以下 Agent",
                "目標執行個體尚未啟用以下 Agent",
            ),
            lines.join(", "),
            locale::text(
                "Sessions will still be cloned; you can install/enable the Agent or switch to another Agent inside the cloned session.",
                "会话仍将正常克隆；打开克隆后的会话时将提示一键安装启用或切换其他 Agent。",
                "會話仍將正常複製；開啟複製後的會話時將提示一鍵安裝啟用或切換其他 Agent。",
            )
        );

        Some(
            Alert::warning("session-clone-agent-warnings", message)
                .title(locale::text(
                    "Agent Readiness Notice",
                    "Agent 可用性提示",
                    "Agent 可用性提示",
                ))
                .into_any_element(),
        )
    }

    fn render_completed_view(
        &self,
        summary: &CloneCompletionSummary,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        let mut details = vec![format!(
            "{}: {} ({})",
            locale::text("Target", "目标实例", "目標執行個體"),
            summary.target_label,
            summary.target_workspace_root
        )];
        details.push(format!(
            "{}: {} ({}: {})",
            locale::text("Cloned sessions", "已克隆会话", "已複製會話"),
            summary.imported_session_count,
            locale::text(
                "native prompt-cache state preserved",
                "已同步原生缓存上下文",
                "已同步原生快取上下文",
            ),
            summary.restorable_native_count
        ));
        if summary.cloned_provider_count > 0 {
            details.push(format!(
                "{}: {}",
                locale::text("Synced Providers", "已同步供应商配置", "已同步供應商設定",),
                summary.cloned_provider_count
            ));
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Alert::success(
                    "session-clone-completed-alert",
                    locale::text(
                        "Cross-instance clone finished.",
                        "跨实例克隆已完成。",
                        "跨執行個體複製已完成。",
                    ),
                )
                .title(locale::text("Clone Complete", "克隆完成", "複製完成")),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(border.opacity(0.80))
                    .p_3()
                    .children(
                        details
                            .into_iter()
                            .map(|line| div().text_xs().text_color(fg).child(line)),
                    ),
            )
            .when(!summary.agent_warnings.is_empty(), |col| {
                let names: Vec<String> = summary
                    .agent_warnings
                    .iter()
                    .map(|w| w.agent_label.clone())
                    .collect();
                col.child(div().text_xs().text_color(muted).child(format!(
                    "{}: {}",
                    locale::text(
                        "Reminder — install/enable or switch Agent on target before sending",
                        "提示 — 在目标实例发送消息前可一键启用或切换以下 Agent",
                        "提示 — 在目標執行個體傳送訊息前可一鍵啟用或切換以下 Agent",
                    ),
                    names.join(", ")
                )))
            })
            .into_any_element()
    }
}

impl gpui::Render for SessionCloneDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let body_content: AnyElement =
            match &self.phase.clone() {
                CloneDialogPhase::Completed => {
                    if let Some(summary) = self.completion.as_ref() {
                        self.render_completed_view(summary, cx)
                    } else {
                        div().into_any_element()
                    }
                }
                CloneDialogPhase::Error(message) => v_flex()
                    .w_full()
                    .gap_3()
                    .child(self.render_source_summary_card(cx))
                    .child(self.render_target_selector(cx))
                    .child(
                        Alert::error("session-clone-error-alert", message.clone())
                            .title(locale::text("Clone Failed", "克隆失败", "複製失敗")),
                    )
                    .into_any_element(),
                CloneDialogPhase::LoadingSource | CloneDialogPhase::ConnectingTarget => v_flex()
                    .w_full()
                    .gap_3()
                    .child(self.render_source_summary_card(cx))
                    .child(self.render_target_selector(cx))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .py_6()
                            .text_sm()
                            .text_color(muted)
                            .child(Spinner::new().small())
                            .child(if self.phase == CloneDialogPhase::LoadingSource {
                                locale::text(
                                    "Preparing sessions and provider configurations…",
                                    "正在读取会话与供应商配置…",
                                    "正在讀取會話與供應商設定…",
                                )
                            } else {
                                locale::text(
                                    "Inspecting target instance workspaces, providers, and Agents…",
                                    "正在检查目标实例的工作区、供应商与 Agent 状态…",
                                    "正在檢查目標執行個體的工作區、供應商與 Agent 狀態…",
                                )
                            }),
                    )
                    .into_any_element(),
                CloneDialogPhase::Ready | CloneDialogPhase::Cloning => {
                    let provider_section = self.render_provider_diff_section(cx);
                    let agent_section = self.render_agent_warnings_section(cx);
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(self.render_source_summary_card(cx))
                        .child(self.render_target_selector(cx))
                        .child(self.render_workspace_confirmation(cx))
                        .when_some(provider_section, |col, el| col.child(el))
                        .when_some(agent_section, |col, el| col.child(el))
                        .into_any_element()
                }
            };

        div()
            .id("session-clone-dialog-scroll")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.body_scroll)
            .scroll_gutter()
            .child(v_flex().w_full().gap_3().pb_2().child(body_content))
            .pinned_scrollbar(&self.body_scroll, ScrollbarAxis::Vertical)
    }
}

pub(super) fn extract_cloned_preferred_runtime_selection_from_vibex_error(
    error: &vibex_core::VibexError,
) -> Option<SessionRuntimeSelection> {
    error
        .diagnostics
        .iter()
        .find(|d| d.key == CLONED_PREFERRED_RUNTIME_SELECTION_DIAGNOSTIC)
        .and_then(|d| serde_json::from_str::<SessionRuntimeSelection>(&d.value).ok())
}

pub(super) fn extract_cloned_preferred_runtime_selection_from_backend_error(
    error: &BackendError,
) -> Option<SessionRuntimeSelection> {
    error
        .diagnostics
        .iter()
        .find(|d| d.key == CLONED_PREFERRED_RUNTIME_SELECTION_DIAGNOSTIC)
        .and_then(|d| serde_json::from_str::<SessionRuntimeSelection>(&d.value).ok())
}

impl VibexWorkbench {
    /// True whenever the user has >= 2 runtime authorities available in the
    /// desktop registry (e.g. embedded `local` + >= 1 paired remote, or >= 2
    /// paired remotes).
    pub(super) fn has_clone_target_instances(&self) -> bool {
        let active_id = self.runtime_registry.active_runtime_id();
        self.runtime_registry
            .runtimes
            .iter()
            .any(|runtime| runtime.id != active_id)
            || (active_id != LOCAL_RUNTIME_ID && self.runtime.is_some())
    }

    pub(super) fn apply_cloned_organization_to_sidebar(&mut self, params: ClonedSidebarApply<'_>) {
        let ClonedSidebarApply {
            authority_key,
            scope,
            target_project_id,
            target_workspace_id,
            session_id_map,
            mirrored_ids,
            now_ms,
        } = params;
        let is_active_authority = self.ui_state.sidebar.active_authority() == authority_key;
        if is_active_authority {
            self.queue_agent_ui_state();
        }
        let mut created_group_ids = BTreeSet::new();
        self.ui_state
            .sidebar
            .mutate_arrangement_for(authority_key, |arrangement| {
                created_group_ids = merge_cloned_organization_into_arrangement(
                    arrangement,
                    scope,
                    target_project_id,
                    target_workspace_id,
                    session_id_map,
                    mirrored_ids,
                    now_ms,
                );
            });

        if authority_key == SidebarUiState::LOCAL_AUTHORITY
            && !created_group_ids.is_empty()
            && let Some(runtime) = self.runtime.clone()
        {
            let revision = self
                .sidebar_organization_view_for(SidebarUiState::LOCAL_AUTHORITY)
                .revision;
            self.ui_state.sidebar.mutate_arrangement_for(
                SidebarUiState::LOCAL_AUTHORITY,
                |arrangement| {
                    for group_id in &created_group_ids {
                        let Some(group) = arrangement.organization.group_mut(group_id) else {
                            continue;
                        };
                        group.disable_automatic_layout();
                        let name = group.name.clone();
                        let members = group
                            .member_session_ids
                            .iter()
                            .filter_map(|id| VibexSessionId::parse(id).ok())
                            .collect::<Vec<_>>();
                        let applied = group.observed_layout();
                        let layout = vibex_core::SessionGroupLayoutIntent {
                            preset: applied.preset,
                            lead_session_ref: applied.visible_session_refs.first().cloned(),
                            preferred_live_panes: Some(applied.live_panes.max(1)),
                        };
                        let _ = runtime
                            .vibex_use()
                            .sync_user_group(group_id, &name, &members, &layout, revision);
                    }
                },
            );
        }

        if is_active_authority {
            self.sidebar_state.row_order = self.ui_state.sidebar.session_order.clone();
            self.sidebar_state.pinned_ids = self.ui_state.sidebar.pinned_session_ids.clone();
            self.sidebar_state.collapsed_ids = self.ui_state.sidebar.collapsed_project_ids.clone();
            self.invalidate_sidebar_projection_cache();
            self.publish_sidebar_invalidation();
            self.queue_agent_ui_state();
        } else {
            self.queue_ui_state();
        }
    }

    pub(super) fn active_instance_display_label(&self) -> String {
        if self.runtime_registry.is_local_active() {
            self.local_runtime_label()
        } else {
            self.runtime_registry
                .active_remote()
                .map(RegisteredRuntime::display_label)
                .unwrap_or_else(|| self.local_runtime_label())
        }
    }

    pub(super) fn available_clone_target_instances(&mut self) -> Vec<CloneTargetInstance> {
        let active_id = self.runtime_registry.active_runtime_id().to_string();
        let remotes: Vec<RegisteredRuntime> = self.runtime_registry.runtimes.clone();
        let mut targets = Vec::new();

        for remote in remotes {
            if remote.id == active_id {
                continue;
            }
            let ssh_profile = self.ssh_profile_for_runtime(&remote.id);
            let reuse_active_tunnel = ssh_profile
                .as_ref()
                .is_some_and(|profile| self.ssh_tunnel_reaches_profile(profile));
            targets.push(CloneTargetInstance {
                id: remote.id.clone(),
                label: remote.display_label(),
                authority_key: remote_authority_key(&remote.credential),
                kind: CloneTargetKind::Remote {
                    registered: Box::new(remote),
                    ssh_profile: ssh_profile.map(Box::new),
                    reuse_active_tunnel,
                },
            });
        }

        if active_id != LOCAL_RUNTIME_ID
            && let Some(local_runtime) = self.runtime.clone()
        {
            targets.push(CloneTargetInstance {
                id: LOCAL_RUNTIME_ID.to_string(),
                label: self.local_runtime_label(),
                authority_key: SidebarUiState::LOCAL_AUTHORITY.to_string(),
                kind: CloneTargetKind::Local {
                    runtime: local_runtime,
                },
            });
        }

        targets
    }

    pub(super) fn batch_selection_is_single_project(&self, ids: &[String]) -> bool {
        let mut project_id: Option<&ProjectId> = None;
        let mut count = 0usize;
        for id in ids {
            let Some(session) = self.sessions.iter().find(|s| {
                s.id.as_str() == id.as_str() && s.deleted_at_ms.is_none() && !s.is_terminal()
            }) else {
                continue;
            };
            match project_id {
                None => project_id = Some(&session.project_id),
                Some(existing) if existing != &session.project_id => return false,
                Some(_) => {}
            }
            count += 1;
        }
        count > 0
    }

    pub(super) fn open_session_clone_dialog(
        &mut self,
        request: CloneSourceRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || !self.has_clone_target_instances() {
            return;
        }
        let Some(source_backend) = self.backend.clone() else {
            return;
        };
        let scope = match collect_clone_scope(
            &request,
            &self.sessions,
            &self.workspaces,
            &self.ui_state.sidebar.organization,
            &self.sidebar_state.pinned_ids,
        ) {
            Ok(scope) => scope,
            Err(message) => {
                self.agent_error = Some(message);
                cx.notify();
                return;
            }
        };
        let targets = self.available_clone_target_instances();
        if targets.is_empty() {
            return;
        }
        let source_label = self.active_instance_display_label();
        let locale_mode = self.ui_state.appearance.locale;
        let workbench = cx.weak_entity();
        let dialog_view: Entity<SessionCloneDialog> = cx.new(|cx| {
            SessionCloneDialog::new(
                SessionCloneDialogInit {
                    workbench,
                    locale_mode,
                    source_backend,
                    source_label,
                    scope,
                    targets,
                },
                window,
                cx,
            )
        });
        let title = locale::text(
            "Clone to Another Instance",
            "克隆到其他实例",
            "複製到其他執行個體",
        )
        .to_string();
        let viewport = window.viewport_size();
        let dialog_width = (f32::from(viewport.width) - 32.0).clamp(360.0, 620.0);
        let dialog_height = (f32::from(viewport.height) - 32.0).clamp(260.0, 620.0);
        let dialog_weak = dialog_view.downgrade();

        window.open_dialog(cx, move |dialog, _, cx| {
            let is_dark = cx.theme().is_dark();
            let popover = theme::semantic_color("popover", is_dark);
            let popover_foreground = theme::semantic_color("popover-foreground", is_dark);
            let is_cloning = dialog_weak
                .read_with(cx, |clone_dialog, _| {
                    clone_dialog.phase == CloneDialogPhase::Cloning
                })
                .unwrap_or(false);
            let footer = dialog_weak
                .update(cx, |clone_dialog, cx| {
                    clone_dialog.render_footer(cx).into_any_element()
                })
                .unwrap_or_else(|_| div().into_any_element());
            let content_view = dialog_view.clone();
            dialog
                .title(title.clone())
                .w(px(dialog_width))
                .max_w(px(dialog_width))
                .h(px(dialog_height))
                .rounded(px(14.0))
                .bg(popover)
                .text_color(popover_foreground)
                .border_color(popover_foreground.opacity(0.10))
                .keyboard(!is_cloning)
                .overlay(true)
                .overlay_closable(!is_cloning)
                .content(move |content, _, _| {
                    content
                        .min_h_0()
                        .overflow_hidden()
                        .pt(px(4.0))
                        .child(content_view.clone())
                })
                .footer(footer)
        });
        cx.notify();
    }

    pub(super) fn switch_to_cloned_runtime_and_reveal(
        &mut self,
        target_runtime_id: String,
        session_id: Option<VibexSessionId>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session_id) = session_id {
            self.pending_clone_reveal_session = Some((target_runtime_id.clone(), session_id));
        }
        if self.runtime_registry.active_runtime_id() == target_runtime_id {
            if let Some((_, sid)) = self.pending_clone_reveal_session.take() {
                self.selected_session_id = Some(sid);
            }
            self.load_agent_overview(cx);
        } else {
            self.switch_to_runtime(target_runtime_id, cx);
        }
    }

    /// Remote twin of [`Self::load_agent_session_projection`]: queries
    /// `backend.agent().runtime_selection(session_id)` so remote instances also
    /// hydrate session runtime selection or surface uninitialized cloned hints.
    pub(super) fn load_agent_session_projection_remote(
        &mut self,
        session_id: VibexSessionId,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let request_session_id = session_id.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.agent().runtime_selection(request_session_id).await
        });
        self.agent_projection_task = Some(cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    if this.session_generation != generation
                        || this.selected_session_id.as_ref() != Some(&session_id)
                    {
                        return;
                    }
                    match outcome {
                        Ok(Ok(selection)) => {
                            this.runtime_selection_uninitialized_sessions
                                .remove(session_id.as_str());
                            this.cloned_preferred_runtime_selections
                                .remove(session_id.as_str());
                            this.apply_runtime_selection_state(&session_id, selection, cx);
                        }
                        Ok(Err(error))
                            if error.code == "session_runtime_selection_uninitialized" =>
                        {
                            if let Some(hint) =
                                extract_cloned_preferred_runtime_selection_from_backend_error(
                                    &error,
                                )
                            {
                                this.cloned_preferred_runtime_selections
                                    .insert(session_id.as_str().to_string(), hint);
                            }
                            this.runtime_selection_uninitialized_sessions
                                .insert(session_id.as_str().to_string());
                            this.runtime_selection = None;
                        }
                        Ok(Err(error)) => {
                            this.agent_error = Some(format!("{}: {}", error.code, error.message));
                        }
                        Err(error) => {
                            this.agent_error =
                                Some(format!("session projection task failed: {error}"));
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    pub(super) fn enable_or_install_agent_for_cloned_session(
        &mut self,
        agent_id: AgentId,
        needs_managed_install: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let key = agent_id.as_str().to_string();
        if !self.enabling_cloned_agents.insert(key.clone()) {
            return;
        }
        cx.notify();

        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            if needs_managed_install {
                backend
                    .management()
                    .install_managed_agent(MutationRequest::new(agent_id.clone()))
                    .await
                    .map_err(|err| format!("{}: {}", err.code, err.message))?;
            }
            backend
                .management()
                .update_agent_config(MutationRequest::new(AgentUpdateConfigRequest {
                    agent_id: agent_id.clone(),
                    added: Some(true),
                    enabled: Some(true),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                }))
                .await
                .map_err(|err| format!("{}: {}", err.code, err.message))?;
            let _ = backend
                .management()
                .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                    agent_id: agent_id.clone(),
                    cwd_scope: None,
                })
                .await;
            let _ = backend
                .agent()
                .probe_agent_runtime_options(MutationRequest::new(AgentRuntimeOptionProbeRequest {
                    agent_id: agent_id.clone(),
                }))
                .await;
            let _ = backend
                .agent()
                .refresh_agent_auth_methods(MutationRequest::new(agent_id))
                .await;
            Ok::<_, String>(())
        });

        cx.spawn(
            async move |entity: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let outcome = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    this.enabling_cloned_agents.remove(&key);
                    match outcome {
                        Ok(Ok(())) => {
                            this.load_agent_overview(cx);
                            this.load_agent_runtime_catalog(cx);
                        }
                        Ok(Err(error)) => {
                            this.agent_error = Some(error);
                        }
                        Err(error) => {
                            this.agent_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            },
        )
        .detach();
    }

    /// Renders an actionable banner above the session composer when the session's
    /// bound `AgentId` is not installed or not enabled on the current instance.
    pub(super) fn render_session_agent_readiness_banner(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let session = self.view_session()?;
        if session.is_terminal() {
            return None;
        }
        let target_agent_id = self
            .view_runtime_selection()
            .map(|sel| sel.agent_id)
            .unwrap_or_else(|| session.agent_id.clone());

        let snapshot = self
            .agent_snapshots
            .iter()
            .find(|entry| entry.id == target_agent_id);

        let (agent_label, gap) = match snapshot {
            Some(entry) if entry.installed && entry.enabled => return None,
            Some(entry) if !entry.installed => (
                entry.label.clone(),
                AgentReadinessGapKind::Missing {
                    managed_installable: entry.managed_install.managed,
                },
            ),
            Some(entry) => (entry.label.clone(), AgentReadinessGapKind::Disabled),
            None if self.agent_snapshots.is_empty() => return None,
            None => (
                target_agent_id.as_str().to_string(),
                AgentReadinessGapKind::Missing {
                    managed_installable: false,
                },
            ),
        };

        let busy = self
            .enabling_cloned_agents
            .contains(target_agent_id.as_str());
        let warning_color = cx.theme().warning;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;

        let switch_candidates: Vec<(String, SessionRuntimeSelection)> = self
            .runtime_catalog
            .as_ref()
            .map(|catalog: &Rc<_>| {
                let mut by_agent: BTreeMap<AgentId, (String, SessionRuntimeSelection)> =
                    BTreeMap::new();
                for option in &catalog.options {
                    if option.selection.agent_id == target_agent_id {
                        continue;
                    }
                    let ready = self.agent_snapshots.iter().any(|entry| {
                        entry.id == option.selection.agent_id && entry.installed && entry.enabled
                    });
                    if !ready || by_agent.contains_key(&option.selection.agent_id) {
                        continue;
                    }
                    let label = self
                        .agent_snapshots
                        .iter()
                        .find(|e| e.id == option.selection.agent_id)
                        .map(|e| e.label.clone())
                        .unwrap_or_else(|| option.selection.agent_id.as_str().to_string());
                    let preferred = self
                        .ui_state
                        .composer
                        .runtime_selections_by_agent
                        .get(&option.selection.agent_id)
                        .cloned()
                        .unwrap_or_else(|| option.selection.clone());
                    by_agent.insert(option.selection.agent_id.clone(), (label, preferred));
                }
                by_agent.into_values().collect()
            })
            .unwrap_or_default();

        let description = match gap {
            AgentReadinessGapKind::Missing { .. } => format!(
                "{} “{}” {}.",
                locale::text("Agent", "当前会话所需的 Agent", "目前會話所需的 Agent"),
                agent_label,
                locale::text(
                    "is not installed on this instance. Install & enable it to continue seamlessly, or switch to another enabled Agent",
                    "在此实例上尚未安装启用。你可以直接安装启用它，或切换到其他已启用的 Agent 继续对话",
                    "在此執行個體上尚未安裝啟用。你可以直接安裝啟用它，或切換到其他已啟用的 Agent 繼續對話",
                )
            ),
            AgentReadinessGapKind::Disabled => format!(
                "{} “{}” {}.",
                locale::text("Agent", "当前会话所需的 Agent", "目前會話所需的 Agent"),
                agent_label,
                locale::text(
                    "is currently disabled on this instance. Enable it or switch to another Agent",
                    "在此实例上尚未启用。你可以一键启用它，或切换到其他 Agent 继续对话",
                    "在此執行個體上尚未啟用。你可以一鍵啟用它，或切換到其他 Agent 繼續對話",
                )
            ),
        };

        let primary_action = match gap {
            AgentReadinessGapKind::Missing {
                managed_installable: true,
            } => {
                let agent_for_click = target_agent_id.clone();
                Button::new("cloned-session-install-enable-agent")
                    .xsmall()
                    .primary()
                    .loading(busy)
                    .label(format!(
                        "{} {}",
                        locale::text("Install & Enable", "安装并启用", "安裝並啟用"),
                        agent_label
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.enable_or_install_agent_for_cloned_session(
                            agent_for_click.clone(),
                            true,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            AgentReadinessGapKind::Disabled => {
                let agent_for_click = target_agent_id.clone();
                Button::new("cloned-session-enable-agent")
                    .xsmall()
                    .primary()
                    .loading(busy)
                    .label(format!(
                        "{} {}",
                        locale::text("Enable", "启用", "啟用"),
                        agent_label
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.enable_or_install_agent_for_cloned_session(
                            agent_for_click.clone(),
                            false,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            AgentReadinessGapKind::Missing {
                managed_installable: false,
            } => Button::new("cloned-session-configure-agent")
                .xsmall()
                .primary()
                .label(locale::text(
                    "Install in Agents Settings",
                    "前往 Agent 管理安装",
                    "前往 Agent 管理安裝",
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_management_and_discover_agents(cx);
                }))
                .into_any_element(),
        };

        let workbench_weak = cx.weak_entity();
        Some(
            h_flex()
                .id("cloned-session-agent-readiness-banner")
                .w_full()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(warning_color.opacity(0.40))
                .bg(warning_color.opacity(0.08))
                .px_3()
                .py_2()
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(agent_brand_icon(
                            target_agent_id.as_str(),
                            px(14.0),
                            Some(muted),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(fg)
                                .child(description),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .child(primary_action)
                        .when(!switch_candidates.is_empty(), |row| {
                            let candidates = switch_candidates.clone();
                            row.child(
                                Button::new("cloned-session-switch-agent")
                                    .xsmall()
                                    .secondary()
                                    .label(locale::text("Switch Agent", "切换 Agent", "切換 Agent"))
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for (label, selection) in candidates.clone() {
                                            let wb = workbench_weak.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(label)
                                                    .icon(Icon::new(IconName::Bot))
                                                    .on_click(move |_, window, cx| {
                                                        let _ = wb.update(cx, |this, cx| {
                                                            this.choose_runtime_selection(
                                                                ComposerTarget::Session,
                                                                selection.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    }),
                                            );
                                        }
                                        menu
                                    }),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentSessionSafety, AgentSessionState, CloneProviderProfileHint, SessionToolSettings,
        SessionTransport,
    };

    fn sample_session(id: &str, project_id: &str, agent_id: &str) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(id).expect("valid session id"),
            title: format!("Session {id}"),
            project_id: ProjectId::parse(project_id).expect("valid project id"),
            workspace_id: WorkspaceId::parse(format!("workspace_{project_id}"))
                .expect("valid workspace id"),
            workspace_root: format!("/work/{project_id}"),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse(agent_id).expect("valid agent id"),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1_000,
            updated_at_ms: 2_000,
            last_message_at_ms: 2_000,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: SessionToolSettings::default(),
            transport: SessionTransport::default(),
            terminal: None,
        }
    }

    fn sample_workspaces() -> Vec<(ProjectRecord, WorkspaceRecord)> {
        vec![
            (
                ProjectRecord {
                    id: ProjectId::parse("project_a").unwrap(),
                    name: "Alpha".to_string(),
                    root_path: "/work/alpha".to_string(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                WorkspaceRecord {
                    id: WorkspaceId::parse("workspace_project_a").unwrap(),
                    project_id: ProjectId::parse("project_a").unwrap(),
                    root_path: "/work/alpha".to_string(),
                    mode: WorkspaceMode::CurrentCheckout,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            ),
            (
                ProjectRecord {
                    id: ProjectId::parse("project_b").unwrap(),
                    name: "Beta".to_string(),
                    root_path: "/work/beta".to_string(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                WorkspaceRecord {
                    id: WorkspaceId::parse("workspace_project_b").unwrap(),
                    project_id: ProjectId::parse("project_b").unwrap(),
                    root_path: "/work/beta".to_string(),
                    mode: WorkspaceMode::CurrentCheckout,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            ),
        ]
    }

    #[test]
    fn collect_clone_scope_enforces_single_project_and_collects_folder_subtree() {
        let sessions = vec![
            sample_session("session_1", "project_a", "claude"),
            sample_session("session_2", "project_a", "codex"),
            sample_session("session_3", "project_b", "claude"),
        ];
        let workspaces = sample_workspaces();
        let mut org = SidebarOrganizationState::default();
        org.folders.insert(
            "f_root_cross".to_string(),
            SidebarFolderUiState {
                name: "Cross Root".to_string(),
                project_id: None,
                workspace_id: None,
                auto_archive_after_days: None,
            },
        );
        org.folders.insert(
            "f_parent".to_string(),
            SidebarFolderUiState {
                name: "Parent".to_string(),
                project_id: Some("project_a".to_string()),
                workspace_id: None,
                auto_archive_after_days: None,
            },
        );
        org.folders.insert(
            "f_child".to_string(),
            SidebarFolderUiState {
                name: "Child".to_string(),
                project_id: Some("project_a".to_string()),
                workspace_id: None,
                auto_archive_after_days: None,
            },
        );
        org.placements.push(SidebarOrganizationPlacement {
            item: SidebarOrganizationItem::Folder("f_parent".to_string()),
            parent_folder_id: None,
        });
        org.placements.push(SidebarOrganizationPlacement {
            item: SidebarOrganizationItem::Folder("f_child".to_string()),
            parent_folder_id: Some("f_parent".to_string()),
        });
        org.placements.push(SidebarOrganizationPlacement {
            item: SidebarOrganizationItem::Session("session_2".to_string()),
            parent_folder_id: Some("f_child".to_string()),
        });

        let cross_sessions = collect_clone_scope(
            &CloneSourceRequest::Sessions(vec![
                VibexSessionId::parse("session_1").unwrap(),
                VibexSessionId::parse("session_3").unwrap(),
            ]),
            &sessions,
            &workspaces,
            &org,
            &BTreeSet::new(),
        );
        assert!(cross_sessions.is_err());

        let cross_folder = collect_clone_scope(
            &CloneSourceRequest::Folder("f_root_cross".to_string()),
            &sessions,
            &workspaces,
            &org,
            &BTreeSet::new(),
        );
        assert!(cross_folder.is_err());

        let folder_scope = collect_clone_scope(
            &CloneSourceRequest::Folder("f_parent".to_string()),
            &sessions,
            &workspaces,
            &org,
            &BTreeSet::new(),
        )
        .expect("project-scoped folder should collect recursively");
        assert_eq!(folder_scope.source_project_id.as_str(), "project_a");
        assert_eq!(folder_scope.session_ids.len(), 1);
        assert_eq!(folder_scope.session_ids[0].as_str(), "session_2");
        assert_eq!(folder_scope.folders.len(), 2);
        assert_eq!(folder_scope.folders[0].0, "f_parent");
        assert_eq!(folder_scope.folders[1].0, "f_child");
    }

    #[test]
    fn compute_missing_provider_actions_detects_missing_provider_and_merges_organization() {
        let exported = vec![ExportedCloneSession {
            source_session_id: VibexSessionId::parse("session_1").unwrap(),
            title: "Cached Turn".to_string(),
            agent_id: AgentId::parse("claude").unwrap(),
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            tools: SessionToolSettings::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            last_message_at_ms: 2,
            archived_at_ms: None,
            runtime_selection: Some(SessionRuntimeSelection::provider(
                AgentId::parse("claude").unwrap(),
                ProviderProfileId::parse("provider_src_1").unwrap(),
                "claude-sonnet-4-5",
            )),
            provider_hint: Some(CloneProviderProfileHint {
                provider_profile_id: ProviderProfileId::parse("provider_src_1").unwrap(),
                display_name: "CustomRelay".to_string(),
                base_url: Some("https://relay.example.com/v1".to_string()),
                protocol: Some("anthropic_messages".to_string()),
                configured_models: vec!["claude-sonnet-4-5".to_string()],
            }),
            timeline_items: Vec::new(),
            native_artifact: None,
            native_artifact_omitted: false,
        }];

        let actions = compute_missing_provider_actions(&exported, &[], &[], &[], &[], &[], &[]);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            MissingProviderAction::CreateProvider {
                display_name,
                models_to_configure,
                agents_to_bind,
                ..
            } => {
                assert_eq!(display_name, "CustomRelay");
                assert_eq!(models_to_configure, &vec!["claude-sonnet-4-5".to_string()]);
                assert_eq!(agents_to_bind, &vec![AgentId::parse("claude").unwrap()]);
            }
            other => panic!("unexpected action: {other:?}"),
        }

        let mut arrangement = SidebarAuthorityArrangement::default();
        let group = SessionGroupUiState::new(
            "Pair",
            "project_a",
            "workspace_project_a",
            vec!["session_1".to_string()],
        );
        let scope = CollectedCloneScope {
            kind: CloneSourceKind::Group,
            display_label: "Pair".to_string(),
            source_project_id: ProjectId::parse("project_a").unwrap(),
            source_project_name: "Alpha".to_string(),
            source_workspace_root: "/work/alpha".to_string(),
            source_workspace_mode: WorkspaceMode::CurrentCheckout,
            session_ids: vec![VibexSessionId::parse("session_1").unwrap()],
            folders: Vec::new(),
            root_folder_id: None,
            groups: vec![("grp_1".to_string(), group)],
            placements: vec![SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Group("grp_1".to_string()),
                parent_folder_id: None,
            }],
            pinned_session_ids: BTreeSet::from(["session_1".to_string()]),
        };
        let mut map = HashMap::new();
        map.insert("session_1".to_string(), "session_target_1".to_string());
        let mut mirrored = RemoteMirroredIds::default();
        mirrored
            .group_ids
            .insert("grp_1".to_string(), "remote_grp_42".to_string());
        let created_groups = merge_cloned_organization_into_arrangement(
            &mut arrangement,
            &scope,
            &ProjectId::parse("project_target").unwrap(),
            &WorkspaceId::parse("workspace_target").unwrap(),
            &map,
            &mirrored,
            999,
        );
        assert!(created_groups.contains("remote_grp_42"));
        assert!(arrangement.pinned_session_ids.contains("session_target_1"));
        assert_eq!(
            arrangement.session_order.first().map(String::as_str),
            Some("session_target_1")
        );
        assert_eq!(arrangement.organization.groups.len(), 1);
        let cloned_group = arrangement
            .organization
            .groups
            .get("remote_grp_42")
            .expect("mirrored group id should be preserved");
        assert_eq!(
            cloned_group.member_session_ids,
            vec!["session_target_1".to_string()]
        );
    }
}
