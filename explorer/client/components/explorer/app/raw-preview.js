// Raw-file preview helpers: map a filename to a MIME type + how the preview
// column should render it, and probe whether bytes actually decode as media.

const MIME = {
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
    avif: 'image/avif',
    bmp: 'image/bmp',
    ico: 'image/x-icon',
    pdf: 'application/pdf',
    html: 'text/html',
    htm: 'text/html',
    mp4: 'video/mp4',
    m4v: 'video/mp4',
    webm: 'video/webm',
    // Chromium does not register video/quicktime, but MOV files using the
    // ISO BMFF H.264/AAC profile decode through its video/mp4 path.
    mov: 'video/mp4',
    ogv: 'video/ogg',
    mp3: 'audio/mpeg',
    wav: 'audio/wav',
    ogg: 'audio/ogg',
    m4a: 'audio/mp4',
    flac: 'audio/flac',
    aac: 'audio/aac',
    txt: 'text/plain',
    md: 'text/plain',
    csv: 'text/plain',
    tsv: 'text/plain',
    log: 'text/plain',
    ini: 'text/plain',
    toml: 'text/plain',
    yaml: 'text/plain',
    yml: 'text/plain',
    xml: 'text/plain',
    json: 'application/json',
    js: 'text/plain',
    mjs: 'text/plain',
    ts: 'text/plain',
    jsx: 'text/plain',
    tsx: 'text/plain',
    css: 'text/plain',
    sh: 'text/plain',
    py: 'text/plain',
    go: 'text/plain',
    rs: 'text/plain',
    rb: 'text/plain',
    java: 'text/plain',
    c: 'text/plain',
    h: 'text/plain',
    cpp: 'text/plain'
}

// highlight.js language id for the text preview (undefined → auto-detect)
const HL = {
    js: 'javascript',
    mjs: 'javascript',
    jsx: 'javascript',
    ts: 'typescript',
    tsx: 'typescript',
    py: 'python',
    rb: 'ruby',
    go: 'go',
    rs: 'rust',
    java: 'java',
    c: 'c',
    h: 'c',
    cpp: 'cpp',
    css: 'css',
    json: 'json',
    xml: 'xml',
    yaml: 'yaml',
    yml: 'yaml',
    toml: 'ini',
    ini: 'ini',
    sh: 'bash',
    md: 'markdown',
    sql: 'sql',
    csv: 'plaintext',
    tsv: 'plaintext',
    txt: 'plaintext',
    log: 'plaintext'
}

// MIME type + preview kind from a filename's extension. Kinds map to how the
// preview column renders: image/video/audio tags, an <iframe> for pdf/html,
// inline text for text/code, download-only for anything else.
export function rawInfo(name) {
    const ext = (name.split('.').pop() || '').toLowerCase()
    const mime = MIME[ext] || 'application/octet-stream'
    let kind = 'other'
    if (mime.startsWith('image/')) kind = 'image'
    else if (mime.startsWith('video/')) kind = 'video'
    else if (mime.startsWith('audio/')) kind = 'audio'
    else if (mime === 'application/pdf') kind = 'pdf'
    else if (mime === 'text/html') kind = 'frame'
    else if (mime.startsWith('text/') || mime === 'application/json') kind = 'text'
    return { mime, kind, lang: HL[ext] }
}

/** @returns {Promise<boolean>} whether the blob URL decodes as a raster image */
export function canDecodeImage(url) {
    return new Promise((resolve) => {
        const img = new Image()
        img.onload = () => resolve(img.naturalWidth > 0)
        img.onerror = () => resolve(false)
        img.src = url
    })
}

/** @returns {Promise<boolean>} whether a media element can load the URL's metadata */
export function canPlayMedia(url, kind) {
    return new Promise((resolve) => {
        const el = document.createElement(kind)
        const done = (ok) => {
            el.removeAttribute('src')
            el.load?.()
            resolve(ok)
        }
        el.preload = 'metadata'
        el.onloadedmetadata = () => done(true)
        el.onerror = () => done(false)
        setTimeout(() => done(false), 3000)
        el.src = url
    })
}
