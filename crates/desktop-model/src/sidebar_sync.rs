//! Wire translation for the sidebar tree.
//!
//! The Desktop owns the sidebar organization; compact clients mirror it. Both
//! shells go through this module so a phone renders the tree the user arranged
//! on the Desktop, and a phone-originated drag lands through exactly the same
//! [`SidebarOrganizationState`] transitions a Desktop drag would take.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{
    MessageAttachment, RemoteSessionGroup, RemoteSidebarDropPosition, RemoteSidebarFolder,
    RemoteSidebarHierarchyMode, RemoteSidebarItemKind, RemoteSidebarItemRef,
    RemoteSidebarNewSessionLocation, RemoteSidebarOrganizationMutation,
    RemoteSidebarOrganizationSnapshot, RemoteSidebarPlacement, RemoteSidebarProjectAppearance,
    RemoteWorktreeLifecycle,
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
    /// Where each managed Worktree stands, by workspace id. The Desktop reads
    /// this from its workspace contexts; a client that cannot see git draws the
    /// same states from here.
    pub worktree_lifecycle: BTreeMap<String, crate::WorktreeLifecycleDisplayState>,
    pub project_location_preferences: BTreeMap<String, NewSessionLocation>,
    pub auto_continue_enabled_by_default: bool,
    pub auto_continue_disabled_project_ids: BTreeSet<String>,
    pub auto_continue_project_ids: BTreeSet<String>,
    pub auto_continue_session_overrides: BTreeMap<String, bool>,
    pub auto_continue_session_ids: BTreeSet<String>,
    pub auto_continue_paused_session_ids: BTreeSet<String>,
    pub unread_session_ids: BTreeSet<String>,
    /// The messages the holder is waiting to send at a later hour.
    ///
    /// A schedule is not arrangement — it does not move a row or change what a
    /// collapse hides — but it is the same kind of fact as the unread badge
    /// beside it: state the reader set that every client has to be able to
    /// show, and that only the holder can act on. It travels here because this
    /// is the one view a client already polls for the marks it draws on a
    /// session row, and a second channel would only be a second thing to keep
    /// in step.
    pub scheduled_sends: Vec<SidebarScheduledSend>,
    /// The holder's whole pre-send queue, oldest first.
    ///
    /// A schedule is the subset of this that carries an hour; the rest waits for
    /// its session to be ready. Both are the reader's own written-ahead work, so
    /// a client that can see one and not the other shows a queue the holder does
    /// not have. Publishing the whole list is what lets a compact client draw
    /// that queue instead of keeping a second one of its own beside it.
    pub queued_sends: Vec<SidebarQueuedSend>,
    /// Sessions whose queue the reader has paused.
    pub paused_queue_session_ids: BTreeSet<String>,
    /// Sessions whose queue releases messages only when the reader says so.
    pub manual_queue_session_ids: BTreeSet<String>,
}

/// Moves one entry inside its session's block of `queue`.
///
/// The queue is one ordered list across sessions, so a reorder is expressed
/// within the block that session owns: a client dragging a row in one session's
/// panel never resequences another session's messages, which it was not shown.
/// `before` names the entry it lands before, or `None` for the end of the block.
///
/// Answers whether the order actually changed. A drop that rebuilds the block
/// exactly as it was is a no-op the caller reports as already applied — a drag
/// that never left its row must not ask the holder for a round trip.
///
/// An unknown message, or a `before` anchor outside the same block, is a
/// rejection: the client acted on a queue the holder has since changed.
fn move_send_within_block<T>(
    queue: &mut Vec<T>,
    session_id: &str,
    message_id: u64,
    before: Option<u64>,
    id: impl Fn(&T) -> u64,
    session: impl Fn(&T) -> &str,
) -> Result<bool, SidebarMutationRejection> {
    let positions: Vec<usize> = queue
        .iter()
        .enumerate()
        .filter(|(_, entry)| session(entry) == session_id)
        .map(|(index, _)| index)
        .collect();
    let Some(from) = positions
        .iter()
        .position(|index| id(&queue[*index]) == message_id)
    else {
        return Err(SidebarMutationRejection::Rejected);
    };
    let target = match before {
        Some(anchor) => positions
            .iter()
            .position(|index| id(&queue[*index]) == anchor)
            .ok_or(SidebarMutationRejection::Rejected)?,
        None => positions.len(),
    };
    // Dropping an entry back where it already is changes nothing. `target`
    // counts the other entries, so the position it came from is its own.
    if target == from {
        return Ok(false);
    }
    // The block's start, so a move inside a session that holds one message
    // cannot drag it out of that session's place in the list.
    let block_start = positions[0];
    let entry = queue.remove(positions[from]);
    let remaining: Vec<usize> = queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| session(queued) == session_id)
        .map(|(index, _)| index)
        .collect();
    let insert_at = match remaining.get(target) {
        Some(index) => *index,
        // Past the end of the block, and never past the point where the next
        // session's block begins: appending to the whole list would put this
        // session's message behind another session's, which the reader never
        // asked for.
        None => remaining
            .last()
            .map_or(block_start.min(queue.len()), |last| last + 1),
    };
    if insert_at == positions[from].min(queue.len()) {
        // The list would be rebuilt exactly as it was: an entry that was
        // already last in its block dropped at the end of it again.
        let same_index = positions[from].saturating_sub(usize::from(insert_at < positions[from]));
        if insert_at == same_index {
            queue.insert(insert_at, entry);
            return Ok(false);
        }
    }
    queue.insert(insert_at, entry);
    Ok(true)
}

/// Moves one entry inside its session's block of the persisted queue.
pub fn move_queued_send_relative(
    queue: &mut Vec<crate::ComposerQueueEntry>,
    session_id: &str,
    message_id: u64,
    before_message_id: Option<u64>,
) -> bool {
    move_send_within_block(
        queue,
        session_id,
        message_id,
        before_message_id,
        |entry| entry.id,
        |entry| entry.session_id.as_str(),
    )
    .unwrap_or(false)
}

/// One message in the holder's pre-send queue, as a client draws it.
///
/// The runtime selection and command invocation the holder keeps with its own
/// entries are absent on purpose: a client did not build this queue, and a
/// message it hands back is addressed by its id. What travels is what a row
/// draws and what an edit carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarQueuedSend {
    pub message_id: u64,
    pub session_id: String,
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
    pub mentions: Vec<vibex_core::VibexUseMention>,
    /// The deadline of a scheduled message; `None` waits only for the session.
    pub scheduled_at_ms: Option<i64>,
}

/// One message waiting for its hour, as the holder publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarScheduledSend {
    pub session_id: String,
    /// The holder's queue entry, which is what an edit or a cancellation
    /// addresses.
    pub message_id: u64,
    pub text: String,
    /// What the message will send with it. The row's title rule reads these the
    /// way the holder does: a message named by its first attachment has to be
    /// named the same on both shells.
    pub attachments: Vec<MessageAttachment>,
    pub scheduled_at_ms: i64,
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

/// The result of a client-originated change. A change that is legal but
/// already reflected in the tree — collapsing a project that is collapsed,
/// dropping a row back where it came from — succeeds as a no-op instead of
/// reporting a rejection the user cannot act on; the compact client and the
/// Desktop already agree about what the sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMutationOutcome {
    /// The tree changed; the Desktop persists and republishes the snapshot.
    Applied(SidebarMutationEffect),
    /// The requested state already held; answering with the current snapshot
    /// is enough for the client to resynchronize.
    AlreadyApplied,
}

/// Move, or take away, the hour one queued message waits for.
///
/// The message stays where it is in the queue and keeps its words; only the
/// clock it waits on changes. `None` is a message that is no longer scheduled —
/// it goes out when its session is next ready, which is what cancelling a
/// deadline means. Answers whether the queue held that message at all: a
/// deadline that arrived for a message the holder has already sent is not a
/// change to make, it is a change that has already happened.
pub fn set_queued_send_schedule(
    queue: &mut [crate::ComposerQueueEntry],
    session_id: &str,
    message_id: u64,
    scheduled_at_ms: Option<i64>,
) -> bool {
    let Some(entry) = queue
        .iter_mut()
        .find(|entry| entry.session_id == session_id && entry.id == message_id)
    else {
        return false;
    };
    entry.scheduled_at_ms = scheduled_at_ms;
    true
}

fn item_to_remote(item: &SidebarOrganizationItem) -> RemoteSidebarItemRef {
    let (kind, id) = match item {
        SidebarOrganizationItem::Folder(id) => (RemoteSidebarItemKind::Folder, id),
        SidebarOrganizationItem::Project(id) => (RemoteSidebarItemKind::Project, id),
        SidebarOrganizationItem::Session(id) => (RemoteSidebarItemKind::Session, id),
        SidebarOrganizationItem::Group(id) => (RemoteSidebarItemKind::Group, id),
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
        RemoteSidebarItemKind::Group => SidebarOrganizationItem::Group(item.id.clone()),
    }
}

impl From<crate::WorktreeLifecycleDisplayState> for RemoteWorktreeLifecycle {
    fn from(state: crate::WorktreeLifecycleDisplayState) -> Self {
        use crate::WorktreeLifecycleDisplayState as State;
        match state {
            State::Working => Self::Working,
            State::Reviewing => Self::Reviewing,
            State::Ready => Self::Ready,
            State::Queued => Self::Queued,
            State::Merging => Self::Merging,
            State::NeedsResolution => Self::NeedsResolution,
            State::Aborting => Self::Aborting,
            State::Archiving => Self::Archiving,
            State::Archived => Self::Archived,
            State::Restoring => Self::Restoring,
            State::Discarding => Self::Discarding,
            State::Discarded => Self::Discarded,
            State::Failed => Self::Failed,
            State::NeedsAttention => Self::NeedsAttention,
        }
    }
}

impl From<RemoteWorktreeLifecycle> for crate::WorktreeLifecycleDisplayState {
    fn from(state: RemoteWorktreeLifecycle) -> Self {
        use RemoteWorktreeLifecycle as Wire;
        match state {
            Wire::Working => Self::Working,
            Wire::Reviewing => Self::Reviewing,
            Wire::Ready => Self::Ready,
            Wire::Queued => Self::Queued,
            Wire::Merging => Self::Merging,
            Wire::NeedsResolution => Self::NeedsResolution,
            Wire::Aborting => Self::Aborting,
            Wire::Archiving => Self::Archiving,
            Wire::Archived => Self::Archived,
            Wire::Restoring => Self::Restoring,
            Wire::Discarding => Self::Discarding,
            Wire::Discarded => Self::Discarded,
            Wire::Failed => Self::Failed,
            Wire::NeedsAttention => Self::NeedsAttention,
        }
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
            groups: self
                .organization
                .groups
                .iter()
                .map(|(id, group)| RemoteSessionGroup {
                    id: id.clone(),
                    name: group.name.clone(),
                    project_id: group.project_id.clone(),
                    workspace_id: group.workspace_id.clone(),
                    member_session_ids: group.member_session_ids.clone(),
                    pinned: group.pinned,
                    auto_continue: group.auto_continue,
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
            collapsed_group_ids: self
                .organization
                .collapsed_group_ids
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
            worktree_lifecycle: self
                .worktree_lifecycle
                .iter()
                .map(|(id, state)| (id.clone(), (*state).into()))
                .collect(),
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
            auto_continue_default_enabled: self.auto_continue_enabled_by_default,
            auto_continue_disabled_project_ids: self
                .auto_continue_disabled_project_ids
                .iter()
                .cloned()
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
            queued_sends: self
                .queued_sends
                .iter()
                .map(|send| vibex_core::RemoteQueuedSend {
                    message_id: send.message_id,
                    session_id: send.session_id.clone(),
                    text: send.text.clone(),
                    attachments: send.attachments.clone(),
                    mentions: send.mentions.clone(),
                    scheduled_at_ms: send.scheduled_at_ms,
                })
                .collect(),
            paused_queue_session_ids: self.paused_queue_session_ids.iter().cloned().collect(),
            manual_queue_session_ids: self.manual_queue_session_ids.iter().cloned().collect(),
            scheduled_sends: self
                .scheduled_sends
                .iter()
                .map(|send| vibex_core::RemoteScheduledSend {
                    session_id: send.session_id.clone(),
                    message_id: send.message_id,
                    text: send.text.clone(),
                    attachments: send.attachments.clone(),
                    scheduled_at_ms: send.scheduled_at_ms,
                })
                .collect(),
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
            groups: snapshot
                .groups
                .iter()
                .map(|group| {
                    (
                        group.id.clone(),
                        crate::SessionGroupUiState::new(
                            group.name.clone(),
                            group.project_id.clone(),
                            group.workspace_id.clone(),
                            group.member_session_ids.clone(),
                        ),
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
            collapsed_group_ids: snapshot.collapsed_group_ids.iter().cloned().collect(),
        };
        for group in &snapshot.groups {
            if let Some(state) = organization.groups.get_mut(&group.id) {
                state.pinned = group.pinned;
                state.auto_continue = group.auto_continue;
            }
        }
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
            worktree_lifecycle: snapshot
                .worktree_lifecycle
                .iter()
                .map(|(id, state)| (id.clone(), (*state).into()))
                .collect(),
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
            auto_continue_enabled_by_default: snapshot.auto_continue_default_enabled,
            auto_continue_disabled_project_ids: snapshot
                .auto_continue_disabled_project_ids
                .iter()
                .cloned()
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
            queued_sends: snapshot
                .queued_sends
                .iter()
                .map(|send| SidebarQueuedSend {
                    message_id: send.message_id,
                    session_id: send.session_id.clone(),
                    text: send.text.clone(),
                    attachments: send.attachments.clone(),
                    mentions: send.mentions.clone(),
                    scheduled_at_ms: send.scheduled_at_ms,
                })
                .collect(),
            paused_queue_session_ids: snapshot.paused_queue_session_ids.iter().cloned().collect(),
            manual_queue_session_ids: snapshot.manual_queue_session_ids.iter().cloned().collect(),
            scheduled_sends: snapshot
                .scheduled_sends
                .iter()
                .map(|send| SidebarScheduledSend {
                    session_id: send.session_id.clone(),
                    message_id: send.message_id,
                    text: send.text.clone(),
                    attachments: send.attachments.clone(),
                    scheduled_at_ms: send.scheduled_at_ms,
                })
                .collect(),
        }
    }

    /// The arrangement a Desktop persisted to its UI state, as a view.
    ///
    /// A shell that owns the runtime but has no Desktop attached — the
    /// character-grid client on a machine where the app is not running — reads
    /// the arrangement from the file the app writes, so both surfaces draw the
    /// tree the reader arranged rather than two different ones. The persisted
    /// auto-continue preferences travel with it: they are the reader's, not the
    /// Desktop's, and a session the Desktop would continue by itself must be
    /// continued by whichever shell is running. The scheduled sends travel with
    /// it for the same reason: they are the reader's, they are persisted, and a
    /// shell that took the home over from a Desktop that is no longer running
    /// must still be able to say which sessions are waiting on an hour. Only
    /// what has no persisted form — unread badges, in-flight countdowns — comes
    /// back empty.
    pub fn from_desktop_ui_state(state: &crate::DesktopUiStateV1) -> Self {
        let sidebar = &state.sidebar;
        let mut view = Self {
            revision: 0,
            organization: sidebar.organization.clone(),
            collapsed_project_ids: sidebar.collapsed_project_ids.clone(),
            collapsed_workspace_ids: sidebar.collapsed_workspace_ids.clone(),
            pinned_session_ids: sidebar.pinned_session_ids.clone(),
            session_order: sidebar.session_order.clone(),
            session_order_anchored_at_ms: sidebar.session_order_anchored_at_ms,
            hierarchy_mode: sidebar.hierarchy_mode,
            project_order: sidebar.project_order.clone(),
            workspace_order: sidebar.workspace_order.clone(),
            project_appearances: sidebar.project_appearances.clone(),
            worktree_titles: sidebar.worktree_titles.clone(),
            // Where a Worktree stands is live state, not an arrangement, so a
            // shell that reads a parked arrangement has none of it until its
            // own git layer answers.
            worktree_lifecycle: BTreeMap::new(),
            project_location_preferences: sidebar.project_location_preferences.clone(),
            auto_continue_enabled_by_default: state.session.auto_continue_enabled_by_default,
            auto_continue_disabled_project_ids: state
                .session
                .auto_continue_disabled_project_ids
                .clone(),
            auto_continue_project_ids: state.session.auto_continue_project_ids.clone(),
            auto_continue_session_overrides: state.session.auto_continue_session_overrides.clone(),
            // The effective set is derived, not persisted: the Desktop
            // materialises a project default into an override when it enables
            // one, and these are the overrides that are still on.
            auto_continue_session_ids: state
                .session
                .auto_continue_session_overrides
                .iter()
                .filter(|(_, enabled)| **enabled)
                .map(|(session_id, _)| session_id.clone())
                .collect(),
            auto_continue_paused_session_ids: state
                .session
                .auto_continue_paused_session_ids
                .clone(),
            unread_session_ids: BTreeSet::new(),
            queued_sends: state
                .composer
                .queue
                .iter()
                .map(|entry| SidebarQueuedSend {
                    message_id: entry.id,
                    session_id: entry.session_id.clone(),
                    text: entry.text.clone(),
                    attachments: entry.attachments.clone(),
                    mentions: entry.mentions.clone(),
                    scheduled_at_ms: entry.scheduled_at_ms,
                })
                .collect(),
            // The send mode is a persisted preference, but it is stored for the
            // whole shell rather than per session. A shell reading a parked
            // arrangement therefore publishes it for every session it knows,
            // which is what the Desktop does when it materializes its own set
            // from the same field.
            manual_queue_session_ids: match state.composer.queue_send_mode {
                crate::ComposerQueueSendMode::Manual => state
                    .composer
                    .queue
                    .iter()
                    .map(|entry| entry.session_id.clone())
                    .collect(),
                crate::ComposerQueueSendMode::Automatic => BTreeSet::new(),
            },
            // A pause is live state: the Desktop sets it when the reader stops
            // a turn and lifts it explicitly, so a shell that only reads the
            // file has none of it.
            paused_queue_session_ids: BTreeSet::new(),
            scheduled_sends: state
                .composer
                .queue
                .iter()
                .filter_map(|entry| {
                    Some(SidebarScheduledSend {
                        session_id: entry.session_id.clone(),
                        message_id: entry.id,
                        text: entry.text.clone(),
                        attachments: entry.attachments.clone(),
                        scheduled_at_ms: entry.scheduled_at_ms?,
                    })
                })
                .collect(),
        };
        view.refresh_revision();
        view
    }

    /// Write this arrangement back into the UI state it came from.
    ///
    /// The inverse of [`Self::from_desktop_ui_state`], and the only writer a
    /// shell without a Desktop attached has. The auto-continue preferences are
    /// the reader's, so they are written back too; the effective set is derived
    /// on the next read. Live-only fields (unread badges, in-flight countdowns)
    /// are the Desktop's and are left alone: this call owns the tree, not the
    /// badges.
    pub fn apply_to_desktop_ui_state(&self, state: &mut crate::DesktopUiStateV1) {
        let sidebar = &mut state.sidebar;
        sidebar.organization = self.organization.clone();
        sidebar.collapsed_project_ids = self.collapsed_project_ids.clone();
        sidebar.collapsed_workspace_ids = self.collapsed_workspace_ids.clone();
        sidebar.pinned_session_ids = self.pinned_session_ids.clone();
        sidebar.session_order = self.session_order.clone();
        sidebar.session_order_anchored_at_ms = self.session_order_anchored_at_ms;
        sidebar.hierarchy_mode = self.hierarchy_mode;
        sidebar.project_order = self.project_order.clone();
        sidebar.workspace_order = self.workspace_order.clone();
        sidebar.worktree_titles = self.worktree_titles.clone();
        sidebar.project_location_preferences = self.project_location_preferences.clone();
        sidebar.project_appearances = self.project_appearances.clone();
        state.session.auto_continue_enabled_by_default = self.auto_continue_enabled_by_default;
        state.session.auto_continue_disabled_project_ids =
            self.auto_continue_disabled_project_ids.clone();
        state.session.auto_continue_project_ids = self.auto_continue_project_ids.clone();
        state.session.auto_continue_session_overrides =
            self.auto_continue_session_overrides.clone();
        state.session.auto_continue_paused_session_ids =
            self.auto_continue_paused_session_ids.clone();
        // The queue's content and the reader's send mode are written back for
        // the same reason the arrangement is: a shell that took the home over
        // from a Desktop that is no longer running owns this queue now, and the
        // edits a client made through it have to survive the handback.
        //
        // Entries the view does not carry are left alone rather than dropped:
        // the view publishes what a client may edit — words, files, mentions,
        // and an hour — not the runtime selection and command a holder keeps
        // with its own entry, so rewriting the whole list from here would lose
        // them. Removals were already applied to the file by the mutation that
        // asked for them.
        for published in &self.queued_sends {
            let Some(entry) = state
                .composer
                .queue
                .iter_mut()
                .find(|entry| entry.id == published.message_id)
            else {
                continue;
            };
            entry.text = published.text.clone();
            entry.attachments = published.attachments.clone();
            entry.mentions = published.mentions.clone();
            entry.scheduled_at_ms = published.scheduled_at_ms;
        }
        // The send mode is one shell-wide preference, so it reads as manual
        // when any session the view publishes is manual. A set holding some
        // sessions and not others has no single persisted form, and the Desktop
        // materializes it for every session the moment it loads — this is that
        // same rule, applied on the way out.
        state.composer.queue_send_mode = if self.manual_queue_session_ids.is_empty() {
            crate::ComposerQueueSendMode::Automatic
        } else {
            crate::ComposerQueueSendMode::Manual
        };
    }

    /// Whether this arrangement says anything at all about where rows go.
    ///
    /// A reader who has never arranged the sidebar has an empty organization:
    /// no folders, no manual order, no pins. Such a view carries no
    /// information, and a client that adopted it would replace its own
    /// (recency-and-project) ordering with an arbitrary id order — so callers
    /// keep their fallback until there is an arrangement to mirror.
    pub fn arranges_anything(&self) -> bool {
        !self.organization.folders.is_empty()
            || !self.organization.groups.is_empty()
            || !self.organization.placements.is_empty()
            || !self.collapsed_project_ids.is_empty()
            || !self.collapsed_workspace_ids.is_empty()
            || !self.pinned_session_ids.is_empty()
            || !self.session_order.is_empty()
            || !self.project_order.is_empty()
            || !self.workspace_order.is_empty()
            || !self.organization.collapsed_folder_ids.is_empty()
    }

    /// Whether a session of `project_id` starts with auto-continue on.
    ///
    /// The shell-wide default covers every project, including projects the
    /// reader creates later; a project's own explicit choice is the stronger
    /// statement and wins over it.
    pub fn auto_continue_project_default(&self, project_id: &str) -> bool {
        if self.auto_continue_project_ids.contains(project_id) {
            return true;
        }
        if self.auto_continue_disabled_project_ids.contains(project_id) {
            return false;
        }
        self.auto_continue_enabled_by_default
    }

    /// Whether a session keeps its pane's group continuing it.
    ///
    /// A group's own choice is the narrowest one it has; without one it follows
    /// the project, which is what lets the shell-wide default reach a group the
    /// reader never touched.
    pub fn auto_continue_group_enabled(
        &self,
        group_choice: Option<bool>,
        project_id: &str,
    ) -> bool {
        group_choice.unwrap_or_else(|| self.auto_continue_project_default(project_id))
    }

    /// Whether the authority holds auto-continue on for one session.
    ///
    /// A session override is the reader's last word on that row; without one
    /// the session follows its project's default. The effective set the holder
    /// publishes comes next, so a client that was only told the ids still
    /// paints the same marks.
    pub fn auto_continue_session_enabled(&self, session_id: &str, project_id: &str) -> bool {
        if let Some(enabled) = self.auto_continue_session_overrides.get(session_id) {
            return *enabled;
        }
        // The holder's effective set already folded the defaults into the ids,
        // so it is the next best answer for a row this view has no project for.
        self.auto_continue_session_ids.contains(session_id)
            || self.auto_continue_project_default(project_id)
    }

    /// Move, or take away, the hour one published send waits for.
    ///
    /// The message belongs to the holder's queue, so the view can only answer
    /// for what it published: a send this client was never told about is one it
    /// cannot reschedule or cancel, and guessing would arm a message the holder
    /// does not have. The holder applies the change to the queue itself — this
    /// only reports that the hour moved, and takes a cancelled deadline out of
    /// the published list without touching the message, which is still the
    /// queue's to send when its session is next ready.
    fn set_scheduled_send(
        &mut self,
        session_id: &str,
        message_id: u64,
        scheduled_at_ms: Option<i64>,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let Some(send) = self
            .scheduled_sends
            .iter_mut()
            .find(|send| send.session_id == session_id && send.message_id == message_id)
        else {
            return Err(SidebarMutationRejection::Rejected);
        };
        match scheduled_at_ms {
            Some(at_ms) if at_ms == send.scheduled_at_ms => {
                Ok(SidebarMutationOutcome::AlreadyApplied)
            }
            Some(at_ms) => {
                send.scheduled_at_ms = at_ms;
                Ok(SidebarMutationEffect::SCHEDULED.into())
            }
            None => {
                self.scheduled_sends.retain(|send| {
                    !(send.session_id == session_id && send.message_id == message_id)
                });
                Ok(SidebarMutationEffect::SCHEDULED.into())
            }
        }
    }

    /// Rewrites one published queue entry in place.
    ///
    /// The words and the files change; the deadline and the position do not.
    /// Editing what a message says is not an instruction about when it goes
    /// out, and the queue's order is the reader's own arrangement — moving a
    /// message because it was edited would reorder a list they set on purpose.
    /// A message the view was never told about is one it cannot rewrite, so an
    /// unknown id is a rejection rather than a silent insert: the holder would
    /// otherwise receive an edit for a message it does not have.
    fn set_queued_send(
        &mut self,
        session_id: &str,
        message_id: u64,
        text: &str,
        attachments: &[MessageAttachment],
        mentions: &[vibex_core::VibexUseMention],
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let Some(send) = self
            .queued_sends
            .iter_mut()
            .find(|send| send.session_id == session_id && send.message_id == message_id)
        else {
            return Err(SidebarMutationRejection::Rejected);
        };
        if send.text == text && send.attachments == attachments && send.mentions == mentions {
            return Ok(SidebarMutationOutcome::AlreadyApplied);
        }
        send.text = text.to_string();
        send.attachments = attachments.to_vec();
        send.mentions = mentions.to_vec();
        // A rewritten message that carries no schedule is still the same entry;
        // its published deadline is untouched because this mutation never spoke
        // about it.
        Ok(SidebarMutationEffect::QUEUE.into())
    }

    /// Drops one published queue entry.
    ///
    /// The message never reached the authority, so this is a change to the
    /// holder's own list. A message that is no longer in the queue has already
    /// left it — the reader asked for it to be gone, and it is — so an unknown
    /// id is a no-op rather than a rejection a client could do nothing about.
    fn remove_queued_send(
        &mut self,
        session_id: &str,
        message_id: u64,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let before = self.queued_sends.len();
        self.queued_sends
            .retain(|send| !(send.session_id == session_id && send.message_id == message_id));
        // The schedule index is the same fact seen from the row's side; a
        // cancelled entry must not keep counting down to an hour nothing
        // happens at.
        self.scheduled_sends
            .retain(|send| !(send.session_id == session_id && send.message_id == message_id));
        if self.queued_sends.len() == before {
            return Ok(SidebarMutationOutcome::AlreadyApplied);
        }
        Ok(SidebarMutationEffect::QUEUE.into())
    }

    fn set_queue_paused(
        &mut self,
        session_id: &str,
        paused: bool,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let changed = if paused {
            self.paused_queue_session_ids.insert(session_id.to_string())
        } else {
            self.paused_queue_session_ids.remove(session_id)
        };
        if !changed {
            return Ok(SidebarMutationOutcome::AlreadyApplied);
        }
        Ok(SidebarMutationEffect::QUEUE.into())
    }

    fn set_queue_manual(
        &mut self,
        session_id: &str,
        manual: bool,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let changed = if manual {
            self.manual_queue_session_ids.insert(session_id.to_string())
        } else {
            self.manual_queue_session_ids.remove(session_id)
        };
        if !changed {
            return Ok(SidebarMutationOutcome::AlreadyApplied);
        }
        Ok(SidebarMutationEffect::QUEUE.into())
    }

    /// Notes one message a client asked the holder to hold.
    ///
    /// The view cannot accept the message itself — the queue, with the runtime
    /// selection it will send with, is the holder's — so this publishes the
    /// entry the client will see once the holder has taken it. The id is
    /// zero-until-assigned: the holder renumbers it, and the client's next
    /// snapshot carries the real one.
    fn enqueue_queued_send(
        &mut self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        mentions: &[vibex_core::VibexUseMention],
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        // Behind everything already waiting for that session, never behind
        // another session's messages: the queue is one list across sessions, and
        // a message written here belongs to its own session's block in it.
        let insert_at = match self
            .queued_sends
            .iter()
            .rposition(|send| send.session_id == session_id)
        {
            Some(last) => last + 1,
            None => self
                .queued_sends
                .iter()
                .position(|send| send.session_id.as_str() > session_id)
                .unwrap_or(self.queued_sends.len()),
        };
        self.queued_sends.insert(
            insert_at,
            SidebarQueuedSend {
                message_id: 0,
                session_id: session_id.to_string(),
                text: text.to_string(),
                attachments: attachments.to_vec(),
                mentions: mentions.to_vec(),
                scheduled_at_ms: None,
            },
        );
        Ok(SidebarMutationEffect::ENQUEUE.into())
    }

    /// Moves one published entry to a new place inside its session's block.
    ///
    /// The queue as a whole is the holder's order, so only the entries that
    /// belong to the named session move: dropping a row in one session's panel
    /// must not resequence another session's messages, which the reader was
    /// never shown. An unknown message, or a "before" anchor that is not in the
    /// same block, is a rejection — the client acted on a queue the holder has
    /// since changed.
    fn move_queued_send(
        &mut self,
        session_id: &str,
        message_id: u64,
        before_message_id: Option<u64>,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        if move_send_within_block(
            &mut self.queued_sends,
            session_id,
            message_id,
            before_message_id,
            |send| send.message_id,
            |send| send.session_id.as_str(),
        )? {
            Ok(SidebarMutationEffect::QUEUE.into())
        } else {
            Ok(SidebarMutationOutcome::AlreadyApplied)
        }
    }

    /// Whether one session's queue advances on its own.
    pub fn queue_sends_automatically(&self, session_id: &str) -> bool {
        !self.manual_queue_session_ids.contains(session_id)
    }

    /// Whether one session's queue is held where the reader left it.
    pub fn queue_is_paused(&self, session_id: &str) -> bool {
        self.paused_queue_session_ids.contains(session_id)
    }

    /// The queue entries one session is holding, oldest first.
    pub fn queued_sends_for<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Iterator<Item = &'a SidebarQueuedSend> {
        self.queued_sends
            .iter()
            .filter(move |send| send.session_id == session_id)
    }

    /// The next send one session is waiting for, among those the holder
    /// published.
    ///
    /// A session can hold more than one scheduled message — the reader queued
    /// them, and the queue is ordered by what they wrote — so the row's
    /// countdown reads the one that goes out first. The others keep their own
    /// deadlines with the holder; a row that counted down to the last of them
    /// would name an hour nothing happens at.
    pub fn scheduled_send_for(&self, session_id: &str) -> Option<&SidebarScheduledSend> {
        self.scheduled_sends
            .iter()
            .filter(|send| send.session_id == session_id)
            .min_by_key(|send| send.scheduled_at_ms)
    }

    /// Content fingerprint of the tree, for optimistic-concurrency checks.
    ///
    /// Only layout-bearing state participates. A move is revalidated against
    /// the full tree by whoever applies it anyway, so folding pure display
    /// state — unread badges, auto-continue markers, per-session activity
    /// timestamps that move a row without the user arranging anything — into
    /// the revision would reject moves that are still valid, surfacing errors
    /// during ordinary streaming.
    pub fn refresh_revision(&mut self) {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.organization.hash(&mut hasher);
        self.session_order.hash(&mut hasher);
        self.session_order_anchored_at_ms.hash(&mut hasher);
        self.project_order.hash(&mut hasher);
        self.workspace_order.hash(&mut hasher);
        self.hierarchy_mode.hash(&mut hasher);
        self.revision = hasher.finish();
    }

    /// Applies a client-originated change. `new_folder_id` is supplied by the
    /// caller so id minting stays with the Desktop, which owns the tree.
    /// Returns the fields that changed so callers persist only what moved.
    pub fn apply_remote(
        &mut self,
        mutation: &RemoteSidebarOrganizationMutation,
        session_projects: &BTreeMap<String, String>,
        session_workspaces: &BTreeMap<String, String>,
        new_folder_id: &str,
        new_group_id: &str,
    ) -> Result<SidebarMutationOutcome, SidebarMutationRejection> {
        let outcome = match mutation {
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
                // A legal move that changes nothing — the dropped row already
                // sits in the requested position, which is how a drag that
                // raced a desktop-side edit or dropped in place lands — is an
                // idempotent success, not an error the client cannot act on.
                let moved = match (anchor, position) {
                    (Some(anchor), RemoteSidebarDropPosition::Into) => {
                        let RemoteSidebarItemKind::Folder = anchor.kind else {
                            return Err(SidebarMutationRejection::Rejected);
                        };
                        if !self.organization.can_move_many_into(
                            &moving,
                            &anchor.id,
                            session_projects,
                        ) {
                            return Err(SidebarMutationRejection::Rejected);
                        }
                        self.organization
                            .move_many_into(&moving, &anchor.id, session_projects)
                    }
                    (Some(anchor), position) => {
                        let anchor = item_from_remote(anchor);
                        if !self.organization.can_move_many_relative(
                            &moving,
                            &anchor,
                            session_projects,
                        ) {
                            return Err(SidebarMutationRejection::Rejected);
                        }
                        self.organization.move_many_relative(
                            &moving,
                            &anchor,
                            matches!(position, RemoteSidebarDropPosition::After),
                            session_projects,
                        )
                    }
                    (None, _) => {
                        let scope = project_id.clone().map_or(
                            SidebarOrganizationScope::Root,
                            SidebarOrganizationScope::Project,
                        );
                        if !self.organization.can_move_many_to_scope_root(
                            &moving,
                            &scope,
                            session_projects,
                        ) {
                            return Err(SidebarMutationRejection::Rejected);
                        }
                        self.organization.move_many_to_scope_root_end(
                            &moving,
                            &scope,
                            session_projects,
                        )
                    }
                };
                if moved {
                    SidebarMutationEffect::ORGANIZATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
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
                match move_workspace_ids_relative(
                    order,
                    workspace_ids,
                    anchor_workspace_id.as_deref(),
                    after,
                ) {
                    WorkspaceMoveOutcome::Illegal => {
                        return Err(SidebarMutationRejection::Rejected);
                    }
                    WorkspaceMoveOutcome::Moved => SidebarMutationEffect::NAVIGATION.into(),
                    WorkspaceMoveOutcome::Unchanged => SidebarMutationOutcome::AlreadyApplied,
                }
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
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::RenameFolder { folder_id, name } => {
                let name = name.trim();
                let Some(folder) = self.organization.folders.get(folder_id) else {
                    return Err(SidebarMutationRejection::Rejected);
                };
                if folder.name == name {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                if !self.organization.rename_folder(folder_id, name) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::DeleteFolder { folder_id } => {
                if !self.organization.delete_folder(folder_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION.into()
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
                if changed {
                    SidebarMutationEffect::ORGANIZATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
            }
            RemoteSidebarOrganizationMutation::SetFolderAutoArchive { folder_id, days } => {
                if !self
                    .organization
                    .set_folder_auto_archive_after_days(folder_id, *days)
                {
                    // The window is out of range, or the folder is gone: the
                    // setter refuses both, and a client cannot tell them apart
                    // from here.
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION.into()
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
                if changed {
                    SidebarMutationEffect::NAVIGATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
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
                if changed {
                    SidebarMutationEffect::NAVIGATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
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
                if changed {
                    SidebarMutationEffect::NAVIGATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
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
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                session_ids,
                paused,
            } => {
                for session_id in session_ids {
                    if !session_projects.contains_key(session_id) {
                        return Err(SidebarMutationRejection::Rejected);
                    }
                    if *paused {
                        // Only a session that will continue on its own can be
                        // suspended, which is the rule the Desktop applies when
                        // it suspends one itself.
                        if !self.auto_continue_session_ids.contains(session_id) {
                            continue;
                        }
                        self.auto_continue_paused_session_ids
                            .insert(session_id.clone());
                    } else {
                        self.auto_continue_paused_session_ids.remove(session_id);
                    }
                }
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::EnqueueScheduledSend { session_id, .. } => {
                if !session_projects.contains_key(session_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                // The queue belongs to the shell, so the view has nothing to
                // move: the effect is what tells the shell to take the message.
                SidebarMutationEffect::ENQUEUE.into()
            }
            RemoteSidebarOrganizationMutation::CreateGroup {
                name,
                project_id,
                workspace_id,
                member_session_ids,
                parent_folder_id,
            } => {
                if !self.organization.create_group(
                    new_group_id,
                    name.trim(),
                    project_id,
                    workspace_id,
                    member_session_ids,
                    session_workspaces,
                    parent_folder_id.clone(),
                ) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                if let Some(parent_folder_id) = parent_folder_id {
                    self.organization
                        .collapsed_folder_ids
                        .remove(parent_folder_id);
                }
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::RenameGroup { group_id, name } => {
                let name = name.trim();
                let Some(group) = self.organization.groups.get(group_id) else {
                    return Err(SidebarMutationRejection::Rejected);
                };
                if group.name == name {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                if !self.organization.rename_group(group_id, name) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::DeleteGroup { group_id } => {
                if !self.organization.delete_group(group_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::SetGroupMembers {
                group_id,
                session_ids,
            } => {
                if !self.organization.groups.contains_key(group_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                if !self.organization.set_group_members(
                    group_id,
                    session_ids.clone(),
                    session_workspaces,
                ) {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                SidebarMutationEffect::ORGANIZATION.into()
            }
            RemoteSidebarOrganizationMutation::SetGroupCollapsed {
                group_id,
                collapsed,
            } => {
                if !self.organization.groups.contains_key(group_id) {
                    return Err(SidebarMutationRejection::Rejected);
                }
                let changed = if *collapsed {
                    self.organization
                        .collapsed_group_ids
                        .insert(group_id.clone())
                } else {
                    self.organization.collapsed_group_ids.remove(group_id)
                };
                if changed {
                    SidebarMutationEffect::ORGANIZATION.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
            }
            RemoteSidebarOrganizationMutation::SetGroupPinned { group_id, pinned } => {
                if !self.organization.set_group_pinned(group_id, *pinned) {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::SetGroupAutoContinue { group_id, enabled } => {
                let Some(group) = self.organization.groups.get(group_id) else {
                    return Err(SidebarMutationRejection::Rejected);
                };
                let members = group.member_session_ids.clone();
                if !self
                    .organization
                    .set_group_auto_continue(group_id, Some(*enabled))
                {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                // The group flag fans out into the per-session overrides the
                // runtime loop actually reads, so a compact client's toggle has
                // the same effect as the Desktop's group menu.
                for session_id in members {
                    if *enabled {
                        self.auto_continue_session_ids.insert(session_id.clone());
                    } else {
                        self.auto_continue_session_ids.remove(&session_id);
                    }
                    self.auto_continue_session_overrides
                        .insert(session_id.clone(), *enabled);
                    self.auto_continue_paused_session_ids.remove(&session_id);
                }
                SidebarMutationEffect::NAVIGATION.into()
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
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::SetProjectNewSessionLocation {
                project_id,
                location,
            } => {
                let next = match location {
                    RemoteSidebarNewSessionLocation::NewWorktree => NewSessionLocation::NewWorktree,
                    RemoteSidebarNewSessionLocation::CurrentCheckout => {
                        NewSessionLocation::CurrentCheckout
                    }
                };
                if self.project_location_preferences.get(project_id) == Some(&next) {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                self.project_location_preferences
                    .insert(project_id.clone(), next);
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::SetHierarchyMode { mode } => {
                let next = match mode {
                    RemoteSidebarHierarchyMode::Detailed => SidebarHierarchyMode::Detailed,
                    RemoteSidebarHierarchyMode::Compact => SidebarHierarchyMode::Compact,
                };
                if self.hierarchy_mode == next {
                    return Ok(SidebarMutationOutcome::AlreadyApplied);
                }
                self.hierarchy_mode = next;
                SidebarMutationEffect::NAVIGATION.into()
            }
            RemoteSidebarOrganizationMutation::SetScheduledSend {
                session_id,
                message_id,
                scheduled_at_ms,
            } => self.set_scheduled_send(session_id, *message_id, *scheduled_at_ms)?,
            RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids,
                unread,
            } => self.set_sessions_unread(session_ids, *unread),
            RemoteSidebarOrganizationMutation::SetQueuedSend {
                session_id,
                message_id,
                text,
                attachments,
                mentions,
            } => self.set_queued_send(session_id, *message_id, text, attachments, mentions)?,
            RemoteSidebarOrganizationMutation::RemoveQueuedSend {
                session_id,
                message_id,
            } => self.remove_queued_send(session_id, *message_id)?,
            RemoteSidebarOrganizationMutation::SetQueuePaused { session_id, paused } => {
                self.set_queue_paused(session_id, *paused)?
            }
            RemoteSidebarOrganizationMutation::SetQueueManual { session_id, manual } => {
                self.set_queue_manual(session_id, *manual)?
            }
            RemoteSidebarOrganizationMutation::EnqueueQueuedSend {
                session_id,
                text,
                attachments,
                mentions,
            } => self.enqueue_queued_send(session_id, text, attachments, mentions)?,
            RemoteSidebarOrganizationMutation::MoveQueuedSend {
                session_id,
                message_id,
                before_message_id,
            } => self.move_queued_send(session_id, *message_id, *before_message_id)?,
            // Sending one message now is a dispatch, not a change to the queue's
            // shape: the holder promotes it and hands it over, and the entry
            // leaves there. The view only says that the queue is about to
            // change so a client repaints instead of acting on a stale list.
            RemoteSidebarOrganizationMutation::SendQueuedSendNow {
                session_id,
                message_id,
            } => {
                if self
                    .queued_sends
                    .iter()
                    .any(|send| send.session_id == *session_id && send.message_id == *message_id)
                {
                    SidebarMutationEffect::QUEUE.into()
                } else {
                    SidebarMutationOutcome::AlreadyApplied
                }
            }
        };
        self.revision = self.revision.wrapping_add(1);
        Ok(outcome)
    }

    /// Set or clear the unread mark on the named sessions, reporting whether
    /// anything changed so a mark that already holds is an idempotent no-op
    /// rather than a round trip nobody asked for.
    ///
    /// The mark is the authority's in-memory value, so both directions are one
    /// assignment: a client that read a session clears it, a client that saw a
    /// finished answer for a session nobody was looking at sets it. A session
    /// the view does not list is still stored — the set is pruned when the row
    /// is removed, and an id with no row draws no dot — because requiring the
    /// session to exist would put a database scan on the hot path of every
    /// finished turn just to reject a name that cannot be seen anyway.
    fn set_sessions_unread(
        &mut self,
        session_ids: &[String],
        unread: bool,
    ) -> SidebarMutationOutcome {
        let mut changed = false;
        for session_id in session_ids {
            changed |= if unread {
                self.unread_session_ids.insert(session_id.clone())
            } else {
                self.unread_session_ids.remove(session_id)
            };
        }
        if changed {
            SidebarMutationEffect::UNREAD.into()
        } else {
            SidebarMutationOutcome::AlreadyApplied
        }
    }
}

impl From<SidebarMutationEffect> for SidebarMutationOutcome {
    fn from(effect: SidebarMutationEffect) -> Self {
        Self::Applied(effect)
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
) -> WorkspaceMoveOutcome {
    if moving_ids.is_empty() {
        return WorkspaceMoveOutcome::Illegal;
    }
    let moving_set = moving_ids.iter().collect::<BTreeSet<_>>();
    if moving_set.len() != moving_ids.len()
        || anchor_id.is_some_and(|anchor| moving_ids.iter().any(|id| id == anchor))
        || moving_ids
            .iter()
            .any(|id| !order.iter().any(|candidate| candidate == id))
    {
        return WorkspaceMoveOutcome::Illegal;
    }
    if let Some(anchor) = anchor_id
        && !order.iter().any(|candidate| candidate == anchor)
    {
        return WorkspaceMoveOutcome::Illegal;
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
    if *order == original {
        WorkspaceMoveOutcome::Unchanged
    } else {
        WorkspaceMoveOutcome::Moved
    }
}

/// Distinguishes a workspace reorder that is not legal for the current order
/// from one that is legal but already in the requested arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceMoveOutcome {
    Illegal,
    Moved,
    Unchanged,
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
    let group_ids = organization
        .groups
        .iter()
        .filter(|(_, group)| group.project_id == project_id)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    sidebar_project_items_for_workspace(
        organization,
        project_id,
        None,
        true,
        true,
        session_ids,
        &group_ids,
        pinned_session_ids,
        parent_folder_id,
    )
}

/// Returns children for one workspace in detailed hierarchy mode. Folders
/// created before workspace ownership existed remain visible through the
/// legacy `sidebar_project_items` path; detailed mode only pulls folders that
/// explicitly belong to this workspace. Compact callers may opt into showing
/// all project folders so older clients do not hide newly scoped folders.
#[allow(clippy::too_many_arguments)]
pub fn sidebar_project_items_for_workspace(
    organization: &SidebarOrganizationState,
    project_id: &str,
    workspace_id: Option<&str>,
    include_legacy_project_folders: bool,
    include_all_workspace_folders: bool,
    session_ids: &[String],
    group_ids: &[String],
    pinned_session_ids: &BTreeSet<String>,
    parent_folder_id: Option<&str>,
) -> Vec<SidebarOrganizationItem> {
    // A session inside a group renders under its group row, so it is not also
    // offered as a sibling here. A caller that lists no groups — a compact
    // surface that has not implemented group rows — keeps the members visible
    // as ordinary sessions instead of losing them.
    let listed_group_ids = group_ids.iter().cloned().collect::<BTreeSet<_>>();
    let grouped_session_ids = if listed_group_ids.is_empty() {
        BTreeSet::new()
    } else {
        organization
            .grouped_session_ids()
            .into_iter()
            .filter(|session_id| {
                organization
                    .group_of_session(session_id)
                    .is_some_and(|group_id| listed_group_ids.contains(group_id))
            })
            .collect()
    };
    let session_ids = session_ids
        .iter()
        .filter(|session_id| !grouped_session_ids.contains(session_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
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
    // A group is a leaf like a session, so it participates in the same sibling
    // ordering and can sit inside a folder.
    available.extend(
        group_ids
            .iter()
            .map(|id| SidebarOrganizationItem::Group(id.clone())),
    );
    let ordered = organization.ordered_children(parent_folder_id, &available);
    let (pinned, rest): (Vec<_>, Vec<_>) = ordered.into_iter().partition(|item| match item {
        SidebarOrganizationItem::Session(id) => pinned_session_ids.contains(id),
        SidebarOrganizationItem::Group(id) => organization.group_is_pinned(id),
        _ => false,
    });
    pinned.into_iter().chain(rest).collect()
}

/// Which half of the sidebar view a mutation touched. The Desktop persists the
/// organization tree and the navigation flags through different paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarMutationEffect {
    pub organization: bool,
    pub navigation: bool,
    /// The holder's queue changed: a deadline was moved or taken away, and the
    /// message it names is still the queue's to send.
    pub scheduled: bool,
    /// The holder's queue has a message to take: a client asked for a send at a
    /// later hour, and only the shell that owns the queue can accept it.
    pub enqueue: bool,
    /// The unread marks changed: another client read sessions the authority was
    /// still badging, and its own readers must stop badging them too.
    pub unread: bool,
    /// One entry of the holder's queue was rewritten or dropped, or a queue was
    /// paused, resumed, or switched between automatic and manual.
    ///
    /// This is the queue's own content rather than its schedule, so it reaches
    /// the holder's list instead of the deadline one entry waits on: only the
    /// shell that owns the queue can edit a message it is holding. Resuming and
    /// switching back to automatic also re-evaluate the head, which is why they
    /// are filed here rather than with the read-only marks.
    pub queue: bool,
}

impl SidebarMutationEffect {
    pub const ORGANIZATION: Self = Self {
        organization: true,
        navigation: false,
        scheduled: false,
        enqueue: false,
        unread: false,
        queue: false,
    };
    pub const NAVIGATION: Self = Self {
        organization: false,
        navigation: true,
        scheduled: false,
        enqueue: false,
        unread: false,
        queue: false,
    };
    pub const SCHEDULED: Self = Self {
        organization: false,
        navigation: false,
        scheduled: true,
        enqueue: false,
        unread: false,
        queue: false,
    };
    pub const UNREAD: Self = Self {
        organization: false,
        navigation: false,
        scheduled: false,
        enqueue: false,
        unread: true,
        queue: false,
    };
    pub const ENQUEUE: Self = Self {
        organization: false,
        navigation: false,
        scheduled: false,
        enqueue: true,
        unread: false,
        queue: false,
    };
    pub const QUEUE: Self = Self {
        organization: false,
        navigation: false,
        scheduled: false,
        enqueue: false,
        unread: false,
        queue: true,
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

    fn session_workspaces() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("session-a".to_string(), "workspace-a".to_string()),
            ("session-b".to_string(), "workspace-a".to_string()),
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
    fn a_persisted_arrangement_reads_back_as_the_view_it_was() {
        // A shell that owns the runtime without a Desktop attached reads the
        // arrangement the app persisted; it has to come back as the same tree
        // the app would draw.
        let mut ui_state = crate::DesktopUiStateV1::default();
        ui_state.sidebar.organization.folders.insert(
            "folder-1".to_string(),
            crate::SidebarFolderUiState {
                name: "Archive".to_string(),
                project_id: Some("project-1".to_string()),
                workspace_id: None,
                auto_archive_after_days: Some(3),
            },
        );
        ui_state
            .sidebar
            .organization
            .placements
            .push(crate::SidebarOrganizationPlacement {
                item: SidebarOrganizationItem::Folder("folder-1".to_string()),
                parent_folder_id: None,
            });
        ui_state
            .sidebar
            .organization
            .collapsed_folder_ids
            .insert("folder-1".to_string());
        ui_state.sidebar.project_order = vec!["project-1".to_string()];
        ui_state.sidebar.session_order = vec!["session-a".to_string()];
        ui_state.sidebar.session_order_anchored_at_ms = 42;
        ui_state
            .sidebar
            .pinned_session_ids
            .insert("session-a".to_string());
        ui_state
            .sidebar
            .collapsed_project_ids
            .insert("project-2".to_string());
        ui_state.sidebar.hierarchy_mode = SidebarHierarchyMode::Detailed;
        // Auto-continue is the reader's preference and travels with the tree.
        ui_state
            .session
            .auto_continue_project_ids
            .insert("project-1".to_string());
        ui_state
            .session
            .auto_continue_session_overrides
            .insert("session-a".to_string(), true);
        ui_state
            .session
            .auto_continue_session_overrides
            .insert("session-b".to_string(), false);
        ui_state
            .session
            .auto_continue_paused_session_ids
            .insert("session-a".to_string());

        let view = SidebarOrganizationView::from_desktop_ui_state(&ui_state);
        assert!(view.arranges_anything());
        assert_eq!(
            view.organization
                .folders
                .get("folder-1")
                .map(|folder| (folder.name.as_str(), folder.auto_archive_after_days)),
            Some(("Archive", Some(3)))
        );
        assert!(view.organization.collapsed_folder_ids.contains("folder-1"));
        assert_eq!(view.project_order, vec!["project-1".to_string()]);
        assert_eq!(view.session_order, vec!["session-a".to_string()]);
        assert_eq!(view.session_order_anchored_at_ms, 42);
        assert!(view.pinned_session_ids.contains("session-a"));
        assert!(view.collapsed_project_ids.contains("project-2"));
        assert_eq!(view.hierarchy_mode, SidebarHierarchyMode::Detailed);
        // The revision is a content fingerprint, not a stored counter: two
        // reads of the same arrangement compare equal, which is what lets a
        // client's echoed revision be checked against it.
        assert_eq!(
            view.revision,
            SidebarOrganizationView::from_desktop_ui_state(&ui_state).revision
        );
        assert!(view.auto_continue_project_ids.contains("project-1"));
        assert_eq!(
            view.auto_continue_session_overrides.get("session-b"),
            Some(&false)
        );
        assert!(
            view.auto_continue_session_ids.contains("session-a"),
            "an enabled override is the effective set"
        );
        assert!(!view.auto_continue_session_ids.contains("session-b"));
        assert!(view.auto_continue_paused_session_ids.contains("session-a"));
        assert!(view.auto_continue_enabled_by_default);
        // And writing it back is the same translation in reverse, so a shell
        // that owns the runtime persists a tree the next read reconstructs
        // exactly — the revision included.
        let mut written = crate::DesktopUiStateV1::default();
        view.apply_to_desktop_ui_state(&mut written);
        assert_eq!(
            SidebarOrganizationView::from_desktop_ui_state(&written),
            view
        );
        let other = crate::DesktopUiStateV1::default();
        assert!(
            !SidebarOrganizationView::from_desktop_ui_state(&other).arranges_anything(),
            "an arrangement nobody made claimed to arrange something"
        );
    }

    #[test]
    fn a_shell_wide_default_covers_every_project_including_new_ones() {
        let mut state = crate::DesktopUiStateV1::default();
        state.session.auto_continue_enabled_by_default = true;
        // One project opted out while the default was on; another carried an
        // explicit opt-in from before the switch existed.
        state
            .session
            .auto_continue_disabled_project_ids
            .insert("project-off".to_string());
        state
            .session
            .auto_continue_project_ids
            .insert("project-on".to_string());

        let view = SidebarOrganizationView::from_desktop_ui_state(&state);
        assert!(view.auto_continue_project_default("project-new"));
        assert!(view.auto_continue_project_default("project-on"));
        assert!(!view.auto_continue_project_default("project-off"));

        // A session without an override follows its project; an override is
        // the reader's last word on that one row.
        assert!(view.auto_continue_session_enabled("session-new", "project-new"));
        assert!(!view.auto_continue_session_enabled("session-off", "project-off"));
        state
            .session
            .auto_continue_session_overrides
            .insert("session-new".to_string(), false);
        let view = SidebarOrganizationView::from_desktop_ui_state(&state);
        assert!(!view.auto_continue_session_enabled("session-new", "project-new"));

        // The default rides the wire both ways, so a compact client applies the
        // same rule without inventing a project it has not seen.
        let remote = view.to_remote();
        assert!(remote.auto_continue_default_enabled);
        assert_eq!(
            remote.auto_continue_disabled_project_ids,
            vec!["project-off".to_string()]
        );
        assert_eq!(
            SidebarOrganizationView::from_remote(&remote),
            view,
            "the default did not survive the wire"
        );
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
    fn a_fresh_session_filed_into_a_folder_projects_inside_it() {
        let mut view = view_with_folder();
        let sessions = [
            "session-a".to_string(),
            "session-b".to_string(),
            "session-fresh".to_string(),
        ];
        let mut session_projects = session_projects();
        session_projects.insert("session-fresh".to_string(), "project-1".to_string());

        // The new-session flow hands the tree a session with no placement of its
        // own, and files it into the folder the user started it from.
        assert!(view.organization.place_into_folder(
            &SidebarOrganizationItem::Session("session-fresh".into()),
            "folder-1",
            &session_projects,
        ));

        let root = sidebar_project_items(
            &view.organization,
            "project-1",
            &sessions,
            &BTreeSet::new(),
            None,
        );
        assert!(
            !root.contains(&SidebarOrganizationItem::Session("session-fresh".into())),
            "a filed session must not also be offered at the project root"
        );
        let children = sidebar_project_items(
            &view.organization,
            "project-1",
            &sessions,
            &BTreeSet::new(),
            Some("folder-1"),
        );
        assert_eq!(
            children,
            [SidebarOrganizationItem::Session("session-fresh".into())]
        );
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
            &[],
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

    /// A client can set a folder's archive window, and the window is the
    /// project's: a second folder taking one clears the first.
    #[test]
    fn a_remote_archive_window_lands_on_the_folder_and_moves_between_siblings() {
        let mut view = view_with_folder();
        assert!(view.organization.create_folder(
            "folder-2",
            "Later",
            Some("project-1".to_string()),
            None,
        ));

        let outcome = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                    folder_id: "folder-1".to_string(),
                    days: Some(7),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("a window in range is legal");
        let SidebarMutationOutcome::Applied(effect) = outcome else {
            panic!("setting a window should report an applied effect");
        };
        assert!(effect.organization);
        assert_eq!(
            view.organization
                .folder("folder-1")
                .and_then(|folder| folder.auto_archive_after_days),
            Some(7)
        );

        view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                folder_id: "folder-2".to_string(),
                days: Some(14),
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("a second window is legal on another folder");
        assert_eq!(
            view.organization
                .folder("folder-1")
                .and_then(|folder| folder.auto_archive_after_days),
            None,
            "one project carries one window, so the first folder gives it up"
        );

        // A window out of range, and a window on a folder that is gone, are
        // both refusals.
        assert!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                    folder_id: "folder-1".to_string(),
                    days: Some(crate::SIDEBAR_AUTO_ARCHIVE_MAX_DAYS + 1),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .is_err()
        );
        assert!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                    folder_id: "folder-gone".to_string(),
                    days: Some(7),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .is_err()
        );

        // Turning the window off is a change of its own.
        let outcome = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
                    folder_id: "folder-2".to_string(),
                    days: None,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("turning a window off is legal");
        let SidebarMutationOutcome::Applied(effect) = outcome else {
            panic!("turning a window off should report an applied effect");
        };
        assert!(effect.organization);
        assert_eq!(
            view.organization
                .folder("folder-2")
                .and_then(|folder| folder.auto_archive_after_days),
            None
        );
    }

    /// A client can lift (and set) the suspension of a session's auto continue,
    /// and only a session that would continue on its own can be suspended.
    #[test]
    fn a_remote_suspension_follows_the_session_it_suspends() {
        let mut view = SidebarOrganizationView::default();
        view.auto_continue_session_ids
            .insert("session-a".to_string());

        // A session that is not continuing on its own cannot be suspended: the
        // mutation is accepted, and does nothing for it.
        view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                session_ids: vec!["session-b".to_string()],
                paused: true,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("a session of the project is legal");
        assert!(view.auto_continue_paused_session_ids.is_empty());

        let outcome = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                    session_ids: vec!["session-a".to_string()],
                    paused: true,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("suspending a continuing session is legal");
        let SidebarMutationOutcome::Applied(effect) = outcome else {
            panic!("a suspension should report an applied effect");
        };
        assert!(effect.navigation);
        assert!(view.auto_continue_paused_session_ids.contains("session-a"));

        view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                session_ids: vec!["session-a".to_string()],
                paused: false,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("lifting a suspension is legal");
        assert!(!view.auto_continue_paused_session_ids.contains("session-a"));

        // A session the project does not know is a refusal, not a no-op.
        assert!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                    session_ids: vec!["session-gone".to_string()],
                    paused: false,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .is_err()
        );
    }

    /// A client's scheduled send is accepted for a session the project knows,
    /// and only the shell that owns the queue can take it.
    fn queued(
        message_id: u64,
        session_id: &str,
        text: &str,
        scheduled_at_ms: Option<i64>,
    ) -> SidebarQueuedSend {
        SidebarQueuedSend {
            message_id,
            session_id: session_id.to_string(),
            text: text.to_string(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            scheduled_at_ms,
        }
    }

    /// A client edits, removes, pauses, and switches a queue it does not own.
    ///
    /// The queue is the Desktop's — only it can release a message — but the
    /// reader may be looking at a phone, so the view has to accept the same four
    /// changes from either seat and report them as the queue's, never as the
    /// tree's.
    #[test]
    fn a_client_edits_the_queue_the_holder_releases() {
        let mut view = SidebarOrganizationView {
            queued_sends: vec![
                queued(1, "session-a", "first", None),
                queued(2, "session-a", "second", Some(1_800_000_000_000)),
                queued(3, "session-b", "other session", None),
            ],
            scheduled_sends: vec![SidebarScheduledSend {
                session_id: "session-a".to_string(),
                message_id: 2,
                text: "second".to_string(),
                attachments: Vec::new(),
                scheduled_at_ms: 1_800_000_000_000,
            }],
            ..SidebarOrganizationView::default()
        };

        // An edit changes the words and nothing else: the deadline it never
        // mentioned stays, because editing what a message says is not an
        // instruction about when it goes.
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 2,
                    text: "second, reworded".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("an entry the view published is one it can rewrite");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("an edit should report an applied effect");
        };
        assert!(
            effect.queue && !effect.organization && !effect.navigation,
            "the queue is not the tree"
        );
        assert_eq!(view.queued_sends[1].text, "second, reworded");
        assert_eq!(
            view.queued_sends[1].scheduled_at_ms,
            Some(1_800_000_000_000),
            "an edit never speaks about the hour"
        );

        // Rewriting it to the words it already has is a no-op rather than a
        // round trip nobody asked for.
        let outcome = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 2,
                    text: "second, reworded".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("an unchanged edit is legal");
        assert_eq!(outcome, SidebarMutationOutcome::AlreadyApplied);

        // A message the view never published is one it cannot rewrite.
        assert!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 99,
                    text: "ghost".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .is_err(),
            "an unknown message is a rejection, not an insert"
        );

        // Pausing and going manual are the queue's own two switches, and both
        // are idempotent: a client setting what already holds has changed
        // nothing.
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetQueuePaused {
                    session_id: "session-a".to_string(),
                    paused: true,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("pausing a known session is legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("a first pause changes the state");
        };
        assert!(effect.queue);
        assert!(view.queue_is_paused("session-a"));
        assert!(
            view.queue_sends_automatically("session-a"),
            "a paused queue is still automatic: the pause is its own switch"
        );
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetQueuePaused {
                    session_id: "session-a".to_string(),
                    paused: true,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("setting what already holds is legal"),
            SidebarMutationOutcome::AlreadyApplied
        );

        view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetQueueManual {
                session_id: "session-a".to_string(),
                manual: true,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("manual mode is a queue preference");
        assert!(!view.queue_sends_automatically("session-a"));

        // A removal takes the entry out of the list and out of the schedule
        // index: a cancelled message must not keep counting down to an hour
        // nothing happens at.
        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::RemoveQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 2,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("removing a published message is legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("a removal changes the queue");
        };
        assert!(effect.queue);
        assert_eq!(
            view.queued_sends_for("session-a").count(),
            1,
            "only the other session's message survives in this one's block"
        );
        assert!(
            view.scheduled_sends.is_empty(),
            "the schedule went with the message"
        );
        // Removing it again is what a retry after a reconnect does, and it is
        // already true.
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::RemoveQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 2,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("removing what is gone is legal"),
            SidebarMutationOutcome::AlreadyApplied
        );
    }

    /// A queued message a client writes is taken, and stays inside its block.
    #[test]
    fn a_client_enqueues_and_reorders_inside_one_session() {
        let mut view = SidebarOrganizationView {
            queued_sends: vec![
                queued(1, "session-a", "one", None),
                queued(2, "session-a", "two", None),
                queued(3, "session-b", "other", None),
            ],
            ..SidebarOrganizationView::default()
        };

        let effect = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::EnqueueQueuedSend {
                    session_id: "session-a".to_string(),
                    text: "three".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("a session of the project is legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("an enqueue asks the shell to take the message");
        };
        assert!(effect.enqueue, "the holder is what actually holds it");
        assert_eq!(
            view.queued_sends_for("session-a").count(),
            3,
            "the message is published at once so the row can draw it"
        );

        // A move reorders only the named session's block: another session's
        // message keeps the place the reader never touched.
        view.apply_remote(
            &RemoteSidebarOrganizationMutation::MoveQueuedSend {
                session_id: "session-a".to_string(),
                message_id: 1,
                before_message_id: Some(2),
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("a drag inside one session is legal");
        let texts: Vec<&str> = view
            .queued_sends
            .iter()
            .map(|send| send.text.as_str())
            .collect();
        assert_eq!(
            texts,
            vec!["two", "one", "three", "other"],
            "the moved message lands before its anchor and nothing else moves"
        );

        // A move to the end of the block, and a drop onto itself, which is the
        // no-op a drag that never left its row reports.
        view.apply_remote(
            &RemoteSidebarOrganizationMutation::MoveQueuedSend {
                session_id: "session-a".to_string(),
                message_id: 2,
                before_message_id: None,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("dropping at the end is legal");
        assert_eq!(
            view.queued_sends_for("session-a")
                .map(|send| send.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "three", "two"]
        );
        assert_eq!(
            view.queued_sends
                .iter()
                .map(|send| send.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "three", "two", "other"],
            "the block ends before the next session's messages, whichever way \
             a row is dragged"
        );
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::MoveQueuedSend {
                    session_id: "session-a".to_string(),
                    message_id: 2,
                    before_message_id: None,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("dropping it where it already is is legal"),
            SidebarMutationOutcome::AlreadyApplied
        );
    }

    /// The queue round-trips through the wire with its entries and switches.
    #[test]
    fn the_published_queue_round_trips_including_its_switches() {
        let view = SidebarOrganizationView {
            queued_sends: vec![
                queued(4, "session-a", "held", None),
                queued(5, "session-a", "later", Some(1_800_000_000_000)),
            ],
            paused_queue_session_ids: BTreeSet::from(["session-a".to_string()]),
            manual_queue_session_ids: BTreeSet::from(["session-b".to_string()]),
            ..SidebarOrganizationView::default()
        };
        let restored = SidebarOrganizationView::from_remote(&view.to_remote());
        assert_eq!(restored.queued_sends, view.queued_sends);
        assert!(restored.queue_is_paused("session-a"));
        assert!(!restored.queue_sends_automatically("session-b"));
        assert!(
            restored.queue_sends_automatically("session-a"),
            "manual mode is per session, not a switch for the whole shell"
        );
    }

    #[test]
    fn a_remote_scheduled_send_asks_the_shell_to_take_it() {
        let mut view = SidebarOrganizationView::default();
        let outcome = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::EnqueueScheduledSend {
                    session_id: "session-a".to_string(),
                    text: "ship it".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: 1_800_000_000_000,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("a session of the project is legal");
        let SidebarMutationOutcome::Applied(effect) = outcome else {
            panic!("an enqueue should report an applied effect");
        };
        assert!(
            effect.enqueue && !effect.organization && !effect.navigation,
            "the queue is the shell's, so the view has nothing of its own to move"
        );

        assert!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::EnqueueScheduledSend {
                    session_id: "session-gone".to_string(),
                    text: "ship it".to_string(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: 1_800_000_000_000,
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .is_err(),
            "a session the project does not know is refused"
        );
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
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("moving a session into a project folder is legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("moving into a folder should report an applied effect");
        };
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
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(outcome, Err(SidebarMutationRejection::Rejected));
        assert_eq!(view.organization.placements, before);
        assert_eq!(view.revision, 0);
    }

    #[test]
    fn a_noop_move_succeeds_without_reordering_the_tree() {
        let mut view = view_with_folder();
        let before = view.organization.placements.clone();
        // session-a already sits directly before session-b, so dropping it
        // "before session-b" again is a legal move that changes nothing.
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::MoveItems {
                items: vec![RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Session,
                    id: "session-a".to_string(),
                }],
                anchor: Some(RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Session,
                    id: "session-b".to_string(),
                }),
                position: RemoteSidebarDropPosition::Before,
                project_id: Some("project-1".to_string()),
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(outcome, Ok(SidebarMutationOutcome::AlreadyApplied));
        assert_eq!(view.organization.placements, before);
    }

    #[test]
    fn a_session_one_client_read_stops_being_unread_on_the_others() {
        // "Unread" lives in the authority's memory, so reading a session on a
        // compact client is only visible everywhere if that client says so. The
        // mutation clears exactly the ids it names and reports that the marks
        // changed, so the shell republishes them to its own readers.
        let mut view = view_with_folder();
        view.unread_session_ids.insert("session-a".to_string());
        view.unread_session_ids.insert("session-b".to_string());
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: vec!["session-a".to_string()],
                unread: false,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(
            outcome,
            Ok(SidebarMutationOutcome::Applied(
                SidebarMutationEffect::UNREAD
            ))
        );
        assert!(!view.unread_session_ids.contains("session-a"));
        assert!(
            view.unread_session_ids.contains("session-b"),
            "an id the reader did not open keeps its mark"
        );
    }

    #[test]
    fn a_completion_one_client_saw_badges_the_session_on_the_others() {
        // The reverse direction: a compact client that drew a dot for a finished
        // answer reports it, and the authority starts badging the session so the
        // Desktop draws the same dot. A session the authority already badges is
        // an idempotent no-op, not a second republish.
        let mut view = view_with_folder();
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: vec!["session-a".to_string()],
                unread: true,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(
            outcome,
            Ok(SidebarMutationOutcome::Applied(
                SidebarMutationEffect::UNREAD
            ))
        );
        assert!(view.unread_session_ids.contains("session-a"));

        let again = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: vec!["session-a".to_string()],
                unread: true,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(again, Ok(SidebarMutationOutcome::AlreadyApplied));
    }

    #[test]
    fn reading_an_already_read_session_is_a_noop_not_a_republish() {
        // A client that retried after a reconnect, or that opened a session the
        // authority had already read by another route, must not make the shell
        // republish every sidebar mark for nothing.
        let mut view = view_with_folder();
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: vec!["session-a".to_string()],
                unread: false,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(outcome, Ok(SidebarMutationOutcome::AlreadyApplied));
    }

    #[test]
    fn collapsing_an_already_collapsed_project_is_a_noop_not_a_rejection() {
        let mut view = view_with_folder();
        view.collapsed_project_ids.insert("project-1".to_string());
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                project_id: "project-1".to_string(),
                collapsed: true,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(outcome, Ok(SidebarMutationOutcome::AlreadyApplied));
    }

    #[test]
    fn setting_the_current_hierarchy_mode_is_a_noop_not_a_rejection() {
        let mut view = view_with_folder();
        view.hierarchy_mode = SidebarHierarchyMode::Compact;
        let outcome = view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetHierarchyMode {
                mode: RemoteSidebarHierarchyMode::Compact,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        );
        assert_eq!(outcome, Ok(SidebarMutationOutcome::AlreadyApplied));
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
                &session_workspaces(),
                "unused",
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
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("collapsing a project is always legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("collapsing a fresh project should report an applied effect");
        };
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
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("workspace reorder should be legal");
        let SidebarMutationOutcome::Applied(effect) = effect else {
            panic!("a real workspace reorder should report an applied effect");
        };
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

    fn queued_message(
        id: u64,
        session_id: &str,
        text: &str,
        scheduled_at_ms: Option<i64>,
    ) -> crate::ComposerQueueEntry {
        crate::ComposerQueueEntry {
            id,
            session_id: session_id.to_string(),
            desired_runtime: vibex_core::SessionRuntimeSelection::provider(
                vibex_core::AgentId::parse("claude").expect("agent id"),
                vibex_core::ProviderProfileId::parse("provider_local_default").expect("profile id"),
                "model:opus",
            ),
            text: text.to_string(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            command: None,
            scheduled_at_ms,
            dispatch_key: None,
        }
    }

    #[test]
    fn only_the_sends_waiting_on_an_hour_are_published() {
        // The queue holds messages for other reasons — a turn still running is
        // this shell's own business. What another client can act on is the
        // message with an hour on it, and that is the whole list.
        let mut state = crate::DesktopUiStateV1::default();
        state.composer.queue = vec![
            queued_message(1, "session-a", "held for the turn", None),
            queued_message(2, "session-a", "send this at nine", Some(1_800_000_000_000)),
            queued_message(3, "session-b", "and this tomorrow", Some(1_800_000_060_000)),
        ];
        let view = SidebarOrganizationView::from_desktop_ui_state(&state);

        assert_eq!(view.scheduled_sends.len(), 2);
        assert_eq!(view.scheduled_sends[0].session_id, "session-a");
        assert_eq!(view.scheduled_sends[0].message_id, 2);
        assert_eq!(view.scheduled_sends[0].text, "send this at nine");
        assert_eq!(view.scheduled_sends[0].scheduled_at_ms, 1_800_000_000_000);
        assert_eq!(
            view.scheduled_send_for("session-a")
                .map(|send| send.message_id),
            Some(2)
        );
        assert!(view.scheduled_send_for("session-c").is_none());
    }

    #[test]
    fn the_next_send_is_the_one_that_goes_out_first() {
        let mut state = crate::DesktopUiStateV1::default();
        state.composer.queue = vec![
            queued_message(1, "session-a", "later", Some(1_800_000_060_000)),
            queued_message(2, "session-a", "sooner", Some(1_800_000_000_000)),
        ];
        let view = SidebarOrganizationView::from_desktop_ui_state(&state);

        assert_eq!(
            view.scheduled_send_for("session-a")
                .map(|send| send.text.as_str()),
            Some("sooner")
        );
    }

    #[test]
    fn a_held_send_survives_the_wire() {
        let mut state = crate::DesktopUiStateV1::default();
        state.composer.queue = vec![queued_message(
            7,
            "session-a",
            "send this at nine",
            Some(1_800_000_000_000),
        )];
        let view = SidebarOrganizationView::from_desktop_ui_state(&state);

        let round_tripped = SidebarOrganizationView::from_remote(&view.to_remote());

        assert_eq!(round_tripped.scheduled_sends, view.scheduled_sends);
    }

    #[test]
    fn a_deadline_moves_and_a_deadline_is_taken_away() {
        let mut view = SidebarOrganizationView {
            scheduled_sends: vec![
                SidebarScheduledSend {
                    session_id: "session-a".to_string(),
                    message_id: 4,
                    text: "send this at nine".to_string(),
                    attachments: Vec::new(),
                    scheduled_at_ms: 1_800_000_000_000,
                },
                SidebarScheduledSend {
                    session_id: "session-b".to_string(),
                    message_id: 5,
                    text: "and this tomorrow".to_string(),
                    attachments: Vec::new(),
                    scheduled_at_ms: 1_800_000_060_000,
                },
            ],
            ..SidebarOrganizationView::default()
        };

        // The same value again is what a client that raced the authority sends:
        // the send already waits for that hour, so nothing moved.
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetScheduledSend {
                    session_id: "session-a".to_string(),
                    message_id: 4,
                    scheduled_at_ms: Some(1_800_000_000_000),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            ),
            Ok(SidebarMutationOutcome::AlreadyApplied)
        );

        let moved = view
            .apply_remote(
                &RemoteSidebarOrganizationMutation::SetScheduledSend {
                    session_id: "session-a".to_string(),
                    message_id: 4,
                    scheduled_at_ms: Some(1_800_000_120_000),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            )
            .expect("moving a published deadline is legal");
        let SidebarMutationOutcome::Applied(effect) = moved else {
            panic!("a moved deadline should report an applied effect");
        };
        assert!(effect.scheduled && !effect.navigation && !effect.organization);
        assert_eq!(
            view.scheduled_send_for("session-a")
                .map(|send| send.scheduled_at_ms),
            Some(1_800_000_120_000)
        );

        // Cancelling takes the send out of the published list and nothing else:
        // the message is still the holder's to send when its session is ready.
        view.apply_remote(
            &RemoteSidebarOrganizationMutation::SetScheduledSend {
                session_id: "session-a".to_string(),
                message_id: 4,
                scheduled_at_ms: None,
            },
            &session_projects(),
            &session_workspaces(),
            "unused",
            "unused",
        )
        .expect("cancelling a published deadline is legal");
        assert!(view.scheduled_send_for("session-a").is_none());
        assert_eq!(view.scheduled_sends.len(), 1, "the other session keeps its");
    }

    #[test]
    fn a_send_the_holder_never_published_cannot_be_edited() {
        // A message the authority has already sent — or never had — is not a
        // deadline this client may move: arming one would promise a send nobody
        // is holding.
        let mut view = SidebarOrganizationView::default();
        assert_eq!(
            view.apply_remote(
                &RemoteSidebarOrganizationMutation::SetScheduledSend {
                    session_id: "session-a".to_string(),
                    message_id: 9,
                    scheduled_at_ms: Some(1_800_000_000_000),
                },
                &session_projects(),
                &session_workspaces(),
                "unused",
                "unused",
            ),
            Err(SidebarMutationRejection::Rejected)
        );
    }

    #[test]
    fn a_deadline_lands_on_the_queue_entry_it_names() {
        let mut queue = vec![
            queued_message(1, "session-a", "first", None),
            queued_message(2, "session-a", "second", Some(1_800_000_000_000)),
        ];

        assert!(set_queued_send_schedule(
            &mut queue,
            "session-a",
            1,
            Some(1_800_000_120_000)
        ));
        assert_eq!(queue[0].scheduled_at_ms, Some(1_800_000_120_000));
        assert_eq!(queue[0].text, "first", "the words are not the deadline's");

        // Clearing gives the message back to the ordinary queue rather than
        // deleting it.
        assert!(set_queued_send_schedule(&mut queue, "session-a", 2, None));
        assert_eq!(queue[1].scheduled_at_ms, None);
        assert_eq!(queue[1].text, "second");

        // The session is part of the address: ids are only unique in one queue.
        assert!(!set_queued_send_schedule(
            &mut queue,
            "session-b",
            1,
            Some(1_800_000_000_000)
        ));
        assert_eq!(queue[0].scheduled_at_ms, Some(1_800_000_120_000));
    }
}
