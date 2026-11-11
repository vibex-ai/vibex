# SSH and WSL Remote Runtime

The desktop can provision `vibex-server` on another host over SSH, or inside a
local WSL2 distro, and drive it as the authoritative runtime through Remote v2.
Code lives in `apps/desktop/src/ssh_connection/`; the workbench wiring is in
`apps/desktop/src/app.rs`. User-facing behaviour is documented in
`docs/remote/ssh-runtime.md`.

## Scenario: Remote instance entry points

### 1. Scope / Trigger

- Any change that adds or renames an SSH/WSL entry point (settings, search,
  palette, Runtime Manager), restores an SSH/WSL runtime, or switches to one.

### 2. Signatures

```rust
// apps/desktop/src/app.rs
VibexWorkbench::boot_remote_runtime(&self) -> Option<RegisteredRuntime>
settings_search_candidates_for_query(query, strings) -> Vec<SettingsSearchCandidate>
settings_section_for_query(query: &str) -> Option<SettingsSection>
settings_section_label(SettingsSection::Connections) -> &'static str // "Remote instances" / "远程实例"
```

### 3. Contracts

- SSH and WSL remote runtimes are always on; there is no stored preference, no
  `feature.rs` module, and no `VIBEX_SSH_REMOTE_RUNTIME` environment gate.
  `Settings → Remote instances` (`SettingsSection::Connections`) is listed in
  settings navigation, search candidates, the keyword fallback, and the command
  palette unconditionally.
- The sidebar label is `Remote instances` / `远程实例` / `遠端執行個體`; the page
  title uses the same string. The section id stays `Connections` so existing
  widget ids, test hooks, and state files keep working.
- An SSH/WSL active runtime boots and switches like any other remote runtime;
  `switch_to_runtime` goes straight to `begin_ssh_restore` /
  `begin_wsl_restore` with no refusal hint.
- Saved profiles, credentials, registry entries, and the remote runtime are
  ordinary user data and are never deleted by a build change.

### 4. Validation & Error Matrix

| Condition | Behaviour |
| --- | --- |
| active runtime is SSH/WSL at boot | remote boot, provisioning as before |
| user switches to an SSH/WSL runtime | connect/restore immediately |
| query `ssh` / `远程` / `实例` | Remote instances results from the shared candidate list |
| older state file with `sshRemoteRuntime` | loads cleanly; the retired key is ignored |

### 5. Good / Base / Bad

- Good: a new SSH entry point lists `SettingsSection::Connections` like every
  other section and reads no flag.
- Base: render paths read `self.developer(cx)` only for real Developer
  preferences.
- Bad: re-introducing a release flag around the section — see §7.

### 6. Tests Required

- `ui_state::developer_preferences_default_off_and_round_trip` — a state file
  carrying the retired `sshRemoteRuntime` key still loads, and the key is not
  written back.
- `app::tests::remote_instances_are_reachable_from_every_settings_entry_point` —
  navigation, search, palette, boot, and switch carry no flag gate; the
  Developer page has no switch.
- `app::tests::connections_section_is_discoverable_and_uses_the_profile_store` —
  the label reads `Remote instances` and the profile store wiring is intact.
- `app::tests::settings_sections_activate_while_the_workbench_is_leased` — must
  keep passing.

### 7. Wrong vs Correct

#### Wrong

```rust
fn settings_section_visible(&self, section: SettingsSection, cx: &App) -> bool {
    section != SettingsSection::Connections || feature::enabled(...) // hidden again
}
```

A release flag around the section hides it from navigation, search, and the
palette at once, and a stale Developer switch in the UI then lies about it.

#### Correct

Leave `SettingsSection::Connections` in the navigation list and in
`settings_search_candidates_for_query`, and keep activation free of workbench
reads: `open_settings` activates a section *inside* `workbench.update`, so
reading the workbench from `activate_settings_section` panics ("already being
updated").

## Scenario: Trust chain and remote command construction

### 1. Scope / Trigger

- Any new `ssh`/`wsl.exe` invocation, remote script, server start path, or
  copyable command shown to the user.

### 2. Signatures

```rust
SshCommand::for_destination(program, &SshDestination, config_path) -> VibexResult<SshCommand>
SshCommand::remote_script(&mut self, script: &str) -> VibexResult<&mut Self> // sh -s, script on stdin
// positional args pushed onto `args` after remote_script are shell-quoted by append_args
launch::LaunchConfig::new(remote_home, remote_install_root, workspace_roots, port)
```

### 3. Contracts

- Every `ssh` call carries `-o BatchMode=yes -o StrictHostKeyChecking=yes`
  and `--` before the target. No `accept-new`, no `UserKnownHostsFile=/dev/null`.
  Unknown and changed host keys are hard failures; the desktop never writes
  `known_hosts`.
- Remote work is a fixed script on stdin plus quoted positional arguments.
  Never interpolate a host, path, user, or key into the script text.
- The runtime binds `127.0.0.1:<port>` (`VIBEX_BIND_ADDR`); the tunnel is
  `-L 127.0.0.1:<local>:127.0.0.1:<remote>` with `ExitOnForwardFailure=yes`.
- **One home owns one runtime, and the port is part of the profile contract.**
  `status --json` is authoritative for the port a runtime actually bound, so a
  healthy status is only usable when `status.port == profile.remote_port`
  (`ServerStatus::is_healthy_on`). A healthy runtime on another port is
  restarted, never adopted: the profile, `runtime.env`, the tunnel and the
  pairing endpoint all follow `remote_port`, so adopting the old port leaves
  pairing dialing a closed loopback port (`remote_http_unreachable`).
  The launcher (`START_SCRIPT`) enforces the same rule and reports
  `launch=port_conflict` instead of `already_running`.
- The environment probe reports `port=own_runtime` when the listener on the
  probed port is this home's live runtime, and that state is not "busy":
  re-adding a host must keep the port the runtime already bound.
- Every `serve` launch (nohup, LaunchAgent plist, WSL foreground) passes
  `--no-pairing`; the pairing code is fetched only via `pairing-code --json`
  and claimed with the certificate from the same payload.
- Artifacts come only from a `VerifiedManifest`; the host re-checks SHA-256.
- On-host layout: install root `~/.local/share/vibex/server/{<version>,current,previous,home,backups}`,
  state `~/.local/state/vibex/server/{runtime.env (0600),server.log,runtime.pid}`.
  Uninstall refuses any root outside the remote home.

### 4. Validation & Error Matrix

| Condition | Code |
| --- | --- |
| host key unknown / changed | `ssh_host_key_unknown` / `ssh_host_key_changed` |
| script > limit / NUL | `ssh_script_too_large` / `ssh_script_invalid` |
| uninstall root outside home | `ssh_uninstall_root_invalid` |
| bootstrap user / key shape | `bootstrap_user_invalid` / `bootstrap_key_invalid` |

### 5. Good / Base / Bad

- Good: `command.remote_script("printf '%s' \"$1\"")?; command.args.push(value.into());`
- Base: diagnostics `log_command` is display-only text; the app never runs it.
- Bad: `format!("tail -n 200 {path}")` passed as the script.

### 6. Tests Required

- `command::tests::hostile_arguments_survive_the_remote_shell_verbatim` — fake
  ssh runs `sh -c "$*"` like sshd; `;`, `$()`, backticks, quotes, newlines reach
  the script literally and nothing executes.
- `launch::tests::start_script_never_prints_a_pairing_code_into_the_log`.
- `launch::tests::start_script_requires_the_running_port_to_match_the_env_port`
  and `supervisor::tests::ensure_started_restarts_a_runtime_bound_to_another_port`
  pin the port-ownership rule; `ensure_started_keeps_a_runtime_on_the_requested_port`
  pins the no-restart path, and
  `environment::tests::a_port_served_by_this_homes_runtime_is_not_busy` pins the
  probe's `own_runtime` classification.
- Docker E2E `provisioning_reaches_ready_over_sshd` asserts the real
  `server.log` has no `pairing_code=` / `vibex://pair`.
- `pairing::tests` — `Debug` of `PairingPayload` redacts code, link, cert.

### 7. Wrong vs Correct

#### Wrong

```rust
generate("ubuntu", &key, 22, now) // bootstrap for an assumed account
```

#### Correct

```rust
let user = self.bootstrap_user_input.read(cx).value().trim().to_string();
generate(&user, &key, 22, now)    // validate_user rejects bad names
```

## Scenario: Windows child console windows

### 1. Scope / Trigger

- Any new `ssh` / `ssh-keygen` / `wsl.exe` spawn under
  `apps/desktop/src/ssh_connection/`, and any change to how one of those
  children is started.

### 2. Signatures

```rust
// apps/desktop/src/ssh_connection/console.rs
console::hidden_command(program: impl AsRef<OsStr>) -> std::process::Command
```

### 3. Contracts

- Those three executables are console programs and the desktop is a
  GUI-subsystem binary, so Windows allocates a console of its own for every
  child — a black window over the reader's desktop that piped stdio does not
  prevent. Every production spawn in `ssh_connection` goes through
  `hidden_command`, which passes `CREATE_NO_WINDOW` on Windows and is a plain
  `Command::new` elsewhere.
- The rule covers the whole module, not only `SshCommand`: the `ssh -G` config
  probe, the `ssh-keygen -F` host-key check, the `-N -L` tunnel,
  `WslCommand`, `WslKeepalive`, and `list_distros` all inherit it.

### 4. Validation & Error Matrix

| Condition | Behaviour |
| --- | --- |
| Windows child without the flag | console window appears and stays for the child's lifetime |
| non-Windows build | helper is `Command::new`, no flags, no behaviour change |

### 5. Good / Base / Bad

- Good: `hidden_command(&self.program)` in `SshCommand::run_with_timeout`.
- Base: `Command::new` in a `#[cfg(test)]` block, which never runs on Windows.
- Bad: `Command::new("ssh")` in production code — the test below fails on it.

### 6. Tests Required

- `ssh_connection::console::tests::remote_instance_children_never_use_command_new_directly`
  — scans the production half of every spawning file for a bare `Command::new`
  and fails naming the file.
- `ssh_connection::console::tests::hidden_command_keeps_the_program_it_was_given`
  — the helper still starts the program it was handed.

## Scenario: WSL distro discovery and its failures

### 1. Scope / Trigger

- Any change to how the desktop lists WSL distros, opens the **Add WSL
  runtime** dialog, or reports a `wsl_*` failure.

### 2. Signatures

```rust
// apps/desktop/src/ssh_connection/wsl.rs
wsl::list_distros(program: &Path) -> Result<Vec<String>, WslError> // LIST_TIMEOUT = 15s
wsl::parse_distro_output(bytes: &[u8]) -> Vec<String>

// apps/desktop/src/app.rs — FoundationSettings
FoundationSettings::load_wsl_distros(&mut self, cx: &mut Context<Self>)
```

### 3. Contracts

- Saying "SSH into WSL" is a category error: the WSL transport is `wsl.exe -d
  <distro> --exec /bin/sh`, so no sshd, key, or agent takes part. A reader who
  reports an SSH failure and a WSL failure at once has hit two unrelated paths.
- A failed `wsl -l -q` must carry `wsl.exe`'s own stderr and exit status. The
  class code names the failure; only the child's sentence names the cause.
- The distro read never runs on the UI thread. `wsl.exe` is a process call with
  no bound of its own, and a half-installed WSL used to freeze the settings
  window before the dialog could paint. It runs through
  `gpui_tokio::Tokio::spawn` + `tokio::task::spawn_blocking` (`spawn_blocking`
  panics when polled from the foreground executor), and `list_distros` enforces
  its own deadline with `try_wait`.
- The dialog owns three states — reading, failed with Retry, list — and a
  superseded read is dropped by generation rather than overwriting a newer one.
- Every `wsl_*` code classifies as `SshDiagnosticKind::Wsl`, whose guidance names
  `wsl --update` / `wsl -l -v` and never offers to install an SSH server.
- A non-empty `SshFailure::stderr_tail` survives into `EnvironmentError::message`
  for the same reason: `ssh_auth_failed` alone is identical for a locked key, an
  unloaded agent, a wrong `IdentityFile` and a host that refuses key auth.

### 4. Validation & Error Matrix

| Condition | Behaviour |
| --- | --- |
| `wsl.exe` not on PATH | `wsl_missing` |
| spawn denied / not found at the path | `wsl_list_spawn_failed` with the OS error |
| `-l -q` exits non-zero | `wsl_exec_failed`, message carries stderr + exit code |
| no answer within `LIST_TIMEOUT` | `wsl_list_timeout`, retryable |
| dialog opened, read still running | "Reading WSL distros…", Connect disabled |
| read failed | the child's message stays in the dialog, plus a Retry button |
| read succeeded with no distros | "Install WSL2 and at least one distro first." |

### 5. Good / Base / Bad

- Good: `list_distros` drains both pipes on reader threads and polls
  `try_wait` against a deadline, then reports `stderr_detail(&stderr)`.
- Base: `parse_distro_output` decoding UTF-16LE only when the stream carries a
  NUL byte.
- Bad: `Command::output()` from a dialog opener — no deadline, no stderr, and
  the UI thread blocks on it.

### 6. Tests Required

- `ssh_connection::wsl::tests::a_failed_list_keeps_the_childs_own_words` — the
  stderr and exit status survive; the error is not marked retryable.
- `ssh_connection::wsl::tests::a_hung_wsl_is_cut_off_at_the_deadline` — a child
  that never returns is killed and reported as `wsl_list_timeout`.
- `ssh_connection::wsl::tests::a_missing_wsl_exe_stays_a_installation_problem`
- `ssh_connection::diagnostics::tests::wsl_failures_get_their_own_card_and_never_ssh_advice`
- `ssh_connection::environment::tests::a_failed_probe_reports_the_openssh_sentence_with_its_class`
- `app::tests::the_wsl_distro_read_runs_off_the_ui_thread_and_reports_every_state`

## Scenario: The runtime a provisioning run borrows

### 1. Scope / Trigger

- Any change to `ProvisioningService` (start, worker thread, cancellation,
  effects handoff), or to where `SshProvisioningEffects::connect` runs.
- A report that an SSH connection establishes and then fails on the next read
  with `Runtime catalog unavailable (remote_socket_write_failed: … A Tokio 1.x
  context was found, but it is being shutdown.)`.

### 2. Signatures

```rust
// apps/desktop/src/ssh_connection/service.rs
ProvisioningService::start(
    runtime: &tokio::runtime::Handle, effects: E, profile_id: String,
    now_ms: i64, retry_failed: bool,
) -> Result<ProvisioningService<E>, ProvisioningServiceError>
ProvisioningService::start_with_cancellation(/* …, cancel: RunCancellation */)

// apps/desktop/src/app.rs
VibexWorkbench::start_ssh_provisioning(/* … */)   // gpui_tokio::Tokio::handle(cx)
```

### 3. Contracts

- **A socket belongs to the runtime that created it, for its whole life.** A
  provisioning run ends by handing a *connected* backend to the workbench, and
  every later RPC writes through that socket. So the run must be driven by a
  runtime that outlives the run — the workbench's `gpui_tokio` runtime, not one
  the service builds for itself.
- The service still owns a dedicated `vibex-ssh-provisioning` thread, because
  the effects future is not `Send`. It enters the borrowed runtime from that
  thread; it does not create a runtime.
- Dropping the runtime that opened a socket does not close it visibly: the
  first write to the file descriptor returns
  `io::ErrorKind::Other` with `"A Tokio 1.x context was found, but it is being
  shutdown."` (`tokio::runtime::is_rt_shutdown_err`). That detail reaches the
  UI as the `remote_socket_write_failed` message on the *catalogue* read, which
  is why the symptom names a runtime catalog and not SSH.
- A current-thread runtime is rejected up front
  (`ssh_provisioning_runtime_unsupported`) instead of accepted and hung on:
  `Handle::block_on` cannot drive a current-thread runtime's I/O and timer
  drivers.

### 4. Validation & Error Matrix

| Condition | Behaviour |
| --- | --- |
| caller passes a multi-thread `Handle` | run proceeds on it; the handoff backend stays usable |
| caller passes a current-thread `Handle` | `ssh_provisioning_runtime_unsupported`, no worker spawned |
| worker thread cannot start | `ssh_provisioning_worker_failed` |
| worker panics | `ssh_provisioning_worker_panicked` from `join` |
| service dropped mid-run | cancellation only; the route/supervisor effects keep their own cleanup |

### 5. Good / Base / Bad

- Good: `start_ssh_provisioning` reads `gpui_tokio::Tokio::handle(cx)` and
  passes it to `ProvisioningService::start`.
- Base: the service keeps the `Send`-bound workaround — a thread for the
  future — while borrowing the runtime for its drivers.
- Bad: `Builder::new_current_thread().enable_all()` inside the worker. Every
  effect up to Connect passes, the backend is installed, and the *next* RPC
  fails with the shutdown error because the runtime died with the worker.

### 6. Tests Required

- `ssh_connection::service::tests::a_socket_opened_by_the_run_still_writes_after_the_worker_exits`
  — opens a socket in the connect step, joins the worker, then writes from the
  caller's runtime; fails with the field error if the run owned its runtime.
- `ssh_connection::service::tests::a_current_thread_runtime_is_rejected_instead_of_hanging_the_run`
- `ssh_connection::service::tests::runs_effects_on_the_worker_and_emits_updates`
- `ssh_connection::service::tests::cancel_stops_the_worker_before_the_next_effect`

## Testing Gotchas

- Fake executables (`ssh`, `ssh-keygen`, `wsl.exe`) must be written with
  `test_support::write_executable` at a `test_support::unique_path`, and spawned
  under `test_support::process_guard()`. Writing in place and executing races
  `ETXTBSY` under the parallel test binary.
- A "port is not listening" assertion must hold the port with a bound,
  non-listening socket (`tokio::net::TcpSocket::bind`), not probe a port whose
  listener was just dropped.
- `scripts/e2e-ssh-docker.sh` needs a server tarball built from the current
  tree; an old artifact without `status --json` fails as
  `ssh_runtime_status_invalid`. Build it in `deploy/server/Dockerfile.build`
  (Debian 12, glibc ≤ 2.35) and check with `scripts/check-server-artifact.mjs`.
- Building a second checkout with a shared `CARGO_TARGET_DIR` duplicates every
  workspace crate (package ids include the source path), roughly 25 GB for the
  desktop test and clippy set. Clean those artifacts before removing the
  checkout.
