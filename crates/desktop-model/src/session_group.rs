//! Session groups: several Agent sessions of one workspace shown together.
//!
//! A session group is a sidebar organization unit that also owns a multi-pane
//! workspace. The group is *not* a preview multi-tab window: the preview panel
//! groups files, Git diffs and terminals, while a session group groups Agent
//! conversations of a single Worktree and gives them one shared right-hand
//! column.
//!
//! This module owns the framework-neutral state only. Identifiers and clock
//! values are injected by the caller, exactly like the rest of this crate.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::SplitDirection;

/// How many groups one sidebar may hold.
pub const SESSION_GROUP_LIMIT: usize = 500;
/// How many sessions one group may hold.
pub const SESSION_GROUP_MEMBER_LIMIT: usize = 64;
/// Group names are bounded like folder names.
pub const SESSION_GROUP_NAME_MAX_CHARS: usize = 160;
/// How many panes one group workspace may split into.
pub const SESSION_GROUP_PANE_LIMIT: usize = 8;
/// How many panes of a group render a full live conversation.
///
/// Panes above this bound keep their place in the layout and still follow
/// authoritative session state, but they render a static projection instead of
/// holding a live timeline, composer and scroll state. The bound is what keeps
/// a wide split from multiplying the per-session resident cost without limit.
pub const SESSION_GROUP_LIVE_PANE_LIMIT: usize = 4;
/// Split shares are stored as integer per-mille so the whole group stays
/// `Eq + Hash` and no persisted document can carry a NaN share.
pub const SESSION_GROUP_SPLIT_SCALE: u32 = 1_000;
/// One pane never shrinks below this share of a split.
const SESSION_GROUP_MIN_SPLIT_SHARE: u32 = 50;
const SESSION_GROUP_MAX_SPLIT_SHARE: u32 = 950;

/// Converts a stored per-mille share into the `0.0..=1.0` ratio the layout
/// renderer wants.
pub fn split_share(size: u16) -> f32 {
    f32::from(size) / SESSION_GROUP_SPLIT_SCALE as f32
}

/// The default pane id of a freshly created group workspace.
pub const SESSION_GROUP_MAIN_PANE_ID: &str = "session-group-pane-main";

/// One pane of a group workspace: the sessions stacked in this pane and the one
/// on top.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroupPane {
    pub id: String,
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub active_session_id: Option<String>,
}

impl SessionGroupPane {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_ids: Vec::new(),
            active_session_id: None,
        }
    }

    /// Drops references to sessions the pane no longer holds and repairs the
    /// active selection so it always names a session the pane still shows.
    fn normalize(&mut self, members: &BTreeSet<String>) {
        let mut seen = BTreeSet::new();
        self.session_ids
            .retain(|session_id| members.contains(session_id) && seen.insert(session_id.clone()));
        self.session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
        let active_is_valid = self
            .active_session_id
            .as_ref()
            .is_some_and(|session_id| self.session_ids.contains(session_id));
        if !active_is_valid {
            self.active_session_id = self.session_ids.first().cloned();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.session_ids.is_empty()
    }
}

/// The pane tree of one group workspace.
///
/// The shape mirrors [`crate::PreviewSplitNode`] because both are split trees,
/// but the payload is a [`SessionGroupPane`] — a session group never shares the
/// preview panel's tab set.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionGroupLayout {
    Pane {
        pane: SessionGroupPane,
    },
    Split {
        id: String,
        direction: SplitDirection,
        children: Vec<SessionGroupLayout>,
        #[serde(default)]
        sizes: Vec<u16>,
    },
}

impl Default for SessionGroupLayout {
    fn default() -> Self {
        Self::Pane {
            pane: SessionGroupPane::new(SESSION_GROUP_MAIN_PANE_ID),
        }
    }
}

impl SessionGroupLayout {
    pub fn pane(pane_id: &str) -> Self {
        Self::Pane {
            pane: SessionGroupPane::new(pane_id),
        }
    }

    fn for_each_pane(&self, visit: &mut impl FnMut(&SessionGroupPane)) {
        match self {
            Self::Pane { pane } => visit(pane),
            Self::Split { children, .. } => {
                for child in children {
                    child.for_each_pane(visit);
                }
            }
        }
    }

    fn for_each_pane_mut(&mut self, visit: &mut impl FnMut(&mut SessionGroupPane)) {
        match self {
            Self::Pane { pane } => visit(pane),
            Self::Split { children, .. } => {
                for child in children {
                    child.for_each_pane_mut(visit);
                }
            }
        }
    }

    pub fn pane_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.for_each_pane(&mut |pane| ids.push(pane.id.clone()));
        ids
    }

    pub fn pane_count(&self) -> usize {
        self.pane_ids().len()
    }

    pub fn find_pane(&self, pane_id: &str) -> Option<&SessionGroupPane> {
        match self {
            Self::Pane { pane } => (pane.id == pane_id).then_some(pane),
            Self::Split { children, .. } => {
                children.iter().find_map(|child| child.find_pane(pane_id))
            }
        }
    }

    pub fn contains_pane(&self, pane_id: &str) -> bool {
        self.find_pane(pane_id).is_some()
    }

    /// The pane that currently shows `session_id`, if any.
    pub fn pane_containing_session(&self, session_id: &str) -> Option<String> {
        match self {
            Self::Pane { pane } => pane
                .session_ids
                .iter()
                .any(|id| id == session_id)
                .then(|| pane.id.clone()),
            Self::Split { children, .. } => children
                .iter()
                .find_map(|child| child.pane_containing_session(session_id)),
        }
    }

    pub fn first_pane_id(&self) -> Option<String> {
        match self {
            Self::Pane { pane } => Some(pane.id.clone()),
            Self::Split { children, .. } => children.iter().find_map(Self::first_pane_id),
        }
    }

    /// The ordered session ids across every pane, first pane first.
    pub fn ordered_session_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.for_each_pane(&mut |pane| ids.extend(pane.session_ids.iter().cloned()));
        ids
    }

    fn find_pane_mut(&mut self, pane_id: &str) -> Option<&mut SessionGroupPane> {
        match self {
            Self::Pane { pane } => (pane.id == pane_id).then_some(pane),
            Self::Split { children, .. } => children
                .iter_mut()
                .find_map(|child| child.find_pane_mut(pane_id)),
        }
    }

    /// Rebuilds the pane tree so that it holds exactly `members`, each in
    /// exactly one pane, and drops panes that lost every session.
    ///
    /// The first pane keeps the caller's `preferred_first_pane_id` so a layout
    /// that survives a membership change keeps its identity; the remaining
    /// panes are re-filled in tree order.
    fn reconcile_members(&mut self, members: &[String], preferred_first_pane_id: &str) {
        let member_set = members.iter().cloned().collect::<BTreeSet<_>>();
        // A pane the user opened on purpose starts empty and must survive
        // normalization; only panes that *became* empty because their sessions
        // left the group are dropped.
        let already_empty = self.empty_pane_ids();
        self.for_each_pane_mut(&mut |pane| pane.normalize(&member_set));

        // Sessions the layout forgot are appended to the first pane in member
        // order, so adding a session never reshuffles the visible panes.
        let mut placed = self
            .ordered_session_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let missing = members
            .iter()
            .filter(|session_id| placed.insert((*session_id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let first_id = self
                .first_pane_id()
                .unwrap_or_else(|| preferred_first_pane_id.to_string());
            if let Some(pane) = self.find_pane_mut(&first_id) {
                pane.session_ids.extend(missing);
                pane.session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
                if pane.active_session_id.is_none() {
                    pane.active_session_id = pane.session_ids.first().cloned();
                }
            }
        }

        self.prune_empty_panes_preserving(&already_empty);
        if !self.has_any_pane() {
            *self = Self::Pane {
                pane: SessionGroupPane::new(preferred_first_pane_id),
            };
            if let Some(pane) = self.find_pane_mut(preferred_first_pane_id) {
                pane.session_ids = members.to_vec();
                pane.session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
                pane.active_session_id = pane.session_ids.first().cloned();
            }
        }
    }

    fn has_any_pane(&self) -> bool {
        match self {
            Self::Pane { .. } => true,
            Self::Split { children, .. } => !children.is_empty(),
        }
    }

    /// The ids of every pane that currently holds no session.
    fn empty_pane_ids(&self) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        self.for_each_pane(&mut |pane| {
            if pane.is_empty() {
                ids.insert(pane.id.clone());
            }
        });
        ids
    }

    /// Removes panes that hold no sessions and collapses single-child splits.
    fn prune_empty_panes(&mut self) {
        self.prune_empty_panes_preserving(&BTreeSet::new());
    }

    /// Like [`Self::prune_empty_panes`], but keeps the panes in `keep`.
    ///
    /// `keep` names the panes that were already empty before the operation, so
    /// a pane the user deliberately opened is not mistaken for one whose
    /// sessions left the group.
    fn prune_empty_panes_preserving(&mut self, keep: &BTreeSet<String>) {
        match self {
            Self::Pane { .. } => {}
            Self::Split { children, .. } => {
                for child in children.iter_mut() {
                    child.prune_empty_panes();
                }
                children.retain(|child| match child {
                    Self::Pane { pane } => !pane.is_empty() || keep.contains(&pane.id),
                    Self::Split { .. } => true,
                });
                // A split that lost every child is itself empty.
                if children.is_empty() {
                    return;
                }
            }
        }
        // Collapsing has to happen from the outside in, so a single-child split
        // is replaced by its child rather than left as a one-pane split.
        if let Self::Split {
            children, sizes, ..
        } = self
            && children.len() == 1
        {
            let only = children.remove(0);
            let _ = sizes;
            *self = only;
        }
    }

    fn normalize_sizes(&mut self) {
        match self {
            Self::Pane { .. } => {}
            Self::Split {
                children, sizes, ..
            } => {
                for child in children.iter_mut() {
                    child.normalize_sizes();
                }
                let count = children.len();
                sizes.truncate(count);
                while sizes.len() < count {
                    sizes.push(1);
                }
                let total = sizes.iter().map(|size| u64::from(*size)).sum::<u64>();
                let scale = u64::from(SESSION_GROUP_SPLIT_SCALE as u16);
                let normalized = sizes
                    .iter()
                    .map(|size| (u64::from(*size) * scale).checked_div(total))
                    .collect::<Option<Vec<u64>>>();
                if let Some(normalized) = normalized {
                    for (size, value) in sizes.iter_mut().zip(normalized) {
                        *size = value as u16;
                    }
                } else {
                    // No usable share at all: fall back to an even split rather
                    // than leaving every pane at a degenerate weight.
                    let even = scale / (count.max(1) as u64);
                    sizes.clear();
                    sizes.resize(count, even as u16);
                }
                for size in sizes.iter_mut() {
                    *size = (*size).clamp(
                        SESSION_GROUP_MIN_SPLIT_SHARE as u16,
                        SESSION_GROUP_MAX_SPLIT_SHARE as u16,
                    );
                }
                // Clamping can move the total away from the scale, so the
                // largest pane absorbs the remainder. That keeps the shares
                // summing to the scale without ever leaving the usable band.
                let clamped_total = sizes.iter().map(|size| u64::from(*size)).sum::<u64>();
                let scale = u64::from(SESSION_GROUP_SPLIT_SCALE as u16);
                if clamped_total != scale
                    && let Some((largest_index, _)) =
                        sizes.iter().enumerate().max_by_key(|(_, size)| **size)
                {
                    let largest = u64::from(sizes[largest_index]);
                    let adjusted = largest + scale;
                    let adjusted = adjusted.saturating_sub(clamped_total);
                    sizes[largest_index] = adjusted.clamp(
                        u64::from(SESSION_GROUP_MIN_SPLIT_SHARE as u16),
                        u64::from(SESSION_GROUP_MAX_SPLIT_SHARE as u16),
                    ) as u16;
                }
            }
        }
    }

    /// Splits `pane_id` in `direction`, moving `session_id` into the new pane.
    ///
    /// The session may live in `pane_id` or in any other pane; the new pane is
    /// always spliced next to `pane_id`, which is what lets a workspace grow
    /// past two panes. Splitting a pane that holds nothing but `session_id`
    /// would leave it empty, so the caller opens an empty pane instead — see
    /// [`Self::split_open_pane`].
    ///
    /// Returns the new pane id, or `None` when the layout cannot split further.
    pub fn split_with_session(
        &mut self,
        pane_id: &str,
        session_id: &str,
        direction: SplitDirection,
        new_pane_id: impl Into<String>,
        new_split_id: impl Into<String>,
        position: SessionGroupSplitPosition,
    ) -> Option<String> {
        if self.pane_count() >= SESSION_GROUP_PANE_LIMIT {
            return None;
        }
        let new_pane_id = new_pane_id.into();
        if new_pane_id.is_empty() || self.contains_pane(&new_pane_id) {
            return None;
        }
        if !self.contains_pane(pane_id) || self.find_pane(pane_id)?.is_empty() {
            return None;
        }
        let source_pane_id = self.pane_containing_session(session_id)?;
        if source_pane_id == pane_id && self.find_pane(pane_id)?.session_ids.len() < 2 {
            return None;
        }
        let already_empty = self.empty_pane_ids();
        {
            let source = self.find_pane_mut(&source_pane_id)?;
            source.session_ids.retain(|id| id != session_id);
            if source.active_session_id.as_deref() == Some(session_id) {
                source.active_session_id = source.session_ids.first().cloned();
            }
        }

        let mut moved = SessionGroupPane::new(new_pane_id.clone());
        moved.session_ids.push(session_id.to_string());
        moved.active_session_id = Some(session_id.to_string());
        let moved_node = Self::Pane { pane: moved };
        let split_id = new_split_id.into();

        if !self.splice_split(pane_id, &split_id, direction, moved_node, position) {
            return None;
        }
        self.prune_empty_panes_preserving(&already_empty);
        self.normalize_sizes();
        Some(new_pane_id)
    }

    /// Splits `pane_id` in `direction`, opening a new empty pane beside it.
    ///
    /// An empty pane is a drop target: the reader moves a tab into it, or drops
    /// a sidebar session on it. It is kept across normalization so the split
    /// survives the next membership change.
    pub fn split_open_pane(
        &mut self,
        pane_id: &str,
        direction: SplitDirection,
        new_pane_id: impl Into<String>,
        new_split_id: impl Into<String>,
        position: SessionGroupSplitPosition,
    ) -> Option<String> {
        if self.pane_count() >= SESSION_GROUP_PANE_LIMIT {
            return None;
        }
        let new_pane_id = new_pane_id.into();
        if new_pane_id.is_empty() || self.contains_pane(&new_pane_id) {
            return None;
        }
        if !self.contains_pane(pane_id) {
            return None;
        }
        let opened = Self::Pane {
            pane: SessionGroupPane::new(new_pane_id.clone()),
        };
        if !self.splice_split(pane_id, &new_split_id.into(), direction, opened, position) {
            return None;
        }
        self.normalize_sizes();
        Some(new_pane_id)
    }

    /// Replaces the pane `pane_id` with a two-child split holding the original
    /// pane and `moved`.
    fn splice_split(
        &mut self,
        pane_id: &str,
        split_id: &str,
        direction: SplitDirection,
        moved: Self,
        position: SessionGroupSplitPosition,
    ) -> bool {
        if matches!(self, Self::Pane { pane } if pane.id == pane_id) {
            let existing = std::mem::take(self);
            let children = match position {
                SessionGroupSplitPosition::Before => vec![moved, existing],
                SessionGroupSplitPosition::After => vec![existing, moved],
            };
            *self = Self::Split {
                id: split_id.to_string(),
                direction,
                children,
                sizes: vec![
                    (SESSION_GROUP_SPLIT_SCALE / 2) as u16,
                    (SESSION_GROUP_SPLIT_SCALE / 2) as u16,
                ],
            };
            return true;
        }
        match self {
            Self::Split { children, .. } => children.iter_mut().any(|child| {
                child.splice_split(pane_id, split_id, direction, moved.clone(), position)
            }),
            Self::Pane { .. } => false,
        }
    }

    /// Moves `session_id` into `target_pane_id`, keeping the source pane.
    ///
    /// Returns whether the layout changed. An empty source pane is pruned.
    pub fn move_session_to_pane(&mut self, session_id: &str, target_pane_id: &str) -> bool {
        let Some(source_pane_id) = self.pane_containing_session(session_id) else {
            return false;
        };
        if source_pane_id == target_pane_id {
            return false;
        }
        if !self.contains_pane(target_pane_id) {
            return false;
        }
        // Panes that were already empty stay: the reader opened them on
        // purpose. The source pane, which is about to lose its last session,
        // does not.
        let already_empty = self.empty_pane_ids();
        let Some(source) = self.find_pane_mut(&source_pane_id) else {
            return false;
        };
        source.session_ids.retain(|id| id != session_id);
        if source.active_session_id.as_deref() == Some(session_id) {
            source.active_session_id = source.session_ids.first().cloned();
        }
        let Some(target) = self.find_pane_mut(target_pane_id) else {
            return false;
        };
        if !target.session_ids.iter().any(|id| id == session_id) {
            target.session_ids.push(session_id.to_string());
        }
        target.active_session_id = Some(session_id.to_string());
        self.prune_empty_panes_preserving(&already_empty);
        self.normalize_sizes();
        true
    }

    /// Reorders the tabs of one pane. `after` places the session directly after
    /// `anchor_session_id`.
    pub fn reorder_pane_session(
        &mut self,
        pane_id: &str,
        session_id: &str,
        anchor_session_id: &str,
        after: bool,
    ) -> bool {
        if session_id == anchor_session_id {
            return false;
        }
        let Some(pane) = self.find_pane_mut(pane_id) else {
            return false;
        };
        if !pane.session_ids.iter().any(|id| id == anchor_session_id) {
            return false;
        }
        let Some(index) = pane.session_ids.iter().position(|id| id == session_id) else {
            return false;
        };
        pane.session_ids.remove(index);
        let Some(anchor_index) = pane
            .session_ids
            .iter()
            .position(|id| id == anchor_session_id)
        else {
            // The anchor vanished; restore the original order.
            pane.session_ids.insert(index, session_id.to_string());
            return false;
        };
        let insertion = anchor_index + usize::from(after);
        pane.session_ids.insert(insertion, session_id.to_string());
        true
    }

    /// Sets which session a pane shows on top.
    pub fn focus_session(&mut self, pane_id: &str, session_id: &str) -> bool {
        let Some(pane) = self.find_pane_mut(pane_id) else {
            return false;
        };
        if !pane.session_ids.iter().any(|id| id == session_id) {
            return false;
        }
        if pane.active_session_id.as_deref() == Some(session_id) {
            return false;
        }
        pane.active_session_id = Some(session_id.to_string());
        true
    }

    pub fn resize_split(&mut self, split_id: &str, sizes: Vec<u16>) -> bool {
        match self {
            Self::Pane { .. } => false,
            Self::Split {
                id,
                children,
                sizes: current,
                ..
            } => {
                if id == split_id {
                    if sizes.len() != children.len() {
                        return false;
                    }
                    if *current == sizes {
                        return false;
                    }
                    *current = sizes;
                    self.normalize_sizes();
                    return true;
                }
                children
                    .iter_mut()
                    .any(|child| child.resize_split(split_id, sizes.clone()))
            }
        }
    }

    /// Splits the group back into one pane holding every member.
    pub fn collapse_to_single_pane(&mut self, pane_id: impl Into<String>, members: &[String]) {
        let mut pane = SessionGroupPane::new(pane_id);
        pane.session_ids = members.to_vec();
        pane.session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
        pane.active_session_id = pane.session_ids.first().cloned();
        *self = Self::Pane { pane };
    }
}

/// Which side of the split target the moved session lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionGroupSplitPosition {
    Before,
    After,
}

/// One session group: sidebar identity plus its multi-pane workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroupUiState {
    pub name: String,
    pub project_id: String,
    /// The Worktree this group is scoped to. Every member must belong to it.
    pub workspace_id: String,
    /// Members in group order; also the order sessions appear in the panes.
    #[serde(default)]
    pub member_session_ids: Vec<String>,
    #[serde(default)]
    pub layout: SessionGroupLayout,
    #[serde(default)]
    pub focused_pane_id: String,
    /// Pane shown alone, covering the rest of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized_pane_id: Option<String>,
    /// Group-level auto continue. `None` inherits the project preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_continue: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

impl SessionGroupUiState {
    pub fn new(
        name: impl Into<String>,
        project_id: impl Into<String>,
        workspace_id: impl Into<String>,
        member_session_ids: Vec<String>,
    ) -> Self {
        let mut member_session_ids = member_session_ids;
        member_session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
        let mut group = Self {
            name: name.into(),
            project_id: project_id.into(),
            workspace_id: workspace_id.into(),
            member_session_ids,
            layout: SessionGroupLayout::default(),
            focused_pane_id: SESSION_GROUP_MAIN_PANE_ID.to_string(),
            maximized_pane_id: None,
            auto_continue: None,
            pinned: false,
        };
        group.normalize();
        group
    }

    pub fn member_count(&self) -> usize {
        self.member_session_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.member_session_ids.is_empty()
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.member_session_ids.iter().any(|id| id == session_id)
    }

    /// Bounds every field and re-derives the layout from the member list, so a
    /// hand-edited or stale document cannot describe a session twice.
    pub fn normalize(&mut self) {
        self.name = bounded_text(&self.name, SESSION_GROUP_NAME_MAX_CHARS)
            .unwrap_or_else(|| "Session group".to_string());
        self.project_id = bounded_text(&self.project_id, 256).unwrap_or_default();
        self.workspace_id = bounded_text(&self.workspace_id, 256).unwrap_or_default();

        let mut seen = BTreeSet::new();
        let mut normalized_members = Vec::with_capacity(self.member_session_ids.len());
        for session_id in std::mem::take(&mut self.member_session_ids) {
            let Some(session_id) = bounded_text(&session_id, 256) else {
                continue;
            };
            if seen.insert(session_id.clone()) {
                normalized_members.push(session_id);
            }
        }
        normalized_members.truncate(SESSION_GROUP_MEMBER_LIMIT);
        self.member_session_ids = normalized_members;

        let members = self.member_session_ids.clone();
        let preferred_first = if self.focused_pane_id.is_empty() {
            SESSION_GROUP_MAIN_PANE_ID.to_string()
        } else {
            self.focused_pane_id.clone()
        };
        self.layout.reconcile_members(&members, &preferred_first);
        self.layout.normalize_sizes();

        let pane_ids = self.layout.pane_ids();
        if !pane_ids.iter().any(|id| id == &self.focused_pane_id) {
            self.focused_pane_id = pane_ids
                .first()
                .cloned()
                .unwrap_or_else(|| SESSION_GROUP_MAIN_PANE_ID.to_string());
        }
        if self
            .maximized_pane_id
            .as_ref()
            .is_some_and(|pane_id| !pane_ids.iter().any(|id| id == pane_id))
        {
            self.maximized_pane_id = None;
        }
    }

    /// Appends sessions that are not members yet. Returns whether anything
    /// changed. Callers are responsible for rejecting a session from another
    /// workspace.
    pub fn add_members(&mut self, session_ids: &[String]) -> bool {
        let mut changed = false;
        for session_id in session_ids {
            let Some(session_id) = bounded_text(session_id, 256) else {
                continue;
            };
            if self.member_session_ids.len() >= SESSION_GROUP_MEMBER_LIMIT {
                break;
            }
            if self.member_session_ids.iter().any(|id| id == &session_id) {
                continue;
            }
            self.member_session_ids.push(session_id);
            changed = true;
        }
        if changed {
            self.normalize();
        }
        changed
    }

    /// Removes sessions from the group and from every pane.
    pub fn remove_members(&mut self, session_ids: &[String]) -> bool {
        let removing = session_ids.iter().cloned().collect::<BTreeSet<_>>();
        let before = self.member_session_ids.len();
        self.member_session_ids
            .retain(|session_id| !removing.contains(session_id));
        if self.member_session_ids.len() == before {
            return false;
        }
        self.normalize();
        true
    }

    /// Replaces the member list, keeping the sessions that remain.
    pub fn set_members(&mut self, session_ids: Vec<String>) -> bool {
        if self.member_session_ids == session_ids {
            return false;
        }
        self.member_session_ids = session_ids;
        self.normalize();
        true
    }

    /// Sessions the group should render with a live view: every session in the
    /// focused pane first, then the remaining panes in tree order.
    ///
    /// The list is bounded by [`SESSION_GROUP_LIVE_PANE_LIMIT`] panes, so the
    /// caller can hold one live conversation per returned pane without letting a
    /// wide split multiply resident memory without limit.
    pub fn live_session_ids(&self) -> Vec<String> {
        let pane_ids = self.layout.pane_ids();
        let mut ordered = Vec::with_capacity(pane_ids.len());
        if pane_ids.iter().any(|id| id == &self.focused_pane_id) {
            ordered.push(self.focused_pane_id.clone());
        }
        for pane_id in pane_ids {
            if !ordered.contains(&pane_id) {
                ordered.push(pane_id);
            }
        }
        if let Some(maximized) = self.maximized_pane_id.as_ref()
            && ordered.iter().any(|id| id == maximized)
        {
            ordered.retain(|id| id == maximized);
        }
        let mut sessions = Vec::new();
        for pane_id in ordered.into_iter().take(SESSION_GROUP_LIVE_PANE_LIMIT) {
            let Some(pane) = self.layout.find_pane(&pane_id) else {
                continue;
            };
            let Some(active) = pane.active_session_id.clone() else {
                continue;
            };
            if !sessions.contains(&active) {
                sessions.push(active);
            }
        }
        sessions
    }

    /// The session the group workspace treats as focused.
    pub fn focused_session_id(&self) -> Option<String> {
        self.layout
            .find_pane(&self.focused_pane_id)
            .and_then(|pane| pane.active_session_id.clone())
    }

    pub fn focus_pane(&mut self, pane_id: &str) -> bool {
        if self.focused_pane_id == pane_id || !self.layout.contains_pane(pane_id) {
            return false;
        }
        self.focused_pane_id = pane_id.to_string();
        true
    }

    pub fn toggle_maximized_pane(&mut self, pane_id: &str) -> bool {
        if !self.layout.contains_pane(pane_id) {
            return false;
        }
        if self.maximized_pane_id.as_deref() == Some(pane_id) {
            self.maximized_pane_id = None;
        } else {
            self.maximized_pane_id = Some(pane_id.to_string());
            self.focused_pane_id = pane_id.to_string();
        }
        true
    }

    /// Collapses the workspace back to one pane holding every member.
    pub fn merge_panes(&mut self) -> bool {
        if self.layout.pane_count() <= 1 && self.maximized_pane_id.is_none() {
            return false;
        }
        let members = self.member_session_ids.clone();
        let pane_id = self
            .layout
            .first_pane_id()
            .unwrap_or_else(|| SESSION_GROUP_MAIN_PANE_ID.to_string());
        self.layout
            .collapse_to_single_pane(pane_id.clone(), &members);
        self.focused_pane_id = pane_id;
        self.maximized_pane_id = None;
        true
    }
}

fn bounded_text(value: &str, max_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max_chars {
        return None;
    }
    Some(trimmed.to_string())
}

/// The group name a new group should take inside one workspace: `会话组 N` for
/// the first free ordinal, or `Session group N` in an English locale.
pub fn next_available_group_name<'a>(
    existing_names: impl IntoIterator<Item = &'a str>,
    localized_stem: &str,
) -> String {
    let taken = existing_names
        .into_iter()
        .map(|name| name.to_lowercase())
        .collect::<BTreeSet<_>>();
    let mut ordinal = 1usize;
    loop {
        let candidate = format!("{localized_stem} {ordinal}");
        if !taken.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        ordinal += 1;
        if ordinal > SESSION_GROUP_LIMIT {
            return format!("{localized_stem} {ordinal}");
        }
    }
}

/// Groups ordered for the sidebar: pinned first, then the caller's order, then
/// by name so a freshly discovered group never lands above a placed one.
pub fn ordered_groups<'a>(
    groups: impl IntoIterator<Item = (&'a String, &'a SessionGroupUiState)>,
    order: &[String],
) -> Vec<(String, &'a SessionGroupUiState)> {
    let positions = order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut entries = groups
        .into_iter()
        .map(|(id, group)| (id.clone(), group))
        .collect::<Vec<_>>();
    entries.sort_by(|(left_id, left), (right_id, right)| {
        left.pinned
            .cmp(&right.pinned)
            .reverse()
            .then_with(|| {
                let left_position = positions.get(left_id.as_str()).copied();
                let right_position = positions.get(right_id.as_str()).copied();
                match (left_position, right_position) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left_id.cmp(right_id))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn a_new_group_holds_every_member_in_one_pane() {
        let group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        assert_eq!(group.member_count(), 2);
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(group.focused_pane_id, SESSION_GROUP_MAIN_PANE_ID);
        assert_eq!(group.layout.ordered_session_ids(), members(&["a", "b"]));
        assert_eq!(group.focused_session_id().as_deref(), Some("a"));
    }

    #[test]
    fn an_empty_group_still_describes_one_pane() {
        let group = SessionGroupUiState::new("会话组 1", "project", "workspace", Vec::new());
        assert!(group.is_empty());
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(group.focused_session_id(), None);
    }

    #[test]
    fn adding_and_removing_members_keeps_the_layout_consistent() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a"]));
        assert!(group.add_members(&members(&["b", "c"])));
        assert!(!group.add_members(&members(&["b"])));
        assert_eq!(group.member_count(), 3);
        assert_eq!(group.layout.pane_count(), 1);

        assert!(group.remove_members(&members(&["a"])));
        assert_eq!(group.member_count(), 2);
        assert_eq!(group.layout.ordered_session_ids(), members(&["b", "c"]));
        assert_eq!(group.focused_session_id().as_deref(), Some("b"));
    }

    #[test]
    fn removing_every_member_leaves_a_valid_empty_layout() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        assert!(group.remove_members(&members(&["a", "b"])));
        assert!(group.is_empty());
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(group.focused_session_id(), None);
        assert!(group.live_session_ids().is_empty());
    }

    #[test]
    fn splitting_moves_a_session_into_a_second_pane() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        let new_pane = group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "b",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("split should succeed");
        assert_eq!(new_pane, "pane-2");
        assert_eq!(group.layout.pane_count(), 2);
        assert_eq!(
            group.layout.pane_containing_session("a").as_deref(),
            Some(SESSION_GROUP_MAIN_PANE_ID)
        );
        assert_eq!(
            group.layout.pane_containing_session("b").as_deref(),
            Some("pane-2")
        );
        // Both members are still present exactly once.
        let mut ordered = group.layout.ordered_session_ids();
        ordered.sort();
        assert_eq!(ordered, members(&["a", "b"]));
    }

    #[test]
    fn splitting_a_pane_with_one_session_is_rejected() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a"]));
        assert!(
            group
                .layout
                .split_with_session(
                    SESSION_GROUP_MAIN_PANE_ID,
                    "a",
                    SplitDirection::Horizontal,
                    "pane-2",
                    "split-1",
                    SessionGroupSplitPosition::After,
                )
                .is_none()
        );
        assert_eq!(group.layout.pane_count(), 1);
    }

    #[test]
    fn splitting_stops_at_the_pane_limit() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]),
        );
        for index in 0..(SESSION_GROUP_PANE_LIMIT - 1) {
            let pane_id = group.layout.first_pane_id().expect("a pane should remain");
            let moved = group
                .layout
                .find_pane(&pane_id)
                .and_then(|pane| pane.session_ids.last().cloned())
                .expect("a session should remain");
            if group
                .layout
                .split_with_session(
                    &pane_id,
                    &moved,
                    SplitDirection::Horizontal,
                    format!("pane-{index}"),
                    format!("split-{index}"),
                    SessionGroupSplitPosition::After,
                )
                .is_none()
            {
                break;
            }
        }
        assert_eq!(group.layout.pane_count(), SESSION_GROUP_PANE_LIMIT);
        assert!(
            group
                .layout
                .split_with_session(
                    SESSION_GROUP_MAIN_PANE_ID,
                    "a",
                    SplitDirection::Horizontal,
                    "pane-overflow",
                    "split-overflow",
                    SessionGroupSplitPosition::After,
                )
                .is_none()
        );
    }

    /// A workspace grows past two panes by splitting any pane again, which is
    /// what makes a group behave like a tab panel instead of a fixed pair.
    #[test]
    fn a_workspace_grows_past_two_panes() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c", "d"]),
        );
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "b",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("first split");
        // The pane that kept the other sessions splits again, so the workspace
        // reaches three panes without ever merging anything back.
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "c",
                SplitDirection::Horizontal,
                "pane-3",
                "split-2",
                SessionGroupSplitPosition::After,
            )
            .expect("second split");
        // A pane holding one tab has nothing to leave behind, so it opens an
        // empty pane instead; that is the path a one-tab pane grows by.
        group
            .layout
            .split_open_pane(
                "pane-2",
                SplitDirection::Vertical,
                "pane-4",
                "split-3",
                SessionGroupSplitPosition::After,
            )
            .expect("third split");
        assert_eq!(group.layout.pane_count(), 4);
        assert_eq!(group.layout.ordered_session_ids().len(), 4);
        for session_id in ["a", "b", "c", "d"] {
            assert!(
                group.layout.pane_containing_session(session_id).is_some(),
                "{session_id} should still live in exactly one pane"
            );
        }
    }

    /// Splitting with a session that lives in another pane moves it next to the
    /// target, which is how a drag from one pane (or the sidebar) creates a new
    /// pane instead of doing nothing.
    #[test]
    fn a_session_from_another_pane_splits_next_to_the_target() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c"]),
        );
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "c",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("first split");
        // "a" lives in the main pane; splitting pane-2 with it must still work.
        let opened = group
            .layout
            .split_with_session(
                "pane-2",
                "a",
                SplitDirection::Vertical,
                "pane-3",
                "split-2",
                SessionGroupSplitPosition::After,
            )
            .expect("cross-pane split");
        assert_eq!(opened, "pane-3");
        assert_eq!(
            group.layout.pane_containing_session("a").as_deref(),
            Some("pane-3")
        );
        assert_eq!(group.layout.pane_count(), 3);
        assert_eq!(group.layout.ordered_session_ids().len(), 3);
    }

    /// A pane opened on purpose starts empty and has to survive normalization,
    /// or the split the reader just made would disappear on the next edit.
    #[test]
    fn an_opened_empty_pane_survives_normalization() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a"]));
        let opened = group
            .layout
            .split_open_pane(
                SESSION_GROUP_MAIN_PANE_ID,
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("opening a pane should succeed");
        assert_eq!(opened, "pane-2");
        assert_eq!(group.layout.pane_count(), 2);
        assert!(
            group
                .layout
                .find_pane("pane-2")
                .is_some_and(|pane| pane.is_empty())
        );

        // Any later membership change normalizes the group; the empty pane stays.
        assert!(group.add_members(&members(&["b"])));
        assert_eq!(group.layout.pane_count(), 2);
        assert!(
            group
                .layout
                .find_pane("pane-2")
                .is_some_and(|pane| pane.is_empty())
        );

        // Filling it makes it an ordinary pane holding that session.
        assert!(group.layout.move_session_to_pane("b", "pane-2"));
        assert_eq!(
            group.layout.pane_containing_session("b").as_deref(),
            Some("pane-2")
        );
        assert_eq!(group.layout.pane_count(), 2);
    }

    #[test]
    fn moving_a_session_out_of_a_pane_prunes_the_empty_pane() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c"]),
        );
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "c",
                SplitDirection::Vertical,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("split should succeed");
        assert_eq!(group.layout.pane_count(), 2);

        assert!(
            group
                .layout
                .move_session_to_pane("c", SESSION_GROUP_MAIN_PANE_ID)
        );
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(
            group.layout.ordered_session_ids(),
            members(&["a", "b", "c"])
        );
    }

    #[test]
    fn a_removed_member_disappears_from_the_layout() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "b",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("split should succeed");
        group.remove_members(&members(&["b"]));
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(group.layout.ordered_session_ids(), members(&["a"]));
    }

    #[test]
    fn live_sessions_put_the_focused_pane_first_and_stay_bounded() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c", "d", "e", "f", "g", "h"]),
        );
        for (index, session) in ["b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            let pane_id = group.layout.first_pane_id().expect("a pane");
            if group
                .layout
                .split_with_session(
                    &pane_id,
                    session,
                    SplitDirection::Horizontal,
                    format!("pane-{index}"),
                    format!("split-{index}"),
                    SessionGroupSplitPosition::After,
                )
                .is_none()
            {
                break;
            }
        }
        assert!(group.layout.pane_count() > SESSION_GROUP_LIVE_PANE_LIMIT);
        let live = group.live_session_ids();
        assert_eq!(live.len(), SESSION_GROUP_LIVE_PANE_LIMIT);
        assert_eq!(live.first().map(String::as_str), Some("a"));
    }

    #[test]
    fn maximized_pane_narrows_the_live_set() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "b",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("split should succeed");
        assert!(group.toggle_maximized_pane("pane-2"));
        assert_eq!(group.maximized_pane_id.as_deref(), Some("pane-2"));
        assert_eq!(group.focused_pane_id, "pane-2");
        assert_eq!(group.live_session_ids(), members(&["b"]));
        assert!(group.toggle_maximized_pane("pane-2"));
        assert_eq!(group.maximized_pane_id, None);
    }

    #[test]
    fn merging_panes_restores_one_pane_with_every_member() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c"]),
        );
        group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "c",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("split should succeed");
        assert!(group.merge_panes());
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(
            group.layout.ordered_session_ids(),
            members(&["a", "b", "c"])
        );
        assert!(!group.merge_panes());
    }

    #[test]
    fn normalize_drops_duplicate_and_unknown_sessions() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        group.member_session_ids = members(&["a", "a", "b", "a"]);
        group.normalize();
        assert_eq!(group.member_session_ids, members(&["a", "b"]));
    }

    #[test]
    fn normalize_repairs_a_layout_that_lost_its_members() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a", "b"]));
        // A hand-edited document that places an unknown session in the pane.
        group.layout = SessionGroupLayout::Pane {
            pane: SessionGroupPane {
                id: "stale-pane".to_string(),
                session_ids: members(&["ghost"]),
                active_session_id: Some("ghost".to_string()),
            },
        };
        group.focused_pane_id = "missing-pane".to_string();
        group.maximized_pane_id = Some("missing-pane".to_string());
        group.normalize();
        assert_eq!(group.layout.ordered_session_ids(), members(&["a", "b"]));
        assert!(group.layout.contains_pane(&group.focused_pane_id));
        assert_eq!(group.maximized_pane_id, None);
    }

    #[test]
    fn normalize_bounds_the_group_name() {
        let mut group = SessionGroupUiState::new("", "project", "workspace", members(&["a"]));
        assert_eq!(group.name, "Session group");
        let long = "x".repeat(SESSION_GROUP_NAME_MAX_CHARS + 1);
        group.name = long;
        group.normalize();
        assert_eq!(group.name, "Session group");
    }

    #[test]
    fn next_available_group_name_skips_taken_ordinals() {
        assert_eq!(
            next_available_group_name(std::iter::empty(), "会话组"),
            "会话组 1"
        );
        assert_eq!(
            next_available_group_name(["会话组 1", "会话组 2"], "会话组"),
            "会话组 3"
        );
        // Matching is case-insensitive so "Session group 1" blocks a duplicate.
        assert_eq!(
            next_available_group_name(["session group 1"], "Session group"),
            "Session group 2"
        );
    }

    #[test]
    fn ordered_groups_puts_pinned_groups_first_then_the_saved_order() {
        let mut first = SessionGroupUiState::new("A", "p", "w", members(&["a"]));
        let second = SessionGroupUiState::new("B", "p", "w", members(&["b"]));
        let third = SessionGroupUiState::new("C", "p", "w", members(&["c"]));
        first.pinned = true;
        let groups = BTreeMap::from([
            ("g1".to_string(), first),
            ("g2".to_string(), second),
            ("g3".to_string(), third),
        ]);
        let ordered = ordered_groups(groups.iter(), &["g2".to_string(), "g3".to_string()]);
        assert_eq!(
            ordered
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["g1", "g2", "g3"]
        );
    }

    #[test]
    fn split_sizes_are_normalized_into_a_usable_share() {
        let mut layout = SessionGroupLayout::Split {
            id: "split-1".to_string(),
            direction: SplitDirection::Horizontal,
            children: vec![
                SessionGroupLayout::pane("pane-1"),
                SessionGroupLayout::pane("pane-2"),
            ],
            sizes: vec![0, 0],
        };
        layout.normalize_sizes();
        let SessionGroupLayout::Split { sizes, .. } = &layout else {
            panic!("layout should stay a split");
        };
        assert_eq!(sizes.len(), 2);
        assert_eq!(
            sizes.iter().map(|size| u32::from(*size)).sum::<u32>(),
            SESSION_GROUP_SPLIT_SCALE
        );
        assert!((split_share(sizes[0]) - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_lopsided_split_is_clamped_but_still_sums_to_the_scale() {
        let mut layout = SessionGroupLayout::Split {
            id: "split-1".to_string(),
            direction: SplitDirection::Vertical,
            children: vec![
                SessionGroupLayout::pane("pane-1"),
                SessionGroupLayout::pane("pane-2"),
                SessionGroupLayout::pane("pane-3"),
            ],
            sizes: vec![980, 10, 10],
        };
        layout.normalize_sizes();
        let SessionGroupLayout::Split { sizes, .. } = &layout else {
            panic!("layout should stay a split");
        };
        assert_eq!(
            sizes.iter().map(|size| u32::from(*size)).sum::<u32>(),
            SESSION_GROUP_SPLIT_SCALE
        );
        assert!(sizes.iter().all(|size| *size >= 50));
    }

    #[test]
    fn reordering_within_a_pane_places_the_session_next_to_its_anchor() {
        let mut group = SessionGroupUiState::new(
            "会话组 1",
            "project",
            "workspace",
            members(&["a", "b", "c"]),
        );
        assert!(
            group
                .layout
                .reorder_pane_session(SESSION_GROUP_MAIN_PANE_ID, "c", "a", false)
        );
        assert_eq!(
            group
                .layout
                .find_pane(SESSION_GROUP_MAIN_PANE_ID)
                .map(|pane| pane.session_ids.clone()),
            Some(members(&["c", "a", "b"]))
        );
        assert!(
            !group
                .layout
                .reorder_pane_session(SESSION_GROUP_MAIN_PANE_ID, "c", "c", false)
        );
    }
}
