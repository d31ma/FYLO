// Run the engine crates' tests on the iOS simulator or the Android emulator
// (ADR 0026). Run it under the machine's VM lock for that device.
//
//   bun scripts/test-mobile.mjs <android|ios> ['<nextest filterset>']
//   bun scripts/test-mobile.mjs <android|ios> --parts      # in slices of ~150 tests
//   bun scripts/test-mobile.mjs <android|ios> --build-only
//   bun scripts/test-mobile.mjs ios --warm-up               # first boot, alone
//   bun scripts/test-mobile.mjs ios --remove-device
//
// --build-only compiles the test programs and starts no device, so the long
// build can run as its own job, outside the VM lock. A DeviceGuard watches
// every device run: above its load or process limit it stops this run's own
// process group (saved when it starts) and shuts this run's device down.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { AndroidEmulator } from './mobile/android-emulator.mjs'
import { DeviceGuard } from './mobile/device-guard.mjs'
import { IosSimulator } from './mobile/ios-simulator.mjs'
import { MobileTargets } from './mobile/mobile-targets.mjs'

class MobileTestRun {
    static PACKAGES = [
        'fylo-format',
        'fylo-query',
        'fylo-vfs',
        'fylo-xattr',
        'fylo-identity',
        'fylo-storage-native',
        'fylo-engine',
        'fylo',
        'fylo-ffi'
    ]
    // Tests that cannot run on a phone: they start a program, or read the
    // repository's files, by its path on the Mac.
    static DEVICE_EXCLUDED = [
        'binary(byte_range_lock_harness)',
        'binary(header_matches_code)',
        'test(=stored_validation::tests::stored_validators_answer_exactly_as_native_compilation_does)'
    ]
    // An iOS app cannot start processes, and on the simulator every child a
    // crash test kills wakes iOS's crash reporters: leave out the tests that
    // start children.
    static IOS_EXCLUDED = [
        // Crash and child tests start a child process; two lease tests only
        // simulate a crashed owner in one process, so they run.
        '(test(/crash|killed|child|abort/) & not test(/^container_lease::tests::(a_crashed_owner_is_taken_over|an_empty_lease_left)/))',
        // Needs trusted byte-range locks, which iOS does not have yet.
        'test(=container_lease::tests::a_held_lease_keeps_a_locked_marker_until_it_is_released)',
        'binary(sql_crash)',
        'binary(open_recovery)',
        'binary(layout_move_crash)',
        'binary(platform_companions)',
        'binary(enclave_identity)'
    ]
    // One test program at a time on a phone OS is cheap, many are not: each
    // test is its own process there (2026-11-11).
    static TEST_THREADS = 2
    // Before each part of a --parts run, wait until the Mac is quiet.
    static QUIET_LOAD = 20
    static QUIET_TIMEOUT_MS = 30 * 60_000
    static WARM_UP_SETTLE_MS = 60_000
    // A new device's first boot is heavy by nature; the Mac logged out at a
    // load of 138, so the warm-up's limit stays well below that.
    static WARM_UP_LOAD_LIMIT = 100
    // About 150 tests per part (the suite has about 950).
    static PARTS = 7
    static SCRIPTS = fileURLToPath(new URL('./mobile/', import.meta.url))
    static PLANS = {
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
    }

    constructor(platform, options) {
        this.platform = platform
        this.plan = MobileTestRun.PLANS[platform]
        this.options = options
        this.filter = options.find((option) => !option.startsWith('--'))
        this.child = undefined
        if (this.plan === undefined) {
            throw new Error(
                'Usage: bun scripts/test-mobile.mjs <android|ios> [options] [filterset]'
            )
        }
    }

    async run() {
        if (this.options.includes('--build-only')) return this.build()
        if (this.options.includes('--remove-device')) return this.removeDevice()
        const device = new this.plan.device(process.env)
        await device.prepare?.()
        const guard = new DeviceGuard({
            loadLimit: this.loadLimit(),
            stop: () => this.stopAll(device)
        })
        guard.start()
        try {
            await device.start()
            return await this.onDevice(device)
        } finally {
            guard.end()
            await device.stop()
        }
    }

    loadLimit() {
        if (this.options.includes('--warm-up')) return MobileTestRun.WARM_UP_LOAD_LIMIT
        return DeviceGuard.LOAD_LIMIT
    }

    onDevice(device) {
        if (this.options.includes('--warm-up')) return this.warmUp()
        if (this.options.includes('--parts')) return this.parts(device)
        return this.cargo(this.nextestArguments(MobileTestRun.PACKAGES), device)
    }

    /** A new device's first boot, alone, until the Mac is quiet again. */
    async warmUp() {
        await new Promise((resolve) => setTimeout(resolve, MobileTestRun.WARM_UP_SETTLE_MS))
        await DeviceGuard.quiet(MobileTestRun.QUIET_LOAD, MobileTestRun.QUIET_TIMEOUT_MS)
        console.error('=== warm-up done')
        return 0
    }

    /** The suite in PARTS slices (nextest's partitioning), each after a quiet Mac. */
    async parts(device) {
        let code = 0
        for (let part = 1; part <= MobileTestRun.PARTS; part += 1) {
            await DeviceGuard.quiet(MobileTestRun.QUIET_LOAD, MobileTestRun.QUIET_TIMEOUT_MS)
            console.error(`=== part ${part} of ${MobileTestRun.PARTS}`)
            const slice = ['--partition', `count:${part}/${MobileTestRun.PARTS}`]
            code = Math.max(
                code,
                await this.cargo(
                    [...this.nextestArguments(MobileTestRun.PACKAGES), ...slice],
                    device
                )
            )
        }
        return code
    }

    build() {
        const packages = MobileTestRun.PACKAGES.flatMap((name) => ['-p', name])
        return this.cargo(
            ['test', '--no-run', '--locked', '--target', this.plan.target, ...packages],
            {}
        )
    }

    async removeDevice() {
        await new IosSimulator().remove()
        return 0
    }

    nextestArguments(names) {
        return [
            'nextest',
            'run',
            '--locked',
            '--target',
            this.plan.target,
            ...names.flatMap((name) => ['-p', name]),
            '--no-fail-fast',
            '--test-threads',
            String(MobileTestRun.TEST_THREADS),
            '-E',
            this.filterset()
        ]
    }

    filterset() {
        const excluded = [...MobileTestRun.DEVICE_EXCLUDED, ...this.platformExcluded()].join(' | ')
        if (this.filter === undefined) return `not (${excluded})`
        return `(${this.filter}) & not (${excluded})`
    }

    platformExcluded() {
        if (this.platform === 'ios') return MobileTestRun.IOS_EXCLUDED
        return []
    }

    environment(device) {
        const name = MobileTargets.underscored(this.plan.target).toUpperCase()
        return {
            ...MobileTargets.environment(this.plan.target, process.env),
            [`CARGO_TARGET_${name}_RUNNER`]: `${MobileTestRun.SCRIPTS}${this.plan.runner}`,
            ANDROID_SERIAL: device.serial ?? '',
            FYLO_IOS_SIMULATOR: device.udid ?? ''
        }
    }

    /** Run cargo in its own process group, whose id the guard can stop. */
    cargo(args, device) {
        return new Promise((resolve, reject) => {
            this.child = spawn(process.execPath, ['./scripts/run-rust.mjs', 'cargo', ...args], {
                stdio: 'inherit',
                env: this.environment(device),
                detached: true
            })
            this.child.once('error', reject)
            this.child.once('exit', (exitCode) => resolve(exitCode ?? 1))
        })
    }

    /** The guard's stop: this run's process group, then this run's device. */
    async stopAll(device) {
        this.stopChild()
        await device.stop()
    }

    stopChild() {
        if (this.child?.pid === undefined || this.child.exitCode !== null) return
        process.kill(-this.child.pid, 'SIGTERM')
    }
}

const [platform, ...options] = process.argv.slice(2)
process.exit(await new MobileTestRun(platform, options).run())
