export default class extends Tac {
  navGroups = [
    { title: 'Getting Started', items: [
      { id: 'overview', label: 'Overview' }, { id: 'install', label: 'Installation' },
      { id: 'storage-model', label: 'Storage Model' }, { id: 'env-vars', label: 'Environment Variables' }
    ]},
    { title: 'API', items: [
      { id: 'nosql', label: 'NoSQL API' }, { id: 'sql', label: 'SQL API' },
      { id: 'operators', label: 'Query Operators' }, { id: 'realtime', label: 'Realtime Listeners' },
      { id: 'joins', label: 'Joins' }, { id: 'bulk', label: 'Bulk Import / Export' },
      { id: 'sync-hooks', label: 'Sync Hooks' }, { id: 'http-gateway', label: 'HTTP Gateway' },
      { id: 'cli', label: 'CLI' }
    ]},
    { title: 'Guides', items: [
      { id: 'schema', label: 'Schema Validation' }, { id: 'encryption', label: 'Field Encryption' },
      { id: 'authorization', label: 'Authorization' }, { id: 'security', label: 'Security' },
      { id: 'version-control', label: 'Version Control' }, { id: 'browser-runtime', label: 'Browser Runtime' },
      { id: 'worm-mode', label: 'WORM Mode' }, { id: 'query-caching', label: 'Query Caching' },
      { id: 'local-dev', label: 'Local Development' }
    ]}
  ]
}
