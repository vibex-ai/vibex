//! Shared, provider-neutral Terminal workflow state.
//!
//! The controller consumes raw byte frames from `TerminalBackend`.  It never
//! owns a PTY or a socket; those remain authoritative in the native/remote
//! backend.  The portable emulator is supplied by `vibex-terminal-ui`, which
//! is WASM-safe and is also re-exported by the native terminal crate.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendError, BackendErrorKind, BackendFuture, BackendOperation,
    BackendResult, DomainCapabilities, MutationRequest, TerminalBackend, TerminalFrame,
    TerminalFrameBatch, TerminalFrameSubscription,
};
use vibex_core::{
    TerminalCreateRequest, TerminalId, TerminalResizeRequest, TerminalSession, TerminalStatus,
    TerminalWriteRequest, WorkspaceId,
};
use vibex_terminal_ui::{TerminalEmulator, TerminalFrameSnapshot, TerminalModeSnapshot};

use crate::{HostViewportSnapshot, MIN_TOUCH_TARGET_PX, ShellKind};

pub const TERMINAL_WORKFLOW_SCHEMA_VERSION: &str = "vibex-terminal-workflow.v1";
pub const TERMINAL_RAW_FRAME_LIMIT: usize = 2_048;
pub const TERMINAL_RAW_BYTE_BUDGET: usize = 16 * 1024 * 1024;
pub const TERMINAL_MAX_INPUT_BYTES: usize = 64 * 1024;
pub const TERMINAL_DEFAULT_ROWS: u16 = 24;
pub const TERMINAL_DEFAULT_COLS: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalConnectionState {
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Rebuilding,
    Closed,
    Offline,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAccessMode {
    ReadOnly,
    ReadWrite,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalPresentation {
    Docked,
    Drawer,
    Fullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKey {
    Escape,
    Control,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Enter,
    Backspace,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKeyModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKeyBarAction {
    pub key: TerminalKey,
    pub label: String,
    pub touch_target_px: u16,
    pub hover_required: bool,
}

impl TerminalKeyBarAction {
    fn new(key: TerminalKey, label: &'static str) -> Self {
        Self {
            key,
            label: label.to_string(),
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        }
    }

    pub fn is_touch_discoverable(&self) -> bool {
        !self.hover_required && self.touch_target_px >= MIN_TOUCH_TARGET_PX
    }
}

pub fn compact_key_bar() -> Vec<TerminalKeyBarAction> {
    [
        (TerminalKey::Escape, "Esc"),
        (TerminalKey::Control, "Ctrl"),
        (TerminalKey::Tab, "Tab"),
        (TerminalKey::ArrowUp, "↑"),
        (TerminalKey::ArrowDown, "↓"),
        (TerminalKey::ArrowLeft, "←"),
        (TerminalKey::ArrowRight, "→"),
        (TerminalKey::Enter, "Enter"),
        (TerminalKey::Backspace, "⌫"),
    ]
    .into_iter()
    .map(|(key, label)| TerminalKeyBarAction::new(key, label))
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalViewport {
    pub width_px: u32,
    pub height_px: u32,
    pub visible_height_px: u32,
    pub keyboard_inset_px: u32,
    pub safe_bottom_px: u32,
}

impl TerminalViewport {
    pub fn from_host(snapshot: &HostViewportSnapshot) -> Self {
        let width_px = finite_dimension(snapshot.width);
        let height_px = finite_dimension(snapshot.height);
        let keyboard_inset_px = finite_dimension(snapshot.keyboard_inset);
        let safe_bottom_px = finite_dimension(snapshot.safe_area.bottom);
        let visible_height_px = height_px.saturating_sub(keyboard_inset_px.max(safe_bottom_px));
        Self {
            width_px,
            height_px,
            visible_height_px,
            keyboard_inset_px,
            safe_bottom_px,
        }
    }

    pub fn keeps_recent_output_visible(&self) -> bool {
        self.visible_height_px > 0 && self.visible_height_px <= self.height_px
    }
}

fn finite_dimension(value: f32) -> u32 {
    if value.is_finite() {
        value.max(0.0).round().min(u32::MAX as f32) as u32
    } else {
        0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TerminalRawBuffer {
    pub terminal_id: Option<TerminalId>,
    pub next_sequence: i64,
    pub dropped_frames: u64,
    pub retained_bytes: usize,
    frames: VecDeque<TerminalFrame>,
    frame_limit: usize,
    byte_budget: usize,
    pub rebuild_count: u64,
}

impl fmt::Debug for TerminalRawBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalRawBuffer")
            .field("terminal_id", &self.terminal_id)
            .field("next_sequence", &self.next_sequence)
            .field("dropped_frames", &self.dropped_frames)
            .field("retained_frame_count", &self.frames.len())
            .field("retained_bytes", &self.retained_bytes)
            .field("frame_limit", &self.frame_limit)
            .field("byte_budget", &self.byte_budget)
            .field("rebuild_count", &self.rebuild_count)
            .finish()
    }
}

impl Default for TerminalRawBuffer {
    fn default() -> Self {
        Self::new(TERMINAL_RAW_FRAME_LIMIT, TERMINAL_RAW_BYTE_BUDGET)
    }
}

impl TerminalRawBuffer {
    pub fn new(frame_limit: usize, byte_budget: usize) -> Self {
        Self {
            terminal_id: None,
            next_sequence: 1,
            dropped_frames: 0,
            retained_bytes: 0,
            frames: VecDeque::new(),
            frame_limit: frame_limit.max(1),
            byte_budget: byte_budget.max(1),
            rebuild_count: 0,
        }
    }

    pub fn frames(&self) -> impl Iterator<Item = &TerminalFrame> {
        self.frames.iter()
    }

    pub fn bytes(&self) -> impl Iterator<Item = &[u8]> {
        self.frames.iter().map(|frame| frame.bytes.as_slice())
    }

    pub fn clear(&mut self, terminal_id: TerminalId, next_sequence: i64) {
        self.terminal_id = Some(terminal_id);
        self.next_sequence = next_sequence.max(1);
        self.dropped_frames = 0;
        self.retained_bytes = 0;
        self.frames.clear();
    }

    pub fn apply_batch(
        &mut self,
        batch: &TerminalFrameBatch,
    ) -> BackendResult<TerminalApplyOutcome> {
        if batch.next_sequence < 1 || batch.frames.iter().any(|frame| frame.sequence < 1) {
            return Err(BackendError::failed(
                "terminal_frame_sequence_invalid",
                "terminal frame sequence must be positive",
            ));
        }
        if self
            .terminal_id
            .as_ref()
            .is_some_and(|terminal_id| terminal_id != &batch.terminal_id)
        {
            return Err(BackendError::conflict(
                "terminal_frame_target_mismatch",
                "terminal frame belongs to another terminal",
            ));
        }
        if self.terminal_id.is_none() {
            self.terminal_id = Some(batch.terminal_id.clone());
        }
        let first_sequence = batch.frames.first().map(|frame| frame.sequence);
        let dropped_advanced = batch.dropped_frames > self.dropped_frames;
        let rebuild = batch.reset_required
            || dropped_advanced
            || batch.next_sequence < self.next_sequence
            || first_sequence.is_some_and(|sequence| sequence > self.next_sequence);

        if rebuild {
            self.frames.clear();
            self.retained_bytes = 0;
            self.rebuild_count = self.rebuild_count.saturating_add(1);
            self.next_sequence = first_sequence.unwrap_or(batch.next_sequence).max(1);
        }

        let mut expected = self.next_sequence;
        let mut accepted_frames = 0usize;
        let mut accepted_bytes = 0usize;
        let mut previous = None;
        for frame in &batch.frames {
            if previous.is_some_and(|sequence| frame.sequence != sequence + 1) {
                return Err(BackendError::conflict(
                    "terminal_frame_batch_non_contiguous",
                    "terminal frame batch is not contiguous",
                ));
            }
            previous = Some(frame.sequence);
            if !rebuild && frame.sequence < expected {
                continue;
            }
            if frame.sequence != expected {
                return Err(BackendError::conflict(
                    "terminal_frame_sequence_gap",
                    "terminal frame batch contains a sequence gap",
                ));
            }
            expected = expected.saturating_add(1);
            accepted_frames = accepted_frames.saturating_add(1);
            accepted_bytes = accepted_bytes.saturating_add(frame.bytes.len());
            self.retained_bytes = self.retained_bytes.saturating_add(frame.bytes.len());
            self.frames.push_back(frame.clone());
        }

        if !rebuild && expected != batch.next_sequence {
            return Err(BackendError::conflict(
                "terminal_frame_batch_incomplete",
                "terminal frame batch does not reach its declared next sequence",
            ));
        }
        self.next_sequence = batch.next_sequence.max(expected).max(1);
        self.dropped_frames = self.dropped_frames.max(batch.dropped_frames);
        let mut evicted = 0usize;
        while self.frames.len() > self.frame_limit || self.retained_bytes > self.byte_budget {
            if let Some(frame) = self.frames.pop_front() {
                self.retained_bytes = self.retained_bytes.saturating_sub(frame.bytes.len());
                evicted = evicted.saturating_add(1);
            } else {
                break;
            }
        }
        Ok(TerminalApplyOutcome {
            accepted_frames,
            accepted_bytes,
            evicted_frames: evicted,
            rebuilt: rebuild,
            next_sequence: self.next_sequence,
            dropped_frames: self.dropped_frames,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalApplyOutcome {
    pub accepted_frames: usize,
    pub accepted_bytes: usize,
    pub evicted_frames: usize,
    pub rebuilt: bool,
    pub next_sequence: i64,
    pub dropped_frames: u64,
}

pub struct TerminalRenderModel {
    emulator: TerminalEmulator,
    pub frame: TerminalFrameSnapshot,
}

impl fmt::Debug for TerminalRenderModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalRenderModel")
            .field("rows", &self.frame.rows)
            .field("columns", &self.frame.columns)
            .field("history_lines", &self.frame.history_lines)
            .field("cell_count", &self.frame.cells.len())
            .field("ingested_bytes", &self.frame.ingested_bytes)
            .finish()
    }
}

impl TerminalRenderModel {
    pub fn new(rows: u16, columns: u16) -> Self {
        let mut emulator = TerminalEmulator::new(rows.max(1), columns.max(1));
        let frame = emulator.frame();
        Self { emulator, frame }
    }

    pub fn reset(&mut self, rows: u16, columns: u16) {
        self.emulator = TerminalEmulator::new(rows.max(1), columns.max(1));
        self.frame = self.emulator.frame();
    }

    pub fn apply(&mut self, bytes: &[u8]) {
        self.emulator.advance(bytes);
        self.frame = self.emulator.frame();
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        self.emulator.resize(rows.max(1), columns.max(1));
        self.frame = self.emulator.frame();
    }

    pub fn modes(&self) -> TerminalModeSnapshot {
        self.emulator.modes()
    }

    pub fn encode_input(&self, input: TerminalInput) -> Vec<u8> {
        match input {
            TerminalInput::Text(text) => text.replace('\0', "").into_bytes(),
            TerminalInput::Character(character) => character.to_string().into_bytes(),
            TerminalInput::Key(key, modifiers) => encode_key(key, modifiers, self.modes()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    Text(String),
    Character(char),
    Key(TerminalKey, TerminalKeyModifiers),
}

fn encode_key(
    key: TerminalKey,
    modifiers: TerminalKeyModifiers,
    modes: TerminalModeSnapshot,
) -> Vec<u8> {
    if key == TerminalKey::Control {
        return Vec::new();
    }
    let modifier = 1
        + usize::from(modifiers.shift)
        + usize::from(modifiers.alt) * 2
        + usize::from(modifiers.control) * 4;
    let modified = modifier != 1;
    let value = match key {
        TerminalKey::Escape => "\x1b".to_string(),
        TerminalKey::Enter => "\r".to_string(),
        TerminalKey::Backspace => {
            if modifiers.alt {
                "\x1b\x7f".to_string()
            } else {
                "\x7f".to_string()
            }
        }
        TerminalKey::Tab if modifiers.shift => "\x1b[Z".to_string(),
        TerminalKey::Tab => "\t".to_string(),
        TerminalKey::ArrowUp
        | TerminalKey::ArrowDown
        | TerminalKey::ArrowLeft
        | TerminalKey::ArrowRight => {
            let suffix = match key {
                TerminalKey::ArrowUp => 'A',
                TerminalKey::ArrowDown => 'B',
                TerminalKey::ArrowRight => 'C',
                TerminalKey::ArrowLeft => 'D',
                _ => unreachable!(),
            };
            if modified {
                format!("\x1b[1;{modifier}{suffix}")
            } else if modes.application_cursor {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[{suffix}")
            }
        }
        TerminalKey::Control => String::new(),
    };
    value.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSessionView {
    pub id: String,
    pub title: String,
    pub shell: String,
    pub status: TerminalStatus,
    pub rows: u16,
    pub cols: u16,
    pub selected: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TerminalWorkflowView {
    pub schema_version: &'static str,
    pub generation: u64,
    pub sessions: Vec<TerminalSessionView>,
    pub active_session_id: Option<String>,
    pub connection: TerminalConnectionState,
    pub access: TerminalAccessMode,
    pub presentation: TerminalPresentation,
    pub key_bar: Vec<TerminalKeyBarAction>,
    pub control_latched: bool,
    pub viewport: Option<TerminalViewport>,
    pub frame: Option<TerminalFrameSnapshot>,
    pub raw_next_sequence: i64,
    pub raw_retained_bytes: usize,
    pub rebuild_count: u64,
    pub last_error: Option<BackendError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRefreshTicket {
    pub generation: u64,
    pub workspace_id: WorkspaceId,
}

pub struct TerminalPollTask {
    generation: u64,
    subscription: Box<dyn TerminalFrameSubscription>,
}

impl TerminalPollTask {
    pub async fn next(
        mut self,
    ) -> (
        u64,
        Box<dyn TerminalFrameSubscription>,
        BackendResult<Option<TerminalFrameBatch>>,
    ) {
        let result = self.subscription.next().await;
        (self.generation, self.subscription, result)
    }
}

#[derive(Clone)]
pub struct TerminalCreateOperation {
    generation: u64,
    workspace_id: WorkspaceId,
    request: MutationRequest<TerminalCreateRequest>,
}

impl fmt::Debug for TerminalCreateOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalCreateOperation")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("request_id", &self.request.request_id)
            .finish()
    }
}

#[derive(Clone)]
pub struct TerminalInputOperation {
    generation: u64,
    terminal_id: TerminalId,
    request: MutationRequest<TerminalWriteRequest>,
}

impl fmt::Debug for TerminalInputOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalInputOperation")
            .field("generation", &self.generation)
            .field("terminal_id", &self.terminal_id)
            .field("request_id", &self.request.request_id)
            .field("input_bytes", &self.request.payload.data.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct TerminalResizeOperation {
    generation: u64,
    terminal_id: TerminalId,
    request: MutationRequest<TerminalResizeRequest>,
}

impl fmt::Debug for TerminalResizeOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalResizeOperation")
            .field("generation", &self.generation)
            .field("terminal_id", &self.terminal_id)
            .field("request_id", &self.request.request_id)
            .field("rows", &self.request.payload.rows)
            .field("cols", &self.request.payload.cols)
            .finish()
    }
}

#[derive(Clone)]
pub struct TerminalCloseOperation {
    generation: u64,
    terminal_id: TerminalId,
    request: MutationRequest<TerminalId>,
}

impl fmt::Debug for TerminalCloseOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalCloseOperation")
            .field("generation", &self.generation)
            .field("terminal_id", &self.terminal_id)
            .field("request_id", &self.request.request_id)
            .finish()
    }
}

impl fmt::Debug for TerminalWorkflowView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalWorkflowView")
            .field("schema_version", &self.schema_version)
            .field("generation", &self.generation)
            .field("session_count", &self.sessions.len())
            .field("active_session_id", &self.active_session_id)
            .field("connection", &self.connection)
            .field("access", &self.access)
            .field("presentation", &self.presentation)
            .field("key_bar_count", &self.key_bar.len())
            .field("control_latched", &self.control_latched)
            .field("viewport", &self.viewport)
            .field("has_frame", &self.frame.is_some())
            .field("raw_next_sequence", &self.raw_next_sequence)
            .field("raw_retained_bytes", &self.raw_retained_bytes)
            .field("rebuild_count", &self.rebuild_count)
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct TerminalWorkflowCapabilities {
    pub schema_version: String,
    pub backend_revision: u64,
    pub domain: DomainCapabilities,
}

impl fmt::Debug for TerminalWorkflowCapabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalWorkflowCapabilities")
            .field("schema_version", &self.schema_version)
            .field("backend_revision", &self.backend_revision)
            .field("availability", &self.domain.availability)
            .field("operation_count", &self.domain.operations.len())
            .finish()
    }
}

impl TerminalWorkflowCapabilities {
    pub fn from_backend(snapshot: &BackendCapabilitySnapshot) -> Self {
        use BackendOperation::*;
        let allowed = [
            TerminalList,
            TerminalCreate,
            TerminalAttach,
            TerminalInput,
            TerminalResize,
            TerminalClose,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        Self {
            schema_version: TERMINAL_WORKFLOW_SCHEMA_VERSION.to_string(),
            backend_revision: snapshot.revision,
            domain: DomainCapabilities {
                availability: snapshot.terminal.availability,
                operations: snapshot
                    .terminal
                    .operations
                    .intersection(&allowed)
                    .copied()
                    .collect(),
            },
        }
    }

    pub fn supports(&self, operation: BackendOperation) -> bool {
        self.domain.supports(operation)
    }

    pub fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        if self.supports(operation) {
            return Ok(());
        }
        let label = terminal_operation_label(operation);
        let error = match self.domain.availability {
            vibex_backend::CapabilityAvailability::Offline => BackendError::offline(
                format!("{label}_offline"),
                "the authoritative terminal backend is offline",
            ),
            vibex_backend::CapabilityAvailability::RequiresPermission => BackendError::permission(
                format!("{label}_permission_required"),
                "terminal permission is required by the paired device",
            ),
            vibex_backend::CapabilityAvailability::Degraded => BackendError::loading(
                format!("{label}_degraded"),
                "terminal service is temporarily degraded",
            ),
            vibex_backend::CapabilityAvailability::Available
            | vibex_backend::CapabilityAvailability::Unsupported => BackendError::unsupported(
                format!("{label}_unsupported"),
                "the requested terminal operation is unavailable",
            ),
        };
        Err(error)
    }
}

fn terminal_operation_label(operation: BackendOperation) -> &'static str {
    use BackendOperation::*;
    match operation {
        TerminalList => "terminal_list",
        TerminalCreate => "terminal_create",
        TerminalAttach => "terminal_attach",
        TerminalInput => "terminal_input",
        TerminalResize => "terminal_resize",
        TerminalClose => "terminal_close",
        _ => "terminal_operation",
    }
}

pub struct TerminalWorkflowState {
    pub generation: u64,
    pub workspace_id: Option<WorkspaceId>,
    pub sessions: Vec<TerminalSession>,
    pub active_session: Option<TerminalSession>,
    pub connection: TerminalConnectionState,
    pub access: TerminalAccessMode,
    pub raw: TerminalRawBuffer,
    pub render: Option<TerminalRenderModel>,
    pub key_bar: Vec<TerminalKeyBarAction>,
    pub control_latched: bool,
    pub viewport: Option<TerminalViewport>,
    pub last_error: Option<BackendError>,
}

impl fmt::Debug for TerminalWorkflowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalWorkflowState")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("session_count", &self.sessions.len())
            .field("has_active_session", &self.active_session.is_some())
            .field("connection", &self.connection)
            .field("access", &self.access)
            .field("raw", &self.raw)
            .field("has_render", &self.render.is_some())
            .field("key_bar_count", &self.key_bar.len())
            .field("control_latched", &self.control_latched)
            .field("viewport", &self.viewport)
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .finish()
    }
}

impl Default for TerminalWorkflowState {
    fn default() -> Self {
        Self {
            generation: 0,
            workspace_id: None,
            sessions: Vec::new(),
            active_session: None,
            connection: TerminalConnectionState::Idle,
            access: TerminalAccessMode::Unknown,
            raw: TerminalRawBuffer::default(),
            render: None,
            key_bar: compact_key_bar(),
            control_latched: false,
            viewport: None,
            last_error: None,
        }
    }
}

impl TerminalWorkflowState {
    pub fn advance_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1).max(1);
        self.generation
    }

    pub fn view(&self, shell: ShellKind) -> TerminalWorkflowView {
        let active_id = self
            .active_session
            .as_ref()
            .map(|session| session.id.clone());
        TerminalWorkflowView {
            schema_version: TERMINAL_WORKFLOW_SCHEMA_VERSION,
            generation: self.generation,
            sessions: self
                .sessions
                .iter()
                .map(|session| TerminalSessionView {
                    id: session.id.as_str().to_string(),
                    title: session.title.clone(),
                    shell: session.shell.clone(),
                    status: session.status,
                    rows: session.rows,
                    cols: session.cols,
                    selected: active_id.as_ref() == Some(&session.id),
                })
                .collect(),
            active_session_id: active_id.map(|id| id.as_str().to_string()),
            connection: self.connection,
            access: self.access,
            presentation: match shell {
                ShellKind::Wide => TerminalPresentation::Docked,
                ShellKind::Medium => TerminalPresentation::Drawer,
                ShellKind::Compact => TerminalPresentation::Fullscreen,
            },
            key_bar: if shell == ShellKind::Compact {
                self.key_bar.clone()
            } else {
                Vec::new()
            },
            control_latched: self.control_latched,
            viewport: self.viewport,
            frame: self.render.as_ref().map(|render| render.frame.clone()),
            raw_next_sequence: self.raw.next_sequence,
            raw_retained_bytes: self.raw.retained_bytes,
            rebuild_count: self.raw.rebuild_count,
            last_error: self.last_error.clone(),
        }
    }
}

pub struct TerminalWorkflowController {
    backend: Arc<dyn TerminalBackend>,
    pub capabilities: TerminalWorkflowCapabilities,
    pub state: TerminalWorkflowState,
    subscription: Option<Box<dyn TerminalFrameSubscription>>,
    subscription_generation: u64,
}

impl TerminalWorkflowController {
    pub fn new(
        backend: Arc<dyn TerminalBackend>,
        capabilities: TerminalWorkflowCapabilities,
    ) -> Self {
        Self {
            backend,
            capabilities,
            state: TerminalWorkflowState::default(),
            subscription: None,
            subscription_generation: 0,
        }
    }

    pub fn set_capabilities(&mut self, capabilities: TerminalWorkflowCapabilities) {
        self.capabilities = capabilities;
    }

    pub fn begin_refresh(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> BackendResult<TerminalRefreshTicket> {
        self.capabilities.require(BackendOperation::TerminalList)?;
        let generation = self.state.advance_generation();
        self.subscription = None;
        self.state.workspace_id = Some(workspace_id.clone());
        self.state.active_session = None;
        self.state.raw = TerminalRawBuffer::default();
        self.state.render = None;
        self.state.connection = TerminalConnectionState::Connecting;
        self.state.last_error = None;
        Ok(TerminalRefreshTicket {
            generation,
            workspace_id,
        })
    }

    pub fn load_sessions(
        &self,
        ticket: TerminalRefreshTicket,
    ) -> BackendFuture<'static, Vec<TerminalSession>> {
        if let Err(error) = self.capabilities.require(BackendOperation::TerminalList) {
            return Box::pin(async move { Err(error) });
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.list_terminals(ticket.workspace_id).await })
    }

    pub fn apply_refresh(
        &mut self,
        ticket: &TerminalRefreshTicket,
        result: BackendResult<Vec<TerminalSession>>,
    ) -> bool {
        if ticket.generation != self.state.generation
            || self.state.workspace_id.as_ref() != Some(&ticket.workspace_id)
        {
            return false;
        }
        match result {
            Ok(sessions) => {
                self.state.sessions = sessions;
                self.state.connection = TerminalConnectionState::Idle;
            }
            Err(error) => {
                self.state.connection = if error.kind == BackendErrorKind::Offline {
                    TerminalConnectionState::Offline
                } else {
                    TerminalConnectionState::Error
                };
                self.state.last_error = Some(error.clone());
            }
        }
        true
    }

    pub async fn refresh(&mut self, workspace_id: WorkspaceId) -> BackendResult<()> {
        let ticket = self.begin_refresh(workspace_id)?;
        let result = self.load_sessions(ticket.clone()).await;
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        let _ = self.apply_refresh(&ticket, result);
        outcome
    }

    pub fn attach(&mut self, terminal_id: TerminalId) -> BackendResult<()> {
        self.capabilities
            .require(BackendOperation::TerminalAttach)?;
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == terminal_id)
            .cloned()
        else {
            return Err(BackendError::failed(
                "terminal_session_missing",
                "the requested terminal session is not in the current workspace",
            ));
        };
        let subscription = self.backend.subscribe_terminal(terminal_id.clone(), 1)?;
        let generation = self.state.advance_generation();
        self.subscription = Some(subscription);
        self.subscription_generation = generation;
        self.state.active_session = Some(session.clone());
        self.state.access = if session.status == TerminalStatus::Running {
            TerminalAccessMode::ReadWrite
        } else {
            TerminalAccessMode::ReadOnly
        };
        self.state.connection = TerminalConnectionState::Connecting;
        self.state.raw = TerminalRawBuffer::new(TERMINAL_RAW_FRAME_LIMIT, TERMINAL_RAW_BYTE_BUDGET);
        self.state.raw.terminal_id = Some(terminal_id);
        self.state.render = Some(TerminalRenderModel::new(session.rows, session.cols));
        self.state.last_error = None;
        Ok(())
    }

    pub fn begin_poll(&mut self) -> Option<TerminalPollTask> {
        let subscription = self.subscription.take()?;
        Some(TerminalPollTask {
            generation: self.subscription_generation,
            subscription,
        })
    }

    pub fn apply_poll_result(
        &mut self,
        generation: u64,
        subscription: Box<dyn TerminalFrameSubscription>,
        result: BackendResult<Option<TerminalFrameBatch>>,
    ) -> BackendResult<bool> {
        if generation != self.state.generation || self.subscription_generation != generation {
            return Ok(false);
        }
        match result {
            Ok(Some(batch)) => {
                self.subscription = Some(subscription);
                let previous_sequence = self.state.raw.next_sequence;
                let rebuilt = self.state.raw.apply_batch(&batch)?.rebuilt;
                if rebuilt {
                    self.state.connection = TerminalConnectionState::Rebuilding;
                    if let Some(render) = self.state.render.as_mut() {
                        let (rows, cols) = self
                            .state
                            .active_session
                            .as_ref()
                            .map(|session| (session.rows, session.cols))
                            .unwrap_or((TERMINAL_DEFAULT_ROWS, TERMINAL_DEFAULT_COLS));
                        render.reset(rows, cols);
                    }
                }
                if let Some(render) = self.state.render.as_mut() {
                    if rebuilt {
                        for frame in self.state.raw.frames() {
                            render.apply(&frame.bytes);
                        }
                    } else {
                        for frame in &batch.frames {
                            if frame.sequence >= previous_sequence {
                                render.apply(&frame.bytes);
                            }
                        }
                    }
                }
                self.state.connection = TerminalConnectionState::Connected;
                Ok(true)
            }
            Ok(None) => {
                // A binary stream ending is transport-ambiguous: the socket
                // may have dropped rather than the terminal closing. Mark the
                // view reconnecting and let the recovery reattach resolve the
                // authoritative terminal state; a deliberate close still
                // reports `Closed` through its own confirmed path.
                self.state.advance_generation();
                self.state.connection = TerminalConnectionState::Reconnecting;
                Ok(false)
            }
            Err(error) => {
                self.subscription = Some(subscription);
                self.state.connection = if error.kind == BackendErrorKind::Offline {
                    TerminalConnectionState::Offline
                } else {
                    TerminalConnectionState::Reconnecting
                };
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn poll(&mut self) -> BackendResult<bool> {
        let Some(task) = self.begin_poll() else {
            return Ok(false);
        };
        let (generation, subscription, result) = task.next().await;
        self.apply_poll_result(generation, subscription, result)
    }

    pub fn begin_create(
        &self,
        request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendResult<TerminalCreateOperation> {
        self.capabilities
            .require(BackendOperation::TerminalCreate)?;
        request.validate()?;
        let workspace_id = self.state.workspace_id.clone().ok_or_else(|| {
            BackendError::failed(
                "terminal_workspace_missing",
                "select a workspace before creating a terminal",
            )
        })?;
        if request.payload.workspace_id != workspace_id {
            return Err(BackendError::conflict(
                "terminal_workspace_generation_stale",
                "the terminal create request targets another workspace",
            ));
        }
        Ok(TerminalCreateOperation {
            generation: self.state.generation,
            workspace_id,
            request,
        })
    }

    pub fn run_create(
        &self,
        operation: TerminalCreateOperation,
    ) -> BackendFuture<'static, TerminalSession> {
        if let Err(error) = self.capabilities.require(BackendOperation::TerminalCreate) {
            return Box::pin(async move { Err(error) });
        }
        if operation.generation != self.state.generation
            || self.state.workspace_id.as_ref() != Some(&operation.workspace_id)
        {
            return Box::pin(async {
                Err(BackendError::conflict(
                    "terminal_workspace_generation_stale",
                    "the terminal create operation is no longer current",
                ))
            });
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.create_terminal(operation.request).await })
    }

    pub fn apply_create(
        &mut self,
        operation: &TerminalCreateOperation,
        result: BackendResult<TerminalSession>,
    ) -> bool {
        if operation.generation != self.state.generation
            || self.state.workspace_id.as_ref() != Some(&operation.workspace_id)
        {
            return false;
        }
        match result {
            Ok(session) if session.workspace_id == operation.workspace_id => {
                self.state.sessions.retain(|item| item.id != session.id);
                self.state.sessions.push(session);
                self.state.last_error = None;
            }
            Ok(_) => {
                self.state.last_error = Some(BackendError::failed(
                    "terminal_create_response_mismatch",
                    "the backend created a terminal in another workspace",
                ))
            }
            Err(error) => self.state.last_error = Some(error),
        }
        true
    }

    pub async fn create(
        &mut self,
        request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendResult<TerminalSession> {
        let operation = self.begin_create(request)?;
        let result = self.run_create(operation.clone()).await;
        let outcome = result.as_ref().cloned().map_err(Clone::clone);
        let _ = self.apply_create(&operation, result);
        outcome
    }

    pub fn begin_send_input(
        &mut self,
        input: TerminalInput,
    ) -> BackendResult<Option<TerminalInputOperation>> {
        self.capabilities.require(BackendOperation::TerminalInput)?;
        let Some(session) = self.state.active_session.clone() else {
            return Err(BackendError::failed(
                "terminal_not_attached",
                "attach a terminal before sending input",
            ));
        };
        if self.state.access == TerminalAccessMode::ReadOnly {
            return Err(BackendError::permission(
                "terminal_read_only",
                "the paired device has read-only terminal access",
            ));
        }
        if matches!(input, TerminalInput::Key(TerminalKey::Control, _)) {
            self.state.control_latched = !self.state.control_latched;
            return Ok(None);
        }
        let input = match input {
            TerminalInput::Key(key, mut modifiers) => {
                modifiers.control |= self.state.control_latched;
                TerminalInput::Key(key, modifiers)
            }
            other => other,
        };
        let bytes = self
            .state
            .render
            .as_ref()
            .map(|render| render.encode_input(input))
            .unwrap_or_default();
        if bytes.len() > TERMINAL_MAX_INPUT_BYTES {
            return Err(BackendError::failed(
                "terminal_input_too_large",
                "terminal input exceeds the bounded request size",
            ));
        }
        let terminal_id = session.id;
        let request = MutationRequest::new(TerminalWriteRequest {
            terminal_id: terminal_id.clone(),
            data: std::str::from_utf8(&bytes)
                .map_err(|_| {
                    BackendError::failed(
                        "terminal_input_not_utf8",
                        "terminal input must use the UTF-8 control contract",
                    )
                })?
                .to_string(),
        });
        Ok(Some(TerminalInputOperation {
            generation: self.state.generation,
            terminal_id,
            request,
        }))
    }

    pub fn run_input(&self, operation: TerminalInputOperation) -> BackendFuture<'static, ()> {
        if let Err(error) = self.capabilities.require(BackendOperation::TerminalInput) {
            return Box::pin(async move { Err(error) });
        }
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return Box::pin(async {
                Err(BackendError::conflict(
                    "terminal_operation_stale",
                    "the terminal input operation is no longer current",
                ))
            });
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.write_terminal(operation.request).await })
    }

    pub fn apply_input(
        &mut self,
        operation: &TerminalInputOperation,
        result: BackendResult<()>,
    ) -> bool {
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return false;
        }
        match result {
            Ok(()) => {
                self.state.control_latched = false;
                self.state.last_error = None;
            }
            Err(error) => self.state.last_error = Some(error),
        }
        true
    }

    pub async fn send_input(&mut self, input: TerminalInput) -> BackendResult<()> {
        let Some(operation) = self.begin_send_input(input)? else {
            return Ok(());
        };
        let result = self.run_input(operation.clone()).await;
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        let _ = self.apply_input(&operation, result);
        outcome
    }

    pub fn begin_resize(&self, rows: u16, cols: u16) -> BackendResult<TerminalResizeOperation> {
        self.capabilities
            .require(BackendOperation::TerminalResize)?;
        let Some(session) = self.state.active_session.clone() else {
            return Err(BackendError::failed(
                "terminal_not_attached",
                "attach a terminal before resizing",
            ));
        };
        let terminal_id = session.id;
        Ok(TerminalResizeOperation {
            generation: self.state.generation,
            terminal_id: terminal_id.clone(),
            request: MutationRequest::new(TerminalResizeRequest {
                terminal_id,
                rows: rows.max(1),
                cols: cols.max(1),
            }),
        })
    }

    pub fn run_resize(
        &self,
        operation: TerminalResizeOperation,
    ) -> BackendFuture<'static, TerminalSession> {
        if let Err(error) = self.capabilities.require(BackendOperation::TerminalResize) {
            return Box::pin(async move { Err(error) });
        }
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return Box::pin(async {
                Err(BackendError::conflict(
                    "terminal_operation_stale",
                    "the terminal resize operation is no longer current",
                ))
            });
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.resize_terminal(operation.request).await })
    }

    pub fn apply_resize(
        &mut self,
        operation: &TerminalResizeOperation,
        result: BackendResult<TerminalSession>,
    ) -> bool {
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return false;
        }
        match result {
            Ok(updated) if updated.id == operation.terminal_id => {
                self.state.active_session = Some(updated.clone());
                self.state.sessions.retain(|item| item.id != updated.id);
                self.state.sessions.push(updated.clone());
                if let Some(render) = self.state.render.as_mut() {
                    render.resize(updated.rows, updated.cols);
                }
                self.state.last_error = None;
            }
            Ok(_) => {
                self.state.last_error = Some(BackendError::failed(
                    "terminal_resize_response_mismatch",
                    "the backend resized a different terminal",
                ))
            }
            Err(error) => self.state.last_error = Some(error),
        }
        true
    }

    pub async fn resize(&mut self, rows: u16, cols: u16) -> BackendResult<TerminalSession> {
        let operation = self.begin_resize(rows, cols)?;
        let result = self.run_resize(operation.clone()).await;
        let outcome = result.as_ref().cloned().map_err(Clone::clone);
        let _ = self.apply_resize(&operation, result);
        outcome
    }

    pub fn begin_close(&self) -> BackendResult<TerminalCloseOperation> {
        self.capabilities.require(BackendOperation::TerminalClose)?;
        let Some(session) = self.state.active_session.clone() else {
            return Err(BackendError::failed(
                "terminal_not_attached",
                "attach a terminal before closing it",
            ));
        };
        Ok(TerminalCloseOperation {
            generation: self.state.generation,
            terminal_id: session.id.clone(),
            request: MutationRequest::new(session.id),
        })
    }

    pub fn run_close(
        &self,
        operation: TerminalCloseOperation,
    ) -> BackendFuture<'static, TerminalSession> {
        if let Err(error) = self.capabilities.require(BackendOperation::TerminalClose) {
            return Box::pin(async move { Err(error) });
        }
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return Box::pin(async {
                Err(BackendError::conflict(
                    "terminal_operation_stale",
                    "the terminal close operation is no longer current",
                ))
            });
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.close_terminal(operation.request).await })
    }

    pub fn apply_close(
        &mut self,
        operation: &TerminalCloseOperation,
        result: BackendResult<TerminalSession>,
    ) -> bool {
        if !self.terminal_operation_is_current(operation.generation, &operation.terminal_id) {
            return false;
        }
        match result {
            Ok(closed) if closed.id == operation.terminal_id => {
                self.state.advance_generation();
                self.subscription = None;
                self.state.connection = TerminalConnectionState::Closed;
                self.state.access = TerminalAccessMode::ReadOnly;
                self.state.active_session = Some(closed.clone());
                self.state.sessions.retain(|item| item.id != closed.id);
                self.state.sessions.push(closed);
                self.state.last_error = None;
            }
            Ok(_) => {
                self.state.last_error = Some(BackendError::failed(
                    "terminal_close_response_mismatch",
                    "the backend closed a different terminal",
                ))
            }
            Err(error) => self.state.last_error = Some(error),
        }
        true
    }

    pub async fn close(&mut self) -> BackendResult<TerminalSession> {
        let operation = self.begin_close()?;
        let result = self.run_close(operation.clone()).await;
        let outcome = result.as_ref().cloned().map_err(Clone::clone);
        let _ = self.apply_close(&operation, result);
        outcome
    }

    pub fn apply_host_viewport(&mut self, snapshot: &HostViewportSnapshot) -> TerminalViewport {
        let viewport = TerminalViewport::from_host(snapshot);
        self.state.viewport = Some(viewport);
        viewport
    }

    pub fn disconnect(&mut self) {
        self.state.advance_generation();
        self.state.connection = TerminalConnectionState::Reconnecting;
        self.subscription = None;
    }

    fn terminal_operation_is_current(&self, generation: u64, terminal_id: &TerminalId) -> bool {
        generation == self.state.generation
            && self
                .state
                .active_session
                .as_ref()
                .is_some_and(|session| &session.id == terminal_id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use vibex_backend::BackendFuture;
    use vibex_core::{TerminalId, TerminalSnapshot};

    struct EmptyTerminalSubscription;

    impl TerminalFrameSubscription for EmptyTerminalSubscription {
        fn next(&mut self) -> BackendFuture<'_, Option<TerminalFrameBatch>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[derive(Clone)]
    struct MockTerminalBackend {
        session: TerminalSession,
        creates: Arc<Mutex<Vec<TerminalCreateRequest>>>,
        writes: Arc<Mutex<Vec<TerminalWriteRequest>>>,
        resizes: Arc<Mutex<Vec<TerminalResizeRequest>>>,
        closes: Arc<Mutex<Vec<TerminalId>>>,
    }

    impl MockTerminalBackend {
        fn new(status: TerminalStatus) -> Self {
            Self {
                session: TerminalSession {
                    id: TerminalId::new(),
                    workspace_id: WorkspaceId::new(),
                    title: "Fixture terminal".into(),
                    shell: "/bin/sh".into(),
                    cwd: "/fixture".into(),
                    rows: 24,
                    cols: 80,
                    status,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    closed_at_ms: None,
                },
                creates: Arc::new(Mutex::new(Vec::new())),
                writes: Arc::new(Mutex::new(Vec::new())),
                resizes: Arc::new(Mutex::new(Vec::new())),
                closes: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl TerminalBackend for MockTerminalBackend {
        fn list_terminals(
            &self,
            _workspace_id: WorkspaceId,
        ) -> BackendFuture<'_, Vec<TerminalSession>> {
            let session = self.session.clone();
            Box::pin(async move { Ok(vec![session]) })
        }

        fn create_terminal(
            &self,
            request: MutationRequest<TerminalCreateRequest>,
        ) -> BackendFuture<'_, TerminalSession> {
            let session = self.session.clone();
            let creates = self.creates.clone();
            Box::pin(async move {
                creates
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock creates poisoned"))?
                    .push(request.payload);
                Ok(session)
            })
        }

        fn terminal_snapshot(
            &self,
            terminal_id: TerminalId,
        ) -> BackendFuture<'_, TerminalSnapshot> {
            let session = self.session.clone();
            Box::pin(async move {
                if terminal_id != session.id {
                    return Err(BackendError::failed(
                        "terminal_session_missing",
                        "fixture terminal was not found",
                    ));
                }
                Ok(TerminalSnapshot {
                    session,
                    chunks: Vec::new(),
                    next_sequence: 1,
                })
            })
        }

        fn subscribe_terminal(
            &self,
            terminal_id: TerminalId,
            _next_sequence: i64,
        ) -> BackendResult<Box<dyn TerminalFrameSubscription>> {
            if terminal_id != self.session.id {
                return Err(BackendError::failed(
                    "terminal_session_missing",
                    "fixture terminal was not found",
                ));
            }
            Ok(Box::new(EmptyTerminalSubscription))
        }

        fn write_terminal(
            &self,
            request: MutationRequest<TerminalWriteRequest>,
        ) -> BackendFuture<'_, ()> {
            let writes = self.writes.clone();
            Box::pin(async move {
                writes
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock writes poisoned"))?
                    .push(request.payload);
                Ok(())
            })
        }

        fn resize_terminal(
            &self,
            request: MutationRequest<TerminalResizeRequest>,
        ) -> BackendFuture<'_, TerminalSession> {
            let mut session = self.session.clone();
            let resizes = self.resizes.clone();
            Box::pin(async move {
                resizes
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock resizes poisoned"))?
                    .push(request.payload.clone());
                session.rows = request.payload.rows;
                session.cols = request.payload.cols;
                Ok(session)
            })
        }

        fn close_terminal(
            &self,
            request: MutationRequest<TerminalId>,
        ) -> BackendFuture<'_, TerminalSession> {
            let mut session = self.session.clone();
            let closes = self.closes.clone();
            Box::pin(async move {
                closes
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock closes poisoned"))?
                    .push(request.payload);
                session.status = TerminalStatus::Killed;
                session.closed_at_ms = Some(2);
                Ok(session)
            })
        }
    }

    fn controller_with_backend(backend: Arc<MockTerminalBackend>) -> TerminalWorkflowController {
        let capabilities = TerminalWorkflowCapabilities::from_backend(
            &BackendCapabilitySnapshot::desktop_native_v1(),
        );
        let mut controller = TerminalWorkflowController::new(backend.clone(), capabilities);
        controller.state.sessions = vec![backend.session.clone()];
        controller.attach(backend.session.id.clone()).unwrap();
        controller
    }

    fn batch(id: &TerminalId, sequences: &[(i64, &[u8])], next: i64) -> TerminalFrameBatch {
        TerminalFrameBatch {
            terminal_id: id.clone(),
            frames: sequences
                .iter()
                .map(|(sequence, bytes)| TerminalFrame {
                    sequence: *sequence,
                    bytes: bytes.to_vec(),
                })
                .collect(),
            next_sequence: next,
            dropped_frames: 0,
            reset_required: false,
        }
    }

    #[test]
    fn raw_frames_replay_contiguously_and_rebuild_on_gap() {
        let id = TerminalId::new();
        let mut buffer = TerminalRawBuffer::new(8, 64);
        let first = buffer.apply_batch(&batch(&id, &[(1, b"A"), (2, b"\x1b[31mB")], 3));
        assert_eq!(first.unwrap().accepted_frames, 2);
        let mut gap = batch(&id, &[(5, b"C")], 6);
        gap.reset_required = true;
        let outcome = buffer.apply_batch(&gap).unwrap();
        assert!(outcome.rebuilt);
        assert_eq!(buffer.next_sequence, 6);
        assert_eq!(buffer.frames().count(), 1);
        assert_eq!(buffer.frames().next().unwrap().bytes, b"C");
    }

    #[test]
    fn raw_buffer_is_bounded_without_leaking_bytes_in_debug() {
        let id = TerminalId::new();
        let mut buffer = TerminalRawBuffer::new(2, 5);
        buffer
            .apply_batch(&batch(&id, &[(1, b"secret"), (2, b"x"), (3, b"y")], 4))
            .unwrap();
        assert!(buffer.frames().count() <= 2);
        assert!(buffer.retained_bytes <= 5);
        let debug = format!("{buffer:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("retained_bytes"));
    }

    #[test]
    fn compact_key_bar_is_touch_discoverable_and_control_is_visible() {
        let actions = compact_key_bar();
        assert!(
            actions
                .iter()
                .any(|action| action.key == TerminalKey::Control)
        );
        assert!(
            actions
                .iter()
                .all(TerminalKeyBarAction::is_touch_discoverable)
        );
    }

    #[test]
    fn viewport_subtracts_keyboard_and_safe_area() {
        let viewport = TerminalViewport::from_host(&HostViewportSnapshot {
            width: 390.0,
            height: 844.0,
            safe_area: crate::HostInsets {
                bottom: 34.0,
                ..Default::default()
            },
            keyboard_visible: true,
            keyboard_inset: 300.0,
            keyboard_source: crate::HostKeyboardSource::Capacitor,
        });
        assert_eq!(viewport.visible_height_px, 544);
        assert!(viewport.keeps_recent_output_visible());
    }

    #[test]
    fn render_model_keeps_ansi_bytes_raw_until_emulator_consumes_them() {
        let mut render = TerminalRenderModel::new(2, 20);
        render.apply(b"hello\x1b[31m world");
        assert!(render.frame.cells.iter().any(|cell| cell.text == "h"));
        assert!(render.frame.ingested_bytes >= 16);
        let debug = format!("{render:?}");
        assert!(!debug.contains("hello"));
    }

    #[test]
    fn terminal_capabilities_filter_to_safe_v1_operations() {
        let snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        let capabilities = TerminalWorkflowCapabilities::from_backend(&snapshot);
        assert!(capabilities.supports(BackendOperation::TerminalList));
        assert!(capabilities.supports(BackendOperation::TerminalInput));
        assert_eq!(capabilities.domain.operations.len(), 6);
        assert!(capabilities.require(BackendOperation::FileDelete).is_err());
    }

    #[tokio::test]
    async fn read_only_input_is_rejected_before_backend_write() {
        let backend = Arc::new(MockTerminalBackend::new(TerminalStatus::Exited));
        let mut controller = controller_with_backend(backend.clone());

        let error = controller
            .send_input(TerminalInput::Text("must-not-send".into()))
            .await
            .unwrap_err();

        assert_eq!(error.code, "terminal_read_only");
        assert!(backend.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn control_latch_applies_to_one_key_then_clears() {
        let backend = Arc::new(MockTerminalBackend::new(TerminalStatus::Running));
        let mut controller = controller_with_backend(backend.clone());

        controller
            .send_input(TerminalInput::Key(
                TerminalKey::Control,
                TerminalKeyModifiers::default(),
            ))
            .await
            .unwrap();
        assert!(controller.state.control_latched);
        assert!(backend.writes.lock().unwrap().is_empty());

        controller
            .send_input(TerminalInput::Key(
                TerminalKey::ArrowUp,
                TerminalKeyModifiers::default(),
            ))
            .await
            .unwrap();

        assert!(!controller.state.control_latched);
        let writes = backend.writes.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].data, "\u{1b}[1;5A");
    }

    #[tokio::test]
    async fn stale_operations_are_rejected_before_backend_calls() {
        let backend = Arc::new(MockTerminalBackend::new(TerminalStatus::Running));
        let mut controller = controller_with_backend(backend.clone());
        let workspace_id = backend.session.workspace_id.clone();
        controller.state.workspace_id = Some(workspace_id.clone());

        let create = controller
            .begin_create(MutationRequest::new(TerminalCreateRequest {
                workspace_id,
                title: Some("sensitive-title".into()),
                shell: Some("/sensitive/shell".into()),
                cwd: Some("/sensitive/path".into()),
                rows: 24,
                cols: 80,
            }))
            .unwrap();
        let input = controller
            .begin_send_input(TerminalInput::Text("sensitive-input".into()))
            .unwrap()
            .unwrap();
        let resize = controller.begin_resize(30, 100).unwrap();
        let close = controller.begin_close().unwrap();

        controller.state.advance_generation();

        for error in [
            controller.run_create(create.clone()).await.unwrap_err(),
            controller.run_input(input.clone()).await.unwrap_err(),
            controller.run_resize(resize.clone()).await.unwrap_err(),
            controller.run_close(close.clone()).await.unwrap_err(),
        ] {
            assert!(error.code.contains("stale"));
        }
        assert!(backend.creates.lock().unwrap().is_empty());
        assert!(backend.writes.lock().unwrap().is_empty());
        assert!(backend.resizes.lock().unwrap().is_empty());
        assert!(backend.closes.lock().unwrap().is_empty());

        let debug = format!("{create:?} {input:?} {resize:?} {close:?}");
        assert!(!debug.contains("sensitive"));
    }

    #[test]
    fn disconnect_fences_an_in_flight_poll_result() {
        let backend = Arc::new(MockTerminalBackend::new(TerminalStatus::Running));
        let mut controller = controller_with_backend(backend);
        let generation = controller.state.generation;

        controller.disconnect();
        let terminal_id = controller.state.active_session.as_ref().unwrap().id.clone();
        let applied = controller
            .apply_poll_result(
                generation,
                Box::new(EmptyTerminalSubscription),
                Ok(Some(batch(&terminal_id, &[(1, b"late")], 2))),
            )
            .unwrap();

        assert!(!applied);
        assert_eq!(
            controller.state.connection,
            TerminalConnectionState::Reconnecting
        );
        assert_eq!(controller.state.raw.retained_bytes, 0);
    }

    #[test]
    fn successful_close_fences_an_in_flight_poll_result() {
        let backend = Arc::new(MockTerminalBackend::new(TerminalStatus::Running));
        let mut controller = controller_with_backend(backend.clone());
        let poll_generation = controller.state.generation;
        let operation = controller.begin_close().unwrap();
        let mut closed = backend.session.clone();
        closed.status = TerminalStatus::Killed;
        closed.closed_at_ms = Some(2);

        assert!(controller.apply_close(&operation, Ok(closed)));
        let applied = controller
            .apply_poll_result(
                poll_generation,
                Box::new(EmptyTerminalSubscription),
                Ok(Some(batch(&backend.session.id, &[(1, b"late")], 2))),
            )
            .unwrap();

        assert!(!applied);
        assert_eq!(controller.state.connection, TerminalConnectionState::Closed);
        assert_eq!(controller.state.raw.retained_bytes, 0);
    }
}
