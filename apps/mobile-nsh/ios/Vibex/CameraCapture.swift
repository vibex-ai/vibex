//
//  CameraCapture.swift
//  Vibex
//
//  A photo for the composer.
//
//  Unlike the pairing scanner, this is the system camera: the photo has no
//  destination until it exists, and the app only needs the file. The Android
//  host starts `ACTION_IMAGE_CAPTURE` with a FileProvider URI for the same
//  reason; here `UIImagePickerController` with `sourceType = .camera` writes
//  a JPEG under the app's caches and the app reads the bytes.
//
//  The camera permission is asked here, before the picker: a picker opened
//  without it shows a black viewfinder with no explanation, while the app's
//  composer has somewhere to say why there is no photo.
//

import AVFoundation
import UIKit

@MainActor
final class CameraCapture: NSObject {
    /// The photo's path, already written.
    var onPhoto: ((String) -> Void)?
    /// Why there is no photo. A cancel says nothing; a failure does.
    var onFailure: ((String) -> Void)?

    /// The view controller showing the picker.
    private weak var presenter: UIViewController?
    private var picker: UIImagePickerController?
    private var completion: (() -> Void)?
    /// Whether the picker is up, so a second request cannot stack another.
    private var presented = false

    /// Opens the camera over `presenter`. `completion` runs once the picker is
    /// gone, whether a photo was taken or not.
    func present(from presenter: UIViewController, completion: @escaping () -> Void) {
        self.presenter = presenter
        self.completion = completion
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            show()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                Task { @MainActor in
                    guard let self else { return }
                    if granted {
                        self.show()
                    } else {
                        self.finish(reason: Self.refused)
                    }
                }
            }
        default:
            finish(reason: Self.refused)
        }
    }

    private static var refused: String {
        Copy.text("camera access was refused", "没有相机权限", "沒有相機權限")
    }

    private func show() {
        guard !presented else { return }
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            finish(reason: Copy.text(
                "this device has no camera",
                "此设备没有相机",
                "此裝置沒有相機"
            ))
            return
        }
        guard let presenter, presenter.presentedViewController == nil else {
            // Something else is in front; opening behind it would show a
            // camera nobody sees. The app is told instead of nothing happening.
            finish(reason: Copy.text(
                "another screen is already open",
                "已有其他界面打开",
                "已有其他介面開啟"
            ))
            return
        }
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.cameraCaptureMode = .photo
        picker.delegate = self
        picker.modalPresentationStyle = .fullScreen
        self.picker = picker
        presented = true
        presenter.present(picker, animated: true)
    }

    /// Writes `image` as a JPEG in the app's caches and reports its path.
    private func keep(image: UIImage) {
        // The app reads the file as JPEG and names the attachment after it;
        // the caches directory is where iOS lets a photo of this size live.
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("camera", isDirectory: true)
        do {
            try FileManager.default.createDirectory(
                at: directory, withIntermediateDirectories: true
            )
            let name = "shot-\(Int(Date().timeIntervalSince1970 * 1000)).jpg"
            let url = directory.appendingPathComponent(name)
            guard let data = image.jpegData(compressionQuality: 0.9) else {
                finish(reason: Copy.text(
                    "the camera returned no photo",
                    "相机没有返回照片",
                    "相機沒有傳回照片"
                ))
                return
            }
            try data.write(to: url, options: .atomic)
            onPhoto?(url.path)
            finish(reason: nil)
        } catch {
            finish(reason: error.localizedDescription)
        }
    }

    private func finish(reason: String?) {
        // The picker is the presented controller, so the *presenter* is the
        // one that dismisses it; calling it on the picker itself is a no-op.
        if presented {
            presented = false
            let presentation = picker?.presentingViewController ?? presenter
            presentation?.dismiss(animated: true)
        }
        picker = nil
        if let reason {
            onFailure?(reason)
        }
        completion?()
        completion = nil
    }
}

extension CameraCapture: UIImagePickerControllerDelegate, UINavigationControllerDelegate {
    // The picker calls these on the main thread; the protocol itself is not
    // isolated, so assert the actor instead of hopping.
    nonisolated func imagePickerController(
        _ picker: UIImagePickerController,
        didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
    ) {
        MainActor.assumeIsolated {
            guard let image = info[.originalImage] as? UIImage else {
                finish(reason: Copy.text(
                    "the camera returned no photo",
                    "相机没有返回照片",
                    "相機沒有傳回照片"
                ))
                return
            }
            keep(image: image)
        }
    }

    nonisolated func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
        // A cancel is the person changing their mind and says nothing, as the
        // Android shell's `RESULT_CANCELED` does.
        MainActor.assumeIsolated {
            finish(reason: nil)
        }
    }
}
