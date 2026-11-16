use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use vibex_core::{
    ImActionId, ImChannelId, ImChannelState, ImChannelStatus, ImChannelTestReport,
    unix_timestamp_ms,
};

use crate::adapter::ImAdapter;
use crate::error::{ImError, ImResult};
use crate::types::{
    ImAttachment, ImButton, ImCallback, ImChatType, ImInboundEvent, ImSentMessage, ImTarget,
    ImThreadTarget,
};

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// The largest file a chat message may bring in.
///
/// The bot API itself refuses files over 20 MB, and a prompt carrying a large
/// binary is rarely what the user meant. The channel caps well below that so a
/// paste of a disk image is rejected with a reason instead of timing out.
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Whether a document is worth handing to the Agent.
fn is_accepted_document(mime_type: &str) -> bool {
    let mime = mime_type.to_ascii_lowercase();
    mime.starts_with("image/")
        || mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json" | "application/pdf" | "application/x-ndjson"
        )
}

pub struct TelegramAdapter {
    channel_id: ImChannelId,
    token: String,
    chat_id: String,
    require_mention_in_groups: bool,
    client: Client,
    api_base: String,
    bot_username: tokio::sync::RwLock<Option<String>>,
    status: Arc<Mutex<ImChannelStatus>>,
}

impl TelegramAdapter {
    pub fn new(
        channel_id: ImChannelId,
        token: String,
        chat_id: String,
        require_mention_in_groups: bool,
        api_base: Option<String>,
        status: Arc<Mutex<ImChannelStatus>>,
    ) -> ImResult<Self> {
        if token.trim().is_empty() {
            return Err(ImError::ConfigurationInvalid(
                "Telegram bot token is required".to_string(),
            ));
        }
        if chat_id.trim().is_empty() {
            return Err(ImError::ConfigurationInvalid(
                "Telegram chat id is required".to_string(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| ImError::Internal)?;
        Ok(Self {
            channel_id,
            token,
            chat_id,
            require_mention_in_groups,
            client,
            api_base: api_base.unwrap_or_else(|| TELEGRAM_API_BASE.to_string()),
            bot_username: tokio::sync::RwLock::new(None),
            status,
        })
    }

    pub fn channel_id(&self) -> &ImChannelId {
        &self.channel_id
    }

    pub fn status(&self) -> Arc<Mutex<ImChannelStatus>> {
        Arc::clone(&self.status)
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }

    /// Starts the long-poll loop. The returned task stops when `shutdown`
    /// flips to true.
    pub fn spawn(
        self: &Arc<Self>,
        inbound_tx: mpsc::Sender<ImInboundEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let adapter = Arc::clone(self);
        tokio::spawn(async move {
            let _ = adapter.refresh_bot_username().await;
            let mut offset: Option<i64> = None;
            let mut backoff_secs = 1_u64;
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let result = tokio::select! {
                    _ = shutdown.changed() => break,
                    result = adapter.get_updates(offset) => result,
                };
                match result {
                    Ok(updates) => {
                        adapter.set_state(ImChannelState::Connected, None, 0);
                        backoff_secs = 1;
                        for update in updates {
                            if let Some(update_id) = update.get("update_id").and_then(Value::as_i64)
                            {
                                offset = Some(update_id.saturating_add(1));
                            }
                            if let Some(event) = adapter.parse_update(&update).await
                                && inbound_tx.send(event).await.is_err()
                            {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let retry_after_ms = backoff_secs.saturating_mul(1000);
                        adapter.set_state(
                            ImChannelState::Degraded,
                            Some(error.code().to_string()),
                            retry_after_ms,
                        );
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(60);
                    }
                }
            }
            adapter.set_state(ImChannelState::Stopped, None, 0);
        })
    }

    async fn get_updates(&self, offset: Option<i64>) -> ImResult<Vec<Value>> {
        let mut body = json!({
            "timeout": 25,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(offset) = offset {
            body["offset"] = json!(offset);
        }
        let response = self
            .client
            .post(self.url("getUpdates"))
            .json(&body)
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ImError::AuthFailed);
        }
        let too_many_requests = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if too_many_requests || value.get("error_code").and_then(Value::as_i64) == Some(429) {
            return Err(ImError::RateLimited {
                retry_after_ms: retry_after_ms(&value),
            });
        }
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(provider_error(&value));
        }
        Ok(value
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn refresh_bot_username(&self) -> ImResult<()> {
        let value = self.call("getMe", json!({})).await?;
        let username = value
            .get("username")
            .and_then(Value::as_str)
            .map(str::to_string);
        *self.bot_username.write().await = username;
        Ok(())
    }

    async fn call(&self, method: &str, body: Value) -> ImResult<Value> {
        let response = self
            .client
            .post(self.url(method))
            .json(&body)
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ImError::AuthFailed);
        }
        let too_many_requests = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if too_many_requests || value.get("error_code").and_then(Value::as_i64) == Some(429) {
            return Err(ImError::RateLimited {
                retry_after_ms: retry_after_ms(&value),
            });
        }
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(provider_error(&value));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn parse_update(&self, update: &Value) -> Option<ImInboundEvent> {
        let received_at_ms = unix_timestamp_ms();
        if let Some(message) = update.get("message") {
            let from = message.get("from")?;
            if from.get("is_bot").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let chat = message.get("chat")?;
            let external_chat_id = chat.get("id")?.as_i64()?.to_string();
            let text = message
                .get("text")
                .or_else(|| message.get("caption"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // A photo or a document carries no text; without this the message
            // would parse as an empty prompt and be dropped before the service
            // ever saw the file.
            let attachments = self.download_attachments(message).await.unwrap_or_default();
            let chat_type = match chat.get("type").and_then(Value::as_str) {
                Some("private") => ImChatType::Direct,
                _ => ImChatType::Group,
            };
            let thread_key = message
                .get("message_thread_id")
                .and_then(Value::as_i64)
                .map(|id| id.to_string())
                .unwrap_or_default();
            Some(ImInboundEvent {
                channel_id: self.channel_id.clone(),
                provider_update_id: update.get("update_id")?.as_i64()?.to_string(),
                external_chat_id,
                chat_type,
                thread_kind: (!thread_key.is_empty()).then(|| "telegram_forum_topic".to_string()),
                thread_key,
                external_user_id: from.get("id")?.as_i64()?.to_string(),
                sender_display_name: sender_name(from),
                external_message_id: message
                    .get("message_id")
                    .and_then(Value::as_i64)
                    .map(|id| id.to_string()),
                text,
                attachments,
                callback: None,
                received_at_ms,
            })
        } else if let Some(callback) = update.get("callback_query") {
            let from = callback.get("from")?;
            let message = callback.get("message")?;
            let chat = message.get("chat")?;
            let external_chat_id = chat.get("id")?.as_i64()?.to_string();
            let thread_key = message
                .get("message_thread_id")
                .and_then(Value::as_i64)
                .map(|id| id.to_string())
                .unwrap_or_default();
            let action_id = callback
                .get("data")
                .and_then(Value::as_str)
                .and_then(|value| ImActionId::parse(value).ok());
            Some(ImInboundEvent {
                channel_id: self.channel_id.clone(),
                provider_update_id: update.get("update_id")?.as_i64()?.to_string(),
                external_chat_id,
                chat_type: ImChatType::Group,
                thread_kind: (!thread_key.is_empty()).then(|| "telegram_forum_topic".to_string()),
                thread_key,
                external_user_id: from.get("id")?.as_i64()?.to_string(),
                sender_display_name: sender_name(from),
                external_message_id: message
                    .get("message_id")
                    .and_then(Value::as_i64)
                    .map(|id| id.to_string()),
                text: String::new(),
                attachments: Vec::new(),
                callback: Some(ImCallback {
                    callback_query_id: callback
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    action_id,
                }),
                received_at_ms,
            })
        } else {
            None
        }
    }

    pub async fn send_text(
        &self,
        target: &ImTarget,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage> {
        let mut body = json!({
            "chat_id": target.chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if !target.thread_key.is_empty() {
            body["message_thread_id"] = json!(target.thread_key);
        }
        if !buttons.is_empty() {
            body["reply_markup"] = json!({
                "inline_keyboard": [buttons
                    .iter()
                    .map(|button| json!({
                        "text": button.label,
                        "callback_data": button.action_id.as_str(),
                    }))
                    .collect::<Vec<_>>()]
            });
        }
        // Retry policy belongs to the outbox: it knows the message priority and
        // whether the caller is waiting, and a second sleep here would stack on
        // top of that. Report the provider's own cool-down and let it decide.
        let result = self.call("sendMessage", body).await?;
        self.mark_outbound();
        let message_id = result
            .get("message_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .ok_or(ImError::ProviderRejected {
                code: "telegram_missing_message_id".to_string(),
                message: "Telegram did not return a message id".to_string(),
            })?;
        Ok(ImSentMessage { message_id })
    }

    pub async fn edit_text(
        &self,
        target: &ImTarget,
        message_id: &str,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<()> {
        let mut body = json!({
            "chat_id": target.chat_id,
            "message_id": message_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if !buttons.is_empty() {
            body["reply_markup"] = json!({
                "inline_keyboard": [buttons
                    .iter()
                    .map(|button| json!({
                        "text": button.label,
                        "callback_data": button.action_id.as_str(),
                    }))
                    .collect::<Vec<_>>()]
            });
        }
        self.call("editMessageText", body).await?;
        self.mark_outbound();
        Ok(())
    }

    pub async fn answer_callback(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> ImResult<()> {
        let mut body = json!({ "callback_query_id": callback_query_id });
        if let Some(text) = text {
            body["text"] = json!(text);
        }
        let _ = self.call("answerCallbackQuery", body).await;
        Ok(())
    }

    pub async fn create_forum_topic(&self, title: &str) -> ImResult<String> {
        let value = self
            .call(
                "createForumTopic",
                json!({ "chat_id": self.chat_id, "name": truncate_title(title) }),
            )
            .await?;
        value
            .get("message_thread_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .ok_or(ImError::ProviderRejected {
                code: "telegram_missing_thread_id".to_string(),
                message: "Telegram did not return a forum topic id".to_string(),
            })
    }

    pub async fn edit_forum_topic(&self, thread_id: &str, title: &str) -> ImResult<()> {
        self.call(
            "editForumTopic",
            json!({
                "chat_id": self.chat_id,
                "message_thread_id": thread_id,
                "name": truncate_title(title),
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn probe(&self) -> ImResult<ImChannelTestReport> {
        let me = self.call("getMe", json!({})).await?;
        let chat = self
            .call("getChat", json!({ "chat_id": self.chat_id }))
            .await?;
        let mut warnings = Vec::new();
        if chat.get("type").and_then(Value::as_str) == Some("channel") {
            warnings.push(
                "This chat is a channel; bots cannot read member messages there.".to_string(),
            );
        }
        Ok(ImChannelTestReport {
            ok: true,
            bot_name: me
                .get("username")
                .and_then(Value::as_str)
                .map(|name| format!("@{name}")),
            chat_title: chat
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| chat.get("username").and_then(Value::as_str))
                .map(str::to_string),
            warnings,
        })
    }

    /// Downloads a photo or document Telegram is hosting.
    ///
    /// Only two shapes are accepted, and both are capped: a photo is taken at
    /// the largest size the message offers, a document only when it declares
    /// an image MIME type or a text-like one. Anything else is named back to the
    /// user by the service rather than silently dropped.
    pub async fn download_attachments(&self, message: &Value) -> ImResult<Vec<ImAttachment>> {
        let mut files = Vec::new();
        if let Some(photos) = message.get("photo").and_then(Value::as_array)
            && let Some(photo) = photos
                .iter()
                .max_by_key(|photo| photo.get("file_size").and_then(Value::as_i64).unwrap_or(0))
            && let Some(file_id) = photo.get("file_id").and_then(Value::as_str)
            && let Some(bytes) = self.download_file(file_id).await?
        {
            files.push(ImAttachment {
                file_name: format!("photo-{}.jpg", files.len() + 1),
                mime_type: "image/jpeg".to_string(),
                bytes,
            });
        }
        if let Some(document) = message.get("document")
            && let Some(mime_type) = document.get("mime_type").and_then(Value::as_str)
            && is_accepted_document(mime_type)
            && let Some(file_id) = document.get("file_id").and_then(Value::as_str)
            && let Some(bytes) = self.download_file(file_id).await?
        {
            files.push(ImAttachment {
                file_name: document
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or("document")
                    .to_string(),
                mime_type: mime_type.to_string(),
                bytes,
            });
        }
        Ok(files)
    }

    async fn download_file(&self, file_id: &str) -> ImResult<Option<Vec<u8>>> {
        let file = self.call("getFile", json!({ "file_id": file_id })).await?;
        let Some(path) = file.get("file_path").and_then(Value::as_str) else {
            return Ok(None);
        };
        let declared = file.get("file_size").and_then(Value::as_i64).unwrap_or(0);
        if declared > MAX_ATTACHMENT_BYTES as i64 {
            return Err(ImError::ProviderRejected {
                code: "telegram_attachment_too_large".to_string(),
                message: "the file is larger than this channel accepts".to_string(),
            });
        }
        let response = self
            .client
            .get(format!(
                "{}/file/bot{}/{path}",
                self.api_base.trim_end_matches('/'),
                self.token
            ))
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let bytes = response.bytes().await.map_err(|_| ImError::Transport)?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(ImError::ProviderRejected {
                code: "telegram_attachment_too_large".to_string(),
                message: "the file is larger than this channel accepts".to_string(),
            });
        }
        Ok(Some(bytes.to_vec()))
    }

    pub async fn is_group_trigger(&self, text: &str) -> bool {
        let text = text.trim_start();
        if text.starts_with('/') {
            return true;
        }
        // A channel that opted out of the mention requirement accepts any text
        // in a group. The default stays conservative: a group bot that reacts to
        // every message is a surprise nobody asked for.
        if !self.require_mention_in_groups {
            return !text.is_empty();
        }
        let username = self.bot_username.read().await.clone();
        username
            .as_deref()
            .is_some_and(|username| text.starts_with(&format!("@{username}")))
    }

    fn set_state(&self, state: ImChannelState, error_code: Option<String>, retry_after_ms: u64) {
        if let Ok(mut status) = self.status.lock() {
            status.state = state;
            status.since_ms = unix_timestamp_ms();
            status.generation = status.generation.saturating_add(1);
            status.last_error = error_code.map(|code| vibex_core::ImChannelErrorView {
                code,
                message: "Telegram connection reported an error".to_string(),
                retry_at_ms: (retry_after_ms > 0)
                    .then(|| unix_timestamp_ms().saturating_add(retry_after_ms as i64)),
            });
        }
    }

    fn mark_outbound(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_outbound_at_ms = Some(unix_timestamp_ms());
            status.state = ImChannelState::Connected;
        }
    }

    pub fn mark_inbound(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_inbound_at_ms = Some(unix_timestamp_ms());
        }
    }
}

fn sender_name(from: &Value) -> String {
    let mut name = from
        .get("first_name")
        .and_then(Value::as_str)
        .unwrap_or("Telegram user")
        .to_string();
    if let Some(last) = from.get("last_name").and_then(Value::as_str) {
        name.push(' ');
        name.push_str(last);
    }
    if let Some(username) = from.get("username").and_then(Value::as_str) {
        name.push_str(&format!(" (@{username})"));
    }
    name
}

/// Telegram reports its own cool-down in `parameters.retry_after` (seconds).
/// Guessing a delay instead of reading it either hammers the API or stalls a
/// reply that could have gone out sooner.
fn retry_after_ms(value: &Value) -> u64 {
    value
        .pointer("/parameters/retry_after")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0).min(60_000.0) as u64)
        .unwrap_or(1_000)
}

fn provider_error(value: &Value) -> ImError {
    let code = value
        .get("error_code")
        .and_then(Value::as_i64)
        .map(|code| code.to_string())
        .unwrap_or_else(|| "telegram_error".to_string());
    let message = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Telegram rejected the request")
        .to_string();
    ImError::ProviderRejected { code, message }
}

#[async_trait::async_trait]
impl ImAdapter for TelegramAdapter {
    fn kind(&self) -> vibex_core::ImChannelKind {
        vibex_core::ImChannelKind::Telegram
    }

    async fn send_text(
        &self,
        target: &ImTarget,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage> {
        TelegramAdapter::send_text(self, target, text, buttons).await
    }

    async fn edit_text(
        &self,
        target: &ImTarget,
        message_id: &str,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<()> {
        TelegramAdapter::edit_text(self, target, message_id, text, buttons).await
    }

    async fn answer_callback(&self, callback_query_id: &str, text: Option<&str>) -> ImResult<()> {
        TelegramAdapter::answer_callback(self, callback_query_id, text).await
    }

    async fn probe(&self) -> ImResult<ImChannelTestReport> {
        TelegramAdapter::probe(self).await
    }

    async fn is_group_trigger(&self, text: &str) -> bool {
        TelegramAdapter::is_group_trigger(self, text).await
    }

    fn mark_inbound(&self) {
        TelegramAdapter::mark_inbound(self);
    }

    async fn create_thread(&self, title: &str) -> ImResult<ImThreadTarget> {
        Ok(ImThreadTarget {
            thread_key: self.create_forum_topic(title).await?,
            thread_kind: "telegram_forum_topic".to_string(),
        })
    }

    async fn edit_thread_title(&self, thread_key: &str, title: &str) -> ImResult<()> {
        self.edit_forum_topic(thread_key, title).await
    }
}

fn truncate_title(title: &str) -> String {
    title.chars().take(128).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn adapter() -> TelegramAdapter {
        TelegramAdapter::new(
            ImChannelId::new(),
            "123:test".to_string(),
            "-100123".to_string(),
            true,
            Some("http://127.0.0.1:1".to_string()),
            Arc::new(Mutex::new(ImChannelStatus::default())),
        )
        .expect("adapter")
    }

    #[tokio::test]
    async fn message_update_normalizes_sender_and_thread() {
        let adapter = adapter();
        let update = json!({
            "update_id": 42,
            "message": {
                "message_id": 7,
                "date": 1,
                "text": "/status",
                "message_thread_id": 99,
                "chat": { "id": -100123, "type": "supergroup", "title": "Team" },
                "from": { "id": 123, "is_bot": false, "first_name": "Ada", "last_name": "L", "username": "ada" }
            }
        });
        let event = adapter.parse_update(&update).await.expect("event");
        assert_eq!(event.provider_update_id, "42");
        assert_eq!(event.external_chat_id, "-100123");
        assert_eq!(event.external_user_id, "123");
        assert_eq!(event.thread_key, "99");
        assert_eq!(event.thread_kind.as_deref(), Some("telegram_forum_topic"));
        assert_eq!(event.sender_display_name, "Ada L (@ada)");
        assert_eq!(event.text, "/status");
        assert_eq!(event.chat_type, ImChatType::Group);
    }

    #[tokio::test]
    async fn callback_update_carries_action_id_and_ignores_bot_senders() {
        let adapter = adapter();
        let action_id = ImActionId::new();
        let update = json!({
            "update_id": 43,
            "callback_query": {
                "id": "cb-1",
                "from": { "id": 123, "is_bot": false, "first_name": "Ada" },
                "data": action_id.as_str(),
                "message": {
                    "message_id": 8,
                    "chat": { "id": -100123, "type": "supergroup" },
                    "message_thread_id": 99
                }
            }
        });
        let event = adapter.parse_update(&update).await.expect("event");
        let callback = event.callback.expect("callback");
        assert_eq!(callback.callback_query_id.as_deref(), Some("cb-1"));
        assert_eq!(callback.action_id, Some(action_id));

        let bot_message = json!({
            "update_id": 44,
            "message": {
                "message_id": 9,
                "chat": { "id": -100123, "type": "supergroup" },
                "from": { "id": 999, "is_bot": true, "first_name": "Bot" }
            }
        });
        assert!(adapter.parse_update(&bot_message).await.is_none());
    }
}
