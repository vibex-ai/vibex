use std::sync::OnceLock;

use vibex_desktop_model::LocaleMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLocale {
    En,
    ZhCn,
    ZhTw,
}

impl ResolvedLocale {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::ZhTw => "zh-TW",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Strings {
    pub sessions: &'static str,
    pub workbench: &'static str,
    pub preview: &'static str,
    pub files_git: &'static str,
    pub loading_runtime: &'static str,
    pub runtime_ready: &'static str,
    pub runtime_using: &'static str,
    pub runtime_waiting: &'static str,
    pub runtime_preparing: &'static str,
    pub runtime_still_using: &'static str,
    pub runtime_use_current: &'static str,
    pub retry: &'static str,
    pub no_workspace: &'static str,
    pub open_workspace: &'static str,
    pub no_preview: &'static str,
    pub no_workspace_files: &'static str,
    pub message_agent: &'static str,
    pub agent_loading_session: &'static str,
    pub agent_start_conversation: &'static str,
    pub agent_select_session: &'static str,
    pub agent_timeline_description: &'static str,
    pub agent_process_events: &'static str,
    pub agent_worked_for: &'static str,
    pub agent_pending_response: &'static str,
    pub agent_expand_process: &'static str,
    pub agent_collapse_process: &'static str,
    pub new_session_title_before_brand: &'static str,
    pub new_session_title_after_brand: &'static str,
    pub new_session_description: &'static str,
    pub new_session_prompt_placeholder: &'static str,
    pub new_session_project_label: &'static str,
    pub new_session_project_placeholder: &'static str,
    pub new_session_search_project: &'static str,
    pub new_session_no_projects: &'static str,
    pub new_session_choose_project: &'static str,
    pub new_session_create: &'static str,
    pub new_session_cancel: &'static str,
    pub new_session_working_in: &'static str,
    pub new_session_no_agents: &'static str,
    pub new_session_add_agent: &'static str,
    pub model_provider_label: &'static str,
    pub model_label: &'static str,
    pub reasoning_depth: &'static str,
    pub conversation_mode: &'static str,
    pub agent_workbench: &'static str,
    pub runtime_unavailable: &'static str,
    pub collapse_sidebar: &'static str,
    pub expand_sidebar: &'static str,
    pub resize_sidebar: &'static str,
    pub go_back: &'static str,
    pub go_forward: &'static str,
    pub pair_mobile: &'static str,
    pub open_settings: &'static str,
    pub settings: &'static str,
    pub settings_description: &'static str,
    pub restore_defaults: &'static str,
    pub restore_defaults_description: &'static str,
    pub restore_defaults_confirm_title: &'static str,
    pub restore_defaults_confirm_description: &'static str,
    pub general: &'static str,
    pub general_description: &'static str,
    pub appearance: &'static str,
    pub appearance_description: &'static str,
    pub theme: &'static str,
    pub theme_description: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
    pub system: &'static str,
    pub system_default: &'static str,
    pub language: &'static str,
    pub language_description: &'static str,
    pub english: &'static str,
    pub simplified_chinese: &'static str,
    pub traditional_chinese: &'static str,
    pub interface_font: &'static str,
    pub interface_font_description: &'static str,
    pub system_ui: &'static str,
    pub choose_interface_font: &'static str,
    pub interface_font_size: &'static str,
    pub interface_font_size_description: &'static str,
    pub interface_font_weight: &'static str,
    pub interface_font_weight_description: &'static str,
    pub code_font: &'static str,
    pub code_font_description: &'static str,
    pub system_monospace: &'static str,
    pub choose_code_font: &'static str,
    pub code_font_size: &'static str,
    pub code_font_size_description: &'static str,
    pub code_font_weight: &'static str,
    pub code_font_weight_description: &'static str,
    pub session_turn_preview_rail: &'static str,
    pub session_turn_preview_rail_description: &'static str,
    pub session_content_width: &'static str,
    pub session_content_width_description: &'static str,
    pub session_content_width_narrow: &'static str,
    pub session_content_width_standard: &'static str,
    pub session_content_width_full: &'static str,
    pub decrease_font_size: &'static str,
    pub increase_font_size: &'static str,
    pub decrease_font_weight: &'static str,
    pub increase_font_weight: &'static str,
    pub session_search_placeholder: &'static str,
    pub session_search_recent: &'static str,
    pub session_search_results: &'static str,
    pub session_search_no_results: &'static str,
    pub session_search_loading: &'static str,
    pub session_search_open: &'static str,
    pub session_search_close: &'static str,
    pub sidebar_new_session: &'static str,
    pub sidebar_providers: &'static str,
    pub sidebar_projects: &'static str,
    pub sidebar_no_matching_sessions: &'static str,
    pub sidebar_new_project: &'static str,
    pub sidebar_import_sessions: &'static str,
    pub sidebar_no_sessions: &'static str,
    pub sidebar_delete_project: &'static str,
    pub sidebar_collapse_all: &'static str,
    pub sidebar_restore: &'static str,
    pub sidebar_batch: &'static str,
    pub sidebar_exit_batch: &'static str,
    pub sidebar_select_all: &'static str,
    pub sidebar_clear: &'static str,
    pub sidebar_delete_selected: &'static str,
    pub sidebar_pin: &'static str,
    pub sidebar_unpin: &'static str,
    pub sidebar_pinned: &'static str,
    pub sidebar_rename: &'static str,
    pub sidebar_delete: &'static str,
    pub sidebar_rename_placeholder: &'static str,
    pub sidebar_rename_empty: &'static str,
    pub sidebar_rename_failed: &'static str,
    pub sidebar_cancel: &'static str,
    pub sidebar_confirm_delete_session: &'static str,
    pub sidebar_confirm_delete_project: &'static str,
    pub sidebar_state_pending: &'static str,
    pub sidebar_state_initializing: &'static str,
    pub sidebar_state_error: &'static str,
    pub sidebar_state_archived: &'static str,
    pub sidebar_yesterday: &'static str,
}

pub fn resolve_locale(mode: LocaleMode, system_locale: Option<&str>) -> ResolvedLocale {
    match mode {
        LocaleMode::En => ResolvedLocale::En,
        LocaleMode::ZhCn => ResolvedLocale::ZhCn,
        LocaleMode::ZhTw => ResolvedLocale::ZhTw,
        LocaleMode::System => {
            let locale = system_locale
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .replace('_', "-");
            if locale.starts_with("zh-tw")
                || locale.starts_with("zh-hk")
                || locale.starts_with("zh-mo")
                || locale.contains("hant")
            {
                ResolvedLocale::ZhTw
            } else if locale.starts_with("zh") {
                ResolvedLocale::ZhCn
            } else {
                ResolvedLocale::En
            }
        }
    }
}

pub fn strings(locale: ResolvedLocale) -> Strings {
    match locale {
        ResolvedLocale::En => Strings {
            sessions: "Sessions",
            workbench: "Workbench",
            preview: "Preview",
            files_git: "Files & Git",
            loading_runtime: "Starting local runtime",
            runtime_ready: "Local runtime ready",
            runtime_using: "Using",
            runtime_waiting: "Waiting to switch to",
            runtime_preparing: "Preparing",
            runtime_still_using: "Still using",
            runtime_use_current: "Use current",
            retry: "Retry",
            no_workspace: "No workspace selected",
            open_workspace: "Open a workspace",
            no_preview: "No preview",
            no_workspace_files: "No workspace files",
            message_agent: "Ask Vibex to inspect, edit, test, or explain this workspace...",
            agent_loading_session: "Loading session...",
            agent_start_conversation: "Start a conversation",
            agent_select_session: "Create or select a session",
            agent_timeline_description: "The authoritative timeline and live runtime events will appear here.",
            agent_process_events: "process events",
            agent_worked_for: "Worked for",
            agent_pending_response: "Thinking...",
            agent_expand_process: "Expand process",
            agent_collapse_process: "Collapse process",
            new_session_title_before_brand: "Work with",
            new_session_title_after_brand: "",
            new_session_description: "Create a fresh session from a project directory or a temporary workspace",
            new_session_prompt_placeholder: "What can I help you with?",
            new_session_project_label: "Project directory",
            new_session_project_placeholder: "Optional: enter or paste a project directory path",
            new_session_search_project: "Search projects...",
            new_session_no_projects: "No matching projects",
            new_session_choose_project: "Choose another directory",
            new_session_create: "Create session",
            new_session_cancel: "Back to current session",
            new_session_working_in: "Temporary session",
            new_session_no_agents: "No Agent has been added. Open Config Center to add one.",
            new_session_add_agent: "Add Agent",
            model_provider_label: "Model provider",
            model_label: "Model",
            reasoning_depth: "Thinking depth",
            conversation_mode: "Conversation mode",
            agent_workbench: "Agent workbench",
            runtime_unavailable: "Runtime unavailable",
            collapse_sidebar: "Collapse sidebar",
            expand_sidebar: "Expand sidebar",
            resize_sidebar: "Resize sidebar",
            go_back: "Go back",
            go_forward: "Go forward",
            pair_mobile: "Pair mobile",
            open_settings: "Open settings",
            settings: "Settings",
            settings_description: "Manage desktop preferences for this Vibex workbench.",
            restore_defaults: "Restore defaults",
            restore_defaults_description: "Reset language, theme, typography, code typography, session width, and preview rail to Vibex defaults.",
            restore_defaults_confirm_title: "Restore default settings?",
            restore_defaults_confirm_description: "This resets language, theme, interface typography, code typography, session content width, and session preview rail.",
            general: "General",
            general_description: "Configure default behavior for the desktop shell.",
            appearance: "Appearance",
            appearance_description: "Choose how the desktop shell resolves light and dark mode.",
            theme: "Theme",
            theme_description: "Applies immediately and persists locally on this device.",
            light: "Light",
            dark: "Dark",
            system: "System",
            system_default: "System default",
            language: "Language",
            language_description: "Select the display language preference for this device.",
            english: "English",
            simplified_chinese: "简体中文",
            traditional_chinese: "繁體中文",
            interface_font: "Interface font",
            interface_font_description: "Choose from fonts installed on this device. Editor and terminal fonts stay unchanged.",
            system_ui: "System UI",
            choose_interface_font: "Choose interface font",
            interface_font_size: "Interface font size",
            interface_font_size_description: "Sets the base UI font size in pixels.",
            interface_font_weight: "Interface font weight",
            interface_font_weight_description: "Sets the base UI font weight.",
            code_font: "Code font",
            code_font_description: "Applies to code blocks, terminal surfaces, and the integrated editor.",
            system_monospace: "System monospace",
            choose_code_font: "Choose code font",
            code_font_size: "Code font size",
            code_font_size_description: "Sets the code rendering font size in pixels.",
            code_font_weight: "Code font weight",
            code_font_weight_description: "Sets the code rendering font weight.",
            session_turn_preview_rail: "Session preview rail",
            session_turn_preview_rail_description: "Show the left-edge conversation turn preview inside Agent sessions.",
            session_content_width: "Session content width",
            session_content_width_description: "Controls the shared width of session messages and the composer.",
            session_content_width_narrow: "Narrow",
            session_content_width_standard: "Standard",
            session_content_width_full: "Full width",
            decrease_font_size: "Decrease font size",
            increase_font_size: "Increase font size",
            decrease_font_weight: "Decrease font weight",
            increase_font_weight: "Increase font weight",
            session_search_placeholder: "Search sessions and messages",
            session_search_recent: "Recent sessions",
            session_search_results: "Search results",
            session_search_no_results: "No matching sessions or messages",
            session_search_loading: "Indexing session messages...",
            session_search_open: "Search sessions",
            session_search_close: "Close search",
            sidebar_new_session: "New chat",
            sidebar_providers: "Config Center",
            sidebar_projects: "Projects",
            sidebar_no_matching_sessions: "No matching sessions",
            sidebar_new_project: "New project",
            sidebar_import_sessions: "Import sessions",
            sidebar_no_sessions: "No sessions",
            sidebar_delete_project: "Delete project",
            sidebar_collapse_all: "Collapse all sessions",
            sidebar_restore: "Restore sessions",
            sidebar_batch: "Batch sessions",
            sidebar_exit_batch: "Exit batch mode",
            sidebar_select_all: "Select all",
            sidebar_clear: "Clear",
            sidebar_delete_selected: "Delete selected",
            sidebar_pin: "Pin",
            sidebar_unpin: "Unpin",
            sidebar_pinned: "Pinned",
            sidebar_rename: "Rename",
            sidebar_delete: "Delete",
            sidebar_rename_placeholder: "Session title",
            sidebar_rename_empty: "Enter a session title.",
            sidebar_rename_failed: "Rename failed.",
            sidebar_cancel: "Cancel",
            sidebar_confirm_delete_session: "Delete session?",
            sidebar_confirm_delete_project: "Delete project?",
            sidebar_state_pending: "PENDING",
            sidebar_state_initializing: "INIT",
            sidebar_state_error: "ERROR",
            sidebar_state_archived: "ARCHIVED",
            sidebar_yesterday: "Yesterday",
        },
        ResolvedLocale::ZhCn => Strings {
            sessions: "会话",
            workbench: "工作台",
            preview: "预览",
            files_git: "文件与 Git",
            loading_runtime: "正在启动本地运行时",
            runtime_ready: "本地运行时已就绪",
            runtime_using: "正在使用",
            runtime_waiting: "正在等待切换到",
            runtime_preparing: "正在准备",
            runtime_still_using: "仍在使用",
            runtime_use_current: "使用当前运行时",
            retry: "重试",
            no_workspace: "尚未选择工作区",
            open_workspace: "打开工作区",
            no_preview: "暂无预览",
            no_workspace_files: "暂无工作区文件",
            message_agent: "让 Vibex 检查、编辑、测试或解释此工作区...",
            agent_loading_session: "正在加载会话...",
            agent_start_conversation: "开始对话",
            agent_select_session: "创建或选择会话",
            agent_timeline_description: "权威会话历史和实时运行事件将在这里显示。",
            agent_process_events: "条过程事件",
            agent_worked_for: "工作了",
            agent_pending_response: "思考中...",
            agent_expand_process: "展开过程",
            agent_collapse_process: "收起过程",
            new_session_title_before_brand: "和",
            new_session_title_after_brand: "一起工作",
            new_session_description: "从项目目录或临时工作区创建一个新的会话",
            new_session_prompt_placeholder: "我能为你做哪些事情？",
            new_session_project_label: "项目目录",
            new_session_project_placeholder: "可选：输入或粘贴项目目录路径",
            new_session_search_project: "搜索项目...",
            new_session_no_projects: "没有匹配的项目",
            new_session_choose_project: "选择其他目录",
            new_session_create: "创建会话",
            new_session_cancel: "返回当前会话",
            new_session_working_in: "临时会话",
            new_session_no_agents: "尚未添加 Agent，请进入配置中心添加。",
            new_session_add_agent: "添加 Agent",
            model_provider_label: "模型供应商",
            model_label: "模型",
            reasoning_depth: "思考深度",
            conversation_mode: "对话模式",
            agent_workbench: "Agent 工作台",
            runtime_unavailable: "运行时不可用",
            collapse_sidebar: "收起侧栏",
            expand_sidebar: "展开侧栏",
            resize_sidebar: "调整侧栏宽度",
            go_back: "后退",
            go_forward: "前进",
            pair_mobile: "配对移动设备",
            open_settings: "打开设置",
            settings: "设置",
            settings_description: "管理此 Vibex 工作台的桌面偏好。",
            restore_defaults: "恢复默认设置",
            restore_defaults_description: "将语言、主题、界面字体、代码字体、会话宽度和预览条恢复为 Vibex 默认值。",
            restore_defaults_confirm_title: "恢复默认设置？",
            restore_defaults_confirm_description: "这会重置语言、主题、界面字体、代码字体、会话内容宽度和会话预览条。",
            general: "常规",
            general_description: "配置桌面端的默认行为。",
            appearance: "外观",
            appearance_description: "选择桌面端如何使用浅色和深色模式。",
            theme: "主题",
            theme_description: "立即生效，并保存在此设备上。",
            light: "浅色",
            dark: "深色",
            system: "跟随系统",
            system_default: "系统默认",
            language: "语言",
            language_description: "选择此设备的界面语言偏好。",
            english: "English",
            simplified_chinese: "简体中文",
            traditional_chinese: "繁體中文",
            interface_font: "界面字体",
            interface_font_description: "从本机已安装字体中选择，编辑器和终端字体保持不变。",
            system_ui: "系统界面字体",
            choose_interface_font: "选择界面字体",
            interface_font_size: "界面字号",
            interface_font_size_description: "按像素设置界面基础字号。",
            interface_font_weight: "界面字重",
            interface_font_weight_description: "设置界面基础字重。",
            code_font: "代码字体",
            code_font_description: "应用到代码块、终端和集成编辑器。",
            system_monospace: "系统等宽字体",
            choose_code_font: "选择代码字体",
            code_font_size: "代码字号",
            code_font_size_description: "按像素设置代码渲染字号。",
            code_font_weight: "代码字重",
            code_font_weight_description: "设置代码渲染字重。",
            session_turn_preview_rail: "会话预览条",
            session_turn_preview_rail_description: "在 Agent 会话中显示左侧对话轮次预览条。",
            session_content_width: "会话内容宽度",
            session_content_width_description: "控制会话消息和下方输入框的整体宽度。",
            session_content_width_narrow: "窄屏",
            session_content_width_standard: "标准",
            session_content_width_full: "全屏",
            decrease_font_size: "减小字体",
            increase_font_size: "增大字体",
            decrease_font_weight: "减小字重",
            increase_font_weight: "增大字重",
            session_search_placeholder: "搜索会话和消息内容",
            session_search_recent: "最近会话",
            session_search_results: "搜索结果",
            session_search_no_results: "没有匹配的会话或消息",
            session_search_loading: "正在索引会话内容...",
            session_search_open: "搜索会话",
            session_search_close: "关闭搜索",
            sidebar_new_session: "新建会话",
            sidebar_providers: "配置中心",
            sidebar_projects: "项目",
            sidebar_no_matching_sessions: "没有匹配的会话",
            sidebar_new_project: "新建项目",
            sidebar_import_sessions: "导入会话",
            sidebar_no_sessions: "暂无会话",
            sidebar_delete_project: "删除项目",
            sidebar_collapse_all: "折叠全部会话",
            sidebar_restore: "恢复会话状态",
            sidebar_batch: "批量处理会话",
            sidebar_exit_batch: "退出批量处理",
            sidebar_select_all: "全选会话",
            sidebar_clear: "清空选择",
            sidebar_delete_selected: "删除所选",
            sidebar_pin: "置顶",
            sidebar_unpin: "取消置顶",
            sidebar_pinned: "已置顶",
            sidebar_rename: "重命名",
            sidebar_delete: "删除",
            sidebar_rename_placeholder: "会话标题",
            sidebar_rename_empty: "请输入会话标题。",
            sidebar_rename_failed: "重命名失败。",
            sidebar_cancel: "取消",
            sidebar_confirm_delete_session: "删除会话？",
            sidebar_confirm_delete_project: "删除项目？",
            sidebar_state_pending: "待处理",
            sidebar_state_initializing: "初始化",
            sidebar_state_error: "错误",
            sidebar_state_archived: "已归档",
            sidebar_yesterday: "昨天",
        },
        ResolvedLocale::ZhTw => Strings {
            sessions: "工作階段",
            workbench: "工作台",
            preview: "預覽",
            files_git: "檔案與 Git",
            loading_runtime: "正在啟動本機執行環境",
            runtime_ready: "本機執行環境已就緒",
            runtime_using: "正在使用",
            runtime_waiting: "正在等待切換到",
            runtime_preparing: "正在準備",
            runtime_still_using: "仍在使用",
            runtime_use_current: "使用目前執行環境",
            retry: "重試",
            no_workspace: "尚未選取工作區",
            open_workspace: "開啟工作區",
            no_preview: "暫無預覽",
            no_workspace_files: "暫無工作區檔案",
            message_agent: "讓 Vibex 檢查、編輯、測試或解釋此工作區...",
            agent_loading_session: "正在載入會話...",
            agent_start_conversation: "開始對話",
            agent_select_session: "建立或選擇會話",
            agent_timeline_description: "權威會話歷史和即時執行事件會顯示在這裡。",
            agent_process_events: "條過程事件",
            agent_worked_for: "工作了",
            agent_pending_response: "思考中...",
            agent_expand_process: "展開過程",
            agent_collapse_process: "收起過程",
            new_session_title_before_brand: "和",
            new_session_title_after_brand: "一起工作",
            new_session_description: "從專案目錄或臨時工作區建立一個新的會話。",
            new_session_prompt_placeholder: "我能為你做哪些事情？",
            new_session_project_label: "專案目錄",
            new_session_project_placeholder: "可選：輸入或貼上專案目錄路徑",
            new_session_search_project: "搜尋專案...",
            new_session_no_projects: "沒有符合的專案",
            new_session_choose_project: "選擇其他目錄",
            new_session_create: "建立會話",
            new_session_cancel: "返回目前會話",
            new_session_working_in: "臨時會話",
            new_session_no_agents: "尚未新增 Agent，請進入配置中心新增。",
            new_session_add_agent: "新增 Agent",
            model_provider_label: "模型供應商",
            model_label: "模型",
            reasoning_depth: "思考深度",
            conversation_mode: "對話模式",
            agent_workbench: "Agent 工作台",
            runtime_unavailable: "執行環境無法使用",
            collapse_sidebar: "收合側邊欄",
            expand_sidebar: "展開側邊欄",
            resize_sidebar: "調整側邊欄寬度",
            go_back: "返回",
            go_forward: "前進",
            pair_mobile: "配對行動裝置",
            open_settings: "開啟設定",
            settings: "設定",
            settings_description: "管理此 Vibex 工作台的桌面偏好。",
            restore_defaults: "恢復預設設定",
            restore_defaults_description: "將語言、主題、介面字體、程式碼字體、會話寬度和預覽條恢復為 Vibex 預設值。",
            restore_defaults_confirm_title: "恢復預設設定？",
            restore_defaults_confirm_description: "這會重設語言、主題、介面字體、程式碼字體、會話內容寬度和會話預覽條。",
            general: "一般",
            general_description: "設定桌面端的預設行為。",
            appearance: "外觀",
            appearance_description: "選擇桌面端如何使用淺色和深色模式。",
            theme: "主題",
            theme_description: "立即生效，並儲存在此裝置上。",
            light: "淺色",
            dark: "深色",
            system: "跟隨系統",
            system_default: "系統預設",
            language: "語言",
            language_description: "選擇此裝置的介面語言偏好。",
            english: "English",
            simplified_chinese: "简体中文",
            traditional_chinese: "繁體中文",
            interface_font: "介面字體",
            interface_font_description: "從本機已安裝字體中選擇，編輯器和終端字體保持不變。",
            system_ui: "系統介面字體",
            choose_interface_font: "選擇介面字體",
            interface_font_size: "介面字號",
            interface_font_size_description: "按像素設定介面基礎字號。",
            interface_font_weight: "介面字重",
            interface_font_weight_description: "設定介面基礎字重。",
            code_font: "程式碼字體",
            code_font_description: "套用到程式碼區塊、終端機和整合編輯器。",
            system_monospace: "系統等寬字體",
            choose_code_font: "選擇程式碼字體",
            code_font_size: "程式碼字號",
            code_font_size_description: "按像素設定程式碼渲染字號。",
            code_font_weight: "程式碼字重",
            code_font_weight_description: "設定程式碼渲染字重。",
            session_turn_preview_rail: "會話預覽條",
            session_turn_preview_rail_description: "在 Agent 會話中顯示左側對話輪次預覽條。",
            session_content_width: "會話內容寬度",
            session_content_width_description: "控制會話訊息和下方輸入框的整體寬度。",
            session_content_width_narrow: "窄屏",
            session_content_width_standard: "標準",
            session_content_width_full: "全屏",
            decrease_font_size: "縮小字體",
            increase_font_size: "放大字體",
            decrease_font_weight: "減小字重",
            increase_font_weight: "增大字重",
            session_search_placeholder: "搜尋會話和訊息內容",
            session_search_recent: "最近會話",
            session_search_results: "搜尋結果",
            session_search_no_results: "沒有符合的會話或訊息",
            session_search_loading: "正在建立會話內容索引...",
            session_search_open: "搜尋會話",
            session_search_close: "關閉搜尋",
            sidebar_new_session: "建立會話",
            sidebar_providers: "配置中心",
            sidebar_projects: "專案",
            sidebar_no_matching_sessions: "沒有符合的會話",
            sidebar_new_project: "新建專案",
            sidebar_import_sessions: "匯入會話",
            sidebar_no_sessions: "暫無會話",
            sidebar_delete_project: "刪除專案",
            sidebar_collapse_all: "折疊全部會話",
            sidebar_restore: "恢復會話狀態",
            sidebar_batch: "批量處理會話",
            sidebar_exit_batch: "退出批量處理",
            sidebar_select_all: "全選會話",
            sidebar_clear: "清空選擇",
            sidebar_delete_selected: "刪除所選",
            sidebar_pin: "置頂",
            sidebar_unpin: "取消置頂",
            sidebar_pinned: "已置頂",
            sidebar_rename: "重新命名",
            sidebar_delete: "刪除",
            sidebar_rename_placeholder: "會話標題",
            sidebar_rename_empty: "請輸入會話標題。",
            sidebar_rename_failed: "重新命名失敗。",
            sidebar_cancel: "取消",
            sidebar_confirm_delete_session: "刪除會話？",
            sidebar_confirm_delete_project: "刪除專案？",
            sidebar_state_pending: "待處理",
            sidebar_state_initializing: "初始化",
            sidebar_state_error: "錯誤",
            sidebar_state_archived: "已封存",
            sidebar_yesterday: "昨天",
        },
    }
}

pub fn apply_locale(mode: LocaleMode) -> ResolvedLocale {
    let locale = resolve_locale(mode, system_locale().as_deref());
    gpui_component::set_locale(locale.tag());
    locale
}

pub fn current_locale() -> ResolvedLocale {
    resolve_locale(LocaleMode::System, Some(&gpui_component::locale()))
}

pub fn current_strings() -> Strings {
    strings(current_locale())
}

pub const fn text_for(
    locale: ResolvedLocale,
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
) -> &'static str {
    match locale {
        ResolvedLocale::En => en,
        ResolvedLocale::ZhCn => zh_cn,
        ResolvedLocale::ZhTw => zh_tw,
    }
}

pub fn text(en: &'static str, zh_cn: &'static str, zh_tw: &'static str) -> &'static str {
    text_for(current_locale(), en, zh_cn, zh_tw)
}

#[derive(Clone, Copy)]
struct MessageTranslation {
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
}

const ERROR_MESSAGES: &[MessageTranslation] = &[
    MessageTranslation {
        en: "Runtime selection is not ready",
        zh_cn: "运行时选择尚未就绪",
        zh_tw: "執行環境選擇尚未就緒",
    },
    MessageTranslation {
        en: "Attachment save location is unavailable",
        zh_cn: "附件保存位置不可用",
        zh_tw: "附件儲存位置無法使用",
    },
    MessageTranslation {
        en: "Select a session before creating a terminal",
        zh_cn: "请先选择会话，再创建终端",
        zh_tw: "請先選擇會話，再建立終端機",
    },
    MessageTranslation {
        en: "No available Agent runtime is configured",
        zh_cn: "尚未配置可用的 Agent 运行时",
        zh_tw: "尚未設定可用的 Agent 執行環境",
    },
    MessageTranslation {
        en: "No enabled Agent is available",
        zh_cn: "没有可用的已启用 Agent",
        zh_tw: "沒有可用的已啟用 Agent",
    },
    MessageTranslation {
        en: "Enter a session title.",
        zh_cn: "请输入会话标题。",
        zh_tw: "請輸入會話標題。",
    },
    MessageTranslation {
        en: "The editor is not ready to save or has an external conflict",
        zh_cn: "编辑器尚未准备好保存，或文件存在外部冲突",
        zh_tw: "編輯器尚未準備好儲存，或檔案存在外部衝突",
    },
    MessageTranslation {
        en: "Select one or more changes first",
        zh_cn: "请先选择一项或多项更改",
        zh_tw: "請先選擇一項或多項變更",
    },
    MessageTranslation {
        en: "Another Git mutation is already running",
        zh_cn: "另一项 Git 操作正在运行",
        zh_tw: "另一項 Git 操作正在執行",
    },
    MessageTranslation {
        en: "A workspace-relative path is required",
        zh_cn: "请输入工作区相对路径",
        zh_tw: "請輸入工作區相對路徑",
    },
    MessageTranslation {
        en: "Workspace switch is waiting because one or more editor buffers are dirty",
        zh_cn: "一个或多个编辑器缓冲区存在未保存更改，工作区切换正在等待",
        zh_tw: "一個或多個編輯器緩衝區存在未儲存變更，工作區切換正在等待",
    },
    MessageTranslation {
        en: "Terminal session is no longer available",
        zh_cn: "终端会话已不可用",
        zh_tw: "終端機會話已無法使用",
    },
    MessageTranslation {
        en: "Unpin the tab before closing it",
        zh_cn: "请先取消固定标签页，再将其关闭",
        zh_tw: "請先取消固定分頁，再將其關閉",
    },
    MessageTranslation {
        en: "Save or discard the dirty editor before closing it",
        zh_cn: "请先保存或放弃编辑器中的更改，再将其关闭",
        zh_tw: "請先儲存或捨棄編輯器中的變更，再將其關閉",
    },
    MessageTranslation {
        en: "Pinned or dirty tabs were kept open",
        zh_cn: "已固定或存在未保存更改的标签页仍保持打开",
        zh_tw: "已固定或存在未儲存變更的分頁仍保持開啟",
    },
    MessageTranslation {
        en: "Another file mutation is already running",
        zh_cn: "另一项文件操作正在运行",
        zh_tw: "另一項檔案操作正在執行",
    },
    MessageTranslation {
        en: "Unsupported binary file",
        zh_cn: "不支持预览此二进制文件",
        zh_tw: "不支援預覽此二進位檔案",
    },
    MessageTranslation {
        en: "Unsupported image format",
        zh_cn: "不支持此图片格式",
        zh_tw: "不支援此圖片格式",
    },
    MessageTranslation {
        en: "The image could not be decoded",
        zh_cn: "无法解码此图片",
        zh_tw: "無法解碼此圖片",
    },
    MessageTranslation {
        en: "Image exceeds the native preview budget",
        zh_cn: "图片超过原生预览大小限制",
        zh_tw: "圖片超過原生預覽大小限制",
    },
    MessageTranslation {
        en: "Corrupt UI state was detected; recovery is waiting for the runtime lock",
        zh_cn: "检测到界面状态损坏；正在等待运行时锁以执行恢复",
        zh_tw: "偵測到介面狀態損毀；正在等待執行環境鎖定以執行復原",
    },
    MessageTranslation {
        en: "Corrupt UI state was quarantined and defaults were restored",
        zh_cn: "已隔离损坏的界面状态并恢复默认设置",
        zh_tw: "已隔離損毀的介面狀態並復原預設設定",
    },
    MessageTranslation {
        en: "Management runtime is not connected",
        zh_cn: "管理运行时未连接",
        zh_tw: "管理執行環境未連線",
    },
    MessageTranslation {
        en: "Model id is required",
        zh_cn: "模型 ID 为必填项",
        zh_tw: "模型 ID 為必填欄位",
    },
    MessageTranslation {
        en: "Select an Agent before creating a provider profile",
        zh_cn: "请先选择 Agent，再创建供应商配置",
        zh_tw: "請先選擇 Agent，再建立供應商設定",
    },
    MessageTranslation {
        en: "Provider profile name is required",
        zh_cn: "供应商配置名称为必填项",
        zh_tw: "供應商設定名稱為必填欄位",
    },
    MessageTranslation {
        en: "Provider profile identity is invalid",
        zh_cn: "供应商配置标识无效",
        zh_tw: "供應商設定識別碼無效",
    },
    MessageTranslation {
        en: "Invalid Agent id",
        zh_cn: "Agent 标识无效",
        zh_tw: "Agent 識別碼無效",
    },
    MessageTranslation {
        en: "Only enabled provider profiles can join failover",
        zh_cn: "只有已启用的供应商配置才能加入故障转移",
        zh_tw: "只有已啟用的供應商設定才能加入故障轉移",
    },
    MessageTranslation {
        en: "MCP server was not found",
        zh_cn: "未找到 MCP 服务器",
        zh_tw: "找不到 MCP 伺服器",
    },
    MessageTranslation {
        en: "Skill was not found",
        zh_cn: "未找到技能",
        zh_tw: "找不到技能",
    },
    MessageTranslation {
        en: "Select exactly two nodes to create an edge",
        zh_cn: "请选择两个节点来创建连线",
        zh_tw: "請選擇兩個節點來建立連線",
    },
    MessageTranslation {
        en: "Graph changed elsewhere. Draft preserved; reload before retrying.",
        zh_cn: "自动化图已在其他位置发生更改。草稿已保留，请重新加载后重试。",
        zh_tw: "自動化圖已在其他位置發生變更。草稿已保留，請重新載入後重試。",
    },
    MessageTranslation {
        en: "A graph title and at least one node are required",
        zh_cn: "自动化图标题和至少一个节点为必填项",
        zh_tw: "自動化圖標題和至少一個節點為必填欄位",
    },
    MessageTranslation {
        en: "Open a workspace session before creating an automation graph",
        zh_cn: "请先打开工作区会话，再创建自动化图",
        zh_tw: "請先開啟工作區會話，再建立自動化圖",
    },
    MessageTranslation {
        en: "ACP command is required",
        zh_cn: "ACP 命令为必填项",
        zh_tw: "ACP 命令為必填欄位",
    },
    MessageTranslation {
        en: "Scheduled task title and prompt are required",
        zh_cn: "定时任务标题和提示词为必填项",
        zh_tw: "排程任務標題和提示詞為必填欄位",
    },
    MessageTranslation {
        en: "Open a workspace session before creating a scheduled task",
        zh_cn: "请先打开工作区会话，再创建定时任务",
        zh_tw: "請先開啟工作區會話，再建立排程任務",
    },
    MessageTranslation {
        en: "A node cannot connect to itself",
        zh_cn: "节点不能连接到自身",
        zh_tw: "節點不能連線到自身",
    },
    MessageTranslation {
        en: "The edge source node is missing",
        zh_cn: "连线的源节点不存在",
        zh_tw: "連線的來源節點不存在",
    },
    MessageTranslation {
        en: "The edge target node is missing",
        zh_cn: "连线的目标节点不存在",
        zh_tw: "連線的目標節點不存在",
    },
    MessageTranslation {
        en: "The edge already exists",
        zh_cn: "该连线已存在",
        zh_tw: "該連線已存在",
    },
    MessageTranslation {
        en: "Automation graph title is required",
        zh_cn: "自动化图标题为必填项",
        zh_tw: "自動化圖標題為必填欄位",
    },
    MessageTranslation {
        en: "Add at least one automation node",
        zh_cn: "请至少添加一个自动化节点",
        zh_tw: "請至少新增一個自動化節點",
    },
    MessageTranslation {
        en: "Node ids must be unique",
        zh_cn: "节点标识必须唯一",
        zh_tw: "節點識別碼必須唯一",
    },
    MessageTranslation {
        en: "Node title is required",
        zh_cn: "节点标题为必填项",
        zh_tw: "節點標題為必填欄位",
    },
    MessageTranslation {
        en: "Save the graph before replacing its definition",
        zh_cn: "请先保存自动化图，再替换其定义",
        zh_tw: "請先儲存自動化圖，再取代其定義",
    },
    MessageTranslation {
        en: "Fixture workspace",
        zh_cn: "测试工作区",
        zh_tw: "測試工作區",
    },
    MessageTranslation {
        en: "Workspace files and Git state are loading",
        zh_cn: "正在加载工作区文件和 Git 状态",
        zh_tw: "正在載入工作區檔案和 Git 狀態",
    },
    MessageTranslation {
        en: "Opened Web Preview in the system browser",
        zh_cn: "已在系统浏览器中打开网页预览",
        zh_tw: "已在系統瀏覽器中開啟網頁預覽",
    },
    MessageTranslation {
        en: "File operation completed",
        zh_cn: "文件操作已完成",
        zh_tw: "檔案操作已完成",
    },
    MessageTranslation {
        en: "Git operation completed",
        zh_cn: "Git 操作已完成",
        zh_tw: "Git 操作已完成",
    },
    MessageTranslation {
        en: "Current Agent work did not finish before the runtime switch deadline.",
        zh_cn: "当前 Agent 工作未能在运行时切换期限前完成。",
        zh_tw: "目前 Agent 工作未能在執行環境切換期限前完成。",
    },
    MessageTranslation {
        en: "Retry the selection after the current work finishes.",
        zh_cn: "请在当前工作完成后重试此选择。",
        zh_tw: "請在目前工作完成後重試此選擇。",
    },
    MessageTranslation {
        en: "The selected Agent runtime requires authentication.",
        zh_cn: "所选 Agent 运行时需要身份验证。",
        zh_tw: "所選 Agent 執行環境需要身分驗證。",
    },
    MessageTranslation {
        en: "Configure the selected Agent profile, then retry the selection.",
        zh_cn: "请配置所选 Agent 配置，然后重试此选择。",
        zh_tw: "請設定所選 Agent 設定，然後重試此選擇。",
    },
    MessageTranslation {
        en: "The selected Agent runtime configuration is unavailable.",
        zh_cn: "所选 Agent 运行时配置不可用。",
        zh_tw: "所選 Agent 執行環境設定無法使用。",
    },
    MessageTranslation {
        en: "Review the selected Agent profile and model, then retry.",
        zh_cn: "请检查所选 Agent 配置和模型，然后重试。",
        zh_tw: "請檢查所選 Agent 設定和模型，然後重試。",
    },
    MessageTranslation {
        en: "The selected Agent runtime could not be activated; the previous runtime remains available.",
        zh_cn: "无法激活所选 Agent 运行时；之前的运行时仍可使用。",
        zh_tw: "無法啟用所選 Agent 執行環境；先前的執行環境仍可使用。",
    },
    MessageTranslation {
        en: "Review the selected runtime configuration and retry.",
        zh_cn: "请检查所选运行时配置并重试。",
        zh_tw: "請檢查所選執行環境設定並重試。",
    },
];

const ERROR_PREFIXES: &[MessageTranslation] = &[
    MessageTranslation {
        en: "UI state load failed: ",
        zh_cn: "界面状态加载失败：",
        zh_tw: "介面狀態載入失敗：",
    },
    MessageTranslation {
        en: "Preview configuration failed: ",
        zh_cn: "预览配置失败：",
        zh_tw: "預覽設定失敗：",
    },
    MessageTranslation {
        en: "agent overview task failed: ",
        zh_cn: "Agent 概览任务失败：",
        zh_tw: "Agent 概覽任務失敗：",
    },
    MessageTranslation {
        en: "right-rail plugin load failed: ",
        zh_cn: "右侧栏插件加载失败：",
        zh_tw: "右側欄外掛載入失敗：",
    },
    MessageTranslation {
        en: "right-rail reorder failed: ",
        zh_cn: "右侧栏排序失败：",
        zh_tw: "右側欄排序失敗：",
    },
    MessageTranslation {
        en: "session load task failed: ",
        zh_cn: "会话加载任务失败：",
        zh_tw: "會話載入任務失敗：",
    },
    MessageTranslation {
        en: "message submission failed: ",
        zh_cn: "消息发送失败：",
        zh_tw: "訊息傳送失敗：",
    },
    MessageTranslation {
        en: "clipboard image capture failed: ",
        zh_cn: "读取剪贴板图片失败：",
        zh_tw: "讀取剪貼簿圖片失敗：",
    },
    MessageTranslation {
        en: "image save failed: ",
        zh_cn: "图片保存失败：",
        zh_tw: "圖片儲存失敗：",
    },
    MessageTranslation {
        en: "terminal create task failed: ",
        zh_cn: "终端创建任务失败：",
        zh_tw: "終端機建立任務失敗：",
    },
    MessageTranslation {
        en: "terminal create failed: ",
        zh_cn: "终端创建失败：",
        zh_tw: "終端機建立失敗：",
    },
    MessageTranslation {
        en: "terminal close task failed: ",
        zh_cn: "终端关闭任务失败：",
        zh_tw: "終端機關閉任務失敗：",
    },
    MessageTranslation {
        en: "Terminal close failed: ",
        zh_cn: "终端关闭失败：",
        zh_tw: "終端機關閉失敗：",
    },
    MessageTranslation {
        en: "terminal close failed: ",
        zh_cn: "终端关闭失败：",
        zh_tw: "終端機關閉失敗：",
    },
    MessageTranslation {
        en: "terminal shell switch task failed: ",
        zh_cn: "终端 Shell 切换任务失败：",
        zh_tw: "終端機 Shell 切換任務失敗：",
    },
    MessageTranslation {
        en: "terminal shell switch failed: ",
        zh_cn: "终端 Shell 切换失败：",
        zh_tw: "終端機 Shell 切換失敗：",
    },
    MessageTranslation {
        en: "external import failed: ",
        zh_cn: "外部导入失败：",
        zh_tw: "外部匯入失敗：",
    },
    MessageTranslation {
        en: "Agent action failed: ",
        zh_cn: "Agent 操作失败：",
        zh_tw: "Agent 操作失敗：",
    },
    MessageTranslation {
        en: "session creation failed: ",
        zh_cn: "会话创建失败：",
        zh_tw: "會話建立失敗：",
    },
    MessageTranslation {
        en: "runtime switch failed: ",
        zh_cn: "运行时切换失败：",
        zh_tw: "執行環境切換失敗：",
    },
    MessageTranslation {
        en: "runtime cancel failed: ",
        zh_cn: "取消运行时切换失败：",
        zh_tw: "取消執行環境切換失敗：",
    },
    MessageTranslation {
        en: "session rename failed: ",
        zh_cn: "会话重命名失败：",
        zh_tw: "會話重新命名失敗：",
    },
    MessageTranslation {
        en: "session mutation failed: ",
        zh_cn: "会话操作失败：",
        zh_tw: "會話操作失敗：",
    },
    MessageTranslation {
        en: "UI state flush failed: ",
        zh_cn: "界面状态保存失败：",
        zh_tw: "介面狀態儲存失敗：",
    },
    MessageTranslation {
        en: "file tree task failed: ",
        zh_cn: "文件树加载任务失败：",
        zh_tw: "檔案樹載入任務失敗：",
    },
    MessageTranslation {
        en: "Git status task failed: ",
        zh_cn: "Git 状态任务失败：",
        zh_tw: "Git 狀態任務失敗：",
    },
    MessageTranslation {
        en: "Git history task failed: ",
        zh_cn: "Git 历史任务失败：",
        zh_tw: "Git 歷史任務失敗：",
    },
    MessageTranslation {
        en: "file save task failed: ",
        zh_cn: "文件保存任务失败：",
        zh_tw: "檔案儲存任務失敗：",
    },
    MessageTranslation {
        en: "file mutation task failed: ",
        zh_cn: "文件操作任务失败：",
        zh_tw: "檔案操作任務失敗：",
    },
    MessageTranslation {
        en: "Git diff task failed: ",
        zh_cn: "Git 差异任务失败：",
        zh_tw: "Git 差異任務失敗：",
    },
    MessageTranslation {
        en: "commit detail task failed: ",
        zh_cn: "提交详情任务失败：",
        zh_tw: "提交詳細資料任務失敗：",
    },
    MessageTranslation {
        en: "blame task failed: ",
        zh_cn: "Git 追溯任务失败：",
        zh_tw: "Git 追溯任務失敗：",
    },
    MessageTranslation {
        en: "Git mutation task failed: ",
        zh_cn: "Git 操作任务失败：",
        zh_tw: "Git 操作任務失敗：",
    },
    MessageTranslation {
        en: "MCP validation task failed: ",
        zh_cn: "MCP 验证任务失败：",
        zh_tw: "MCP 驗證任務失敗：",
    },
    MessageTranslation {
        en: "Skill validation task failed: ",
        zh_cn: "技能验证任务失败：",
        zh_tw: "技能驗證任務失敗：",
    },
    MessageTranslation {
        en: "management action failed: ",
        zh_cn: "管理操作失败：",
        zh_tw: "管理操作失敗：",
    },
    MessageTranslation {
        en: "Config center refresh failed: ",
        zh_cn: "配置中心刷新失败：",
        zh_tw: "配置中心重新整理失敗：",
    },
    MessageTranslation {
        en: "API Key loading failed: ",
        zh_cn: "API Key 加载失败：",
        zh_tw: "API Key 載入失敗：",
    },
    MessageTranslation {
        en: "Provider configuration save failed: ",
        zh_cn: "供应商配置保存失败：",
        zh_tw: "供應商設定儲存失敗：",
    },
    MessageTranslation {
        en: "Model detection failed: ",
        zh_cn: "模型探测失败：",
        zh_tw: "模型探測失敗：",
    },
    MessageTranslation {
        en: "Config center action failed: ",
        zh_cn: "配置中心操作失败：",
        zh_tw: "配置中心操作失敗：",
    },
    MessageTranslation {
        en: "MCP discovery failed: ",
        zh_cn: "MCP 探测失败：",
        zh_tw: "MCP 探測失敗：",
    },
    MessageTranslation {
        en: "MCP import failed: ",
        zh_cn: "MCP 导入失败：",
        zh_tw: "MCP 匯入失敗：",
    },
    MessageTranslation {
        en: "Skill discovery failed: ",
        zh_cn: "技能探测失败：",
        zh_tw: "技能探測失敗：",
    },
    MessageTranslation {
        en: "Skill import failed: ",
        zh_cn: "技能导入失败：",
        zh_tw: "技能匯入失敗：",
    },
    MessageTranslation {
        en: "Graph save failed: ",
        zh_cn: "自动化图保存失败：",
        zh_tw: "自動化圖儲存失敗：",
    },
    MessageTranslation {
        en: "Graph creation failed: ",
        zh_cn: "自动化图创建失败：",
        zh_tw: "自動化圖建立失敗：",
    },
    MessageTranslation {
        en: "Native export preview failed: ",
        zh_cn: "原生配置导出预览失败：",
        zh_tw: "原生設定匯出預覽失敗：",
    },
    MessageTranslation {
        en: "Scheduled task creation failed: ",
        zh_cn: "定时任务创建失败：",
        zh_tw: "排程任務建立失敗：",
    },
    MessageTranslation {
        en: "Saved image copy to ",
        zh_cn: "图片副本已保存到 ",
        zh_tw: "圖片副本已儲存到 ",
    },
    MessageTranslation {
        en: "Saved ",
        zh_cn: "已保存 ",
        zh_tw: "已儲存 ",
    },
    MessageTranslation {
        en: "Opened ",
        zh_cn: "已打开 ",
        zh_tw: "已開啟 ",
    },
];

fn translated_message<'a>(
    locale: ResolvedLocale,
    message: &'a str,
) -> Option<std::borrow::Cow<'a, str>> {
    for translation in ERROR_MESSAGES {
        if [translation.en, translation.zh_cn, translation.zh_tw].contains(&message) {
            return Some(std::borrow::Cow::Borrowed(text_for(
                locale,
                translation.en,
                translation.zh_cn,
                translation.zh_tw,
            )));
        }
    }
    for translation in ERROR_PREFIXES {
        for prefix in [translation.en, translation.zh_cn, translation.zh_tw] {
            if let Some(detail) = message.strip_prefix(prefix) {
                let localized_detail = if detail.contains("; ")
                    || detail
                        .split_once(": ")
                        .is_some_and(|(code, _)| is_stable_error_code(code))
                {
                    std::borrow::Cow::Owned(localize_error_message_for(locale, detail))
                } else {
                    std::borrow::Cow::Borrowed(detail)
                };
                return Some(std::borrow::Cow::Owned(format!(
                    "{}{}",
                    text_for(locale, translation.en, translation.zh_cn, translation.zh_tw,),
                    localized_detail
                )));
            }
        }
    }
    None
}

fn is_stable_error_code(value: &str) -> bool {
    value.contains('_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn contains_han(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'))
}

fn error_code_summary(locale: ResolvedLocale, code: &str) -> &'static str {
    let (en, zh_cn, zh_tw) = if code.contains("not_found") {
        (
            "The requested item was not found",
            "未找到请求的项目",
            "找不到要求的項目",
        )
    } else if code.contains("invalid") || code.contains("malformed") {
        ("The request is invalid", "请求无效", "要求無效")
    } else if code.contains("unavailable") || code.contains("not_ready") {
        (
            "The requested function is unavailable",
            "请求的功能不可用",
            "要求的功能無法使用",
        )
    } else if code.contains("required") || code.contains("missing") {
        (
            "Required information is missing",
            "缺少必填信息",
            "缺少必填資訊",
        )
    } else if code.contains("conflict") || code.contains("revision_changed") {
        (
            "The current state conflicts with this operation",
            "当前状态与此操作冲突",
            "目前狀態與此操作衝突",
        )
    } else if code.contains("denied") || code.contains("permission") || code.contains("forbidden") {
        ("Permission was denied", "权限不足", "權限不足")
    } else if code.contains("timeout") || code.contains("timed_out") {
        ("The operation timed out", "操作超时", "操作逾時")
    } else if code.contains("already_exists") {
        (
            "The requested item already exists",
            "请求的项目已存在",
            "要求的項目已存在",
        )
    } else {
        ("The operation failed", "操作失败", "操作失敗")
    };
    text_for(locale, en, zh_cn, zh_tw)
}

pub fn localize_error_message_for(locale: ResolvedLocale, message: &str) -> String {
    if let Some(translated) = translated_message(locale, message) {
        return translated.into_owned();
    }

    if let Some((code, detail)) = message.split_once(": ")
        && is_stable_error_code(code)
    {
        let translated_detail = translated_message(locale, detail).map(|value| value.into_owned());
        let detail = translated_detail.unwrap_or_else(|| match locale {
            ResolvedLocale::En if !contains_han(detail) => detail.to_string(),
            ResolvedLocale::ZhCn | ResolvedLocale::ZhTw if contains_han(detail) => {
                detail.to_string()
            }
            _ => error_code_summary(locale, code).to_string(),
        });
        return format!("{code}: {detail}");
    }

    if message.contains("; ") {
        return message
            .split("; ")
            .map(|part| localize_error_message_for(locale, part))
            .collect::<Vec<_>>()
            .join(text_for(locale, "; ", "；", "；"));
    }

    match locale {
        ResolvedLocale::En if !contains_han(message) => message.to_string(),
        ResolvedLocale::En => "Operation failed".to_string(),
        ResolvedLocale::ZhCn if contains_han(message) => message.to_string(),
        ResolvedLocale::ZhTw if contains_han(message) => message.to_string(),
        ResolvedLocale::ZhCn => format!("操作失败（诊断信息：{message}）"),
        ResolvedLocale::ZhTw => format!("操作失敗（診斷資訊：{message}）"),
    }
}

pub fn localize_error_message(message: &str) -> String {
    localize_error_message_for(current_locale(), message)
}

pub fn localize_ui_message_for(locale: ResolvedLocale, message: &str) -> String {
    translated_message(locale, message)
        .map(|message| message.into_owned())
        .unwrap_or_else(|| message.to_string())
}

pub fn localize_ui_message(message: &str) -> String {
    localize_ui_message_for(current_locale(), message)
}

pub fn system_locale() -> Option<String> {
    static SYSTEM_LOCALE: OnceLock<Option<String>> = OnceLock::new();
    SYSTEM_LOCALE
        .get_or_init(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|key| {
                    std::env::var(key)
                        .ok()
                        .filter(|value| !value.trim().is_empty())
                })
                .or_else(|| {
                    let locale = gpui_component::locale().to_string();
                    (!locale.trim().is_empty()).then_some(locale)
                })
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_modes_resolve_without_hidden_fallbacks() {
        assert_eq!(
            resolve_locale(LocaleMode::En, Some("zh_CN")),
            ResolvedLocale::En
        );
        assert_eq!(
            resolve_locale(LocaleMode::ZhCn, Some("en_US")),
            ResolvedLocale::ZhCn
        );
        assert_eq!(
            resolve_locale(LocaleMode::ZhTw, Some("en_US")),
            ResolvedLocale::ZhTw
        );
        assert_eq!(
            resolve_locale(LocaleMode::System, Some("zh_TW.UTF-8")),
            ResolvedLocale::ZhTw
        );
        assert_eq!(
            resolve_locale(LocaleMode::System, Some("zh_CN.UTF-8")),
            ResolvedLocale::ZhCn
        );
        assert_eq!(
            resolve_locale(LocaleMode::System, Some("zh-HK")),
            ResolvedLocale::ZhTw
        );
        assert_eq!(
            resolve_locale(LocaleMode::System, Some("en_US.UTF-8")),
            ResolvedLocale::En
        );
    }

    #[test]
    fn locale_tags_and_settings_strings_cover_all_supported_languages() {
        assert_eq!(ResolvedLocale::En.tag(), "en");
        assert_eq!(ResolvedLocale::ZhCn.tag(), "zh-CN");
        assert_eq!(ResolvedLocale::ZhTw.tag(), "zh-TW");

        assert_eq!(strings(ResolvedLocale::En).settings, "Settings");
        assert_eq!(strings(ResolvedLocale::ZhCn).settings, "设置");
        assert_eq!(strings(ResolvedLocale::ZhTw).settings, "設定");
        assert_eq!(strings(ResolvedLocale::En).general, "General");
        assert_eq!(strings(ResolvedLocale::ZhCn).theme, "主题");
        assert_eq!(
            strings(ResolvedLocale::ZhTw).restore_defaults,
            "恢復預設設定"
        );
        assert_eq!(
            strings(ResolvedLocale::ZhCn).message_agent,
            "让 Vibex 检查、编辑、测试或解释此工作区..."
        );
        assert_eq!(strings(ResolvedLocale::ZhTw).appearance, "外觀");
        assert_eq!(
            strings(ResolvedLocale::ZhCn).session_turn_preview_rail,
            "会话预览条"
        );
        assert_eq!(
            strings(ResolvedLocale::ZhTw).session_content_width,
            "會話內容寬度"
        );
        assert_eq!(
            strings(ResolvedLocale::En).session_content_width_full,
            "Full width"
        );
        assert_eq!(
            strings(ResolvedLocale::En).new_session_prompt_placeholder,
            "What can I help you with?"
        );
        assert_eq!(
            strings(ResolvedLocale::ZhCn).new_session_prompt_placeholder,
            "我能为你做哪些事情？"
        );
        assert_eq!(
            strings(ResolvedLocale::ZhTw).new_session_prompt_placeholder,
            "我能為你做哪些事情？"
        );
    }

    #[test]
    fn stored_errors_are_rendered_in_the_requested_locale() {
        assert_eq!(
            localize_error_message_for(
                ResolvedLocale::ZhCn,
                "file_not_found: workspace file does not exist",
            ),
            "file_not_found: 未找到请求的项目"
        );
        assert_eq!(
            localize_error_message_for(
                ResolvedLocale::ZhTw,
                "file save task failed: background worker stopped",
            ),
            "檔案儲存任務失敗：background worker stopped"
        );
        assert_eq!(
            localize_error_message_for(
                ResolvedLocale::ZhCn,
                "terminal create failed: terminal_not_found: terminal is gone",
            ),
            "终端创建失败：terminal_not_found: 未找到请求的项目"
        );
        assert_eq!(
            localize_error_message_for(ResolvedLocale::En, "请输入会话标题。"),
            "Enter a session title."
        );
    }

    #[test]
    fn compound_validation_errors_translate_each_message() {
        assert_eq!(
            localize_error_message_for(
                ResolvedLocale::ZhCn,
                "Automation graph title is required; Add at least one automation node",
            ),
            "自动化图标题为必填项；请至少添加一个自动化节点"
        );
    }

    #[test]
    fn status_messages_can_be_relocalized_after_they_are_stored() {
        assert_eq!(
            localize_ui_message_for(ResolvedLocale::ZhTw, "File operation completed"),
            "檔案操作已完成"
        );
        assert_eq!(
            localize_ui_message_for(ResolvedLocale::En, "已保存 src/main.rs"),
            "Saved src/main.rs"
        );
    }
}
