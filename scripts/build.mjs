import { appendFile, cp, mkdir, rm } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'

const here = path.dirname(fileURLToPath(import.meta.url))
const root = path.resolve(here, '..')
const srcDir = path.join(root, 'src')
const distDir = path.join(root, 'dist')
const typeOnlyRuntimeFiles = new Set([
    'src/storage/types.js',
    'src/query/types.js',
    'src/types/fylo.js',
    'src/types/vendor.js'
])

await rm(distDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
await mkdir(distDir, { recursive: true })
await cp(srcDir, distDir, {
    recursive: true,
    filter(source) {
        const relative = path.relative(root, source).split(path.sep).join('/')
        if (!relative || relative === 'src') return true
        if (relative.endsWith('.ts') || relative.endsWith('.d.ts')) return false
        if (path.basename(relative) === '.DS_Store') return false
        if (typeOnlyRuntimeFiles.has(relative)) return false
        return true
    }
})

await new Promise((resolve, reject) => {
    const child = spawn(
        process.execPath,
        [
            path.join(root, 'node_modules', 'typescript', 'bin', 'tsc'),
            '-p',
            'tsconfig.build.json',
            '--noCheck'
        ],
        {
            cwd: root,
            stdio: 'inherit'
        }
    )

    child.on('exit', (code) => {
        if (code === 0) resolve(undefined)
        else reject(new Error(`Type declaration build failed with exit code ${code ?? 'unknown'}`))
    })
    child.on('error', reject)
})

await appendFile(
    path.join(distDir, 'types', 'index.d.ts'),
    '\n\ndeclare global {\n    var Fylo: typeof import("./api/fylo.js").default\n}\n'
)
