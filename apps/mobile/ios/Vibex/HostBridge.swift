//
//  HostBridge.swift
//  Vibex
//
//  The shell's half of the Rust app's host seam.
//
//  The Rust app asks for the camera, Bonjour and the settings page by
//  queueing a request (`vibex_mobile_take_host_request`); this polls the
//  queue every 200 ms and opens the right surface. What those surfaces observe
//  comes back through the C functions in `VibexMobile.h`, which write into
//  the app's process-wide host channel — the same contract the Android host
//  implements in `HostBridge.java` and `MainActivity.java`.
//
//  One request is taken per poll: the queue is a button-press queue, not a
//  stream, and answering a scan or a camera asks the person for something
//  else before the next request makes sense.
//

import UIKit

/// The values `vibex_mobile_take_host_request` answers with.
enum HostRequest: Int32 {
    case none = 0
    case scan = 1
    case discoveryStart = 2
    case discoveryStop = 3
    case camera = 4
    case appSettings = 5
}

@MainActor
final class HostBridge {
    /// The view controller the scanner and the camera are presented over.
    private weak var presenter: UIViewController?
    private let discovery = LanDiscovery()
    private let camera = CameraCapture()

    /// Whether a scanner is already up: a second press of Scan while it is
    /// open must not stack a second full-screen camera.
    private var scanning = false
    private var takingPhoto = false
    private var pollTask: Task<Void, Never>?

    /// The background task that keeps the process runnable while the app is
    /// away, and whether the app asked for one.
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    private var wantsBackground = false

    init(presenter: UIViewController) {
        self.presenter = presenter
        discovery.onEvent = { json in
            json.withCString { vibex_mobile_discovery_event($0) }
        }
        camera.onPhoto = { path in
            path.withCString { vibex_mobile_camera_photo($0) }
        }
        camera.onFailure = { reason in
            reason.withCString { vibex_mobile_camera_failed($0) }
        }
    }

    /// Tells the app what this shell can do, then starts answering it.
    ///
    /// Called before the app is mounted: `scanner_available` decides whether
    /// Scan opens this shell's camera or falls back to the photo picker, and
    /// `discovery_available` whether Find asks Bonjour or browses mDNS in
    /// Rust.
    func start() {
        vibex_mobile_set_scanner_available(1)
        vibex_mobile_set_camera_available(1)
        vibex_mobile_set_discovery_available(1)
        if let name = UIDevice.current.name.nilIfEmpty {
            name.withCString { vibex_mobile_set_device_name($0) }
        }
        // One task, not one per poll: the queue is answered on the main actor
        // for as long as the shell runs.
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(200))
                guard let self, !Task.isCancelled else { return }
                self.poll()
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
        discovery.stop()
        endBackgroundTask()
    }

    /// Answers one queued request and applies one pending state change.
    private func poll() {
        // An unknown code is the app asking for something this shell does not
        // know; it is left alone rather than guessed at.
        switch HostRequest(rawValue: vibex_mobile_take_host_request()) ?? .none {
        case .scan:
            openScanner()
        case .discoveryStart:
            discovery.start()
        case .discoveryStop:
            discovery.stop()
        case .camera:
            openCamera()
        case .appSettings:
            openAppSettings()
        case .none:
            break
        }

        switch vibex_mobile_take_keep_screen_on() {
        case 1:
            UIApplication.shared.isIdleTimerDisabled = true
        case 0:
            UIApplication.shared.isIdleTimerDisabled = false
        default:
            break
        }

        switch vibex_mobile_take_connection_service() {
        case 1:
            wantsBackground = true
            beginBackgroundTaskIfNeeded()
        case 0:
            wantsBackground = false
            endBackgroundTask()
        default:
            break
        }
    }

    // MARK: - Scanning

    /// Opens the pairing scanner.
    ///
    /// The scanner has its own photo-library fallback, so a refused camera
    /// still leaves a way in instead of a dead button.
    private func openScanner() {
        guard !scanning, let presenter else { return }
        scanning = true
        let scanner = PairingScannerViewController()
        scanner.onCode = { [weak self] value in
            value.withCString { vibex_mobile_scan_result($0) }
            self?.scanning = false
        }
        scanner.onClose = { [weak self] in
            self?.scanning = false
        }
        scanner.modalPresentationStyle = .fullScreen
        if presenter.presentedViewController != nil {
            // Something the person opened is in front (an alert, a sheet): a
            // scanner behind it would never be seen. The app asks again.
            scanning = false
            return
        }
        presenter.present(scanner, animated: true)
    }

    // MARK: - Camera

    /// Opens the camera for a composer attachment.
    ///
    /// The photo is written as a JPEG under the app's caches and the app reads
    /// the bytes; a failure is reported with its reason because the camera UI
    /// is `UIImagePickerController`'s, which has nowhere to say it.
    private func openCamera() {
        guard !takingPhoto, let presenter else { return }
        if presenter.presentedViewController != nil {
            return
        }
        takingPhoto = true
        camera.present(from: presenter) { [weak self] in
            self?.takingPhoto = false
        }
    }

    // MARK: - Settings

    private func openAppSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }

    // MARK: - Background

    /// Asks the system for time to finish what the app is doing, as iOS has no
    /// foreground service: the runtime's connection and an in-flight turn keep
    /// running until the system takes the time back (about 30 seconds).
    func beginBackgroundTaskIfNeeded() {
        guard wantsBackground, backgroundTask == .invalid else { return }
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "vibex.connection") {
            [weak self] in
            // Expired: nothing may run now. Drop the assertion; the next
            // backgrounding begins a new one.
            self?.endBackgroundTask()
        }
    }

    func endBackgroundTask() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    /// The app went to the background with a connection: hold the task now.
    func didEnterBackground() {
        beginBackgroundTaskIfNeeded()
    }

    /// The app is back: a task begun while it was away ends when it becomes
    /// active, and the queue begins a new one on the next backgrounding while
    /// the runtime is still connected.
    func willEnterForeground() {
        endBackgroundTask()
    }
}

extension String {
    /// This string, or `nil` when it is empty.
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
