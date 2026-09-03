<p align="center">
  <a href="README.md">English</a> &nbsp;|&nbsp;
  <a href="README.zh-CN.md">简体中文</a>
</p>

<h1 align="center">
  <img src="logo-wordmark-white.svg" alt="Vibex" width="168" />
</h1>

<p align="center">
  <strong>你的 Agent, 你的工作区, 你的掌控——从提示词到提交, 从桌面到移动端。</strong><br />
  面向 Agent 驱动软件开发的原生、本地优先 AI 编程工作台。
</p>

<p align="center">
  <a href="https://github.com/vibex-ai/vibex"><img src="https://img.shields.io/github/stars/vibex-ai/vibex?style=flat&logo=github" alt="GitHub stars" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--or--later-blue?style=flat" alt="许可证: AGPL-3.0 或更高版本" /></a>
  <img src="https://img.shields.io/badge/Rust-1.97.0-black?style=flat&logo=rust&logoColor=white" alt="Rust 1.97.0" />
  <img src="https://img.shields.io/badge/UI-GPUI-2563eb?style=flat" alt="GPUI" />
  <img src="https://img.shields.io/badge/status-0.1.0--rc.1-f97316?style=flat" alt="发布状态: 0.1.0-rc.1" />
</p>

<p align="center">
  <a href="#快速开始">快速开始</a> &nbsp;&bull;&nbsp;
  <a href="#功能">功能</a> &nbsp;&bull;&nbsp;
  <a href="#架构">架构</a> &nbsp;&bull;&nbsp;
  <a href="#开发">开发</a>
</p>

<h2 align="center">支持的 Agent</h2>

<p align="center">
  <a href="https://docs.anthropic.com/en/docs/claude-code"><img src="https://img.shields.io/badge/Claude%20Code-D97757?style=for-the-badge&logoColor=white&logo=claude" alt="Claude Code" />&nbsp;</a>
  <a href="https://github.com/openai/codex"><img src="https://img.shields.io/badge/Codex-000000?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyBmaWxsPSIjZmZmIiByb2xlPSJpbWciIHZpZXdCb3g9IjAgMCAyNCAyNCIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj48dGl0bGU%2BT3BlbkFJPC90aXRsZT48cGF0aCBkPSJNMjIuMjgxOSA5LjgyMTFhNS45ODQ3IDUuOTg0NyAwIDAgMC0uNTE1Ny00LjkxMDggNi4wNDYyIDYuMDQ2MiAwIDAgMC02LjUwOTgtMi45QTYuMDY1MSA2LjA2NTEgMCAwIDAgNC45ODA3IDQuMTgxOGE1Ljk4NDcgNS45ODQ3IDAgMCAwLTMuOTk3NyAyLjkgNi4wNDYyIDYuMDQ2MiAwIDAgMCAuNzQyNyA3LjA5NjYgNS45OCA1Ljk4IDAgMCAwIC41MTEgNC45MTA3IDYuMDUxIDYuMDUxIDAgMCAwIDYuNTE0NiAyLjkwMDFBNS45ODQ3IDUuOTg0NyAwIDAgMCAxMy4yNTk5IDI0YTYuMDU1NyA2LjA1NTcgMCAwIDAgNS43NzE4LTQuMjA1OCA1Ljk4OTQgNS45ODk0IDAgMCAwIDMuOTk3Ny0yLjkwMDEgNi4wNTU3IDYuMDU1NyAwIDAgMC0uNzQ3NS03LjA3Mjl6bS05LjAyMiAxMi42MDgxYTQuNDc1NSA0LjQ3NTUgMCAwIDEtMi44NzY0LTEuMDQwOGwuMTQxOS0uMDgwNCA0Ljc3ODMtMi43NTgyYS43OTQ4Ljc5NDggMCAwIDAgLjM5MjctLjY4MTN2LTYuNzM2OWwyLjAyIDEuMTY4NmEuMDcxLjA3MSAwIDAgMSAuMDM4LjA1MnY1LjU4MjZhNC41MDQgNC41MDQgMCAwIDEtNC40OTQ1IDQuNDk0NHptLTkuNjYwNy00LjEyNTRhNC40NzA4IDQuNDcwOCAwIDAgMS0uNTM0Ni0zLjAxMzdsLjE0Mi4wODUyIDQuNzgzIDIuNzU4MmEuNzcxMi43NzEyIDAgMCAwIC43ODA2IDBsNS44NDI4LTMuMzY4NXYyLjMzMjRhLjA4MDQuMDgwNCAwIDAgMS0uMDMzMi4wNjE1TDkuNzQgMTkuOTUwMmE0LjQ5OTIgNC40OTkyIDAgMCAxLTYuMTQwOC0xLjY0NjR6TTIuMzQwOCA3Ljg5NTZhNC40ODUgNC40ODUgMCAwIDEgMi4zNjU1LTEuOTcyOFYxMS42YS43NjY0Ljc2NjQgMCAwIDAgLjM4NzkuNjc2NWw1LjgxNDQgMy4zNTQzLTIuMDIwMSAxLjE2ODVhLjA3NTcuMDc1NyAwIDAgMS0uMDcxIDBsLTQuODMwMy0yLjc4NjVBNC41MDQgNC41MDQgMCAwIDEgMi4zNDA4IDcuODcyem0xNi41OTYzIDMuODU1OEwxMy4xMDM4IDguMzY0IDE1LjExOTIgNy4yYS4wNzU3LjA3NTcgMCAwIDEgLjA3MSAwbDQuODMwMyAyLjc5MTNhNC40OTQ0IDQuNDk0NCAwIDAgMS0uNjc2NSA4LjEwNDJ2LTUuNjc3MmEuNzkuNzkgMCAwIDAtLjQwNy0uNjY3em0yLjAxMDctMy4wMjMxbC0uMTQyLS4wODUyLTQuNzczNS0yLjc4MThhLjc3NTkuNzc1OSAwIDAgMC0uNzg1NCAwTDkuNDA5IDkuMjI5N1Y2Ljg5NzRhLjA2NjIuMDY2MiAwIDAgMSAuMDI4NC0uMDYxNWw0LjgzMDMtMi43ODY2YTQuNDk5MiA0LjQ5OTIgMCAwIDEgNi42ODAyIDQuNjZ6TTguMzA2NSAxMi44NjNsLTIuMDItMS4xNjM4YS4wODA0LjA4MDQgMCAwIDEtLjAzOC0uMDU2N1Y2LjA3NDJhNC40OTkyIDQuNDk5MiAwIDAgMSA3LjM3NTctMy40NTM3bC0uMTQyLjA4MDVMOC43MDQgNS40NTlhLjc5NDguNzk0OCAwIDAgMC0uMzkyNy42ODEzem0xLjA5NzYtMi4zNjU0bDIuNjAyLTEuNDk5OCAyLjYwNjkgMS40OTk4djIuOTk5NGwtMi41OTc0IDEuNDk5Ny0yLjYwNjctMS40OTk3WiIvPjwvc3ZnPg%3D%3D" alt="Codex" />&nbsp;</a>
  <a href="https://z.ai"><img src="https://img.shields.io/badge/ZCode-1F63EC?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMCAzMCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTE1LjQ3LDcuMWwtMS4zLDEuODVjLTAuMiwwLjI5LTAuNTQsMC40Ny0wLjksMC40N2gtNy4xVjcuMDlDNi4xNiw3LjEsMTUuNDcsNy4xLDE1LjQ3LDcuMXoiLz48cG9seWdvbiBwb2ludHM9IjI0LjMsNy4xIDEzLjE0LDIyLjkxIDUuNywyMi45MSAxNi44Niw3LjEgIi8%2BPHBhdGggZD0iTTE0LjUzLDIyLjkxbDEuMzEtMS44NmMwLjItMC4yOSwwLjU0LTAuNDcsMC45LTAuNDdoNy4wOXYyLjMzSDE0LjUzeiIvPjwvc3ZnPg%3D%3D" alt="ZCode" />&nbsp;</a>
  <a href="https://opencode.ai"><img src="https://img.shields.io/badge/OpenCode-000000?style=for-the-badge&logoColor=white&logo=opencode" alt="OpenCode" />&nbsp;</a>
  <a href="https://antigravity.google/docs/ide/extensions"><img src="https://img.shields.io/badge/Antigravity-3589FD?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTEyIDFjLjYgNS44IDUuMiAxMC40IDExIDExLTUuOC42LTEwLjQgNS4yLTExIDExLS42LTUuOC01LjItMTAuNC0xMS0xMSA1LjgtLjYgMTAuNC01LjIgMTEtMTF6Ii8%2BPC9zdmc%2B" alt="Antigravity" />&nbsp;</a>
  <a href="https://cline.bot/cli"><img src="https://img.shields.io/badge/Cline-18181B?style=for-the-badge&logoColor=white&logo=cline" alt="Cline" />&nbsp;</a>
  <a href="https://www.codebuddy.cn/cli/"><img src="https://img.shields.io/badge/Codebuddy%20Code-6C4DFF?style=for-the-badge&logoColor=white&logo=codebuddy" alt="Codebuddy Code" />&nbsp;</a>
  <a href="https://docs.cursor.com/en/cli/overview"><img src="https://img.shields.io/badge/Cursor-000000?style=for-the-badge&logoColor=white&logo=cursor" alt="Cursor" />&nbsp;</a>
  <a href="https://geminicli.com"><img src="https://img.shields.io/badge/Gemini%20CLI-8E75B2?style=for-the-badge&logoColor=white&logo=googlegemini" alt="Gemini CLI" />&nbsp;</a>
  <a href="https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli"><img src="https://img.shields.io/badge/GitHub%20Copilot-1F2328?style=for-the-badge&logoColor=white&logo=githubcopilot" alt="GitHub Copilot" />&nbsp;</a>
  <a href="https://cli.devin.ai/docs"><img src="https://img.shields.io/badge/Devin-0E1015?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHJlY3QgeD0iNCIgeT0iMyIgd2lkdGg9IjQiIGhlaWdodD0iMTgiIHJ4PSIxIi8%2BPHJlY3QgeD0iMTAiIHk9IjMiIHdpZHRoPSI0IiBoZWlnaHQ9IjE4IiByeD0iMSIgdHJhbnNmb3JtPSJza2V3WCgtMTIpIiAvPjxyZWN0IHg9IjE2IiB5PSIzIiB3aWR0aD0iNCIgaGVpZ2h0PSIxOCIgcng9IjEiLz48L3N2Zz4%3D" alt="Devin" />&nbsp;</a>
  <a href="https://docs.x.ai/build/overview"><img src="https://img.shields.io/badge/Grok-000000?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTEzIDIgMyAxNGg3bC0xIDggMTItMTRoLThsMC02eiIvPjwvc3ZnPg%3D%3D" alt="Grok" />&nbsp;</a>
  <a href="https://hermes-agent.nousresearch.com/docs/user-guide/features/acp"><img src="https://img.shields.io/badge/Hermes-3B3F40?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPGNpcmNsZSBjeD0iNyIgY3k9IjEyIiByPSIyLjQiLz48Y2lyY2xlIGN4PSIxMyIgY3k9IjEyIiByPSIyLjQiLz48Y2lyY2xlIGN4PSIxOSIgY3k9IjEyIiByPSIyLjQiLz48L3N2Zz4%3D" alt="Hermes" />&nbsp;</a>
  <a href="https://github.com/MoonshotAI/kimi-cli"><img src="https://img.shields.io/badge/Kimi%20Code-000000?style=for-the-badge&logoColor=white&logo=moonshotai" alt="Kimi Code" />&nbsp;</a>
  <a href="https://github.com/svkozak/pi-acp"><img src="https://img.shields.io/badge/Pi-09090B?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4MDAgODAwIj48cGF0aCBmaWxsPSIjZmZmIiBmaWxsLXJ1bGU9ImV2ZW5vZGQiIGQ9Ik0xNjUuMjkgMTY1LjI5IEg1MTcuMzYgVjQwMCBINDAwIFY1MTcuMzYgSDI4Mi42NSBWNjM0LjcyIEgxNjUuMjkgWiBNMjgyLjY1IDI4Mi42NSBWNDAwIEg0MDAgVjI4Mi42NSBaIi8%2BPHBhdGggZmlsbD0iI2ZmZiIgZD0iTTUxNy4zNiA0MDAgSDYzNC43MiBWNjM0LjcyIEg1MTcuMzYgWiIvPjwvc3ZnPg%3D%3D" alt="Pi" />&nbsp;</a>
  <a href="https://github.com/vibex-ai/deepseek-harness-acp"><img src="https://img.shields.io/badge/DeepSeek%20Harness-5786FE?style=for-the-badge&logoColor=white&logo=deepseek" alt="DeepSeek Harness" />&nbsp;</a>
  <a href="https://agentclientprotocol.com"><img src="https://img.shields.io/badge/%2B%20Any%20ACP%20Agent-57606A?style=for-the-badge&logoColor=white&logo=data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTEyIDRhMS42IDEuNiAwIDAgMSAxLjYgMS42djQuOGg0LjhhMS42IDEuNiAwIDEgMSAwIDMuMmgtNC44djQuOGExLjYgMS42IDAgMSAxLTMuMiAwdi00LjhINS42YTEuNiAxLjYgMCAxIDEgMC0zLjJoNC44VjUuNkExLjYgMS42IDAgMCAxIDEyIDR6Ii8%2BPC9zdmc%2B" alt="+ Any ACP Agent" />&nbsp;</a>
</p>

Vibex 将 Agent 会话、源代码、Git、终端、预览和供应商配置整合到一个专注的工作台中。桌面运行时统一管理 Agent 进程、会话历史、工作区文件、Git、PTY、供应商和权限。原生移动端可以远程监控并操控相同的会话，可选的自托管 Relay 只承载加密帧。

运行本地桌面端无需 Vibex 托管的工作区。你的代码仓库、凭据和持久化会话状态都会保留在你选择的设备上。

## 为什么选择 Vibex

| 原则 | 实际体验 |
| --- | --- |
| **本地优先的权威模型** | PC 负责运行时和持久化状态。移动端是类型安全的远程客户端，而不是第二个后端。 |
| **原生优先** | 桌面端和移动端均由 Rust 与 GPUI 驱动，不依赖浏览器壳，也没有独立的 WebUI 产品。 |
| **统一的 Agent 契约** | 所有在线 Agent 会话都使用 ACP 与托管适配器，集成共享同一套会话、能力、权限和终端界面。 |
| **远程访问不锁定** | 可以通过 Direct、Tailnet 或自建 Relay 配对。Relay 只转发加密帧，不会成为第二个数据库。 |
| **可检查的工作流** | 计划、工具调用、进程详情、差异、审批、询问表单、错误和重连状态都清晰可见。 |
| **自由选择供应商** | 在 Vibex 作用域内配置 Agent、模型供应商、凭据、MCP 服务器、Skills、Prompts 和 Hooks。 |

## 功能

Vibex 面向完整的开发闭环：让 Agent 修改代码，检查它的操作，运行项目，审查差异，并在需要时从任何位置继续会话。

| 工作流 | 功能包含 |
| --- | --- |
| **Agent 会话** | 以结构化时间线流式呈现用户与 Agent 消息, 支持 Markdown、推理、计划、工具与进程详情、附件、审批和类型化询问表单。可以停止、继续、分叉、重命名和恢复会话, 也可以导入受支持的本地历史记录。 |
| **工作区与编辑器** | 浏览工作区范围内的文件树, 按名称或内容搜索, 在版本检查下读写文件, 并明确处理编码、换行符和大文件限制。 |
| **Git 原生审查** | 查看状态、历史、差异和 blame; 创建与切换分支; 暂存、取消暂存、还原、提交、拉取和推送; 管理隔离的 Git worktree, 并提供受保护的合并和 rebase 恢复流程。 |
| **终端与内容** | 使用支持 ANSI 仿真的原生 PTY, 提供标签页、调整大小、滚动缓冲区、搜索和原始字节处理。在工作台中预览 Markdown、图片、PDF 及受支持的 Office 文档。 |
| **Agent 与供应商中心** | 发现和管理 Agent 运行时, 选择模型与推理选项, 完成供应商认证, 执行健康检查与能力探测, 并将供应商配置与在线路由身份分开管理。 |
| **MCP 与工作流资源** | 导入、校验并限定 MCP 服务器、Skills、Prompts 和 Hooks 的作用域, 为 Agent 提供可复用上下文, 避免配置散落在不同的 home 目录。 |
| **自动化** | 安排一次性、周期性或每日 Agent 运行, 通过自动化图编排任务, 并提供明确的运行状态、恢复能力和审计历史。 |
| **提醒与连续性** | 会话完成、失败或等待输入时接收桌面通知, 跟踪未读完成项, 并在重连或重启后恢复权威时间线状态。 |
| **移动端伴侣** | 将 iOS 或 Android 客户端与桌面运行时配对, 查看相同的 GUI Agent 时间线, 批准或拒绝请求, 发送后续指令, 检查文件和 Git 状态, 并在可用时使用远程终端界面。 |

## 支持的 Agent

Vibex 使用 [Agent Client Protocol](https://agentclientprotocol.com/) (ACP) 作为唯一的在线 Agent 传输协议。运行时与供应商无关: Agent 的身份和能力与连接它所使用的协议彼此分离。

内置预设包括 **Claude Code**、**Codex**、**ZCode** 和 **OpenCode**。ACP 目录还包含 **Antigravity**、**Cline**、**Codebuddy Code**、**Cursor**、**Gemini CLI**、**GitHub Copilot**、**Devin**、**Grok**、**Hermes**、**Kimi Code**、**Pi**、**DeepSeek Harness** 等集成。Vibex 会根据本机已安装的运行时、供应商配置和实时能力探测结果判断可用性。

你也可以注册任意兼容 ACP 的可执行程序, 配置其命令、参数、环境变量和显示元数据。新增 Agent 无需修改 UI, 也无需再实现一套厂商专用的会话协议。

## 原生产品形态

| 产品形态 | 作用 | 入口 |
| --- | --- | --- |
| **桌面端** | 完整的原生 GPUI 工作台, 也是 Agent、工作区、文件、Git、PTY、供应商、权限和持久化状态的权威 `DesktopRuntime`。 | 支持 Linux、macOS 和 Windows; 使用 `pnpm dev:desktop` 启动。 |
| **移动端** | 原生 GPUI iOS 和 Android 伴侣应用。将桌面会话模型渲染为 GUI 时间线, 并发送类型化远程变更。 | `apps/mobile`; 使用 `pnpm build:mobile:android` 或 `pnpm build:mobile:ios` 构建。 |
| **自托管 Relay** | 可选的 Rust/Axum 加密 WebSocket 帧传输服务。不存储工作区、供应商、Agent 或应用数据。 | `apps/relay-server` 和 `deploy/relay`; 使用 `pnpm smoke:relay:local` 验证。 |

移动端不会启动本地 Agent, 不拥有工作区文件系统, 不修改本地 Git, 也不会成为第二个 PTY 权威。它的紧凑原生 UI 保留桌面端 Agent 时间线语义, 包括加载、流式输出、审批、错误和重连状态。

## 快速开始

### 前置条件

- 支持子模块的 Git
- Node.js 22
- pnpm 11.3.0
- Rust 1.97.0, 由 `rust-toolchain.toml` 固定
- 用于桌面开发的图形会话和可用 Vulkan 驱动

原生移动端构建还需要 Android SDK/NDK 与 `cargo-ndk`, 或者 macOS、Xcode、XcodeGen 以及 Apple Rust targets。准确命令见下面的平台章节。

### 运行桌面工作台

```bash
git clone https://github.com/vibex-ai/vibex.git
cd vibex
git submodule update --init --recursive
pnpm install --frozen-lockfile
pnpm dev:desktop
```

桌面应用会以本地运行时启动。在 Config Center 中配置 Agent 和供应商, 打开项目目录, 然后从工作台创建 Agent 会话。

### 仓库检查

```bash
pnpm check
pnpm release:build-smoke
pnpm check:mobile-native
```

`pnpm check` 会运行常规 Rust、前端、许可证和确定性行为检查。`pnpm release:build-smoke` 检查原生桌面端、移动端和 Relay 的构建图。`pnpm check:mobile-native` 验证移动端 crate 和已提交的原生项目契约, 但不会假设当前主机安装了 Android 或 iOS SDK。

## 开发

### 桌面端打包

迭代开发时可以先运行针对性的桌面检查:

```bash
cargo check -p vibex-desktop --locked
cargo test -p vibex-desktop --locked
pnpm smoke:first-frame
```

Linux 预览包使用 cargo-packager 和经过审查的 PDFium 运行时:

```bash
cargo install cargo-packager --version 0.11.8 --locked
pnpm prepare:pdfium
pnpm package:preview
```

构建 RC 或 Stable 包前请阅读[发布运行手册](docs/operations/release.md)。生成的安装包和运行时二进制文件属于构建产物, 不会提交到仓库。

### Android

安装 Android SDK/NDK、`cargo-ndk` 以及所需 ABI 对应的 Rust targets。Debug 默认使用 `arm64-v8a` 和 `x86_64`; Release 默认使用 `arm64-v8a`。

```bash
pnpm build:mobile:android
pnpm package:mobile:android
```

设置 `VIBEX_MOBILE_ANDROID_TARGETS` 可以用空格分隔的 ABI 列表覆盖 Debug 默认值。Release APK 未签名, 分发前必须使用目标发布密钥完成对齐和签名。

### iOS

在 macOS 上安装 Xcode、XcodeGen 以及 `aarch64-apple-ios` 和 `aarch64-apple-ios-sim` Rust targets:

```bash
pnpm build:mobile:ios
```

该命令会构建 `VibexFFI.xcframework` 并生成 Xcode 项目。代码签名、模拟器或真机选择以及分发凭据由开发者或发布流水线在本地管理。

### 自托管 Relay

Relay 是可选组件。可以这样在本地运行:

```bash
docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server
curl -fsS http://127.0.0.1:9700/health
docker compose -f deploy/relay/docker-compose.yml down
```

关于通过 Caddy 使用 HTTPS、在私有 Tailnet 中发布、运行时限制以及可选的运营方推送适配器, 请阅读 [Relay 部署指南](deploy/relay/README.md)。本地开发请保持默认的 loopback 绑定。

## 架构

桌面运行时是唯一的权威状态所有者。共享契约和投影让原生桌面端与移动端保持一致, 同时避免重复业务状态。

```text
DesktopRuntime
|-- ACP Agent 会话与权威时间线
|-- 工作区文件、编辑器、Git 与托管 worktree
|-- 原生 PTY、内容预览、供应商与权限
`-- 类型化 RemoteGateway v2
    |-- Direct 或 Tailnet 传输
    `-- 加密的自托管 Relay
            -> AutoRemoteTransport
            -> 原生移动端 GPUI 客户端
```

主要的所有权边界如下:

- `crates/core` 定义序列化 id、DTO、错误、能力以及远程协议契约。
- `crates/desktop-model` 负责与框架无关的会话和时间线投影及 reducer。
- `crates/vibex-backend` 提供原生适配器与远程适配器共享的、与供应商无关的能力 facade。
- `crates/vibex-ui` 负责语义化 token、可移植组件模型、工作流控制器和 shell 组合。
- `crates/vibex-remote-client` 负责配对、重连、同步以及 Direct、Tailnet 和 Relay 路由选择。
- `apps/desktop` 负责原生工作台和 `DesktopRuntime`; `apps/mobile` 只将共享的远程投影组合成紧凑的原生界面。

详细契约请阅读 [UI 架构边界](docs/architecture/ui-boundary.md)、[平台支持矩阵](docs/platform/support-matrix.md) 和 [Remote Protocol v2](docs/remote/protocol-v2.md)。

## 技术栈

| 层 | 技术 |
| --- | --- |
| **应用** | Rust 2024、Cargo workspace、Rust 1.97.0 |
| **原生 UI** | GPUI、`gpui-component`、共享 Rust UI 契约、生成式设计 token |
| **Agent 运行时** | Agent Client Protocol schema 1.6、托管 ACP 适配器、Tokio |
| **工作区运行时** | 基于 `rusqlite` 的 SQLite、文件系统与 Git 服务、`xpty`、`alacritty_terminal` |
| **内容处理** | `pulldown-cmark`、HTML5ever、MathJax SVG、Mermaid 渲染、PDFium、ZIP/XML 解析器 |
| **远程传输** | Axum、HTTP/WebSocket v2、Rustls、`tokio-tungstenite`、X25519、HKDF、HMAC、ChaCha20-Poly1305 |
| **平台** | Linux、macOS 和 Windows 桌面端; 通过 `gpui_android` 与 `gpui_ios` 支持原生 Android 和 iOS |
| **工具链** | Node.js 22、pnpm 11.3.0、确定性 smoke tests 和基于证据的发布门禁 |

## 隐私与控制

- **本地优先存储:** 会话时间线、工作区元数据、供应商配置和运行时状态由桌面运行时持有并存储在本地。
- **明确的权限控制:** Agent 工具、终端操作、设备访问和远程变更都经过能力门控并可审计。
- **加密远程访问:** Direct、Tailnet 和 Relay 路由都使用类型化握手与加密传输。Relay 只转发帧, 无法解密业务载荷。
- **有作用域的供应商配置:** 默认情况下 Vibex 不会改写真实 Agent home 配置。导出操作需要用户明确触发, 并提供预览和回滚边界。
- **脱敏诊断:** token、私钥、提示词、文件内容和终端字节不会写入日志和证据产物。

## 参与贡献

欢迎提交 Issue 和 Pull Request。提交变更前请注意:

- 保持 `DesktopRuntime` 为唯一权威状态所有者, 移动端行为必须通过类型化后端和远程契约路由。
- 迭代时运行与改动相关的最小检查集, 涉及跨层改动时再运行 `pnpm check`。
- 如果改动了发布门禁覆盖的行为, 请同时更新对应的证据。

## 许可证

Vibex 是自由软件, 采用 [GNU Affero General Public License v3.0 或更高版本](LICENSE) 授权。第三方包、字体、图标和 PDFium 的声明记录在 [docs/licenses](docs/licenses/README.md) 中。
