plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "de.samuel.usbcd"
    compileSdk = 35

    defaultConfig {
        applicationId = "de.samuel.usbcd"
        minSdk = 26
        targetSdk = 35
        versionCode = 20
        versionName = "0.6.16"
    }

    // Eigener Release-Schlüssel – legt installer\build-installer.ps1 beim ersten Bauen an
    // und übergibt ihn über Umgebungsvariablen. Ohne sie (z. B. „Run“ in Android Studio)
    // wird mit dem Debug-Schlüssel signiert; USBCD ersetzt so eine Fassung beim Anstecken.
    val releaseStore = System.getenv("USBCD_KEYSTORE")
    signingConfigs {
        if (releaseStore != null) {
            create("release") {
                storeFile = file(releaseStore)
                storePassword = System.getenv("USBCD_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("USBCD_KEY_ALIAS") ?: "usbcd"
                keyPassword = System.getenv("USBCD_KEYSTORE_PASSWORD")
                storeType = "PKCS12"
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    lint {
        // Release-Build auch ohne Lint-Durchlauf (vom Setup-Build automatisch gebaut)
        checkReleaseBuilds = false
        abortOnError = false
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}
// Keine Fremdbibliotheken nötig – nur Android-Framework-APIs.
