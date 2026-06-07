import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { collectHtmlFiles, distRoot } from './dist-html.mjs'

function relativeAssetPath(htmlFile, assetName) {
  const htmlDir = path.dirname(htmlFile)
  const relativeRoot = path.relative(htmlDir, distRoot)
  const assetPath = path.posix.join(relativeRoot ? relativeRoot.split(path.sep).join(path.posix.sep) : '.', assetName)
  return assetPath.startsWith('.') ? assetPath : `./${assetPath}`
}

async function normalizeHtmlFile(htmlFile) {
  let html = await readFile(htmlFile, 'utf8')
  const original = html

  for (const assetName of ['main.js', 'spa-renderer.js']) {
    const relativePath = relativeAssetPath(htmlFile, assetName)
    html = html.replaceAll(`src="/${assetName}"`, `src="${relativePath}"`)
  }

  // Inject styles.css link if not already present
  const stylesPath = relativeAssetPath(htmlFile, 'assets/styles.css')
  const stylesLink = `<link rel="stylesheet" href="${stylesPath}">`
  if (!html.includes('styles.css')) {
    html = html.replace('</head>', `    ${stylesLink}\n</head>`)
  } else {
    html = html.replaceAll(`href="/assets/styles.css"`, `href="${stylesPath}"`)
  }

  if (html !== original) {
    await writeFile(htmlFile, html)
  }
}

export async function normalizeDistHtml() {
  const htmlFiles = await collectHtmlFiles()

  for (const htmlFile of htmlFiles) {
    await normalizeHtmlFile(htmlFile)
  }
}

if (import.meta.main) {
  await normalizeDistHtml()
}
