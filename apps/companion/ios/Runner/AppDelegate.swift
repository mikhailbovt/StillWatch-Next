import Flutter
import UIKit
import AVFoundation
import UserNotifications

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  private var scanner: PairingScanner?
  private var pushChannel: FlutterMethodChannel?
  private var pushToken: String?
  private var pushFailed = false
  private var pushInteractions: [String] = []
  private lazy var notificationDelegate = StillWatchNotificationDelegate(fallback: self) { [weak self] notice in
    guard let self else { return }
    if self.pushInteractions.count == 8 { self.pushInteractions.removeFirst() }
    self.pushInteractions.append(notice)
    self.pushChannel?.invokeMethod("interaction", arguments: nil)
  }

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    UNUserNotificationCenter.current().delegate = notificationDelegate
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    if let push = engineBridge.pluginRegistry.registrar(forPlugin: "StillWatchPush") {
      let channel = FlutterMethodChannel(
        name: "com.stillwatch.companion/apns", binaryMessenger: push.messenger())
      pushChannel = channel
      channel.setMethodCallHandler { [weak self] call, result in
        guard let self else { result(nil); return }
        switch call.method {
        case "register": self.registerPush(prompt: call.arguments as? Bool == true, result: result)
        case "token":
          var state: [String: Any] = ["failed": self.pushFailed]
          if let token = self.pushToken { state["token"] = token }
          result(state)
        case "takeInteractions":
          let pending = self.pushInteractions
          self.pushInteractions.removeAll()
          result(pending)
        default: result(FlutterMethodNotImplemented)
        }
      }
    }
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

  private func registerPush(prompt: Bool, result: @escaping FlutterResult) {
    let center = UNUserNotificationCenter.current()
    center.getNotificationSettings { settings in
      switch settings.authorizationStatus {
      case .authorized, .provisional, .ephemeral:
        DispatchQueue.main.async { self.beginPushRegistration(result) }
      case .notDetermined where prompt:
        center.requestAuthorization(options: [.alert, .sound]) { allowed, _ in
          DispatchQueue.main.async {
            if allowed { self.beginPushRegistration(result) }
            else { result(false) }
          }
        }
      default:
        DispatchQueue.main.async {
          self.pushToken = nil
          result(false)
        }
      }
    }
  }

  private func beginPushRegistration(_ result: FlutterResult) {
    pushFailed = false
    UIApplication.shared.registerForRemoteNotifications()
    result(true)
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    super.application(application, didRegisterForRemoteNotificationsWithDeviceToken: deviceToken)
    if !deviceToken.isEmpty && deviceToken.count <= 256 {
      pushToken = deviceToken.map { String(format: "%02x", $0) }.joined()
      pushFailed = false
      pushChannel?.invokeMethod("registrationChanged", arguments: nil)
    }
  }

  override func application(
    _ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error
  ) {
    super.application(application, didFailToRegisterForRemoteNotificationsWithError: error)
    pushFailed = true
    pushChannel?.invokeMethod("registrationChanged", arguments: nil)
  }

}

private final class StillWatchNotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
  private weak var fallback: UNUserNotificationCenterDelegate?
  private let open: (String) -> Void

  init(fallback: UNUserNotificationCenterDelegate, open: @escaping (String) -> Void) {
    self.fallback = fallback
    self.open = open
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    if pushNotice(notification) != nil {
      completionHandler([])
      return
    }
    if fallback?.userNotificationCenter?(center, willPresent: notification, withCompletionHandler: completionHandler) == nil {
      completionHandler([])
    }
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    if let notice = pushNotice(response.notification) {
      if response.actionIdentifier == UNNotificationDefaultActionIdentifier {
        DispatchQueue.main.async { self.open(notice) }
      }
      completionHandler()
      return
    }
    if fallback?.userNotificationCenter?(center, didReceive: response, withCompletionHandler: completionHandler) == nil {
      completionHandler()
    }
  }

  private func pushNotice(_ notification: UNNotification) -> String? {
    guard notification.request.trigger is UNPushNotificationTrigger,
      let value = notification.request.content.userInfo["stillwatch_notice"] as? String,
      let notice = UUID(uuidString: value) else { return nil }
    return notice.uuidString.lowercased()
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
