use serde::{Deserialize, Serialize};

use crate::AccessSignature;

/// One policy version as stored: sealed with the catalog key, and signed
/// over its plain bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicyEnvelope {
    /// The policy's bytes sealed with the catalog key, base64.
    pub sealed: String,
    /// The signatures over the policy's bytes.
    pub signatures: Vec<AccessSignature>,
}
