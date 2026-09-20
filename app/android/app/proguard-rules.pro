# R8/ProGuard rules for the release build (minify + resource shrinking, requested on
# fdroiddata!43625).
#
# Scope note before adding anything here: R8 sees only the Java/Kotlin shim. The Dart code lives
# in libapp.so and the security core in libnightdrop.so, so neither the UI logic nor any crypto,
# ratchet or transport code is affected by these rules. What R8 can break is the plugin layer —
# and it breaks it at runtime, silently, on a device. Prefer a keep rule over a clever one.

# Our own Android entry points. MainActivity is reached from the manifest and ForegroundService is
# declared there too, so both are kept without help — but Downloads is reached only from
# MainActivity.configureFlutterEngine, and the MethodChannel contract is a *string* name matched at
# runtime. Keeping the class by name means a future refactor cannot quietly strip the handler and
# leave the Dart side calling into nothing.
-keep class app.nightdrop.MainActivity { *; }
-keep class app.nightdrop.Downloads { *; }

# Flutter ships its own keep rules for the embedding, applied by the Flutter Gradle plugin, so
# there is deliberately no blanket `-keep class io.flutter.**` here. A blanket keep would retain
# PlayStoreDeferredComponentManager among other things, which is precisely the code we do not ship
# — and keeping the whole embedding would undo most of the shrinking this change is for.
#
# Flutter's embedding references Play Core for deferred components. We build no dynamic feature
# modules and F-Droid would not accept the Play libraries, so those references are unreachable —
# but R8 fails the build on them rather than warning, so they have to be named explicitly.
-dontwarn com.google.android.play.core.**

# Plugins ship their own consumer ProGuard rules, which Gradle applies automatically — so there
# are deliberately no blanket keeps for them here. A first attempt kept `com.dexterous.**` and
# `**.zxing.**` wholesale and the dex came out LARGER than the unminified build, which is the
# tell-tale of rules broad enough to make R8 a no-op. Add a narrow rule only when a smoke test on
# hardware shows something actually broke.

# Gson (flutter_local_notifications) reflects over field generic types when restoring scheduled
# notifications after a reboot; without the signature attribute that deserialisation fails.
-keepattributes Signature, *Annotation*

# Kotlin coroutines/metadata used across the plugin layer.
-keepattributes RuntimeVisibleAnnotations,RuntimeVisibleParameterAnnotations
-dontwarn kotlin.**
-dontwarn kotlinx.**

# R8 warns about compile-only references it cannot resolve in these plugins. They are not reached
# on Android at runtime; silencing keeps the build output readable rather than papering over a
# real missing class, which would surface as a NoClassDefFoundError in the smoke test.
-dontwarn javax.annotation.**
-dontwarn org.conscrypt.**
-dontwarn org.bouncycastle.**
-dontwarn org.openjsse.**
