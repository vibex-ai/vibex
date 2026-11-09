//! Product copy for the errors the runtime writes itself.
//!
//! Agent adapters report failures as a stable code plus an English
//! explanation. Both are persisted in the timeline, and both reach every
//! client, so the explanation is product copy rather than provider prose: it
//! has to follow the reader's language like every other label. The code is the
//! identity — several messages can share one code, and one sentence can be
//! re-tagged by a generic adapter — so the catalogue matches the pair first and
//! falls back to the sentence.
//!
//! Text that came from the provider is not in this table and is returned
//! exactly as written: only the runtime knows what it meant, and a client
//! inventing a translation for an upstream error would be worse than showing
//! the provider's own words.

use crate::locale::Locale;

/// One runtime error sentence and its translations.
#[derive(Clone, Copy)]
struct ErrorCopy {
    code: &'static str,
    en: &'static str,
    zh_cn: &'static str,
    zh_tw: &'static str,
}

impl ErrorCopy {
    fn text(self, locale: Locale) -> &'static str {
        locale.text(self.en, self.zh_cn, self.zh_tw)
    }
}

/// The runtime's own provider and transport failures.
///
/// The list covers what a turn can put in front of the reader: an account that
/// cannot use the provider, a missing sign-in, an adapter that stopped
/// answering, and the Codex/OpenCode terminal errors that used to surface as
/// raw English. Diagnostics stay in the error's structured fields and are not
/// translated here.
const RUNTIME_ERRORS: &[ErrorCopy] = &[
    ErrorCopy {
        code: "provider_access_unavailable",
        en: "The model provider rejected the request because account usage is unavailable",
        zh_cn: "模型供应商拒绝了请求，因为账户用量不可用",
        zh_tw: "模型供應商拒絕了請求，因為帳戶用量無法使用",
    },
    ErrorCopy {
        code: "provider_authentication_required",
        en: "Agent authentication is required",
        zh_cn: "需要先完成 Agent 身份验证",
        zh_tw: "需要先完成 Agent 身分驗證",
    },
    ErrorCopy {
        code: "provider_credential_invalid",
        en: "The configured provider credential has an unusable format",
        zh_cn: "已配置的供应商凭据格式不可用",
        zh_tw: "已設定的供應商憑證格式無法使用",
    },
    ErrorCopy {
        code: "agent_authentication_required",
        en: "Agent account must be authenticated before the session can run",
        zh_cn: "会话运行前必须先完成 Agent 账户身份验证",
        zh_tw: "工作階段執行前必須先完成 Agent 帳戶身分驗證",
    },
    ErrorCopy {
        code: "acp_auth_required",
        en: "authentication required",
        zh_cn: "需要身份验证",
        zh_tw: "需要身分驗證",
    },
    ErrorCopy {
        code: "grok_model_api_unavailable",
        en: "Grok model service is temporarily unavailable",
        zh_cn: "Grok 模型服务暂时不可用",
        zh_tw: "Grok 模型服務暫時無法使用",
    },
    ErrorCopy {
        code: "acp_rpc_error",
        en: "ACP agent returned a JSON-RPC error",
        zh_cn: "ACP Agent 返回了 JSON-RPC 错误",
        zh_tw: "ACP Agent 傳回了 JSON-RPC 錯誤",
    },
    ErrorCopy {
        code: "acp_request_timeout",
        en: "ACP agent did not answer the JSON-RPC request in time",
        zh_cn: "ACP Agent 未及时响应 JSON-RPC 请求",
        zh_tw: "ACP Agent 未及時回應 JSON-RPC 請求",
    },
    ErrorCopy {
        code: "acp_process_exited",
        en: "ACP agent process exited before returning a JSON-RPC response",
        zh_cn: "ACP Agent 进程在返回 JSON-RPC 响应前已退出",
        zh_tw: "ACP Agent 處理程序在傳回 JSON-RPC 回應前已結束",
    },
    ErrorCopy {
        code: "acp_process_closed",
        en: "ACP agent process is no longer running",
        zh_cn: "ACP Agent 进程已不再运行",
        zh_tw: "ACP Agent 處理程序已不再執行",
    },
    ErrorCopy {
        code: "acp_request_encode_failed",
        en: "ACP JSON-RPC message could not be encoded",
        zh_cn: "无法编码 ACP JSON-RPC 消息",
        zh_tw: "無法編碼 ACP JSON-RPC 訊息",
    },
    ErrorCopy {
        code: "acp_session_new_invalid",
        en: "ACP session/new response did not include a session id",
        zh_cn: "ACP session/new 响应未包含会话 ID",
        zh_tw: "ACP session/new 回應未包含工作階段 ID",
    },
    ErrorCopy {
        code: "acp_session_fork_invalid",
        en: "ACP session/fork response did not include a session id",
        zh_cn: "ACP session/fork 响应未包含会话 ID",
        zh_tw: "ACP session/fork 回應未包含工作階段 ID",
    },
    ErrorCopy {
        code: "agent_runtime_probe_session_id_missing",
        en: "ACP session/new did not return a session identity",
        zh_cn: "ACP session/new 未返回会话标识",
        zh_tw: "ACP session/new 未傳回工作階段識別碼",
    },
    ErrorCopy {
        code: "acp_restore_invalid_response",
        en: "ACP restore response was not an object",
        zh_cn: "ACP 恢复响应不是对象",
        zh_tw: "ACP 復原回應不是物件",
    },
    ErrorCopy {
        code: "acp_restore_invalid_response",
        en: "ACP restore response included an invalid native session id",
        zh_cn: "ACP 恢复响应包含无效的原生会话 ID",
        zh_tw: "ACP 復原回應包含無效的原生工作階段 ID",
    },
    ErrorCopy {
        code: "codex_model_list_page_limit_exceeded",
        en: "Codex model capability probe exceeded the page limit",
        zh_cn: "Codex 模型能力探测超出了分页上限",
        zh_tw: "Codex 模型能力探測超出了分頁上限",
    },
    ErrorCopy {
        code: "codex_model_list_invalid_response",
        en: "Codex model capability response did not contain a model list",
        zh_cn: "Codex 模型能力响应未包含模型列表",
        zh_tw: "Codex 模型能力回應未包含模型清單",
    },
    ErrorCopy {
        code: "acp_turn_without_output",
        en: "The Agent ended the turn without returning any model output",
        zh_cn: "Agent 结束了回合，但没有返回任何模型输出",
        zh_tw: "Agent 結束了回合，但沒有傳回任何模型輸出",
    },
    ErrorCopy {
        code: "acp_session_failure",
        en: "ACP agent reported a terminal session failure",
        zh_cn: "ACP Agent 报告了终结性会话失败",
        zh_tw: "ACP Agent 回報了終結性工作階段失敗",
    },
    ErrorCopy {
        code: "acp_runtime_lock_poisoned",
        en: "ACP runtime state could not be locked",
        zh_cn: "无法锁定 ACP 运行时状态",
        zh_tw: "無法鎖定 ACP 執行環境狀態",
    },
    ErrorCopy {
        code: "acp_elicitation_response_encode_failed",
        en: "failed to encode ACP elicitation response",
        zh_cn: "无法编码 ACP 征求响应",
        zh_tw: "無法編碼 ACP 徵求回應",
    },
    ErrorCopy {
        code: "acp_process_exited",
        en: "ACP process exited before the session attachment became ready",
        zh_cn: "ACP 进程在会话附件就绪前已退出",
        zh_tw: "ACP 處理程序在工作階段附件就緒前已結束",
    },
    ErrorCopy {
        code: "acp_binary_missing",
        en: "ACP agent binary was not found at the configured path",
        zh_cn: "在配置路径下未找到 ACP Agent 可执行文件",
        zh_tw: "在設定路徑下找不到 ACP Agent 執行檔",
    },
    ErrorCopy {
        code: "acp_spawn_failed",
        en: "ACP agent process could not be started",
        zh_cn: "无法启动 ACP Agent 进程",
        zh_tw: "無法啟動 ACP Agent 處理程序",
    },
    ErrorCopy {
        code: "acp_stdio_unavailable",
        en: "ACP agent stdin was not available",
        zh_cn: "ACP Agent 的标准输入不可用",
        zh_tw: "ACP Agent 的標準輸入無法使用",
    },
    ErrorCopy {
        code: "acp_stdio_unavailable",
        en: "ACP agent stdout was not available",
        zh_cn: "ACP Agent 的标准输出不可用",
        zh_tw: "ACP Agent 的標準輸出無法使用",
    },
    ErrorCopy {
        code: "acp_stdio_unavailable",
        en: "ACP agent stderr was not available",
        zh_cn: "ACP Agent 的标准错误输出不可用",
        zh_tw: "ACP Agent 的標準錯誤輸出無法使用",
    },
    ErrorCopy {
        code: "acp_process_stdin_closed",
        en: "ACP agent process stdin is closed",
        zh_cn: "ACP Agent 进程的标准输入已关闭",
        zh_tw: "ACP Agent 處理程序的標準輸入已關閉",
    },
    ErrorCopy {
        code: "acp_steering_response_invalid",
        en: "ACP steering response did not report a recognized outcome",
        zh_cn: "ACP 引导响应未报告可识别的结果",
        zh_tw: "ACP 引導回應未回報可辨識的結果",
    },
    ErrorCopy {
        code: "acp_fs_write_roots_unavailable",
        en: "ACP writable roots are unavailable",
        zh_cn: "ACP 可写根目录不可用",
        zh_tw: "ACP 可寫入根目錄無法使用",
    },
    ErrorCopy {
        code: "acp_registration_barrier_missing",
        en: "ACP attachment registration barrier was not created",
        zh_cn: "ACP 附件注册屏障未创建",
        zh_tw: "ACP 附件註冊屏障未建立",
    },
    ErrorCopy {
        code: "opencode_model_api_error",
        en: "OpenCode ended the model turn without returning any model output",
        zh_cn: "OpenCode 结束了模型回合，但没有返回任何模型输出",
        zh_tw: "OpenCode 結束了模型回合，但沒有傳回任何模型輸出",
    },
    ErrorCopy {
        code: "provider_retry_exhausted",
        en: "retry budget exhausted",
        zh_cn: "重试次数已耗尽",
        zh_tw: "重試次數已耗盡",
    },
    ErrorCopy {
        code: "acp_retry_exhausted",
        en: "Codex exhausted its automatic retry attempts",
        zh_cn: "Codex 已用完自动重试次数",
        zh_tw: "Codex 已用完自動重試次數",
    },
    ErrorCopy {
        code: "session_not_found",
        en: "not found",
        zh_cn: "未找到",
        zh_tw: "找不到",
    },
    ErrorCopy {
        code: "model_gateway_keys_cooling",
        en: "Every Key serving this model is cooling down after a failure; requests resume when one recovers",
        zh_cn: "所有为该模型服务的 Key 都在失败后冷却，待其中一个恢复后即可继续",
        zh_tw: "所有為該模型服務的 Key 都在失敗後冷卻，待其中一個恢復後即可繼續",
    },
    ErrorCopy {
        code: "model_gateway_upstream_failed",
        en: "The model gateway's upstream request failed",
        zh_cn: "模型网关的上游请求失败",
        zh_tw: "模型網關的上游請求失敗",
    },
];

/// The message a timeline error row should show.
///
/// `code` is the runtime's stable error code (`TimelineRow::title`) and
/// `message` is its English explanation (`TimelineRow::body`). A known pair is
/// replaced with the reader's language; anything else — including a provider
/// sentence carried inside a known code — is passed through untouched.
pub fn timeline_error_message(locale: Locale, code: &str, message: &str) -> String {
    if locale == Locale::En && !message.trim().is_empty() {
        return message.to_string();
    }
    if let Some(text) = formatted_error(locale, code, message) {
        return text;
    }
    // The timeline keeps the runtime's sentence and the Agent's explanation in
    // one body, separated by a blank line. Translate the sentence and leave the
    // explanation as it was relayed: it is evidence, not copy.
    if let Some(copy) = RUNTIME_ERRORS.iter().find(|copy| copy.code == code)
        && let Some(detail) = message
            .strip_prefix(copy.en)
            .and_then(|rest| rest.strip_prefix("\n\n"))
            .map(str::trim)
        && !detail.is_empty()
    {
        return format!("{}\n\n{detail}", copy.text(locale));
    }
    if let Some(copy) = RUNTIME_ERRORS
        .iter()
        .find(|copy| copy.code == code && (copy.en == message || message.trim().is_empty()))
    {
        return copy.text(locale).to_string();
    }
    // A generic adapter can re-tag a sentence that another code owns. The
    // sentence still says the same thing, so the catalogue is asked again
    // without the code before the English is shown.
    if let Some(copy) = RUNTIME_ERRORS.iter().find(|copy| copy.en == message) {
        return copy.text(locale).to_string();
    }
    message.to_string()
}

/// The translation of a runtime sentence when the caller only has the text.
///
/// The desktop keeps a message-oriented error table for surfaces that never
/// receive an error code; this lets that table reuse the same translations
/// instead of growing a second copy of them.
pub fn known_message(locale: Locale, message: &str) -> Option<&'static str> {
    RUNTIME_ERRORS
        .iter()
        .find(|copy| copy.en == message)
        .map(|copy| copy.text(locale))
}

fn formatted_error(locale: Locale, code: &str, message: &str) -> Option<String> {
    if locale == Locale::En {
        return None;
    }
    match code {
        "opencode_model_api_error" => {
            let detail = message.strip_prefix("OpenCode model request failed: ")?;
            Some(format!(
                "{}{detail}",
                locale.text(
                    "OpenCode model request failed: ",
                    "OpenCode 模型请求失败：",
                    "OpenCode 模型請求失敗：",
                )
            ))
        }
        "codex_http_status_error" => {
            let status = message.strip_prefix("Codex model request failed with HTTP ")?;
            Some(format!(
                "{}{status}{}",
                locale.text(
                    "Codex model request failed with HTTP ",
                    "Codex 模型请求失败（HTTP ",
                    "Codex 模型請求失敗（HTTP ",
                ),
                locale.text(")", "）", "）"),
            ))
        }
        "acp_turn_stopped_abnormally" => {
            let reason = message.strip_prefix("ACP turn stopped before a completed reply: ")?;
            Some(format!(
                "{}{reason}",
                locale.text(
                    "ACP turn stopped before a completed reply: ",
                    "ACP 回合在完成回复前停止：",
                    "ACP 回合在完成回覆前停止：",
                )
            ))
        }
        "codex_stream_reconnect_exhausted" => {
            let rest = message.strip_prefix("Codex model stream failed after ")?;
            let (attempts, detail) = rest.split_once(" reconnect attempts")?;
            let attempts: u32 = attempts.trim().parse().ok()?;
            let detail = detail.strip_prefix(": ").unwrap_or_default();
            let body = match locale {
                Locale::En => unreachable!(),
                Locale::ZhCn => format!("Codex 模型流在 {attempts} 次重连后失败"),
                Locale::ZhTw => format!("Codex 模型串流在 {attempts} 次重新連線後失敗"),
                other => crate::locale_copy::fill(
                    other,
                    "Codex model stream failed after {attempts} reconnect attempts",
                    &[("attempts", attempts.to_string())],
                ),
            };
            if detail.is_empty() {
                Some(body)
            } else {
                Some(format!("{body}{}{detail}", locale.text(": ", "：", "：")))
            }
        }
        "acp_retry_exhausted" => retry_exhausted(locale, message, "ACP"),
        "agent_retry_exhausted" => retry_exhausted(locale, message, "Agent"),
        "agent_workspace_missing" => workspace_missing(locale, message),
        _ => None,
    }
}

/// A session whose persisted workspace is gone.
///
/// The sentence carries the path the reader has to look at, so only the
/// sentence is translated: the path is evidence and stays exactly as recorded.
fn workspace_missing(locale: Locale, message: &str) -> Option<String> {
    for (prefix, translated) in [
        (
            "The Agent session workspace is missing: ",
            locale.text(
                "The Agent session workspace is missing: ",
                "Agent 会话的工作目录已不存在：",
                "Agent 工作階段的工作目錄已不存在：",
            ),
        ),
        (
            "The Agent session working directory is missing: ",
            locale.text(
                "The Agent session working directory is missing: ",
                "Agent 会话的工作子目录已不存在：",
                "Agent 工作階段的工作子目錄已不存在：",
            ),
        ),
    ] {
        if let Some(path) = message.strip_prefix(prefix) {
            return Some(format!("{translated}{path}"));
        }
    }
    None
}

fn retry_exhausted(locale: Locale, message: &str, subject: &str) -> Option<String> {
    let prefix = format!("{subject} retry attempts exhausted");
    let rest = message.strip_prefix(&prefix)?;
    let body = match locale {
        Locale::En => unreachable!(),
        Locale::ZhCn => format!("{subject} 重试次数已耗尽"),
        Locale::ZhTw => format!("{subject} 重試次數已耗盡"),
        other => crate::locale_copy::fill(
            other,
            "{subject} retry attempts exhausted",
            &[("subject", subject.to_string())],
        ),
    };
    if rest.is_empty() {
        return Some(body);
    }
    if let Some(attempts) = rest.strip_prefix(" after ") {
        if let Some((attempt, max_attempts)) = attempts.split_once('/')
            && attempt.bytes().all(|byte| byte.is_ascii_digit())
            && max_attempts.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Some(format!("{body}（{attempt}/{max_attempts}）"));
        }
        if let Some(attempt) = attempts.strip_prefix("attempt ")
            && attempt.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Some(match locale {
                Locale::En => unreachable!(),
                Locale::ZhCn => format!("{body}（已尝试 {attempt} 次）"),
                Locale::ZhTw => format!("{body}（已嘗試 {attempt} 次）"),
                other => {
                    let suffix = crate::locale_copy::fill(
                        other,
                        "(attempted {attempt} times)",
                        &[("attempt", attempt.to_string())],
                    );
                    format!("{body}{suffix}")
                }
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{known_message, timeline_error_message};
    use crate::locale::Locale;

    #[test]
    fn provider_access_errors_follow_the_reader() {
        let message =
            "The model provider rejected the request because account usage is unavailable";
        assert_eq!(
            timeline_error_message(Locale::ZhCn, "provider_access_unavailable", message),
            "模型供应商拒绝了请求，因为账户用量不可用"
        );
        assert_eq!(
            timeline_error_message(Locale::ZhTw, "provider_access_unavailable", message),
            "模型供應商拒絕了請求，因為帳戶用量無法使用"
        );
        assert_eq!(
            timeline_error_message(Locale::En, "provider_access_unavailable", message),
            message
        );
    }

    #[test]
    fn gateway_failures_follow_the_reader_and_keep_the_relayed_body() {
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "model_gateway_keys_cooling",
                "Every Key serving this model is cooling down after a failure; requests resume when one recovers",
            ),
            "所有为该模型服务的 Key 都在失败后冷却，待其中一个恢复后即可继续"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhTw,
                "model_gateway_upstream_failed",
                "The model gateway's upstream request failed\n\n{\"error\":{\"code\":\"model_gateway_upstream_failed\"}}",
            ),
            "模型網關的上游請求失敗\n\n{\"error\":{\"code\":\"model_gateway_upstream_failed\"}}"
        );
    }

    #[test]
    fn provider_prose_is_not_rewritten() {
        let provider = "429 rate limit exceeded for model gpt-5";
        assert_eq!(
            timeline_error_message(Locale::ZhCn, "codex_unattributed_agent_error", provider),
            provider
        );
        assert_eq!(
            timeline_error_message(Locale::ZhCn, "provider_access_unavailable", provider),
            provider
        );
    }

    #[test]
    fn adapter_prefixes_keep_the_provider_detail() {
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "opencode_model_api_error",
                "OpenCode model request failed: invalid api key",
            ),
            "OpenCode 模型请求失败：invalid api key"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "codex_http_status_error",
                "Codex model request failed with HTTP 401",
            ),
            "Codex 模型请求失败（HTTP 401）"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhTw,
                "acp_turn_stopped_abnormally",
                "ACP turn stopped before a completed reply: max_tokens",
            ),
            "ACP 回合在完成回覆前停止：max_tokens"
        );
    }

    #[test]
    fn retry_counts_and_reconnects_are_formatted() {
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "acp_retry_exhausted",
                "ACP retry attempts exhausted after 3/5",
            ),
            "ACP 重试次数已耗尽（3/5）"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "agent_retry_exhausted",
                "Agent retry attempts exhausted after attempt 2",
            ),
            "Agent 重试次数已耗尽（已尝试 2 次）"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "codex_stream_reconnect_exhausted",
                "Codex model stream failed after 4 reconnect attempts: connection reset",
            ),
            "Codex 模型流在 4 次重连后失败：connection reset"
        );
    }

    #[test]
    fn a_missing_workspace_keeps_its_path_in_every_language() {
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "agent_workspace_missing",
                "The Agent session workspace is missing: /tmp/vibex/sessions",
            ),
            "Agent 会话的工作目录已不存在：/tmp/vibex/sessions"
        );
        assert_eq!(
            timeline_error_message(
                Locale::ZhTw,
                "agent_workspace_missing",
                "The Agent session workspace is missing: /tmp/vibex/sessions",
            ),
            "Agent 工作階段的工作目錄已不存在：/tmp/vibex/sessions"
        );
        // English and any sentence this formatter does not own pass through.
        let message = "The Agent session workspace is missing: /home/reader/project";
        assert_eq!(
            timeline_error_message(Locale::En, "agent_workspace_missing", message),
            message
        );
    }

    #[test]
    fn a_renamed_code_can_still_match_the_sentence() {
        assert_eq!(
            timeline_error_message(
                Locale::ZhCn,
                "acp_rpc_error",
                "ACP agent process is no longer running",
            ),
            "ACP Agent 进程已不再运行"
        );
        assert_eq!(
            known_message(Locale::ZhTw, "Agent authentication is required"),
            Some("需要先完成 Agent 身分驗證")
        );
        assert_eq!(known_message(Locale::ZhCn, "not a runtime sentence"), None);
    }
}
