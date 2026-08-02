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
  workspaceId, sourcePath, targetWorkspaceId?, expectedSourceHead?,
  expectedTargetHead?, preflightRevision?
}

GitWorktreeDiscardRequest {
  workspaceId, worktreePath, force, expectedHead?, preflightRevision?
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
- Merge must use the stored target. A caller may omit the target or repeat the
  stored target, but may not redirect an existing managed Worktree.
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
- `WebRemoteBackend::git_worktree_create` always fails with
  `remote_worktree_mutation_unsupported`; Web/mobile never execute local Git
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
  stable request tags, auth redaction, and injected-authority HTTP routing.
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
