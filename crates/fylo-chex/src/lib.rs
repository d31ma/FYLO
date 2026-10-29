//! CHEX: regex-driven JSON validation, built into FYLO.
//!
//! Vendored from CHEX v26.32.02 (<https://github.com/d31ma/CHEX>, MIT, see
//! `LICENSE-CHEX`). FYLO used to drive the `chex` binary as a subprocess; the
//! engine now calls this crate in-process. Only the validation core is kept:
//! the CLI, machine loop, C ABI, and JNI bindings stay upstream.
//!
//! Every leaf value in a CHEX schema is a regex pattern string. Data values are
//! coerced to strings and tested against the pattern. A trailing `?` on a schema
//! key marks the field nullable.
//!
//! Ported from the JavaScript implementation this repository shipped through
//! v26.28. Error names and messages are reproduced byte-for-byte so the machine
//! envelope did not change across the rewrite; `tests/golden.rs` replays the
//! envelopes recorded from that build and is what holds the contract in place.
//!
//! A schema is compiled once into a [`CompiledSchema`] whose leaves are sparse
//! DFAs (ADR 0017 section 4). The default `compile` feature holds everything
//! that needs a regex compiler: the definition check, the compiler, the
//! schema loader, and the per-thread caches. Without it the crate only loads a
//! serialized compiled schema and validates with it, linking just
//! `regex-automata`'s `dfa-search`, which is what the browser build can afford.

#[cfg(feature = "compile")]
use std::collections::HashMap;
#[cfg(feature = "compile")]
use std::sync::Arc;

#[cfg(feature = "compile")]
use regex::Regex;
use serde_json::{Map, Value};

mod compiled;
mod envelope;
mod tree;

pub use compiled::CompiledSchema;

#[cfg(feature = "compile")]
const MAX_REGEX_LENGTH: usize = 500;
const MESSAGE_TRUNCATE_AT: usize = 100;

/// A CHEX failure. `name` carries the error class the machine envelope reports —
/// the names are inherited from the original JavaScript build's error classes.
#[derive(Debug, Clone)]
pub struct ChexError {
    pub name: &'static str,
    pub message: String,
}

#[cfg_attr(not(feature = "compile"), allow(dead_code))]
impl ChexError {
    fn new(name: &'static str, message: String) -> Self {
        Self { name, message }
    }
    fn config(message: String) -> Self {
        Self::new("ConfigError", message)
    }
    fn invalid_input(message: String) -> Self {
        Self::new("InvalidInputError", message)
    }
    fn invalid_name(message: String) -> Self {
        Self::new("InvalidNameError", message)
    }
    fn schema_load(message: String) -> Self {
        Self::new("SchemaLoadError", message)
    }
    fn validation(message: String) -> Self {
        Self::new("ValidationError", message)
    }
    /// Anything reported as a bare `Error` rather than a CHEX error class.
    pub fn plain(message: impl Into<String>) -> Self {
        Self::new("Error", message.into())
    }
}

impl std::fmt::Display for ChexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ChexError {}

pub type Result<T> = std::result::Result<T, ChexError>;

// ---------------------------------------------------------------------------
// JS value semantics
// ---------------------------------------------------------------------------

/// Reproduce JavaScript's `String(value)` for a JSON value, which is what the
/// regex is tested against.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number(n),
        Value::String(s) => s.clone(),
        // `Array.prototype.join` renders null/undefined elements as empty.
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn js_number(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }
    let Some(value) = number.as_f64() else {
        return number.to_string();
    };
    // JSON `1.0` is `String()`-ed as "1" by JS but as "1.0" by serde_json.
    if value.fract() == 0.0 && value.abs() < 1e21 {
        // The guard above keeps this well inside i128's range.
        #[allow(clippy::cast_possible_truncation)]
        return format!("{}", value as i128);
    }
    // ponytail: diverges from JS exponent form for |x| >= 1e21 or < 1e-6.
    // Vendor ryu-js (as FYLO does) if a schema ever needs to match those.
    format!("{value}")
}

fn truncate(value: &str) -> String {
    // ponytail: JS slices UTF-16 units, this slices chars. Differs only for
    // non-BMP text in a >100-char path, which is cosmetic (message only).
    if value.chars().count() > MESSAGE_TRUNCATE_AT {
        let head: String = value.chars().take(MESSAGE_TRUNCATE_AT).collect();
        format!("{head}...")
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// Schema shape
// ---------------------------------------------------------------------------

fn is_nullable(key: &str) -> bool {
    key.ends_with('?')
}

fn data_key(key: &str) -> &str {
    if is_nullable(key) {
        &key[..key.len() - 1]
    } else {
        key
    }
}

/// An object is a Record descriptor when its single key is itself a regex,
/// i.e. `{ "^keyRegex$": "^valueRegex$" }`.
fn is_record_type(schema: &Map<String, Value>) -> bool {
    schema.len() == 1 && schema.keys().next().is_some_and(|key| key.starts_with('^'))
}

/// FYLO's field-encryption list, which sits beside the properties at the top
/// of a schema. It describes how fields are stored, not a field of the
/// document, so neither the definition check nor data validation treats it as
/// a property.
const ENCRYPTED_DIRECTIVE: &str = "$encrypted";

fn is_directive(path: &str, schema_key: &str) -> bool {
    path.is_empty() && schema_key == ENCRYPTED_DIRECTIVE
}

fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

// ---------------------------------------------------------------------------
// Regex constraints
// ---------------------------------------------------------------------------

/// Patterns compiled so far on this thread. Every validated write used to
/// compile each leaf's pattern again, which cost more than the match. A
/// `Regex` clone shares the compiled program. The cache is cleared when it
/// reaches its bound rather than evicting one entry at a time.
#[cfg(feature = "compile")]
const REGEX_CACHE_LIMIT: usize = 4096;

#[cfg(feature = "compile")]
thread_local! {
    static REGEX_CACHE: std::cell::RefCell<HashMap<String, Regex>> =
        std::cell::RefCell::new(HashMap::new());
}

#[cfg(feature = "compile")]
fn cached_regex(pattern: &str) -> std::result::Result<Regex, regex::Error> {
    REGEX_CACHE.with(|cache| {
        if let Some(regex) = cache.borrow().get(pattern) {
            return Ok(regex.clone());
        }
        let regex = Regex::new(pattern)?;
        let mut cache = cache.borrow_mut();
        if cache.len() >= REGEX_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(pattern.to_owned(), regex.clone());
        Ok(regex)
    })
}

#[cfg(feature = "compile")]
struct RegexConstraint {
    regex: Regex,
    full_path: String,
    schema_label: String,
}

#[cfg(feature = "compile")]
impl RegexConstraint {
    fn compile(pattern: &str, full_path: &str, schema_label: &str) -> Result<Self> {
        if pattern.len() > MAX_REGEX_LENGTH {
            return Err(ChexError::validation(format!(
                "Regex pattern for '{}' in schema '{}' exceeds maximum allowed length",
                truncate(full_path),
                truncate(schema_label)
            )));
        }
        let regex = cached_regex(pattern).map_err(|_| {
            ChexError::validation(format!(
                "Invalid RegEx pattern for '{}' in schema '{}'",
                truncate(full_path),
                truncate(schema_label)
            ))
        })?;
        Ok(Self {
            regex,
            full_path: full_path.to_string(),
            schema_label: schema_label.to_string(),
        })
    }

    fn test(&self, value: &Value) -> Result<()> {
        if self.regex.is_match(&js_string(value)) {
            return Ok(());
        }
        Err(ChexError::validation(format!(
            "RegEx pattern fails for property '{}' in schema '{}'",
            truncate(&self.full_path),
            truncate(&self.schema_label)
        )))
    }

    /// Same as [`test`] but for a key, which is already a string.
    fn test_str(&self, value: &str) -> Result<()> {
        self.test(&Value::String(value.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Schema definition validation
// ---------------------------------------------------------------------------

/// Check that every leaf of `schema` is a compilable, non-empty regex string and
/// that arrays hold exactly one item schema.
///
/// # Errors
/// Returns `InvalidInputError` for a malformed schema and `ValidationError`
/// for a pattern that is too long or does not compile.
#[cfg(feature = "compile")]
pub fn validate_definition(schema: &Value, label: &str) -> Result<()> {
    tree::build(schema, label, &mut |pattern, full_path| {
        RegexConstraint::compile(pattern, full_path, label).map(drop)
    })
    .map(drop)
}

// ---------------------------------------------------------------------------
// Data validation
// ---------------------------------------------------------------------------

#[cfg(feature = "compile")]
/// Validate `data` against an already-definition-checked `schema`.
///
/// # Errors
/// Returns `ValidationError` for the first property that does not match.
pub fn validate_object(
    schema: &Map<String, Value>,
    data: &Map<String, Value>,
    label: &str,
) -> Result<()> {
    validate_object_at(schema, data, label, "")
}

#[cfg(feature = "compile")]
fn validate_object_at(
    schema: &Map<String, Value>,
    data: &Map<String, Value>,
    label: &str,
    path: &str,
) -> Result<()> {
    // Schema keys may carry a trailing `?` for nullability; data keys never do.
    for key in data.keys() {
        let declared = schema.contains_key(key) && !is_directive(path, key);
        if declared || schema.contains_key(&format!("{key}?")) {
            continue;
        }
        return Err(ChexError::validation(format!(
            "Property '{}' does not exist in schema '{}'",
            truncate(key),
            truncate(label)
        )));
    }

    for schema_key in schema.keys() {
        if is_directive(path, schema_key) {
            continue;
        }
        validate_property(schema, data, schema_key, label, path)?;
    }
    Ok(())
}

#[cfg(feature = "compile")]
fn validate_property(
    schema: &Map<String, Value>,
    data: &Map<String, Value>,
    schema_key: &str,
    label: &str,
    path: &str,
) -> Result<()> {
    let Some(schema_value) = schema.get(schema_key) else {
        return Ok(());
    };
    let key = data_key(schema_key);
    let full_path = join_path(path, key);
    let value = data.get(key).unwrap_or(&Value::Null);
    let defined = !value.is_null();

    let reject_missing = || {
        ChexError::validation(format!(
            "Property '{}' cannot be null or undefined in schema '{}'",
            truncate(&full_path),
            truncate(label)
        ))
    };
    let type_mismatch = |at: &str, expected: &str| {
        ChexError::validation(format!(
            "Type mismatch for '{}' in schema '{}': expected an {expected}",
            truncate(at),
            truncate(label)
        ))
    };

    if !defined {
        return if is_nullable(schema_key) {
            Ok(())
        } else {
            Err(reject_missing())
        };
    }

    match schema_value {
        Value::String(pattern) => RegexConstraint::compile(pattern, &full_path, label)?.test(value),

        Value::Array(item_schemas) => {
            let Value::Array(items) = value else {
                return Err(type_mismatch(&full_path, "array"));
            };
            match item_schemas.first() {
                Some(Value::String(pattern)) => {
                    let constraint = RegexConstraint::compile(pattern, &full_path, label)?;
                    for item in items {
                        constraint.test(item)?;
                    }
                    Ok(())
                }
                Some(Value::Object(item_schema)) => {
                    for (index, item) in items.iter().enumerate() {
                        let item_path = format!("{full_path}[{index}]");
                        let Value::Object(item) = item else {
                            return Err(type_mismatch(&item_path, "object"));
                        };
                        validate_object_at(item_schema, item, label, &item_path)?;
                    }
                    Ok(())
                }
                // Unreachable once the definition validator has run.
                _ => Ok(()),
            }
        }

        Value::Object(nested_schema) => {
            let Value::Object(nested) = value else {
                return Err(type_mismatch(&full_path, "object"));
            };
            if !is_record_type(nested_schema) {
                return validate_object_at(nested_schema, nested, label, &full_path);
            }
            let Some((key_pattern, Value::String(value_pattern))) = nested_schema.iter().next()
            else {
                return Ok(());
            };
            for (entry_key, entry_value) in nested {
                RegexConstraint::compile(
                    key_pattern,
                    &format!("{full_path}.<key:{entry_key}>"),
                    label,
                )?
                .test_str(entry_key)?;
                RegexConstraint::compile(value_pattern, &join_path(&full_path, entry_key), label)?
                    .test(entry_value)?;
            }
            Ok(())
        }

        // Unreachable once the definition validator has run; the JS also no-ops.
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Schema loading (native only — wasm has no filesystem)
// ---------------------------------------------------------------------------

/// Reject JSONL: more than one non-empty line where every line is its own object.
fn assert_not_json_lines(text: &str, schema_path: &str) -> Result<()> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > 1
        && lines
            .iter()
            .all(|line| line.starts_with('{') && line.ends_with('}'))
    {
        return Err(ChexError::schema_load(format!(
            "Schema files must contain one JSON object, not JSONL: '{schema_path}'"
        )));
    }
    Ok(())
}

/// Parse the text of one `*.schema.json` file.
///
/// # Errors
/// Returns `SchemaLoadError` for JSON Lines or text that is not JSON.
pub fn parse_schema_document(text: &str, schema_path: &str) -> Result<Value> {
    assert_not_json_lines(text, schema_path)?;
    serde_json::from_str(text)
        .map_err(|_| ChexError::schema_load(format!("Failed to load schema from '{schema_path}'")))
}

#[cfg(feature = "compile")]
/// Accept `file://` URLs and the leading-slash Windows form returned by URL APIs.
fn normalize_location(path: &str, require_json_file: bool) -> Result<String> {
    if path.is_empty() {
        return Err(ChexError::config("A schema path is required".to_string()));
    }
    let mut normalized = path.to_string();
    if let Some(rest) = path.strip_prefix("file://") {
        // ponytail: no percent-decoding. Add it when a schema path needs %20.
        normalized = rest.to_string();
    } else if path.strip_prefix("file:").is_some() {
        normalized = path[5..].to_string();
    }
    if is_windows_drive_form(&normalized) {
        normalized = normalized[1..].to_string();
    }
    if require_json_file && !normalized.to_lowercase().ends_with(".schema.json") {
        return Err(ChexError::config(
            "Schema path must point to a .schema.json file".to_string(),
        ));
    }
    Ok(normalized)
}

#[cfg(feature = "compile")]
fn is_windows_drive_form(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes[3] == b'/'
}

#[cfg(feature = "compile")]
/// Where a schema reference resolves to, and how it is cached and labelled.
struct SchemaSource {
    path: String,
    label: String,
    cache_key: String,
}

#[cfg(feature = "compile")]
/// Loads, definition-checks, and caches schemas, then validates data against them.
#[derive(Default)]
pub struct SchemaValidator {
    pub schema_path: Option<String>,
    pub schema_dir: Option<String>,
    cache: HashMap<String, Arc<CompiledSchema>>,
}

#[cfg(feature = "compile")]
impl SchemaValidator {
    pub fn new(schema_path: Option<String>, schema_dir: Option<String>) -> Self {
        Self {
            schema_path,
            schema_dir,
            cache: HashMap::new(),
        }
    }

    fn assert_schema_name(name: &str) -> Result<()> {
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if !valid || name.contains("..") {
            return Err(ChexError::invalid_name("Invalid schema name".to_string()));
        }
        Ok(())
    }

    fn looks_like_path(schema_ref: &str) -> bool {
        schema_ref.contains('/')
            || schema_ref.contains('\\')
            || schema_ref.to_lowercase().ends_with(".schema.json")
    }

    fn source_for(&self, schema_ref: &str) -> Result<SchemaSource> {
        if let Some(path) = self.schema_path.as_deref().filter(|p| !p.is_empty()) {
            return Ok(SchemaSource {
                path: normalize_location(path, true)?,
                label: path.to_string(),
                cache_key: format!("path:{path}"),
            });
        }

        if Self::looks_like_path(schema_ref) || self.schema_dir.is_none() {
            return Ok(SchemaSource {
                path: normalize_location(schema_ref, true)?,
                label: schema_ref.to_string(),
                cache_key: format!("path:{schema_ref}"),
            });
        }

        let dir = self.schema_dir.as_deref().unwrap_or_default();
        if dir.is_empty() {
            return Err(ChexError::config(
                "A schema directory is required for name-based lookup".to_string(),
            ));
        }
        Self::assert_schema_name(schema_ref)?;
        let normalized = normalize_location(dir, false)?;
        let base = normalized.trim_end_matches(['/', '\\']);
        Ok(SchemaSource {
            path: format!("{base}/{schema_ref}.schema.json"),
            label: schema_ref.to_string(),
            cache_key: format!("dir:{dir}:{schema_ref}"),
        })
    }

    /// The compiled schema `schema_ref` resolves to. The file is read on every
    /// call that misses this validator's own cache, so an edited schema takes
    /// effect at once, but its text is parsed and compiled only once per
    /// thread.
    ///
    /// # Errors
    /// Returns the CHEX error for an unresolvable, unreadable, or malformed
    /// schema.
    pub fn compiled(&mut self, schema_ref: &str) -> Result<Arc<CompiledSchema>> {
        let source = self.source_for(schema_ref)?;
        if let Some(schema) = self.cache.get(&source.cache_key) {
            return Ok(Arc::clone(schema));
        }
        let text = std::fs::read_to_string(&source.path).map_err(|_| {
            ChexError::schema_load(format!("Failed to load schema from '{}'", source.path))
        })?;
        let schema = compiled::cache::file(&source.path, &source.label, &text, || {
            let schema = parse_schema_document(&text, &source.path)?;
            CompiledSchema::compile(&schema, &source.label)
        })?;
        self.cache.insert(source.cache_key, Arc::clone(&schema));
        Ok(schema)
    }

    /// Validate `data`, returning it unchanged on success.
    ///
    /// # Errors
    /// Returns the CHEX error for an unresolvable, unreadable, or malformed
    /// schema, or for data that does not match it.
    pub fn validate(&mut self, schema_ref: &str, data: &Map<String, Value>) -> Result<()> {
        self.compiled(schema_ref)?.validate(data)
    }
}

#[cfg(feature = "compile")]
/// Validate `data` against an in-memory schema.
///
/// # Errors
/// Returns the CHEX error for a malformed schema or non-matching data.
pub fn validate_inline(schema: &Value, data: &Map<String, Value>, label: &str) -> Result<()> {
    compile_cached(schema, label)?.validate(data)
}

#[cfg(feature = "compile")]
/// [`CompiledSchema::compile`], once per distinct `(schema, label)` on this
/// thread.
///
/// # Errors
/// Returns the definition check's error for a malformed schema.
pub fn compile_cached(schema: &Value, label: &str) -> Result<Arc<CompiledSchema>> {
    compiled::cache::inline(schema, label)
}

#[cfg(all(test, feature = "compile"))]
mod tests {
    // Behaviour is covered end to end by tests/validation.rs, tests/machine.rs,
    // tests/ffi.rs, and tests/golden.rs. What is left here is the handful of
    // internals those cannot reach from outside the crate.
    use super::*;
    use serde_json::json;

    #[test]
    fn coerces_values_the_way_javascript_does() {
        // The regex is matched against `String(value)`, not the JSON text.
        assert_eq!(js_string(&json!("abc")), "abc");
        assert_eq!(js_string(&json!(30)), "30");
        assert_eq!(js_string(&json!(-5)), "-5");
        // JSON `1.0` renders as "1" in JS but as "1.0" via serde_json's default.
        assert_eq!(js_string(&json!(1.0)), "1");
        assert_eq!(js_string(&json!(-273.15)), "-273.15");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(null)), "null");
        // Array.prototype.join renders null elements as empty strings.
        assert_eq!(js_string(&json!([1, null, 2])), "1,,2");
        assert_eq!(js_string(&json!(["a", ["b", "c"]])), "a,b,c");
        assert_eq!(js_string(&json!({})), "[object Object]");
    }

    #[test]
    fn caps_regex_pattern_length() {
        let within = "a".repeat(MAX_REGEX_LENGTH);
        assert!(RegexConstraint::compile(&within, "p", "s").is_ok());

        let beyond = "a".repeat(MAX_REGEX_LENGTH + 1);
        let Err(error) = RegexConstraint::compile(&beyond, "p", "s") else {
            panic!("an over-length pattern should be capped");
        };
        assert_eq!(error.name, "ValidationError");
        assert_eq!(
            error.message,
            "Regex pattern for 'p' in schema 's' exceeds maximum allowed length"
        );
    }

    #[test]
    fn truncates_long_paths_in_messages() {
        let long = "x".repeat(MESSAGE_TRUNCATE_AT + 20);
        let truncated = truncate(&long);
        assert_eq!(truncated.len(), MESSAGE_TRUNCATE_AT + 3);
        assert!(truncated.ends_with("..."));
        assert_eq!(truncate("short"), "short");
    }
}
