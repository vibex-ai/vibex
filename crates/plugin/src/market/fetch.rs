//! Fetching market documents under one network policy.
//!
//! Both market sources (the signed index and GitHub topic discovery) go through
//! this client, so the public-https rule, the per-hop redirect validation and
//! the body cap cannot drift apart between them.

use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;

use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::PluginError;

/// The longest a market request may take.
pub const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The most redirects one market request may follow.
pub const MAX_MARKET_REDIRECTS: usize = 5;

/// Which addresses a market request may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketUrlPolicy {
    /// Only public HTTPS addresses, as a shipped build uses.
    Public,
    /// Also loopback over HTTP, for tests and local development.
    Testing,
}

impl MarketUrlPolicy {
    /// Whether a URL is acceptable under this policy.
    pub fn validate(&self, url: &Url) -> Result<(), PluginError> {
        let testing = matches!(self, Self::Testing);
        match url.scheme() {
            "https" => {}
            "http" if testing => {}
            scheme => {
                return Err(PluginError::Network(format!(
                    "the scheme `{scheme}` is not allowed"
                )));
            }
        }
        let host = url
            .host()
            .ok_or_else(|| PluginError::Network("the address has no host".to_string()))?;
        if testing {
            return Ok(());
        }
        match host {
            url::Host::Domain(domain) => {
                if domain.eq_ignore_ascii_case("localhost")
                    || domain.to_ascii_lowercase().ends_with(".localhost")
                {
                    return Err(PluginError::Network(format!(
                        "the host `{domain}` is not a public address"
                    )));
                }
            }
            url::Host::Ipv4(address) => {
                if !is_public_ip(IpAddr::V4(address)) {
                    return Err(PluginError::Network(format!(
                        "the address `{address}` is not public"
                    )));
                }
            }
            url::Host::Ipv6(address) => {
                if !is_public_ip(IpAddr::V6(address)) {
                    return Err(PluginError::Network(format!(
                        "the address `{address}` is not public"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Whether an IP is a public, routable address.
pub fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
                || address.octets()[0] == 0
                || (address.octets()[0] == 100 && (64..=127).contains(&address.octets()[1]))
                || (address.octets()[0] == 192 && address.octets()[1] == 0)
                || (address.octets()[0] == 198 && address.octets()[1] == 18))
        }
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    let segments = address.segments();
    // Unique local (fc00::/7) and link local (fe80::/10).
    if (segments[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    if (segments[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    // IPv4-mapped addresses are checked as IPv4.
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ip(IpAddr::V4(mapped));
    }
    true
}

/// A market HTTP client.
#[derive(Debug, Clone)]
pub struct MarketHttpClient {
    client: reqwest::Client,
    policy: MarketUrlPolicy,
    timeout: Duration,
}

impl Default for MarketHttpClient {
    fn default() -> Self {
        Self::new(MarketUrlPolicy::Public)
    }
}

impl MarketHttpClient {
    pub fn new(policy: MarketUrlPolicy) -> Self {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .user_agent(concat!("vibex-plugin/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            client,
            policy,
            timeout: MARKET_REQUEST_TIMEOUT,
        }
    }

    pub fn policy(&self) -> MarketUrlPolicy {
        self.policy
    }

    /// Fetches a document, following redirects manually and capping the body.
    pub async fn fetch_bytes(&self, url: &Url, max_bytes: u64) -> Result<Vec<u8>, PluginError> {
        let (status, body) = self.send_collect(url, max_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(PluginError::Network(format!(
                "the server answered {status}"
            )));
        }
        Ok(body)
    }

    /// Fetches a document, treating `404` as an absent document.
    ///
    /// Topic discovery reads a manifest from many repositories; one that does
    /// not carry it is skipped, not an error.
    pub async fn fetch_optional_bytes(
        &self,
        url: &Url,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, PluginError> {
        let (status, body) = self.send_collect(url, max_bytes).await?;
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(PluginError::Network(format!(
                "the server answered {status}"
            )));
        }
        Ok(Some(body))
    }

    async fn send_collect(&self, url: &Url, max_bytes: u64) -> Result<(u16, Vec<u8>), PluginError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut current = url.clone();
        for _hop in 0..=MAX_MARKET_REDIRECTS {
            self.policy.validate(&current)?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(PluginError::Network("the request timed out".to_string()));
            }
            let response = tokio::time::timeout(remaining, self.client.get(current.clone()).send())
                .await
                .map_err(|_| PluginError::Network("the request timed out".to_string()))?
                .map_err(|error| PluginError::Network(error.to_string()))?;

            let status = response.status().as_u16();
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        PluginError::Network("a redirect carried no location".to_string())
                    })?;
                current = current
                    .join(location)
                    .map_err(|error| PluginError::Network(error.to_string()))?;
                continue;
            }

            if !(200..300).contains(&status) {
                return Ok((status, Vec::new()));
            }
            if let Some(length) = response.content_length()
                && length > max_bytes
            {
                return Err(PluginError::ResponseTooLarge);
            }

            let mut body = Vec::new();
            let mut response = response;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(PluginError::Network("the request timed out".to_string()));
                }
                let chunk = tokio::time::timeout(remaining, response.chunk())
                    .await
                    .map_err(|_| PluginError::Network("the request timed out".to_string()))?
                    .map_err(|error| PluginError::Network(error.to_string()))?;
                let Some(chunk) = chunk else {
                    break;
                };
                if body.len() as u64 + chunk.len() as u64 > max_bytes {
                    return Err(PluginError::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            return Ok((status, body));
        }
        Err(PluginError::Network("too many redirects".to_string()))
    }

    /// Fetches and decodes a JSON document.
    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        url: &Url,
        max_bytes: u64,
    ) -> Result<T, PluginError> {
        let bytes = self.fetch_bytes(url, max_bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_policy_rejects_local_addresses() {
        let policy = MarketUrlPolicy::Public;
        for url in [
            "http://example.com/x",
            "https://localhost/x",
            "https://127.0.0.1/x",
            "https://10.1.2.3/x",
            "https://192.168.1.1/x",
            "https://[::1]/x",
            "https://169.254.1.1/x",
            "https://0.0.0.0/x",
            "https://api.localhost/x",
        ] {
            assert!(
                policy.validate(&Url::parse(url).unwrap()).is_err(),
                "{url} should be rejected"
            );
        }
        assert!(
            policy
                .validate(&Url::parse("https://plugins.example.com/index.json").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn testing_policy_still_rejects_non_http_schemes() {
        let policy = MarketUrlPolicy::Testing;
        assert!(
            policy
                .validate(&Url::parse("http://127.0.0.1:8080/x").unwrap())
                .is_ok()
        );
        assert!(
            policy
                .validate(&Url::parse("file:///etc/passwd").unwrap())
                .is_err()
        );
        assert!(
            policy
                .validate(&Url::parse("ftp://example.com/x").unwrap())
                .is_err()
        );
    }

    #[test]
    fn ip_classification_matches_the_ranges() {
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(is_public_ip(address.parse().unwrap()), "{address}");
        }
        for address in ["127.0.0.1", "10.0.0.1", "192.168.0.1", "fc00::1", "fe80::1"] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
    }
}
