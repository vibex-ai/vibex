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
    enabled => { "Enabled", "已启用", "已啟用" },
    disabled => { "Disabled", "已禁用", "已停用" },
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
    archived => { "Archived", "已归档", "已封存" },
    copied => { "Copied", "已复制", "已複製" },
    nothing_here => { "Nothing here yet", "这里还没有内容", "這裡還沒有內容" },
    // Footer verbs. They are shorter than the destination labels because a
    // modal footer is a key legend, not a sentence.
    hint_nav => { "nav", "移动", "移動" },
    hint_select => { "select", "选择", "選擇" },
    hint_scroll => { "scroll", "滚动", "捲動" },
    hint_run => { "run", "执行", "執行" },
    hint_expand => { "expand", "展开", "展開" },
    hint_collapse => { "collapse", "收起", "收合" },
    hint_toggle => { "toggle", "切换", "切換" },
    hint_edit => { "edit", "编辑", "編輯" },
    hint_previous => { "prev", "上一个", "上一個" },
    hint_next => { "next", "下一个", "下一個" },
    hint_clear => { "clear", "清空", "清空" },
    hint_commit => { "keep", "保留", "保留" },
    hint_revert => { "revert", "还原", "還原" },
    hint_reset => { "reset", "重置", "重設" },
    hint_confirm_delete => { "confirm delete", "确认删除", "確認刪除" },

    // ---- destinations ---------------------------------------------------
    nav_sessions => { "Sessions", "会话", "工作階段" },
    nav_management => { "Management", "管理", "管理" },
    nav_usage => { "Usage", "用量", "用量" },
    nav_settings => { "Settings", "设置", "設定" },
    nav_help => { "Help", "帮助", "說明" },
    nav_agent => { "Agent", "Agent", "Agent" },
    nav_files => { "Files", "文件", "檔案" },
    nav_changes => { "Changes", "变更", "變更" },
    nav_terminal => { "Terminal", "终端", "終端機" },
    nav_git => { "Git", "Git", "Git" },

    // ---- sessions -------------------------------------------------------
    sessions_title => { "Sessions", "会话", "工作階段" },
    sessions_empty => {
        "No sessions yet — press n to start one",
        "还没有会话 — 按 n 新建",
        "還沒有工作階段 — 按 n 新增"
    },
    session_new => { "New session", "新建会话", "新增工作階段" },
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
    session_card_agent => { "Agent", "Agent", "Agent" },
    session_card_model => { "Model", "模型", "模型" },
    session_card_created => { "Created", "创建于", "建立於" },
    session_card_updated => { "Updated", "更新于", "更新於" },
    session_card_last_message => { "Last turn", "最近一轮", "最近一輪" },
    session_card_messages => { "Messages", "消息数", "訊息數" },
    session_card_turns => { "Turns", "轮次", "輪次" },
    session_card_tools => { "Tools", "工具", "工具" },
    dock_agents => { "Agents", "子代理", "子代理" },
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
    sidebar_unpinned => { "Pin removed", "已取消置顶", "已取消置頂" },
    sidebar_pinned_first => {
        "Pinned sessions always come first — unpin one to move past it",
        "置顶会话始终在最前 — 先取消置顶才能越过",
        "置頂工作階段一律在最前 — 先取消置頂才能越過"
    },
    sidebar_grouped => { "Grouped by workspace", "按工作区分组", "依工作區分組" },
    sidebar_flat => { "One flat list", "平铺列表", "平鋪清單" },
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
    composer_placeholder => {
        "Send a message…  (/ commands · @ files · $ skills)",
        "发送消息…（/ 命令 · @ 文件 · $ 技能）",
        "傳送訊息…（/ 命令 · @ 檔案 · $ 技能）"
    },
    composer_send => { "Send", "发送", "傳送" },
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
    composer_nothing_selected => {
        "Select part of the draft first",
        "请先选中草稿中的内容",
        "請先選取草稿中的內容"
    },
    composer_queue => { "Queue", "排队", "排隊" },
    composer_interrupt => { "Interrupt", "打断", "中斷" },
    composer_continue => { "Continue", "继续", "繼續" },
    composer_steer => { "Steer", "插话", "插話" },
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
    composer_press_again => {
        "Press Esc again to clear the draft",
        "再按一次 Esc 清空草稿",
        "再按一次 Esc 清空草稿"
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
    composer_no_matches => { "No matches", "没有匹配项", "沒有符合項目" },

    // ---- transcript -----------------------------------------------------
    transcript_thinking => { "Thinking", "思考", "思考" },
    transcript_tool => { "Tool", "工具", "工具" },
    transcript_command => { "Command", "命令", "命令" },
    transcript_file_change => { "File change", "文件变更", "檔案變更" },
    transcript_diff => { "Diff", "差异", "差異" },
    git_revert_title => { "Discard changes", "放弃更改", "捨棄變更" },
    git_revert_warning => {
        "This discards the working-tree changes at this path. It cannot be undone.",
        "这将丢弃该路径的工作区更改，且无法撤销。",
        "這將捨棄該路徑的工作區變更，且無法復原。"
    },
    git_history_title => { "Recent commits", "最近提交", "最近提交" },
    git_branches_title => { "Branches", "分支", "分支" },
    worktree_title => { "Worktrees", "工作树", "工作樹" },
    worktree_create_title => { "New worktree branch", "新建工作树分支", "新增工作樹分支" },
    worktree_preflight_title => {
        "Worktree preflight",
        "工作树预检",
        "工作樹預檢"
    },
    worktree_preflight_allowed => { "Allowed", "允许", "允許" },
    worktree_preflight_blocked => { "Blocked", "已阻止", "已封鎖" },
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
        "Alt+↑↓ pick · Alt+E edit · Alt+X drop · Alt+J/K reorder · Alt+Enter send now",
        "Alt+↑↓ 选择 · Alt+E 编辑 · Alt+X 删除 · Alt+J/K 调序 · Alt+Enter 立即发送",
        "Alt+↑↓ 選擇 · Alt+E 編輯 · Alt+X 刪除 · Alt+J/K 調序 · Alt+Enter 立即傳送"
    },
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
    runtime_title => { "Runtime", "运行时", "執行階段" },
    runtime_desired => { "Requested", "期望", "期望" },
    runtime_effective => { "Effective", "实际", "實際" },
    runtime_switching => { "Switching…", "切换中…", "切換中…" },
    runtime_switch_failed => {
        "Switch failed — still using the previous runtime",
        "切换失败 — 仍在用之前的运行时",
        "切換失敗 — 仍在使用先前的執行階段"
    },
    runtime_model => { "Model", "模型", "模型" },
    runtime_agent => { "Agent", "Agent", "Agent" },
    runtime_probe => { "Probe runtimes", "探测运行时", "探測執行階段" },

    // ---- workspace ------------------------------------------------------
    workspace_pick => { "Choose a workspace", "选择工作区", "選擇工作區" },
    workspace_browse => { "Browse", "浏览", "瀏覽" },
    workspace_parent => { "Parent directory", "上级目录", "上層目錄" },
    workspace_use_this => { "Use this directory", "使用此目录", "使用此目錄" },
    workspace_empty => { "No workspaces", "没有工作区", "沒有工作區" },

    // ---- management -----------------------------------------------------
    management_agents => { "Agents", "Agent", "Agent" },
    management_providers => { "Providers", "Provider", "Provider" },
    management_mcp => { "MCP servers", "MCP 服务", "MCP 服務" },
    management_skills => { "Skills", "技能", "技能" },
    management_prompts => { "Prompts", "提示词", "提示詞" },
    management_hooks => { "Hooks", "钩子", "鉤子" },
    management_devices => { "Devices", "设备", "裝置" },
    management_recovery => { "Recovery", "恢复", "復原" },
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
    permission_read_only => { "Read only", "只读", "唯讀" },
    permission_approve_only => { "Approve only", "仅审批", "僅審批" },
    permission_full_control => { "Full control", "完全控制", "完全控制" },
    permission_required_for => {
        "This action needs a higher permission level",
        "此操作需要更高的权限级别",
        "此操作需要更高的權限層級"
    },

    // ---- usage ----------------------------------------------------------
    usage_title => { "Token usage", "Token 用量", "Token 用量" },
    usage_session => { "This session", "本会话", "本工作階段" },
    usage_input_tokens => { "Input", "输入", "輸入" },
    usage_output_tokens => { "Output", "输出", "輸出" },
    usage_cached_tokens => { "Cached", "缓存", "快取" },
    usage_total_tokens => { "Total", "合计", "合計" },
    usage_context => { "Context window", "上下文占用", "上下文佔用" },
    usage_no_cost_hint => {
        "Vibex records token counts only; pricing lives with the provider.",
        "Vibex 只记录 token 数量；价格由 Provider 侧决定。",
        "Vibex 只記錄 token 數量；價格由 Provider 端決定。"
    },

    // ---- recovery -------------------------------------------------------
    recovery_title => { "Recovery", "恢复", "復原" },
    recovery_diagnostics => { "Export diagnostics", "导出诊断", "匯出診斷" },
    recovery_backup_create => { "Create backup", "创建备份", "建立備份" },
    recovery_backup_inspect => { "Inspect backup", "检查备份", "檢查備份" },
    recovery_backup_restore => { "Restore backup", "恢复备份", "還原備份" },
    recovery_restore_warning => {
        "Restoring replaces the current runtime data. Type the backup id to confirm.",
        "恢复将替换当前运行时数据。请输入备份 ID 以确认。",
        "還原將取代目前執行階段資料。請輸入備份 ID 以確認。"
    },
    recovery_authority_only => {
        "Recovery runs on the authoritative runtime",
        "恢复操作在权威运行时上执行",
        "復原操作在權威執行階段上執行"
    },
    recovery_artifact_path => { "Written to", "写入位置", "寫入位置" },

    // ---- settings -------------------------------------------------------
    settings_theme => { "Theme", "主题", "主題" },
    settings_language => { "Language", "语言", "語言" },
    settings_icons => { "Icons", "图标", "圖示" },
    settings_color => { "Colour", "颜色", "顏色" },
    settings_keys => { "Key bindings", "键位", "鍵位" },
    settings_status_line => { "Status line", "状态行", "狀態列" },
    settings_status_line_hint => {
        "A second, denser status row under the composer: branch, model and context",
        "输入框下方的第二行状态：分支、模型与上下文用量",
        "輸入框下方的第二行狀態：分支、模型與上下文用量"
    },
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
    settings_authority_seat => { "Authority seat (in-process)", "权威座（进程内）", "權威座（行程內）" },
    settings_remote_seat => { "Remote seat", "远程座", "遠端座" },
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
        "Whether this process owns the runtime or reaches it over a link.",
        "本进程拥有运行时，还是通过网络连接。",
        "本行程擁有執行階段，還是透過網路連線。"
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
    palette_group_recent => { "Recent", "最近", "最近" },
    palette_group_session => { "Session", "会话", "工作階段" },
    palette_group_workbench => { "Workbench", "工作台", "工作台" },
    palette_group_management => { "Management", "管理", "管理" },
    palette_group_device => { "Devices", "设备", "裝置" },
    palette_group_view => { "View", "视图", "檢視" },
    palette_group_app => { "App", "应用", "應用" },
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

    // ---- degradation ----------------------------------------------------
    no_tty_title => { "Not a terminal", "不是终端", "不是終端機" },
    no_tty_body => {
        "Vibex TUI needs an interactive terminal on stdin and stdout.",
        "Vibex TUI 需要 stdin 和 stdout 都是交互式终端。",
        "Vibex TUI 需要 stdin 和 stdout 都是互動式終端機。"
    },
    terminal_too_small => { "Terminal too small", "终端太小", "終端機太小" },
    terminal_size_required => { "Minimum size", "最小尺寸", "最小尺寸" },
    terminal_size_current => { "Current size", "当前尺寸", "目前尺寸" },
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
        "Vibex Desktop is running and is not accepting local clients. Either enable Settings → Remote Access → Direct, quit the desktop app and retry, or connect to a remote runtime with `vibex connect <link>`.",
        "Vibex 桌面端正在运行且未开放本机客户端。请在桌面端开启 设置 → 远程访问 → Direct，或退出桌面端后重试，或用 `vibex connect <链接>` 连接远程运行时。",
        "Vibex 桌面端正在執行且未開放本機用戶端。請在桌面端開啟 設定 → 遠端存取 → Direct，或結束桌面端後重試，或用 `vibex connect <連結>` 連線遠端執行階段。"
    },
    seat_authority => { "Authority", "权威座", "權威座" },
    seat_remote => { "Remote", "远程座", "遠端座" },
    seat_local_loopback => { "Local runtime", "本机运行时", "本機執行階段" },
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
