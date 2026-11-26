//
//  ShareIntake.swift
//  Vibex
//
//  What the share extension sends the app.
//
//  A share extension cannot start the containing app itself; it writes what
//  was shared to a shared App Group container (the one both targets declare
//  in `Vibex.entitlements` and `VibexShare.entitlements`) and asks the system
//  to open `vibex://share`. The app reads the file on its next `didBecomeActive`
//  and gives it to the composer the way Android's `ACTION_SEND` text is given.
//

import UIKit

@MainActor
enum ShareIntake {
    /// The App Group both targets declare.
    static let group = "group.ai.vibex.mobile.nsh"
    /// The file the extension writes.
    static let fileName = "pending-share.json"

    /// Takes what a share extension left, if anything.
    ///
    /// Called when the app opens a `vibex://share` URL and when it becomes
    /// active; a share with no file is a no-op, so both can call it. The app
    /// and the extension are different processes, so this is the only channel
    /// — there is no in-memory handoff to rely on.
    static func takePending() {
        guard let url = container()?.appendingPathComponent(fileName),
              let data = try? Data(contentsOf: url),
              let payload = try? JSONDecoder().decode(Payload.self, from: data)
        else {
            return
        }
        // Removed before it is handed over: a failure to read it a second time
        // must not paste the same text twice.
        try? FileManager.default.removeItem(at: url)
        let text = payload.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        text.withCString { vibex_mobile_nsh_share_text($0) }
    }

    /// The shared container, when the entitlement is present in this build.
    private static func container() -> URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    /// What the extension writes.
    struct Payload: Codable {
        /// The shared text (a URL is shared as its text).
        var text: String
    }
}
