//! Readable, lossless detail projections for typed timeline activities.

use std::path::Path;

use serde_json::Value;
use vibex_core::{AgentEventRawExtension, TimelinePayload, ToolCallPayload};
use vibex_desktop_model::TimelineRow;

use crate::{
    locale::Locale,
    timeline::{self as timeline_activity, ActivityKind as ProcessActivityIcon},
};

/// Presentation is derived from the bounded event text. Keep the original
/// invocation separately so copying structured arguments remains lossless.
pub fn invocation(tool: &ToolCallPayload, kind: ProcessActivityIcon) -> Option<(String, String)> {
    let source = invocation_source(tool, kind)?;
    Some((input_text(&source, kind), source))
}

fn invocation_source(tool: &ToolCallPayload, kind: ProcessActivityIcon) -> Option<String> {
    let extension = tool.raw_extension.as_ref();
    extension
        .and_then(|extension| extension.raw_input.as_deref())
        .filter(|input| !input.trim().is_empty())
        .or(tool
            .input_summary
            .as_deref()
            .filter(|input| !input.trim().is_empty()))
        .map(str::to_string)
        .or_else(|| {
            if !matches!(
                kind,
                ProcessActivityIcon::Command
                    | ProcessActivityIcon::Search
                    | ProcessActivityIcon::Directory
                    | ProcessActivityIcon::FileRead
                    | ProcessActivityIcon::FileEdit
                    | ProcessActivityIcon::FileCreate
                    | ProcessActivityIcon::FileDelete
            ) {
                return None;
            }
            if matches!(
                kind,
                ProcessActivityIcon::FileRead
                    | ProcessActivityIcon::FileEdit
                    | ProcessActivityIcon::FileCreate
                    | ProcessActivityIcon::FileDelete
            ) && let Some(extension) =
                extension.filter(|extension| !extension.locations.is_empty())
            {
                return Some(
                    extension
                        .locations
                        .iter()
                        .map(|location| location.uri.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            let target = timeline_activity::activity_target(&tool.summary, kind);
            (!target.is_empty()).then(|| target.to_string())
        })
}

fn input_text(source: &str, kind: ProcessActivityIcon) -> String {
    let Ok(value) = serde_json::from_str::<Value>(source) else {
        return source.to_string();
    };
    if let Value::String(text) = &value {
        return text.clone();
    }
    let keys = timeline_activity::input_keys(kind);
    if let Some(object) = value.as_object()
        && let Some((primary, text)) = keys.iter().find_map(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| (*key, text))
        })
    {
        let mut lines = vec![text.to_string()];
        for (key, value) in object.iter().filter(|(key, _)| key.as_str() != primary) {
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| pretty(value));
            let separator = if text.contains('\n') { "\n" } else { " " };
            lines.push(format!("{key}:{separator}{text}"));
        }
        return lines.join("\n");
    }
    pretty(&value)
}

/// Prefer captured output to summaries, which may only describe a terminal
/// attachment. An explicitly empty result must not revive an older summary.
pub fn output(
    extension: Option<&AgentEventRawExtension>,
    summary: Option<&str>,
    known_exit_code: Option<i32>,
    locale: Locale,
) -> Option<String> {
    let source = extension
        .and_then(|extension| extension.raw_output.as_ref())
        .map(|output| output.text.as_str())
        .or_else(|| summary.filter(|text| text.trim() != "terminal output"))?;
    let text = output_text(source, known_exit_code, locale);
    (!text.trim().is_empty()).then_some(text)
}

fn output_text(source: &str, known_exit_code: Option<i32>, locale: Locale) -> String {
    let Ok(value) = serde_json::from_str::<Value>(source) else {
        return source.to_string();
    };
    let Some(object) = value.as_object() else {
        return pretty(&value);
    };
    // Only unwrap recognized result envelopes. Application JSON with additional
    // fields remains intact instead of silently losing data named "output".
    let output_keys = ["formatted_output", "stdout", "stderr"];
    if object.keys().all(|key| {
        output_keys.contains(&key.as_str()) || matches!(key.as_str(), "exit_code" | "exitCode")
    }) && output_keys.iter().any(|key| object.contains_key(*key))
        && output_keys.iter().all(|key| {
            object
                .get(*key)
                .is_none_or(|value| value.is_string() || value.is_null())
        })
    {
        let code = object.get("exit_code").or_else(|| object.get("exitCode"));
        let conflicting_codes = object
            .get("exit_code")
            .zip(object.get("exitCode"))
            .is_some_and(|(left, right)| left != right);
        if !conflicting_codes && code.is_none_or(|code| code.is_null() || code.as_i64().is_some()) {
            let mut text = String::new();
            for part in output_keys
                .iter()
                .filter_map(|key| object.get(*key).and_then(Value::as_str))
                .filter(|text| !text.is_empty())
            {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(part);
            }
            if let Some(code) = code.and_then(Value::as_i64)
                && code != 0
                && Some(code) != known_exit_code.map(i64::from)
            {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(
                    &locale
                        .text("Exit code: {code}", "退出码：{code}", "結束代碼：{code}")
                        .replace("{code}", &code.to_string()),
                );
            }
            return text;
        }
    }
    if object
        .keys()
        .all(|key| matches!(key.as_str(), "content" | "isError"))
        && object.get("isError").is_none_or(Value::is_boolean)
        && let Some(content) = object.get("content").and_then(Value::as_array)
        && content.iter().all(|block| {
            block.as_object().is_some_and(|block| {
                block
                    .keys()
                    .all(|key| matches!(key.as_str(), "type" | "text"))
                    && block.get("type").and_then(Value::as_str) == Some("text")
                    && block.get("text").is_some_and(Value::is_string)
            })
        })
    {
        let text = content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        return if object.get("isError").and_then(Value::as_bool) == Some(true) {
            format!("{}\n{text}", locale.text("Error", "错误", "錯誤"))
        } else {
            text
        };
    }
    pretty(&value)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

pub fn command_cwd<'a>(cwd: Option<&'a str>, workspace_root: Option<&str>) -> Option<&'a str> {
    cwd.filter(|cwd| {
        !cwd.trim().is_empty()
            && workspace_root.is_none_or(|root| *cwd != "." && Path::new(cwd) != Path::new(root))
    })
}

/// Bounded source retained until an expanded detail is actually rendered.
/// Keeping formatting lazy lets terminal block caches avoid reparsing hidden
/// tool output on every streamed token.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Detail {
    label: &'static str,
    source: String,
    format: DetailFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DetailFormat {
    Plain,
    FileContent,
    Input(ProcessActivityIcon),
    Output(Option<i32>, Locale),
    Metadata,
}

impl Detail {
    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn text(&self) -> String {
        match self.format {
            DetailFormat::Input(kind) => input_text(&self.source, kind),
            DetailFormat::Output(code, locale) => output_text(&self.source, code, locale),
            DetailFormat::Plain | DetailFormat::Metadata | DetailFormat::FileContent => {
                self.source.clone()
            }
        }
    }

    pub fn copy_text(&self) -> String {
        match self.format {
            DetailFormat::Input(_) => self.source.clone(),
            _ => self.text(),
        }
    }

    pub fn is_metadata(&self) -> bool {
        self.format == DetailFormat::Metadata
    }

    pub fn is_file_content(&self) -> bool {
        self.format == DetailFormat::FileContent
    }
}

/// Project typed activity fields without losing original arguments or captured
/// output. Labels name copy actions; the reading surface need not repeat them.
pub fn project(
    row: &TimelineRow,
    payload: Option<&TimelinePayload>,
    workspace_root: Option<&str>,
    locale: Locale,
) -> Vec<Detail> {
    let mut details = Vec::new();
    let input = locale.text("Input", "输入", "輸入");
    let output_label = locale.text("Output", "输出", "輸出");
    let mut push = |label, source: String, format| {
        if !source.trim().is_empty() {
            details.push(Detail {
                label,
                source,
                format,
            });
        }
    };
    let output_source = |extension: Option<&AgentEventRawExtension>, summary: Option<&str>| {
        extension
            .and_then(|extension| extension.raw_output.as_ref())
            .map(|output| output.text.clone())
            .or_else(|| {
                summary
                    .filter(|text| text.trim() != "terminal output")
                    .map(str::to_string)
            })
    };
    match payload {
        Some(TimelinePayload::ToolCall(tool)) => {
            let kind = timeline_activity::generic_activity_kind(&tool.tool_name, &tool.summary);
            if let Some(source) = invocation_source(tool, kind) {
                push(input, source, DetailFormat::Input(kind));
            } else {
                // Identity remains inspectable even when an output envelope
                // decodes to an empty snapshot. Do not parse hidden output to
                // decide whether this fallback is needed.
                let mut source = tool.tool_name.clone();
                if let Some(extension) = &tool.raw_extension {
                    for (key, value) in &extension.meta {
                        source.push_str(&format!("\n{key}: {value}"));
                    }
                }
                push(
                    locale.text("Tool", "工具", "工具"),
                    source,
                    DetailFormat::Plain,
                );
            }
            if let Some(source) =
                output_source(tool.raw_extension.as_ref(), tool.output_summary.as_deref())
            {
                push(output_label, source, DetailFormat::Output(None, locale));
            }
        }
        Some(TimelinePayload::Command(command)) => {
            if let Some(cwd) = command_cwd(command.cwd.as_deref(), workspace_root) {
                push(
                    locale.text("Working directory", "工作目录", "工作目錄"),
                    cwd.to_string(),
                    DetailFormat::Metadata,
                );
            }
            push(
                locale.text("Command", "命令", "命令"),
                command.command.clone(),
                DetailFormat::Plain,
            );
            if let Some(source) = output_source(
                command.raw_extension.as_ref(),
                command.output_summary.as_deref(),
            ) {
                push(
                    output_label,
                    source,
                    DetailFormat::Output(command.exit_code, locale),
                );
            }
            if let Some(code) = command.exit_code.filter(|code| *code != 0) {
                push(
                    locale.text("Exit code", "退出码", "結束代碼"),
                    code.to_string(),
                    DetailFormat::Metadata,
                );
            }
        }
        Some(TimelinePayload::FileOperation(file)) => {
            push(
                locale.text("File", "文件", "檔案"),
                file.path.clone(),
                DetailFormat::Plain,
            );
            if let Some(source) = output_source(file.raw_extension.as_ref(), Some(&file.summary))
                && source.trim() != file.path
            {
                push(output_label, source, DetailFormat::Output(None, locale));
            }
            if let Some(patch) = &file.patch {
                push(
                    locale.text("Changes", "变更", "變更"),
                    patch.text.clone(),
                    DetailFormat::FileContent,
                );
            } else if let Some(text) = file.new_text.as_ref().or(file.old_text.as_ref()) {
                push(
                    locale.text("Contents", "内容", "內容"),
                    text.clone(),
                    DetailFormat::FileContent,
                );
            }
        }
        Some(TimelinePayload::WebSearch(search)) => {
            push(
                locale.text("Query", "查询", "查詢"),
                search.query.clone(),
                DetailFormat::Plain,
            );
            if let Some(source) = output_source(
                search.raw_extension.as_ref(),
                search.result_summary.as_deref(),
            ) {
                push(output_label, source, DetailFormat::Output(None, locale));
            }
        }
        _ => push(
            locale.text("Details", "详情", "詳細資料"),
            row.body.clone(),
            DetailFormat::Plain,
        ),
    }
    details
}

/// A terminal or text export keeps context metadata while omitting redundant
/// Input/Output headings. Empty captured snapshots contribute no stale text.
pub fn text(details: &[Detail]) -> String {
    details
        .iter()
        .filter_map(|detail| {
            let value = detail.text();
            if value.trim().is_empty() {
                return None;
            }
            Some(if detail.is_metadata() {
                format!("{}: {value}", detail.label())
            } else {
                value
            })
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentEventRawOutput, AgentEventRawOutputMode};

    fn output(
        extension: Option<&AgentEventRawExtension>,
        summary: Option<&str>,
        code: Option<i32>,
    ) -> Option<String> {
        super::output(extension, summary, code, Locale::En)
    }

    fn output_text(source: &str, code: Option<i32>) -> String {
        super::output_text(source, code, Locale::En)
    }

    fn captured(text: &str) -> AgentEventRawExtension {
        AgentEventRawExtension::new(
            Vec::new(),
            None,
            Some(AgentEventRawOutput::new(AgentEventRawOutputMode::Snapshot, text).0),
            Vec::new(),
            Default::default(),
            false,
        )
    }

    fn row_for(payload: &TimelinePayload) -> TimelineRow {
        use vibex_core::{
            TimelineItem, TimelineItemId, TimelineRedactionState, TimelineSource, VibexSessionId,
        };
        vibex_desktop_model::timeline_rows(&[TimelineItem {
            id: TimelineItemId::new(),
            session_id: VibexSessionId::new(),
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload: payload.clone(),
        }])
        .remove(0)
    }

    #[test]
    fn generic_tool_titles_expose_the_argument_and_preserve_the_original_input() {
        for (name, input, target, path) in [
            (
                "execute",
                r#"{"command":"cargo check\n  --locked","timeout":1000}"#,
                "cargo check --locked",
                None,
            ),
            (
                "read_file",
                r#"{"path":"src/nested/main.rs","limit":40}"#,
                "main.rs",
                Some("src/nested/main.rs"),
            ),
        ] {
            let payload = TimelinePayload::ToolCall(ToolCallPayload {
                tool_call_id: "call".into(),
                tool_name: name.into(),
                status: vibex_core::ToolCallStatus::Completed,
                summary: name.into(),
                input_summary: Some(input.into()),
                output_summary: None,
                raw_extension: None,
            });
            let row = row_for(&payload);
            let activity = timeline_activity::Activity::project(&row, Some(&payload), Locale::En);
            assert_eq!(activity.target(), target);
            assert_eq!(activity.path(), path);
            let details = project(&row, Some(&payload), None, Locale::En);
            assert_eq!(details[0].copy_text(), input);
            assert!(!text(&details).contains(r#""command""#));
        }
    }

    #[test]
    fn an_empty_result_envelope_still_discloses_the_tool_identity() {
        for output in ["", r#"{"formatted_output":"","exit_code":0}"#] {
            let payload = TimelinePayload::ToolCall(ToolCallPayload {
                tool_call_id: "call".into(),
                tool_name: "custom_tool".into(),
                status: vibex_core::ToolCallStatus::Completed,
                summary: "Finished".into(),
                input_summary: None,
                output_summary: Some("stale summary".into()),
                raw_extension: Some(captured(output)),
            });
            let row = row_for(&payload);
            assert_eq!(
                text(&project(&row, Some(&payload), None, Locale::En)),
                "custom_tool"
            );
        }
    }

    #[test]
    fn captured_results_replace_attachment_summaries_and_restore_line_breaks() {
        let text = "10:fn main() {\n11:    println!(\"hello\");\n12:}\n";
        let raw = serde_json::json!({"formatted_output": text, "exit_code": 0}).to_string();
        assert_eq!(
            output(Some(&captured(&raw)), Some("terminal output"), None),
            Some(text.into())
        );
        assert_eq!(output(None, Some("terminal output"), None), None);
        assert_eq!(
            output(Some(&captured("terminal output")), None, None),
            Some("terminal output".into())
        );
    }

    #[test]
    fn empty_snapshots_do_not_restore_stale_output() {
        for source in ["", "\n", r#"{"formatted_output":"","exit_code":0}"#] {
            assert_eq!(
                output(Some(&captured(source)), Some("old result"), None),
                None
            );
        }
    }

    #[test]
    fn failed_results_keep_stderr_and_exit_status() {
        let raw = r#"{"stdout":"checking\n","stderr":"  permission denied\n","exit_code":2}"#;
        let result = output_text(raw, None);
        assert!(result.starts_with("checking\n  permission denied\n"));
        assert!(result.ends_with('2'));
        assert_eq!(output_text(raw, Some(2)), "checking\n  permission denied\n");
        assert!(
            output_text(raw, Some(0)).ends_with('2'),
            "a conflicting success code cannot hide a failure"
        );
    }

    #[test]
    fn text_content_is_readable_without_losing_non_text_blocks() {
        assert_eq!(
            output_text(
                r#"{"content":[{"type":"text","text":"one\n  two"}],"isError":false}"#,
                None
            ),
            "one\n  two"
        );
        let error = output_text(
            r#"{"content":[{"type":"text","text":"  not found"}],"isError":true}"#,
            None,
        );
        assert!(error.ends_with("\n  not found"));
        assert!(error.starts_with(Locale::En.text("Error", "错误", "錯誤")));
        let source = serde_json::json!({"content": [{"type": "image", "mimeType": "image/png"}]});
        assert_eq!(
            serde_json::from_str::<Value>(&output_text(&source.to_string(), None)).unwrap(),
            source
        );
    }

    #[test]
    fn unfamiliar_json_and_partial_results_preserve_all_data() {
        for source in [
            serde_json::json!({"formatted_output": "value", "file": "src/main.rs", "exit_code": 0}),
            serde_json::json!({"output": "value", "has_more": true}),
            serde_json::json!({"formatted_output": "value", "exit_code": "unknown"}),
            serde_json::json!({"formatted_output": "value", "exit_code": 0, "exitCode": 2}),
        ] {
            assert_eq!(
                serde_json::from_str::<Value>(&output_text(&source.to_string(), None)).unwrap(),
                source
            );
        }
        for text in [
            r#"{"formatted_output":"partial\nresult"#,
            "    source code\n\n",
            r#"printf '%s\n' value"#,
        ] {
            assert_eq!(output_text(text, None), text);
        }
    }

    #[test]
    fn compact_arguments_keep_multiline_commands_and_every_extra_field() {
        let command = "printf '%s\\n' value\n  cargo check --locked";
        let input = serde_json::json!({"command": command, "timeout": 1000});
        let text = input_text(&input.to_string(), ProcessActivityIcon::Command);
        assert!(text.starts_with(command));
        assert!(text.contains("timeout: 1000"));
        let unknown = serde_json::json!({"options": {"recursive": true}, "path": "src"});
        assert_eq!(
            serde_json::from_str::<Value>(&input_text(
                &unknown.to_string(),
                ProcessActivityIcon::Integration
            ))
            .unwrap(),
            unknown
        );
    }

    #[test]
    fn command_context_only_omits_the_current_workspace() {
        assert_eq!(command_cwd(Some("/workspace/"), Some("/workspace")), None);
        assert_eq!(command_cwd(Some("."), Some("/workspace")), None);
        assert_eq!(
            command_cwd(Some("/workspace/tools"), Some("/workspace")),
            Some("/workspace/tools")
        );
        assert_eq!(command_cwd(Some("/workspace"), None), Some("/workspace"));
    }
}
