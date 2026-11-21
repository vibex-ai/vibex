//! Local-network discovery of runtimes, in Rust.
//!
//! The runtime advertises `_vibex._tcp.local.` with the zero-config TXT record
//! (`mode=zero_config`, its server id and identity key). Browsing it here keeps
//! the phone's "Find" working in the standalone app, without a Kotlin or Swift
//! host: `mdns-sd` multicasts on the LAN, and a candidate is only passed on
//! when the record is the shape the pairing session expects.

use std::{collections::BTreeMap, net::Ipv4Addr, thread, time::Duration};

use futures_channel::mpsc::UnboundedSender;
use mdns_sd::{Receiver, RecvTimeoutError, ResolvedService, ServiceDaemon, ServiceEvent};

/// The service type the desktop runtime advertises.
pub const SERVICE_TYPE: &str = "_vibex._tcp.local.";

/// A runtime the local network found and the phone may pair with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Stable while the service stays visible (its full name).
    pub key: String,
    /// The name the desktop advertises.
    pub name: String,
    /// The zero-config origins to try, the most likely reachable first.
    ///
    /// A runtime advertises every address of every interface it has (a VPN, a
    /// container bridge and the LAN address itself), so a service record is
    /// not one address: dialling an arbitrary one is how "Find" ends up
    /// waiting on an address the phone cannot route to.
    pub origins: Vec<String>,
    pub server_id: String,
    /// The server's identity key, URL-safe base64.
    pub server_key: String,
}

/// What a browse reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Found(Candidate),
    Lost(String),
    Failed,
}

/// a browsing session; dropping it stops the browse.
pub struct Discovery {
    daemon: ServiceDaemon,
}

impl Discovery {
    /// Stops browsing (the daemon's threads end).
    pub fn stop(&self) {
        let _ = self.daemon.shutdown();
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Starts browsing; `None` when the mDNS daemon cannot start.
pub fn start(sender: UnboundedSender<Event>) -> Option<Discovery> {
    let daemon = ServiceDaemon::new().ok()?;
    let receiver: Receiver<ServiceEvent> = match daemon.browse(SERVICE_TYPE) {
        Ok(receiver) => receiver,
        Err(_) => {
            let _ = daemon.shutdown();
            let _ = sender.unbounded_send(Event::Failed);
            return None;
        }
    };
    thread::spawn(move || {
        loop {
            match receiver.recv_timeout(Duration::from_millis(500)) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some(candidate) = candidate_of(&info) {
                        let _ = sender.unbounded_send(Event::Found(candidate));
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                    let _ = sender.unbounded_send(Event::Lost(fullname));
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Some(Discovery { daemon })
}

/// The candidate a resolved service describes, when it is the zero-config
/// record this client can pair with.
fn candidate_of(info: &ResolvedService) -> Option<Candidate> {
    let txt = info
        .get_properties()
        .iter()
        .map(|property| (property.key().to_owned(), property.val_str().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let addresses = info.get_addresses_v4().into_iter().collect::<Vec<_>>();
    candidate_from_parts(
        info.get_fullname(),
        info.get_port(),
        addresses,
        &txt,
        info.get_property_val_str("display_name"),
    )
}

/// The validated candidate for one resolved advertisement. Split out so the
/// record's rules are unit-tested without a network.
fn candidate_from_parts(
    fullname: &str,
    port: u16,
    addresses: Vec<Ipv4Addr>,
    txt: &BTreeMap<String, String>,
    display_name: Option<&str>,
) -> Option<Candidate> {
    if port == 0 {
        return None;
    }
    if txt.get("version").map(String::as_str) != Some("1")
        || txt.get("pairing").map(String::as_str) != Some("available")
        || txt.get("protocol_min").map(String::as_str) != Some("2")
        || txt.get("protocol_max").map(String::as_str) != Some("2")
    {
        return None;
    }
    let mode = txt.get("mode").map(String::as_str);
    let server_id = txt.get("server_id")?.trim();
    let server_key = txt.get("server_identity_public_key")?.trim();
    if mode != Some("zero_config")
        || server_id.is_empty()
        || server_id.len() > 128
        || server_key.is_empty()
    {
        return None;
    }
    let origins = origins_for(addresses, port);
    let name = display_name
        .or_else(|| txt.get("display_name").map(String::as_str))
        .unwrap_or(fullname)
        .trim();
    let name = if name.is_empty() { fullname } else { name };
    Some(Candidate {
        key: fullname.to_owned(),
        name: name.to_owned(),
        origins,
        server_id: server_id.to_owned(),
        server_key: server_key.to_owned(),
    })
}

/// The origins to try for one advertisement, best first.
///
/// The phone reaches the runtime over the interface it shares with it, so the
/// address in the phone's own subnet is tried first; the remaining private
/// addresses, then anything else, follow rather than being dropped.
fn origins_for(addresses: Vec<Ipv4Addr>, port: u16) -> Vec<String> {
    rank_ipv4(addresses)
        .into_iter()
        .map(|address| format!("http://{address}:{port}"))
        .collect()
}

/// The origins for the addresses a platform host reported.
///
/// A host bridge sends the record's addresses as text; a literal IPv4 address
/// is ranked like the mDNS path's, and anything else (a name, an IPv6 address)
/// is kept last instead of being dropped.
pub(crate) fn origins_from_hosts(hosts: Vec<String>, port: u16) -> Vec<String> {
    let mut parsed = Vec::new();
    let mut other = Vec::new();
    for host in hosts {
        let host = host.trim();
        if host.is_empty() {
            continue;
        }
        match host.parse::<Ipv4Addr>() {
            Ok(address) => parsed.push(address),
            Err(_) => other.push(host.to_owned()),
        }
    }
    let mut origins = rank_ipv4(parsed)
        .into_iter()
        .map(|address| format!("http://{address}:{port}"))
        .collect::<Vec<_>>();
    origins.extend(
        other
            .into_iter()
            .map(|host| format!("http://{host}:{port}")),
    );
    origins
}

/// Orders addresses by how likely the phone can route to them.
pub(crate) fn rank_ipv4(addresses: Vec<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let local = local_ipv4();
    let mut addresses = addresses;
    addresses.sort_by_key(|address| address_rank(*address, local));
    addresses.dedup();
    addresses
}

fn address_rank(address: Ipv4Addr, local: Option<Ipv4Addr>) -> (u8, u32) {
    if address.is_loopback() {
        return (9, u32::from(address));
    }
    if let Some(local) = local
        && same_24(address, local)
    {
        return (0, 0);
    }
    let octets = address.octets();
    let bucket = if octets[0] == 192 && octets[1] == 168 {
        1
    } else if octets[0] == 10 {
        2
    } else if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        3
    } else if address.is_link_local() {
        5
    } else {
        4
    };
    (bucket, u32::from(address))
}

fn same_24(left: Ipv4Addr, right: Ipv4Addr) -> bool {
    left.octets()[..3] == right.octets()[..3]
}

/// The address this device uses for the network, when it has one.
///
/// Connecting a UDP socket sends nothing; it only asks the routing table which
/// source address it would use, which is the address of the interface the
/// runtime we are pairing with would see.
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()? {
        std::net::SocketAddr::V4(address) => Some(*address.ip()),
        std::net::SocketAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{candidate_from_parts, origins_for};
    use std::{collections::BTreeMap, net::Ipv4Addr};

    fn txt(mode: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("version".to_owned(), "1".to_owned()),
            ("pairing".to_owned(), "available".to_owned()),
            ("protocol_min".to_owned(), "2".to_owned()),
            ("protocol_max".to_owned(), "2".to_owned()),
            ("mode".to_owned(), mode.to_owned()),
            ("server_id".to_owned(), "server-1".to_owned()),
            (
                "server_identity_public_key".to_owned(),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            ),
            ("display_name".to_owned(), "Studio".to_owned()),
        ])
    }

    #[test]
    fn a_zero_config_record_becomes_a_candidate() {
        let candidate = candidate_from_parts(
            "_vibex._tcp.local",
            4123,
            vec![Ipv4Addr::new(192, 168, 1, 20)],
            &txt("zero_config"),
            None,
        )
        .expect("a valid record");
        assert_eq!(candidate.name, "Studio");
        assert_eq!(candidate.origins, ["http://192.168.1.20:4123"]);
        assert_eq!(candidate.server_id, "server-1");
    }

    #[test]
    fn a_runtime_with_several_addresses_offers_the_lan_one_first() {
        let origins = origins_for(
            vec![
                Ipv4Addr::new(172, 17, 0, 1),
                Ipv4Addr::new(100, 78, 36, 59),
                Ipv4Addr::new(192, 168, 0, 105),
            ],
            1429,
        );
        assert_eq!(origins.len(), 3, "every address is tried, not just one");
        assert_eq!(
            origins.first().map(String::as_str),
            Some("http://192.168.0.105:1429"),
            "the LAN address is dialled before a container bridge or a tailnet one"
        );
    }

    #[test]
    fn a_direct_https_record_is_not_offered() {
        assert!(
            candidate_from_parts(
                "_vibex._tcp.local",
                4123,
                vec![Ipv4Addr::new(192, 168, 1, 20)],
                &txt("direct"),
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn a_record_without_its_identity_is_refused() {
        let mut record = txt("zero_config");
        record.remove("server_identity_public_key");
        assert!(
            candidate_from_parts(
                "_vibex._tcp.local",
                4123,
                vec![Ipv4Addr::new(192, 168, 1, 20)],
                &record,
                None,
            )
            .is_none()
        );
    }
}
