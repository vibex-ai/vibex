# Vibex v0.1.1 Release Notes

- **Release Date:** 2026-11-10
- **Summary:** A follow-up to the first stable release. Remote hosts can be added back to back, the Windows taskbar and its SSH helpers behave, broken pages reach the embedded browser, and `vibex-bin` actually reaches the AUR.

---

## English

### 🚀 Highlights

- **Several remote hosts at once:** adding SSH hosts one after another now provisions every one of them. Each request waits its turn instead of cancelling the download already in flight, and a host that cannot be reached no longer parks the rest of the queue.
- **`vibex-bin` on the AUR:** the stable release now hands the AUR job the tag. Arch users install the reviewed Linux package with `paru -S vibex-bin` / `yay -S vibex-bin` instead of unpacking a `.deb` by hand.
- **A labelled window on Windows:** the taskbar button now carries "Vibex" instead of a bare icon, and the shell no longer flashes a console window while it talks to a remote host.
- **Pages that need a real browser:** the embedded browser reports a normal desktop user agent on Windows, so Cloudflare-style checks serve the page instead of a challenge.

### 🛠 Remote runtime

- Provisioning is a queue. A second host no longer replaces the first start task, which used to abort the running download and leave a `.part` file behind with the row stuck at **Offline / Not installed**.
- Provisioning failures — an unreachable host, an unavailable runtime configuration, a stopped task — arrive as top-level hints. The WSL dialog closes on submit, so the error used to be written into a body that was already gone.
- A stalled artifact download fails within a minute of silence instead of holding the run open for five minutes. The connection has its own 30-second budget and the stream may take as long as it needs while bytes keep arriving.
- Every SSH, `ssh-keygen` and `wsl.exe` child starts hidden. Windows used to open a console window for each of them during connect, tunnel, and host-key probing.

### 🛠 Desktop

- The workbench, the tray-restored window and the computer-use panel are titled from the release channel's product name, so Windows taskbar buttons and the window switcher show a label.

### 🛠 Embedded browser

- The Windows build reads the Chromium version from the version-named build directory next to the launcher, with no process started. The embedded browser therefore sends `Chrome/<version>` instead of `HeadlessChrome`, which bot checks used to answer with a challenge page.

### 🛠 Packaging and release

- `vibex-bin` is published from a stable GitHub Release. The publish job dispatches the AUR workflow with the tag, because an event created by the default token does not start another workflow; the dispatch is reported rather than fatal so a release is never failed over it.
- The AUR job hands the PKGBUILD tree and its `SRCDEST` to the unprivileged `builder` account before the first `makepkg` run. Both directories are write targets, and handing over only one of them failed run after run.
- The quality gate runs again. Formatting compares bytes instead of building a diff matrix that needed tens of gigabytes on `app.rs`, the macOS clippy job no longer fails on a Windows-only helper, and the MCP secret test no longer reaches the real keychain.

### ⚡️ Performance

- The embedded browser no longer starts a Chromium process to learn its version. Reading the build directory is instant and leaves no stray window behind on every runtime start.

### ⬆️ Upgrading

Install 0.1.1 over 0.1.0 as usual: sessions, workspaces, Provider configuration and remote hosts carry over, and a provisioning run interrupted by 0.1.0 can simply be retried from the host's row.

---

## 中文

### 🚀 核心亮点

- **多台远程主机可连续添加：** 连续添加多个 SSH 主机时，每一台都会被正确部署，后来的请求排队等待，而不是把正在进行的下载中断；某台主机不可达也不会再堵住后面的队列。
- **`vibex-bin` 正式上架 AUR：** 正式版发布会把 tag 交给 AUR 任务，Arch 用户可以直接 `paru -S vibex-bin` / `yay -S vibex-bin` 安装，无需再手动解开 `.deb`。
- **Windows 任务栏显示名称：** 任务栏按钮不再只有一个图标，而是显示 "Vibex"；与远程主机通信时也不会再弹出黑色控制台窗口。
- **需要真实浏览器的页面可正常打开：** Windows 上的内嵌浏览器改为上报普通桌面 User-Agent，Cloudflare 一类的校验会直接给出页面而不是验证页。

### 🛠 远程运行时

- 部署改为队列执行。添加第二台主机不会再覆盖第一次的启动任务——旧实现会中断正在下载的数据流，留下 `.part` 文件，并让该主机永远停在 **离线 / 未安装**。
- 部署失败（主机不可达、运行时配置不可用、任务意外停止）统一在顶部提示中呈现。WSL 对话框在提交后即关闭，此前的错误写进了一个已经不在屏幕上的表单。
- 下载卡住时会在静默一分钟后失败，而不是把整个部署挂满五分钟。连接本身有 30 秒预算，只要还在持续收到数据，耗时不受限制。
- 所有 `ssh`、`ssh-keygen` 与 `wsl.exe` 子进程都以隐藏方式启动。此前 Windows 在连接、隧道和主机密钥探测阶段会为每个子进程弹出控制台窗口。

### 🛠 桌面端

- 工作台窗口、从托盘恢复的窗口以及 computer-use 面板都以发布渠道的产品名作为标题，Windows 任务栏与窗口切换器因此能显示名称。

### 🛠 内嵌浏览器

- Windows 版改为从启动器旁以版本号命名的构建目录读取 Chromium 版本，不再启动进程探测。内嵌浏览器因此发送 `Chrome/<version>` 而不是 `HeadlessChrome`，后者会被反爬校验判定为无头浏览器并返回验证页。

### 🛠 打包与发布

- `vibex-bin` 由稳定的 GitHub Release 发布。发布任务会以 tag 显式触发 AUR workflow：由默认 token 产生的事件不会启动另一个 workflow；该触发失败只做记录，绝不会因此让发布失败。
- AUR 任务在第一次 `makepkg` 之前，把 PKGBUILD 目录和 `SRCDEST` 一并交给非特权的 `builder` 账户。两个目录都需要写入权限，此前只交接其中一个，导致连续多次运行失败。
- 质量门禁恢复运作。格式化检查改为比较字节，不再为 `app.rs` 构造需要数十 GB 内存的 diff 矩阵；macOS 的 clippy 任务不再因一个仅 Windows 使用的辅助函数报错；MCP 密钥测试不再访问真实钥匙串。

### ⚡️ 性能

- 内嵌浏览器不再为获取版本号启动一次 Chromium。读取构建目录是瞬时的，也不会在每次运行时启动时留下一个多余的窗口。

### ⬆️ 升级说明

直接覆盖安装 0.1.1 即可：会话、工作区、Provider 配置与远程主机都会保留；被 0.1.0 中断的部署，从对应主机的行重新发起即可。
