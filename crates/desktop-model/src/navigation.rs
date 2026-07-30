use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchRoute {
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub primary_tab: String,
    pub right_rail: String,
    pub selected_file_path: Option<String>,
    pub selected_git_path: Option<String>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationHistory {
    entries: Vec<WorkbenchRoute>,
    cursor: usize,
    capacity: usize,
}

impl NavigationHistory {
    pub fn new(initial: WorkbenchRoute, capacity: usize) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
            capacity: capacity.max(1),
        }
    }

    pub fn current(&self) -> &WorkbenchRoute {
        &self.entries[self.cursor]
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn push(&mut self, route: WorkbenchRoute) {
        if self.current() == &route {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(route);
        if self.entries.len() > self.capacity {
            let overflow = self.entries.len() - self.capacity;
            self.entries.drain(..overflow);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub fn replace(&mut self, route: WorkbenchRoute) {
        self.entries[self.cursor] = route;
    }

    pub fn back(&mut self) -> Option<&WorkbenchRoute> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        Some(self.current())
    }

    pub fn forward(&mut self) -> Option<&WorkbenchRoute> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        Some(self.current())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SidebarState {
    pub row_order: Vec<String>,
    pub pinned_ids: BTreeSet<String>,
    pub collapsed_ids: BTreeSet<String>,
    pub selected_ids: BTreeSet<String>,
}

impl SidebarState {
    pub fn toggle_selected(&mut self, id: impl Into<String>) -> bool {
        let id = id.into();
        if self.selected_ids.remove(&id) {
            false
        } else {
            self.selected_ids.insert(id);
            true
        }
    }

    pub fn select_all<I>(&mut self, ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.selected_ids.extend(ids);
    }

    pub fn clear_selection(&mut self) {
        self.selected_ids.clear();
    }

    pub fn reconcile<I>(&mut self, authoritative_ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        let authoritative = authoritative_ids.into_iter().collect::<Vec<_>>();
        let valid = authoritative.iter().cloned().collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        self.row_order
            .retain(|id| valid.contains(id) && seen.insert(id.clone()));
        self.row_order.extend(
            authoritative
                .into_iter()
                .filter(|id| seen.insert(id.clone())),
        );
        self.pinned_ids.retain(|id| valid.contains(id));
        self.collapsed_ids.retain(|id| valid.contains(id));
        self.selected_ids.retain(|id| valid.contains(id));
        let original_positions = self
            .row_order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.clone(), index))
            .collect::<BTreeMap<_, _>>();
        self.row_order.sort_by_key(|id| {
            (
                !self.pinned_ids.contains(id),
                original_positions.get(id).copied().unwrap_or(usize::MAX),
            )
        });
    }

    pub fn move_row_relative(&mut self, moving_id: &str, target_id: &str, after: bool) -> bool {
        if moving_id == target_id {
            return false;
        }
        let Some(moving_index) = self.row_order.iter().position(|id| id == moving_id) else {
            return false;
        };
        let Some(target_index) = self.row_order.iter().position(|id| id == target_id) else {
            return false;
        };
        if (!after && moving_index + 1 == target_index)
            || (after && target_index + 1 == moving_index)
        {
            return false;
        }

        let moving = self.row_order.remove(moving_index);
        let target_index = self
            .row_order
            .iter()
            .position(|id| id == target_id)
            .expect("target row must remain after removing a different row");
        self.row_order
            .insert(target_index + usize::from(after), moving);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchPathState {
    pub selected_file_path: Option<String>,
    pub selected_git_path: Option<String>,
    pub open_buffer_paths: BTreeMap<String, bool>,
}

impl WorkbenchPathState {
    pub fn move_path(&mut self, source: &str, destination: &str) {
        self.selected_file_path = self
            .selected_file_path
            .take()
            .map(|path| replace_path_prefix(&path, source, destination));
        self.selected_git_path = self
            .selected_git_path
            .take()
            .map(|path| replace_path_prefix(&path, source, destination));
        self.open_buffer_paths = std::mem::take(&mut self.open_buffer_paths)
            .into_iter()
            .map(|(path, dirty)| (replace_path_prefix(&path, source, destination), dirty))
            .collect();
    }

    pub fn delete_path(&mut self, path: &str) {
        if self
            .selected_file_path
            .as_deref()
            .is_some_and(|candidate| is_path_within(candidate, path))
        {
            self.selected_file_path = None;
        }
        if self
            .selected_git_path
            .as_deref()
            .is_some_and(|candidate| is_path_within(candidate, path))
        {
            self.selected_git_path = None;
        }
        self.open_buffer_paths
            .retain(|candidate, _| !is_path_within(candidate, path));
    }
}

fn replace_path_prefix(path: &str, source: &str, destination: &str) -> String {
    if path == source {
        return destination.to_string();
    }
    path.strip_prefix(source)
        .filter(|suffix| suffix.starts_with('/'))
        .map(|suffix| format!("{destination}{suffix}"))
        .unwrap_or_else(|| path.to_string())
}

fn is_path_within(candidate: &str, parent: &str) -> bool {
    candidate == parent
        || candidate
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(name: &str) -> WorkbenchRoute {
        WorkbenchRoute {
            primary_tab: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn back_then_new_navigation_discards_only_the_forward_branch() {
        let mut history = NavigationHistory::new(route("agent"), 10);
        history.push(route("files"));
        history.push(route("git"));
        assert_eq!(history.back(), Some(&route("files")));
        assert!(history.can_go_forward());

        history.push(route("terminal"));
        assert_eq!(history.current(), &route("terminal"));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(&route("files")));
    }

    #[test]
    fn path_updates_cover_descendants_without_touching_similar_prefixes() {
        let mut state = WorkbenchPathState {
            selected_file_path: Some("src/old/lib.rs".into()),
            selected_git_path: Some("src/older.rs".into()),
            open_buffer_paths: BTreeMap::from([
                ("src/old/a.rs".into(), true),
                ("src/older.rs".into(), false),
            ]),
        };
        state.move_path("src/old", "src/new");
        assert_eq!(state.selected_file_path.as_deref(), Some("src/new/lib.rs"));
        assert_eq!(state.selected_git_path.as_deref(), Some("src/older.rs"));
        assert!(state.open_buffer_paths.contains_key("src/new/a.rs"));
        assert!(state.open_buffer_paths.contains_key("src/older.rs"));
    }

    #[test]
    fn sidebar_rows_move_before_or_after_a_stable_target() {
        let mut state = SidebarState {
            row_order: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            ..Default::default()
        };

        assert!(state.move_row_relative("a", "c", false));
        assert_eq!(state.row_order, ["b", "a", "c", "d"]);
        assert!(state.move_row_relative("d", "b", true));
        assert_eq!(state.row_order, ["b", "d", "a", "c"]);
        assert!(!state.move_row_relative("d", "b", true));
        assert!(!state.move_row_relative("missing", "b", false));
        assert!(!state.move_row_relative("a", "missing", false));
    }

    #[test]
    fn sidebar_batch_selection_is_stable_and_reconciles_removed_rows() {
        let mut state = SidebarState::default();

        assert!(state.toggle_selected("session_a"));
        assert!(!state.toggle_selected("session_a"));
        state.select_all(["session_a".into(), "session_b".into(), "session_b".into()]);
        assert_eq!(
            state.selected_ids,
            BTreeSet::from(["session_a".into(), "session_b".into()])
        );

        state.reconcile(["session_b".into(), "session_c".into()]);
        assert_eq!(state.selected_ids, BTreeSet::from(["session_b".into()]));
        state.clear_selection();
        assert!(state.selected_ids.is_empty());
    }
}
