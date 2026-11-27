# Vibex v0.1.10 Release Notes

## English

### 📌 Release Overview

**Vibex 0.1.10**: This release completely rebuilds the mobile client, with fully native UI and interaction. It substantially improves and updates Antigravity and the other major Agent runtimes. It also adds cross-instance session cloning and migration, removes the two-hour long-task timeout, and brings official Nix flake support.

---

### 🚀 What's new

- **Mobile fully rebuilt with a better experience:** the phone app no longer assembles a phone out of desktop widgets — iOS mounts UIKit, Android mounts native views, driven by a shared Rust engine. It delivers the full set of native gestures for text, caret and scrolling, and brings the desktop's core features over intact — drawer group dragging, streaming Markdown rendering, the `/` and `@` palettes, attachments and permission cards. It also substantially improves the phone's right rail (file tree folding, read-only viewing, Git history and diff comparison) and the terminal's cross-tab reuse.
- **Cross-instance session cloning and migration:** a project, a group, or a single or batch selection of sessions can now be exported from one Vibex instance and imported into another, with missing Providers or models synced automatically and the prompt cache preserved.
- **Antigravity becomes a first-class Agent:** a dedicated dialect profile and event enricher normalize its tool calls, `extra_high` thinking depth is supported, and direct Provider Profiles switch to `gemini-api-key` auth.
- **Agent runtimes move to the latest versions:** every major Agent runtime is updated to its latest upstream version (Claude 0.112.0, Codex 2.12.0, and so on), each keeping its previous release as the compatibility floor.
- **The two-hour hard timeout is removed:** long tasks are no longer forcibly interrupted at the two-hour mark; if there is no output for ten consecutive minutes, a friendly notice appears above the input (stop and continue, or dismiss).
- **A development desktop can install a locally built server:** a debug or preview build can install a locally built server tarball through `VIBEX_DEV_SERVER_ARTIFACT`.
- **Official Nix flake support:** `flake.nix` packages the published Linux artifacts directly and handles dependencies through `autoPatchelfHook`, so NixOS runs smoothly without `LD_LIBRARY_PATH`.
- **Desktop detail fixes:** the composer expands instantly when a draft wraps, the independent scrollbar track colour is more accurate, switching sessions no longer occasionally drops already-loaded timeline history, and the right rail's traversal optimization eliminates the stalls caused by large files.

---

### ⬆️ Upgrading

**How to upgrade:** install **0.1.10** over the previous version. The desktop, Android and iOS all upgrade in place. Your sessions, workspaces, Provider configuration, Skills, remote hosts and drafts are all kept.

**Notes:**

1. **Mobile rebuilt:** the new mobile client is officially back. Users of the old build can upgrade in place, and the existing pairing and settings are imported smoothly.
2. **Agent runtimes:** update each Agent runtime to its latest version for the best tool-event and interaction experience (older versions remain protected by the compatibility floor).
3. **Long-task notice:** tasks no longer have a two-hour automatic cutoff; if a task produces no output for ten consecutive minutes there is a friendly notice, and the system does not cancel automatically.
4. **NixOS users:** the official flake is already updated to 0.1.10 and runs safely without any additional environment variables.

---

## 中文

### 📌 版本速览

**Vibex 0.1.10**：该版本完全重做了移动端，UI 与交互全面原生化。特别对 Antigravity 与其他主流 Agent 运行时进行了大幅优化与最新版本适配。同时支持跨实例会话克隆迁移、解除长任务两小时超时，并带来了官方 Nix flake 支持。

---

### 🚀 更新内容

- **移动端全面重做与体验升级：** 手机端彻底告别桌面控件拼凑，iOS 挂载 UIKit、Android 挂载原生视图，由 Rust 共享引擎驱动。不仅实现文本、光标、滚动等全套原生手势，更完美搬入了桌面端的核心功能——抽屉分组拖动、Markdown 流式渲染、`/@` 快捷面板、附件与权限卡片。同时大幅优化了手机端右侧栏（文件树折叠、只读查看、Git 历史与差异对比）及终端的跨标签页复用。
- **支持跨实例克隆与迁移会话：** 项目、分组或单选/批量选中的会话现在可以从一个 Vibex 实例导出并导入到另一个实例，自动同步缺失的 Provider 或模型，并保留提示缓存。
- **Antigravity 成为一等 Agent：** 提供专属方言配置与事件富化器，工具调用统一标准化，支持 `extra_high` 思考深度，直连 Provider 改用 `gemini-api-key` 认证。
- **Agent 运行时版本全面跟进：** 各主流 Agent 运行时已升级至最新上游版本（如 Claude 0.112.0、Codex 2.12.0 等），并保留上一版作为兼容下限。
- **解除两小时硬性超时限制：** 长任务不再在两小时处被强制中断；若连续十分钟没有输出，输入框上方会友好提示（可选择停止并继续，或忽略）。
- **桌面端开发版支持安装本地服务端：** 调试版或预览版可通过 `VIBEX_DEV_SERVER_ARTIFACT` 正常安装本地构建的压缩包服务端。
- **官方 Nix flake 支持：** `flake.nix` 直接打包已发布的 Linux 产物并通过 `autoPatchelfHook` 处理依赖，使 NixOS 无需 `LD_LIBRARY_PATH` 即可流畅运行。
- **桌面端若干细节修复：** 草稿换行时输入框瞬时展开、独立滚动条轨道颜色更准确、修复了切换会话时偶尔丢失已加载时间线历史的问题，右栏遍历性能优化彻底解决大文件卡顿。

---

### ⬆️ 升级指南

**如何升级：** 直接用 **0.1.10** 版本覆盖旧版本即可，桌面端、Android 和 iOS 均支持原地覆盖升级。您的会话、工作区、Provider 配置、Skills、远程主机和草稿均会被完整保留。

**注意事项：**

1. **移动端重做：** 全新移动端正式回归，旧版用户可直接覆盖升级，系统会平滑导入原有的配对与设置。
2. **Agent 运行时：** 建议将各 Agent 运行时更新至最新版本，以获得最佳的工具事件与交互体验（旧版本仍受兼容下限保护）。
3. **长任务提示：** 任务不再有 2 小时自动截断限制；若任务连续 10 分钟无输出会有温馨提示，系统不会自动取消。
4. **NixOS 用户：** 官方 flake 已适配 0.1.10，可在无附加环境变量的情况下直接安全运行。
