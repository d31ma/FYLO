import { CLIENT_COUNT } from '../../../shared/scripts/shims.js'

export default class extends Tac {
  // Fallback copy; refreshed from shared data through Tachyon's local-first fetch.
  stats = [
    { value: '1', label: 'canonical file per document' },
    { value: '0', label: 'native addons or external services' },
    { value: 'O(log n)', label: "mmap'd prefix index lookups" },
    { value: String(CLIENT_COUNT), label: 'language clients (shims + local-first)' },
  ]

  @onMount
  async refresh() {
    try {
      const response = await this.fetch('/shared/data/stats.json')
      const payload = await response.json()
      if (Array.isArray(payload) && payload.length) this.stats = payload
    } catch (_) {
      /* offline or static host without shared data — fallback stays */
    }
  }
}
