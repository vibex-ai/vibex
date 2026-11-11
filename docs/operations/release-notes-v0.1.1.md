# Vibex v0.1.1 Release Notes

- **Release Date:** 2026-11-11
- **Summary:** A follow-up to the first stable release. Remote hosts can be added back to back, the Windows taskbar and its SSH helpers behave, remote failures say what went wrong, screenshot markup keeps up with the cursor, broken pages reach the embedded browser, and `vibex-bin` actually reaches the AUR.

---

## English

### 🚀 Highlights

- **Several remote hosts at once:** adding SSH hosts one after another now provisions every one of them. Each request waits its turn instead of cancelling the download already in flight, and a host that cannot be reached no longer parks the rest of the queue.
- **`vibex-bin` on the AUR:** the stable release now hands the AUR job the tag. Arch users install the reviewed Linux package with `paru -S vibex-bin` / `yay -S vibex-bin` instead of unpacking a `.deb` by hand.
- **A labelled window on Windows:** the taskbar button now carries "Vibex" instead of a bare icon, and the shell no longer flashes a console window while it talks to a remote host.
- **Remote hosts that stay connected — and say why when they cannot:** provisioning now runs on the workbench runtime, so the session opened right after a successful SSH connect keeps working instead of failing its first read with a socket-shutdown error. A `wsl.exe` that fails reports its own message, a hung WSL read gives up in 15 seconds instead of freezing the settings window, and an authentication failure says what the server actually said.
- **Annotation that keeps up with the cursor:** boxes, lines, arrows, brush and mosaic strokes no longer blink out mid-gesture or stall the window while a 4K screenshot is being marked up, and undo history is bounded by bytes as well as by entries.
- **A Providers → Models tab that stays readable:** rows are measured, so an open inline editor pushes the rows below it instead of having their names and switches paint over its fields.
- **Pages that need a real browser:** the embedded browser reports a normal desktop user agent on Windows, so Cloudflare-style checks serve the page instead of a challenge.

### 🛠 Remote runtime

- Provisioning is a queue. A second host no longer replaces the first start task, which used to abort the running download and leave a `.part` file behind with the row stuck at **Offline / Not installed**.
- Provisioning failures — an unreachable host, an unavailable runtime configuration, a stopped task — arrive as top-level hints. The WSL dialog closes on submit, so the error used to be written into a body that was already gone.
- A stalled artifact download fails within a minute of silence instead of holding the run open for five minutes. The connection has its own 30-second budget and the stream may take as long as it needs while bytes keep arriving.
- Every SSH, `ssh-keygen` and `wsl.exe` child starts hidden. Windows used to open a console window for each of them during connect, tunnel, and host-key probing.
- A remote runtime now borrows the workbench's process-lifetime Tokio runtime instead of building one and dropping it with the provisioning worker. The backend used to outlive its reactor, so the next request's write hit a dead I/O driver and a catalogue read reported `remote_socket_write_failed` right after a successful connect. A current-thread handle is refused up front rather than hanging the run.
- A failing `wsl.exe` is reported with its own message. Listing distros now drains both pipes against a 15-second deadline and distinguishes a spawn failure, a non-zero exit with the child's stderr and exit code, and a timeout, instead of collapsing all three into "could not list distros". The read runs off the UI thread, and the dialog shows reading, failed with **Retry**, and the distro list as separate states.
- WSL errors are diagnosed as WSL. Every `wsl_*` code used to classify as unknown and was answered with the SSH cards' "install an SSH server", advice for a transport that has no SSH hop; the guidance now names `wsl --update` and `wsl -l -v`.
- An authentication failure carries the server's stderr. `EnvironmentError::message` returned only the class, so a locked key, an empty agent and a wrong `IdentityFile` all read "SSH authentication failed"; the message now travels with the same stderr tail that install and launch already reported.

### 🛠 Desktop

- The workbench, the tray-restored window and the computer-use panel are titled from the release channel's product name, so Windows taskbar buttons and the window switcher show a label.
- Screenshot annotation draws from one working image instead of encoding a PNG per pointer sample. Each move used to run two full-size codec passes, copy the picture and deep-copy the undo history on the UI thread — about 100 ms per event for a 4K screenshot — while the `img()` element had no pixels between the old image id and the new decode, which is why the picture blinked out under the cursor. A stroke now extends the working image in place, a shape redraws over the gesture baseline, at most one upload happens per frame, and a retired texture is released after the frame that drew it. Undo history is capped by a byte budget as well as by its 64 entries.
- The Providers → Models tab places its rows with GPUI's measured list. The previous fixed-height virtual list could not know the inline editor's height, which depends on the pane width, the locale and how the protocol chips wrap, so the open panel grew past its budget and the rows after it painted inside it. Opening or closing the editor re-measures the rows, the list owns the scrollbar gutter, and the card scrolls its rows rather than the page scrolling the card.

### 🛠 Embedded browser

- The Windows build reads the Chromium version from the version-named build directory next to the launcher, with no process started. The embedded browser therefore sends `Chrome/<version>` instead of `HeadlessChrome`, which bot checks used to answer with a challenge page.

### 🛠 Packaging and release

- `vibex-bin` is published from a stable GitHub Release. The publish job dispatches the AUR workflow with the tag, because an event created by the default token does not start another workflow; the dispatch is reported rather than fatal so a release is never failed over it.
- The AUR job hands the PKGBUILD tree and its `SRCDEST` to the unprivileged `builder` account before the first `makepkg` run. Both directories are write targets, and handing over only one of them failed run after run.
- The quality gate runs again. Formatting compares bytes instead of building a diff matrix that needed tens of gigabytes on `app.rs`, the macOS clippy job no longer fails on a Windows-only helper, and the MCP secret test no longer reaches the real keychain.
- The license gate ignores Gradle's build tree. A local Android build copies the registered emoji font into `app/build/`, and the scan compared that copy against the policy entry it was generated from, failing `pnpm check:licenses` on a machine that had merely built an APK. The build directory joins `jniLibs` and the iOS XCFramework as a generated root; the pinned source under `src/main/assets/fonts` stays the only counted input.

### ⚡️ Performance

- The embedded browser no longer starts a Chromium process to learn its version. Reading the build directory is instant and leaves no stray window behind on every runtime start.
- Marking up a screenshot no longer re-encodes the picture on every pointer sample, so a gesture on a 4K image stops costing roughly 100 ms of UI-thread work per move.
- Listing WSL distros no longer runs on the UI thread, so a half-installed WSL, a stopped `LxssManager` or an AppLocker prompt can no longer freeze the settings window before the dialog paints.

### ⬆️ Upgrading

Install 0.1.1 over 0.1.0 as usual: sessions, workspaces, Provider configuration and remote hosts carry over, and a provisioning run interrupted by 0.1.0 can simply be retried from the host's row.

---

## 中文

### 🚀 核心亮点

- **多台远程主机可连续添加：** 连续添加多个 SSH 主机时，每一台都会被正确部署，后来的请求排队等待，而不是把正在进行的下载中断；某台主机不可达也不会再堵住后面的队列。
- **`vibex-bin` 正式上架 AUR：** 正式版发布会把 tag 交给 AUR 任务，Arch 用户可以直接 `paru -S vibex-bin` / `yay -S vibex-bin` 安装，无需再手动解开 `.deb`。
- **Windows 任务栏显示名称：** 任务栏按钮不再只有一个图标，而是显示 "Vibex"；与远程主机通信时也不会再弹出黑色控制台窗口。
- **远程连接更稳，失败时说明原因：** 部署改为运行在工作台运行时上，SSH 连接成功后的会话不会再在第一次读取时报 socket 已关闭；`wsl.exe` 失败会带上它自己的报错，WSL 读取卡住时 15 秒即放弃而不再冻结设置窗口，认证失败也会显示服务端真正返回的信息。
- **标注跟随光标：** 画框、直线、箭头、画笔与马赛克不再在拖动过程中闪断，也不会在标注 4K 截图时卡住窗口；撤销历史同时受字节数限制。
- **供应商 → 模型页可读性修复：** 行高改为实测，展开的行内编辑面板会把后续行向下推，而不是让它们的名称与开关画在面板字段上。
- **需要真实浏览器的页面可正常打开：** Windows 上的内嵌浏览器改为上报普通桌面 User-Agent，Cloudflare 一类的校验会直接给出页面而不是验证页。

### 🛠 远程运行时

- 部署改为队列执行。添加第二台主机不会再覆盖第一次的启动任务——旧实现会中断正在下载的数据流，留下 `.part` 文件，并让该主机永远停在 **离线 / 未安装**。
- 部署失败（主机不可达、运行时配置不可用、任务意外停止）统一在顶部提示中呈现。WSL 对话框在提交后即关闭，此前的错误写进了一个已经不在屏幕上的表单。
- 下载卡住时会在静默一分钟后失败，而不是把整个部署挂满五分钟。连接本身有 30 秒预算，只要还在持续收到数据，耗时不受限制。
- 所有 `ssh`、`ssh-keygen` 与 `wsl.exe` 子进程都以隐藏方式启动。此前 Windows 在连接、隧道和主机密钥探测阶段会为每个子进程弹出控制台窗口。
- 远程运行时改为借用工作台的生命周期 Tokio 运行时，而不是自建一个并在部署任务结束时随它一起销毁。此前后端比它的 reactor 活得更久，下一次请求写入时 I/O driver 已失效，于是一次成功连接之后读取目录会报 `remote_socket_write_failed`。current-thread 运行时句柄会被直接拒绝，否则整次部署会挂住而不是报错。
- `wsl.exe` 失败时会带上它自己的信息。列出 WSL 发行版现在会在独立线程上读取两个管道，并以 15 秒为上限，分别报告启动失败、非零退出（附 stderr 与退出码）与超时，不再把三种情况都写成 "无法列出 WSL 发行版"。读取移出 UI 线程，对话框明确区分读取中、失败（可 **重试**）与发行版列表三种状态。
- WSL 错误按 WSL 归类。此前所有 `wsl_*` 错误都被当作未知错误，给出的建议是 SSH 卡片的"安装 SSH 服务器"——而 WSL 链路里根本没有 SSH。现在归类为 `SshDiagnosticKind::Wsl`，建议为 `wsl --update` / `wsl -l -v`。
- 认证失败会携带服务端 stderr。`EnvironmentError::message` 此前只返回错误类别，因此密钥被锁、agent 为空、`IdentityFile` 写错都显示为同一句"SSH 认证失败"；现在它会像 install / launch 一样带上 stderr 尾部。

### 🛠 桌面端

- 工作台窗口、从托盘恢复的窗口以及 computer-use 面板都以发布渠道的产品名作为标题，Windows 任务栏与窗口切换器因此能显示名称。
- 截图标注改为基于同一张工作图像绘制，不再为每个指针采样编码一次 PNG。此前每次移动都要在 UI 线程上跑两遍全尺寸编解码、复制整张图片，并通过状态克隆深拷贝整个撤销历史——4K 截图下每次事件约 100 ms；同时 `img()` 元素在旧图片 id 被丢弃与新解码到达之间没有像素可画，这就是光标下画面会闪断的原因。现在笔刷与马赛克原地扩展工作图像，形状基于手势起点重绘，每帧最多上传一次，退役的纹理会画完当帧后用 `Window::drop_image` 释放。撤销历史除 64 条上限外，还增加了字节预算。
- 供应商 → 模型页改用 GPUI 的实测列表摆放行。此前的定高虚拟列表无法得知行内编辑面板的真实高度——它取决于面板宽度、语言以及协议标签的换行方式——面板展开后超出预算，后续行就被摆进了面板内部。现在展开或收起编辑面板都会重新测量行高，滚动条槽位归列表所有，卡片内部滚动而不是整页滚动。

### 🛠 内嵌浏览器

- Windows 版改为从启动器旁以版本号命名的构建目录读取 Chromium 版本，不再启动进程探测。内嵌浏览器因此发送 `Chrome/<version>` 而不是 `HeadlessChrome`，后者会被反爬校验判定为无头浏览器并返回验证页。

### 🛠 打包与发布

- `vibex-bin` 由稳定的 GitHub Release 发布。发布任务会以 tag 显式触发 AUR workflow：由默认 token 产生的事件不会启动另一个 workflow；该触发失败只做记录，绝不会因此让发布失败。
- AUR 任务在第一次 `makepkg` 之前，把 PKGBUILD 目录和 `SRCDEST` 一并交给非特权的 `builder` 账户。两个目录都需要写入权限，此前只交接其中一个，导致连续多次运行失败。
- 质量门禁恢复运作。格式化检查改为比较字节，不再为 `app.rs` 构造需要数十 GB 内存的 diff 矩阵；macOS 的 clippy 任务不再因一个仅 Windows 使用的辅助函数报错；MCP 密钥测试不再访问真实钥匙串。
- 许可证检查忽略 Gradle 构建目录。本地构建 Android 包时，Gradle 会把已登记的 emoji 字体复制到 `app/build/`，扫描再拿这份副本与它生成自的策略条目比对，于是仅仅构建过 APK 的机器上 `pnpm check:licenses` 就会失败。现在 `app/build` 与 `jniLibs`、iOS XCFramework 一样作为生成目录排除；`src/main/assets/fonts` 下登记的源文件仍是唯一计入的输入。

### ⚡️ 性能

- 内嵌浏览器不再为获取版本号启动一次 Chromium。读取构建目录是瞬时的，也不会在每次运行时启动时留下一个多余的窗口。
- 截图标注不再为每个指针采样重新编码整张图片，4K 图片上的一次拖动不再消耗约 100 ms 的 UI 线程时间。
- 列出 WSL 发行版不再占用 UI 线程，半安装的 WSL、停止的 `LxssManager` 或 AppLocker 提示都不会再在对话框绘制前冻结设置窗口。

### ⬆️ 升级说明

直接覆盖安装 0.1.1 即可：会话、工作区、Provider 配置与远程主机都会保留；被 0.1.0 中断的部署，从对应主机的行重新发起即可。
