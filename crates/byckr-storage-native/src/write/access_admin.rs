//! `fylo access grant`, `revoke` and `settle` (ADR 0021): an admin of a
//! protected `.vdbx` database writes the next policy version and the
//! keyring it calls for, or finishes a pending catalog-key rotation.

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
            .change_access(|access, _| access.prepare_grant(request))
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
            .change_access(|access, next_lsns| access.prepare_revoke(id, next_lsns))
    }

    /// Finish a catalog-key rotation a revocation left pending, as this
    /// root's admin identity (ADR 0021 section 7): the directory is
    /// resealed under the current catalog key, the access document drops
    /// the previous key, and a `settle` policy version signed by the admin
    /// records it. Returns the policy version the rotation started at, or
    /// `None` when none was pending, and then nothing is written. Safe to
    /// run again after a stop at any point.
    ///
    /// # Errors
    ///
    /// As [`Self::grant_access`].
    pub fn settle_access(&self) -> Result<Option<u64>, NativeStorageError> {
        self.protected_database()?.settle_access()
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
