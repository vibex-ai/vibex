//
//  ShareViewController.swift
//  VibexShare
//
//  The share extension: "Share to Vibex" from Safari, Notes, Photos and
//  anywhere else that shares text or a link.
//
//  An extension cannot run the app, so this hands the text over in the URL the
//  system opens: `vibex://share?text=…`, with a pasteboard fallback for text
//  longer than a URL should carry. `ShareIntake.swift` in the app reads it and
//  puts it in the composer, which is the iOS counterpart of the Android host's
//  `ACTION_SEND` handling.
//
//  A shared App Group container would be the tidier channel, but it needs the
//  App Groups capability, and a free Apple ID — the account this app's install
//  path is built around — cannot sign one. The GPUI client had no entitlements
//  for the same reason, and keeping it that way is what lets an upgrade
//  re-sign in place.
//
//  The sheet is the system's: the person picks Vibex in the share sheet and
//  the text is sent, with no extra UI of ours to confirm.
//

import UIKit
import UniformTypeIdentifiers

@MainActor
final class ShareViewController: UIViewController {
    /// Kept in step with `ShareIntake.maxTextLength` in the app.
    private let maxTextLength = 6000

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .clear
        receive()
    }

    /// Reads the shared item and forwards it.
    private func receive() {
        guard let item = extensionContext?.inputItems.first as? NSExtensionItem else {
            finish()
            return
        }
        // The text the share sheet attached, then any URL, then one of the
        // item's attachments opened as plain text: the three shapes Safari,
        // Files and Notes send.
        if let text = item.attributedContentText?.string, !text.isEmpty {
            send(text)
            return
        }
        if let providers = item.attachments {
            if let provider = providers.first(where: {
                $0.hasItemConformingToTypeIdentifier(UTType.url.identifier)
            }) {
                load(provider, type: UTType.url.identifier) { value in
                    if let url = value as? URL {
                        return url.absoluteString
                    }
                    return value as? String
                }
                return
            }
            if let provider = providers.first(where: {
                $0.hasItemConformingToTypeIdentifier(UTType.plainText.identifier)
            }) {
                load(provider, type: UTType.plainText.identifier) { value in
                    if let text = value as? String {
                        return text
                    }
                    if let data = value as? Data {
                        return String(data: data, encoding: .utf8)
                    }
                    return nil
                }
                return
            }
        }
        finish()
    }

    /// Loads one attachment and turns it into the text to share.
    ///
    /// The completion runs off the main thread; the parsed string comes back
    /// here, where the extension's own state lives.
    private func load(
        _ provider: NSItemProvider,
        type: String,
        parse: @escaping @Sendable (Any?) -> String?
    ) {
        provider.loadItem(forTypeIdentifier: type, options: nil) { value, _ in
            let text = parse(value)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    if let text, !text.isEmpty {
                        self.send(text)
                    } else {
                        self.finish()
                    }
                }
            }
        }
    }

    /// Hands the shared text over in the URL that opens the app.
    private func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            finish()
            return
        }
        var components = URLComponents()
        components.scheme = "vibex"
        components.host = "share"
        if trimmed.count <= maxTextLength {
            components.queryItems = [URLQueryItem(name: "text", value: trimmed)]
        } else {
            // Too long for a URL: the words go on the pasteboard and the URL
            // says where to look. `ShareIntake` reads it back.
            UIPasteboard.general.string = trimmed
            components.queryItems = [URLQueryItem(name: "pasteboard", value: "1")]
        }
        guard let url = components.url else {
            finish()
            return
        }
        open(url)
    }

    /// Asks the system to open `url`, which brings the app forward.
    ///
    /// `NSExtensionContext.open` is the documented opener, but a share
    /// extension is not one of the types it serves everywhere and it can
    /// report failure. The responder chain's `openURL:` reaches the same
    /// opener from an extension that has no `UIApplication.shared`, so it is
    /// the fallback rather than a dead end.
    private func open(_ url: URL) {
        guard let context = extensionContext else {
            openThroughResponderChain(url)
            finish()
            return
        }
        context.open(url) { opened in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    if !opened {
                        self.openThroughResponderChain(url)
                    }
                    self.finish()
                }
            }
        }
    }

    private func openThroughResponderChain(_ url: URL) {
        var responder: UIResponder? = self
        let selector = sel_registerName("openURL:")
        while let current = responder {
            if current.responds(to: selector) {
                _ = current.perform(selector, with: url)
                return
            }
            responder = current.next
        }
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil)
    }
}
