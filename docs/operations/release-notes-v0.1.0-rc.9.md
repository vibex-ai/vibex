# Vibex v0.1.0-rc.9 Release Notes

- Released: 2026-11-04 · Range: `v0.1.0-rc.8...v0.1.0-rc.9` · 499 commits

---

## English

### 🚀 Highlights

- **A local model gateway** — One loopback endpoint now fronts every shared Provider. It speaks OpenAI Chat Completions, OpenAI Responses, Anthropic Messages and Google Generative AI, translates between them, and forwards each call to whichever Provider serves the requested model.
- **Routes that fail over** — A route is one selectable model. Compose it from whole Providers or from `(Provider, model)` pairs, pick one of seven strategies, and let the gateway move to the next member when one cannot answer.
- **A rebuilt Provider workspace** — Providers, route groups and the gateway now share one workspace with a foldable sidebar, identity headers and in-place editing. A Provider you configured by hand became a reusable template, and preset model lists come from the models.dev catalogue.
- **SSH and WSL remote runtimes** — The desktop can put its authoritative runtime on another machine: install `vibex-server` over SSH, open a tunnel, pair automatically, and upgrade with a rollback that keeps the runtime's identity.
- **Self-hosted artifacts that ship themselves** — Headless server tarballs for Linux x86_64/aarch64 and macOS are built in a pinned Debian 12 builder, the release mirrors stable images to Docker Hub and CNB, an AUR package republishes the Linux `.deb`, and the Linux payload is a third smaller.
- **The timeline reads like a document** — Tool calls render as structure rather than JSON: path and value fields, todo checklists, questions as lists, and edits as diffs. Headers lead with intent, outcome and timing.
- **Every printed key is a button** — In the TUI, a `key label` legend is now a control: clicking it presses exactly the key it names, through the same dispatcher the keyboard uses.
- **Ten more guardians** — The new-session arena grew from six encounters to sixteen, each with its own entrance, lane rhythm and companion effect.

### 🛠 New features

**Local model gateway**

- One loopback endpoint serves the four wire APIs coding Agents already speak and forwards each request to the shared Provider that serves the requested model; protocol translation happens through a single intermediate representation, including tool calls re-encoded across streaming shapes.
- A conversation stays on the Key that holds its prompt cache, so pinning never costs a cache miss; routing state (cooldowns, pins, rotation, inflight) never enters an Agent process fingerprint, so replacing a Key never restarts a process.
- Concurrency and request rate are bounded per Key: a lane caps in-flight requests, a full lane waits in a bounded FIFO queue and only then answers 429, and a rolling minute caps starts, so a burst degrades into a wait instead of an unbounded backlog.
- Route groups gained a second shape: `ProviderGroup` composes whole Providers and offers the **intersection** of their enabled catalogues, while `ModelGroup` names `(Provider, model)` pairs behind one Agent-facing id. A model only one Provider serves is deliberately not part of a group, because the group's strategy must never send a model to a Provider that has never heard of it.
- Seven strategies — ordered, round-robin, least-used, smart, lowest-latency, weighted and manual — all computable from local observations. A route may set a first-token deadline that bounds both the response headers and the first streamed token.
- Circuit breakers are scoped to a route group rather than to a Provider, and an undeclared protocol is probed instead of being refused.
- The request ledger records which route a request actually took: members skipped because they were unavailable are separate from attempts that failed, and the trace carries ids and failure kinds only — never a prompt, a reply or Key material.
- The Gateway page is a workspace of its own: a live route map that replays each request, expandable request history, p50/p95 traffic with selectable ranges, alert thresholds, default timeouts, upstream ordering, and a clear action that states its retention window and asks first.
- A route named in a session is what the Agent asks for, and a direct Provider stays a one-member route on the same code path, so routing rules never grow a second variant.

**Providers**

- The shared Provider record is the write authority: editing one place reaches every Agent bound to it, and a binding may deliberately name no Provider when the Agent authenticates itself.
- The Provider workspace was rebuilt from scratch: a left column folding the gateway, Providers and route groups under one New menu, a shared identity header, underline tabs and section cards, in-place settings, and route groups editing on their own page.
- Presets ship as data — 65 of them — and no longer carry model ids: applying one reads the vendor's entry from the bundled models.dev catalogue (53 providers), including limits and capability flags, so a preset does not age the moment a vendor ships a model.
- A Provider you configured by hand becomes a reusable template under a **User-defined** heading. The template is projected from the record on every read, carries no secret-valued header, is addressed with a `user:` prefix so it can never shadow a shipped preset, and can be dismissed and restored without touching the Provider, its Key or its models.
- A Provider created from a template records where it came from, so a copy never becomes a template of its own and the library cannot grow a copy of a copy.
- Provider Key rows identify a credential by a masked fingerprint instead of a generated label, and an eye button reveals the value through an explicit read while ordinary snapshots carry only the mask.
- Request-header overrides are projected to native Agents and to the gateway; import previews one source at a time, pages large model lists, and tests a draft before it is saved; a draft address and Key fetch their models; per-key model scope and proxy are configurable.
- The catalogue suggests capabilities from a maintained table, and an unfunded account now reads as unfunded instead of failing its model list.

**Remote runtimes (SSH and WSL)**

- The desktop installs `vibex-server` on a remote Linux host, starts it on loopback with a pinned TLS certificate, opens an `ssh -L` tunnel, claims the one-time pairing code over SSH itself and connects over Remote Protocol v2 — no Docker, no public port, no copied code.
- A nine-step provisioning state machine owns the order, cancellation safe points, retry after failure and the invariant that a runtime id is exposed only once it is ready; a duplicate begin returns the attempt already running.
- An environment probe checks Linux, architecture, glibc, disk headroom, home write access, `sha256sum` and the target port with typed blocking errors, and reports Git, `SHELL` and an unknown port as warnings.
- The tunnel is supervised: a watchdog checks the child every 500 ms and rebuilds the same tunnel spec once, surfacing the restart or the failure instead of leaving a silent dead loopback endpoint.
- Upgrades are transactional: the new artifact is installed beside the current one, the runtime identity survives, and a runtime that will not start rolls back to the previous version, which is started and checked.
- Uninstall can preserve the home directory, and an SSH profile can be edited, removed and re-provisioned; a paired SSH connection can be removed even before it ever provisioned.
- macOS remote hosts are supported through a LaunchAgent, with arm64 and x86_64 server artifacts published for them.
- WSL2 distros are provisioned with no sshd at all: upload, install, start, wait for actual localhost forwarding, claim pairing on loopback, and restore after `wsl --shutdown`.
- A documented root bootstrap script installs sshd for hosts that lack it without touching `PasswordAuthentication`, enabling root or changing the firewall, and a cloud-init template covers providers where the key is pasted at VM creation.
- Remote script arguments are shell-quoted and hostile values reach the script verbatim; pairing codes are kept out of the server log; the remote server log is readable from diagnostics, and platform failures carry recovery guidance.
- Remote instances now live under **Settings → Remote instances** in the workspace group and are always listed in navigation, search and the command palette; the earlier Developer switch and its environment variable are gone, and a runtime that is active in the registry boots and switches like any other remote runtime.
- The title-bar runtime panel became a switcher over the instances already paired, and its footer action opens Settings instead of carrying a second pairing form.

**Timeline and sessions**

- Tool calls render as readable structure instead of pretty JSON: `path: value` fields, todo lists as checklists, ask-user questions and answers as lists, harness output envelopes unwrapped, and edits or writes drawn as diffs with `+N/-M` — counted in the turn's changed files and on mobile too.
- Titles that only named the tool defer to the call's own command, path or query, and the `cd <absolute path> &&` prefix no longer pushes the command itself past the truncation point.
- Activity headers lead with intent and outcome: a failed call shows the first line it printed while collapsed, settled commands and Agent calls show how long they ran, fetches get their own kind and globe icon, and a group summary leads with the files it changed.
- Workspace images referenced from a message — relative, root-relative, absolute or `file://` — render inline, confined to the workspace, and open a full-window preview on click; transcript images became clickable thumbnails.
- Built-in tool panels are chosen per session rather than per Agent: the new-session page records an explicit on/off per panel, an absent entry follows the Agent, and a fork or delegation copies the map.
- Long bodies are read through their own viewport, a running session opens onto its history rather than its live run, and the reader's place survives content landing above it.
- The new-session composer gained a fullscreen control, and auto-continue became a shell-wide default that continues without an explicit opt-in.
- The sidebar narrows its session list with the same funnel posture menu the session row already draws — all, running, waiting for you, unread, failed and pinned — and batch work follows the visible rows.
- Open editor tabs follow a file that changed on disk, and the light theme was retuned to a cool neutral ladder with solid, opaque module fills.

**Markets and management**

- The MCP market asks the registry the reader's own question instead of walking a capped background catalogue, so a name deep in the published order is reachable immediately; paging walks the registry cursor and a result set is kept per question.
- Market entries now carry the publisher's own picture and a remote entry's required headers, which become install inputs routed to the server's headers rather than its environment.
- The Skill market reads a public catalogue that answers in about a second: a ranked home page parsed once and shared for six hours, a browse page that folds one repository to at most four entries, and installs lifted out of the repository the catalogue names.
- Market cards install on click, search runs as you type across MCP servers and Skills, and per-feature Agent capsules choose which Agents receive a feature.

**TUI**

- Every `key label` legend is a press target: the queue and dock bands, modal footers, the settings footer and the new-session page's bracketed hints publish their rect with the chord they name, and a press runs through the same `handle_key` path the keyboard uses.
- `Chord::from_hint` reads a legend the way a reader reads it — `^S`, `Space`, `↑`, `J/K`, `Alt+↑↓` — and a paired arrow is split into two one-cell targets so the arrow under the pointer picks the direction.
- The queue band and the dock now scroll to keep their cursors visible, and a click maps through the window the band drew instead of counting lines from the top of the rect.
- The Providers table can switch its gateway and shows plan presets with their prices; imported Keys are named by what they do rather than by a generic warning, and columns that cannot fit are dropped instead of being squeezed.
- A settled tool call stays folded, the answer that lands in front of the reader stays open, and `Ctrl+C` interrupts only the session page.
- The prompt band is sized at the width the composer actually paints.

**Mobile**

- The text ladder is read at the size it paints, the connecting page carries the Vibex wordmark, sync badges draw as text, and the CBDT emoji font ships as an APK asset.
- Files and commits search while typing, the reader's place is kept when the timeline scrolls, and a session that ends hands the screen to the reader.
- The resume path no longer aborts on a dead connection, new sessions open eagerly, and the terminal holds still while a keyboard closes.

**Arena**

- Ten guardians joined the original six, for sixteen encounters in all: Cursor, Cline, Grok, ZCode, Kimi, CodeBuddy, Antigravity, Devin, Gemini and Hermes.
- Each guardian owns its entrance, exit and lane rhythm, and each companion effect is unlocked by its own win.

### ⚡️ Performance

- The gateway virtualizes the request ledger and pages history, and a request appears in the ledger while it runs rather than only once it has finished.
- Long transcript bodies are read in chunks through a viewport, and windowed bodies fade at their edges instead of laying out everything they hold.
- The model list virtualizes, and a draft's endpoint and Key are probed once rather than on every keystroke.
- The Skill market's search replaced an eight-to-thirteen-second round trip with roughly one second, and the MCP market no longer depends on a background walk finishing.
- Linux x86_64 release links run through lld with `--icf=all`, folding duplicate monomorphised bodies.

### 🔧 Under the hood

- Versions bumped to `0.1.0-rc.9` across the workspace, the desktop packager inputs, the Android versionName fallback, the READMEs, the deployment docs and the reviewed asset bundles.
- The license SBOM and third-party notices were regenerated for the current graph: 1080 Cargo packages, 10 asset and font bundles, and 5 native and installer inputs, all approved.
- Two workspace members arrived — `crates/model-gateway` for the gateway and `crates/model-catalog` for the bundled catalogue — and the database schema moved from 63 to 87.
- The workspace moved to `gpui-kit` `0.8.0`, and `rustls` to `0.23.46` and `zip` to `9.1.3`.
- The open security findings were cleared: a path-injection read in the Config Center was replaced by asking the database itself, a reducer helper was renamed so a test assertion no longer looks like credential logging, and the Linux GTK stack moved to gtk 0.19 with a `tray-icon` StatusNotifierItem backend, which removes the `glib` 0.18 advisory from the lockfile.
- Release engineering: headless server artifacts are linked inside a digest-pinned Debian 12 builder and checked against a glibc 2.35 floor, macOS server artifacts are built and ad-hoc signed, stable container images are mirrored to Docker Hub and CNB by copying the index GHCR already published, and `vibex-bin` republishes the reviewed Linux `.deb` on the AUR with checksums and `namcap` verification.
- The Linux `.deb` is repacked into xz and drops from 119.8 MB to 69.8 MB; the packaging host now needs `xz` and `ld.lld`.
- macOS and Windows packaging remains build and package evidence, and PDFium distribution approval still covers linux-x86_64 only.
- Version literals inside the SSH test fixtures stay on `rc.8` on purpose: the upgrade and rollback tests need a real previous release to upgrade from.

---

## 中文

### 🚀 核心亮点

- **本地模型网关** — 一个回环端点现在挡在所有共享 Provider 前面。它说 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 和 Google Generative AI 四种协议，在它们之间翻译，并把每次调用转给服务所请求模型的 Provider。
- **会故障转移的路由** — 路由就是一个可选模型。可以用整个 Provider 组合，也可以用 `(Provider, model)` 组合，从七种策略里挑一种，某个成员答不出来时网关自动走到下一个。
- **重建的 Provider 工作区** — Provider、路由组和网关合成一个工作区：可折叠侧栏、统一的身份头、原地编辑。手工配置的 Provider 变成可复用模板，预设的模型列表改由 models.dev 目录提供。
- **SSH 与 WSL 远程运行时** — 桌面端可以把权威运行时放到另一台机器上：通过 SSH 安装 `vibex-server`、建立隧道、自动配对，并在升级失败时回滚且保住运行时身份。
- **自托管产物自己发布** — Linux x86_64/aarch64 与 macOS 的无头服务器包在固定版本的 Debian 12 构建器里产出；稳定版镜像同时镜像到 Docker Hub 和 CNB；AUR 包重新发布 Linux `.deb`；Linux 负载体积缩小三分之一。
- **对话流像文档一样可读** — 工具调用不再是一坨 JSON，而是结构化呈现：路径与值字段、待办清单、以列表呈现的提问，以及以差异呈现的编辑。头部先讲意图、结果和耗时。
- **终端里每个画出来的键都是按钮** — TUI 中 `键 标签` 形式的图例现在就是控件：点击它会按下它写明的那个键，走的是键盘同一条分发路径。
- **又多了十位守卫** — 新建会话首页的竞技场从六场战斗扩到十六场，每位守卫都有自己的登场、巡游节奏和随从效果。

### 🛠 新功能

**本地模型网关**

- 一个回环端点提供编码 Agent 已经在用的四种线路协议，并把每个请求转给服务所请求模型的共享 Provider；协议翻译走同一套中间表示，包括在流式形态之间重新编码工具调用。
- 会话留在持有其提示缓存的 Key 上，因此固定不会带来缓存未命中；路由状态（冷却、固定、轮换、在途）永远不进入 Agent 进程指纹，所以换 Key 不会重启进程。
- 并发与请求速率按 Key 设限：泳道限制在途请求数，泳道满时先进有界 FIFO 队列等待，之后才回 429，滚动一分钟限制启动次数，突发流量退化为等待而不是无界积压。
- 路由组多了一种形态：`ProviderGroup` 组合整个 Provider，提供它们已启用目录的**交集**；`ModelGroup` 把 `(Provider, model)` 对放在一个面向 Agent 的 id 后面。只有一家 Provider 能服务的模型故意不属于组，因为组的策略绝不能把模型发给一个从没听说过它的 Provider。
- 七种策略——顺序、轮询、最少使用、智能、最低延迟、加权、手动——全部可由本地观测算出。路由可以设置首 token 期限，同时约束响应头和第一个流式 token。
- 熔断器按路由组而非 Provider 划分，未声明的协议会被探测而不是直接拒绝。
- 请求账本记录一次请求真正走过的路由：因不可用而被跳过的成员与试过但失败的尝试分开记录，追踪只带 id 和失败类型——绝不含提示、回复或 Key 材料。
- 网关页面本身就是一个工作区：可重放每次请求的实时路由图、可展开的请求历史、可选取时间范围的 p50/p95 流量、告警阈值、默认超时、上游排序，以及会说明保留窗口并先确认的清空操作。
- 会话里点名的路由就是 Agent 请求的对象，直连 Provider 仍是同一条代码路径上的单成员路由，因此路由规则不会长出第二套实现。

**Provider**

- 共享 Provider 记录是写入权威：在一处编辑会到达所有绑定它的 Agent，而当 Agent 自己认证时，绑定也可以故意不指定 Provider。
- Provider 工作区从零重建：左栏把网关、Provider 和路由组折在同一个「新建」菜单下，统一的身份头、下划线标签和分区卡片，设置原地编辑，路由组在自己的页面上编辑。
- 预设作为数据发布——共 65 个——不再自带模型 id：应用预设时从随包的 models.dev 目录（53 家 Provider）读取该厂商条目，包括上下文上限与能力标记，因此预设不会在厂商发新模型的那一刻过时。
- 手工配置的 Provider 会变成可复用模板，放在**用户自定义**标题下。模板每次读取时从记录投影而来，不带任何密钥类请求头，id 带 `user:` 前缀因此永不覆盖内置预设，可以移除也可以恢复，而 Provider、它的 Key 和模型都不受影响。
- 从模板创建的 Provider 会记录来源，因此副本不会自己变成模板，模板库不会长出「副本的副本」。
- Provider 的 Key 行改用掩码指纹标识凭证，而不是生成的标签；眼睛按钮通过一次显式读取展示真值，普通快照只带掩码。
- 请求头覆盖会投射到原生 Agent 和网关；导入一次预览一个来源，大模型列表分页，保存前先测试草稿；填好地址与 Key 的草稿会去抓模型；支持按 Key 的模型范围与代理。
- 目录会从维护好的表中建议模型能力，未充值账号会如实显示为未充值，而不是让模型列表失败。

**远程运行时（SSH 与 WSL）**

- 桌面端在远程 Linux 主机上安装 `vibex-server`，以固定 TLS 证书在回环上启动，打开 `ssh -L` 隧道，自己通过 SSH 领取一次性配对码，再用 Remote Protocol v2 连接——不需要 Docker、不需要公网端口、不需要手动抄配对码。
- 九步开通状态机负责顺序、取消安全点、失败重试，以及「运行时 id 只在就绪后才暴露」这一不变量；重复发起会返回已经在跑的那次尝试。
- 环境探测检查 Linux、架构、glibc、磁盘余量、home 可写、`sha256sum` 和目标端口，并以带类型的阻断错误报告；Git、`SHELL` 和未知端口只作为警告。
- 隧道受监管：看门狗每 500 毫秒检查子进程，并按同一份隧道规格重建一次，把重启或失败如实呈现，而不是留下一个静默死掉的回环端点。
- 升级是事务性的：新产物装在当前版本旁边，运行时身份保留；起不来的运行时会回滚到上一版，并启动、校验上一版。
- 卸载可以选择保留 home 目录，SSH profile 可以编辑、移除和重新开通；已配对的 SSH 连接即使还没开通也能被删除。
- macOS 远程主机通过 LaunchAgent 支持，并为其发布 arm64 与 x86_64 服务器产物。
- WSL2 发行版完全不需要 sshd：上传、安装、启动、等待 localhost 转发真正可用、在回环上领取配对，并在 `wsl --shutdown` 后恢复。
- 提供一份可审计的 root 引导脚本，为没有 sshd 的主机安装它，同时不改 `PasswordAuthentication`、不启用 root、不动防火墙；cloud-init 模板则覆盖在创建 VM 时就粘贴公钥的厂商。
- 远程脚本参数会做 shell 转义，恶意取值会原样到达脚本；配对码不会进入服务器日志；远程服务器日志可从诊断页读取，平台类失败会附带恢复指引。
- 远程实例现在位于工作区分组的**设置 → 远程实例**，始终出现在导航、搜索和命令面板中；此前的开发者开关及其环境变量已移除，注册表里处于激活状态的运行时和其他远程运行时一样启动和切换。
- 标题栏的运行时面板变成已配对实例的切换器，底部动作改为打开设置，而不再携带第二套配对表单。

**对话流与会话**

- 工具调用以可读结构呈现而不是美化后的 JSON：`路径: 值` 字段、待办清单、以列表呈现的提问与回答、解包后的 harness 输出信封，编辑或写入画成带 `+N/-M` 的差异——并计入本回合的变更文件，移动端同样如此。
- 只写了工具名的标题会让位给调用自己的命令、路径或查询，`cd <绝对路径> &&` 前缀不再把命令本身挤过截断点。
- 活动头部先讲意图和结果：失败的调用在折叠时就显示它打印的第一行，结束的命令和 Agent 调用显示耗时，抓取类调用有了自己的类型和地球图标，分组摘要先讲它改了哪些文件。
- 消息里引用的工作区图片——相对、根相对、绝对或 `file://`——会内联渲染，被限制在工作区内，点击打开全窗口预览；对话流里的图片也变成可点缩略图。
- 内置工具面板改为按会话选择：新建会话页为每个面板记录明确的开关，缺省条目跟随 Agent，分叉或委派会复制这份映射。
- 长正文通过自己的视口读取，运行中的会话打开时落在历史上而不是当前这轮，上方内容落进来时读者的位置不会被顶走。
- 新建会话输入框增加了全屏控件，自动续跑成为整个 shell 的默认值，无需显式开启也会继续。
- 侧栏用与会话行相同的漏斗姿态菜单收窄会话列表——全部、运行中、等你处理、未读、失败、已置顶——批量操作跟随可见行。
- 打开的编辑器标签会跟随磁盘上的改动，浅色主题重新调成冷调中性阶梯，模块底色改为实心不透明。

**市场与管理**

- MCP 市场直接向注册表提出读者自己的问题，而不是走一遍有上限的后台目录，因此深藏在发布顺序里的名字也能立刻找到；分页沿着注册表游标走，结果集按问题保留。
- 市场条目现在带上发布方自己的图片，远程条目必需的请求头会成为安装输入并送入服务器的 headers 而不是 environment。
- 技能市场改读一个约一秒响应的公开目录：首页排名只解析一次并共享六小时，浏览页把同一个仓库折叠到最多四条，安装则从目录点名的仓库里取出。
- 市场卡片点击即安装，MCP 服务器与技能都支持边输入边搜索，按功能的 Agent 胶囊用来选择哪些 Agent 获得该能力。

**TUI**

- 每个 `键 标签` 图例都是可点目标：队列与停靠带、弹窗页脚、设置页脚、新建会话页的方括号提示都会发布自己的矩形和它写明的键，点击走与键盘相同的 `handle_key` 路径。
- `Chord::from_hint` 按读者的读法解析图例——`^S`、`Space`、`↑`、`J/K`、`Alt+↑↓`——成对的箭头会拆成两个单格目标，指针下面的那个箭头决定方向。
- 队列带与停靠带会滚动以保持光标可见，点击会经过该带真正绘制的窗口映射，而不是从矩形顶端数行。
- Provider 表格可以切换它使用的网关，并按价格展示套餐预设；导入的 Key 按其实际作用命名而不是给一句泛泛的警告，放不下的列会被丢弃而不是硬挤。
- 结束的工具调用保持折叠，落在读者眼前的回答保持展开，`Ctrl+C` 只中断会话页。
- 输入带的尺寸按输入框真正绘制的宽度来计算。

**移动端**

- 字号阶梯按真正绘制的尺寸读取，连接页带上了 Vibex 字标，同步徽标改为文字绘制，CBDT emoji 字体作为 APK 资源随包发布。
- 文件与提交支持边输入边搜索，对话流滚动时读者位置保持不变，会话结束时把屏幕交还给读者。
- 连接已死时恢复路径不再直接中止，新建会话会立即打开，键盘收起时终端保持不动。

**竞技场**

- 在原初六位之外新增十位守卫，共十六场战斗：Cursor、Cline、Grok、ZCode、Kimi、CodeBuddy、Antigravity、Devin、Gemini 和 Hermes。
- 每位守卫都有自己的登场、退场和巡游节奏，每个随从效果由各自的那场胜利解锁。

### ⚡️ 性能

- 网关把请求账本虚拟化并对历史分页，请求在运行时就出现在账本里，而不是结束后才出现。
- 对话流的长正文按块从自己的视口读取，开窗的正文在边缘淡出，而不是把它装的内容全部排版。
- 模型列表虚拟化，草稿的端点与 Key 只探测一次，而不是每次按键都探。
- 技能市场的搜索把一次八到十三秒的往返换成约一秒，MCP 市场也不再依赖后台目录走完。
- Linux x86_64 发布链接走 lld 的 `--icf=all`，折叠重复的单态化代码。

### 🔧 底层

- 版本号统一升到 `0.1.0-rc.9`：工作区、桌面打包输入、Android versionName 回退值、README、部署文档，以及许可证策略里已审核的资源包版本。
- 许可证 SBOM 与第三方声明按当前依赖图重新生成：1080 个 Cargo 包、10 个资源／字体包、5 个原生与安装器输入，全部已批准。
- 工作区新增两个成员——`crates/model-gateway`（网关）和 `crates/model-catalog`（随包目录）——数据库 schema 从 63 升到 87。
- 工作区升级到 `gpui-kit` `0.8.0`，`rustls` 升到 `0.23.46`、`zip` 升到 `9.1.3`。
- 已清掉未决的安全告警：配置中心那处路径注入改为直接问数据库本身，一个 reducer 辅助函数改名后测试断言不再看起来像在记录凭证，Linux GTK 栈迁到 gtk 0.19 并改用 `tray-icon` 的 StatusNotifierItem 后端，从而把 `glib` 0.18 的公告移出锁文件。
- 发布工程：无头服务器产物在摘要固定的 Debian 12 构建器内链接，并按 glibc 2.35 下限检查；macOS 服务器产物构建并做 ad-hoc 签名；稳定版容器镜像通过复制 GHCR 已发布的索引镜像到 Docker Hub 与 CNB；`vibex-bin` 在 AUR 上重新发布已审核的 Linux `.deb`，并做校验和与 `namcap` 验证。
- Linux `.deb` 改为 xz 重打包，体积从 119.8 MB 降到 69.8 MB；打包主机现在需要 `xz` 和 `ld.lld`。
- macOS 与 Windows 打包仍属于构建与打包证据，PDFium 分发批准仍只覆盖 linux-x86_64。
- SSH 测试夹具里的版本字面量故意留在 `rc.8`：升级与回滚测试需要一个真实的上一版本来升级。
