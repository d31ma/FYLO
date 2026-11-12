// Build FYLO's Android library (ADR 0026): fylo-ffi for each Android ABI with
// the `mobile` profile, then the Gradle project in bindings/mobile/android.
//
//   bun scripts/mobile/build-aar.mjs           # the AAR
//   bun scripts/mobile/build-aar.mjs --test    # and the instrumented tests on
//                                              # the emulator (take the VM lock)
//
// Needs the pinned NDK, Gradle 8 on PATH (or FYLO_GRADLE naming the project's
// pinned wrapper, as the release job does) and JAVA_HOME set to a JDK 17 to 21.
// Output: <cargo target dir>/fylo-android/fylo-android-<version>.aar
import { spawn } from 'node:child_process'
import { copyFileSync, mkdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { CargoArtifacts } from '../cargo-artifacts.mjs'
import { AndroidEmulator } from './android-emulator.mjs'
import { MobileTargets } from './mobile-targets.mjs'

class AarBuild {
    static PROJECT = fileURLToPath(new URL('../../bindings/mobile/android/', import.meta.url))
    static ABI_FOLDERS = {
        [MobileTargets.ANDROID_ARM]: 'arm64-v8a',
        [MobileTargets.ANDROID_X86]: 'x86_64'
    }
    static LIBRARY = 'libfylo_ffi.so'
    static PROFILE = 'mobile'

    static async run(options) {
        const output = CargoArtifacts.path('fylo-android')
        const libraries = join(output, 'jniLibs')
        for (const target of Object.keys(AarBuild.ABI_FOLDERS)) {
            await AarBuild.buildLibrary(target)
            AarBuild.place(target, libraries)
        }
        await AarBuild.gradle(
            ['--project-dir', AarBuild.PROJECT, ':fylo:assembleRelease'],
            libraries
        )
        AarBuild.publish(output)
        if (options.includes('--test')) await AarBuild.test(libraries)
    }

    static buildLibrary(target) {
        return AarBuild.step(
            `cargo ${target}`,
            'cargo',
            [
                'build',
                '--locked',
                '--profile',
                AarBuild.PROFILE,
                '--target',
                target,
                '-p',
                'fylo-ffi'
            ],
            MobileTargets.environment(target, process.env)
        )
    }

    static place(target, libraries) {
        const folder = join(libraries, AarBuild.ABI_FOLDERS[target])
        mkdirSync(folder, { recursive: true })
        const built = CargoArtifacts.path(target, AarBuild.PROFILE, AarBuild.LIBRARY)
        copyFileSync(built, join(folder, AarBuild.LIBRARY))
    }

    static gradle(tasks, libraries) {
        return AarBuild.step('gradle', process.env.FYLO_GRADLE ?? 'gradle', [
            ...tasks,
            `-Pfylo.jniLibs=${libraries}`,
            `-Pfylo.version=${AarBuild.version()}`,
            '--console=plain'
        ])
    }

    static publish(output) {
        const built = join(AarBuild.PROJECT, 'fylo', 'build', 'outputs', 'aar', 'fylo-release.aar')
        const named = join(output, `fylo-android-${AarBuild.version()}.aar`)
        copyFileSync(built, named)
        console.error(`=== aar ${named}`)
    }

    static async test(libraries) {
        const emulator = new AndroidEmulator(process.env)
        await emulator.start()
        try {
            process.env.ANDROID_SERIAL = emulator.serial
            await AarBuild.gradle(
                ['--project-dir', AarBuild.PROJECT, ':fylo:connectedDebugAndroidTest'],
                libraries
            )
        } finally {
            await emulator.stop()
        }
    }

    static version() {
        const manifest = readFileSync(new URL('../../package.json', import.meta.url), 'utf8')
        return JSON.parse(manifest).version
    }

    static step(name, command, args, env = process.env) {
        console.error(`=== ${name}`)
        return new Promise((resolve, reject) => {
            const child = spawn(command, args, { stdio: 'inherit', env })
            child.once('error', reject)
            child.once('exit', (code) =>
                code === 0 ? resolve(undefined) : reject(new Error(`${name} exited with ${code}`))
            )
        })
    }
}

await AarBuild.run(process.argv.slice(2))
