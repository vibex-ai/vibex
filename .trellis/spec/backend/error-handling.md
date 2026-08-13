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

- Trigger: Desktop file tree Open In actions launch a selected workspace path
  with the OS default app or native terminal.
- These commands are desktop-only shell integrations, not Vibex PTY terminal
  sessions and not frontend-only absolute path concatenation.

### 2. Signatures

```text
file_open_default_app(FileMutationRequest) -> ()
file_open_native_terminal(FileMutationRequest) -> ()
file_open_tool_list() -> FileOpenTool[]
file_open_with_tool(FileOpenWithToolRequest) -> ()
FileMutationRequest {
  workspace_id,
  path,
  new_path,
  recursive,
  overwrite
}
FileOpenTool {
  id,
  label,
  kind: fileManager | terminal | ide
}
FileOpenWithToolRequest {
  workspace_id,
  path,
  tool_id
}
```

### 3. Contracts

- Resolve `path` through `WorkspaceFileService::resolve_existing_path` before
  launching any process so relative UI paths cannot escape the workspace root.
- Default App may open files or directories and should use platform-specific
  system openers such as macOS `open`, Windows `cmd /C start`, and Linux
  desktop opener fallbacks.
- Native Terminal opens the target directory directly; when the target is a
  file, it opens the file's parent directory.
- Missing paths return structured validation errors from the file service.
- Failed process spawns return `Process/file_open_*` errors with redacted
  diagnostics such as the attempted opener, not raw unchecked UI paths.
- `file_open_tool_list` always includes File Manager and Native Terminal, then
  appends only detected IDE/project tools such as VS Code, Cursor, Windsurf,
  Zed, JetBrains IDEs, Sublime Text, or Xcode.
- `file_open_with_tool` must resolve the workspace-relative path first, then
  dispatch to the selected built-in opener or detected project tool. Unknown or
  unavailable tool ids must not fall through to a shell string.

### 4. Validation & Error Matrix

- Missing or escaped path -> `Validation/file_path_missing` or the existing
  file-service path validation error.
- Native Terminal target is a file without a parent directory ->
  `Validation/file_open_native_terminal_parent_missing`.
- Default App opener spawn fails -> `Process/file_open_default_app_failed`.
- Default App opener is unavailable on Linux ->
  `Process/file_open_default_app_unavailable`.
- File Manager target is a file without a parent directory ->
  `Validation/file_open_file_manager_parent_missing`.
- File Manager opener spawn fails -> `Process/file_open_file_manager_failed`.
- Native Terminal spawn fails -> `Process/file_open_native_terminal_failed`.
- Native Terminal opener is unavailable on Linux ->
  `Process/file_open_native_terminal_unavailable`.
- Open With tool id is unknown -> `Validation/file_open_tool_unknown`.
- Open With tool id is known but not installed/detected ->
  `Process/file_open_tool_unavailable`.
- Open With project tool spawn fails -> `Process/file_open_tool_failed`.

### 5. Good/Base/Bad Cases

- Good: `file_open_default_app` receives `docs/readme.md`, resolves it inside
  the workspace, then launches the OS default app.
- Good: `file_open_native_terminal` receives `docs/readme.md` and launches the
  native terminal with `docs/` as the working directory.
- Base: empty `path` resolves to the workspace root and can be opened in the
  default app or native terminal.
- Good: `file_open_tool_list` returns File Manager, Native Terminal, and only
  detected project tools; the frontend dropdown does not show unavailable IDEs.
- Good: `file_open_with_tool` receives `tool_id=cursor`, verifies the Cursor
  CLI or app exists, and spawns it with the resolved workspace path.
- Bad: React computes an absolute path from `workspaceRootPath` and calls a
  shell/open plugin directly for Default App or Native Terminal.
- Bad: Native Terminal routes through Vibex PTY terminal creation before the PTY
  shell workflow is explicitly designed.
- Bad: `file_open_with_tool` accepts arbitrary shell command text from the UI.

### 6. Tests Required

- Unit test `WorkspaceFileService::resolve_existing_path` preserves existing
  path traversal protection and resolves empty path to the workspace root.
- Command-level smoke or mocked-process test should verify Default App resolves
  through the file service before launching a platform opener.
- Command-level smoke or mocked-process test should verify Native Terminal uses
  the target directory for directories and the parent directory for files.
- Frontend type/check coverage should keep Open In Editor disabled for known
  binary-like extensions while Default App and Native Terminal remain available
  for existing workspace paths.
- Unit or command-level tests should verify `file_open_with_tool` rejects
  unknown ids before process spawn and only launches tools from the static
  detected-tool registry.

### 7. Wrong vs Correct

#### Wrong

```typescript
const absolutePath = `${workspaceRootPath}/${target.path}`;
await openPath(absolutePath);
```

#### Correct

```typescript
await api.fileOpenDefaultApp({
  workspaceId,
  path: target.path,
  newPath: null,
  recursive: false,
  overwrite: false
});
```
