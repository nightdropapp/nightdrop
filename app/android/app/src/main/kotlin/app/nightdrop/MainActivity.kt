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
 * interaction, no unlock, and no user intent behind it.
 *
 * **API 33+ uses [setRecentsScreenshotEnabled], which is the tool built for exactly this**: the
 * system simply never snapshots the activity, and deliberate screenshots are untouched because
 * `FLAG_SECURE` is never involved. Set once, not toggled.
 *
 * Older releases fall back to holding `FLAG_SECURE` while backgrounded — added in `onPause`,
 * cleared in `onResume`. That fallback is **known to be unreliable**, which is why it is no longer
 * the main path: on a Galaxy S25 (Android 16) the thumbnail still showed the conversation, because
 * the system captures its snapshot as the transition begins and a flag set in `onPause` cannot
 * retroactively blank a frame already taken. Below API 33 there is nothing better available, so it
 * stays as a best effort rather than a guarantee.
 *
 * Detection is Android 14 (API 34) only. On anything older — and on desktop — a screenshot happens
 * silently, so the peer's silence is not evidence of anything. [CHANNEL]'s `canDetect` exists so the
 * Dart side can tell the user which of those two worlds they are in instead of implying a
 * guarantee.
 */
class MainActivity : FlutterActivity() {
    private var channel: MethodChannel? = null

    // Unrelated to screenshots; registered here only because this is where the engine is configured.
    // See [Downloads].
    private var downloads: MethodChannel? = null

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
                    // Who installed us. Used to switch the automatic update check OFF when this
                    // copy came from F-Droid: F-Droid IS the update channel for those users, and a
                    // second updater asking our onion site every day is duplicative — it is on
                    // F-Droid's own review checklist as something to look for. The check stays on
                    // for sideloads and the desktop AppImage, which have no channel at all, and the
                    // manual "Update app" item keeps working everywhere.
                    //
                    // Null when Android will not say (or nobody is recorded), which is treated as
                    // "not F-Droid" — the honest default, since guessing F-Droid would silently
                    // deprive a sideloader of the only update signal they get.
                    "installerPackage" -> result.success(installerPackage())
                    else -> result.notImplemented()
                }
            }
        }
        downloads = Downloads.install(flutterEngine, applicationContext)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33) {
            // Never snapshot this activity for Recents. Permanent, and independent of FLAG_SECURE,
            // so screenshots keep working.
            setRecentsScreenshotEnabled(false)
        } else {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }

    override fun onResume() {
        super.onResume()
        // Foreground: let the user screenshot if they choose. Only the pre-33 fallback sets this
        // flag at all; on 33+ it is never set, so there is nothing to clear.
        if (Build.VERSION.SDK_INT < 33) {
            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
        if (Build.VERSION.SDK_INT >= 34) {
            screenCaptureCallback?.let { registerScreenCaptureCallback(mainExecutor, it) }
        }
    }

    override fun onPause() {
        if (Build.VERSION.SDK_INT >= 34) {
            screenCaptureCallback?.let { unregisterScreenCaptureCallback(it) }
        }
        // Pre-33 fallback only (see the class comment): best effort at blanking the thumbnail.
        if (Build.VERSION.SDK_INT < 33) {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
        super.onPause()
    }

    override fun onDestroy() {
        channel?.setMethodCallHandler(null)
        channel = null
        downloads?.setMethodCallHandler(null)
        downloads = null
        super.onDestroy()
    }

    /// The package that installed this app, or null if unknown. `getInstallSourceInfo` replaced the
    /// deprecated `getInstallerPackageName` in API 30; both can legitimately return null.
    private fun installerPackage(): String? =
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                packageManager.getInstallSourceInfo(packageName).installingPackageName
            } else {
                @Suppress("DEPRECATION")
                packageManager.getInstallerPackageName(packageName)
            }
        } catch (_: Exception) {
            null
        }

    companion object {
        const val CHANNEL = "app.nightdrop/screenshots"
    }
}
