# SSH and WSL Remote Runtime

The desktop can provision `vibex-server` on another host over SSH, or inside a
local WSL2 distro, and drive it as the authoritative runtime through Remote v2.
Code lives in `apps/desktop/src/ssh_connection/`; the workbench wiring is in
`apps/desktop/src/app.rs`. User-facing behaviour is documented in
`docs/remote/ssh-runtime.md`.

## Scenario: Release flag and entry points

### 1. Scope / Trigger

- Any change that adds an SSH/WSL entry point (settings, search, palette,
  Runtime Manager), restores an SSH/WSL runtime, or switches to one.

### 2. Signatures

```rust
// apps/desktop/src/ssh_connection/feature.rs
pub const FEATURE_ENV: &str = "VIBEX_SSH_REMOTE_RUNTIME";
pub fn enabled(setting: bool) -> bool;          // env override, else setting
pub fn env_override() -> Option<bool>;          // 1/true/on/yes, 0/false/off/no

// crates/desktop-model/src/ui_state.rs
DeveloperUiState { ssh_remote_runtime: bool }   // serde "sshRemoteRuntime", default false

// apps/desktop/src/app.rs
VibexWorkbench::ssh_remote_runtime_enabled(&self) -> bool
VibexWorkbench::runtime_is_ssh_or_wsl(&self, runtime_id: &str) -> bool
FoundationSettings::settings_section_visible(&self, SettingsSection, &App) -> bool
FoundationSettings::visible_settings_search_candidates(&self, query, strings, &App)
```

### 3. Contracts

- Default off. The environment value wins over the stored setting in both
  directions; unrecognised values are ignored.
- Off hides entry points only. It never deletes `ssh-connections.json`,
  `wsl-connections.json`, registry entries, credentials, or the remote runtime.
- Gates while off:
  - settings navigation, search candidates, command palette, and the
    `settings_section_for_query` keyword fallback skip `SettingsSection::Connections`;
  - `boot_remote_runtime` returns `None` for an SSH/WSL active runtime (the
    embedded runtime boots; the registry keeps the remote active);
  - `switch_to_runtime` refuses an SSH/WSL runtime with a warning hint before
    any `begin_ssh_restore` / `begin_wsl_restore`.
- Paired Docker/HTTPS/Relay runtimes are never gated.

### 4. Validation & Error Matrix

| Condition | Behaviour |
| --- | --- |
| flag off, active runtime is SSH/WSL at boot | local boot, no provisioning |
| flag off, user switches to SSH/WSL runtime | hint, no state change |
| flag off, query `ssh` / `主机` | no Connections result; Developer "SSH remote runtime" row is found |
| env set | Developer switch shows the effective value and is disabled |

### 5. Good / Base / Bad

- Good: a new SSH entry point checks `ssh_remote_runtime_enabled()` (workbench)
  or `settings_section_visible` (settings) before acting.
- Base: render paths read the flag through `self.developer(cx)`.
- Bad: reading workbench state from `activate_settings_section` — see §7.

### 6. Tests Required

- `ssh_connection::feature::tests` — override parsing both directions.
- `ui_state::developer_preferences_default_off_and_round_trip` — legacy state
  loads off; `sshRemoteRuntime` round-trips.
- `app::tests::ssh_remote_runtime_flag_gates_every_entry_point` — each gate
  above is present, and the switch gate precedes `begin_ssh_restore`.
- `app::tests::settings_sections_activate_while_the_workbench_is_leased` — must
  keep passing.

### 7. Wrong vs Correct

#### Wrong

```rust
fn activate_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
    if !self.settings_section_visible(section, cx) { return; } // reads the workbench
    self.active_section = section;
```

`open_settings` activates a section *inside* `workbench.update`, so reading the
workbench there panics ("already being updated").

#### Correct

Filter the inputs that can name a hidden section (navigation list, search
candidates, keyword fallback) and leave activation free of workbench reads.

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
