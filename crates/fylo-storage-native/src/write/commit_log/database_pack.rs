use std::path::{Path, PathBuf};

use fylo_format::LogRecord;
use serde_json::{Map, Value};

use super::super::{
    GenerationRecord, NativeWriteRoot, durable_replace, failpoint, json_error, remove_dir_durable,
    sync_parent,
};
use super::database_pack_report::{DatabasePackReport, PackedCollection};
use super::document_image::DocumentImage;
use super::file_export::FileExport;
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_import::SegmentImport;
use crate::segments::LocationState;
use crate::{
    DICTIONARY_TRAINING_BYTES, DatabaseKey, FilesystemProfile, NativeCollection, NativeRoot,
    NativeStorageError, NativeStorageErrorCode, RootOpenOptions, path_exists_no_follow,
};

/// Converts between a directory root and a `.fxdb` database (ADR 0016
/// section 11).
///
/// `pack` copies every document collection of a directory root, in the
/// file or the segment layout, into a new database, the way a layout
/// migration copies one collection (ADR 0015 section 9). The database is
/// built as a staging file beside `--out` and checked against the source
/// with the full oracle: the same live and deleted identifiers, for every
/// document the same bytes, attributes, mode and modification time, and the
/// same schema registry. Only then is it flushed and given its name, which
/// is the commit point. Every collection's lock is held from the first read
/// to that point, so nothing writes the source meanwhile. A pack that stops
/// leaves no `--out`, only the staging file, which the next pack to the same
/// name removes.
///
/// `unpack` writes a database's collections as a new directory root in the
/// file layout: for each, the tree `fylo export` writes, its catalog
/// descriptor and its schema registry.
impl NativeWriteRoot {
    const PACK_STAGING_SUFFIX: &'static str = ".pack-staging";

    /// Pack this directory root into a new `.fxdb` database at `out`, which
    /// must not exist.
    ///
    /// `allow_engine_only_access` must be set to pack records that name
    /// another owner, whose access descriptors a database enforces in FYLO
    /// only, as a segment collection does (ADR 0015 section 10).
    ///
    /// # Errors
    ///
    /// Returns `ELAYOUTUNSUPPORTED` for a root that uses buckets, version
    /// control or the embedded queue, or the EFS sidecar profile;
    /// `ENATIVE_UNSUPPORTED` for a database root or an existing `out`; and
    /// an error for a copy that fails verification, lock contention or an
    /// I/O failure. Only recovery writes the source, as before an export,
    /// and a failed pack leaves no `out`.
    pub fn pack_database(
        &self,
        out: &Path,
        allow_engine_only_access: bool,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        self.pack_with(out, allow_engine_only_access, None)
    }

    /// [`Self::pack_database`], into a database encrypted with `key`
    /// (ADR 0016 section 9), which opens with the same key.
    ///
    /// # Errors
    ///
    /// The same as [`Self::pack_database`].
    pub fn pack_encrypted_database(
        &self,
        out: &Path,
        allow_engine_only_access: bool,
        key: &DatabaseKey,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        self.pack_with(out, allow_engine_only_access, Some(key))
    }

    fn pack_with(
        &self,
        out: &Path,
        allow_engine_only_access: bool,
        key: Option<&DatabaseKey>,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(Self::pack_unsupported(
                "this build cannot write a .fxdb database",
            ));
        }
        self.root.assert_profile_owned()?;
        if self.root.is_database() {
            return Err(Self::pack_unsupported(
                "fylo pack takes a directory root, and this root is already a .fxdb database",
            ));
        }
        self.require_packable()?;
        let names = self.packed_collection_names()?;
        // A bare file name lives in the working directory, which the commit
        // flushes.
        let out = &match out.parent() {
            Some(parent) if parent.as_os_str().is_empty() => Path::new(".").join(out),
            _ => out.to_owned(),
        };
        Self::require_new_output(out)?;
        let staging = Self::pack_staging(out)?;
        let mut locks = Vec::with_capacity(names.len());
        let mut collections = Vec::with_capacity(names.len());
        for name in &names {
            let collection = self.root.collection(name)?;
            locks.push(self.lock_collection(&collection)?);
            self.recover_locked(&collection)?;
            // Recovery may have moved the descriptor's schema head.
            collections.push(self.root.collection(name)?);
        }
        Self::remove_staging(&staging)?;
        let built = self
            .build_pack(&collections, &staging, (allow_engine_only_access, key))
            .and_then(|report| Self::flush_staging(&staging).map(|()| report))
            .and_then(|report| failpoint("after-pack-staging").map(|()| report));
        let report = match built {
            Ok(report) => report,
            Err(error) => {
                // The staging file is never read; one this cannot remove is
                // removed by the next pack.
                let _ = Self::remove_staging(&staging);
                return Err(error);
            }
        };
        Self::adopt_staging(&staging, out)?;
        drop(locks);
        Ok(report)
    }

    /// Write this `.fxdb` database's collections as a new directory root in
    /// the file layout at `out`, which must be absent or empty. Only
    /// recovery writes the database, as before an export.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSUPPORTED` for a directory root or a non-empty
    /// `out`, and an error for a copy that fails verification, lock
    /// contention or an I/O failure. What a failed unpack wrote is removed.
    pub fn unpack_database(&self, out: &Path) -> Result<DatabasePackReport, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let Some(names) = self.root.database_collection_names() else {
            return Err(Self::pack_unsupported(
                "fylo unpack takes a .fxdb database, and this root is a directory",
            ));
        };
        // A locked database lists no collections; unpacking it would write
        // an empty root, not its documents.
        if let Some(database) = self.root.database() {
            database.require_key()?;
        }
        let unpacked = Self::writing(Self::empty_output(out)?);
        let outcome = self.unpack_into(&unpacked, &names);
        if outcome.is_err() {
            // `out` was empty, so everything in it is this unpack's.
            let _ = Self::clear_output(unpacked.root.path());
        }
        outcome
    }

    fn unpack_into(
        &self,
        unpacked: &Self,
        names: &[String],
    ) -> Result<DatabasePackReport, NativeStorageError> {
        let mut locks = Vec::with_capacity(names.len());
        let mut collections = Vec::with_capacity(names.len());
        for name in names {
            let collection = self.root.collection(name)?;
            locks.push(self.lock_collection(&collection)?);
            self.recover_locked(&collection)?;
            collections.push(collection);
        }
        let mut report = DatabasePackReport::default();
        for collection in &collections {
            report
                .collections
                .push(self.unpack_collection(unpacked, collection)?);
        }
        Ok(report)
    }

    /// Refuse a root that uses what a `.fxdb` database does not hold yet
    /// (ADR 0016 section 1), naming it.
    fn require_packable(&self) -> Result<(), NativeStorageError> {
        if self.filesystem_profile() != FilesystemProfile::Native {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root on the EFS sidecar profile, which has no .fxdb form",
            ));
        }
        if path_exists_no_follow(&self.root.repository_root().join(".fylo-vcs"))? {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root that uses document version control, which a .fxdb database does not hold",
            ));
        }
        if Self::first_entry(&self.root.path().join(".fylo-queue"))?.is_some() {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root that uses the embedded queue, which a .fxdb database does not hold",
            ));
        }
        if let Some(bucket) = Self::first_entry(&self.root.path().join(".buckets"))? {
            return Err(NativeRoot::database_unsupported(&format!(
                "cannot pack a root that uses buckets (raw-file collections), which a .fxdb database does not hold: bucket \"{bucket}\""
            )));
        }
        Ok(())
    }

    /// The name of one entry of `directory`, or `None` when it is absent or
    /// empty.
    fn first_entry(directory: &Path) -> Result<Option<String>, NativeStorageError> {
        if !path_exists_no_follow(directory)? {
            return Ok(None);
        }
        let mut entries = fylo_vfs::read_dir(directory).map_err(NativeStorageError::io)?;
        entries
            .next()
            .transpose()
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .map_err(NativeStorageError::io)
    }

    /// The document collections of this directory root, by name. A
    /// collection exists when its directory does.
    fn packed_collection_names(&self) -> Result<Vec<String>, NativeStorageError> {
        let directory = self.root.path().join(".collections");
        if !path_exists_no_follow(&directory)? {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fylo_vfs::read_dir(&directory).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            let kind = entry.file_type().map_err(NativeStorageError::io)?;
            let name = entry.file_name().to_str().map(ToOwned::to_owned);
            let Some(name) = name.filter(|_| kind.is_dir() && !kind.is_symlink()) else {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::UnsafePath,
                    format!(
                        "collection entry {} is not a directory",
                        entry.path().display()
                    ),
                ));
            };
            crate::validate_collection_name(&name)?;
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    fn require_new_output(out: &Path) -> Result<(), NativeStorageError> {
        if path_exists_no_follow(out)? {
            return Err(Self::pack_unsupported(&format!(
                "output {} already exists; fylo pack never replaces a file",
                out.display()
            )));
        }
        Ok(())
    }

    /// The staging file of a pack to `out`: a hidden sibling, so the rename
    /// that commits it stays in one directory.
    fn pack_staging(out: &Path) -> Result<PathBuf, NativeStorageError> {
        let name = out.file_name().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                format!("output {} names no file", out.display()),
            )
        })?;
        let parent = out.parent().unwrap_or_else(|| Path::new("."));
        Ok(parent.join(format!(
            ".{}{}",
            name.to_string_lossy(),
            Self::PACK_STAGING_SUFFIX
        )))
    }

    /// Remove a staging file and the lease files beside it, and forget this
    /// process's open of it, as a stopped pack leaves them.
    fn remove_staging(staging: &Path) -> Result<(), NativeStorageError> {
        if path_exists_no_follow(staging)? {
            let _ = NativeRoot::close_database(staging);
            fylo_vfs::remove_file(staging).map_err(NativeStorageError::io)?;
        }
        let (Some(parent), Some(name)) = (staging.parent(), staging.file_name()) else {
            return Ok(());
        };
        let leases = format!("{}.lease.", name.to_string_lossy());
        for entry in fylo_vfs::read_dir(parent).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if entry.file_name().to_string_lossy().starts_with(&leases) {
                fylo_vfs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
            }
        }
        Ok(())
    }

    /// Build the database in `staging` and verify every collection of it.
    /// This process's open of the file is closed before it returns, so the
    /// file is renamed with no writer holding it.
    fn build_pack(
        &self,
        collections: &[NativeCollection],
        staging: &Path,
        (allow_engine_only_access, key): (bool, Option<&DatabaseKey>),
    ) -> Result<DatabasePackReport, NativeStorageError> {
        let mut options = RootOpenOptions::new(staging);
        match key {
            Some(key) => {
                NativeRoot::create_encrypted_database(staging, key)?;
                options = options.with_database_key(key.clone());
            }
            None => NativeRoot::create_database(staging)?,
        }
        let outcome = Self::open_profiled(options).and_then(|packed| {
            let mut report = DatabasePackReport::default();
            for collection in collections {
                report.collections.push(self.pack_collection(
                    &packed,
                    collection,
                    allow_engine_only_access,
                )?);
            }
            Ok(report)
        });
        NativeRoot::close_database(staging)?;
        outcome
    }

    /// Copy one collection into the database `packed`, then check the copy
    /// against `images`, the source's documents.
    fn pack_collection(
        &self,
        packed: &Self,
        source: &NativeCollection,
        allow_engine_only_access: bool,
    ) -> Result<PackedCollection, NativeStorageError> {
        let images = DocumentImage::read_collection(source)?;
        if !allow_engine_only_access {
            Self::require_owner_only_access(source, &images)?;
        }
        let versions = self.registry_versions(source)?;
        let mut descriptor = Self::database_descriptor(source.shard_width());
        if let Some(mode) = source.validation() {
            descriptor.insert("validation".into(), Value::from(mode.as_str()));
        }
        packed.add_database_collection(source.name(), descriptor)?;
        let collection = packed.root.collection(source.name())?;
        let _lock = packed.lock_collection(&collection)?;
        Self::import_records(&collection, (&images, &versions))?;
        let medium = collection.segment_medium()?;
        Self::require_registry(&*medium, &versions)?;
        DocumentImage::require_same(&images, &DocumentImage::read_segments(medium)?)?;
        Ok(PackedCollection::of(source.name(), &images, versions.len()))
    }

    /// Append the schema versions in ordinal order, then each document as
    /// a commit record of its own in identifier order, as a migration to
    /// segments does (`SegmentImport`), taking a new segment extent when
    /// the next record does not fit. A checkpoint then folds the locations,
    /// the prefix index and the schemas snapshot from the records.
    ///
    /// Nothing here is recovered: a pack that stops discards the file.
    fn import_records(
        collection: &NativeCollection,
        (images, versions): (&[DocumentImage], &[Vec<u8>]),
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let stored = collection.stored_generation()?;
        let generation = SegmentCheckpoint::next_stable(&stored, stored.generation);
        let mut segment = Self::open_active_segment(collection, &stored)?;
        let mut live = 0;
        if !versions.is_empty() {
            medium.mark_schemas()?;
        }
        let mut ordered: Vec<&DocumentImage> = images.iter().collect();
        ordered.sort_by(|left, right| left.id.cmp(&right.id));
        medium.prime_dictionary(&Self::first_records(&ordered, generation)?)?;
        for (index, version) in versions.iter().enumerate() {
            let ordinal = u32::try_from(index + 1).map_err(|_| {
                NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge,
                    "schema registry has too many versions",
                )
            })?;
            let record = |lsn| SegmentImport::schema_record((ordinal, version), lsn, generation);
            Self::append_packed(
                collection,
                (&mut segment, &mut live, generation),
                (&record, false),
            )?;
        }
        for image in ordered {
            let record = |lsn| SegmentImport::document_record(image, lsn, generation);
            Self::append_packed(
                collection,
                (&mut segment, &mut live, generation),
                (&record, true),
            )?;
        }
        segment.full_flush()?;
        let epoch = medium.require_slot()?.table_epoch;
        let stable = GenerationRecord::log_stable(generation, segment.position());
        SegmentCheckpoint::publish(collection, &stable, epoch)?;
        SegmentCheckpoint::run(collection, segment, generation)?;
        Ok(())
    }

    /// The records of the first documents of `ordered` whose bodies reach
    /// what a dictionary trains on, or of all of them, for the medium to
    /// train the collection's dictionary on before they are written: a
    /// collection that holds that much gets it at its first checkpoint, and
    /// a pack writes every record after it.
    fn first_records(
        ordered: &[&DocumentImage],
        generation: u64,
    ) -> Result<Vec<LogRecord>, NativeStorageError> {
        let (mut records, mut total) = (Vec::new(), 0);
        for image in ordered {
            if total >= DICTIONARY_TRAINING_BYTES {
                break;
            }
            let record = SegmentImport::document_record(image, 0, generation)?;
            total += record.blobs.len();
            records.push(record);
        }
        Ok(records)
    }

    /// Append the record `record` builds for the next sequence number,
    /// counting its bytes in `live` when it holds a document. When it does
    /// not fit in the active segment, that segment is sealed with the
    /// `live` bytes it holds and the next opens with a checkpoint record, as
    /// a checkpoint's rotation does.
    fn append_packed(
        collection: &NativeCollection,
        (segment, live, generation): (&mut LogSegment, &mut u64, u64),
        (record, document): (&dyn Fn(u64) -> Result<LogRecord, NativeStorageError>, bool),
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let mut encoded = segment.encode(&record(segment.next_lsn())?)?;
        if segment.length() + encoded.len() as u64 > medium.max_segment_bytes() {
            medium.seal_segment(segment.sequence(), *live)?;
            *live = 0;
            let manifest = SegmentCheckpoint::manifest(&collection.path, &*medium)?;
            let manifest = serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?;
            *segment =
                LogSegment::create_documents(&*medium, segment.next_lsn(), &manifest, generation)?;
            encoded = segment.encode(&record(segment.next_lsn())?)?;
        }
        segment.append(&encoded, 1)?;
        if document {
            *live += encoded.len() as u64;
        }
        Ok(())
    }

    /// Flush the finished staging file through the drive cache.
    fn flush_staging(staging: &Path) -> Result<(), NativeStorageError> {
        let file = fylo_vfs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(staging)
            .map_err(NativeStorageError::io)?;
        crate::sync_handle(&file).map_err(NativeStorageError::io)
    }

    /// Give the verified staging file its name, never replacing a file that
    /// appeared at `out` meanwhile: a link where the filesystem has them,
    /// which fails on an existing name, and otherwise a rename after a
    /// check.
    fn adopt_staging(staging: &Path, out: &Path) -> Result<(), NativeStorageError> {
        let exists = || {
            Self::pack_unsupported(&format!(
                "output {} appeared while the pack ran; fylo pack never replaces a file",
                out.display()
            ))
        };
        match fylo_vfs::hard_link(staging, out) {
            Ok(()) => fylo_vfs::remove_file(staging).map_err(NativeStorageError::io)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::remove_staging(staging)?;
                return Err(exists());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                if path_exists_no_follow(out)? {
                    Self::remove_staging(staging)?;
                    return Err(exists());
                }
                fylo_vfs::rename(staging, out).map_err(NativeStorageError::io)?;
            }
            Err(error) => return Err(NativeStorageError::io(error)),
        }
        sync_parent(out)
    }

    /// Write one collection of this database as a file layout collection of
    /// `unpacked`, then check the written files against the documents.
    fn unpack_collection(
        &self,
        unpacked: &Self,
        source: &NativeCollection,
    ) -> Result<PackedCollection, NativeStorageError> {
        let images = DocumentImage::read_collection(source)?;
        let versions = self.registry_versions(source)?;
        let directory = Self::write_unpacked(unpacked, source, &images, &versions)?;
        let copies = images
            .iter()
            .map(|image| {
                let path = directory.join(FileExport::relative_path(image, source.shard_width()));
                DocumentImage::read_file(&path, (&image.id, image.state))
            })
            .collect::<Result<Vec<_>, _>>()?;
        DocumentImage::require_same(&images, &copies)?;
        Ok(PackedCollection::of(source.name(), &images, versions.len()))
    }

    /// Write `images` and the schema `versions` of collection `source` of a
    /// database as a file layout collection of `unpacked`: its catalog
    /// descriptor, the tree `fylo export` writes, and the registry, in a
    /// registry commit's order. Returns the collection's directory.
    pub(super) fn write_unpacked(
        unpacked: &Self,
        source: &NativeCollection,
        images: &[DocumentImage],
        versions: &[Vec<u8>],
    ) -> Result<PathBuf, NativeStorageError> {
        let name = source.name();
        let mut descriptor: Map<String, Value> = Self::default_descriptor(source);
        if let Some(mode) = source.validation() {
            descriptor.insert("validation".into(), Value::from(mode.as_str()));
        }
        let mut encoded = serde_json::to_vec(&descriptor).map_err(|error| json_error(&error))?;
        encoded.push(b'\n');
        let descriptor_path = unpacked.descriptor_path(name);
        if let Some(parent) = descriptor_path.parent() {
            fylo_vfs::create_dir_all(parent).map_err(NativeStorageError::io)?;
        }
        durable_replace(&descriptor_path, &encoded)?;
        let directory = unpacked.root.path().join(".collections").join(name);
        FileExport::build(&unpacked.root, &directory, images, source.shard_width())?;
        let collection = unpacked.root.collection(name)?;
        if let Some(head) = Self::registry_head(versions)? {
            // A registry commit's order (ADR 0017 section 3): the version
            // files, the generation fence, then the descriptor's head.
            unpacked.write_registry_files(&collection, versions)?;
            unpacked.fence_generation(&collection)?;
            unpacked.write_schema_head(&collection, head)?;
        }
        Ok(directory)
    }

    /// The ordinal of the newest of `versions`, or `None` for none.
    fn registry_head(versions: &[Vec<u8>]) -> Result<Option<u32>, NativeStorageError> {
        if versions.is_empty() {
            return Ok(None);
        }
        u32::try_from(versions.len()).map(Some).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "schema registry has too many versions",
            )
        })
    }

    /// Remove everything in `out`.
    pub(super) fn clear_output(out: &Path) -> Result<(), NativeStorageError> {
        for entry in fylo_vfs::read_dir(out).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if entry.file_type().map_err(NativeStorageError::io)?.is_dir() {
                remove_dir_durable(&entry.path())?;
            } else {
                fylo_vfs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
            }
        }
        Ok(())
    }

    fn pack_unsupported(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::Unsupported, message)
    }
}

impl PackedCollection {
    fn of(name: &str, images: &[DocumentImage], schema_versions: usize) -> Self {
        let deleted = images
            .iter()
            .filter(|image| image.state == LocationState::Deleted)
            .count();
        Self {
            collection: name.to_owned(),
            documents: images.len() - deleted,
            deleted,
            schema_versions,
        }
    }
}
