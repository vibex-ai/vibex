# Managed Worktree Identity And Recovery

## Scenario: Runtime-owned managed Worktree lifecycle

### 1. Scope / Trigger

- Trigger: changing managed Worktree identity, eligibility, persistence,
  create/merge/discard orchestration, startup reconciliation, or Backend/remote
  Worktree capabilities.
- `DesktopRuntime` is the only Worktree state authority. `crates/git` probes and
  mutates Git, `crates/db` stores durable facts, and the runtime-owned
  `WorktreeCoordinator` is the only layer allowed to orchestrate both.
- Lifecycle locks fence Git/SQLite mutations only. They must never become Agent,
  Terminal, Files, or Session admission locks.

### 2. Signatures

```text
crates/git
  canonical_path_identity(path) -> GitPathIdentity
  same_path_identity(left, right) -> bool
  repository_identity(repo_path) -> GitRepositoryIdentity
  project_git_eligibility(project_id, project_path) -> GitProjectEligibility
  worktree_add_recoverable(repo_path, path, request, expected_head, recovery)
    -> GitWorktreeSummary
  worktree_merge(target_path, source_ref, expected_source_head,
    expected_target_branch, expected_target_head) -> String
  worktree_rebase_source(source_path, target_path, source_branch,
    expected_source_head, expected_target_branch, expected_target_head) -> String
  worktree_rebase_finish(source_path, target_path, source_branch,
    rebased_source_head, expected_target_branch, expected_target_head) -> String
  worktree_remove(repo_path, GitWorktreeDiscardRequest { expectedHead, ... })
    -> String

GitBackend
  git_worktree_eligibility(workspace_id) -> GitProjectEligibility
  git_worktree_snapshot(workspace_id) -> GitWorktreeLifecycleSnapshot
  git_worktree_create(MutationRequest<GitWorktreeCreateRequest>)
    -> GitWorktreeCreateResult

GitWorktreeCreateRequest {
  workspaceId, branchName, baseRef?, name?, worktreePath?,
  targetWorkspaceId?, targetBranch?
}

GitWorktreeCreateResult { worktree, workspace, managed, operation }

GitWorktreeMergeRequest {
  workspaceId, sourcePath, targetWorkspaceId?, strategy?, expectedSourceHead?,
  expectedTargetHead?, preflightRevision?
}

GitWorktreeMergeStrategy { no_ff_merge, rebase_and_merge, unknown }

GitWorktreeDiscardRequest {
  workspaceId, worktreePath, force, deleteBranch?, expectedHead?, preflightRevision?
}

GitWorktreeDestructivePreflight {
  action, allowed, revision, sourceHead?, targetHead?, risks[], observedAtMs
}
```

SQLite schema version 33 adds only nullable/defaulted columns:

```text
git_managed_worktrees:
  repo_identity_key, worktree_identity_key, repository_identity_json,
  worktree_identity_json, canonical_worktree_path, origin_workspace_id,
  base_head, target_workspace_id, target_branch,
  reconciliation_state = "unverified", diagnostic_json

git_worktree_operations:
  idempotency_key, request_fingerprint,
  checkpoint = "intent_recorded", detail_json,
  lease_owner, lease_expires_at_ms, attempt = 0, diagnostic_json
```

`worktree_identity_key` and `idempotency_key` each have a partial unique index.
Operation claims are conditional updates; an expired lease may be taken over,
but an unexpired `Running` lease returns busy.

### 3. Contracts

#### Identity and eligibility

- All lookup, ownership, Git-list matching, orphan detection, and lock keys use
  `GitPathIdentity.comparisonKey`; do not introduce a second path comparison
  helper. Preserve the display path separately.
- Existing filesystem aliases are resolved through canonical/filesystem
  identity. Missing tails use a canonical existing ancestor plus lexical
  normalization. Windows drive/separator/case semantics are normalized;
  symlink and macOS system aliases must compare as the same existing path.
  Do not case-fold an unproven macOS path or treat a Unix backslash as a path
  separator.
- Repository identity is the canonical `git rev-parse --git-common-dir`
  identity. Main, nested, and linked worktrees of one repository therefore
  share the repository lock key.
- Eligibility requires a non-bare working tree, repository/common-dir identity,
  a commit-resolving `HEAD`, and a valid default base ref. Bare, unborn,
  missing, non-directory, non-working-tree, and unprobeable paths return typed
  `Ineligible` reasons and a revision; UI checks never replace command-time
  revalidation.

#### Stable managed record

- A managed record fixes `originWorkspaceId`, `baseRef`, `baseHead`,
  `targetWorkspaceId`, and `targetBranch` at creation. `base*` means where the
  branch started; `target*` means where Merge Back goes.
- Generic Session creation must resolve an active Workspace by normalized root
  path and mode before applying the new-directory Project creation path. A
  registered `VibexWorktree` keeps the Project identity assigned by the
  coordinator; its directory must never become a second Project merely because
  an Agent Session is opened there.
- Merge must use the stored target. A caller may omit the target or repeat the
  stored target, but may not redirect an existing managed Worktree.
- An absent merge strategy preserves the legacy `no_ff_merge` behavior. Unknown
  strategies fail closed. `rebase_and_merge` rebases the managed source branch
  onto the fixed target HEAD, records the rewritten source HEAD, then advances
  the fixed target with `--ff-only`; it never creates a merge commit.
- Old rows remain readable. If origin/base/target identity cannot be proven,
  reconciliation sets `NeedsAttention`; it never guesses a branch.
- Unknown serialized enum values decode to `Unknown` and are non-actionable.
  They must not be treated as success or ordinary retryable failure.
- An unknown operation-detail schema version, checkpoint, or lock kind is
  readable for diagnostics but not executable; persist `NeedsAttention` before
  any Git/filesystem side effect.

#### Coordination and create saga

- `worktreePath = None` delegates path allocation to the runtime-managed root.
  `worktreePath = Some(path)` is authoritative, but it must be bounded,
  control-free, absolute, absent on disk, outside the repository and every
  existing Workspace, and not already owned by a managed Worktree. Canonical
  missing-tail identity is used for these checks; UI preview validation never
  replaces them.
- Worktree display/name input is bounded to 128 bytes. The shared
  `managed_worktree_name_slug` produces a separator-normalized path/ref
  component; UI and coordinator must not maintain divergent slug functions.
- Reserve the durable intent before Git or filesystem side effects. The
  idempotency key is unique and bound to a request fingerprint; reuse with a
  different request returns `worktree_operation_idempotency_conflict`.
- Request fingerprints are persisted across upgrades. An additive optional
  request field must preserve the legacy serialized shape when absent (for
  example, `worktreePath = None` uses `skip_serializing_if`); a present value
  participates in the fingerprint. Do not turn a legacy retry into an
  idempotency conflict by serializing a newly added `null` field.
- Discarding a managed Worktree may explicitly request `deleteBranch`. The
  coordinator removes the registered Worktree with the requested force mode
  first, then deletes its local branch while holding the same Git mutation
  guard. The default `false` value is omitted from serialized requests so old
  clients retain their request shape and branch deletion remains opt-in.
- Acquire all repository/path/workspace-index keys atomically after sorting by
  `(kind, key)`. Never hold a partial multi-key claim while waiting.
- If a merge/discard intent is reserved but the in-process lifecycle lock is
  already held, finish that no-side-effect attempt as retryable `Failed` before
  returning busy. Do not leave a `Pending` row that blocks its own next
  preflight.
- The create checkpoint order is:

```text
IntentRecorded -> LocksAcquired -> GitAddStarted -> GitAdded
  -> WorkspacePersisted -> ManagedRecordPersisted -> DatabaseCommitted
  -> Completed
```

- Recovery may reuse only an exact branch/registration whose branch and head
  match the durable intent. An existing unregistered directory or mismatched
  branch/head is `NeedsAttention`; never adopt or delete it automatically.
- A successful retry or startup reconciliation produces exactly one operation,
  one `VibexWorktree` Workspace, and one managed record.

#### Destructive preflight and reconciliation

- Merge/discard execution requires the preflight revision and observed heads.
  Re-read Git and durable ownership after locks are acquired; stale revisions,
  moved heads, dirty blocking state, active operations, or ownership mismatch
  must block execution.
- The coordinator's ordered lifecycle lock is not the final mutation fence.
  While holding the repository common-dir file lock, `crates/git` must again
  verify source HEAD, target branch, target HEAD, and target dirty state before
  merge, or the registered source HEAD before remove. Do not split this final
  check from the Git side effect.
- Rebase conflict resolution is owned by the source Workspace; no-ff merge
  conflict resolution is owned by the target Workspace. Rebase continue and
  abort must verify the durable source branch/original HEAD and target
  branch/HEAD. Abort restores the exact clean source and leaves the target
  unchanged.
- Rebase completion is checkpointed before target fast-forward. Startup may
  finish a recorded exact rebased HEAD when the target still equals the fixed
  preflight HEAD, or recognize an already completed exact fast-forward. A
  rewritten source without a recorded rebased HEAD is uncertain and becomes
  `NeedsAttention`; recovery must not guess from topology alone.
- Startup reconciliation joins operation checkpoints, managed rows, Workspace
  rows, `git worktree list --porcelain`, branch heads, and filesystem presence.
- Proven exact state may be completed or resumed. Missing directories, stale or
  mismatched registrations, legacy records without fixed targets, and unowned
  directories produce typed diagnostics. Uncertain state is preserved: no
  reset, forced removal, branch deletion, or directory deletion.
- Reconciliation is durable-state idempotent. Running it twice without external
  changes must not create rows, directories, or changing diagnostics.

#### Backend and remote boundary

- Native capability exposes `GitWorktreeRead` and `GitWorktreeCreate` and
  delegates to the same runtime coordinator.
- Remote capability may expose `GitWorktreeRead` only when the server negotiates
  `git_worktree_read`. `RemoteWorkbenchRuntime` reads through an injected
  `RemoteWorktreeSnapshotSource` backed by the desktop authority.
- Before serialization, remote eligibility/lifecycle reads replace every local
  display/canonical repository and Worktree path with an opaque ID-based
  projection while preserving non-sensitive presence/`exists` semantics. They
  remove operation idempotency keys, request fingerprints, path identities,
  lock/lease/queue keys and preflight revisions. Readiness check commands become
  `recorded-check`; outcome and timestamp remain available.
- `WebRemoteBackend::git_worktree_create` always fails with
  `remote_worktree_mutation_unsupported`; mobile never executes local Git
  mutation.
- No environment keys configure these contracts.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Path is missing, not a directory, not a worktree, bare, or unborn | `GitProjectEligibility::Ineligible` with the corresponding stable reason. |
| Eligibility revision changed before create | `worktree_eligibility_stale`. |
| Worktree name is empty, unbounded, or contains controls | `worktree_name_invalid`; do not reserve or touch Git. |
| Custom path is empty, unbounded, contains controls, or is relative | `worktree_path_invalid` / `worktree_path_not_absolute`; do not reserve or touch Git. |
| Custom path already exists or has a managed owner | `worktree_path_exists` / `worktree_path_owned`; preserve the existing path. |
| Custom path resolves inside the repository or any existing Workspace | `worktree_path_inside_repository` / `worktree_path_inside_workspace`; do not create directories or Git registrations. |
| Base/target ref does not resolve to a commit | `git_ref_not_found` or the typed missing-target/base error; no intent-side effect beyond a safely retryable reservation. |
| Idempotency key is reused for another fingerprint | `worktree_operation_idempotency_conflict`; do not insert a second operation. |
| Lifecycle key or durable lease is held | `worktree_lifecycle_busy` or `worktree_operation_busy`; do not partially acquire keys, and do not leave a no-side-effect local-lock attempt active. |
| Recovery finds an exact Git registration and exact branch/head | Resume from durable facts and converge to one result. |
| Recovery finds an unregistered directory or identity mismatch | `NeedsAttention` with bounded diagnostic; preserve the directory and branch. |
| Managed directory is missing or registration is stale/mismatched | `GitWorktreeReconciliationState::NeedsAttention`; never clean up automatically. |
| Merge/discard confirmation omits revision/head or facts changed | `worktree_preflight_required` / required-head error / `worktree_preflight_stale`. |
| Source/target facts move before the common-dir mutation lock is held | `worktree_source_head_changed`, `worktree_target_branch_changed`, or `worktree_target_head_changed`; do not merge/remove. |
| Preflight contains blocking risks | `worktree_preflight_blocked`; UI confirmation cannot bypass it. |
| Unknown lifecycle/status/checkpoint/lock value or detail schema is decoded | Preserve it for diagnostics, map enum values to `Unknown`, and fail closed as `NeedsAttention`. |
| Remote client requests create | `remote_worktree_mutation_unsupported`, even if read capability is available. |
| Remote read source is not installed | `remote_worktree_read_unavailable`. |
| Remote lifecycle serialization contains a local path, check command, idempotency/fingerprint, lease, lock, queue key, or preflight revision | Fail the redaction regression; never send the unsanitized snapshot. |

Diagnostics contain stable code, bounded safe summary, severity, retryability,
optional recovery action and IDs. Do not include auth tokens, environment
values, file contents, or unbounded Git output.

### 5. Good / Base / Bad Cases

- Good: two concurrent create calls with one idempotency key share one durable
  operation; one caller may observe busy, and retry returns the same completed
  result.
- Good: the process stops after `GitAdded`; a fresh coordinator verifies the
  exact registration and finishes the Workspace/managed rows once.
- Good: source and target heads match the preflight revision, so Merge Back uses
  the stored target Workspace and branch.
- Good: the caller supplies an absent absolute path outside all known
  Workspaces; the returned `WorkspaceRecord.rootPath` is that authoritative
  location, its canonical identity owns the managed record, and retry returns
  the same record.
- Base: a normal `CurrentCheckout` Session remains unchanged and Worktree locks
  do not affect multiple Agent sessions sharing any Workspace.
- Bad: compare raw path strings, infer repository identity from `.git` being a
  directory, or create a separate remote Worktree authority.
- Bad: infer Merge Back target from the target checkout's current branch at
  execution time.
- Bad: resolve uncertain recovery by `reset --hard`, forced worktree removal,
  deleting a branch, or deleting an unowned storage directory.

### 6. Tests Required

- Core serde tests assert fixed origin/base/target round trips, legacy operation
  JSON defaults, absent additive fields preserve the legacy request shape, and
  unknown enum values fail closed.
- SQLite migration tests start at v32, apply v33 additively, and assert legacy
  rows, identity uniqueness, idempotent reservation, conditional lease claim,
  expired-lease takeover, archive identity retention, and Workspace FK detach
  behavior.
- Git tests cover nested and linked worktrees, bare/unborn rejection, full ref
  resolution, Windows case/separators, symlink aliases, macOS `/var` aliases,
  recoverable add exact branch/head verification, and merge/remove head
  revalidation while the common-dir mutation lock is held.
- Git and coordinator tests cover no-ff and rebase-and-merge selection, linear
  target history, source-owned rebase conflicts, continue/abort, stale target
  rejection, and recovery between rebase completion and target fast-forward.
- Coordinator tests inject every create checkpoint, assert explicit retry
  uniqueness, simulate process loss and restart a fresh coordinator after every
  checkpoint, and run reconciliation twice. An unknown durable checkpoint must
  produce no filesystem or managed-row side effect.
- Concurrency tests assert deterministic atomic multi-key locking and duplicate
  create behavior, create/discard convergence, and two source merges serialized
  against one target. Lifecycle tests assert fixed-target merge, stale preflight
  rejection, and explicit force for dirty discard.
- Reconciliation tests assert missing and unowned directories become diagnostics
  without deletion.
- Create-path tests cover managed default allocation, an authoritative custom
  path, relative/control/oversize input, existing paths, repository nesting,
  nesting under another Project's Workspace, and managed identity conflicts.
- Backend/remote tests assert native read/create, negotiated remote read only,
  stable request tags, auth redaction, injected-authority HTTP routing, opaque
  path identity, preserved non-sensitive existence/outcome facts, and absence of
  private operation/check-command sentinels in serialized snapshots.
- Run focused crate tests, `cargo check -p vibex-remote-client
  --target wasm32-unknown-unknown --locked`, `pnpm check:frontend`, and the full
  `pnpm check:rust` gate with a clean Python environment when the host injects
  incompatible `PYTHONHOME`/`PYTHONPATH` values.

### 7. Wrong vs Correct

#### Wrong

```rust
if stored_path == porcelain_path {
    git_merge(current_target_branch);
}

if recovery_is_uncertain {
    git_worktree_remove_force(path);
}
```

#### Correct

```rust
if vibex_git::same_path_identity(&stored_path, &porcelain_path) {
    let preflight = coordinator.merge_preflight(&request)?;
    let confirmed = GitWorktreeMergeRequest {
        expected_source_head: preflight.source_head.clone(),
        expected_target_head: preflight.target_head.clone(),
        preflight_revision: Some(preflight.revision),
        ..request
    };
    coordinator.merge(&confirmed)?;
}

if recovery_is_uncertain {
    persist_needs_attention_diagnostic();
    // Preserve Git metadata, branch, and filesystem state for inspection.
}

let request = GitWorktreeCreateRequest {
    worktree_path: custom_absolute_path,
    ..request
};
let result = backend.git_worktree_create(MutationRequest::new(request)).await?;
use_authoritative_workspace(result.workspace);
```

## Scenario: Merge Recovery And Managed Directory Lifecycle

### 1. Scope / Trigger

- Trigger: changing Worktree readiness, merge planning/execution, target queueing,
  conflict recovery, Agent assistance, Archive/Restore/Discard, or the lifecycle
  snapshot consumed by Desktop and remote clients.
- `DesktopRuntime::WorktreeCoordinator` remains the only cross-Git/SQLite
  authority. `crates/git` owns Git facts and commands, while `crates/db` stores
  readiness and operation checkpoints. UI state never completes an operation.
- Agent sessions, Terminals, Files, and ordinary reads are not lifecycle lock
  consumers. Running Agent/Terminal counts are warning facts, not admission
  control, and no lifecycle command silently stops them.

### 2. Signatures

```text
GitBackend
  git_worktree_snapshot(workspaceId) -> GitWorktreeLifecycleSnapshot
  git_worktree_readiness(workspaceId) -> GitWorktreeReadinessRecord?
  git_worktree_set_readiness(MutationRequest<GitWorktreeReadinessRequest>)
    -> GitWorktreeReadinessRecord
  git_worktree_merge_plan(GitWorktreeMergeRequest) -> GitWorktreeMergePlan
  git_worktree_merge(MutationRequest<GitWorktreeMergeRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_resolve_conflict(MutationRequest<GitWorktreeConflictResolveRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_stage_conflicts(MutationRequest<GitWorktreeConflictStageRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_bind_assistance_session(
    MutationRequest<GitWorktreeAssistanceSessionRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_continue_merge(MutationRequest<GitWorktreeOperationRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_abort_merge(MutationRequest<GitWorktreeOperationRequest>)
    -> GitWorktreeOperationRecord
  git_worktree_{archive,restore,discard}_preflight(request)
    -> GitWorktreeDestructivePreflight
  git_worktree_{archive,restore,discard}(MutationRequest<request>)
    -> GitWorktreeOperationRecord

GitWorktreeReadinessRecord {
  worktreeId, workspaceId,
  state: working | reviewing | ready_to_merge | merge_queued | merge_running,
  sourceHead, dirtyFingerprint, targetWorkspaceId, targetBranch,
  checks[], revision, updatedAtMs
}

GitWorktreeMergePlan {
  planId, sourceWorkspaceId, sourcePath, sourceBranch, sourceHead,
  targetWorkspaceId, targetPath, targetBranch, targetHead,
  summary, readiness, runningConsumers, preflight
}

GitWorktreeOperationDetail {
  expectedSourceHead, expectedTargetHead, targetBranch,
  mergeStrategy, queueKey, queuePosition, conflicts[],
  sourceCommitsAfterStart, assistanceSessionId, checkpoint, diagnostic
}
```

SQLite schema version 34 adds one table without rewriting v33 rows:

```text
git_worktree_readiness(
  worktree_id PK -> git_managed_worktrees ON DELETE CASCADE,
  workspace_id -> workspaces ON DELETE CASCADE,
  state, source_head, dirty_fingerprint,
  target_workspace_id, target_branch, checks_json, revision, updated_at_ms
)
```

### 3. Contracts

#### Readiness and immutable merge plans

- Readiness belongs to one managed Worktree. Marking `ready_to_merge` requires a
  clean source and exact `sourceHead` plus `dirtyFingerprint`. Any later HEAD or
  dirty-fingerprint change durably returns it to `working`; optional check
  records are bounded user/Agent-reported facts, never commands Vibex claims to
  have run.
- A merge plan uses the managed record's fixed target Workspace/branch and exact
  source/target heads. It includes typed blocking risks, non-blocking unpushed and
  running-consumer warnings, a concrete action label, and a revision derived from
  all executable facts.
- Confirmation supplies the plan's source head, target head, and preflight
  revision. The coordinator reacquires ordered lifecycle locks and the Git
  common-directory mutation lock, then reloads ownership, branch, heads, dirty
  state, readiness, active Git operations, and risks. Changed facts require a
  new plan; an old confirmation never executes against a newer target.
- Merges targeting one Workspace serialize through the durable target queue.
  Queue position is visible, but reaching the front is not approval: the queued
  operation must still match the confirmed source/target facts before executing
  `git merge --no-ff <exact-source-head>`.

#### Conflict, assistance, and finalization

- A verified conflict records `NeedsResolution`, `ConflictDetected`, exact
  source/target heads, typed conflict files, and the target Workspace. It is not
  flattened to `Failed`. Source commits made after merge start are counted but
  never enter this merge scene.
- Conflict resolution requests name the operation, target Workspace, path, and
  `target` or `source` version. Controlled stage accepts only paths owned by that
  operation. Completion is based on the unmerged index, not conflict-marker text.
- While a target operation is live, runtime lifecycle calls plus generic
  checkout, commit, and revert are rejected by
  `worktree_target_operation_fenced`. Reads, edits, controlled stage, Terminal,
  and Agent sessions remain available. This fence does not pretend to intercept
  arbitrary commands typed in a Terminal.
- Agent assistance binds one active, non-archived Session in the target
  Workspace to `assistanceSessionId` in durable operation detail. Reuse validates
  the same operation/Workspace/Session fence. The injected context is bounded,
  uses fixed heads and typed conflict paths, and states that only the user may
  continue or abort. Other Sessions remain unrestricted.
- Continue revalidates operation identity, branch, `MERGE_HEAD`, expected heads,
  and an empty unmerged index before making the merge commit. Abort runs
  `git merge --abort` and proves the target returned to `head_before`. Any
  unprovable result remains `NeedsAttention`; there is no reset fallback.

#### Archive, Restore, Discard, and reconciliation

- Archive, Restore, Discard, and Merge Back are separate operation kinds with
  separate preflights. Archive rejects dirty content and preserves Workspace,
  branch, managed record, and Session history. Restore must reuse the same
  Workspace ID, canonical path, branch, and recorded head. Discard removes the
  directory/registration but does not delete the branch.
- Dirty Discard is permitted only through the explicit `force` request after its
  risk was shown. Active Git operations, ownership mismatch, occupied restore
  paths, stale revisions, and inconsistent registrations remain blocking.
  Unpushed commits and running consumers remain visible non-blocking risks.
- Startup reconciliation joins durable operation/checkpoint data with
  `MERGE_HEAD`, unmerged entries, current heads/branch, managed records, Git
  registrations, and directory presence. It may prove Completed, Aborted, or
  NeedsResolution. Inconsistent or external merge scenes become NeedsAttention
  and are preserved; repeated reconciliation is idempotent.
- Native advertises `GitWorktreeLifecycleMutate`. A negotiated remote client may
  advertise `GitWorktreeRead`, but every lifecycle mutation returns
  `remote_worktree_mutation_unsupported` and cannot synthesize a local plan.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Ready request observes dirty source or stale head/fingerprint | `worktree_readiness_dirty_source` or `worktree_readiness_stale`; persist/return `working`. |
| Merge source is unmanaged, cross-repository, same checkout, or redirected to another target | `worktree_not_managed`, ownership risk, or `worktree_target_override_rejected`; no Git side effect. |
| Source/target head, branch, dirty state, readiness, operation, or warnings changed after confirmation | `worktree_preflight_stale` or `worktree_merge_queue_confirmation_stale`; require a fresh plan. |
| `git merge --no-ff` produces a matching live conflict scene | Persist `NeedsResolution` plus typed conflicts; keep the target fence. |
| Generic checkout/commit/revert targets a live conflict Workspace | `worktree_target_operation_fenced`; preserve file, index, and `MERGE_HEAD`. |
| Conflict request has the wrong operation, Workspace, path, or Git scene | `worktree_merge_resolution_fence_mismatch` / `worktree_merge_resolution_scene_changed`; preserve the scene. |
| Assistance Session is missing, archived/deleted, in another Workspace, or already bound elsewhere | Stable `worktree_assistance_session_*`; do not replace the durable association. |
| Continue sees a mismatched `MERGE_HEAD` or unmerged entries | Keep NeedsResolution/NeedsAttention; do not commit. |
| Abort cannot prove exact `head_before` and cleared Git markers | `worktree_merge_abort_needs_attention`; never reset. |
| Archive is dirty, Restore path is occupied, or lifecycle facts changed | Blocking preflight / `worktree_preflight_stale`; keep the managed record and files. |
| Remote client calls any lifecycle mutation | `remote_worktree_mutation_unsupported`; no RPC/local Git mutation. |

### 5. Good / Base / Bad Cases

- Good: two ready sources queue for `main`; the first merge advances target
  HEAD, so the second receives a refreshed plan instead of executing its old
  confirmation.
- Good: a conflict survives restart, an associated target Session is restored,
  source gains another commit, and continue merges only the originally pinned
  source head.
- Good: Archive removes a clean directory and Restore recreates the same path,
  branch, Workspace ID, and Session history owner.
- Base: running Agent sessions and Terminals appear as warnings and keep running
  during merge/conflict; a normal checkout with no target operation has no
  managed lifecycle state.
- Bad: infer target from the currently checked-out branch, trust a UI pending
  flag as serialization, stage conflict paths through the ordinary Changes
  selection, or resolve uncertain recovery with force/reset/branch deletion.

### 6. Tests Required

- Core serde tests cover readiness, merge strategy/conflict kinds, assistance
  association, legacy operation detail defaults, and unknown enum fail-closed
  behavior.
- SQLite tests migrate through v34 and round-trip readiness revisions/checks,
  operation detail, assistance Session ID, cascade behavior, and legacy rows.
- Git temporary-repository tests cover exact-head no-ff merge, modified/modified,
  added/added, delete/modify, binary conflicts, source/target selection,
  controlled stage, continue, abort, and common-dir lock revalidation.
- Runtime tests cover dirty/stale readiness, fixed-target plan refresh, same-target
  queue serialization, restart reconciliation, external merge preservation,
  source commits after start, assistance binding, generic revert fencing, and
  Archive/Restore identity reuse.
- External-merge reconciliation tests must exercise both lifecycle refresh and
  startup recovery: an exact target/source-parent merge commit is `Completed`, a
  clean return to the recorded target head is `Aborted`, and any other scene
  without `MERGE_HEAD` remains `NeedsAttention`. Marker absence alone is never
  evidence of completion.
- Restore recovery completes only when the registration, branch, path, and
  recorded head all match. An Archived/Discarded record whose directory or Git
  registration reappears, or whose repository cannot be inspected, remains
  closed but becomes `NeedsAttention`; repeated recovery preserves the same
  diagnostic and must not relabel that scene `Consistent` or adopt its new head.
- Backend/remote tests cover every native trait method, disconnected fixtures,
  negotiated read-only lifecycle snapshots, and one stable remote mutation error.
- Run affected package tests and `pnpm check:rust`; changes to Code Workbench
  inputs must also regenerate and validate its evidence through the writer.

### 7. Wrong vs Correct

#### Wrong

```rust
if ui_confirmed {
    git_merge(&target.current_branch, &source.current_head)?;
}

if merge_failed {
    operation.status = Failed;
    git_reset_hard(&target, &head_before)?;
}
```

#### Correct

```rust
let plan = coordinator.merge_plan(&draft)?;
let confirmed = GitWorktreeMergeRequest {
    expected_source_head: Some(plan.source_head.clone()),
    expected_target_head: Some(plan.target_head.clone()),
    preflight_revision: Some(plan.preflight.revision.clone()),
    ..draft
};
let operation = coordinator.merge(&confirmed)?;

if operation.status == GitWorktreeOperationStatus::NeedsResolution {
    preserve_target_scene_and_publish_snapshot(operation);
}
```
