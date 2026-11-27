//
//  LanDiscovery.swift
//  Vibex
//
//  Local-network pairing discovery with Bonjour.
//
//  The desktop advertises `_vibex._tcp.local.` with the zero-config TXT record
//  (its mode, server id and identity key); this browses it, resolves each
//  service to the addresses the phone can dial, and emits the same JSON the
//  Android shell's `LanDiscovery.java` does. The Rust app parses that contract
//  (`apps/mobile/src/host.rs`), so Find lists the runtimes with the
//  native browser instead of the in-process mDNS daemon.
//
//  Resolution uses `NSNetService` rather than the Network framework: resolving
//  through `NWConnection` means opening a TCP connection to the runtime just
//  to learn its address, while `NSNetService` reports the addresses and the
//  port the record carries without dialling. `NSNetService` is deprecated in
//  favour of the Network framework, which has no address-level resolution
//  that does not connect, so the deprecation is accepted here: it is a build
//  warning, not a broken build.
//
//  iOS asks for local-network access the first time a browse starts, because
//  `NSBonjourServices` names the type in `Info.plist`. A refusal arrives as a
//  missing-configuration error; it is reported as `permission_denied` so the
//  page offers the settings shortcut instead of an empty list.
//

import Foundation

/// The browse, isolated so the deprecated API stays in one place.
@MainActor
private final class Browser: NSObject {
    var onEvent: ((String) -> Void)?
    private var browser: NSNetServiceBrowser?
    private var services: [String: NSNetService] = [:]
    private var known: Set<String> = []

    func start() {
        stop()
        let browser = NSNetServiceBrowser()
        browser.delegate = self
        browser.searchForServices(ofType: "_vibex._tcp.", inDomain: "local.")
        self.browser = browser
    }

    func stop() {
        for service in services.values {
            service.delegate = nil
            service.stop()
        }
        services.removeAll()
        known.removeAll()
        browser?.delegate = nil
        browser?.stop()
        browser = nil
    }

    private func emit(_ event: [String: Any]) {
        guard JSONSerialization.isValidJSONObject(event),
              let data = try? JSONSerialization.data(withJSONObject: event),
              let json = String(data: data, encoding: .utf8)
        else {
            // The contract's `failed` carries a message, which the pairing
            // page shows; an event that cannot be encoded is still reported.
            onEvent?("{\"kind\":\"failed\",\"message\":\"a discovery event could not be encoded\"}")
            return
        }
        onEvent?(json)
    }

    /// The TXT record of `service` as text, only the values that are UTF-8.
    private func txt(of service: NSNetService) -> [String: String] {
        guard let data = service.txtRecordData() else { return [:] }
        var values: [String: String] = [:]
        for (key, raw) in NSNetService.dictionary(fromTXTRecord: data) {
            if let value = String(data: raw, encoding: .utf8) {
                values[key] = value
            }
        }
        return values
    }

    /// The IPv4 addresses in `service.addresses`, numeric and most reachable
    /// first.
    private func hosts(of service: NSNetService) -> [String] {
        var hosts: [String] = []
        for data in service.addresses ?? [] {
            let host = data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) -> String in
                guard let base = buffer.baseAddress else { return "" }
                let length = socklen_t(buffer.count)
                var text = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                let result = getnameinfo(
                    base.assumingMemoryBound(to: sockaddr.self),
                    length,
                    &text,
                    socklen_t(text.count),
                    nil,
                    0,
                    NI_NUMERICHOST
                )
                return result == 0 ? String(cString: text) : ""
            }
            guard !host.isEmpty, !hosts.contains(host) else { continue }
            hosts.append(host)
        }
        return Self.rank(hosts)
    }

    /// Orders addresses by how likely the phone can route to them, as the
    /// Android shell's `rank` does; the app ranks what it is given again.
    private static func rank(_ hosts: [String]) -> [String] {
        hosts.sorted { score($0) < score($1) }
    }

    private static func score(_ address: String) -> Int {
        if address.hasPrefix("192.168.") { return 1 }
        if address.hasPrefix("10.") { return 2 }
        if address.hasPrefix("172.") { return 3 }
        if address.hasPrefix("169.254.") { return 5 }
        return 4
    }

    /// Reports one resolved service as the bridge's `candidate` message.
    private func report(_ service: NSNetService) {
        let key = service.name
        let hosts = hosts(of: service)
        guard !hosts.isEmpty, service.port != 0 else {
            // No address is no candidate: the app would list a row that cannot
            // be dialled. The service is reported again if it changes.
            return
        }
        let values = txt(of: service)
        var event: [String: Any] = [
            "kind": "candidate",
            "key": key,
            "serviceInstance": key,
            "hosts": hosts,
            "host": hosts[0],
            "port": service.port,
            "interfaceScope": service.domain ?? "",
            "txt": values,
        ]
        if let name = values["display_name"], !name.isEmpty {
            event["name"] = name
        }
        emit(event)
        known.insert(key)
    }
}

// The delegates are Objective-C protocols, so their methods are not isolated;
// each one asserts the main actor, which is the run loop the browse started on
// and therefore the one the callbacks arrive on.
extension Browser: NSNetServiceBrowserDelegate {
    nonisolated func netServiceBrowser(
        _ browser: NSNetServiceBrowser,
        didFind service: NSNetService,
        moreComing: Bool
    ) {
        MainActor.assumeIsolated {
            services[service.name] = service
            service.delegate = self
            service.resolve(withTimeout: 5)
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NSNetServiceBrowser,
        didRemove service: NSNetService,
        moreComing: Bool
    ) {
        MainActor.assumeIsolated {
            services.removeValue(forKey: service.name)
            if known.remove(service.name) != nil {
                emit([
                    "kind": "removed",
                    "key": service.name,
                    "serviceInstance": service.name,
                ])
            }
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NSNetServiceBrowser,
        didNotSearch errorDict: [String: NSNumber]
    ) {
        MainActor.assumeIsolated {
            // The dictionary is keyed by the constant the callback documents.
            let code = errorDict[NSNetServicesErrorCode]?.intValue
            // `NSNetServicesMissingRequiredConfigurationError` is what a
            // refused local-network grant answers a browse with.
            if code == NSNetServicesError.missingRequiredConfiguration.rawValue {
                emit(["kind": "permission_denied"])
            } else {
                emit([
                    "kind": "failed",
                    "message": "the local-network browser could not start",
                ])
            }
            stop()
        }
    }
}

extension Browser: NSNetServiceDelegate {
    nonisolated func netServiceDidResolveAddress(_ sender: NSNetService) {
        MainActor.assumeIsolated {
            report(sender)
        }
    }

    nonisolated func netService(
        _ sender: NSNetService,
        didNotResolve errorDict: [String: NSNumber]
    ) {
        MainActor.assumeIsolated {
            services.removeValue(forKey: sender.name)
        }
    }
}

/// The app's discovery seam: one browse at a time, events in the bridge's
/// JSON contract.
@MainActor
final class LanDiscovery {
    /// One discovery event, as the bridge's JSON contract.
    var onEvent: ((String) -> Void)?

    private let browser = Browser()

    func start() {
        browser.onEvent = onEvent
        browser.start()
    }

    func stop() {
        browser.stop()
    }
}
