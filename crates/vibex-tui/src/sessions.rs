//! The session list as the authority arranged it.
//!
//! The Desktop owns the sidebar: its folders, the order of projects and
//! sessions inside them, which sessions are pinned, and which folders are
//! collapsed. A client that renders from the same [`SidebarOrganizationView`]
//! draws the same tree, so a reader who arranged the list on the desktop finds
//! it arranged when they open the terminal — and a change made here lands
//! there.
//!
//! The fallback lives in [`crate::app::App::sidebar_rows`]: when no arrangement
//! is available (no runtime, a runtime with no shell attached, or a reader who
//! has never arranged anything) the list is projected from the sessions
//! themselves rather than from a tree that says nothing.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{AgentSession, AgentSessionState, TimelineItem, TimelinePayload};
use vibex_desktop_model::{
    AgentSidebarRow, AgentSidebarRowKind, SidebarOrganizationItem, SidebarOrganizationView,
    sidebar_project_items_for_workspace, sidebar_root_items, sort_sidebar_sessions,
};

/// A project as the list draws it: the name the sidebar shows and the age the
/// ordering falls back to when the reader has not arranged projects by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub id: String,
    pub label: String,
    pub created_at_ms: i64,
    /// Whether the runtime has a workspace for this project.
    ///
    /// A project the reader has opened but not yet talked to still has a row on
    /// the desktop, so it has one here; a project named only by a session that
    /// has since been deleted does not, and drawing it would put an empty
    /// heading in the list.
    pub has_workspace: bool,
}

pub struct SessionListInput<'a> {
    pub view: &'a SidebarOrganizationView,
    pub sessions: &'a [AgentSession],
    pub projects: &'a [ProjectEntry],
    /// The client's own unread marks, folded in beside the authority's.
    pub unread_session_ids: &'a BTreeSet<String>,
    pub query: &'a str,
}

/// The list as the page draws it.
///
/// The rows are the tree; the counts beside them are what a heading needs to
/// say *how much* it holds, which the drawn rows cannot answer once a group is
/// folded shut. The tally is what the page's header summarises: the reader
/// scanning for work wants to know how much of it there is before reading a
/// single row.
pub struct SessionListRows {
    pub rows: Vec<AgentSidebarRow>,
    /// Sessions under each row's heading, parallel to `rows`. A session row is
    /// nobody's heading, so it carries zero; a folded heading still carries the
    /// size of what it hides.
    pub counts: Vec<usize>,
    /// Sessions the list holds, by the state family the header counts. Only
    /// families with something in them are worth a chip, so zero-count
    /// families are left out.
    pub states: Vec<(AgentSessionState, usize)>,
}

/// The state families the header counts, in the order its chips read them.
///
/// The order is by what the reader has to act on: a session waiting on them,
/// then one still working, then the ones that stopped badly, then the quiet
/// rest. `Initializing` counts as running — the Agent has been asked and has
/// not answered — and a closed or archived session is not news.
pub const SESSION_STATE_CHIPS: [AgentSessionState; 4] = [
    AgentSessionState::NeedsInput,
    AgentSessionState::Running,
    AgentSessionState::Error,
    AgentSessionState::Idle,
];

/// Which chip family a session belongs to, when it belongs to one.
pub fn session_state_chip(state: AgentSessionState) -> Option<AgentSessionState> {
    match state {
        AgentSessionState::NeedsInput => Some(AgentSessionState::NeedsInput),
        AgentSessionState::Running | AgentSessionState::Initializing => {
            Some(AgentSessionState::Running)
        }
        AgentSessionState::Error => Some(AgentSessionState::Error),
        AgentSessionState::Idle => Some(AgentSessionState::Idle),
        AgentSessionState::Closed | AgentSessionState::Archived => None,
    }
}

/// How many sessions the header counts in each state family.
///
/// Counted from the sessions the list is *about* rather than from the rows it
/// drew: a folded heading still holds its sessions, and a header that forgot
/// them would say the reader has less waiting than they do. A filter narrows
/// the count the same way it narrows the list.
pub fn session_state_tally(
    sessions: &[AgentSession],
    query: &str,
) -> Vec<(AgentSessionState, usize)> {
    let mut states = SESSION_STATE_CHIPS
        .iter()
        .map(|state| (*state, 0usize))
        .collect::<Vec<_>>();
    for session in sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none() && session_matches(session, query))
    {
        let Some(chip) = session_state_chip(session.state) else {
            continue;
        };
        if let Some(count) = states
            .iter_mut()
            .find(|(state, _)| *state == chip)
            .map(|(_, count)| count)
        {
            *count += 1;
        }
    }
    states.retain(|(_, count)| *count > 0);
    states
}

/// The tree the reader arranged, flattened into rows.
pub fn session_list_rows(input: &SessionListInput<'_>) -> SessionListRows {
    let query = input.query.trim().to_lowercase();
    let searching = !query.is_empty();

    let mut sessions_by_project = BTreeMap::<String, Vec<AgentSession>>::new();
    for session in input
        .sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none())
    {
        sessions_by_project
            .entry(session.project_id.as_str().to_string())
            .or_default()
            .push(session.clone());
    }

    let states = session_state_tally(input.sessions, &query);

    let mut projects = input.projects.to_vec();
    // A session can outlive the workspace listing this client last fetched.
    // The project still has a name to draw — the one the session's root
    // carries — and hiding the session would be worse than an unpolished row.
    let known = projects
        .iter()
        .map(|project| project.id.clone())
        .collect::<BTreeSet<_>>();
    for (project_id, sessions) in &sessions_by_project {
        if known.contains(project_id) {
            continue;
        }
        let label = sessions
            .first()
            .map(|session| workspace_label(&session.workspace_root))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| project_id.clone());
        projects.push(ProjectEntry {
            id: project_id.clone(),
            label,
            created_at_ms: 0,
            has_workspace: false,
        });
    }
    let entries = projects
        .iter()
        .map(|project| (project.id.clone(), project.clone()))
        .collect::<BTreeMap<_, _>>();
    let project_ids = ordered_project_ids(&projects, &input.view.project_order);

    let mut rows = Vec::new();
    let mut counts = Vec::new();
    push_root_children(
        &mut rows,
        &mut counts,
        input,
        &sessions_by_project,
        &entries,
        &project_ids,
        &query,
        searching,
        None,
        0,
    );
    SessionListRows {
        rows,
        counts,
        states,
    }
}

/// Projects in the order the sidebar shows them: the reader's arrangement
/// first, then the projects created earliest, which is how the Desktop orders
/// a project the reader never moved.
fn ordered_project_ids(projects: &[ProjectEntry], project_order: &[String]) -> Vec<String> {
    let positions = project_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut entries = projects.to_vec();
    entries.sort_by(|left, right| {
        let left_position = positions.get(left.id.as_str()).copied();
        let right_position = positions.get(right.id.as_str()).copied();
        left_position
            .is_none()
            .cmp(&right_position.is_none())
            .then_with(|| {
                left_position
                    .unwrap_or(usize::MAX)
                    .cmp(&right_position.unwrap_or(usize::MAX))
            })
            .then_with(|| left.created_at_ms.cmp(&right.created_at_ms))
            .then_with(|| left.id.cmp(&right.id))
    });
    entries.into_iter().map(|project| project.id).collect()
}

/// Root-level rows: folders and projects, in the arrangement's order.
#[allow(clippy::too_many_arguments)]
fn push_root_children(
    rows: &mut Vec<AgentSidebarRow>,
    counts: &mut Vec<usize>,
    input: &SessionListInput<'_>,
    sessions_by_project: &BTreeMap<String, Vec<AgentSession>>,
    entries: &BTreeMap<String, ProjectEntry>,
    project_ids: &[String],
    query: &str,
    searching: bool,
    parent_folder_id: Option<&str>,
    depth: u8,
) {
    let organization = &input.view.organization;
    for item in sidebar_root_items(organization, project_ids, parent_folder_id) {
        match item {
            SidebarOrganizationItem::Project(project_id) => {
                push_project(
                    rows,
                    counts,
                    input,
                    sessions_by_project,
                    entries,
                    &project_id,
                    query,
                    searching,
                    parent_folder_id,
                    depth,
                );
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let matches_name = folder.name.to_lowercase().contains(query);
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                let mut children = Vec::new();
                let mut child_counts = Vec::new();
                // The folder's children are built even while it is folded shut:
                // the heading says how much it holds either way, and a folded
                // heading that could not count would be a heading that lies.
                push_root_children(
                    &mut children,
                    &mut child_counts,
                    input,
                    sessions_by_project,
                    entries,
                    project_ids,
                    query,
                    searching,
                    Some(&folder_id),
                    depth + 1,
                );
                if searching && !matches_name && children.is_empty() {
                    continue;
                }
                rows.push(AgentSidebarRow {
                    id: format!("folder:{folder_id}"),
                    kind: AgentSidebarRowKind::Folder,
                    project_id: folder.project_id.clone().unwrap_or_default(),
                    workspace_id: folder.workspace_id.clone().unwrap_or_default(),
                    session_id: None,
                    label: folder.name.clone(),
                    depth,
                    pinned: false,
                    selected: false,
                    collapsed,
                    state: None,
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                counts.push(subtree_sessions(&children, &child_counts));
                if !collapsed || searching {
                    rows.extend(children);
                    counts.extend(child_counts);
                }
            }
            SidebarOrganizationItem::Session(_) | SidebarOrganizationItem::Group(_) => {}
        }
    }
}

/// One project's rows: its folders and sessions, with pins hoisted.
#[allow(clippy::too_many_arguments)]
fn push_project(
    rows: &mut Vec<AgentSidebarRow>,
    counts: &mut Vec<usize>,
    input: &SessionListInput<'_>,
    sessions_by_project: &BTreeMap<String, Vec<AgentSession>>,
    entries: &BTreeMap<String, ProjectEntry>,
    project_id: &str,
    query: &str,
    searching: bool,
    parent_folder_id: Option<&str>,
    depth: u8,
) {
    let Some(entry) = entries.get(project_id) else {
        return;
    };
    let label = &entry.label;
    let mut project_sessions = sessions_by_project
        .get(project_id)
        .cloned()
        .unwrap_or_default();
    sort_sidebar_sessions(
        &mut project_sessions,
        &input.view.session_order,
        input.view.session_order_anchored_at_ms,
        &input.view.pinned_session_ids,
    );
    let session_ids = project_sessions
        .iter()
        .filter(|session| session_matches(session, query))
        .map(|session| session.id.as_str().to_string())
        .collect::<Vec<_>>();
    let mut children = Vec::new();
    let mut child_counts = Vec::new();
    push_project_children(
        &mut children,
        &mut child_counts,
        input,
        project_id,
        &project_sessions,
        &session_ids,
        query,
        searching,
        parent_folder_id,
        depth + 1,
    );
    if project_sessions.is_empty() && !entry.has_workspace {
        // A project nothing live is left in: no workspace to open, no session
        // to show. The desktop drops the same row.
        return;
    }
    let matches_name = label.to_lowercase().contains(query);
    if searching && !matches_name && children.is_empty() {
        return;
    }
    let collapsed = input.view.collapsed_project_ids.contains(project_id);
    rows.push(AgentSidebarRow {
        id: format!("project:{project_id}"),
        kind: AgentSidebarRowKind::Project,
        project_id: project_id.to_string(),
        workspace_id: String::new(),
        session_id: None,
        label: label.clone(),
        depth,
        pinned: false,
        selected: false,
        collapsed,
        state: None,
        parent_id: parent_folder_id.map(ToString::to_string),
    });
    counts.push(subtree_sessions(&children, &child_counts));
    if !collapsed || searching {
        rows.extend(children);
        counts.extend(child_counts);
    }
}

/// A project's children at one folder level: sessions and nested folders.
#[allow(clippy::too_many_arguments)]
fn push_project_children(
    rows: &mut Vec<AgentSidebarRow>,
    counts: &mut Vec<usize>,
    input: &SessionListInput<'_>,
    project_id: &str,
    project_sessions: &[AgentSession],
    session_ids: &[String],
    query: &str,
    searching: bool,
    parent_folder_id: Option<&str>,
    depth: u8,
) {
    let organization = &input.view.organization;
    let items = sidebar_project_items_for_workspace(
        organization,
        project_id,
        None,
        true,
        true,
        session_ids,
        &[],
        &input.view.pinned_session_ids,
        parent_folder_id,
    );
    for item in items {
        match item {
            SidebarOrganizationItem::Session(session_id) => {
                let Some(session) = project_sessions
                    .iter()
                    .find(|session| session.id.as_str() == session_id)
                else {
                    continue;
                };
                rows.push(AgentSidebarRow {
                    id: format!("session:{}", session.id.as_str()),
                    kind: AgentSidebarRowKind::Session,
                    project_id: project_id.to_string(),
                    workspace_id: session.workspace_id.as_str().to_string(),
                    session_id: Some(session.id.clone()),
                    label: session.title.clone(),
                    depth,
                    pinned: input.view.pinned_session_ids.contains(session.id.as_str()),
                    selected: false,
                    collapsed: false,
                    state: Some(session.state),
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                counts.push(0);
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let matches_name = folder.name.to_lowercase().contains(query);
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                let mut children = Vec::new();
                let mut child_counts = Vec::new();
                // Built whether or not the folder is open, for the same reason
                // the root-level folder is: the count on the heading.
                push_project_children(
                    &mut children,
                    &mut child_counts,
                    input,
                    project_id,
                    project_sessions,
                    session_ids,
                    query,
                    searching,
                    Some(&folder_id),
                    depth + 1,
                );
                if searching && !matches_name && children.is_empty() {
                    continue;
                }
                rows.push(AgentSidebarRow {
                    id: format!("folder:{folder_id}"),
                    kind: AgentSidebarRowKind::Folder,
                    project_id: project_id.to_string(),
                    workspace_id: folder.workspace_id.clone().unwrap_or_default(),
                    session_id: None,
                    label: folder.name.clone(),
                    depth,
                    pinned: false,
                    selected: false,
                    collapsed,
                    state: None,
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                counts.push(subtree_sessions(&children, &child_counts));
                if !collapsed || searching {
                    rows.extend(children);
                    counts.extend(child_counts);
                }
            }
            // A group row is a leaf this client does not draw yet; its members
            // are listed as ordinary sessions by the caller passing no groups,
            // so nothing is hidden behind a row that does not exist.
            SidebarOrganizationItem::Project(_) | SidebarOrganizationItem::Group(_) => {}
        }
    }
}

/// How many sessions a heading holds: every session in its subtree, folded or
/// not.
///
/// Counted through the children's own counts rather than by looking at the
/// drawn rows, because a folded heading keeps its sessions out of the row list
/// while still holding them.
fn subtree_sessions(children: &[AgentSidebarRow], counts: &[usize]) -> usize {
    children
        .iter()
        .zip(counts)
        .map(|(row, count)| {
            if row.kind == AgentSidebarRowKind::Session {
                1
            } else {
                *count
            }
        })
        .sum()
}

/// How many characters of a session's last words the list keeps.
///
/// The renderer truncates to the column it has; this is the bound that keeps a
/// pasted document from living in memory for the rest of the process.
const ECHO_CHARACTERS: usize = 200;

/// What a session last did, one line, for the list's second line.
///
/// Only what the Agent said or did. A *delta* is a message still arriving and
/// would rewrite the line on every chunk, and the reader's own message is
/// already the title of half these rows. The text comes from the Agent, so it
/// is flattened onto one line and stripped of control characters before any
/// surface draws it.
pub fn session_echo(item: &TimelineItem) -> Option<String> {
    let text = match &item.payload {
        TimelinePayload::AgentMessage(message) => message.text.as_str(),
        TimelinePayload::ToolCall(call) => {
            if call.summary.trim().is_empty() {
                call.tool_name.as_str()
            } else {
                call.summary.as_str()
            }
        }
        TimelinePayload::Command(command) => command.command.as_str(),
        TimelinePayload::FileOperation(operation) => {
            if operation.summary.trim().is_empty() {
                operation.path.as_str()
            } else {
                operation.summary.as_str()
            }
        }
        TimelinePayload::Error(error) => error.message.as_str(),
        _ => return None,
    };
    one_line(text)
}

/// One line of a session's own words, or nothing when there is nothing to say.
fn one_line(text: &str) -> Option<String> {
    // A control character is not a line break: it is a cursor move, a bell or
    // the start of an escape sequence, and drawing it would let the Agent
    // rewrite the list around itself. Whitespace keeps its meaning as a
    // separator, so it becomes a space like the rest.
    let flattened = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let flattened = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.is_empty() {
        return None;
    }
    Some(flattened.chars().take(ECHO_CHARACTERS).collect())
}

/// Whether a session is one the list is about, for a given filter.
pub(crate) fn session_matches(session: &AgentSession, query: &str) -> bool {
    query.is_empty()
        || session.title.to_lowercase().contains(query)
        || session.workspace_root.to_lowercase().contains(query)
}

/// Heading counts for a projection that draws its own rows.
///
/// The arranged tree counts a heading while it builds it, because only it knows
/// which sessions it decided to keep. A projection built elsewhere does not, so
/// the headings are counted here from the sessions the rows name: the same
/// question, asked of the rows rather than of the tree.
pub fn heading_counts(
    rows: &[AgentSidebarRow],
    sessions: &[AgentSession],
    query: &str,
) -> Vec<usize> {
    rows.iter()
        .map(|row| {
            if row.kind == AgentSidebarRowKind::Session {
                return 0;
            }
            sessions
                .iter()
                .filter(|session| {
                    session.deleted_at_ms.is_none()
                        && session_matches(session, query)
                        && session.project_id.as_str() == row.project_id
                        && (row.workspace_id.is_empty()
                            || session.workspace_id.as_str() == row.workspace_id)
                })
                .count()
        })
        .collect()
}

/// The last path segment of a workspace root, which is the name a project row
/// falls back to when the workspace listing has not arrived.
pub fn workspace_label(root: &str) -> String {
    std::path::Path::new(root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(root)
        .to_string()
}

/// Project entries for the list: the runtime's projects, plus any project a
/// loaded session names that the workspace listing has not caught up with.
pub fn project_entries(
    workspaces: &[vibex_backend::WorkspaceSummary],
    sessions: &[AgentSession],
) -> Vec<ProjectEntry> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for summary in workspaces {
        let id = summary.project.id.as_str().to_string();
        if seen.insert(id.clone()) {
            entries.push(ProjectEntry {
                id,
                label: summary.project.name.clone(),
                created_at_ms: summary.project.created_at_ms,
                has_workspace: true,
            });
        }
    }
    for session in sessions {
        let id = session.project_id.as_str().to_string();
        if seen.insert(id) {
            entries.push(ProjectEntry {
                label: workspace_label(&session.workspace_root),
                created_at_ms: session.created_at_ms,
                id: session.project_id.as_str().to_string(),
                has_workspace: false,
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, AgentSessionState, ProjectId, VibexSessionId, WorkspaceId, WorkspaceMode,
    };
    use vibex_desktop_model::{
        SidebarFolderUiState, SidebarOrganizationPlacement, SidebarOrganizationState,
    };

    /// A session whose project id carries the prefix the id type requires.
    fn session(id: &str, project: &str, title: &str, last_message_at_ms: i64) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(id).expect("valid session id"),
            title: title.to_string(),
            project_id: ProjectId::parse(format!("project_{project}")).expect("valid project id"),
            workspace_id: WorkspaceId::new(),
            workspace_root: format!("/repo/{project}"),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("claude").expect("valid agent id"),
            state: AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 10,
            updated_at_ms: 20,
            last_message_at_ms,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    /// The arrangement the desktop is holding: `vibex` with a collapsed
    /// "archive" folder, one pinned session hoisted above the manual order, and
    /// the projects themselves ordered by hand.
    fn arranged_view() -> SidebarOrganizationView {
        let mut organization = SidebarOrganizationState::default();
        organization.folders.insert(
            "folder-archive".to_string(),
            SidebarFolderUiState {
                name: "archive".to_string(),
                project_id: Some("project_vibex".to_string()),
                workspace_id: None,
                auto_archive_after_days: None,
            },
        );
        // The placements are the drag order: the projects at the root, then
        // the project's own children — two sessions, the folder, and the
        // session that lives inside it.
        organization.placements = vec![
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Project("project_vibex".to_string()),
                parent_folder_id: None,
            },
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Project("project_site".to_string()),
                parent_folder_id: None,
            },
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Session("session_kept0001".to_string()),
                parent_folder_id: None,
            },
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Session("session_recent01".to_string()),
                parent_folder_id: None,
            },
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Folder("folder-archive".to_string()),
                parent_folder_id: None,
            },
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Session("session_old00001".to_string()),
                parent_folder_id: Some("folder-archive".to_string()),
            },
        ];
        organization
            .collapsed_folder_ids
            .insert("folder-archive".to_string());
        SidebarOrganizationView {
            revision: 7,
            organization,
            collapsed_project_ids: BTreeSet::new(),
            collapsed_workspace_ids: BTreeSet::new(),
            pinned_session_ids: BTreeSet::from(["session_kept0001".to_string()]),
            session_order: vec![
                "session_kept0001".to_string(),
                "session_recent01".to_string(),
                "session_older001".to_string(),
                "session_old00001".to_string(),
            ],
            // A manual order holds for anything inactive since the anchor.
            session_order_anchored_at_ms: i64::MAX,
            hierarchy_mode: vibex_desktop_model::SidebarHierarchyMode::Compact,
            project_order: vec!["project_vibex".to_string(), "project_site".to_string()],
            workspace_order: BTreeMap::new(),
            project_appearances: BTreeMap::new(),
            worktree_titles: BTreeMap::new(),
            project_location_preferences: BTreeMap::new(),
            auto_continue_project_ids: BTreeSet::new(),
            auto_continue_session_overrides: BTreeMap::new(),
            auto_continue_session_ids: BTreeSet::new(),
            auto_continue_paused_session_ids: BTreeSet::new(),
            unread_session_ids: BTreeSet::from(["session_recent01".to_string()]),
        }
    }

    fn list_for(view: &SidebarOrganizationView, query: &str) -> SessionListRows {
        let sessions = vec![
            session("session_older001", "vibex", "older", 3),
            session("session_recent01", "vibex", "recent", 2),
            session("session_kept0001", "vibex", "kept", 1),
            session("session_old00001", "vibex", "archived away", 4),
            session("session_site0001", "site", "the site", 5),
        ];
        let projects = vec![
            ProjectEntry {
                id: "project_site".to_string(),
                label: "vibex-site".to_string(),
                created_at_ms: 5,
                has_workspace: true,
            },
            ProjectEntry {
                id: "project_vibex".to_string(),
                label: "vibex".to_string(),
                created_at_ms: 6,
                has_workspace: true,
            },
        ];
        session_list_rows(&SessionListInput {
            view,
            sessions: &sessions,
            projects: &projects,
            unread_session_ids: &BTreeSet::new(),
            query,
        })
    }

    fn rows_for(view: &SidebarOrganizationView, query: &str) -> Vec<AgentSidebarRow> {
        list_for(view, query).rows
    }

    fn labels(rows: &[AgentSidebarRow]) -> Vec<String> {
        rows.iter().map(|row| row.label.clone()).collect()
    }

    #[test]
    fn a_heading_counts_what_it_holds_even_when_it_is_folded_shut() {
        let view = arranged_view();
        let list = list_for(&view, "");
        let (index, _) = list
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.label == "archive")
            .expect("the folded folder's heading");
        // The folder hides its one session, and still says it holds one.
        assert_eq!(
            list.counts[index], 1,
            "a folded heading stopped counting: {:#?}",
            list.rows
        );
        assert!(!list.rows.iter().any(|row| row.label == "archived away"));
        // A session row is nobody's heading.
        let (session_index, _) = list
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.label == "kept")
            .expect("a session row");
        assert_eq!(list.counts[session_index], 0);
        // The project heading counts its whole subtree, the folded folder's
        // session included.
        let (project_index, _) = list
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.label == "vibex")
            .expect("the project heading");
        assert_eq!(list.counts[project_index], 4);
    }

    #[test]
    fn the_header_tally_counts_the_sessions_the_list_is_about() {
        let view = arranged_view();
        let mut sessions = vec![
            session("session_older001", "vibex", "older", 3),
            session("session_recent01", "vibex", "recent", 2),
        ];
        sessions[0].state = AgentSessionState::Running;
        sessions[1].state = AgentSessionState::NeedsInput;
        let mut quiet = session("session_quiet001", "vibex", "quiet", 1);
        quiet.state = AgentSessionState::Idle;
        // A session nobody can act on, and one that was deleted: neither is
        // news the header should carry.
        let mut closed = session("session_closed001", "vibex", "closed", 1);
        closed.state = AgentSessionState::Closed;
        let mut deleted = session("session_gone0001", "vibex", "gone", 1);
        deleted.deleted_at_ms = Some(30);
        sessions.extend([quiet, closed, deleted]);
        let projects = vec![ProjectEntry {
            id: "project_vibex".to_string(),
            label: "vibex".to_string(),
            created_at_ms: 6,
            has_workspace: true,
        }];
        let list = session_list_rows(&SessionListInput {
            view: &view,
            sessions: &sessions,
            projects: &projects,
            unread_session_ids: &BTreeSet::new(),
            query: "",
        });
        assert_eq!(
            list.states,
            vec![
                (AgentSessionState::NeedsInput, 1),
                (AgentSessionState::Running, 1),
                (AgentSessionState::Idle, 1),
            ],
            "the tally is not what the list holds"
        );

        // A filter narrows the tally the same way it narrows the list.
        let list = session_list_rows(&SessionListInput {
            view: &view,
            sessions: &sessions,
            projects: &projects,
            unread_session_ids: &BTreeSet::new(),
            query: "quiet",
        });
        assert_eq!(
            list.states,
            vec![(AgentSessionState::Idle, 1)],
            "a filter left the header counting hidden sessions"
        );
    }

    fn echo_item(payload: TimelinePayload) -> TimelineItem {
        let session_id = VibexSessionId::new();
        TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: 1,
            timestamp_ms: 1,
            source: vibex_core::TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        }
    }

    #[test]
    fn a_session_echo_is_one_clean_line_of_what_the_agent_did() {
        assert_eq!(
            session_echo(&echo_item(TimelinePayload::AgentMessage(
                vibex_core::AgentMessagePayload {
                    text: "done\x1b[2J\n\n  with it ".to_string(),
                    is_final: true,
                }
            ))),
            Some("done [2J with it".to_string()),
            "a control character was allowed through, or the line was not flattened"
        );
        assert_eq!(
            session_echo(&echo_item(TimelinePayload::ToolCall(
                vibex_core::ToolCallPayload {
                    tool_call_id: "call-1".to_string(),
                    tool_name: "read_file".to_string(),
                    status: vibex_core::ToolCallStatus::Completed,
                    summary: "Read src/main.rs".to_string(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }
            ))),
            Some("Read src/main.rs".to_string())
        );
        assert_eq!(
            session_echo(&echo_item(TimelinePayload::AgentMessageDelta(
                vibex_core::AgentMessageDeltaPayload {
                    text_delta: "half a thought".to_string(),
                    chunk_index: 0,
                    phase: None,
                }
            ))),
            None,
            "a message still arriving is not what the session last said"
        );
        assert_eq!(
            session_echo(&echo_item(TimelinePayload::UserMessage(
                vibex_core::UserMessagePayload {
                    text: "do the thing".to_string(),
                    attachments: Vec::new(),
                    delivery: Default::default(),
                    provenance: vibex_core::MessageProvenance::LegacyUnknown,
                }
            ))),
            None,
            "the reader's own message is the title of half these rows"
        );
        assert_eq!(
            session_echo(&echo_item(TimelinePayload::Reasoning(
                vibex_core::ReasoningPayload {
                    text: "thinking about it".to_string(),
                    is_final: true,
                }
            ))),
            None,
            "the Agent's private reasoning is not what it did"
        );
    }

    #[test]
    fn the_list_follows_the_arrangement_an_authority_published() {
        let view = arranged_view();
        let rows = rows_for(&view, "");
        // The project order is the reader's; the pinned session leads its
        // project's band; the collapsed folder keeps its place and hides what
        // it holds.
        assert_eq!(
            labels(&rows),
            vec![
                "vibex",
                "kept",
                "recent",
                "archive",
                "older",
                "vibex-site",
                "the site",
            ],
            "the tree is not the one that was arranged: {rows:#?}"
        );
        assert_eq!(rows[0].kind, AgentSidebarRowKind::Project);
        assert!(rows[1].pinned, "the pinned session lost its pin");
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[3].kind, AgentSidebarRowKind::Folder);
        assert!(rows[3].collapsed, "the archive folder lost its collapse");
        assert_eq!(rows[3].depth, 1);
        // The folder's own child is not drawn while it is closed; the session
        // the reader never dragged follows the placed ones.
        assert!(!rows.iter().any(|row| row.label == "archived away"));
        assert_eq!(rows[4].label, "older");
    }

    #[test]
    fn a_query_opens_folders_and_drops_what_does_not_match() {
        let view = arranged_view();
        let rows = rows_for(&view, "archived");
        // Searching reaches inside a closed folder rather than hiding the
        // session the reader is looking for.
        assert_eq!(
            labels(&rows),
            vec!["vibex", "archive", "archived away"],
            "a search did not reach into the folder: {rows:#?}"
        );
        assert_eq!(rows[2].depth, 2);

        let rows = rows_for(&view, "site");
        assert_eq!(
            labels(&rows),
            vec!["vibex-site", "the site"],
            "a project with no surviving session was not dropped: {rows:#?}"
        );
    }

    #[test]
    fn a_project_nothing_lives_in_is_not_a_row() {
        let view = arranged_view();
        let sessions = vec![session("session_recent01", "vibex", "recent", 2)];
        let projects = vec![
            ProjectEntry {
                id: "project_vibex".to_string(),
                label: "vibex".to_string(),
                created_at_ms: 1,
                has_workspace: false,
            },
            // Named by no live session and never opened: the desktop drops it,
            // and an empty heading here would say a project exists that the
            // list can say nothing about.
            ProjectEntry {
                id: "project_gone".to_string(),
                label: "gone".to_string(),
                created_at_ms: 2,
                has_workspace: false,
            },
        ];
        let rows = session_list_rows(&SessionListInput {
            view: &view,
            sessions: &sessions,
            projects: &projects,
            unread_session_ids: &BTreeSet::new(),
            query: "",
        })
        .rows;
        assert!(
            !rows.iter().any(|row| row.label == "gone"),
            "an empty project kept a row: {rows:#?}"
        );

        // A project with a workspace but no sessions yet keeps its row, the
        // way the desktop shows a checkout that has not been talked to.
        let projects = vec![ProjectEntry {
            id: "project_gone".to_string(),
            label: "gone".to_string(),
            created_at_ms: 2,
            has_workspace: true,
        }];
        let rows = session_list_rows(&SessionListInput {
            view: &view,
            sessions: &[],
            projects: &projects,
            unread_session_ids: &BTreeSet::new(),
            query: "",
        })
        .rows;
        assert_eq!(labels(&rows), vec!["gone"], "{rows:#?}");
    }

    #[test]
    fn a_session_the_workspace_listing_never_mentioned_still_has_a_project() {
        let view = arranged_view();
        let sessions = vec![session("session_orphan01", "project-gone", "orphan", 1)];
        let rows = session_list_rows(&SessionListInput {
            view: &view,
            sessions: &sessions,
            projects: &[],
            unread_session_ids: &BTreeSet::new(),
            query: "",
        })
        .rows;
        // The project names the session's own root, so the session is not lost
        // behind a listing that has not caught up.
        assert_eq!(labels(&rows), vec!["project-gone", "orphan"], "{rows:#?}");
    }
}
