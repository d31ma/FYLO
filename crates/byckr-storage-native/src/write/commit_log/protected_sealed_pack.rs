use std::path::Path;

use super::super::{NativeWriteRoot, failpoint};
use super::database_pack_report::DatabasePackReport;
use super::document_image::DocumentImage;
use crate::database_access::{RekeyPlan, RekeyScope};
use crate::{
    DatabaseAccess, DatabaseIdentity, DocumentRekey, FieldKeys, NativeCollection, NativeRoot,
    NativeStorageError, RootOpenOptions,
};

/// `fylo pack <db.vdbx> --out <copy.vdbx> --sealed` of a protected
/// database (ADR 0021 section 7), run by an admin.
///
/// The copy is built as the new file of a protected `fylo rekey` is, with
/// every key and the policy chain as they are and no new policy version:
/// the same database id, catalog key and collection key epochs, every
/// record signed again by the admin. It is then written as a sealed file
/// whose blocks are not encrypted again: its units stay sealed under the
/// catalog and collection keys, so each identity reads only what it was
/// granted, as in the source. The sealed file is checked against the
/// source with the oracle and as `fylo verify` checks it with the admin's
/// identity, flushed, and only then named; a pack that stops leaves no
/// output.
impl NativeWriteRoot {
    pub(super) fn pack_sealed_protected(
        &self,
        out: &Path,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        let database = self.require_database("fylo pack --sealed")?;
        let access = Self::admin_access(&database)?;
        let names = self.pack_names()?;
        let out = &Self::pack_output(out);
        Self::require_new_output(out)?;
        let collections = self.packed_collections(&names)?;
        let plan = access
            .plan_rekey(RekeyScope::Nothing)
            .map_err(|error| Self::access_error(&error))?;
        let staging = Self::pack_staging(out, Self::PACK_STAGING_SUFFIX)?;
        let sealing = Self::pack_staging(out, Self::SEAL_STAGING_SUFFIX)?;
        Self::remove_staging(&staging)?;
        Self::remove_staging(&sealing)?;
        let report = self
            .seal_protected(&collections.1, (&staging, &sealing), (&plan, &access))
            .inspect_err(|_| {
                Self::abandon_staging(&staging);
                Self::abandon_staging(&sealing);
            })?;
        Self::adopt_staging(&sealing, out)?;
        drop(collections);
        Ok(report)
    }

    fn seal_protected(
        &self,
        collections: &[NativeCollection],
        (staging, sealing): (&Path, &Path),
        (plan, access): (&RekeyPlan, &DatabaseAccess),
    ) -> Result<DatabasePackReport, NativeStorageError> {
        let report = self.stage_rekeyed(collections, staging, (plan, access), &KeptFields)?;
        let identity = access.pinned_identity();
        let blocks = Self::write_sealed_protected(staging, sealing, &identity)?;
        Self::remove_staging(staging)?;
        Self::require_sealed_copy(collections, sealing, &identity)?;
        Self::verify_protected(sealing, &identity)?;
        Self::flush_staging(sealing)?;
        failpoint("after-pack-staging")?;
        Ok(DatabasePackReport {
            sealed: Some(blocks),
            ..report
        })
    }

    #[cfg(not(target_family = "wasm"))]
    fn write_sealed_protected(
        staging: &Path,
        sealing: &Path,
        identity: &DatabaseIdentity,
    ) -> Result<crate::ContainerSealedReport, NativeStorageError> {
        let mut file = crate::ContainerFile::open_reader(staging)
            .map_err(crate::segments::ContainerMedium::error)?;
        file.unlock_with_identity(identity)
            .map_err(crate::segments::ContainerMedium::error)?;
        file.write_sealed(sealing, None)
            .map_err(crate::segments::ContainerMedium::error)
    }

    /// The browser carries no zstd compressor, so it writes no sealed file.
    #[cfg(target_family = "wasm")]
    fn write_sealed_protected(
        _staging: &Path,
        _sealing: &Path,
        _identity: &DatabaseIdentity,
    ) -> Result<crate::ContainerSealedReport, NativeStorageError> {
        Err(Self::pack_unsupported(
            "this build cannot write a sealed .vdbx database",
        ))
    }

    /// Check every collection the sealed file holds against `collections`,
    /// the source, reading it through its blocks as the admin.
    fn require_sealed_copy(
        collections: &[NativeCollection],
        sealing: &Path,
        identity: &DatabaseIdentity,
    ) -> Result<(), NativeStorageError> {
        let options = RootOpenOptions::new(sealing).with_identity(identity.clone());
        let outcome = NativeRoot::open_profiled(options).and_then(|packed| {
            for source in collections {
                let images = DocumentImage::read_collection(source)?;
                let copy = packed.collection(source.name())?;
                DocumentImage::require_same(&images, &DocumentImage::read_collection(&copy)?)?;
            }
            Ok(())
        });
        NativeRoot::close_database(sealing)?;
        outcome
    }
}

/// A sealed pack renews no key, so no `$encrypted` value is written again.
struct KeptFields;

impl DocumentRekey for KeptFields {
    fn rekey_document(
        &self,
        _collection: &str,
        body: &[u8],
        _target: &FieldKeys,
    ) -> Result<Vec<u8>, String> {
        Ok(body.to_vec())
    }
}
