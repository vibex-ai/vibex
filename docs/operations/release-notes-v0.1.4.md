# Vibex v0.1.4 Release Notes

- **Release Date:** 2026-11-12
- **Summary:** A release about work reaching every surface: Linux gains RPM packages, a shared Provider now fans out to every bound Agent, Codex honours declared reasoning efforts and context windows, WSL setup reports its progress, and mobile session sync carries far less traffic.

---

## English

### 🚀 Highlights

- **Linux RPM packages:** the Linux release now ships `.rpm` for x86_64 and aarch64 beside the `.deb` and AppImage, repacked from the already reviewed `.deb` payload, and the updater manifest binds it as an `rpm/system_installer` artifact.
- **A shared Provider reaches every Agent:** editing a Provider on the Providers page now updates every Agent bound to it, instead of only the Agent the record was first derived from.
- **Codex honours your declared models:** an alias model with declared reasoning efforts or a context window is projected into a private Codex catalogue, so the effort is offered and the declared window applies.
- **WSL runtime setup shows its progress:** adding a WSL runtime now reports each setup step and keeps the connection listed under **Remote connections** with its live status.
- **Mobile session sync is lighter:** the phone fetches attachment bytes only when a picture is opened, and reasoning updates carry only the new delta.

### 🐧 Linux packaging

- Linux x86_64 and aarch64 now publish an `.rpm` alongside the `.deb` and AppImage. `scripts/build-rpm.mjs` extracts the reviewed `.deb` data payload and drives `rpmbuild`, so the installed files, modes, and the embedded PDFium runtime stay byte-identical to the `.deb`; ELF `NEEDED` entries become portable SONAME requirements.
- The tagged publish job uploads the RPM, the release page lists its download, and the signed updater manifest carries it as an `rpm/system_installer` artifact.

### 🖥 Desktop

- **The update panel opens on demand:** a background update check no longer opens the title-bar panel by itself. The release still surfaces through the title-bar entry, and the panel opens on click. The panel is drawn as an in-tree layer so dialogs paint above it and Settings stays readable.
- **WSL runtime setup shows its progress:** choosing **Connect WSL** now reports the distro check, download, install, start, forwarding, pairing, and connect steps as they happen, and keeps running if the dialog is closed. The connection stays under **Remote connections** with its live status, so a failed attempt can be retried or inspected there.

### 🔌 Providers and Agents

- **A shared Provider is one record, and every bound Agent now sees it.** A Model added or edited on the Providers page fans out to each Agent's own launch row and runtime profile, while each Agent still shows only the protocols it declares. Deleting a Model stays on the Providers page, where the full catalogue is the input, so saving an Agent page no longer removes Models other Agents can still reach.
- Gateway-backed route groups and remote dispatcher writes now republish the catalogue too, so a Provider edit, a deleted Provider, or a Key change refreshes every affected Agent, including writes that arrive from a paired device.
- **Codex gets a projected catalogue.** Codex answers `model/list` from a catalogue that only describes OpenAI's own model ids, so an alias with a declared context window or reasoning depth reported an empty effort list and an unprojected window. Vibex now writes a private `vibex-models.json` beside the Profile's `config.toml` that lists every enabled Model with its declared context window and thinking levels, and points `model_catalog_json` at it. A Profile that declares neither keeps its previous output unchanged.
- **A managed Agent update no longer leaves a stale catalogue:** an install or rollback changes the launch fingerprint, and a stored snapshot is now treated as current only when it matches that fingerprint, so new sessions no longer fail with `runtime_switch_configuration_unavailable`.

### 📱 Mobile and remote

- **Mobile session sync carries less:** timeline reads replace inline attachments with a bounded marker and fetch the detail item only when the picture opens; `agent_session` events are scoped to the open session, with only notifications while the app is backgrounded; and a gap is recovered with bounded catch-up pages instead of a cold window.
- Reasoning chunks are sent as individual deltas instead of one growing live item, and inline attachment bytes are discounted from the timeline page budget.
- **Timeline pages now fit the Relay frame:** a remote timeline page is clamped to 512 KiB, leaving headroom for the E2EE envelope inside the Relay's WebSocket frame, which otherwise closed the PC socket and left the phone retrying into a room with no authority. The Relay's frame limit is exposed as `VIBEX_RELAY_MAX_WEBSOCKET_FRAME_BYTES` (`maxWebsocketFrameBytes`, default 2 MiB).

### 🛠 Skills and Marketplace

- **Install no longer waits for the whole preview:** the Install button stays live while a Skill folder is fetched and previewed, so a press on a large Skill is remembered and starts the install the moment the preview lands. A press after a failed read restarts the read and arms the install; the pending press is cleared when the pane stops owning it. The bytes written are still exactly the bytes the preview disclosed.

### ⬆️ Upgrading

Install 0.1.4 over 0.1.3 as usual: sessions, workspaces, Provider configuration, Skills, and remote hosts carry over. On Linux the new RPM is an alternative to the existing `.deb`; the Windows and macOS desktop packages are unchanged.

---

## 中文

### 🚀 核心亮点

- **Linux RPM 安装包：** Linux 版本在 `.deb` 和 AppImage 之外新增 x86_64 与 aarch64 的 `.rpm`，由已经审核过的 `.deb` 载荷重新打包；更新清单会把它记为 `rpm/system_installer` 产物。
- **共享 Provider 会同步到每个 Agent：** 在 Providers 页面修改 Provider 后，所有绑定它的 Agent 都会更新，而不再只有最初派生出该记录的 Agent。
- **Codex 会采用你声明的模型：** 别名模型声明的推理档位或上下文窗口会写入 Codex 的私有模型目录，档位可选，声明的窗口也会生效。
- **WSL 运行时安装显示进度：** 添加 WSL 运行时后会逐项报告安装步骤，连接也会以实时状态保留在 **远程连接** 列表中。
- **移动端会话同步更省流量：** 手机只在打开图片时才获取附件字节，推理更新只发送新增的增量。

### 🐧 Linux 打包

- Linux x86_64 与 aarch64 现在会在 `.deb` 和 AppImage 之外发布 `.rpm`。`scripts/build-rpm.mjs` 解出已审核 `.deb` 的数据载荷并驱动 `rpmbuild`，因此安装的文件、权限以及内置 PDFium 运行时与 `.deb` 完全一致；ELF `NEEDED` 会转成可移植的 SONAME 依赖。
- 打标签的发布任务会上传 RPM，发布页会列出下载链接，已签名的更新清单会把它记为 `rpm/system_installer` 产物。

### 🖥 桌面体验

- **更新面板按需打开：** 后台检查更新不再自动弹出标题栏面板，但仍会在标题栏入口提示，点击后才打开面板。面板改为树内图层绘制，对话框会盖在它之上，设置页也保持可读。
- **WSL 运行时安装显示进度：** 点击 **连接 WSL** 后会依次报告发行版检查、下载、安装、启动、端口转发、配对和连接等步骤，关闭对话框也会继续执行。连接会以实时状态保留在 **远程连接** 列表中，失败的尝试可以从那里重试或查看。

### 🔌 Provider 与 Agent

- **一个共享 Provider 只有一条记录，现在所有绑定的 Agent 都能看到它。** 在 Providers 页面新增或修改的 Model 会同步到每个 Agent 自己的启动行和运行时 Profile，同时每个 Agent 仍只显示自己声明的协议。删除 Model 仍只在 Providers 页面进行，那里以完整目录为依据，因此在 Agent 页面保存不会再删掉其他 Agent 仍可使用的 Model。
- 由网关支撑的路由组和远程调度端写入也会重新发布目录，因此 Provider 的修改、删除或 Key 变化都会刷新所有受影响的 Agent，包括从已配对设备发来的写入。
- **Codex 会得到投影后的模型目录。** Codex 的 `model/list` 只描述 OpenAI 自己的模型 id，因此声明了上下文窗口或推理深度的别名会报告空的档位列表，窗口也不会生效。Vibex 现在会在 Profile 的 `config.toml` 旁写入私有的 `vibex-models.json`，列出所有启用的 Model 及其声明的上下文窗口与思考档位，并让 `model_catalog_json` 指向它；两者都未声明的 Profile 保持原有输出不变。
- **托管 Agent 更新后不再留下过期目录：** 安装或回滚会改变启动指纹，只有与当前指纹一致的完整快照才会被视为有效，因此新会话不会再以 `runtime_switch_configuration_unavailable` 失败。

### 📱 移动端与远程

- **移动端会话同步流量更少：** 时间线读取会把内联附件换成有上限的标记，只在打开图片时才获取详情；`agent_session` 事件按当前打开的会话限定，应用退到后台时只保留通知；断档用有上限的追赶分页恢复，而不是重新拉取整个窗口。
- 推理分片改为逐条增量发送，不再合并成一个不断增长的实时条目；内联附件字节也会从时间线分页预算中扣除。
- **时间线分页现在能放进 Relay 帧：** 远程时间线分页被限制在 512 KiB，为 E2EE 信封在 Relay WebSocket 帧内留出余量，否则 Relay 会关闭 PC 连接，手机则会在一个已无权威端的房间里反复重试。Relay 的帧上限通过 `VIBEX_RELAY_MAX_WEBSOCKET_FRAME_BYTES`（`maxWebsocketFrameBytes`，默认 2 MiB）暴露。

### 🛠 技能与市场

- **安装不再等待整个预览：** Skill 文件夹在获取和预览期间 Install 按钮保持可用，大型 Skill 的点击会被记住，并在预览完成的瞬间开始安装。读取失败后的点击会重新读取并预约安装；当面板不再拥有该操作时，预约会被清除。写入的字节仍然完全等于预览所披露的字节。

### ⬆️ 升级

按正常方式用 0.1.4 覆盖 0.1.3 即可：会话、工作区、Provider 配置、Skills 与远程主机都会保留。Linux 上新的 RPM 可作为现有 `.deb` 之外的另一种选择；Windows 和 macOS 的桌面安装包没有变化。
