// Export a document collection to NDJSON, or import NDJSON/JSON into one.
// The parent owns _db/_fs and does the actual work; this component only
// collects the choices and publishes them over the pub/sub hub.
//
// NB: field names avoid Tac-reserved ones (e.g. `target` resolves to the build
// target, `web`), so the export/import selections use `fromCol`/`toCol`.

export default class {
    /** @type {string[]} document collections (export sources / import targets) */
    collections = []
    writable = false
    busy = false
    message = ''

    // Local UI state (not props — never threaded back to the parent).
    fromCol = ''
    toCol = ''
    /** @type {File | null} */
    file = null
    fileName = ''

    setFrom(event) {
        this.fromCol = event.target.value
    }

    setTo(event) {
        this.toCol = event.target.value
    }

    pickFile(event) {
        this.file = event.target.files?.[0] ?? null
        this.fileName = this.file?.name ?? ''
    }

    runExport() {
        if (!this.fromCol) return
        this.tac.publish('explorer:export', this.fromCol)
    }

    runImport() {
        if (!this.file || !this.toCol) return
        this.tac.publish('explorer:import', { collection: this.toCol, file: this.file })
    }
}
