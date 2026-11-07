use serde::{Deserialize, Serialize};

/// A signature over one policy version, by the key `key` names
/// (`fid1:p256:...`), base64.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSignature {
    /// The signer's identifier.
    pub key: String,
    /// ECDSA P-256, 64 bytes, base64.
    pub signature: String,
}
