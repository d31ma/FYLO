//! A schema compiled once: the tree resolved and every leaf pattern compiled
//! to a sparse DFA (ADR 0017 section 4).
//!
//! The native build compiles a schema from its source with
//! [`CompiledSchema::compile`]. Every build, the browser included, can load
//! the serialized form with [`CompiledSchema::from_bytes`], which needs only
//! `regex-automata`'s `dfa-search` feature: no parser, no compiler.

use std::borrow::Cow;
use std::sync::Arc;

use regex_automata::Input;
use regex_automata::dfa::Automaton;
use regex_automata::dfa::sparse;
use serde_json::{Map, Value};

use crate::tree::{self, Node, ObjectNode};
use crate::{ChexError, Result, join_path, js_string, truncate};

/// How one leaf pattern is matched.
pub(crate) enum Matcher {
    /// The pattern's forward, unanchored sparse DFA.
    Dfa(Box<sparse::DFA<Vec<u8>>>),
    /// A pattern with no DFA form, such as a Unicode word boundary. Only a
    /// native build has these, and a schema holding one is not portable.
    #[cfg(feature = "compile")]
    Regex(regex::Regex),
}

impl Matcher {
    /// `Regex::is_match`: is there a match anywhere in `haystack`?
    fn is_match(&self, haystack: &str) -> std::result::Result<bool, regex_automata::MatchError> {
        match self {
            // `try_search_fwd` skips empty matches that split a codepoint, as
            // `regex` does in its default UTF-8 mode, so `is_some` is exactly
            // `Regex::is_match`. `earliest` stops at the first match state.
            Self::Dfa(dfa) => Ok(dfa
                .try_search_fwd(&Input::new(haystack).earliest(true))?
                .is_some()),
            #[cfg(feature = "compile")]
            Self::Regex(regex) => Ok(regex.is_match(haystack)),
        }
    }
}

/// A leaf pattern and how it is matched. Leaves with the same pattern share one.
pub(crate) struct Leaf {
    pub(crate) pattern: String,
    pub(crate) matcher: Matcher,
}

impl Leaf {
    fn test(&self, value: &Value, at: &dyn Fn() -> String, label: &str) -> Result<()> {
        let text = match value {
            Value::String(text) => Cow::Borrowed(text.as_str()),
            other => Cow::Owned(js_string(other)),
        };
        self.test_str(&text, at, label)
    }

    fn test_str(&self, text: &str, at: &dyn Fn() -> String, label: &str) -> Result<()> {
        match self.matcher.is_match(text) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ChexError::validation(format!(
                "RegEx pattern fails for property '{}' in schema '{}'",
                truncate(&at()),
                truncate(label)
            ))),
            // Only a stored DFA built with quit bytes or without unanchored
            // starts can refuse a search; the native compiler builds neither.
            Err(error) => Err(ChexError::plain(format!(
                "Compiled pattern for '{}' in schema '{}' cannot be searched: {error}",
                truncate(&at()),
                truncate(label)
            ))),
        }
    }
}

/// A definition-checked CHEX schema with every leaf compiled.
///
/// [`validate`](Self::validate) gives the same result and the same error,
/// byte for byte, as [`validate_inline`](crate::validate_inline) on the
/// schema it was compiled from.
pub struct CompiledSchema {
    pub(crate) label: String,
    /// The schema source this was compiled from.
    pub(crate) source: Value,
    pub(crate) root: ObjectNode<Arc<Leaf>>,
    /// Each distinct leaf, in first-seen schema order.
    pub(crate) leaves: Vec<Arc<Leaf>>,
}

impl CompiledSchema {
    /// Assemble a compiled schema from `source`, asking `matcher` for each
    /// distinct pattern once.
    pub(crate) fn assemble(
        source: Value,
        label: &str,
        matcher: &mut impl FnMut(&str, &str) -> Result<Matcher>,
    ) -> Result<Self> {
        let mut leaves: Vec<Arc<Leaf>> = Vec::new();
        let root = tree::build(&source, label, &mut |pattern, full_path| {
            if let Some(leaf) = leaves.iter().find(|leaf| leaf.pattern == pattern) {
                return Ok(Arc::clone(leaf));
            }
            let leaf = Arc::new(Leaf {
                pattern: pattern.to_owned(),
                matcher: matcher(pattern, full_path)?,
            });
            leaves.push(Arc::clone(&leaf));
            Ok(leaf)
        })?;
        Ok(Self {
            label: label.to_owned(),
            source,
            root,
            leaves,
        })
    }

    /// The schema label error messages name.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The schema source this was compiled from.
    pub fn source(&self) -> &Value {
        &self.source
    }

    /// Whether every leaf has a DFA, so that the schema can be serialized for
    /// and validated by a build without a regex compiler.
    pub fn is_portable(&self) -> bool {
        self.non_portable_patterns().next().is_none()
    }

    /// The patterns that have no DFA form, in schema order.
    pub fn non_portable_patterns(&self) -> impl Iterator<Item = &str> {
        self.leaves
            .iter()
            .filter(|leaf| !matches!(leaf.matcher, Matcher::Dfa(_)))
            .map(|leaf| leaf.pattern.as_str())
    }

    /// Validate `data`.
    ///
    /// # Errors
    /// Returns `ValidationError` for the first property that does not match,
    /// exactly as [`validate_object`](crate::validate_object) would.
    pub fn validate(&self, data: &Map<String, Value>) -> Result<()> {
        validate_object(&self.root, data, &self.label, "")
    }
}

impl std::fmt::Debug for CompiledSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSchema")
            .field("label", &self.label)
            .field("leaves", &self.leaves.len())
            .field("portable", &self.is_portable())
            .finish_non_exhaustive()
    }
}

fn validate_object(
    object: &ObjectNode<Arc<Leaf>>,
    data: &Map<String, Value>,
    label: &str,
    path: &str,
) -> Result<()> {
    // Schema keys may carry a trailing `?` for nullability; data keys never do.
    for key in data.keys() {
        if object.accepts(key) {
            continue;
        }
        return Err(ChexError::validation(format!(
            "Property '{}' does not exist in schema '{}'",
            truncate(key),
            truncate(label)
        )));
    }

    for field in &object.fields {
        let key = field.data_key();
        let value = data.get(key).unwrap_or(&Value::Null);
        let full_path = || join_path(path, key);

        if value.is_null() {
            if field.nullable() {
                continue;
            }
            return Err(ChexError::validation(format!(
                "Property '{}' cannot be null or undefined in schema '{}'",
                truncate(&full_path()),
                truncate(label)
            )));
        }
        let type_mismatch = |at: &str, expected: &str| {
            ChexError::validation(format!(
                "Type mismatch for '{}' in schema '{}': expected an {expected}",
                truncate(at),
                truncate(label)
            ))
        };

        match &field.node {
            Node::Pattern(leaf) => leaf.test(value, &full_path, label)?,
            Node::Patterns(leaf) => {
                let Value::Array(items) = value else {
                    return Err(type_mismatch(&full_path(), "array"));
                };
                for item in items {
                    leaf.test(item, &full_path, label)?;
                }
            }
            Node::Objects(item_schema) => {
                let Value::Array(items) = value else {
                    return Err(type_mismatch(&full_path(), "array"));
                };
                let full_path = full_path();
                for (index, item) in items.iter().enumerate() {
                    let item_path = format!("{full_path}[{index}]");
                    let Value::Object(item) = item else {
                        return Err(type_mismatch(&item_path, "object"));
                    };
                    validate_object(item_schema, item, label, &item_path)?;
                }
            }
            Node::Object(nested_schema) => {
                let Value::Object(nested) = value else {
                    return Err(type_mismatch(&full_path(), "object"));
                };
                validate_object(nested_schema, nested, label, &full_path())?;
            }
            Node::Record {
                key: key_leaf,
                value: value_leaf,
            } => {
                let Value::Object(nested) = value else {
                    return Err(type_mismatch(&full_path(), "object"));
                };
                for (entry_key, entry_value) in nested {
                    key_leaf.test_str(
                        entry_key,
                        &|| format!("{}.<key:{entry_key}>", full_path()),
                        label,
                    )?;
                    value_leaf.test(entry_value, &|| join_path(&full_path(), entry_key), label)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Native compilation
// ---------------------------------------------------------------------------

/// Bounds on one leaf's automata, which also bound the time spent building
/// them. A pattern past any of them falls back to `regex` natively and makes
/// the schema non-portable.
#[cfg(feature = "compile")]
const NFA_SIZE_LIMIT: usize = 1 << 20;
#[cfg(feature = "compile")]
const DFA_SIZE_LIMIT: usize = 4 << 20;
#[cfg(feature = "compile")]
const DETERMINIZE_SIZE_LIMIT: usize = 8 << 20;

/// Build the forward, unanchored sparse DFA that matches exactly what
/// `regex::Regex::new(pattern)` matches. The syntax and NFA configurations are
/// `regex`'s defaults: Unicode on, UTF-8 on.
#[cfg(feature = "compile")]
pub(crate) fn build_dfa(pattern: &str) -> Option<sparse::DFA<Vec<u8>>> {
    use regex_automata::dfa::{StartKind, dense};
    use regex_automata::nfa::thompson;
    use regex_automata::util::syntax;

    let dense = dense::Builder::new()
        .syntax(syntax::Config::new())
        .thompson(thompson::Config::new().nfa_size_limit(Some(NFA_SIZE_LIMIT)))
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Unanchored)
                .dfa_size_limit(Some(DFA_SIZE_LIMIT))
                .determinize_size_limit(Some(DETERMINIZE_SIZE_LIMIT)),
        )
        .build(pattern)
        .ok()?;
    dense.to_sparse().ok()
}

#[cfg(feature = "compile")]
impl CompiledSchema {
    /// Compile `schema`: check its definition exactly as
    /// [`validate_definition`](crate::validate_definition) does, then compile
    /// each leaf to a DFA, falling back to `regex` for a leaf that has none.
    ///
    /// # Errors
    /// Returns the definition check's error for a malformed schema.
    pub fn compile(schema: &Value, label: &str) -> Result<Self> {
        Self::assemble(schema.clone(), label, &mut |pattern, full_path| {
            // `regex` first, so a bad or over-long pattern reports exactly as
            // it always has.
            let regex = crate::RegexConstraint::compile(pattern, full_path, label)?.regex;
            Ok(build_dfa(pattern).map_or(Matcher::Regex(regex), |dfa| Matcher::Dfa(Box::new(dfa))))
        })
    }
}

// ---------------------------------------------------------------------------
// Per-thread caches (native)
// ---------------------------------------------------------------------------

#[cfg(feature = "compile")]
pub(crate) mod cache {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;

    use serde_json::Value;

    use super::CompiledSchema;
    use crate::Result;

    /// Compiled schemas held per thread. Each map is cleared when it reaches
    /// its bound rather than evicting one entry at a time.
    const CACHE_LIMIT: usize = 256;

    /// A schema file's text and what it compiled to.
    type FileEntry = (String, Arc<CompiledSchema>);

    thread_local! {
        /// In-memory schemas, by a hash of `(label, schema)`.
        static INLINE: RefCell<HashMap<u64, Vec<Arc<CompiledSchema>>>> =
            RefCell::new(HashMap::new());
        /// Schema files, by `(path, label)`, with the text they were compiled from.
        static FILES: RefCell<HashMap<(String, String), FileEntry>> =
            RefCell::new(HashMap::new());
    }

    /// `CompiledSchema::compile`, once per distinct `(schema, label)` on this
    /// thread.
    pub(crate) fn inline(schema: &Value, label: &str) -> Result<Arc<CompiledSchema>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        label.hash(&mut hasher);
        hash_value(schema, &mut hasher);
        let key = hasher.finish();

        let hit = INLINE.with(|cache| {
            cache.borrow().get(&key).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.label == label && same_value(&entry.source, schema))
                    .cloned()
            })
        });
        if let Some(compiled) = hit {
            return Ok(compiled);
        }
        let compiled = Arc::new(CompiledSchema::compile(schema, label)?);
        INLINE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= CACHE_LIMIT {
                cache.clear();
            }
            cache.entry(key).or_default().push(Arc::clone(&compiled));
        });
        Ok(compiled)
    }

    /// The schema compiled from the file at `path` whose text is `text`;
    /// `compile` runs only when this thread has not compiled that text.
    pub(crate) fn file(
        path: &str,
        label: &str,
        text: &str,
        compile: impl FnOnce() -> Result<CompiledSchema>,
    ) -> Result<Arc<CompiledSchema>> {
        let key = (path.to_owned(), label.to_owned());
        let hit = FILES.with(|cache| {
            cache
                .borrow()
                .get(&key)
                .filter(|(cached_text, _)| cached_text == text)
                .map(|(_, compiled)| Arc::clone(compiled))
        });
        if let Some(compiled) = hit {
            return Ok(compiled);
        }
        let compiled = Arc::new(compile()?);
        FILES.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, (text.to_owned(), Arc::clone(&compiled)));
        });
        Ok(compiled)
    }

    fn hash_value(value: &Value, hasher: &mut impl Hasher) {
        match value {
            Value::Null => 0u8.hash(hasher),
            Value::Bool(b) => (1u8, b).hash(hasher),
            Value::Number(n) => (2u8, n.to_string()).hash(hasher),
            Value::String(s) => (3u8, s).hash(hasher),
            Value::Array(items) => {
                (4u8, items.len()).hash(hasher);
                for item in items {
                    hash_value(item, hasher);
                }
            }
            Value::Object(map) => {
                (5u8, map.len()).hash(hasher);
                for (key, item) in map {
                    key.hash(hasher);
                    hash_value(item, hasher);
                }
            }
        }
    }

    /// Equality that, unlike `Value`'s, counts object key order: the order
    /// decides which error a document reports first.
    fn same_value(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && same_value(va, vb))
            }
            _ => a == b,
        }
    }
}
