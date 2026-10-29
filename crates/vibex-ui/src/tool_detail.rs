//! Readable, lossless detail projections for typed timeline activities.

use std::path::Path;

use serde_json::Value;
use vibex_core::{
    AgentEventRawExtension, FileOperationKind, FileOperationPayload, TimelinePayload,
    ToolCallPayload,
};
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
    let Some(object) = value.as_object() else {
        return fields_text(&fields(&value));
    };
    if let Some(todos) = todo_items_in(object) {
        return todos_text(&todos);
    }
    if let Some(questions) = questions_in(object) {
        return questions;
    }
    let keys = timeline_activity::input_keys(kind);
    let primary = keys.iter().find_map(|key| {
        let text = primary_text(object.get(*key)?)?;
        Some((*key, text))
    });
    let mut lines = Vec::new();
    if let Some((_, text)) = &primary {
        lines.push(text.clone());
    }
    let rest = object
        .iter()
        .filter(|(key, _)| primary.as_ref().is_none_or(|(primary, _)| key != primary))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let rest = fields_text(&fields(&Value::Object(rest)));
    if !rest.is_empty() {
        lines.push(rest);
    }
    lines.join("\n")
}

/// The argument a header leads with: a string, or a list of strings such as
/// the `commands` a batch runner takes.
fn primary_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Array(items) if !items.is_empty() => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .map(|items| items.join("\n")),
        _ => None,
    }
}

/// How many leaves of a nested argument are listed before the rest is counted.
const MAX_FIELDS: usize = 48;

/// A JSON argument as readable `path: value` pairs.
///
/// Agents name the same arguments differently and nest them freely, so the
/// reading surface never shows the JSON itself: objects become dotted paths,
/// lists of plain values a comma-separated line, and empty values are left
/// out. The exact invocation stays available through the copy action.
fn fields(value: &Value) -> Vec<(String, String)> {
    fn walk(path: String, value: &Value, out: &mut Vec<(String, String)>, omitted: &mut usize) {
        if out.len() >= MAX_FIELDS {
            *omitted += 1;
            return;
        }
        match value {
            Value::Null => {}
            Value::String(text) if text.trim().is_empty() => {}
            Value::String(text) => out.push((path, text.clone())),
            Value::Bool(_) | Value::Number(_) => out.push((path, value.to_string())),
            Value::Array(items) if items.is_empty() => {}
            Value::Array(items)
                if items
                    .iter()
                    .all(|item| !item.is_object() && !item.is_array()) =>
            {
                let line = items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map_or_else(|| item.to_string(), str::to_string)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push((path, line));
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(format!("{path}[{}]", index + 1), item, out, omitted);
                }
            }
            Value::Object(object) => {
                for (key, item) in object {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(path, item, out, omitted);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut omitted = 0;
    walk(String::new(), value, &mut out, &mut omitted);
    if omitted > 0 {
        out.push(("…".to_string(), format!("+{omitted}")));
    }
    out
}

fn fields_text(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            if key.is_empty() {
                value.clone()
            } else if value.contains('\n') {
                format!("{key}:\n{value}")
            } else {
                format!("{key}: {value}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Status of one entry in a todo list an agent wrote through a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TodoStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl TodoStatus {
    /// The status key the timelines' checklist rows draw their glyph from.
    pub fn key(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn mark(self) -> &'static str {
        match self {
            Self::Pending => "○",
            Self::Running => "◐",
            Self::Completed => "✓",
            Self::Failed => "✕",
        }
    }
}

/// The checklist a todo or plan tool call carries.
///
/// Claude's `TodoWrite`, OpenCode's `todowrite` and the harness `todos`
/// argument use `content`; Codex's `update_plan` uses `plan` entries with a
/// `step`. The list is drawn as a checklist instead of its JSON.
pub fn todo_items(tool: &ToolCallPayload) -> Option<Vec<(TodoStatus, String)>> {
    let value = serde_json::from_str::<Value>(input_source(tool)?).ok()?;
    todo_items_in(value.as_object()?)
}

fn todo_items_in(object: &serde_json::Map<String, Value>) -> Option<Vec<(TodoStatus, String)>> {
    let items = ["todos", "plan", "items", "steps"]
        .iter()
        .find_map(|key| object.get(*key)?.as_array())?;
    if items.is_empty() {
        return None;
    }
    items
        .iter()
        .map(|item| {
            let text = ["content", "step", "title", "text", "description"]
                .iter()
                .find_map(|key| item.get(*key)?.as_str())?;
            let status = match item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_ascii_lowercase()
                .replace(['-', ' '], "_")
                .as_str()
            {
                "completed" | "complete" | "done" => TodoStatus::Completed,
                "in_progress" | "inprogress" | "running" | "active" => TodoStatus::Running,
                "failed" | "error" | "cancelled" | "canceled" => TodoStatus::Failed,
                _ => TodoStatus::Pending,
            };
            Some((status, text.to_string()))
        })
        .collect()
}

fn todos_text(items: &[(TodoStatus, String)]) -> String {
    items
        .iter()
        .map(|(status, text)| format!("{} {text}", status.mark()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The questions an ask-the-user tool call put to the reader, as text.
fn questions_in(object: &serde_json::Map<String, Value>) -> Option<String> {
    let questions = object.get("questions")?.as_array()?;
    let mut lines = Vec::new();
    for question in questions {
        let text = question.get("question").and_then(Value::as_str);
        let header = question.get("header").and_then(Value::as_str);
        match (header, text) {
            (Some(header), Some(text)) => lines.push(format!("{header} — {text}")),
            (Some(text), None) | (None, Some(text)) => lines.push(text.to_string()),
            (None, None) => return None,
        }
        for option in question
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| option.as_str());
            let Some(label) = label else { continue };
            match option.get("description").and_then(Value::as_str) {
                Some(description) if !description.is_empty() => {
                    lines.push(format!("  · {label}: {description}"))
                }
                _ => lines.push(format!("  · {label}")),
            }
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn input_source(tool: &ToolCallPayload) -> Option<&str> {
    tool.raw_extension
        .as_ref()
        .and_then(|extension| extension.raw_input.as_deref())
        .filter(|input| !input.trim().is_empty())
        .or(tool
            .input_summary
            .as_deref()
            .filter(|input| !input.trim().is_empty()))
}

/// The file change a generic tool call describes in its arguments.
///
/// Most ACP agents report an edit as a plain tool call whose arguments carry
/// the replaced and replacing text — `old_string`/`new_string` for Claude and
/// the harness, `oldText`/`newText` or `old_str`/`new_str` elsewhere, an
/// `edits` list for multi-edits — and a write as the new `content`. Reading
/// those back as a file operation is what lets the timeline draw the change
/// as a diff with line counts rather than as two quoted strings.
pub fn file_operation(tool: &ToolCallPayload) -> Option<FileOperationPayload> {
    let kind = timeline_activity::generic_activity_kind(&tool.tool_name, &tool.summary);
    if !matches!(
        kind,
        ProcessActivityIcon::FileEdit | ProcessActivityIcon::FileCreate
    ) {
        return None;
    }
    let value = serde_json::from_str::<Value>(input_source(tool)?).ok()?;
    let object = value.as_object()?;
    let path = ["file_path", "path", "filePath", "target_file", "file"]
        .iter()
        .find_map(|key| object.get(*key)?.as_str())
        .map(str::to_string)
        .or_else(|| {
            tool.raw_extension
                .as_ref()?
                .locations
                .first()
                .map(|location| location.uri.clone())
        })?;
    const PAIRS: [(&str, &str); 5] = [
        ("old_string", "new_string"),
        ("oldText", "newText"),
        ("old_str", "new_str"),
        ("old_text", "new_text"),
        ("oldString", "newString"),
    ];
    let pair = |object: &serde_json::Map<String, Value>| {
        PAIRS.iter().find_map(|(old, new)| {
            let new = object.get(*new)?.as_str()?;
            let old = object.get(*old).and_then(Value::as_str).unwrap_or_default();
            Some((old.to_string(), new.to_string()))
        })
    };
    let (operation, old_text, new_text) = if let Some((old, new)) = pair(object) {
        (FileOperationKind::Edit, Some(old), new)
    } else if let Some(edits) = object.get("edits").and_then(Value::as_array) {
        let edits = edits
            .iter()
            .map(|edit| pair(edit.as_object()?))
            .collect::<Option<Vec<_>>>()
            .filter(|edits| !edits.is_empty())?;
        let (old, new): (Vec<_>, Vec<_>) = edits.into_iter().unzip();
        (
            FileOperationKind::Edit,
            Some(old.join("\n")),
            new.join("\n"),
        )
    } else {
        let content = ["content", "contents", "file_text", "text"]
            .iter()
            .find_map(|key| object.get(*key)?.as_str())?;
        (FileOperationKind::Write, None, content.to_string())
    };
    Some(FileOperationPayload {
        operation,
        path,
        summary: tool.summary.clone(),
        old_text,
        new_text: Some(new_text),
        patch: None,
        raw_extension: tool.raw_extension.clone(),
    })
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
        return tagged_file_content(source).unwrap_or(source).to_string();
    };
    let Some(object) = value.as_object() else {
        return fields_text(&fields(&value));
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
    // The harness envelope: one `output` string and an error flag.
    if object
        .keys()
        .all(|key| matches!(key.as_str(), "output" | "isError" | "is_error"))
        && let Some(text) = object.get("output").and_then(Value::as_str)
    {
        let failed = object
            .get("isError")
            .or_else(|| object.get("is_error"))
            .and_then(Value::as_bool)
            == Some(true);
        let text = tagged_file_content(text).unwrap_or(text);
        return if failed {
            format!("{}\n{text}", locale.text("Error", "错误", "錯誤"))
        } else {
            text.to_string()
        };
    }
    // The answers an ask-the-user tool returned.
    if object.len() == 1
        && let Some(answers) = object.get("answers").and_then(Value::as_array)
    {
        let lines = answers
            .iter()
            .filter_map(|answer| {
                let mut picked = answer
                    .get("selected")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if let Some(custom) = answer
                    .get("custom")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    picked.push(custom.to_string());
                }
                (!picked.is_empty()).then(|| format!("→ {}", picked.join(", ")))
            })
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            return lines.join("\n");
        }
    }
    fields_text(&fields(&value))
}

/// The body of a `<path>…</path><type>…</type><content>…</content>` read
/// result, which is how the harness reports a file it read.
fn tagged_file_content(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<path>") {
        return None;
    }
    let start = trimmed.find("<content>")? + "<content>".len();
    let body = &trimmed[start..];
    let body = body
        .strip_suffix("</content>")
        .or_else(|| body.trim_end().strip_suffix("</content>"))?;
    Some(body.strip_prefix('\n').unwrap_or(body))
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
        Some(TimelinePayload::ToolCall(tool)) if let Some(file) = file_operation(tool) => {
            push(
                locale.text("File", "文件", "檔案"),
                file.path.clone(),
                DetailFormat::Plain,
            );
            push(
                locale.text("Changes", "变更", "變更"),
                file.canonical_patch()
                    .map(|patch| patch.text)
                    .unwrap_or_default(),
                DetailFormat::FileContent,
            );
            if let Some(source) =
                output_source(tool.raw_extension.as_ref(), tool.output_summary.as_deref())
            {
                push(output_label, source, DetailFormat::Output(None, locale));
            }
        }
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
            output_text(&source.to_string(), None),
            "content[1].type: image\ncontent[1].mimeType: image/png"
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
            // Read as fields rather than JSON, but every value is still there.
            let text = output_text(&source.to_string(), None);
            assert!(!text.contains('{'), "{text}");
            for (key, value) in source.as_object().unwrap() {
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string);
                assert!(text.contains(&format!("{key}: {value}")), "{text}");
            }
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
            input_text(&unknown.to_string(), ProcessActivityIcon::Integration),
            "options.recursive: true\npath: src"
        );
    }

    fn tool(name: &str, summary: &str, input: Value) -> ToolCallPayload {
        ToolCallPayload {
            tool_call_id: "call".into(),
            tool_name: name.into(),
            status: vibex_core::ToolCallStatus::Completed,
            summary: summary.into(),
            input_summary: Some(input.to_string()),
            output_summary: None,
            raw_extension: None,
        }
    }

    #[test]
    fn edits_and_writes_in_tool_arguments_become_file_changes() {
        let edit = file_operation(&tool(
            "edit",
            "Edit src/lib.rs",
            serde_json::json!({"file_path": "src/lib.rs", "old_string": "a", "new_string": "b"}),
        ))
        .unwrap();
        assert_eq!(edit.operation, FileOperationKind::Edit);
        assert_eq!(edit.path, "src/lib.rs");
        assert_eq!(edit.old_text.as_deref(), Some("a"));
        assert_eq!(edit.new_text.as_deref(), Some("b"));

        let multi = file_operation(&tool(
            "MultiEdit",
            "",
            serde_json::json!({"path": "a.rs", "edits": [
                {"oldText": "1", "newText": "2"}, {"oldText": "3", "newText": "4"}
            ]}),
        ))
        .unwrap();
        assert_eq!(multi.new_text.as_deref(), Some("2\n4"));

        let write = file_operation(&tool(
            "Write",
            "",
            serde_json::json!({"file_path": "new.rs", "content": "fn main() {}"}),
        ))
        .unwrap();
        assert_eq!(write.operation, FileOperationKind::Write);
        assert_eq!(write.old_text, None);

        // A message with `content` is not a file write.
        assert!(
            file_operation(&tool(
                "SendMessage",
                "",
                serde_json::json!({"to": "a", "content": "hi"})
            ))
            .is_none()
        );
    }

    #[test]
    fn todo_lists_and_questions_read_as_lists() {
        let todos = tool(
            "think",
            "Update plan",
            serde_json::json!({"todos": [
                {"content": "Map callers", "status": "completed"},
                {"content": "Gate the code", "status": "in_progress"},
                {"content": "Verify", "status": "pending"}
            ]}),
        );
        assert_eq!(
            todo_items(&todos).unwrap(),
            vec![
                (TodoStatus::Completed, "Map callers".to_string()),
                (TodoStatus::Running, "Gate the code".to_string()),
                (TodoStatus::Pending, "Verify".to_string()),
            ]
        );
        let text = input_text(
            todos.input_summary.as_deref().unwrap(),
            ProcessActivityIcon::Todo,
        );
        assert_eq!(text, "✓ Map callers\n◐ Gate the code\n○ Verify");

        let questions = serde_json::json!({"questions": [{
            "header": "Scope", "question": "Which host?",
            "options": [{"label": "WSL", "description": "Windows"}, {"label": "Mac"}]
        }]});
        assert_eq!(
            input_text(&questions.to_string(), ProcessActivityIcon::Generic),
            "Scope — Which host?\n  · WSL: Windows\n  · Mac"
        );
        assert_eq!(
            output_text(
                r#"{"answers":[{"id":"q","selected":["WSL"],"custom":"later"}]}"#,
                None
            ),
            "→ WSL, later"
        );
    }

    #[test]
    fn titles_that_only_name_the_tool_defer_to_the_arguments() {
        let target = |tool: ToolCallPayload| {
            let payload = TimelinePayload::ToolCall(tool);
            timeline_activity::Activity::project(&row_for(&payload), Some(&payload), Locale::En)
                .target()
                .to_string()
        };
        assert_eq!(
            target(tool(
                "other",
                "Bash",
                serde_json::json!({"command": "ls -la"})
            )),
            "ls -la"
        );
        assert_eq!(
            target(tool(
                "execute",
                "run_commands",
                serde_json::json!({"commands": ["cargo fmt", "cargo test"]})
            )),
            "cargo fmt (+1)"
        );
        let mut opaque = tool("read", "ACP tool call call_5d276d", serde_json::json!({}));
        opaque.input_summary = None;
        assert_eq!(target(opaque), "read");
    }

    #[test]
    fn harness_envelopes_unwrap_to_their_text() {
        assert_eq!(
            output_text(
                r#"{"output":"<path>/a.rs</path>\n<type>file</type>\n<content>\n1: fn a() {}\n</content>","isError":false}"#,
                None
            ),
            "1: fn a() {}\n"
        );
        assert_eq!(
            output_text(r#"{"output":"updated","isError":false}"#, None),
            "updated"
        );
        assert!(output_text(r#"{"output":"boom","isError":true}"#, None).ends_with("\nboom"));
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
