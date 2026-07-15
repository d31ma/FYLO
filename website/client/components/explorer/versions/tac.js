// Version-history list. Rows come in as a prop (serializable); the Restore
// action goes back to the Explorer via the pub/sub hub, so no callbacks need
// threading through props. Used by both the document and the file views.

export default class {
    /** @type {{ commit: string, hash: string, message: string, at: string, current: boolean }[]} */
    rows = []
    writable = false
    empty = 'No committed history yet.'

    restore(v) {
        this.tac.publish('explorer:restore', v)
    }
}
