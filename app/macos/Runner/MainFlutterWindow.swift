import Cocoa
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  private var screenshots: FlutterMethodChannel?

  // A chat whose contact has "Block screenshots" on is open (Dart's `holdBlock`).
  private var heldByChat = false

  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)

    // Before the window is first shown, so no capture sees it unblocked.
    applyCaptureBlock()
    let channel = FlutterMethodChannel(
      name: "app.nightdrop/screenshots",
      binaryMessenger: flutterViewController.engine.binaryMessenger)
    channel.setMethodCallHandler { [weak self] call, result in
      self?.handleScreenshots(call, result)
    }
    screenshots = channel

    super.awakeFromNib()
  }

  /// "Block screenshots" on macOS (lib/src/core/screenshot_detector.dart, `CaptureBlock.partial`):
  /// `sharingType = .none` asks the window server to leave this window out of captures. The older
  /// capture APIs honour it; ScreenCaptureKit on recent macOS may not, so the UI calls it a
  /// request, never a guarantee. Not built or run yet: there is no Mac in the build setup.
  private func handleScreenshots(_ call: FlutterMethodCall, _ result: @escaping FlutterResult) {
    switch call.method {
    case "getBlockCapture":
      result(Self.blockCapture())
    case "setBlockCapture":
      UserDefaults.standard.set(call.arguments as? Bool ?? false, forKey: Self.blockCaptureKey)
      applyCaptureBlock()
      result(nil)
    case "holdBlock":
      heldByChat = call.arguments as? Bool ?? false
      applyCaptureBlock()
      result(nil)
    case "canDetect", "isRecording":
      result(false)  // macOS does not tell an app it is being captured
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  private func applyCaptureBlock() {
    sharingType = (Self.blockCapture() || heldByChat) ? .none : .readOnly
  }

  private static let blockCaptureKey = "block_capture"

  /// On unless the user turned it off (security settings start on).
  private static func blockCapture() -> Bool {
    UserDefaults.standard.object(forKey: blockCaptureKey) as? Bool ?? true
  }
}
