use std::path::Path;
use std::sync::Arc;

use super::super::{NativeWriteRoot, failpoint};
use super::database_pack_report::DatabasePackReport;
use super::document_image::DocumentImage;
use super::segment_compactor::SegmentCompactor;
use crate::container_database::ContainerDatabase;
use crate::database_access::RekeyPlan;
use crate::segments::ContainerMedium;
use crate::{
    DatabaseAccess, DocumentRekey, NativeCollection, NativeRoot, NativeStorageError,
    NativeStorageErrorCode, RootOpenOptions,
};

/// `fylo rekey <db.vdbx>` and `fylo rekey <db.vdbx> --collection <name>`
/// for a protected database (ADR 0021 section 7), run by an admin.
///
/// Both write a new file, as the rekey of a shared-key database does, and
/// rename it over the database: rewriting in place would leave the old
/// ciphertext in the extents it frees, readable with a key a revoked
/// identity kept. The new file has the database's id and its policy chain,
/// sealed under the catalog key, followed by a `rekey` version signed by
/// the admin; every record is copied, signed again by the admin, and
/// sealed under its collection's key in the new file. Without
/// `--collection` the catalog key and every collection key are new; with
/// it only that collection's key is new, at the epoch after its newest,
/// and every other key carries over. A new collection key drops the old
/// epochs, so the engine's [`DocumentRekey`] writes each `$encrypted` value
/// of that collection again under the new field key, which also moves
/// values a protected database wrote under `fylo.encryption.key` before
/// per-collection field keys.
///
/// The commit point is the rename: a stop before it leaves the database
/// as it was, with a staging file the next pack or rekey of that name
/// removes, and running the rekey again does all of it again. The database
/// is held exclusively throughout, as the shared-key rekey holds it.
impl NativeWriteRoot {
    /// Rewrite this protected database under fresh keys, or, with
    /// `collection`, that collection under a new key, as the admin
    /// identity it was opened with. Returns what was copied.
    ///
    /// # Errors
    ///
    /// `ENATIVE_UNSUPPORTED` for a directory root, a database that is not
    /// protected or a sealed one; `ENOIDENTITY` for one opened without an
    /// identity; `EACCES` for an identity that is not an admin; `EPOLICY`
    /// for a collection the database does not have; `EROOTLOCKED` and
    /// `EROOTLEASELOST` as the shared-key rekey returns them; and an error
    /// for a value `rekey` cannot write again, a copy that fails
    /// verification, or an I/O failure. Every failure leaves the database
    /// as it was.
    pub fn rekey_protected_database(
        &self,
        collection: Option<&str>,
        rekey: &dyn DocumentRekey,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let database = self.require_database("fylo rekey <db.vdbx>")?;
        let access = Self::admin_access(&database)?;
        let path = self.root.path().to_owned();
        let outcome = database.exclusively(|| {
            let writer = database.hold_writer()?;
            let names = database.collection_names();
            let collections = self.packed_collections(&names)?;
            let plan = access
                .plan_rekey(collection)
                .map_err(|error| Self::access_error(&error))?;
            let staging = Self::pack_staging(&path, Self::PACK_STAGING_SUFFIX)?;
            Self::remove_staging(&staging)?;
            let report = Self::renewing(&database, true, || {
                self.stage_rekeyed(&collections.1, &staging, (&plan, &access), rekey)
            })
            .inspect_err(|_| Self::abandon_staging(&staging))?;
            Self::replace_database(&database, Some(writer), (&staging, &path))?;
            drop(collections);
            Ok(report)
        });
        // The next open in this process reads the file at the path afresh.
        let closed = NativeRoot::close_database(&path);
        let report = outcome?;
        closed?;
        Ok(report)
    }

    /// The access of a protected, unsealed database opened by an admin.
    fn admin_access(
        database: &ContainerDatabase,
    ) -> Result<Arc<DatabaseAccess>, NativeStorageError> {
        if !database.is_protected() || database.is_sealed() {
            return Err(Self::pack_unsupported(
                "this fylo rekey rewrites a protected .vdbx database that is not sealed; a \
                 sealed one is read-only, so rekey its source and pack it again",
            ));
        }
        let access = database
            .access()
            .ok_or(crate::DatabaseAccessError::NoIdentity)
            .map_err(|error| Self::access_error(&error))?;
        access
            .require_admin()
            .map_err(|error| Self::access_error(&error))?;
        Ok(access)
    }

    fn access_error(error: &crate::DatabaseAccessError) -> NativeStorageError {
        NativeStorageError::new(
            ContainerMedium::access_code(error),
            format!("{error} ({})", error.code()),
        )
    }

    /// Build, check and flush the new file at `staging`.
    fn stage_rekeyed(
        &self,
        collections: &[NativeCollection],
        staging: &Path,
        (plan, access): (&RekeyPlan, &DatabaseAccess),
        rekey: &dyn DocumentRekey,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        ContainerDatabase::create_rekeyed(staging, plan)?;
        let identity = access.pinned_identity();
        let options = RootOpenOptions::new(staging).with_identity(identity.clone());
        let built = Self::open_profiled(options).and_then(|packed| {
            packed.inherit_keys(plan)?;
            let mut report = DatabasePackReport::default();
            for source in collections {
                let rotated = plan.rotated.iter().any(|name| name == source.name());
                report
                    .collections
                    .push(self.copy_rekeyed(&packed, source, rotated, rekey)?);
            }
            Ok(report)
        });
        NativeRoot::close_database(staging)?;
        let report = built?;
        Self::verify_protected(staging, &identity)?;
        Self::flush_staging(staging)?;
        failpoint("after-pack-staging")?;
        Ok(report)
    }

    /// Hand the plan's keys to the new file, which gives them to each
    /// collection as it adds it.
    fn inherit_keys(&self, plan: &RekeyPlan) -> Result<(), NativeStorageError> {
        let access = self
            .root
            .database_access()
            .flatten()
            .ok_or(crate::DatabaseAccessError::NoIdentity)
            .map_err(|error| Self::access_error(&error))?;
        access.inherit(plan.keys.clone());
        Ok(())
    }

    /// Copy `source` into `packed`, writing its `$encrypted` values again
    /// under its new field key when its key is `rotated`.
    fn copy_rekeyed(
        &self,
        packed: &Self,
        source: &NativeCollection,
        rotated: bool,
        rekey: &dyn DocumentRekey,
    ) -> Result<super::database_pack_report::PackedCollection, NativeStorageError> {
        let versions = self.add_packed_collection(packed, source)?;
        let mut images = DocumentImage::read_collection(source)?;
        if rotated {
            Self::rekey_images(packed, source.name(), &mut images, rekey)?;
        }
        Self::fill_packed_collection(packed, source.name(), (&images, &versions), false)
    }

    fn rekey_images(
        packed: &Self,
        name: &str,
        images: &mut [DocumentImage],
        rekey: &dyn DocumentRekey,
    ) -> Result<(), NativeStorageError> {
        let target = packed.root.field_keys(name)?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "the rekeyed copy is not a protected database",
            )
        })?;
        for image in images {
            image.body = rekey
                .rekey_document(name, &image.body, &target)
                .map_err(|reason| {
                    NativeStorageError::codec_rejected(format!(
                        "document {} of collection {name} could not be rekeyed: {reason}",
                        image.id
                    ))
                })?;
        }
        Ok(())
    }
}
