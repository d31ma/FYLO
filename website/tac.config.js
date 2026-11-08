// @ts-check

import { readdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'

const VIEWPORT = '<meta name="viewport" content="width=device-width, initial-scale=1">'
const STYLESHEETS = ['/shared/assets/duvay/duvay.min.css', '/shared/assets/site.css']
const MARK = '<!--fylo-head-->'

/** @param {string} root */
async function routeDocuments(root) {
    /** @type {string[]} */
    const found = []

    /** @param {string} directory */
    async function walk(directory) {
        for (const entry of await readdir(directory, { withFileTypes: true })) {
            const target = path.join(directory, entry.name)
            if (entry.isDirectory()) await walk(target)
            else if (entry.name === 'index.html') found.push(target)
        }
    }

    await walk(root)
    return found
}

/**
 * Tachyon client-renders Tac templates. Keep viewport and shared styles in the
 * bootstrap head so every route has a responsive, styled first paint even
 * before JavaScript runs.
 *
 * @param {{ targetRoots: Record<string, string> }} context
 */
export async function postBundle({ targetRoots }) {
    const webRoot = targetRoots.web
    if (!webRoot) return

    const head = [
        VIEWPORT,
        ...STYLESHEETS.map((stylesheet) => `<link rel="stylesheet" href="${stylesheet}">`),
        MARK
    ]
        .map((tag) => `    ${tag}`)
        .join('\n')

    let patched = 0
    for (const file of await routeDocuments(webRoot)) {
        const html = await readFile(file, 'utf8')
        if (html.includes(MARK)) continue
        await writeFile(file, html.replace('</head>', `${head}\n</head>`))
        patched += 1
    }

    console.log(`[fylo] linked shared styles + viewport into ${patched} route documents`)
}

export default { postBundle }
