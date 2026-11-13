//! Lark / Feishu adapter.
//!
//! The adapter uses the long-connection event stream, so a desktop behind NAT
//! needs no public callback URL. Approval prompts go out as interactive cards
//! whose buttons carry the same one-shot action token the text fallback prints;
//! a press arrives as a `card.action.trigger` event and resolves through the
//! shared approval path.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use vibex_core::{
    ImChannelId, ImChannelKind, ImChannelState, ImChannelStatus, ImChannelTestReport,
    unix_timestamp_ms,
};

use crate::adapter::ImAdapter;
use crate::error::{ImError, ImResult};
use crate::types::{
    ImAttachment, ImButton, ImChatType, ImInboundEvent, ImSentMessage, ImTarget, ImThreadTarget,
};

const LARK_API_BASE: &str = "https://open.feishu.cn";

/// The largest file a Lark message may bring in.
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

pub struct LarkAdapter {
    channel_id: ImChannelId,
    app_id: String,
    app_secret: String,
    chat_id: Option<String>,
    client: Client,
    api_base: String,
    tenant_token: RwLock<Option<(String, i64)>>,
    status: Arc<Mutex<ImChannelStatus>>,
}

impl LarkAdapter {
    pub fn new(
        channel_id: ImChannelId,
        app_id: String,
        app_secret: String,
        chat_id: Option<String>,
        api_base: Option<String>,
        status: Arc<Mutex<ImChannelStatus>>,
    ) -> ImResult<Self> {
        if app_id.trim().is_empty() {
            return Err(ImError::ConfigurationInvalid(
                "Lark app id is required".to_string(),
            ));
        }
        if app_secret.trim().is_empty() {
            return Err(ImError::ConfigurationInvalid(
                "Lark app secret is required".to_string(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| ImError::Internal)?;
        Ok(Self {
            channel_id,
            app_id,
            app_secret,
            chat_id,
            client,
            api_base: api_base.unwrap_or_else(|| LARK_API_BASE.to_string()),
            tenant_token: RwLock::new(None),
            status,
        })
    }

    pub fn channel_id(&self) -> &ImChannelId {
        &self.channel_id
    }

    pub fn spawn(
        self: &Arc<Self>,
        inbound_tx: mpsc::Sender<ImInboundEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let adapter = Arc::clone(self);
        tokio::spawn(async move {
            let mut backoff_secs = 1_u64;
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let result = tokio::select! {
                    _ = shutdown.changed() => break,
                    result = adapter.run_connection(&inbound_tx) => result,
                };
                match result {
                    Ok(()) => {
                        backoff_secs = 1;
                        adapter.set_state(ImChannelState::Connected, None, 0);
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

    async fn run_connection(&self, inbound_tx: &mpsc::Sender<ImInboundEvent>) -> ImResult<()> {
        let token = self.tenant_token().await?;
        let url = self.websocket_url().await?;
        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|_| ImError::Transport)?;
        let (mut write, mut read) = stream.split();
        self.set_state(ImChannelState::Connected, None, 0);
        loop {
            let Some(message) = read.next().await else {
                break;
            };
            let message = message.map_err(|_| ImError::Transport)?;
            match message {
                Message::Binary(bytes) => {
                    if let Some(event) = decode_frame(&bytes).and_then(|frame| {
                        (frame.method == 1 && frame.payload_type == "event")
                            .then_some(frame.payload)
                            .and_then(|payload| String::from_utf8(payload).ok())
                            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    }) && let Some(inbound) = self.parse_event(&event).await
                    {
                        self.mark_inbound();
                        if inbound_tx.send(inbound).await.is_err() {
                            break;
                        }
                    }
                }
                Message::Text(text) => {
                    if let Ok(event) = serde_json::from_str::<Value>(&text)
                        && let Some(inbound) = self.parse_event(&event).await
                    {
                        self.mark_inbound();
                        if inbound_tx.send(inbound).await.is_err() {
                            break;
                        }
                    }
                }
                Message::Ping(payload) => {
                    let _ = write.send(Message::Pong(payload)).await;
                }
                Message::Close(_) => break,
                Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        let _ = write.close().await;
        let _ = token;
        Ok(())
    }

    async fn tenant_token(&self) -> ImResult<String> {
        if let Some((token, expires_at_ms)) = self.tenant_token.read().await.as_ref()
            && *expires_at_ms > unix_timestamp_ms()
        {
            return Ok(token.clone());
        }
        let response = self
            .client
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.api_base
            ))
            .json(&json!({ "app_id": self.app_id, "app_secret": self.app_secret }))
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if value.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(provider_error(&value));
        }
        let token = value
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .ok_or(ImError::AuthFailed)?
            .to_string();
        let expire = value.get("expire").and_then(Value::as_i64).unwrap_or(7200);
        *self.tenant_token.write().await = Some((
            token.clone(),
            unix_timestamp_ms().saturating_add(expire.saturating_sub(300) * 1000),
        ));
        Ok(token)
    }

    async fn websocket_url(&self) -> ImResult<String> {
        let response = self
            .client
            .post(format!("{}/callback/ws/endpoint", self.api_base))
            .json(&json!({ "AppID": self.app_id, "AppSecret": self.app_secret }))
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if value.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(provider_error(&value));
        }
        value
            .pointer("/data/URL")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ImError::Transport)
    }

    async fn send_text(
        &self,
        target: &ImTarget,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage> {
        let token = self.tenant_token().await?;
        let body = message_body(text, buttons);
        let response = self
            .client
            .post(format!(
                "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
                self.api_base
            ))
            .bearer_auth(token)
            .json(&json!({
                "receive_id": target.chat_id,
                "msg_type": body.msg_type,
                "content": body.content,
            }))
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if value.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(provider_error(&value));
        }
        self.mark_outbound();
        Ok(ImSentMessage {
            message_id: value
                .pointer("/data/message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    async fn edit_text(
        &self,
        _target: &ImTarget,
        message_id: &str,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<()> {
        if message_id.is_empty() {
            return Ok(());
        }
        let token = self.tenant_token().await?;
        let body = message_body(text, buttons);
        let response = self
            .client
            .put(format!(
                "{}/open-apis/im/v1/messages/{message_id}",
                self.api_base
            ))
            .bearer_auth(token)
            .json(&json!({
                "msg_type": body.msg_type,
                "content": body.content,
            }))
            .send()
            .await
            .map_err(|_| ImError::Transport)?;
        let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
        if value.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(provider_error(&value));
        }
        Ok(())
    }

    async fn probe(&self) -> ImResult<ImChannelTestReport> {
        let token = self.tenant_token().await?;
        let mut chat_title = None;
        if let Some(chat_id) = self.chat_id.as_deref() {
            let response = self
                .client
                .get(format!("{}/open-apis/im/v1/chats/{chat_id}", self.api_base))
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| ImError::Transport)?;
            let value: Value = response.json().await.map_err(|_| ImError::Transport)?;
            if value.get("code").and_then(Value::as_i64) != Some(0) {
                return Err(provider_error(&value));
            }
            chat_title = value
                .pointer("/data/name")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        Ok(ImChannelTestReport {
            ok: true,
            bot_name: Some(self.app_id.clone()),
            chat_title,
            warnings: Vec::new(),
        })
    }

    async fn parse_event(&self, event: &Value) -> Option<ImInboundEvent> {
        if let Some(action) = self.parse_card_action(event) {
            return Some(action);
        }
        let event_type = event.pointer("/header/event_type")?.as_str()?;
        if event_type != "im.message.receive_v1" {
            return None;
        }
        let sender = event.pointer("/event/sender")?;
        if sender.get("sender_type").and_then(Value::as_str) == Some("app") {
            return None;
        }
        let message = event.pointer("/event/message")?;
        let message_type = message.get("message_type").and_then(Value::as_str)?;
        if !matches!(message_type, "text" | "image" | "file") {
            return None;
        }
        let content = message.get("content").and_then(Value::as_str)?;
        let content: Value = serde_json::from_str(content).ok()?;
        let mut text = content
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(mentions) = message.get("mentions").and_then(Value::as_array) {
            for mention in mentions {
                if let Some(key) = mention.get("key").and_then(Value::as_str) {
                    text = text.replace(key, "");
                }
            }
        }
        let attachments = self.download_lark_attachments(message).await;
        if text.trim().is_empty() && attachments.is_empty() {
            return None;
        }
        let text = text.trim().to_string();
        let chat_id = message.get("chat_id")?.as_str()?.to_string();
        let chat_type = match message.get("chat_type").and_then(Value::as_str) {
            Some("p2p") => ImChatType::Direct,
            _ => ImChatType::Group,
        };
        let thread_key = message
            .get("thread_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(ImInboundEvent {
            channel_id: self.channel_id.clone(),
            provider_update_id: event.pointer("/header/event_id")?.as_str()?.to_string(),
            external_chat_id: chat_id,
            chat_type,
            thread_kind: (!thread_key.is_empty()).then(|| "lark_thread".to_string()),
            thread_key,
            external_user_id: sender
                .pointer("/sender_id/open_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sender_display_name: sender
                .pointer("/sender_id/open_id")
                .and_then(Value::as_str)
                .unwrap_or("Lark user")
                .to_string(),
            external_message_id: message
                .get("message_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            text,
            attachments,
            callback: None,
            received_at_ms: unix_timestamp_ms(),
        })
    }

    /// A card button press arrives as its own event with the button's `value`.
    fn parse_card_action(&self, event: &Value) -> Option<ImInboundEvent> {
        let event_type = event.pointer("/header/event_type")?.as_str()?;
        if event_type != "card.action.trigger" {
            return None;
        }
        let action = event.pointer("/event/action")?;
        let value = action.get("value")?;
        let action_id = value
            .get("vibex_action")
            .and_then(Value::as_str)
            .and_then(|value| vibex_core::ImActionId::parse(value).ok())?;
        let operator = event.pointer("/event/operator")?;
        let chat_id = event
            .pointer("/event/context/open_chat_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(ImInboundEvent {
            channel_id: self.channel_id.clone(),
            provider_update_id: event.pointer("/header/event_id")?.as_str()?.to_string(),
            external_chat_id: chat_id,
            chat_type: ImChatType::Direct,
            thread_key: String::new(),
            thread_kind: None,
            external_user_id: operator
                .pointer("/operator_id/open_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sender_display_name: operator
                .pointer("/operator_id/open_id")
                .and_then(Value::as_str)
                .unwrap_or("Lark user")
                .to_string(),
            external_message_id: None,
            text: String::new(),
            attachments: Vec::new(),
            callback: Some(crate::types::ImCallback {
                callback_query_id: None,
                action_id: Some(action_id),
            }),
            received_at_ms: unix_timestamp_ms(),
        })
    }

    /// Downloads the image or file a Lark message carried.
    ///
    /// The resource endpoint needs the message id and the file key, both of
    /// which the message content names. A failure is reported as no attachment
    /// rather than dropping the whole message: the text may still be useful.
    async fn download_lark_attachments(&self, message: &Value) -> Vec<ImAttachment> {
        let Some(message_id) = message.get("message_id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(content) = message
            .get("content")
            .and_then(Value::as_str)
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
        else {
            return Vec::new();
        };
        let Some(file_key) = content.get("file_key").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Ok(token) = self.tenant_token().await else {
            return Vec::new();
        };
        let response = self
            .client
            .get(format!(
                "{}/open-apis/im/v1/messages/{message_id}/resources/{file_key}?type=file",
                self.api_base
            ))
            .bearer_auth(token)
            .send()
            .await;
        let Ok(response) = response else {
            return Vec::new();
        };
        let Ok(bytes) = response.bytes().await else {
            return Vec::new();
        };
        if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
            return Vec::new();
        }
        vec![ImAttachment {
            file_name: content
                .get("file_name")
                .and_then(Value::as_str)
                .unwrap_or("lark-file")
                .to_string(),
            mime_type: content
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            bytes: bytes.to_vec(),
        }]
    }

    fn set_state(&self, state: ImChannelState, error_code: Option<String>, retry_after_ms: u64) {
        if let Ok(mut status) = self.status.lock() {
            status.state = state;
            status.since_ms = unix_timestamp_ms();
            status.generation = status.generation.saturating_add(1);
            status.last_error = error_code.map(|code| vibex_core::ImChannelErrorView {
                code,
                message: "Lark connection reported an error".to_string(),
                retry_at_ms: (retry_after_ms > 0)
                    .then(|| unix_timestamp_ms().saturating_add(retry_after_ms as i64)),
            });
        }
    }

    fn mark_inbound(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_inbound_at_ms = Some(unix_timestamp_ms());
        }
    }

    fn mark_outbound(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_outbound_at_ms = Some(unix_timestamp_ms());
            status.state = ImChannelState::Connected;
        }
    }
}

#[async_trait::async_trait]
impl ImAdapter for LarkAdapter {
    fn kind(&self) -> ImChannelKind {
        ImChannelKind::Lark
    }

    async fn send_text(
        &self,
        target: &ImTarget,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage> {
        LarkAdapter::send_text(self, target, text, buttons).await
    }

    async fn edit_text(
        &self,
        target: &ImTarget,
        message_id: &str,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<()> {
        LarkAdapter::edit_text(self, target, message_id, text, buttons).await
    }

    async fn answer_callback(&self, _callback_query_id: &str, _text: Option<&str>) -> ImResult<()> {
        Ok(())
    }

    async fn probe(&self) -> ImResult<ImChannelTestReport> {
        LarkAdapter::probe(self).await
    }

    async fn is_group_trigger(&self, text: &str) -> bool {
        text.trim_start().starts_with('/')
    }

    fn mark_inbound(&self) {
        LarkAdapter::mark_inbound(self);
    }

    async fn create_thread(&self, _title: &str) -> ImResult<ImThreadTarget> {
        Err(ImError::Unsupported)
    }

    async fn edit_thread_title(&self, _thread_key: &str, _title: &str) -> ImResult<()> {
        Err(ImError::Unsupported)
    }
}

/// One outbound message body: an interactive card when there are actions to
/// take, plain text otherwise.
struct MessageBody {
    msg_type: &'static str,
    content: String,
}

fn message_body(text: &str, buttons: &[ImButton]) -> MessageBody {
    if buttons.is_empty() {
        return MessageBody {
            msg_type: "text",
            content: json!({ "text": text }).to_string(),
        };
    }
    // The action value is the same one-shot token the text fallback prints, so
    // a card press and a typed code resolve through one path.
    let actions: Vec<Value> = buttons
        .iter()
        .map(|button| {
            json!({
                "tag": "button",
                "text": { "tag": "plain_text", "content": button.label },
                "type": "default",
                "value": {
                    "vibex_action": button.action_id.as_str(),
                },
            })
        })
        .collect();
    let card = json!({
        "config": { "wide_screen_mode": true },
        "elements": [
            { "tag": "div", "text": { "tag": "plain_text", "content": text } },
            { "tag": "action", "actions": actions },
        ],
    });
    MessageBody {
        msg_type: "interactive",
        content: card.to_string(),
    }
}

fn provider_error(value: &Value) -> ImError {
    ImError::ProviderRejected {
        code: value
            .get("code")
            .and_then(Value::as_i64)
            .map(|code| code.to_string())
            .unwrap_or_else(|| "lark_error".to_string()),
        message: value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("Lark rejected the request")
            .to_string(),
    }
}

struct Frame {
    method: i32,
    payload_type: String,
    payload: Vec<u8>,
}

/// Minimal protobuf decoder for the long-connection frame envelope.
///
/// Only the fields the adapter reads are decoded; unknown fields are skipped
/// using their wire type so a newer server can add fields without breaking the
/// stream.
fn decode_frame(bytes: &[u8]) -> Option<Frame> {
    let mut cursor = 0;
    let mut method = 0;
    let mut payload_type = String::new();
    let mut payload = Vec::new();
    while cursor < bytes.len() {
        let key = read_varint(bytes, &mut cursor)?;
        let field = key >> 3;
        let wire = key & 7;
        match (field, wire) {
            (4, 0) => method = read_varint(bytes, &mut cursor)? as i32,
            (7, 2) => payload_type = read_string(bytes, &mut cursor)?,
            (8, 2) => payload = read_bytes(bytes, &mut cursor)?.to_vec(),
            _ => skip_field(bytes, &mut cursor, wire)?,
        }
    }
    Some(Frame {
        method,
        payload_type,
        payload,
    })
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let mut value = 0_u64;
    let mut shift = 0;
    while *cursor < bytes.len() {
        let byte = bytes[*cursor];
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn read_bytes<'a>(bytes: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    let length = read_varint(bytes, cursor)? as usize;
    let end = cursor.checked_add(length)?;
    let slice = bytes.get(*cursor..end)?;
    *cursor = end;
    Some(slice)
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    String::from_utf8(read_bytes(bytes, cursor)?.to_vec()).ok()
}

fn skip_field(bytes: &[u8], cursor: &mut usize, wire: u64) -> Option<()> {
    match wire {
        0 => {
            let _ = read_varint(bytes, cursor)?;
        }
        1 => *cursor = cursor.checked_add(8)?,
        2 => {
            let _ = read_bytes(bytes, cursor)?;
        }
        5 => *cursor = cursor.checked_add(4)?,
        _ => return None,
    }
    (*cursor <= bytes.len()).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn field(field: u64, wire: u64, payload: &[u8], out: &mut Vec<u8>) {
        varint((field << 3) | wire, out);
        if wire == 2 {
            varint(payload.len() as u64, out);
        }
        out.extend_from_slice(payload);
    }

    #[tokio::test]
    async fn parses_a_text_message_and_strips_mentions() {
        let adapter = LarkAdapter::new(
            ImChannelId::new(),
            "cli_test".to_string(),
            "secret".to_string(),
            Some("oc_test".to_string()),
            Some("http://127.0.0.1:1".to_string()),
            Arc::new(Mutex::new(ImChannelStatus::default())),
        )
        .expect("adapter");
        let event = json!({
            "header": {
                "event_type": "im.message.receive_v1",
                "event_id": "evt-1"
            },
            "event": {
                "sender": {
                    "sender_type": "user",
                    "sender_id": { "open_id": "ou_user" }
                },
                "message": {
                    "message_id": "om_1",
                    "chat_id": "oc_chat",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"@_user_1 /status\"}",
                    "mentions": [{ "key": "@_user_1" }],
                    "thread_id": "omt_1"
                }
            }
        });
        let inbound = adapter.parse_event(&event).await.expect("event");
        assert_eq!(inbound.text, "/status");
        assert_eq!(inbound.external_chat_id, "oc_chat");
        assert_eq!(inbound.external_user_id, "ou_user");
        assert_eq!(inbound.thread_key, "omt_1");
        assert_eq!(inbound.thread_kind.as_deref(), Some("lark_thread"));
    }

    #[tokio::test]
    async fn group_trigger_is_command_only() {
        let adapter = LarkAdapter::new(
            ImChannelId::new(),
            "cli_test".to_string(),
            "secret".to_string(),
            None,
            Some("http://127.0.0.1:1".to_string()),
            Arc::new(Mutex::new(ImChannelStatus::default())),
        )
        .expect("adapter");
        assert!(adapter.is_group_trigger("/status").await);
        assert!(!adapter.is_group_trigger("hello").await);
    }

    #[test]
    fn decodes_data_frame_envelope() {
        let mut bytes = Vec::new();
        field(4, 0, &[1], &mut bytes);
        field(7, 2, b"event", &mut bytes);
        field(8, 2, br#"{"header":{"event_type":"x"}}"#, &mut bytes);
        let frame = decode_frame(&bytes).expect("frame");
        assert_eq!(frame.method, 1);
        assert_eq!(frame.payload_type, "event");
        assert_eq!(frame.payload, br#"{"header":{"event_type":"x"}}"#.to_vec());
    }
}
