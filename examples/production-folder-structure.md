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
        4V/
          4V6329YC0F2.json
          4V6329YC0PG.json
      .deleted/
      events/
        users.ndjson
      index/
        manifest.json
        keys.snapshot
        keys.wal
      locks/
        .gitkeep
    orders/
      docs/
        4V/
          4V6329YC0R0.json
          4V6329YC0RG.json
      .deleted/
      events/
        orders.ndjson
      index/
        manifest.json
        keys.snapshot
        keys.wal
      locks/
        .gitkeep
    article/
      docs/
      .deleted/
      index/
        manifest.json
        keys.snapshot
        keys.wal
```

## Directory Roles

| Path                                  | Purpose                                                                                |
| ------------------------------------- | -------------------------------------------------------------------------------------- |
| `.collections/<collection>/docs/`     | Document JSON files, bucketed by the first two TTID characters.                        |
| `.collections/<collection>/.deleted/` | Hidden read-only tombstones; original TTID filename and deletion time as file `mtime`. |
| `.collections/<collection>/events/`   | Append-only collection event journal.                                                  |
| `.collections/<collection>/index/`    | Local filesystem prefix index catalog.                                                 |
| `.collections/<collection>/locks/`    | Advisory document and collection write locks.                                          |
| `schemas/<collection>/manifest.json`  | Collection schema version manifest used by validation and admin commands.              |
| `schemas/<collection>/history/`       | Versioned CHEX schemas named `<version>.schema.json`.                                  |

Queue data, when enabled, is global to the FYLO root and remains outside
`.collections`:

```text
examples/db/
  .queue/
    topics/
    consumers/
    dlq/
```
