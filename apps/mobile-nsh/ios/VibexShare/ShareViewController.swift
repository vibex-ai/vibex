//
//  ShareViewController.swift
//  VibexShare
//
//  The share extension: "Share to Vibex" from Safari, Notes, Photos and
//  anywhere else that shares text or a link.
//
//  An extension cannot run the app, so this writes the text into the App
//  Group container both targets declare and opens `vibex://share`. The app
//  reads the file (`ShareIntake.swift`) and puts it in the composer, which is
//  the iOS counterpart of the Android host's `ACTION_SEND` handling.
//
//  The sheet is the system's: the person picks Vibex in the share sheet and
//  the text is sent, with no extra UI of ours to confirm.
//

import UIKit
import UniformTypeIdentifiers

@MainActor
final class ShareViewController: UIViewController {
    private let group = "group.ai.vibex.mobile.nsh"
    private let fileName = "pending-share.json"

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

    /// Writes the shared text and asks the system to open the app.
    private func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            finish()
            return
        }
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: group
        ) else {
            finish()
            return
        }
        let payload: [String: String] = ["text": trimmed]
        if let data = try? JSONSerialization.data(withJSONObject: payload) {
            try? data.write(
                to: container.appendingPathComponent(fileName), options: .atomic
            )
        }
        openApp()
    }

    /// Opens `vibex://share`, which the app answers with the pending text.
    private func openApp() {
        guard let url = URL(string: "vibex://share") else {
            finish()
            return
        }
        // An extension has no `UIApplication.shared`; the responder chain
        // still reaches the system's opener.
        var responder: UIResponder? = self
        let selector = sel_registerName("openURL:")
        while let current = responder {
            if current.responds(to: selector) {
                _ = current.perform(selector, with: url)
                break
            }
            responder = current.next
        }
        finish()
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil)
    }
}
