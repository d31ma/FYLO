// Build fylo-ffi's static library, link tests/c/smoke.c against it with the
// system C compiler, and run it on one new .vdbx file (ADR 0026).
//
//   bun run test:ffi
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { CargoArtifacts } from './cargo-artifacts.mjs'

class FfiSmoke {
    static CRATE = fileURLToPath(new URL('../crates/fylo-ffi/', import.meta.url))
    static SYSTEM_LIBRARIES = {
        darwin: ['-framework', 'Security', '-framework', 'CoreFoundation', '-liconv'],
        linux: ['-lpthread', '-ldl', '-lm']
    }

    static async run() {
        const scratch = mkdtempSync(join(tmpdir(), 'fylo-ffi-smoke-'))
        try {
            await FfiSmoke.step('build', process.execPath, [
                './scripts/run-rust.mjs',
                'cargo',
                'build',
                '--locked',
                '-p',
                'fylo-ffi'
            ])
            const program = join(scratch, 'smoke')
            await FfiSmoke.step('link', '/usr/bin/cc', FfiSmoke.linkArguments(program))
            await FfiSmoke.step('run', program, [join(scratch, 'smoke.vdbx')])
        } finally {
            rmSync(scratch, { recursive: true, force: true })
        }
    }

    static linkArguments(program) {
        return [
            '-std=c11',
            '-Wall',
            '-Werror',
            '-I',
            join(FfiSmoke.CRATE, 'include'),
            join(FfiSmoke.CRATE, 'tests', 'c', 'smoke.c'),
            CargoArtifacts.path('debug', 'libfylo_ffi.a'),
            ...FfiSmoke.systemLibraries(),
            '-o',
            program
        ]
    }

    static systemLibraries() {
        const libraries = FfiSmoke.SYSTEM_LIBRARIES[process.platform]
        if (libraries === undefined) throw new Error(`no link line for ${process.platform}`)
        return libraries
    }

    static step(name, command, args) {
        console.error(`=== ${name}`)
        return new Promise((resolve, reject) => {
            const child = spawn(command, args, { stdio: 'inherit' })
            child.once('error', reject)
            child.once('exit', (code) =>
                code === 0 ? resolve(undefined) : reject(new Error(`${name} exited with ${code}`))
            )
        })
    }
}

await FfiSmoke.run()
