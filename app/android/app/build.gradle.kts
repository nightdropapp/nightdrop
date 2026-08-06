import com.android.build.gradle.internal.api.ApkVariantOutputImpl
import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// Store identity + launcher name, overridable at build time without editing sources —
// e.g. before a full release rename:
//   ORG_GRADLE_PROJECT_appId=org.example.newid \
//   ORG_GRADLE_PROJECT_appName="New Name" flutter build apk
// (install-android-app.sh exposes these as NIGHTDROP_APP_ID / NIGHTDROP_APP_NAME.)
// The Kotlin `namespace` below stays fixed — it names code, not the shipped identity.
val appId = (project.findProperty("appId") as String?) ?: "app.nightdrop"
val appName = (project.findProperty("appName") as String?) ?: "Night Drop"

// Release signing (MAINTENANCE.md §11): create android/key.properties (gitignored) with
//   storeFile=/absolute/path/to/nightdrop-release.jks
//   storePassword=...
//   keyAlias=nightdrop
//   keyPassword=...
// Without it, release builds fall back to the DEBUG key — fine for local testing,
// never for distribution.
val keystoreProperties = Properties()
val keystorePropertiesFile = rootProject.file("key.properties")
if (keystorePropertiesFile.exists()) {
    FileInputStream(keystorePropertiesFile).use { keystoreProperties.load(it) }
}

android {
    namespace = "app.nightdrop"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    // Strip AGP's encrypted "Dependency metadata" APK signing block. It is opaque and
    // non-reproducible, and F-Droid's `check apk` scanner rejects any extra signing block
    // ("Found extra signing block 'Dependency metadata'"). Off for both APK and bundle.
    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
        // flutter_local_notifications uses java.time APIs that need desugaring on older APIs.
        isCoreLibraryDesugaringEnabled = true
    }

    defaultConfig {
        applicationId = appId
        // Fills android:label="${appName}" in AndroidManifest.xml.
        manifestPlaceholders["appName"] = appName
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        // flutter_secure_storage's encrypted SharedPreferences requires API 23+.
        minSdk = maxOf(flutter.minSdkVersion, 23)
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (keystorePropertiesFile.exists()) {
            create("release") {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = if (keystorePropertiesFile.exists()) {
                signingConfigs.getByName("release")
            } else {
                // No keystore configured: debug-sign so `flutter run --release` still
                // works locally. A distributable release REQUIRES key.properties.
                signingConfigs.getByName("debug")
            }
        }
    }
}

dependencies {
    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.4")
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

// F-Droid's per-ABI version-code scheme (requested by the reviewer on fdroiddata!43625).
//
// Flutter's own `--split-per-abi` default adds 1000/2000/4000 per ABI, which makes the version
// codes of ONE release span a range wider than the gap between releases: x86_64 of 0.1.15 is
// 4016, while armeabi-v7a of 0.1.16 would be 1017. Anything picking "the highest version code"
// — F-Droid's current-version logic included — then reads an older release as the newest.
//
// `versionCode * 10 + abi` keeps every ABI of a release adjacent and the ordering monotonic
// across releases.
//
// The universal APK gets slot 4 — ABOVE all three per-ABI builds — and this is the point that was
// wrong until 0.1.18. It used to keep the plain base code, which put it *below* every per-ABI
// build of the same release: universal 0.1.17 was 403 while its own arm64 build was 4032. Android
// reads that as a downgrade and refuses to install, so anyone running a per-ABI build — every
// F-Droid user, and now everyone the in-app updater has served, since that deliberately fetches
// per-ABI — got "App not installed" from the website's primary download, with nothing to say why.
// Slot 4 is above 4043 and above the old universal 403, so it installs over anything shipped so
// far and stays monotonic in both directions.
//
// NOTE: the codes this produces must stay ABOVE anything already published, or Android refuses
// the update as a downgrade. See MAINTENANCE.md — the base version code was raised when this
// scheme was adopted, precisely because 16*10+1 = 161 is far below the 4016 already shipped.
val abiCodes = mapOf("armeabi-v7a" to 1, "arm64-v8a" to 2, "x86_64" to 3)
// Plain `val`, not `const val`: a .kts script body is not a Kotlin top level, and `const` there
// fails compilation with "Const 'val' is only allowed on top level, in named objects, or in
// companion objects."
val universalAbiSlot = 4
android.applicationVariants.configureEach {
    val variant = this
    variant.outputs.forEach { output ->
        val abi = output.filters.find { it.filterType == "ABI" }?.identifier
        // No ABI filter means the universal APK.
        val slot = if (abi == null) universalAbiSlot else abiCodes[abi]
        if (slot != null) {
            (output as ApkVariantOutputImpl).versionCodeOverride = variant.versionCode * 10 + slot
        }
    }
}
