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
///   * "Block screenshots" has no iOS equivalent (no FLAG_SECURE), so it is never offered here;
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
      result(false)
    case "setBlockCapture", "installerPackage":
      result(nil)  // nothing to block on iOS; not an F-Droid install
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

  private func sendRecording() {
    let captured = UIApplication.shared.applicationState == .active && Self.screenIsCaptured()
    screenshots.invokeMethod("recording", arguments: captured)
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
    // Not watching while inactive: a recording seen again on return is reported as new.
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
