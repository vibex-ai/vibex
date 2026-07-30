use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
#[cfg(not(target_family = "wasm"))]
use rand_core::{OsRng, RngCore};
use serde::Serialize;
use serde_json::Value as JsonValue;
use sha2::Sha256;
use vibex_core::{
    CorrelationId, ErrorCategory, RelayEncryptedFrame, RelayErrorCode, RelayFrameId,
    RelayFrameKind, RelayPeerId, RelayPlaintextEnvelope, RelayProtocolVersion, RelayRoomId,
    RelaySessionId, VibexError, VibexResult, unix_timestamp_ms,
};
use x25519_dalek::{PublicKey, StaticSecret};

const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 24;

pub const RELAY_CRYPTO_SUITE_V1: &str = "x25519-hkdf-sha256-xchacha20poly1305-v1";
pub const RELAY_CRYPTO_SUITE_V2: &str = "x25519-hkdf-sha256-xchacha20poly1305-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayCryptoSuite {
    LegacyV1,
    DirectionalV2,
}

impl RelayCryptoSuite {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyV1 => RELAY_CRYPTO_SUITE_V1,
            Self::DirectionalV2 => RELAY_CRYPTO_SUITE_V2,
        }
    }

    pub fn negotiate(value: Option<&str>) -> VibexResult<Self> {
        match value {
            None | Some(RELAY_CRYPTO_SUITE_V1) => Ok(Self::LegacyV1),
            Some(RELAY_CRYPTO_SUITE_V2) => Ok(Self::DirectionalV2),
            Some(_) => Err(relay_error(
                RelayErrorCode::UnsupportedProtocol,
                "relay crypto suite is not supported",
            )),
        }
    }
}

pub struct RelayKeypair {
    secret: StaticSecret,
    public_key: PublicKey,
}

impl RelayKeypair {
    pub fn generate() -> Self {
        #[cfg(not(target_family = "wasm"))]
        {
            let secret = StaticSecret::random_from_rng(OsRng);
            let public_key = PublicKey::from(&secret);
            return Self { secret, public_key };
        }
        #[cfg(target_family = "wasm")]
        {
            let mut bytes = [0_u8; KEY_SIZE];
            let crypto = web_sys::window()
                .and_then(|window| window.crypto().ok())
                .expect("browser crypto is required for Relay session keys");
            crypto
                .get_random_values_with_u8_array(&mut bytes)
                .expect("browser crypto failed to generate Relay session keys");
            Self::from_private_key_bytes(bytes)
        }
    }

    pub fn from_private_key_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public_key = PublicKey::from(&secret);
        Self { secret, public_key }
    }

    pub fn from_private_key_base64url(value: &str) -> VibexResult<Self> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
            relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "relay private key was not valid base64url",
            )
        })?;
        let bytes: [u8; KEY_SIZE] = bytes.try_into().map_err(|_| {
            relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "relay private key had an invalid length",
            )
        })?;
        Ok(Self::from_private_key_bytes(bytes))
    }

    pub fn public_key_base64(&self) -> String {
        BASE64.encode(self.public_key.as_bytes())
    }

    pub fn public_key_bytes(&self) -> [u8; KEY_SIZE] {
        self.public_key.to_bytes()
    }

    pub fn private_key_bytes(&self) -> [u8; KEY_SIZE] {
        self.secret.to_bytes()
    }
}

impl fmt::Debug for RelayKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayKeypair")
            .field("public_key", &self.public_key_base64())
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySessionConfig {
    pub room_id: RelayRoomId,
    pub session_id: RelaySessionId,
    pub local_peer_id: RelayPeerId,
    pub remote_peer_id: RelayPeerId,
}

pub struct RelaySession {
    config: RelaySessionConfig,
    suite: RelayCryptoSuite,
    send_cipher: XChaCha20Poly1305,
    receive_cipher: XChaCha20Poly1305,
    send_key: [u8; KEY_SIZE],
    receive_key: [u8; KEY_SIZE],
    next_send_counter: u64,
    next_receive_counter: u64,
}

impl RelaySession {
    pub fn session_id(&self) -> &RelaySessionId {
        &self.config.session_id
    }

    pub fn remote_peer_id(&self) -> &RelayPeerId {
        &self.config.remote_peer_id
    }

    pub fn establish(
        local_keypair: &RelayKeypair,
        remote_public_key_base64: &str,
        config: RelaySessionConfig,
    ) -> VibexResult<Self> {
        Self::establish_with_suite(
            local_keypair,
            remote_public_key_base64,
            config,
            RelayCryptoSuite::LegacyV1,
            None,
        )
    }

    /// Establish a fresh session with endpoint-bound, direction-specific keys.
    /// The endpoint is part of the transcript context and must be the same
    /// canonical URL on both peers; reconnects should always use a new session.
    pub fn establish_with_endpoint(
        local_keypair: &RelayKeypair,
        remote_public_key_base64: &str,
        config: RelaySessionConfig,
        endpoint: Option<&str>,
    ) -> VibexResult<Self> {
        Self::establish_with_suite(
            local_keypair,
            remote_public_key_base64,
            config,
            RelayCryptoSuite::DirectionalV2,
            endpoint,
        )
    }

    pub fn establish_with_suite(
        local_keypair: &RelayKeypair,
        remote_public_key_base64: &str,
        config: RelaySessionConfig,
        suite: RelayCryptoSuite,
        endpoint: Option<&str>,
    ) -> VibexResult<Self> {
        let remote_public_key = decode_public_key(remote_public_key_base64)?;
        let shared_secret = local_keypair.secret.diffie_hellman(&remote_public_key);
        if !shared_secret.was_contributory() {
            return Err(relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "relay key agreement did not produce a contributory secret",
            ));
        }
        let (send_key, receive_key) = match suite {
            RelayCryptoSuite::LegacyV1 => {
                let key = derive_legacy_session_key(
                    shared_secret.as_bytes(),
                    &config,
                    local_keypair.public_key_base64(),
                    remote_public_key_base64,
                )?;
                (key, key)
            }
            RelayCryptoSuite::DirectionalV2 => (
                derive_direction_key(
                    shared_secret.as_bytes(),
                    &config,
                    local_keypair.public_key_base64(),
                    remote_public_key_base64,
                    &config.local_peer_id,
                    endpoint,
                )?,
                derive_direction_key(
                    shared_secret.as_bytes(),
                    &config,
                    local_keypair.public_key_base64(),
                    remote_public_key_base64,
                    &config.remote_peer_id,
                    endpoint,
                )?,
            ),
        };

        Ok(Self {
            config,
            suite,
            send_cipher: XChaCha20Poly1305::new((&send_key).into()),
            receive_cipher: XChaCha20Poly1305::new((&receive_key).into()),
            send_key,
            receive_key,
            next_send_counter: 1,
            next_receive_counter: 1,
        })
    }

    /// Establish a v2 session from fresh ephemeral X25519 keys.  The static
    /// public keys are included in the KDF context so an ephemeral key cannot
    /// be transplanted to another paired identity, while the actual key
    /// agreement includes only the per-connection ephemeral secret.  Static
    /// identity possession is authenticated by the handshake proof helpers
    /// below before this method is called.
    pub fn establish_with_ephemeral(
        local_ephemeral: &RelayKeypair,
        remote_ephemeral_public_key_base64: &str,
        config: RelaySessionConfig,
        endpoint: Option<&str>,
        local_static_public_key: &str,
        remote_static_public_key: &str,
    ) -> VibexResult<Self> {
        let remote_ephemeral = decode_public_key(remote_ephemeral_public_key_base64)?;
        let shared_secret = local_ephemeral.secret.diffie_hellman(&remote_ephemeral);
        if !shared_secret.was_contributory() {
            return Err(relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "relay ephemeral key agreement did not produce a contributory secret",
            ));
        }
        let (send_key, receive_key) = (
            derive_ephemeral_direction_key(
                shared_secret.as_bytes(),
                &config,
                local_static_public_key,
                remote_static_public_key,
                &local_ephemeral.public_key_base64(),
                remote_ephemeral_public_key_base64,
                &config.local_peer_id,
                endpoint,
            )?,
            derive_ephemeral_direction_key(
                shared_secret.as_bytes(),
                &config,
                local_static_public_key,
                remote_static_public_key,
                &local_ephemeral.public_key_base64(),
                remote_ephemeral_public_key_base64,
                &config.remote_peer_id,
                endpoint,
            )?,
        );
        Ok(Self {
            config,
            suite: RelayCryptoSuite::DirectionalV2,
            send_cipher: XChaCha20Poly1305::new((&send_key).into()),
            receive_cipher: XChaCha20Poly1305::new((&receive_key).into()),
            send_key,
            receive_key,
            next_send_counter: 1,
            next_receive_counter: 1,
        })
    }

    pub fn seal_json(
        &mut self,
        kind: RelayFrameKind,
        correlation_id: Option<CorrelationId>,
        business_payload_json: JsonValue,
    ) -> VibexResult<RelayEncryptedFrame> {
        let counter = self.next_send_counter;
        self.next_send_counter = self.next_send_counter.checked_add(1).ok_or_else(|| {
            relay_error(RelayErrorCode::FrameOutOfOrder, "relay counter overflow")
        })?;

        let plaintext = RelayPlaintextEnvelope {
            room_id: self.config.room_id.clone(),
            session_id: self.config.session_id.clone(),
            sender_peer_id: self.config.local_peer_id.clone(),
            recipient_peer_id: self.config.remote_peer_id.clone(),
            correlation_id: correlation_id.clone(),
            kind,
            counter,
            issued_at_ms: unix_timestamp_ms(),
            business_payload_json,
        };
        let plaintext_bytes = serde_json::to_vec(&plaintext).map_err(|_| {
            relay_error(
                RelayErrorCode::InvalidFrame,
                "failed to serialize relay plaintext envelope",
            )
        })?;

        let nonce_bytes = match self.suite {
            RelayCryptoSuite::LegacyV1 => random_nonce()?,
            RelayCryptoSuite::DirectionalV2 => derive_nonce(&self.send_key, counter)?,
        };
        let nonce = XNonce::from_slice(&nonce_bytes);

        let visible = RelayVisibleFrameMetadata {
            protocol_version: RelayProtocolVersion::foundation(),
            room_id: self.config.room_id.clone(),
            session_id: self.config.session_id.clone(),
            sender_peer_id: self.config.local_peer_id.clone(),
            recipient_peer_id: self.config.remote_peer_id.clone(),
            correlation_id: correlation_id.clone(),
            kind,
            counter,
        };
        let aad = associated_data(&visible)?;
        let ciphertext = self
            .send_cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &plaintext_bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                relay_error(RelayErrorCode::CryptoSetupFailed, "relay encryption failed")
            })?;

        Ok(RelayEncryptedFrame {
            protocol_version: RelayProtocolVersion::foundation(),
            room_id: self.config.room_id.clone(),
            session_id: self.config.session_id.clone(),
            frame_id: RelayFrameId::new(),
            sender_peer_id: self.config.local_peer_id.clone(),
            recipient_peer_id: self.config.remote_peer_id.clone(),
            correlation_id,
            kind,
            nonce: BASE64.encode(nonce_bytes),
            ciphertext: BASE64.encode(ciphertext),
            counter,
            created_at_ms: unix_timestamp_ms(),
        })
    }

    pub fn open_json(
        &mut self,
        frame: &RelayEncryptedFrame,
    ) -> VibexResult<RelayPlaintextEnvelope> {
        self.validate_visible_frame(frame)?;

        let expected = self.next_receive_counter;
        if frame.counter < expected {
            return Err(relay_error(
                RelayErrorCode::ReplayDetected,
                "relay frame counter was already accepted",
            ));
        }
        if frame.counter > expected {
            return Err(relay_error(
                RelayErrorCode::FrameOutOfOrder,
                "relay frame counter arrived out of order",
            ));
        }

        let nonce = decode_nonce(&frame.nonce)?;
        let ciphertext = decode_ciphertext(&frame.ciphertext)?;
        let visible = RelayVisibleFrameMetadata {
            protocol_version: frame.protocol_version,
            room_id: frame.room_id.clone(),
            session_id: frame.session_id.clone(),
            sender_peer_id: frame.sender_peer_id.clone(),
            recipient_peer_id: frame.recipient_peer_id.clone(),
            correlation_id: frame.correlation_id.clone(),
            kind: frame.kind,
            counter: frame.counter,
        };
        let aad = associated_data(&visible)?;
        let plaintext_bytes = self
            .receive_cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                relay_error(
                    RelayErrorCode::DecryptFailed,
                    "relay frame failed authenticated decryption",
                )
            })?;
        if self.suite == RelayCryptoSuite::DirectionalV2 {
            let expected_nonce = derive_nonce(&self.receive_key, frame.counter)?;
            if nonce != expected_nonce {
                return Err(relay_error(
                    RelayErrorCode::InvalidFrame,
                    "relay frame nonce does not match its direction and counter",
                ));
            }
        }
        let plaintext: RelayPlaintextEnvelope =
            serde_json::from_slice(&plaintext_bytes).map_err(|_| {
                relay_error(
                    RelayErrorCode::InvalidFrame,
                    "relay plaintext envelope was not valid JSON",
                )
            })?;

        self.validate_plaintext_matches_frame(frame, &plaintext)?;
        self.next_receive_counter = self.next_receive_counter.checked_add(1).ok_or_else(|| {
            relay_error(
                RelayErrorCode::FrameOutOfOrder,
                "relay receive counter overflow",
            )
        })?;

        Ok(plaintext)
    }

    fn validate_visible_frame(&self, frame: &RelayEncryptedFrame) -> VibexResult<()> {
        if frame.protocol_version != RelayProtocolVersion::foundation() {
            return Err(relay_error(
                RelayErrorCode::UnsupportedProtocol,
                "unsupported relay protocol version",
            ));
        }
        if frame.room_id != self.config.room_id || frame.session_id != self.config.session_id {
            return Err(relay_error(
                RelayErrorCode::InvalidRoom,
                "relay frame does not belong to this room or session",
            ));
        }
        if frame.sender_peer_id != self.config.remote_peer_id {
            return Err(relay_error(
                RelayErrorCode::InvalidFrame,
                "relay frame sender does not match the remote peer",
            ));
        }
        if frame.recipient_peer_id != self.config.local_peer_id {
            return Err(relay_error(
                RelayErrorCode::InvalidFrame,
                "relay frame recipient does not match the local peer",
            ));
        }
        Ok(())
    }

    fn validate_plaintext_matches_frame(
        &self,
        frame: &RelayEncryptedFrame,
        plaintext: &RelayPlaintextEnvelope,
    ) -> VibexResult<()> {
        if plaintext.room_id != frame.room_id
            || plaintext.session_id != frame.session_id
            || plaintext.sender_peer_id != frame.sender_peer_id
            || plaintext.recipient_peer_id != frame.recipient_peer_id
            || plaintext.correlation_id != frame.correlation_id
            || plaintext.kind != frame.kind
            || plaintext.counter != frame.counter
        {
            return Err(relay_error(
                RelayErrorCode::InvalidFrame,
                "relay plaintext metadata does not match the encrypted frame",
            ));
        }
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
fn random_nonce() -> VibexResult<[u8; NONCE_SIZE]> {
    let mut nonce = [0_u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    Ok(nonce)
}

#[cfg(target_family = "wasm")]
fn random_nonce() -> VibexResult<[u8; NONCE_SIZE]> {
    let mut nonce = [0_u8; NONCE_SIZE];
    let crypto = web_sys::window()
        .and_then(|window| window.crypto().ok())
        .ok_or_else(|| {
            relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "browser crypto is unavailable for legacy Relay nonce generation",
            )
        })?;
    crypto
        .get_random_values_with_u8_array(&mut nonce)
        .map_err(|_| {
            relay_error(
                RelayErrorCode::CryptoSetupFailed,
                "browser crypto failed to generate a legacy Relay nonce",
            )
        })?;
    Ok(nonce)
}

pub fn relay_transcript_hash(
    protocol_version: RelayProtocolVersion,
    endpoint: &str,
    room_id: &RelayRoomId,
    session_id: &RelaySessionId,
    first_peer_id: &RelayPeerId,
    first_public_key: &str,
    second_peer_id: &RelayPeerId,
    second_public_key: &str,
    permission_context: Option<&str>,
    crypto_suite: RelayCryptoSuite,
) -> VibexResult<String> {
    relay_transcript_hash_with_ephemeral(
        protocol_version,
        endpoint,
        room_id,
        session_id,
        first_peer_id,
        first_public_key,
        "",
        second_peer_id,
        second_public_key,
        "",
        permission_context,
        crypto_suite,
    )
}

pub fn relay_transcript_hash_with_ephemeral(
    protocol_version: RelayProtocolVersion,
    endpoint: &str,
    room_id: &RelayRoomId,
    session_id: &RelaySessionId,
    first_peer_id: &RelayPeerId,
    first_public_key: &str,
    first_ephemeral_public_key: &str,
    second_peer_id: &RelayPeerId,
    second_public_key: &str,
    second_ephemeral_public_key: &str,
    permission_context: Option<&str>,
    crypto_suite: RelayCryptoSuite,
) -> VibexResult<String> {
    use sha2::Digest as _;
    let transcript = relay_handshake_transcript(
        protocol_version,
        endpoint,
        room_id,
        Some(session_id),
        first_peer_id,
        first_public_key,
        first_ephemeral_public_key,
        second_peer_id,
        second_public_key,
        second_ephemeral_public_key,
        permission_context,
        crypto_suite,
    )?;
    Ok(BASE64.encode(Sha256::digest(transcript)))
}

/// Canonical, role-independent handshake transcript used for static identity
/// authentication and ephemeral key confirmation.  Peer tuples are ordered by
/// peer id so both sides serialize exactly the same bytes.
pub fn relay_handshake_transcript(
    protocol_version: RelayProtocolVersion,
    endpoint: &str,
    room_id: &RelayRoomId,
    session_id: Option<&RelaySessionId>,
    first_peer_id: &RelayPeerId,
    first_static_public_key: &str,
    first_ephemeral_public_key: &str,
    second_peer_id: &RelayPeerId,
    second_static_public_key: &str,
    second_ephemeral_public_key: &str,
    permission_context: Option<&str>,
    crypto_suite: RelayCryptoSuite,
) -> VibexResult<Vec<u8>> {
    let mut peers = [
        (
            first_peer_id.as_str(),
            first_static_public_key,
            first_ephemeral_public_key,
        ),
        (
            second_peer_id.as_str(),
            second_static_public_key,
            second_ephemeral_public_key,
        ),
    ];
    peers.sort_by(|left, right| left.0.cmp(right.0));
    vibex_core::canonical_json_vec(&serde_json::json!({
        "protocolVersion": protocol_version,
        "endpoint": endpoint.trim(),
        "roomId": room_id,
        "sessionId": session_id.map(RelaySessionId::as_str).unwrap_or(""),
        "peers": [
            {
                "peerId": peers[0].0,
                "staticPublicKey": peers[0].1,
                "ephemeralPublicKey": peers[0].2,
            },
            {
                "peerId": peers[1].0,
                "staticPublicKey": peers[1].1,
                "ephemeralPublicKey": peers[1].2,
            },
        ],
        "permissionContext": permission_context.unwrap_or(""),
        "cryptoSuite": crypto_suite.as_str(),
    }))
    .map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay handshake transcript could not be encoded",
        )
    })
}

/// Create a static-identity HMAC proof for a handshake transcript.  The
/// transcript contains the fresh ephemeral public keys, so the static keys
/// authenticate the exchange without becoming the traffic-encryption key.
pub fn relay_handshake_authentication_tag(
    local_static: &RelayKeypair,
    remote_static_public_key_base64: &str,
    transcript: &[u8],
) -> VibexResult<String> {
    let remote = decode_public_key(remote_static_public_key_base64)?;
    let shared = local_static.secret.diffie_hellman(&remote);
    if !shared.was_contributory() {
        return Err(relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay static identity key agreement was not contributory",
        ));
    }
    let hk = Hkdf::<Sha256>::new(Some(b"vibex-relay-handshake-auth-v1"), shared.as_bytes());
    let mut key = [0_u8; KEY_SIZE];
    hk.expand(transcript, &mut key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay handshake authentication key derivation failed",
        )
    })?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay handshake authentication setup failed",
        )
    })?;
    mac.update(transcript);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub fn verify_relay_handshake_authentication_tag(
    local_static: &RelayKeypair,
    remote_static_public_key_base64: &str,
    transcript: &[u8],
    supplied: &str,
) -> VibexResult<()> {
    let supplied = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(supplied)
        .map_err(|_| {
            relay_error(
                RelayErrorCode::InvalidFrame,
                "relay handshake authentication proof is malformed",
            )
        })?;
    let remote = decode_public_key(remote_static_public_key_base64)?;
    let shared = local_static.secret.diffie_hellman(&remote);
    if !shared.was_contributory() {
        return Err(relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay static identity key agreement was not contributory",
        ));
    }
    let hk = Hkdf::<Sha256>::new(Some(b"vibex-relay-handshake-auth-v1"), shared.as_bytes());
    let mut key = [0_u8; KEY_SIZE];
    hk.expand(transcript, &mut key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay handshake authentication key derivation failed",
        )
    })?;
    let mut verifier = <Hmac<Sha256> as Mac>::new_from_slice(&key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay handshake authentication setup failed",
        )
    })?;
    verifier.update(transcript);
    verifier.verify_slice(&supplied).map_err(|_| {
        relay_error(
            RelayErrorCode::InvalidFrame,
            "relay handshake authentication proof did not match",
        )
    })
}

impl fmt::Debug for RelaySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelaySession")
            .field("config", &self.config)
            .field("suite", &self.suite)
            .field("send_cipher", &"<redacted>")
            .field("receive_cipher", &"<redacted>")
            .field("send_key", &"<redacted>")
            .field("receive_key", &"<redacted>")
            .field("next_send_counter", &self.next_send_counter)
            .field("next_receive_counter", &self.next_receive_counter)
            .finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayVisibleFrameMetadata {
    protocol_version: RelayProtocolVersion,
    room_id: RelayRoomId,
    session_id: RelaySessionId,
    sender_peer_id: RelayPeerId,
    recipient_peer_id: RelayPeerId,
    correlation_id: Option<CorrelationId>,
    kind: RelayFrameKind,
    counter: u64,
}

fn derive_direction_key(
    shared_secret: &[u8; KEY_SIZE],
    config: &RelaySessionConfig,
    local_public_key_base64: String,
    remote_public_key_base64: &str,
    sender_peer_id: &RelayPeerId,
    endpoint: Option<&str>,
) -> VibexResult<[u8; KEY_SIZE]> {
    let salt = format!("{}:{}", config.room_id.as_str(), config.session_id.as_str());
    let mut peer_ids = [
        config.local_peer_id.as_str().to_string(),
        config.remote_peer_id.as_str().to_string(),
    ];
    peer_ids.sort();
    let mut public_keys = [
        local_public_key_base64,
        remote_public_key_base64.to_string(),
    ];
    public_keys.sort();
    let endpoint = endpoint.unwrap_or("").trim();
    let info = format!(
        "vibex-relay-e2ee-v2:{}:{}:{}:{}:{}:{}",
        peer_ids[0],
        peer_ids[1],
        public_keys[0],
        public_keys[1],
        sender_peer_id.as_str(),
        endpoint,
    );

    let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), shared_secret);
    let mut key = [0_u8; KEY_SIZE];
    hk.expand(info.as_bytes(), &mut key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "failed to derive relay session key",
        )
    })?;
    Ok(key)
}

fn derive_ephemeral_direction_key(
    shared_secret: &[u8; KEY_SIZE],
    config: &RelaySessionConfig,
    local_static_public_key: &str,
    remote_static_public_key: &str,
    local_ephemeral_public_key: &str,
    remote_ephemeral_public_key: &str,
    sender_peer_id: &RelayPeerId,
    endpoint: Option<&str>,
) -> VibexResult<[u8; KEY_SIZE]> {
    let salt = format!("{}:{}", config.room_id.as_str(), config.session_id.as_str());
    let mut peer_ids = [
        config.local_peer_id.as_str().to_string(),
        config.remote_peer_id.as_str().to_string(),
    ];
    peer_ids.sort();
    let mut static_keys = [
        local_static_public_key.to_string(),
        remote_static_public_key.to_string(),
    ];
    static_keys.sort();
    let mut ephemeral_keys = [
        local_ephemeral_public_key.to_string(),
        remote_ephemeral_public_key.to_string(),
    ];
    ephemeral_keys.sort();
    let info = format!(
        "vibex-relay-e2ee-v2-ephemeral:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        peer_ids[0],
        peer_ids[1],
        static_keys[0],
        static_keys[1],
        ephemeral_keys[0],
        ephemeral_keys[1],
        sender_peer_id.as_str(),
        endpoint.unwrap_or("").trim(),
        config.session_id.as_str(),
    );
    let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), shared_secret);
    let mut key = [0_u8; KEY_SIZE];
    hk.expand(info.as_bytes(), &mut key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "failed to derive ephemeral relay session key",
        )
    })?;
    Ok(key)
}

fn derive_legacy_session_key(
    shared_secret: &[u8; KEY_SIZE],
    config: &RelaySessionConfig,
    local_public_key_base64: String,
    remote_public_key_base64: &str,
) -> VibexResult<[u8; KEY_SIZE]> {
    let salt = format!("{}:{}", config.room_id.as_str(), config.session_id.as_str());
    let mut peer_ids = [
        config.local_peer_id.as_str().to_string(),
        config.remote_peer_id.as_str().to_string(),
    ];
    peer_ids.sort();
    let mut public_keys = [
        local_public_key_base64,
        remote_public_key_base64.to_string(),
    ];
    public_keys.sort();
    let info = format!(
        "vibex-relay-e2ee-v1:{}:{}:{}:{}",
        peer_ids[0], peer_ids[1], public_keys[0], public_keys[1]
    );
    let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), shared_secret);
    let mut key = [0_u8; KEY_SIZE];
    hk.expand(info.as_bytes(), &mut key).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "failed to derive legacy relay session key",
        )
    })?;
    Ok(key)
}

fn derive_nonce(key: &[u8; KEY_SIZE], counter: u64) -> VibexResult<[u8; NONCE_SIZE]> {
    let hk = Hkdf::<Sha256>::new(None, key);
    let mut nonce = [0_u8; NONCE_SIZE];
    let info = format!("vibex-relay-nonce-v1:{counter}");
    hk.expand(info.as_bytes(), &mut nonce).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "failed to derive relay frame nonce",
        )
    })?;
    Ok(nonce)
}

fn decode_public_key(value: &str) -> VibexResult<PublicKey> {
    let bytes = BASE64.decode(value).map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay public key was not valid base64",
        )
    })?;
    let bytes: [u8; KEY_SIZE] = bytes.try_into().map_err(|_| {
        relay_error(
            RelayErrorCode::CryptoSetupFailed,
            "relay public key had an invalid length",
        )
    })?;
    Ok(PublicKey::from(bytes))
}

fn decode_nonce(value: &str) -> VibexResult<[u8; NONCE_SIZE]> {
    let bytes = BASE64.decode(value).map_err(|_| {
        relay_error(
            RelayErrorCode::InvalidFrame,
            "relay frame nonce was not valid base64",
        )
    })?;
    bytes.try_into().map_err(|_| {
        relay_error(
            RelayErrorCode::InvalidFrame,
            "relay frame nonce had an invalid length",
        )
    })
}

fn decode_ciphertext(value: &str) -> VibexResult<Vec<u8>> {
    BASE64.decode(value).map_err(|_| {
        relay_error(
            RelayErrorCode::InvalidFrame,
            "relay frame ciphertext was not valid base64",
        )
    })
}

fn associated_data(metadata: &RelayVisibleFrameMetadata) -> VibexResult<Vec<u8>> {
    serde_json::to_vec(metadata).map_err(|_| {
        relay_error(
            RelayErrorCode::InvalidFrame,
            "failed to serialize relay associated data",
        )
    })
}

fn relay_error(code: RelayErrorCode, message: impl Into<String>) -> VibexError {
    VibexError::new(ErrorCategory::Remote, code.as_str(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vibex_core::{RemoteOperationKind, RemoteRequestEnvelope};

    fn session_pair() -> (RelaySession, RelaySession) {
        let pc_keypair = RelayKeypair::from_private_key_bytes([7_u8; KEY_SIZE]);
        let phone_keypair = RelayKeypair::from_private_key_bytes([9_u8; KEY_SIZE]);
        let room_id = RelayRoomId::new();
        let session_id = RelaySessionId::new();
        let pc_peer_id = RelayPeerId::new();
        let phone_peer_id = RelayPeerId::new();

        let pc = RelaySession::establish(
            &pc_keypair,
            &phone_keypair.public_key_base64(),
            RelaySessionConfig {
                room_id: room_id.clone(),
                session_id: session_id.clone(),
                local_peer_id: pc_peer_id.clone(),
                remote_peer_id: phone_peer_id.clone(),
            },
        )
        .unwrap();
        let phone = RelaySession::establish(
            &phone_keypair,
            &pc_keypair.public_key_base64(),
            RelaySessionConfig {
                room_id,
                session_id,
                local_peer_id: phone_peer_id,
                remote_peer_id: pc_peer_id,
            },
        )
        .unwrap();

        (pc, phone)
    }

    fn sample_business_payload() -> JsonValue {
        serde_json::to_value(
            RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession).with_payload(json!({
                "auth": {
                    "authToken": "secret-auth-token",
                    "deviceId": "device_00000000000000000000000000000000"
                },
                "prompt": "sample prompt body",
                "path": "src/secret.rs"
            })),
        )
        .unwrap()
    }

    #[test]
    fn encrypts_and_decrypts_remote_business_envelope_without_plaintext_leak() {
        let (mut pc, mut phone) = session_pair();
        let correlation_id = CorrelationId::new();
        let frame = phone
            .seal_json(
                RelayFrameKind::Command,
                Some(correlation_id.clone()),
                sample_business_payload(),
            )
            .unwrap();

        let serialized = serde_json::to_string(&frame).unwrap();
        let debug = format!("{frame:?}");

        assert_eq!(frame.correlation_id, Some(correlation_id.clone()));
        assert!(!serialized.contains("sample prompt body"));
        assert!(!serialized.contains("secret-auth-token"));
        assert!(!serialized.contains("src/secret.rs"));
        assert!(!debug.contains(&frame.ciphertext));

        let opened = pc.open_json(&frame).unwrap();
        assert_eq!(opened.kind, RelayFrameKind::Command);
        assert_eq!(opened.correlation_id, Some(correlation_id));
        assert_eq!(
            opened.business_payload_json["payload"]["prompt"],
            "sample prompt body"
        );
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let (mut pc, mut phone) = session_pair();
        let mut frame = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();
        let replacement = if frame.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        };
        frame.ciphertext.replace_range(0..1, replacement);

        let err = pc.open_json(&frame).unwrap_err();

        assert_eq!(err.code, RelayErrorCode::DecryptFailed.as_str());
        assert!(!err.message.contains("sample prompt body"));
    }

    #[test]
    fn rejects_wrong_key() {
        let (pc, mut phone) = session_pair();
        let frame = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();

        let wrong_keypair = RelayKeypair::from_private_key_bytes([11_u8; KEY_SIZE]);
        let phone_keypair = RelayKeypair::from_private_key_bytes([9_u8; KEY_SIZE]);
        let mut wrong_pc = RelaySession::establish(
            &wrong_keypair,
            &phone_keypair.public_key_base64(),
            pc.config.clone(),
        )
        .unwrap();

        let err = wrong_pc.open_json(&frame).unwrap_err();

        assert_eq!(err.code, RelayErrorCode::DecryptFailed.as_str());
    }

    #[test]
    fn rejects_wrong_room_or_session() {
        let (_pc, mut phone) = session_pair();
        let frame = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();

        let pc_keypair = RelayKeypair::from_private_key_bytes([7_u8; KEY_SIZE]);
        let phone_keypair = RelayKeypair::from_private_key_bytes([9_u8; KEY_SIZE]);
        let mut wrong_pc = RelaySession::establish(
            &pc_keypair,
            &phone_keypair.public_key_base64(),
            RelaySessionConfig {
                room_id: RelayRoomId::new(),
                session_id: frame.session_id.clone(),
                local_peer_id: frame.recipient_peer_id.clone(),
                remote_peer_id: frame.sender_peer_id.clone(),
            },
        )
        .unwrap();

        let err = wrong_pc.open_json(&frame).unwrap_err();

        assert_eq!(err.code, RelayErrorCode::InvalidRoom.as_str());
    }

    #[test]
    fn rejects_replayed_frame() {
        let (mut pc, mut phone) = session_pair();
        let frame = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();

        pc.open_json(&frame).unwrap();
        let err = pc.open_json(&frame).unwrap_err();

        assert_eq!(err.code, RelayErrorCode::ReplayDetected.as_str());
    }

    #[test]
    fn rejects_out_of_order_frame() {
        let (mut pc, mut phone) = session_pair();
        let _first = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();
        let second = phone
            .seal_json(RelayFrameKind::Command, None, sample_business_payload())
            .unwrap();

        let err = pc.open_json(&second).unwrap_err();

        assert_eq!(err.code, RelayErrorCode::FrameOutOfOrder.as_str());
    }

    #[test]
    fn authenticated_ephemerals_reject_tamper_and_reconnects_derive_fresh_direction_keys() {
        let pc_static = RelayKeypair::from_private_key_bytes([31_u8; KEY_SIZE]);
        let device_static = RelayKeypair::from_private_key_bytes([37_u8; KEY_SIZE]);
        let room_id = RelayRoomId::new();
        let pc_peer_id = RelayPeerId::new();
        let device_peer_id = RelayPeerId::new();
        let device_ephemeral = RelayKeypair::from_private_key_bytes([41_u8; KEY_SIZE]);
        let pc_ephemeral = RelayKeypair::from_private_key_bytes([43_u8; KEY_SIZE]);
        let first_session_id = RelaySessionId::new();
        let hello_transcript = relay_handshake_transcript(
            RelayProtocolVersion::foundation(),
            "https://relay.example.test",
            &room_id,
            None,
            &device_peer_id,
            &device_static.public_key_base64(),
            &device_ephemeral.public_key_base64(),
            &pc_peer_id,
            &pc_static.public_key_base64(),
            "",
            Some("permission-context"),
            RelayCryptoSuite::DirectionalV2,
        )
        .unwrap();
        let proof = relay_handshake_authentication_tag(
            &device_static,
            &pc_static.public_key_base64(),
            &hello_transcript,
        )
        .unwrap();
        verify_relay_handshake_authentication_tag(
            &pc_static,
            &device_static.public_key_base64(),
            &hello_transcript,
            &proof,
        )
        .unwrap();

        let tampered_transcript = relay_handshake_transcript(
            RelayProtocolVersion::foundation(),
            "https://relay.example.test",
            &room_id,
            None,
            &device_peer_id,
            &device_static.public_key_base64(),
            &device_ephemeral.public_key_base64(),
            &pc_peer_id,
            &pc_static.public_key_base64(),
            "",
            Some("permission-context-tampered"),
            RelayCryptoSuite::DirectionalV2,
        )
        .unwrap();
        let err = verify_relay_handshake_authentication_tag(
            &pc_static,
            &device_static.public_key_base64(),
            &tampered_transcript,
            &proof,
        )
        .unwrap_err();
        assert_eq!(err.code, RelayErrorCode::InvalidFrame.as_str());

        let first_pc = RelaySession::establish_with_ephemeral(
            &pc_ephemeral,
            &device_ephemeral.public_key_base64(),
            RelaySessionConfig {
                room_id: room_id.clone(),
                session_id: first_session_id.clone(),
                local_peer_id: pc_peer_id.clone(),
                remote_peer_id: device_peer_id.clone(),
            },
            Some("https://relay.example.test"),
            &pc_static.public_key_base64(),
            &device_static.public_key_base64(),
        )
        .unwrap();
        let first_device = RelaySession::establish_with_ephemeral(
            &device_ephemeral,
            &pc_ephemeral.public_key_base64(),
            RelaySessionConfig {
                room_id: room_id.clone(),
                session_id: first_session_id,
                local_peer_id: device_peer_id.clone(),
                remote_peer_id: pc_peer_id.clone(),
            },
            Some("https://relay.example.test"),
            &device_static.public_key_base64(),
            &pc_static.public_key_base64(),
        )
        .unwrap();
        assert_eq!(first_pc.send_key, first_device.receive_key);
        assert_eq!(first_pc.receive_key, first_device.send_key);

        let reconnect_pc_ephemeral = RelayKeypair::from_private_key_bytes([47_u8; KEY_SIZE]);
        let reconnect_device_ephemeral = RelayKeypair::from_private_key_bytes([53_u8; KEY_SIZE]);
        let reconnect_pc = RelaySession::establish_with_ephemeral(
            &reconnect_pc_ephemeral,
            &reconnect_device_ephemeral.public_key_base64(),
            RelaySessionConfig {
                room_id,
                session_id: RelaySessionId::new(),
                local_peer_id: pc_peer_id,
                remote_peer_id: device_peer_id,
            },
            Some("https://relay.example.test"),
            &pc_static.public_key_base64(),
            &device_static.public_key_base64(),
        )
        .unwrap();
        assert_ne!(first_pc.send_key, reconnect_pc.send_key);
        assert_ne!(first_pc.receive_key, reconnect_pc.receive_key);
    }

    #[test]
    fn redacts_secrets_from_debug_output() {
        let keypair = RelayKeypair::generate();
        let (pc, _phone) = session_pair();
        let private_key_marker = BASE64.encode([7_u8; KEY_SIZE]);

        assert!(!format!("{keypair:?}").contains(&private_key_marker));
        assert!(!format!("{pc:?}").contains("XChaCha20Poly1305"));
        assert!(format!("{pc:?}").contains("<redacted>"));
    }
}
