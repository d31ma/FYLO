// Languages with a shipped client shim. `dir` is the folder under clients/,
// `cmt` the line-comment token used when we annotate a snippet.
const LANGS = [
    { key: 'python', label: 'Python', dir: 'python', cmt: '#' },
    { key: 'ruby', label: 'Ruby', dir: 'ruby', cmt: '#' },
    { key: 'node', label: 'Node.js', dir: 'node', cmt: '//' },
    { key: 'php', label: 'PHP', dir: 'php', cmt: '//' },
    { key: 'go', label: 'Go', dir: 'go', cmt: '//' },
    { key: 'csharp', label: 'C#', dir: 'csharp', cmt: '//' },
    { key: 'java', label: 'Java', dir: 'java', cmt: '//' },
    { key: 'dart', label: 'Dart', dir: 'dart', cmt: '//' },
    { key: 'web', label: 'JS (Browser)', dir: 'web', cmt: '//' }
]

const FYLO_BROWSER_LOADER = 'https://d31ma.github.io/FYLO/version/26.45.07/fylo.js'

// Native object/array literal renderers, one per language. Object arguments are
// built with each language's native container — no JSON strings.
function pyLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `[${v.map(pyLit).join(', ')}]`
    return `{${Object.entries(v)
        .map(([k, val]) => `"${k}": ${pyLit(val)}`)
        .join(', ')}}`
}
function rubyLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `[${v.map(rubyLit).join(', ')}]`
    return `{ ${Object.entries(v)
        .map(([k, val]) => `"${k}" => ${rubyLit(val)}`)
        .join(', ')} }`
}
function jsLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `'${v}'`
    if (Array.isArray(v)) return `[${v.map(jsLit).join(', ')}]`
    return `{ ${Object.entries(v)
        .map(([k, val]) => `${k}: ${jsLit(val)}`)
        .join(', ')} }`
}
function phpLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `'${v}'` // single quotes so $keys aren't interpolated
    if (Array.isArray(v)) return `[${v.map(phpLit).join(', ')}]`
    return `[${Object.entries(v)
        .map(([k, val]) => `'${k}' => ${phpLit(val)}`)
        .join(', ')}]`
}
function goLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `[]any{${v.map(goLit).join(', ')}}`
    return `map[string]any{${Object.entries(v)
        .map(([k, val]) => `"${k}": ${goLit(val)}`)
        .join(', ')}}`
}
function javaLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `List.of(${v.map(javaLit).join(', ')})`
    return `Map.of(${Object.entries(v)
        .flatMap(([k, val]) => [`"${k}"`, javaLit(val)])
        .join(', ')})`
}
function csharpLit(v) {
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `new object[] { ${v.map(csharpLit).join(', ')} }`
    return `new Dictionary<string, object> { ${Object.entries(v)
        .map(([k, val]) => `["${k}"] = ${csharpLit(val)}`)
        .join(', ')} }`
}
function dartLit(v) {
    const esc = (s) => s.replace(/\$/g, '\\$') // $ starts interpolation in Dart strings
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `'${esc(v)}'`
    if (Array.isArray(v)) return `[${v.map(dartLit).join(', ')}]`
    return `{${Object.entries(v)
        .map(([k, val]) => `'${esc(k)}': ${dartLit(val)}`)
        .join(', ')}}`
}

// Positional argument order for each op's dedicated method.
const METHODS = {
    createCollection: ['collection', 'kind'],
    putData: ['collection', 'data'],
    getDoc: ['collection', 'id'],
    getLatest: ['collection', 'id'],
    patchDoc: ['collection', 'id', 'newDoc'],
    delDoc: ['collection', 'id'],
    restoreDoc: ['collection', 'id'],
    findDocs: ['collection', 'query'],
    executeSQL: ['sql']
}

// The op name cased to each language's method convention.
function methodName(lang, op) {
    if (lang === 'go' || lang === 'csharp') return op.charAt(0).toUpperCase() + op.slice(1)
    if (lang === 'python' || lang === 'ruby') {
        return op.replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase()
    }
    return op // node / php / java keep the camelCase op name
}

// Render one argument in the target language.
function argLit(lang, v) {
    // A `__ref` value is an existing variable in the snippet, not a literal.
    if (v && typeof v === 'object' && !Array.isArray(v) && typeof v.__ref === 'string') {
        return v.__ref
    }
    switch (lang) {
        case 'python':
            return pyLit(v)
        case 'ruby':
            return rubyLit(v)
        case 'node':
        case 'web':
            return jsLit(v)
        case 'php':
            return phpLit(v)
        case 'go':
            return goLit(v)
        case 'java':
            return javaLit(v)
        case 'csharp':
            return csharpLit(v)
        case 'dart':
            return dartLit(v)
        default:
            return pyLit(v)
    }
}

// Short method name per op for the collection facade.
const SHORT = {
    createCollection: 'create',
    dropCollection: 'drop',
    inspectCollection: 'inspect',
    rebuildCollection: 'rebuild',
    putData: 'put',
    getDoc: 'get',
    getLatest: 'latest',
    patchDoc: 'patch',
    delDoc: 'delete',
    restoreDoc: 'restore',
    findDocs: 'find'
}
// Languages whose clients expose `db.<collection>` dynamic sugar.
const DYNAMIC = new Set(['node', 'web', 'python', 'ruby', 'php'])

// One collection-scoped facade call: `db.users.put(...)` in dynamic languages,
// `db.collection("users").put(...)` in the rest.
function call(lang, op) {
    let method = SHORT[op.op] || op.op
    if (lang === 'go' || lang === 'csharp')
        method = method.charAt(0).toUpperCase() + method.slice(1)
    const rest = (METHODS[op.op] || [])
        .filter((k) => k !== 'collection' && op[k] !== undefined)
        .map((k) => argLit(lang, op[k]))
        .join(', ')
    const accessor = lang === 'go' || lang === 'csharp' ? 'Collection' : 'collection'
    const receiver = DYNAMIC.has(lang)
        ? `${lang === 'php' ? '$db->' : 'db.'}${op.collection}`
        : `db.${accessor}(${argLit(lang, op.collection)})`
    const sep = lang === 'php' ? '->' : '.' // PHP method access
    const invocation = `${receiver}${sep}${method}(${rest})`
    switch (lang) {
        case 'node':
            return `await ${invocation}`
        case 'dart':
            return `await ${invocation};`
        case 'csharp':
        case 'java':
        case 'php':
            return `${invocation};`
        default:
            return invocation // python / ruby / go
    }
}

// Open/close boilerplate per language. Bodies are indented for languages that
// scope the connection in a block (Python `with`, Ruby block, Java try).
const SCAFFOLD = {
    python: {
        open: ['from fylo import Fylo', '', 'with Fylo("/mnt/fylo") as db:'],
        indent: '    ',
        close: []
    },
    ruby: {
        open: ['require_relative "fylo"', '', 'Fylo.open("/mnt/fylo") do |db|'],
        indent: '  ',
        close: ['end']
    },
    node: {
        open: ["import { Fylo } from './fylo.mjs'", '', "const db = new Fylo('/mnt/fylo')"],
        indent: '',
        close: []
    },
    php: {
        open: ["require 'fylo.php';", '', '$db = new Fylo("/mnt/fylo");'],
        indent: '',
        close: []
    },
    go: {
        open: [
            'import fylo "yourapp/fylo"',
            '',
            'db, _ := fylo.Open("/mnt/fylo", "fylo")',
            'defer db.Close()'
        ],
        indent: '',
        close: []
    },
    csharp: {
        open: [
            'using System.Collections.Generic;',
            '',
            'using var db = new Fylo.Fylo("/mnt/fylo");'
        ],
        indent: '',
        close: []
    },
    java: {
        open: [
            'import java.util.Map;',
            'import java.util.List;',
            '',
            'try (Fylo db = new Fylo("/mnt/fylo")) {'
        ],
        indent: '    ',
        close: ['}']
    },
    dart: {
        open: [
            "import 'fylo.dart';",
            '',
            'Future<void> main() async {',
            "  final db = await Fylo.open('/mnt/fylo');"
        ],
        indent: '  ',
        close: ['}']
    }
}

// `imports` are extra lines a snippet needs ahead of the language's own
// boilerplate, such as Go's "fmt".
function scaffold(lang, bodyLines, imports = []) {
    const s = SCAFFOLD[lang]
    const body = bodyLines.map((l) => (l ? s.indent + l : l))
    return [...imports, ...s.open, '', ...body, ...s.close].join('\n')
}

// The variable that holds the id `put` produced, as each language passes it.
function idRef(lang) {
    if (lang === 'php') return '$id'
    return 'id'
}

// `put` resolves with the new document id. Most clients return it as a string;
// the statically typed ones need a cast, and Java returns the raw
// response envelope, so the id is read from its `result` field.
function putIdLines(lang, line) {
    switch (lang) {
        case 'python':
        case 'ruby':
            return [`id = ${line}`]
        case 'php':
            return [`$id = ${line}`]
        case 'node':
            return [`const id = ${line}`]
        case 'dart':
            return [`final id = ${line}`]
        case 'go':
            return [`idValue, _ := ${line}`, 'id := idValue.(string)']
        case 'java':
            return [
                `String response = ${line}`,
                'String id = new ObjectMapper().readTree(response).get("result").asText();'
            ]
        case 'csharp':
            return [`string id = ${line.replace(/;$/, '')}.GetString();`]
        default:
            return [line]
    }
}

// Extra imports the id-reading lines above need.
function putIdImports(lang) {
    if (lang === 'java') {
        return [
            '// Reads the raw response envelope with Jackson (com.fasterxml.jackson).',
            'import com.fasterxml.jackson.databind.ObjectMapper;'
        ]
    }
    return []
}

export default class {
    hydrate() {}
    /** @type {string} */
    $section = 'install' // sessionStorage-persisted active section

    /** @type {string} */
    $lang = 'python' // sessionStorage-persisted active language

    langs = LANGS

    sections = [
        { key: 'install', label: 'Install', code: true },
        { key: 'crud', label: 'CRUD', code: true },
        { key: 'query', label: 'Querying', code: true },
        { key: 'sql', label: 'SQL', code: true },
        { key: 'schema', label: 'Schemas', code: false },
        { key: 'security', label: 'Security', code: false },
        { key: 'cli', label: 'CLI', code: false }
    ]

    queryStrategies = [
        { op: '$eq', index: 'Exact match key (eq)', fallback: '—' },
        {
            op: '$gt $gte $lt $lte',
            index: 'Sortable numeric key (n / nr)',
            fallback: 'Full scan if non-numeric'
        },
        { op: '$contains', index: 'Exact match on array members', fallback: '—' },
        { op: "$like 'ali%'", index: 'Forward prefix (f)', fallback: 'Full scan' },
        { op: "$like '%ice'", index: 'Reversed prefix (r)', fallback: 'Full scan' },
        { op: "$like '%lic%'", index: 'Trigram (g3) → hydrate → verify', fallback: 'Full scan' },
        {
            op: "$match 'meet tom'",
            index: 'Word keys (w) of a word field',
            fallback: 'Full scan if the field has no word index'
        },
        {
            op: "$sort { sentAt: 'desc' }",
            index: 'Walks sortable numeric keys (n / nr) in order',
            fallback: 'Documents without a number are left out'
        }
    ]

    show(key) {
        this.$section = key
    }

    showLang(key) {
        this.$lang = key
    }

    isCodeSection() {
        const section = this.sections.find((s) => s.key === this.$section)
        return section ? section.code : false
    }

    langMeta() {
        return LANGS.find((l) => l.key === this.$lang) || LANGS[0]
    }

    // ---- Language-aware code (install / crud / query / sql) ----

    installCode() {
        const lang = this.$lang
        if (lang === 'web') return this.webInstall()
        const { cmt, dir } = this.langMeta()
        return [
            `${cmt} 1. Install the fylo binary (macOS/Linux)`,
            'curl -fsSL https://fylo.del.ma/install.sh | sh',
            '',
            `${cmt} 2. Add clients/${dir}/ (one file, no external deps) from the`,
            `${cmt}    version-matched clients bundle attached to the latest release:`,
            `${cmt}    github.com/d31ma/Fylo/releases/latest/download/fylo-clients.tar.gz`,
            '',
            scaffold(lang, [
                `${cmt} Create a collection, then write your first document.`,
                call(lang, { op: 'createCollection', collection: 'users', kind: 'document' }),
                call(lang, {
                    op: 'putData',
                    collection: 'users',
                    data: { name: 'Ada', role: 'admin' }
                })
            ])
        ].join('\n')
    }

    crudCode() {
        const lang = this.$lang
        if (lang === 'web') return this.webCrud()
        const { cmt } = this.langMeta()
        const id = { __ref: idRef(lang) }
        const put = call(lang, {
            op: 'putData',
            collection: 'users',
            data: { name: 'Ada', role: 'admin' }
        })
        return scaffold(
            lang,
            [
                call(lang, { op: 'createCollection', collection: 'users', kind: 'document' }),
                '',
                `${cmt} Create — put returns the new document id.`,
                ...putIdLines(lang, put),
                '',
                `${cmt} Read the latest version, update in place (TTID preserved), then soft-delete.`,
                call(lang, { op: 'getLatest', collection: 'users', id }),
                call(lang, { op: 'patchDoc', collection: 'users', id, newDoc: { role: 'owner' } }),
                call(lang, { op: 'delDoc', collection: 'users', id }),
                `${cmt} ...and bring it back:`,
                call(lang, { op: 'restoreDoc', collection: 'users', id })
            ],
            putIdImports(lang)
        )
    }

    queryCode() {
        const lang = this.$lang
        if (lang === 'web') return this.webQuery()
        const { cmt } = this.langMeta()
        return scaffold(lang, [
            `${cmt} Prefix indexes narrow first; only matching documents are hydrated.`,
            `${cmt} Entries in $ops are OR'd; keys within an entry are AND'd.`,
            call(lang, {
                op: 'findDocs',
                collection: 'users',
                query: { $ops: [{ role: { $eq: 'admin' } }, { age: { $gte: 18 } }] }
            })
        ])
    }

    sqlCode() {
        const lang = this.$lang
        const { cmt } = this.langMeta()
        const body = this.sqlBody(lang, cmt)
        if (lang === 'web') return this.webScaffold(body)
        return scaffold(lang, body, lang === 'go' ? ['import "fmt"'] : [])
    }

    // Each language's native `sql` interpolation. Node/JS-browser (tagged
    // template) and C# (FormattableString) escape values for you; the rest
    // inline verbatim, so untrusted input must be escaped by the caller.
    sqlBody(lang, cmt) {
        switch (lang) {
            case 'node':
            case 'web':
                return [
                    `${cmt} Tagged template — interpolated values are escaped for you.`,
                    "const role = 'admin'",
                    'await db.sql`SELECT * FROM users WHERE role = ${role}`'
                ]
            case 'csharp':
                return [
                    `${cmt} Interpolated string ($"…") — values are escaped for you.`,
                    'var role = "admin";',
                    'db.Sql($"SELECT * FROM users WHERE role = {role}");'
                ]
            case 'python':
                return [
                    `${cmt} Native f-string — quote and escape untrusted values yourself.`,
                    'role = "admin"',
                    `db.sql(f"SELECT * FROM users WHERE role = '{role}'")`
                ]
            case 'ruby':
                return [
                    `${cmt} Native interpolation — quote and escape untrusted values yourself.`,
                    'role = "admin"',
                    `db.sql("SELECT * FROM users WHERE role = '#{role}'")`
                ]
            case 'php':
                return [
                    `${cmt} Native interpolation — quote and escape untrusted values yourself.`,
                    '$role = "admin";',
                    `$db->sql("SELECT * FROM users WHERE role = '$role'");`
                ]
            case 'go':
                return [
                    `${cmt} Sprintf — quote and escape untrusted values yourself.`,
                    'role := "admin"',
                    `db.Sql(fmt.Sprintf("SELECT * FROM users WHERE role = '%s'", role))`
                ]
            case 'java':
                return [
                    `${cmt} Concatenation — quote and escape untrusted values yourself.`,
                    'String role = "admin";',
                    `db.sql("SELECT * FROM users WHERE role = '" + role + "'");`
                ]
            case 'dart':
                return [
                    `${cmt} Native interpolation ($) — quote and escape untrusted values yourself.`,
                    "final role = 'admin';",
                    `await db.sql("SELECT * FROM users WHERE role = '\$role'");`
                ]
            default:
                return [
                    call(lang, { op: 'executeSQL', sql: 'SELECT * FROM users WHERE active = true' })
                ]
        }
    }

    // ---- JS (Browser): local OPFS/FSA storage, not a binary shim ----

    webScaffold(body) {
        return [
            `// Add once to <head>: <script src="${FYLO_BROWSER_LOADER}"></script>`,
            '',
            'const db = await Fylo.open({ wasm: true })',
            '',
            ...body
        ].join('\n')
    }

    webInstall() {
        return [
            '// The browser client is a bundled OPFS/FSA engine — no binary or backend.',
            '// The version-pinned loader and engine are published on GitHub Pages.',
            '',
            this.webScaffold([
                '// All reads and writes hit the browser-local OPFS store — fully offline.',
                'await db.users.create()',
                "const id = await db.users.put({ name: 'Ada', role: 'admin' })",
                'const doc = await db.users.latest(id)',
                '',
                '// Alternative: mount a user-selected FYLO root (Chromium). Call the picker',
                '// from a click handler so the browser can grant directory access.',
                "const handle = await showDirectoryPicker({ mode: 'readwrite' })",
                'const mounted = await Fylo.open({',
                "  storage: { type: 'fsa', handle, access: 'readwrite' },",
                '  worker: true,',
                '  wasm: true',
                '})'
            ])
        ].join('\n')
    }

    webCrud() {
        return this.webScaffold([
            '// Create the collection, then put — it returns the new document id.',
            'await db.users.create()',
            "const id = await db.users.put({ name: 'Ada', role: 'admin' })",
            '',
            '// Read latest, update in place (id preserved), soft-delete, then restore.',
            'await db.users.latest(id)',
            "await db.users.patch(id, { role: 'owner' })",
            'await db.users.delete(id)',
            'await db.users.restore(id)'
        ])
    }

    webQuery() {
        return this.webScaffold([
            "// Same query API as the server; entries in $ops are OR'd, keys within AND'd.",
            "const cursor = db.users.find({ $ops: [{ role: { $eq: 'admin' } }, { age: { $gte: 18 } }] })",
            'for await (const page of cursor.collect()) console.log(page)'
        ])
    }

    // ---- Language-neutral reference (schema / security / gateway) ----

    schemaCode() {
        return [
            '<fylo.schema>/users/',
            '  manifest.json          ← { "current": "v2", "versions": [...] }',
            '  history/',
            '    v1.schema.json       ← chex regex schema',
            '    v2.schema.json       ← head schema',
            '  migrations/',
            '    v1-to-v2.json        ← { "steps": [{ "rename": "fullName", "to": "name" }] }'
        ].join('\n')
    }

    securityCode() {
        return [
            '# Encryption is declared in the schema; access is granted per application identity.',
            '',
            '# 1. Encrypt fields: list them in the collection schema.',
            '#    <fylo.schema>/users/history/v1.schema.json',
            '{ "$encrypted": ["email", "ssn"], "name": "^.+$", "email": "^.+@.+$" }',
            '',
            '# 2. Create identities and a protected database.',
            'fylo identity create --store file:admin.pem',
            'fylo identity create --store file:app.pem',
            'fylo create app.vdbx --admin file:admin.pem   # prints the trust anchor, fid1:...',
            'fylo identity show file:app.pem --json        # prints the public key, fpk1:...',
            '',
            '# 3. Grant the application identity read access to one collection.',
            'fylo access grant app.vdbx --grantee web --key fpk1:... --actions read --collection users \\',
            '    --identity file:admin.pem --trust-anchor fid1:...',
            '',
            '# 4. Open the database as that identity, pinned to the admin that created it.',
            'fylo exec --loop --root app.vdbx --identity file:app.pem --trust-anchor fid1:...',
            '# Shims read fylo.identity and fylo.trustAnchor from application.json,',
            '# or FYLO_IDENTITY and FYLO_TRUST_ANCHOR from the environment.'
        ].join('\n')
    }

    cliCode() {
        return [
            '# Query and admin from the shell',
            'fylo "SELECT * FROM posts WHERE published = true"',
            'fylo inspect posts --root /mnt/fylo --json',
            'fylo rebuild posts --root /mnt/fylo',
            '',
            '# Git-like document version control',
            'fylo checkout -b feature/docs --root /mnt/fylo',
            'fylo commit -m "snapshot feature docs" --root /mnt/fylo',
            'fylo diff --root /mnt/fylo',
            'fylo merge feature/docs -m "merge feature docs" --root /mnt/fylo'
        ].join('\n')
    }

    // Code shown in the active language-aware section's <pre>.
    code() {
        switch (this.$section) {
            case 'crud':
                return this.crudCode()
            case 'query':
                return this.queryCode()
            case 'sql':
                return this.sqlCode()
            default:
                return this.installCode()
        }
    }
}
