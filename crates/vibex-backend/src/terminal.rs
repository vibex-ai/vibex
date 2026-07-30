use std::fmt;

use serde::{Deserialize, Serialize};
use vibex_core::{
    TerminalCreateRequest, TerminalId, TerminalResizeRequest, TerminalSession, TerminalSnapshot,
    TerminalWriteRequest, WorkspaceId,
};

use crate::{BackendBound, BackendFuture, BackendResult, MutationRequest};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrame {
    pub sequence: i64,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for TerminalFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalFrame")
            .field("sequence", &self.sequence)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrameBatch {
    pub terminal_id: TerminalId,
    pub frames: Vec<TerminalFrame>,
    pub next_sequence: i64,
    pub dropped_frames: u64,
    pub reset_required: bool,
}

impl fmt::Debug for TerminalFrameBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalFrameBatch")
            .field("terminal_id", &self.terminal_id)
            .field("frame_count", &self.frames.len())
            .field("next_sequence", &self.next_sequence)
            .field("dropped_frames", &self.dropped_frames)
            .field("reset_required", &self.reset_required)
            .finish()
    }
}

pub trait TerminalFrameSubscription: BackendBound {
    fn next(&mut self) -> BackendFuture<'_, Option<TerminalFrameBatch>>;
}

pub trait TerminalBackend: BackendBound {
    fn list_terminals(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, Vec<TerminalSession>>;

    fn create_terminal(
        &self,
        request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendFuture<'_, TerminalSession>;

    fn terminal_snapshot(&self, terminal_id: TerminalId) -> BackendFuture<'_, TerminalSnapshot>;

    fn subscribe_terminal(
        &self,
        terminal_id: TerminalId,
        next_sequence: i64,
    ) -> BackendResult<Box<dyn TerminalFrameSubscription>>;

    fn write_terminal(
        &self,
        request: MutationRequest<TerminalWriteRequest>,
    ) -> BackendFuture<'_, ()>;

    fn resize_terminal(
        &self,
        request: MutationRequest<TerminalResizeRequest>,
    ) -> BackendFuture<'_, TerminalSession>;

    fn close_terminal(
        &self,
        request: MutationRequest<TerminalId>,
    ) -> BackendFuture<'_, TerminalSession>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_frame_debug_exposes_metadata_without_output_bytes() {
        let frame = TerminalFrame {
            sequence: 7,
            bytes: b"terminal-secret".to_vec(),
        };
        let debug = format!("{frame:?}");

        assert!(debug.contains("sequence: 7"));
        assert!(debug.contains("byte_len: 15"));
        assert!(!debug.contains("terminal-secret"));

        let batch = TerminalFrameBatch {
            terminal_id: TerminalId::new(),
            frames: vec![frame],
            next_sequence: 8,
            dropped_frames: 2,
            reset_required: true,
        };
        let debug = format!("{batch:?}");

        assert!(debug.contains("frame_count: 1"));
        assert!(debug.contains("next_sequence: 8"));
        assert!(debug.contains("reset_required: true"));
        assert!(!debug.contains("sequence: 7"));
        assert!(!debug.contains("terminal-secret"));
    }
}
