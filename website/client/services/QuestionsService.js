@Service
export default class QuestionsService {
    /** @type {string} */
    _opened = 'rebuild'

    /**
     * @param {Event} _event
     * @param {string} key
     */
    toggle(_event, key) {
        this._opened = this._nextOpened(key)
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    panelClass(key) {
        return this._classFor(this._opened === key)
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    expanded(key) {
        return String(this._opened === key)
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    _nextOpened(key) {
        if (this._opened === key) return ''
        return key
    }

    /**
     * @param {boolean} open
     * @returns {string}
     */
    _classFor(open) {
        if (open) return 'w-expand open'
        return 'w-expand'
    }
}
