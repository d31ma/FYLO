export default class extends Tac {
  copySnippet(id) {
    const el = document.getElementById(id)
    if (el) navigator.clipboard.writeText(el.innerText).catch(() => {})
  }
}
