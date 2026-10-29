# SSH Remote Runtime

Vibex desktop can run its authoritative runtime on another machine and drive
it over SSH. The desktop installs `vibex-server` on the remote host, starts it
on loopback with a pinned TLS certificate, opens an `ssh -L` tunnel, fetches a
one-time pairing code over SSH and claims it itself, then connects over
[Remote Protocol v2](protocol-v2.md).

You do not need Docker, an HTTPS certificate, a public port, or a manually
copied pairing code. You do need SSH access to the host — or, for a Linux host
without sshd, an out-of-band console to bootstrap it once.

The same flow covers a WSL2 distro on the local Windows machine, which needs no
sshd at all.

> **Beta.** The feature is off by default. See [Feature flag](#feature-flag).

## Contents

- [Quick start (Linux over SSH)](#quick-start-linux-over-ssh)
- [macOS remote](#macos-remote)
- [Windows 11 + WSL2](#windows-11--wsl2)
- [Linux without sshd (bootstrap)](#linux-without-sshd-bootstrap)
- [Upgrade and rollback](#upgrade-and-rollback)
- [Edit, remove, uninstall](#edit-remove-uninstall)
- [Security boundary](#security-boundary)
- [Troubleshooting](#troubleshooting)
- [Operations reference](#operations-reference)
- [Feature flag](#feature-flag)
- [Support matrix](#support-matrix)

## Quick start (Linux over SSH)

Prerequisites on the desktop:

- An OpenSSH client (`ssh`, `ssh-keygen`) on `PATH`.
- A host entry in `~/.ssh/config` (or the config file you point Vibex at) that
  logs in **non-interactively** — key or agent authentication. Vibex runs
  every command with `BatchMode=yes`, so password and keyboard-interactive
  prompts are never answered.
- The host key already in `known_hosts`. Vibex never accepts an unknown key on
  your behalf; connect once from a terminal (`ssh <alias>`) and compare the
  fingerprint first.

Prerequisites on the host:

- Linux x86_64 or aarch64 with glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+,
  RHEL 9+).
- `tar` and `sha256sum` (or `shasum`), a writable home directory, and at least
  512 MiB free under it.
- No root access is required; everything lives in your home directory.

Steps:

1. Settings → Connections → **Add SSH connection**. Vibex lists the hosts it
   finds in your SSH config.
2. Pick the host and choose **Probe and add**. The probe checks OS,
   architecture, glibc, free space, tools, and whether the runtime port is
   free, then saves the connection and starts provisioning.
3. Provisioning walks through *Resolve host → Check environment → Upload →
   Install → Start → Open tunnel → Pair → Connect → Ready*. A failed step shows
   a diagnostics card with the next step (see [Troubleshooting](#troubleshooting)).
4. When it is Ready, the runtime appears in the Runtime Manager and becomes the
   active runtime. Switch back to the local runtime from the same menu at any
   time.

The runtime keeps running when SSH disconnects. To reconnect — after a dropped
tunnel or an app restart — the desktop starts the runtime again if it stopped,
re-opens the tunnel, and connects with the stored, pinned credential. If that
fails at launch, the workbench falls back to the local runtime; choose
**Connect** (or **Fix**) on the connection to provision and pair again.

## macOS remote

macOS hosts are supported as **remote development machines** (beta), on
Apple silicon and Intel.

1. On the Mac, turn on Remote Login: **System Settings → General → Sharing →
   Remote Login**, or run:

   ```bash
   sudo systemsetup -setremotelogin on
   ```

2. Add the Mac as in the [quick start](#quick-start-linux-over-ssh).

The server artifact is ad-hoc signed and uploaded over SSH, so it carries no
quarantine attribute and Gatekeeper does not block it; it is not notarized.

Lifecycle (Runtime Manager → the runtime → *macOS lifecycle*):

- **nohup** (default) — started on demand; survives SSH disconnects, not a
  reboot. The desktop starts it again on the next connect.
- **At login** — a per-user LaunchAgent
  (`~/Library/LaunchAgents/dev.vibex.server.<profile>.plist`, `RunAtLoad`,
  no `KeepAlive`) starts the runtime when that user logs in. LaunchAgents need
  a GUI login session, so enable automatic login if the Mac must come back
  unattended after a reboot.

Sleep pauses the runtime and drops the tunnel; the desktop reconnects after the
Mac wakes. To keep the Mac awake while you work, run `caffeinate -i` on it or
turn on *Prevent automatic sleeping* in System Settings.

## Windows 11 + WSL2

A WSL2 distro on the same Windows machine is treated as a local Linux
environment, not a remote Windows host. No sshd and no SSH keys are involved.

1. Install WSL2 and at least one distro (`wsl --install`).
2. Settings → Connections → **Add WSL runtime**, pick the distro, and choose
   **Connect WSL**.

Vibex runs the Linux x86_64 artifact inside the distro through `wsl.exe`,
installs it under the distro user's `~/.local/share/vibex/server`, and reaches
it through WSL2 localhost forwarding. The Runtime Manager shows it as
`WSL · <distro>`.

The desktop holds the runtime as the foreground process of a `wsl.exe` child,
because WSL stops a distro once its last Windows client exits. After
`wsl --shutdown` (or a crash) the desktop notices the child exit, starts the
distro and the runtime again, and reconnects.

If localhost forwarding is disabled (`localhostForwarding=false` in
`%UserProfile%\.wslconfig`), the connect fails at the forwarding check; turn it
back on and run `wsl --shutdown`.

Native Windows hosts are not supported.

## Linux without sshd (bootstrap)

A fresh VM may have no SSH server. If you can reach a root shell another way —
the cloud provider's web console, a serial console, or cloud-init — Vibex can
generate a one-time command that sets SSH up.

1. Settings → Connections → **Bootstrap SSH**.
2. Enter the **existing** Linux account Vibex will connect as (for example
   `ubuntu`, `debian`, `ec2-user`) and choose **Generate command**. The desktop
   public key is read from `~/.ssh/id_ed25519.pub` (or `id_rsa.pub` /
   `id_ecdsa.pub`).
3. **Copy command** and run it in the console within 15 minutes.
4. Add a matching entry to `~/.ssh/config`, add the printed host-key
   fingerprint to `known_hosts` after comparing it, and choose **Recheck SSH**.
   Provisioning then continues over SSH as usual.

The command embeds the audited payload (`scripts/vibex-bootstrap.sh`),
checks its SHA-256 before running it, and passes a token of the form
`v1.<expires-ms>.<nonce>`. The script:

- installs the OpenSSH server with `apt`, `dnf`, `yum`, or `apk`;
- enables and starts sshd, creating its run directory where needed;
- appends the one desktop key to that user's `authorized_keys`;
- prints the host-key fingerprints;
- rejects an expired token, and records a used token under
  `/var/lib/vibex/` so replaying the same command is refused;
- refuses a locked account when sshd has no PAM (Alpine), instead of reporting
  a login that would never work.

It never changes `PasswordAuthentication`, never enables root login, and never
touches the firewall. If the provider's firewall or security group blocks port
22, open it yourself.

For new VMs, `scripts/vibex-bootstrap-cloud-init.yaml` is a cloud-init template
that creates a `vibex` user with your key and installs OpenSSH. Replace the key
placeholder before use. It grants that user passwordless sudo — remove the
`sudo` line if you do not want that.

Without SSH access and without any out-of-band console, a host cannot be
initialized.

## Upgrade and rollback

When the connected runtime is older than the desktop, the Runtime Manager shows
**Update runtime**. Upgrading:

1. downloads the matching server artifact from the signed release manifest and
   verifies its signature, size, and SHA-256;
2. uploads and installs it into a new version directory;
3. stops the old runtime and snapshots its database (with WAL/SHM) under
   `backups/`;
4. starts the new version and waits for health;
5. switches `current`, keeping the old version as `previous`.

If the new version fails health, the desktop stops it, switches back to
`previous`, restores the database snapshot, and starts the old version.

An upgrade **restarts the runtime and interrupts running turns and terminals**;
the desktop asks before it starts. The runtime home — identity, TLS
certificate, device grants, data — is kept, so **no re-pairing is needed**.

Per-connection policy (Runtime Manager → the runtime):

- **Notify** (default) — show Update runtime and wait for you.
- **When idle** — recorded as a preference; there is no background scheduler
  yet, so it behaves like Notify.
- **Pin** — hide the update action for this runtime.

Upgrades never cross channels (stable ↔ rc). Old version directories other
than `current` and `previous` are cleaned up after a successful upgrade.

### Compatibility

The desktop evaluates every connected runtime:

| State | Meaning | Action |
| --- | --- | --- |
| Compatible | Same protocol, channel, and major/minor version | — |
| Update available | Runtime one minor version (or a patch) behind | Update runtime |
| Runtime update required | Runtime two or more minor versions behind, or missing a required capability | Update runtime |
| Update Vibex | Runtime newer than this desktop | Update Vibex (Settings → About) |
| Incompatible | No shared protocol version, a different channel (stable vs rc), or a different major version | Install matching versions on both sides |

The protocol range, channel, and major version are hard gates; the minor and
patch distance only drives the recommendation.

## Edit, remove, uninstall

- **Edit** changes the display name, SSH client and config path, workspace
  roots, and (until the runtime is bound) the local tunnel port. Reconnect to
  apply.
- **Remove** deletes only the local connection profile and credential. The
  remote runtime and its data stay on the host.
- **Uninstall runtime** stops the remote process (and unloads the macOS
  LaunchAgent), deletes the installed versions, and — only if you tick it —
  the runtime home. It refuses any install root outside your remote home and
  never deletes workspace files.

## Security boundary

Trust chain: **SSH host key → your SSH authentication → pinned TLS certificate
→ device grant**.

- **Host keys.** Every `ssh` call passes `StrictHostKeyChecking=yes`. An
  unknown key and a changed key are both hard failures; a changed key shows the
  old and new fingerprints. There is no `accept-new` path.
- **Loopback only.** The runtime binds `127.0.0.1:<port>` on the host and the
  tunnel binds `127.0.0.1:<local-port>` on the desktop, with
  `ExitOnForwardFailure=yes`. Nothing listens on a public interface.
- **TLS pinning.** The runtime's self-signed certificate reaches the desktop
  over SSH, out of band, and is pinned on first use; a different certificate is
  rejected.
- **Pairing code.** Fetched over SSH with `pairing-code --json` and claimed
  directly. It is never shown in the UI, written to the server log (the
  runtime starts with `serve --no-pairing`), included in diagnostics, or
  printed by `Debug`.
- **Artifacts.** Only artifacts listed in a signed release manifest are
  downloaded; size and SHA-256 are checked on the desktop and again on the
  host. There is no unsigned fallback.
- **Remote commands.** Scripts are sent on stdin to `sh -s`; every argument is
  shell-quoted, and host names, paths, user names, and keys are validated
  before anything runs.
- **Secrets on disk.** The remote env file is `0600` under a `umask 077`
  state directory. On the desktop the connection profiles and credentials are
  `0600` on Unix; on Windows they rely on the default ACL of your user profile
  directory.
- **Phones.** An SSH or WSL runtime is reachable only through the desktop's
  tunnel. Phones cannot pair with it directly.

## Troubleshooting

Each failure shows a card with the classified cause, next steps, **Copy
diagnostics**, and — once the runtime is installed — **View log** (the last 200
lines of `server.log`) and **Copy log command**.

| Card | Codes | What to do |
| --- | --- | --- |
| SSH client missing | `ssh_client_missing`, `ssh_spawn_failed` | Install OpenSSH: `sudo apt install openssh-client` (Debian/Ubuntu) or `brew install openssh` (macOS). On Windows use WSL instead. |
| No SSH server | `ssh_connection_refused` | Linux: install/start sshd, or use [bootstrap](#linux-without-sshd-bootstrap). On a Mac this card reads *Remote Login is off* — turn on Remote Login. On a first connect, when the OS is not known yet, the card offers both. |
| Authentication failed | `ssh_auth_failed` | Make sure `ssh <alias>` logs in from a terminal without a prompt (key loaded in the agent, right `IdentityFile`), then Retry. |
| Host key unknown | `ssh_host_key_unknown` | Run `ssh <alias>` once in a terminal, compare and accept the fingerprint, then Retry. |
| Host key changed | `ssh_host_key_changed` | Stop. Compare the old and new fingerprints with the host's owner. Only after confirming a legitimate change, remove the old entry with `ssh-keygen -R <host>` and reconnect. |
| Timeout / DNS | `ssh_timeout`, `ssh_dns_failed` | Check the host name, network, VPN, and firewall. |
| Permission | `ssh_permission_denied`, `ssh_environment_home_not_writable` | Fix ownership and write permission of the remote home and install root. |
| Disk | `ssh_environment_disk_low` | Free at least 512 MiB under the remote home. |
| glibc too old | `ssh_environment_glibc_too_old`, `ssh_environment_missing_glibc` | The host needs glibc 2.35+ (Ubuntu 22.04+, Debian 12+, RHEL 9+). musl-only hosts such as Alpine cannot run the runtime. |
| Port busy | `ssh_environment_port_busy` | Free the runtime port or Retry; the next free port is chosen. |
| Start failed | `ssh_launch_failed`, `ssh_runtime_start_failed`, `ssh_runtime_health_timeout`, … | View log, fix the cause it names, Retry. |
| Version | `ssh_upgrade_incompatible` | Update runtime or Update Vibex, per the compatibility state. |
| Tunnel | `ssh_tunnel_*` | Usually the local port is taken or SSH dropped; Retry. |
| Pairing | `ssh_pairing_*`, `remote_*` | Retry. If the runtime revoked this desktop, choose Connect on the connection to pair again. |
| Upgrade | `ssh_upgrade_*` | The upgrade rolled back to `previous`; View log for the new version's failure. |

## Operations reference

On the host (`$HOME` is the remote user's home):

| Path | Contents |
| --- | --- |
| `~/.local/share/vibex/server/<version>/vibex-server` | Installed versions |
| `~/.local/share/vibex/server/current`, `previous` | Symlinks to the active and the rollback version |
| `~/.local/share/vibex/server/home/` | Runtime home: identity, TLS certificate, device grants, `vibex.db`, provider secrets (file store) |
| `~/.local/share/vibex/server/backups/upgrade-*` | Database snapshots taken before upgrades |
| `~/.local/state/vibex/server/runtime.env` | Runtime environment (`0600`) |
| `~/.local/state/vibex/server/server.log` | Server log |
| `~/.local/state/vibex/server/runtime.pid` | PID file |
| `~/Library/LaunchAgents/dev.vibex.server.<profile>.plist` | macOS *At login* lifecycle only |

Manual control over SSH:

```bash
# status (JSON)
ssh <alias> 'set -a; . ~/.local/state/vibex/server/runtime.env; set +a; ~/.local/share/vibex/server/current/vibex-server status --json'

# stop
ssh <alias> 'set -a; . ~/.local/state/vibex/server/runtime.env; set +a; ~/.local/share/vibex/server/current/vibex-server stop'

# log
ssh <alias> 'tail -n 200 ~/.local/state/vibex/server/server.log'
```

Manual uninstall (what **Uninstall runtime** does): stop the runtime, then
`rm -rf ~/.local/share/vibex/server` — keep `~/.local/share/vibex/server/home`
if you want to keep identity and data. On macOS also run
`launchctl bootout gui/$(id -u)/dev.vibex.server.<profile>` and delete the
plist.

On the desktop, connection profiles live in the Vibex home as
`ssh-connections.json` and `wsl-connections.json`; the runtime credential sits
in the remote runtime registry next to them.

## Feature flag

SSH and WSL runtimes ship behind a flag that is **off by default**.

- Turn it on in **Settings → Developer → SSH remote runtime**.
- `VIBEX_SSH_REMOTE_RUNTIME=1` turns it on for one launch and
  `VIBEX_SSH_REMOTE_RUNTIME=0` turns it off; the environment variable wins over
  the setting, and the Developer switch locks while it is set. Use `0` as a
  kill switch that needs no release.

While the flag is off, the Connections section is hidden from settings,
search, and the command palette; an SSH or WSL runtime that was active boots
the local runtime instead; and switching to one from the Runtime Manager is
refused with a hint. Nothing is deleted — connection profiles, credentials,
the Runtime Manager entry, and the remote runtime are kept, and turning the
flag back on restores them. Docker/HTTPS servers, Remote v2 Direct/Relay, and
phone pairing are not affected by the flag.

## Support matrix

Status as verified for the 0.1.0 release candidates. Anything not listed is
untested.

| Platform | Status | Verified on |
| --- | --- | --- |
| Linux x86_64 | Beta | Ubuntu 24.04 (CI Docker E2E: provision, uninstall, upgrade + rollback); artifact smoke on Ubuntu 22.04 |
| Linux aarch64 | Experimental | Artifact is built and published (glibc 2.35 baseline); no end-to-end run on aarch64 hardware |
| macOS 14 arm64 | Beta | Real M1 hardware: provision, SSH disconnect, reboot with At login, sleep/wake, upgrade + rollback, uninstall |
| macOS x86_64 | Experimental | x86_64 artifact only under Rosetta on Apple silicon; no Intel Mac run |
| Windows 11 + WSL2 | Beta | Real Windows 11 machine (Arch distro): connect without sshd, recovery after `wsl --shutdown` |
| Bootstrap: Ubuntu / Debian | Beta | Docker Ubuntu 24.04, WSL2 Ubuntu without sshd |
| Bootstrap: RHEL 9, Alpine 3.20 | Experimental | Containers / WSL2 only; Alpine bootstrap works but the runtime itself needs glibc |
| Native Windows host | Unsupported | — |
