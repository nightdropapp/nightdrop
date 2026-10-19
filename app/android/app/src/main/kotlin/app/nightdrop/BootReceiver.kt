package app.nightdrop

import android.app.ActivityManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import io.flutter.FlutterInjector
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.FlutterEngineCache
import io.flutter.embedding.engine.dart.DartExecutor

/**
 * Background delivery after a reboot or an app update (ARCHITECTURE.md §11.8).
 *
 * Both end the process, and until now nothing brought delivery back until the user opened the app.
 * The plugin's own `autoRunOnBoot` was no answer: it restarts only the foreground *service*, not
 * the Flutter engine that runs the Tor core, which is the "watching for messages over nothing" case
 * the service refuses. This starts the engine itself, the same cached engine [MainActivity] attaches
 * to, and runs `main` with `--boot`; the Dart side (`BootDelivery`) starts the service and the core,
 * or, when an app lock keeps the key out of reach, posts a reminder and ends the process.
 *
 * Only while background delivery is on, read from [isEnabled]: the copy [MainActivity] keeps of the
 * Dart-side setting, because no Dart runs before this decides.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> Unit
            else -> return
        }
        val app = context.applicationContext
        if (!isEnabled(app)) return
        // Already running (an update can land while the app is open): nothing to bring back.
        if (FlutterEngineCache.getInstance().get(MainActivity.ENGINE_ID) != null) return
        // Being opened right now. Android delivers BOOT_COMPLETED to an app leaving the stopped
        // state (after a force-stop) as it starts, which is usually because the user just tapped
        // it: the broadcast then arrives before the activity has made its engine, and starting a
        // `--boot` engine here would hand the screen a boot-path engine that posts "locked, open
        // Night Drop" while it is being opened. The activity starts the engine itself.
        if (inForeground(app)) return

        val loader = FlutterInjector.instance().flutterLoader()
        loader.startInitialization(app)
        loader.ensureInitializationComplete(app, null)
        val engine = FlutterEngine(app)
        FlutterEngineCache.getInstance().put(MainActivity.ENGINE_ID, engine)
        engine.dartExecutor.executeDartEntrypoint(
            DartExecutor.DartEntrypoint(loader.findAppBundlePath(), "main"),
            listOf(BOOT_ARG),
        )
    }

    /** Whether this process is starting for, or showing, a screen the user is looking at. */
    private fun inForeground(context: Context): Boolean {
        val am = context.getSystemService(ActivityManager::class.java) ?: return false
        val me = am.runningAppProcesses?.firstOrNull { it.pid == android.os.Process.myPid() }
            ?: return false
        return me.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
    }

    companion object {
        /** Keep in step with `BootDelivery.arg` (Dart). */
        const val BOOT_ARG = "--boot"

        private const val PREFS = "nightdrop_process"
        private const val KEY_ENABLED = "background_delivery"

        fun isEnabled(context: Context): Boolean =
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_ENABLED, false)

        /** Called with every `setKeepAlive` from Dart, which reports the setting on each launch. */
        fun setEnabled(context: Context, enabled: Boolean) {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(KEY_ENABLED, enabled)
                .apply()
        }
    }
}
