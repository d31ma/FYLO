// VS Code–style file/folder icons for the bucket (Miller-column) browser.
// In the spirit of VS Code's Seti theme, colour carries most of the recognition:
// a document/folder silhouette tinted per file-type category. No icon pack is
// bundled — the Explorer stays offline and dependency-free.

// Seti-ish category colours.
const COLOR = {
    image: '#a3cfbb',
    video: '#c586c0',
    audio: '#4ec9b0',
    code: '#519aba',
    script: '#cbcb41',
    json: '#f1c40f',
    markdown: '#519aba',
    style: '#42a5f5',
    data: '#a074c4',
    archive: '#e8a33d',
    pdf: '#e05252',
    doc: '#4285f4',
    text: '#9aa7b0'
}

// extension → category
const CATEGORY = {
    png: 'image',
    jpg: 'image',
    jpeg: 'image',
    gif: 'image',
    webp: 'image',
    svg: 'image',
    avif: 'image',
    bmp: 'image',
    ico: 'image',
    mp4: 'video',
    m4v: 'video',
    webm: 'video',
    mov: 'video',
    ogv: 'video',
    mkv: 'video',
    mp3: 'audio',
    wav: 'audio',
    ogg: 'audio',
    m4a: 'audio',
    flac: 'audio',
    aac: 'audio',
    js: 'script',
    mjs: 'script',
    cjs: 'script',
    jsx: 'script',
    sh: 'script',
    bash: 'script',
    ts: 'code',
    tsx: 'code',
    py: 'code',
    rb: 'code',
    go: 'code',
    rs: 'code',
    java: 'code',
    c: 'code',
    h: 'code',
    cpp: 'code',
    cs: 'code',
    php: 'code',
    swift: 'code',
    kt: 'code',
    dart: 'code',
    html: 'code',
    htm: 'code',
    css: 'style',
    scss: 'style',
    less: 'style',
    json: 'json',
    yaml: 'data',
    yml: 'data',
    toml: 'data',
    xml: 'data',
    csv: 'data',
    tsv: 'data',
    ini: 'data',
    env: 'data',
    md: 'markdown',
    markdown: 'markdown',
    zip: 'archive',
    tar: 'archive',
    gz: 'archive',
    tgz: 'archive',
    rar: 'archive',
    '7z': 'archive',
    bz2: 'archive',
    xz: 'archive',
    pdf: 'pdf',
    doc: 'doc',
    docx: 'doc',
    rtf: 'doc',
    txt: 'text',
    log: 'text'
}

const svg = (inner) =>
    `<svg class="ex-ficon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">${inner}</svg>`

const fileSvg = (color) =>
    svg(
        `<path fill="${color}" d="M4 1h5l4 4v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1z"/>` +
            `<path fill="rgba(0,0,0,.28)" d="M9 1l4 4H9.8A.8.8 0 0 1 9 4.2z"/>`
    )

const folderSvg = (color) =>
    svg(
        `<path fill="${color}" d="M1.5 4.4A1 1 0 0 1 2.5 3.4h3.1l1.3 1.4h6.6a1 1 0 0 1 1 1V13a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z"/>`
    )

function ext(name) {
    const dot = name.lastIndexOf('.')
    return dot > 0 ? name.slice(dot + 1).toLowerCase() : ''
}

/** Inline SVG for a file, tinted by its type category. */
export function fileIconSvg(name) {
    const category = CATEGORY[ext(name)] ?? 'text'
    return fileSvg(COLOR[category] ?? COLOR.text)
}

/** Inline SVG for a folder (VS Code blue). */
export function folderIconSvg() {
    return folderSvg('#7aa6da')
}
