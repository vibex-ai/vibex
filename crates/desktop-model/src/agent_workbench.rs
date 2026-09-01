use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentDelegationId, AgentMessagePhase, AgentSession, AgentSessionState,
    ElicitationRequestStatus, PermissionRequestStatus, PlanStepPayload, PlanStepStatus, RetryPhase,
    TimelineItem, TimelineItemKind, TimelinePayload, ToolCallStatus, VibexSessionId,
};

use crate::{ReasoningDisplayMode, SidebarState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSidebarRowKind {
    Project,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSidebarRow {
    pub id: String,
    pub kind: AgentSidebarRowKind,
    pub project_id: String,
    pub workspace_id: String,
    pub session_id: Option<VibexSessionId>,
    pub label: String,
    pub depth: u8,
    pub pinned: bool,
    pub selected: bool,
    pub collapsed: bool,
    pub state: Option<AgentSessionState>,
}

pub fn project_sidebar_rows(
    sessions: &[AgentSession],
    state: &SidebarState,
    query: &str,
) -> Vec<AgentSidebarRow> {
    let query = query.trim().to_lowercase();
    let mut groups = BTreeMap::<(String, String), Vec<&AgentSession>>::new();
    for session in sessions
        .iter()
        .filter(|session| session.deleted_at_ms.is_none())
    {
        groups
            .entry((
                session.workspace_id.to_string(),
                session.project_id.to_string(),
            ))
            .or_default()
            .push(session);
    }

    let order = state
        .row_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for ((workspace_id, project_id), mut project_sessions) in groups {
        project_sessions.sort_by_key(|session| {
            (
                !state.pinned_ids.contains(session.id.as_str()),
                order
                    .get(session.id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX),
                std::cmp::Reverse(session.last_message_at_ms),
                session.id.as_str(),
            )
        });
        let project_label = project_sessions
            .first()
            .and_then(|session| {
                std::path::Path::new(&session.workspace_root)
                    .file_name()
                    .and_then(|name| name.to_str())
            })
            .filter(|label| !label.is_empty())
            .unwrap_or("Workspace")
            .to_string();
        let project_matches = query.is_empty() || project_label.to_lowercase().contains(&query);
        let visible_sessions = project_sessions
            .iter()
            .copied()
            .filter(|session| {
                project_matches
                    || session.title.to_lowercase().contains(&query)
                    || session.workspace_root.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        if visible_sessions.is_empty() {
            continue;
        }
        let collapsed = state.collapsed_ids.contains(&project_id);
        rows.push(AgentSidebarRow {
            id: format!("project:{project_id}"),
            kind: AgentSidebarRowKind::Project,
            project_id: project_id.clone(),
            workspace_id: workspace_id.clone(),
            session_id: None,
            label: project_label,
            depth: 0,
            pinned: false,
            selected: false,
            collapsed,
            state: None,
        });
        if !collapsed || !query.is_empty() {
            rows.extend(visible_sessions.into_iter().map(|session| AgentSidebarRow {
                id: format!("session:{}", session.id.as_str()),
                kind: AgentSidebarRowKind::Session,
                project_id: project_id.clone(),
                workspace_id: workspace_id.clone(),
                session_id: Some(session.id.clone()),
                label: session.title.clone(),
                depth: 1,
                pinned: state.pinned_ids.contains(session.id.as_str()),
                selected: state.selected_ids.contains(session.id.as_str()),
                collapsed: false,
                state: Some(session.state),
            }));
        }
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineRowKind {
    UserMessage,
    AgentMessage,
    Reasoning,
    Plan,
    ToolCall,
    Command,
    FileOperation,
    WebSearch,
    TodoUpdate,
    Collaboration,
    ImageGeneration,
    GitNotice,
    SystemNotice,
    PermissionRequest,
    PermissionResolution,
    ElicitationRequest,
    ElicitationResolution,
    Retry,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRow {
    pub id: String,
    pub kind: TimelineRowKind,
    pub item_ids: Vec<String>,
    pub turn_id: Option<String>,
    pub turn_item_count: usize,
    pub turn_failed: bool,
    pub turn_pending_permission: bool,
    pub conclusion: bool,
    pub first_sequence: i64,
    pub last_sequence: i64,
    pub title: String,
    pub body: String,
    pub streaming: bool,
    pub collapsible: bool,
    pub pending_permission: bool,
    pub failed: bool,
    pub runtime_attribution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTurnPreview {
    pub turn_id: String,
    pub row_index: usize,
    pub title: String,
    pub item_count: usize,
    pub failed: bool,
    pub pending_permission: bool,
}

pub fn timeline_turn_previews(rows: &[TimelineRow]) -> Vec<TimelineTurnPreview> {
    let mut seen = BTreeSet::new();
    rows.iter()
        .enumerate()
        .filter_map(|(row_index, row)| {
            let turn_id = row.turn_id.as_ref()?;
            seen.insert(turn_id.clone()).then(|| TimelineTurnPreview {
                turn_id: turn_id.clone(),
                row_index,
                title: row.title.clone(),
                item_count: row.turn_item_count,
                failed: row.turn_failed,
                pending_permission: row.turn_pending_permission,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineConversationTurn {
    pub id: String,
    pub user_row: Option<TimelineRow>,
    pub process_rows: Vec<TimelineRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_activity_groups: Vec<TimelineProcessActivityGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_activity_groups_with_commands: Vec<TimelineProcessActivityGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_activity_groups_with_file_operations: Vec<TimelineProcessActivityGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_activity_groups_with_commands_and_file_operations:
        Vec<TimelineProcessActivityGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_status: Option<String>,
    pub conclusion_row: Option<TimelineRow>,
    pub runtime_attribution: Option<String>,
    pub complete: bool,
    pub failed: bool,
    pub pending_permission: bool,
    pub item_count: usize,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineProcessActivityGroup {
    pub id: String,
    pub start_row: usize,
    pub end_row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanProjection {
    pub sequence: i64,
    pub turn_anchor_sequence: i64,
    pub title: String,
    pub steps: Vec<PlanStepPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveCollaborationProjection {
    pub sequence: i64,
    pub action: String,
    pub status: vibex_core::ToolCallStatus,
    pub summary: String,
    pub agent_label: Option<String>,
}

/// Provider-neutral metadata for a product-managed child Agent card. The
/// timeline keeps the authoritative collaboration item; this projection keeps
/// GPUI callers from parsing payloads locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDelegationProjection {
    pub delegation_id: AgentDelegationId,
    pub child_session_id: VibexSessionId,
    pub action: String,
    pub status: ToolCallStatus,
    pub summary: String,
    pub agent_label: Option<String>,
}

/// Returns the latest managed-child metadata represented by a timeline row.
/// Provider-native collaboration rows deliberately return `None` because they
/// do not carry a durable child session identity.
pub fn timeline_row_delegation(
    row: &TimelineRow,
    items: &[TimelineItem],
) -> Option<TimelineDelegationProjection> {
    if row.kind != TimelineRowKind::Collaboration {
        return None;
    }
    let item = items
        .iter()
        .filter(|item| row.item_ids.iter().any(|id| id == item.id.as_str()))
        .max_by_key(|item| item.sequence)?;
    let TimelinePayload::Collaboration(collaboration) = &item.payload else {
        return None;
    };
    Some(TimelineDelegationProjection {
        delegation_id: collaboration.delegation_id.clone()?,
        child_session_id: collaboration.child_session_id.clone()?,
        action: collaboration.action.clone(),
        status: collaboration.status,
        summary: collaboration.summary.clone(),
        agent_label: collaboration.agent_label.clone(),
    })
}

pub fn has_managed_child_agent_delegations(items: &[TimelineItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            &item.payload,
            TimelinePayload::Collaboration(collaboration)
                if collaboration.delegation_id.is_some() && collaboration.child_session_id.is_some()
        )
    })
}

impl AgentPlanProjection {
    pub fn completed_step_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Completed)
            .count()
    }

    pub fn current_step_number(&self) -> usize {
        self.steps
            .iter()
            .position(|step| step.status == PlanStepStatus::Running)
            .or_else(|| {
                self.steps
                    .iter()
                    .position(|step| step.status == PlanStepStatus::Failed)
            })
            .or_else(|| {
                self.steps
                    .iter()
                    .position(|step| step.status == PlanStepStatus::Pending)
            })
            .map(|index| index + 1)
            .unwrap_or(self.steps.len())
    }

    pub fn is_complete(&self) -> bool {
        !self.steps.is_empty()
            && self
                .steps
                .iter()
                .all(|step| step.status == PlanStepStatus::Completed)
    }
}

pub fn current_agent_plan(items: &[TimelineItem]) -> Option<AgentPlanProjection> {
    let turn_anchor_sequence = items
        .iter()
        .rev()
        .find(|item| item.kind == TimelineItemKind::UserMessage)
        .map(|item| item.sequence)
        .unwrap_or_default();

    items
        .iter()
        .rev()
        .take_while(|item| item.sequence > turn_anchor_sequence)
        .find_map(|item| {
            let (title, steps) = match &item.payload {
                TimelinePayload::Plan(plan) => (&plan.title, &plan.steps),
                TimelinePayload::TodoUpdate(todo) => (&todo.title, &todo.items),
                _ => return None,
            };
            (!steps.is_empty()).then(|| AgentPlanProjection {
                sequence: item.sequence,
                turn_anchor_sequence,
                title: title.clone(),
                steps: steps.clone(),
            })
        })
}

pub fn active_collaborations(items: &[TimelineItem]) -> Vec<ActiveCollaborationProjection> {
    let mut latest_by_identity = BTreeMap::<String, ActiveCollaborationProjection>::new();
    for item in items {
        let TimelinePayload::Collaboration(collaboration) = &item.payload else {
            continue;
        };
        let identity = item.provider_correlation_id.clone().unwrap_or_else(|| {
            format!(
                "{}:{}",
                collaboration.action,
                collaboration.agent_label.as_deref().unwrap_or_default()
            )
        });
        latest_by_identity.insert(
            identity,
            ActiveCollaborationProjection {
                sequence: item.sequence,
                action: collaboration.action.clone(),
                status: collaboration.status,
                summary: collaboration.summary.clone(),
                agent_label: collaboration.agent_label.clone(),
            },
        );
    }
    let mut active = latest_by_identity
        .into_values()
        .filter(|collaboration| {
            matches!(
                collaboration.status,
                vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress
            )
        })
        .collect::<Vec<_>>();
    active.sort_by_key(|collaboration| collaboration.sequence);
    active
}

/// Concatenates the trailing run of streaming reasoning deltas. Agents emit
/// thought chunks as individual timeline items, so the in-flight status is the
/// accumulated run rather than the most recent fragment; joining the deltas
/// keeps the pending indicator stable while a thinking stream grows.
///
/// Only the leading whitespace is trimmed: the tail has to stay verbatim so a
/// consumer can extend the accumulation by appending the next chunk instead of
/// rescanning the whole run, which is what keeps thinking streams off a
/// quadratic reprojection path.
fn accumulated_streaming_reasoning(items: &[&TimelineItem]) -> Option<String> {
    let mut chunks: Vec<&str> = Vec::new();
    for item in items.iter().rev() {
        match &item.payload {
            TimelinePayload::Reasoning(reasoning) if !reasoning.is_final => {
                chunks.push(&reasoning.text);
            }
            _ => break,
        }
    }
    let accumulated = chunks.into_iter().rev().collect::<String>();
    let accumulated = accumulated.trim_start();
    (!accumulated.trim_end().is_empty()).then(|| accumulated.to_string())
}

pub fn timeline_conversation_turns(
    items: &[TimelineItem],
    session_state: Option<AgentSessionState>,
    pending_turn_active: bool,
) -> Vec<TimelineConversationTurn> {
    timeline_conversation_turns_with_reasoning_mode(
        items,
        session_state,
        pending_turn_active,
        ReasoningDisplayMode::LatestAtBottom,
    )
}

/// Projects conversation turns with an explicit reasoning presentation mode.
/// The legacy `timeline_conversation_turns` adapter intentionally retains the
/// compact bottom-indicator behavior for callers that do not have a UI
/// preference available.
pub fn timeline_conversation_turns_with_reasoning_mode(
    items: &[TimelineItem],
    session_state: Option<AgentSessionState>,
    pending_turn_active: bool,
    reasoning_display_mode: ReasoningDisplayMode,
) -> Vec<TimelineConversationTurn> {
    let visible_items = items
        .iter()
        .filter(|item| item.kind != TimelineItemKind::SystemNotice)
        .collect::<Vec<_>>();
    let mut rows_by_turn = BTreeMap::<String, Vec<TimelineRow>>::new();
    for row in timeline_rows_from_refs(&visible_items) {
        if let Some(turn_id) = row.turn_id.clone() {
            rows_by_turn.entry(turn_id).or_default().push(row);
        }
    }
    let grouped_turns = crate::timeline::timeline_turn_refs(visible_items.iter().copied());
    let last_turn_index = grouped_turns.len().checked_sub(1);
    let provider_turn_finished = matches!(
        session_state,
        Some(
            AgentSessionState::Idle
                | AgentSessionState::Error
                | AgentSessionState::Closed
                | AgentSessionState::Archived
        )
    );
    let mut turns = grouped_turns
        .into_iter()
        .enumerate()
        .map(|(index, turn)| {
            let turn_id = turn
                .user_item
                .as_ref()
                .map(|item| format!("turn:{}", item.id))
                .or_else(|| {
                    turn.response_items
                        .first()
                        .map(|item| format!("turn:continuation:{}", item.id))
                })
                .unwrap_or_else(|| format!("turn:{index}"));
            let mut turn_rows = rows_by_turn.remove(&turn_id).unwrap_or_default();
            let user_row_index = turn_rows
                .iter()
                .position(|row| row.kind == TimelineRowKind::UserMessage);
            let user_row = user_row_index.map(|row_index| turn_rows.remove(row_index));
            let runtime_attribution = consistent_row_runtime_attribution(&turn_rows);
            let provider_finished_for_turn =
                provider_turn_finished && last_turn_index == Some(index);
            let conclusion_item =
                find_conversation_turn_conclusion(&turn.response_items, provider_finished_for_turn);
            let conclusion_item_id = conclusion_item.map(|item| item.id.to_string());
            let conclusion_row_index = conclusion_item_id.as_ref().and_then(|conclusion_id| {
                turn_rows
                    .iter()
                    .position(|row| row.item_ids.iter().any(|item_id| item_id == conclusion_id))
            });
            let mut conclusion_row =
                conclusion_row_index.map(|row_index| turn_rows.remove(row_index));
            if let Some(row) = conclusion_row.as_mut() {
                row.conclusion = true;
                if provider_finished_for_turn {
                    row.streaming = false;
                }
            }
            let live_status = accumulated_streaming_reasoning(&turn.response_items);
            let trailing_reasoning_item_ids = turn
                .response_items
                .iter()
                .rev()
                .take_while(|item| {
                    matches!(
                        &item.payload,
                        TimelinePayload::Reasoning(reasoning) if !reasoning.is_final
                    )
                })
                .map(|item| item.id.to_string())
                .collect::<BTreeSet<_>>();
            let final_agent_item_ids = turn
                .response_items
                .iter()
                .filter(|item| is_final_agent_message(item))
                .map(|item| item.id.to_string())
                .collect::<BTreeSet<_>>();
            let has_terminal_response = turn
                .response_items
                .iter()
                .any(|item| is_final_agent_message(item) || is_turn_boundary_error(item));
            let complete = has_terminal_response || provider_finished_for_turn;
            if reasoning_display_mode == ReasoningDisplayMode::Timeline {
                for row in &mut turn_rows {
                    if row.kind == TimelineRowKind::Reasoning {
                        row.streaming = !complete
                            && row
                                .item_ids
                                .iter()
                                .any(|item_id| trailing_reasoning_item_ids.contains(item_id));
                    }
                }
            }
            turn_rows.retain(|row| {
                !(reasoning_display_mode == ReasoningDisplayMode::LatestAtBottom
                    && row.kind == TimelineRowKind::Reasoning
                    && row.streaming)
                    && row.kind != TimelineRowKind::PermissionResolution
                    && row.kind != TimelineRowKind::ElicitationResolution
                    && !matches!(
                        row.kind,
                        TimelineRowKind::Plan | TimelineRowKind::TodoUpdate
                    )
                    && !row
                        .item_ids
                        .iter()
                        .any(|item_id| final_agent_item_ids.contains(item_id))
            });
            if provider_finished_for_turn {
                for row in &mut turn_rows {
                    if row.kind == TimelineRowKind::AgentMessage {
                        row.streaming = false;
                    }
                }
            }
            turn_rows = compact_conversation_process_rows(turn_rows, &turn.response_items);
            let process_activity_groups = timeline_process_activity_groups(&turn_rows);
            let process_activity_groups_with_commands =
                timeline_process_activity_groups_with_commands(&turn_rows);
            let process_activity_groups_with_file_operations =
                timeline_process_activity_groups_with_file_operations(&turn_rows);
            let process_activity_groups_with_commands_and_file_operations =
                timeline_process_activity_groups_with_commands_and_file_operations(&turn_rows);
            let live_status = (!complete).then_some(live_status).flatten();
            let started_at_ms = turn
                .user_item
                .as_ref()
                .map(|item| item.timestamp_ms)
                .or_else(|| turn.response_items.first().map(|item| item.timestamp_ms))
                .unwrap_or_default();
            let ended_at_ms = if complete {
                turn.response_items
                    .last()
                    .map(|item| item.timestamp_ms)
                    .or_else(|| conclusion_item.map(|item| item.timestamp_ms))
            } else {
                conclusion_item
                    .map(|item| item.timestamp_ms)
                    .or_else(|| turn.response_items.last().map(|item| item.timestamp_ms))
            };
            TimelineConversationTurn {
                id: turn_id,
                user_row,
                process_rows: turn_rows,
                process_activity_groups,
                process_activity_groups_with_commands,
                process_activity_groups_with_file_operations,
                process_activity_groups_with_commands_and_file_operations,
                live_status,
                conclusion_row,
                runtime_attribution,
                complete,
                failed: turn.failed,
                pending_permission: !turn.pending_permission_ids.is_empty(),
                item_count: usize::from(turn.user_item.is_some()) + turn.response_items.len(),
                started_at_ms,
                ended_at_ms,
            }
        })
        .collect::<Vec<_>>();

    let last_turn = turns.last();
    let should_show_pending_turn = session_state == Some(AgentSessionState::Running)
        && (turns.is_empty()
            || (pending_turn_active && last_turn.is_some_and(|turn| turn.complete && turn.failed)));
    if should_show_pending_turn {
        let last_item = visible_items.last().copied();
        turns.push(TimelineConversationTurn {
            id: format!(
                "turn:pending:{}",
                last_item
                    .map(|item| item.sequence.to_string())
                    .unwrap_or_else(|| "empty".into())
            ),
            user_row: None,
            process_rows: Vec::new(),
            process_activity_groups: Vec::new(),
            process_activity_groups_with_commands: Vec::new(),
            process_activity_groups_with_file_operations: Vec::new(),
            process_activity_groups_with_commands_and_file_operations: Vec::new(),
            live_status: None,
            conclusion_row: None,
            runtime_attribution: None,
            complete: false,
            failed: false,
            pending_permission: false,
            item_count: 0,
            started_at_ms: last_item.map(|item| item.timestamp_ms).unwrap_or_default(),
            ended_at_ms: None,
        });
    }
    turns
}

pub fn timeline_process_activity_groups(rows: &[TimelineRow]) -> Vec<TimelineProcessActivityGroup> {
    timeline_process_activity_groups_matching(rows, false, false)
}

pub fn timeline_process_activity_groups_with_commands(
    rows: &[TimelineRow],
) -> Vec<TimelineProcessActivityGroup> {
    timeline_process_activity_groups_matching(rows, true, false)
}

pub fn timeline_process_activity_groups_with_file_operations(
    rows: &[TimelineRow],
) -> Vec<TimelineProcessActivityGroup> {
    timeline_process_activity_groups_matching(rows, false, true)
}

pub fn timeline_process_activity_groups_with_commands_and_file_operations(
    rows: &[TimelineRow],
) -> Vec<TimelineProcessActivityGroup> {
    timeline_process_activity_groups_matching(rows, true, true)
}

fn timeline_process_activity_groups_matching(
    rows: &[TimelineRow],
    include_commands: bool,
    include_file_operations: bool,
) -> Vec<TimelineProcessActivityGroup> {
    let mut groups = Vec::new();
    let mut group_start = None;

    for (index, row) in rows.iter().enumerate() {
        if (is_process_activity_row(row.kind) && !row.id.starts_with("delegation:"))
            || (include_commands && row.kind == TimelineRowKind::Command)
            || (include_file_operations && row.kind == TimelineRowKind::FileOperation)
        {
            group_start.get_or_insert(index);
            continue;
        }
        if let Some(start_row) = group_start.take()
            && index.saturating_sub(start_row) > 1
        {
            groups.push(TimelineProcessActivityGroup {
                id: format!("activity-group:{}", rows[start_row].id),
                start_row,
                end_row: index,
            });
        }
    }

    if let Some(start_row) = group_start
        && rows.len().saturating_sub(start_row) > 1
    {
        groups.push(TimelineProcessActivityGroup {
            id: format!("activity-group:{}", rows[start_row].id),
            start_row,
            end_row: rows.len(),
        });
    }

    groups
}

fn is_process_activity_row(kind: TimelineRowKind) -> bool {
    matches!(
        kind,
        TimelineRowKind::ToolCall
            | TimelineRowKind::WebSearch
            | TimelineRowKind::TodoUpdate
            | TimelineRowKind::Collaboration
            | TimelineRowKind::Retry
    )
}

fn consistent_row_runtime_attribution(rows: &[TimelineRow]) -> Option<String> {
    let mut attribution = None::<String>;
    for candidate in rows {
        let Some(candidate) = candidate.runtime_attribution.as_ref() else {
            continue;
        };
        if attribution
            .as_ref()
            .is_some_and(|existing| existing != candidate)
        {
            return None;
        }
        attribution.get_or_insert_with(|| candidate.clone());
    }
    attribution
}

fn compact_conversation_process_rows(
    rows: Vec<TimelineRow>,
    items: &[&TimelineItem],
) -> Vec<TimelineRow> {
    let items_by_id = items
        .iter()
        .copied()
        .map(|item| (item.id.to_string(), item))
        .collect::<BTreeMap<_, _>>();
    let mut compacted = Vec::<TimelineRow>::new();
    let mut index_by_key = BTreeMap::<String, usize>::new();
    for mut row in rows {
        let key = row.item_ids.iter().rev().find_map(|item_id| {
            items_by_id
                .get(item_id)
                .and_then(|item| process_compaction_key(item))
        });
        let Some(key) = key else {
            compacted.push(row);
            continue;
        };
        let Some(existing_index) = index_by_key.get(&key).copied() else {
            index_by_key.insert(key, compacted.len());
            compacted.push(row);
            continue;
        };
        let existing = &mut compacted[existing_index];
        let mut item_ids = std::mem::take(&mut existing.item_ids);
        for item_id in std::mem::take(&mut row.item_ids) {
            record_row_item_id(&mut item_ids, item_id);
        }
        row.id = existing.id.clone();
        row.item_ids = item_ids;
        row.first_sequence = existing.first_sequence;
        *existing = row;
    }
    compacted
}

fn process_compaction_key(item: &TimelineItem) -> Option<String> {
    match &item.payload {
        TimelinePayload::ToolCall(tool) => Some(format!("tool:{}", tool.tool_call_id)),
        TimelinePayload::Command(command) => Some(format!(
            "command:{}:{}",
            command.cwd.as_deref().unwrap_or_default(),
            command.command
        )),
        TimelinePayload::FileOperation(operation) => {
            Some(format!("file:{:?}:{}", operation.operation, operation.path))
        }
        TimelinePayload::WebSearch(search) => Some(format!("web:{}", search.query)),
        TimelinePayload::TodoUpdate(todo) => Some(format!("todo:{}", todo.title)),
        TimelinePayload::Collaboration(collaboration) => Some(format!(
            "collaboration:{}",
            item.provider_correlation_id
                .clone()
                .unwrap_or_else(|| format!(
                    "{}:{}",
                    collaboration.action,
                    collaboration.agent_label.as_deref().unwrap_or_default()
                ))
        )),
        TimelinePayload::ImageGeneration(_) => Some(format!(
            "image:{}",
            item.provider_correlation_id
                .as_deref()
                .unwrap_or_else(|| item.id.as_str())
        )),
        _ => None,
    }
}

fn find_conversation_turn_conclusion<'a>(
    items: &[&'a TimelineItem],
    provider_turn_finished: bool,
) -> Option<&'a TimelineItem> {
    let latest_error = items
        .iter()
        .copied()
        .enumerate()
        .rev()
        .find(|(_, item)| is_turn_boundary_error(item));
    let final_agent_message = select_agent_message(items, true);
    let streaming_agent_message = select_streaming_agent_message(items);
    let fallback_agent_message = provider_turn_finished
        .then(|| select_agent_message(items, false))
        .flatten();
    let agent_message = final_agent_message
        .or(streaming_agent_message)
        .or(fallback_agent_message);

    match (latest_error, agent_message) {
        (Some((error_index, error)), Some((message_index, _))) if error_index > message_index => {
            Some(error)
        }
        (_, Some((_, message))) => Some(message),
        (Some((_, error)), None) => Some(error),
        (None, None) => None,
    }
}

fn select_streaming_agent_message<'a>(
    items: &[&'a TimelineItem],
) -> Option<(usize, &'a TimelineItem)> {
    let (index, item) = items.iter().copied().enumerate().next_back()?;
    matches!(
        item.payload,
        TimelinePayload::AgentMessageDelta(ref delta)
            if delta.phase == Some(AgentMessagePhase::FinalAnswer)
    )
    .then_some((index, item))
}

fn select_agent_message<'a>(
    items: &[&'a TimelineItem],
    final_only: bool,
) -> Option<(usize, &'a TimelineItem)> {
    let candidates = items
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, item)| {
            let text = agent_message_text(item)?;
            let eligible = if final_only {
                is_final_agent_message(item)
            } else {
                matches!(
                    item.payload,
                    TimelinePayload::AgentMessageDelta(ref delta)
                        if delta.phase != Some(AgentMessagePhase::Commentary)
                )
            };
            if text.trim().is_empty() || !eligible {
                return None;
            }
            Some((index, item, text))
        })
        .collect::<Vec<_>>();
    let mut selected = candidates.last().copied()?;
    for candidate in candidates.iter().rev().skip(1) {
        let selected_text = selected.2.trim();
        let candidate_text = candidate.2.trim();
        if !candidate_text.is_empty()
            && selected_text.len() > candidate_text.len()
            && selected_text.ends_with(candidate_text)
        {
            selected = *candidate;
        }
    }
    Some((selected.0, selected.1))
}

fn agent_message_text(item: &TimelineItem) -> Option<&str> {
    match &item.payload {
        TimelinePayload::AgentMessageDelta(message) => Some(&message.text_delta),
        TimelinePayload::AgentMessage(message) => Some(&message.text),
        _ => None,
    }
}

fn is_final_agent_message(item: &TimelineItem) -> bool {
    matches!(
        &item.payload,
        TimelinePayload::AgentMessage(message) if message.is_final
    )
}

fn is_turn_boundary_error(item: &TimelineItem) -> bool {
    item.kind == TimelineItemKind::Error && item.provider_correlation_id.is_none()
}

const RECONNECT_PROGRESS_PREFIX: &str = "Reconnecting... ";

fn compact_reconnect_progress_text(text: &str) -> String {
    if !text.contains(RECONNECT_PROGRESS_PREFIX) {
        return text.to_string();
    }

    let mut compacted = String::with_capacity(text.len());
    let mut latest_progress = None;
    for paragraph in text.split_inclusive("\n\n") {
        let content = paragraph.strip_suffix("\n\n").unwrap_or(paragraph);
        if is_reconnect_progress_paragraph(content) {
            latest_progress = Some(paragraph);
            continue;
        }
        if let Some(progress) = latest_progress.take() {
            compacted.push_str(progress);
        }
        compacted.push_str(paragraph);
    }
    if let Some(progress) = latest_progress {
        compacted.push_str(progress);
    }
    compacted
}

fn is_reconnect_progress_paragraph(paragraph: &str) -> bool {
    let Some(counter) = paragraph.trim().strip_prefix(RECONNECT_PROGRESS_PREFIX) else {
        return false;
    };
    let Some((attempt, total)) = counter.split_once('/') else {
        return false;
    };
    let (Ok(attempt), Ok(total)) = (attempt.parse::<u32>(), total.parse::<u32>()) else {
        return false;
    };
    attempt > 0 && attempt <= total
}

/// A merged row only needs stable endpoints. The inclusive sequence range
/// represents intermediate chunks without retaining one heap String per token.
fn record_row_item_id(item_ids: &mut Vec<String>, item_id: String) {
    match item_ids.len() {
        0 => item_ids.push(item_id),
        1 if item_ids[0] != item_id => item_ids.push(item_id),
        1 => {}
        _ if item_ids.last() != Some(&item_id) => {
            item_ids.truncate(2);
            item_ids[1] = item_id;
        }
        _ => {}
    }
}

pub fn timeline_rows(items: &[TimelineItem]) -> Vec<TimelineRow> {
    let item_refs = items.iter().collect::<Vec<_>>();
    timeline_rows_from_refs(&item_refs)
}

fn timeline_rows_from_refs(items: &[&TimelineItem]) -> Vec<TimelineRow> {
    let resolved_permission_ids = items
        .iter()
        .filter_map(|item| match &item.payload {
            TimelinePayload::PermissionResolution(resolution) => {
                Some(resolution.request_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let resolved_elicitation_ids = items
        .iter()
        .filter_map(|item| match &item.payload {
            TimelinePayload::ElicitationResolution(resolution) => {
                Some(resolution.request_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut rows = Vec::<TimelineRow>::new();
    for item in items.iter().copied() {
        let correlation = item
            .correlation_id
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| item.provider_correlation_id.clone());
        match &item.payload {
            TimelinePayload::AgentMessageDelta(delta) => {
                let final_answer = delta.phase == Some(AgentMessagePhase::FinalAnswer);
                let key = correlation
                    .as_deref()
                    .map(|value| format!("agent:{value}"))
                    .unwrap_or_else(|| format!("agent-delta:{}", item.id));
                if let Some(previous) = rows.last_mut().filter(|row| {
                    row.kind == TimelineRowKind::AgentMessage
                        && row.streaming
                        && row.conclusion == final_answer
                        && runtime_attribution_is_compatible(row, item)
                }) {
                    previous.body.push_str(&delta.text_delta);
                    if delta.text_delta.contains(RECONNECT_PROGRESS_PREFIX) {
                        previous.body = compact_reconnect_progress_text(&previous.body);
                    }
                    record_row_item_id(&mut previous.item_ids, item.id.to_string());
                    previous.last_sequence = item.sequence;
                    merge_runtime_attribution(previous, item);
                } else {
                    rows.push(TimelineRow {
                        id: key,
                        kind: TimelineRowKind::AgentMessage,
                        item_ids: vec![item.id.to_string()],
                        turn_id: None,
                        turn_item_count: 0,
                        turn_failed: false,
                        turn_pending_permission: false,
                        conclusion: final_answer,
                        first_sequence: item.sequence,
                        last_sequence: item.sequence,
                        title: "Agent".into(),
                        body: delta.text_delta.clone(),
                        streaming: true,
                        collapsible: false,
                        pending_permission: false,
                        failed: false,
                        runtime_attribution: runtime_attribution(item),
                        file_path: None,
                    });
                }
            }
            TimelinePayload::AgentMessage(message) => {
                let message_text = compact_reconnect_progress_text(&message.text);
                let key = correlation
                    .as_deref()
                    .map(|value| format!("agent:{value}"))
                    .unwrap_or_else(|| format!("agent:{}", item.id));
                if let Some(previous) = rows.last_mut().filter(|row| {
                    row.kind == TimelineRowKind::AgentMessage
                        && row.streaming
                        && runtime_attribution_is_compatible(row, item)
                }) {
                    previous.body = message_text;
                    record_row_item_id(&mut previous.item_ids, item.id.to_string());
                    previous.last_sequence = item.sequence;
                    previous.streaming = !message.is_final;
                    merge_runtime_attribution(previous, item);
                } else {
                    rows.push(simple_row(
                        item,
                        key,
                        TimelineRowKind::AgentMessage,
                        "Agent",
                        message_text,
                        !message.is_final,
                        false,
                    ));
                }
            }
            TimelinePayload::UserMessage(message) => rows.push(simple_row(
                item,
                format!("user:{}", item.id),
                TimelineRowKind::UserMessage,
                "You",
                message.text.clone(),
                false,
                false,
            )),
            TimelinePayload::Reasoning(reasoning) => {
                if !reasoning.is_final
                    && let Some(previous) = rows.last_mut().filter(|row| {
                        row.kind == TimelineRowKind::Reasoning
                            && row.streaming
                            && runtime_attribution_is_compatible(row, item)
                    })
                {
                    previous.body.push_str(&reasoning.text);
                    record_row_item_id(&mut previous.item_ids, item.id.to_string());
                    previous.last_sequence = item.sequence;
                    merge_runtime_attribution(previous, item);
                } else {
                    rows.push(simple_row(
                        item,
                        format!("reasoning:{}", item.id),
                        TimelineRowKind::Reasoning,
                        "Reasoning",
                        reasoning.text.clone(),
                        !reasoning.is_final,
                        true,
                    ));
                }
            }
            TimelinePayload::Plan(plan) => rows.push(simple_row(
                item,
                format!("plan:{}", item.id),
                TimelineRowKind::Plan,
                plan.title.clone(),
                plan.steps
                    .iter()
                    .map(|step| format!("{:?}: {}", step.status, step.title))
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
                true,
            )),
            TimelinePayload::ToolCall(tool) => rows.push(simple_row(
                item,
                format!("tool:{}", tool.tool_call_id),
                TimelineRowKind::ToolCall,
                tool.tool_name.clone(),
                tool.output_summary
                    .clone()
                    .or_else(|| tool.input_summary.clone())
                    .unwrap_or_else(|| tool.summary.clone()),
                matches!(
                    tool.status,
                    vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress
                ),
                true,
            )),
            TimelinePayload::Command(command) => rows.push(simple_row(
                item,
                format!("command:{}", item.id),
                TimelineRowKind::Command,
                command.command.clone(),
                command.output_summary.clone().unwrap_or_default(),
                command.status == vibex_core::CommandStatus::Started,
                true,
            )),
            TimelinePayload::FileOperation(operation) => {
                let mut row = simple_row(
                    item,
                    format!("file:{}", item.id),
                    TimelineRowKind::FileOperation,
                    format!("{:?} {}", operation.operation, operation.path),
                    operation.summary.clone(),
                    false,
                    true,
                );
                row.file_path = Some(operation.path.clone());
                rows.push(row);
            }
            TimelinePayload::WebSearch(search) => rows.push(simple_row(
                item,
                format!("web:{}", item.id),
                TimelineRowKind::WebSearch,
                search.query.clone(),
                search.result_summary.clone().unwrap_or_default(),
                matches!(
                    search.status,
                    vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress
                ),
                true,
            )),
            TimelinePayload::TodoUpdate(todo) => rows.push(simple_row(
                item,
                format!("todo:{}", item.id),
                TimelineRowKind::TodoUpdate,
                todo.title.clone(),
                todo.items
                    .iter()
                    .map(|step| format!("{:?}: {}", step.status, step.title))
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
                true,
            )),
            TimelinePayload::Collaboration(collaboration) => {
                let key = collaboration
                    .delegation_id
                    .as_ref()
                    .map(|id| format!("delegation:{id}"))
                    .unwrap_or_else(|| format!("collaboration:{}", item.id));
                if collaboration.delegation_id.is_some()
                    && let Some(previous) = rows.iter_mut().find(|row| row.id == key)
                {
                    previous.title = collaboration
                        .agent_label
                        .clone()
                        .unwrap_or_else(|| collaboration.action.clone());
                    previous.body = collaboration.summary.clone();
                    previous.streaming = matches!(
                        collaboration.status,
                        ToolCallStatus::Started | ToolCallStatus::Progress
                    );
                    previous.failed = matches!(collaboration.status, ToolCallStatus::Failed);
                    previous.last_sequence = item.sequence;
                    record_row_item_id(&mut previous.item_ids, item.id.to_string());
                } else {
                    let mut row = simple_row(
                        item,
                        key,
                        TimelineRowKind::Collaboration,
                        collaboration
                            .agent_label
                            .clone()
                            .unwrap_or_else(|| collaboration.action.clone()),
                        collaboration.summary.clone(),
                        matches!(
                            collaboration.status,
                            ToolCallStatus::Started | ToolCallStatus::Progress
                        ),
                        true,
                    );
                    row.failed = matches!(collaboration.status, ToolCallStatus::Failed);
                    rows.push(row);
                }
            }
            TimelinePayload::ImageGeneration(image) => rows.push(simple_row(
                item,
                format!("image:{}", item.id),
                TimelineRowKind::ImageGeneration,
                "Image generation",
                image.summary.clone(),
                matches!(
                    image.status,
                    vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress
                ),
                true,
            )),
            TimelinePayload::GitNotice(notice) => rows.push(simple_row(
                item,
                format!("git:{}", item.id),
                TimelineRowKind::GitNotice,
                "Git",
                notice.summary.clone(),
                false,
                false,
            )),
            TimelinePayload::SystemNotice(notice) => rows.push(simple_row(
                item,
                format!("system:{}", item.id),
                TimelineRowKind::SystemNotice,
                format!("{:?}", notice.level),
                notice.message.clone(),
                false,
                false,
            )),
            TimelinePayload::PermissionRequest(request) => {
                let mut row = simple_row(
                    item,
                    format!("permission:{}", request.id),
                    TimelineRowKind::PermissionRequest,
                    request.title.clone(),
                    request
                        .details
                        .iter()
                        .map(|detail| format!("{}: {}", detail.label, detail.value))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    false,
                    true,
                );
                row.pending_permission = request.status == PermissionRequestStatus::Pending
                    && !resolved_permission_ids.contains(request.id.as_str());
                rows.push(row);
            }
            TimelinePayload::PermissionResolution(resolution) => rows.push(simple_row(
                item,
                format!("permission-resolution:{}", resolution.request_id),
                TimelineRowKind::PermissionResolution,
                "Permission resolved",
                format!("{:?}", resolution.response),
                false,
                false,
            )),
            TimelinePayload::ElicitationRequest(request) => {
                let mut body = request.message.clone();
                if let Some(description) = request.description.as_deref()
                    && !description.trim().is_empty()
                    && description.trim() != request.message.trim()
                {
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(description);
                }
                let mut row = simple_row(
                    item,
                    format!("elicitation:{}", request.id),
                    TimelineRowKind::ElicitationRequest,
                    request
                        .title
                        .clone()
                        .unwrap_or_else(|| "Input requested".to_string()),
                    body,
                    false,
                    true,
                );
                row.pending_permission = request.status == ElicitationRequestStatus::Pending
                    && !resolved_elicitation_ids.contains(request.id.as_str());
                rows.push(row);
            }
            TimelinePayload::ElicitationResolution(resolution) => rows.push(simple_row(
                item,
                format!("elicitation-resolution:{}", resolution.request_id),
                TimelineRowKind::ElicitationResolution,
                "Input request resolved",
                format!("{:?}", resolution.action),
                false,
                false,
            )),
            TimelinePayload::Retry(retry) => {
                let key = correlation
                    .as_deref()
                    .map(|value| format!("retry:{value}"))
                    .unwrap_or_else(|| format!("retry:{}", item.id));
                let (title, body, streaming) = retry_row_text(retry);
                if let Some(previous) = rows.iter_mut().find(|row| row.id == key) {
                    previous.title = title;
                    previous.body = body;
                    previous.streaming = streaming;
                    previous.last_sequence = item.sequence;
                    record_row_item_id(&mut previous.item_ids, item.id.to_string());
                    merge_runtime_attribution(previous, item);
                } else {
                    rows.push(simple_row(
                        item,
                        key,
                        TimelineRowKind::Retry,
                        title,
                        body,
                        streaming,
                        false,
                    ));
                }
            }
            TimelinePayload::Error(error) => {
                let mut row = simple_row(
                    item,
                    format!("error:{}", item.id),
                    TimelineRowKind::Error,
                    error.code.clone(),
                    error.message.clone(),
                    false,
                    true,
                );
                row.failed = true;
                rows.push(row);
            }
        }
    }
    decorate_turn_metadata(&mut rows, items);
    rows
}

pub fn timeline_agent_message_count_after_sequence(
    items: &[TimelineItem],
    previous_end_sequence: Option<i64>,
) -> usize {
    let start = previous_end_sequence
        .map(|sequence| {
            items
                .partition_point(|item| item.sequence <= sequence)
                .saturating_sub(1)
        })
        .unwrap_or(0);
    timeline_rows(&items[start..])
        .into_iter()
        .filter(|row| {
            row.kind == TimelineRowKind::AgentMessage
                && !row.body.trim().is_empty()
                && previous_end_sequence.is_none_or(|sequence| row.first_sequence > sequence)
        })
        .count()
}

#[derive(Default)]
struct TurnRowMetadata {
    item_count: usize,
    failed: bool,
    pending_permission: bool,
    conclusion_item_id: Option<String>,
}

fn decorate_turn_metadata(rows: &mut [TimelineRow], items: &[&TimelineItem]) {
    let mut item_to_turn = BTreeMap::<String, String>::new();
    let mut turn_metadata = BTreeMap::<String, TurnRowMetadata>::new();

    for (index, turn) in crate::timeline::timeline_turn_refs(items.iter().copied())
        .into_iter()
        .enumerate()
    {
        let turn_id = turn
            .user_item
            .as_ref()
            .map(|item| format!("turn:{}", item.id))
            .or_else(|| {
                turn.response_items
                    .first()
                    .map(|item| format!("turn:continuation:{}", item.id))
            })
            .unwrap_or_else(|| format!("turn:{index}"));
        let item_count = usize::from(turn.user_item.is_some()) + turn.response_items.len();
        if let Some(user_item) = &turn.user_item {
            item_to_turn.insert(user_item.id.to_string(), turn_id.clone());
        }
        for item in &turn.response_items {
            item_to_turn.insert(item.id.to_string(), turn_id.clone());
        }
        turn_metadata.insert(
            turn_id,
            TurnRowMetadata {
                item_count,
                failed: turn.failed,
                pending_permission: !turn.pending_permission_ids.is_empty(),
                conclusion_item_id: turn.conclusion_item_id,
            },
        );
    }

    for row in rows {
        let Some(turn_id) = row
            .item_ids
            .first()
            .and_then(|item_id| item_to_turn.get(item_id))
            .cloned()
        else {
            continue;
        };
        let Some(metadata) = turn_metadata.get(&turn_id) else {
            continue;
        };
        row.turn_id = Some(turn_id);
        row.turn_item_count = metadata.item_count;
        row.turn_failed = metadata.failed;
        row.turn_pending_permission = metadata.pending_permission;
        row.conclusion = metadata
            .conclusion_item_id
            .as_ref()
            .is_some_and(|conclusion_id| row.item_ids.iter().any(|id| id == conclusion_id));
    }
}

fn simple_row(
    item: &TimelineItem,
    id: String,
    kind: TimelineRowKind,
    title: impl Into<String>,
    body: String,
    streaming: bool,
    collapsible: bool,
) -> TimelineRow {
    TimelineRow {
        id,
        kind,
        item_ids: vec![item.id.to_string()],
        turn_id: None,
        turn_item_count: 0,
        turn_failed: false,
        turn_pending_permission: false,
        conclusion: false,
        first_sequence: item.sequence,
        last_sequence: item.sequence,
        title: title.into(),
        body,
        streaming,
        collapsible,
        pending_permission: false,
        failed: false,
        runtime_attribution: runtime_attribution(item),
        file_path: None,
    }
}

fn retry_row_text(retry: &vibex_core::AgentRetryPayload) -> (String, String, bool) {
    let phase_title = match retry.phase {
        RetryPhase::Started => "Retrying",
        RetryPhase::Recovered => "Retry recovered",
        RetryPhase::Exhausted => "Retry exhausted",
    };
    let attempt = match (retry.attempt, retry.max_attempts) {
        (Some(attempt), Some(max)) => format!("attempt {attempt}/{max}"),
        (Some(attempt), None) => format!("attempt {attempt}"),
        _ => String::new(),
    };
    let title = if attempt.is_empty() {
        phase_title.to_string()
    } else {
        format!("{phase_title} ({attempt})")
    };
    let delay = retry
        .delay_ms
        .filter(|delay| *delay > 0)
        .map(|delay| format!("waiting {}s", delay.div_ceil(1000)));
    let body = [
        attempt,
        delay.unwrap_or_default(),
        retry.reason.clone().unwrap_or_default(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" - ");
    (title, body, retry.phase == RetryPhase::Started)
}

fn runtime_attribution(item: &TimelineItem) -> Option<String> {
    item.execution_attribution.as_ref().map(|attribution| {
        format!(
            "{} · {} · {}",
            attribution.agent_label, attribution.auth_source_label, attribution.model_label
        )
    })
}

fn runtime_attribution_is_compatible(row: &TimelineRow, item: &TimelineItem) -> bool {
    match (&row.runtime_attribution, runtime_attribution(item)) {
        (Some(existing), Some(incoming)) => existing == &incoming,
        _ => true,
    }
}

fn merge_runtime_attribution(row: &mut TimelineRow, item: &TimelineItem) {
    if row.runtime_attribution.is_none() {
        row.runtime_attribution = runtime_attribution(item);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineFollowState {
    pub following_bottom: bool,
    pub unread_count: u32,
    pub anchor_row_id: Option<String>,
    pub anchor_offset_px: i32,
}

impl Default for TimelineFollowState {
    fn default() -> Self {
        Self {
            following_bottom: true,
            unread_count: 0,
            anchor_row_id: None,
            anchor_offset_px: 0,
        }
    }
}

impl TimelineFollowState {
    pub fn content_appended(&mut self, count: usize) {
        if !self.following_bottom {
            self.unread_count = self.unread_count.saturating_add(count as u32);
        }
    }

    pub fn set_following_bottom(&mut self, following: bool) {
        self.following_bottom = following;
        if following {
            self.unread_count = 0;
            self.anchor_row_id = None;
            self.anchor_offset_px = 0;
        }
    }

    pub fn preserve_anchor(&mut self, row_id: impl Into<String>, offset_px: i32) {
        self.following_bottom = false;
        self.anchor_row_id = Some(row_id.into());
        self.anchor_offset_px = offset_px;
    }
}

pub fn pending_permission_ids(items: &[TimelineItem]) -> BTreeSet<String> {
    let mut pending = BTreeSet::new();
    for item in items {
        match &item.payload {
            TimelinePayload::PermissionRequest(request)
                if request.status == PermissionRequestStatus::Pending =>
            {
                pending.insert(request.id.to_string());
            }
            TimelinePayload::PermissionResolution(resolution) => {
                pending.remove(resolution.request_id.as_str());
            }
            TimelinePayload::ElicitationRequest(request)
                if request.status == ElicitationRequestStatus::Pending =>
            {
                pending.insert(request.id.to_string());
            }
            TimelinePayload::ElicitationResolution(resolution) => {
                pending.remove(resolution.request_id.as_str());
            }
            _ => {}
        }
    }
    pending
}

pub fn timeline_kind_coverage(items: &[TimelineItem]) -> Vec<TimelineItemKind> {
    let mut kinds = Vec::new();
    for item in items {
        if !kinds.contains(&item.kind) {
            kinds.push(item.kind);
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vibex_core::{
        AgentDelegationId, AgentId, AgentMessageDeltaPayload, AgentMessagePayload,
        AgentRetryPayload, AgentSessionSafety, CollaborationPayload, FileOperationPayload,
        PlanPayload, ProjectId, ReasoningPayload, RetryKind, RetryPhase, TimelineItemId,
        TimelineRedactionState, TimelineSource, TodoUpdatePayload, ToolCallPayload, ToolCallStatus,
        TurnExecutionAttributionView, UserMessagePayload, WorkspaceId, WorkspaceMode,
    };

    fn session(id: &str, project: &str, title: &str, updated_at_ms: i64) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(id).unwrap(),
            title: title.into(),
            project_id: ProjectId::parse(project).unwrap(),
            workspace_id: WorkspaceId::parse(format!("workspace_{project}")).unwrap(),
            workspace_root: format!("/work/{project}"),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 0,
            updated_at_ms,
            last_message_at_ms: updated_at_ms,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn item(sequence: i64, correlation: Option<&str>, payload: TimelinePayload) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId::parse(format!("timeline_{sequence}")).unwrap(),
            session_id: VibexSessionId::parse("session_current").unwrap(),
            sequence,
            timestamp_ms: sequence,
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: correlation.map(|value| value.parse().unwrap()),
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        }
    }

    fn retry_item(
        sequence: i64,
        provider_correlation_id: &str,
        phase: RetryPhase,
        attempt: Option<u32>,
        max_attempts: Option<u32>,
        reason: Option<&str>,
    ) -> TimelineItem {
        let mut item = item(
            sequence,
            None,
            TimelinePayload::Retry(AgentRetryPayload {
                kind: RetryKind::ModelRequest,
                phase,
                attempt,
                max_attempts,
                delay_ms: None,
                reason: reason.map(str::to_string),
            }),
        );
        item.source = TimelineSource::Provider;
        item.provider_correlation_id = Some(provider_correlation_id.to_string());
        item
    }

    #[test]
    fn timeline_presentation_borrows_lossless_file_snapshots() {
        let original = item(
            1,
            None,
            TimelinePayload::FileOperation(FileOperationPayload {
                operation: vibex_core::FileOperationKind::Edit,
                path: "apps/desktop/src/app.rs".into(),
                summary: "Edited app.rs".into(),
                old_text: Some("a".repeat(1_000_000)),
                new_text: Some("b".repeat(1_000_000)),
                patch: None,
                raw_extension: Some(vibex_core::AgentEventRawExtension::new(
                    Vec::new(),
                    Some("raw input that is not rendered".into()),
                    None,
                    Vec::new(),
                    std::collections::BTreeMap::new(),
                    false,
                )),
            }),
        );

        let projected = timeline_conversation_turns(
            std::slice::from_ref(&original),
            Some(AgentSessionState::Idle),
            false,
        );
        let TimelinePayload::FileOperation(original_file) = &original.payload else {
            panic!("expected original file operation");
        };
        assert_eq!(
            original_file.old_text.as_ref().map(String::len),
            Some(1_000_000)
        );
        assert_eq!(
            original_file.new_text.as_ref().map(String::len),
            Some(1_000_000)
        );
        let projected_file = &projected[0].process_rows[0];
        assert_eq!(
            projected_file.file_path.as_deref(),
            Some(original_file.path.as_str())
        );
        assert_eq!(projected_file.body, original_file.summary);
    }

    #[test]
    fn sidebar_projection_preserves_collapse_while_search_reveals_matches() {
        let sessions = vec![
            session("session_a", "project_a", "Alpha", 1),
            session("session_b", "project_a", "Beta", 2),
        ];
        let mut state = SidebarState::default();
        state.collapsed_ids.insert("project_a".into());
        assert_eq!(project_sidebar_rows(&sessions, &state, "").len(), 1);
        let rows = project_sidebar_rows(&sessions, &state, "beta");
        assert_eq!(rows.len(), 2);
        assert!(state.collapsed_ids.contains("project_a"));
        assert_eq!(rows[1].session_id.as_ref().unwrap().as_str(), "session_b");
    }

    #[test]
    fn streaming_delta_and_final_message_reconcile_into_one_stable_row() {
        let rows = timeline_rows(&[
            item(
                1,
                Some("correlation_turn_a"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "hel".into(),
                    chunk_index: 0,
                    phase: None,
                }),
            ),
            item(
                2,
                Some("correlation_turn_a"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "lo".into(),
                    chunk_index: 1,
                    phase: None,
                }),
            ),
            item(
                3,
                Some("correlation_turn_a"),
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "hello".into(),
                    is_final: true,
                }),
            ),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "agent:correlation_turn_a");
        assert_eq!(rows[0].body, "hello");
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].item_ids.len(), 2);
        assert!(rows[0].conclusion);
        assert_eq!(rows[0].turn_item_count, 3);
    }

    #[test]
    fn unread_message_count_tracks_agent_rows_instead_of_timeline_items() {
        let mut items = vec![item(
            1,
            None,
            TimelinePayload::Reasoning(ReasoningPayload {
                text: "thinking".into(),
                is_final: false,
            }),
        )];
        assert_eq!(timeline_agent_message_count_after_sequence(&items, None), 0);

        items.push(item(
            2,
            Some("correlation_turn_a"),
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "hel".into(),
                chunk_index: 0,
                phase: None,
            }),
        ));
        assert_eq!(
            timeline_agent_message_count_after_sequence(&items, Some(1)),
            1
        );

        items.push(item(
            3,
            Some("correlation_turn_a"),
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "lo".into(),
                chunk_index: 1,
                phase: None,
            }),
        ));
        items.push(item(
            4,
            Some("correlation_turn_a"),
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "hello".into(),
                is_final: true,
            }),
        ));
        assert_eq!(
            timeline_agent_message_count_after_sequence(&items, Some(2)),
            0
        );

        items.push(item(
            5,
            None,
            TimelinePayload::Reasoning(ReasoningPayload {
                text: "more thinking".into(),
                is_final: true,
            }),
        ));
        assert_eq!(
            timeline_agent_message_count_after_sequence(&items, Some(4)),
            0
        );

        items.push(item(
            6,
            Some("correlation_turn_b"),
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        ));
        assert_eq!(
            timeline_agent_message_count_after_sequence(&items, Some(5)),
            1
        );
    }

    #[test]
    fn reconnect_progress_replaces_the_previous_attempt_in_stream_and_final_text() {
        let mut items = (1_u32..=5)
            .map(|attempt| {
                item(
                    i64::from(attempt),
                    None,
                    TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                        text_delta: format!("Reconnecting... {attempt}/5\n\n"),
                        chunk_index: attempt - 1,
                        phase: None,
                    }),
                )
            })
            .collect::<Vec<_>>();

        let rows = timeline_rows(&items);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body, "Reconnecting... 5/5\n\n");
        assert_eq!(rows[0].item_ids.len(), 2);
        assert_eq!(rows[0].turn_item_count, 5);

        items.push(item(
            6,
            None,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: concat!(
                    "Reconnecting... 1/5\n\n",
                    "Reconnecting... 2/5\n\n",
                    "Reconnecting... 3/5\n\n",
                    "Reconnecting... 4/5\n\n",
                    "Reconnecting... 5/5\n\n",
                    "unexpected status 404 Not Found\n\n",
                )
                .into(),
                is_final: true,
            }),
        ));

        let rows = timeline_rows(&items);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].body,
            "Reconnecting... 5/5\n\nunexpected status 404 Not Found\n\n"
        );
        assert_eq!(rows[0].item_ids.len(), 2);
        assert_eq!(rows[0].turn_item_count, 6);
        assert!(!rows[0].streaming);
    }

    #[test]
    fn adjacent_stream_chunks_merge_without_a_shared_correlation_and_final_replaces_body() {
        let rows = timeline_rows(&[
            item(
                1,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "I am ".into(),
                    chunk_index: 0,
                    phase: None,
                }),
            ),
            item(
                2,
                Some("correlation_provider_chunk_2"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "Cod".into(),
                    chunk_index: 1,
                    phase: None,
                }),
            ),
            item(
                3,
                Some("correlation_provider_chunk_3"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "ex".into(),
                    chunk_index: 2,
                    phase: None,
                }),
            ),
            item(
                4,
                Some("correlation_provider_final"),
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "I am Codex".into(),
                    is_final: true,
                }),
            ),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "agent-delta:timeline_1");
        assert_eq!(rows[0].body, "I am Codex");
        assert_eq!(rows[0].item_ids.len(), 2);
        assert_eq!(rows[0].turn_item_count, 4);
        assert_eq!(rows[0].first_sequence, 1);
        assert_eq!(rows[0].last_sequence, 4);
        assert!(!rows[0].streaming);
    }

    #[test]
    fn stream_compaction_stops_at_attribution_and_event_boundaries() {
        let mut first = item(
            1,
            None,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "first".into(),
                chunk_index: 0,
                phase: None,
            }),
        );
        first.execution_attribution = Some(TurnExecutionAttributionView {
            agent_label: "Codex".into(),
            auth_source_label: "Profile A".into(),
            model_label: "Model A".into(),
        });
        let mut second = item(
            2,
            None,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "second".into(),
                chunk_index: 1,
                phase: None,
            }),
        );
        second.execution_attribution = Some(TurnExecutionAttributionView {
            agent_label: "Codex".into(),
            auth_source_label: "Profile B".into(),
            model_label: "Model B".into(),
        });
        let rows = timeline_rows(&[
            first,
            second,
            item(
                3,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "boundary".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "third".into(),
                    chunk_index: 2,
                    phase: None,
                }),
            ),
        ]);

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].body, "first");
        assert_eq!(rows[1].body, "second");
        assert_eq!(rows[2].kind, TimelineRowKind::UserMessage);
        assert_eq!(rows[3].body, "third");
    }

    #[test]
    fn adjacent_non_final_reasoning_chunks_merge_without_correlation() {
        let rows = timeline_rows(&[
            item(
                1,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "inspect ".into(),
                    is_final: false,
                }),
            ),
            item(
                2,
                Some("correlation_reasoning_chunk_2"),
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "state".into(),
                    is_final: false,
                }),
            ),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body, "inspect state");
        assert_eq!(rows[0].item_ids.len(), 2);
        assert!(rows[0].streaming);
    }

    #[test]
    fn streaming_reasoning_becomes_live_status_instead_of_process_history() {
        let mut items = vec![
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Inspect the workspace".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Planning targeted extraction".into(),
                    is_final: false,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read files".into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Evaluating ".into(),
                    is_final: false,
                }),
            ),
            item(
                5,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "persistence strategy".into(),
                    is_final: false,
                }),
            ),
        ];

        let active = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);
        assert_eq!(active.len(), 1);
        // Consecutive thought deltas accumulate instead of flashing the latest
        // fragment; the run stops at the tool call boundary.
        assert_eq!(
            active[0].live_status.as_deref(),
            Some("Evaluating persistence strategy")
        );
        assert_eq!(active[0].process_rows.len(), 1);
        assert_eq!(active[0].process_rows[0].kind, TimelineRowKind::ToolCall);

        items.push(item(
            6,
            None,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "Done".into(),
                is_final: true,
            }),
        ));
        let completed = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);
        assert!(completed[0].complete);
        assert!(completed[0].live_status.is_none());
        assert_eq!(completed[0].process_rows.len(), 1);
        assert_eq!(completed[0].process_rows[0].kind, TimelineRowKind::ToolCall);
    }

    #[test]
    fn live_status_accumulates_only_the_trailing_streaming_run() {
        // The reasoning before the tool call is finished history, so only the
        // deltas after it join the live status.
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Investigate".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Earlier segment".into(),
                    is_final: false,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read files".into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Fresh ".into(),
                    is_final: false,
                }),
            ),
            item(
                5,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "segment".into(),
                    is_final: false,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);
        assert_eq!(turns[0].live_status.as_deref(), Some("Fresh segment"));
    }

    #[test]
    fn timeline_reasoning_mode_keeps_history_and_marks_only_the_trailing_run_live() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Investigate".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "First thought ".into(),
                    is_final: false,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "run".into(),
                    is_final: false,
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read files".into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
            item(
                5,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Second thought".into(),
                    is_final: false,
                }),
            ),
        ];

        let turns = timeline_conversation_turns_with_reasoning_mode(
            &items,
            Some(AgentSessionState::Running),
            false,
            ReasoningDisplayMode::Timeline,
        );
        let rows = &turns[0].process_rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, TimelineRowKind::Reasoning);
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].body, "First thought run");
        assert_eq!(rows[1].kind, TimelineRowKind::ToolCall);
        assert_eq!(rows[2].kind, TimelineRowKind::Reasoning);
        assert!(rows[2].streaming);
        assert_eq!(turns[0].live_status.as_deref(), Some("Second thought"));

        let mut after_event = items.to_vec();
        after_event.push(item(
            6,
            None,
            TimelinePayload::ToolCall(ToolCallPayload {
                tool_call_id: "tool_write".into(),
                tool_name: "write".into(),
                status: ToolCallStatus::Started,
                summary: "Write files".into(),
                input_summary: None,
                output_summary: None,
                raw_extension: None,
            }),
        ));
        let turns = timeline_conversation_turns_with_reasoning_mode(
            &after_event,
            Some(AgentSessionState::Running),
            false,
            ReasoningDisplayMode::Timeline,
        );
        let rows = &turns[0].process_rows;
        assert_eq!(rows[2].kind, TimelineRowKind::Reasoning);
        assert!(!rows[2].streaming);
        assert!(turns[0].live_status.is_none());
    }

    #[test]
    fn live_status_keeps_the_complete_long_stream() {
        let long_prefix = "x".repeat(4_064);
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Think long".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: long_prefix,
                    is_final: false,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "最新思考".into(),
                    is_final: false,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);
        let status = turns[0].live_status.as_deref().expect("live status");
        assert_eq!(status.chars().count(), 4_064 + "最新思考".chars().count());
        assert!(status.starts_with('x'));
        assert!(status.ends_with("最新思考"));
    }

    #[test]
    fn final_reasoning_keeps_its_historical_process_row() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Explain".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Published reasoning".into(),
                    is_final: true,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "Answer".into(),
                    is_final: true,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns[0].process_rows.len(), 1);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::Reasoning);
        assert_eq!(turns[0].process_rows[0].body, "Published reasoning");
    }

    #[test]
    fn idle_reasoning_only_turn_clears_live_status_and_completes() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Inspect".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Reasoning(ReasoningPayload {
                    text: "Inspecting files".into(),
                    is_final: false,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert!(turns[0].complete);
        assert!(turns[0].live_status.is_none());
        assert!(turns[0].process_rows.is_empty());
        assert!(turns[0].conclusion_row.is_none());
    }

    #[test]
    fn conversation_turn_projection_keeps_user_and_one_combined_agent_conclusion() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Who are you?".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "I am ".into(),
                    chunk_index: 0,
                    phase: None,
                }),
            ),
            item(
                3,
                Some("correlation_chunk_3"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "Codex".into(),
                    chunk_index: 1,
                    phase: None,
                }),
            ),
            item(
                4,
                Some("correlation_final_4"),
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "I am Codex".into(),
                    is_final: true,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_row.as_ref().unwrap().body, "Who are you?");
        assert!(turns[0].process_rows.is_empty());
        let conclusion = turns[0].conclusion_row.as_ref().unwrap();
        assert_eq!(conclusion.body, "I am Codex");
        assert_eq!(conclusion.item_ids.len(), 2);
        assert_eq!(conclusion.turn_item_count, 4);
        assert!(turns[0].complete);
    }

    #[test]
    fn current_plan_uses_the_latest_provider_neutral_snapshot_in_the_active_turn() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Implement the feature".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Plan(PlanPayload {
                    title: "Plan".into(),
                    steps: vec![
                        PlanStepPayload {
                            title: "Inspect".into(),
                            status: PlanStepStatus::Running,
                        },
                        PlanStepPayload {
                            title: "Implement".into(),
                            status: PlanStepStatus::Pending,
                        },
                    ],
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::TodoUpdate(TodoUpdatePayload {
                    title: "Implementation tasks".into(),
                    items: vec![
                        PlanStepPayload {
                            title: "Inspect".into(),
                            status: PlanStepStatus::Completed,
                        },
                        PlanStepPayload {
                            title: "Implement".into(),
                            status: PlanStepStatus::Running,
                        },
                    ],
                    raw_extension: None,
                }),
            ),
        ];

        let plan = current_agent_plan(&items).unwrap();

        assert_eq!(plan.sequence, 3);
        assert_eq!(plan.turn_anchor_sequence, 1);
        assert_eq!(plan.title, "Implementation tasks");
        assert_eq!(plan.completed_step_count(), 1);
        assert_eq!(plan.current_step_number(), 2);
        assert!(!plan.is_complete());
    }

    #[test]
    fn current_plan_resets_when_a_new_user_turn_starts() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::Plan(PlanPayload {
                    title: "Old plan".into(),
                    steps: vec![PlanStepPayload {
                        title: "Old step".into(),
                        status: PlanStepStatus::Running,
                    }],
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Start something else".into(),
                    attachments: Vec::new(),
                }),
            ),
        ];

        assert!(current_agent_plan(&items).is_none());
    }

    #[test]
    fn active_collaboration_projection_keeps_only_latest_running_identities() {
        let collaboration = |sequence, identity: &str, agent: &str, status| {
            let mut item = item(
                sequence,
                None,
                TimelinePayload::Collaboration(CollaborationPayload {
                    action: "spawn_agent".into(),
                    status,
                    summary: format!("{agent} {status:?}"),
                    agent_label: Some(agent.into()),
                    delegation_id: None,
                    child_session_id: None,
                    raw_extension: None,
                }),
            );
            item.provider_correlation_id = Some(identity.into());
            item
        };
        let items = [
            collaboration(1, "worker-a", "Reviewer", ToolCallStatus::Started),
            collaboration(2, "worker-b", "Builder", ToolCallStatus::Progress),
            collaboration(3, "worker-a", "Reviewer", ToolCallStatus::Completed),
        ];

        let active = active_collaborations(&items);

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent_label.as_deref(), Some("Builder"));
        assert_eq!(active[0].sequence, 2);
        assert_eq!(active[0].status, ToolCallStatus::Progress);
    }

    #[test]
    fn managed_delegation_rows_coalesce_and_keep_the_child_session_projection() {
        let delegation_id = AgentDelegationId::new();
        let child_session_id = VibexSessionId::parse("session_child").unwrap();
        let collaboration = |sequence, status, summary: &str| {
            item(
                sequence,
                None,
                TimelinePayload::Collaboration(CollaborationPayload {
                    action: "delegate_to_agent".into(),
                    status,
                    summary: summary.into(),
                    agent_label: Some("Reviewer".into()),
                    delegation_id: Some(delegation_id.clone()),
                    child_session_id: Some(child_session_id.clone()),
                    raw_extension: None,
                }),
            )
        };
        let items = vec![
            collaboration(1, ToolCallStatus::Started, "Starting"),
            collaboration(2, ToolCallStatus::Completed, "Reviewed the changes"),
        ];

        let rows = timeline_rows(&items);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, format!("delegation:{delegation_id}"));
        assert_eq!(rows[0].body, "Reviewed the changes");
        assert_eq!(rows[0].item_ids.len(), 2);
        let projection = timeline_row_delegation(&rows[0], &items).unwrap();
        assert_eq!(projection.delegation_id, delegation_id);
        assert_eq!(projection.child_session_id, child_session_id);
        assert_eq!(projection.status, ToolCallStatus::Completed);
        assert!(has_managed_child_agent_delegations(&items));
    }

    #[test]
    fn heavyweight_timeline_cards_do_not_collapse_into_activity_groups() {
        assert!(is_process_activity_row(TimelineRowKind::ToolCall));
        assert!(is_process_activity_row(TimelineRowKind::WebSearch));
        assert!(is_process_activity_row(TimelineRowKind::Collaboration));
        assert!(is_process_activity_row(TimelineRowKind::Retry));
        assert!(!is_process_activity_row(TimelineRowKind::Command));
        assert!(!is_process_activity_row(TimelineRowKind::FileOperation));
        assert!(!is_process_activity_row(TimelineRowKind::ImageGeneration));
    }

    #[test]
    fn retry_rows_coalesce_progress_into_one_dynamic_activity_line() {
        let items = vec![
            retry_item(
                1,
                "retry-turn-1",
                RetryPhase::Started,
                Some(1),
                Some(3),
                Some("provider returned 503"),
            ),
            retry_item(
                2,
                "retry-turn-1",
                RetryPhase::Started,
                Some(2),
                Some(3),
                Some("provider returned 502"),
            ),
            retry_item(
                3,
                "retry-turn-1",
                RetryPhase::Exhausted,
                Some(3),
                Some(3),
                Some("provider returned 500"),
            ),
        ];

        let rows = timeline_rows(&items);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, TimelineRowKind::Retry);
        assert_eq!(rows[0].id, "retry:retry-turn-1");
        assert_eq!(rows[0].title, "Retry exhausted (attempt 3/3)");
        assert_eq!(rows[0].body, "attempt 3/3 - provider returned 500");
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].first_sequence, 1);
        assert_eq!(rows[0].last_sequence, 3);
        assert_eq!(rows[0].item_ids, vec!["timeline_1", "timeline_3"]);
    }

    #[test]
    fn compact_activity_groups_can_include_commands() {
        let row = |id: &str, kind: TimelineRowKind| TimelineRow {
            id: id.into(),
            kind,
            item_ids: vec![id.into()],
            turn_id: Some("turn:compact-tools".into()),
            turn_item_count: 3,
            turn_failed: false,
            turn_pending_permission: false,
            conclusion: false,
            first_sequence: 1,
            last_sequence: 1,
            title: id.into(),
            body: String::new(),
            streaming: false,
            collapsible: false,
            pending_permission: false,
            failed: false,
            runtime_attribution: None,
            file_path: None,
        };
        let rows = vec![
            row("tool:read", TimelineRowKind::ToolCall),
            row("command:check", TimelineRowKind::Command),
            row("tool:search", TimelineRowKind::WebSearch),
        ];

        assert!(timeline_process_activity_groups(&rows).is_empty());
        assert_eq!(
            timeline_process_activity_groups_with_commands(&rows),
            vec![TimelineProcessActivityGroup {
                id: "activity-group:tool:read".into(),
                start_row: 0,
                end_row: 3,
            }]
        );
    }

    #[test]
    fn compact_activity_groups_cover_file_operation_display_modes() {
        let row = |id: &str, kind: TimelineRowKind| TimelineRow {
            id: id.into(),
            kind,
            item_ids: vec![id.into()],
            turn_id: Some("turn:compact-files".into()),
            turn_item_count: 4,
            turn_failed: false,
            turn_pending_permission: false,
            conclusion: false,
            first_sequence: 1,
            last_sequence: 1,
            title: id.into(),
            body: String::new(),
            streaming: false,
            collapsible: false,
            pending_permission: false,
            failed: false,
            runtime_attribution: None,
            file_path: None,
        };
        let rows = vec![
            row("tool:read", TimelineRowKind::ToolCall),
            row("file:edit", TimelineRowKind::FileOperation),
            row("command:check", TimelineRowKind::Command),
            row("tool:search", TimelineRowKind::WebSearch),
        ];

        assert!(timeline_process_activity_groups(&rows).is_empty());
        assert_eq!(
            timeline_process_activity_groups_with_commands(&rows),
            vec![TimelineProcessActivityGroup {
                id: "activity-group:command:check".into(),
                start_row: 2,
                end_row: 4,
            }]
        );
        assert_eq!(
            timeline_process_activity_groups_with_file_operations(&rows),
            vec![TimelineProcessActivityGroup {
                id: "activity-group:tool:read".into(),
                start_row: 0,
                end_row: 2,
            }]
        );
        assert_eq!(
            timeline_process_activity_groups_with_commands_and_file_operations(&rows),
            vec![TimelineProcessActivityGroup {
                id: "activity-group:tool:read".into(),
                start_row: 0,
                end_row: 4,
            }]
        );
    }

    #[test]
    fn conversation_turn_projection_moves_plan_rows_out_of_the_chat_flow() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Implement".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Plan(PlanPayload {
                    title: "Plan".into(),
                    steps: vec![PlanStepPayload {
                        title: "Inspect".into(),
                        status: PlanStepStatus::Running,
                    }],
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::TodoUpdate(TodoUpdatePayload {
                    title: "Todo".into(),
                    items: vec![PlanStepPayload {
                        title: "Inspect".into(),
                        status: PlanStepStatus::Completed,
                    }],
                    raw_extension: None,
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read files".into(),
                    input_summary: Some("src/lib.rs".into()),
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].process_rows.len(), 1);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::ToolCall);
    }

    #[test]
    fn idle_delta_only_turn_keeps_user_row_and_projects_merged_fallback_conclusion() {
        let system_notice = |sequence: i64| {
            item(
                sequence,
                None,
                TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                    level: vibex_core::SystemNoticeLevel::Info,
                    message: "runtime state".into(),
                }),
            )
        };
        let items = [
            system_notice(1),
            system_notice(2),
            item(
                3,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "hi".into(),
                    attachments: Vec::new(),
                }),
            ),
            system_notice(4),
            item(
                5,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "Hi. ".into(),
                    chunk_index: 0,
                    phase: None,
                }),
            ),
            item(
                6,
                Some("correlation_chunk_6"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "What are you working on?".into(),
                    chunk_index: 1,
                    phase: None,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_row.as_ref().map(|row| row.body.as_str()),
            Some("hi")
        );
        assert_eq!(
            turns[0]
                .conclusion_row
                .as_ref()
                .map(|row| row.body.as_str()),
            Some("Hi. What are you working on?")
        );
        assert!(turns[0].process_rows.is_empty());
        assert!(turns[0].complete);
    }

    #[test]
    fn conversation_turn_projection_streams_conclusion_separately_from_process() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Continue".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read project files".into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "I have applied the code change.".into(),
                    chunk_index: 0,
                    phase: Some(AgentMessagePhase::Commentary),
                }),
            ),
            item(
                4,
                Some("correlation_chunk_4"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "work ".into(),
                    chunk_index: 1,
                    phase: Some(AgentMessagePhase::FinalAnswer),
                }),
            ),
            item(
                5,
                Some("correlation_chunk_5"),
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "in progress".into(),
                    chunk_index: 2,
                    phase: Some(AgentMessagePhase::FinalAnswer),
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].process_rows.len(), 2);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::ToolCall);
        assert_eq!(
            turns[0].process_rows[1].body,
            "I have applied the code change."
        );
        let conclusion = turns[0].conclusion_row.as_ref().unwrap();
        assert_eq!(conclusion.body, "work in progress");
        assert!(conclusion.streaming);
        assert!(conclusion.conclusion);
        assert!(!turns[0].complete);
    }

    #[test]
    fn agent_stream_before_later_process_activity_remains_in_process_history() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Continue".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "I will inspect first.".into(),
                    chunk_index: 0,
                    phase: Some(AgentMessagePhase::Commentary),
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Started,
                    summary: "Reading project files".into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);

        assert_eq!(turns.len(), 1);
        assert!(turns[0].conclusion_row.is_none());
        assert_eq!(turns[0].process_rows.len(), 2);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::AgentMessage);
        assert_eq!(turns[0].process_rows[1].kind, TimelineRowKind::ToolCall);
    }

    #[test]
    fn commentary_at_the_running_turn_tail_remains_process_history() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Continue".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "The public commit is complete; I am checking status.".into(),
                    chunk_index: 0,
                    phase: Some(AgentMessagePhase::Commentary),
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Running), false);

        assert_eq!(turns.len(), 1);
        assert!(turns[0].conclusion_row.is_none());
        assert_eq!(turns[0].process_rows.len(), 1);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::AgentMessage);
        assert!(turns[0].process_rows[0].streaming);
    }

    #[test]
    fn completed_commentary_only_turn_has_no_conclusion() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Continue".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                    text_delta: "I am still checking the result.".into(),
                    chunk_index: 0,
                    phase: Some(AgentMessagePhase::Commentary),
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns.len(), 1);
        assert!(turns[0].complete);
        assert!(turns[0].conclusion_row.is_none());
        assert_eq!(turns[0].process_rows.len(), 1);
        assert_eq!(turns[0].process_rows[0].kind, TimelineRowKind::AgentMessage);
        assert!(!turns[0].process_rows[0].streaming);
    }

    #[test]
    fn conversation_turn_projection_compacts_process_updates_at_the_start_position() {
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Inspect".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Started,
                    summary: "Reading".into(),
                    input_summary: Some("src/lib.rs".into()),
                    output_summary: None,
                    raw_extension: None,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: "tool_read".into(),
                    tool_name: "read".into(),
                    status: ToolCallStatus::Completed,
                    summary: "Read".into(),
                    input_summary: Some("src/lib.rs".into()),
                    output_summary: Some("Done".into()),
                    raw_extension: None,
                }),
            ),
            item(
                4,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "Finished".into(),
                    is_final: true,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns[0].process_rows.len(), 1);
        assert_eq!(turns[0].process_rows[0].id, "tool:tool_read");
        assert_eq!(turns[0].process_rows[0].item_ids.len(), 2);
        assert_eq!(turns[0].process_rows[0].first_sequence, 2);
        assert_eq!(turns[0].process_rows[0].last_sequence, 3);
        assert_eq!(turns[0].process_rows[0].body, "Done");
    }

    #[test]
    fn conversation_turn_projection_groups_consecutive_activity_between_agent_messages() {
        let tool = |sequence: i64, tool_call_id: &str, summary: &str| {
            item(
                sequence,
                None,
                TimelinePayload::ToolCall(ToolCallPayload {
                    tool_call_id: tool_call_id.into(),
                    tool_name: tool_call_id.into(),
                    status: ToolCallStatus::Completed,
                    summary: summary.into(),
                    input_summary: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            )
        };
        let items = [
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Inspect".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "I will inspect the project first.".into(),
                    is_final: false,
                }),
            ),
            tool(3, "tool_read", "Read the project guide"),
            tool(4, "tool_list", "Listed files"),
            item(
                5,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "The workspace contains Rust and TypeScript.".into(),
                    is_final: false,
                }),
            ),
            tool(6, "tool_search", "Searched code"),
            tool(7, "tool_check", "Checked the entry point"),
            item(
                8,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "Finished".into(),
                    is_final: true,
                }),
            ),
        ];

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);
        let turn = &turns[0];

        assert_eq!(turn.process_rows.len(), 6);
        assert_eq!(
            turn.process_activity_groups,
            vec![
                TimelineProcessActivityGroup {
                    id: "activity-group:tool:tool_read".into(),
                    start_row: 1,
                    end_row: 3,
                },
                TimelineProcessActivityGroup {
                    id: "activity-group:tool:tool_search".into(),
                    start_row: 4,
                    end_row: 6,
                },
            ]
        );
        assert_eq!(
            turn.process_rows[turn.process_activity_groups[0].end_row - 1].title,
            "tool_list"
        );
        assert_eq!(
            turn.process_rows[turn.process_activity_groups[1].end_row - 1].title,
            "tool_check"
        );
    }

    #[test]
    fn row_projection_marks_error_and_hidden_continue_as_separate_turns() {
        let rows = timeline_rows(&[
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "try".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Error(vibex_core::TimelineErrorPayload {
                    code: "turn_failed".into(),
                    message: "failed".into(),
                    recoverable: true,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "continued".into(),
                    is_final: true,
                }),
            ),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].turn_id, rows[1].turn_id);
        assert_ne!(rows[1].turn_id, rows[2].turn_id);
        assert!(rows[1].turn_failed);
        assert!(rows[1].conclusion);
        assert!(!rows[2].turn_failed);
        assert!(rows[2].conclusion);
    }

    #[test]
    fn turn_preview_projection_keeps_first_virtual_row_per_turn() {
        let rows = timeline_rows(&[
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "try".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                2,
                None,
                TimelinePayload::Error(vibex_core::TimelineErrorPayload {
                    code: "turn_failed".into(),
                    message: "failed".into(),
                    recoverable: true,
                }),
            ),
            item(
                3,
                None,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "continued".into(),
                    is_final: true,
                }),
            ),
        ]);
        let previews = timeline_turn_previews(&rows);
        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].row_index, 0);
        assert_eq!(previews[0].item_count, 2);
        assert!(previews[0].failed);
        assert_eq!(previews[1].row_index, 2);
        assert_eq!(previews[1].item_count, 1);
        assert!(!previews[1].failed);
    }

    #[test]
    fn follow_state_does_not_steal_scroll_when_reader_is_displaced() {
        let mut state = TimelineFollowState::default();
        state.preserve_anchor("agent:turn_a", 18);
        state.content_appended(3);
        assert!(!state.following_bottom);
        assert_eq!(state.unread_count, 3);
        assert_eq!(state.anchor_row_id.as_deref(), Some("agent:turn_a"));
        state.set_following_bottom(true);
        assert_eq!(state.unread_count, 0);
        assert!(state.anchor_row_id.is_none());
    }

    #[test]
    fn user_message_is_projected_as_a_distinct_row() {
        let rows = timeline_rows(&[item(
            1,
            None,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "ship it".into(),
                attachments: Vec::new(),
            }),
        )]);
        assert_eq!(rows[0].kind, TimelineRowKind::UserMessage);
        assert_eq!(rows[0].body, "ship it");
    }

    #[test]
    fn permission_rows_resolve_independently_within_one_turn() {
        let session_id = VibexSessionId::parse("session_current").unwrap();
        let first_request_id = vibex_core::RequestId::parse("request_first").unwrap();
        let second_request_id = vibex_core::RequestId::parse("request_second").unwrap();
        let permission_request = |id: vibex_core::RequestId, requested_at_ms| {
            TimelinePayload::PermissionRequest(vibex_core::PermissionRequest {
                id,
                session_id: session_id.clone(),
                project_id: None,
                workspace_id: None,
                provider_request_id: None,
                risk_category: vibex_core::PermissionRiskCategory::Command,
                title: "Run command?".into(),
                details: Vec::new(),
                allowed_responses: vec![
                    vibex_core::PermissionResponseKind::Approve,
                    vibex_core::PermissionResponseKind::Deny,
                ],
                response_options: Vec::new(),
                status: PermissionRequestStatus::Pending,
                requested_at_ms,
                expires_at_ms: None,
            })
        };
        let rows = timeline_rows(&[
            item(
                1,
                None,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "run both".into(),
                    attachments: Vec::new(),
                }),
            ),
            item(2, None, permission_request(first_request_id.clone(), 2)),
            item(3, None, permission_request(second_request_id, 3)),
            item(
                4,
                None,
                TimelinePayload::PermissionResolution(vibex_core::PermissionResolution {
                    request_id: first_request_id,
                    session_id,
                    response: vibex_core::PermissionResponseKind::Approve,
                    responder_device_id: None,
                    provider_resolution_id: None,
                    note: None,
                    resolved_at_ms: 4,
                }),
            ),
        ]);
        let permission_rows = rows
            .iter()
            .filter(|row| row.kind == TimelineRowKind::PermissionRequest)
            .collect::<Vec<_>>();

        assert_eq!(permission_rows.len(), 2);
        assert!(!permission_rows[0].pending_permission);
        assert!(permission_rows[0].turn_pending_permission);
        assert!(permission_rows[1].pending_permission);
        assert!(permission_rows[1].turn_pending_permission);
    }

    #[test]
    fn every_canonical_timeline_kind_has_a_product_row_projection() {
        let payloads = serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
            "../tests/fixtures/agent-timeline-kinds-v1.json"
        ))
        .unwrap();
        let items = payloads
            .into_iter()
            .enumerate()
            .map(|(index, fixture)| {
                let kind = fixture["kind"].as_str().unwrap();
                let payload = fixture["payload"].clone();
                serde_json::from_value::<TimelineItem>(json!({
                    "id": format!("timeline_{}", index + 1),
                    "sessionId": "session_current",
                    "sequence": index + 1,
                    "timestampMs": index + 1,
                    "source": if kind == "user_message" { "user" } else { "agent" },
                    "kind": kind,
                    "correlationId": null,
                    "providerCorrelationId": null,
                    "redactionState": "none",
                    "payload": payload
                }))
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(timeline_kind_coverage(&items).len(), 19);
        let rows = timeline_rows(&items);
        assert_eq!(rows.len(), 18);
        assert_eq!(rows.iter().map(|row| row.item_ids.len()).sum::<usize>(), 19);
        assert!(rows.iter().any(|row| {
            row.kind == TimelineRowKind::PermissionRequest && !row.pending_permission
        }));
        assert!(
            rows.iter()
                .any(|row| row.kind == TimelineRowKind::Error && row.failed)
        );
        assert!(rows.iter().any(|row| {
            row.kind == TimelineRowKind::ElicitationRequest && !row.pending_permission
        }));
        assert!(rows.iter().any(|row| {
            row.kind == TimelineRowKind::AgentMessage && !row.streaming && row.item_ids.len() == 2
        }));
        assert!(
            rows.iter()
                .find(|row| row.kind == TimelineRowKind::FileOperation)
                .is_some_and(|row| row.file_path.as_deref() == Some("src/lib.rs"))
        );
    }

    #[test]
    fn large_timeline_and_streaming_projection_stay_row_bounded() {
        let session_id = VibexSessionId::parse("session_current").unwrap();
        let history = (1..=5_000)
            .map(|sequence| {
                item(
                    sequence,
                    None,
                    TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                        level: vibex_core::SystemNoticeLevel::Info,
                        message: format!("notice {sequence}"),
                    }),
                )
            })
            .collect::<Vec<_>>();
        let mut model = crate::TimelineModel::default();
        model.replace_authoritative(session_id, history);
        assert_eq!(model.rows().len(), 5_000);

        let streaming = (1..=1_800)
            .map(|sequence| {
                item(
                    sequence,
                    Some("correlation_stream"),
                    TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                        text_delta: "x".into(),
                        chunk_index: sequence as u32 - 1,
                        phase: None,
                    }),
                )
            })
            .collect::<Vec<_>>();
        let rows = timeline_rows(&streaming);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body.len(), 1_800);
        assert_eq!(rows[0].item_ids.len(), 2);
        assert_eq!(rows[0].item_ids[0], streaming[0].id.as_str());
        assert_eq!(rows[0].item_ids[1], streaming[1_799].id.as_str());
        assert_eq!(rows[0].first_sequence, 1);
        assert_eq!(rows[0].last_sequence, 1_800);
    }

    #[test]
    fn large_conversation_projection_stays_turn_bounded() {
        let items = (0..2_500_i64)
            .flat_map(|turn_index| {
                let user_sequence = turn_index * 2 + 1;
                let agent_sequence = user_sequence + 1;
                [
                    item(
                        user_sequence,
                        None,
                        TimelinePayload::UserMessage(UserMessagePayload {
                            text: format!("message {turn_index}"),
                            attachments: Vec::new(),
                        }),
                    ),
                    item(
                        agent_sequence,
                        None,
                        TimelinePayload::AgentMessage(AgentMessagePayload {
                            text: format!("response {turn_index}"),
                            is_final: true,
                        }),
                    ),
                ]
            })
            .collect::<Vec<_>>();

        let turns = timeline_conversation_turns(&items, Some(AgentSessionState::Idle), false);

        assert_eq!(turns.len(), 2_500);
        assert_eq!(
            turns.iter().map(|turn| turn.item_count).sum::<usize>(),
            5_000
        );
        assert!(turns.iter().all(|turn| {
            turn.user_row.is_some() && turn.conclusion_row.is_some() && turn.process_rows.is_empty()
        }));
    }

    #[test]
    #[ignore = "five-minute wall-clock GPUI stream/session-switch soak gate"]
    fn agent_stream_switch_soak_is_bounded() {
        let duration_seconds = std::env::var("VIBEX_AGENT_SOAK_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(300)
            .max(1);
        let sessions = [
            VibexSessionId::parse("session_soak_a").unwrap(),
            VibexSessionId::parse("session_soak_b").unwrap(),
        ];
        let mut active_index = 0_usize;
        let mut model = crate::TimelineModel::default();
        model.replace_authoritative(sessions[active_index].clone(), Vec::new());
        let started = std::time::Instant::now();
        let mut tick = 0_i64;
        let mut max_rows = 0_usize;
        let mut max_items = 0_usize;

        while started.elapsed() < std::time::Duration::from_secs(duration_seconds) {
            tick += 1;
            if tick % 30 == 0 {
                active_index = 1 - active_index;
                model.replace_authoritative(sessions[active_index].clone(), Vec::new());
            }
            let active = sessions[active_index].clone();
            let stale = sessions[1 - active_index].clone();
            let sequence = tick % 30 + 1;
            let active_item = TimelineItem {
                session_id: active.clone(),
                ..item(
                    sequence,
                    Some("correlation_soak"),
                    TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                        text_delta: "x".into(),
                        chunk_index: sequence as u32 - 1,
                        phase: None,
                    }),
                )
            };
            assert!(model.apply_live(vibex_core::TimelineLiveEvent {
                session_id: active,
                sequence,
                item: active_item,
            }));
            let stale_item = TimelineItem {
                session_id: stale.clone(),
                ..item(
                    sequence,
                    Some("correlation_stale"),
                    TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                        text_delta: "stale".into(),
                        chunk_index: sequence as u32 - 1,
                        phase: None,
                    }),
                )
            };
            assert!(!model.apply_live(vibex_core::TimelineLiveEvent {
                session_id: stale,
                sequence,
                item: stale_item,
            }));
            let rows = model.rows();
            max_rows = max_rows.max(rows.len());
            max_items = max_items.max(rows.iter().map(|row| row.item_ids.len()).sum());
            assert!(
                max_rows <= 1,
                "stream projection must remain one affected row"
            );
            assert!(
                max_items <= 30,
                "session switches must bound active stream items"
            );
            std::thread::sleep(std::time::Duration::from_millis(33));
        }

        assert!(tick >= duration_seconds as i64 * 25);
    }
}
