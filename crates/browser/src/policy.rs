//! Browser safety policy: URL classification, file-path authorization, download
//! naming and the versioned risk disclaimer.
//!
//! The threat model this file implements:
//!
//! * A page can contain instructions aimed at the agent. Page content is
//!   untrusted data, never authority.
//! * The browser runs on the runtime host, so it can reach loopback and private
//!   network services that are not exposed to the internet at all.
//! * A persistent profile carries real login state, so an injected agent could
//!   perform genuine actions on sites the user is signed in to.
//!
//! Loopback is **not** blanket-trusted: only origins the runtime positively
//! identified as this workspace's development server skip domain approval.

use std::path::{Component, PathBuf};

use vibex_core::{BrowserUnavailableReason, is_loopback_origin, redact_url_for_ledger};

use crate::error::{BrowserError, BrowserResult};

/// Bumping this re-shows the risk disclaimer after a material change in the
/// wording.
pub const BROWSER_RISK_DISCLAIMER_VERSION: u32 = 1;

/// Largest download filename the runtime will store.
pub const MAX_DOWNLOAD_FILENAME_CHARS: usize = 180;

/// Schemes the embedded browser will load.
///
/// `file://` is deliberately absent. Local previews go through
/// [`authorize_preview_path`] and are served as data the runtime controls, so
/// an absolute filesystem path is never handed to the page or the model.
pub const ALLOWED_SCHEMES: &[&str] = &["http", "https", "about", "data", "blob"];

/// Document extensions the local preview may open.
pub const ALLOWED_PREVIEW_EXTENSIONS: &[&str] = &["html", "htm"];

/// Why a navigation needs a human decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationDecision {
    /// Same-origin navigation, or a navigation to the workspace's own dev
    /// server. No prompt.
    Allowed,
    /// A cross-origin navigation to a public origin. Requires approval unless
    /// the domain is already granted for the session.
    RequiresApproval { origin: String, domain: String },
    /// A navigation to a loopback or private-network origin that the runtime
    /// did not identify as this workspace's dev server. Always prompts, even if
    /// the public domain is granted.
    RequiresApprovalForPrivateNetwork { origin: String, domain: String },
    /// The scheme is not loadable at all.
    Refused { reason: String },
}

/// Classifies a navigation target.
pub fn classify_navigation(
    target: &str,
    current_url: Option<&str>,
    workspace_dev_server_origins: &[String],
    granted_domains: &[String],
) -> NavigationDecision {
    if target.trim().is_empty() {
        return NavigationDecision::Refused {
            reason: "the navigation target is empty".to_string(),
        };
    }
    let Ok(parsed) = url::Url::parse(target) else {
        return NavigationDecision::Refused {
            reason: "the navigation target is not an absolute URL".to_string(),
        };
    };
    if !ALLOWED_SCHEMES.contains(&parsed.scheme()) {
        return NavigationDecision::Refused {
            reason: format!("the `{}` scheme is not loadable", parsed.scheme()),
        };
    }
    // `about:blank` and inline documents carry no network origin.
    if matches!(parsed.scheme(), "about" | "data" | "blob") {
        return NavigationDecision::Allowed;
    }
    let origin = parsed.origin().ascii_serialization();
    let domain = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    if workspace_dev_server_origins
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&origin))
    {
        return NavigationDecision::Allowed;
    }

    let same_origin = current_url
        .and_then(|current| url::Url::parse(current).ok())
        .is_some_and(|current| current.origin() == parsed.origin());
    if same_origin {
        return NavigationDecision::Allowed;
    }

    if granted_domains
        .iter()
        .any(|granted| granted.eq_ignore_ascii_case(&domain))
    {
        return NavigationDecision::Allowed;
    }

    if is_loopback_origin(target) || is_private_network_host(parsed.host_str().unwrap_or_default())
    {
        return NavigationDecision::RequiresApprovalForPrivateNetwork { origin, domain };
    }

    NavigationDecision::RequiresApproval { origin, domain }
}

/// True when the host is a loopback address, a private range address, or a
/// link-local / cloud-metadata style name.
///
/// The browser runs on the runtime host, so these are the services an injected
/// agent most wants to reach and that no public-domain allowlist protects.
pub fn is_private_network_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return match address {
            std::net::IpAddr::V4(address) => {
                address.is_loopback()
                    || address.is_private()
                    || address.is_link_local()
                    || address.is_unspecified()
                    // 100.64.0.0/10 carrier-grade NAT.
                    || (address.octets()[0] == 100 && (64..128).contains(&address.octets()[1]))
            }
            std::net::IpAddr::V6(address) => {
                address.is_loopback()
                    || address.is_unspecified()
                    // Unique local addresses fc00::/7.
                    || (address.segments()[0] & 0xfe00) == 0xfc00
                    // Link-local fe80::/10.
                    || (address.segments()[0] & 0xffc0) == 0xfe80
            }
        };
    }
    // Cloud metadata endpoints are reachable by name on several providers.
    matches!(
        host.as_str(),
        "metadata.google.internal" | "metadata.goog" | "instance-data"
    )
}

/// Resolves a path inside one of the authorized roots.
///
/// Symbolic links are resolved first: a link inside the workspace pointing at
/// `/etc` must not pass the boundary check. The comparison uses path
/// components, not `starts_with` on a string, so `/work-evil` does not look
/// like it lives inside `/work`.
pub fn authorize_path(candidate: &str, allowed_roots: &[PathBuf]) -> BrowserResult<PathBuf> {
    if candidate.trim().is_empty() {
        return Err(BrowserError::validation(
            "browser_path_empty",
            "the requested path is empty",
        ));
    }
    let requested = PathBuf::from(candidate);
    if requested
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(BrowserError::permission(
            "browser_path_not_authorized",
            "the requested path escapes the authorized roots",
        ));
    }
    let base = if requested.is_absolute() {
        requested.clone()
    } else {
        allowed_roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(&requested)
    };
    let resolved = std::fs::canonicalize(&base).map_err(|error| {
        BrowserError::validation(
            "browser_path_not_found",
            "the requested path does not exist",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut authorized_root: Option<PathBuf> = None;
    for root in allowed_roots {
        let Ok(root) = std::fs::canonicalize(root) else {
            continue;
        };
        if resolved == root || resolved.starts_with(&root) {
            authorized_root = Some(root);
            break;
        }
    }
    let Some(root) = authorized_root else {
        return Err(BrowserError::permission(
            "browser_path_not_authorized",
            "the requested path is outside the Agent's authorized project and attached directories",
        ));
    };
    let _ = root;
    Ok(resolved)
}

/// Authorizes a local HTML preview and returns its canonical path.
pub fn authorize_preview_path(
    candidate: &str,
    allowed_roots: &[PathBuf],
) -> BrowserResult<PathBuf> {
    let resolved = authorize_path(candidate, allowed_roots)?;
    let extension = resolved
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !ALLOWED_PREVIEW_EXTENSIONS.contains(&extension.as_str()) {
        return Err(BrowserError::validation(
            "browser_preview_extension_unsupported",
            "only .html and .htm files can be opened as a local preview",
        ));
    }
    Ok(resolved)
}

/// Authorizes a list of files for upload.
pub fn authorize_upload_paths(
    candidates: &[String],
    allowed_roots: &[PathBuf],
) -> BrowserResult<Vec<PathBuf>> {
    if candidates.is_empty() {
        return Err(BrowserError::validation(
            "browser_upload_empty",
            "at least one file must be supplied",
        ));
    }
    candidates
        .iter()
        .map(|candidate| authorize_path(candidate, allowed_roots))
        .collect()
}

/// Produces a filesystem-safe download filename.
///
/// Control characters, path separators and Windows-reserved characters are
/// stripped: the page chooses this name, so it must not be able to steer the
/// write out of the download directory.
pub fn sanitize_download_filename(raw: &str, fallback_timestamp_ms: i64) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let last_segment = cleaned
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let safe: String = last_segment
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .take(MAX_DOWNLOAD_FILENAME_CHARS)
        .collect();
    let safe = safe.trim_matches(['.', ' ']).to_string();
    if safe.is_empty() {
        format!("download-{fallback_timestamp_ms}")
    } else {
        safe
    }
}

/// True when the user has acknowledged the current risk disclaimer.
pub fn has_acknowledged_risk_disclaimer(acknowledged_version: u32) -> bool {
    acknowledged_version >= BROWSER_RISK_DISCLAIMER_VERSION
}

/// The one-line untrusted-content notice appended to every browser tool
/// description.
pub fn untrusted_content_notice() -> &'static str {
    vibex_core::BROWSER_UNTRUSTED_CONTENT_NOTICE
}

/// Explanation shown next to the onboarding card for a missing browser.
pub fn missing_browser_guidance(platform: &str) -> String {
    match platform {
        "windows" => "Install Google Chrome or Microsoft Edge. Edge is preinstalled on \
                      Windows 10 and 11 and works unchanged."
            .to_string(),
        "macos" => "Install Google Chrome, Chromium, Microsoft Edge or Brave. Safari cannot be \
                    used: it is WebKit-based and does not implement the DevTools Protocol."
            .to_string(),
        _ => "Install Google Chrome or Chromium from your distribution. Snap and Flatpak builds \
              may restrict profile directories and file-descriptor inheritance; a distribution \
              package is the most reliable choice."
            .to_string(),
    }
}

/// Builds the "browser is unavailable" detail message for a reason.
pub fn unavailable_detail(reason: BrowserUnavailableReason) -> String {
    match reason {
        BrowserUnavailableReason::BrowserMissing => missing_browser_guidance(std::env::consts::OS),
        BrowserUnavailableReason::RemoteRuntimeUnsupported => {
            "The runtime this client is paired with is remote, and the browser frame transport \
             over Remote v2 is not implemented yet."
                .to_string()
        }
        BrowserUnavailableReason::PlatformUnsupported => {
            "The embedded browser is not supported on the runtime's platform.".to_string()
        }
        BrowserUnavailableReason::RemoteDebuggingDisabled => {
            "The browser refused remote debugging. Chrome 136 and later require a non-default \
             --user-data-dir, and an enterprise RemoteDebuggingAllowed policy can disable it \
             entirely."
                .to_string()
        }
        BrowserUnavailableReason::DisclaimerPending => {
            "Review and accept the embedded browser risk notice to enable the panel.".to_string()
        }
        BrowserUnavailableReason::FeatureDisabled => {
            "The embedded browser is disabled for this runtime.".to_string()
        }
    }
}

/// Summary text for a ledger entry, with anything sensitive removed.
pub fn ledger_summary(action: &str, target: Option<&str>) -> String {
    match target {
        Some(target) if !target.is_empty() => {
            format!("{action} {}", redact_url_for_ledger(target))
        }
        _ => action.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(
        target: &str,
        current: Option<&str>,
        dev_servers: &[&str],
        granted: &[&str],
    ) -> NavigationDecision {
        classify_navigation(
            target,
            current,
            &dev_servers
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            &granted.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn same_origin_navigation_is_allowed_without_a_prompt() {
        assert_eq!(
            decision(
                "https://example.com/b",
                Some("https://example.com/a"),
                &[],
                &[]
            ),
            NavigationDecision::Allowed
        );
    }

    #[test]
    fn cross_origin_navigation_requires_approval() {
        match decision(
            "https://other.test/x",
            Some("https://example.com/a"),
            &[],
            &[],
        ) {
            NavigationDecision::RequiresApproval { domain, .. } => assert_eq!(domain, "other.test"),
            other => panic!("unexpected decision: {other:?}"),
        }
    }

    #[test]
    fn granted_domains_skip_the_prompt() {
        assert_eq!(
            decision("https://other.test/x", None, &[], &["other.test"]),
            NavigationDecision::Allowed
        );
    }

    #[test]
    fn workspace_dev_server_origin_is_exempt_but_other_loopback_is_not() {
        assert_eq!(
            decision(
                "http://127.0.0.1:5173/",
                None,
                &["http://127.0.0.1:5173"],
                &[]
            ),
            NavigationDecision::Allowed
        );
        assert!(matches!(
            decision(
                "http://127.0.0.1:2375/",
                None,
                &["http://127.0.0.1:5173"],
                &[]
            ),
            NavigationDecision::RequiresApprovalForPrivateNetwork { .. }
        ));
        assert!(matches!(
            decision("http://localhost:8080/", None, &[], &[]),
            NavigationDecision::RequiresApprovalForPrivateNetwork { .. }
        ));
    }

    #[test]
    fn private_network_hosts_are_recognized() {
        assert!(is_private_network_host("10.0.0.5"));
        assert!(is_private_network_host("192.168.1.1"));
        assert!(is_private_network_host("172.16.4.4"));
        assert!(is_private_network_host("169.254.169.254"));
        assert!(is_private_network_host("100.64.1.1"));
        assert!(is_private_network_host("fd00::1"));
        assert!(is_private_network_host("metadata.google.internal"));
        assert!(!is_private_network_host("example.com"));
        assert!(!is_private_network_host("8.8.8.8"));
        assert!(!is_private_network_host("172.32.0.1"));
    }

    #[test]
    fn file_urls_and_exotic_schemes_are_refused() {
        assert!(matches!(
            decision("file:///etc/passwd", None, &[], &[]),
            NavigationDecision::Refused { .. }
        ));
        assert!(matches!(
            decision("chrome://settings", None, &[], &[]),
            NavigationDecision::Refused { .. }
        ));
        assert!(matches!(
            decision("not a url", None, &[], &[]),
            NavigationDecision::Refused { .. }
        ));
    }

    #[test]
    fn about_blank_is_allowed() {
        assert_eq!(
            decision("about:blank", None, &[], &[]),
            NavigationDecision::Allowed
        );
    }

    #[test]
    fn path_authorization_rejects_escapes_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("index.html");
        std::fs::write(&inside, b"<html></html>").unwrap();
        let roots = vec![root.path().to_path_buf()];

        assert_eq!(
            authorize_path(inside.to_str().unwrap(), &roots).unwrap(),
            std::fs::canonicalize(&inside).unwrap()
        );
        let escaped = authorize_path("../outside.html", &roots).unwrap_err();
        assert_eq!(escaped.code, "browser_path_not_authorized");
        let missing = authorize_path("nope.html", &roots).unwrap_err();
        assert_eq!(missing.code, "browser_path_not_found");

        #[cfg(unix)]
        {
            let link = root.path().join("escape.html");
            std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
            let error = authorize_path(link.to_str().unwrap(), &roots).unwrap_err();
            assert_eq!(error.code, "browser_path_not_authorized");
        }
    }

    #[test]
    fn path_authorization_does_not_confuse_sibling_prefixes() {
        let parent = tempfile::tempdir().unwrap();
        let work = parent.path().join("work");
        let work_evil = parent.path().join("work-evil");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&work_evil).unwrap();
        let secret = work_evil.join("secret.html");
        std::fs::write(&secret, b"<html></html>").unwrap();

        let error = authorize_path(secret.to_str().unwrap(), &[work]).unwrap_err();
        assert_eq!(error.code, "browser_path_not_authorized");
    }

    #[test]
    fn preview_only_accepts_html() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("app.js");
        std::fs::write(&script, b"console.log(1)").unwrap();
        let error = authorize_preview_path(script.to_str().unwrap(), &[root.path().to_path_buf()])
            .unwrap_err();
        assert_eq!(error.code, "browser_preview_extension_unsupported");
    }

    #[test]
    fn download_filenames_cannot_steer_the_write() {
        assert_eq!(sanitize_download_filename("../../etc/passwd", 0), "passwd");
        assert_eq!(sanitize_download_filename("a\\b\\c.txt", 0), "c.txt");
        assert_eq!(
            sanitize_download_filename("re:port?.txt", 0),
            "re_port_.txt"
        );
        assert_eq!(sanitize_download_filename("", 1234), "download-1234");
        assert_eq!(sanitize_download_filename("...", 7), "download-7");
        assert_eq!(sanitize_download_filename("\u{7}evil\n.txt", 0), "evil.txt");
        let long = sanitize_download_filename(&"a".repeat(500), 0);
        assert_eq!(long.chars().count(), MAX_DOWNLOAD_FILENAME_CHARS);
    }

    #[test]
    fn risk_disclaimer_is_versioned() {
        assert!(!has_acknowledged_risk_disclaimer(0));
        assert!(has_acknowledged_risk_disclaimer(
            BROWSER_RISK_DISCLAIMER_VERSION
        ));
    }

    #[test]
    fn ledger_summaries_drop_query_strings() {
        assert_eq!(
            ledger_summary("navigate to", Some("https://example.com/a?token=1")),
            "navigate to https://example.com/a"
        );
        assert_eq!(ledger_summary("list tabs", None), "list tabs");
    }

    #[test]
    fn guidance_is_platform_specific() {
        assert!(missing_browser_guidance("macos").contains("Safari"));
        assert!(missing_browser_guidance("windows").contains("Edge"));
        assert!(missing_browser_guidance("linux").contains("Chromium"));
    }
}
