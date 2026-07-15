// Reusable confirmation dialog. All text comes in as props; accept/cancel go
// back to the Explorer via the pub/sub hub, tagged with `action` so the parent
// knows which dialog (delete vs restore) resolved.

const INITIALIZED = new WeakSet()
const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

export default class {
    title = ''
    bodyPre = ''
    code = ''
    bodyPost = ''
    confirmLabel = 'Confirm'
    danger = false
    action = ''
    restoreFocusTo = null

    ensureFocused() {
        if (typeof document === 'undefined' || !this.tac?.isBrowser || INITIALIZED.has(this)) {
            return ''
        }
        INITIALIZED.add(this)
        this.restoreFocusTo = document.activeElement
        queueMicrotask(() => {
            document.querySelector('.explorer-confirm [data-confirm-cancel]')?.focus()
        })
        return ''
    }

    restoreFocus() {
        const target = this.restoreFocusTo
        queueMicrotask(() => {
            if (target?.isConnected && typeof target.focus === 'function') target.focus()
        })
    }

    accept() {
        this.tac.publish('explorer:confirm', this.action)
        this.restoreFocus()
    }

    cancel() {
        this.tac.publish('explorer:cancel', this.action)
        this.restoreFocus()
    }

    backdrop(event) {
        if (event.target.classList.contains('explorer-confirm')) this.cancel()
    }

    dialogKey(event) {
        if (event.key === 'Escape') {
            event.preventDefault()
            this.cancel()
            return
        }
        if (event.key !== 'Tab') return
        const focusable = [...event.currentTarget.querySelectorAll(FOCUSABLE)]
        if (!focusable.length) {
            event.preventDefault()
            event.currentTarget.focus()
            return
        }
        const first = focusable[0]
        const last = focusable[focusable.length - 1]
        if (event.shiftKey && document.activeElement === first) {
            event.preventDefault()
            last.focus()
        } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault()
            first.focus()
        }
    }
}
