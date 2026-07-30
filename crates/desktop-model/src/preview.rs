use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const PREVIEW_MAIN_PANE_ID: &str = "preview-pane-main";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewTarget {
    File {
        path: String,
    },
    Terminal {
        #[serde(alias = "terminalId")]
        terminal_id: String,
    },
    Web {
        #[serde(alias = "webId")]
        web_id: String,
        url: String,
    },
    GitDiff {
        path: String,
        staged: bool,
    },
    GitCommit {
        #[serde(alias = "commitHash")]
        commit_hash: String,
        subject: Option<String>,
        #[serde(alias = "focusPath")]
        focus_path: Option<String>,
        #[serde(alias = "focusRequestId")]
        focus_request_id: Option<u64>,
    },
}

impl PreviewTarget {
    pub fn normalize(mut self) -> Option<Self> {
        match &mut self {
            Self::File { path } | Self::GitDiff { path, .. } => {
                *path = normalized_text(path)?;
            }
            Self::Terminal { terminal_id } => {
                *terminal_id = normalized_text(terminal_id)?;
            }
            Self::Web { web_id, url } => {
                *web_id = normalized_text(web_id)?;
                *url = url.trim().to_string();
            }
            Self::GitCommit {
                commit_hash,
                subject,
                focus_path,
                ..
            } => {
                *commit_hash = normalized_text(commit_hash)?;
                *subject = subject.take().and_then(|value| normalized_text(&value));
                *focus_path = focus_path.take().and_then(|value| normalized_text(&value));
            }
        }
        Some(self)
    }

    pub fn tab_id(&self) -> String {
        match self {
            Self::File { path } => format!("file:{path}"),
            Self::Terminal { terminal_id } => format!("terminal:{terminal_id}"),
            Self::Web { web_id, .. } => format!("web:{web_id}"),
            Self::GitDiff { path, staged } => {
                format!("git:{}:{path}", if *staged { "staged" } else { "unstaged" })
            }
            Self::GitCommit { commit_hash, .. } => format!("git-commit:{commit_hash}"),
        }
    }

    fn moved_path(&self, source: &str, destination: &str) -> Self {
        let move_path = |path: &str| replace_path_prefix(path, source, destination);
        match self {
            Self::File { path } => Self::File {
                path: move_path(path),
            },
            Self::GitDiff { path, staged } => Self::GitDiff {
                path: move_path(path),
                staged: *staged,
            },
            Self::GitCommit {
                commit_hash,
                subject,
                focus_path,
                focus_request_id,
            } => Self::GitCommit {
                commit_hash: commit_hash.clone(),
                subject: subject.clone(),
                focus_path: focus_path.as_deref().map(move_path),
                focus_request_id: *focus_request_id,
            },
            other => other.clone(),
        }
    }

    fn references_path(&self, path: &str) -> bool {
        match self {
            Self::File { path: target } | Self::GitDiff { path: target, .. } => {
                path_is_equal_or_descendant(target, path)
            }
            Self::GitCommit { focus_path, .. } => focus_path
                .as_deref()
                .is_some_and(|target| path_is_equal_or_descendant(target, path)),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTab {
    pub id: String,
    pub target: PreviewTarget,
    pub created_at_ms: i64,
    pub pinned: bool,
    pub temporary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPane {
    pub id: String,
    pub tab_ids: Vec<String>,
    pub active_tab_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewSplitNode {
    Pane {
        pane: PreviewPane,
    },
    Split {
        id: String,
        direction: SplitDirection,
        children: Vec<PreviewSplitNode>,
        sizes: Vec<f32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSplitPosition {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewState {
    pub tabs: BTreeMap<String, PreviewTab>,
    pub root: PreviewSplitNode,
    pub focused_pane_id: String,
    pub fullscreen_tab_id: Option<String>,
    #[serde(default)]
    pub side_preview_tab_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewCloseDisposition {
    Closed,
    Missing,
    Pinned,
    Protected,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            tabs: BTreeMap::new(),
            root: default_root(),
            focused_pane_id: PREVIEW_MAIN_PANE_ID.to_string(),
            fullscreen_tab_id: None,
            side_preview_tab_id: None,
        }
    }
}

impl PreviewState {
    pub fn normalize(&mut self) {
        let mut tabs = BTreeMap::new();
        for (_, mut tab) in std::mem::take(&mut self.tabs) {
            let Some(target) = tab.target.normalize() else {
                continue;
            };
            if !tab.pinned && tab.temporary {
                // Temporary tabs are session-only and never restored from persistence.
                continue;
            }
            let id = target.tab_id();
            tab.id = id.clone();
            tab.target = target;
            tabs.entry(id)
                .and_modify(|current: &mut PreviewTab| {
                    current.pinned |= tab.pinned;
                    current.created_at_ms = current.created_at_ms.min(tab.created_at_ms);
                })
                .or_insert(tab);
        }
        self.tabs = tabs;

        let valid_tabs = self.tabs.keys().cloned().collect::<BTreeSet<_>>();
        let mut seen_tabs = BTreeSet::new();
        let mut seen_panes = BTreeSet::new();
        self.root = normalize_node(
            std::mem::replace(&mut self.root, default_root()),
            &valid_tabs,
            &mut seen_tabs,
            &mut seen_panes,
        )
        .unwrap_or_else(default_root);

        let first_pane = first_pane_id(&self.root)
            .unwrap_or(PREVIEW_MAIN_PANE_ID)
            .to_string();
        let missing = self
            .tabs
            .keys()
            .filter(|id| !seen_tabs.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            update_pane(&mut self.root, &first_pane, &mut |pane| {
                pane.tab_ids.extend(missing.clone());
            });
        }
        order_all_panes(&mut self.root, &self.tabs);

        if !contains_pane(&self.root, &self.focused_pane_id) {
            self.focused_pane_id = first_pane_id(&self.root)
                .unwrap_or(PREVIEW_MAIN_PANE_ID)
                .to_string();
        }
        if self
            .fullscreen_tab_id
            .as_ref()
            .is_some_and(|id| !self.tabs.contains_key(id))
        {
            self.fullscreen_tab_id = None;
        }
        if self
            .side_preview_tab_id
            .as_ref()
            .is_some_and(|id| !self.tabs.contains_key(id))
        {
            self.side_preview_tab_id = None;
        }
    }

    pub fn preview_file(
        &mut self,
        path: impl Into<String>,
        pane_id: Option<&str>,
        created_at_ms: i64,
    ) -> Option<String> {
        let target = PreviewTarget::File { path: path.into() }.normalize()?;
        let id = target.tab_id();
        let pane_id = pane_id
            .filter(|id| contains_pane(&self.root, id))
            .unwrap_or(&self.focused_pane_id)
            .to_string();
        let temporary = self
            .tabs
            .values()
            .filter(|tab| {
                tab.id != id && tab.temporary && matches!(tab.target, PreviewTarget::File { .. })
            })
            .map(|tab| tab.id.clone())
            .collect::<Vec<_>>();
        for tab_id in temporary {
            self.tabs.remove(&tab_id);
            remove_tab_from_panes(&mut self.root, &tab_id, None);
        }
        self.tabs.entry(id.clone()).or_insert_with(|| PreviewTab {
            id: id.clone(),
            target,
            created_at_ms,
            pinned: false,
            temporary: true,
        });
        remove_tab_from_panes(&mut self.root, &id, Some(&pane_id));
        update_pane(&mut self.root, &pane_id, &mut |pane| {
            if !pane.tab_ids.contains(&id) {
                pane.tab_ids.push(id.clone());
            }
            pane.active_tab_id = Some(id.clone());
        });
        self.focused_pane_id = pane_id;
        self.prune_empty_panes();
        self.normalize_live();
        Some(id)
    }

    pub fn open(
        &mut self,
        target: PreviewTarget,
        pane_id: Option<&str>,
        created_at_ms: i64,
    ) -> Option<String> {
        let target = target.normalize()?;
        let id = target.tab_id();
        self.tabs
            .entry(id.clone())
            .and_modify(|tab| {
                tab.target = target.clone();
                tab.temporary = false;
            })
            .or_insert_with(|| PreviewTab {
                id: id.clone(),
                target,
                created_at_ms,
                pinned: false,
                temporary: false,
            });
        let pane_id = pane_id
            .filter(|id| contains_pane(&self.root, id))
            .unwrap_or(&self.focused_pane_id)
            .to_string();
        remove_tab_from_panes(&mut self.root, &id, Some(&pane_id));
        update_pane(&mut self.root, &pane_id, &mut |pane| {
            if !pane.tab_ids.contains(&id) {
                pane.tab_ids.push(id.clone());
            }
            pane.active_tab_id = Some(id.clone());
        });
        self.focused_pane_id = pane_id;
        self.prune_empty_panes();
        self.normalize_live();
        Some(id)
    }

    pub fn close(&mut self, tab_id: &str, force: bool) -> bool {
        self.close_guarded(tab_id, force, &BTreeSet::new()) == PreviewCloseDisposition::Closed
    }

    pub fn close_guarded(
        &mut self,
        tab_id: &str,
        force: bool,
        protected_tab_ids: &BTreeSet<String>,
    ) -> PreviewCloseDisposition {
        let Some(tab) = self.tabs.get(tab_id) else {
            return PreviewCloseDisposition::Missing;
        };
        if tab.pinned && !force {
            return PreviewCloseDisposition::Pinned;
        }
        if protected_tab_ids.contains(tab_id) && !force {
            return PreviewCloseDisposition::Protected;
        }
        self.tabs.remove(tab_id);
        remove_tab_from_panes(&mut self.root, tab_id, None);
        self.prune_empty_panes();
        self.normalize_live();
        PreviewCloseDisposition::Closed
    }

    pub fn close_other_tabs(
        &mut self,
        tab_id: &str,
        pane_id: &str,
        protected_tab_ids: &BTreeSet<String>,
    ) -> Vec<(String, PreviewCloseDisposition)> {
        let Some(pane) = pane_by_id(&self.root, pane_id) else {
            return Vec::new();
        };
        if !pane.tab_ids.iter().any(|id| id == tab_id) {
            return Vec::new();
        }
        let candidates = pane
            .tab_ids
            .iter()
            .filter(|id| id.as_str() != tab_id)
            .cloned()
            .collect::<Vec<_>>();
        candidates
            .into_iter()
            .map(|id| {
                let disposition = self.close_guarded(&id, false, protected_tab_ids);
                (id, disposition)
            })
            .collect()
    }

    pub fn close_all_tabs(
        &mut self,
        pane_id: &str,
        protected_tab_ids: &BTreeSet<String>,
    ) -> Vec<(String, PreviewCloseDisposition)> {
        let Some(pane) = pane_by_id(&self.root, pane_id) else {
            return Vec::new();
        };
        let candidates = pane.tab_ids.clone();
        candidates
            .into_iter()
            .map(|id| {
                let disposition = self.close_guarded(&id, false, protected_tab_ids);
                (id, disposition)
            })
            .collect()
    }

    pub fn toggle_pin(&mut self, tab_id: &str) -> bool {
        let Some(tab) = self.tabs.get_mut(tab_id) else {
            return false;
        };
        let next_pinned = !tab.pinned;
        tab.pinned = next_pinned;
        tab.temporary = false;
        if next_pinned {
            move_tab_to_pane_start(&mut self.root, tab_id);
        }
        order_all_panes(&mut self.root, &self.tabs);
        true
    }

    pub fn focus(&mut self, tab_id: &str) -> bool {
        let Some(pane_id) = pane_containing_tab(&self.root, tab_id).map(str::to_string) else {
            return false;
        };
        update_pane(&mut self.root, &pane_id, &mut |pane| {
            pane.active_tab_id = Some(tab_id.to_string());
        });
        self.focused_pane_id = pane_id;
        true
    }

    pub fn move_to_pane(&mut self, tab_id: &str, pane_id: &str) -> bool {
        if !self.tabs.contains_key(tab_id) || !contains_pane(&self.root, pane_id) {
            return false;
        }
        remove_tab_from_panes(&mut self.root, tab_id, Some(pane_id));
        update_pane(&mut self.root, pane_id, &mut |pane| {
            if !pane.tab_ids.iter().any(|id| id == tab_id) {
                pane.tab_ids.push(tab_id.to_string());
            }
            pane.active_tab_id = Some(tab_id.to_string());
        });
        self.focused_pane_id = pane_id.to_string();
        self.prune_empty_panes();
        self.normalize_live();
        true
    }

    pub fn reorder_pane_tabs(&mut self, pane_id: &str, requested: &[String]) -> bool {
        let Some(current) = pane_by_id(&self.root, pane_id).map(|pane| pane.tab_ids.clone()) else {
            return false;
        };
        let mut seen = BTreeSet::new();
        let mut next = requested
            .iter()
            .filter(|id| current.contains(id) && seen.insert((*id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        next.extend(
            current
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned(),
        );
        if next == current {
            return false;
        }
        update_pane(&mut self.root, pane_id, &mut |pane| {
            pane.tab_ids = next.clone()
        });
        order_all_panes(&mut self.root, &self.tabs);
        true
    }

    pub fn focus_pane(&mut self, pane_id: &str) -> bool {
        if !contains_pane(&self.root, pane_id) {
            return false;
        }
        self.focused_pane_id = pane_id.to_string();
        true
    }

    pub fn resize_split(&mut self, split_id: &str, sizes: Vec<f32>) -> bool {
        resize_split_node(&mut self.root, split_id, sizes)
    }

    pub fn split(
        &mut self,
        tab_id: &str,
        target_pane_id: &str,
        position: PreviewSplitPosition,
        new_pane_id: &str,
        new_split_id: &str,
    ) -> bool {
        if !self.tabs.contains_key(tab_id)
            || !contains_pane(&self.root, target_pane_id)
            || normalized_text(new_pane_id).is_none()
            || normalized_text(new_split_id).is_none()
            || contains_pane(&self.root, new_pane_id)
        {
            return false;
        }
        remove_tab_from_panes(&mut self.root, tab_id, Some(target_pane_id));
        let created = split_pane(
            &mut self.root,
            target_pane_id,
            tab_id,
            position,
            new_pane_id,
            new_split_id,
        );
        if created {
            self.focused_pane_id = new_pane_id.to_string();
            self.normalize_live();
        }
        created
    }

    pub fn split_pruned(
        &mut self,
        tab_id: &str,
        target_pane_id: &str,
        position: PreviewSplitPosition,
        new_pane_id: &str,
        new_split_id: &str,
    ) -> bool {
        if !self.split(tab_id, target_pane_id, position, new_pane_id, new_split_id) {
            return false;
        }
        self.prune_empty_panes();
        self.normalize_live();
        true
    }

    pub fn move_path(&mut self, source: &str, destination: &str) {
        let Some(source) = normalized_text(source) else {
            return;
        };
        let Some(destination) = normalized_text(destination) else {
            return;
        };
        let mut replacements = BTreeMap::new();
        let mut tabs = BTreeMap::new();
        for (_, mut tab) in std::mem::take(&mut self.tabs) {
            tab.target = tab.target.moved_path(&source, &destination);
            let next_id = tab.target.tab_id();
            if next_id != tab.id {
                replacements.insert(tab.id.clone(), next_id.clone());
            }
            tab.id = next_id.clone();
            tabs.insert(next_id, tab);
        }
        self.tabs = tabs;
        replace_tab_ids(&mut self.root, &replacements);
        if let Some(fullscreen) = self.fullscreen_tab_id.take() {
            self.fullscreen_tab_id =
                Some(replacements.get(&fullscreen).cloned().unwrap_or(fullscreen));
        }
        self.normalize_live();
    }

    pub fn delete_path(&mut self, path: &str) {
        self.delete_path_guarded(path, &BTreeSet::new());
    }

    pub fn delete_path_guarded(
        &mut self,
        path: &str,
        protected_tab_ids: &BTreeSet<String>,
    ) -> Vec<String> {
        let Some(path) = normalized_text(path) else {
            return Vec::new();
        };
        let removed = self
            .tabs
            .iter()
            .filter(|(_, tab)| tab.target.references_path(&path))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let mut protected = Vec::new();
        for id in removed {
            if protected_tab_ids.contains(&id) {
                protected.push(id);
                continue;
            }
            self.tabs.remove(&id);
            remove_tab_from_panes(&mut self.root, &id, None);
        }
        self.prune_empty_panes();
        self.normalize_live();
        protected
    }

    pub fn set_fullscreen(&mut self, tab_id: Option<&str>) -> bool {
        if let Some(tab_id) = tab_id
            && !self.tabs.contains_key(tab_id)
        {
            return false;
        }
        self.fullscreen_tab_id = tab_id.map(str::to_string);
        true
    }

    pub fn set_side_preview(&mut self, tab_id: Option<&str>) -> bool {
        if let Some(tab_id) = tab_id
            && !self.tabs.contains_key(tab_id)
        {
            return false;
        }
        self.side_preview_tab_id = tab_id.map(str::to_string);
        true
    }

    pub fn active_tab_id(&self, pane_id: &str) -> Option<&str> {
        pane_by_id(&self.root, pane_id)?.active_tab_id.as_deref()
    }

    pub fn pane_ids(&self) -> Vec<&str> {
        let mut ids = Vec::new();
        collect_pane_ids(&self.root, &mut ids);
        ids
    }

    fn normalize_live(&mut self) {
        normalize_panes_in_place(&mut self.root, &self.tabs);
        order_all_panes(&mut self.root, &self.tabs);
        if !contains_pane(&self.root, &self.focused_pane_id) {
            self.focused_pane_id = first_pane_id(&self.root)
                .unwrap_or(PREVIEW_MAIN_PANE_ID)
                .to_string();
        }
        if self
            .fullscreen_tab_id
            .as_ref()
            .is_some_and(|id| !self.tabs.contains_key(id))
        {
            self.fullscreen_tab_id = None;
        }
        if self
            .side_preview_tab_id
            .as_ref()
            .is_some_and(|id| !self.tabs.contains_key(id))
        {
            self.side_preview_tab_id = None;
        }
    }

    fn prune_empty_panes(&mut self) {
        self.root = prune_empty(std::mem::replace(&mut self.root, default_root()))
            .unwrap_or_else(default_root);
    }
}

fn normalized_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn default_root() -> PreviewSplitNode {
    PreviewSplitNode::Pane {
        pane: PreviewPane {
            id: PREVIEW_MAIN_PANE_ID.to_string(),
            tab_ids: Vec::new(),
            active_tab_id: None,
        },
    }
}

fn path_is_equal_or_descendant(candidate: &str, ancestor: &str) -> bool {
    candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
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

fn normalize_node(
    node: PreviewSplitNode,
    valid_tabs: &BTreeSet<String>,
    seen_tabs: &mut BTreeSet<String>,
    seen_panes: &mut BTreeSet<String>,
) -> Option<PreviewSplitNode> {
    match node {
        PreviewSplitNode::Pane { mut pane } => {
            pane.id = normalized_text(&pane.id)?;
            if !seen_panes.insert(pane.id.clone()) {
                return None;
            }
            pane.tab_ids
                .retain(|id| valid_tabs.contains(id) && seen_tabs.insert(id.clone()));
            normalize_pane(&mut pane);
            Some(PreviewSplitNode::Pane { pane })
        }
        PreviewSplitNode::Split {
            id,
            direction,
            children,
            sizes,
        } => {
            let id = normalized_text(&id)?;
            let children = children
                .into_iter()
                .filter_map(|child| normalize_node(child, valid_tabs, seen_tabs, seen_panes))
                .collect::<Vec<_>>();
            match children.len() {
                0 => None,
                1 => children.into_iter().next(),
                count => Some(PreviewSplitNode::Split {
                    id,
                    direction,
                    sizes: normalize_sizes(sizes, count),
                    children,
                }),
            }
        }
    }
}

fn normalize_sizes(sizes: Vec<f32>, count: usize) -> Vec<f32> {
    if sizes.len() == count && sizes.iter().all(|size| size.is_finite() && *size > 0.0) {
        let total = sizes.iter().sum::<f32>();
        if total > 0.0 {
            return sizes.into_iter().map(|size| size / total).collect();
        }
    }
    vec![1.0 / count as f32; count]
}

fn normalize_pane(pane: &mut PreviewPane) {
    let mut seen = BTreeSet::new();
    pane.tab_ids.retain(|id| seen.insert(id.clone()));
    if pane
        .active_tab_id
        .as_ref()
        .is_none_or(|id| !pane.tab_ids.contains(id))
    {
        pane.active_tab_id = pane.tab_ids.first().cloned();
    }
}

fn normalize_panes_in_place(node: &mut PreviewSplitNode, tabs: &BTreeMap<String, PreviewTab>) {
    match node {
        PreviewSplitNode::Pane { pane } => {
            pane.tab_ids.retain(|id| tabs.contains_key(id));
            normalize_pane(pane);
        }
        PreviewSplitNode::Split {
            children, sizes, ..
        } => {
            for child in children.iter_mut() {
                normalize_panes_in_place(child, tabs);
            }
            *sizes = normalize_sizes(std::mem::take(sizes), children.len());
        }
    }
}

fn order_all_panes(node: &mut PreviewSplitNode, tabs: &BTreeMap<String, PreviewTab>) {
    match node {
        PreviewSplitNode::Pane { pane } => {
            pane.tab_ids
                .sort_by_key(|id| !tabs.get(id).is_some_and(|tab| tab.pinned));
            normalize_pane(pane);
        }
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                order_all_panes(child, tabs);
            }
        }
    }
}

fn move_tab_to_pane_start(node: &mut PreviewSplitNode, tab_id: &str) {
    match node {
        PreviewSplitNode::Pane { pane } => {
            if let Some(index) = pane.tab_ids.iter().position(|id| id == tab_id) {
                let tab_id = pane.tab_ids.remove(index);
                pane.tab_ids.insert(0, tab_id);
                normalize_pane(pane);
            }
        }
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                move_tab_to_pane_start(child, tab_id);
            }
        }
    }
}

fn first_pane_id(node: &PreviewSplitNode) -> Option<&str> {
    match node {
        PreviewSplitNode::Pane { pane } => Some(&pane.id),
        PreviewSplitNode::Split { children, .. } => children.iter().find_map(first_pane_id),
    }
}

fn contains_pane(node: &PreviewSplitNode, pane_id: &str) -> bool {
    match node {
        PreviewSplitNode::Pane { pane } => pane.id == pane_id,
        PreviewSplitNode::Split { children, .. } => {
            children.iter().any(|child| contains_pane(child, pane_id))
        }
    }
}

fn pane_by_id<'a>(node: &'a PreviewSplitNode, pane_id: &str) -> Option<&'a PreviewPane> {
    match node {
        PreviewSplitNode::Pane { pane } => (pane.id == pane_id).then_some(pane),
        PreviewSplitNode::Split { children, .. } => {
            children.iter().find_map(|child| pane_by_id(child, pane_id))
        }
    }
}

fn collect_pane_ids<'a>(node: &'a PreviewSplitNode, ids: &mut Vec<&'a str>) {
    match node {
        PreviewSplitNode::Pane { pane } => ids.push(&pane.id),
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                collect_pane_ids(child, ids);
            }
        }
    }
}

fn resize_split_node(node: &mut PreviewSplitNode, split_id: &str, sizes: Vec<f32>) -> bool {
    match node {
        PreviewSplitNode::Pane { .. } => false,
        PreviewSplitNode::Split {
            id,
            children,
            sizes: current,
            ..
        } if id == split_id => {
            let normalized = normalize_sizes(sizes, children.len());
            if *current == normalized {
                return false;
            }
            *current = normalized;
            true
        }
        PreviewSplitNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| resize_split_node(child, split_id, sizes.clone())),
    }
}

fn pane_containing_tab<'a>(node: &'a PreviewSplitNode, tab_id: &str) -> Option<&'a str> {
    match node {
        PreviewSplitNode::Pane { pane } => pane
            .tab_ids
            .iter()
            .any(|id| id == tab_id)
            .then_some(pane.id.as_str()),
        PreviewSplitNode::Split { children, .. } => children
            .iter()
            .find_map(|child| pane_containing_tab(child, tab_id)),
    }
}

fn update_pane(
    node: &mut PreviewSplitNode,
    pane_id: &str,
    update: &mut impl FnMut(&mut PreviewPane),
) {
    match node {
        PreviewSplitNode::Pane { pane } if pane.id == pane_id => update(pane),
        PreviewSplitNode::Pane { .. } => {}
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                update_pane(child, pane_id, update);
            }
        }
    }
}

fn remove_tab_from_panes(node: &mut PreviewSplitNode, tab_id: &str, except_pane: Option<&str>) {
    match node {
        PreviewSplitNode::Pane { pane } => {
            if except_pane != Some(pane.id.as_str()) {
                pane.tab_ids.retain(|id| id != tab_id);
                normalize_pane(pane);
            }
        }
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                remove_tab_from_panes(child, tab_id, except_pane);
            }
        }
    }
}

fn replace_tab_ids(node: &mut PreviewSplitNode, replacements: &BTreeMap<String, String>) {
    match node {
        PreviewSplitNode::Pane { pane } => {
            for id in &mut pane.tab_ids {
                if let Some(replacement) = replacements.get(id) {
                    *id = replacement.clone();
                }
            }
            if let Some(active) = pane.active_tab_id.as_mut()
                && let Some(replacement) = replacements.get(active)
            {
                *active = replacement.clone();
            }
            normalize_pane(pane);
        }
        PreviewSplitNode::Split { children, .. } => {
            for child in children {
                replace_tab_ids(child, replacements);
            }
        }
    }
}

fn prune_empty(node: PreviewSplitNode) -> Option<PreviewSplitNode> {
    match node {
        PreviewSplitNode::Pane { pane } => {
            (!pane.tab_ids.is_empty()).then_some(PreviewSplitNode::Pane { pane })
        }
        PreviewSplitNode::Split {
            id,
            direction,
            children,
            ..
        } => {
            let children = children
                .into_iter()
                .filter_map(prune_empty)
                .collect::<Vec<_>>();
            match children.len() {
                0 => None,
                1 => children.into_iter().next(),
                count => Some(PreviewSplitNode::Split {
                    id,
                    direction,
                    sizes: vec![1.0 / count as f32; count],
                    children,
                }),
            }
        }
    }
}

fn split_pane(
    node: &mut PreviewSplitNode,
    target_pane_id: &str,
    tab_id: &str,
    position: PreviewSplitPosition,
    new_pane_id: &str,
    new_split_id: &str,
) -> bool {
    match node {
        PreviewSplitNode::Pane { pane } if pane.id == target_pane_id => {
            pane.tab_ids.retain(|id| id != tab_id);
            normalize_pane(pane);
            let source = node.clone();
            let destination = PreviewSplitNode::Pane {
                pane: PreviewPane {
                    id: new_pane_id.to_string(),
                    tab_ids: vec![tab_id.to_string()],
                    active_tab_id: Some(tab_id.to_string()),
                },
            };
            let destination_first = matches!(
                position,
                PreviewSplitPosition::Left | PreviewSplitPosition::Top
            );
            let direction = if matches!(
                position,
                PreviewSplitPosition::Left | PreviewSplitPosition::Right
            ) {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            *node = PreviewSplitNode::Split {
                id: new_split_id.to_string(),
                direction,
                children: if destination_first {
                    vec![destination, source]
                } else {
                    vec![source, destination]
                },
                sizes: vec![0.5, 0.5],
            };
            true
        }
        PreviewSplitNode::Pane { .. } => false,
        PreviewSplitNode::Split { children, .. } => children.iter_mut().any(|child| {
            split_pane(
                child,
                target_pane_id,
                tab_id,
                position,
                new_pane_id,
                new_split_id,
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> PreviewTarget {
        PreviewTarget::File {
            path: path.to_string(),
        }
    }

    #[test]
    fn tab_exists_in_exactly_one_pane_and_pinned_tabs_sort_first() {
        let mut state = PreviewState::default();
        let first = state.open(file("src/a.rs"), None, 10).unwrap();
        let second = state.open(file("src/b.rs"), None, 20).unwrap();
        assert!(state.toggle_pin(&second));
        assert!(state.split(
            &first,
            PREVIEW_MAIN_PANE_ID,
            PreviewSplitPosition::Right,
            "preview-pane-two",
            "preview-split-one",
        ));
        assert!(state.move_to_pane(&second, "preview-pane-two"));

        let pane_memberships = [PREVIEW_MAIN_PANE_ID, "preview-pane-two"]
            .into_iter()
            .filter(|pane_id| {
                let mut found = false;
                update_pane(&mut state.root, pane_id, &mut |pane| {
                    found = pane.tab_ids.contains(&second);
                });
                found
            })
            .count();
        assert_eq!(pane_memberships, 1);
        assert_eq!(
            pane_containing_tab(&state.root, &second),
            Some("preview-pane-two")
        );
    }

    #[test]
    fn moving_the_last_tab_prunes_the_empty_source_pane() {
        let mut state = PreviewState::default();
        let tab_id = state.open(file("src/a.rs"), None, 10).unwrap();
        assert!(state.split(
            &tab_id,
            PREVIEW_MAIN_PANE_ID,
            PreviewSplitPosition::Right,
            "preview-pane-two",
            "preview-split-one",
        ));

        assert!(state.move_to_pane(&tab_id, PREVIEW_MAIN_PANE_ID));
        assert_eq!(state.pane_ids(), vec![PREVIEW_MAIN_PANE_ID]);
        assert_eq!(
            state.active_tab_id(PREVIEW_MAIN_PANE_ID),
            Some(tab_id.as_str())
        );
        assert_eq!(state.focused_pane_id, PREVIEW_MAIN_PANE_ID);
    }

    #[test]
    fn opening_an_existing_tab_in_another_pane_prunes_its_empty_source() {
        let mut state = PreviewState::default();
        let first = state.open(file("src/a.rs"), None, 10).unwrap();
        let second = state.open(file("src/b.rs"), None, 20).unwrap();
        assert!(state.split(
            &first,
            PREVIEW_MAIN_PANE_ID,
            PreviewSplitPosition::Right,
            "preview-pane-two",
            "preview-split-one",
        ));

        assert_eq!(
            state.open(file("src/b.rs"), Some("preview-pane-two"), 30),
            Some(second.clone())
        );
        assert_eq!(state.pane_ids(), vec!["preview-pane-two"]);
        assert_eq!(
            state.active_tab_id("preview-pane-two"),
            Some(second.as_str())
        );
    }

    #[test]
    fn replacing_a_temporary_tab_keeps_its_target_pane() {
        let mut state = PreviewState::default();
        let persistent = state.open(file("src/persistent.rs"), None, 10).unwrap();
        assert!(state.split(
            &persistent,
            PREVIEW_MAIN_PANE_ID,
            PreviewSplitPosition::Right,
            "preview-pane-two",
            "preview-split-one",
        ));
        let old = state
            .preview_file("src/old.rs", Some(PREVIEW_MAIN_PANE_ID), 20)
            .unwrap();

        let new = state
            .preview_file("src/new.rs", Some(PREVIEW_MAIN_PANE_ID), 30)
            .unwrap();
        assert!(!state.tabs.contains_key(&old));
        assert_eq!(
            state.pane_ids(),
            vec![PREVIEW_MAIN_PANE_ID, "preview-pane-two"]
        );
        assert_eq!(
            state.active_tab_id(PREVIEW_MAIN_PANE_ID),
            Some(new.as_str())
        );
    }

    #[test]
    fn pinning_moves_the_newly_pinned_tab_before_existing_pinned_tabs() {
        let mut state = PreviewState::default();
        let first = state.open(file("src/a.rs"), None, 10).unwrap();
        let second = state.open(file("src/b.rs"), None, 20).unwrap();
        let third = state.open(file("src/c.rs"), None, 30).unwrap();

        assert!(state.toggle_pin(&first));
        assert!(state.toggle_pin(&third));
        assert_eq!(
            pane_by_id(&state.root, PREVIEW_MAIN_PANE_ID)
                .expect("main pane exists")
                .tab_ids,
            vec![third, first, second]
        );
    }

    #[test]
    fn pruned_split_removes_the_empty_source_pane() {
        let mut state = PreviewState::default();
        let tab_id = state.open(file("src/a.rs"), None, 10).unwrap();

        assert!(state.split_pruned(
            &tab_id,
            PREVIEW_MAIN_PANE_ID,
            PreviewSplitPosition::Right,
            "preview-pane-two",
            "preview-split-one",
        ));
        assert_eq!(state.pane_ids(), vec!["preview-pane-two"]);
        assert_eq!(state.focused_pane_id, "preview-pane-two");
        assert_eq!(
            state.active_tab_id("preview-pane-two"),
            Some(tab_id.as_str())
        );
    }

    #[test]
    fn rename_updates_tabs_and_fullscreen_while_delete_cleans_descendants() {
        let mut state = PreviewState::default();
        let file_id = state.open(file("src/old/lib.rs"), None, 10).unwrap();
        let diff_id = state
            .open(
                PreviewTarget::GitDiff {
                    path: "src/old/main.rs".into(),
                    staged: false,
                },
                None,
                20,
            )
            .unwrap();
        assert!(state.set_fullscreen(Some(&file_id)));

        state.move_path("src/old", "src/new");
        assert!(state.tabs.contains_key("file:src/new/lib.rs"));
        assert!(state.tabs.contains_key("git:unstaged:src/new/main.rs"));
        assert_eq!(
            state.fullscreen_tab_id.as_deref(),
            Some("file:src/new/lib.rs")
        );
        assert!(!state.tabs.contains_key(&diff_id));

        state.delete_path("src/new");
        assert!(state.tabs.is_empty());
        assert_eq!(state.fullscreen_tab_id, None);
        assert_eq!(first_pane_id(&state.root), Some(PREVIEW_MAIN_PANE_ID));
    }

    #[test]
    fn persisted_normalization_drops_temporary_and_duplicate_membership() {
        let mut state = PreviewState::default();
        state.tabs.insert(
            "wrong".into(),
            PreviewTab {
                id: "wrong".into(),
                target: file(" a.rs "),
                created_at_ms: 1,
                pinned: false,
                temporary: false,
            },
        );
        state.tabs.insert(
            "temp".into(),
            PreviewTab {
                id: "temp".into(),
                target: file("temp.rs"),
                created_at_ms: 2,
                pinned: false,
                temporary: true,
            },
        );
        state.root = PreviewSplitNode::Split {
            id: "split".into(),
            direction: SplitDirection::Horizontal,
            children: vec![
                PreviewSplitNode::Pane {
                    pane: PreviewPane {
                        id: "one".into(),
                        tab_ids: vec!["file:a.rs".into()],
                        active_tab_id: Some("missing".into()),
                    },
                },
                PreviewSplitNode::Pane {
                    pane: PreviewPane {
                        id: "two".into(),
                        tab_ids: vec!["file:a.rs".into()],
                        active_tab_id: None,
                    },
                },
            ],
            sizes: vec![99.0],
        };

        state.normalize();
        assert_eq!(state.tabs.keys().collect::<Vec<_>>(), vec!["file:a.rs"]);
        assert_eq!(pane_containing_tab(&state.root, "file:a.rs"), Some("one"));
    }
}
