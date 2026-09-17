# Vibex v0.1.0-rc.4 Release Notes

- Released: 2026-09-17 · Range: `v0.1.0-rc.3...v0.1.0-rc.4` · 32 commits

---

## English

### Highlights

- **GPUI now comes from crates.io** — the 95 MB `vendor/zed` submodule and its `[patch.crates-io]` block are gone: `gpui` and `gpui_platform` resolve from the published `gpui-pre` 0.3.5 family, `gpui-pre-mobile` is pinned by revision for the Android/iOS platform layer, `crates/gpui-tokio` carries Zed's Apache-2.0 `gpui_tokio` bridge, Linux keeps a bounded font database through a platform delegation wrapper, and frosted glass retires with the fork-only scene primitives it depended on (`6ffcf7c`, `825bb97`, `9ecc107`).
- **One runtime manager on both clients** — desktop and mobile gain a single place to browse, add, switch, rename and remove runtimes. `remote-runtimes.json` (v2) keeps every grant beside `activeRuntimeId`, mobile hosts move to `vibex-native-mobile-hosts.v2`, switching parks the embedded runtime instead of shutting it down so local terminals and Agent turns survive the round trip, and a failed switch leaves the workbench exactly where it was (`3e72051`).
- **A runtime says which machine it is** — `RemoteServerKind` (desktop / headless / unknown) travels with the runtime's self-description from `/api/v2/info` and the v2 handshake, so an operator can tell a paired Vibex desktop from a standalone `vibex-server` before driving it. The field is additive on the wire, and a peer that predates it keeps the generic label instead of being guessed at (`a33e67c`).
- **Ten light and dark palettes, chosen independently** — the appearance setting stops being a single light/dark switch: the token source is keyed by theme id and ships Vibex, Catppuccin, Gruvbox, Solarized, GitHub, Tokyo Night, and Nord variants, any light palette pairs with any dark one, and a themes directory under the app home extends the catalog. Every curated theme clears 4.5:1 for body and muted text, and a user theme re-derives each unpinned foreground against the surface it is painted on (`04d582d`).
- **The window chrome follows the platform** — the title bar becomes a 38px overlay that carries no fill or bottom border of its own, so the sidebar tone, workbench surface, and panel seams run to the window edge behind it, and the right rail paints one shared surface token. Which caption buttons exist is now one decision, `resolve_window_controls`: macOS keeps its AppKit traffic lights, Windows keeps the system hit areas that Snap Layouts needs, and Linux draws buttons only for a client-decorated window and then follows GTK's `gtk-decoration-layout` (`5a2f015`, `a72bf55`, `4035d0e`, `dd87a6f`, `18c8628`).
- **A silent ACP turn now fails instead of answering nothing** — an Adapter that swallows an internal provider failure and still returns `end_turn` with no `session/update` at all (Kimi Code 1.50.0 on a rejected API key) used to persist an empty final message that rendered as a vanished answer with no error and a completed submission. Completion now reports `acp_turn_without_output` with a recovery hint, while a pending permission or elicitation, a user-requested stop, and any recognized `session/update` still keep a legitimate turn intact (`dcd01d8`).
- **Session search stops rebuilding itself** — opening search rebuilt the index for every session and re-ran the whole scan on every frame the dialog stayed open, which showed up as a drop from ~58 fps to 6-8 on a live session. `AgentManager` now remembers the paths this process verified, a paged timeline read keeps one connection instead of opening one per page, projection moves to the blocking pool, and the dialog keeps its index across a close so a reopen is a no-op (`95d0a8a`).

### New Features

**Runtime & remote**

- Give both clients one runtime manager to browse, add, switch, rename and remove runtimes, with the list and detail stage sharing one anchored panel so management never stacks overlays (`3e72051`)
- Keep the embedded runtime alive across a switch (L1 keep-alive), connect before swapping so a wrong address changes nothing, and record the failure reason per runtime (`3e72051`)
- Tell a desktop-hosted runtime from a headless `vibex-server` through `RemoteServerKind`, carried on the credential and the phone's per-runtime metadata so it is known at pairing time and refreshed on every connect (`a33e67c`)

**Theme & design system**

- Ship a curated catalog of ten light and dark palettes with independent selection per appearance, and let users extend it with theme files that name only the roles they want to change (`04d582d`)
- Derive hover and selection washes from the active theme's own background, so a warm or tinted palette no longer receives neutral grey chrome (`04d582d`)
- Paint every right-rail panel — files, Git, and the child Agent timeline — with one `right-rail-surface` token so the rail reads as a single pane (`18c8628`)

**Desktop**

- Float the window chrome above the shell as a 38px overlay, and reserve `TITLE_BAR_HEIGHT` in every column that must not underlap it (`5a2f015`)
- Resolve caption buttons per platform, honour GTK's `gtk-decoration-layout`, drop buttons the window cannot perform, and render each cluster outside the drag region so a caption press never starts a window move (`a72bf55`)
- Slide the usage range switch as a travelling thumb and soften control borders (`37e8029`)
- Group the usage cross-filters behind the range control and name each trigger's applied value, with a `+N` suffix for further selections (`2a03753`)
- Rework the model provider editor: Enter saves instead of closing the dialog, every control writes into the draft, and errors report under the field that produced them (`69489d7`)
- Host the MCP and Skills native-export card on their own pages, so a resource family can be written into an Agent's configuration without opening the unreachable Advanced page (`f7c3129`)
- Add a Developer settings section whose switch overlays the gpui-fps HUD (`a80f3fd`), let the HUD be dragged and persist its placement as `developer.fpsMonitorPlacement` (`b2aa073`), and record its samples as diagnostics data in `diagnostics/fps-monitor.jsonl` (`8a30d6a`)

**Agent & ACP**

- Upgrade the zcode adapter from 0.17.2 to 0.37.1 (`d88327f`)
- Declare zcode's usage contract as per-turn, show the adapter's readable `toolCall.content` and affected `toolCall.locations` in the approval dialog, and pin the interpreter with `ZCODE_ACP_RUNTIME=node` (`d88327f`)

### Fixes

**Desktop**

- Bundle every icon the workbench asks for: thirty-four registered paths were missing and two pointed at the wrong directory, so those surfaces drew a gap where an icon should be (`2b37048`)
- Show the runtime button's icon and keep its panel open, instead of letting the popover and the trigger's `on_click` toggle against each other (`de067b9`)
- Answer the sidebar-organization bridge for the embedded authority specifically, so a paired client never receives the displayed remote runtime's tree (`444226b`)
- Release a settled streaming shrink of the timeline extent, so a collapsed card or merged process row no longer leaves a blank band the viewport parks on (`3e59958`)
- Honour the macOS Dock icon grid with `icon-macos-*.png` renditions and route a Dock reopen request through `SystemTray::restore` (`a5af0ee`)
- Order the provider connection fields before its models, matching the order they are filled in when a Provider is created (`232e8b6`)
- Give the usage toolbar and chart-header controls the standard `small` frame instead of the cramped xsmall one (`c98f0f4`)

**Agent & ACP**

- Fail a completely silent ACP turn instead of persisting an empty answer, with `needs_input`, user-cancelled, and activity-only turns exempted (`dcd01d8`)
- Lock Cline's base URL to `api.openai.com` through an Agent-owned `FixedEndpoint` control, and preview the pinned origin in the projection plan (`4285157`)

**Mobile & build**

- Pass the NDK API level when building Android: `cargo ndk` defaults to platform 21 while `gpui-pre-mobile` links `libnativewindow.so`, which the NDK only ships from API 26, so the link step failed for `build:mobile:android`, `package:mobile:android`, and the tagged release workflow (`50a03f9`)

### Performance

- Keep the session search index warm across dialog closes, scan it in a debounced background task instead of in `render`, and match ASCII and case-less text in place (`95d0a8a`)

### Internal, Build & Docs

- Replace the `vendor/zed` fork with published `gpui-pre` 0.3.5: remove the submodule, `.gitmodules`, and the submodule-aware license plumbing, add `crates/gpui-tokio` for the unpublished Apache-2.0 `gpui_tokio` bridge, move the IBM Plex Sans and Lilex faces and the mobile undo icon out of the submodule, and rebuild the mobile platform facade, IME host, and iOS entry point on `gpui-pre-mobile` (`6ffcf7c`)
- Track zed `d89e9c2` and pin the gpui-kit family to `gpui-kit` main `fb26e617` with a fourth `[patch.crates-io]` entry (`825bb97`), then bump the pin for `ImageSource::evict` (`9ecc107`)
- Record the git-pinned gpui-kit dependency source in spec, including the upstream-merge requirement before the revision moves past the gpui-pre version the fork reports (`e1d56bc`)
- Update `check-mobile-native.mjs` to assert the new dependency sources, platform facade, and IME host instead of fork internals; update `check-licenses.mjs` and `source-identities.mjs` for the new dependency graph, `generate-tokens.mjs` for the theme catalog, and `check-release.mjs` and `package-desktop-release.mjs` for the macOS icon grid (`6ffcf7c`, `04d582d`, `a5af0ee`)
- Regenerate the SBOM, third-party notices, and license policy, and record the relocated font and icon provenance (`6ffcf7c`, `a5af0ee`)
- Refresh the release packaging matrix, platform support matrix, UI-boundary architecture note, and the license gate README (`6ffcf7c`, `a5af0ee`)
- Update the frontend, usage-statistics, agent-session-protocol, and architecture-baseline specs for the chrome, toolbar, empty-turn, and dependency-source contracts (`2a03753`, `dcd01d8`, `e1d56bc`)
- Fix the workspace clippy gate: `CaptureScrollWheel`'s listener field is now a named `CaptureScrollWheelListener` alias, so `clippy::type_complexity` no longer fails `pnpm check:rust` and the CI job that runs `pnpm check` (`45709c8`)

---

## 中文

### 亮点

- **GPUI 改为从 crates.io 获取** — 95 MB 的 `vendor/zed` 子模块及其 `[patch.crates-io]` 块已移除：`gpui` 与 `gpui_platform` 解析自已发布的 `gpui-pre` 0.3.5 家族，Android/iOS 平台层所用的 `gpui-pre-mobile` 按 revision 固定；Zed 那 100 行 Apache-2.0 的 `gpui_tokio` 桥接没有已发布包，改由新增的 `crates/gpui-tokio` 承载；Linux 通过平台委托包装层保留有界字体库；毛玻璃效果随其依赖的 fork 专属场景原语一并下线（`6ffcf7c`、`825bb97`、`9ecc107`）。
- **两个客户端共用一套运行时管理器** — 桌面端与移动端各自有了浏览、添加、切换、重命名与删除运行时的唯一入口。`remote-runtimes.json`（v2）把每份授权与 `activeRuntimeId` 存在一起，移动端主机存储迁移到 `vibex-native-mobile-hosts.v2`；切换时改为挂起内嵌运行时而非销毁，本地终端与 Agent 回合得以跨往返存活；切换失败则工作台原地不动（`3e72051`）。
- **运行时会说明自己是哪台机器** — `RemoteServerKind`（desktop / headless / unknown）随运行时自描述经 `/api/v2/info` 与 v2 握手传递，运维在驱动之前即可分辨配对的是 Vibex 桌面端还是独立的 `vibex-server`。该字段在线路上是纯增量，早于该字段的对方仍显示通用标签而不会被猜测（`a33e67c`）。
- **十套明暗配色，明暗独立选择** — 外观设置不再是单一明暗开关：token 源改按主题 id 索引，内置 Vibex、Catppuccin、Gruvbox、Solarized、GitHub、Tokyo Night 与 Nord 变体，任意浅色可与任意深色搭配；应用 home 下的 themes 目录可扩展目录。所有精选主题的正文与次要文字对比度均达到 4.5:1，用户主题会针对实际绘制的表面重新推导每个未固定的前景色（`04d582d`）。
- **窗口外框跟随平台** — 标题栏改为 38px 浮层，自身不再绘制填充与下边线，侧栏色调、工作台表面与面板接缝因此一直延伸到窗口边缘，右栏统一使用同一个表面 token。标题按钮的有无现在由唯一决策 `resolve_window_controls` 决定：macOS 保留 AppKit 交通灯，Windows 保留 Snap Layouts 所需的系统命中区，Linux 仅在客户端装饰窗口上绘制按钮并遵循 GTK 的 `gtk-decoration-layout`（`5a2f015`、`a72bf55`、`4035d0e`、`dd87a6f`、`18c8628`）。
- **静默的 ACP 回合现在会失败，而不是答非所问** — 适配器吞掉内部 provider 失败却仍以 `end_turn` 返回、且完全没有任何 `session/update` 时（Kimi Code 1.50.0 在 API key 被拒时即如此），过去会持久化一条空的最终消息，表现为回答凭空消失、没有报错、提交却记为完成。现在完成阶段会报 `acp_turn_without_output` 并给出恢复提示；而待处理的权限或 elicitation、用户主动停止、以及任何可识别的 `session/update` 仍会让正常回合保持完好（`dcd01d8`）。
- **会话搜索不再反复重建** — 过去每次打开搜索都会为所有会话重建索引，并在对话框打开的每一帧重跑整轮扫描，在活跃会话上表现为从约 58 fps 掉到 6-8。现在 `AgentManager` 记住本进程已验证的路径，分页时间线读取复用同一个连接而非每页新开，投影移至阻塞线程池，对话框关闭时保留索引，重开即为空操作（`95d0a8a`）。

### 新功能

**运行时与远程**

- 为两个客户端提供统一的运行时管理器，用于浏览、添加、切换、重命名与删除运行时；列表与详情阶段共用同一个锚定面板，管理操作不再堆叠浮层（`3e72051`）
- 切换时保留内嵌运行时（L1 keep-alive）；先连接成功再切换，地址错误则工作台保持不变；每个运行时单独记录失败原因（`3e72051`）
- 通过 `RemoteServerKind` 区分桌面端承载的运行时与无头 `vibex-server`，该信息随凭据与手机端各运行时元数据保存，配对时即已知并在每次连接时刷新（`a33e67c`）

**主题与设计系统**

- 内置十套明暗配色目录，明暗两种外观各自独立选择；用户可用只声明想改角色的主题文件扩展目录（`04d582d`）
- 悬停与选中底色改为从当前主题自身背景推导，暖色或带色调的配色不再收到中性灰控件底色（`04d582d`）
- 右栏所有面板（文件、Git、子 Agent 时间线）统一绘制 `right-rail-surface` token，整栏读作同一块面板（`18c8628`）

**桌面端**

- 窗口外框以 38px 浮层浮在工作台之上，所有不应被其压住的列都预留 `TITLE_BAR_HEIGHT`（`5a2f015`）
- 标题按钮按平台解析，遵循 GTK 的 `gtk-decoration-layout`，剔除窗口无法执行的按钮；左右按钮组绘制在拖拽区之外，按标题按钮不会触发窗口移动（`a72bf55`）
- 用量范围开关改为滑动滑块，并柔化控件描边（`37e8029`）
- 用量交叉筛选器归为一组排在范围控件之后，每个触发器直接显示已应用的值，多选时追加 `+N`（`2a03753`）
- 重做模型 Provider 编辑器：回车改为保存而非关闭对话框，所有控件直接写入草稿，错误显示在产生它的字段下方（`69489d7`）
- MCP 与 Skills 的原生导出卡片移到各自页面，无需进入从主导航无法到达的 Advanced 页即可把资源族写入 Agent 配置（`f7c3129`）
- 新增开发者设置分区，开关即可叠加 gpui-fps HUD（`a80f3fd`）；HUD 可拖动并把位置持久化为 `developer.fpsMonitorPlacement`（`b2aa073`）；其采样作为诊断数据记录到 `diagnostics/fps-monitor.jsonl`（`8a30d6a`）

**Agent 与 ACP**

- zcode 适配器由 0.17.2 升级到 0.37.1（`d88327f`）
- 将 zcode 的用量契约声明为按回合；审批对话框优先显示适配器可读的 `toolCall.content` 并列出受影响的 `toolCall.locations`；以 `ZCODE_ACP_RUNTIME=node` 固定解释器（`d88327f`）

### 修复

**桌面端**

- 补齐工作台请求的全部图标：34 个路径未注册、2 个指向了错误目录，导致相应位置只画出空白（`2b37048`）
- 显示运行时按钮的图标并保持其面板常开，不再让 popover 与触发器的 `on_click` 互相切换（`de067b9`）
- 侧栏组织桥接改为专门应答内嵌权威端，配对客户端不会再收到当前显示的远程运行时的树（`444226b`）
- 流式行已稳定的收缩会释放时间线高度，折叠卡片或合并的流程行不再留下视口停驻的空白带（`3e59958`）
- 以 `icon-macos-*.png` 适配 macOS Dock 图标网格，并让 Dock 重新打开请求走 `SystemTray::restore`（`a5af0ee`）
- Provider 编辑器中连接字段排在模型之前，与创建 Provider 时的填写顺序一致（`232e8b6`）
- 用量工具栏与图表标题控件改用标准 `small` 尺寸，替换局促的 xsmall（`c98f0f4`）

**Agent 与 ACP**

- 完全静默的 ACP 回合改为失败，不再持久化空回答；待输入、用户取消与仅有活动的回合除外（`dcd01d8`）
- 通过 Agent 自有的 `FixedEndpoint` 控件把 Cline 的 base URL 锁定为 `api.openai.com`，并在投影计划中预览该固定地址（`4285157`）

**移动端与构建**

- Android 构建传入 NDK API 级别：`cargo ndk` 默认 platform 21，而 `gpui-pre-mobile` 链接 `libnativewindow.so`（NDK 自 API 26 起才提供），导致 `build:mobile:android`、`package:mobile:android` 与打标签的发布工作流均在链接阶段失败（`50a03f9`）

### 性能

- 会话搜索索引在对话框关闭后保持温热，改由去抖的后台任务扫描而非在 `render` 中执行，并对 ASCII 与无大小写文本就地匹配（`95d0a8a`）

### 内部、构建与文档

- 以已发布的 `gpui-pre` 0.3.5 取代 `vendor/zed` fork：移除子模块、`.gitmodules` 与依赖子模块的许可证管线；新增 `crates/gpui-tokio` 承载未发布的 Apache-2.0 `gpui_tokio` 桥接；IBM Plex Sans、Lilex 字体与移动端撤销图标迁出子模块；移动端平台门面、输入法宿主与 iOS 入口重建于 `gpui-pre-mobile` 之上（`6ffcf7c`）
- 跟踪 zed `d89e9c2`，并以第四个 `[patch.crates-io]` 条目把 gpui-kit 家族固定到 `gpui-kit` main `fb26e617`（`825bb97`），随后为 `ImageSource::evict` 更新固定点（`9ecc107`）
- 在 spec 中记录 git 固定的 gpui-kit 依赖来源，并写明在该修订越过 fork 所报告的 gpui-pre 版本之前必须先完成上游合并（`e1d56bc`）
- `check-mobile-native.mjs` 改为断言新的依赖来源、平台门面与输入法宿主，不再检查 fork 内部；`check-licenses.mjs` 与 `source-identities.mjs` 适配新的依赖图，`generate-tokens.mjs` 适配主题目录，`check-release.mjs` 与 `package-desktop-release.mjs` 适配 macOS 图标网格（`6ffcf7c`、`04d582d`、`a5af0ee`）
- 重新生成 SBOM、第三方声明与许可证策略，并记录迁移后的字体与图标来源（`6ffcf7c`、`a5af0ee`）
- 更新发布打包矩阵、平台支持矩阵、UI 边界架构说明与许可证门禁 README（`6ffcf7c`、`a5af0ee`）
- 更新前端、用量统计、Agent 会话协议与架构基线 spec，以记录外框、工具栏、空回合与依赖来源契约（`2a03753`、`dcd01d8`、`e1d56bc`）
- 修复工作区 clippy 门禁：`CaptureScrollWheel` 的监听器字段改为具名 `CaptureScrollWheelListener` 别名，`clippy::type_complexity` 不再让 `pnpm check:rust` 及运行 `pnpm check` 的 CI 任务失败（`45709c8`）
