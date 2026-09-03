<h1 align="center">
  <img src="logo-wordmark-white.svg" alt="Vibex" width="168" />
</h1>

<p align="center">
  <a href="README.md">English</a> &nbsp;|&nbsp;
  <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <strong>Run your Agents. Own your workspace. Ship the change.</strong><br />
  A native, local-first AI coding workbench for Agent-powered software development.
</p>

<p align="center">
  <a href="https://github.com/vibex-ai/vibex"><img src="https://img.shields.io/github/stars/vibex-ai/vibex?style=flat&logo=github" alt="GitHub stars" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--or--later-blue?style=flat" alt="License: AGPL-3.0-or-later" /></a>
  <img src="https://img.shields.io/badge/Rust-1.97.0-black?style=flat&logo=rust&logoColor=white" alt="Rust 1.97.0" />
  <img src="https://img.shields.io/badge/UI-GPUI-2563eb?style=flat" alt="GPUI" />
  <img src="https://img.shields.io/badge/status-0.1.0--rc.1-f97316?style=flat" alt="Release status: 0.1.0-rc.1" />
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> &nbsp;&bull;&nbsp;
  <a href="#features">Features</a> &nbsp;&bull;&nbsp;
  <a href="#architecture">Architecture</a> &nbsp;&bull;&nbsp;
  <a href="#development">Development</a>
</p>

Vibex brings Agent sessions, source code, Git, terminals, previews, and provider
configuration into one focused workbench. The desktop runtime stays in control
of Agent processes, session history, workspace files, Git, PTYs, providers, and
permissions. A native mobile client lets you monitor and steer those same
sessions remotely, while an optional self-hosted Relay carries only encrypted
frames.

No Vibex-hosted workspace is required to run the local desktop. Your repository,
credentials, and durable session state remain on the machines you choose.

## Why Vibex

| Principle | What it means in practice |
| --- | --- |
| **Local-first authority** | The PC owns the runtime and durable state. Mobile is a typed remote client, not a second backend. |
| **Native by default** | Rust and GPUI power the desktop and mobile clients without a browser shell or a separate WebUI product. |
| **One Agent contract** | Every online Agent session uses ACP and managed adapters, giving integrations a shared session, capability, permission, and terminal surface. |
| **Remote without lock-in** | Pair over Direct, Tailnet, or your own Relay. The Relay forwards encrypted frames and never becomes a second database. |
| **Inspectable workflows** | Plans, tool calls, process details, diffs, approvals, elicitation forms, errors, and reconnect states stay visible and explicit. |
| **Provider choice** | Configure Agents, model providers, credentials, MCP servers, Skills, Prompts, and Hooks inside Vibex-scoped configuration. |

## Features

Vibex is designed for the complete loop: ask an Agent to change code, inspect
what it did, run the project, review the diff, and keep the session available
wherever you are.

| Workflow | Included |
| --- | --- |
| **Agent sessions** | Stream user and Agent messages as a structured timeline with Markdown, reasoning, plans, tool and process details, attachments, approvals, and typed elicitation forms. Stop, continue, fork, rename, and resume sessions, or import supported local histories. |
| **Workspace and editor** | Browse workspace-scoped file trees, search by name or content, read and write files with revision checks, and keep encoding, line endings, and large-file limits explicit. |
| **Git-native review** | Inspect status, history, diffs, and blame; create and switch branches; stage, unstage, revert, commit, fetch, and push; and manage isolated Git worktrees with guarded merge and rebase recovery. |
| **Terminal and content** | Use native PTYs with ANSI emulation, tabs, resize, scrollback, search, and raw-byte handling. Preview Markdown, images, PDFs, and supported Office documents in the workbench. |
| **Agent and provider center** | Discover and manage Agent runtimes, select models and reasoning options, authenticate providers, run health and capability probes, and keep provider profiles separate from online route identity. |
| **MCP and workflow resources** | Import, validate, and scope MCP servers, Skills, Prompts, and Hooks. Give Agents reusable context without scattering configuration across unrelated home directories. |
| **Automation** | Schedule one-shot, interval, or daily Agent runs and compose automation graphs with explicit run state, recovery, and audit history. |
| **Attention and continuity** | Receive desktop notifications for completed, failed, or input-blocked sessions, track unread completions, and recover authoritative timeline state after reconnects or restarts. |
| **Mobile companion** | Pair an iOS or Android client with the desktop runtime, read the same GUI Agent timeline, approve or decline requests, send follow-ups, inspect files and Git state, and use remote terminal surfaces when available. |

## Supported Agents

Vibex uses the [Agent Client Protocol](https://agentclientprotocol.com/) (ACP)
as the only online Agent transport. The runtime is provider-neutral: an Agent's
identity and capabilities are kept separate from the protocol used to connect it.

Built-in presets include **Claude Code**, **Codex**, **ZCode**, and
**OpenCode**. The ACP catalog also includes integrations such as **Antigravity**,
**Cline**, **Codebuddy Code**, **Cursor**, **Gemini CLI**, **GitHub Copilot**,
**Devin**, **Grok**, **Hermes**, **Kimi Code**, **Pi**, **DeepSeek Harness**, and
more. Availability is checked against the installed runtime, provider
configuration, and live capability probes.

You can also register any ACP-compatible executable with its command, arguments,
environment, and display metadata. Adding an Agent does not require a new UI or
a second vendor-specific session protocol.

## Native Surfaces

| Surface | Role | Entry point |
| --- | --- | --- |
| **Desktop** | Full native GPUI workbench and the authoritative `DesktopRuntime` for Agents, workspaces, files, Git, PTYs, providers, permissions, and durable state. | Linux, macOS, and Windows targets; start with `pnpm dev:desktop`. |
| **Mobile** | Native GPUI iOS and Android companion. It renders the desktop session model as a GUI timeline and sends typed remote mutations. | `apps/mobile`; build with `pnpm build:mobile:android` or `pnpm build:mobile:ios`. |
| **Self-hosted Relay** | Optional Rust/Axum transport for encrypted WebSocket frames. It stores no workspace, provider, Agent, or application data. | `apps/relay-server` and `deploy/relay`; validate with `pnpm smoke:relay:local`. |

Mobile never starts a local Agent, owns a workspace filesystem, mutates local
Git, or becomes a second PTY authority. Its compact native UI preserves the
desktop Agent timeline semantics, including loading, streaming, approval,
error, and reconnect states.

## Quick Start

### Prerequisites

- Git with submodule support
- Node.js 22
- pnpm 11.3.0
- Rust 1.97.0, pinned by `rust-toolchain.toml`
- A graphical session and working Vulkan driver for desktop development

Native mobile builds additionally require the Android SDK/NDK and `cargo-ndk`,
or macOS with Xcode, XcodeGen, and the Apple Rust targets. See the platform
sections below for the exact commands.

### Run the desktop workbench

```bash
git clone https://github.com/vibex-ai/vibex.git
cd vibex
git submodule update --init --recursive
pnpm install --frozen-lockfile
pnpm dev:desktop
```

The desktop app starts with a local runtime. Configure an Agent and provider in
the Config Center, open a project directory, then create an Agent session from
the workbench.

### Repository checks

```bash
pnpm check
pnpm release:build-smoke
pnpm check:mobile-native
```

`pnpm check` runs the normal Rust, frontend, license, and deterministic behavior
gates. `pnpm release:build-smoke` checks the native desktop, mobile, and Relay
build graph. `pnpm check:mobile-native` validates the mobile crate and checked-in
native project contract without claiming that an Android or iOS SDK is installed
on the host.

## Development

### Desktop packaging

Targeted desktop checks are useful while iterating:

```bash
cargo check -p vibex-desktop --locked
cargo test -p vibex-desktop --locked
pnpm smoke:first-frame
```

Linux preview packages use cargo-packager and the reviewed PDFium runtime:

```bash
cargo install cargo-packager --version 0.11.8 --locked
pnpm prepare:pdfium
pnpm package:preview
```

Use [the release runbook](docs/operations/release.md) before building RC or
Stable packages. Generated packages and runtime binaries are build outputs and
are not tracked in the repository.

### Android

Install the Android SDK/NDK, `cargo-ndk`, and the Rust targets for the ABIs you
will build. Debug defaults to `arm64-v8a` and `x86_64`; release defaults to
`arm64-v8a`.

```bash
pnpm build:mobile:android
pnpm package:mobile:android
```

Set `VIBEX_MOBILE_ANDROID_TARGETS` to a space-separated ABI list to override the
debug defaults. The release APK is unsigned and must be aligned and signed with
the intended release key before distribution.

### iOS

On macOS, install Xcode, XcodeGen, and the `aarch64-apple-ios` plus
`aarch64-apple-ios-sim` Rust targets:

```bash
pnpm build:mobile:ios
```

This builds `VibexFFI.xcframework` and generates the Xcode project. Signing,
simulator or device selection, and distribution credentials remain local to the
developer or release pipeline.

### Self-hosted Relay

The Relay is optional. Run it locally with:

```bash
docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server
curl -fsS http://127.0.0.1:9700/health
docker compose -f deploy/relay/docker-compose.yml down
```

Use [the Relay deployment guide](deploy/relay/README.md) for HTTPS through
Caddy, private Tailnet publication, runtime limits, and the optional
operator-owned push adapter. Keep the default loopback binding for local
development.

## Architecture

The desktop runtime is the single authority. Shared contracts and projections
keep native desktop and mobile behavior aligned without duplicating business
state.

```text
DesktopRuntime
|-- ACP Agent sessions and authoritative timelines
|-- Workspace files, editor, Git, and managed worktrees
|-- Native PTYs, content previews, providers, and permissions
`-- Typed RemoteGateway v2
    |-- Direct or Tailnet transport
    `-- Encrypted self-hosted Relay
            -> AutoRemoteTransport
            -> Native mobile GPUI client
```

The main ownership boundaries are:

- `crates/core` defines serialized ids, DTOs, errors, capabilities, and remote
  protocol contracts.
- `crates/desktop-model` owns framework-neutral session and timeline projections
  and reducers.
- `crates/vibex-backend` exposes provider-neutral capability facades shared by
  native and remote adapters.
- `crates/vibex-ui` owns semantic tokens, portable component models, workflow
  controllers, and shell composition.
- `crates/vibex-remote-client` owns pairing, reconnect, synchronization, and
  Direct, Tailnet, and Relay route selection.
- `apps/desktop` owns the native workbench and `DesktopRuntime`; `apps/mobile`
  only composes the shared remote projections into a compact native surface.

Read the [UI architecture boundary](docs/architecture/ui-boundary.md),
[platform support matrix](docs/platform/support-matrix.md), and
[Remote Protocol v2](docs/remote/protocol-v2.md) for the detailed contracts.

## Tech Stack

| Layer | Technologies |
| --- | --- |
| **Application** | Rust 2024, Cargo workspace, Rust 1.97.0 |
| **Native UI** | GPUI, `gpui-component`, shared Rust UI contracts, generated design tokens |
| **Agent runtime** | Agent Client Protocol schema 1.6, managed ACP adapters, Tokio |
| **Workspace runtime** | SQLite via `rusqlite`, filesystem and Git services, `xpty`, `alacritty_terminal` |
| **Content** | `pulldown-cmark`, HTML5ever, MathJax SVG, Mermaid rendering, PDFium, ZIP/XML parsers |
| **Remote transport** | Axum, HTTP/WebSocket v2, Rustls, `tokio-tungstenite`, X25519, HKDF, HMAC, ChaCha20-Poly1305 |
| **Platforms** | Linux, macOS, and Windows desktop; native Android and iOS through `gpui_android` and `gpui_ios` |
| **Tooling** | Node.js 22, pnpm 11.3.0, deterministic smoke tests and evidence-based release gates |

## Privacy and Control

- **Local-first storage:** session timelines, workspace metadata, provider
  profiles, and runtime state are owned by the desktop runtime and stored locally.
- **Explicit permissions:** Agent tools, terminal actions, device access, and
  remote mutations are capability-gated and auditable.
- **Encrypted remote access:** Direct, Tailnet, and Relay routes use typed
  handshakes and encrypted transport. Relay forwards frames but cannot decrypt
  business payloads.
- **Scoped provider configuration:** Vibex does not rewrite real Agent home
  configuration by default. Export is an explicit user action with preview and
  rollback boundaries.
- **Redacted diagnostics:** tokens, private keys, prompts, file contents, and
  terminal bytes are excluded from logs and evidence artifacts.

## Contributing

Issues and pull requests are welcome. Before opening a change:

- Keep `DesktopRuntime` as the only authority and route mobile behavior through
  typed backend and remote contracts.
- Run the smallest relevant gate while iterating, then run `pnpm check` for
  cross-layer changes.
- Include updated evidence when changing behavior covered by a release gate.

## License

Vibex is free software licensed under the
[GNU Affero General Public License v3.0 or later](LICENSE). Third-party package,
font, icon, and PDFium notices are recorded in
[docs/licenses](docs/licenses/README.md).
