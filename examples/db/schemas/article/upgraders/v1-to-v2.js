/**
 * Upgrade an `article` document from v1 to v2 by deriving `slug` from `title`.
 * Pure function; no I/O, no mutation of input.
 */
export default function upgrade(doc) {
    const slug =
        String(doc.title ?? '')
            .toLowerCase()
            .trim()
            .replace(/[^a-z0-9]+/g, '-')
            .replace(/^-+|-+$/g, '') || 'untitled'
    return { ...doc, slug }
}
