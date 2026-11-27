//
//  AppDelegate.swift
//  Vibex
//
//  The shell's UIApplicationDelegate.
//
//  It owns the window, mounts the Rust app, and forwards what iOS hands the
//  application rather than the view: a URL the app was opened with (the
//  `vibex://` pairing link), a Home Screen quick action, text from the share
//  extension, and the foreground/background transitions the background task
//  follows.
//
//  The app inside is one root for the whole process: the pairing page and the
//  session shell swap in place, so nothing here re-creates it on a
//  backgrounding or a rotation.
//

import UIKit

@main
@MainActor
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private var app: UnsafeMutableRawPointer?
    private var bridge: HostBridge?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [
            UIApplication.LaunchOptionsKey: Any
        ]?
    ) -> Bool {
        // Before the app runs, so a notification press that started the
        // process waits for it instead of being dropped.
        vibex_mobile_nsh_install_notifications()

        // The container the app mounts into: a plain, full-bleed view. The app
        // draws its own safe areas and status bar, so this controller adds
        // nothing on top of it.
        let root = UIViewController()
        root.view.backgroundColor = .systemBackground
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = root
        window.makeKeyAndVisible()
        self.window = window

        let bridge = HostBridge(presenter: root)
        bridge.start()
        self.bridge = bridge
        app = vibex_mobile_nsh_attach(
            Unmanaged.passUnretained(root.view).toOpaque()
        )

        if let url = launchOptions?[.url] as? URL {
            open(url)
        }
        applyShortcut(launchOptions?[.shortcutItem] as? UIApplicationShortcutItem)
        // Registered here rather than in `Info.plist`, so the action and its
        // handler's type cannot drift apart. The system keeps them across
        // launches; setting them again each launch is how a changed title or
        // icon reaches an installed app.
        application.shortcutItems = [
            UIApplicationShortcutItem(
                type: Shortcut.newSession,
                localizedTitle: Copy.text("New session", "新建会话", "新增工作階段"),
                localizedSubtitle: nil,
                icon: UIApplicationShortcutIcon(systemImageName: "square.and.pencil"),
                userInfo: nil
            )
        ]
        return true
    }

    // MARK: - URLs

    func application(
        _ app: UIApplication,
        open url: URL,
        options: [UIApplication.OpenURLOptionsKey: Any] = [:]
    ) -> Bool {
        open(url)
        return true
    }

    /// Routes a `vibex://` link through the same channel a scanned code uses.
    ///
    /// The pairing page claims whatever entry it is given, so a link the
    /// system opened and a QR the camera read take one path: a pairing entry
    /// is a pairing entry. A link of another scheme is ignored rather than
    /// fed to the claimer.
    private func open(_ url: URL) {
        guard url.scheme == "vibex" else { return }
        if url.host == "share" {
            // The share extension's handoff: the text rides in this URL (or,
            // when it was too long, on the pasteboard it names).
            ShareIntake.take(from: url)
            return
        }
        url.absoluteString.withCString { vibex_mobile_nsh_scan_result($0) }
    }

    // MARK: - Quick actions

    /// The "New session" Home Screen action, when the app is already running.
    func application(
        _ application: UIApplication,
        performActionFor shortcutItem: UIApplicationShortcutItem,
        completionHandler: @escaping (Bool) -> Void
    ) {
        completionHandler(applyShortcut(shortcutItem))
    }

    @discardableResult
    private func applyShortcut(_ item: UIApplicationShortcutItem?) -> Bool {
        guard let item, item.type == Shortcut.newSession else { return false }
        vibex_mobile_nsh_new_session()
        return true
    }

    // MARK: - Lifecycle

    func applicationDidEnterBackground(_ application: UIApplication) {
        bridge?.didEnterBackground()
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        bridge?.willEnterForeground()
    }

    func applicationWillTerminate(_ application: UIApplication) {
        bridge?.stop()
        vibex_mobile_nsh_detach(app)
        app = nil
    }
}

/// The quick action types the app registers, and answers.
enum Shortcut {
    /// "New session": open the new-session composer.
    static let newSession = "ai.vibex.mobile.new-session"
}
