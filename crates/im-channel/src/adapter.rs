use async_trait::async_trait;
use vibex_core::{ImChannelKind, ImChannelTestReport};

use crate::error::ImResult;
use crate::types::{ImButton, ImSentMessage, ImTarget, ImThreadTarget};

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

    async fn probe(&self) -> ImResult<ImChannelTestReport>;

    async fn is_group_trigger(&self, text: &str) -> bool;

    fn mark_inbound(&self);

    fn requires_group_mention(&self) -> bool;

    async fn create_thread(&self, _title: &str) -> ImResult<ImThreadTarget> {
        Err(crate::error::ImError::Unsupported)
    }

    async fn edit_thread_title(&self, _thread_key: &str, _title: &str) -> ImResult<()> {
        Err(crate::error::ImError::Unsupported)
    }
}
