<!-- TRELLIS:START -->
# Trellis Instructions

These instructions are for AI assistants working in this project.

This project is managed by Trellis. The working knowledge you need lives under `.trellis/`:

- `.trellis/workflow.md` — development phases, when to create tasks, skill routing
- `.trellis/spec/` — package- and layer-scoped coding guidelines (read before writing code in a given layer)
- `.trellis/workspace/` — per-developer journals and session traces
- `.trellis/tasks/` — active and archived tasks (PRDs, research, jsonl context)

If a Trellis command is available on your platform (e.g. `/trellis:finish-work`, `/trellis:continue`), prefer it over manual steps. Not every platform exposes every command.

If you're using Codex or another agent-capable tool, additional project-scoped helpers may live in:
- `.agents/skills/` — reusable Trellis skills
- `.codex/agents/` — optional custom subagents

Managed by Trellis. Edits outside this block are preserved; edits inside may be overwritten by a future `trellis update`.

<!-- TRELLIS:END -->

---

# Vibex Project Instructions

Everything below this line is project-owned. `trellis update` only rewrites the
managed block above, so these rules survive template upgrades.

## 1. Two-repository boundary — read this before any `git` command

**This working tree is tracked by two different git repositories at the same
time.** Getting this wrong publishes private material to a public open-source
repository, and rewriting public history afterwards does not un-publish it.

| | Public code repository | Private task repository |
| --- | --- | --- |
| Reached by | plain `git ...` | `git --git-dir=.trellis/.taskgit ...` |
| Git dir | `.git` | `.trellis/.taskgit` |
| Work tree | repository root | `.trellis/` |
| Remote | shared open-source repo (`origin`) | **each developer's own private repo, or none** |
| Visibility | public | private to that one developer |

There is at most one private repository *per developer*. It is never a
team-shared location, its URL is never referenced from any tracked file, and it
is **optional** — see "Contributors without a private task repository" below.
Private material stays out of the public remote either way.

### Path ownership

**Private — belongs only to the task repository:**

```
.trellis/tasks/          active + archived tasks (prd.md, design.md, research/, *.jsonl)
.trellis/research/       raw research notes and superseded planning reports
.trellis/workspace/<dev>/  per-developer journals, screenshots, personal index
.trellis/.runtime/       session-scoped runtime state
.trellis/.current-task   active-task pointer
```

**Public — belongs to the code repository:** everything else, including
`.trellis/spec/`, `.trellis/scripts/`, `.trellis/agents/`, `.trellis/config.yaml`,
`.trellis/workflow.md`, and `.trellis/workspace/index.md`.

The split is semantic, not just mechanical: `spec/` holds durable, shareable
conclusions; `research/` and `tasks/` hold working material that quotes
third-party source trees, local absolute paths, and unreleased plans. When you
promote a finding from research into spec, rewrite it — do not paste it.

### Rules for agents

1. **Never pass `-f` / `--force` to `git add` for anything under `.trellis/`.**
   The ignore rules are the only thing standing between the task tree and the
   public remote, and `-f` is the one flag that defeats them. If a `git add`
   appears to "miss" a task file, that is the design working correctly — switch
   to the `--git-dir` form instead of forcing it.
2. **Never stage an explicit `.trellis/tasks/...`, `.trellis/research/...`,
   `.trellis/workspace/<dev>/...` path in the public repository.**
3. **Commit task artifacts through the task repository only:**
   ```bash
   git --git-dir=.trellis/.taskgit add -A
   git --git-dir=.trellis/.taskgit commit -m "..."
   ```
   `core.worktree` is already configured, so `--git-dir` is the only flag needed.
4. **A finished task produces a public commit for code and spec, and — when the
   developer has a private task repository — a separate commit there for
   `prd.md` / `design.md` / `research/` / journal.** Do not consider Phase 3.4
   done after committing only the public side. When no `.trellis/.taskgit`
   exists, the task artifacts stay uncommitted on disk by design; leave them.
5. **Never move the private ignore rules into `.trellis/.gitignore`.** See below.
6. **Never push either repository without being asked.**

### Verify before every commit that touches `.trellis/`

```bash
git diff --cached --name-only \
  | grep -E '^\.trellis/(tasks/|research/|\.runtime/|\.current-task$)|^\.trellis/workspace/[^/]+/' \
  && echo "STOP — private task content is staged for the public repository"
```

Empty output means safe. Any hit means unstage those paths before committing.

### Why the ignore rules live in the root `.gitignore`

The private paths are ignored from the **repository-root** `.gitignore`, not from
`.trellis/.gitignore`. This is deliberate and load-bearing:

- The task repository's work tree starts at `.trellis/`, so it never reads the
  root `.gitignore` — the rules there hide those paths from the public repo only.
- A `.gitignore` *inside* a work tree outranks that repository's
  `info/exclude`. Putting `/tasks/` back into `.trellis/.gitignore` would make
  the task repository silently blind to every new task file: `git status` would
  report a clean tree while work goes untracked.

`.trellis/.gitignore` therefore keeps only rules both repositories want
(`.plan-log`, `*.tmp`, `__pycache__`, `.developer`, ...). `trellis update` may try
to restore the upstream version of that file — if it does, keep the local one.

### Symptoms of a broken setup

- `git status` (public) lists files under `.trellis/tasks/` → root ignore rules lost.
- `git --git-dir=.trellis/.taskgit status` lists `spec/` or `scripts/` →
  `.taskgit/info/exclude` lost.
- `git --git-dir=.trellis/.taskgit status` reports clean while new task files
  exist → private rules leaked back into `.trellis/.gitignore`.

Stop and report any of these instead of working around them.

### Contributors without a private task repository

A private task repository is **optional**. Contributors who skip it simply keep
their task tree local: the files sit in `.trellis/` untracked and ignored, and
Trellis works in full — task creation, planning artifacts, research, journals,
spec updates, every script.

Nothing leaks in that mode. The protection comes from the ignore rules in the
repository-root `.gitignore`, which is a tracked file every clone inherits.
`.trellis/.taskgit` is only a backup-and-sync mechanism; it is not what keeps
private material out of the public remote.

What local-only contributors give up, and the one thing to warn them about:

- No backup and no cross-machine sync — the task tree lives and dies with the
  working copy.
- **`git clean -fdx` (or `-fdX`) permanently deletes the entire task tree.**
  Those paths are ignored, so `-x` / `-X` sweeps them away with no git history
  anywhere to restore from. Never run `git clean` with `-x` or `-X` in this
  repository without checking `.trellis/` first.
- Parallel git worktrees each carry their own independent task tree.

Note on Trellis session auto-commit: `add_session.py` would normally stage the
journal and current task dir into whatever plain `git` points at — the public
repository — which always fails here because those paths are ignored. Trellis
warns and skips rather than forcing (`safe_git_add` never retries with `-f`).
This project therefore sets `session_auto_commit: false` in
`.trellis/config.yaml` so the doomed attempt is not made at all.

If you ever see that warning, note its wording — *"Trellis manages these
specific paths and they should be tracked"* — is misleading in this repository.
Do not act on it. Rule 1 above applies without exception.

### Setting up a private task repository (optional)

The task repository is not created by cloning the code repository. A developer
who wants backup and cross-machine sync wires it to their own private remote
once:

```bash
GIT_DIR=.trellis/.taskgit GIT_WORK_TREE=.trellis git init -b main
git --git-dir=.trellis/.taskgit config core.worktree ..
git --git-dir=.trellis/.taskgit config core.bare false
git --git-dir=.trellis/.taskgit config core.excludesFile /dev/null  # ignore global gitignore

cat > .trellis/.taskgit/info/exclude <<'EOF'
/*
!/tasks/
!/research/
!/.runtime/
!/.current-task
!/workspace/
/workspace/*
!/workspace/*/
EOF

git --git-dir=.trellis/.taskgit remote add origin <your-own-private-task-repo>
git --git-dir=.trellis/.taskgit fetch origin
git --git-dir=.trellis/.taskgit checkout -B main origin/main   # existing repo only
```

A shell alias removes most of the friction:

```fish
function tgit --wraps git; git --git-dir=(git rev-parse --show-toplevel)/.trellis/.taskgit $argv; end
```

## 2. Project state

Vibex is a Rust-first, local-first AI coding workbench, `0.1.0-rc.1`,
AGPL-3.0-or-later. Two product clients share one GPUI design system; the mobile
client has a separate WASM runtime and Capacitor host:

| Surface | Source | Stack |
| --- | --- | --- |
| Native desktop | `apps/desktop` | Rust + GPUI |
| Mobile runtime | `apps/mobile-wasm` | Rust + GPUI-WASM, Compact/Medium only |
| Mobile shell | `apps/mobile` | Capacitor 8 + bundled mobile runtime |

Layout: `apps/` (desktop, mobile-wasm, mobile, relay-server) · `crates/` (~25 Rust
crates: agent/ACP adapters, db, fs, git, terminal, relay, remote, content,
diagnostics, vibex-ui, vibex-terminal-ui, ...) · `scripts/` (Node evidence and
gate scripts) · `.trellis/` (workflow, spec, tasks).

The pnpm workspace covers `apps/*` only; there is no top-level `packages/`.

Architectural invariants that constrain most changes — read
`.trellis/spec/guides/architecture-baseline.md` before anything spanning
desktop / mobile / remote:

- `apps/desktop` is the sole source of visual, interaction, and
  information-architecture truth. Mobile derives from it.
- The PC `DesktopRuntime` is the only authoritative state owner; mobile code is
  a network client.
- Shared UI lives in `crates/vibex-ui`; `apps/desktop` is never compiled to WASM
  as a whole.
- Relay is transport, not a second database or state authority.
- There is no WebUI/PWA product. `apps/mobile-wasm` is bundled into Capacitor;
  its browser host exists only for local development and automation. Relay and
  Desktop packages never host these assets.

Toolchain: Rust 1.97.0 (pinned by `rust-toolchain.toml`), edition 2024,
Node.js 22, pnpm 11.3.0 (pinned by `package.json`).

## 3. Development guide

Run everything from the repository root.

```bash
corepack enable && pnpm install --frozen-lockfile

pnpm dev:desktop            # cargo run -p vibex-desktop --locked
pnpm dev:mobile-wasm        # mobile runtime in a local development host

pnpm check:rust             # Rust quality gate
pnpm check:frontend         # per-package typecheck + eslint
pnpm lint
```

`pnpm check` chains roughly thirty gates and takes a long time — treat it as a
pre-release gate, not an edit-loop command. During iteration run only the gates
covering what you touched.

This project uses an **evidence-based gate convention**: `capture:*` scripts run
a scenario and write an evidence artifact (`--write`), while the matching
`check:*` script re-runs it and fails when reality and the recorded evidence
diverge. Many also accept `--self-test`. When a `check:*` gate fails, fix the
code or re-capture deliberately — do not hand-edit evidence files.

`smoke:*` targets exercise real subsystems (`smoke:db`, `smoke:git`,
`smoke:pty`, `smoke:codex`, `smoke:claude`, `smoke:relay:local`); `e2e:*` and
`package:*` cover regression harnesses and Linux packaging.

Layer guidelines live in `.trellis/spec/backend/index.md` and
`.trellis/spec/frontend/index.md`. Read the relevant index — and its
Pre-Development Checklist and Quality Check sections — before writing code in
that layer.

## 4. Working with Trellis here

`.trellis/workflow.md` is authoritative for the three-phase workflow
(Plan → Execute → Finish); this section only records what is specific to this
repository.

- Initialize identity once: `python3 ./.trellis/scripts/init_developer.py <name>`.
  This creates `.trellis/.developer` (local only, never committed anywhere) and
  `.trellis/workspace/<name>/` (private repository).
- Task creation requires user consent, and consent to create a task is not
  consent to start implementing.
- Planning artifacts, research output, and journals are **private**. Spec updates
  are **public**. A single task therefore normally touches both sides.
- Phase 3.3 (spec update) writes into `.trellis/spec/` — public. Before promoting
  research into spec, strip local absolute paths, third-party source excerpts,
  and anything unreleased.
- Phase 3.4 (commit) applies the boundary rules in §1: draft the public commit
  from code changes plus spec updates, and commit task artifacts separately via
  `--git-dir` (or leave them on disk if you have no task repository).
- `/trellis:finish-work` archives the task and appends a journal entry. Both are
  private paths, so they never enter the public commit; commit them in the task
  repository if you have one. `session_auto_commit` is off, so no script will try
  to stage them for you.

## 5. Quick reference

```bash
# public repository
git status
git add <paths> && git commit -m "..."

# private task repository (same files, different repository)
git --git-dir=.trellis/.taskgit status
git --git-dir=.trellis/.taskgit add -A
git --git-dir=.trellis/.taskgit commit -m "..."

# leak guard — must print nothing
git diff --cached --name-only \
  | grep -E '^\.trellis/(tasks/|research/|\.runtime/|\.current-task$)|^\.trellis/workspace/[^/]+/'
```
