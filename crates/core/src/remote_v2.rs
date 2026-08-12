use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    CorrelationId, DeviceId, ErrorCategory, EventId, RemoteActionClass, RemoteAuthProof,
    RemoteDeviceDetail, RemoteDevicePermissionLevel, RemoteOperationKind,
    RemoteRevokeDeviceRequest, RequestId, VibexError, unix_timestamp_ms,
};

macro_rules! impl_unknown_safe_enum {
    ($name:ty, $($wire:literal => $variant:path),+ $(,)?) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => $variant,)+
                    _ => <$name>::Unknown,
                })
            }
        }
    };
}

pub const REMOTE_PROTOCOL_V2_MAJOR: u16 = 2;
pub const REMOTE_PROTOCOL_V2_MINOR: u16 = 0;
pub const REMOTE_V2_BINARY_MAGIC: [u8; 4] = *b"VBX2";
pub const REMOTE_V2_MAX_BINARY_HEADER_BYTES: usize = 64 * 1024;
pub const REMOTE_V2_MAX_BINARY_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProtocolVersionRange {
    pub min: crate::RemoteProtocolVersion,
    pub max: crate::RemoteProtocolVersion,
}

impl RemoteProtocolVersionRange {
    pub const fn v2() -> Self {
        let version = crate::RemoteProtocolVersion {
            major: REMOTE_PROTOCOL_V2_MAJOR,
            minor: REMOTE_PROTOCOL_V2_MINOR,
        };
        Self {
            min: version,
            max: version,
        }
    }

    pub fn negotiate(self, peer: Self) -> Option<crate::RemoteProtocolVersion> {
        let min = max_version(self.min, peer.min);
        let max = min_version(self.max, peer.max);
        (version_key(min) <= version_key(max)).then_some(max)
    }
}

fn version_key(version: crate::RemoteProtocolVersion) -> (u16, u16) {
    (version.major, version.minor)
}

fn min_version(
    left: crate::RemoteProtocolVersion,
    right: crate::RemoteProtocolVersion,
) -> crate::RemoteProtocolVersion {
    if version_key(left) <= version_key(right) {
        left
    } else {
        right
    }
}

fn max_version(
    left: crate::RemoteProtocolVersion,
    right: crate::RemoteProtocolVersion,
) -> crate::RemoteProtocolVersion {
    if version_key(left) >= version_key(right) {
        left
    } else {
        right
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteClientType {
    DesktopWeb,
    Mobile,
    Browser,
    Native,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteClientType,
    "desktop_web" => RemoteClientType::DesktopWeb,
    "mobile" => RemoteClientType::Mobile,
    "browser" => RemoteClientType::Browser,
    "native" => RemoteClientType::Native,
    "unknown" => RemoteClientType::Unknown,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTimeoutClass {
    Interactive,
    #[default]
    Standard,
    LongRunning,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteTimeoutClass,
    "interactive" => RemoteTimeoutClass::Interactive,
    "standard" => RemoteTimeoutClass::Standard,
    "long_running" => RemoteTimeoutClass::LongRunning,
    "unknown" => RemoteTimeoutClass::Unknown,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRetryClass {
    Never,
    Reconnect,
    RefreshAuthentication,
    AuthoritativeResync,
    RetryWithIdempotencyKey,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteRetryClass,
    "never" => RemoteRetryClass::Never,
    "reconnect" => RemoteRetryClass::Reconnect,
    "refresh_authentication" => RemoteRetryClass::RefreshAuthentication,
    "authoritative_resync" => RemoteRetryClass::AuthoritativeResync,
    "retry_with_idempotency_key" => RemoteRetryClass::RetryWithIdempotencyKey,
    "unknown" => RemoteRetryClass::Unknown,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCloseCode {
    Normal,
    ProtocolError,
    UnsupportedVersion,
    AuthenticationRequired,
    DeviceRevoked,
    PolicyViolation,
    ServerShutdown,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteCloseCode,
    "normal" => RemoteCloseCode::Normal,
    "protocol_error" => RemoteCloseCode::ProtocolError,
    "unsupported_version" => RemoteCloseCode::UnsupportedVersion,
    "authentication_required" => RemoteCloseCode::AuthenticationRequired,
    "device_revoked" => RemoteCloseCode::DeviceRevoked,
    "policy_violation" => RemoteCloseCode::PolicyViolation,
    "server_shutdown" => RemoteCloseCode::ServerShutdown,
    "unknown" => RemoteCloseCode::Unknown,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStreamCursor {
    pub domain: String,
    pub generation: u64,
    pub cursor: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHello {
    pub client_id: String,
    pub client_type: RemoteClientType,
    pub app_version: String,
    pub protocol_range: RemoteProtocolVersionRange,
    pub device_id: DeviceId,
    pub device_identity_public_key: String,
    pub client_ephemeral_public_key: String,
    pub identity_proof: String,
    /// Relay carries the device grant only inside the outer E2EE session.
    /// Direct mode authenticates its one-use WS ticket and leaves this empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_auth: Option<RemoteAuthProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_context_hash: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub enabled_features: Vec<String>,
    pub last_session_epoch: Option<u64>,
    #[serde(default)]
    pub cursors: Vec<RemoteStreamCursor>,
}

impl fmt::Debug for RemoteHello {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteHello")
            .field("client_id", &self.client_id)
            .field("client_type", &self.client_type)
            .field("app_version", &self.app_version)
            .field("protocol_range", &self.protocol_range)
            .field("device_id", &self.device_id)
            .field(
                "has_device_identity_public_key",
                &!self.device_identity_public_key.is_empty(),
            )
            .field(
                "has_client_ephemeral_public_key",
                &!self.client_ephemeral_public_key.is_empty(),
            )
            .field("has_identity_proof", &!self.identity_proof.is_empty())
            .field("has_relay_auth", &self.relay_auth.is_some())
            .field("transport_endpoint", &self.transport_endpoint)
            .field(
                "has_permission_context_hash",
                &self.permission_context_hash.is_some(),
            )
            .field("capabilities", &self.capabilities)
            .field("enabled_features", &self.enabled_features)
            .field("last_session_epoch", &self.last_session_epoch)
            .field("cursors", &self.cursors)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServerInfoV2 {
    pub server_id: String,
    pub server_identity_public_key: String,
    pub desktop_version: String,
    pub protocol_range: RemoteProtocolVersionRange,
    pub selected_protocol: crate::RemoteProtocolVersion,
    pub server_ephemeral_public_key: String,
    pub session_key_confirmation: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub enabled_features: Vec<String>,
    #[serde(default)]
    pub device_permissions: Vec<RemoteActionClass>,
    pub session_epoch: u64,
    pub connection_id: RequestId,
    pub server_time_ms: i64,
}

impl fmt::Debug for RemoteServerInfoV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteServerInfoV2")
            .field("server_id", &self.server_id)
            .field("desktop_version", &self.desktop_version)
            .field("protocol_range", &self.protocol_range)
            .field("selected_protocol", &self.selected_protocol)
            .field(
                "has_server_ephemeral_public_key",
                &!self.server_ephemeral_public_key.is_empty(),
            )
            .field(
                "has_session_key_confirmation",
                &!self.session_key_confirmation.is_empty(),
            )
            .field("capabilities", &self.capabilities)
            .field("enabled_features", &self.enabled_features)
            .field("device_permissions", &self.device_permissions)
            .field("session_epoch", &self.session_epoch)
            .field("connection_id", &self.connection_id)
            .field("server_time_ms", &self.server_time_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePing {
    pub nonce: u64,
    pub sent_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSubscribeRequestV2 {
    pub subscription_id: String,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub cursors: Vec<RemoteStreamCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSubscriptionAcceptedV2 {
    pub subscription_id: String,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub resync_required: Vec<RemoteResyncRequired>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAttachmentKind {
    AgentTimeline,
    Terminal,
    FileTransfer,
    Git,
    Provider,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteAttachmentKind,
    "agent_timeline" => RemoteAttachmentKind::AgentTimeline,
    "terminal" => RemoteAttachmentKind::Terminal,
    "file_transfer" => RemoteAttachmentKind::FileTransfer,
    "git" => RemoteAttachmentKind::Git,
    "provider" => RemoteAttachmentKind::Provider,
    "unknown" => RemoteAttachmentKind::Unknown,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAttachRequestV2 {
    pub attachment_id: String,
    pub kind: RemoteAttachmentKind,
    pub resource_id: String,
    pub scope_id: Option<String>,
    pub generation: u64,
    pub after_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAttachmentAcceptedV2 {
    pub attachment_id: String,
    pub generation: u64,
    pub next_sequence: u64,
    pub snapshot_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDetachRequestV2 {
    pub attachment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteResyncRequired {
    pub domain: String,
    pub generation: u64,
    pub reason: String,
    pub authoritative_operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCloseReason {
    pub code: RemoteCloseCode,
    pub message: String,
    pub retry: RemoteRetryClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteControlMessageV2 {
    Hello(RemoteHello),
    ServerInfo(RemoteServerInfoV2),
    Ping(RemotePing),
    Pong(RemotePing),
    Subscribe(RemoteSubscribeRequestV2),
    Subscribed(RemoteSubscriptionAcceptedV2),
    Attach(RemoteAttachRequestV2),
    Attached(RemoteAttachmentAcceptedV2),
    Detach(RemoteDetachRequestV2),
    Detached(RemoteDetachRequestV2),
    ResyncRequired(RemoteResyncRequired),
    Close(RemoteCloseReason),
    Unknown,
}

impl<'de> Deserialize<'de> for RemoteControlMessageV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TaggedControl {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            data: Option<JsonValue>,
        }

        let tagged = TaggedControl::deserialize(deserializer)?;
        let data = tagged.data.unwrap_or(JsonValue::Null);
        macro_rules! decode {
            ($ty:ty, $variant:ident) => {
                serde_json::from_value::<$ty>(data)
                    .map(Self::$variant)
                    .map_err(serde::de::Error::custom)
            };
        }
        match tagged.kind.as_str() {
            "hello" => decode!(RemoteHello, Hello),
            "server_info" => decode!(RemoteServerInfoV2, ServerInfo),
            "ping" => decode!(RemotePing, Ping),
            "pong" => decode!(RemotePing, Pong),
            "subscribe" => decode!(RemoteSubscribeRequestV2, Subscribe),
            "subscribed" => decode!(RemoteSubscriptionAcceptedV2, Subscribed),
            "attach" => decode!(RemoteAttachRequestV2, Attach),
            "attached" => decode!(RemoteAttachmentAcceptedV2, Attached),
            "detach" => decode!(RemoteDetachRequestV2, Detach),
            "detached" => decode!(RemoteDetachRequestV2, Detached),
            "resync_required" => decode!(RemoteResyncRequired, ResyncRequired),
            "close" => decode!(RemoteCloseReason, Close),
            _ => Ok(Self::Unknown),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMutationContract {
    pub idempotency_key: String,
    pub expected_revision: Option<String>,
    pub expected_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRpcRequestV2 {
    pub request_id: RequestId,
    pub correlation_id: Option<CorrelationId>,
    pub operation: String,
    #[serde(default)]
    pub timeout_class: RemoteTimeoutClass,
    pub mutation: Option<RemoteMutationContract>,
    pub payload: Option<JsonValue>,
    pub created_at_ms: i64,
}

impl RemoteRpcRequestV2 {
    pub fn new(operation: RemoteOperationKind, payload: Option<JsonValue>) -> Self {
        let operation = serde_json::to_value(operation)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unsupported".to_string());
        Self {
            request_id: RequestId::new(),
            correlation_id: None,
            operation,
            timeout_class: RemoteTimeoutClass::Standard,
            mutation: None,
            payload,
            created_at_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProtocolError {
    pub error: VibexError,
    pub retryable: bool,
    pub retry_after_ms: Option<u32>,
}

impl RemoteProtocolError {
    pub fn from_error(error: VibexError) -> Self {
        let retryable = matches!(
            error.category,
            ErrorCategory::Process | ErrorCategory::Storage | ErrorCategory::Remote
        );
        Self {
            error,
            retryable,
            retry_after_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRpcResultMetadata {
    pub revision: Option<String>,
    pub generation: Option<u64>,
    pub cursor: Option<u64>,
    pub resync_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRpcResponseV2 {
    pub request_id: RequestId,
    pub correlation_id: Option<CorrelationId>,
    pub payload: Option<JsonValue>,
    pub error: Option<RemoteProtocolError>,
    #[serde(default)]
    pub metadata: RemoteRpcResultMetadata,
    pub completed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEventV2 {
    pub event_id: EventId,
    pub channel: String,
    pub generation: u64,
    pub sequence: u64,
    pub correlation_id: Option<CorrelationId>,
    pub payload: Option<JsonValue>,
    pub emitted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum RemoteJsonMessageV2 {
    Control(RemoteControlMessageV2),
    RpcRequest(RemoteRpcRequestV2),
    RpcResponse(RemoteRpcResponseV2),
    Event(RemoteEventV2),
    Unknown,
}

impl<'de> Deserialize<'de> for RemoteJsonMessageV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TaggedMessage {
            kind: String,
            #[serde(default)]
            body: Option<JsonValue>,
        }

        let tagged = TaggedMessage::deserialize(deserializer)?;
        let body = tagged.body.unwrap_or(JsonValue::Null);
        macro_rules! decode {
            ($ty:ty, $variant:ident) => {
                serde_json::from_value::<$ty>(body)
                    .map(Self::$variant)
                    .map_err(serde::de::Error::custom)
            };
        }
        match tagged.kind.as_str() {
            "control" => decode!(RemoteControlMessageV2, Control),
            "rpc_request" => decode!(RemoteRpcRequestV2, RpcRequest),
            "rpc_response" => decode!(RemoteRpcResponseV2, RpcResponse),
            "event" => decode!(RemoteEventV2, Event),
            _ => Ok(Self::Unknown),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteBinaryFrameKind {
    TerminalInput,
    TerminalOutput,
    TerminalSnapshot,
    FileUploadChunk,
    FileDownloadChunk,
    FileSnapshot,
    Unknown,
}

impl_unknown_safe_enum!(
    RemoteBinaryFrameKind,
    "terminal_input" => RemoteBinaryFrameKind::TerminalInput,
    "terminal_output" => RemoteBinaryFrameKind::TerminalOutput,
    "terminal_snapshot" => RemoteBinaryFrameKind::TerminalSnapshot,
    "file_upload_chunk" => RemoteBinaryFrameKind::FileUploadChunk,
    "file_download_chunk" => RemoteBinaryFrameKind::FileDownloadChunk,
    "file_snapshot" => RemoteBinaryFrameKind::FileSnapshot,
    "unknown" => RemoteBinaryFrameKind::Unknown,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBinaryFrameHeader {
    pub protocol_version: crate::RemoteProtocolVersion,
    pub kind: RemoteBinaryFrameKind,
    pub stream_id: String,
    pub request_id: Option<RequestId>,
    pub generation: u64,
    pub sequence: u64,
    pub offset: u64,
    pub total_size: Option<u64>,
    pub snapshot: bool,
    pub end_of_stream: bool,
    pub checksum_sha256: Option<String>,
    pub payload_length: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RemoteBinaryFrame {
    pub header: RemoteBinaryFrameHeader,
    pub payload: Vec<u8>,
}

impl fmt::Debug for RemoteBinaryFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteBinaryFrame")
            .field("kind", &self.header.kind)
            .field("stream_id", &self.header.stream_id)
            .field("generation", &self.header.generation)
            .field("sequence", &self.header.sequence)
            .field("payload_length", &self.payload.len())
            .finish()
    }
}

impl RemoteBinaryFrame {
    pub fn encode(mut self) -> Result<Vec<u8>, VibexError> {
        if self.payload.len() > REMOTE_V2_MAX_BINARY_PAYLOAD_BYTES {
            return Err(VibexError::validation(
                "remote_binary_payload_too_large",
                "remote binary payload exceeds the frame limit",
            ));
        }
        self.header.payload_length = u32::try_from(self.payload.len()).map_err(|_| {
            VibexError::validation(
                "remote_binary_payload_too_large",
                "remote binary payload length is invalid",
            )
        })?;
        let header = serde_json::to_vec(&self.header).map_err(|_| {
            VibexError::validation(
                "remote_binary_header_invalid",
                "remote binary header could not be encoded",
            )
        })?;
        if header.len() > REMOTE_V2_MAX_BINARY_HEADER_BYTES {
            return Err(VibexError::validation(
                "remote_binary_header_too_large",
                "remote binary header exceeds the frame limit",
            ));
        }
        let header_length = u32::try_from(header.len()).map_err(|_| {
            VibexError::validation(
                "remote_binary_header_too_large",
                "remote binary header length is invalid",
            )
        })?;
        let mut encoded = Vec::with_capacity(8 + header.len() + self.payload.len());
        encoded.extend_from_slice(&REMOTE_V2_BINARY_MAGIC);
        encoded.extend_from_slice(&header_length.to_be_bytes());
        encoded.extend_from_slice(&header);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, VibexError> {
        if encoded.len() < 8 || encoded[..4] != REMOTE_V2_BINARY_MAGIC {
            return Err(VibexError::validation(
                "remote_binary_magic_invalid",
                "remote binary frame has an invalid magic prefix",
            ));
        }
        let header_length = u32::from_be_bytes(encoded[4..8].try_into().map_err(|_| {
            VibexError::validation(
                "remote_binary_header_invalid",
                "remote binary frame header length is invalid",
            )
        })?) as usize;
        if header_length == 0 || header_length > REMOTE_V2_MAX_BINARY_HEADER_BYTES {
            return Err(VibexError::validation(
                "remote_binary_header_invalid",
                "remote binary frame header length is out of range",
            ));
        }
        let payload_offset = 8usize.checked_add(header_length).ok_or_else(|| {
            VibexError::validation(
                "remote_binary_header_invalid",
                "remote binary frame header length overflowed",
            )
        })?;
        if payload_offset > encoded.len() {
            return Err(VibexError::validation(
                "remote_binary_truncated",
                "remote binary frame is truncated",
            ));
        }
        let header: RemoteBinaryFrameHeader = serde_json::from_slice(&encoded[8..payload_offset])
            .map_err(|_| {
            VibexError::validation(
                "remote_binary_header_invalid",
                "remote binary frame header is invalid",
            )
        })?;
        let payload = encoded[payload_offset..].to_vec();
        if payload.len() > REMOTE_V2_MAX_BINARY_PAYLOAD_BYTES
            || usize::try_from(header.payload_length).ok() != Some(payload.len())
        {
            return Err(VibexError::validation(
                "remote_binary_payload_length_mismatch",
                "remote binary frame payload length does not match its header",
            ));
        }
        if header.protocol_version.major != REMOTE_PROTOCOL_V2_MAJOR {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_protocol_version_unsupported",
                "remote binary frame protocol version is unsupported",
            ));
        }
        Ok(Self { header, payload })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemotePairingTransport {
    Direct,
    Tailnet,
    SelfHostedRelay,
    Unknown,
}

impl_unknown_safe_enum!(
    RemotePairingTransport,
    "direct" => RemotePairingTransport::Direct,
    "tailnet" => RemotePairingTransport::Tailnet,
    "self_hosted_relay" => RemotePairingTransport::SelfHostedRelay,
    "unknown" => RemotePairingTransport::Unknown,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingCandidate {
    pub transport: RemotePairingTransport,
    pub url: String,
    #[serde(default)]
    pub relay_room_id: Option<crate::RelayRoomId>,
    #[serde(default)]
    pub relay_pc_peer_id: Option<crate::RelayPeerId>,
    #[serde(default)]
    pub relay_pc_public_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingOfferSummary {
    pub format_version: u16,
    pub protocol_range: RemoteProtocolVersionRange,
    pub server_id: String,
    pub server_identity_public_key: String,
    pub offer_id: RequestId,
    pub expires_at_ms: i64,
    #[serde(default)]
    pub direct_candidates: Vec<RemotePairingCandidate>,
    pub relay_candidate: Option<RemotePairingCandidate>,
    pub permission_level: RemoteDevicePermissionLevel,
    #[serde(default)]
    pub granted_permissions: Vec<RemoteActionClass>,
    pub canceled: bool,
    pub claimed_device_id: Option<DeviceId>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingOffer {
    #[serde(flatten)]
    pub summary: RemotePairingOfferSummary,
    pub one_time_challenge: String,
}

impl fmt::Debug for RemotePairingOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemotePairingOffer")
            .field("summary", &self.summary)
            .field(
                "has_one_time_challenge",
                &!self.one_time_challenge.is_empty(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePairingOfferRequest {
    pub permission_level: RemoteDevicePermissionLevel,
    pub ttl_ms: Option<u32>,
    #[serde(default)]
    pub direct_candidates: Vec<RemotePairingCandidate>,
    pub relay_candidate: Option<RemotePairingCandidate>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePairingOfferResponse {
    pub offer: RemotePairingOffer,
    pub launch_fragment: String,
}

impl fmt::Debug for RemoteCreatePairingOfferResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCreatePairingOfferResponse")
            .field("offer", &self.offer)
            .field("has_launch_fragment", &!self.launch_fragment.is_empty())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingOfferRequest {
    pub offer_id: RequestId,
    pub one_time_challenge: String,
    pub expected_server_id: String,
    pub expected_server_identity_public_key: String,
    pub display_name: String,
    pub device_identity_public_key: String,
    pub claim_nonce: String,
}

impl fmt::Debug for RemoteClaimPairingOfferRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteClaimPairingOfferRequest")
            .field("offer_id", &self.offer_id)
            .field("expected_server_id", &self.expected_server_id)
            .field("display_name", &self.display_name)
            .field(
                "has_device_identity",
                &!self.device_identity_public_key.is_empty(),
            )
            .field("has_claim_nonce", &!self.claim_nonce.is_empty())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingOfferResponse {
    pub device: RemoteDeviceDetail,
    pub device_grant_token: String,
    pub session_id: RequestId,
}

impl fmt::Debug for RemoteClaimPairingOfferResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteClaimPairingOfferResponse")
            .field("device", &self.device)
            .field(
                "has_device_grant_token",
                &!self.device_grant_token.is_empty(),
            )
            .field("session_id", &self.session_id)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCancelPairingOfferRequest {
    pub offer_id: RequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeviceOperationKind {
    CreatePairingOffer,
    CancelPairingOffer,
    ListDevices,
    RevokeDevice,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceCreatePairingOfferRequest {
    pub auth: RemoteAuthProof,
    pub request: RemoteCreatePairingOfferRequest,
}

impl fmt::Debug for RemoteDeviceCreatePairingOfferRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceCreatePairingOfferRequest")
            .field("auth", &self.auth)
            .field("permission_level", &self.request.permission_level)
            .field("ttl_ms", &self.request.ttl_ms)
            .field(
                "direct_candidate_count",
                &self.request.direct_candidates.len(),
            )
            .field(
                "has_relay_candidate",
                &self.request.relay_candidate.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceCancelPairingOfferRequest {
    pub auth: RemoteAuthProof,
    pub request: RemoteCancelPairingOfferRequest,
}

impl fmt::Debug for RemoteDeviceCancelPairingOfferRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceCancelPairingOfferRequest")
            .field("auth", &self.auth)
            .field("offer_id", &self.request.offer_id)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceListRequest {
    pub auth: RemoteAuthProof,
}

impl fmt::Debug for RemoteDeviceListRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceListRequest")
            .field("auth", &self.auth)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceListResponse {
    pub devices: Vec<RemoteDeviceDetail>,
}

impl fmt::Debug for RemoteDeviceListResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceListResponse")
            .field("device_count", &self.devices.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceRevokeRequest {
    pub auth: RemoteAuthProof,
    pub request: RemoteRevokeDeviceRequest,
}

impl fmt::Debug for RemoteDeviceRevokeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceRevokeRequest")
            .field("auth", &self.auth)
            .field("device_id", &self.request.device_id)
            .field("has_reason", &self.request.reason.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteDeviceRequest {
    CreatePairingOffer(RemoteDeviceCreatePairingOfferRequest),
    CancelPairingOffer(RemoteDeviceCancelPairingOfferRequest),
    ListDevices(RemoteDeviceListRequest),
    RevokeDevice(RemoteDeviceRevokeRequest),
}

impl RemoteDeviceRequest {
    pub const fn operation_kind(&self) -> RemoteDeviceOperationKind {
        match self {
            Self::CreatePairingOffer(_) => RemoteDeviceOperationKind::CreatePairingOffer,
            Self::CancelPairingOffer(_) => RemoteDeviceOperationKind::CancelPairingOffer,
            Self::ListDevices(_) => RemoteDeviceOperationKind::ListDevices,
            Self::RevokeDevice(_) => RemoteDeviceOperationKind::RevokeDevice,
        }
    }
}

impl fmt::Debug for RemoteDeviceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteDeviceRequest")
            .field("operation", &self.operation_kind())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWsTicketRequest {
    pub auth: crate::RemoteAuthProof,
}

impl fmt::Debug for RemoteWsTicketRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteWsTicketRequest")
            .field("auth", &self.auth)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWsTicketResponse {
    pub ticket: String,
    pub subprotocol: String,
    pub proof_challenge: String,
    pub expires_at_ms: i64,
}

impl fmt::Debug for RemoteWsTicketResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteWsTicketResponse")
            .field("has_ticket", &!self.ticket.is_empty())
            .field("subprotocol_prefix", &"vibex-ticket")
            .field("has_proof_challenge", &!self.proof_challenge.is_empty())
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

pub fn remote_permissions_for_level(
    permission_level: RemoteDevicePermissionLevel,
) -> Vec<RemoteActionClass> {
    let mut permissions = vec![
        RemoteActionClass::ReadProject,
        RemoteActionClass::ReadAgentSession,
        RemoteActionClass::ReadProviderSettings,
    ];
    if matches!(
        permission_level,
        RemoteDevicePermissionLevel::ApproveOnly | RemoteDevicePermissionLevel::FullControl
    ) {
        permissions.push(RemoteActionClass::ResolvePermission);
    }
    if permission_level == RemoteDevicePermissionLevel::FullControl {
        permissions.extend([
            RemoteActionClass::MutateAgentSession,
            RemoteActionClass::MutateAgentAuthentication,
            RemoteActionClass::MutateFile,
            RemoteActionClass::MutateGit,
            RemoteActionClass::MutateTerminal,
            RemoteActionClass::MutateProviderSettings,
            RemoteActionClass::ReadDeviceManagement,
            RemoteActionClass::MutateDeviceManagement,
        ]);
    }
    permissions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2() -> crate::RemoteProtocolVersion {
        crate::RemoteProtocolVersion {
            major: REMOTE_PROTOCOL_V2_MAJOR,
            minor: REMOTE_PROTOCOL_V2_MINOR,
        }
    }

    #[test]
    fn version_range_negotiates_highest_common_version() {
        let selected = RemoteProtocolVersionRange {
            min: crate::RemoteProtocolVersion { major: 1, minor: 5 },
            max: crate::RemoteProtocolVersion { major: 2, minor: 4 },
        }
        .negotiate(RemoteProtocolVersionRange {
            min: crate::RemoteProtocolVersion { major: 2, minor: 0 },
            max: crate::RemoteProtocolVersion { major: 3, minor: 0 },
        });

        assert_eq!(
            selected,
            Some(crate::RemoteProtocolVersion { major: 2, minor: 4 })
        );
        assert!(
            RemoteProtocolVersionRange::v2()
                .negotiate(RemoteProtocolVersionRange {
                    min: crate::RemoteProtocolVersion { major: 3, minor: 0 },
                    max: crate::RemoteProtocolVersion { major: 3, minor: 1 },
                })
                .is_none()
        );
    }

    #[test]
    fn hello_and_rpc_shapes_are_stable_golden_json() {
        let device_id = DeviceId::parse("device_phone").unwrap();
        let hello = RemoteJsonMessageV2::Control(RemoteControlMessageV2::Hello(RemoteHello {
            client_id: "phone".to_string(),
            client_type: RemoteClientType::Mobile,
            app_version: "1.2.3".to_string(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id,
            device_identity_public_key: "device-static-public".to_string(),
            client_ephemeral_public_key: "client-ephemeral-public".to_string(),
            identity_proof: "identity-proof-secret".to_string(),
            relay_auth: None,
            transport_endpoint: None,
            permission_context_hash: None,
            capabilities: vec!["rpc".to_string(), "binary_terminal".to_string()],
            enabled_features: vec!["agent".to_string()],
            last_session_epoch: Some(7),
            cursors: vec![RemoteStreamCursor {
                domain: "agent".to_string(),
                generation: 2,
                cursor: 9,
            }],
        }));
        let encoded = serde_json::to_string(&hello).unwrap();
        assert_eq!(
            encoded,
            r#"{"kind":"control","body":{"type":"hello","data":{"clientId":"phone","clientType":"mobile","appVersion":"1.2.3","protocolRange":{"min":{"major":2,"minor":0},"max":{"major":2,"minor":0}},"deviceId":"device_phone","deviceIdentityPublicKey":"device-static-public","clientEphemeralPublicKey":"client-ephemeral-public","identityProof":"identity-proof-secret","capabilities":["rpc","binary_terminal"],"enabledFeatures":["agent"],"lastSessionEpoch":7,"cursors":[{"domain":"agent","generation":2,"cursor":9}]}}}"#
        );

        let request = RemoteJsonMessageV2::RpcRequest(RemoteRpcRequestV2 {
            request_id: RequestId::parse("request_rpc").unwrap(),
            correlation_id: None,
            operation: "git".to_string(),
            timeout_class: RemoteTimeoutClass::LongRunning,
            mutation: Some(RemoteMutationContract {
                idempotency_key: "commit-1".to_string(),
                expected_revision: Some("rev-4".to_string()),
                expected_generation: Some(3),
            }),
            payload: Some(serde_json::json!({"type": "git_commit"})),
            created_at_ms: 10,
        });
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"kind":"rpc_request","body":{"requestId":"request_rpc","correlationId":null,"operation":"git","timeoutClass":"long_running","mutation":{"idempotencyKey":"commit-1","expectedRevision":"rev-4","expectedGeneration":3},"payload":{"type":"git_commit"},"createdAtMs":10}}"#
        );
    }

    #[test]
    fn unknown_client_control_and_binary_kinds_decode_without_crashing() {
        let client: RemoteClientType = serde_json::from_str(r#""future_client""#).unwrap();
        assert_eq!(client, RemoteClientType::Unknown);

        let control: RemoteControlMessageV2 =
            serde_json::from_str(r#"{"type":"future_control","data":{"x":1}}"#).unwrap();
        assert_eq!(control, RemoteControlMessageV2::Unknown);

        let kind: RemoteBinaryFrameKind = serde_json::from_str(r#""future_binary""#).unwrap();
        assert_eq!(kind, RemoteBinaryFrameKind::Unknown);
    }

    #[test]
    fn binary_frame_round_trip_has_stable_prefix_and_redacted_debug() {
        let frame = RemoteBinaryFrame {
            header: RemoteBinaryFrameHeader {
                protocol_version: v2(),
                kind: RemoteBinaryFrameKind::TerminalOutput,
                stream_id: "terminal_demo".to_string(),
                request_id: Some(RequestId::parse("request_binary").unwrap()),
                generation: 4,
                sequence: 12,
                offset: 0,
                total_size: None,
                snapshot: false,
                end_of_stream: false,
                checksum_sha256: None,
                payload_length: 0,
            },
            payload: b"secret-terminal-output".to_vec(),
        };
        let encoded = frame.clone().encode().unwrap();
        assert_eq!(&encoded[..4], b"VBX2");
        let decoded = RemoteBinaryFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.payload, b"secret-terminal-output");
        assert_eq!(decoded.header.sequence, 12);
        let debug = format!("{decoded:?}");
        assert!(!debug.contains("secret-terminal-output"));
        assert!(debug.contains("payload_length"));
    }

    #[test]
    fn pairing_and_ticket_debug_output_redacts_secret_material() {
        let offer = RemotePairingOffer {
            summary: RemotePairingOfferSummary {
                format_version: 1,
                protocol_range: RemoteProtocolVersionRange::v2(),
                server_id: "server-test".to_string(),
                server_identity_public_key: "server-public".to_string(),
                offer_id: RequestId::parse("request_offer").unwrap(),
                expires_at_ms: 100,
                direct_candidates: vec![],
                relay_candidate: None,
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                granted_permissions: remote_permissions_for_level(
                    RemoteDevicePermissionLevel::ReadOnly,
                ),
                canceled: false,
                claimed_device_id: None,
            },
            one_time_challenge: "pairing-challenge-secret".to_string(),
        };
        assert!(!format!("{offer:?}").contains("pairing-challenge-secret"));

        let ticket = RemoteWsTicketResponse {
            ticket: "ws-ticket-secret".to_string(),
            subprotocol: "vibex-ticket.ws-ticket-secret".to_string(),
            proof_challenge: "proof-challenge-secret".to_string(),
            expires_at_ms: 200,
        };
        assert!(!format!("{ticket:?}").contains("ws-ticket-secret"));
        assert!(!format!("{ticket:?}").contains("proof-challenge-secret"));

        let hello = RemoteHello {
            client_id: "browser".to_string(),
            client_type: RemoteClientType::Browser,
            app_version: "test".to_string(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id: DeviceId::parse("device_debug").unwrap(),
            device_identity_public_key: "identity-public".to_string(),
            client_ephemeral_public_key: "ephemeral-public".to_string(),
            identity_proof: "identity-proof-secret".to_string(),
            relay_auth: None,
            transport_endpoint: None,
            permission_context_hash: None,
            capabilities: Vec::new(),
            enabled_features: Vec::new(),
            last_session_epoch: None,
            cursors: Vec::new(),
        };
        assert!(!format!("{hello:?}").contains("identity-proof-secret"));
    }

    #[test]
    fn device_management_debug_omits_auth_routes_and_revoke_reason() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::parse("device_admin").unwrap(),
            auth_token: "device-auth-token-sentinel".to_string(),
        };
        let create = RemoteDeviceCreatePairingOfferRequest {
            auth: auth.clone(),
            request: RemoteCreatePairingOfferRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(60_000),
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "https://private-route.invalid/sentinel".to_string(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }],
                relay_candidate: None,
            },
        };
        let revoke = RemoteDeviceRevokeRequest {
            auth,
            request: RemoteRevokeDeviceRequest {
                device_id: DeviceId::parse("device_target").unwrap(),
                reason: Some("private-revoke-reason-sentinel".to_string()),
            },
        };
        for debug in [
            format!("{create:?}"),
            format!("{:?}", RemoteDeviceRequest::CreatePairingOffer(create)),
            format!("{revoke:?}"),
            format!("{:?}", RemoteDeviceRequest::RevokeDevice(revoke)),
        ] {
            assert!(!debug.contains("device-auth-token-sentinel"));
            assert!(!debug.contains("private-route.invalid"));
            assert!(!debug.contains("private-revoke-reason-sentinel"));
        }
    }
}
