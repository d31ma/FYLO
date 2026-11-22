@Service
export default class InstallCommandService {
    /** @type {{ render(): Promise<void> }} */
    @Needs
    taskScheduler

    /** @type {AbortSignal} */
    @Needs
    lifecycle

    /** @type {string} */
    command = 'curl -fsSL https://fylo.del.ma/install.sh | sh'

    /** @type {string} */
    status = 'Copy'

    /** @type {ReturnType<typeof setTimeout> | undefined} */
    _timer = undefined

    async copy() {
        this.status = await this._statusAfterWriting()
        clearTimeout(this._timer)
        this._timer = setTimeout(() => this._reset(), 2200)
    }

    @OnStop
    dispose() {
        clearTimeout(this._timer)
    }

    /** @returns {Promise<string>} */
    _statusAfterWriting() {
        return navigator.clipboard.writeText(this.command).then(
            () => 'Copied',
            () => 'Select to copy'
        )
    }

    _reset() {
        if (this.lifecycle.aborted) return
        this.status = 'Copy'
        void this.taskScheduler.render().catch(() => {})
    }
}
