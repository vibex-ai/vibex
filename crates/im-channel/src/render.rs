use vibex_core::{AgentSession, PermissionRequest};

use crate::i18n::ImLocale;

/// The most characters one chat message carries.
///
/// Telegram's hard limit is 4096 UTF-16 code units; 3800 leaves room for the
/// part marker and keeps a multi-byte character from pushing a message over.
const MAX_MESSAGE_CHARS: usize = 3800;

/// Extra room reserved for the `(3/7)` prefix on a continued answer.
const PART_MARKER_RESERVE: usize = 24;

pub fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Splits one answer into ordered parts a chat can actually deliver.
///
/// A long answer used to be cut at the limit with a single ellipsis, so the
/// reader lost the rest of it. The answer is instead split on paragraph, then
/// line, then word boundaries, and each part is labelled `(n/total)`. A single
/// unbroken run — a minified file, a long URL — is cut at the limit because
/// there is no boundary to prefer.
pub fn final_answer_parts(text: &str, locale: ImLocale) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return vec![text.to_string()];
    }
    let budget = MAX_MESSAGE_CHARS - PART_MARKER_RESERVE;
    let chunks = split_message(text, budget);
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let marker = match locale {
                ImLocale::En => format!("({}/{})", index + 1, total),
                ImLocale::ZhCn => format!("（{}/{})", index + 1, total),
                ImLocale::ZhTw => format!("（{}/{}）", index + 1, total),
            };
            format!(
                "{marker}
{chunk}"
            )
        })
        .collect()
}

/// Splits on the largest boundary that fits: blank line, then newline, then
/// whitespace, then a hard cut.
fn split_message(text: &str, budget: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while rest.chars().count() > budget {
        let window: String = rest.chars().take(budget).collect();
        let cut = window
            .rfind("\n\n")
            .map(|index| index + 2)
            .or_else(|| window.rfind('\n').map(|index| index + 1))
            .or_else(|| window.rfind(char::is_whitespace).map(|index| index + 1))
            .filter(|cut| *cut > budget / 4)
            .unwrap_or(window.len());
        let (head, tail) = rest.split_at(cut);
        parts.push(head.trim_end().to_string());
        rest = tail.trim_start();
    }
    if !rest.trim().is_empty() {
        parts.push(rest.trim_end().to_string());
    }
    parts
}

pub fn error_message(code: &str, message: &str) -> String {
    format!("Vibex error ({}): {}", code, truncate(message, 1200))
}

pub fn help(locale: ImLocale, prefix: &str) -> String {
    match locale {
        ImLocale::En => format!(
            "Vibex IM commands\n\
             /pair <code>  pair this chat\n\
             /whoami      show your role\n\
             /status      current session status\n\
             /sessions    recent sessions\n\
             /use <n|id>  bind a session\n\
             /new <text>  create a session and send\n\
             /stop        stop the running turn\n\
             /approve [code]\n\
             /always [code]\n\
             /deny [code]\n\
             /say <text>  send text starting with /\n\
             /unbind      remove this binding\n\
             Prefix: {prefix}"
        ),
        ImLocale::ZhCn => format!(
            "Vibex IM 命令\n\
             /pair <配对码>  配对这个聊天\n\
             /whoami      查看身份和角色\n\
             /status      当前会话状态\n\
             /sessions    最近会话\n\
             /use <序号|ID> 绑定会话\n\
             /new <内容>  新建会话并发送\n\
             /stop        停止当前回合\n\
             /approve [短码] 批准\n\
             /always [短码] 本会话总是允许\n\
             /deny [短码] 拒绝\n\
             /say <内容>  原样发送以 / 开头的文本\n\
             /unbind      解绑当前聊天\n\
             前缀：{prefix}"
        ),
        ImLocale::ZhTw => format!(
            "Vibex IM 指令\n\
             /pair <配對碼>  配對這個聊天\n\
             /whoami      查看身分和角色\n\
             /status      目前工作階段狀態\n\
             /sessions    最近工作階段\n\
             /use <序號|ID> 綁定工作階段\n\
             /new <內容>  建立工作階段並傳送\n\
             /stop        停止目前回合\n\
             /approve [短碼] 批准\n\
             /always [短碼] 本工作階段總是允許\n\
             /deny [短碼] 拒絕\n\
             /say <內容>  原樣傳送以 / 開頭的文字\n\
             /unbind      解除目前聊天綁定\n\
             前綴：{prefix}"
        ),
    }
}

/// Sent once at start-up when work finished while the runtime was not running.
pub fn resume_notice(locale: ImLocale, missed_final_answers: usize) -> &'static str {
    match (locale, missed_final_answers) {
        (ImLocale::En, 0) => {
            "Vibex is back. A pending approval could not be delivered before the restart and is re-sent below."
        }
        (ImLocale::En, _) => {
            "Vibex is back. Some replies finished while it was offline; open Vibex for the full transcript. Pending approvals are re-sent below."
        }
        (ImLocale::ZhCn, 0) => "Vibex 已恢复。重启前有一条待处理审批未送达，已在下方重新发送。",
        (ImLocale::ZhCn, _) => {
            "Vibex 已恢复。停机期间有回复已完成，完整内容请在 Vibex 里查看；待处理审批已在下方重新发送。"
        }
        (ImLocale::ZhTw, 0) => "Vibex 已恢復。重新啟動前有一筆待處理審批未送達，已在下方重新傳送。",
        (ImLocale::ZhTw, _) => {
            "Vibex 已恢復。停機期間有回覆已完成，完整內容請在 Vibex 裡查看；待處理審批已在下方重新傳送。"
        }
    }
}

/// Sent after the subscription fell behind and some live events were lost.
pub fn lag_notice(locale: ImLocale) -> &'static str {
    match locale {
        ImLocale::En => {
            "Vibex fell behind and some live updates were not delivered. Send /status for the current state; pending approvals are re-sent below."
        }
        ImLocale::ZhCn => {
            "Vibex 处理积压时漏掉了一些实时更新。发送 /status 查看当前状态；待处理的审批已在下方重新发送。"
        }
        ImLocale::ZhTw => {
            "Vibex 處理積壓時漏掉了一些即時更新。傳送 /status 查看目前狀態；待處理的審批已在下方重新傳送。"
        }
    }
}

pub fn session_list(sessions: &[AgentSession], locale: ImLocale) -> String {
    let mut text = match locale {
        ImLocale::En => "Recent sessions\n".to_string(),
        ImLocale::ZhCn => "最近会话\n".to_string(),
        ImLocale::ZhTw => "最近工作階段\n".to_string(),
    };
    if sessions.is_empty() {
        text.push_str(match locale {
            ImLocale::En => "No sessions yet. Use /new <text> to start one.",
            ImLocale::ZhCn => "还没有会话。用 /new <内容> 新建一个。",
            ImLocale::ZhTw => "還沒有工作階段。用 /new <內容> 建立一個。",
        });
        return text;
    }
    for (index, session) in sessions.iter().enumerate() {
        let title = if session.title.trim().is_empty() {
            "Untitled".to_string()
        } else {
            truncate(&session.title, 64)
        };
        text.push_str(&format!(
            "{}. {} · {} · {}\n",
            index + 1,
            short_session_id(session),
            title,
            state_label(session, locale),
        ));
    }
    text.push_str(match locale {
        ImLocale::En => "Use /use <n> to bind one.",
        ImLocale::ZhCn => "用 /use <序号> 绑定一个。",
        ImLocale::ZhTw => "用 /use <序號> 綁定一個。",
    });
    text
}

pub fn short_session_id(session: &AgentSession) -> String {
    let id = session.id.as_str();
    id.rsplit('_')
        .next()
        .unwrap_or(id)
        .chars()
        .take(8)
        .collect()
}

pub fn state_label(session: &AgentSession, locale: ImLocale) -> &'static str {
    use vibex_core::AgentSessionState;
    match (session.display_state(), locale) {
        (AgentSessionState::Initializing, ImLocale::ZhCn) => "初始化",
        (AgentSessionState::Initializing, ImLocale::ZhTw) => "初始化",
        (AgentSessionState::Initializing, _) => "initializing",
        (AgentSessionState::Idle, ImLocale::ZhCn) => "空闲",
        (AgentSessionState::Idle, ImLocale::ZhTw) => "閒置",
        (AgentSessionState::Idle, _) => "idle",
        (AgentSessionState::Running, ImLocale::ZhCn) => "运行中",
        (AgentSessionState::Running, ImLocale::ZhTw) => "執行中",
        (AgentSessionState::Running, _) => "running",
        (AgentSessionState::NeedsInput, ImLocale::ZhCn) => "等待输入",
        (AgentSessionState::NeedsInput, ImLocale::ZhTw) => "等待輸入",
        (AgentSessionState::NeedsInput, _) => "needs input",
        (AgentSessionState::Error, ImLocale::ZhCn) => "错误",
        (AgentSessionState::Error, ImLocale::ZhTw) => "錯誤",
        (AgentSessionState::Error, _) => "error",
        (AgentSessionState::Closed, ImLocale::ZhCn) => "已关闭",
        (AgentSessionState::Closed, ImLocale::ZhTw) => "已關閉",
        (AgentSessionState::Closed, _) => "closed",
        (AgentSessionState::Archived, ImLocale::ZhCn) => "已归档",
        (AgentSessionState::Archived, ImLocale::ZhTw) => "已封存",
        (AgentSessionState::Archived, _) => "archived",
    }
}

pub fn permission_text(request: &PermissionRequest, locale: ImLocale) -> String {
    let mut text = match locale {
        ImLocale::En => "Approval required\n".to_string(),
        ImLocale::ZhCn => "需要批准\n".to_string(),
        ImLocale::ZhTw => "需要批准\n".to_string(),
    };
    text.push_str(&format!(
        "{}: {}\n",
        risk_label(request, locale),
        request.title
    ));
    for detail in request.details.iter().take(8) {
        text.push_str(&format!(
            "{}: {}\n",
            detail.label,
            truncate(&detail.value, 500)
        ));
    }
    text.push_str(match locale {
        ImLocale::En => "Choose an action below, or reply /approve <code>.",
        ImLocale::ZhCn => "点击下面的按钮，或回复 /approve <短码>。",
        ImLocale::ZhTw => "點擊下面的按鈕，或回覆 /approve <短碼>。",
    });
    text
}

fn risk_label(request: &PermissionRequest, locale: ImLocale) -> &'static str {
    use vibex_core::PermissionRiskCategory;
    match (request.risk_category, locale) {
        (PermissionRiskCategory::Command, ImLocale::ZhCn) => "命令",
        (PermissionRiskCategory::Command, ImLocale::ZhTw) => "命令",
        (PermissionRiskCategory::Command, _) => "command",
        (PermissionRiskCategory::FileReadSensitive, ImLocale::ZhCn) => "敏感读取",
        (PermissionRiskCategory::FileReadSensitive, ImLocale::ZhTw) => "敏感讀取",
        (PermissionRiskCategory::FileReadSensitive, _) => "sensitive read",
        (PermissionRiskCategory::FileWrite, ImLocale::ZhCn) => "文件写入",
        (PermissionRiskCategory::FileWrite, ImLocale::ZhTw) => "檔案寫入",
        (PermissionRiskCategory::FileWrite, _) => "file write",
        (PermissionRiskCategory::FileDeleteOrMove, ImLocale::ZhCn) => "删除/移动",
        (PermissionRiskCategory::FileDeleteOrMove, ImLocale::ZhTw) => "刪除/移動",
        (PermissionRiskCategory::FileDeleteOrMove, _) => "delete or move",
        (PermissionRiskCategory::Network, ImLocale::ZhCn) => "网络",
        (PermissionRiskCategory::Network, ImLocale::ZhTw) => "網路",
        (PermissionRiskCategory::Network, _) => "network",
        (PermissionRiskCategory::GitDestructive, ImLocale::ZhCn) => "Git 破坏性操作",
        (PermissionRiskCategory::GitDestructive, ImLocale::ZhTw) => "Git 破壞性操作",
        (PermissionRiskCategory::GitDestructive, _) => "destructive git",
        (PermissionRiskCategory::ProviderConfigExport, ImLocale::ZhCn) => "配置导出",
        (PermissionRiskCategory::ProviderConfigExport, ImLocale::ZhTw) => "設定匯出",
        (PermissionRiskCategory::ProviderConfigExport, _) => "config export",
        (PermissionRiskCategory::CustomTool, ImLocale::ZhCn) => "自定义工具",
        (PermissionRiskCategory::CustomTool, ImLocale::ZhTw) => "自訂工具",
        (PermissionRiskCategory::CustomTool, _) => "custom tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_answer_stays_one_message() {
        let parts = final_answer_parts("All done.", ImLocale::En);
        assert_eq!(parts, vec!["All done.".to_string()]);
    }

    #[test]
    fn a_long_answer_is_split_on_a_boundary_and_labelled() {
        let paragraph = "Sentence that keeps going. ".repeat(60);
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let parts = final_answer_parts(&text, ImLocale::En);
        assert!(parts.len() > 1, "expected more than one part");
        for (index, part) in parts.iter().enumerate() {
            assert!(part.starts_with(&format!("({}/", index + 1)));
            assert!(part.chars().count() <= MAX_MESSAGE_CHARS);
        }
        // Every word survives: splitting must not drop content.
        let rejoined: String = parts
            .iter()
            .map(|part| part.split_once('\n').map(|(_, body)| body).unwrap_or(part))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            rejoined.split_whitespace().count(),
            text.split_whitespace().count()
        );
    }

    #[test]
    fn an_unbroken_run_is_cut_at_the_limit() {
        let text = "x".repeat(MAX_MESSAGE_CHARS * 2 + 10);
        let parts = final_answer_parts(&text, ImLocale::En);
        assert!(parts.len() >= 2, "a run longer than one message is split");
        assert!(
            parts
                .iter()
                .all(|part| part.chars().count() <= MAX_MESSAGE_CHARS)
        );
        // Nothing is lost: the parts carry every character of the original.
        let body: String = parts
            .iter()
            .map(|part| part.split_once('\n').map(|(_, body)| body).unwrap_or(part))
            .collect();
        assert_eq!(body.chars().count(), text.chars().count());
    }

    #[test]
    fn whitespace_only_answers_produce_no_parts() {
        assert!(final_answer_parts("   \n\n  ", ImLocale::En).is_empty());
    }
}
