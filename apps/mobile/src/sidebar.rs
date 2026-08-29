//! Sidebar tree projection and drag arithmetic for the compact client.
//!
//! The Desktop owns the sidebar organization. This module turns a synced
//! [`SidebarOrganizationView`] into the flat row list the phone renders, using
//! the same shared ordering helpers the Desktop draws from, and works out where
//! a finger drag would drop. Keeping it free of GPUI types keeps the tree and
//! the drop rules under test.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{AgentSession, AgentSessionState, VibexSessionId, WorkspaceMode};
use vibex_desktop_model::{
    SidebarHierarchyMode, SidebarOrganizationItem, SidebarOrganizationView,
    sidebar_project_items_for_workspace, sidebar_root_items,
};

use crate::theme;

/// Folders may nest, but a runaway parent chain must not build an unbounded
/// row list; the Desktop applies the same ceiling when it renders.
const MAX_FOLDER_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowKind {
    Folder,
    Project,
    Workspace,
    Session,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SidebarRow {
    pub item: SidebarOrganizationItem,
    pub kind: SidebarRowKind,
    pub depth: usize,
    /// Horizontal offset the Desktop would give this row, in pixels. The
    /// Desktop nests real containers whose padding differs per level, so a
    /// depth multiplier cannot reproduce it; carrying the resolved offset keeps
    /// the two trees aligned column for column.
    pub indent: f32,
    pub label: String,
    /// Scope the row lives in: `None` at the sidebar root, otherwise the
    /// project subtree. A move may not cross scopes.
    pub project_id: Option<String>,
    /// Worktree owner for detailed-hierarchy folders and sessions. Compact
    /// clients keep this alongside the project scope so folder creation can
    /// round-trip without losing its worktree owner.
    pub workspace_id: Option<String>,
    /// Secondary line for a workspace row (usually its branch or compact path).
    pub detail: Option<String>,
    /// Number of direct children represented by the row. Projects use this for
    /// the workspace count badge; workspace rows use it for their session count.
    pub child_count: usize,
    pub collapsed: bool,
    pub pinned: bool,
    pub selected: bool,
    pub state: Option<AgentSessionState>,
    pub session_id: Option<VibexSessionId>,
    pub unread: bool,
    pub auto_continue: bool,
}

impl SidebarRow {
    pub fn id(&self) -> &str {
        if self.kind == SidebarRowKind::Workspace {
            self.workspace_id.as_deref().unwrap_or_default()
        } else {
            self.item.id()
        }
    }
}

/// A project the phone knows about, with the label the Desktop would show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarProject {
    pub id: String,
    pub label: String,
}

/// A workspace displayed beneath a project in the desktop's detailed sidebar.
/// Workspace rows are presentation-only on mobile: their sessions and folders
/// still use the existing project-scoped organization protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarWorkspace {
    pub id: String,
    pub project_id: String,
    pub label: String,
    pub detail: String,
    pub branch: Option<String>,
    pub mode: WorkspaceMode,
    pub collapsed: bool,
}

pub struct SidebarRowInput<'a> {
    pub view: &'a SidebarOrganizationView,
    pub projects: &'a [SidebarProject],
    pub workspaces: &'a [SidebarWorkspace],
    pub sessions: &'a [AgentSession],
    pub selected_session_id: Option<&'a VibexSessionId>,
    pub query: &'a str,
}

fn session_matches(session: &AgentSession, query: &str) -> bool {
    query.is_empty()
        || session.title.to_lowercase().contains(query)
        || session.workspace_root.to_lowercase().contains(query)
}

fn ordered_ids(ids: &[String], preferred: &[String]) -> Vec<String> {
    let positions = preferred
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = ids.to_vec();
    ordered.sort_by_key(|id| {
        (
            positions.get(id.as_str()).is_none(),
            positions.get(id.as_str()).copied().unwrap_or(usize::MAX),
            id.clone(),
        )
    });
    ordered
}

fn ordered_sessions<'a>(
    sessions: impl IntoIterator<Item = &'a AgentSession>,
    view: &SidebarOrganizationView,
) -> Vec<&'a AgentSession> {
    let positions = view
        .session_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut sessions = sessions.into_iter().collect::<Vec<_>>();
    sessions.sort_by_key(|session| {
        (
            !view.pinned_session_ids.contains(session.id.as_str()),
            positions.get(session.id.as_str()).is_none(),
            positions
                .get(session.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            std::cmp::Reverse(session.last_message_at_ms),
            session.id.as_str().to_string(),
        )
    });
    sessions
}

fn reorder_session_items(
    items: Vec<SidebarOrganizationItem>,
    view: &SidebarOrganizationView,
) -> Vec<SidebarOrganizationItem> {
    let session_ids = items
        .iter()
        .filter_map(|item| match item {
            SidebarOrganizationItem::Session(id) => Some(id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let ordered = ordered_ids(&session_ids, &view.session_order);
    let mut next = ordered.into_iter();
    items
        .into_iter()
        .map(|item| match item {
            SidebarOrganizationItem::Session(_) => next
                .next()
                .map(SidebarOrganizationItem::Session)
                .unwrap_or(item),
            item => item,
        })
        .collect()
}

fn reorder_project_items(
    items: Vec<SidebarOrganizationItem>,
    project_order: &[String],
) -> Vec<SidebarOrganizationItem> {
    let ids = items
        .iter()
        .filter_map(|item| match item {
            SidebarOrganizationItem::Project(id) => Some(id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let ordered = ordered_ids(&ids, project_order);
    let mut next = ordered.into_iter();
    items
        .into_iter()
        .map(|item| match item {
            SidebarOrganizationItem::Project(_) => next
                .next()
                .map(SidebarOrganizationItem::Project)
                .unwrap_or(item),
            item => item,
        })
        .collect()
}

fn workspace_status(
    sessions: &[&AgentSession],
    view: &SidebarOrganizationView,
) -> (Option<AgentSessionState>, bool) {
    let running = sessions.iter().any(|session| {
        matches!(
            session.state,
            AgentSessionState::Running | AgentSessionState::Initializing
        )
    });
    let error = sessions
        .iter()
        .any(|session| session.state == AgentSessionState::Error);
    let needs_input = sessions
        .iter()
        .any(|session| session.state == AgentSessionState::NeedsInput);
    let unread = sessions
        .iter()
        .any(|session| view.unread_session_ids.contains(session.id.as_str()));
    let state = if running {
        Some(AgentSessionState::Running)
    } else if error {
        Some(AgentSessionState::Error)
    } else if needs_input {
        Some(AgentSessionState::NeedsInput)
    } else if unread {
        // Idle is rendered with the completed/unread treatment by the mobile
        // row; keeping the state typed avoids inventing a second wire enum.
        Some(AgentSessionState::Idle)
    } else {
        None
    };
    (state, unread)
}

/// Detailed hierarchy is useful only when a project has Git-backed workspace
/// identity. Remote workspace summaries expose that identity as a branch; a
/// managed Vibex worktree remains Git-backed even while its branch snapshot is
/// still loading. This mirrors the desktop's per-project fallback instead of
/// applying the global mode to every project indiscriminately.
fn project_uses_detailed_hierarchy(
    hierarchy_mode: SidebarHierarchyMode,
    workspaces: &[&SidebarWorkspace],
) -> bool {
    hierarchy_mode == SidebarHierarchyMode::Detailed
        && workspaces.iter().any(|workspace| {
            workspace.mode == WorkspaceMode::VibexWorktree || workspace.branch.is_some()
        })
}

fn workspace_branch_name(branch: &str) -> &str {
    branch
        .rsplit_once('/')
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
        .unwrap_or(branch)
}

/// Flattens the Desktop's tree into rows. A non-empty query expands everything
/// and drops projects with no surviving session, matching the Desktop.
pub fn sidebar_rows(input: SidebarRowInput<'_>) -> Vec<SidebarRow> {
    let query = input.query.trim().to_lowercase();
    let searching = !query.is_empty();
    let organization = &input.view.organization;

    let mut sessions_by_project = BTreeMap::<String, Vec<&AgentSession>>::new();
    for session in input
        .sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none())
    {
        sessions_by_project
            .entry(session.project_id.as_str().to_string())
            .or_default()
            .push(session);
    }

    let mut workspaces_by_project = BTreeMap::<String, Vec<&SidebarWorkspace>>::new();
    for workspace in input.workspaces {
        workspaces_by_project
            .entry(workspace.project_id.clone())
            .or_default()
            .push(workspace);
    }
    let workspace_ids = input
        .workspaces
        .iter()
        .map(|workspace| workspace.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut sessions_by_workspace = BTreeMap::<String, Vec<&AgentSession>>::new();
    for session in input
        .sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none())
    {
        // The workspace id is authoritative, but older sessions can carry an
        // id from a previous desktop snapshot. Match the same project/root
        // identity fallback used by the desktop projection.
        let workspace_id = if workspace_ids.contains(session.workspace_id.as_str()) {
            Some(session.workspace_id.as_str().to_string())
        } else {
            input
                .workspaces
                .iter()
                .find(|workspace| {
                    workspace.project_id == session.project_id.as_str()
                        && workspace.detail == session.workspace_root
                        && workspace.mode == session.workspace_mode
                })
                .map(|workspace| workspace.id.clone())
        };
        if let Some(workspace_id) = workspace_id {
            sessions_by_workspace
                .entry(workspace_id)
                .or_default()
                .push(session);
        }
    }

    let visible_projects = input
        .projects
        .iter()
        .filter(|project| {
            if !searching {
                return true;
            }
            if project.label.to_lowercase().contains(&query) {
                return true;
            }
            if workspaces_by_project
                .get(&project.id)
                .is_some_and(|workspaces| {
                    workspaces.iter().any(|workspace| {
                        workspace.label.to_lowercase().contains(&query)
                            || workspace.detail.to_lowercase().contains(&query)
                            || workspace
                                .branch
                                .as_deref()
                                .is_some_and(|branch| branch.to_lowercase().contains(&query))
                    })
                })
            {
                return true;
            }
            sessions_by_project
                .get(&project.id)
                .is_some_and(|sessions| {
                    sessions
                        .iter()
                        .any(|session| session_matches(session, &query))
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    let project_labels = visible_projects
        .iter()
        .map(|project| (project.id.clone(), project.label.clone()))
        .collect::<BTreeMap<_, _>>();
    let project_ids = ordered_ids(
        &visible_projects
            .iter()
            .map(|project| project.id.clone())
            .collect::<Vec<_>>(),
        &input.view.project_order,
    );

    let mut rows = Vec::new();
    push_root_children(
        &mut rows,
        &input,
        organization,
        &project_ids,
        &project_labels,
        &workspaces_by_project,
        &sessions_by_project,
        &sessions_by_workspace,
        &query,
        None,
        0,
        0.0,
    );
    rows
}

#[allow(clippy::too_many_arguments)]
fn push_root_children(
    rows: &mut Vec<SidebarRow>,
    input: &SidebarRowInput<'_>,
    organization: &vibex_desktop_model::SidebarOrganizationState,
    project_ids: &[String],
    project_labels: &BTreeMap<String, String>,
    workspaces_by_project: &BTreeMap<String, Vec<&SidebarWorkspace>>,
    sessions_by_project: &BTreeMap<String, Vec<&AgentSession>>,
    sessions_by_workspace: &BTreeMap<String, Vec<&AgentSession>>,
    query: &str,
    parent_folder_id: Option<&str>,
    depth: usize,
    indent: f32,
) {
    if depth > MAX_FOLDER_DEPTH {
        return;
    }
    let root_items = reorder_project_items(
        sidebar_root_items(organization, project_ids, parent_folder_id),
        &input.view.project_order,
    );
    for item in root_items {
        match item {
            SidebarOrganizationItem::Project(project_id) => {
                let Some(label) = project_labels.get(&project_id) else {
                    continue;
                };
                let project_workspaces = workspaces_by_project
                    .get(&project_id)
                    .map_or(&[][..], Vec::as_slice);
                let detailed_hierarchy =
                    project_uses_detailed_hierarchy(input.view.hierarchy_mode, project_workspaces);
                let collapsed = input.view.collapsed_project_ids.contains(&project_id);
                let selected = input.selected_session_id.is_some_and(|selected| {
                    sessions_by_project
                        .get(&project_id)
                        .is_some_and(|sessions| {
                            sessions.iter().any(|session| session.id == *selected)
                        })
                });
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Project(project_id.clone()),
                    kind: SidebarRowKind::Project,
                    depth,
                    indent,
                    label: label.clone(),
                    project_id: None,
                    workspace_id: None,
                    detail: None,
                    // Desktop's project badge counts workspaces in both
                    // hierarchy modes; Compact only changes where sessions
                    // are rendered, not the project summary.
                    child_count: workspaces_by_project.get(&project_id).map_or(0, Vec::len),
                    collapsed,
                    pinned: false,
                    selected,
                    state: None,
                    session_id: None,
                    unread: false,
                    auto_continue: false,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                // A project's own column is indented in the compact tree; the
                // detailed tree hands that column to the worktree card instead
                // and keeps its children flush with the project row.
                let child_indent = if !detailed_hierarchy {
                    indent + theme::SIDEBAR_PROJECT_SESSION_INDENT
                } else {
                    indent
                };
                push_project_children(
                    rows,
                    input,
                    organization,
                    &project_id,
                    workspaces_by_project,
                    sessions_by_project,
                    sessions_by_workspace,
                    query,
                    None,
                    depth + 1,
                    child_indent,
                    true,
                );
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Folder(folder_id.clone()),
                    kind: SidebarRowKind::Folder,
                    depth,
                    indent,
                    label: folder.name.clone(),
                    project_id: None,
                    workspace_id: folder.workspace_id.clone(),
                    detail: None,
                    child_count: 0,
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
                    unread: false,
                    auto_continue: false,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                push_root_children(
                    rows,
                    input,
                    organization,
                    project_ids,
                    project_labels,
                    workspaces_by_project,
                    sessions_by_project,
                    sessions_by_workspace,
                    query,
                    Some(&folder_id),
                    depth + 1,
                    indent + theme::SIDEBAR_FOLDER_CHILD_INDENT,
                );
            }
            SidebarOrganizationItem::Session(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_project_children(
    rows: &mut Vec<SidebarRow>,
    input: &SidebarRowInput<'_>,
    organization: &vibex_desktop_model::SidebarOrganizationState,
    project_id: &str,
    workspaces_by_project: &BTreeMap<String, Vec<&SidebarWorkspace>>,
    sessions_by_project: &BTreeMap<String, Vec<&AgentSession>>,
    sessions_by_workspace: &BTreeMap<String, Vec<&AgentSession>>,
    query: &str,
    parent_folder_id: Option<&str>,
    depth: usize,
    indent: f32,
    render_workspaces: bool,
) {
    if depth > MAX_FOLDER_DEPTH {
        return;
    }
    let project_sessions = sessions_by_project
        .get(project_id)
        .cloned()
        .unwrap_or_default();
    let project_workspaces = workspaces_by_project
        .get(project_id)
        .map_or(&[][..], Vec::as_slice);
    let detailed_hierarchy =
        project_uses_detailed_hierarchy(input.view.hierarchy_mode, project_workspaces);
    let compact = !detailed_hierarchy;
    let project_sessions = if compact {
        project_sessions
    } else {
        project_sessions
            .into_iter()
            .filter(|session| {
                !sessions_by_workspace
                    .values()
                    .any(|sessions| sessions.iter().any(|candidate| candidate.id == session.id))
            })
            .collect::<Vec<_>>()
    };
    let project_sessions = ordered_sessions(project_sessions, input.view);
    let session_ids = project_sessions
        .iter()
        .filter(|session| session_matches(session, query))
        .map(|session| session.id.as_str().to_string())
        .collect::<Vec<_>>();
    let project_items = reorder_session_items(
        sidebar_project_items_for_workspace(
            organization,
            project_id,
            None,
            true,
            true,
            &session_ids,
            &input.view.pinned_session_ids,
            parent_folder_id,
        ),
        input.view,
    );
    for item in project_items {
        match item {
            SidebarOrganizationItem::Session(session_id) => {
                let Some(session) = project_sessions
                    .iter()
                    .find(|session| session.id.as_str() == session_id)
                else {
                    continue;
                };
                rows.push(session_row(input, project_id, session, depth, indent));
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Folder(folder_id.clone()),
                    kind: SidebarRowKind::Folder,
                    depth,
                    indent,
                    label: folder.name.clone(),
                    project_id: Some(project_id.to_string()),
                    workspace_id: folder.workspace_id.clone(),
                    detail: None,
                    child_count: 0,
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
                    unread: false,
                    auto_continue: false,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                push_project_children(
                    rows,
                    input,
                    organization,
                    project_id,
                    workspaces_by_project,
                    sessions_by_project,
                    sessions_by_workspace,
                    query,
                    Some(&folder_id),
                    depth + 1,
                    indent + theme::SIDEBAR_FOLDER_CHILD_INDENT,
                    false,
                );
            }
            SidebarOrganizationItem::Project(_) => {}
        }
    }

    if !render_workspaces || !detailed_hierarchy {
        return;
    }
    let workspace_ids = workspaces_by_project
        .get(project_id)
        .into_iter()
        .flatten()
        .filter(|workspace| {
            query.is_empty()
                || workspace.label.to_lowercase().contains(query)
                || workspace.detail.to_lowercase().contains(query)
                || workspace
                    .branch
                    .as_deref()
                    .is_some_and(|branch| branch.to_lowercase().contains(query))
                || sessions_by_workspace
                    .get(&workspace.id)
                    .is_some_and(|sessions| {
                        sessions
                            .iter()
                            .any(|session| session_matches(session, query))
                    })
        })
        .map(|workspace| workspace.id.clone())
        .collect::<Vec<_>>();
    let workspace_ids = ordered_ids(
        &workspace_ids,
        input
            .view
            .workspace_order
            .get(project_id)
            .map_or(&[], Vec::as_slice),
    );
    for workspace_id in workspace_ids {
        let Some(workspace) = workspaces_by_project
            .get(project_id)
            .into_iter()
            .flatten()
            .find(|workspace| workspace.id == workspace_id)
        else {
            continue;
        };
        let workspace_sessions = sessions_by_workspace
            .get(&workspace.id)
            .cloned()
            .unwrap_or_default();
        let workspace_sessions = ordered_sessions(workspace_sessions, input.view);
        let session_ids = workspace_sessions
            .iter()
            .filter(|session| session_matches(session, query))
            .map(|session| session.id.as_str().to_string())
            .collect::<Vec<_>>();
        rows.push(SidebarRow {
            // The shared organization protocol intentionally has no workspace
            // item. This placeholder is never sent as a mutation.
            item: SidebarOrganizationItem::Project(workspace.id.clone()),
            kind: SidebarRowKind::Workspace,
            depth,
            indent,
            label: input
                .view
                .worktree_titles
                .get(&workspace.id)
                .cloned()
                .or_else(|| {
                    workspace
                        .branch
                        .as_deref()
                        .map(workspace_branch_name)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| workspace.label.clone()),
            project_id: Some(project_id.to_string()),
            workspace_id: Some(workspace.id.clone()),
            detail: workspace
                .branch
                .clone()
                .or_else(|| Some(workspace.detail.clone())),
            child_count: workspace_sessions.len(),
            collapsed: workspace.collapsed,
            pinned: false,
            selected: input.selected_session_id.is_some_and(|selected| {
                workspace_sessions
                    .iter()
                    .any(|session| session.id == *selected)
            }),
            state: workspace_status(&workspace_sessions, input.view).0,
            session_id: None,
            unread: workspace_status(&workspace_sessions, input.view).1,
            auto_continue: false,
        });
        if workspace.collapsed && query.is_empty() {
            continue;
        }
        push_workspace_children(
            rows,
            input,
            organization,
            project_id,
            &workspace.id,
            &workspace_sessions,
            &session_ids,
            query,
            None,
            depth + 1,
            indent + theme::SIDEBAR_WORKSPACE_SESSION_INDENT,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_workspace_children(
    rows: &mut Vec<SidebarRow>,
    input: &SidebarRowInput<'_>,
    organization: &vibex_desktop_model::SidebarOrganizationState,
    project_id: &str,
    workspace_id: &str,
    workspace_sessions: &[&AgentSession],
    session_ids: &[String],
    query: &str,
    parent_folder_id: Option<&str>,
    depth: usize,
    indent: f32,
) {
    if depth > MAX_FOLDER_DEPTH {
        return;
    }
    let workspace_items = reorder_session_items(
        sidebar_project_items_for_workspace(
            organization,
            project_id,
            Some(workspace_id),
            false,
            false,
            session_ids,
            &input.view.pinned_session_ids,
            parent_folder_id,
        ),
        input.view,
    );
    for item in workspace_items {
        match item {
            SidebarOrganizationItem::Session(session_id) => {
                let Some(session) = workspace_sessions
                    .iter()
                    .find(|session| session.id.as_str() == session_id)
                else {
                    continue;
                };
                rows.push(session_row(input, project_id, session, depth, indent));
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Folder(folder_id.clone()),
                    kind: SidebarRowKind::Folder,
                    depth,
                    indent,
                    label: folder.name.clone(),
                    project_id: Some(project_id.to_string()),
                    workspace_id: Some(workspace_id.to_string()),
                    detail: None,
                    child_count: 0,
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
                    unread: false,
                    auto_continue: false,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                push_workspace_children(
                    rows,
                    input,
                    organization,
                    project_id,
                    workspace_id,
                    workspace_sessions,
                    session_ids,
                    query,
                    Some(&folder_id),
                    depth + 1,
                    indent + theme::SIDEBAR_FOLDER_CHILD_INDENT,
                );
            }
            SidebarOrganizationItem::Project(_) => {}
        }
    }
}

fn session_row(
    input: &SidebarRowInput<'_>,
    project_id: &str,
    session: &AgentSession,
    depth: usize,
    indent: f32,
) -> SidebarRow {
    let session_id = session.id.as_str().to_string();
    SidebarRow {
        item: SidebarOrganizationItem::Session(session_id.clone()),
        kind: SidebarRowKind::Session,
        depth,
        indent,
        label: session.title.clone(),
        project_id: Some(project_id.to_string()),
        workspace_id: Some(session.workspace_id.as_str().to_string()),
        detail: None,
        child_count: 0,
        collapsed: false,
        pinned: input.view.pinned_session_ids.contains(&session_id),
        selected: input
            .selected_session_id
            .is_some_and(|selected| selected.as_str() == session_id),
        state: Some(session.state),
        session_id: Some(session.id.clone()),
        unread: input.view.unread_session_ids.contains(session_id.as_str()),
        auto_continue: input
            .view
            .auto_continue_session_ids
            .contains(session_id.as_str()),
    }
}

/// Session ids mapped to their project, as the mutation validator expects.
pub fn session_projects(sessions: &[AgentSession]) -> BTreeMap<String, String> {
    sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none())
        .map(|session| {
            (
                session.id.as_str().to_string(),
                session.project_id.as_str().to_string(),
            )
        })
        .collect()
}

/// Where a drag would land if the finger were released now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarDropTarget {
    pub index: usize,
    pub position: SidebarDropPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarDropPosition {
    Before,
    After,
    Into,
}

/// The band in the middle of a folder row that means "drop inside" rather than
/// "reorder around". Narrow enough that reordering past a folder stays easy.
const FOLDER_INTO_BAND: f32 = 0.3;

/// Resolves the finger position to a row index. `offset_y` is the list's scroll
/// offset, which GPUI reports as negative once scrolled down.
pub fn row_at_position(pointer_y: f32, list_top: f32, offset_y: f32, row_height: f32) -> f32 {
    (pointer_y - list_top - offset_y) / row_height.max(1.0)
}

/// Turns a fractional row position into a drop target. Dropping onto the
/// dragged row itself is not a move, so those resolve to `None`.
pub fn drop_target(
    rows: &[SidebarRow],
    dragged_index: usize,
    row_position: f32,
) -> Option<SidebarDropTarget> {
    if rows.is_empty() {
        return None;
    }
    let last = rows.len() - 1;
    let clamped = row_position.clamp(0.0, rows.len() as f32 - 0.001);
    let index = (clamped.floor() as usize).min(last);
    let fraction = clamped - index as f32;
    if index == dragged_index {
        return None;
    }
    let position = if rows[index].kind == SidebarRowKind::Folder
        && (FOLDER_INTO_BAND..=1.0 - FOLDER_INTO_BAND).contains(&fraction)
    {
        SidebarDropPosition::Into
    } else if fraction >= 0.5 {
        SidebarDropPosition::After
    } else {
        SidebarDropPosition::Before
    };
    Some(SidebarDropTarget { index, position })
}

/// Whether the drag started on the row's grip column rather than its body.
/// Android delivers a touch pan as a scroll anchored at the press point, so the
/// grip is what separates "move this row" from "scroll the list".
pub fn press_is_on_grip(pointer_x: f32, list_right: f32, grip_width: f32) -> bool {
    pointer_x >= list_right - grip_width
}

/// Ancestor folder ids of a row, so a folder cannot be dropped into itself.
pub fn ancestors_of(rows: &[SidebarRow], index: usize) -> BTreeSet<String> {
    let mut ancestors = BTreeSet::new();
    let Some(row) = rows.get(index) else {
        return ancestors;
    };
    let mut depth = row.depth;
    for candidate in rows[..index].iter().rev() {
        if candidate.depth < depth {
            depth = candidate.depth;
            if candidate.kind == SidebarRowKind::Folder {
                ancestors.insert(candidate.id().to_string());
            }
        }
    }
    ancestors
}

/// Guide lines a row sits behind, as x offsets. The Desktop draws one down the
/// child column of every open folder; a flat row list has to re-derive them
/// from its ancestors, one line per enclosing folder.
pub fn folder_guides(rows: &[SidebarRow], index: usize) -> Vec<f32> {
    let Some(row) = rows.get(index) else {
        return Vec::new();
    };
    let mut guides = Vec::new();
    let mut depth = row.depth;
    for candidate in rows[..index].iter().rev() {
        if candidate.depth < depth {
            depth = candidate.depth;
            if candidate.kind == SidebarRowKind::Folder {
                guides.push(candidate.indent + theme::SIDEBAR_FOLDER_GUIDE_OFFSET);
            }
        }
    }
    guides
}

/// Where a row falls inside the card the Desktop wraps around a worktree and
/// the sessions filed under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarCardEdge {
    /// The worktree row itself, with children below it.
    Top,
    Middle,
    Bottom,
}

/// One row's slice of a worktree card.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarCard {
    pub workspace_id: String,
    /// Indent of the worktree row that opened the card, so every slice draws
    /// its border down the same column.
    pub indent: f32,
    pub edge: SidebarCardEdge,
}

/// The worktree card each row belongs to. A collapsed or empty worktree gets no
/// card, matching the Desktop, which only draws one once the worktree has a
/// child column to enclose.
pub fn workspace_cards(rows: &[SidebarRow]) -> Vec<Option<SidebarCard>> {
    let mut cards = vec![None; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        if row.kind != SidebarRowKind::Workspace {
            continue;
        }
        let Some(workspace_id) = row.workspace_id.clone() else {
            continue;
        };
        let last_child = rows
            .iter()
            .enumerate()
            .skip(index + 1)
            .take_while(|(_, candidate)| candidate.depth > row.depth)
            .map(|(child_index, _)| child_index)
            .last();
        let Some(last_child) = last_child else {
            continue;
        };
        cards[index] = Some(SidebarCard {
            workspace_id: workspace_id.clone(),
            indent: row.indent,
            edge: SidebarCardEdge::Top,
        });
        for (child_index, card) in cards
            .iter_mut()
            .enumerate()
            .take(last_child + 1)
            .skip(index + 1)
        {
            *card = Some(SidebarCard {
                workspace_id: workspace_id.clone(),
                indent: row.indent,
                edge: if child_index == last_child {
                    SidebarCardEdge::Bottom
                } else {
                    SidebarCardEdge::Middle
                },
            });
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentId, AgentSessionSafety, ProjectId, WorkspaceId, WorkspaceMode};
    use vibex_desktop_model::SidebarOrganizationView;

    fn session(id: &str, project_id: &str, title: &str) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(format!("session_{id}")).expect("valid session id"),
            title: title.to_string(),
            project_id: ProjectId::parse(format!("project_{project_id}")).expect("valid project"),
            workspace_id: WorkspaceId::parse(format!("workspace_{project_id}"))
                .expect("valid workspace id"),
            workspace_root: format!("/tmp/{project_id}"),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("claude").expect("valid agent id"),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 0,
            updated_at_ms: 0,
            last_message_at_ms: 0,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn row(kind: SidebarRowKind, id: &str, depth: usize) -> SidebarRow {
        let item = match kind {
            SidebarRowKind::Folder => SidebarOrganizationItem::Folder(id.to_string()),
            SidebarRowKind::Project => SidebarOrganizationItem::Project(id.to_string()),
            SidebarRowKind::Workspace => SidebarOrganizationItem::Project(id.to_string()),
            SidebarRowKind::Session => SidebarOrganizationItem::Session(id.to_string()),
        };
        SidebarRow {
            item,
            kind,
            depth,
            indent: depth as f32 * theme::SIDEBAR_FOLDER_CHILD_INDENT,
            label: id.to_string(),
            project_id: None,
            workspace_id: None,
            detail: None,
            child_count: 0,
            collapsed: false,
            pinned: false,
            selected: false,
            state: None,
            session_id: None,
            unread: false,
            auto_continue: false,
        }
    }

    #[test]
    fn a_project_with_no_organization_still_renders_its_sessions() {
        let view = SidebarOrganizationView::default();
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &[],
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, SidebarRowKind::Project);
        assert_eq!(rows[1].kind, SidebarRowKind::Session);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn a_collapsed_project_hides_its_sessions_until_a_search_expands_it() {
        let mut view = SidebarOrganizationView::default();
        view.collapsed_project_ids
            .insert("project_project".to_string());
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let collapsed = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &[],
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(collapsed.len(), 1);
        let searched = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &[],
            sessions: &sessions,
            selected_session_id: None,
            query: "hello",
        });
        assert_eq!(searched.len(), 2);
    }

    #[test]
    fn detailed_rows_group_sessions_under_their_workspace() {
        let mut view = SidebarOrganizationView::default();
        view.hierarchy_mode = SidebarHierarchyMode::Detailed;
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        let workspaces = vec![SidebarWorkspace {
            id: "workspace_project".to_string(),
            project_id: "project_project".to_string(),
            label: "project".to_string(),
            detail: "/tmp/project".to_string(),
            branch: Some("feature/mobile-sidebar".to_string()),
            mode: WorkspaceMode::CurrentCheckout,
            collapsed: false,
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].child_count, 1);
        assert_eq!(rows[1].kind, SidebarRowKind::Workspace);
        assert_eq!(rows[1].id(), "workspace_project");
        assert_eq!(rows[1].label, "mobile-sidebar");
        assert_eq!(rows[1].detail.as_deref(), Some("feature/mobile-sidebar"));
        assert_eq!(rows[1].child_count, 1);
        assert_eq!(rows[2].kind, SidebarRowKind::Session);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn detailed_mode_falls_back_to_compact_for_a_non_git_project() {
        let mut view = SidebarOrganizationView::default();
        view.hierarchy_mode = SidebarHierarchyMode::Detailed;
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "plain-folder".to_string(),
        }];
        let workspaces = vec![SidebarWorkspace {
            id: "workspace_project".to_string(),
            project_id: "project_project".to_string(),
            label: "plain-folder".to_string(),
            detail: "/tmp/plain-folder".to_string(),
            branch: None,
            mode: WorkspaceMode::CurrentCheckout,
            collapsed: false,
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec![SidebarRowKind::Project, SidebarRowKind::Session]
        );
        assert_eq!(rows[1].indent, theme::SIDEBAR_PROJECT_SESSION_INDENT);
    }

    #[test]
    fn compact_rows_keep_workspace_sessions_at_project_level() {
        let view = SidebarOrganizationView::default();
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        let workspaces = vec![SidebarWorkspace {
            id: "workspace_project".to_string(),
            project_id: "project_project".to_string(),
            label: "project".to_string(),
            detail: "/tmp/project".to_string(),
            branch: None,
            mode: WorkspaceMode::CurrentCheckout,
            collapsed: false,
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, SidebarRowKind::Project);
        assert_eq!(rows[1].kind, SidebarRowKind::Session);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn a_project_folder_nests_the_sessions_placed_inside_it() {
        let mut view = SidebarOrganizationView::default();
        assert!(view.organization.create_folder(
            "folder",
            "Archive",
            Some("project_project".to_string()),
            None
        ));
        let sessions = vec![
            session("session-a", "project", "Kept"),
            session("session-b", "project", "Filed"),
        ];
        let projects = session_projects(&sessions);
        view.organization.reconcile(
            &["project_project".to_string()],
            &projects
                .iter()
                .map(|(session_id, project_id)| (session_id.clone(), project_id.clone()))
                .collect::<Vec<_>>(),
        );
        assert!(view.organization.move_many_into(
            &[SidebarOrganizationItem::Session(
                "session_session-b".to_string()
            )],
            "folder",
            &projects,
        ));
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &[SidebarProject {
                id: "project_project".to_string(),
                label: "vibex".to_string(),
            }],
            workspaces: &[],
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        let kinds = rows
            .iter()
            .map(|row| (row.kind, row.id().to_string(), row.depth))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                (SidebarRowKind::Project, "project_project".to_string(), 0),
                (SidebarRowKind::Folder, "folder".to_string(), 1),
                (SidebarRowKind::Session, "session_session-b".to_string(), 2),
                (SidebarRowKind::Session, "session_session-a".to_string(), 1),
            ]
        );
    }

    #[test]
    fn the_middle_of_a_folder_row_drops_inside_and_the_edges_reorder() {
        let rows = vec![
            row(SidebarRowKind::Session, "dragged", 1),
            row(SidebarRowKind::Folder, "folder", 1),
        ];
        assert_eq!(
            drop_target(&rows, 0, 1.5).map(|target| target.position),
            Some(SidebarDropPosition::Into)
        );
        assert_eq!(
            drop_target(&rows, 0, 1.05).map(|target| target.position),
            Some(SidebarDropPosition::Before)
        );
        assert_eq!(
            drop_target(&rows, 0, 1.95).map(|target| target.position),
            Some(SidebarDropPosition::After)
        );
    }

    #[test]
    fn a_drag_released_over_its_own_row_is_not_a_move() {
        let rows = vec![
            row(SidebarRowKind::Session, "dragged", 1),
            row(SidebarRowKind::Session, "other", 1),
        ];
        assert_eq!(drop_target(&rows, 0, 0.4), None);
        assert!(drop_target(&rows, 0, 1.4).is_some());
    }

    #[test]
    fn a_drop_past_the_last_row_lands_after_it_instead_of_nowhere() {
        let rows = vec![
            row(SidebarRowKind::Session, "dragged", 1),
            row(SidebarRowKind::Session, "other", 1),
        ];
        assert_eq!(
            drop_target(&rows, 0, 9.0),
            Some(SidebarDropTarget {
                index: 1,
                position: SidebarDropPosition::After
            })
        );
    }

    #[test]
    fn a_row_position_follows_the_scroll_offset() {
        assert_eq!(row_at_position(100.0, 20.0, 0.0, 40.0), 2.0);
        assert_eq!(row_at_position(100.0, 20.0, -40.0, 40.0), 3.0);
    }

    #[test]
    fn ancestors_stop_at_the_first_shallower_row_of_each_level() {
        let rows = vec![
            row(SidebarRowKind::Project, "project", 0),
            row(SidebarRowKind::Folder, "outer", 1),
            row(SidebarRowKind::Folder, "inner", 2),
            row(SidebarRowKind::Session, "session", 3),
        ];
        assert_eq!(
            ancestors_of(&rows, 3),
            BTreeSet::from(["inner".to_string(), "outer".to_string()])
        );
    }

    #[test]
    fn a_root_folder_nests_projects_and_survives_a_missing_folder_record() {
        let mut view = SidebarOrganizationView::default();
        assert!(view.organization.create_folder("root", "Work", None, None));
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        view.organization
            .reconcile(&["project_project".to_string()], &[]);
        assert!(view.organization.move_many_into(
            &[SidebarOrganizationItem::Project(
                "project_project".to_string()
            )],
            "root",
            &BTreeMap::new(),
        ));
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &[],
            sessions: &[],
            selected_session_id: None,
            query: "",
        });
        assert_eq!(
            rows.iter()
                .map(|row| (row.kind, row.depth))
                .collect::<Vec<_>>(),
            vec![(SidebarRowKind::Folder, 0), (SidebarRowKind::Project, 1)]
        );

        // A snapshot can name a folder it no longer carries if the desktop
        // pruned it mid-flight. Adopting it must return the project to the root
        // rather than orphan it out of the tree.
        let mut snapshot = view.to_remote();
        snapshot.folders.clear();
        let repaired = SidebarOrganizationView::from_remote(&snapshot);
        let rows = sidebar_rows(SidebarRowInput {
            view: &repaired,
            projects: &projects,
            workspaces: &[],
            sessions: &[],
            selected_session_id: None,
            query: "",
        });
        assert_eq!(
            rows.iter()
                .map(|row| (row.kind, row.depth))
                .collect::<Vec<_>>(),
            vec![(SidebarRowKind::Project, 0)]
        );
    }

    #[test]
    fn a_collapsed_folder_hides_its_children() {
        let mut view = SidebarOrganizationView::default();
        assert!(view.organization.create_folder(
            "folder",
            "Archive",
            Some("project_project".to_string()),
            None
        ));
        let sessions = vec![session("session-a", "project", "Filed")];
        let projects = session_projects(&sessions);
        view.organization.reconcile(
            &["project_project".to_string()],
            &projects
                .iter()
                .map(|(session_id, project_id)| (session_id.clone(), project_id.clone()))
                .collect::<Vec<_>>(),
        );
        assert!(view.organization.move_many_into(
            &[SidebarOrganizationItem::Session(
                "session_session-a".to_string()
            )],
            "folder",
            &projects,
        ));
        view.organization
            .collapsed_folder_ids
            .insert("folder".to_string());
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &[SidebarProject {
                id: "project_project".to_string(),
                label: "vibex".to_string(),
            }],
            workspaces: &[],
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec![SidebarRowKind::Project, SidebarRowKind::Folder]
        );
    }

    #[test]
    fn only_the_grip_column_starts_a_move() {
        assert!(press_is_on_grip(360.0, 380.0, 32.0));
        assert!(!press_is_on_grip(200.0, 380.0, 32.0));
    }

    #[test]
    fn indents_follow_the_desktop_columns_rather_than_the_row_depth() {
        let mut view = SidebarOrganizationView::default();
        view.hierarchy_mode = SidebarHierarchyMode::Detailed;
        let projects = vec![SidebarProject {
            id: "project_project".to_string(),
            label: "vibex".to_string(),
        }];
        let workspaces = vec![SidebarWorkspace {
            id: "workspace_project".to_string(),
            project_id: "project_project".to_string(),
            label: "project".to_string(),
            detail: "/tmp/project".to_string(),
            branch: Some("main".to_string()),
            mode: WorkspaceMode::CurrentCheckout,
            collapsed: false,
        }];
        let sessions = vec![session("session-a", "project", "Hello")];
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        // A worktree keeps the project's own column; only its sessions step in.
        assert_eq!(rows[0].indent, 0.0);
        assert_eq!(rows[1].indent, 0.0);
        assert_eq!(rows[2].indent, theme::SIDEBAR_WORKSPACE_SESSION_INDENT);

        view.hierarchy_mode = SidebarHierarchyMode::Compact;
        let rows = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(rows[1].indent, theme::SIDEBAR_PROJECT_SESSION_INDENT);
    }

    #[test]
    fn a_row_sits_behind_one_guide_per_enclosing_folder() {
        let rows = vec![
            row(SidebarRowKind::Project, "project", 0),
            row(SidebarRowKind::Folder, "outer", 1),
            row(SidebarRowKind::Folder, "inner", 2),
            row(SidebarRowKind::Session, "session", 3),
        ];
        assert!(folder_guides(&rows, 0).is_empty());
        assert_eq!(
            folder_guides(&rows, 3),
            vec![
                rows[2].indent + theme::SIDEBAR_FOLDER_GUIDE_OFFSET,
                rows[1].indent + theme::SIDEBAR_FOLDER_GUIDE_OFFSET,
            ]
        );
    }

    #[test]
    fn a_worktree_card_spans_its_sessions_and_skips_an_empty_worktree() {
        let mut rows = vec![
            row(SidebarRowKind::Project, "project", 0),
            row(SidebarRowKind::Workspace, "workspace", 1),
            row(SidebarRowKind::Session, "session-a", 2),
            row(SidebarRowKind::Session, "session-b", 2),
            row(SidebarRowKind::Workspace, "empty", 1),
        ];
        rows[1].workspace_id = Some("workspace".to_string());
        rows[4].workspace_id = Some("empty".to_string());
        let cards = workspace_cards(&rows);
        assert_eq!(cards[0], None);
        assert_eq!(
            cards[1].as_ref().map(|card| card.edge),
            Some(SidebarCardEdge::Top)
        );
        assert_eq!(
            cards[2].as_ref().map(|card| card.edge),
            Some(SidebarCardEdge::Middle)
        );
        assert_eq!(
            cards[3].as_ref().map(|card| card.edge),
            Some(SidebarCardEdge::Bottom)
        );
        assert_eq!(
            cards[3].as_ref().map(|card| card.workspace_id.as_str()),
            Some("workspace")
        );
        assert_eq!(cards[4], None);
    }
}
