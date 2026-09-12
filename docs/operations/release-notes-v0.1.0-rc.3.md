# Vibex v0.1.0-rc.3 Release Notes

- Released: 2026-09-12 · Range: `v0.1.0-rc.2...v0.1.0-rc.3` · 91 commits

---

## English

### Highlights

- **The remote authority now serves the workbench** — a paired headless `vibex-server` answers almost every desktop domain over Remote v2: the Config Center as one aggregated read, the MCP / Skills / Prompts / Hooks / Automation / Scheduled management domains, Agent lifecycle and credentials, sign-in, terminals, the code workbench, session search and child-Agent timelines, worktree lifecycle, provider usage, and the device audit trail (`ec6e378`, `847feb9`, `4b4f42d`, `7b51cce`).
- **Pair a server from a connection link** — `vibex-server` prints a `vibex://pair#/code/<payload>` string that carries the server URL, the one-time code, and the DER certificate, next to a `sha256:` fingerprint and a QR code; desktop and mobile pin the certificate before the first request, so a LAN runtime no longer needs a public CA (`8e2e523`).
- **One backend facade on the desktop** — the Config Center, Management Center, Agent account lifecycle, terminals, elicitations, inline message rewrite, and child-Agent timelines all reach the authority through `BackendFacade`, so local and paired runtimes behave identically (`ffaffbc`, `84d1e4c`, `33cfd6c`).
- **gpui-kit on both clients** — desktop moves to DataTable, `Bubble`, TabBar, Tags, Marker, Form/Field, `Select`, and `Collapsible` while retiring hand-rolled widgets; mobile adopts the kit theme and overlay root and rebuilds its inputs on the kit `Input` and `Textarea` (`cf2bc8b`, `4836301`, `976e11e`).
- **Mobile composer long-press and IME** — a long press opens an in-app selection toolbar with drag handles instead of the kit's focus-stealing menu, selects the word under the finger on Android, and keeps the soft keyboard up (`e2f176f`, `79fe1fd`, `4e7ea2c`).
- **Model limits and thinking depth** — Provider Models declare their context and output limits and their thinking depth, route models project DeepSeek Harness reasoning efforts correctly, and the quadratic streaming projection is now linear with virtualized thought blocks (`24996aa`, `6fec8b9`, `ce66427`, `ce4adcc`).

### New Features

**Remote & server**

- Serve the Config Center as one aggregated authority read, then fill in its MCP, Skills, Prompts, Hooks, Automation, Scheduled, Advanced, managed-Agent, and native-config management domains (`ec6e378`, `847feb9`, `9efd0f8`, `d08b3bd`, `cec95da`, `4b4f42d`)
- Close the remaining provider-settings gaps and serve Agent lifecycle, runtime option probes, configuration updates, auth-context seeding, Agent sign-in, and credential storage over Remote v2 (`28e4517`, `4a074ee`, `f1d613f`, `669b114`, `5d6bb7a`, `e0a72da`)
- Discover Agent-owned model catalogues and resolve composer command discovery on the authority (`95c7137`, `2adca41`)
- Serve sessions, provider usage summaries, project deletion, worktree rename, and the worktree lifecycle on the authority; load child-Agent timelines and the session search index remotely; rewrite the last user message over Remote v2 (`7b51cce`, `161c8bd`, `ca54b6d`, `8457d36`, `33cfd6c`)
- Drive the desktop code workbench through the backend facade and serve the workbench, composer, and sign-in terminals remotely (`64ea211`, `fcf8153`); inject the terminal transport into the terminal surface so either authority can back it (`85936a9`)
- Read the device audit trail and serve Config Center recovery over Remote v2 (`b9c23a1`, `5facfd3`); serve the agent delegation MCP sidecar from the headless binary (`4239bea`)
- Pair a `vibex-server` client from a connection link: `vibex://pair#/code/<payload>` carries the server URL, one-time code, and DER certificate, printed beside a `sha256:` fingerprint and a QR, so both clients pin the certificate before the first request (`8e2e523`)

**Desktop**

- Let a provider model declare its context and output limits (`24996aa`) and edit each model's thinking depth in the provider editor (`6fec8b9`); let a route model declare its own thinking depth (`61c077c`)
- Fold Agent selection into the provider and model menu (`5055a20`)
- Project Cline providers with a catalogue model picker (`4da7999`)
- Load the Config Center through the aggregated authority read and drive its MCP, Skills, and provider actions remotely (`39b1ecf`, `63d85f3`); move the remaining Config Center actions and provider-profile duplication onto the backend facade and give the Management Center a facade handle (`ffaffbc`, `503057c`, `84d1e4c`)
- Run the Agent account lifecycle through the backend (`8087687`); poll durable submissions and interrupt through the backend (`7866dee`)
- Write legacy Agent model-provider profiles and secrets through the backend facade (`5ea9758`, `6795317`)
- Say why Config Center reads are unavailable on a paired runtime (`260402b`)
- Migrate desktop UI onto gpui-kit: DataTable for the usage statistics table, `Bubble` for user messages, segmented tabs, tags, marker, and form fields, `Select` for the new-session base-ref picker, `Collapsible` for the composer collaboration reveal, and kit replacements for hand-rolled widgets (progress ring, tab bars, button group, alerts, spinner, status bar, and duplicated input wrappers) (`6e91977`, `af7d9c8`, `bd9af83`, `d3009b9`, `06ffa7f`, `cf2bc8b`)

**Mobile**

- Adopt the gpui-kit theme and overlay root on the phone: `gpui_component::init`, a `Root` window owning the kit overlay layers, and `theme::apply_component_theme` pointed at the shared `vibex_ui` tokens (`4836301`)
- Replace the hand-rolled composer with the kit `Textarea` and move the file editor onto it (`976e11e`, `0835b87`)
- Move the workbench fields, sidebar search, runtime model search, pairing and prompt fields, and runtime feature fields onto the kit `Input` (`d4cc328`, `2eb68e8`, `43231a5`, `eed1801`, `b075519`)
- Draw the composer's own selection toolbar and drag handles over the input, and surface long press as a right click on Android (`e2f176f`, `488871e`)

### Fixes

**Remote & server**

- Keep configured pairing routes across startup reconcile and align advertised capabilities with the device grant (`c4854ea`, `1e2eabf`)
- Keep the temporary session root in the runtime home instead of the OS temporary directory, so a published workspace survives a container recreation, and name the two recoverable session-create rejections — `remote_agent_workspace_root_missing` and `remote_agent_workspace_mode_mismatch` — while still never echoing an authority path (`c7f19f5`)

**Desktop**

- Keep one sidebar arrangement per runtime authority, so a paired server's project list can no longer reconcile against the embedded runtime's, and release the embedded runtime home lock on authority switch (`2146c29`, `abdcc97`)
- Resolve elicitations through the backend facade (`94fa9f1`)
- Stop the inline message edit bubble from collapsing (`c5fb5f2`)
- Restore tab captions dropped by the gpui-kit `Tab` icon slot (`455b697`)
- Make streaming projection linear and virtualize thought blocks, and reclaim the stale streaming row extent on session restore (`ce4adcc`, `97c5113`)
- Size the usage table so `DataTable` renders rows, and sync its delegate when the statistics or dimension change (`12df972`, `f4cba90`)
- Disable frosted glass on Windows' blurless renderer (`b02b3f1`)

**Mobile**

- Stop the keyboard collapsing under the long-press menu (`79fe1fd`)
- Select the word under a long press on Android and follow the caret when syncing Android's IME mirror (`4e7ea2c`, `c2a7764`)

**Provider & Agent config**

- Keep provider secrets on a host without a keychain: a `HostFile` backend writes an owner-only `provider-secrets.json`, with `VIBEX_PROVIDER_SECRET_STORE=keychain|file` to override the choice (`3e18938`)
- Keep the system role for DeepSeek Harness route models (`b2041e2`), project the Harness's reasoning efforts for route models (`ce66427`), and keep prompt images deliverable (`0f5bb4c`)
- Resolve the effort option id as reasoning effort (`b5eab03`)
- Match per-Agent model provider wire protocols, including Google Vertex as a first-class protocol (`0815a6a`)
- Key the Agent account model catalogue by a resource-free fingerprint (`6cc0346`)

**Theme & platform**

- Fix the macOS directory-picker drive detection (`b107e36`)

### Internal, Build & Docs

- Bump the vendored zed submodule to `f748b84d68` for the `gpui_apple` update (`854081d`)
- Bump gpui-component and gpui-kit-assets from 0.6.0 to 0.6.1 (`0413be7`)
- Update the server deployment assets: `.dockerignore` and the Dockerfile (`3ccdb68`)
- Add the gpui-kit and design-guides agent skills (`c30dc24`)
- Document the current remote-client coverage in the `deploy/server` README and the architecture baseline (`0d58732`), and record in spec which desktop concerns still run only on the local runtime (`d6a1dc1`, `b55f68e`)
- Add the v0.1.0-rc.2 release note (`c7bff6f`)
- Drop a useless `BackendError` conversion that failed the workspace clippy gate (`5e83215`)

---

## 中文

### 亮点

- **远程权威端已承接整个工作台** — 配对后的无头 `vibex-server` 现在通过 Remote v2 应答几乎全部桌面端领域：配置中心的单次聚合读取，MCP / Skills / Prompts / Hooks / Automation / Scheduled 管理域，Agent 生命周期与凭据、登录、终端、代码工作台、会话搜索与子 Agent 时间线、worktree 生命周期、Provider 用量以及设备审计记录（`ec6e378`、`847feb9`、`4b4f42d`、`7b51cce`）。
- **用连接链接配对服务器** — `vibex-server` 会打印 `vibex://pair#/code/<payload>` 连接串，其中同时携带服务器地址、一次性配对码与 DER 证书，并在旁边给出 `sha256:` 指纹与二维码；桌面端与移动端在发出第一个请求前即固定该证书，局域网运行时不再需要公共 CA（`8e2e523`）。
- **桌面端统一走后端门面** — 配置中心、管理中心、Agent 账户生命周期、终端、elicitation、消息内联重写与子 Agent 时间线全部经 `BackendFacade` 触达权威端，本地运行时与配对运行时行为一致（`ffaffbc`、`84d1e4c`、`33cfd6c`）。
- **两个客户端同时接入 gpui-kit** — 桌面端改用 DataTable、`Bubble`、TabBar、Tags、Marker、Form/Field、`Select` 与 `Collapsible`，并下线手写组件；移动端接入 kit 主题与浮层根节点，输入全部重建在 kit 的 `Input` 与 `Textarea` 之上（`cf2bc8b`、`4836301`、`976e11e`）。
- **移动端输入框长按与输入法** — 长按改为应用内绘制的选择工具条与拖拽手柄（不再使用会抢焦点、收起软键盘的 kit 菜单），Android 上直接选中长按处的单词，键盘保持展开（`e2f176f`、`79fe1fd`、`4e7ea2c`）。
- **模型上限与思考深度** — Provider 模型可声明上下文与输出上限以及思考深度，路由模型的 DeepSeek Harness 推理强度投影修正，二次复杂度的流式投影改为线性并对思考块启用虚拟化（`24996aa`、`6fec8b9`、`ce66427`、`ce4adcc`）。

### 新功能

**远程与服务端**

- 配置中心改为权威端的单次聚合读取，随后补齐 MCP、Skills、Prompts、Hooks、Automation、Scheduled、Advanced、托管 Agent 与原生配置管理域（`ec6e378`、`847feb9`、`9efd0f8`、`d08b3bd`、`cec95da`、`4b4f42d`）
- 补完剩余 provider 设置缺口，并将 Agent 生命周期、运行时选项探测、配置更新、认证上下文播种、Agent 登录与凭据存储搬到 Remote v2（`28e4517`、`4a074ee`、`f1d613f`、`669b114`、`5d6bb7a`、`e0a72da`）
- 在权威端发现 Agent 自有的模型目录，并解析输入框命令补全（`95c7137`、`2adca41`）
- 在权威端提供会话、Provider 用量摘要、项目删除、worktree 重命名与 worktree 生命周期；远程加载子 Agent 时间线与会话搜索索引；通过 Remote v2 重写最后一条用户消息（`7b51cce`、`161c8bd`、`ca54b6d`、`8457d36`、`33cfd6c`）
- 让桌面端代码工作台走后端门面，并远程提供工作台、输入框与登录终端（`64ea211`、`fcf8153`）；把终端传输注入终端界面，使两种权威端都可承载（`85936a9`）
- 通过 Remote v2 读取设备审计记录并提供配置中心恢复（`b9c23a1`、`5facfd3`）；无头二进制直接提供 agent delegation MCP sidecar（`4239bea`）
- 通过连接链接配对 `vibex-server` 客户端：`vibex://pair#/code/<payload>` 携带服务器地址、一次性配对码与 DER 证书，并与 `sha256:` 指纹、二维码一同打印，两端在首个请求前即固定证书（`8e2e523`）

**桌面端**

- Provider 模型可声明上下文与输出上限（`24996aa`），并可在 Provider 编辑器中编辑每个模型的思考深度（`6fec8b9`）；路由模型也可声明自己的思考深度（`61c077c`）
- 将 Agent 选择并入 Provider 与模型菜单（`5055a20`）
- 以目录模型选择器投影 Cline Provider（`4da7999`）
- 配置中心改走权威端聚合读取，其 MCP、Skills 与 Provider 操作远程执行（`39b1ecf`、`63d85f3`）；其余配置中心操作与 Provider profile 复制搬到后端门面，并给管理中心接入门面句柄（`ffaffbc`、`503057c`、`84d1e4c`）
- Agent 账户生命周期走后端（`8087687`）；持久化提交轮询与中断走后端（`7866dee`）
- 旧版 Agent 模型 Provider profile 与密钥写入走后端门面（`5ea9758`、`6795317`）
- 配对运行时明确提示配置中心读取不可用的原因（`260402b`）
- 桌面端 UI 迁移到 gpui-kit：用量统计表改用 DataTable，用户消息改用 `Bubble`，分段标签页、标签、Marker 与表单字段接入 kit，新会话 base-ref 选择器改用 `Select`，输入框协作区展开改用 `Collapsible`，并以 kit 组件替换手写控件（进度环、标签条、按钮组、提示条、加载动画、状态栏与重复的输入框包装）（`6e91977`、`af7d9c8`、`bd9af83`、`d3009b9`、`06ffa7f`、`cf2bc8b`）

**移动端**

- 手机端接入 gpui-kit 主题与浮层根节点：`gpui_component::init`、承载 kit 浮层层的 `Root` 窗口，以及指向共享 `vibex_ui` token 的 `theme::apply_component_theme`（`4836301`）
- 用手写输入框替换为 kit `Textarea`，文件编辑器一并迁移（`976e11e`、`0835b87`）
- 工作台字段、侧栏搜索、运行时模型搜索、配对与提示词字段、运行时功能字段全部迁移到 kit `Input`（`d4cc328`、`2eb68e8`、`43231a5`、`eed1801`、`b075519`）
- 在输入框之上自绘选择工具条与拖拽手柄，并在 Android 上把长按映射为右键（`e2f176f`、`488871e`）

### 修复

**远程与服务端**

- 启动对账时保留已配置的配对路由，并让对外声明能力与设备授权保持一致（`c4854ea`、`1e2eabf`）
- 临时会话根目录改放到运行时 home 下（而非操作系统临时目录），容器重建后已发布的 workspace 依然可用；同时区分两种可恢复的会话创建拒绝原因（`remote_agent_workspace_root_missing` 与 `remote_agent_workspace_mode_mismatch`），且仍不回显权威端路径（`c7f19f5`）

**桌面端**

- 侧栏布局按运行时权威端分别保存，配对服务器的项目列表不再与内嵌运行时的布局互相覆盖；切换权威端时释放内嵌运行时的 home 锁（`2146c29`、`abdcc97`）
- elicitation 走后端门面解析（`94fa9f1`）
- 消息内联编辑气泡不再塌陷（`c5fb5f2`）
- 恢复被 gpui-kit `Tab` 图标槽吞掉的标签文字（`455b697`）
- 流式投影改为线性并对思考块启用虚拟化；会话恢复时回收过期的流式行高度（`ce4adcc`、`97c5113`）
- 调整用量表尺寸使 `DataTable` 能渲染数据行，并在统计或维度变化时同步其 delegate（`12df972`、`f4cba90`）
- Windows 无模糊渲染器下关闭毛玻璃（`b02b3f1`）

**移动端**

- 长按菜单不再收起软键盘（`79fe1fd`）
- Android 下选中长按处的单词，并在同步 Android 输入法镜像时跟随光标（`4e7ea2c`、`c2a7764`）

**Provider 与 Agent 配置**

- 无钥匙串主机也能保存 Provider 密钥：新增 `HostFile` 后端，写入仅属主可读的 `provider-secrets.json`，可用 `VIBEX_PROVIDER_SECRET_STORE=keychain|file` 覆盖（`3e18938`）
- DeepSeek Harness 路由模型保留 system 角色（`b2041e2`），为其投影 Harness 的推理强度（`ce66427`），并保证提示词图片可正常投递（`0f5bb4c`）
- 将 effort 选项 id 解析为推理强度（`b5eab03`）
- 对齐各 Agent 的模型 Provider 通信协议，并把 Google Vertex 提升为一等协议（`0815a6a`）
- Agent 账户模型目录改按去除资源标识的指纹做键（`6cc0346`）

**主题与平台**

- 修复 macOS 目录选择器的磁盘检测（`b107e36`）

### 内部、构建与文档

- vendored zed 子模块升级到 `f748b84d68`，包含 `gpui_apple` 更新（`854081d`）
- gpui-component 与 gpui-kit-assets 由 0.6.0 升至 0.6.1（`0413be7`）
- 更新服务端部署资产：`.dockerignore` 与 Dockerfile（`3ccdb68`）
- 新增 gpui-kit 与设计指南 agent 技能（`c30dc24`）
- 在 `deploy/server` README 与架构基线中记录当前远程客户端覆盖范围（`0d58732`），并在 spec 中记录仍仅运行于本地运行时的桌面端功能（`d6a1dc1`、`b55f68e`）
- 新增 v0.1.0-rc.2 发布说明（`c7bff6f`）
- 移除一处多余的 `BackendError` 转换，修复工作区 clippy 门禁（`5e83215`）
