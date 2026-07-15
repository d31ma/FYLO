export default class extends Tac {
  /** @type {string} */
  openKey = 'rebuild'

  toggle(key) {
    this.openKey = this.openKey === key ? '' : key
  }

  faqs = [
    {
      key: 'rebuild',
      q: 'What happens if an index gets corrupted?',
      a: 'Nothing is lost. Documents are the source of truth and indexes are derived accelerators — the rebuild operation reconstructs every index entry by scanning the canonical document files.',
    },
    {
      key: 'languages',
      q: 'Which languages can I use FYLO from?',
      a: 'Any language that can spawn a process. FYLO ships as a single binary that speaks a JSON machine protocol over stdin/stdout; drop-in shims are provided for Python, Ruby, Node/TypeScript, PHP, Go, Rust, C#, Java, and Dart, and the protocol is tested in CI against even more. For platforms that can\'t spawn the binary there are local-only clients that embed the engine on-device — the browser bundle, native iOS (Swift) and Android (Kotlin) clients, and a Flutter client.',
    },
    {
      key: 'explorer',
      q: 'Can I browse a FYLO database visually?',
      a: "Yes — Fylo Explorer is a browser UI over a real FYLO root on your disk, opened through the File System Access API. Pick the folder once and browse collections, inspect documents, and filter with SQL WHERE expressions (role = 'admin' AND age >= 30). It is read-only by default — the engine rebuilds indexes into memory, never touching the folder — with opt-in writes that go through the engine. Chromium-only, since Firefox and Safari do not implement real-folder access.",
    },
    {
      key: 'replication',
      q: 'How do I replicate to S3 or GCS?',
      a: 'FYLO owns local storage and querying; you own how the root reaches remote storage. onWrite and onDelete sync hooks notify your storage client in await-sync or fire-and-forget mode, and the s3-client index backend can store index keys as zero-byte S3 objects.',
    },
    {
      key: 'transactions',
      q: 'Are there transactions?',
      a: 'Writes are serialized per collection with advisory file locks. There are no cross-collection atomic commits — declare related objects as their own collections and join them at query time with joinDocs.',
    },
    {
      key: 'encryption',
      q: 'Is my data encrypted at rest?',
      a: 'Fields listed in a schema’s $encrypted array are stored with AES-GCM. Equality lookups use HMAC blind indexes, so queries work without decrypting — with the documented trade-off that value repetition counts are observable.',
    },
  ]
}
