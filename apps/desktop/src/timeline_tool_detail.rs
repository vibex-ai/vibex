use std::path::Path;

use serde_json::Value;
use vibex_core::{AgentEventRawExtension, ToolCallPayload};

use super::{ProcessActivityIcon, locale, timeline_activity};

/// Presentation is derived from the bounded event text. Keep the original
/// invocation separately so copying structured arguments remains lossless.
pub(super) fn invocation(
    tool: &ToolCallPayload,
    kind: ProcessActivityIcon,
) -> Option<(String, String)> {
    let extension = tool.raw_extension.as_ref();
    let source = extension
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
        })?;
    Some((input_text(&source, kind), source))
}

fn input_text(source: &str, kind: ProcessActivityIcon) -> String {
    let Ok(value) = serde_json::from_str::<Value>(source) else {
        return source.to_string();
    };
    if let Value::String(text) = &value {
        return text.clone();
    }
    let keys: &[&str] = match kind {
        ProcessActivityIcon::Command => &["command", "cmd"],
        ProcessActivityIcon::FileRead
        | ProcessActivityIcon::FileEdit
        | ProcessActivityIcon::FileCreate
        | ProcessActivityIcon::FileDelete => &["file_path", "path", "filePath"],
        ProcessActivityIcon::Search => &["pattern", "query"],
        ProcessActivityIcon::Directory => &["pattern", "glob", "path"],
        _ => &[],
    };
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
pub(super) fn output(
    extension: Option<&AgentEventRawExtension>,
    summary: Option<&str>,
    known_exit_code: Option<i32>,
) -> Option<String> {
    let source = extension
        .and_then(|extension| extension.raw_output.as_ref())
        .map(|output| output.text.as_str())
        .or_else(|| summary.filter(|text| text.trim() != "terminal output"))?;
    let text = output_text(source, known_exit_code);
    (!text.trim().is_empty()).then_some(text)
}

fn output_text(source: &str, known_exit_code: Option<i32>) -> String {
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
                    &locale::text("Exit code: {code}", "退出码：{code}", "結束代碼：{code}")
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
            format!("{}\n{text}", locale::text("Error", "错误", "錯誤"))
        } else {
            text
        };
    }
    pretty(&value)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

pub(super) fn command_cwd<'a>(
    cwd: Option<&'a str>,
    workspace_root: Option<&str>,
) -> Option<&'a str> {
    cwd.filter(|cwd| {
        !cwd.trim().is_empty()
            && workspace_root.is_none_or(|root| *cwd != "." && Path::new(cwd) != Path::new(root))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentEventRawOutput, AgentEventRawOutputMode};

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
        assert!(error.starts_with(locale::text("Error", "错误", "錯誤")));
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
