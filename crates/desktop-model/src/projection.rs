use serde::{Deserialize, Serialize};
use vibex_core::{
    FileEntryKind, FileTreeEntry, GitChange, GitChangeKind, ProviderKind, ProviderProfile,
    ProviderProfileStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeRow {
    pub id: String,
    pub path: String,
    pub parent_path: Option<String>,
    pub label: String,
    pub kind: FileEntryKind,
    pub depth: usize,
    pub hidden: bool,
    pub ignored: bool,
}

impl From<&FileTreeEntry> for FileTreeRow {
    fn from(entry: &FileTreeEntry) -> Self {
        Self {
            id: format!("file:{}:{}", entry.workspace_id, entry.path),
            path: entry.path.clone(),
            parent_path: entry.parent_path.clone(),
            label: entry.name.clone(),
            kind: entry.kind,
            depth: entry
                .path
                .split('/')
                .filter(|part| !part.is_empty())
                .count(),
            hidden: entry.hidden,
            ignored: entry.ignored,
        }
    }
}

pub fn project_file_tree(entries: &[FileTreeEntry]) -> Vec<FileTreeRow> {
    let mut rows = entries.iter().map(FileTreeRow::from).collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let left_directory = left.kind == FileEntryKind::Directory;
        let right_directory = right.kind == FileEntryKind::Directory;
        right_directory
            .cmp(&left_directory)
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });
    rows
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChangeRow {
    pub id: String,
    pub path: String,
    pub original_path: Option<String>,
    pub kind: GitChangeKind,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl From<&GitChange> for GitChangeRow {
    fn from(change: &GitChange) -> Self {
        Self {
            id: format!(
                "git:{}:{}",
                if change.staged { "staged" } else { "unstaged" },
                change.path
            ),
            path: change.path.clone(),
            original_path: change.original_path.clone(),
            kind: change.kind,
            staged: change.staged,
            unstaged: change.unstaged,
            additions: change.additions,
            deletions: change.deletions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFormModel {
    pub id: String,
    pub agent_id: String,
    pub kind: ProviderKind,
    pub status: ProviderProfileStatus,
    pub display_name: String,
    pub account_alias: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub secret_reference_count: usize,
}

impl From<&ProviderProfile> for ProviderFormModel {
    fn from(profile: &ProviderProfile) -> Self {
        Self {
            id: profile.id.to_string(),
            agent_id: profile.agent_id.to_string(),
            kind: profile.kind,
            status: profile.status,
            display_name: profile.display_name.clone(),
            account_alias: profile.account_alias.clone(),
            base_url: profile.base_url.clone(),
            default_model: profile.default_model.clone(),
            reasoning_effort: profile.reasoning_effort.clone(),
            secret_reference_count: profile.secrets.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::WorkspaceId;

    #[test]
    fn file_rows_have_stable_ids_and_directory_first_order() {
        let workspace_id = WorkspaceId::parse("workspace_fixture").unwrap();
        let rows = project_file_tree(&[
            FileTreeEntry {
                workspace_id: workspace_id.clone(),
                path: "z.rs".into(),
                name: "z.rs".into(),
                parent_path: None,
                kind: FileEntryKind::File,
                size_bytes: Some(1),
                modified_at_ms: None,
                hidden: false,
                ignored: false,
            },
            FileTreeEntry {
                workspace_id,
                path: "src".into(),
                name: "src".into(),
                parent_path: None,
                kind: FileEntryKind::Directory,
                size_bytes: None,
                modified_at_ms: None,
                hidden: false,
                ignored: false,
            },
        ]);
        assert_eq!(rows[0].label, "src");
        assert_eq!(rows[1].id, "file:workspace_fixture:z.rs");
    }
}
