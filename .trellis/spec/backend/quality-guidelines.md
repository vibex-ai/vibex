# Backend Quality Guidelines

Backend quality is measured by whether PC, Web, and mobile can rely on one
local-first service model without provider leakage, data loss, or unsafe config
side effects.

Current evidence: source code, tests, completed Trellis tasks, and
[Architecture Baseline](../guides/architecture-baseline.md) for cross-platform/remote architecture.

> Legacy cutover note (2026-07-29): long Tauri build, command, UI-state import,
> and browser fixture scenarios retained below are pre-cutover evidence. They do
> not define current commands or release gates. Current checks target GPUI
> Desktop, GPUI-WASM Web/mobile, Relay, the Rust workspace, and published-artifact
> rollback.

## Required Design Checks

Before implementing backend work, verify:

- Does this preserve a provider-neutral Agent session API?
- Does this keep the PC runtime authoritative for local files, Git, terminal,
  Agent sessions, Provider config, and timeline?
- Does this avoid default writes to native Claude/Codex user config?
- Does this support reconnect through authoritative fetch plus live stream?
- Does this keep Relay zero-knowledge for business payloads?
- Does this record enough durable state for restart recovery?
- Does this expose capability gaps explicitly instead of hiding them?

## Testing Expectations

Add tests at the service or adapter boundary for:

- Provider event to Vibex timeline mapping.
- Session state transitions.
- Permission request/resolution flow.
- Provider injection plan generation and redaction.
- Config export backup/rollback behavior.
- Timeline sequence catch-up.
- Device permission enforcement.
- Database migrations and restart recovery.

Integration smoke tests should cover:

- Codex simple streaming session and interrupt.
- Claude Code simple streaming session and interrupt.
- Git status and diff.
- PTY create, resize, output, and kill.
- SQLite migration startup.
- Remote client timeline fetch and live event stream.

## Scenario: Shared GPUI Backend Facade, Terminal Reset, And Safe Debug

### 1. Scope / Trigger

- Trigger: adding or changing a shared GPUI Backend trait, `BackendFacade`, the
  native `DesktopRuntime` adapter, or a Terminal frame subscription.
- The shared contract is compiled for native and single-threaded WASM consumers;
  target-specific executor bounds and sensitive diagnostics are part of the API.

### 2. Signatures

```text
BackendFacade {
  agent: Arc<dyn AgentBackend>, workspace: Arc<dyn WorkspaceBackend>,
  file: Arc<dyn FileBackend>, git: Arc<dyn GitBackend>,
  terminal: Arc<dyn TerminalBackend>, management: Arc<dyn ManagementBackend>,
  device: Arc<dyn DeviceBackend>
}

native BackendFuture<T> = Future<Output = BackendResult<T>> + Send
wasm   BackendFuture<T> = Future<Output = BackendResult<T>>

TerminalBackend::subscribe_terminal(terminal_id, next_sequence)
  -> BackendResult<Box<dyn TerminalFrameSubscription>>
TerminalFrameSubscription::next()
  -> BackendFuture<Option<TerminalFrameBatch>>
TerminalFrameBatch {
  terminal_id, frames, next_sequence, dropped_frames, reset_required
}
```

### 3. Contracts

- Keep Agent, Workspace, File, Git, Terminal, Management, and Device as separate
  capability traits. `BackendFacade` aggregates those traits and a capability
  snapshot; it does not become a second giant behavior trait.
- The default `vibex-backend` graph is WASM-safe. Native composition is behind
  the `native` feature and excluded on `target_family = "wasm"`. Native futures and
  trait objects are `Send + Sync`; WASM futures remain local to the browser thread.
- `NativeBackend` delegates to the existing `DesktopRuntime` and domain services.
  It may validate and translate errors/events, but it does not duplicate storage,
  PTY ownership, Git semantics, or authoritative mutation state.
- A Terminal batch sets `reset_required` when the requested sequence is ahead of
  the server's next sequence after a runtime restart, when the first available
  frame is later than requested, or when the dropped-frame counter increases.
  Consumers must invalidate incremental parser/buffer state before applying more
  output; a sequence rewind must not be treated as an empty contiguous poll.
- Serialized Terminal frames retain their bytes for transport. Their `Debug`
  implementations expose only bounded metadata: sequence/byte length for a frame,
  and terminal id/frame count/cursors/drop/reset fields for a batch. Raw output bytes
  must not enter logs, panic reports, snapshots, or evidence through derived `Debug`.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| A shared Backend unconditionally requires Tokio, native I/O, or `Send + Sync` on WASM | `wasm32-unknown-unknown` check or graph gate fails. |
| Native adapter reimplements a domain mutation instead of delegating to `DesktopRuntime` | Reject in dependency/data-flow review. |
| Requested Terminal sequence is greater than the server's next sequence | Return the next non-empty batch with `reset_required = true`. |
| First available sequence skips the requested cursor, or dropped count increases | Return `reset_required = true`; never silently append across the gap. |
| Cursor is contiguous and drop count did not increase | Return `reset_required = false`. |
| Formatting a frame or batch with `Debug` reveals Terminal bytes | Redaction regression fails. |

### 5. Good / Base / Bad Cases

- Good: a server restart rewinds `next_sequence` from 9 to 4; the next batch carries
  `reset_required = true`, so the controller discards its stale incremental state.
- Base: cursor 4 receives frame 4 with an unchanged dropped count and no reset.
- Bad: decide reset only from buffer eviction, derive `Debug` on raw byte payloads,
  or add one Backend trait that owns all seven domains.

### 6. Tests Required

- `cargo test -p vibex-backend --locked` asserts safe frame `Debug` output.
- `cargo test -p vibex-backend --features native --locked` covers eviction,
  dropped-frame growth, normal continuity, and server sequence rewind.
- `cargo check -p vibex-ui --target wasm32-unknown-unknown --locked` and
  `pnpm check:graph` prove the shared default graph remains WASM-isolated.
- `pnpm check:rust` remains the full workspace fmt/check/Clippy/test gate.

### 7. Wrong vs Correct

#### Wrong

```rust
#[derive(Debug)]
struct TerminalFrame { sequence: i64, bytes: Vec<u8> }

let reset = first_sequence.is_some_and(|first| first > requested_sequence)
    || dropped_frames > previous_dropped_frames;
```

#### Correct

```rust
impl fmt::Debug for TerminalFrame {
    // Emit sequence and byte_len only; never format bytes.
}

let reset = requested_sequence > server_next_sequence
    || first_sequence.is_some_and(|first| first > requested_sequence)
    || dropped_frames > previous_dropped_frames;
```

## Scenario: Git Worktree Path Identity

### 1. Scope / Trigger

- Trigger: Git worktree create/list/remove code compares paths returned by
  native Git commands with paths selected by Vibex.

### 2. Signatures

```text
worktree_add(repo_path, path, request) -> GitWorktreeSummary
worktree_list(workspace_id, repo_path) -> GitWorktreeListResponse
git worktree list --porcelain -> lines beginning with "worktree <path>"
```

### 3. Contracts

- Worktree path equality is filesystem identity, not raw string equality.
- Path comparison must try canonical filesystem paths when both paths exist.
- Fallback comparison may normalize `.` / `..` and separators, but must not
  discard leading `..` from unresolved relative paths.
- This matters on platforms where temp or system paths have aliases, such as
  macOS `/var` and `/private/var`.

### 4. Validation & Error Matrix

- `git worktree add` succeeds but created path is not found in the list ->
  return `Process/worktree_create_missing_after_add` with the requested path in
  redacted diagnostics.
- `git worktree list --porcelain` returns a path alias for the created worktree
  -> match via canonical path and continue successfully.

### 5. Good/Base/Bad Cases

- Good: `/var/.../worktree` and `/private/var/.../worktree` match after
  canonicalization.
- Base: identical absolute paths match without filesystem calls.
- Bad: raw `Path::new(left) == Path::new(right)` is the only comparison.

### 6. Tests Required

- Unit test the create/list/remove flow against a temporary repository.
- Regression test path aliases such as `root/child/..` matching `root`.

### 7. Wrong vs Correct

#### Wrong

```rust
Path::new(left) == Path::new(right)
```

#### Correct

```rust
left_path == right_path || canonical_or_normalized_path_match(left_path, right_path)
```

## Scenario: Git Selected Commit, Push, And Filtered History Contracts

### 1. Scope / Trigger

- Trigger: Desktop or remote clients commit selected Git paths, request
  commit-and-push, push without an explicit target, or filter history by ref and
  author.
- This is a cross-layer contract: `crates/core` owns DTOs, `crates/git` owns Git
  command semantics, Tauri/remote handlers only wire requests, and frontend
  hooks consume generated protocol types.

### 2. Signatures

```text
GitCommitRequest {
  workspaceId,
  message,
  paths: string[],
  amend: boolean,
  pushAfter: boolean
}
GitCommitResult {
  workspaceId,
  shortCommit,
  summary,
  committedAtMs,
  pushResult: GitRemoteActionResult | null
}
GitRemoteActionKind = fetch | push
GitHistoryRequest {
  workspaceId,
  limit,
  beforeCommit,
  refName: string | null,
  author: string | null
}
GitHistoryResponse {
  workspaceId,
  commits,
  hasMore,
  authors: GitHistoryAuthor[]
}
GitHistoryAuthor { name, email }
```

### 3. Contracts

- `paths=[]` keeps staged-only commit behavior. `paths!=[]` commits only those
  selected paths with `git commit --only -- <paths>`.
- Selected untracked paths must be `git add`ed before `git commit --only`.
- Unchecked staged paths must remain staged and must not be included in a
  selected-path commit.
- `amend=true` passes `--amend` through the selected-path commit path.
- `pushAfter=true` runs the same typed push behavior after a successful commit
  and returns the push result.
- Push with explicit `remote` and `branch` runs `git push -u <remote> <branch>`.
- Push without explicit target runs `git push` when an upstream exists; otherwise
  it uses `git push -u origin <current-branch>` when the checkout is not
  detached and `origin` exists.
- History `refName` is a concrete local or remote ref; clients that show a branch
  selector must not send an implicit "all branches" sentinel.
- History `author` filters commits, while `authors` is computed for the selected
  ref independently from the current author filter.

### 4. Validation & Error Matrix

- Empty commit message -> `Validation/empty_commit_message`.
- Empty path list for path-required mutations -> `Validation/git_paths_empty`.
- Absolute or escaping Git path -> `Validation/git_absolute_path_rejected` or
  `Validation/git_path_traversal_rejected`.
- Invalid ref/remote/branch syntax -> `Validation/git_ref_invalid`.
- Missing history ref -> `Validation/git_ref_not_found`.
- Commit with no staged changes and no selected paths -> `Conflict/no_staged_changes`.
- Push target only supplies remote or branch -> `Validation/git_push_target_incomplete`.
- Push without upstream while detached -> `Conflict/git_push_detached_head`.
- Push without upstream and without `origin` -> `Conflict/git_push_no_origin`.
- Git command failure after validation -> `Process/git_command_failed` with
  bounded redacted stderr diagnostics.

### 5. Good/Base/Bad Cases

- Good: selected `a.txt` commits with `git commit --only -- a.txt`; unrelated
  staged `b.txt` remains staged after the commit.
- Good: selected untracked `new.txt` is added first, then committed with
  `--only`.
- Good: no-upstream branch with `origin` automatically establishes upstream via
  `git push -u origin <current-branch>`.
- Base: `paths=[]` still commits currently staged changes for legacy callers.
- Bad: frontend shells out to Git or constructs command strings instead of
  calling typed Git mutations.
- Bad: UI sends `All branches` or an empty sentinel as a history ref; use a real
  current/local/remote branch ref instead.

### 6. Tests Required

- Unit test selected-file commit leaves unrelated staged files staged.
- Unit test selected untracked files are added before commit.
- Unit test amend with selected paths.
- Unit test push with existing upstream and no-upstream `origin` fallback.
- Unit test detached/no-origin push failures assert exact error codes.
- Unit test history ref filtering, author filtering, and `authors` independence
  from the author filter.

### 7. Wrong vs Correct

#### Wrong

```rust
run_git(root, &["commit", "-m", message])?;
run_git(root, &["push", "origin", branch_from_ui])?;
```

#### Correct

```rust
run_git_paths(root, &["add"], &selected_untracked_paths)?;
let mut args = vec!["commit".to_string(), "--only".to_string(), "-m".to_string(), message];
args.push("--".to_string());
args.extend(paths);
run_git_owned(root, &args)?;
remote_action(workspace_id, root, &GitRemoteActionRequest {
    kind: GitRemoteActionKind::Push,
    remote: None,
    branch: None,
    workspace_id,
})?;
```

## Scenario: Phase 9 Crash Recovery And Long-Run State Review

### 1. Scope / Trigger

- Trigger: Phase 9 records release-readiness restart semantics across durable
  local-first subsystems.
- The recovery matrix lives at `docs/operations/recovery-matrix.md` and is the
  bounded evidence artifact for restart behavior and known gaps.

### 2. Contracts

- SQLite/service-owned filesystem state is authoritative after restart. UI
  cache, live WebSocket state, provider handles, PTY handles, and Relay rooms
  are not recovery authorities.
- Runtime-only handles must be recreated through the owning service or the
  durable state must be classified as stale/non-running.
- Durable `running` or `initializing` records are hints that require a
  runtime-owned resume check; they are not proof that work is active.
- A stored terminal with `running` or legacy `stale` status that is missing
  from `TerminalManager` live runtime is an open tab: terminal listing must
  recreate a live PTY with the stored shell, size, title, and cwd, persist it as
  `running`, and return only usable `running` terminal tabs to clients.
- Terminals with `killed` or `exited` status are closed/non-usable records:
  keep them in durable storage for history, but do not return them in terminal
  tab lists.
- Scheduled-task stale-run recovery remains owned by `ScheduledTaskRunner`.
- Relay server rooms and bridge response channels are ephemeral; durable remote
  trust and audit records remain SQLite-owned.
- Recovery evidence must be bounded and redacted. Do not include prompts,
  Agent messages, terminal output, file contents, environment values,
  provider-native payloads, native ids, raw Git diffs, raw logs, secrets, auth
  tokens, pairing codes, or plaintext key material.

### 3. Validation

- Focused restart test:
  `cargo test -p vibex-remote terminal_list_restores_missing_open_terminals`.
- Default release-readiness check remains `pnpm check`; it must not start real
  providers, public Relay, physical mobile flows, or network credentials.

## Scenario: Phase 9 Performance And Scale Baseline

### 1. Scope / Trigger

- Trigger: Phase 9 adds explicit release-readiness performance evidence before
  E2E harness and release packaging work.
- The baseline lives in `crates/diagnostics` because it is an operations
  evidence surface that aggregates DB, filesystem, Git, and terminal service
  behavior without making storage depend on higher-level services.

### 2. Signatures

Command:

```text
pnpm baseline:performance
```

Rust binary:

```text
cargo run -p vibex-diagnostics --bin vibex-performance-baseline
```

Library:

```text
run_performance_baseline() -> VibexResult<PerformanceBaselineResult>
PerformanceBaselineResult {
  schema_version,
  generated_at_ms,
  overall_status,
  checks: Vec<PerformanceBaselineCheck>
}
PerformanceBaselineCheck {
  name,
  status,
  classification,
  fixture_size,
  elapsed_ms,
  output_count,
  limit,
  notes
}
```

Task evidence:

```text
.trellis/tasks/<phase-9-performance-task>/performance-baseline.json
```

### 3. Contracts

- Default baseline fixtures must be disposable local directories/databases under
  `target/stage0` or task-local evidence paths.
- The command must not scan user workspaces and must not start real Claude,
  Codex, OpenCode, ACP, scheduled provider runtimes, public Relay, physical
  mobile, or network credential flows.
- Coverage must include:
  - large timeline fetch on disposable SQLite;
  - capped DB query evidence;
  - generated file tree/search traversal;
  - generated Git status or diff fixture;
  - terminal buffer/ring behavior without serializing output.
- Output is bounded JSON. It may include counts, fixture sizes, elapsed
  milliseconds, limits, and classifications.
- Output must not include prompts, Agent messages, terminal output, file
  contents, env values, provider-native payloads, native ids, raw Git diffs,
  raw logs, secrets, auth tokens, absolute temp paths, or plaintext device key
  material.
- `overall_status=fail` means at least one blocker and the binary exits
  non-zero after printing JSON. Follow-up-only evidence remains a successful
  command so release reviewers can inspect the JSON.

### 4. Validation & Error Matrix

- Fixture root cannot be created or cleaned -> storage error
  `performance_baseline_*`.
- Timeline fetch does not return requested bounded page -> check
  `status=fail`, `classification=blocker`.
- Capped DB query exceeds repository cap -> check `status=fail`,
  `classification=blocker`.
- File traversal/search misses generated fixture minimum or exceeds configured
  limit -> check `status=fail`, `classification=blocker`.
- Git binary unavailable -> check `status=follow_up`,
  `classification=follow_up`; do not fail the whole baseline.
- Git fixture command fails after `git` is available -> check `status=fail`,
  `classification=blocker`.
- Terminal buffer does not drop older chunks when capacity is exceeded -> check
  `status=fail`, `classification=blocker`.

### 5. Good/Base/Bad Cases

- Good: `pnpm baseline:performance` prints JSON with `overallStatus=pass` and
  all checks classified as `acceptable_mvp_limit`.
- Base: Git is unavailable in a constrained environment; the baseline records a
  follow-up Git check and still exits successfully for evidence review.
- Bad: baseline output includes raw terminal lines, file contents, raw Git diff,
  absolute temp paths, prompts, provider payloads, or secrets.
- Bad: baseline is added to `pnpm check` as a hard timing threshold, making the
  default quality gate machine-speed dependent.
- Bad: diagnostics evidence is implemented by making `crates/db` depend on
  filesystem, Git, or terminal service crates.

### 6. Tests Required

- Unit test serialization/redaction bounds for the baseline output:
  - includes expected check names;
  - excludes generated terminal lines;
  - excludes generated file contents;
  - excludes Git fixture contents;
  - excludes temp fixture/database path fragments.
- Full release-readiness validation should run:
  - `pnpm baseline:performance`;
  - `pnpm check`;
  - Trellis task validation;
  - `git diff --check`.

### 7. Wrong vs Correct

#### Wrong

```text
pnpm check -> always run performance timings and fail on a slow laptop
crates/db -> depends on vibex-fs/vibex-git/vibex-terminal for evidence
JSON evidence -> includes terminal output or raw Git diff for debugging
```

#### Correct

```text
pnpm baseline:performance -> explicit release evidence command
crates/diagnostics -> aggregates service crate checks
JSON evidence -> records counts, limits, elapsed_ms, and classifications only
```

## Scenario: Phase 9 E2E Regression Harness

### 1. Scope / Trigger

- Trigger: Phase 9 adds explicit provider-free E2E regression evidence after
  focused smokes, backup/recovery checks, and performance baseline evidence.
- The harness lives in `crates/diagnostics` because it is an operations
  evidence surface that composes Agent, Remote, workspace file, Git, terminal,
  scheduled-task, and import contracts without making those service crates
  depend on each other.

### 2. Signatures

Command:

```text
pnpm e2e:regression
```

Rust binary:

```text
cargo run -p vibex-diagnostics --bin vibex-e2e-regression-harness
```

Library:

```text
run_e2e_regression_harness() -> VibexResult<E2eRegressionHarnessResult>
E2eRegressionHarnessResult {
  schema_version,
  generated_at_ms,
  overall_status,
  checks: Vec<E2eRegressionCheck>
}
E2eRegressionCheck {
  name,
  status,
  classification,
  fixture_size,
  output_count,
  notes
}
```

Task evidence:

```text
.trellis/tasks/<phase-9-e2e-task>/e2e-regression.json
```

### 3. Contracts

- Default E2E fixtures must be disposable local directories/databases under
  `target/stage0`.
- The command must not run as part of `pnpm check`; it is explicit release
  evidence.
- The command must not start real Claude, Codex, OpenCode, ACP, scheduled
  real-provider runtimes, public Relay, physical mobile, browser credential
  flows, or network credential flows.
- Coverage must include:
  - mock Agent create/send/fetch timeline flow;
  - `RemoteDispatcher` authenticated request envelopes and typed response
    decoding;
  - workspace open/list, file tree/read/search/write, generated Git status, and
    terminal create/write/snapshot/list/kill basics;
  - mock scheduled-task runner visibility/status counts;
  - fixture-backed Codex and Claude import preview/import contracts.
- Output is bounded JSON. It may include check names, statuses,
  classifications, fixture sizes, output counts, and high-level notes.
- Output must not include prompts, Agent messages, terminal output, file
  contents, env values, provider-native payloads, native ids, raw Git diffs,
  raw logs, secrets, auth tokens, pairing codes, absolute temp paths, database
  paths, or plaintext key material.
- `overall_status=fail` means at least one blocker and the binary exits
  non-zero after printing JSON. Follow-up-only evidence remains a successful
  command so release reviewers can inspect the JSON.

### 4. Validation & Error Matrix

- Fixture root cannot be created or cleaned -> storage error
  `e2e_regression_*`.
- Remote request payload encode/decode failure -> validation error
  `e2e_remote_payload_*`.
- Remote dispatcher returns an error response without structured error ->
  remote error `e2e_remote_dispatch_failed`.
- Mock Agent timeline does not return appended items or an idle session ->
  check `status=fail`, `classification=blocker`.
- Remote workbench misses workspace/file/terminal basics -> check
  `status=fail`, `classification=blocker`.
- Git binary unavailable -> check `status=follow_up`,
  `classification=follow_up`; do not fail the whole harness.
- Git fixture command fails after `git` is available -> check `status=fail`,
  `classification=blocker`.
- Scheduled mock task does not produce one succeeded run -> check
  `status=fail`, `classification=blocker`.
- Codex or Claude fixture import does not preview/import expected candidates ->
  check `status=fail`, `classification=blocker`.

### 5. Good/Base/Bad Cases

- Good: `pnpm e2e:regression` prints JSON with `overallStatus=pass` and all
  checks classified as `acceptable_mvp_limit`.
- Base: Git is unavailable in a constrained environment; the harness records a
  follow-up Git note and still exits successfully if all other E2E checks pass.
- Bad: harness evidence includes prompt text, file contents, terminal chunks,
  raw Git diff, auth token, pairing code, native thread/session id, or temp/db
  path.
- Bad: E2E regression is added to `pnpm check`, causing the default deterministic
  quality gate to start PTY/Git fixtures on every type-check run.
- Bad: the harness opens HTTP/WebSocket listeners or browser automation when the
  required Web/PWA contract can be covered through `RemoteDispatcher` envelopes
  plus `pnpm check` binding/type validation.

### 6. Tests Required

- Unit test serialization/redaction bounds for E2E output:
  - includes expected check names;
  - excludes generated Agent prompt, scheduled prompt, file content, terminal
    output, auth/pairing field names, native id fields, temp fixture path
    fragments, and database paths.
- Full release-readiness validation should run:
  - `pnpm e2e:regression`;
  - `cargo test -p vibex-diagnostics e2e_regression`;
  - `pnpm check`;
  - Trellis task validation;
  - `git diff --check`.

### 7. Wrong vs Correct

#### Wrong

```text
pnpm check -> always run E2E PTY/Git fixtures
JSON evidence -> includes RemoteAuthProof/authToken for debugging
Remote/Web coverage -> launches a browser or public Relay before packaging work
```

#### Correct

```text
pnpm e2e:regression -> explicit provider-free release evidence command
crates/diagnostics -> aggregates local service/protocol checks
Remote/Web coverage -> RemoteDispatcher envelopes + pnpm check binding/type validation
JSON evidence -> records statuses, counts, fixture sizes, and classifications only
```

## Scenario: Phase 9 Release Packaging Matrix

### 1. Scope / Trigger

- Trigger: Phase 9 adds release/package readiness evidence after diagnostics,
  backup/recovery, performance, and E2E release gates.
- The release packaging matrix lives at
  `docs/operations/release-packaging-matrix.md` and records local self-build
  commands, host-specific package commands, known gaps, auto-update policy, and
  pre-tag evidence requirements.

### 2. Signatures

Command:

```text
pnpm release:build-smoke
```

Smoke steps:

```text
pnpm --filter @vibex/desktop tauri build --debug --no-bundle --ci
pnpm --filter @vibex/mobile validate
cargo check -p vibex-relay-server --bin vibex-relay-server
```

Task evidence:

```text
.trellis/tasks/<phase-9-release-packaging-task>/release-packaging-evidence.json
```

Evidence shape:

```text
{
  schemaVersion,
  generatedAtMs,
  overallStatus,
  runCommand,
  checks: [
    {
      name,
      command,
      status,
      classification,
      surface,
      platform,
      notes
    }
  ],
  platformCoverage,
  followUps
}
```

### 3. Contracts

- `pnpm release:build-smoke` is explicit release evidence and must not be added
  to `pnpm check`.
- The desktop smoke uses Tauri `--debug --no-bundle --ci` so it validates the
  local build path without generating signed, notarized, or host-specific
  bundles.
- `apps/desktop/src-tauri/tauri.conf.json` may enable bundle intent for
  self-builds, but local smoke must remain independent of signing and hosted
  updater credentials.
- Tauri bundle identifiers must be reverse-domain identifiers that do not end
  in `.app`, because `.app` conflicts with the macOS application bundle
  extension.
- Mobile smoke must validate the shared Web/PWA assets through the Capacitor
  shell without generating or committing Android/iOS native projects.
- Relay smoke must validate the `vibex-relay-server` binary target without
  starting a public Relay.
- Auto-update is optional and user-controllable follow-up work. Unsigned local
  self-builds must not contact hosted update services.
- Evidence must be bounded JSON. It may include command names, pass/fail
  status, classifications, surfaces, platforms, and high-level notes.
- Evidence must not include command output, terminal output, raw logs, raw Git
  diffs, env values, signing identities, certificate paths, store-account ids,
  secrets, auth tokens, pairing codes, private keys, provider payloads, prompt
  bodies, generated native projects, or artifact file contents.

### 4. Validation & Error Matrix

- Tauri build path fails -> `desktop_tauri_debug_no_bundle` is
  `status=fail`, `classification=blocker`.
- Web/PWA build fails through mobile validation ->
  `web_pwa_static_build` is `status=fail`, `classification=blocker`.
- Mobile shell typecheck or Web asset build fails -> `mobile_shell_validate` is
  `status=fail`, `classification=blocker`.
- Relay binary check fails -> `relay_server_binary_check` is `status=fail`,
  `classification=blocker`.
- Linux package dependencies, macOS host, Windows host, Android SDK/Xcode,
  public Relay/NAT, or signing credentials unavailable -> record
  `explicit_manual` or `blocked_follow_up`; do not fail deterministic local
  smoke unless the release claims that surface.
- Tauri identifier ends in `.app` -> fix config before recording release
  packaging evidence.
- Evidence includes raw build logs, paths to signing material, provider data, or
  native project contents -> evidence is invalid and must be regenerated.

### 5. Good/Base/Bad Cases

- Good: `pnpm release:build-smoke` passes, records desktop/Web/mobile/Relay
  deterministic checks, and the matrix lists host-specific package commands and
  known gaps.
- Base: Linux `.deb`, `.rpm`, AppImage, macOS, Windows, Android, iOS, Docker,
  and public Relay proof are named as explicit manual evidence until those
  hosts or credentials are intentionally available.
- Bad: release packaging evidence starts a real provider, public Relay,
  physical mobile flow, signing/notarization, hosted update check, or network
  credential flow.
- Bad: Android/iOS generated native projects are committed as part of the local
  release smoke.

### 6. Tests Required

- Full release-readiness validation should run:
  - `pnpm release:build-smoke`;
  - `pnpm check`;
  - Trellis task validation;
  - `git diff --check`.
- Review `docs/operations/release-packaging-matrix.md` for Linux, macOS,
  Windows, Web/PWA, mobile shell, Relay, auto-update, manual evidence, and
  pre-tag evidence coverage.
- Review `release-packaging-evidence.json` to confirm it contains status
  metadata only and no logs, secrets, provider payloads, env values, signing
  paths, raw diffs, or generated native project contents.

### 7. Wrong vs Correct

#### Wrong

```text
pnpm check -> pnpm release:build-smoke
pnpm release:build-smoke -> tauri build --bundles deb,rpm,appimage --no-sign
JSON evidence -> paste full Tauri/Vite/Cargo logs for debugging
```

This mixes default deterministic checks with release packaging smoke, makes
local validation depend on host package dependencies, and leaks oversized or
sensitive evidence.

#### Correct

```text
pnpm release:build-smoke -> tauri build --debug --no-bundle --ci
docs/operations/release-packaging-matrix.md -> host package commands and gaps
JSON evidence -> command names, statuses, classifications, surfaces, notes
```

The release smoke proves the local build paths while the matrix names manual
package, signing, host, mobile, Relay, and auto-update follow-ups explicitly.

## Scenario: Stage 0 Local Integration Checks

### 1. Scope / Trigger

- Trigger: Stage 0 introduces a Cargo workspace, pnpm workspace, SQLite/Git/PTY
  smoke binaries, and explicit ACP Agent smoke commands.
- This is an infra contract because future tasks will inherit the root quality
  commands and smoke entrypoints.

### 2. Signatures

- Default check: `pnpm check`.
- Rust check: `pnpm check:rust`.
- Frontend check: `pnpm check:frontend`.
- Local smoke: `pnpm smoke:db`, `pnpm smoke:git`, `pnpm smoke:pty`.
- Explicit real Agent smoke: `pnpm smoke:codex`, `pnpm smoke:claude`,
  `pnpm smoke:agents`.
- Explicit real Agent session smoke: `pnpm smoke:session:codex`,
  `pnpm smoke:session:claude`, `pnpm smoke:sessions`.
- Explicit real ACP smoke: `pnpm smoke:acp:opencode`.
- Explicit scheduled-task smoke: `pnpm smoke:scheduled:mock`,
  `pnpm smoke:scheduled:codex`, `pnpm smoke:scheduled:claude`, and
  `pnpm smoke:scheduled:acp`.

### 3. Contracts

- `pnpm check` must never start real Codex or Claude Code sessions.
- `pnpm check:rust` formats the explicit Vibex workspace package list before
  checking and testing the workspace.
- The Vibex workspace has no Claude or Codex Rust SDK path dependencies. CI
  must not clone `claude-code-api-rs`, `codex-sdk-rs`, or recreate an `sdk/`
  checkout area.
- `pnpm smoke:db` defaults to `target/stage0/vibex-smoke.db`; `VIBEX_DB_PATH`
  may override the path.
- `pnpm smoke:git` must accept an unborn Git repository where `HEAD` has no
  commit yet.
- `pnpm smoke:claude` may read `VIBEX_CLAUDE_MODEL`; it uses the managed Claude
  ACP Adapter and must not enable a Native SDK fallback.
- Real Codex and Claude smokes must use an explicit disposable workspace outside
  the development root. They must pass the safe path through the typed ACP
  runtime configuration or `CreateAgentSessionRequest.workspaceRoot`.
- The development root is resolved at runtime by
  `vibex_agent::forbidden_agent_smoke_root()`: it defaults to the parent of the
  repository checkout that produced the build and may be overridden with an
  absolute `VIBEX_AGENT_SMOKE_FORBIDDEN_ROOT`. No absolute developer path may be
  hardcoded in source, spec, or evidence.
- Real ACP smokes must also use an explicit disposable workspace outside the
  development root, must resolve the ACP Provider through typed
  `AcpProviderConfig` from the catalog/profile service, and must record redacted
  evidence instead of raw native ACP payloads.
- ACP smoke setup must ensure its managed Adapter process starts outside the
  development root before accepting user work.
- `VIBEX_AGENT_SMOKE_WORKSPACE` may override the disposable workspace only when
  it is an absolute path that resolves outside the development root. Empty,
  relative, equal-to-root, or nested-under-root overrides are validation failures
  before starting a real provider.
- Real Agent smoke JSON should include `workspacePath` so failures can be
  audited without guessing the provider cwd.
- Scheduled-task smoke JSON should include `workspacePath`, `taskId`, `runIds`,
  run `statusCounts`, recovery consistency, and redaction flags. It must not
  include raw prompts, terminal output, env values, provider-native payloads,
  native ids, raw Git diffs, or full provider logs.
- `pnpm smoke:scheduled:mock` may run as deterministic local evidence through
  `ScheduledTaskRunner` and `MockAgentProvider`. Real scheduled provider smoke
  commands are explicit and may return structured `blocked` evidence until
  provider-specific scheduled runtime wiring is implemented.
- `pnpm smoke:acp:opencode` may start or attempt `/usr/bin/opencode acp`, but it
  must stay outside `pnpm check`, must not join `pnpm smoke:agents` by default,
  and may return a `blocked` JSON status when the generic ACP adapter boundary
  returns a structured `process/*`, `provider/*`, or `capability/*` error.
  After a concrete runtime client exists, failures should be more specific than
  the disabled-runtime `capability/acp_runtime_unavailable` placeholder.
- On local OpenCode `1.17.9`, the proven ACP transport is stdio
  newline-delimited JSON-RPC 2.0 through `opencode acp`. A successful smoke
  must exercise `AgentProvider::create_session` and
  `AgentProvider::send_turn`; `opencode serve` is reference material only and
  must not be counted as ACP smoke success.
- OpenCode ACP smoke may include a redacted `session/new` model/config snapshot
  with response keys, config categories, model counts, current model summary,
  and bounded model samples. It must not store the raw `session/new` payload.

### 4. Validation & Error Matrix

- A Claude or Codex Rust SDK path dependency appears in a crate manifest,
  `Cargo.lock`, or CI checkout -> remove it; online execution is ACP-only.
- Git path missing or not a work tree -> typed validation error.
- Git unborn branch -> return branch from `symbolic-ref`, `shortCommit: null`,
  and do not fail.
- Codex or Claude binary missing -> process/discovery error from explicit smoke,
  not from `pnpm check`.
- OpenCode ACP binary missing -> process/discovery error from
  `pnpm smoke:acp:opencode`, not from `pnpm check`.
- Scheduled provider runtime not wired -> `pnpm smoke:scheduled:codex`,
  `pnpm smoke:scheduled:claude`, or `pnpm smoke:scheduled:acp` records
  `status=blocked` with a bounded diagnostic instead of starting a provider
  process implicitly.
- ACP runtime client intentionally disabled -> explicit smoke records
  `status=blocked` plus the structured `capability/acp_runtime_unavailable`
  error; this is evidence, not a deterministic-check failure. Once an explicit
  runtime is selected, startup/handshake failures should use concrete
  `process/*` or `provider/*` codes such as
  `process/acp_opencode_exited_before_handshake`.
- Provider auth/rate-limit/service failure -> explicit smoke may receive
  streaming events but report provider failure in its JSON summary.
- Real Agent smoke workspace under the resolved development root ->
  validation error before provider spawn.

### 5. Good/Base/Bad Cases

- Good: `pnpm check` passes with no network and no Provider auth.
- Base: `pnpm smoke:git` returns branch and dirty state in a new repository with
  no commits.
- Base: `pnpm smoke:acp:opencode` records path/version/config/capability
  evidence and either a completed create-session/send-turn boundary result or a
  structured blocker when no production ACP process client or provider
  handshake is wired.
- Base: `pnpm smoke:acp:opencode` records a bounded OpenCode ACP
  `session/new` model/config snapshot when the local provider returns optional
  config metadata; missing optional fields are recorded as unavailable rather
  than failing an otherwise valid session.
- Base: `pnpm smoke:scheduled:mock` creates a disposable workspace, runs a
  one-shot scheduled task through the mock provider, recovers a stale scheduled
  run, and emits redacted evidence with run ids and status counts.
- Base: `pnpm smoke:scheduled:codex` records blocked evidence when scheduled
  real-provider runtime wiring is unavailable, without launching Codex.
- Bad: CI recreates an `sdk/` checkout or a crate adds a Claude/Codex Native SDK
  path dependency; both restore a removed runtime surface.
- Bad: an ACP smoke starts OpenCode from the Vibex checkout or stores raw ACP
  provider payloads, env values, auth tokens, or terminal logs as task evidence.

### 6. Tests Required

- Unit test id/error/event serialization in `crates/core`.
- Unit or smoke test SQLite open/migrate/sentinel round-trip.
- Unit or smoke test Git status against the current repository and a non-Git
  directory.
- Unit or smoke test PTY spawn/read/resize/terminate with a deterministic marker.
- Compile the managed Codex and Claude ACP smoke binaries in default Rust checks.
- CI must run `cargo metadata --no-deps` or an equivalent Cargo command before
  binding generation without checking out external Rust SDK repositories.
- Run real Agent smoke only through explicit local commands.
- Run scheduled-task smoke only through explicit `pnpm smoke:scheduled:*`
  commands. Default validation may compile the smoke binary but must not run
  real scheduled provider processes.
- `cargo test -p vibex-agent-acp` must assert ACP event mapping, disabled
  runtime structured errors, and smoke evidence redaction helpers.
- `pnpm smoke:acp:opencode` must be run manually only when real OpenCode local
  evidence is required.

### 7. Wrong vs Correct

#### Wrong

```toml
[dependencies]
native-agent-sdk = { path = "../native-agent-sdk" }
```

Native SDK path dependencies restore a removed online runtime surface.

#### Correct

```toml
[dependencies]
vibex-agent-acp = { path = "../agent-acp" }
```

Route online Agent work through Vibex's typed ACP integration instead of adding
a Claude or Codex Rust SDK dependency.

#### Wrong

```rust
CreateAgentSessionRequest {
    workspace_root: ".".to_string(),
    // ...
}
```

This can run a real Codex or Claude Code provider from the Vibex development
checkout when the smoke is invoked from the repository.

#### Correct

```rust
let workspace_path = resolve_agent_smoke_workspace("codex", "session")?;
CreateAgentSessionRequest {
    workspace_root: workspace_path.display().to_string(),
    // ...
}
```

Resolve and print a disposable smoke workspace outside the resolved
development root before any real provider starts.

#### Wrong

```bash
pnpm check && /usr/bin/opencode acp
```

This mixes deterministic validation with a real ACP provider process and does
not prove the Vibex typed ACP config path.

#### Correct

```bash
pnpm smoke:acp:opencode > .trellis/tasks/<task>/opencode-acp-smoke.json
```

Run real OpenCode ACP only through the explicit smoke command, using the typed
catalog/profile config path and a disposable smoke workspace.

## Scenario: Phase 2 PC Workbench Local Services

### 1. Scope / Trigger

- Trigger: Phase 2 adds the first local-project PC workbench vertical slice:
  workspace/project summaries, workspace-safe file operations, Git status/diff/
  stage/unstage/revert/commit, PTY terminal sessions, additive SQLite metadata,
  and typed service contracts.
- This is a cross-layer contract because Rust DTOs, SQLite tables, service
  crates, Tauri commands, frontend hooks, and screenshots all depend on the same
  shape.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
workspace_list_projects() -> Vec<ProjectWorkspaceSummary>
workspace_open_project(OpenWorkspaceRequest) -> ProjectWorkspaceSummary

file_list_tree(FileTreeRequest) -> Vec<FileTreeEntry>
file_read(FileReadRequest) -> FileReadResponse
file_write(FileWriteRequest) -> FileReadResponse
file_delete(FileMutationRequest) -> ()
file_rename(FileMutationRequest) -> FileTreeEntry
file_search(FileSearchRequest) -> Vec<FileSearchResult>

git_status(WorkspaceId) -> GitStatusSummary
git_diff(GitDiffRequest) -> GitDiffResponse
git_stage(GitStageRequest) -> GitStatusSummary
git_unstage(GitStageRequest) -> GitStatusSummary
git_revert(GitStageRequest) -> GitStatusSummary
git_commit(GitCommitRequest) -> GitCommitResult

terminal_list(WorkspaceId) -> Vec<TerminalSession>
terminal_create(TerminalCreateRequest) -> TerminalSession
terminal_snapshot(TerminalId) -> TerminalSnapshot
terminal_write(TerminalWriteRequest) -> ()
terminal_resize(TerminalResizeRequest) -> ()
terminal_kill(TerminalId) -> TerminalSession

TerminalManager::with_raw_observation_capacity(chunk_count, byte_capacity)
TerminalManager::write_bytes(TerminalId, &[u8]) -> ()
TerminalManager::raw_snapshot(TerminalId) -> TerminalRawSnapshot
```

Additive SQLite v3 tables:

```text
terminal_sessions(terminal_id, workspace_id, title, shell, cwd, rows, cols,
  status, created_at_ms, updated_at_ms, closed_at_ms)
workbench_recent_files(workspace_id, path, last_opened_at_ms)
git_snapshots(workspace_id, branch, short_commit, dirty, changed_files,
  captured_at_ms)
```

### 3. Contracts

- `crates/core` remains the source of truth for serializable file, Git,
  terminal, workspace, and workbench DTOs; Rust consumers import those types
  directly through the established service and Backend boundaries.
- `crates/fs` owns root containment. Every relative path must resolve under the
  active workspace root after normalization, even if the frontend already
  confirmed an action.
- `crates/git` uses the Git CLI for this slice and maps command failures into
  `VibexError` categories instead of returning raw stderr to the UI.
- `crates/terminal` stores metadata in SQLite but keeps high-volume output in a
  bounded in-memory buffer exposed through snapshots.
- Raw PTY observation is opt-in feasibility/debug evidence only. It preserves bytes
  without UTF-8 conversion, evicts complete oldest chunks, and never retains a single
  chunk larger than its byte capacity. Oversized chunks increment `dropped_chunks`
  and are not stored; callers must fail any lossless evidence run when that count is
  nonzero.
- Desktop Tauri command handlers stay thin: resolve workspace metadata from the
  DB, delegate to service crates, persist metadata/snapshots, and return Vibex
  DTOs or `VibexError`.

### 4. Validation & Error Matrix

- Workspace id missing from SQLite -> `validation/workspace_not_found`.
- File path escapes the workspace root -> `validation/path_traversal_rejected`.
- File read exceeds the requested or service limit -> typed validation or
  truncated preview metadata.
- Git path is not a repository -> typed validation Git error.
- Git commit message is empty or no staged changes exist -> typed validation or
  conflict Git error.
- Terminal id missing or closed -> typed validation/process terminal error.
- Raw snapshot requested without opt-in ->
  `capability/terminal_raw_observation_disabled`.
- Raw PTY read chunk exceeds the observation byte capacity -> drop the complete chunk,
  increment `dropped_chunks`, and keep `retained_bytes <= capacity_bytes`.
- SQLite migration or repository failure -> `storage/*`.

### 5. Good/Base/Bad Cases

- Good: one local workspace can show Agent, file tree/Monaco, Git diff/actions,
  and terminal tabs at the same time using generated Vibex DTOs.
- Base: browser screenshot mode can use deterministic Tauri mock responses, but
  native Tauri runtime still goes through `__TAURI_INTERNALS__` and real
  commands.
- Bad: UI stores terminal output in SQLite, bypasses backend path containment,
  branches on native provider payloads, or exposes deferred Git history/blame
  actions as if they were implemented.

### 6. Tests Required

- DB migration/repository tests for terminal metadata, recent files, and Git
  snapshots.
- File service tests for traversal rejection, read/write, language detection,
  destructive operation errors, and bounded search.
- Git temp-repo tests for status, diff, stage, unstage, revert, and commit.
- Terminal manager tests for create/write/resize/snapshot/kill and bounded
  output behavior, including non-UTF-8 preservation, whole-chunk eviction, and an
  oversized chunk that never breaches the byte bound.
- `pnpm check`, `pnpm smoke:db`, `pnpm smoke:git`, `pnpm smoke:pty`,
  `pnpm smoke:files`, and desktop build before commit.

### 7. Wrong vs Correct

#### Wrong

```rust
// Tauri command trusts a UI path and writes it directly.
std::fs::write(request.path, request.content)?;
```

#### Correct

```rust
// Service resolves under the workspace root before any file side effect.
WorkspaceFileService::new(&workspace_root, workspace_id)?.write_file(&request)?;
```

## Scenario: Packaged Linux PTY Host Environment

### 1. Scope / Trigger

- Trigger: `TerminalManager` starts an interactive user shell from a Linux
  AppImage process whose launcher injected package-private runtime environment.

### 2. Signatures

```text
TerminalManager::spawn_runtime(cwd, session) -> TerminalSession
sanitize_terminal_environment(&mut portable_pty::CommandBuilder)

AppImage detection: APPDIR = absolute path
Filtered path-list keys: PATH, LD_LIBRARY_PATH, GSETTINGS_SCHEMA_DIR,
  GST_PLUGIN_SYSTEM_PATH, GST_PLUGIN_SYSTEM_PATH_1_0, PERLLIB,
  PYTHONHOME, PYTHONPATH, QT_PLUGIN_PATH, XDG_DATA_DIRS
Removed launcher keys: APPDIR, APPIMAGE, ARGV0, OWD,
  PYTHONDONTWRITEBYTECODE
```

### 3. Contracts

- Sanitize only the PTY child command. Do not mutate the desktop process
  environment because the packaged binary still needs its AppImage libraries
  and resource paths.
- When `APPDIR` is an absolute path, remove path-list entries rooted beneath it
  and preserve every host entry in order. Remove a key when no host entry
  remains.
- Remove AppImage launcher markers and the launcher-owned Python bytecode flag
  from the child. Preserve unrelated user variables exactly.
- Missing or non-absolute `APPDIR` means ordinary development/native launch;
  leave the environment unchanged. Non-Linux implementations are no-ops.
- This boundary prevents host commands from loading the AppImage runtime. In
  particular, linuxdeploy may set `PYTHONHOME=<APPDIR>/usr` without packaging
  Python's standard library, causing shell startup helpers to fail importing
  `encodings`.

### 4. Validation & Error Matrix

- Absolute `APPDIR` plus mixed host/AppImage `PATH` -> retain host entries only.
- `PYTHONHOME` or `PYTHONPATH` contains only AppImage paths -> remove the key.
- Package-only `LD_LIBRARY_PATH` -> remove it from the PTY child.
- Missing/relative `APPDIR` -> no environment rewrite.
- Path-list reconstruction fails -> remove the affected package-contaminated
  key rather than passing an invalid value to the shell.
- Unrelated environment entry -> preserve byte-for-byte.

### 5. Good/Base/Bad Cases

- Good: an AppImage terminal starts the user's fish/bash shell, and a Python
  greeting helper resolves the host Python and imports `encodings` normally.
- Base: `cargo run` has no `APPDIR`; the terminal inherits the developer's
  virtualenv and other ordinary environment unchanged.
- Bad: clear the whole child environment, which loses the user's PATH, locale,
  SSH agent, toolchain, and shell configuration.
- Bad: pass AppImage `PYTHONHOME`, `PYTHONPATH`, or `LD_LIBRARY_PATH` into the
  interactive shell because the desktop process itself needed them.

### 6. Tests Required

- Unit-test a mixed AppImage/host PATH and assert order-preserving host output.
- Assert `PYTHONHOME`, `PYTHONPATH`, launcher markers, and package-only library
  paths are absent after sanitation.
- Assert an unrelated sentinel variable survives unchanged.
- Run the terminal service tests and compile the GPUI desktop target after
  changing this boundary.

### 7. Wrong vs Correct

#### Wrong

```rust
let command = CommandBuilder::new(shell); // inherits APPDIR Python/runtime paths
pair.slave.spawn_command(command)?;
```

#### Correct

```rust
let mut command = CommandBuilder::new(shell);
command.cwd(cwd);
sanitize_terminal_environment(&mut command); // child-only host environment
pair.slave.spawn_command(command)?;
```

## Scenario: Advanced Git And Managed Worktrees

### 1. Scope / Trigger

- Trigger: Phase 2 extends Git from status/diff/stage/commit into history,
  commit detail, blame, branch/remotes/fetch, and Vibex-managed worktree
  create/list/merge/discard.
- This is a cross-layer contract because Rust DTOs, Git CLI parsing, SQLite v4,
  typed Backend adapters, and the Git panel must agree on bounded payload shapes.

### 2. Signatures

Additional Tauri commands:

```text
git_history(GitHistoryRequest) -> GitHistoryResponse
git_commit_detail(GitCommitDetailRequest) -> GitCommitDetail
git_blame(GitBlameRequest) -> GitBlameResponse
git_branch_list(WorkspaceId) -> GitBranchListResponse
git_branch_create(GitBranchCreateRequest) -> GitStatusSummary
git_branch_checkout(GitBranchCheckoutRequest) -> GitStatusSummary
git_remote_action(GitRemoteActionRequest) -> GitRemoteActionResult
git_worktree_list(WorkspaceId) -> GitWorktreeListResponse
git_worktree_create(GitWorktreeCreateRequest) -> GitWorktreeCreateResult
git_worktree_merge(GitWorktreeMergeRequest) -> GitWorktreeOperationRecord
git_worktree_discard(GitWorktreeDiscardRequest) -> GitWorktreeOperationRecord
```

Additive SQLite v4 tables:

```text
git_managed_worktrees(worktree_id, project_id, workspace_id, repo_root,
  worktree_path, branch, base_ref, head, status, created_at_ms,
  updated_at_ms, closed_at_ms)
git_worktree_operations(operation_id, project_id, source_workspace_id,
  target_workspace_id, operation, status, worktree_path, branch, base_ref,
  head_before, head_after, error, created_at_ms, updated_at_ms)
```

### 3. Contracts

- `crates/core/src/git.rs` owns every Git/worktree request and response DTO;
  frontend code consumes those types through the shared Rust Backend contracts.
- `crates/git` is the only layer that shells out to `git`; Tauri handlers
  resolve workspace/project records, persist snapshots or operation metadata,
  and delegate parsing/execution to `vibex_git`.
- History and blame are bounded: history limits clamp to the service maximum,
  blame defaults to a bounded line range, and commit patches reuse diff
  truncation.
- Worktree create/merge/discard must insert a `git_worktree_operations` record
  before filesystem or Git side effects and update it to `running`,
  `completed`, or `failed`.
- Merge/discard operate on Vibex-managed worktrees by default. Unmanaged paths
  are rejected at the Tauri/DB boundary before destructive Git commands run.
- Worktrees are created under `~/.vibex/worktrees/<project-id>/...`, not under
  the active repository or `target/`.

### 4. Validation & Error Matrix

- Workspace id missing -> `validation/workspace_not_found`.
- Commit/ref/branch argument empty, whitespace-containing, or option-like ->
  `validation/git_ref_invalid`.
- Git history on unborn repository -> empty `GitHistoryResponse`, not a thrown
  process error.
- Blame path is absolute or traverses upward -> `validation/git_path_*`.
- Fetch remote/auth/native Git failure -> structured `VibexError` with redacted
  stderr diagnostic, not raw stderr UI rendering.
- Merge target dirty -> `conflict/worktree_merge_dirty_target`.
- Merge/discard path not present in `git_managed_worktrees` ->
  `validation/worktree_not_managed`.
- SQLite operation insert/update failure -> `storage/worktree_operation_*`.

### 5. Good/Base/Bad Cases

- Good: a managed worktree create writes an operation record, runs
  `git worktree add`, creates a `WorkspaceMode::VibexWorktree` workspace under
  the source project, records managed metadata, and returns generated protocol
  types to the UI.
- Base: local bare repository fetch tests exercise fetch behavior without
  external network or credentials.
- Bad: UI passes an arbitrary path to discard and backend runs
  `git worktree remove` without checking `git_managed_worktrees`.

### 6. Tests Required

- Core binding generation and drift check for new Git/worktree DTOs.
- DB migration/repository tests for managed worktree and operation record
  lifecycle.
- Git temp-repo tests for history/detail/blame, branch create/checkout, local
  bare remote fetch, and worktree add/list/remove.
- Tauri/Rust workspace check for command signatures and repository wiring.
- Frontend typecheck/lint for generated protocol imports, query hooks, and Git
  panel tabs.

### 7. Wrong vs Correct

#### Wrong

```rust
// Discards any path supplied by the UI.
vibex_git::worktree_remove(&repo_root, &request)?;
```

#### Correct

```rust
// Reject unmanaged paths before the destructive Git command.
let managed = ManagedWorktreeRepository::get_by_path(&conn, &request.worktree_path)?
    .ok_or_else(|| VibexError::validation("worktree_not_managed", "worktree is not managed by Vibex"))?;
vibex_git::worktree_remove(&managed.repo_root, &request)?;
```

## Compatibility Policy

Claude Code and Codex protocols may change frequently. Adapters need:

- Version detection.
- Capability probing.
- Raw fallback where available.
- Raw event diagnostics.
- Tests around unknown fields and unsupported operations.

Do not make the UI or database schema depend on one provider's latest event
shape.

## Security Review Checklist

- Secrets are stored only in approved secret storage or encrypted records.
- Logs, audit logs, and injection previews are redacted.
- Remote device permissions are enforced server-side.
- Pairing codes expire.
- Relay cannot decrypt business payloads.
- Dangerous terminal, file, Git, and config export operations require explicit
  confirmation when initiated remotely.

## Performance Review Checklist

- Terminal output is throttled and buffered.
- Timeline events are paginated and sequence-based.
- Large file/screenshot transfer is chunked.
- Git status/diff can handle large repositories without blocking UI.
- Provider raw logs are bounded by retention.

## Anti-Patterns

- Do not add backend features by first adding UI-specific endpoints.
- Do not duplicate separate Claude and Codex service stacks above the adapter
  layer.
- Do not assume mobile can hold a full IDE state model.
- Do not treat Relay as required infrastructure for local or LAN usage.

## Scenario: Workspace Feature Unification And Canonical JSON

### 1. Scope / Trigger

- Trigger: adding a workspace member or dependency enables a behavior-changing
  feature on a shared crate, especially `serde_json/preserve_order`.
- Cargo workspace checks may unify features even when the new application does
  not depend on the service whose tests change.

### 2. Signatures

```text
stable_json_text(&serde_json::Value) -> String
pnpm check:rust -> cargo check/clippy/test --workspace --locked
```

### 3. Contracts

- JSON stored inside a protocol string, timeline payload, correlation input, or
  golden fixture must recursively sort object keys before serialization.
- `serde_json::Value::to_string()` is not a canonicalization boundary: its map
  order changes when `preserve_order` is unified into the build.
- A dependency-graph change must run the full workspace test command. Focused
  package tests alone may compile a different feature set and miss the drift.
- Golden files record the established contract. Do not update them when the only
  cause is a newly unified serialization feature.

### 4. Validation & Error Matrix

- Full workspace golden differs only inside nested JSON strings -> restore
  recursive canonicalization at the producer boundary.
- `cargo tree --workspace -e features -i serde_json` shows `preserve_order` ->
  verify every persisted or compared JSON string uses an explicit stable form.
- Structured JSON object field order changes but semantic values are equal -> do
  not treat display order as a protocol change unless the contract says otherwise.
- Canonical serialization fails -> emit the boundary's existing bounded fallback;
  never leak raw provider input while reporting the failure.

### 5. Good/Base/Bad Cases

- Good: ACP event `rawInput` recursively sorts keys, so adding GPUI leaves live,
  transcript, and expected timeline fixtures byte-stable.
- Base: typed DTO serialization keeps its declared struct field order and does
  not need generic object sorting.
- Bad: update a golden fixture from `{"kind":"update","path":"..."}` to
  `{"path":"...","kind":"update"}` after a dependency enables
  `preserve_order`.

### 6. Tests Required

- Unit-test canonical JSON with maps inserted in reverse key order and with a
  nested object.
- Run the affected focused golden test for fast feedback.
- Run `cargo test --workspace --locked` to exercise the unified feature graph.
- Keep binding and parity checks green when canonical JSON crosses those layers.

### 7. Wrong vs Correct

#### Wrong

```rust
let raw_input = value.to_string();
```

#### Correct

```rust
let mut entries = object.iter().collect::<Vec<_>>();
entries.sort_unstable_by_key(|(key, _)| *key);
let canonical = Value::Object(entries.into_iter()
    .map(|(key, value)| (key.clone(), sorted_json_value(value)))
    .collect());
let raw_input = serde_json::to_string(&canonical)?;
```

## Scenario: Desktop Iframe Embed Preflight

### 1. Scope / Trigger

- Trigger: Desktop commands decide whether a right-rail or preview web page may
  be loaded inside a React-managed DOM iframe.

### 2. Signatures

```text
right_rail_iframe_embed_check({ url }) -> {
  status: "supported" | "blocked" | "unknown",
  blockingHeader: string | null,
  blockingValue: string | null,
  finalUrl: string | null
}
```

### 3. Contracts

- Parse and validate the URL with the same http/https-only rules used by
  desktop webview navigation commands.
- Check known platform policy blocks before issuing network requests. If a
  domain is known to freeze WebKitGTK as a child iframe, return `blocked` with
  `blockingHeader = "vibex-iframe-policy"` and a stable `blockingValue`.
- Header-based blocks still come from `X-Frame-Options` and
  `Content-Security-Policy: frame-ancestors`.
- `unknown` is only for network/probe uncertainty, not for known unsafe domains.

### 4. Validation & Error Matrix

- Invalid URL -> command validation error.
- Non-http(s) URL -> command validation error.
- Known WebKitGTK-unstable iframe domain -> `blocked`.
- `X-Frame-Options: DENY|SAMEORIGIN|ALLOW-FROM` -> `blocked`.
- CSP `frame-ancestors` without localhost/tauri allowance -> `blocked`.
- Probe network failure -> `unknown`.

### 5. Good/Base/Bad Cases

- Good: `https://www.baidu.com` returns `blocked` before the frontend creates an
  iframe, so the workbench remains responsive and offers external-open.
- Base: a normal embeddable site with no blocking headers returns `supported`.
- Bad: treating the absence of frame-blocking headers as sufficient proof and
  letting React create an iframe for a domain that freezes WebKitGTK.

### 6. Tests Required

- Unit-test every policy-blocked domain and a substring non-match such as
  `notbaidu.com`.
- Unit-test X-Frame-Options and CSP frame-ancestors parsing.
- Run `cargo test -p vibex-desktop right_rail_iframe_embed` after changes.

### 7. Wrong vs Correct

#### Wrong

```rust
let checked = right_rail_iframe_embed_response(response.headers(), final_url);
return Ok(checked);
```

#### Correct

```rust
if let Some(response) = right_rail_iframe_embed_blocked_by_policy(&url) {
    return Ok(response);
}

let checked = right_rail_iframe_embed_response(response.headers(), final_url);
```

## Scenario: Bounded PDF Controller Load And Render Lifecycle

### 1. Scope / Trigger

- Trigger: GPUI or another local preview opens and renders PDF bytes through
  `PdfDocumentController` and the reviewed PDFium runtime.
- A PDF feasibility spike proves engine viability only. Product-controller behavior
  requires separate evidence through the actual controller API.

### 2. Signatures

```text
PdfDocumentController::activate(generation)
PdfDocumentController::open(&PdfiumEngine, bytes, password, generation)
PdfDocumentController::render_viewport(
  &PdfiumEngine, password, PdfViewportRequest, &PdfCancellationToken
)
PdfDocumentController::close(generation)
read_pdf_source(path) -> Vec<u8>

vibex-desktop --native-content-pdf-controller \
  <pdfium-library> <fixture.pdf> <encrypted-fixture.pdf> \
  <too-many-pages.pdf> <extreme-page.pdf> <oversized-source.pdf> <output.json>
VIBEX_PDF_ENCRYPTED_FIXTURE_PASSWORD=<reviewed-test-password>
pnpm check:pdf-fixtures
pnpm check:pdf-controller
```

### 3. Contracts

- A caller must activate a non-zero generation before `open()`. A mismatched generation
  returns `conflict/pdf_activation_stale` without clearing the current document.
- Once a generation is accepted for a new open, transition to `Loading` and release the
  previous source bytes, metadata, and decoded-page cache before source-size validation
  or a native PDFium call. A failed reload must never leave the old document visible.
- Empty or over-256-MiB source, native load, page-count, and page-metadata failures all
  increment `loadFailures`, clear `documentLoaded`, and transition the lifecycle to
  `Error` with a stable code.
- Filesystem callers use `read_pdf_source()` before PDFium binding. It rejects metadata
  size 0 or greater than 256 MiB without reading the file, then reads through a
  256-MiB+1 limiter and rechecks the resulting length so a file that grows between
  metadata and read cannot cause an unbounded allocation.
- Accept 1-10,000 pages. Viewports require an ordered in-range page span and a target
  width from 64 through 4,096 pixels. Render visible pages plus one page of overscan on
  each bounded side.
- Cache keys include page index and target width. The decoded RGBA cache is bounded by
  both page count and bytes, uses LRU eviction, and never inserts a page larger than the
  byte budget.
- Page metadata dimensions must be finite and positive. Before calling PDFium render,
  estimate `targetWidth * ceil(targetWidth * heightPoints / widthPoints) * 4` with
  checked arithmetic and reject work above the cache byte budget. Do not allocate a
  large native bitmap and discover the budget violation only at cache insertion.
- Check cancellation before native rendering, between pages, and before cache insertion.
  Cancellation returns `conflict/pdf_render_cancelled` and increments only the bounded
  cancellation counter.
- `close()` releases source bytes, metadata, and decoded pages. Diagnostics contain
  engine/version, counts, stable codes, and resource budgets only; never paths, PDF text,
  source bytes, passwords, or rendered content.
- Controller evidence is distinct from `pdf_spike`. The deterministic encrypted fixture
  uses a fixed file ID and reviewed test-only PDF 1.4 Standard Security Handler input;
  its generator and SHA-256 are committed. The test password may enter only the fixture
  contract and the runner's temporary environment. It must not enter CLI arguments,
  controller state, evidence, diagnostics, or logs, and the legacy fixture encryption is
  not a production recommendation.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Generation was not activated or changed | `conflict/pdf_activation_stale`; preserve current state. |
| Empty or source over 256 MiB | `validation/pdf_source_size_invalid`; clear old state and enter `Error`. |
| Source metadata cannot be read | `storage/pdf_source_metadata_failed`; do not bind PDFium. |
| Source grows past 256 MiB after metadata | Limited read returns at most 256 MiB+1, then `validation/pdf_source_size_invalid`. |
| Corrupt PDFium format | `validation/pdf_document_corrupt`; increment `loadFailures`. |
| Password required or incorrect | `capability/pdf_password_required`. |
| Correct reviewed fixture password | Open and render normally without storing the password. |
| Zero pages or more than 10,000 | `capability/pdf_page_count_unsupported`. |
| Metadata lookup fails | `process/pdf_page_metadata_failed`; increment `loadFailures`. |
| Non-finite or non-positive page dimensions | `validation/pdf_page_dimensions_invalid`. |
| Invalid viewport or width | `validation/pdf_viewport_invalid`; do not render. |
| Estimated RGBA exceeds cache bytes | `capability/pdf_page_exceeds_cache_budget` before native render. |
| Pre-cancelled or cancelled render | `conflict/pdf_render_cancelled`; do not insert the pending page. |
| One decoded page exceeds cache bytes | `capability/pdf_page_exceeds_cache_budget`. |
| Close succeeds | lifecycle `Closed`, zero resident pages/bytes, no loaded metadata. |

### 5. Good/Base/Bad Cases

- Good: activate generation 2 while generation 1 content is loaded, open corrupt bytes,
  and observe `Error`, no metadata, zero resident cache, and one additional load failure.
- Good: render page 0 at fit width twice; the second viewport reuses both page 0 and its
  one-page overscan without incrementing native render requests.
- Good: the deterministic encrypted fixture fails with `pdf_password_required` for no
  password and an incorrect password, then opens/renders with the reviewed password;
  both failures clear document/cache state and no report contains the password.
- Good: a sparse 256-MiB+1 source is rejected before content allocation; a deterministic
  10,001-page fixture fails with `pdf_page_count_unsupported`; an extreme page fails
  RGBA preflight with `pdf_page_exceeds_cache_budget` and adds zero native render
  requests.
- Base: scan a 12-page fixture with a three-page cache; resident pages remain at or below
  three and eviction is non-zero.
- Bad: validate or call PDFium first, then clear the previous document only after the new
  load succeeds. A failed reload can expose stale content under the new target.
- Bad: rename an old feasibility report and treat its private cache implementation as
  proof that `PdfDocumentController` honors lifecycle, cancellation, and close contracts.
- Bad: call `fs::read()` first and rely on `open(bytes)` to reject an oversized source
  after the entire file has already been allocated.

### 6. Tests Required

- Unit-test activation, load-state reset, failure accounting, LRU page/byte bounds,
  oversized-page rejection, pre-render byte estimation, cancellation token sharing,
  and diagnostic redaction.
- Run the real controller against the reviewed Linux PDFium library and 12-page fixture.
  Assert metadata, fit/zoom aspect ratio, overscan indexes, cache reuse/eviction,
  pre-cancellation, source-size/corrupt errors, failed-reload clearing, and close release.
- Verify the deterministic encrypted fixture byte-for-byte. Run missing, incorrect, and
  correct password cases through real PDFium; assert exact codes, failure counts, zero
  retained state after failure, successful correct-password rendering, and a repository
  evidence scan that rejects the fixture password.
- Verify the deterministic large fixtures and a temporary sparse oversized source.
  Assert exact error codes, page-count failure cleanup, zero native render requests for
  the extreme page, and zero decoded bytes for every rejected input.
- Bind committed evidence to controller source, lifecycle source, CLI runner, fixture,
  PDFium review, root lockfile, and runtime library SHA-256. Run its negative self-test.
- Recapture the older engine-feasibility and Native Content evidence when their bound
  source trees change; rebuild Linux packages before the final root `pnpm check`.

### 7. Wrong vs Correct

#### Wrong

```rust
let document = engine.load(bytes)?;
self.bytes = Some(bytes);
self.metadata = Some(read_metadata(document)?);
self.cache.clear();
```

#### Correct

```rust
self.lifecycle.begin_load(generation)?;
self.clear_document();
let document = match engine.load(bytes) {
    Ok(document) => document,
    Err(error) => return self.fail_open(generation, error),
};
// Stage bounded metadata, finish lifecycle, then publish the new document state.
```

## Scenario: Supervised Per-Request PDFium Worker

### 1. Scope / Trigger

- Trigger: product GPUI PDF loading or page rendering invokes PDFium against an
  untrusted local document.
- Thread cancellation and generation fencing are insufficient because a native crash
  or non-returning call can terminate or permanently occupy the desktop process.

### 2. Signatures

```text
run_isolated_pdf_request(
  libraryPath, documentPath, generation, pageIndex, targetWidth,
  timeout, &PdfCancellationToken, PdfWorkerFaultMode
) -> IsolatedPdfExecution

vibex-desktop --native-content-pdf-worker-once \
  <pdfium-library> <fixture.pdf> <generation> <page-index> <target-width> \
  <output-directory> <report.json> <none|crash|hang>

vibex-desktop --native-content-pdf-worker-supervisor \
  <pdfium-library> <fixture.pdf> <output.json>

vibex-desktop --native-content-pdf-worker-soak \
  <pdfium-library> <fixture.pdf> <output.json>
```

### 3. Contracts

- Spawn one child process per PDF request. The child alone binds PDFium, owns the
  `PdfDocumentController`, reads the source, decodes pages, and writes bounded RGBA
  files plus a versioned JSON report into a request-specific temporary directory.
- The parent polls without blocking the GPUI foreground. Cancellation or the 15-second
  hard deadline kills and waits for the child before returning. Normal, error, crash,
  timeout, and cancellation paths must reap every successfully spawned child.
- Child output paths must be one normal path component. Validate checked RGBA byte
  dimensions and actual file length before constructing a `PdfPageBitmap`.
- The temporary directory is removed on every parent return path. Child stdout, stderr,
  document paths, PDF text, page pixels, and passwords are not copied into diagnostics
  or evidence.
- After child exit, current controller/native resident items and bytes are zero. A
  separate `lastWorkerResources` projection may report the last child's bounded peak
  cache metrics; it is not current residency.
- The first production boundary deliberately sacrifices controller-cache reuse across
  requests. GPUI may retain only its separately bounded `RenderImage` cache.
- The Linux soak runs 49 requests: 37 normal renders, four pre-cancellations, four
  aborts, and four hard timeouts, with a successful normal request after every fault.
  It samples `/proc` for parent RSS, open FDs, and direct children, and compares
  request-owned temporary directory counts before and after. Parent RSS growth is
  bounded to 64 MiB; current native resources remain zero.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Current executable or temporary directory unavailable | `pdf_worker_executable_unavailable` / `pdf_worker_temp_failed`; no child claimed. |
| Child cannot spawn | `pdf_worker_spawn_failed`; no child claimed or reaped. |
| Cancellation token changes while child runs | Kill and reap; `pdf_render_cancelled`; disposition `cancelled`. |
| Hard deadline expires | Kill and reap; `pdf_worker_timeout`; disposition `timed_out`. |
| Child aborts or exits non-zero | Reap; `pdf_worker_crashed`; disposition `crashed`. |
| Report schema/path/bitmap length is invalid | `pdf_worker_protocol_failed`; temporary files removed. |
| Child returns a typed controller error | Preserve the stable error code; disposition `clean_exit`; current resources zero. |
| Child succeeds | Return metadata/pages, reap child, remove temporary files, and expose only bounded last-worker metrics. |
| Soak ends with child/FD/temp-dir growth | Report `status=failed`; do not publish passing evidence. |
| Parent RSS grows by more than 64 MiB | Report `status=failed`; keep raw paths/content out of the report. |
| Non-Linux host invokes the `/proc` soak | Return an explicit unavailable error; retain source compatibility without a false pass. |

### 5. Good/Base/Bad Cases

- Good: a normal render succeeds, an injected abort becomes `pdf_worker_crashed`, the
  following render succeeds, an injected hang becomes `pdf_worker_timeout`, and the
  next render succeeds; all five children are started and reaped.
- Good: the GPUI surface cancels an obsolete page request, rejects its stale generation,
  and starts only the latest pending page after the old child is reaped.
- Base: opening an encrypted PDF returns `pdf_password_required` from a cleanly exited
  worker with zero current and last-worker resident bytes.
- Good: 49 soak requests start and reap 49 children, recover after all 12 injected
  cancel/crash/timeout faults, retain the initial FD/child/temp-dir counts, and remain
  within the 64 MiB parent RSS growth budget.
- Bad: run PDFium in a background thread and call dropping the future crash containment.
- Bad: report the last child's decoded cache as current desktop-process native memory.

### 6. Tests Required

- Unit-test released resource budgets, unsafe/nested bitmap paths, and checked RGBA size
  overflow.
- Run the real supervisor against the reviewed Linux PDFium library and fixture. Assert
  normal render, crash detection, hard timeout, recovery after both failures, five
  children started, five children reaped, and all privacy flags false.
- Commit source-bound evidence and negative self-tests that reject missed crash,
  missed timeout, unreaped child, failed recovery, or privacy leakage.
- The same evidence includes the real Linux soak and rejects child, FD, temp-directory,
  RSS-budget, current-resource, or privacy drift.
- Surface evidence must assert `resources` is released, `lastWorkerResources` is bounded,
  `workerProcesses.currentProcesses == 0`, and started/reaped counts match.

### 7. Wrong vs Correct

#### Wrong

```rust
cx.background_spawn(async move { controller.render_viewport(&engine, request, &token) });
// Dropping this future cannot contain abort() or a native call that never returns.
```

#### Correct

```rust
let execution = run_isolated_pdf_request(
    &library, &document, generation, page, width,
    PDF_WORKER_TIMEOUT, &token, PdfWorkerFaultMode::None,
);
// Publish only after the child is reaped and the UI generation is still current.
```

## Scenario: Bounded Office ZIP/XML Parsing

### 1. Scope / Trigger

- Trigger: GPUI or another local preview parses DOCX, XLSX, ODS, or PPTX bytes.
- Office archives are untrusted local input even when selected through a native file
  dialog; parsing must be bounded before a model reaches UI state.

### 2. Signatures

```text
OfficeDocumentController::open(path, bytes, generation)
OfficeDocumentController::open_with_control(
  path, bytes, generation, &OfficeCancellationToken, timeout
)

OfficeParserDiagnostics {
  archiveEntries, decodedBytes, rejectedEntries,
  cancelledRequests, timedOutRequests, documentLoaded, resources
}
```

### 3. Contracts

- Reject empty/source-oversized input, excessive entry counts, unsafe archive paths,
  decoded entries over 16 MiB, aggregate decoded bytes over 32 MiB, and compression
  ratios over 100 before XML model construction.
- One shared parse guard checks cancellation and the deadline in archive enumeration,
  XML event loops, slide enumeration, and before/after bounded part reads.
- XML is well formed only when EOF is reached at depth zero. `quick-xml` can emit EOF
  for an unclosed element without returning a reader error, so EOF alone is not
  success.
- Reject DTDs and custom entity references. Numeric and the five predefined XML
  references may be decoded without enabling entity expansion.
- Text budgets are UTF-8 byte budgets. Truncate only at a character boundary; never
  use `chars().take(remaining_bytes)` because multibyte text can exceed the byte cap.
- Any supported-format failure moves the lifecycle to `Error` with a stable code.
  Diagnostics retain counts/codes only and never paths, XML, cell values, paragraphs,
  slide text, or source bytes.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Cancellation token set | `conflict/office_parse_cancelled`; increment cancellation count. |
| Deadline reached | `process/office_parse_timeout`; increment timeout count. |
| EOF with non-zero XML depth | `validation/office_xml_malformed`. |
| DTD present | `capability/office_xml_doctype_unsupported`. |
| Unknown/custom entity | `capability/office_xml_entity_unsupported`. |
| Invalid UTF-8 part | `validation/office_part_encoding_unsupported`. |
| Unsafe path / entry count / size / ratio | The matching bounded `office_archive_*` error. |

### 5. Good/Base/Bad Cases

- Good: a DOCX containing CJK/emoji truncates at or below 256 KiB on a valid UTF-8
  boundary, while cancellation/timeout counters remain content-free.
- Base: XLSX without `sharedStrings.xml` parses inline/numeric cells; if the optional
  part exists but is malformed, the error propagates instead of being silently
  converted to “missing”.
- Bad: call `.ok()` on an optional archive part and swallow malformed XML, cancellation,
  timeout, or encoding failures.
- Bad: accept `<a>text` because the XML reader returned EOF without an error.

### 6. Tests Required

- Normal DOCX, XLSX/ODS 80 x 20, ordered PPTX, and legacy unsupported cases.
- Path traversal, entry-count, oversized entry, compression-ratio, malformed XML,
  invalid encoding, depth, DTD/entity, cancellation, timeout, and source-size cases.
- Multibyte text test asserting total bytes stay within `OFFICE_TEXT_LIMIT` and the
  result remains a valid character boundary.
- Serialize diagnostics after success and failure and scan for fixture content/path
  sentinels.
- Run `cargo test -p vibex-content --locked`, scoped Clippy with repository
  allowances, and root `pnpm check` before commit.

### 7. Wrong vs Correct

#### Wrong

```rust
Event::Eof => break;
output.push(value.chars().take(remaining_bytes).collect());
let shared = read_archive_text(archive, "xl/sharedStrings.xml", stats).ok();
```

#### Correct

```rust
Event::Eof if depth != 0 => {
    return Err(VibexError::validation("office_xml_malformed", "Office XML is malformed"));
}
Event::Eof => break;
let mut end = value.len().min(remaining_bytes);
while end > 0 && !value.is_char_boundary(end) { end -= 1; }
let shared = read_optional_archive_text(archive, "xl/sharedStrings.xml", stats, guard)?;
```

## Scenario: GPUI Release Migration And Reversible Channel Ownership

### 1. Scope / Trigger

- Trigger: the Tauri desktop shell exports its frozen UI preferences to the
  GPUI shell, or a release candidate is promoted across Preview, RC, and stable.
- The bridge touches a Tauri command, the desktop model's versioned file store,
  the shared runtime home lock, packaging, and redacted diagnostics.

### 2. Signatures

```text
desktop_ui_state_import(DesktopUiStateImportRequest) -> DesktopUiStateImportResult
desktop_ui_state_status() -> DesktopUiStateMigrationStatus
UiStateStore::import_tauri(request, now_ms) -> Result<DesktopUiStateImportResult, UiStateError>
UiStateStore::load_read_only() -> Result<UiStateLoad, UiStateError>
ReleaseController::{promote_preview_to_rc, approve_stable_transfer, rollback_to_tauri}
VIBEX_CHANNEL=preview|rc|stable
VIBEX_IMPORT_REQUEST=/absolute/path/to/request.json
node scripts/build-channel.mjs <preview|rc|stable>
pnpm capture:release:linux
pnpm check:release

linux-release-evidence.v1 -> {
  source: { captureParentCommit, cargoLockSha256, sourceInputTreeSha256 },
  scope: { included: [linux_x11, linux_wayland], acceptedDeviation: [macos, windows] },
  artifacts: { preview, rc, stable, retainedTauriExporter },
  performanceAndSoak: { observations, performanceBenefit, streamingSessionSwitch, terminalActivity }
}

approved deviation ids, in order -> [
  ordinary_web_preview_v1,
  right_rail_web_external_open_v1,
  macos_windows_release_deferred
]
```

### 3. Contracts

- The final Tauri exporter enumerates all 25 frozen storage keys, including
  missing values as explicit `null` entries. It never removes legacy
  localStorage or sessionStorage.
- Rust owns schema validation, canonical SHA-256, source shell/version,
  `desktop-ui-state-marker.v1:<checksum>`, section-level fallback, atomic
  replacement, bounded backups, and stale-reference cleanup. An empty set from
  an authoritative repository means no records and therefore removes all stale
  ids.
- GPUI may preload an existing state for its first frame only through
  `load_read_only`, which never renames or writes a file. It must acquire the
  selected home lock through `DesktopRuntime::start` before import, corruption
  quarantine, authoritative SQLite reference lookup, or writer creation. A
  failed/future-schema load keeps the writer disabled.
- `first_import` is idempotent for the same checksum. A changed checksum returns
  `reimport_available` without mutation; only explicit `reimport` may replace an
  already imported state. `reset` writes defaults and preserves a prior backup.
  Older source schema versions may omit newer optional keys; current schema v1
  exports the complete frozen inventory.
- Migration errors cross Tauri/GPUI boundaries as stable category codes only;
  raw paths, schema text, keys, and `error.to_string()` never enter diagnostics.
- Preview and RC use distinct application ids and homes. The channel is embedded
  when a packaged artifact is built, and a runtime environment variable cannot
  change that identity. Stable GPUI uses the stable app id only from an artifact
  explicitly built with the `stable` channel and the `desktop-stable`
  copied home. The Tauri stable home remains a separate rollback source.
- Local package scripts invoke `build-channel.mjs` with the matching
  Preview/RC/Stable argument before the matching `cargo packager` config; do not
  rely on POSIX-only inline env syntax or a previously cached binary. The current
  release validates Linux only and makes no macOS/Windows buildability claim.
- Linux release evidence binds the exact source-input tree, Cargo.lock, retained
  channel binaries, `.deb`/AppImage artifacts, compiled app-id/home probes, and
  retained Tauri rollback binary. Its only platform dispositions are passed Linux
  Wayland/XWayland and explicit `accepted_deviation` for macOS/Windows, with
  `crossPlatformBuildClaim=false`.
- A task-closure waiver for an expensive final recapture never changes the captured
  source hash or converts historical native evidence into a pass. Record the waiver in
  task/candidate/runbook artifacts, keep `linux_release_evidence` failed as stale, and
  require a fresh exact-source capture before Stable promotion.
- Each Linux backend runs at least five startup trials and an exact 300-second
  complete-process-tree observation. The frozen idle memory metric is
  `process_tree_idle_end_rss_kib`: compare the final sample (`endRssKiB`) to the
  budget. Record `maximumRssKiB` for diagnosis only; it cannot replace the end value.
- The Linux release writer refreshes Terminal stress before the aggregate Wayland
  Native Content capture. Because Native Content binds the Terminal evidence by exact
  SHA-256, a successful release capture must leave both evidence files mutually current.
- Before building release artifacts, the writer runs the Native Content prerequisite
  check for the physical Wayland session, capture commands, and pinned `wtype` binary;
  missing capture infrastructure must fail before the long observations and soaks.
- A post-capture recovery is exceptional and requires an explicit user no-repeat
  decision after every release stage has already passed. Record the complete ordered
  stage list, prior exact measurement-series provenance, current artifact identities,
  and the independently recovered Native Content identity; preflight rejects drift in
  any recovery field. Never use this disposition for an early, failed, or short gate.
- A source fix discovered after that complete capture may share the no-repeat decision
  only when the record preserves both the captured and current source-tree identities,
  names the exact affected release input, says that artifacts were not rebuilt, and
  binds focused regression, original-failure repetition, owning workspace suites, and
  scoped Clippy results. This is task-closure evidence, not a physical-observation or
  rebuilt-artifact claim; any subsequent source drift fails closed again.
- Every Preview/RC/Stable `.deb` must preserve the approved PDFium library bytes.
  AppImage may apply only the reviewed linuxdeploy `$ORIGIN` RUNPATH transform while
  preserving ELF Build ID and `NEEDED`. Both formats must include the exact notice,
  manifest, and complete 16-file reviewed license set.
- The release parity audit accounts for 183 baseline commands plus two transition
  commands, for 185 total dispositions.
- `release-parity-audit.v1.acceptedDeviations` and
  `release-evidence.v1.approvedDeviations` contain the same complete, ordered,
  duplicate-free deviation id list. A candidate cannot omit a content-surface
  deviation merely because the workflow audit recorded it elsewhere.
- Stable transfer requires explicit opt-in, an isolated migrated copy, a clear
  home lock, every gate passed or explicitly accepted, and a published rollback
  drill. Rollback toggles ownership back to Tauri without deleting GPUI homes or
  evidence. Preview-to-RC promotion additionally requires passed/accepted
  `ui_state_migration`, `home_lock_isolation`, and `security_and_redaction`
  evidence. Re-transfer after rollback reuses the one GPUI Stable identity
  record rather than appending a duplicate.
- Release diagnostics expose only bounded backend/revision/window/DPI/schema,
  cache-count, clean-shutdown, and crash-count projections. Build-time values
  are length- and marker-bounded before serialization.

### 4. Validation & Error Matrix

- Missing current-schema frozen key -> `validation/desktop_ui_state_invalid`.
- Wrong storage medium, duplicate key, oversize value, or malformed checksum ->
  the corresponding bounded `desktop_ui_state_import_*` validation code.
- Changed checksum in `first_import` -> `reimport_available`, no file mutation.
- Corrupt prior UI-state JSON -> quarantine and bounded default recovery; SQLite,
  Provider secrets, sessions, terminals, and Relay trust remain untouched.
- Preview/RC home lock held -> promotion fails; stable transfer without RC,
  rollback drill, or complete gates fails closed.
- RC promotion with any required pre-RC gate pending/failed ->
  `GateIncomplete`; booleans alone never promote the candidate.
- Packaged/runtime channel mismatch ->
  `release_channel_override_rejected`; runtime-only Stable ->
  `stable_channel_requires_release_build`; unknown channel ->
  `release_channel_invalid`.
- Missing/unknown channel argument to the build wrapper -> exit 2 before Cargo;
  Cargo failure or signal -> nonzero wrapper exit and no packaging step.
- Sensitive/whitespace build metadata -> omit the field rather than exporting it.
- Linux evidence omits either backend, claims cross-platform buildability, or does not
  record both deferred platforms -> reject the evidence.
- Release audit/candidate deviation ids are missing, duplicated, reordered, or differ
  from the approved three-id set -> fail release preflight; do not infer approval from
  a workflow status alone.
- Current source differs from the captured source without the bounded post-capture fix
  record -> keep preflight failed with `release source identity is stale`; never refresh
  only the captured hash or mark the changed tree as physically observed.
- Backend observation is shorter than 300 seconds, lacks its first/final sample, or
  substitutes peak RSS for `process_tree_idle_end_rss_kib` -> reject the performance gate.
- `.deb` PDFium differs from the approved SHA-256, or its notice/manifest/license set
  differs -> reject the package.
- AppImage PDFium changes Build ID/`NEEDED`, uses a RUNPATH other than `$ORIGIN`, or
  omits a reviewed notice/license -> reject the bounded package transform.

### 5. Good/Base/Bad Cases

- Good: a 25-entry request imports once, a repeated request is idempotent, and a
  changed request visibly requires explicit reimport while legacy storage remains.
- Good: GPUI reads the first-frame snapshot without mutation, acquires the home
  lock, performs import/quarantine/reference cleanup, then enables its writer and
  reloads the canonical state.
- Good: the exact Stable `.deb` and AppImage pass compiled probes and PDFium resource
  checks, then Wayland/XWayland each finish 300 seconds with the final RSS below budget.
- Good: parity audit and release candidate both list ordinary Web Preview, right-rail
  Web, and macOS/Windows deviation ids exactly once in the approved order.
- Base: an older request omits optional keys and receives defaults while current
  schema exports all 25 entries.
- Base: macOS/Windows remain explicit accepted deviations with no inferred build,
  install, signing, rollback, or native-runtime result.
- Base: the user waives a final recapture for task closure; retain the old capture as
  historical and leave Stable release approval blocked.
- Base: after every long gate completed, a targeted source fix passes the bounded
  validation matrix under an explicit no-repeat decision; retain both source identities
  and state that the artifacts were not rebuilt.
- Bad: invoke import on every Tauri launch and overwrite a newer GPUI edit, use
  a copied browser id list as authority when SQLite is empty, or launch stable
  GPUI against the Tauri home.
- Bad: open/migrate SQLite or flush UI state before acquiring the home lock, or
  let `VIBEX_CHANNEL=stable` override an already built Preview/RC binary.
- Bad: approve an AppImage because it launches without checking its transformed
  PDFium identity, or use the observation peak in place of the frozen end-RSS metric.
- Bad: mark ordinary Web Preview `accepted_deviation` in the workflow audit but omit it
  from the release candidate's approved-deviation ledger.

### 6. Tests Required

- Model tests cover complete inventory, checksum null/string distinction,
  first/reimport/reset transitions, corrupt quarantine, atomic writes, empty
  authoritative references, preview terminal cleanup, and redacted metadata.
- Runtime tests cover Preview/RC/stable home and app-id isolation, process lock
  contention, required RC gate enforcement, ownership transfer, rollback
  ownership restoration, and idempotent Stable identity reuse.
- Model/GPUI tests cover non-mutating corrupt preload, post-lock state reload,
  packaged/runtime channel mismatch, invalid channel, and build-only Stable.
- Release checker asserts each local package command compiles its matching
  build-time channel before invoking the matching Packager config.
- `pnpm check:release` verifies current source/package identities, exact
  300-second observations, 20 restart cycles, both five-minute soaks, 100 Terminal
  restores, `.deb`/AppImage PDFium contracts, and negative mutations for every gate.
- `pnpm check:release --self-test` rejects a missing or duplicate approved
  deviation and verifies that audit/candidate ledgers remain synchronized.
- The release checker verifies that the writer refreshes aggregate Native Content
  after rewriting Terminal stress and preflights its capture dependencies before
  expensive work; the writer must not leave root `pnpm check` stale.
- A task closed under a generic final-recapture waiver asserts that
  `pnpm check:release` fails only the stale Linux source-identity gate and that the
  stored capture hash is unchanged. A bounded post-capture source-fix disposition must
  additionally reject a stale current-tree hash, altered affected-file set, false
  artifact-rebuild claim, or incomplete targeted validation.
- Tauri/GPUI typechecks, binding drift, release checker, migration smoke, and
  diagnostics smoke must pass. Native package/soak/observation tests remain explicit
  Linux runner evidence and cannot be replaced by a later local rebuild.

### 7. Wrong vs Correct

#### Wrong

```rust
if request.mode == FirstImport {
    store.save(imported_state)?; // overwrites a later GPUI edit on every launch
}
```

#### Correct

```rust
if prior_checksum.is_some() && prior_checksum != Some(candidate_checksum) {
    return Ok(reimport_available_without_mutation());
}
```

#### Wrong

```rust
let references = DesktopRuntime::ui_state_references_for_database(&db_path)?;
store.import_tauri(&request, now_ms)?; // runtime home lock is not held
```

#### Correct

```rust
let runtime = DesktopRuntime::start(config).await?; // owns the home lock
runtime.import_ui_state(&request, now_ms)?;
let state = UiStateStore::new(runtime.ui_state_path()).load_or_default(now_ms)?;
```

#### Wrong

```text
idle RSS gate = maximumRssKiB <= frozen process_tree_idle_end_rss_kib budget
AppImage PDFium accepted because the outer package SHA-256 exists
```

#### Correct

```text
idle RSS gate = endRssKiB <= frozen process_tree_idle_end_rss_kib budget
.deb = exact PDFium SHA-256; AppImage = source/transformed SHA-256 + Build ID + NEEDED + $ORIGIN
```

#### Wrong

```json
{
  "approvedDeviations": [
    { "id": "right_rail_web_external_open_v1" },
    { "id": "macos_windows_release_deferred" }
  ]
}
```

## Scenario: Cross-Platform Release Evidence Gate

### 1. Scope / Trigger

- Trigger: creating or reviewing a release qualification report spanning GPUI-WASM
  Web, Capacitor mobile, Desktop, Direct Remote, or self-hosted Relay.
- The gate is an evidence contract, not a product feature and must not change stable
  defaults or turn missing platform evidence into an accepted pass.

### 2. Signatures

```text
check-cross-platform-release-gate [--write] [--self-test]
cross-platform-release-gate.json -> { candidate, evidenceBindings, gates, decision }
```

Required candidate identity fields are `sourceCommit`, `cargoLockSha256`,
`pnpmLockSha256`, `webSourceTreeSha256`, `mobileShellTreeSha256`, and the Android
artifact `{ path, bytes, sha256, applicationId }`.

### 3. Contracts

- `browserCurrent`, `androidCurrent`, and `mobileCurrent` must bind to the same
  current source/lock/artifact identities before evidence can be described as current.
- Physical Android/iOS status is independent from package/build status. A visible
  boot frame, contract test, emulator, or synthetic input never sets a physical
  scenario to `passed`.
- Gate statuses are `passed`, `blocked`, `failed`, or `accepted_deviation`; unresolved
  P0 accessibility, physical-platform, pairing, data-loss, or core-workflow findings
  force `decision = FAIL` and `releaseEligible = false`.
- Scope may claim Direct LAN/private network and user self-hosted Relay only. The
  official/public Vibex Relay is explicitly excluded from v1 evidence.
- Reports contain bounded metadata and hashes only. Secrets, pairing material,
  prompt bodies, terminal bytes, raw logs, and user workspace content are forbidden.

### 4. Validation & Error Matrix

- Stale source/lock/APK identity -> checker failure; do not reuse the evidence.
- Missing physical scenario, iOS host, NAT/cellular path, or current soak ->
  `blocked`, never `passed`.
- Browser semantic tree unavailable -> P0 accessibility `failed`.
- Report says `PASS` while any gate is `blocked`/`failed` -> checker failure.
- Evidence includes official Relay claim or sensitive fields -> checker failure.

### 5. Good/Base/Bad Cases

- Good: Android APK installs and shows a real Compact frame, while all unperformed
  IME/touch/keyboard scenarios remain `not_tested` and platform remains blocked.
- Base: protocol unit tests and local Relay smoke pass, but LAN/NAT/cellular evidence
  remains a named network blocker.
- Bad: copy a historical Android partial run to a rebuilt APK, call browser canvas
  pixels accessibility evidence, or turn contract tests into physical release proof.

### 6. Tests Required

- Run `node scripts/check-cross-platform-release-gate.mjs --self-test` and assert that
  stale hashes, false PASS mutations, and official Relay claims are rejected.
- Run the Web/mobile evidence checkers, remote/core/db/runtime tests, Relay local smoke,
  backup/diagnostics smoke, and `pnpm release:build-smoke` where disk capacity allows.
- Verify the final report has nine gate entries, a bounded redaction section, and a
  decision consistent with every gate status.

### 7. Wrong vs Correct

#### Wrong

```json
{
  "android": "installed",
  "accessibility": "passed",
  "decision": "PASS"
}
```

#### Correct

```json
{
  "androidPhysicalPassed": false,
  "accessibility": { "status": "failed", "blockers": ["browser_accessibility_adapter_missing"] },
  "decision": "FAIL",
  "releaseEligible": false
}
```

#### Correct

```json
{
  "approvedDeviations": [
    { "id": "ordinary_web_preview_v1" },
    { "id": "right_rail_web_external_open_v1" },
    { "id": "macos_windows_release_deferred" }
  ]
}
```
