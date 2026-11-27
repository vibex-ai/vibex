//! The file the phone keeps its paired runtimes in.
//!
//! One JSON document in the app's data directory. A credential is a bearer
//! grant, so the file is written with owner-only permissions where the
//! platform has them, and atomically: a crash leaves the previous file intact
//! rather than a half-written one.
//!
//! The schema is versioned and `deny_unknown_fields` at both levels, so a
//! store written by a newer build is refused rather than silently misread.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use vibex_backend::{BackendError, BackendResult};

use crate::pairing::MobileCredentialBundle;

/// The version of the store file this build writes and reads.
pub const PAIRING_STORE_SCHEMA_VERSION: &str = "vibex-mobile-pairing-store.v1";

/// The document on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingStoreFile {
    /// The schema this file was written with.
    pub schema_version: String,
    /// The runtimes, most recent first.
    #[serde(default)]
    pub runtimes: Vec<MobileCredentialBundle>,
}

/// The phone's saved runtimes, in one file.
#[derive(Clone, Debug)]
pub struct PairingStore {
    path: PathBuf,
}

impl PairingStore {
    /// A store at `path`; the file need not exist yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file the store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved runtimes, most recent first.
    ///
    /// A missing file is an empty store: a fresh install is not an error.
    pub fn load(&self) -> BackendResult<Vec<MobileCredentialBundle>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(read_error(&error)),
        };
        let file: PairingStoreFile = serde_json::from_slice(&bytes).map_err(|error| {
            BackendError::failed(
                "mobile_pairing_store_invalid",
                format!("the pairing store cannot be read: {error}"),
            )
        })?;
        if file.schema_version != PAIRING_STORE_SCHEMA_VERSION {
            return Err(BackendError::unsupported(
                "mobile_pairing_store_incompatible",
                "the pairing store was written by an incompatible build",
            ));
        }
        Ok(file.runtimes)
    }

    /// Replaces the stored runtimes.
    pub fn save(&self, runtimes: &[MobileCredentialBundle]) -> BackendResult<()> {
        let file = PairingStoreFile {
            schema_version: PAIRING_STORE_SCHEMA_VERSION.to_owned(),
            runtimes: runtimes.to_vec(),
        };
        let mut json = serde_json::to_vec_pretty(&file).map_err(|error| {
            BackendError::failed(
                "mobile_pairing_store_write_failed",
                format!("the pairing store cannot be serialized: {error}"),
            )
        })?;
        json.push(b'\n');

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| write_error(&error))?;
        }
        // Write next to the target and rename, so a crash cannot truncate the
        // credentials already saved.
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, &json).map_err(|error| write_error(&error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // A bearer grant: the owner reads it, nobody else does.
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                .map_err(|error| write_error(&error))?;
        }
        // Windows cannot rename over an existing file.
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| write_error(&error))?;
        }
        fs::rename(&temporary, &self.path).map_err(|error| write_error(&error))
    }

    /// Adds or replaces the runtime with the same host id, at the front.
    ///
    /// Returns the new list. Re-pairing the same desktop updates its entry
    /// instead of growing the picker and retains previously learned routes
    /// (LAN, Tailscale, direct URLs, relay) rather than overwriting them.
    pub fn upsert(
        &self,
        mut bundle: MobileCredentialBundle,
    ) -> BackendResult<Vec<MobileCredentialBundle>> {
        let host_id = bundle.host_id().to_owned();
        let mut runtimes = self.load()?;
        if let Some(existing) = runtimes.iter().find(|runtime| runtime.host_id() == host_id) {
            bundle.merge_from_existing(existing);
        }
        runtimes.retain(|runtime| runtime.host_id() != host_id);
        runtimes.insert(0, bundle);
        self.save(&runtimes)?;
        Ok(runtimes)
    }

    /// Updates an existing runtime in place without reordering the list.
    ///
    /// If the runtime has already been removed, leaves the store unchanged so
    /// an in-flight probe or rename cannot resurrect a deleted host.
    pub fn update_in_place(
        &self,
        bundle: MobileCredentialBundle,
    ) -> BackendResult<Vec<MobileCredentialBundle>> {
        let host_id = bundle.host_id().to_owned();
        let mut runtimes = self.load()?;
        if let Some(slot) = runtimes
            .iter_mut()
            .find(|runtime| runtime.host_id() == host_id)
        {
            *slot = bundle;
            self.save(&runtimes)?;
        }
        Ok(runtimes)
    }

    /// Removes the runtime with `host_id`, if it is there.
    pub fn remove(&self, host_id: &str) -> BackendResult<Vec<MobileCredentialBundle>> {
        let mut runtimes = self.load()?;
        runtimes.retain(|runtime| runtime.host_id() != host_id);
        self.save(&runtimes)?;
        Ok(runtimes)
    }
}

fn read_error(error: &std::io::Error) -> BackendError {
    BackendError::failed(
        "mobile_pairing_store_unreadable",
        format!("the pairing store cannot be read: {error}"),
    )
}

fn write_error(error: &std::io::Error) -> BackendError {
    BackendError::failed(
        "mobile_pairing_store_write_failed",
        format!("the pairing store cannot be written: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::{
        MobileLocalNetworkCandidate, MobileRelayCandidate, MobileRemoteRouteBundle,
    };
    use vibex_core::{DeviceId, RelayPeerId, RelayRoomId, RemoteAuthProof, RemoteClientType};
    use vibex_remote_client::RemoteCredentialRecord;

    fn bundle(host: &str) -> MobileCredentialBundle {
        MobileCredentialBundle {
            schema_version: crate::pairing::MOBILE_CREDENTIAL_SCHEMA_VERSION.to_owned(),
            record: RemoteCredentialRecord {
                server_url: format!("https://{host}.example"),
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "grant".to_owned(),
                },
                device_identity_public_key: "public-key".to_owned(),
                server_identity_public_key: Some("server-key".to_owned()),
            },
            identity_private_key: "private-key".to_owned(),
            expected_server_id: host.to_owned(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            display_name: Some(host.to_owned()),
            route: None,
        }
    }

    fn store() -> (PairingStore, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "vibex-pairing-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        (PairingStore::new(directory.join("pairing.json")), directory)
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let (store, directory) = store();
        assert_eq!(store.load().unwrap(), Vec::new());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn runtimes_round_trip_most_recent_first() {
        let (store, directory) = store();
        assert!(store.load().unwrap().is_empty());
        store.upsert(bundle("desktop")).unwrap();
        let runtimes = store.upsert(bundle("laptop")).unwrap();
        assert_eq!(
            runtimes
                .iter()
                .map(|runtime| runtime.expected_server_id.as_str())
                .collect::<Vec<_>>(),
            vec!["laptop", "desktop"]
        );
        // Re-pairing the same host replaces its entry instead of adding one.
        let runtimes = store.upsert(bundle("desktop")).unwrap();
        assert_eq!(
            runtimes
                .iter()
                .map(|runtime| runtime.expected_server_id.as_str())
                .collect::<Vec<_>>(),
            vec!["desktop", "laptop"]
        );
        assert_eq!(store.load().unwrap().len(), 2);

        let runtimes = store.remove("desktop").unwrap();
        assert_eq!(runtimes.len(), 1);
        assert_eq!(runtimes[0].expected_server_id, "laptop");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn upsert_merges_routes_across_repairings_of_the_same_host() {
        let (store, directory) = store();
        let mut lan = bundle("desktop");
        lan.display_name = Some("Studio Mac".to_owned());
        lan.route = Some(MobileRemoteRouteBundle {
            local_network: Some(MobileLocalNetworkCandidate {
                url: "https://192.168.1.50:8443".to_owned(),
                tls_certificate_der: "cert".to_owned(),
            }),
            direct_candidates: Vec::new(),
            relay: None,
        });
        store.upsert(lan).unwrap();

        let mut tailscale = bundle("desktop");
        tailscale.display_name = None;
        tailscale.route = Some(MobileRemoteRouteBundle {
            local_network: None,
            direct_candidates: vec!["https://studio.tailnet.ts.net".to_owned()],
            relay: None,
        });
        store.upsert(tailscale).unwrap();

        let mut relay = bundle("desktop");
        relay.display_name = None;
        relay.route = Some(MobileRemoteRouteBundle {
            local_network: None,
            direct_candidates: Vec::new(),
            relay: Some(MobileRelayCandidate {
                url: "https://relay.example.com".to_owned(),
                room_id: RelayRoomId::new(),
                local_peer_id: RelayPeerId::new(),
                pc_peer_id: RelayPeerId::new(),
                pc_public_key: "pub".to_owned(),
            }),
        });
        let runtimes = store.upsert(relay).unwrap();
        assert_eq!(runtimes.len(), 1);
        let merged = &runtimes[0];
        assert_eq!(merged.display_name.as_deref(), Some("Studio Mac"));
        let route = merged.route.as_ref().unwrap();
        assert_eq!(
            route.local_network.as_ref().map(|c| c.url.as_str()),
            Some("https://192.168.1.50:8443")
        );
        assert_eq!(
            route.direct_candidates,
            vec!["https://studio.tailnet.ts.net".to_owned()]
        );
        assert_eq!(
            route.relay.as_ref().map(|c| c.url.as_str()),
            Some("https://relay.example.com")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn update_in_place_preserves_order_and_ignores_removed_hosts() {
        let (store, directory) = store();
        store.upsert(bundle("desktop")).unwrap();
        store.upsert(bundle("laptop")).unwrap();

        let mut updated = bundle("desktop");
        updated.display_name = Some("Studio Mac".to_owned());
        let runtimes = store.update_in_place(updated).unwrap();
        assert_eq!(
            runtimes
                .iter()
                .map(|r| (r.expected_server_id.as_str(), r.display_name.as_deref()))
                .collect::<Vec<_>>(),
            vec![("laptop", Some("laptop")), ("desktop", Some("Studio Mac"))]
        );

        let runtimes = store.update_in_place(bundle("deleted")).unwrap();
        assert_eq!(runtimes.len(), 2);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn an_incompatible_store_is_refused() {
        let (store, directory) = store();
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            store.path(),
            br#"{"schemaVersion":"vibex-mobile-pairing-store.v99","runtimes":[]}"#,
        )
        .unwrap();
        let error = store.load().unwrap_err();
        assert_eq!(error.code, "mobile_pairing_store_incompatible");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_corrupt_store_is_reported() {
        let (store, directory) = store();
        fs::create_dir_all(&directory).unwrap();
        fs::write(store.path(), b"not json").unwrap();
        let error = store.load().unwrap_err();
        assert_eq!(error.code, "mobile_pairing_store_invalid");
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let (store, directory) = store();
        store.upsert(bundle("desktop")).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        let _ = fs::remove_dir_all(directory);
    }
}
