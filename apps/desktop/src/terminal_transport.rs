//! Authority-agnostic terminal transport for the desktop terminal surface.
//!
//! The surface renders raw PTY bytes but never owns the process. Native mode
//! serves those bytes from an in-process [`TerminalManager`]; remote mode
//! serves the same contract from the paired runtime through
//! [`TerminalBackend`]. Keeping both behind [`TerminalTransport`] lets the
//! surface render an authoritative terminal without knowing which runtime owns
//! it.

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use gpui::{BackgroundExecutor, Task};
use vibex_backend::{
    BackendError, MutationRequest, TerminalBackend, TerminalFrame, TerminalFrameBatch,
};
use vibex_content::TERMINAL_RAW_BUFFER_BYTES;
use vibex_core::{
    TerminalCreateRequest, TerminalId, TerminalResizeRequest, TerminalSession, TerminalStatus,
    TerminalSwitchShellRequest, TerminalWriteRequest, VibexError,
};
use vibex_terminal::{TerminalManager, TerminalRawOutputChunk, TerminalRawSnapshot};

/// Failure reported by a [`TerminalTransport`] operation.
///
/// Both authorities speak through this shape: the in-process manager's
/// [`VibexError`] and the backend facade's [`BackendError`] carry the same
/// domain code and message, and the surface renders the message it already
/// rendered for the local path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTransportError {
    pub code: String,
    pub message: String,
}

impl TerminalTransportError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<VibexError> for TerminalTransportError {
    fn from(error: VibexError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<BackendError> for TerminalTransportError {
    fn from(error: BackendError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

pub type TerminalTransportResult<T> = Result<T, TerminalTransportError>;

pub type TerminalTransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = TerminalTransportResult<T>> + Send + 'a>>;

/// Operations the desktop terminal surface needs from whichever authority owns
/// the PTY.
///
/// Every method is asynchronous because a paired runtime answers over the
/// remote connection. The local transport resolves without awaiting, so the
/// surface's existing synchronous transitions stay synchronous when the
/// in-process manager owns the terminal.
pub trait TerminalTransport: Send + Sync + 'static {
    /// Starts a terminal inside `workspace_root`.
    fn create_terminal(
        &self,
        workspace_root: &Path,
        request: TerminalCreateRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession>;

    /// Re-attaches a persisted session, restarting it when the authority no
    /// longer holds the PTY.
    fn restore_terminal(
        &self,
        workspace_root: &Path,
        session: TerminalSession,
    ) -> TerminalTransportFuture<'_, TerminalSession>;

    /// Replaces the session's shell without creating a second terminal domain
    /// where the authority supports it.
    fn switch_shell(
        &self,
        request: TerminalSwitchShellRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession>;

    fn write_bytes(
        &self,
        terminal_id: &TerminalId,
        bytes: &[u8],
    ) -> TerminalTransportFuture<'_, ()>;

    fn resize_terminal(
        &self,
        request: &TerminalResizeRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession>;

    /// Stops the terminal and returns its final session.
    fn close_terminal(
        &self,
        terminal_id: &TerminalId,
    ) -> TerminalTransportFuture<'_, TerminalSession>;

    /// Returns the session plus the raw output observed after `next_sequence`.
    ///
    /// The snapshot is the parser's contract: chunk sequences are contiguous,
    /// `retained_bytes` matches the returned chunks, and `next_sequence` is the
    /// cursor that follows them. A gap between `next_sequence` and the first
    /// returned chunk tells the parser to rebuild, exactly as it does for the
    /// in-process ring.
    fn poll_terminal(
        &self,
        terminal_id: &TerminalId,
        next_sequence: i64,
    ) -> TerminalTransportFuture<'_, TerminalRawSnapshot>;
}

/// Terminal transport backed by the in-process [`TerminalManager`].
#[derive(Clone)]
pub struct LocalTerminalTransport {
    manager: TerminalManager,
}

impl LocalTerminalTransport {
    pub fn new(manager: TerminalManager) -> Self {
        Self { manager }
    }

    /// Creates a terminal without awaiting.
    ///
    /// `TerminalSurface::new` must know its initial session before it can build
    /// the widget, and the in-process manager answers without awaiting.
    pub fn create_now(
        &self,
        workspace_root: &Path,
        request: TerminalCreateRequest,
    ) -> TerminalTransportResult<TerminalSession> {
        self.manager
            .create(workspace_root, request)
            .map_err(Into::into)
    }
}

impl TerminalTransport for LocalTerminalTransport {
    fn create_terminal(
        &self,
        workspace_root: &Path,
        request: TerminalCreateRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let manager = self.manager.clone();
        let workspace_root = workspace_root.to_path_buf();
        Box::pin(async move { manager.create(&workspace_root, request).map_err(Into::into) })
    }

    fn restore_terminal(
        &self,
        workspace_root: &Path,
        session: TerminalSession,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let manager = self.manager.clone();
        let workspace_root = workspace_root.to_path_buf();
        Box::pin(async move {
            manager
                .restore(&workspace_root, session)
                .map_err(Into::into)
        })
    }

    fn switch_shell(
        &self,
        request: TerminalSwitchShellRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let manager = self.manager.clone();
        Box::pin(async move { manager.switch_shell(&request).map_err(Into::into) })
    }

    fn write_bytes(
        &self,
        terminal_id: &TerminalId,
        bytes: &[u8],
    ) -> TerminalTransportFuture<'_, ()> {
        let manager = self.manager.clone();
        let terminal_id = terminal_id.clone();
        let bytes = bytes.to_vec();
        Box::pin(async move {
            manager
                .write_bytes(&terminal_id, &bytes)
                .map_err(Into::into)
        })
    }

    fn resize_terminal(
        &self,
        request: &TerminalResizeRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let manager = self.manager.clone();
        let request = request.clone();
        Box::pin(async move { manager.resize(&request).map_err(Into::into) })
    }

    fn close_terminal(
        &self,
        terminal_id: &TerminalId,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let manager = self.manager.clone();
        let terminal_id = terminal_id.clone();
        Box::pin(async move { manager.kill(&terminal_id).map_err(Into::into) })
    }

    fn poll_terminal(
        &self,
        terminal_id: &TerminalId,
        next_sequence: i64,
    ) -> TerminalTransportFuture<'_, TerminalRawSnapshot> {
        let manager = self.manager.clone();
        let terminal_id = terminal_id.clone();
        Box::pin(async move {
            manager
                .raw_snapshot_from(&terminal_id, next_sequence)
                .map_err(Into::into)
        })
    }
}

/// Remote frames buffered per terminal before the oldest are dropped.
///
/// The budget matches the in-process raw observation ring so the remote parser
/// rebuilds under the same pressure as the native one.
const REMOTE_TERMINAL_BUFFER_BYTES: usize = TERMINAL_RAW_BUFFER_BYTES;

/// Delay before re-attaching a frame stream that ended while the authority
/// still reports the terminal as running.
const REMOTE_TERMINAL_REATTACH_DELAY: Duration = Duration::from_millis(1_000);

/// Terminal transport backed by a paired runtime's [`TerminalBackend`].
///
/// The backend delivers output as a frame subscription, while the surface polls
/// a cursor. Each terminal therefore gets a pump task that buffers whatever the
/// subscription delivers, so a poll only drains frames the parser has not seen
/// and never waits on a quiet terminal.
pub struct RemoteTerminalTransport {
    backend: Arc<dyn TerminalBackend>,
    executor: BackgroundExecutor,
    terminals: Arc<Mutex<BTreeMap<String, Arc<RemoteTerminal>>>>,
}

impl RemoteTerminalTransport {
    pub fn new(backend: Arc<dyn TerminalBackend>, executor: BackgroundExecutor) -> Self {
        Self {
            backend,
            executor,
            terminals: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl TerminalTransport for RemoteTerminalTransport {
    fn create_terminal(
        &self,
        _workspace_root: &Path,
        request: TerminalCreateRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        Box::pin(async move {
            let session = backend
                .create_terminal(MutationRequest::new(request))
                .await?;
            lock_unpoisoned(&terminals).insert(
                session.id.as_str().to_string(),
                Arc::new(RemoteTerminal::new(session.clone())),
            );
            Ok(session)
        })
    }

    fn restore_terminal(
        &self,
        _workspace_root: &Path,
        session: TerminalSession,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        Box::pin(async move {
            let requested_id = session.id.clone();
            let restored = match backend.terminal_snapshot(requested_id.clone()).await {
                Ok(snapshot) => snapshot.session,
                // A closed terminal leaves the authority's registry, so a
                // restart re-creates it from the persisted session parameters.
                Err(_) => {
                    backend
                        .create_terminal(MutationRequest::new(TerminalCreateRequest {
                            workspace_id: session.workspace_id.clone(),
                            title: Some(session.title.clone()),
                            shell: Some(session.shell.clone()),
                            cwd: Some(session.cwd.clone()),
                            rows: session.rows,
                            cols: session.cols,
                        }))
                        .await?
                }
            };
            let mut terminals = lock_unpoisoned(&terminals);
            terminals.remove(requested_id.as_str());
            terminals.insert(
                restored.id.as_str().to_string(),
                Arc::new(RemoteTerminal::new(restored.clone())),
            );
            drop(terminals);
            Ok(restored)
        })
    }

    fn switch_shell(
        &self,
        request: TerminalSwitchShellRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        Box::pin(async move {
            let cached = lock_unpoisoned(&terminals)
                .get(request.terminal_id.as_str())
                .cloned();
            let previous = match cached {
                Some(terminal) => terminal.session().ok_or_else(missing_session_error)?,
                None => {
                    backend
                        .terminal_snapshot(request.terminal_id.clone())
                        .await?
                        .session
                }
            };
            // `TerminalBackend` has no shell mutation, so the requested shell
            // becomes the successor session and the previous one is retired.
            let session = backend
                .create_terminal(MutationRequest::new(TerminalCreateRequest {
                    workspace_id: previous.workspace_id.clone(),
                    title: Some(previous.title.clone()),
                    shell: Some(request.shell),
                    cwd: Some(previous.cwd.clone()),
                    rows: previous.rows,
                    cols: previous.cols,
                }))
                .await?;
            let _ = backend
                .close_terminal(MutationRequest::new(previous.id.clone()))
                .await;
            let mut terminals = lock_unpoisoned(&terminals);
            terminals.remove(previous.id.as_str());
            terminals.insert(
                session.id.as_str().to_string(),
                Arc::new(RemoteTerminal::new(session.clone())),
            );
            drop(terminals);
            Ok(session)
        })
    }

    fn write_bytes(
        &self,
        terminal_id: &TerminalId,
        bytes: &[u8],
    ) -> TerminalTransportFuture<'_, ()> {
        let backend = self.backend.clone();
        let terminal_id = terminal_id.clone();
        // The remote write contract carries text; every byte the surface
        // produces (text, key, mouse and paste encodings) is UTF-8.
        let Ok(data) = std::str::from_utf8(bytes) else {
            return Box::pin(async move {
                Err(TerminalTransportError::new(
                    "terminal_input_not_utf8",
                    "terminal input must use the UTF-8 control contract",
                ))
            });
        };
        let data = data.to_string();
        Box::pin(async move {
            backend
                .write_terminal(MutationRequest::new(TerminalWriteRequest {
                    terminal_id,
                    data,
                }))
                .await?;
            Ok(())
        })
    }

    fn resize_terminal(
        &self,
        request: &TerminalResizeRequest,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        let terminal_id = request.terminal_id.clone();
        let request = request.clone();
        Box::pin(async move {
            let session = backend
                .resize_terminal(MutationRequest::new(request))
                .await?;
            if let Some(terminal) = lock_unpoisoned(&terminals)
                .get(terminal_id.as_str())
                .cloned()
            {
                terminal.set_session(session.clone());
            }
            Ok(session)
        })
    }

    fn close_terminal(
        &self,
        terminal_id: &TerminalId,
    ) -> TerminalTransportFuture<'_, TerminalSession> {
        let backend = self.backend.clone();
        let terminals = self.terminals.clone();
        let terminal_id = terminal_id.clone();
        Box::pin(async move {
            let session = backend
                .close_terminal(MutationRequest::new(terminal_id.clone()))
                .await?;
            // Dropping the entry cancels the frame pump with it.
            lock_unpoisoned(&terminals).remove(terminal_id.as_str());
            Ok(session)
        })
    }

    fn poll_terminal(
        &self,
        terminal_id: &TerminalId,
        next_sequence: i64,
    ) -> TerminalTransportFuture<'_, TerminalRawSnapshot> {
        let backend = self.backend.clone();
        let executor = self.executor.clone();
        let terminals = self.terminals.clone();
        let terminal_id = terminal_id.clone();
        Box::pin(async move {
            let cached = lock_unpoisoned(&terminals)
                .get(terminal_id.as_str())
                .cloned();
            let terminal = match cached {
                Some(terminal) => terminal,
                // A surface may render a session this transport never created,
                // for example one listed from the authority. Read the
                // authoritative session once, then keep polling that entry.
                None => match backend.terminal_snapshot(terminal_id.clone()).await {
                    Ok(snapshot) => {
                        let terminal = Arc::new(RemoteTerminal::new(snapshot.session));
                        lock_unpoisoned(&terminals)
                            .insert(terminal_id.as_str().to_string(), terminal.clone());
                        terminal
                    }
                    Err(error) => {
                        // Remember the refusal so a stale surface cannot turn
                        // every poll into another round trip.
                        let terminal = Arc::new(RemoteTerminal::unavailable(
                            terminal_id.clone(),
                            error.into(),
                        ));
                        lock_unpoisoned(&terminals)
                            .insert(terminal_id.as_str().to_string(), terminal.clone());
                        terminal
                    }
                },
            };
            terminal.ensure_pump(&backend, &executor, next_sequence)?;
            terminal.poll_snapshot(next_sequence)
        })
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn missing_session_error() -> TerminalTransportError {
    TerminalTransportError::new(
        "terminal_session_missing",
        "terminal session is not available",
    )
}

/// One remote terminal: its authoritative session plus the frames its pump has
/// observed but the surface has not polled yet.
struct RemoteTerminal {
    terminal_id: TerminalId,
    state: Mutex<RemoteTerminalState>,
}

struct RemoteTerminalState {
    session: Option<TerminalSession>,
    error: Option<TerminalTransportError>,
    frames: VecDeque<TerminalFrame>,
    retained_bytes: usize,
    dropped_frames: u64,
    pump: Option<Task<()>>,
}

impl RemoteTerminal {
    fn new(session: TerminalSession) -> Self {
        Self {
            terminal_id: session.id.clone(),
            state: Mutex::new(RemoteTerminalState {
                session: Some(session),
                error: None,
                frames: VecDeque::new(),
                retained_bytes: 0,
                dropped_frames: 0,
                pump: None,
            }),
        }
    }

    fn unavailable(terminal_id: TerminalId, error: TerminalTransportError) -> Self {
        Self {
            terminal_id,
            state: Mutex::new(RemoteTerminalState {
                session: None,
                error: Some(error),
                frames: VecDeque::new(),
                retained_bytes: 0,
                dropped_frames: 0,
                pump: None,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, RemoteTerminalState> {
        lock_unpoisoned(&self.state)
    }

    fn session(&self) -> Option<TerminalSession> {
        self.state().session.clone()
    }

    fn set_session(&self, session: TerminalSession) {
        let mut state = self.state();
        state.session = Some(session);
        state.error = None;
    }

    /// Starts the frame pump once. Later polls reuse it so the subscription
    /// keeps its place even while the surface is idle.
    fn ensure_pump(
        self: &Arc<Self>,
        backend: &Arc<dyn TerminalBackend>,
        executor: &BackgroundExecutor,
        next_sequence: i64,
    ) -> TerminalTransportResult<()> {
        let mut state = self.state();
        if state.pump.is_some() || state.session.is_none() {
            return Ok(());
        }
        let subscription =
            backend.subscribe_terminal(self.terminal_id.clone(), next_sequence.max(1))?;
        let backend = backend.clone();
        let spawn_executor = executor.clone();
        let timer_executor = executor.clone();
        let terminal = Arc::downgrade(self);
        state.pump = Some(spawn_executor.spawn(async move {
            let mut subscription = subscription;
            let mut cursor = next_sequence.max(1);
            loop {
                match subscription.next().await {
                    Ok(Some(batch)) => {
                        cursor = batch.next_sequence.max(cursor);
                        let Some(terminal) = terminal.upgrade() else {
                            break;
                        };
                        terminal.push_batch(batch);
                    }
                    Ok(None) | Err(_) => {
                        let Some(terminal) = terminal.upgrade() else {
                            break;
                        };
                        // A stream can end because the socket dropped while the
                        // terminal keeps running; only a finished session stops
                        // the pump for good.
                        if !terminal.refresh_after_stream_end(&backend).await {
                            break;
                        }
                        timer_executor.timer(REMOTE_TERMINAL_REATTACH_DELAY).await;
                        match backend.subscribe_terminal(terminal.terminal_id.clone(), cursor) {
                            Ok(next) => subscription = next,
                            Err(_) => break,
                        }
                    }
                }
            }
        }));
        Ok(())
    }

    /// Refreshes the session once the frame stream ended. Returns whether the
    /// terminal is still running and the pump should re-attach.
    async fn refresh_after_stream_end(&self, backend: &Arc<dyn TerminalBackend>) -> bool {
        match backend.terminal_snapshot(self.terminal_id.clone()).await {
            Ok(snapshot) => {
                let running = snapshot.session.status == TerminalStatus::Running;
                self.set_session(snapshot.session);
                running
            }
            Err(_) => true,
        }
    }

    fn push_batch(&self, batch: TerminalFrameBatch) {
        let mut state = self.state();
        // `reset_required` needs no separate handling: the parser rebuilds when
        // the sequence cursor moves backwards or leaves a gap, and the authority
        // sets the flag from that same condition on the frames it replays.
        state.dropped_frames = state.dropped_frames.max(batch.dropped_frames);
        for frame in batch.frames {
            state.retained_bytes = state.retained_bytes.saturating_add(frame.bytes.len());
            state.frames.push_back(frame);
        }
        while state.retained_bytes > REMOTE_TERMINAL_BUFFER_BYTES && state.frames.len() > 1 {
            if state.drop_front().is_some() {
                state.dropped_frames = state.dropped_frames.saturating_add(1);
            }
        }
    }

    fn poll_snapshot(&self, next_sequence: i64) -> TerminalTransportResult<TerminalRawSnapshot> {
        let mut state = self.state();
        let Some(session) = state.session.clone() else {
            return Err(state.error.clone().unwrap_or_else(missing_session_error));
        };
        let frames = state.drain(next_sequence);
        let next_sequence = frames
            .last()
            .map(|frame| frame.sequence.saturating_add(1))
            .unwrap_or(next_sequence);
        let retained_bytes = frames.iter().map(|frame| frame.bytes.len()).sum();
        let chunks = frames
            .into_iter()
            .map(|frame| TerminalRawOutputChunk {
                sequence: frame.sequence,
                data: frame.bytes,
            })
            .collect();
        Ok(TerminalRawSnapshot {
            session,
            chunks,
            next_sequence,
            retained_bytes,
            dropped_chunks: state.dropped_frames,
        })
    }
}

impl RemoteTerminalState {
    /// Returns the run of frames the parser can consume in one snapshot.
    ///
    /// Frames the parser already consumed are discarded. The run starts at the
    /// first frame the parser has not seen, even when the authority dropped the
    /// frames before it: the parser answers a cursor gap with a rebuild, which
    /// is exactly what the in-process ring makes it do. The run ends at a
    /// discontinuity because a snapshot must carry contiguous sequences; the
    /// frames beyond it are handed over on the next poll, once the parser has
    /// rebuilt to this run's end.
    fn drain(&mut self, next_sequence: i64) -> Vec<TerminalFrame> {
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.sequence < next_sequence)
        {
            self.drop_front();
        }
        let mut frames: Vec<TerminalFrame> = Vec::new();
        while let Some(next) = self.frames.front().map(|frame| frame.sequence) {
            if frames
                .last()
                .is_some_and(|previous| next != previous.sequence.saturating_add(1))
            {
                break;
            }
            let Some(frame) = self.drop_front() else {
                break;
            };
            frames.push(frame);
        }
        frames
    }

    fn drop_front(&mut self) -> Option<TerminalFrame> {
        let frame = self.frames.pop_front()?;
        self.retained_bytes = self.retained_bytes.saturating_sub(frame.bytes.len());
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use vibex_core::WorkspaceId;

    /// Polls a transport future once. The in-process manager answers without
    /// awaiting, so one poll resolves the local contract.
    fn poll_once<F: Future>(future: F) -> Option<F::Output> {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        }
    }

    fn session(terminal_id: TerminalId) -> TerminalSession {
        TerminalSession {
            id: terminal_id,
            workspace_id: WorkspaceId::new(),
            title: "Terminal 1".into(),
            shell: "/bin/sh".into(),
            cwd: "/tmp".into(),
            rows: 24,
            cols: 80,
            status: TerminalStatus::Running,
            created_at_ms: 1,
            updated_at_ms: 1,
            closed_at_ms: None,
        }
    }

    fn batch(
        terminal_id: &TerminalId,
        frames: &[(i64, &[u8])],
        next_sequence: i64,
        dropped_frames: u64,
    ) -> TerminalFrameBatch {
        TerminalFrameBatch {
            terminal_id: terminal_id.clone(),
            frames: frames
                .iter()
                .map(|(sequence, bytes)| TerminalFrame {
                    sequence: *sequence,
                    bytes: bytes.to_vec(),
                })
                .collect(),
            next_sequence,
            dropped_frames,
            reset_required: false,
        }
    }

    #[test]
    fn remote_poll_maps_buffered_frames_onto_the_parser_snapshot() {
        let terminal_id = TerminalId::new();
        let terminal = RemoteTerminal::new(session(terminal_id.clone()));
        terminal.push_batch(batch(&terminal_id, &[(1, b"one"), (2, b"two")], 3, 0));

        let snapshot = terminal.poll_snapshot(1).expect("poll should succeed");
        assert_eq!(snapshot.session.id, terminal_id);
        assert_eq!(snapshot.next_sequence, 3);
        assert_eq!(snapshot.retained_bytes, 6);
        assert_eq!(snapshot.dropped_chunks, 0);
        assert_eq!(
            snapshot
                .chunks
                .iter()
                .map(|chunk| (chunk.sequence, chunk.data.clone()))
                .collect::<Vec<_>>(),
            vec![(1, b"one".to_vec()), (2, b"two".to_vec())]
        );

        let drained = terminal.poll_snapshot(3).expect("poll should succeed");
        assert!(drained.chunks.is_empty());
        assert_eq!(drained.next_sequence, 3);
        assert_eq!(drained.retained_bytes, 0);
    }

    #[test]
    fn remote_poll_splits_at_a_frame_gap_instead_of_mixing_sequences() {
        let terminal_id = TerminalId::new();
        let terminal = RemoteTerminal::new(session(terminal_id.clone()));
        terminal.push_batch(batch(&terminal_id, &[(1, b"one")], 2, 0));
        terminal.push_batch(batch(&terminal_id, &[(4, b"four")], 5, 1));

        let first = terminal.poll_snapshot(1).expect("poll should succeed");
        assert_eq!(first.next_sequence, 2);
        assert_eq!(first.chunks.len(), 1);
        assert_eq!(first.dropped_chunks, 1);

        let second = terminal.poll_snapshot(2).expect("poll should succeed");
        assert_eq!(second.next_sequence, 5);
        assert_eq!(
            second
                .chunks
                .iter()
                .map(|chunk| chunk.sequence)
                .collect::<Vec<_>>(),
            vec![4]
        );
    }

    #[test]
    fn remote_poll_discards_frames_the_parser_already_consumed() {
        let terminal_id = TerminalId::new();
        let terminal = RemoteTerminal::new(session(terminal_id.clone()));
        terminal.push_batch(batch(&terminal_id, &[(1, b"one"), (2, b"two")], 3, 0));

        let snapshot = terminal.poll_snapshot(3).expect("poll should succeed");
        assert!(snapshot.chunks.is_empty());
        assert_eq!(snapshot.retained_bytes, 0);
    }

    #[test]
    fn remote_poll_reports_an_attach_refusal_without_another_round_trip() {
        let terminal = RemoteTerminal::unavailable(
            TerminalId::new(),
            TerminalTransportError::new("terminal_not_found", "terminal session was not found"),
        );

        let error = terminal
            .poll_snapshot(1)
            .expect_err("unavailable terminal should keep reporting its error");
        assert_eq!(error.code, "terminal_not_found");
        assert_eq!(error.message, "terminal session was not found");
    }

    #[test]
    fn local_transport_keeps_the_manager_snapshot_error_contract() {
        let transport = LocalTerminalTransport::new(TerminalManager::new());
        let error = poll_once(transport.poll_terminal(&TerminalId::new(), 0))
            .expect("the in-process manager answers without awaiting")
            .expect_err("a non-positive cursor is rejected");
        assert_eq!(error.code, "terminal_raw_sequence_invalid");
        assert_eq!(
            error.message,
            "terminal raw snapshot sequence must be positive"
        );
    }
}
