use vibex_core::ImActionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImChatType {
    Direct,
    Group,
    Thread,
}

/// One file a user sent with a message.
///
/// The bytes are already downloaded: the channel decides what it accepts, and
/// the Agent is never handed a provider URL it would have to fetch itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImInboundEvent {
    pub channel_id: vibex_core::ImChannelId,
    pub provider_update_id: String,
    pub external_chat_id: String,
    pub chat_type: ImChatType,
    pub thread_key: String,
    pub thread_kind: Option<String>,
    pub external_user_id: String,
    pub sender_display_name: String,
    pub external_message_id: Option<String>,
    pub text: String,
    /// Files that came with the message, already downloaded and validated.
    pub attachments: Vec<ImAttachment>,
    pub callback: Option<ImCallback>,
    pub received_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImCallback {
    pub callback_query_id: Option<String>,
    pub action_id: Option<ImActionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImSentMessage {
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImButton {
    pub action_id: ImActionId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImTarget {
    pub chat_id: String,
    pub thread_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImThreadTarget {
    pub thread_key: String,
    pub thread_kind: String,
}
