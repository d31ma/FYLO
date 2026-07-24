import { mkdir } from 'node:fs/promises'
import path from 'node:path'

function releaseCommit() {
    const candidate = process.env.FYLO_BUILD_COMMIT ?? process.env.GITHUB_SHA
    return candidate && /^[0-9a-f]{40}$/i.test(candidate) ? candidate : 'unknown'
}

function hostBuildTarget() {
    const platform =
        process.platform === 'darwin'
            ? 'macos'
            : process.platform === 'win32'
              ? 'windows'
              : process.platform
    return `${platform}-${process.arch}`
}

function parseArguments(argv) {
    let target
    let output
    for (let index = 0; index < argv.length; index++) {
        const argument = argv[index]
        if (argument !== '--target' && argument !== '--outfile') {
            throw new Error(`Unknown build argument: ${argument}`)
        }
        const value = argv[++index]
        if (!value) throw new Error(`Missing value for ${argument}`)
        if (argument === '--target') target = value
        else output = value
    }
    return { target, output }
}

function targetIdentity(target) {
    if (!target) return hostBuildTarget()
    const match = /^bun-(linux|darwin|windows)-(x64|arm64)$/.exec(target)
    if (!match) throw new Error(`Unsupported FYLO build target: ${target}`)
    const platform = match[1] === 'darwin' ? 'macos' : match[1]
    return `${platform}-${match[2]}`
}

const options = parseArguments(process.argv.slice(2))
const output = path.resolve(
    options.output ?? path.join('dist-bin', process.platform === 'win32' ? 'fylo.exe' : 'fylo')
)
await mkdir(path.dirname(output), { recursive: true })

const commit = releaseCommit()
const definitions = {
    FYLO_BUILD_COMMIT: commit,
    FYLO_BUILD_TARGET: targetIdentity(options.target),
    FYLO_BUILD_KIND: commit === 'unknown' ? 'development-compiled' : 'release'
}
const args = ['bun', 'build', '--compile', './src/cli/index.js', '--outfile', output]
if (options.target) args.push('--target', options.target)
for (const [name, value] of Object.entries(definitions)) {
    args.push('--define', `${name}=${JSON.stringify(value)}`)
}

const build = Bun.spawn(args, {
    stdin: 'inherit',
    stdout: 'inherit',
    stderr: 'inherit'
})
const exitCode = await build.exited
if (exitCode !== 0) process.exit(exitCode)
