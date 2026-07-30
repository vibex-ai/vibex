use std::ops::Range;

use serde::{Deserialize, Serialize};
use vibex_core::MessageAttachment;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerAttachment {
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub mime_type: Option<String>,
}

impl ComposerAttachment {
    pub fn is_image(&self) -> bool {
        self.mime_type
            .as_deref()
            .is_some_and(|mime_type| mime_type.starts_with("image/"))
            || self.path.as_deref().is_some_and(|path| {
                matches!(
                    path.rsplit('.')
                        .next()
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some(
                        "png"
                            | "jpg"
                            | "jpeg"
                            | "gif"
                            | "webp"
                            | "svg"
                            | "bmp"
                            | "tif"
                            | "tiff"
                            | "ico"
                            | "pbm"
                            | "pgm"
                            | "ppm"
                            | "pnm",
                    )
                )
            })
    }

    pub fn markdown_reference(&self) -> Option<String> {
        let path = self.path.as_deref().filter(|_| self.is_image())?;
        let label = self
            .label
            .replace('\\', "\\\\")
            .replace('[', "\\[")
            .replace(']', "\\]");
        let encoded_path = path
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
            .replace('(', "%28")
            .replace(')', "%29");
        Some(format!("![{label}](file://{encoded_path})"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComposerNode {
    Text { text: String },
    Attachment { attachment: ComposerAttachment },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerDocument {
    pub nodes: Vec<ComposerNode>,
}

impl ComposerDocument {
    pub fn normalize(&mut self) {
        let mut nodes = Vec::new();
        for node in std::mem::take(&mut self.nodes) {
            match node {
                ComposerNode::Text { text } if text.is_empty() => {}
                ComposerNode::Text { text } => {
                    if let Some(ComposerNode::Text { text: previous }) = nodes.last_mut() {
                        previous.push_str(&text);
                    } else {
                        nodes.push(ComposerNode::Text { text });
                    }
                }
                ComposerNode::Attachment { attachment }
                    if attachment.id.trim().is_empty() || attachment.label.trim().is_empty() => {}
                ComposerNode::Attachment { mut attachment } => {
                    attachment.id = attachment.id.trim().to_string();
                    attachment.label = attachment.label.trim().to_string();
                    attachment.path = attachment
                        .path
                        .take()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty());
                    attachment.mime_type = attachment
                        .mime_type
                        .take()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty());
                    nodes.push(ComposerNode::Attachment { attachment });
                }
            }
        }
        self.nodes = nodes;
    }

    pub fn text(&self) -> String {
        self.nodes
            .iter()
            .filter_map(|node| match node {
                ComposerNode::Text { text } => Some(text.as_str()),
                ComposerNode::Attachment { .. } => None,
            })
            .collect()
    }

    pub fn attachments(&self) -> impl Iterator<Item = &ComposerAttachment> {
        self.nodes.iter().filter_map(|node| match node {
            ComposerNode::Attachment { attachment } => Some(attachment),
            ComposerNode::Text { .. } => None,
        })
    }

    pub fn is_sendable(&self) -> bool {
        !self.text().trim().is_empty() || self.attachments().next().is_some()
    }

    pub fn remove_attachment(&mut self, attachment_id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|node| {
            !matches!(
                node,
                ComposerNode::Attachment { attachment } if attachment.id == attachment_id
            )
        });
        before != self.nodes.len()
    }

    pub fn message_attachments(&self) -> Vec<MessageAttachment> {
        self.attachments()
            .map(|attachment| MessageAttachment {
                label: attachment.label.clone(),
                mime_type: attachment.mime_type.clone(),
                uri: attachment
                    .path
                    .as_ref()
                    .map(|path| format!("file://{path}")),
                inline_text_offset: None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerState {
    pub document: ComposerDocument,
    pub composition_active: bool,
    pub submitting: bool,
    pub submission_generation: u64,
    pub last_error: Option<String>,
}

impl ComposerState {
    pub fn can_submit(&self) -> bool {
        self.document.is_sendable() && !self.composition_active && !self.submitting
    }

    pub fn begin_submission(&mut self) -> Option<(u64, ComposerDocument)> {
        if !self.can_submit() {
            return None;
        }
        self.submission_generation = self.submission_generation.saturating_add(1);
        self.submitting = true;
        self.last_error = None;
        Some((self.submission_generation, self.document.clone()))
    }

    pub fn accept_submission(&mut self, generation: u64) -> bool {
        if !self.submitting || generation != self.submission_generation {
            return false;
        }
        self.document = ComposerDocument::default();
        self.submitting = false;
        true
    }

    pub fn fail_submission(&mut self, generation: u64, error: impl Into<String>) -> bool {
        if !self.submitting || generation != self.submission_generation {
            return false;
        }
        self.submitting = false;
        self.last_error = Some(error.into());
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerTriggerKind {
    Command,
    File,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerTrigger {
    pub kind: ComposerTriggerKind,
    pub query: String,
    pub character_range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerSuggestionSelection {
    pub item_count: usize,
    pub selected_index: Option<usize>,
}

impl ComposerSuggestionSelection {
    pub fn replace_items(&mut self, item_count: usize) {
        self.item_count = item_count;
        self.selected_index = match (item_count, self.selected_index) {
            (0, _) => None,
            (_, Some(index)) => Some(index.min(item_count - 1)),
            (_, None) => Some(0),
        };
    }

    pub fn select_next(&mut self) {
        if self.item_count == 0 {
            self.selected_index = None;
            return;
        }
        self.selected_index = Some(
            self.selected_index
                .map(|index| (index + 1) % self.item_count)
                .unwrap_or(0),
        );
    }

    pub fn select_previous(&mut self) {
        if self.item_count == 0 {
            self.selected_index = None;
            return;
        }
        self.selected_index = Some(
            self.selected_index
                .map(|index| index.checked_sub(1).unwrap_or(self.item_count - 1))
                .unwrap_or(self.item_count - 1),
        );
    }

    pub fn dismiss(&mut self) {
        self.item_count = 0;
        self.selected_index = None;
    }
}

pub fn composer_trigger_at(text: &str, caret_character: usize) -> Option<ComposerTrigger> {
    let characters = text.chars().collect::<Vec<_>>();
    if caret_character > characters.len() {
        return None;
    }
    let start = characters[..caret_character]
        .iter()
        .rposition(|character| matches!(character, ' ' | '\n' | '\t' | '\u{200b}'))
        .map_or(0, |index| index + 1);
    if start >= caret_character {
        return None;
    }
    let kind = match characters[start] {
        '/' => ComposerTriggerKind::Command,
        '@' => ComposerTriggerKind::File,
        '$' => ComposerTriggerKind::Skill,
        _ => return None,
    };
    let query = characters[start + 1..caret_character]
        .iter()
        .collect::<String>();
    let repeated_trigger = match kind {
        ComposerTriggerKind::Command => '/',
        ComposerTriggerKind::File => '@',
        ComposerTriggerKind::Skill => '$',
    };
    (!query.contains(repeated_trigger)).then_some(ComposerTrigger {
        kind,
        query,
        character_range: start..caret_character,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_is_a_document_node_not_a_text_placeholder() {
        let mut document = ComposerDocument {
            nodes: vec![
                ComposerNode::Text {
                    text: "Review ".into(),
                },
                ComposerNode::Attachment {
                    attachment: ComposerAttachment {
                        id: " file-1 ".into(),
                        label: " lib.rs ".into(),
                        path: Some(" src/lib.rs ".into()),
                        mime_type: None,
                    },
                },
                ComposerNode::Text {
                    text: " please".into(),
                },
            ],
        };
        document.normalize();
        assert_eq!(document.text(), "Review  please");
        assert_eq!(document.attachments().next().unwrap().id, "file-1");
        assert!(!document.text().contains('\u{fffc}'));
    }

    #[test]
    fn trigger_uses_character_offsets_for_unicode_text() {
        let text = "修复 @src/文件";
        let trigger = composer_trigger_at(text, text.chars().count()).unwrap();
        assert_eq!(trigger.kind, ComposerTriggerKind::File);
        assert_eq!(trigger.query, "src/文件");
        assert_eq!(
            &text.chars().collect::<Vec<_>>()[trigger.character_range],
            &text.chars().collect::<Vec<_>>()[3..]
        );
    }

    #[test]
    fn trigger_rejects_repeated_trigger_character() {
        assert!(composer_trigger_at("/review/", "/review/".chars().count()).is_none());
        assert!(composer_trigger_at("@src/@test", "@src/@test".chars().count()).is_none());
        assert!(composer_trigger_at("$rust$quality", "$rust$quality".chars().count()).is_none());
        assert!(composer_trigger_at("/@file", "/@file".chars().count()).is_some());
    }

    #[test]
    fn trigger_uses_the_same_token_boundaries_as_tauri() {
        let text = "before\u{200b}@src";
        let trigger = composer_trigger_at(text, text.chars().count()).unwrap();
        assert_eq!(trigger.query, "src");

        let text = "before\r@src";
        assert!(composer_trigger_at(text, text.chars().count()).is_none());
    }

    #[test]
    fn failed_submission_preserves_text_and_attachments_until_acceptance() {
        let mut state = ComposerState {
            document: ComposerDocument {
                nodes: vec![
                    ComposerNode::Text {
                        text: "review".into(),
                    },
                    ComposerNode::Attachment {
                        attachment: ComposerAttachment {
                            id: "image-1".into(),
                            label: "screen.png".into(),
                            path: Some("/tmp/screen.png".into()),
                            mime_type: Some("image/png".into()),
                        },
                    },
                ],
            },
            ..Default::default()
        };
        let (generation, _) = state.begin_submission().unwrap();
        assert!(state.fail_submission(generation, "switch failed"));
        assert_eq!(state.document.text(), "review");
        assert_eq!(state.document.attachments().count(), 1);

        let (generation, _) = state.begin_submission().unwrap();
        assert!(state.accept_submission(generation));
        assert!(!state.document.is_sendable());
    }

    #[test]
    fn suggestion_selection_wraps_and_survives_bounded_refresh() {
        let mut selection = ComposerSuggestionSelection::default();
        selection.replace_items(3);
        assert_eq!(selection.selected_index, Some(0));
        selection.select_previous();
        assert_eq!(selection.selected_index, Some(2));
        selection.select_next();
        assert_eq!(selection.selected_index, Some(0));
        selection.select_next();
        selection.select_next();
        selection.replace_items(2);
        assert_eq!(selection.selected_index, Some(1));
        selection.dismiss();
        assert_eq!(selection.selected_index, None);
    }

    #[test]
    fn image_attachment_projects_safe_markdown_reference() {
        let attachment = ComposerAttachment {
            id: "image-1".into(),
            label: "screen [1].png".into(),
            path: Some("/tmp/screen (1).png".into()),
            mime_type: Some("image/png".into()),
        };
        assert!(attachment.is_image());
        assert_eq!(
            attachment.markdown_reference().as_deref(),
            Some("![screen \\[1\\].png](file:///tmp/screen%20%281%29.png)")
        );
        assert!(
            ComposerAttachment {
                id: "image-2".into(),
                label: "diagram.svg".into(),
                path: Some("/tmp/diagram.svg".into()),
                mime_type: None,
            }
            .is_image()
        );
    }
}
