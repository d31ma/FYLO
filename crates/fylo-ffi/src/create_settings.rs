use std::path::Path;

use fylo::{DatabaseCreation, DatabaseKey, Identity};
use serde::Deserialize;

use crate::call_failure::CallFailure;
use crate::create_mode::CreateMode;
use crate::database_key_argument::DatabaseKeyArgument;

/// `create` in the open options: make the database when it does not exist.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateSettings {
    mode: CreateMode,
    admin: Option<String>,
}

impl CreateSettings {
    /// Make the database at `path` unless something is already there.
    /// Returns the admin of a database it made protected.
    pub(crate) fn create_missing(
        &self,
        path: &Path,
        key: Option<&DatabaseKey>,
    ) -> Result<Option<Identity>, CallFailure> {
        let creation = self.creation(key)?;
        Self::create_when_absent(path, &creation)?;
        Ok(Self::admin_of(creation))
    }

    /// Whether this mode makes a database protected by an identity.
    pub(crate) fn is_protected(&self) -> bool {
        self.mode == CreateMode::Protected
    }

    fn creation(&self, key: Option<&DatabaseKey>) -> Result<DatabaseCreation, CallFailure> {
        match self.mode {
            CreateMode::Plain => Self::plain(key),
            CreateMode::Protected => Ok(DatabaseCreation::Protected(self.admin()?)),
            CreateMode::Encrypted => Self::encrypted(key),
        }
    }

    fn plain(key: Option<&DatabaseKey>) -> Result<DatabaseCreation, CallFailure> {
        if key.is_some() {
            return Err(CallFailure::bad_request(
                "create.mode \"plain\" takes no encryption key; use \"encrypted\"",
            ));
        }
        Ok(DatabaseCreation::Plain)
    }

    fn encrypted(key: Option<&DatabaseKey>) -> Result<DatabaseCreation, CallFailure> {
        let key = key.ok_or_else(|| {
            DatabaseKeyArgument::missing("create.mode \"encrypted\" needs an encryption key")
        })?;
        Ok(DatabaseCreation::Encrypted(key.clone()))
    }

    fn admin(&self) -> Result<Identity, CallFailure> {
        let reference = self.admin.as_deref().ok_or_else(|| {
            CallFailure::bad_request(
                "create.mode \"protected\" needs create.admin, a key reference",
            )
        })?;
        Ok(Identity::from_reference(reference)?)
    }

    fn create_when_absent(path: &Path, creation: &DatabaseCreation) -> Result<(), CallFailure> {
        if path.exists() {
            return Ok(());
        }
        Ok(creation.create(path)?)
    }

    fn admin_of(creation: DatabaseCreation) -> Option<Identity> {
        match creation {
            DatabaseCreation::Protected(admin) => Some(admin),
            DatabaseCreation::Plain | DatabaseCreation::Encrypted(_) => None,
        }
    }
}
