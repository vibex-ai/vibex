use std::collections::HashMap;

pub use vibex_agent::ProviderEvent;
use vibex_core::{
    AgentMessageDeltaPayload, ReasoningPayload, TimelinePayload, ToolCallPayload, ToolCallStatus,
};

pub fn map_stream_event(
    event: &serde_json::Value,
    chunk_index: &mut u32,
    tool_blocks: &mut HashMap<u64, (String, String, String)>,
) -> Option<ProviderEvent> {
    match event.get("type").and_then(|value| value.as_str()) {
        Some("content_block_start") => {
            let content_block = event.get("content_block")?;
            if content_block.get("type").and_then(|value| value.as_str()) != Some("tool_use") {
                return None;
            }
            let index = event
                .get("index")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let tool_id = content_block
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("claude-tool")
                .to_string();
            let tool_name = content_block
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("tool")
                .to_string();
            let input_summary = content_block
                .get("input")
                .map(|value| value.to_string())
                .unwrap_or_default();
            tool_blocks.insert(
                index,
                (tool_id.clone(), tool_name.clone(), input_summary.clone()),
            );
            Some(ProviderEvent::agent(TimelinePayload::ToolCall(
                ToolCallPayload {
                    tool_call_id: tool_id,
                    tool_name,
                    status: ToolCallStatus::Started,
                    summary: "Claude tool call".to_string(),
                    input_summary: truncate_summary(input_summary),
                    output_summary: None,
                    raw_extension: None,
                },
            )))
        }
        Some("content_block_delta") => {
            let delta = event.get("delta")?;
            match delta.get("type").and_then(|value| value.as_str()) {
                Some("text_delta") => {
                    let text = delta.get("text").and_then(|value| value.as_str())?;
                    if text.is_empty() {
                        return None;
                    }
                    let event = ProviderEvent::agent(TimelinePayload::AgentMessageDelta(
                        AgentMessageDeltaPayload {
                            text_delta: text.to_string(),
                            chunk_index: *chunk_index,
                        },
                    ));
                    *chunk_index = (*chunk_index).saturating_add(1);
                    Some(event)
                }
                Some("thinking_delta") => {
                    let text = delta
                        .get("thinking")
                        .or_else(|| delta.get("text"))
                        .and_then(|value| value.as_str())?;
                    (!text.is_empty()).then(|| {
                        ProviderEvent::agent(TimelinePayload::Reasoning(ReasoningPayload {
                            text: text.to_string(),
                            is_final: false,
                        }))
                    })
                }
                Some("input_json_delta") => {
                    let index = event
                        .get("index")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let partial_json = delta
                        .get("partial_json")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let (tool_id, tool_name, input_summary) =
                        tool_blocks.entry(index).or_insert_with(|| {
                            (
                                format!("claude-tool-{index}"),
                                "tool".to_string(),
                                String::new(),
                            )
                        });
                    input_summary.push_str(partial_json);
                    Some(ProviderEvent::agent(TimelinePayload::ToolCall(
                        ToolCallPayload {
                            tool_call_id: tool_id.clone(),
                            tool_name: tool_name.clone(),
                            status: ToolCallStatus::Progress,
                            summary: "Claude tool input".to_string(),
                            input_summary: truncate_summary(input_summary.clone()),
                            output_summary: None,
                            raw_extension: None,
                        },
                    )))
                }
                _ => None,
            }
        }
        _ => None,
    }
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
