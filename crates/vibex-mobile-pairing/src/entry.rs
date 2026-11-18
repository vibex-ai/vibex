//! What a scanned or pasted pairing entry is, and the claim that follows.

use vibex_backend::{BackendError, BackendResult};
use vibex_remote_client::ClientDeviceIdentity;

use crate::pairing::{MobilePairedRuntime, claim_pairing_link, claim_server_pairing_code};

/// A pairing entry the phone can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingEntry {
    /// A link: a desktop offer (`vibex://…/#/pair/…`) or a server's
    /// connection string (`vibex://…#/code/…` or a web link carrying one).
    Link { link: String },
    /// A bare one-time code, which needs the server's address as well.
    Code { server_url: String, code: String },
}

/// Whether the entry is a complete link (it carries its own address).
pub fn is_pairing_link(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("vibex://") || value.contains("#/pair/") || value.contains("#/code/")
}

/// Reads a typed or pasted entry into the flow it belongs to.
///
/// A code needs `server_url`; a link carries its own and ignores it. Both are
/// trimmed, and an empty entry is an error rather than a silent no-op so the
/// sheet can say what is missing.
pub fn classify_entry(entry: &str, server_url: &str) -> BackendResult<PairingEntry> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(invalid(
            "Paste the pairing code or link from the computer or the cloud runtime.",
        ));
    }
    if is_pairing_link(entry) {
        return Ok(PairingEntry::Link {
            link: entry.to_owned(),
        });
    }
    let server_url = server_url.trim();
    if server_url.is_empty() {
        return Err(invalid(
            "Enter the server address as well as the pairing code.",
        ));
    }
    Ok(PairingEntry::Code {
        server_url: server_url.to_owned(),
        code: entry.to_owned(),
    })
}

/// Claims whatever [`classify_entry`] reads.
///
/// `device_name` is this phone's identity in the runtime's paired-device
/// list (the platform's name); `previous_identity` lets a re-pairing update
/// the runtime's existing entry for this phone instead of adding another.
pub async fn claim_entry(
    entry: String,
    server_url: String,
    device_name: String,
    previous_identity: Option<ClientDeviceIdentity>,
) -> BackendResult<MobilePairedRuntime> {
    match classify_entry(&entry, &server_url)? {
        PairingEntry::Link { link } => {
            claim_pairing_link(link, device_name, previous_identity).await
        }
        PairingEntry::Code { server_url, code } => {
            claim_server_pairing_code(server_url, code, device_name, previous_identity).await
        }
    }
}

fn invalid(message: &str) -> BackendError {
    BackendError::failed("remote_pairing_code_request_invalid", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_carry_their_own_address() {
        for link in [
            "vibex://pair#/pair/abc",
            "https://desktop.example/#/pair/abc",
            "vibex://pair#/code/abc",
            "https://server.example/#/code/abc",
        ] {
            assert_eq!(
                classify_entry(link, "").unwrap(),
                PairingEntry::Link {
                    link: link.to_owned()
                }
            );
        }
    }

    #[test]
    fn a_bare_code_needs_the_server_address() {
        assert_eq!(
            classify_entry(" 123456 ", " https://server.example ").unwrap(),
            PairingEntry::Code {
                server_url: "https://server.example".to_owned(),
                code: "123456".to_owned(),
            }
        );
        let error = classify_entry("123456", " ").unwrap_err();
        assert_eq!(error.code, "remote_pairing_code_request_invalid");
    }

    #[test]
    fn an_empty_entry_is_reported_not_ignored() {
        let error = classify_entry("  ", "https://server.example").unwrap_err();
        assert_eq!(error.code, "remote_pairing_code_request_invalid");
    }
}
