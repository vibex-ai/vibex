use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::limits::{DATA_IMAGE_MAX_ENCODED_BYTES, bounded_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Workspace,
    DataImage,
    Http,
    Fragment,
    Blocked,
}

pub type MarkdownAssetKind = ResourceKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRole {
    Image,
    Link,
}

pub type MarkdownAssetRole = ResourceRole;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedResource {
    pub role: ResourceRole,
    pub source: String,
    pub kind: ResourceKind,
    pub resolved: Option<String>,
    pub label: Option<String>,
    pub error_code: Option<String>,
}

pub type MarkdownAsset = ResolvedResource;

#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    base_path: String,
}

impl ResourcePolicy {
    pub fn new(base_path: impl AsRef<str>) -> Self {
        Self {
            base_path: normalize_base_path(base_path.as_ref()),
        }
    }

    pub fn for_file(file_path: impl AsRef<str>) -> Self {
        let base_path = Path::new(file_path.as_ref())
            .parent()
            .map(path_to_slash)
            .unwrap_or_default();
        Self::new(base_path)
    }

    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    pub fn resolve(
        &self,
        role: ResourceRole,
        source: &str,
        label: Option<&str>,
    ) -> ResolvedResource {
        let source = source.trim();
        let label = label
            .map(|label| bounded_text(label, 240))
            .filter(|label| !label.is_empty());
        if source.starts_with('#') && is_valid_fragment(source) {
            return resource(
                role,
                source,
                ResourceKind::Fragment,
                Some(source),
                label,
                None,
            );
        }
        if source.starts_with("data:") {
            let valid = role == ResourceRole::Image
                && source.len() <= DATA_IMAGE_MAX_ENCODED_BYTES
                && is_supported_data_image(source);
            return if valid {
                resource(
                    role,
                    source,
                    ResourceKind::DataImage,
                    Some(source),
                    label,
                    None,
                )
            } else {
                blocked(role, source, label, "markdown_data_url_rejected")
            };
        }
        if let Ok(url) = Url::parse(source) {
            return match url.scheme().to_ascii_lowercase().as_str() {
                "http" | "https" => resource(
                    role,
                    source,
                    ResourceKind::Http,
                    Some(url.as_str()),
                    label,
                    None,
                ),
                _ => blocked(role, source, label, "markdown_url_scheme_rejected"),
            };
        }
        match resolve_workspace_resource(&self.base_path, source) {
            Some(path) => resource(
                role,
                source,
                ResourceKind::Workspace,
                Some(&path),
                label,
                None,
            ),
            None => blocked(role, source, label, "markdown_workspace_path_rejected"),
        }
    }
}

fn blocked(
    role: ResourceRole,
    source: &str,
    label: Option<String>,
    code: &str,
) -> ResolvedResource {
    resource(role, source, ResourceKind::Blocked, None, label, Some(code))
}

fn resource(
    role: ResourceRole,
    source: &str,
    kind: ResourceKind,
    resolved: Option<&str>,
    label: Option<String>,
    error_code: Option<&str>,
) -> ResolvedResource {
    let preserve_data_image = kind == ResourceKind::DataImage;
    ResolvedResource {
        role,
        source: if preserve_data_image {
            source.to_string()
        } else {
            bounded_text(source, 2_048)
        },
        kind,
        resolved: resolved.map(|value| {
            if preserve_data_image {
                value.to_string()
            } else {
                bounded_text(value, 2_048)
            }
        }),
        label,
        error_code: error_code.map(str::to_string),
    }
}

fn normalize_base_path(base_path: &str) -> String {
    base_path
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .fold(Vec::<&str>::new(), |mut segments, segment| {
            if segment == ".." {
                segments.pop();
            } else {
                segments.push(segment);
            }
            segments
        })
        .join("/")
}

fn resolve_workspace_resource(base_path: &str, source: &str) -> Option<String> {
    if source.is_empty() || source.starts_with("//") || source.contains('\0') {
        return None;
    }
    let source = source.split(['?', '#']).next().unwrap_or(source);
    let mut segments = if source.starts_with('/') {
        Vec::new()
    } else {
        base_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    for segment in source.trim_start_matches('/').replace('\\', "/").split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment if segment.contains(':') => return None,
            segment => segments.push(segment.to_string()),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn is_valid_fragment(source: &str) -> bool {
    source.len() > 1
        && source.len() <= 256
        && source[1..]
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
}

fn is_supported_data_image(source: &str) -> bool {
    let Some((header, payload)) = source.split_once(',') else {
        return false;
    };
    let media = header.to_ascii_lowercase();
    matches!(
        media.as_str(),
        "data:image/png;base64"
            | "data:image/jpeg;base64"
            | "data:image/gif;base64"
            | "data:image/webp;base64"
    ) && !payload.is_empty()
        && payload.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'\r' | b'\n')
        })
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_handles_fragments_urls_data_images_and_workspace_paths() {
        let policy = ResourcePolicy::for_file("docs/guide/readme.md");
        assert_eq!(
            policy.resolve(ResourceRole::Link, "#intro", None).kind,
            ResourceKind::Fragment
        );
        assert_eq!(
            policy
                .resolve(ResourceRole::Link, "../api.md", None)
                .resolved
                .as_deref(),
            Some("docs/api.md")
        );
        assert_eq!(
            policy
                .resolve(ResourceRole::Link, "../../../etc/passwd", None)
                .kind,
            ResourceKind::Blocked
        );
        assert_eq!(
            policy
                .resolve(ResourceRole::Link, "javascript:alert(1)", None)
                .kind,
            ResourceKind::Blocked
        );
        assert_eq!(
            policy
                .resolve(ResourceRole::Image, "data:image/png;base64,aGVsbG8=", None,)
                .kind,
            ResourceKind::DataImage
        );
    }

    #[test]
    fn policy_preserves_a_valid_data_image_beyond_diagnostic_text_limits() {
        let payload = "A".repeat(4_096);
        let source = format!("data:image/png;base64,{payload}");
        let resource = ResourcePolicy::new("").resolve(ResourceRole::Image, &source, None);

        assert_eq!(resource.kind, ResourceKind::DataImage);
        assert_eq!(resource.source, source);
        assert_eq!(resource.resolved.as_deref(), Some(resource.source.as_str()));
    }
}
