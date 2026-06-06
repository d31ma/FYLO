import path from 'node:path'
import { stat } from 'node:fs/promises'

const PROJECT_ROOT = import.meta.dir.replace(/\/tests\/helpers$/, '')
const DIST_ROOT = path.join(PROJECT_ROOT, 'dist')

async function fileExists(filePath) {
  try {
    const info = await stat(filePath)
    return info.isFile()
  } catch {
    return false
  }
}

async function resolveFilePath(pathname) {
  const safePath = pathname === '/' ? '/index.html' : pathname
  const cleanPath = decodeURIComponent(safePath).replace(/^\/+/, '')
  const directPath = path.join(DIST_ROOT, cleanPath)

  if (await fileExists(directPath)) return directPath

  if (!path.extname(cleanPath)) {
    const nestedIndexPath = path.join(DIST_ROOT, cleanPath, 'index.html')
    if (await fileExists(nestedIndexPath)) return nestedIndexPath
  }

  return null
}

export async function startDistServer() {
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    development: false,
    async fetch(request) {
      const url = new URL(request.url)
      const filePath = await resolveFilePath(url.pathname)

      if (!filePath) {
        return new Response('Not found', { status: 404 })
      }

      return new Response(Bun.file(filePath))
    },
  })

  return {
    port: server.port,
    stop() {
      server.stop(true)
    },
    url: `http://127.0.0.1:${server.port}`,
  }
}
