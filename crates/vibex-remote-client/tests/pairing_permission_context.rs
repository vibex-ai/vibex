//! The Relay pairing claim must verify the PC's Ready transcript against the
//! permission context the PC put into it.
//!
//! `RemoteGateway::relay_handshake_context` returns `remote_device_unknown` for
//! a device it has never seen, so a first-ever pairing receives a Ready with no
//! permission context. A device the PC already knows — a re-pair, or any phone
//! that had paired once before — receives a real context hash. The claim used to
//! hash `None` unconditionally, which matched only the unknown-device case and
//! failed every known-device one with `relay_handshake_transcript_mismatch`.
//! On the phone that read "Relay pairing transcript did not match the pinned
//! route", and the device could not pair over the Relay again.
//!
//! These tests drive the real `claim_pairing_offer_via_relay` against a stub
//! Relay that answers exactly like the desktop does, so they fail if the claim
//! stops binding the context.

#![cfg(not(target_family = "wasm"))]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use vibex_core::{
    DeviceId, RelayControlMessage, RelayHandshakeHello, RelayHandshakeReady, RelayPeerId,
    RelayPeerRole, RelayProtocolVersion, RelayRoomId, RelaySessionId, RelayTransportMode,
    RemoteClaimPairingOfferRequest, RemoteDevicePermissionLevel, RemotePairingCandidate,
    RemotePairingOffer, RemotePairingOfferSummary, RemotePairingTransport,
    RemoteProtocolVersionRange, RequestId, unix_timestamp_ms,
};
use vibex_relay::{
    RELAY_CRYPTO_SUITE_V2, RelayCryptoSuite, RelayKeypair, relay_handshake_authentication_tag,
    relay_handshake_transcript, relay_transcript_hash_with_ephemeral,
};
use vibex_remote_client::{ClientDeviceIdentity, claim_pairing_offer_via_relay};

/// The permission context a PC that already knows the device would return.
const KNOWN_DEVICE_CONTEXT: &str = "permission-context-for-a-known-device";

#[derive(Clone)]
struct StubRelay {
    state: Arc<StubState>,
}

struct StubState {
    pc_keypair: RelayKeypair,
    pc_ephemeral: RelayKeypair,
    room_id: RelayRoomId,
    pc_peer_id: RelayPeerId,
    /// `Some` mimics a PC that already knows the device; `None` mimics a PC
    /// that has never seen it (first-ever pairing).
    permission_context: Option<String>,
    /// What the stub saw in the claim's hello, for assertions.
    seen: Mutex<Option<RelayHandshakeHello>>,
    /// The PC-side E2EE session, established when the claim was answered.
    session: Mutex<Option<vibex_relay::RelaySession>>,
    /// The device the stub granted, so `/command` can answer with its detail.
    granted: Mutex<Option<vibex_core::RemoteDeviceDetail>>,
}

impl StubRelay {
    fn new(permission_context: Option<String>) -> Self {
        Self {
            state: Arc::new(StubState {
                pc_keypair: RelayKeypair::generate(),
                pc_ephemeral: RelayKeypair::generate(),
                room_id: RelayRoomId::new(),
                pc_peer_id: RelayPeerId::new(),
                permission_context,
                seen: Mutex::new(None),
                session: Mutex::new(None),
                granted: Mutex::new(None),
            }),
        }
    }
}

async fn stub_info(State(_stub): State<StubRelay>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "serviceName": "Vibex Relay Server",
        "serverVersion": "0.1.10",
        "protocolVersion": { "major": 0, "minor": 5 },
        "features": {
            "pcWebsocket": true,
            "deviceWebsocket": true,
            "websocketFrames": true,
            "httpPairBridge": true,
            "httpCommandBridge": true,
            "pushRegistration": false,
            "pushDispatch": false,
            "pushProviderConfigured": false
        },
        "limits": {
            "roomTtlMs": 3_600_000,
            "maxWebsocketFrameBytes": 3_145_728,
            "bridgeTimeoutMs": 30_000,
            "heartbeatTimeoutMs": 45_000,
            "maxRooms": 20_000,
            "maxTotalConnections": 20_000,
            "maxPendingPerRoom": 64,
            "maxConnectionsPerRoom": 1,
            "maxDevicesPerRoom": 8,
            "maxBodyBytes": 1_048_576,
            "rateLimitWindowMs": 1_000,
            "maxRequestsPerWindowPerRoom": 120,
            "maxQueueBytesPerConnection": 6_291_456,
            "maxBandwidthBytesPerWindow": 16_777_216,
            "maxConnectionsPerIp": 32,
            "maxConnectionAttemptsPerIpPerWindow": 120,
            "maxRoomCreatesPerIpPerWindow": 20,
            "ipRateLimitWindowMs": 60_000,
            "maxTrackedClientIps": 65_536,
            "maxPushInstallations": 256,
            "maxPushDedupEntries": 4_096,
            "pushAdapterTimeoutMs": 10_000
        }
    }))
}

/// Answers `/api/rooms/{id}/pair` exactly like `handle_pair` does: it builds a
/// Ready whose transcript hash and ephemeral proof are bound to the permission
/// context this stub resolved for the claiming device.
async fn stub_pair(
    State(stub): State<StubRelay>,
    Json(message): Json<RelayControlMessage>,
) -> Result<Json<RelayControlMessage>, (StatusCode, String)> {
    let RelayControlMessage::Hello(hello) = message else {
        return Err((StatusCode::BAD_REQUEST, "expected a hello".to_string()));
    };
    if hello.role != RelayPeerRole::Device {
        return Err((
            StatusCode::BAD_REQUEST,
            "expected a device role".to_string(),
        ));
    }
    let device_ephemeral = hello
        .ephemeral_public_key
        .clone()
        .ok_or((StatusCode::BAD_REQUEST, "missing ephemeral key".to_string()))?;
    *stub.state.seen.lock().expect("seen lock") = Some(hello.clone());

    let state = &stub.state;
    let mut ready = RelayHandshakeReady::new(
        state.room_id.clone(),
        RelaySessionId::new(),
        state.pc_peer_id.clone(),
        state.pc_keypair.public_key_base64(),
    );
    ready.role = RelayPeerRole::Pc;
    ready.transport_mode = RelayTransportMode::WebSocket;
    ready.crypto_suite = Some(RELAY_CRYPTO_SUITE_V2.to_string());
    ready.ephemeral_public_key = Some(state.pc_ephemeral.public_key_base64());
    ready.permission_context_hash = state.permission_context.clone();

    let endpoint = hello.endpoint.clone().unwrap_or_default();
    let context = ready.permission_context_hash.as_deref();
    let pc_ephemeral_public = state.pc_ephemeral.public_key_base64();

    let transcript = relay_handshake_transcript(
        RelayProtocolVersion::foundation(),
        &endpoint,
        &state.room_id,
        Some(&ready.session_id),
        &hello.peer_id,
        &hello.public_key,
        &device_ephemeral,
        &ready.peer_id,
        &ready.public_key,
        &pc_ephemeral_public,
        context,
        RelayCryptoSuite::DirectionalV2,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    ready.transcript_hash = Some(
        relay_transcript_hash_with_ephemeral(
            RelayProtocolVersion::foundation(),
            &endpoint,
            &state.room_id,
            &ready.session_id,
            &hello.peer_id,
            &hello.public_key,
            &device_ephemeral,
            &ready.peer_id,
            &ready.public_key,
            &pc_ephemeral_public,
            context,
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?,
    );
    ready.ephemeral_proof = Some(
        relay_handshake_authentication_tag(&state.pc_keypair, &hello.public_key, &transcript)
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?,
    );

    // Establish the PC side of the E2EE session so `/command` can answer the
    // claim with an encrypted PairResponse, exactly like the desktop does.
    let session = vibex_relay::RelaySession::establish_with_ephemeral(
        &state.pc_ephemeral,
        &device_ephemeral,
        vibex_relay::RelaySessionConfig {
            room_id: state.room_id.clone(),
            session_id: ready.session_id.clone(),
            local_peer_id: state.pc_peer_id.clone(),
            remote_peer_id: hello.peer_id.clone(),
        },
        Some(&endpoint),
        &state.pc_keypair.public_key_base64(),
        &hello.public_key,
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    *state.session.lock().expect("session lock") = Some(session);
    *state.granted.lock().expect("granted lock") = Some(vibex_core::RemoteDeviceDetail {
        device_id: hello.remote_device_id.clone().unwrap_or_else(DeviceId::new),
        display_name: "Test Phone".to_string(),
        public_key: Some(hello.public_key.clone()),
        grant_revision: 1,
        permission_level: RemoteDevicePermissionLevel::ReadOnly,
        status: vibex_core::RemoteDeviceStatus::Active,
        paired_at_ms: Some(unix_timestamp_ms()),
        last_seen_at_ms: Some(unix_timestamp_ms()),
        revoked_at_ms: None,
        created_at_ms: unix_timestamp_ms(),
        updated_at_ms: unix_timestamp_ms(),
    });

    Ok(Json(RelayControlMessage::Ready(ready)))
}

/// Answers `/api/rooms/{id}/command` like the desktop bridge does: it opens the
/// claim's encrypted frame, then seals a PairResponse carrying the device grant.
async fn stub_command(
    State(stub): State<StubRelay>,
    Json(frame): Json<vibex_core::RelayEncryptedFrame>,
) -> Result<Json<RelayControlMessage>, (StatusCode, String)> {
    let correlation_id = frame
        .correlation_id
        .clone()
        .ok_or((StatusCode::BAD_REQUEST, "missing correlation".to_string()))?;
    let state = &stub.state;
    let mut session = state.session.lock().expect("session lock");
    let session = session
        .as_mut()
        .ok_or((StatusCode::CONFLICT, "no session".to_string()))?;
    let opened = session
        .open_json(&frame)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    // The claim seals the pairing request itself, not an RPC envelope.
    let request: vibex_core::RemoteClaimPairingOfferRequest =
        serde_json::from_value(opened.business_payload_json)
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let device = state
        .granted
        .lock()
        .expect("granted lock")
        .clone()
        .ok_or((StatusCode::CONFLICT, "no grant".to_string()))?;
    let response = vibex_core::RemoteRpcResponseV2 {
        request_id: request.offer_id.clone(),
        correlation_id: Some(correlation_id.clone()),
        payload: Some(
            serde_json::to_value(vibex_core::RemoteClaimPairingOfferResponse {
                device,
                device_grant_token: "grant-token".to_string(),
                session_id: RequestId::new(),
            })
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?,
        ),
        error: None,
        metadata: Default::default(),
        completed_at_ms: unix_timestamp_ms(),
    };
    let sealed = session
        .seal_json(
            vibex_core::RelayFrameKind::PairResponse,
            Some(correlation_id),
            serde_json::to_value(response)
                .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?,
        )
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(RelayControlMessage::Encrypted(sealed)))
}

async fn spawn_stub(stub: StubRelay) -> String {
    let router = Router::new()
        .route("/api/info", get(stub_info))
        .route("/api/rooms/{room_id}/pair", post(stub_pair))
        .route("/api/rooms/{room_id}/command", post(stub_command))
        .with_state(stub.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub relay");
    let addr = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve stub relay");
    });
    format!("http://{addr}")
}

fn offer_for(stub: &StubRelay) -> RemotePairingOffer {
    let state = &stub.state;
    RemotePairingOffer {
        summary: RemotePairingOfferSummary {
            format_version: 1,
            protocol_range: RemoteProtocolVersionRange::v2(),
            server_id: "desktop-stub".to_string(),
            server_identity_public_key: "server-public".to_string(),
            offer_id: RequestId::new(),
            expires_at_ms: unix_timestamp_ms() + 60_000,
            direct_candidates: Vec::new(),
            relay_candidate: Some(RemotePairingCandidate {
                transport: RemotePairingTransport::SelfHostedRelay,
                url: "http://relay.invalid".to_string(),
                relay_room_id: Some(state.room_id.clone()),
                relay_pc_peer_id: Some(state.pc_peer_id.clone()),
                relay_pc_public_key: Some(state.pc_keypair.public_key_base64()),
            }),
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            granted_permissions: Vec::new(),
            canceled: false,
            claimed_device_id: None,
        },
        one_time_challenge: "challenge-0123456789abcdef".to_string(),
    }
}

fn claim_request(
    offer: &RemotePairingOffer,
    identity: &ClientDeviceIdentity,
) -> RemoteClaimPairingOfferRequest {
    RemoteClaimPairingOfferRequest {
        offer_id: offer.summary.offer_id.clone(),
        one_time_challenge: offer.one_time_challenge.clone(),
        expected_server_id: offer.summary.server_id.clone(),
        expected_server_identity_public_key: offer.summary.server_identity_public_key.clone(),
        display_name: "Test Phone".to_string(),
        device_identity_public_key: identity.public_key_base64(),
        claim_nonce: "nonce-0123456789abcdef".to_string(),
    }
}

/// A PC that already knows the device returns a permission context. The claim
/// must hash that same value; hashing `None` produced
/// `relay_handshake_transcript_mismatch` and the phone showed "Relay pairing
/// transcript did not match the pinned route".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_verifies_a_ready_bound_to_a_known_device_permission_context() {
    let stub = StubRelay::new(Some(KNOWN_DEVICE_CONTEXT.to_string()));
    let relay_url = spawn_stub(stub.clone()).await;
    let offer = offer_for(&stub);
    let identity = ClientDeviceIdentity::generate(DeviceId::new()).expect("device identity");

    let result = claim_pairing_offer_via_relay(
        relay_url,
        stub.state.room_id.clone(),
        RelayPeerId::new(),
        stub.state.pc_peer_id.clone(),
        stub.state.pc_keypair.public_key_base64(),
        claim_request(&offer, &identity),
        identity,
        true,
    )
    .await;

    match result {
        Ok(claimed) => assert_eq!(
            claimed.device.permission_level,
            RemoteDevicePermissionLevel::ReadOnly
        ),
        Err(error) => panic!(
            "a Ready bound to a known-device permission context must verify, got {}: {}",
            error.code, error.message
        ),
    }

    // The claim really reached the stub, so the failure above would be about
    // the transcript and not about an unreachable Relay.
    assert!(
        stub.state.seen.lock().expect("seen lock").is_some(),
        "the claim never reached the stub Relay"
    );
}

/// The first-ever pairing path still works: an unknown device gets a Ready
/// with no permission context, and the claim must verify that too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_verifies_a_ready_without_a_permission_context() {
    let stub = StubRelay::new(None);
    let relay_url = spawn_stub(stub.clone()).await;
    let offer = offer_for(&stub);
    let identity = ClientDeviceIdentity::generate(DeviceId::new()).expect("device identity");

    let result = claim_pairing_offer_via_relay(
        relay_url,
        stub.state.room_id.clone(),
        RelayPeerId::new(),
        stub.state.pc_peer_id.clone(),
        stub.state.pc_keypair.public_key_base64(),
        claim_request(&offer, &identity),
        identity,
        true,
    )
    .await;

    if let Err(error) = result {
        panic!(
            "a Ready without a permission context must still verify, got {}: {}",
            error.code, error.message
        );
    }
    assert!(
        stub.state.seen.lock().expect("seen lock").is_some(),
        "the claim never reached the stub Relay"
    );
}

/// The claim must send the canonical Relay endpoint in its hello, because the
/// PC binds the same string into the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_announces_the_canonical_endpoint() {
    let stub = StubRelay::new(Some(KNOWN_DEVICE_CONTEXT.to_string()));
    let relay_url = spawn_stub(stub.clone()).await;
    let offer = offer_for(&stub);
    let identity = ClientDeviceIdentity::generate(DeviceId::new()).expect("device identity");
    let expected_endpoint = relay_url.clone();

    let _ = claim_pairing_offer_via_relay(
        relay_url,
        stub.state.room_id.clone(),
        RelayPeerId::new(),
        stub.state.pc_peer_id.clone(),
        stub.state.pc_keypair.public_key_base64(),
        claim_request(&offer, &identity),
        identity,
        true,
    )
    .await;

    let seen = stub
        .state
        .seen
        .lock()
        .expect("seen lock")
        .clone()
        .expect("the claim reached the stub");
    assert_eq!(seen.endpoint.as_deref(), Some(expected_endpoint.as_str()));
}
