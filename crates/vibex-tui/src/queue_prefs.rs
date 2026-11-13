//! What the Composer is still holding, remembered between runs.
//!
//! A held message is the reader's own words waiting for a turn or for an hour,
//! and a schedule is the one instruction a client cannot carry out on the spot:
//! a message given tomorrow morning to send has to outlive the process that
//! took it. The TUI kept its queue in memory alone, so closing the terminal
//! threw the schedule away — the reader came back to a session that had never
//! been asked for anything. The Desktop keeps the same queue in the UI state it
//! persists and loads it again at startup; this is that behaviour, in the file
//! this client keeps its own state in.
//!
//! Only what a later run can honestly rebuild is written down. A message's
//! words and its deadline travel as they are. A picture travels as the file its
//! bytes are in rather than as bytes: a state file carrying raw pixels would
//! grow by megabytes for one screenshot, and the send path already knows how to
//! put clipboard bytes in a file and name it. A picture whose bytes cannot be
//! written anywhere is dropped from the file — the words are what cannot be
//! retyped — and the message itself is kept.
//!
//! The file sits beside the arrangement, the switcher's memory and the
//! interface's own settings, under the same home, so a reader who wants this
//! client to forget everything clears one directory.
//!
//! Everything here needs no terminal, so the shape can be driven in a test
//! without one — which is what keeps a state file honest about what it can
//! rebuild.

use std::path::Path;

use serde::{Deserialize, Serialize};
use vibex_core::VibexSessionId;

use crate::app::QueuedMessage;
use crate::composer::{IMAGE_CAP, ImageAttachment, ImageSource};

/// How many held messages the file keeps, across every session.
///
/// A queue is what the reader is about to say, not a history: past this many
/// the oldest is dropped, so a client left running cannot grow the file without
/// limit. The bound is on messages rather than on sessions because a queue is
/// one list — the same list the reader reorders with `Alt+J` and `Alt+K`.
pub const QUEUE_PREFERENCE_LIMIT: usize = 64;

/// One message's own text.
///
/// The authority rejects a prompt longer than this anyway; the copy here only
/// keeps a corrupt or runaway value out of the file.
pub const QUEUE_TEXT_LIMIT: usize = 256 * 1024;

/// The queue as it was written down.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldQueue {
    #[serde(default)]
    pub messages: Vec<HeldMessage>,
}

/// One held message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldMessage {
    /// The session the message was written for, as it was written.
    ///
    /// Kept as the string the id prints rather than as a parsed id, because a
    /// file written by a newer build has to be readable as *something* by an
    /// older one: an entry whose session cannot be parsed is dropped on its
    /// own, and the messages around it still arrive.
    pub session_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<HeldImage>,
    /// The typed `@` references the message addresses.
    ///
    /// They are part of the message's intent, so a held message that comes back
    /// after a restart still names the same collaborator and configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<vibex_core::VibexUseMention>,
    /// The local deadline of a scheduled message, in milliseconds since the
    /// epoch. `None` is an ordinary held message that only waits for the
    /// session to be ready — the same field, with the same meaning, the
    /// Desktop persists in `ComposerQueueEntry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at_ms: Option<i64>,
}

/// One picture, named by the file its bytes are in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldImage {
    /// `Image #1`, exactly as the chip reads — restored rather than renumbered,
    /// so the message says what the reader saw it say.
    pub label: String,
    pub mime_type: String,
    /// How many bytes the picture is, as it was measured when it was attached.
    pub byte_len: usize,
    /// Where the bytes are, as a path the runtime can read.
    pub path: String,
    /// Where the picture sat in the message's text, which is what puts the
    /// chip back in its place.
    pub inline_text_offset: u32,
}

impl HeldQueue {
    /// The queue as it would be written down.
    ///
    /// The newest messages are the ones kept when the queue is longer than the
    /// bound: they are the ones the reader is about to act on, and the oldest
    /// are the ones a reader who queued sixty-four messages has stopped
    /// thinking about.
    pub fn of(queue: &[QueuedMessage]) -> Self {
        let start = queue.len().saturating_sub(QUEUE_PREFERENCE_LIMIT);
        Self {
            messages: queue[start..].iter().map(HeldMessage::of).collect(),
        }
    }

    /// Read what a previous run wrote.
    ///
    /// An unreadable file is an empty queue rather than an error: a state file
    /// that cannot be parsed is not worth refusing to start over, and every
    /// other file this client keeps answers a broken read the same way.
    pub fn load(path: Option<&Path>) -> Vec<QueuedMessage> {
        let Some(path) = path else {
            return Vec::new();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let Ok(held) = serde_json::from_str::<Self>(&raw) else {
            return Vec::new();
        };
        held.restore()
    }

    /// Persist the queue. A failure is silent: losing a held message to a full
    /// disk is not worth interrupting the reader over, and the next change
    /// tries again.
    pub fn save(&self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, body);
        }
    }

    /// The messages this file describes, in the order it holds them.
    ///
    /// The order is the queue's own: a message the reader moved above another
    /// has to come back above it, because that order is what decides which of
    /// two ready messages goes first. The bound applies on the way in as well
    /// as on the way out — the file is hand-editable, and one that was edited
    /// to hold a million messages is not a reason to start with a million.
    fn restore(self) -> Vec<QueuedMessage> {
        self.messages
            .into_iter()
            .filter_map(HeldMessage::restore)
            .take(QUEUE_PREFERENCE_LIMIT)
            .collect()
    }
}

impl HeldMessage {
    fn of(queued: &QueuedMessage) -> Self {
        Self {
            session_id: queued.session_id.as_str().to_string(),
            text: clamp_text(&queued.text),
            images: queued
                .images
                .iter()
                .filter_map(|(image, offset)| HeldImage::of(image, *offset))
                .collect(),
            mentions: queued.mentions.clone(),
            scheduled_at_ms: queued.scheduled_at_ms,
        }
    }

    fn restore(self) -> Option<QueuedMessage> {
        // A session id the authority would not accept cannot address a send, so
        // the entry is dropped rather than queued against a session that cannot
        // exist.
        let session_id = VibexSessionId::parse(&self.session_id).ok()?;
        let mut images = self
            .images
            .into_iter()
            .filter_map(HeldImage::restore)
            .collect::<Vec<_>>();
        images.truncate(IMAGE_CAP);
        Some(QueuedMessage {
            session_id,
            text: self.text,
            images,
            mentions: self.mentions,
            scheduled_at_ms: self.scheduled_at_ms,
        })
    }
}

impl HeldImage {
    /// The picture as a file, or `None` when its bytes are nowhere to be kept.
    fn of(image: &ImageAttachment, inline_text_offset: u32) -> Option<Self> {
        let path = match &image.source {
            // Already a path, and the runtime reads the file itself — the same
            // form the send path would hand it.
            ImageSource::Path(path) => path.clone(),
            // Bytes have to be put somewhere. They go where the send path puts
            // them, named by content, so queueing the same screenshot twice —
            // or queueing it and then sending it — writes one file.
            ImageSource::Bytes(bytes) => {
                crate::composer::materialise_attachment(&image.mime_type, bytes)?
            }
        };
        Some(Self {
            label: image.label.clone(),
            mime_type: image.mime_type.clone(),
            byte_len: image.byte_len,
            path,
            inline_text_offset,
        })
    }

    fn restore(self) -> Option<(ImageAttachment, u32)> {
        // The preview is redrawn from the file rather than remembered: the
        // raster is a cell grid whose size the picture decides, and a file that
        // cannot be read is still an attachment — the runtime it is sent to is
        // the one that has to have it. A preview-less chip says so, which is
        // what a picture this client cannot decode already looks like.
        let thumbnail = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| crate::preview::thumbnail(&bytes));
        Some((
            ImageAttachment {
                label: self.label,
                mime_type: self.mime_type,
                source: ImageSource::Path(self.path),
                byte_len: self.byte_len,
                thumbnail,
            },
            self.inline_text_offset,
        ))
    }
}

/// One piece of message text, cut to what the file will carry.
///
/// The cut is on a character boundary so the result is still valid text, and it
/// is preferred over dropping the message: a message whose text was truncated
/// can still be edited and sent, which a message that vanished cannot.
fn clamp_text(text: &str) -> String {
    if text.len() <= QUEUE_TEXT_LIMIT {
        return text.to_string();
    }
    let mut end = QUEUE_TEXT_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::composer::ImageSource;

    fn message(session: &str, text: &str, scheduled_at_ms: Option<i64>) -> QueuedMessage {
        QueuedMessage {
            session_id: VibexSessionId::parse(session).expect("a valid session id"),
            text: text.to_string(),
            images: Vec::new(),
            mentions: Vec::new(),
            scheduled_at_ms,
        }
    }

    #[test]
    fn a_scheduled_message_comes_back_with_its_hour() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-queue.json");
        let mut first = message(
            "session_queue0001",
            "send me later",
            Some(1_800_000_000_000),
        );
        first.mentions.push(vibex_core::VibexUseMention::agent(
            &vibex_core::AgentId::parse("claude").expect("agent id"),
            &vibex_core::VibexUseRef::runtime_option("option-claude"),
            Some("@Claude"),
        ));
        let queue = vec![
            first,
            message("session_queue0002", "and me after the turn", None),
        ];

        HeldQueue::of(&queue).save(Some(&path));
        let restored = HeldQueue::load(Some(&path));

        assert_eq!(restored, queue, "the queue did not survive the round trip");
        // The file is the reader's to read: the deadline is in it under the
        // name the Desktop persists it by.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("scheduledAtMs"),
            "the file does not name the deadline:\n{raw}"
        );
        assert!(
            raw.contains("session_queue0002"),
            "the second session is missing:\n{raw}"
        );
    }

    #[test]
    fn the_order_of_the_queue_is_the_order_of_the_file() {
        // Two messages that are both ready go out in the order the reader put
        // them in, so the file cannot be a set.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-queue.json");
        let queue = vec![
            message("session_queue0001", "second", None),
            message("session_queue0001", "first", None),
        ];
        HeldQueue::of(&queue).save(Some(&path));
        let restored = HeldQueue::load(Some(&path));
        assert_eq!(
            restored
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[test]
    fn a_picture_travels_as_a_file_and_comes_back_with_its_place() {
        let directory = tempfile::tempdir().unwrap();
        let mut queue = vec![message("session_queue0001", "look at Image #1", None)];
        queue[0].images.push((
            ImageAttachment {
                label: "Image #1".to_string(),
                mime_type: "image/png".to_string(),
                source: ImageSource::Bytes(Arc::new(small_png())),
                byte_len: small_png().len(),
                thumbnail: None,
            },
            12,
        ));

        HeldQueue::of(&queue).save(Some(&directory.path().join("tui-queue.json")));
        let restored = HeldQueue::load(Some(&directory.path().join("tui-queue.json")));

        let (image, offset) = restored[0].images.first().expect("the picture was dropped");
        assert_eq!(*offset, 12, "the chip lost its place in the text");
        assert_eq!(image.label, "Image #1");
        assert_eq!(image.byte_len, small_png().len());
        let ImageSource::Path(path) = &image.source else {
            panic!("the restored picture is not named by a file");
        };
        assert_eq!(
            std::fs::read(path).expect("the bytes were not written anywhere"),
            small_png(),
            "the file does not hold the picture that was queued"
        );
        // The preview is rebuilt from the file rather than remembered, so the
        // chip can draw again without the bytes being in the state file.
        assert!(
            image.thumbnail.is_some(),
            "the restored picture has no preview"
        );
    }

    #[test]
    fn a_queue_longer_than_the_bound_keeps_its_newest_messages() {
        let queue = (0..QUEUE_PREFERENCE_LIMIT + 5)
            .map(|index| message("session_queue0001", &format!("message {index}"), None))
            .collect::<Vec<_>>();
        let held = HeldQueue::of(&queue);
        assert_eq!(held.messages.len(), QUEUE_PREFERENCE_LIMIT);
        assert_eq!(
            held.messages.first().map(|held| held.text.as_str()),
            Some("message 5"),
            "the oldest messages were not the ones dropped"
        );

        // The bound is applied on the way back in too: the file is the
        // reader's to edit, and one edited to hold a million messages is not a
        // reason to start the run with a million.
        let mut inflated = held.clone();
        inflated.messages = (0..QUEUE_PREFERENCE_LIMIT * 2)
            .map(|index| HeldMessage {
                session_id: "session_queue0001".to_string(),
                text: format!("edited in {index}"),
                images: Vec::new(),
                mentions: Vec::new(),
                scheduled_at_ms: None,
            })
            .collect();
        assert_eq!(inflated.restore().len(), QUEUE_PREFERENCE_LIMIT);
    }

    #[test]
    fn text_longer_than_the_bound_is_cut_on_a_character_boundary() {
        let long = "日".repeat(QUEUE_TEXT_LIMIT);
        let held = HeldQueue::of(&[message("session_queue0001", &long, None)]);
        let text = &held.messages[0].text;
        assert!(text.len() <= QUEUE_TEXT_LIMIT);
        assert!(text.chars().all(|character| character == '日'));
    }

    #[test]
    fn an_entry_that_cannot_address_a_session_is_dropped_alone() {
        // A file an older build cannot read whole is still worth as much as it
        // can be read for: one unusable entry must not cost the reader the
        // messages around it.
        let held = HeldQueue {
            messages: vec![
                HeldMessage {
                    session_id: "not a session".to_string(),
                    text: "lost".to_string(),
                    images: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: None,
                },
                HeldMessage {
                    session_id: "session_queue0001".to_string(),
                    text: "kept".to_string(),
                    images: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: Some(1_800_000_000_000),
                },
            ],
        };
        let restored = held.restore();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].text, "kept");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_empty_queue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-queue.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(HeldQueue::load(Some(&path)).is_empty());
        assert!(HeldQueue::load(Some(&directory.path().join("absent.json"))).is_empty());
        assert!(HeldQueue::load(None).is_empty());
    }

    /// A 1x1 PNG, so the preview has something real to decode.
    fn small_png() -> Vec<u8> {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        PNG.to_vec()
    }
}
