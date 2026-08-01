package app.nightdrop

import android.app.Activity
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * Two halves of the screenshot policy (#1), plus the Recents thumbnail fix.
 *
 * Screenshots are deliberately **allowed**. Blocking them with a permanent `FLAG_SECURE` only
 * pushes someone to photograph the screen with another phone, which no API can detect — so the
 * honest trade is to let the capture happen and make it *visible*: the app logs it and tells the
 * peer (see `Node::report_screenshot`).
 *
 * The Recents snapshot is different, and is blocked. Android snapshots the window when the activity
 * leaves the foreground, and that image sits in the task switcher for anyone who opens it — no
 * interaction, no unlock, and no user intent behind it. So `FLAG_SECURE` is held only while
 * backgrounded: added in `onPause`, which runs before the snapshot is taken, cleared in `onResume`
 * so deliberate screenshots keep working. It is also set in `onCreate` because a configuration
 * change or a very early task switch can reach Recents before the first `onPause` on some OEM
 * builds.
 *
 * Detection is Android 14 (API 34) only. On anything older — and on desktop — a screenshot happens
 * silently, so the peer's silence is not evidence of anything. [CHANNEL]'s `canDetect` exists so the
 * Dart side can tell the user which of those two worlds they are in instead of implying a
 * guarantee.
 */
class MainActivity : FlutterActivity() {
    private var channel: MethodChannel? = null

    // API 34+ only. Held as a field so it can be unregistered in onPause: the callback fires only
    // while the activity is visible, and leaving it registered across the lifecycle leaks it.
    private val screenCaptureCallback =
        if (Build.VERSION.SDK_INT >= 34) {
            Activity.ScreenCaptureCallback {
                // Reports *that* a capture happened. There is no access to the image, by design of
                // the platform API and of this feature.
                channel?.invokeMethod("screenshot", null)
            }
        } else {
            null
        }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL).apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    // Whether this device can report screenshots at all, so the UI can be honest
                    // rather than claiming a protection it doesn't have below API 34.
                    "canDetect" -> result.success(Build.VERSION.SDK_INT >= 34)
                    else -> result.notImplemented()
                }
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    override fun onResume() {
        super.onResume()
        // Foreground: let the user screenshot if they choose.
        window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        if (Build.VERSION.SDK_INT >= 34) {
            screenCaptureCallback?.let { registerScreenCaptureCallback(mainExecutor, it) }
        }
    }

    override fun onPause() {
        if (Build.VERSION.SDK_INT >= 34) {
            screenCaptureCallback?.let { unregisterScreenCaptureCallback(it) }
        }
        // Set before the framework grabs the Recents snapshot, so the thumbnail comes out blank.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        super.onPause()
    }

    override fun onDestroy() {
        channel?.setMethodCallHandler(null)
        channel = null
        super.onDestroy()
    }

    companion object {
        const val CHANNEL = "app.nightdrop/screenshots"
    }
}
