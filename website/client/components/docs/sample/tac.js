// Multi-language code samples for the documentation pages.
//
// The renderer below is copied from components/docs/content — Tachyon
// components cannot import shared modules, so the duplication is deliberate.
// Keep the two in sync when a client's method naming changes.
// Languages with a shipped client shim. `dir` is the folder under clients/,
// `cmt` the line-comment token used when we annotate a snippet.
const LANGS = [
    { key: 'python', label: 'Python', dir: 'python', cmt: '#' },
    { key: 'ruby', label: 'Ruby', dir: 'ruby', cmt: '#' },
    { key: 'node', label: 'Node.js', dir: 'node', cmt: '//' },
    { key: 'php', label: 'PHP', dir: 'php', cmt: '//' },
    { key: 'go', label: 'Go', dir: 'go', cmt: '//' },
    { key: 'rust', label: 'Rust', dir: 'rust', cmt: '//' },
    { key: 'csharp', label: 'C#', dir: 'csharp', cmt: '//' },
    { key: 'java', label: 'Java', dir: 'java', cmt: '//' },
    { key: 'swift', label: 'Swift (iOS)', dir: 'swift', cmt: '//', mobile: true },
    { key: 'kotlin', label: 'Kotlin (Android)', dir: 'kotlin', cmt: '//', mobile: true },
    { key: 'dart', label: 'Dart', dir: 'dart', cmt: '//' },
    { key: 'flutter', label: 'Flutter', dir: 'flutter', cmt: '//', mobile: true },
    { key: 'web', label: 'JS (Browser)', dir: 'web', cmt: '//' }
]

const FYLO_BROWSER_LOADER = 'https://d31ma.github.io/FYLO/version/26.33.02/fylo.js'

// Swift (iOS), Kotlin (Android), and Flutter are local-first mobile clients — they
// embed the engine in a WebView, on-device only, like the browser client.
const isMobile = (lang) => lang === 'swift' || lang === 'kotlin' || lang === 'flutter'

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
function rustJson(v) {
    if (typeof v === 'number') return `${v}.into()`
    if (typeof v === 'string') return `"${v}".into()`
    if (Array.isArray(v)) return `Json::arr(vec![${v.map(rustJson).join(', ')}])`
    return `Json::obj(vec![${Object.entries(v)
        .map(([k, val]) => `("${k}", ${rustJson(val)})`)
        .join(', ')}])`
}
function swiftLit(v) {
    // Swift uses \(…) for interpolation, so a literal `$` in a string needs no escaping.
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${v}"`
    if (Array.isArray(v)) return `[${v.map(swiftLit).join(', ')}]`
    return `[${Object.entries(v)
        .map(([k, val]) => `"${k}": ${swiftLit(val)}`)
        .join(', ')}]`
}
function kotlinLit(v) {
    const esc = (s) => s.replace(/\$/g, '\\$') // $ starts interpolation in Kotlin strings
    if (typeof v === 'number') return String(v)
    if (typeof v === 'string') return `"${esc(v)}"`
    if (Array.isArray(v)) return `listOf(${v.map(kotlinLit).join(', ')})`
    return `mapOf(${Object.entries(v)
        .map(([k, val]) => `"${esc(k)}" to ${kotlinLit(val)}`)
        .join(', ')})`
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
    if (lang === 'python' || lang === 'ruby' || lang === 'rust') {
        return op.replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase()
    }
    return op // node / php / java keep the camelCase op name
}

// Render one argument in the target language. Rust scalars are &str; its object
// args use the Json builder.
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
        case 'swift':
            return swiftLit(v)
        case 'kotlin':
            return kotlinLit(v)
        case 'dart':
        case 'flutter':
            return dartLit(v)
        case 'rust':
            return typeof v === 'string' ? `"${v}"` : rustJson(v)
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

// Go and Rust take the collection kind as a required argument; every other
// client defaults it to "document".
function withRequiredKind(lang, op) {
    const required = lang === 'go' || lang === 'rust'
    if (op.op !== 'createCollection' || op.kind !== undefined || !required) return op
    return { ...op, kind: 'document' }
}

// The browser client returns cursors for reads: `get(id)` resolves with
// `.once()`, and `find(query)` streams through `.collect()`.
function browserCursor(lang, op, invocation) {
    if (lang !== 'web') return invocation
    if (op === 'getDoc') return `${invocation}.once()`
    if (op === 'findDocs') return `Array.fromAsync(${invocation}.collect())`
    return invocation
}

// One collection-scoped facade call: `db.users.put(...)` in dynamic languages,
// `db.collection("users").put(...)` in the rest.
function call(lang, requested) {
    const op = withRequiredKind(lang, requested)
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
    const invocation = browserCursor(lang, op.op, `${receiver}${sep}${method}(${rest})`)
    switch (lang) {
        case 'node':
        case 'web':
            return `await ${invocation}`
        case 'dart':
        case 'flutter':
            return `await ${invocation};`
        case 'swift':
            return `try await ${invocation}` // async local-first mobile client
        case 'rust':
            return `${invocation}?;`
        case 'csharp':
        case 'java':
        case 'php':
            return `${invocation};`
        default:
            return invocation // python / ruby / go / kotlin
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
    rust: {
        open: ['use fylo::{Fylo, Json};', '', 'let mut db = Fylo::open("/mnt/fylo", "fylo")?;'],
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
    swift: {
        open: [
            '// Fylo.swift is compiled into the app target, so there is no module to import.',
            '',
            'let db = try await Fylo()'
        ],
        indent: '',
        close: []
    },
    kotlin: {
        open: [
            '// inside a coroutine (e.g. lifecycleScope.launch { … })',
            '',
            'val db = Fylo.open(context)'
        ],
        indent: '',
        close: []
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
    },
    flutter: {
        open: [
            "import 'fylo.dart';",
            '',
            '// in an async context (e.g. initState / an async method)',
            'final db = await Fylo.open();'
        ],
        indent: '',
        close: []
    },
    web: {
        open: [
            `<script src="${FYLO_BROWSER_LOADER}"></script>`,
            '<script type="module">',
            '  const db = await Fylo.open()'
        ],
        indent: '  ',
        close: ['</script>']
    }
}

// Child-process configuration is intentionally rendered separately from CRUD
// recipes: each native shim exposes an idiomatic options shape, while browser
// and mobile clients embed the portable engine and therefore have no child
// process configuration file to select.
const CONFIGURATION_SAMPLES = {
    python: `from fylo import Fylo

with Fylo(
    "/mnt/fylo",
    binary="/opt/fylo/bin/fylo",
    config="./config/application.json",
) as db:
    db.create_collection("users")`,
    ruby: `require_relative "fylo"

Fylo.open(
  "/mnt/fylo",
  binary: "/opt/fylo/bin/fylo",
  config: "./config/application.json"
) do |db|
  db.create_collection("users")
end`,
    node: `import { Fylo } from './fylo.mjs'

const db = new Fylo('/mnt/fylo', {
    binary: '/opt/fylo/bin/fylo',
    config: './config/application.json'
})`,
    php: `require 'fylo.php';

$db = new Fylo(
    '/mnt/fylo',
    '/opt/fylo/bin/fylo',
    './config/application.json'
);`,
    go: `import fylo "yourapp/fylo"

db, err := fylo.OpenWithOptions("/mnt/fylo", fylo.Options{
    Binary: "/opt/fylo/bin/fylo",
    Config: "./config/application.json",
})
if err != nil { return err }
defer db.Close()`,
    rust: `use fylo::{Fylo, FyloOptions};

let mut db = Fylo::open_with_options(
    "/mnt/fylo",
    FyloOptions {
        binary: "/opt/fylo/bin/fylo".into(),
        config: Some("./config/application.json".into()),
        ..Default::default()
    },
)?;`,
    csharp: `using var db = new Fylo.Fylo(
    "/mnt/fylo",
    new Fylo.Fylo.Options {
        Binary = "/opt/fylo/bin/fylo",
        Config = "./config/application.json"
    }
);`,
    java: `Fylo.Options options = new Fylo.Options();
options.binary = "/opt/fylo/bin/fylo";
options.config = "./config/application.json";

try (Fylo db = new Fylo("/mnt/fylo", options)) {
    db.createCollection("users");
}`,
    dart: `import 'fylo.dart';

Future<void> main() async {
  final db = await Fylo.open(
    '/mnt/fylo',
    binary: '/opt/fylo/bin/fylo',
    config: './config/application.json',
  );
}`,
    swift: `// Fylo.swift is compiled into the app target, so there is no module to import.
let db = try await Fylo()

// iOS embeds FYLO in a WebView; configure its on-device
// storage host directly. No native child process is spawned.`,
    kotlin: `val db = Fylo.open(context)

// Android embeds FYLO in a WebView; configure its on-device
// storage host directly. No native child process is spawned.`,
    flutter: `Future<void> configureFylo() async {
  final db = await Fylo.open();

  // Flutter embeds FYLO in a WebView; configure its on-device
  // storage host directly. No native child process is spawned.
}`,
    web: `<script src="${FYLO_BROWSER_LOADER}"></script>
<script type="module">
  const db = await Fylo.open({
    storage: 'opfs',
    worker: true,
    wasm: true
  })

  // Browser storage is configured directly; there is no child env.
</script>`
}

// `imports` are extra lines a recipe needs ahead of the language's own
// boilerplate, such as Go's "fmt" or the JSON decoder Rust and Java use to read
// the raw response envelope.
function scaffold(lang, bodyLines, imports = []) {
    const s = SCAFFOLD[lang]
    const body = bodyLines.map((l) => (l ? s.indent + l : l))
    return [...imports, ...s.open, '', ...body, ...s.close].join('\n')
}

// A recipe is an ordered list of steps. A step is either a comment line or one
// machine operation, which `call()` renders into the selected language.
const RECIPES = {
    'first-write': [
        { op: 'createCollection', collection: 'users' },
        { blank: true },
        { assign: 'id', op: 'putData', collection: 'users', data: { name: 'Ada', role: 'admin' } },
        { assign: 'doc', op: 'getLatest', collection: 'users', id: '<id>' }
    ],
    query: [
        { comment: 'Every field was indexed on write — nothing to declare first.' },
        {
            assign: 'admins',
            op: 'findDocs',
            collection: 'users',
            query: { $ops: [{ role: { $eq: 'admin' } }] }
        },
        { blank: true },
        { comment: 'The same question in SQL, against the same engine and indexes.' },
        { assign: 'rows', op: 'executeSQL', sql: "SELECT * FROM users WHERE role = 'admin'" }
    ],
    crud: [
        { op: 'createCollection', collection: 'users' },
        { blank: true },
        {
            assign: 'id',
            op: 'putData',
            collection: 'users',
            data: { name: 'Jane Doe', age: 29, team: 'platform' }
        },
        { assign: 'doc', op: 'getDoc', collection: 'users', id: '<id>' },
        { blank: true },
        { comment: 'patch preserves the document TTID — an update, not a replace.' },
        {
            op: 'patchDoc',
            collection: 'users',
            id: '<id>',
            newDoc: { team: 'core-platform' }
        },
        { op: 'delDoc', collection: 'users', id: '<id>' }
    ],
    restore: [
        { comment: 'A soft-deleted document keeps its TTID and can be brought back.' },
        { op: 'restoreDoc', collection: 'users', id: '<id>' }
    ],
    'find-ops': [
        { comment: 'Exact match' },
        {
            assign: 'exact',
            op: 'findDocs',
            collection: 'users',
            query: { $ops: [{ name: { $eq: 'Alice' } }] }
        },
        { blank: true },
        { comment: 'Range — numeric fields' },
        {
            assign: 'adults',
            op: 'findDocs',
            collection: 'users',
            query: { $ops: [{ age: { $gte: 18 } }] }
        },
        { blank: true },
        { comment: 'Array membership' },
        {
            assign: 'engineers',
            op: 'findDocs',
            collection: 'users',
            query: { $ops: [{ tags: { $contains: 'engineering' } }] }
        },
        { blank: true },
        { comment: 'OR — every entry in $ops is a separate branch' },
        {
            assign: 'privileged',
            op: 'findDocs',
            collection: 'users',
            query: { $ops: [{ role: { $eq: 'admin' } }, { role: { $eq: 'owner' } }] }
        }
    ],
    sql: [
        { assign: 'created', op: 'executeSQL', sql: 'CREATE TABLE posts' },
        {
            assign: 'inserted',
            op: 'executeSQL',
            sql: "INSERT INTO posts (title, published) VALUES ('Hello', true)"
        },
        { assign: 'posts', op: 'executeSQL', sql: 'SELECT * FROM posts WHERE published = true' },
        { blank: true },
        { comment: 'EXPLAIN reports the access path without executing the statement.' },
        {
            assign: 'plan',
            op: 'executeSQL',
            sql: "EXPLAIN SELECT * FROM posts WHERE title = 'Hello'"
        }
    ],
    rebuild: [
        { comment: 'Documents are truth; the index is derived and can always be rebuilt.' },
        { assign: 'result', op: 'rebuildCollection', collection: 'posts' },
        { assign: 'info', op: 'inspectCollection', collection: 'posts' }
    ],
    bucket: [
        { nativeOnly: true },
        { comment: 'A bucket is a collection whose values are bytes rather than records.' },
        { op: 'createCollection', collection: 'assets', kind: 'file' },
        { blank: true },
        { comment: 'Derived file metadata is indexed, so keys are queryable.' },
        {
            assign: 'reports',
            op: 'findDocs',
            collection: 'assets',
            query: { $ops: [{ key: { $like: '/reports/%' } }] }
        }
    ]
}

// Assignment syntax per language for a step that binds its result to a name.
function bind(lang, name) {
    switch (lang) {
        case 'python':
        case 'ruby':
            return `${name} = `
        case 'node':
        case 'web':
            return `const ${name} = `
        case 'php':
            return `$${name} = `
        case 'go':
            return `${name}, _ := `
        case 'rust':
            return `let ${name} = `
        case 'csharp':
            return `var ${name} = `
        case 'java':
            return `var ${name} = `
        case 'swift':
            return `let ${name} = `
        case 'kotlin':
            return `val ${name} = `
        case 'dart':
        case 'flutter':
            return `final ${name} = `
        default:
            return `${name} = `
    }
}

// `<id>` in a recipe means "the identifier the previous step produced", rendered
// as that language's variable reference rather than a string literal.
function idRef(lang) {
    if (lang === 'php') return '$id'
    if (lang === 'rust') return '&id'
    return 'id'
}

// Statement terminator / await form, matching `call()`'s tail.
function finish(lang, invocation) {
    switch (lang) {
        case 'node':
        case 'web':
            return `await ${invocation}`
        case 'dart':
        case 'flutter':
            return `await ${invocation};`
        case 'swift':
            return `try await ${invocation}`
        case 'rust':
            return `${invocation}?;`
        case 'csharp':
        case 'java':
        case 'php':
            return `${invocation};`
        default:
            return invocation
    }
}

// SQL statements quote with ' internally, so they are always emitted as a
// double-quoted literal — the single-quote languages would otherwise terminate
// the string early. `$` is escaped where the language interpolates it.
function sqlLit(lang, sql) {
    const interpolatesDollar =
        lang === 'php' || lang === 'kotlin' || lang === 'dart' || lang === 'flutter'
    let escaped = sql.replace(/\\/g, '\\\\').replace(/"/g, '\\"')
    if (interpolatesDollar) escaped = escaped.replace(/\$/g, '\\$')
    return `"${escaped}"`
}

// SQL is not collection-scoped — it is a top-level method on the client. The
// browser client uses its `sql` tagged template, which escapes interpolated
// values.
function sqlCall(lang, sql) {
    if (lang === 'web') return `await db.sql\`${sql}\``
    const receiver = lang === 'php' ? '$db->' : 'db.'
    return finish(lang, `${receiver}${methodName(lang, 'executeSQL')}(${sqlLit(lang, sql)})`)
}

// `const x = await db…()` — the binding goes in front of the whole expression,
// await included.
function applyBind(lang, name, line) {
    return bind(lang, name) + line
}

// `put` resolves with the new document id. Most clients return it as a string;
// the statically typed ones need a cast, and Rust and Java return the raw
// response envelope, so the id is read from its `result` field.
function putIdLines(lang, line) {
    const invocation = line.replace(/;$/, '')
    switch (lang) {
        case 'go':
            return [`idValue, _ := ${line}`, 'id := idValue.(string)']
        case 'rust':
            return [
                `let response = ${line}`,
                'let reply: serde_json::Value = serde_json::from_str(&response)?;',
                'let id = reply["result"].as_str().unwrap_or_default().to_owned();'
            ]
        case 'java':
            return [
                `String response = ${line}`,
                'String id = new ObjectMapper().readTree(response).get("result").asText();'
            ]
        case 'csharp':
            return [`string id = ${invocation}.GetString();`]
        case 'swift':
            return [`let id = ${line} as! String`]
        case 'kotlin':
            return [`val id = ${line} as String`]
        default:
            return [applyBind(lang, 'id', line)]
    }
}

// Each step renders to one or more lines.
function renderStep(lang, step) {
    if (step.blank) return ['']
    if (step.comment) {
        const cmt = LANGS.find((l) => l.key === lang)?.cmt || '//'
        return [`${cmt} ${step.comment}`]
    }
    if (step.op === 'executeSQL') {
        const line = sqlCall(lang, step.sql)
        return [step.assign ? applyBind(lang, step.assign, line) : line]
    }
    const resolved = { ...step }
    if (resolved.id === '<id>') resolved.id = { __ref: idRef(lang) }
    const line = call(lang, resolved)
    if (step.assign === 'id') return putIdLines(lang, line)
    return [step.assign ? applyBind(lang, step.assign, line) : line]
}

// Names a recipe binds to results it shows but never reads again. Go rejects
// unused variables, so its samples print them.
function shownResults(steps) {
    return steps.filter((step) => step.assign && step.assign !== 'id').map((s) => s.assign)
}

function recipeBody(lang, steps) {
    const lines = steps.filter((step) => !step.nativeOnly).flatMap((s) => renderStep(lang, s))
    const results = shownResults(steps)
    if (lang === 'go' && results.length > 0) lines.push('', `fmt.Println(${results.join(', ')})`)
    return lines
}

function recipeImports(lang, steps) {
    const readsId = steps.some((step) => step.assign === 'id')
    if (lang === 'go' && shownResults(steps).length > 0) return ['import "fmt"']
    if (lang === 'java' && readsId) {
        return [
            '// Reads the raw response envelope with Jackson (com.fasterxml.jackson).',
            'import com.fasterxml.jackson.databind.ObjectMapper;'
        ]
    }
    if (lang === 'rust' && readsId) {
        return ['// Reads the raw response envelope with serde_json (add it to Cargo.toml).']
    }
    return []
}

// Raw-file buckets need the native engine: the browser loader and the mobile
// WebView clients reject `kind: "file"` with an error.
function nativeOnlyNote(lang) {
    const cmt = LANGS.find((l) => l.key === lang)?.cmt || '//'
    return [
        `${cmt} Raw-file buckets (kind "file") need the native fylo binary.`,
        `${cmt} The browser loader and the iOS, Android and Flutter clients reject`,
        `${cmt} them with an error in this release. Use a binary-backed client`,
        `${cmt} (Python, Ruby, Node.js, PHP, Go, Rust, C#, Java or Dart) instead.`
    ].join('\n')
}

export default class {
    constructor(props = {}) {
        Object.assign(this, props)
    }

    hydrate(root) {
        this.root = root
        this.renderSelection()
    }
    /** @type {string} */
    topic = 'crud'

    /** @type {string} */
    $lang = 'python' // sessionStorage-persisted, shared by every sample on the page

    langs = LANGS

    showLang(_event, key) {
        this.$lang = key
        this.renderSelection()
    }

    renderSelection() {
        for (const button of this.root?.querySelectorAll('[data-lang]') ?? []) {
            const active = button.dataset.lang === this.$lang
            button.className = active
                ? 'w-btn w-btn-tonal w-btn--sm'
                : 'w-btn w-btn-ghost w-btn--sm'
            button.setAttribute('aria-pressed', String(active))
        }
    }

    activeLabel() {
        return (LANGS.find((l) => l.key === this.$lang) || LANGS[0]).label
    }

    code() {
        const lang = LANGS.find((l) => l.key === this.$lang) ? this.$lang : 'python'
        if (this.topic === 'configuration') return CONFIGURATION_SAMPLES[lang]
        const steps = RECIPES[this.topic] || RECIPES.crud
        const nativeOnly = steps.some((step) => step.nativeOnly)
        if (nativeOnly && (lang === 'web' || isMobile(lang))) return nativeOnlyNote(lang)
        return scaffold(lang, recipeBody(lang, steps), recipeImports(lang, steps))
    }
}
