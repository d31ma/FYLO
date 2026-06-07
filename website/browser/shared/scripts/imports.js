// ── Global helpers ──────────────────────────────────────────────────────────
globalThis.copySnippet = function(id) {
  const el = document.getElementById(id)
  if (el) navigator.clipboard.writeText(el.innerText).catch(() => {})
}

// ── Favicon (inline SVG — matches the hexagon logo) ───────────────────────
const faviconSVG = `<svg viewBox="0 0 30 30" fill="none" xmlns="http://www.w3.org/2000/svg"><rect width="30" height="30" rx="6" fill="#0b0a08"/><path d="M15 2.5L26.5 9.25V22.75L15 29.5L3.5 22.75V9.25L15 2.5Z" fill="rgba(248,155,75,0.1)" stroke="#f89b4b" stroke-width="1.5" stroke-linejoin="round"/><path d="M10 11h10M10 15h6M10 19h8" stroke="#f89b4b" stroke-width="1.6" stroke-linecap="round"/></svg>`
const favicon = document.createElement('link')
favicon.rel = 'icon'
favicon.type = 'image/svg+xml'
favicon.href = 'data:image/svg+xml,' + encodeURIComponent(faviconSVG)
document.head.appendChild(favicon)

// ── SEO meta tags ──────────────────────────────────────────────────────────
const seoMeta = [
  { name: 'description', content: 'Filesystem-first document storage for Bun with one canonical file per document, collection index files, SQL helpers, realtime listeners, and optional sync hooks.' },
  { name: 'keywords', content: 'fylo, bun, document store, filesystem, sql, sync hooks, realtime, javascript, typescript' },
  { name: 'author', content: 'Fylo contributors' },
  { name: 'robots', content: 'index, follow' },
  { property: 'og:type', content: 'website' },
  { property: 'og:title', content: 'FYLO – Filesystem-first document storage for Bun' },
  { property: 'og:description', content: 'One canonical file per document, one collection index file for fast queries, and optional sync hooks for your replication layer.' },
  { property: 'og:url', content: 'https://fylo.del.ma' },
  { property: 'og:site_name', content: 'FYLO' },
  { name: 'twitter:card', content: 'summary' },
  { name: 'twitter:title', content: 'FYLO – Filesystem-first document storage for Bun' },
  { name: 'twitter:description', content: 'One canonical file per document, one collection index file for fast queries, and optional sync hooks for your replication layer.' },
]
for (const attrs of seoMeta) {
  const meta = document.createElement('meta')
  for (const [k, v] of Object.entries(attrs)) meta.setAttribute(k, v)
  document.head.appendChild(meta)
}
