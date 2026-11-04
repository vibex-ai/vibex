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

use vibex_core::{AgentSession, AgentSessionState, TimelineItem, TimelinePayload, VibexSessionId};
use vibex_desktop_model::{
    AgentSidebarRow, AgentSidebarRowKind, SidebarOrganizationItem, SidebarOrganizationView,
    sidebar_project_items_for_workspace, sidebar_root_items, sort_sidebar_sessions,
};

use crate::locale::Strings;

/// One thing a project's or a folder's action button offers.
///
/// The desktop builds the same list from the row it was opened on: a project
/// can start a session, hold a new folder, fold, or be deleted; a folder can do
/// those and be renamed as well. What a row can do is therefore a property of
/// the row, which is why the menu is built from the row rather than kept as one
/// list with the impossible entries disabled — a reader should not have to read
/// past a verb that was never going to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowAction {
    /// Start a session that opens in the row's own directory.
    NewSession,
    /// Make a folder inside this heading.
    NewFolder,
    /// Rename a folder.
    RenameFolder,
    /// Fold the heading shut, or open it again.
    ToggleCollapse,
    /// Delete a project and everything filed under it.
    DeleteProject,
    /// Delete a folder, keeping the sessions it held.
    DeleteFolder,
}

impl SidebarRowAction {
    /// What the menu row says.
    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::NewSession => strings.sidebar_action_new_session(),
            Self::NewFolder => strings.sidebar_action_new_folder(),
            Self::RenameFolder => strings.sidebar_action_rename_folder(),
            Self::ToggleCollapse => strings.sidebar_action_collapse(),
            Self::DeleteProject => strings.sidebar_action_delete_project(),
            Self::DeleteFolder => strings.sidebar_action_delete_folder(),
        }
    }

    /// Whether the action is a label that has to say "open" rather than "fold".
    pub const fn is_fold(self) -> bool {
        matches!(self, Self::ToggleCollapse)
    }

    /// Whether the action cannot be undone, so it is drawn as a warning and
    /// asks before it runs.
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::DeleteProject | Self::DeleteFolder)
    }

    /// The actions a heading offers, in the order the desktop's own menu lists
    /// them.
    ///
    /// A folder no project owns cannot hold a session, so it is not offered one
    /// — the same rule the desktop's folder row follows when it hides its own
    /// new-session button.
    pub fn for_heading(
        kind: vibex_desktop_model::AgentSidebarRowKind,
        project_scoped: bool,
    ) -> Vec<Self> {
        use vibex_desktop_model::AgentSidebarRowKind;
        match kind {
            AgentSidebarRowKind::Project => vec![
                Self::NewSession,
                Self::NewFolder,
                Self::ToggleCollapse,
                Self::DeleteProject,
            ],
            AgentSidebarRowKind::Folder => {
                let mut actions = Vec::new();
                if project_scoped {
                    actions.push(Self::NewSession);
                }
                actions.extend([
                    Self::NewFolder,
                    Self::RenameFolder,
                    Self::ToggleCollapse,
                    Self::DeleteFolder,
                ]);
                actions
            }
            AgentSidebarRowKind::Session => Vec::new(),
        }
    }
}

/// The columns of a heading's action menu, and where the reader is in it.
///
/// The heading travels with the menu by id rather than by position: a list
/// refresh that lands while the menu is open must not make the choice act on
/// whichever heading took the cursor's place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRowActions {
    pub title: String,
    pub row_id: String,
    pub actions: Vec<SidebarRowAction>,
    /// Whether the heading is folded shut, so the fold verb says "expand".
    pub collapsed: bool,
    pub selected: usize,
}

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
    /// The ownership tree the child rows are drawn from.
    pub children: &'a crate::children::ChildSessions,
    pub query: &'a str,
}

/// The list as the page draws it.
///
/// The rows are the tree; the tally is what the page's header summarises, so a
/// reader scanning for work knows how much of it there is before reading a
/// single row. How much one *heading* holds is not carried: nothing draws it,
/// and a heading is read by opening it.
pub struct SessionListRows {
    pub rows: Vec<AgentSidebarRow>,
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

/// Whether a session in this state wears the row's turning ring.
///
/// The one mark in the list that moves, so the row that draws the frame and the
/// clock that steps the phase have to answer with the same reading of the
/// state: two copies of this question is how a ring ends up standing still
/// under a clock that thinks it has nothing to draw — or a clock running for a
/// mark no row paints.
pub fn session_state_turns_ring(state: AgentSessionState) -> bool {
    matches!(
        state,
        AgentSessionState::Running | AgentSessionState::Initializing
    )
}

/// How many sessions the header counts in each state family.
///
/// Counted from the sessions the list is *about* rather than from the rows it
/// drew: a folded heading still holds its sessions, and a header that forgot
/// them would say the reader has less waiting than they do. A filter narrows
/// the count the same way it narrows the list.
pub fn session_state_tally(
    sessions: &[AgentSession],
    children: &crate::children::ChildSessions,
    query: &str,
) -> Vec<(AgentSessionState, usize)> {
    let mut states = SESSION_STATE_CHIPS
        .iter()
        .map(|state| (*state, 0usize))
        .collect::<Vec<_>>();
    // The root list and the children the ownership tree named: a worker that is
    // waiting on the reader is as much news as the session that delegated it,
    // and the header describes the list they are both drawn in.
    for session in
        sessions
            .iter()
            .filter(|session| session.deleted_at_ms.is_none() && session_matches(session, query))
            .chain(children.child_sessions().filter(|session| {
                session.deleted_at_ms.is_none() && session_matches(session, query)
            }))
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

    let states = session_state_tally(input.sessions, input.children, &query);

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
    push_root_children(
        &mut rows,
        input,
        &sessions_by_project,
        &entries,
        &project_ids,
        &query,
        searching,
        None,
        0,
    );
    SessionListRows { rows, states }
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
                // The folder's children are built even while it is folded shut:
                // a search reaches inside a folded folder, and a heading whose
                // children were never built could not answer it.
                push_root_children(
                    &mut children,
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
                    delegated: false,
                    child_count: 0,
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                if !collapsed || searching {
                    rows.extend(children);
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
    let session_ids = {
        let mut ids = project_sessions
            .iter()
            .filter(|session| session_matches(session, query))
            .map(|session| session.id.as_str().to_string())
            .collect::<Vec<_>>();
        // A session whose own name does not match is still the way *to* a child
        // that does: dropping it would make the matching worker unreachable.
        if searching {
            for session in &project_sessions {
                if !ids.iter().any(|id| id == session.id.as_str())
                    && descendant_matches(input.children, &session.id, query, 0)
                {
                    ids.push(session.id.as_str().to_string());
                }
            }
        }
        ids
    };
    let mut children = Vec::new();
    push_project_children(
        &mut children,
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
        delegated: false,
        child_count: 0,
        parent_id: parent_folder_id.map(ToString::to_string),
    });
    if !collapsed || searching {
        rows.extend(children);
    }
}

/// A project's children at one folder level: sessions and nested folders.
#[allow(clippy::too_many_arguments)]
fn push_project_children(
    rows: &mut Vec<AgentSidebarRow>,
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
                let child_count = input.children.declared_child_count(session.id.as_str());
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
                    collapsed: input.children.is_collapsed(session.id.as_str()),
                    state: Some(session.state),
                    delegated: false,
                    child_count,
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                // The branch hangs under the row that owns it, indented one
                // step in, and only while the reader has it open. A search
                // reaches inside a folded branch the way it reaches inside a
                // folded folder, so the reader finds the worker by name.
                if child_count > 0
                    && (!input.children.is_collapsed(session.id.as_str()) || searching)
                {
                    push_child_rows(
                        rows,
                        input.children,
                        project_id,
                        &session.id,
                        depth + 1,
                        query,
                        searching,
                    );
                }
            }
            SidebarOrganizationItem::Folder(folder_id) => {
                let Some(folder) = organization.folder(&folder_id) else {
                    continue;
                };
                let matches_name = folder.name.to_lowercase().contains(query);
                let collapsed = organization.collapsed_folder_ids.contains(&folder_id);
                let mut children = Vec::new();
                // Built whether or not the folder is open, for the same reason
                // the root-level folder is: a search reaches inside it.
                push_project_children(
                    &mut children,
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
                    delegated: false,
                    child_count: 0,
                    parent_id: parent_folder_id.map(ToString::to_string),
                });
                if !collapsed || searching {
                    rows.extend(children);
                }
            }
            // A group row is a leaf this client does not draw yet; its members
            // are listed as ordinary sessions by the caller passing no groups,
            // so nothing is hidden behind a row that does not exist.
            SidebarOrganizationItem::Project(_) | SidebarOrganizationItem::Group(_) => {}
        }
    }
}

/// One session's delegated children, then theirs, under the row that owns them.
///
/// A child is a session row like any other — it opens, it carries its state and
/// its own age — that happens to hang one indent in from the session that
/// delegated it. The chain is walked to whatever depth the runtime published,
/// so a worker that delegated further work is not drawn as a leaf.
#[allow(clippy::too_many_arguments)]
fn push_child_rows(
    rows: &mut Vec<AgentSidebarRow>,
    children: &crate::children::ChildSessions,
    project_id: &str,
    parent_session_id: &VibexSessionId,
    depth: u8,
    query: &str,
    searching: bool,
) {
    for node in children.children(parent_session_id.as_str()) {
        let Some(child_id) = node.session_ref.session_id() else {
            continue;
        };
        let session = children.session(child_id.as_str());
        let title = session
            .map(|session| session.title.as_str())
            .unwrap_or(node.title.as_str());
        // A child whose own name does not match is still the way to a
        // grandchild that does; dropping it would hide the match.
        if searching
            && !child_matches(session, title, query)
            && !descendant_matches(children, &child_id, query, 0)
        {
            continue;
        }
        let child_count = children.declared_child_count(child_id.as_str());
        let collapsed = children.is_collapsed(child_id.as_str());
        rows.push(AgentSidebarRow {
            id: format!("session:{}", child_id.as_str()),
            kind: AgentSidebarRowKind::Session,
            project_id: project_id.to_string(),
            workspace_id: session
                .map(|session| session.workspace_id.as_str().to_string())
                .unwrap_or_default(),
            session_id: Some(child_id.clone()),
            label: title.to_string(),
            depth,
            pinned: false,
            selected: false,
            collapsed,
            state: session.map(|session| session.state),
            delegated: true,
            child_count,
            // A child's siblings are its own, not its parent's neighbours: the
            // key names the session the branch hangs from, so a reorder can
            // never treat a worker as a peer of the session that owns it.
            parent_id: Some(format!("session:{}", parent_session_id.as_str())),
        });
        if child_count > 0 && (!collapsed || searching) {
            push_child_rows(
                rows,
                children,
                project_id,
                &child_id,
                depth + 1,
                query,
                searching,
            );
        }
    }
}

/// Whether a child session is one the list is about, for a given filter.
fn child_matches(session: Option<&AgentSession>, title: &str, query: &str) -> bool {
    query.is_empty()
        || title.to_lowercase().contains(query)
        || session.is_some_and(|session| session.workspace_root.to_lowercase().contains(query))
}

/// Whether anything under a session is what the reader is looking for.
fn descendant_matches(
    children: &crate::children::ChildSessions,
    parent: &VibexSessionId,
    query: &str,
    depth: u8,
) -> bool {
    // The runtime bounds ownership depth; this is the guard that keeps a
    // malformed graph from making a filter walk forever.
    if depth > 16 {
        return false;
    }
    children.children(parent.as_str()).iter().any(|node| {
        let Some(child_id) = node.session_ref.session_id() else {
            return false;
        };
        let session = children.session(child_id.as_str());
        let title = session
            .map(|session| session.title.as_str())
            .unwrap_or(node.title.as_str());
        child_matches(session, title, query)
            || descendant_matches(children, &child_id, query, depth + 1)
    })
}

/// The fallback list, with the filter applied where the ownership tree is.
///
/// The shared projection filters a session by its own name alone, which would
/// hide the session a matching worker hangs under and make the worker
/// unreachable. So the projection is asked for everything and the filter is
/// applied here, where the tree is known: a session stays when it matches or
/// when something under it does, a heading stays when its own name matches or
/// when it still has a row, and the children are filtered and nested by the
/// same rules the arranged list uses.
pub fn filtered_child_rows(
    rows: Vec<AgentSidebarRow>,
    loaded: &[AgentSession],
    children: &crate::children::ChildSessions,
    query: &str,
) -> Vec<AgentSidebarRow> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return nest_child_rows(rows, children, "");
    }
    let mut kept = Vec::with_capacity(rows.len());
    // A heading the reader's own name matched keeps everything under it, the
    // way the shared projection keeps a matching project's sessions.
    let mut keep_all_below: Option<u8> = None;
    // Headings wait for the first row that proves them worth drawing.
    let mut pending: Vec<AgentSidebarRow> = Vec::new();
    for row in rows {
        let Some(session_id) = row.session_id.clone() else {
            if keep_all_below.is_some_and(|depth| row.depth > depth) {
                kept.push(row);
                continue;
            }
            if row.label.to_lowercase().contains(&query) {
                keep_all_below = Some(row.depth);
                kept.append(&mut pending);
                kept.push(row);
                continue;
            }
            keep_all_below = None;
            pending.push(row);
            continue;
        };
        let session = loaded
            .iter()
            .find(|session| session.id == session_id)
            .or_else(|| children.session(session_id.as_str()));
        let matches = keep_all_below.is_some()
            || session.is_some_and(|session| session_matches(session, &query))
            || descendant_matches(children, &session_id, &query, 0);
        if matches {
            kept.append(&mut pending);
            kept.push(row);
        }
    }
    nest_child_rows(kept, children, &query)
}

/// Hang each session's children under its row, for a projection built
/// elsewhere.
///
/// The fallback projection is a flat list of headings and sessions, so the
/// ownership rows are spliced in afterwards with the same filter and the same
/// indentation the arranged tree uses. Counts are *not* rebuilt here: the
/// heading tally is counted from the sessions themselves, which is the only way
/// a folded heading can still hold what it hides.
pub fn nest_child_rows(
    rows: Vec<AgentSidebarRow>,
    children: &crate::children::ChildSessions,
    query: &str,
) -> Vec<AgentSidebarRow> {
    let query = query.trim().to_lowercase();
    let searching = !query.is_empty();
    let mut nested = Vec::with_capacity(rows.len());
    for mut row in rows {
        let Some(session_id) = row.session_id.clone() else {
            nested.push(row);
            continue;
        };
        let child_count = children.declared_child_count(session_id.as_str());
        let collapsed = children.is_collapsed(session_id.as_str());
        row.delegated = false;
        row.child_count = child_count;
        row.collapsed = collapsed;
        let project_id = row.project_id.clone();
        let depth = row.depth;
        nested.push(row);
        if child_count == 0 || (collapsed && !searching) {
            continue;
        }
        push_child_rows(
            &mut nested,
            children,
            &project_id,
            &session_id,
            depth + 1,
            &query,
            searching,
        );
    }
    nested
}

/// The branch one delegated row hangs from, read from the row outward: one
/// entry per level below the session that owns it, the row's own level first.
///
/// A row at depth `d` sits under `d - 1` levels of drawn branch. The rightmost
/// column of the guide lands on the Agent mark of the session that owns the row,
/// and carries that session's children down to the next one; each column to its
/// left lands on the mark of one ancestor further up, and carries *its* children
/// down. So entry `i` answers "does the session at depth `d - i` have a sibling
/// after it" — `true` means the trunk in that column runs on past this row, and
/// `false` means the column closes here.
///
/// A column is only drawn while the row that owns its level draws a branch of
/// its own. A session's row is not always a delegated one: the arrangement puts
/// a session at depth 2 when its project sits inside a folder, and that row
/// spends its leading columns on the selection bar rather than on a guide. A
/// trunk in that row's column would then arrive from no line above and reach
/// none below — the one thing a reader cannot tell a tree line from — so the
/// walk stops at the first level whose row is delegated, and a row one level
/// above such a session hangs from its elbow alone.
///
/// The answer is read from the flat rows rather than the ownership pages, which
/// is what makes a folded branch come out right: a folded sibling is not on the
/// list, so the trunk above the last row the reader can see stops where the eye
/// says it should. The walk is bounded, so a malformed projection cannot make a
/// frame run away.
pub fn branch_shape(rows: &[AgentSidebarRow], index: usize) -> Vec<bool> {
    let Some(depth) = rows.get(index).map(|row| row.depth) else {
        return Vec::new();
    };
    if depth < 2 {
        // A row directly under a heading owns no drawn branch: there is no
        // session mark above it to hang an elbow from.
        return Vec::new();
    }
    let mut shape = Vec::new();
    for step in 0..(usize::from(depth) - 1).min(BRANCH_DEPTH_LIMIT) {
        let level = depth - step as u8;
        // Every level but the row's own needs the row that owns it to draw a
        // guide: rows are nested depth-first, so the nearest row above at this
        // depth is the ancestor the column would hang from.
        if step > 0
            && !rows[..index]
                .iter()
                .rev()
                .find(|row| row.depth == level)
                .is_some_and(|row| row.delegated)
        {
            break;
        }
        // The first row at or above this level after the row says whether
        // the session at `level` has a sibling: a row *at* the level is one,
        // and a row above it means the subtree ended.
        shape.push(
            rows.iter()
                .skip(index + 1)
                .find(|row| row.depth <= level)
                .is_some_and(|row| row.depth == level),
        );
    }
    shape
}

/// How deep the renderer will draw a delegated branch.
///
/// The runtime bounds ownership depth; this is the guard that keeps a malformed
/// graph from making a frame walk forever.
pub const BRANCH_DEPTH_LIMIT: usize = 16;

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
            tools: Default::default(),
            transport: vibex_core::SessionTransport::Acp,
            terminal: None,
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
            worktree_lifecycle: BTreeMap::new(),
            project_location_preferences: BTreeMap::new(),
            auto_continue_enabled_by_default: false,
            auto_continue_disabled_project_ids: BTreeSet::new(),
            auto_continue_project_ids: BTreeSet::new(),
            auto_continue_session_overrides: BTreeMap::new(),
            auto_continue_session_ids: BTreeSet::new(),
            auto_continue_paused_session_ids: BTreeSet::new(),
            unread_session_ids: BTreeSet::from(["session_recent01".to_string()]),
            queued_sends: Vec::new(),
            paused_queue_session_ids: BTreeSet::new(),
            manual_queue_session_ids: BTreeSet::new(),
            scheduled_sends: Vec::new(),
        }
    }

    fn list_for(view: &SidebarOrganizationView, query: &str) -> SessionListRows {
        list_with_children(view, query, &crate::children::ChildSessions::default())
    }

    fn list_with_children(
        view: &SidebarOrganizationView,
        query: &str,
        children: &crate::children::ChildSessions,
    ) -> SessionListRows {
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
            children,
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
    fn a_folded_folder_hides_its_rows_and_a_search_still_reaches_them() {
        let view = arranged_view();
        let list = list_for(&view, "");
        assert!(
            !list.rows.iter().any(|row| row.label == "archived away"),
            "a folded folder drew its child: {:#?}",
            list.rows
        );
        assert!(
            list.rows.iter().any(|row| row.label == "archive"),
            "a folded folder lost its own heading: {:#?}",
            list.rows
        );
        // The folder's children are still built, which is what lets the filter
        // find a session the reader cannot see.
        let found = list_for(&view, "archived away");
        assert!(
            labels(&found.rows).contains(&"archived away".to_string()),
            "a search did not reach inside the folder: {:#?}",
            found.rows
        );
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
            children: &crate::children::ChildSessions::default(),
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
            children: &crate::children::ChildSessions::default(),
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
                    mentions: Vec::new(),
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
            children: &crate::children::ChildSessions::default(),
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
            children: &crate::children::ChildSessions::default(),
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
            children: &crate::children::ChildSessions::default(),
            query: "",
        })
        .rows;
        // The project names the session's own root, so the session is not lost
        // behind a listing that has not caught up.
        assert_eq!(labels(&rows), vec!["project-gone", "orphan"], "{rows:#?}");
    }

    /// An ownership page: `nodes` name the children, `sessions` name them for
    /// the list.
    fn tree_page(
        nodes: Vec<vibex_core::SessionTreeNode>,
        registry: Vec<AgentSession>,
    ) -> vibex_core::SessionTreePage {
        vibex_core::SessionTreePage {
            nodes,
            ancestors: Vec::new(),
            registry,
            next_cursor: None,
            has_more: false,
        }
    }

    fn child_node(id: &str, parent: &str, child_count: usize) -> vibex_core::SessionTreeNode {
        vibex_core::SessionTreeNode {
            session_ref: vibex_core::VibexUseRef::session(
                &VibexSessionId::parse(id).expect("valid session id"),
            ),
            parent_session_ref: Some(vibex_core::VibexUseRef::session(
                &VibexSessionId::parse(parent).expect("valid session id"),
            )),
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

    /// A tree holding one child under `session_recent01`, and one grandchild
    /// under that child.
    fn children_tree() -> crate::children::ChildSessions {
        let mut children = crate::children::ChildSessions::default();
        let parent = VibexSessionId::parse("session_recent01").unwrap();
        children.apply(None, false, 1_000, tree_page(Vec::new(), Vec::new()));
        children.apply(
            Some(&parent),
            false,
            1_000,
            tree_page(
                vec![child_node("session_worker001", "session_recent01", 1)],
                vec![session("session_worker001", "vibex", "the worker", 6)],
            ),
        );
        let worker = VibexSessionId::parse("session_worker001").unwrap();
        children.apply(
            Some(&worker),
            false,
            1_000,
            tree_page(
                vec![child_node("session_helper001", "session_worker001", 0)],
                vec![session("session_helper001", "vibex", "the helper", 7)],
            ),
        );
        children
    }

    #[test]
    fn a_delegated_session_hangs_under_the_session_that_owns_it() {
        let view = arranged_view();
        let children = children_tree();
        let list = list_with_children(&view, "", &children);
        let rows = &list.rows;
        // The arranged order is kept, and each worker follows its own owner one
        // indent in.
        assert_eq!(
            labels(rows),
            vec![
                "vibex",
                "kept",
                "recent",
                "the worker",
                "the helper",
                "archive",
                "older",
                "vibex-site",
                "the site",
            ],
            "the branch is not under its owner: {rows:#?}"
        );
        let worker = rows
            .iter()
            .find(|row| row.label == "the worker")
            .expect("the worker's row");
        assert!(worker.delegated, "a child row did not say it is delegated");
        assert_eq!(
            worker.depth, 2,
            "a child row is not indented under its owner"
        );
        assert_eq!(worker.child_count, 1, "a child's own branch was lost");
        let helper = rows
            .iter()
            .find(|row| row.label == "the helper")
            .expect("the helper's row");
        assert_eq!(helper.depth, 3, "the branch stops one level deep");
        // Folding the owner's branch hides both levels at once.
        let mut folded = children.clone();
        folded.toggle_collapsed("session_recent01");
        let list = list_with_children(&view, "", &folded);
        assert_eq!(
            labels(&list.rows),
            vec![
                "vibex",
                "kept",
                "recent",
                "archive",
                "older",
                "vibex-site",
                "the site"
            ],
            "a folded branch was still drawn: {:#?}",
            list.rows
        );
    }

    #[test]
    fn a_search_reaches_inside_a_folded_branch() {
        let view = arranged_view();
        let mut children = children_tree();
        children.toggle_collapsed("session_recent01");
        let list = list_with_children(&view, "helper", &children);
        assert_eq!(
            labels(&list.rows),
            vec!["vibex", "recent", "the worker", "the helper"],
            "a filter did not reach into a folded branch: {:#?}",
            list.rows
        );
    }

    #[test]
    fn the_fallback_projection_nests_the_same_way() {
        let children = children_tree();
        let rows = vec![
            AgentSidebarRow {
                id: "project:project_vibex".to_string(),
                kind: AgentSidebarRowKind::Project,
                project_id: "project_vibex".to_string(),
                workspace_id: String::new(),
                session_id: None,
                label: "vibex".to_string(),
                depth: 0,
                pinned: false,
                selected: false,
                collapsed: false,
                state: None,
                parent_id: None,
                delegated: false,
                child_count: 0,
            },
            AgentSidebarRow {
                id: "session:session_recent01".to_string(),
                kind: AgentSidebarRowKind::Session,
                project_id: "project_vibex".to_string(),
                workspace_id: "workspace_one".to_string(),
                session_id: Some(VibexSessionId::parse("session_recent01").unwrap()),
                label: "recent".to_string(),
                depth: 1,
                pinned: false,
                selected: false,
                collapsed: false,
                state: Some(AgentSessionState::Idle),
                parent_id: None,
                delegated: false,
                child_count: 0,
            },
        ];
        let nested = nest_child_rows(rows, &children, "");
        assert_eq!(
            labels(&nested),
            vec!["vibex", "recent", "the worker", "the helper"],
            "{nested:#?}"
        );
        assert_eq!(nested[1].child_count, 1);
        assert_eq!(nested[1].depth, 1);
        assert!(nested[2].delegated);
        assert_eq!(nested[2].depth, 2);
        assert_eq!(nested[3].depth, 3);
    }

    /// A list of session rows from `(depth, delegated)` pairs, for the branch
    /// readings below.
    fn branch_rows(spec: &[(u8, bool)]) -> Vec<AgentSidebarRow> {
        spec.iter()
            .enumerate()
            .map(|(index, &(depth, delegated))| AgentSidebarRow {
                id: format!("row-{index}"),
                kind: AgentSidebarRowKind::Session,
                project_id: "project_vibex".to_string(),
                workspace_id: String::new(),
                session_id: Some(
                    VibexSessionId::parse(format!("session_brnch{index:04}")).unwrap(),
                ),
                label: "s".to_string(),
                depth,
                pinned: false,
                selected: false,
                collapsed: false,
                state: Some(AgentSessionState::Idle),
                parent_id: None,
                delegated,
                child_count: 0,
            })
            .collect()
    }

    #[test]
    fn a_trunk_runs_past_a_row_only_while_a_sibling_follows_it() {
        // Three workers under one owner, the last of them with a child of its
        // own. Depth 2 rows are the owner's children; depth 3 is the helper.
        let rows = branch_rows(&[(1, false), (2, false), (2, true), (3, true), (2, true)]);
        // The first child has siblings after it, so its trunk runs on.
        assert_eq!(branch_shape(&rows, 1), vec![true]);
        // The second has the sibling that follows it, and so does the last: a
        // trunk runs past each while something is still under the owner.
        assert_eq!(branch_shape(&rows, 2), vec![true]);
        assert_eq!(branch_shape(&rows, 4), vec![false]);
        // The helper is one level deeper. Its own column has nothing after it,
        // so it closes at the elbow; the column of the parent it hangs from runs
        // on to that parent's next sibling.
        assert_eq!(branch_shape(&rows, 3), vec![false, true]);
        // A row under a heading owns no drawn branch.
        assert!(branch_shape(&rows, 0).is_empty());
    }

    /// A column is only drawn while the row that owns its level draws a branch.
    ///
    /// The arrangement nests a session at depth 2 when its project sits inside a
    /// folder, and that row spends its leading columns on the selection bar: it
    /// has no elbow for a trunk to leave. A worker under such a session hangs
    /// from its own elbow alone, however many sessions of that level follow it —
    /// a line in their column would arrive from no row above that drew one.
    #[test]
    fn a_branch_never_holds_a_column_its_own_row_draws_nothing_in() {
        // A project row, a session inside a folder, that session's worker, and
        // the project's next session.
        let rows = branch_rows(&[(1, false), (2, false), (3, true), (2, false)]);
        assert_eq!(branch_shape(&rows, 2), vec![false], "the worker's guide");
        // The same worker under a *delegated* parent keeps the column: its
        // parent draws an elbow there, and the parent's next sibling one too.
        let rows = branch_rows(&[(1, false), (2, true), (3, true), (2, true)]);
        assert_eq!(branch_shape(&rows, 2), vec![false, true], "under a worker");
    }
}
