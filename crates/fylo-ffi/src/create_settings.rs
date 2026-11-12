use std::path::Path;

use fylo::{DatabaseCreation, Identity};
use serde::Deserialize;

use crate::call_failure::CallFailure;
use crate::create_mode::CreateMode;

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
    pub(crate) fn create_missing(&self, path: &Path) -> Result<Option<Identity>, CallFailure> {
        let creation = self.creation()?;
        Self::create_when_absent(path, &creation)?;
        Ok(Self::admin_of(creation))
    }

    fn creation(&self) -> Result<DatabaseCreation, CallFailure> {
        match self.mode {
            CreateMode::Plain => Ok(DatabaseCreation::Plain),
            CreateMode::Protected => Ok(DatabaseCreation::Protected(self.admin()?)),
        }
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
            DatabaseCreation::Plain => None,
        }
    }
}
