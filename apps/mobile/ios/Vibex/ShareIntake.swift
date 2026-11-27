//
//  ShareIntake.swift
//  Vibex
//
//  What the share extension sends the app.
//
//  A share extension cannot start the containing app itself. It can, however,
//  ask the system to open one, and a `vibex://` URL carries a query string, so
//  the text travels in the URL: `vibex://share?text=…`. That is deliberate.
//  The alternative — a shared App Group container — needs a managed
//  entitlement, and a free Apple ID cannot sign one, which is exactly how this
//  app is installed (`docs/operations/ios-unsigned-install.md`). A URL keeps
//  the share working on the same install path the GPUI client used.
//
//  iOS caps how long an opened URL may be, and a share can be long, so a
//  payload past `maxTextLength` goes to the pasteboard instead and the URL
//  says so (`vibex://share?pasteboard=1`).
//

import UIKit

@MainActor
enum ShareIntake {
    /// The scheme host both sides use.
    static let urlHost = "share"
    /// The longest text the URL carries; past it the pasteboard is used.
    static let maxTextLength = 6000
    /// The query key holding the shared text.
    static let textKey = "text"
    /// The query key that says "read the pasteboard instead".
    static let pasteboardKey = "pasteboard"

    /// Takes what a share URL carried, if anything.
    ///
    /// Called when the app opens `vibex://share`. A URL with neither the text
    /// nor the pasteboard marker is a no-op, so the app can answer the scheme
    /// unconditionally.
    static func take(from url: URL) {
        guard url.scheme == "vibex", url.host == urlHost else { return }
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let value = { (key: String) in items.first { $0.name == key }?.value }
        var text = value(textKey) ?? ""
        if text.isEmpty, value(pasteboardKey) != nil {
            // The extension's long-text path: the words are on the pasteboard,
            // and iOS shows its own "pasted from" notice when they are read.
            text = UIPasteboard.general.string ?? ""
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        trimmed.withCString { vibex_mobile_share_text($0) }
    }
}
