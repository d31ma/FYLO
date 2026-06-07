import path from 'node:path'
import { mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { chromium } from 'playwright'
import pixelmatch from 'pixelmatch'
import { PNG } from 'pngjs'
import { ensureBundle } from '../helpers/ensure-bundle.js'
import { startDistServer } from '../helpers/dist-server.js'

const PROJECT_ROOT = import.meta.dir.replace(/\/tests\/visual$/, '')
const BASELINE_DIR = path.join(PROJECT_ROOT, 'tests/visual/baselines')
const ARTIFACT_DIR = path.join(PROJECT_ROOT, 'tests/visual/artifacts')
const SHOULD_UPDATE = process.env.UPDATE_VISUAL === '1'
const MAX_DIFF_PIXELS = 150
const SCENARIOS = [
  {
    name: 'home-mobile',
    path: '/',
    readySelector: '#hero-heading',
    viewport: { width: 390, height: 844 },
  },
  {
    name: 'home-desktop',
    path: '/',
    readySelector: '#hero-heading',
    viewport: { width: 1440, height: 1200 },
  },
  {
    name: 'docs-mobile',
    path: '/docs',
    readySelector: '.docs-wrap',
    viewport: { width: 390, height: 844 },
  },
  {
    name: 'docs-desktop',
    path: '/docs',
    readySelector: '.docs-wrap',
    viewport: { width: 1440, height: 1200 },
  },
]

let browser
let preview

async function fileExists(filePath) {
  try {
    await stat(filePath)
    return true
  } catch {
    return false
  }
}

async function captureScenario(scenario) {
  const page = await browser.newPage({
    deviceScaleFactor: 1,
    viewport: scenario.viewport,
  })

  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.goto(`${preview.url}${scenario.path}`, { waitUntil: 'networkidle' })
  await page.locator(scenario.readySelector).waitFor()
  await page.addStyleTag({
    content: `
      *,
      *::before,
      *::after {
        animation: none !important;
        transition: none !important;
        caret-color: transparent !important;
        scroll-behavior: auto !important;
      }
    `,
  })
  await page.screenshot({
    fullPage: true,
    path: path.join(ARTIFACT_DIR, `${scenario.name}.actual.png`),
  })
  await page.close()
}

async function compareScenario(scenario) {
  const actualPath = path.join(ARTIFACT_DIR, `${scenario.name}.actual.png`)
  const diffPath = path.join(ARTIFACT_DIR, `${scenario.name}.diff.png`)
  const baselinePath = path.join(BASELINE_DIR, `${scenario.name}.png`)

  if (SHOULD_UPDATE || !(await fileExists(baselinePath))) {
    await mkdir(BASELINE_DIR, { recursive: true })
    const actual = await readFile(actualPath)
    await writeFile(baselinePath, actual)
    await rm(actualPath, { force: true })
    await rm(diffPath, { force: true })
    return { updated: true }
  }

  const [actualBuffer, baselineBuffer] = await Promise.all([
    readFile(actualPath),
    readFile(baselinePath),
  ])
  const actual = PNG.sync.read(actualBuffer)
  const baseline = PNG.sync.read(baselineBuffer)

  if (actual.width !== baseline.width || actual.height !== baseline.height) {
    throw new Error(
      `${scenario.name} dimensions changed from ${baseline.width}x${baseline.height} to ${actual.width}x${actual.height}. ` +
      `Review ${actualPath} and refresh with \`bun run test:visual:update\` if intentional.`
    )
  }

  const diff = new PNG({ width: actual.width, height: actual.height })
  const mismatchCount = pixelmatch(
    actual.data,
    baseline.data,
    diff.data,
    actual.width,
    actual.height,
    { includeAA: false, threshold: 0.1 }
  )

  if (mismatchCount > MAX_DIFF_PIXELS) {
    await writeFile(diffPath, PNG.sync.write(diff))
    throw new Error(
      `${scenario.name} drifted by ${mismatchCount} pixels. ` +
      `See ${actualPath} and ${diffPath}, or run \`bun run test:visual:update\` if the change is intentional.`
    )
  }

  await rm(actualPath, { force: true })
  await rm(diffPath, { force: true })
  return { updated: false }
}

beforeAll(async () => {
  await ensureBundle()
  await mkdir(ARTIFACT_DIR, { recursive: true })
  preview = await startDistServer()
  browser = await chromium.launch()
})

afterAll(async () => {
  await browser?.close()
  preview?.stop()
})

describe('visual snapshots', () => {
  for (const scenario of SCENARIOS) {
    test(`${scenario.name} matches the baseline`, async () => {
      await captureScenario(scenario)
      const result = await compareScenario(scenario)

      if (SHOULD_UPDATE) {
        expect(result.updated).toBeTrue()
        return
      }

      expect(result.updated).toBeFalse()
    })
  }
})
