//! `fylo access grant` and `fylo access revoke` (ADR 0021): an admin of a
//! protected `.vdbx` database writes the next policy version and the
//! keyring it calls for.

use crate::{
    AccessGrantRequest, AccessPolicy, NativeStorageError, NativeStorageErrorCode, NativeWriteRoot,
};

impl NativeWriteRoot {
    /// Add `request`'s rule as the next policy version, signed by the
    /// identity this root was opened with, which must be an admin. The
    /// identity the rule names gets the catalog key and every key of each
    /// collection the rule covers. Returns the new policy.
    ///
    /// # Errors
    ///
    /// `ENATIVE_UNSUPPORTED` for a root that is not a protected database,
    /// `ENOIDENTITY` without an identity, `EACCES` for an identity that is
    /// not an admin, `EPOLICY` for a rule id in use or a malformed rule, and
    /// the write failure.
    pub fn grant_access(
        &self,
        request: &AccessGrantRequest,
    ) -> Result<AccessPolicy, NativeStorageError> {
        self.protected_database()?
            .change_access(|access| access.prepare_grant(request))
    }

    /// Remove the rule `id` as the next policy version, signed by this
    /// root's admin identity. Every collection the rule covered starts a
    /// new key epoch. Returns the new policy.
    ///
    /// # Errors
    ///
    /// As [`Self::grant_access`], and `EPOLICY` when no rule has that id or
    /// the database would be left with no admin.
    pub fn revoke_access(&self, id: &str) -> Result<AccessPolicy, NativeStorageError> {
        self.protected_database()?
            .change_access(|access| access.prepare_revoke(id))
    }

    fn protected_database(
        &self,
    ) -> Result<std::sync::Arc<crate::container_database::ContainerDatabase>, NativeStorageError>
    {
        self.root
            .database()
            .filter(|database| database.is_protected())
            .cloned()
            .ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::Unsupported,
                    "access rules belong to a protected .vdbx database (fylo create --admin)",
                )
            })
    }
}
