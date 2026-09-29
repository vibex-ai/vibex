# Vibex v0.1.0-rc.7 Release Notes

- Released: 2026-09-29 · Range: `v0.1.0-rc.6...v0.1.0-rc.7` · 92 commits

---

## English

### Highlights

- **A browser in the workbench, and your Agent can drive it** — A browser panel sits beside the terminal and the preview, backed by the Chrome you already have. You watch the page live while the Agent works in the same tab, so what it acts on and what you see never drift apart.
- **Jump between the page and the code** — Alt+click an element to open the source file at the line that rendered it, and Alt+click a source line to highlight that element in the page. Both directions tell you why when they cannot answer.
- **Browser tools reach almost every Agent** — The built-in browser server is delivered to every Agent that can host it, and each Agent's row states what it received or why it received nothing.
- **The preview opens files from anywhere** — Files outside the project open read-only, and the tab menu's file browser now picks files as well as folders.
- **Prompts become a first-class object** — Prompts gets its own config center tab with a list and a single editor, and an enabled Prompt appears in the composer as a quick phrase.
- **The Skill market reads ClawHub** — Browse the registry's own rankings, and install a Skill as the whole folder it is, references and scripts included.
- **Faster and quieter** — Idle repaints are cut across the sidebar, the timers and the hover effects, and a streamed turn stops paying a database round trip for every chunk.
- **The GPUI kit moves to 0.7.0** — Dialogs, sheets and notifications are owned by the root layer, and a picked reference becomes one atomic token in the composer.

### New Features

**Embedded browser**

- Open the browser from the right rail, from the editor header beside the terminal entry, or from the tab strip's "+" menu; it is a tool panel in the workbench, not a second application window (`8b7b7e9`, `e144dad`, `75a01fd`)
- Full navigation: back, forward and reload buttons that disable when a tab has nowhere to go, the mouse's side buttons, and a right-click page menu with back, forward, reload, copy, paste and select all (`f625d8d`, `171c446`)
- Tabs behave the way a browser's do: a page that opens a tab gets one of its own, a tab the page closes disappears with it, and a tab the Agent opened is marked as the Agent's while it drives (`b525944`, `66679a6`, `5fd55b6`)
- Each tab shows the page's own title with a loading spinner, plus the site's icon and a width limit so a long title cannot crowd the strip (`66679a6`, `e1e8028`)
- Your browser tabs come back after a restart, and if one cannot be restored the panel says why on the page instead of showing a blank tab (`ade29a4`)
- Typing, clicking, scrolling and shortcuts all reach the page, including Ctrl/Cmd+C/X/V, F5 and Ctrl+R to reload, and Ctrl+L to focus the address bar (`5a4684a`, `ac451cf`, `a8da112`)
- A dropdown on the page opens the panel's own menu, and a page asking for a file opens the workbench's file browser and hands your choice back to the page (`112359b`, `9a52f05`)
- Find in page highlights every hit and scrolls to the active one, and the pointer mirrors the cursor of whatever element it is over, so a link looks like a link before you click it (`aa33e67`)
- Downloads are denied until you allow them, then save under a safe name that never overwrites, with a progress popup, a button that opens the folder, and one announcement per save (`aa33e67`, `c9f1915`, `9a52f05`)
- An HD toggle switches the live view to lossless images for close inspection, and your choice is remembered (`e06d4a7`)
- The panel's activity button shows the Agent's browser operations as they happen, and a banner warns you while a recording is keeping raw form values in memory (`1f0cb32`, `a96f3da`)
- A dev server printed by any terminal tab is detected and offered as a notification instead of navigating you away, and browser settings let you choose the new-tab page and the address-bar search engine (`1c7c1cf`, `df03fcf`)
- Pausing is always your explicit act: a page the Agent is driving shows a play/pause button, the tab strip turns green while it drives and muted once paused, and clicking, scrolling or typing never pauses it for you (`197fef5`, `2e6e35a`)

**Agent browser tools**

- The built-in browser server reaches Agents over the local endpoint or a sidecar, including Agents whose CLI reads its own MCP file; `pi` and `factory-droid` receive nothing and say so instead of failing quietly (`b0f6fb2`, `4dc4a90`)
- All 31 tools keep their coarse, fine and visual tiers, and anything read from a page is fenced off as untrusted content rather than trusted on the strength of a notice (`c81194a`)
- Pages inside cross-origin frames can be read too, so an embedded widget is not a blind spot (`9cd20d0`)
- Uploading a file or opening a local file in the browser asks you first, with the file names on the card and no "always allow", because being inside the project is not consent to send a file out (`9cd20d0`)
- Navigation is gated everywhere: a new tab obeys the same policy as an address you type, a one-off approval is not remembered, and a tab in use cannot be closed underneath the Agent by cleanup (`6cde975`)

**Browser and code**

- Alt+click in the page opens the element's source file at the reported line, and the click never reaches the page; when there is no answer the panel says why inline (`ae3e82f`)
- Alt+click on a source line highlights and scrolls to the element that line rendered, using the same highlight an Agent's action draws (`6aec9f4`)

**Preview and files**

- Files outside the project open in the preview panel read-only: the client reads them directly, a save is refused with an explanation, and the tab keeps showing you where the file really lives (`904c7be`, `3d83253`)
- The tab menu's file browser now picks files as well as folders — a row click selects instead of descending, and the confirmed path opens in the preview (`d41d783`)

**Config center**

- Prompts is a primary tab with a searchable list, one card per record, and a single editor for the selected Prompt; the list previews the Prompt's text instead of repeating its type, and leaving with unsaved edits asks before discarding them (`4d203b5`, `37a2fb7`)
- Enabled Prompts appear in the composer's `/` popup as quick phrases, inserted exactly as written and ordered by how often you use them, then by most recently edited (`b1e0d21`)
- The quick-phrase popup sizes itself to its rows, previews the text it would insert, and keeps a long name from collapsing that preview (`8c3dadb`)
- The MCP market pages the whole registry rather than the first window it fetched, and the transport filter narrows the results before the page is cut, so a filtered view is never mistaken for the whole catalog (`5f575e8`)
- The management sidebar's resource rows share one selected treatment, the result summary reads at body size, and a card description that must be cut ends with an ellipsis instead of mid-word (`f467d7d`)

**Skills**

- The Skill market reads ClawHub with the registry's own rankings and paging, so browsing no longer needs a stand-in search and the catalog can actually page (`3d95486`)
- A Skill installs as a whole folder — the instructions plus the references, scripts and templates they call — unpacked for your review before it is installed, confined to its own folder, and reported per file when something is refused (`3d95486`)
- One install reaches Claude, Codex, Cursor, Gemini, OpenCode and the default source through the native Skill export (`3d95486`)

**Agent management**

- A managed Agent runtime is pinned to a stable entry point, so restarting an Agent no longer rewrites your durable configuration or invalidates the sessions bound to it (`64f3c7b`)
- A cosmetic edit to a provider profile no longer invalidates its session bindings, while a real change to an endpoint, credential or command still rebuilds them as it must (`eeb55ac`)
- OpenCode gains an auto-accept mode you can pick per session, which also stops workspace-external reads from prompting every time (`af8a9fa`)
- A Codex session starts on the profile model you selected, so choosing another model restarts cleanly instead of leaving the process on the old one (`b5dc05d`)
- Claude profile models stay resolvable without editing your own Claude configuration (`9d71fcc`)
- A queued message survives local write contention instead of reporting that it was not sent (`8530dd5`)

**Composer**

- A `/command`, `@file` or `$skill` you pick from the suggestion menu becomes one unit: the caret steps over it, one backspace deletes it whole, and clicking a file token opens that file — while the text sent to the Agent stays exactly what you see (`3f47e3c`)

**Workbench and settings**

- Find follows focus: Ctrl+F goes to the focused editor, then to the focused browser panel, and only a conversation that really holds the keyboard opens session search (`d793b4f`, `9a52f05`)
- Dragging a session moves every row you had selected, including the current one, so what is highlighted and what moves are always the same rows (`b699a38`, `7b0c3e9`)
- Settings rows decide stacked or inline from the page width, the value column stays shrinkable with a floor for the label, and long values wrap cleanly instead of shrinking to a character per line (`68f8be5`)
- The developer FPS HUD opens centred in the window every time, is dragged as an offset from that middle, and never leaves the window (`87a2268`, `4b52031`)
- The update entry and the About page were repaired: the icon uses the success colour, opening About from the update panel no longer trips over itself, and the release notes scroll without moving the panel behind them (`7581dff`)
- The editor keeps your selection visible while a context menu, a dialog or another pane holds the keyboard (`a6c62ec`)
- Turning off "pause animation when inactive" now really keeps a backgrounded workbench animating, including the sidebar spinners, the terminal cursor and the turn timer (`beacd98`)
- The Chinese interface now calls the composer's `$` trigger Skill rather than 技能, consistently across the composer, the sidebar and the market (`1f51aa9`, `5f575e8`)

### Fixes

- Open a tab on Windows: a leftover browser endpoint can no longer point at a dead browser, and the installed Chrome is no longer probed on every launch (`a7a7289`)
- Fix the first click on the browser entry crashing, and keep the live view running instead of freezing on the last frame when a stream ends (`9c9a00c`, `90ab6f8`)
- Drop an inherited proxy setting that made every page fail to load, and keep the address you asked for when a page fails so the tab does not sit on an internal error page URL (`5d7d9cb`, `1f66568`)
- Deliver the panel the events it was dropping — page dialogs, file choosers and availability changes — let only deliberate input take over from the Agent, and deny a stopped session's approval request immediately instead of waiting out its timeout (`2e6e35a`, `3079c7d`)
- Announce each download once, fix the panel's Space, Enter, Ctrl+X and download feedback, and make dragging select text the way you expect (`c9f1915`, `9a52f05`, `65c8ab2`)
- Remove the blocking risk notice that made the browser unusable, and drop the inspector and element picker that could not attach safely (`93ab323`, `228ec67`)
- Keep the market's pager on screen while the cards scroll under it, give the unscrolled pane its height back, and repair the About page's release facts (`0c0d534`, `e638395`, `7581dff`)
- Size setting rows by the page and wrap long values, keep self-driven animations running when the inactive pause is off, and keep the market's import form scrolling on its own (`68f8be5`, `beacd98`)
- A remote panel now reports itself degraded and says that these display choices configure nothing on the machine actually running the page, instead of failing silently (`6a7d58f`)

### Performance

- Cut idle repaints: hover effects, sidebar status spinners, the turn timer and locale lookups no longer redraw the window when nothing changed, and the FPS HUD draws behind a cache boundary (`80f4b4d`)
- Stop paying a database round trip, a commit and a poll for every streamed chunk, and scale the shared runtime with the host it runs on (`e934ec0`)
- Back off instead of burning frames in the background: the selected session polls less while the window is inactive, and token usage is read off the interface thread (`80f4b4d`)

### Under the hood

- Version numbers were bumped to `0.1.0-rc.7` across the workspace, the packaging inputs, the Android version fallback, the README and deployment documentation, and the reviewed asset bundles in the license policy
- The workspace moved to the published `gpui-kit` 0.7.0 and `gpui-pre` 0.3.7, and the license SBOM and notices were regenerated for the new dependency graph

## 中文

### 亮点

- **工作台里内置浏览器，而且 Agent 能驱动它** — 浏览器面板与终端、预览并列，用的是你已经装好的 Chrome。页面实时可见，Agent 在同一个标签里操作，因此它操作的对象和你看到的内容不会脱节。
- **在页面与代码之间来回跳转** — Alt+点击页面元素，打开渲染它的源文件并定位到行；Alt+点击源码行，在页面中高亮该行渲染出的元素。答不出来时，两个方向都会说明原因。
- **浏览器工具几乎能到达每个 Agent** — 内置浏览器服务会投递给每一个能承载它的 Agent，每个 Agent 的行都会说明它拿到了什么，或者为什么什么都没拿到。
- **预览可以打开任意位置的文件** — 项目外的文件以只读方式打开，标签菜单的文件浏览器现在也能选文件，而不只是文件夹。
- **Prompt 成为一等对象** — Prompt 有了自己的配置中心标签，左侧列表、右侧单一编辑器；启用的 Prompt 会作为快捷短语出现在输入框中。
- **技能市场改读 ClawHub** — 可以直接浏览注册表自己的排名，技能以完整文件夹安装，其中的引用、脚本一并带上。
- **更快、更安静** — 侧栏、计时器和悬停效果带来的空闲重绘被削减，流式回合不再为每个分块付出一次数据库往返。
- **GPUI 套件升到 0.7.0** — 对话框、面板和通知层改由根层拥有，选中的引用在输入框中成为一个原子化的整体。

### 新功能

**内嵌浏览器**

- 可从右栏、编辑器头部终端入口旁，或标签栏的「+」菜单打开浏览器；它是工作台里的一个工具面板，而不是第二个应用窗口（`8b7b7e9`、`e144dad`、`75a01fd`）
- 完整导航：前进、后退和刷新按钮在无处可去时禁用，鼠标侧键可用，右键页面菜单提供前进、后退、刷新、复制、粘贴和全选（`f625d8d`、`171c446`）
- 标签的行为和浏览器一致：页面自己打开的标签会新建一个，页面关闭的标签随之消失，Agent 打开的标签在它驱动期间带专属标记（`b525944`、`66679a6`、`5fd55b6`）
- 每个标签显示页面自己的标题和加载指示器，还有站点图标，以及宽度上限，长标题不会挤占整条标签栏（`66679a6`、`e1e8028`）
- 浏览器标签在重启后恢复；恢复不了的标签会在页面上说明原因，而不是留一个空白标签（`ade29a4`）
- 输入、点击、滚动和快捷键都能到达页面，包括 Ctrl/Cmd+C/X/V、F5 与 Ctrl+R 刷新、Ctrl+L 聚焦地址栏（`5a4684a`、`ac451cf`、`a8da112`）
- 页面上的下拉框会打开面板自己的菜单；页面请求文件时会打开工作台的文件浏览器，并把你确认的选择交回页面（`112359b`、`9a52f05`）
- 页内查找会高亮每一处命中并滚动到当前项；指针会映射所指元素的光标，因此链接在点击前就看得出来是链接（`aa33e67`）
- 下载默认拒绝，允许后以永不覆盖的安全文件名保存，并显示进度弹窗、一个打开目录的按钮，每次保存只通告一次（`aa33e67`、`c9f1915`、`9a52f05`）
- HD 开关可把实时画面切换为无损图像以便细看，你的选择会被记住（`e06d4a7`）
- 面板的活动按钮会实时显示 Agent 的浏览器操作；录制期间会显示横幅，提示原始表单值仍留在内存中（`1f0cb32`、`a96f3da`）
- 任何终端标签打印出的开发服务器都会被检测到，并以通知形式提供而不是把你跳转走；浏览器设置可选择新标签起始页和地址栏搜索引擎（`1c7c1cf`、`df03fcf`）
- 暂停始终是你的主动动作：Agent 正在驱动的页面会显示播放/暂停按钮，驱动期间标签栏变绿、暂停后转为静音色，点击、滚动或输入都不会替你暂停它（`197fef5`、`2e6e35a`）

**Agent 浏览器工具**

- 内置浏览器服务通过本地端点或 sidecar 投递，包括那些 CLI 读取自己 MCP 文件的 Agent；`pi` 与 `factory-droid` 什么都拿不到，会如实标注而不是静默失败（`b0f6fb2`、`4dc4a90`）
- 31 个工具保留粗粒度、细粒度和视觉三档，并且从页面读到的一切都按不可信内容围起来，而不是凭一句提示就采信（`c81194a`）
- 跨源框架内的页面同样可以读取，嵌入的小组件不会成为盲区（`9cd20d0`）
- 上传文件或在浏览器中打开本地文件会先询问你，卡片上列出文件名且没有「始终允许」——位于项目内并不等于同意把文件送出去（`9cd20d0`）
- 导航处处受限：新建标签与你手动输入的地址受同一套策略约束，一次性批准不会被记住，正在使用的标签也不会被清理流程在 Agent 脚下关掉（`6cde975`）

**浏览器与代码**

- 在页面里 Alt+点击会打开该元素的源文件并定位到行，这次点击不会到达页面；没有答案时面板会内联说明原因（`ae3e82f`）
- 在源码行上 Alt+点击会高亮并滚动到这一行渲染出的元素，用的是与 Agent 操作相同的那个高亮（`6aec9f4`）

**预览与文件**

- 项目外的文件在预览面板中以只读方式打开：客户端直接读取，保存会被拒绝并给出说明，标签也始终显示文件真正所在的位置（`904c7be`、`3d83253`）
- 标签菜单的文件浏览器现在也能选文件：点击行是选中而不是进入目录，确认的路径会在预览中打开（`d41d783`）

**配置中心**

- Prompt 成为配置中心的一级标签：可搜索的列表、每条记录一张卡片、所选 Prompt 的单一编辑器；列表预览 Prompt 正文而不是重复它的类型，带着未保存修改离开会先询问（`4d203b5`、`37a2fb7`）
- 已启用的 Prompt 会作为快捷短语出现在输入框的 `/` 弹窗中，按原文插入，排序先按使用频率再按最近编辑（`b1e0d21`）
- 快捷短语弹窗会按行数调整高度，预览它将插入的文本，并避免过长的名称把预览挤没（`8c3dadb`）
- MCP 市场分页遍历整个注册表，而不只是已抓取的第一个窗口；传输筛选在切页之前收窄结果，筛选后的视图不会被误当成完整目录（`5f575e8`）
- 管理侧栏的资源行统一一种选中样式，结果摘要改为正文尺寸，必须截断的卡片说明以省略号结尾而不是从词中间切断（`f467d7d`）

**技能**

- 技能市场改读 ClawHub，使用注册表自己的排名和分页，因此浏览不再需要替代查询，目录也能真正分页（`3d95486`）
- 技能以完整文件夹安装——说明文档加上它调用的引用、脚本和模板——安装前解包供你查看，条目被限制在技能自己的文件夹内，被拒绝的文件会逐个报告（`3d95486`）
- 一次安装即可到达 Claude、Codex、Cursor、Gemini、OpenCode 和默认来源，走的是原生技能导出（`3d95486`）

**Agent 管理**

- 托管 Agent 运行时被固定到稳定入口，重启 Agent 不再改写你的持久配置，也不会让绑定到它的会话失效（`64f3c7b`）
- 对外观性修改供应商 Profile 不再让会话绑定失效，而端点、凭证或命令的真正改动仍会照常重建它们（`eeb55ac`）
- OpenCode 新增可按会话选择的自动接受模式，同时也让工作区外的读取不再每次都弹窗（`af8a9fa`）
- Codex 会话在你所选 Profile 模型上启动，因此换一个模型会干净地重启，而不是让进程停留在旧模型上（`b5dc05d`）
- Claude Profile 模型无需修改你自己的 Claude 配置即可解析（`9d71fcc`）
- 排队中的消息能在本地写入争用中存活，不再报「消息未发送」（`8530dd5`）

**输入框**

- 从建议菜单选中的 `/` 命令、`@` 文件或 `$` 技能成为一个整体：光标一步跨过，一次退格整体删除，点击文件 token 即可打开该文件——而发给 Agent 的文本仍然与你看到的完全一致（`3f47e3c`）

**工作台与设置**

- 查找跟随焦点：Ctrl+F 先交给聚焦的编辑器，再交给聚焦的浏览器面板，只有真正持有键盘的对话才打开会话搜索（`d793b4f`、`9a52f05`）
- 拖动会话会移动你已选中的每一行，包括当前会话行，因此高亮的内容和移动的内容始终是同一批行（`b699a38`、`7b0c3e9`）
- 设置行按页面宽度决定堆叠还是同行，值列保持可收缩且标签列有下限，长值能干净换行而不会被挤成每行一个字符（`68f8be5`）
- 开发者 FPS HUD 每次打开都居中显示，拖动是相对中心的偏移，且永远不会跑出窗口（`87a2268`、`4b52031`）
- 更新入口与 About 页面已修复：图标使用成功色，从更新面板打开 About 不再自顾自地出错，发布说明滚动时不会带着后面的面板一起动（`7581dff`）
- 右键菜单、对话框或另一个分屏持有键盘时，编辑器仍显示你的选区（`a6c62ec`）
- 关闭「窗口失活时暂停动画」后，后台工作台真的继续动，包括侧栏指示器、终端光标和回合计时器（`beacd98`）
- 中文界面把该功能称为 Skill 而不是「技能」，在输入框、侧栏和市场标题中保持一致（`1f51aa9`、`5f575e8`）

### 修复

- 修复在 Windows 上打开标签：残留的浏览器端点不再指向已死的浏览器，也不再每次启动都探测你安装的 Chrome（`a7a7289`）
- 修复第一次点击浏览器入口就崩溃的问题，画面流结束时不再冻结在最后一帧（`9c9a00c`、`90ab6f8`）
- 丢弃会导致所有页面加载失败的继承代理设置；页面加载失败时保留你请求的地址，标签不再停在内部的错误页地址上（`5d7d9cb`、`1f66568`）
- 补上面板此前丢掉的事件——页面对话框、文件选择器和可用性变化——让只有明确输入才会从 Agent 手中接管，并让已停止会话的批准请求立即被拒绝而不是耗完超时（`2e6e35a`、`3079c7d`）
- 每次下载只通告一次，修复面板的空格、回车、Ctrl+X 和下载反馈，并让拖动按你预期的方式选择文本（`c9f1915`、`9a52f05`、`65c8ab2`）
- 移除让浏览器无法使用的阻断式风险提示，并删除无法安全附着的检查器和元素拾取器（`93ab323`、`228ec67`）
- 让市场分页器固定显示而卡片在其下滚动，把不滚动面板的高度还给它，并修复 About 页面的发布信息（`0c0d534`、`e638395`、`7581dff`）
- 设置行按页面排版并让长值换行；关闭失活暂停后自驱动动画继续运行；市场的导入表单仍使用自身滚动（`68f8be5`、`beacd98`）
- 远程面板现在会如实标记为降级，并说明这些显示选项不会配置真正运行页面的那台机器，而不是静默失败（`6a7d58f`）

### 性能

- 削减空闲重绘：悬停效果、侧栏状态指示器、回合计时器和区域设置查询在无变化时不再重绘窗口，FPS HUD 也在缓存边界之后绘制（`80f4b4d`）
- 流式分块不再各付一次数据库往返、一次提交和一次轮询，共享运行时也按所在主机的规模调整（`e934ec0`）
- 在后台退让而不是空烧帧：窗口失活时所选会话的轮询减少，token 用量改在界面线程之外读取（`80f4b4d`）

### 底层改动

- 版本号统一升到 `0.1.0-rc.7`，覆盖工作区、打包输入、Android 版本回退值、README 与部署文档，以及许可证策略中已审核的资源包
- 工作区迁移到已发布的 `gpui-kit` 0.7.0 与 `gpui-pre` 0.3.7，并按新的依赖图重新生成许可证 SBOM 与声明
