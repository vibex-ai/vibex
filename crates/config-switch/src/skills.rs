use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

use vibex_core::{AgentId, WorkspaceId};
use vibex_db::{WorkspaceRepository, apply_migrations, open_database};

use crate::ProviderConfigService;

const LOCAL_SKILL_SCAN_DEPTH: usize = 6;
const LOCAL_SKILL_READ_LIMIT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct LocalSkillRoot {
    pub path: PathBuf,
    pub source: String,
    pub source_agent_id: AgentId,
}

#[derive(Debug, Clone)]
pub struct LocalSkillEntry {
    pub manifest_path: PathBuf,
    pub root_source: String,
    pub source_agent_id: AgentId,
    pub name: Option<String>,
    pub description: Option<String>,
    pub command_name: String,
    pub source_hash: String,
    pub content_preview: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LocalSkillMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LocalSkillScanRequest {
    pub source_agent_id: Option<AgentId>,
    pub workspace_id: Option<WorkspaceId>,
}

impl ProviderConfigService {
    pub fn scan_local_skills(
        &self,
        request: LocalSkillScanRequest,
    ) -> vibex_core::VibexResult<Vec<LocalSkillEntry>> {
        let mut entries = Vec::new();
        for root in self.local_skill_roots(&request)? {
            collect_local_skill_entries(&root, &mut entries);
        }
        entries.sort_by(|left, right| {
            left.command_name
                .cmp(&right.command_name)
                .then_with(|| left.manifest_path.cmp(&right.manifest_path))
        });
        Ok(entries)
    }

    fn local_skill_roots(
        &self,
        request: &LocalSkillScanRequest,
    ) -> vibex_core::VibexResult<Vec<LocalSkillRoot>> {
        let mut roots = Vec::new();
        let mut seen = HashSet::new();
        for agent in self.import_scan_agents(request.source_agent_id.clone())? {
            for skill_root in crate::import_scan_agent_skill_roots(&agent) {
                push_local_skill_root(
                    &mut roots,
                    &mut seen,
                    skill_root,
                    format!("agent:{}", agent.id.as_str()),
                    agent.id.clone(),
                );
            }
        }

        if let Some(workspace_id) = &request.workspace_id {
            let mut conn = open_database(self.database_path())?;
            apply_migrations(&mut conn)?;
            let (_, workspace) =
                WorkspaceRepository::get(&conn, workspace_id)?.ok_or_else(|| {
                    vibex_core::VibexError::validation(
                        "workspace_not_found",
                        "workspace was not found",
                    )
                    .with_diagnostic("workspaceId", workspace_id.as_str())
                })?;
            push_local_skill_root(
                &mut roots,
                &mut seen,
                PathBuf::from(workspace.root_path)
                    .join(".agents")
                    .join("skills"),
                "workspace_agents",
                AgentId::parse("claude")?,
            );
        }

        Ok(roots)
    }
}

pub fn parse_local_skill_metadata(content: &str) -> LocalSkillMetadata {
    let mut metadata = LocalSkillMetadata::default();
    if let Some(frontmatter) = local_skill_frontmatter(content) {
        for line in frontmatter.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = clean_local_skill_metadata_value(value);
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "name" | "display_name" | "title" => metadata.name = Some(value),
                "description" => metadata.description = Some(value),
                _ => {}
            }
        }
    }

    if metadata.name.is_none() {
        metadata.name = content.lines().find_map(|line| {
            line.trim()
                .strip_prefix("# ")
                .map(str::trim)
                .filter(|heading| !heading.is_empty())
                .map(ToString::to_string)
        });
    }

    metadata
}

pub fn command_token_from_skill_name(value: &str) -> String {
    let mut token = String::new();
    let mut last_was_separator = false;
    for ch in value.trim().trim_start_matches('$').chars() {
        if ch.is_whitespace() {
            if !token.is_empty() && !last_was_separator {
                token.push('-');
                last_was_separator = true;
            }
            continue;
        }
        if ch == '/' || ch.is_control() {
            continue;
        }
        token.extend(ch.to_lowercase());
        last_was_separator = false;
    }
    let token = token.trim_matches('-').to_string();
    if token.is_empty() {
        "skill".to_string()
    } else {
        token
    }
}

pub fn stable_hash_hex(value: impl AsRef<str>) -> String {
    let mut hasher = StableHasher::default();
    value.as_ref().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn push_local_skill_root(
    roots: &mut Vec<LocalSkillRoot>,
    seen: &mut HashSet<String>,
    path: PathBuf,
    source: impl Into<String>,
    source_agent_id: AgentId,
) {
    let key = path.display().to_string();
    if seen.insert(key) {
        roots.push(LocalSkillRoot {
            path,
            source: source.into(),
            source_agent_id,
        });
    }
}

fn collect_local_skill_entries(root: &LocalSkillRoot, entries: &mut Vec<LocalSkillEntry>) {
    collect_local_skill_entries_inner(root, &root.path, LOCAL_SKILL_SCAN_DEPTH, entries);
}

fn collect_local_skill_entries_inner(
    root: &LocalSkillRoot,
    directory: &Path,
    depth_remaining: usize,
    entries: &mut Vec<LocalSkillEntry>,
) {
    if depth_remaining == 0 || !directory.is_dir() {
        return;
    }
    let Ok(read_dir) = fs::read_dir(directory) else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file()
            && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
        {
            if let Some(skill) = local_skill_entry(root, &path) {
                entries.push(skill);
            }
            continue;
        }
        if file_type.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(should_descend_local_skill_dir)
        {
            collect_local_skill_entries_inner(root, &path, depth_remaining - 1, entries);
        }
    }
}

fn should_descend_local_skill_dir(name: &str) -> bool {
    !matches!(
        name,
        ".git" | "node_modules" | "target" | "references" | "assets" | "scripts"
    )
}

fn local_skill_entry(root: &LocalSkillRoot, path: &Path) -> Option<LocalSkillEntry> {
    let content = read_local_skill_content(path);
    let metadata = content
        .as_deref()
        .map(parse_local_skill_metadata)
        .unwrap_or_default();
    let fallback_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("skill");
    let command_name =
        command_token_from_skill_name(metadata.name.as_deref().unwrap_or(fallback_name));
    let manifest_path = path.display().to_string();

    Some(LocalSkillEntry {
        manifest_path: path.to_path_buf(),
        root_source: root.source.clone(),
        source_agent_id: root.source_agent_id.clone(),
        name: metadata.name,
        description: metadata.description,
        command_name,
        source_hash: stable_hash_hex(&manifest_path),
        content_preview: content.map(|value| value.chars().take(2048).collect()),
    })
}

fn read_local_skill_content(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut content = String::new();
    file.by_ref()
        .take(LOCAL_SKILL_READ_LIMIT_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    Some(content)
}

fn local_skill_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn clean_local_skill_metadata_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_local_skill_frontmatter_metadata() {
        let metadata = parse_local_skill_metadata(
            r#"---
name: "openai-docs"
description: "Use OpenAI docs for current product guidance."
---

# Ignored Heading
"#,
        );

        assert_eq!(metadata.name.as_deref(), Some("openai-docs"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("Use OpenAI docs for current product guidance.")
        );
    }

    #[test]
    fn falls_back_to_first_heading_for_local_skill_name() {
        let metadata = parse_local_skill_metadata("# Andrej Karpathy Perspective\n\nBody");

        assert_eq!(
            command_token_from_skill_name(metadata.name.as_deref().unwrap()),
            "andrej-karpathy-perspective"
        );
    }

    #[test]
    fn scans_nested_local_skill_manifests() {
        let root_path = unique_temp_dir("vibex-local-skill-scan");
        let skill_dir = root_path.join("pack").join("examples").join("demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: "demo-skill"
description: "Nested demo skill."
---
"#,
        )
        .unwrap();

        let root = LocalSkillRoot {
            path: root_path.clone(),
            source: "test".to_string(),
            source_agent_id: AgentId::parse("codex").unwrap(),
        };
        let mut entries = Vec::new();
        collect_local_skill_entries(&root, &mut entries);

        assert!(entries.iter().any(|entry| {
            entry.command_name == "demo-skill"
                && entry.description.as_deref() == Some("Nested demo skill.")
                && entry.source_agent_id.as_str() == "codex"
        }));

        fs::remove_dir_all(root_path).unwrap();
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
