use fylo::{AccessTrust, IdentityId};
use serde::Deserialize;

use crate::call_failure::CallFailure;

/// `identity.trust` in the open options: `{"pinned": "fid1:p256:..."}` or
/// `"firstUse"` (ADR 0021 Q7).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum TrustSettings {
    Pinned(String),
    FirstUse,
}

impl TrustSettings {
    pub(crate) fn to_trust(&self) -> Result<AccessTrust, CallFailure> {
        match self {
            TrustSettings::Pinned(anchor) => Ok(AccessTrust::Pinned(IdentityId::parse(anchor)?)),
            TrustSettings::FirstUse => Ok(AccessTrust::FirstUse),
        }
    }
}
