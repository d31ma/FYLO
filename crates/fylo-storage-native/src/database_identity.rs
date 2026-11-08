use std::sync::Arc;

use fylo_identity::Identity;

use crate::AccessTrust;

/// The application identity a protected `.vdbx` database is opened with,
/// and how its policy is trusted (ADR 0021).
#[derive(Clone, Debug)]
pub struct DatabaseIdentity {
    identity: Arc<Identity>,
    trust: AccessTrust,
    session_keys: bool,
}

impl DatabaseIdentity {
    /// Open protected databases as `identity`, trusting their policy as
    /// `trust` says.
    #[must_use]
    pub fn new(identity: Identity, trust: AccessTrust) -> DatabaseIdentity {
        DatabaseIdentity {
            identity: Arc::new(identity),
            trust,
            session_keys: false,
        }
    }

    /// Sign records with a session key this identity certifies once per
    /// session (ADR 0021, phase 8), even when its key is a software key.
    /// A hardware identity (`enclave:`, `tpm:`) always does, since each of
    /// its own signatures takes milliseconds.
    #[must_use]
    pub fn with_session_keys(mut self, session_keys: bool) -> DatabaseIdentity {
        self.session_keys = session_keys;
        self
    }

    /// Open protected databases as an identity already held.
    pub(crate) fn shared(
        identity: Arc<Identity>,
        trust: AccessTrust,
        session_keys: bool,
    ) -> DatabaseIdentity {
        DatabaseIdentity {
            identity,
            trust,
            session_keys,
        }
    }

    /// Whether records are signed with session keys.
    #[must_use]
    pub fn uses_session_keys(&self) -> bool {
        self.session_keys || self.identity.is_hardware()
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
