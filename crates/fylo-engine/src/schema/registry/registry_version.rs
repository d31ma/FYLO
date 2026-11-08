use std::fmt::Write;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// One committed schema version, as the registry stores it (ADR 0017
/// section 3). Versions are immutable: a later change is a new ordinal.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RegistryVersion {
    pub(crate) format: String,
    /// 1, 2, 3, … in commit order.
    pub(crate) ordinal: u32,
    /// The `_v` value documents written at this version carry.
    pub(crate) label: String,
    /// The CHEX schema, `$encrypted` included, in the order it was authored.
    pub(crate) chex: Value,
    /// SHA-256 of `chex` in canonical form; see [`Self::digest`].
    pub(crate) sha256: String,
    /// `{"steps": [...]}` from the previous ordinal; absent for ordinal 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) migration: Option<Value>,
    /// The compiled validator of ADR 0017 section 4: the `fylo-chex`
    /// envelope of the schema's leaf DFAs, base64url without padding. It is
    /// derived from `chex` at push; a native build recompiles `chex` at open
    /// instead of trusting these bytes, which are for builds without a regex
    /// compiler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) compiled: Option<String>,
    /// Whether every leaf has a DFA, so that a build without a regex compiler
    /// can validate this version from `compiled` (ADR 0017 section 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) portable: Option<bool>,
    pub(crate) committed: Committed,
}

/// When and by what a version was committed.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Committed {
    pub(crate) at: String,
    pub(crate) actor: String,
}

impl RegistryVersion {
    pub(crate) const FORMAT: &'static str = "fylo.schema-version.v1";

    /// SHA-256 of `chex` as canonical JSON: object keys sorted by their
    /// UTF-16 code units at every level, no insignificant whitespace. Two
    /// spellings of one schema, reordered or reformatted, share a digest, so
    /// only a change of meaning is a conflict.
    pub(crate) fn digest(chex: &Value) -> String {
        let mut canonical = String::new();
        Self::write_canonical(chex, &mut canonical);
        let mut encoded = String::with_capacity(64);
        for byte in Sha256::digest(canonical.as_bytes()) {
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }

    fn write_canonical(value: &Value, output: &mut String) {
        match value {
            Value::Object(fields) => {
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
                output.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    output.push_str(&Value::String(key.clone()).to_string());
                    output.push(':');
                    Self::write_canonical(&fields[key], output);
                }
                output.push('}');
            }
            Value::Array(items) => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    Self::write_canonical(item, output);
                }
                output.push(']');
            }
            scalar => output.push_str(&scalar.to_string()),
        }
    }
}
