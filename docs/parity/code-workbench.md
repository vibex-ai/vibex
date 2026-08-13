# Code Workbench Gate

The code workbench gate covers the current GPUI Files, editor, Preview, Git,
diff, and Markdown surfaces. It defines the current regression contract for
those features.

## Covered Behavior

- Workspace-scoped file trees, search, selection, mutations, drag/drop, retry,
  and generation fencing.
- File, Git diff, commit, and Terminal targets with tab, split,
  resize, focus, close, and persistence behavior.
- Independent editor buffers, revision-checked saves, encoding and line-ending
  handling, large-file guards, search/replace, undo/redo, and IME input.
- GFM Markdown rendering with bounded workspace, HTTP, data, image, math, and
  diagram handling.
- Git status, history, stage/unstage, revert, commit, branch, blame, push/fetch,
  and worktree projections through typed backend operations.
- Bounded rendering for large file trees and diffs, cache eviction, and repeated
  workspace/revision switching.

## Evidence

`docs/parity/evidence/code-workbench.json` stores the source-bound model,
performance, source-contract, and visual status. A model-only capture records
visual evidence as `pending`; it does not reuse screenshots from another source.
Full-capture screenshots live in `docs/parity/screenshots/current/code-workbench/`.

The checker rejects stale current-source identities, unbounded render contracts,
missing screenshots, invalid visual metrics, and sensitive workspace content.
Physical captures prove only the platform and viewport recorded by the evidence.

Run the gate with:

```bash
pnpm check:code-workbench
```

Create new evidence only on a suitable capture host:

```bash
pnpm capture:code-workbench
```

Refresh only the model, performance, and source contracts when no unlocked
physical Wayland session is available:

```bash
pnpm capture:code-workbench:model
```

This mode leaves the physical visual result pending.
