#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownLimits {
    pub max_source_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_resources: usize,
    pub max_diagnostics: usize,
    pub max_code_bytes: usize,
    pub max_artifacts: usize,
    pub max_artifact_source_bytes: usize,
    pub max_concurrent_artifacts: usize,
    pub max_artifact_queue: usize,
    pub max_artifact_cache_entries: usize,
    pub max_artifact_cache_bytes: usize,
    pub artifact_timeout_ms: u64,
    pub max_svg_bytes: usize,
    pub max_svg_elements: usize,
    pub max_svg_depth: usize,
    pub max_svg_dimension: u32,
    pub max_svg_pixels: u64,
    pub max_svg_text_bytes: usize,
    pub max_svg_path_bytes: usize,
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
            max_artifacts: 64,
            max_artifact_source_bytes: 128 * 1024,
            max_concurrent_artifacts: 2,
            max_artifact_queue: 128,
            max_artifact_cache_entries: 64,
            max_artifact_cache_bytes: 32 * 1024 * 1024,
            artifact_timeout_ms: 5_000,
            max_svg_bytes: 4 * 1024 * 1024,
            max_svg_elements: 50_000,
            max_svg_depth: 128,
            max_svg_dimension: 16_384,
            max_svg_pixels: 4 * 1024 * 1024,
            max_svg_text_bytes: 1024 * 1024,
            max_svg_path_bytes: 2 * 1024 * 1024,
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
