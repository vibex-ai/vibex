use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use vibex_core::{
    GitManagedWorktreeRecord, GitManagedWorktreeStatus, GitPathIdentity, GitProjectEligibility,
    GitProjectEligibilityState, GitWorktreeCreateRequest, GitWorktreeCreateResult,
    GitWorktreeDestructiveAction, GitWorktreeDestructivePreflight, GitWorktreeDiagnostic,
    GitWorktreeDiagnosticSeverity, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeListResponse, GitWorktreeLockKey, GitWorktreeLockKind, GitWorktreeMergeRequest,
    GitWorktreeOperationCheckpoint, GitWorktreeOperationDetail, GitWorktreeOperationKind,
    GitWorktreeOperationRecord, GitWorktreeOperationStatus, GitWorktreeReconcileReport,
    GitWorktreeReconciliationState, GitWorktreeRisk, GitWorktreeRiskKind, GitWorktreeSummary,
    ProjectId, RequestId, VibexError, VibexResult, WorkspaceId, WorkspaceMode, WorkspaceRecord,
    unix_timestamp_ms,
};
use vibex_db::{
    ManagedWorktreeRepository, WorkspaceRepository, WorktreeOperationClaimOutcome,
    WorktreeOperationRepository, open_database,
};

use crate::GitHandle;
use crate::workbench::workspace_record;

const WORKTREE_OPERATION_LEASE_MS: i64 = 30_000;
const WORKTREE_OPERATION_DETAIL_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone)]
pub struct WorktreeCreateContext {
    pub request_id: RequestId,
    pub idempotency_key: Option<String>,
}

impl WorktreeCreateContext {
    pub fn new(request_id: RequestId, idempotency_key: Option<String>) -> Self {
        Self {
            request_id,
            idempotency_key,
        }
    }
}

#[derive(Clone)]
pub struct WorktreeCoordinator {
    db_path: PathBuf,
    lease_owner: String,
    lock_registry: WorktreeLockRegistry,
    fault_injector: WorktreeFaultInjector,
}

impl WorktreeCoordinator {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            lease_owner: format!("desktop-worktree:{}", RequestId::new().as_str()),
            lock_registry: WorktreeLockRegistry::default(),
            fault_injector: WorktreeFaultInjector::default(),
        }
    }

    pub fn project_git_eligibility(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitProjectEligibility> {
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, workspace_id)?;
        Ok(vibex_git::project_git_eligibility(
            project.id,
            workspace.root_path,
        ))
    }

    pub fn lifecycle_snapshot(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitWorktreeLifecycleSnapshot> {
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, workspace_id)?;
        let eligibility =
            vibex_git::project_git_eligibility(project.id.clone(), &workspace.root_path);
        let managed_worktrees =
            ManagedWorktreeRepository::list_for_project(&connection, &project.id)?;
        let operations = WorktreeOperationRepository::list_for_project(&connection, &project.id)?;
        let diagnostics = managed_worktrees
            .iter()
            .filter_map(|record| record.diagnostic.clone())
            .chain(
                operations
                    .iter()
                    .filter_map(|record| record.detail.diagnostic.clone()),
            )
            .collect::<Vec<_>>();
        let revision = snapshot_revision(&eligibility, &managed_worktrees, &operations)?;
        Ok(GitWorktreeLifecycleSnapshot {
            workspace_id: workspace.id,
            eligibility,
            managed_worktrees,
            operations,
            diagnostics,
            revision,
        })
    }

    pub fn list(&self, workspace_id: &WorkspaceId) -> VibexResult<GitWorktreeListResponse> {
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, workspace_id)?;
        let mut response = vibex_git::worktree_list(workspace.id, &workspace.root_path)?;
        let managed = ManagedWorktreeRepository::list_for_project(&connection, &project.id)?;
        for worktree in &mut response.worktrees {
            if let Some(record) = managed
                .iter()
                .find(|record| managed_matches_summary(record, worktree))
            {
                worktree.managed = true;
                worktree.workspace_id = record.workspace_id.clone();
            }
        }
        Ok(response)
    }

    pub fn create(
        &self,
        request: &GitWorktreeCreateRequest,
        context: WorktreeCreateContext,
    ) -> VibexResult<GitWorktreeCreateResult> {
        let mut connection = open_database(&self.db_path)?;
        let (project, origin_workspace) = workspace_record(&connection, &request.workspace_id)?;
        let request_fingerprint = create_request_fingerprint(&project.id, request)?;
        let idempotency_key = context
            .idempotency_key
            .unwrap_or_else(|| format!("worktree-create:{}", context.request_id.as_str()));

        let operation = if let Some(existing) =
            WorktreeOperationRepository::get_by_idempotency_key(&connection, &idempotency_key)?
        {
            verify_reserved_create(&existing, &project.id, &request_fingerprint)?;
            existing
        } else {
            let operation = self.build_create_intent(
                &connection,
                project,
                origin_workspace,
                request,
                context.request_id,
                idempotency_key,
                request_fingerprint,
            )?;
            WorktreeOperationRepository::reserve(&mut connection, &operation)?
        };
        drop(connection);

        let result = (|| {
            self.fault_injector
                .trip(GitWorktreeOperationCheckpoint::IntentRecorded)?;
            self.execute_create(operation.clone())
        })();
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                if !is_busy_error(&error) && !is_simulated_process_loss(&error) {
                    let _ = self.record_create_failure(&operation.operation_id, &error);
                }
                Err(error)
            }
        }
    }

    pub fn merge_preflight(
        &self,
        request: &GitWorktreeMergeRequest,
    ) -> VibexResult<GitWorktreeDestructivePreflight> {
        Ok(self.merge_preflight_facts(request, None)?.preflight)
    }

    pub fn discard_preflight(
        &self,
        request: &GitWorktreeDiscardRequest,
    ) -> VibexResult<GitWorktreeDestructivePreflight> {
        Ok(self.discard_preflight_facts(request, None)?.preflight)
    }

    pub fn merge(
        &self,
        request: &GitWorktreeMergeRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let initial = self.merge_preflight_facts(request, None)?;
        validate_merge_confirmation(request, &initial.preflight)?;
        if !initial.preflight.allowed {
            return Err(blocked_preflight_error(&initial.preflight));
        }
        let mut connection = open_database(&self.db_path)?;
        let operation = lifecycle_operation(
            &initial.managed,
            GitWorktreeOperationKind::MergeBack,
            initial.target_workspace.as_ref(),
            &initial.preflight,
            lifecycle_lock_keys(
                &initial.managed,
                initial
                    .target_workspace
                    .as_ref()
                    .map(|workspace| &workspace.id),
            )?,
            request_fingerprint("merge", request)?,
        );
        let operation = WorktreeOperationRepository::reserve(&mut connection, &operation)?;
        if operation.status == GitWorktreeOperationStatus::Completed {
            return Ok(operation);
        }
        ensure_lifecycle_operation_contract(&connection, &operation)?;
        let _locks = self.claim_lifecycle_locks(&connection, &operation)?;
        let claimed = match claim_lifecycle_operation(&connection, &operation, &self.lease_owner)? {
            LifecycleClaim::Acquired(record) => record,
            LifecycleClaim::Completed(record) => return Ok(record),
        };
        WorktreeOperationRepository::update_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::LocksAcquired,
            None,
        )?;
        let current = self.merge_preflight_facts(request, Some(&claimed.operation_id))?;
        validate_merge_confirmation(request, &current.preflight)?;
        if !current.preflight.allowed {
            let error = blocked_preflight_error(&current.preflight);
            mark_lifecycle_failure(&connection, &claimed, &error, false)?;
            return Err(error);
        }
        let target_workspace = current.target_workspace.as_ref().ok_or_else(|| {
            VibexError::validation(
                "worktree_target_workspace_missing",
                "managed worktree has no fixed target workspace",
            )
        })?;
        let source_ref = current.managed.branch.as_deref().ok_or_else(|| {
            VibexError::validation(
                "worktree_source_branch_missing",
                "managed worktree has no source branch",
            )
        })?;
        let target_branch = current.managed.target_branch.as_deref().ok_or_else(|| {
            VibexError::validation(
                "worktree_fixed_target_branch_missing",
                "managed worktree has no fixed target branch",
            )
        })?;
        let expected_source_head = current.preflight.source_head.as_deref().ok_or_else(|| {
            VibexError::conflict(
                "worktree_source_head_missing",
                "worktree source head is unavailable after preflight",
            )
        })?;
        let expected_target_head = current.preflight.target_head.as_deref().ok_or_else(|| {
            VibexError::conflict(
                "worktree_target_head_missing",
                "worktree target head is unavailable after preflight",
            )
        })?;
        let result = vibex_git::worktree_merge(
            &target_workspace.root_path,
            source_ref,
            expected_source_head,
            target_branch,
            expected_target_head,
        );
        match result {
            Ok(_) => {
                let head_after = vibex_git::resolve_head(&target_workspace.root_path)?;
                ManagedWorktreeRepository::update_status(
                    &connection,
                    &current.managed.worktree_path,
                    GitManagedWorktreeStatus::Merged,
                    Some(&head_after),
                    Some(unix_timestamp_ms()),
                )?;
                WorktreeOperationRepository::mark_outcome(
                    &connection,
                    &claimed.operation_id,
                    GitWorktreeOperationStatus::Completed,
                    GitWorktreeOperationCheckpoint::Completed,
                    Some(&head_after),
                    None,
                )
            }
            Err(error) => {
                let needs_resolution =
                    vibex_git::status(target_workspace.id.clone(), &target_workspace.root_path)
                        .map(|status| {
                            status
                                .changes
                                .iter()
                                .any(|change| change.kind == vibex_core::GitChangeKind::Unmerged)
                        })
                        .unwrap_or(false);
                mark_lifecycle_failure(&connection, &claimed, &error, needs_resolution)?;
                Err(error)
            }
        }
    }

    pub fn discard(
        &self,
        request: &GitWorktreeDiscardRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let initial = self.discard_preflight_facts(request, None)?;
        validate_discard_confirmation(request, &initial.preflight)?;
        if !initial.preflight.allowed {
            return Err(blocked_preflight_error(&initial.preflight));
        }
        let mut connection = open_database(&self.db_path)?;
        let operation = lifecycle_operation(
            &initial.managed,
            GitWorktreeOperationKind::Discard,
            None,
            &initial.preflight,
            lifecycle_lock_keys(&initial.managed, None)?,
            request_fingerprint("discard", request)?,
        );
        let operation = WorktreeOperationRepository::reserve(&mut connection, &operation)?;
        if operation.status == GitWorktreeOperationStatus::Completed {
            return Ok(operation);
        }
        ensure_lifecycle_operation_contract(&connection, &operation)?;
        let _locks = self.claim_lifecycle_locks(&connection, &operation)?;
        let claimed = match claim_lifecycle_operation(&connection, &operation, &self.lease_owner)? {
            LifecycleClaim::Acquired(record) => record,
            LifecycleClaim::Completed(record) => return Ok(record),
        };
        WorktreeOperationRepository::update_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::LocksAcquired,
            None,
        )?;
        let current = self.discard_preflight_facts(request, Some(&claimed.operation_id))?;
        validate_discard_confirmation(request, &current.preflight)?;
        if !current.preflight.allowed {
            let error = blocked_preflight_error(&current.preflight);
            mark_lifecycle_failure(&connection, &claimed, &error, false)?;
            return Err(error);
        }
        let exact_request = GitWorktreeDiscardRequest {
            workspace_id: request.workspace_id.clone(),
            worktree_path: current.managed.worktree_path.clone(),
            force: request.force,
            expected_head: request.expected_head.clone(),
            preflight_revision: request.preflight_revision.clone(),
        };
        match vibex_git::worktree_remove(&current.managed.repo_root, &exact_request) {
            Ok(_) => {
                ManagedWorktreeRepository::update_status(
                    &connection,
                    &current.managed.worktree_path,
                    GitManagedWorktreeStatus::Discarded,
                    current.preflight.source_head.as_deref(),
                    Some(unix_timestamp_ms()),
                )?;
                WorktreeOperationRepository::mark_outcome(
                    &connection,
                    &claimed.operation_id,
                    GitWorktreeOperationStatus::Completed,
                    GitWorktreeOperationCheckpoint::Completed,
                    current.preflight.source_head.as_deref(),
                    None,
                )
            }
            Err(error) => {
                mark_lifecycle_failure(&connection, &claimed, &error, false)?;
                Err(error)
            }
        }
    }

    fn merge_preflight_facts(
        &self,
        request: &GitWorktreeMergeRequest,
        ignored_operation: Option<&RequestId>,
    ) -> VibexResult<DestructiveFacts> {
        let connection = open_database(&self.db_path)?;
        let managed = managed_for_path(&connection, &request.source_path)?.ok_or_else(|| {
            VibexError::validation("worktree_not_managed", "worktree is not managed by Vibex")
        })?;
        let (_, source_workspace) = workspace_record(&connection, &request.workspace_id)?;
        verify_managed_source(&managed, &source_workspace)?;
        let target_workspace_id = managed.target_workspace_id.as_ref().ok_or_else(|| {
            VibexError::conflict(
                "worktree_fixed_target_missing",
                "managed worktree has no provable fixed target workspace",
            )
        })?;
        if request
            .target_workspace_id
            .as_ref()
            .is_some_and(|requested| requested != target_workspace_id)
        {
            return Err(VibexError::conflict(
                "worktree_target_override_rejected",
                "merge target does not match the managed worktree fixed target",
            ));
        }
        let (_, target_workspace) = workspace_record(&connection, target_workspace_id)?;
        if target_workspace.project_id != managed.project_id {
            return Err(VibexError::conflict(
                "worktree_target_project_mismatch",
                "managed worktree target belongs to a different project",
            ));
        }
        let target_branch = managed.target_branch.as_deref().ok_or_else(|| {
            VibexError::conflict(
                "worktree_fixed_target_branch_missing",
                "managed worktree has no provable fixed target branch",
            )
        })?;
        let source_branch = managed.branch.as_deref().ok_or_else(|| {
            VibexError::conflict(
                "worktree_source_branch_missing",
                "managed worktree has no source branch",
            )
        })?;
        let source_head = vibex_git::resolve_ref_head(&managed.repo_root, source_branch).ok();
        let target_head = vibex_git::resolve_head(&target_workspace.root_path).ok();
        let source_status =
            vibex_git::status(source_workspace.id.clone(), &source_workspace.root_path)?;
        let target_status =
            vibex_git::status(target_workspace.id.clone(), &target_workspace.root_path)?;
        let registration_matches = registered_managed_worktree(&managed);
        let mut risks = Vec::new();
        if managed.status != GitManagedWorktreeStatus::Active {
            risks.push(risk(
                GitWorktreeRiskKind::UnknownState,
                true,
                "managed worktree is not active",
            ));
        }
        if !registration_matches {
            risks.push(risk(
                GitWorktreeRiskKind::MissingGitRegistration,
                true,
                "managed worktree has no matching Git registration",
            ));
        }
        if source_status.dirty {
            risks.push(risk(
                GitWorktreeRiskKind::DirtySource,
                true,
                "source worktree has uncommitted changes",
            ));
        }
        if target_status.dirty {
            risks.push(risk(
                GitWorktreeRiskKind::DirtyTarget,
                true,
                "target workspace has uncommitted changes",
            ));
        }
        if target_status.branch.as_deref() != Some(target_branch) {
            risks.push(risk(
                GitWorktreeRiskKind::OwnershipMismatch,
                true,
                "target workspace is not on the fixed target branch",
            ));
        }
        if request
            .expected_source_head
            .as_deref()
            .is_some_and(|expected| source_head.as_deref() != Some(expected))
        {
            risks.push(risk(
                GitWorktreeRiskKind::SourceHeadChanged,
                true,
                "source head changed after preflight",
            ));
        }
        if request
            .expected_target_head
            .as_deref()
            .is_some_and(|expected| target_head.as_deref() != Some(expected))
        {
            risks.push(risk(
                GitWorktreeRiskKind::TargetHeadChanged,
                true,
                "target head changed after preflight",
            ));
        }
        append_active_operation_risk(&connection, &managed, ignored_operation, &mut risks)?;
        let revision = destructive_revision(
            GitWorktreeDestructiveAction::MergeBack,
            &managed,
            source_head.as_deref(),
            target_head.as_deref(),
            &risks,
        )?;
        Ok(DestructiveFacts {
            managed,
            target_workspace: Some(target_workspace),
            preflight: GitWorktreeDestructivePreflight {
                action: GitWorktreeDestructiveAction::MergeBack,
                allowed: !risks.iter().any(|risk| risk.blocking),
                revision,
                source_head,
                target_head,
                risks,
                observed_at_ms: unix_timestamp_ms(),
            },
        })
    }

    fn discard_preflight_facts(
        &self,
        request: &GitWorktreeDiscardRequest,
        ignored_operation: Option<&RequestId>,
    ) -> VibexResult<DestructiveFacts> {
        let connection = open_database(&self.db_path)?;
        let managed = managed_for_path(&connection, &request.worktree_path)?.ok_or_else(|| {
            VibexError::validation("worktree_not_managed", "worktree is not managed by Vibex")
        })?;
        let (_, source_workspace) = workspace_record(&connection, &request.workspace_id)?;
        verify_managed_source(&managed, &source_workspace)?;
        let source_head = managed
            .branch
            .as_deref()
            .and_then(|branch| vibex_git::resolve_ref_head(&managed.repo_root, branch).ok());
        let source_status =
            vibex_git::status(source_workspace.id.clone(), &source_workspace.root_path)?;
        let mut risks = Vec::new();
        if managed.status != GitManagedWorktreeStatus::Active {
            risks.push(risk(
                GitWorktreeRiskKind::UnknownState,
                true,
                "managed worktree is not active",
            ));
        }
        if !registered_managed_worktree(&managed) {
            risks.push(risk(
                GitWorktreeRiskKind::MissingGitRegistration,
                true,
                "managed worktree has no matching Git registration",
            ));
        }
        if source_status.dirty {
            risks.push(risk(
                GitWorktreeRiskKind::DirtySource,
                !request.force,
                "source worktree has uncommitted changes",
            ));
        }
        if request
            .expected_head
            .as_deref()
            .is_some_and(|expected| source_head.as_deref() != Some(expected))
        {
            risks.push(risk(
                GitWorktreeRiskKind::SourceHeadChanged,
                true,
                "source head changed after preflight",
            ));
        }
        append_active_operation_risk(&connection, &managed, ignored_operation, &mut risks)?;
        let revision = destructive_revision(
            GitWorktreeDestructiveAction::Discard,
            &managed,
            source_head.as_deref(),
            None,
            &risks,
        )?;
        Ok(DestructiveFacts {
            managed,
            target_workspace: None,
            preflight: GitWorktreeDestructivePreflight {
                action: GitWorktreeDestructiveAction::Discard,
                allowed: !risks.iter().any(|risk| risk.blocking),
                revision,
                source_head,
                target_head: None,
                risks,
                observed_at_ms: unix_timestamp_ms(),
            },
        })
    }

    fn build_create_intent(
        &self,
        connection: &vibex_db::DbConnection,
        project: vibex_core::ProjectRecord,
        origin_workspace: WorkspaceRecord,
        request: &GitWorktreeCreateRequest,
        operation_id: RequestId,
        idempotency_key: String,
        request_fingerprint: String,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let eligibility =
            vibex_git::project_git_eligibility(project.id.clone(), &origin_workspace.root_path);
        if !eligibility.is_eligible() {
            return Err(ineligible_create_error(&eligibility));
        }
        vibex_git::validate_worktree_create(&origin_workspace.root_path, request)?;

        let repository_identity = eligibility.repository_identity.clone().ok_or_else(|| {
            VibexError::validation(
                "worktree_repository_identity_missing",
                "Git repository identity is unavailable",
            )
        })?;
        let base_ref = request
            .base_ref
            .clone()
            .or_else(|| eligibility.default_base_ref.clone())
            .ok_or_else(|| {
                VibexError::validation(
                    "worktree_base_ref_missing",
                    "worktree base ref is unavailable",
                )
            })?;
        let base_head = vibex_git::resolve_ref_head(&origin_workspace.root_path, &base_ref)?;
        let target_workspace = self.resolve_create_target(
            connection,
            &project.id,
            &origin_workspace,
            request.target_workspace_id.as_ref(),
        )?;
        let target_eligibility =
            vibex_git::project_git_eligibility(project.id.clone(), &target_workspace.root_path);
        let target_repository =
            target_eligibility
                .repository_identity
                .as_ref()
                .ok_or_else(|| {
                    VibexError::validation(
                        "worktree_target_repository_missing",
                        "worktree target is not a valid Git working tree",
                    )
                })?;
        if target_repository.comparison_key != repository_identity.comparison_key {
            return Err(VibexError::validation(
                "worktree_target_repository_mismatch",
                "worktree target belongs to a different Git repository",
            ));
        }
        let target_branch = request
            .target_branch
            .clone()
            .or(target_eligibility.current_branch)
            .ok_or_else(|| {
                VibexError::validation(
                    "worktree_target_branch_missing",
                    "worktree target must have a named branch",
                )
            })?;
        let target_head = vibex_git::resolve_ref_head(&target_workspace.root_path, &target_branch)?;
        let worktree_path = self.resolve_create_worktree_path(
            connection,
            &project.id,
            &repository_identity,
            request,
            &operation_id,
        )?;
        let worktree_path_identity = vibex_git::canonical_path_identity(&worktree_path);
        let lock_keys = create_lock_keys(&repository_identity, &worktree_path_identity);
        let now = unix_timestamp_ms();
        Ok(GitWorktreeOperationRecord {
            operation_id,
            project_id: project.id,
            source_workspace_id: Some(origin_workspace.id.clone()),
            target_workspace_id: Some(target_workspace.id),
            operation: GitWorktreeOperationKind::Create,
            status: GitWorktreeOperationStatus::Pending,
            worktree_path: Some(worktree_path.to_string_lossy().to_string()),
            branch: Some(request.branch_name.clone()),
            base_ref: Some(base_ref),
            head_before: Some(base_head.clone()),
            head_after: None,
            error: None,
            detail: GitWorktreeOperationDetail {
                idempotency_key: Some(idempotency_key),
                request_fingerprint: Some(request_fingerprint),
                repository_identity: Some(repository_identity),
                source_path_identity: Some(vibex_git::canonical_path_identity(
                    &origin_workspace.root_path,
                )),
                target_path_identity: Some(worktree_path_identity),
                lock_keys,
                origin_workspace_id: Some(origin_workspace.id),
                base_head: Some(base_head),
                target_branch: Some(target_branch),
                expected_source_head: None,
                expected_target_head: Some(target_head),
                preflight_revision: Some(eligibility.revision),
                checkpoint: GitWorktreeOperationCheckpoint::IntentRecorded,
                ..GitWorktreeOperationDetail::default()
            },
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    fn resolve_create_target(
        &self,
        connection: &vibex_db::DbConnection,
        project_id: &ProjectId,
        origin_workspace: &WorkspaceRecord,
        requested_target: Option<&WorkspaceId>,
    ) -> VibexResult<WorkspaceRecord> {
        if let Some(target_workspace_id) = requested_target {
            let (_, workspace) = workspace_record(connection, target_workspace_id)?;
            if &workspace.project_id != project_id {
                return Err(VibexError::validation(
                    "worktree_target_project_mismatch",
                    "worktree target belongs to a different project",
                ));
            }
            return Ok(workspace);
        }
        WorkspaceRepository::list(connection)?
            .into_iter()
            .find(|(_, workspace)| {
                &workspace.project_id == project_id
                    && workspace.mode == WorkspaceMode::CurrentCheckout
            })
            .map(|(_, workspace)| workspace)
            .or_else(|| {
                (origin_workspace.mode == WorkspaceMode::CurrentCheckout)
                    .then(|| origin_workspace.clone())
            })
            .ok_or_else(|| {
                VibexError::validation(
                    "worktree_target_workspace_missing",
                    "project current checkout workspace was not found",
                )
            })
    }

    fn claim_lifecycle_locks(
        &self,
        connection: &vibex_db::DbConnection,
        operation: &GitWorktreeOperationRecord,
    ) -> VibexResult<WorktreeLockClaim> {
        match self.lock_registry.claim(operation.detail.lock_keys.clone()) {
            Ok(claim) => Ok(claim),
            Err(error) if error.code == "worktree_lifecycle_busy" => {
                mark_lifecycle_failure(connection, operation, &error, false)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn execute_create(
        &self,
        operation: GitWorktreeOperationRecord,
    ) -> VibexResult<GitWorktreeCreateResult> {
        if operation.status == GitWorktreeOperationStatus::Completed {
            return self.reconstruct_create_result(&operation.operation_id);
        }
        if matches!(
            operation.status,
            GitWorktreeOperationStatus::NeedsAttention
                | GitWorktreeOperationStatus::NeedsResolution
                | GitWorktreeOperationStatus::Aborted
                | GitWorktreeOperationStatus::Unknown
        ) {
            return Err(VibexError::conflict(
                "worktree_operation_needs_attention",
                "worktree operation requires manual attention",
            ));
        }
        if operation.operation != GitWorktreeOperationKind::Create {
            return Err(VibexError::validation(
                "worktree_operation_kind_mismatch",
                "worktree operation is not a create operation",
            ));
        }
        if !operation_detail_is_executable(&operation.detail) {
            return Err(VibexError::conflict(
                "worktree_operation_contract_unknown",
                "worktree operation uses an unknown durable contract",
            ));
        }
        let _locks = self
            .lock_registry
            .claim(operation.detail.lock_keys.clone())?;
        let connection = open_database(&self.db_path)?;
        let claim = WorktreeOperationRepository::try_claim(
            &connection,
            &operation.operation_id,
            &self.lease_owner,
            unix_timestamp_ms(),
            WORKTREE_OPERATION_LEASE_MS,
        )?;
        let claimed = match claim {
            WorktreeOperationClaimOutcome::Acquired(record) => record,
            WorktreeOperationClaimOutcome::Completed(record) => {
                return self.reconstruct_create_result(&record.operation_id);
            }
            WorktreeOperationClaimOutcome::Busy(_) => {
                return Err(VibexError::conflict(
                    "worktree_operation_busy",
                    "worktree operation is already running",
                ));
            }
            WorktreeOperationClaimOutcome::NeedsAttention(_) => {
                return Err(VibexError::conflict(
                    "worktree_operation_needs_attention",
                    "worktree operation requires manual attention",
                ));
            }
        };
        let recovery_attempt = claimed.detail.attempt > 1
            || operation.detail.checkpoint >= GitWorktreeOperationCheckpoint::GitAddStarted;
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::LocksAcquired,
            None,
        )?;

        let source_workspace_id = claimed
            .source_workspace_id
            .as_ref()
            .ok_or_else(|| incomplete_create_intent("origin workspace"))?;
        let (project, origin_workspace) = workspace_record(&connection, source_workspace_id)?;
        if project.id != claimed.project_id {
            return Err(VibexError::conflict(
                "worktree_project_identity_mismatch",
                "worktree operation project identity changed",
            ));
        }
        let worktree_path = PathBuf::from(
            claimed
                .worktree_path
                .as_deref()
                .ok_or_else(|| incomplete_create_intent("worktree path"))?,
        );
        let branch = claimed
            .branch
            .as_ref()
            .ok_or_else(|| incomplete_create_intent("branch"))?
            .clone();
        let base_head = claimed
            .detail
            .base_head
            .as_ref()
            .or(claimed.head_before.as_ref())
            .ok_or_else(|| incomplete_create_intent("base head"))?
            .clone();
        let target_workspace_id = claimed
            .target_workspace_id
            .as_ref()
            .ok_or_else(|| incomplete_create_intent("target workspace"))?
            .clone();
        let target_branch = claimed
            .detail
            .target_branch
            .as_ref()
            .ok_or_else(|| incomplete_create_intent("target branch"))?
            .clone();

        if let Some(parent) = worktree_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                VibexError::storage(
                    "worktree_parent_create_failed",
                    "failed to create managed worktree parent",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
            })?;
        }
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::GitAddStarted,
            None,
        )?;
        let exact_request = GitWorktreeCreateRequest {
            workspace_id: origin_workspace.id.clone(),
            branch_name: branch.clone(),
            base_ref: Some(base_head.clone()),
            name: None,
            worktree_path: None,
            target_workspace_id: Some(target_workspace_id.clone()),
            target_branch: Some(target_branch.clone()),
        };
        let mut worktree = vibex_git::worktree_add_recoverable(
            &origin_workspace.root_path,
            &worktree_path,
            &exact_request,
            Some(&base_head),
            recovery_attempt,
        )?;
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::GitAdded,
            worktree.head.as_deref(),
        )?;

        let worktree_workspace = WorkspaceRepository::ensure_for_project(
            &connection,
            &project.id,
            &worktree.path,
            WorkspaceMode::VibexWorktree,
        )?;
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::WorkspacePersisted,
            worktree.head.as_deref(),
        )?;

        worktree.managed = true;
        worktree.workspace_id = Some(worktree_workspace.id.clone());
        let now = unix_timestamp_ms();
        let managed = GitManagedWorktreeRecord {
            worktree_id: claimed.operation_id.clone(),
            project_id: project.id,
            workspace_id: Some(worktree_workspace.id.clone()),
            repo_root: origin_workspace.root_path,
            worktree_path: worktree.path.clone(),
            repository_identity: claimed.detail.repository_identity.clone(),
            worktree_path_identity: Some(vibex_git::canonical_path_identity(&worktree.path)),
            branch: worktree.branch.clone().or(Some(branch)),
            origin_workspace_id: Some(origin_workspace.id),
            base_ref: claimed.base_ref.clone(),
            base_head: Some(base_head),
            target_workspace_id: Some(target_workspace_id),
            target_branch: Some(target_branch),
            head: worktree.head.clone(),
            status: GitManagedWorktreeStatus::Active,
            reconciliation_state: GitWorktreeReconciliationState::Consistent,
            diagnostic: None,
            created_at_ms: claimed.created_at_ms,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        ManagedWorktreeRepository::upsert(&connection, &managed)?;
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::ManagedRecordPersisted,
            worktree.head.as_deref(),
        )?;
        self.persist_checkpoint(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationCheckpoint::DatabaseCommitted,
            worktree.head.as_deref(),
        )?;
        let operation = WorktreeOperationRepository::mark_outcome(
            &connection,
            &claimed.operation_id,
            GitWorktreeOperationStatus::Completed,
            GitWorktreeOperationCheckpoint::Completed,
            worktree.head.as_deref(),
            None,
        )?;
        Ok(GitWorktreeCreateResult {
            worktree,
            workspace: worktree_workspace,
            managed,
            operation,
        })
    }

    fn persist_checkpoint(
        &self,
        connection: &vibex_db::DbConnection,
        operation_id: &RequestId,
        checkpoint: GitWorktreeOperationCheckpoint,
        head_after: Option<&str>,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let record = WorktreeOperationRepository::update_checkpoint(
            connection,
            operation_id,
            checkpoint,
            head_after,
        )?;
        self.fault_injector.trip(checkpoint)?;
        Ok(record)
    }

    fn reconstruct_create_result(
        &self,
        operation_id: &RequestId,
    ) -> VibexResult<GitWorktreeCreateResult> {
        let connection = open_database(&self.db_path)?;
        let operation = WorktreeOperationRepository::get(&connection, operation_id)?
            .ok_or_else(|| incomplete_create_intent("operation"))?;
        let managed =
            ManagedWorktreeRepository::get_by_id(&connection, operation_id)?.ok_or_else(|| {
                VibexError::storage(
                    "worktree_managed_record_missing",
                    "completed worktree operation has no managed record",
                )
            })?;
        let workspace_id = managed.workspace_id.as_ref().ok_or_else(|| {
            VibexError::storage(
                "worktree_workspace_identity_missing",
                "managed worktree has no workspace identity",
            )
        })?;
        let (_, workspace) = workspace_record(&connection, workspace_id)?;
        let worktrees = vibex_git::worktree_list(workspace.id.clone(), &managed.repo_root)?;
        let mut worktree = worktrees
            .worktrees
            .into_iter()
            .find(|summary| managed_matches_summary(&managed, summary))
            .ok_or_else(|| {
                VibexError::conflict(
                    "worktree_registration_missing",
                    "managed worktree is not registered with Git",
                )
            })?;
        worktree.managed = true;
        worktree.workspace_id = Some(workspace.id.clone());
        Ok(GitWorktreeCreateResult {
            worktree,
            workspace,
            managed,
            operation,
        })
    }

    fn record_create_failure(
        &self,
        operation_id: &RequestId,
        error: &VibexError,
    ) -> VibexResult<()> {
        let connection = open_database(&self.db_path)?;
        let Some(operation) = WorktreeOperationRepository::get(&connection, operation_id)? else {
            return Ok(());
        };
        if operation.status == GitWorktreeOperationStatus::Completed {
            return Ok(());
        }
        let unknown_contract = !operation_detail_is_executable(&operation.detail);
        let classification = classify_create_state(&operation);
        let (status, checkpoint, code, summary, retryable, recovery_action) = match classification {
            _ if unknown_contract => (
                GitWorktreeOperationStatus::NeedsAttention,
                GitWorktreeOperationCheckpoint::NeedsAttention,
                "worktree_operation_contract_unknown",
                "worktree creation uses an unknown durable contract",
                false,
                Some("upgrade_or_inspect".to_string()),
            ),
            CreateStateClassification::NoSideEffects => (
                GitWorktreeOperationStatus::Failed,
                operation.detail.checkpoint,
                "worktree_create_failed",
                "worktree creation failed before a durable Git worktree was observed",
                true,
                Some("retry".to_string()),
            ),
            CreateStateClassification::ExactBranchOnly
            | CreateStateClassification::ExactRegistration => (
                GitWorktreeOperationStatus::Recoverable,
                operation.detail.checkpoint,
                "worktree_create_recoverable",
                "worktree creation has verified side effects and can be resumed",
                true,
                Some("retry".to_string()),
            ),
            CreateStateClassification::UnregisteredDirectory
            | CreateStateClassification::IdentityMismatch
            | CreateStateClassification::Uninspectable => (
                GitWorktreeOperationStatus::NeedsAttention,
                GitWorktreeOperationCheckpoint::NeedsAttention,
                "worktree_create_needs_attention",
                "worktree creation left state that cannot be changed automatically",
                false,
                Some("inspect_worktree".to_string()),
            ),
        };
        let diagnostic = GitWorktreeDiagnostic {
            code: code.to_string(),
            summary: format!("{summary}: {}", bounded_error_code(error)),
            severity: GitWorktreeDiagnosticSeverity::Error,
            retryable,
            recovery_action,
            operation_id: Some(operation.operation_id.clone()),
            worktree_id: Some(operation.operation_id.clone()),
            observed_at_ms: unix_timestamp_ms(),
        };
        WorktreeOperationRepository::mark_outcome(
            &connection,
            operation_id,
            status,
            checkpoint,
            None,
            Some(&diagnostic),
        )?;
        if let Some(managed) = ManagedWorktreeRepository::get_by_id(&connection, operation_id)? {
            let state = if status == GitWorktreeOperationStatus::NeedsAttention {
                GitWorktreeReconciliationState::NeedsAttention
            } else {
                GitWorktreeReconciliationState::Recoverable
            };
            ManagedWorktreeRepository::update_reconciliation(
                &connection,
                &managed.worktree_id,
                state,
                Some(&diagnostic),
            )?;
        }
        Ok(())
    }

    pub fn reconcile_on_startup(&self) -> VibexResult<GitWorktreeReconcileReport> {
        let connection = open_database(&self.db_path)?;
        let operations = WorktreeOperationRepository::list_reconcilable(&connection)?;
        drop(connection);
        let mut report = GitWorktreeReconcileReport::default();
        for operation in operations {
            report.inspected_operations = report.inspected_operations.saturating_add(1);
            if operation.operation != GitWorktreeOperationKind::Create {
                continue;
            }
            let classification = classify_create_state(&operation);
            if operation.status == GitWorktreeOperationStatus::Running {
                let diagnostic = reconciliation_diagnostic(
                    &operation,
                    "worktree_create_interrupted",
                    "interrupted worktree creation will be resumed from durable facts",
                    true,
                );
                let connection = open_database(&self.db_path)?;
                WorktreeOperationRepository::mark_outcome(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Recoverable,
                    operation.detail.checkpoint,
                    None,
                    Some(&diagnostic),
                )?;
            }
            if matches!(
                classification,
                CreateStateClassification::UnregisteredDirectory
                    | CreateStateClassification::IdentityMismatch
                    | CreateStateClassification::Uninspectable
            ) {
                let error = VibexError::conflict(
                    "worktree_reconciliation_uncertain",
                    "worktree state could not be reconciled automatically",
                );
                self.record_create_failure(&operation.operation_id, &error)?;
                report.needs_attention = report.needs_attention.saturating_add(1);
                continue;
            }
            match self.execute_create(operation.clone()) {
                Ok(_) => {
                    report.completed_operations = report.completed_operations.saturating_add(1);
                }
                Err(error) if is_busy_error(&error) => {
                    report.recoverable_operations = report.recoverable_operations.saturating_add(1);
                }
                Err(error) => {
                    self.record_create_failure(&operation.operation_id, &error)?;
                    let connection = open_database(&self.db_path)?;
                    let current =
                        WorktreeOperationRepository::get(&connection, &operation.operation_id)?;
                    match current.map(|record| record.status) {
                        Some(GitWorktreeOperationStatus::NeedsAttention) => {
                            report.needs_attention = report.needs_attention.saturating_add(1);
                        }
                        Some(GitWorktreeOperationStatus::Recoverable) => {
                            report.recoverable_operations =
                                report.recoverable_operations.saturating_add(1);
                        }
                        _ => {
                            report.failed_operations = report.failed_operations.saturating_add(1);
                        }
                    }
                }
            }
        }
        self.reconcile_managed_records(&mut report)?;
        self.report_orphan_directories(&mut report)?;
        Ok(report)
    }

    fn reconcile_managed_records(
        &self,
        report: &mut GitWorktreeReconcileReport,
    ) -> VibexResult<()> {
        let connection = open_database(&self.db_path)?;
        let records = ManagedWorktreeRepository::list_all(&connection)?;
        for mut record in records {
            report.inspected_worktrees = report.inspected_worktrees.saturating_add(1);
            if matches!(
                record.status,
                GitManagedWorktreeStatus::Archived | GitManagedWorktreeStatus::Discarded
            ) {
                if record.reconciliation_state != GitWorktreeReconciliationState::Consistent
                    || record.diagnostic.is_some()
                {
                    ManagedWorktreeRepository::update_reconciliation(
                        &connection,
                        &record.worktree_id,
                        GitWorktreeReconciliationState::Consistent,
                        None,
                    )?;
                }
                continue;
            }
            let registration = vibex_git::worktree_list(WorkspaceId::new(), &record.repo_root)
                .ok()
                .and_then(|list| {
                    list.worktrees
                        .into_iter()
                        .find(|summary| managed_matches_summary(&record, summary))
                });
            let path_exists = Path::new(&record.worktree_path).is_dir();
            let fixed_identity_complete = record.origin_workspace_id.is_some()
                && record.base_head.is_some()
                && record.target_workspace_id.is_some()
                && record.target_branch.is_some();
            let (state, diagnostic) = match (registration, path_exists, fixed_identity_complete) {
                (Some(summary), true, true)
                    if record.branch.as_deref() == summary.branch.as_deref() =>
                {
                    let changed = record.repository_identity != summary.repository_identity
                        || record.worktree_path_identity != summary.path_identity
                        || record.head != summary.head
                        || record.reconciliation_state
                            != GitWorktreeReconciliationState::Consistent
                        || record.diagnostic.is_some();
                    if changed {
                        record.repository_identity = summary.repository_identity;
                        record.worktree_path_identity = summary.path_identity;
                        record.head = summary.head;
                        record.reconciliation_state = GitWorktreeReconciliationState::Consistent;
                        record.diagnostic = None;
                        record.updated_at_ms = unix_timestamp_ms();
                        ManagedWorktreeRepository::upsert(&connection, &record)?;
                    }
                    (GitWorktreeReconciliationState::Consistent, None)
                }
                (Some(_), false, _) => (
                    GitWorktreeReconciliationState::NeedsAttention,
                    Some(managed_diagnostic(
                        &record,
                        "managed_worktree_directory_missing",
                        "Git registration exists but the managed worktree directory is missing",
                    )),
                ),
                (Some(_), true, false) => (
                    GitWorktreeReconciliationState::NeedsAttention,
                    Some(managed_diagnostic(
                        &record,
                        "managed_worktree_fixed_target_missing",
                        "legacy managed worktree has no provable fixed origin/base/target identity",
                    )),
                ),
                (Some(_), true, true) => (
                    GitWorktreeReconciliationState::NeedsAttention,
                    Some(managed_diagnostic(
                        &record,
                        "managed_worktree_registration_mismatch",
                        "managed worktree registration does not match its durable identity",
                    )),
                ),
                (None, true, _) => (
                    GitWorktreeReconciliationState::NeedsAttention,
                    Some(managed_diagnostic(
                        &record,
                        "managed_worktree_orphan_directory",
                        "managed worktree directory exists without a matching Git registration",
                    )),
                ),
                (None, false, _) => (
                    GitWorktreeReconciliationState::NeedsAttention,
                    Some(managed_diagnostic(
                        &record,
                        "managed_worktree_missing",
                        "managed worktree directory and Git registration are missing",
                    )),
                ),
            };
            let diagnostic = diagnostic.map(|diagnostic| {
                record
                    .diagnostic
                    .as_ref()
                    .filter(|existing| existing.code == diagnostic.code)
                    .cloned()
                    .unwrap_or(diagnostic)
            });
            if record.reconciliation_state != state || record.diagnostic != diagnostic {
                ManagedWorktreeRepository::update_reconciliation(
                    &connection,
                    &record.worktree_id,
                    state,
                    diagnostic.as_ref(),
                )?;
            }
            if let Some(diagnostic) = diagnostic {
                report.needs_attention = report.needs_attention.saturating_add(1);
                report.diagnostics.push(diagnostic);
            }
        }
        Ok(())
    }

    fn report_orphan_directories(
        &self,
        report: &mut GitWorktreeReconcileReport,
    ) -> VibexResult<()> {
        let Some(home) = self.db_path.parent() else {
            return Ok(());
        };
        let root = home.join("worktrees");
        if !root.is_dir() {
            return Ok(());
        }
        let connection = open_database(&self.db_path)?;
        let managed = ManagedWorktreeRepository::list_all(&connection)?;
        for project_entry in read_directories(&root)? {
            for worktree_entry in read_directories(&project_entry)? {
                let identity = vibex_git::canonical_path_identity(&worktree_entry);
                if managed.iter().any(|record| {
                    record
                        .worktree_path_identity
                        .as_ref()
                        .is_some_and(|stored| stored.comparison_key == identity.comparison_key)
                        || vibex_git::same_path_identity(&record.worktree_path, &worktree_entry)
                }) {
                    continue;
                }
                let diagnostic = GitWorktreeDiagnostic {
                    code: "vibex_worktree_orphan_directory".to_string(),
                    summary: "Vibex worktree storage contains an unowned directory".to_string(),
                    severity: GitWorktreeDiagnosticSeverity::Warning,
                    retryable: false,
                    recovery_action: Some("inspect_worktree".to_string()),
                    operation_id: None,
                    worktree_id: None,
                    observed_at_ms: unix_timestamp_ms(),
                };
                report.needs_attention = report.needs_attention.saturating_add(1);
                report.diagnostics.push(diagnostic);
            }
        }
        Ok(())
    }

    fn managed_worktree_path(
        &self,
        project_id: &ProjectId,
        name: &str,
        operation_id: &RequestId,
    ) -> VibexResult<PathBuf> {
        let root = self.db_path.parent().ok_or_else(|| {
            VibexError::storage(
                "desktop_runtime_home_parent_missing",
                "desktop runtime database has no home directory",
            )
        })?;
        let short_id = operation_id
            .as_str()
            .rsplit('_')
            .next()
            .unwrap_or(operation_id.as_str())
            .chars()
            .take(8)
            .collect::<String>();
        Ok(root
            .join("worktrees")
            .join(project_id.as_str())
            .join(format!(
                "{}-{short_id}",
                vibex_core::managed_worktree_name_slug(name)
            )))
    }

    fn resolve_create_worktree_path(
        &self,
        connection: &vibex_db::DbConnection,
        project_id: &ProjectId,
        repository_identity: &vibex_core::GitRepositoryIdentity,
        request: &GitWorktreeCreateRequest,
        operation_id: &RequestId,
    ) -> VibexResult<PathBuf> {
        let Some(requested_path) = request.worktree_path.as_deref() else {
            return self.managed_worktree_path(
                project_id,
                request.name.as_deref().unwrap_or(&request.branch_name),
                operation_id,
            );
        };
        let path = PathBuf::from(requested_path);
        let identity = vibex_git::canonical_path_identity(&path);
        if path.exists() {
            return Err(VibexError::conflict(
                "worktree_path_exists",
                "custom worktree path already exists",
            ));
        }
        if path_identity_is_within(&identity, &repository_identity.repository_root) {
            return Err(VibexError::validation(
                "worktree_path_inside_repository",
                "custom worktree path must be outside the repository checkout",
            ));
        }
        if ManagedWorktreeRepository::get_by_identity_key(connection, &identity.comparison_key)?
            .is_some()
        {
            return Err(VibexError::conflict(
                "worktree_path_owned",
                "custom worktree path is already owned by a managed worktree",
            ));
        }
        for (_, workspace) in WorkspaceRepository::list(connection)? {
            let workspace_identity = vibex_git::canonical_path_identity(&workspace.root_path);
            if path_identity_is_within(&identity, &workspace_identity) {
                return Err(VibexError::validation(
                    "worktree_path_inside_workspace",
                    "custom worktree path must be outside an existing project workspace",
                ));
            }
        }
        Ok(path)
    }

    #[cfg(test)]
    fn fail_once_after(&self, checkpoint: GitWorktreeOperationCheckpoint) {
        self.fault_injector.fail_once_after(checkpoint);
    }

    #[cfg(test)]
    fn simulate_process_loss_once_after(&self, checkpoint: GitWorktreeOperationCheckpoint) {
        self.fault_injector
            .simulate_process_loss_once_after(checkpoint);
    }
}

impl GitHandle {
    pub fn project_git_eligibility(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitProjectEligibility> {
        self.worktrees.project_git_eligibility(workspace_id)
    }

    pub fn worktree_snapshot(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitWorktreeLifecycleSnapshot> {
        self.worktrees.lifecycle_snapshot(workspace_id)
    }

    pub fn worktree_list(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitWorktreeListResponse> {
        self.worktrees.list(workspace_id)
    }

    pub fn worktree_create(
        &self,
        request: &GitWorktreeCreateRequest,
    ) -> VibexResult<GitWorktreeCreateResult> {
        self.worktree_create_with_context(
            request,
            WorktreeCreateContext::new(RequestId::new(), None),
        )
    }

    pub fn worktree_create_with_context(
        &self,
        request: &GitWorktreeCreateRequest,
        context: WorktreeCreateContext,
    ) -> VibexResult<GitWorktreeCreateResult> {
        self.worktrees.create(request, context)
    }

    pub fn worktree_merge_preflight(
        &self,
        request: &GitWorktreeMergeRequest,
    ) -> VibexResult<GitWorktreeDestructivePreflight> {
        self.worktrees.merge_preflight(request)
    }

    pub fn worktree_merge(
        &self,
        request: &GitWorktreeMergeRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        self.worktrees.merge(request)
    }

    pub fn worktree_discard_preflight(
        &self,
        request: &GitWorktreeDiscardRequest,
    ) -> VibexResult<GitWorktreeDestructivePreflight> {
        self.worktrees.discard_preflight(request)
    }

    pub fn worktree_discard(
        &self,
        request: &GitWorktreeDiscardRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        self.worktrees.discard(request)
    }

    pub fn reconcile_worktrees_on_startup(&self) -> VibexResult<GitWorktreeReconcileReport> {
        self.worktrees.reconcile_on_startup()
    }
}

struct DestructiveFacts {
    managed: GitManagedWorktreeRecord,
    target_workspace: Option<WorkspaceRecord>,
    preflight: GitWorktreeDestructivePreflight,
}

enum LifecycleClaim {
    Acquired(GitWorktreeOperationRecord),
    Completed(GitWorktreeOperationRecord),
}

fn managed_for_path(
    connection: &vibex_db::DbConnection,
    path: &str,
) -> VibexResult<Option<GitManagedWorktreeRecord>> {
    let identity = vibex_git::canonical_path_identity(path);
    if let Some(record) =
        ManagedWorktreeRepository::get_by_identity_key(connection, &identity.comparison_key)?
    {
        return Ok(Some(record));
    }
    if let Some(record) = ManagedWorktreeRepository::get_by_path(connection, path)? {
        return Ok(Some(record));
    }
    Ok(ManagedWorktreeRepository::list_all(connection)?
        .into_iter()
        .find(|record| vibex_git::same_path_identity(&record.worktree_path, path)))
}

fn verify_managed_source(
    managed: &GitManagedWorktreeRecord,
    workspace: &WorkspaceRecord,
) -> VibexResult<()> {
    if managed.project_id != workspace.project_id
        || managed.workspace_id.as_ref() != Some(&workspace.id)
        || !vibex_git::same_path_identity(&managed.worktree_path, &workspace.root_path)
    {
        return Err(VibexError::conflict(
            "worktree_source_ownership_mismatch",
            "workspace does not own the requested managed worktree",
        ));
    }
    Ok(())
}

fn registered_managed_worktree(managed: &GitManagedWorktreeRecord) -> bool {
    vibex_git::worktree_list(WorkspaceId::new(), &managed.repo_root)
        .map(|list| {
            list.worktrees.into_iter().any(|summary| {
                managed_matches_summary(managed, &summary)
                    && managed.branch.as_deref() == summary.branch.as_deref()
            })
        })
        .unwrap_or(false)
}

fn append_active_operation_risk(
    connection: &vibex_db::DbConnection,
    managed: &GitManagedWorktreeRecord,
    ignored_operation: Option<&RequestId>,
    risks: &mut Vec<GitWorktreeRisk>,
) -> VibexResult<()> {
    let active = WorktreeOperationRepository::list_for_project(connection, &managed.project_id)?
        .into_iter()
        .filter(|operation| ignored_operation != Some(&operation.operation_id))
        .filter(|operation| {
            matches!(
                operation.status,
                GitWorktreeOperationStatus::Pending
                    | GitWorktreeOperationStatus::Running
                    | GitWorktreeOperationStatus::Recoverable
            )
        })
        .any(|operation| {
            operation
                .worktree_path
                .as_deref()
                .is_some_and(|path| vibex_git::same_path_identity(path, &managed.worktree_path))
        });
    if active {
        risks.push(risk(
            GitWorktreeRiskKind::ActiveOperation,
            true,
            "another worktree lifecycle operation is active",
        ));
    }
    Ok(())
}

fn risk(kind: GitWorktreeRiskKind, blocking: bool, summary: &str) -> GitWorktreeRisk {
    GitWorktreeRisk {
        kind,
        blocking,
        summary: summary.to_string(),
    }
}

fn destructive_revision(
    action: GitWorktreeDestructiveAction,
    managed: &GitManagedWorktreeRecord,
    source_head: Option<&str>,
    target_head: Option<&str>,
    risks: &[GitWorktreeRisk],
) -> VibexResult<String> {
    let payload = serde_json::to_vec(&(
        action,
        &managed.worktree_id,
        &managed.project_id,
        &managed.workspace_id,
        &managed.target_workspace_id,
        &managed.target_branch,
        source_head,
        target_head,
        risks,
    ))
    .map_err(|_| {
        VibexError::process(
            "worktree_preflight_revision_failed",
            "worktree preflight could not be fingerprinted",
        )
    })?;
    Ok(format!(
        "worktree-preflight-v1:{:x}",
        Sha256::digest(payload)
    ))
}

fn lifecycle_lock_keys(
    managed: &GitManagedWorktreeRecord,
    target_workspace_id: Option<&WorkspaceId>,
) -> VibexResult<Vec<GitWorktreeLockKey>> {
    let repository = managed
        .repository_identity
        .clone()
        .or_else(|| vibex_git::repository_identity(&managed.repo_root).ok())
        .ok_or_else(|| {
            VibexError::conflict(
                "worktree_repository_identity_missing",
                "managed worktree repository identity is unavailable",
            )
        })?;
    let path = managed
        .worktree_path_identity
        .clone()
        .unwrap_or_else(|| vibex_git::canonical_path_identity(&managed.worktree_path));
    let mut keys = create_lock_keys(&repository, &path);
    if let Some(target_workspace_id) = target_workspace_id {
        keys.push(GitWorktreeLockKey {
            kind: GitWorktreeLockKind::WorkspaceIndex,
            key: target_workspace_id.as_str().to_string(),
        });
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

fn lifecycle_operation(
    managed: &GitManagedWorktreeRecord,
    kind: GitWorktreeOperationKind,
    target_workspace: Option<&WorkspaceRecord>,
    preflight: &GitWorktreeDestructivePreflight,
    lock_keys: Vec<GitWorktreeLockKey>,
    request_fingerprint: String,
) -> GitWorktreeOperationRecord {
    let operation_id = RequestId::new();
    let kind_label = match kind {
        GitWorktreeOperationKind::MergeBack => "merge",
        GitWorktreeOperationKind::Discard => "discard",
        _ => "lifecycle",
    };
    let idempotency_key = format!(
        "worktree-{kind_label}:{}:{}",
        managed.worktree_id.as_str(),
        preflight.revision
    );
    let now = unix_timestamp_ms();
    GitWorktreeOperationRecord {
        operation_id,
        project_id: managed.project_id.clone(),
        source_workspace_id: managed.workspace_id.clone(),
        target_workspace_id: target_workspace.map(|workspace| workspace.id.clone()),
        operation: kind,
        status: GitWorktreeOperationStatus::Pending,
        worktree_path: Some(managed.worktree_path.clone()),
        branch: managed.branch.clone(),
        base_ref: managed.base_ref.clone(),
        head_before: preflight.source_head.clone(),
        head_after: None,
        error: None,
        detail: GitWorktreeOperationDetail {
            idempotency_key: Some(idempotency_key),
            request_fingerprint: Some(request_fingerprint),
            repository_identity: managed.repository_identity.clone(),
            source_path_identity: managed.worktree_path_identity.clone(),
            target_path_identity: target_workspace
                .map(|workspace| vibex_git::canonical_path_identity(&workspace.root_path)),
            lock_keys,
            origin_workspace_id: managed.origin_workspace_id.clone(),
            base_head: managed.base_head.clone(),
            target_branch: managed.target_branch.clone(),
            expected_source_head: preflight.source_head.clone(),
            expected_target_head: preflight.target_head.clone(),
            preflight_revision: Some(preflight.revision.clone()),
            checkpoint: GitWorktreeOperationCheckpoint::IntentRecorded,
            ..GitWorktreeOperationDetail::default()
        },
        created_at_ms: now,
        updated_at_ms: now,
    }
}

fn claim_lifecycle_operation(
    connection: &vibex_db::DbConnection,
    operation: &GitWorktreeOperationRecord,
    lease_owner: &str,
) -> VibexResult<LifecycleClaim> {
    match WorktreeOperationRepository::try_claim(
        connection,
        &operation.operation_id,
        lease_owner,
        unix_timestamp_ms(),
        WORKTREE_OPERATION_LEASE_MS,
    )? {
        WorktreeOperationClaimOutcome::Acquired(record) => Ok(LifecycleClaim::Acquired(record)),
        WorktreeOperationClaimOutcome::Completed(record) => Ok(LifecycleClaim::Completed(record)),
        WorktreeOperationClaimOutcome::Busy(_) => Err(VibexError::conflict(
            "worktree_operation_busy",
            "worktree operation is already running",
        )),
        WorktreeOperationClaimOutcome::NeedsAttention(_) => Err(VibexError::conflict(
            "worktree_operation_needs_attention",
            "worktree operation requires manual attention",
        )),
    }
}

fn validate_merge_confirmation(
    request: &GitWorktreeMergeRequest,
    preflight: &GitWorktreeDestructivePreflight,
) -> VibexResult<()> {
    let revision = request.preflight_revision.as_deref().ok_or_else(|| {
        VibexError::validation(
            "worktree_preflight_required",
            "merge requires a current destructive preflight revision",
        )
    })?;
    let expected_source = request.expected_source_head.as_deref().ok_or_else(|| {
        VibexError::validation(
            "worktree_expected_source_head_required",
            "merge requires the preflight source head",
        )
    })?;
    let expected_target = request.expected_target_head.as_deref().ok_or_else(|| {
        VibexError::validation(
            "worktree_expected_target_head_required",
            "merge requires the preflight target head",
        )
    })?;
    if revision != preflight.revision
        || preflight.source_head.as_deref() != Some(expected_source)
        || preflight.target_head.as_deref() != Some(expected_target)
    {
        return Err(VibexError::conflict(
            "worktree_preflight_stale",
            "worktree merge facts changed after preflight",
        ));
    }
    Ok(())
}

fn validate_discard_confirmation(
    request: &GitWorktreeDiscardRequest,
    preflight: &GitWorktreeDestructivePreflight,
) -> VibexResult<()> {
    let revision = request.preflight_revision.as_deref().ok_or_else(|| {
        VibexError::validation(
            "worktree_preflight_required",
            "discard requires a current destructive preflight revision",
        )
    })?;
    let expected_source = request.expected_head.as_deref().ok_or_else(|| {
        VibexError::validation(
            "worktree_expected_source_head_required",
            "discard requires the preflight source head",
        )
    })?;
    if revision != preflight.revision || preflight.source_head.as_deref() != Some(expected_source) {
        return Err(VibexError::conflict(
            "worktree_preflight_stale",
            "worktree discard facts changed after preflight",
        ));
    }
    Ok(())
}

fn blocked_preflight_error(preflight: &GitWorktreeDestructivePreflight) -> VibexError {
    VibexError::conflict(
        "worktree_preflight_blocked",
        "worktree lifecycle operation is blocked by current Git facts",
    )
    .with_diagnostic(
        "blockingRiskCount",
        preflight
            .risks
            .iter()
            .filter(|risk| risk.blocking)
            .count()
            .to_string(),
    )
}

fn mark_lifecycle_failure(
    connection: &vibex_db::DbConnection,
    operation: &GitWorktreeOperationRecord,
    error: &VibexError,
    needs_resolution: bool,
) -> VibexResult<GitWorktreeOperationRecord> {
    let status = if needs_resolution {
        GitWorktreeOperationStatus::NeedsResolution
    } else {
        GitWorktreeOperationStatus::Failed
    };
    let diagnostic = GitWorktreeDiagnostic {
        code: if needs_resolution {
            "worktree_merge_needs_resolution".to_string()
        } else {
            "worktree_lifecycle_failed".to_string()
        },
        summary: format!(
            "worktree lifecycle operation failed: {}",
            bounded_error_code(error)
        ),
        severity: GitWorktreeDiagnosticSeverity::Error,
        retryable: !needs_resolution,
        recovery_action: Some(if needs_resolution {
            "resolve_conflicts".to_string()
        } else {
            "retry".to_string()
        }),
        operation_id: Some(operation.operation_id.clone()),
        worktree_id: None,
        observed_at_ms: unix_timestamp_ms(),
    };
    WorktreeOperationRepository::mark_outcome(
        connection,
        &operation.operation_id,
        status,
        if needs_resolution {
            GitWorktreeOperationCheckpoint::NeedsAttention
        } else {
            operation.detail.checkpoint
        },
        None,
        Some(&diagnostic),
    )
}

fn request_fingerprint<T: serde::Serialize>(kind: &str, request: &T) -> VibexResult<String> {
    let payload = serde_json::to_vec(request).map_err(|_| {
        VibexError::process(
            "worktree_request_fingerprint_failed",
            "worktree request could not be fingerprinted",
        )
    })?;
    Ok(format!("worktree-{kind}-v1:{:x}", Sha256::digest(payload)))
}

#[derive(Debug, Clone, Default)]
struct WorktreeLockRegistry {
    active: Arc<Mutex<BTreeSet<GitWorktreeLockKey>>>,
}

impl WorktreeLockRegistry {
    fn claim(&self, mut keys: Vec<GitWorktreeLockKey>) -> VibexResult<WorktreeLockClaim> {
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return Err(VibexError::validation(
                "worktree_lock_keys_missing",
                "worktree operation has no lifecycle lock identity",
            ));
        }
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active.iter().any(|active_key| keys.contains(active_key)) {
            return Err(VibexError::conflict(
                "worktree_lifecycle_busy",
                "another worktree lifecycle operation is using the same Git state",
            ));
        }
        active.extend(keys.iter().cloned());
        drop(active);
        Ok(WorktreeLockClaim {
            active: self.active.clone(),
            keys,
        })
    }
}

#[derive(Debug)]
struct WorktreeLockClaim {
    active: Arc<Mutex<BTreeSet<GitWorktreeLockKey>>>,
    keys: Vec<GitWorktreeLockKey>,
}

impl Drop for WorktreeLockClaim {
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in &self.keys {
            active.remove(key);
        }
    }
}

#[derive(Clone, Default)]
struct WorktreeFaultInjector {
    pending: Arc<Mutex<BTreeSet<GitWorktreeOperationCheckpoint>>>,
    simulated_process_loss: Arc<Mutex<BTreeSet<GitWorktreeOperationCheckpoint>>>,
}

impl WorktreeFaultInjector {
    fn trip(&self, checkpoint: GitWorktreeOperationCheckpoint) -> VibexResult<()> {
        let mut simulated_process_loss = self
            .simulated_process_loss
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if simulated_process_loss.remove(&checkpoint) {
            return Err(VibexError::process(
                "worktree_test_process_loss",
                "simulated process loss after worktree checkpoint",
            ));
        }
        drop(simulated_process_loss);
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.remove(&checkpoint) {
            return Err(VibexError::process(
                "worktree_test_fault",
                "injected worktree saga failure",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn fail_once_after(&self, checkpoint: GitWorktreeOperationCheckpoint) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(checkpoint);
    }

    #[cfg(test)]
    fn simulate_process_loss_once_after(&self, checkpoint: GitWorktreeOperationCheckpoint) {
        self.simulated_process_loss
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(checkpoint);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateStateClassification {
    NoSideEffects,
    ExactBranchOnly,
    ExactRegistration,
    UnregisteredDirectory,
    IdentityMismatch,
    Uninspectable,
}

fn classify_create_state(operation: &GitWorktreeOperationRecord) -> CreateStateClassification {
    let (Some(repo_root), Some(worktree_path), Some(branch), Some(base_head)) = (
        operation
            .detail
            .repository_identity
            .as_ref()
            .map(|identity| identity.repository_root.normalized_path.as_str()),
        operation.worktree_path.as_deref(),
        operation.branch.as_deref(),
        operation
            .detail
            .base_head
            .as_deref()
            .or(operation.head_before.as_deref()),
    ) else {
        return CreateStateClassification::Uninspectable;
    };
    let list = match vibex_git::worktree_list(WorkspaceId::new(), repo_root) {
        Ok(list) => list,
        Err(_) => return CreateStateClassification::Uninspectable,
    };
    if let Some(summary) = list
        .worktrees
        .iter()
        .find(|summary| vibex_git::same_path_identity(&summary.path, worktree_path))
    {
        return if summary.branch.as_deref() == Some(branch)
            && summary.head.as_deref() == Some(base_head)
        {
            CreateStateClassification::ExactRegistration
        } else {
            CreateStateClassification::IdentityMismatch
        };
    }
    if Path::new(worktree_path).exists() {
        return CreateStateClassification::UnregisteredDirectory;
    }
    match vibex_git::local_branch_head(repo_root, branch) {
        Ok(Some(head)) if head == base_head => CreateStateClassification::ExactBranchOnly,
        Ok(Some(_)) => CreateStateClassification::IdentityMismatch,
        Ok(None) => CreateStateClassification::NoSideEffects,
        Err(_) => CreateStateClassification::Uninspectable,
    }
}

fn create_lock_keys(
    repository_identity: &vibex_core::GitRepositoryIdentity,
    worktree_path_identity: &GitPathIdentity,
) -> Vec<GitWorktreeLockKey> {
    vec![
        GitWorktreeLockKey {
            kind: GitWorktreeLockKind::Repository,
            key: repository_identity.comparison_key.clone(),
        },
        GitWorktreeLockKey {
            kind: GitWorktreeLockKind::WorktreePath,
            key: worktree_path_identity.comparison_key.clone(),
        },
    ]
}

fn operation_detail_is_executable(detail: &GitWorktreeOperationDetail) -> bool {
    detail.schema_version == WORKTREE_OPERATION_DETAIL_SCHEMA_VERSION
        && detail.checkpoint != GitWorktreeOperationCheckpoint::Unknown
        && detail
            .lock_keys
            .iter()
            .all(|key| key.kind != GitWorktreeLockKind::Unknown)
}

fn ensure_lifecycle_operation_contract(
    connection: &vibex_db::DbConnection,
    operation: &GitWorktreeOperationRecord,
) -> VibexResult<()> {
    if operation_detail_is_executable(&operation.detail) {
        return Ok(());
    }
    let diagnostic = GitWorktreeDiagnostic {
        code: "worktree_operation_contract_unknown".to_string(),
        summary: "worktree lifecycle operation uses an unknown durable contract".to_string(),
        severity: GitWorktreeDiagnosticSeverity::Error,
        retryable: false,
        recovery_action: Some("upgrade_or_inspect".to_string()),
        operation_id: Some(operation.operation_id.clone()),
        worktree_id: None,
        observed_at_ms: unix_timestamp_ms(),
    };
    WorktreeOperationRepository::mark_outcome(
        connection,
        &operation.operation_id,
        GitWorktreeOperationStatus::NeedsAttention,
        GitWorktreeOperationCheckpoint::NeedsAttention,
        None,
        Some(&diagnostic),
    )?;
    Err(VibexError::conflict(
        "worktree_operation_contract_unknown",
        "worktree operation uses an unknown durable contract",
    ))
}

fn verify_reserved_create(
    operation: &GitWorktreeOperationRecord,
    project_id: &ProjectId,
    request_fingerprint: &str,
) -> VibexResult<()> {
    if operation.operation != GitWorktreeOperationKind::Create
        || &operation.project_id != project_id
        || operation.detail.request_fingerprint.as_deref() != Some(request_fingerprint)
    {
        return Err(VibexError::conflict(
            "worktree_operation_idempotency_conflict",
            "worktree idempotency key belongs to another request",
        ));
    }
    Ok(())
}

fn create_request_fingerprint(
    project_id: &ProjectId,
    request: &GitWorktreeCreateRequest,
) -> VibexResult<String> {
    let payload = serde_json::to_vec(&(project_id, request)).map_err(|_| {
        VibexError::process(
            "worktree_request_fingerprint_failed",
            "worktree request could not be fingerprinted",
        )
    })?;
    Ok(format!("worktree-create-v1:{:x}", Sha256::digest(payload)))
}

fn snapshot_revision(
    eligibility: &GitProjectEligibility,
    managed: &[GitManagedWorktreeRecord],
    operations: &[GitWorktreeOperationRecord],
) -> VibexResult<String> {
    let payload = serde_json::to_vec(&(eligibility, managed, operations)).map_err(|_| {
        VibexError::process(
            "worktree_snapshot_revision_failed",
            "worktree lifecycle snapshot could not be fingerprinted",
        )
    })?;
    Ok(format!(
        "worktree-snapshot-v1:{:x}",
        Sha256::digest(payload)
    ))
}

fn managed_matches_summary(
    record: &GitManagedWorktreeRecord,
    summary: &GitWorktreeSummary,
) -> bool {
    match (
        record.worktree_path_identity.as_ref(),
        summary.path_identity.as_ref(),
    ) {
        (Some(left), Some(right)) if left.comparison_key == right.comparison_key => true,
        _ => vibex_git::same_path_identity(&record.worktree_path, &summary.path),
    }
}

fn ineligible_create_error(eligibility: &GitProjectEligibility) -> VibexError {
    VibexError::validation(
        "worktree_project_ineligible",
        "project is not eligible for managed Git worktrees",
    )
    .with_diagnostic(
        "eligibilityState",
        match eligibility.state {
            GitProjectEligibilityState::Probing => "probing",
            GitProjectEligibilityState::Eligible => "eligible",
            GitProjectEligibilityState::Ineligible => "ineligible",
            GitProjectEligibilityState::Unknown => "unknown",
        },
    )
}

fn incomplete_create_intent(field: &str) -> VibexError {
    VibexError::storage(
        "worktree_create_intent_incomplete",
        "durable worktree create intent is incomplete",
    )
    .with_diagnostic("field", field)
}

fn reconciliation_diagnostic(
    operation: &GitWorktreeOperationRecord,
    code: &str,
    summary: &str,
    retryable: bool,
) -> GitWorktreeDiagnostic {
    GitWorktreeDiagnostic {
        code: code.to_string(),
        summary: summary.to_string(),
        severity: GitWorktreeDiagnosticSeverity::Warning,
        retryable,
        recovery_action: retryable.then(|| "retry".to_string()),
        operation_id: Some(operation.operation_id.clone()),
        worktree_id: Some(operation.operation_id.clone()),
        observed_at_ms: unix_timestamp_ms(),
    }
}

fn managed_diagnostic(
    record: &GitManagedWorktreeRecord,
    code: &str,
    summary: &str,
) -> GitWorktreeDiagnostic {
    GitWorktreeDiagnostic {
        code: code.to_string(),
        summary: summary.to_string(),
        severity: GitWorktreeDiagnosticSeverity::Warning,
        retryable: false,
        recovery_action: Some("inspect_worktree".to_string()),
        operation_id: None,
        worktree_id: Some(record.worktree_id.clone()),
        observed_at_ms: unix_timestamp_ms(),
    }
}

fn bounded_error_code(error: &VibexError) -> String {
    error.code.chars().take(96).collect()
}

fn is_busy_error(error: &VibexError) -> bool {
    matches!(
        error.code.as_str(),
        "worktree_lifecycle_busy" | "worktree_operation_busy" | "git_mutation_in_progress"
    )
}

fn is_simulated_process_loss(error: &VibexError) -> bool {
    cfg!(test) && error.code == "worktree_test_process_loss"
}

fn read_directories(root: &Path) -> VibexResult<Vec<PathBuf>> {
    let entries = fs::read_dir(root).map_err(|error| {
        VibexError::storage(
            "worktree_storage_scan_failed",
            "failed to inspect managed worktree storage",
        )
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
    })?;
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            VibexError::storage(
                "worktree_storage_scan_failed",
                "failed to inspect managed worktree storage",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn path_identity_is_within(candidate: &GitPathIdentity, root: &GitPathIdentity) -> bool {
    let candidate = candidate
        .canonical_path
        .as_deref()
        .unwrap_or(&candidate.normalized_path);
    let root = root
        .canonical_path
        .as_deref()
        .unwrap_or(&root.normalized_path);
    Path::new(candidate).starts_with(root)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;
    use vibex_db::{apply_migrations, open_database};

    use super::*;

    #[test]
    fn ordered_lock_claim_is_atomic_and_order_independent() {
        let registry = WorktreeLockRegistry::default();
        let repo = GitWorktreeLockKey {
            kind: GitWorktreeLockKind::Repository,
            key: "repo-a".to_string(),
        };
        let path = GitWorktreeLockKey {
            kind: GitWorktreeLockKind::WorktreePath,
            key: "path-a".to_string(),
        };
        let first = registry.claim(vec![path.clone(), repo.clone()]).unwrap();
        let error = registry
            .claim(vec![repo.clone(), path.clone()])
            .unwrap_err();
        assert_eq!(error.code, "worktree_lifecycle_busy");
        drop(first);
        let second = registry.claim(vec![repo, path]).unwrap();
        assert_eq!(second.keys[0].kind, GitWorktreeLockKind::Repository);
        assert_eq!(second.keys[1].kind, GitWorktreeLockKind::WorktreePath);
    }

    #[test]
    fn duplicate_create_requests_share_one_operation_under_race() {
        use std::sync::Barrier;
        use std::thread;

        let fixture = Fixture::new();
        let coordinator = Arc::new(fixture.coordinator.clone());
        let request = Arc::new(fixture.request.clone());
        let context = fixture.context.clone();
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let coordinator = coordinator.clone();
            let request = request.clone();
            let context = context.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                coordinator.create(&request, context)
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(results.iter().any(Result::is_ok));
        assert!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .all(|error| {
                    matches!(
                        error.code.as_str(),
                        "worktree_lifecycle_busy" | "worktree_operation_busy"
                    )
                })
        );
        coordinator
            .create(&request, fixture.context.clone())
            .unwrap();
        fixture.assert_single_result();
    }

    #[test]
    fn concurrent_create_and_discard_converge_without_corruption() {
        use std::sync::Barrier;
        use std::thread;

        let fixture = Fixture::new();
        let first = fixture.create();
        let discard_draft = GitWorktreeDiscardRequest {
            workspace_id: first.workspace.id.clone(),
            worktree_path: first.worktree.path.clone(),
            force: false,
            expected_head: None,
            preflight_revision: None,
        };
        let discard_preflight = fixture
            .coordinator
            .discard_preflight(&discard_draft)
            .unwrap();
        let discard_request = GitWorktreeDiscardRequest {
            expected_head: discard_preflight.source_head.clone(),
            preflight_revision: Some(discard_preflight.revision),
            ..discard_draft.clone()
        };
        let second_id = RequestId::new();
        let second_request = GitWorktreeCreateRequest {
            workspace_id: fixture.workspace.id.clone(),
            branch_name: format!(
                "feature/{}",
                second_id.as_str().rsplit('_').next().unwrap_or("parallel")
            ),
            base_ref: Some("main".to_string()),
            name: Some("parallel-create".to_string()),
            worktree_path: None,
            target_workspace_id: Some(fixture.workspace.id.clone()),
            target_branch: Some("main".to_string()),
        };
        let second_context =
            WorktreeCreateContext::new(second_id, Some("create-discard-race".to_string()));
        let coordinator = Arc::new(fixture.coordinator.clone());
        let barrier = Arc::new(Barrier::new(3));

        let create_handle = {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            let request = second_request.clone();
            let context = second_context.clone();
            thread::spawn(move || {
                barrier.wait();
                coordinator.create(&request, context)
            })
        };
        let discard_handle = {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            let request = discard_request.clone();
            thread::spawn(move || {
                barrier.wait();
                coordinator.discard(&request)
            })
        };
        barrier.wait();
        let create_result = create_handle.join().unwrap();
        let discard_result = discard_handle.join().unwrap();
        for error in [create_result.as_ref().err(), discard_result.as_ref().err()]
            .into_iter()
            .flatten()
        {
            assert!(
                matches!(
                    error.code.as_str(),
                    "worktree_lifecycle_busy" | "worktree_operation_busy"
                ),
                "unexpected race error: {error:?}"
            );
        }

        let second = match create_result {
            Ok(result) => result,
            Err(_) => coordinator.create(&second_request, second_context).unwrap(),
        };
        if discard_result.is_err() {
            let current = coordinator.discard_preflight(&discard_draft).unwrap();
            coordinator
                .discard(&GitWorktreeDiscardRequest {
                    expected_head: current.source_head.clone(),
                    preflight_revision: Some(current.revision),
                    ..discard_draft
                })
                .unwrap();
        }

        assert!(Path::new(&second.worktree.path).is_dir());
        assert!(!Path::new(&first.worktree.path).exists());
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        let managed = ManagedWorktreeRepository::list_all(&connection).unwrap();
        assert_eq!(managed.len(), 2);
        assert_eq!(
            managed
                .iter()
                .filter(|record| record.status == GitManagedWorktreeStatus::Active)
                .count(),
            1
        );
        assert_eq!(
            managed
                .iter()
                .filter(|record| record.status == GitManagedWorktreeStatus::Discarded)
                .count(),
            1
        );
        let (_, project_workspace) = workspace_record(&connection, &fixture.workspace.id).unwrap();
        assert_eq!(
            WorktreeOperationRepository::list_for_project(
                &connection,
                &project_workspace.project_id
            )
            .unwrap()
            .len(),
            3
        );
    }

    #[test]
    fn every_create_checkpoint_retries_to_one_durable_result() {
        for checkpoint in [
            GitWorktreeOperationCheckpoint::IntentRecorded,
            GitWorktreeOperationCheckpoint::LocksAcquired,
            GitWorktreeOperationCheckpoint::GitAddStarted,
            GitWorktreeOperationCheckpoint::GitAdded,
            GitWorktreeOperationCheckpoint::WorkspacePersisted,
            GitWorktreeOperationCheckpoint::ManagedRecordPersisted,
            GitWorktreeOperationCheckpoint::DatabaseCommitted,
        ] {
            let fixture = Fixture::new();
            fixture.coordinator.fail_once_after(checkpoint);
            let first = fixture
                .coordinator
                .create(&fixture.request, fixture.context.clone());
            assert!(first.is_err(), "checkpoint {checkpoint:?} did not fail");
            let recovered = fixture
                .coordinator
                .create(&fixture.request, fixture.context.clone())
                .unwrap_or_else(|error| panic!("checkpoint {checkpoint:?}: {error:?}"));
            assert_eq!(
                recovered.operation.status,
                GitWorktreeOperationStatus::Completed
            );
            fixture.assert_single_result();
            fixture.coordinator.reconcile_on_startup().unwrap();
            let first_snapshot = fixture
                .coordinator
                .lifecycle_snapshot(&fixture.workspace.id)
                .unwrap();
            fixture.coordinator.reconcile_on_startup().unwrap();
            let second_snapshot = fixture
                .coordinator
                .lifecycle_snapshot(&fixture.workspace.id)
                .unwrap();
            assert_eq!(first_snapshot, second_snapshot);
            fixture.assert_single_result();
        }
    }

    #[test]
    fn every_create_checkpoint_recovers_after_simulated_process_loss() {
        for checkpoint in [
            GitWorktreeOperationCheckpoint::IntentRecorded,
            GitWorktreeOperationCheckpoint::LocksAcquired,
            GitWorktreeOperationCheckpoint::GitAddStarted,
            GitWorktreeOperationCheckpoint::GitAdded,
            GitWorktreeOperationCheckpoint::WorkspacePersisted,
            GitWorktreeOperationCheckpoint::ManagedRecordPersisted,
            GitWorktreeOperationCheckpoint::DatabaseCommitted,
        ] {
            let fixture = Fixture::new();
            fixture
                .coordinator
                .simulate_process_loss_once_after(checkpoint);
            let first = fixture
                .coordinator
                .create(&fixture.request, fixture.context.clone());
            assert_eq!(
                first.unwrap_err().code,
                "worktree_test_process_loss",
                "checkpoint {checkpoint:?} did not simulate process loss"
            );

            let restarted = WorktreeCoordinator::new(fixture.coordinator.db_path.clone());
            let report = restarted
                .reconcile_on_startup()
                .unwrap_or_else(|error| panic!("checkpoint {checkpoint:?}: {error:?}"));
            assert_eq!(report.completed_operations, 1, "checkpoint {checkpoint:?}");
            fixture.assert_single_result();

            let snapshot = restarted.lifecycle_snapshot(&fixture.workspace.id).unwrap();
            assert_eq!(snapshot.operations.len(), 1);
            assert_eq!(
                snapshot.operations[0].status,
                GitWorktreeOperationStatus::Completed
            );
            restarted.reconcile_on_startup().unwrap();
            let repeated = restarted.lifecycle_snapshot(&fixture.workspace.id).unwrap();
            assert_eq!(snapshot, repeated);
            fixture.assert_single_result();
        }
    }

    #[test]
    fn unknown_operation_checkpoint_fails_closed_without_side_effects() {
        let fixture = Fixture::new();
        fixture
            .coordinator
            .fail_once_after(GitWorktreeOperationCheckpoint::IntentRecorded);
        assert!(
            fixture
                .coordinator
                .create(&fixture.request, fixture.context.clone())
                .is_err()
        );
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        connection
            .execute(
                "UPDATE git_worktree_operations SET checkpoint = 'future_checkpoint'",
                [],
            )
            .unwrap();
        let operation = WorktreeOperationRepository::get(&connection, &fixture.context.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            operation.detail.checkpoint,
            GitWorktreeOperationCheckpoint::Unknown
        );
        let worktree_path = operation.worktree_path.unwrap();
        drop(connection);

        let error = fixture
            .coordinator
            .create(&fixture.request, fixture.context.clone())
            .unwrap_err();
        assert_eq!(error.code, "worktree_operation_contract_unknown");
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        let operation = WorktreeOperationRepository::get(&connection, &fixture.context.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.status, GitWorktreeOperationStatus::NeedsAttention);
        assert_eq!(
            operation.detail.checkpoint,
            GitWorktreeOperationCheckpoint::NeedsAttention
        );
        assert!(!Path::new(&worktree_path).exists());
        assert!(
            ManagedWorktreeRepository::list_all(&connection)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn merge_uses_fixed_target_and_rejects_stale_preflight() {
        let fixture = Fixture::new();
        let created = fixture.create();
        fs::write(
            Path::new(&created.worktree.path).join("feature.txt"),
            "feature\n",
        )
        .unwrap();
        git(Path::new(&created.worktree.path), &["add", "feature.txt"]);
        git(
            Path::new(&created.worktree.path),
            &["commit", "-m", "feature"],
        );

        let draft = GitWorktreeMergeRequest {
            workspace_id: created.workspace.id.clone(),
            source_path: created.worktree.path.clone(),
            target_workspace_id: None,
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
        };
        let preflight = fixture.coordinator.merge_preflight(&draft).unwrap();
        assert!(preflight.allowed);
        let confirmed = GitWorktreeMergeRequest {
            expected_source_head: preflight.source_head.clone(),
            expected_target_head: preflight.target_head.clone(),
            preflight_revision: Some(preflight.revision.clone()),
            ..draft.clone()
        };

        fs::write(fixture.repo().join("target.txt"), "target\n").unwrap();
        git(&fixture.repo(), &["add", "target.txt"]);
        git(&fixture.repo(), &["commit", "-m", "target moved"]);
        let error = fixture.coordinator.merge(&confirmed).unwrap_err();
        assert_eq!(error.code, "worktree_preflight_stale");

        let current = fixture.coordinator.merge_preflight(&draft).unwrap();
        let current_request = GitWorktreeMergeRequest {
            expected_source_head: current.source_head.clone(),
            expected_target_head: current.target_head.clone(),
            preflight_revision: Some(current.revision),
            ..draft
        };
        let completed = fixture.coordinator.merge(&current_request).unwrap();
        assert_eq!(completed.status, GitWorktreeOperationStatus::Completed);
        assert!(fixture.repo().join("feature.txt").is_file());
    }

    #[test]
    fn concurrent_source_merges_serialize_and_revalidate_target_head() {
        use std::sync::Barrier;
        use std::thread;

        let fixture = Fixture::new();
        let first = fixture.create();
        let second_id = RequestId::new();
        let second = fixture
            .coordinator
            .create(
                &GitWorktreeCreateRequest {
                    workspace_id: fixture.workspace.id.clone(),
                    branch_name: format!(
                        "feature/{}",
                        second_id.as_str().rsplit('_').next().unwrap_or("merge")
                    ),
                    base_ref: Some("main".to_string()),
                    name: Some("parallel-merge".to_string()),
                    worktree_path: None,
                    target_workspace_id: Some(fixture.workspace.id.clone()),
                    target_branch: Some("main".to_string()),
                },
                WorktreeCreateContext::new(second_id, Some("parallel-source-merge".to_string())),
            )
            .unwrap();
        for (created, file) in [(&first, "first.txt"), (&second, "second.txt")] {
            fs::write(Path::new(&created.worktree.path).join(file), file).unwrap();
            git(Path::new(&created.worktree.path), &["add", file]);
            git(
                Path::new(&created.worktree.path),
                &["commit", "-m", &format!("add {file}")],
            );
        }

        let first_draft = GitWorktreeMergeRequest {
            workspace_id: first.workspace.id.clone(),
            source_path: first.worktree.path.clone(),
            target_workspace_id: None,
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
        };
        let second_draft = GitWorktreeMergeRequest {
            workspace_id: second.workspace.id.clone(),
            source_path: second.worktree.path.clone(),
            target_workspace_id: None,
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
        };
        let first_preflight = fixture.coordinator.merge_preflight(&first_draft).unwrap();
        let second_preflight = fixture.coordinator.merge_preflight(&second_draft).unwrap();
        let first_request = GitWorktreeMergeRequest {
            expected_source_head: first_preflight.source_head,
            expected_target_head: first_preflight.target_head,
            preflight_revision: Some(first_preflight.revision),
            ..first_draft.clone()
        };
        let second_request = GitWorktreeMergeRequest {
            expected_source_head: second_preflight.source_head,
            expected_target_head: second_preflight.target_head,
            preflight_revision: Some(second_preflight.revision),
            ..second_draft.clone()
        };
        let coordinator = Arc::new(fixture.coordinator.clone());
        let barrier = Arc::new(Barrier::new(3));
        let first_handle = {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                coordinator.merge(&first_request)
            })
        };
        let second_handle = {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                coordinator.merge(&second_request)
            })
        };
        barrier.wait();
        let first_result = first_handle.join().unwrap();
        let second_result = second_handle.join().unwrap();
        assert_eq!(
            [first_result.is_ok(), second_result.is_ok()]
                .into_iter()
                .filter(|succeeded| *succeeded)
                .count(),
            1
        );
        for error in [first_result.as_ref().err(), second_result.as_ref().err()]
            .into_iter()
            .flatten()
        {
            assert!(
                matches!(
                    error.code.as_str(),
                    "worktree_lifecycle_busy"
                        | "worktree_operation_busy"
                        | "worktree_preflight_stale"
                ),
                "unexpected merge race error: {error:?}"
            );
        }

        let retry_draft = if first_result.is_err() {
            first_draft
        } else {
            second_draft
        };
        let current = coordinator.merge_preflight(&retry_draft).unwrap();
        assert!(current.allowed);
        coordinator
            .merge(&GitWorktreeMergeRequest {
                expected_source_head: current.source_head.clone(),
                expected_target_head: current.target_head.clone(),
                preflight_revision: Some(current.revision),
                ..retry_draft
            })
            .unwrap();

        assert!(fixture.repo().join("first.txt").is_file());
        assert!(fixture.repo().join("second.txt").is_file());
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        let managed = ManagedWorktreeRepository::list_all(&connection).unwrap();
        assert_eq!(managed.len(), 2);
        assert!(
            managed
                .iter()
                .all(|record| record.status == GitManagedWorktreeStatus::Merged)
        );
        let (_, project_workspace) = workspace_record(&connection, &fixture.workspace.id).unwrap();
        let operations = WorktreeOperationRepository::list_for_project(
            &connection,
            &project_workspace.project_id,
        )
        .unwrap();
        assert_eq!(
            operations
                .iter()
                .filter(|operation| {
                    operation.operation == GitWorktreeOperationKind::MergeBack
                        && operation.status == GitWorktreeOperationStatus::Completed
                })
                .count(),
            2
        );
        assert!(operations.iter().all(|operation| {
            !matches!(
                operation.status,
                GitWorktreeOperationStatus::Pending | GitWorktreeOperationStatus::Running
            )
        }));
    }

    #[test]
    fn discard_preflight_requires_force_for_dirty_source() {
        let fixture = Fixture::new();
        let created = fixture.create();
        fs::write(
            Path::new(&created.worktree.path).join("uncommitted.txt"),
            "local\n",
        )
        .unwrap();
        let draft = GitWorktreeDiscardRequest {
            workspace_id: created.workspace.id.clone(),
            worktree_path: created.worktree.path.clone(),
            force: false,
            expected_head: None,
            preflight_revision: None,
        };
        let blocked = fixture.coordinator.discard_preflight(&draft).unwrap();
        assert!(!blocked.allowed);
        assert!(
            blocked
                .risks
                .iter()
                .any(|risk| { risk.kind == GitWorktreeRiskKind::DirtySource && risk.blocking })
        );
        let forced_draft = GitWorktreeDiscardRequest {
            force: true,
            ..draft
        };
        let allowed = fixture
            .coordinator
            .discard_preflight(&forced_draft)
            .unwrap();
        assert!(allowed.allowed);
        let confirmed = GitWorktreeDiscardRequest {
            expected_head: allowed.source_head.clone(),
            preflight_revision: Some(allowed.revision),
            ..forced_draft
        };
        let completed = fixture.coordinator.discard(&confirmed).unwrap();
        assert_eq!(completed.status, GitWorktreeOperationStatus::Completed);
        assert!(!Path::new(&created.worktree.path).exists());
    }

    #[test]
    fn missing_managed_directory_is_reported_without_cleanup() {
        let fixture = Fixture::new();
        let created = fixture.create();
        fs::remove_dir_all(&created.worktree.path).unwrap();
        let first = fixture.coordinator.reconcile_on_startup().unwrap();
        assert!(first.needs_attention >= 1);
        let snapshot = fixture
            .coordinator
            .lifecycle_snapshot(&fixture.workspace.id)
            .unwrap();
        let managed = &snapshot.managed_worktrees[0];
        assert_eq!(managed.status, GitManagedWorktreeStatus::Active);
        assert_eq!(
            managed.reconciliation_state,
            GitWorktreeReconciliationState::NeedsAttention
        );
        assert_eq!(
            managed.diagnostic.as_ref().map(|value| value.code.as_str()),
            Some("managed_worktree_directory_missing")
        );
        fixture.coordinator.reconcile_on_startup().unwrap();
        let repeated = fixture
            .coordinator
            .lifecycle_snapshot(&fixture.workspace.id)
            .unwrap();
        assert_eq!(snapshot, repeated);
        assert!(!Path::new(&created.worktree.path).exists());
    }

    #[test]
    fn unowned_managed_storage_directory_is_reported_without_cleanup() {
        let fixture = Fixture::new();
        let orphan = fixture
            .coordinator
            .db_path
            .parent()
            .unwrap()
            .join("worktrees/unowned-project/leftover");
        fs::create_dir_all(&orphan).unwrap();

        let report = fixture.coordinator.reconcile_on_startup().unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "vibex_worktree_orphan_directory" })
        );
        assert!(orphan.is_dir());
    }

    #[test]
    fn custom_create_path_is_authoritative_and_registered_once() {
        let fixture = Fixture::new();
        let custom_path = fixture._temp.path().join("custom/worktree");
        let mut request = fixture.request.clone();
        request.worktree_path = Some(custom_path.to_string_lossy().into_owned());

        let created = fixture
            .coordinator
            .create(&request, fixture.context.clone())
            .unwrap();

        assert!(vibex_git::same_path_identity(
            &created.worktree.path,
            &custom_path
        ));
        assert!(custom_path.is_dir());
        fixture.assert_single_result();
    }

    #[test]
    fn custom_create_path_cannot_be_nested_inside_an_existing_workspace() {
        let fixture = Fixture::new();
        let mut request = fixture.request.clone();
        request.worktree_path = Some(
            fixture
                .repo
                .join("nested-worktree")
                .to_string_lossy()
                .into_owned(),
        );

        let error = fixture
            .coordinator
            .create(&request, fixture.context.clone())
            .unwrap_err();

        assert_eq!(error.code, "worktree_path_inside_repository");
    }

    #[test]
    fn custom_create_path_rejects_existing_paths_without_reserving_an_intent() {
        let fixture = Fixture::new();
        let custom_path = fixture._temp.path().join("existing-worktree");
        fs::create_dir_all(&custom_path).unwrap();
        let mut request = fixture.request.clone();
        request.worktree_path = Some(custom_path.to_string_lossy().into_owned());

        let error = fixture
            .coordinator
            .create(&request, fixture.context.clone())
            .unwrap_err();

        assert_eq!(error.code, "worktree_path_exists");
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        assert!(
            WorktreeOperationRepository::list_for_project(
                &connection,
                &fixture.workspace.project_id,
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn custom_create_path_cannot_be_nested_inside_another_workspace() {
        let fixture = Fixture::new();
        let other_workspace = fixture._temp.path().join("other-project");
        fs::create_dir_all(&other_workspace).unwrap();
        let connection = open_database(&fixture.coordinator.db_path).unwrap();
        WorkspaceRepository::ensure(
            &connection,
            &other_workspace,
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let mut request = fixture.request.clone();
        request.worktree_path = Some(
            other_workspace
                .join("nested-worktree")
                .to_string_lossy()
                .into_owned(),
        );

        let error = fixture
            .coordinator
            .create(&request, fixture.context.clone())
            .unwrap_err();

        assert_eq!(error.code, "worktree_path_inside_workspace");
    }

    struct Fixture {
        _temp: TempDir,
        repo: PathBuf,
        coordinator: WorktreeCoordinator,
        workspace: WorkspaceRecord,
        request: GitWorktreeCreateRequest,
        context: WorktreeCreateContext,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "test@example.invalid"]);
            git(&repo, &["config", "user.name", "Vibex Test"]);
            fs::write(repo.join("README.md"), "foundation\n").unwrap();
            git(&repo, &["add", "README.md"]);
            git(&repo, &["commit", "-m", "initial"]);

            let db_path = temp.path().join("runtime/vibex.db");
            fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            let mut connection = open_database(&db_path).unwrap();
            apply_migrations(&mut connection).unwrap();
            let (_, workspace) =
                WorkspaceRepository::ensure(&connection, &repo, WorkspaceMode::CurrentCheckout)
                    .unwrap();
            let coordinator = WorktreeCoordinator::new(db_path);
            let request_id = RequestId::new();
            Self {
                _temp: temp,
                repo,
                coordinator,
                request: GitWorktreeCreateRequest {
                    workspace_id: workspace.id.clone(),
                    branch_name: format!(
                        "feature/{}",
                        request_id.as_str().rsplit('_').next().unwrap_or("worktree")
                    ),
                    base_ref: Some("main".to_string()),
                    name: Some("fault-recovery".to_string()),
                    worktree_path: None,
                    target_workspace_id: Some(workspace.id.clone()),
                    target_branch: Some("main".to_string()),
                },
                context: WorktreeCreateContext::new(
                    request_id,
                    Some("create-fault-recovery".to_string()),
                ),
                workspace,
            }
        }

        fn create(&self) -> GitWorktreeCreateResult {
            self.coordinator
                .create(&self.request, self.context.clone())
                .unwrap()
        }

        fn repo(&self) -> PathBuf {
            self.repo.clone()
        }

        fn assert_single_result(&self) {
            let connection = open_database(&self.coordinator.db_path).unwrap();
            let (_, project_workspace) = workspace_record(&connection, &self.workspace.id).unwrap();
            let managed = ManagedWorktreeRepository::list_all(&connection).unwrap();
            let operations = WorktreeOperationRepository::list_for_project(
                &connection,
                &project_workspace.project_id,
            )
            .unwrap();
            let worktree_workspaces = WorkspaceRepository::list(&connection)
                .unwrap()
                .into_iter()
                .filter(|(_, workspace)| workspace.mode == WorkspaceMode::VibexWorktree)
                .count();
            assert_eq!(managed.len(), 1);
            assert_eq!(operations.len(), 1);
            assert_eq!(worktree_workspaces, 1);
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
