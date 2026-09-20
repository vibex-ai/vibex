use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vibex_core::AgentSession;

use crate::{SESSION_GROUP_NAME_MAX_CHARS, SessionGroupUiState};

const SIDEBAR_FOLDER_LIMIT: usize = 2_000;
const SIDEBAR_GROUP_LIMIT: usize = 500;
const SIDEBAR_ORGANIZATION_ITEM_LIMIT: usize = 5_000;
const SIDEBAR_FOLDER_DEPTH_LIMIT: usize = 32;
const SIDEBAR_ITEM_ID_MAX_CHARS: usize = 256;
const SIDEBAR_FOLDER_NAME_MAX_CHARS: usize = 160;
const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;

pub const SIDEBAR_AUTO_ARCHIVE_MAX_DAYS: u8 = 14;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SidebarOrganizationItem {
    Folder(String),
    Project(String),
    Session(String),
    /// A session group row. Unlike a folder it carries a workspace of its own,
    /// so it is a leaf in the organization tree: it may sit inside a folder but
    /// nothing may sit inside it.
    Group(String),
}

impl SidebarOrganizationItem {
    pub fn id(&self) -> &str {
        match self {
            Self::Folder(id) | Self::Project(id) | Self::Session(id) | Self::Group(id) => id,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarFolderUiState {
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// A detailed-hierarchy folder may be scoped to one workspace/worktree.
    /// `None` preserves the legacy project-level folder representation.
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_archive_after_days: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarOrganizationPlacement {
    pub item: SidebarOrganizationItem,
    #[serde(default)]
    pub parent_folder_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SidebarOrganizationScope {
    Root,
    Project(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SidebarOrganizationState {
    #[serde(default)]
    pub folders: BTreeMap<String, SidebarFolderUiState>,
    /// Session groups, keyed by group id. A group is authoritative for its own
    /// membership and workspace layout, so it travels with the organization
    /// tree that also owns its placement.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<String, SessionGroupUiState>,
    #[serde(default)]
    pub placements: Vec<SidebarOrganizationPlacement>,
    #[serde(default)]
    pub collapsed_folder_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub collapsed_group_ids: BTreeSet<String>,
}

impl SidebarOrganizationState {
    pub fn normalize(&mut self) {
        let mut folders = BTreeMap::new();
        for (id, folder) in std::mem::take(&mut self.folders) {
            if folders.len() >= SIDEBAR_FOLDER_LIMIT {
                break;
            }
            let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                continue;
            };
            let Some(name) = bounded_text(&folder.name, SIDEBAR_FOLDER_NAME_MAX_CHARS) else {
                continue;
            };
            let project_id = folder
                .project_id
                .and_then(|id| bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS));
            let workspace_id = folder
                .workspace_id
                .and_then(|id| bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS))
                .filter(|_| project_id.is_some());
            let auto_archive_after_days = folder.auto_archive_after_days.filter(|days| {
                project_id.is_some() && (1..=SIDEBAR_AUTO_ARCHIVE_MAX_DAYS).contains(days)
            });
            folders.entry(id).or_insert(SidebarFolderUiState {
                name,
                project_id,
                workspace_id,
                auto_archive_after_days,
            });
        }
        self.folders = folders;

        let mut groups = BTreeMap::new();
        for (id, mut group) in std::mem::take(&mut self.groups) {
            if groups.len() >= SIDEBAR_GROUP_LIMIT {
                break;
            }
            let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                continue;
            };
            group.normalize();
            if group.project_id.is_empty() || group.workspace_id.is_empty() {
                continue;
            }
            groups.entry(id).or_insert(group);
        }
        self.groups = groups;

        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        let valid_group_ids = self.groups.keys().cloned().collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        self.placements = std::mem::take(&mut self.placements)
            .into_iter()
            .filter_map(|placement| {
                normalize_placement(placement, &valid_folder_ids, &valid_group_ids)
            })
            .filter(|placement| seen.insert(placement.item.clone()))
            .take(SIDEBAR_ORGANIZATION_ITEM_LIMIT)
            .collect();
        self.ensure_folder_placements();
        self.ensure_group_placements();
        self.enforce_placement_limit();
        self.repair_parents();
        self.enforce_one_auto_archive_folder_per_project();
        self.deduplicate_sibling_folder_names();
        self.deduplicate_sibling_group_names();
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
        self.collapsed_group_ids
            .retain(|id| self.groups.contains_key(id));
    }

    pub fn reconcile(
        &mut self,
        ordered_project_ids: &[String],
        ordered_session_projects: &[(String, String)],
    ) {
        self.normalize();
        let valid_project_ids = ordered_project_ids.iter().cloned().collect::<BTreeSet<_>>();
        let session_projects = ordered_session_projects
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();

        self.folders.retain(|_, folder| {
            folder
                .project_id
                .as_ref()
                .is_none_or(|project_id| valid_project_ids.contains(project_id))
        });

        // A group follows its project and its members. Losing the project drops
        // the group; losing every member drops the group too, because a group
        // with no sessions has no workspace to show. A deleted Worktree deletes
        // its sessions, so this same pass dissolves the groups anchored to it.
        self.groups
            .retain(|_, group| valid_project_ids.contains(&group.project_id));
        for group in self.groups.values_mut() {
            let before = group.member_session_ids.len();
            group
                .member_session_ids
                .retain(|session_id| session_projects.contains_key(session_id));
            if group.member_session_ids.len() != before {
                group.normalize();
            }
        }
        self.groups.retain(|_, group| !group.is_empty());
        let valid_group_ids = self.groups.keys().cloned().collect::<BTreeSet<_>>();

        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        self.placements.retain(|placement| match &placement.item {
            SidebarOrganizationItem::Folder(id) => valid_folder_ids.contains(id),
            SidebarOrganizationItem::Project(id) => valid_project_ids.contains(id),
            SidebarOrganizationItem::Session(id) => session_projects.contains_key(id),
            SidebarOrganizationItem::Group(id) => valid_group_ids.contains(id),
        });
        self.ensure_folder_placements();
        self.ensure_group_placements();
        self.repair_parents();

        for placement in &mut self.placements {
            let valid_parent = placement.parent_folder_id.as_ref().is_none_or(|parent_id| {
                let Some(parent) = self.folders.get(parent_id) else {
                    return false;
                };
                match &placement.item {
                    SidebarOrganizationItem::Folder(folder_id) => {
                        self.folders.get(folder_id).is_some_and(|folder| {
                            folder.project_id == parent.project_id
                                && folder.workspace_id == parent.workspace_id
                        })
                    }
                    SidebarOrganizationItem::Project(_) => parent.project_id.is_none(),
                    SidebarOrganizationItem::Session(session_id) => session_projects
                        .get(session_id)
                        .is_some_and(|project_id| parent.project_id.as_ref() == Some(project_id)),
                    SidebarOrganizationItem::Group(group_id) => self
                        .groups
                        .get(group_id)
                        .is_some_and(|group| parent.project_id.as_ref() == Some(&group.project_id)),
                }
            });
            if !valid_parent {
                placement.parent_folder_id = None;
            }
        }
        self.deduplicate_sibling_folder_names();
        self.deduplicate_sibling_group_names();

        let mut placed = self
            .placements
            .iter()
            .map(|placement| placement.item.clone())
            .collect::<BTreeSet<_>>();
        for project_id in ordered_project_ids {
            let item = SidebarOrganizationItem::Project(project_id.clone());
            if placed.insert(item.clone()) {
                self.placements.push(SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                });
            }
        }
        let mut new_groups = Vec::new();
        for group_id in self.groups.keys() {
            let item = SidebarOrganizationItem::Group(group_id.clone());
            if placed.insert(item.clone()) {
                new_groups.push(item);
            }
        }
        for item in new_groups {
            // A newly discovered group lands directly above its project's first
            // root session, so a group the user just created is visible where
            // the sessions it was built from already are.
            let project_id = match &item {
                SidebarOrganizationItem::Group(group_id) => self
                    .groups
                    .get(group_id)
                    .map(|group| group.project_id.clone()),
                _ => None,
            };
            let insertion_index = project_id
                .as_ref()
                .and_then(|project_id| {
                    self.placements.iter().position(|placement| {
                        placement.parent_folder_id.is_none()
                            && matches!(
                                &placement.item,
                                SidebarOrganizationItem::Session(existing_id)
                                    if session_projects.get(existing_id) == Some(project_id)
                            )
                    })
                })
                .unwrap_or(self.placements.len());
            self.placements.insert(
                insertion_index,
                SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                },
            );
        }
        let mut new_sessions = Vec::new();
        for (session_id, project_id) in ordered_session_projects {
            let item = SidebarOrganizationItem::Session(session_id.clone());
            if placed.insert(item.clone()) {
                new_sessions.push((item, project_id.clone()));
            }
        }
        for (item, project_id) in new_sessions {
            // A newly discovered session is the only item allowed to move when
            // the authoritative list changes. Insert it before the first root
            // session in its project instead of appending it to the placement
            // tail. The subsequent root-session alignment can then update the
            // session order without shifting existing sessions across folders.
            let insertion_index = self
                .placements
                .iter()
                .position(|placement| {
                    placement.parent_folder_id.is_none()
                        && matches!(
                            &placement.item,
                            SidebarOrganizationItem::Session(existing_id)
                                if session_projects.get(existing_id) == Some(&project_id)
                        )
                })
                .unwrap_or(self.placements.len());
            self.placements.insert(
                insertion_index,
                SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                },
            );
        }
        self.enforce_placement_limit();
        self.align_root_session_order(ordered_session_projects);
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
        self.collapsed_group_ids
            .retain(|id| self.groups.contains_key(id));
    }

    pub fn cleanup_references(
        &mut self,
        project_ids: &BTreeSet<String>,
        session_ids: &BTreeSet<String>,
    ) {
        self.folders.retain(|_, folder| {
            folder
                .project_id
                .as_ref()
                .is_none_or(|project_id| project_ids.contains(project_id))
        });
        self.groups
            .retain(|_, group| project_ids.contains(&group.project_id));
        for group in self.groups.values_mut() {
            let before = group.member_session_ids.len();
            group
                .member_session_ids
                .retain(|session_id| session_ids.contains(session_id));
            if group.member_session_ids.len() != before {
                group.normalize();
            }
        }
        self.groups.retain(|_, group| !group.is_empty());
        let valid_group_ids = self.groups.keys().cloned().collect::<BTreeSet<_>>();

        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        self.placements.retain(|placement| match &placement.item {
            SidebarOrganizationItem::Folder(id) => valid_folder_ids.contains(id),
            SidebarOrganizationItem::Project(id) => project_ids.contains(id),
            SidebarOrganizationItem::Session(id) => session_ids.contains(id),
            SidebarOrganizationItem::Group(id) => valid_group_ids.contains(id),
        });
        self.ensure_folder_placements();
        self.ensure_group_placements();
        self.enforce_placement_limit();
        self.repair_parents();
        self.deduplicate_sibling_folder_names();
        self.deduplicate_sibling_group_names();
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
        self.collapsed_group_ids
            .retain(|id| self.groups.contains_key(id));
    }

    pub fn create_folder(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: Option<String>,
        parent_folder_id: Option<String>,
    ) -> bool {
        self.create_folder_with_workspace(id, name, project_id, None, parent_folder_id)
    }

    /// Creates a folder with an optional workspace/worktree owner. The legacy
    /// `create_folder` entry point remains project-scoped for older clients.
    pub fn create_folder_with_workspace(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: Option<String>,
        workspace_id: Option<String>,
        parent_folder_id: Option<String>,
    ) -> bool {
        let id = id.into();
        let name = name.into();
        let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
            return false;
        };
        let Some(name) = bounded_text(&name, SIDEBAR_FOLDER_NAME_MAX_CHARS) else {
            return false;
        };
        if self.folders.len() >= SIDEBAR_FOLDER_LIMIT || self.folders.contains_key(&id) {
            return false;
        }
        let project_id = match project_id {
            Some(id) => {
                let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                    return false;
                };
                Some(id)
            }
            None => None,
        };
        let workspace_id = match workspace_id {
            Some(id) => {
                let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                    return false;
                };
                project_id.is_some().then_some(id)
            }
            None => None,
        };
        let parent_folder_id = match parent_folder_id {
            Some(parent_id) => {
                let Some(parent_id) = bounded_text(&parent_id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                    return false;
                };
                let Some(parent) = self.folders.get(&parent_id) else {
                    return false;
                };
                if parent.project_id != project_id
                    || parent.workspace_id != workspace_id
                    || self.folder_depth(&parent_id) >= SIDEBAR_FOLDER_DEPTH_LIMIT
                {
                    return false;
                }
                Some(parent_id)
            }
            None => None,
        };
        if !self.folder_name_is_available_at(
            None,
            &name,
            project_id.as_deref(),
            workspace_id.as_deref(),
            parent_folder_id.as_deref(),
        ) {
            return false;
        }
        self.folders.insert(
            id.clone(),
            SidebarFolderUiState {
                name,
                project_id,
                workspace_id,
                auto_archive_after_days: None,
            },
        );
        let insertion_index = parent_folder_id
            .as_ref()
            .and_then(|parent_id| {
                self.placements.iter().position(|placement| {
                    placement.item == SidebarOrganizationItem::Folder(parent_id.clone())
                })
            })
            .map_or(0, |index| index + 1);
        self.placements.insert(
            insertion_index,
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Folder(id),
                parent_folder_id,
            },
        );
        self.enforce_placement_limit();
        true
    }

    pub fn rename_folder(&mut self, folder_id: &str, name: &str) -> bool {
        let Some(name) = bounded_text(name, SIDEBAR_FOLDER_NAME_MAX_CHARS) else {
            return false;
        };
        let Some(folder) = self.folders.get(folder_id) else {
            return false;
        };
        if folder.name == name {
            return false;
        }
        let project_id = folder.project_id.clone();
        let parent_folder_id =
            self.parent_of(&SidebarOrganizationItem::Folder(folder_id.to_string()));
        if !self.folder_name_is_available_at(
            Some(folder_id),
            &name,
            project_id.as_deref(),
            folder.workspace_id.as_deref(),
            parent_folder_id.as_deref(),
        ) {
            return false;
        }
        let Some(folder) = self.folders.get_mut(folder_id) else {
            return false;
        };
        folder.name = name;
        true
    }

    pub fn folder_name_available(&self, folder_id: &str, name: &str) -> bool {
        let Some(name) = bounded_text(name, SIDEBAR_FOLDER_NAME_MAX_CHARS) else {
            return false;
        };
        let Some(folder) = self.folders.get(folder_id) else {
            return false;
        };
        let parent_folder_id =
            self.parent_of(&SidebarOrganizationItem::Folder(folder_id.to_string()));
        self.folder_name_is_available_at(
            Some(folder_id),
            &name,
            folder.project_id.as_deref(),
            folder.workspace_id.as_deref(),
            parent_folder_id.as_deref(),
        )
    }

    pub fn next_available_folder_name(
        &self,
        preferred_name: &str,
        project_id: Option<&str>,
        parent_folder_id: Option<&str>,
    ) -> Option<String> {
        self.next_available_folder_name_for_workspace(
            preferred_name,
            project_id,
            None,
            parent_folder_id,
        )
    }

    pub fn next_available_folder_name_for_workspace(
        &self,
        preferred_name: &str,
        project_id: Option<&str>,
        workspace_id: Option<&str>,
        parent_folder_id: Option<&str>,
    ) -> Option<String> {
        let preferred_name = bounded_text(preferred_name, SIDEBAR_FOLDER_NAME_MAX_CHARS)?;
        Some(next_unique_folder_name(&preferred_name, |candidate| {
            self.folder_name_is_available_at(
                None,
                candidate,
                project_id,
                workspace_id,
                parent_folder_id,
            )
        }))
    }

    pub fn delete_folder(&mut self, folder_id: &str) -> bool {
        if !self.folders.contains_key(folder_id) {
            return false;
        }

        // Folder deletion removes the complete local classification subtree. The
        // referenced Project/Session records remain authoritative and become
        // unplaced, so the next root projection can show them again safely.
        let mut deleted_folder_ids = BTreeSet::from([folder_id.to_string()]);
        loop {
            let descendants = self
                .placements
                .iter()
                .filter_map(|placement| {
                    let SidebarOrganizationItem::Folder(candidate_id) = &placement.item else {
                        return None;
                    };
                    placement
                        .parent_folder_id
                        .as_ref()
                        .filter(|parent_id| deleted_folder_ids.contains(*parent_id))
                        .and_then(|_| {
                            (!deleted_folder_ids.contains(candidate_id)).then_some(candidate_id)
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
            if descendants.is_empty() {
                break;
            }
            deleted_folder_ids.extend(descendants);
        }

        self.folders
            .retain(|id, _| !deleted_folder_ids.contains(id));
        self.collapsed_folder_ids
            .retain(|id| !deleted_folder_ids.contains(id));
        self.placements.retain(|placement| {
            !matches!(
                &placement.item,
                SidebarOrganizationItem::Folder(id) if deleted_folder_ids.contains(id)
            ) && !placement
                .parent_folder_id
                .as_ref()
                .is_some_and(|parent_id| deleted_folder_ids.contains(parent_id))
        });
        true
    }

    pub fn folder(&self, folder_id: &str) -> Option<&SidebarFolderUiState> {
        self.folders.get(folder_id)
    }

    // -- Session groups ---------------------------------------------------

    pub fn group(&self, group_id: &str) -> Option<&SessionGroupUiState> {
        self.groups.get(group_id)
    }

    pub fn group_mut(&mut self, group_id: &str) -> Option<&mut SessionGroupUiState> {
        self.groups.get_mut(group_id)
    }

    /// The group that currently holds `session_id`. A session belongs to at
    /// most one group.
    pub fn group_of_session(&self, session_id: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|(_, group)| group.contains(session_id))
            .map(|(id, _)| id.as_str())
    }

    /// Every group of one project, in sidebar order. Compact clients list a
    /// project's sessions at project level, so they need the project's groups
    /// regardless of which Worktree each one is anchored to.
    pub fn groups_for_project(&self, project_id: &str) -> Vec<(String, &SessionGroupUiState)> {
        let order = self
            .placements
            .iter()
            .filter_map(|placement| match &placement.item {
                SidebarOrganizationItem::Group(id) => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let entries = self
            .groups
            .iter()
            .filter(|(_, group)| group.project_id == project_id);
        crate::ordered_groups(entries, &order)
    }

    /// Groups anchored to one Worktree, in sidebar order.
    pub fn groups_for_workspace(
        &self,
        project_id: &str,
        workspace_id: &str,
    ) -> Vec<(String, &SessionGroupUiState)> {
        let order = self
            .placements
            .iter()
            .filter_map(|placement| match &placement.item {
                SidebarOrganizationItem::Group(id) => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let entries = self.groups.iter().filter(|(_, group)| {
            group.project_id == project_id && group.workspace_id == workspace_id
        });
        crate::ordered_groups(entries, &order)
    }

    /// The group scope, for placement and drop validation.
    pub fn group_scope(&self, group_id: &str) -> Option<SidebarOrganizationScope> {
        self.groups
            .get(group_id)
            .map(|group| SidebarOrganizationScope::Project(group.project_id.clone()))
    }

    /// Creates a group and places it. Members whose workspace does not match
    /// are dropped: a group is always single-Worktree.
    #[allow(clippy::too_many_arguments)]
    pub fn create_group(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: &str,
        workspace_id: &str,
        member_session_ids: &[String],
        session_workspaces: &BTreeMap<String, String>,
        parent_folder_id: Option<String>,
    ) -> bool {
        let id = id.into();
        let name = name.into();
        let Some(id) = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
            return false;
        };
        let Some(project_id) = bounded_text(project_id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
            return false;
        };
        let Some(workspace_id) = bounded_text(workspace_id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
            return false;
        };
        if self.groups.len() >= SIDEBAR_GROUP_LIMIT || self.groups.contains_key(&id) {
            return false;
        }
        let parent_folder_id = match parent_folder_id {
            Some(parent_id) => {
                let Some(parent_id) = bounded_text(&parent_id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                    return false;
                };
                let Some(parent) = self.folders.get(&parent_id) else {
                    return false;
                };
                if parent.project_id.as_deref() != Some(project_id.as_str()) {
                    return false;
                }
                Some(parent_id)
            }
            None => None,
        };

        let members = member_session_ids
            .iter()
            .filter(|session_id| {
                session_workspaces
                    .get(session_id.as_str())
                    .is_some_and(|workspace| workspace == &workspace_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if members.is_empty() {
            return false;
        }

        let mut group = SessionGroupUiState::new(name, project_id, workspace_id, members);
        group.normalize();
        self.groups.insert(id.clone(), group);

        let insertion_index = parent_folder_id
            .as_ref()
            .and_then(|parent_id| {
                self.placements.iter().position(|placement| {
                    placement.item == SidebarOrganizationItem::Folder(parent_id.clone())
                })
            })
            .map_or(0, |index| index + 1);
        self.placements.insert(
            insertion_index,
            SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Group(id),
                parent_folder_id,
            },
        );
        self.enforce_placement_limit();
        true
    }

    /// Adds sessions to a group. Sessions from another Worktree are rejected.
    pub fn add_sessions_to_group(
        &mut self,
        group_id: &str,
        session_ids: &[String],
        session_workspaces: &BTreeMap<String, String>,
    ) -> bool {
        let Some(workspace_id) = self
            .groups
            .get(group_id)
            .map(|group| group.workspace_id.clone())
        else {
            return false;
        };
        let accepted = session_ids
            .iter()
            .filter(|session_id| {
                // A session already in this group is fine to repeat; a session
                // in another group is moved, because a session belongs to one
                // group. A session from another Worktree is refused.
                session_workspaces
                    .get(session_id.as_str())
                    .is_some_and(|workspace| workspace == &workspace_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if accepted.is_empty() {
            return false;
        }
        for session_id in &accepted {
            if let Some(other_group_id) = self.group_of_session(session_id)
                && other_group_id != group_id
            {
                let other_group_id = other_group_id.to_string();
                self.remove_sessions_from_group(&other_group_id, std::slice::from_ref(session_id));
            }
        }
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        group.add_members(&accepted)
    }

    /// Removes sessions from a group. A group that loses every member is
    /// dissolved, because a group with no sessions has no workspace.
    pub fn remove_sessions_from_group(&mut self, group_id: &str, session_ids: &[String]) -> bool {
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        if !group.remove_members(session_ids) {
            return false;
        }
        if group.is_empty() {
            self.delete_group(group_id);
        }
        true
    }

    pub fn set_group_members(
        &mut self,
        group_id: &str,
        session_ids: Vec<String>,
        session_workspaces: &BTreeMap<String, String>,
    ) -> bool {
        let Some(workspace_id) = self
            .groups
            .get(group_id)
            .map(|group| group.workspace_id.clone())
        else {
            return false;
        };
        let accepted = session_ids
            .into_iter()
            .filter(|session_id| {
                session_workspaces
                    .get(session_id.as_str())
                    .is_some_and(|workspace| workspace == &workspace_id)
            })
            .collect::<Vec<_>>();
        if accepted.is_empty() {
            self.delete_group(group_id);
            return true;
        }
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        group.set_members(accepted)
    }

    pub fn rename_group(&mut self, group_id: &str, name: &str) -> bool {
        let Some(name) = bounded_text(name, SESSION_GROUP_NAME_MAX_CHARS) else {
            return false;
        };
        let Some(group) = self.groups.get(group_id) else {
            return false;
        };
        if group.name == name {
            return false;
        }
        let project_id = group.project_id.clone();
        let workspace_id = group.workspace_id.clone();
        let parent_folder_id =
            self.parent_of(&SidebarOrganizationItem::Group(group_id.to_string()));
        if !self.group_name_is_available_at(
            Some(group_id),
            &name,
            &project_id,
            &workspace_id,
            parent_folder_id.as_deref(),
        ) {
            return false;
        }
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        group.name = name;
        true
    }

    /// Removes a group. The member sessions stay authoritative and simply
    /// become ungrouped again.
    pub fn delete_group(&mut self, group_id: &str) -> bool {
        if self.groups.remove(group_id).is_none() {
            return false;
        }
        self.collapsed_group_ids.remove(group_id);
        self.placements.retain(|placement| {
            placement.item != SidebarOrganizationItem::Group(group_id.to_string())
        });
        true
    }

    pub fn set_group_pinned(&mut self, group_id: &str, pinned: bool) -> bool {
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        if group.pinned == pinned {
            return false;
        }
        group.pinned = pinned;
        true
    }

    pub fn set_group_auto_continue(&mut self, group_id: &str, enabled: Option<bool>) -> bool {
        let Some(group) = self.groups.get_mut(group_id) else {
            return false;
        };
        if group.auto_continue == enabled {
            return false;
        }
        group.auto_continue = enabled;
        true
    }

    pub fn toggle_group_collapsed(&mut self, group_id: &str) -> Option<bool> {
        if !self.groups.contains_key(group_id) {
            return None;
        }
        if self.collapsed_group_ids.remove(group_id) {
            Some(false)
        } else {
            self.collapsed_group_ids.insert(group_id.to_string());
            Some(true)
        }
    }

    pub fn group_is_collapsed(&self, group_id: &str) -> bool {
        self.collapsed_group_ids.contains(group_id)
    }

    /// Every session that a group already shows. These render under their group
    /// row, so a sibling list must not repeat them.
    pub fn grouped_session_ids(&self) -> BTreeSet<String> {
        self.groups
            .values()
            .flat_map(|group| group.member_session_ids.iter().cloned())
            .collect()
    }

    pub fn group_is_pinned(&self, group_id: &str) -> bool {
        self.groups.get(group_id).is_some_and(|group| group.pinned)
    }

    pub fn group_name_is_available_at(
        &self,
        excluded_group_id: Option<&str>,
        name: &str,
        project_id: &str,
        workspace_id: &str,
        parent_folder_id: Option<&str>,
    ) -> bool {
        let comparable_name = comparable_folder_name(name);
        !self.groups.iter().any(|(candidate_id, group)| {
            excluded_group_id != Some(candidate_id.as_str())
                && group.project_id == project_id
                && group.workspace_id == workspace_id
                && self
                    .parent_of(&SidebarOrganizationItem::Group(candidate_id.clone()))
                    .as_deref()
                    == parent_folder_id
                && comparable_folder_name(&group.name) == comparable_name
        })
    }

    /// The next free default name for a new group in one Worktree.
    pub fn next_available_group_name(
        &self,
        project_id: &str,
        workspace_id: &str,
        stem: &str,
    ) -> String {
        let existing = self
            .groups
            .values()
            .filter(|group| group.project_id == project_id && group.workspace_id == workspace_id)
            .map(|group| group.name.as_str());
        crate::next_available_group_name(existing, stem)
    }

    fn ensure_group_placements(&mut self) {
        let mut placed = self
            .placements
            .iter()
            .map(|placement| placement.item.clone())
            .collect::<BTreeSet<_>>();
        for group_id in self.groups.keys() {
            let item = SidebarOrganizationItem::Group(group_id.clone());
            if placed.insert(item.clone()) {
                self.placements.push(SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                });
            }
        }
    }

    fn deduplicate_sibling_group_names(&mut self) {
        let mut seen_ids = BTreeSet::new();
        let mut group_ids = self
            .placements
            .iter()
            .filter_map(|placement| match &placement.item {
                SidebarOrganizationItem::Group(id) if seen_ids.insert(id.clone()) => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        group_ids.extend(
            self.groups
                .keys()
                .filter(|id| seen_ids.insert((*id).clone()))
                .cloned(),
        );

        let mut used_names = BTreeMap::<(String, String, Option<String>), BTreeSet<String>>::new();
        for group_id in group_ids {
            let Some(group) = self.groups.get(&group_id) else {
                continue;
            };
            let name = group.name.clone();
            let scope = (group.project_id.clone(), group.workspace_id.clone());
            let parent = self.parent_of(&SidebarOrganizationItem::Group(group_id.clone()));
            let names = used_names.entry((scope.0, scope.1, parent)).or_default();
            let unique_name = next_unique_folder_name(&name, |candidate| {
                !names.contains(&comparable_folder_name(candidate))
            });
            names.insert(comparable_folder_name(&unique_name));
            if unique_name != name
                && let Some(group) = self.groups.get_mut(&group_id)
            {
                group.name = unique_name;
            }
        }
    }

    pub fn set_folder_auto_archive_after_days(
        &mut self,
        folder_id: &str,
        days: Option<u8>,
    ) -> bool {
        if days.is_some_and(|days| !(1..=SIDEBAR_AUTO_ARCHIVE_MAX_DAYS).contains(&days)) {
            return false;
        }
        let Some(project_id) = self
            .folders
            .get(folder_id)
            .and_then(|folder| folder.project_id.clone())
        else {
            return false;
        };

        let mut changed = false;
        if days.is_some() {
            for (candidate_id, folder) in &mut self.folders {
                if candidate_id != folder_id
                    && folder.project_id.as_deref() == Some(project_id.as_str())
                    && folder.auto_archive_after_days.take().is_some()
                {
                    changed = true;
                }
            }
        }
        let Some(folder) = self.folders.get_mut(folder_id) else {
            return changed;
        };
        if folder.auto_archive_after_days != days {
            folder.auto_archive_after_days = days;
            changed = true;
        }
        changed
    }

    pub fn apply_scheduled_archives(
        &mut self,
        sessions: &[AgentSession],
        pinned_session_ids: &BTreeSet<String>,
        now_ms: i64,
    ) -> bool {
        let targets = self
            .placements
            .iter()
            .filter_map(|placement| {
                let SidebarOrganizationItem::Folder(folder_id) = &placement.item else {
                    return None;
                };
                let folder = self.folders.get(folder_id)?;
                Some((
                    folder_id.clone(),
                    folder.project_id.clone()?,
                    folder.auto_archive_after_days?,
                ))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return false;
        }

        let session_projects = sessions
            .iter()
            .map(|session| {
                (
                    session.id.as_str().to_string(),
                    session.project_id.as_str().to_string(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let activity_by_session = sessions
            .iter()
            .map(|session| {
                (
                    session.id.as_str().to_string(),
                    session.last_message_at_ms.max(session.created_at_ms),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let original = self.placements.clone();

        for (folder_id, project_id, days) in targets {
            let cutoff_ms = now_ms.saturating_sub(i64::from(days) * MILLIS_PER_DAY);
            let mut moving = sessions
                .iter()
                .filter(|session| {
                    session.project_id.as_str() == project_id
                        && session.archived_at_ms.is_none()
                        && session.deleted_at_ms.is_none()
                        && !pinned_session_ids.contains(session.id.as_str())
                        && self
                            .parent_of(&SidebarOrganizationItem::Session(
                                session.id.as_str().to_string(),
                            ))
                            .is_none()
                        && session.last_message_at_ms.max(session.created_at_ms) <= cutoff_ms
                })
                .map(|session| SidebarOrganizationItem::Session(session.id.as_str().to_string()))
                .collect::<Vec<_>>();
            moving.sort_by_key(|item| {
                (
                    std::cmp::Reverse(
                        activity_by_session
                            .get(item.id())
                            .copied()
                            .unwrap_or_default(),
                    ),
                    item.id().to_string(),
                )
            });
            if !moving.is_empty() {
                self.move_many_into(&moving, &folder_id, &session_projects);
            }
            self.sort_folder_sessions_by_activity(&folder_id, &activity_by_session);
        }

        self.placements != original
    }

    pub fn folder_contains_items(&self, folder_id: &str) -> bool {
        self.folders.contains_key(folder_id)
            && self.placements.iter().any(|placement| {
                matches!(
                    placement.item,
                    SidebarOrganizationItem::Project(_)
                        | SidebarOrganizationItem::Session(_)
                        | SidebarOrganizationItem::Group(_)
                ) && placement
                    .parent_folder_id
                    .as_deref()
                    .is_some_and(|parent_id| {
                        parent_id == folder_id || self.folder_is_descendant_of(parent_id, folder_id)
                    })
            })
    }

    pub fn folder_scope(&self, folder_id: &str) -> Option<SidebarOrganizationScope> {
        self.folders.get(folder_id).map(|folder| {
            folder.project_id.clone().map_or(
                SidebarOrganizationScope::Root,
                SidebarOrganizationScope::Project,
            )
        })
    }

    pub fn item_scope(
        &self,
        item: &SidebarOrganizationItem,
        session_projects: &BTreeMap<String, String>,
    ) -> Option<SidebarOrganizationScope> {
        match item {
            SidebarOrganizationItem::Folder(id) => self.folder_scope(id),
            SidebarOrganizationItem::Project(_) => Some(SidebarOrganizationScope::Root),
            SidebarOrganizationItem::Session(id) => session_projects
                .get(id)
                .cloned()
                .map(SidebarOrganizationScope::Project),
            SidebarOrganizationItem::Group(id) => self.group_scope(id),
        }
    }

    pub fn parent_of(&self, item: &SidebarOrganizationItem) -> Option<String> {
        self.placements
            .iter()
            .find(|placement| &placement.item == item)
            .and_then(|placement| placement.parent_folder_id.clone())
    }

    pub fn ordered_children(
        &self,
        parent_folder_id: Option<&str>,
        available_items: &[SidebarOrganizationItem],
    ) -> Vec<SidebarOrganizationItem> {
        let available = available_items.iter().cloned().collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut children = self
            .placements
            .iter()
            .filter(|placement| placement.parent_folder_id.as_deref() == parent_folder_id)
            .filter_map(|placement| {
                (available.contains(&placement.item) && seen.insert(placement.item.clone()))
                    .then_some(placement.item.clone())
            })
            .collect::<Vec<_>>();
        if parent_folder_id.is_none() {
            // Newly discovered items without a placement fall back to the scope
            // root; placed items must appear only below their recorded parent.
            // Only this branch needs to know which items are placed at all, so
            // the other callers — every folder level of every render — do not
            // pay for collecting the placement set.
            let placed = self
                .placements
                .iter()
                .map(|placement| placement.item.clone())
                .collect::<BTreeSet<_>>();
            children.extend(
                available_items
                    .iter()
                    .filter(|item| !placed.contains(*item) && seen.insert((*item).clone()))
                    .cloned(),
            );
        }
        children
    }

    pub fn can_move_relative(
        &self,
        moving: &SidebarOrganizationItem,
        target: &SidebarOrganizationItem,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        let moving_scope = self.item_scope(moving, session_projects);
        if moving == target
            || moving_scope.is_none()
            || moving_scope != self.item_scope(target, session_projects)
        {
            return false;
        }
        let target_parent = self.parent_of(target);
        !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if target_parent.as_deref() == Some(folder_id) || target_parent.as_deref().is_some_and(|parent_id| self.folder_is_descendant_of(parent_id, folder_id)))
            && !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if !self.folder_move_fits(folder_id, target_parent.as_deref()))
            && !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if !self.folder_name_available_at_parent(folder_id, target_parent.as_deref()))
    }

    pub fn can_move_into(
        &self,
        moving: &SidebarOrganizationItem,
        target_folder_id: &str,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        let target = SidebarOrganizationItem::Folder(target_folder_id.to_string());
        let moving_scope = self.item_scope(moving, session_projects);
        if moving_scope.is_none() || moving_scope != self.item_scope(&target, session_projects) {
            return false;
        }
        !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if folder_id == target_folder_id || self.folder_is_descendant_of(target_folder_id, folder_id))
            && !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if !self.folder_move_fits(folder_id, Some(target_folder_id)))
            && !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if !self.folder_name_available_at_parent(folder_id, Some(target_folder_id)))
    }

    pub fn move_relative(
        &mut self,
        moving: &SidebarOrganizationItem,
        target: &SidebarOrganizationItem,
        after: bool,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.move_many_relative(
            std::slice::from_ref(moving),
            target,
            after,
            session_projects,
        )
    }

    pub fn can_move_many_relative(
        &self,
        moving: &[SidebarOrganizationItem],
        target: &SidebarOrganizationItem,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.valid_moving_items(moving)
            && !moving.contains(target)
            && moving
                .iter()
                .all(|item| self.can_move_relative(item, target, session_projects))
    }

    pub fn move_many_relative(
        &mut self,
        moving: &[SidebarOrganizationItem],
        target: &SidebarOrganizationItem,
        after: bool,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        if !self.can_move_many_relative(moving, target, session_projects) {
            return false;
        }
        let Some(target_parent) = self
            .placements
            .iter()
            .find(|placement| &placement.item == target)
            .map(|placement| placement.parent_folder_id.clone())
        else {
            return false;
        };
        let moving_placements = moving
            .iter()
            .map(|item| {
                self.placements
                    .iter()
                    .find(|placement| &placement.item == item)
                    .cloned()
            })
            .collect::<Option<Vec<_>>>();
        let Some(mut moving_placements) = moving_placements else {
            return false;
        };
        let original = self.placements.clone();
        let moving_set = moving.iter().cloned().collect::<BTreeSet<_>>();
        self.placements
            .retain(|placement| !moving_set.contains(&placement.item));
        let Some(target_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == target)
        else {
            self.placements = original;
            return false;
        };
        for placement in &mut moving_placements {
            placement.parent_folder_id.clone_from(&target_parent);
        }
        let insertion_index = target_index + usize::from(after);
        self.placements
            .splice(insertion_index..insertion_index, moving_placements);
        self.placements != original
    }

    pub fn move_into(
        &mut self,
        moving: &SidebarOrganizationItem,
        target_folder_id: &str,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.move_many_into(
            std::slice::from_ref(moving),
            target_folder_id,
            session_projects,
        )
    }

    pub fn can_move_many_into(
        &self,
        moving: &[SidebarOrganizationItem],
        target_folder_id: &str,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.valid_moving_items(moving)
            && moving
                .iter()
                .all(|item| self.can_move_into(item, target_folder_id, session_projects))
    }

    pub fn move_many_into(
        &mut self,
        moving: &[SidebarOrganizationItem],
        target_folder_id: &str,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        if !self.can_move_many_into(moving, target_folder_id, session_projects) {
            return false;
        }
        let moving_placements = moving
            .iter()
            .map(|item| {
                self.placements
                    .iter()
                    .find(|placement| &placement.item == item)
                    .cloned()
            })
            .collect::<Option<Vec<_>>>();
        let Some(mut moving_placements) = moving_placements else {
            return false;
        };
        let original = self.placements.clone();
        let moving_set = moving.iter().cloned().collect::<BTreeSet<_>>();
        self.placements
            .retain(|placement| !moving_set.contains(&placement.item));
        for placement in &mut moving_placements {
            placement.parent_folder_id = Some(target_folder_id.to_string());
        }
        let insertion_index = self
            .placements
            .iter()
            .rposition(|placement| placement.parent_folder_id.as_deref() == Some(target_folder_id))
            .map_or_else(
                || {
                    self.placements
                        .iter()
                        .position(|placement| {
                            placement.item
                                == SidebarOrganizationItem::Folder(target_folder_id.to_string())
                        })
                        .map_or(self.placements.len(), |index| index + 1)
                },
                |index| index + 1,
            );
        self.placements
            .splice(insertion_index..insertion_index, moving_placements);
        self.placements != original
    }

    pub fn move_to_scope_root_end(
        &mut self,
        moving: &SidebarOrganizationItem,
        scope: &SidebarOrganizationScope,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.move_many_to_scope_root_end(std::slice::from_ref(moving), scope, session_projects)
    }

    pub fn can_move_many_to_scope_root(
        &self,
        moving: &[SidebarOrganizationItem],
        scope: &SidebarOrganizationScope,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.valid_moving_items(moving)
            && moving
                .iter()
                .all(|item| self.can_move_to_scope_root(item, scope, session_projects))
    }

    pub fn move_many_to_scope_root_end(
        &mut self,
        moving: &[SidebarOrganizationItem],
        scope: &SidebarOrganizationScope,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        if !self.can_move_many_to_scope_root(moving, scope, session_projects) {
            return false;
        }
        let moving_placements = moving
            .iter()
            .map(|item| {
                self.placements
                    .iter()
                    .find(|placement| &placement.item == item)
                    .cloned()
            })
            .collect::<Option<Vec<_>>>();
        let Some(mut moving_placements) = moving_placements else {
            return false;
        };
        let original = self.placements.clone();
        let moving_set = moving.iter().cloned().collect::<BTreeSet<_>>();
        self.placements
            .retain(|placement| !moving_set.contains(&placement.item));
        for placement in &mut moving_placements {
            placement.parent_folder_id = None;
        }
        let insertion_index = self
            .placements
            .iter()
            .enumerate()
            .filter(|(_, placement)| placement.parent_folder_id.is_none())
            .filter(|(_, placement)| {
                self.item_scope(&placement.item, session_projects).as_ref() == Some(scope)
            })
            .map(|(index, _)| index + 1)
            .next_back()
            .unwrap_or(self.placements.len());
        self.placements
            .splice(insertion_index..insertion_index, moving_placements);
        self.placements != original
    }

    pub fn can_move_to_scope_root(
        &self,
        moving: &SidebarOrganizationItem,
        scope: &SidebarOrganizationScope,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        self.item_scope(moving, session_projects).as_ref() == Some(scope)
            && !matches!(moving, SidebarOrganizationItem::Folder(folder_id) if !self.folder_name_available_at_parent(folder_id, None))
    }

    fn valid_moving_items(&self, moving: &[SidebarOrganizationItem]) -> bool {
        !moving.is_empty()
            && moving.iter().cloned().collect::<BTreeSet<_>>().len() == moving.len()
            && (moving.len() == 1
                || moving
                    .iter()
                    .all(|item| !matches!(item, SidebarOrganizationItem::Folder(_))))
    }

    fn sort_folder_sessions_by_activity(
        &mut self,
        folder_id: &str,
        activity_by_session: &BTreeMap<String, i64>,
    ) {
        let mut sessions = self
            .placements
            .iter()
            .filter(|placement| placement.parent_folder_id.as_deref() == Some(folder_id))
            .filter(|placement| {
                matches!(
                    &placement.item,
                    SidebarOrganizationItem::Session(session_id)
                        if activity_by_session.contains_key(session_id)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by_key(|placement| {
            (
                std::cmp::Reverse(
                    activity_by_session
                        .get(placement.item.id())
                        .copied()
                        .unwrap_or_default(),
                ),
                placement.item.id().to_string(),
            )
        });
        let mut sessions = sessions.into_iter();
        for placement in &mut self.placements {
            if placement.parent_folder_id.as_deref() == Some(folder_id)
                && matches!(
                    &placement.item,
                    SidebarOrganizationItem::Session(session_id)
                        if activity_by_session.contains_key(session_id)
                )
                && let Some(next) = sessions.next()
            {
                *placement = next;
            }
        }
    }

    pub fn toggle_collapsed(&mut self, folder_id: &str) -> Option<bool> {
        if !self.folders.contains_key(folder_id) {
            return None;
        }
        if self.collapsed_folder_ids.remove(folder_id) {
            Some(false)
        } else {
            self.collapsed_folder_ids.insert(folder_id.to_string());
            Some(true)
        }
    }

    pub fn folder_is_descendant_of(&self, folder_id: &str, ancestor_id: &str) -> bool {
        let mut current = Some(folder_id.to_string());
        let mut seen = BTreeSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                return false;
            }
            let Some(parent) = self.parent_of(&SidebarOrganizationItem::Folder(id)) else {
                return false;
            };
            if parent == ancestor_id {
                return true;
            }
            current = Some(parent);
        }
        false
    }

    fn align_root_session_order(&mut self, ordered_session_projects: &[(String, String)]) {
        let desired_positions = ordered_session_projects
            .iter()
            .enumerate()
            .map(|(index, (session_id, _))| (session_id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let session_projects = ordered_session_projects
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let mut slots_by_project = BTreeMap::<String, Vec<usize>>::new();
        for (index, placement) in self.placements.iter().enumerate() {
            let SidebarOrganizationItem::Session(session_id) = &placement.item else {
                continue;
            };
            if placement.parent_folder_id.is_some() {
                continue;
            }
            if let Some(project_id) = session_projects.get(session_id) {
                slots_by_project
                    .entry(project_id.clone())
                    .or_default()
                    .push(index);
            }
        }

        for slots in slots_by_project.into_values() {
            let mut sessions = slots
                .iter()
                .map(|index| self.placements[*index].item.clone())
                .collect::<Vec<_>>();
            sessions.sort_by_key(|item| {
                desired_positions
                    .get(item.id())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            for (index, item) in slots.into_iter().zip(sessions) {
                self.placements[index].item = item;
            }
        }
    }

    fn folder_depth(&self, folder_id: &str) -> usize {
        let mut depth = 0;
        let mut current = Some(folder_id.to_string());
        let mut seen = BTreeSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                return SIDEBAR_FOLDER_DEPTH_LIMIT;
            }
            current = self.parent_of(&SidebarOrganizationItem::Folder(id));
            depth += 1;
            if depth >= SIDEBAR_FOLDER_DEPTH_LIMIT {
                break;
            }
        }
        depth
    }

    fn folder_move_fits(&self, folder_id: &str, parent_folder_id: Option<&str>) -> bool {
        let parent_depth = match parent_folder_id {
            Some(parent_id) if self.folders.contains_key(parent_id) => self.folder_depth(parent_id),
            Some(_) => return false,
            None => 0,
        };
        parent_depth.saturating_add(self.folder_subtree_height(folder_id))
            <= SIDEBAR_FOLDER_DEPTH_LIMIT
    }

    fn folder_subtree_height(&self, folder_id: &str) -> usize {
        self.folders
            .keys()
            .filter_map(|candidate_id| self.folder_distance_from(candidate_id, folder_id))
            .max()
            .unwrap_or(1)
    }

    fn folder_name_available_at_parent(
        &self,
        folder_id: &str,
        parent_folder_id: Option<&str>,
    ) -> bool {
        let Some(folder) = self.folders.get(folder_id) else {
            return false;
        };
        self.folder_name_is_available_at(
            Some(folder_id),
            &folder.name,
            folder.project_id.as_deref(),
            folder.workspace_id.as_deref(),
            parent_folder_id,
        )
    }

    fn folder_name_is_available_at(
        &self,
        excluded_folder_id: Option<&str>,
        name: &str,
        project_id: Option<&str>,
        workspace_id: Option<&str>,
        parent_folder_id: Option<&str>,
    ) -> bool {
        let comparable_name = comparable_folder_name(name);
        !self.folders.iter().any(|(candidate_id, folder)| {
            excluded_folder_id != Some(candidate_id.as_str())
                && folder.project_id.as_deref() == project_id
                && folder.workspace_id.as_deref() == workspace_id
                && self
                    .parent_of(&SidebarOrganizationItem::Folder(candidate_id.clone()))
                    .as_deref()
                    == parent_folder_id
                && comparable_folder_name(&folder.name) == comparable_name
        })
    }

    fn folder_distance_from(&self, folder_id: &str, ancestor_id: &str) -> Option<usize> {
        let mut current = folder_id.to_string();
        let mut distance = 1_usize;
        let mut seen = BTreeSet::new();
        loop {
            if current == ancestor_id {
                return Some(distance);
            }
            if !seen.insert(current.clone()) {
                return None;
            }
            current = self.parent_of(&SidebarOrganizationItem::Folder(current))?;
            distance = distance.saturating_add(1);
        }
    }

    fn ensure_folder_placements(&mut self) {
        let mut placed = self
            .placements
            .iter()
            .map(|placement| placement.item.clone())
            .collect::<BTreeSet<_>>();
        for folder_id in self.folders.keys() {
            let item = SidebarOrganizationItem::Folder(folder_id.clone());
            if placed.insert(item.clone()) {
                self.placements.push(SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                });
            }
        }
    }

    fn enforce_placement_limit(&mut self) {
        let folder_count = self
            .placements
            .iter()
            .filter(|placement| matches!(placement.item, SidebarOrganizationItem::Folder(_)))
            .count();
        let mut remaining_non_folders =
            SIDEBAR_ORGANIZATION_ITEM_LIMIT.saturating_sub(folder_count);
        self.placements.retain(|placement| {
            if matches!(placement.item, SidebarOrganizationItem::Folder(_)) {
                return true;
            }
            let retain = remaining_non_folders > 0;
            remaining_non_folders = remaining_non_folders.saturating_sub(1);
            retain
        });
    }

    fn repair_parents(&mut self) {
        let folder_scopes = self
            .folders
            .iter()
            .map(|(id, folder)| {
                (
                    id.clone(),
                    (folder.project_id.clone(), folder.workspace_id.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for placement in &mut self.placements {
            let Some(parent_id) = placement.parent_folder_id.as_ref() else {
                continue;
            };
            let valid =
                folder_scopes
                    .get(parent_id)
                    .is_some_and(|parent_scope| match &placement.item {
                        SidebarOrganizationItem::Folder(folder_id) => {
                            folder_id != parent_id
                                && folder_scopes.get(folder_id) == Some(parent_scope)
                        }
                        SidebarOrganizationItem::Project(_) => {
                            parent_scope.0.is_none() && parent_scope.1.is_none()
                        }
                        SidebarOrganizationItem::Session(_) => parent_scope.0.is_some(),
                        SidebarOrganizationItem::Group(_) => parent_scope.0.is_some(),
                    });
            if !valid {
                placement.parent_folder_id = None;
            }
        }

        let folder_ids = self.folders.keys().cloned().collect::<Vec<_>>();
        for folder_id in folder_ids {
            let mut current = Some(folder_id.clone());
            let mut seen = BTreeSet::new();
            let mut invalid = false;
            for _ in 0..SIDEBAR_FOLDER_DEPTH_LIMIT {
                let Some(id) = current.as_ref() else {
                    break;
                };
                if !seen.insert(id.clone()) {
                    invalid = true;
                    break;
                }
                current = self.parent_of(&SidebarOrganizationItem::Folder(id.clone()));
            }
            if current.is_some() {
                invalid = true;
            }
            if invalid
                && let Some(placement) = self.placements.iter_mut().find(|placement| {
                    placement.item == SidebarOrganizationItem::Folder(folder_id.clone())
                })
            {
                placement.parent_folder_id = None;
            }
        }
    }

    fn deduplicate_sibling_folder_names(&mut self) {
        let mut seen_ids = BTreeSet::new();
        let mut folder_ids = self
            .placements
            .iter()
            .filter_map(|placement| match &placement.item {
                SidebarOrganizationItem::Folder(id) if seen_ids.insert(id.clone()) => {
                    Some(id.clone())
                }
                SidebarOrganizationItem::Folder(_)
                | SidebarOrganizationItem::Project(_)
                | SidebarOrganizationItem::Session(_)
                | SidebarOrganizationItem::Group(_) => None,
            })
            .collect::<Vec<_>>();
        folder_ids.extend(
            self.folders
                .keys()
                .filter(|id| seen_ids.insert((*id).clone()))
                .cloned(),
        );

        let mut used_names =
            BTreeMap::<(Option<String>, Option<String>, Option<String>), BTreeSet<String>>::new();
        for folder_id in folder_ids {
            let Some(folder) = self.folders.get(&folder_id) else {
                continue;
            };
            let name = folder.name.clone();
            let scope = folder.project_id.clone();
            let workspace = folder.workspace_id.clone();
            let parent = self.parent_of(&SidebarOrganizationItem::Folder(folder_id.clone()));
            let names = used_names.entry((scope, workspace, parent)).or_default();
            let unique_name = next_unique_folder_name(&name, |candidate| {
                !names.contains(&comparable_folder_name(candidate))
            });
            names.insert(comparable_folder_name(&unique_name));
            if unique_name != name
                && let Some(folder) = self.folders.get_mut(&folder_id)
            {
                folder.name = unique_name;
            }
        }
    }

    fn enforce_one_auto_archive_folder_per_project(&mut self) {
        let mut claimed_projects = BTreeSet::new();
        for placement in &self.placements {
            let SidebarOrganizationItem::Folder(folder_id) = &placement.item else {
                continue;
            };
            let Some(folder) = self.folders.get_mut(folder_id) else {
                continue;
            };
            let Some(project_id) = folder.project_id.as_ref() else {
                folder.auto_archive_after_days = None;
                continue;
            };
            if folder.auto_archive_after_days.is_some()
                && !claimed_projects.insert(project_id.clone())
            {
                folder.auto_archive_after_days = None;
            }
        }
    }
}

fn normalize_placement(
    placement: SidebarOrganizationPlacement,
    valid_folder_ids: &BTreeSet<String>,
    valid_group_ids: &BTreeSet<String>,
) -> Option<SidebarOrganizationPlacement> {
    let item = match placement.item {
        SidebarOrganizationItem::Folder(id) => {
            let id = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS)?;
            valid_folder_ids
                .contains(&id)
                .then_some(SidebarOrganizationItem::Folder(id))?
        }
        SidebarOrganizationItem::Project(id) => {
            SidebarOrganizationItem::Project(bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS)?)
        }
        SidebarOrganizationItem::Session(id) => {
            SidebarOrganizationItem::Session(bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS)?)
        }
        SidebarOrganizationItem::Group(id) => {
            let id = bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS)?;
            valid_group_ids
                .contains(&id)
                .then_some(SidebarOrganizationItem::Group(id))?
        }
    };
    let parent_folder_id = placement
        .parent_folder_id
        .and_then(|id| bounded_text(&id, SIDEBAR_ITEM_ID_MAX_CHARS))
        .filter(|id| valid_folder_ids.contains(id));
    Some(SidebarOrganizationPlacement {
        item,
        parent_folder_id,
    })
}

fn bounded_text(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.chars().take(max_chars).collect())
}

fn comparable_folder_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn next_unique_folder_name(
    preferred_name: &str,
    mut available: impl FnMut(&str) -> bool,
) -> String {
    if available(preferred_name) {
        return preferred_name.to_string();
    }
    for index in 2..=SIDEBAR_FOLDER_LIMIT + 1 {
        let suffix = format!(" {index}");
        let stem_chars = SIDEBAR_FOLDER_NAME_MAX_CHARS.saturating_sub(suffix.chars().count());
        let stem = preferred_name
            .chars()
            .take(stem_chars)
            .collect::<String>()
            .trim_end()
            .to_string();
        let candidate = format!("{stem}{suffix}");
        if available(&candidate) {
            return candidate;
        }
    }
    unreachable!("the folder limit guarantees an available numbered name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, AgentSessionSafety, AgentSessionState, ProjectId, WorkspaceId, WorkspaceMode,
    };

    fn agent_session(project_id: &ProjectId, last_message_at_ms: i64) -> AgentSession {
        AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: format!("Session {last_message_at_ms}"),
            project_id: project_id.clone(),
            workspace_id: WorkspaceId::new(),
            workspace_root: "/repo".into(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: last_message_at_ms.saturating_sub(1),
            updated_at_ms: last_message_at_ms,
            last_message_at_ms,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn session_projects() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("session-a".to_string(), "project-a".to_string()),
            ("session-b".to_string(), "project-b".to_string()),
        ])
    }

    #[test]
    fn sibling_folder_names_are_unique_within_each_scope_and_parent() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("alpha", "Alpha", None, None));
        assert!(!state.create_folder("duplicate", " alpha ", None, None));
        assert_eq!(
            state.next_available_folder_name("Alpha", None, None),
            Some("Alpha 2".into())
        );

        assert!(state.create_folder("container", "Container", None, None));
        assert!(state.create_folder("nested-alpha", "Alpha", None, Some("container".into()),));
        assert!(state.create_folder("project-alpha", "Alpha", Some("project-a".into()), None,));
        assert!(state.create_folder("beta", "Beta", None, None));
        assert!(!state.folder_name_available("beta", "ALPHA"));
        assert!(!state.rename_folder("beta", "ALPHA"));
        assert_eq!(
            state.folder("beta").map(|folder| folder.name.as_str()),
            Some("Beta")
        );
    }

    #[test]
    fn workspace_folder_children_must_share_the_workspace_scope() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder_with_workspace(
            "parent",
            "Parent",
            Some("project-a".into()),
            Some("workspace-a".into()),
            None,
        ));
        assert!(!state.create_folder_with_workspace(
            "wrong-child",
            "Child",
            Some("project-a".into()),
            Some("workspace-b".into()),
            Some("parent".into()),
        ));
        assert!(state.create_folder_with_workspace(
            "child",
            "Child",
            Some("project-a".into()),
            Some("workspace-a".into()),
            Some("parent".into()),
        ));
    }

    #[test]
    fn new_folders_are_inserted_before_existing_siblings() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("older", "Older", None, None));
        assert!(state.create_folder("newer", "Newer", None, None));
        assert!(state.create_folder("child-older", "Child Older", None, Some("older".into())));
        assert!(state.create_folder("child-newer", "Child Newer", None, Some("older".into())));

        let available = state
            .folders
            .keys()
            .cloned()
            .map(SidebarOrganizationItem::Folder)
            .collect::<Vec<_>>();
        assert_eq!(
            state.ordered_children(None, &available),
            [
                SidebarOrganizationItem::Folder("newer".into()),
                SidebarOrganizationItem::Folder("older".into()),
            ]
        );
        assert_eq!(
            state.ordered_children(Some("older"), &available),
            [
                SidebarOrganizationItem::Folder("child-newer".into()),
                SidebarOrganizationItem::Folder("child-older".into()),
            ]
        );
    }

    #[test]
    fn folder_moves_reject_a_name_collision_at_the_destination() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("left", "Left", None, None));
        assert!(state.create_folder("right", "Right", None, None));
        assert!(state.create_folder("left-child", "Shared", None, Some("left".into())));
        assert!(state.create_folder("right-child", "shared", None, Some("right".into())));
        let moving = SidebarOrganizationItem::Folder("left-child".into());

        assert!(!state.can_move_into(&moving, "right", &BTreeMap::new()));
        assert!(!state.move_into(&moving, "right", &BTreeMap::new()));
        assert_eq!(state.parent_of(&moving).as_deref(), Some("left"));

        assert!(state.rename_folder("right-child", "Other"));
        assert!(state.move_into(&moving, "right", &BTreeMap::new()));
        assert_eq!(state.parent_of(&moving).as_deref(), Some("right"));

        assert!(state.create_folder("root-shared", "Shared", None, None));
        assert!(!state.can_move_to_scope_root(
            &moving,
            &SidebarOrganizationScope::Root,
            &BTreeMap::new(),
        ));
        assert!(!state.move_to_scope_root_end(
            &moving,
            &SidebarOrganizationScope::Root,
            &BTreeMap::new(),
        ));
        assert_eq!(state.parent_of(&moving).as_deref(), Some("right"));
    }

    #[test]
    fn normalization_repairs_names_and_delete_removes_nested_subtree() {
        let mut state = SidebarOrganizationState {
            folders: BTreeMap::from([
                (
                    "first".into(),
                    SidebarFolderUiState {
                        name: "Duplicate".into(),
                        project_id: None,
                        workspace_id: None,
                        auto_archive_after_days: None,
                    },
                ),
                (
                    "second".into(),
                    SidebarFolderUiState {
                        name: "duplicate".into(),
                        project_id: None,
                        workspace_id: None,
                        auto_archive_after_days: None,
                    },
                ),
            ]),
            placements: vec![
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("first".into()),
                    parent_folder_id: None,
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("second".into()),
                    parent_folder_id: None,
                },
            ],
            groups: BTreeMap::new(),
            collapsed_folder_ids: BTreeSet::new(),
            collapsed_group_ids: BTreeSet::new(),
        };

        state.normalize();
        assert_eq!(state.folder("first").unwrap().name, "Duplicate");
        assert_eq!(state.folder("second").unwrap().name, "duplicate 2");

        assert!(state.create_folder("parent", "Parent", None, None));
        assert!(state.create_folder("nested", "Promoted", None, Some("parent".into())));
        assert!(state.create_folder("root", "Promoted", None, None));
        assert!(state.delete_folder("parent"));
        assert!(!state.folders.contains_key("parent"));
        assert!(!state.folders.contains_key("nested"));
        assert!(state.folders.contains_key("root"));
    }

    #[test]
    fn organization_rejects_cross_project_and_cyclic_moves() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("a", "A", Some("project-a".into()), None));
        assert!(state.create_folder(
            "a-child",
            "Child",
            Some("project-a".into()),
            Some("a".into())
        ));
        assert!(state.create_folder("b", "B", Some("project-b".into()), None));
        state.reconcile(
            &["project-a".into(), "project-b".into()],
            &[
                ("session-a".into(), "project-a".into()),
                ("session-b".into(), "project-b".into()),
            ],
        );
        let sessions = session_projects();

        assert!(state.can_move_into(
            &SidebarOrganizationItem::Session("session-a".into()),
            "a",
            &sessions,
        ));
        assert!(!state.can_move_into(
            &SidebarOrganizationItem::Session("session-a".into()),
            "b",
            &sessions,
        ));
        assert!(!state.can_move_into(
            &SidebarOrganizationItem::Folder("a".into()),
            "a-child",
            &sessions,
        ));
    }

    #[test]
    fn deleting_a_folder_removes_nested_classification_and_unplaces_contents() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("parent", "Parent", None, None));
        assert!(state.create_folder("child", "Child", None, Some("parent".into())));
        state.reconcile(&["project-a".into()], &[]);
        assert!(state.move_into(
            &SidebarOrganizationItem::Project("project-a".into()),
            "parent",
            &BTreeMap::new(),
        ));
        assert!(state.folder_contains_items("parent"));
        assert!(!state.folder_contains_items("child"));

        assert!(state.delete_folder("parent"));
        assert!(!state.folders.contains_key("parent"));
        assert!(!state.folders.contains_key("child"));
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Project("project-a".into())),
            None
        );
        assert_eq!(
            state.ordered_children(
                None,
                &[SidebarOrganizationItem::Project("project-a".into())],
            ),
            [SidebarOrganizationItem::Project("project-a".into())]
        );
    }

    #[test]
    fn deleting_a_project_folder_removes_nested_session_classification() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("parent", "Parent", Some("project-a".into()), None,));
        assert!(state.create_folder(
            "child",
            "Child",
            Some("project-a".into()),
            Some("parent".into()),
        ));
        state.reconcile(
            &["project-a".into()],
            &[("session-a".into(), "project-a".into())],
        );
        let sessions = BTreeMap::from([("session-a".to_string(), "project-a".to_string())]);
        assert!(state.move_into(
            &SidebarOrganizationItem::Session("session-a".into()),
            "child",
            &sessions,
        ));

        assert!(state.delete_folder("parent"));
        assert!(!state.folders.contains_key("parent"));
        assert!(!state.folders.contains_key("child"));
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Session("session-a".into())),
            None
        );
        assert_eq!(
            state.ordered_children(
                None,
                &[SidebarOrganizationItem::Session("session-a".into())],
            ),
            [SidebarOrganizationItem::Session("session-a".into())]
        );
    }

    #[test]
    fn nested_items_can_move_back_to_their_scope_root() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("root", "Root", None, None));
        assert!(state.create_folder("project", "Project", Some("project-a".into()), None));
        state.reconcile(
            &["project-a".into()],
            &[("session-a".into(), "project-a".into())],
        );
        let sessions = BTreeMap::from([("session-a".to_string(), "project-a".to_string())]);
        assert!(state.move_into(
            &SidebarOrganizationItem::Project("project-a".into()),
            "root",
            &sessions,
        ));
        assert!(state.move_into(
            &SidebarOrganizationItem::Session("session-a".into()),
            "project",
            &sessions,
        ));

        assert!(state.move_to_scope_root_end(
            &SidebarOrganizationItem::Project("project-a".into()),
            &SidebarOrganizationScope::Root,
            &sessions,
        ));
        assert!(state.move_to_scope_root_end(
            &SidebarOrganizationItem::Session("session-a".into()),
            &SidebarOrganizationScope::Project("project-a".into()),
            &sessions,
        ));
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Project("project-a".into())),
            None
        );
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Session("session-a".into())),
            None
        );
    }

    #[test]
    fn multiple_items_move_as_one_ordered_block() {
        let mut state = SidebarOrganizationState::default();
        state.reconcile(
            &[
                "project-a".into(),
                "project-b".into(),
                "project-c".into(),
                "project-d".into(),
            ],
            &[],
        );
        let moving = [
            SidebarOrganizationItem::Project("project-b".into()),
            SidebarOrganizationItem::Project("project-d".into()),
        ];
        assert!(state.move_many_relative(
            &moving,
            &SidebarOrganizationItem::Project("project-a".into()),
            false,
            &BTreeMap::new(),
        ));
        assert_eq!(
            state.ordered_children(
                None,
                &[
                    SidebarOrganizationItem::Project("project-a".into()),
                    SidebarOrganizationItem::Project("project-b".into()),
                    SidebarOrganizationItem::Project("project-c".into()),
                    SidebarOrganizationItem::Project("project-d".into()),
                ],
            ),
            [
                SidebarOrganizationItem::Project("project-b".into()),
                SidebarOrganizationItem::Project("project-d".into()),
                SidebarOrganizationItem::Project("project-a".into()),
                SidebarOrganizationItem::Project("project-c".into()),
            ]
        );
    }

    #[test]
    fn multiple_sessions_move_into_a_folder_together() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("sessions", "Sessions", Some("project-a".into()), None,));
        state.reconcile(
            &["project-a".into()],
            &[
                ("session-a".into(), "project-a".into()),
                ("session-b".into(), "project-a".into()),
                ("session-c".into(), "project-a".into()),
            ],
        );
        let sessions = BTreeMap::from([
            ("session-a".to_string(), "project-a".to_string()),
            ("session-b".to_string(), "project-a".to_string()),
            ("session-c".to_string(), "project-a".to_string()),
        ]);
        let moving = [
            SidebarOrganizationItem::Session("session-c".into()),
            SidebarOrganizationItem::Session("session-a".into()),
        ];
        assert!(state.move_many_into(&moving, "sessions", &sessions));
        assert_eq!(state.ordered_children(Some("sessions"), &moving), moving);
        assert!(state.folder_contains_items("sessions"));
    }

    #[test]
    fn nested_projects_sessions_and_folders_can_transfer_between_folders() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("root-a", "Root A", None, None));
        assert!(state.create_folder("root-b", "Root B", None, None));
        assert!(state.create_folder("nested", "Nested", None, Some("root-a".into())));
        assert!(state.create_folder("project-a", "Project A", Some("project-a".into()), None,));
        assert!(state.create_folder("project-b", "Project B", Some("project-a".into()), None,));
        state.reconcile(
            &["project-a".into()],
            &[("session-a".into(), "project-a".into())],
        );
        let sessions = BTreeMap::from([("session-a".to_string(), "project-a".to_string())]);
        let project = SidebarOrganizationItem::Project("project-a".into());
        let session = SidebarOrganizationItem::Session("session-a".into());
        let folder = SidebarOrganizationItem::Folder("nested".into());

        assert!(state.move_into(&project, "root-a", &sessions));
        assert!(state.move_into(&project, "root-b", &sessions));
        assert_eq!(state.parent_of(&project).as_deref(), Some("root-b"));

        assert!(state.move_into(&session, "project-a", &sessions));
        assert!(state.move_into(&session, "project-b", &sessions));
        assert_eq!(state.parent_of(&session).as_deref(), Some("project-b"));

        assert!(state.move_into(&folder, "root-b", &sessions));
        assert_eq!(state.parent_of(&folder).as_deref(), Some("root-b"));
        assert!(state.move_to_scope_root_end(&folder, &SidebarOrganizationScope::Root, &sessions,));
        assert_eq!(state.parent_of(&folder), None);
    }

    #[test]
    fn moving_folders_preserves_the_depth_limit_for_the_whole_subtree() {
        let mut state = SidebarOrganizationState::default();
        let mut parent = None;
        for depth in 0..SIDEBAR_FOLDER_DEPTH_LIMIT {
            let folder_id = format!("deep-{depth}");
            assert!(state.create_folder(
                folder_id.clone(),
                folder_id.clone(),
                None,
                parent.clone(),
            ));
            parent = Some(folder_id);
        }
        assert!(state.create_folder("moving", "Moving", None, None));
        assert!(state.create_folder("moving-child", "Moving Child", None, Some("moving".into()),));

        let sessions = BTreeMap::new();
        let moving = SidebarOrganizationItem::Folder("moving".into());
        assert!(!state.can_move_into(&moving, "deep-30", &sessions));
        assert!(!state.can_move_relative(
            &moving,
            &SidebarOrganizationItem::Folder("deep-31".into()),
            &sessions,
        ));

        let moving_child = SidebarOrganizationItem::Folder("moving-child".into());
        assert!(state.move_into(&moving_child, "deep-30", &sessions));
        assert_eq!(state.parent_of(&moving_child).as_deref(), Some("deep-30"));
    }

    #[test]
    fn relative_moves_change_parent_and_keep_the_requested_sibling_order() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("folder-a", "A", None, None));
        assert!(state.create_folder("folder-a-child", "A Child", None, Some("folder-a".into()),));
        assert!(state.create_folder("folder-b", "B", None, None));

        let moving = SidebarOrganizationItem::Folder("folder-a-child".into());
        let target = SidebarOrganizationItem::Folder("folder-b".into());
        assert!(state.move_relative(&moving, &target, false, &BTreeMap::new()));
        assert_eq!(state.parent_of(&moving), None);
        assert_eq!(
            state.ordered_children(
                None,
                &[
                    SidebarOrganizationItem::Folder("folder-a".into()),
                    moving.clone(),
                    target.clone(),
                ],
            ),
            [
                moving,
                target,
                SidebarOrganizationItem::Folder("folder-a".into()),
            ]
        );
    }

    #[test]
    fn ordered_children_projects_each_item_only_below_its_direct_parent() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("root", "Root", None, None));
        assert!(state.create_folder("child", "Child", None, Some("root".into())));
        assert!(state.create_folder("sibling", "Sibling", None, None));
        let available = [
            SidebarOrganizationItem::Folder("root".into()),
            SidebarOrganizationItem::Folder("child".into()),
            SidebarOrganizationItem::Folder("sibling".into()),
            SidebarOrganizationItem::Project("unplaced".into()),
        ];

        assert_eq!(
            state.ordered_children(None, &available),
            [
                SidebarOrganizationItem::Folder("sibling".into()),
                SidebarOrganizationItem::Folder("root".into()),
                SidebarOrganizationItem::Project("unplaced".into()),
            ]
        );
        assert_eq!(
            state.ordered_children(Some("root"), &available),
            [SidebarOrganizationItem::Folder("child".into())]
        );
        assert!(state.ordered_children(Some("child"), &available).is_empty());
    }

    #[test]
    fn reconcile_preserves_manual_session_order_and_inserts_new_sessions_first() {
        let mut state = SidebarOrganizationState::default();
        let sessions = BTreeMap::from([
            ("first".to_string(), "project".to_string()),
            ("second".to_string(), "project".to_string()),
        ]);
        state.reconcile(
            &["project".into()],
            &[
                ("first".into(), "project".into()),
                ("second".into(), "project".into()),
            ],
        );
        assert!(state.move_relative(
            &SidebarOrganizationItem::Session("second".into()),
            &SidebarOrganizationItem::Session("first".into()),
            false,
            &sessions,
        ));

        state.reconcile(
            &["project".into()],
            &[
                ("new".into(), "project".into()),
                ("second".into(), "project".into()),
                ("first".into(), "project".into()),
            ],
        );
        let available = [
            SidebarOrganizationItem::Session("first".into()),
            SidebarOrganizationItem::Session("second".into()),
            SidebarOrganizationItem::Session("new".into()),
        ];

        assert_eq!(
            state.ordered_children(None, &available),
            [
                SidebarOrganizationItem::Session("new".into()),
                SidebarOrganizationItem::Session("second".into()),
                SidebarOrganizationItem::Session("first".into()),
            ]
        );
    }

    #[test]
    fn reconcile_inserts_new_session_without_shifting_sessions_across_folder() {
        let mut state = SidebarOrganizationState::default();
        let sessions = BTreeMap::from([
            ("first".to_string(), "project".to_string()),
            ("second".to_string(), "project".to_string()),
        ]);
        state.reconcile(
            &["project".into()],
            &[
                ("first".into(), "project".into()),
                ("second".into(), "project".into()),
            ],
        );
        assert!(state.create_folder("folder", "Folder", Some("project".into()), None,));
        assert!(state.move_relative(
            &SidebarOrganizationItem::Folder("folder".into()),
            &SidebarOrganizationItem::Session("first".into()),
            true,
            &sessions,
        ));

        state.reconcile(
            &["project".into()],
            &[
                ("new".into(), "project".into()),
                ("first".into(), "project".into()),
                ("second".into(), "project".into()),
            ],
        );

        assert_eq!(
            state.placements,
            [
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Project("project".into()),
                    parent_folder_id: None,
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Session("new".into()),
                    parent_folder_id: None,
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Session("first".into()),
                    parent_folder_id: None,
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("folder".into()),
                    parent_folder_id: None,
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Session("second".into()),
                    parent_folder_id: None,
                },
            ]
        );
    }

    #[test]
    fn normalization_reserves_placements_for_folders_at_the_item_limit() {
        let mut state = SidebarOrganizationState {
            folders: BTreeMap::from([(
                "folder".into(),
                SidebarFolderUiState {
                    name: "Folder".into(),
                    project_id: None,
                    workspace_id: None,
                    auto_archive_after_days: None,
                },
            )]),
            placements: (0..SIDEBAR_ORGANIZATION_ITEM_LIMIT)
                .map(|index| SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Project(format!("project-{index}")),
                    parent_folder_id: None,
                })
                .collect(),
            groups: BTreeMap::new(),
            collapsed_folder_ids: BTreeSet::new(),
            collapsed_group_ids: BTreeSet::new(),
        };

        state.normalize();

        assert_eq!(state.placements.len(), SIDEBAR_ORGANIZATION_ITEM_LIMIT);
        assert!(state.placements.iter().any(|placement| {
            placement.item == SidebarOrganizationItem::Folder("folder".into())
        }));
    }

    #[test]
    fn normalize_breaks_folder_cycles_and_deduplicates_items() {
        let mut state = SidebarOrganizationState {
            folders: BTreeMap::from([
                (
                    "a".into(),
                    SidebarFolderUiState {
                        name: "A".into(),
                        project_id: None,
                        workspace_id: None,
                        auto_archive_after_days: None,
                    },
                ),
                (
                    "b".into(),
                    SidebarFolderUiState {
                        name: "B".into(),
                        project_id: None,
                        workspace_id: None,
                        auto_archive_after_days: None,
                    },
                ),
            ]),
            placements: vec![
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("a".into()),
                    parent_folder_id: Some("b".into()),
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("b".into()),
                    parent_folder_id: Some("a".into()),
                },
                SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Folder("a".into()),
                    parent_folder_id: None,
                },
            ],
            collapsed_folder_ids: BTreeSet::from(["a".into(), "missing".into()]),
            groups: BTreeMap::new(),
            collapsed_group_ids: BTreeSet::new(),
        };

        state.normalize();

        assert_eq!(
            state
                .placements
                .iter()
                .filter(|placement| placement.item == SidebarOrganizationItem::Folder("a".into()))
                .count(),
            1
        );
        assert!(!state.folder_is_descendant_of("a", "a"));
        assert_eq!(state.collapsed_folder_ids, BTreeSet::from(["a".into()]));
    }

    #[test]
    fn scheduled_archive_settings_are_project_scoped_bounded_and_unique() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("root", "Root", None, None));
        assert!(state.create_folder("first", "First", Some("project-a".into()), None));
        assert!(state.create_folder("second", "Second", Some("project-a".into()), None));

        assert!(!state.set_folder_auto_archive_after_days("root", Some(3)));
        assert!(!state.set_folder_auto_archive_after_days("first", Some(0)));
        assert!(
            !state.set_folder_auto_archive_after_days(
                "first",
                Some(SIDEBAR_AUTO_ARCHIVE_MAX_DAYS + 1),
            )
        );
        assert!(state.set_folder_auto_archive_after_days("first", Some(3)));
        assert!(state.set_folder_auto_archive_after_days("second", Some(7)));
        assert_eq!(state.folder("first").unwrap().auto_archive_after_days, None);
        assert_eq!(
            state.folder("second").unwrap().auto_archive_after_days,
            Some(7),
        );
        assert!(state.set_folder_auto_archive_after_days("second", None));
    }

    #[test]
    fn scheduled_archives_move_only_unfiled_unpinned_inactive_sessions_in_time_order() {
        let project = ProjectId::new();
        let other_project = ProjectId::new();
        let now_ms = 20 * MILLIS_PER_DAY;
        let recent = agent_session(&project, now_ms - 2 * MILLIS_PER_DAY);
        let older = agent_session(&project, now_ms - 6 * MILLIS_PER_DAY);
        let old = agent_session(&project, now_ms - 4 * MILLIS_PER_DAY);
        let pinned = agent_session(&project, now_ms - 8 * MILLIS_PER_DAY);
        let organized = agent_session(&project, now_ms - 9 * MILLIS_PER_DAY);
        let other = agent_session(&other_project, now_ms - 10 * MILLIS_PER_DAY);
        let sessions = vec![
            recent.clone(),
            older.clone(),
            old.clone(),
            pinned.clone(),
            organized.clone(),
            other.clone(),
        ];

        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder(
            "archive",
            "Archive",
            Some(project.as_str().to_string()),
            None,
        ));
        assert!(state.create_folder("manual", "Manual", Some(project.as_str().to_string()), None,));
        state.reconcile(
            &[
                project.as_str().to_string(),
                other_project.as_str().to_string(),
            ],
            &sessions
                .iter()
                .map(|session| {
                    (
                        session.id.as_str().to_string(),
                        session.project_id.as_str().to_string(),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let session_projects = sessions
            .iter()
            .map(|session| {
                (
                    session.id.as_str().to_string(),
                    session.project_id.as_str().to_string(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert!(state.move_into(
            &SidebarOrganizationItem::Session(organized.id.as_str().to_string()),
            "manual",
            &session_projects,
        ));
        assert!(state.set_folder_auto_archive_after_days("archive", Some(3)));

        assert!(state.apply_scheduled_archives(
            &sessions,
            &BTreeSet::from([pinned.id.as_str().to_string()]),
            now_ms,
        ));
        for session in [&old, &older] {
            assert_eq!(
                state
                    .parent_of(&SidebarOrganizationItem::Session(
                        session.id.as_str().to_string(),
                    ))
                    .as_deref(),
                Some("archive"),
            );
        }
        for session in [&recent, &pinned, &other] {
            assert_eq!(
                state.parent_of(&SidebarOrganizationItem::Session(
                    session.id.as_str().to_string(),
                )),
                None,
            );
        }
        assert_eq!(
            state
                .parent_of(&SidebarOrganizationItem::Session(
                    organized.id.as_str().to_string(),
                ))
                .as_deref(),
            Some("manual"),
        );
        let archived = [
            SidebarOrganizationItem::Session(older.id.as_str().to_string()),
            SidebarOrganizationItem::Session(old.id.as_str().to_string()),
        ];
        assert_eq!(
            state.ordered_children(Some("archive"), &archived),
            [
                SidebarOrganizationItem::Session(old.id.as_str().to_string()),
                SidebarOrganizationItem::Session(older.id.as_str().to_string()),
            ],
        );
    }

    fn group_workspaces() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("session-a".to_string(), "workspace-a".to_string()),
            ("session-b".to_string(), "workspace-a".to_string()),
            ("session-c".to_string(), "workspace-b".to_string()),
        ])
    }

    fn group_members(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn a_group_is_created_with_a_placement_and_its_members() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        let group = state.group("group-1").expect("group should exist");
        assert_eq!(
            group.member_session_ids,
            group_members(&["session-a", "session-b"])
        );
        assert_eq!(group.workspace_id, "workspace-a");
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Group("group-1".into())),
            None
        );
        assert_eq!(state.group_of_session("session-a"), Some("group-1"));
        assert_eq!(state.group_of_session("session-c"), None);
    }

    #[test]
    fn a_group_refuses_members_from_another_worktree() {
        let mut state = SidebarOrganizationState::default();
        assert!(!state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-c"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.group("group-1").is_none());
    }

    #[test]
    fn adding_a_session_that_already_belongs_to_a_group_moves_it() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.create_group(
            "group-2",
            "会话组 2",
            "project-a",
            "workspace-a",
            &group_members(&["session-b"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.add_sessions_to_group(
            "group-2",
            &group_members(&["session-a"]),
            &group_workspaces()
        ));
        // A session belongs to exactly one group, so the move vacates group-1.
        assert_eq!(state.group_of_session("session-a"), Some("group-2"));
        assert!(state.group("group-1").is_none());
        assert_eq!(
            state
                .group("group-2")
                .expect("group-2 should remain")
                .member_session_ids,
            group_members(&["session-b", "session-a"])
        );
    }

    #[test]
    fn removing_the_last_member_dissolves_the_group_and_keeps_the_sessions() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.remove_sessions_from_group("group-1", &group_members(&["session-a"])));
        assert!(state.group("group-1").is_some());
        assert!(state.remove_sessions_from_group("group-1", &group_members(&["session-b"])));
        // The group goes; the sessions stay authoritative and simply ungrouped.
        assert!(state.group("group-1").is_none());
        assert_eq!(state.group_of_session("session-a"), None);
        assert!(!state
            .placements
            .iter()
            .any(|placement| placement.item == SidebarOrganizationItem::Group("group-1".into())));
    }

    #[test]
    fn deleting_a_group_keeps_its_sessions_placements() {
        let mut state = SidebarOrganizationState::default();
        state.placements.push(SidebarOrganizationPlacement {
            item: SidebarOrganizationItem::Session("session-a".into()),
            parent_folder_id: None,
        });
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.delete_group("group-1"));
        assert!(state.group("group-1").is_none());
        assert!(state.placements.iter().any(
            |placement| placement.item == SidebarOrganizationItem::Session("session-a".into())
        ));
    }

    #[test]
    fn reconcile_dissolves_a_group_whose_sessions_are_gone() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        // Both members disappeared — a deleted Worktree deletes its sessions.
        state.reconcile(&["project-a".to_string()], &[]);
        assert!(state.group("group-1").is_none());
    }

    #[test]
    fn reconcile_prunes_missing_members_without_dropping_the_group() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        state.reconcile(
            &["project-a".to_string()],
            &[("session-b".to_string(), "project-a".to_string())],
        );
        let group = state.group("group-1").expect("group should survive");
        assert_eq!(group.member_session_ids, group_members(&["session-b"]));
    }

    #[test]
    fn cleanup_references_drops_groups_and_members_that_are_gone() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        state.cleanup_references(
            &BTreeSet::from(["project-a".to_string()]),
            &BTreeSet::from(["session-a".to_string()]),
        );
        let group = state.group("group-1").expect("group should survive");
        assert_eq!(group.member_session_ids, group_members(&["session-a"]));
        state.cleanup_references(&BTreeSet::new(), &BTreeSet::new());
        assert!(state.group("group-1").is_none());
    }

    #[test]
    fn a_group_can_sit_inside_a_folder_and_survives_deleting_the_folder() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("folder-1", "Archive", Some("project-a".to_string()), None,));
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            Some("folder-1".to_string()),
        ));
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Group("group-1".into())),
            Some("folder-1".to_string())
        );
        assert!(state.delete_folder("folder-1"));
        // The folder's classification goes, but the group is not a folder child
        // record: it becomes unplaced and is restored by the next projection.
        assert!(state.group("group-1").is_some());
    }

    #[test]
    fn group_names_are_unique_within_one_worktree() {
        let mut state = SidebarOrganizationState::default();
        assert_eq!(
            state.next_available_group_name("project-a", "workspace-a", "会话组"),
            "会话组 1"
        );
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            None,
        ));
        assert_eq!(
            state.next_available_group_name("project-a", "workspace-a", "会话组"),
            "会话组 2"
        );
        // Another Worktree has its own ordinals.
        assert_eq!(
            state.next_available_group_name("project-a", "workspace-b", "会话组"),
            "会话组 1"
        );
        assert!(!state.rename_group("group-1", "  "));
        assert!(state.rename_group("group-1", "重构"));
        assert_eq!(
            state.group("group-1").map(|group| group.name.as_str()),
            Some("重构")
        );
    }

    #[test]
    fn pinning_and_collapsing_a_group_are_independent_of_sessions() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            None,
        ));
        assert!(!state.group_is_pinned("group-1"));
        assert!(state.set_group_pinned("group-1", true));
        assert!(state.group_is_pinned("group-1"));
        assert_eq!(state.toggle_group_collapsed("group-1"), Some(true));
        assert!(state.group_is_collapsed("group-1"));
        assert_eq!(state.toggle_group_collapsed("group-1"), Some(false));
        assert_eq!(state.toggle_group_collapsed("missing"), None);
    }

    #[test]
    fn a_group_carries_a_group_level_auto_continue() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a"]),
            &group_workspaces(),
            None,
        ));
        assert_eq!(
            state.group("group-1").and_then(|group| group.auto_continue),
            None
        );
        assert!(state.set_group_auto_continue("group-1", Some(true)));
        assert_eq!(
            state.group("group-1").and_then(|group| group.auto_continue),
            Some(true)
        );
        assert!(!state.set_group_auto_continue("group-1", Some(true)));
    }

    #[test]
    fn grouped_sessions_are_listed_once_under_their_group() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        let grouped = state.grouped_session_ids();
        assert!(grouped.contains("session-a"));
        assert!(grouped.contains("session-b"));
        assert!(!grouped.contains("session-c"));
    }

    #[test]
    fn deleting_a_group_makes_its_sessions_siblings_again() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        let project_id = "project-a".to_string();
        let workspace_id = "workspace-a".to_string();
        let group_ids = state
            .groups_for_workspace(&project_id, &workspace_id)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let items = crate::sidebar_project_items_for_workspace(
            &state,
            &project_id,
            Some(&workspace_id),
            false,
            false,
            &group_members(&["session-a", "session-b"]),
            &group_ids,
            &BTreeSet::new(),
            None,
        );
        // While the group is listed, its members are not also siblings.
        assert!(items.contains(&SidebarOrganizationItem::Group("group-1".into())));
        assert!(!items.contains(&SidebarOrganizationItem::Session("session-a".into())));

        assert!(state.delete_group("group-1"));
        let items = crate::sidebar_project_items_for_workspace(
            &state,
            &project_id,
            Some(&workspace_id),
            false,
            false,
            &group_members(&["session-a", "session-b"]),
            &[],
            &BTreeSet::new(),
            None,
        );
        assert!(items.contains(&SidebarOrganizationItem::Session("session-a".into())));
    }

    #[test]
    fn a_group_round_trips_through_serde() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_group(
            "group-1",
            "会话组 1",
            "project-a",
            "workspace-a",
            &group_members(&["session-a", "session-b"]),
            &group_workspaces(),
            None,
        ));
        assert!(state.set_group_pinned("group-1", true));
        let encoded = serde_json::to_string(&state).expect("state should serialize");
        let decoded: SidebarOrganizationState =
            serde_json::from_str(&encoded).expect("state should deserialize");
        assert_eq!(decoded.groups, state.groups);
        assert_eq!(decoded.placements, state.placements);
    }
}
