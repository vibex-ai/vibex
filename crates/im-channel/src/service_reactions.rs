//! Reaction mirroring: an Agent's emoji reaches the chat message it answered.
//!
//! The runtime records an Agent reaction as a hidden system notice whose
//! provider correlation names the user timeline item it decorates. That item id
//! is not the chat's message id, so the channel remembers the chat message a
//! prompt came from, keyed by the correlation the prompt carried; when the user
//! timeline item appears on the live stream, the target moves onto it. A later
//! reaction then finds the message to react to.
//!
//! The map is memory-only on purpose: a reaction is presentation and arrives
//! while the chat that sent the prompt is connected. A restart mid-turn loses
//! the mirror, not the transcript.

use vibex_core::{SystemNoticePayload, VIBEX_USE_REACTION_CORRELATION_PREFIX};

use super::*;

/// Where one prompt came from, so a reaction can find the chat message again.
#[derive(Debug, Clone)]
pub(super) struct PromptTarget {
    pub channel_id: ImChannelId,
    pub external_chat_id: String,
    pub thread_key: String,
    /// The provider's id for the message the reader sent.
    pub external_message_id: String,
    pub created_at_ms: i64,
}

/// A reaction arrives seconds after the message, not days.
const PROMPT_TARGET_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Enough for a busy group without letting the map grow without bound.
const PROMPT_TARGET_LIMIT: usize = 256;

#[derive(Default)]
pub(super) struct PromptTargets {
    /// Correlation id to target, until the user timeline item names itself.
    pending: HashMap<String, PromptTarget>,
    /// `(session, timeline item)` to target, once that item exists.
    by_item: HashMap<(String, String), PromptTarget>,
}

impl PromptTargets {
    /// Drops what is too old or leaves the oldest behind past the cap.
    fn prune(&mut self, now_ms: i64) {
        let cutoff = now_ms.saturating_sub(PROMPT_TARGET_TTL_MS);
        self.pending
            .retain(|_, target| target.created_at_ms >= cutoff);
        self.by_item
            .retain(|_, target| target.created_at_ms >= cutoff);
        while self.by_item.len() > PROMPT_TARGET_LIMIT {
            let Some(oldest) = self
                .by_item
                .iter()
                .min_by_key(|(_, target)| target.created_at_ms)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.by_item.remove(&oldest);
        }
        while self.pending.len() > PROMPT_TARGET_LIMIT {
            let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, target)| target.created_at_ms)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.pending.remove(&oldest);
        }
    }
}

impl ImChannelService {
    /// Remembers which chat message a submitted prompt came from.
    pub(super) fn note_prompt_target(&self, correlation: &CorrelationId, target: PromptTarget) {
        let Ok(mut targets) = self.inner.prompt_targets.lock() else {
            return;
        };
        targets.prune(unix_timestamp_ms());
        targets
            .pending
            .insert(correlation.as_str().to_string(), target);
    }

    /// Moves a pending target onto the user timeline item it became.
    pub(super) fn bind_prompt_item(
        &self,
        session_id: &VibexSessionId,
        item_id: &str,
        correlation: &str,
    ) {
        let Ok(mut targets) = self.inner.prompt_targets.lock() else {
            return;
        };
        let Some(target) = targets.pending.remove(correlation) else {
            return;
        };
        targets.by_item.insert(
            (session_id.as_str().to_string(), item_id.to_string()),
            target,
        );
    }

    fn prompt_target_for(
        &self,
        session_id: &VibexSessionId,
        item_id: &str,
    ) -> Option<PromptTarget> {
        let targets = self.inner.prompt_targets.lock().ok()?;
        targets
            .by_item
            .get(&(session_id.as_str().to_string(), item_id.to_string()))
            .cloned()
    }

    /// Mirrors an Agent reaction onto the chat message it reacted to.
    pub(super) async fn mirror_reaction(
        &self,
        session_id: &VibexSessionId,
        notice: &SystemNoticePayload,
        correlation: &str,
    ) {
        let Some(item_id) = correlation.strip_prefix(VIBEX_USE_REACTION_CORRELATION_PREFIX) else {
            return;
        };
        let Some(target) = self.prompt_target_for(session_id, item_id) else {
            return;
        };
        let Some(runtime) = self
            .inner
            .channels
            .read()
            .await
            .get(&target.channel_id)
            .cloned()
        else {
            return;
        };
        let target_message = ImTarget {
            chat_id: target.external_chat_id,
            thread_key: target.thread_key,
        };
        // A transport that cannot react, or a provider that refuses this emoji,
        // is not a failure the reader needs to hear about.
        if let Err(error) = runtime
            .adapter
            .set_reaction(
                &target_message,
                &target.external_message_id,
                notice.message.trim(),
            )
            .await
        {
            tracing::debug!(
                target: "vibex_im",
                error_code = error.code(),
                "IM reaction was not mirrored"
            );
        }
    }
}
