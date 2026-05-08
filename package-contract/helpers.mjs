import { randomBytes } from 'node:crypto'
import { access, copyFile, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { mkdtempSync, rmSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const repoRoot = path.resolve(fileURLToPath(new URL('..', import.meta.url)))
const packageTempRoot = mkdtempSync(path.join(os.tmpdir(), 'fylo-package-'))
let packedTarball

process.on('exit', () => {
  rmSync(packageTempRoot, { recursive: true, force: true })
})

export function uniqueName(prefix = 'fylo') {
  return `${prefix}-${Date.now()}-${randomBytes(3).toString('hex')}`
}

export function run(command, args, { cwd, env = {}, timeout = 120_000 } = {}) {
  const result = spawnSync(command, args, {
    cwd,
    env: { ...process.env, ...env },
    encoding: 'utf8',
    timeout,
  })

  return {
    status: result.status,
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? '',
    error: result.error,
  }
}

export function assertRun(result, label) {
  if (result.status === 0 && !result.error) return

  throw new Error(
    [
      `${label} failed with status ${result.status}`,
      result.error ? `error: ${result.error.message}` : undefined,
      result.stdout ? `stdout:\n${result.stdout}` : undefined,
      result.stderr ? `stderr:\n${result.stderr}` : undefined,
    ]
      .filter(Boolean)
      .join('\n\n')
  )
}

async function fileExists(filePath) {
  try {
    await access(filePath)
    return true
  } catch {
    return false
  }
}

export function fyloTarball() {
  if (process.env.FYLO_PACKAGE_TARBALL) return process.env.FYLO_PACKAGE_TARBALL
  if (packedTarball) return packedTarball

  const pack = run('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', packageTempRoot], {
    cwd: repoRoot,
  })
  assertRun(pack, 'npm pack --json --ignore-scripts')

  const metadata = JSON.parse(pack.stdout)
  const filename = Array.isArray(metadata) && typeof metadata[0]?.filename === 'string'
    ? metadata[0].filename
    : undefined
  if (!filename) {
    throw new Error(`npm pack did not return a tarball filename: ${pack.stdout}`)
  }
  packedTarball = path.join(packageTempRoot, filename)
  return packedTarball
}

export async function createFyloConsumer() {
  const tarball = fyloTarball()
  const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-consumer-'))

  await writeFile(
    path.join(root, 'package.json'),
    JSON.stringify({ private: true, type: 'module' }, null, 2)
  )

  const repoNpmrc = path.join(repoRoot, '.npmrc')
  if (await fileExists(repoNpmrc)) {
    await copyFile(repoNpmrc, path.join(root, '.npmrc'))
  }

  const install = run('bun', ['add', tarball], { cwd: root })
  assertRun(install, `bun add ${tarball}`)

  return {
    root,
    async runModule(source, env = {}) {
      const file = path.join(root, `${uniqueName('script')}.mjs`)
      await writeFile(file, source)

      const result = run('bun', [file], {
        cwd: root,
        env,
      })
      assertRun(result, `bun ${path.basename(file)}`)
      return result
    },
    async cleanup() {
      await rm(root, { recursive: true, force: true })
    },
  }
}
