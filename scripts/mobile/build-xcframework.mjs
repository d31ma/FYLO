// Build FYLO's iOS package (ADR 0026): fylo-ffi as a static library for the
// device and the simulator with the `mobile` profile, Fylo.xcframework with
// the CFylo module (crates/fylo-ffi/include), its release zip and SwiftPM
// checksum, inside a copy of the FyloKit Swift package (<target>/fylo-ios/FyloKit).
//
//   bun scripts/mobile/build-xcframework.mjs           # the xcframework and zip
//   bun scripts/mobile/build-xcframework.mjs --test    # and XCTest on a new
//                                                      # simulator (VM lock: ios)
//
// Everything is written under <cargo target dir>/fylo-ios; a test run's
// Xcode derived data lives in a temporary folder and is deleted after it.
import { spawn } from 'node:child_process'
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { CargoArtifacts } from '../cargo-artifacts.mjs'
import { IosSimulator } from './ios-simulator.mjs'
import { MobileTargets } from './mobile-targets.mjs'

class XcframeworkBuild {
    static PACKAGE = fileURLToPath(new URL('../../bindings/mobile/ios/FyloKit/', import.meta.url))
    static HEADERS = fileURLToPath(new URL('../../crates/fylo-ffi/include/', import.meta.url))
    static TARGETS = [MobileTargets.IOS_DEVICE, MobileTargets.IOS_SIMULATOR]
    static LIBRARY = 'libfylo_ffi.a'
    static PROFILE = 'mobile'
    static FRAMEWORK = 'Fylo.xcframework'

    static async run(options) {
        const output = CargoArtifacts.path('fylo-ios')
        for (const target of XcframeworkBuild.TARGETS) await XcframeworkBuild.buildLibrary(target)
        const copy = XcframeworkBuild.assemblePackage(output)
        const framework = join(copy, XcframeworkBuild.FRAMEWORK)
        await XcframeworkBuild.createFramework(framework)
        await XcframeworkBuild.zip(output, framework)
        if (options.includes('--test')) await XcframeworkBuild.test(copy)
    }

    static buildLibrary(target) {
        return XcframeworkBuild.step(
            `cargo ${target}`,
            'cargo',
            [
                'build',
                '--locked',
                '--profile',
                XcframeworkBuild.PROFILE,
                '--target',
                target,
                '-p',
                'fylo-ffi'
            ],
            MobileTargets.environment(target, process.env)
        )
    }

    static createFramework(framework) {
        rmSync(framework, { recursive: true, force: true })
        const libraries = XcframeworkBuild.TARGETS.flatMap((target) => [
            '-library',
            CargoArtifacts.path(target, XcframeworkBuild.PROFILE, XcframeworkBuild.LIBRARY),
            '-headers',
            XcframeworkBuild.HEADERS
        ])
        return XcframeworkBuild.step('xcframework', 'xcodebuild', [
            '-create-xcframework',
            ...libraries,
            '-output',
            framework
        ])
    }

    static async zip(output, framework) {
        const zip = join(output, `Fylo-${XcframeworkBuild.version()}.xcframework.zip`)
        rmSync(zip, { force: true })
        await XcframeworkBuild.step('zip', 'ditto', [
            '-c',
            '-k',
            '--sequesterRsrc',
            '--keepParent',
            framework,
            zip
        ])
        const checksum = await XcframeworkBuild.capture('swift', [
            'package',
            'compute-checksum',
            zip
        ])
        writeFileSync(`${zip}.checksum`, `${checksum.trim()}\n`)
        console.error(`=== xcframework ${zip} checksum ${checksum.trim()}`)
    }

    /**
     * A copy of the FyloKit package beside the build output; the xcframework
     * is then written into it, so nothing large is copied.
     */
    static assemblePackage(output) {
        const copy = join(output, 'FyloKit')
        rmSync(copy, { recursive: true, force: true })
        mkdirSync(copy, { recursive: true })
        for (const part of ['Package.swift', 'Sources', 'Tests']) {
            cpSync(join(XcframeworkBuild.PACKAGE, part), join(copy, part), { recursive: true })
        }
        return copy
    }

    /**
     * XCTest on a new simulator. Xcode's derived data goes to a temporary
     * folder on a local disk, deleted afterwards: Xcode renames its files in
     * place, which an NFS build volume refuses, and tests need no index.
     */
    static async test(copy) {
        const derived = mkdtempSync(join(tmpdir(), 'fylo-xctest-'))
        const simulator = new IosSimulator()
        await simulator.start()
        try {
            await XcframeworkBuild.step(
                'xctest',
                'xcodebuild',
                [
                    'test',
                    '-scheme',
                    'FyloKit',
                    '-destination',
                    `platform=iOS Simulator,id=${simulator.udid}`,
                    '-derivedDataPath',
                    derived,
                    'COMPILER_INDEX_STORE_ENABLE=NO'
                ],
                process.env,
                copy
            )
        } finally {
            await simulator.stop()
            rmSync(derived, { recursive: true, force: true })
        }
    }

    static version() {
        const manifest = readFileSync(new URL('../../package.json', import.meta.url), 'utf8')
        return JSON.parse(manifest).version
    }

    static step(name, command, args, env = process.env, cwd = undefined) {
        console.error(`=== ${name}`)
        return new Promise((resolve, reject) => {
            const child = spawn(command, args, { stdio: 'inherit', env, cwd })
            child.once('error', reject)
            child.once('exit', (code) =>
                code === 0 ? resolve(undefined) : reject(new Error(`${name} exited with ${code}`))
            )
        })
    }

    static capture(command, args) {
        return new Promise((resolve, reject) => {
            const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'inherit'] })
            let text = ''
            child.stdout.on('data', (chunk) => {
                text += chunk
            })
            child.once('error', reject)
            child.once('exit', (code) =>
                code === 0 ? resolve(text) : reject(new Error(`${command} exited with ${code}`))
            )
        })
    }
}

await XcframeworkBuild.run(process.argv.slice(2))
