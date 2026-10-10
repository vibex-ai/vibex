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
use vibex_core::{
    AppliedGroupLayout, SessionGroupLayoutIntent, SessionGroupLayoutPreset,
    VIBEX_USE_MAX_LIVE_PANES, VibexUseRef,
};

use crate::SplitDirection;

/// Distinct team outcomes; a finished round is not an accepted task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionGroupTeamStatus {
    pub running: usize,
    pub waiting: usize,
    pub review: usize,
    pub results_ready: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl SessionGroupTeamStatus {
    pub fn record(
        &mut self,
        node: Option<&vibex_core::SessionTreeNode>,
        state: vibex_core::AgentSessionState,
    ) {
        use vibex_core::{AgentSessionState, DelegationCompletionPolicy, DelegationTaskPhase};
        match node.and_then(|node| node.task_phase) {
            Some(DelegationTaskPhase::Completed)
                if node.and_then(|node| node.completion_policy)
                    == Some(DelegationCompletionPolicy::SingleTurnLegacy) =>
            {
                self.results_ready += 1
            }
            Some(DelegationTaskPhase::Completed) => self.completed += 1,
            Some(DelegationTaskPhase::Failed) => self.failed += 1,
            Some(DelegationTaskPhase::Cancelled) => self.cancelled += 1,
            Some(DelegationTaskPhase::AwaitingReview) => self.review += 1,
            _ if node.is_some_and(|node| node.blocked_on.is_some())
                || state == AgentSessionState::NeedsInput =>
            {
                self.waiting += 1
            }
            Some(
                DelegationTaskPhase::Queued
                | DelegationTaskPhase::Starting
                | DelegationTaskPhase::Active
                | DelegationTaskPhase::Cancelling,
            ) => self.running += 1,
            None => match state {
                AgentSessionState::Initializing | AgentSessionState::Running => self.running += 1,
                AgentSessionState::Error => self.failed += 1,
                _ => {}
            },
        }
    }
}

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

    /// The last pane in reading order, which is where overflow tabs land.
    pub fn last_pane_mut(&mut self) -> Option<&mut SessionGroupPane> {
        match self {
            Self::Pane { pane } => Some(pane),
            Self::Split { children, .. } => children.iter_mut().rev().find_map(Self::last_pane_mut),
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
    /// past two panes. Splitting a pane with its own tab moves the tab into the
    /// new half and keeps the original pane, empty and ready for another
    /// session, so a one-tab pane splits instead of refusing.
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
        let mut already_empty = self.empty_pane_ids();
        if source_pane_id == pane_id {
            // The source pane is about to lose its only session. Keep it: the
            // reader asked for a split, not for the pane to disappear.
            already_empty.insert(pane_id.to_string());
        }
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

/// One pane holding exactly these sessions, with the first one on top.
fn pane_with(pane_id: &str, session_ids: &[String]) -> SessionGroupPane {
    let mut pane = SessionGroupPane::new(pane_id);
    pane.session_ids = session_ids.to_vec();
    pane.session_ids.truncate(SESSION_GROUP_MEMBER_LIMIT);
    pane.active_session_id = pane.session_ids.first().cloned();
    pane
}

/// Even per-mille shares for `count` children.
fn even_sizes(count: usize) -> Vec<u16> {
    if count == 0 {
        return Vec::new();
    }
    let share = (SESSION_GROUP_SPLIT_SCALE / count as u32) as u16;
    let mut sizes = vec![share; count];
    if let Some(last) = sizes.last_mut() {
        *last = SESSION_GROUP_SPLIT_SCALE as u16 - share * (count as u16 - 1);
    }
    sizes
}

fn split(
    id: &str,
    direction: SplitDirection,
    children: Vec<SessionGroupLayout>,
) -> SessionGroupLayout {
    let sizes = even_sizes(children.len());
    SessionGroupLayout::Split {
        id: id.to_string(),
        direction,
        children,
        sizes,
    }
}

/// One pane per session, side by side. Sessions beyond the budget become tabs
/// on the last visible pane.
fn columns_layout(
    ordered: &[String],
    live: usize,
) -> (SessionGroupLayout, Vec<String>, Vec<String>) {
    let live = live.max(1).min(ordered.len());
    let visible: Vec<String> = ordered.iter().take(live).cloned().collect();
    let tabbed: Vec<String> = ordered.iter().skip(live).cloned().collect();
    if live == 1 {
        let mut pane = pane_with(SESSION_GROUP_MAIN_PANE_ID, &visible);
        pane.session_ids.extend(tabbed.iter().cloned());
        return (SessionGroupLayout::Pane { pane }, visible, tabbed);
    }
    let mut children = Vec::with_capacity(live);
    for (index, session_id) in visible.iter().enumerate() {
        children.push(SessionGroupLayout::Pane {
            pane: pane_with(&pane_id(index), std::slice::from_ref(session_id)),
        });
    }
    if let Some(last) = children.last_mut()
        && let SessionGroupLayout::Pane { pane } = last
    {
        pane.session_ids.extend(tabbed.iter().cloned());
    }
    (
        split(
            "session-group-split-columns",
            SplitDirection::Horizontal,
            children,
        ),
        visible,
        tabbed,
    )
}

/// A roughly square grid. The fourth cell and beyond stack as tabs.
fn grid_layout(ordered: &[String], live: usize) -> (SessionGroupLayout, Vec<String>, Vec<String>) {
    let live = live.max(1).min(ordered.len());
    let visible: Vec<String> = ordered.iter().take(live).cloned().collect();
    let tabbed: Vec<String> = ordered.iter().skip(live).cloned().collect();
    if live <= 2 {
        return columns_layout(ordered, live);
    }
    let columns = live.div_ceil(2);
    let rows: Vec<SessionGroupLayout> = (0..2)
        .map(|row| {
            let cells: Vec<SessionGroupLayout> = (0..columns)
                .filter_map(|column| {
                    let index = row * columns + column;
                    let session_id = visible.get(index)?;
                    Some(SessionGroupLayout::Pane {
                        pane: pane_with(&pane_id(index), std::slice::from_ref(session_id)),
                    })
                })
                .collect();
            if cells.len() == 1 {
                cells.into_iter().next().unwrap_or_default()
            } else {
                split(
                    &format!("session-group-split-row-{row}"),
                    SplitDirection::Horizontal,
                    cells,
                )
            }
        })
        .collect();
    let mut layout = split("session-group-split-grid", SplitDirection::Vertical, rows);
    if !tabbed.is_empty()
        && let Some(last_pane) = layout.last_pane_mut()
    {
        last_pane.session_ids.extend(tabbed.iter().cloned());
    }
    (layout, visible, tabbed)
}

/// The lead in a stable column of its own, workers stacked beside it.
fn lead_and_workers_layout(
    ordered: &[String],
    live: usize,
) -> (SessionGroupLayout, Vec<String>, Vec<String>) {
    let live = live.max(1).min(ordered.len());
    if live <= 2 {
        return columns_layout(ordered, live);
    }
    let visible: Vec<String> = ordered.iter().take(live).cloned().collect();
    let tabbed: Vec<String> = ordered.iter().skip(live).cloned().collect();
    let lead = SessionGroupLayout::Pane {
        pane: pane_with(&pane_id(0), std::slice::from_ref(&visible[0])),
    };
    let workers: Vec<SessionGroupLayout> = visible
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, session_id)| SessionGroupLayout::Pane {
            pane: pane_with(&pane_id(index), std::slice::from_ref(session_id)),
        })
        .collect();
    let mut worker_column = if workers.len() == 1 {
        workers.into_iter().next().unwrap_or_default()
    } else {
        split(
            "session-group-split-workers",
            SplitDirection::Vertical,
            workers,
        )
    };
    if !tabbed.is_empty()
        && let SessionGroupLayout::Pane { pane } = &mut worker_column
    {
        pane.session_ids.extend(tabbed.iter().cloned());
    } else if !tabbed.is_empty()
        && let Some(pane) = worker_column.last_pane_mut()
    {
        pane.session_ids.extend(tabbed.iter().cloned());
    }
    (
        split(
            "session-group-split-lead",
            SplitDirection::Horizontal,
            vec![lead, worker_column],
        ),
        visible,
        tabbed,
    )
}

fn pane_id(index: usize) -> String {
    if index == 0 {
        SESSION_GROUP_MAIN_PANE_ID.to_string()
    } else {
        format!("{SESSION_GROUP_MAIN_PANE_ID}-{index}")
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
    /// Agent-requested layout, retained until the user arranges the panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_layout: Option<SessionGroupLayoutIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_automatic_layout: Option<SessionGroupLayoutIntent>,
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
            automatic_layout: None,
            applied_automatic_layout: None,
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

    /// Reports the arrangement the group currently has, without changing it.
    ///
    /// Presenting an existing group uses this: the caller learns what is on
    /// screen, and the user's own arrangement is left exactly as it was.
    pub fn observed_layout(&self) -> AppliedGroupLayout {
        let visible = self.live_session_ids();
        let preset = if self.maximized_pane_id.is_some() {
            SessionGroupLayoutPreset::Single
        } else if let Some(applied) = self.applied_automatic_layout.as_ref() {
            applied.preset
        } else {
            match &self.layout {
                SessionGroupLayout::Pane { .. } => SessionGroupLayoutPreset::Tabs,
                SessionGroupLayout::Split { direction, .. } => match direction {
                    SplitDirection::Horizontal => SessionGroupLayoutPreset::Columns,
                    SplitDirection::Vertical => SessionGroupLayoutPreset::Grid,
                },
            }
        };
        let reference =
            |id: &String| VibexUseRef::new(vibex_core::VibexUseResourceKind::Session, id.clone());
        AppliedGroupLayout {
            preset,
            live_panes: visible.len(),
            visible_session_refs: visible.iter().map(reference).collect(),
            tabbed_session_refs: self
                .member_session_ids
                .iter()
                .filter(|id| !visible.contains(id))
                .map(reference)
                .collect(),
        }
    }

    /// Applies an automatic layout using the conversation column's width in
    /// rems. Its minimum pane width scales with interface zoom.
    pub fn set_automatic_layout(
        &mut self,
        intent: SessionGroupLayoutIntent,
        width_rem: f32,
    ) -> AppliedGroupLayout {
        self.automatic_layout = Some(intent);
        self.applied_automatic_layout = None;
        self.adapt_layout_to_width(width_rem);
        self.observed_layout()
    }

    /// Reflows only when a width boundary or the requested layout changes.
    /// Tab selection remains stable while the window stays in the same band.
    fn effective_automatic_layout(&self, width_rem: f32) -> Option<SessionGroupLayoutIntent> {
        let intent = self.automatic_layout.as_ref()?;
        let budget = intent
            .preferred_live_panes
            .unwrap_or(VIBEX_USE_MAX_LIVE_PANES)
            .clamp(1, VIBEX_USE_MAX_LIVE_PANES)
            .min(self.member_count().max(1));
        let columns = if width_rem.is_finite() {
            (width_rem.max(0.0) / 24.0).floor() as usize
        } else {
            1
        };
        let preset = match intent.preset {
            SessionGroupLayoutPreset::Single | SessionGroupLayoutPreset::Tabs => intent.preset,
            _ if columns < 2 => SessionGroupLayoutPreset::Tabs,
            SessionGroupLayoutPreset::LeadAndWorkers if budget == 3 && columns >= 3 => {
                SessionGroupLayoutPreset::Columns
            }
            SessionGroupLayoutPreset::LeadAndWorkers if budget >= 4 => {
                SessionGroupLayoutPreset::Grid
            }
            SessionGroupLayoutPreset::Columns if columns < budget => SessionGroupLayoutPreset::Grid,
            _ => intent.preset,
        };
        Some(SessionGroupLayoutIntent {
            preset,
            lead_session_ref: intent.lead_session_ref.clone(),
            preferred_live_panes: Some(budget),
        })
    }

    pub fn needs_layout_adaptation(&self, width_rem: f32) -> bool {
        self.effective_automatic_layout(width_rem).as_ref()
            != self.applied_automatic_layout.as_ref()
    }

    pub fn adapt_layout_to_width(&mut self, width_rem: f32) -> bool {
        let Some(effective) = self.effective_automatic_layout(width_rem) else {
            return false;
        };
        if self.applied_automatic_layout.as_ref() == Some(&effective) {
            return false;
        }
        let focused = self.focused_session_id();
        let before = self.layout.clone();
        self.apply_layout_preset(
            effective.preset,
            effective
                .lead_session_ref
                .as_ref()
                .map(|reference| reference.id.as_str()),
            effective.preferred_live_panes,
        );
        if let Some(focused) = focused
            && let Some(pane_id) = self.layout.pane_containing_session(&focused)
        {
            self.layout.focus_session(&pane_id, &focused);
            self.focused_pane_id = pane_id;
        }
        self.applied_automatic_layout = Some(effective);
        before != self.layout
    }

    /// Manual pane edits take over from the automatic presentation request.
    pub fn disable_automatic_layout(&mut self) {
        self.automatic_layout = None;
        self.applied_automatic_layout = None;
    }

    pub fn apply_layout_preset(
        &mut self,
        preset: SessionGroupLayoutPreset,
        lead_session_id: Option<&str>,
        preferred_live_panes: Option<usize>,
    ) -> AppliedGroupLayout {
        let live_budget = preferred_live_panes
            .unwrap_or(SESSION_GROUP_LIVE_PANE_LIMIT)
            .clamp(1, SESSION_GROUP_LIVE_PANE_LIMIT)
            .min(SESSION_GROUP_MEMBER_LIMIT);
        let lead = lead_session_id
            .filter(|lead| self.contains(lead))
            .map(ToString::to_string)
            .or_else(|| self.member_session_ids.first().cloned());

        // The lead always stays visible; the rest are filled in member order.
        let mut ordered = Vec::with_capacity(self.member_session_ids.len());
        if let Some(lead) = lead.as_ref() {
            ordered.push(lead.clone());
        }
        for session_id in &self.member_session_ids {
            if Some(session_id) != lead.as_ref() {
                ordered.push(session_id.clone());
            }
        }

        let (layout, _, _) = match (preset, ordered.len()) {
            (_, 0) => (SessionGroupLayout::default(), Vec::new(), Vec::new()),
            (SessionGroupLayoutPreset::Single | SessionGroupLayoutPreset::Tabs, _) => {
                let pane = pane_with(SESSION_GROUP_MAIN_PANE_ID, &ordered);
                (
                    SessionGroupLayout::Pane { pane },
                    ordered.clone(),
                    Vec::new(),
                )
            }
            (SessionGroupLayoutPreset::Columns, count) => {
                columns_layout(&ordered, count.min(live_budget))
            }
            (SessionGroupLayoutPreset::Grid, count) => {
                grid_layout(&ordered, count.min(live_budget))
            }
            (SessionGroupLayoutPreset::LeadAndWorkers, count) => {
                lead_and_workers_layout(&ordered, count.min(live_budget))
            }
        };

        self.layout = layout;
        self.maximized_pane_id = None;
        let pane_ids = self.layout.pane_ids();
        self.focused_pane_id = pane_ids
            .first()
            .cloned()
            .unwrap_or_else(|| SESSION_GROUP_MAIN_PANE_ID.to_string());
        self.normalize();

        let mut applied = self.observed_layout();
        applied.preset = preset;
        applied
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
    fn splitting_a_pane_with_one_session_moves_it_and_keeps_the_pane() {
        let mut group =
            SessionGroupUiState::new("会话组 1", "project", "workspace", members(&["a"]));
        let opened = group
            .layout
            .split_with_session(
                SESSION_GROUP_MAIN_PANE_ID,
                "a",
                SplitDirection::Horizontal,
                "pane-2",
                "split-1",
                SessionGroupSplitPosition::After,
            )
            .expect("a one-tab pane should still split");
        // The tab lands in the new half and the original pane stays, empty, so
        // the reader can fill it with another session.
        assert_eq!(opened, "pane-2");
        assert_eq!(
            group.layout.pane_containing_session("a").as_deref(),
            Some("pane-2")
        );
        assert_eq!(group.layout.pane_count(), 2);
        assert!(
            group
                .layout
                .find_pane(SESSION_GROUP_MAIN_PANE_ID)
                .is_some_and(|pane| pane.is_empty())
        );
        // And the empty pane survives the next membership change.
        assert!(group.add_members(&members(&["b"])));
        assert_eq!(group.layout.pane_count(), 2);
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

    fn preset_group(members: &[&str]) -> SessionGroupUiState {
        SessionGroupUiState::new(
            "Team",
            "project",
            "workspace",
            members.iter().map(|id| (*id).to_string()).collect(),
        )
    }

    #[test]
    fn a_single_preset_keeps_every_member_as_a_tab() {
        let mut group = preset_group(&["lead", "worker-a", "worker-b"]);
        let applied =
            group.apply_layout_preset(SessionGroupLayoutPreset::Single, Some("lead"), None);
        assert_eq!(group.layout.pane_count(), 1);
        assert_eq!(
            group.layout.ordered_session_ids(),
            vec!["lead", "worker-a", "worker-b"]
        );
        assert_eq!(applied.visible_session_refs.len(), 1);
        assert_eq!(applied.tabbed_session_refs.len(), 2);
        assert_eq!(applied.preset, SessionGroupLayoutPreset::Single);
    }

    #[test]
    fn lead_and_workers_keeps_the_lead_in_its_own_column() {
        let mut group = preset_group(&["worker-a", "lead", "worker-b"]);
        let applied =
            group.apply_layout_preset(SessionGroupLayoutPreset::LeadAndWorkers, Some("lead"), None);
        assert_eq!(group.layout.pane_count(), 3);
        let lead_pane = group
            .layout
            .pane_containing_session("lead")
            .expect("the lead keeps a pane of its own");
        assert_eq!(
            group
                .layout
                .find_pane(&lead_pane)
                .map(|pane| pane.session_ids.clone()),
            Some(vec!["lead".to_string()])
        );
        assert_eq!(applied.visible_session_refs.len(), 3);
    }

    #[test]
    fn sessions_beyond_the_live_budget_become_tabs_instead_of_disappearing() {
        let members = ["a", "b", "c", "d", "e", "f"];
        let mut group = preset_group(&members);
        let applied =
            group.apply_layout_preset(SessionGroupLayoutPreset::Columns, Some("a"), Some(2));
        assert_eq!(applied.visible_session_refs.len(), 2);
        assert_eq!(applied.tabbed_session_refs.len(), 4);
        // Every member is still reachable: overflow lands as tabs, not in a
        // pane list that silently dropped it.
        let placed = group.layout.ordered_session_ids();
        for member in members {
            assert!(placed.contains(&member.to_string()), "{member} was dropped");
        }
        assert!(group.layout.pane_count() <= 2);
    }

    #[test]
    fn a_grid_preset_stays_within_the_pane_limit() {
        let mut group = preset_group(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        let applied = group.apply_layout_preset(SessionGroupLayoutPreset::Grid, None, Some(4));
        assert!(group.layout.pane_count() <= 4);
        assert_eq!(applied.live_panes, group.layout.pane_count());
        assert!(!applied.visible_session_refs.is_empty());
    }

    #[test]
    fn applying_a_preset_clears_a_maximized_pane_and_focuses_a_live_one() {
        let mut group = preset_group(&["a", "b", "c"]);
        group.maximized_pane_id = Some(SESSION_GROUP_MAIN_PANE_ID.to_string());
        group.apply_layout_preset(SessionGroupLayoutPreset::Columns, Some("a"), None);
        assert!(group.maximized_pane_id.is_none());
        assert!(group.layout.contains_pane(&group.focused_pane_id));
    }

    #[test]
    fn an_empty_group_keeps_a_single_pane() {
        let mut group = preset_group(&[]);
        let applied =
            group.apply_layout_preset(SessionGroupLayoutPreset::LeadAndWorkers, None, None);
        assert_eq!(group.layout.pane_count(), 1);
        assert!(applied.visible_session_refs.is_empty());
        assert_eq!(applied.live_panes, 0);
    }

    fn automatic_intent() -> SessionGroupLayoutIntent {
        SessionGroupLayoutIntent {
            preset: SessionGroupLayoutPreset::LeadAndWorkers,
            lead_session_ref: None,
            preferred_live_panes: Some(4),
        }
    }

    #[test]
    fn observed_layout_counts_only_live_panes_and_keeps_all_other_members_reachable() {
        let mut group = preset_group(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        group.layout = split(
            "eight-panes",
            SplitDirection::Horizontal,
            group
                .member_session_ids
                .iter()
                .enumerate()
                .map(|(index, id)| SessionGroupLayout::Pane {
                    pane: pane_with(&pane_id(index), std::slice::from_ref(id)),
                })
                .collect(),
        );
        group.focused_pane_id = pane_id(7);
        let observed = group.observed_layout();
        assert_eq!(observed.live_panes, 4);
        assert_eq!(
            observed
                .visible_session_refs
                .iter()
                .map(|reference| reference.id.as_str())
                .collect::<Vec<_>>(),
            ["h", "a", "b", "c"]
        );
        assert_eq!(observed.tabbed_session_refs.len(), 4);
        group.maximized_pane_id = Some(pane_id(7));
        let observed = group.observed_layout();
        assert_eq!(observed.live_panes, 1);
        assert_eq!(observed.visible_session_refs[0].id, "h");
        assert_eq!(observed.tabbed_session_refs.len(), 7);
    }

    #[test]
    fn automatic_team_layout_adapts_to_width_without_losing_the_selected_tab() {
        let mut group = preset_group(&["lead", "a", "b"]);
        assert_eq!(
            group.set_automatic_layout(automatic_intent(), 80.0).preset,
            SessionGroupLayoutPreset::Columns
        );
        assert_eq!(group.layout.pane_count(), 3);
        assert!(group.adapt_layout_to_width(44.0));
        assert_eq!(
            group.observed_layout().preset,
            SessionGroupLayoutPreset::Tabs
        );
        assert_eq!(group.live_session_ids(), ["lead"]);
        assert!(group.layout.focus_session(SESSION_GROUP_MAIN_PANE_ID, "b"));
        assert!(!group.adapt_layout_to_width(45.0));
        assert_eq!(group.live_session_ids(), ["b"]);
        assert!(group.adapt_layout_to_width(80.0));
        assert_eq!(group.focused_session_id().as_deref(), Some("b"));
        group.add_members(&["c".into()]);
        assert!(group.adapt_layout_to_width(80.0));
        assert_eq!(
            group.observed_layout().preset,
            SessionGroupLayoutPreset::Grid
        );
        assert_eq!(group.live_session_ids().len(), 4);
        assert_eq!(group.focused_session_id().as_deref(), Some("b"));
    }

    #[test]
    fn automatic_layout_restores_the_selected_tab_and_manual_layout_stays_owned_by_the_user() {
        let mut group = preset_group(&["lead", "a", "b", "c"]);
        group.set_automatic_layout(automatic_intent(), 40.0);
        group.layout.focus_session(SESSION_GROUP_MAIN_PANE_ID, "c");
        let mut restored: SessionGroupUiState =
            serde_json::from_str(&serde_json::to_string(&group).unwrap()).unwrap();
        restored.normalize();
        assert!(!restored.needs_layout_adaptation(41.0));
        assert!(!restored.adapt_layout_to_width(41.0));
        assert_eq!(restored.live_session_ids(), ["c"]);
        restored.disable_automatic_layout();
        let manual = restored.layout.clone();
        assert!(!restored.adapt_layout_to_width(100.0));
        assert_eq!(restored.layout, manual);
    }

    #[test]
    fn review_ready_confirmed_failure_and_cancellation_are_distinct_team_counts() {
        use vibex_core::{
            AgentSessionState, DelegationCompletionPolicy, DelegationTaskPhase, SessionTreeNode,
        };
        let mut node = SessionTreeNode {
            session_ref: VibexUseRef::session(&vibex_core::VibexSessionId::new()),
            parent_session_ref: None,
            title: "Worker".into(),
            agent_id: None,
            agent_label: None,
            task_ref: None,
            task_title: None,
            task_phase: None,
            completion_policy: Some(DelegationCompletionPolicy::OwnerReview),
            child_count: 0,
            has_more_children: false,
            blocked_on: None,
            active_descendants: 0,
            blocked_descendants: 0,
            current_task_ref: None,
            updated_at_ms: 0,
        };
        let mut status = SessionGroupTeamStatus::default();
        for phase in [
            DelegationTaskPhase::AwaitingReview,
            DelegationTaskPhase::Completed,
            DelegationTaskPhase::Failed,
            DelegationTaskPhase::Cancelled,
        ] {
            node.task_phase = Some(phase);
            status.record(Some(&node), AgentSessionState::Idle);
        }
        node.task_phase = Some(DelegationTaskPhase::Completed);
        node.completion_policy = Some(DelegationCompletionPolicy::SingleTurnLegacy);
        status.record(Some(&node), AgentSessionState::Idle);
        status.record(None, AgentSessionState::Running);
        status.record(None, AgentSessionState::NeedsInput);
        assert_eq!(
            (
                status.review,
                status.results_ready,
                status.completed,
                status.failed,
                status.cancelled,
                status.running,
                status.waiting
            ),
            (1, 1, 1, 1, 1, 1, 1)
        );
    }
}
