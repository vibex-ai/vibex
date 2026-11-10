# Vibex v0.1.0 Release Notes

- **Release Date:** 2026-11-10
- **Summary:** First stable release (`v0.1.0`), marking the milestone transition from RCs into a fully supported multi-surface Agent workbench across desktop, mobile, and terminal.

---

## English

### 🚀 Highlights
- **First Stable Release:** The 0.1.0 tag introduces official support, `latest` container tags, and AUR publishing.
- **Real Terminal Sessions:** Run Agents directly inside a PTY terminal via 13 verified CLIs while retaining sidebar, search, mobile, and TUI integration.
- **A True Workbench:** Combines sessions, source code, Git, terminals, an embedded browser, local model gateways, Agent teams, goals, and multi-pane session groups.
- **Your Runtime, Your Machine:** Host the authoritative `DesktopRuntime` locally, on a remote LAN server (`vibex-server`), or via automatic SSH/WSL remote runtimes.
- **Three Surfaces, One Runtime:** Desktop, mobile, and terminal (`vibex`) clients control the exact same authoritative runtime and install from the same release.
- **10 Languages Supported:** English, Simplified/Traditional Chinese, Japanese, Korean, German, French, Spanish, Brazilian Portuguese, and Russian.

### 🛠 Key Features
- **Sessions & Workbench:** Terminal transport distinction, split-pane session groups, command palette search, session filters, inline goals, and lifecycle task delegation.
- **Terminal:** Scrollback search, vector box-drawing, Nerd Font integration, ligatures, OSC clipboard/title/color queries, and mouse reporting.
- **Desktop:** 20 built-in themes, system-following window frames, Chrome-backed interactive embedded browser, Provider workspace, and smart tray/exit options.
- **Gateway & Models:** Unified local model gateway (OpenAI, Anthropic, Google), customizable failover routing, and detailed model capabilities.
- **Remote & Mobile:** QR pairing (`vibex-server`), remote SSH/WSL setup, and full-featured mobile app parity.

### ⚡️ Performance & Under the Hood
- **Performance:** Terminal grid rendering accelerated (242ms → 3ms), indexed instant session search, virtualized timelines, and background resource cleanup (Android APK reduced from 429MB to 73MB).
- **Core Reliability:** Isolated stable vs. RC application directories with safe database migration/import, environment sanitization, and updated SBOM/licenses.

### ⬆️ Upgrading from a Release Candidate

Stable and RC are separate applications with their own data directories, so a
stable install starts empty. To carry your RC work forward, open
**Settings → Data & Diagnostics → Local storage → Import RC data** and confirm.
Vibex snapshots the RC database, migrates and verifies the copy, then swaps it in
on the next start; your workspaces, sessions, and Provider configuration come
with it, and the current data is kept if the import fails. The RC install is
never modified, so you can keep using it as a fallback.

---

## 中文

### 🚀 核心亮点
- **首个正式版发布：** 标志着项目正式步入稳定支持阶段，发布了 AUR 包与 `latest` 容器镜像。
- **真实终端会话：** 支持在 PTY 终端中直接运行 Agent CLI（已验证 13 款），同时侧栏、通知与多端保持同步。
- **全功能开发者工作台：** 集成会话、源码、Git、终端、可驱动的内嵌浏览器、模型网关、Agent 团队与分屏会话组。
- **自主掌控运行时：** 核心运行时完全由您掌控，支持本地运行、局域网 `vibex-server` 托管，或通过 SSH/WSL 自动部署远程运行时。
- **三端合一：** 桌面端、移动端与终端客户端（`vibex`）无缝驱动同一运行时。
- **多语言：** 全面支持中（简/繁）、英、日、韩、德、法、西、葡、俄 10 种语言。

### 🛠 核心功能
- **会话与工作台：** 支持 ACP 时间线与纯终端模式、多标签分屏、命令面板式全局搜索、目标管理及任务委派。
- **终端：** 回滚区搜索、矢量制表符、自带 Nerd Font、连字支持及完整的 OSC 查询与鼠标上报。
- **桌面端：** 20 套精美内置主题、系统自适应窗口边框、基于 Chrome 的双向交互内嵌浏览器、统一的 Provider 模型工作区。
- **网关与模型：** 本地模型网关（支持 OpenAI、Anthropic、Google 互转）、灵活的故障转移路由策略。
- **远程与移动端：** 基于二维码的极速配对、SSH/WSL 远程开发支持，以及功能对齐的移动客户端。

### ⚡️ 性能与底层改进
- **性能优化：** 终端渲染帧率大幅优化（从 242ms 降至 3ms）、秒级会话搜索、时间线虚拟化，Android APK 体积从 429MB 锐减至 73MB。
- **底层加固：** 正式版与 RC 版数据目录完全隔离（支持安全的数据库迁移导入）、环境白名单清理机制、全面更新的依赖许可证。

### ⬆️ 从候选版本升级

正式版与 RC 版是两个独立应用，各自拥有独立的数据目录，因此正式版安装后从空数据开始。要继承 RC 版本的工作，请打开 **设置 → 数据与诊断 → 本地存储 → 从 RC 版本导入数据** 并确认。Vibex 会先对 RC 数据库做快照、迁移并校验副本，在下次启动时换入；工作区、会话与 Provider 配置会一并带过来，导入失败时保留当前数据。RC 安装本身不会被修改，您可以继续把它作为退路使用。
