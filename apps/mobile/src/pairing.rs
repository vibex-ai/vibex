use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{
    DeviceId, RelayPeerId, RemoteAuthProof, RemoteClientType, RemoteDeviceStatus,
    RemotePairingOffer, RemotePairingTransport, RequestId,
};
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity, DirectCandidate,
    PAIRING_FRAGMENT_PREFIX, PairingClaimRoute, PairingEntryHint, PairingEntryHintKind,
    RelayClientConfig, RemoteClientConfig, RemoteCredentialRecord, WebRemoteBackend,
    claim_pairing_offer, claim_pairing_offer_via_relay, pairing_claim_request,
    parse_pairing_offer_fragment, select_pairing_claim_route,
};

pub const MOBILE_CREDENTIAL_SCHEMA_VERSION: &str = "vibex-native-mobile-credentials.v1";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MobileCredentialBundle {
    pub schema_version: String,
    pub record: RemoteCredentialRecord,
    pub identity_private_key: String,
    pub expected_server_id: String,
    pub client_type: RemoteClientType,
    #[serde(default)]
    pub allow_insecure_local_dev: bool,
    #[serde(default)]
    pub route: Option<MobileRemoteRouteBundle>,
}

impl fmt::Debug for MobileCredentialBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MobileCredentialBundle")
            .field("schema_version", &self.schema_version)
            .field("has_auth_token", &!self.record.auth.auth_token.is_empty())
            .field(
                "has_device_identity_public_key",
                &!self.record.device_identity_public_key.is_empty(),
            )
            .field(
                "has_server_identity_public_key",
                &self.record.server_identity_public_key.is_some(),
            )
            .field(
                "has_identity_private_key",
                &!self.identity_private_key.is_empty(),
            )
            .field(
                "has_expected_server_id",
                &!self.expected_server_id.is_empty(),
            )
            .field("client_type", &self.client_type)
            .field("allow_insecure_local_dev", &self.allow_insecure_local_dev)
            .field("route", &self.route)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MobileRemoteRouteBundle {
    #[serde(default)]
    pub direct_candidates: Vec<String>,
    pub relay: Option<MobileRelayCandidate>,
}

impl fmt::Debug for MobileRemoteRouteBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MobileRemoteRouteBundle")
            .field("direct_candidate_count", &self.direct_candidates.len())
            .field("has_relay", &self.relay.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MobileRelayCandidate {
    pub url: String,
    pub room_id: vibex_core::RelayRoomId,
    pub local_peer_id: RelayPeerId,
    pub pc_peer_id: RelayPeerId,
    pub pc_public_key: String,
}

impl fmt::Debug for MobileRelayCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MobileRelayCandidate")
            .field("has_pc_public_key", &!self.pc_public_key.is_empty())
            .finish()
    }
}

impl MobileCredentialBundle {
    pub fn validate(&self) -> BackendResult<()> {
        if self.schema_version != MOBILE_CREDENTIAL_SCHEMA_VERSION
            || self.client_type != RemoteClientType::Mobile
            || self.expected_server_id.trim().is_empty()
            || self.identity_private_key.trim().is_empty()
            || self.record.auth.auth_token.trim().is_empty()
            || self.record.device_identity_public_key.trim().is_empty()
            || self.record.server_identity_public_key.is_none()
        {
            return Err(BackendError::failed(
                "mobile_credentials_invalid",
                "native mobile credentials failed identity or version validation",
            ));
        }
        let identity = ClientDeviceIdentity::from_private_key_base64(
            self.record.auth.device_id.clone(),
            &self.identity_private_key,
        )?;
        if identity.public_key_base64() != self.record.device_identity_public_key {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "stored client identity does not match the remote device grant",
            ));
        }
        let config = self.auto_transport_config()?;
        AutoRemoteTransport::new(config).map(|_| ())
    }

    pub fn client_config(&self) -> BackendResult<RemoteClientConfig> {
        let identity = ClientDeviceIdentity::from_private_key_base64(
            self.record.auth.device_id.clone(),
            &self.identity_private_key,
        )?;
        let mut config = RemoteClientConfig::from_credentials(self.record.clone(), identity)?;
        config.expected_server_id = Some(self.expected_server_id.clone());
        config.client_id = "vibex-mobile".to_string();
        config.client_type = RemoteClientType::Mobile;
        config.allow_insecure_local_dev = self.allow_insecure_local_dev && cfg!(debug_assertions);
        config.validate()?;
        Ok(config)
    }

    pub fn backend(&self) -> BackendResult<Arc<WebRemoteBackend>> {
        self.validate()?;
        let transport = AutoRemoteTransport::new(self.auto_transport_config()?)?;
        Ok(Arc::new(WebRemoteBackend::from_auto(transport)))
    }

    fn auto_transport_config(&self) -> BackendResult<AutoRemoteTransportConfig> {
        let config = self.client_config()?;
        let route = self.route.as_ref().ok_or_else(|| {
            BackendError::failed(
                "mobile_route_missing",
                "native mobile credentials do not contain a remote route",
            )
        })?;
        let direct_candidates = route
            .direct_candidates
            .iter()
            .enumerate()
            .map(|(index, url)| {
                let mut candidate_config = config.clone();
                candidate_config.base_url = url.clone();
                candidate_config.validate()?;
                Ok(DirectCandidate {
                    url: url.clone(),
                    label: format!("direct-{index}"),
                    priority: u8::try_from(index).unwrap_or(u8::MAX),
                })
            })
            .collect::<BackendResult<Vec<_>>>()?;
        let relay = route
            .relay
            .as_ref()
            .map(|relay| {
                if relay.pc_public_key.trim().is_empty() {
                    return Err(invalid_relay());
                }
                Ok(RelayClientConfig {
                    relay_url: relay.url.clone(),
                    room_id: relay.room_id.clone(),
                    local_peer_id: relay.local_peer_id.clone(),
                    pc_peer_id: relay.pc_peer_id.clone(),
                    pc_public_key: Some(relay.pc_public_key.clone()),
                    remote: config.clone(),
                })
            })
            .transpose()?;
        Ok(AutoRemoteTransportConfig {
            remote: config,
            direct_candidates,
            relay,
        })
    }
}

pub async fn claim_pairing_link(link: String) -> BackendResult<MobileCredentialBundle> {
    let now_ms = vibex_core::unix_timestamp_ms();
    let offer = parse_pairing_offer_fragment(&link, now_ms)?;
    let transport = pairing_link_transport(&link)?.unwrap_or(preferred_transport(&offer)?);
    let hint = PairingEntryHint {
        schema_version: vibex_remote_client::PAIRING_ENTRY_HINT_SCHEMA_VERSION.to_string(),
        kind: PairingEntryHintKind::MobileApp,
        origin: None,
        transport: Some(transport),
    };
    let route = select_pairing_claim_route(&offer, &hint)?;
    let provisional_identity = ClientDeviceIdentity::generate(DeviceId::new())?;
    let request = pairing_claim_request(
        &offer,
        "Vibex Mobile",
        provisional_identity.public_key_base64(),
        RequestId::new().into_string(),
    )?;
    let private_key = provisional_identity.private_key_base64();
    let response = match &route {
        PairingClaimRoute::Direct { claim_base_url, .. } => {
            claim_pairing_offer(
                claim_base_url.clone(),
                request.clone(),
                cfg!(debug_assertions),
            )
            .await
        }
        PairingClaimRoute::Relay(relay) => {
            claim_pairing_offer_via_relay(
                relay.url.clone(),
                relay.relay_room_id.clone().ok_or_else(invalid_relay)?,
                RelayPeerId::new(),
                relay.relay_pc_peer_id.clone().ok_or_else(invalid_relay)?,
                relay
                    .relay_pc_public_key
                    .clone()
                    .ok_or_else(invalid_relay)?,
                request.clone(),
                provisional_identity.clone(),
                cfg!(debug_assertions),
            )
            .await
        }
    }?;
    if response.device.status != RemoteDeviceStatus::Active
        || response.device.public_key.as_deref()
            != Some(request.device_identity_public_key.as_str())
        || response.device_grant_token.trim().is_empty()
    {
        return Err(BackendError::failed(
            "remote_pairing_claim_response_invalid",
            "pairing claim did not return the expected active device grant",
        ));
    }
    let identity = ClientDeviceIdentity::from_private_key_base64(
        response.device.device_id.clone(),
        &private_key,
    )?;
    let bundle = MobileCredentialBundle {
        schema_version: MOBILE_CREDENTIAL_SCHEMA_VERSION.to_string(),
        record: RemoteCredentialRecord {
            server_url: match &route {
                PairingClaimRoute::Direct { claim_base_url, .. } => claim_base_url.clone(),
                PairingClaimRoute::Relay(relay) => relay.url.clone(),
            },
            auth: RemoteAuthProof {
                device_id: response.device.device_id,
                auth_token: response.device_grant_token,
            },
            device_identity_public_key: identity.public_key_base64(),
            server_identity_public_key: Some(offer.summary.server_identity_public_key.clone()),
        },
        identity_private_key: identity.private_key_base64(),
        expected_server_id: offer.summary.server_id.clone(),
        client_type: RemoteClientType::Mobile,
        allow_insecure_local_dev: cfg!(debug_assertions),
        route: Some(route_bundle(&offer)),
    };
    bundle.validate()?;
    Ok(bundle)
}

fn pairing_link_transport(link: &str) -> BackendResult<Option<RemotePairingTransport>> {
    let link = link.trim();
    if link.starts_with(PAIRING_FRAGMENT_PREFIX) {
        return Ok(None);
    }
    let url = Url::parse(link).map_err(|_| invalid_pairing_entry())?;
    if url.scheme() != "vibex"
        || url.host_str() != Some("open")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
    {
        return Err(invalid_pairing_entry());
    }
    let transport = match url.path() {
        "/direct" => RemotePairingTransport::Direct,
        "/tailnet" => RemotePairingTransport::Tailnet,
        "/self_hosted_relay" => RemotePairingTransport::SelfHostedRelay,
        _ => return Err(invalid_pairing_entry()),
    };
    Ok(Some(transport))
}

fn preferred_transport(offer: &RemotePairingOffer) -> BackendResult<RemotePairingTransport> {
    offer
        .summary
        .direct_candidates
        .iter()
        .find(|candidate| candidate.transport == RemotePairingTransport::Direct)
        .or_else(|| {
            offer
                .summary
                .direct_candidates
                .iter()
                .find(|candidate| candidate.transport == RemotePairingTransport::Tailnet)
        })
        .map(|candidate| candidate.transport)
        .or_else(|| {
            offer
                .summary
                .relay_candidate
                .as_ref()
                .map(|candidate| candidate.transport)
        })
        .ok_or_else(|| {
            BackendError::unsupported(
                "remote_pairing_route_missing",
                "pairing offer does not contain a native mobile route",
            )
        })
}

fn route_bundle(offer: &RemotePairingOffer) -> MobileRemoteRouteBundle {
    let direct_candidates = offer
        .summary
        .direct_candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.transport,
                RemotePairingTransport::Direct | RemotePairingTransport::Tailnet
            )
        })
        .map(|candidate| candidate.url.clone())
        .collect();
    let relay = offer
        .summary
        .relay_candidate
        .as_ref()
        .and_then(|candidate| {
            Some(MobileRelayCandidate {
                url: candidate.url.clone(),
                room_id: candidate.relay_room_id.clone()?,
                local_peer_id: RelayPeerId::new(),
                pc_peer_id: candidate.relay_pc_peer_id.clone()?,
                pc_public_key: candidate.relay_pc_public_key.clone()?,
            })
        });
    MobileRemoteRouteBundle {
        direct_candidates,
        relay,
    }
}

fn invalid_relay() -> BackendError {
    BackendError::failed(
        "remote_pairing_relay_candidate_invalid",
        "Relay pairing candidate is incomplete",
    )
}

fn invalid_pairing_entry() -> BackendError {
    BackendError::failed(
        "remote_pairing_entry_invalid",
        "pairing link is not a trusted Vibex mobile entry",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_output_redacts_secrets() {
        let private_route = "https://private-route.example";
        let bundle = MobileCredentialBundle {
            schema_version: MOBILE_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: RemoteCredentialRecord {
                server_url: "https://desktop.example".to_string(),
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "super-secret-grant".to_string(),
                },
                device_identity_public_key: "public-key".to_string(),
                server_identity_public_key: Some("server-key".to_string()),
            },
            identity_private_key: "super-secret-private-key".to_string(),
            expected_server_id: "private-desktop-id".to_string(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            route: Some(MobileRemoteRouteBundle {
                direct_candidates: vec![private_route.to_string()],
                relay: None,
            }),
        };

        let debug = format!("{bundle:?}");
        assert!(!debug.contains("super-secret-grant"));
        assert!(!debug.contains("super-secret-private-key"));
        assert!(!debug.contains(private_route));
        assert!(!debug.contains("private-desktop-id"));
        assert!(debug.contains("has_auth_token: true"));
        assert!(debug.contains("has_identity_private_key: true"));
    }

    #[test]
    fn trusted_mobile_link_preserves_the_desktop_selected_transport() {
        for (name, expected) in [
            ("direct", RemotePairingTransport::Direct),
            ("tailnet", RemotePairingTransport::Tailnet),
            ("self_hosted_relay", RemotePairingTransport::SelfHostedRelay),
        ] {
            let link = format!("vibex://open/{name}#/pair/encoded-offer");
            assert_eq!(pairing_link_transport(&link).unwrap(), Some(expected));
        }
        assert_eq!(
            pairing_link_transport("#/pair/encoded-offer").unwrap(),
            None
        );
        assert_eq!(
            pairing_link_transport("https://host.example/#/pair/encoded-offer")
                .unwrap_err()
                .code,
            "remote_pairing_entry_invalid"
        );
    }

    #[test]
    fn credential_validation_rejects_tampered_route_urls_before_network_use() {
        let identity = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        let bundle = MobileCredentialBundle {
            schema_version: MOBILE_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: RemoteCredentialRecord {
                server_url: "https://desktop.example".to_string(),
                auth: RemoteAuthProof {
                    device_id: identity.device_id().clone(),
                    auth_token: "grant".to_string(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some("server-public".to_string()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: "desktop".to_string(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            route: Some(MobileRemoteRouteBundle {
                direct_candidates: vec!["https://desktop.example?token=leak".to_string()],
                relay: None,
            }),
        };

        assert_eq!(
            bundle.validate().unwrap_err().code,
            "remote_url_secret_boundary_invalid"
        );
    }
}
