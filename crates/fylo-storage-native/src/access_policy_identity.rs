use serde::Serialize;

/// An identity a policy names: one or more public keys
/// (`fpk1:p256:...`), so a key can be rotated by adding the new one before
/// removing the old.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AccessPolicyIdentity {
    /// The identity's public keys.
    pub keys: Vec<String>,
    /// A label for people, such as the application's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl AccessPolicyIdentity {
    /// An identity from the policy's JSON.
    pub(crate) fn from_value(value: &serde_json::Value) -> Result<AccessPolicyIdentity, String> {
        use crate::access_json::AccessJson;
        let map = AccessJson::object(value, "an identity")?;
        AccessJson::known_keys(map, &["keys", "label"], "an identity")?;
        Ok(AccessPolicyIdentity {
            keys: AccessJson::strings(map, "keys")?,
            label: AccessJson::optional_string(map, "label")?,
        })
    }
}
