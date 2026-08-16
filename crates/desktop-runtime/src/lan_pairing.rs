use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use url::Url;
use vibex_core::{
    RemoteLanPairingAdvertisement, RemoteZeroConfigLanPairingAdvertisement, VibexError, VibexResult,
};

pub const LAN_PAIRING_SERVICE_TYPE: &str = "_vibex._tcp.local.";

pub trait LanPairingAdvertiser: Send + Sync {
    fn start(&self, advertisement: &RemoteLanPairingAdvertisement) -> VibexResult<()>;
    fn stop(&self) -> VibexResult<()>;
}

pub trait ZeroConfigLanPairingAdvertiser: Send + Sync {
    fn start(&self, advertisement: &RemoteZeroConfigLanPairingAdvertisement) -> VibexResult<()>;
    fn stop(&self) -> VibexResult<()>;
}

#[derive(Default)]
pub struct MdnsLanPairingAdvertiser {
    registered: Mutex<Option<RegisteredService>>,
}

#[derive(Default)]
pub struct MdnsZeroConfigLanPairingAdvertiser {
    registered: Mutex<Option<RegisteredService>>,
}

impl std::fmt::Debug for MdnsZeroConfigLanPairingAdvertiser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MdnsZeroConfigLanPairingAdvertiser")
            .field(
                "advertising",
                &self
                    .registered
                    .lock()
                    .is_ok_and(|registered| registered.is_some()),
            )
            .finish()
    }
}

impl std::fmt::Debug for MdnsLanPairingAdvertiser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MdnsLanPairingAdvertiser")
            .field(
                "advertising",
                &self
                    .registered
                    .lock()
                    .is_ok_and(|registered| registered.is_some()),
            )
            .finish()
    }
}

struct RegisteredService {
    daemon: ServiceDaemon,
    fullname: String,
}

impl LanPairingAdvertiser for MdnsLanPairingAdvertiser {
    fn start(&self, advertisement: &RemoteLanPairingAdvertisement) -> VibexResult<()> {
        validate_advertisement(advertisement)?;
        self.stop()?;

        let origin =
            Url::parse(&advertisement.direct_origin).map_err(|_| invalid_advertisement())?;
        let host = origin.host_str().ok_or_else(invalid_advertisement)?;
        let port = origin
            .port_or_known_default()
            .ok_or_else(invalid_advertisement)?;
        let properties = vec![
            ("version", "1".to_string()),
            ("advertisement_id", advertisement.advertisement_id.clone()),
            ("display_name", advertisement.display_name.clone()),
            ("protocol_min", advertisement.protocol_min.to_string()),
            ("protocol_max", advertisement.protocol_max.to_string()),
            ("pairing", "available".to_string()),
        ];
        let service = ServiceInfo::new(
            LAN_PAIRING_SERVICE_TYPE,
            &advertisement.service_instance,
            host,
            "",
            port,
            properties.as_slice(),
        )
        .map_err(|_| invalid_advertisement())?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_string();
        let daemon = ServiceDaemon::new().map_err(|_| {
            VibexError::process(
                "remote_lan_advertiser_unavailable",
                "the LAN pairing advertiser could not start",
            )
        })?;
        if daemon.register(service).is_err() {
            let _ = daemon.shutdown();
            return Err(VibexError::process(
                "remote_lan_advertisement_failed",
                "the LAN pairing service could not be advertised",
            ));
        }
        let mut registered = self.registered.lock().map_err(|_| state_error())?;
        *registered = Some(RegisteredService { daemon, fullname });
        Ok(())
    }

    fn stop(&self) -> VibexResult<()> {
        let registered = self.registered.lock().map_err(|_| state_error())?.take();
        if let Some(registered) = registered {
            let _ = registered.daemon.unregister(&registered.fullname);
            let _ = registered.daemon.shutdown();
        }
        Ok(())
    }
}

impl ZeroConfigLanPairingAdvertiser for MdnsZeroConfigLanPairingAdvertiser {
    fn start(&self, advertisement: &RemoteZeroConfigLanPairingAdvertisement) -> VibexResult<()> {
        validate_zero_config_advertisement(advertisement)?;
        self.stop()?;

        let properties = vec![
            ("version", "1".to_string()),
            ("mode", "zero_config".to_string()),
            ("advertisement_id", advertisement.advertisement_id.clone()),
            ("display_name", advertisement.display_name.clone()),
            ("server_id", advertisement.server_id.clone()),
            (
                "server_identity_public_key",
                advertisement.server_identity_public_key.clone(),
            ),
            ("protocol_min", advertisement.protocol_min.to_string()),
            ("protocol_max", advertisement.protocol_max.to_string()),
            ("pairing", "available".to_string()),
        ];
        let host_suffix = advertisement
            .advertisement_id
            .rsplit_once('-')
            .map_or(advertisement.advertisement_id.as_str(), |(_, suffix)| {
                suffix
            })
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect::<String>();
        let host = format!("vibex-{host_suffix}.local.");
        let service = ServiceInfo::new(
            LAN_PAIRING_SERVICE_TYPE,
            &advertisement.service_instance,
            &host,
            "",
            advertisement.local_port,
            properties.as_slice(),
        )
        .map_err(|_| invalid_advertisement())?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_string();
        let daemon = ServiceDaemon::new().map_err(|_| {
            VibexError::process(
                "remote_zero_config_advertiser_unavailable",
                "zero-config LAN pairing advertiser could not start",
            )
        })?;
        if daemon.register(service).is_err() {
            let _ = daemon.shutdown();
            return Err(VibexError::process(
                "remote_zero_config_advertisement_failed",
                "zero-config LAN pairing service could not be advertised",
            ));
        }
        let mut registered = self.registered.lock().map_err(|_| state_error())?;
        *registered = Some(RegisteredService { daemon, fullname });
        Ok(())
    }

    fn stop(&self) -> VibexResult<()> {
        stop_registered(&self.registered)
    }
}

fn validate_advertisement(advertisement: &RemoteLanPairingAdvertisement) -> VibexResult<()> {
    let txt_bytes = advertisement.advertisement_id.len()
        + advertisement.display_name.len()
        + advertisement.protocol_min.to_string().len()
        + advertisement.protocol_max.to_string().len()
        + 96;
    if advertisement.service_instance.is_empty()
        || advertisement.service_instance.len() > 63
        || advertisement.display_name.is_empty()
        || advertisement.display_name.len() > 192
        || advertisement.advertisement_id.len() < 16
        || advertisement.advertisement_id.len() > 128
        || advertisement.protocol_min != 2
        || advertisement.protocol_max != 2
        || txt_bytes > 512
    {
        return Err(invalid_advertisement());
    }
    let url = Url::parse(&advertisement.direct_origin).map_err(|_| invalid_advertisement())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(invalid_advertisement());
    }
    Ok(())
}

fn validate_zero_config_advertisement(
    advertisement: &RemoteZeroConfigLanPairingAdvertisement,
) -> VibexResult<()> {
    let public_key_valid = URL_SAFE_NO_PAD
        .decode(&advertisement.server_identity_public_key)
        .ok()
        .is_some_and(|key| key.len() == 32 && key.iter().any(|byte| *byte != 0));
    let txt_bytes = advertisement.advertisement_id.len()
        + advertisement.display_name.len()
        + advertisement.server_id.len()
        + advertisement.server_identity_public_key.len()
        + 160;
    if advertisement.service_instance.is_empty()
        || advertisement.service_instance.len() > 63
        || advertisement.display_name.is_empty()
        || advertisement.display_name.len() > 192
        || advertisement.advertisement_id.len() < 16
        || advertisement.advertisement_id.len() > 128
        || advertisement.server_id.is_empty()
        || advertisement.server_id.len() > 128
        || !public_key_valid
        || advertisement.local_port == 0
        || advertisement.protocol_min != 2
        || advertisement.protocol_max != 2
        || txt_bytes > 512
    {
        return Err(invalid_advertisement());
    }
    Ok(())
}

fn stop_registered(registered: &Mutex<Option<RegisteredService>>) -> VibexResult<()> {
    let registered = registered.lock().map_err(|_| state_error())?.take();
    if let Some(registered) = registered {
        let _ = registered.daemon.unregister(&registered.fullname);
        let _ = registered.daemon.shutdown();
    }
    Ok(())
}

fn invalid_advertisement() -> VibexError {
    VibexError::validation(
        "remote_lan_advertisement_invalid",
        "LAN pairing advertisement is invalid or exceeds its bounds",
    )
}

fn state_error() -> VibexError {
    VibexError::process(
        "remote_lan_advertiser_state_unavailable",
        "LAN pairing advertiser state is unavailable",
    )
}

impl Drop for MdnsLanPairingAdvertiser {
    fn drop(&mut self) {
        if let Ok(registered) = self.registered.get_mut()
            && let Some(registered) = registered.take()
        {
            let _ = registered.daemon.unregister(&registered.fullname);
            let _ = registered.daemon.shutdown();
        }
    }
}

impl Drop for MdnsZeroConfigLanPairingAdvertiser {
    fn drop(&mut self) {
        if let Ok(registered) = self.registered.get_mut()
            && let Some(registered) = registered.take()
        {
            let _ = registered.daemon.unregister(&registered.fullname);
            let _ = registered.daemon.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisement_validation_rejects_oversized_metadata() {
        let advertisement = RemoteLanPairingAdvertisement {
            advertisement_id: "adv-01234567890123456789".into(),
            service_instance: "Vibex Desktop-01234567".into(),
            display_name: "x".repeat(400),
            direct_origin: "https://desktop.example.test".into(),
            protocol_min: 2,
            protocol_max: 2,
        };
        assert_eq!(
            validate_advertisement(&advertisement).unwrap_err().code,
            "remote_lan_advertisement_invalid"
        );
    }

    #[test]
    fn zero_config_advertisement_requires_a_valid_desktop_identity() {
        let mut advertisement = RemoteZeroConfigLanPairingAdvertisement {
            advertisement_id: "local-adv-0123456789012345".into(),
            service_instance: "Vibex Desktop-01234567".into(),
            display_name: "Vibex Desktop".into(),
            server_id: "desktop-test".into(),
            server_identity_public_key: URL_SAFE_NO_PAD.encode([7u8; 32]),
            local_port: 4242,
            protocol_min: 2,
            protocol_max: 2,
        };
        validate_zero_config_advertisement(&advertisement).unwrap();

        advertisement.server_identity_public_key = URL_SAFE_NO_PAD.encode([0u8; 32]);
        assert_eq!(
            validate_zero_config_advertisement(&advertisement)
                .unwrap_err()
                .code,
            "remote_lan_advertisement_invalid"
        );
    }
}
