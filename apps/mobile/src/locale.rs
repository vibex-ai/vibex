use std::sync::OnceLock;

use vibex_ui::locale::Locale;

static SYSTEM_LOCALE: OnceLock<Locale> = OnceLock::new();

/// The mobile client follows the device language for every fresh launch.  A
/// user preference is intentionally not persisted here: the native app has no
/// independent language setting and should track the platform default.
pub fn current() -> Locale {
    *SYSTEM_LOCALE.get_or_init(|| Locale::from_system_tag(sys_locale::get_locale().as_deref()))
}

pub const fn text_for(
    locale: Locale,
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
) -> &'static str {
    match locale {
        Locale::En => en,
        Locale::ZhCn => zh_cn,
        Locale::ZhTw => zh_tw,
    }
}

pub fn text(en: &'static str, zh_cn: &'static str, zh_tw: &'static str) -> &'static str {
    text_for(current(), en, zh_cn, zh_tw)
}

/// Translate a repeated mobile label without making every rendering helper
/// carry a locale parameter. Unknown provider/workspace copy remains intact.
pub fn common_for(locale: Locale, en: &'static str) -> &'static str {
    let (zh_cn, zh_tw) = match en {
        "Search" => ("搜索", "搜尋"),
        "Explorer" => ("资源管理器", "檔案總管"),
        "No files found" => ("未找到文件", "找不到檔案"),
        "Search results" => ("搜索结果", "搜尋結果"),
        "Editor" => ("编辑器", "編輯器"),
        "Use desktop version" => ("使用桌面版本", "使用桌面版本"),
        "Read only" => ("只读", "唯讀"),
        "Saving..." => ("保存中…", "儲存中…"),
        "Save" => ("保存", "儲存"),
        "Changes" => ("更改", "變更"),
        "Working tree is clean" => ("工作区干净", "工作區乾淨"),
        "Diff" => ("差异", "差異"),
        "Commit" => ("提交", "提交"),
        "Cancel" => ("取消", "取消"),
        "Working..." => ("处理中…", "處理中…"),
        "Process" => ("过程", "處理程序"),
        "Hide" => ("隐藏", "隱藏"),
        "Stage" => ("暂存", "暫存"),
        "Unstage" => ("取消暂存", "取消暫存"),
        "New" => ("新建", "新增"),
        "Refresh" => ("刷新", "重新整理"),
        "Close" => ("关闭", "關閉"),
        "Close terminal" => ("关闭终端", "關閉終端機"),
        "Send" => ("发送", "傳送"),
        "Agents" => ("Agent", "Agent"),
        "No Agent summaries published" => ("桌面尚未发布 Agent 摘要", "桌面尚未發佈 Agent 摘要"),
        "Provider profiles" => ("供应商配置", "供應商設定檔"),
        "No provider profiles published" => ("桌面尚未发布供应商配置", "桌面尚未發佈供應商設定檔"),
        "Health" => ("健康状态", "健康狀態"),
        "No health results yet" => ("还没有健康检查结果", "尚無健康檢查結果"),
        "Runtime probes" => ("运行时探测", "執行環境探測"),
        "No runtime probes recorded" => ("还没有运行时探测记录", "尚無執行環境探測記錄"),
        "Agent / provider / model" => ("Agent / 供应商 / 模型", "Agent / 供應商 / 模型"),
        "Runtime catalog unavailable" => ("运行时目录不可用", "執行環境目錄無法使用"),
        "Reasoning" => ("推理", "推理"),
        "Default" => ("默认", "預設"),
        "Mode" => ("模式", "模式"),
        "Session options" => ("会话选项", "工作階段選項"),
        "Loading value..." => ("正在加载值…", "正在載入值…"),
        "Configured by Agent" => ("由 Agent 配置", "由 Agent 設定"),
        "On" => ("开启", "開啟"),
        "Off" => ("关闭", "關閉"),
        "Apply runtime" => ("应用运行时", "套用執行環境"),
        "Applying..." => ("应用中…", "套用中…"),
        "Check health" => ("检查健康状态", "檢查健康狀態"),
        "Checking..." => ("检查中…", "檢查中…"),
        "Files" => ("文件", "檔案"),
        "Git" => ("Git", "Git"),
        "Terminal" => ("终端", "終端機"),
        "Providers" => ("供应商", "供應商"),
        "Runtime" => ("运行时", "執行環境"),
        "Loading" => ("加载中", "載入中"),
        "Clean" => ("干净", "乾淨"),
        "Unsaved" => ("未保存", "未儲存"),
        "Saving" => ("保存中", "儲存中"),
        "Saved" => ("已保存", "已儲存"),
        "Conflict" => ("冲突", "衝突"),
        "Offline" => ("离线", "離線"),
        "Too large" => ("过大", "過大"),
        "Try Again" => ("重试", "重試"),
        "Pairing..." => ("配对中…", "配對中…"),
        "Use QR Code" => ("使用二维码", "使用 QR Code"),
        "Local Network Pairing" => ("局域网配对", "區域網路配對"),
        "Find Desktops" => ("查找桌面端", "尋找桌面版"),
        "Nearby desktops" => ("附近的桌面端", "附近的桌面版"),
        "No nearby desktops found" => ("未找到附近的桌面端", "找不到附近的桌面版"),
        "Searching..." => ("搜索中…", "搜尋中…"),
        "Pair" => ("配对", "配對"),
        "Vibex Remote v2" => ("Vibex Remote v2", "Vibex Remote v2"),
        "Connecting to desktop..." => ("正在连接桌面端…", "正在連線至桌面版…"),
        "Desktop is unavailable" => ("桌面端不可用", "桌面版無法使用"),
        "Retry" => ("重试", "重試"),
        "Disconnect" => ("断开连接", "中斷連線"),
        "Workspace tools" => ("工作区工具", "工作區工具"),
        "Loading conversation..." => ("正在加载对话…", "正在載入對話…"),
        "No messages yet" => ("还没有消息", "尚無訊息"),
        "New session" => ("新建会话", "新增工作階段"),
        "Usage Statistics" => ("用量统计", "用量統計"),
        "Projects" => ("项目", "專案"),
        "Settings" => ("设置", "設定"),
        "Hosts" => ("主机", "主機"),
        "Switch host" => ("切换主机", "切換主機"),
        "Add host" => ("添加主机", "新增主機"),
        "Connected" => ("已连接", "已連線"),
        "Connection" => ("连接", "連線"),
        "Host" => ("主机", "主機"),
        "Language" => ("语言", "語言"),
        "Server ID" => ("服务器 ID", "伺服器 ID"),
        "Appearance" => ("外观", "外觀"),
        "Dark appearance" => ("深色外观", "深色外觀"),
        "Follows system language" => ("跟随系统语言", "跟隨系統語言"),
        "Notifications" => ("通知", "通知"),
        "Enable notifications" => ("启用通知", "啟用通知"),
        "Agent access" => ("Agent 访问", "Agent 存取"),
        "Provider settings" => ("供应商设置", "供應商設定"),
        "Runtime options" => ("运行时选项", "執行環境選項"),
        "About" => ("关于", "關於"),
        "Version" => ("版本", "版本"),
        "Usage details are available on the desktop host." => (
            "详细用量统计可在桌面端查看。",
            "詳細用量統計可在桌面版查看。",
        ),
        "Pair another host" => ("配对另一台主机", "配對另一台主機"),
        "No hosts paired" => ("尚未配对主机", "尚未配對主機"),
        "Back to hosts" => ("返回主机列表", "返回主機列表"),
        "Back" => ("返回", "返回"),
        "Rename" => ("重命名", "重新命名"),
        "Archive" => ("归档", "封存"),
        "Delete" => ("删除", "刪除"),
        "Conversations" => ("对话", "對話"),
        "Sessions" => ("会话", "工作階段"),
        "Continue" => ("继续", "繼續"),
        "Stop" => ("停止", "停止"),
        "Approve" => ("批准", "核准"),
        "Deny" => ("拒绝", "拒絕"),
        "Always allow" => ("始终允许", "一律允許"),
        "Show less" => ("收起", "顯示較少"),
        "Resolving..." => ("处理中…", "處理中…"),
        "No" => ("否", "否"),
        "Yes" => ("是", "是"),
        "Input requested" => ("需要输入", "需要輸入"),
        "Decline" => ("拒绝", "拒絕"),
        "Submitting..." => ("提交中…", "提交中…"),
        "Submit" => ("提交", "送出"),
        "File saved on desktop" => ("文件已保存到桌面端", "檔案已儲存到桌面版"),
        "Desktop file version loaded" => ("已加载桌面端文件版本", "已載入桌面版檔案版本"),
        "Commit created on desktop" => ("已在桌面端创建提交", "已在桌面版建立提交"),
        "Provider health probes completed" => ("供应商健康检查已完成", "供應商健康檢查已完成"),
        "Runtime selection sent to desktop" => {
            ("运行时选择已发送到桌面端", "執行環境選擇已傳送至桌面版")
        }
        "Loading repository status..." => ("正在加载仓库状态…", "正在載入儲存庫狀態…"),
        "No output yet" => ("还没有输出", "尚無輸出"),
        "Select or create a terminal" => ("选择或新建终端", "選擇或新增終端機"),
        "Select an Agent session first" => ("请先选择 Agent 会话", "請先選擇 Agent 工作階段"),
        "Value" => ("值", "值"),
        "Command" => ("命令", "命令"),
        "Sensitive read" => ("敏感读取", "敏感讀取"),
        "File write" => ("文件写入", "檔案寫入"),
        "Delete or move" => ("删除或移动", "刪除或移動"),
        "Network" => ("网络", "網路"),
        "Destructive Git" => ("破坏性 Git 操作", "破壞性 Git 操作"),
        "Config export" => ("配置导出", "設定匯出"),
        "Custom tool" => ("自定义工具", "自訂工具"),
        "Plan" => ("计划", "計畫"),
        "Tool" => ("工具", "工具"),
        "File operation" => ("文件操作", "檔案操作"),
        "Web search" => ("网页搜索", "網頁搜尋"),
        "Task update" => ("任务更新", "任務更新"),
        "Collaboration" => ("协作", "協作"),
        "Image generation" => ("图像生成", "圖像生成"),
        "System" => ("系统", "系統"),
        "Approval" => ("审批", "核准"),
        "Approval response" => ("审批结果", "核准結果"),
        "Input response" => ("输入结果", "輸入結果"),
        "Error" => ("错误", "錯誤"),
        "Message" => ("消息", "訊息"),
        "Session renamed" => ("会话已重命名", "工作階段已重新命名"),
        "Session archived" => ("会话已归档", "工作階段已封存"),
        "Session deleted" => ("会话已删除", "工作階段已刪除"),
        _ => return en,
    };
    text_for(locale, en, zh_cn, zh_tw)
}

pub fn common(en: &'static str) -> &'static str {
    common_for(current(), en)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_copy_remains_english_and_supported_copy_is_translated() {
        assert_eq!(text_for(Locale::ZhCn, "Save", "保存", "儲存"), "保存");
        assert_eq!(text_for(Locale::ZhTw, "Save", "保存", "儲存"), "儲存");
        assert_eq!(text_for(Locale::En, "Save", "保存", "儲存"), "Save");
        assert_eq!(
            common_for(Locale::ZhCn, "File saved on desktop"),
            "文件已保存到桌面端"
        );
        assert_eq!(
            common_for(Locale::ZhTw, "Runtime selection sent to desktop"),
            "執行環境選擇已傳送至桌面版"
        );
        assert_eq!(
            common_for(Locale::ZhCn, "provider supplied label"),
            "provider supplied label"
        );
    }
}
