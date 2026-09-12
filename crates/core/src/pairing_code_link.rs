//! Pairing-code connection links (`vibex://pair#/code/<payload>`).
//!
//! A headless runtime that serves its own self-signed certificate has no
//! public CA to lean on, so the certificate has to reach the client **out of
//! band**. `vibex-server` prints one of these links — and a QR rendering of it
//! — next to the one-time pairing code, and the operator copies or scans it.
//! The certificate therefore arrives over a channel an on-path attacker does
//! not control, and the client can pin it before it ever speaks TLS to the
//! runtime. Fetching the certificate from the server instead would make the
//! pin a trust-on-first-use decision with no authentication behind it.
//!
//! The link is a secret: it carries the single-use pairing code. Treat it the
//! same way the bare code is treated — the QR/link channel is the operator's
//! own screen, not the network.

use std::fmt;
use std::net::IpAddr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::{VibexError, VibexResult};

/// Custom scheme that marks a pairing-code link.
pub const PAIRING_CODE_LINK_SCHEME: &str = "vibex";
/// Custom scheme host that marks a pairing-code link.
pub const PAIRING_CODE_LINK_HOST: &str = "pair";
/// Scheme and host prefix of every encoded link.
pub const PAIRING_CODE_LINK_PREFIX: &str = "vibex://pair";
/// Fragment prefix that carries the base64url JSON payload.
pub const PAIRING_CODE_FRAGMENT_PREFIX: &str = "#/code/";
/// Schema version of the payload carried inside the fragment.
pub const PAIRING_CODE_LINK_SCHEMA_VERSION: &str = "vibex-pairing-code.v1";
/// Bounded size for the whole link string.
pub const MAX_PAIRING_CODE_LINK_BYTES: usize = 32 * 1024;
/// Bounded size for the decoded payload.
pub const MAX_PAIRING_CODE_LINK_PAYLOAD_BYTES: usize = 32 * 1024;
/// Bounded size for an embedded certificate (a self-signed Ed25519 leaf is
/// well under 1 KiB; the bound leaves room for a short chain).
pub const MAX_PAIRING_CODE_CERTIFICATE_BYTES: usize = 16 * 1024;
/// Bounded size for the pairing code itself.
pub const MAX_PAIRING_CODE_CHARS: usize = 64;

/// One copy-pasteable/scannable pairing entry for a headless runtime.
///
/// `tls_certificate_der` is present when the runtime serves a self-signed
/// certificate (`pinned_certificate` TLS policy). It is `None` for a runtime
/// behind a publicly trusted certificate, where normal system roots apply.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemotePairingCodeLink {
    pub schema_version: String,
    pub server_url: String,
    pub pairing_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_certificate_der: Option<String>,
}

impl fmt::Debug for RemotePairingCodeLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pairing code is a one-time secret; only its presence is printed.
        formatter
            .debug_struct("RemotePairingCodeLink")
            .field("schema_version", &self.schema_version)
            .field("server_url", &self.server_url)
            .field("has_pairing_code", &!self.pairing_code.is_empty())
            .field("has_tls_certificate", &self.tls_certificate_der.is_some())
            .finish()
    }
}

impl RemotePairingCodeLink {
    /// Builds a validated link. `tls_certificate_der` is the base64url-encoded
    /// DER leaf certificate; pass `None` when the runtime uses a publicly
    /// trusted certificate.
    pub fn new(
        server_url: impl Into<String>,
        pairing_code: impl Into<String>,
        tls_certificate_der: Option<String>,
    ) -> VibexResult<Self> {
        let link = Self {
            schema_version: PAIRING_CODE_LINK_SCHEMA_VERSION.to_string(),
            server_url: server_url.into(),
            pairing_code: pairing_code.into(),
            tls_certificate_der,
        };
        link.validate()?;
        Ok(link)
    }

    pub fn validate(&self) -> VibexResult<()> {
        if self.schema_version != PAIRING_CODE_LINK_SCHEMA_VERSION {
            return Err(invalid("pairing link schema version is not supported"));
        }
        let url = parse_server_url(&self.server_url)?;
        if self.pairing_code.is_empty()
            || self.pairing_code.len() > MAX_PAIRING_CODE_CHARS
            || !self.pairing_code.chars().all(|character| {
                character.is_ascii_digit() || matches!(character, '-' | ' ' | '\t')
            })
        {
            return Err(invalid(
                "pairing link must carry a bounded numeric pairing code",
            ));
        }
        if let Some(encoded) = self.tls_certificate_der.as_deref() {
            decode_certificate(encoded)?;
            // Self-signed certificates only make sense on a local network, and
            // a pinned route to an arbitrary Internet host would bypass the
            // public CA ecosystem rather than merely replace it. Clients
            // enforce the same rule when they build a transport, so the
            // contract refuses such a link up front instead of producing one
            // that is guaranteed to be rejected later.
            if url.scheme() != "https" || !url_host_is_local_network(&url) {
                return Err(invalid(
                    "a pinned pairing link must use HTTPS on a local network address",
                ));
            }
        }
        Ok(())
    }

    /// Encodes the full `vibex://pair#/code/<payload>` link.
    pub fn encode(&self) -> VibexResult<String> {
        self.validate()?;
        let payload = serde_json::to_vec(self).map_err(|_| {
            VibexError::validation(
                "remote_pairing_link_encode_failed",
                "pairing link payload could not be serialized",
            )
        })?;
        if payload.len() > MAX_PAIRING_CODE_LINK_PAYLOAD_BYTES {
            return Err(invalid("pairing link payload exceeds the bounded size"));
        }
        Ok(format!(
            "{PAIRING_CODE_LINK_PREFIX}{PAIRING_CODE_FRAGMENT_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(&payload)
        ))
    }

    /// Parses a full link or a bare `#/code/<payload>` fragment. The fragment
    /// form is what a QR scanner may hand back after trimming the scheme.
    pub fn parse(value: &str) -> VibexResult<Self> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_PAIRING_CODE_LINK_BYTES {
            return Err(invalid("pairing link is empty or exceeds the bounded size"));
        }
        let marker = value.find(PAIRING_CODE_FRAGMENT_PREFIX).ok_or_else(|| {
            invalid("pairing link does not contain the expected payload fragment")
        })?;
        if marker > 0 {
            // Anything before the fragment must be a well-formed custom-scheme
            // link with no credentials, port, query, or nested fragment.
            let prefix = &value[..marker];
            let url = Url::parse(prefix).map_err(|_| invalid("pairing link prefix is invalid"))?;
            if url.scheme() != PAIRING_CODE_LINK_SCHEME
                || url.host_str() != Some(PAIRING_CODE_LINK_HOST)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
                || url.query().is_some()
                || !matches!(url.path(), "" | "/")
            {
                return Err(invalid("pairing link prefix is not a pairing entry point"));
            }
        }
        let encoded = &value[marker + PAIRING_CODE_FRAGMENT_PREFIX.len()..];
        if encoded.is_empty()
            || encoded.len() > MAX_PAIRING_CODE_LINK_PAYLOAD_BYTES
            || encoded
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        {
            return Err(invalid(
                "pairing link payload is not bounded base64url data",
            ));
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| invalid("pairing link payload is not valid base64url data"))?;
        if decoded.len() > MAX_PAIRING_CODE_LINK_PAYLOAD_BYTES {
            return Err(invalid(
                "decoded pairing link payload exceeds the bounded size",
            ));
        }
        let link: Self = serde_json::from_slice(&decoded)
            .map_err(|_| invalid("pairing link payload is not a valid entry"))?;
        link.validate()?;
        Ok(link)
    }

    /// Decoded certificate bytes, when the link pins one.
    pub fn certificate_der(&self) -> VibexResult<Option<Vec<u8>>> {
        self.tls_certificate_der
            .as_deref()
            .map(decode_certificate)
            .transpose()
    }

    /// `sha256:<base64url>` digest an operator can compare against the value
    /// the runtime printed on its own console.
    pub fn tls_fingerprint(&self) -> VibexResult<Option<String>> {
        Ok(self
            .certificate_der()?
            .map(|der| certificate_fingerprint(&der)))
    }

    /// Normalized server URL without a trailing slash.
    pub fn normalized_server_url(&self) -> VibexResult<String> {
        let url = parse_server_url(&self.server_url)?;
        Ok(url.as_str().trim_end_matches('/').to_string())
    }
}

/// `sha256:<base64url(sha256(der))>`.
pub fn certificate_fingerprint(certificate_der: &[u8]) -> String {
    let digest = Sha256::digest(certificate_der);
    format!("sha256:{}", URL_SAFE_NO_PAD.encode(digest))
}

/// Loopback, RFC1918, or link-local address — the reach of a private LAN.
///
/// Shared by the runtime that advertises a pinned link and the client that
/// accepts one, so both ends agree on which addresses a self-signed
/// certificate may cover.
pub fn is_local_network_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
    }
}

/// [`is_local_network_address`] applied to a parsed URL host.
///
/// Uses `Url::host` rather than `Url::host_str`, because the string form of an
/// IPv6 host keeps its brackets and would never parse as an address.
pub fn url_host_is_local_network(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => is_local_network_address(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => is_local_network_address(IpAddr::V6(address)),
        _ => false,
    }
}

/// `url::Url` rejects a bare `host:port`, so a link without a scheme is
/// rejected here rather than silently misinterpreted.
fn parse_server_url(value: &str) -> VibexResult<Url> {
    let url = Url::parse(value).map_err(|_| invalid("pairing link server URL is invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "pairing link server URL must be HTTP(S) without credentials, query, or fragment",
        ));
    }
    Ok(url)
}

fn decode_certificate(encoded: &str) -> VibexResult<Vec<u8>> {
    if encoded.is_empty() || encoded.len() > MAX_PAIRING_CODE_CERTIFICATE_BYTES * 2 {
        return Err(invalid("pairing link certificate is empty or oversized"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("pairing link certificate is not valid base64url data"))?;
    if bytes.is_empty() || bytes.len() > MAX_PAIRING_CODE_CERTIFICATE_BYTES {
        return Err(invalid("pairing link certificate is empty or oversized"));
    }
    if bytes[0] != 0x30 {
        return Err(invalid("pairing link certificate is not a DER sequence"));
    }
    Ok(bytes)
}

fn invalid(message: impl Into<String>) -> VibexError {
    VibexError::validation("remote_pairing_link_invalid", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_certificate() -> Vec<u8> {
        // A DER SEQUENCE header plus bounded filler; the link contract only
        // validates the envelope, the TLS stack validates the certificate.
        let mut bytes = vec![0x30, 0x82, 0x01, 0x00];
        bytes.extend(std::iter::repeat_n(0x41_u8, 252));
        bytes
    }

    fn encoded_certificate() -> String {
        URL_SAFE_NO_PAD.encode(sample_certificate())
    }

    #[test]
    fn link_round_trips_with_and_without_a_pinned_certificate() {
        let plain = RemotePairingCodeLink::new("https://vibex.example.com", "123-456-789", None)
            .expect("plain link");
        let encoded = plain.encode().expect("encode");
        assert!(encoded.starts_with("vibex://pair#/code/"));
        assert_eq!(
            RemotePairingCodeLink::parse(&encoded).expect("parse"),
            plain
        );

        let pinned = RemotePairingCodeLink::new(
            "https://192.168.1.10:8765/",
            "123 456 789",
            Some(encoded_certificate()),
        )
        .expect("pinned link");
        let encoded = pinned.encode().expect("encode");
        let parsed = RemotePairingCodeLink::parse(&encoded).expect("parse");
        assert_eq!(parsed, pinned);
        assert_eq!(
            parsed.normalized_server_url().expect("url"),
            "https://192.168.1.10:8765"
        );
        assert_eq!(
            parsed.certificate_der().expect("der").as_deref(),
            Some(sample_certificate().as_slice())
        );
    }

    #[test]
    fn bare_fragment_is_accepted_but_foreign_prefixes_are_not() {
        let link =
            RemotePairingCodeLink::new("https://127.0.0.1:8765", "123456789", None).expect("link");
        let encoded = link.encode().expect("encode");
        let fragment = encoded
            .find(PAIRING_CODE_FRAGMENT_PREFIX)
            .map(|marker| &encoded[marker..])
            .expect("fragment");
        assert_eq!(
            RemotePairingCodeLink::parse(fragment).expect("fragment parse"),
            link
        );
        assert!(RemotePairingCodeLink::parse("vibex://open/direct#/pair/abc").is_err());
        assert!(RemotePairingCodeLink::parse("https://evil.example/#/code/abc").is_err());
    }

    #[test]
    fn insecure_transport_and_pinned_certificates_do_not_combine() {
        let error = RemotePairingCodeLink::new(
            "http://192.168.1.10:8765",
            "123-456-789",
            Some(encoded_certificate()),
        )
        .expect_err("http with a pin must be rejected");
        assert_eq!(error.code, "remote_pairing_link_invalid");

        // Plain HTTP without a pin stays representable so a debug client can
        // still pair with a loopback runtime that has no TLS at all.
        RemotePairingCodeLink::new("http://127.0.0.1:8765", "123-456-789", None)
            .expect("loopback http link");
    }

    #[test]
    fn pinned_transport_is_limited_to_the_local_network() {
        for rejected in [
            "https://vibex.example.com",
            "https://203.0.113.10:8765",
            "https://vibex.local:8765",
        ] {
            assert!(
                RemotePairingCodeLink::new(rejected, "123-456-789", Some(encoded_certificate()))
                    .is_err(),
                "{rejected} is not a local numeric address"
            );
        }
        for accepted in [
            "https://127.0.0.1:8765",
            "https://10.0.0.5:8765",
            "https://172.16.4.4:8765",
            "https://192.168.1.10:8765",
            "https://[fe80::1]:8765",
            "https://[fd00::5]:8765",
        ] {
            RemotePairingCodeLink::new(accepted, "123-456-789", Some(encoded_certificate()))
                .unwrap_or_else(|_| panic!("{accepted} must be a local network address"));
        }
    }

    #[test]
    fn invalid_payloads_are_rejected() {
        assert!(RemotePairingCodeLink::parse("").is_err());
        assert!(RemotePairingCodeLink::parse("vibex://pair#/code/!!!!").is_err());
        assert!(RemotePairingCodeLink::parse("vibex://pair#/pair/abcd").is_err());
        assert!(
            RemotePairingCodeLink::new("https://127.0.0.1:8765", "code-with-letters", None)
                .is_err()
        );
        assert!(
            RemotePairingCodeLink::new("127.0.0.1:8765", "123-456-789", None).is_err(),
            "a server URL without a scheme is not a URL"
        );
        assert!(
            RemotePairingCodeLink::new(
                "https://127.0.0.1:8765",
                "123-456-789",
                Some(URL_SAFE_NO_PAD.encode([0x00_u8, 0x01, 0x02])),
            )
            .is_err(),
            "a non-DER certificate is rejected before any TLS work"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_redacted_debug_hides_secrets() {
        let link = RemotePairingCodeLink::new(
            "https://192.168.1.10:8765",
            "123-456-789",
            Some(encoded_certificate()),
        )
        .expect("link");
        let fingerprint = link.tls_fingerprint().expect("fingerprint").expect("some");
        assert!(fingerprint.starts_with("sha256:"));
        assert_eq!(
            fingerprint,
            certificate_fingerprint(&sample_certificate()),
            "fingerprint only depends on the certificate bytes"
        );
        let debug = format!("{link:?}");
        assert!(!debug.contains("123-456-789"));
        assert!(!debug.contains(&encoded_certificate()));
    }
}
