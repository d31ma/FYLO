const TITLE = 'Schemas & migrations — FYLO'
document.title = TITLE

export default class {
    layoutCode = `<fylo.schema>/
  <collection>/
    manifest.json          # { current, versions: [{ v, sha256?, addedAt? }] }
    history/
      v1.schema.json       # chex regex schema
      v2.schema.json       # head is whichever manifest.current points at
    migrations/
      v1-to-v2.json        # { "steps": [ ... ] }, run when moving v1 → v2`

    manifestCode = `{
    "current": "v2",
    "versions": [
        { "v": "v1", "addedAt": "2026-04-01T00:00:00Z" },
        { "v": "v2", "addedAt": "2026-04-27T00:00:00Z" }
    ]
}`

    schemaCode = `{
    "id": "^[0-9]+$",
    "title": "^.+$",
    "body": "^.+$",
    "slug": "^[a-z0-9-]+$"
}`

    migrationCode = `{
    "steps": [{ "slugify": "slug", "from": "title" }]
}`

    stepsCode = `{ "rename": "a", "to": "b" }                 # move a field
{ "copy": "a", "to": "b" }                   # copy a field
{ "default": "a", "value": 0 }               # set only if missing or null
{ "set": "a", "value": 0 }                   # set unconditionally
{ "remove": "a" }                            # delete a field
{ "convert": "a", "to": "number" }           # "string", "number" or "boolean"
{ "map": "a", "values": { "y": true }, "else": false }
{ "wrap": "a" }                              # non-array value → [value]
{ "lower": "a" }, { "upper": "a" }           # Unicode full case mapping
{ "trim": "a" }                              # strip surrounding whitespace
{ "replace": "a", "find": "x", "with": "y" } # literal text, no regex
{ "template": "a", "value": "{first} {name/last}" }  # {{ and }} are literal braces
{ "slugify": "a", "from": "title" }          # a-z0-9 runs joined by -, or "untitled"`

    cliCode = `fylo schema inspect article --schema-dir ./schemas --json
fylo schema doctor  article --schema-dir ./schemas
fylo schema validate article @article.json --schema-dir ./schemas --json`
}
