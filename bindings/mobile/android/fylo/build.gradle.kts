plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

// The .so files come from scripts/mobile/build-aar.mjs, laid out by ABI
// (arm64-v8a, x86_64). They are build output, never committed.
val nativeLibraries: String =
    providers.gradleProperty("fylo.jniLibs").getOrElse("build/fylo-jniLibs")
val fyloVersion: String = providers.gradleProperty("fylo.version").getOrElse("0.0.0-dev")

android {
    namespace = "io.github.d31ma.fylo"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
        aarMetadata {
            minCompileSdk = 26
        }
    }

    sourceSets["main"].jniLibs.srcDir(nativeLibraries)

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    packaging {
        jniLibs {
            keepDebugSymbols += "**/libfylo_ffi.so"
        }
    }
}

dependencies {
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("junit:junit:4.13.2")
}

version = fyloVersion
