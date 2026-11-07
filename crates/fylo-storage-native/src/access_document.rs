use serde::{Deserialize, Serialize};

use crate::{AccessKeyWrap, AccessPolicyEnvelope};

/// What a protected database's access slots hold (ADR 0021): every policy
/// version, oldest first, and the keyring. The wraps are readable by
/// anyone; each opens only with its recipient's private key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessDocument {
    /// Always [`AccessDocument::FORMAT`].
    pub format: String,
    /// The policy versions, oldest first.
    pub policies: Vec<AccessPolicyEnvelope>,
    /// The key wraps.
    pub keyring: Vec<AccessKeyWrap>,
}

impl AccessDocument {
    /// The access document format this build reads and writes.
    pub const FORMAT: &'static str = "fylo.access/1";

    /// The wraps addressed to `recipient`.
    pub fn wraps_for<'a>(&'a self, recipient: &'a str) -> impl Iterator<Item = &'a AccessKeyWrap> {
        self.keyring
            .iter()
            .filter(move |wrap| wrap.recipient == recipient)
    }
}
