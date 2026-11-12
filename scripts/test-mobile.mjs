// Run the engine crates' tests on the iOS simulator or the Android emulator
// (ADR 0026). Run it under the machine's VM lock for that device.
//
//   bun scripts/test-mobile.mjs <android|ios> ['<nextest filterset>']
//   bun scripts/test-mobile.mjs <android|ios> --build-only
//
// --build-only compiles the test programs and starts no device, so the long
// build can run as its own job, outside the VM lock.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { AndroidEmulator } from './mobile/android-emulator.mjs'
import { IosSimulator } from './mobile/ios-simulator.mjs'
import { MobileTargets } from './mobile/mobile-targets.mjs'

const PACKAGES = [
    'fylo-format',
    'fylo-query',
    'fylo-vfs',
    'fylo-identity',
    'fylo-storage-native',
    'fylo-engine',
    'fylo',
    'fylo-ffi'
]
// Tests that cannot run on a phone: they start a program, or read the
// repository's files, by its path on the Mac.
const DEVICE_EXCLUDED = [
    'binary(byte_range_lock_harness)',
    'binary(header_matches_code)',
    'test(=stored_validation::tests::stored_validators_answer_exactly_as_native_compilation_does)'
].join(' | ')
const DEVICE_TEST_THREADS = 2
const scripts = fileURLToPath(new URL('./mobile/', import.meta.url))
const [platform, ...options] = process.argv.slice(2)
const buildOnly = options.includes('--build-only')
const filter = options.find((option) => option !== '--build-only')
const plan = {
    android: {
        target: MobileTargets.ANDROID_ARM,
        runner: 'run-on-android.sh',
        device: AndroidEmulator
    },
    ios: {
        target: MobileTargets.IOS_SIMULATOR,
        runner: 'run-on-ios-simulator.sh',
        device: IosSimulator
    }
}[platform]
if (plan === undefined)
    throw new Error('Usage: bun scripts/test-mobile.mjs <android|ios> [filterset]')

if (buildOnly) {
    process.exit(await run(buildArguments(plan.target), environment(plan, {})))
}
const device = new plan.device(process.env)
await device.start()
let code = 1
try {
    code = await run(nextestArguments(plan.target, filter), environment(plan, device))
} finally {
    await device.stop()
}
process.exit(code)

function nextestArguments(target, filterset) {
    const selection = ['-E', deviceFilterset(filterset)]
    const packages = PACKAGES.flatMap((name) => ['-p', name])
    return [
        'nextest',
        'run',
        '--locked',
        '--target',
        target,
        ...packages,
        '--no-fail-fast',
        // One test program at a time on a phone OS is cheap, many are not: an
        // iOS simulator or Android emulator runs every test as its own
        // process, and twelve at once overloaded the Mac (2026-11-11).
        '--test-threads',
        String(DEVICE_TEST_THREADS),
        ...selection
    ]
}

function deviceFilterset(filterset) {
    if (filterset === undefined) return `not (${DEVICE_EXCLUDED})`
    return `(${filterset}) & not (${DEVICE_EXCLUDED})`
}

function buildArguments(target) {
    const packages = PACKAGES.flatMap((name) => ['-p', name])
    return ['test', '--no-run', '--locked', '--target', target, ...packages]
}

function environment(plan, device) {
    const name = MobileTargets.underscored(plan.target).toUpperCase()
    return {
        ...MobileTargets.environment(plan.target, process.env),
        [`CARGO_TARGET_${name}_RUNNER`]: `${scripts}${plan.runner}`,
        ANDROID_SERIAL: device.serial ?? '',
        FYLO_IOS_SIMULATOR: device.udid ?? ''
    }
}

function run(args, env) {
    return new Promise((resolve, reject) => {
        const child = spawn(process.execPath, ['./scripts/run-rust.mjs', 'cargo', ...args], {
            stdio: 'inherit',
            env
        })
        child.once('error', reject)
        child.once('exit', (exitCode) => resolve(exitCode ?? 1))
    })
}
