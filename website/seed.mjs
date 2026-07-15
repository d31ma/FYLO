// Seeds website/db with demo data for the FYLO Explorer.
// Run: bun run seed   (then open the Explorer and pick website/db as the root)
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../src/index.js'

const root = path.join(import.meta.dir, 'db')
await rm(root, { recursive: true, force: true })

const db = new Fylo(root, { versioning: { autoCommit: false } })

// --- users: a document collection with varied fields for filter/SQL demos ---
await db.users.create()
const USERS = [
    { name: 'Ada Lovelace', role: 'admin', age: 36, team: 'engines' },
    { name: 'Grace Hopper', role: 'admin', age: 85, team: 'compilers' },
    { name: 'Alan Turing', role: 'owner', age: 41, team: 'engines' },
    { name: 'Katherine Johnson', role: 'analyst', age: 101, team: 'trajectories' },
    { name: 'Margaret Hamilton', role: 'owner', age: 88, team: 'guidance' },
    { name: 'Edsger Dijkstra', role: 'viewer', age: 72, team: 'algorithms' },
    { name: 'Barbara Liskov', role: 'admin', age: 84, team: 'abstractions' },
    { name: 'Donald Knuth', role: 'viewer', age: 87, team: 'algorithms' },
    { name: 'Radia Perlman', role: 'analyst', age: 73, team: 'networks' },
    { name: 'Linus Torvalds', role: 'owner', age: 55, team: 'kernels' }
]
/** @type {string[]} */
const userIds = []
for (const user of USERS) userIds.push(await db.users.put(user))

// --- posts: cross-references users, arrays for $contains demos ---
await db.posts.create()
const POSTS = [
    { title: 'Notes on the Analytical Engine', tags: ['history', 'engines'], published: true },
    { title: 'Compiling the future', tags: ['compilers'], published: true },
    { title: 'On computable numbers', tags: ['theory', 'history'], published: true },
    { title: 'Orbital mechanics by hand', tags: ['math'], published: false },
    { title: 'Software engineering, named', tags: ['guidance', 'history'], published: true },
    { title: 'Goto considered harmful', tags: ['essays'], published: true },
    { title: 'Substitutability', tags: ['theory'], published: false },
    { title: 'Literate programming', tags: ['essays', 'books'], published: true }
]
for (let i = 0; i < POSTS.length; i++) {
    await db.posts.put({ ...POSTS[i], authorId: userIds[i % userIds.length], likes: i * 7 })
}

// --- assets: a bucket with every previewable file type at random depths ---
await db.assets.create({ kind: 'file' })

const bin = (b64) => Uint8Array.from(Buffer.from(b64, 'base64'))

// Tiny but valid raster images (1x1) so the image preview actually renders.
const PNG = bin(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
)
const GIF = bin('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7')
const WEBP = bin('UklGRhoAAABXRUJQVlA4TA0AAAAvAAAAEAcQERGIiP4HAA==')
const JPG = bin(
    '/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/wAALCAABAAEBAREA/8QAFAABAAAAAAAAAAAAAAAAAAAAAv/EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAT8Af//Z'
)
const LOGO_SVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" fill="#f89b4b"/><path d="M20 10h18l12 12v28a4 4 0 0 1-4 4H20a4 4 0 0 1-4-4V14a4 4 0 0 1 4-4Z" fill="#171007"/><circle cx="26" cy="30" r="3.4" fill="#f89b4b"/><circle cx="26" cy="45" r="3.4" fill="#f89b4b"/><circle cx="39" cy="34" r="3.4" fill="#f89b4b"/><path d="M26 33.4v8.2M39 37.4a10.5 10.5 0 0 1-9.2 7.3" fill="none" stroke="#f89b4b" stroke-width="2.5" stroke-linecap="round"/></svg>`

// A valid 2x2 24-bit BMP.
function makeBmp() {
    const w = 2,
        h = 2,
        rowSize = Math.ceil((w * 3) / 4) * 4,
        dataSize = rowSize * h,
        fileSize = 54 + dataSize
    const dv = new DataView(new ArrayBuffer(fileSize))
    dv.setUint8(0, 0x42)
    dv.setUint8(1, 0x4d)
    dv.setUint32(2, fileSize, true)
    dv.setUint32(10, 54, true)
    dv.setUint32(14, 40, true)
    dv.setInt32(18, w, true)
    dv.setInt32(22, h, true)
    dv.setUint16(26, 1, true)
    dv.setUint16(28, 24, true)
    dv.setUint32(34, dataSize, true)
    for (let y = 0; y < h; y++) {
        let off = 54 + y * rowSize
        for (let x = 0; x < w; x++) {
            dv.setUint8(off++, 0x4b) // B
            dv.setUint8(off++, 0x9b) // G
            dv.setUint8(off++, 0xf8) // R  → FYLO orange
        }
    }
    return new Uint8Array(dv.buffer)
}

// A valid short WAV tone (playable in the audio preview).
function makeWav() {
    const rate = 8000,
        n = 2400
    const dv = new DataView(new ArrayBuffer(44 + n * 2))
    const s = (o, str) => {
        for (let i = 0; i < str.length; i++) dv.setUint8(o + i, str.charCodeAt(i))
    }
    s(0, 'RIFF')
    dv.setUint32(4, 36 + n * 2, true)
    s(8, 'WAVE')
    s(12, 'fmt ')
    dv.setUint32(16, 16, true)
    dv.setUint16(20, 1, true)
    dv.setUint16(22, 1, true)
    dv.setUint32(24, rate, true)
    dv.setUint32(28, rate * 2, true)
    dv.setUint16(32, 2, true)
    dv.setUint16(34, 16, true)
    s(36, 'data')
    dv.setUint32(40, n * 2, true)
    for (let i = 0; i < n; i++) dv.setInt16(44 + i * 2, Math.round(Math.sin(i * 0.14) * 9000), true)
    return new Uint8Array(dv.buffer)
}

// A minimal valid single-page PDF with correct xref offsets.
function makePdf(text) {
    const content = `BT /F1 22 Tf 24 90 Td (${text}) Tj ET`
    const objs = [
        '<</Type/Catalog/Pages 2 0 R>>',
        '<</Type/Pages/Kids[3 0 R]/Count 1>>',
        '<</Type/Page/Parent 2 0 R/Resources<</Font<</F1 5 0 R>>>>/MediaBox[0 0 320 130]/Contents 4 0 R>>',
        `<</Length ${content.length}>>\nstream\n${content}\nendstream`,
        '<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>'
    ]
    let pdf = '%PDF-1.4\n'
    const offsets = []
    objs.forEach((body, i) => {
        offsets.push(pdf.length)
        pdf += `${i + 1} 0 obj\n${body}\nendobj\n`
    })
    const xref = pdf.length
    pdf += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n`
    for (const o of offsets) pdf += `${String(o).padStart(10, '0')} 00000 n \n`
    pdf += `trailer\n<</Size ${objs.length + 1}/Root 1 0 R>>\nstartxref\n${xref}\n%%EOF`
    return new TextEncoder().encode(pdf)
}

// Extension → bytes. Real, decodable files for png/gif/jpg/webp/svg/bmp, a
// valid PDF, a playable WAV, and all text/code. Formats whose tiny valid bytes
// aren't practical to embed (avif/ico and the compressed A/V codecs) get a
// labelled placeholder — the preview shows a clean "download to view" fallback
// for those rather than a broken player.
const placeholder = (ext) => `(demo placeholder — not a real .${ext} file)\n`
const HTML = '<!doctype html><h1>Report</h1><p>An HTML file rendered in the preview.</p>'
const CODE = {
    txt: 'A plain text file.\nSecond line.\n',
    md: '# Readme\n\n- bullet one\n- bullet two\n\n`inline code`\n',
    csv: 'name,role,age\nAda,admin,36\nGrace,admin,85\n',
    json: JSON.stringify({ name: 'FYLO', kind: 'demo', nested: { ok: true } }, null, 2),
    xml: '<?xml version="1.0"?>\n<root>\n  <item id="1">hello</item>\n</root>\n',
    yaml: 'name: fylo\nfeatures:\n  - buckets\n  - explorer\n',
    log: '[00:00:01] boot\n[00:00:02] ready\n[00:00:03] serving\n',
    js: 'export const add = (a, b) => a + b\nconsole.log(add(2, 3))\n',
    mjs: "import { add } from './add.js'\nexport default add\n",
    ts: 'export function greet(name: string): string {\n  return `hi ${name}`\n}\n',
    jsx: 'export const App = () => <main>hello</main>\n',
    css: '.explorer { display: flex }\n.explorer-row { padding: 0.5rem }\n',
    sh: '#!/bin/sh\nset -e\necho "seeding fylo"\n',
    py: 'def greet(name):\n    return f"hi {name}"\n\nprint(greet("fylo"))\n',
    go: 'package main\n\nimport "fmt"\n\nfunc main() { fmt.Println("fylo") }\n',
    rs: 'fn main() {\n    println!("fylo");\n}\n',
    rb: 'def greet(name)\n  "hi #{name}"\nend\nputs greet("fylo")\n',
    java: 'public class App {\n  public static void main(String[] a) { System.out.println("fylo"); }\n}\n',
    c: '#include <stdio.h>\nint main(){ printf("fylo\\n"); return 0; }\n',
    h: '#ifndef FYLO_H\n#define FYLO_H\nint answer(void);\n#endif\n'
}

/** @type {{ name: string, data: Uint8Array | string }[]} */
const ASSETS = [
    { name: 'photo.png', data: PNG },
    { name: 'pixel.gif', data: GIF },
    { name: 'sample.jpg', data: JPG },
    { name: 'logo.svg', data: LOGO_SVG },
    { name: 'bitmap.bmp', data: makeBmp() },
    { name: 'icon.webp', data: WEBP },
    { name: 'modern.avif', data: placeholder('avif') },
    { name: 'favicon.ico', data: placeholder('ico') },
    { name: 'report.pdf', data: makePdf('FYLO demo PDF') },
    { name: 'page.html', data: HTML },
    { name: 'tone.wav', data: makeWav() },
    { name: 'song.mp3', data: placeholder('mp3') },
    { name: 'audio.ogg', data: placeholder('ogg') },
    { name: 'track.m4a', data: placeholder('m4a') },
    { name: 'lossless.flac', data: placeholder('flac') },
    { name: 'sound.aac', data: placeholder('aac') },
    { name: 'clip.mp4', data: placeholder('mp4') },
    { name: 'movie.webm', data: placeholder('webm') },
    { name: 'video.mov', data: placeholder('mov') },
    { name: 'anim.ogv', data: placeholder('ogv') },
    ...Object.entries(CODE).map(([ext, body]) => ({ name: `file.${ext}`, data: body }))
]

// Scatter every asset across random folder depths.
const FOLDERS = [
    '/',
    '/images/',
    '/images/icons/',
    '/docs/',
    '/docs/2026/',
    '/docs/archive/',
    '/media/',
    '/media/clips/',
    '/code/',
    '/code/src/',
    '/misc/deep/nested/'
]
for (const asset of ASSETS) {
    const folder = FOLDERS[Math.floor(Math.random() * FOLDERS.length)]
    await db.assets.put(new File([asset.data], asset.name), { key: `${folder}${asset.name}` })
}

const users = await db.users.inspect()
const posts = await db.posts.inspect()
const assets = await db.assets.inspect()
console.log(`Seeded ${root}`)
console.log(`  users:  ${users.docsStored} documents`)
console.log(`  posts:  ${posts.docsStored} documents`)
console.log(`  assets: ${assets.docsStored} files`)
