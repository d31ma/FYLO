# FYLO Storage Contract v2 (DRAFT)

**Draft. Not built, not released.** This contract describes the on-disk layout
of [ADR 0033](../../../docs/adr/0033-primary-layout-on-a-data-drive.md) and the
`.vdbx` large objects of
[ADR 0034](../../../docs/adr/0034-large-objects-in-vdbx.md). Both ADRs are
"Accepted, not yet built". The current release writes
[v1](../v1/README.md). Slices L1-2 to L1-10 may still change details here; a
change after the layout release needs a new ADR.

## Canonical identifiers

| Boundary                 | Identifier                       | Schema                                                                  |
| ------------------------ | -------------------------------- | ----------------------------------------------------------------------- |
| App folder marker        | `fylo.layout.v2`                 | [format.schema.json](format.schema.json)                                |
| Namespace lease record   | `fylo.namespace-lease.v1`        | [namespace-lease.schema.json](namespace-lease.schema.json)              |
| Namespace marker         | `fylo.namespace.v1`              | [namespace.schema.json](namespace.schema.json)                          |
| Collection descriptor    | `fylo.collection-descriptor.v2`  | [collection-descriptor.schema.json](collection-descriptor.schema.json)  |
| Document body            | `fylo.document.json.v1`          | unchanged from v1                                                       |
| Prefix index             | `fylo.local-fs.index.v1`         | unchanged from v1, new place                                            |
| Transaction journal      | `fylo.collection-transaction.v1` | unchanged from v1, new place                                            |
| Generation state         | `fylo.collection-generation.v1`  | unchanged from v1, new place                                            |

Document bodies do not change. What changes is where every file lives, who
owns a namespace, and how metadata is stored.

## Layout

```text
<drive>/                                   mount point; not the boot disk
  <app>/                                   the root; its folder name is the app name
    .fylo/format.json                      fylo.layout.v2
    .fylo/namespaces.lock                  held only while a namespace is created or dropped
    .fylo/trash/                           dropped namespaces, deleted after the drop
    .collections .buckets .fylo-catalog    empty plain files (guards against old binaries)
    .<ns>.fylo-root-owner.lock             kernel lock file of namespace <ns>
    .<ns>.fylo-root-owner.lock.json        lease record of namespace <ns>
    .<ns>.fylo-root-owner.claim.<g>        short-lived claim of generation <g> (exclusive create)
    .<ns>.fylo-root-turn.lock              fair-wait turn lock of namespace <ns>
    <ns>/
      .fylo/namespace.json                 fylo.namespace.v1
      .fylo/catalog/<collection>.json      fylo.collection-descriptor.v2
      .fylo/transactions/                  structural journal
      .fylo/schemas/ .fylo/queue/ .fylo/vcs/
      .collections .buckets .fylo-catalog  the same guard files
      <collection>/
        <TTID>.json                        document
        <TTID>.<ext>                       file; <TTID>.bin for json, no or unsafe extension
        .<TTID>.<ext>.fylo-attrs           metadata, only when format.json says "sidecar"
        .fylo/index/ .fylo/wal/ .fylo/deleted/ .fylo/locks/
        .fylo/segments/ .fylo/state.json .fylo/events/
        <S>/<TTID>.json, <S>/<TTID>.<ext>  when shardWidth > 0
    <ns>.vdbx                              a namespace kept as one file (never with a folder <ns>/)
```

The exact names inside `<collection>/.fylo/` and `<ns>/.fylo/` are fixed by
slices L1-5 and L1-8. The rules that do not change: nothing derived lives in
the visible tree, and no name of an app, namespace or collection starts with
`.`.

## Names

App, namespace and collection names: lowercase `a-z`, digits `0-9` and `-`;
2 to 64 bytes; first and last character a letter or a digit; Windows device
names (`con`, `prn`, `aux`, `nul`, `com1`-`com9`, `lpt1`-`lpt9`) and FYLO's
reserved words refused. The schemas carry the character rule as a pattern;
the device names and reserved words are checked by the engine.

## Metadata

`format.json` records the probe result. With `"xattr"`, metadata is in
extended attributes (Windows: NTFS alternate data streams), as in v1. With
`"sidecar"`, every payload has a hidden `.<payload>.fylo-attrs` file in the
same folder, in v1's sidecar map format (ADR 0010). The `user.fylo.record-identity`
binding replaces v1's `.collections`/`.buckets` field with the namespace name
and drops the kind. Whether the sidecar format keeps ADR 0010's root
`uid`/`gid` capture is decided in slice L1-4.

## Ownership

See ADR 0033 section 4. A process owns namespace `<ns>` only while it holds
the kernel lock on `.<ns>.fylo-root-owner.lock` and the lease record names its
`ownerId` and `generation`. It checks this before every commit
(`EROOTLEASELOST` otherwise). Heartbeat every 5 s, takeover after 20 s of no
change, measured on the taker's own clock.

## `.vdbx` large objects

See ADR 0034. A `.vdbx` that holds files sets the incompatible superblock flag
`large-objects`. An object record adds `length`, `sha256`, `chunkSize`,
`runs` (`[first extent, count]` pairs, at most 4,096) and, in an encrypted
file, a 32-byte `salt`. Object extents carry the new extent kind `object`.
The byte layout of the chunk frame header is fixed by slice L1-9a.

## Compatibility

- v1 roots (a folder with a `.collections/` folder) open **read-only** in the
  layout release; writes fail with `ELEGACYLAYOUT`. `fylo upgrade-root` copies
  a v1 root into a v2 namespace.
- Older binaries fail on a v2 app folder or namespace folder, because
  `.collections` is a plain file there.
- Any further writer format change needs an ADR, a migration plan,
  interrupted-upgrade tests, a rollback policy and golden fixtures, as for v1.
