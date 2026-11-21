//! The process-wide Tokio runtime.
//!
//! `nsh_core::element::background` is `tokio::spawn`, which needs an ambient
//! runtime; an engine task does not carry one, so a spawn from a press handler
//! dies with the task and leaves the page waiting on a signal nothing will set.
//! The GPUI client keeps the same helper for the same reason
//! (`apps/mobile/src/background_connection.rs`). Pairing, probes and the
//! session shell's network work run on this runtime, off the engine thread,
//! and the engine task awaits the returned handle.

use std::sync::OnceLock;

fn process_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to initialize the mobile process runtime")
    })
}

/// A handle to the process runtime, for work that must not run on the engine
/// thread.
pub fn handle() -> tokio::runtime::Handle {
    process_runtime().handle().clone()
}

/// Runs `future` to completion on the process runtime (tests only).
#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    process_runtime().block_on(future)
}

#[cfg(test)]
mod tests {
    #[test]
    fn work_spawns_without_an_ambient_runtime() {
        // The engine task that starts pairing is not inside a Tokio runtime.
        // `tokio::spawn` there panics, which is how the page ended up waiting
        // on a signal nothing would set; the process handle does not need one.
        assert!(tokio::runtime::Handle::try_current().is_err());
        let task = super::handle().spawn(async { 7u8 });
        assert_eq!(super::block_on(task).expect("the task ran"), 7);
    }
}
