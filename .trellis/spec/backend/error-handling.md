# Error Handling

Backend errors must be typed, user-actionable at API boundaries, and detailed
enough for diagnostics without leaking secrets. Provider adapters need extra
care because Codex, Claude Code, and ACP protocols can change independently of
Vibex.

Evidence: current `VibexError` contracts, boundary tests, and source-backed specs.

> Legacy cutover note (2026-07-29): Tauri command examples retained later in this
> file are historical boundary evidence. Current product boundaries are the GPUI
> Backend facade and versioned Remote protocol; do not recreate Tauri handlers.

## Error Categories

Model errors around domain categories, not around implementation libraries:

- `Validation`: invalid request, unsupported option, bad path, invalid provider
  profile.
- `Capability`: requested operation is not supported by this provider or device
  permission level.
- `Permission`: denied, expired, revoked device, or unresolved approval.
- `Provider`: native Agent SDK/CLI failure.
- `Process`: binary missing, version unsupported, process exited, timeout.
- `Storage`: SQLite, migration, file IO, keychain, or backup failure.
- `Remote`: pairing, transport, reconnect, sequence, or encryption failure.
- `Conflict`: stale state, concurrent turn, worktree conflict, or Git state
  mismatch.

## API Error Shape

Current Remote and GPUI Backend boundaries return structured errors with:

- Stable error code.
- User-facing message.
- Optional recovery hint.
- Correlation id.
- Redacted diagnostic details.
- Capability data when the failure is due to unsupported provider behavior.

Do not return raw SDK errors directly to the UI.

## Provider Adapter Errors

Adapters should preserve raw error data in redacted diagnostics while mapping to
Vibex error categories. Required behavior:

- Include provider type and detected provider version when known.
- Include native request id or event id when available.
- Distinguish unsupported capability from transient process failure.
- Record raw fallback data in debug logs, not in normal user messages.
- An ACP JSON-RPC error or early process exit whose raw bounded message clearly
  says authentication/login is required maps to
  `provider/provider_authentication_required`, with a sign-in recovery hint,
  instead of the generic `acp_rpc_error` or `acp_process_exited`. Classify the
  original bounded text before redaction removes the discriminating wording;
  diagnostics still store only the redacted form.
- When an ACP process exits before its response, allow one short bounded stderr
  drain before classifying the failure. Absence of an explicit authentication
  signal remains `process/acp_process_exited`; do not infer authentication from
  an empty channel close alone.

## Permission Errors

When a turn is blocked by approval, represent it as `needs_input`, not `error`.
Use `error` only when the provider failed, the request expired, or the session
cannot continue without user action beyond a normal approval.

## Remote Errors

Remote errors must separate authentication/authorization failures from
transport failures. A revoked device, stale pairing code, lost WebSocket, and
missing sequence are different conditions and should not collapse to a generic
"connection failed" response.

## Recovery and Restart

Local-first runtime means crash recovery matters. For side effects that span the
database and external systems, write recovery records before performing the
external action when possible. Examples:

- Native config export backup before write.
- Worktree operation intent before filesystem changes.
- Session start record before provider process spawn.
- Permission resolution record before provider callback.

## Anti-Patterns

- Do not panic for user, provider, filesystem, Git, or network failures.
- Do not convert capability gaps into generic provider errors.
- Do not leak API keys, headers, private file paths, or prompt contents in
  error strings unless the user explicitly requested a diagnostic export.
- Do not ignore native provider unknown fields. Preserve them for diagnostics
  when possible.

## Scenario: Workspace File Create And Copy Mutations

### 1. Scope / Trigger

- Trigger: Desktop file tree actions create folders or paste copied files and
  directories through Tauri commands.

### 2. Signatures

```text
file_create_directory(FileMutationRequest) -> FileTreeEntry
file_copy(FileMutationRequest) -> FileTreeEntry
FileMutationRequest {
  workspace_id,
  path,
  new_path,
  recursive,
  overwrite
}
```

### 3. Contracts

- Resolve all paths through the workspace file service before touching the
  filesystem so relative UI paths cannot escape the workspace root.
- Creating a directory returns a structured conflict if the target already
  exists and `overwrite=false`; if `overwrite=true`, the existing target must
  already be a directory.
- Copying requires `new_path`, rejects source and target equality, rejects
  existing targets unless `overwrite=true`, and requires `recursive=true` when
  the source is a directory.
- Recursive directory copy must reject copying a directory into itself or one
  of its descendants.
- Errors should use stable validation/conflict/storage codes; command handlers
  should delegate validation to the file service rather than reimplementing it.

### 4. Validation & Error Matrix

- Directory target exists with `overwrite=false` ->
  `Conflict/file_create_directory_target_exists`.
- Directory target exists but is a file -> `Validation/file_create_directory_target_is_file`.
- Copy request omits `new_path` -> `Validation/file_copy_target_missing`.
- Copy target equals source -> `Validation/file_copy_target_same_as_source`.
- Copy target exists with `overwrite=false` -> `Conflict/file_copy_target_exists`.
- Copy source is a directory and `recursive=false` ->
  `Validation/file_copy_directory_requires_recursive`.
- Copy directory target is inside source -> `Validation/file_copy_target_inside_source`.
- Filesystem create/copy/read failures -> `Storage/file_*` with redacted path
  diagnostics.

### 5. Good/Base/Bad Cases

- Good: create `docs/`, copy `docs/readme.md` to `docs/copy.md`, then return a
  `FileTreeEntry` for the new target so frontend queries can invalidate and
  reopen the file tree.
- Base: copy `docs/` to `docs-copy/` with `recursive=true`; nested files are
  preserved and the returned entry is the copied directory.
- Bad: command handler constructs filesystem paths directly from UI input and
  bypasses workspace-root validation.
- Bad: recursive copy allows `docs/` -> `docs/nested/`, causing unbounded
  self-copy behavior.

### 6. Tests Required

- Unit test directory creation and file copy through `WorkspaceFileService`.
- Unit test recursive directory copy preserves nested children.
- Regression test copying a directory into itself or a descendant returns
  `file_copy_target_inside_source`.
- Existing path-traversal tests must continue to pass for every new mutation
  that resolves a writable target.

### 7. Wrong vs Correct

#### Wrong

```rust
#[tauri::command]
fn file_copy(request: FileMutationRequest) -> Result<(), std::io::Error> {
    std::fs::copy(request.path, request.new_path.unwrap())?;
    Ok(())
}
```

#### Correct

```rust
#[tauri::command]
fn file_copy(
    state: tauri::State<'_, WorkbenchRuntime>,
    request: FileMutationRequest,
) -> Result<FileTreeEntry, VibexError> {
    let (_conn, service) = file_service_for_workspace(&state.db_path, &request.workspace_id)?;
    service.copy_path(&request)
}
```

## Scenario: Workspace File Open In System Targets

### 1. Scope / Trigger

Desktop file-tree and preview actions open an existing file or directory in the
OS default app, file manager, native terminal or an installed project tool.
Platform openers run on the client; the backend decides whether a workspace path
is local to that client.

### 2. Signatures

```rust
FileBackend::resolve_local_path(WorkspaceId, String)
    -> BackendFuture<Option<PathBuf>>
CodeWorkbench::local_open_path(&mut self, path, cx)
resolve_external_open_path(backend, workspace_id, path, directory)
    -> Result<PathBuf, BackendError>
```

`NativeBackend` delegates to `FileHandle::resolve_existing_path`, which uses
`WorkspaceFileService::resolve_existing_path`. The default backend implementation
returns `None`; it does not expose or reinterpret a remote filesystem path.

### 3. Contracts

- Native resolution validates the workspace and containment before returning
  `Some(path)`. An empty relative path resolves to the workspace root. Files,
  directories and allowed symlink targets retain the file service's semantics.
- Local openers receive the actual file or directory, never a preview-cache
  copy. Editing in another application must change the workspace file; terminals
  must use the selected directory or the selected file's parent.
- A remote file may be materialized through the bounded authoritative byte read
  before opening locally. A remote directory cannot be materialized as a file:
  report `remote_directory_open_unavailable` and offer the workspace terminal.
- Never infer local authority from the existence of a server-supplied path on
  the client's filesystem. Only the backend's validated `Some(path)` grants the
  local path fast path.
- The editor remains unavailable for directories and known binary formats.
  Installed tool discovery controls the project-tool list, and an unknown tool
  id must not fall through to arbitrary shell text.

### 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Native file, folder or empty root path | Validated original path |
| Missing or escaped native path | Existing file-service validation error |
| Remote file | Bounded local materialization |
| Remote directory | `Unsupported/remote_directory_open_unavailable` |
| Missing local external file | `local_file_missing` |
| Unknown or unavailable tool | `file_open_tool_unknown` or `file_open_tool_unavailable` |
| Platform opener fails | Existing `file_open_*` process error |

### 5. Good/Base/Bad Cases

- Good: opening `docs/readme.md` in an IDE edits that workspace file; opening
  `docs` or the empty root path does not attempt a byte read.
- Base: a remote file opens a downloaded copy on this machine.
- Bad: route every Open In action through `materialize_preview_file`, which
  rejects directories and sends local files to a temporary cache.
- Bad: concatenate a workspace root in the UI and bypass backend validation.

### 6. Tests Required

`code_workbench::file_panel_tests` must assert that native files, directories and
root resolve to their original canonical paths, and that missing/traversing
paths fail. Remote directory resolution must fail before any file byte read.
Changes to platform openers must also cover directory/file-parent behavior and
installed-tool checks.

### 7. Wrong vs Correct

```rust
// Wrong: a directory is not a file preview, and a local editor needs the original.
materialize_preview_file(backend, workspace_id, path).await
```

```rust
// Correct: ask the authority for a validated local path before downloading bytes.
if let Some(local) = backend.file()
    .resolve_local_path(workspace_id.clone(), path.to_string()).await?
{
    return Ok(local);
}
```
