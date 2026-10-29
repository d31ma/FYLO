// Static server that makes the page cross-origin isolated.
//
// `SharedArrayBuffer` — and therefore the `Atomics.wait` bridge OPFS needs — is
// unavailable unless the document is cross-origin isolated, which requires both
// headers below. Python's `http.server` sends neither, so the browser gate
// cannot use it.
//
// `--no-isolation` leaves the headers off, as GitHub Pages and most static
// hosts do, for the single-file hosts that must work without them. Byte
// ranges are served either way, so a hosted `.fxdb` can be read in place.
//
// Usage: node scripts/serve-isolated.mjs [port] [root] [--no-isolation]

import { createReadStream, statSync } from 'node:fs'
import { createServer } from 'node:http'
import { extname, join, normalize, resolve } from 'node:path'

const isolated = !process.argv.includes('--no-isolation')
const [portArgument, rootArgument] = process.argv
    .slice(2)
    .filter((value) => !value.startsWith('--'))
const root = resolve(rootArgument ?? '.')
const port = Number(portArgument ?? 4173)

const TYPES = {
    '.html': 'text/html; charset=utf-8',
    '.mjs': 'text/javascript; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.wasm': 'application/wasm',
    '.fxdb': 'application/vnd.d31ma.fxdb'
}

createServer((request, response) => {
    // Containment: a joined path that escapes the root is refused rather than
    // served, so the gate cannot read the machine it runs on.
    const requested = decodeURIComponent((request.url ?? '/').split('?')[0])
    const path = join(root, normalize(requested).replace(/^(\.\.[/\\])+/, ''))
    if (!path.startsWith(root)) {
        response.writeHead(403).end('forbidden')
        return
    }
    let stats
    try {
        stats = statSync(path)
    } catch {
        response.writeHead(404).end('not found')
        return
    }
    if (stats.isDirectory()) {
        response.writeHead(404).end('not found')
        return
    }
    const headers = {
        'Content-Type': TYPES[extname(path)] ?? 'application/octet-stream',
        'Accept-Ranges': 'bytes',
        // Strong, so a client can send it back as `If-Range`.
        ETag: `"${stats.size.toString(16)}-${Math.trunc(stats.mtimeMs).toString(16)}"`,
        'Last-Modified': stats.mtime.toUTCString(),
        'Cache-Control': 'no-store'
    }
    if (isolated) {
        headers['Cross-Origin-Opener-Policy'] = 'same-origin'
        headers['Cross-Origin-Embedder-Policy'] = 'require-corp'
        headers['Cross-Origin-Resource-Policy'] = 'same-origin'
    }
    const range = byteRange(request, headers.ETag, stats.size)
    if (range === 'unsatisfiable') {
        response.writeHead(416, { ...headers, 'Content-Range': `bytes */${stats.size}` }).end()
        return
    }
    if (range) {
        response.writeHead(206, {
            ...headers,
            'Content-Range': `bytes ${range.start}-${range.end}/${stats.size}`,
            'Content-Length': range.end - range.start + 1
        })
        createReadStream(path, range).pipe(response)
        return
    }
    response.writeHead(200, { ...headers, 'Content-Length': stats.size })
    createReadStream(path).pipe(response)
}).listen(port, '127.0.0.1', () => {
    const mode = isolated ? 'cross-origin isolated' : 'without isolation'
    console.log(`serving ${root} ${mode} on http://127.0.0.1:${port}`)
})

/** One `bytes=first-last` range, or null to send the whole file. */
function byteRange(request, etag, size) {
    const header = request.headers.range
    if (!header) return null
    // A changed file answers whole, as RFC 9110 has `If-Range` do.
    const ifRange = request.headers['if-range']
    if (ifRange && ifRange !== etag) return null
    const match = /^bytes=(\d*)-(\d*)$/.exec(header)
    if (!match || (match[1] === '' && match[2] === '')) return null
    let start
    let end
    if (match[1] === '') {
        start = Math.max(0, size - Number(match[2]))
        end = size - 1
    } else {
        start = Number(match[1])
        end = match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1)
    }
    if (start >= size || start > end) return 'unsatisfiable'
    return { start, end }
}
