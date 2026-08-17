use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

const SIDEBAR_FOLDER_LIMIT: usize = 2_000;
const SIDEBAR_ORGANIZATION_ITEM_LIMIT: usize = 5_000;
const SIDEBAR_FOLDER_DEPTH_LIMIT: usize = 32;
const SIDEBAR_ITEM_ID_MAX_CHARS: usize = 256;
const SIDEBAR_FOLDER_NAME_MAX_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SidebarOrganizationItem {
    Folder(String),
    Project(String),
    Session(String),
}

impl SidebarOrganizationItem {
    pub fn id(&self) -> &str {
        match self {
            Self::Folder(id) | Self::Project(id) | Self::Session(id) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarFolderUiState {
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SidebarOrganizationState {
    #[serde(default)]
    pub folders: BTreeMap<String, SidebarFolderUiState>,
    #[serde(default)]
    pub placements: Vec<SidebarOrganizationPlacement>,
    #[serde(default)]
    pub collapsed_folder_ids: BTreeSet<String>,
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
            folders
                .entry(id)
                .or_insert(SidebarFolderUiState { name, project_id });
        }
        self.folders = folders;

        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        self.placements = std::mem::take(&mut self.placements)
            .into_iter()
            .filter_map(|placement| normalize_placement(placement, &valid_folder_ids))
            .filter(|placement| seen.insert(placement.item.clone()))
            .take(SIDEBAR_ORGANIZATION_ITEM_LIMIT)
            .collect();
        self.ensure_folder_placements();
        self.enforce_placement_limit();
        self.repair_parents();
        self.deduplicate_sibling_folder_names();
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
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
        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        self.placements.retain(|placement| match &placement.item {
            SidebarOrganizationItem::Folder(id) => valid_folder_ids.contains(id),
            SidebarOrganizationItem::Project(id) => valid_project_ids.contains(id),
            SidebarOrganizationItem::Session(id) => session_projects.contains_key(id),
        });
        self.ensure_folder_placements();
        self.repair_parents();

        for placement in &mut self.placements {
            let valid_parent = placement.parent_folder_id.as_ref().is_none_or(|parent_id| {
                let Some(parent) = self.folders.get(parent_id) else {
                    return false;
                };
                match &placement.item {
                    SidebarOrganizationItem::Folder(folder_id) => self
                        .folders
                        .get(folder_id)
                        .is_some_and(|folder| folder.project_id == parent.project_id),
                    SidebarOrganizationItem::Project(_) => parent.project_id.is_none(),
                    SidebarOrganizationItem::Session(session_id) => session_projects
                        .get(session_id)
                        .is_some_and(|project_id| parent.project_id.as_ref() == Some(project_id)),
                }
            });
            if !valid_parent {
                placement.parent_folder_id = None;
            }
        }
        self.deduplicate_sibling_folder_names();

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
        for (session_id, _) in ordered_session_projects {
            let item = SidebarOrganizationItem::Session(session_id.clone());
            if placed.insert(item.clone()) {
                self.placements.push(SidebarOrganizationPlacement {
                    item,
                    parent_folder_id: None,
                });
            }
        }
        self.enforce_placement_limit();
        self.align_root_session_order(ordered_session_projects);
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
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
        let valid_folder_ids = self.folders.keys().cloned().collect::<BTreeSet<_>>();
        self.placements.retain(|placement| match &placement.item {
            SidebarOrganizationItem::Folder(id) => valid_folder_ids.contains(id),
            SidebarOrganizationItem::Project(id) => project_ids.contains(id),
            SidebarOrganizationItem::Session(id) => session_ids.contains(id),
        });
        self.ensure_folder_placements();
        self.enforce_placement_limit();
        self.repair_parents();
        self.deduplicate_sibling_folder_names();
        self.collapsed_folder_ids
            .retain(|id| self.folders.contains_key(id));
    }

    pub fn create_folder(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: Option<String>,
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
        let parent_folder_id = match parent_folder_id {
            Some(parent_id) => {
                let Some(parent_id) = bounded_text(&parent_id, SIDEBAR_ITEM_ID_MAX_CHARS) else {
                    return false;
                };
                let Some(parent) = self.folders.get(&parent_id) else {
                    return false;
                };
                if parent.project_id != project_id
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
            parent_folder_id.as_deref(),
        ) {
            return false;
        }
        self.folders
            .insert(id.clone(), SidebarFolderUiState { name, project_id });
        self.placements.push(SidebarOrganizationPlacement {
            item: SidebarOrganizationItem::Folder(id),
            parent_folder_id,
        });
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
            parent_folder_id.as_deref(),
        )
    }

    pub fn next_available_folder_name(
        &self,
        preferred_name: &str,
        project_id: Option<&str>,
        parent_folder_id: Option<&str>,
    ) -> Option<String> {
        let preferred_name = bounded_text(preferred_name, SIDEBAR_FOLDER_NAME_MAX_CHARS)?;
        Some(next_unique_folder_name(&preferred_name, |candidate| {
            self.folder_name_is_available_at(None, candidate, project_id, parent_folder_id)
        }))
    }

    pub fn delete_folder(&mut self, folder_id: &str) -> bool {
        if !self.folders.contains_key(folder_id) {
            return false;
        }
        let parent = self.parent_of(&SidebarOrganizationItem::Folder(folder_id.to_string()));
        for placement in &mut self.placements {
            if placement.parent_folder_id.as_deref() == Some(folder_id) {
                placement.parent_folder_id = parent.clone();
            }
        }
        self.placements.retain(|placement| {
            placement.item != SidebarOrganizationItem::Folder(folder_id.to_string())
        });
        self.collapsed_folder_ids.remove(folder_id);
        self.folders.remove(folder_id);
        self.deduplicate_sibling_folder_names();
        true
    }

    pub fn folder(&self, folder_id: &str) -> Option<&SidebarFolderUiState> {
        self.folders.get(folder_id)
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
        let placed = self
            .placements
            .iter()
            .map(|placement| placement.item.clone())
            .collect::<BTreeSet<_>>();
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
        if !self.can_move_relative(moving, target, session_projects) {
            return false;
        }
        let Some(source_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == moving)
        else {
            return false;
        };
        let Some(original_target_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == target)
        else {
            return false;
        };
        let old_parent = self.placements[source_index].parent_folder_id.clone();
        let target_parent = self.placements[original_target_index]
            .parent_folder_id
            .clone();
        if old_parent == target_parent
            && ((!after && source_index + 1 == original_target_index)
                || (after && original_target_index + 1 == source_index))
        {
            return false;
        }
        let mut moving_placement = self.placements.remove(source_index);
        let Some(target_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == target)
        else {
            self.placements.insert(source_index, moving_placement);
            return false;
        };
        moving_placement.parent_folder_id = self.placements[target_index].parent_folder_id.clone();
        let insertion_index = target_index + usize::from(after);
        self.placements.insert(insertion_index, moving_placement);
        true
    }

    pub fn move_into(
        &mut self,
        moving: &SidebarOrganizationItem,
        target_folder_id: &str,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        if !self.can_move_into(moving, target_folder_id, session_projects) {
            return false;
        }
        let Some(source_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == moving)
        else {
            return false;
        };
        let old_parent = self.placements[source_index].parent_folder_id.clone();
        let mut moving_placement = self.placements.remove(source_index);
        moving_placement.parent_folder_id = Some(target_folder_id.to_string());
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
        let changed = old_parent.as_deref() != Some(target_folder_id)
            || insertion_index != source_index.min(self.placements.len());
        self.placements.insert(insertion_index, moving_placement);
        changed
    }

    pub fn move_to_scope_root_end(
        &mut self,
        moving: &SidebarOrganizationItem,
        scope: &SidebarOrganizationScope,
        session_projects: &BTreeMap<String, String>,
    ) -> bool {
        if !self.can_move_to_scope_root(moving, scope, session_projects) {
            return false;
        }
        let Some(source_index) = self
            .placements
            .iter()
            .position(|placement| &placement.item == moving)
        else {
            return false;
        };
        let old_parent = self.placements[source_index].parent_folder_id.clone();
        let mut moving_placement = self.placements.remove(source_index);
        moving_placement.parent_folder_id = None;
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
        let changed =
            old_parent.is_some() || insertion_index != source_index.min(self.placements.len());
        self.placements.insert(insertion_index, moving_placement);
        changed
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
            parent_folder_id,
        )
    }

    fn folder_name_is_available_at(
        &self,
        excluded_folder_id: Option<&str>,
        name: &str,
        project_id: Option<&str>,
        parent_folder_id: Option<&str>,
    ) -> bool {
        let comparable_name = comparable_folder_name(name);
        !self.folders.iter().any(|(candidate_id, folder)| {
            excluded_folder_id != Some(candidate_id.as_str())
                && folder.project_id.as_deref() == project_id
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
            .map(|(id, folder)| (id.clone(), folder.project_id.clone()))
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
                        SidebarOrganizationItem::Project(_) => parent_scope.is_none(),
                        SidebarOrganizationItem::Session(_) => parent_scope.is_some(),
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
                | SidebarOrganizationItem::Session(_) => None,
            })
            .collect::<Vec<_>>();
        folder_ids.extend(
            self.folders
                .keys()
                .filter(|id| seen_ids.insert((*id).clone()))
                .cloned(),
        );

        let mut used_names = BTreeMap::<(Option<String>, Option<String>), BTreeSet<String>>::new();
        for folder_id in folder_ids {
            let Some(folder) = self.folders.get(&folder_id) else {
                continue;
            };
            let name = folder.name.clone();
            let scope = folder.project_id.clone();
            let parent = self.parent_of(&SidebarOrganizationItem::Folder(folder_id.clone()));
            let names = used_names.entry((scope, parent)).or_default();
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
}

fn normalize_placement(
    placement: SidebarOrganizationPlacement,
    valid_folder_ids: &BTreeSet<String>,
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
    fn normalization_and_delete_promotion_repair_sibling_name_collisions() {
        let mut state = SidebarOrganizationState {
            folders: BTreeMap::from([
                (
                    "first".into(),
                    SidebarFolderUiState {
                        name: "Duplicate".into(),
                        project_id: None,
                    },
                ),
                (
                    "second".into(),
                    SidebarFolderUiState {
                        name: "duplicate".into(),
                        project_id: None,
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
            collapsed_folder_ids: BTreeSet::new(),
        };

        state.normalize();
        assert_eq!(state.folder("first").unwrap().name, "Duplicate");
        assert_eq!(state.folder("second").unwrap().name, "duplicate 2");

        assert!(state.create_folder("parent", "Parent", None, None));
        assert!(state.create_folder("nested", "Promoted", None, Some("parent".into())));
        assert!(state.create_folder("root", "Promoted", None, None));
        assert!(state.delete_folder("parent"));
        let promoted_names = ["nested", "root"]
            .into_iter()
            .map(|id| comparable_folder_name(&state.folder(id).unwrap().name))
            .collect::<BTreeSet<_>>();
        assert_eq!(promoted_names.len(), 2);
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
    fn deleting_a_folder_promotes_contents_without_deleting_them() {
        let mut state = SidebarOrganizationState::default();
        assert!(state.create_folder("parent", "Parent", None, None));
        assert!(state.create_folder("child", "Child", None, Some("parent".into())));
        state.reconcile(&["project-a".into()], &[]);
        assert!(state.move_into(
            &SidebarOrganizationItem::Project("project-a".into()),
            "parent",
            &BTreeMap::new(),
        ));

        assert!(state.delete_folder("parent"));
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Folder("child".into())),
            None
        );
        assert_eq!(
            state.parent_of(&SidebarOrganizationItem::Project("project-a".into())),
            None
        );
        assert!(state.folders.contains_key("child"));
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
                SidebarOrganizationItem::Folder("folder-a".into()),
                moving,
                target,
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
                SidebarOrganizationItem::Folder("root".into()),
                SidebarOrganizationItem::Folder("sibling".into()),
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
    fn normalization_reserves_placements_for_folders_at_the_item_limit() {
        let mut state = SidebarOrganizationState {
            folders: BTreeMap::from([(
                "folder".into(),
                SidebarFolderUiState {
                    name: "Folder".into(),
                    project_id: None,
                },
            )]),
            placements: (0..SIDEBAR_ORGANIZATION_ITEM_LIMIT)
                .map(|index| SidebarOrganizationPlacement {
                    item: SidebarOrganizationItem::Project(format!("project-{index}")),
                    parent_folder_id: None,
                })
                .collect(),
            collapsed_folder_ids: BTreeSet::new(),
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
                    },
                ),
                (
                    "b".into(),
                    SidebarFolderUiState {
                        name: "B".into(),
                        project_id: None,
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
}
