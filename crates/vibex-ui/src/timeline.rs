//! Desktop timeline semantics shared by GUI and terminal clients.
//!
//! These projections own labels, targets and disclosure defaults, never the
//! renderer or authoritative session state. Full paths remain identities even
//! when a compact surface displays only a basename.

use crate::locale::Locale;
use std::collections::BTreeSet;
use vibex_core::{
    FileOperationKind, PlanStepStatus, TimelineItem, TimelinePayload, ToolCallPayload,
};
use vibex_desktop_model::{
    TimelineConversationTurn, TimelineProcessActivityGroup, TimelineRow, TimelineRowKind,
};

/// The common height of a live reasoning window, in text rows.
pub const REASONING_WINDOW_LINES: usize = 6;

/// How far the start of a live window's source moves at a time.
///
/// The window shows six rows, so the text view only needs the end of the
/// thought; this bounds what it lays out to between one and two quanta.
pub const REASONING_WINDOW_TAIL_QUANTUM: usize = 2 * 1024;

/// The end of a live thought that is enough to fill its window.
///
/// The window clips the body to its newest rows, but a clip is not a budget:
/// handed the whole thought, the text view lays out and paints every row of it
/// on each delta, and a long thought costs frames in
/// proportion to its length while showing six rows. The cut advances in fixed
/// quanta, so between steps the source only grows at its end and the view
/// appends instead of reparsing. It starts at a paragraph, or failing that a
/// line, inside text that was already there, so the start stays put as deltas
/// arrive.
pub fn reasoning_window_tail(body: &str) -> &str {
    let quantum = REASONING_WINDOW_TAIL_QUANTUM;
    if body.len() <= quantum * 2 {
        return body;
    }
    let mut cut = (body.len() - quantum) / quantum * quantum;
    while !body.is_char_boundary(cut) {
        cut += 1;
    }
    let mut settled_end = (cut + quantum).min(body.len());
    while !body.is_char_boundary(settled_end) {
        settled_end -= 1;
    }
    let settled = &body[cut..settled_end];
    let start = settled
        .find("\n\n")
        .map(|ix| ix + 2)
        .or_else(|| settled.find('\n').map(|ix| ix + 1))
        .unwrap_or(0);
    &body[cut + start..]
}

pub fn turn_finished(turn: &TimelineConversationTurn) -> bool {
    turn.complete || turn.superseded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    Command,
    Search,
    Directory,
    FileRead,
    FileEdit,
    FileCreate,
    FileDelete,
    Todo,
    Collaboration,
    Image,
    Integration,
    Web,
    Retry,
    Generic,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Activity {
    locale: Locale,
    action: &'static str,
    target: String,
    path: Option<String>,
    icon: ActivityKind,
    failed: bool,
    /// The first line of what a failed call printed, for its collapsed row.
    error: Option<String>,
}

impl Activity {
    pub fn project(row: &TimelineRow, payload: Option<&TimelinePayload>, locale: Locale) -> Self {
        use ActivityKind as Icon;
        let (icon, action, target, path, failed) = match payload {
            Some(TimelinePayload::ToolCall(tool)) => {
                let icon = generic_activity_kind(&tool.tool_name, &tool.summary);
                let summary = if tool.summary.trim().is_empty() {
                    &tool.tool_name
                } else {
                    &tool.summary
                };
                // A command's own description says what it is for; the command
                // itself stays one disclosure away.
                let described = (icon == Icon::Command)
                    .then(|| tool_input_string(tool, &["description"]))
                    .flatten();
                let input_target = described.or_else(|| {
                    summary_is_uninformative(&tool.summary, &tool.tool_name)
                        .then(|| tool_input_target(tool, icon))
                        .flatten()
                });
                let summary = if summary_is_uninformative(summary, &tool.tool_name)
                    && input_target.is_none()
                {
                    // An ACP call id or a bare `Bash: {}` says nothing; the
                    // tool's own name is the most a reader can be told.
                    readable_tool_name(&tool.tool_name, summary)
                } else {
                    summary.to_string()
                };
                let summary = summary.as_str();
                let path = matches!(
                    icon,
                    Icon::FileRead | Icon::FileEdit | Icon::FileCreate | Icon::FileDelete
                )
                .then_some(tool.raw_extension.as_ref())
                .flatten()
                .filter(|extension| extension.locations.len() == 1)
                .and_then(|extension| extension.locations.first())
                .map(|location| location.uri.clone())
                .or_else(|| {
                    matches!(
                        icon,
                        Icon::FileRead | Icon::FileEdit | Icon::FileCreate | Icon::FileDelete
                    )
                    .then(|| input_target.clone())
                    .flatten()
                });
                let target = path
                    .as_deref()
                    .or(input_target.as_deref())
                    .unwrap_or_else(|| activity_target(summary, icon));
                (
                    icon,
                    action_label(icon, locale),
                    target.to_string(),
                    path,
                    tool.status == vibex_core::ToolCallStatus::Failed,
                )
            }
            Some(TimelinePayload::Command(command)) => (
                Icon::Command,
                action_label(Icon::Command, locale),
                command.command.clone(),
                None,
                command.status == vibex_core::CommandStatus::Failed,
            ),
            Some(TimelinePayload::FileOperation(file)) => {
                let (icon, action) = match file.operation {
                    FileOperationKind::Read => {
                        (Icon::FileRead, action_label(Icon::FileRead, locale))
                    }
                    FileOperationKind::Write => {
                        (Icon::FileCreate, action_label(Icon::FileCreate, locale))
                    }
                    FileOperationKind::Edit => {
                        (Icon::FileEdit, action_label(Icon::FileEdit, locale))
                    }
                    FileOperationKind::Delete => {
                        (Icon::FileDelete, action_label(Icon::FileDelete, locale))
                    }
                    FileOperationKind::Move => {
                        (Icon::FileEdit, locale.text("Move", "移动", "移動"))
                    }
                };
                (
                    icon,
                    action,
                    file.path.clone(),
                    Some(file.path.clone()),
                    false,
                )
            }
            Some(TimelinePayload::WebSearch(search)) => (
                Icon::Search,
                action_label(Icon::Search, locale),
                search.query.clone(),
                None,
                search.status == vibex_core::ToolCallStatus::Failed,
            ),
            Some(TimelinePayload::TodoUpdate(plan)) => (
                Icon::Todo,
                action_label(Icon::Todo, locale),
                format!(
                    "{}/{}",
                    plan.items
                        .iter()
                        .filter(|step| step.status == PlanStepStatus::Completed)
                        .count(),
                    plan.items.len()
                ),
                None,
                plan.items
                    .iter()
                    .any(|step| step.status == PlanStepStatus::Failed),
            ),
            Some(TimelinePayload::Plan(plan)) => (
                Icon::Todo,
                action_label(Icon::Todo, locale),
                format!(
                    "{}/{}",
                    plan.steps
                        .iter()
                        .filter(|step| step.status == PlanStepStatus::Completed)
                        .count(),
                    plan.steps.len()
                ),
                None,
                plan.steps
                    .iter()
                    .any(|step| step.status == PlanStepStatus::Failed),
            ),
            Some(TimelinePayload::Collaboration(agent)) => (
                Icon::Collaboration,
                action_label(Icon::Collaboration, locale),
                if agent.summary.is_empty() {
                    agent.action.clone()
                } else {
                    agent.summary.clone()
                },
                None,
                agent.status == vibex_core::ToolCallStatus::Failed,
            ),
            Some(TimelinePayload::ImageGeneration(image)) => (
                Icon::Image,
                action_label(Icon::Image, locale),
                image.summary.clone(),
                None,
                image.status == vibex_core::ToolCallStatus::Failed,
            ),
            _ => {
                let icon = match row.kind {
                    TimelineRowKind::Command => Icon::Command,
                    TimelineRowKind::FileOperation => Icon::FileRead,
                    TimelineRowKind::WebSearch => Icon::Search,
                    TimelineRowKind::TodoUpdate | TimelineRowKind::Plan => Icon::Todo,
                    TimelineRowKind::Collaboration => Icon::Collaboration,
                    TimelineRowKind::ImageGeneration => Icon::Image,
                    TimelineRowKind::Retry => Icon::Retry,
                    _ => generic_activity_kind(&row.title, ""),
                };
                (
                    icon,
                    action_label(icon, locale),
                    row.title.clone(),
                    row.file_path.clone(),
                    row.failed,
                )
            }
        };
        let error = failed.then(|| failure_line(payload, locale)).flatten();
        let target = match icon {
            Icon::Command => display_command(&target).to_string(),
            _ => target,
        };
        Self {
            locale,
            action,
            target: single_line(&path.as_deref().map_or(target, file_display_name)),
            path,
            icon,
            failed,
            error,
        }
    }

    /// The first line a failed call printed, if it printed anything.
    pub fn error_line(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn action(&self) -> &'static str {
        self.action
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn icon(&self) -> ActivityKind {
        self.icon
    }
    pub fn is_failed(&self) -> bool {
        self.failed
    }
    pub fn label(&self) -> String {
        format!(
            "{} {}",
            self.action,
            self.path.as_deref().unwrap_or(&self.target)
        )
        .trim_end()
        .to_string()
    }
    pub fn approximate_bytes(&self) -> usize {
        self.target.len() + self.path.as_ref().map_or(0, String::len)
    }
}

pub(crate) fn input_keys(kind: ActivityKind) -> &'static [&'static str] {
    match kind {
        ActivityKind::Command => &["command", "cmd", "commands", "script"],
        ActivityKind::FileRead
        | ActivityKind::FileEdit
        | ActivityKind::FileCreate
        | ActivityKind::FileDelete => &["file_path", "path", "filePath"],
        ActivityKind::Search => &["pattern", "query", "regex", "q"],
        ActivityKind::Collaboration => &["description", "subagent_type", "agent", "prompt"],
        ActivityKind::Integration => &["name", "skill", "server", "tool"],
        ActivityKind::Web => &["url", "query", "queries"],
        ActivityKind::Directory => &["pattern", "glob", "path"],
        _ => &[],
    }
}

fn tool_input_target(tool: &ToolCallPayload, kind: ActivityKind) -> Option<String> {
    let keys = input_keys(kind);
    if keys.is_empty() {
        return None;
    }
    let source = tool
        .raw_extension
        .as_ref()
        .and_then(|extension| extension.raw_input.as_deref())
        .filter(|source| !source.trim().is_empty())
        .or(tool.input_summary.as_deref())?
        .trim();
    match serde_json::from_str::<serde_json::Value>(source) {
        Ok(serde_json::Value::String(text)) => Some(text),
        Ok(serde_json::Value::Object(object)) if kind == ActivityKind::Search => {
            let pattern = keys.iter().find_map(|key| {
                object
                    .get(*key)?
                    .as_str()
                    .filter(|text| !text.trim().is_empty())
            })?;
            Some(
                match object
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .filter(|path| !path.trim().is_empty() && *path != ".")
                {
                    Some(path) => format!("{pattern} in {path}"),
                    None => pattern.to_string(),
                },
            )
        }
        Ok(serde_json::Value::Object(object)) => keys.iter().find_map(|key| {
            match object.get(*key)? {
                serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
                // A batch runner's `commands`: the first stands for the batch.
                serde_json::Value::Array(items) => {
                    let mut texts = items.iter().filter_map(serde_json::Value::as_str);
                    let first = texts.next()?;
                    let more = texts.count();
                    Some(if more == 0 {
                        first.to_string()
                    } else {
                        format!("{first} (+{more})")
                    })
                }
                _ => None,
            }
        }),
        Err(_) if !source.is_empty() && !source.starts_with(['{', '[']) => Some(source.to_string()),
        _ => None,
    }
}

/// Whether a tool call's title names the tool rather than what it did.
///
/// Agents title calls very differently: some send the command or path, others
/// only the tool's name (`Bash`, `Read`), a generated id (`ACP tool call
/// call_5d27…`) or the name over empty arguments (`Bash: {}`). Those titles
/// leave the header to the call's own arguments.
fn summary_is_uninformative(summary: &str, tool_name: &str) -> bool {
    let summary = summary.trim();
    summary.is_empty()
        || summary.eq_ignore_ascii_case(tool_name.trim())
        || summary.starts_with("ACP tool call")
        || summary.ends_with(": {}")
        || (!summary.contains(char::is_whitespace)
            && summary.len() <= 40
            && summary
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_-.".contains(character)))
}

/// A tool's name as the header shows it when nothing better is known.
///
/// An MCP tool reads as its server and tool (`github · create_issue`), and a
/// snake- or kebab-case name as words (`ask_user_question` → `Ask user
/// question`).
fn readable_tool_name(tool_name: &str, summary: &str) -> String {
    let summary = summary.trim();
    let name = if !summary.starts_with("ACP tool call") && !summary.is_empty() {
        summary.strip_suffix(": {}").unwrap_or(summary)
    } else {
        match tool_name.trim() {
            "" | "tool" | "other" => summary,
            name => name,
        }
    };
    if let Some((server, tool)) = mcp_tool_name(name) {
        return format!("{server} · {tool}");
    }
    humanize_tool_name(name)
}

/// `mcp__server__tool` (Claude) or `mcp_server_tool` style names, split.
pub fn mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

fn humanize_tool_name(name: &str) -> String {
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || name.contains([':', '/', '.'])
        || !name.contains(['_', '-'])
    {
        return name.to_string();
    }
    let words = name
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let mut characters = words.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

/// One string argument of a tool call, by the first of `keys` it carries.
fn tool_input_string(tool: &ToolCallPayload, keys: &[&str]) -> Option<String> {
    let source = tool
        .raw_extension
        .as_ref()
        .and_then(|extension| extension.raw_input.as_deref())
        .filter(|source| !source.trim().is_empty())
        .or(tool.input_summary.as_deref())?;
    let value = serde_json::from_str::<serde_json::Value>(source).ok()?;
    keys.iter().find_map(|key| {
        value
            .get(*key)?
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

/// A command as its header shows it.
///
/// Agents routinely open every command with `cd <absolute path> &&` to pin the
/// directory it runs in. That prefix is the same on every row and pushes what
/// the command does past the truncation point, so the header leaves it out;
/// the full command stays in the detail.
pub fn display_command(command: &str) -> &str {
    let trimmed = command.trim_start();
    let Some(rest) = trimmed.strip_prefix("cd ") else {
        return command;
    };
    let Some(end) = rest.find(" && ").or_else(|| rest.find("; ")) else {
        return command;
    };
    let directory = rest[..end].trim().trim_matches(['"', '\'']);
    if directory.contains(char::is_whitespace) && !rest[..end].trim().starts_with(['"', '\'']) {
        return command;
    }
    let separator = if rest[end..].starts_with(" && ") {
        4
    } else {
        2
    };
    let remainder = rest[end + separator..].trim_start();
    if remainder.is_empty() {
        command
    } else {
        remainder
    }
}

/// The first readable line a failed call left behind.
fn failure_line(payload: Option<&TimelinePayload>, locale: Locale) -> Option<String> {
    let text = match payload? {
        TimelinePayload::ToolCall(tool) => crate::tool_detail::output(
            tool.raw_extension.as_ref(),
            tool.output_summary.as_deref(),
            None,
            locale,
        )?,
        TimelinePayload::Command(command) => crate::tool_detail::output(
            command.raw_extension.as_ref(),
            command.output_summary.as_deref(),
            command.exit_code,
            locale,
        )?,
        _ => return None,
    };
    let heading = locale.text("Error", "错误", "錯誤");
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != heading)
        .map(|line| line.chars().take(240).collect())
}

/// How long a settled row ran, from its first event to its last.
///
/// Only commands and delegated agents carry a duration: those are the calls a
/// reader waits on, and a read or a search finishing in milliseconds would
/// only add noise. `items` is the sequence-sorted timeline the row came from.
pub fn row_duration_ms(
    row: &TimelineRow,
    activity: &Activity,
    items: &[TimelineItem],
) -> Option<i64> {
    if row.streaming
        || !matches!(
            activity.icon,
            ActivityKind::Command | ActivityKind::Collaboration
        )
    {
        return None;
    }
    let index = items
        .binary_search_by_key(&row.first_sequence, |item| item.sequence)
        .ok()?;
    let elapsed = row.timestamp_ms - items[index].timestamp_ms;
    (elapsed >= 1_000).then_some(elapsed)
}

/// A duration as a header reads it: `4.2s`, `38s`, `2m 05s`, `1h 03m`.
pub fn format_duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 10_000 {
        return format!("{:.1}s", ms as f64 / 1_000.0);
    }
    let seconds = ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, seconds % 3_600 / 60)
    }
}

fn action_label(icon: ActivityKind, locale: Locale) -> &'static str {
    match icon {
        ActivityKind::Command => locale.text("Run", "运行", "執行"),
        ActivityKind::Search => locale.text("Search", "搜索", "搜尋"),
        ActivityKind::Directory => locale.text("List", "列出", "列出"),
        ActivityKind::FileRead => locale.text("Read", "读取", "讀取"),
        ActivityKind::FileEdit => locale.text("Edit", "编辑", "編輯"),
        ActivityKind::FileCreate => locale.text("Write", "写入", "寫入"),
        ActivityKind::FileDelete => locale.text("Delete", "删除", "刪除"),
        ActivityKind::Todo => locale.text("Plan", "计划", "計畫"),
        ActivityKind::Collaboration => "Agent",
        ActivityKind::Image => locale.text("Image", "图像", "圖像"),
        ActivityKind::Integration => "MCP",
        ActivityKind::Web => locale.text("Web", "网页", "網頁"),
        ActivityKind::Retry => locale.text("Retry", "重试", "重試"),
        ActivityKind::Generic => locale.text("Tool", "工具", "工具"),
    }
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A summary may already start with its action. Remove only a matching verb;
/// command text and unfamiliar summaries retain their complete visible value.
pub fn activity_target(summary: &str, icon: ActivityKind) -> &str {
    let summary = summary.trim();
    let Some((verb, target)) = summary.split_once(char::is_whitespace) else {
        return summary;
    };
    let verbs: &[&str] = match icon {
        ActivityKind::Command => &["run", "ran", "running", "execute", "executed"],
        ActivityKind::Search => &["search", "searched", "searching"],
        ActivityKind::Directory => &["list", "listed", "listing"],
        ActivityKind::FileRead => &["read", "reading", "view", "viewed"],
        ActivityKind::FileEdit => &["edit", "edited", "editing", "patch", "patched"],
        ActivityKind::FileCreate => &["write", "wrote", "writing", "create", "created"],
        ActivityKind::FileDelete => &["delete", "deleted", "remove", "removed"],
        _ => &[],
    };
    if verbs
        .iter()
        .any(|candidate| verb.eq_ignore_ascii_case(candidate))
    {
        target.trim_start()
    } else {
        summary
    }
}

pub fn group_open(
    turn: &TimelineConversationTurn,
    group: &TimelineProcessActivityGroup,
    explicit: Option<bool>,
) -> bool {
    explicit.unwrap_or_else(|| {
        !turn_finished(turn)
            && turn
                .conclusion_row
                .as_ref()
                .is_none_or(|row| row.body.trim().is_empty())
            && (group.end_row == turn.process_rows.len()
                || turn
                    .process_rows
                    .get(group.start_row..group.end_row)
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row.streaming || row.pending_permission)
                    }))
    })
}

pub fn is_running(row: &TimelineRow, turn_live: bool) -> bool {
    turn_live && row.streaming && !row.failed && !row.pending_permission
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActivitySummary {
    commands: usize,
    reads: usize,
    edits: BTreeSet<String>,
    searches: usize,
    plans: usize,
    agents: usize,
    other: usize,
    failed: usize,
}

impl ActivitySummary {
    pub fn record(&mut self, activity: &Activity, row_id: &str) {
        self.failed += usize::from(activity.failed);
        match activity.icon {
            ActivityKind::Command => self.commands += 1,
            ActivityKind::FileRead => self.reads += 1,
            ActivityKind::FileEdit | ActivityKind::FileCreate | ActivityKind::FileDelete => {
                self.edits
                    .insert(activity.path.as_deref().unwrap_or(row_id).to_string());
            }
            ActivityKind::Search | ActivityKind::Directory => self.searches += 1,
            ActivityKind::Todo => self.plans += 1,
            ActivityKind::Collaboration => self.agents += 1,
            _ => self.other += 1,
        }
    }

    pub fn label(&self, locale: Locale) -> String {
        let entries = [
            (
                self.edits.len(),
                "Changed {n} file",
                "Changed {n} files",
                "修改 {n} 个文件",
                "修改 {n} 個檔案",
            ),
            (
                self.commands,
                "Ran {n} command",
                "Ran {n} commands",
                "运行 {n} 条命令",
                "執行 {n} 條命令",
            ),
            (
                self.reads,
                "Read {n} file",
                "Read {n} files",
                "读取 {n} 个文件",
                "讀取 {n} 個檔案",
            ),
            (
                self.searches,
                "Searched {n} time",
                "Searched {n} times",
                "搜索 {n} 次",
                "搜尋 {n} 次",
            ),
            (
                self.plans,
                "Updated {n} plan",
                "Updated {n} plans",
                "更新 {n} 次计划",
                "更新 {n} 次計畫",
            ),
            (
                self.agents,
                "Called {n} agent",
                "Called {n} agents",
                "调用 {n} 个 Agent",
                "呼叫 {n} 個 Agent",
            ),
            (
                self.other,
                "Called {n} tool",
                "Called {n} tools",
                "调用 {n} 次工具",
                "呼叫 {n} 次工具",
            ),
            (
                self.failed,
                "{n} failed",
                "{n} failed",
                "{n} 项失败",
                "{n} 項失敗",
            ),
        ];
        entries
            .into_iter()
            .filter(|(count, ..)| *count > 0)
            .map(|(count, singular, plural, zh, tw)| {
                locale
                    .text(if count == 1 { singular } else { plural }, zh, tw)
                    .replace("{n}", &count.to_string())
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn generic_activity_kind(tool_name: &str, summary: &str) -> ActivityKind {
    semantic_tool_activity_icon(tool_name)
        .or_else(|| semantic_tool_activity_icon(summary))
        .unwrap_or(ActivityKind::Generic)
}

fn semantic_tool_activity_icon(value: &str) -> Option<ActivityKind> {
    let terms = normalized_activity_terms(value);
    let has_any = |candidates: &[&str]| {
        terms
            .split_whitespace()
            .any(|term| candidates.contains(&term))
    };

    if value.trim_start().starts_with("mcp__") {
        Some(ActivityKind::Integration)
    } else if has_any(&["todo", "todos", "plan", "checklist"]) {
        Some(ActivityKind::Todo)
    } else if has_any(&[
        "command",
        "execute",
        "exec",
        "shell",
        "terminal",
        "bash",
        "powershell",
        "run",
        "ran",
    ]) {
        Some(ActivityKind::Command)
    } else if has_any(&["web", "fetch", "url", "browse", "http", "https"]) {
        Some(ActivityKind::Web)
    } else if has_any(&["search", "searched", "grep", "find", "query", "rg"]) {
        Some(ActivityKind::Search)
    } else if has_any(&[
        "list",
        "listed",
        "glob",
        "directory",
        "directories",
        "folder",
        "folders",
        "tree",
    ]) {
        Some(ActivityKind::Directory)
    } else if has_any(&["delete", "deleted", "remove", "removed", "trash"]) {
        Some(ActivityKind::FileDelete)
    } else if has_any(&["create", "created", "write", "wrote", "add", "added", "new"]) {
        Some(ActivityKind::FileCreate)
    } else if has_any(&[
        "edit", "edited", "patch", "patched", "replace", "replaced", "update", "updated",
    ]) {
        Some(ActivityKind::FileEdit)
    } else if has_any(&[
        "read",
        "view",
        "viewed",
        "open",
        "opened",
        "inspect",
        "inspected",
        "load",
        "loaded",
    ]) {
        Some(ActivityKind::FileRead)
    } else if has_any(&["agent", "subagent", "collaboration", "delegate", "task"]) {
        Some(ActivityKind::Collaboration)
    } else if has_any(&["image", "picture", "photo"]) {
        Some(ActivityKind::Image)
    } else if has_any(&["mcp", "plugin", "integration", "skill"]) {
        Some(ActivityKind::Integration)
    } else {
        None
    }
}

fn normalized_activity_terms(value: &str) -> String {
    let mut terms = String::with_capacity(value.len());
    let mut previous_was_lower_or_digit = false;
    let mut previous_was_separator = true;
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase()
                && previous_was_lower_or_digit
                && !previous_was_separator
            {
                terms.push(' ');
            }
            terms.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
            previous_was_separator = false;
        } else if !previous_was_separator && !terms.is_empty() {
            terms.push(' ');
            previous_was_lower_or_digit = false;
            previous_was_separator = true;
        }
    }
    terms.truncate(terms.trim_end().len());
    terms
}

pub fn file_display_name(path: &str) -> String {
    if let Ok(url) = url::Url::parse(path)
        && url.scheme() == "file"
    {
        return url
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .map(|name| {
                percent_encoding::percent_decode_str(name)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| path.to_string());
    }
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}
