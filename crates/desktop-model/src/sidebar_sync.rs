//! Wire translation for the sidebar tree.
//!
//! The Desktop owns the sidebar organization; compact clients mirror it. Both
//! shells go through this module so a phone renders the tree the user arranged
//! on the Desktop, and a phone-originated drag lands through exactly the same
//! [`SidebarOrganizationState`] transitions a Desktop drag would take.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{
    RemoteSidebarDropPosition, RemoteSidebarFolder, RemoteSidebarHierarchyMode,
    RemoteSidebarItemKind, RemoteSidebarItemRef, RemoteSidebarNewSessionLocation,
    RemoteSidebarOrganizationMutation, RemoteSidebarOrganizationSnapshot, RemoteSidebarPlacement,
    RemoteSidebarProjectAppearance,
};

use crate::{
    NewSessionLocation, SidebarHierarchyMode, SidebarOrganizationItem, SidebarOrganizationScope,
    SidebarOrganizationState, SidebarProjectAppearance,
};

/// The subset of sidebar view state a compact client needs to draw the tree.
/// The Desktop keeps these in separate places, so they travel together here
/// rather than as four independent round trips that could disagree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidebarOrganizationView {
    pub revision: u64,
    pub organization: SidebarOrganizationState,
    pub collapsed_project_ids: BTreeSet<String>,
    pub collapsed_workspace_ids: BTreeSet<String>,
    pub pinned_session_ids: BTreeSet<String>,
    pub session_order: Vec<String>,
    /// Wall-clock (ms) of the last manual session arrangement; see
    /// [`crate::SidebarUiState::session_order_anchored_at_ms`].
    pub session_order_anchored_at_ms: i64,
    pub hierarchy_mode: SidebarHierarchyMode,
    pub project_order: Vec<String>,
    pub workspace_order: BTreeMap<String, Vec<String>>,
    pub project_appearances: BTreeMap<String, SidebarProjectAppearance>,
    pub worktree_titles: BTreeMap<String, String>,
    pub project_location_preferences: BTreeMap<String, NewSessionLocation>,
    pub auto_continue_project_ids: BTreeSet<String>,
    pub auto_continue_session_overrides: BTreeMap<String, bool>,
    pub auto_continue_session_ids: BTreeSet<String>,
    pub auto_continue_paused_session_ids: BTreeSet<String>,
    pub unread_session_ids: BTreeSet<String>,
}

/// Why the Desktop refused a client-originated sidebar change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMutationRejection {
    /// The client acted on a tree the Desktop has since changed.
    StaleRevision,
    /// The move or folder edit is not legal for the current tree.
    Rejected,
}

impl SidebarMutationRejection {
    pub const fn stable_code(self) -> &'static str {
        match self {
            Self::StaleRevision => "remote_sidebar_organization_stale_revision",
            Self::Rejected => "remote_sidebar_organization_rejected",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::StaleRevision => {
                "the sidebar changed on the desktop after this device rendered it"
            }
            Self::Rejected => "the desktop rejected this sidebar change",
        }
    }
}

fn item_to_remote(item: &SidebarOrganizationItem) -> RemoteSidebarItemRef {
    let (kind, id) = match item {
        SidebarOrganizationItem::Folder(id) => (RemoteSidebarItemKind::Folder, id),
        SidebarOrganizationItem::Project(id) => (RemoteSidebarItemKind::Project, id),
        SidebarOrganizationItem::Session(id) => (RemoteSidebarItemKind::Session, id),
    };
    RemoteSidebarItemRef {
        kind,
        id: id.clone(),
    }
}

pub fn item_from_remote(item: &RemoteSidebarItemRef) -> SidebarOrganizationItem {
    match item.kind {
        RemoteSidebarItemKind::Folder => SidebarOrganizationItem::Folder(item.id.clone()),
        RemoteSidebarItemKind::Project => SidebarOrganizationItem::Project(item.id.clone()),
        RemoteSidebarItemKind::Session => SidebarOrganizationItem::Session(item.id.clone()),
    }
}

impl SidebarOrganizationView {
    pub fn to_remote(&self) -> RemoteSidebarOrganizationSnapshot {
        RemoteSidebarOrganizationSnapshot {
            revision: self.revision,
            folders: self
                .organization
                .folders
                .iter()
                .map(|(id, folder)| RemoteSidebarFolder {
                    id: id.clone(),
                    name: folder.name.clone(),
                    project_id: folder.project_id.clone(),
                    workspace_id: folder.workspace_id.clone(),
                    auto_archive_after_days: folder.auto_archive_after_days,
                })
                .collect(),
            placements: self
                .organization
                .placements
                .iter()
                .map(|placement| RemoteSidebarPlacement {
                    item: item_to_remote(&placement.item),
                    parent_folder_id: placement.parent_folder_id.clone(),
                })
                .collect(),
            collapsed_folder_ids: self
                .organization
                .collapsed_folder_ids
                .iter()
                .cloned()
                .collect(),
            collapsed_project_ids: self.collapsed_project_ids.iter().cloned().collect(),
            collapsed_workspace_ids: self.collapsed_workspace_ids.iter().cloned().collect(),
            pinned_session_ids: self.pinned_session_ids.iter().cloned().collect(),
            session_order: self.session_order.clone(),
            session_order_anchored_at_ms: self.session_order_anchored_at_ms,
            hierarchy_mode: match self.hierarchy_mode {
                SidebarHierarchyMode::Detailed => RemoteSidebarHierarchyMode::Detailed,
                SidebarHierarchyMode::Compact => RemoteSidebarHierarchyMode::Compact,
            },
            project_order: self.project_order.clone(),
            workspace_order: self.workspace_order.clone(),
            project_appearances: self
                .project_appearances
                .iter()
                .map(|(id, appearance)| {
                    (
                        id.clone(),
                        RemoteSidebarProjectAppearance {
                            logo: format!("{:?}", appearance.logo).to_ascii_lowercase(),
                            color: format!("{:?}", appearance.color).to_ascii_lowercase(),
                            custom_logo_file: appearance.custom_logo_file.clone(),
                        },
                    )
                })
                .collect(),
            worktree_titles: self.worktree_titles.clone(),
            project_new_session_locations: self
                .project_location_preferences
                .iter()
                .map(|(id, location)| {
                    (
                        id.clone(),
                        match location {
                            NewSessionLocation::NewWorktree => {
                                RemoteSidebarNewSessionLocation::NewWorktree
                            }
                            NewSessionLocation::CurrentCheckout => {
                                RemoteSidebarNewSessionLocation::CurrentCheckout
                            }
                        },
                    )
                })
                .collect(),
            auto_continue_project_ids: self.auto_continue_project_ids.iter().cloned().collect(),
            auto_continue_session_overrides: self.auto_continue_session_overrides.clone(),
            auto_continue_session_ids: self.auto_continue_session_ids.iter().cloned().collect(),
            auto_continue_paused_session_ids: self
                .auto_continue_paused_session_ids
                .iter()
                .cloned()
                .collect(),
            unread_session_ids: self.unread_session_ids.iter().cloned().collect(),
        }
    }

    pub fn from_remote(snapshot: &RemoteSidebarOrganizationSnapshot) -> Self {
        let mut organization = SidebarOrganizationState {
            folders: snapshot
                .folders
                .iter()
                .map(|folder| {
                    (
                        folder.id.clone(),
                        crate::SidebarFolderUiState {
                            name: folder.name.clone(),
                            project_id: folder.project_id.clone(),
                            workspace_id: folder.workspace_id.clone(),
                            auto_archive_after_days: folder.auto_archive_after_days,
                        },
                    )
                })
                .collect(),
            placements: snapshot
                .placements
                .iter()
                .map(|placement| crate::SidebarOrganizationPlacement {
                    item: item_from_remote(&placement.item),
                    parent_folder_id: placement.parent_folder_id.clone(),
                })
                .collect(),
            collapsed_folder_ids: snapshot.collapsed_folder_ids.iter().cloned().collect(),
        };
        organization.normalize();
        Self {
            revision: snapshot.revision,
            organization,
            collapsed_project_ids: snapshot.collapsed_project_ids.iter().cloned().collect(),
            collapsed_workspace_ids: snapshot.collapsed_workspace_ids.iter().cloned().collect(),
            pinned_session_ids: snapshot.pinned_session_ids.iter().cloned().collect(),
            session_order: snapshot.session_order.clone(),
            session_order_anchored_at_ms: snapshot.session_order_anchored_at_ms,
            hierarchy_mode: match snapshot.hierarchy_mode {
                RemoteSidebarHierarchyMode::Detailed => SidebarHierarchyMode::Detailed,
                RemoteSidebarHierarchyMode::Compact => SidebarHierarchyMode::Compact,
            },
            project_order: snapshot.project_order.clone(),
            workspace_order: snapshot.workspace_order.clone(),
            project_appearances: snapshot
                .project_appearances
                .iter()
                .map(|(id, appearance)| {
                    (
                        id.clone(),
                        SidebarProjectAppearance {
                            // Unknown values can come from a newer desktop;
                            // preserve the record (including a custom image)
                            // while falling back to the stable defaults.
                            logo: parse_project_logo(&appearance.logo).unwrap_or_default(),
                            color: parse_project_logo_color(&appearance.color).unwrap_or_default(),
                            custom_logo_file: appearance.custom_logo_file.clone(),
                        },
                    )
                })
                .collect(),
            worktree_titles: snapshot.worktree_titles.clone(),
            project_location_preferences: snapshot
                .project_new_session_locations
                .iter()
                .map(|(id, location)| {
                    (
                        id.clone(),
                        match location {
                            RemoteSidebarNewSessionLocation::NewWorktree => {
                                NewSessionLocation::NewWorktree
                            }
                            RemoteSidebarNewSessionLocation::CurrentCheckout => {
                                NewSessionLocation::CurrentCheckout
                            }
                        },
                    )
                })
                .collect(),
            auto_continue_project_ids: snapshot.auto_continue_project_ids.iter().cloned().collect(),
            auto_continue_session_overrides: snapshot.auto_continue_session_overrides.clone(),
            auto_continue_session_ids: snapshot.auto_continue_session_ids.iter().cloned().collect(),
            auto_continue_paused_session_ids: snapshot
                .auto_continue_paused_session_ids
                .iter()
                .cloned()
                .collect(),
            unread_session_ids: snapshot.unread_session_ids.iter().cloned().collect(),
        }
    }

    /// Applies a client-originated change. `new_folder_id` is supplied by the
    /// caller so id minting stays with the Desktop, which owns the tree.
    /// Returns the fields that changed so callers persist only what moved.
    pub fn apply_remote(
        &mut self,
        mutation: &RemoteSidebarOrganizationMutation,
        session_projects: &BTreeMap<String, String>,
        new_folder_id: &str,
    ) -> Result<SidebarMutationEffect, SidebarMutationRejection> {
        let changed = match mutation {
            RemoteSidebarOrganizationMutation::MoveItems {
                items,
                anchor,
                position,
                project_id,
            } => {
                let moving = items.iter().map(item_from_remote).collect::<Vec<_>>();
                if moving.is_empty() {
                    return Err(SidebarMutationRejection::Rejected);
                }
                let moved = match (anchor, position) {
                    (Some(anchor), RemoteSidebarDropPosition::Into) => {
                        let RemoteSidebarItemKind::Folder = anchor.kind else {
                            return Err(SidebarMutationRejection::Rejected);
                        };
                        self.organization
                            .move_many_into(&moving, &anchor.id, session_projects)
                    }
                    (Some(anchor), position) => self.organization.move_many_relative(
                        &moving,
                        &item_from_remote(anchor),
                        matches!(position, RemoteSidebarDropPosition::After),
                        session_projects,
                    ),
                    (None, _) => {
                        let scope = project_id.clone().map_or(
                            SidebarOrganizationScope::Root,
                            SidebarOrganizationScope::Project,
                        );
                        self.organization.move_many_to_scope_root_end(
                            &moving,
                            &scope,
                            session_projects,
                        )
                    }
                };
                if !moved {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION
            }
            RemoteSidebarOrganizationMutation::MoveWorkspaces {
                project_id,
                workspace_ids,
                anchor_workspace_id,
                position,
            } => {
                let after = match position {
                    RemoteSidebarDropPosition::Before => false,
                    RemoteSidebarDropPosition::After => true,
                    RemoteSidebarDropPosition::Into => {
                        return Err(SidebarMutationRejection::Rejected);
                    }
                };
                let Some(order) = self.workspace_order.get_mut(project_id) else {
                    return Err(SidebarMutationRejection::Rejected);
                };
                if !move_workspace_ids_relative(
                    order,
                    workspace_ids,
                    anchor_workspace_id.as_deref(),
                    after,
                ) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::CreateFolder {
                name,
                project_id,
                workspace_id,
                parent_folder_id,
            } => {
                if !self.organization.create_folder_with_workspace(
                    new_folder_id,
                    name.trim(),
                    project_id.clone(),
                    workspace_id.clone(),
                    parent_folder_id.clone(),
                ) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                if let Some(parent_folder_id) = parent_folder_id {
                    self.organization
                        .collapsed_folder_ids
                        .remove(parent_folder_id);
                }
                SidebarMutationEffect::ORGANIZATION
            }
            RemoteSidebarOrganizationMutation::RenameFolder { folder_id, name } => {
                if !self.organization.rename_folder(folder_id, name.trim()) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION
            }
            RemoteSidebarOrganizationMutation::DeleteFolder { folder_id } => {
                if !self.organization.delete_folder(folder_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION
            }
            RemoteSidebarOrganizationMutation::SetFolderCollapsed {
                folder_id,
                collapsed,
            } => {
                if !self.organization.folders.contains_key(folder_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                let changed = if *collapsed {
                    self.organization
                        .collapsed_folder_ids
                        .insert(folder_id.clone())
                } else {
                    self.organization.collapsed_folder_ids.remove(folder_id)
                };
                if !changed {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION
            }
            RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                project_id,
                collapsed,
            } => {
                let changed = if *collapsed {
                    self.collapsed_project_ids.insert(project_id.clone())
                } else {
                    self.collapsed_project_ids.remove(project_id)
                };
                if !changed {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::SetWorkspaceCollapsed {
                workspace_id,
                collapsed,
            } => {
                let changed = if *collapsed {
                    self.collapsed_workspace_ids.insert(workspace_id.clone())
                } else {
                    self.collapsed_workspace_ids.remove(workspace_id)
                };
                if !changed {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::SetSessionPinned { session_id, pinned } => {
                if !session_projects.contains_key(session_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                let changed = if *pinned {
                    self.pinned_session_ids.insert(session_id.clone())
                } else {
                    self.pinned_session_ids.remove(session_id)
                };
                if !changed {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::SetSessionAutoContinue {
                session_id,
                enabled,
            } => {
                if !session_projects.contains_key(session_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                if *enabled {
                    self.auto_continue_session_ids.insert(session_id.clone());
                } else {
                    self.auto_continue_session_ids.remove(session_id);
                }
                self.auto_continue_session_overrides
                    .insert(session_id.clone(), *enabled);
                // An explicit enable/disable always clears a pause; the
                // suspension is only set by pausing or stopping a session.
                self.auto_continue_paused_session_ids.remove(session_id);
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::SetWorktreeTitle {
                workspace_id,
                title,
            } => {
                let title = title.trim();
                if title.is_empty()
                    || title.chars().count() > 160
                    || title.chars().any(char::is_control)
                {
                    return Err(SidebarMutationRejection::Rejected);
                }
                self.worktree_titles
                    .insert(workspace_id.clone(), title.to_string());
                SidebarMutationEffect::NAVIGATION
            }
            RemoteSidebarOrganizationMutation::SetHierarchyMode { mode } => {
                let next = match mode {
                    RemoteSidebarHierarchyMode::Detailed => SidebarHierarchyMode::Detailed,
                    RemoteSidebarHierarchyMode::Compact => SidebarHierarchyMode::Compact,
                };
                if self.hierarchy_mode == next {
                    return Err(SidebarMutationRejection::Rejected);
                }
                self.hierarchy_mode = next;
                SidebarMutationEffect::NAVIGATION
            }
        };
        self.revision = self.revision.wrapping_add(1);
        Ok(changed)
    }
}

fn parse_project_logo(value: &str) -> Option<crate::SidebarProjectLogo> {
    crate::SidebarProjectLogo::ALL
        .into_iter()
        .find(|logo| format!("{:?}", logo).eq_ignore_ascii_case(value))
}

fn parse_project_logo_color(value: &str) -> Option<crate::SidebarProjectLogoColor> {
    crate::SidebarProjectLogoColor::ALL
        .into_iter()
        .find(|color| format!("{:?}", color).eq_ignore_ascii_case(value))
}

fn move_workspace_ids_relative(
    order: &mut Vec<String>,
    moving_ids: &[String],
    anchor_id: Option<&str>,
    after: bool,
) -> bool {
    if moving_ids.is_empty() {
        return false;
    }
    let moving_set = moving_ids.iter().collect::<BTreeSet<_>>();
    if moving_set.len() != moving_ids.len()
        || anchor_id.is_some_and(|anchor| moving_ids.iter().any(|id| id == anchor))
        || moving_ids
            .iter()
            .any(|id| !order.iter().any(|candidate| candidate == id))
    {
        return false;
    }
    if let Some(anchor) = anchor_id
        && !order.iter().any(|candidate| candidate == anchor)
    {
        return false;
    }

    let original = order.clone();
    let moving_order = order
        .iter()
        .filter(|id| moving_set.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    order.retain(|id| !moving_set.contains(id));
    let insert_at = anchor_id.map_or(order.len(), |anchor| {
        order
            .iter()
            .position(|id| id == anchor)
            .map(|index| if after { index + 1 } else { index })
            .unwrap_or(order.len())
    });
    order.splice(insert_at..insert_at, moving_order);
    *order != original
}

/// Root-level children under `parent_folder_id`: root folders and projects, in
/// the order the user arranged. Both shells build the sidebar from this so the
/// tree cannot drift between them.
pub fn sidebar_root_items(
    organization: &SidebarOrganizationState,
    project_ids: &[String],
    parent_folder_id: Option<&str>,
) -> Vec<SidebarOrganizationItem> {
    let mut available = organization
        .folders
        .iter()
        .filter(|(_, folder)| folder.project_id.is_none())
        .map(|(id, _)| SidebarOrganizationItem::Folder(id.clone()))
        .collect::<Vec<_>>();
    available.extend(
        project_ids
            .iter()
            .map(|id| SidebarOrganizationItem::Project(id.clone())),
    );
    organization.ordered_children(parent_folder_id, &available)
}

/// Children of a project under `parent_folder_id`: its folders and sessions,
/// with pinned sessions hoisted to the front the way the Desktop shows them.
pub fn sidebar_project_items(
    organization: &SidebarOrganizationState,
    project_id: &str,
    session_ids: &[String],
    pinned_session_ids: &BTreeSet<String>,
    parent_folder_id: Option<&str>,
) -> Vec<SidebarOrganizationItem> {
    sidebar_project_items_for_workspace(
        organization,
        project_id,
        None,
        true,
        true,
        session_ids,
        pinned_session_ids,
        parent_folder_id,
    )
}

/// Returns children for one workspace in detailed hierarchy mode. Folders
/// created before workspace ownership existed remain visible through the
/// legacy `sidebar_project_items` path; detailed mode only pulls folders that
/// explicitly belong to this workspace. Compact callers may opt into showing
/// all project folders so older clients do not hide newly scoped folders.
pub fn sidebar_project_items_for_workspace(
    organization: &SidebarOrganizationState,
    project_id: &str,
    workspace_id: Option<&str>,
    include_legacy_project_folders: bool,
    include_all_workspace_folders: bool,
    session_ids: &[String],
    pinned_session_ids: &BTreeSet<String>,
    parent_folder_id: Option<&str>,
) -> Vec<SidebarOrganizationItem> {
    let mut available = organization
        .folders
        .iter()
        .filter(|(_, folder)| {
            folder.project_id.as_deref() == Some(project_id)
                && (folder.workspace_id.as_deref() == workspace_id
                    || (include_all_workspace_folders && workspace_id.is_none())
                    || (include_legacy_project_folders
                        && folder.workspace_id.is_none()
                        && workspace_id.is_none()))
        })
        .map(|(id, _)| SidebarOrganizationItem::Folder(id.clone()))
        .collect::<Vec<_>>();
    available.extend(
        session_ids
            .iter()
            .map(|id| SidebarOrganizationItem::Session(id.clone())),
    );
    let ordered = organization.ordered_children(parent_folder_id, &available);
    let (pinned, rest): (Vec<_>, Vec<_>) = ordered.into_iter().partition(
        |item| matches!(item, SidebarOrganizationItem::Session(id) if pinned_session_ids.contains(id)),
    );
    pinned.into_iter().chain(rest).collect()
}

/// Which half of the sidebar view a mutation touched. The Desktop persists the
/// organization tree and the navigation flags through different paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarMutationEffect {
    pub organization: bool,
    pub navigation: bool,
}

impl SidebarMutationEffect {
    pub const ORGANIZATION: Self = Self {
        organization: true,
        navigation: false,
    };
    pub const NAVIGATION: Self = Self {
        organization: false,
        navigation: true,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_projects() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("session-a".to_string(), "project-1".to_string()),
            ("session-b".to_string(), "project-1".to_string()),
        ])
    }

    fn view_with_folder() -> SidebarOrganizationView {
        let mut view = SidebarOrganizationView::default();
        for session in ["session-a", "session-b"] {
            view.organization
                .placements
                .push(crate::SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Session(session.to_string()),
                    parent_folder_id: None,
                });
        }
        assert!(view.organization.create_folder(
            "folder-1",
            "Archive",
            Some("project-1".to_string()),
            None,
        ));
        view
    }

    #[test]
    fn a_snapshot_round_trips_through_the_wire_representation() {
        let view = view_with_folder();
        let restored = SidebarOrganizationView::from_remote(&view.to_remote());
        assert_eq!(restored.organization.folders, view.organization.folders);
        assert_eq!(
            restored.organization.placements,
            view.organization.placements
        );
        assert_eq!(restored.revision, view.revision);
    }

    #[test]
    fn unknown_project_appearance_values_keep_custom_logo_metadata() {
        let mut snapshot = vibex_core::RemoteSidebarOrganizationSnapshot::default();
        snapshot.project_appearances.insert(
            "project-1".to_string(),
            vibex_core::RemoteSidebarProjectAppearance {
                logo: "future_logo".to_string(),
                color: "future_color".to_string(),
                custom_logo_file: Some(format!("{}.png", "a".repeat(64))),
            },
        );

        let restored = SidebarOrganizationView::from_remote(&snapshot);
        let appearance = restored
            .project_appearances
            .get("project-1")
            .expect("unknown appearance values should not drop the record");
        assert_eq!(appearance.logo, crate::SidebarProjectLogo::Boxes);
        assert_eq!(appearance.color, crate::SidebarProjectLogoColor::Neutral);
        assert!(appearance.custom_logo_file.is_some());
    }

    #[test]
    fn workspace_folder_projection_isolated_from_sibling_worktrees() {
        let mut view = SidebarOrganizationView::default();
        assert!(view.organization.create_folder_with_workspace(
            "folder-a",
            "A",
            Some("project-1".into()),
            Some("workspace-a".into()),
            None,
        ));
        assert!(view.organization.create_folder_with_workspace(
            "folder-b",
            "B",
            Some("project-1".into()),
            Some("workspace-b".into()),
            None,
        ));
        assert!(view.organization.create_folder(
            "folder-legacy",
            "Legacy",
            Some("project-1".into()),
            None,
        ));
        let sessions = ["session-a".to_string(), "session-b".to_string()];
        let scoped = sidebar_project_items_for_workspace(
            &view.organization,
            "project-1",
            Some("workspace-a"),
            false,
            false,
            &sessions,
            &BTreeSet::new(),
            None,
        );
        assert!(scoped.contains(&SidebarOrganizationItem::Folder("folder-a".into())));
        assert!(!scoped.contains(&SidebarOrganizationItem::Folder("folder-b".into())));
        assert!(!scoped.contains(&SidebarOrganizationItem::Folder("folder-legacy".into())));

        let compact = sidebar_project_items(
            &view.organization,
            "project-1",
            &sessions,
            &BTreeSet::new(),
            None,
        );
        assert!(compact.contains(&SidebarOrganizationItem::Folder("folder-a".into())));
        assert!(compact.contains(&SidebarOrganizationItem::Folder("folder-b".into())));
        assert!(compact.contains(&SidebarOrganizationItem::Folder("folder-legacy".into())));
    }

    #[test]
    fn a_remote_drag_moves_a_session_into_a_folder_and_bumps_the_revision() {
        let mut view = view_with_folder();
        let before = view.revision;
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::MoveItems {
                    items: vec![RemoteSidebarItemRef {
                        kind: RemoteSidebarItemKind::Session,
                        id: "session-a".to_string(),
                    }],
                    anchor: Some(RemoteSidebarItemRef {
                        kind: RemoteSidebarItemKind::Folder,
                        id: "folder-1".to_string(),
                    }),
                    position: RemoteSidebarDropPosition::Into,
                    project_id: Some("project-1".to_string()),
                },
                &session_projects(),
                "unused",
            )
            .expect("moving a session into a project folder is legal");
        assert!(effect.organization);
        assert_eq!(view.revision, before + 1);
        assert_eq!(
            view.organization
                .parent_of(&SidebarOrganizationItem::Session("session-a".to_string())),
            Some("folder-1".to_string())
        );
    }

    #[test]
    fn an_illegal_move_is_rejected_without_touching_the_tree() {
        let mut view = view_with_folder();
        let before = view.organization.placements.clone();
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::MoveItems {
                items: vec![RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Folder,
                    id: "folder-1".to_string(),
                }],
                anchor: Some(RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Folder,
                    id: "folder-1".to_string(),
                }),
                position: RemoteSidebarDropPosition::Into,
                project_id: Some("project-1".to_string()),
            },
            &session_projects(),
            "unused",
        );
        assert_eq!(outcome, Err(SidebarMutationRejection::Rejected));
        assert_eq!(view.organization.placements, before);
        assert_eq!(view.revision, 0);
    }

    #[test]
    fn pinning_an_unknown_session_is_refused() {
        let mut view = view_with_folder();
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetSessionPinned {
                    session_id: "missing".to_string(),
                    pinned: true,
                },
                &session_projects(),
                "unused",
            ),
            Err(SidebarMutationRejection::Rejected)
        );
    }

    #[test]
    fn collapsing_a_project_reports_a_navigation_effect() {
        let mut view = view_with_folder();
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                    project_id: "project-1".to_string(),
                    collapsed: true,
                },
                &session_projects(),
                "unused",
            )
            .expect("collapsing a project is always legal");
        assert!(effect.navigation && !effect.organization);
        assert!(view.collapsed_project_ids.contains("project-1"));
    }

    #[test]
    fn moving_workspaces_updates_the_persisted_project_order() {
        let mut view = view_with_folder();
        view.workspace_order.insert(
            "project-1".to_string(),
            vec![
                "workspace-a".to_string(),
                "workspace-b".to_string(),
                "workspace-c".to_string(),
            ],
        );
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::MoveWorkspaces {
                    project_id: "project-1".to_string(),
                    workspace_ids: vec!["workspace-a".to_string()],
                    anchor_workspace_id: Some("workspace-c".to_string()),
                    position: RemoteSidebarDropPosition::After,
                },
                &session_projects(),
                "unused",
            )
            .expect("workspace reorder should be legal");
        assert!(effect.navigation && !effect.organization);
        assert_eq!(
            view.workspace_order.get("project-1"),
            Some(&vec![
                "workspace-b".to_string(),
                "workspace-c".to_string(),
                "workspace-a".to_string(),
            ])
        );
    }
}
