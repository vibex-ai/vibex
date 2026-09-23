# Vibex v0.1.0-rc.6 Release Notes

- Released: 2026-09-23 · Range: `v0.1.0-rc.5...v0.1.0-rc.6` · 96 commits

---

## English

### Highlights

- **One workbench, several sessions at once** — Session groups bind a worktree's sessions into one sidebar row and one split workspace, where every pane keeps its own conversation, composer and tabs. Drag sessions in and out, drop a tab on a pane edge to split, and read a collapsed group's status at a glance.
- **MCP and Skill marketplaces** — Browse and install MCP servers and Skills from the config center. The MCP catalog is indexed locally, so search is instant and covers the whole registry, and an install reaches every Agent that can host it.
- **Notifications you can act on** — Desktop notifications use the platform channel gpui provides, stay out of the way while the window is in front, and bring the workbench back on the session that raised them when clicked.
- **Runtimes and devices are named and manageable** — Both default to the machine's device name and can be renamed from either side. A returning phone updates its existing record, a revoked device can be restored without pairing again, and the list shows who is online right now.
- **A managed Agent can go back to the version Vibex verified** — Rollback installs the catalog pin wherever it can complete, and DeepSeek Harness 0.4.33 now works beside 0.4.32.
- **Faster and quieter** — A four-pane session group no longer holds the workbench in a repaint loop, the Files panel stops probing every installed editor on every frame, and repeating animations are capped at 30 fps.
- **Pairing on mobile starts with the camera** — Scanning leads, manual entry is one field, and a QR code can be read from a screenshot in the photo library.

### New Features

**Session groups**

- A group row stacks the group's distinct Agent avatars, shows the member count, and expands to its member sessions; its menu carries auto continue for every member, pin, rename, dissolve and delete-with-sessions (`6f3c8d7`)
- Groups join the organization tree: they nest in folders, reorder, drag, persist in `desktop-ui-state.json` and mirror over the remote sidebar protocol; a drop from another worktree is refused with an explicit message (`6f3c8d7`)
- A collapsed group reports the most actionable status among its members — needs input, then running, then error, then unread completion — and shows the member count only when there is nothing to report (`ae3c1a4`)
- A group with auto-continue on is carried by the status ring turning green instead of a second glyph in the trailing column, which the user could not turn off (`4e78772`)
- Every session owns a complete `SessionView` — timeline, projections, scroll and measurement state, runtime selection, streaming bookkeeping, expansions — and borrowing moves the whole view between the store and the renderer, so a switch cannot lose, truncate or mix a view (`9e8db4a`)
- A pane renders its own session's timeline, agent, model cascade, auto-continue countdown and checkout, and keeps streaming while it is not focused (`c0e1161`, `9e8db4a`, `2aea6ce`)
- Members that were never focused get a timeline fetched and parked from the authoritative items, so every pane shows the full conversation; a failed fetch leaves the summary in place and is not retried (`32713a9`)
- Every pane renders the same `render_composer` as the main workbench, against its own session's textarea, draft, attachments, queue, history, goal and plan editing, and terminal surfaces (`3f90ce3`)
- Only the focused pane's composer is enabled; the others render the same surface for their own session, dimmed and inert, until the pane is clicked (`02a6ea0`)
- The pane tab strip is the editor's strip with session tabs in it: it scrolls horizontally (wheel included), reveals the tab the selection moved to, and reorders by dragging a tab onto a sibling tab (`a548784`)
- Sessions enter and leave a group by drag: a tab dragged to the sidebar leaves the group, dropping dragged sessions on a group row or any member row joins them, and dropping them anywhere else releases them (`a548784`)
- A shift/ctrl selection offers "Create session group (N)" in the session menu, so forming a group no longer requires batch mode first (`a548784`)
- A tab dragged onto a pane edge splits on that axis, including a drag that stays inside its own pane; a one-tab pane splits instead of refusing, and a pane can be maximized (`a548784`, `3a0f711`)
- The group row's disclosure chevron lands on the Agent-logo column of the session rows beside it, the avatar stack reserves the width it paints, the group context menu titles itself at the top, and "Add to session group" is a submenu (`7d79054`, `731d722`)
- The right rail's activity buttons reorder by drag, with an insertion line and the order persisted in `right_rail.activity_order` (`c1b3a19`)
- Group members stay ordinary session rows on mobile, which has no group row yet (`6f3c8d7`)

**MCP & Skill markets**

- Add MCP and Skill marketplaces to the config center, reachable from their settings sidebars: search, per-source failure reporting, an install form with Agent selection, MCP environment variables, and a Skill document preview that is exactly what the install writes (`ee1aca1`)
- Fetch catalogs behind a public-network boundary: credentials-free https only, non-public addresses refused, manual redirects revalidated at every hop, bounded response bodies and one shared deadline (`ee1aca1`)
- Expose the market over the relay protocol as provider requests, so a paired device reads and installs through the authoritative runtime, and installs inherit mutation idempotency and audit records (`ee1aca1`)
- Index the MCP registry by walking its cursor into a process-wide index on a background thread, and answer every query by filtering it locally; the index keeps growing to thousands of entries while the market is open (`2234aab`)
- Search matches description and publisher, not just the name, and the response reports the indexed catalog size and total match count so a capped window is never mistaken for a complete catalog (`2234aab`)
- Raise the request ceiling above the slowest observed page and retry a dropped page, since a slow page was indistinguishable from an outage (`2234aab`)
- Map the fields the registry actually publishes — repository, publisher, lifecycle status, `updatedAt`, `isSecret` — and prefer npm over pypi over a remote package, so an oci-first record no longer installs `npx <image>` (`2234aab`)
- Fix launcher argument order and stop passing `-y` to `uvx`, which has no such flag (`2234aab`)
- Deliver an installed MCP server through each selected Agent's own channel, writing the native MCP file for native-config Agents and skipping Agents with no channel with a diagnostic (`69216b2`)
- Page both markets with the kit's Pagination, twelve cards at a time, against the window already fetched, because neither upstream can answer "show me page 5"; a new search resets to the first page and a page that no longer exists falls back to the last one (`aef87d6`)
- Populate the Skill market without a query by resolving an empty or one-character query to a broad browse query and re-ranking it by install count (`2175224`)
- Read each market from one fixed upstream — the official MCP registry and a public skill index — which drops the catalog source type, the source manager, the builtin pick catalogs and the invented category chips (`b25f764`)
- Render the markets inline in the section body, lift the MCP and Skill create actions above import, and give every action its own inline pane so the list always reads as the way back to a resource (`48665c1`, `4483a47`, `c3d5196`, `00c9877`)
- Rework the MCP and Skill management pages onto the Agent sidebar's card, control band and type scale, with the query and Search button on one taller row, transport filters beneath, the kit's progress in the Search icon slot, and the install form's homepage link on the title row (`4bed0d5`, `18d4f6f`)
- Remove the per-page native-export card from the MCP and Skills pages, since MCP servers already reach each enabled Agent through its own channel (`35ef65d`)
- Land the `ScrollGutter` trait the management scroll regions call, and apply it across the workbench's scroll regions so the overlay scrollbar never covers the last column (`4bed0d5`, `2e08866`)

**Notifications**

- Replace the per-platform notification senders with gpui's `SystemNotification` and `App::show_system_notification`, and register the Windows AppUserModelID and Linux application name at startup (`ea1952a`)
- Route a timeline notification by window focus: an in-app hint while the window is in front, a system notification when it is not, tagged with the session id so a newer notification replaces an older one (`ea1952a`)
- Clicking a system notification restores or rebuilds the workbench window and selects the session that raised it, silently ignoring a closed session or an unknown tag (`78a8b44`)
- Deduplicate timeline notifications by a per-session watermark that advances on dismissal and mute too, so one completion no longer raises two identical prompts (`dd5c3a8`)
- Make a light hint's message selectable and copyable through `gpui_ext::hint_notification`, without a drag release falling through to click-to-dismiss (`ebcc88b`)

**Remote & devices**

- Publish the runtime's own display name as `server_display_name` in `server_info` and `serverDisplayName` in `/api/v2/info`, defaulting to the machine's device name and following an operator rename stored in the new `runtime_identity` table (migration v56) (`9432036`)
- Add `rename_device` and `rename_runtime` to `device_management`, both requiring `mutate_device_management`, validated through `normalize_remote_display_name`, audited, and broadcast as a device event so connected clients apply the new name without reconnecting (`9432036`)
- A device rename leaves the grant, its revision and its status untouched, so a live device never has to pair again (`9432036`)
- Keep one device record per client: a phone presents its stored device identity on every pairing, the existing row keeps its id and creation time, the fresh pairing replaces its grant and permission level, and the grant revision is bumped (`6c55376`)
- Add a Delete command for every paired-device record; deleting an active record revokes its grant and disconnects the client first, and audit history is kept in both cases (`6c55376`)
- Restore a revoked device grant so a phone that kept its credential reconnects without pairing again, with the grant revision moved so nothing negotiated before the revocation is reused; only a revoked record restores (`35be91a`)
- Show which paired devices are online now, read from the Gateway connection registry and re-read on a short timer while the list is open, instead of the stored last-seen sentence (`35be91a`)
- Rename runtimes and devices from the desktop runtime manager, the paired-device list, and the mobile host list, each through the runtime as the authority (`9432036`)

**Agent management**

- Roll a managed Agent back to the Vibex-verified catalog pin, carrying the action through the backend facade and the remote protocol and re-probing runtime options afterwards because the Adapter version decides which model catalogue the Agent advertises (`6d837e2`)
- Scope `reject_semver_downgrade` to the channel install target, and refuse a binary archive or a `manual` catalog entry as a typed capability error instead of writing a mislabelled install (`6d837e2`)
- Offer the rollback only where it can complete, deriving the offer from the installation state and the catalog rather than storing it (`6d837e2`)
- Keep DeepSeek Harness model ids working across 0.4.32 and 0.4.33 by projecting the bare id and registering the qualified `route::model` spelling as a read-back alias, rejecting an alias another Model already owns (`c68814b`)
- Split the catalog's compatibility floor from its pin, declaring 0.4.32 only for DeepSeek Harness, so an installed older runtime keeps its credential surface, model row and `session/new` (`16d5681`)

**Provider import**

- Import Grok Build providers from CC Switch by parsing its `config.toml`: `[models].default` and its `[model.<id>]` tables, `api_backend` onto the wire protocol, `context_window` onto the context tokens, and the credential migrated under the config's `env_key` (`12cb935`)
- Carry per-Model declarations on the import item so a source can state wire protocols and display names, and accept the unquoted `[model.grok-4.5]` spelling CC Switch's deep-link import writes (`12cb935`)
- Gate native import to the Agents it supports — claude, codex, gemini, hermes, opencode, pi and grok — and hide the action everywhere else (`5d10a23`)
- Name the reason an import found nothing — CC Switch missing, no provider for this Agent, already imported, or blocked by a parse error — with a localized message that names the Agent, instead of one bare error code (`72a45b4`)
- Report an empty import as an informational notice rather than an error alert, keeping the error code for a genuine parse failure (`b248628`)

**Workbench & editor**

- Scope the workbench column per project checkout or per session, chosen by a new Workbench setting, parking the live column and adopting the incoming one on a switch; scoped state covers the rail visibility, width and activity, the editor panel's visibility and width, the multi-tab editor, and the integrated panel's presentation, while loaded rows stay shared (`c3527ca`)
- Remember the multi-tab editor layout per session, persisting parked layouts in `PreviewUiState.session_layouts`, bounded by newest tab, with fullscreen kept as panel state (`cf080d6`)
- Call the multi-tab surface the editor everywhere, and make closing it collapse the panel while keeping every open tab, so the editor comes back exactly as it was left (`ab1580d`, `98c4654`)
- Toggle the editor from a rail logo button that reads one `preview_panel_open` helper, so the docked flag and the narrow-window overlay cannot disagree (`4abaae0`)
- Draw the panel toggles with the mirrored sidebar glyphs — `sidebar-left` on the title-bar toggle, `sidebar-right` on the rail's editor button — bundled as `currentColor` strokes with no literal fill (`a64eec6`)
- Reveal nested files in the Files tree by walking the parent chain, opening the directories the tree knows, fetching the first missing one, and retrying until the file can be selected and scrolled into view (`0b4bc8f`)
- Add a reveal control to the file, diff and commit headers and to every commit file row, opening the rail's Files tree on that file; the commit message body now starts folded (`98c4654`)
- Highlight composer `/` commands, `@` mentions and `$` skills with a rounded chip painted over the same glyphs, wrapping per line and painting nothing once scrolled out of the viewport (`57b020f`)
- Keep the question and answers of a settled input request as a disclosure: the header carries the outcome, the collapsed body keeps the question and recorded answer, and expanding reveals every labelled field (`c0ac5d7`)
- Jump to the pending permission card from its alert, and put the alert away for the requests currently pending (`c4c5be3`)
- Remember a dismissed permission alert across restarts, keyed by the pending-request set so a later request brings it back (`13a0537`)
- Add an Agents group to global search that opens the config center on the matched Agent, led by catalog identity for a non-empty query (`c261f6f`)
- Link the runtime manager and the connect-a-mobile-device dialog to their documentation through one shared help-glyph builder (`9d7a2c0`)

**Settings & appearance**

- Add `pauseInactiveAnimation` to the appearance state, off by default, gating the motion layer's window-inactive pause instead of applying it unconditionally, with the setting exposed beside reduced motion (`2c72ee8`)
- Offer System, Direct and Custom network proxy modes instead of one ambiguous switch, showing the address, the bypass list and the connection test only under Custom and migrating state persisted before modes existed (`94f909a`)
- Describe the Network proxy row by what it governs — Agent requests, Git and terminal commands, Agent installs, and updates — instead of naming an extension marketplace and a built-in browser Vibex does not have (`e79139e`)
- Keep dialog inputs editable: the device rename, terminal rename, runtime feature value and custom shell dialogs now create their input before `open_dialog` and take focus on the next frame, and the mobile host rename sheet focuses its field and raises the keyboard (`e069446`)

**Mobile**

- Lead the pairing page with scanning, keep the other entries behind one "Can't scan?" group, and collapse manual entry to a single field that reveals the server address only for a bare code (`af36fa4`)
- Decode a QR image picked from the photo library, and add an open-settings bridge on both hosts for the denied local-network case (`af36fa4`)
- Pair under the phone's own device name (Android `Build.MODEL`) instead of a generic product name (`9432036`)
- Render the runtime's published name in the host list, observed at connect and from the rename event (`9432036`)

**Data & diagnostics**

- Import release-candidate data into the stable channel from Data & Diagnostics, staging and verifying a migrated snapshot before anything replaces the live files, and swapping at the next runtime start after the home lock is held (`687494c`)
- Ask once on a stable first launch that finds RC data next to its home, and record the answer so the prompt never returns (`687494c`)

**Developer experience**

- Move `gpui-kit` to the published 0.6.4 release, add the matching `gpui-base`, and bump `gpui-pre-mobile` (`c768bc9`)
- Adopt the new kit APIs: the file search field becomes one `InputGroup`, the usage charts sample the kit's motion layer through `gpui-base`, and the runtime manager gains its add dialog, per-row meta lines, lane metrics and an error strip (`c768bc9`)
- Report added and removed line counts per file in Git status and commit detail (`c768bc9`)
- Record the Git path-identity and Windows `MAX_PATH` contracts in spec, and the pin-versus-floor and second-credential-gate contracts for DeepSeek Harness (`9a3a0e6`, `16d5681`)

### Fixes

**Desktop — session panes and focus**

- Stop focus from changing what a pane shows: materialization treats the live view's session as available and parking a fetched view never replaces an existing one, and every pane renders the same conversation area and composer (`9a390f4`)
- Keep each pane on its own conversation by parking the live view under its own session id and handing the focused session's view back explicitly (`2a418bd`)
- Give each pane its own timeline element, built from the session the list belongs to, so panes stop sharing one measured row layout and one viewport (`3a0f711`)
- Route the prepaint turn-height measurement and the borrowed view by session, so measurements land in the view that owns them and a pane that loses focus keeps its layout (`b97cda4`, `2aea6ce`)
- Keep a scrolled split pane from snapping back to the bottom by borrowing the pane's view in the wheel listener and carrying that session into the resume timer (`ae9bd3d`)
- Resolve a same-pane drop by region, and filter the shared pane drop target by `pane_id`, so a drag inside one pane splits instead of being a no-op or landing on whichever pane painted last (`3a0f711`, `4959240`)
- Give each group pane its own composer entity, bound to the session it was created for, so one textarea is not shared by the whole workspace (`02a6ea0`)
- Trim the per-pane textarea store to the sessions a group still holds, and drop the per-pane textarea map and session draft map (`3f90ce3`, `304ee96`)
- Keep a multi-selection whole when dragging sessions: a range may cross the pinned band, the payload appends every selected row the sibling order does not know, and a drag no longer refuses to start when the order lacks its row (`bdc20d2`)
- Let the landing row decide a drop's group, and take the sessions a drop applies to from the selection itself rather than a payload captured when the row was built (`c8b4873`)
- Keep a group's panes reachable from the sidebar: selecting a member moves the group's focused pane and that pane's active tab, and a maximized workspace follows the selection (`09f3c47`)
- Derive pane focus from the selection as well as the group layout, so a restored layout cannot disagree with it (`09f3c47`)
- Keep a pane's timeline live while it is unfocused by borrowing its own view for row build, unit build and height recording (`2aea6ce`)
- Keep a non-focused pane's tabs draggable by no longer stopping mouse-down propagation (`3a0f711`)
- Paint monochrome Agent marks with the sidebar foreground so they follow the theme, while polychrome brand marks keep their embedded colours (`09f3c47`)
- Report a group's auto-continue through the status ring, and keep a worktree row passing `false` because its sessions can disagree (`4e78772`)

**Desktop — layout, chrome and readability**

- Keep the user bubble's last wrapped line inside the pill by capping the body at the width the pill's content box resolves to, with a regression probe comparing the pill's height against a plain text element (`a4684c0`)
- Keep a pinned session's state mark in the lane left of its pin mark, and yield the trailing column to the hover action cluster in every row (`2c8ba9b`)
- Stop a dialog builder from rebuilding its inputs on every repaint, which replaced the entity being typed into (`e069446`)
- Put the MCP and Skill controls back on the kit's type scale: medium search fields at 32px with 14px text, one 32px control band across the sidebar, and a genuinely left-aligned market label (`18d4f6f`)
- Give the MCP market the reference density: cards may shrink to 260px so the grid fits two or three columns, the address chip leads with the transport glyph, and both market headings lead with their registry glyph (`18d4f6f`)
- Register the terminal icon the MCP transport rows ask for, and bundle `chevrons-left.svg` with `book-open.svg` (`96133bb`, `9e8db4a`)
- Land `ScrollGutter` across the workbench's scroll regions so the overlay scrollbar never covers the last column (`2e08866`)

**Desktop — sessions, sidebar and timeline**

- Drop a stale error dot once a session works again by projecting a locally pending turn as running over a stale `error` snapshot, while `needs_input` keeps winning; admission and completion now publish a session snapshot (`555ae3b`)
- Anchor a pending turn to the moment its projection is built instead of the Unix epoch, which had flashed a header reading "worked for 497217h 12m", and fall back to the borrowed view's session so a brand-new session keeps its optimistic first message (`f7ca797`)
- Highlight the composer's trigger tokens as ranges inside the text, so the caret can move into one and edit it like any other character (`57b020f`)
- Show the most actionable member status on a collapsed group row, using the priority a workspace row already encodes (`ae3c1a4`)
- Align group rows with the Agent-logo column of the session rows beside them (`7d79054`, `09f3c47`, `731d722`)

**Market**

- Fix the MCP registry's unusable interactive search and its single-page ceiling by indexing the catalog locally (`2234aab`)
- Populate the Skill market without a query instead of rendering empty until the user types (`2175224`)
- Deliver installed MCP servers to every Agent that can host them, and report the ones that cannot (`69216b2`)
- Report an empty CC Switch import as a notice, and name why nothing was found (`b248628`, `72a45b4`)
- Page the markets against the fetched window, and reset or clamp the page when a search changes the result set (`aef87d6`)
- Drop the catalog source manager, builtin pick catalogs and invented category chips, so every user searches the same upstreams with the same failure modes (`b25f764`)

**Git & Windows**

- Resolve the mutation lock path through `repository_common_dir` instead of a comparison identity, whose verbatim prefix `CreateFileW` rejected with `ERROR_INVALID_NAME`; the same mangled value had broken the `git -C` calls made from `normalized_path` (`9930556`)
- Strip the Windows namespace prefix before normalizing, repairing 19 of the 32 `vibex-git` tests on Windows (`9930556`)
- Keep test temp paths and Git's own state files inside the Windows 260-character budget, and pin `core.autocrlf=false` in test repositories so byte-exact assertions do not read back CRLF (`953a3a3`)

**Agent, ACP and usage**

- Keep a DeepSeek Harness runtime usable across the 0.4.33 pin bump by splitting the compatibility floor from the pin and projecting the route-derived credential name (`16d5681`)
- Keep DeepSeek Harness model ids working across adapter versions with a read-back alias for the qualified spelling (`c68814b`)
- Launch every npm ACP adapter with `spawn(process.execPath, [adapter, ...args])` instead of importing it in-process, which had left `process.argv[1]` pointing at the launcher and silently disabled CLI adapters that only start when their own entry file is invoked (`f483d54`)
- Pre-extract the vendored DeepSeek Harness runtime with symlink entries skipped and the `.dsh-acp-runtime` marker written, because Windows refuses to create the `node_modules/.bin` symlinks without Developer Mode or elevation (`f483d54`)
- Count turn-scoped adapters by registering DeepSeek Harness as turn-scoped with a per-request `usage_update`, so a growing reading no longer loses the previous turn's tokens a second time and a shrinking reading is not read as a counter reset (`624f2e4`)
- Label an aggregate "API requests" only once every turn reported them, and keep naming the turn count until then (`624f2e4`)
- Run the proxy connection test on the Tokio runtime, since the probe's first DNS or socket operation panicked a GPUI worker thread and took the workbench down (`30a31a9`)
- Fix a Windows-only path separator assertion in the DeepSeek install manifest test (`f483d54`)

**Build and quality gate**

- Apply the rustfmt hunks the repository's own `check:rust` gate failed on, and collapse the `collapsible_if` clippy rejected in the market size-limit check (`8f1d0f8`, `72da172`, `73d143b`, `5fe4936`, `d49a915`)

### Performance

- Give every session-group pane its own `SessionGroupPaneView` behind an `Entity::cached` boundary, so one pane's animation no longer rebuilds the whole split and a notify raised inside a pane reaches only that pane (`e6f690a`)
- Stop the agent thinking shimmer from outliving an interrupted turn by asking the session whether its turn is live, which had kept a shimmer running in every pane holding that session (`e6f690a`)
- Drive the composer's auto-continue sync from the events that change its inputs instead of the render path, and issue one probe per revision per backoff window: per-frame probe re-entries went from 111 to 0 on a four-pane group (`e545edc`)
- Borrow panes unweighed and hand every pane back through one weighed release, and share the runtime catalog with a memo bounded per distinct selection (`e545edc`)
- Route the prepaint height measurement by session and weigh a stored session view once, instead of rebuilding the row-size table on every frame (`b97cda4`)
- Cache the external-editor probe per process and warm it from a background executor, because the Files panel header probed PATH × `PATHEXT` × 16 editors on every frame — about 4600 filesystem queries and 60 ms per probe on Windows (`3a0cd61`)
- Limit the local spinner, the startup wordmark shimmer and the mobile pairing gradient to 30 fps, so sidebar loading no longer drives a whole-window repaint (`06223cb`)
- Launch npm-generated `.cmd`/`.bat` Node shims directly through `process.execPath`, removing one `cmd.exe` and its console host per ACP session on Windows (`06223cb`)
- Pause the workbench's repeating animations when its window is inactive only when the new appearance setting asks for it, so a backgrounded workbench is not frozen by default (`06223cb`, `2c72ee8`)
- Look session-view and group-load keys up borrowed instead of allocating an owned key per member per frame, and let `system_locale` hand back a `&'static str` instead of cloning on every `strings()` call (`b97cda4`)

### Under the hood

- The first-party icon bundle grew from 191 to 194 files: `sidebar-left.svg` and `sidebar-right.svg` for the panel toggles and `chevrons-left.svg` for the pane tab menu, all three Lucide ISC paths, registered with their provenance, count and tree digest in the license policy (`a64eec6`, `a548784`, `9e8db4a`)
- Version numbers were bumped to `0.1.0-rc.6` across the workspace, the packaging inputs, the Android version fallback, the README and deployment documentation, the native-content package expectations, and the reviewed first-party asset-bundle versions in the license policy
- Spec updates: the inactive-window rendering pause is recorded as an opt-in appearance preference (`be6e01b`), the Git path identity and Windows `MAX_PATH` contracts (`9a3a0e6`), the executor boundary for async probes (`30a31a9`), and the pin-versus-floor and second-credential-gate contracts for DeepSeek Harness (`16d5681`)
- New or extended test coverage: session-group pane views and focus routing, split and drag region resolution, the pinned-band selection scope, session group drag payloads, the MCP market index and its paging, native MCP delivery classification, the CC Switch Grok Build parser, the paired-device identity and presence, the runtime rename protocol, the manager rollback gating, the pill height against a measured text element, the rendered pill tabs of the config center nav, selectable hint text, the proxy mode migration, the settled elicitation disclosure, the Workbench scope re-keying and the per-session editor layouts (`2232f70`, `a4684c0`, `bdc20d2`, `c8b4873`, `e545edc`, `b97cda4`, `69216b2`, `12cb935`, `6c55376`, `35be91a`, `9432036`, `6d837e2`, `ebcc88b`, `94f909a`, `c0ac5d7`, `c3527ca`, `cf080d6`, `e069446`)
- Dependencies moved to the published `gpui-kit` 0.6.4 with the matching `gpui-base`, and `gpui-pre-mobile` was bumped (`c768bc9`)

## 中文

### 亮点

- **一个工作台，同时驱动多个会话** — 会话组把同一工作区的会话绑成侧栏一行和一块分屏工作区，每个分屏都有自己的对话、输入框和标签。可以拖拽进出会话组、把标签拖到分屏边缘来分屏，折叠的组行一眼就能看到成员状态。
- **MCP 与技能市场** — 在配置中心浏览并安装 MCP 服务与技能。MCP 目录在本地建立索引，搜索即时且覆盖整个注册表，安装也会送达每一个能承载它的 Agent。
- **可以点击响应的通知** — 桌面通知改用 gpui 提供的系统通道，窗口在前台时不会打扰，点击通知会把工作台唤回到发出通知的会话。
- **运行时与设备有名字，也能被管理** — 两者默认取机器设备名，两侧都能重命名。回访的手机会更新既有记录，被撤销的设备无需重新配对即可恢复，列表还会显示当前在线状态。
- **托管 Agent 可以回退到 Vibex 验证过的版本** — 只要条件允许，回滚就会安装目录中固定的版本；DeepSeek Harness 0.4.33 也能与 0.4.32 并存。
- **更快、更安静** — 四屏会话组不再让工作台陷入重绘循环，文件面板不再每帧探测所有已安装编辑器，重复动画限制在 30 fps。
- **移动端配对从相机开始** — 扫码排在首位，手动输入收成一个字段，还能从相册里的截图读取二维码。

### 新功能

**会话组**

- 组行叠放组内不同的 Agent 头像、显示成员数量，并可展开到成员会话；菜单提供对全部成员的自动继续、置顶、重命名、解散和连会话删除（`6f3c8d7`）
- 会话组加入组织树：可放进文件夹、排序、拖拽、持久化到 `desktop-ui-state.json` 并通过远程侧栏协议同步；从其它工作树拖入会以明确提示拒绝（`6f3c8d7`）
- 折叠的组报告成员中最有行动价值的状态——需要输入、运行中、错误、未读完成——只有无状态可报时才显示成员数量（`ae3c1a4`）
- 开启自动继续的组由状态环变绿表示，而不是在尾部列再画一个用户无法关闭的图标（`4e78772`）
- 每个会话拥有完整的 `SessionView`——时间线、投影、滚动与测量状态、运行时选择、流式记账、展开状态——借用会把整个视图在存储与渲染器之间移动，切换不会丢失、截断或混淆视图（`9e8db4a`）
- 分屏渲染自己会话的时间线、Agent、模型级联、自动继续倒计时和检出，并且在未聚焦时继续接收流式更新（`c0e1161`、`9e8db4a`、`2aea6ce`）
- 从未聚焦过的成员会按权威条目抓取并停放一份时间线，使每个分屏都显示完整对话；抓取失败时保留摘要且不再重试（`32713a9`）
- 每个分屏渲染与主工作台相同的 `render_composer`，对应自己会话的文本框、草稿、附件、队列、历史、目标与计划编辑以及终端界面（`3f90ce3`）
- 只有聚焦分屏的输入框可用；其它分屏为各自会话渲染同一界面，但变暗且不可交互，直到点击该分屏（`02a6ea0`）
- 分屏标签栏就是编辑器的标签栏：可横向滚动（含滚轮）、自动显露选中项移动到的标签，并可通过把标签拖到相邻标签上重新排序（`a548784`）
- 会话可通过拖拽进出会话组：标签拖到侧栏即离开会话组，把拖动的会话放到组行或任一成员行即加入，放到其它任何地方即释放（`a548784`）
- Shift/Ctrl 多选后会话菜单提供「新建会话组 (N)」，组建会话组不再需要先进入批量模式（`a548784`）
- 标签拖到分屏边缘即按该轴分屏，包括停留在同一分屏内的拖拽；只有一个标签的分屏也会分屏而不是拒绝，分屏还可以最大化（`a548784`、`3a0f711`）
- 组行的展开箭头落在旁边会话行的 Agent 标志列上，头像堆叠预留自己绘制的宽度，组右键菜单在顶部显示标题，「添加到会话组」改为子菜单（`7d79054`、`731d722`）
- 右栏的活动按钮可以拖动排序，拖动时有插入线，顺序持久化在 `right_rail.activity_order`（`c1b3a19`）
- 移动端仍把会话组成员作为普通会话行显示，因为移动端还没有组行（`6f3c8d7`）

**MCP 与技能市场**

- 配置中心新增 MCP 与技能市场，可从各自的设置侧栏进入：搜索、按来源报告失败、带 Agent 选择的安装表单、MCP 环境变量，以及与实际写入内容完全一致的技能文档预览（`ee1aca1`）
- 目录抓取置于公共网络边界之后：仅允许无凭证 https、拒绝非公网地址、每一跳都重新校验手动重定向、限制响应体大小并共用同一截止时间（`ee1aca1`）
- 市场通过中继协议以 provider 请求暴露，使已配对设备通过权威运行时读取和安装，安装同时继承变更幂等与审计记录（`ee1aca1`）
- 在后台线程按游标把 MCP 注册表走成进程级索引，所有查询在本地过滤；市场打开期间索引会持续增长到数千条（`2234aab`）
- 搜索同时匹配描述和发布者而不只是名称，响应报告已索引的目录规模和匹配总数，使截断的窗口不会被误认为完整目录（`2234aab`）
- 请求上限提高到超过实测最慢的一页，并对掉页重试，因为慢页此前与故障无法区分（`2234aab`）
- 映射注册表真正发布的字段——仓库、发布者、生命周期状态、`updatedAt`、`isSecret`——并按 npm、pypi、远程包的顺序优先选择，oci 优先的记录不再安装成 `npx <image>`（`2234aab`）
- 修正启动器参数顺序，不再给 `uvx` 传它没有的 `-y` 标志（`2234aab`）
- 通过每个所选 Agent 自己的通道投递已安装的 MCP 服务，为原生配置类 Agent 写原生 MCP 文件，对没有通道的 Agent 带诊断信息跳过（`69216b2`）
- 两个市场都改用套件的 Pagination 分页，每页十二张卡片，针对已抓取的窗口分页，因为两个上游都无法回答「显示第 5 页」；新搜索回到第一页，页码不存在时回落到最后一页（`aef87d6`）
- 无查询时也能填充技能市场：把空查询或单字符查询解析为宽泛的浏览查询，并按安装量重新排序（`2175224`）
- 每个市场只读一个固定上游——官方 MCP 注册表和一个公共技能索引——因此删除了目录来源类型、来源管理器、内置精选目录和自造的分类标签（`b25f764`）
- 市场改为在区块主体内渲染，MCP 与技能的创建操作上移到导入之前，每个操作也各自占用内联面板，使资源列表始终是回到资源的路径（`48665c1`、`4483a47`、`c3d5196`、`00c9877`）
- MCP 与技能管理页改对齐 Agent 侧栏的卡片、控件带和字号体系：查询与搜索按钮位于同一行更高控件上，传输筛选在其下方，搜索按钮在图标位显示套件进度，安装表单的主页链接移到标题行（`4bed0d5`、`18d4f6f`）
- 移除 MCP 与技能页各自的原生导出卡片，因为 MCP 服务已通过各自的通道送达每个启用的 Agent（`35ef65d`）
- 落地管理滚动区域调用的 `ScrollGutter` trait，并应用到工作台的滚动区域，使悬浮滚动条不再盖住最后一列（`4bed0d5`、`2e08866`）

**通知**

- 用 gpui 的 `SystemNotification` 和 `App::show_system_notification` 取代各平台发送器，并在启动时注册 Windows AppUserModelID 和 Linux 应用名（`ea1952a`）
- 时间线通知按窗口焦点分流：窗口在前台时发应用内提示，否则发系统通知，并以会话 id 作为 tag 让新通知覆盖旧通知（`ea1952a`）
- 点击系统通知会唤起或重建工作台窗口并选中发出通知的会话，会话已关闭或 tag 非法时静默忽略（`78a8b44`）
- 按会话水位去重时间线通知，关闭或静音时也推进水位，一次完成不再弹出两个相同提示（`dd5c3a8`）
- 通过 `gpui_ext::hint_notification` 让轻提示的消息可选中、可复制，拖拽释放也不会误触点击关闭（`ebcc88b`）

**远程与设备**

- 运行时在 `server_info` 中以 `server_display_name`、在 `/api/v2/info` 中以 `serverDisplayName` 发布自己的显示名，默认取机器设备名，并跟随存放在新表 `runtime_identity`（迁移 v56）中的运维重命名（`9432036`）
- `device_management` 新增 `rename_device` 与 `rename_runtime`，两者都要求 `mutate_device_management` 权限、经 `normalize_remote_display_name` 校验、写入审计，并广播设备事件让已连接客户端无需重连即可应用新名字（`9432036`）
- 设备重命名不动授权、授权修订号和状态，在线设备无需重新配对（`9432036`）
- 每个客户端只保留一条设备记录：手机每次配对都出示已保存的设备身份，既有记录保留 id 和创建时间，新配对替换其授权和权限级别，并提升授权修订号（`6c55376`）
- 已配对设备列表的每条记录都新增删除命令；删除活跃记录会先撤销授权并断开客户端，两种情况下都保留审计历史（`6c55376`）
- 被撤销的设备授权可以恢复，保留凭证的手机无需重新配对即可连回，同时提升授权修订号使撤销前协商的一切不再被复用；只有已撤销的记录可以恢复（`35be91a`）
- 已配对设备列表显示当前在线状态，直接读取 Gateway 连接注册表并在列表打开期间短定时刷新，而不再显示可能自相矛盾的「最后在线」句子（`35be91a`）
- 桌面端运行时管理器、已配对设备列表和移动端主机列表都能重命名运行时与设备，且都以运行时为权威（`9432036`）

**Agent 管理**

- 把托管 Agent 回滚到 Vibex 验证过的目录固定版本，该操作贯穿后端门面和远程协议，并在之后重新探测运行时选项，因为 Adapter 版本决定 Agent 公布哪份模型目录（`6d837e2`）
- 把 `reject_semver_downgrade` 限定在通道安装目标上，对二进制归档或 `manual` 目录条目以类型化能力错误拒绝，而不是写入一个标签错误的安装（`6d837e2`）
- 只在能完成时提供回滚，该判断由安装状态和目录推导而来而非存储（`6d837e2`）
- 通过投影裸 id、并把限定拼写 `route::model` 注册为读回别名，让 DeepSeek Harness 的模型 id 在 0.4.32 与 0.4.33 上都能工作；已被其它模型占用的别名仍会被拒绝（`c68814b`）
- 把目录的兼容下限与固定版本拆开，只为 DeepSeek Harness 声明 0.4.32，使已安装的旧版运行时保留凭证界面、模型行和 `session/new`（`16d5681`）

**供应商导入**

- 从 CC Switch 导入 Grok Build 供应商：解析其 `config.toml` 的 `[models].default` 与 `[model.<id>]` 表，把 `api_backend` 映射到 wire 协议、`context_window` 映射到上下文 token 数，并把凭证迁移到配置声明的 `env_key` 之下（`12cb935`）
- 在导入项上携带每个模型的声明，使来源可以给出 wire 协议和显示名，并接受 CC Switch 深链导入写出的不带引号 `[model.grok-4.5]` 拼写（`12cb935`）
- 原生导入只对支持的 Agent 提供——claude、codex、gemini、hermes、opencode、pi 和 grok——其它 Agent 隐藏该操作（`5d10a23`）
- 明确导入无结果的原因——CC Switch 缺失、该 Agent 没有供应商、已全部导入、或被解析错误阻塞——并给出带 Agent 名称的本地化消息，而不是一个裸错误码（`72a45b4`）
- 空导入按信息提示上报而不是错误弹窗，真正的解析失败才保留错误码（`b248628`）

**工作台与编辑器**

- 工作台栏按项目检出或按会话分键，由新的「工作台」设置决定，切换时停放当前栏并接管目标栏；分键状态覆盖右栏的可见性、宽度和活动，编辑器面板的可见性与宽度，多标签编辑器，以及集成面板的呈现；已加载的行仍保持共享（`c3527ca`）
- 多标签编辑器布局按会话记忆，停放的布局持久化在 `PreviewUiState.session_layouts` 中，按最新标签设上限，全屏仍属于面板状态（`cf080d6`）
- 多标签界面统一称为编辑器；关闭它改为折叠面板并保留所有已打开标签，使编辑器回来时与离开时完全一致（`ab1580d`、`98c4654`）
- 右栏标志按钮切换编辑器，并统一读取 `preview_panel_open`，使停靠标志与窄窗口浮层不会互相矛盾（`4abaae0`）
- 面板切换按钮改用镜像的侧栏字形——标题栏用 `sidebar-left`，右栏编辑器按钮用 `sidebar-right`——并以无字面填充的 `currentColor` 描边打包（`a64eec6`）
- 文件树可以定位嵌套文件：沿父链逐级展开文件树已知的目录，抓取第一个缺失的目录，并从加载完成回调重试直到文件可被选中并滚动到可见位置（`0b4bc8f`）
- 文件、差异和提交头部以及每个提交文件行都新增定位控件，可在右栏文件树中打开该文件；提交信息正文默认折叠（`98c4654`）
- 输入框中的 `/` 命令、`@` 提及和 `$` 技能以圆角底片覆盖在同一批字形上；换行的 token 按行高亮，滚出视口后不再绘制（`57b020f`）
- 已结束的输入请求保留问题与回答并渲染为可展开行：头部显示结果，折叠正文保留问题和已记录的回答，展开后显示每个带标签的字段（`c0ac5d7`）
- 点击权限等待提示可以跳转到对应的权限卡片，也可以把提示对当前待处理请求收起（`c4c5be3`）
- 已关闭的权限提示跨重启记忆，并按待处理请求集合作为键，后续请求会让提示重新出现（`13a0537`）
- 全局搜索新增 Agents 分组，可打开配置中心并定位到匹配的 Agent；非空查询时该分组排在前面（`c261f6f`）
- 运行时管理器与连接移动设备对话框通过共享的帮助字形构建器链接到各自文档（`9d7a2c0`）

**设置与外观**

- 外观状态新增 `pauseInactiveAnimation`，默认关闭，用来控制动效层的窗口失活暂停而不是无条件生效，并与「减少动态效果」并列暴露（`2c72ee8`）
- 网络代理改为「跟随系统」「直连」「自定义」三种模式，取代含义模糊的单一开关；只有自定义模式显示地址、绕过列表和连接测试，并迁移模式出现之前持久化的状态（`94f909a`）
- 网络代理行的说明改为描述它实际管辖的范围——Agent 请求、Git 与终端命令、Agent 安装和更新——而不再提 Vibex 并不具备的扩展市场和内置浏览器（`e79139e`）
- 对话框输入框保持可编辑：设备重命名、终端重命名、运行时特性值和自定义 shell 都在 `open_dialog` 之前创建输入实体并在下一帧取焦，移动端主机重命名面板打开时取焦并唤起键盘（`e069446`）

**移动端**

- 配对页以扫码为主，其它入口收进「无法扫码？」分组，手动输入收成一个字段且只在纯配对码时显示服务器地址（`af36fa4`）
- 支持解码从相册选择的二维码图片，两个宿主都新增本地网络被拒时的打开设置桥接（`af36fa4`）
- 以手机自己的设备名（Android `Build.MODEL`）配对，而不再用通用产品名（`9432036`）
- 主机列表渲染运行时公布的名字，在连接时和收到重命名事件时更新（`9432036`）

**数据与诊断**

- 从「数据与诊断」把候选版数据导入稳定通道：先暂存并校验迁移后的快照，再替换任何线上文件；替换发生在下一次运行时启动、拿到 home 锁之后（`687494c`）
- 稳定版首次启动发现 home 旁有 RC 数据时只询问一次，并记录答案使提示不再出现（`687494c`）

**开发者体验**

- `gpui-kit` 迁移到已发布的 0.6.4，补上对应的 `gpui-base`，并升级 `gpui-pre-mobile`（`c768bc9`）
- 采用新的套件 API：文件搜索框改为一个 `InputGroup`，用量图表通过 `gpui-base` 采样套件动效层，运行时管理器新增添加对话框、逐行元信息、泳道度量和错误条（`c768bc9`）
- Git 状态和提交详情按文件报告新增与删除行数（`c768bc9`）
- 在 spec 中记录 Git 路径身份与 Windows `MAX_PATH` 契约，以及 DeepSeek Harness 的固定版本与兼容下限、第二道凭证闸门契约（`9a3a0e6`、`16d5681`）

### 修复

**桌面端——分屏与焦点**

- 焦点不再改变分屏显示的内容：物化流程把实时视图的会话视为可用，停放抓取到的视图时不再替换既有视图，并且每个分屏渲染相同的对话区域和输入框（`9a390f4`）
- 把实时视图停放在它自己的会话 id 下并显式交回聚焦会话的视图，使每个分屏保持在自己的对话上（`2a418bd`）
- 每个分屏的时间线元素按所属会话生成 id，使分屏不再共用一份测量行布局和一个视口（`3a0f711`）
- 预绘制阶段的回合高度测量与借用视图按会话路由，使测量落在拥有它的视图里，失去焦点的分屏保持自己的布局（`b97cda4`、`2aea6ce`）
- 在滚轮监听器中借用分屏自己的视图并把该会话带入恢复计时器，使滚动过的分屏不再被拉回底部（`ae9bd3d`）
- 同屏落点按区域解析，并按 `pane_id` 过滤共享落点目标，使同一分屏内的拖拽可以分屏，而不是无操作或落到最后绘制的分屏上（`3a0f711`、`4959240`）
- 每个分屏拥有自己的输入框实体，并绑定到创建它的会话，不再让整个工作区共用一个文本框（`02a6ea0`）
- 每个分屏的文本框存储裁剪到会话组仍持有的会话，并删除按分屏的文本框映射和会话草稿映射（`3f90ce3`、`304ee96`）
- 拖动多选会话时保持选择完整：范围可以跨越置顶区，载荷会补上兄弟顺序不知道的已选行，且当顺序中缺少起始行时拖拽不再拒绝启动（`bdc20d2`）
- 由落点所在行决定拖放进入哪个会话组，并且拖放作用的会话来自当前选择而不是构建该行时捕获的载荷（`c8b4873`）
- 会话组的各个分屏可以从侧栏到达：选中成员会移动该组的聚焦分屏及该分屏的活动标签，最大化的会话组也跟随选择（`09f3c47`）
- 分屏焦点同时由选择和会话组布局推导，使恢复的布局不会与选择不一致（`09f3c47`）
- 未聚焦的分屏通过借用自身视图完成行构建、单元构建和高度记录，使它的时间线保持实时（`2aea6ce`）
- 非聚焦分屏不再阻止鼠标按下事件冒泡，使它的标签可以拖动（`3a0f711`）
- 单色 Agent 标志改用侧栏前景色绘制以跟随主题，多色品牌标志保留自带颜色（`09f3c47`）
- 会话组的自动继续由状态环表达；工作树行仍传 `false`，因为它的会话可能不一致（`4e78772`）

**桌面端——布局、窗口装饰与可读性**

- 把消息体宽度限制在气泡内容盒解析出的宽度内，使用户气泡最后一行换行内容不再被裁掉，并新增对比气泡高度与同宽纯文本元素的回归探针（`a4684c0`）
- 置顶会话的状态标记保留在置顶标记左侧的车道上，尾部列在每一行都让位给悬停操作簇（`2c8ba9b`）
- 对话框构建器不再每次重绘都重建输入实体，此前会替换用户正在输入的实体（`e069446`）
- MCP 与技能控件回到套件字号体系：搜索框用 medium 尺寸、32px 高、14px 文字，侧栏共用一条 32px 控件带，市场标签真正左对齐（`18d4f6f`）
- MCP 市场获得参考稿的密度：卡片可缩到 260px，使网格能排两到三列；地址胶囊以传输字形开头，两个市场标题都以各自的注册表字形开头（`18d4f6f`）
- 注册 MCP 传输行请求的终端图标，并打包 `chevrons-left.svg` 与 `book-open.svg`（`96133bb`、`9e8db4a`）
- 把 `ScrollGutter` 应用到工作台的滚动区域，使悬浮滚动条不再盖住最后一列（`2e08866`）

**桌面端——会话、侧栏与时间线**

- 会话恢复工作后清除过期的错误圆点：把本地待处理回合投影为运行中并覆盖过期的 `error` 快照，同时 `needs_input` 仍然优先；回合准入和完成现在都会发布会话快照（`555ae3b`）
- 待处理回合锚定到投影构建时刻而不是 Unix 纪元，此前会闪现「已工作 497217h 12m」的标题；并回退到借用视图的会话，使全新会话保留乐观的首条消息（`f7ca797`）
- 输入框的触发 token 以文本内范围高亮，使光标可以进入并像普通字符一样编辑（`57b020f`）
- 折叠的组行显示最有行动价值的成员状态，使用工作区行已有的优先级（`ae3c1a4`）
- 组行与旁边会话行的 Agent 标志列对齐（`7d79054`、`09f3c47`、`731d722`）

**市场**

- 通过在本地索引目录，修复 MCP 注册表无法交互搜索和只能取一页的问题（`2234aab`）
- 技能市场无查询时也能填充，不再在用户输入前一直空白（`2175224`）
- 已安装的 MCP 服务投递到每个能承载它的 Agent，并报告不能承载的 Agent（`69216b2`）
- 空导入按提示上报，并说明没有找到内容的原因（`b248628`、`72a45b4`）
- 市场按已抓取窗口分页，搜索改变结果集时重置或收敛页码（`aef87d6`）
- 删除目录来源管理器、内置精选目录和自造分类标签，使所有用户搜索同一批上游并面对同样的失败模式（`b25f764`）

**Git 与 Windows**

- 通过 `repository_common_dir` 解析变更锁路径，而不再使用比较用的身份值——后者的 verbatim 前缀会被 `CreateFileW` 以 `ERROR_INVALID_NAME` 拒绝，同一个被破坏的值也曾让 `normalized_path` 发出的 `git -C` 调用失败（`9930556`）
- 归一化前先剥离 Windows 命名空间前缀，修复了 Windows 上 32 个 `vibex-git` 测试中的 19 个（`9930556`）
- 让测试临时路径和 Git 自身状态文件留在 Windows 260 字符预算内，并在测试仓库中固定 `core.autocrlf=false`，使逐字节断言不会读回 CRLF（`953a3a3`）

**Agent、ACP 与用量**

- 通过拆分兼容下限与固定版本、并投影路由推导出的凭证名，使 DeepSeek Harness 运行时在固定版本升到 0.4.33 后仍然可用（`16d5681`）
- 通过为限定拼写注册读回别名，使 DeepSeek Harness 模型 id 跨 Adapter 版本可用（`c68814b`）
- 所有 npm ACP Adapter 改用 `spawn(process.execPath, [adapter, ...args])` 启动，而不再在进程内导入——后者让 `process.argv[1]` 指向启动器，静默禁用了只有调用自身入口文件才会启动的 CLI Adapter（`f483d54`）
- 预解包内置的 DeepSeek Harness 运行时，跳过符号链接条目并写入 `.dsh-acp-runtime` 标记，因为 Windows 在没有开发者模式或提权时拒绝创建 `node_modules/.bin` 符号链接（`f483d54`）
- 把 DeepSeek Harness 注册为按回合计量的 Adapter 并提供按请求的 `usage_update`，使增长的读数不再重复丢失上一回合的 token，缩小的读数也不会被当成计数器重置（`624f2e4`）
- 只有每个回合都报告了 API 请求数时才把聚合值标为「API 请求」，在此之前继续标为回合数（`624f2e4`）
- 代理连接测试改在 Tokio 运行时上执行，因为探测的第一次 DNS 或 socket 操作会 panic 掉 GPUI 工作线程并带下工作台（`30a31a9`）
- 修复 DeepSeek 安装清单测试中仅 Windows 出现的路径分隔符断言（`f483d54`）

**构建与质量门**

- 应用仓库自身 `check:rust` 门失败的三处 rustfmt 改动，并把市场大小限制检查中被 clippy 判为 `collapsible_if` 的嵌套 `if` 合并（`8f1d0f8`、`72da172`、`73d143b`、`5fe4936`、`d49a915`）

### 性能

- 每个会话组分屏在自己的 `Entity::cached` 边界后渲染 `SessionGroupPaneView`，使一个分屏的动画不再重建整个分屏树，分屏内部发出的通知也只到达该分屏（`e6f690a`）
- 通过向会话查询回合是否仍在进行，阻止 Agent 思考微光在被打断的回合之后继续存在——此前它会让持有该会话的每个分屏持续重绘（`e6f690a`）
- 输入框的自动继续同步改由改变其输入的事件驱动，而不是渲染路径，并且每个修订每个退避窗口只发起一次探测：四屏会话组中每帧探测重入从 111 次降到 0 次（`e545edc`）
- 分屏以非称重方式借用视图，并由工作区统一走一次称重释放；运行时目录改为共享，级联 memo 按不同选择设界（`e545edc`）
- 预绘制高度测量按会话路由，已缓存的会话视图只称重一次，不再每帧重建行尺寸表（`b97cda4`）
- 外部编辑器探测按进程缓存并由后台执行器预热，因为文件面板头部此前每帧都要探测 PATH × `PATHEXT` × 16 个编辑器——Windows 上每次约 4600 次文件系统查询、约 60 ms（`3a0cd61`）
- 本地 Spinner、启动 wordmark 微光和移动端配对渐变限制为 30 fps，侧栏加载不再驱动整窗重绘（`06223cb`）
- npm 生成的 `.cmd`/`.bat` Node shim 直接用 `process.execPath` 启动，在 Windows 上每个 ACP 会话省去一个 `cmd.exe` 及其控制台宿主（`06223cb`）
- 只有新的外观设置要求时才在窗口失活时暂停工作台的重复动画，后台工作台默认不再被冻结（`06223cb`、`2c72ee8`）
- 会话视图和会话组加载的键改为借用查找，不再每帧为每个成员分配一个自有键；`system_locale` 返回 `&'static str`，不再在每次 `strings()` 调用时克隆缓存字符串（`b97cda4`）

### 底层改动

- 第一方图标包从 191 个文件增至 194 个：面板切换用的 `sidebar-left.svg` 和 `sidebar-right.svg`，以及分屏标签菜单用的 `chevrons-left.svg`，三者都是 Lucide 的 ISC 路径，并连同来源、数量和树摘要登记进许可证策略（`a64eec6`、`a548784`、`9e8db4a`）
- 版本号统一升到 `0.1.0-rc.6`，覆盖工作区、打包输入、Android 版本回退值、README 与部署文档、原生内容包预期，以及许可证策略中已审核的第一方资源包版本
- spec 更新：窗口失活时的渲染暂停记录为可选的外观偏好（`be6e01b`），Git 路径身份与 Windows `MAX_PATH` 契约（`9a3a0e6`），异步探测的执行器边界（`30a31a9`），以及 DeepSeek Harness 的固定版本与兼容下限、第二道凭证闸门契约（`16d5681`）
- 新增或扩展的测试覆盖：会话组分屏视图与焦点路由、分屏与拖拽落点解析、置顶区选择范围、会话组拖拽载荷、MCP 市场索引与分页、原生 MCP 投递分类、CC Switch Grok Build 解析器、已配对设备身份与在线状态、运行时重命名协议、管理器回滚门控、气泡高度与实测文本元素的对比、配置中心导航渲染出的胶囊标签、可选中提示文本、代理模式迁移、已结束输入请求的展开行、工作台分键切换与按会话的编辑器布局（`2232f70`、`a4684c0`、`bdc20d2`、`c8b4873`、`e545edc`、`b97cda4`、`69216b2`、`12cb935`、`6c55376`、`35be91a`、`9432036`、`6d837e2`、`ebcc88b`、`94f909a`、`c0ac5d7`、`c3527ca`、`cf080d6`、`e069446`）
- 依赖迁移到已发布的 `gpui-kit` 0.6.4 及对应的 `gpui-base`，并升级 `gpui-pre-mobile`（`c768bc9`）
