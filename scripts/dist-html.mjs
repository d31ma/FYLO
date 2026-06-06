import { existsSync } from 'node:fs'
import { readdir, stat } from 'node:fs/promises'
import path from 'node:path'

export const distRoot = path.join(process.cwd(), 'dist')

export async function collectHtmlFiles(dir = distRoot) {
  if (!existsSync(dir)) return []

  const entries = await readdir(dir, { withFileTypes: true })
  const files = []

  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name)

    if (entry.isDirectory()) {
      files.push(...await collectHtmlFiles(fullPath))
      continue
    }

    if (entry.isFile() && entry.name.endsWith('.html')) {
      files.push(fullPath)
    }
  }

  return files.sort()
}

export async function htmlFingerprint(dir = distRoot) {
  const htmlFiles = await collectHtmlFiles(dir)
  const parts = []

  for (const file of htmlFiles) {
    const info = await stat(file)
    parts.push(`${file}:${info.size}:${info.mtimeMs}`)
  }

  return parts.join('|')
}
