use serde_json::{Map, Value};

/// Reads the access document and policy from parsed JSON, refusing unknown
/// keys as `deny_unknown_fields` would. Parsing through `serde_json::Value`
/// keeps typed deserializers out of the browser engine (ADR 0021, P5-5).
pub(crate) struct AccessJson;

impl AccessJson {
    pub(crate) fn object<'a>(
        value: &'a Value,
        name: &str,
    ) -> Result<&'a Map<String, Value>, String> {
        value
            .as_object()
            .ok_or_else(|| format!("{name} is not an object"))
    }

    pub(crate) fn known_keys(
        map: &Map<String, Value>,
        allowed: &[&str],
        name: &str,
    ) -> Result<(), String> {
        match map.keys().find(|key| !allowed.contains(&key.as_str())) {
            Some(key) => Err(format!("{name} has an unknown key {key:?}")),
            None => Ok(()),
        }
    }

    pub(crate) fn string(map: &Map<String, Value>, key: &str) -> Result<String, String> {
        map.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{key} must be a string"))
    }

    pub(crate) fn optional_string(
        map: &Map<String, Value>,
        key: &str,
    ) -> Result<Option<String>, String> {
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(_) => Self::string(map, key).map(Some),
        }
    }

    pub(crate) fn number(map: &Map<String, Value>, key: &str) -> Result<u64, String> {
        map.get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{key} must be a whole number"))
    }

    pub(crate) fn optional_u32(map: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(_) => u32::try_from(Self::number(map, key)?)
                .map(Some)
                .map_err(|_| format!("{key} is too large")),
        }
    }

    pub(crate) fn array<'a>(
        map: &'a Map<String, Value>,
        key: &str,
    ) -> Result<&'a Vec<Value>, String> {
        map.get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{key} must be an array"))
    }

    pub(crate) fn strings(map: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
        Self::array(map, key)?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} holds a non-string"))
            })
            .collect()
    }
}
