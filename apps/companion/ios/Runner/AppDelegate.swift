import Flutter
import UIKit
import AVFoundation
import UserNotifications

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  private var scanner: PairingScanner?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    UNUserNotificationCenter.current().delegate = self
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "StillWatchPairing") else {
      return
    }
    let channel = FlutterMethodChannel(
      name: "com.stillwatch.companion/pairing", binaryMessenger: registrar.messenger())
    channel.setMethodCallHandler { [weak self] call, result in
      guard call.method == "scanQr" else {
        result(FlutterMethodNotImplemented)
        return
      }
      guard let self, self.scanner == nil else {
        result(FlutterError(code: "busy", message: "Pairing scanner is already open", details: nil))
        return
      }
      let scanner = PairingScanner { [weak self] value in
        self?.scanner = nil
        result(value)
      }
      self.scanner = scanner
      guard let scene = UIApplication.shared.connectedScenes
        .compactMap({ $0 as? UIWindowScene })
        .first(where: { $0.activationState == .foregroundActive }),
        var presenter = scene.keyWindow?.rootViewController else {
        self.scanner = nil
        result(FlutterError(code: "unavailable", message: "No active window", details: nil))
        return
      }
      while let presented = presenter.presentedViewController {
        presenter = presented
      }
      scanner.modalPresentationStyle = .fullScreen
      presenter.present(scanner, animated: true)
    }
  }
}

private final class PairingScanner: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
  private let session = AVCaptureSession()
  private let captureQueue = DispatchQueue(label: "com.stillwatch.pairing.camera")
  private var finish: ((Any?) -> Void)?
  private var preview: AVCaptureVideoPreviewLayer?

  init(finish: @escaping (Any?) -> Void) {
    self.finish = finish
    super.init(nibName: nil, bundle: nil)
  }

  required init?(coder: NSCoder) { nil }

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .black
    let layer = AVCaptureVideoPreviewLayer(session: session)
    layer.videoGravity = .resizeAspectFill
    view.layer.addSublayer(layer)
    preview = layer

    let cancel = UIButton(type: .system)
    cancel.configuration = .filled()
    cancel.setTitle("Cancel", for: .normal)
    cancel.addTarget(self, action: #selector(cancelScan), for: .touchUpInside)
    let hint = UILabel()
    hint.text = "Point the camera at a StillWatch pairing code"
    hint.textColor = .white
    hint.numberOfLines = 0
    let controls = UIStackView(arrangedSubviews: [hint, cancel])
    controls.axis = .horizontal
    controls.spacing = 16
    controls.alignment = .center
    controls.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(controls)
    NSLayoutConstraint.activate([
      controls.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 16),
      controls.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -16),
      controls.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12)
    ])

    AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
      DispatchQueue.main.async {
        guard let self, self.finish != nil else { return }
        if granted {
          self.captureQueue.async { [weak self] in self?.startCapture() }
        } else {
          self.complete(FlutterError(code: "camera_permission", message: "Camera permission is unavailable", details: nil))
        }
      }
    }
  }

  override func viewDidLayoutSubviews() {
    super.viewDidLayoutSubviews()
    preview?.frame = view.bounds
  }

  private func startCapture() {
    do {
      guard let camera = AVCaptureDevice.default(for: .video) else { throw ScanError.unavailable }
      let input = try AVCaptureDeviceInput(device: camera)
      let output = AVCaptureMetadataOutput()
      guard session.canAddInput(input), session.canAddOutput(output) else { throw ScanError.unavailable }
      session.beginConfiguration()
      session.addInput(input)
      session.addOutput(output)
      output.setMetadataObjectsDelegate(self, queue: .main)
      guard output.availableMetadataObjectTypes.contains(.qr) else {
        session.commitConfiguration()
        throw ScanError.unavailable
      }
      output.metadataObjectTypes = [.qr]
      session.commitConfiguration()
      session.startRunning()
    } catch {
      DispatchQueue.main.async { [weak self] in
        self?.complete(FlutterError(code: "unavailable", message: "The QR scanner is unavailable", details: nil))
      }
    }
  }

  func metadataOutput(
    _ output: AVCaptureMetadataOutput,
    didOutput metadataObjects: [AVMetadataObject],
    from connection: AVCaptureConnection
  ) {
    guard let value = metadataObjects.compactMap({ ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue })
      .first(where: { $0.utf8.count <= 8192 && $0.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("{") }) else { return }
    complete(value)
  }

  @objc private func cancelScan() { complete(nil) }

  private func complete(_ value: Any?) {
    guard let callback = finish else { return }
    finish = nil
    captureQueue.async { [session] in
      if session.isRunning { session.stopRunning() }
    }
    dismiss(animated: true) { callback(value) }
  }

  private enum ScanError: Error { case unavailable }
}
