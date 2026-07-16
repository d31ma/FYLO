const THEME_KEY = 'w-theme'
const THEME_ICON = { dark: '☾', light: '☀' }
const ASSET_VERSION = '__FYLO_ASSET_VERSION__'

globalThis.__FYLO_BUILD = ASSET_VERSION

function assetUrl(path) {
    return `${path}?v=${ASSET_VERSION}`
}

function addStylesheet(path) {
    if (document.querySelector(`link[href^="${path}"]`)) return
    const link = document.createElement('link')
    link.rel = 'stylesheet'
    link.href = assetUrl(path)
    document.head.appendChild(link)
}

function addModulePreload(path) {
    if (document.querySelector(`link[href^="${path}"]`)) return
    const link = document.createElement('link')
    link.rel = 'modulepreload'
    link.href = assetUrl(path)
    document.head.appendChild(link)
}

function addScript(path, type = '') {
    if (document.querySelector(`script[src^="${path}"]`)) return
    const script = document.createElement('script')
    if (type) script.type = type
    script.src = assetUrl(path)
    document.head.appendChild(script)
}

function currentTheme() {
    try {
        return localStorage.getItem(THEME_KEY) || 'dark'
    } catch {
        return 'dark'
    }
}

function applyTheme(theme) {
    document.documentElement.setAttribute('w-theme', theme)
    try {
        localStorage.setItem(THEME_KEY, theme)
    } catch {}
    for (const icon of document.querySelectorAll('[w-theme-icon]')) {
        const next = THEME_ICON[theme] || THEME_ICON.dark
        if (icon.textContent !== next) icon.textContent = next
    }
}

if (typeof document !== 'undefined') {
    document.title = 'FX | Fylo Explorer'
    for (const stylesheet of [
        '/shared/assets/duvay/duvay.min.css',
        '/shared/assets/theme.css',
        '/shared/assets/highlight-theme.css',
        '/shared/assets/explorer.css'
    ]) {
        addStylesheet(stylesheet)
    }
    addModulePreload('/shared/assets/fylo-web.mjs')
    addScript('/shared/assets/highlight.min.js')
    addScript('/shared/assets/duvay/duvay-wc.min.js', 'module')
    applyTheme(currentTheme())

    document.addEventListener('click', (event) => {
        if (!event.target.closest('[w-theme-toggle]')) return
        applyTheme(currentTheme() === 'dark' ? 'light' : 'dark')
    })

    new MutationObserver(() => applyTheme(currentTheme())).observe(document.documentElement, {
        childList: true,
        subtree: true
    })

    const faviconSvg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="#f89b4b"/><path d="M20 10h18l12 12v28a4 4 0 0 1-4 4H20a4 4 0 0 1-4-4V14a4 4 0 0 1 4-4Z" fill="#171007"/><circle cx="26" cy="30" r="3.4" fill="#f89b4b"/><circle cx="26" cy="45" r="3.4" fill="#f89b4b"/><circle cx="39" cy="34" r="3.4" fill="#f89b4b"/><path d="M26 33.4v8.2M39 37.4a10.5 10.5 0 0 1-9.2 7.3" fill="none" stroke="#f89b4b" stroke-width="2.5" stroke-linecap="round"/></svg>`
    const favicon = document.createElement('link')
    favicon.rel = 'icon'
    favicon.type = 'image/svg+xml'
    favicon.href = `data:image/svg+xml,${encodeURIComponent(faviconSvg)}`
    document.head.appendChild(favicon)

    const description = document.createElement('meta')
    description.name = 'description'
    description.content =
        'Fylo Explorer is a local, in-browser workbench for browsing, querying, and editing a FYLO document store.'
    document.head.appendChild(description)
}
