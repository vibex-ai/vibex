use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;
use vibex_core::{
    AgentNotificationIntent, AgentNotificationKind, AgentSession, CorrelationId,
    CreateAgentSessionRequest, FetchTimelineRequest, ImActionId, ImBindingId, ImBindingStatus,
    ImBindingSummary, ImChannelConfig, ImChannelConfigEnvelope, ImChannelDraft, ImChannelId,
    ImChannelKind, ImChannelState, ImChannelStatus, ImChannelSummary, ImChannelTestReport,
    ImMessageId, ImPairingCodeInfo, ImPairingId, ImPairingRequest, ImPrincipalId, ImPrincipalRole,
    ImPrincipalStatus, ImPrincipalSummary, ImSessionDefaults, MessageProvenance, PermissionRequest,
    PermissionRequestStatus, PermissionResolution, PermissionResponseKind,
    PermissionResponseOption, ResolvePermissionRequest, SendAgentMessageRequest, TimelineLiveEvent,
    TimelinePayload, VibexError, VibexResult, VibexSessionId, unix_timestamp_ms,
};
use vibex_db::{
    ImActionRecord, ImActionRepository, ImApprovalCardRecord, ImApprovalCardRepository,
    ImAuditRecord, ImAuditRepository, ImBindingRecord, ImBindingRepository, ImChannelRecord,
    ImChannelRepository, ImChannelSecretRecord, ImChannelSecretRepository, ImMessageRecord,
    ImMessageRepository, ImPairingRecord, ImPairingRepository, ImPrincipalRecord,
    ImPrincipalRepository,
};

use crate::adapter::ImAdapter;
use crate::agent_port::ImAgentPort;
use crate::error::ImError;
use crate::i18n::ImLocale;
use crate::render;
use crate::telegram::TelegramAdapter;
use crate::types::{ImButton, ImChatType, ImInboundEvent, ImSentMessage, ImTarget};

const PAIRING_TTL_MS: i64 = 10 * 60 * 1000;
const PAIRING_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const DEFAULT_SESSION_LIST_LIMIT: usize = 5;

#[derive(Clone)]
pub struct ImChannelServiceConfig {
    pub db_path: PathBuf,
    pub home_dir: PathBuf,
    pub agent: Arc<dyn ImAgentPort>,
    /// Test-only override for the Telegram API root.
    pub telegram_api_base: Option<String>,
    pub locale: Option<String>,
}

pub struct ImChannelService {
    inner: Arc<Inner>,
}

#[path = "service_approvals.rs"]
mod approvals;
#[path = "service_events.rs"]
mod events;

struct Inner {
    config: ImChannelServiceConfig,
    agent: Arc<dyn ImAgentPort>,
    channels: RwLock<HashMap<ImChannelId, Arc<ChannelRuntime>>>,
    statuses: RwLock<HashMap<ImChannelId, Arc<Mutex<ImChannelStatus>>>>,
    inbound_tx: mpsc::Sender<ImInboundEvent>,
    inbound_rx: AsyncMutex<Option<mpsc::Receiver<ImInboundEvent>>>,
    shutdown: watch::Sender<bool>,
    router_task: AsyncMutex<Option<JoinHandle<()>>>,
    event_task: AsyncMutex<Option<JoinHandle<()>>>,
    session_choices: Mutex<HashMap<ImPrincipalId, Vec<VibexSessionId>>>,
}

struct ChannelRuntime {
    adapter: Arc<dyn ImAdapter>,
    task: JoinHandle<()>,
    send_lock: Arc<AsyncMutex<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Ok(mut task) = self.router_task.try_lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
        if let Ok(mut task) = self.event_task.try_lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
        if let Ok(channels) = self.channels.try_read() {
            for runtime in channels.values() {
                runtime.task.abort();
            }
        }
    }
}

impl ImChannelService {
    pub fn new(config: ImChannelServiceConfig) -> VibexResult<Self> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(Inner {
                agent: Arc::clone(&config.agent),
                config,
                channels: RwLock::new(HashMap::new()),
                statuses: RwLock::new(HashMap::new()),
                inbound_tx,
                inbound_rx: AsyncMutex::new(Some(inbound_rx)),
                shutdown,
                router_task: AsyncMutex::new(None),
                event_task: AsyncMutex::new(None),
                session_choices: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub async fn start(self: &Arc<Self>) -> VibexResult<()> {
        let mut conn = self.open()?;
        vibex_db::apply_migrations(&mut conn)?;
        let Some(inbound_rx) = self.inner.inbound_rx.lock().await.take() else {
            return Err(VibexError::process(
                "im_service_already_started",
                "IM channel service was already started",
            ));
        };
        let router = Arc::downgrade(self);
        *self.inner.router_task.lock().await = Some(tokio::spawn(async move {
            Self::router_loop(router, inbound_rx).await
        }));
        let events = Arc::downgrade(self);
        let shutdown = self.inner.shutdown.subscribe();
        *self.inner.event_task.lock().await = Some(tokio::spawn(async move {
            Self::event_loop(events, shutdown).await
        }));

        let records = ImChannelRepository::list(&conn)?;
        for record in records {
            if record.enabled
                && let Err(error) = self.connect_channel(&record).await
            {
                tracing::warn!(
                    target: "vibex_im",
                    channel_id = %record.channel_id,
                    error_code = %error.code,
                    "IM channel failed to connect"
                );
            }
        }
        Ok(())
    }

    pub async fn stop(&self) {
        let _ = self.inner.shutdown.send(true);
        if let Some(task) = self.inner.router_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.inner.event_task.lock().await.take() {
            task.abort();
        }
        let runtimes: Vec<Arc<ChannelRuntime>> = {
            let mut channels = self.inner.channels.write().await;
            channels.drain().map(|(_, runtime)| runtime).collect()
        };
        for runtime in runtimes {
            runtime.task.abort();
        }
        let statuses = self.inner.statuses.write().await;
        for status in statuses.values() {
            if let Ok(mut status) = status.lock() {
                status.state = ImChannelState::Stopped;
                status.since_ms = unix_timestamp_ms();
            }
        }
    }

    pub async fn list_channels(&self) -> VibexResult<Vec<ImChannelSummary>> {
        let conn = self.open()?;
        let records = ImChannelRepository::list(&conn)?;
        let mut summaries = Vec::with_capacity(records.len());
        for record in records {
            let status = self.status_for(&record.channel_id, record.enabled).await;
            summaries.push(self.summary_for(&conn, &record, status)?);
        }
        Ok(summaries)
    }

    pub async fn create_channel(&self, draft: ImChannelDraft) -> VibexResult<ImChannelSummary> {
        if draft.display_name.trim().is_empty() {
            return Err(VibexError::validation(
                "im_channel_name_required",
                "IM channel name is required",
            ));
        }
        if !matches!(draft.config, ImChannelConfig::Telegram(_)) {
            return Err(VibexError::capability(
                "im_channel_kind_unsupported",
                "this build supports Telegram IM channels only",
            ));
        }
        let now = unix_timestamp_ms();
        let channel_id = ImChannelId::new();
        let envelope = ImChannelConfigEnvelope {
            schema_version: 1,
            config: draft.config,
            default_session: draft.default_session,
        };
        let event_filter_json = if draft.event_filter.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&draft.event_filter).map_err(|_| {
                VibexError::validation("im_event_filter_invalid", "event filter is not valid JSON")
            })?)
        };
        let record = ImChannelRecord {
            channel_id: channel_id.clone(),
            display_name: draft.display_name.trim().to_string(),
            kind: ImChannelKind::Telegram,
            enabled: false,
            config_json: serde_json::to_string(&envelope).map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration could not be encoded",
                )
            })?,
            event_filter_json,
            locale: draft.locale,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let conn = self.open()?;
        ImChannelRepository::insert(&conn, &record)?;
        self.audit(
            &conn,
            &channel_id,
            None,
            "channel_created",
            Some("channel"),
            Some(channel_id.as_str()),
            "allowed",
            None,
        )?;
        let status = self.status_for(&record.channel_id, record.enabled).await;
        self.summary_for(&conn, &record, status)
    }

    pub async fn set_channel_enabled(
        &self,
        channel_id: &ImChannelId,
        enabled: bool,
    ) -> VibexResult<ImChannelSummary> {
        let conn = self.open()?;
        let record = ImChannelRepository::get(&conn, channel_id)?.ok_or_else(|| {
            VibexError::validation("im_channel_not_found", "IM channel was not found")
        })?;
        ImChannelRepository::set_enabled(&conn, channel_id, enabled, unix_timestamp_ms())?;
        if enabled {
            self.reload_channel(channel_id).await?;
        } else {
            self.disconnect_channel(channel_id).await;
        }
        let record = ImChannelRepository::get(&conn, channel_id)?.unwrap_or(record);
        let status = self.status_for(&record.channel_id, record.enabled).await;
        self.summary_for(&conn, &record, status)
    }

    pub async fn delete_channel(&self, channel_id: &ImChannelId) -> VibexResult<()> {
        self.disconnect_channel(channel_id).await;
        let conn = self.open()?;
        ImChannelSecretRepository::delete_for_channel(&conn, channel_id)?;
        ImChannelRepository::delete(&conn, channel_id)?;
        self.inner.statuses.write().await.remove(channel_id);
        self.audit(
            &conn,
            channel_id,
            None,
            "channel_deleted",
            Some("channel"),
            Some(channel_id.as_str()),
            "allowed",
            None,
        )?;
        Ok(())
    }

    pub async fn set_bot_token(&self, channel_id: &ImChannelId, token: &str) -> VibexResult<()> {
        let conn = self.open()?;
        let _record = ImChannelRepository::get(&conn, channel_id)?.ok_or_else(|| {
            VibexError::validation("im_channel_not_found", "IM channel was not found")
        })?;
        if token.trim().is_empty() {
            return Err(VibexError::validation(
                "im_bot_token_required",
                "Telegram bot token is required",
            ));
        }
        let lookup_key = format!("im:{}:bot_token", channel_id.as_str());
        vibex_config_switch::secrets::store_im_secret(&lookup_key, token.trim())?;
        let backend = vibex_config_switch::secrets::im_secret_write_backend();
        let backend = match backend {
            vibex_core::ProviderSecretBackend::HostFile => "host_file",
            _ => "os_keychain",
        };
        let record = ImChannelSecretRecord {
            channel_id: channel_id.clone(),
            secret_kind: "bot_token".to_string(),
            backend: backend.to_string(),
            lookup_key,
            setup_state: "configured".to_string(),
            redacted_hint: "configured".to_string(),
            updated_at_ms: unix_timestamp_ms(),
        };
        ImChannelSecretRepository::upsert(&conn, &record)?;
        self.reload_channel(channel_id).await?;
        Ok(())
    }

    pub async fn set_default_session(
        &self,
        channel_id: &ImChannelId,
        default_session: Option<ImSessionDefaults>,
    ) -> VibexResult<ImChannelSummary> {
        let conn = self.open()?;
        let record = ImChannelRepository::get(&conn, channel_id)?.ok_or_else(|| {
            VibexError::validation("im_channel_not_found", "IM channel was not found")
        })?;
        let mut envelope: ImChannelConfigEnvelope = serde_json::from_str(&record.config_json)
            .map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration is not valid",
                )
            })?;
        envelope.default_session = default_session;
        let updated = ImChannelRecord {
            config_json: serde_json::to_string(&envelope).map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration could not be encoded",
                )
            })?,
            updated_at_ms: unix_timestamp_ms(),
            ..record
        };
        ImChannelRepository::update(&conn, &updated)?;
        self.reload_channel(channel_id).await?;
        let status = self.status_for(&updated.channel_id, updated.enabled).await;
        self.summary_for(&conn, &updated, status)
    }

    pub async fn test_channel(&self, channel_id: &ImChannelId) -> VibexResult<ImChannelTestReport> {
        let conn = self.open()?;
        let record = ImChannelRepository::get(&conn, channel_id)?.ok_or_else(|| {
            VibexError::validation("im_channel_not_found", "IM channel was not found")
        })?;
        let (token, _) = self.telegram_credentials(&conn, &record)?;
        let envelope: ImChannelConfigEnvelope =
            serde_json::from_str(&record.config_json).map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration is not valid",
                )
            })?;
        let ImChannelConfig::Telegram(config) = envelope.config else {
            return Err(VibexError::capability(
                "im_channel_kind_unsupported",
                "this build supports Telegram IM channels only",
            ));
        };
        let adapter = TelegramAdapter::new(
            record.channel_id.clone(),
            token,
            config.chat_id,
            config.require_mention_in_groups,
            self.inner.config.telegram_api_base.clone(),
            Arc::new(Mutex::new(ImChannelStatus::default())),
        )
        .map_err(im_error_to_vibex)?;
        adapter.probe().await.map_err(im_error_to_vibex)
    }

    pub async fn create_pairing_code(
        &self,
        request: ImPairingRequest,
    ) -> VibexResult<ImPairingCodeInfo> {
        let conn = self.open()?;
        if ImChannelRepository::get(&conn, &request.channel_id)?.is_none() {
            return Err(VibexError::validation(
                "im_channel_not_found",
                "IM channel was not found",
            ));
        }
        let ttl_ms = request.ttl_ms.map(i64::from).unwrap_or(PAIRING_TTL_MS);
        if ttl_ms <= 0 || ttl_ms > 24 * 60 * 60 * 1000 {
            return Err(VibexError::validation(
                "im_pairing_ttl_invalid",
                "pairing code lifetime is out of range",
            ));
        }
        let code = generate_pairing_code();
        let code_hash = sha256_hex(&code);
        let now = unix_timestamp_ms();
        let record = ImPairingRecord {
            pairing_id: ImPairingId::new(),
            channel_id: request.channel_id.clone(),
            code_hash,
            role: request.role,
            scope: "dm".to_string(),
            target_chat_id: None,
            created_by_principal_id: None,
            expires_at_ms: now.saturating_add(ttl_ms),
            claimed_principal_id: None,
            created_at_ms: now,
            claimed_at_ms: None,
        };
        ImPairingRepository::insert(&conn, &record)?;
        self.audit(
            &conn,
            &request.channel_id,
            None,
            "pairing_created",
            Some("pairing"),
            Some(record.pairing_id.as_str()),
            "allowed",
            Some(&format!("role={}", request.role.as_str())),
        )?;
        Ok(ImPairingCodeInfo {
            channel_id: request.channel_id,
            code,
            role: request.role,
            expires_at_ms: record.expires_at_ms,
        })
    }

    pub async fn list_principals(
        &self,
        channel_id: &ImChannelId,
    ) -> VibexResult<Vec<ImPrincipalSummary>> {
        let conn = self.open()?;
        Ok(ImPrincipalRepository::list_for_channel(&conn, channel_id)?
            .into_iter()
            .map(principal_summary)
            .collect())
    }

    pub async fn revoke_principal(&self, principal_id: &ImPrincipalId) -> VibexResult<()> {
        let conn = self.open()?;
        let principal = ImPrincipalRepository::get(&conn, principal_id)?.ok_or_else(|| {
            VibexError::validation("im_principal_not_found", "IM principal was not found")
        })?;
        ImPrincipalRepository::revoke(&conn, principal_id, unix_timestamp_ms())?;
        self.audit(
            &conn,
            &principal.channel_id,
            Some(principal_id),
            "principal_revoked",
            Some("principal"),
            Some(principal_id.as_str()),
            "allowed",
            None,
        )?;
        Ok(())
    }

    pub async fn list_bindings(
        &self,
        channel_id: &ImChannelId,
    ) -> VibexResult<Vec<ImBindingSummary>> {
        let conn = self.open()?;
        Ok(ImBindingRepository::list_for_channel(&conn, channel_id)?
            .into_iter()
            .map(binding_summary)
            .collect())
    }

    pub async fn revoke_binding(&self, binding_id: &ImBindingId) -> VibexResult<()> {
        let conn = self.open()?;
        let binding = ImBindingRepository::get(&conn, binding_id)?.ok_or_else(|| {
            VibexError::validation("im_binding_not_found", "IM binding was not found")
        })?;
        ImBindingRepository::set_status(
            &conn,
            binding_id,
            ImBindingStatus::Revoked,
            unix_timestamp_ms(),
        )?;
        self.audit(
            &conn,
            &binding.channel_id,
            None,
            "binding_revoked",
            Some("binding"),
            Some(binding_id.as_str()),
            "allowed",
            None,
        )?;
        Ok(())
    }

    fn open(&self) -> VibexResult<vibex_db::DbConnection> {
        vibex_db::open_database(&self.inner.config.db_path)
    }

    fn summary_for(
        &self,
        conn: &vibex_db::DbConnection,
        record: &ImChannelRecord,
        status: ImChannelStatus,
    ) -> VibexResult<ImChannelSummary> {
        let secrets = ImChannelSecretRepository::list_for_channel(conn, &record.channel_id)?;
        let configured = secrets
            .iter()
            .any(|secret| secret.secret_kind == "bot_token" && secret.setup_state == "configured");
        let default_session = serde_json::from_str::<ImChannelConfigEnvelope>(&record.config_json)
            .ok()
            .and_then(|envelope| envelope.default_session);
        let event_filter = record
            .event_filter_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
            .filter(|filter| !filter.is_empty())
            .unwrap_or_else(|| {
                vibex_core::DEFAULT_IM_EVENT_FILTER
                    .iter()
                    .map(|id| id.to_string())
                    .collect()
            });
        Ok(ImChannelSummary {
            channel_id: record.channel_id.clone(),
            display_name: record.display_name.clone(),
            kind: record.kind,
            enabled: record.enabled,
            configured,
            default_session,
            event_filter,
            locale: record.locale.clone(),
            status,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
        })
    }

    async fn status_for(&self, channel_id: &ImChannelId, enabled: bool) -> ImChannelStatus {
        if let Some(status) = self.inner.statuses.read().await.get(channel_id) {
            return status
                .lock()
                .map(|status| status.clone())
                .unwrap_or_default();
        }
        ImChannelStatus {
            state: if enabled {
                ImChannelState::Stopped
            } else {
                ImChannelState::Disabled
            },
            ..ImChannelStatus::default()
        }
    }

    async fn status_slot(&self, channel_id: &ImChannelId) -> Arc<Mutex<ImChannelStatus>> {
        let mut statuses = self.inner.statuses.write().await;
        statuses
            .entry(channel_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ImChannelStatus::default())))
            .clone()
    }

    async fn connect_channel(&self, record: &ImChannelRecord) -> VibexResult<()> {
        if self
            .inner
            .channels
            .read()
            .await
            .contains_key(&record.channel_id)
        {
            return Ok(());
        }
        let envelope: ImChannelConfigEnvelope =
            serde_json::from_str(&record.config_json).map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration is not valid",
                )
            })?;
        let ImChannelConfig::Telegram(config) = envelope.config else {
            return Err(VibexError::capability(
                "im_channel_kind_unsupported",
                "this build supports Telegram IM channels only",
            ));
        };
        let conn = self.open()?;
        let (token, _) = self.telegram_credentials(&conn, record)?;
        let status = self.status_slot(&record.channel_id).await;
        if let Ok(mut status) = status.lock() {
            status.state = ImChannelState::Starting;
            status.since_ms = unix_timestamp_ms();
        }
        let adapter = Arc::new(
            TelegramAdapter::new(
                record.channel_id.clone(),
                token,
                config.chat_id,
                config.require_mention_in_groups,
                self.inner.config.telegram_api_base.clone(),
                status,
            )
            .map_err(im_error_to_vibex)?,
        );
        let task = adapter.spawn(
            self.inner.inbound_tx.clone(),
            self.inner.shutdown.subscribe(),
        );
        let adapter: Arc<dyn ImAdapter> = adapter;
        let runtime = ChannelRuntime {
            adapter,
            task,
            send_lock: Arc::new(AsyncMutex::new(())),
        };
        self.inner
            .channels
            .write()
            .await
            .insert(record.channel_id.clone(), Arc::new(runtime));
        Ok(())
    }

    async fn disconnect_channel(&self, channel_id: &ImChannelId) {
        if let Some(runtime) = self.inner.channels.write().await.remove(channel_id) {
            runtime.task.abort();
        }
        if let Some(status) = self.inner.statuses.read().await.get(channel_id)
            && let Ok(mut status) = status.lock()
        {
            status.state = ImChannelState::Stopped;
            status.since_ms = unix_timestamp_ms();
        }
    }

    async fn reload_channel(&self, channel_id: &ImChannelId) -> VibexResult<()> {
        self.disconnect_channel(channel_id).await;
        let conn = self.open()?;
        let Some(record) = ImChannelRepository::get(&conn, channel_id)? else {
            return Ok(());
        };
        if record.enabled {
            self.connect_channel(&record).await?;
        }
        Ok(())
    }

    fn telegram_credentials(
        &self,
        conn: &vibex_db::DbConnection,
        record: &ImChannelRecord,
    ) -> VibexResult<(String, String)> {
        let secret = ImChannelSecretRepository::get(conn, &record.channel_id, "bot_token")?
            .ok_or_else(|| {
                VibexError::validation(
                    "im_bot_token_missing",
                    "Telegram bot token is not configured",
                )
            })?;
        let token = vibex_config_switch::secrets::resolve_im_secret(&secret.lookup_key)?
            .ok_or_else(|| {
                VibexError::validation(
                    "im_bot_token_missing",
                    "Telegram bot token is not configured",
                )
            })?;
        Ok((token, secret.lookup_key))
    }

    async fn router_loop(this: Weak<Self>, mut rx: mpsc::Receiver<ImInboundEvent>) {
        while let Some(event) = rx.recv().await {
            let Some(service) = this.upgrade() else {
                break;
            };
            if let Err(error) = service.handle_inbound(event.clone()).await {
                tracing::warn!(
                    target: "vibex_im",
                    error_code = %error.code,
                    "IM inbound message handling failed"
                );
                service.reply_error(&event, &error).await;
            }
        }
    }

    async fn reply_error(&self, event: &ImInboundEvent, error: &VibexError) {
        let Some(runtime) = self
            .inner
            .channels
            .read()
            .await
            .get(&event.channel_id)
            .cloned()
        else {
            return;
        };
        let text = format!("Vibex error ({}): {}", error.code, error.message);
        let _ = self.reply(event, &runtime, &text).await;
    }

    async fn handle_inbound(&self, event: ImInboundEvent) -> VibexResult<()> {
        let conn = self.open()?;
        let Some(record) = ImChannelRepository::get(&conn, &event.channel_id)? else {
            return Ok(());
        };
        if !record.enabled {
            return Ok(());
        }
        let message_id = ImMessageId::new().into_string();
        let preview = render::truncate(&event.text, 256);
        let inserted = ImMessageRepository::insert(
            &conn,
            &ImMessageRecord {
                message_id,
                channel_id: event.channel_id.clone(),
                direction: "inbound".to_string(),
                provider_update_id: Some(event.provider_update_id.clone()),
                external_message_id: event.external_message_id.clone(),
                binding_id: None,
                principal_id: None,
                kind: if event.text.starts_with('/') {
                    "command".to_string()
                } else {
                    "message".to_string()
                },
                content_preview: (!preview.is_empty()).then_some(preview),
                content_sha256: (!event.text.is_empty()).then(|| sha256_hex(&event.text)),
                status: "received".to_string(),
                error_code: None,
                error_detail: None,
                created_at_ms: event.received_at_ms,
            },
        )?;
        if !inserted {
            return Ok(());
        }
        let Some(runtime) = self
            .inner
            .channels
            .read()
            .await
            .get(&event.channel_id)
            .cloned()
        else {
            return Ok(());
        };
        runtime.adapter.mark_inbound();
        if let Some(callback) = &event.callback {
            let action_id = callback.action_id.clone();
            if let Some(callback_query_id) = callback.callback_query_id.as_deref() {
                let _ = runtime
                    .adapter
                    .answer_callback(callback_query_id, None)
                    .await;
            }
            let Some(action_id) = action_id else {
                self.reply(&event, &runtime, "This action is no longer available.")
                    .await?;
                return Ok(());
            };
            return self.handle_callback(&event, &runtime, &action_id).await;
        }

        let principal = ImPrincipalRepository::get_by_external(
            &conn,
            &event.channel_id,
            &event.external_user_id,
        )?;
        let Some(principal) = principal else {
            if event.chat_type != ImChatType::Direct {
                return Ok(());
            }
            if event.text.trim_start().starts_with("/pair") {
                return self.handle_pairing(&event, &runtime).await;
            }
            self.reply(
                &event,
                &runtime,
                "This bot is not paired with you yet. Ask the owner for a pairing code, then send /pair <code>.",
            )
            .await?;
            return Ok(());
        };
        if principal.status != ImPrincipalStatus::Active {
            self.reply(
                &event,
                &runtime,
                "Your IM identity has been revoked. Ask the owner to pair you again.",
            )
            .await?;
            return Ok(());
        }
        ImPrincipalRepository::touch(&conn, &principal.principal_id, unix_timestamp_ms())?;

        let mut text = event.text.clone();
        if event.chat_type != ImChatType::Direct {
            if !runtime.adapter.is_group_trigger(&text).await {
                return Ok(());
            }
            if text.trim_start().starts_with('@') {
                text = text
                    .trim_start()
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_default();
            }
        }
        if text.trim_start().starts_with('/') {
            self.handle_command(&event, &runtime, &principal, &text)
                .await
        } else {
            self.handle_plain_text(&event, &runtime, &principal, &text)
                .await
        }
    }

    async fn handle_pairing(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
    ) -> VibexResult<()> {
        if event.chat_type != ImChatType::Direct {
            self.reply(
                event,
                runtime,
                "Pairing codes are accepted in a direct message only.",
            )
            .await?;
            return Ok(());
        }
        let code = event
            .text
            .trim()
            .strip_prefix("/pair")
            .unwrap_or_default()
            .trim();
        if code.is_empty() {
            self.reply(
                event,
                runtime,
                "Send /pair <code> with the code from Vibex settings.",
            )
            .await?;
            return Ok(());
        }
        let code_hash = sha256_hex(&code.to_uppercase());
        let now = unix_timestamp_ms();
        let mut conn = self.open()?;
        let Some(pairing) = ImPairingRepository::claim(&conn, &code_hash, now)? else {
            drop(conn);
            self.reply(event, runtime, "That pairing code is invalid or expired.")
                .await?;
            return Ok(());
        };
        {
            let tx = conn.transaction().map_err(|_| {
                VibexError::storage(
                    "im_pairing_transaction_failed",
                    "pairing transaction failed",
                )
            })?;
            let existing = ImPrincipalRepository::get_by_external(
                &tx,
                &event.channel_id,
                &event.external_user_id,
            )?;
            let principal_id = existing
                .as_ref()
                .map(|principal| principal.principal_id.clone())
                .unwrap_or_else(ImPrincipalId::new);
            let principal = ImPrincipalRecord {
                principal_id: principal_id.clone(),
                channel_id: event.channel_id.clone(),
                external_user_id: event.external_user_id.clone(),
                display_name: event.sender_display_name.clone(),
                role: pairing.role,
                status: ImPrincipalStatus::Active,
                paired_at_ms: Some(now),
                last_seen_at_ms: Some(now),
                revoked_at_ms: None,
                created_at_ms: existing
                    .as_ref()
                    .map(|principal| principal.created_at_ms)
                    .unwrap_or(now),
                updated_at_ms: now,
            };
            ImPrincipalRepository::upsert_active(&tx, &principal)?;
            ImPairingRepository::set_claimed_principal(&tx, &pairing.pairing_id, &principal_id)?;
            self.audit(
                &tx,
                &event.channel_id,
                Some(&principal_id),
                "pairing_claimed",
                Some("pairing"),
                Some(pairing.pairing_id.as_str()),
                "allowed",
                Some(&format!("role={}", pairing.role.as_str())),
            )?;
            tx.commit().map_err(|_| {
                VibexError::storage("im_pairing_commit_failed", "pairing transaction failed")
            })?;
        }
        drop(conn);
        let role = role_label(pairing.role, self.locale(event));
        self.reply(
            event,
            runtime,
            &format!("Paired as {role}. Send /help to see what you can do."),
        )
        .await?;
        Ok(())
    }

    async fn handle_callback(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        action_id: &ImActionId,
    ) -> VibexResult<()> {
        let conn = self.open()?;
        let Some(principal) = ImPrincipalRepository::get_by_external(
            &conn,
            &event.channel_id,
            &event.external_user_id,
        )?
        else {
            self.reply(event, runtime, "Pair this chat before approving.")
                .await?;
            return Ok(());
        };
        if principal.status != ImPrincipalStatus::Active || !principal.role.can_approve() {
            self.reply(event, runtime, "You do not have permission to approve.")
                .await?;
            return Ok(());
        }
        let Some(action) = ImActionRepository::get(&conn, action_id)? else {
            self.reply(event, runtime, "This action is no longer available.")
                .await?;
            return Ok(());
        };
        if action.consumed_at_ms.is_some() {
            self.reply(event, runtime, "This action was already handled.")
                .await?;
            return Ok(());
        }
        if action.expires_at_ms <= unix_timestamp_ms() {
            self.reply(event, runtime, "This action has expired.")
                .await?;
            return Ok(());
        }
        if action.channel_id != event.channel_id {
            self.reply(event, runtime, "This action belongs to another channel.")
                .await?;
            return Ok(());
        }
        let payload: ApprovalActionPayload =
            serde_json::from_str(&action.payload_json).map_err(|_| {
                VibexError::validation(
                    "im_action_payload_invalid",
                    "approval action payload is invalid",
                )
            })?;
        let Some(binding) = ImBindingRepository::get(&conn, &action.binding_id)? else {
            self.reply(event, runtime, "This conversation is no longer bound.")
                .await?;
            return Ok(());
        };
        if binding.external_chat_id != event.external_chat_id
            || binding.thread_key != event.thread_key
        {
            self.reply(event, runtime, "This action belongs to another chat.")
                .await?;
            return Ok(());
        }
        let Some(request) = self
            .pending_permission(&binding.session_id, Some(&payload.request_id))
            .await?
        else {
            self.reply(
                event,
                runtime,
                "That approval request is no longer pending.",
            )
            .await?;
            return Ok(());
        };
        let Some(option) = request
            .response_options
            .iter()
            .find(|option| option.option_id == payload.option_id)
            .cloned()
        else {
            self.reply(
                event,
                runtime,
                "That approval option is no longer available.",
            )
            .await?;
            return Ok(());
        };
        if !ImActionRepository::consume(
            &conn,
            action_id,
            &principal.principal_id,
            unix_timestamp_ms(),
        )? {
            self.reply(event, runtime, "This action was already handled.")
                .await?;
            return Ok(());
        }
        let status = self
            .apply_resolution(
                event,
                runtime,
                &binding,
                &request,
                &option,
                &principal,
                action.card_id.as_deref(),
            )
            .await?;
        self.reply(event, runtime, &status).await?;
        Ok(())
    }

    async fn handle_command(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        text: &str,
    ) -> VibexResult<()> {
        let mut parts = text.trim_start().splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default().to_ascii_lowercase();
        let args = parts.next().unwrap_or_default().trim();
        let locale = self.locale(event);
        match command.as_str() {
            "/start" | "/help" => {
                self.reply(event, runtime, &render::help(locale, "/"))
                    .await?;
            }
            "/whoami" => {
                let role = role_label(principal.role, locale);
                self.reply(
                    event,
                    runtime,
                    &format!(
                        "Vibex IM identity\nUser: {}\nRole: {role}\nChannel: {}",
                        principal.display_name, event.channel_id
                    ),
                )
                .await?;
            }
            "/status" => self.handle_status(event, runtime, principal).await?,
            "/sessions" => self.handle_sessions(event, runtime, principal).await?,
            "/use" => self.handle_use(event, runtime, principal, args).await?,
            "/new" => self.handle_new(event, runtime, principal, args).await?,
            "/stop" => self.handle_stop(event, runtime, principal).await?,
            "/approve" => {
                self.handle_text_approval(event, runtime, principal, args, ApprovalChoice::Approve)
                    .await?
            }
            "/always" => {
                self.handle_text_approval(event, runtime, principal, args, ApprovalChoice::Always)
                    .await?
            }
            "/deny" => {
                self.handle_text_approval(event, runtime, principal, args, ApprovalChoice::Deny)
                    .await?
            }
            "/say" => {
                if !principal.role.can_prompt() {
                    self.reply(event, runtime, "Your role cannot send prompts.")
                        .await?;
                } else if args.is_empty() {
                    self.reply(event, runtime, "Usage: /say <text>").await?;
                } else {
                    self.handle_plain_text(event, runtime, principal, args)
                        .await?;
                }
            }
            "/unbind" => self.handle_unbind(event, runtime, principal).await?,
            _ => {
                self.reply(
                    event,
                    runtime,
                    &format!("Unknown command {command}. Send /help for the command list."),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_status(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                "No session is bound. Use /sessions or /new.",
            )
            .await?;
            return Ok(());
        };
        let session = self.inner.agent.get_session(&binding.session_id).await?;
        let pending =
            ImApprovalCardRepository::list_pending_for_binding(&conn, &binding.binding_id)?;
        let role = role_label(principal.role, self.locale(event));
        self.reply(
            event,
            runtime,
            &format!(
                "Vibex session\n#{} · {}\nState: {}\nAgent: {}\nApprovals pending: {}\nRole: {role}",
                render::short_session_id(&session),
                session.title,
                render::state_label(&session, self.locale(event)),
                session.agent_id,
                pending.len(),
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_sessions(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        let mut sessions = self.inner.agent.list_sessions(false).await?;
        sessions.retain(|session| session.deleted_at_ms.is_none());
        sessions.sort_by_key(|session| std::cmp::Reverse(session.last_message_at_ms));
        sessions.truncate(DEFAULT_SESSION_LIST_LIMIT);
        if let Ok(mut choices) = self.inner.session_choices.lock() {
            choices.insert(
                principal.principal_id.clone(),
                sessions.iter().map(|session| session.id.clone()).collect(),
            );
        }
        self.reply(
            event,
            runtime,
            &render::session_list(&sessions, self.locale(event)),
        )
        .await?;
        Ok(())
    }

    async fn handle_use(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        args: &str,
    ) -> VibexResult<()> {
        if !principal.role.can_approve() {
            self.reply(event, runtime, "Your role cannot bind sessions.")
                .await?;
            return Ok(());
        }
        if args.is_empty() {
            self.reply(event, runtime, "Usage: /use <n|session-id>")
                .await?;
            return Ok(());
        }
        let session_id = if let Ok(index) = args.parse::<usize>() {
            self.inner
                .session_choices
                .lock()
                .ok()
                .and_then(|choices| choices.get(&principal.principal_id).cloned())
                .and_then(|choices| choices.get(index.saturating_sub(1)).cloned())
        } else {
            let sessions = self.inner.agent.list_sessions(false).await?;
            sessions
                .into_iter()
                .find(|session| {
                    session.id.as_str() == args
                        || render::short_session_id(session).eq_ignore_ascii_case(args)
                })
                .map(|session| session.id)
        };
        let Some(session_id) = session_id else {
            self.reply(event, runtime, "Session not found. Send /sessions first.")
                .await?;
            return Ok(());
        };
        let session = self.inner.agent.get_session(&session_id).await?;
        let conn = self.open()?;
        let binding = self.make_binding(event, principal, &session, None);
        ImBindingRepository::upsert(&conn, &binding)?;
        self.reply(
            event,
            runtime,
            &format!(
                "Bound to #{} · {}",
                render::short_session_id(&session),
                session.title
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_new(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        prompt: &str,
    ) -> VibexResult<()> {
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot start sessions.")
                .await?;
            return Ok(());
        }
        if prompt.trim().is_empty() {
            self.reply(event, runtime, "Usage: /new <prompt>").await?;
            return Ok(());
        }
        let conn = self.open()?;
        let record = ImChannelRepository::get(&conn, &event.channel_id)?.ok_or_else(|| {
            VibexError::validation("im_channel_not_found", "IM channel was not found")
        })?;
        let envelope: ImChannelConfigEnvelope =
            serde_json::from_str(&record.config_json).map_err(|_| {
                VibexError::validation(
                    "im_channel_config_invalid",
                    "IM channel configuration is not valid",
                )
            })?;
        let topic_mode = matches!(
            &envelope.config,
            ImChannelConfig::Telegram(config) if config.topic_mode
        );
        let (workspace_root, workspace_mode, runtime_selection, safety) = if let Some(defaults) =
            envelope.default_session
        {
            let runtime_selection = defaults.runtime_selection();
            (
                defaults.workspace_root,
                defaults.workspace_mode,
                runtime_selection,
                vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            )
        } else if let Some(binding) = self.binding_for(&conn, event)? {
            let session = self.inner.agent.get_session(&binding.session_id).await?;
            let selection = self
                .inner
                .agent
                .runtime_selection(&binding.session_id)
                .await?;
            (
                session.workspace_root,
                session.workspace_mode,
                selection.desired,
                session.safety,
            )
        } else {
            self.reply(
                    event,
                    runtime,
                    "This channel has no default workspace and Agent. Bind a session with /use first, or configure defaults in Vibex settings.",
                )
                .await?;
            return Ok(());
        };
        let session = self
            .inner
            .agent
            .create_session(CreateAgentSessionRequest {
                runtime: runtime_selection,
                workspace_root,
                workspace_mode,
                title: None,
                safety: Some(safety),
                session_id: None,
                defer_runtime_materialization: true,
                tools: Default::default(),
            })
            .await?;
        let mut reply_event = event.clone();
        if topic_mode && event.chat_type == ImChatType::Group && event.thread_key.is_empty() {
            match runtime.adapter.create_thread(&topic_title(prompt)).await {
                Ok(thread) => {
                    reply_event.thread_key = thread.thread_key;
                    reply_event.thread_kind = Some(thread.thread_kind);
                }
                Err(error) => {
                    self.reply(
                        event,
                        runtime,
                        &format!("Could not create a Telegram topic: {error}"),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        let binding = self.make_binding(&reply_event, principal, &session, None);
        ImBindingRepository::upsert(&conn, &binding)?;
        self.send_prompt(&reply_event, &session.id, prompt).await?;
        self.reply(
            &reply_event,
            runtime,
            &format!(
                "Created #{} · {}\nSend /status to follow it.",
                render::short_session_id(&session),
                session.title
            ),
        )
        .await?;
        Ok(())
    }

    async fn handle_stop(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot stop sessions.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "No session is bound.").await?;
            return Ok(());
        };
        self.inner.agent.interrupt(&binding.session_id).await?;
        self.reply(event, runtime, "Stopped the current turn.")
            .await?;
        Ok(())
    }

    async fn handle_text_approval(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        code: &str,
        choice: ApprovalChoice,
    ) -> VibexResult<()> {
        if !principal.role.can_approve() {
            self.reply(event, runtime, "Your role cannot approve.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "No session is bound.").await?;
            return Ok(());
        };
        let (request, action_card) = if code.trim().is_empty() {
            let cards =
                ImApprovalCardRepository::list_pending_for_binding(&conn, &binding.binding_id)?;
            let Some(card) = cards.first() else {
                self.reply(event, runtime, "There is no pending approval.")
                    .await?;
                return Ok(());
            };
            let Some(request) = self
                .pending_permission(&binding.session_id, Some(&card.request_id))
                .await?
            else {
                self.reply(
                    event,
                    runtime,
                    "That approval request is no longer pending.",
                )
                .await?;
                return Ok(());
            };
            (request, Some(card.card_id.clone()))
        } else {
            let Some(action) = ImActionRepository::get_pending_by_short_code(
                &conn,
                &binding.binding_id,
                &code.trim().to_uppercase(),
                unix_timestamp_ms(),
            )?
            else {
                self.reply(event, runtime, "That approval code is invalid or expired.")
                    .await?;
                return Ok(());
            };
            let payload: ApprovalActionPayload = serde_json::from_str(&action.payload_json)
                .map_err(|_| {
                    VibexError::validation(
                        "im_action_payload_invalid",
                        "approval action payload is invalid",
                    )
                })?;
            if !ImActionRepository::consume(
                &conn,
                &action.action_id,
                &principal.principal_id,
                unix_timestamp_ms(),
            )? {
                self.reply(event, runtime, "That approval was already handled.")
                    .await?;
                return Ok(());
            }
            let Some(request) = self
                .pending_permission(&binding.session_id, Some(&payload.request_id))
                .await?
            else {
                self.reply(
                    event,
                    runtime,
                    "That approval request is no longer pending.",
                )
                .await?;
                return Ok(());
            };
            (request, Some(action.card_id.unwrap_or_default()))
        };
        let Some(option) = choose_option(&request, choice) else {
            self.reply(event, runtime, "That approval option is not available.")
                .await?;
            return Ok(());
        };
        let status = self
            .apply_resolution(
                event,
                runtime,
                &binding,
                &request,
                &option,
                principal,
                action_card.as_deref(),
            )
            .await?;
        self.reply(event, runtime, &status).await?;
        Ok(())
    }

    async fn handle_unbind(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        if !principal.role.can_admin() {
            self.reply(event, runtime, "Only an admin can unbind this chat.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "This chat is not bound.")
                .await?;
            return Ok(());
        };
        ImBindingRepository::set_status(
            &conn,
            &binding.binding_id,
            ImBindingStatus::Revoked,
            unix_timestamp_ms(),
        )?;
        self.reply(event, runtime, "This chat is no longer bound to a session.")
            .await?;
        Ok(())
    }

    async fn handle_plain_text(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        text: &str,
    ) -> VibexResult<()> {
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot send prompts.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                "No session is bound. Use /sessions or /new.",
            )
            .await?;
            return Ok(());
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        self.send_prompt(event, &binding.session_id, text).await?;
        self.reply(event, runtime, "Sent. I will report back here.")
            .await?;
        Ok(())
    }

    async fn send_prompt(
        &self,
        event: &ImInboundEvent,
        session_id: &VibexSessionId,
        text: &str,
    ) -> VibexResult<()> {
        let runtime = self.inner.agent.runtime_selection(session_id).await?;
        let key = sha256_hex(&format!(
            "{}:{}:{}",
            event.channel_id, event.provider_update_id, session_id
        ));
        self.inner
            .agent
            .send_message(SendAgentMessageRequest {
                session_id: session_id.clone(),
                message_idempotency_key: key,
                desired_runtime: runtime.desired,
                text: text.to_string(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                reasoning_effort: None,
                correlation_id: Some(CorrelationId::new()),
                delivery: Default::default(),
                prompt_context: None,
                provenance: MessageProvenance::human_input(),
            })
            .await?;
        Ok(())
    }

    fn binding_for(
        &self,
        conn: &vibex_db::DbConnection,
        event: &ImInboundEvent,
    ) -> VibexResult<Option<ImBindingRecord>> {
        ImBindingRepository::get_by_target(
            conn,
            &event.channel_id,
            &event.external_chat_id,
            &event.thread_key,
        )
    }

    fn make_binding(
        &self,
        event: &ImInboundEvent,
        principal: &ImPrincipalRecord,
        session: &AgentSession,
        existing: Option<&ImBindingRecord>,
    ) -> ImBindingRecord {
        let now = unix_timestamp_ms();
        ImBindingRecord {
            binding_id: existing
                .map(|binding| binding.binding_id.clone())
                .unwrap_or_default(),
            channel_id: event.channel_id.clone(),
            external_chat_id: event.external_chat_id.clone(),
            thread_key: event.thread_key.clone(),
            thread_kind: event.thread_kind.clone(),
            session_id: session.id.clone(),
            created_by_principal_id: Some(principal.principal_id.clone()),
            status: ImBindingStatus::Active,
            last_delivered_sequence: 0,
            title: Some(session.title.clone()),
            title_sync_enabled: matches!(event.chat_type, ImChatType::Group)
                && !event.thread_key.is_empty(),
            provider_payload_json: None,
            created_at_ms: existing.map(|binding| binding.created_at_ms).unwrap_or(now),
            updated_at_ms: now,
        }
    }

    async fn send_to_binding(
        &self,
        binding: &ImBindingRecord,
        text: &str,
    ) -> VibexResult<Option<ImSentMessage>> {
        let Some(runtime) = self
            .inner
            .channels
            .read()
            .await
            .get(&binding.channel_id)
            .cloned()
        else {
            return Ok(None);
        };
        let target = ImTarget {
            chat_id: binding.external_chat_id.clone(),
            thread_key: binding.thread_key.clone(),
        };
        let sent = runtime
            .adapter
            .send_text(&target, text, &[])
            .await
            .map_err(im_error_to_vibex)?;
        Ok(Some(sent))
    }

    async fn reply(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        text: &str,
    ) -> VibexResult<()> {
        let target = ImTarget {
            chat_id: event.external_chat_id.clone(),
            thread_key: event.thread_key.clone(),
        };
        let _guard = runtime.send_lock.lock().await;
        runtime
            .adapter
            .send_text(&target, text, &[])
            .await
            .map_err(im_error_to_vibex)?;
        Ok(())
    }

    fn locale(&self, event: &ImInboundEvent) -> ImLocale {
        let _ = event;
        ImLocale::parse(self.inner.config.locale.as_deref())
    }

    fn locale_for_channel(&self, channel_id: &ImChannelId) -> ImLocale {
        let _ = channel_id;
        ImLocale::parse(self.inner.config.locale.as_deref())
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        conn: &vibex_db::DbConnection,
        channel_id: &ImChannelId,
        principal_id: Option<&ImPrincipalId>,
        action: &str,
        target_kind: Option<&str>,
        target_id: Option<&str>,
        outcome: &str,
        detail: Option<&str>,
    ) -> VibexResult<()> {
        ImAuditRepository::insert(
            conn,
            &ImAuditRecord {
                audit_id: format!("imaudit_{}", Uuid::new_v4().simple()),
                channel_id: channel_id.clone(),
                principal_id: principal_id.cloned(),
                action: action.to_string(),
                target_kind: target_kind.map(str::to_string),
                target_id: target_id.map(str::to_string),
                outcome: outcome.to_string(),
                detail_json: detail.map(str::to_string),
                created_at_ms: unix_timestamp_ms(),
            },
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum ApprovalChoice {
    Approve,
    Always,
    Deny,
}

impl ApprovalChoice {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Always => "always",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ApprovalActionPayload {
    request_id: String,
    option_id: String,
    choice: String,
}

fn choose_option(
    request: &PermissionRequest,
    choice: ApprovalChoice,
) -> Option<PermissionResponseOption> {
    let kind = match choice {
        ApprovalChoice::Approve => PermissionResponseKind::Approve,
        ApprovalChoice::Always => PermissionResponseKind::AlwaysAllowForSession,
        ApprovalChoice::Deny => PermissionResponseKind::Deny,
    };
    request
        .response_options
        .iter()
        .find(|option| option.response == kind)
        .cloned()
        .or_else(|| {
            (choice_is_always(choice))
                .then(|| {
                    request
                        .response_options
                        .iter()
                        .find(|option| option.response == PermissionResponseKind::Approve)
                })
                .flatten()
                .cloned()
        })
}

const fn choice_is_always(choice: ApprovalChoice) -> bool {
    matches!(choice, ApprovalChoice::Always)
}

fn topic_title(prompt: &str) -> String {
    format!("Vibex · {}", render::truncate(prompt.trim(), 112))
        .chars()
        .take(128)
        .collect()
}

fn role_label(role: ImPrincipalRole, locale: ImLocale) -> &'static str {
    match (role, locale) {
        (ImPrincipalRole::Observer, ImLocale::ZhCn) => "只读",
        (ImPrincipalRole::Observer, ImLocale::ZhTw) => "唯讀",
        (ImPrincipalRole::Observer, _) => "observer",
        (ImPrincipalRole::Approver, ImLocale::ZhCn) => "审批",
        (ImPrincipalRole::Approver, ImLocale::ZhTw) => "審批",
        (ImPrincipalRole::Approver, _) => "approver",
        (ImPrincipalRole::Operator, ImLocale::ZhCn) => "操作",
        (ImPrincipalRole::Operator, ImLocale::ZhTw) => "操作",
        (ImPrincipalRole::Operator, _) => "operator",
        (ImPrincipalRole::Admin, ImLocale::ZhCn) => "管理员",
        (ImPrincipalRole::Admin, ImLocale::ZhTw) => "管理員",
        (ImPrincipalRole::Admin, _) => "admin",
    }
}

fn generate_pairing_code() -> String {
    let entropy = Uuid::new_v4().simple().to_string();
    let mut chars = entropy.bytes().filter_map(|byte| {
        let index = usize::from(byte) % PAIRING_CODE_ALPHABET.len();
        PAIRING_CODE_ALPHABET.get(index).copied()
    });
    let mut code = String::new();
    for _ in 0..10 {
        if let Some(byte) = chars.next() {
            code.push(char::from(byte));
        }
    }
    while code.len() < 10 {
        code.push('K');
    }
    format!("VBX-{}-{}", &code[..5], &code[5..])
}

fn action_short_code(action_id: &ImActionId) -> String {
    action_id
        .as_str()
        .rsplit('_')
        .next()
        .unwrap_or_default()
        .chars()
        .take(6)
        .collect::<String>()
        .to_uppercase()
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn timeline_event_id(payload: &TimelinePayload) -> Option<&'static str> {
    match payload {
        TimelinePayload::AgentMessage(message) if message.is_final => Some("turn_complete"),
        TimelinePayload::PermissionRequest(_) => Some("permission_request"),
        TimelinePayload::PermissionResolution(_) => Some("permission_resolution"),
        TimelinePayload::ElicitationRequest(_) => Some("elicitation_request"),
        TimelinePayload::ElicitationResolution(_) => Some("elicitation_resolution"),
        TimelinePayload::Error(_) => Some("error"),
        _ => None,
    }
}

fn event_allowed(record: &ImChannelRecord, event_id: &str) -> bool {
    let filter = record
        .event_filter_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .filter(|filter| !filter.is_empty());
    match filter {
        Some(filter) => filter.iter().any(|id| id == event_id),
        None => vibex_core::DEFAULT_IM_EVENT_FILTER.contains(&event_id),
    }
}

fn principal_summary(record: ImPrincipalRecord) -> ImPrincipalSummary {
    ImPrincipalSummary {
        principal_id: record.principal_id,
        channel_id: record.channel_id,
        external_user_id: record.external_user_id,
        display_name: record.display_name,
        role: record.role,
        status: record.status,
        paired_at_ms: record.paired_at_ms,
        last_seen_at_ms: record.last_seen_at_ms,
        revoked_at_ms: record.revoked_at_ms,
    }
}

fn binding_summary(record: ImBindingRecord) -> ImBindingSummary {
    ImBindingSummary {
        binding_id: record.binding_id,
        channel_id: record.channel_id,
        external_chat_id: record.external_chat_id,
        thread_key: record.thread_key,
        thread_kind: record.thread_kind,
        session_id: record.session_id,
        status: record.status,
        title: record.title,
        last_delivered_sequence: record.last_delivered_sequence,
        updated_at_ms: record.updated_at_ms,
    }
}

fn im_error_to_vibex(error: ImError) -> VibexError {
    VibexError::process(error.code(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vibex_core::{
        AgentSessionRuntimeSelectionState, ImChannelConfigEnvelope, ImTelegramConfig,
        PermissionRiskCategory, ResolveElicitationRequest, TimelineItem, TimelinePage,
    };

    struct MockAgent {
        timeline: tokio::sync::broadcast::Sender<TimelineLiveEvent>,
        sessions: tokio::sync::broadcast::Sender<AgentSession>,
        notifications: tokio::sync::broadcast::Sender<AgentNotificationIntent>,
    }

    impl MockAgent {
        fn new() -> Self {
            let (timeline, _) = tokio::sync::broadcast::channel(8);
            let (sessions, _) = tokio::sync::broadcast::channel(8);
            let (notifications, _) = tokio::sync::broadcast::channel(8);
            Self {
                timeline,
                sessions,
                notifications,
            }
        }
    }

    #[async_trait::async_trait]
    impl ImAgentPort for MockAgent {
        fn subscribe_timeline(&self) -> tokio::sync::broadcast::Receiver<TimelineLiveEvent> {
            self.timeline.subscribe()
        }

        fn subscribe_session_updates(&self) -> tokio::sync::broadcast::Receiver<AgentSession> {
            self.sessions.subscribe()
        }

        fn subscribe_notifications(
            &self,
        ) -> tokio::sync::broadcast::Receiver<AgentNotificationIntent> {
            self.notifications.subscribe()
        }

        async fn get_session(&self, _session_id: &VibexSessionId) -> VibexResult<AgentSession> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn list_sessions(&self, _include_archived: bool) -> VibexResult<Vec<AgentSession>> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn fetch_timeline(
            &self,
            _request: FetchTimelineRequest,
        ) -> VibexResult<TimelinePage> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn runtime_selection(
            &self,
            _session_id: &VibexSessionId,
        ) -> VibexResult<AgentSessionRuntimeSelectionState> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn create_session(
            &self,
            _request: CreateAgentSessionRequest,
        ) -> VibexResult<AgentSession> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn send_message(
            &self,
            _request: SendAgentMessageRequest,
        ) -> VibexResult<Vec<TimelineItem>> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn interrupt(&self, _session_id: &VibexSessionId) -> VibexResult<()> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn resolve_permission(
            &self,
            _request: ResolvePermissionRequest,
        ) -> VibexResult<TimelineItem> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }

        async fn resolve_elicitation(
            &self,
            _request: ResolveElicitationRequest,
        ) -> VibexResult<TimelineItem> {
            Err(VibexError::process(
                "mock_agent",
                "not implemented in this test",
            ))
        }
    }

    async fn service() -> (ImChannelService, tempfile::TempDir) {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("vibex.db");
        vibex_config_switch::secrets::configure_provider_secret_store(
            vibex_config_switch::secrets::ProviderSecretStoreConfig::host_file(dir.path()),
        );
        let mut conn = vibex_db::open_database(&db_path).expect("open db");
        vibex_db::apply_migrations(&mut conn).expect("migrate");
        let service = ImChannelService::new(ImChannelServiceConfig {
            db_path,
            home_dir: dir.path().to_path_buf(),
            agent: Arc::new(MockAgent::new()),
            telegram_api_base: None,
            locale: None,
        })
        .expect("service");
        (service, dir)
    }

    fn telegram_draft() -> ImChannelDraft {
        ImChannelDraft {
            display_name: "Test bot".to_string(),
            config: ImChannelConfig::Telegram(ImTelegramConfig {
                chat_id: "-100123".to_string(),
                topic_mode: false,
                require_mention_in_groups: true,
            }),
            default_session: None,
            event_filter: Vec::new(),
            locale: None,
        }
    }

    #[tokio::test]
    async fn channel_can_be_created_configured_and_paired() {
        let (service, _dir) = service().await;
        let channel = service
            .create_channel(telegram_draft())
            .await
            .expect("create channel");
        assert!(!channel.enabled);
        assert!(!channel.configured);

        service
            .set_bot_token(&channel.channel_id, "123456:test-token")
            .await
            .expect("set token");
        let channels = service.list_channels().await.expect("list channels");
        assert_eq!(channels.len(), 1);
        assert!(channels[0].configured);
        assert!(!channels[0].enabled);

        let pairing = service
            .create_pairing_code(ImPairingRequest {
                channel_id: channel.channel_id.clone(),
                role: ImPrincipalRole::Approver,
                ttl_ms: Some(60_000),
            })
            .await
            .expect("pairing code");
        assert!(pairing.code.starts_with("VBX-"));
        assert_eq!(pairing.role, ImPrincipalRole::Approver);
    }

    #[test]
    fn pairing_code_shape_is_stable() {
        let code = generate_pairing_code();
        assert_eq!(code.len(), 15);
        assert!(code.starts_with("VBX-"));
        assert_eq!(code.chars().filter(|ch| *ch == '-').count(), 2);
    }

    #[test]
    fn always_falls_back_to_approve_when_provider_has_no_always_option() {
        let request = PermissionRequest {
            id: vibex_core::RequestId::new(),
            session_id: VibexSessionId::new(),
            project_id: None,
            workspace_id: None,
            provider_request_id: None,
            risk_category: PermissionRiskCategory::Command,
            title: "Run tests".to_string(),
            details: Vec::new(),
            allowed_responses: vec![
                PermissionResponseKind::Approve,
                PermissionResponseKind::Deny,
            ],
            response_options: vec![
                PermissionResponseOption {
                    option_id: "allow".to_string(),
                    label: "Allow".to_string(),
                    response: PermissionResponseKind::Approve,
                },
                PermissionResponseOption {
                    option_id: "deny".to_string(),
                    label: "Deny".to_string(),
                    response: PermissionResponseKind::Deny,
                },
            ],
            status: PermissionRequestStatus::Pending,
            requested_at_ms: 1,
            expires_at_ms: None,
        };
        let option = choose_option(&request, ApprovalChoice::Always).expect("fallback option");
        assert_eq!(option.option_id, "allow");
        assert_eq!(option.response, PermissionResponseKind::Approve);
    }

    #[test]
    fn default_event_filter_keeps_approvals_and_drops_prompt_echo() {
        let record = ImChannelRecord {
            channel_id: ImChannelId::new(),
            display_name: "Test".to_string(),
            kind: ImChannelKind::Telegram,
            enabled: true,
            config_json: serde_json::to_string(&ImChannelConfigEnvelope::new(
                ImChannelConfig::Telegram(ImTelegramConfig {
                    chat_id: "-100".to_string(),
                    topic_mode: false,
                    require_mention_in_groups: true,
                }),
            ))
            .expect("config"),
            event_filter_json: None,
            locale: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        assert!(event_allowed(&record, "permission_request"));
        assert!(event_allowed(&record, "turn_complete"));
        assert!(!event_allowed(&record, "user_prompt_sent"));
    }
}
