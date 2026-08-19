use serde::{Deserialize, Serialize};

use crate::ids::WorkspaceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePreviewKind {
    Text,
    Markdown,
    Image,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileEncoding {
    #[default]
    Utf8,
    Utf8Bom,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileLineEnding {
    #[default]
    None,
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeRequest {
    pub workspace_id: WorkspaceId,
    pub path: Option<String>,
    pub max_depth: Option<u32>,
    pub include_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeEntry {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub name: String,
    pub parent_path: Option<String>,
    pub kind: FileEntryKind,
    pub size_bytes: Option<u64>,
    pub modified_at_ms: Option<i64>,
    pub hidden: bool,
    pub ignored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadRequest {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadResponse {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub name: String,
    pub preview_kind: FilePreviewKind,
    pub content: Option<String>,
    pub size_bytes: u64,
    pub modified_at_ms: Option<i64>,
    pub language: Option<String>,
    pub truncated: bool,
    #[serde(default)]
    pub encoding: FileEncoding,
    #[serde(default)]
    pub line_ending: FileLineEnding,
    #[serde(default)]
    pub content_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWriteRequest {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub content: String,
    pub create_if_missing: bool,
    #[serde(default)]
    pub expected_revision: Option<String>,
    #[serde(default)]
    pub encoding: Option<FileEncoding>,
    #[serde(default)]
    pub line_ending: Option<FileLineEnding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMutationRequest {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub new_path: Option<String>,
    pub recursive: bool,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchRequest {
    pub workspace_id: WorkspaceId,
    pub query: String,
    pub include_content: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub regex: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub name: String,
    pub kind: FileEntryKind,
    pub line: Option<u32>,
    pub snippet: Option<String>,
    #[serde(default)]
    pub match_start: Option<u32>,
    #[serde(default)]
    pub match_end: Option<u32>,
    #[serde(default)]
    pub matched_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_file_search_payloads_default_new_match_fields() {
        let request: FileSearchRequest = serde_json::from_value(serde_json::json!({
            "workspaceId": "workspace_legacy",
            "query": "needle",
            "includeContent": true,
            "limit": 20
        }))
        .unwrap();
        assert!(!request.case_sensitive);
        assert!(!request.whole_word);
        assert!(!request.regex);

        let result: FileSearchResult = serde_json::from_value(serde_json::json!({
            "workspaceId": "workspace_legacy",
            "path": "src/lib.rs",
            "name": "lib.rs",
            "kind": "file",
            "line": 1,
            "snippet": "needle"
        }))
        .unwrap();
        assert_eq!(result.match_start, None);
        assert_eq!(result.match_end, None);
        assert_eq!(result.matched_text, None);
    }
}
