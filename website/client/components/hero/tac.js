export default class {
    hydrate(root) {
        this.root = root
    }
    installCmd = 'curl -fsSL https://fylo.del.ma/install.sh | sh'
    async copyInstall() {
        const status = this.root?.querySelector('.home-install-status')
        try {
            await navigator.clipboard.writeText(this.installCmd)
            if (status) status.textContent = 'Copied'
            setTimeout(() => {
                if (status) status.textContent = 'Copy'
            }, 2200)
        } catch (_) {
            if (status) status.textContent = 'Select to copy'
        }
        return this.installCmd
    }
}
