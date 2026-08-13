//! Pure management-center projections and reducers.
//!
//! These types intentionally contain only redacted/display-safe values.  The
//! runtime owns durable records and side effects; GPUI owns entity lifetime and
//! delegates actions to the typed desktop runtime facade.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentCatalogListResponse, AgentId, AgentSnapshotEntry, AutomationEdgeCondition,
    AutomationEdgeCreateRequest, AutomationGraph, AutomationGraphDefinitionUpdateRequest,
    AutomationNodeConfig, AutomationNodeCreateRequest, AutomationNodeId, AutomationNodePosition,
    McpServer, Prompt, ProviderKind, ProviderProfile, ProviderProfileStatus, ScheduledTask,
    ScheduledTaskAttentionSummary, ScheduledTaskAuditRecord, ScheduledTaskRun, Skill,
};

use crate::{AsyncGeneration, MutationState, QueryError, QueryState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ManagementSection {
    #[default]
    Agents,
    ModelProviders,
    Mcp,
    Skills,
    PromptsHooks,
    Advanced,
    Scheduled,
    Automation,
    Relay,
    Recovery,
}

impl ManagementSection {
    pub const ALL: [Self; 10] = [
        Self::Agents,
        Self::ModelProviders,
        Self::Mcp,
        Self::Skills,
        Self::PromptsHooks,
        Self::Advanced,
        Self::Scheduled,
        Self::Automation,
        Self::Relay,
        Self::Recovery,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::ModelProviders => "model-providers",
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::PromptsHooks => "prompts-hooks",
            Self::Advanced => "advanced",
            Self::Scheduled => "scheduled",
            Self::Automation => "automation",
            Self::Relay => "relay",
            Self::Recovery => "recovery",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::ModelProviders => "Model Providers",
            Self::Mcp => "MCP",
            Self::Skills => "Skills",
            Self::PromptsHooks => "Prompts & Hooks",
            Self::Advanced => "Advanced",
            Self::Scheduled => "Scheduled",
            Self::Automation => "Automation",
            Self::Relay => "Relay",
            Self::Recovery => "Recovery",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.key() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementNavigation {
    pub active: ManagementSection,
    pub generation: u64,
    dirty: BTreeSet<ManagementSection>,
}

impl Default for ManagementNavigation {
    fn default() -> Self {
        Self {
            active: ManagementSection::Agents,
            generation: 0,
            dirty: BTreeSet::new(),
        }
    }
}

impl ManagementNavigation {
    pub fn is_dirty(&self, section: ManagementSection) -> bool {
        self.dirty.contains(&section)
    }

    pub fn mark_dirty(&mut self, section: ManagementSection, dirty: bool) {
        if dirty {
            self.dirty.insert(section);
        } else {
            self.dirty.remove(&section);
        }
    }

    pub fn dirty_sections(&self) -> impl Iterator<Item = ManagementSection> + '_ {
        self.dirty.iter().copied()
    }

    /// Switches section only when the current dirty section is explicitly
    /// confirmed. The generation increments for every accepted switch so old
    /// async completions can be discarded by the owner.
    pub fn switch(&mut self, next: ManagementSection, discard_dirty_current: bool) -> bool {
        if next == self.active {
            return true;
        }
        if self.is_dirty(self.active) && !discard_dirty_current {
            return false;
        }
        if discard_dirty_current {
            self.mark_dirty(self.active, false);
        }
        self.active = next;
        self.generation = self.generation.saturating_add(1);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementResourceState<T> {
    pub query: QueryState<T>,
    pub mutation: MutationState<T>,
    pub draft: Option<T>,
    pub dirty: bool,
}

impl<T> Default for ManagementResourceState<T> {
    fn default() -> Self {
        Self {
            query: QueryState::default(),
            mutation: MutationState::default(),
            draft: None,
            dirty: false,
        }
    }
}

impl<T> ManagementResourceState<T> {
    pub fn begin_query(&mut self) -> AsyncGeneration {
        self.query.begin()
    }

    pub fn resolve_query(&mut self, generation: AsyncGeneration, value: T, now_ms: i64) -> bool {
        self.query.resolve(generation, value, now_ms)
    }

    pub fn reject_query(&mut self, generation: AsyncGeneration, error: QueryError) -> bool {
        self.query.reject(generation, error)
    }

    pub fn preserve_draft_on_refetch(&mut self, value: T) {
        if !self.dirty {
            self.draft = Some(value);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfileDraft {
    pub profile_id: Option<String>,
    pub agent_id: Option<AgentId>,
    pub kind: ProviderKind,
    pub display_name: String,
    pub account_alias: String,
    pub base_url: String,
    pub default_model: String,
    pub reasoning_effort: String,
    pub status: ProviderProfileStatus,
    /// Secret values are deliberately not represented in this model.
    pub api_key_touched: bool,
    transient_secret: Option<TransientSecret>,
}

#[derive(Clone, PartialEq, Eq)]
struct TransientSecret(String);

impl fmt::Debug for TransientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransientSecret([redacted])")
    }
}

impl Drop for TransientSecret {
    fn drop(&mut self) {
        // Keep the transient value out of allocator reuse where practical.
        // It is never serialized or logged, and the owning draft is short-lived.
        unsafe { self.0.as_bytes_mut().fill(0) };
    }
}

impl ProviderProfileDraft {
    pub fn empty(kind: ProviderKind) -> Self {
        Self {
            profile_id: None,
            agent_id: None,
            kind,
            display_name: String::new(),
            account_alias: String::new(),
            base_url: String::new(),
            default_model: String::new(),
            reasoning_effort: String::new(),
            status: ProviderProfileStatus::Enabled,
            api_key_touched: false,
            transient_secret: None,
        }
    }

    pub fn from_profile(profile: &ProviderProfile) -> Self {
        Self {
            profile_id: Some(profile.id.as_str().to_string()),
            agent_id: Some(profile.agent_id.clone()),
            kind: profile.kind,
            display_name: profile.display_name.clone(),
            account_alias: profile.account_alias.clone().unwrap_or_default(),
            base_url: profile.base_url.clone().unwrap_or_default(),
            default_model: profile.default_model.clone().unwrap_or_default(),
            reasoning_effort: profile.reasoning_effort.clone().unwrap_or_default(),
            status: profile.status,
            api_key_touched: false,
            transient_secret: None,
        }
    }

    pub fn set_transient_secret(&mut self, value: String) {
        self.transient_secret = Some(TransientSecret(value));
        self.api_key_touched = true;
    }

    pub fn clear_transient_secret(&mut self) {
        self.transient_secret = None;
        self.api_key_touched = true;
    }

    pub fn has_transient_secret(&self) -> bool {
        self.transient_secret.is_some()
    }

    pub fn redacted_summary(&self) -> ProviderDraftRedactedSummary {
        ProviderDraftRedactedSummary {
            profile_id: self.profile_id.clone(),
            display_name: self.display_name.clone(),
            kind: self.kind,
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            secret_touched: self.api_key_touched,
            secret_configured: self.transient_secret.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDraftRedactedSummary {
    pub profile_id: Option<String>,
    pub display_name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub default_model: String,
    pub secret_touched: bool,
    pub secret_configured: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCenterSnapshot {
    pub agents: Vec<AgentSnapshotEntry>,
    pub catalog: Option<AgentCatalogListResponse>,
    pub profiles: Vec<ProviderProfileProjection>,
    pub mcp_servers: Vec<McpServer>,
    pub skills: Vec<Skill>,
    pub prompts: Vec<Prompt>,
    pub hooks: Vec<vibex_core::Hook>,
    pub scheduled: Vec<ScheduledTask>,
    pub graphs: Vec<AutomationGraph>,
}

/// Display-safe provider profile projection.  Unlike `ProviderProfile`, this
/// type intentionally omits provider options and secret references so it can
/// be retained by a GPUI entity or included in a contract fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileProjection {
    pub id: String,
    pub agent_id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    pub base_url: Option<String>,
    pub status: ProviderProfileStatus,
    pub account_alias: Option<String>,
    pub default_model: Option<String>,
    pub configured_model_count: usize,
    pub secret_configured: bool,
    pub updated_at_ms: i64,
}

impl ProviderProfileProjection {
    pub fn from_profile(profile: &ProviderProfile) -> Self {
        Self {
            id: profile.id.as_str().to_string(),
            agent_id: profile.agent_id.as_str().to_string(),
            kind: profile.kind,
            display_name: profile.display_name.clone(),
            base_url: profile.base_url.clone(),
            status: profile.status,
            account_alias: profile.account_alias.clone(),
            default_model: profile.default_model.clone(),
            configured_model_count: profile.configured_models.len(),
            secret_configured: !profile.secrets.is_empty(),
            updated_at_ms: profile.updated_at_ms,
        }
    }
}

impl ProviderCenterSnapshot {
    pub fn redacted_profiles(&self) -> &[ProviderProfileProjection] {
        &self.profiles
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledManagementState {
    pub tasks: Vec<ScheduledTask>,
    pub runs: Vec<ScheduledTaskRun>,
    pub attention: Vec<ScheduledTaskAttentionSummary>,
    pub audit: Vec<ScheduledTaskAuditRecord>,
    pub selected_task_id: Option<String>,
    pub destructive_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RelayManagementState {
    pub status: String,
    pub settings_loaded: bool,
    pub trusted_device_count: usize,
    pub revoked_device_count: usize,
    pub audit_count: usize,
    pub retryable_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOperationState {
    pub operation: String,
    pub phase: String,
    pub progress_percent: u8,
    pub destination: Option<String>,
    pub rollback_available: bool,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementLoadState {
    Idle,
    Loading,
    Ready,
    Empty,
    Partial,
    Disabled,
    Error,
    MutationPending,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ManagementError {
    pub fn from_vibex(error: &vibex_core::VibexError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            retryable: !matches!(
                error.category,
                vibex_core::ErrorCategory::Validation | vibex_core::ErrorCategory::Permission
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationGraphDraft {
    pub graph_id: Option<String>,
    pub title: String,
    pub description: String,
    pub nodes: Vec<AutomationNodeDraft>,
    pub edges: Vec<AutomationEdgeDraft>,
    pub selected_node_ids: BTreeSet<String>,
    pub viewport: GraphViewport,
    pub base_version: Option<u32>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationNodeDraft {
    pub id: String,
    pub kind: vibex_core::AutomationNodeKind,
    pub title: String,
    pub config: AutomationNodeConfig,
    pub position: GraphPosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationEdgeDraft {
    pub source_node_id: String,
    pub target_node_id: String,
    pub condition: AutomationEdgeCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphViewport {
    pub zoom_percent: u16,
    pub pan_x: i32,
    pub pan_y: i32,
}

impl Default for GraphViewport {
    fn default() -> Self {
        Self {
            zoom_percent: 100,
            pan_x: 0,
            pan_y: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphValidationIssue {
    pub code: &'static str,
    pub node_id: Option<String>,
    pub edge_index: Option<usize>,
    pub message: String,
}

impl AutomationGraphDraft {
    pub fn empty() -> Self {
        Self {
            graph_id: None,
            title: String::new(),
            description: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            selected_node_ids: BTreeSet::new(),
            viewport: GraphViewport::default(),
            base_version: None,
            dirty: false,
        }
    }

    pub fn from_graph(graph: &AutomationGraph) -> Self {
        Self {
            graph_id: Some(graph.id.as_str().to_string()),
            title: graph.title.clone(),
            description: graph.description.clone().unwrap_or_default(),
            nodes: graph
                .nodes
                .iter()
                .map(|node| AutomationNodeDraft {
                    id: node.id.as_str().to_string(),
                    kind: node.kind,
                    title: node.title.clone(),
                    config: node.config.clone(),
                    position: node
                        .position
                        .as_ref()
                        .map(|position| GraphPosition {
                            x: position.x,
                            y: position.y,
                        })
                        .unwrap_or(GraphPosition { x: 40, y: 40 }),
                })
                .collect(),
            edges: graph
                .edges
                .iter()
                .map(|edge| AutomationEdgeDraft {
                    source_node_id: edge.source_node_id.as_str().to_string(),
                    target_node_id: edge.target_node_id.as_str().to_string(),
                    condition: edge.condition.clone(),
                })
                .collect(),
            selected_node_ids: BTreeSet::new(),
            viewport: GraphViewport::default(),
            base_version: Some(graph.version),
            dirty: false,
        }
    }

    pub fn add_node(
        &mut self,
        id: AutomationNodeId,
        kind: vibex_core::AutomationNodeKind,
        title: impl Into<String>,
        config: AutomationNodeConfig,
    ) -> bool {
        let id = id.as_str().to_string();
        if self.nodes.iter().any(|node| node.id == id) {
            return false;
        }
        let offset = 40 + (self.nodes.len() as i32 % 5) * 180;
        self.nodes.push(AutomationNodeDraft {
            id: id.clone(),
            kind,
            title: title.into(),
            config,
            position: GraphPosition {
                x: offset,
                y: 40 + (self.nodes.len() as i32 / 5) * 120,
            },
        });
        self.selected_node_ids.clear();
        self.selected_node_ids.insert(id);
        self.dirty = true;
        true
    }

    pub fn move_node(&mut self, id: &str, position: GraphPosition) -> bool {
        let Some(node) = self.nodes.iter_mut().find(|node| node.id == id) else {
            return false;
        };
        if node.position == position {
            return false;
        }
        node.position = position;
        self.dirty = true;
        true
    }

    pub fn connect(
        &mut self,
        source_node_id: &str,
        target_node_id: &str,
        condition: AutomationEdgeCondition,
    ) -> Result<(), GraphValidationIssue> {
        if source_node_id == target_node_id {
            return Err(GraphValidationIssue {
                code: "automation_graph_self_edge",
                node_id: Some(source_node_id.to_string()),
                edge_index: None,
                message: "A node cannot connect to itself".to_string(),
            });
        }
        if !self.nodes.iter().any(|node| node.id == source_node_id) {
            return Err(GraphValidationIssue {
                code: "automation_graph_edge_source_missing",
                node_id: Some(source_node_id.to_string()),
                edge_index: None,
                message: "The edge source node is missing".to_string(),
            });
        }
        if !self.nodes.iter().any(|node| node.id == target_node_id) {
            return Err(GraphValidationIssue {
                code: "automation_graph_edge_target_missing",
                node_id: Some(target_node_id.to_string()),
                edge_index: None,
                message: "The edge target node is missing".to_string(),
            });
        }
        if self.edges.iter().any(|edge| {
            edge.source_node_id == source_node_id && edge.target_node_id == target_node_id
        }) {
            return Err(GraphValidationIssue {
                code: "automation_graph_duplicate_edge",
                node_id: None,
                edge_index: None,
                message: "The edge already exists".to_string(),
            });
        }
        self.edges.push(AutomationEdgeDraft {
            source_node_id: source_node_id.to_string(),
            target_node_id: target_node_id.to_string(),
            condition,
        });
        self.dirty = true;
        Ok(())
    }

    pub fn delete_selection(&mut self) -> bool {
        if self.selected_node_ids.is_empty() {
            return false;
        }
        let before_nodes = self.nodes.len();
        self.nodes
            .retain(|node| !self.selected_node_ids.contains(&node.id));
        self.edges.retain(|edge| {
            !self.selected_node_ids.contains(&edge.source_node_id)
                && !self.selected_node_ids.contains(&edge.target_node_id)
        });
        self.selected_node_ids.clear();
        let changed = before_nodes != self.nodes.len();
        self.dirty |= changed;
        changed
    }

    pub fn zoom_by(&mut self, delta_percent: i16) {
        let next = (self.viewport.zoom_percent as i16 + delta_percent).clamp(40, 220);
        self.viewport.zoom_percent = next as u16;
    }

    pub fn pan_by(&mut self, dx: i32, dy: i32) {
        self.viewport.pan_x = self.viewport.pan_x.saturating_add(dx);
        self.viewport.pan_y = self.viewport.pan_y.saturating_add(dy);
    }

    pub fn mark_saved(&mut self, version: u32) {
        self.base_version = Some(version);
        self.dirty = false;
    }

    /// A failed compare-and-swap keeps the local draft intact while updating
    /// only the diagnostic revision, allowing the caller to offer reload or
    /// explicit overwrite through a fresh action.
    pub fn preserve_after_conflict(&mut self, current_version: Option<u32>) {
        if current_version.is_some() {
            self.base_version = current_version;
        }
        self.dirty = true;
    }

    pub fn validate(&self) -> Vec<GraphValidationIssue> {
        let mut issues = Vec::new();
        if self.title.trim().is_empty() {
            issues.push(GraphValidationIssue {
                code: "automation_graph_title_empty",
                node_id: None,
                edge_index: None,
                message: "Automation graph title is required".to_string(),
            });
        }
        if self.nodes.is_empty() {
            issues.push(GraphValidationIssue {
                code: "automation_graph_nodes_empty",
                node_id: None,
                edge_index: None,
                message: "Add at least one automation node".to_string(),
            });
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.clone()) {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_duplicate_node_id",
                    node_id: Some(node.id.clone()),
                    edge_index: None,
                    message: "Node ids must be unique".to_string(),
                });
            }
            if node.title.trim().is_empty() {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_node_title_empty",
                    node_id: Some(node.id.clone()),
                    edge_index: None,
                    message: "Node title is required".to_string(),
                });
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            if !ids.contains(&edge.source_node_id) {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_edge_source_missing",
                    node_id: Some(edge.source_node_id.clone()),
                    edge_index: Some(index),
                    message: "The edge source node is missing".to_string(),
                });
            }
            if !ids.contains(&edge.target_node_id) {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_edge_target_missing",
                    node_id: Some(edge.target_node_id.clone()),
                    edge_index: Some(index),
                    message: "The edge target node is missing".to_string(),
                });
            }
            if edge.source_node_id == edge.target_node_id {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_self_edge",
                    node_id: Some(edge.source_node_id.clone()),
                    edge_index: Some(index),
                    message: "A node cannot connect to itself".to_string(),
                });
            }
        }
        let mut edge_pairs = BTreeSet::new();
        for (index, edge) in self.edges.iter().enumerate() {
            let pair = (edge.source_node_id.clone(), edge.target_node_id.clone());
            if !edge_pairs.insert(pair) {
                issues.push(GraphValidationIssue {
                    code: "automation_graph_duplicate_edge",
                    node_id: None,
                    edge_index: Some(index),
                    message: "The edge already exists".to_string(),
                });
            }
        }
        issues
    }

    pub fn to_definition_request(
        &self,
    ) -> Result<AutomationGraphDefinitionUpdateRequest, Vec<GraphValidationIssue>> {
        let issues = self.validate();
        if !issues.is_empty() {
            return Err(issues);
        }
        let graph_id = self
            .graph_id
            .as_deref()
            .and_then(|id| vibex_core::AutomationGraphId::parse(id).ok())
            .ok_or_else(|| {
                vec![GraphValidationIssue {
                    code: "automation_graph_id_missing",
                    node_id: None,
                    edge_index: None,
                    message: "Save the graph before replacing its definition".to_string(),
                }]
            })?;
        Ok(AutomationGraphDefinitionUpdateRequest {
            graph_id,
            nodes: self
                .nodes
                .iter()
                .map(|node| AutomationNodeCreateRequest {
                    id: vibex_core::AutomationNodeId::parse(node.id.clone()).ok(),
                    kind: node.kind,
                    title: node.title.clone(),
                    config: node.config.clone(),
                    position: Some(AutomationNodePosition {
                        x: node.position.x,
                        y: node.position.y,
                    }),
                })
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|edge| AutomationEdgeCreateRequest {
                    source_node_id: vibex_core::AutomationNodeId::parse(
                        edge.source_node_id.clone(),
                    )
                    .expect("validated automation edge source id"),
                    target_node_id: vibex_core::AutomationNodeId::parse(
                        edge.target_node_id.clone(),
                    )
                    .expect("validated automation edge target id"),
                    condition: edge.condition.clone(),
                })
                .collect(),
            expected_version: self.base_version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingContextProjection {
    pub workspace: Option<String>,
    pub session_id: Option<String>,
    pub mode: String,
}

impl PairingContextProjection {
    pub fn new(
        workspace: Option<String>,
        session_id: Option<String>,
        mode: impl Into<String>,
    ) -> Self {
        Self {
            workspace,
            session_id,
            mode: mode.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedDiagnosticProjection {
    pub status: String,
    pub destination: Option<String>,
    pub record_count: u32,
    pub redaction_verified: bool,
    pub error_code: Option<String>,
}

impl Default for RedactedDiagnosticProjection {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            destination: None,
            record_count: 0,
            redaction_verified: false,
            error_code: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementStateSummary {
    pub section: ManagementSection,
    pub generation: u64,
    pub dirty_section_count: usize,
    pub graph_dirty: bool,
    pub diagnostic_status: String,
}

pub fn management_state_summary(
    navigation: &ManagementNavigation,
    graph: &AutomationGraphDraft,
    diagnostics: &RedactedDiagnosticProjection,
) -> ManagementStateSummary {
    ManagementStateSummary {
        section: navigation.active,
        generation: navigation.generation,
        dirty_section_count: ManagementSection::ALL
            .into_iter()
            .filter(|section| navigation.is_dirty(*section))
            .count(),
        graph_dirty: graph.dirty,
        diagnostic_status: diagnostics.status.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AutomationEdgeConditionKind, AutomationNodeKind, AutomationNodePosition, WorkspaceMode,
    };

    fn prompt_config() -> AutomationNodeConfig {
        AutomationNodeConfig::AgentPrompt(vibex_core::AutomationAgentPromptConfig {
            prompt_template: "Review the diff".to_string(),
            provider_kind: Some(ProviderKind::Acp),
            provider_profile_id: None,
            safety: None,
            workspace_root: None,
            workspace_mode: Some(WorkspaceMode::CurrentCheckout),
        })
    }

    #[test]
    fn section_switch_preserves_dirty_state_until_explicit_discard() {
        let mut nav = ManagementNavigation::default();
        nav.mark_dirty(ManagementSection::Agents, true);
        assert!(!nav.switch(ManagementSection::Mcp, false));
        assert_eq!(nav.active, ManagementSection::Agents);
        assert!(nav.switch(ManagementSection::Mcp, true));
        assert_eq!(nav.generation, 1);
        assert!(!nav.is_dirty(ManagementSection::Agents));
    }

    #[test]
    fn graph_reducer_rejects_self_and_duplicate_edges() {
        let mut draft = AutomationGraphDraft::empty();
        draft.title = "Flow".to_string();
        let first = AutomationNodeId::new();
        let second = AutomationNodeId::new();
        assert!(draft.add_node(
            first.clone(),
            AutomationNodeKind::AgentPrompt,
            "One",
            prompt_config()
        ));
        assert!(draft.add_node(
            second.clone(),
            AutomationNodeKind::ApprovalGate,
            "Two",
            AutomationNodeConfig::ApprovalGate(vibex_core::AutomationApprovalGateConfig {
                title: "Approve".to_string(),
                details: "Review".to_string(),
                risk_category: vibex_core::PermissionRiskCategory::Command,
                allowed_responses: vec![vibex_core::PermissionResponseKind::Approve],
            })
        ));
        let condition = AutomationEdgeCondition {
            kind: AutomationEdgeConditionKind::OnSuccess,
            expression: None,
        };
        assert!(
            draft
                .connect(first.as_str(), first.as_str(), condition.clone())
                .is_err()
        );
        assert!(
            draft
                .connect(first.as_str(), second.as_str(), condition.clone())
                .is_ok()
        );
        assert!(
            draft
                .connect(first.as_str(), second.as_str(), condition)
                .is_err()
        );
        assert_eq!(draft.validate().len(), 0);
    }

    #[test]
    fn graph_definition_carries_cas_version_and_stable_positions() {
        let mut draft = AutomationGraphDraft::empty();
        draft.graph_id = Some("automation_graph_test".to_string());
        draft.title = "Review".to_string();
        draft.base_version = Some(7);
        let id = AutomationNodeId::new();
        draft.add_node(
            id,
            AutomationNodeKind::AgentPrompt,
            "Review",
            prompt_config(),
        );
        let request = draft.to_definition_request().unwrap();
        assert_eq!(request.expected_version, Some(7));
        assert_eq!(
            request.nodes[0].position,
            Some(AutomationNodePosition { x: 40, y: 40 })
        );
    }

    #[test]
    fn pairing_projection_contains_only_non_secret_selection_metadata() {
        let projection = PairingContextProjection::new(
            Some("/workspace".to_string()),
            Some("session_mock".to_string()),
            "current_checkout",
        );
        assert_eq!(projection.workspace.as_deref(), Some("/workspace"));
        assert_eq!(projection.session_id.as_deref(), Some("session_mock"));
        assert_eq!(projection.mode, "current_checkout");
        let json = serde_json::to_string(&projection).unwrap();
        assert!(!json.contains("127.0.0.1:1421"));
        assert!(!json.contains("fixture"));
        assert!(!json.to_lowercase().contains("qr"));
    }
    #[test]
    fn provider_draft_debug_and_serialized_summary_never_contain_secret() {
        let mut draft = ProviderProfileDraft::empty(ProviderKind::Acp);
        draft.set_transient_secret("sentinel-secret-value".to_string());
        let debug = format!("{draft:?}");
        assert!(!debug.contains("sentinel-secret-value"));
        let summary = serde_json::to_string(&draft.redacted_summary()).unwrap();
        assert!(!summary.contains("sentinel-secret-value"));
        assert!(summary.contains("secretConfigured"));
    }
}
