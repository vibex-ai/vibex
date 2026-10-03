//! Attachments as the transcript draws them.
//!
//! A message is not only its text. The composer's `[Image #1]` is a draft
//! surface — the Agent is never told about a placeholder it cannot see — so the
//! label is dropped on the way out and `inline_text_offset` (UTF-16 units into
//! the *sent* text) is all that still says where the picture sat. A client that
//! leaves it out loses the place, and the picture lands at the end of the
//! paragraph: a different message. The terminal can draw no pixels, but it can
//! put the placeholder back where the reader wrote it.

use vibex_core::MessageAttachment;

use crate::composer::utf16_offset_to_byte;
use crate::locale::Strings;

/// A message body with every attachment drawn as a placeholder where it sat.
///
/// The text is otherwise untouched: this is the same message the wire carries,
/// with the places its attachments named written back in. `strings` only words
/// a placeholder for an attachment that arrived without a label of its own.
pub fn with_attachments(text: &str, attachments: &[MessageAttachment], strings: Strings) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    // The offsets are the wire's, so they are sorted rather than trusted: a
    // message can carry several, and the text is walked once, left to right.
    let mut placed = attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            attachment
                .inline_text_offset
                .map(|offset| (offset as usize, index, attachment))
        })
        .collect::<Vec<_>>();
    placed.sort_by_key(|(offset, index, _)| (*offset, *index));

    let mut body = String::with_capacity(text.len() + attachments.len() * 12);
    let mut cursor = 0usize;
    for (offset, _, attachment) in placed {
        let byte = utf16_offset_to_byte(text, offset).clamp(cursor, text.len());
        body.push_str(&text[cursor..byte]);
        cursor = byte;
        push_word(&mut body, &placeholder(attachment, strings));
        // What follows is appended after this, and a terminal draws the
        // placeholder as prose: without a space of its own it fuses with the
        // next word (`[Image #1]after`).
        if text[cursor..]
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace())
        {
            body.push(' ');
        }
    }
    body.push_str(&text[cursor..]);
    // An offset is optional on the wire, and one that carries none has no place
    // to go back to: it is appended in the order it was sent, which is where a
    // reader that cannot place a picture has always found it.
    for attachment in attachments
        .iter()
        .filter(|attachment| attachment.inline_text_offset.is_none())
    {
        push_word(&mut body, &placeholder(attachment, strings));
    }
    body
}

/// Append a placeholder, one word apart from whatever it follows.
///
/// The wire text separates a picture from its neighbours the way the client
/// that wrote it did — a TUI draft's own spaces survive verbatim, and a desktop
/// chip's padding was consumed by the submission — so the space on the left is
/// supplied here when the text before it does not already end in whitespace.
fn push_word(body: &mut String, placeholder: &str) {
    if !body.is_empty() && !body.ends_with(char::is_whitespace) {
        body.push(' ');
    }
    body.push_str(placeholder);
}

/// The words drawn where an attachment sat.
///
/// The reader saw `[Image #1]` while writing, and a message written on the
/// desktop carries the file's name instead; either way the label is what names
/// the attachment, so it is what the reader gets back. A label that is already
/// bracketed is a placeholder already and is never bracketed twice, and an
/// attachment with no label at all is still named.
fn placeholder(attachment: &MessageAttachment, strings: Strings) -> String {
    let label = attachment.label.trim();
    if label.is_empty() {
        let word = if is_image(attachment) {
            strings.transcript_image()
        } else {
            strings.composer_attachments()
        };
        return format!("[{word}]");
    }
    if label.starts_with('[') && label.ends_with(']') && label.len() > 2 {
        label.to_string()
    } else {
        format!("[{label}]")
    }
}

/// Whether an attachment names a picture, by media type or by its own name.
///
/// Only consulted to word a placeholder for an attachment that arrived without
/// a label: one that has a label says what it is.
fn is_image(attachment: &MessageAttachment) -> bool {
    if attachment
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.trim().to_ascii_lowercase().starts_with("image/"))
    {
        return true;
    }
    let Some(uri) = attachment.uri.as_deref() else {
        return false;
    };
    if uri.to_ascii_lowercase().starts_with("data:image/") {
        return true;
    }
    crate::composer::image_mime_for_path(uri).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(label: &str, offset: Option<u32>) -> MessageAttachment {
        MessageAttachment {
            label: label.to_string(),
            mime_type: Some("image/png".to_string()),
            uri: Some("file:///tmp/shot.png".to_string()),
            inline_text_offset: offset,
        }
    }

    fn strings() -> Strings {
        Strings::for_locale(crate::locale::Locale::En)
    }

    #[test]
    fn a_placeholder_goes_back_where_the_label_sat() {
        // A TUI draft's spaces survive the send, so the offset lands between
        // them and the message reads exactly as it was written.
        let body = with_attachments("before  after", &[image("[Image #1]", Some(7))], strings());
        assert_eq!(body, "before [Image #1] after");
    }

    #[test]
    fn an_attachment_is_one_word_apart_from_its_neighbours() {
        // A desktop chip's padding was consumed by the submission, so neither
        // side supplies the space the placeholder needs.
        let body = with_attachments("beforeafter", &[image("[Image #1]", Some(6))], strings());
        assert_eq!(body, "before [Image #1] after");
    }

    #[test]
    fn an_offset_is_counted_in_utf16_units() {
        // The offset counts what the other clients count. A CJK message is
        // where UTF-16 units and bytes disagree, and guessing bytes puts the
        // picture a character off.
        let body = with_attachments("你好 世界", &[image("[Image #1]", Some(3))], strings());
        assert_eq!(body, "你好 [Image #1] 世界");
    }

    #[test]
    fn an_attachment_without_an_offset_is_appended() {
        let body = with_attachments("look at this", &[image("", None)], strings());
        assert_eq!(body, "look at this [Image]");
    }

    #[test]
    fn a_label_that_is_already_a_placeholder_is_not_bracketed_twice() {
        let body = with_attachments("hi", &[image("[Image #2]", None)], strings());
        assert_eq!(body, "hi [Image #2]");
    }

    #[test]
    fn a_file_name_becomes_a_placeholder_of_its_own() {
        let file = MessageAttachment {
            label: "notes.txt".to_string(),
            mime_type: Some("text/plain".to_string()),
            uri: Some("file:///tmp/notes.txt".to_string()),
            inline_text_offset: None,
        };
        let body = with_attachments("read this", &[file], strings());
        assert_eq!(body, "read this [notes.txt]");

        let unnamed = MessageAttachment {
            label: String::new(),
            mime_type: Some("application/pdf".to_string()),
            uri: None,
            inline_text_offset: None,
        };
        assert_eq!(
            with_attachments("see", &[unnamed], strings()),
            "see [Attachments]"
        );
    }

    #[test]
    fn a_message_with_no_attachments_is_left_alone() {
        assert_eq!(with_attachments("plain text", &[], strings()), "plain text");
    }

    #[test]
    fn attachments_keep_their_order_and_their_places() {
        let body = with_attachments(
            "a  b  c",
            &[image("[Image #2]", Some(5)), image("[Image #1]", Some(2))],
            strings(),
        );
        assert_eq!(body, "a [Image #1] b [Image #2] c");
    }
}
