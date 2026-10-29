//! The shape of a CHEX schema, resolved once.
//!
//! [`build`] walks a schema the way the definition check always has and calls
//! back for every leaf pattern, so the definition check, the native compiler,
//! and the loader of stored compiled schemas share one walk and therefore
//! report the same first error for the same schema.

use serde_json::{Map, Value};

use crate::{
    ChexError, Result, data_key, is_directive, is_nullable, is_record_type, join_path, truncate,
};

/// One object level of a schema: its properties in schema order, and the data
/// keys it accepts.
pub(crate) struct ObjectNode<L> {
    pub(crate) fields: Vec<Field<L>>,
    /// Every data key this level accepts: each schema key, and each nullable
    /// schema key without its `?`. The top-level `$encrypted` directive is not
    /// one of them. Sorted; a sorted list keeps the browser build smaller than
    /// a hash set would.
    accepted: Vec<String>,
}

impl<L> ObjectNode<L> {
    pub(crate) fn accepts(&self, key: &str) -> bool {
        self.accepted
            .binary_search_by(|accepted| accepted.as_str().cmp(key))
            .is_ok()
    }
}

pub(crate) struct Field<L> {
    pub(crate) schema_key: String,
    pub(crate) node: Node<L>,
}

impl<L> Field<L> {
    pub(crate) fn data_key(&self) -> &str {
        data_key(&self.schema_key)
    }

    pub(crate) fn nullable(&self) -> bool {
        is_nullable(&self.schema_key)
    }
}

pub(crate) enum Node<L> {
    /// `"field": "^re$"`
    Pattern(L),
    /// `"field": ["^re$"]`
    Patterns(L),
    /// `"field": [{ ... }]`
    Objects(ObjectNode<L>),
    /// `"field": { ... }`
    Object(ObjectNode<L>),
    /// `"field": { "^keyRe$": "^valueRe$" }`
    Record { key: L, value: L },
}

/// Walk `schema`, calling `leaf(pattern, full_path)` for every non-empty
/// pattern string.
///
/// # Errors
/// Returns `InvalidInputError` for a malformed schema, or whatever `leaf`
/// returns for a pattern it refuses.
pub(crate) fn build<L>(
    schema: &Value,
    label: &str,
    leaf: &mut impl FnMut(&str, &str) -> Result<L>,
) -> Result<ObjectNode<L>> {
    let map = require_object_schema(schema, label, "")?;
    build_map(map, label, "", leaf)
}

fn require_regex_string<L>(
    value: &Value,
    full_path: &str,
    label: &str,
    leaf: &mut impl FnMut(&str, &str) -> Result<L>,
) -> Result<L> {
    let Value::String(pattern) = value else {
        return Err(ChexError::invalid_input(format!(
            "Schema value for '{}' in schema '{}' must be a regex string",
            truncate(full_path),
            truncate(label)
        )));
    };
    if pattern.is_empty() {
        return Err(ChexError::invalid_input(format!(
            "Schema pattern for '{}' in schema '{}' cannot be empty",
            truncate(full_path),
            truncate(label)
        )));
    }
    leaf(pattern, full_path)
}

fn require_object_schema<'a>(
    schema: &'a Value,
    label: &str,
    path: &str,
) -> Result<&'a Map<String, Value>> {
    let Value::Object(map) = schema else {
        return Err(ChexError::invalid_input(format!(
            "Schema '{}' must be a JSON object",
            truncate(label)
        )));
    };
    if map.is_empty() {
        let location = if path.is_empty() {
            String::new()
        } else {
            format!(" at '{}'", truncate(path))
        };
        return Err(ChexError::invalid_input(format!(
            "Schema '{}'{location} must define at least one property",
            truncate(label)
        )));
    }
    Ok(map)
}

fn build_map<L>(
    map: &Map<String, Value>,
    label: &str,
    path: &str,
    leaf: &mut impl FnMut(&str, &str) -> Result<L>,
) -> Result<ObjectNode<L>> {
    let mut fields = Vec::with_capacity(map.len());
    let mut accepted = Vec::with_capacity(map.len());
    for (schema_key, schema_value) in map {
        if is_directive(path, schema_key) {
            continue;
        }
        let full_path = join_path(path, data_key(schema_key));

        let node = match schema_value {
            Value::String(_) => {
                Node::Pattern(require_regex_string(schema_value, &full_path, label, leaf)?)
            }
            Value::Array(items) => build_array(items, &full_path, label, leaf)?,
            Value::Object(nested) => build_object(nested, &full_path, label, leaf)?,
            _ => {
                return Err(ChexError::invalid_input(format!(
                    "Schema value for '{}' in schema '{}' must be a regex string",
                    truncate(&full_path),
                    truncate(label)
                )));
            }
        };
        accepted.push(schema_key.clone());
        if is_nullable(schema_key) {
            accepted.push(data_key(schema_key).to_owned());
        }
        fields.push(Field {
            schema_key: schema_key.clone(),
            node,
        });
    }
    accepted.sort_unstable();
    accepted.dedup();
    Ok(ObjectNode { fields, accepted })
}

fn build_array<L>(
    items: &[Value],
    full_path: &str,
    label: &str,
    leaf: &mut impl FnMut(&str, &str) -> Result<L>,
) -> Result<Node<L>> {
    let malformed = || {
        ChexError::invalid_input(format!(
            "Array schema for '{}' in schema '{}' must contain exactly one regex string or object schema",
            truncate(full_path),
            truncate(label)
        ))
    };
    let [item] = items else {
        return Err(malformed());
    };

    match item {
        Value::String(_) => Ok(Node::Patterns(require_regex_string(
            item, full_path, label, leaf,
        )?)),
        Value::Object(_) => {
            let nested_path = format!("{full_path}[]");
            let nested = require_object_schema(item, label, &nested_path)?;
            Ok(Node::Objects(build_map(nested, label, &nested_path, leaf)?))
        }
        _ => Err(malformed()),
    }
}

fn build_object<L>(
    nested: &Map<String, Value>,
    full_path: &str,
    label: &str,
    leaf: &mut impl FnMut(&str, &str) -> Result<L>,
) -> Result<Node<L>> {
    if nested.is_empty() {
        return Err(ChexError::invalid_input(format!(
            "Schema '{}' at '{}' must define at least one property",
            truncate(label),
            truncate(full_path)
        )));
    }

    if is_record_type(nested)
        && let Some((key_pattern, value_pattern)) = nested.iter().next()
    {
        let key = require_regex_string(
            &Value::String(key_pattern.clone()),
            &format!("{full_path}.<key>"),
            label,
            leaf,
        )?;
        let value = require_regex_string(value_pattern, full_path, label, leaf)?;
        return Ok(Node::Record { key, value });
    }

    Ok(Node::Object(build_map(nested, label, full_path, leaf)?))
}
