#![cfg(not(target_family = "wasm"))]

use std::time::Duration;

use tokio::net::TcpListener;
use vibex_core::{
    AgentId, DeviceId, RelayPeerId, RemoteAgentDeepLinkResolveRequest,
    RemoteAgentDeepLinkResolveResponse, RemoteAgentRequest, RemoteAgentSessionListRequest,
    RemoteAgentSessionListResponse, RemoteAttachRequestV2, RemoteAttachmentKind, RemoteAuthProof,
    RemoteBinaryFrameKind, RemoteCreatePairingOfferRequest, RemoteDeepLinkResolutionStatus,
    RemoteDevicePermissionLevel, RemoteOperationKind, RemotePairingCandidate,
    RemotePairingTransport, RemoteRpcRequestV2, TerminalCreateRequest, TimelinePayload,
    TimelineRedactionState, TimelineSource, WorkspaceMode,
};
use vibex_db::{
    SessionRepository, TimelineRepository, WorkspaceRepository, apply_migrations, open_database,
};
use vibex_desktop_runtime::{RelayClientRuntime, RelayClientSettingsUpdate};
use vibex_relay_server::{RelayServerConfig, build_router};
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteTrustService,
    RemoteWorkbenchRuntime,
};
use vibex_remote_client::{
    ActiveRemoteRoute, AutoRemoteTransport, AutoRemoteTransportConfig, ChunkedFileReceiver,
    ClientDeviceIdentity, DirectCandidate, FileChunkDescriptor, FileChunkError, FileChunkSink,
    RelayClientConfig, RemoteClientConfig, RemoteConnectionState, RemoteLifecycleSignal,
    RemoteTransport, RemoteTransportEvent, TerminalBinaryBuffer, claim_pairing_offer_via_relay,
    pairing_claim_request,
};
use vibex_terminal::TerminalManager;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_e2ee_remote_v2_handshake_rpc_event_revoke_and_reconnect_smoke() {
    let root = std::env::temp_dir().join(format!(
        "vibex-relay-e2ee-smoke-{}-{}",
        std::process::id(),
        vibex_core::RequestId::new().as_str()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create smoke root");
    let db_path = root.join("vibex.db");
    let identity_path = root.join("desktop-identity.json");
    let workspace_root = root.join("workspace");
    std::fs::create_dir_all(&workspace_root).expect("create workspace");

    let mut connection = open_database(&db_path).expect("open smoke database");
    apply_migrations(&mut connection).expect("apply smoke migrations");
    let manager = std::sync::Arc::new(vibex_agent::AgentManager::new(&db_path).unwrap());
    let (project, workspace) = WorkspaceRepository::ensure(
        &connection,
        workspace_root.to_str().unwrap(),
        WorkspaceMode::CurrentCheckout,
    )
    .unwrap();
    let workspace_id = workspace.id.clone();
    let file_payload = b"relay-file-transfer-remains-e2ee\n";
    std::fs::write(workspace_root.join("relay-smoke.bin"), file_payload)
        .expect("write Relay file fixture");
    let now = vibex_core::unix_timestamp_ms();
    let session = vibex_core::AgentSession {
        id: vibex_core::VibexSessionId::new(),
        title: "Relay smoke".to_string(),
        project_id: project.id,
        workspace_id: workspace_id.clone(),
        workspace_root: workspace.root_path,
        workspace_mode: workspace.mode,
        agent_id: AgentId::parse("codex").unwrap(),
        state: vibex_core::AgentSessionState::Idle,
        safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: now,
        updated_at_ms: now,
        archived_at_ms: None,
        deleted_at_ms: None,
    };
    SessionRepository::insert(&connection, &session).unwrap();

    drop(connection);

    let direct_reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_address = direct_reservation.local_addr().unwrap();
    drop(direct_reservation);
    let direct_url = format!("http://{direct_address}");
    let gateway_config = RemoteGatewayConfig::loopback_enabled(direct_address.to_string());
    let terminal_manager = TerminalManager::with_raw_observation_capacity(128, 64 * 1024);
    let terminal = terminal_manager
        .create(
            &workspace_root,
            TerminalCreateRequest {
                workspace_id: workspace_id.clone(),
                title: Some("Relay terminal smoke".to_string()),
                shell: None,
                cwd: None,
                rows: 24,
                cols: 80,
            },
        )
        .expect("create Relay terminal fixture");
    let terminal_marker = b"relay-terminal-binary-smoke";
    terminal_manager
        .write_bytes(&terminal.id, b"printf 'relay-terminal-binary-smoke\\n'\n")
        .expect("write Relay terminal fixture");
    let dispatcher = RemoteDispatcher::with_agent_and_workbench(
        gateway_config.service.clone(),
        manager.clone(),
        RemoteWorkbenchRuntime::new(&db_path, terminal_manager.clone()),
    );
    let gateway = RemoteGateway::new(gateway_config, dispatcher.clone(), &db_path, identity_path);
    let desktop_identity = gateway.identity().unwrap();
    let relay_runtime =
        RelayClientRuntime::with_remote_gateway(dispatcher, gateway.clone()).unwrap();

    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = relay_listener.local_addr().unwrap();
    let relay_url = format!("http://{relay_address}");
    let relay_server = tokio::spawn(async move {
        axum::serve(relay_listener, build_router(RelayServerConfig::default()))
            .await
            .unwrap();
    });
    let settings = relay_runtime.get_settings().await;
    relay_runtime
        .update_settings(RelayClientSettingsUpdate {
            enabled: Some(true),
            relay_url: Some(Some(relay_url.clone())),
            ..RelayClientSettingsUpdate::default()
        })
        .await
        .unwrap();
    relay_runtime.start().await.unwrap();
    wait_for_pc(&relay_url, &relay_runtime).await;
    // The desktop PC uses its configured room, while the client candidate
    // below pins the same route metadata and persistent PC key.
    let pc_status = relay_runtime.get_status().await;
    let device_seed = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
    let connection = open_database(&db_path).unwrap();
    let offer = RemoteTrustService::create_pairing_offer(
        &connection,
        &desktop_identity,
        RemoteCreatePairingOfferRequest {
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            ttl_ms: Some(60_000),
            direct_candidates: Vec::new(),
            relay_candidate: Some(RemotePairingCandidate {
                transport: RemotePairingTransport::SelfHostedRelay,
                url: relay_url.clone(),
                relay_room_id: Some(settings.room_id.clone()),
                relay_pc_peer_id: Some(settings.pc_peer_id.clone()),
                relay_pc_public_key: Some(pc_status.pc_public_key.clone()),
            }),
        },
    )
    .unwrap()
    .offer;
    drop(connection);
    let claim_request = pairing_claim_request(
        &offer,
        "Relay smoke device",
        device_seed.public_key_base64(),
        vibex_core::RequestId::new().into_string(),
    )
    .unwrap();
    let claimed = claim_pairing_offer_via_relay(
        relay_url.clone(),
        settings.room_id.clone(),
        RelayPeerId::new(),
        settings.pc_peer_id.clone(),
        pc_status.pc_public_key.clone(),
        claim_request,
        device_seed.clone(),
        true,
    )
    .await
    .expect("Relay-only pairing claim");
    let device = claimed.device;
    let grant_token = claimed.device_grant_token;
    let device_identity = ClientDeviceIdentity::from_private_key_base64(
        device.device_id.clone(),
        &device_seed.private_key_base64(),
    )
    .unwrap();

    let mut remote = RemoteClientConfig::new(
        &relay_url,
        RemoteAuthProof {
            device_id: device.device_id.clone(),
            auth_token: grant_token,
        },
    )
    .with_device_identity(device_identity);
    remote.allow_insecure_local_dev = true;
    remote.expected_server_id = Some(desktop_identity.server_id().to_string());
    remote.expected_server_identity_public_key = Some(desktop_identity.public_key_base64());
    let relay = RelayClientConfig {
        relay_url: relay_url.clone(),
        room_id: settings.room_id,
        local_peer_id: RelayPeerId::new(),
        pc_peer_id: settings.pc_peer_id,
        pc_public_key: Some(pc_status.pc_public_key),
        remote,
    };
    let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
        remote: relay.remote.clone(),
        direct_candidates: vec![DirectCandidate {
            url: direct_url.clone(),
            label: "recovering-direct".to_string(),
            priority: 0,
        }],
        relay: Some(relay),
    })
    .unwrap();

    let info = match transport.connect().await {
        Ok(info) => info,
        Err(error) => {
            eprintln!("pc relay status: {:?}", relay_runtime.get_status().await);
            if let Ok(response) = reqwest::get(format!("{relay_url}/health")).await {
                eprintln!(
                    "relay health: {}",
                    response.text().await.unwrap_or_default()
                );
            }
            panic!("Relay v2 handshake: {error:?}");
        }
    };
    assert_eq!(info.server_id, desktop_identity.server_id());
    assert_eq!(transport.state().state, RemoteConnectionState::Online);
    assert_eq!(transport.active_route(), ActiveRemoteRoute::Relay);
    transport.heartbeat().await.expect("Relay heartbeat");

    let payload = serde_json::to_value(RemoteAgentRequest::ListSessions(
        RemoteAgentSessionListRequest {
            auth: RemoteAuthProof {
                device_id: device.device_id.clone(),
                auth_token: "payload-auth-is-overwritten".to_string(),
            },
            include_archived: Some(false),
            timeline_limit: Some(10),
        },
    ))
    .unwrap();
    let response = transport
        .request(RemoteRpcRequestV2::new(
            RemoteOperationKind::AgentSession,
            Some(payload),
        ))
        .await
        .expect("Relay RPC");
    let sessions: RemoteAgentSessionListResponse =
        serde_json::from_value(response.payload.unwrap()).unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert_eq!(sessions.sessions[0].session.id, session.id);

    let deep_link_payload = serde_json::to_value(RemoteAgentRequest::ResolveOpaqueLocator(
        RemoteAgentDeepLinkResolveRequest {
            auth: RemoteAuthProof {
                device_id: device.device_id.clone(),
                auth_token: "payload-auth-is-overwritten".to_string(),
            },
            notification_id: "relay-notification".to_string(),
            opaque_locator: session.id.as_str().to_string(),
        },
    ))
    .unwrap();
    let deep_link_response = transport
        .request(RemoteRpcRequestV2::new(
            RemoteOperationKind::AgentSession,
            Some(deep_link_payload),
        ))
        .await
        .expect("Relay authoritative deep-link resolve");
    let deep_link: RemoteAgentDeepLinkResolveResponse =
        serde_json::from_value(deep_link_response.payload.unwrap()).unwrap();
    assert_eq!(
        deep_link.resolution.status,
        RemoteDeepLinkResolutionStatus::Resolved
    );
    assert_eq!(deep_link.resolution.session_id.as_ref(), Some(&session.id));

    let mut connection = open_database(&db_path).unwrap();
    let item = TimelineRepository::append(
        &mut connection,
        &session.id,
        TimelineSource::System,
        TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
            level: vibex_core::SystemNoticeLevel::Info,
            message: "relay-active-event".to_string(),
        }),
        None,
        None,
        TimelineRedactionState::None,
    )
    .unwrap();
    manager.publish_external_timeline_item(item).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), transport.next_event())
        .await
        .expect("timeline event timeout")
        .expect("timeline event error")
        .expect("timeline event closed");
    assert!(matches!(event, RemoteTransportEvent::Event(_)));

    let file_attachment_id = "relay-file-download".to_string();
    let file_attachment = transport
        .attach(RemoteAttachRequestV2 {
            attachment_id: file_attachment_id.clone(),
            kind: RemoteAttachmentKind::FileTransfer,
            resource_id: "relay-smoke.bin".to_string(),
            scope_id: Some(workspace_id.as_str().to_string()),
            generation: info.session_epoch,
            after_sequence: 0,
        })
        .await
        .expect("attach Relay file transfer");
    assert_eq!(file_attachment.attachment_id, file_attachment_id);
    let mut file_receiver = ChunkedFileReceiver::new(
        file_attachment_id.clone(),
        1024 * 1024,
        64 * 1024,
        CollectingFileSink::default(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match transport
                .next_binary_event_for(Some(file_attachment_id.clone()))
                .await
                .expect("Relay file binary event")
            {
                Some(RemoteTransportEvent::Binary(frame)) => {
                    assert_eq!(frame.header.kind, RemoteBinaryFrameKind::FileDownloadChunk);
                    let finished = frame.header.end_of_stream;
                    file_receiver.push_binary_frame(&frame).unwrap();
                    if finished {
                        break;
                    }
                }
                Some(RemoteTransportEvent::Closed) | None => {
                    panic!("Relay closed during file transfer")
                }
                Some(event) => panic!("unexpected Relay file event: {event:?}"),
            }
        }
    })
    .await
    .expect("Relay file transfer timeout");
    let file_sink = file_receiver.into_sink();
    assert!(file_sink.finished);
    assert_eq!(file_sink.bytes, file_payload);
    transport
        .detach(file_attachment_id)
        .await
        .expect("detach Relay file transfer");

    let terminal_attachment_id = "relay-terminal-attachment".to_string();
    let terminal_stream_id = terminal.id.as_str().to_string();
    let terminal_attachment = transport
        .attach(RemoteAttachRequestV2 {
            attachment_id: terminal_attachment_id.clone(),
            kind: RemoteAttachmentKind::Terminal,
            resource_id: terminal_stream_id.clone(),
            scope_id: Some(workspace_id.as_str().to_string()),
            generation: info.session_epoch,
            after_sequence: 1,
        })
        .await
        .expect("attach Relay terminal stream");
    assert_eq!(terminal_attachment.attachment_id, terminal_attachment_id);
    let mut terminal_buffer = TerminalBinaryBuffer::new(terminal.id.clone(), 128, 1);
    let mut terminal_bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match transport
                .next_binary_event_for(Some(terminal_stream_id.clone()))
                .await
                .expect("Relay terminal binary event")
            {
                Some(RemoteTransportEvent::Binary(frame)) => {
                    assert_eq!(frame.header.kind, RemoteBinaryFrameKind::TerminalOutput);
                    terminal_buffer.push_frame(&frame).unwrap();
                    terminal_bytes.extend_from_slice(&frame.payload);
                    if terminal_bytes
                        .windows(terminal_marker.len())
                        .any(|window| window == terminal_marker)
                    {
                        break;
                    }
                }
                Some(RemoteTransportEvent::Closed) | None => {
                    panic!("Relay closed during Terminal streaming")
                }
                Some(event) => panic!("unexpected Relay Terminal event: {event:?}"),
            }
        }
    })
    .await
    .expect("Relay terminal stream timeout");
    assert!(terminal_buffer.take_batch().is_some());
    transport
        .detach(terminal_attachment_id)
        .await
        .expect("detach Relay terminal stream");

    let started_direct = gateway
        .start()
        .await
        .expect("start recovered Direct gateway")
        .expect("Direct gateway address");
    assert_eq!(started_direct, direct_address);
    wait_for_direct(&direct_url).await;
    transport.apply_lifecycle_signal(RemoteLifecycleSignal::NetworkChanged);
    wait_for_route(&transport, ActiveRemoteRoute::Direct).await;
    assert_eq!(transport.state().state, RemoteConnectionState::Online);
    transport
        .heartbeat()
        .await
        .expect("Direct heartbeat after Relay handoff");
    let direct_payload = serde_json::to_value(RemoteAgentRequest::ListSessions(
        RemoteAgentSessionListRequest {
            auth: RemoteAuthProof {
                device_id: device.device_id.clone(),
                auth_token: "payload-auth-is-overwritten".to_string(),
            },
            include_archived: Some(false),
            timeline_limit: Some(10),
        },
    ))
    .unwrap();
    let direct_response = transport
        .request(RemoteRpcRequestV2::new(
            RemoteOperationKind::AgentSession,
            Some(direct_payload),
        ))
        .await
        .expect("Direct RPC after Relay handoff");
    let direct_sessions: RemoteAgentSessionListResponse =
        serde_json::from_value(direct_response.payload.unwrap()).unwrap();
    assert_eq!(direct_sessions.sessions[0].session.id, session.id);

    gateway
        .stop()
        .await
        .expect("stop Direct gateway for fallback");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let fallback_payload = serde_json::to_value(RemoteAgentRequest::ListSessions(
        RemoteAgentSessionListRequest {
            auth: RemoteAuthProof {
                device_id: device.device_id.clone(),
                auth_token: "payload-auth-is-overwritten".to_string(),
            },
            include_archived: Some(false),
            timeline_limit: Some(10),
        },
    ))
    .unwrap();
    let fallback_response = transport
        .request(RemoteRpcRequestV2::new(
            RemoteOperationKind::AgentSession,
            Some(fallback_payload),
        ))
        .await
        .expect("offline Direct operation should fall back to Relay");
    let fallback_sessions: RemoteAgentSessionListResponse =
        serde_json::from_value(fallback_response.payload.unwrap()).unwrap();
    assert_eq!(fallback_sessions.sessions[0].session.id, session.id);
    wait_for_route(&transport, ActiveRemoteRoute::Relay).await;
    transport
        .heartbeat()
        .await
        .expect("Relay heartbeat after Direct failure fallback");

    let restored_direct = gateway
        .start()
        .await
        .expect("restore Direct gateway after fallback")
        .expect("restored Direct gateway address");
    assert_eq!(restored_direct, direct_address);
    wait_for_direct(&direct_url).await;
    transport.apply_lifecycle_signal(RemoteLifecycleSignal::NetworkChanged);
    wait_for_route(&transport, ActiveRemoteRoute::Direct).await;
    transport
        .heartbeat()
        .await
        .expect("Direct heartbeat after fallback restoration");

    RemoteTrustService::revoke_device(
        &connection,
        vibex_core::RemoteRevokeDeviceRequest {
            device_id: device.device_id.clone(),
            reason: Some("relay smoke revoke".to_string()),
        },
    )
    .unwrap();
    gateway.disconnect_device(&device.device_id);
    let closed = tokio::time::timeout(Duration::from_secs(3), transport.next_event())
        .await
        .expect("revoke close timeout")
        .expect("revoke event error");
    assert!(matches!(
        closed,
        Some(RemoteTransportEvent::Control(_)) | Some(RemoteTransportEvent::Closed)
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(transport.state().state, RemoteConnectionState::Revoked);

    transport.disconnect().await.unwrap();
    gateway.stop().await.unwrap();
    relay_runtime.stop().await.unwrap();
    terminal_manager.kill(&terminal.id).unwrap();
    relay_server.abort();
    let _ = std::fs::remove_dir_all(root);
}

async fn wait_for_pc(relay_url: &str, runtime: &RelayClientRuntime) {
    for _ in 0..100 {
        if runtime.get_status().await.state
            == vibex_desktop_runtime::RelayClientConnectionState::Connected
            && let Ok(response) = reqwest::get(format!("{relay_url}/health")).await
            && let Ok(response) = response.error_for_status()
            && let Ok(health) = response
                .json::<vibex_relay_server::RelayHealthStatus>()
                .await
            && health.active_rooms == 1
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("PC Relay client did not connect");
}

async fn wait_for_direct(direct_url: &str) {
    for _ in 0..100 {
        if let Ok(response) = reqwest::get(format!("{direct_url}/api/v2/info")).await
            && response.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("Direct gateway did not become reachable");
}

async fn wait_for_route(transport: &AutoRemoteTransport, expected: ActiveRemoteRoute) {
    for _ in 0..150 {
        if transport.active_route() == expected
            && transport.state().state == RemoteConnectionState::Online
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "Auto transport did not switch routes: expected {expected:?}, actual {:?}, state {:?}",
        transport.active_route(),
        transport.state()
    );
}

#[derive(Default)]
struct CollectingFileSink {
    bytes: Vec<u8>,
    finished: bool,
}

impl FileChunkSink for CollectingFileSink {
    fn write_chunk(
        &mut self,
        _descriptor: &FileChunkDescriptor,
        bytes: &[u8],
    ) -> Result<(), FileChunkError> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self, _checksum_sha256: Option<&str>) -> Result<(), FileChunkError> {
        self.finished = true;
        Ok(())
    }

    fn cancel(&mut self) {}
}
