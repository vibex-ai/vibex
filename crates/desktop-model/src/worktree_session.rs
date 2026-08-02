use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentSession, AgentSessionState, GitProjectEligibility, GitWorktreeCreateRequest,
    GitWorktreeLifecycleSnapshot, ProjectId, ProjectRecord, WorkspaceId, WorkspaceMode,
    WorkspaceRecord, managed_worktree_name_slug,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewSessionLocation {
    NewWorktree,
    #[default]
    #[serde(other)]
    CurrentCheckout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SidebarHierarchyMode {
    Detailed,
    #[default]
    #[serde(other)]
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewSessionSubmissionStage {
    #[default]
    Idle,
    CreatingWorktree,
    CreatingSession,
    StartingAgent,
    SendingPrompt,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionSubmissionState {
    pub stage: NewSessionSubmissionStage,
    pub idempotency_key: String,
    pub created_workspace: Option<WorkspaceRecord>,
    pub error_code: Option<String>,
}

impl Default for NewSessionSubmissionState {
    fn default() -> Self {
        Self {
            stage: NewSessionSubmissionStage::Idle,
            idempotency_key: String::new(),
            created_workspace: None,
            error_code: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionProjectTicket {
    pub generation: u64,
    pub project_id: ProjectId,
    pub origin_workspace_id: WorkspaceId,
    pub project_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewSessionWorkspaceState {
    pub project_id: Option<ProjectId>,
    pub origin_workspace_id: Option<WorkspaceId>,
    pub project_root: String,
    pub fixed_workspace: Option<WorkspaceRecord>,
    pub preference: NewSessionLocation,
    pub location: NewSessionLocation,
    pub location_touched: bool,
    pub eligibility: Option<GitProjectEligibility>,
    pub generation: u64,
    pub base_ref: Option<String>,
    pub base_ref_touched: bool,
    pub worktree_name: String,
    pub worktree_path: String,
    pub name_touched: bool,
    pub path_touched: bool,
    pub suggestion_nonce: String,
    pub managed_root: PathBuf,
    pub submission: NewSessionSubmissionState,
}

impl NewSessionWorkspaceState {
    pub fn clear(&mut self, nonce: impl Into<String>, managed_root: impl Into<PathBuf>) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            suggestion_nonce: nonce.into(),
            managed_root: managed_root.into(),
            submission: NewSessionSubmissionState::default(),
            ..Self::default()
        };
    }

    pub fn select_project(
        &mut self,
        project: &ProjectRecord,
        origin_workspace: &WorkspaceRecord,
        preference: NewSessionLocation,
        nonce: impl Into<String>,
        managed_root: impl Into<PathBuf>,
        prompt_title: Option<&str>,
    ) -> NewSessionProjectTicket {
        self.generation = self.generation.wrapping_add(1);
        self.project_id = Some(project.id.clone());
        self.origin_workspace_id = Some(origin_workspace.id.clone());
        self.project_root = origin_workspace.root_path.clone();
        self.fixed_workspace = None;
        self.preference = preference;
        self.location = NewSessionLocation::CurrentCheckout;
        self.location_touched = false;
        self.eligibility = None;
        self.base_ref = None;
        self.base_ref_touched = false;
        self.name_touched = false;
        self.path_touched = false;
        self.suggestion_nonce = nonce.into();
        self.managed_root = managed_root.into();
        self.submission = NewSessionSubmissionState {
            idempotency_key: format!(
                "new-session-worktree:{}:{}",
                project.id.as_str(),
                bounded_nonce(&self.suggestion_nonce)
            ),
            ..NewSessionSubmissionState::default()
        };
        self.refresh_auto_values(prompt_title);
        self.ticket()
            .expect("selected project must produce a ticket")
    }

    pub fn select_existing_workspace(
        &mut self,
        project: &ProjectRecord,
        origin_workspace: &WorkspaceRecord,
        workspace: &WorkspaceRecord,
        nonce: impl Into<String>,
        managed_root: impl Into<PathBuf>,
        prompt_title: Option<&str>,
    ) -> NewSessionProjectTicket {
        let ticket = self.select_project(
            project,
            origin_workspace,
            NewSessionLocation::CurrentCheckout,
            nonce,
            managed_root,
            prompt_title,
        );
        self.fixed_workspace = Some(workspace.clone());
        self.location = NewSessionLocation::CurrentCheckout;
        self.location_touched = true;
        ticket
    }

    pub fn ticket(&self) -> Option<NewSessionProjectTicket> {
        Some(NewSessionProjectTicket {
            generation: self.generation,
            project_id: self.project_id.clone()?,
            origin_workspace_id: self.origin_workspace_id.clone()?,
            project_root: self.project_root.clone(),
        })
    }

    pub fn apply_eligibility(
        &mut self,
        ticket: &NewSessionProjectTicket,
        eligibility: GitProjectEligibility,
    ) -> bool {
        if ticket.generation != self.generation
            || self.project_id.as_ref() != Some(&ticket.project_id)
            || self.origin_workspace_id.as_ref() != Some(&ticket.origin_workspace_id)
            || eligibility.project_id != ticket.project_id
            || !eligibility_path_matches(&ticket.project_root, &eligibility)
        {
            return false;
        }
        if eligibility.is_eligible() {
            if !self.base_ref_touched {
                self.base_ref = eligibility.default_base_ref.clone();
            }
            if !self.location_touched && self.fixed_workspace.is_none() {
                self.location = self.preference;
            }
        } else {
            self.location = NewSessionLocation::CurrentCheckout;
            self.base_ref = None;
        }
        self.eligibility = Some(eligibility);
        true
    }

    pub fn set_location(&mut self, location: NewSessionLocation) -> bool {
        if self.fixed_workspace.is_some()
            || (location == NewSessionLocation::NewWorktree && !self.worktree_available())
        {
            return false;
        }
        self.location = location;
        self.location_touched = true;
        true
    }

    pub fn set_base_ref(&mut self, base_ref: impl Into<String>) -> bool {
        let base_ref = base_ref.into();
        if !self
            .eligibility
            .as_ref()
            .is_some_and(|eligibility| eligibility.selectable_base_refs.contains(&base_ref))
        {
            return false;
        }
        self.base_ref = Some(base_ref);
        self.base_ref_touched = true;
        true
    }

    pub fn set_worktree_name(&mut self, value: impl Into<String>) {
        if self.submission.created_workspace.is_some() {
            return;
        }
        self.worktree_name = value.into();
        self.name_touched = true;
        if !self.path_touched {
            self.worktree_path = self.auto_path_preview();
        }
    }

    pub fn set_worktree_path(&mut self, value: impl Into<String>) {
        if self.submission.created_workspace.is_some() {
            return;
        }
        self.worktree_path = value.into();
        self.path_touched = true;
    }

    pub fn restore_auto_values(&mut self, prompt_title: Option<&str>) {
        if self.submission.created_workspace.is_some() {
            return;
        }
        self.name_touched = false;
        self.path_touched = false;
        self.refresh_auto_values(prompt_title);
    }

    pub fn refresh_auto_values(&mut self, prompt_title: Option<&str>) {
        if !self.name_touched {
            self.worktree_name = suggested_worktree_name(prompt_title, &self.suggestion_nonce);
        }
        if !self.path_touched {
            self.worktree_path = self.auto_path_preview();
        }
    }

    pub fn worktree_available(&self) -> bool {
        self.eligibility
            .as_ref()
            .is_some_and(GitProjectEligibility::is_eligible)
    }

    pub fn is_probing(&self) -> bool {
        self.project_id.is_some() && self.eligibility.is_none()
    }

    pub fn selected_workspace(&self) -> Option<&WorkspaceRecord> {
        self.submission
            .created_workspace
            .as_ref()
            .or(self.fixed_workspace.as_ref())
    }

    pub fn branch_name(&self) -> String {
        let slug = managed_worktree_name_slug(self.worktree_name.trim())
            .chars()
            .take(96)
            .collect::<String>();
        format!("vibex/{}", if slug.is_empty() { "worktree" } else { &slug })
    }

    pub fn worktree_create_request(&self) -> Result<GitWorktreeCreateRequest, &'static str> {
        if self.location != NewSessionLocation::NewWorktree || !self.worktree_available() {
            return Err("worktree_project_ineligible");
        }
        let workspace_id = self
            .origin_workspace_id
            .clone()
            .ok_or("worktree_origin_workspace_missing")?;
        let base_ref = self
            .base_ref
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or("worktree_base_ref_missing")?;
        let worktree_name = self.worktree_name.trim();
        if worktree_name.is_empty() {
            return Err("worktree_name_empty");
        }
        if worktree_name.len() > 128 || worktree_name.chars().any(char::is_control) {
            return Err("worktree_name_invalid");
        }
        let worktree_path = self
            .path_touched
            .then(|| self.worktree_path.trim().to_string());
        if worktree_path.as_deref().is_some_and(str::is_empty) {
            return Err("worktree_path_empty");
        }
        if worktree_path.as_deref().is_some_and(|path| {
            path.len() > 4_096
                || path.chars().any(char::is_control)
                || !custom_worktree_path_is_absolute(path)
        }) {
            return Err("worktree_path_invalid");
        }
        Ok(GitWorktreeCreateRequest {
            workspace_id: workspace_id.clone(),
            branch_name: self.branch_name(),
            base_ref: Some(base_ref),
            name: Some(worktree_name.to_string()),
            worktree_path,
            target_workspace_id: Some(workspace_id),
            target_branch: self
                .eligibility
                .as_ref()
                .and_then(|eligibility| eligibility.current_branch.clone()),
        })
    }

    pub fn expected_revision(&self) -> Option<&str> {
        self.eligibility
            .as_ref()
            .map(|eligibility| eligibility.revision.as_str())
    }

    pub fn begin_submission(&mut self) {
        self.submission.error_code = None;
        self.submission.stage = if self.fixed_workspace.is_some()
            || self.submission.created_workspace.is_some()
            || self.location == NewSessionLocation::CurrentCheckout
        {
            NewSessionSubmissionStage::CreatingSession
        } else {
            NewSessionSubmissionStage::CreatingWorktree
        };
    }

    pub fn mark_workspace_ready(&mut self, workspace: WorkspaceRecord) {
        self.submission.created_workspace = Some(workspace);
        self.submission.stage = NewSessionSubmissionStage::CreatingSession;
        self.submission.error_code = None;
    }

    pub fn mark_stage(&mut self, stage: NewSessionSubmissionStage) {
        self.submission.stage = stage;
        self.submission.error_code = None;
    }

    pub fn mark_failed(&mut self, code: impl Into<String>) {
        self.submission.stage = NewSessionSubmissionStage::Failed;
        self.submission.error_code = Some(code.into());
    }

    pub fn reset_after_success(&mut self) {
        self.submission = NewSessionSubmissionState::default();
        self.fixed_workspace = None;
    }

    fn auto_path_preview(&self) -> String {
        let Some(project_id) = self.project_id.as_ref() else {
            return String::new();
        };
        self.managed_root
            .join("worktrees")
            .join(project_id.as_str())
            .join(format!(
                "{}-*",
                managed_worktree_name_slug(self.worktree_name.trim())
            ))
            .to_string_lossy()
            .into_owned()
    }
}

fn eligibility_path_matches(project_root: &str, eligibility: &GitProjectEligibility) -> bool {
    eligibility.project_canonical_path.original_path == project_root
        || eligibility.project_canonical_path.normalized_path == project_root
        || eligibility.project_canonical_path.canonical_path.as_deref() == Some(project_root)
}

fn bounded_nonce(value: &str) -> String {
    let suffix = value
        .chars()
        .rev()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    if suffix.is_empty() {
        "session".to_string()
    } else {
        suffix
    }
}

pub fn suggested_worktree_name(title: Option<&str>, nonce: &str) -> String {
    let base = managed_worktree_name_slug(title.unwrap_or("session").trim());
    let base = base.chars().take(48).collect::<String>();
    format!("{base}-{}", bounded_nonce(nonce))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceContextProjection {
    pub project_id: ProjectId,
    pub project_name: String,
    pub workspace_id: WorkspaceId,
    pub workspace_mode: WorkspaceMode,
    pub workspace_root: String,
    pub branch: Option<String>,
    pub managed_worktree_id: Option<String>,
    pub git_dirty: bool,
}

impl WorkspaceContextProjection {
    pub fn from_authoritative(
        project: &ProjectRecord,
        workspace: &WorkspaceRecord,
        eligibility: Option<&GitProjectEligibility>,
        lifecycle: Option<&GitWorktreeLifecycleSnapshot>,
        git_branch: Option<&str>,
        git_dirty: bool,
    ) -> Self {
        let managed = lifecycle.and_then(|snapshot| {
            (snapshot.workspace_id == workspace.id)
                .then_some(())
                .and_then(|_| {
                    snapshot
                        .managed_worktrees
                        .iter()
                        .find(|managed| managed.workspace_id.as_ref() == Some(&workspace.id))
                })
        });
        Self {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            workspace_id: workspace.id.clone(),
            workspace_mode: workspace.mode,
            workspace_root: workspace.root_path.clone(),
            branch: managed
                .and_then(|managed| managed.branch.clone())
                .or_else(|| eligibility.and_then(|value| value.current_branch.clone()))
                .or_else(|| git_branch.map(str::to_string)),
            managed_worktree_id: managed.map(|managed| managed.worktree_id.to_string()),
            git_dirty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkspaceAgentSummary {
    pub total: usize,
    pub running: usize,
    pub needs_input: usize,
    pub failed: usize,
}

impl WorkspaceAgentSummary {
    fn from_sessions(sessions: &[AgentSession]) -> Self {
        let mut summary = Self {
            total: sessions.len(),
            ..Self::default()
        };
        for session in sessions {
            match session.state {
                AgentSessionState::Running | AgentSessionState::Initializing => {
                    summary.running += 1;
                }
                AgentSessionState::NeedsInput => summary.needs_input += 1,
                AgentSessionState::Error => summary.failed += 1,
                AgentSessionState::Idle
                | AgentSessionState::Closed
                | AgentSessionState::Archived => {}
            }
        }
        summary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarWorkspaceProjection {
    pub row_id: String,
    pub workspace: WorkspaceRecord,
    pub context: WorkspaceContextProjection,
    pub sessions: Vec<AgentSession>,
    pub agent_summary: WorkspaceAgentSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarProjectProjection {
    pub row_id: String,
    pub project: ProjectRecord,
    pub workspaces: Vec<SidebarWorkspaceProjection>,
    pub compact_sessions: Vec<AgentSession>,
}

pub fn sidebar_project_projections(
    workspaces: &[(ProjectRecord, WorkspaceRecord)],
    sessions: &[AgentSession],
    contexts: &BTreeMap<String, WorkspaceContextProjection>,
    project_order: &[String],
    session_order: &[String],
    pinned_session_ids: &BTreeSet<String>,
    query: &str,
) -> Vec<SidebarProjectProjection> {
    let query = query.trim().to_lowercase();
    let project_positions = project_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let session_positions = session_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut grouped = BTreeMap::<String, (ProjectRecord, Vec<WorkspaceRecord>)>::new();
    for (project, workspace) in workspaces {
        let entry = grouped
            .entry(project.id.as_str().to_string())
            .or_insert_with(|| (project.clone(), Vec::new()));
        entry.1.push(workspace.clone());
    }
    let mut grouped = grouped.into_values().collect::<Vec<_>>();
    grouped.sort_by_key(|(project, _)| {
        (
            project_positions
                .get(project.id.as_str())
                .copied()
                .is_none(),
            project_positions
                .get(project.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            project.created_at_ms,
            project.id.as_str().to_string(),
        )
    });

    grouped
        .into_iter()
        .filter_map(|(project, mut project_workspaces)| {
            project_workspaces.sort_by_key(|workspace| {
                (
                    workspace.mode != WorkspaceMode::CurrentCheckout,
                    workspace.created_at_ms,
                    workspace.id.as_str().to_string(),
                )
            });
            let project_matches = query.is_empty()
                || [project.name.as_str(), project.root_path.as_str()]
                    .iter()
                    .any(|value| value.to_lowercase().contains(&query));
            let mut workspace_rows = Vec::new();
            for workspace in project_workspaces {
                let mut workspace_sessions = sessions
                    .iter()
                    .filter(|session| {
                        session.deleted_at_ms.is_none() && session.workspace_id == workspace.id
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                sort_sessions(
                    &mut workspace_sessions,
                    &session_positions,
                    pinned_session_ids,
                );
                let agent_summary = WorkspaceAgentSummary::from_sessions(&workspace_sessions);
                let context = contexts
                    .get(workspace.id.as_str())
                    .cloned()
                    .unwrap_or_else(|| WorkspaceContextProjection {
                        project_id: project.id.clone(),
                        project_name: project.name.clone(),
                        workspace_id: workspace.id.clone(),
                        workspace_mode: workspace.mode,
                        workspace_root: workspace.root_path.clone(),
                        branch: None,
                        managed_worktree_id: None,
                        git_dirty: false,
                    });
                let workspace_matches = project_matches
                    || query.is_empty()
                    || [
                        workspace.root_path.as_str(),
                        context.branch.as_deref().unwrap_or(""),
                    ]
                    .iter()
                    .any(|value| value.to_lowercase().contains(&query));
                let visible_sessions = if workspace_matches {
                    workspace_sessions.clone()
                } else {
                    workspace_sessions
                        .iter()
                        .filter(|session| session_matches_query(session, &query))
                        .cloned()
                        .collect()
                };
                if !query.is_empty() && !workspace_matches && visible_sessions.is_empty() {
                    continue;
                }
                workspace_rows.push(SidebarWorkspaceProjection {
                    row_id: format!("workspace:{}", workspace.id.as_str()),
                    workspace,
                    context,
                    agent_summary,
                    sessions: visible_sessions,
                });
            }
            if workspace_rows.is_empty() {
                return None;
            }
            let mut compact_sessions = workspace_rows
                .iter()
                .flat_map(|workspace| workspace.sessions.iter().cloned())
                .collect::<Vec<_>>();
            sort_sessions(
                &mut compact_sessions,
                &session_positions,
                pinned_session_ids,
            );
            Some(SidebarProjectProjection {
                row_id: format!("project:{}", project.id.as_str()),
                project,
                workspaces: workspace_rows,
                compact_sessions,
            })
        })
        .collect()
}

fn sort_sessions(
    sessions: &mut [AgentSession],
    positions: &BTreeMap<&str, usize>,
    pinned: &BTreeSet<String>,
) {
    sessions.sort_by_key(|session| {
        (
            !pinned.contains(session.id.as_str()),
            positions
                .get(session.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            std::cmp::Reverse(session.updated_at_ms),
            session.id.as_str().to_string(),
        )
    });
}

fn session_matches_query(session: &AgentSession, query: &str) -> bool {
    [
        session.title.as_str(),
        session.agent_id.as_str(),
        session.workspace_root.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
        || format!("{:?}", session.state)
            .to_lowercase()
            .contains(query)
}

pub fn custom_worktree_path_is_absolute(path: &str) -> bool {
    Path::new(path).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, AgentSessionSafety, GitManagedWorktreeRecord, GitManagedWorktreeStatus,
        GitPathIdentity, GitProjectEligibilityState, GitWorktreeReconciliationState, RequestId,
    };

    fn project() -> ProjectRecord {
        ProjectRecord {
            id: ProjectId::new(),
            name: "Vibex".into(),
            root_path: "/repo".into(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn workspace(project: &ProjectRecord, mode: WorkspaceMode, root: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            id: WorkspaceId::new(),
            project_id: project.id.clone(),
            root_path: root.into(),
            mode,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn eligibility(project: &ProjectRecord, root: &str, revision: &str) -> GitProjectEligibility {
        GitProjectEligibility {
            project_id: project.id.clone(),
            project_canonical_path: GitPathIdentity {
                original_path: root.into(),
                normalized_path: root.into(),
                canonical_path: Some(root.into()),
                filesystem_id: None,
                comparison_key: format!("path:{root}"),
                exists: true,
            },
            state: GitProjectEligibilityState::Eligible,
            repository_identity: Some(vibex_core::GitRepositoryIdentity {
                repository_root: GitPathIdentity {
                    original_path: root.into(),
                    normalized_path: root.into(),
                    canonical_path: Some(root.into()),
                    filesystem_id: None,
                    comparison_key: format!("path:{root}"),
                    exists: true,
                },
                git_common_dir: GitPathIdentity {
                    original_path: format!("{root}/.git"),
                    normalized_path: format!("{root}/.git"),
                    canonical_path: Some(format!("{root}/.git")),
                    filesystem_id: None,
                    comparison_key: format!("path:{root}/.git"),
                    exists: true,
                },
                comparison_key: format!("git:{root}"),
            }),
            current_branch: Some("main".into()),
            default_base_ref: Some("main".into()),
            selectable_base_refs: vec!["main".into(), "origin/main".into()],
            observed_head: Some("a".repeat(40)),
            revision: revision.into(),
            disabled_reason: None,
        }
    }

    fn session(workspace: &WorkspaceRecord, title: &str, state: AgentSessionState) -> AgentSession {
        AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: title.into(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    #[test]
    fn preference_only_preselects_after_current_project_probe() {
        let project = project();
        let checkout = workspace(&project, WorkspaceMode::CurrentCheckout, "/repo");
        let mut state = NewSessionWorkspaceState::default();
        let ticket = state.select_project(
            &project,
            &checkout,
            NewSessionLocation::NewWorktree,
            "nonce-12345678",
            "/home/vibex",
            Some("Add workspace UI"),
        );
        assert_eq!(state.location, NewSessionLocation::CurrentCheckout);
        assert!(state.is_probing());
        assert!(state.apply_eligibility(&ticket, eligibility(&project, "/repo", "r1")));
        assert_eq!(state.location, NewSessionLocation::NewWorktree);
        assert!(state.set_location(NewSessionLocation::CurrentCheckout));
        assert_eq!(state.preference, NewSessionLocation::NewWorktree);
    }

    #[test]
    fn stale_probe_and_auto_refresh_do_not_overwrite_current_manual_fields() {
        let project = project();
        let checkout = workspace(&project, WorkspaceMode::CurrentCheckout, "/repo");
        let mut state = NewSessionWorkspaceState::default();
        let stale = state.select_project(
            &project,
            &checkout,
            NewSessionLocation::CurrentCheckout,
            "nonce-a",
            "/home/vibex",
            Some("First"),
        );
        state.set_worktree_name("manual-name");
        state.set_worktree_path("/tmp/manual-path");
        let current = state.select_project(
            &project,
            &checkout,
            NewSessionLocation::CurrentCheckout,
            "nonce-b",
            "/home/vibex",
            Some("Second"),
        );
        state.set_worktree_name("kept-name");
        state.set_worktree_path("/tmp/kept-path");
        assert!(!state.apply_eligibility(&stale, eligibility(&project, "/repo", "old")));
        assert!(state.apply_eligibility(&current, eligibility(&project, "/repo", "new")));
        state.refresh_auto_values(Some("Changed prompt"));
        assert_eq!(state.worktree_name, "kept-name");
        assert_eq!(state.worktree_path, "/tmp/kept-path");
    }

    #[test]
    fn completed_worktree_is_reused_after_session_stage_failure() {
        let project = project();
        let checkout = workspace(&project, WorkspaceMode::CurrentCheckout, "/repo");
        let created = workspace(&project, WorkspaceMode::VibexWorktree, "/wt");
        let mut state = NewSessionWorkspaceState::default();
        let ticket = state.select_project(
            &project,
            &checkout,
            NewSessionLocation::NewWorktree,
            "nonce",
            "/home/vibex",
            None,
        );
        state.apply_eligibility(&ticket, eligibility(&project, "/repo", "r1"));
        let key = state.submission.idempotency_key.clone();
        state.begin_submission();
        state.mark_workspace_ready(created.clone());
        state.mark_failed("session_create_failed");
        state.begin_submission();
        assert_eq!(
            state.submission.stage,
            NewSessionSubmissionStage::CreatingSession
        );
        assert_eq!(state.selected_workspace(), Some(&created));
        assert_eq!(state.submission.idempotency_key, key);
    }

    #[test]
    fn custom_worktree_settings_are_validated_before_backend_submission() {
        let project = project();
        let checkout = workspace(&project, WorkspaceMode::CurrentCheckout, "/repo");
        let mut state = NewSessionWorkspaceState::default();
        let ticket = state.select_project(
            &project,
            &checkout,
            NewSessionLocation::NewWorktree,
            "nonce",
            "/home/vibex",
            None,
        );
        assert!(state.apply_eligibility(&ticket, eligibility(&project, "/repo", "r1")));
        state.set_worktree_path("relative/path");
        assert_eq!(
            state.worktree_create_request().unwrap_err(),
            "worktree_path_invalid"
        );
        state.set_worktree_path("/tmp/worktree");
        state.set_worktree_name("bad\nname");
        assert_eq!(
            state.worktree_create_request().unwrap_err(),
            "worktree_name_invalid"
        );
    }

    #[test]
    fn sidebar_projection_renders_one_project_for_multiple_workspaces() {
        let project = project();
        let checkout = workspace(&project, WorkspaceMode::CurrentCheckout, "/repo");
        let worktree = workspace(&project, WorkspaceMode::VibexWorktree, "/wt");
        let sessions = vec![
            session(&checkout, "Main", AgentSessionState::Idle),
            session(&worktree, "Feature", AgentSessionState::Running),
        ];
        let projects = sidebar_project_projections(
            &[
                (project.clone(), checkout.clone()),
                (project.clone(), worktree.clone()),
            ],
            &sessions,
            &BTreeMap::new(),
            &[],
            &[],
            &BTreeSet::new(),
            "",
        );
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].workspaces.len(), 2);
        assert_eq!(projects[0].compact_sessions.len(), 2);
        assert_eq!(projects[0].workspaces[1].agent_summary.running, 1);
    }

    #[test]
    fn sidebar_search_keeps_the_authoritative_workspace_agent_summary() {
        let project = project();
        let worktree = workspace(&project, WorkspaceMode::VibexWorktree, "/wt");
        let sessions = vec![
            session(&worktree, "Visible", AgentSessionState::Idle),
            session(&worktree, "Hidden", AgentSessionState::Running),
        ];
        let projects = sidebar_project_projections(
            &[(project, worktree)],
            &sessions,
            &BTreeMap::new(),
            &[],
            &[],
            &BTreeSet::new(),
            "Visible",
        );

        assert_eq!(projects[0].workspaces[0].sessions.len(), 1);
        assert_eq!(projects[0].workspaces[0].agent_summary.total, 2);
        assert_eq!(projects[0].workspaces[0].agent_summary.running, 1);
    }

    #[test]
    fn context_uses_managed_branch_identity_for_worktree() {
        let project = project();
        let worktree = workspace(&project, WorkspaceMode::VibexWorktree, "/wt");
        let managed = GitManagedWorktreeRecord {
            worktree_id: RequestId::new(),
            project_id: project.id.clone(),
            workspace_id: Some(worktree.id.clone()),
            repo_root: "/repo".into(),
            worktree_path: "/wt".into(),
            repository_identity: None,
            worktree_path_identity: None,
            branch: Some("vibex/feature".into()),
            origin_workspace_id: None,
            base_ref: Some("main".into()),
            base_head: Some("a".repeat(40)),
            target_workspace_id: None,
            target_branch: Some("main".into()),
            head: Some("a".repeat(40)),
            status: GitManagedWorktreeStatus::Active,
            reconciliation_state: GitWorktreeReconciliationState::Consistent,
            diagnostic: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            closed_at_ms: None,
        };
        let snapshot = GitWorktreeLifecycleSnapshot {
            workspace_id: worktree.id.clone(),
            eligibility: eligibility(&project, "/wt", "r1"),
            managed_worktrees: vec![managed.clone()],
            operations: Vec::new(),
            diagnostics: Vec::new(),
            revision: "snapshot".into(),
        };
        let context = WorkspaceContextProjection::from_authoritative(
            &project,
            &worktree,
            Some(&snapshot.eligibility),
            Some(&snapshot),
            None,
            true,
        );
        assert_eq!(context.branch.as_deref(), Some("vibex/feature"));
        assert_eq!(
            context.managed_worktree_id.as_deref(),
            Some(managed.worktree_id.as_str())
        );
        assert!(context.git_dirty);
    }
}
