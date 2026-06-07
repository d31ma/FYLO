export default class extends Tac {
  installCmd = "bun add @d31ma/fylo"
  installCopied = false

  async copyInstall() {
    try {
      await navigator.clipboard.writeText(this.installCmd)
      this.installCopied = true
      setTimeout(() => { this.installCopied = false }, 2200)
    } catch (_) {}
  }
}
