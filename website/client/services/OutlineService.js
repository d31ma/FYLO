@Service
export default class OutlineService {
    /** @type {{ render(): Promise<void> }} */
    @Needs
    taskScheduler

    /** @type {AbortSignal} */
    @Needs
    lifecycle

    /** @type {{ link: string, label: string }[]} */
    headings = []

    /** @type {boolean} */
    showing = false

    /** @type {string} */
    _signature = '[]'

    @OnStart
    initialize() {
        document.addEventListener('tachyon:rendered', () => this._refresh(), {
            signal: this.lifecycle
        })
        this._refresh()
    }

    _refresh() {
        const headings = this._collect()
        const signature = JSON.stringify(headings)
        if (signature === this._signature) return
        this._signature = signature
        this.headings = headings
        this.showing = headings.length > 1
        void this.taskScheduler.render().catch(() => {})
    }

    /** @returns {{ link: string, label: string }[]} */
    _collect() {
        const used = new Set()
        const found = document.querySelectorAll('.doc-main .doc-section > h2')
        return [...found].map((heading) => this._entry(heading, used))
    }

    /**
     * @param {Element} heading
     * @param {Set<string>} used
     * @returns {{ link: string, label: string }}
     */
    _entry(heading, used) {
        heading.id = this._uniqueId(this._baseId(heading), used)
        return { link: `#${heading.id}`, label: heading.textContent.trim() }
    }

    /**
     * @param {Element} heading
     * @returns {string}
     */
    _baseId(heading) {
        return [heading.id, this._slug(String(heading.textContent)), 'section'].find(Boolean)
    }

    /**
     * @param {string} text
     * @returns {string}
     */
    _slug(text) {
        return text
            .trim()
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, '-')
            .replace(/^-|-$/g, '')
    }

    /**
     * @param {string} base
     * @param {Set<string>} used
     * @returns {string}
     */
    _uniqueId(base, used) {
        let id = base
        let suffix = 2
        while (used.has(id)) id = `${base}-${suffix++}`
        used.add(id)
        return id
    }
}
