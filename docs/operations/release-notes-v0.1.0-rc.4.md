# Vibex v0.1.0-rc.4 Release Notes

- Released: 2026-09-17 · Range: `v0.1.0-rc.3...v0.1.0-rc.4` · 45 commits

---

## English

### Highlights

- **Ten built-in themes, with light and dark chosen separately** — Appearance is no longer a single light/dark switch. Vibex now ships ten ready-made palettes — five light (Vibex, Catppuccin Latte, Gruvbox, Solarized, and GitHub) and five dark (Vibex, Catppuccin Mocha, Gruvbox, Tokyo Night, and Nord) — any light theme can be paired with any dark one, and you can add your own theme file to the themes folder in the Vibex home directory. Every built-in theme keeps body and secondary text at a contrast ratio of at least 4.5:1, and a custom theme adjusts any colour you do not set yourself to the background it is actually drawn on (`04d582d`).
- **The window frame now follows your system** — The title bar floats over the app instead of owning a strip of its own, so the sidebar and the panels run all the way to the top edge and the right-hand rail reads as one surface. The buttons in the title bar match the platform you are on: macOS keeps its traffic lights, Windows keeps the areas Snap Layouts needs, and Linux follows your desktop's own button layout. Buttons the window cannot perform are no longer drawn, and pressing one never starts a window drag (`5a2f015`, `a72bf55`, `4035d0e`, `dd87a6f`, `18c8628`).
- **One place to manage every runtime** — Desktop and mobile now share a single Runtime Manager for browsing, adding, switching, renaming, and removing runtimes. Switching away no longer shuts down the built-in runtime, so local terminals and running Agent turns survive the round trip; a wrong address changes nothing and the failure reason is kept with that runtime. The list also tells you whether a runtime is another Vibex desktop or a headless server (one with no window of its own) before you connect (`3e72051`, `a33e67c`).
- **A rebuilt Agent, provider, and model picker** — Choosing a model in the composer is now one floating menu that behaves like the rest of the app: an Agent tab strip, a starred-models view, a star on every row, the default reasoning level marked, and search that puts the closest matches and your favourites first. The panel keeps the same height while you type or switch Agent, so it no longer jumps around under your pointer, and the search box is wider (`973e015`, `6a6380a`).
- **An Agent that fails silently now tells you** — An Agent connection could hide an internal failure (a rejected API key, for example) and end the turn with nothing at all: the answer vanished, no error appeared, and the message still counted as sent. That now ends the turn with an error and a suggestion for what to try, while a real stop, a permission prompt, or any actual activity still completes normally (`dcd01d8`).
- **Search no longer drags the app down** — Opening session search used to rebuild its index for every session and repeat the whole scan on every frame, which dropped a busy session from about 58 fps to 6-8. The index is now built once and kept warm, the scan runs in the background, and reopening the dialog is instant (`95d0a8a`).
- **Terminals stay smooth** — The terminal rebuilt every single cell as its own object on every repaint — which happens on each line of output and each cursor blink — so even an idle terminal could stall the window twice a second. It now draws the whole grid in one pass: the same frame that took about 242 ms in a debug build takes about 3 ms (`eabe83b`).
- **Android builds install as updates, at a normal size** — Every Android package carried the same internal version number, so a new APK could not replace an older install on any channel that checks versions. The number is now derived from the release version, so `0.1.0-rc.4` sorts after `0.1.0-rc.3`, and a version that cannot be encoded fails the build instead of silently reusing the old number. Release builds also stop picking up libraries left behind by earlier debug builds: an APK that had grown to 429 MB is back to about 73 MB (`cb9809f`, `8de5d4e`).
- **No more bundled editor source** — Vibex used to carry a 95 MB copy of the Zed editor's source tree to build its interface toolkit. That copy is gone: the toolkit now comes from ordinary published packages, which makes the checkout much smaller and the build easier to reproduce. The one visible consequence is that the frosted-glass effect, which depended on that copy, has been retired (`6ffcf7c`, `825bb97`, `9ecc107`).

### New Features

**Runtime & remote**

- Browse, add, switch, rename, and remove runtimes from one window on both desktop and mobile, with the list and the details of the selected runtime in one panel instead of stacked dialogs (`3e72051`)
- Keep the built-in runtime running while you are switched away, connect first so a bad address changes nothing, and keep the failure reason with each runtime (`3e72051`)
- See whether a runtime is a Vibex desktop or a headless server, known from the moment you pair and refreshed on every connection (`a33e67c`)

**Themes & appearance**

- Pick from ten built-in light and dark palettes and mix them freely, and extend the list with a theme file that names only the colours you want to change (`04d582d`)
- Hover and selection colours now come from the theme's own background, so warm or tinted themes no longer get neutral grey highlights (`04d582d`)
- The right rail — files, Git, and child-Agent timelines — now paints as a single surface (`18c8628`)

**Desktop**

- The title bar floats above the app as a 38-pixel overlay, and every column leaves room for it so nothing hides underneath (`5a2f015`)
- Title-bar buttons follow the platform's own rules, respect the GTK decoration layout on Linux, hide actions the window cannot perform, and never start a window drag when pressed (`a72bf55`)
- The Usage range switch now slides, and control outlines are softer (`37e8029`)
- Usage filters are grouped after the range switch, and each one shows the value it applies — with `+N` for further selections — instead of a count you have to open the menu to understand (`2a03753`)
- The model provider editor saves with Enter instead of closing, every field writes into the draft, and errors appear under the field that caused them (`69489d7`)
- Export MCP and Skills configuration to an Agent from their own pages, without going through the Advanced page that the main navigation cannot reach (`f7c3129`)
- A Developer settings section turns the FPS overlay on and off (`a80f3fd`), the overlay can be dragged and remembers where you put it (`b2aa073`), and its samples can be recorded as diagnostics (`8a30d6a`)
- The composer's Agent, provider, and model picker is now a proper floating menu with an Agent tab strip, a starred-models view, star toggles on each row, the default reasoning level marked, and open/close animation (`973e015`)
- Search in that picker ranks prefix matches first, then partial matches, then provider names, keeps starred models on top, remembers and reorders favourites, locks the Agent row while a switch is in flight, and returns focus to the composer when it closes (`973e015`)

**Agent & ACP**

- The zcode adapter is updated from 0.17.2 to 0.37.1 (`d88327f`)
- Approval prompts for zcode now show the readable description of the tool call and the files it will touch, and the adapter always runs on Node (`d88327f`)
- Cursor now publishes one option per model parameter instead of freezing every combination into its own model id, so models are listed by name and `fast`, `context`, and reasoning depth are chosen as options on the model you picked (`8ab812f`)
- Reasoning depth stays settable during a session, a boolean thinking toggle appears under Session options instead of being read as a depth level, and a model saved under an old variant id (`claude-opus-5[thinking=true,context=300k]`) collapses to its model name once the CLI proves it uses the new shape (`8ab812f`)

### Fixes

**Desktop**

- Restore the icons that were silently missing: thirty-four icon paths drew nothing — thirty-two were never registered (sidebar project logos, file and folder actions, file-type icons, and the drag handle) and two pointed at the wrong folder (`2b37048`)
- The runtime button now shows its icon, and its panel stays open when you click it (`de067b9`)
- A paired phone always receives its own sidebar layout, instead of the layout of the remote runtime the desktop happens to be showing (`444226b`)
- Collapsing a card or merging process rows no longer leaves a blank strip under the last row that the view stays parked on (`3e59958`)
- The macOS Dock icon now matches the size of its neighbours, and clicking the Dock icon reopens the window (`a5af0ee`)
- In the provider editor, connection fields come before models, in the order you fill them in (`232e8b6`)
- The Usage toolbar and chart headers use the standard small control size instead of a cramped one (`c98f0f4`)
- The provider and model menu keeps one height for a given window size, so it no longer resizes while you type or switch Agent; the list scrolls inside it and the search box gets the width back (`6a6380a`)
- Switching sessions no longer nudges the conversation: the timeline keeps its measured row heights and remembers which turn you were reading, so a reader who scrolled up stays exactly where they were (`5785cdb`)
- Unselected session rows in the sidebar are easier to read and now match the project and workspace rows (`fff3271`)

**Agent & ACP**

- A completely silent turn now fails instead of saving an empty answer; turns that are waiting for input, stopped by you, or doing real work are unaffected (`dcd01d8`)
- Cline's address is fixed to `api.openai.com`: the Agent owns that endpoint and ignores whatever address a profile carries, and the preview shows the address it will actually call (`4285157`)

**Mobile & build**

- Android builds now pass the NDK API level, so the Android build, package, and tagged-release steps no longer fail while linking the app (`50a03f9`)
- Android version numbers are derived from the release version, so a new build installs over an older one; a version that cannot be encoded, or a prerelease number past 99, fails the build (`cb9809f`)
- Release builds clear out libraries left behind by earlier debug builds, so a release APK no longer ships debug libraries (`8de5d4e`)
- Swiping a drawer open no longer scrolls the page it reveals, and the flick that follows the swipe no longer carries into the drawer's list (`2609443`)
- Tapping a field that already has focus brings the keyboard back after you dismissed it, on both Android and iOS; tapping a button next to a field does not open the keyboard, and the tap does nothing while the keyboard is already up (`fe7f843`)

### Performance

- Session search keeps its index between openings, scans in the background instead of while drawing, and matches text in place (`95d0a8a`)
- The terminal draws its whole grid as one element and handles pointer input with a single listener (`eabe83b`)

### Under the hood

- Vibex no longer vendors the Zed source tree: the interface toolkit comes from published packages, the small unpublished bridge it needed now lives in this repository, the bundled fonts and mobile icons moved out of the old copy, and the mobile platform layer, keyboard handling, and iOS entry point were rebuilt on the published package (`6ffcf7c`)
- The interface toolkit family is pinned to a specific revision, with a note in the specs about when that pin may move (`825bb97`, `9ecc107`, `e1d56bc`)
- The build and licence checks were updated for the new dependencies and assets, the list of bundled software (SBOM), third-party notices, and licence policy were regenerated, and the release packaging, platform support, UI boundary, and licence documents were refreshed (`6ffcf7c`, `04d582d`, `a5af0ee`)
- The project's specs were updated for the window chrome, the Usage toolbar, silent Agent turns, the dependency source, terminal drawing, and Cursor's parameterized model picker (`2a03753`, `dcd01d8`, `e1d56bc`, `eabe83b`, `8ab812f`)
- A code-quality check that failed on the mobile scroll listener no longer breaks the Rust quality gate or CI (`45709c8`)
- The Android release job no longer dies before it builds anything: the SDK setup action still asked for the legacy `tools` package, which Google no longer publishes, and it is now pinned to the version that dropped it (`b870998`)
- Version numbers were bumped to `0.1.0-rc.4` across the workspace and the packaging inputs (`4dd6ce6`)

---

## 中文

### 亮点

- **内置十套主题，明暗可以分开选** — 外观不再只是一个「浅色／深色」开关。Vibex 现在自带十套配色：五套浅色（Vibex、Catppuccin Latte、Gruvbox、Solarized、GitHub）和五套深色（Vibex、Catppuccin Mocha、Gruvbox、Tokyo Night、Nord）；任意浅色都能和任意深色搭配，你也可以在 Vibex 主目录的 themes 文件夹里放自己的主题文件来扩充。所有内置主题的正文与次要文字对比度都不低于 4.5:1；自定义主题里没有指定的颜色，会自动按它实际所在的背景调整（`04d582d`）。
- **窗口边框跟随你的系统** — 标题栏改为浮在界面之上，不再单独占一条，因此侧栏和各个面板一直延伸到窗口顶边，右侧栏看起来也是完整的一块。标题栏按钮与你所在的平台一致：macOS 保留红黄绿交通灯，Windows 保留贴靠布局需要的区域，Linux 跟随你桌面自己的按钮布局。窗口做不到的按钮不再显示，按按钮也不会误触发拖动窗口（`5a2f015`、`a72bf55`、`4035d0e`、`dd87a6f`、`18c8628`）。
- **所有运行时集中在一处管理** — 桌面端和手机端现在共用同一个运行时管理器，用来浏览、添加、切换、重命名和删除运行时。切走时不再关掉内置运行时，本地终端和正在跑的 Agent 回合都能保留；地址填错什么都不会变，失败原因会记在对应的运行时上。列表还会在连接之前就告诉你，对方是另一台 Vibex 桌面端还是无界面的服务器（`3e72051`、`a33e67c`）。
- **重做的 Agent／服务商／模型选择器** — 在输入框里选模型现在是一个和全局一致的浮动菜单：顶部是 Agent 标签条，有「已加星」视图，每行都有星标，默认推理档位有标记，搜索会把最贴近的结果和你收藏的模型排在前面。输入或切换 Agent 时面板高度保持不变，不会在鼠标底下跳来跳去，搜索框也更宽了（`973e015`、`6a6380a`）。
- **Agent 静默失败时现在会告诉你** — 某些 Agent 适配器会把内部错误（比如 API key 被拒）吞掉，然后什么都不回就结束回合：回答凭空消失、没有任何报错，消息却算作已发送。现在这种情况会以错误结束，并提示可以怎么处理；而真正的停止、权限确认或确实有内容产生的回合不受影响（`dcd01d8`）。
- **搜索不再拖慢整个应用** — 以前打开会话搜索会为每个会话重建索引，而且对话框开着的时候每一帧都重跑一遍，繁忙会话会从约 58 fps 掉到 6-8。现在索引只建一次并保持可用，扫描放到后台，再次打开对话框是瞬间完成的（`95d0a8a`）。
- **终端保持流畅** — 终端以前每次重绘都要把每个单元格重新构造成一个对象，而输出每一行、光标每闪一次都会重绘，所以连空闲终端都可能每秒卡住窗口两次。现在整个网格一次画完：同一帧在 debug 构建下从约 242 ms 降到约 3 ms（`eabe83b`）。
- **Android 安装包可以正常升级，体积也恢复正常** — 以前每个 Android 安装包的内部版本号都一样，凡是会检查版本的渠道，新 APK 都装不上旧版本。现在版本号由发布版本推导，`0.1.0-rc.4` 排在 `0.1.0-rc.3` 之后；无法编码的版本会让构建直接失败，而不是悄悄沿用旧号。发布构建也不再带上早先 debug 构建遗留的库：曾涨到 429 MB 的安装包回到约 73 MB（`cb9809f`、`8de5d4e`）。
- **不再内置编辑器源码** — Vibex 过去为了构建界面工具包，要带上一份 95 MB 的 Zed 编辑器源码。这份副本已经移除，工具包改为使用正常发布的软件包，代码检出小了很多，构建也更容易复现。唯一看得见的变化是：依赖这份副本的毛玻璃效果已随之下线（`6ffcf7c`、`825bb97`、`9ecc107`）。

### 新功能

**运行时与远程**

- 桌面端和手机端都能在同一个窗口里浏览、添加、切换、重命名和删除运行时；列表与所选运行时的详情放在同一个面板里，不再层层叠叠弹窗（`3e72051`）
- 切走时保持内置运行时继续运行；先连接成功再切换，地址错误则一切照旧；每个运行时单独记录失败原因（`3e72051`）
- 一眼看出某个运行时是 Vibex 桌面端还是无界面服务器，配对时即已知，并在每次连接时刷新（`a33e67c`）

**主题与外观**

- 十套内置明暗配色任选，明暗可自由搭配；也可以只写想改的颜色，用自己的主题文件扩充（`04d582d`）
- 悬停和选中颜色改为取自主题自身的背景，暖色或带色调的主题不再配到中性灰的高亮（`04d582d`）
- 右侧栏（文件、Git、子 Agent 时间线）现在整体画成一块（`18c8628`）

**桌面端**

- 标题栏以 38 像素浮层浮在界面上方，每一列都为它留出空间，内容不会被压住（`5a2f015`）
- 标题栏按钮遵循各平台自己的规则，在 Linux 上遵循 GTK 的装饰布局，窗口做不到的操作不再显示，按下按钮也不会误触发拖动窗口（`a72bf55`）
- 用量页的范围开关改为滑动切换，控件描边更柔和（`37e8029`）
- 用量页的筛选器归到范围开关之后成组排列，每个直接显示它筛选的值（多选时显示 `+N`），不用再打开菜单才知道筛了什么（`2a03753`）
- 模型服务商编辑器改为回车保存而不关闭对话框，所有字段都会写入草稿，错误显示在出问题的字段下方（`69489d7`）
- MCP 与 Skills 的配置导出移到各自页面，不必再绕进主导航到不了的 Advanced 页（`f7c3129`）
- 新增开发者设置分区，可开关帧率浮层（`a80f3fd`）；浮层可拖动并记住位置（`b2aa073`）；采样可作为诊断数据记录（`8a30d6a`）
- 输入框的 Agent／服务商／模型选择器改为真正的浮动菜单：Agent 标签条、「已加星」视图、每行的星标开关、标出默认推理档位，并带打开／关闭动画（`973e015`）
- 选择器搜索先按开头匹配、再按包含匹配、最后按服务商排序，已加星模型置顶；收藏可保存并重排列表；切换进行中锁定 Agent 行；关闭后焦点回到输入框（`973e015`）

**Agent 与 ACP**

- zcode 适配器从 0.17.2 升级到 0.37.1（`d88327f`）
- zcode 的审批提示现在会显示工具调用的可读说明和将要改动的文件，适配器固定使用 Node 运行（`d88327f`）
- Cursor 现在按模型参数逐个发布选项，不再把每种参数组合冻结成单独的模型 id：模型按名称列出，`fast`、`context` 和推理深度作为所选模型上的选项来设置（`8ab812f`）
- 推理深度在会话中仍可随时调整；布尔型的 thinking 开关归入会话选项，不再被当成一个深度档位；用旧变体 id 保存的模型（`claude-opus-5[thinking=true,context=300k]`）在确认 CLI 已使用新形态后，会自动收敛为模型名（`8ab812f`）

### 修复

**桌面端**

- 补齐此前悄悄缺失的图标：共 34 个图标路径画不出东西，其中 32 个从未注册（侧栏项目图标、文件与文件夹操作、文件类型图标、拖动手柄），另有 2 个指向了错误的目录（`2b37048`）
- 运行时按钮现在会显示自己的图标，点击后其面板保持打开（`de067b9`）
- 配对的手机会始终拿到自己的侧栏布局，而不是桌面端当时正在显示的远程运行时的布局（`444226b`）
- 折叠卡片或合并流程行之后，最后一行下方不再留下一条空白带、让视图停在那里（`3e59958`）
- macOS 的 Dock 图标大小现在与旁边的图标一致，点击 Dock 图标也能重新打开窗口（`a5af0ee`）
- 服务商编辑器里连接字段排在模型之前，与你填写时的顺序一致（`232e8b6`）
- 用量页工具栏和图表标题改用标准的小尺寸控件，不再局促（`c98f0f4`）
- 服务商与模型菜单在同一窗口尺寸下保持固定高度，输入或切换 Agent 时不再改变大小；列表在内部滚动，搜索框也拿回了宽度（`6a6380a`）
- 切换会话不再让对话跳动：时间线保留已测量的行高，并记住你正在看的那一轮，向上翻看的读者会停在原处（`5785cdb`）
- 侧栏未选中会话行更易读，与项目和 workspace 行保持一致（`fff3271`）

**Agent 与 ACP**

- 完全没有输出的回合现在会失败，不再保存一条空回答；等待输入、被你停止或确实有内容产生的回合不受影响（`dcd01d8`）
- Cline 的地址固定为 `api.openai.com`：该端点由 Agent 自己掌握，配置里填的地址不再起作用，预览中会显示它实际调用的地址（`4285157`）

**移动端与构建**

- Android 构建现在会传入 NDK API 级别，构建、打包和打标签发布流程不再在链接环节报错（`50a03f9`）
- Android 版本号由发布版本推导，新构建可以覆盖安装旧版本；无法编码的版本、或预发布序号超过 99 时构建直接失败（`cb9809f`）
- 发布构建会先清掉早先 debug 构建遗留的库，安装包不再夹带 debug 库（`8de5d4e`）
- 滑开抽屉不再顺带滚动它展开的页面，滑动之后手指带起的惯性也不会传到抽屉列表里（`2609443`）
- 键盘收起后，点击仍然聚焦的输入框可以重新唤出键盘（Android 与 iOS 均如此）；点击输入框旁边的按钮不会弹出键盘，键盘已经在屏幕上时点击也不会重复唤起（`fe7f843`）

### 性能

- 会话搜索在对话框关闭后保留索引，扫描放在后台而不是绘制时进行，文本就地匹配（`95d0a8a`）
- 终端把整个网格作为一个元素绘制，指针输入也收敛为单个监听器（`eabe83b`）

### 底层改动

- 不再内置 Zed 源码：界面工具包改用已发布的软件包，其中一小段未发布的桥接代码移入本仓库；随包字体和移动端图标迁出旧副本；移动端平台层、键盘处理和 iOS 入口都重建在已发布的软件包之上（`6ffcf7c`）
- 界面工具包系列固定到某个具体修订，并在项目文档（spec）中写明该固定点何时可以前移（`825bb97`、`9ecc107`、`e1d56bc`）
- 构建与许可证检查适配了新的依赖和资源，重新生成软件物料清单（SBOM）、第三方声明与许可证策略，并更新发布打包、平台支持、UI 边界与许可证文档（`6ffcf7c`、`04d582d`、`a5af0ee`）
- 项目文档（spec）更新了窗口边框、用量工具栏、静默 Agent 回合、依赖来源、终端绘制与 Cursor 参数化模型选择器相关内容（`2a03753`、`dcd01d8`、`e1d56bc`、`eabe83b`、`8ab812f`）
- 移动端滚动监听器的一处代码检查失败不再影响 Rust 质量门禁和 CI（`45709c8`）
- Android 发布任务不再在构建开始前就中断：SDK 初始化动作仍在请求 Google 已停止发布的旧 `tools` 包，现已固定到去掉该包的版本（`b870998`）
- 工作区与打包输入的版本号统一升到 `0.1.0-rc.4`（`4dd6ce6`）
