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

pub fn error_message(code: &str, message: &str, locale: ImLocale) -> String {
    format!(
        "{} ({code})\n{}",
        locale.text("Vibex error", "Vibex 错误", "Vibex 錯誤"),
        truncate(message, 1200)
    )
}

/// The one-line answer to `/help`, with the full list behind a button.
pub fn help_short(locale: ImLocale) -> String {
    match locale {
        ImLocale::En => "Vibex\n\
             /status session · /sessions switch\n\
             /new <text> start a session\n\
             /model /provider /effort /mode /agent tune it\n\
             Type / for every command"
            .to_string(),
        ImLocale::ZhCn => "Vibex\n\
             /status 会话 · /sessions 切换\n\
             /new <内容> 新建会话\n\
             /model /provider /effort /mode /agent 调整\n\
             输入 / 查看全部命令"
            .to_string(),
        ImLocale::ZhTw => "Vibex\n\
             /status 工作階段 · /sessions 切換\n\
             /new <內容> 建立工作階段\n\
             /model /provider /effort /mode /agent 調整\n\
             輸入 / 查看全部指令"
            .to_string(),
    }
}

/// The command menu a client shows in its `/` picker.
pub fn command_specs(locale: ImLocale) -> Vec<crate::types::ImCommandSpec> {
    let entries: &[(&str, &str)] = match locale {
        ImLocale::En => &[
            ("pair", "Pair this chat with a code"),
            ("status", "Session, Agent, model and pending approvals"),
            ("sessions", "Pick a session to bind"),
            ("use", "Bind a session by number or id"),
            ("new", "Start a session and send the first prompt"),
            ("model", "Choose the model"),
            ("provider", "Choose the provider"),
            ("effort", "Choose the reasoning effort"),
            ("mode", "Choose the Agent mode"),
            ("agent", "Switch the Agent"),
            ("stop", "Interrupt the running turn"),
            ("approve", "Approve the pending request"),
            ("always", "Allow for the rest of the session"),
            ("deny", "Deny the pending request"),
            ("say", "Send text that starts with a slash"),
            ("whoami", "Show your role"),
            ("unbind", "Drop this chat's binding"),
            ("help", "Commands and shortcuts"),
        ],
        ImLocale::ZhCn => &[
            ("pair", "用配对码配对这个聊天"),
            ("status", "会话、Agent、模型与待审批"),
            ("sessions", "选择要绑定的会话"),
            ("use", "按序号或 ID 绑定会话"),
            ("new", "新建会话并发送第一条内容"),
            ("model", "选择模型"),
            ("provider", "选择供应商"),
            ("effort", "选择思考强度"),
            ("mode", "选择 Agent 模式"),
            ("agent", "切换 Agent"),
            ("stop", "停止当前回合"),
            ("approve", "批准待处理请求"),
            ("always", "本会话内总是允许"),
            ("deny", "拒绝待处理请求"),
            ("say", "原样发送以斜杠开头的文本"),
            ("whoami", "查看身份和角色"),
            ("unbind", "解除当前聊天的绑定"),
            ("help", "命令与快捷操作"),
        ],
        ImLocale::ZhTw => &[
            ("pair", "用配對碼配對這個聊天"),
            ("status", "工作階段、Agent、模型與待審批"),
            ("sessions", "選擇要綁定的工作階段"),
            ("use", "按序號或 ID 綁定工作階段"),
            ("new", "建立工作階段並傳送第一則內容"),
            ("model", "選擇模型"),
            ("provider", "選擇供應商"),
            ("effort", "選擇思考強度"),
            ("mode", "選擇 Agent 模式"),
            ("agent", "切換 Agent"),
            ("stop", "停止目前回合"),
            ("approve", "批准待處理請求"),
            ("always", "本工作階段內總是允許"),
            ("deny", "拒絕待處理請求"),
            ("say", "原樣傳送以斜線開頭的內容"),
            ("whoami", "查看身分和角色"),
            ("unbind", "解除目前聊天的綁定"),
            ("help", "指令與快捷操作"),
        ],
    };
    entries
        .iter()
        .map(|(command, description)| crate::types::ImCommandSpec::new(*command, *description))
        .collect()
}

/// The live progress line.
pub fn working(locale: ImLocale, label: Option<&str>, seconds: i64) -> String {
    let verb = match locale {
        ImLocale::En => "Working",
        ImLocale::ZhCn => "处理中",
        ImLocale::ZhTw => "處理中",
    };
    let subject = label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or(verb);
    format!("⏳ {subject} · {seconds}s")
}

/// The live answer, bounded to its tail and marked as still growing.
pub fn live_answer(text: &str, tail_chars: usize, _locale: ImLocale) -> String {
    let trimmed = text.trim_start();
    let length = trimmed.chars().count();
    let body = if length > tail_chars {
        let skip = length - tail_chars;
        format!("…{}", trimmed.chars().skip(skip).collect::<String>())
    } else {
        trimmed.to_string()
    };
    format!("{body}▍")
}

/// The marker an interrupted turn leaves behind.
pub fn stopped(locale: ImLocale) -> &'static str {
    match locale {
        ImLocale::En => "⏹ Stopped.",
        ImLocale::ZhCn => "⏹ 已停止。",
        ImLocale::ZhTw => "⏹ 已停止。",
    }
}

pub fn help(locale: ImLocale) -> String {
    let mut text = String::from(locale.text("Vibex commands", "Vibex 命令", "Vibex 指令"));
    for spec in command_specs(locale) {
        text.push('\n');
        text.push_str(&format!("/{}  {}", spec.command, spec.description));
    }
    text
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

/// Answers a sender whose message arrived faster than the channel accepts.
pub fn rate_limited(locale: ImLocale, retry_after_secs: u64) -> String {
    match locale {
        ImLocale::En => {
            format!("Too many messages for this chat. Try again in about {retry_after_secs}s.")
        }
        ImLocale::ZhCn => format!("这个聊天的消息太频繁了，请约 {retry_after_secs} 秒后再试。"),
        ImLocale::ZhTw => format!("這個聊天的訊息太頻繁了，請約 {retry_after_secs} 秒後再試。"),
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
    let mut text = String::from(locale.text("Sessions", "会话", "工作階段"));
    if sessions.is_empty() {
        text.push('\n');
        text.push_str(locale.text(
            "None yet. /new <text> starts one.",
            "还没有会话。用 /new <内容> 新建一个。",
            "還沒有工作階段。用 /new <內容> 建立一個。",
        ));
        return text;
    }
    for (index, session) in sessions.iter().enumerate() {
        let title = if session.title.trim().is_empty() {
            locale.text("Untitled", "未命名", "未命名").to_string()
        } else {
            truncate(session.title.trim(), 48)
        };
        text.push_str(&format!(
            "\n{}. {} · {title} · {}",
            index + 1,
            short_session_id(session),
            state_label(session, locale),
        ));
    }
    text
}

/// The line under a session list that has buttons to tap.
pub fn session_tap_hint(locale: ImLocale) -> &'static str {
    locale.text(
        "Tap one to bind it.",
        "点一个即可绑定。",
        "點一個即可綁定。",
    )
}

/// The line under a session list with no buttons, where a number is typed.
pub fn session_use_hint(locale: ImLocale) -> &'static str {
    locale.text(
        "Use /use <n> to bind one.",
        "用 /use <序号> 绑定一个。",
        "用 /use <序號> 綁定一個。",
    )
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
    let mut text = format!(
        "🔐 {} · {}",
        locale.text("Approval", "需要批准", "需要批准"),
        risk_label(request, locale)
    );
    text.push('\n');
    text.push_str(&truncate(&request.title, 200));
    for detail in request.details.iter().take(6) {
        text.push_str(&format!(
            "\n{}: {}",
            detail.label,
            truncate(&detail.value, 200)
        ));
    }
    text.push('\n');
    text.push_str(locale.text(
        "Tap a button, or reply /approve <code>.",
        "点下面的按钮，或回复 /approve <短码>。",
        "點下面的按鈕，或回覆 /approve <短碼>。",
    ));
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

fn default_true_text(locale: ImLocale) -> &'static str {
    match locale {
        ImLocale::En => "default",
        ImLocale::ZhCn => "默认",
        ImLocale::ZhTw => "預設",
    }
}

fn effort_label(locale: ImLocale) -> &'static str {
    match locale {
        ImLocale::En => "effort",
        ImLocale::ZhCn => "思考",
        ImLocale::ZhTw => "思考",
    }
}

fn mode_label(locale: ImLocale) -> &'static str {
    match locale {
        ImLocale::En => "mode",
        ImLocale::ZhCn => "模式",
        ImLocale::ZhTw => "模式",
    }
}

/// The selection in force, one line, for a picker card or `/status`.
pub fn runtime_summary(options: &vibex_core::ImRuntimeOptions) -> String {
    format!("{} · {}", options.agent_label, options.model_label)
}

/// The tuning that differs from a model default, or nothing when it does not.
///
/// A card that always prints `effort default · mode default` buries the one
/// value the reader came to change, so only real choices are shown.
pub fn runtime_tuning(locale: ImLocale, options: &vibex_core::ImRuntimeOptions) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(effort) = options.effort_label.as_deref() {
        parts.push(format!("{} {effort}", effort_label(locale)));
    }
    if let Some(mode) = options.mode_label.as_deref() {
        parts.push(format!("{} {mode}", mode_label(locale)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A picker card: what is being chosen, what it is now, and what happens next.
pub fn picker(locale: ImLocale, title: &str, current: &str) -> String {
    format!(
        "{title}\n{} {current}\n{}",
        locale.text("Now", "当前", "目前"),
        locale.text(
            "Tap a value; it applies to the next turn.",
            "点选后下一轮生效。",
            "點選後下一輪生效。",
        )
    )
}

/// Confirms one setting and shows the selection it left behind.
pub fn runtime_applied(field: &str, value: &str, selection: &str) -> String {
    format!("✅ {field} · {value}\n{}", truncate(selection, 200))
}

/// Who is speaking, and in which channel.
pub fn identity(locale: ImLocale, name: &str, role: &str, channel_id: &str) -> String {
    format!(
        "{name} · {role}\n{} {}",
        locale.text("Channel", "频道", "頻道"),
        short_channel_id(channel_id)
    )
}

/// The session card: what it is, what it runs, and anything waiting on a human.
pub fn status_card(
    locale: ImLocale,
    session: &AgentSession,
    role: &str,
    runtime: Option<&vibex_core::ImRuntimeOptions>,
    pending: usize,
) -> String {
    let title = if session.title.trim().is_empty() {
        locale.text("Untitled", "未命名", "未命名").to_string()
    } else {
        truncate(session.title.trim(), 64)
    };
    let mut lines = vec![
        format!("#{} · {title}", short_session_id(session)),
        format!("{} · {role}", state_label(session, locale)),
    ];
    if let Some(runtime) = runtime {
        lines.push(runtime_summary(runtime));
        if let Some(tuning) = runtime_tuning(locale, runtime) {
            lines.push(tuning);
        }
    }
    if pending > 0 {
        lines.push(format!(
            "{} {pending}",
            locale.text("Pending", "待审批", "待審批")
        ));
    }
    lines.join("\n")
}

/// The word a resolved approval is reported with.
pub fn approval_result(
    locale: ImLocale,
    response: vibex_core::PermissionResponseKind,
) -> &'static str {
    use vibex_core::PermissionResponseKind;
    match (response, locale) {
        (PermissionResponseKind::Approve, ImLocale::ZhCn) => "✅ 已批准",
        (PermissionResponseKind::Approve, ImLocale::ZhTw) => "✅ 已批准",
        (PermissionResponseKind::Approve, _) => "✅ Approved",
        (PermissionResponseKind::Deny, ImLocale::ZhCn) => "⛔ 已拒绝",
        (PermissionResponseKind::Deny, ImLocale::ZhTw) => "⛔ 已拒絕",
        (PermissionResponseKind::Deny, _) => "⛔ Denied",
        (PermissionResponseKind::AlwaysAllowForSession, ImLocale::ZhCn) => "✅ 本会话已允许",
        (PermissionResponseKind::AlwaysAllowForSession, ImLocale::ZhTw) => "✅ 本工作階段已允許",
        (PermissionResponseKind::AlwaysAllowForSession, _) => "✅ Allowed for this session",
    }
}

pub fn short_channel_id(channel_id: &str) -> String {
    match channel_id.split_once('_') {
        Some((prefix, rest)) => {
            let head: String = rest.chars().take(8).collect();
            format!("{prefix}_{head}")
        }
        None => truncate(channel_id, 16),
    }
}

/// A one-line summary of a selection that has just been applied.
pub fn selection_line(locale: ImLocale, selection: &vibex_core::SessionRuntimeSelection) -> String {
    let mut parts = vec![
        selection.agent_id.as_str().to_string(),
        selection
            .model
            .model_id()
            .unwrap_or_else(|| default_true_text(locale))
            .to_string(),
    ];
    if let Some(effort) = selection.reasoning_effort.as_deref() {
        parts.push(format!("{} {effort}", effort_label(locale)));
    }
    if let Some(mode) = selection.mode_id.as_deref() {
        parts.push(format!("{} {mode}", mode_label(locale)));
    }
    parts.join(" · ")
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

    fn options_with(effort: Option<&str>, mode: Option<&str>) -> vibex_core::ImRuntimeOptions {
        vibex_core::ImRuntimeOptions {
            session_revision: 1,
            selection_revision: 1,
            current: vibex_core::SessionRuntimeSelection::provider(
                vibex_core::AgentId::parse("claude").expect("agent id"),
                vibex_core::ProviderProfileId::new(),
                "model",
            ),
            agent_label: "Claude Code".to_string(),
            model_label: "model".to_string(),
            effort_label: effort.map(str::to_string),
            mode_label: mode.map(str::to_string),
            agents: Vec::new(),
            providers: Vec::new(),
            models: Vec::new(),
            efforts: Vec::new(),
            modes: Vec::new(),
        }
    }

    #[test]
    fn a_status_card_shows_only_the_tuning_that_was_chosen() {
        assert_eq!(
            runtime_tuning(ImLocale::En, &options_with(None, None)),
            None,
            "a default effort and mode are not worth a line"
        );
        assert_eq!(
            runtime_tuning(ImLocale::En, &options_with(Some("max"), None)).as_deref(),
            Some("effort max")
        );
        assert_eq!(
            runtime_tuning(ImLocale::En, &options_with(Some("max"), Some("read-only"))).as_deref(),
            Some("effort max · mode read-only")
        );
    }

    #[test]
    fn a_picker_names_the_field_and_its_current_value() {
        let card = picker(ImLocale::ZhCn, "模型", "loomy/DeepSeek");
        assert_eq!(card, "模型\n当前 loomy/DeepSeek\n点选后下一轮生效。");
    }

    #[test]
    fn an_identity_card_shortens_the_channel_id() {
        let card = identity(
            ImLocale::ZhCn,
            "p",
            "操作",
            "imchannel_817f9b1fdb164ee5a60474330e802b85",
        );
        assert_eq!(card, "p · 操作\n频道 imchannel_817f9b1f");
    }

    #[test]
    fn a_session_list_carries_no_trailing_hint_of_its_own() {
        let list = session_list(&[], ImLocale::ZhCn);
        assert!(list.starts_with("会话\n"), "{list}");
        assert!(
            !list.contains("/use"),
            "the caller adds its own hint: {list}"
        );
    }

    #[test]
    fn an_approval_result_is_read_in_the_chat_s_language() {
        assert_eq!(
            approval_result(ImLocale::En, vibex_core::PermissionResponseKind::Approve),
            "✅ Approved"
        );
        assert_eq!(
            approval_result(ImLocale::ZhCn, vibex_core::PermissionResponseKind::Deny),
            "⛔ 已拒绝"
        );
        assert_eq!(
            approval_result(
                ImLocale::ZhTw,
                vibex_core::PermissionResponseKind::AlwaysAllowForSession
            ),
            "✅ 本工作階段已允許"
        );
    }

    #[test]
    fn the_full_help_lists_every_command_the_menu_offers() {
        let help = help(ImLocale::En);
        for spec in command_specs(ImLocale::En) {
            assert!(
                help.contains(&format!("/{}  {}", spec.command, spec.description)),
                "{} is missing from the full help",
                spec.command
            );
        }
        assert!(help.contains("/provider"));
    }

    #[test]
    fn whitespace_only_answers_produce_no_parts() {
        assert!(final_answer_parts("   \n\n  ", ImLocale::En).is_empty());
    }
}
