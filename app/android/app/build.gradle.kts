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
