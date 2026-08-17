use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde_json::Value as JsonValue;
use sha2::Sha256;
use vibex_core::{
    ErrorCategory, REMOTE_LOCAL_LAN_TLS_HOSTNAME, REMOTE_ZERO_CONFIG_LAN_PAIRING_SCHEMA_VERSION,
    RelayEncryptedFrame, RelayFrameKind, RelayPeerId, RelayRoomId, RelaySessionId,
    RemoteLanPairingDiscoverySummary, RemoteZeroConfigLanPairingHello,
    RemoteZeroConfigLanPairingHelloAccepted, VibexError, VibexResult,
    remote_zero_config_lan_session_context,
};
use vibex_relay::{RelayCryptoSuite, RelayKeypair, RelaySession, RelaySessionConfig};

use super::RemoteIdentity;

pub(crate) const ZERO_CONFIG_PAIRING_HELLO_PATH: &str = "/api/v2/pairing/lan-zero/hello";
pub(crate) const ZERO_CONFIG_PAIRING_REQUEST_PATH: &str = "/api/v2/pairing/lan-zero/request";
pub(crate) const ZERO_CONFIG_PAIRING_STATUS_PATH: &str = "/api/v2/pairing/lan-zero/status";
pub(crate) const ZERO_CONFIG_PAIRING_CLAIM_PATH: &str = "/api/v2/pairing/lan-zero/claim";
pub(crate) const ZERO_CONFIG_PAIRING_SESSION_TTL: Duration = Duration::from_secs(120);

const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

pub(crate) struct LocalLanTlsIdentity {
    pub certificate_der: Vec<u8>,
    pub private_key_der: Vec<u8>,
    pub certificate_base64: String,
}

impl fmt::Debug for LocalLanTlsIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalLanTlsIdentity")
            .field("certificate_len", &self.certificate_der.len())
            .field("has_private_key", &true)
            .finish()
    }
}

pub(crate) fn derive_local_lan_tls_identity(
    identity: &RemoteIdentity,
) -> VibexResult<LocalLanTlsIdentity> {
    let hkdf = Hkdf::<Sha256>::new(
        Some(b"vibex.remote.local-lan-tls.v1"),
        &identity.private_key_bytes(),
    );
    let mut seed = [0u8; 32];
    hkdf.expand(identity.server_id().as_bytes(), &mut seed)
        .map_err(|_| {
            VibexError::storage(
                "remote_local_lan_tls_identity_failed",
                "failed to derive the local LAN TLS identity",
            )
        })?;
    let mut private_key_der = Vec::with_capacity(ED25519_PKCS8_PREFIX.len() + seed.len());
    private_key_der.extend_from_slice(&ED25519_PKCS8_PREFIX);
    private_key_der.extend_from_slice(&seed);
    let private_key = PrivatePkcs8KeyDer::from(private_key_der.clone());
    let key_pair =
        KeyPair::from_pkcs8_der_and_sign_algo(&private_key, &PKCS_ED25519).map_err(|_| {
            VibexError::storage(
                "remote_local_lan_tls_identity_failed",
                "failed to initialize the local LAN TLS identity",
            )
        })?;
    let params =
        CertificateParams::new(vec![REMOTE_LOCAL_LAN_TLS_HOSTNAME.to_string()]).map_err(|_| {
            VibexError::storage(
                "remote_local_lan_tls_certificate_failed",
                "failed to configure the local LAN TLS certificate",
            )
        })?;
    let certificate = params.self_signed(&key_pair).map_err(|_| {
        VibexError::storage(
            "remote_local_lan_tls_certificate_failed",
            "failed to generate the local LAN TLS certificate",
        )
    })?;
    let certificate_der = certificate.der().to_vec();
    Ok(LocalLanTlsIdentity {
        certificate_base64: URL_SAFE_NO_PAD.encode(&certificate_der),
        certificate_der,
        private_key_der,
    })
}

pub(crate) struct ZeroConfigLanSession {
    pub session_id: RelaySessionId,
    pub expires_at_ms: i64,
    pub session: RelaySession,
}

impl fmt::Debug for ZeroConfigLanSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZeroConfigLanSession")
            .field("session_id", &self.session_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("has_session", &true)
            .finish()
    }
}

pub(crate) fn establish_server_session(
    identity: &RemoteIdentity,
    discovery: RemoteLanPairingDiscoverySummary,
    lan_gateway_port: u16,
    lan_gateway_tls_certificate: String,
    hello: RemoteZeroConfigLanPairingHello,
    now_ms: i64,
) -> VibexResult<(
    RemoteZeroConfigLanPairingHelloAccepted,
    ZeroConfigLanSession,
)> {
    if hello.client_nonce.len() < 16
        || hello.client_nonce.len() > 256
        || !hello.client_nonce.is_ascii()
        || hello.client_nonce.chars().any(char::is_whitespace)
    {
        return Err(VibexError::validation(
            "remote_zero_config_pairing_hello_invalid",
            "zero-config LAN pairing hello nonce is invalid",
        ));
    }
    let client_key = URL_SAFE_NO_PAD
        .decode(&hello.client_ephemeral_public_key)
        .ok()
        .filter(|key| key.len() == 32)
        .ok_or_else(|| {
            VibexError::validation(
                "remote_zero_config_pairing_hello_invalid",
                "zero-config LAN pairing client key is invalid",
            )
        })?;
    if client_key.iter().all(|byte| *byte == 0) {
        return Err(VibexError::validation(
            "remote_zero_config_pairing_hello_invalid",
            "zero-config LAN pairing client key is invalid",
        ));
    }
    let client_relay_public_key = STANDARD.encode(client_key);
    let server_keypair = RelayKeypair::from_private_key_bytes(identity.private_key_bytes());
    if lan_gateway_port == 0 || lan_gateway_tls_certificate.is_empty() {
        return Err(VibexError::validation(
            "remote_local_lan_route_invalid",
            "zero-config pairing requires a valid local LAN Gateway route",
        ));
    }
    let session_context = remote_zero_config_lan_session_context(
        &hello.client_nonce,
        lan_gateway_port,
        &lan_gateway_tls_certificate,
    );
    let room_id = RelayRoomId::new();
    let session_id = RelaySessionId::new();
    let server_peer_id = RelayPeerId::new();
    let config = RelaySessionConfig {
        room_id: room_id.clone(),
        session_id: session_id.clone(),
        local_peer_id: server_peer_id.clone(),
        remote_peer_id: hello.client_peer_id.clone(),
    };
    let session = RelaySession::establish_with_suite(
        &server_keypair,
        &client_relay_public_key,
        config,
        RelayCryptoSuite::DirectionalV2,
        Some(&session_context),
    )
    .map_err(|_| {
        VibexError::new(
            ErrorCategory::Permission,
            "remote_zero_config_pairing_hello_rejected",
            "zero-config LAN pairing hello could not establish an encrypted session",
        )
    })?;
    let expires_at_ms = now_ms.saturating_add(
        i64::try_from(ZERO_CONFIG_PAIRING_SESSION_TTL.as_millis()).unwrap_or(120_000),
    );
    let accepted = RemoteZeroConfigLanPairingHelloAccepted {
        schema_version: REMOTE_ZERO_CONFIG_LAN_PAIRING_SCHEMA_VERSION.to_string(),
        session_id: session_id.clone(),
        room_id,
        client_peer_id: hello.client_peer_id,
        server_peer_id,
        server_id: identity.server_id().to_string(),
        server_identity_public_key: identity.public_key_base64(),
        lan_gateway_port,
        lan_gateway_tls_certificate,
        discovery,
    };
    Ok((
        accepted,
        ZeroConfigLanSession {
            session_id,
            expires_at_ms,
            session,
        },
    ))
}

pub(crate) fn open_json(
    session: &mut ZeroConfigLanSession,
    frame: &RelayEncryptedFrame,
    now_ms: i64,
) -> VibexResult<JsonValue> {
    if session.expires_at_ms <= now_ms || frame.session_id != session.session_id {
        return Err(VibexError::new(
            ErrorCategory::Permission,
            "remote_zero_config_pairing_session_expired",
            "zero-config LAN pairing session has expired",
        ));
    }
    session
        .session
        .open_json(frame)
        .map(|envelope| envelope.business_payload_json)
        .map_err(|_| {
            VibexError::new(
                ErrorCategory::Permission,
                "remote_zero_config_pairing_frame_invalid",
                "zero-config LAN pairing frame could not be decrypted",
            )
        })
}

pub(crate) fn seal_json(
    session: &mut ZeroConfigLanSession,
    payload: JsonValue,
) -> VibexResult<RelayEncryptedFrame> {
    session
        .session
        .seal_json(RelayFrameKind::PairResponse, None, payload)
        .map_err(|_| {
            VibexError::process(
                "remote_zero_config_pairing_response_failed",
                "zero-config LAN pairing response could not be encrypted",
            )
        })
}
