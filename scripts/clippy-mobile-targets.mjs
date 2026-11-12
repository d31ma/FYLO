// Clippy for the iOS and Android targets (ADR 0026). Build scripts run, so
// Android needs the pinned NDK; iOS needs Xcode.
import { spawn } from 'node:child_process'
import { MobileTargets } from './mobile/mobile-targets.mjs'

const PACKAGES = [
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

for (const target of MobileTargets.all()) {
    console.error(`=== clippy --target ${target}`)
    await run(
        process.execPath,
        [
            './scripts/run-rust.mjs',
            'cargo',
            'clippy',
            '--locked',
            '--target',
            target,
            ...PACKAGES.flatMap((name) => ['-p', name]),
            '--all-targets',
            '--',
            '-D',
            'warnings'
        ],
        MobileTargets.environment(target, process.env)
    )
}
console.log(`Clippy passed for ${MobileTargets.all().join(', ')}`)

function run(command, args, environment) {
    return new Promise((resolve, reject) => {
        const child = spawn(command, args, { stdio: 'inherit', env: environment })
        child.once('error', reject)
        child.once('exit', (code) =>
            code === 0 ? resolve(undefined) : reject(new Error(`${command} exited with ${code}`))
        )
    })
}
