//
//  PairingScannerViewController.swift
//  Vibex
//
//  The pairing scanner: a full-screen camera with a viewfinder, close, a hint
//  and a photo-library fallback for when the code is already a screenshot.
//
//  It matches the Android shell's `ScannerActivity` — same layout, same copy,
//  same rule that only a `vibex://` pairing entry is accepted, and the same
//  "keep the scanner open" behavior when the person picked the wrong image.
//  iOS decodes the picked image with Vision, so no other framework is needed.
//

import AVFoundation
import PhotosUI
import UIKit
import Vision

@MainActor
final class PairingScannerViewController: UIViewController {
    /// The pairing entry the camera or the picked image held. Called once.
    var onCode: ((String) -> Void)?
    /// The scanner went away without a code (a cancel, a swipe down).
    var onClose: (() -> Void)?

    private let session = AVCaptureSession()
    private let previewLayer = AVCaptureVideoPreviewLayer()
    private let overlay = ViewfinderOverlay()
    private var handled = false
    private var previewConfigured = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureControls()
        requestCamera()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer.frame = view.bounds
        overlay.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        stopSession()
        if !handled {
            onClose?()
        }
    }

    // MARK: - Controls

    private func configureControls() {
        previewLayer.videoGravity = .resizeAspectFill
        view.layer.insertSublayer(previewLayer, at: 0)
        view.addSubview(overlay)

        let close = UIButton(type: .close)
        close.translatesAutoresizingMaskIntoConstraints = false
        close.tintColor = .white
        close.accessibilityLabel = Copy.text("Close scanner", "关闭扫码", "關閉掃描")
        close.addTarget(self, action: #selector(closeTapped), for: .touchUpInside)
        view.addSubview(close)

        let hint = UILabel()
        hint.translatesAutoresizingMaskIntoConstraints = false
        hint.text = Copy.text(
            "Scan the pairing QR code shown by Vibex, or pick a screenshot",
            "扫描 Vibex 显示的配对二维码，或选择一张截图",
            "掃描 Vibex 顯示的配對 QR Code，或選擇一張截圖"
        )
        hint.textColor = UIColor.white.withAlphaComponent(0.92)
        hint.font = .systemFont(ofSize: 15)
        hint.textAlignment = .center
        hint.numberOfLines = 2
        view.addSubview(hint)

        var configuration = UIButton.Configuration.plain()
        configuration.title = Copy.text("Choose from photos", "从相册选择", "從相簿選擇")
        configuration.baseForegroundColor = .white
        configuration.contentInsets = NSDirectionalEdgeInsets(
            top: 12, leading: 24, bottom: 12, trailing: 24
        )
        let pick = UIButton(configuration: configuration)
        pick.translatesAutoresizingMaskIntoConstraints = false
        pick.backgroundColor = UIColor.white.withAlphaComponent(0.2)
        pick.layer.cornerRadius = 24
        pick.layer.borderWidth = 1
        pick.layer.borderColor = UIColor.white.withAlphaComponent(0.4).cgColor
        pick.addTarget(self, action: #selector(pickImageTapped), for: .touchUpInside)
        view.addSubview(pick)

        NSLayoutConstraint.activate([
            close.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
            close.trailingAnchor.constraint(
                equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -12
            ),
            close.widthAnchor.constraint(equalToConstant: 44),
            close.heightAnchor.constraint(equalToConstant: 44),
            pick.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            pick.bottomAnchor.constraint(
                equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -28
            ),
            hint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            hint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            hint.bottomAnchor.constraint(equalTo: pick.topAnchor, constant: -16),
        ])
    }

    // MARK: - Camera

    private func requestCamera() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            startSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                Task { @MainActor in
                    guard let self else { return }
                    if granted {
                        self.startSession()
                    } else {
                        self.showCameraPermissionError()
                    }
                }
            }
        default:
            showCameraPermissionError()
        }
    }

    private func startSession() {
        guard !previewConfigured else {
            startRunning()
            return
        }
        guard let camera = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input)
        else {
            showCameraPermissionError()
            return
        }
        let metadata = AVCaptureMetadataOutput()
        guard session.canAddOutput(metadata) else {
            showCameraPermissionError()
            return
        }
        session.beginConfiguration()
        session.addInput(input)
        session.addOutput(metadata)
        metadata.setMetadataObjectsDelegate(self, queue: .main)
        metadata.metadataObjectTypes = [.qr]
        session.commitConfiguration()
        previewConfigured = true
        previewLayer.session = session
        // The connection exists only once the input is added; the preview then
        // fills the screen in portrait like the Android scanner's does.
        if let connection = previewLayer.connection, connection.isVideoRotationAngleSupported(90) {
            connection.videoRotationAngle = 90
        }
        startRunning()
    }

    private func startRunning() {
        // `startRunning` blocks until the device is configured; it belongs off
        // the main thread. `AVCaptureSession` is documented as safe to start
        // from another thread, hence the unchecked box.
        let box = SessionBox(session: session)
        DispatchQueue.global(qos: .userInitiated).async {
            if !box.session.isRunning {
                box.session.startRunning()
            }
        }
    }

    private func stopSession() {
        if session.isRunning {
            session.stopRunning()
        }
    }

    // MARK: - Completion

    private func finishScan(_ value: String) {
        guard !handled else { return }
        handled = true
        stopSession()
        onCode?(value)
        dismiss(animated: true)
    }

    private static func isPairingEntry(_ value: String) -> Bool {
        value.hasPrefix("vibex://open/")
            || value.hasPrefix("vibex://pair#")
            || (value.hasPrefix("vibex://") && value.contains("#/pair/"))
    }

    private func showCameraPermissionError() {
        let alert = UIAlertController(
            title: Copy.text("Camera access unavailable", "无法使用相机", "無法使用相機"),
            message: Copy.text(
                "Allow camera access to scan the pairing code, or pick a screenshot "
                    + "from your photos.",
                "请允许访问相机以扫描配对码，或从相册选择一张截图。",
                "請允許存取相機以掃描配對碼，或從相簿選擇一張截圖。"
            ),
            preferredStyle: .alert
        )
        alert.addAction(
            UIAlertAction(
                title: Copy.text("Choose from photos", "从相册选择", "從相簿選擇"),
                style: .default
            ) { [weak self] _ in
                self?.pickImageTapped()
            }
        )
        alert.addAction(
            UIAlertAction(title: Copy.text("Close", "关闭", "關閉"), style: .cancel) {
                [weak self] _ in
                self?.dismiss(animated: true)
            }
        )
        present(alert, animated: true)
    }

    @objc private func closeTapped() {
        dismiss(animated: true)
    }

    // MARK: - Photo library

    @objc private func pickImageTapped() {
        var configuration = PHPickerConfiguration()
        configuration.filter = .images
        configuration.selectionLimit = 1
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = self
        present(picker, animated: true)
    }

    /// Decodes a pairing entry out of an image the person picked.
    ///
    /// Staying on the scanner when nothing matches is deliberate: the person
    /// almost always picked the wrong screenshot, and dismissing would make
    /// them reopen the scanner to try the right one.
    private func scan(image: UIImage) {
        guard let cgImage = image.cgImage else {
            showNoPairingCodeFound()
            return
        }
        let request = VNDetectBarcodesRequest()
        request.symbologies = [.qr]
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        do {
            try handler.perform([request])
        } catch {
            showNoPairingCodeFound()
            return
        }
        for observation in request.results ?? [] {
            guard let value = observation.payloadStringValue,
                  Self.isPairingEntry(value)
            else {
                continue
            }
            finishScan(value)
            return
        }
        showNoPairingCodeFound()
    }

    private func showNoPairingCodeFound() {
        let alert = UIAlertController(
            title: Copy.text("No pairing code found", "没有找到配对码", "沒有找到配對碼"),
            message: Copy.text(
                "That image does not contain a Vibex pairing QR code.",
                "这张图片里没有 Vibex 配对二维码。",
                "這張圖片裡沒有 Vibex 配對 QR Code。"
            ),
            preferredStyle: .alert
        )
        alert.addAction(
            UIAlertAction(title: Copy.text("Try again", "重试", "重試"), style: .default)
        )
        present(alert, animated: true)
    }
}

// MARK: - Camera frames

extension PairingScannerViewController: AVCaptureMetadataOutputObjectsDelegate {
    // The output's queue is the main one, but the protocol is not isolated:
    // assert the actor rather than hop, so a frame cannot be decoded after the
    // scanner went away.
    nonisolated func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        MainActor.assumeIsolated {
            guard !handled,
                  let code = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
                  let value = code.stringValue,
                  Self.isPairingEntry(value)
            else {
                return
            }
            finishScan(value)
        }
    }
}

// MARK: - Picked images

extension PairingScannerViewController: PHPickerViewControllerDelegate {
    nonisolated func picker(
        _ picker: PHPickerViewController,
        didFinishPicking results: [PHPickerResult]
    ) {
        // The picker dismisses and then the image loads off the main thread;
        // the decode goes back to the actor once it is there.
        MainActor.assumeIsolated {
            picker.dismiss(animated: true)
        }
        guard let provider = results.first?.itemProvider,
              provider.canLoadObject(ofClass: UIImage.self)
        else {
            return
        }
        provider.loadObject(ofClass: UIImage.self) { [weak self] object, _ in
            guard let image = object as? UIImage else { return }
            Task { @MainActor in
                self?.scan(image: image)
            }
        }
    }
}

/// `AVCaptureSession` handed to a background queue: it is thread-safe for
/// `startRunning`, but not `Sendable`.
private struct SessionBox: @unchecked Sendable {
    let session: AVCaptureSession
}

// MARK: - Viewfinder

/// The scrim and corner brackets around the scan area, as the Android
/// scanner draws them.
private final class ViewfinderOverlay: UIView {
    override func draw(_ rect: CGRect) {
        guard let context = UIGraphicsGetCurrentContext() else { return }
        let side = min(280, rect.width - 48)
        let left = (rect.width - side) / 2
        let top = (rect.height - side) * 0.42
        let frame = CGRect(x: left, y: top, width: side, height: side)

        context.setFillColor(UIColor.black.withAlphaComponent(0.6).cgColor)
        context.fill(CGRect(x: 0, y: 0, width: rect.width, height: frame.minY))
        context.fill(
            CGRect(x: 0, y: frame.maxY, width: rect.width, height: rect.height - frame.maxY)
        )
        context.fill(CGRect(x: 0, y: frame.minY, width: frame.minX, height: frame.height))
        context.fill(
            CGRect(
                x: frame.maxX, y: frame.minY, width: rect.width - frame.maxX, height: frame.height
            )
        )

        context.setStrokeColor(UIColor.white.cgColor)
        context.setLineWidth(4)
        let arm = min(28, side * 0.2)
        for (start, end) in [
            (CGPoint(x: frame.minX, y: frame.minY), CGPoint(x: frame.minX + arm, y: frame.minY)),
            (CGPoint(x: frame.minX, y: frame.minY), CGPoint(x: frame.minX, y: frame.minY + arm)),
            (CGPoint(x: frame.maxX, y: frame.minY), CGPoint(x: frame.maxX - arm, y: frame.minY)),
            (CGPoint(x: frame.maxX, y: frame.minY), CGPoint(x: frame.maxX, y: frame.minY + arm)),
            (CGPoint(x: frame.minX, y: frame.maxY), CGPoint(x: frame.minX + arm, y: frame.maxY)),
            (CGPoint(x: frame.minX, y: frame.maxY), CGPoint(x: frame.minX, y: frame.maxY - arm)),
            (CGPoint(x: frame.maxX, y: frame.maxY), CGPoint(x: frame.maxX - arm, y: frame.maxY)),
            (CGPoint(x: frame.maxX, y: frame.maxY), CGPoint(x: frame.maxX, y: frame.maxY - arm)),
        ] {
            context.move(to: start)
            context.addLine(to: end)
        }
        context.strokePath()
    }
}
