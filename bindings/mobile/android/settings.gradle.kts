// FYLO's Android library (ADR 0026): the Kotlin wrapper over fylo-ffi's JNI
// entry points. scripts/mobile/build-aar.mjs builds the native libraries and
// runs this build.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "fylo-android"
include(":fylo")
