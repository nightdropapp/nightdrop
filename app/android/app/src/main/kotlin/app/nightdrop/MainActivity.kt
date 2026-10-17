package app.nightdrop

import android.app.Activity
import android.app.NotificationManager
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import android.os.Process
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.FlutterEngineCache
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

    // Lets Dart say whether the engine should outlive this screen, and end the app on Exit.
    private var process: MethodChannel? = null

    /**
     * One engine for the whole process, not one per screen.
     *
     * The Dart side is not just UI: it runs the Tor core's poller and posts the notifications.
     * Owned by the activity, it died whenever the activity did — swipe the app away and background
     * delivery stopped receiving while its "Watching for messages" notification said otherwise,
     * and reopening built a second core over the one still shutting down (see
     * docs/advisories/2026-09-26-reopen-after-swipe-showed-recovery-screen.md). Cached here, a new
     * screen reattaches to the running engine instead.
     *
     * Whether it is *kept* when the screen goes is [keepEngine]'s call, made in [onDestroy].
     */
    override fun provideFlutterEngine(context: Context): FlutterEngine =
        FlutterEngineCache.getInstance().get(ENGINE_ID)
            ?: FlutterEngine(context.applicationContext).also {
                FlutterEngineCache.getInstance().put(ENGINE_ID, it)
            }

    // The engine is ours (cached above), not the screen's: onDestroy decides its fate.
    override fun shouldDestroyEngineWithHost(): Boolean = false

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // An engine BootReceiver started had no screen, so its Dart `setKeepAlive` reached no
        // channel; without this the engine would be destroyed with the first screen that closes.
        keepEngine = keepEngine || BootReceiver.isEnabled(applicationContext)
        process = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, PROCESS_CHANNEL).apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    // True while background delivery is on: the engine then outlives the screen.
                    "setKeepAlive" -> {
                        keepEngine = call.arguments as? Boolean ?: false
                        // Remembered for BootReceiver, which decides before any Dart runs.
                        BootReceiver.setEnabled(applicationContext, keepEngine)
                        result.success(null)
                    }
                    // Issue #15. Dart has already stopped the service and shut the core down (Tor
                    // closed, saves written); all that is left is the process. Ended outright rather
                    // than left cached, so "Exit" cannot mean "still running somewhere".
                    "exit" -> {
                        result.success(null)
                        Handler(Looper.getMainLooper()).post {
                            finishAndRemoveTask()
                            FlutterEngineCache.getInstance().remove(ENGINE_ID)
                            Process.killProcess(Process.myPid())
                        }
                    }
                    // An invite link carries a single-use pass: mark the copy sensitive, so the
                    // clipboard preview hides it and keyboards that honour the flag keep it out of
                    // their history (self-audit 2026-10-17). The extra predates its API 33
                    // constant and is ignored where unknown.
                    "copySensitive" -> {
                        val text = call.arguments as? String ?: ""
                        val clip = ClipData.newPlainText("Night Drop", text)
                        clip.description.extras = PersistableBundle().apply {
                            putBoolean("android.content.extra.IS_SENSITIVE", true)
                        }
                        (getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
                            .setPrimaryClip(clip)
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        }
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
                    // Opt-in "Block screenshots" (self-audit 2026-10-17): FLAG_SECURE held for the
                    // whole life of the window, which also blocks screen recording and casting.
                    // Kept in Android's own preferences so it applies before any Dart runs.
                    "getBlockCapture" -> result.success(blockCapture())
                    "setBlockCapture" -> {
                        val on = call.arguments as? Boolean ?: false
                        getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                            .putBoolean(PREF_BLOCK_CAPTURE, on).apply()
                        if (on) {
                            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        } else {
                            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        }
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        }
        downloads = Downloads.install(flutterEngine, applicationContext)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        renameBackgroundDeliveryChannel()
        if (Build.VERSION.SDK_INT >= 33) {
            // Never snapshot this activity for Recents. Permanent, and independent of FLAG_SECURE,
            // so screenshots keep working.
            setRecentsScreenshotEnabled(false)
        } else {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
        if (blockCapture()) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    private fun blockCapture(): Boolean =
        getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(PREF_BLOCK_CAPTURE, false)

    override fun onResume() {
        super.onResume()
        // Foreground: let the user screenshot if they choose. Only the pre-33 fallback sets this
        // flag at all; on 33+ it is never set, so there is nothing to clear.
        if (Build.VERSION.SDK_INT < 33 && !blockCapture()) {
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
        process?.setMethodCallHandler(null)
        process = null
        val engine = flutterEngine
        super.onDestroy()
        // Kept only when background delivery needs it; otherwise the engine goes with the screen,
        // as it always did. Never on a configuration change, which recreates the screen at once.
        if (!keepEngine && !isChangingConfigurations) {
            downloads?.setMethodCallHandler(null)
            FlutterEngineCache.getInstance().remove(ENGINE_ID)
            engine?.destroy()
        }
        // Left installed when the engine is kept: it works on the application context, and an
        // update download can still be finishing in the background.
        downloads = null
    }

    /**
     * Rename an existing background-delivery notification channel to match the Dart side
     * (`BackgroundDelivery.init` — keep the two in step).
     *
     * The plugin creates the channel only if it is missing, so installs that already have it would
     * keep the old name, "Background delivery" — the same as the in-app switch, which sent a user
     * to turn off delivery itself when they meant to hide this notification. Re-registering an
     * existing id updates its name and description and leaves the user's settings alone.
     */
    private fun renameBackgroundDeliveryChannel() {
        if (Build.VERSION.SDK_INT < 26) return
        val nm = getSystemService(NotificationManager::class.java) ?: return
        val channel = nm.getNotificationChannel("nightdrop_background") ?: return
        channel.name = "Background delivery status"
        channel.description = "Shows that Night Drop is checking for messages in the background. " +
            "Turning this off only hides the notification — messages keep arriving."
        nm.createNotificationChannel(channel)
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
        const val PREFS = "nightdrop_ui"
        const val PREF_BLOCK_CAPTURE = "block_capture"
        const val PROCESS_CHANNEL = "app.nightdrop/process"
        const val ENGINE_ID = "main"

        /** Set from Dart via `setKeepAlive`; process-wide, since the engine outlives any screen. */
        @Volatile
        private var keepEngine = false
    }
}
