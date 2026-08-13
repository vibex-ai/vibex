# Component Guidelines

Vibex UI components must make Agent activity, local workspace state, and remote
approval flows readable across desktop and mobile. `apps/desktop` is the only
visual, interaction, and information-architecture baseline.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md), GPUI Desktop source, and
source-bound GPUI parity evidence.

React/Tailwind/shadcn/Radix sections retained below are historical evidence from
the deleted legacy apps, not maintenance contracts. Their paths, APIs, and test
instructions must not be used to build current GPUI or GPUI-WASM components.

## Layout Components

PC desktop uses a multi-panel workbench:

- Left navigation for projects and conversations.
- Central area for Agent chat, editor, terminal, or integrated workspace tabs.
- Right rail for files, Git, details, or contextual panels.
- Collapsible sidebar, file panel, Git panel, and terminal panel.
- Split panes and tab navigation.

Compact GPUI shells use single-task list-to-detail navigation:

- Host/device list.
- Project list.
- Session list.
- Session detail.
- Permission approval.
- Files, Git, terminal, Provider settings, and system settings.

Keep layout state explicit and serializable enough for persistence or restore.

### Top Bar Critical Actions

Top-bar actions that open core surfaces, such as mobile/Web pairing and
settings, must remain reachable at narrow desktop widths. Keep these controls
outside the shrinkable title/path/status content flow, usually by anchoring them
to the right side next to native window controls and reserving matching
right-side padding on the draggable/title region.

Do not hide critical top-bar actions with breakpoint-only classes such as
`hidden ... min-[860px]:flex`. Non-critical badges may hide first, but the
buttons that open pairing, settings, or other primary workbench controls should
stay visible.

Icon-only top-bar controls and right-rail activity/plugin buttons should expose
a shadcn/Radix tooltip whose text matches the button's accessible title or
`aria-label`. For right-edge activity bars, place tooltips on the left side so
they remain visible inside the workbench viewport. Keep these icon-button
tooltips subtle and arrowless; the label text is the affordance. Icon-button
tooltips should wait about one second before opening so rapid button scanning
does not create visual noise, and they should close immediately when the pointer
leaves the trigger rather than staying hoverable and covering adjacent buttons.

The same contract applies to native GPUI controls. A `gpui_component::Button`
with a visible `.label(...)` uses that label as its AccessKit name. An icon-only
button may use `.tooltip(...)` as the accessible-name fallback, and the rendered
button must project the resolved value through `aria_label`; giving the floating
tooltip itself `Role::Tooltip` does not name its trigger. When both are present,
the visible label wins so explanatory tooltip copy cannot replace the command's
short accessible name.

### GPUI Button hover ownership

The locked `gpui-component` `Button` renderer owns the enabled/unselected hover
style. Do not call GPUI's generic `InteractiveElement::hover` directly on a
`Button`: the caller populates the base element's hover slot, then `Button::render`
tries to populate the same slot and GPUI panics with `hover style already set`.
Use a built-in `ButtonVariants` style and shared theme tokens. Use `.on_hover(...)`
only when an event callback is required; it does not replace the visual hover
contract. If a genuinely new visual variant is needed, express and test it through
the component variant API or a dedicated semantic control instead of stacking a
second generic hover refinement.

Wrong:

```rust
Button::new("preview-pane-new")
    .ghost()
    .hover(|style| style.bg(cx.theme().background))
```

Correct:

```rust
Button::new("preview-pane-new").ghost()
```

Regression coverage must render the enabled, unselected control through a real
GPUI fixture because compile-only and model tests do not execute `Button::render`.
The Code Workbench source contract additionally rejects a direct hover override on
its preview-tab add button.

Production GPUI workbench roots must mount the component overlay hosts after the
main shell content. Append `Root::render_sheet_layer`,
`Root::render_dialog_layer`, and `Root::render_notification_layer` in that
stacking order from the root view that owns the window. Calling
`gpui_component::init`, constructing `Root`, or invoking `window.open_sheet`
alone is not evidence that the corresponding layer is rendered. Keep sheet
state in the window/root owner (`has_active_sheet`, `close_sheet`) so title-bar
buttons, Escape/outside close, and programmatic startup all observe one overlay.

Floating sidebars rendered with shadcn/Radix `Sheet` must account for nested
portaled overlays such as `DropdownMenu`. If a sidebar action menu portals its
content outside the Sheet subtree, the Sheet outside-interaction handler should
ignore interactions inside that menu content so opening or using the menu does
not collapse the sidebar drawer. Check both the wrapper event target and Radix
`event.detail.originalEvent.target`, and prefer `DropdownMenu modal={false}`
for action menus inside Sheet content to avoid competing modal focus layers.

Project/session sidebars may collapse session groups per project. Keep the
collapsed project ids in the workbench owner rather than inside one rendered
sidebar instance, so inline sidebar, floating drawer, and hover preview all show
the same expanded/collapsed state. A "collapse all" control should snapshot the
previous collapsed-id set before collapsing every project and restore that
snapshot on the next activation. Use recognizable expand/collapse iconography
for this global control rather than history or generic chevron symbols. Pinned
sessions should keep a persistent inline marker before the session title so the
pin state remains visible when the row action buttons are hidden.
Project headers in the session sidebar should display the project name only,
not the workspace root path, to keep the rail scannable. Project-header clicks
that expand/collapse sessions or activate a project are internal sidebar
interactions and must not auto-close a floating Sheet/sidebar; reserve automatic
drawer closure for explicit navigation actions that intentionally leave the
sidebar context.

When explicit navigation closes a hover-preview sidebar, suppress hover-driven
reopening for longer than the sidebar's exit transition. The closing panel stays
mounted during that transition, and residual pointer events from a slightly
dragged click can otherwise reverse the animation halfway through. Capture the
pre-navigation hover state before clearing it and use the shared suppression
path; docked sidebar navigation must keep its requested-open preference.

```rust
let hover_preview_was_open = self.sidebar_hover_preview_open;
// Apply navigation state before clearing the preview.
if hover_preview_was_open {
    self.suppress_sidebar_hover_preview();
} else {
    self.close_sidebar_hover_preview();
}
```

Regression coverage must assert both the requested-open/drawer result and that
hover-preview navigation enters the suppression path.

Panel resize handles should stay below floating sidebars, drawers, dialogs, and
other portaled overlays in z-index. The resize hot zone only needs to sit above
its owning panel content; using overlay-level z-index values can make resize
guides or hover zones bleed through floating sidebar previews.

Brand and product logos that are unavailable in lucide-react should render
through Iconify React in offline mode, using a checked-in subset of Iconify logo
data rather than hand-drawn local SVG approximations or runtime network icon
fetches. Prefer multicolor `logos:*` icons for products such as VS Code,
JetBrains IDEs, OpenAI, Claude, and Gemini; use `simple-icons:*` only when no
multicolor logo exists in Iconify. When extracting a local Iconify subset, keep
the source collection's root-level default `width` and `height` values as well
as per-icon overrides; missing collection dimensions can produce a too-narrow
SVG viewBox and visibly clip logos.

In GPUI, render SVGs with fixed embedded brand colors through `gpui::img`.
`gpui_component::Icon` paints SVGs as alpha masks and therefore collapses those
colors into one theme color. Reserve `Icon` for `currentColor` SVGs and other
intentionally monochrome glyphs.

GPUI new-session runtime selectors must follow the Tauri responsive contract.
Above `860px`, show the icon, selected value, and chevron; at `860px` and below,
use fixed `32px` icon-only triggers while keeping the selector name and current
value in the tooltip. Use the Provider database, current model brand, reasoning
brain, and conversation-mode shield icons in that order.

Non-compact runtime-selector dropdown triggers size intrinsically to the current
selected label. Do not assign per-selector widths or truncate the selected label;
keep the icon, no-wrap label, and chevron non-shrinking. Wrap the `Popover` in a
`flex_none` container so the composer row scrolls and the new-session row wraps
before any trigger is compressed. Applying `flex_none` directly to `Popover`
styles its overlay content rather than its rendered trigger wrapper.

When the composer runtime-selector row is narrower than its contents, keep it
horizontally scrollable without rendering a scrollbar. GPUI's x-only overflow
maps a regular mouse wheel's vertical delta onto the horizontal axis, so users
can move through the choices by scrolling directly over the row.

```rust
// Wrong: short values waste space and long values are clipped.
Button::new("runtime-selector").w(px(112.0)).child(truncated_label);

// Correct: the selected label contributes its full intrinsic width.
let trigger = Button::new("runtime-selector").px_2().child(intrinsic_content);
div().flex_none().child(Popover::new("runtime-menu").trigger(trigger));
```

## Timeline Cards

Render Agent activity through provider-neutral cards:

- User message.
- Agent message.
- Reasoning/thought.
- Plan step and Todo.
- Tool call.
- Command execution.
- File operation.
- Git diff update.
- MCP call.
- Permission request.
- Compact/context boundary.
- Subagent/task event.
- Delegation, Team, and automation run event.
- Error, warning, and system notice.

Cards that can grow large must be collapsible. Tool, diff, terminal, and plan
cards should support compact summaries for mobile.

Delegation and Team cards should show parent/child session links, role or slot,
current state, pending permission ownership, and latest result summary without
requiring components to read provider-native child-turn payloads.

## Preview Components

Generated result previews are part of the coding workflow, not a separate office
suite. Preview UI should support:

- Markdown, code/text, HTML, image, diff, logs, test reports, and common binary
  file summaries.
- Source/preview toggles where both representations matter.
- Multi-tab preview state on desktop.
- Snapshot/version history when the backend exposes it.
- Open in system app, download, copy path/content, and request-Agent-edit
  actions.

Mobile/Web previews should stay lightweight: read-only rendering, diff review,
history inspection, and explicit "ask Agent to modify" flows are preferred over
large embedded editors.

### Markdown Table Scrolling

When a Markdown table is wider than its viewport, keep a persistent
`ScrollHandle` for that table node and attach the overflow container and visible
horizontal scrollbar to the same handle. A predominantly vertical wheel gesture
over the table must move that handle horizontally and stop propagation while
the table has horizontal overflow, including at either horizontal edge. This
prevents the table and its scrollable page or timeline ancestor from moving at
the same time. If the table has no horizontal overflow, leave the event
unconsumed so normal page scrolling still works.

Cover this behavior with a real GPUI layout test: place a wide table inside a
parent with a non-zero vertical scroll range, dispatch a vertical
`ScrollWheelEvent` over a laid-out table cell, then assert that the table's x
offset changed and the parent's y offset remained zero.

### Empty States

Empty-state cards rendered inside right rails, preview tabs, file panels, Git
panels, and other resizable workbench panes must size against the available pane
width, not their content's intrinsic minimum width. Use `w-full min-w-0` on the
card/container plus normal text wrapping on title and description. Otherwise
narrow panes can collapse Chinese or long unspaced copy into one-character-wide
vertical columns.

### Diff Views

Diff line highlighting must define readable foreground colors for both light
and dark themes. Do not use light-only text classes such as `text-*-100` unless
they are guarded by a `dark:` variant; in light theme, added/deleted/hunk text
should use darker foregrounds while the background carries the status tint.

Diff summary counts must come from the same line-level edit script used to
render the preview. Do not treat everything between a shared prefix and suffix
as changed: separate edits with unchanged lines between them would inflate both
the added and removed counts. Preview context and truncation may bound rendered
rows, but must not change the full edit-script totals.

A height-capped GPUI diff must resolve that cap into a definite scroll viewport.
Give the stateful node that tracks the persistent `ScrollHandle` an explicit
bounded size, use GPUI overflow on that node, and attach both-axis scrollbars to
the same handle. Content-sized descendants inside an ancestor with only a
maximum height are not sufficient evidence that vertical wheel scrolling works.

When that diff viewport is nested inside a scrollable timeline, attach an
`on_scroll_wheel` handler to the same stateful node and call
`cx.stop_propagation()` unconditionally. GPUI applies the viewport's overflow
scroll before bubble listeners run, so the diff still moves while the timeline
does not. Leaving the event unconsumed makes both scroll handles move; consuming
it only when the diff is away from an edge introduces unwanted scroll chaining
at the first or last row.

Cover both axes with a real GPUI layout test: render content taller and wider
than the viewport inside a parent with a non-zero scroll range, dispatch
vertical and horizontal `ScrollWheelEvent` values inside the diff, and assert
the diff handle offsets, the parent offset remaining zero, and a late row's
changed bounds for the vertical path. Compile-only checks and source-string
assertions do not exercise GPUI layout, wheel routing, or event propagation.

### File Tree Icons

Desktop file trees, including the right rail file preview panel, should keep
file icon shape and file-type color in one descriptor map keyed by filename or
extension. Do not maintain parallel icon and color maps that can drift.

File-type icon colors should come from theme CSS variables, not raw Tailwind
color classes. Keep Git status, selected state, and ignored state readable as
separate row/text state; file-type color must not become the only status signal.

When a directory contains exactly one child and that child is also a directory,
desktop file trees may compact the chain into one row such as
`archive / 2026-06`. The compacted row should render the full loaded
single-directory chain, not a fixed number of segments. Each displayed segment
should remain independently clickable; selecting a segment should make the
following rows render from that directory as the temporary subtree root, while a
row-level toggle should expand/collapse every directory path in the displayed
chain together. Segment selection may add the clicked chain to expanded state
once, but derived effects that watch selected path or rendered rows must not
re-add chain paths after the user toggles them closed. Do not restrict
expand/collapse to only the folder icon or one segment button. Segment-specific
hover/focus affordances should apply only when the row displays a real compact
chain with more than one segment, and the hover state should remain visible for
as long as the pointer stays over the segment hit area. Prefer text emphasis or
a highlighted subtle underline rather than fading to a neutral gray; use
explicit pointer/focus state when CSS pseudo-class hover does not remain stable
in nested row controls. Avoid dark inline background chips or persistent blocks
that compete with the full-row selection state.
Git Changes trees should use the same compact directory-chain visual language
as the Files panel, with a single checkbox for the compacted directory row and a
row-level expand/collapse action that toggles every directory path in the chain
together.
Git Changes file-name status colors should reuse the Files panel status text
mapping where it does not conflict with Git semantics: unstaged new files are
red, staged added files use the added color, modified-like files use the
modified color, and deleted files keep a muted strikethrough state. Keep the
short status badge as the precise Git state indicator.
Git History commit-file lists should reuse the Git Changes tree renderer and
directory compaction instead of building a separate tree. Convert
`GitCommitDetail.files` into the same view-model shape used by Git Changes
rows, but keep history rows read-only: no staging checkboxes or working-tree
mutation affordances. When the history surface only needs the file list, request
commit detail with `includePatch: false` so selecting a commit does not load a
large patch unnecessarily.

Selected file rows should use a full-row highlight frame, not just text color,
so selection remains obvious when file-type icon colors and Git status colors
are also present.

Desktop file trees may support drag-to-move for files and directories through
the typed file rename/move mutation. Drop targets should be directories only,
including the workspace root, and must reject no-op moves, moving into the same
path, and moving a directory into one of its descendants. For compact directory
chains, dragging the row should move the first displayed segment so the whole
chain moves together, while dragging a hovered segment should move that specific
directory. Segments may also act as directory drop targets when doing so keeps
the target unambiguous. While dragging inside a scrollable file tree, hovering
near the top or bottom edge should auto-scroll the list so users can move items
beyond the currently visible rows. When a hovered directory drop target is
expanded, the visible rows in that directory subtree should receive a subtle
full-row range highlight in addition to the stronger target-row highlight, so
the destination scope is clear. Visible non-directory rows inside an expanded
directory may also accept drops by resolving the destination to their parent
directory, so users do not have to release exactly on the directory row after
the intended folder is already expanded. Do not make every draggable row or
segment show a persistent hand/grab cursor in its normal hover state; keep the
normal file-tree cursor calm and reserve grab-style cursor feedback for the
active drag state.

File tree typeahead should be scoped to currently rendered rows only. When the
file tree panel has focus and users type characters, show the current typed
buffer in the panel without truncating it, highlight visible files and visible
directory segments whose displayed names contain that exact continuous text,
and mark the matched substring characters with a subtle background. Keep that
substring highlight stable until the user deletes the entire typed buffer,
presses an explicit clear key such as Escape, or focus leaves the file tree
panel; do not clear it through a short inactivity timer. Do not search folded,
unloaded, or otherwise hidden descendants.

Desktop file trees should provide a right-click context menu on both rows and
empty file-tree space. Row context targets should act on that file, directory,
symlink, or compact-chain segment target; empty-space context targets should
act on the workspace root or currently shown root so users can create or paste
even in an empty directory. Keep filesystem mutations behind typed file hooks
and backend service commands instead of direct browser-only state updates.
File explorer panel headers may include compact workspace-level actions such as
a fullscreen toggle and an Open With dropdown. Fullscreen should enlarge only
the right-rail file panel surface and keep a clear exit affordance in the same
header. Header Open With actions operate on the workspace root and should show
stable system actions such as File Manager and Native Terminal plus only
backend-detected installed IDE/project tools; do not hard-code unavailable IDEs
in the UI, and do not pass arbitrary shell command strings from React. When
space allows, prefer a split button: the primary side shows the currently
selected tool icon only and directly opens the workspace root with that tool,
while the chevron side only opens the tool menu. Before the user selects a
tool, the primary side should show the same generic Open In icon used by the
menu entry.
Menus should include create file/folder, cut, copy, paste, copy relative path,
copy absolute path, copy file name, rename, delete, and Open In actions where
the current target supports them. Destructive delete still requires explicit
confirmation through an app-styled Dialog, not a native `window.confirm`, and
Open In must use desktop shell capabilities only after resolving the selected
relative path against the workspace root. The Editor Open In action must be
disabled for directories, symlinks with unknown targets, compressed archives,
media, fonts, executables, database files, Office/PDF documents, and other
known binary-like extensions such as `gz`. Default App and Native Terminal Open
In actions should call backend/Tauri file commands with the workspace-relative
path, not construct absolute filesystem paths in React; Native Terminal opens a
directory directly and a file's parent directory. Row and empty-space Open In
menus should also include backend-detected IDE/project tools, grouped separately
from fixed system actions, and call the typed open-with-tool command for the
current context target path. File rows should not show
persistent or hover-only trailing delete icons; keep destructive actions in the
context menu or editor chrome. New file/folder actions should insert an inline
pending row under the target directory with a focused name input; Enter or blur
commits the typed name, Escape cancels, and empty input cancels without
mutation. Guard the pending row against duplicate Enter/blur submission until
the create mutation settles, and prefer read-only/submitting styling over
disabling the input if disabling would trigger blur and re-enter the commit
path. Rename must use the same inline input interaction in the existing row or
compact-chain segment instead of a native prompt. Inline create/rename
validation or mutation errors should render beneath the pending input with
wrapping text so narrow right rails do not hide the message horizontally. When
inline rename is started from a Radix context menu, defer mounting/focusing the
input until after the context menu has closed; otherwise the menu's close-time
focus restore can blur the new input and commit the unchanged name, making edit
mode appear to flash and immediately exit. Directory rename must update every
visible file-tree cache entry whose path or parent path is the renamed directory
or one of its descendants, and must also copy expanded subtree query cache keys
from the old directory path to the new directory path; otherwise stale expanded
subtree data can make the folder appear to revert even though the filesystem
rename succeeded.
If the app has a global native `contextmenu` suppressor, it must skip
Radix/shadcn context menu triggers so Radix can receive the unmodified
right-click event and position the custom menu. While a row context menu is
open, the target row should use the same full-row selected frame as normal file
selection. If users right-click a different row while a context menu is already
open, the previous menu should be replaced by a newly positioned menu for the
new target.

Wrong:

```tsx
const iconByExtension = new Map([["md", BookOpenText]]);
const colorByExtension = new Map([["md", "text-blue-400"]]);
```

Correct:

```tsx
const iconByExtension = new Map([
  ["md", { icon: BookOpenText, tone: "markdown" }]
]);
```

## Rendering Performance for High-Frequency Workbench Panels

The desktop workbench owner component subscribes to high-frequency state
(terminal polling, git status, file tree, chat streams). Any panel it renders —
especially the compound preview panel, whose tab strip renders one Radix
`ContextMenu` tree per tab — must be isolated behind `memo` boundaries with
referentially stable props, or every unrelated state tick re-renders the whole
panel tree and the UI feels sluggish as tab count grows.

### Convention: memo boundaries need stable props end to end

**What**: Wrapping a component in `memo` is only half the contract. Every prop
passed to it (and to memoized children like `PreviewTabButton`) must be
referentially stable: callbacks via `useCallback`, derived arrays/maps/values
via `useMemo`, and constant fallbacks as module-level constants (e.g.
`const EMPTY_OPEN_TOOLS: FileOpenTool[] = []`), never fresh `[]`/`{}` literals
or inline arrows in JSX.

**Why**: A prior perf pass found `PreviewTabButton` was already `memo(...)` but
completely ineffective because the pane view passed six inline handler consts
and a per-tab `openTools.filter(...)` array — new identities every render.

**Wrong**:

```tsx
<PreviewTabButton
  detectedOpenTools={fileOpenPath ? openTools.filter((t) => t.kind === "ide") : []}
  onOpenTabInEditor={(tab) => { /* inline const or arrow, new identity per render */ }}
/>
```

**Correct**:

```tsx
const ideOpenTools = useMemo(() => openTools.filter((t) => t.kind === "ide"), [openTools]);
const handleOpenTabInEditor = useCallback((tab: PreviewTab) => { ... }, [onOpenFileForEdit]);

<PreviewTabButton
  detectedOpenTools={fileOpenPath && selectedWorkspaceId ? ideOpenTools : EMPTY_OPEN_TOOLS}
  onOpenTabInEditor={handleOpenTabInEditor}
/>
```

Per-item shared derived state (such as the closable-tab count feeding every
tab's context-menu enablement) must be computed once per pane with `useMemo`,
not rescanned inside each tab render.

> **Warning**: The repo ESLint config does not enable
> `react-hooks/exhaustive-deps`. Dependency arrays for new `useCallback`/
> `useMemo` hooks must be audited manually; list every component-scope reactive
> capture. React-query `mutation.mutate` and Zustand actions are stable and safe
> to depend on directly.

> **Warning**: Do not replace the per-tab Radix `ContextMenu` in the preview
> tab strip with a single shared context menu. That direction was tried and
> rolled back because it felt slower in desktop testing; do not reapply it
> without profiling evidence.

## Permission Components

Permission UI must show:

- Requested action.
- Provider/session/project context.
- Risk category.
- Details and diff/command preview where applicable.
- Available responses.
- Which device/user resolved it after completion.

Primary approve/deny actions must be reachable by thumb on mobile. Dangerous
actions need clear confirmation, especially terminal commands, file deletion,
Git revert, push, and native config export.

In the conversation timeline, an adjacent `Command` and command-risk
`PermissionRequest` from the same turn form one interaction. Render the command
once, put the approval actions in its footer, and let the pending permission
override any provisional `Running` label. Do not stack a second permission card
that repeats the same command.

Permission presentation is a user-facing projection, not a dump of provider
transport details. Parse structured input to recover actionable fields such as
the command, arguments, and working directory, while suppressing correlation,
routing, and response-option fields such as `requestId`, `toolCallId`, `tool`,
and `options`. A standalone permission card may retain additional clearly
user-facing details, but raw JSON and provider bookkeeping do not belong in the
message body.

When a turn is blocked on approval, both the command card and the pending-turn
footer must say that confirmation is awaited; neither may imply that execution
is already running. Resolved historical permissions keep their outcome and no
longer render response buttons.

## Provider Components

Provider UI must show Vibex Provider Profiles, not raw config files. Components
for runtime injection preview should display redacted env, headers, endpoints,
SDK options, CLI args, and temporary config overlay paths.

The Agent detail surface is a user-facing authentication and Profile-management
surface, not a diagnostics console:

- Render dynamically advertised Agent, environment, and terminal auth methods
  before the selected Agent's Provider Profiles.
- Keep method choices as separated rows within one authentication section; do
  not nest decorative method cards inside an outer card.
- Secret environment fields use masked inputs and expose only a configured
  marker plus an explicit clear action. Optional fields are labeled; exact
  Agent-provided environment names remain visible.
- The native Desktop Provider Profile API Key editor is the deliberate local
  exception: it may resolve the saved value through `DesktopRuntime`, place it
  in a masked `InputState`, and expose the standard eye toggle. Do not represent
  a configured key with a fixed `***` placeholder because the toggle would have
  no original value to reveal. The plaintext remains confined to that local
  editor state and the Secret mutation request.
- Render logout only when advertised. A terminal method opens the shared
  Terminal surface with a close action and explicit running/success/failure
  state.
- Do not render runtime verification, runtime-option snapshots, Provider
  projection internals, raw ACP payloads, or resolved authentication-method
  credential values in the Agent detail surface. The local Provider Profile
  editor exception above applies only to its explicit masked API Key control.

Native export UI must always include diff, backup, atomic write, and rollback
information.

## Styling

- Use semantic tokens from `crates/vibex-ui/theme/tokens.json` across native and WASM GPUI.
- Preserve dark mode as a first-class path.
- Use shared GPUI/gpui-component primitives; legacy React may keep shadcn/Radix until cutover.
- GPUI delete, remove, clear, and destructive close actions must use the shared
  `icons/vibex/trash-2.svg` glyph. Do not use `IconName::Delete`: the locked
  component library renders it as a backspace-style symbol rather than a trash
  can, which makes destructive actions ambiguous. Conditional actions must
  return `Icon` values from every branch:

  ```rust
  // Wrong: renders the backspace-style delete glyph.
  Button::new("delete-item").icon(IconName::Delete);

  // Correct: uses the shared destructive-action glyph.
  Button::new("delete-item").icon(Icon::default().path("icons/vibex/trash-2.svg"));

  // Correct: both conditional branches resolve to Icon.
  button.icon(if clearing {
      Icon::new(IconName::Undo2)
  } else {
      Icon::default().path("icons/vibex/trash-2.svg")
  });
  ```

  Desktop source-contract coverage must reject `IconName::Delete` in
  `apps/desktop/src/app.rs` and `apps/desktop/src/management.rs`.
- Text-bearing overlays such as dialogs, sheets, dropdown menus, selects,
  context menus, command palettes, and tooltips should avoid scale/zoom
  animations and backdrop blur. In Tauri/WebKit these effects can keep text on
  a composited layer and make glyphs look soft or fuzzy. Prefer opacity and
  slide-only transitions with opaque `bg-popover` surfaces. For centered
  dialogs, prefer `inset` plus auto margins over translating the content
  container with `transform`; transform-based centering can soften all text
  inside the dialog.
- Do not default to flat, generic layouts when implementing new screens. Follow the
  current GPUI Desktop workbench and its domain-component language.

## Accessibility

- Interactive cards need keyboard focus states.
- Collapsible cards need accessible expanded/collapsed state.
- Dialogs and command palettes must trap focus.
- Controlled dialogs whose trigger is rendered outside their Radix `Dialog` root must
  capture the opening element and restore it after Escape, Cancel, or close when that
  element is still connected. Do not prevent Radix close autofocus when the original
  trigger was removed by the completed action.
- Workbench dialogs that can open at narrow desktop widths should keep the
  shadcn/Radix `DialogContent` centered unless intentionally anchored, size
  against viewport margins such as `calc(100vw - 2rem)` and
  `calc(100vh - 2rem)`, and keep the outer content non-scrolling when possible
  so the built-in close button remains reachable.
- Long dialog forms should split their layout into a bounded field scroll area
  and a `DialogFooter` with `shrink-0`, so submit/cancel actions do not float in
  the middle of form content when the available height is narrow.
- Dialogs that use a `flex` column shell with `flex-1` body content and
  `overflow-hidden` must set an explicit viewport-bounded height, not only
  `max-height`; otherwise a transform-free `h-fit` dialog shell can collapse the
  body region and leave only the header visible.
- Approval buttons need labels that describe the action, not only icons.
- Terminal views need copy/select behavior that works without pointer-only
  interactions.

## Anti-Patterns

- Do not render raw JSON provider events as user-facing UI.
- Do not create separate Claude and Codex timeline component trees.
- Do not put destructive actions in icon-only buttons.
- Do not make mobile controls depend on hover.
