const TITLE = 'FX | Fylo Explorer'
document.title = TITLE

// The Explorer needs assets the marketing pages don't (the FYLO engine, hljs,
// DuVay's web-component bundle, and its own scoped stylesheet). Inject them the
// first time this route renders in the browser, so the rest of the site stays
// lean and DuVay WCs aren't registered site-wide. Idempotent; injected <head>
// nodes survive Tac morphs (see the standalone Explorer's imports.js).
function loadExplorerAssets() {
    if (globalThis.__fyloExplorerAssets) return
    globalThis.__fyloExplorerAssets = true
    const candidate = globalThis.__FYLO_BUILD || String(Date.now())
    const v = String(candidate)
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$/.test(v)) {
        throw new Error('Invalid FYLO Explorer build token')
    }
    globalThis.__FYLO_BUILD = v
    const addCss = (href) => {
        if (document.querySelector(`link[href^="${href}"]`)) return
        const link = document.createElement('link')
        link.rel = 'stylesheet'
        link.href = `${href}?v=${v}`
        document.head.appendChild(link)
    }
    const addScript = (src, module) => {
        if (document.querySelector(`script[src^="${src}"]`)) return
        const s = document.createElement('script')
        if (module) s.type = 'module'
        s.src = `${src}?v=${v}`
        document.head.appendChild(s)
    }
    addCss('/shared/assets/duvay/duvay.min.css')
    addCss('/shared/assets/site.css')
    addCss('/shared/assets/highlight-theme.css')
    addCss('/shared/assets/explorer.css')
    if (!globalThis.hljs) addScript('/shared/assets/highlight.min.js', false)
    addScript('/shared/assets/duvay/duvay-wc.min.js', true)
}

// Run at module load — the page constructor doesn't re-run on the client after
// prerender+hydrate, but the module does. The prerender stubs `document` (for
// document.title) without querySelector, so gate on the real browser API.
if (typeof document !== 'undefined' && typeof document.querySelector === 'function') {
    loadExplorerAssets()
}

export default class extends Tac {
    constructor(props = {}, tac = undefined) {
        super(props, tac)
        if (this.isBrowser) document.title = TITLE
    }
}
