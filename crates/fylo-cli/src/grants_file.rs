use std::path::Path;

use fylo_storage_native::{AccessGrantRequest, AccessRule};
use serde_json::{Map, Value};

use crate::access_command::AccessCommand;

/// The grants `fylo pack --protect --grants <file.json>` adds to the new
/// database, each as `fylo access grant` would:
///
/// ```json
/// {"grants": [
///   {"grantee": "web", "key": "fpk1:p256:...", "actions": ["read"],
///    "collection": "users", "rule": "web-users", "label": "web app"}
/// ]}
/// ```
///
/// Each grant names exactly one of `"collection": "<name>"`,
/// `"allCollections": true` and `"database": true`; `key` is one key or a
/// list of keys; `rule` defaults to `<grantee>-<resource>`.
pub struct GrantsFile;

impl GrantsFile {
    const GRANTS: &'static str = "grants";
    const GRANTEE: &'static str = "grantee";
    const KEY: &'static str = "key";
    const ACTIONS: &'static str = "actions";
    const COLLECTION: &'static str = "collection";
    const ALL_COLLECTIONS: &'static str = "allCollections";
    const DATABASE: &'static str = "database";
    const RULE: &'static str = "rule";
    const LABEL: &'static str = "label";
    const FIELDS: [&'static str; 8] = [
        Self::GRANTEE,
        Self::KEY,
        Self::ACTIONS,
        Self::COLLECTION,
        Self::ALL_COLLECTIONS,
        Self::DATABASE,
        Self::RULE,
        Self::LABEL,
    ];

    /// Read the grants in the file at `path`.
    ///
    /// # Errors
    ///
    /// Returns the reason the file cannot be read or a grant is malformed.
    pub fn read(path: &Path) -> Result<Vec<AccessGrantRequest>, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read --grants {}: {error}", path.display()))?;
        let document: Value = serde_json::from_str(&text)
            .map_err(|error| format!("--grants {} is not JSON: {error}", path.display()))?;
        let grants = Self::list(&document)?;
        Self::requests(grants)
    }

    fn list(document: &Value) -> Result<&Vec<Value>, String> {
        document
            .get(Self::GRANTS)
            .and_then(Value::as_array)
            .ok_or_else(|| "--grants needs a JSON object with a \"grants\" list".to_owned())
    }

    fn requests(grants: &[Value]) -> Result<Vec<AccessGrantRequest>, String> {
        let mut requests = Vec::with_capacity(grants.len());
        for grant in grants {
            requests.push(Self::request(grant)?);
        }
        Ok(requests)
    }

    fn request(grant: &Value) -> Result<AccessGrantRequest, String> {
        let fields = Self::fields(grant)?;
        let grantee = Self::text(fields, Self::GRANTEE)?;
        let resource = Self::resource(fields)?;
        let id = Self::optional_text(fields, Self::RULE)?
            .map_or_else(|| format!("{grantee}-{resource}"), ToOwned::to_owned);
        Ok(AccessGrantRequest {
            keys: AccessCommand::keys(&Self::keys(fields)?)?,
            label: Self::optional_text(fields, Self::LABEL)?.map(ToOwned::to_owned),
            rule: AccessRule {
                id,
                identity: grantee.to_owned(),
                actions: AccessCommand::actions(&Self::strings(fields, Self::ACTIONS)?.join(","))?,
                resources: vec![resource],
            },
        })
    }

    fn fields(grant: &Value) -> Result<&Map<String, Value>, String> {
        let fields = grant
            .as_object()
            .ok_or_else(|| "each grant in --grants is a JSON object".to_owned())?;
        Self::require_known(fields)?;
        Ok(fields)
    }

    fn require_known(fields: &Map<String, Value>) -> Result<(), String> {
        match fields
            .keys()
            .find(|name| !Self::FIELDS.contains(&name.as_str()))
        {
            Some(name) => Err(format!(
                "a grant in --grants has the unknown field {name:?}"
            )),
            None => Ok(()),
        }
    }

    fn text<'a>(fields: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
        Self::optional_text(fields, name)?
            .ok_or_else(|| format!("a grant in --grants needs the text field {name:?}"))
    }

    fn optional_text<'a>(
        fields: &'a Map<String, Value>,
        name: &str,
    ) -> Result<Option<&'a str>, String> {
        match fields.get(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(_) => Err(format!("the field {name:?} of a grant in --grants is text")),
        }
    }

    fn keys(fields: &Map<String, Value>) -> Result<String, String> {
        match fields.get(Self::KEY) {
            Some(Value::String(key)) => Ok(key.clone()),
            Some(Value::Array(_)) => Ok(Self::strings(fields, Self::KEY)?.join(",")),
            _ => Err("a grant in --grants needs \"key\": one fpk1: key or a list of them".into()),
        }
    }

    fn strings(fields: &Map<String, Value>, name: &str) -> Result<Vec<String>, String> {
        let values = fields
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("a grant in --grants needs the list {name:?}"))?;
        let mut strings = Vec::with_capacity(values.len());
        for value in values {
            strings.push(Self::string(value, name)?);
        }
        Ok(strings)
    }

    fn string(value: &Value, name: &str) -> Result<String, String> {
        value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("the list {name:?} of a grant in --grants holds text only"))
    }

    fn resource(fields: &Map<String, Value>) -> Result<String, String> {
        let named = [
            Self::optional_text(fields, Self::COLLECTION)?.map(AccessRule::collection_resource),
            Self::flag(fields, Self::ALL_COLLECTIONS).then(|| AccessRule::collection_resource("*")),
            Self::flag(fields, Self::DATABASE).then(AccessRule::database_resource),
        ];
        Self::one(named.into_iter().flatten().collect())
    }

    fn flag(fields: &Map<String, Value>, name: &str) -> bool {
        fields.get(name) == Some(&Value::Bool(true))
    }

    fn one(mut resources: Vec<String>) -> Result<String, String> {
        if resources.len() == 1 {
            return Ok(resources.remove(0));
        }
        Err(
            "a grant in --grants names exactly one of \"collection\", \"allCollections\": true \
             and \"database\": true"
                .into(),
        )
    }
}
