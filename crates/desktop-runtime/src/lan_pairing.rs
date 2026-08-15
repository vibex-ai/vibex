use std::sync::Mutex;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use url::Url;
use vibex_core::{RemoteLanPairingAdvertisement, VibexError, VibexResult};

pub const LAN_PAIRING_SERVICE_TYPE: &str = "_vibex._tcp.local.";

pub trait LanPairingAdvertiser: Send + Sync {
    fn start(&self, advertisement: &RemoteLanPairingAdvertisement) -> VibexResult<()>;
    fn stop(&self) -> VibexResult<()>;
}

#[derive(Default)]
pub struct MdnsLanPairingAdvertiser {
    registered: Mutex<Option<RegisteredService>>,
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
}
