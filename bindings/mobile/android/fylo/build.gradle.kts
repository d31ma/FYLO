plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
    `maven-publish`
}

// The .so files come from scripts/mobile/build-aar.mjs, laid out by ABI
// (arm64-v8a, x86_64). They are build output, never committed.
val nativeLibraries: String =
    providers.gradleProperty("fylo.jniLibs").getOrElse("build/fylo-jniLibs")
val fyloVersion: String = providers.gradleProperty("fylo.version").getOrElse("0.0.0-dev")
// The release job publishes the AAR it checksummed and attested, not a rebuild.
val verifiedAar: String? = providers.gradleProperty("fylo.aar").orNull

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

    publishing {
        singleVariant("release") {
            withSourcesJar()
        }
    }
}

dependencies {
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("junit:junit:4.13.2")
}

version = fyloVersion

// GitHub Packages is FYLO's Maven registry of record (ADR 0026, D2). The
// release job publishes with its GITHUB_TOKEN:
//   gradlew :fylo:publishReleasePublicationToGitHubPackagesRepository
afterEvaluate {
    publishing {
        publications {
            create<MavenPublication>("release") {
                if (verifiedAar == null) {
                    from(components["release"])
                } else {
                    artifact(file(verifiedAar)) { extension = "aar" }
                }
                groupId = "io.github.d31ma"
                artifactId = "fylo-android"
                version = fyloVersion
                pom {
                    name.set("FYLO for Android")
                    description.set("FYLO's native engine for Android apps: a Kotlin wrapper over its C ABI")
                    url.set("https://github.com/d31ma/Fylo")
                    licenses {
                        license {
                            name.set("MIT")
                            url.set("https://github.com/d31ma/Fylo/blob/main/LICENSE")
                        }
                    }
                }
            }
        }
        repositories {
            maven {
                name = "GitHubPackages"
                url = uri("https://maven.pkg.github.com/d31ma/FYLO")
                credentials {
                    username = System.getenv("GITHUB_ACTOR")
                    password = System.getenv("GITHUB_TOKEN")
                }
            }
        }
    }
}
