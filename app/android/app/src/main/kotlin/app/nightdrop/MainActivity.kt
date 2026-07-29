package app.nightdrop

import android.os.Bundle
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity

/**
 * Keeps chat contents out of the app-switcher thumbnail without blocking screenshots.
 *
 * Android snapshots the window when the activity leaves the foreground, and that image persists in
 * Recents — so anyone who opens the task switcher sees the last conversation, with no interaction
 * and no unlock needed. `FLAG_SECURE` suppresses that snapshot, but setting it permanently would
 * also block deliberate screenshots, which are a legitimate thing to want (and which the peer is
 * told about instead, see the screenshot notice).
 *
 * So the flag is held only while backgrounded: added in `onPause`, which runs before the snapshot
 * is taken, and cleared in `onResume` so screenshots work normally whenever the user is actually
 * looking at the app. It is also set in `onCreate` because a configuration change or a very early
 * task switch can reach Recents before the first `onPause` on some OEM builds.
 */
class MainActivity : FlutterActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    override fun onResume() {
        super.onResume()
        // Foreground: let the user screenshot if they choose.
        window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    override fun onPause() {
        // Set before the framework grabs the Recents snapshot, so the thumbnail comes out blank.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        super.onPause()
    }
}
