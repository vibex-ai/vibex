# Vibex v0.1.2 Release Notes

- **Release Date:** 2026-11-11
- **Summary:** A small follow-up to v0.1.1. Adding an SSH host now shows its progress the moment you click, the host that connects shows up in Remote instances, and the AUR job stops failing on a stale Arch container.

---

## English

### 🚀 Highlights

- **A new SSH host answers on the click:** the step graph opens immediately instead of appearing minutes later, once the runtime package has finished downloading.
- **A connected host appears in Remote instances:** the runtime a pairing created is listed in the panel and can be switched to, instead of being driven by the workbench while the list still shows only this device.
- **`vibex-bin` builds again:** the AUR job upgrades its Arch container in the same transaction as its prerequisites, so a rolling-release image can no longer abort the build before packaging starts.

### 🛠 Remote runtime

- Adding an SSH host publishes a provisioning placeholder before the deployment starts. A run begins with the SSH probe, the signed release manifest and a roughly 38 MB artifact download, and all three happen before the provisioning service exists. The progress dialog only rendered the service's snapshots, so that whole window showed nothing: the click looked inert and **Cancel** had nothing to act on. The dialog now opens at step 1 with a live line naming the stage, and the row's Cancel stops the download.
- The placeholder survives the handoff to the service, so the dialog does not blank between the download finishing and the worker reporting its first step, and a dialog closed during the download stays closed when the run continues.
- A paired runtime is registered before it becomes active. Pairing writes the runtime registry from the provisioning worker thread, while the workbench held the copy it read at boot; nothing reloaded it, so the new host had no row in **Remote instances** and activating it named an id the registry did not contain. The registry now reloads on install, with the credential in hand as a fallback when the pairing never reached the store.

### 🛠 Packaging and release

- The AUR job upgrades its container before installing prerequisites. It ran `pacman -Sy`, which refreshes the databases but not the installed packages, so an `archlinux:base-devel` image rebuilt on its own cadence still carried curl, pacman, sudo, libarchive and libngtcp2 built against `libcrypto.so=3-64` while pacman resolved openssl 4.1.0. The transaction aborted before any packaging step ran. The container is now upgraded in the same transaction as the install, and the shape is held under contract so a partial upgrade cannot return unnoticed.

### ⬆️ Upgrading

Install 0.1.2 over 0.1.1 as usual: sessions, workspaces, Provider configuration and remote hosts carry over. A host that is already paired keeps its runtime; open **Remote instances** to switch to it.

---

## 中文

### 🚀 核心亮点

- **添加 SSH 主机即刻响应：** 点击后步骤图立刻出现，不再等到运行时包下载完毕才姗姗来迟。
- **已连接的主机出现在远程实例列表：** 配对产生的运行时会列在面板里并可切换，不再出现工作台已经在驱动它、列表却只有本机的情况。
- **`vibex-bin` 恢复构建：** AUR 任务在安装依赖的同一事务里升级 Arch 容器，滚动更新的镜像不会再在打包开始前中断构建。

### 🛠 远程运行时

- 添加 SSH 主机时，在部署开始前就发布一个占位进度。一次部署以 SSH 环境探测、签名发布清单和约 38 MB 的运行时包下载开始，这三件事都发生在配置服务创建之前。进度对话框此前只渲染该服务的快照，于是这整段时间没有任何显示：点击看起来毫无反应，**取消**也没有可作用的对象。现在对话框在第一步就打开，并有一行实时说明当前阶段，该行的取消会真正中止下载。
- 占位进度在移交给配置服务时保留，因此下载结束到 worker 报告第一步之间对话框不会闪空；在下载期间关闭的对话框，在运行继续后也不会自己重新打开。
- 配对的运行时会先注册再激活。配对是在配置 worker 线程里写运行时注册表的，而工作台持有的是启动时读入的那份副本；此前没有任何地方重新加载它，于是新主机在**远程实例**里没有对应的行，而激活时又指定了一个注册表中并不存在的 id。现在安装路径会重新加载注册表，并在配对未写入存储时用手上的凭据兜底。

### 🛠 打包与发布

- AUR 任务在安装依赖前先升级容器。此前执行的是 `pacman -Sy`，它只刷新数据库而不升级已安装的包，因此按自身节奏重建的 `archlinux:base-devel` 镜像中，curl、pacman、sudo、libarchive 与 libngtcp2 仍依赖 `libcrypto.so=3-64`，而 pacman 解析出的是 openssl 4.1.0，事务在任何打包步骤之前就中止了。现在容器与依赖在同一事务中一起升级，并有契约检查固定这一形态，部分升级不会在无人察觉时回归。

### ⬆️ 升级说明

直接覆盖安装 0.1.2 即可：会话、工作区、Provider 配置与远程主机都会保留。已配对的主机会保留其运行时，打开**远程实例**即可切换过去。
