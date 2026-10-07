use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use vibex_core::{FilePreviewKind, FileReadResponse};

pub const IMAGE_MAX_DIMENSION: u32 = 16_384;
pub const IMAGE_MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;
pub const IMAGE_CACHE_ITEM_LIMIT: usize = 32;
pub const IMAGE_CACHE_BYTE_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPreviewKind {
    TextEditor,
    Markdown,
    Image,
    MediaExternalOnly,
    Pdf,
    Office,
    UnsupportedBinary,
}

pub fn content_preview_kind(file: &FileReadResponse) -> ContentPreviewKind {
    let path_kind = content_preview_kind_for_path(&file.path);
    match file.preview_kind {
        FilePreviewKind::Text => match path_kind {
            ContentPreviewKind::Markdown | ContentPreviewKind::Pdf | ContentPreviewKind::Office => {
                path_kind
            }
            _ => ContentPreviewKind::TextEditor,
        },
        FilePreviewKind::Markdown => ContentPreviewKind::Markdown,
        FilePreviewKind::Image => ContentPreviewKind::Image,
        FilePreviewKind::Binary => match path_kind {
            ContentPreviewKind::MediaExternalOnly
            | ContentPreviewKind::Pdf
            | ContentPreviewKind::Office => path_kind,
            _ => ContentPreviewKind::UnsupportedBinary,
        },
    }
}

pub fn content_preview_kind_for_path(path: &str) -> ContentPreviewKind {
    match extension(path).as_deref() {
        Some("md" | "mdx" | "markdown") => ContentPreviewKind::Markdown,
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp") => ContentPreviewKind::Image,
        Some(
            "aac" | "avi" | "flac" | "m4a" | "mkv" | "mov" | "mp3" | "mp4" | "ogg" | "wav" | "webm",
        ) => ContentPreviewKind::MediaExternalOnly,
        Some("pdf") => ContentPreviewKind::Pdf,
        Some("doc" | "docx" | "xls" | "xlsx" | "ods" | "ppt" | "pptx") => {
            ContentPreviewKind::Office
        }
        _ => ContentPreviewKind::TextEditor,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCacheKey {
    pub path: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCacheEntry {
    pub width: u32,
    pub height: u32,
    pub decoded_bytes: usize,
    pub last_used_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageCacheInsertError {
    DimensionsInvalid,
    ItemBudgetExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundedImageCache {
    entries: BTreeMap<ImageCacheKey, ImageCacheEntry>,
    byte_limit: usize,
    item_limit: usize,
    resident_bytes: usize,
    epoch: u64,
    evictions: u64,
}

impl Default for BoundedImageCache {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            byte_limit: IMAGE_CACHE_BYTE_LIMIT,
            item_limit: IMAGE_CACHE_ITEM_LIMIT,
            resident_bytes: 0,
            epoch: 0,
            evictions: 0,
        }
    }
}

impl BoundedImageCache {
    pub fn with_budget(item_limit: usize, byte_limit: usize) -> Self {
        Self {
            item_limit: item_limit.max(1),
            byte_limit: byte_limit.max(1),
            ..Self::default()
        }
    }

    pub fn insert(
        &mut self,
        key: ImageCacheKey,
        width: u32,
        height: u32,
        decoded_bytes: usize,
    ) -> Result<Vec<ImageCacheKey>, ImageCacheInsertError> {
        if width == 0 || height == 0 || width > IMAGE_MAX_DIMENSION || height > IMAGE_MAX_DIMENSION
        {
            return Err(ImageCacheInsertError::DimensionsInvalid);
        }
        if decoded_bytes == 0
            || decoded_bytes > IMAGE_MAX_DECODED_BYTES
            || decoded_bytes > self.byte_limit
        {
            return Err(ImageCacheInsertError::ItemBudgetExceeded);
        }
        self.epoch = self.epoch.saturating_add(1).max(1);
        if let Some(previous) = self.entries.remove(&key) {
            self.resident_bytes = self.resident_bytes.saturating_sub(previous.decoded_bytes);
        }
        self.entries.insert(
            key.clone(),
            ImageCacheEntry {
                width,
                height,
                decoded_bytes,
                last_used_epoch: self.epoch,
            },
        );
        self.resident_bytes = self.resident_bytes.saturating_add(decoded_bytes);
        let mut evicted = Vec::new();
        while self.entries.len() > self.item_limit || self.resident_bytes > self.byte_limit {
            let Some(candidate) = self
                .entries
                .iter()
                .filter(|(candidate, _)| *candidate != &key)
                .min_by_key(|(_, entry)| entry.last_used_epoch)
                .map(|(candidate, _)| candidate.clone())
            else {
                self.entries.remove(&key);
                self.resident_bytes = self.resident_bytes.saturating_sub(decoded_bytes);
                return Err(ImageCacheInsertError::ItemBudgetExceeded);
            };
            if let Some(entry) = self.entries.remove(&candidate) {
                self.resident_bytes = self.resident_bytes.saturating_sub(entry.decoded_bytes);
                self.evictions = self.evictions.saturating_add(1);
                evicted.push(candidate);
            }
        }
        Ok(evicted)
    }

    pub fn touch(&mut self, key: &ImageCacheKey) -> Option<&ImageCacheEntry> {
        self.epoch = self.epoch.saturating_add(1).max(1);
        let entry = self.entries.get_mut(key)?;
        entry.last_used_epoch = self.epoch;
        self.entries.get(key)
    }

    pub fn remove(&mut self, key: &ImageCacheKey) -> bool {
        let Some(entry) = self.entries.remove(key) else {
            return false;
        };
        self.resident_bytes = self.resident_bytes.saturating_sub(entry.decoded_bytes);
        true
    }

    pub fn resident_items(&self) -> usize {
        self.entries.len()
    }

    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }
}

fn extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_file(
        path: &str,
        preview_kind: FilePreviewKind,
        content: Option<&str>,
    ) -> FileReadResponse {
        FileReadResponse {
            workspace_id: vibex_core::WorkspaceId::new(),
            path: path.to_string(),
            name: Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path)
                .to_string(),
            preview_kind,
            content: content.map(str::to_string),
            size_bytes: content.map_or(16, |content| content.len() as u64),
            modified_at_ms: Some(1),
            language: None,
            truncated: false,
            encoding: if preview_kind == FilePreviewKind::Binary {
                vibex_core::FileEncoding::Binary
            } else {
                vibex_core::FileEncoding::Utf8
            },
            line_ending: if preview_kind == FilePreviewKind::Binary {
                vibex_core::FileLineEnding::None
            } else {
                vibex_core::FileLineEnding::Lf
            },
            content_revision: "r1".to_string(),
        }
    }

    #[test]
    fn image_cache_rejects_oversize_and_evicts_lru() {
        let mut cache = BoundedImageCache::with_budget(2, 100);
        let key = |path: &str| ImageCacheKey {
            path: path.into(),
            revision: "r1".into(),
        };
        cache.insert(key("a"), 1, 1, 40).unwrap();
        cache.insert(key("b"), 1, 1, 40).unwrap();
        cache.touch(&key("a"));
        let evicted = cache.insert(key("c"), 1, 1, 40).unwrap();
        assert_eq!(evicted, vec![key("b")]);
        assert_eq!(cache.resident_items(), 2);
        assert_eq!(
            cache.insert(key("huge"), IMAGE_MAX_DIMENSION + 1, 1, 4),
            Err(ImageCacheInsertError::DimensionsInvalid)
        );
    }

    #[test]
    fn content_routes_preserve_native_and_external_boundaries() {
        assert_eq!(
            content_preview_kind_for_path("a.pdf"),
            ContentPreviewKind::Pdf
        );
        assert_eq!(
            content_preview_kind_for_path("a.docx"),
            ContentPreviewKind::Office
        );
        assert_eq!(
            content_preview_kind_for_path("a.mp4"),
            ContentPreviewKind::MediaExternalOnly
        );
        assert_eq!(
            content_preview_kind_for_path("a.svg"),
            ContentPreviewKind::Image
        );
    }

    #[test]
    fn text_preview_routes_match_the_tauri_open_range() {
        for path in [
            ".env",
            "Dockerfile",
            "Makefile",
            "src/main.c",
            "schema.xml",
            "query.sql",
            "settings.ini",
            "NOTICE",
            "notes.unknown",
        ] {
            assert_eq!(
                content_preview_kind_for_path(path),
                ContentPreviewKind::TextEditor,
                "{path} should be read before its preview kind is decided"
            );
            assert_eq!(
                content_preview_kind(&preview_file(path, FilePreviewKind::Text, Some("text"))),
                ContentPreviewKind::TextEditor,
                "{path} should open in the text editor"
            );
        }
    }

    #[test]
    fn service_binary_result_wins_over_text_like_suffixes() {
        for path in ["archive.unknown", "config.json", "README"] {
            assert_eq!(
                content_preview_kind(&preview_file(path, FilePreviewKind::Binary, None)),
                ContentPreviewKind::UnsupportedBinary,
                "{path} should remain a binary preview"
            );
        }
    }

    #[test]
    fn service_and_path_hints_preserve_markdown_and_svg_previews() {
        assert_eq!(
            content_preview_kind(&preview_file(
                "README.MD",
                FilePreviewKind::Text,
                Some("# Readme")
            )),
            ContentPreviewKind::Markdown
        );
        assert_eq!(
            content_preview_kind(&preview_file("logo.svg", FilePreviewKind::Image, None)),
            ContentPreviewKind::Image
        );
    }
}
