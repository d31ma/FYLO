@Service
export default class LanguageService {
    /** @type {string} */
    @SessionState
    chosen = 'python'

    /**
     * @param {Event} _event
     * @param {string} key
     */
    choose(_event, key) {
        this.chosen = key
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    buttonClass(key) {
        return this._classFor(this.chosen === key)
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    pressed(key) {
        return String(this.chosen === key)
    }

    /**
     * @param {boolean} chosen
     * @returns {string}
     */
    _classFor(chosen) {
        if (chosen) return 'w-btn w-btn-tonal w-btn--sm'
        return 'w-btn w-btn-ghost w-btn--sm'
    }
}
