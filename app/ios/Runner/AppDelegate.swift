import Flutter
import UIKit

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    if let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "NightDropChannels") {
      NightDropChannels.register(with: registrar)
    }
  }
}

/// The iOS side of the two channels MainActivity.kt answers on Android, so the Dart code is the
/// same on both (iOS is not shipped yet; written to the same contract, not yet built or run).
///
/// `app.nightdrop/screenshots` — capture transparency (#1):
///   * a screenshot is reported (`userDidTakeScreenshotNotification`) — *that* it happened, never
///     the image, as on Android 14+;
///   * a screen recording, AirPlay or mirroring is reported (`UIScreen.isCaptured`), the
///     counterpart of Android 15's recording callback: Dart reports a start once per recording;
///   * "Block screenshots" (on by default) cannot stop a screenshot on iOS — there is no
///     FLAG_SECURE — so here it covers the app while the screen is recorded, mirrored or AirPlayed
///     (`CaptureBlock.recordingOnly` in screenshot_detector.dart). Screenshots are still reported.
///     A chat whose contact blocks captures holds it the same way (`holdBlock`);
///   * the app-switcher snapshot is covered while the app is inactive, the counterpart of
///     Android's `setRecentsScreenshotEnabled(false)`.
/// `app.nightdrop/process` — copying and sharing an invite link (self-audit 2026-10-17):
///   * `copySensitive`: the pasteboard item is local-only (never Universal Clipboard to the
///     user's other Apple devices) and expires after a minute;
///   * `shareText`: the system share sheet, without its "Copy" action, so the link goes straight
///     to the chosen app.
final class NightDropChannels: NSObject {
  private let screenshots: FlutterMethodChannel
  private let process: FlutterMethodChannel
  private var cover: UIView?
  // Shown over the app while it is being recorded and captures are blocked.
  private var captureCover: UIView?
  // A chat whose contact has "Block screenshots" on is open (Dart's `holdBlock`).
  private var heldByChat = false

  private init(messenger: FlutterBinaryMessenger) {
    screenshots = FlutterMethodChannel(name: "app.nightdrop/screenshots", binaryMessenger: messenger)
    process = FlutterMethodChannel(name: "app.nightdrop/process", binaryMessenger: messenger)
    super.init()
  }

  // Held for the life of the app: the channels and observers live as long as the engine.
  private static var instance: NightDropChannels?

  static func register(with registrar: FlutterPluginRegistrar) {
    let channels = NightDropChannels(messenger: registrar.messenger())
    channels.start()
    instance = channels
  }

  private func start() {
    screenshots.setMethodCallHandler { [weak self] call, result in
      self?.handleScreenshots(call, result)
    }
    process.setMethodCallHandler { [weak self] call, result in
      self?.handleProcess(call, result)
    }
    let center = NotificationCenter.default
    center.addObserver(
      self, selector: #selector(screenshotTaken),
      name: UIApplication.userDidTakeScreenshotNotification, object: nil)
    center.addObserver(
      self, selector: #selector(captureChanged),
      name: UIScreen.capturedDidChangeNotification, object: nil)
    center.addObserver(
      self, selector: #selector(becameActive),
      name: UIApplication.didBecomeActiveNotification, object: nil)
    center.addObserver(
      self, selector: #selector(resignedActive),
      name: UIApplication.willResignActiveNotification, object: nil)
  }

  // MARK: app.nightdrop/screenshots

  private func handleScreenshots(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) {
    switch call.method {
    case "canDetect":
      result(true)
    case "getBlockCapture":
      result(Self.blockCapture())
    case "setBlockCapture":
      UserDefaults.standard.set(call.arguments as? Bool ?? false, forKey: Self.blockCaptureKey)
      sendRecording()
      result(nil)
    case "holdBlock":
      heldByChat = call.arguments as? Bool ?? false
      sendRecording()
      result(nil)
    case "isRecording":
      result(recording)
    case "installerPackage":
      result(nil)  // not an F-Droid install
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  @objc private func screenshotTaken() {
    screenshots.invokeMethod("screenshot", arguments: nil)
  }

  @objc private func captureChanged() {
    sendRecording()
  }

  // The last recording state sent, for Dart to ask when a chat opens (as on Android).
  private var recording = false

  private func sendRecording() {
    let captured = UIApplication.shared.applicationState == .active && Self.screenIsCaptured()
    let blocking = Self.blockCapture() || heldByChat
    setCaptureCover(captured && blocking)
    // A recording that sees only the cover has nothing to report (Android does the same).
    recording = captured && !blocking
    screenshots.invokeMethod("recording", arguments: recording)
  }

  private func setCaptureCover(_ on: Bool) {
    if !on {
      captureCover?.removeFromSuperview()
      captureCover = nil
      return
    }
    guard captureCover == nil, let window = Self.keyWindow() else { return }
    let view = UIView(frame: window.bounds)
    view.backgroundColor = .black
    view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    let label = UILabel()
    label.text = "Night Drop is hidden while the screen is recorded or shared."
    label.textColor = .lightGray
    label.numberOfLines = 0
    label.textAlignment = .center
    label.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(label)
    NSLayoutConstraint.activate([
      label.centerXAnchor.constraint(equalTo: view.centerXAnchor),
      label.centerYAnchor.constraint(equalTo: view.centerYAnchor),
      label.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -48),
    ])
    window.addSubview(view)
    captureCover = view
  }

  private static let blockCaptureKey = "block_capture"

  /// On unless the user turned it off (security settings start on).
  private static func blockCapture() -> Bool {
    UserDefaults.standard.object(forKey: blockCaptureKey) as? Bool ?? true
  }

  private static func screenIsCaptured() -> Bool {
    if #available(iOS 17.0, *), let scene = keyWindow()?.windowScene {
      return scene.traitCollection.sceneCaptureState == .active
    }
    return UIScreen.main.isCaptured
  }

  @objc private func becameActive() {
    cover?.removeFromSuperview()
    cover = nil
    sendRecording()  // a recording already running when the app comes forward counts
  }

  @objc private func resignedActive() {
    // Not watching while inactive: a recording seen again on return is reported as new. The
    // capture cover stays: a recording keeps running while the app is inactive.
    recording = false
    screenshots.invokeMethod("recording", arguments: false)
    // Cover the window before the system snapshots it for the app switcher.
    guard cover == nil, let window = Self.keyWindow() else { return }
    let view = UIView(frame: window.bounds)
    view.backgroundColor = .black
    view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    window.addSubview(view)
    cover = view
  }

  // MARK: app.nightdrop/process

  private func handleProcess(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) {
    switch call.method {
    case "copySensitive":
      let text = call.arguments as? String ?? ""
      UIPasteboard.general.setItems(
        [["public.utf8-plain-text": text]],
        options: [
          .localOnly: true,
          .expirationDate: Date().addingTimeInterval(60),
        ])
      result(nil)
    case "shareText":
      let text = call.arguments as? String ?? ""
      guard let presenter = Self.topViewController() else {
        result(FlutterError(code: "no_window", message: "nothing to present from", details: nil))
        return
      }
      let sheet = UIActivityViewController(activityItems: [text], applicationActivities: nil)
      sheet.excludedActivityTypes = [.copyToPasteboard]
      if let popover = sheet.popoverPresentationController {  // iPad
        popover.sourceView = presenter.view
        popover.sourceRect = CGRect(
          x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 0, height: 0)
        popover.permittedArrowDirections = []
      }
      presenter.present(sheet, animated: true)
      result(nil)
    case "setKeepAlive":
      result(nil)  // iOS decides background lifetime itself
    case "exit":
      // Dart has already shut the core down (Tor closed, saves written). iOS has no supported way
      // to quit; ending the process is what "Exit" promises. Revisit before an App Store build.
      result(nil)
      DispatchQueue.main.async { exit(0) }
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  // MARK: helpers

  private static func keyWindow() -> UIWindow? {
    UIApplication.shared.connectedScenes
      .compactMap { $0 as? UIWindowScene }
      .flatMap { $0.windows }
      .first { $0.isKeyWindow }
  }

  private static func topViewController() -> UIViewController? {
    var top = keyWindow()?.rootViewController
    while let presented = top?.presentedViewController { top = presented }
    return top
  }
}
