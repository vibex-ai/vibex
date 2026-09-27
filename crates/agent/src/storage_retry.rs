//! Bounded retries for transient local SQLite contention.
//!
//! Every durable write in this crate opens its own connection, so two of them
//! can overlap on the same database file. SQLite reports that overlap as a
//! *transient* failure (`database is locked`, `SQLITE_BUSY`, or the snapshot
//! variant a deferred read-then-write transaction hits). The same statement
//! succeeds once the other writer commits, so callers that would otherwise
//! terminalize user-visible work must retry instead of reporting a durable
//! fault.

use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use vibex_core::{VibexError, VibexResult};

/// Attempts after the first failure. Lock contention inside one process
/// clears within a scheduler hop, so a handful of backed-off retries is
/// enough; a permanently locked database still surfaces the original error.
pub(crate) const TRANSIENT_STORAGE_RETRY_LIMIT: usize = 5;

const TRANSIENT_STORAGE_RETRY_BASE_DELAY: Duration = Duration::from_millis(25);
const TRANSIENT_STORAGE_RETRY_MAX_DELAY: Duration = Duration::from_millis(400);

/// Whether `error` is local SQLite lock contention rather than a durable fault.
///
/// `storage_err` keeps the raw SQLite message as a diagnostic, so the message
/// is the only place the two cases differ.
pub(crate) fn is_transient_storage_error(error: &VibexError) -> bool {
    error.diagnostics.iter().any(|diagnostic| {
        let value = diagnostic.value.to_ascii_lowercase();
        value.contains("database is locked")
            || value.contains("database table is locked")
            || value.contains("database schema is locked")
            || value.contains("database busy")
            || value.contains("sqlite_busy")
    })
}

/// Exponential backoff, capped so a contended write still lands promptly.
pub(crate) fn transient_storage_retry_delay(attempt: usize) -> Duration {
    let shift = u32::try_from(attempt.min(4)).unwrap_or(4);
    TRANSIENT_STORAGE_RETRY_BASE_DELAY
        .saturating_mul(1u32 << shift)
        .min(TRANSIENT_STORAGE_RETRY_MAX_DELAY)
}

/// Runs `operation` again while it keeps failing with transient contention and
/// returns the last error once the retry budget is exhausted.
pub(crate) async fn retry_transient_storage<T, F, Fut>(mut operation: F) -> VibexResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = VibexResult<T>>,
{
    let mut attempt = 0;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error)
                if is_transient_storage_error(&error)
                    && attempt < TRANSIENT_STORAGE_RETRY_LIMIT =>
            {
                attempt += 1;
                sleep(transient_storage_retry_delay(attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn locked_error(code: &str) -> VibexError {
        VibexError::storage(code, "failed to write durable state")
            .with_diagnostic("error", "database is locked")
    }

    #[test]
    fn only_lock_contention_is_transient() {
        assert!(is_transient_storage_error(&locked_error(
            "runtime_switch_insert_failed"
        )));
        assert!(is_transient_storage_error(
            &VibexError::storage("timeline_append_failed", "failed to append")
                .with_diagnostic("error", "database table is locked")
        ));
        assert!(!is_transient_storage_error(
            &VibexError::storage("runtime_switch_insert_failed", "failed to insert")
                .with_diagnostic("error", "no such column: pending_switch_id")
        ));
        assert!(!is_transient_storage_error(&VibexError::conflict(
            "runtime_switch_pending_exists",
            "another runtime switch is already pending for this session",
        )));
    }

    #[test]
    fn retry_delay_is_bounded() {
        assert!(transient_storage_retry_delay(1) < transient_storage_retry_delay(2));
        assert_eq!(
            transient_storage_retry_delay(64),
            TRANSIENT_STORAGE_RETRY_MAX_DELAY
        );
    }

    #[tokio::test]
    async fn transient_errors_are_retried_and_durable_errors_are_not() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let value = retry_transient_storage(|| {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(locked_error("runtime_switch_insert_failed"))
                } else {
                    Ok(attempt)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let error = retry_transient_storage(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                Err::<(), _>(
                    VibexError::storage("runtime_switch_insert_failed", "failed to insert")
                        .with_diagnostic("error", "UNIQUE constraint failed"),
                )
            }
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "runtime_switch_insert_failed");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let error = retry_transient_storage(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { Err::<(), _>(locked_error("runtime_switch_insert_failed")) }
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "runtime_switch_insert_failed");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            TRANSIENT_STORAGE_RETRY_LIMIT + 1
        );
    }
}
