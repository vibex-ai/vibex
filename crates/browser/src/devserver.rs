//! Development-server URL detection from terminal output.
//!
//! Detection runs on the **runtime side** PTY output stream, not on the UI's
//! visible grid: the panel may be hidden, the text may have scrolled away, and
//! when the runtime is remote the PTY is not on the UI's machine at all.
//!
//! Two rules keep this from producing junk:
//!
//! 1. A URL in terminal output proves nothing — it may be a log line, a
//!    documented example, or another process's output. Every candidate is
//!    confirmed with a real TCP connect before it is offered.
//! 2. A development server is only ever *offered*, never opened
//!    automatically. The user decides.

use std::time::Duration;

/// How long a readiness probe waits for a connection.
pub const DEV_SERVER_PROBE_TIMEOUT: Duration = Duration::from_millis(600);
/// Ports probed per detected host.
pub const MAX_CANDIDATES: usize = 8;

/// A confirmed development server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevServerCandidate {
    pub origin: String,
    pub host: String,
    pub port: u16,
}

/// Incremental scanner over a terminal output stream.
///
/// URLs are routinely split across read chunks (`http://local` + `host:5173/`),
/// so the scanner keeps a bounded tail between calls.
#[derive(Debug)]
pub struct DevServerScanner {
    tail: String,
    seen: Vec<String>,
    enabled: bool,
}

/// Characters that may appear in a URL inside terminal output.
fn is_url_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            ':' | '/' | '.' | '-' | '_' | '~' | '%' | '?' | '=' | '&' | '#' | '+' | '@' | '[' | ']'
        )
}

/// Length of the retained tail. Long enough for any realistic URL.
const TAIL_CHARS: usize = 512;

impl Default for DevServerScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl DevServerScanner {
    pub fn new() -> Self {
        Self {
            tail: String::new(),
            seen: Vec::new(),
            enabled: true,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.tail.clear();
        }
    }

    /// Feeds one output chunk and returns newly discovered URL candidates.
    ///
    /// Candidates are deduplicated per scanner instance so a server that
    /// reprints its banner does not produce a stream of prompts.
    pub fn push(&mut self, chunk: &str) -> Vec<(String, String, u16)> {
        if !self.enabled {
            return Vec::new();
        }
        self.tail.push_str(chunk);
        if self.tail.chars().count() > TAIL_CHARS {
            let skip = self.tail.chars().count() - TAIL_CHARS;
            let boundary = self
                .tail
                .char_indices()
                .nth(skip)
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.tail.drain(..boundary);
        }
        let mut found = Vec::new();
        for (origin, host, port) in scan_text(&self.tail) {
            if self.seen.contains(&origin) {
                continue;
            }
            if self.seen.len() >= MAX_CANDIDATES {
                self.seen.remove(0);
            }
            self.seen.push(origin.clone());
            found.push((origin, host, port));
        }
        found
    }

    /// Remembers an origin so it is not reported again.
    pub fn remember(&mut self, origin: &str) {
        if !self.seen.iter().any(|entry| entry == origin) {
            self.seen.push(origin.to_string());
        }
    }

    pub fn clear(&mut self) {
        self.seen.clear();
        self.tail.clear();
    }
}

/// Extracts `(origin, host, port)` triples from free text.
///
/// Only loopback and private-network hosts are considered: a public URL printed
/// by a build tool is far more likely to be documentation than a local server.
///
/// The scan runs over bytes, not characters. Terminal banners are full of
/// non-ASCII decoration (`➜`, box drawing, emoji), and mixing byte offsets from
/// `str::find` with character indices silently extracts the wrong slice.
pub fn scan_text(text: &str) -> Vec<(String, String, u16)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let Some(start) = find_scheme(bytes, index) else {
            break;
        };
        let mut end = start;
        while end < bytes.len() && is_url_char(bytes[end] as char) {
            end += 1;
        }
        index = end.max(start + 1);
        let Ok(raw) = std::str::from_utf8(&bytes[start..end]) else {
            continue;
        };
        // Trailing punctuation is almost always sentence punctuation, not URL.
        let raw = raw.trim_end_matches(['.', ',', ';', ')', ']', '\'', '"']);
        let Ok(parsed) = url::Url::parse(raw) else {
            continue;
        };
        let Some(host) = parsed.host_str().map(str::to_string) else {
            continue;
        };
        if !is_local_host(&host) {
            continue;
        }
        let Some(port) = parsed.port().or_else(|| parsed.port_or_known_default()) else {
            continue;
        };
        let origin = serialized_origin(&parsed);
        if found
            .iter()
            .any(|(existing, _, _): &(String, String, u16)| *existing == origin)
        {
            continue;
        }
        found.push((origin, host, port));
    }
    found
}

/// Serializes a URL's origin as `scheme://host[:port]`.
fn serialized_origin(parsed: &url::Url) -> String {
    let host = parsed.host_str().unwrap_or_default();
    match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    }
}

/// Finds the next `http://` or `https://` at or after `from`.
fn find_scheme(bytes: &[u8], from: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        let rest = &bytes[index..];
        if rest.starts_with(b"http://") || rest.starts_with(b"https://") {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn is_local_host(host: &str) -> bool {
    crate::policy::is_private_network_host(host)
}

/// Confirms a candidate with a real TCP connect.
///
/// A regex match is a guess; a completed connection is evidence.
pub async fn probe_candidate(host: &str, port: u16) -> bool {
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    matches!(
        tokio::time::timeout(
            DEV_SERVER_PROBE_TIMEOUT,
            tokio::net::TcpStream::connect(&target)
        )
        .await,
        Ok(Ok(_))
    )
}

/// Scans and confirms in one step.
pub async fn detect_ready_dev_servers(text: &str) -> Vec<DevServerCandidate> {
    let mut confirmed = Vec::new();
    for (origin, host, port) in scan_text(text) {
        if probe_candidate(&host, port).await {
            confirmed.push(DevServerCandidate { origin, host, port });
        }
    }
    confirmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vite_style_banner_is_detected() {
        let candidates = scan_text("  ➜  Local:   http://localhost:5173/\n");
        assert_eq!(
            candidates,
            vec![(
                "http://localhost:5173".to_string(),
                "localhost".to_string(),
                5173
            )]
        );
    }

    #[test]
    fn next_and_webpack_style_banners_are_detected() {
        assert_eq!(scan_text("- ready started server on 0.0.0.0:3000").len(), 0);
        assert_eq!(
            scan_text("▲ Next.js 15.0.0\n- Local:        http://localhost:3000")
                .first()
                .map(|(origin, _, _)| origin.clone()),
            Some("http://localhost:3000".to_string())
        );
        assert_eq!(
            scan_text("webpack compiled. On Your Network: http://192.168.1.5:8080/")
                .first()
                .map(|(origin, _, _)| origin.clone()),
            Some("http://192.168.1.5:8080".to_string())
        );
    }

    #[test]
    fn public_urls_are_ignored() {
        assert!(scan_text("see https://vitejs.dev/config/ for details").is_empty());
        assert!(scan_text("downloaded https://example.com/pkg.tgz").is_empty());
    }

    #[test]
    fn trailing_punctuation_is_trimmed() {
        let candidates = scan_text("open http://localhost:5173/.");
        assert_eq!(candidates[0].0, "http://localhost:5173");
        let candidates = scan_text("(http://localhost:5173/)");
        assert_eq!(candidates[0].0, "http://localhost:5173");
    }

    #[test]
    fn default_ports_are_filled_in() {
        let candidates = scan_text("serving at http://localhost/");
        assert_eq!(candidates[0].2, 80);
    }

    #[test]
    fn duplicates_in_one_chunk_collapse() {
        let candidates = scan_text("http://localhost:5173 and http://localhost:5173");
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn the_scanner_joins_urls_split_across_chunks() {
        let mut scanner = DevServerScanner::new();
        assert!(scanner.push("  ➜  Local:   http://local").is_empty());
        let found = scanner.push("host:5173/\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "http://localhost:5173");
    }

    #[test]
    fn the_scanner_does_not_repeat_an_origin() {
        let mut scanner = DevServerScanner::new();
        assert_eq!(scanner.push("http://localhost:5173\n").len(), 1);
        assert!(scanner.push("\nrestarting\n").is_empty());
        assert!(scanner.push("http://localhost:5173\n").is_empty());
    }

    #[test]
    fn the_scanner_can_be_disabled() {
        let mut scanner = DevServerScanner::new();
        scanner.set_enabled(false);
        assert!(scanner.push("http://localhost:5173\n").is_empty());
    }

    #[test]
    fn tail_is_bounded() {
        let mut scanner = DevServerScanner::new();
        for _ in 0..64 {
            scanner.push(&"x".repeat(256));
        }
        assert!(scanner.tail.chars().count() <= TAIL_CHARS);
    }

    #[tokio::test]
    async fn probing_a_closed_port_fails() {
        // Port 1 on loopback is not listening in any supported environment.
        assert!(!probe_candidate("127.0.0.1", 1).await);
    }

    #[tokio::test]
    async fn probing_a_real_listener_succeeds() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe_candidate("127.0.0.1", port).await);
    }

    #[tokio::test]
    async fn only_confirmed_servers_are_reported() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let text = format!("Local: http://127.0.0.1:{port}/ and http://127.0.0.1:1/");
        let confirmed = detect_ready_dev_servers(&text).await;
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].port, port);
    }
}
