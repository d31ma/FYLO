export default class extends Tac {
  navGroups = [
    { title: 'Core', items: [
      { id: 'getting-started', label: 'Getting Started' }, { id: 'crud', label: 'Document CRUD' },
      { id: 'sql-querying', label: 'SQL Querying' }, { id: 'nosql-operators', label: 'NoSQL Operators' },
      { id: 'realtime-listeners', label: 'Realtime Listeners' }
    ]},
    { title: 'Replication & Security', items: [
      { id: 'sync-hooks', label: 'Sync Hooks' }, { id: 'schema-validation', label: 'Schema Validation' },
      { id: 'field-encryption', label: 'Field Encryption' }, { id: 'row-level-security', label: 'Row-Level Security' }
    ]},
    { title: 'Advanced', items: [
      { id: 'version-control', label: 'Version Control' }, { id: 'browser-runtime', label: 'Browser Runtime' },
      { id: 'http-gateway', label: 'HTTP Gateway' }, { id: 'worm-mode', label: 'WORM Mode' },
      { id: 'local-queue', label: 'Local Queue' }, { id: 'query-caching', label: 'Query Caching' }
    ]}
  ]
}
