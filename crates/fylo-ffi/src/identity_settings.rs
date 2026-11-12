use fylo::{AccessTrust, Identity};
use serde::Deserialize;

use crate::call_failure::CallFailure;
use crate::trust_settings::TrustSettings;

/// `identity` in the open options: the key reference a protected database
/// opens with, and which admin it must have been created by.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentitySettings {
    reference: String,
    trust: TrustSettings,
}

impl IdentitySettings {
    pub(crate) fn open(&self) -> Result<(Identity, AccessTrust), CallFailure> {
        let identity = Identity::from_reference(&self.reference)?;
        Ok((identity, self.trust.to_trust()?))
    }
}
