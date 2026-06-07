export default class extends Tac {
  features = [
    {
      key: 'canonical-documents',
      title: 'Canonical Documents',
      kind: 'plain',
      text: 'Each document is stored as its own canonical JSON file, which keeps the durable model easy to inspect, debug, and rebuild from.',
    },
    {
      key: 'collection-index-files',
      title: 'Collection Index Files',
      kind: 'plain',
      text: 'FYLO builds a collection index file to narrow reads before hydrating matching documents, so queries stay fast without treating the index as the source of truth.',
    },
    {
      key: 'realtime-journal',
      title: 'Realtime Journal',
      kind: 'plain',
      text: 'Listeners are powered by an append-only filesystem event journal, which keeps document and query subscriptions live without a separate worker tier.',
    },
    {
      key: 'sql-nosql-apis',
      title: 'SQL + NoSQL APIs',
      kind: 'api',
    },
    {
      key: 'version-control',
      title: 'Document Version Control',
      kind: 'plain',
      text: 'Git-like branching, commits, diffs, and merges for your document store. Branch, commit, merge, and restore with the CLI or programmatic API.',
    },
    {
      key: 'browser-runtime',
      title: 'Browser Runtime',
      kind: 'plain',
      text: 'Full FYLO engine running in the browser via OPFS. SharedWorker and DedicatedWorker support with an in-process fallback for environments without workers.',
    },
    {
      key: 'http-gateway',
      title: 'HTTP Gateway',
      kind: 'plain',
      text: 'Embed a REST API server with Bearer token auth, CORS, and OpenAPI spec. Query via URL filters, run SQL over HTTP, and serve scoped clients.',
    },
    {
      key: 'sync-hooks',
      title: 'Sync Hooks',
      kind: 'sync',
    },
    {
      key: 'validation-encryption',
      title: 'Validation, Auth + Encryption',
      kind: 'plain',
      text: 'Schema-aware validation, app-supplied auth policies with row-level security, and AES-256-GCM field-level encryption with HMAC blind indexes.',
    },
  ]
}
