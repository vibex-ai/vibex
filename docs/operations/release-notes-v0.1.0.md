# Vibex v0.1.0 Release Notes

- Released: 2026-11-10 · First stable release · Milestone summary of the ten release candidates from `v0.1.0-rc.1`

---

## English

### 🚀 Highlights

- **Vibex 0.1.0 is the first stable release** — Ten release candidates turned a single-window Agent client into a workbench that spans a desktop app, a phone, and a terminal, all driving one authoritative runtime. The `0.1.0` tag is the first one this project calls supported: it carries no release-candidate suffix, it moves the `latest` container tags, and it publishes to the AUR.
- **Agent sessions can run inside a real terminal** — A session can be created in terminal mode, where the Agent's own CLI owns the conversation inside a PTY instead of Vibex drawing a timeline. Thirteen Agents are verified for it, and the sidebar, search, notifications, mobile and TUI keep working on the same row.
- **A workbench, not just a chat window** — Sessions, source code, Git, terminals, an embedded browser the Agent can drive, a local model gateway, Agent teams, scheduled messages, goals, and a session group that puts several conversations side by side.
- **Your runtime, your machine** — The authoritative `DesktopRuntime` stays on hardware you choose. A paired headless `vibex-server` turns a cloud box or a machine on your LAN into that authority, SSH and WSL remote runtimes put it on another host with an automatic install, tunnel and rollback, and the Relay carries only encrypted frames it cannot read.
- **Three surfaces, one runtime** — The native desktop is the source of truth; the native mobile client and the `vibex` terminal client read and steer the same sessions remotely. All three install from the same release, and the terminal client now opens the channel its package was built as without any environment variable.
- **Ten languages** — The desktop, the phone and the terminal client render in English, Simplified and Traditional Chinese, Japanese, Korean, German, French, Spanish, Brazilian Portuguese and Russian.

### 🛠 New features

**Sessions and the workbench**

- A session can run as an ACP conversation with a timeline, or as a terminal session that starts a CLI and owns no timeline of its own. `SessionTransport` names the difference, and terminal creation is its own entry point rather than a flag.
- Session groups bind a worktree's sessions into one sidebar row and one split workspace, where every pane keeps its own conversation, composer and tabs; a tab dropped on a pane edge splits it.
- Session search became the workbench command palette: one query answers over sessions, settings entries and quick actions, with each action's keybinding shown.
- A session with no project reads as "Temporary session" everywhere, from one predicate shared by the desktop, the TUI and the phone.
- Session filters are a multi-select set, sessions inside a project sort by creation, update, title or a hand-made arrangement, and pinned sessions keep their own band above the sorted rows.
- Goals are part of the session rather than a row in the transcript: a fixed bar above the composer shows the phase, the objective and the provider's own stats, with Edit, Pause, Resume and Clear offered only when the adapter advertises them.
- A delegation is a task with a lifecycle — keep it open, follow up in the same child session, accept or cancel it — and the ownership tree shows how the work moved.
- A queued message can steer the turn that is already running on Agents that negotiate `_session/steering`, and falls back to interrupt-and-resend for everyone else.
- Scheduled messages wait in the queue across restarts, on every client, editable by whichever client holds them.
- The composer walks your earlier messages with Up and Down, edited buffers save themselves after a typing pause, and a typed path in the `@` mention list browses that directory directly.

**Terminal**

- Scrollback search with match navigation, a draggable scrollbar, `Shift+PageUp/PageDown`, and a wheel that becomes arrow keys in the alternate screen.
- Box-drawing characters are painted from their arms as joined vector lines, `Symbols Nerd Font Mono` ships with the app, and cell metrics come from the font's own advance so CJK still spans two columns.
- Ligatures are a preference, plain HTTP(S) URLs open on click, OSC 52 clipboard, OSC 0/2 titles and OSC 4/10/11/12 colour queries are answered, and mouse reports work in the 1005, 1015 and 1016 dialects.
- Terminal-mode CLIs receive your MCP servers, Vibex delegation, the embedded browser and computer use through the launch input each one actually reads, with credentials kept out of the argv.
- The whole workbench fits in a terminal: `vibex` renders sessions, streaming Markdown, approvals, the queue, settings and pairing, either starting its own runtime or attaching to the one on your desk. Every printed `key label` is a button that presses exactly the key it names.

**Desktop**

- Twenty built-in palettes — ten light, ten dark — any light theme paired with any dark one, plus your own theme file in the themes folder; every built-in theme keeps body and secondary text at 4.5:1 or better.
- The window frame follows your system: the title bar floats over the app, macOS keeps its traffic lights, Windows keeps the areas Snap Layouts needs, and Linux follows your desktop's own button layout.
- An embedded browser backed by the Chrome you already have, sitting beside the terminal and the preview. Alt+click an element to open the source file at the line that rendered it, and Alt+click a source line to highlight that element in the page.
- The preview panel can live in a detached window and come back, moving every open tab, buffer and terminal with it; full screen gives the panel the workbench column instead of hiding the sidebar and the rail.
- A Provider workspace that puts providers, route groups and the gateway in one place, with in-place editing, reusable templates and preset model lists from the models.dev catalogue.
- Closing the main window either leaves the Agent running in the system tray or quits immediately, as one choice with two named outcomes. Background running stays the default.
- Updates download themselves, install on quit, and explain what changed, and About shows the running version's own release notes as rendered Markdown.

**Gateway, providers and models**

- A local model gateway on one loopback endpoint that speaks OpenAI Chat Completions, OpenAI Responses, Anthropic Messages and Google Generative AI, translates between them, and forwards each call to whichever Provider serves the requested model.
- A route is one selectable model, composed from whole Providers or `(Provider, model)` pairs, with seven failover strategies.
- Provider Models declare their context and output limits and their thinking depth, and the streaming projection is linear with virtualized thought blocks.
- An Agent connection that fails silently now ends the turn with an error and a suggestion instead of dropping the answer.

**Remote, mobile and Relay**

- A paired headless `vibex-server` answers almost every desktop domain over Remote v2: the Config Center, the management domains, Agent lifecycle and credentials, terminals, the code workbench, session search, worktrees, provider usage and the device audit trail.
- `vibex-server` prints a `vibex://pair#/code/<payload>` link carrying the server URL, the one-time code and the DER certificate next to a `sha256:` fingerprint and a QR code, so a LAN runtime needs no public CA.
- The desktop can put its authoritative runtime on another machine over SSH or into a local WSL2 distro: install `vibex-server`, open a tunnel, pair automatically, and upgrade with a rollback that keeps the runtime's identity.
- The phone follows the desktop's model — session groups and the ownership tree, a right rail with the file tree and diffs, scheduled sends, worktrees, goals, haptics, and a rebuilt pairing flow that starts with the camera and can read a QR code from a screenshot.
- Reduce Motion reaches the kit's own flag on both clients, and an open session keeps its own workspace and conversation even when a snapshot read fails.

### ⚡️ Performance

- The terminal draws its whole grid in one pass instead of rebuilding every cell on every repaint: a frame that took about 242 ms in a debug build takes about 3 ms.
- Session search builds its index once and keeps it warm, so opening it no longer drops a busy session from about 58 fps to 6-8.
- The timeline virtualizes by turn and paints a turn's process section through a windowed run, so a session that ran for hours no longer rebuilds every process row on every frame.
- Config Center writes, Provider probes, route-group saves, gateway reads and terminal start/restart run off the frame thread, behind a source guard that fails when an awaited statement names the backend outside the bridge or a Tokio spawn.
- Detached Agent daemons are reclaimed by what they run, the computer-use driver returns its memory after five idle minutes, and abandoned frame pumps retire themselves — nothing is left running behind the app.
- A four-pane session group no longer holds the workbench in a repaint loop, the Files panel stops probing every installed editor on every frame, and repeating animations are capped at 30 fps.
- Android release builds stop picking up libraries left behind by earlier debug builds: an APK that had grown to 429 MB is back to about 73 MB.

### 🔧 Under the hood

- Versions are `0.1.0` across the workspace, the desktop packager inputs, the Android versionName fallback, the READMEs, the deployment docs and the reviewed asset bundles.
- The license SBOM and third-party notices were regenerated for the current graph: 1110 Cargo packages, 11 asset and font bundles, and 5 native and installer inputs, all approved.
- The bundled WenQuanYi Micro Hei TTC is patched in place so its 11,390 full-width Hangul and ideograph glyphs advance one em instead of 256/2048, which is what had collapsed Korean into a smear. `MODIFICATIONS.txt` records the change, as Apache-2.0 section 4(b) asks.
- The `vibex` terminal client bakes its packaged channel in at compile time with `option_env!("VIBEX_CHANNEL")`, read after `--home`, `VIBEX_HOME`/`VIBEX_DB_PATH`, `--channel` and `VIBEX_CHANNEL`; `vibex status` prints `channel=` and `channel-source=` beside the home.
- Host children share one environment-sanitation policy in `vibex_core::process_environment`, so a packaged AppImage no longer leaks its `APPDIR` and `LD_LIBRARY_PATH` into the Agent adapters, the terminal shell or the vendor installer.
- RC and stable remain separate applications with separate application ids and data directories, so a published-artifact rollback can never race the other channel over the same files. A stable install therefore starts empty; the RC home can be imported once from Settings or the first-launch prompt, which snapshots the RC database with `VACUUM INTO`, migrates and verifies the snapshot, then swaps it in on the next runtime start with a rollback directory behind it. The RC home itself is never modified.
- Version literals in the SSH and WSL test fixtures stay on their earlier release numbers on purpose: the upgrade and rollback tests need a real previous release to upgrade from and to keep installed beside the new one.
- A bare `chore(release)` version bump is kept out of the changelog, and the release page carries no per-change list: a release spans several hundred commits, and these notes already say what changed.

---

## 中文

### 🚀 核心亮点

- **Vibex 0.1.0 是第一个正式版** — 十个候选版本把一个单窗口的 Agent 客户端，变成了横跨桌面、手机和终端的完整工作台，三者驱动同一个权威运行时。`0.1.0` 是这个项目第一次称为「受支持」的标签：它不带候选版本后缀，会把容器镜像的 `latest` 推上去，也会发布到 AUR。
- **Agent 会话可以跑在真正的终端里** — 会话现在能以终端模式创建：对话由 Agent 自己的 CLI 在 PTY 里负责，而不是由 Vibex 绘制时间线。已为这一模式验证十三个 Agent，侧栏、搜索、通知、移动端和 TUI 都在同一行上继续工作。
- **这是工作台，不只是聊天窗口** — 会话、源码、Git、终端、Agent 可以驱动的内嵌浏览器、本地模型网关、Agent 团队、定时消息、目标，以及把多个对话并排放在一起的会话组。
- **运行时在你自己的机器上** — 权威的 `DesktopRuntime` 始终留在你选择的硬件上。配对的 `vibex-server` 可以把云主机或局域网机器变成这个权威；SSH 与 WSL 远程运行时能把它放到另一台主机上，自动安装、建立隧道并在失败时回滚；Relay 只承载它读不到的加密帧。
- **三个界面，一个运行时** — 原生桌面端是事实来源；原生移动端和 `vibex` 终端客户端远程读取并操控同一批会话。三者从同一次发布安装，终端客户端现在无需任何环境变量就能打开其安装包所属的通道。
- **十种语言** — 桌面端、手机端和终端客户端都能以英语、简体中文、繁体中文、日语、韩语、德语、法语、西班牙语、巴西葡萄牙语和俄语显示。

### 🛠 新功能

**会话与工作台**

- 会话可以作为带时间线的 ACP 对话运行，也可以作为终端会话启动一个 CLI、自身不拥有时间线。`SessionTransport` 说明这个区别，终端创建是独立入口而不是一个开关。
- 会话组把一个 worktree 的会话绑成侧栏里的一行和一个分屏工作区，每个窗格保留自己的对话、输入框和标签页；把标签页拖到窗格边缘即分屏。
- 会话搜索变成工作台的命令面板：一次查询同时回答会话、设置项和快捷操作，每个操作都标出自己的快捷键。
- 没有项目的会话在所有地方都读作「临时会话」，这条判定由桌面端、TUI 和手机共用同一处实现。
- 会话筛选是可多选的集合；项目内的会话可按创建、更新、标题或手动排列排序，置顶会话仍留在排序行之上的独立带里。
- 目标成为会话的一部分，而不是时间线里的一行：输入框上方的固定条显示阶段、目标和 Provider 自己的统计，只有在适配器声明支持时才提供编辑、暂停、继续和清除。
- 委派变成有生命周期的任务——保持打开、在同一个子会话里追加跟进、接受或取消——所有权树显示工作如何流转。
- 队列里的消息可以注入正在运行的那一轮（针对协商了 `_session/steering` 的 Agent），其他 Agent 则退回「中断后重发」。
- 定时消息跨重启留在队列里，每个客户端都能看到，持有它的客户端可以随时修改。
- 输入框用上下键走回之前发过的消息；编辑过的缓冲区在停止输入后自动保存；在 `@` 提及列表里输入路径会直接浏览该目录。

**终端**

- 回滚缓冲区搜索与匹配跳转、可拖动滚动条、`Shift+PageUp/PageDown`，以及在备用屏里把滚轮转成方向键。
- 制表字符按笔画画成相连的矢量线，`Symbols Nerd Font Mono` 随应用发布，单元格度量取自字体自身的步进，因此 CJK 仍占两列。
- 连字成为偏好项，纯 HTTP(S) 链接点击即打开；回应 OSC 52 剪贴板、OSC 0/2 标题和 OSC 4/10/11/12 颜色查询；鼠标上报支持 1005、1015、1016 三种方言。
- 终端模式的 CLI 通过各自真正读取的启动输入，收到你的 MCP 服务器、Vibex 委派、内嵌浏览器和计算机使用能力，而凭证不进入 argv。
- 整个工作台都能装进终端：`vibex` 渲染会话、流式 Markdown、审批、队列、设置和配对，既能自己启动运行时，也能附着到桌面端正在运行的那一个。界面里印出的每个 `按键 标签` 都是按钮，按下即等于按那个键。

**桌面端**

- 二十套内置配色——十套浅色、十套深色——任意浅色都能搭配任意深色，还可以把自己的主题文件放进主题目录；每套内置主题的正文与次要文字对比度都在 4.5:1 以上。
- 窗口边框跟随系统：标题栏浮在应用之上，macOS 保留红绿灯按钮，Windows 保留贴靠布局需要的区域，Linux 跟随桌面自己的按钮布局。
- 基于你已有 Chrome 的内嵌浏览器，与终端和预览并排。Alt+点击页面元素可打开渲染它的源文件并定位到那一行，Alt+点击源码行则在页面里高亮对应元素。
- 预览面板可以独立成窗再收回来，打开着的标签页、缓冲区和终端一并跟随；全屏时面板占据工作台列，而不是把侧栏和右栏藏起来。
- Provider 工作区把 Provider、路由组和网关放在一处，支持原地编辑、可复用模板，以及来自 models.dev 目录的预设模型列表。
- 关闭主窗口时，要么让 Agent 继续在系统托盘里运行，要么立即退出——一个选择、两个明确结果。后台运行仍是默认值。
- 更新会自行下载、在退出时安装并说明改了什么；关于页面以渲染后的 Markdown 显示当前版本自己的发布说明。

**网关、Provider 与模型**

- 一个本地模型网关，在单个回环端点上同时说 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 和 Google Generative AI，在它们之间翻译，并把每次调用转给服务所请求模型的 Provider。
- 路由就是一个可选模型，可以用整个 Provider 组合，也可以用 `(Provider, model)` 组合，并支持七种故障转移策略。
- Provider 模型会声明自己的上下文与输出上限以及思考深度，流式投影改为线性并按思考块虚拟化。
- 静默失败的 Agent 连接现在会以一条带建议的错误结束这一轮，而不是让回答凭空消失。

**远程、移动端与 Relay**

- 配对的 `vibex-server` 通过 Remote v2 回答几乎全部桌面领域：配置中心、各管理域、Agent 生命周期与凭证、终端、代码工作台、会话搜索、worktree、Provider 用量和设备审计轨迹。
- `vibex-server` 会打印 `vibex://pair#/code/<payload>` 链接，同时携带服务器地址、一次性代码和 DER 证书，并配一个 `sha256:` 指纹和二维码，因此局域网运行时不再需要公共 CA。
- 桌面端可以通过 SSH 或本地 WSL2 发行版把权威运行时放到另一台机器上：安装 `vibex-server`、建立隧道、自动配对，并在升级失败时回滚且保住运行时身份。
- 手机端跟随桌面端的模型——会话组与所有权树、带文件树和差异的右栏、定时发送、worktree、目标、触感反馈，以及以相机开始、还能从截图里读取二维码的重建配对流程。
- 「减弱动态效果」在两个客户端上都传进 kit 自己的开关；即使快照读取失败，打开的会话也保持自己的 workspace 和对话。

### ⚡️ 性能

- 终端一次绘完整张网格，而不是每次重绘都重建每个单元格：debug 构建里约 242 毫秒的一帧，现在约 3 毫秒。
- 会话搜索只建一次索引并保持温热，打开它不再让一个繁忙会话从约 58 fps 掉到 6-8。
- 时间线按回合虚拟化，一个回合的进程区按窗口绘制，因此跑了数小时的会话不再每帧重建每一行进程。
- 配置中心写入、Provider 探测、路由组保存、网关读取以及终端启动／重启都移出帧线程，并配有源码守卫：被 await 的语句在桥接层或 Tokio spawn 之外提到 backend 时就失败。
- 脱离进程树的 Agent 守护进程按它运行的东西收回，计算机使用驱动在空闲五分钟后交还内存，无人观看的帧泵自行退场——应用身后不留任何进程。
- 四窗格的会话组不再让工作台陷入重绘循环，文件面板不再每帧探测每个已安装编辑器，重复动画封顶 30 fps。
- Android 发布构建不再带上早先 debug 构建残留的库：一度涨到 429 MB 的 APK 回到约 73 MB。

### 🔧 底层

- 版本号统一为 `0.1.0`：工作区、桌面打包输入、Android versionName 回退值、README、部署文档，以及许可证策略里已审核的资源包版本。
- 许可证 SBOM 与第三方声明按当前依赖图重新生成：1110 个 Cargo 包、11 个资源／字体包、5 个原生与安装器输入，全部已批准。
- 随包的 WenQuanYi Micro Hei TTC 就地打了补丁：其中 11,390 个全宽谚文与表意文字字形改为一步进，而不是 256/2048，这正是把韩语挤成一团的成因。按 Apache-2.0 第 4(b) 条要求，`MODIFICATIONS.txt` 记录了这处修改。
- `vibex` 终端客户端在编译期用 `option_env!("VIBEX_CHANNEL")` 把打包通道烧进去，读取顺序排在 `--home`、`VIBEX_HOME`/`VIBEX_DB_PATH`、`--channel` 和 `VIBEX_CHANNEL` 之后；`vibex status` 在 home 旁边打印 `channel=` 和 `channel-source=`。
- 宿主子进程共用 `vibex_core::process_environment` 里的一条环境清洗策略，因此打包的 AppImage 不再把 `APPDIR` 和 `LD_LIBRARY_PATH` 泄漏给 Agent 适配器、终端 shell 或厂商安装器。
- RC 与正式版仍是两个独立应用，各有自己的应用 id 和数据目录，因此已发布产物的回滚不可能与另一个通道争抢同一批文件。正式版安装因此从空开始；RC 的 home 可以从设置或首次启动提示里一次性导入——先用 `VACUUM INTO` 快照 RC 数据库、迁移并校验快照，再在下次运行时启动时换入，并留有回滚目录。RC 的 home 本身永不被修改。
- SSH 与 WSL 测试夹具里的版本字面量故意保留早先的版本号：升级与回滚测试需要一个真实的上一版本可以升级，并留在新版本旁边。
- 纯 `chore(release)` 版本号变更不会进入更新日志；发布页也不带逐条变更列表：一次发布横跨数百个提交，而这些说明已经讲清了改了什么。
