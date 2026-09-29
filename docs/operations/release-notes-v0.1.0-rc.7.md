# Vibex v0.1.0-rc.7 Release Notes

- Released: 2026-09-29 · Range: `v0.1.0-rc.6...v0.1.0-rc.7` · 92 commits

---

## English

### Highlights

- **An embedded browser the Agent can drive** — A browser tool panel beside the terminal and the preview, backed by the system Chrome over CDP. The runtime owns the process, the tabs, the policy and the audit ledger, and the panel and the Agent share one tab: a human watches live frames while the Agent reads the same page's accessibility tree, so what the Agent acts on and what the reader sees cannot diverge.
- **Point at what you see, land in the code — and back** — Alt+click in the page opens the element's source file at its line, and Alt+click on a source line highlights the element that line rendered in the page. Both directions run the same framework probe the Agent uses, and both say why when they cannot answer.
- **The browser tools now reach almost every Agent** — The built-in server travels over the loopback HTTP endpoint or the stateless stdio sidecar, including to Agents whose CLI reads its own MCP file, and each Agent's row states which transport it gets or why it gets none.
- **Approvals that hold** — A domain approval is remembered per full origin in a new table, applies to opening a tab as much as to navigating, and every local file handed to a page asks on its own with no "always allow". Downloads stay denied until the reader turns them on.
- **Files outside the project open in the preview** — Anything the picker returns is previewed read-only, and the tab menu's file browser now picks files, not only folders.
- **The Skill market reads ClawHub** — The registry's own rankings and cursor paging, and a Skill installs as the whole folder: unpacked when the preview opens, treated as untrusted input, and exported to every Agent through the native Skill path.
- **Prompts become a first-class object** — Prompts joins Agents, MCP and Skills as a config-center tab with a list and a single editor, and an enabled Prompt arrives in the composer as a quick phrase under the `/` popup's second tab.
- **Managed Agents stop being repaired on every start** — External Node runtimes resolve through a link under the Vibex root, and a Profile's auth-source revision is a digest of its launch contract, so a restart no longer rewrites durable configuration or invalidates session bindings.
- **Faster and quieter** — Idle repaints are cut across hover fades, sidebar spinners, the turn timer and locale lookups, and a streamed turn stops paying a connection, a commit and a poll for every provider chunk.
- **The GPUI kit moves to 0.7.0** — With gpui-pre 0.3.7: Root owns the dialog, sheet and notification layers, popovers measure their trigger gap with offset, the plot module moves to gpui-base, and the composer inserts picked references as the kit's atomic inline tokens.

### New Features

**Embedded browser**

- The panel is a tool panel, not an application shell: the runtime owns the browser process, its isolated profile, the CDP connection, tab and ref state, policy and the redacted ledger, and clients subscribe through the `BrowserBackend` seam; it opens from the right rail, from the editor header beside the terminal entry, and from the tab strip's "+" menu (`8b7b7e9`, `e144dad`, `75a01fd`)
- The transport is pipe-first (fd 3/4, NUL-delimited) with a loopback websocket fallback read from `DevToolsActivePort`; framing belongs to the transport, so no caller appends a terminator (`8b7b7e9`, `a7a7289`)
- Navigation: back, forward and reload buttons that disable when the tab has nowhere to go, the mouse's side buttons, and a right-click page menu with back, forward, reload, copy, paste and select all; the reload glyph is the circular arrow a browser uses (`f625d8d`, `171c446`, `e1e8028`)
- Tabs: a tab a page opens (`target="_blank"`, `window.open`) is adopted as a preview tab of its own, a tab closed from the page closes its preview tab, closing a preview tab destroys the runtime tab instead of leaving its screencast running, and a tab an Agent opened is shown with a Bot mark that turns green while the Agent drives it (`b525944`, `66679a6`, `5fd55b6`, `aa33e67`)
- A tab's label is the page's title with a spinner while it loads, plus the site's own icon fetched by the runtime and a width limit that ellipsizes (`66679a6`, `e1e8028`)
- A frame stream that ends or fails re-subscribes instead of freezing the last picture, and showing a tab makes it the browser's active target, which a page needs to keep its own timers and animations running (`90ab6f8`, `0238fab`)
- Input: pointer and keyboard events travel as viewport CSS pixels; the wheel sign is flipped for CDP, held buttons travel as CDP's `buttons` mask, Space and Enter carry the text Chrome would have generated, Ctrl/Cmd+C/X/V use the system clipboard, F5/Ctrl+R reload and Ctrl+L focuses the address bar (`5a4684a`, `ac451cf`, `65c8ab2`, `a8da112`, `9a52f05`)
- A page `<select>` opens the panel's own list (probed on hover, so the click is not delayed past its own release), and a page's file chooser opens the workbench's file browser and hands the confirmed path to its `<input type=file>` (`112359b`, `9a52f05`)
- Find in page walks the page's text nodes and paints hits with the CSS Custom Highlight API rather than inserting `<mark>` nodes, which would break a framework's next render; the active hit gets one marker attribute, because Chrome will not scroll to a bare range (`aa33e67`)
- The panel mirrors the computed cursor of the element under the pointer — one probe per round trip, and a keyword the platform cannot express falls back to the arrow — because a screencast frame carries no cursor (`aa33e67`)
- Downloads are denied until the reader allows them and then land in the runtime's own `downloads` directory under a sanitized name that never overwrites; the panel shows a progress popup with the saved path, a folder button that creates and reveals the directory, and one announcement per save (`aa33e67`, `c9f1915`, `9a52f05`)
- An HD toggle restarts the screencast as lossless PNG beside the default JPEG 80, and the choice is a preference the settings keep (`e06d4a7`)
- The panel's activity button docks the runtime's redacted operation ledger under the page and appends Agent actions as they happen; a banner states while a recording is keeping raw form values in memory (`1f0cb32`, `a96f3da`)
- A dev server printed by any terminal tab is detected from the runtime's PTY reader, joins the workspace's allow-list only after its port answers, and is offered as a notification rather than navigated to (`1c7c1cf`)
- Browser settings choose the new-tab start page (Google, Bing, Baidu or a custom address) and the address-bar search engine (Google, Bing, Baidu, DuckDuckGo or a custom `{query}` template); the panel's own tabs use them, and an Agent's tabs are left alone (`df03fcf`)
- A browser tab survives a restart: the address is remembered on the preview target, the runtime tab is recreated there, and the preview tab, its surface, its subscription and its label are rebound; a tab with no address opens the start page, and every failure states its reason on the surface (`ade29a4`)
- Pausing is the reader's explicit act: a page the Agent drives floats a play/pause button with an animated bloom, the tab strip turns green while the Agent drives and muted once it is paused, and clicking, scrolling or typing never pauses the Agent (`197fef5`, `2e6e35a`)

**Agent browser tools**

- The built-in browser server is delivered as wire MCP over the loopback HTTP endpoint, with the stateless stdio sidecar forwarding to the same handler, and it now also reaches Agents whose CLI reads its own MCP file — a built-in is never written to that file, so the double-registration reason for keeping that tier empty does not apply; `pi` and `factory-droid` receive nothing and say so (`8b7b7e9`, `b0f6fb2`, `4dc4a90`, `6a7d58f`)
- The session bearer token is derived once in `vibex-core` as `btok_<session id>_<mac>` and verified with the same function, so both transports authenticate (`ad7e8c1`)
- The 31 tools keep their coarse, fine and visual tiers, and page-derived output is fenced between explicit untrusted-content delimiters instead of trusting a notice sentence (`8b7b7e9`, `c81194a`)
- A cross-origin frame is read with its own `Accessibility.getFullAXTree` and merged into the observation with a shared element budget, and each merged element carries the session its DOM calls must run on (`9cd20d0`)
- `browser_upload` and `browser_preview_open` ask the human first through `browser_local_file_approval_required`, with the file names on the card and no "always allow", because being inside an authorized root is not consent to send a file out of the workspace (`9cd20d0`)
- Navigation is gated everywhere: `browser_create_tab` obeys the same policy as `browser_navigate`, a one-off approval travels with the retry instead of being remembered, and durable grants are keyed by the full origin in a new table (migration 59) (`6cde975`)
- A tool call holds an operation guard over its tab for its duration so the tab reclaimer cannot close a tab in use, and background sessions are capped with the oldest idle ones closed first (`6cde975`)

**Browser ↔ code**

- Alt+click in the page resolves the element under the pointer and opens its file at the reported line through the same probe the Agent's `browser_element_source` runs; the click never reaches the page, and a miss — no framework hook, no element, no exact line — states its reason inline (`ae3e82f`)
- Alt+click on a source line highlights the element that line rendered in the page, scrolls it into view and draws the same `Overlay.highlightNode` box an Agent's action uses; the page is not rewritten, every visible panel is asked in turn, and a miss is visible (`6aec9f4`)

**Preview & files**

- Open files outside the project in the preview panel: the desktop client reads them directly, shows them read-only (a save is refused with an explanation), and keeps the absolute path while in-project files keep the workspace-relative form the backend expects (`904c7be`, `3d83253`)
- The tab menu's file browser picks files as well as folders: the listing adds files, a row click selects instead of descending, and the confirmed path opens in the preview panel (`d41d783`)

**Config center**

- Prompts becomes a primary config-center tab with a searchable list of one card per record and a single editor for the selected Prompt; the list and the form read one `selected_prompt_id`, a row previews the Prompt body instead of repeating the kind and scope every record shares, and leaving the tab with unsaved edits asks before discarding them (`4d203b5`, `37a2fb7`)
- Enabled Prompts appear in the composer's `/` popup under a quick-phrase tab, inserted verbatim at the caret and ordered by use count then by most recently edited; the Config Center form creates and edits the same record, and each row can be enabled or disabled, which is what admits it to the phrase tab (`b1e0d21`)
- The quick-phrase popup measures its footer and its taller rows, previews the text the phrase would insert on a wrapped second line, and caps a long name so it cannot collapse the preview (`8c3dadb`)
- The MCP market pages the whole registry by offset: the index ceiling moves to 40,000 entries with a six-hour TTL, a page is 20 entries, the transport filter narrows the result before the page is cut, and changing a page returns the grid to the top (`5f575e8`)
- One selected treatment for the management sidebar's resource rows, the transport label matches the wording the market card uses, the result summary reads at body size, and a clamped card description ends with an ellipsis instead of being cut mid-word (`f467d7d`)

**Skills**

- The Skill market reads ClawHub instead of a document index: it lists and ranks its own catalog, pages by cursor, and publishes the Skill folder as an archive, so browsing no longer needs a stand-in query and the market can page (`3d95486`)
- A Skill installs as a folder: the archive is unpacked when the preview opens (the preview is the disclosure, and the files travel back with the install request), entry names are confined to the Skill folder, directories and non-UTF-8 entries are skipped, per-file, file-count and total-byte ceilings bound a decompression bomb, and every refused file is reported (`3d95486`)
- The folder is stored beside the database and handed to each Agent through the native Skill export, so one install reaches Claude, Codex, Cursor, Gemini, OpenCode and the default source (`3d95486`)

**Agent management**

- Pin a managed Agent runtime to a stable entry point: an external Node runtime resolves through a link under the Vibex root, the shell-scoped path leaves the install fingerprint, and a repaired Agent publishes a catalog invalidation so selectors do not wait for the rest of the probe (`64f3c7b`)
- Key a Provider Profile's auth-source revision on a digest of the fields the ACP launch and restore contract depends on, so a cosmetic edit keeps session bindings usable while an endpoint, credential, command or status change still rebuilds them (`eeb55ac`)
- Expose an OpenCode auto-accept mode: the inline-provider overlay injects a Vibex-managed `vibex-auto` agent that appears in the session mode selector and allows edit, bash, webfetch and websearch, plus `external_directory: allow` so workspace-external reads stop prompting, while `read` and `doom_loop` keep their defaults (`af8a9fa`, `7594113`)
- Start a Codex session on the selected profile model by merging a single-model launch into the adapter's Codex config override, and record the selection in the spawn snapshot so choosing another model restarts the process instead of mutating one whose startup override names the old model (`b5dc05d`)
- Route Claude profile models through the Claude Code session settings tier: each configured model gets a distinct alias slot that is projected into the tier applied last, so a configured model stays resolvable without editing a user file (`9d71fcc`)
- A queued message survives local write contention: switch and selection transactions take the write lock before their reads and retry transient `SQLITE_BUSY_SNAPSHOT` instead of reporting "message not sent" (`8530dd5`)

**Composer**

- A `/command`, `@file` or `$skill` picked from the suggestion menu is inserted as an atomic inline token with the kit's `replace_range_with_token`: the caret steps over it, one backspace deletes it whole, a click on a file token opens that path, and the document still holds exactly the text the Agent is sent, with the trailing separator outside the token (`3f47e3c`)

**Workbench & settings**

- Find follows focus: the workbench offers Ctrl+F to the focused editor, then to the focused browser panel, and only a conversation that really holds the keyboard opens the session search; a session group renders the bar above its focused pane's composer, and only that pane highlights the query (`d793b4f`, `9a52f05`)
- A session drag acts on the rows the sidebar paints as selected, including the current session's row while a selection is on screen, so the highlighted set and the moved set are the same rows in both directions (`b699a38`, `7b0c3e9`)
- Settings rows decide stacked or inline from the page width rather than the window width, the value column stays shrinkable with a label floor, and byte sizes join their unit with a non-breaking space so a wrapped value cannot read as two numbers (`68f8be5`)
- The developer FPS HUD opens centred in the window every time it is switched on, is dragged as an offset from that middle clamped inside the window, and keeps a drag hitbox that a cached boundary would have collapsed (`87a2268`, `4b52031`)
- The title-bar update icon paints in the theme's success colour, opening About from the update panel no longer panics on a re-entrant workbench read, its release facts are a grid that no longer grows a blank band, and the release-notes box consumes its own wheel events (`7581dff`)
- The editor keeps its selection visible while a context menu, dialog or another pane holds the keyboard, by mirroring the range as a text-background decoration that follows edits and clears on focus (`a6c62ec`)
- "Pause animation when inactive" reaches the frame drivers the motion gate cannot: the sidebar spinners, the terminal cursor blink and the turn timer now ask one shared decision point, so turning the preference off no longer freezes a backgrounded workbench (`beacd98`)
- The Chinese copy calls the feature Skill rather than 技能 — the composer's `$` trigger and prompt placeholders, and the management sidebar and market headings (`1f51aa9`, `5f575e8`)

**Developer experience**

- Move the workspace to the published `gpui-kit` 0.7.0 and `gpui-pre` 0.3.7 in one step, with `gpui-pre-mobile` on the revision that builds against 0.3.7 (`64751bb`)
- Adopt the 0.7.0 APIs: Root owns the dialog, sheet and notification layers and the workbench registers a `gpui_base::RootPlugin` hint layer so hints paint above a dialog backdrop, client-decorated windows take the kit's own frame, `Popover` measures its trigger gap with `offset`, the plot module and its `chart.grid` token come from gpui-base, `DatePickerEvent::Change` carries a `DateTime`, and `Theme::update` replaces the manual global-mutate-and-sync pair (`64751bb`)

### Fixes

**Browser**

- Open a tab on Windows: the websocket transport no longer sends the pipe's NUL terminator inside its text frame, and the loopback endpoint file is removed before spawning and refused when it predates the launch, so a leftover `DevToolsActivePort` can no longer point at a dead browser (`a7a7289`)
- Stop starting the user's own Chrome on every launch: `--version` is not a version switch on Windows, so detection no longer runs the probe there and elsewhere runs it against a deadline that kills a browser which never answers (`a7a7289`)
- Poll the browser service inside the Tokio runtime: `LocalBrowserTransport` installs the runtime for each poll, the frame stream included, which fixes the first click of the browser entry crashing with "there is no reactor running" (`9c9a00c`)
- Decide the browser child's proxy configuration at launch instead of inheriting it, so a Clash-style `all_proxy=socks5://…` is dropped when both scheme variables exist and otherwise becomes `--proxy-server`, instead of every page failing with `ERR_EMPTY_RESPONSE` (`5d7d9cb`)
- Keep the requested URL when a page fails to load: the internal `chrome-error://chromewebdata/` URL stays out of the tab, while the error page still renders and the failure stays in the network diagnostics (`1f66568`)
- Wire the events the panel was dropping — page dialogs, file choosers, a session whose execution source flipped, and availability changes — and make only deliberate input take over from the Agent, with a hand-back that calls `resume_agent_operations` and announces itself (`2e6e35a`)
- Drop an approval card the user already stopped: the wait now checks whether the session's tab was aborted and denies immediately instead of sitting out the rest of its 120-second TTL (`3079c7d`)
- Announce each download once: a save Chrome reports per session and per tab no longer produces two identical notifications and two ledger rows, and the popup's folder button reveals the runtime's download directory, creating it when missing (`c9f1915`)
- Complete the panel's input and download feedback: Space and Enter travel as `keyDown` with their text, Ctrl+X copies before the page cuts, F5/Ctrl+R reload, Ctrl+L focuses the address bar, page-file choosers open the workbench's browser, downloads get progress and refused saves get a warning row, and the temporary address-bar diagnostics are gone (`9a52f05`)
- Make dragging select text: a move names the held button so Blink runs the selection gesture, a released move still says none, and focus emulation is enabled per tab because a headless browser has no window focus (`65c8ab2`)
- Keep captures reproducible and textures released: both visual-regression captures are pinned to the default viewport at scale 1 and wait for network idle, replaced favicons and frames parked on a tab switch hand their `Arc<RenderImage>` back through `pending_drop`, and `deviceScaleFactor` follows the window instead of a hardcoded 1.0 (`c81194a`)
- Remove the blocking risk notice that made the browser unusable: the modal gate, its event, the parked request and the UI-state field are gone, and opening the browser now opens the browser (`93ab323`, `7ee8db3`)
- Drop the docked inspector and the element picker: Chrome DevTools cannot attach over `--remote-debugging-pipe` without opening a loopback debug port that would let any local process drive the browser, so both entries and their runtime and transport surface are gone, while the toolbar, context menu, selection, site icon and label limit stay (`228ec67`, `068c4e7`, `ad5a4d8`)
- Keep the remote panel honest: a panel served by a paired runtime reports itself degraded and the settings card says the display choices configure nothing on the machine running the page; the remote browser frame transport is explicitly not enabled, answering an attach with a resync requirement and a frame with a capability error rather than silence (`8b7b7e9`, `6a7d58f`)

**Market & management**

- Keep the market's pager on screen: the pane now decides whether the body scrolls itself, so the grid keeps a definite height and the pager stays put while the cards scroll under it; the import form keeps the pane's own scrolling because it is longer than the viewport (`0c0d534`, `053b793`)
- Give the unscrolled pane the window's height back, which the removed scroll wrapper had been supplying, so the market's grid is no longer centred down an empty pane (`e638395`)
- Page the MCP market over the whole registry instead of the first indexed window: the request carries the transport filter, the runtime honors `offset` and reports `total_matches`, and the Skill market keeps paging locally through the window it fetched because its index ignores `offset` (`5f575e8`)

**Workbench & settings**

- Size setting rows by the page and wrap long values: the value column stays shrinkable, the label column keeps a floor, and the storage chip's text sits in a `min_w_0().flex_1()` box that gives it a definite width to wrap at, fixing a label squeezed to one character per line (`68f8be5`)
- Repair the update entry and About: the update icon uses the success colour, the About update check runs one tick later so it cannot read the workbench while it is already being updated, and the release facts are a grid (`7581dff`)
- Move the rows the sidebar paints as selected, so a shift/ctrl selection that includes the current session's row drags every highlighted row and carries its count badge (`b699a38`, `7b0c3e9`)
- Give find to the focused surface: a file editor's Ctrl+F opens the file's own find, a browser panel's opens the page's find, and only a conversation with the keyboard falls through to the session search (`d793b4f`)
- Keep the editor's selection painted under a context menu by mirroring the range as a decoration while the editor is unfocused (`a6c62ec`)
- Keep self-driven animations running when the inactive pause is off, so a backgrounded workbench with a running session is not frozen by default (`beacd98`)

**Agent, ACP and runtime**

- Start Codex sessions on the selected profile model instead of failing `apply_session_config` with `runtime_switch_configuration_unavailable` when the model is outside the adapter's built-in catalogue (`b5dc05d`)
- Route Claude profile models through the session settings tier so a model outside the picker rows Claude Code derives from `ANTHROPIC_DEFAULT_*_MODEL` stays resolvable, and drop `ANTHROPIC_MODEL` when it names an assigned model (`9d71fcc`)
- Keep a runtime switch reservation alive under local write contention by taking the write lock before the reads and retrying transient `SQLITE_BUSY_SNAPSHOT` around the durable switch writes and the submission drive loop (`8530dd5`)

### Performance

- Cut idle repaints and per-frame work: hover fades notify their owning view instead of calling `Window::refresh` and defeating every `.cached()` subtree, sidebar status spinners run at 10fps and draw still in an inactive window, the turn timer and terminal cursor blink skip their repaints while inactive, the FPS HUD renders behind a cache boundary, the resolved locale is stored once instead of being re-parsed twice per call, the legacy-folder migration memoises its inputs, the batch "select all" check stops allocating per session, and the session-view cache budget drops to six views (`80f4b4d`)
- Stop paying a connection, an fsync and a poll per streamed chunk: `synchronous = NORMAL` pairs with WAL, the streamed-append path reuses one warm write connection, a migration drops the index that duplicated the timeline primary key, `wait_for_terminal` parks on a progress signal and re-reads once a second instead of at 40Hz, the ACP diagnostics ring records stderr only, the shared Tokio runtime scales its workers with the host, and the per-chunk timeline lookup becomes constant (`e934ec0`)
- Back off instead of burning frames in the background: attachments no longer break the 16ms timeline batch, the token-usage snapshot reads on the background executor instead of migrating the database on the UI thread, and the selected session's fallback poll backs off while the window is inactive (`80f4b4d`)

### Under the hood

- Version numbers were bumped to `0.1.0-rc.7` across the workspace, the packaging inputs, the Android version fallback, the README and deployment documentation, the native-content package expectations, and the reviewed first-party asset-bundle versions in the license policy
- New database work in this range: migration 57 adds the browser audit tables, migration 59 re-keys browser grants by origin in `browser_origin_grants`, migration 60 adds `prompt_usage` for quick-phrase ordering, and the streamed-append migration drops `idx_agent_timeline_session_sequence` (`8b7b7e9`, `6cde975`, `b1e0d21`, `e934ec0`)
- Dependencies moved to the published `gpui-kit` 0.7.0 with `gpui-pre` 0.3.7 and a `gpui-pre-mobile` revision that builds against it, and the license SBOM and notices were regenerated for the new graph (`64751bb`)
- Spec updates: the embedded browser's ownership, invariants, security rules, panel wiring, approvals and testing contracts (`2382453`, `fbb970a`, `98bd1e3`, `0d568f9`, `735ceb2`, `b0f6fb2`, `197fef5`, `9cd20d0`, `c81194a`, `6cde975`), composer references as atomic tokens (`3f47e3c`), settings rows measured by page width (`68f8be5`), Codex's single-model startup selection and Claude's session settings tier (`b5dc05d`, `9d71fcc`), the OpenCode `vibex-auto` agent (`af8a9fa`, `7594113`), the mirrored editor selection (`a6c62ec`), and the review policy for dependency sources (`64751bb`)
- New or extended test coverage: real-Chrome suites for find, cursor, select menus, clipboard, downloads, file choosers, auth challenges, permission prompts and frame streams (`e5430ec`, `1d6fffa`, `aa33e67`, `112359b`, `a8da112`, `90ab6f8`), page tabs and the leftover `DevToolsActivePort` restart case (`b525944`, `a7a7289`), the executor seam that reproduced the first-click crash (`9c9a00c`), the CDP framing rules against both transports (`a7a7289`), the browser contract probe and its delivery matrix (`8b7b7e9`, `4dc4a90`, `6a7d58f`), origin grants and their live set (`6cde975`), the local-file approval key (`9cd20d0`), the settings-row layout at both page widths (`68f8be5`), the drag set and its payload (`b699a38`, `7b0c3e9`), the find chord reaching the focused surface (`d793b4f`, `9a52f05`), the prompt-usage ordering (`b1e0d21`), the motion truth table behind the inactive pause (`beacd98`), the FPS HUD drag (`4b52031`), and the About card and update entry (`7581dff`)
- The browser smoke runs on `windows-latest` as well as `ubuntu-latest`, because Windows is the platform whose CDP transport is the loopback port, and a browser test may skip only when no browser is installed (`a7a7289`)
- Build and formatting repairs that kept the quality gates green: the `IdleBrowserTransport` method the clipboard commit missed (`b68906e`), the motion test's stand-in owner id built at runtime because this gpui version has no const `EntityId` constructor (`5044e4b`), and rustfmt-only commits the gate required (`b8381bb`, `641a945`, `bf6ad44`)

## 中文

### 亮点

- **一个 Agent 可以驱动的内嵌浏览器** — 与终端、预览并列的浏览器工具面板，由系统 Chrome 通过 CDP 支撑。运行时拥有进程、标签、策略和审计账本，面板与 Agent 共用同一个标签：人看实时画面，Agent 读同一个页面的无障碍树，因此 Agent 操作的对象与读者看到的内容不会分叉。
- **指着看到的东西，直接落到代码——再反过来** — 在页面里 Alt+点击会打开该元素所在的源文件并定位到行；在源码行上 Alt+点击，页面会高亮这一行渲染出的元素。两个方向都走 Agent 使用的同一个框架探测，答不出来时也都会说明原因。
- **浏览器工具现在几乎能到达每个 Agent** — 内置服务通过回环 HTTP 端点或无状态 stdio sidecar 投递，包括那些 CLI 读取自己 MCP 文件的 Agent；每个 Agent 的行会说明它拿到哪种传输，或者为什么完全拿不到。
- **站得住脚的授权** — 域名授权按完整 origin 记录在新表中，打开标签与导航受同一套策略约束，任何交给页面的本地文件都会单独询问且没有「始终允许」。下载默认拒绝，直到读者主动开启。
- **项目外的文件也能在预览中打开** — 选择器返回的任何文件都以只读方式预览，标签菜单的文件浏览器现在也能选文件，而不只是文件夹。
- **技能市场改读 ClawHub** — 使用注册表自己的排名和游标分页；技能以整个文件夹安装：打开预览时解包、按不可信输入处理，并通过原生技能导出送达每个 Agent。
- **Prompt 成为一等对象** — Prompt 与 Agents、MCP、Skills 并列成为配置中心的一个标签，左侧是列表、右侧是唯一编辑器；启用的 Prompt 会作为快捷短语出现在输入框 `/` 弹窗的第二个标签页里。
- **托管 Agent 不再每次启动都被「修复」** — 外部 Node 运行时统一通过 Vibex 根目录下的链接解析，Provider Profile 的凭证源修订号改为启动契约的摘要，重启不再改写持久配置或让会话绑定失效。
- **更快、更安静** — 悬停淡入淡出、侧栏加载指示器、回合计时器和区域设置查询带来的空闲重绘被削减；流式回合不再为每个供应商分块付出一次连接、一次提交和一次轮询。
- **GPUI 套件升到 0.7.0** — 同时升级 gpui-pre 0.3.7：对话框、面板和通知层由 Root 拥有，Popover 用 offset 度量触发间隙，图表模块移入 gpui-base，输入框把选中的引用插入为套件的原子化行内 token。

### 新功能

**内嵌浏览器**

- 面板是工具面板而不是应用外壳：运行时拥有浏览器进程、隔离配置目录、CDP 连接、标签与 ref 状态、策略和已脱敏的账本，客户端通过 `BrowserBackend` 接缝订阅；可从右栏、编辑器头部终端入口旁，以及标签栏的「+」菜单打开（`8b7b7e9`、`e144dad`、`75a01fd`）
- 传输以管道为主（fd 3/4，NUL 分隔），并保留从 `DevToolsActivePort` 读取的回环 websocket 回退；分帧属于传输层，调用方不再追加终止符（`8b7b7e9`、`a7a7289`）
- 导航：前进、后退和刷新按钮在无处可去时禁用，鼠标侧键走同一条路径，右键页面菜单提供后退、前进、刷新、复制、粘贴和全选；刷新图标改用浏览器惯用的环形箭头（`f625d8d`、`171c446`、`e1e8028`）
- 标签：页面自己打开的标签（`target="_blank"`、`window.open`）会成为独立的预览标签，页面关闭的标签会关闭对应预览标签，关闭预览标签会销毁运行时标签而不是留下还在串流的画面；Agent 打开的标签带 Bot 标记，Agent 驱动期间变绿（`b525944`、`66679a6`、`5fd55b6`、`aa33e67`）
- 标签标题显示页面标题并在加载时显示 Spinner，站点图标由运行时抓取，标签宽度有上限并省略（`66679a6`、`e1e8028`）
- 结束或失败的画面流会重新订阅，而不是把最后一帧冻在屏幕上；显示某个标签时也会把它设为浏览器的活动目标，页面因此不会误以为自己被隐藏而停掉自己的计时器和动画（`90ab6f8`、`0238fab`）
- 输入：指针和键盘事件以视口 CSS 像素传递；滚轮符号为 CDP 翻转，按住的按键以 CDP 的 `buttons` 掩码传递，空格和回车携带 Chrome 本应生成的文本，Ctrl/Cmd+C/X/V 使用系统剪贴板，F5/Ctrl+R 刷新，Ctrl+L 聚焦地址栏（`5a4684a`、`ac451cf`、`65c8ab2`、`a8da112`、`9a52f05`）
- 页面 `<select>` 打开面板自己的列表（在悬停时探测，避免点击晚于自身释放才到达页面），页面的文件选择器打开工作台自己的文件浏览器并把确认路径交给 `<input type=file>`（`112359b`、`9a52f05`）
- 页内查找遍历页面文本节点并用 CSS Custom Highlight API 绘制命中，而不是插入 `<mark>` 节点——后者会破坏框架的下一次渲染；当前命中只打一个标记属性，因为 Chrome 不会滚动到裸 range（`aa33e67`）
- 面板映射指针所指元素的计算光标（每次往返只探测一次，平台表达不了的关键字回退为箭头），因为画面串流本身不携带光标（`aa33e67`）
- 下载默认拒绝，读者允许后落入运行时自己的 `downloads` 目录，使用经净化的文件名且从不覆盖；面板显示带保存路径的进度弹窗、一个按需创建并打开目录的文件夹按钮，每次保存只通告一次（`aa33e67`、`c9f1915`、`9a52f05`）
- HD 开关把画面串流切换为无损 PNG，默认仍是 JPEG 80，该选择作为偏好被设置保留（`e06d4a7`）
- 面板的活动按钮把运行时的脱敏操作账本停靠在页面下方，并在 Agent 操作发生时追加；录制期间会显示横幅说明原始表单值仍留在内存中（`1f0cb32`、`a96f3da`）
- 任何终端标签打印出的开发服务器都会从运行时 PTY 读取器中被检测到，端口答复后才加入本工作区的允许列表，并且只以通知形式提供而不是直接导航（`1c7c1cf`）
- 浏览器设置可以选择新标签的起始页（Google、Bing、百度或自定义地址）和地址栏搜索引擎（Google、Bing、百度、DuckDuckGo 或带 `{query}` 的自定义模板）；面板自己创建的标签遵循它们，Agent 的标签不受影响（`df03fcf`）
- 浏览器标签可以跨重启恢复：地址记在预览目标上，运行时标签按该地址重建，预览标签、其界面、订阅和标题一并重新绑定；没有地址的标签打开起始页，每一种失败都会在界面上说明原因（`ade29a4`）
- 暂停是读者的明确动作：Agent 正在驱动的页面会浮出一个带动画光晕的播放/暂停按钮，Agent 驱动时标签栏变绿、暂停后转为静音色；点击、滚动或输入永远不会暂停 Agent（`197fef5`、`2e6e35a`）

**Agent 浏览器工具**

- 内置浏览器服务以 wire MCP 形式通过回环 HTTP 端点投递，无状态 stdio sidecar 转发到同一个处理器；它现在也能到达那些 CLI 读取自己 MCP 文件的 Agent——内置服务从不写入该文件，所以「重复注册」这一拒绝理由并不适用；`pi` 与 `factory-droid` 仍然什么都拿不到，并如实标注（`8b7b7e9`、`b0f6fb2`、`4dc4a90`、`6a7d58f`）
- 会话 bearer token 在 `vibex-core` 中统一按 `btok_<session id>_<mac>` 派生，并用同一个函数验证，两种传输因此都能通过认证（`ad7e8c1`）
- 31 个工具保留粗粒度、细粒度和视觉三档，页面来源的输出被明确的不可信内容分隔符围起来，而不再只依赖一句提示（`8b7b7e9`、`c81194a`）
- 跨源 iframe 用自己会话的 `Accessibility.getFullAXTree` 读取并与主观察合并、共用元素预算；合并进来的元素携带其 DOM 调用必须运行的会话（`9cd20d0`）
- `browser_upload` 与 `browser_preview_open` 先通过 `browser_local_file_approval_required` 询问用户，卡片列出文件名且没有「始终允许」——位于授权根目录内并不等于同意把文件送出工作区（`9cd20d0`）
- 导航处处受限：`browser_create_tab` 与 `browser_navigate` 走同一套策略，一次性批准随重试传递而不被记住，持久授权按完整 origin 记录在新表（迁移 59）中（`6cde975`）
- 工具调用期间对所在标签持有操作守卫，回收器因此不能关闭正在使用的标签；后台会话也有上限，超出时先关闭最久空闲的（`6cde975`）

**浏览器与代码**

- 在页面里 Alt+点击会解析指针下的元素，并走 Agent 的 `browser_element_source` 所用的同一个探测，打开对应文件并定位到行；这次点击不会到达页面；没有框架钩子、没有元素、没有精确行等失败都会内联说明原因（`ae3e82f`）
- 在源码行上 Alt+点击会高亮该行渲染出的元素、滚动到可见位置，并绘制与 Agent 操作相同的 `Overlay.highlightNode` 方框；页面不会被改写，每个可见面板依次被询问，失败也可见（`6aec9f4`）

**预览与文件**

- 在预览面板中打开项目外的文件：桌面客户端直接读取，以只读方式显示（保存会被拒绝并给出说明），并保留绝对路径；项目内文件仍保持后端期望的工作区相对形式（`904c7be`、`3d83253`）
- 标签菜单的文件浏览器现在也能选文件：列表加入文件，点击行是选中而不是进入目录，确认的路径在预览面板中打开（`d41d783`）

**配置中心**

- Prompt 成为配置中心的一级标签：可搜索的列表每条记录一张卡片，主体是所选 Prompt 的唯一编辑器；列表和表单读取同一个 `selected_prompt_id`，行内预览 Prompt 正文而不是重复每条记录都相同的类型与范围，带着未保存修改离开标签会先询问（`4d203b5`、`37a2fb7`）
- 已启用的 Prompt 出现在输入框 `/` 弹窗的快捷短语标签页中，按原样插入光标处，排序先按使用次数再按最近编辑；配置中心表单创建和编辑的就是同一条记录，每行都可以启用或停用，这也正是它进入短语标签页的条件（`b1e0d21`）
- 快捷短语弹窗把页脚和更高的行计入高度，在换行的第二行预览该短语将插入的文本，并限制过长的名称，避免它把预览挤没（`8c3dadb`）
- MCP 市场按偏移分页遍历整个注册表：索引上限提高到 40,000 条、目录 TTL 延长到六小时，每页 20 条，传输筛选在切页之前收窄结果，切换页码会回到网格顶部（`5f575e8`）
- 管理侧栏的资源行统一一种选中样式，传输标签与市场卡片用词一致，结果摘要改为正文尺寸，被截断的卡片说明以省略号结尾而不是从词中间切断（`f467d7d`）

**技能**

- 技能市场改读 ClawHub，而不再是只发布文档的索引：它自己列出并排序目录、按游标分页，并把技能文件夹作为归档发布，因此浏览不再需要替代查询，市场也能真正分页（`3d95486`）
- 技能以文件夹为单位安装：归档在打开预览时解包（预览就是披露，文件随安装请求一起回传），条目名被限制在技能文件夹内，目录和非 UTF-8 条目被跳过，单文件、文件数和总字节上限约束解压炸弹，每个被拒绝的文件都会被报告（`3d95486`）
- 该文件夹存放在数据库旁，并通过原生技能导出交给每个 Agent，因此一次安装即可到达 Claude、Codex、Cursor、Gemini、OpenCode 和默认来源（`3d95486`）

**Agent 管理**

- 把托管 Agent 运行时固定到稳定入口：外部 Node 运行时通过 Vibex 根目录下的链接解析，shell 作用域路径不再进入安装指纹，修复后的 Agent 立即发布目录失效通知，选择器无需等待其余探测（`64f3c7b`）
- Provider Profile 的凭证源修订号改为 ACP 启动与恢复契约所依赖字段的摘要，外观性编辑保留会话绑定可用，而端点、凭证、命令或状态变化仍会重建它们（`eeb55ac`）
- 暴露 OpenCode 自动接受模式：内联供应商覆盖层注入一个 Vibex 管理的 `vibex-auto` agent，出现在会话模式选择器中，允许 edit、bash、webfetch 和 websearch，并以 `external_directory: allow` 让工作区外的读取不再弹窗，同时 `read` 与 `doom_loop` 保留默认行为（`af8a9fa`、`7594113`）
- 让 Codex 会话在所选的 Profile 模型上启动：把只带一个模型的启动合并进 Adapter 的 Codex 配置覆盖，并把该选择记入进程启动快照，使选择另一个模型时重启进程，而不是改写一个启动覆盖仍指向旧模型的过程（`b5dc05d`）
- 把 Claude Profile 模型接到 Claude Code 的会话设置层：为每个配置的模型分配不同的别名槽并投影到最后生效的设置层，使配置的模型无需修改用户文件即可解析（`9d71fcc`）
- 排队中的消息能在本地写入争用中存活：切换和选择事务先取写锁再读取，并对瞬时的 `SQLITE_BUSY_SNAPSHOT` 重试，而不是报「消息未发送」（`8530dd5`）

**输入框**

- 从建议菜单选中的 `/` 命令、`@` 文件和 `$` 技能以原子化行内 token 插入（套件的 `replace_range_with_token`）：光标一步跨过、一次退格整体删除、点击文件 token 打开该路径，文档中仍然正好是发给 Agent 的文本，尾部分隔符留在 token 之外（`3f47e3c`）

**工作台与设置**

- 查找跟随焦点：工作台先把 Ctrl+F 交给聚焦的编辑器，再交给聚焦的浏览器面板，只有真正持有键盘的对话才打开会话搜索；会话组在聚焦分屏的输入框上方渲染查找栏，也只有该分屏高亮查询（`d793b4f`、`9a52f05`）
- 拖动会话作用于侧栏绘制为选中的那些行，包括选择可见时的当前会话行，因此高亮集合与移动集合在两个方向上都一致（`b699a38`、`7b0c3e9`）
- 设置行按页面宽度而不是窗口宽度决定堆叠还是同行，值列保持可收缩且标签列有下限，字节大小与单位之间使用不换行空格，换行后的值不会被读成两个数字（`68f8be5`）
- 开发者 FPS HUD 每次打开都居中显示，拖动是相对中心的偏移并限制在窗口内，拖动命中区也不再因缓存边界而塌缩为零（`87a2268`、`4b52031`）
- 标题栏更新图标使用主题的成功色，从更新面板打开 About 不再因重入读取工作台而 panic，发布信息改为网格布局、不再长出空白带，发布说明框自己消费滚轮事件（`7581dff`）
- 右键菜单、对话框或另一个分屏持有键盘时，编辑器仍显示自己的选区：把范围镜像为随编辑移动、聚焦时清除的文本背景装饰（`a6c62ec`）
- 「窗口失活时暂停动画」现在也覆盖动效闸门触及不到的帧驱动：侧栏指示器、终端光标闪烁和回合计时器共用同一个判断点，关闭该偏好后后台工作台不再被冻结（`beacd98`）
- 中文文案把该功能称为 Skill 而不是「技能」——包括输入框的 `$` 触发与提示占位符，以及管理侧栏和市场标题（`1f51aa9`、`5f575e8`）

**开发者体验**

- 工作区一次性迁移到已发布的 `gpui-kit` 0.7.0 与 `gpui-pre` 0.3.7，`gpui-pre-mobile` 使用可对 0.3.7 构建的修订版（`64751bb`）
- 采用 0.7.0 的新 API：对话框、面板和通知层改由 Root 拥有，工作台注册 `gpui_base::RootPlugin` 提示层使提示绘制在对话框遮罩之上，客户端自绘窗口采用套件自带边框，`Popover` 用 `offset` 度量触发间隙，图表模块及其 `chart.grid` token 来自 gpui-base，`DatePickerEvent::Change` 携带 `DateTime`，`Theme::update` 取代手动的全局改再同步（`64751bb`）

### 修复

**浏览器**

- 在 Windows 上打开标签：websocket 传输不再把管道的 NUL 终止符放进文本帧，回环端点文件在启动前删除、且早于本次启动的一律拒绝，因此残留的 `DevToolsActivePort` 不再指向已死的浏览器（`a7a7289`）
- 不再每次启动都打开用户自己的 Chrome：Windows 上 `--version` 不是版本开关，因此该平台不再运行探测，其它平台则给探测加上截止时间，永不答复的浏览器会被杀掉（`a7a7289`）
- 在 Tokio 运行时内轮询浏览器服务：`LocalBrowserTransport` 为每次轮询安装运行时（画面流也不例外），修复了第一次点击浏览器入口就因「there is no reactor running」而崩溃的问题（`9c9a00c`）
- 浏览器子进程的代理配置在启动时决定而不是继承：当 http/https 两个变量都存在时丢弃 `all_proxy`，否则把它的值作为 `--proxy-server` 传给 Chrome，页面不再以 `ERR_EMPTY_RESPONSE` 全部失败（`5d7d9cb`）
- 页面加载失败时保留请求的 URL：内部的 `chrome-error://chromewebdata/` 不再进入标签，错误页仍会渲染、失败仍留在网络诊断中（`1f66568`）
- 接上面板此前丢掉的运行时事件——页面对话框、文件选择器、执行来源发生变化的会话、可用性变化——并让只有明确输入才会接管，交还时调用会自我通告的 `resume_agent_operations`（`2e6e35a`）
- 用户已经停止时撤掉批准卡片：等待现在会检查会话所在标签是否已被中止并立即拒绝，而不是耗完剩下的 120 秒 TTL（`3079c7d`）
- 每次下载只通告一次：Chrome 按会话和按标签各报一次不再产生两条相同通知和两行账本；弹窗的文件夹按钮会打开运行时下载目录，缺失时先创建（`c9f1915`）
- 补全面板的输入与下载反馈：空格和回车以携带文本的 `keyDown` 发送，Ctrl+X 先复制再让页面剪切，F5/Ctrl+R 刷新，Ctrl+L 聚焦地址栏，页面文件选择器打开工作台的浏览器，下载有进度、被拒绝的保存有警告行，地址栏的临时诊断信息被移除（`9a52f05`）
- 让拖动可以选择文本：移动事件说明按住的按键，使 Blink 运行选择手势；释放后的移动仍为 none；每个标签启用焦点模拟，因为无头浏览器没有窗口焦点（`65c8ab2`）
- 保持截图可复现、纹理被释放：两次视觉回归截图都固定到默认视口的 1 倍缩放并等待网络空闲，被替换的 favicon 与切标签时停放的帧通过 `pending_drop` 归还 `Arc<RenderImage>`，`deviceScaleFactor` 跟随窗口而不是硬编码 1.0（`c81194a`）
- 移除让浏览器无法使用的阻断式风险提示：模态闸门、事件、被停放请求和 UI 状态字段全部删除，打开浏览器就是打开浏览器（`93ab323`、`7ee8db3`）
- 删除停靠式检查器和元素拾取器：Chrome DevTools 无法在 `--remote-debugging-pipe` 下附着，除非开放会让任何本地进程驱动浏览器的回环调试端口，因此两个入口及其运行时与传输界面一并移除，工具栏、右键菜单、选择、站点图标和标题宽度限制保留（`228ec67`、`068c4e7`、`ad5a4d8`）
- 远程面板如实呈现：由配对运行时服务时面板标记为降级，设置卡片说明这些显示选项不会配置真正运行页面的那台机器；远程浏览器画面传输明确未启用，附着会得到需要重新同步的答复，帧会得到类型化能力错误而不是沉默（`8b7b7e9`、`6a7d58f`）

**市场与管理**

- 让市场分页器留在屏幕上：面板现在自己决定主体是否滚动，因此网格高度确定、分页器固定而卡片在其下滚动；导入表单比视口更长，仍使用面板自身的滚动（`0c0d534`、`053b793`）
- 把被移除的滚动包装器曾提供的窗口高度还给不滚动的面板，市场网格不再居中漂在空面板中（`e638395`）
- MCP 市场按整个注册表分页，而不是只在已索引的第一个窗口内翻页：请求携带传输筛选，运行时支持 `offset` 并报告 `total_matches`；技能索引忽略 `offset`，因此该市场仍在已抓取的窗口内本地分页（`5f575e8`）

**工作台与设置**

- 设置行按页面宽度排版并让长值换行：值列保持可收缩、标签列有下限，存储摘要的文字放在 `min_w_0().flex_1()` 盒子里获得确定的换行宽度，修复标签被挤成每行一个字符的问题（`68f8be5`）
- 修好更新入口与 About：更新图标使用成功色，About 的更新检查延后一帧执行，不再在更新面板内重复读取工作台，发布信息改为网格布局（`7581dff`）
- 让拖动作用于侧栏绘制为选中的行，包含当前会话行的 Shift/Ctrl 选择会拖动所有高亮行并带上数量徽标（`b699a38`、`7b0c3e9`）
- 查找交给聚焦界面：文件编辑器的 Ctrl+F 打开文件自身的查找，浏览器面板的打开页内查找，只有持有键盘的对话才落到会话搜索（`d793b4f`）
- 编辑器在右键菜单下仍显示选区：编辑器未聚焦时把范围镜像为装饰（`a6c62ec`）
- 关闭失活暂停后，自驱动动画继续运行，后台工作台中有会话在跑也不会被冻结（`beacd98`）

**Agent、ACP 与运行时**

- Codex 会话改在所选 Profile 模型上启动，不再因模型不在 Adapter 内置目录中而让 `apply_session_config` 以 `runtime_switch_configuration_unavailable` 失败（`b5dc05d`）
- 把 Claude Profile 模型接到会话设置层，使 Claude Code 从 `ANTHROPIC_DEFAULT_*_MODEL` 推导出的选择器行之外的模型仍可解析，并在 `ANTHROPIC_MODEL` 指向已分配模型时丢弃它（`9d71fcc`）
- 在本地写入争用中保住运行时切换预留：读取前先取写锁，并在持久切换写入和提交驱动循环周围重试瞬时的 `SQLITE_BUSY_SNAPSHOT`（`8530dd5`）

### 性能

- 削减空闲重绘与逐帧开销：悬停淡入淡出改为通知所属视图，而不再调用 `Window::refresh` 让窗口里所有 `.cached()` 子树失效；侧栏状态指示器降到 10fps 且在失活窗口中静止绘制；回合计时器和终端光标闪烁在失活时跳过重绘；FPS HUD 在缓存边界后渲染；解析后的区域设置只存一次，不再每次调用解析两遍；旧版文件夹迁移记住输入；批量「全选」检查不再为每个会话分配字符串；会话视图缓存预算降到六个视图（`80f4b4d`）
- 流式分块不再各付一次连接、fsync 和轮询：`synchronous = NORMAL` 与 WAL 配对；流式追加路径复用一条预热的写连接；一次迁移删除与时间线主键重复的索引；`wait_for_terminal` 停在进度信号上并改为每秒重读一次而不是 40Hz；ACP 诊断环只记录 stderr；共享 Tokio 运行时按主机规模调整工作线程；每个分块的时间线查找从二次降为常数（`e934ec0`）
- 在后台退让而不是空烧帧：附件更新不再打断 16ms 的时间线批处理；token 用量快照在后台执行器上读取，不再在 UI 线程打开并迁移数据库；所选会话的回退轮询在窗口失活时退避（`80f4b4d`）

### 底层改动

- 版本号统一升到 `0.1.0-rc.7`，覆盖工作区、打包输入、Android 版本回退值、README 与部署文档、原生内容包预期，以及许可证策略中已审核的第一方资源包版本
- 本区间的数据库改动：迁移 57 新增浏览器审计表，迁移 59 在 `browser_origin_grants` 中按 origin 重新记录浏览器授权，迁移 60 新增用于快捷短语排序的 `prompt_usage`，流式追加迁移删除 `idx_agent_timeline_session_sequence`（`8b7b7e9`、`6cde975`、`b1e0d21`、`e934ec0`）
- 依赖迁移到已发布的 `gpui-kit` 0.7.0、`gpui-pre` 0.3.7，以及可对 0.3.7 构建的 `gpui-pre-mobile` 修订版，并按新的依赖图重新生成许可证 SBOM 与声明（`64751bb`）
- spec 更新：内嵌浏览器的归属、不变量、安全规则、面板接线、批准与测试契约（`2382453`、`fbb970a`、`98bd1e3`、`0d568f9`、`735ceb2`、`b0f6fb2`、`197fef5`、`9cd20d0`、`c81194a`、`6cde975`），输入框引用作为原子化 token（`3f47e3c`），设置行按页面宽度度量（`68f8be5`），Codex 的单模型启动选择与 Claude 的会话设置层（`b5dc05d`、`9d71fcc`），OpenCode 的 `vibex-auto` agent（`af8a9fa`、`7594113`），编辑器选区镜像（`a6c62ec`），以及依赖源的审查策略（`64751bb`）
- 新增或扩展的测试覆盖：针对查找、光标、select 菜单、剪贴板、下载、文件选择器、认证挑战、权限提示和画面流的真实 Chrome 套件（`e5430ec`、`1d6fffa`、`aa33e67`、`112359b`、`a8da112`、`90ab6f8`），页面标签与残留 `DevToolsActivePort` 的重启用例（`b525944`、`a7a7289`），复现首次点击崩溃的执行器接缝（`9c9a00c`），两种传输上的 CDP 分帧规则（`a7a7289`），浏览器契约探针及其投递矩阵（`8b7b7e9`、`4dc4a90`、`6a7d58f`），origin 授权及其活动集合（`6cde975`），本地文件批准键（`9cd20d0`），两种页面宽度下的设置行布局（`68f8be5`），拖动集合及其载荷（`b699a38`、`7b0c3e9`），查找快捷键到达聚焦界面（`d793b4f`、`9a52f05`），Prompt 使用排序（`b1e0d21`），失活暂停背后的动效真值表（`beacd98`），FPS HUD 拖动（`4b52031`），以及 About 卡片与更新入口（`7581dff`）
- 浏览器冒烟测试同时在 `ubuntu-latest` 和 `windows-latest` 上运行，因为 Windows 是 CDP 传输走回环端口的平台；只有机器上没有浏览器时，浏览器测试才允许跳过（`a7a7289`）
- 保持质量门通过的构建与格式修复：剪贴板提交漏掉的 `IdleBrowserTransport` 方法（`b68906e`），因当前 gpui 版本的 `EntityId` 没有 const 构造函数而在运行时构造的动效测试替身 owner id（`5044e4b`），以及质量门要求的纯 rustfmt 提交（`b8381bb`、`641a945`、`bf6ad44`）
