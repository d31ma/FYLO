// ── Favicon (inline SVG — matches the hexagon logo) ───────────────────────
const faviconSVG = `<svg viewBox="0 0 30 30" fill="none" xmlns="http://www.w3.org/2000/svg"><rect width="30" height="30" rx="6" fill="#0b0a08"/><path d="M15 2.5L26.5 9.25V22.75L15 29.5L3.5 22.75V9.25L15 2.5Z" fill="rgba(248,155,75,0.1)" stroke="#f89b4b" stroke-width="1.5" stroke-linejoin="round"/><path d="M10 11h10M10 15h6M10 19h8" stroke="#f89b4b" stroke-width="1.6" stroke-linecap="round"/></svg>`
const favicon = document.createElement('link')
favicon.rel = 'icon'
favicon.type = 'image/svg+xml'
favicon.href = 'data:image/svg+xml,' + encodeURIComponent(faviconSVG)
document.head.appendChild(favicon)

// Fallback PNG favicon for Safari (same SVG rendered as data URI)
const faviconPng = document.createElement('link')
faviconPng.rel = 'alternate icon'
faviconPng.href = favicon.href
document.head.appendChild(faviconPng)

const stylesheet = document.createElement('link')
stylesheet.rel = 'stylesheet'
stylesheet.href = '/assets/styles.css'
document.head.appendChild(stylesheet)

// ── SEO meta tags ──────────────────────────────────────────────────────────
const seoMeta = [
  { name: 'description', content: 'Filesystem-first document storage for Bun with one canonical file per document, collection index files, SQL helpers, realtime listeners, and optional sync hooks.' },
  { name: 'keywords', content: 'fylo, bun, document store, filesystem, s3 files, sql, sync hooks, realtime, javascript, typescript' },
  { name: 'author', content: 'Fylo contributors' },
  { name: 'robots', content: 'index, follow' },
  // Open Graph
  { property: 'og:type', content: 'website' },
  { property: 'og:title', content: 'FYLO – Filesystem-first document storage for Bun' },
  { property: 'og:description', content: 'One canonical file per document, one collection index file for fast queries, and optional sync hooks for your replication layer.' },
  { property: 'og:url', content: 'https://fylo.del.ma' },
  { property: 'og:site_name', content: 'FYLO' },
  // Twitter / X Card
  { name: 'twitter:card', content: 'summary' },
  { name: 'twitter:title', content: 'FYLO – Filesystem-first document storage for Bun' },
  { name: 'twitter:description', content: 'One canonical file per document, one collection index file for fast queries, and optional sync hooks for your replication layer.' },
]

for (const attrs of seoMeta) {
  const meta = document.createElement('meta')
  for (const [k, v] of Object.entries(attrs)) meta.setAttribute(k, v)
  document.head.appendChild(meta)
}

// ── Canonical + title ──────────────────────────────────────────────────────
const canonical = document.createElement('link')
canonical.rel = 'canonical'
canonical.href = location.origin + location.pathname
document.head.appendChild(canonical)

// Update title + canonical on SPA navigation
const _pushState = history.pushState.bind(history)
history.pushState = function(state, title, url) {
  _pushState(state, title, url)
  canonical.href = location.origin + location.pathname
  updatePageTitle(location.pathname)
}
window.addEventListener('popstate', () => {
  canonical.href = location.origin + location.pathname
  updatePageTitle(location.pathname)
})

function updatePageTitle(pathname) {
  const titles = {
    '/': 'FYLO – Filesystem-first document storage for Bun',
    '/docs': 'Documentation – FYLO',
  }
  document.title = titles[pathname] ?? 'FYLO'
}
updatePageTitle(location.pathname)

// Preserve in-page hash navigation before Tachyon's SPA router intercepts links.
document.addEventListener('click', (event) => {
  const link = event.target instanceof Element ? event.target.closest('a[href]') : null
  if (!link) return

  const url = new URL(link.href, location.origin)
  const isSamePageHashLink = (
    url.origin === location.origin &&
    url.pathname === location.pathname &&
    url.hash &&
    !link.hasAttribute('@click')
  )

  if (!isSamePageHashLink) return

  const target = document.getElementById(url.hash.slice(1))
  if (!target) return

  event.preventDefault()
  event.stopImmediatePropagation()

  history.pushState(history.state, '', `${location.pathname}${location.search}${url.hash}`)
  target.scrollIntoView({ behavior: 'smooth', block: 'start' })
}, true)
