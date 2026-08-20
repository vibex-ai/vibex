//! Sidebar tree projection and drag arithmetic for the compact client.
//!
//! The Desktop owns the sidebar organization. This module turns a synced
//! [`SidebarOrganizationView`] into the flat row list the phone renders, using
//! the same shared ordering helpers the Desktop draws from, and works out where
//! a finger drag would drop. Keeping it free of GPUI types keeps the tree and
//! the drop rules under test.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{AgentSession, AgentSessionState, VibexSessionId};
use vibex_desktop_model::{
    SidebarOrganizationItem, SidebarOrganizationView, sidebar_project_items, sidebar_root_items,
};

/// Folders may nest, but a runaway parent chain must not build an unbounded
/// row list; the Desktop applies the same ceiling when it renders.
const MAX_FOLDER_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowKind {
    Folder,
    Project,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub item: SidebarOrganizationItem,
    pub kind: SidebarRowKind,
    pub depth: usize,
    pub label: String,
    /// Scope the row lives in: `None` at the sidebar root, otherwise the
    /// project subtree. A move may not cross scopes.
    pub project_id: Option<String>,
    /// Worktree owner for detailed-hierarchy folders and sessions. Compact
    /// clients keep this alongside the project scope so folder creation can
    /// round-trip without losing its worktree owner.
    pub workspace_id: Option<String>,
    pub collapsed: bool,
    pub pinned: bool,
    pub selected: bool,
    pub state: Option<AgentSessionState>,
    pub session_id: Option<VibexSessionId>,
}

impl SidebarRow {
    pub fn id(&self) -> &str {
        self.item.id()
    }
}

/// A project the phone knows about, with the label the Desktop would show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarProject {
    pub id: String,
    pub label: String,
}

pub struct SidebarRowInput<'a> {
    pub view: &'a SidebarOrganizationView,
    pub projects: &'a [SidebarProject],
    pub sessions: &'a [AgentSession],
    pub selected_session_id: Option<&'a VibexSessionId>,
    pub query: &'a str,
}

fn session_matches(session: &AgentSession, query: &str) -> bool {
    query.is_empty()
        || session.title.to_lowercase().contains(query)
        || session.workspace_root.to_lowercase().contains(query)
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
    let project_ids = visible_projects
        .iter()
        .map(|project| project.id.clone())
        .collect::<Vec<_>>();

    let mut rows = Vec::new();
    push_root_children(
        &mut rows,
        &input,
        organization,
        &project_ids,
        &project_labels,
        &sessions_by_project,
        &query,
        None,
        0,
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
    sessions_by_project: &BTreeMap<String, Vec<&AgentSession>>,
    query: &str,
    parent_folder_id: Option<&str>,
    depth: usize,
) {
    if depth > MAX_FOLDER_DEPTH {
        return;
    }
    for item in sidebar_root_items(organization, project_ids, parent_folder_id) {
        match item {
            SidebarOrganizationItem::Project(project_id) => {
                let Some(label) = project_labels.get(&project_id) else {
                    continue;
                };
                let collapsed = input.view.collapsed_project_ids.contains(&project_id);
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Project(project_id.clone()),
                    kind: SidebarRowKind::Project,
                    depth,
                    label: label.clone(),
                    project_id: None,
                    workspace_id: None,
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                push_project_children(
                    rows,
                    input,
                    organization,
                    &project_id,
                    sessions_by_project,
                    query,
                    None,
                    depth + 1,
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
                    label: folder.name.clone(),
                    project_id: None,
                    workspace_id: folder.workspace_id.clone(),
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
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
                    sessions_by_project,
                    query,
                    Some(&folder_id),
                    depth + 1,
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
    sessions_by_project: &BTreeMap<String, Vec<&AgentSession>>,
    query: &str,
    parent_folder_id: Option<&str>,
    depth: usize,
) {
    if depth > MAX_FOLDER_DEPTH {
        return;
    }
    let project_sessions = sessions_by_project
        .get(project_id)
        .cloned()
        .unwrap_or_default();
    let session_ids = project_sessions
        .iter()
        .filter(|session| session_matches(session, query))
        .map(|session| session.id.as_str().to_string())
        .collect::<Vec<_>>();
    for item in sidebar_project_items(
        organization,
        project_id,
        &session_ids,
        &input.view.pinned_session_ids,
        parent_folder_id,
    ) {
        match item {
            SidebarOrganizationItem::Session(session_id) => {
                let Some(session) = project_sessions
                    .iter()
                    .find(|session| session.id.as_str() == session_id)
                else {
                    continue;
                };
                rows.push(SidebarRow {
                    item: SidebarOrganizationItem::Session(session_id.clone()),
                    kind: SidebarRowKind::Session,
                    depth,
                    label: session.title.clone(),
                    project_id: Some(project_id.to_string()),
                    workspace_id: Some(session.workspace_id.as_str().to_string()),
                    collapsed: false,
                    pinned: input.view.pinned_session_ids.contains(&session_id),
                    selected: input
                        .selected_session_id
                        .is_some_and(|selected| selected.as_str() == session_id),
                    state: Some(session.state),
                    session_id: Some(session.id.clone()),
                });
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
                    label: folder.name.clone(),
                    project_id: Some(project_id.to_string()),
                    workspace_id: folder.workspace_id.clone(),
                    collapsed,
                    pinned: false,
                    selected: false,
                    state: None,
                    session_id: None,
                });
                if collapsed && query.is_empty() {
                    continue;
                }
                push_project_children(
                    rows,
                    input,
                    organization,
                    project_id,
                    sessions_by_project,
                    query,
                    Some(&folder_id),
                    depth + 1,
                );
            }
            SidebarOrganizationItem::Project(_) => {}
        }
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
            SidebarRowKind::Session => SidebarOrganizationItem::Session(id.to_string()),
        };
        SidebarRow {
            item,
            kind,
            depth,
            label: id.to_string(),
            project_id: None,
            workspace_id: None,
            collapsed: false,
            pinned: false,
            selected: false,
            state: None,
            session_id: None,
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
            sessions: &sessions,
            selected_session_id: None,
            query: "",
        });
        assert_eq!(collapsed.len(), 1);
        let searched = sidebar_rows(SidebarRowInput {
            view: &view,
            projects: &projects,
            sessions: &sessions,
            selected_session_id: None,
            query: "hello",
        });
        assert_eq!(searched.len(), 2);
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
}
