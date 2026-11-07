use serde::{Deserialize, Serialize};

/// An identity a policy names: one or more public keys
/// (`fpk1:p256:...`), so a key can be rotated by adding the new one before
/// removing the old.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicyIdentity {
    /// The identity's public keys.
    pub keys: Vec<String>,
    /// A label for people, such as the application's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
