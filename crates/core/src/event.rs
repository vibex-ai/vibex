use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, CorrelationId, EventId};
use crate::time::unix_timestamp_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const fn stage0() -> Self {
        Self { major: 0, minor: 1 }
    }
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::stage0()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolChannelKind {
    Control,
    Terminal,
    Chunk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolFrameEncoding {
    Json,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventChannel {
    pub id: ChannelId,
    pub kind: ProtocolChannelKind,
    pub frame_encoding: ProtocolFrameEncoding,
}

impl EventChannel {
    pub fn control_json() -> Self {
        Self {
            id: ChannelId::new(),
            kind: ProtocolChannelKind::Control,
            frame_encoding: ProtocolFrameEncoding::Json,
        }
    }

    pub fn terminal_binary_reserved() -> Self {
        Self {
            id: ChannelId::new(),
            kind: ProtocolChannelKind::Terminal,
            frame_encoding: ProtocolFrameEncoding::Binary,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkDescriptor {
    pub channel_id: ChannelId,
    pub offset: u64,
    pub length: u32,
    pub is_final: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    FoundationStatus,
    SmokeStarted,
    SmokeCompleted,
    TerminalChunkReserved,
    ProtocolNotice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundationStatusPayload {
    pub app_name: String,
    pub workspace_version: String,
    pub core_contract_version: String,
    pub generated_at_ms: i64,
}

impl FoundationStatusPayload {
    pub fn stage0() -> Self {
        Self {
            app_name: "Vibex".to_string(),
            workspace_version: env!("CARGO_PKG_VERSION").to_string(),
            core_contract_version: "stage0-minimal".to_string(),
            generated_at_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    Empty,
    FoundationStatus(FoundationStatusPayload),
    Chunk(ChunkDescriptor),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexEventEnvelope {
    pub protocol_version: ProtocolVersion,
    pub event_id: EventId,
    pub timestamp_ms: i64,
    pub correlation_id: Option<CorrelationId>,
    pub channel: EventChannel,
    pub kind: EventKind,
    pub payload: EventPayload,
}

impl VibexEventEnvelope {
    pub fn new(kind: EventKind, payload: EventPayload) -> Self {
        Self {
            protocol_version: ProtocolVersion::default(),
            event_id: EventId::new(),
            timestamp_ms: unix_timestamp_ms(),
            correlation_id: None,
            channel: EventChannel::control_json(),
            kind,
            payload,
        }
    }

    pub fn foundation_status() -> Self {
        Self::new(
            EventKind::FoundationStatus,
            EventPayload::FoundationStatus(FoundationStatusPayload::stage0()),
        )
    }

    pub fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_version_and_control_channel() {
        let envelope = VibexEventEnvelope::foundation_status();

        assert_eq!(envelope.protocol_version, ProtocolVersion::stage0());
        assert_eq!(envelope.channel.kind, ProtocolChannelKind::Control);
        assert_eq!(envelope.channel.frame_encoding, ProtocolFrameEncoding::Json);
    }

    #[test]
    fn envelope_serializes_discriminated_payload() {
        let envelope = VibexEventEnvelope::foundation_status();
        let json = serde_json::to_value(envelope).unwrap();

        assert_eq!(json["kind"], "foundation_status");
        assert_eq!(json["payload"]["type"], "foundation_status");
    }
}
