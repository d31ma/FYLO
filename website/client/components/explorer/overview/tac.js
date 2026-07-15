// Read-only repo dashboard: an aggregate write-activity graph, per-collection/
// bucket item counts + write-cadence sparklines (from .fylo-vcs), plus totals.
// Pure presentation — the parent computes the numbers and passes them down.

export default class {
    /** @type {{ name: string, kind: string, count: number, writes: number, spark: number[] }[]} */
    rows = []
    totals = { collections: 0, buckets: 0, items: 0, commits: 0 }
    /** @type {{ series: number[], from: number, to: number, buckets: number }} */
    chart = { series: [], from: 0, to: 0, buckets: 24 }

    // SVG polyline points for a per-row write-cadence series, normalized to a
    // 100×24 box (viewBox units; the element is scaled by CSS).
    spark(series) {
        if (!series || series.length === 0) return ''
        const max = Math.max(1, ...series)
        const step = series.length > 1 ? 100 / (series.length - 1) : 0
        return series
            .map((v, i) => `${(i * step).toFixed(1)},${(24 - (v / max) * 24).toFixed(1)}`)
            .join(' ')
    }

    hasSpark(series) {
        return Array.isArray(series) && series.some((v) => v > 0)
    }

    hasChart() {
        return this.chart?.series?.some((v) => v > 0) ?? false
    }

    // Bars for the aggregate timeline: each entry is a <rect> spanning one time
    // bucket, with a native `<title>` tooltip (date range + write count).
    bars() {
        const series = this.chart?.series ?? []
        if (series.length === 0) return []
        const max = Math.max(1, ...series)
        const slot = 100 / series.length
        const gap = series.length > 40 ? 0.2 : 1
        return series.map((v, i) => {
            const h = (v / max) * 40
            return {
                x: (i * slot).toFixed(2),
                y: (40 - h).toFixed(2),
                w: Math.max(0.5, slot - gap).toFixed(2),
                h: h.toFixed(2),
                title: `${this.bucketDate(i)} · ${v} write${v === 1 ? '' : 's'}`
            }
        })
    }

    // Approximate date at the centre of time bucket `i`.
    bucketDate(i) {
        const c = this.chart
        if (!c?.to) return ''
        const span = c.to - c.from
        const at = c.from + ((i + 0.5) / (c.buckets || 1)) * span
        return new Date(at).toLocaleDateString()
    }

    fromLabel() {
        return this.chart?.from ? new Date(this.chart.from).toLocaleDateString() : ''
    }

    toLabel() {
        return this.chart?.to ? new Date(this.chart.to).toLocaleDateString() : ''
    }
}
