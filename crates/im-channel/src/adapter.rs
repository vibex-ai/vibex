use async_trait::async_trait;
use vibex_core::{ImChannelKind, ImChannelTestReport};

use crate::error::ImResult;
use crate::types::{ImActivity, ImButton, ImCommandSpec, ImSentMessage, ImTarget, ImThreadTarget};

/// The platform-neutral operations the channel service needs from a transport.
///
/// The trait deliberately keeps the protocol surface small: text, buttons,
/// callback acknowledgements, a read-only probe, and optional thread support.
/// Platform-specific start-up stays in the concrete adapter so a transport can
/// own its long-poll loop without leaking it into the service.
#[async_trait]
pub trait ImAdapter: Send + Sync + 'static {
    fn kind(&self) -> ImChannelKind;

    async fn send_text(
        &self,
        target: &ImTarget,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage>;

    async fn edit_text(
        &self,
        target: &ImTarget,
        message_id: &str,
        text: &str,
        buttons: &[ImButton],
    ) -> ImResult<()>;

    async fn answer_callback(&self, callback_query_id: &str, text: Option<&str>) -> ImResult<()>;

    /// Sends Markdown, letting the transport render it.
    ///
    /// The default treats it as plain text: a transport without rich text
    /// support says so by sending the Markdown verbatim rather than by dropping
    /// the answer.
    async fn send_markdown(
        &self,
        target: &ImTarget,
        markdown: &str,
        buttons: &[ImButton],
    ) -> ImResult<ImSentMessage> {
        self.send_text(target, markdown, buttons).await
    }

    /// Replaces a message with rendered Markdown.
    async fn edit_markdown(
        &self,
        target: &ImTarget,
        message_id: &str,
        markdown: &str,
        buttons: &[ImButton],
    ) -> ImResult<()> {
        self.edit_text(target, message_id, markdown, buttons).await
    }

    /// Shows a chat action while a turn runs. A transport that has none does
    /// nothing, which is why the default is not an error.
    async fn send_activity(&self, _target: &ImTarget, _activity: ImActivity) -> ImResult<()> {
        Ok(())
    }

    /// Publishes the slash-command menu, when the transport has one.
    async fn register_commands(&self, _commands: &[ImCommandSpec]) -> ImResult<()> {
        Ok(())
    }

    async fn probe(&self) -> ImResult<ImChannelTestReport>;

    /// Whether an inbound group message is addressed to the bot.
    ///
    /// The adapter owns this policy because it depends on what the transport
    /// can see: Telegram knows its own `@username`, a thread transport may only
    /// ever receive commands.
    async fn is_group_trigger(&self, text: &str) -> bool;

    fn mark_inbound(&self);

    async fn create_thread(&self, _title: &str) -> ImResult<ImThreadTarget> {
        Err(crate::error::ImError::Unsupported)
    }

    async fn edit_thread_title(&self, _thread_key: &str, _title: &str) -> ImResult<()> {
        Err(crate::error::ImError::Unsupported)
    }

    /// Downloads the files a message carried.
    ///
    /// The adapter owns this because only it knows how the provider exposes
    /// file bytes and how large a download it will tolerate. The default
    /// refuses, so a transport without attachment support says so instead of
    /// silently dropping them.
    async fn download_attachments(
        &self,
        _message: &serde_json::Value,
    ) -> ImResult<Vec<crate::types::ImAttachment>> {
        Err(crate::error::ImError::Unsupported)
    }
}
