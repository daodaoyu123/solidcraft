plugins { id("com.android.application") }

android {
    namespace = "io.archipelagos.solidcraft"
    compileSdk = 37
    defaultConfig {
        applicationId = "io.archipelagos.solidcraft"
        minSdk = 26
        targetSdk = 37
        versionCode = 2201
        versionName = "0.22.1-android1"
        ndk { abiFilters += ((project.findProperty("abis") as String?) ?: "arm64-v8a,x86_64").split(",") }
    }
    buildTypes {
        debug { isJniDebuggable = false }
        // Phone build: jniLibs from --profile android-small (arm64 only), signed with the debug key.
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    packaging { jniLibs { useLegacyPackaging = true } }
}
