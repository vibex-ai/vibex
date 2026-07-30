pub use vibex_agent::ProviderEvent;
use vibex_core::{
    AgentMessagePayload, CollaborationPayload, CommandPayload, CommandStatus, FileOperationKind,
    FileOperationPayload, PlanPayload, PlanStepPayload, PlanStepStatus, ReasoningPayload,
    TimelineErrorPayload, TimelinePayload, TodoUpdatePayload, ToolCallPayload, ToolCallStatus,
    WebSearchPayload,
};

pub fn map_wire_item(item: &serde_json::Value) -> Vec<ProviderEvent> {
    let Some(object) = item.as_object() else {
        return Vec::new();
    };
    let item_type = object.get("type").and_then(serde_json::Value::as_str);
    let text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let optional_text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let id = text("id");

    match item_type {
        Some("agentMessage") => vec![ProviderEvent::agent(TimelinePayload::AgentMessage(
            AgentMessagePayload {
                text: text("text"),
                is_final: object.get("phase").and_then(serde_json::Value::as_str)
                    == Some("final_answer"),
            },
        ))],
        Some("reasoning") => vec![ProviderEvent::agent(TimelinePayload::Reasoning(
            ReasoningPayload {
                text: reasoning_text(object),
                is_final: true,
            },
        ))],
        Some("plan") => vec![ProviderEvent::agent(TimelinePayload::Plan(PlanPayload {
            title: "Codex plan".to_string(),
            steps: vec![PlanStepPayload {
                title: text("text"),
                status: PlanStepStatus::Running,
            }],
        }))],
        Some("commandExecution") => vec![ProviderEvent::agent(TimelinePayload::Command(
            CommandPayload {
                command: text("command"),
                cwd: None,
                status: command_status(object.get("status").and_then(serde_json::Value::as_str)),
                exit_code: object
                    .get("exitCode")
                    .and_then(serde_json::Value::as_i64)
                    .map(|value| value as i32),
                output_summary: truncate_summary(text("aggregatedOutput")),
                raw_extension: None,
            },
        ))],
        Some("fileChange") => {
            let status = status_label(object.get("status").and_then(serde_json::Value::as_str));
            object
                .get("changes")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| change.as_object())
                .map(|change| {
                    let operation = match change.get("kind").and_then(serde_json::Value::as_str) {
                        Some("add") => FileOperationKind::Write,
                        Some("delete") => FileOperationKind::Delete,
                        _ => FileOperationKind::Edit,
                    };
                    ProviderEvent::agent(TimelinePayload::FileOperation(FileOperationPayload {
                        operation,
                        path: change
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        summary: format!("Codex file change: {status}"),
                        old_text: None,
                        new_text: None,
                        raw_extension: None,
                    }))
                })
                .collect()
        }
        Some("mcpToolCall") => vec![tool_event(
            id,
            format!("{}:{}", text("server"), text("tool")),
            mcp_status(object.get("status").and_then(serde_json::Value::as_str)),
            object
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null)
                .to_string(),
            object.get("result").cloned().map(|value| value.to_string()),
        )],
        Some("dynamicToolCall") => vec![tool_event(
            id,
            text("tool"),
            match object.get("success").and_then(serde_json::Value::as_bool) {
                Some(true) => ToolCallStatus::Completed,
                Some(false) => ToolCallStatus::Failed,
                None => ToolCallStatus::Progress,
            },
            object
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null)
                .to_string(),
            Some(format!("status={}", text("status"))),
        )],
        Some("collabToolCall") => vec![ProviderEvent::agent(TimelinePayload::Collaboration(
            CollaborationPayload {
                action: text("tool"),
                status: text_tool_status(&text("status")),
                summary: optional_text("agentStatus")
                    .or_else(|| optional_text("prompt"))
                    .unwrap_or_else(|| "Codex collaboration update".to_string()),
                agent_label: optional_text("receiverThreadId")
                    .or_else(|| optional_text("newThreadId")),
                raw_extension: None,
            },
        ))],
        Some("webSearch") => vec![ProviderEvent::agent(TimelinePayload::WebSearch(
            WebSearchPayload {
                query: text("query"),
                status: ToolCallStatus::Completed,
                result_summary: None,
                raw_extension: None,
            },
        ))],
        Some("todoList") => vec![ProviderEvent::agent(TimelinePayload::TodoUpdate(
            TodoUpdatePayload {
                title: "Codex todo".to_string(),
                items: object
                    .get("items")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.as_object())
                    .map(|item| PlanStepPayload {
                        title: item
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        status: if item
                            .get("completed")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            PlanStepStatus::Completed
                        } else {
                            PlanStepStatus::Pending
                        },
                    })
                    .collect(),
                raw_extension: None,
            },
        ))],
        Some("error") => vec![ProviderEvent::provider(TimelinePayload::Error(
            TimelineErrorPayload {
                code: "codex_item_error".to_string(),
                message: text("message"),
                recoverable: true,
            },
        ))],
        _ => Vec::new(),
    }
}

fn reasoning_text(object: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
        return text.to_string();
    }
    let joined = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let summary = joined("summary");
    let content = joined("content");
    match (summary.is_empty(), content.is_empty()) {
        (true, _) => content,
        (_, true) => summary,
        _ => format!("{summary}\n{content}"),
    }
}

fn command_status(status: Option<&str>) -> CommandStatus {
    match status {
        Some("completed") => CommandStatus::Completed,
        Some("failed" | "declined") => CommandStatus::Failed,
        _ => CommandStatus::Started,
    }
}

fn mcp_status(status: Option<&str>) -> ToolCallStatus {
    match status {
        Some("completed") => ToolCallStatus::Completed,
        Some("failed") => ToolCallStatus::Failed,
        Some("inProgress" | "in_progress") => ToolCallStatus::Started,
        _ => ToolCallStatus::Progress,
    }
}

fn text_tool_status(status: &str) -> ToolCallStatus {
    match status.trim().to_ascii_lowercase().as_str() {
        "completed" | "complete" | "done" | "success" => ToolCallStatus::Completed,
        "failed" | "error" | "cancelled" | "canceled" => ToolCallStatus::Failed,
        "running" | "in_progress" | "progress" => ToolCallStatus::Progress,
        _ => ToolCallStatus::Started,
    }
}

fn status_label(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") => "Completed",
        Some("failed") => "Failed",
        Some("declined") => "Declined",
        Some("inProgress" | "in_progress") => "InProgress",
        _ => "Unknown",
    }
}

fn tool_event(
    tool_call_id: String,
    tool_name: String,
    status: ToolCallStatus,
    input_summary: String,
    output_summary: Option<String>,
) -> ProviderEvent {
    ProviderEvent::agent(TimelinePayload::ToolCall(ToolCallPayload {
        tool_call_id,
        tool_name,
        status,
        summary: "Codex tool call".to_string(),
        input_summary: truncate_summary(input_summary),
        output_summary: output_summary.and_then(truncate_summary),
        raw_extension: None,
    }))
}

fn truncate_summary(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if value.len() > 4_000 {
        Some(format!("{}...(truncated)", &value[..4_000]))
    } else {
        Some(value.to_string())
    }
}
