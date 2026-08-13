use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::ids::{
    CorrelationId, DeviceId, RelayConnectionId, RelayFrameId, RelayPeerId, RelayRoomId,
    RelaySessionId,
};
use crate::time::unix_timestamp_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl RelayProtocolVersion {
    pub const fn foundation() -> Self {
        Self { major: 0, minor: 5 }
    }
}

impl Default for RelayProtocolVersion {
    fn default() -> Self {
        Self::foundation()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFrameKind {
    PairRequest,
    PairResponse,
    Command,
    Response,
    Event,
    Heartbeat,
    HeartbeatAck,
    Error,
}

/// Identifies the endpoint role on the Relay WebSocket.  The Relay uses this
/// only for connection routing; it is never a business authorization grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelayPeerRole {
    #[default]
    Pc,
    Device,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelayTransportMode {
    #[default]
    HttpBridge,
    WebSocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    UnsupportedProtocol,
    InvalidFrame,
    InvalidRoom,
    InvalidCorrelation,
    CryptoSetupFailed,
    DecryptFailed,
    ReplayDetected,
    FrameOutOfOrder,
    ConnectionLimit,
    QueueLimit,
    RateLimit,
    BandwidthLimit,
    PeerNotFound,
    SessionRevoked,
    PushAuthenticationRequired,
    PushProviderUnavailable,
}

impl RelayErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedProtocol => "relay_unsupported_protocol",
            Self::InvalidFrame => "relay_invalid_frame",
            Self::InvalidRoom => "relay_invalid_room",
            Self::InvalidCorrelation => "relay_invalid_correlation",
            Self::CryptoSetupFailed => "relay_crypto_setup_failed",
            Self::DecryptFailed => "relay_decrypt_failed",
            Self::ReplayDetected => "relay_replay_detected",
            Self::FrameOutOfOrder => "relay_frame_out_of_order",
            Self::ConnectionLimit => "relay_connection_limit",
            Self::QueueLimit => "relay_queue_limit",
            Self::RateLimit => "relay_rate_limit",
            Self::BandwidthLimit => "relay_bandwidth_limit",
            Self::PeerNotFound => "relay_peer_not_found",
            Self::SessionRevoked => "relay_session_revoked",
            Self::PushAuthenticationRequired => "relay_push_authentication_required",
            Self::PushProviderUnavailable => "relay_push_provider_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHandshakeHello {
    pub protocol_version: RelayProtocolVersion,
    pub room_id: RelayRoomId,
    pub peer_id: RelayPeerId,
    pub public_key: String,
    pub supported_versions: Vec<RelayProtocolVersion>,
    /// Added in the full-duplex transport.  Missing values decode as the
    /// legacy PC registration shape so old clients can still use the bridge.
    #[serde(default)]
    pub role: RelayPeerRole,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Explicitly negotiates the E2EE transcript/key schedule.  `None` is
    /// retained for legacy v1 HTTP bridge clients.
    #[serde(default)]
    pub crypto_suite: Option<String>,
    /// Device identity hint used only inside the PC-authenticated handshake.
    /// The Relay may route it but must never treat it as authorization.
    #[serde(default)]
    pub remote_device_id: Option<DeviceId>,
    #[serde(default)]
    pub remote_device_identity_public_key: Option<String>,
    /// Fresh per-connection X25519 public key.  The long-lived `public_key`
    /// remains the pairing identity; this value is used only for the v2
    /// session key schedule.
    #[serde(default)]
    pub ephemeral_public_key: Option<String>,
    /// HMAC proof over the handshake transcript using the paired static
    /// identity.  Relay servers forward it without interpreting it.
    #[serde(default)]
    pub ephemeral_proof: Option<String>,
    pub timestamp_ms: i64,
}

impl RelayHandshakeHello {
    pub fn new(room_id: RelayRoomId, peer_id: RelayPeerId, public_key: impl Into<String>) -> Self {
        Self {
            protocol_version: RelayProtocolVersion::foundation(),
            room_id,
            peer_id,
            public_key: public_key.into(),
            supported_versions: vec![RelayProtocolVersion::foundation()],
            role: RelayPeerRole::Pc,
            endpoint: None,
            capabilities: vec!["http_bridge".to_string()],
            crypto_suite: None,
            remote_device_id: None,
            remote_device_identity_public_key: None,
            ephemeral_public_key: None,
            ephemeral_proof: None,
            timestamp_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHandshakeReady {
    pub protocol_version: RelayProtocolVersion,
    pub selected_version: RelayProtocolVersion,
    pub room_id: RelayRoomId,
    pub session_id: RelaySessionId,
    pub peer_id: RelayPeerId,
    pub public_key: String,
    #[serde(default)]
    pub role: RelayPeerRole,
    #[serde(default)]
    pub transport_mode: RelayTransportMode,
    #[serde(default)]
    pub transcript_hash: Option<String>,
    /// Echoes the negotiated E2EE suite.  A missing value means legacy bridge
    /// compatibility mode and must not be interpreted as v2.
    #[serde(default)]
    pub crypto_suite: Option<String>,
    /// PC-created single-connection challenge used by the inner Remote v2
    /// identity transcript. It is opaque to the Relay server.
    #[serde(default)]
    pub proof_challenge: Option<String>,
    /// Current authoritative Remote v2 session epoch.
    #[serde(default)]
    pub remote_session_epoch: Option<u64>,
    /// Pinned desktop Remote v2 identity. This may differ from the ephemeral
    /// Relay encryption key and is verified again by the inner handshake.
    #[serde(default)]
    pub desktop_identity_public_key: Option<String>,
    #[serde(default)]
    pub permission_context_hash: Option<String>,
    #[serde(default)]
    pub ephemeral_public_key: Option<String>,
    #[serde(default)]
    pub ephemeral_proof: Option<String>,
    pub timestamp_ms: i64,
}

impl RelayHandshakeReady {
    pub fn new(
        room_id: RelayRoomId,
        session_id: RelaySessionId,
        peer_id: RelayPeerId,
        public_key: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: RelayProtocolVersion::foundation(),
            selected_version: RelayProtocolVersion::foundation(),
            room_id,
            session_id,
            peer_id,
            public_key: public_key.into(),
            role: RelayPeerRole::Pc,
            transport_mode: RelayTransportMode::HttpBridge,
            transcript_hash: None,
            crypto_suite: None,
            proof_challenge: None,
            remote_session_epoch: None,
            desktop_identity_public_key: None,
            permission_context_hash: None,
            ephemeral_public_key: None,
            ephemeral_proof: None,
            timestamp_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEncryptedFrame {
    pub protocol_version: RelayProtocolVersion,
    pub room_id: RelayRoomId,
    pub session_id: RelaySessionId,
    pub frame_id: RelayFrameId,
    pub sender_peer_id: RelayPeerId,
    pub recipient_peer_id: RelayPeerId,
    pub correlation_id: Option<CorrelationId>,
    pub kind: RelayFrameKind,
    pub nonce: String,
    pub ciphertext: String,
    pub counter: u64,
    pub created_at_ms: i64,
}

impl fmt::Debug for RelayEncryptedFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayEncryptedFrame")
            .field("protocol_version", &self.protocol_version)
            .field("room_id", &self.room_id)
            .field("session_id", &self.session_id)
            .field("frame_id", &self.frame_id)
            .field("sender_peer_id", &self.sender_peer_id)
            .field("recipient_peer_id", &self.recipient_peer_id)
            .field("correlation_id", &self.correlation_id)
            .field("kind", &self.kind)
            .field("nonce", &"<redacted>")
            .field("ciphertext", &"<redacted>")
            .field("counter", &self.counter)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPlaintextEnvelope {
    pub room_id: RelayRoomId,
    pub session_id: RelaySessionId,
    pub sender_peer_id: RelayPeerId,
    pub recipient_peer_id: RelayPeerId,
    pub correlation_id: Option<CorrelationId>,
    pub kind: RelayFrameKind,
    pub counter: u64,
    pub issued_at_ms: i64,
    pub business_payload_json: JsonValue,
}

impl fmt::Debug for RelayPlaintextEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayPlaintextEnvelope")
            .field("room_id", &self.room_id)
            .field("session_id", &self.session_id)
            .field("sender_peer_id", &self.sender_peer_id)
            .field("recipient_peer_id", &self.recipient_peer_id)
            .field("correlation_id", &self.correlation_id)
            .field("kind", &self.kind)
            .field("counter", &self.counter)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("business_payload_json", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHeartbeat {
    pub room_id: RelayRoomId,
    pub peer_id: RelayPeerId,
    pub connection_id: Option<RelayConnectionId>,
    pub sequence: u64,
    pub sent_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHeartbeatAck {
    pub room_id: RelayRoomId,
    pub peer_id: RelayPeerId,
    pub connection_id: Option<RelayConnectionId>,
    pub sequence: u64,
    pub acknowledged_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayError {
    pub code: RelayErrorCode,
    pub message: String,
    pub correlation_id: Option<CorrelationId>,
    pub retryable: bool,
    pub created_at_ms: i64,
}

impl RelayError {
    pub fn new(code: RelayErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            correlation_id: None,
            retryable: false,
            created_at_ms: unix_timestamp_ms(),
        }
    }

    pub fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    pub const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RelayControlMessage {
    Hello(RelayHandshakeHello),
    Ready(RelayHandshakeReady),
    Encrypted(RelayEncryptedFrame),
    Heartbeat(RelayHeartbeat),
    HeartbeatAck(RelayHeartbeatAck),
    Error(RelayError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayBridgeMessage {
    pub correlation_id: CorrelationId,
    pub room_id: RelayRoomId,
    pub message: RelayControlMessage,
}

/// Full-duplex peer envelope.  The Relay uses only these routing fields and
/// forwards `message` without parsing business payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPeerMessage {
    pub room_id: RelayRoomId,
    pub sender_peer_id: RelayPeerId,
    pub recipient_peer_id: RelayPeerId,
    pub message: RelayControlMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayNotificationProviderKind {
    WebPush,
    Apns,
    Fcm,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPushRegistration {
    pub installation_id: String,
    pub provider: RelayNotificationProviderKind,
    pub provider_token: String,
}

impl fmt::Debug for RelayPushRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayPushRegistration")
            .field("has_installation_id", &!self.installation_id.is_empty())
            .field("provider", &self.provider)
            .field("has_provider_token", &!self.provider_token.is_empty())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayOpaqueNotification {
    pub notification_id: String,
    pub installation_id: String,
    pub opaque_locator: String,
    pub expires_at_ms: i64,
    pub ciphertext: Option<String>,
}

impl fmt::Debug for RelayOpaqueNotification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayOpaqueNotification")
            .field("has_notification_id", &!self.notification_id.is_empty())
            .field("has_installation_id", &!self.installation_id.is_empty())
            .field("has_opaque_locator", &!self.opaque_locator.is_empty())
            .field("expires_at_ms", &self.expires_at_ms)
            .field("has_ciphertext", &self.ciphertext.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPushDispatchResult {
    pub accepted: bool,
    pub provider_configured: bool,
    pub duplicate: bool,
    pub expires_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayDeepLink {
    pub notification_id: String,
    pub opaque_locator: String,
    pub expires_at_ms: i64,
}

impl fmt::Debug for RelayDeepLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayDeepLink")
            .field("has_notification_id", &!self.notification_id.is_empty())
            .field("has_opaque_locator", &!self.opaque_locator.is_empty())
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayRemoteHandshakeContext {
    pub device_id: DeviceId,
    pub device_identity_public_key: String,
    pub desktop_identity_public_key: String,
    pub proof_challenge: String,
    pub session_epoch: u64,
    pub permission_context_hash: String,
    pub transport_endpoint: String,
}

impl fmt::Debug for RelayRemoteHandshakeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayRemoteHandshakeContext")
            .field("device_id", &self.device_id)
            .field(
                "has_device_identity_public_key",
                &!self.device_identity_public_key.is_empty(),
            )
            .field(
                "has_desktop_identity_public_key",
                &!self.desktop_identity_public_key.is_empty(),
            )
            .field("has_proof_challenge", &!self.proof_challenge.is_empty())
            .field("session_epoch", &self.session_epoch)
            .field(
                "has_permission_context_hash",
                &!self.permission_context_hash.is_empty(),
            )
            .field("transport_endpoint", &self.transport_endpoint)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_control_message_serializes_with_stable_tag() {
        let hello = RelayControlMessage::Hello(RelayHandshakeHello::new(
            RelayRoomId::new(),
            RelayPeerId::new(),
            "public-key",
        ));

        let json = serde_json::to_value(&hello).unwrap();

        assert_eq!(json["type"], "hello");
        assert_eq!(json["data"]["protocolVersion"]["major"], 0);
        assert_eq!(json["data"]["protocolVersion"]["minor"], 5);
    }

    #[test]
    fn relay_encrypted_frame_does_not_debug_or_serialize_plaintext() {
        let frame = RelayEncryptedFrame {
            protocol_version: RelayProtocolVersion::foundation(),
            room_id: RelayRoomId::new(),
            session_id: RelaySessionId::new(),
            frame_id: RelayFrameId::new(),
            sender_peer_id: RelayPeerId::new(),
            recipient_peer_id: RelayPeerId::new(),
            correlation_id: Some(CorrelationId::new()),
            kind: RelayFrameKind::Command,
            nonce: "nonce-secret".to_string(),
            ciphertext: "ciphertext-secret".to_string(),
            counter: 1,
            created_at_ms: 2,
        };

        let serialized = serde_json::to_string(&frame).unwrap();
        let debug = format!("{frame:?}");

        assert!(serialized.contains("ciphertext-secret"));
        assert!(!serialized.contains("sample prompt body"));
        assert!(!debug.contains("ciphertext-secret"));
        assert!(!debug.contains("nonce-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn relay_plaintext_debug_redacts_business_payload() {
        let plaintext = RelayPlaintextEnvelope {
            room_id: RelayRoomId::new(),
            session_id: RelaySessionId::new(),
            sender_peer_id: RelayPeerId::new(),
            recipient_peer_id: RelayPeerId::new(),
            correlation_id: None,
            kind: RelayFrameKind::Command,
            counter: 1,
            issued_at_ms: 2,
            business_payload_json: serde_json::json!({"prompt": "sample prompt body"}),
        };

        let debug = format!("{plaintext:?}");

        assert!(!debug.contains("sample prompt body"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn relay_bridge_message_preserves_correlation_room_and_control_message() {
        let correlation_id = CorrelationId::new();
        let room_id = RelayRoomId::new();
        let bridge = RelayBridgeMessage {
            correlation_id: correlation_id.clone(),
            room_id: room_id.clone(),
            message: RelayControlMessage::Hello(RelayHandshakeHello::new(
                room_id.clone(),
                RelayPeerId::new(),
                "public-key",
            )),
        };

        let json = serde_json::to_value(&bridge).unwrap();
        let round_trip: RelayBridgeMessage = serde_json::from_value(json.clone()).unwrap();

        assert_eq!(json["correlationId"], correlation_id.as_str());
        assert_eq!(json["roomId"], room_id.as_str());
        assert_eq!(json["message"]["type"], "hello");
        assert_eq!(round_trip, bridge);
    }

    #[test]
    fn peer_message_preserves_opaque_routing_metadata() {
        let message = RelayPeerMessage {
            room_id: RelayRoomId::new(),
            sender_peer_id: RelayPeerId::new(),
            recipient_peer_id: RelayPeerId::new(),
            message: RelayControlMessage::Error(RelayError::new(
                RelayErrorCode::QueueLimit,
                "queue is full",
            )),
        };
        let encoded = serde_json::to_value(&message).unwrap();
        let decoded: RelayPeerMessage = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, message);
    }
}
