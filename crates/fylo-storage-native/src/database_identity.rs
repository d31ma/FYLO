use std::sync::Arc;

use fylo_identity::Identity;

use crate::AccessTrust;

/// The application identity a protected `.vdbx` database is opened with,
/// and how its policy is trusted (ADR 0021).
#[derive(Clone, Debug)]
pub struct DatabaseIdentity {
    identity: Arc<Identity>,
    trust: AccessTrust,
}

impl DatabaseIdentity {
    /// Open protected databases as `identity`, trusting their policy as
    /// `trust` says.
    #[must_use]
    pub fn new(identity: Identity, trust: AccessTrust) -> DatabaseIdentity {
        DatabaseIdentity {
            identity: Arc::new(identity),
            trust,
        }
    }

    /// Open protected databases as an identity already held.
    pub(crate) fn shared(identity: Arc<Identity>, trust: AccessTrust) -> DatabaseIdentity {
        DatabaseIdentity { identity, trust }
    }

    /// The identity.
    #[must_use]
    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    /// How the policy is trusted.
    #[must_use]
    pub fn trust(&self) -> &AccessTrust {
        &self.trust
    }
}
