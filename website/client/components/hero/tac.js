import { CLIENT_COUNT } from '../../shared/scripts/shims.js'

export default class extends Tac {
  installCmd = 'curl -fsSL https://fylo.del.ma/install.sh | sh'
  installCopied = false

  ticks = [
    'Filesystem-first',
    'Zero native addons',
    'One self-contained binary',
    `${CLIENT_COUNT} language clients included`,
  ]

  @publish('install-copied')
  async copyInstall() {
    try {
      await navigator.clipboard.writeText(this.installCmd)
      this.installCopied = true
      setTimeout(() => {
        this.installCopied = false
      }, 2200)
    } catch (_) {
      /* clipboard unavailable — hint text stays */
    }
    return this.installCmd
  }
}
