//! Framework-neutral PC Relay client owned by the desktop runtime.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use vibex_core::{
    RelayBridgeMessage, RelayControlMessage, RelayError, RelayErrorCode, RelayFrameKind,
    RelayHandshakeHello, RelayHandshakeReady, RelayHeartbeat, RelayPeerId, RelayPeerMessage,
    RelayPeerRole, RelayProtocolVersion, RelayRemoteHandshakeContext, RelayRoomId, RelaySessionId,
    RemoteClaimPairingOfferRequest, RemoteJsonMessageV2, RemoteProtocolError,
    RemoteRequestEnvelope, RemoteRpcResponseV2, RemoteRpcResultMetadata, VibexError, VibexResult,
    unix_timestamp_ms,
};
use vibex_relay::{
    RELAY_CRYPTO_SUITE_V2, RelayCryptoSuite, RelayKeypair, RelaySession, RelaySessionConfig,
    relay_handshake_authentication_tag, relay_handshake_transcript,
    relay_transcript_hash_with_ephemeral, verify_relay_handshake_authentication_tag,
};
use vibex_remote::{RelayAttachmentTasks, RelayRemoteOutbound, RemoteDispatcher};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayClientSettings {
    pub enabled: bool,
    pub relay_url: Option<String>,
    pub room_id: RelayRoomId,
    pub pc_peer_id: RelayPeerId,
    pub heartbeat_interval_ms: u64,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
}

impl Default for RelayClientSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_url: None,
            room_id: RelayRoomId::new(),
            pc_peer_id: RelayPeerId::new(),
            heartbeat_interval_ms: 15_000,
            reconnect_initial_ms: 1_000,
            reconnect_max_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RelayClientSettingsUpdate {
    pub enabled: Option<bool>,
    pub relay_url: Option<Option<String>>,
    pub room_id: Option<RelayRoomId>,
    pub pc_peer_id: Option<RelayPeerId>,
    pub heartbeat_interval_ms: Option<u64>,
    pub reconnect_initial_ms: Option<u64>,
    pub reconnect_max_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayClientConnectionState {
    Disabled,
    Disconnected,
    Connecting,
    Connected,
    Retrying,
    Degraded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayClientStatus {
    pub state: RelayClientConnectionState,
    pub room_id: RelayRoomId,
    pub pc_peer_id: RelayPeerId,
    pub relay_url: Option<String>,
    pub connected_at_ms: Option<i64>,
    pub last_heartbeat_ack_ms: Option<i64>,
    pub reconnect_attempt: u32,
    pub next_retry_at_ms: Option<i64>,
    pub last_error: Option<String>,
    pub pc_public_key: String,
}

impl RelayClientStatus {
    fn disabled(settings: &RelayClientSettings) -> Self {
        Self {
            state: RelayClientConnectionState::Disabled,
            room_id: settings.room_id.clone(),
            pc_peer_id: settings.pc_peer_id.clone(),
            relay_url: settings.relay_url.clone(),
            connected_at_ms: None,
            last_heartbeat_ack_ms: None,
            reconnect_attempt: 0,
            next_retry_at_ms: None,
            last_error: None,
            pc_public_key: String::new(),
        }
    }

    fn with_state(
        settings: &RelayClientSettings,
        state: RelayClientConnectionState,
        reconnect_attempt: u32,
        next_retry_at_ms: Option<i64>,
        last_error: Option<String>,
    ) -> Self {
        Self {
            state,
            room_id: settings.room_id.clone(),
            pc_peer_id: settings.pc_peer_id.clone(),
            relay_url: settings.relay_url.clone(),
            connected_at_ms: (state == RelayClientConnectionState::Connected)
                .then(unix_timestamp_ms),
            last_heartbeat_ack_ms: None,
            reconnect_attempt,
            next_retry_at_ms,
            last_error,
            pc_public_key: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct RelayClientRuntime {
    inner: Arc<RelayClientRuntimeInner>,
}

struct RelayClientRuntimeInner {
    settings: Mutex<RelayClientSettings>,
    status: Mutex<RelayClientStatus>,
    dispatcher: RemoteDispatcher,
    keypair: RelayKeypair,
    remote_gateway: Option<vibex_remote::RemoteGateway>,
    task: Mutex<Option<JoinHandle<()>>>,
    stop_tx: Mutex<Option<oneshot::Sender<()>>>,
}

struct ActiveRelaySession {
    crypto: RelaySession,
    remote_context: Option<RelayRemoteHandshakeContext>,
    remote_auth: Option<vibex_core::RemoteAuthProof>,
    subscriptions: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    binary_sequences: HashMap<(String, u64), u64>,
    attachment_tasks: RelayAttachmentTasks,
    outbound_sender: Option<mpsc::Sender<RelayRemoteOutbound>>,
    outbound: Option<mpsc::Receiver<RelayRemoteOutbound>>,
}

struct SessionOutbound {
    recipient_peer_id: RelayPeerId,
    message: RelayRemoteOutbound,
}

impl RelayClientRuntime {
    pub fn new(dispatcher: RemoteDispatcher) -> Self {
        Self::new_inner(dispatcher, None, RelayKeypair::generate())
    }

    pub fn with_remote_gateway(
        dispatcher: RemoteDispatcher,
        gateway: vibex_remote::RemoteGateway,
    ) -> VibexResult<Self> {
        let keypair = RelayKeypair::from_private_key_bytes(gateway.relay_transport_private_key()?);
        Ok(Self::new_inner(dispatcher, Some(gateway), keypair))
    }

    fn new_inner(
        dispatcher: RemoteDispatcher,
        remote_gateway: Option<vibex_remote::RemoteGateway>,
        keypair: RelayKeypair,
    ) -> Self {
        let settings = RelayClientSettings::default();
        Self {
            inner: Arc::new(RelayClientRuntimeInner {
                status: Mutex::new(RelayClientStatus::disabled(&settings)),
                settings: Mutex::new(settings),
                dispatcher,
                keypair,
                remote_gateway,
                task: Mutex::new(None),
                stop_tx: Mutex::new(None),
            }),
        }
    }

    pub async fn get_settings(&self) -> RelayClientSettings {
        self.inner.settings.lock().await.clone()
    }

    pub async fn update_settings(
        &self,
        update: RelayClientSettingsUpdate,
    ) -> VibexResult<RelayClientSettings> {
        let updated = {
            let mut settings = self.inner.settings.lock().await;
            if let Some(enabled) = update.enabled {
                settings.enabled = enabled;
            }
            if let Some(relay_url) = update.relay_url {
                settings.relay_url = normalize_optional_url(relay_url)?;
            }
            if let Some(room_id) = update.room_id {
                settings.room_id = room_id;
            }
            if let Some(pc_peer_id) = update.pc_peer_id {
                settings.pc_peer_id = pc_peer_id;
            }
            if let Some(heartbeat_interval_ms) = update.heartbeat_interval_ms {
                settings.heartbeat_interval_ms =
                    validate_positive_ms(heartbeat_interval_ms, "relay_heartbeat_invalid")?;
            }
            if let Some(reconnect_initial_ms) = update.reconnect_initial_ms {
                settings.reconnect_initial_ms =
                    validate_positive_ms(reconnect_initial_ms, "relay_reconnect_initial_invalid")?;
            }
            if let Some(reconnect_max_ms) = update.reconnect_max_ms {
                settings.reconnect_max_ms =
                    validate_positive_ms(reconnect_max_ms, "relay_reconnect_max_invalid")?;
            }
            if settings.reconnect_initial_ms > settings.reconnect_max_ms {
                return Err(VibexError::validation(
                    "relay_reconnect_range_invalid",
                    "Relay reconnect initial delay must not exceed the maximum delay",
                ));
            }
            settings.clone()
        };

        let mut status = self.inner.status.lock().await;
        if matches!(
            status.state,
            RelayClientConnectionState::Disabled
                | RelayClientConnectionState::Disconnected
                | RelayClientConnectionState::Error
        ) {
            *status = if updated.enabled {
                RelayClientStatus::with_state(
                    &updated,
                    RelayClientConnectionState::Disconnected,
                    0,
                    None,
                    None,
                )
            } else {
                RelayClientStatus::disabled(&updated)
            };
        } else {
            status.room_id = updated.room_id.clone();
            status.pc_peer_id = updated.pc_peer_id.clone();
            status.relay_url = updated.relay_url.clone();
        }

        Ok(updated)
    }

    pub async fn get_status(&self) -> RelayClientStatus {
        let mut status = self.inner.status.lock().await.clone();
        status.pc_public_key = self.inner.keypair.public_key_base64();
        status
    }

    pub async fn start(&self) -> VibexResult<RelayClientStatus> {
        let settings = self.get_settings().await;
        if !settings.enabled {
            self.set_status(RelayClientStatus::disabled(&settings))
                .await;
            return Ok(self.get_status().await);
        }
        let relay_url = settings.relay_url.clone().ok_or_else(|| {
            VibexError::validation(
                "relay_url_required",
                "Relay URL is required before starting the Relay client",
            )
        })?;
        let _ = relay_websocket_url(&relay_url)?;

        let mut task = self.inner.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Ok(self.get_status().await);
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        *self.inner.stop_tx.lock().await = Some(stop_tx);
        let runtime = self.clone();
        *task = Some(tokio::spawn(async move {
            runtime.run_loop(stop_rx).await;
        }));
        drop(task);

        Ok(self.get_status().await)
    }

    pub async fn stop(&self) -> VibexResult<RelayClientStatus> {
        if let Some(stop_tx) = self.inner.stop_tx.lock().await.take() {
            let _ = stop_tx.send(());
        }
        if let Some(handle) = self.inner.task.lock().await.take() {
            handle.abort();
        }
        let settings = self.get_settings().await;
        self.set_status(if settings.enabled {
            RelayClientStatus::with_state(
                &settings,
                RelayClientConnectionState::Disconnected,
                0,
                None,
                None,
            )
        } else {
            RelayClientStatus::disabled(&settings)
        })
        .await;
        Ok(self.get_status().await)
    }

    async fn run_loop(&self, mut stop_rx: oneshot::Receiver<()>) {
        let mut reconnect_attempt = 0_u32;
        loop {
            let settings = self.get_settings().await;
            if !settings.enabled {
                self.set_status(RelayClientStatus::disabled(&settings))
                    .await;
                return;
            }
            let Some(relay_url) = settings.relay_url.clone() else {
                self.set_status(RelayClientStatus::with_state(
                    &settings,
                    RelayClientConnectionState::Error,
                    reconnect_attempt,
                    None,
                    Some("Relay URL is required".to_string()),
                ))
                .await;
                return;
            };
            let ws_url = match relay_websocket_url(&relay_url) {
                Ok(ws_url) => ws_url,
                Err(err) => {
                    self.set_status(RelayClientStatus::with_state(
                        &settings,
                        RelayClientConnectionState::Error,
                        reconnect_attempt,
                        None,
                        Some(err.message),
                    ))
                    .await;
                    return;
                }
            };

            self.set_status(RelayClientStatus::with_state(
                &settings,
                RelayClientConnectionState::Connecting,
                reconnect_attempt,
                None,
                None,
            ))
            .await;

            let exit = self
                .connect_once(settings.clone(), &ws_url, &mut stop_rx)
                .await;
            match exit {
                RelayLoopExit::Stopped => return,
                RelayLoopExit::Disconnected(reason) => {
                    reconnect_attempt = reconnect_attempt.saturating_add(1);
                    let delay_ms = reconnect_delay_ms(&settings, reconnect_attempt);
                    let next_retry_at_ms = unix_timestamp_ms() + delay_ms as i64;
                    self.set_status(RelayClientStatus::with_state(
                        &settings,
                        RelayClientConnectionState::Retrying,
                        reconnect_attempt,
                        Some(next_retry_at_ms),
                        Some(reason),
                    ))
                    .await;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                        _ = &mut stop_rx => {
                            self.set_status(RelayClientStatus::with_state(
                                &settings,
                                RelayClientConnectionState::Disconnected,
                                reconnect_attempt,
                                None,
                                None,
                            )).await;
                            return;
                        }
                    }
                }
            }
        }
    }

    async fn connect_once(
        &self,
        settings: RelayClientSettings,
        ws_url: &str,
        stop_rx: &mut oneshot::Receiver<()>,
    ) -> RelayLoopExit {
        let connect_result = tokio::select! {
            result = connect_async(ws_url) => result,
            _ = &mut *stop_rx => return RelayLoopExit::Stopped,
        };
        let (ws_stream, _) = match connect_result {
            Ok(connected) => connected,
            Err(err) => return RelayLoopExit::Disconnected(redact_error(err.to_string())),
        };
        let (mut writer, mut reader) = ws_stream.split();
        let hello = RelayControlMessage::Hello(RelayHandshakeHello::new(
            settings.room_id.clone(),
            settings.pc_peer_id.clone(),
            self.inner.keypair.public_key_base64(),
        ));
        let hello = match hello {
            RelayControlMessage::Hello(mut hello) => {
                hello.role = RelayPeerRole::Pc;
                hello.endpoint = Some(
                    ws_url
                        .strip_suffix("/ws")
                        .unwrap_or(ws_url)
                        .trim_end_matches('/')
                        .to_string(),
                );
                hello.capabilities = vec![
                    "websocket_frames".to_string(),
                    "opaque_peer_routing".to_string(),
                    "http_bridge".to_string(),
                ];
                hello.crypto_suite = Some(RELAY_CRYPTO_SUITE_V2.to_string());
                RelayControlMessage::Hello(hello)
            }
            _ => unreachable!(),
        };
        let encoded = match serde_json::to_string(&hello) {
            Ok(encoded) => encoded,
            Err(_) => {
                return RelayLoopExit::Disconnected(
                    "Relay registration frame could not be encoded".to_string(),
                );
            }
        };
        if writer.send(Message::Text(encoded.into())).await.is_err() {
            return RelayLoopExit::Disconnected(
                "Relay registration frame could not be sent".to_string(),
            );
        }

        self.set_status(RelayClientStatus::with_state(
            &settings,
            RelayClientConnectionState::Connected,
            0,
            None,
            None,
        ))
        .await;

        let mut heartbeat =
            tokio::time::interval(Duration::from_millis(settings.heartbeat_interval_ms.max(1)));
        let mut heartbeat_sequence = 0_u64;
        let mut sessions = HashMap::new();
        let (session_outbound_tx, mut session_outbound_rx) = mpsc::channel::<SessionOutbound>(128);

        loop {
            tokio::select! {
                _ = &mut *stop_rx => {
                    let _ = writer.send(Message::Close(None)).await;
                    return RelayLoopExit::Stopped;
                }
                _ = heartbeat.tick() => {
                    heartbeat_sequence = heartbeat_sequence.saturating_add(1);
                    let heartbeat = RelayControlMessage::Heartbeat(RelayHeartbeat {
                        room_id: settings.room_id.clone(),
                        peer_id: settings.pc_peer_id.clone(),
                        connection_id: None,
                        sequence: heartbeat_sequence,
                        sent_at_ms: unix_timestamp_ms(),
                    });
                    let Ok(encoded) = serde_json::to_string(&heartbeat) else {
                        return RelayLoopExit::Disconnected("Relay heartbeat could not be encoded".to_string());
                    };
                    if writer.send(Message::Text(encoded.into())).await.is_err() {
                        return RelayLoopExit::Disconnected("Relay heartbeat could not be sent".to_string());
                    }
                }
                Some(outbound) = session_outbound_rx.recv() => {
                    let Some(peer) = self
                        .seal_session_outbound(outbound, &mut sessions)
                        .await
                    else {
                        continue;
                    };
                    let Ok(encoded) = serde_json::to_string(&peer) else {
                        return RelayLoopExit::Disconnected("Relay session frame could not be encoded".to_string());
                    };
                    if writer.send(Message::Text(encoded.into())).await.is_err() {
                        return RelayLoopExit::Disconnected("Relay session frame could not be sent".to_string());
                    }
                }
                message = reader.next() => {
                    let Some(message) = message else {
                        return RelayLoopExit::Disconnected("Relay WebSocket closed".to_string());
                    };
                    let message = match message {
                        Ok(Message::Text(text)) => text.to_string(),
                        Ok(Message::Close(_)) => return RelayLoopExit::Disconnected("Relay WebSocket closed".to_string()),
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => continue,
                        Err(err) => return RelayLoopExit::Disconnected(redact_error(err.to_string())),
                    };
                    if let Ok(peer) = serde_json::from_str::<RelayPeerMessage>(&message) {
                        let responses = self.handle_peer_message(peer, &mut sessions).await;
                        for response in responses {
                            let Ok(encoded) = serde_json::to_string(&response) else {
                                return RelayLoopExit::Disconnected("Relay peer response could not be encoded".to_string());
                            };
                            if writer.send(Message::Text(encoded.into())).await.is_err() {
                                return RelayLoopExit::Disconnected("Relay peer response could not be sent".to_string());
                            }
                        }
                        self.install_session_outbound_forwarders(
                            &mut sessions,
                            &session_outbound_tx,
                        );
                        continue;
                    }
                    if let Ok(bridge) = serde_json::from_str::<RelayBridgeMessage>(&message) {
                        let response = self.handle_bridge_message(bridge, &mut sessions).await;
                        let Ok(encoded) = serde_json::to_string(&response) else {
                            return RelayLoopExit::Disconnected("Relay bridge response could not be encoded".to_string());
                        };
                        if writer.send(Message::Text(encoded.into())).await.is_err() {
                            return RelayLoopExit::Disconnected("Relay bridge response could not be sent".to_string());
                        }
                        continue;
                    }
                    if let Ok(RelayControlMessage::HeartbeatAck(ack)) =
                        serde_json::from_str::<RelayControlMessage>(&message)
                    {
                        let mut status = self.inner.status.lock().await;
                        status.last_heartbeat_ack_ms = Some(ack.acknowledged_at_ms);
                        status.state = RelayClientConnectionState::Connected;
                    }
                }
            }
        }
    }

    async fn handle_bridge_message(
        &self,
        bridge: RelayBridgeMessage,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
    ) -> RelayBridgeMessage {
        let settings = self.get_settings().await;
        if bridge.room_id != settings.room_id {
            return bridge_error(
                &bridge,
                RelayErrorCode::InvalidRoom,
                "Relay bridge room does not match this PC client",
                false,
            );
        }

        match bridge.message.clone() {
            RelayControlMessage::Hello(hello) => self.handle_pair(bridge, hello, sessions).await,
            RelayControlMessage::Encrypted(frame)
                if matches!(
                    frame.kind,
                    RelayFrameKind::Command | RelayFrameKind::PairRequest
                ) =>
            {
                let session_id = frame.session_id.clone();
                let Some(session) = sessions.get_mut(&session_id) else {
                    return bridge_error(
                        &bridge,
                        RelayErrorCode::InvalidFrame,
                        "Relay command session is unknown",
                        false,
                    );
                };
                let plaintext = match session.crypto.open_json(&frame) {
                    Ok(plaintext) => plaintext,
                    Err(err) => {
                        return bridge_error(
                            &bridge,
                            relay_error_code(&err),
                            "Relay command frame could not be decrypted",
                            false,
                        );
                    }
                };
                if frame.kind == RelayFrameKind::PairRequest {
                    if session.remote_context.is_some() {
                        return bridge_error(
                            &bridge,
                            RelayErrorCode::InvalidFrame,
                            "Relay pairing claim is only valid before Remote authentication",
                            false,
                        );
                    }
                    let Some(gateway) = self.inner.remote_gateway.as_ref() else {
                        return bridge_error(
                            &bridge,
                            RelayErrorCode::InvalidFrame,
                            "Relay pairing claim is unavailable without a RemoteGateway",
                            false,
                        );
                    };
                    let request = match serde_json::from_value::<RemoteClaimPairingOfferRequest>(
                        plaintext.business_payload_json,
                    ) {
                        Ok(request) => request,
                        Err(_) => {
                            return bridge_error(
                                &bridge,
                                RelayErrorCode::InvalidFrame,
                                "Relay pairing claim payload is invalid",
                                false,
                            );
                        }
                    };
                    let request_id = request.offer_id.clone();
                    let (payload, error) = match gateway.relay_claim_pairing_offer(request) {
                        Ok(response) => match serde_json::to_value(response) {
                            Ok(payload) => (Some(payload), None),
                            Err(_) => {
                                return bridge_error(
                                    &bridge,
                                    RelayErrorCode::InvalidFrame,
                                    "Relay pairing response could not be encoded",
                                    false,
                                );
                            }
                        },
                        Err(error) => (None, Some(RemoteProtocolError::from_error(error))),
                    };
                    let response = RemoteRpcResponseV2 {
                        request_id,
                        correlation_id: Some(bridge.correlation_id.clone()),
                        payload,
                        error,
                        metadata: RemoteRpcResultMetadata::default(),
                        completed_at_ms: unix_timestamp_ms(),
                    };
                    let encrypted = match session.crypto.seal_json(
                        RelayFrameKind::PairResponse,
                        Some(bridge.correlation_id.clone()),
                        serde_json::to_value(response).unwrap_or_default(),
                    ) {
                        Ok(encrypted) => encrypted,
                        Err(error) => {
                            return bridge_error(
                                &bridge,
                                relay_error_code(&error),
                                "Relay pairing response could not be encrypted",
                                false,
                            );
                        }
                    };
                    return RelayBridgeMessage {
                        correlation_id: bridge.correlation_id,
                        room_id: bridge.room_id,
                        message: RelayControlMessage::Encrypted(encrypted),
                    };
                }
                if self.inner.remote_gateway.is_some() && session.remote_context.is_none() {
                    return bridge_error(
                        &bridge,
                        RelayErrorCode::SessionRevoked,
                        "Relay session must complete pairing before Remote commands",
                        false,
                    );
                }
                let response_json = if let (Some(gateway), Some(context)) = (
                    self.inner.remote_gateway.as_ref(),
                    session.remote_context.as_ref(),
                ) {
                    if let Ok(message) = serde_json::from_value::<RemoteJsonMessageV2>(
                        plaintext.business_payload_json.clone(),
                    ) {
                        match gateway
                            .relay_process_json(
                                context,
                                message,
                                &session.subscriptions,
                                session.remote_auth.as_ref(),
                                session.outbound_sender.as_ref(),
                                &mut session.attachment_tasks,
                            )
                            .await
                        {
                            Ok(mut messages) if messages.len() == 1 => {
                                serde_json::to_value(messages.remove(0))
                            }
                            Ok(_) => Err(serde_json::Error::io(std::io::Error::other(
                                "unexpected relay response count",
                            ))),
                            Err(_) => Err(serde_json::Error::io(std::io::Error::other(
                                "remote v2 relay dispatch failed",
                            ))),
                        }
                    } else if let Ok(bytes) =
                        decode_remote_binary_payload(&plaintext.business_payload_json)
                    {
                        match gateway
                            .relay_process_binary(
                                context,
                                &bytes,
                                &mut session.binary_sequences,
                                session.remote_auth.as_ref(),
                            )
                            .await
                        {
                            Ok(mut messages) if messages.len() == 1 => match messages.remove(0) {
                                RelayRemoteOutbound::Json(message) => serde_json::to_value(message),
                                RelayRemoteOutbound::Binary(_) => {
                                    Err(serde_json::Error::io(std::io::Error::other(
                                        "legacy Relay bridge cannot return binary",
                                    )))
                                }
                            },
                            _ => Err(serde_json::Error::io(std::io::Error::other(
                                "remote v2 relay binary dispatch failed",
                            ))),
                        }
                    } else {
                        Err(serde_json::Error::io(std::io::Error::other(
                            "relay payload was not Remote v2",
                        )))
                    }
                } else {
                    let request = serde_json::from_value::<RemoteRequestEnvelope>(
                        plaintext.business_payload_json,
                    );
                    match request {
                        Ok(request) => {
                            serde_json::to_value(self.inner.dispatcher.dispatch(request).await)
                        }
                        Err(error) => Err(error),
                    }
                };
                let response_json = match response_json {
                    Ok(response_json) => response_json,
                    Err(_) => {
                        return bridge_error(
                            &bridge,
                            RelayErrorCode::InvalidFrame,
                            "Remote response could not be encoded for Relay",
                            false,
                        );
                    }
                };
                let encrypted = match session.crypto.seal_json(
                    RelayFrameKind::Response,
                    Some(bridge.correlation_id.clone()),
                    response_json,
                ) {
                    Ok(encrypted) => encrypted,
                    Err(err) => {
                        return bridge_error(
                            &bridge,
                            relay_error_code(&err),
                            "Remote response could not be encrypted for Relay",
                            false,
                        );
                    }
                };
                RelayBridgeMessage {
                    correlation_id: bridge.correlation_id,
                    room_id: bridge.room_id,
                    message: RelayControlMessage::Encrypted(encrypted),
                }
            }
            RelayControlMessage::Encrypted(_) => bridge_error(
                &bridge,
                RelayErrorCode::InvalidFrame,
                "Relay bridge command requires an encrypted command frame",
                false,
            ),
            _ => bridge_error(
                &bridge,
                RelayErrorCode::InvalidFrame,
                "Relay bridge message type is not supported by the PC client",
                false,
            ),
        }
    }

    async fn handle_peer_message(
        &self,
        peer: RelayPeerMessage,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
    ) -> Vec<RelayPeerMessage> {
        let settings = self.get_settings().await;
        let recipient_peer_id = peer.sender_peer_id.clone();
        let bridge = RelayBridgeMessage {
            correlation_id: match &peer.message {
                RelayControlMessage::Encrypted(frame) => frame
                    .correlation_id
                    .clone()
                    .unwrap_or_else(vibex_core::CorrelationId::new),
                RelayControlMessage::Error(error) => error
                    .correlation_id
                    .clone()
                    .unwrap_or_else(vibex_core::CorrelationId::new),
                _ => vibex_core::CorrelationId::new(),
            },
            room_id: peer.room_id.clone(),
            message: peer.message,
        };
        if !matches!(bridge.message, RelayControlMessage::Encrypted(_)) {
            let response = self.handle_bridge_message(bridge, sessions).await;
            return vec![RelayPeerMessage {
                room_id: response.room_id,
                sender_peer_id: settings.pc_peer_id,
                recipient_peer_id,
                message: response.message,
            }];
        }
        self.handle_encrypted_peer_message(bridge, recipient_peer_id, sessions)
            .await
    }

    async fn handle_encrypted_peer_message(
        &self,
        bridge: RelayBridgeMessage,
        recipient_peer_id: RelayPeerId,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
    ) -> Vec<RelayPeerMessage> {
        let settings = self.get_settings().await;
        let RelayControlMessage::Encrypted(frame) = bridge.message.clone() else {
            return Vec::new();
        };
        let session_id = frame.session_id.clone();
        let Some(session) = sessions.get_mut(&session_id) else {
            return vec![peer_error(
                &settings,
                &recipient_peer_id,
                &bridge,
                RelayErrorCode::InvalidFrame,
                "Relay session is unknown",
            )];
        };
        let plaintext = match session.crypto.open_json(&frame) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                return vec![peer_error(
                    &settings,
                    &recipient_peer_id,
                    &bridge,
                    relay_error_code(&error),
                    "Relay frame could not be decrypted",
                )];
            }
        };
        let (Some(gateway), Some(context)) = (
            self.inner.remote_gateway.as_ref(),
            session.remote_context.as_ref(),
        ) else {
            return vec![peer_error(
                &settings,
                &recipient_peer_id,
                &bridge,
                RelayErrorCode::InvalidFrame,
                "Relay v2 session is not attached to RemoteGateway",
            )];
        };
        let result = if let Ok(message) =
            serde_json::from_value::<RemoteJsonMessageV2>(plaintext.business_payload_json.clone())
        {
            let hello_auth = match &message {
                RemoteJsonMessageV2::Control(vibex_core::RemoteControlMessageV2::Hello(hello)) => {
                    hello.relay_auth.clone()
                }
                _ => None,
            };
            let result = gateway
                .relay_process_json(
                    context,
                    message,
                    &session.subscriptions,
                    session.remote_auth.as_ref(),
                    session.outbound_sender.as_ref(),
                    &mut session.attachment_tasks,
                )
                .await
                .map(|messages| {
                    messages
                        .into_iter()
                        .map(RelayRemoteOutbound::Json)
                        .collect::<Vec<_>>()
                });
            if result.is_ok() && hello_auth.is_some() {
                session.remote_auth = hello_auth;
            }
            result
        } else if let Ok(bytes) = decode_remote_binary_payload(&plaintext.business_payload_json) {
            if session.remote_auth.is_none() {
                return vec![peer_error(
                    &settings,
                    &recipient_peer_id,
                    &bridge,
                    RelayErrorCode::SessionRevoked,
                    "Remote v2 Relay binary frame arrived before authentication",
                )];
            }
            gateway
                .relay_process_binary(
                    context,
                    &bytes,
                    &mut session.binary_sequences,
                    session.remote_auth.as_ref(),
                )
                .await
        } else {
            Err(VibexError::validation(
                "remote_relay_payload_invalid",
                "relay payload was not a Remote v2 JSON or binary frame",
            ))
        };
        let outbound = match result {
            Ok(outbound) => outbound,
            Err(error) => {
                let code = relay_error_code(&error);
                if code == RelayErrorCode::SessionRevoked {
                    sessions.remove(&session_id);
                }
                return vec![peer_error(
                    &settings,
                    &recipient_peer_id,
                    &bridge,
                    code,
                    "Remote v2 Relay session was rejected",
                )];
            }
        };
        outbound
            .into_iter()
            .filter_map(|message| {
                seal_peer_outbound(
                    &settings,
                    &recipient_peer_id,
                    &bridge.room_id,
                    sessions.get_mut(&session_id)?,
                    message,
                )
                .ok()
            })
            .collect()
    }

    fn install_session_outbound_forwarders(
        &self,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
        sender: &mpsc::Sender<SessionOutbound>,
    ) {
        for session in sessions.values_mut() {
            let Some(mut receiver) = session.outbound.take() else {
                continue;
            };
            let recipient_peer_id = session.crypto.remote_peer_id().clone();
            let sender = sender.clone();
            tokio::spawn(async move {
                while let Some(message) = receiver.recv().await {
                    if sender
                        .send(SessionOutbound {
                            recipient_peer_id: recipient_peer_id.clone(),
                            message,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    }

    async fn seal_session_outbound(
        &self,
        outbound: SessionOutbound,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
    ) -> Option<RelayPeerMessage> {
        let settings = self.get_settings().await;
        let session = sessions
            .values_mut()
            .find(|session| session.crypto.remote_peer_id() == &outbound.recipient_peer_id)?;
        seal_peer_outbound(
            &settings,
            &outbound.recipient_peer_id,
            &settings.room_id,
            session,
            outbound.message,
        )
        .ok()
    }

    async fn handle_pair(
        &self,
        bridge: RelayBridgeMessage,
        hello: RelayHandshakeHello,
        sessions: &mut HashMap<RelaySessionId, ActiveRelaySession>,
    ) -> RelayBridgeMessage {
        let settings = self.get_settings().await;
        if hello.protocol_version != RelayProtocolVersion::foundation()
            || hello.room_id != bridge.room_id
        {
            return bridge_error(
                &bridge,
                RelayErrorCode::UnsupportedProtocol,
                "Relay pair request protocol or room is not supported",
                false,
            );
        }
        // A device can reconnect while the old synthetic RemoteGateway
        // outbound task is still draining. Keep only the newest crypto/session
        // context for that peer so asynchronous notifications cannot be sealed
        // with a stale room/session route.
        sessions.retain(|_, session| session.crypto.remote_peer_id() != &hello.peer_id);
        let session_id = RelaySessionId::new();
        let suite = match RelayCryptoSuite::negotiate(hello.crypto_suite.as_deref()) {
            Ok(suite) => suite,
            Err(err) => {
                return bridge_error(
                    &bridge,
                    relay_error_code(&err),
                    "Relay pair request crypto suite is not supported",
                    false,
                );
            }
        };
        let endpoint = hello.endpoint.as_deref();
        let session_config = RelaySessionConfig {
            room_id: bridge.room_id.clone(),
            session_id: session_id.clone(),
            local_peer_id: settings.pc_peer_id.clone(),
            remote_peer_id: hello.peer_id.clone(),
        };
        let mut pc_ephemeral = None;
        let session = match if suite == RelayCryptoSuite::DirectionalV2 {
            let device_ephemeral = hello.ephemeral_public_key.as_deref().ok_or_else(|| {
                VibexError::validation(
                    "relay_ephemeral_key_required",
                    "Relay v2 hello requires a fresh ephemeral public key",
                )
            });
            let proof = hello.ephemeral_proof.as_deref().ok_or_else(|| {
                VibexError::validation(
                    "relay_ephemeral_proof_required",
                    "Relay v2 hello requires an authenticated ephemeral key",
                )
            });
            device_ephemeral.and_then(|device_ephemeral| {
                proof.and_then(|proof| {
                    let transcript = relay_handshake_transcript(
                        hello.protocol_version,
                        hello.endpoint.as_deref().unwrap_or_default(),
                        &bridge.room_id,
                        None,
                        &hello.peer_id,
                        &hello.public_key,
                        device_ephemeral,
                        &settings.pc_peer_id,
                        &self.inner.keypair.public_key_base64(),
                        "",
                        None,
                        suite,
                    )?;
                    verify_relay_handshake_authentication_tag(
                        &self.inner.keypair,
                        &hello.public_key,
                        &transcript,
                        proof,
                    )?;
                    let ephemeral = RelayKeypair::generate();
                    let established = RelaySession::establish_with_ephemeral(
                        &ephemeral,
                        device_ephemeral,
                        session_config.clone(),
                        endpoint,
                        &self.inner.keypair.public_key_base64(),
                        &hello.public_key,
                    )?;
                    pc_ephemeral = Some(ephemeral);
                    Ok(established)
                })
            })
        } else {
            RelaySession::establish_with_suite(
                &self.inner.keypair,
                &hello.public_key,
                session_config,
                suite,
                endpoint,
            )
        } {
            Ok(session) => session,
            Err(err) => {
                return bridge_error(
                    &bridge,
                    relay_error_code(&err),
                    "Relay pair request could not establish an encrypted session",
                    false,
                );
            }
        };
        sessions.insert(
            session_id.clone(),
            ActiveRelaySession {
                crypto: session,
                remote_context: None,
                remote_auth: None,
                subscriptions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
                binary_sequences: HashMap::new(),
                attachment_tasks: RelayAttachmentTasks::default(),
                outbound_sender: None,
                outbound: None,
            },
        );
        let mut ready = RelayHandshakeReady::new(
            bridge.room_id.clone(),
            session_id,
            settings.pc_peer_id,
            self.inner.keypair.public_key_base64(),
        );
        ready.role = RelayPeerRole::Pc;
        ready.transport_mode = vibex_core::RelayTransportMode::WebSocket;
        ready.crypto_suite = Some(suite.as_str().to_string());
        ready.ephemeral_public_key = pc_ephemeral.as_ref().map(RelayKeypair::public_key_base64);
        if let (Some(gateway), Some(device_id), Some(device_public_key)) = (
            self.inner.remote_gateway.as_ref(),
            hello.remote_device_id.as_ref(),
            hello.remote_device_identity_public_key.as_ref(),
        ) {
            match gateway.relay_handshake_context(
                device_id,
                device_public_key,
                hello.endpoint.as_deref().unwrap_or_default(),
            ) {
                Ok(context) => {
                    if let Some(session) = sessions.get_mut(&ready.session_id) {
                        session.remote_context = Some(context.clone());
                        if let Ok((sender, receiver)) =
                            gateway.relay_outbound(&context, session.subscriptions.clone())
                        {
                            session.outbound_sender = Some(sender);
                            session.outbound = Some(receiver);
                        }
                    }
                    ready.proof_challenge = Some(context.proof_challenge);
                    ready.remote_session_epoch = Some(context.session_epoch);
                    ready.desktop_identity_public_key = Some(context.desktop_identity_public_key);
                    ready.permission_context_hash = Some(context.permission_context_hash);
                }
                Err(error) if error.code == "remote_device_unknown" => {
                    // A Relay-only first pairing has no Remote device record
                    // yet. Keep the outer E2EE session in a restricted state;
                    // only an encrypted PairRequest may be processed below.
                }
                Err(error) => {
                    sessions.remove(&ready.session_id);
                    return bridge_error(
                        &bridge,
                        if error.code == "remote_device_revoked" {
                            RelayErrorCode::SessionRevoked
                        } else {
                            RelayErrorCode::CryptoSetupFailed
                        },
                        "Relay device identity could not start a Remote v2 session",
                        false,
                    );
                }
            }
        } else if suite == RelayCryptoSuite::DirectionalV2 {
            sessions.remove(&ready.session_id);
            return bridge_error(
                &bridge,
                RelayErrorCode::CryptoSetupFailed,
                "Relay v2 pairing requires a bound Remote device identity",
                false,
            );
        }
        if suite == RelayCryptoSuite::DirectionalV2 {
            let permission_context = ready.permission_context_hash.as_deref();
            let device_ephemeral = hello.ephemeral_public_key.as_deref().unwrap_or_default();
            let pc_ephemeral_public = ready.ephemeral_public_key.as_deref().unwrap_or_default();
            match relay_transcript_hash_with_ephemeral(
                ready.protocol_version,
                hello.endpoint.as_deref().unwrap_or_default(),
                &ready.room_id,
                &ready.session_id,
                &hello.peer_id,
                &hello.public_key,
                device_ephemeral,
                &ready.peer_id,
                &ready.public_key,
                pc_ephemeral_public,
                permission_context,
                suite,
            ) {
                Ok(hash) => ready.transcript_hash = Some(hash),
                Err(_) => {
                    sessions.remove(&ready.session_id);
                    return bridge_error(
                        &bridge,
                        RelayErrorCode::CryptoSetupFailed,
                        "Relay handshake transcript could not be bound",
                        false,
                    );
                }
            }
            let proof_transcript = relay_handshake_transcript(
                ready.protocol_version,
                hello.endpoint.as_deref().unwrap_or_default(),
                &ready.room_id,
                Some(&ready.session_id),
                &hello.peer_id,
                &hello.public_key,
                device_ephemeral,
                &ready.peer_id,
                &ready.public_key,
                pc_ephemeral_public,
                permission_context,
                suite,
            );
            ready.ephemeral_proof = match proof_transcript.and_then(|transcript| {
                relay_handshake_authentication_tag(
                    &self.inner.keypair,
                    &hello.public_key,
                    &transcript,
                )
            }) {
                Ok(proof) => Some(proof),
                Err(_) => {
                    sessions.remove(&ready.session_id);
                    return bridge_error(
                        &bridge,
                        RelayErrorCode::CryptoSetupFailed,
                        "Relay handshake proof could not be created",
                        false,
                    );
                }
            };
        }
        RelayBridgeMessage {
            correlation_id: bridge.correlation_id,
            room_id: bridge.room_id.clone(),
            message: RelayControlMessage::Ready(ready),
        }
    }

    async fn set_status(&self, status: RelayClientStatus) {
        *self.inner.status.lock().await = status;
    }
}

enum RelayLoopExit {
    Stopped,
    Disconnected(String),
}

fn normalize_optional_url(value: Option<String>) -> VibexResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let _ = relay_websocket_url(&trimmed)?;
    Ok(Some(trimmed))
}

fn validate_positive_ms(value: u64, code: &'static str) -> VibexResult<u64> {
    if value == 0 {
        return Err(VibexError::validation(
            code,
            "Relay duration settings must be greater than zero",
        ));
    }
    Ok(value)
}

fn relay_websocket_url(relay_url: &str) -> VibexResult<String> {
    let trimmed = relay_url.trim().trim_end_matches('/');
    let base = if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        return Err(VibexError::validation(
            "relay_url_invalid",
            "Relay URL must use http, https, ws, or wss",
        ));
    };
    Ok(format!("{base}/ws"))
}

fn reconnect_delay_ms(settings: &RelayClientSettings, attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(16);
    let multiplier = 1_u64 << exponent;
    settings
        .reconnect_initial_ms
        .saturating_mul(multiplier)
        .min(settings.reconnect_max_ms)
        .max(1)
}

fn bridge_error(
    bridge: &RelayBridgeMessage,
    code: RelayErrorCode,
    message: &'static str,
    retryable: bool,
) -> RelayBridgeMessage {
    RelayBridgeMessage {
        correlation_id: bridge.correlation_id.clone(),
        room_id: bridge.room_id.clone(),
        message: RelayControlMessage::Error(
            RelayError::new(code, message)
                .with_correlation_id(bridge.correlation_id.clone())
                .retryable(retryable),
        ),
    }
}

fn peer_error(
    settings: &RelayClientSettings,
    recipient_peer_id: &RelayPeerId,
    bridge: &RelayBridgeMessage,
    code: RelayErrorCode,
    message: &'static str,
) -> RelayPeerMessage {
    RelayPeerMessage {
        room_id: bridge.room_id.clone(),
        sender_peer_id: settings.pc_peer_id.clone(),
        recipient_peer_id: recipient_peer_id.clone(),
        message: RelayControlMessage::Error(
            RelayError::new(code, message)
                .with_correlation_id(bridge.correlation_id.clone())
                .retryable(false),
        ),
    }
}

fn seal_peer_outbound(
    settings: &RelayClientSettings,
    recipient_peer_id: &RelayPeerId,
    room_id: &RelayRoomId,
    session: &mut ActiveRelaySession,
    message: RelayRemoteOutbound,
) -> VibexResult<RelayPeerMessage> {
    use base64::Engine as _;

    let (payload, correlation_id, kind) = match message {
        RelayRemoteOutbound::Json(message) => {
            let correlation_id = match &message {
                RemoteJsonMessageV2::RpcRequest(request) => request.correlation_id.clone(),
                RemoteJsonMessageV2::RpcResponse(response) => response.correlation_id.clone(),
                RemoteJsonMessageV2::Event(event) => event.correlation_id.clone(),
                RemoteJsonMessageV2::Control(_) | RemoteJsonMessageV2::Unknown => None,
            };
            let payload = serde_json::to_value(message).map_err(|_| {
                VibexError::validation(
                    "remote_relay_response_encode_failed",
                    "Remote v2 Relay response could not be encoded",
                )
            })?;
            (payload, correlation_id, RelayFrameKind::Response)
        }
        RelayRemoteOutbound::Binary(bytes) => (
            serde_json::json!({
                "encoding": "remote_binary_base64url",
                "bytes": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            }),
            None,
            RelayFrameKind::Event,
        ),
    };
    let encrypted = session.crypto.seal_json(kind, correlation_id, payload)?;
    Ok(RelayPeerMessage {
        room_id: room_id.clone(),
        sender_peer_id: settings.pc_peer_id.clone(),
        recipient_peer_id: recipient_peer_id.clone(),
        message: RelayControlMessage::Encrypted(encrypted),
    })
}

fn decode_remote_binary_payload(value: &serde_json::Value) -> Result<Vec<u8>, ()> {
    use base64::Engine as _;
    let object = value.as_object().ok_or(())?;
    if object.get("encoding").and_then(serde_json::Value::as_str) != Some("remote_binary_base64url")
    {
        return Err(());
    }
    let encoded = object
        .get("bytes")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ())
}

fn relay_error_code(error: &VibexError) -> RelayErrorCode {
    match error.code.as_str() {
        "relay_unsupported_protocol" => RelayErrorCode::UnsupportedProtocol,
        "relay_invalid_room" => RelayErrorCode::InvalidRoom,
        "relay_invalid_correlation" => RelayErrorCode::InvalidCorrelation,
        "relay_crypto_setup_failed" => RelayErrorCode::CryptoSetupFailed,
        "relay_decrypt_failed" => RelayErrorCode::DecryptFailed,
        "relay_replay_detected" => RelayErrorCode::ReplayDetected,
        "relay_frame_out_of_order" => RelayErrorCode::FrameOutOfOrder,
        "remote_device_revoked"
        | "remote_device_unknown"
        | "remote_device_identity_mismatch"
        | "remote_relay_session_epoch_stale"
        | "remote_permission_context_changed" => RelayErrorCode::SessionRevoked,
        _ => RelayErrorCode::InvalidFrame,
    }
}

fn redact_error(message: String) -> String {
    if message.is_empty() {
        "Relay transport failed".to_string()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_agent::AgentManager;
    use vibex_core::{
        AgentId, CorrelationId, RemoteAgentRequest, RemoteAgentSendMessageRequest,
        RemoteAgentSessionListRequest, RemoteAgentSessionListResponse, RemoteAuthProof,
        RemoteClaimPairingCodeRequest, RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel,
        RemoteEnvelopeStatus, RemoteOperationKind, RemoteResponseEnvelope, RequestId,
        SendAgentMessageRequest, SessionRuntimeSelection, WorkspaceMode,
    };
    use vibex_db::{
        RemoteDeviceRepository, SessionRepository, WorkspaceRepository, apply_migrations,
        open_database,
    };
    use vibex_remote::{RemoteServiceConfig, RemoteTrustService};

    #[tokio::test]
    async fn default_settings_and_status_are_disabled() {
        let runtime = RelayClientRuntime::new(RemoteDispatcher::new(
            RemoteServiceConfig::loopback_disabled(),
        ));

        let settings = runtime.get_settings().await;
        let status = runtime.get_status().await;

        assert!(!settings.enabled);
        assert_eq!(status.state, RelayClientConnectionState::Disabled);
        assert_eq!(status.room_id, settings.room_id);
    }

    #[tokio::test]
    async fn start_when_disabled_does_not_connect() {
        let runtime = RelayClientRuntime::new(RemoteDispatcher::new(
            RemoteServiceConfig::loopback_disabled(),
        ));

        let status = runtime.start().await.unwrap();

        assert_eq!(status.state, RelayClientConnectionState::Disabled);
        assert_eq!(
            runtime.get_status().await.state,
            RelayClientConnectionState::Disabled
        );
    }

    #[tokio::test]
    async fn pair_and_command_dispatch_through_remote_dispatcher() {
        let (db_path, manager) = test_agent_manager("relay-command");
        let session = create_mock_session(&manager, "Relay command").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Phone");
        let dispatcher =
            RemoteDispatcher::with_agent_manager(RemoteServiceConfig::loopback_disabled(), manager);
        let runtime = RelayClientRuntime::new(dispatcher);
        let settings = runtime.get_settings().await;
        let mobile_keypair = RelayKeypair::from_private_key_bytes([9_u8; 32]);
        let mobile_peer_id = RelayPeerId::new();
        let mut pc_sessions = HashMap::new();
        let pair_correlation_id = CorrelationId::new();
        let pair = RelayBridgeMessage {
            correlation_id: pair_correlation_id.clone(),
            room_id: settings.room_id.clone(),
            message: RelayControlMessage::Hello(RelayHandshakeHello::new(
                settings.room_id.clone(),
                mobile_peer_id.clone(),
                mobile_keypair.public_key_base64(),
            )),
        };

        let ready_bridge = runtime.handle_bridge_message(pair, &mut pc_sessions).await;
        let RelayControlMessage::Ready(ready) = ready_bridge.message else {
            panic!("expected relay ready response");
        };
        assert_eq!(ready_bridge.correlation_id, pair_correlation_id);
        assert_eq!(ready.room_id, settings.room_id);
        assert_eq!(pc_sessions.len(), 1);

        let mut mobile_session = RelaySession::establish(
            &mobile_keypair,
            &ready.public_key,
            RelaySessionConfig {
                room_id: settings.room_id.clone(),
                session_id: ready.session_id,
                local_peer_id: mobile_peer_id,
                remote_peer_id: settings.pc_peer_id.clone(),
            },
        )
        .unwrap();
        let command_correlation_id = CorrelationId::new();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession).with_payload(
            serde_json::to_value(RemoteAgentRequest::ListSessions(
                RemoteAgentSessionListRequest {
                    auth,
                    include_archived: Some(false),
                    timeline_limit: Some(10),
                },
            ))
            .unwrap(),
        );
        let command_frame = mobile_session
            .seal_json(
                RelayFrameKind::Command,
                Some(command_correlation_id.clone()),
                serde_json::to_value(request).unwrap(),
            )
            .unwrap();
        let command = RelayBridgeMessage {
            correlation_id: command_correlation_id.clone(),
            room_id: settings.room_id.clone(),
            message: RelayControlMessage::Encrypted(command_frame),
        };

        let response_bridge = runtime
            .handle_bridge_message(command, &mut pc_sessions)
            .await;
        let RelayControlMessage::Encrypted(response_frame) = response_bridge.message else {
            panic!("expected encrypted relay response");
        };
        assert_eq!(response_bridge.correlation_id, command_correlation_id);
        let opened = mobile_session.open_json(&response_frame).unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_value(opened.business_payload_json).unwrap();
        let payload: RemoteAgentSessionListResponse =
            serde_json::from_value(response.payload.unwrap()).unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(payload.sessions.len(), 1);
        assert_eq!(payload.sessions[0].session.id, session.id);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn revoked_device_fails_after_relay_decrypt_without_plaintext_leak() {
        let (db_path, manager) = test_agent_manager("relay-revoked");
        let session = create_mock_session(&manager, "Relay revoked").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Phone");
        revoke_device(&db_path, &auth);
        let dispatcher =
            RemoteDispatcher::with_agent_manager(RemoteServiceConfig::loopback_disabled(), manager);
        let runtime = RelayClientRuntime::new(dispatcher);
        let (mut mobile_session, mut pc_sessions, settings) = pair_mobile(&runtime).await;
        let command_correlation_id = CorrelationId::new();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession).with_payload(
            serde_json::to_value(RemoteAgentRequest::ListSessions(
                RemoteAgentSessionListRequest {
                    auth,
                    include_archived: Some(false),
                    timeline_limit: Some(10),
                },
            ))
            .unwrap(),
        );
        let command_frame = mobile_session
            .seal_json(
                RelayFrameKind::Command,
                Some(command_correlation_id.clone()),
                serde_json::to_value(request).unwrap(),
            )
            .unwrap();

        let response_bridge = runtime
            .handle_bridge_message(
                RelayBridgeMessage {
                    correlation_id: command_correlation_id,
                    room_id: settings.room_id,
                    message: RelayControlMessage::Encrypted(command_frame),
                },
                &mut pc_sessions,
            )
            .await;
        let RelayControlMessage::Encrypted(response_frame) = response_bridge.message else {
            panic!("expected encrypted relay response");
        };
        let opened = mobile_session.open_json(&response_frame).unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_value(opened.business_payload_json).unwrap();
        let error = response.error.unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(error.code, "remote_device_revoked");
        assert!(!format!("{response_frame:?}").contains(&session.title));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn under_permissioned_relay_command_returns_encrypted_remote_error() {
        let (db_path, manager) = test_agent_manager("relay-denied");
        let session = create_mock_session(&manager, "Relay denied").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Phone");
        let dispatcher =
            RemoteDispatcher::with_agent_manager(RemoteServiceConfig::loopback_disabled(), manager);
        let runtime = RelayClientRuntime::new(dispatcher);
        let (mut mobile_session, mut pc_sessions, settings) = pair_mobile(&runtime).await;
        let command_correlation_id = CorrelationId::new();
        let prompt = "relay prompt body with token secret";
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession).with_payload(
            serde_json::to_value(RemoteAgentRequest::SendMessage(
                RemoteAgentSendMessageRequest {
                    auth,
                    request: SendAgentMessageRequest {
                        session_id: session.id.clone(),
                        message_idempotency_key: "relay-denied-message".to_string(),
                        desired_runtime: SessionRuntimeSelection::provider(
                            session.agent_id.clone(),
                            vibex_core::ProviderProfileId::parse("provider_acp_relay_test")
                                .unwrap(),
                            "relay-stub",
                        ),
                        text: prompt.to_string(),
                        attachments: Vec::new(),
                        reasoning_effort: None,
                        correlation_id: None,
                    },
                },
            ))
            .unwrap(),
        );
        let command_frame = mobile_session
            .seal_json(
                RelayFrameKind::Command,
                Some(command_correlation_id.clone()),
                serde_json::to_value(request).unwrap(),
            )
            .unwrap();

        let response_bridge = runtime
            .handle_bridge_message(
                RelayBridgeMessage {
                    correlation_id: command_correlation_id,
                    room_id: settings.room_id,
                    message: RelayControlMessage::Encrypted(command_frame),
                },
                &mut pc_sessions,
            )
            .await;
        let RelayControlMessage::Encrypted(response_frame) = response_bridge.message else {
            panic!("expected encrypted relay response");
        };
        let frame_debug = format!("{response_frame:?}");
        let opened = mobile_session.open_json(&response_frame).unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_value(opened.business_payload_json).unwrap();
        let error = response.error.unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(error.code, "remote_permission_denied");
        assert!(!frame_debug.contains(prompt));

        cleanup_db(db_path);
    }

    async fn pair_mobile(
        runtime: &RelayClientRuntime,
    ) -> (
        RelaySession,
        HashMap<RelaySessionId, ActiveRelaySession>,
        RelayClientSettings,
    ) {
        let settings = runtime.get_settings().await;
        let mobile_keypair = RelayKeypair::from_private_key_bytes([13_u8; 32]);
        let mobile_peer_id = RelayPeerId::new();
        let mut pc_sessions = HashMap::new();
        let pair = RelayBridgeMessage {
            correlation_id: CorrelationId::new(),
            room_id: settings.room_id.clone(),
            message: RelayControlMessage::Hello(RelayHandshakeHello::new(
                settings.room_id.clone(),
                mobile_peer_id.clone(),
                mobile_keypair.public_key_base64(),
            )),
        };
        let ready_bridge = runtime.handle_bridge_message(pair, &mut pc_sessions).await;
        let RelayControlMessage::Ready(ready) = ready_bridge.message else {
            panic!("expected relay ready response");
        };
        let mobile_session = RelaySession::establish(
            &mobile_keypair,
            &ready.public_key,
            RelaySessionConfig {
                room_id: settings.room_id.clone(),
                session_id: ready.session_id,
                local_peer_id: mobile_peer_id,
                remote_peer_id: settings.pc_peer_id.clone(),
            },
        )
        .unwrap();
        (mobile_session, pc_sessions, settings)
    }

    /// Session-creation-only stand-in so relay tests can run against an
    /// AgentManager without spawning real provider runtimes.
    fn test_agent_manager(label: &str) -> (std::path::PathBuf, Arc<AgentManager>) {
        let db_path = std::env::temp_dir().join(format!(
            "vibex-desktop-relay-{label}-{}.db",
            RequestId::new().as_str()
        ));
        (
            db_path.clone(),
            Arc::new(AgentManager::new(&db_path).unwrap()),
        )
    }

    async fn create_mock_session(manager: &AgentManager, title: &str) -> vibex_core::AgentSession {
        let mut conn = open_database(manager.database_path()).unwrap();
        apply_migrations(&mut conn).unwrap();
        let workspace_root = format!("/tmp/vibex-desktop-relay-{title}");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = vibex_core::unix_timestamp_ms();
        let session = vibex_core::AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: title.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        session
    }

    fn pair_device(
        db_path: &std::path::Path,
        permission_level: RemoteDevicePermissionLevel,
        display_name: &str,
    ) -> RemoteAuthProof {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let created = RemoteTrustService::create_pairing_code(
            &conn,
            RemoteCreatePairingCodeRequest {
                permission_level,
                ttl_ms: Some(60_000),
            },
        )
        .unwrap();
        let claimed = RemoteTrustService::claim_pairing_code(
            &conn,
            RemoteClaimPairingCodeRequest {
                pairing_code: created.pairing_code,
                display_name: display_name.to_string(),
                public_key: None,
            },
        )
        .unwrap();
        RemoteAuthProof {
            device_id: claimed.device.device_id,
            auth_token: claimed.auth_token,
        }
    }

    fn revoke_device(db_path: &std::path::Path, auth: &RemoteAuthProof) {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let stored = RemoteDeviceRepository::get(&conn, &auth.device_id)
            .unwrap()
            .unwrap();
        RemoteDeviceRepository::revoke(&conn, &stored.detail.device_id, unix_timestamp_ms())
            .unwrap();
    }

    fn cleanup_db(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
