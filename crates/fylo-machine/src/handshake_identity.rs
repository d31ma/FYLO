use std::path::Path;

use fylo_storage_native::{
    AccessTrust, DatabaseAccess, DatabaseIdentity, NativeRoot, NativeStorageError, RootOpenOptions,
};
use serde_json::{Value, json};

use crate::ApplicationConfig;

/// The handshake's `identity`: the application identity a session opens
/// protected databases with, and what it may do in the session's root when
/// that is a protected `.vdbx` database (ADR 0021, phase 6).
pub(crate) struct HandshakeIdentity;

impl HandshakeIdentity {
    const PINNED: &'static str = "pinned";
    const FIRST_USE: &'static str = "firstUse";
    const UNPINNED: &'static str = "unpinned";

    /// `null` without an identity; otherwise the identity, its trust
    /// setting, and the grants it holds in `root`.
    pub(crate) fn report(config: &ApplicationConfig, root: Option<&Path>) -> Value {
        config.identity.as_ref().map_or(Value::Null, |identity| {
            Self::identity(config, identity, root)
        })
    }

    fn identity(
        config: &ApplicationConfig,
        identity: &DatabaseIdentity,
        root: Option<&Path>,
    ) -> Value {
        let mut report = json!({
            "id": identity.identity().id().as_str(),
            "publicKey": identity.identity().public().to_string(),
            "trust": Self::trust(identity.trust()),
            "database": Self::database(config, root),
        });
        if let AccessTrust::Pinned(anchor) = identity.trust() {
            report["trustAnchor"] = json!(anchor.as_str());
        }
        report
    }

    fn trust(trust: &AccessTrust) -> &'static str {
        match trust {
            AccessTrust::Pinned(_) => Self::PINNED,
            AccessTrust::FirstUse => Self::FIRST_USE,
            AccessTrust::Unpinned => Self::UNPINNED,
        }
    }

    /// The grants in `root`, when it is an existing `.vdbx` file: never
    /// creates one, and opens it read-only.
    fn database(config: &ApplicationConfig, root: Option<&Path>) -> Value {
        match root.filter(|root| root.is_file()) {
            Some(root) => Self::opened(config, root),
            None => Value::Null,
        }
    }

    fn opened(config: &ApplicationConfig, root: &Path) -> Value {
        let options = config.with_database_access(RootOpenOptions::new(root));
        match NativeRoot::open_profiled(options) {
            Ok(opened) => Self::grants(&opened),
            Err(error) => Self::refused(&error),
        }
    }

    fn grants(root: &NativeRoot) -> Value {
        match root.database_access() {
            Some(Some(access)) => Self::access(&access),
            _ => Value::Null,
        }
    }

    fn access(access: &DatabaseAccess) -> Value {
        let grants = access.grants();
        json!({
            "trustAnchor": access.anchor().as_str(),
            "trustedOnFirstUse": access.trusted_on_first_use(),
            "admin": grants.is_admin(),
            "grants": grants.collections(),
        })
    }

    fn refused(error: &NativeStorageError) -> Value {
        json!({
            "error": {
                "code": error.code().as_str(),
                "message": error.to_string(),
            }
        })
    }
}
