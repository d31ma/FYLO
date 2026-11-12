use std::path::Path;

use fylo_identity::Identity;
use fylo_storage_native::{DatabaseKey, NativeRoot};

use crate::Error;

/// What kind of new `.vdbx` database [`DatabaseCreation::create`] makes
/// (ADR 0016, ADR 0021, ADR 0026).
#[derive(Debug)]
pub enum DatabaseCreation {
    /// Not encrypted and with no identities: anyone who can read the file
    /// reads it. Only when the application asks for it (ADR 0021 Q2).
    Plain,
    /// Encrypted, with `admin` as its admin and trust anchor, and every
    /// record signed (ADR 0021).
    Protected(Identity),
    /// Encrypted with a key the application holds, and no identities: the
    /// shared-key format of ADR 0016 section 9 (ADR 0026 section 9).
    Encrypted(DatabaseKey),
}

impl DatabaseCreation {
    /// Make the database at `path`. An existing file is never replaced.
    ///
    /// # Errors
    ///
    /// Fails when `path` exists or is not a `.vdbx` path, when the folder
    /// cannot be written, or when the admin's key cannot sign or wrap.
    pub fn create(&self, path: &Path) -> Result<(), Error> {
        match self {
            DatabaseCreation::Plain => NativeRoot::create_database(path)?,
            DatabaseCreation::Protected(admin) => {
                NativeRoot::create_protected_database(path, admin)?;
            }
            DatabaseCreation::Encrypted(key) => NativeRoot::create_encrypted_database(path, key)?,
        }
        Ok(())
    }
}
