import { mkdir, rm } from 'node:fs/promises'
import path from 'node:path'
import { collectHtmlFiles, htmlFingerprint } from './dist-html.mjs'
import { normalizeDistHtml } from './postbundle.mjs'
import { runInherited, startInherited } from './process-utils.mjs'

const assetsRoot = path.join(process.cwd(), 'assets')
const bundleLockDir = path.join(process.cwd(), '.bundle-lock')
const args = process.argv.slice(2)
const isWatchMode = args.includes('--watch')
const tailwindArgs = [
  '@tailwindcss/cli',
  '-i', './src/styles.css',
  '-o', './assets/styles.css',
]

async function buildTailwind() {
  await mkdir(assetsRoot, { recursive: true })
  await runInherited('bunx', [
    ...tailwindArgs,
    '--minify',
  ])
}

function startTailwindWatch() {
  return startInherited('bunx', [
    ...tailwindArgs,
    '--watch',
  ])
}

async function runOnce() {
  await mkdir(bundleLockDir, { recursive: true })

  try {
    await runInherited('bunx', ['tach.bundle', ...args])
    await buildTailwind()
    await normalizeDistHtml()
  } finally {
    await rm(bundleLockDir, { recursive: true, force: true })
  }
}

async function runWatch() {
  await buildTailwind()
  const child = startInherited('bunx', ['tach.bundle', ...args])
  const tailwind = startTailwindWatch()
  let lastFingerprint = ''
  let normalizeInFlight = false

  const maybeNormalize = async () => {
    if (normalizeInFlight) return
    normalizeInFlight = true

    try {
      const nextFingerprint = await htmlFingerprint()
      if (!nextFingerprint || nextFingerprint === lastFingerprint) return

      await normalizeDistHtml()
      lastFingerprint = await htmlFingerprint()
    } finally {
      normalizeInFlight = false
    }
  }

  const timer = setInterval(() => {
    void maybeNormalize()
  }, 400)

  const shutdown = (signal) => {
    clearInterval(timer)
    child.kill(signal)
    tailwind.kill(signal)
  }

  process.on('SIGINT', () => shutdown('SIGINT'))
  process.on('SIGTERM', () => shutdown('SIGTERM'))

  child.on('exit', (code) => {
    clearInterval(timer)
    if (!tailwind.killed) tailwind.kill('SIGTERM')
    process.exit(code ?? 0)
  })

  tailwind.on('exit', (code) => {
    if (code && code !== 0) {
      clearInterval(timer)
      if (!child.killed) child.kill('SIGTERM')
      process.exit(code)
    }
  })
}

if (isWatchMode) {
  await runWatch()
} else {
  await runOnce()
}
