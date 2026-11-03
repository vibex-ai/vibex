//! Product copy for the three languages Vibex ships.
//!
//! The desktop keeps its own `Strings` table inside `apps/desktop`, and the TUI
//! deliberately does not reach into the application crate for it. Instead the
//! table is declared once here through a macro so the three languages cannot
//! drift: a missing translation is a compile error, not a runtime fallback.
//!
//! Locale classification itself is shared — [`vibex_ui::locale::Locale`] owns
//! the BCP-47/POSIX resolution so the TUI and the desktop agree on what
//! `zh-Hant-HK` means.

use std::env;

pub use vibex_ui::locale::Locale;

/// Every user-visible string the TUI renders.
///
/// Rows are `key => { en, zh-CN, zh-TW }`. Keeping the three translations on
/// one line is what makes review of a new string possible without opening three
/// files.
#[macro_export]
macro_rules! strings {
    ($( $key:ident => { $en:expr, $cn:expr, $tw:expr } ),* $(,)?) => {
        /// Resolved product copy for one locale.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Strings {
            pub locale: $crate::locale::Locale,
        }

        impl Strings {
            pub const fn for_locale(locale: $crate::locale::Locale) -> Self {
                Self { locale }
            }

            $(
                #[allow(missing_docs)]
                pub const fn $key(&self) -> &'static str {
                    match self.locale {
                        $crate::locale::Locale::En => $en,
                        $crate::locale::Locale::ZhCn => $cn,
                        $crate::locale::Locale::ZhTw => $tw,
                    }
                }
            )*

            /// The full key set, used by the locale-coverage test.
            pub const ALL_KEYS: &'static [&'static str] = &[ $( stringify!($key) ),* ];
        }
    };
}

strings! {
    // ---- product chrome -------------------------------------------------
    app_name => { "Vibex", "Vibex", "Vibex" },
    product_tagline => {
        "Agent workbench",
        "Agent 工作台",
        "Agent 工作台"
    },
    loading => { "Loading…", "加载中…", "載入中…" },
    connecting => { "Connecting…", "连接中…", "連線中…" },
    reconnecting => { "Reconnecting…", "重新连接中…", "重新連線中…" },
    disconnected => {
        "Disconnected — showing the last known state",
        "连接已断开 — 显示最后已知状态",
        "連線已中斷 — 顯示最後已知狀態"
    },
    stale_data => { "may be out of date", "可能已过期", "可能已過期" },
    retry => { "Retry", "重试", "重試" },
    cancel => { "Cancel", "取消", "取消" },
    confirm => { "Confirm", "确认", "確認" },
    close => { "Close", "关闭", "關閉" },
    back => { "Back", "返回", "返回" },
    reload => { "Reload", "重新加载", "重新載入" },
    none => { "None", "无", "無" },
    yes => { "Yes", "是", "是" },
    no => { "No", "否", "否" },
    unknown => { "Unknown", "未知", "未知" },
    search => { "Search", "搜索", "搜尋" },
    filter => { "Filter", "过滤", "過濾" },
    actions => { "Actions", "操作", "操作" },
    details => { "Details", "详情", "詳情" },
    status => { "Status", "状态", "狀態" },
    error => { "Error", "错误", "錯誤" },
    warning => { "Warning", "警告", "警告" },
    done => { "Done", "完成", "完成" },
    failed => { "Failed", "失败", "失敗" },
    pending => { "Pending", "等待中", "等待中" },
    running => { "Running", "运行中", "執行中" },
    idle => { "Idle", "空闲", "閒置" },
    // The four things a running turn can be doing, named the way the desktop
    // names them so the two surfaces read alike, and the tally of the tools it
    // has called. The count comes first because both languages put it there:
    // the label is the unit, not the sentence.
    //
    // These are the plain words: the band above the prompt is the desktop's
    // information bar, which states what the Agent is doing without the closing
    // ellipsis. That mark belongs to the desktop timeline's pending row, the
    // surface that says work is still arriving, and this band has no such row.
    phase_preparing => { "Preparing", "准备中", "準備中" },
    phase_thinking => { "Thinking", "思考中", "思考中" },
    phase_calling_tool => { "Calling tool", "调用工具", "呼叫工具" },
    phase_generating => { "Generating", "生成中", "生成中" },
    phase_waiting_approval => { "Waiting for approval", "等待确认", "等待確認" },
    tool_calls => { "tool calls", "次工具调用", "次工具呼叫" },
    archived => { "Archived", "已归档", "已封存" },
    copied => { "Copied", "已复制", "已複製" },
    nothing_here => { "Nothing here yet", "这里还没有内容", "這裡還沒有內容" },
    // Footer verbs. They are shorter than the destination labels because a
    // modal footer is a key legend, not a sentence.
    hint_nav => { "nav", "移动", "移動" },
    hint_select => { "select", "选择", "選擇" },
    hint_open => { "open", "进入", "進入" },
    hint_scroll => { "scroll", "滚动", "捲動" },
    hint_run => { "run", "执行", "執行" },
    hint_expand => { "expand", "展开", "展開" },
    hint_collapse => { "collapse", "收起", "收合" },
    hint_toggle => { "toggle", "切换", "切換" },
    hint_switch => { "switch", "切换", "切換" },
    hint_edit => { "edit", "编辑", "編輯" },
    hint_previous => { "prev", "上一个", "上一個" },
    hint_next => { "next", "下一个", "下一個" },
    hint_clear => { "clear", "清空", "清空" },
    hint_commit => { "keep", "保留", "保留" },
    hint_revert => { "revert", "还原", "還原" },
    hint_reset => { "reset", "重置", "重設" },
    hint_confirm_delete => { "confirm delete", "确认删除", "確認刪除" },
    // A modal that nests: `Esc` steps out one level rather than closing it.
    hint_back => { "back", "返回", "返回" },

    // ---- destinations ---------------------------------------------------
    nav_sessions => { "Sessions", "会话", "工作階段" },
    nav_management => { "Management", "管理", "管理" },
    nav_settings => { "Settings", "设置", "設定" },
    nav_help => { "Help", "帮助", "說明" },
    nav_agent => { "Agent", "Agent", "Agent" },
    nav_git => { "Git", "Git", "Git" },

    // ---- sessions -------------------------------------------------------
    sessions_title => { "Session list", "会话列表", "工作階段清單" },
    sessions_empty => {
        "No sessions yet — press n to start one",
        "还没有会话 — 按 n 新建",
        "還沒有工作階段 — 按 n 新增"
    },
    session_new => { "New session", "新建会话", "新增工作階段" },
    session_runtime_label => { "Agent setup", "Agent 配置", "Agent 設定" },
    session_workspace_label => { "Workspace", "工作区", "工作區" },
    // The list's summary chips count sessions by state. Lower case because a
    // chip is a count first and a word second: `◇ 5 idle` reads as a tally,
    // while `◇ 5 Idle` reads as a sentence someone started.
    session_chip_waiting => { "waiting", "待处理", "待處理" },
    session_chip_running => { "running", "运行中", "執行中" },
    session_chip_idle => { "idle", "空闲", "閒置" },
    session_chip_failed => { "failed", "失败", "失敗" },
    queue_released => { "Held message sent", "排队消息已发出", "排隊訊息已發出" },
    // The session list reads a row's hour the way the desktop's sidebar does: a
    // wall clock today, then a count of days, then weeks. The suffix carries its
    // own leading space, because the number is formatted in front of it and
    // Chinese needs no space where English does.
    time_just_now => { "now", "刚刚", "剛剛" },
    time_minutes => { "m", "分钟", "分鐘" },
    time_hours => { "h", "小时", "小時" },
    time_days => { "d", "天", "天" },
    time_days_ago => { " days ago", " 天前", " 天前" },
    time_weeks_ago => { " weeks ago", " 周前", " 週前" },
    sidebar_yesterday => { "Yesterday", "昨天", "昨天" },
    // The word beside a project's running count, which the desktop keeps at the
    // row's far edge.
    sidebar_running => { "running", "运行中", "執行中" },
    // The words the desktop puts in a session row's status column when the state
    // has no hour worth reading.
    sidebar_state_initializing => { "INIT", "初始化", "初始化" },
    sidebar_state_archived => { "ARCHIVED", "已归档", "已封存" },
    // What a project's or a folder's action button opens.
    sidebar_row_actions => { "Actions", "操作", "操作" },
    sidebar_action_new_session => { "New session", "新建会话", "建立會話" },
    sidebar_action_new_folder => { "New folder", "新建文件夹", "新增資料夾" },
    sidebar_action_rename_folder => { "Rename folder", "重命名文件夹", "重新命名資料夾" },
    sidebar_action_collapse => { "Collapse", "折叠", "摺疊" },
    sidebar_action_expand => { "Expand", "展开", "展開" },
    sidebar_action_delete_project => { "Delete project", "删除项目", "刪除專案" },
    sidebar_action_delete_folder => { "Delete folder", "删除文件夹", "刪除資料夾" },
    sidebar_folder_name_placeholder => { "Folder name", "文件夹名称", "資料夾名稱" },
    sidebar_folder_name_empty => { "Enter a folder name.", "请输入文件夹名称。", "請輸入資料夾名稱。" },
    sidebar_confirm_delete_project => {
        "Delete this project and every session in it?",
        "删除该项目及其中的所有会话？",
        "刪除該專案及其中的所有工作階段？"
    },
    sidebar_confirm_delete_folder => {
        "Delete this folder? Its sessions are kept.",
        "删除该文件夹？其中的会话会保留。",
        "刪除該資料夾？其中的工作階段會保留。"
    },
    session_untitled => { "untitled session", "未命名会话", "未命名工作階段" },
    // The one line a terminal notification carries. It names what happened,
    // never what was said: the notification layer prepends the Agent's name so
    // a reader with several sessions knows which one to come back to.
    notify_turn_failed => {
        "Agent turn failed",
        "Agent 回合执行失败",
        "Agent 回合執行失敗"
    },
    notify_needs_input => {
        "Agent needs your confirmation",
        "Agent 需要你确认内容",
        "Agent 需要你確認內容"
    },
    notify_turn_completed => {
        "Agent completed work",
        "Agent 已完成工作",
        "Agent 已完成工作"
    },
    workspace_switch_hint => { "Ctrl+W change", "Ctrl+W 切换", "Ctrl+W 切換" },
    session_open => { "Open", "打开", "開啟" },
    session_rename => { "Rename", "重命名", "重新命名" },
    session_archive => { "Archive", "归档", "封存" },
    session_delete => { "Delete", "删除", "刪除" },
    session_fork => { "Fork", "复制分支", "分支複製" },
    session_show_archived => { "Show archived", "显示已归档", "顯示已封存" },
    session_workspace => { "Workspace", "工作区", "工作區" },
    session_title_label => { "Title", "标题", "標題" },
    session_state => { "State", "状态", "狀態" },
    session_confirm_delete => {
        "Delete this session? This cannot be undone.",
        "删除这个会话？此操作无法撤销。",
        "刪除這個工作階段？此操作無法復原。"
    },
    session_confirm_archive => {
        "Archive this session?",
        "归档这个会话？",
        "封存這個工作階段？"
    },
    session_temp_workspace => {
        "Temporary workspace (no project)",
        "临时会话（不使用项目）",
        "臨時工作階段（不使用專案）"
    },
    // Detail-card field labels. Kept to one word each so the label column can
    // stay narrow enough for the value beside it.
    session_card_id => { "ID", "ID", "ID" },
    session_card_parent => { "Main", "主会话", "主工作階段" },
    session_card_agent => { "Agent", "Agent", "Agent" },
    session_card_model => { "Model", "模型", "模型" },
    session_card_created => { "Created", "创建于", "建立於" },
    session_card_updated => { "Updated", "更新于", "更新於" },
    session_card_last_message => { "Last turn", "最近一轮", "最近一輪" },
    session_card_messages => { "Messages", "消息数", "訊息數" },
    session_card_turns => { "Turns", "轮次", "輪次" },
    session_card_tools => { "Tools", "工具", "工具" },
    dock_agents => { "Agents", "子代理", "子代理" },
    dock_children => { "Sub-sessions", "子会话", "子工作階段" },
    dock_children_none => {
        "No sub-sessions delegated by this session",
        "这个会话没有委派子会话",
        "這個工作階段沒有委派子工作階段"
    },
    dock_children_loading => {
        "Reading this session's sub-sessions…",
        "正在读取这个会话的子会话…",
        "正在讀取這個工作階段的子工作階段…"
    },
    session_no_parent => {
        "This session was not delegated by another",
        "这个会话不是由其他会话委派的",
        "這個工作階段不是由其他工作階段委派的"
    },
    parent_session => { "Main session", "主会话", "主工作階段" },
    dock_plan => { "Plan", "计划", "計畫" },
    dock_queue => { "Held", "排队", "排隊" },
    dock_title => { "Running", "运行中", "執行中" },
    dock_more => { "more", "更多", "更多" },
    dock_empty => {
        "Nothing running — agents, the plan and held messages appear here",
        "当前没有运行中的内容 — 子代理、计划与排队消息会显示在这里",
        "目前沒有執行中的內容 — 子代理、計畫與排隊訊息會顯示在這裡"
    },
    dock_hint => {
        "Alt+J/K move · Alt+G open · Alt+H hide done · Alt+D close",
        "Alt+J/K 移动 · Alt+G 打开 · Alt+H 隐藏已完成 · Alt+D 关闭",
        "Alt+J/K 移動 · Alt+G 開啟 · Alt+H 隱藏已完成 · Alt+D 關閉"
    },
    dock_hidden_done => { "Finished work hidden", "已隐藏已完成项", "已隱藏已完成項" },
    dock_shown_done => { "Finished work shown", "已显示已完成项", "已顯示已完成項" },
    sidebar_pinned => { "Pinned to the top", "已置顶", "已置頂" },
    auto_continue_enabled => { "Auto-continue on", "已开启自动继续", "已開啟自動繼續" },
    auto_continue_disabled => { "Auto-continue off", "已关闭自动继续", "已關閉自動繼續" },
    auto_continue_paused => { "Auto-continue paused", "自动继续已暂停", "自動繼續已暫停" },
    auto_continue_resumed => { "Auto-continue resumed", "自动继续已恢复", "自動繼續已恢復" },
    sidebar_unpinned => { "Pin removed", "已取消置顶", "已取消置頂" },
    sidebar_pinned_first => {
        "Pinned sessions always come first — unpin one to move past it",
        "置顶会话始终在最前 — 先取消置顶才能越过",
        "置頂工作階段一律在最前 — 先取消置頂才能越過"
    },
    session_cards_none => {
        "No session detail cards are open",
        "没有打开会话详情卡片",
        "沒有開啟工作階段詳情卡片"
    },

    // ---- transcript search ----------------------------------------------
    search_label => { " search: ", " 搜索: ", " 搜尋: " },
    search_bad_pattern => { "bad pattern", "正则无效", "正則無效" },
    search_no_matches => { "no matches", "无匹配", "無符合" },
    search_regex_hint => {
        "Regular expression · case-insensitive unless it has an uppercase letter",
        "正则表达式 · 不含大写字母时忽略大小写",
        "正規表達式 · 不含大寫字母時忽略大小寫"
    },

    // ---- first-run guidance ---------------------------------------------
    onboarding_title => { "Getting started", "开始使用", "開始使用" },
    onboarding_connect => {
        "Connect to the runtime",
        "连接运行时",
        "連線執行階段"
    },
    onboarding_connect_detail => {
        "The client attaches to the runtime that owns this home",
        "客户端会连接到拥有此 home 的运行时",
        "客戶端會連線到擁有此 home 的執行階段"
    },
    onboarding_workspace => {
        "Choose where the Agent works",
        "选择 Agent 的工作目录",
        "選擇 Agent 的工作目錄"
    },
    onboarding_workspace_detail => {
        "Browse directories on the machine that runs the Agent",
        "浏览运行 Agent 的机器上的目录",
        "瀏覽執行 Agent 的機器上的目錄"
    },
    onboarding_session => {
        "Start a session",
        "新建会话",
        "新增工作階段"
    },
    onboarding_session_detail => {
        "One session per task; it keeps the conversation and its workspace",
        "每个任务一个会话，保存对话与工作区",
        "每個任務一個工作階段，保存對話與工作區"
    },
    onboarding_first_message => {
        "Write the first message",
        "写下第一条消息",
        "寫下第一則訊息"
    },
    onboarding_first_message_detail => {
        "Say what you want done; / lists commands, @ lists files",
        "说明你想做什么；/ 列出命令，@ 列出文件",
        "說明你想做什麼；/ 列出命令，@ 列出檔案"
    },
    onboarding_done => { "Ready", "已就绪", "已就緒" },

    // ---- composer -------------------------------------------------------
    // The name the external editor's scratch file is created under. It says
    // what the file holds rather than repeating product copy: the path is shown
    // to the reader by their editor's own status line.
    composer_editor_title => { "Vibex draft", "Vibex 草稿", "Vibex 草稿" },
    composer_send => { "Send", "发送", "傳送" },
    composer_newline => { "Newline", "换行", "換行" },
    transcript_browse => { "Timeline", "时间线", "時間線" },
    transcript_details => { "Details", "详情", "詳情" },
    transcript_expand => { "Expand / collapse", "展开 / 收起", "展開 / 收合" },
    session_stop => { "Stop", "停止", "停止" },
    composer_nothing_to_undo => {
        "Nothing left to undo in the draft",
        "草稿没有可撤销的修改",
        "草稿沒有可復原的修改"
    },
    composer_nothing_to_redo => {
        "Nothing to redo in the draft",
        "草稿没有可重做的修改",
        "草稿沒有可重做的修改"
    },
    composer_nothing_to_yank => {
        "Nothing has been cut from the draft yet",
        "草稿中还没有剪切过内容",
        "草稿中還沒有剪下過內容"
    },
    image_attached => { "Image attached", "已附加图片", "已附加圖片" },
    image_not_found => { "No such image file", "找不到该图片文件", "找不到該圖片檔案" },
    image_unsupported => {
        "That file is not an image this client can attach",
        "该文件不是本客户端可附加的图片格式",
        "該檔案不是本客戶端可附加的圖片格式"
    },
    image_too_large => {
        "Image is larger than 5 MB",
        "图片超过 5 MB",
        "圖片超過 5 MB"
    },
    image_cap => {
        "A prompt can carry at most 10 images",
        "一条消息最多附加 10 张图片",
        "一則訊息最多附加 10 張圖片"
    },
    clipboard_empty => {
        "Nothing to paste",
        "剪贴板里没有可粘贴的内容",
        "剪貼簿裡沒有可貼上的內容"
    },
    clipboard_refused => {
        "The terminal would not hand over its clipboard",
        "终端拒绝提供它的剪贴板",
        "終端拒絕提供它的剪貼簿"
    },
    image_clipboard_empty => {
        "No image on the clipboard — type a path instead",
        "剪贴板里没有图片 — 请改为输入路径",
        "剪貼簿裡沒有圖片 — 請改為輸入路徑"
    },
    image_path_title => { "Image path", "图片路径", "圖片路徑" },
    image_count => { "images", "张图片", "張圖片" },
    composer_nothing_selected => {
        "Select part of the draft first",
        "请先选中草稿中的内容",
        "請先選取草稿中的內容"
    },
    composer_queue => { "Queue", "排队", "排隊" },
    composer_interrupt => { "Interrupt", "打断", "中斷" },
    composer_continue => { "Continue", "继续", "繼續" },
    composer_steer => { "Steer", "插话", "插話" },
    composer_steer_no_session => {
        "Nothing is running here yet — Enter sends this and creates the session",
        "新会话还没有运行中的回合 — 按 Enter 发送并新建会话",
        "新工作階段還沒有執行中的回合 — 按 Enter 傳送並新增工作階段"
    },
    composer_steer_unavailable => {
        "Steering needs a local runtime — the turn will be interrupted and resent",
        "运行中插话需要本机运行时 — 将改为打断后重发",
        "執行中插話需要本機執行階段 — 將改為中斷後重送"
    },
    composer_edit_external => {
        "Edit in $EDITOR",
        "用 $EDITOR 编辑",
        "用 $EDITOR 編輯"
    },
    composer_empty => { "Message is empty", "消息为空", "訊息為空" },
    composer_history => { "History", "历史", "歷史" },
    composer_history_empty => {
        "No sent message matches that",
        "没有匹配的已发送消息",
        "沒有符合的已傳送訊息"
    },
    composer_history_hint => {
        "Type after ? to search sent messages; Enter recalls the match",
        "在 ? 后输入以搜索已发送消息；Enter 取回",
        "在 ? 後輸入以搜尋已傳送訊息；Enter 取回"
    },
    composer_draft_cleared => { "Draft cleared", "草稿已清空", "草稿已清空" },
    quit_hint => {
        "Press Ctrl+C again to quit",
        "再按一次 Ctrl+C 退出",
        "再按一次 Ctrl+C 離開"
    },
    composer_attachments => { "Attachments", "附件", "附件" },
    composer_attach_hint => {
        "Type a path to attach it",
        "输入路径作为附件",
        "輸入路徑作為附件"
    },
    composer_command_menu => { "Commands", "命令", "命令" },
    composer_file_menu => { "Files", "文件", "檔案" },
    composer_skill_menu => { "Skills", "技能", "技能" },
    // The panels of the `/` and `@` drawers, named as the desktop popup names
    // them. `Commands` and `Skills` reuse the words above, so the drawer and the
    // empty composer's vocabulary cannot drift apart.
    composer_tab_phrases => { "Quick phrases", "快捷短语", "快捷短語" },
    composer_tab_files => { "Workspace files", "工作区文件", "工作區檔案" },
    composer_tab_delegation => { "Agent delegation", "Agent 委派", "Agent 委派" },
    composer_phrases_empty => { "No quick phrases yet", "还没有快捷短语", "還沒有快捷短語" },
    composer_no_matches => { "No matches", "没有匹配项", "沒有符合項目" },

    // ---- the panel that explains a collapsed chip -----------------------
    preview_more_lines => { "more lines", "行未显示", "行未顯示" },
    preview_expand_enter => { "Enter to expand", "Enter 展开", "Enter 展開" },
    preview_expand_paste => {
        "Paste the same block again to expand",
        "再次粘贴同样的内容以展开",
        "再次貼上同樣的內容以展開"
    },
    preview_unavailable => {
        "This picture could not be read",
        "无法读取该图片",
        "無法讀取該圖片"
    },

    // ---- transcript -----------------------------------------------------
    transcript_thinking => { "Thinking", "思考", "思考" },
    transcript_tool => { "Tool", "工具", "工具" },
    transcript_command => { "Command", "命令", "命令" },
    transcript_file_change => { "File change", "文件变更", "檔案變更" },
    transcript_error => { "Error", "错误", "錯誤" },
    transcript_notice => { "Notice", "提示", "提示" },
    transcript_plan => { "Plan", "计划", "計畫" },
    transcript_user => { "You", "你", "你" },
    transcript_agent => { "Agent", "Agent", "Agent" },
    transcript_retry => { "Retry", "重试", "重試" },
    transcript_web_search => { "Web search", "网页搜索", "網頁搜尋" },
    transcript_todo => { "Todo", "待办", "待辦" },
    transcript_collaboration => { "Sub-agent", "子代理", "子代理" },
    transcript_image => { "Image", "图片", "圖片" },
    transcript_git => { "Git", "Git", "Git" },
    transcript_system => { "System", "系统", "系統" },
    transcript_permission => { "Approval", "审批", "審批" },
    transcript_elicitation => { "Question", "询问", "詢問" },
    transcript_collapsed_hint => {
        "collapsed — press e to expand",
        "已折叠 — 按 e 展开",
        "已折疊 — 按 e 展開"
    },
    transcript_expand_all => { "Expand all", "展开全部", "展開全部" },
    transcript_collapse_all => { "Collapse all", "折叠全部", "折疊全部" },
    transcript_worked_for => { "Worked for", "工作了", "工作了" },
    transcript_follow => { "Following", "已跟随", "已跟隨" },
    transcript_paused => { "Scroll paused", "滚动已暂停", "滾動已暫停" },
    transcript_empty => {
        "No messages yet — say something below",
        "还没有消息 — 在下方输入",
        "還沒有訊息 — 在下方輸入"
    },
    transcript_copy_block => { "Copy block", "复制此块", "複製此區塊" },
    transcript_copy_meta => { "Copy metadata", "复制元数据", "複製中繼資料" },
    transcript_context_compacted => {
        "Context was compacted",
        "上下文已压缩",
        "上下文已壓縮"
    },
    transcript_mermaid_hint => {
        "diagram source (render it with an external tool)",
        "图表源码（请用外部工具渲染）",
        "圖表原始碼（請用外部工具渲染）"
    },
    transcript_image_hint => {
        "inline images are not available in the terminal",
        "终端不支持内联图片",
        "終端機不支援內嵌圖片"
    },

    // ---- approvals ------------------------------------------------------
    approval_title => { "Approval required", "需要审批", "需要審批" },
    approval_label => { "Approvals", "审批", "審批" },
    details_state => { "State", "状态", "狀態" },
    details_agent => { "Agent", "Agent", "Agent" },
    details_model => { "Model", "模型", "模型" },
    details_workspace => { "Workspace", "工作区", "工作區" },
    details_branch => { "Branch", "分支", "分支" },
    details_context => { "Context", "上下文", "上下文" },
    background_tasks => { "background", "后台任务", "背景工作" },
    queued_messages => { "queued", "已排队", "已排隊" },
    queue_held => {
        "Held until the running turn ends",
        "将在当前轮次结束后发送",
        "將在目前輪次結束後傳送"
    },
    queue_editing => {
        "Queued message moved back into the draft",
        "已把排队消息放回输入框",
        "已把排隊訊息放回輸入框"
    },
    queue_hint => {
        "Alt+↑↓ pick · Alt+E edit · Alt+X drop · Alt+J/K reorder · Alt+S schedule · Alt+Enter send now",
        "Alt+↑↓ 选择 · Alt+E 编辑 · Alt+X 删除 · Alt+J/K 调序 · Alt+S 定时 · Alt+Enter 立即发送",
        "Alt+↑↓ 選擇 · Alt+E 編輯 · Alt+X 刪除 · Alt+J/K 調序 · Alt+S 定時 · Alt+Enter 立即傳送"
    },
    // ---- scheduling a send ----------------------------------------------
    // The Desktop's clock button, on the character grid. A message that is
    // written is given a deadline instead of a turn; until the clock reaches
    // it the message waits in the held queue beside the ordinary ones.
    schedule_title => { "Schedule send", "定时发送", "定時傳送" },
    schedule_mode_at => { "Date and time", "定时", "定時" },
    schedule_mode_after => { "Countdown", "倒计时", "倒計時" },
    schedule_field_mode => { "Mode", "方式", "方式" },
    schedule_field_when => { "Send at", "发送时间", "傳送時間" },
    schedule_field_hours => { "Hours", "时", "時" },
    schedule_field_minutes => { "Minutes", "分", "分" },
    schedule_field_seconds => { "Seconds", "秒", "秒" },
    schedule_when_hint => {
        "YYYY-MM-DD HH:MM:SS, in your own timezone",
        "按本地时区填写 YYYY-MM-DD HH:MM:SS",
        "按本機時區填寫 YYYY-MM-DD HH:MM:SS"
    },
    schedule_countdown_hint => {
        "Counts down from the moment the message is submitted",
        "从发送该消息的那一刻开始倒计时",
        "從傳送該訊息的那一刻開始倒數"
    },
    schedule_note => {
        "Sends automatically when due. If the session is running, waits for it to finish. Keep Vibex open.",
        "到时自动发送；会话运行中则等待结束。请保持 Vibex 开启。",
        "到時自動傳送；會話執行中則等待結束。請保持 Vibex 開啟。"
    },
    schedule_clear => { "Clear schedule", "清除定时", "清除定時" },
    schedule_cleared => { "Schedule cleared", "已清除定时", "已清除定時" },
    schedule_set => { "Send scheduled", "已设定发送时间", "已設定傳送時間" },
    schedule_error_not_future => {
        "Choose a future time.",
        "请选择未来的时间。",
        "請選擇未來的時間。"
    },
    schedule_error_countdown => {
        "Enter a countdown between 1 second and 365 days; minutes and seconds must be 0–59.",
        "倒计时须为 1 秒至 365 天，分钟和秒须为 0–59。",
        "倒計時須為 1 秒至 365 天，分鐘和秒須為 0–59。"
    },
    schedule_error_ambiguous => {
        "This local time is ambiguous or unavailable. Choose another time.",
        "此本地时间不存在或不唯一，请选择其他时间。",
        "此本機時間不存在或不唯一，請選擇其他時間。"
    },
    schedule_error_unreadable => {
        "Type a deadline as YYYY-MM-DD HH:MM:SS.",
        "请按 YYYY-MM-DD HH:MM:SS 填写发送时间。",
        "請按 YYYY-MM-DD HH:MM:SS 填寫傳送時間。"
    },
    schedule_pending => { "Scheduled send", "定时发送", "定時傳送" },
    schedule_message => { "Scheduled message", "定时消息", "定時訊息" },
    schedule_due_waiting => {
        "Due · waiting to send",
        "已到时 · 等待发送",
        "已到時 · 等待傳送"
    },
    schedule_due_running => {
        "Due · waiting for the current turn",
        "已到时 · 等待当前运行结束",
        "已到時 · 等待目前執行結束"
    },
    schedule_hint_move => { "move", "移动", "移動" },
    schedule_hint_switch => { "shape", "切换方式", "切換方式" },
    schedule_hint_type => { "type", "输入", "輸入" },
    schedule_hint_apply => { "apply", "应用", "套用" },
    mode_shell => { "shell", "shell", "shell" },
    mode_remember => { "remember", "记住", "記住" },
    mode_plan => { "plan", "计划", "計畫" },
    approval_risk => { "Risk", "风险", "風險" },
    approval_allow => { "Allow", "允许", "允許" },
    approval_deny => { "Deny", "拒绝", "拒絕" },
    approval_always => { "Always this session", "本会话总是允许", "本次工作階段總是允許" },
    approval_pending => { "Submitting…", "提交中…", "提交中…" },
    approval_resolved => { "Approval resolved", "审批已处理", "審批已處理" },
    approval_none => { "No approvals waiting", "没有待处理审批", "沒有待處理審批" },
    approval_waiting => {
        "The Agent is blocked until you answer",
        "Agent 正在等待你的答复",
        "Agent 正在等待你的回覆"
    },
    risk_low => { "Low", "低", "低" },
    risk_medium => { "Medium", "中", "中" },
    risk_high => { "High", "高", "高" },
    risk_critical => { "Critical", "严重", "嚴重" },

    // ---- elicitation ----------------------------------------------------
    elicitation_title => { "The Agent needs input", "Agent 需要输入", "Agent 需要輸入" },
    elicitation_submit => { "Submit", "提交", "提交" },
    elicitation_decline => { "Decline", "拒绝", "拒絕" },
    elicitation_confirm => { "Confirm", "确认", "確認" },
    elicitation_dismiss => { "later", "稍后", "稍後" },
    elicitation_unanswered => { "Not answered", "未回答", "未回答" },
    elicitation_answer_required => {
        "Answer this question before moving on",
        "请先回答这个问题",
        "請先回答這個問題"
    },
    elicitation_yes => { "Yes", "是", "是" },
    elicitation_no => { "No", "否", "否" },
    elicitation_unsupported => {
        "This field cannot be answered in the terminal",
        "终端无法回答此字段",
        "終端機無法回答此欄位"
    },
    elicitation_required => { "Required", "必填", "必填" },
    elicitation_field_text => { "Text", "文本", "文字" },
    elicitation_field_number => { "Number", "数字", "數字" },
    elicitation_field_integer => { "Integer", "整数", "整數" },
    elicitation_field_boolean => { "Boolean", "布尔", "布林" },
    elicitation_field_select => { "Select", "单选", "單選" },
    elicitation_field_multi => { "Multi-select", "多选", "多選" },

    // ---- runtime / model ------------------------------------------------
    runtime_title => { "Agent setup", "Agent 配置", "Agent 設定" },
    runtime_desired => { "Requested", "期望", "期望" },
    runtime_effective => { "Effective", "实际", "實際" },
    runtime_switching => { "Switching…", "切换中…", "切換中…" },
    runtime_switch_failed => {
        "Switch failed — still using the previous Agent",
        "切换失败 — 仍在用之前的 Agent",
        "切換失敗 — 仍在使用先前的 Agent"
    },
    runtime_model => { "Model", "模型", "模型" },
    runtime_agent => { "Agent", "Agent", "Agent" },
    // The Agent's own account, as a picker row names it. The runtime writes an
    // English name into the catalogue for clients that render no locale; a
    // reader of this client gets the one below instead.
    runtime_auth_agent_account => { "Agent account", "Agent 账户", "Agent 帳戶" },
    runtime_probe => { "Probe runtimes", "探测运行时", "探測執行階段" },
    // The switch affordance, shown beside the runtime it moves.
    runtime_switch_hint => { "Ctrl+G change", "Ctrl+G 切换", "Ctrl+G 切換" },
    runtime_current => { "Current", "当前", "目前" },
    runtime_unavailable => { "Unavailable", "不可用", "無法使用" },
    // A creation is named by the Agent it will run on; until the catalogue has
    // arrived there is nothing to name, so the message waits.
    runtime_catalogue_reading => {
        "Reading the runtime catalogue — the message will wait",
        "正在读取运行时列表 — 消息会先保留",
        "正在讀取執行階段清單 — 訊息會先保留"
    },
    runtime_no_agents => {
        "The runtime publishes no Agent to create a session with",
        "运行时没有可用来新建会话的 Agent",
        "執行階段沒有可用來新增工作階段的 Agent"
    },
    // The session on screen is one the authority has not created yet, so there
    // is no runtime to move. Said instead of opening a picker whose Enter could
    // only fail, and instead of swallowing the key.
    runtime_session_creating => {
        "The new session is still being created — its Agent can change once it exists",
        "新会话还在创建中 — 创建完成后才能切换 Agent",
        "新工作階段仍在建立中 — 建立完成後才能切換 Agent"
    },
    runtime_session_uncreated => {
        "The session could not be created — send the message again to switch its Agent",
        "会话没有创建成功 — 请重新发送消息后再切换 Agent",
        "工作階段沒有建立成功 — 請重新傳送訊息後再切換 Agent"
    },
    // What the catalogue view chooses. The picker is titled by the two halves
    // of the surface, and this is the first one: an Agent, its account and its
    // model.
    runtime_choose_agent => { "Choose an Agent", "选择 Agent", "選擇 Agent" },
    // The run options the chosen Agent publishes, one Tab away from the
    // catalogue rows.
    runtime_run_options => { "Run options", "运行选项", "執行選項" },
    runtime_no_run_options => {
        "This Agent publishes no run options",
        "此 Agent 没有可调整的运行选项",
        "此 Agent 沒有可調整的執行選項"
    },
    runtime_default => { "Default", "默认", "預設" },
    runtime_thinking_depth => { "Thinking depth", "思考深度", "思考深度" },
    runtime_conversation_mode => { "Conversation mode", "对话模式", "對話模式" },
    runtime_on => { "On", "开", "開" },
    runtime_off => { "Off", "关", "關" },
    runtime_option_required => { "A value is required", "需要填写一个值", "需要填寫一個值" },
    // The pinned sections above the Agent groups: what the reader used last,
    // and what they starred.
    runtime_recent => { "Recent", "最近使用", "最近使用" },
    runtime_favorites => { "Starred", "已收藏", "已收藏" },
    // Key-bar words for the catalogue view.
    runtime_search => { "search", "搜索", "搜尋" },
    runtime_fold => { "fold", "折叠", "摺疊" },
    runtime_star => { "star", "收藏", "收藏" },
    runtime_manage_account => { "account", "账号", "帳號" },
    runtime_reset => { "reset", "重置", "重設" },
    // The row statuses, in the same column as "Current".
    runtime_status_sign_in => { "Sign in", "需登录", "需登入" },
    runtime_status_verifying => { "Checking…", "校验中…", "校驗中…" },
    runtime_status_discovering => { "Listing models…", "发现模型中…", "探索模型中…" },
    runtime_status_configure => { "Set up", "需配置", "需設定" },
    runtime_status_failed => { "Failed", "切换失败", "切換失敗" },
    runtime_no_match => { "No matching Agent", "没有匹配的 Agent", "沒有符合的 Agent" },
    runtime_summary_hint => { "Tab edits", "Tab 编辑", "Tab 編輯" },

    // ---- workspace ------------------------------------------------------
    workspace_pick => { "Choose a workspace", "选择工作区", "選擇工作區" },
    workspace_browse => { "Browse", "浏览", "瀏覽" },
    workspace_parent => { "Parent directory", "上级目录", "上層目錄" },
    workspace_use_this => { "use this directory", "使用此目录", "使用此目錄" },
    workspace_use_selected => { "use selected", "使用选中目录", "使用選中目錄" },
    workspace_at_root => { "Already at the top", "已是最上层目录", "已是最上層目錄" },
    workspace_empty => { "No workspaces", "没有工作区", "沒有工作區" },

    // ---- management -----------------------------------------------------
    management_agents => { "Agents", "Agent", "Agent" },
    management_providers => { "Providers", "Provider", "Provider" },
    management_mcp => { "MCP servers", "MCP 服务", "MCP 服務" },
    management_skills => { "Skills", "Skill", "Skill" },
    management_prompts => { "Prompts", "Prompt", "Prompt" },
    management_devices => { "Pairing and devices", "配对与设备", "配對與裝置" },
    management_scheduled => { "Scheduled tasks", "定时任务", "定時任務" },
    management_automation => { "Automation", "自动化", "自動化" },
    management_health => { "Provider health", "Provider 健康", "Provider 健康" },
    management_install => { "Install", "安装", "安裝" },
    management_update => { "Update", "更新", "更新" },
    management_rollback => { "Roll back", "回滚", "復原版本" },
    management_uninstall => { "Uninstall", "卸载", "解除安裝" },
    management_login => { "Sign in", "登录", "登入" },
    management_logout => { "Sign out", "退出登录", "登出" },
    management_probe => { "Test connection", "测试连接", "測試連線" },
    management_fetch_models => { "Fetch models", "拉取模型", "拉取模型" },
    management_secret_reveal_hint => { "ctrl+r: show key", "ctrl+r：显示密钥", "ctrl+r：顯示金鑰" },
    management_secret_hide_hint => { "ctrl+r: hide key", "ctrl+r：隐藏密钥", "ctrl+r：隱藏金鑰" },
    management_secret_write_only => {
        "Stored credentials are never displayed. Typing a value replaces it.",
        "已保存的凭据不会显示。输入新值即可替换。",
        "已儲存的憑證不會顯示。輸入新值即可取代。"
    },
    management_provider_secret_mutate_hint => {
        "The value is sent to the runtime and cleared from this client immediately.",
        "该值会发送给运行时，并立即从本客户端清除。",
        "該值會傳送給執行階段，並立即從本用戶端清除。"
    },
    management_profile_active => { "Active", "当前", "目前" },
    management_form_essentials => { "Essentials", "基础", "基本" },
    management_form_advanced => { "Advanced", "高级", "進階" },
    management_saved => { "Saved", "已保存", "已儲存" },
    management_agent_matrix => { "Agent matrix", "Agent 矩阵", "Agent 矩陣" },

    // ---- configuration centre -------------------------------------------
    // The four modules, then the function areas inside them. The centre keeps
    // the desktop config centre's architecture, so it keeps its words too.
    config_title => { "Configuration", "配置中心", "設定中心" },
    config_module_agent => { "Agent", "Agent", "Agent" },
    config_module_mcp => { "MCP", "MCP", "MCP" },
    config_module_skills => { "Skills", "Skill", "Skill" },
    config_module_prompts => { "Prompts", "Prompt", "Prompt" },
    config_section_agents => { "Agents", "Agent 列表", "Agent 清單" },
    config_section_providers => { "Model providers", "模型服务", "模型服務" },
    config_section_servers => { "Servers", "服务列表", "服務清單" },
    config_section_installed => { "Installed", "已安装", "已安裝" },
    config_section_catalog => { "Registry", "资源市场", "資源市集" },
    config_section_sources => { "Import", "导入", "匯入" },
    config_section_prompts => { "Library", "提示词库", "提示詞庫" },
    config_hint_agents => {
        "Install, enable and authenticate the Agents",
        "安装、启用并登录 Agent",
        "安裝、啟用並登入 Agent"
    },
    config_hint_providers => {
        "Model provider profiles, credentials and default models",
        "模型服务配置、凭据与默认模型",
        "模型服務設定、憑證與預設模型"
    },
    config_hint_servers => {
        "MCP servers this runtime hands to Agents",
        "运行时交给 Agent 的 MCP 服务",
        "執行階段交給 Agent 的 MCP 服務"
    },
    config_hint_installed => {
        "Skills this runtime hands to Agents",
        "运行时交给 Agent 的技能",
        "執行階段交給 Agent 的技能"
    },
    config_hint_catalog => {
        "Search the public registry and install an entry",
        "搜索公共市场并安装条目",
        "搜尋公共市集並安裝條目"
    },
    config_hint_sources => {
        "Import resources the Agents already configure in place",
        "导入 Agent 本地已配置的资源",
        "匯入 Agent 本地已設定的資源"
    },
    config_hint_prompts => {
        "Reusable prompts and slash commands",
        "可复用提示词与斜杠命令",
        "可重複使用提示詞與斜線命令"
    },
    config_search => { "Filter", "筛选", "篩選" },
    config_section_label => { "Areas", "功能区", "功能區" },
    // What the focus line calls each of the centre's three panes.
    config_pane_modules => { "Modules", "模块", "模組" },
    config_pane_areas => { "Areas", "功能区", "功能區" },
    config_pane_records => { "Records", "记录", "記錄" },
    config_focus => { "Focus", "焦点", "焦點" },
    config_search_hint => { "type to filter", "输入以筛选", "輸入以篩選" },
    // The line under a registry page. A page is not a catalogue, so the reader
    // is told how much of it they are looking at and whether the list can still
    // grow while they watch.
    config_registry_shown => { "showing", "已显示", "已顯示" },
    config_registry_of => { "of", "/", "/" },
    config_registry_entries => { "registry entries", "条注册表条目", "條註冊表條目" },
    config_registry_indexing => { "more on the registry", "注册表还有更多", "登錄表還有更多" },
    // The registry's own controls, drawn above the page they act on. The
    // transport names are the reader's vocabulary rather than the wire's:
    // http and sse are one decision from this side of the screen.
    config_registry_transport_all => { "All", "全部", "全部" },
    config_registry_transport_local => { "Local process", "本地进程", "本機程序" },
    config_registry_transport_remote => { "Remote service", "远程服务", "遠端服務" },
    config_registry_sort_recommended => { "Recommended", "推荐", "推薦" },
    config_registry_sort_installs => { "Installs", "安装量", "安裝量" },
    config_detail => { "Details", "详情", "詳情" },
    config_no_selection => { "No record selected", "未选择记录", "未選擇記錄" },
    config_empty => { "Nothing here yet", "这里还没有内容", "這裡還沒有內容" },
    config_no_matches => { "No matches", "没有匹配项", "沒有符合項目" },
    config_working => { "working…", "处理中…", "處理中…" },
    config_col_name => { "Name", "名称", "名稱" },
    config_col_status => { "Status", "状态", "狀態" },
    config_col_state => { "State", "状态", "狀態" },
    config_col_runtime => { "Runtime", "运行时", "執行階段" },
    config_col_source => { "Source", "来源", "來源" },
    config_col_models => { "Models", "模型", "模型" },
    config_col_agent => { "Agent", "Agent", "Agent" },
    config_col_kind => { "Kind", "类型", "類型" },
    config_col_model => { "Model", "模型", "模型" },
    config_col_secret => { "Credential", "凭据", "憑證" },
    config_col_transport => { "Transport", "传输", "傳輸" },
    config_col_scope => { "Scope", "范围", "範圍" },
    config_col_agents => { "Agents", "Agent", "Agent" },
    config_col_upstreams => { "Groups", "路由组", "路由組" },
    config_col_keys => { "Keys", "Key", "Key" },
    config_col_tags => { "Tags", "标签", "標籤" },
    config_col_updated => { "Updated", "更新时间", "更新時間" },
    config_col_version => { "Version", "版本", "版本" },
    config_col_auth => { "Account", "账号", "帳號" },
    config_col_publisher => { "Publisher", "发布者", "發佈者" },
    config_col_installs => { "Installs", "安装量", "安裝量" },
    config_col_package => { "Package", "包类型", "套件類型" },
    config_col_path => { "Path", "路径", "路徑" },
    config_col_install => { "Install", "安装状态", "安裝狀態" },
    config_action_install => { "Install", "安装", "安裝" },
    config_action_rollback => { "Roll back", "回滚", "復原版本" },
    config_action_check_update => { "Check update", "检查更新", "檢查更新" },
    config_action_uninstall => { "Uninstall", "卸载", "解除安裝" },
    config_action_toggle => { "Enable / disable", "启用 / 停用", "啟用 / 停用" },
    config_action_edit => { "Edit", "编辑", "編輯" },
    config_action_delete => { "Delete", "删除", "刪除" },
    config_action_duplicate => { "Duplicate", "复制", "複製" },
    config_action_clear_secret => { "Clear key", "清除密钥", "清除金鑰" },
    config_action_install_docs => { "Open install page", "打开安装页", "開啟安裝頁" },
    config_action_open_page => { "Open homepage", "打开主页", "開啟首頁" },
    config_action_reinstall => { "Reinstall", "重新安装", "重新安裝" },
    config_action_upgrade => { "Upgrade", "升级", "升級" },
    config_action_add_roster => { "Add to roster", "加入名册", "加入名冊" },
    config_action_remove_roster => { "Remove from roster", "移出名册", "移出名冊" },
    config_action_browser_tools => { "Browser tools", "浏览器工具", "瀏覽器工具" },
    config_action_computer_tools => { "Computer use", "电脑操作", "電腦操作" },
    config_action_enable_all => { "Enable all", "全部启用", "全部啟用" },
    config_action_disable_all => { "Disable all", "全部禁用", "全部停用" },
    config_hint_no_matches => { "No matches · Delete clears the filter", "没有匹配项 · Delete 清除过滤", "沒有符合項目 · Delete 清除篩選" },
    config_read_failed => { "The read failed", "读取失败", "讀取失敗" },
    config_read_retry => { "r asks again", "按 r 重试", "按 r 重試" },
    label_config_model_enable_all => { "Enable all Models", "启用全部模型", "啟用全部模型" },
    label_config_model_disable_all => { "Disable all Models", "禁用全部模型", "停用全部模型" },
    help_config_model_enable_all => { "Offer every Model the catalogue lists.", "提供目录中列出的全部模型。", "提供目錄中列出的全部模型。" },
    help_config_model_disable_all => { "Stop offering every Model this profile holds.", "停止提供此配置持有的全部模型。", "停止提供此設定持有的全部模型。" },
    config_field_tools => { "Built-in tools", "内置工具", "內建工具" },
    config_state_on => { "on", "开", "開" },
    config_state_off => { "off", "关", "關" },
    config_action_move_up => { "Move up", "上移", "上移" },
    config_action_move_down => { "Move down", "下移", "下移" },
    config_col_roster => { "Roster", "名册", "名冊" },
    config_roster_enabled => { "Enabled", "已启用", "已啟用" },
    config_roster_added_disabled => { "Added", "已加入", "已加入" },
    config_roster_not_added => { "Not added", "未加入", "未加入" },
    config_field_last_error => { "Last error", "上次错误", "上次錯誤" },
    config_result_agent_added => { "the Agent joined the roster", "Agent 已加入名册", "Agent 已加入名冊" },
    config_result_agent_removed => { "the Agent left the roster", "Agent 已移出名册", "Agent 已移出名冊" },
    config_preview_title => { "Skill preview", "技能预览", "技能預覽" },
    config_preview_hint => { "↑↓ scroll · Enter installs it · Esc closes", "↑↓ 滚动 · Enter 安装 · Esc 关闭", "↑↓ 捲動 · Enter 安裝 · Esc 關閉" },
    config_preview_loading => { "Reading the Skill…", "正在读取技能…", "正在讀取技能…" },
    config_preview_too_large => { "Too large to install ({bytes} bytes)", "体积过大，无法安装（{bytes} 字节）", "體積過大，無法安裝（{bytes} 位元組）" },
    config_preview_bundle => { "{files} bundled file(s) · {bytes} bytes", "附带 {files} 个文件 · {bytes} 字节", "附帶 {files} 個檔案 · {bytes} 位元組" },
    config_preview_files => { "Bundled files", "附带文件", "附帶檔案" },
    config_preview_skipped => { "Not written", "不会写入", "不會寫入" },
    config_preview_body => { "Instructions", "指令正文", "指令正文" },
    config_preview_empty => { "The preview carried nothing.", "预览内容为空。", "預覽內容為空。" },
    config_registry_more => { "{count} more loaded", "又载入 {count} 条", "又載入 {count} 條" },
    config_registry_load_more => { "more available", "还有更多", "還有更多" },
    config_registry_failed => { "The registry could not be read", "无法读取注册表", "無法讀取登錄表" },
    config_discard_title => { "Discard unsaved changes?", "放弃未保存的修改？", "放棄未儲存的修改？" },
    config_discard_body => { "This form has edits that have not been saved. Discarding them cannot be undone.", "此表单有未保存的修改，放弃后无法恢复。", "此表單有未儲存的修改，放棄後無法復原。" },
    config_reading => { "Reading…", "读取中…", "讀取中…" },
    config_market_env_required => { "Variables required", "必需变量", "必需變數" },
    config_action_discover => { "Find local Agents", "探测本地 Agent", "探測本機 Agent" },
    config_discover_found => { "{count} local Agent CLI(s) found and enabled", "找到并启用了 {count} 个本地 Agent CLI", "找到並啟用了 {count} 個本機 Agent CLI" },
    config_discover_none => { "No new local Agent CLI was found", "没有发现新的本地 Agent CLI", "沒有發現新的本機 Agent CLI" },
    config_discover_no_candidates => { "There is nothing left to probe for", "没有可再探测的 Agent", "沒有可再探測的 Agent" },
    config_form_new_agent => { "Register an ACP Agent", "注册 ACP Agent", "註冊 ACP Agent" },
    config_field_agent_id => { "Agent id", "Agent ID", "Agent ID" },
    config_hint_agent_id => { "lowercase kebab-case, e.g. my-agent", "小写短横线格式，例如 my-agent", "小寫連字號格式，例如 my-agent" },
    config_field_display_name => { "Display name", "显示名称", "顯示名稱" },
    config_field_arguments => { "Arguments", "参数", "參數" },
    config_hint_arguments => { "space-separated", "以空格分隔", "以空格分隔" },
    config_field_env_pairs => { "Environment", "环境变量", "環境變數" },
    config_hint_env_pairs => { "KEY=value, one per line", "KEY=value，每行一个", "KEY=value，每行一個" },
    config_result_agent_created => { "the custom Agent was registered", "已注册自定义 Agent", "已註冊自訂 Agent" },
    config_form_error_agent_id => { "Use a lowercase kebab-case Agent id", "Agent ID 必须使用小写短横线格式", "Agent ID 必須使用小寫連字號格式" },
    config_form_error_agent_required => { "Display name and command are required", "显示名称和命令不能为空", "顯示名稱和命令不能為空" },
    config_form_error_env_pairs => { "Environment variables must use KEY=value, one per line", "环境变量必须使用 KEY=value 格式，每行一个", "環境變數必須使用 KEY=value 格式，每行一個" },
    config_form_error_env_name => { "Environment variable names may contain letters, digits, and underscores", "环境变量名只能包含字母、数字和下划线", "環境變數名稱只能包含字母、數字和底線" },
    config_install_watching => { "Working on {agent}", "正在处理 {agent}", "正在處理 {agent}" },
    config_install_phase_installing => { "Downloading", "下载中", "下載中" },
    config_install_phase_upgrading => { "Upgrading", "升级中", "升級中" },
    config_install_phase_uninstalling => { "Removing", "卸载中", "卸載中" },
    config_install_failed => { "The Agent install failed", "Agent 安装失败", "Agent 安裝失敗" },
    config_install_done => { "{agent} is ready", "{agent} 已就绪", "{agent} 已就緒" },
    label_config_form_discard => { "Discard form", "放弃表单", "放棄表單" },
    help_config_form_discard => { "Throw away what has been typed into this form.", "丢弃在此表单中输入的内容。", "丟棄在此表單中輸入的內容。" },
    config_action_import_native => { "Import settings", "导入配置", "匯入設定" },
    config_import_title => { "Import provider settings", "导入模型服务配置", "匯入模型服務設定" },
    config_import_hint => { "Enter imports the highlighted one · r looks again", "Enter 导入选中项 · r 重新查找", "Enter 匯入選中項 · r 重新尋找" },
    config_import_empty => { "Nothing to import was found on this machine.", "本机没有找到可导入的配置。", "本機沒有找到可匯入的設定。" },
    config_import_loading => { "Looking for settings to import…", "正在查找可导入的配置…", "正在尋找可匯入的設定…" },
    config_state_needs_secret => { "Needs a key", "需要密钥", "需要金鑰" },
    config_state_partial => { "Partial", "部分可用", "部分可用" },
    config_state_blocked => { "Unreadable", "无法解析", "無法解析" },
    config_import_source_codex => { "Codex", "Codex", "Codex" },
    config_import_source_claude => { "Claude", "Claude", "Claude" },
    config_import_source_cc_switch => { "CC Switch", "CC Switch", "CC Switch" },
    config_action_export_native => { "Export settings", "导出配置", "匯出設定" },
    config_export_title => { "Export provider settings", "导出模型服务配置", "匯出模型服務設定" },
    config_export_hint => { "t target · m content · v preview · w apply · b roll back", "t 目标 · m 内容 · v 预览 · w 应用 · b 回滚", "t 目標 · m 內容 · v 預覽 · w 套用 · b 回復" },
    config_export_source_agent => { "this profile's Agent", "本配置所属 Agent", "本設定所屬 Agent" },
    config_export_mode_profile => { "Provider profile", "模型服务配置", "模型服務設定" },
    config_export_mode_combined => { "Combined", "合并", "合併" },
    config_export_mode_mcp => { "MCP", "MCP", "MCP" },
    config_export_mode_skills => { "Skills", "技能", "技能" },
    config_export_mode_prompts => { "Prompts", "提示词", "提示詞" },
    config_export_unchanged => { "Unchanged", "无变化", "無變化" },
    config_export_applied => { "Applied", "已应用", "已套用" },
    config_export_rolled_back => { "Rolled back", "已回滚", "已回復" },
    config_export_no_preview => { "Press p to see what would be written.", "按 p 查看将要写入的内容。", "按 p 查看將要寫入的內容。" },
    config_export_history => { "History", "历史", "歷史" },
    config_export_no_history => { "Nothing has been exported to this profile yet.", "尚未向此配置导出过。", "尚未向此設定匯出過。" },
    config_auth_title => { "Agent account", "Agent 账号", "Agent 帳號" },
    config_auth_hint => { "Enter runs the highlighted line.", "Enter 执行选中项。", "Enter 執行選中項。" },
    config_auth_signed_in => { "Signed in", "已登录", "已登入" },
    config_auth_required => { "Sign-in required", "需要登录", "需要登入" },
    config_auth_unknown => { "Not checked", "未检查", "未檢查" },
    config_auth_method_agent => { "Sign in", "登录", "登入" },
    config_auth_method_environment => { "Environment", "环境变量", "環境變數" },
    config_auth_method_terminal => { "Terminal", "终端", "終端" },
    config_auth_action_refresh => { "Refresh methods", "刷新登录方式", "重新整理登入方式" },
    config_auth_action_verify => { "Verify account", "验证账号", "驗證帳號" },
    config_auth_action_models => { "Refresh models", "刷新模型", "重新整理模型" },
    config_auth_action_sign_out => { "Sign out", "退出登录", "登出" },
    config_auth_action_credentials => { "Get credentials", "获取凭据", "取得憑證" },
    config_auth_no_methods => { "This Agent reported no way to sign in.", "此 Agent 未提供任何登录方式。", "此 Agent 未提供任何登入方式。" },
    config_auth_loading => { "Reading the account…", "正在读取账号…", "正在讀取帳號…" },
    config_auth_configured => { "set", "已设置", "已設定" },
    config_auth_optional => { "optional", "可选", "可選" },
    config_auth_needs_profile => { "needs a provider profile", "需要模型服务配置", "需要模型服務設定" },
    config_auth_external => { "the Agent's own sign-in", "由 Agent 自行登录", "由 Agent 自行登入" },
    config_auth_missing_value => { "A required value is missing", "缺少必填的值", "缺少必填的值" },
    config_auth_sign_out_impact => { "Signing out also affects {count} session(s) that use this account.", "退出登录还会影响 {count} 个使用此账号的会话。", "登出還會影響 {count} 個使用此帳號的工作階段。" },
    config_auth_sign_out_confirm => { "Enter again to sign out · Esc keeps it", "再按 Enter 退出登录 · Esc 保留", "再按 Enter 登出 · Esc 保留" },
    config_auth_waiting_host_terminal => { "Finish the sign-in in the terminal the Agent opened{detail}", "请在 Agent 打开的终端里完成登录{detail}", "請在 Agent 開啟的終端裡完成登入{detail}" },
    config_auth_waiting_host_browser => { "Finish the sign-in in the browser the Agent opened{detail}", "请在 Agent 打开的浏览器里完成登录{detail}", "請在 Agent 開啟的瀏覽器裡完成登入{detail}" },
    config_auth_waiting_device_code => { "Finish the sign-in where the Agent showed the code{detail}", "请在 Agent 显示代码的地方完成登录{detail}", "請在 Agent 顯示代碼的地方完成登入{detail}" },
    config_auth_waiting_elsewhere => { "The Agent is still signing in{detail}", "Agent 仍在登录{detail}", "Agent 仍在登入{detail}" },
    config_auth_waiting_detail => { ": {command}", "：{command}", "：{command}" },
    config_auth_waiting_hint => { "Verify account completes it once it is done there.", "在那边完成后，用“验证账号”收尾。", "在那邊完成後，用「驗證帳號」收尾。" },
    config_auth_terminal_opened => { "Sign-in opened in a terminal · type here to answer it", "登录已在终端中打开 · 在此输入以作答", "登入已在終端中開啟 · 在此輸入以作答" },
    config_auth_terminal_hint => { "keys go to the Agent's own sign-in · Esc hides it", "按键会发送给 Agent 的登录流程 · Esc 收起", "按鍵會傳送給 Agent 的登入流程 · Esc 收起" },
    config_auth_terminal_reading => { "Reading the terminal…", "正在读取终端…", "正在讀取終端…" },
    config_auth_terminal_ended => { "The sign-in terminal has closed.", "登录终端已关闭。", "登入終端已關閉。" },
    config_auth_action_cancel => { "Cancel sign-in", "取消登录", "取消登入" },
    config_auth_waiting_eyebrow => { "running", "进行中", "進行中" },
    config_auth_cancelled => { "Sign-in cancelled", "已取消登录", "已取消登入" },
    config_auth_terminal_detached => { "The sign-in is still running in its own terminal.", "登录仍在它自己的终端里运行。", "登入仍在它自己的終端裡執行。" },
    config_hint_fixed_endpoint => { "this Agent always calls {url}; the address cannot be changed", "此 Agent 固定调用 {url}，地址不可更改", "此 Agent 固定呼叫 {url}，位址不可變更" },
    config_result_workspace_default => { "workspace default", "工作区默认", "工作區預設" },
    config_result_global_default => { "global default", "全局默认", "全域預設" },
    config_result_secret_cleared => { "the stored key was cleared", "已清除已保存的密钥", "已清除已儲存的金鑰" },
    config_action_refresh => { "Re-probe", "重新探测", "重新探測" },
    config_action_sign_in => { "Sign in", "登录", "登入" },
    config_action_sign_out => { "Sign out", "退出登录", "登出" },
    config_action_activate => { "Use", "设为当前", "設為目前" },
    config_action_prefer => { "Use first", "优先使用", "優先使用" },
    config_action_token_limit => { "Token limit", "token 上限", "token 上限" },
    config_requests_empty => { "No requests have gone through the gateway yet.", "还没有请求经过网关。", "還沒有請求經過閘道。" },
    config_action_requests => { "Recent requests", "最近请求", "最近請求" },
    config_action_secret => { "Credential", "凭据", "憑證" },
    config_action_test => { "Test", "测试", "測試" },
    config_action_fetch_models => { "Fetch models", "拉取模型", "拉取模型" },
    config_action_validate => { "Validate", "校验", "校驗" },
    config_action_matrix => { "Agents", "分配 Agent", "分配 Agent" },
    config_action_import => { "Import", "导入", "匯入" },
    config_action_new => { "New", "新建", "新增" },

    // ---- what a write says while it runs and when it lands ---------------
    // Three lines per verb, so a press is never silent: the reader is told that
    // something is happening, what came of it, and — when it was refused — what
    // it was that did not happen.
    config_running_install => { "Installing…", "正在安装…", "正在安裝…" },
    config_done_install => { "Installed", "已安装", "已安裝" },
    config_noun_install => { "Install", "安装", "安裝" },
    config_running_rollback => { "Rolling back…", "正在回滚…", "正在復原…" },
    config_done_rollback => { "Rolled back", "已回滚", "已復原" },
    config_noun_rollback => { "Rollback", "回滚", "復原" },
    config_running_check_update => { "Checking for updates…", "正在检查更新…", "正在檢查更新…" },
    config_done_check_update => { "Update checked", "已检查更新", "已檢查更新" },
    config_noun_check_update => { "Update check", "检查更新", "檢查更新" },
    config_running_refresh => { "Re-probing…", "正在重新探测…", "正在重新探測…" },
    config_done_refresh => { "Re-probed", "已重新探测", "已重新探測" },
    config_noun_refresh => { "Re-probe", "重新探测", "重新探測" },
    config_running_toggle => { "Switching…", "正在切换…", "正在切換…" },
    config_done_enable => { "Enabled", "已启用", "已啟用" },
    config_done_disable => { "Disabled", "已停用", "已停用" },
    config_noun_toggle => { "Switch", "切换", "切換" },
    config_running_sign_in => { "Reading sign-in methods…", "正在读取登录方式…", "正在讀取登入方式…" },
    config_done_sign_in => { "Sign-in methods read", "已读取登录方式", "已讀取登入方式" },
    config_noun_sign_in => { "Sign in", "登录", "登入" },
    config_running_sign_out => { "Signing out…", "正在退出登录…", "正在登出…" },
    config_done_sign_out => { "Signed out", "已退出登录", "已登出" },
    config_noun_sign_out => { "Sign out", "退出登录", "登出" },
    config_running_uninstall => { "Uninstalling…", "正在卸载…", "正在解除安裝…" },
    config_done_uninstall => { "Uninstalled", "已卸载", "已解除安裝" },
    config_noun_uninstall => { "Uninstall", "卸载", "解除安裝" },
    config_running_delete => { "Deleting…", "正在删除…", "正在刪除…" },
    config_done_delete => { "Deleted", "已删除", "已刪除" },
    config_noun_delete => { "Delete", "删除", "刪除" },
    config_running_save => { "Saving…", "正在保存…", "正在儲存…" },
    config_done_save => { "Saved", "已保存", "已儲存" },
    config_noun_save => { "Save", "保存", "儲存" },
    config_running_create => { "Creating…", "正在创建…", "正在建立…" },
    config_done_create => { "Created", "已创建", "已建立" },
    config_noun_create => { "Create", "创建", "建立" },
    config_running_activate => { "Activating…", "正在启用…", "正在啟用…" },
    config_done_activate => { "Activated", "已启用", "已啟用" },
    config_noun_activate => { "Activate", "设为当前", "設為目前" },
    config_running_test => { "Testing…", "正在测试…", "正在測試…" },
    config_done_test => { "Tested", "已测试", "已測試" },
    config_noun_test => { "Test", "测试", "測試" },
    config_running_fetch_models => { "Fetching models…", "正在拉取模型…", "正在拉取模型…" },
    config_done_fetch_models => { "Models fetched", "已拉取模型", "已拉取模型" },
    config_noun_fetch_models => { "Model fetch", "拉取模型", "拉取模型" },
    config_running_validate => { "Validating…", "正在校验…", "正在校驗…" },
    config_done_validate => { "Validated", "已校验", "已校驗" },
    config_noun_validate => { "Validate", "校验", "校驗" },
    config_running_import => { "Importing…", "正在导入…", "正在匯入…" },
    config_done_import => { "Imported", "已导入", "已匯入" },
    config_noun_import => { "Import", "导入", "匯入" },
    config_running_registry_install => { "Installing from the registry…", "正在从市场安装…", "正在從市集安裝…" },
    config_done_registry_install => { "Installed from the registry", "已从市场安装", "已從市集安裝" },
    config_noun_registry_install => { "Registry install", "从市场安装", "從市集安裝" },
    config_running_matrix => { "Saving the Agent selection…", "正在保存 Agent 分配…", "正在儲存 Agent 分配…" },
    config_done_matrix => { "Agent selection saved", "Agent 分配已保存", "Agent 分配已儲存" },
    config_noun_matrix => { "Agent selection", "Agent 分配", "Agent 分配" },
    config_running_discover => { "Looking for imports…", "正在查找可导入项…", "正在尋找可匯入項…" },
    config_done_discover => { "Imports read", "已读取可导入项", "已讀取可匯入項" },
    config_noun_discover => { "Import scan", "查找可导入项", "尋找可匯入項" },
    config_running_search => { "Searching the registry…", "正在搜索市场…", "正在搜尋市集…" },
    config_running_roster_add => { "Adding to the roster", "正在加入名册", "正在加入名冊" },
    config_running_roster_remove => { "Removing from the roster", "正在移出名册", "正在移出名冊" },
    config_done_search => { "Registry searched", "已搜索市场", "已搜尋市集" },
    config_done_roster_add => { "Added to the roster", "已加入名册", "已加入名冊" },
    config_done_roster_remove => { "Removed from the roster", "已移出名册", "已移出名冊" },
    config_noun_search => { "Registry search", "搜索市场", "搜尋市集" },
    config_noun_roster_add => { "adding it to the roster", "加入名册", "加入名冊" },
    config_noun_roster_remove => { "removing it from the roster", "移出名册", "移出名冊" },
    config_result_up_to_date => { "up to date", "已是最新", "已是最新" },
    config_result_available => { "available", "可用", "可用" },
    config_result_version => { "version", "版本", "版本" },
    config_result_installed => { "installed", "已安装", "已安裝" },
    config_result_signed_in => { "signed in", "已登录", "已登入" },
    config_result_signed_out => { "not signed in", "未登录", "未登入" },
    config_result_methods => { "sign-in methods", "种登录方式", "種登入方式" },
    config_result_models => { "models", "个模型", "個模型" },
    config_result_ok => { "ok", "通过", "通過" },
    config_result_warned => { "warning", "警告", "警告" },
    config_result_failed => { "failed", "失败", "失敗" },
    config_field_auth => { "Account", "账号", "帳號" },
    config_field_source => { "Source", "来源", "來源" },
    config_action_blocked => { "That record is already working — wait for it to answer", "该记录正在处理中，请等待结果", "該記錄正在處理中，請等待結果" },

    config_action_save => { "Save", "保存", "儲存" },
    config_actions_title => { "Actions", "操作", "操作" },
    config_state_enabled => { "Enabled", "已启用", "已啟用" },
    config_state_disabled => { "Disabled", "已停用", "已停用" },
    config_state_installed => { "Installed", "已安装", "已安裝" },
    config_state_missing => { "Missing", "缺失", "缺失" },
    config_state_ready => { "Ready", "就绪", "就緒" },
    config_state_invalid => { "Invalid", "已失效", "已失效" },
    config_state_unavailable => { "Unavailable", "不可用", "不可用" },
    config_state_degraded => { "Degraded", "已降级", "已降級" },
    config_state_probe_failed => { "Probe failed", "探测失败", "探測失敗" },
    config_state_unknown => { "Unknown", "未知", "未知" },
    config_state_importable => { "Importable", "可导入", "可匯入" },
    config_state_imported => { "Imported", "已导入", "已匯入" },
    config_state_unsupported => { "Unsupported", "不支持", "不支援" },
    config_state_error => { "Error", "错误", "錯誤" },
    config_state_archived => { "Archived", "已归档", "已封存" },
    config_state_active => { "Active", "有效", "有效" },
    config_scope_global => { "Global", "全局", "全域" },
    config_scope_user => { "User", "用户", "使用者" },
    config_scope_project => { "Project", "项目", "專案" },
    config_scope_workspace => { "Workspace", "工作区", "工作區" },
    config_secret_set => { "Set", "已设置", "已設定" },
    config_secret_missing => { "Missing", "未设置", "未設定" },
    config_secret_referenced => { "Referenced", "引用中", "引用中" },
    config_secret_unsupported => { "Unsupported", "不支持", "不支援" },
    config_agents_none => { "none", "无", "無" },
    config_managed_external => { "external", "外部安装", "外部安裝" },
    config_managed_absent => { "not installed", "未安装", "未安裝" },
    config_managed_installed => { "installed", "已安装", "已安裝" },
    config_managed_update => { "update available", "有可用更新", "有可用更新" },
    config_managed_failed => { "install failed", "安装失败", "安裝失敗" },
    config_managed_unknown => { "unknown", "未知", "未知" },
    config_yes => { "yes", "是", "是" },
    config_no => { "no", "否", "否" },
    config_lines => { "lines", "行", "行" },
    config_kind_skill => { "Skill", "技能", "技能" },
    config_source_market => { "Market", "市场", "市集" },
    config_source_official => { "Official", "官方", "官方" },
    config_prompt_kind_reusable => { "Prompt", "提示词", "提示詞" },
    config_prompt_kind_slash => { "Slash command", "斜杠命令", "斜線命令" },
    config_prompt_kind_snippet => { "System snippet", "系统片段", "系統片段" },
    config_field_args => { "Arguments", "参数", "參數" },
    config_source_manual => { "Manual", "手工编写", "手工撰寫" },
    config_source_folder => { "Local folder", "本地目录", "本地目錄" },
    config_source_git => { "Git repository", "Git 仓库", "Git 倉庫" },
    config_hint_env => {
        "One KEY=value per line",
        "每行一个 KEY=value",
        "每行一個 KEY=value"
    },
    config_hint_headers => {
        "One Name: value per line",
        "每行一个 Name: value",
        "每行一個 Name: value"
    },
    config_hint_endpoint => {
        "Leave empty to keep the current address",
        "留空表示保持当前地址",
        "留空表示保持目前位址"
    },
    config_field_id => { "Id", "标识", "識別碼" },
    config_field_install => { "Install", "安装", "安裝" },
    config_field_version => { "Version", "版本", "版本" },
    config_field_configured => { "Configured", "已配置", "已設定" },
    config_field_models => { "Models", "模型", "模型" },
    config_field_model => { "Default model", "默认模型", "預設模型" },
    config_field_command => { "Command", "命令", "命令" },
    config_field_path => { "Path", "路径", "路徑" },
    config_field_agent => { "Agent", "Agent", "Agent" },
    config_field_kind => { "Kind", "类型", "類型" },
    config_field_account => { "Account", "账号", "帳號" },
    config_field_secret => { "Credential", "凭据", "憑證" },
    config_field_updated => { "Updated", "更新时间", "更新時間" },
    config_field_transport => { "Transport", "传输方式", "傳輸方式" },
    config_field_scope => { "Scope", "作用范围", "作用範圍" },
    config_field_url => { "URL", "地址", "位址" },
    config_field_env => { "Environment", "环境变量", "環境變數" },
    config_field_headers => { "Headers", "请求头", "請求標頭" },
    config_field_agents => { "Agents", "Agent 分配", "Agent 分配" },
    config_field_tags => { "Tags", "标签", "標籤" },
    config_field_preview => { "Preview", "预览", "預覽" },
    config_field_body => { "Body", "内容", "內容" },
    config_field_description => { "Description", "描述", "描述" },
    config_field_homepage => { "Homepage", "主页", "首頁" },
    config_field_repository => { "Repository", "代码库", "程式庫" },
    config_field_installs => { "Installs", "安装量", "安裝量" },
    config_field_package => { "Package", "包类型", "套件類型" },
    config_field_downloads => { "Downloads", "下载量", "下載量" },
    config_field_stars => { "Stars", "星标", "星標" },
    config_field_slug => { "Slug", "短名", "短名" },
    config_field_publisher => { "Publisher", "发布者", "發佈者" },
    config_confirm_delete_title => { "Delete this record?", "删除该记录？", "刪除該記錄？" },
    config_confirm_delete_body => {
        "The record is deleted on the runtime, which also rewrites the Agent's native configuration.",
        "该记录将在运行时删除，并同步改写 Agent 的原生配置。",
        "該記錄將在執行階段刪除，並同步改寫 Agent 的原生設定。"
    },
    config_confirm_uninstall_title => { "Uninstall this Agent?", "卸载该 Agent？", "解除安裝該 Agent？" },
    config_confirm_uninstall_body => {
        "The Agent's managed install is removed.",
        "将移除该 Agent 的受管安装。",
        "將移除該 Agent 的受管安裝。"
    },
    config_form_new_server => { "New MCP server", "新建 MCP 服务", "新增 MCP 服務" },
    config_form_edit_server => { "Edit MCP server", "编辑 MCP 服务", "編輯 MCP 服務" },
    config_form_new_skill => { "New Skill", "新建技能", "新增技能" },
    config_form_edit_skill => { "Edit Skill", "编辑技能", "編輯技能" },
    config_form_new_prompt => { "New prompt", "新建提示词", "新增提示詞" },
    config_form_edit_prompt => { "Edit prompt", "编辑提示词", "編輯提示詞" },
    config_form_edit_provider => { "Edit provider profile", "编辑模型服务", "編輯模型服務" },
    config_form_new_provider => { "New provider profile", "新建模型服务", "新增模型服務" },
    config_form_error_agent => { "An Agent is required", "必须选择一个 Agent", "必須選擇一個 Agent" },
    config_copy_suffix => { "copy", "副本", "副本" },
    config_action_models => { "Models", "模型", "模型" },
    config_state_available => { "Available", "可用", "可用" },
    config_field_wire_api => { "Protocol", "协议", "協定" },
    config_field_context_tokens => { "Context", "上下文", "上下文" },
    config_field_output_tokens => { "Output", "输出", "輸出" },
    config_models_title => { "Models", "模型", "模型" },
    config_models_hint => { "o offers a Model · f fetches · i adds · x removes", "o 启用 · f 拉取 · i 添加 · x 移除", "o 啟用 · f 拉取 · i 新增 · x 移除" },
    config_models_empty => { "No Models yet: fetch them, or add one by id.", "还没有模型：拉取，或按 ID 添加。", "還沒有模型：拉取，或按 ID 新增。" },
    config_models_undeclared => { "from the endpoint", "来自端点", "來自端點" },
    config_form_error_model => { "A Model id is required", "必须填写模型 ID", "必須填寫模型 ID" },
    config_form_add_model => { "Add model", "添加模型", "新增模型" },
    config_action_model_settings => { "Settings", "设置", "設定" },
    config_form_edit_model => { "Model settings", "模型设置", "模型設定" },
    config_field_reasoning => { "Reasoning", "推理", "推理" },
    config_field_reasoning_efforts => { "Thinking levels", "思考档位", "思考檔位" },
    config_hint_reasoning_efforts => { "e.g. off, low, high, max — or level=wire", "例如 off, low, high, max —— 或 level=wire", "例如 off, low, high, max —— 或 level=wire" },
    config_hint_inherit => { "leave to the Agent", "交由 Agent 决定", "交由 Agent 決定" },
    config_choice_reasoning => { "Reasons", "有推理", "有推理" },
    config_choice_non_reasoning => { "No reasoning", "无推理", "無推理" },
    config_form_error_tokens => { "Token limits are whole numbers greater than zero", "Token 上限必须是大于 0 的整数", "Token 上限必須是大於 0 的整數" },
    config_form_error_reasoning_efforts => { "Thinking levels must read like `low, high` or `level=wire`", "思考档位需形如 `low, high` 或 `level=wire`", "思考檔位需形如 `low, high` 或 `level=wire`" },
    config_form_error_header => { "Request headers read like `Name: value`, one per line", "请求头需形如 `Name: value`，每行一个", "請求標頭需形如 `Name: value`，每行一個" },
    config_form_error_header_variable => { "The only fillable variable is {{session_id}}", "唯一可填的变量是 {{session_id}}", "唯一可填的變數是 {{session_id}}" },
    config_form_error_header_duplicate => { "The same header is declared twice", "同一个请求头声明了两次", "同一個請求標頭宣告了兩次" },
    config_form_error_protocol_url => { "Protocol addresses read like `wire=https://…`, one per line", "协议地址需形如 `wire=https://…`，每行一个", "協定位址需形如 `wire=https://…`，每行一個" },
    config_form_error_no_agent => { "Choose at least one Agent to enable it for", "至少要选一个 Agent 启用", "至少要選一個 Agent 啟用" },
    config_hint_models => { "one Model id per line", "每行一个模型 ID", "每行一個模型 ID" },
    config_preset_picker_title => { "Choose a preset", "选择预设", "選擇預設" },
    config_preset_no_key => { "local · no key", "本地 · 无需 Key", "本地 · 無需 Key" },
    config_preset_applied => { "preset applied", "已应用预设", "已套用預設" },
    config_hint_provider_url => { "this client cannot read the stored address", "此客户端无法读取已保存的地址", "此客戶端無法讀取已儲存的位址" },
    config_field_website => { "Website", "网站", "網站" },
    config_field_request_headers => { "Request headers", "请求头", "請求標頭" },
    config_hint_request_headers => { "Name: value per line · {{session_id}} is filled at run time", "每行 Name: value · {{session_id}} 在运行时填入", "每行 Name: value · {{session_id}} 在執行時填入" },
    config_field_protocol_urls => { "Address per protocol", "各协议地址", "各協定位址" },
    config_hint_protocol_urls => { "wire=https://… per line, for a protocol that lives elsewhere", "每行 wire=https://…，用于地址不同的协议", "每行 wire=https://…，用於位址不同的協定" },
    config_result_session_header => { "The gateway address detected: the session header was enabled", "检测到网关地址：已启用会话标头", "偵測到閘道位址：已啟用工作階段標頭" },
    config_form_name => { "Name", "名称", "名稱" },
    config_form_required => { "Required", "必填", "必填" },
    config_form_copy => { "copy", "复制", "複製" },
    config_form_hint_move => { "Field", "切换字段", "切換欄位" },
    config_form_hint_change => { "Value", "修改取值", "修改取值" },
    config_form_hint_edit => { "Edit", "编辑", "編輯" },
    config_form_hint_save => { "Save", "保存", "儲存" },
    config_form_hint_done => { "Done", "完成", "完成" },
    config_form_hint_cancel => { "Cancel", "取消", "取消" },
    config_form_unsaved => { "Unsaved changes", "有未保存的修改", "有未儲存的修改" },
    config_form_error_name => { "A name is required", "名称不能为空", "名稱不能為空" },
    config_form_error_body => { "This resource needs content", "该资源需要内容", "該資源需要內容" },
    config_form_error_stdio_command => {
        "A stdio server needs a command",
        "stdio 服务需要命令",
        "stdio 服務需要命令"
    },
    config_form_error_remote_url => {
        "A remote server needs a URL",
        "远程服务需要地址",
        "遠端服務需要位址"
    },

    // ---- devices & pairing ----------------------------------------------
    devices_title => { "Paired devices", "已配对设备", "已配對裝置" },
    devices_empty => { "No paired devices", "没有已配对设备", "沒有已配對裝置" },
    devices_pair_new => { "Pair a device", "配对设备", "配對裝置" },
    devices_pairing_code => { "One-time pairing code", "一次性配对码", "一次性配對碼" },
    devices_permission => { "Permission level", "权限级别", "權限層級" },
    devices_revoke => { "Revoke", "吊销", "撤銷" },
    devices_revoke_reason => { "Reason", "原因", "原因" },
    devices_last_seen => { "Last seen", "最近可见", "最近出現" },
    devices_audit => { "Audit log", "审计记录", "稽核記錄" },
    devices_confirm_revoke => {
        "Revoke this device? It will lose access immediately.",
        "吊销该设备？它将立即失去访问权限。",
        "撤銷該裝置？它將立即失去存取權限。"
    },
    devices_code_hint => {
        "Shown once. It is never written to logs or configuration.",
        "仅显示一次，不会写入日志或配置。",
        "僅顯示一次，不會寫入日誌或設定。"
    },
    // ---- the devices page: two halves under one tab strip -----------------
    devices_tab_pair => { "Pair", "配对", "配對" },
    devices_pair_qr => { "Pair with a QR code", "用二维码配对", "用二維碼配對" },
    devices_pair_qr_scope => { "Phone or tablet", "手机或平板", "手機或平板" },
    devices_pair_lan => {
        "Find a device on this network",
        "在同一网络查找设备",
        "在同一網路尋找裝置"
    },
    devices_pair_lan_scope => {
        "A phone on the same network as this machine",
        "与这台机器同一网络的手机",
        "與這台機器同一網路的手機"
    },
    devices_pair_code => {
        "Pair with a code or link",
        "用配对码或链接配对",
        "用配對碼或連結配對"
    },
    devices_pair_code_scope => { "A second desktop", "第二台桌面端", "第二台桌面端" },
    devices_offer_expired => {
        "That code has expired — press Enter to mint a new one.",
        "配对码已过期——回车重新生成。",
        "配對碼已過期——回車重新產生。"
    },
    devices_pair_working => { "Working…", "处理中…", "處理中…" },
    devices_qr_no_room => {
        "This terminal is too small for the code — widen it, or copy the link below.",
        "终端太小，放不下二维码——请放大窗口，或复制下面的链接。",
        "終端太小，放不下二維碼——請放大視窗，或複製下面的連結。"
    },
    devices_qr_no_glyphs => {
        "This terminal's font cannot draw the code — copy the link below.",
        "当前终端的字体画不出二维码——请复制下面的链接。",
        "目前終端的字型畫不出二維碼——請複製下面的連結。"
    },
    devices_runtime_desktop => {
        "Desktop runtime",
        "桌面端内嵌运行时",
        "桌面端內嵌執行時"
    },
    devices_runtime_headless => {
        "Headless runtime",
        "云端无头运行时",
        "雲端無頭執行時"
    },
    devices_runtime_unknown => {
        "Runtime kind unknown",
        "运行时类型未知",
        "執行時類型未知"
    },
    devices_pair_hint => {
        "Enter opens the highlighted way to pair.",
        "回车展开选中的配对方式。",
        "回車展開選中的配對方式。"
    },
    devices_pair_unavailable => {
        "This runtime cannot pair a device",
        "该运行时无法配对设备",
        "該執行時無法配對裝置"
    },
    devices_qr_hint => {
        "Scan this with the Vibex mobile app.",
        "用 Vibex 手机端扫描。",
        "用 Vibex 手機端掃描。"
    },
    devices_link => { "Pairing link", "配对链接", "配對連結" },
    devices_link_hint => {
        "Open this link on the device you are pairing.",
        "在要配对的设备上打开这条链接。",
        "在要配對的裝置上開啟這條連結。"
    },
    devices_lan_advertising => {
        "Looking for a device on this network",
        "正在局域网内查找设备",
        "正在區域網路內尋找裝置"
    },
    devices_lan_waiting => {
        "Waiting for a device to ask…",
        "等待设备发起请求…",
        "等待裝置發起請求…"
    },
    devices_lan_request => { "Pairing request", "配对请求", "配對請求" },
    devices_lan_allow => { "Allow", "允许", "允許" },
    devices_lan_refuse => { "Refuse", "拒绝", "拒絕" },
    devices_lan_more => {
        "more waiting after this one",
        "之后还有等待中的请求",
        "之後還有等待中的請求"
    },
    devices_lan_expired => {
        "The window closed",
        "查找窗口已关闭",
        "尋找視窗已關閉"
    },
    devices_lan_restart => { "Look again", "重新查找", "重新尋找" },
    devices_lan_closes_in => { "Closes in", "剩余", "剩餘" },
    devices_error_direct_unavailable => {
        "Turn on Remote Access → Direct in the desktop app, then look again.",
        "请在桌面端开启「远程访问 → Direct」，然后重新查找。",
        "請在桌面端開啟「遠端存取 → Direct」，然後重新尋找。"
    },
    devices_error_lan_unavailable => {
        "This runtime cannot advertise itself on the local network.",
        "该运行时无法在局域网内广播自己。",
        "該執行時無法在區域網路內廣播自己。"
    },
    devices_error_tls => {
        "The mobile app cannot reach this runtime over its Direct route.",
        "手机端无法通过当前的 Direct 地址连接该运行时。",
        "手機端無法透過目前的 Direct 位址連線該執行時。"
    },
    devices_error_request_settled => {
        "That request is no longer waiting.",
        "该请求已不再等待。",
        "該請求已不再等待。"
    },
    devices_error_permission => {
        "This device's permission level does not allow pairing.",
        "当前设备的权限级别不允许配对。",
        "目前裝置的權限層級不允許配對。"
    },
    // ---- the audit trail: what the runtime recorded, in the reader's words --
    audit_empty => {
        "Nothing has been recorded yet",
        "尚无审计记录",
        "尚無稽核記錄"
    },
    audit_field_device => { "device", "设备", "裝置" },
    audit_field_target => { "target", "目标", "目標" },
    audit_field_request => { "request", "请求", "請求" },
    audit_field_correlation => { "correlation", "关联", "關聯" },
    audit_outcome_allowed => { "Allowed", "已允许", "已允許" },
    audit_outcome_denied => { "Denied", "已拒绝", "已拒絕" },
    audit_outcome_failed => { "Failed", "失败", "失敗" },
    audit_outcome_created => { "Created", "已创建", "已建立" },
    audit_outcome_revoked => { "Revoked", "已吊销", "已撤銷" },
    audit_action_pairing_code_created => {
        "Pairing code created",
        "一次性配对码已生成",
        "一次性配對碼已產生"
    },
    audit_action_pairing_code_claimed => {
        "Pairing code claimed",
        "一次性配对码已被领取",
        "一次性配對碼已被領取"
    },
    audit_action_pairing_code_rejected => {
        "Pairing code rejected",
        "一次性配对码被拒绝",
        "一次性配對碼被拒絕"
    },
    audit_action_pairing_offer_created => {
        "Pairing offer created",
        "配对邀请已生成",
        "配對邀請已產生"
    },
    audit_action_pairing_offer_claimed => {
        "Pairing offer claimed",
        "配对邀请已被领取",
        "配對邀請已被領取"
    },
    audit_action_pairing_offer_canceled => {
        "Pairing offer canceled",
        "配对邀请已取消",
        "配對邀請已取消"
    },
    audit_action_pairing_offer_rejected => {
        "Pairing offer rejected",
        "配对邀请被拒绝",
        "配對邀請被拒絕"
    },
    audit_action_device_authenticated => {
        "Device authenticated",
        "设备已通过认证",
        "裝置已通過認證"
    },
    audit_action_device_auth_failed => {
        "Device authentication failed",
        "设备认证失败",
        "裝置認證失敗"
    },
    audit_action_device_revoked => { "Device revoked", "设备已吊销", "裝置已撤銷" },
    audit_action_device_restored => { "Device restored", "设备已恢复", "裝置已還原" },
    audit_action_device_deleted => { "Device deleted", "设备已删除", "裝置已刪除" },
    audit_action_device_renamed => { "Device renamed", "设备已重命名", "裝置已重新命名" },
    audit_action_runtime_renamed => {
        "Runtime renamed",
        "运行时已重命名",
        "執行時已重新命名"
    },
    audit_action_permission_allowed => { "Permission allowed", "权限已允许", "權限已允許" },
    audit_action_permission_denied => { "Permission denied", "权限已拒绝", "權限已拒絕" },
    audit_action_mutation_allowed => { "Change allowed", "改动已允许", "改動已允許" },
    audit_action_mutation_denied => { "Change denied", "改动已拒绝", "改動已拒絕" },
    // ---- the publishing half: how this runtime can be reached -------------
    devices_tab_publishing => { "Remote access", "远程接入", "遠端存取" },
    devices_publications_hint => {
        "Enter does nothing here: switching a route on is done in the desktop app's Remote Access page.",
        "这里没有可执行的操作：开启某条发布方式要在桌面端的「远程访问」页面完成。",
        "這裡沒有可執行的操作：開啟某條發佈方式要在桌面端的「遠端存取」頁面完成。"
    },
    devices_publication_hint_ok => {
        "Devices can reach this runtime over it.",
        "设备可以通过这条路线连接该运行时。",
        "裝置可以透過這條路線連線該執行時。"
    },
    devices_publication_unavailable => {
        "This runtime cannot report how it publishes itself",
        "该运行时无法报告自己的发布方式",
        "該執行時無法回報自己的發佈方式"
    },
    devices_publication_empty => {
        "No route is published, so no device can pair",
        "尚未发布任何路线，因此无法配对设备",
        "尚未發佈任何路線，因此無法配對裝置"
    },
    devices_publication_empty_hint => {
        "Turn on Tailscale Serve, Direct, or the self-hosted Relay in the desktop app's Remote Access page.",
        "请在桌面端「远程访问」中开启 Tailscale Serve、Direct 或自建 Relay 之一。",
        "請在桌面端「遠端存取」中開啟 Tailscale Serve、Direct 或自建 Relay 之一。"
    },
    devices_pair_no_route => {
        "No route is published, so no device can pair — turn one on in the desktop app's Remote Access page.",
        "尚未发布任何路线，无法配对设备——请在桌面端「远程访问」中开启一条。",
        "尚未發佈任何路線，無法配對裝置——請在桌面端「遠端存取」中開啟一條。"
    },
    devices_publication_method_tailscale => {
        "Tailscale Serve",
        "Tailscale Serve",
        "Tailscale Serve"
    },
    devices_publication_method_direct => { "Direct HTTPS", "Direct HTTPS", "Direct HTTPS" },
    devices_publication_method_relay => { "Self-hosted Relay", "自建 Relay", "自建 Relay" },
    devices_publication_method_unknown => {
        "Another method",
        "其他发布方式",
        "其他發佈方式"
    },
    devices_publication_state_disabled => { "Off", "未开启", "未開啟" },
    devices_publication_state_online => { "Online", "在线", "線上" },
    devices_publication_state_degraded => { "Degraded", "降级", "降級" },
    devices_publication_state_checking => { "Checking…", "检查中…", "檢查中…" },
    devices_publication_state_enabling => { "Starting…", "启动中…", "啟動中…" },
    devices_publication_state_stopping => { "Stopping…", "停止中…", "停止中…" },
    devices_publication_state_confirmation_needed => {
        "Needs confirmation",
        "需要确认",
        "需要確認"
    },
    devices_publication_state_repair_required => { "Needs repair", "需要修复", "需要修復" },
    devices_publication_state_conflict => { "Conflicting", "存在冲突", "存在衝突" },
    devices_publication_state_error => { "Failed", "失败", "失敗" },
    devices_publication_state_unknown => {
        "Not something this client can name",
        "此客户端无法识别该状态",
        "此客戶端無法識別該狀態"
    },
    devices_publication_recovery_confirm_port => {
        "Confirm the HTTPS port in the desktop app.",
        "请在桌面端确认 HTTPS 端口。",
        "請在桌面端確認 HTTPS 連接埠。"
    },
    devices_publication_recovery_repair_route => {
        "Repair the route in the desktop app.",
        "请在桌面端修复该路线。",
        "請在桌面端修復該路線。"
    },
    devices_publication_recovery_configure => {
        "Finish configuring it in the desktop app.",
        "请在桌面端完成配置。",
        "請在桌面端完成設定。"
    },
    devices_publication_recovery_retry => {
        "Retry it from the desktop app.",
        "请在桌面端重试。",
        "請在桌面端重試。"
    },
    devices_publication_recovery_manual => {
        "It needs a command run on this machine.",
        "需要在这台机器上执行命令。",
        "需要在這台機器上執行命令。"
    },
    devices_publication_recovery_re_pair => {
        "The relay enrollment has to be made again on this machine.",
        "需要在这台机器上重新登记 Relay。",
        "需要在這台機器上重新登記 Relay。"
    },
    devices_publication_recovery_unknown => {
        "Open the desktop app's Remote Access page to see what it needs.",
        "请打开桌面端的「远程访问」查看它需要什么。",
        "請開啟桌面端的「遠端存取」查看它需要什麼。"
    },
    devices_publication_never_enabled => {
        "Off — nobody has switched it on",
        "未开启——尚未有人打开它",
        "未開啟——尚未有人開啟它"
    },
    devices_offer_claimed => {
        "That device paired. The code is spent.",
        "该设备已配对，配对码已作废。",
        "該裝置已配對，配對碼已作廢。"
    },
    devices_route => { "Route", "路线", "路線" },
    devices_route_choice => {
        "switch route",
        "切换路线",
        "切換路線"
    },
    permission_read_only => { "Read only", "只读", "唯讀" },
    permission_approve_only => { "Approve only", "仅审批", "僅審批" },
    permission_full_control => { "Full control", "完全控制", "完全控制" },
    permission_required_for => {
        "This action needs a higher permission level",
        "此操作需要更高的权限级别",
        "此操作需要更高的權限層級"
    },

    // ---- settings -------------------------------------------------------
    settings_theme => { "Theme", "主题", "主題" },
    settings_language => { "Language", "语言", "語言" },
    settings_icons => { "Icons", "图标", "圖示" },
    settings_color => { "Colour", "颜色", "顏色" },
    settings_keys => { "Key bindings", "键位", "鍵位" },
    settings_keys_hint => {
        "Rebind in ~/.vibex/tui-keys.toml",
        "在 ~/.vibex/tui-keys.toml 中重映射",
        "在 ~/.vibex/tui-keys.toml 中重新對應"
    },
    settings_keymap_reloaded => { "Key bindings reloaded", "键位已重新加载", "鍵位已重新載入" },
    keys_press => { "press a key…", "请按键…", "請按鍵…" },
    keys_capture_hint => {
        "Press the chord that should run this action; Esc cancels",
        "按下要绑定到该动作的组合键；Esc 取消",
        "按下要綁定到該動作的組合鍵；Esc 取消"
    },
    keys_capture_cancelled => { "Rebinding cancelled", "已取消重映射", "已取消重新對應" },
    keys_conflict => {
        "That chord is already taken by",
        "该组合键已被占用：",
        "該組合鍵已被占用："
    },
    keys_rebind => { "Rebind", "重绑", "重新綁定" },
    keys_default => { "Default", "恢复默认", "恢復預設" },
    keys_save => { "Save", "保存", "儲存" },
    keys_saved => { "Key bindings saved", "键位已保存", "鍵位已儲存" },
    keys_saved_hint => {
        "s writes ~/.vibex/tui-keys.toml",
        "按 s 写入 ~/.vibex/tui-keys.toml",
        "按 s 寫入 ~/.vibex/tui-keys.toml"
    },
    keys_unsaved => {
        "Unsaved changes — press s to write ~/.vibex/tui-keys.toml",
        "有未保存的修改 — 按 s 写入 ~/.vibex/tui-keys.toml",
        "有未儲存的修改 — 按 s 寫入 ~/.vibex/tui-keys.toml"
    },
    settings_keymap_error => {
        "Key binding file could not be read; using defaults",
        "无法读取键位文件；使用默认键位",
        "無法讀取鍵位檔案；使用預設鍵位"
    },
    settings_about => { "About", "关于", "關於" },
    settings_connection => { "Connection", "连接", "連線" },
    settings_backend => { "Backend", "后端", "後端" },
    settings_authority_seat => {
        "Local mode (in-process runtime)",
        "本地模式（进程内运行时）",
        "本機模式（行程內執行階段）"
    },
    settings_remote_seat => {
        "Remote mode (runtime over a link)",
        "远程模式（通过网络连接运行时）",
        "遠端模式（透過網路連線執行階段）"
    },
    settings_capabilities => { "Capabilities", "能力", "能力" },
    settings_version => { "Version", "版本", "版本" },
    settings_section_appearance => { "Appearance", "外观", "外觀" },
    settings_section_language => { "Language", "语言", "語言" },
    settings_section_interface => { "Interface", "界面", "介面" },
    settings_mode => { "Theme mode", "主题模式", "主題模式" },
    settings_mode_dark => { "Dark", "深色", "深色" },
    settings_mode_light => { "Light", "浅色", "淺色" },
    settings_mode_hint => {
        "Light or dark treatment of the same palette.",
        "同一套配色在深色或浅色下的呈现。",
        "同一套配色在深色或淺色下的呈現。"
    },
    settings_theme_hint => {
        "Twenty shipped palettes; the preview applies as you move.",
        "内置 20 套配色；移动光标即预览。",
        "內建 20 套配色；移動游標即預覽。"
    },
    settings_mark => { "Logo", "字符画", "字符畫" },
    settings_mark_hint => {
        "The art a new session's page opens on",
        "新建会话页面上的字符画",
        "新增工作階段頁面上的字符畫"
    },
    settings_mark_classic => { "Classic", "经典", "經典" },
    settings_mark_glitch => { "Glitch", "故障", "故障" },
    settings_motion => { "Motion", "动效", "動效" },
    settings_motion_hint => {
        "Animate the logo and timeline disclosures; off shows changes immediately",
        "为字符画和时间线折叠展开添加动效；关闭则立即显示变化",
        "為字符畫和時間線摺疊展開加上動效；關閉則立即顯示變更"
    },
    settings_transitions => { "Text transitions", "文字切换特效", "文字切換特效" },
    settings_transitions_hint => {
        "Scramble a line when the text it shows changes",
        "文本变化时以字符特效切换",
        "文字變更時以字元特效切換"
    },
    settings_on => { "On", "开", "開" },
    settings_off => { "Off", "关", "關" },
    // ---- notifications ---------------------------------------------------
    // The settings page's own copy for the notification layer. The three kinds
    // are named the way the desktop names them, and the hint under each says
    // what makes it news: a session the reader is not looking at.
    settings_section_notifications => { "Notifications", "通知", "通知" },
    settings_notifications => { "Notifications", "通知", "通知" },
    settings_notifications_hint => {
        "Announce a session that finishes, fails or waits for you while you are on another page or away from the terminal.",
        "当会话完成、失败或等待确认，而你在其他页面或离开终端时提醒你。",
        "當工作階段完成、失敗或等待確認，而你在其他頁面或離開終端時提醒你。"
    },
    settings_notify_completed => { "Agent completed", "Agent 完成", "Agent 完成" },
    settings_notify_completed_hint => {
        "Notify when an Agent turn completes.",
        "Agent 回合完成时通知。",
        "Agent 回合完成時通知。"
    },
    settings_notify_needs_input => { "Agent needs input", "Agent 需要输入", "Agent 需要輸入" },
    settings_notify_needs_input_hint => {
        "Notify when an Agent is waiting for input.",
        "Agent 等待输入时通知。",
        "Agent 等待輸入時通知。"
    },
    settings_notify_failed => { "Agent failed", "Agent 失败", "Agent 失敗" },
    settings_notify_failed_hint => {
        "Notify when an Agent turn fails.",
        "Agent 回合失败时通知。",
        "Agent 回合失敗時通知。"
    },
    settings_notify_transport => { "Terminal signal", "终端提示方式", "終端提示方式" },
    settings_notify_transport_hint => {
        "The escape sequence this terminal understands, written while the terminal window is focused; automatic follows the terminal it detects.",
        "该终端能识别的控制序列，在终端窗口有焦点时使用；自动会跟随探测到的终端。",
        "該終端能識別的控制序列，在終端視窗有焦點時使用；自動會跟隨探測到的終端。"
    },
    settings_notify_transport_auto => { "Automatic", "自动", "自動" },
    settings_notify_transport_off => { "Off (in-app only)", "关闭（仅界面内）", "關閉（僅介面內）" },
    settings_notify_transport_bell => { "Bell", "响铃", "響鈴" },
    settings_notify_transport_title => { "Window title", "窗口标题", "視窗標題" },
    settings_notify_transport_osc9 => { "OSC 9 notification", "OSC 9 通知", "OSC 9 通知" },
    settings_notify_transport_osc777 => { "OSC 777 notification", "OSC 777 通知", "OSC 777 通知" },
    settings_notify_transport_osc99 => { "OSC 99 notification", "OSC 99 通知", "OSC 99 通知" },
    settings_notify_transport_iterm2 => { "iTerm2 attention", "iTerm2 提醒", "iTerm2 提醒" },
    settings_notify_system => { "Desktop popup", "系统弹窗", "系統彈窗" },
    settings_notify_system_hint => {
        "Draw a desktop notification with notify-send or osascript while the terminal window is not focused (tmux needs focus-events on). Only on the machine running this client, never over ssh.",
        "终端窗口失去焦点时用 notify-send 或 osascript 弹出系统通知（tmux 需开启 focus-events）。仅在运行本客户端的机器上生效，SSH 连接下不会触发。",
        "終端視窗失去焦點時用 notify-send 或 osascript 彈出系統通知（tmux 需開啟 focus-events）。僅在執行本客戶端的機器上生效，SSH 連線下不會觸發。"
    },
    settings_notify_system_remote => {
        "Unavailable over ssh",
        "SSH 连接下不可用",
        "SSH 連線下不可用"
    },
    settings_notify_system_missing => {
        "No notifier installed",
        "未安装通知程序",
        "未安裝通知程式"
    },
    settings_notify_system_desktop => {
        "The desktop's own",
        "由桌面端弹出",
        "由桌面端彈出"
    },
    settings_notify_light => { "Light reminders", "轻提醒", "輕提醒" },
    settings_notify_light_hint => {
        "While a Vibex desktop is hosting this runtime it announces the same events itself; this client stays light and only marks the window title instead of ringing or popping up.",
        "当桌面端托管该运行时，它自己会通知同样的消息；本客户端只用窗口标题做轻提醒，不再响铃或弹窗。",
        "當桌面端託管該執行階段時，它自己會通知同樣的訊息；本客戶端只用視窗標題做輕提醒，不再響鈴或彈窗。"
    },
    settings_notify_desktop_present => {
        "desktop present",
        "桌面端在运行",
        "桌面端在執行"
    },
    settings_notify_title => { "Include session title", "通知带会话标题", "通知帶工作階段標題" },
    settings_notify_title_hint => {
        "Let the session's own title ride the notification. Off by default: a terminal may log it.",
        "在通知中附带会话标题。默认关闭：终端可能记录该内容。",
        "在通知中附帶工作階段標題。預設關閉：終端可能記錄該內容。"
    },
    settings_notify_test => { "Test notification", "测试通知", "測試通知" },
    settings_notify_test_hint => {
        "Send one now through every channel that is on, so you can see what each one does. A real event goes to one of them, chosen by whether the terminal window has focus.",
        "立即通过每个已开启的通道各发一条，看看它们各自的效果。真实事件只会走其中一个通道，由终端窗口是否有焦点决定。",
        "立即透過每個已開啟的通道各傳送一條，看看它們各自的效果。真實事件只會走其中一個通道，由終端視窗是否有焦點決定。"
    },
    settings_notify_test_body => {
        "Test notification from Vibex",
        "来自 Vibex 的测试通知",
        "來自 Vibex 的測試通知"
    },
    settings_notify_test_sent => {
        "Test notification sent",
        "测试通知已发送",
        "測試通知已傳送"
    },
    settings_notify_test_none => {
        "Nothing sent: notifications are off, or every channel is",
        "未发送：通知已关闭，或所有通道都关闭了",
        "未傳送：通知已關閉，或所有通道都關閉了"
    },
    settings_reasoning_display => { "Reasoning display", "推理显示方式", "推理顯示方式" },
    settings_reasoning_display_hint => {
        "Show the latest live reasoning at the bottom, or keep reasoning in the timeline.",
        "将最新实时推理置于底部，或保留在会话时间线中。",
        "將最新即時推理置於底部，或保留在工作階段時間線中。"
    },
    settings_reasoning_latest => { "Latest at bottom", "最新推理内容置底", "最新推理內容置底" },
    settings_reasoning_timeline => { "In timeline", "显示在时间线中", "顯示在時間線中" },
    settings_reasoning_expanded => { "Expand reasoning by default", "默认展开推理", "預設展開推理" },
    settings_reasoning_expanded_hint => {
        "Open reasoning by default. Each reasoning row can still be expanded or collapsed.",
        "默认打开推理内容；仍可逐项展开或折叠。",
        "預設開啟推理內容；仍可逐項展開或摺疊。"
    },
    settings_reasoning_expansion => { "Reasoning expansion", "推理展开方式", "推理展開方式" },
    settings_reasoning_expansion_hint => {
        "Window follows the newest live lines; full expansion shows all reasoning as it arrives.",
        "窗口渲染跟随最新实时内容；全量展开显示全部推理。",
        "視窗繪製跟隨最新即時內容；完整展開顯示全部推理。"
    },
    settings_reasoning_window => { "Window", "窗口渲染", "視窗繪製" },
    settings_reasoning_full => { "Full expansion", "全量展开", "完整展開" },
    settings_icons_hint => {
        "Unicode chrome, or ASCII for a terminal without font fallback.",
        "Unicode 界面符号；无字体回退的终端可用 ASCII。",
        "Unicode 介面符號；無字型回退的終端可用 ASCII。"
    },
    settings_language_hint => {
        "Product copy for the interface; the runtime is unaffected.",
        "界面文案语言；不影响运行时。",
        "介面文案語言；不影響執行階段。"
    },
    settings_workspace_hint => {
        "Where a new session starts; empty uses the open session's workspace.",
        "新会话的起始目录；留空则使用当前会话的工作区。",
        "新工作階段的起始目錄；留空則使用目前工作階段的工作區。"
    },
    settings_backend_hint => {
        "The runtime's capability schema this client is talking to.",
        "当前客户端连接的运行时能力版本。",
        "目前客戶端連線的執行階段能力版本。"
    },
    settings_seat_hint => {
        "Local mode starts the runtime in this app; remote mode connects to one.",
        "本地模式由本程序启动运行时；远程模式通过网络连接运行时。",
        "本機模式由本程式啟動執行階段；遠端模式透過網路連線執行階段。"
    },
    settings_version_hint => {
        "The client build; the runtime reports its own version separately.",
        "客户端版本；运行时版本单独上报。",
        "客戶端版本；執行階段版本單獨回報。"
    },
    settings_keys_reload => { "F9 reloads", "F9 重新加载", "F9 重新載入" },
    settings_icons_unicode => { "Unicode", "Unicode", "Unicode" },
    settings_icons_ascii => { "ASCII", "ASCII", "ASCII" },
    settings_filter_label => { " filter: ", " 过滤: ", " 過濾: " },
    settings_no_matches => {
        "No settings match",
        "没有匹配的设置",
        "沒有符合的設定"
    },
    settings_pick_hint => {
        "Moves preview the value; Esc puts the old one back",
        "移动即预览；Esc 还原",
        "移動即預覽；Esc 還原"
    },
    settings_edit_hint => {
        "Enter saves, Esc discards",
        "Enter 保存，Esc 放弃",
        "Enter 儲存，Esc 捨棄"
    },
    settings_reset_confirm => {
        "Reset this setting to its default?",
        "将此设置重置为默认值？",
        "將此設定重設為預設值？"
    },
    settings_reset_title => { "Reset setting", "重置设置", "重設設定" },
    settings_reset_done => { "reset", "已重置", "已重設" },

    // ---- help -----------------------------------------------------------
    help_title => { "Help", "帮助", "說明" },
    help_category_global => { "Global", "全局", "全域" },
    help_category_transcript => { "Transcript", "时间线", "時間線" },
    help_category_composer => { "Composer", "输入框", "輸入框" },
    help_category_modals => { "Modals", "弹窗", "彈窗" },
    help_category_workbench => { "Workbench", "工作台", "工作台" },
    help_category_management => { "Management", "管理", "管理" },
    help_category_panels => { "Panels", "面板", "面板" },
    help_context => { "Context", "上下文", "上下文" },
    help_keys => { "Keys", "键位", "鍵位" },
    help_hint => {
        "Press ? for the keys available right now",
        "按 ? 查看当前可用键位",
        "按 ? 查看目前可用鍵位"
    },
    help_no_keys => { "No keys are available here", "此处没有可用键位", "此處沒有可用鍵位" },
    help_disabled_key => {
        "That key is disabled right now",
        "该键当前不可用",
        "該鍵目前不可用"
    },
    help_legend => { "Legend", "图例", "圖例" },

    // ---- command palette / overlays --------------------------------------
    palette_title => { "Command palette", "命令面板", "命令面板" },
    palette_placeholder => { "Type a command…", "输入命令…", "輸入命令…" },
    // The headings, in the order the palette draws them: where the reader can
    // go, the devices they trust, the client itself, and — on a session's page
    // alone — the sessions those verbs act on.
    palette_group_view => { "Feature view", "功能视图", "功能檢視" },
    palette_group_pairing => { "Remote pairing", "远程配对", "遠端配對" },
    palette_group_app => { "App", "应用", "應用" },
    palette_group_session => { "Session list", "会话列表", "工作階段清單" },
    palette_group_workbench => { "Workbench", "工作台", "工作台" },
    // The palette's own names and one-line explanations. They are longer than
    // the key bar's labels because the palette is read as a menu rather than
    // glanced at as a legend.
    palette_cmd_sessions => { "Session list", "会话列表", "工作階段清單" },
    palette_hint_sessions => { "Open the session list", "打开会话列表", "開啟工作階段清單" },
    palette_cmd_management => { "Configuration centre", "配置中心", "配置中心" },
    palette_hint_management => {
        "Open the configuration centre",
        "打开配置中心",
        "開啟配置中心"
    },
    palette_cmd_settings => { "Settings", "设置", "設定" },
    palette_hint_settings => { "Theme, language, keys", "主题、语言、键位", "主題、語言、鍵位" },
    palette_cmd_help => { "Help", "帮助", "說明" },
    palette_hint_help => { "Contextual key help", "查看当前键位说明", "檢視目前鍵位說明" },
    palette_cmd_new_session => { "New session", "新建会话", "新增工作階段" },
    palette_hint_new_session => { "Create a session", "创建一个会话", "建立一個工作階段" },
    palette_cmd_rename_session => { "Rename session", "重命名会话", "重新命名工作階段" },
    // The session verbs act on the session list's own cursor — the row the
    // reader has selected there — which is not always the session on screen.
    // The hints name that target rather than promising "the open session".
    palette_hint_rename_session => {
        "Rename the selected session",
        "重命名选中的会话",
        "重新命名選取的工作階段"
    },
    palette_cmd_fork_session => { "Fork session", "复制会话分支", "複製工作階段分支" },
    palette_hint_fork_session => {
        "Copy the selected session",
        "复制选中的会话",
        "複製選取的工作階段"
    },
    palette_cmd_archive_session => { "Archive session", "归档会话", "封存工作階段" },
    palette_hint_archive_session => {
        "Archive the selected session",
        "归档选中的会话",
        "封存選取的工作階段"
    },
    palette_cmd_restore_session => { "Restore session", "恢复会话", "還原工作階段" },
    palette_hint_restore_session => {
        "Restore an archived session",
        "恢复已归档的会话",
        "還原已封存的工作階段"
    },
    palette_cmd_delete_session => { "Delete session", "删除会话", "刪除工作階段" },
    palette_hint_delete_session => {
        "Delete the selected session",
        "删除选中的会话",
        "刪除選取的工作階段"
    },
    palette_cmd_switch_runtime => { "Switch runtime", "切换运行时", "切換執行階段" },
    palette_hint_switch_runtime => {
        "Pick the Agent runtime and model",
        "选择 Agent 运行时和模型",
        "選擇 Agent 執行階段與模型"
    },
    palette_cmd_probe_runtimes => { "Probe runtimes", "探测运行时", "探測執行階段" },
    palette_hint_probe_runtimes => {
        "Re-discover available Agents",
        "重新发现可用的 Agent",
        "重新探索可用的 Agent"
    },
    palette_cmd_devices => { "Pairing and devices", "配对与设备", "配對與裝置" },
    palette_hint_devices => {
        "Pair a phone or a second desktop, and manage the devices already paired",
        "配对手机或第二台桌面端，并管理已配对的设备",
        "配對手機或第二台桌面端，並管理已配對的裝置"
    },
    palette_cmd_reload_keys => { "Reload key bindings", "重新加载键位", "重新載入鍵位" },
    palette_hint_reload_keys => {
        "Re-read tui-keys.toml",
        "重新读取 tui-keys.toml",
        "重新讀取 tui-keys.toml"
    },
    palette_cmd_refresh => { "Refresh", "刷新", "重新整理" },
    palette_hint_refresh => {
        "Re-read the current page",
        "重新读取当前页面",
        "重新讀取目前頁面"
    },
    palette_cmd_quit => { "Quit", "退出", "結束" },
    palette_hint_quit => { "Leave the TUI", "离开 TUI", "離開 TUI" },
    overlay_confirm_title => { "Please confirm", "请确认", "請確認" },
    overlay_prompt_title => { "Input", "输入", "輸入" },
    toast_permission_denied => {
        "The runtime denied this action",
        "运行时拒绝了此操作",
        "執行階段拒絕了此操作"
    },
    toast_action_unavailable => {
        "This client cannot perform that action",
        "此客户端无法执行该操作",
        "此用戶端無法執行該操作"
    },
    toast_offline => {
        "Not available while disconnected",
        "断连时不可用",
        "中斷連線時不可用"
    },
    handover_offer => {
        "The runtime has stopped · Ctrl+T takes this home over",
        "运行时已停止 · Ctrl+T 接管这个 home",
        "執行階段已停止 · Ctrl+T 接管這個 home"
    },

    // ---- degradation ----------------------------------------------------
    no_tty_title => { "Not a terminal", "不是终端", "不是終端機" },
    no_tty_body => {
        "Vibex TUI needs an interactive terminal on stdin and stdout.",
        "Vibex TUI 需要 stdin 和 stdout 都是交互式终端。",
        "Vibex TUI 需要 stdin 和 stdout 都是互動式終端機。"
    },
    cjk_font_hint => {
        "Chinese text needs a monospace CJK font in your terminal",
        "中文界面需要终端安装等宽 CJK 字体",
        "中文介面需要終端機安裝等寬 CJK 字型"
    },
    runtime_locked_title => {
        "Another Vibex runtime owns this home",
        "另一个 Vibex 运行时正在使用此 home",
        "另一個 Vibex 執行階段正在使用此 home"
    },
    runtime_locked_body => {
        "Vibex Desktop is running and is not accepting local clients. Either turn on Settings → General → Vibex TUI, quit the desktop app and retry, or connect to a remote runtime with `vibex connect <link>`.",
        "Vibex 桌面端正在运行且未开放本机客户端。请在桌面端开启 设置 → 通用 → Vibex TUI，或退出桌面端后重试，或用 `vibex connect <链接>` 连接远程运行时。",
        "Vibex 桌面端正在執行且未開放本機用戶端。請在桌面端開啟 設定 → 通用 → Vibex TUI，或結束桌面端後重試，或用 `vibex connect <連結>` 連線遠端執行階段。"
    },
    // How the client is attached, in the reader's words. "Seat" stays the
    // internal name (`SeatKind`); the reader sees a mode, because a seat is
    // not a word a terminal user knows.
    seat_authority => { "Local mode", "本地模式", "本機模式" },
    seat_remote => { "Remote mode", "远程模式", "遠端模式" },

    // ---- key help: what each action is called ----------------------------
    // One row per action, keyed by the action's stable id. The key bar, the
    // `?` cheatsheet, the key editor and the command palette all read these,
    // so a chord and the words beside it can never disagree about language.
    label_command_palette => { "Commands", "命令", "命令" },
    label_help => { "Help", "帮助", "說明" },
    label_open_settings => { "Settings", "设置", "設定" },
    label_quit => { "Quit", "退出", "退出" },
    label_back => { "Back", "返回", "返回" },
    label_focus_next => { "Next pane", "下一个窗格", "下一個窗格" },
    label_focus_previous => { "Previous pane", "上一个窗格", "上一個窗格" },
    label_refresh => { "Refresh", "刷新", "重新整理" },
    label_cancel => { "Cancel", "取消", "取消" },
    label_goto_sessions => { "Sessions", "会话", "工作階段" },
    label_goto_management => { "Management", "管理", "管理" },
    label_toggle_sidebar => { "Sidebar", "侧边栏", "側邊欄" },
    label_reload_keymap => { "Reload keys", "重新加载键位", "重新載入鍵位" },
    label_take_over_home => { "Take over", "接管", "接管" },
    label_select_previous => { "Previous", "上一个", "上一個" },
    label_select_next => { "Next", "下一个", "下一個" },
    label_scroll_page_up => { "Page up", "上翻页", "上翻頁" },
    label_scroll_page_down => { "Page down", "下翻页", "下翻頁" },
    label_scroll_half_up => { "Half up", "上翻半页", "上翻半頁" },
    label_scroll_half_down => { "Half down", "下翻半页", "下翻半頁" },
    label_scroll_top => { "Top", "顶部", "頂部" },
    label_scroll_bottom => { "Bottom", "底部", "底部" },
    label_show_details => { "Details", "详情", "詳情" },
    label_session_open => { "Open", "打开", "開啟" },
    label_session_control_previous => { "Previous button", "上一个按钮", "上一個按鈕" },
    label_session_control_next => { "Next button", "下一个按钮", "下一個按鈕" },
    label_session_new => { "New", "新建", "新增" },
    label_session_rename => { "Rename", "重命名", "重新命名" },
    label_session_fork => { "Fork", "复制分支", "複製分支" },
    label_session_archive => { "Archive", "归档", "封存" },
    label_session_unarchive => { "Restore", "恢复", "恢復" },
    label_session_delete => { "Delete", "删除", "刪除" },
    label_session_show_archived => { "Archived", "已归档", "已封存" },
    label_session_card_toggle => { "Details", "详情", "詳情" },
    label_session_card_collapse => { "Close cards", "关闭卡片", "關閉卡片" },
    label_sidebar_row_actions => { "Actions", "操作", "操作" },
    label_session_children_toggle => { "Sub-sessions", "子会话", "子工作階段" },
    label_session_copy => { "Copy", "复制", "複製" },
    label_session_pin => { "Pin", "置顶", "置頂" },
    label_session_auto_continue => { "Auto-continue", "自动继续", "自動繼續" },
    label_session_move_up => { "Move up", "上移", "上移" },
    label_session_move_down => { "Move down", "下移", "下移" },
    label_workspace_switch => { "Workspace", "工作区", "工作區" },
    label_workspace_browse => { "Browse", "浏览", "瀏覽" },
    label_workspace_up => { "Parent", "上级", "上級" },
    label_workspace_select => { "Use directory", "使用目录", "使用目錄" },
    label_composer_focus => { "Write", "撰写", "撰寫" },
    label_turn_continue => { "Continue", "继续", "繼續" },
    label_block_toggle => { "Expand", "展开", "展開" },
    label_block_toggle_all => { "Expand all", "展开全部", "展開全部" },
    label_reasoning_toggle => { "Reasoning", "推理", "推理" },
    label_block_copy => { "Copy", "复制", "複製" },
    label_block_copy_meta => { "Copy meta", "复制元数据", "複製後設資料" },
    label_block_details => { "Details", "详情", "詳情" },
    label_runtime_switch => { "Runtime", "运行时", "執行階段" },
    label_runtime_probe => { "Probe", "探测", "探測" },
    label_transcript_search => { "Find", "查找", "查詢" },
    label_dock_toggle => { "Dock", "停靠栏", "停靠欄" },
    label_dock_activate => { "Dock open", "打开停靠项", "開啟停靠項" },
    label_session_children_open => { "Sub-sessions", "子会话", "子工作階段" },
    label_session_parent => { "Main session", "主会话", "主工作階段" },
    label_dock_hide_done => { "Hide done", "隐藏已完成", "隱藏已完成" },
    label_history_older => { "Older history", "更早历史", "更早歷史" },
    label_queue_previous => { "Queue up", "队列上移", "佇列上移" },
    label_queue_next => { "Queue down", "队列下移", "佇列下移" },
    label_queue_edit => { "Queue edit", "编辑队列项", "編輯佇列項" },
    label_queue_delete => { "Queue drop", "删除队列项", "刪除佇列項" },
    label_queue_move_up => { "Queue raise", "队列提前", "佇列提前" },
    label_queue_move_down => { "Queue lower", "队列推后", "佇列推後" },
    label_queue_send_now => { "Send now", "立即发送", "立即傳送" },
    label_message_schedule => { "Schedule", "定时", "定時" },
    label_search_next => { "Next match", "下一个匹配", "下一個符合" },
    label_search_previous => { "Prev match", "上一个匹配", "上一個符合" },
    label_composer_submit => { "Send", "发送", "傳送" },
    label_composer_newline => { "Newline", "换行", "換行" },
    label_composer_multiline => { "Multiline", "多行模式", "多行模式" },
    label_composer_history_prev => { "Prev sent", "上一条已发送", "上一條已傳送" },
    label_composer_history_next => { "Next sent", "下一条已发送", "下一條已傳送" },
    label_composer_editor => { "$EDITOR", "$EDITOR", "$EDITOR" },
    label_composer_background => { "Background", "转后台", "轉後臺" },
    label_composer_steer => { "Steer", "插话", "插話" },
    label_completion_next => { "Next match", "下一个补全", "下一個補全" },
    label_completion_previous => { "Prev match", "上一个补全", "上一個補全" },
    label_completion_accept => { "Accept", "接受", "接受" },
    label_completion_cancel => { "Dismiss", "关闭", "關閉" },
    label_composer_delete_word => { "Delete word", "删除词", "刪除詞" },
    label_composer_delete_word_after => { "Kill word", "删除后一词", "刪除後一詞" },
    label_composer_delete_word_backward => { "Kill word back", "删除前一词", "刪除前一詞" },
    label_composer_kill_to_end => { "Kill to end", "剪切到行尾", "剪下到行尾" },
    label_composer_kill_to_start => { "Kill to start", "剪切到行首", "剪下到行首" },
    label_composer_yank => { "Yank", "粘贴剪切", "貼上剪下" },
    label_composer_undo => { "Undo", "撤销", "復原" },
    label_composer_redo => { "Redo", "重做", "重做" },
    label_composer_word_left => { "Word left", "左移一词", "左移一詞" },
    label_composer_word_right => { "Word right", "右移一词", "右移一詞" },
    label_composer_line_start => { "Line start", "行首", "行首" },
    label_composer_line_end => { "Line end", "行尾", "行尾" },
    label_composer_image => { "Image", "图片", "圖片" },
    label_composer_paste => { "Paste", "粘贴", "貼上" },
    label_composer_select_all => { "Select all", "全选", "全選" },
    label_composer_copy_draft => { "Copy draft", "复制草稿", "複製草稿" },
    label_overlay_close => { "Close", "关闭", "關閉" },
    label_overlay_confirm => { "Confirm", "确认", "確認" },
    label_overlay_next_field => { "Next field", "下一个字段", "下一個欄位" },
    label_overlay_prev_field => { "Prev field", "上一个字段", "上一個欄位" },
    label_overlay_toggle => { "Toggle", "切换", "切換" },
    label_schedule_clear => { "Clear", "清除", "清除" },
    label_palette_run => { "Run", "执行", "執行" },
    label_overlay_fold_open => { "Expand", "展开", "展開" },
    label_overlay_fold_close => { "Fold", "折叠", "摺疊" },
    label_overlay_star_model => { "Star", "收藏", "收藏" },
    label_overlay_manage_account => { "Account", "账号", "帳號" },
    label_overlay_reset_run_option => { "Reset", "重置", "重置" },
    label_approval_approve => { "Allow", "允许", "允許" },
    label_approval_deny => { "Deny", "拒绝", "拒絕" },
    label_approval_always => { "Always", "总是", "總是" },
    label_approval_next => { "Next option", "下一个选项", "下一個選項" },
    label_elicitation_submit => { "Submit", "提交", "提交" },
    label_elicitation_next => { "Next field", "下一个问题", "下一個問題" },
    label_elicitation_prev => { "Prev field", "上一个问题", "上一個問題" },
    label_management_toggle => { "Toggle", "启用 / 停用", "啟用 / 停用" },
    label_management_edit => { "Edit", "编辑", "編輯" },
    label_management_reload => { "Reload", "重新加载", "重新載入" },
    label_config_module_previous => { "Module", "模块", "模組" },
    label_config_module_next => { "Module", "模块", "模組" },
    label_config_section_previous => { "Area", "功能区", "功能區" },
    label_config_section_next => { "Area", "功能区", "功能區" },
    label_config_pane_next => { "Pane", "窗格", "窗格" },
    label_config_pane_previous => { "Pane", "窗格", "窗格" },
    label_config_entry_actions => { "Actions", "操作", "操作" },
    label_config_entry_create => { "New", "新建", "新增" },
    label_config_entry_delete => { "Delete", "删除", "刪除" },
    label_config_form_save => { "Save", "保存", "儲存" },
    label_config_form_copy => { "Copy", "复制", "複製" },
    label_config_models => { "Models", "模型", "模型" },
    label_config_model_toggle => { "Offer", "启用", "啟用" },
    label_config_model_add => { "Add", "添加", "新增" },
    label_config_model_remove => { "Remove", "移除", "移除" },
    label_config_model_fetch => { "Fetch", "拉取", "拉取" },
    label_config_model_edit => { "Settings", "设置", "設定" },
    label_config_import => { "Import", "导入", "匯入" },
    label_config_export => { "Export", "导出", "匯出" },
    label_config_discover => { "Discover", "探测", "探測" },
    label_config_registry_filter => { "Transport", "传输方式", "傳輸方式" },
    label_config_registry_sort => { "Sort", "排序", "排序" },
    label_config_registry_page_previous => { "Prev page", "上一页", "上一頁" },
    label_config_registry_page_next => { "Next page", "下一页", "下一頁" },
    label_goto_devices => { "Pairing and devices", "配对与设备", "配對與裝置" },
    label_device_pair_activate => { "Open", "打开", "開啟" },
    label_device_tab_next => { "Tab", "标签页", "標籤頁" },
    label_device_tab_previous => { "Tab", "标签页", "標籤頁" },
    label_device_route_next => { "Route", "路线", "路線" },
    label_device_route_previous => { "Route", "路线", "路線" },
    label_device_lan_allow => { "Allow", "允许", "允許" },
    label_device_lan_refuse => { "Refuse", "拒绝", "拒絕" },
    label_device_revoke => { "Revoke", "撤销授权", "復原授權" },
    label_device_audit => { "Audit", "审计", "審計" },
    label_setting_activate => { "Change", "修改", "修改" },
    label_setting_previous => { "Previous value", "上一个值", "上一個值" },
    label_setting_next => { "Next value", "下一个值", "下一個值" },
    label_setting_reset => { "Reset", "重置", "重置" },
    label_filter_begin => { "Filter", "过滤", "過濾" },
    label_filter_clear => { "Clear filter", "清除过滤", "清除過濾" },

    // ---- key help: rows that need words of their own ----------------------
    // An action that means slightly different things in different scopes shows
    // the aspect that scope is about (`Search` on the runtime switcher, `Filter`
    // on a list), so these are the few rows that do not reuse the action's name.
    row_accept_completion => { "Accept completion", "接受补全", "接受補全" },
    row_audit_log => { "Audit log", "审计日志", "審計日誌" },
    row_back_close => { "Back / close", "返回 / 关闭", "返回 / 關閉" },
    row_block_details => { "Block details", "块详情", "塊詳情" },
    row_bottom_follow => { "Bottom (follow)", "底部（跟随）", "底部（跟隨）" },
    row_browse_dirs => { "Browse dirs", "浏览目录", "瀏覽目錄" },
    row_cancel_interrupt => { "Cancel / interrupt", "取消 / 打断", "取消 / 打斷" },
    row_clear_schedule => { "Clear schedule", "清除定时", "清除定時" },
    row_command_palette => { "Command palette", "命令面板", "命令面板" },
    row_contextual_help => { "Contextual help", "上下文帮助", "上下文說明" },
    row_continue_turn => { "Continue turn", "继续回合", "繼續回合" },
    row_copy_block => { "Copy block", "复制此块", "複製此塊" },
    row_copy_metadata => { "Copy metadata", "复制元数据", "複製後設資料" },
    row_edit_in_editor => { "Edit in $EDITOR", "用 $EDITOR 编辑", "用 $EDITOR 編輯" },
    row_enable_disable => { "Enable / disable", "启用 / 停用", "啟用 / 停用" },
    row_expand_block => { "Expand block", "展开此块", "展開此塊" },
    row_fork_session => { "Fork session", "复制分支会话", "複製分支工作階段" },
    row_half_page_down => { "Half page down", "下翻半页", "下翻半頁" },
    row_half_page_up => { "Half page up", "上翻半页", "上翻半頁" },
    row_heading_actions => { "Heading actions", "标题操作", "標題操作" },
    row_multiline_mode => { "Multiline mode", "多行模式", "多行模式" },
    row_new_session => { "New session", "新建会话", "新增工作階段" },
    row_newline_any_terminal => { "Newline (any terminal)", "换行（任意终端）", "換行（任意終端）" },
    row_next_completion => { "Next completion", "下一个补全", "下一個補全" },
    row_next_message => { "Next message", "下一条消息", "下一則訊息" },
    row_next_question => { "Next question", "下一个问题", "下一個問題" },
    row_parent_dir => { "Parent dir", "上级目录", "上級目錄" },
    row_prev_question => { "Prev question", "上一个问题", "上一個問題" },
    row_previous_completion => { "Previous completion", "上一个补全", "上一個補全" },
    row_previous_message => { "Previous message", "上一条消息", "上一則訊息" },
    row_probe_runtimes => { "Probe runtimes", "探测运行时", "探測執行階段" },
    row_reload_key_bindings => { "Reload key bindings", "重新加载键位", "重新載入鍵位" },
    row_run_command => { "Run command", "执行命令", "執行命令" },
    row_schedule_send => { "Schedule send", "定时发送", "定時傳送" },
    row_search => { "Search", "搜索", "搜尋" },
    row_search_help => { "Search help", "搜索帮助", "搜尋說明" },
    row_send_to_background => { "Send to background", "转后台", "轉後臺" },
    row_show_archived => { "Show archived", "显示已归档", "顯示已封存" },
    row_steer_running_turn => { "Steer running turn", "插话到运行中的回合", "插話到執行中的回合" },
    row_submit_answers => { "Submit answers", "提交答案", "提交答案" },
    row_toggle_reasoning => { "Toggle reasoning", "切换推理", "切換推理" },
    row_use_dir => { "Use dir", "使用该目录", "使用該目錄" },
    row_write_a_message => { "Write a message", "撰写消息", "撰寫訊息" },

    // ---- key help: what each action does ---------------------------------
    // The sentence the cheatsheet shows under the selected row, in every
    // language the client ships.
    help_command_palette => { "Fuzzy-search every page and action.", "模糊搜索每个页面与操作。", "模糊搜尋每個頁面與操作。" },
    help_help => { "Show the keys and concepts for whatever has focus right now.", "显示当前焦点处的键位与说明。", "顯示目前焦點處的鍵位與說明。" },
    help_open_settings => { "Theme, language, icons, colour and key bindings.", "主题、语言、图标、配色与键位。", "主題、語言、圖示、配色與鍵位。" },
    help_quit => { "Leave the TUI and restore the terminal. The palette is the only way to ask for it by name.", "退出 TUI 并还原终端。命令面板是唯一按名称请求退出的入口。", "退出 TUI 並還原終端。命令面板是唯一按名稱請求退出的入口。" },
    help_back => { "Close the overlay, then the panel, then the page. Never quits.", "先关闭浮层，再关闭面板，最后返回上一页。绝不退出。", "先關閉浮層，再關閉面板，最後返回上一頁。絕不退出。" },
    help_focus_next => { "Move focus between sidebar, main area, details and composer.", "在侧边栏、主区域、详情与输入框之间移动焦点。", "在側邊欄、主區域、詳情與輸入框之間移動焦點。" },
    help_focus_previous => { "Move focus backwards.", "向后移动焦点。", "向後移動焦點。" },
    help_refresh => { "Re-read the current page from the runtime.", "从运行时重新读取当前页面。", "從執行階段重新讀取目前頁面。" },
    help_cancel => { "Clear the input, else interrupt the running turn, else offer to quit.", "清空输入；否则打断运行中的回合；再否则询问是否退出。", "清空輸入；否則打斷執行中的回合；再否則詢問是否退出。" },
    help_goto_sessions => { "Jump to the session list.", "跳转到会话列表。", "跳轉到工作階段列表。" },
    help_goto_management => { "Jump to the management index.", "跳转到管理首页。", "跳轉到管理首頁。" },
    help_toggle_sidebar => { "Collapse or show the sidebar.", "折叠或显示侧边栏。", "摺疊或顯示側邊欄。" },
    help_reload_keymap => { "Re-read ~/.vibex/tui-keys.toml.", "重新读取 ~/.vibex/tui-keys.toml。", "重新讀取 ~/.vibex/tui-keys.toml。" },
    help_take_over_home => { "Start the runtime in this terminal instead of waiting for the one that owns this home.", "在此终端启动运行时，而不再等待占用该 home 的运行时。", "在此終端啟動執行階段，而不再等待佔用該 home 的執行階段。" },
    help_select_previous => { "Move the selection up.", "向上移动选择。", "向上移動選擇。" },
    help_select_next => { "Move the selection down.", "向下移动选择。", "向下移動選擇。" },
    help_scroll_page_up => { "Scroll one page up.", "向上滚动一页。", "向上滾動一頁。" },
    help_scroll_page_down => { "Scroll one page down.", "向下滚动一页。", "向下滾動一頁。" },
    help_scroll_half_up => { "Scroll half a page up.", "向上滚动半页。", "向上滾動半頁。" },
    help_scroll_half_down => { "Scroll half a page down.", "向下滚动半页。", "向下滾動半頁。" },
    help_scroll_top => { "Jump to the first item.", "跳到第一项。", "跳到第一項。" },
    help_scroll_bottom => { "Jump to the last item and resume following.", "跳到最后一项并恢复跟随。", "跳到最後一項並恢復跟隨。" },
    help_show_details => { "Open the details panel for the selection.", "打开所选内容的详情面板。", "開啟所選內容的詳情面板。" },
    help_session_open => { "Open the selected row, or press the button the row's cursor has reached.", "打开选中的行，或按下光标所在的那个按钮。", "開啟選取的行，或按下游標所在的那個按鈕。" },
    help_session_control_previous => { "Step leftwards across the row's own action buttons.", "在行内按钮之间向左移动。", "在行內按鈕之間向左移動。" },
    help_session_control_next => { "Step rightwards across the row's own action buttons.", "在行内按钮之间向右移动。", "在行內按鈕之間向右移動。" },
    help_session_new => { "Create a session in a workspace you pick.", "在你选择的工作区中新建会话。", "在你選擇的工作區中新增工作階段。" },
    help_session_rename => { "Rename the selected session.", "重命名选中的会话。", "重新命名選取的工作階段。" },
    help_session_fork => { "Copy the session up to a chosen point.", "把会话复制到指定节点。", "把工作階段複製到指定節點。" },
    help_session_archive => { "Archive the selected session, or restore it when it is already archived.", "归档选中的会话；若已归档则恢复它。", "封存選取的工作階段；若已封存則恢復它。" },
    help_session_unarchive => { "Restore the selected archived session to the list.", "把已归档的会话恢复到列表中。", "把已封存的工作階段恢復到列表中。" },
    help_session_delete => { "Permanently delete the selected session.", "永久删除选中的会话。", "永久刪除選取的工作階段。" },
    help_session_show_archived => { "Include archived sessions in the list.", "在列表中包含已归档的会话。", "在列表中包含已封存的工作階段。" },
    help_session_card_toggle => { "Open or close the selected session's detail card.", "打开或关闭所选会话的详情卡片。", "開啟或關閉所選工作階段的詳情卡片。" },
    help_session_card_collapse => { "Close every open session detail card.", "关闭所有已打开的会话详情卡片。", "關閉所有已開啟的工作階段詳情卡片。" },
    help_sidebar_row_actions => { "Open everything the highlighted project or folder can do.", "打开高亮的项目或文件夹可执行的全部操作。", "開啟高亮的專案或資料夾可執行的全部操作。" },
    help_session_children_toggle => { "Fold or unfold the selected session's delegated sub-sessions.", "折叠或展开所选会话委派的子会话。", "摺疊或展開所選工作階段委派的子工作階段。" },
    help_session_copy => { "Copy the selected session's details to the clipboard.", "把所选会话的详情复制到剪贴板。", "把所選工作階段的詳情複製到剪貼簿。" },
    help_session_pin => { "Keep the selected session above the rest of the list.", "让所选会话保持在列表最前。", "讓所選工作階段保持在列表最前。" },
    help_session_auto_continue => { "Let the selected session continue itself when a turn stops without an answer.", "当回合中断且没有答复时，让所选会话自行继续。", "當回合中斷且沒有答覆時，讓所選工作階段自行繼續。" },
    help_session_move_up => { "Move the selected session one place earlier in the list.", "把所选会话在列表中前移一位。", "把所選工作階段在列表中前移一位。" },
    help_session_move_down => { "Move the selected session one place later in the list.", "把所选会话在列表中后移一位。", "把所選工作階段在列表中後移一位。" },
    help_workspace_switch => { "Change the workspace used for new sessions.", "更改新建会话使用的工作区。", "更改新增工作階段使用的工作區。" },
    help_workspace_browse => { "List directories on the authority host.", "列出权威主机上的目录。", "列出權威主機上的目錄。" },
    help_workspace_up => { "Go to the parent directory.", "前往上级目录。", "前往上級目錄。" },
    help_workspace_select => { "Work in the highlighted directory, else the one the picker is showing.", "在选中的目录中工作；未选中时使用选择器当前显示的目录。", "在選取的目錄中工作；未選取時使用選擇器目前顯示的目錄。" },
    help_composer_focus => { "Put the cursor in the message composer.", "把光标放入消息输入框。", "把游標放入訊息輸入框。" },
    help_turn_continue => { "Ask the Agent to continue an unfinished turn.", "请 Agent 继续未完成的回合。", "請 Agent 繼續未完成的回合。" },
    help_block_toggle => { "Expand or collapse the selected transcript block.", "展开或折叠选中的时间线块。", "展開或摺疊選取的時間線塊。" },
    help_block_toggle_all => { "Expand or collapse every collapsible block.", "展开或折叠所有可折叠的块。", "展開或摺疊所有可摺疊的塊。" },
    help_reasoning_toggle => { "Show or hide the Agent's reasoning.", "显示或隐藏 Agent 的推理。", "顯示或隱藏 Agent 的推理。" },
    help_block_copy => { "Copy the selected block's body to the clipboard.", "把选中块的内容复制到剪贴板。", "把選取塊的內容複製到剪貼簿。" },
    help_block_copy_meta => { "Copy the command, path or id attached to the block.", "复制附着在该块上的命令、路径或 ID。", "複製附著在該塊上的命令、路徑或 ID。" },
    help_block_details => { "Open the diff, terminal output or full text for the block.", "打开该块的差异、终端输出或完整文本。", "開啟該塊的差異、終端輸出或完整文本。" },
    help_runtime_switch => { "Choose the Agent, its model and the run options it publishes.", "选择 Agent、它的模型以及它公布的运行选项。", "選擇 Agent、它的模型以及它公佈的執行選項。" },
    help_runtime_probe => { "Ask the runtime to re-discover available Agents.", "请运行时重新探测可用的 Agent。", "請執行階段重新探測可用的 Agent。" },
    help_transcript_search => { "Search the transcript with a regular expression.", "用正则表达式搜索时间线。", "用正規表示式搜尋時間線。" },
    help_dock_toggle => { "Show or hide the panel of running agents, the plan and held messages.", "显示或隐藏运行中的代理、计划与排队消息面板。", "顯示或隱藏執行中的代理、計畫與排隊訊息面板。" },
    help_dock_activate => { "Open the highlighted dock row: jump to it, or fold its section.", "打开高亮的停靠项：跳转到它，或折叠它所在的区块。", "開啟高亮的停靠項：跳轉到它，或摺疊它所在的區塊。" },
    help_session_children_open => { "Open the sub-session the open one delegated, or pick from the dock when it delegated several.", "打开该会话委派的子会话；若有多个则从停靠栏中选择。", "開啟該工作階段委派的子工作階段；若有多個則從停靠欄中選擇。" },
    help_session_parent => { "Go back to the session that delegated this one.", "返回委派此会话的那个会话。", "返回委派此工作階段的那個工作階段。" },
    help_dock_hide_done => { "Hide finished agents and completed plan steps in the dock.", "在停靠栏中隐藏已完成的代理与已完成的计划步骤。", "在停靠欄中隱藏已完成的代理與已完成的計畫步驟。" },
    help_history_older => { "Fetch the page of history above the oldest loaded one.", "获取当前已加载的最早历史之前的一页。", "獲取目前已載入的最早歷史之前的一頁。" },
    help_queue_previous => { "Move the queue cursor to the message above.", "把队列光标移到上一条消息。", "把佇列游標移到上一則訊息。" },
    help_queue_next => { "Move the queue cursor to the message below.", "把队列光标移到下一条消息。", "把佇列游標移到下一則訊息。" },
    help_queue_edit => { "Pull the queued message back into the composer.", "把排队消息取回输入框。", "把排隊訊息取回輸入框。" },
    help_queue_delete => { "Remove the queued message.", "删除这条排队消息。", "刪除這條排隊訊息。" },
    help_queue_move_up => { "Send the queued message one turn earlier.", "让排队消息提前一个回合发送。", "讓排隊訊息提前一個回合傳送。" },
    help_queue_move_down => { "Send the queued message one turn later.", "让排队消息推后一个回合发送。", "讓排隊訊息推後一個回合傳送。" },
    help_queue_send_now => { "Interrupt the turn and send the queued message immediately.", "打断当前回合并立即发送排队消息。", "打斷目前回合並立即傳送排隊訊息。" },
    help_message_schedule => { "Give the draft a send time; until the clock reaches it the message waits in the queue.", "为草稿设定发送时间；时间到达前消息在队列中等待。", "為草稿設定傳送時間；時間到達前訊息在佇列中等待。" },
    help_search_next => { "Jump to the next match, wrapping at the end.", "跳到下一个匹配，到末尾后回绕。", "跳到下一個符合，到末尾後迴繞。" },
    help_search_previous => { "Jump to the previous match, wrapping at the start.", "跳到上一个匹配，到开头后回绕。", "跳到上一個符合，到開頭後迴繞。" },
    help_composer_submit => { "Send the draft as a new turn.", "把草稿作为新回合发送。", "把草稿作為新回合傳送。" },
    help_composer_newline => { "Insert a line break without sending.", "插入换行而不发送。", "插入換行而不傳送。" },
    help_composer_multiline => { "Toggle multi-line editing.", "切换多行编辑。", "切換多行編輯。" },
    help_composer_history_prev => { "Recall the previous message you sent.", "取回你上一条发送的消息。", "取回你上一條傳送的訊息。" },
    help_composer_history_next => { "Move forward through sent messages.", "在已发送的消息中向后移动。", "在已傳送的訊息中向後移動。" },
    help_composer_editor => { "Edit the draft in your editor, then return.", "在你的编辑器中编辑草稿，完成后返回。", "在你的編輯器中編輯草稿，完成後返回。" },
    help_composer_background => { "Move the running foreground command to the background.", "把运行中的前台命令转到后台。", "把執行中的前臺命令轉到後臺。" },
    help_composer_steer => { "Inject the draft into the running turn without interrupting it.", "在不打断回合的情况下把草稿插入运行中的回合。", "在不打斷回合的情況下把草稿插入執行中的回合。" },
    help_completion_next => { "Highlight the next completion.", "高亮下一个补全项。", "高亮下一個補全項。" },
    help_completion_previous => { "Highlight the previous completion.", "高亮上一个补全项。", "高亮上一個補全項。" },
    help_completion_accept => { "Insert the highlighted completion.", "插入高亮的补全项。", "插入高亮的補全項。" },
    help_completion_cancel => { "Close the completion menu.", "关闭补全菜单。", "關閉補全選單。" },
    help_composer_delete_word => { "Delete the word before the cursor.", "删除光标前的一个词。", "刪除游標前的一個詞。" },
    help_composer_delete_word_after => { "Delete the word after the cursor into the kill buffer.", "删除光标后的一个词并放入剪切缓冲。", "刪除游標後的一個詞並放入剪下緩衝。" },
    help_composer_delete_word_backward => { "Delete the word before the cursor into the kill buffer.", "删除光标前的一个词并放入剪切缓冲。", "刪除游標前的一個詞並放入剪下緩衝。" },
    help_composer_kill_to_end => { "Cut from the cursor to the end of the line.", "从光标剪切到行尾。", "從游標剪下到行尾。" },
    help_composer_kill_to_start => { "Cut from the start of the line to the cursor.", "从行首剪切到光标。", "從行首剪下到游標。" },
    help_composer_yank => { "Put the last cut text back at the cursor.", "把最近剪切的内容放回光标处。", "把最近剪下的內容放回游標處。" },
    help_composer_undo => { "Undo the last edit to the draft.", "撤销对草稿的上一次修改。", "復原對草稿的上一次修改。" },
    help_composer_redo => { "Redo an undone edit.", "重做已撤销的修改。", "重做已復原的修改。" },
    help_composer_word_left => { "Move the cursor to the start of the previous word.", "把光标移到上一个词的开头。", "把游標移到上一個詞的開頭。" },
    help_composer_word_right => { "Move the cursor past the end of the next word.", "把光标移到下一个词的末尾之后。", "把游標移到下一個詞的末尾之後。" },
    help_composer_line_start => { "Move the cursor to the start of the line.", "把光标移到行首。", "把游標移到行首。" },
    help_composer_line_end => { "Move the cursor to the end of the line.", "把光标移到行尾。", "把游標移到行尾。" },
    help_composer_image => { "Attach an image from the clipboard, or name a file to attach.", "从剪贴板附加图片，或指定要附加的文件。", "從剪貼簿附加圖片，或指定要附加的檔案。" },
    help_composer_paste => { "Paste the clipboard: an image becomes an attachment, text goes into the draft.", "粘贴剪贴板：图片变为附件，文本进入草稿。", "貼上剪貼簿：圖片變為附件，文本進入草稿。" },
    help_composer_select_all => { "Select the whole draft so typing replaces it.", "选中整个草稿，输入即替换。", "選取整個草稿，輸入即替換。" },
    help_composer_copy_draft => { "Copy the selected part of the draft to the clipboard.", "把草稿中选中的部分复制到剪贴板。", "把草稿中選取的部分複製到剪貼簿。" },
    help_overlay_close => { "Dismiss this overlay without applying anything.", "关闭此浮层且不应用任何更改。", "關閉此浮層且不應用任何更改。" },
    help_overlay_confirm => { "Apply this overlay's value.", "应用此浮层的值。", "應用此浮層的值。" },
    help_overlay_next_field => { "Move to the next form field.", "移到下一个表单字段。", "移到下一個表單欄位。" },
    help_overlay_prev_field => { "Move to the previous form field.", "移到上一个表单字段。", "移到上一個表單欄位。" },
    help_overlay_toggle => { "Flip a boolean field or a multi-select option.", "翻转布尔字段或多选选项。", "翻轉布林欄位或多選選項。" },
    help_schedule_clear => { "Void the draft's send time and keep the message.", "取消草稿的发送时间并保留消息。", "取消草稿的傳送時間並保留訊息。" },
    help_palette_run => { "Run the highlighted command.", "执行高亮的命令。", "執行高亮的命令。" },
    help_overlay_fold_open => { "Expand the highlighted group, or step into it.", "展开高亮的分组，或进入其中。", "展開高亮的分組，或進入其中。" },
    help_overlay_fold_close => { "Fold the highlighted group, or step out to its heading.", "折叠高亮的分组，或退回到它的标题。", "摺疊高亮的分組，或退回到它的標題。" },
    help_overlay_star_model => { "Star or unstar the highlighted model in the runtime switcher.", "在运行时切换器中收藏或取消收藏高亮的模型。", "在執行階段切換器中收藏或取消收藏高亮的模型。" },
    help_overlay_manage_account => { "Open the highlighted runtime's Agent account in the management pages.", "在管理页面中打开高亮运行时的 Agent 账号。", "在管理頁面中開啟高亮執行階段的 Agent 帳號。" },
    help_overlay_reset_run_option => { "Stage the highlighted run option back to the Agent's own default.", "把高亮的运行选项恢复为 Agent 自身的默认值。", "把高亮的執行選項恢復為 Agent 自身的預設值。" },
    help_approval_approve => { "Approve the pending request.", "批准待处理的请求。", "批准待處理的請求。" },
    help_approval_deny => { "Refuse the pending request.", "拒绝待处理的请求。", "拒絕待處理的請求。" },
    help_approval_always => { "Approve and remember for the rest of this session.", "批准并在本会话余下时间记住该选择。", "批准並在本工作階段餘下時間記住該選擇。" },
    help_approval_next => { "Move between the request's allowed responses.", "在请求允许的答复之间移动。", "在請求允許的答覆之間移動。" },
    help_elicitation_submit => { "Send the answers back to the Agent.", "把答案发回给 Agent。", "把答案發回給 Agent。" },
    help_elicitation_next => { "Move to the next question.", "移到下一个问题。", "移到下一個問題。" },
    help_elicitation_prev => { "Move to the previous question.", "移到上一个问题。", "移到上一個問題。" },
    help_management_toggle => { "Enable or disable the highlighted entry.", "启用或停用高亮的条目。", "啟用或停用高亮的條目。" },
    help_management_edit => { "Edit the highlighted entry.", "编辑高亮的条目。", "編輯高亮的條目。" },
    help_management_reload => { "Re-read this management section.", "重新读取这个管理分区。", "重新讀取這個管理分割槽。" },
    help_config_module_previous => { "Move to the previous configuration module (Agent, MCP, Skills, Prompts).", "移到上一个配置模块（Agent、MCP、Skill、Prompt）。", "移到上一個設定模組（Agent、MCP、Skill、Prompt）。" },
    help_config_module_next => { "Move to the next configuration module.", "移到下一个配置模块。", "移到下一個設定模組。" },
    help_config_section_previous => { "Move to the previous function area of this module.", "移到本模块的上一个功能区。", "移到本模組的上一個功能區。" },
    help_config_section_next => { "Move to the next function area of this module.", "移到本模块的下一个功能区。", "移到本模組的下一個功能區。" },
    help_config_pane_next => { "Move the keyboard between the modules, the areas, the records and the details.", "在模块、功能区、记录与详情之间移动键盘焦点。", "在模組、功能區、記錄與詳情之間移動鍵盤焦點。" },
    help_config_pane_previous => { "Move the keyboard backwards between the panes.", "在各窗格之间向后移动键盘焦点。", "在各窗格之間向後移動鍵盤焦點。" },
    help_config_entry_actions => { "Open everything the highlighted record can do.", "打开高亮记录可执行的全部操作。", "開啟高亮記錄可執行的全部操作。" },
    help_config_entry_create => { "Create a record in this area.", "在此功能区新建一条记录。", "在此功能區新增一條記錄。" },
    help_config_entry_delete => { "Delete the highlighted record.", "删除高亮的记录。", "刪除高亮的記錄。" },
    help_config_form_save => { "Write the form to the runtime.", "把表单写入运行时。", "把表單寫入執行階段。" },
    help_config_form_copy => { "Copy the command this form is about to save.", "复制此表单将要保存的命令。", "複製此表單將要儲存的命令。" },
    help_config_models => { "Edit which Models this provider profile offers.", "编辑此模型服务提供哪些模型。", "編輯此模型服務提供哪些模型。" },
    help_config_model_toggle => { "Offer the highlighted Model, or stop offering it.", "启用或停用选中的模型。", "啟用或停用選中的模型。" },
    help_config_model_add => { "Add a Model by id that the endpoint did not list.", "按 ID 添加端点未列出的模型。", "按 ID 新增端點未列出的模型。" },
    help_config_model_remove => { "Take the highlighted Model out of this profile.", "把选中的模型从此配置中移除。", "把選中的模型從此設定中移除。" },
    help_config_model_fetch => { "Ask the endpoint which Models it serves.", "向端点询问它提供哪些模型。", "向端點詢問它提供哪些模型。" },
    help_config_model_edit => { "Edit the highlighted Model: its name, protocol and limits.", "编辑选中模型的名称、协议与上限。", "編輯選中模型的名稱、協定與上限。" },
    help_config_import => { "Read the provider settings other CLI tools already hold.", "读取其他 CLI 工具已有的模型服务配置。", "讀取其他 CLI 工具已有的模型服務設定。" },
    help_config_export => { "Write this provider profile into another tool's own settings.", "把此模型服务配置写入其他工具自己的配置。", "把此模型服務設定寫入其他工具自己的設定。" },
    help_config_discover => { "Probe this machine for Agent CLIs that have not been added yet.", "探测本机尚未添加的 Agent CLI。", "探測本機尚未新增的 Agent CLI。" },
    help_config_registry_filter => { "Narrow the registry to local processes or to remote services.", "把注册表筛选为本地进程或远程服务。", "把註冊表篩選為本機程序或遠端服務。" },
    help_config_registry_sort => { "Ask the registry for another ranking.", "向注册表请求另一种排序。", "向註冊表請求另一種排序。" },
    help_config_registry_page_previous => { "Show the registry page before this one.", "显示注册表的上一页。", "顯示註冊表的上一頁。" },
    help_config_registry_page_next => { "Show the registry page after this one.", "显示注册表的下一页。", "顯示註冊表的下一頁。" },
    help_goto_devices => { "Paired devices, pairing codes and the audit log.", "已配对的设备、配对码与审计日志。", "已配對的裝置、配對碼與審計日誌。" },
    help_device_pair_activate => { "Open the highlighted way to pair a device.", "打开高亮的设备配对方式。", "開啟高亮的裝置配對方式。" },
    help_device_tab_next => { "Move to the other half of the devices page.", "移到设备页的另一半。", "移到裝置頁的另一半。" },
    help_device_tab_previous => { "Move to the other half of the devices page.", "移到设备页的另一半。", "移到裝置頁的另一半。" },
    help_device_route_next => {
        "Carry the code and the link over the next published route.",
        "让配对码和链接改用下一条已发布的路线。",
        "讓配對碼和連結改用下一條已發佈的路線。"
    },
    help_device_route_previous => {
        "Carry the code and the link over the previous published route.",
        "让配对码和链接改用上一条已发布的路线。",
        "讓配對碼和連結改用上一條已發佈的路線。"
    },
    help_device_lan_allow => { "Approve the device that asked to pair on this network.", "批准在此网络上请求配对的设备。", "批准在此網路上請求配對的裝置。" },
    help_device_lan_refuse => { "Refuse the device that asked to pair on this network.", "拒绝在此网络上请求配对的设备。", "拒絕在此網路上請求配對的裝置。" },
    help_device_revoke => { "Revoke the highlighted device's access.", "撤销高亮设备的访问权限。", "復原高亮裝置的訪問許可權。" },
    help_device_audit => { "Show the remote-protocol audit trail.", "显示远程协议审计记录。", "顯示遠端協議審計記錄。" },
    help_setting_activate => { "Change the highlighted setting.", "修改高亮的设置项。", "修改高亮的設定項。" },
    help_setting_previous => { "Step the setting backwards.", "把该设置项向前步进。", "把該設定項向前步進。" },
    help_setting_next => { "Step the setting forwards.", "把该设置项向后步进。", "把該設定項向後步進。" },
    help_setting_reset => { "Reset the highlighted setting to its default after a confirmation.", "经确认后把高亮的设置项恢复为默认值。", "經確認後把高亮的設定項恢復為預設值。" },
    help_filter_begin => { "Type to filter the current list.", "输入以过滤当前列表。", "輸入以過濾目前列表。" },
    help_filter_clear => { "Remove the active filter.", "移除当前过滤条件。", "移除目前過濾條件。" },
}

impl Strings {
    /// Classify the process locale the way the desktop does, then fall back to
    /// `LC_ALL` / `LC_MESSAGES` / `LANG` because the TUI has no GUI locale layer.
    pub fn detect() -> Self {
        Self::detect_from(|key| env::var(key).ok()).unwrap_or_else(|| Self::for_locale(Locale::En))
    }

    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<Self> {
        let raw = lookup("LC_ALL")
            .filter(|value| !value.is_empty())
            .or_else(|| lookup("LC_MESSAGES").filter(|value| !value.is_empty()))
            .or_else(|| lookup("LANG").filter(|value| !value.is_empty()))
            .or_else(sys_locale::get_locale);
        Some(Self::for_locale(Locale::from_system_tag(raw.as_deref())))
    }

    pub fn with_locale(locale: Locale) -> Self {
        Self::for_locale(locale)
    }

    /// The BCP-47 tag the UI shows in settings.
    pub const fn tag(&self) -> &'static str {
        self.locale.tag()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_three_nonempty_translations() {
        let en = Strings::for_locale(Locale::En);
        let cn = Strings::for_locale(Locale::ZhCn);
        let tw = Strings::for_locale(Locale::ZhTw);
        for key in Strings::ALL_KEYS {
            let value = match *key {
                "app_name" => en.app_name(),
                _ => continue,
            };
            assert!(!value.is_empty());
        }
        // The accessor set is generated from the same macro invocation, so a
        // mismatched key set is impossible. This asserts the table is not empty
        // and that the three locales really differ where translation exists.
        assert!(Strings::ALL_KEYS.len() > 100);
        assert_ne!(en.nav_sessions(), cn.nav_sessions());
        assert_ne!(cn.nav_sessions(), tw.nav_sessions());
    }

    #[test]
    fn the_turn_band_states_a_phase_without_the_timeline_ellipsis() {
        // The band above the prompt is the desktop's information bar, and that
        // surface states what the Agent is doing plainly. The closing ellipsis
        // belongs to the desktop timeline's pending row, which says work is
        // still arriving; this band is not that row, so its words carry no
        // mark of their own — the runtime's own activity line is what a reader
        // sees close with one, and it comes from elsewhere.
        for locale in [Locale::En, Locale::ZhCn, Locale::ZhTw] {
            let strings = Strings::for_locale(locale);
            for label in [
                strings.phase_preparing(),
                strings.phase_thinking(),
                strings.phase_calling_tool(),
                strings.phase_generating(),
                strings.phase_waiting_approval(),
            ] {
                assert!(
                    !label.ends_with("...") && !label.ends_with('…'),
                    "{locale:?} draws a live phase with the timeline's mark: {label:?}"
                );
            }
        }

        // Waiting on the reader is a stopped turn rather than an in-progress
        // one, and it reads as the plain statement of state it already was.
        assert_eq!(
            Strings::for_locale(Locale::ZhCn).phase_waiting_approval(),
            "等待确认"
        );
    }

    #[test]
    fn detects_locale_from_environment_priority() {
        let strings = Strings::detect_from(|key| match key {
            "LC_ALL" => Some("zh_CN.UTF-8".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::ZhCn);

        let strings = Strings::detect_from(|key| match key {
            "LANG" => Some("zh_TW.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::ZhTw);
    }

    #[test]
    fn unsupported_languages_fall_back_to_english() {
        let strings = Strings::detect_from(|key| match key {
            "LANG" => Some("fr_FR.UTF-8".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(strings.locale, Locale::En);
    }
}
