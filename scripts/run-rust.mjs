import { CargoArtifacts } from './cargo-artifacts.mjs'
import { spawn } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { delimiter, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const config = await readFile(new URL('../rust-toolchain.toml', import.meta.url), 'utf8')
const toolchain = config.match(/^channel\s*=\s*"([^"]+)"\s*$/m)?.[1]

if (!toolchain) throw new Error('rust-toolchain.toml must define an exact channel')
if (process.argv.length < 3) {
    throw new Error('Usage: bun scripts/run-rust.mjs <cargo|rustc|rustdoc> [...arguments]')
}

const [command, ...arguments_] = process.argv.slice(2)
const args = arguments_.map((value, index) =>
    arguments_[index - 1] === '--output-path' ? CargoArtifacts.report(value) : value
)
const rustc = await rustupWhich('rustc')
const rustdoc = await rustupWhich('rustdoc')
const toolchainBin = dirname(rustc)

await new Promise((resolve, reject) => {
    const child = spawn('rustup', ['run', toolchain, command, ...args], {
        cwd: root,
        env: {
            ...process.env,
            RUSTC: rustc,
            RUSTDOC: rustdoc,
            RUSTUP_TOOLCHAIN: toolchain,
            PATH: `${toolchainBin}${delimiter}${process.env.PATH ?? ''}`
        },
        stdio: 'inherit'
    })
    child.once('error', reject)
    child.once('exit', (code) =>
        code === 0 ? resolve(undefined) : reject(new Error(`${command} exited with ${code}`))
    )
})

async function rustupWhich(binary) {
    return await new Promise((resolve, reject) => {
        const child = spawn('rustup', ['which', binary, '--toolchain', toolchain], {
            cwd: root,
            stdio: ['ignore', 'pipe', 'inherit']
        })
        let output = ''
        child.stdout.on('data', (chunk) => (output += chunk))
        child.once('error', reject)
        child.once('close', (code) => {
            if (code !== 0) {
                reject(new Error(`rustup which ${binary} exited with ${code}`))
                return
            }

            const resolved = output.trim()
            if (!resolved) {
                reject(new Error(`rustup which ${binary} returned an empty path`))
                return
            }

            resolve(resolved)
        })
    })
}
