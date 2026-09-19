# Vibex v0.1.0-rc.5 Release Notes

- Released: 2026-09-20 · Range: `v0.1.0-rc.4...v0.1.0-rc.5` · 95 commits

---

## English

### Highlights

- **Goals are now part of the session, not a row in the transcript** — Vibex normalizes the goal surfaces ACP Agents already publish — the provider-neutral `_meta.goal` extension and Codex's legacy `_meta.codex.goal` namespace — into one shared model, saves every change as a goal timeline item, and pumps goal transitions that arrive outside a turn so they are no longer dropped. The goal moved out of the transcript into a fixed bar above the composer, stacked under the queued-message bar, showing the phase, the objective, and the provider's own stats, with Edit, Pause, Resume, and Clear offered only when the adapter advertises them. The pencil opens an inline editor that re-objectives through a goal control instead of the composer, and a blocked or usage-limited goal can now be cleared, where only active and paused goals offered Clear before (`f90d55e`, `b7496f5`, `cd33868`, `f9afb09`).
- **The preview panel can live in a window of its own, and full screen no longer swallows the workbench** — The panel header gained a pop-out control that rehosts the multi-tab panel in a detached window and a dock control that brings it back; the panel entity never changes hands, so every open tab, editor buffer, and terminal moves with it, and exactly one panel exists at a time. Closing the detached window hands the panel back to the workbench column instead of taking it down, and closing the panel closes its window. Full screen used to cover the whole shell and made the sidebar and the right rail disappear; it now gives the panel the workbench column, so both keep their place, their width, and their activity bar. A new Workbench setting decides where the preview opens — inline, which is the default, or in a window of its own (`ebf868d`, `e9737b5`, `a877bf9`).
- **Session search became the workbench command palette** — The sidebar's session-only search is rebuilt on the interface kit's command palette, and its trigger moved from one project's toolbar to the title bar, immediately before the runtime control, so Cmd-K opens it from anywhere. One query now answers over three groups: sessions (title and message content through the existing background index, opening the session and jumping to the message that hit), settings entries (opening Settings on the matching section with the row highlighted), and quick actions (real actions registered on the workbench, so each row shows its keybinding and stays reachable from the keyboard and the menus, with unmet preconditions disabled rather than inert). Escape clears a non-empty query on the first press and dismisses on the second (`f7c1ab3`, `7341c1b`, `c8f7f4b`).
- **Updates download themselves, install on quit, and explain what changed** — A new Automatic updates setting sits beside Update prompts; with it on, a release is downloaded as soon as it is found, and otherwise Vibex offers the update and waits for confirmation. The transient version toast became a persistent rounded panel below the title-bar arrow that shows download progress and the install action, with a footer that opens About, and a staged update installs on quit when you did not install it. About now shows the release's version, publication time, download size, and its release notes as rendered Markdown, and when nothing newer exists it falls back to the notes published for the installed version, so the card always answers what you are running or about to run (`0ed31c2`, `9b252a7`, `12605e3`, `642eefd`, `905c894`).
- **A queued message can steer the turn that is already running** — DeepSeek Harness 0.4.32 advertises `_meta.steering.supported` and accepts `_session/steering`, which injects a user message into the running turn; Vibex ignored both, so a queued message could only reach a busy Agent by cancelling the turn first. Vibex now records the advertisement, offers a Steer action only for Agents that negotiated it, and falls back to interrupt-and-resend for everyone else. The delivery is recorded on the user's timeline item (`prompt`, `steer`, `resend`), so the transcript explains why a turn stopped, and a turn is marked superseded once a later turn exists — its live status, running duration, and streamed-row extension stop while the rows it already produced stay visible and expanded (`b99920f`, `dbab0ba`, `b866300`).
- **The timeline stops paying for the whole session on every frame** — The timeline virtualizes by turn, so one virtual row is an entire turn, and an expanded turn rebuilt, laid out, and painted every one of its process rows on every frame; a session that ran for hours is one turn with hundreds of rows. Turning on "expand reasoning by default" made each reasoning row two Markdown views instead of one clipped line, which took the desktop app from 60 fps to 30, and to 12 while the layout settled. Expanded reasoning rows now keep one Markdown surface, projection caches are sized per expanded turn instead of per screen, the layout fingerprint is memoized, a turn's process section paints through a windowed run, and a folder's session rows are windowed instead of resolving every row against the folder's session list (`7c5aa8c`, `2dedcde`, `01c823c`, `40f51b8`).
- **Appearance ships twenty palettes, and the theme reaches every control** — Appearance went from ten built-in palettes to twenty: five new light (Rose Pine Dawn, Everforest Light, Ayu Light, One Light, Kanagawa Lotus) and five new dark (Rose Pine, Everforest Dark, Ayu Dark, Dracula, Kanagawa Wave). Each is authored in the shared token source's 67-role model, and each new palette's secondary text is walked along its own hue until it clears 4.5:1 on both the page and the muted plate. Separately, the interface framework reloads its own neutral palette on every theme change, so switches, segmented tabs, outline buttons, scrollbars, skeletons, and text selections had been painting its stock grey in any tinted theme; the theme bridge now fills every remaining framework token from the active theme (`1556492`, `d29c59e`, `5e2af95`, `a7b5fa2`).
- **The model provider editor was rebuilt around a fetched catalogue** — Discovery now fills a picker and configures nothing on its own: checking a row adds that model to the draft, unchecking releases it while keeping its declared capabilities so re-checking restores them, and hand-typed ids still join the chosen list. The picker is the only Model list; the second column shows the clicked row's editable fields instead of repeating the list with an Edit button on every row, and selection follows the model id so renaming keeps the column on the model you are editing. Stored provider keys are read back into the editor, masked, with an eye to reveal them, and an untouched field never rewrites the stored value (`4848f52`, `46f7990`, `8df655b`, `27ac399`, `a52e213`).
- **The composer remembers what you already sent, and edited files save themselves** — Up and Down in the composer now walk the selected session's earlier user messages, so a message can be re-sent without retyping; Up only starts from an empty composer, typing into a recalled message ends the walk so an edit is never replaced by a stale draft, and Down steps back toward the newest and past it restores the draft the walk started from. Edited buffers are written back without an explicit save — after a typing pause, 1000 ms by default, or when the editor or window loses focus — with a Manual mode that restores save-only editing and an adjustable idle delay in Workbench settings. The fullscreen composer also became editable edge to edge; it had kept the collapsed two-to-eight row cap, so everything past roughly eight rows was dead space (`f477b4c`, `65bd62b`, `fdc87bc`).

### New Features

**Goals**

- Goal state is normalized into one shared model and persisted as a goal timeline item, and goal transitions that arrive outside a turn are pumped instead of dropped; the ACP adapter parses the initialize advertisement, `session_info_update` goal decoding, `_session/goal`, and the legacy `_codex/session/goal_control` (`f90d55e`)
- A goal card with Pause, Resume, and Clear gating and localized labels ships on desktop and mobile (`f90d55e`)
- A fixed goal bar above the composer, stacked directly under the queued-message bar, shows the phase, the objective, and the provider stats, and the transcript no longer projects a goal row (`b7496f5`)
- The objective is editable inline in the bar; saving issues a `set` or `edit` goal control carried through the ACP control parameters and the mock agent (`cd33868`)
- A blocked or usage-limited goal can be cleared, not just an active or paused one (`f9afb09`)
- A `/goal` user message keeps its bubble and gains a ghost goal icon whose tooltip marks the message as a goal prompt (`b7496f5`, `cd33868`)
- The goal badge rides the bubble's lower-edge reaction region — the same pill a queued delivery already uses, goal badge inboard — so the body keeps its full content width and a wrapped line starts where an ordinary message's does (`23d25f8`)

**Updates**

- An Automatic updates setting beside Update prompts downloads a release as soon as it is found when on, and otherwise offers the update and waits for confirmation (`0ed31c2`)
- A persistent rounded panel below the title-bar arrow shows download progress and the install action, with a footer that opens About; a staged update installs on quit when you did not install it (`0ed31c2`)
- About shows the release version, publication time, download size, and localized release notes as rendered Markdown, with a status chip, a description, and the single valid next action (`9b252a7`)
- With nothing newer available, the same card shows the notes published for the installed version, and shows nothing when that release predates the notes asset (`642eefd`, `905c894`)
- The release pipeline stages `vibex-release-notes.md` from the tagged commit's `docs/operations/release-notes-v<version>.md` and fails the release when it is missing or carries no `## English` section; the desktop fetches it from the verified tag under a bounded size and a shorter timeout, and notes selection resolves the interface language with a fallback to generic Chinese and then English (`12605e3`)

**Preview window**

- Pop the multi-tab preview panel into a detached window and dock it back; every open tab, editor buffer, and terminal moves with the panel entity, and exactly one panel exists at a time (`ebf868d`)
- Closing the detached window hands the panel back to the workbench column instead of taking it down, closing the panel closes its window, and the detached window keeps Ctrl+S and go-to-line through the same editor actions the shell handles (`ebf868d`)
- Full screen gives the panel the workbench column, so the sidebar and right rail keep their place, width, and activity bar, and the rail keeps its resize handle (`e9737b5`)
- A Workbench setting chooses where the preview opens — inline, the default, or a window of its own; the header's pop-out and dock controls write the same preference, and a restored layout reopens the preview in the configured host once the runtime is ready (`e9737b5`)

**Global search**

- Cmd-K opens a workbench command palette from anywhere, triggered from the title bar immediately before the runtime control (`f7c1ab3`)
- One query answers over sessions (title and message content, opening the session and jumping to the hit), settings entries (opening Settings on the matching section with the row highlighted), and quick actions (real actions showing their keybinding and disabled when the precondition is unmet) (`f7c1ab3`)
- The palette renders its own query field at the large control size, draws rules between the session, settings, and quick-action groups, and fixes arrow-key highlight movement and Escape-clears-then-closes (`7341c1b`)
- Session search indexes and highlights conversation text only — user messages, agent answers, reasoning, and plans — and tints just the matched keyword, with the current match painted stronger and scrolled into view when stepping through hits (`c8f7f4b`)
- The palette content sits one gutter off its border, and the scrollbar no longer sits under session timestamps and keybinding hints (`baa227b`, `a4cec5b`)
- The command is called "Global search" everywhere, including the trigger tooltip and the shortcut table (`7341c1b`)

**Composer & sessions**

- Recall earlier user messages with Up and Down in the composer; the placeholder advertises the keys alongside the existing command, mention, and skill triggers (`f477b4c`)
- The fullscreen composer is editable edge to edge, while the collapsed composer keeps its two-to-eight row growth (`fdc87bc`)
- Autosave writes edited files back after a typing pause, 1000 ms by default, or on editor or window focus loss, with a Manual mode, an adjustable idle delay in Workbench settings, and a pending edit flushed when its tab closes (`65bd62b`)
- Queued messages that steered into or interrupted a turn are recorded on the user timeline item and rendered as a labelled reaction on the bubble edge — primary for steer, warning for resend — so the interrupted turn above is explained without relying on colour alone (`dbab0ba`, `b866300`)
- Native steering offers a Steer action only for Agents that advertised `_meta.steering.supported`, with interrupt-and-resend as the fallback for every other Agent and for a failed or `promptRequired` native attempt (`b99920f`)
- A session parked on an approval or elicitation shows a circled question-mark call to action in the warning colour, with a localized tooltip, in the session row and in the workspace row that aggregates it, instead of a loading spinner (`fe89fb8`)
- The scroll-to-bottom control floats over the timeline instead of shortening the viewport by its own height (`09b98e5`)
- The session preview rail numbers only turns anchored by a user message, so auto-continue turns fold into the turn that triggered them and a conversation with only continuation turns does not mount the rail (`130a70c`)

**Agent & ACP**

- DeepSeek Harness keeps its slash-command catalog across the prepared attachment window, and the composer shows a pinned pre-session catalog (`/status`, `/model`, `/compact`, `/goal`, `/permission`, `/plan`, `/feedback`) before a session exists (`aec42c2`, `dc23ae4`)
- The DeepSeek Harness `danger-full-access` permission switch no longer fails on every attempt (`da3bffc`)
- The DeepSeek Harness catalog pin moves to 0.4.32, the version that advertises steering (`b99920f`)

**Model provider editor**

- Pick models from a fetched catalogue: checking a row adds it to the draft, unchecking releases it while keeping declared capabilities, hand-typed ids still join the list, and the two panes sit side by side on a wide window and stack on a narrow one, each scrolling on its own (`4848f52`)
- The picker is the only Model list, the second column shows the clicked row's editable fields, a row the provider does not offer presents the command that adds it, and selection is tracked by model id (`46f7990`)
- Stored provider keys are read back into the editor, shown masked, and revealed by an eye; an untouched field never rewrites the stored value, and Clear is an explicit command (`8df655b`)
- The editor is rebuilt as panels with caption strips, a scrolling body, and their own foot, with one chip shape for the catalogue, interface, reasoning, and thinking-level groups (`27ac399`)
- The editor's surfaces are quieted: the identity and connection band keeps content height, panes are plain columns, rows carry state with one wash instead of filled pills, and Test connection joins Cancel and Save on the trailing edge (`a52e213`)

**Pairing & remote**

- A Paired devices mode in the Connect-mobile-device dialog shows status, permission, last seen, revoke with confirmation, refresh, counts, and an empty state; the registry loads when the dialog opens and reloads after a pairing or revoke (`3901a8a`)
- The paired-device list pages six devices at a time, renders the pager only when the registry pages, shows a "13-18 of 24" caption, and clamps the page when a revoke empties it (`d26c2de`)

**Settings & appearance**

- Settings pages declare labelled groups, each with a small label, an optional full-width control, and its own panel, with wider space between groups than inside one (`c3b9182`)
- Appearance mode is chosen with preview cards that paint a miniature workbench in the palette each mode resolves to; system splits the card between both slots, the selected card keeps a primary border and name, and the cards are radio buttons with an accessible name (`5e2af95`)
- Light theme and dark theme became dropdowns showing the swatch and the theme's name in both the closed control and the menu (`a7b5fa2`)
- Ten more built-in palettes, bringing the total to twenty (`1556492`)
- Settings shortcuts render as keycaps, with the raw text kept as a fallback for a keystroke the interface toolkit refuses to parse (`a262799`)
- Every shortcut label and group is localized to the active language, and the string table now carries the locale it was resolved for instead of reaching for a process-wide locale (`f1367ec`)
- Agent notification toggles moved from General into Session settings (`ca1eec1`)
- Star project directories in the directory picker: a starred folder joins the Places rail as its own group, newest star first and bounded to twelve, and starring again from the row or the rail removes it (`16f798b`)

**Workbench & editor**

- The commit detail tab shortens its label to 48 characters with the whole subject in a tooltip, clamps the message body to three lines with a click and keyboard expand toggle, replaces the header badge with a summary row (changed-file count, total additions and deletions, side-by-side, wrap, and collapse-all toggles), stands file rows at 36px, starts every changed diff row with a colour rail, and projects the patch into aligned side-by-side rows in the model so the split view keeps the same virtualization and word-diff cache (`20455d4`)
- Loading surfaces draw skeletons at the row height the real list already uses instead of a spinner or a misleading empty state, with the gates pinned in a source contract (`96af670`)
- Empty states across desktop and mobile use the kit's empty-state component instead of seven hand-rolled helpers and a dozen inline blocks (`695c1ae`)

**Developer experience**

- Remote Access hints and settings operation results are pushed through the kit's notification component, with tone following the result and each family having its own id so a newer hint replaces the older one (`4bba017`)
- Hints paint above dialog backdrops, deferred in the gap between the dialog band and the popup band (`a254439`)
- The collapsed thinking label uses the kit's shimmer text, keeps the previous 1.2 s sweep and 0.42 spread, and stays static under reduced motion (`6b818e5`)

### Fixes

**Desktop — layout, chrome, and readability**

- The settings dialog and the command palette keep their content inside their rounded corners, since the interface toolkit clips children to a square; both round the content surface themselves, one pixel inside the border radius (`e5e0092`, `85c3f37`)
- The command palette's rows, query field, and footer sit one gutter off the border, and its scrollbar lane is reserved inside two-line rows so timestamps and keybinding hints are no longer under the thumb (`baa227b`, `a4cec5b`)
- The commit message body actually collapses to three lines: the clamp budgeted each newline-separated line separately, so a body of paragraphs rendered in full; the preview now projects leading logical lines and drops the repeated subject (`2d4ebfe`)
- The file preview header is flattened and uses a pencil edit icon (`5bf7818`)
- Preview tab actions get standard small icon-button geometry so hover paints a rounded rectangle, and single-click preview tabs use the bundled sans italic face (`8d49875`)
- The usage range control matches the segmented bars — a filled track, a raised page-coloured pill, and no outlines (`7910b20`)
- The settings font selects are backed by a searchable list, so typing in the popup actually narrows the family list instead of accepting typing and doing nothing (`5a8f729`)
- The shortcut dialog's input stays editable across repaints; the input entity was created inside the dialog builder, which is re-evaluated on every repaint, so its focus handle and text were replaced while typing, and a rejected chord now shows its reason inside the dialog (`1b0d087`)
- Switching sessions no longer strands a composer: the fork's window-wide action lock was released only when no session had been announced, so the normal path left every send button loading and the new-session home without runtime controls until restart; the lock is now released unconditionally when the fork settles, and the same rule was applied to the other takers (`a25c532`)

**Desktop — sessions, sidebar, and timeline**

- A streaming process unit alternated between its measured height and the estimate on every frame because its content revision advances with every chunk, which made a running session's timeline bounce when following its bottom; a streaming unit never shrinks below its last measured height (`1115b36`)
- The windowed process run's two loose ends are wired: the shared unit walk reads the display preference through the pairing flag it computed, and the virtual list's turn renderer is handed the turn index the run resolves its turn by (`1999ed2`)
- The sidebar group rows are disclosure controls again: project and workspace rows collapse and expand without switching focus, undoing the session-switch-on-group-click behaviour added earlier in this same range (`72b798d`)
- Clicking a sidebar project or workspace row no longer switches sessions, while the New Session home keeps activating the workspace directly (`72b798d`)

**Desktop — agent turn presentation**

- The delivery mark for steered and resent messages moved from an inline logo in the bubble body to an icon-only ghost button in the bubble's lower-edge reaction region, tinted on the glyph so the hue survives hover, with the label kept in the tooltip and the accessibility name (`b866300`)
- A session waiting on an approval or elicitation shows a call-to-action glyph instead of the progress spinner, and the error state is no longer hidden by an optimistic turn (`fe89fb8`)
- Auto-continue turns no longer consume session-preview rail slots or preview a bare turn number above an error (`130a70c`)

**Agent & ACP**

- State-only session updates, including available-command and config-option updates, now apply at their exact fence even while an attachment is still prepared; losing the DeepSeek Harness catalog left the committed attachment reporting an authoritative empty catalog, which suppressed the pre-session fallback and hid the composer's slash commands for as long as that attachment stayed current (`aec42c2`)
- Config-option updates no longer reach a prepared attachment: the DeepSeek Harness bridge publishes a config-option update before answering the mode switch, which bumped the runtime configuration revision and made the confirmation reject itself as stale, so every new DeepSeek Harness session's `danger-full-access` switch failed (`da3bffc`)
- Remaining goal tokens are clamped at zero, and wire tokens are folded only at a camelCase boundary so an all-caps `RESUME` stays one token instead of becoming `r_e_s_u_m_e` (`acc5026`)
- `GoalPhase` derives its default instead of carrying a manual implementation (`eebcaad`)
- An unused goal-card binding and an unused goal-phase import were dropped (`3a6a38a`, `f35bddc`)

**Markdown & rendering**

- A streaming Thought painted its parse-pending fallback through the code renderer, so every reasoning row appeared as a bordered code card with a copy button and then snapped into prose; literals now render as selectable wrapped text and estimate height the same way, matching mobile (`c5f1364`)

**Theme**

- The interface framework's own neutral palette is filled in from the active theme, so tinted appearances no longer paint switches, segmented tabs, outline buttons, scrollbars, skeletons, and text selections in stock grey (`d29c59e`)

**Localization**

- A bare Latin "Worktree" no longer sits beside a translated option in the new-session location control; it is now translated in both Chinese locales (`34585b4`)
- Shortcut labels are no longer hardcoded English, which had shown mixed-language copy in the shortcuts page, settings search, and palette quick actions at once (`f1367ec`)

**Build, release, and dependencies**

- The release publish retries up to three times, clearing any leftover release between attempts: `gh release create` stages a draft and uploads assets concurrently, and its transport retry reused an asset name without deleting the interrupted upload, so GitHub answered HTTP 422 and gh rolled the whole draft back — the rc.4 run failed exactly there on the Android arm64 APK and left no release behind (`e115770`)
- `brace-expansion` was bumped to 1.1.21 and 5.0.12 for its denial-of-service advisories; the 5.x line's engine requirement moves to node 20 or 22 and newer (`967ba44`)
- `js-yaml` was bumped to 4.3.2 for CVE-2026-84375 (`2fb3c5d`)

### Performance

- **Expanded reasoning rows** — With "expand reasoning by default" on, the desktop app ran at 30 fps instead of 60, and dropped to 12 while the layout settled. An expanded reasoning row now keeps one Markdown surface, since the disclosure header is a single clipped line of styled text and the second view bought a second element tree, keyed state entry, selection buffer, and hitbox per row; projection caches are sized per expanded turn instead of per screen, because the old 32-entry table missed on nearly every row with hundreds of rows and re-copied each body into a fresh allocation; and the layout fingerprint is memoized against a cheap turn-level shape key (`7c5aa8c`)
- **Markdown view state** — The selection buffer concatenated the whole document into one string every frame, plus a formatted element id per text segment, whether or not anything was selected; it is now materialized on demand, and segment ids come from a prefix built once per view. Block virtualization thresholds sat at 24 blocks or 16 KB with 8 blocks, so a typical reasoning row always took the full-render path; they now sit where virtualizing pays for itself. A non-streaming document up to 16 KB was parsed on the main thread when its view was created, which stalled the single frame that creates one view per reasoning row; thoughts now take the background-parse path past a small budget, while answers and document previews keep the larger one (`2dedcde`)
- **Turn process rows** — One virtual row is a whole turn, so an expanded turn built and painted every process row on every frame; a turn's process section now paints through a windowed run, reserving the height its units occupy and building only the ones the viewport shows. The run and the height estimator share one walk, measured unit heights are kept per unit id and revision, and the find bar's pending match is built even when it falls outside the window so it can still be scrolled to (`01c823c`)
- **Sidebar session rows** — A folder holding hundreds of sessions built, laid out, and painted every row on each frame, and every row resolved its session by rescanning the folder's session list, so frame cost grew with folder size rather than with rows on screen. Each contiguous band of session rows now renders through one element that builds only the rows the viewport can see; rows resolve by index into the cached projection, the selected row is built even when scrolled out, and the legacy folder migration returns early when no folder is legacy and indexes workspaces once (`40f51b8`)

### Under the hood

- Empty states moved to the kit's empty-state component, preserving call-site signatures, copy, padding, gaps, and alignment (`695c1ae`); hints moved to the kit's notification component (`4bba017`); the collapsed thinking label moved to the kit's shimmer text, and the desktop shimmer constant was renamed because the startup wordmark is now its only other consumer (`6b818e5`); the usage charts were rewritten on the kit's plots, moving 746 lines into a dedicated module (`40a5a7f`); the model provider editor became panels and chips (`27ac399`); the selected model's settings open beside the picker (`46f7990`); agent notification toggles moved into Session settings (`ca1eec1`); the directory picker's footer hint was dropped (`92926b4`)
- The preview window code was made clippy- and rustfmt-clean, and its keep-alive observer field renamed so clippy stays quiet (`ea96106`, `0393a74`)
- Specs were updated for state-only ACP updates at the prepared fence and native steering, the release-notes asset and About behaviour, the provider configuration contract, GPUI dialog input lifetime rules and the user-message delivery mark, composer queue pause semantics, and device management as a local operation beside remote-access setup
- The release workflow gained a step that stages the tagged commit's release notes and fails the release when the file is missing or has no English section, and the release checker and runbook were updated for the same contract (`12605e3`); the publish-retry recovery is pinned in the release contract (`e115770`)
- Dependencies were bumped for security: `brace-expansion` to 1.1.21 and 5.0.12 (`967ba44`), `js-yaml` to 4.3.2 for CVE-2026-84375 (`2fb3c5d`), and the DeepSeek Harness ACP catalog pin to 0.4.32 (`b99920f`)
- The repository now ignores patch and merge backup files, so a stray `.orig` or `.rej` copy of a source file can never reach the public repository (`a80a5ce`)
- Tests were added or extended for the preview panel host, paired-device paging and clamping, sidebar row windowing and reserved flow height, composer queue and turn-completion probes, the locale invariant that no Latin letters survive outside the product name in either Chinese locale, full coverage of the interface framework's theme tokens, the skeleton gates, and the session-switch action lock (`2759f32`, `d26c2de`, `40f51b8`, `b1b8e74`, `60bd848`, `f1367ec`, `d29c59e`, `96af670`, `a25c532`)
- Version numbers were bumped to `0.1.0-rc.5` across the workspace, the packaging inputs, the Android version fallback, the README and deployment documentation, and the reviewed first-party asset-bundle versions in the license policy

---

## 中文

### 亮点

- **目标（Goal）成为会话的一部分，而不再是记录里的一行** — Vibex 现在把 ACP Agent 本就发布的目标信息——通用的 `_meta.goal` 扩展和 Codex 旧的 `_meta.codex.goal` 命名空间——归一成同一套模型，每次变化都存成一条目标时间线条目，并且会把回合之外到达的目标状态变化推送出来，不再丢弃。目标从记录里移到输入框上方的固定栏，紧挨在排队消息栏下面，显示阶段、目标和 Agent 自己上报的统计；只有适配器明确声明支持时，才会出现编辑、暂停、继续和清除。铅笔图标会在原地编辑目标，并通过目标控制而不是输入框重新下发；被阻塞或额度用尽的目标现在也能清除，而以前只有进行中和已暂停的目标才有清除（`f90d55e`、`b7496f5`、`cd33868`、`f9afb09`）。
- **预览面板可以独立成窗，全屏也不再吞掉整个工作台** — 面板标题栏新增了弹出按钮，把多标签预览面板转移到独立窗口，停靠按钮则把它收回来；面板实体始终没有易主，所以打开的标签、编辑缓冲区和终端都会跟着走，并且同一时间只存在一个面板。关闭独立窗口会把面板交还工作台列，而不是把它关掉；关闭面板则会关掉它的窗口。以前全屏会盖住整个外壳，侧栏和右侧栏都会消失；现在全屏把工作台列交给面板，两者都保留原来的位置、宽度和活动栏。工作台设置里可以选择预览打开的位置——默认内嵌，或独立成窗（`ebf868d`、`e9737b5`、`a877bf9`）。
- **会话搜索升级为工作台命令面板** — 侧栏原本只能搜会话的搜索框改为基于界面工具包的命令面板，入口也从某个项目的工具栏移到标题栏、紧挨运行时控件之前，因此在任何位置按 Cmd-K 都能打开。一次输入会同时给出三组结果：会话（标题和消息正文，走已有的后台索引，打开会话并跳到命中的那条消息）、设置项（打开设置并定位到对应分区、高亮那一行）和快捷操作（注册在工作台上的真实 Action，因此每行都会显示快捷键，键盘和菜单里也能触发；前置条件不满足时显示为不可用而不是无反应）。输入非空时按 Esc 先清空，再按一次才关闭（`f7c1ab3`、`7341c1b`、`c8f7f4b`）。
- **更新会自己下载、退出时安装，并说明改了什么** — 在「更新提示」旁边新增了「自动更新」设置：开启后一发现新版本就立刻下载，否则 Vibex 会先询问并等待确认。原本一闪而过的版本提示改为标题栏箭头下方常驻的圆角面板，显示下载进度和安装操作，底部还能打开「关于」；如果没有手动安装，已下载的更新会在退出时安装。「关于」现在会显示该版本的版本号、发布时间、下载大小，并把发布说明渲染成 Markdown；没有更新时则回退到当前已安装版本对应的发布说明，因此这张卡片总能回答你正在运行或即将运行的是什么（`0ed31c2`、`9b252a7`、`12605e3`、`642eefd`、`905c894`）。
- **排队消息可以「转入」正在运行的回合** — DeepSeek Harness 0.4.32 声明了 `_meta.steering.supported` 并接受 `_session/steering`，可以把用户消息注入正在运行的回合；Vibex 之前两者都忽略，排队消息只能先取消当前回合才能送达繁忙的 Agent。现在 Vibex 会记录这项声明，只对协商成功的 Agent 提供「转入」操作，其他 Agent 仍然回退为「打断并重发」。投递方式会记录在用户消息的时间线条目上（`prompt`、`steer`、`resend`），因此记录里能解释回合为什么中断；当后面已经存在更新的回合时，旧回合会标记为被取代——它的实时状态、运行时长和流式行扩展都会停止，但已经产生的行仍然可见并保持展开（`b99920f`、`dbab0ba`、`b866300`）。
- **时间线不再为整个会话逐帧买单** — 时间线按回合虚拟化，也就是说一个虚拟行就是一整个回合，而展开的回合每一帧都会重建、布局并绘制它的每一条过程行；跑了几个小时的会话就是一个拥有数百行的回合。打开「默认展开推理」后，每条推理行会变成两个 Markdown 视图而不是一行截断文本，桌面端因此从 60 fps 掉到 30，布局稳定前还会掉到 12。现在展开的推理行只保留一个 Markdown 视图，投影缓存按展开的回合而不是按屏幕大小分配，布局指纹也做了记忆化；回合的过程区改为窗口化绘制，文件夹的会话行同样窗口化，不再为每一行重新扫描文件夹的会话列表（`7c5aa8c`、`2dedcde`、`01c823c`、`40f51b8`）。
- **外观自带二十套配色，主题也终于覆盖到每个控件** — 外观从十套内置配色增加到二十套：新增五套浅色（Rose Pine Dawn、Everforest Light、Ayu Light、One Light、Kanagawa Lotus）和五套深色（Rose Pine、Everforest Dark、Ayu Dark、Dracula、Kanagawa Wave）。每套都在共享 token 源的 67 个角色模型里编写，新增配色的次要文字会沿自身色相调整，直到在页面和弱化底板上都达到 4.5:1 的对比度。另外，界面框架在每次切换主题时都会重新加载自己的中性色板，导致开关、分段标签、描边按钮、滚动条、骨架屏和文本选中的颜色在带色调的主题里一直是框架自带的灰色；现在主题桥会把框架剩余的每个 token 都用当前主题填满（`1556492`、`d29c59e`、`5e2af95`、`a7b5fa2`）。
- **模型服务商编辑器围绕拉取到的模型目录重建** — 探测到的模型现在只会填充选择器，不再自动写入配置：勾选一行会把这个模型加入草稿，取消勾选会移除它但保留已声明的能力，重新勾选即可恢复；手动输入的 id 也仍然可以加入列表。选择器是唯一的模型列表，右栏显示当前点选那一行的可编辑字段，而不是把列表再重复一遍并在每行放一个编辑按钮；选中状态按模型 id 跟踪，因此重命名后右栏仍然停在正在编辑的模型上。已保存的服务商密钥会以掩码形式回填到编辑器，并可用眼睛图标显示；没有改动过的字段不会重写已保存的值（`4848f52`、`46f7990`、`8df655b`、`27ac399`、`a52e213`）。
- **输入框记得你发过什么，编辑过的文件会自己保存** — 在输入框里按上下方向键可以翻出当前会话早先的用户消息，不用重新输入就能再次发送；上键只在输入框为空时开始，往翻出的消息里输入会结束这次翻阅，因此编辑不会被旧草稿覆盖，下键逐步回到最新，越过最新则恢复开始翻阅时的草稿。编辑过的缓冲区不再需要显式保存——停止输入一段时间（默认 1000 毫秒）后，或编辑器、窗口失去焦点时就会写回；工作台设置里提供「手动」模式恢复只手动保存，并可调整空闲延迟。全屏输入框也改为整宽可编辑；它此前仍沿用折叠状态下的两到八行高度上限，导致大约八行之后全是点不到的死区（`f477b4c`、`65bd62b`、`fdc87bc`）。

### 新功能

**目标**

- 目标状态归一成同一套模型并持久化为目标时间线条目，回合之外到达的状态变化会被推送而不是丢弃；ACP 适配器会解析初始化声明、`session_info_update` 中的目标、`_session/goal`，以及旧的 `_codex/session/goal_control`（`f90d55e`）
- 桌面端和手机端都有带暂停、继续、清除门控和本地化文案的目标卡片（`f90d55e`）
- 输入框上方的固定目标栏紧接在排队消息栏下面，显示阶段、目标和 Agent 统计，记录里不再投影目标行（`b7496f5`）
- 目标可以在栏内原地编辑；保存时通过 ACP 控制参数和 mock agent 下发 `set` 或 `edit` 目标控制（`cd33868`）
- 被阻塞或额度用尽的目标也能清除，不再只限于进行中和已暂停（`f9afb09`）
- `/goal` 用户消息保留自己的气泡，并带有一个幽灵目标图标，提示这是目标指令（`b7496f5`、`cd33868`）
- 目标徽标挂在气泡下边缘的反应区，和排队投递用的是同一枚胶囊、目标徽标靠内，因此正文保持完整宽度，换行位置也和普通消息一致（`23d25f8`）

**更新**

- 「更新提示」旁的「自动更新」设置在开启时一发现新版本就下载，否则先询问并等待确认（`0ed31c2`）
- 标题栏箭头下方的常驻圆角面板显示下载进度和安装操作，底部可打开「关于」；没有手动安装时，已下载的更新会在退出时安装（`0ed31c2`）
- 「关于」显示版本号、发布时间、下载大小和渲染为 Markdown 的本地化发布说明，并带有状态标签、描述和唯一可执行的下一步操作（`9b252a7`）
- 没有更新时，同一张卡片会显示当前已安装版本的发布说明；该版本早于发布说明机制时则不显示（`642eefd`、`905c894`）
- 发布流水线会把标记提交里的 `docs/operations/release-notes-v<version>.md` 暂存为 `vibex-release-notes.md`，文件缺失或没有 `## English` 段落时直接让发布失败；桌面端在限定大小和更短超时下从已验证的标签获取它，并按界面语言选择段落，依次回退到通用中文和英文（`12605e3`）

**预览窗口**

- 把多标签预览面板弹出为独立窗口，也可以停靠回来；打开的标签、编辑缓冲区和终端都跟着面板实体走，同一时间只存在一个面板（`ebf868d`）
- 关闭独立窗口会把面板交还工作台列而不是关掉它，关闭面板则会关掉窗口；独立窗口通过外壳同一套编辑器操作保留 Ctrl+S 和跳转到行（`ebf868d`）
- 全屏把工作台列交给面板，侧栏和右侧栏保留位置、宽度和活动栏，右侧栏也保留自己的拖拽调整宽度手柄（`e9737b5`）
- 工作台设置决定预览打开的位置——默认内嵌，或独立成窗；标题栏的弹出和停靠按钮写入同一项设置，恢复布局时会在运行时就绪后按配置重新打开预览（`e9737b5`）

**全局搜索**

- 在任何位置按 Cmd-K 都能打开工作台命令面板，入口位于标题栏、紧挨运行时控件之前（`f7c1ab3`）
- 一次输入同时检索会话（标题和消息正文，打开会话并跳到命中处）、设置项（打开设置并定位分区、高亮对应行）和快捷操作（真实 Action，显示快捷键，前置条件不满足时不可用）（`f7c1ab3`）
- 面板使用大号控件尺寸绘制自己的输入框，在会话、设置和快捷操作三组之间画分隔线，并修正了方向键高亮移动和 Esc 先清空再关闭的行为（`7341c1b`）
- 会话搜索只索引和高亮对话文本——用户消息、Agent 回答、推理和计划——并且只给命中的关键词着色，当前命中颜色更重，逐条跳转时会滚动到可见位置（`c8f7f4b`）
- 面板内容与边框之间留出一个间距，滚动条也不再压在会话时间和快捷键提示上（`baa227b`、`a4cec5b`）
- 该命令在入口提示和快捷键表中统一称为「全局搜索」（`7341c1b`）

**输入框与会话**

- 在输入框里用上下方向键翻出早先的用户消息；占位提示会在已有的命令、提及和技能触发词旁标出这两个键（`f477b4c`）
- 全屏输入框整宽可编辑，折叠状态的输入框仍保持两到八行的增长范围（`fdc87bc`）
- 自动保存会在停止输入一段时间（默认 1000 毫秒）后，或编辑器、窗口失去焦点时写回编辑过的文件；提供「手动」模式、工作台设置里可调的空闲延迟，并在关闭标签时冲刷尚未写回的编辑（`65bd62b`）
- 转入或打断回合的排队消息会记录在用户时间线条目上，并在气泡边缘显示带文字说明的反应标记——转入为主色，重发为警告色——因此上方被打断的回合不靠颜色也能看懂（`dbab0ba`、`b866300`）
- 原生转入只对声明了 `_meta.steering.supported` 的 Agent 提供「转入」操作，其他 Agent 以及转入失败或需要提示的情况一律回退为打断并重发（`b99920f`）
- 停在权限确认或追问上的会话不再显示加载转圈，而是在会话行和汇总它的工作区行上显示警告色的圆圈问号行动提示，并带有本地化提示文字（`fe89fb8`）
- 滚动到底部的控件改为浮在时间线上方，不再用自己的高度压缩可视区域（`09b98e5`）
- 会话预览导轨只为带用户消息的回合编号，自动继续的回合会并入触发它的回合，只有继续回合的对话不会挂载导轨（`130a70c`）

**Agent 与 ACP**

- DeepSeek Harness 在预备附件窗口期间不再丢失斜杠命令目录，会话建立前输入框也会显示固定的预置目录（`/status`、`/model`、`/compact`、`/goal`、`/permission`、`/plan`、`/feedback`）（`aec42c2`、`dc23ae4`）
- DeepSeek Harness 的 `danger-full-access` 权限开关不再每次尝试都失败（`da3bffc`）
- DeepSeek Harness 的目录版本固定到 0.4.32，也就是声明支持转入的版本（`b99920f`）

**模型服务商编辑器**

- 从拉取到的目录中挑选模型：勾选一行加入草稿，取消勾选会移除但保留已声明的能力，手动输入的 id 也能加入列表；宽窗口下两栏并排，窄窗口下上下堆叠，各自独立滚动（`4848f52`）
- 选择器是唯一的模型列表，右栏显示点选行的可编辑字段，服务商不提供的模型会给出添加它的命令，选中状态按模型 id 跟踪（`46f7990`）
- 已保存的服务商密钥以掩码回填，可用眼睛图标显示；未改动的字段不会重写已保存的值，「清除」是显式命令（`8df655b`）
- 编辑器重建为带说明条的面板、可滚动主体和各自的底栏，目录、接口、推理和思考级别分组共用同一种标签样式（`27ac399`）
- 编辑器的表面更安静：身份与连接区保持内容高度，各栏改为普通列，行状态用一层浅色而不是实心胶囊表达，「测试连接」与取消、保存一起排在尾部（`a52e213`）

**配对与远程**

- 连接手机设备对话框新增「已配对设备」模式：状态、权限、最后在线时间、带确认的吊销、刷新、数量统计和空状态；对话框打开时加载注册表，配对或吊销后重新加载（`3901a8a`）
- 已配对设备列表每页六台，只有注册表分页时才显示翻页器，并显示「第 13-18 台，共 24 台」的说明；吊销导致当前页为空时会自动收拢页码（`d26c2de`）

**设置与外观**

- 设置页可以声明带标签的分组，每组有小标题、可选的全宽控件和自己的面板，组与组之间的间距大于组内（`c3b9182`）
- 外观模式用预览卡片选择，卡片会用该模式解析出的配色画一个微缩工作台；跟随系统会把卡片分成两半，选中卡片保留主色边框和名称，卡片本身是可访问的单选按钮（`5e2af95`）
- 浅色主题和深色主题改为下拉框，收起状态和菜单里都显示色块和主题名（`a7b5fa2`）
- 新增十套内置配色，总数达到二十套（`1556492`）
- 设置里的快捷键以键帽形式绘制，界面工具包无法解析的按键仍保留原始文本作为回退（`a262799`）
- 所有快捷键名称和分组都会本地化到当前语言，字符串表现在会带上解析时使用的语言，而不是去读进程级语言设置（`f1367ec`）
- Agent 通知开关从「通用」移到「会话」设置（`ca1eec1`）
- 目录选择器可以给项目目录加星：加星的文件夹会作为独立分组出现在「位置」栏，最新的在前、最多十二个，在行内或栏内再次点击即可取消（`16f798b`）

**工作台与编辑器**

- 提交详情标签的标题截断到 48 个字符，完整标题放进提示；提交正文折叠为三行并支持点击和键盘展开；标题徽标改为摘要行（变更文件数、总新增与删除、并排、换行、全部折叠）；文件行高改为 36 像素；每条变更的 diff 行以颜色竖条开头；补丁在模型中投影为对齐的并排行，因此分栏视图仍保留同样的虚拟化和词级 diff 缓存（`20455d4`）
- 加载中的界面改为按真实列表已有的行高绘制骨架屏，而不是转圈或误导性的空状态，判定条件固定在源码契约里（`96af670`）
- 桌面端和手机端的空状态改用工具包的空状态组件，替换掉七个手写辅助函数和十几处内联实现（`695c1ae`）

**开发者体验**

- 远程访问提示和设置操作结果改为通过工具包的通知组件推送，语气跟随结果，每类提示有独立 id，新提示会替换旧提示（`4bba017`）
- 提示绘制在对话框遮罩之上，延迟到对话框层与弹层之间的间隙（`a254439`）
- 折叠的思考标签改用工具包的微光文字，保留原先 1.2 秒扫过和 0.42 扩散参数，并在减弱动态效果时保持静止（`6b818e5`）

### 修复

**桌面端——布局、窗口装饰与可读性**

- 设置对话框和命令面板的内容保持在圆角内：界面工具包会把子元素裁成方形，因此两者自己把内容表面也做成圆角，位置在边框圆角内一像素（`e5e0092`、`85c3f37`）
- 命令面板的行、输入框和底栏与边框之间留出一个间距，滚动条轨道预留在两行高的行内，时间和快捷键提示不再被滑块压住（`baa227b`、`a4cec5b`）
- 提交正文真正折叠为三行：原先的截断会按换行分别计算，导致分段正文整段显示；现在预览只投影开头的逻辑行，并去掉重复的标题（`2d4ebfe`）
- 文件预览标题栏改为扁平样式，编辑图标换成铅笔（`5bf7818`）
- 预览标签的操作按钮改为标准的小号图标按钮尺寸，悬停时画出圆角矩形；单击打开的预览标签使用内置的无衬线斜体（`8d49875`）
- 用量范围控件与分段条保持一致——填充轨道、抬起的页面色胶囊，没有描边（`7910b20`）
- 设置里的字体选择改用可搜索列表，在弹层里输入会真正缩小字体族列表，而不是接受输入却毫无反应（`5a8f729`）
- 快捷键对话框的输入框在重绘后仍可编辑：输入框实体原先在对话框构建函数里创建，而该函数每次重绘都会重新执行，导致输入过程中焦点句柄和文本被替换；被拒绝的按键组合现在也会在对话框内显示原因（`1b0d087`）
- 切换会话不再让输入框卡住：fork 的窗口级操作锁原先只在没有会话被宣告时才释放，正常路径下会让所有发送按钮一直转圈，新会话首页也失去运行时控件，直到重启才恢复；现在 fork 结算时会无条件释放该锁，其他获取方也套用同一规则（`a25c532`）

**桌面端——会话、侧栏与时间线**

- 流式过程单元会在实测高度和估算高度之间逐帧跳动，因为它的内容修订号每来一块数据都会前进，导致运行中的会话在跟随底部时时间线上下抖动；现在流式单元不会低于上一次实测高度（`1115b36`）
- 窗口化过程运行的两处遗留接线补上：共享的单元遍历通过它自己算出的配对标志读取显示偏好，虚拟列表的回合渲染器也会拿到该运行解析回合时使用的索引（`1999ed2`）
- 侧栏分组行重新成为展开／折叠控件：项目行和工作区行可以收起和展开而不切换焦点，撤销了本周期早些时候加入的「点击分组即切换会话」行为（`72b798d`）
- 点击侧栏的项目行或工作区行不再切换会话，新会话首页仍会直接激活工作区（`72b798d`）

**桌面端——Agent 回合呈现**

- 转入和重发消息的投递标记从气泡正文里的内联图标移到气泡下边缘反应区里的纯图标幽灵按钮，颜色加在字形上，因此悬停时色相不会丢失，文字说明保留在提示和无障碍名称里（`b866300`）
- 等待权限确认或追问的会话显示行动提示图标而不是进度转圈，错误状态也不再被乐观回合掩盖（`fe89fb8`）
- 自动继续的回合不再占用会话预览导轨的位置，也不会在错误上方预览一个孤零零的回合编号（`130a70c`）

**Agent 与 ACP**

- 只改状态的会话更新（包括可用命令和配置项更新）现在会在自己的确切围栏处生效，即使附件仍处于预备状态；此前丢失 DeepSeek Harness 目录会让已提交的附件报告一个权威的空目录，从而压掉会话前的回退目录，只要该附件仍然有效，输入框的斜杠命令就一直不显示（`aec42c2`）
- 配置项更新不再作用于预备状态的附件：DeepSeek Harness 桥在回答模式切换之前先发布配置项更新，抬高了运行时配置修订号，使确认逻辑把自己判为过期，因此每个新建的 DeepSeek Harness 会话的 `danger-full-access` 开关都会失败（`da3bffc`）
- 剩余目标 token 用零下限截断，线上 token 只在 camelCase 边界折叠，因此全大写的 `RESUME` 仍是一个 token，而不会变成 `r_e_s_u_m_e`（`acc5026`）
- `GoalPhase` 改为派生默认值，不再手写实现（`eebcaad`）
- 删除了未使用的目标卡片绑定和未使用的目标阶段导入（`3a6a38a`、`f35bddc`）

**Markdown 与渲染**

- 流式思考内容会把解析中的回退块交给代码渲染器，导致每条推理行先显示成带复制按钮的边框代码卡片，然后突然变成正文；现在字面块按可选中的自动换行文本渲染，高度估算方式也一致，与手机端对齐（`c5f1364`）

**主题**

- 界面框架自己的中性色板会用当前主题填满，带色调的外观不再把开关、分段标签、描边按钮、滚动条、骨架屏和文本选中画成框架自带的灰色（`d29c59e`）

**本地化**

- 新建会话位置控件里不再出现光秃秃的英文「Worktree」与已翻译选项并排；两种中文现在都会翻译它（`34585b4`）
- 快捷键名称不再硬编码英文，此前快捷键页、设置搜索和命令面板快捷操作会同时出现中英混排（`f1367ec`）

**构建、发布与依赖**

- 发布上传最多重试三次，并在两次尝试之间清理残留的 release：`gh release create` 会先建草稿再并发上传资源，而它的传输层重试会复用资源名却没有删除中断的上传，于是 GitHub 返回 HTTP 422，gh 把整个草稿回滚——rc.4 的发布正是在 Android arm64 APK 上这样失败，且没有留下任何 release（`e115770`）
- `brace-expansion` 升级到 1.1.21 和 5.0.12，修复拒绝服务公告；5.x 系列要求的 Node 版本改为 20 或 22 及以上（`967ba44`）
- `js-yaml` 升级到 4.3.2，修复 CVE-2026-84375（`2fb3c5d`）

### 性能

- **展开的推理行** — 打开「默认展开推理」时，桌面端从 60 fps 掉到 30，布局稳定前还会掉到 12。现在展开的推理行只保留一个 Markdown 视图：展开标题只是一行截断的样式文本，第二个视图却为每一行多买了一套元素树、带 key 的状态条目、选择缓冲区和命中区域；投影缓存按展开的回合而不是按屏幕大小分配，因为原先 32 条的表在数百行的会话里几乎每行都落空，还会把每段正文重新复制成新的分配；布局指纹也针对廉价的回合级形状键做了记忆化（`7c5aa8c`）
- **Markdown 视图状态** — 选择缓冲区每一帧都会把整篇文档拼成一个字符串，并且无论是否有选中内容，都为每个文本片段格式化一个元素 id；现在改为按需生成，片段 id 来自每个视图只构建一次的前缀。块虚拟化阈值原本是 24 块或 16 KB 配 8 块，导致典型的推理行总是走完整渲染路径；现在阈值调到了虚拟化真正划算的位置。不超过 16 KB 的非流式文档原先会在创建视图时于主线程解析，而每个推理行都要创建一个视图，于是卡住那一帧；现在思考内容超过很小的预算就走后台解析，回答和文档预览仍保留更大的预算（`2dedcde`）
- **回合过程行** — 一个虚拟行就是一整个回合，因此展开的回合每一帧都会构建并绘制所有过程行；现在回合的过程区通过窗口化运行绘制，预留这些单元占用的高度，只构建视口内可见的部分。运行和高度估算共用同一次遍历，实测单元高度按单元 id 和修订号保存；查找栏的待定命中即使落在窗口之外也会构建，因此仍能滚动过去（`01c823c`）
- **侧栏会话行** — 拥有数百个会话的文件夹每一帧都会构建、布局并绘制所有行，而且每一行都要重新扫描文件夹的会话列表来解析自己，导致帧开销随文件夹大小而不是屏幕行数增长。现在每一段连续的会话行通过一个元素渲染，只构建视口能看到的行；行按索引从缓存投影中解析，选中的行即使滚出视口也会构建；旧文件夹迁移在没有旧格式文件夹时提前返回，并只索引一次工作区（`40f51b8`）

### 底层改动

- 空状态改用工具包的空状态组件，保留调用点签名、文案、内边距、间距和对齐（`695c1ae`）；提示改用工具包的通知组件（`4bba017`）；折叠的思考标签改用工具包的微光文字，桌面端微光常量也重新命名，因为启动字标成了它唯一的其他使用者（`6b818e5`）；用量图表基于工具包的图表重写，746 行代码移入独立模块（`40a5a7f`）；模型服务商编辑器改为面板加标签（`27ac399`）；选中模型的设置移到选择器旁边打开（`46f7990`）；Agent 通知开关移入会话设置（`ca1eec1`）；目录选择器的底部提示被删除（`92926b4`）
- 预览窗口相关代码清理到 clippy 和 rustfmt 通过，保活观察者字段也重新命名以保持 clippy 安静（`ea96106`、`0393a74`）
- 规格文档更新了预备围栏处的只改状态 ACP 更新与原生转入、发布说明资源和「关于」行为、服务商配置契约、GPUI 对话框输入生命周期规则与用户消息投递标记、输入框排队暂停语义，以及设备管理作为远程访问设置旁的本地操作
- 发布工作流新增暂存标记提交发布说明的步骤，文件缺失或没有英文段落时让发布失败，发布检查脚本和运行手册也同步了该契约（`12605e3`）；发布重试恢复逻辑固定进发布契约（`e115770`）
- 依赖出于安全考虑升级：`brace-expansion` 升到 1.1.21 和 5.0.12（`967ba44`），`js-yaml` 升到 4.3.2 修复 CVE-2026-84375（`2fb3c5d`），DeepSeek Harness 的 ACP 目录固定到 0.4.32（`b99920f`）
- 仓库现在忽略补丁和合并备份文件，源码的 `.orig`、`.rej` 副本不会再有进入公开仓库的机会（`a80a5ce`）
- 新增或扩展的测试覆盖：预览面板宿主、已配对设备分页与收拢、侧栏行窗口化与预留流高度、输入框排队与回合完成探测、两种中文里除产品名外不出现拉丁字母的本地化不变量、界面框架主题 token 的完整覆盖、骨架屏判定条件，以及会话切换操作锁（`2759f32`、`d26c2de`、`40f51b8`、`b1b8e74`、`60bd848`、`f1367ec`、`d29c59e`、`96af670`、`a25c532`）
- 版本号统一升到 `0.1.0-rc.5`，覆盖工作区、打包输入、Android 版本回退值、README 与部署文档，以及许可证策略中已审核的第一方资源包版本
