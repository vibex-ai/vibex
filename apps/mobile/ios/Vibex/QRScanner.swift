import AVFoundation
import PhotosUI
import UIKit
import VibexFFI
import Vision

/// Full-screen scanner for the one-time Vibex pairing entry.
///
/// Both pairing targets reach the phone as a `vibex://` link — a desktop
/// advertises `#/pair/`, a headless runtime prints `#/code/` — so one scanner
/// covers both and the user never has to say which one they hold.
///
/// The camera is the primary input, but not the only one: the code is often
/// already on the device as a screenshot, or the camera is unavailable, so the
/// same Vision reader also decodes an image picked from the photo library.
/// `PHPickerViewController` runs out of process, so it needs no photo-library
/// permission.
private final class QRScannerViewController: UIViewController,
    AVCaptureMetadataOutputObjectsDelegate,
    PHPickerViewControllerDelegate
{
    private let session = AVCaptureSession()
    private let previewLayer = AVCaptureVideoPreviewLayer()
    private var handled = false
    private let overlay = ViewfinderOverlay()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureControls()
        requestCameraAndStart()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer.frame = view.bounds
        overlay.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if session.isRunning {
            session.stopRunning()
        }
    }

    private func configureControls() {
        view.layer.insertSublayer(previewLayer, at: 0)
        view.addSubview(overlay)

        let close = UIButton(type: .close)
        close.translatesAutoresizingMaskIntoConstraints = false
        close.tintColor = .white
        close.accessibilityLabel = tr("Close scanner", "关闭扫码", "關閉掃描")
        close.addTarget(self, action: #selector(closeTapped), for: .touchUpInside)
        view.addSubview(close)

        let hint = UILabel()
        hint.translatesAutoresizingMaskIntoConstraints = false
        hint.text = tr(
            "Scan the pairing QR code shown by Vibex, or pick a screenshot",
            "扫描 Vibex 显示的配对二维码，或选择一张截图",
            "掃描 Vibex 顯示的配對 QR Code，或選擇一張截圖"
        )
        hint.textColor = UIColor.white.withAlphaComponent(0.92)
        hint.font = .systemFont(ofSize: 15)
        hint.textAlignment = .center
        hint.numberOfLines = 2
        view.addSubview(hint)

        var pickConfiguration = UIButton.Configuration.plain()
        pickConfiguration.title = tr("Choose from photos", "从相册选择", "從相簿選擇")
        pickConfiguration.baseForegroundColor = .white
        pickConfiguration.contentInsets = NSDirectionalEdgeInsets(
            top: 12, leading: 24, bottom: 12, trailing: 24
        )
        let pick = UIButton(configuration: pickConfiguration)
        pick.translatesAutoresizingMaskIntoConstraints = false
        pick.backgroundColor = UIColor.white.withAlphaComponent(0.2)
        pick.layer.cornerRadius = 24
        pick.layer.borderWidth = 1
        pick.layer.borderColor = UIColor.white.withAlphaComponent(0.4).cgColor
        pick.addTarget(self, action: #selector(pickImageTapped), for: .touchUpInside)
        view.addSubview(pick)

        NSLayoutConstraint.activate([
            close.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
            close.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -12),
            close.widthAnchor.constraint(equalToConstant: 44),
            close.heightAnchor.constraint(equalToConstant: 44),
            pick.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            pick.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -28),
            hint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            hint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            hint.bottomAnchor.constraint(equalTo: pick.topAnchor, constant: -16),
        ])
    }

    private func requestCameraAndStart() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    guard let self else { return }
                    if granted {
                        self.configureSession()
                    } else {
                        self.showCameraPermissionError()
                    }
                }
            }
        default:
            showCameraPermissionError()
        }
    }

    private func configureSession() {
        guard let camera = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input) else {
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

        previewLayer.session = session
        previewLayer.videoGravity = .resizeAspectFill
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            self?.session.startRunning()
        }
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !handled,
              let code = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              let value = code.stringValue,
              isPairingEntry(value) else {
            return
        }
        finishScan(value)
    }

    // MARK: - Photo library

    @objc
    private func pickImageTapped() {
        var configuration = PHPickerConfiguration()
        configuration.filter = .images
        configuration.selectionLimit = 1
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = self
        present(picker, animated: true)
    }

    func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
        picker.dismiss(animated: true)
        guard let provider = results.first?.itemProvider,
              provider.canLoadObject(ofClass: UIImage.self) else {
            return
        }
        provider.loadObject(ofClass: UIImage.self) { [weak self] object, _ in
            guard let image = object as? UIImage else { return }
            DispatchQueue.main.async { self?.scan(image: image) }
        }
    }

    /// Decodes a pairing entry out of an image the user picked.
    ///
    /// Staying on the scanner when nothing matches is deliberate: the user
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
        for observation in (request.results ?? []).compactMap({ $0 as? VNBarcodeObservation }) {
            guard let value = observation.payloadStringValue, isPairingEntry(value) else {
                continue
            }
            finishScan(value)
            return
        }
        showNoPairingCodeFound()
    }

    private func showNoPairingCodeFound() {
        let alert = UIAlertController(
            title: tr("No pairing code found", "没有找到配对码", "沒有找到配對碼"),
            message: tr(
                "That image does not contain a Vibex pairing QR code.",
                "这张图片里没有 Vibex 配对二维码。",
                "這張圖片裡沒有 Vibex 配對 QR Code。"
            ),
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(
            title: tr("Try again", "重试", "重試"), style: .default
        ))
        present(alert, animated: true)
    }

    // MARK: - Completion

    private func finishScan(_ value: String) {
        guard !handled else { return }
        handled = true
        session.stopRunning()
        value.withCString { vibex_mobile_pairing_qr_scanned($0) }
        dismiss(animated: true)
    }

    private func showCameraPermissionError() {
        let alert = UIAlertController(
            title: tr("Camera access unavailable", "无法使用相机", "無法使用相機"),
            message: tr(
                "Allow camera access to scan the pairing code, or pick a screenshot from your photos.",
                "请允许访问相机以扫描配对码，或从相册选择一张截图。",
                "請允許存取相機以掃描配對碼，或從相簿選擇一張截圖。"
            ),
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(
            title: tr("Choose from photos", "从相册选择", "從相簿選擇"), style: .default
        ) { [weak self] _ in
            self?.pickImageTapped()
        })
        alert.addAction(UIAlertAction(title: tr("Close", "关闭", "關閉"), style: .cancel))
        present(alert, animated: true)
    }

    @objc
    private func closeTapped() {
        dismiss(animated: true)
    }

    private func isPairingEntry(_ value: String) -> Bool {
        value.hasPrefix("vibex://open/") || value.hasPrefix("vibex://pair#")
    }

    /// Picks the copy for the device language, matching the in-app locales.
    private func tr(_ en: String, _ zhCn: String, _ zhTw: String) -> String {
        let locale = Locale.current
        guard locale.language.languageCode?.identifier == "zh" else { return en }
        if locale.language.script?.identifier == "Hant" {
            return zhTw
        }
        switch locale.region?.identifier {
        case "TW", "HK", "MO": return zhTw
        default: return zhCn
        }
    }
}

private final class ViewfinderOverlay: UIView {
    override func draw(_ rect: CGRect) {
        guard let context = UIGraphicsGetCurrentContext() else { return }
        let side = min(280, rect.width - 48)
        let left = (rect.width - side) / 2
        let top = (rect.height - side) * 0.42
        let frame = CGRect(x: left, y: top, width: side, height: side)

        context.setFillColor(UIColor.black.withAlphaComponent(0.6).cgColor)
        context.fill(CGRect(x: 0, y: 0, width: rect.width, height: frame.minY))
        context.fill(CGRect(x: 0, y: frame.maxY, width: rect.width, height: rect.height - frame.maxY))
        context.fill(CGRect(x: 0, y: frame.minY, width: frame.minX, height: frame.height))
        context.fill(CGRect(x: frame.maxX, y: frame.minY, width: rect.width - frame.maxX, height: frame.height))

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

private func topViewController(_ root: UIViewController?) -> UIViewController? {
    guard let root else { return nil }
    if let presented = root.presentedViewController {
        return topViewController(presented)
    }
    if let navigation = root as? UINavigationController {
        return topViewController(navigation.visibleViewController)
    }
    if let tab = root as? UITabBarController {
        return topViewController(tab.selectedViewController)
    }
    return root
}

@_cdecl("vibex_ios_present_pairing_scanner")
public func vibexIosPresentPairingScanner() {
    DispatchQueue.main.async {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
        guard let root = windows.first(where: { $0.isKeyWindow })?.rootViewController,
              let presenter = topViewController(root) else {
            return
        }
        let scanner = QRScannerViewController()
        scanner.modalPresentationStyle = .fullScreen
        presenter.present(scanner, animated: true)
    }
}
