# Code Workbench Contract

This page records the behavioral contract for the GPUI Files, editor, Preview,
Git, diff, and Markdown surfaces. It is the reference for what those features
must keep doing across refactors.

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

Regressions in these areas are caught by the workspace test suites
(`cargo test`) and the `smoke:*` targets; there is no separate capture-based
gate for this surface anymore.
