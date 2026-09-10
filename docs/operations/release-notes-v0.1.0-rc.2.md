# Vibex v0.1.0-rc.2 Release Notes

- Released: 2026-09-10 · Range: `v0.1.0-rc.1...v0.1.0-rc.2` · 42 commits

---

## English

### Highlights

- **Remote & cloud runtime, end to end** — a new headless `vibex-server` daemon turns a cloud box into the authoritative DesktopRuntime for both desktop and mobile clients over Remote v2. Pairing codes are operator-displayable one-time secrets stored only as SHA-256 hashes, claimed through a rate-limited endpoint that issues exactly one device grant bound to the client identity key.
- **Desktop client mode** — the workbench can now drive a paired remote runtime (a headless `vibex-server` or another desktop) instead of the local runtime, with a server-pinned, identity-bound credential store, a Remote Runtime settings page, and automatic reconnect at launch.
- **Cloud deployment story** — `deploy/server` ships a multi-stage non-root Dockerfile with healthcheck, docker compose, and Caddy reverse-proxy configuration.
- **Mobile overhaul** — session settings, tools screens, and the terminal rebuilt; persisted dark/light theme and language switching; battery-optimization allowlist guidance; page-stack back navigation.
- **Desktop visuals** — frosted-glass surfaces backed by new backdrop-blur GPUI primitives; upgrade to gpui-kit 0.6.0; per-tool-type expanded timeline details; in-app directory picker.

### New Features

**Remote & server**

- Implement the cloud-based headless service foundation: Remote v2 gateway, pairing, and remote-client backend (`4c9b7bf`)
- Ship the headless cloud runtime end to end: `vibex-server` daemon with `serve` / `status` / `pairing-code` / `revoke` / `config-check` subcommands, graceful SIGINT/SIGTERM shutdown, and `headless_from_environment` covering the full `VIBEX_*` deployment contract (bind, deployment mode, TLS modes, host/origin allowlists, forwarded-header trust, peer rate limits, home lock). Pairing codes become bounded grouped numeric secrets stored only as SHA-256 hashes with a rate-limited `POST /api/v2/pairing/code/claim` endpoint, single-use device grants, audit records, and redacted Debug output. The mobile client gains "Pair with a Cloud Server" (address + numeric code), and remote clients no longer manufacture provider-profile revisions (`fc975bd`)
- Connect the workbench to a paired remote runtime: desktop remote-client credentials (server-pinned, identity-bound, atomic 0600 store), Settings → Remote Runtime page with pairing form, live mode/server state and Forget, automatic reconnect at launch, and the `VIBEX_DISABLE_REMOTE_CLIENT` escape hatch (`7e6b320`)
- Update cloud deployment assets: `deploy/server` Caddyfile, Dockerfile, docker-compose, and README (`d930095`)

**Desktop**

- Frosted-glass surfaces with backdrop-blur GPUI primitives (`Window::paint_backdrop_blur`, edge fade), a `GlassSettings` global, and a frosted-glass switch in appearance settings (`df2cfbd`)
- Upgrade gpui-component 0.5.2 → gpui-kit 0.6.0 while keeping a single GPUI in the tree (`01b8d67`)
- Render per-tool-type expanded timeline details (`af7e271`)
- Add an in-app project directory picker for new sessions (`9931d9d`)
- Order import-picker projects by session count descending (`340676c`)
- Align pane expand/collapse on a shared width clip (`cccfa05`)
- Unify drag-resize seam styling across resizable panes (`68d00c8`)
- Scroll queued message previews within the three-line composer viewport (`af047da`)

**Mobile**

- Overhaul session settings, tools screens, and terminal: desktop-aligned session settings cascade (agent + provider/model in one surface), full-screen file/commit viewer/editor surfaces, terminal rebuilt on the shared `TerminalRenderModel` (VT cell grid, auto-fit PTY resize, touch-pan scrollback, session chips, latching control key bar), persisted dark/light theme and language switching (`11bc3af`)
- Add page-stack back navigation, stricter drawer swipes, and resume recovery (`0cdbf2d`)
- Detect and guide battery-optimization allowlisting (`3811115`)
- Cache loaded sessions and refresh timelines incrementally (`473eb67`)
- Align model icons with the desktop brand catalog (`b548671`)
- Align timeline rendering with desktop (`cb92697`)
- Relax the main-screen drawer swipe gates one notch (`7f29089`)

### Fixes

**Desktop**

- Restore plan bar height with a self-clamped scroll list (`ea2d93b`); make the composer plan bar scroll long step lists (`1097fa0`)
- Stop the overlay entrance animation from overriding overlay positioning (`fd55f17`)
- Resolve relative breadcrumbs and polish the directory picker layout (`2ad61de`)
- Apply pending timeline height corrections across session switches (`005d2ce`)
- Accept idempotent sidebar changes from compact clients (`6dda349`)
- Suppress console windows for spawned helpers on Windows (`d3f4bcb`); suppress the node console window during managed npm agent installs (`66d6ea5`)

**Mobile**

- Render runtime strip agent logos aligned with desktop (`35b612f`)
- Keep the window root focused so back navigation fires (`fe0cfad`)
- Roll back rejected sidebar changes silently (`544bbba`)
- Refresh battery-allowlist state when the request dialog closes (`985b825`)
- Correct token flattening that turned dark-mode borders blue (`aa34c72`)

**Theme & platform**

- Restore the dark card surface tone lost to token contrast collapse (`75d8612`)
- Fix a macOS build issue in directory-picker drive detection (`b107e36`)
- Bump vendored zed for the macOS backdrop-blur compile fix (`532005c`) and the blur scratch warning fix (`ad5f2ad`)

### Internal, Build & Docs

- Remove capture-based evidence gates and the hosted native gate from release tooling (`bc51919`)
- Align quality and release docs with the slimmed gate set (`fa527ac`)
- Refresh the desktop SBOM for the new vendored zed revision (`52914a8`)
- Apply rustfmt and clippy fixes across desktop and mobile (`35a8885`)
- Rebuild the mobile runtime options sheet as four fixed bands (`529fc63`)
- Version bump to 0.1.0-rc.2 (`808a2a4`)

---

## 中文

### 亮点

- **远程与云端运行时全链路落地** — 新增无头 `vibex-server` 守护进程，让一台云服务器成为桌面端与移动端共同依赖的权威 DesktopRuntime（走 Remote v2 协议）。配对码是运营者可展示的一次性密钥，落盘仅保存 SHA-256 哈希，通过限流的领取接口发放唯一绑定客户端身份密钥的设备授权。
- **桌面端客户端模式** — 工作台现在可以连接到配对的远程运行时（无头 `vibex-server` 或另一台桌面端）作为权威运行时，替代本地运行时；配套服务器固定、身份绑定的凭据存储、设置页 "Remote Runtime"，以及启动时自动重连。
- **云端部署方案** — `deploy/server` 提供多阶段构建、非 root 运行、带健康检查的 Dockerfile，docker compose 编排，以及 Caddy 反向代理配置。
- **移动端大改版** — 会话设置、工具页面与终端全面重建；支持持久化的深色/浅色主题与语言切换；电池优化白名单检测引导；页面栈式返回导航。
- **桌面端视觉升级** — 基于全新 backdrop-blur GPUI 原语的毛玻璃表面；升级到 gpui-kit 0.6.0；按工具类型展开的时间线详情；应用内目录选择器。

### 新功能

**远程与服务端**

- 实现云端无头服务的基础部分：Remote v2 网关、配对流程与远程客户端后端（`4c9b7bf`）
- 无头云端运行时端到端交付：`vibex-server` 守护进程提供 `serve` / `status` / `pairing-code` / `revoke` / `config-check` 子命令，优雅响应 SIGINT/SIGTERM，`headless_from_environment` 覆盖完整 `VIBEX_*` 部署契约（绑定地址、部署模式、TLS 模式、host/origin 白名单、转发头信任、对端限流、home 锁定）。配对码改为有界分组数字密钥，仅以 SHA-256 哈希存储，新增限流的 `POST /api/v2/pairing/code/claim` 接口、一次性设备授权、审计记录与脱敏 Debug 输出。移动端新增"配对云服务器"流程（服务器地址 + 数字配对码），远程客户端不再自行编造 provider profile 修订号（`fc975bd`）
- 工作台连接配对的远程运行时：桌面端远程客户端凭据栈（服务器固定、身份绑定、原子写入的 0600 存储），设置页 Remote Runtime 提供配对表单、实时模式/服务器状态与 Forget 操作，启动时自动重连，并提供 `VIBEX_DISABLE_REMOTE_CLIENT` 逃生开关（`7e6b320`）
- 更新云端部署资产：`deploy/server` 的 Caddyfile、Dockerfile、docker-compose 与 README（`d930095`）

**桌面端**

- 基于新的 backdrop-blur GPUI 原语（`Window::paint_backdrop_blur`、边缘淡出）实现毛玻璃表面，新增 `GlassSettings` 全局与外观设置中的毛玻璃开关（`df2cfbd`）
- gpui-component 0.5.2 升级为 gpui-kit 0.6.0，保持全树单一 GPUI（`01b8d67`）
- 时间线按工具类型渲染展开详情（`af7e271`）
- 新建会话支持应用内项目目录选择器（`9931d9d`）
- 导入选择器中的项目按会话数降序排列（`340676c`）
- 面板展开/收起对齐到共享宽度裁剪（`cccfa05`）
- 统一各可调宽面板的拖拽调宽接缝样式（`68d00c8`）
- 排队消息预览在输入框三行视口内可滚动（`af047da`）

**移动端**

- 会话设置、工具页面与终端大改版：与桌面端对齐的会话设置级联（agent 与 provider/model 同一界面），文件/提交全屏查看与编辑界面，终端重建于共享 `TerminalRenderModel`（VT 单元格网格、随表面尺寸自动适配 PTY、触摸平移回滚、会话切换芯片、锁定的控制键条），持久化的深色/浅色主题与语言切换（`11bc3af`）
- 新增页面栈式返回导航、更严格的抽屉滑动门槛与恢复续用（`0cdbf2d`）
- 检测并引导电池优化白名单设置（`3811115`）
- 缓存已加载会话并增量刷新时间线（`473eb67`）
- 模型图标与桌面端品牌图集对齐（`b548671`）
- 时间线渲染与桌面端对齐（`cb92697`）
- 主屏抽屉滑动门槛放宽一档（`7f29089`）

### 修复

**桌面端**

- 计划栏高度恢复：改用自钳制滚动列表（`ea2d93b`）；输入框计划栏可滚动查看较长步骤列表（`1097fa0`）
- 浮层入场动画不再覆盖浮层定位（`fd55f17`）
- 解析相对路径面包屑，打磨目录选择器布局（`2ad61de`）
- 会话切换时应用未落盘的时间线高度修正（`005d2ce`）
- 接受来自紧凑客户端的幂等侧栏变更（`6dda349`）
- Windows 下为 spawned 助手进程隐藏 console 窗口（`d3f4bcb`）；托管 npm agent 安装时隐藏 node console 窗口（`66d6ea5`）

**移动端**

- 运行时条 agent logo 渲染并与桌面端对齐（`35b612f`）
- 保持窗口 root 焦点，返回导航正常触发（`fe0cfad`）
- 被拒绝的侧栏变更静默回滚（`544bbba`）
- 请求弹窗关闭时刷新电池白名单状态（`985b825`）
- 修正 token 展平错误导致的深色模式边框偏蓝（`aa34c72`）

**主题与平台**

- 恢复因 token 对比度塌缩而丢失的深色卡片表面色调（`75d8612`）
- 修复目录选择器磁盘检测的 macOS 构建问题（`b107e36`）
- 升级 vendored zed：修复 macOS backdrop-blur 编译错误（`532005c`）与 blur scratch 警告（`ad5f2ad`）

### 内部、构建与文档

- 发布工具链移除基于截图的证据门禁与托管原生构建门禁（`bc51919`）
- 质量与发布文档对齐精简后的门禁集（`fa527ac`）
- 刷新桌面端 SBOM 至新的 vendored zed 版本（`52914a8`）
- 桌面端与移动端统一执行 rustfmt 与 clippy 修复（`35a8885`）
- 移动端运行时选项底部面板重构为四个固定分区（`529fc63`）
- 版本号升至 0.1.0-rc.2（`808a2a4`）
