# Production Root Fixture

This directory documents the tracked `examples/db` fixture used by the
integration tests. It is shaped like a production FYLO root and contains small
mock `users` and `orders` collections.

```text
examples/db/
  schemas/
    users/
      manifest.json
      history/
        v1.schema.json
    orders/
      manifest.json
      history/
        v1.schema.json
    article/
      manifest.json
      history/
        v1.schema.json
        v2.schema.json
      upgraders/
        v1-to-v2.js
    rules.json
  .collections/
    users/
      docs/
        4U/
          4UUB32VGUDW.json
        4V/
          4V3M1R8K9CA.json
      events/
        users.ndjson
      index/
        manifest.json
        keys.snapshot
        keys.wal
      locks/
        collection.lock
        4UUB32VGUDW.lock
      heads/
        4UUB32VGUDW.json
      versions/
        4V3M1R8K9CA.meta.json
    orders/
      docs/
        4W/
          4WKVZ8NTG2P.json
      events/
        orders.ndjson
      index/
        manifest.json
        keys.snapshot
        keys.wal
      locks/
        collection.lock
```

## Directory Roles

| Path                                  | Purpose                                                         |
| ------------------------------------- | --------------------------------------------------------------- |
| `.collections/<collection>/docs/`     | Document JSON files, bucketed by the first two TTID characters. |
| `.collections/<collection>/events/`   | Append-only collection event journal.                           |
| `.collections/<collection>/index/`    | Local filesystem prefix index catalog.                          |
| `.collections/<collection>/locks/`    | Advisory document and collection write locks.                   |
| `.collections/<collection>/heads/`    | WORM lineage head pointers, present when WORM mode is enabled.  |
| `.collections/<collection>/versions/` | WORM version metadata, present when WORM mode is enabled.       |
| `schemas/<collection>/manifest.json`  | Collection schema version manifest used by validation and admin commands. |
| `schemas/<collection>/history/`       | Versioned CHEX schemas named `<version>.schema.json`.           |

Queue data, when enabled, is global to the FYLO root and remains outside
`.collections`:

```text
examples/db/
  .queue/
    topics/
    consumers/
    dlq/
```
