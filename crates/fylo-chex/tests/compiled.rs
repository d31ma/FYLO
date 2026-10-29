//! `CompiledSchema`: DFA leaves must match exactly what `regex` matches, the
//! compiled validator must report exactly what the source validator reports,
//! and the serialized form must round-trip and refuse anything else.

use fylo_chex::{CompiledSchema, validate_definition, validate_object};
use serde_json::{Map, Value, json};

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected a JSON object, got {other}"),
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// The validator as it ran before compiled schemas: definition check, then a
/// walk over the source.
fn source_validate(schema: &Value, data: &Map<String, Value>, label: &str) -> Result<(), String> {
    validate_definition(schema, label).map_err(|e| format!("{}: {}", e.name, e.message))?;
    let Value::Object(schema) = schema else {
        unreachable!("the definition check refuses a non-object schema")
    };
    validate_object(schema, data, label).map_err(|e| format!("{}: {}", e.name, e.message))
}

fn compiled_validate(compiled: &CompiledSchema, data: &Map<String, Value>) -> Result<(), String> {
    compiled
        .validate(data)
        .map_err(|e| format!("{}: {}", e.name, e.message))
}

fn round_trip(compiled: &CompiledSchema) -> CompiledSchema {
    CompiledSchema::from_bytes(&compiled.to_bytes())
        .unwrap_or_else(|error| panic!("round trip failed: {}", error.message))
}

// ---------------------------------------------------------------------------
// DFA leaves against `regex`
// ---------------------------------------------------------------------------

const PATTERNS: &[&str] = &[
    // ASCII
    "^[0-9]+$",
    "^[a-z0-9-]+$",
    "abc",
    "^a|b$",
    "^(foo|bar)baz",
    "x*",
    "^$",
    "^$|^x$",
    "^.+$",
    ".",
    "^.{3}$",
    "^-?[0-9]+(\\.[0-9]+)?$",
    "^(true|false)$",
    "^ORD-[0-9]+$",
    "a{2,4}",
    "(?U)a+b",
    "(?x) a b c # comment",
    "[[:alpha:]]+",
    "^[^a-z]*$",
    "^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$",
    "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(\\.\\d+)?Z$",
    "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
    // Perl classes, which are Unicode by default
    "^\\w+$",
    "\\w",
    "^\\d+$",
    "\\d",
    "^\\D+$",
    "\\s",
    "^\\S+$",
    "^\\W$",
    "(?-u:\\w)+",
    "^(?-u:\\d)+$",
    // Case folding
    "(?i)hello",
    "(?i)^straße$",
    "(?i)^k$",
    "(?i)[a-z]+",
    "(?i)É",
    // Non-ASCII literals and ranges
    "é",
    "[α-ω]",
    "^[\\u{1F600}-\\u{1F64F}]$",
    "^\\x{0}$",
    // Anchors, flags, and line handling
    "(?s)^.$",
    "(?m)^b$",
    "(?m)$",
    "(?Rm)^a$",
    "\\Aabc\\z",
    // ASCII word boundaries have a DFA form
    "(?-u:\\b)foo(?-u:\\b)",
    "(?-u:\\B)",
    "(?-u:\\B)x",
    "^(?-u:\\b)",
];

/// Unicode word boundaries have no DFA form: these compile natively with
/// `regex` and make the schema non-portable.
const NON_PORTABLE: &[&str] = &["\\bword\\b", "\\B", "^\\b\\w+\\b$", "(?i)\\bfoo"];

fn haystacks() -> Vec<String> {
    let mut list: Vec<String> = [
        "",
        "a",
        "b",
        "x",
        "aa",
        "aab",
        "abc",
        "ABC",
        "Abc",
        "foobaz",
        "barbaz",
        "foo",
        "foo bar",
        "a foo b",
        "foobar",
        "word",
        "a word.",
        "swordfish",
        "héllo",
        "hello",
        "HELLO",
        "straße",
        "STRASSE",
        "STRAẞE",
        "ſ",
        "k",
        "K",
        "\u{212A}",
        "123",
        "-1.5",
        "1.",
        "١٢٣",
        "12a",
        " ",
        "\t",
        "\u{85}",
        "\u{2028}",
        "\u{a0}",
        "a\nb",
        "a\r\nb",
        "b\n",
        "\n",
        "é",
        "É",
        "e\u{301}",
        "aé",
        "éa",
        "αβγ",
        "Ω",
        "😀",
        "a😀b",
        "\0",
        "true",
        "false",
        "null",
        "x@y.io",
        "x@@y.io",
        "2026-10-29",
        "2026-10-29T12:00:00Z",
        "2026-10-29T12:00:00.5Z",
        "٢٠٢٦-١٠-٢٩T١٢:٠٠:٠٠Z",
        "ORD-42",
        "ORD-",
        "[object Object]",
        "1,,2",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    list.push("a".repeat(1000));
    list.push("é".repeat(300));
    list.push(format!("{}b", "a".repeat(200)));
    list
}

fn leaf_schema(pattern: &str) -> Value {
    json!({ "f": pattern })
}

fn leaf_matches(compiled: &CompiledSchema, haystack: &str) -> bool {
    compiled.validate(&object(json!({ "f": haystack }))).is_ok()
}

#[test]
fn dfa_leaves_match_exactly_what_regex_matches() {
    let haystacks = haystacks();
    let mut checked = 0;
    for &pattern in PATTERNS.iter().chain(NON_PORTABLE) {
        let regex = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
        let compiled = CompiledSchema::compile(&leaf_schema(pattern), "differential")
            .unwrap_or_else(|e| panic!("{pattern}: {}", e.message));
        let expect_portable = !NON_PORTABLE.contains(&pattern);
        assert_eq!(compiled.is_portable(), expect_portable, "{pattern}");
        let loaded = expect_portable.then(|| round_trip(&compiled));

        for haystack in &haystacks {
            let expected = regex.is_match(haystack);
            assert_eq!(
                leaf_matches(&compiled, haystack),
                expected,
                "compiled {pattern:?} on {haystack:?}"
            );
            if let Some(loaded) = &loaded {
                assert_eq!(
                    leaf_matches(loaded, haystack),
                    expected,
                    "loaded {pattern:?} on {haystack:?}"
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 3000, "only {checked} pairs checked");
}

#[test]
fn bad_patterns_report_as_they_always_have() {
    for pattern in ["(", "[a-", "\\p{Greek}", "a{2,1}"] {
        let schema = leaf_schema(pattern);
        let compiled = CompiledSchema::compile(&schema, "bad").expect_err(pattern);
        let defined = validate_definition(&schema, "bad").expect_err(pattern);
        assert_eq!(compiled.name, "ValidationError");
        assert_eq!(compiled.message, defined.message);
        assert_eq!(
            compiled.message,
            "Invalid RegEx pattern for 'f' in schema 'bad'"
        );
    }
    let long = leaf_schema(&"a".repeat(501));
    let error = CompiledSchema::compile(&long, "bad").expect_err("over-long pattern");
    assert_eq!(
        error.message,
        "Regex pattern for 'f' in schema 'bad' exceeds maximum allowed length"
    );
}

#[test]
fn a_pattern_whose_dfa_is_too_large_falls_back_to_regex() {
    for pattern in ["a{99999}", "^\\w{200}$"] {
        let started = std::time::Instant::now();
        let compiled = CompiledSchema::compile(&leaf_schema(pattern), "big").expect(pattern);
        println!(
            "{pattern}: gave up on the DFA after {:?}",
            started.elapsed()
        );
        assert!(!compiled.is_portable(), "{pattern}");
        let regex = regex::Regex::new(pattern).expect(pattern);
        // Short haystacks: without a DFA, `regex` runs its PikeVM, which is
        // slow on a pattern this large.
        for haystack in ["", "x", "aaa", "ééé"] {
            assert_eq!(leaf_matches(&compiled, haystack), regex.is_match(haystack));
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled validation against source validation
// ---------------------------------------------------------------------------

/// Every way to perturb one property of `data`.
fn mutations(data: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let replacements = [
        Value::Null,
        json!("zz"),
        json!("Jane Doe"),
        json!(7),
        json!(1.5),
        json!(true),
        json!([]),
        json!(["a", 1]),
        json!([{ "sku": "A-1", "quantity": 0 }]),
        json!({}),
        json!({ "city": "Paris" }),
        json!({ "Bad Key": "x" }),
    ];
    let mut out = vec![data.clone()];
    for key in data.keys() {
        let mut removed = data.clone();
        removed.shift_remove(key);
        out.push(removed);
        for replacement in &replacements {
            let mut changed = data.clone();
            changed.insert(key.clone(), replacement.clone());
            out.push(changed);
        }
    }
    let mut extra = data.clone();
    extra.insert("unexpected".into(), json!("x"));
    out.push(extra);
    let mut directive = data.clone();
    directive.insert("$encrypted".into(), json!(["name"]));
    out.push(directive);
    out
}

#[test]
fn compiled_schemas_report_exactly_what_the_source_reports() {
    let mut cases: Vec<(Value, Value)> = ["person", "order", "measure", "status"]
        .iter()
        .map(|name| {
            (
                fixture(&format!("valid/{name}.schema.json")),
                fixture(&format!("valid/{name}.data.json")),
            )
        })
        .collect();
    cases.push((
        json!({
            "$encrypted": ["email"],
            "email": "^[^@]+@[^@]+$",
            "name?": "^\\w+$",
            "tags": [{ "label": "^[a-z]+$", "weight?": "^\\d+$", "^odd$": "^x$" }],
            "labels": { "^[a-z]+$": "^\\w+$" },
            "deep": { "inner": { "leaf": "\\bok\\b" } }
        }),
        json!({
            "email": "a@b",
            "name": "héllo",
            "tags": [{ "label": "x", "^odd$": "x" }, { "label": "y", "weight": 2, "^odd$": "x" }],
            "labels": { "en": "Hello", "fr": "Bonjour" },
            "deep": { "inner": { "leaf": "it is ok here" } }
        }),
    ));

    let mut compared = 0;
    for (schema, data) in &cases {
        let compiled = CompiledSchema::compile(schema, "parity").expect("fixture compiles");
        let loaded = compiled.is_portable().then(|| round_trip(&compiled));
        let documents = mutations(&object(data.clone()));
        for document in &documents {
            let expected = source_validate(schema, document, "parity");
            assert_eq!(
                compiled_validate(&compiled, document),
                expected,
                "{document:?}"
            );
            let inline = fylo_chex::validate_inline(schema, document, "parity")
                .map_err(|e| format!("{}: {}", e.name, e.message));
            assert_eq!(inline, expected, "{document:?}");
            if let Some(loaded) = &loaded {
                assert_eq!(
                    compiled_validate(loaded, document),
                    expected,
                    "{document:?}"
                );
            }
            compared += 1;
        }
        // Nested documents get the same treatment one level down.
        for (key, value) in object(data.clone()) {
            let Value::Object(nested) = value else {
                continue;
            };
            for changed in mutations(&nested) {
                let mut document = object(data.clone());
                document.insert(key.clone(), Value::Object(changed));
                let expected = source_validate(schema, &document, "parity");
                assert_eq!(
                    compiled_validate(&compiled, &document),
                    expected,
                    "{document:?}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 200, "only {compared} documents compared");
}

#[test]
fn malformed_schemas_report_exactly_what_the_definition_check_reports() {
    let names = [
        "array-object-empty",
        "empty-array-pattern",
        "empty-object",
        "empty-pattern",
        "invalid-regex-pattern",
        "multiple-array-templates",
        "non-string-leaf",
    ];
    let mut schemas: Vec<Value> = names
        .iter()
        .map(|name| fixture(&format!("invalid/{name}.schema.json")))
        .collect();
    schemas.extend([
        json!([]),
        json!("^x$"),
        json!({ "a": { "^k$": 1 } }),
        json!({ "a": { "^k$": "" } }),
        json!({ "a": { "^(": "x" } }),
        json!({ "a": [1] }),
        json!({ "a": [{}] }),
        json!({ "a": null }),
        json!({ "a": "^x$", "b": "(" , "c": "" }),
    ]);
    for schema in &schemas {
        let expected = validate_definition(schema, "malformed").expect_err("malformed");
        let error = CompiledSchema::compile(schema, "malformed").expect_err("malformed");
        assert_eq!(
            (error.name, &error.message),
            (expected.name, &expected.message)
        );
        let error =
            fylo_chex::validate_inline(schema, &Map::new(), "malformed").expect_err("malformed");
        assert_eq!(
            (error.name, &error.message),
            (expected.name, &expected.message)
        );
    }
}

#[test]
fn repeated_patterns_share_one_leaf_and_one_stored_dfa() {
    let one = CompiledSchema::compile(&json!({ "a": "^\\w+$" }), "s").expect("compiles");
    let two =
        CompiledSchema::compile(&json!({ "a": "^\\w+$", "b": ["^\\w+$"] }), "s").expect("compiles");
    let difference = two.to_bytes().len() - one.to_bytes().len();
    assert!(
        difference < 64,
        "the second leaf stored {difference} more bytes"
    );
}

// ---------------------------------------------------------------------------
// Serialized form
// ---------------------------------------------------------------------------

fn sample() -> CompiledSchema {
    CompiledSchema::compile(&fixture("valid/person.schema.json"), "person").expect("compiles")
}

#[test]
fn round_trip_keeps_the_label_the_source_and_the_bytes() {
    let compiled = sample();
    let bytes = compiled.to_bytes();
    let loaded = CompiledSchema::from_bytes(&bytes).expect("loads");
    assert_eq!(loaded.label(), "person");
    assert_eq!(loaded.source(), compiled.source());
    assert!(loaded.is_portable());
    assert_eq!(loaded.to_bytes(), bytes, "serialization is deterministic");
}

#[test]
fn from_bytes_refuses_every_truncation() {
    let bytes = sample().to_bytes();
    for len in 0..bytes.len() {
        let error = CompiledSchema::from_bytes(&bytes[..len]).expect_err("truncated");
        assert_eq!(error.name, "SchemaLoadError", "at {len}: {}", error.message);
    }
}

#[test]
fn from_bytes_refuses_every_bit_flip() {
    let bytes = sample().to_bytes();
    let mut copy = bytes.clone();
    for index in 0..bytes.len() {
        for bit in 0..8 {
            copy[index] ^= 1 << bit;
            let error = CompiledSchema::from_bytes(&copy).expect_err("flipped");
            assert_eq!(error.name, "SchemaLoadError");
            copy[index] ^= 1 << bit;
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A record whose checksum was recomputed after the damage gets past the
/// checksum, so this exercises the bounds checks and `regex-automata`'s
/// validating loader: never a panic, and a schema that does load still runs.
#[test]
fn from_bytes_survives_damage_behind_a_valid_checksum() {
    let bytes = sample().to_bytes();
    let body = bytes.len() - 8;
    let documents = [
        object(fixture("valid/person.data.json")),
        object(json!({ "name": "x" })),
    ];
    let mut loaded = 0;
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for round in 0..4000 {
        let mut copy = bytes[..body].to_vec();
        // xorshift: a few random byte changes per round
        for _ in 0..=(round % 3) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index = usize::try_from(state % body as u64).expect("fits");
            copy[index] ^= u8::try_from((state >> 32) & 0xff).expect("fits") | 1;
        }
        let checksum = fnv1a(&copy);
        copy.extend_from_slice(&checksum.to_le_bytes());
        if let Ok(schema) = CompiledSchema::from_bytes(&copy) {
            loaded += 1;
            for document in &documents {
                let _ = schema.validate(document);
            }
        }
    }
    println!("{loaded} of 4000 damaged records still loaded");
}

#[test]
fn from_bytes_refuses_other_formats_and_versions() {
    let error = CompiledSchema::from_bytes(b"{\"a\":\"^x$\"}......").expect_err("not ours");
    assert_eq!(error.message, "Not a compiled CHEX schema (bad magic)");

    let mut bytes = sample().to_bytes();
    bytes[8] = 2;
    let body = bytes.len() - 8;
    let checksum = fnv1a(&bytes[..body]);
    bytes[body..].copy_from_slice(&checksum.to_le_bytes());
    let error = CompiledSchema::from_bytes(&bytes).expect_err("future version");
    assert_eq!(
        error.message,
        "Compiled schema format version 2 is not supported (expected 1)"
    );
}

#[test]
fn a_non_portable_schema_validates_natively_but_does_not_load() {
    let schema = json!({ "title": "\\bfoo\\b", "count": "^\\d+$" });
    let compiled = CompiledSchema::compile(&schema, "headlines").expect("compiles natively");
    assert!(!compiled.is_portable());
    assert_eq!(
        compiled.non_portable_patterns().collect::<Vec<_>>(),
        ["\\bfoo\\b"]
    );
    assert!(
        compiled
            .validate(&object(json!({ "title": "a foo b", "count": 3 })))
            .is_ok()
    );
    let error = compiled
        .validate(&object(json!({ "title": "afoob", "count": 3 })))
        .expect_err("no word boundary");
    assert_eq!(
        error.message,
        "RegEx pattern fails for property 'title' in schema 'headlines'"
    );

    let error = CompiledSchema::from_bytes(&compiled.to_bytes()).expect_err("not portable");
    assert_eq!(error.name, "SchemaLoadError");
    assert_eq!(
        error.message,
        "Compiled schema 'headlines' is not portable: a pattern has no DFA form (such as a \
         Unicode word boundary), so only a native build can validate it"
    );
}

// ---------------------------------------------------------------------------
// Timing (run with `cargo test --release -p fylo-chex --test compiled --
// --ignored --nocapture`; prints, never asserts)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn timing_cached_compiled_schema_against_the_uncached_path() {
    use std::time::Instant;

    const ROUNDS: u32 = 10_000;
    let schema = json!({
        "id": "^[0-9a-f-]{36}$",
        "name": "^\\w+( \\w+)*$",
        "email": "^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$",
        "age": "^[0-9]{1,3}$",
        "active": "^(true|false)$",
        "created": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(\\.\\d+)?Z$",
        "country": "^[A-Z]{2}$",
        "score": "^-?[0-9]+(\\.[0-9]+)?$",
        "tags": ["^[a-z]+$"],
        "nickname?": "^.+$"
    });
    let data = object(json!({
        "id": "123e4567-e89b-12d3-a456-426614174000",
        "name": "Jane Doe",
        "email": "jane@example.com",
        "age": 42,
        "active": true,
        "created": "2026-10-29T12:34:56.789Z",
        "country": "CA",
        "score": -3.25,
        "tags": ["alpha", "beta", "gamma"],
        "nickname": null
    }));

    let start = Instant::now();
    let compiled = CompiledSchema::compile(&schema, "timing").expect("compiles");
    let compile_time = start.elapsed();

    // Before: the definition check and the source walk on every write (the
    // per-thread regex cache warm).
    source_validate(&schema, &data, "timing").expect("valid");
    let start = Instant::now();
    for _ in 0..ROUNDS {
        source_validate(&schema, &data, "timing").expect("valid");
    }
    let uncached = start.elapsed();

    fylo_chex::validate_inline(&schema, &data, "timing").expect("valid");
    let start = Instant::now();
    for _ in 0..ROUNDS {
        fylo_chex::validate_inline(&schema, &data, "timing").expect("valid");
    }
    let cached = start.elapsed();

    let start = Instant::now();
    for _ in 0..ROUNDS {
        compiled.validate(&data).expect("valid");
    }
    let direct = start.elapsed();

    // The engine's path: a schema file, read on every write.
    let dir = std::env::temp_dir().join(format!("fylo-chex-timing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("timing.schema.json");
    std::fs::write(&path, serde_json::to_string(&schema).expect("json")).expect("write");
    let path = path.to_string_lossy().into_owned();
    let start = Instant::now();
    for _ in 0..ROUNDS {
        let text = std::fs::read_to_string(&path).expect("read");
        let schema = fylo_chex::parse_schema_document(&text, &path).expect("parse");
        source_validate(&schema, &data, &path).expect("valid");
    }
    let file_uncached = start.elapsed();
    let start = Instant::now();
    for _ in 0..ROUNDS {
        fylo_chex::SchemaValidator::default()
            .validate(&path, &data)
            .expect("valid");
    }
    let file_cached = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);

    let per = |total: std::time::Duration| total.as_secs_f64() * 1e6 / f64::from(ROUNDS);
    println!("compile once (10 leaves, DFAs):        {compile_time:?}");
    println!(
        "inline, definition check + walk:        {uncached:?} ({:.2} us/doc)",
        per(uncached)
    );
    println!(
        "inline, cached CompiledSchema:          {cached:?} ({:.2} us/doc)",
        per(cached)
    );
    println!(
        "CompiledSchema::validate directly:      {direct:?} ({:.2} us/doc)",
        per(direct)
    );
    println!(
        "file, read + parse + check + walk:      {file_uncached:?} ({:.2} us/doc)",
        per(file_uncached)
    );
    println!(
        "file, SchemaValidator (cached compile): {file_cached:?} ({:.2} us/doc)",
        per(file_cached)
    );
}
