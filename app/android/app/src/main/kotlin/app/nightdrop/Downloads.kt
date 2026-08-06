package app.nightdrop

import android.content.ContentValues
import android.content.Context
import android.os.Build
import android.provider.MediaStore
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * Puts a finished file into the user's **public Downloads folder**, where a file manager can
 * actually find it.
 *
 * This exists because the obvious cheaper answer does not work any more. `getExternalStorageDirectory()`
 * returns a path under `/Android/data/`, and since Android 11 the system Files app and every
 * SAF-based file manager refuse to navigate there — so a build written to it is no easier to find
 * than one in app-private storage, which was the complaint. `MediaStore.Downloads` is the supported
 * route to the real folder and, on API 29+, needs **no permission at all**: this ships without
 * `WRITE_EXTERNAL_STORAGE`, which [AndroidManifest.xml] deliberately strips.
 *
 * Below API 29 there is no MediaStore Downloads collection and the public folder needs that
 * permission, so [publish] reports unavailable and the Dart side falls back to app-specific external
 * storage — which *is* browsable on those releases, since the `/Android/data/` lockdown came later.
 *
 * The copy runs with `IS_PENDING` set, so nothing is visible to other apps until the bytes are all
 * there. That mirrors the rule the update path already keeps in Rust: a file a user can see is a
 * file they may be one tap from installing, so an incomplete one must never be visible. A failed
 * copy deletes the row rather than leaving a truncated entry in Downloads.
 */
object Downloads {
    const val CHANNEL = "app.nightdrop/downloads"

    fun install(engine: FlutterEngine, context: Context): MethodChannel =
        MethodChannel(engine.dartExecutor.binaryMessenger, CHANNEL).apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    // Whether the public folder is reachable without a permission. Asked so Dart can
                    // choose its fallback rather than discovering it through an exception.
                    "available" -> result.success(Build.VERSION.SDK_INT >= 29)

                    "publish" -> {
                        val src = call.argument<String>("srcPath")
                        val name = call.argument<String>("displayName")
                        val mime = call.argument<String>("mimeType")
                            ?: "application/octet-stream"
                        if (src == null || name == null) {
                            result.error("bad_args", "srcPath and displayName are required", null)
                        } else {
                            try {
                                result.success(publish(context, File(src), name, mime))
                            } catch (e: Exception) {
                                result.error("publish_failed", e.message, null)
                            }
                        }
                    }

                    else -> result.notImplemented()
                }
            }
        }

    /**
     * Copies [src] into Downloads as [displayName], returning the folder-relative location to show
     * the user, or null if this release has no MediaStore Downloads collection.
     *
     * The name is a request, not a guarantee — MediaStore de-duplicates against what is already
     * there, so the returned location is read back rather than assumed.
     */
    private fun publish(
        context: Context,
        src: File,
        displayName: String,
        mimeType: String,
    ): String? {
        if (Build.VERSION.SDK_INT < 29) return null
        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, displayName)
            put(MediaStore.Downloads.MIME_TYPE, mimeType)
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw java.io.IOException("Downloads rejected the entry")
        try {
            val out = resolver.openOutputStream(uri)
                ?: throw java.io.IOException("Downloads gave no stream to write")
            out.use { sink -> src.inputStream().use { it.copyTo(sink) } }
            values.clear()
            values.put(MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
        } catch (e: Exception) {
            // Leave nothing behind. A half-written entry in Downloads is exactly the "file the user
            // might tap" this is supposed to prevent.
            runCatching { resolver.delete(uri, null, null) }
            throw e
        }

        val actual = resolver.query(
            uri,
            arrayOf(MediaStore.Downloads.DISPLAY_NAME),
            null,
            null,
            null,
        )?.use { if (it.moveToFirst()) it.getString(0) else null } ?: displayName
        return "Downloads/$actual"
    }
}
