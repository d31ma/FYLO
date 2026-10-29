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
use crate::platform_companion::{read_dir_without_companions, remove_companion};
use crate::segments::LocationState;
use crate::{
    DICTIONARY_TRAINING_BYTES, DatabaseKey, FilesystemProfile, NativeCollection, NativeRoot,
    NativeStorageError, NativeStorageErrorCode, RootOpenOptions, path_exists_no_follow,
};

/// Converts between a directory root and a `.vdbx` database (ADR 0016
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
/// `pack --sealed` builds the same staging file with every record body
/// stored as is, then writes it as a sealed file: read-only, with no free
/// extents, recompressed in 64 KiB blocks (ADR 0016 sections 8 and 11). The
/// sealed file is checked against the source with the oracle again, read
/// through its blocks, before it is given its name. It also takes a `.vdbx`
/// database as its source, a consistent copy of a live one: every
/// collection's claim is held from the first read to the commit, so no
/// writer in any process commits to it meanwhile.
///
/// `unpack` writes a database's collections as a new directory root in the
/// file layout: for each, the tree `fylo export` writes, its catalog
/// descriptor and its schema registry.
impl NativeWriteRoot {
    const PACK_STAGING_SUFFIX: &'static str = ".pack-staging";
    const SEAL_STAGING_SUFFIX: &'static str = ".seal-staging";

    /// Pack this directory root into a new `.vdbx` database at `out`, which
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
        self.pack_with(out, allow_engine_only_access, (None, false))
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
        self.pack_with(out, allow_engine_only_access, (Some(key), false))
    }

    /// Pack this root, a directory root or a `.vdbx` database, into a new
    /// sealed database at `out`, which must not exist: read-only, with no
    /// free extents, and recompressed in 64 KiB blocks (ADR 0016 sections 8
    /// and 11). With `key` the blocks are sealed with it, and a database
    /// that is encrypted packs only with a key, so its documents never land
    /// in a plain file.
    ///
    /// From a database this is a consistent copy for a backup: every
    /// collection's claim is held from the first read to the commit, so
    /// writers of the database, in this process or another, wait for it,
    /// and its readers do not. A sealed database is read without a claim,
    /// since nothing writes it.
    ///
    /// # Errors
    ///
    /// The same as [`Self::pack_database`], but that a database is taken;
    /// `EKEY` for an encrypted database without `key` or a locked one.
    pub fn pack_sealed_database(
        &self,
        out: &Path,
        allow_engine_only_access: bool,
        key: Option<&DatabaseKey>,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        self.pack_with(out, allow_engine_only_access, (key, true))
    }

    fn pack_with(
        &self,
        out: &Path,
        allow_engine_only_access: bool,
        (key, sealed): (Option<&DatabaseKey>, bool),
    ) -> Result<DatabasePackReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(Self::pack_unsupported(
                "this build cannot write a .vdbx database",
            ));
        }
        self.root.assert_profile_owned()?;
        let database = self.root.database();
        match database {
            Some(_) if !sealed => {
                return Err(Self::pack_unsupported(
                    "fylo pack takes a directory root, and this root is already a .vdbx \
                     database; fylo pack --sealed copies one",
                ));
            }
            Some(database) => {
                database.require_key()?;
                if database.is_encrypted() && key.is_none() {
                    return Err(NativeStorageError::new(
                        NativeStorageErrorCode::KeyRequired,
                        "the .vdbx database is encrypted, so its sealed pack is encrypted too \
                         and needs the key (fylo.encryption.key)",
                    ));
                }
            }
            None => self.require_packable()?,
        }
        let names = match self.root.database_collection_names() {
            Some(mut names) => {
                names.sort();
                names
            }
            None => self.packed_collection_names()?,
        };
        // A bare file name lives in the working directory, which the commit
        // flushes.
        let out = &match out.parent() {
            Some(parent) if parent.as_os_str().is_empty() => Path::new(".").join(out),
            _ => out.to_owned(),
        };
        Self::require_new_output(out)?;
        let staging = Self::pack_staging(out, Self::PACK_STAGING_SUFFIX)?;
        let sealing = Self::pack_staging(out, Self::SEAL_STAGING_SUFFIX)?;
        let collections = self.packed_collections(&names)?;
        Self::remove_staging(&staging)?;
        Self::remove_staging(&sealing)?;
        let built = self
            .build_pack(
                &collections.1,
                &staging,
                (allow_engine_only_access, if sealed { None } else { key }),
                sealed,
            )
            .and_then(|report| {
                if !sealed {
                    return Ok((report, &staging));
                }
                let blocks = self.seal_pack(&collections.1, &staging, &sealing, key)?;
                Self::remove_staging(&staging)?;
                Ok((
                    DatabasePackReport {
                        sealed: Some(blocks),
                        ..report
                    },
                    &sealing,
                ))
            })
            .and_then(|(report, built)| Self::flush_staging(built).map(|()| (report, built)))
            .and_then(|built| failpoint("after-pack-staging").map(|()| built));
        let (report, built) = match built {
            Ok(built) => built,
            Err(error) => {
                // A staging file is never read; one this cannot remove is
                // removed by the next pack.
                let _ = Self::remove_staging(&staging);
                let _ = Self::remove_staging(&sealing);
                return Err(error);
            }
        };
        Self::adopt_staging(built, out)?;
        drop(collections);
        Ok(report)
    }

    /// Write the container `staging` as a sealed file at `sealing`.
    #[cfg(not(target_family = "wasm"))]
    fn write_sealed(
        staging: &Path,
        sealing: &Path,
        key: Option<&DatabaseKey>,
    ) -> Result<crate::ContainerSealedReport, NativeStorageError> {
        crate::ContainerFile::open_reader(staging)
            .and_then(|file| file.write_sealed(sealing, key.map(DatabaseKey::secret)))
            .map_err(crate::segments::ContainerMedium::error)
    }

    /// The browser carries no zstd compressor, so it reads sealed files
    /// but never writes one.
    #[cfg(target_family = "wasm")]
    fn write_sealed(
        _staging: &Path,
        _sealing: &Path,
        _key: Option<&DatabaseKey>,
    ) -> Result<crate::ContainerSealedReport, NativeStorageError> {
        Err(Self::pack_unsupported(
            "this build cannot write a sealed .vdbx database",
        ))
    }

    /// The collections named `names`, in that order, each locked and
    /// recovered, as `fylo export` does, with the locks to hold until the
    /// pack has its name. A sealed database is never written, so it takes
    /// no lock and needs no recovery.
    fn packed_collections(
        &self,
        names: &[String],
    ) -> Result<(Vec<super::CollectionGuard>, Vec<NativeCollection>), NativeStorageError> {
        let read_only = self
            .root
            .database()
            .is_some_and(|database| database.is_sealed());
        let mut locks = Vec::with_capacity(names.len());
        let mut collections = Vec::with_capacity(names.len());
        for name in names {
            let collection = self.root.collection(name)?;
            if !read_only {
                locks.push(self.lock_collection(&collection)?);
                self.recover_locked(&collection)?;
            }
            // Recovery may have moved the descriptor's schema head.
            collections.push(self.root.collection(name)?);
        }
        Ok((locks, collections))
    }

    /// Write the plain staging file `staging`, which [`Self::build_pack`]
    /// built and checked, as a sealed file at `sealing`, sealed with `key`
    /// when one is given, and check it against `collections`, the source,
    /// with the oracle again, reading it through its blocks. This process's
    /// open of it is closed before this returns.
    fn seal_pack(
        &self,
        collections: &[NativeCollection],
        staging: &Path,
        sealing: &Path,
        key: Option<&DatabaseKey>,
    ) -> Result<crate::ContainerSealedReport, NativeStorageError> {
        let blocks = Self::write_sealed(staging, sealing, key)?;
        let mut options = RootOpenOptions::new(sealing);
        if let Some(key) = key {
            options = options.with_database_key(key.clone());
        }
        let outcome = Self::open_profiled(options).and_then(|packed| {
            for source in collections {
                let images = DocumentImage::read_collection(source)?;
                let copy = packed.root.collection(source.name())?;
                Self::require_registry(&*copy.segment_medium()?, &self.registry_versions(source)?)?;
                DocumentImage::require_same(&images, &DocumentImage::read_collection(&copy)?)?;
            }
            Ok(())
        });
        NativeRoot::close_database(sealing)?;
        outcome.map(|()| blocks)
    }

    /// Write this `.vdbx` database's collections as a new directory root in
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
                "fylo unpack takes a .vdbx database, and this root is a directory",
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
        let (_locks, collections) = self.packed_collections(names)?;
        let mut report = DatabasePackReport::default();
        for collection in &collections {
            report
                .collections
                .push(self.unpack_collection(unpacked, collection)?);
        }
        Ok(report)
    }

    /// Refuse a root that uses what a `.vdbx` database does not hold yet
    /// (ADR 0016 section 1), naming it.
    fn require_packable(&self) -> Result<(), NativeStorageError> {
        if self.filesystem_profile() != FilesystemProfile::Native {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root on the EFS sidecar profile, which has no .vdbx form",
            ));
        }
        if path_exists_no_follow(&self.root.repository_root().join(".fylo-vcs"))? {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root that uses document version control, which a .vdbx database does not hold",
            ));
        }
        if Self::first_entry(&self.root.path().join(".fylo-queue"))?.is_some() {
            return Err(NativeRoot::database_unsupported(
                "cannot pack a root that uses the embedded queue, which a .vdbx database does not hold",
            ));
        }
        if let Some(bucket) = Self::first_entry(&self.root.path().join(".buckets"))? {
            return Err(NativeRoot::database_unsupported(&format!(
                "cannot pack a root that uses buckets (raw-file collections), which a .vdbx database does not hold: bucket \"{bucket}\""
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
        let mut entries = read_dir_without_companions(directory).map_err(NativeStorageError::io)?;
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
        for entry in read_dir_without_companions(&directory).map_err(NativeStorageError::io)? {
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

    /// The staging file of a pack to `out` with `suffix`: a hidden sibling,
    /// so the rename that commits it stays in one directory.
    fn pack_staging(out: &Path, suffix: &str) -> Result<PathBuf, NativeStorageError> {
        let name = out.file_name().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                format!("output {} names no file", out.display()),
            )
        })?;
        let parent = out.parent().unwrap_or_else(|| Path::new("."));
        Ok(parent.join(format!(".{}{suffix}", name.to_string_lossy())))
    }

    /// Remove a staging file and the lease files beside it, and forget this
    /// process's open of it, as a stopped pack leaves them.
    fn remove_staging(staging: &Path) -> Result<(), NativeStorageError> {
        if path_exists_no_follow(staging)? {
            let _ = NativeRoot::close_database(staging);
            fylo_vfs::remove_file(staging).map_err(NativeStorageError::io)?;
        }
        remove_companion(staging);
        let (Some(parent), Some(name)) = (staging.parent(), staging.file_name()) else {
            return Ok(());
        };
        let leases = format!("{}.lease.", name.to_string_lossy());
        for entry in fylo_vfs::read_dir(parent).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if entry.file_name().to_string_lossy().starts_with(&leases) {
                fylo_vfs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
                remove_companion(&entry.path());
            }
        }
        Ok(())
    }

    /// Build the database in `staging` and verify every collection of it,
    /// with every record body stored as is when it is to be `sealed`.
    /// This process's open of the file is closed before it returns, so the
    /// file is renamed with no writer holding it.
    fn build_pack(
        &self,
        collections: &[NativeCollection],
        staging: &Path,
        (allow_engine_only_access, key): (bool, Option<&DatabaseKey>),
        sealed: bool,
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
                    (allow_engine_only_access, sealed),
                )?);
            }
            Ok(report)
        });
        NativeRoot::close_database(staging)?;
        outcome
    }

    /// Copy one collection into the database `packed`, uncompressed for a
    /// sealed pack, then check the copy against `images`, the source's
    /// documents.
    fn pack_collection(
        &self,
        packed: &Self,
        source: &NativeCollection,
        (allow_engine_only_access, sealed): (bool, bool),
    ) -> Result<PackedCollection, NativeStorageError> {
        let images = DocumentImage::read_collection(source)?;
        // A database's records have their access enforced in FYLO only
        // already, so copying them into another changes nothing.
        if !allow_engine_only_access && !self.root.is_database() {
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
        Self::import_records(&collection, (&images, &versions), sealed)?;
        let medium = collection.segment_medium()?;
        Self::require_registry(&*medium, &versions)?;
        DocumentImage::require_same(&images, &DocumentImage::read_segments(medium)?)?;
        Ok(PackedCollection::of(source.name(), &images, versions.len()))
    }

    /// Append the schema versions in ordinal order, then each document as
    /// a commit record of its own in identifier order, as a migration to
    /// segments does (`SegmentImport`), taking a new segment extent when
    /// the next record does not fit. A checkpoint then folds the locations,
    /// the prefix index and the schemas snapshot from the records. For a
    /// sealed pack the bodies are stored as is, and no dictionary is
    /// trained: the blocks compress them.
    ///
    /// Nothing here is recovered: a pack that stops discards the file.
    fn import_records(
        collection: &NativeCollection,
        (images, versions): (&[DocumentImage], &[Vec<u8>]),
        uncompressed: bool,
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
        if uncompressed {
            medium.write_uncompressed();
        } else {
            medium.prime_dictionary(&Self::first_records(&ordered, generation)?)?;
        }
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
        if path_exists_no_follow(out)? {
            Self::remove_staging(staging)?;
            return Err(exists());
        }
        match fylo_vfs::hard_link(staging, out) {
            Ok(()) => fylo_vfs::remove_file(staging).map_err(NativeStorageError::io)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::remove_staging(staging)?;
                return Err(exists());
            }
            // Any other failure means the filesystem has no hard links:
            // exFAT and FAT answer `EPERM`, macOS SMB shares `ENOTSUP`, which
            // `std` does not map to `Unsupported`.
            Err(_) => Self::adopt_without_link(staging, out, exists)?,
        }
        // The staging file's lease, marked free when the build closed it.
        Self::remove_staging(staging)?;
        sync_parent(out)
    }

    /// Name `staging` as `out` where hard links are refused: reserve the name
    /// by exclusive create, then rename the staging file over that empty
    /// reservation, which every filesystem replaces in one step. A refused
    /// link on an SMB share can leave an empty file at `out`; one that was
    /// absent just before the link and is still empty is taken as that.
    fn adopt_without_link(
        staging: &Path,
        out: &Path,
        exists: impl Fn() -> NativeStorageError,
    ) -> Result<(), NativeStorageError> {
        let reserved = match fylo_vfs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
        {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        if !reserved {
            let debris = fylo_vfs::symlink_metadata(out)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0);
            if !debris {
                Self::remove_staging(staging)?;
                return Err(exists());
            }
        }
        fylo_vfs::rename(staging, out).map_err(NativeStorageError::io)
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
        for entry in read_dir_without_companions(out).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if entry.file_type().map_err(NativeStorageError::io)?.is_dir() {
                remove_dir_durable(&entry.path())?;
            } else {
                fylo_vfs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
                remove_companion(&entry.path());
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

#[cfg(test)]
mod tests {
    use super::NativeWriteRoot;
    use crate::NativeStorageError;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "fylo-adopt-{name}-{}-{}",
            std::process::id(),
            crate::wall_clock()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn refused() -> NativeStorageError {
        NativeWriteRoot::pack_unsupported("output appeared")
    }

    #[test]
    fn without_hard_links_the_staging_file_takes_a_free_or_empty_name() {
        let directory = scratch("free");
        for debris in [false, true] {
            let staging = directory.join(".db.pack-staging");
            let out = directory.join("db.vdbx");
            fs::write(&staging, b"database").unwrap();
            if debris {
                // What a refused link on an SMB share leaves behind.
                fs::write(&out, b"").unwrap();
            }
            NativeWriteRoot::adopt_without_link(&staging, &out, refused).unwrap();
            assert_eq!(fs::read(&out).unwrap(), b"database");
            assert!(!staging.exists());
            fs::remove_file(&out).unwrap();
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn without_hard_links_a_file_already_at_the_output_is_never_replaced() {
        let directory = scratch("taken");
        let staging = directory.join(".db.pack-staging");
        let out = directory.join("db.vdbx");
        fs::write(&staging, b"database").unwrap();
        fs::write(&out, b"someone else's").unwrap();
        assert!(NativeWriteRoot::adopt_without_link(&staging, &out, refused).is_err());
        assert_eq!(fs::read(&out).unwrap(), b"someone else's");
        assert!(!staging.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
