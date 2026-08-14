import AVFoundation
import UIKit
import VibexFFI

private final class QRScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
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
        close.accessibilityLabel = "Close scanner"
        close.addTarget(self, action: #selector(closeTapped), for: .touchUpInside)
        view.addSubview(close)

        let hint = UILabel()
        hint.translatesAutoresizingMaskIntoConstraints = false
        hint.text = "Scan the pairing QR code in Vibex desktop"
        hint.textColor = UIColor.white.withAlphaComponent(0.92)
        hint.font = .systemFont(ofSize: 15)
        hint.textAlignment = .center
        hint.numberOfLines = 2
        view.addSubview(hint)

        NSLayoutConstraint.activate([
            close.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
            close.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -12),
            close.widthAnchor.constraint(equalToConstant: 44),
            close.heightAnchor.constraint(equalToConstant: 44),
            hint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            hint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            hint.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -28),
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
              value.hasPrefix("vibex://open/") else {
            return
        }
        handled = true
        session.stopRunning()
        value.withCString { vibex_mobile_pairing_qr_scanned($0) }
        dismiss(animated: true)
    }

    private func showCameraPermissionError() {
        let alert = UIAlertController(
            title: "Camera access unavailable",
            message: "Allow camera access to scan the Vibex pairing code.",
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "Close", style: .cancel) { [weak self] _ in
            self?.dismiss(animated: true)
        })
        present(alert, animated: true)
    }

    @objc
    private func closeTapped() {
        dismiss(animated: true)
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
