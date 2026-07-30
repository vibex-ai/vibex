use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vibex_backend::{TerminalFrame, TerminalFrameBatch};
use vibex_core::{RemoteBinaryFrame, RemoteBinaryFrameKind, TerminalId};

pub const DEFAULT_MAX_FILE_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_FILE_CHUNK_BYTES: usize = 64 * 1024;
pub const DEFAULT_BINARY_QUEUE_CAPACITY: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChunkDescriptor {
    pub transfer_id: String,
    pub sequence: u64,
    pub offset: u64,
    pub total_size: Option<u64>,
    pub checksum_sha256: String,
    pub end_of_stream: bool,
}

impl FileChunkDescriptor {
    pub fn from_binary_frame(frame: &RemoteBinaryFrame) -> Result<Self, FileChunkError> {
        if frame.header.kind != RemoteBinaryFrameKind::FileDownloadChunk
            && frame.header.kind != RemoteBinaryFrameKind::FileUploadChunk
        {
            return Err(FileChunkError::TransferIdInvalid);
        }
        let checksum_sha256 = frame
            .header
            .checksum_sha256
            .clone()
            .ok_or(FileChunkError::ChecksumInvalid)?;
        Ok(Self {
            transfer_id: frame.header.stream_id.clone(),
            sequence: frame.header.sequence,
            offset: frame.header.offset,
            total_size: frame.header.total_size,
            checksum_sha256,
            end_of_stream: frame.header.end_of_stream,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChunkError {
    TransferIdInvalid,
    Finished,
    StreamMismatch,
    GenerationMismatch,
    SequenceGap { expected: u64, received: u64 },
    OffsetMismatch { expected: u64, received: u64 },
    ChunkTooLarge { max: usize, received: usize },
    TransferTooLarge { max: u64, received: u64 },
    TotalSizeMismatch { expected: u64, received: u64 },
    ChecksumInvalid,
    FinalChecksumMismatch,
    Cancelled,
}

pub trait FileChunkSink {
    fn write_chunk(
        &mut self,
        descriptor: &FileChunkDescriptor,
        bytes: &[u8],
    ) -> Result<(), FileChunkError>;
    fn finish(&mut self, checksum_sha256: Option<&str>) -> Result<(), FileChunkError>;
    fn cancel(&mut self);
}

/// Validates ordering, offsets, checksums and size limits without buffering a
/// complete file.  A caller supplies a sink that writes each accepted chunk to
/// a file or browser stream.
pub struct ChunkedFileReceiver<S> {
    sink: S,
    transfer_id: String,
    next_sequence: u64,
    next_offset: u64,
    total_size: Option<u64>,
    received_bytes: u64,
    max_size: u64,
    max_chunk_size: usize,
    digest: Sha256,
    cancelled: bool,
    finished: bool,
}

impl<S: FileChunkSink> ChunkedFileReceiver<S> {
    pub fn new(
        transfer_id: impl Into<String>,
        max_size: u64,
        max_chunk_size: usize,
        sink: S,
    ) -> Result<Self, FileChunkError> {
        let transfer_id = transfer_id.into();
        if transfer_id.trim().is_empty() || transfer_id.len() > 256 {
            return Err(FileChunkError::TransferIdInvalid);
        }
        Ok(Self {
            sink,
            transfer_id,
            next_sequence: 0,
            next_offset: 0,
            total_size: None,
            received_bytes: 0,
            max_size: max_size.max(1),
            max_chunk_size: max_chunk_size.max(1),
            digest: Sha256::new(),
            cancelled: false,
            finished: false,
        })
    }

    pub fn push(
        &mut self,
        descriptor: &FileChunkDescriptor,
        bytes: &[u8],
    ) -> Result<(), FileChunkError> {
        if self.cancelled {
            return Err(FileChunkError::Cancelled);
        }
        if self.finished {
            return Err(FileChunkError::Finished);
        }
        if descriptor.transfer_id != self.transfer_id {
            return Err(FileChunkError::TransferIdInvalid);
        }
        if descriptor.sequence != self.next_sequence {
            return Err(FileChunkError::SequenceGap {
                expected: self.next_sequence,
                received: descriptor.sequence,
            });
        }
        if descriptor.offset != self.next_offset {
            return Err(FileChunkError::OffsetMismatch {
                expected: self.next_offset,
                received: descriptor.offset,
            });
        }
        if bytes.len() > self.max_chunk_size {
            return Err(FileChunkError::ChunkTooLarge {
                max: self.max_chunk_size,
                received: bytes.len(),
            });
        }
        let next_total = self.received_bytes.checked_add(bytes.len() as u64).ok_or(
            FileChunkError::TransferTooLarge {
                max: self.max_size,
                received: u64::MAX,
            },
        )?;
        if next_total > self.max_size {
            return Err(FileChunkError::TransferTooLarge {
                max: self.max_size,
                received: next_total,
            });
        }
        if let Some(total_size) = descriptor.total_size {
            if total_size > self.max_size {
                return Err(FileChunkError::TransferTooLarge {
                    max: self.max_size,
                    received: total_size,
                });
            }
            if next_total > total_size {
                return Err(FileChunkError::TotalSizeMismatch {
                    expected: total_size,
                    received: next_total,
                });
            }
            if let Some(expected_total) = self.total_size
                && expected_total != total_size
            {
                return Err(FileChunkError::TotalSizeMismatch {
                    expected: expected_total,
                    received: total_size,
                });
            }
            self.total_size = Some(total_size);
        }
        if !verify_sha256(bytes, &descriptor.checksum_sha256) {
            return Err(FileChunkError::ChecksumInvalid);
        }

        if descriptor.end_of_stream && self.total_size.is_some_and(|total| total != next_total) {
            return Err(FileChunkError::TotalSizeMismatch {
                expected: self.total_size.unwrap_or_default(),
                received: next_total,
            });
        }

        self.sink.write_chunk(descriptor, bytes)?;
        self.digest.update(bytes);
        self.received_bytes = next_total;
        self.next_offset = self.next_offset.checked_add(bytes.len() as u64).ok_or(
            FileChunkError::TransferTooLarge {
                max: self.max_size,
                received: u64::MAX,
            },
        )?;
        self.next_sequence = self.next_sequence.saturating_add(1);

        if descriptor.end_of_stream {
            self.sink.finish(Some(&hex_digest(self.digest.clone())))?;
            self.finished = true;
        }
        Ok(())
    }

    pub fn push_binary_frame(&mut self, frame: &RemoteBinaryFrame) -> Result<(), FileChunkError> {
        let descriptor = FileChunkDescriptor::from_binary_frame(frame)?;
        self.push(&descriptor, &frame.payload)
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.sink.cancel();
    }

    pub fn received_bytes(&self) -> u64 {
        self.received_bytes
    }

    pub fn computed_checksum_sha256(&self) -> String {
        hex_digest(self.digest.clone())
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn into_sink(self) -> S {
        self.sink
    }
}

fn verify_sha256(bytes: &[u8], expected: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    expected.len() == 64 && hex_digest_bytes(bytes) == expected
}

fn hex_digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_digest(digest: Sha256) -> String {
    let digest = digest.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone)]
pub struct TerminalBinaryBuffer {
    terminal_id: TerminalId,
    capacity: usize,
    frames: VecDeque<TerminalFrame>,
    next_sequence: i64,
    generation: Option<u64>,
    dropped_frames: u64,
    reset_required: bool,
}

impl TerminalBinaryBuffer {
    pub fn new(terminal_id: TerminalId, capacity: usize, next_sequence: i64) -> Self {
        Self {
            terminal_id,
            capacity: capacity.max(1),
            frames: VecDeque::new(),
            next_sequence: next_sequence.max(1),
            generation: None,
            dropped_frames: 0,
            reset_required: false,
        }
    }

    pub fn push_frame(&mut self, frame: &RemoteBinaryFrame) -> Result<(), FileChunkError> {
        if frame.header.stream_id != self.terminal_id.as_str() {
            return Err(FileChunkError::StreamMismatch);
        }
        if frame.header.kind != RemoteBinaryFrameKind::TerminalOutput
            && frame.header.kind != RemoteBinaryFrameKind::TerminalSnapshot
        {
            return Err(FileChunkError::StreamMismatch);
        }
        if let Some(generation) = self.generation {
            if generation != frame.header.generation {
                return Err(FileChunkError::GenerationMismatch);
            }
        } else {
            self.generation = Some(frame.header.generation);
        }
        if let Some(expected) = frame.header.checksum_sha256.as_deref()
            && !verify_sha256(&frame.payload, expected)
        {
            return Err(FileChunkError::ChecksumInvalid);
        }
        let sequence =
            i64::try_from(frame.header.sequence).map_err(|_| FileChunkError::SequenceGap {
                expected: self.next_sequence as u64,
                received: frame.header.sequence,
            })?;
        if sequence < self.next_sequence && !frame.header.snapshot {
            return Err(FileChunkError::SequenceGap {
                expected: self.next_sequence as u64,
                received: frame.header.sequence,
            });
        }
        if sequence > self.next_sequence || frame.header.snapshot {
            self.reset_required = true;
        }
        if frame.header.snapshot {
            self.frames.clear();
        }
        if self.frames.len() >= self.capacity {
            self.frames.pop_front();
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            self.reset_required = true;
        }
        self.frames.push_back(TerminalFrame {
            sequence,
            bytes: frame.payload.clone(),
        });
        self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
        Ok(())
    }

    pub fn take_batch(&mut self) -> Option<TerminalFrameBatch> {
        if self.frames.is_empty() {
            return None;
        }
        Some(TerminalFrameBatch {
            terminal_id: self.terminal_id.clone(),
            frames: self.frames.drain(..).collect(),
            next_sequence: self.next_sequence,
            dropped_frames: self.dropped_frames,
            reset_required: std::mem::take(&mut self.reset_required),
        })
    }

    pub fn next_sequence(&self) -> i64 {
        self.next_sequence
    }

    /// Discard retained frames while preserving an explicit rebuild marker.
    /// The next snapshot/output frame can then be consumed without mixing it
    /// with bytes from the previous generation.
    pub fn require_reset(&mut self, next_sequence: i64) {
        self.frames.clear();
        self.next_sequence = next_sequence.max(1);
        self.reset_required = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{RemoteBinaryFrameHeader, RemoteProtocolVersion};

    #[derive(Default)]
    struct Sink {
        bytes: Vec<u8>,
        finished: bool,
        cancelled: bool,
    }

    impl FileChunkSink for Sink {
        fn write_chunk(
            &mut self,
            _descriptor: &FileChunkDescriptor,
            bytes: &[u8],
        ) -> Result<(), FileChunkError> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }

        fn finish(&mut self, _checksum_sha256: Option<&str>) -> Result<(), FileChunkError> {
            self.finished = true;
            Ok(())
        }

        fn cancel(&mut self) {
            self.cancelled = true;
        }
    }

    fn descriptor(
        transfer_id: &str,
        sequence: u64,
        offset: u64,
        bytes: &[u8],
        end: bool,
    ) -> FileChunkDescriptor {
        FileChunkDescriptor {
            transfer_id: transfer_id.to_string(),
            sequence,
            offset,
            total_size: Some(3),
            checksum_sha256: hex_digest_bytes(bytes),
            end_of_stream: end,
        }
    }

    #[test]
    fn file_receiver_validates_offsets_checksums_and_size() {
        let mut receiver = ChunkedFileReceiver::new("transfer", 16, 4, Sink::default()).unwrap();
        receiver
            .push(&descriptor("transfer", 0, 0, b"ab", false), b"ab")
            .unwrap();
        assert!(matches!(
            receiver.push(&descriptor("transfer", 2, 2, b"c", true), b"c"),
            Err(FileChunkError::SequenceGap { .. })
        ));
        receiver
            .push(&descriptor("transfer", 1, 2, b"c", true), b"c")
            .unwrap();
        assert_eq!(receiver.received_bytes(), 3);
        assert!(receiver.into_sink().finished);
    }

    #[test]
    fn invalid_final_size_is_rejected_before_sink_commit() {
        let mut receiver = ChunkedFileReceiver::new("transfer", 16, 4, Sink::default()).unwrap();
        let descriptor = FileChunkDescriptor {
            transfer_id: "transfer".to_string(),
            sequence: 0,
            offset: 0,
            total_size: Some(3),
            checksum_sha256: hex_digest_bytes(b"ab"),
            end_of_stream: true,
        };
        assert!(matches!(
            receiver.push(&descriptor, b"ab"),
            Err(FileChunkError::TotalSizeMismatch {
                expected: 3,
                received: 2
            })
        ));
        let sink = receiver.into_sink();
        assert!(sink.bytes.is_empty());
        assert!(!sink.finished);
    }

    #[test]
    fn terminal_buffer_is_bounded_and_marks_reset_on_eviction_or_gap() {
        let terminal_id = TerminalId::new();
        let mut buffer = TerminalBinaryBuffer::new(terminal_id.clone(), 1, 1);
        for (sequence, bytes) in [(1, b"a".to_vec()), (3, b"c".to_vec())] {
            buffer
                .push_frame(&RemoteBinaryFrame {
                    header: RemoteBinaryFrameHeader {
                        protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                        kind: RemoteBinaryFrameKind::TerminalOutput,
                        stream_id: terminal_id.as_str().to_string(),
                        request_id: None,
                        generation: 1,
                        sequence,
                        offset: 0,
                        total_size: None,
                        snapshot: false,
                        end_of_stream: false,
                        checksum_sha256: None,
                        payload_length: 0,
                    },
                    payload: bytes,
                })
                .unwrap();
        }
        let batch = buffer.take_batch().unwrap();
        assert!(batch.reset_required);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.dropped_frames, 1);
    }

    #[test]
    fn terminal_buffer_rejects_cross_stream_frames() {
        let terminal_id = TerminalId::new();
        let mut buffer = TerminalBinaryBuffer::new(terminal_id, 4, 1);
        let error = buffer
            .push_frame(&RemoteBinaryFrame {
                header: RemoteBinaryFrameHeader {
                    protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                    kind: RemoteBinaryFrameKind::TerminalOutput,
                    stream_id: TerminalId::new().as_str().to_string(),
                    request_id: None,
                    generation: 1,
                    sequence: 1,
                    offset: 0,
                    total_size: None,
                    snapshot: false,
                    end_of_stream: false,
                    checksum_sha256: Some(hex_digest_bytes(b"bytes")),
                    payload_length: 0,
                },
                payload: b"bytes".to_vec(),
            })
            .unwrap_err();
        assert_eq!(error, FileChunkError::StreamMismatch);
    }
}
