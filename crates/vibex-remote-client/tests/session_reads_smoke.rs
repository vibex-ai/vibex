#![cfg(not(target_family = "wasm"))]

use std::sync::Arc;

use vibex_agent::AgentManager;
use vibex_backend::AgentBackend as _;
use vibex_core::{
    AgentId, AgentSession, AgentSessionSafety, AgentSessionState, DeviceId, FetchTimelineRequest,
    RemoteAgentRequest, RemoteAgentSessionListRequest, RemoteAuthProof,
    RemoteCreatePairingOfferRequest, RemoteDevicePermissionLevel, RemoteOperationKind,
    RemoteRpcRequestV2, RequestId, TimelinePayload, TimelineRedactionState, TimelineSource,
    UserMessagePayload, VibexSessionId, WorkspaceMode, unix_timestamp_ms,
};
use vibex_db::{SessionRepository, TimelineRepository, WorkspaceRepository, open_database};
use vibex_remote::{RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteTrustService};
use vibex_remote_client::{
    ClientDeviceIdentity, DirectWebSocketTransport, RemoteClientConfig, RemoteConnectionState,
    WebRemoteBackend, pairing_claim_request,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_session_reads_do_not_bundle_every_timeline() {
    session_reads_do_not_bundle_every_timeline(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_lan_session_reads_do_not_bundle_every_timeline() {
    session_reads_do_not_bundle_every_timeline(true).await;
}

async fn session_reads_do_not_bundle_every_timeline(pinned_lan: bool) {
    let root = std::env::temp_dir().join(format!("vibex-session-reads-{}", RequestId::new()));
    std::fs::create_dir_all(&root).unwrap();
    let db_path = root.join("vibex.db");
    let manager = Arc::new(AgentManager::new(&db_path).unwrap());
    let mut conn = open_database(&db_path).unwrap();
    let (project, workspace) = WorkspaceRepository::ensure(
        &conn,
        root.to_str().unwrap(),
        WorkspaceMode::CurrentCheckout,
    )
    .unwrap();

    // Every individual history page fits, but bundling previews for all nine
    // sessions exceeds tungstenite's default 16 MiB frame limit. The facade
    // needs only session metadata here; history has its own paginated read.
    let history = "x".repeat(2 * 1024 * 1024);
    for index in 0..9 {
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: format!("Session {index}"),
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        TimelineRepository::append(
            &mut conn,
            &session.id,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: history.clone(),
                ..Default::default()
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
    }

    let gateway_config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
    let dispatcher = RemoteDispatcher::with_agent_manager(gateway_config.service.clone(), manager);
    let gateway = RemoteGateway::new(
        gateway_config,
        dispatcher,
        &db_path,
        root.join("identity.json"),
    );
    let (base_url, certificate) = if pinned_lan {
        let local = gateway.start_local_lan_gateway(0).await.unwrap();
        (
            format!("https://127.0.0.1:{}", local.bound_addr.port()),
            Some(local.tls_certificate_base64),
        )
    } else {
        let address = gateway.start().await.unwrap().unwrap();
        (format!("http://{address}"), None)
    };
    let server_identity = gateway.identity().unwrap();
    let seed = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
    let offer = RemoteTrustService::create_pairing_offer(
        &conn,
        &server_identity,
        RemoteCreatePairingOfferRequest {
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            ttl_ms: Some(60_000),
            direct_candidates: Vec::new(),
            relay_candidate: None,
        },
    )
    .unwrap();
    let claim = RemoteTrustService::claim_pairing_offer(
        &conn,
        pairing_claim_request(
            &offer.offer,
            "Session reader",
            seed.public_key_base64(),
            RequestId::new().into_string(),
        )
        .unwrap(),
    )
    .unwrap();
    let identity = ClientDeviceIdentity::from_private_key_base64(
        claim.device.device_id.clone(),
        &seed.private_key_base64(),
    )
    .unwrap();
    let mut config = RemoteClientConfig::new(
        base_url,
        RemoteAuthProof {
            device_id: claim.device.device_id,
            auth_token: claim.device_grant_token,
        },
    )
    .with_device_identity(identity);
    config.allow_insecure_local_dev = !pinned_lan;
    config.pinned_tls_certificate_der = certificate;
    config.expected_server_id = Some(server_identity.server_id().to_string());
    config.expected_server_identity_public_key = Some(server_identity.public_key_base64());

    let auth = config.auth.clone();
    let backend = WebRemoteBackend::from_direct(DirectWebSocketTransport::new(config).unwrap());
    backend.connect().await.unwrap();
    let sessions = backend.list_sessions(false).await.unwrap_or_else(|error| {
        panic!(
            "session list failed: {error:?}; {:?}",
            backend.connection_state()
        )
    });
    assert_eq!(sessions.len(), 9);
    let session = backend.open_session(sessions[0].id.clone()).await.unwrap();
    assert_eq!(session.id, sessions[0].id);
    let page = backend
        .fetch_timeline(FetchTimelineRequest {
            session_id: session.id,
            after_sequence: None,
            before_sequence: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(matches!(
        &page.items[0].payload,
        TimelinePayload::UserMessage(message) if message.text == history
    ));
    backend.transport().heartbeat().await.unwrap();
    assert_eq!(
        backend.connection_state().state,
        RemoteConnectionState::Online
    );

    // Explicit legacy preview requests still honor their requested limit.
    // If such a frame is oversized, retain the read failure that explains
    // the disconnect instead of losing it behind the pending RPC error.
    let payload = serde_json::to_value(RemoteAgentRequest::ListSessions(
        RemoteAgentSessionListRequest {
            auth,
            include_archived: Some(false),
            timeline_limit: Some(50),
        },
    ))
    .unwrap();
    backend
        .transport()
        .request(RemoteRpcRequestV2::new(
            RemoteOperationKind::AgentSession,
            Some(payload),
        ))
        .await
        .expect_err("explicit previews exceed the frame budget");
    let state = backend.connection_state();
    assert_eq!(state.state, RemoteConnectionState::Offline);
    assert_eq!(
        state.last_error_code.as_deref(),
        Some("remote_socket_read_failed")
    );
    assert!(
        state
            .last_error_message
            .unwrap()
            .contains("Message too long")
    );
    backend.connect().await.unwrap();
    assert_eq!(backend.list_sessions(false).await.unwrap().len(), 9);
    backend.transport().heartbeat().await.unwrap();
    backend.disconnect().await.unwrap();
    gateway.stop().await.unwrap();
    drop(conn);
    let _ = std::fs::remove_dir_all(root);
}
