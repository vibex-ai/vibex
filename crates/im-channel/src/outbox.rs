//! Per-channel outbound queue.
//!
//! Every outbound message goes through one worker per channel rather than
//! calling the transport from whichever task happens to hold the reply. That
//! buys three things the transports need and the callers should not each
//! reimplement:
//!
//! - one call at a time per channel, so ordering is the order the runtime
//!   produced it;
//! - a token bucket per chat, so a burst of tool approvals does not trip the
//!   provider's rate limit;
//! - bounded retries for transient failures, honouring the provider's own
//!   `retry_after` when it sends one instead of guessing a delay.
//!
//! A caller still gets the provider message id back: the work is queued, but the
//! caller awaits the result. The priority decides how hard the worker tries
//! before it reports a failure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use vibex_core::{ImChannelState, ImChannelStatus, unix_timestamp_ms};

use crate::adapter::ImAdapter;
use crate::error::{ImError, ImResult};
use crate::types::{ImButton, ImSentMessage, ImTarget};

/// How hard the outbox tries before it reports a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundPriority {
    /// Approvals and anything the user must act on. A rate limit or a transport
    /// blip is retried for a long time before giving up.
    Critical,
    /// Final answers and errors. Retried briefly.
    Normal,
    /// Acknowledgements and progress. One attempt, then dropped.
    BestEffort,
}

impl OutboundPriority {
    const fn max_retries(self) -> u32 {
        match self {
            Self::Critical => 6,
            Self::Normal => 2,
            Self::BestEffort => 0,
        }
    }
}

/// A token bucket per chat id.
///
/// Provider limits are per chat, so a per-channel bucket would still let a busy
/// forum topic starve a quiet direct message. Capacity and refill are
/// deliberately conservative: a turn sends a handful of messages, so the bucket
/// only engages on a burst of approvals.
pub(crate) struct ChatRateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    capacity: f64,
    refill_per_second: f64,
}

struct Bucket {
    tokens: f64,
    last_refill: std::time::Instant,
}

impl ChatRateLimiter {
    pub(crate) fn new(capacity: f64, refill_per_second: f64) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: capacity.max(1.0),
            refill_per_second: refill_per_second.max(0.1),
        }
    }

    async fn acquire(&self, chat_id: &str) {
        loop {
            let wait = {
                let mut buckets = self.buckets.lock().expect("rate limiter lock");
                let capacity = self.capacity;
                let refill = self.refill_per_second;
                let bucket = buckets
                    .entry(chat_id.to_string())
                    .or_insert_with(|| Bucket {
                        tokens: capacity,
                        last_refill: std::time::Instant::now(),
                    });
                let now = std::time::Instant::now();
                let elapsed = now
                    .saturating_duration_since(bucket.last_refill)
                    .as_secs_f64();
                bucket.tokens = (bucket.tokens + elapsed * refill).min(capacity);
                bucket.last_refill = now;
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    return;
                }
                let missing = 1.0 - bucket.tokens;
                Duration::from_secs_f64(missing / refill)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

enum OutboundCall {
    Send {
        text: String,
        buttons: Vec<ImButton>,
    },
    Edit {
        message_id: String,
        text: String,
    },
}

struct OutboxJob {
    chat_id: String,
    target: ImTarget,
    call: OutboundCall,
    priority: OutboundPriority,
    reply: oneshot::Sender<ImResult<ImSentMessage>>,
}

/// The handle callers use to enqueue one outbound call.
#[derive(Clone)]
pub(crate) struct Outbox {
    tx: mpsc::Sender<OutboxJob>,
    status: Arc<Mutex<ImChannelStatus>>,
}

impl Outbox {
    /// Starts the channel's sender worker.
    ///
    /// The worker owns the adapter and the rate limiter. When the shutdown
    /// signal fires it stops taking work: a call in flight finishes, anything
    /// still queued is dropped, and the caller sees that as a closed channel.
    pub(crate) fn spawn(
        adapter: Arc<dyn ImAdapter>,
        status: Arc<Mutex<ImChannelStatus>>,
        mut shutdown: watch::Receiver<bool>,
    ) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<OutboxJob>(64);
        let task_status = Arc::clone(&status);
        let task = tokio::spawn(async move {
            let rate = ChatRateLimiter::new(3.0, 4.0);
            loop {
                let job = tokio::select! {
                    _ = shutdown.changed() => break,
                    job = rx.recv() => match job {
                        Some(job) => job,
                        None => break,
                    },
                };
                let chat_id = job.chat_id.clone();
                let result = deliver(&*adapter, &rate, &task_status, &chat_id, &job).await;
                note_status(&task_status, &result);
                let _ = job.reply.send(result);
            }
        });
        (Self { tx, status }, task)
    }

    pub(crate) async fn send(
        &self,
        target: ImTarget,
        text: String,
        buttons: Vec<ImButton>,
        priority: OutboundPriority,
    ) -> ImResult<ImSentMessage> {
        let (reply, wait) = oneshot::channel();
        self.enqueue(OutboxJob {
            chat_id: target.chat_id.clone(),
            target,
            call: OutboundCall::Send { text, buttons },
            priority,
            reply,
        })
        .await?;
        wait.await.map_err(|_| ImError::Internal)?
    }

    pub(crate) async fn edit(
        &self,
        target: ImTarget,
        message_id: String,
        text: String,
    ) -> ImResult<()> {
        let (reply, wait) = oneshot::channel();
        self.enqueue(OutboxJob {
            chat_id: target.chat_id.clone(),
            target,
            call: OutboundCall::Edit { message_id, text },
            priority: OutboundPriority::Normal,
            reply,
        })
        .await?;
        wait.await.map_err(|_| ImError::Internal)??;
        Ok(())
    }

    async fn enqueue(&self, job: OutboxJob) -> ImResult<()> {
        if let Ok(mut status) = self.status.lock() {
            status.pending_outbox = status.pending_outbox.saturating_add(1);
        }
        self.tx.send(job).await.map_err(|_| ImError::Internal)
    }
}

async fn deliver(
    adapter: &dyn ImAdapter,
    rate: &ChatRateLimiter,
    status: &Mutex<ImChannelStatus>,
    chat_id: &str,
    job: &OutboxJob,
) -> ImResult<ImSentMessage> {
    let mut attempt = 0_u32;
    loop {
        rate.acquire(chat_id).await;
        let result = match &job.call {
            OutboundCall::Send { text, buttons } => {
                adapter.send_text(&job.target, text, buttons).await
            }
            OutboundCall::Edit { message_id, text } => adapter
                .edit_text(&job.target, message_id, text, &[])
                .await
                .map(|()| ImSentMessage {
                    message_id: message_id.clone(),
                }),
        };
        match result {
            Ok(sent) => return Ok(sent),
            Err(error) => {
                let retryable = matches!(
                    error,
                    ImError::RateLimited { .. }
                        | ImError::Transport
                        | ImError::Internal
                        | ImError::ProviderRejected { .. }
                );
                if attempt >= job.priority.max_retries() || !retryable {
                    return Err(error);
                }
                attempt = attempt.saturating_add(1);
                let delay = match &error {
                    ImError::RateLimited { retry_after_ms } => {
                        Duration::from_millis((*retry_after_ms).max(500))
                    }
                    _ => Duration::from_millis(500_u64.saturating_mul(1_u64 << attempt.min(4))),
                };
                if let Ok(mut guard) = status.lock() {
                    guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
                    guard.last_error = Some(vibex_core::ImChannelErrorView {
                        code: error.code().to_string(),
                        message: error.to_string(),
                        retry_at_ms: Some(
                            unix_timestamp_ms().saturating_add(delay.as_millis() as i64),
                        ),
                    });
                }
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn note_status(status: &Mutex<ImChannelStatus>, result: &ImResult<ImSentMessage>) {
    let Ok(mut status) = status.lock() else {
        return;
    };
    status.pending_outbox = status.pending_outbox.saturating_sub(1);
    status.generation = status.generation.saturating_add(1);
    match result {
        Ok(_) => {
            status.consecutive_failures = 0;
            status.last_outbound_at_ms = Some(unix_timestamp_ms());
            status.last_error = None;
            if status.state != ImChannelState::Connected {
                status.state = ImChannelState::Connected;
            }
        }
        Err(error) => {
            status.consecutive_failures = status.consecutive_failures.saturating_add(1);
            status.last_error = Some(vibex_core::ImChannelErrorView {
                code: error.code().to_string(),
                message: error.to_string(),
                retry_at_ms: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use vibex_core::{ImChannelKind, ImChannelTestReport};

    use super::*;
    use crate::types::ImThreadTarget;

    /// Fails the first `failures` sends with a rate limit, then succeeds.
    struct FlakyAdapter {
        failures: AtomicU32,
        calls: AtomicU32,
        retry_after_ms: u64,
    }

    impl FlakyAdapter {
        fn new(failures: u32) -> Self {
            Self::with_retry_after(failures, 1)
        }

        fn with_retry_after(failures: u32, retry_after_ms: u64) -> Self {
            Self {
                failures: AtomicU32::new(failures),
                calls: AtomicU32::new(0),
                retry_after_ms,
            }
        }
    }

    #[async_trait]
    impl ImAdapter for FlakyAdapter {
        fn kind(&self) -> ImChannelKind {
            ImChannelKind::Telegram
        }

        async fn send_text(
            &self,
            _target: &ImTarget,
            _text: &str,
            _buttons: &[ImButton],
        ) -> ImResult<ImSentMessage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let remaining = self.failures.load(Ordering::SeqCst);
            if remaining > 0 {
                self.failures.store(remaining - 1, Ordering::SeqCst);
                return Err(ImError::RateLimited {
                    retry_after_ms: self.retry_after_ms,
                });
            }
            Ok(ImSentMessage {
                message_id: "1".to_string(),
            })
        }

        async fn edit_text(
            &self,
            _target: &ImTarget,
            _message_id: &str,
            _text: &str,
            _buttons: &[ImButton],
        ) -> ImResult<()> {
            Ok(())
        }

        async fn answer_callback(
            &self,
            _callback_query_id: &str,
            _text: Option<&str>,
        ) -> ImResult<()> {
            Ok(())
        }

        async fn probe(&self) -> ImResult<ImChannelTestReport> {
            Ok(ImChannelTestReport {
                ok: true,
                bot_name: None,
                chat_title: None,
                warnings: Vec::new(),
            })
        }

        async fn is_group_trigger(&self, _text: &str) -> bool {
            false
        }

        fn mark_inbound(&self) {}

        async fn create_thread(&self, _title: &str) -> ImResult<ImThreadTarget> {
            Err(ImError::Unsupported)
        }

        async fn edit_thread_title(&self, _thread_key: &str, _title: &str) -> ImResult<()> {
            Err(ImError::Unsupported)
        }
    }

    fn target() -> ImTarget {
        ImTarget {
            chat_id: "42".to_string(),
            thread_key: String::new(),
        }
    }

    #[tokio::test]
    async fn a_busy_chat_does_not_block_another_chat() {
        let limiter = ChatRateLimiter::new(1.0, 0.5);
        limiter.acquire("busy").await;
        // The busy chat has spent its only token; a different chat still has
        // its own and must not wait for the refill.
        let started = std::time::Instant::now();
        limiter.acquire("quiet").await;
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn critical_outbound_is_retried_past_a_rate_limit() {
        let adapter = Arc::new(FlakyAdapter::new(2));
        let status = Arc::new(Mutex::new(ImChannelStatus::default()));
        // The sender has to stay alive: a dropped one closes the receiver, and
        // the worker would treat that as a shutdown before it sends anything.
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (outbox, _task) = Outbox::spawn(adapter.clone(), Arc::clone(&status), shutdown_rx);
        let sent = outbox
            .send(
                target(),
                "approval".to_string(),
                Vec::new(),
                OutboundPriority::Critical,
            )
            .await
            .expect("critical send succeeds after retries");
        assert_eq!(sent.message_id, "1");
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        let status = status.lock().expect("status");
        assert_eq!(status.pending_outbox, 0);
        assert_eq!(status.consecutive_failures, 0);
        assert_eq!(status.state, ImChannelState::Connected);
    }

    #[tokio::test]
    async fn best_effort_outbound_gives_up_immediately() {
        let adapter = Arc::new(FlakyAdapter::new(1));
        let status = Arc::new(Mutex::new(ImChannelStatus::default()));
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (outbox, _task) = Outbox::spawn(adapter.clone(), Arc::clone(&status), shutdown_rx);
        let error = outbox
            .send(
                target(),
                "ack".to_string(),
                Vec::new(),
                OutboundPriority::BestEffort,
            )
            .await
            .expect_err("best effort reports the rate limit");
        assert!(matches!(error, ImError::RateLimited { .. }));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        let status = status.lock().expect("status");
        assert_eq!(status.pending_outbox, 0);
        assert_eq!(status.consecutive_failures, 1);
        assert!(status.last_error.is_some());
    }
}
