use std::path::Path;

use super::super::{NativeWriteRoot, failpoint};
use super::database_pack_report::DatabasePackReport;
use super::document_image::DocumentImage;
use super::record_access_migration::RecordAccessMigration;
use crate::segments::ContainerMedium;
use crate::{
    AccessGrantRequest, ContainerFile, DatabaseIdentity, NativeCollection, NativeRoot,
    NativeStorageError, NativeStorageErrorCode, RootOpenOptions,
};

/// `fylo pack --protect` (ADR 0021, phase 6): a directory root, or a
/// `.vdbx` database that is plain or encrypted with the shared key, copied
/// into a new protected database whose admin and trust anchor is the
/// identity given.
///
/// The copy is built as a staging file beside `out`, as `fylo pack` builds
/// one: every collection with its documents, deleted documents and schema
/// registry, its prefix index folded again from the records at the first
/// checkpoint, and every record signed by the admin. Each record's POSTIX
/// access descriptor is dropped or kept as metadata, as
/// [`RecordAccessMigration`] says; the grants are added as the next policy
/// versions. The staging file is then checked against the source with the
/// oracle, collection by collection, and as `fylo verify` checks it with
/// the admin's identity (structures and every signature), flushed, and only
/// then given its name. The source is never written, but by the recovery
/// any open runs, and every collection's lock is held from the first read
/// to the rename. A pack that stops leaves no `out`.
impl NativeWriteRoot {
    /// Pack this root into a new protected `.vdbx` database at `out`, which
    /// must not exist, with `admin` as its admin and trust anchor.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSUPPORTED` for a protected source or an existing
    /// `out`; `ELAYOUTUNSUPPORTED` for a directory root that uses what a
    /// database does not hold; `EKEY` for a shared-key database opened
    /// without its key; `EPOLICY` for a record with an access descriptor
    /// under [`RecordAccessMigration::Refuse`] or a refused grant; and an
    /// error for a copy that fails verification, lock contention or an I/O
    /// failure.
    pub fn pack_protected_database(
        &self,
        out: &Path,
        admin: &DatabaseIdentity,
        (record_access, grants): (RecordAccessMigration, &[AccessGrantRequest]),
    ) -> Result<DatabasePackReport, NativeStorageError> {
        self.require_protectable()?;
        let names = self.pack_names()?;
        let out = &Self::pack_output(out);
        Self::require_new_output(out)?;
        let collections = self.packed_collections(&names)?;
        let staging = Self::pack_staging(out, Self::PACK_STAGING_SUFFIX)?;
        Self::remove_staging(&staging)?;
        let report = self
            .stage_protected(&collections.1, &staging, admin, (record_access, grants))
            .inspect_err(|_| Self::abandon_staging(&staging))?;
        Self::adopt_staging(&staging, out)?;
        drop(collections);
        Ok(report)
    }

    /// Refuse a source `--protect` cannot take.
    fn require_protectable(&self) -> Result<(), NativeStorageError> {
        self.root.assert_profile_owned()?;
        Self::require_segments()?;
        match self.root.database() {
            Some(database) => Self::require_unprotected(database),
            None => self.require_packable(),
        }
    }

    fn require_segments() -> Result<(), NativeStorageError> {
        if NativeCollection::SEGMENTS_SUPPORTED {
            return Ok(());
        }
        Err(Self::pack_unsupported(
            "this build cannot write a .vdbx database",
        ))
    }

    fn require_unprotected(
        database: &crate::container_database::ContainerDatabase,
    ) -> Result<(), NativeStorageError> {
        if database.is_protected() {
            return Err(Self::pack_unsupported(
                "the source is a protected database already; fylo pack --protect takes a \
                 directory root or a plain or shared-key .vdbx database",
            ));
        }
        database.require_key()
    }

    /// Build, check and flush the staging file.
    fn stage_protected(
        &self,
        collections: &[NativeCollection],
        staging: &Path,
        admin: &DatabaseIdentity,
        (record_access, grants): (RecordAccessMigration, &[AccessGrantRequest]),
    ) -> Result<DatabasePackReport, NativeStorageError> {
        NativeRoot::create_protected_database(staging, admin.identity())?;
        let options = RootOpenOptions::new(staging).with_identity(admin.clone());
        let built = Self::open_profiled(options).and_then(|packed| {
            let report = self.copy_protected(&packed, collections, record_access)?;
            packed.grant_all(grants)?;
            Ok(report)
        });
        NativeRoot::close_database(staging)?;
        let report = built?;
        Self::verify_protected(staging, admin)?;
        Self::flush_staging(staging)?;
        failpoint("after-pack-staging")?;
        Ok(report)
    }

    fn copy_protected(
        &self,
        packed: &Self,
        collections: &[NativeCollection],
        record_access: RecordAccessMigration,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        let mut report = DatabasePackReport::default();
        for source in collections {
            let images = DocumentImage::read_collection(source)?;
            let images = record_access.apply(source.name(), images)?;
            report
                .collections
                .push(self.copy_collection(packed, source, &images, false)?);
        }
        Ok(report)
    }

    fn grant_all(&self, grants: &[AccessGrantRequest]) -> Result<(), NativeStorageError> {
        for grant in grants {
            self.grant_access(grant)?;
        }
        Ok(())
    }

    /// Check the staging file as `fylo verify` does with the admin's
    /// identity: its structures and every record's signature.
    fn verify_protected(
        staging: &Path,
        admin: &DatabaseIdentity,
    ) -> Result<(), NativeStorageError> {
        let mut file = ContainerFile::open_reader(staging).map_err(ContainerMedium::error)?;
        file.unlock_with_identity(admin)
            .map_err(ContainerMedium::error)?;
        let report = file.verify().map_err(ContainerMedium::error)?;
        if report.ok {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!(
                "the protected copy failed verification: {:?}",
                report.problems
            ),
        ))
    }

    /// Remove a staging file a failed build left. A staging file is never
    /// read; one this cannot remove is removed by the next pack.
    fn abandon_staging(staging: &Path) {
        let _ = Self::remove_staging(staging);
    }
}
