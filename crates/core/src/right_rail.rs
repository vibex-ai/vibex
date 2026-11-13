//! Right-rail web panels.
//!
//! A web panel is an address the reader pinned to the right rail. It is drawn
//! by the embedded browser in a user-owned tab, so the page is a real page:
//! its script runs, links work, and a dashboard behaves like a dashboard.
//!
//! A panel is user data, never code. Nothing is installed, granted, or trusted
//! to show one; the only thing stored is a title and an address.

use serde::{Deserialize, Serialize};

/// Upper bound on the panels one reader may pin.
pub const MAX_RIGHT_RAIL_WEB_PANELS: usize = 24;
/// Upper bound on a panel title, in characters.
pub const MAX_RIGHT_RAIL_WEB_PANEL_TITLE_CHARS: usize = 64;
/// Upper bound on a panel address, in bytes.
pub const MAX_RIGHT_RAIL_WEB_PANEL_URL_BYTES: usize = 2048;

/// One address the reader pinned to the right rail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebPanel {
    pub panel_id: String,
    pub title: String,
    pub url: String,
    /// Where the panel sits on the rail.
    pub order_index: i64,
}

/// Adds a panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebPanelAddRequest {
    pub title: String,
    pub url: String,
}

/// Removes a panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebPanelRemoveRequest {
    pub panel_id: String,
}

/// Normalises and checks a panel title.
pub fn validate_web_panel_title(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("a web panel needs a title".to_string());
    }
    if title.chars().count() > MAX_RIGHT_RAIL_WEB_PANEL_TITLE_CHARS {
        return Err(format!(
            "a web panel title is at most {MAX_RIGHT_RAIL_WEB_PANEL_TITLE_CHARS} characters"
        ));
    }
    Ok(title.to_string())
}

/// Normalises and checks a panel address.
///
/// The embedded browser loads the address, so the rule is the one the market's
/// network policy already uses: a public `https` address, without credentials.
/// A loopback `http` address is allowed too, because a local dashboard has no
/// certificate to offer.
pub fn validate_web_panel_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_RIGHT_RAIL_WEB_PANEL_URL_BYTES {
        return Err(format!(
            "a web panel needs an address of at most {MAX_RIGHT_RAIL_WEB_PANEL_URL_BYTES} bytes"
        ));
    }
    let parsed =
        url::Url::parse(url).map_err(|_| "the web panel address is not a URL".to_string())?;
    let host = parsed.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    match parsed.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err("a web panel address must use https, or http on loopback".to_string());
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("a web panel address must not carry credentials".to_string());
    }
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_trimmed_and_bounded() {
        assert_eq!(validate_web_panel_title("  Status  ").unwrap(), "Status");
        assert!(validate_web_panel_title("   ").is_err());
        assert!(validate_web_panel_title(&"x".repeat(65)).is_err());
        assert!(validate_web_panel_title(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn an_address_must_be_public_https_or_loopback_http() {
        assert_eq!(
            validate_web_panel_url("https://status.example.com/").unwrap(),
            "https://status.example.com/"
        );
        assert!(validate_web_panel_url("http://localhost:3000/").is_ok());
        assert!(validate_web_panel_url("http://127.0.0.1:2375/").is_ok());
        for refused in [
            "http://example.com/",
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "https://user:pass@example.com/",
            "",
        ] {
            assert!(
                validate_web_panel_url(refused).is_err(),
                "{refused} should be refused"
            );
        }
    }
}
