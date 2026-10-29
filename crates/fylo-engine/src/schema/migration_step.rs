use serde_json::{Map, Number, Value};

use super::document_path::DocumentPath;

/// One declarative migration step (ADR 0017 section 5, ADR 0018 section 2).
///
/// A step is a JSON object whose one operation key names the step and the
/// field it works on, plus that operation's parameters, for example
/// `{"rename": "a/b", "to": "a/c"}`. Every build runs the same steps, so no
/// step executes code or compiles a pattern.
pub(super) enum MigrationStep {
    Rename(DocumentPath, DocumentPath),
    Default(DocumentPath, Value),
    Set(DocumentPath, Value),
    Remove(DocumentPath),
    Copy(DocumentPath, DocumentPath),
    Convert(DocumentPath, Conversion),
    Map {
        path: DocumentPath,
        values: Map<String, Value>,
        otherwise: Option<Value>,
    },
    Wrap(DocumentPath),
    Lower(DocumentPath),
    Upper(DocumentPath),
    Trim(DocumentPath),
    Replace {
        path: DocumentPath,
        find: String,
        with: String,
    },
    Template(DocumentPath, String),
    Slugify(DocumentPath, DocumentPath),
}

pub(super) enum Conversion {
    String,
    Number,
    Boolean,
}

/// The operation keys and the parameters each one takes.
const OPERATIONS: &[(&str, &[&str])] = &[
    ("rename", &["to"]),
    ("default", &["value"]),
    ("set", &["value"]),
    ("remove", &[]),
    ("copy", &["to"]),
    ("convert", &["to"]),
    ("map", &["values", "else"]),
    ("wrap", &[]),
    ("lower", &[]),
    ("upper", &[]),
    ("trim", &[]),
    ("replace", &["find", "with"]),
    ("template", &["value"]),
    ("slugify", &["from"]),
];

impl MigrationStep {
    pub(super) fn parse(step: &Value) -> Result<Self, String> {
        let fields = step
            .as_object()
            .ok_or_else(|| format!("migration step must be an object, got {step}"))?;
        let (operation, parameters) = Self::operation(fields)?;
        Self::reject_unknown(fields, operation, parameters)?;
        let path = DocumentPath::parse(Self::text(fields, operation)?)?;
        Self::build(operation, path, fields)
    }

    fn operation(
        fields: &Map<String, Value>,
    ) -> Result<(&'static str, &'static [&'static str]), String> {
        let mut found = OPERATIONS
            .iter()
            .filter(|(operation, _)| fields.contains_key(*operation));
        match (found.next(), found.next()) {
            (Some((operation, parameters)), None) => Ok((operation, parameters)),
            (None, _) => Err(format!(
                "migration step {} names no known operation",
                Value::Object(fields.clone())
            )),
            (Some((first, _)), Some((second, _))) => Err(format!(
                "migration step names both {first:?} and {second:?}; use one operation per step"
            )),
        }
    }

    fn reject_unknown(
        fields: &Map<String, Value>,
        operation: &str,
        parameters: &[&str],
    ) -> Result<(), String> {
        match fields
            .keys()
            .find(|key| *key != operation && !parameters.contains(&key.as_str()))
        {
            Some(key) => Err(format!(
                "migration step {operation:?} does not take {key:?}"
            )),
            None => Ok(()),
        }
    }

    fn build(
        operation: &str,
        path: DocumentPath,
        fields: &Map<String, Value>,
    ) -> Result<Self, String> {
        Ok(match operation {
            "rename" => Self::Rename(path, Self::path(fields, "to")?),
            "default" => Self::Default(path, Self::value(fields, operation, "value")?),
            "set" => Self::Set(path, Self::value(fields, operation, "value")?),
            "remove" => Self::Remove(path),
            "copy" => Self::Copy(path, Self::path(fields, "to")?),
            "convert" => Self::Convert(path, Self::conversion(fields)?),
            "map" => Self::Map {
                path,
                values: fields
                    .get("values")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or("migration step \"map\" needs a \"values\" object")?,
                otherwise: fields.get("else").cloned(),
            },
            "wrap" => Self::Wrap(path),
            "lower" => Self::Lower(path),
            "upper" => Self::Upper(path),
            "trim" => Self::Trim(path),
            "replace" => {
                let find = Self::text(fields, "find")?.to_owned();
                if find.is_empty() {
                    return Err("migration step \"replace\" needs a non-empty \"find\"".into());
                }
                Self::Replace {
                    path,
                    find,
                    with: Self::text(fields, "with")?.to_owned(),
                }
            }
            "template" => Self::Template(path, Self::text(fields, "value")?.to_owned()),
            _ => Self::Slugify(path, Self::path(fields, "from")?),
        })
    }

    fn text<'a>(fields: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
        fields
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("migration step field {key:?} must be a string"))
    }

    fn path(fields: &Map<String, Value>, key: &str) -> Result<DocumentPath, String> {
        DocumentPath::parse(Self::text(fields, key)?)
    }

    fn value(fields: &Map<String, Value>, operation: &str, key: &str) -> Result<Value, String> {
        fields
            .get(key)
            .cloned()
            .ok_or_else(|| format!("migration step {operation:?} needs {key:?}"))
    }

    fn conversion(fields: &Map<String, Value>) -> Result<Conversion, String> {
        match Self::text(fields, "to")? {
            "string" => Ok(Conversion::String),
            "number" => Ok(Conversion::Number),
            "boolean" => Ok(Conversion::Boolean),
            other => Err(format!(
                "migration step \"convert\" cannot convert to {other:?}; use string, number or boolean"
            )),
        }
    }

    /// The top-level field this step may write or remove.
    pub(super) fn written_field(&self) -> &str {
        match self {
            Self::Rename(_, to) | Self::Copy(_, to) => to.top(),
            Self::Default(path, _)
            | Self::Set(path, _)
            | Self::Remove(path)
            | Self::Convert(path, _)
            | Self::Map { path, .. }
            | Self::Wrap(path)
            | Self::Lower(path)
            | Self::Upper(path)
            | Self::Trim(path)
            | Self::Replace { path, .. }
            | Self::Template(path, _)
            | Self::Slugify(path, _) => path.top(),
        }
    }

    /// The top-level field a rename takes its value out of, which it removes.
    pub(super) fn vacated_field(&self) -> Option<&str> {
        match self {
            Self::Rename(from, _) => Some(from.top()),
            _ => None,
        }
    }

    /// Apply the step. A step whose field is missing changes nothing; a field
    /// of the wrong type fails the migration.
    pub(super) fn apply(&self, document: &mut Map<String, Value>) -> Result<(), String> {
        match self {
            Self::Rename(from, to) => {
                if let Some(value) = from.remove(document) {
                    to.set(document, value)?;
                }
            }
            Self::Default(path, value) => {
                if path.present(document).is_none() {
                    path.set(document, value.clone())?;
                }
            }
            Self::Set(path, value) => path.set(document, value.clone())?,
            Self::Remove(path) => {
                path.remove(document);
            }
            Self::Copy(from, to) => {
                if let Some(value) = from.get(document).cloned() {
                    to.set(document, value)?;
                }
            }
            Self::Convert(path, conversion) => {
                if let Some(value) = path.present(document) {
                    let converted = conversion.apply(value, path)?;
                    path.set(document, converted)?;
                }
            }
            Self::Map {
                path,
                values,
                otherwise,
            } => Self::map(document, path, values, otherwise.as_ref())?,
            Self::Wrap(path) => {
                if let Some(value) = path.get(document).filter(|value| !value.is_array()) {
                    let wrapped = Value::Array(vec![value.clone()]);
                    path.set(document, wrapped)?;
                }
            }
            Self::Lower(path) => {
                Self::edit_text(document, path, "lower", str::to_lowercase)?;
            }
            Self::Upper(path) => {
                Self::edit_text(document, path, "upper", str::to_uppercase)?;
            }
            Self::Trim(path) => {
                Self::edit_text(document, path, "trim", |text| text.trim().to_owned())?;
            }
            Self::Replace { path, find, with } => {
                Self::edit_text(document, path, "replace", |text| text.replace(find, with))?;
            }
            Self::Template(path, template) => {
                let rendered = Self::render(document, template)?;
                path.set(document, Value::String(rendered))?;
            }
            Self::Slugify(path, from) => {
                let source = match from.present(document) {
                    Some(value) => scalar_text(value).ok_or_else(|| {
                        format!(
                            "migration step \"slugify\" cannot read {} as text",
                            from.display()
                        )
                    })?,
                    None => String::new(),
                };
                path.set(document, Value::String(slugify(&source)))?;
            }
        }
        Ok(())
    }

    fn map(
        document: &mut Map<String, Value>,
        path: &DocumentPath,
        values: &Map<String, Value>,
        otherwise: Option<&Value>,
    ) -> Result<(), String> {
        let Some(value) = path.get(document) else {
            return Ok(());
        };
        let key = scalar_text(value).unwrap_or_else(|| value.to_string());
        let replacement = values.get(&key).or(otherwise).cloned();
        if let Some(replacement) = replacement {
            path.set(document, replacement)?;
        }
        Ok(())
    }

    fn edit_text(
        document: &mut Map<String, Value>,
        path: &DocumentPath,
        operation: &str,
        edit: impl FnOnce(&str) -> String,
    ) -> Result<(), String> {
        let Some(value) = path.present(document) else {
            return Ok(());
        };
        let text = value.as_str().ok_or_else(|| {
            format!(
                "migration step {operation:?} needs a string at {}, found {value}",
                path.display()
            )
        })?;
        let edited = edit(text);
        path.set(document, Value::String(edited))
    }

    /// Fill `{path}` placeholders from the document. `{{` and `}}` are literal
    /// braces; a missing or null field renders as nothing.
    fn render(document: &Map<String, Value>, template: &str) -> Result<String, String> {
        let mut rendered = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(index) = rest.find(['{', '}']) {
            rendered.push_str(&rest[..index]);
            let tail = &rest[index..];
            if let Some(after) = tail.strip_prefix("{{") {
                rendered.push('{');
                rest = after;
            } else if let Some(after) = tail.strip_prefix("}}") {
                rendered.push('}');
                rest = after;
            } else if let Some(after) = tail.strip_prefix('{') {
                let end = after
                    .find('}')
                    .ok_or_else(|| format!("migration template {template:?} has an unclosed {{"))?;
                let path = DocumentPath::parse(&after[..end])?;
                if let Some(value) = path.present(document) {
                    rendered.push_str(&scalar_text(value).ok_or_else(|| {
                        format!(
                            "migration template cannot render {} as text",
                            path.display()
                        )
                    })?);
                }
                rest = &after[end + 1..];
            } else {
                return Err(format!(
                    "migration template {template:?} has an unmatched }}"
                ));
            }
        }
        rendered.push_str(rest);
        Ok(rendered)
    }
}

impl Conversion {
    fn apply(&self, value: &Value, path: &DocumentPath) -> Result<Value, String> {
        let failed = |target: &str| {
            format!(
                "migration step \"convert\" cannot convert {value} at {} to {target}",
                path.display()
            )
        };
        match self {
            Self::String => scalar_text(value)
                .map(Value::String)
                .ok_or_else(|| failed("string")),
            Self::Number => match value {
                Value::Number(_) => Ok(value.clone()),
                Value::Bool(flag) => Ok(Value::from(u8::from(*flag))),
                Value::String(text) => parse_number(text).ok_or_else(|| failed("number")),
                _ => Err(failed("number")),
            },
            Self::Boolean => match value {
                Value::Bool(_) => Ok(value.clone()),
                Value::String(text) if text == "true" => Ok(Value::Bool(true)),
                Value::String(text) if text == "false" => Ok(Value::Bool(false)),
                Value::Number(number) if number.as_f64() == Some(1.0) => Ok(Value::Bool(true)),
                Value::Number(number) if number.as_f64() == Some(0.0) => Ok(Value::Bool(false)),
                _ => Err(failed("boolean")),
            },
        }
    }
}

/// JavaScript's `String(value)` for a scalar, or `None` for an object or
/// array. `null` is handled by callers, which treat it as missing.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number_text(number)),
        Value::Null => Some("null".to_owned()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn number_text(number: &Number) -> String {
    match number.as_f64() {
        // `1.0` is "1" in JavaScript but "1.0" through serde_json.
        Some(value) if number.is_f64() && value.fract() == 0.0 && value.abs() < 1e15 => {
            format!("{value:.0}")
        }
        _ => number.to_string(),
    }
}

fn parse_number(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(integer) = trimmed.parse::<i64>() {
        return Some(Value::from(integer));
    }
    let float = trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    Number::from_f64(float).map(Value::Number)
}

/// The README's slug upgrader, as a step:
/// `String(x ?? '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'untitled'`.
fn slugify(source: &str) -> String {
    let mut slug = String::with_capacity(source.len());
    let mut pending_dash = false;
    for character in source.to_lowercase().chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "untitled".to_owned()
    } else {
        slug
    }
}
