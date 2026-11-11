import { existsSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

/** The iOS and Android Rust targets FYLO builds, and the compilers each one needs (ADR 0026). */
export class MobileTargets {
    static IOS_DEVICE = 'aarch64-apple-ios'
    static IOS_SIMULATOR = 'aarch64-apple-ios-sim'
    static ANDROID_ARM = 'aarch64-linux-android'
    static ANDROID_X86 = 'x86_64-linux-android'
    static NDK_VERSION = '30.0.16248370'
    static ANDROID_MIN_SDK = 26
    static IOS_DEPLOYMENT_TARGET = '17.0'
    static SYSTEM_CLANG = '/usr/bin/clang'

    /** @returns {string[]} */
    static all() {
        return [
            MobileTargets.IOS_DEVICE,
            MobileTargets.IOS_SIMULATOR,
            MobileTargets.ANDROID_ARM,
            MobileTargets.ANDROID_X86
        ]
    }

    /**
     * @param {string} target
     * @returns {boolean}
     */
    static isAndroid(target) {
        return target.endsWith('-linux-android')
    }

    /**
     * The process environment with the C compiler and linker `target` needs.
     *
     * @param {string} target
     * @param {NodeJS.ProcessEnv} environment
     * @returns {NodeJS.ProcessEnv}
     */
    static environment(target, environment) {
        const variables = MobileTargets.isAndroid(target)
            ? MobileTargets.androidVariables(target, environment)
            : MobileTargets.appleVariables(target)
        return { ...environment, ...variables }
    }

    /**
     * Apple's own clang (the Swift toolchain's clang, which may come first
     * on PATH, has no iOS SDK), and iOS 17, the floor of TACHYON's hosts.
     *
     * @param {string} target
     * @returns {Record<string, string>}
     */
    static appleVariables(target) {
        return {
            [`CC_${MobileTargets.underscored(target)}`]: MobileTargets.SYSTEM_CLANG,
            IPHONEOS_DEPLOYMENT_TARGET: MobileTargets.IOS_DEPLOYMENT_TARGET
        }
    }

    /**
     * @param {string} target
     * @param {NodeJS.ProcessEnv} environment
     * @returns {Record<string, string>}
     */
    static androidVariables(target, environment) {
        const bin = MobileTargets.ndkBin(environment)
        const clang = join(bin, `${target}${MobileTargets.ANDROID_MIN_SDK}-clang`)
        const name = MobileTargets.underscored(target)
        return {
            [`CC_${name}`]: clang,
            [`AR_${name}`]: join(bin, 'llvm-ar'),
            [`CARGO_TARGET_${name.toUpperCase()}_LINKER`]: clang
        }
    }

    /**
     * @param {NodeJS.ProcessEnv} environment
     * @returns {string}
     */
    static ndkBin(environment) {
        const ndk = MobileTargets.ndkHome(environment)
        const host = process.platform === 'darwin' ? 'darwin-x86_64' : 'linux-x86_64'
        const bin = join(ndk, 'toolchains', 'llvm', 'prebuilt', host, 'bin')
        if (!existsSync(bin)) {
            throw new Error(`Android NDK ${MobileTargets.NDK_VERSION} is not installed at ${ndk}`)
        }
        return bin
    }

    /**
     * @param {NodeJS.ProcessEnv} environment
     * @returns {string}
     */
    static ndkHome(environment) {
        const sdk = environment.ANDROID_HOME ?? join(homedir(), 'Library', 'Android', 'sdk')
        return environment.ANDROID_NDK_HOME ?? join(sdk, 'ndk', MobileTargets.NDK_VERSION)
    }

    /**
     * @param {string} target
     * @returns {string}
     */
    static underscored(target) {
        return target.replaceAll('-', '_')
    }
}
