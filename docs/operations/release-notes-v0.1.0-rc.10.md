# Vibex v0.1.0-rc.10 Release Notes

- Released: 2026-11-08 · Range: `v0.1.0-rc.9...v0.1.0-rc.10` · 106 commits

---

## English

### 🚀 Highlights

- **Agent sessions in a real terminal** — A session can now be created in terminal mode, where the Agent's own CLI owns the conversation inside a PTY instead of Vibex drawing a timeline. Thirteen Agents are verified for it, and the sidebar, search, notifications, mobile and TUI all keep working on the same row.
- **A terminal that reads like one** — Scrollback search with match navigation, a draggable scrollbar, box-drawing characters painted from their arms, a bundled Symbols Nerd Font, ligatures, URL click-to-open, OSC clipboard and title support, and mouse reports in three dialects.
- **Your MCP servers reach native CLIs** — The reader's configured MCP servers, Vibex delegation, the embedded browser and computer use are handed to each terminal-mode CLI through the launch input it actually reads, with credentials kept out of the argv.
- **Nothing left running behind the app** — Detached Agent daemons are reclaimed by what they run, the computer-use driver gives its memory back after five idle minutes, and abandoned frame pumps retire themselves.
- **gpui-kit 0.9.0** — The workspace moves to the new kit and `gpui-pre` `0.3.11`, and puts two of its affordances to work: an overview ruler in the editor's find bar, and a caret that follows Reduce Motion on both clients.
- **A release page that assembles itself** — The publish job composes one page from the tagged notes, the assets the run actually produced, and GitHub's contributor credit. The macOS app bundle is ad-hoc signed so Gatekeeper stops calling it damaged.

### 🛠 New features

**Terminal sessions**

- `SessionTransport` names how a session runs: an ACP session owns a timeline and a runtime binding, a terminal session starts a CLI and owns neither. Terminal creation is its own entry point rather than a flag on ACP creation, and `TransportKind` is left alone on purpose.
- Thirteen Agent CLIs are verified for terminal mode — Claude, Codex, OpenCode, Gemini, pi, Grok, Cline, Copilot, Cursor, Kimi, CodeBuddy, Devin and Hermes. Each Agent's prompt interface, model/effort/mode flags, restart command and exit-notice session id live in one `TerminalCli` table instead of scattered matches.
- The configured model rides the command line, because a terminal session has no other channel; an Agent default passes nothing rather than a guessed id, and an Agent with no known flag gets none. Terminal mode carries a Provider Profile only, and the switch now says so instead of letting the reader find out from the response.
- The new session's draft is passed to the CLI as its positional prompt and the attachments follow as paths, so the Agent starts on the turn the reader already described.
- What Vibex still owes the reader outside the terminal rectangle is recorded as observation facts — liveness, activity, the Agent's own session id, and the timestamps the unread and notification paths read. `Unknown` is a first-class value, because claiming idle while the CLI waits on an approval prompt is a claim the reader would act on.
- Activity is read from the bottom of the screen rather than the viewport, so scrolling up does not change what the Agent is doing; a `working -> idle` transition needs several agreeing scans because it is what fires completions, and a reading past its freshness window is treated as unknown.
- A session whose process ends is closed rather than left looking healthy, and a restart re-offers the same argv, working directory and Provider Profile — the profile is re-resolved rather than read back, so secrets stay in the keychain.
- A restart rebuilds the PTY from the recorded argv; a session whose command cannot be rebuilt keeps its row and is marked error, so the reader sees which session did not come back.
- The desktop wraps the grid in a rounded embedded window, and the prompt box takes the terminal's own shape when the mode is chosen, so the page says which interface it is about to hand over before it does.
- The phone can reach a terminal session's Agent: the session snapshot names its terminal, selecting the session attaches it through the ordinary path, and the attach survives racing the asynchronous session list.
- The TUI and the phone report a terminal session's Agent activity through the same projection the desktop uses, and the TUI names the desktop instead of drawing an empty transcript for a conversation it cannot show.

**Terminal rendering**

- Search every retained line rather than only the painted viewport, highlight all matches, and step through them with previous/next that scrolls the match into view.
- A draggable scrollbar over the grid, `Shift+PageUp/PageDown` and `Shift+Home/End`, and a wheel that becomes arrow keys in the alternate screen so a full-screen Agent scrolls.
- Box Drawing characters are painted from their arms as joined vector lines — heavy, double, dashed, rounded and diagonal forms included — so tables and panel borders line up across cells.
- `Symbols Nerd Font Mono` ships with the app and private-use symbol glyphs are fitted to the cell outline, which removes the tofu an Agent's status icons used to paint.
- Cell metrics come from the font's own advance rather than a fixed 8x18 constant, rows are shaped as runs instead of one glyph at a time, and wide cells keep the font's advance so CJK still spans two columns.
- Ligatures are a preference, plain HTTP(S) URLs open on click, and a word or a line is selected by double or triple click.
- OSC 52 clipboard stores and reads, OSC 0/2 titles, OSC 4/10/11/12 colour queries, and mouse reports in the 1005, 1015 and 1016 dialects are answered.
- A terminal background opacity setting, and theme ink lifted to a minimum contrast while a program's own true colour is left alone.

**MCP delivery**

- Terminal-mode CLIs receive the reader's MCP servers through the additive launch input each one reads — `--mcp-config` for Claude Code and CodeBuddy, `--additional-mcp-config` for Copilot, `-c` overrides for Codex, `OPENCODE_CONFIG_CONTENT` for OpenCode, a managed scope for Hermes, `CLINE_MCP_SETTINGS_PATH` for Cline, and the process environment for pi.
- When a Provider projection relocates the Agent home, the servers in the reader's real home are restated so they are not lost.
- Credentials stay out of the argv: files are `0600` in a per-session directory that is removed with the session, and Codex receives environment variable names instead of values. Delivery is re-minted on create, restart and crash restore.
- Cursor, Gemini, Grok, Kimi Code and Devin have no per-process MCP input, so both tool panels are shown as unavailable in terminal mode rather than advertised and then silently dropped.

**Desktop**

- Sidebar session filters are a multi-select set rather than one posture, and a second filter widens the column instead of emptying it.
- Sessions inside a project can be sorted by creation, update, title or a manual arrangement; pinned sessions stay in their own band above the sorted rows, and a state file written before the sort existed opens on the default.
- A typed path in the `@` file mention list browses that directory directly, so a directory deep in a large workspace is reachable even when the workspace-wide listing is bounded out first.
- Each Agent's delegation row keeps its own runtime configuration, chosen with the row's edit button, which outranks both the default source and the Agent's last used pair; it is not the last session selection, because a hand-off is a different decision.
- The computer-use panel opens when an Agent starts driving, floating without taking the keyboard, with its own on-by-default setting and the exclusion that a refused call cannot pop a panel open; a panel already on screen is left alone.
- A session with no project now reads as "Temporary session" in the sidebar, its context menu, the new-session picker and search, with a green sparkle resolved rather than stored as its default logo. The predicate lives in `vibex-core` and `vibex-desktop-model`, so the TUI and the phone apply the same rule in their own wording.
- The new-session page offers an Agent whose Adapter is still downloading, with a working pulse in the Agent's own logo footprint and a top-center card that points at Config Center, and the same button creates the session the moment the download lands.
- The transport switch moved out of the prompt box to the far end of the workspace row, because "who owns the interface" is not another "where does this run" control; it is hidden entirely for an Agent with no terminal mode.
- The composer placeholder describes quick phrases, references and history, and the `@` popup draws conversation references on their own tab so a conversation no longer sits under Agent delegation.
- The editor's find bar marks every hit on an overview ruler, and a closed bar clears its marks because they belong to the search rather than the document.
- Runtime failures arrive as top light hints, one Agent failure reads as one hint with copyable details, and a failed turn is no longer repeated in the composer's error strip.

**Mobile**

- Reduce Motion now reaches gpui's own flag, so kit components that ask the context stop animating, and the caret is steady in both the theme and the base layer the inputs paint from.
- An open session keeps its own workspace and conversation: the rail is retargeted from the session row before the snapshot read, and a read that fails no longer leaves the previous conversation's files, changes and terminals under the new page.
- A timeline window that had to cut the live turn is retried without a turn band by `vibex_ui::load_timeline_window`, so all three shells install a contiguous window instead of painting "No messages yet" over a session that was never empty.
- Backspace on the phone's terminal reaches the shell: the IME sink holds one space and is re-seeded from the change it forwarded, so a held or fast Backspace is measured against the document it will actually edit.

**TUI**

- Run options are refreshed after runtime probes answer, so a probe that lands late no longer leaves the page on a stale catalogue.

**Release engineering**

- `scripts/release-notes.mjs` writes a range brief, scaffolds the bilingual document and rejects one that lost a language section or still carries only comments; `scripts/release-body.mjs` composes the page from the tagged notes, a Downloads table rendered from the assets the run publishes, and the contributor credit with the comparison link.
- The publish job runs the validator before uploading, fails when the per-change list appears in the body, and drops a platform whose job produced nothing instead of linking a file that 404s.
- `scripts/sign-macos-app.mjs` seals the macOS bundle inside out — nested Mach-O executables under `<bundle identifier>.<file name>`, then the bundle — before the dmg is assembled, and the packager names both `app,dmg` so the app format runs ahead of the hook.
- The macOS signature stays ad hoc: no Developer ID, no notarization, no hardened runtime, no secure timestamp. That turns "damaged" into the ordinary unidentified-developer prompt the user can allow once.

### ⚡️ Performance

- The terminal grid caches shaped rows by text, style and size, reshapes only damaged rows through the prepaint cache, and paints each row as runs rather than positioning every glyph by hand.
- A resize now fills the pane instead of leaving the painted grid at its old size until the round trip finishes, and the whole paint pass is clipped to the grid's own content mask.
- Config Center writes, Provider probes, route-group saves, gateway reads and terminal start/restart run off the frame thread, with a source guard that fails when an awaited statement names the backend outside the bridge or a Tokio spawn.
- The computer-use driver daemon is stopped after five idle minutes — `stop` first so it removes its own socket, then killed if it does not leave — and only a daemon this client started is stopped.
- A frame pump checks for a live receiver before each capture and retires itself and its channel when nobody watches; a failed capture is retried with a doubling delay capped at ten seconds instead of at the frame rate.
- Short-lived driver subcommands lead their own process group, and a call past its deadline kills the group, so a timeout no longer leaves an orphaned release waiting on the daemon.
- Detached Agent daemons are reaped by what they run: a candidate is adopted by init or the user's `systemd --user` manager and its command line names a file under `<home>/acp-agents/agents/<agent>/`, and it is ended only when no attached process of the same Agent remains anywhere and that has held for two minutes.

### 🔧 Under the hood

- Versions bumped to `0.1.0-rc.10` across the workspace, the desktop packager inputs, the Android versionName fallback, the READMEs, the deployment docs and the reviewed asset bundles.
- The license SBOM and third-party notices were regenerated for the current graph: 1110 Cargo packages, 11 asset and font bundles, and 5 native and installer inputs, all approved.
- The bundled Symbols Nerd Font is registered as a reviewed font input — `vibex-desktop-nerd-symbols`, MIT, two files tied to a tree hash — so the font that removes tofu is covered by the same provenance gate as every other embedded face.
- The database schema moved from 87 to 88: `transport` on the session row and a `session_terminals` link table, with `provider_profile_id` added in place for databases that already ran the migration.
- The workspace moved to `gpui-kit` `0.9.0` and `gpui-pre` `0.3.11`; the kit's theme structs are now `#[non_exhaustive]`, `ScrollWheelEvent` became `ScrollEvent`, and the mobile facade's `insets()` became `safe_insets()` so it cannot resolve to gpui's own inherent method.
- The default theme's diff status colours follow the kit's new green/amber/red family and gain a `deleted` tone; the other eighteen palettes keep their own authored colours.
- Reviewed Agent pins moved to the releases their channels publish: Claude `0.101.0`, Codex `2.5.0` (runtime `0.165.0`), ZCode `0.82.3`, OpenCode `2.2.6`, Antigravity `1.4.5`, Cline `3.0.74`, CodeBuddy `2.181.0`, Copilot `1.0.107`, Cursor `2026.11.05`, DeepSeek Harness `0.4.40`, Gemini `0.67.0`, Grok `1.0.78` and Kimi `2.5.2`, each keeping its previous release as the compatibility floor.
- `_session/steering` accepts `queued` and `startedNewTurn` as answers that mean the Agent owns the text, because treating them as `PromptRequired` would submit the same message twice.
- User-facing text is no longer replaced with `[redacted-sensitive-output]`: vendor refusals, permission titles, model ids, gateway and browser ledger entries, typed form values and Provider endpoints survive, and secret references stay the storage boundary.
- Platform-gated paths now compile on macOS and Windows: a Linux-only `OsStr` import, unused `OsString`/`Path` imports, parameters only one `cfg` branch reads, and `let`-plus-trailing-expression arms that a non-Linux build leaves unreachable.
- Version literals in the SSH and WSL test fixtures stay on `rc.8` on purpose: the upgrade and rollback tests need a real previous release to upgrade from and to keep installed beside the new one.

---

## 中文

### 🚀 核心亮点

- **Agent 会话跑在真正的终端里** — 会话现在可以以终端模式创建：对话由 Agent 自己的 CLI 在 PTY 里负责，而不是由 Vibex 绘制时间线。已为这一模式验证十三个 Agent，侧栏、搜索、通知、移动端和 TUI 都在同一行上继续工作。
- **一个像终端的终端** — 回滚缓冲区搜索与匹配跳转、可拖动滚动条、按笔画绘制的制表字符、随包发布的 Symbols Nerd Font、连字、URL 点击打开、OSC 剪贴板与标题支持，以及三种方言的鼠标上报。
- **MCP 服务器直达原生 CLI** — 读者配置的 MCP 服务器、Vibex 委派、内嵌浏览器和计算机使用能力，通过每个终端模式 CLI 真正读取的启动输入交给它，而凭证不进入 argv。
- **不在应用身后留下任何进程** — 脱离进程树的 Agent 守护进程按它运行的东西收回，计算机使用驱动在空闲五分钟后交还内存，无人观看的帧泵自行退场。
- **gpui-kit 0.9.0** — 工作区升级到新 kit 与 `gpui-pre` `0.3.11`，并把它新增的两项能力用了起来：编辑器查找栏的概览标尺，以及两端都跟随「减弱动态效果」的光标。
- **自己拼装出来的发布页** — 发布任务现在从带标签的发布说明、本次实际产出的产物，以及 GitHub 的新贡献者署名拼出一页。macOS 应用包做了 ad-hoc 签名，Gatekeeper 不再说它「已损坏」。

### 🛠 新功能

**终端会话**

- `SessionTransport` 说明一个会话怎么运行：ACP 会话拥有时间线和运行时绑定，终端会话只启动一个 CLI、两者都不拥有。终端创建是独立入口而不是 ACP 创建上的一个开关，`TransportKind` 有意保持不动。
- 十三个 Agent CLI 已通过终端模式验证——Claude、Codex、OpenCode、Gemini、pi、Grok、Cline、Copilot、Cursor、Kimi、CodeBuddy、Devin 和 Hermes。每个 Agent 的提示方式、模型／强度／模式参数、重启命令和退出提示里的会话 id 都收在一张 `TerminalCli` 表里，而不是散落在各处匹配。
- 配置的模型随命令行传入，因为终端会话没有别的通道；Agent 默认值不传任何东西，而不是猜一个 id，没有已知参数的 Agent 则不传。终端模式只能携带 Provider Profile，开关现在会直接说明这一点，而不是让读者从回复里发现。
- 新建会话的草稿作为位置参数交给 CLI，附件以路径跟随，因此 Agent 从读者已经描述过的那一轮开始。
- Vibex 在终端矩形之外仍然欠读者的东西，被记录为观测事实——存活状态、活动、Agent 自己的会话 id，以及未读和通知路径读取的时间戳。`Unknown` 是一等取值，因为在 CLI 等待审批提示时声称空闲，是读者会据此行动的错误结论。
- 活动状态从屏幕底部读取而不是视口，因此读者向上滚动不会改变 Agent 正在做什么；`working -> idle` 需要多次一致的扫描结果，因为那是触发完成通知的转变，而过期的读数按未知处理。
- 进程结束的会话会被关闭，而不是继续看起来健康；重启会重新提供同一份 argv、工作目录和 Provider Profile——Profile 是重新解析而不是读回的，因此密钥留在钥匙串里。
- 重启从记录的 argv 重建 PTY；命令无法重建的会话保留其行并标记为错误，读者能看出哪个会话没有回来。
- 桌面端把网格包进一个圆角的内嵌窗口，选择模式时输入框会变成终端自身的形状，页面在交出去之前就说明它即将交接哪一种界面。
- 手机能够到达终端会话的 Agent：会话快照写明它的终端，选中会话即通过普通路径附着，并且附着能扛住与会话列表的竞态。
- TUI 和手机通过桌面端同一套投影报告终端会话的 Agent 活动；TUI 则指出对话在桌面端，而不是为一个它无法展示的对话画一张空时间线。

**终端渲染**

- 搜索每一条保留的行而不是只搜已绘制的视口，高亮全部匹配，并用上一处／下一处把匹配滚动到视野内。
- 网格上有可拖动的滚动条，支持 `Shift+PageUp/PageDown` 与 `Shift+Home/End`，在备用屏里把滚轮转成方向键，让全屏 Agent 也能滚动。
- 制表字符按笔画画成相连的矢量线——包括粗线、双线、虚线、圆角和斜线形态——因此表格和面板边框跨单元格对齐。
- `Symbols Nerd Font Mono` 随应用发布，私用区符号字形贴合单元格轮廓，清掉了 Agent 状态图标原本画出的方块。
- 单元格度量取自字体自身的步进，而不是固定的 8x18 常量；行按同样式片段成形而不是逐字摆放，宽单元格保留字体的步进，因此 CJK 仍占两列。
- 连字成为偏好项，纯 HTTP(S) 链接点击即打开，双击或三击选中一个词或一行。
- 回应 OSC 52 剪贴板读写、OSC 0/2 标题、OSC 4/10/11/12 颜色查询，以及 1005、1015、1016 三种方言的鼠标上报。
- 新增终端背景透明度设置；主题墨色会被提亮到最低对比度，而程序自己声明的真彩不受影响。

**MCP 投递**

- 终端模式 CLI 通过各自真正读取的附加启动输入收到读者的 MCP 服务器——Claude Code 和 CodeBuddy 用 `--mcp-config`，Copilot 用 `--additional-mcp-config`，Codex 用 `-c` 覆盖，OpenCode 用 `OPENCODE_CONFIG_CONTENT`，Hermes 用受管作用域，Cline 用 `CLINE_MCP_SETTINGS_PATH`，pi 用进程环境。
- 当 Provider 投影迁移了 Agent home 时，读者真实 home 里的服务器会被重新声明，因此不会丢失。
- 凭证不进入 argv：文件权限 `0600`，放在随会话删除的每会话目录里，Codex 收到的是环境变量名而不是值。投递在创建、重启和崩溃恢复时重新生成。
- Cursor、Gemini、Grok、Kimi Code 和 Devin 没有按进程传入 MCP 的入口，因此两个工具面板在终端模式下直接显示为不可用，而不是先宣称可用、随后悄悄丢掉。

**桌面端**

- 侧栏会话筛选改为可多选的集合而不是单一姿态，第二个筛选会加宽这一列而不是把它清空。
- 项目内的会话可以按创建、更新、标题或手动排列排序；置顶会话仍留在排序行之上的独立带里，而写在排序存在之前的状态文件会以默认值打开。
- 在 `@` 文件提及列表里输入路径会直接浏览该目录，因此即使工作区级清单先被上限截断，大工作区深处的目录依然可达。
- 每个 Agent 的委派行保留自己的运行时配置，用该行的编辑按钮选择，优先级高于默认来源和该 Agent 上次使用的组合；它不是上次会话选择，因为交接是另一个决定。
- Agent 开始驱动桌面时计算机使用面板会打开，浮动显示且不抢键盘，有自己的默认开启设置，并排除了「被拒绝的调用不能弹出面板」；已在屏幕上的面板不会被再次打开。
- 没有项目的会话现在在侧栏、右键菜单、新建会话选择器和搜索里都显示为「临时会话」，默认图标是解析出来的绿色星光而不是存下来的值。判定逻辑放在 `vibex-core` 和 `vibex-desktop-model`，因此 TUI 和手机会用自己的措辞套用同一条规则。
- 新建会话页会提供 Adapter 仍在下载的 Agent，用 Agent 自己 logo 尺寸里的工作进行中脉冲表示，并给出指向配置中心的中上部卡片；下载一完成，同一个按钮就能创建会话。
- 传输方式开关从输入框移到了工作区行末端，因为「谁拥有界面」不是又一个「在哪里运行」的控件；没有终端模式的 Agent 完全不显示这个开关。
- 输入框占位文案说明了快捷语、引用和历史，`@` 弹窗把会话引用放到自己的标签页，会话不再排在 Agent 委派下面。
- 编辑器查找栏在概览标尺上标出每一处命中，关闭查找栏会清掉标记，因为它们属于这次搜索而不是文档。
- 运行时失败以顶部轻提示出现，一次 Agent 失败只读作一条带可复制详情的提示，失败的回合不再在输入框错误条里重复一遍。

**移动端**

- 「减弱动态效果」现在会传进 gpui 自己的开关，询问上下文的 kit 组件也停止动画；光标在主题层和输入框绘制的基层都是稳定的。
- 打开的会话保持自己的 workspace 和对话：右栏在发起快照读取前就按会话行重新定位，读取失败也不会把上一个对话的文件、变更和终端留在新页面下面。
- 必须切掉当前回合的时间线窗口，会由 `vibex_ui::load_timeline_window` 去掉回合带重试一次，因此三个 shell 都装上连续窗口，而不是在一个从来不为空的会话上画「暂无消息」。
- 手机终端的 Backspace 能到达 shell：IME 输入槽保留一个空格，并从它转发的这次变更就地重新播种，因此连按或快速 Backspace 都是针对它真正要编辑的文档来测量。

**TUI**

- 运行时探测返回后会刷新运行选项，因此晚到的探测不会把页面留在过期目录上。

**发布工程**

- `scripts/release-notes.mjs` 生成区间简报、搭出双语文档骨架，并拒绝缺少语言段落或仍只有注释的文档；`scripts/release-body.mjs` 从带标签的说明、按本次实际发布产物渲染的下载表，以及带比较链接的贡献者署名拼出页面。
- 发布任务在上传前运行校验器，正文出现逐条变更列表时直接失败，某个平台没有产出时该行会消失，而不是链接到一个 404。
- `scripts/sign-macos-app.mjs` 在组装 dmg 之前由内向外封装 macOS 应用包——先处理 `<bundle identifier>.<file name>` 下的嵌套 Mach-O，再处理整个包——打包器同时声明 `app,dmg`，让 app 格式排在钩子之前。
- macOS 签名仍是 ad-hoc：没有 Developer ID、没有公证、没有强化运行时、没有安全时间戳。这足以把「已损坏」变成用户可以允许一次的普通「未识别开发者」提示。

### ⚡️ 性能

- 终端网格按文本、样式和字号缓存已成形行，只通过预绘制缓存重整受损行，并按片段整行绘制而不是逐字定位。
- 尺寸变化现在直接填满面板，而不是让已绘制的网格停在旧尺寸直到往返完成；整趟绘制都裁剪到网格自己的内容遮罩内。
- 配置中心的写入、Provider 探测、路由组保存、网关读取以及终端启动／重启都移出帧线程，并配有一个源码守卫：当被 await 的语句在桥接层或 Tokio spawn 之外提到 backend 时就失败。
- 计算机使用驱动守护进程在空闲五分钟后被停止——先 `stop` 让它自己移除套接字，不退出再杀掉——并且只停止本客户端自己启动的那个。
- 帧泵在每次捕获前检查接收端是否还活着，无人观看时自行退场并回收通道；捕获失败按翻倍延迟重试并封顶十秒，而不是按帧率重试。
- 短命的驱动子命令各自领一个进程组，超时即杀掉整个组，因此超时不再留下一个还在等守护进程的孤儿 release 进程。
- 脱离进程树的 Agent 守护进程按它运行的东西收回：候选进程被 init 或用户的 `systemd --user` 收养，且命令行指向 `<home>/acp-agents/agents/<agent>/` 下的文件；只有在同一 Agent 在任何位置都不再留有附着进程、且这一状态持续两分钟后才会被结束。

### 🔧 底层

- 版本号统一升到 `0.1.0-rc.10`：工作区、桌面打包输入、Android versionName 回退值、README、部署文档，以及许可证策略里已审核的资源包版本。
- 许可证 SBOM 与第三方声明按当前依赖图重新生成：1110 个 Cargo 包、11 个资源／字体包、5 个原生与安装器输入，全部已批准。
- 随包的 Symbols Nerd Font 登记为受审字体输入——`vibex-desktop-nerd-symbols`，MIT，两个文件绑定到一个树哈希——因此这个消掉方块的字体与其他内嵌字体走同一道来源审核。
- 数据库 schema 从 87 升到 88：会话行上的 `transport` 和一张 `session_terminals` 关联表，`provider_profile_id` 为已经跑过该迁移的数据库就地补列。
- 工作区升级到 `gpui-kit` `0.9.0` 与 `gpui-pre` `0.3.11`；kit 的主题结构体现在是 `#[non_exhaustive]`，`ScrollWheelEvent` 改名为 `ScrollEvent`，移动端门面的 `insets()` 改为 `safe_insets()`，以免解析到 gpui 自己的同名固有方法。
- 默认主题的差异状态色跟随 kit 新的绿／琥珀／红家族，并补上 `deleted` 色调；其余十八套配色保留各自手写的颜色。
- 已审核的 Agent 版本推进到各通道发布的最新版：Claude `0.101.0`、Codex `2.5.0`（运行时 `0.165.0`）、ZCode `0.82.3`、OpenCode `2.2.6`、Antigravity `1.4.5`、Cline `3.0.74`、CodeBuddy `2.181.0`、Copilot `1.0.107`、Cursor `2026.11.05`、DeepSeek Harness `0.4.40`、Gemini `0.67.0`、Grok `1.0.78`、Kimi `2.5.2`，每个都把上一版保留为兼容下限。
- `_session/steering` 接受 `queued` 与 `startedNewTurn` 作为「Agent 已经接管这段文字」的回答，因为把它们当作 `PromptRequired` 会把同一条消息提交两次。
- 面向用户的文本不再被替换为 `[redacted-sensitive-output]`：厂商拒绝原文、权限标题、模型 id、网关与浏览器账本条目、表单输入值和 Provider 端点都会保留，密钥引用仍是存储边界。
- 平台门控路径现在能在 macOS 和 Windows 上编译：Linux 专用的 `OsStr` 导入、未使用的 `OsString`/`Path` 导入、只有一个 `cfg` 分支读取的参数，以及非 Linux 构建里变成不可达分支的 `let` 加尾表达式。
- SSH 与 WSL 测试夹具里的版本字面量故意留在 `rc.8`：升级与回滚测试需要一个真实的上一版本可以升级，并留在新版本旁边。
