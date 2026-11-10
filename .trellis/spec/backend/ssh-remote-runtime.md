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
