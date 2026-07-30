use std::collections::{HashMap, VecDeque};
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use vibex_core::{
    TerminalCreateRequest, TerminalId, TerminalOutputChunk, TerminalResizeRequest, TerminalSession,
    TerminalSnapshot, TerminalStatus, TerminalSwitchShellRequest, TerminalWriteRequest, VibexError,
    VibexResult, WorkspaceId, unix_timestamp_ms,
};

mod emulator;
mod feasibility;

pub use emulator::*;
pub use feasibility::{TerminalFeasibilityRun, run_terminal_feasibility};

const MARKER: &str = "vibex-pty-smoke-ok";
const DEFAULT_RING_CAPACITY: usize = 2000;
const MIN_RAW_OBSERVATION_CAPACITY: usize = 4096;
const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?1;2c";
const PRIMARY_DEVICE_ATTRIBUTES_QUERIES: [&[u8]; 2] = [b"\x1b[c", b"\x1b[0c"];

type PtyWriter = Box<dyn Write + Send>;
type PtyMaster = Box<dyn MasterPty + Send>;
type PtyChild = Box<dyn Child + Send + Sync>;

#[derive(Clone)]
pub struct TerminalManager {
    sessions: Arc<Mutex<HashMap<TerminalId, TerminalRuntime>>>,
    ring_capacity: usize,
    raw_observation_capacity: Option<usize>,
}

struct TerminalRuntime {
    session: TerminalSession,
    master: PtyMaster,
    writer: Arc<Mutex<PtyWriter>>,
    child: PtyChild,
    buffer: Arc<Mutex<TerminalBuffer>>,
}

#[derive(Debug)]
struct TerminalBuffer {
    chunks: VecDeque<TerminalOutputChunk>,
    next_sequence: i64,
    capacity: usize,
    raw: Option<RawTerminalBuffer>,
}

#[derive(Debug)]
struct RawTerminalBuffer {
    chunks: VecDeque<TerminalRawOutputChunk>,
    next_sequence: i64,
    retained_bytes: usize,
    capacity_bytes: usize,
    dropped_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRawOutputChunk {
    pub sequence: i64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRawSnapshot {
    pub session: TerminalSession,
    pub chunks: Vec<TerminalRawOutputChunk>,
    pub next_sequence: i64,
    pub retained_bytes: usize,
    pub dropped_chunks: u64,
}

#[derive(Debug)]
pub struct TerminalShutdownReport {
    pub sessions: Vec<TerminalSession>,
    pub failures: Vec<VibexError>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self::with_ring_capacity(DEFAULT_RING_CAPACITY)
    }

    pub fn with_ring_capacity(ring_capacity: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ring_capacity: ring_capacity.max(1),
            raw_observation_capacity: None,
        }
    }

    pub fn with_raw_observation_capacity(
        ring_capacity: usize,
        raw_observation_capacity: usize,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ring_capacity: ring_capacity.max(1),
            raw_observation_capacity: Some(
                raw_observation_capacity.max(MIN_RAW_OBSERVATION_CAPACITY),
            ),
        }
    }

    pub fn raw_observation_capacity(&self) -> Option<usize> {
        self.raw_observation_capacity
    }

    pub fn list(&self, workspace_id: &WorkspaceId) -> VibexResult<Vec<TerminalSession>> {
        let mut sessions = self.lock_sessions()?;
        for runtime in sessions.values_mut() {
            refresh_exit_status(runtime)?;
        }
        Ok(sessions
            .values()
            .filter(|runtime| &runtime.session.workspace_id == workspace_id)
            .map(|runtime| runtime.session.clone())
            .collect())
    }

    pub fn create(
        &self,
        workspace_root: impl AsRef<Path>,
        request: TerminalCreateRequest,
    ) -> VibexResult<TerminalSession> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        let cwd = resolve_cwd(&workspace_root, request.cwd.as_deref())?;
        let rows = if request.rows == 0 { 24 } else { request.rows };
        let cols = if request.cols == 0 { 80 } else { request.cols };
        let shell = request
            .shell
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_shell);
        let title = request
            .title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| shell_title(&shell));

        let now = unix_timestamp_ms();
        let terminal_id = TerminalId::new();
        let session = TerminalSession {
            id: terminal_id.clone(),
            workspace_id: request.workspace_id,
            title,
            shell,
            cwd: cwd.to_string_lossy().to_string(),
            rows,
            cols,
            status: TerminalStatus::Running,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        self.spawn_runtime(cwd, session)
    }

    pub fn restore(
        &self,
        workspace_root: impl AsRef<Path>,
        mut session: TerminalSession,
    ) -> VibexResult<TerminalSession> {
        {
            let mut sessions = self.lock_sessions()?;
            if let Some(runtime) = sessions.get_mut(&session.id) {
                refresh_exit_status(runtime)?;
                if runtime.session.status == TerminalStatus::Running {
                    return Ok(runtime.session.clone());
                }
            }
            sessions.remove(&session.id);
        }

        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        let cwd = restore_cwd(&workspace_root, &session.cwd);
        let shell = if session.shell.trim().is_empty() {
            default_shell()
        } else {
            session.shell
        };
        let title = if session.title.trim().is_empty() {
            shell_title(&shell)
        } else {
            session.title
        };
        let now = unix_timestamp_ms();
        session.title = title;
        session.shell = shell;
        session.cwd = cwd.to_string_lossy().to_string();
        session.rows = if session.rows == 0 { 24 } else { session.rows };
        session.cols = if session.cols == 0 { 80 } else { session.cols };
        session.status = TerminalStatus::Running;
        session.updated_at_ms = now;
        session.closed_at_ms = None;
        self.spawn_runtime(cwd, session)
    }

    fn spawn_runtime(
        &self,
        cwd: PathBuf,
        session: TerminalSession,
    ) -> VibexResult<TerminalSession> {
        let shell = session.shell.clone();
        let terminal_id = session.id.clone();
        let rows = session.rows;
        let cols = session.cols;
        let buffer = Arc::new(Mutex::new(TerminalBuffer {
            chunks: VecDeque::new(),
            next_sequence: 1,
            capacity: self.ring_capacity,
            raw: self
                .raw_observation_capacity
                .map(|capacity_bytes| RawTerminalBuffer {
                    chunks: VecDeque::new(),
                    next_sequence: 1,
                    retained_bytes: 0,
                    capacity_bytes,
                    dropped_chunks: 0,
                }),
        }));
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| {
                VibexError::process("pty_open_failed", "failed to open PTY")
                    .with_diagnostic("error", err.to_string())
            })?;

        let mut command = CommandBuilder::new(&shell);
        command.cwd(cwd.as_os_str());
        sanitize_terminal_environment(&mut command);
        let child = pair.slave.spawn_command(command).map_err(|err| {
            VibexError::process("pty_spawn_failed", "failed to spawn PTY shell")
                .with_diagnostic("shell", shell)
                .with_diagnostic("cwd", cwd.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        let reader = pair.master.try_clone_reader().map_err(|err| {
            VibexError::process("pty_reader_failed", "failed to clone PTY reader")
                .with_diagnostic("error", err.to_string())
        })?;
        let writer = pair.master.take_writer().map_err(|err| {
            VibexError::process("pty_writer_failed", "failed to take PTY writer")
                .with_diagnostic("error", err.to_string())
        })?;
        let writer = Arc::new(Mutex::new(writer));
        spawn_reader_thread(terminal_id.clone(), reader, writer.clone(), buffer.clone());
        let runtime = TerminalRuntime {
            session: session.clone(),
            master: pair.master,
            writer,
            child,
            buffer,
        };
        self.lock_sessions()?.insert(terminal_id, runtime);
        Ok(session)
    }

    pub fn snapshot(&self, terminal_id: &TerminalId) -> VibexResult<TerminalSnapshot> {
        let mut sessions = self.lock_sessions()?;
        let runtime = sessions.get_mut(terminal_id).ok_or_else(|| {
            VibexError::validation("terminal_not_found", "terminal session was not found")
        })?;
        refresh_exit_status(runtime)?;
        let buffer = runtime.buffer.lock().map_err(|_| {
            VibexError::process(
                "terminal_buffer_poisoned",
                "terminal output buffer lock failed",
            )
        })?;
        Ok(TerminalSnapshot {
            session: runtime.session.clone(),
            chunks: buffer.chunks.iter().cloned().collect(),
            next_sequence: buffer.next_sequence,
        })
    }

    pub fn raw_snapshot(&self, terminal_id: &TerminalId) -> VibexResult<TerminalRawSnapshot> {
        self.raw_snapshot_from(terminal_id, 1)
    }

    /// Returns only raw chunks at or after `next_sequence`. If the requested
    /// sequence was evicted or belongs to an older runtime, the retained ring
    /// is returned so the consumer can rebuild safely.
    pub fn raw_snapshot_from(
        &self,
        terminal_id: &TerminalId,
        next_sequence: i64,
    ) -> VibexResult<TerminalRawSnapshot> {
        if next_sequence < 1 {
            return Err(VibexError::validation(
                "terminal_raw_sequence_invalid",
                "terminal raw snapshot sequence must be positive",
            ));
        }
        let mut sessions = self.lock_sessions()?;
        let runtime = sessions.get_mut(terminal_id).ok_or_else(|| {
            VibexError::validation("terminal_not_found", "terminal session was not found")
        })?;
        refresh_exit_status(runtime)?;
        let buffer = runtime.buffer.lock().map_err(|_| {
            VibexError::process(
                "terminal_buffer_poisoned",
                "terminal output buffer lock failed",
            )
        })?;
        let raw = buffer.raw.as_ref().ok_or_else(|| {
            VibexError::capability(
                "terminal_raw_observation_disabled",
                "raw terminal output observation is disabled",
            )
        })?;
        let (chunks, retained_bytes) = raw_chunks_from(raw, next_sequence);
        Ok(TerminalRawSnapshot {
            session: runtime.session.clone(),
            chunks,
            next_sequence: raw.next_sequence,
            retained_bytes,
            dropped_chunks: raw.dropped_chunks,
        })
    }

    pub fn write(&self, request: &TerminalWriteRequest) -> VibexResult<()> {
        self.write_bytes(&request.terminal_id, request.data.as_bytes())
    }

    pub fn write_bytes(&self, terminal_id: &TerminalId, data: &[u8]) -> VibexResult<()> {
        let sessions = self.lock_sessions()?;
        let runtime = sessions.get(terminal_id).ok_or_else(|| {
            VibexError::validation("terminal_not_found", "terminal session was not found")
        })?;
        if runtime.session.status != TerminalStatus::Running {
            return Err(VibexError::conflict(
                "terminal_not_running",
                "terminal session is not running",
            ));
        }
        let mut writer = runtime.writer.lock().map_err(|_| {
            VibexError::process("terminal_writer_poisoned", "terminal writer lock failed")
        })?;
        writer.write_all(data).map_err(|err| {
            VibexError::process("terminal_write_failed", "failed to write to terminal")
                .with_diagnostic("error", err.to_string())
        })?;
        writer.flush().map_err(|err| {
            VibexError::process("terminal_flush_failed", "failed to flush terminal input")
                .with_diagnostic("error", err.to_string())
        })?;
        Ok(())
    }

    pub fn resize(&self, request: &TerminalResizeRequest) -> VibexResult<TerminalSession> {
        let mut sessions = self.lock_sessions()?;
        let runtime = sessions.get_mut(&request.terminal_id).ok_or_else(|| {
            VibexError::validation("terminal_not_found", "terminal session was not found")
        })?;
        let rows = request.rows.max(1);
        let cols = request.cols.max(1);
        runtime
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| {
                VibexError::process("pty_resize_failed", "failed to resize PTY")
                    .with_diagnostic("error", err.to_string())
            })?;
        runtime.session.rows = rows;
        runtime.session.cols = cols;
        runtime.session.updated_at_ms = unix_timestamp_ms();
        Ok(runtime.session.clone())
    }

    pub fn switch_shell(
        &self,
        request: &TerminalSwitchShellRequest,
    ) -> VibexResult<TerminalSession> {
        let shell = request.shell.trim();
        if shell.is_empty() {
            return Err(VibexError::validation(
                "terminal_shell_missing",
                "terminal shell is required",
            ));
        }

        let mut runtime = {
            let mut sessions = self.lock_sessions()?;
            sessions.remove(&request.terminal_id).ok_or_else(|| {
                VibexError::validation("terminal_not_found", "terminal session was not found")
            })?
        };

        refresh_exit_status(&mut runtime)?;
        if runtime.session.status == TerminalStatus::Running
            && let Err(err) = runtime.child.kill()
        {
            self.lock_sessions()?
                .insert(request.terminal_id.clone(), runtime);
            return Err(VibexError::process(
                "terminal_shell_switch_kill_failed",
                "failed to stop existing terminal process",
            )
            .with_diagnostic("error", err.to_string()));
        }

        let cwd = PathBuf::from(&runtime.session.cwd)
            .canonicalize()
            .map_err(|err| {
                VibexError::validation("terminal_cwd_missing", "terminal cwd does not exist")
                    .with_diagnostic("path", runtime.session.cwd.clone())
                    .with_diagnostic("error", err.to_string())
            })?;
        if !cwd.is_dir() {
            return Err(VibexError::validation(
                "terminal_cwd_not_directory",
                "terminal cwd must be a directory",
            ));
        }

        let mut session = runtime.session;
        session.shell = shell.to_string();
        session.status = TerminalStatus::Running;
        session.updated_at_ms = unix_timestamp_ms();
        session.closed_at_ms = None;
        self.spawn_runtime(cwd, session)
    }

    pub fn kill(&self, terminal_id: &TerminalId) -> VibexResult<TerminalSession> {
        let mut sessions = self.lock_sessions()?;
        let mut runtime = sessions.remove(terminal_id).ok_or_else(|| {
            VibexError::validation("terminal_not_found", "terminal session was not found")
        })?;
        refresh_exit_status(&mut runtime)?;
        if runtime.session.status == TerminalStatus::Running
            && let Err(err) = runtime.child.kill()
        {
            sessions.insert(terminal_id.clone(), runtime);
            return Err(VibexError::process(
                "terminal_kill_failed",
                "failed to kill terminal process",
            )
            .with_diagnostic("error", err.to_string()));
        }
        runtime.session.status = TerminalStatus::Killed;
        runtime.session.updated_at_ms = unix_timestamp_ms();
        runtime.session.closed_at_ms = Some(runtime.session.updated_at_ms);
        Ok(runtime.session.clone())
    }

    /// Stops every owned PTY and returns the final session snapshots for
    /// persistence by the desktop composition root.
    pub fn shutdown_all(&self) -> VibexResult<TerminalShutdownReport> {
        let mut sessions = self.lock_sessions()?;
        let mut stopped = Vec::with_capacity(sessions.len());
        let mut failures = Vec::new();
        let runtimes = std::mem::take(&mut *sessions);
        for (terminal_id, mut runtime) in runtimes {
            if runtime.session.status == TerminalStatus::Running
                && let Err(error) = runtime.child.kill()
            {
                failures.push(
                    VibexError::process(
                        "terminal_shutdown_failed",
                        "failed to stop a terminal during desktop shutdown",
                    )
                    .with_diagnostic("terminalId", terminal_id.to_string())
                    .with_diagnostic("error", error.to_string()),
                );
                sessions.insert(terminal_id, runtime);
                continue;
            }
            runtime.session.status = TerminalStatus::Killed;
            runtime.session.updated_at_ms = unix_timestamp_ms();
            runtime.session.closed_at_ms = Some(runtime.session.updated_at_ms);
            stopped.push(runtime.session);
        }
        stopped.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(TerminalShutdownReport {
            sessions: stopped,
            failures,
        })
    }

    fn lock_sessions(
        &self,
    ) -> VibexResult<std::sync::MutexGuard<'_, HashMap<TerminalId, TerminalRuntime>>> {
        self.sessions.lock().map_err(|_| {
            VibexError::process("terminal_manager_poisoned", "terminal manager lock failed")
        })
    }
}

fn raw_chunks_from(
    raw: &RawTerminalBuffer,
    requested_sequence: i64,
) -> (Vec<TerminalRawOutputChunk>, usize) {
    let rebuild = requested_sequence > raw.next_sequence
        || raw
            .chunks
            .front()
            .is_some_and(|chunk| chunk.sequence > requested_sequence);
    let chunks = raw
        .chunks
        .iter()
        .filter(|chunk| rebuild || chunk.sequence >= requested_sequence)
        .cloned()
        .collect::<Vec<_>>();
    let retained_bytes = chunks.iter().map(|chunk| chunk.data.len()).sum();
    (chunks, retained_bytes)
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn_reader_thread(
    terminal_id: TerminalId,
    mut reader: Box<dyn Read + Send>,
    writer: Arc<Mutex<PtyWriter>>,
    buffer: Arc<Mutex<TerminalBuffer>>,
) {
    thread::spawn(move || {
        let mut bytes = [0_u8; 4096];
        let mut compatibility_responder = TerminalCompatibilityResponder::default();
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    push_raw_output(&buffer, &bytes[..count]);
                    let output = match writer.lock() {
                        Ok(mut writer) => {
                            compatibility_responder.filter(&bytes[..count], writer.as_mut())
                        }
                        Err(_) => break,
                    };
                    push_output_chunk(&terminal_id, &buffer, output);
                }
                Err(_) => break,
            }
        }
        push_output_chunk(&terminal_id, &buffer, compatibility_responder.flush());
    });
}

fn push_raw_output(buffer: &Arc<Mutex<TerminalBuffer>>, output: &[u8]) {
    if output.is_empty() {
        return;
    }
    let Ok(mut buffer) = buffer.lock() else {
        return;
    };
    let Some(raw) = buffer.raw.as_mut() else {
        return;
    };
    if output.len() > raw.capacity_bytes {
        raw.dropped_chunks += 1;
        return;
    }
    while raw.retained_bytes.saturating_add(output.len()) > raw.capacity_bytes {
        let Some(dropped) = raw.chunks.pop_front() else {
            break;
        };
        raw.retained_bytes = raw.retained_bytes.saturating_sub(dropped.data.len());
        raw.dropped_chunks += 1;
    }
    let sequence = raw.next_sequence;
    raw.next_sequence += 1;
    raw.retained_bytes += output.len();
    raw.chunks.push_back(TerminalRawOutputChunk {
        sequence,
        data: output.to_vec(),
    });
}

fn push_output_chunk(
    terminal_id: &TerminalId,
    buffer: &Arc<Mutex<TerminalBuffer>>,
    output: Vec<u8>,
) {
    if output.is_empty() {
        return;
    }

    let data = String::from_utf8_lossy(&output).to_string();
    if let Ok(mut buffer) = buffer.lock() {
        let sequence = buffer.next_sequence;
        buffer.next_sequence += 1;
        buffer.chunks.push_back(TerminalOutputChunk {
            terminal_id: terminal_id.clone(),
            sequence,
            data,
            timestamp_ms: unix_timestamp_ms(),
        });
        while buffer.chunks.len() > buffer.capacity {
            buffer.chunks.pop_front();
        }
    }
}

#[derive(Default)]
struct TerminalCompatibilityResponder {
    pending: Vec<u8>,
}

impl TerminalCompatibilityResponder {
    fn filter(&mut self, input: &[u8], writer: &mut dyn Write) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pending.len() + input.len());
        bytes.append(&mut self.pending);
        bytes.extend_from_slice(input);

        let mut output = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            let remaining = &bytes[index..];
            if let Some(query_length) = primary_device_attributes_query_length(remaining) {
                let _ = writer.write_all(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
                let _ = writer.flush();
                index += query_length;
                continue;
            }

            if is_primary_device_attributes_query_prefix(remaining) {
                self.pending.extend_from_slice(remaining);
                break;
            }

            output.push(bytes[index]);
            index += 1;
        }

        output
    }

    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

fn primary_device_attributes_query_length(bytes: &[u8]) -> Option<usize> {
    PRIMARY_DEVICE_ATTRIBUTES_QUERIES
        .iter()
        .find(|query| bytes.starts_with(query))
        .map(|query| query.len())
}

fn is_primary_device_attributes_query_prefix(bytes: &[u8]) -> bool {
    PRIMARY_DEVICE_ATTRIBUTES_QUERIES
        .iter()
        .any(|query| bytes.len() < query.len() && query.starts_with(bytes))
}

fn refresh_exit_status(runtime: &mut TerminalRuntime) -> VibexResult<()> {
    if runtime.session.status != TerminalStatus::Running {
        return Ok(());
    }
    match runtime.child.try_wait().map_err(|err| {
        VibexError::process("terminal_wait_failed", "failed to inspect terminal process")
            .with_diagnostic("error", err.to_string())
    })? {
        Some(_) => {
            runtime.session.status = TerminalStatus::Exited;
            runtime.session.updated_at_ms = unix_timestamp_ms();
            runtime.session.closed_at_ms = Some(runtime.session.updated_at_ms);
        }
        None => {}
    }
    Ok(())
}

fn canonical_workspace_root(root: &Path) -> VibexResult<PathBuf> {
    if !root.exists() || !root.is_dir() {
        return Err(VibexError::validation(
            "workspace_root_missing",
            "workspace root does not exist",
        )
        .with_diagnostic("path", root.display().to_string()));
    }
    root.canonicalize().map_err(|err| {
        VibexError::storage(
            "workspace_root_canonicalize_failed",
            "failed to resolve workspace root",
        )
        .with_diagnostic("path", root.display().to_string())
        .with_diagnostic("error", err.to_string())
    })
}

fn resolve_cwd(workspace_root: &Path, cwd: Option<&str>) -> VibexResult<PathBuf> {
    let candidate = match cwd {
        Some(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                workspace_root.join(path)
            }
        }
        _ => workspace_root.to_path_buf(),
    };
    let candidate = candidate.canonicalize().map_err(|err| {
        VibexError::validation("terminal_cwd_missing", "terminal cwd does not exist")
            .with_diagnostic("path", candidate.display().to_string())
            .with_diagnostic("error", err.to_string())
    })?;
    if !candidate.starts_with(workspace_root) {
        return Err(VibexError::validation(
            "terminal_cwd_outside_workspace",
            "terminal cwd must stay inside the workspace root",
        )
        .with_diagnostic("cwd", candidate.display().to_string())
        .with_diagnostic("root", workspace_root.display().to_string()));
    }
    if !candidate.is_dir() {
        return Err(VibexError::validation(
            "terminal_cwd_not_directory",
            "terminal cwd must be a directory",
        ));
    }
    Ok(candidate)
}

fn restore_cwd(workspace_root: &Path, cwd: &str) -> PathBuf {
    resolve_cwd(workspace_root, Some(cwd)).unwrap_or_else(|_| workspace_root.to_path_buf())
}

fn default_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[cfg(target_os = "linux")]
fn sanitize_terminal_environment(command: &mut CommandBuilder) {
    let Some(app_dir) = command.get_env("APPDIR").map(PathBuf::from) else {
        return;
    };
    if !app_dir.is_absolute() {
        return;
    }

    // linuxdeploy/AppImage adds package-private runtime paths and Python
    // overrides to the application process. They are required by the desktop
    // binary, but leaking them into an interactive PTY makes host programs use
    // the AppImage runtime (for example Python then cannot import `encodings`).
    for key in [
        "PATH",
        "LD_LIBRARY_PATH",
        "GSETTINGS_SCHEMA_DIR",
        "GST_PLUGIN_SYSTEM_PATH",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "PERLLIB",
        "PYTHONHOME",
        "PYTHONPATH",
        "QT_PLUGIN_PATH",
        "XDG_DATA_DIRS",
    ] {
        filter_app_dir_paths(command, key, &app_dir);
    }

    for key in [
        "APPDIR",
        "APPIMAGE",
        "ARGV0",
        "OWD",
        "PYTHONDONTWRITEBYTECODE",
    ] {
        command.env_remove(key);
    }
}

#[cfg(not(target_os = "linux"))]
fn sanitize_terminal_environment(_: &mut CommandBuilder) {}

#[cfg(target_os = "linux")]
fn filter_app_dir_paths(command: &mut CommandBuilder, key: &str, app_dir: &Path) {
    let Some(value) = command.get_env(key).map(OsStr::to_os_string) else {
        return;
    };
    let paths = std::env::split_paths(&value).collect::<Vec<_>>();
    let retained = paths
        .iter()
        .filter(|path| !path.starts_with(app_dir))
        .cloned()
        .collect::<Vec<_>>();
    if retained.len() == paths.len() {
        return;
    }
    match std::env::join_paths(retained) {
        Ok(value) if !value.is_empty() => command.env(key, value),
        _ => command.env_remove(key),
    }
}

fn shell_title(shell: &str) -> String {
    Path::new(shell)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(shell)
        .to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtySmokeResult {
    pub command: String,
    pub output: String,
    pub resized: bool,
    pub marker_seen: bool,
}

pub fn run_pty_smoke() -> VibexResult<PtySmokeResult> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| {
            VibexError::process("pty_open_failed", "failed to open PTY")
                .with_diagnostic("error", err.to_string())
        })?;

    let command = smoke_command();
    let command_label = command_label();
    let mut child = pair.slave.spawn_command(command).map_err(|err| {
        VibexError::process("pty_spawn_failed", "failed to spawn PTY command")
            .with_diagnostic("error", err.to_string())
    })?;

    pair.master
        .resize(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| {
            VibexError::process("pty_resize_failed", "failed to resize PTY")
                .with_diagnostic("error", err.to_string())
        })?;

    let mut reader = pair.master.try_clone_reader().map_err(|err| {
        VibexError::process("pty_reader_failed", "failed to clone PTY reader")
            .with_diagnostic("error", err.to_string())
    })?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut output = String::new();
        let mut buf = [0_u8; 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..count]));
                    if output.contains(MARKER) {
                        let _ = tx.send(output);
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(output);
    });

    child.wait().map_err(|err| {
        VibexError::process("pty_wait_failed", "failed waiting for PTY command")
            .with_diagnostic("error", err.to_string())
    })?;

    let output = rx.recv_timeout(Duration::from_secs(5)).map_err(|err| {
        VibexError::process("pty_read_timeout", "timed out reading PTY output")
            .with_diagnostic("error", err.to_string())
    })?;
    let marker_seen = output.contains(MARKER);
    if !marker_seen {
        return Err(VibexError::process(
            "pty_marker_missing",
            "PTY command completed but expected marker was not captured",
        )
        .with_diagnostic("output", output.trim()));
    }

    Ok(PtySmokeResult {
        command: command_label,
        output: output.trim().to_string(),
        resized: true,
        marker_seen,
    })
}

fn smoke_command() -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/C");
        cmd.arg(format!("echo {MARKER}"));
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-lc");
        cmd.arg(format!("printf {MARKER}"));
        cmd
    }
}

fn command_label() -> String {
    #[cfg(target_os = "windows")]
    {
        format!("cmd.exe /C echo {MARKER}")
    }

    #[cfg(not(target_os = "windows"))]
    {
        format!("/bin/sh -lc 'printf {MARKER}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_smoke_captures_marker() {
        let result = run_pty_smoke().unwrap();
        assert!(result.marker_seen);
        assert!(result.output.contains(MARKER));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_environment_removes_appimage_runtime_overrides() {
        let app_dir = Path::new("/tmp/.mount_vibex-test");
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("APPDIR", app_dir);
        command.env("APPIMAGE", "/tmp/vibex.AppImage");
        command.env(
            "PATH",
            "/tmp/.mount_vibex-test/usr/bin:/home/test/.local/bin:/usr/bin",
        );
        command.env(
            "LD_LIBRARY_PATH",
            "/tmp/.mount_vibex-test/usr/lib:/opt/host/lib",
        );
        command.env("PYTHONHOME", "/tmp/.mount_vibex-test/usr");
        command.env("PYTHONPATH", "/tmp/.mount_vibex-test/usr/share/pyshared");
        command.env("PYTHONDONTWRITEBYTECODE", "1");
        command.env("VIBEX_TEST_SENTINEL", "preserved");

        sanitize_terminal_environment(&mut command);

        assert_eq!(
            command.get_env("PATH"),
            Some(OsStr::new("/home/test/.local/bin:/usr/bin"))
        );
        assert_eq!(
            command.get_env("LD_LIBRARY_PATH"),
            Some(OsStr::new("/opt/host/lib"))
        );
        for key in [
            "APPDIR",
            "APPIMAGE",
            "PYTHONHOME",
            "PYTHONPATH",
            "PYTHONDONTWRITEBYTECODE",
        ] {
            assert_eq!(command.get_env(key), None, "{key} should not leak");
        }
        assert_eq!(
            command.get_env("VIBEX_TEST_SENTINEL"),
            Some(OsStr::new("preserved"))
        );
    }

    #[test]
    fn compatibility_responder_answers_primary_device_attributes() {
        let mut responder = TerminalCompatibilityResponder::default();
        let mut response = Vec::new();

        let output = responder.filter(b"before\x1b[cafter", &mut response);

        assert_eq!(output, b"beforeafter");
        assert_eq!(response, PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
        assert!(responder.flush().is_empty());
    }

    #[test]
    fn compatibility_responder_answers_split_primary_device_attributes() {
        let mut responder = TerminalCompatibilityResponder::default();
        let mut response = Vec::new();

        let first_output = responder.filter(b"before\x1b[", &mut response);
        let second_output = responder.filter(b"0cafter", &mut response);

        assert_eq!(first_output, b"before");
        assert_eq!(second_output, b"after");
        assert_eq!(response, PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
        assert!(responder.flush().is_empty());
    }

    #[test]
    fn compatibility_responder_preserves_other_escape_sequences() {
        let mut responder = TerminalCompatibilityResponder::default();
        let mut response = Vec::new();

        let first_output = responder.filter(b"\x1b[", &mut response);
        let second_output = responder.filter(b"?25h", &mut response);

        assert!(first_output.is_empty());
        assert_eq!(second_output, b"\x1b[?25h");
        assert!(response.is_empty());
        assert!(responder.flush().is_empty());
    }

    #[test]
    fn terminal_manager_captures_output_and_kills_session() {
        let manager = TerminalManager::new();
        let workspace_id = WorkspaceId::new();
        let session = manager
            .create(
                std::env::temp_dir(),
                TerminalCreateRequest {
                    workspace_id: workspace_id.clone(),
                    title: Some("test".to_string()),
                    shell: Some(default_shell()),
                    cwd: None,
                    rows: 24,
                    cols: 80,
                },
            )
            .unwrap();
        manager
            .write(&TerminalWriteRequest {
                terminal_id: session.id.clone(),
                data: format!("printf {MARKER}\\n"),
            })
            .unwrap();
        let mut marker_seen = false;
        for _ in 0..20 {
            let snapshot = manager.snapshot(&session.id).unwrap();
            marker_seen = snapshot
                .chunks
                .iter()
                .any(|chunk| chunk.data.contains(MARKER));
            if marker_seen {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(marker_seen);
        let resized = manager
            .resize(&TerminalResizeRequest {
                terminal_id: session.id.clone(),
                rows: 30,
                cols: 100,
            })
            .unwrap();
        assert_eq!(resized.rows, 30);
        let killed = manager.kill(&session.id).unwrap();
        assert_eq!(killed.status, TerminalStatus::Killed);
    }

    #[test]
    fn raw_observation_is_opt_in_and_preserves_non_utf8_bytes() {
        let buffer = Arc::new(Mutex::new(TerminalBuffer {
            chunks: VecDeque::new(),
            next_sequence: 1,
            capacity: 1,
            raw: Some(RawTerminalBuffer {
                chunks: VecDeque::new(),
                next_sequence: 1,
                retained_bytes: 0,
                capacity_bytes: MIN_RAW_OBSERVATION_CAPACITY,
                dropped_chunks: 0,
            }),
        }));

        push_raw_output(&buffer, b"valid\xff\xfe");

        let buffer = buffer.lock().unwrap();
        let raw = buffer.raw.as_ref().unwrap();
        assert_eq!(raw.retained_bytes, 7);
        assert_eq!(raw.chunks[0].data, b"valid\xff\xfe");
        assert_eq!(raw.dropped_chunks, 0);
    }

    #[test]
    fn raw_observation_evicts_whole_chunks_at_its_byte_bound() {
        let buffer = Arc::new(Mutex::new(TerminalBuffer {
            chunks: VecDeque::new(),
            next_sequence: 1,
            capacity: 1,
            raw: Some(RawTerminalBuffer {
                chunks: VecDeque::new(),
                next_sequence: 1,
                retained_bytes: 0,
                capacity_bytes: 6,
                dropped_chunks: 0,
            }),
        }));

        push_raw_output(&buffer, b"abcd");
        push_raw_output(&buffer, b"efgh");

        let buffer = buffer.lock().unwrap();
        let raw = buffer.raw.as_ref().unwrap();
        assert_eq!(raw.retained_bytes, 4);
        assert_eq!(raw.chunks.len(), 1);
        assert_eq!(raw.chunks[0].sequence, 2);
        assert_eq!(raw.dropped_chunks, 1);
    }

    #[test]
    fn raw_observation_rejects_a_chunk_larger_than_its_byte_bound() {
        let buffer = Arc::new(Mutex::new(TerminalBuffer {
            chunks: VecDeque::new(),
            next_sequence: 1,
            capacity: 1,
            raw: Some(RawTerminalBuffer {
                chunks: VecDeque::new(),
                next_sequence: 1,
                retained_bytes: 0,
                capacity_bytes: 4,
                dropped_chunks: 0,
            }),
        }));

        push_raw_output(&buffer, b"oversized");

        let buffer = buffer.lock().unwrap();
        let raw = buffer.raw.as_ref().unwrap();
        assert_eq!(raw.retained_bytes, 0);
        assert!(raw.chunks.is_empty());
        assert_eq!(raw.dropped_chunks, 1);
    }

    #[test]
    fn incremental_raw_snapshot_clones_only_unconsumed_chunks_and_preserves_rebuilds() {
        let raw = RawTerminalBuffer {
            chunks: VecDeque::from([
                TerminalRawOutputChunk {
                    sequence: 2,
                    data: b"old".to_vec(),
                },
                TerminalRawOutputChunk {
                    sequence: 3,
                    data: b"new".to_vec(),
                },
            ]),
            next_sequence: 4,
            retained_bytes: 6,
            capacity_bytes: MIN_RAW_OBSERVATION_CAPACITY,
            dropped_chunks: 1,
        };

        let (none, none_bytes) = raw_chunks_from(&raw, 4);
        assert!(none.is_empty());
        assert_eq!(none_bytes, 0);

        let (incremental, incremental_bytes) = raw_chunks_from(&raw, 3);
        assert_eq!(incremental.len(), 1);
        assert_eq!(incremental[0].sequence, 3);
        assert_eq!(incremental_bytes, 3);

        for missing_or_restarted in [1, 5] {
            let (rebuild, rebuild_bytes) = raw_chunks_from(&raw, missing_or_restarted);
            assert_eq!(rebuild.len(), 2);
            assert_eq!(rebuild_bytes, raw.retained_bytes);
        }
    }
}
