use std::path::Path;
use std::sync::Arc;

use fylo_machine::ApplicationConfig;
use fylo_storage_native::{DatabaseAccess, NativeRoot, RootOpenOptions};
use serde_json::{Value, json};

/// `fylo access list <db.vdbx>` (ADR 0021): open a protected database with
/// the configured identity and print what it may do there, the trust anchor
/// that created the database, and the newest policy.
pub struct AccessCommand;

impl AccessCommand {
    const USAGE: &'static str = "Usage:\n  fylo access list <db.vdbx> --identity <reference> [--trust-anchor <fid1:...> | --trust-on-first-use]";

    /// Run `fylo access` with its positional arguments after `access`.
    ///
    /// # Errors
    /// The usage text for unknown arguments, `ENOIDENTITY` without an
    /// identity, the open's refusal, or a database that is not protected.
    pub fn run(positionals: &[String], application: &ApplicationConfig) -> Result<String, String> {
        let path = Self::database(positionals)?;
        let options = application.with_database_access(RootOpenOptions::new(path));
        let root = NativeRoot::open_profiled(options).map_err(|error| error.to_string())?;
        let access = Self::access(&root, path)?;
        serde_json::to_string_pretty(&Self::render(&access)).map_err(|error| error.to_string())
    }

    fn database(positionals: &[String]) -> Result<&Path, String> {
        match positionals {
            [command, path] if command == "list" => Ok(Path::new(path)),
            _ => Err(Self::USAGE.to_owned()),
        }
    }

    fn access(root: &NativeRoot, path: &Path) -> Result<Arc<DatabaseAccess>, String> {
        match root.database_access() {
            Some(Some(access)) => Ok(access),
            Some(None) => Err(fylo_storage_native::DatabaseAccessError::NoIdentity.to_string()),
            None => Err(format!(
                "ENATIVE_UNSUPPORTED: {} is not a protected .vdbx database",
                path.display()
            )),
        }
    }

    fn render(access: &DatabaseAccess) -> Value {
        let grants = access.grants();
        json!({
            "identity": access.identity().as_str(),
            "trustAnchor": access.anchor().as_str(),
            "trustedOnFirstUse": access.trusted_on_first_use(),
            "admin": grants.is_admin(),
            "grants": grants.collections(),
            "policy": access.policy(),
        })
    }
}
