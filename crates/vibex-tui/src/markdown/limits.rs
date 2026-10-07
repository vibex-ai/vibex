#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownLimits {
    pub max_source_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_resources: usize,
    pub max_diagnostics: usize,
    pub max_code_bytes: usize,
}

impl Default for MarkdownLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MARKDOWN_MAX_SOURCE_BYTES,
            max_nodes: 100_000,
            max_depth: 128,
            max_resources: MARKDOWN_MAX_RESOURCES,
            max_diagnostics: 128,
            max_code_bytes: 1024 * 1024,
        }
    }
}

pub const MARKDOWN_MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const MARKDOWN_MAX_RESOURCES: usize = 256;
pub const DATA_IMAGE_MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;

pub fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
