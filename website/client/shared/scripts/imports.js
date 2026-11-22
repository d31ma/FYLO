import { installScrollStrips } from './scroll-strips.js'

const THEME_KEY = 'w-theme'
const THEME_ICON = { dark: '☾', light: '☀' }
const THEME_LABEL = { dark: 'Dark', light: 'Light' }

function applyTheme(theme) {
    document.documentElement.setAttribute('w-theme', theme)
    try {
        localStorage.setItem(THEME_KEY, theme)
    } catch (_) {}
    for (const el of document.querySelectorAll('[w-theme-icon]')) {
        el.textContent = THEME_ICON[theme] || THEME_ICON.dark
    }
    for (const el of document.querySelectorAll('[w-theme-label]')) {
        el.textContent = THEME_LABEL[theme] || THEME_LABEL.light
    }
}

function currentTheme() {
    try {
        return localStorage.getItem(THEME_KEY) || 'light'
    } catch (_) {
        return 'light'
    }
}

if (typeof document !== 'undefined') {
    const syncScrollStrips = installScrollStrips()
    // Match the DuVay site structure: light by default, with a persisted choice.
    applyTheme(currentTheme())

    // Delegated toggles — survive any Tac rerender since nothing is bound to nodes.
    document.addEventListener('click', (event) => {
        if (event.target.closest('[w-theme-toggle]')) {
            applyTheme(currentTheme() === 'dark' ? 'light' : 'dark')
        }
    })

    const revealInScroller = (active, scroller) => {
        if (!active || !scroller || scroller.scrollWidth <= scroller.clientWidth) return
        const offset =
            scroller.scrollLeft +
            active.getBoundingClientRect().left -
            scroller.getBoundingClientRect().left -
            (scroller.clientWidth - active.offsetWidth) / 2
        scroller.scrollTo({ left: Math.max(0, offset) })
    }
    const revealLanguageSelections = () => {
        for (const strip of document.querySelectorAll('.doc-sample-langs')) {
            revealInScroller(strip.querySelector('[aria-pressed="true"]'), strip)
        }
    }
    const currentPath = () => location.pathname.replace(/\/$/, '') || '/'
    // Highlight the docs sidebar entry for the current route. Delegated from
    // here rather than bound in the component, so it survives Tac rerenders.
    const syncDocNav = () => {
        const here = currentPath()
        for (const link of document.querySelectorAll('.doc-nav-list a')) {
            const target = link.getAttribute('href').replace(/\/$/, '') || '/'
            link.classList.toggle('active', target === here)
            if (target === here) link.setAttribute('aria-current', 'page')
            else link.removeAttribute('aria-current')
        }
    }
    // On narrow screens the docs sidebar collapses to a horizontal strip, where
    // the current page can sit off-screen with nothing to suggest it exists.
    const revealActive = () => {
        const docLink = document.querySelector('.doc-nav-list a.active')
        revealInScroller(docLink, docLink?.closest('.doc-nav-inner'))
        revealLanguageSelections()
    }
    const syncRoute = () => {
        syncDocNav()
        revealActive()
        syncScrollStrips()
    }
    document.addEventListener('tachyon:rendered', syncRoute)
    document.addEventListener('click', (event) => {
        if (event.target.closest('.doc-sample-langs button')) requestAnimationFrame(revealLanguageSelections)
    })
    syncRoute()

    // Tac rerenders can rebuild the header (and its [w-theme-icon] span) — keep it in sync.
    new MutationObserver(() => {
        const theme = currentTheme()
        for (const el of document.querySelectorAll('[w-theme-icon]')) {
            if (el.textContent !== (THEME_ICON[theme] || THEME_ICON.dark)) {
                el.textContent = THEME_ICON[theme] || THEME_ICON.dark
            }
        }
        for (const el of document.querySelectorAll('[w-theme-label]')) {
            if (el.textContent !== (THEME_LABEL[theme] || THEME_LABEL.light)) {
                el.textContent = THEME_LABEL[theme] || THEME_LABEL.light
            }
        }
        // A morph can also rebuild the docs sidebar and drop `active`.
        syncDocNav()
        syncScrollStrips()
    }).observe(document.documentElement, { childList: true, subtree: true })
}
