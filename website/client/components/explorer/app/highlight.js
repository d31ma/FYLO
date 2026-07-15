// Syntax-highlight text with highlight.js (window.hljs, loaded by imports.js).
// Its output escapes the source and only adds <span> tags, so the result is
// safe to render as trusted HTML. The trailing newline keeps the last line from
// clipping when the highlight layer scrolls under a textarea.

const escapeHtml = (text) =>
    text.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c])

/**
 * @param {string} text
 * @param {string | undefined} lang  a hljs language id, or undefined to auto-detect
 * @returns {string} highlighted HTML with a trailing newline
 */
export function highlightToHtml(text, lang) {
    const hljs = globalThis.hljs
    let value
    try {
        if (!hljs) value = escapeHtml(text)
        else if (lang && hljs.getLanguage?.(lang))
            value = hljs.highlight(text, { language: lang, ignoreIllegals: true }).value
        else value = hljs.highlightAuto(text).value // no matching grammar bundled
    } catch {
        value = escapeHtml(text)
    }
    return `${value}\n`
}
