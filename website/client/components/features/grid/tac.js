import { CLIENT_COUNT } from '../../../shared/scripts/shims.js'

export default class extends Tac {
  // Exposed for template interpolation ({clientCount} in tac.html).
  clientCount = CLIENT_COUNT

  features = [
    {
      area: 'Storage',
      color: 'primary',
      title: 'Documents are truth',
      text: 'Each document is one canonical JSON file on disk, sharded by TTID prefix. Easy to inspect, debug, back up, and rebuild from.',
    },
    {
      area: 'Indexing',
      color: 'primary',
      title: 'Zero-payload prefix indexes',
      text: "S3-style key-only index entries in an mmap'd sorted catalog. Queries narrow by binary search, then hydrate only matching documents.",
    },
    {
      area: 'Query',
      color: 'success',
      title: 'SQL + NoSQL APIs',
      text: 'Query with a JSON operation protocol — put, find, patch, join — or plain SQL over the same engine. Exact, range, prefix, and trigram strategies.',
    },
    {
      area: 'Versioning',
      color: 'success',
      title: 'Git-like version control',
      text: 'Branch, commit, diff, merge, and restore your document store. Auto-commit on writes with content-addressed, deduplicated snapshots.',
    },
    {
      area: 'Distribution',
      color: 'warning',
      title: 'One self-contained binary',
      text: `Download a single executable — no runtime, no daemon, no native addons. Install once, then use drop-in clients for ${CLIENT_COUNT} languages — thin shims plus local-first browser and mobile.`,
    },
    {
      area: 'Security',
      color: 'error',
      title: 'Encryption, RLS & WORM',
      text: 'AES-GCM field encryption with HMAC blind indexes, app-supplied row-level security policies, and strict write-once WORM collections.',
    },
    {
      area: 'Replication',
      color: 'warning',
      title: 'Sync hooks & local queue',
      text: 'onWrite / onDelete hooks in await-sync or fire-and-forget mode, plus a durable local queue with consumer groups and dead-letter files.',
    },
    {
      area: 'Network',
      color: 'success',
      title: 'Remote HTTP gateway',
      text: 'A PostgREST-inspired REST boundary with Bearer auth, URL filters, branch profiles, SQL over HTTP, and an OpenAPI description.',
    },
    {
      area: 'Interop',
      color: 'primary',
      title: 'One protocol, many languages',
      text: 'A compiled executable speaks a JSON machine protocol tested against Python, Ruby, PHP, Dart, Java, C#, C++, Swift, Kotlin, and Rust.',
    },
  ]
}
