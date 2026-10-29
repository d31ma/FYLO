use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::super::{
    GenerationRecord, NativeWriteRoot, durable_replace, failpoint, json_error, read_bounded_json,
    remove_dir_durable, sync_directory, write_json_durable,
};
use super::document_image::DocumentImage;
use super::file_export::FileExport;
use super::layout_migration_report::LayoutMigrationReport;
use super::log_checkpoint::LogCheckpoint;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_import::SegmentImport;
use super::segment_layout::SegmentLayout;
use super::segment_schemas::SegmentSchemas;
use crate::segments::{LocationState, SegmentPaths, TableCache};
use crate::{
    CollectionKind, CollectionLayout, FilesystemProfile, NativeCollection, NativeStorageError,
    NativeStorageErrorCode,
};

/// A durable record that a layout migration passed verification and is
/// swapping its copy into place. While it exists, every opener finishes the
/// swap before touching the collection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LayoutMigrationMarker {
    format: String,
    to: CollectionLayout,
    generation: u64,
}

/// Moves a document collection between the file layout and the segment
/// layout (ADR 0015 section 9).
///
/// The copy is built in `.layout-staging` and checked against the source
/// with the full oracle: the same live and deleted identifiers, and for
/// every document the same bytes, attributes, mode and modification time.
/// Only then is a durable marker written. From the marker on, the swap only
/// moves forward, and any later opener completes it.
impl NativeWriteRoot {
    const MARKER_FORMAT: &'static str = "fylo.layout-migration.v1";
    const MARKER_FILE: &'static str = "layout-migration.json";
    const MIGRATION_TRANSACTION: &'static str = "layout-migration";
    const STAGING: &'static str = ".layout-staging";
    const RETIRED: &'static str = ".layout-old";
    const FILE_NAMES: [&'static str; 3] = ["docs", ".deleted", "index"];
    const SEGMENT_NAMES: [&'static str; 3] = ["segments", "index", "docs"];

    /// Move collection `name` to `layout`.
    ///
    /// `allow_engine_only_access` must be set to move a collection whose
    /// records name another owner to segments, where FYLO enforces their
    /// access descriptors but no longer gives each record its own operating
    /// system owner (ADR 0015 section 10).
    ///
    /// # Errors
    ///
    /// Returns an error for a file collection, a non-native profile, a root
    /// with version control, a copy that fails verification (the collection
    /// is left as it was), lock contention or an I/O failure.
    pub fn migrate_layout(
        &self,
        name: &str,
        layout: CollectionLayout,
        allow_engine_only_access: bool,
    ) -> Result<LayoutMigrationReport, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(name)?;
        self.require_migratable(&collection)?;
        let _lock = self.lock_collection(&collection.path)?;
        self.recover_locked(&collection)?;
        // Recovery may have moved the descriptor's schema head.
        let collection = self.root.collection(name)?;
        let images = DocumentImage::read_collection(&collection)?;
        let report = LayoutMigrationReport::of(name, layout, &images);
        if collection.layout() == layout {
            return Ok(report);
        }
        if layout == CollectionLayout::Segments && !allow_engine_only_access {
            Self::require_owner_only_access(&collection, &images)?;
        }
        let generation = collection.stored_generation()?.generation;
        let target = generation + 2 - generation % 2 + 2;
        let versions = self.registry_versions(&collection)?;
        let staging = collection.path.join(Self::STAGING);
        self.build_staging(
            &collection,
            (layout, target),
            (&images, &versions),
            &staging,
        )?;
        if let Err(error) = failpoint("after-layout-staging") {
            remove_dir_durable(&staging)?;
            return Err(error);
        }
        if layout == CollectionLayout::Files {
            self.write_registry_files(&collection, &versions)?;
        }
        Self::write_marker(&collection, layout, target)?;
        let swapped = failpoint("after-layout-marker")
            .and_then(|()| Self::publish_migration_writing(&collection, generation))
            .and_then(|()| self.swap_entries(&collection))
            .and_then(|_| failpoint("after-layout-swap"))
            .and_then(|()| self.finish_layout_migration(&collection));
        self.roll_forward(&collection, swapped)?;
        Ok(report)
    }

    /// The marker is the commit point. A failure after it is finished here
    /// by running the idempotent swap again, so a live writer leaves nothing
    /// for recovery; only when that also fails does the marker wait for the
    /// next opener.
    fn roll_forward(
        &self,
        collection: &NativeCollection,
        swapped: Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        let Err(error) = swapped else {
            return Ok(());
        };
        self.finish_layout_migration(collection).map_err(|_| error)
    }

    /// Finish a migration a crash interrupted after its marker. Returns
    /// whether there was one; the caller's view of the collection is then
    /// out of date.
    pub(crate) fn finish_layout_migration_if_pending(
        &self,
        collection: &NativeCollection,
    ) -> Result<bool, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Ok(false);
        }
        if crate::path_exists_no_follow(&Self::marker_path(collection)?)? {
            self.finish_layout_migration(collection)?;
            return Ok(true);
        }
        let staging = collection.path.join(Self::STAGING);
        if crate::path_exists_no_follow(&staging)? {
            remove_dir_durable(&staging)?;
        }
        Ok(false)
    }

    fn require_migratable(&self, collection: &NativeCollection) -> Result<(), NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(Self::unsupported(
                "this build does not carry the segment layout",
            ));
        }
        if collection.kind() != CollectionKind::Document {
            return Err(Self::unsupported(
                "raw-file collections keep the file layout",
            ));
        }
        if self.filesystem_profile() != FilesystemProfile::Native {
            return Err(Self::unsupported(
                "the segment layout requires the native filesystem profile",
            ));
        }
        if crate::path_exists_no_follow(&self.root.repository_root().join(".fylo-vcs"))? {
            return Err(Self::unsupported(
                "collections of a root with version control keep the file layout for now",
            ));
        }
        Ok(())
    }

    /// Refuse records whose descriptor names an owner other than the root's,
    /// whose operating system ownership a segment collection cannot keep.
    fn require_owner_only_access(
        collection: &NativeCollection,
        images: &[DocumentImage],
    ) -> Result<(), NativeStorageError> {
        let owner = Self::directory_owner(&collection.path)?;
        for image in images {
            let foreign = crate::decode_access_descriptor(&image.attributes)?
                .is_some_and(|descriptor| Some((descriptor.uid, descriptor.gid)) != owner);
            if foreign {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::PermissionDenied,
                    format!(
                        "record {} names another owner; segments enforce its access in FYLO only, so pass --allow-engine-only-access to migrate",
                        image.id
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The collection's schema registry, version by version from ordinal 1,
    /// which a migration carries over (ADR 0017 section 3): a segment
    /// collection's schema records become the directory registry's files,
    /// and those files become schema records. A segment collection's
    /// descriptor is first brought up to its records' head, which the file
    /// layout reads.
    fn registry_versions(
        &self,
        collection: &NativeCollection,
    ) -> Result<Vec<Vec<u8>>, NativeStorageError> {
        if collection.uses_segments() {
            let schemas = SegmentSchemas::load(
                &SegmentPaths::of(&collection.path),
                collection.schema_head(),
            )?;
            if let Some(head) = schemas.head()
                && Some(head) != collection.schema_head()
            {
                self.write_schema_head(collection, head)?;
            }
            return Ok(schemas
                .entries()
                .iter()
                .map(|entry| entry.version.as_bytes().to_vec())
                .collect());
        }
        (1..=collection.schema_head().unwrap_or(0))
            .map(|ordinal| self.root.read_schema_version(collection.name(), ordinal))
            .collect()
    }

    /// Write a segment collection's versions as the directory registry's
    /// files before the swap. A file layout collection reads them from its
    /// descriptor's head down; until the swap nothing reads them, and a
    /// migration run again writes them again.
    fn write_registry_files(
        &self,
        collection: &NativeCollection,
        versions: &[Vec<u8>],
    ) -> Result<(), NativeStorageError> {
        for (index, version) in versions.iter().enumerate() {
            let ordinal = u32::try_from(index + 1)
                .map_err(|_| Self::unsupported("schema registry has too many versions"))?;
            durable_replace(
                &self.schema_version_path(collection.name(), ordinal)?,
                version,
            )?;
        }
        Ok(())
    }

    fn build_staging(
        &self,
        collection: &NativeCollection,
        (layout, generation): (CollectionLayout, u64),
        (images, versions): (&[DocumentImage], &[Vec<u8>]),
        staging: &Path,
    ) -> Result<(), NativeStorageError> {
        if crate::path_exists_no_follow(staging)? {
            remove_dir_durable(staging)?;
        }
        let outcome = self
            .write_staging(
                collection,
                (layout, generation),
                (images, versions),
                staging,
            )
            .and_then(|()| Self::verify_staging(collection, layout, (images, versions), staging));
        if outcome.is_err() && crate::path_exists_no_follow(staging)? {
            remove_dir_durable(staging)?;
        }
        outcome
    }

    fn write_staging(
        &self,
        collection: &NativeCollection,
        (layout, generation): (CollectionLayout, u64),
        (images, versions): (&[DocumentImage], &[Vec<u8>]),
        staging: &Path,
    ) -> Result<(), NativeStorageError> {
        match layout {
            CollectionLayout::Segments => {
                SegmentImport::build(&self.root, staging, (images, versions), generation)
            }
            CollectionLayout::Files => {
                FileExport::build(&self.root, staging, images, collection.shard_width())
            }
        }
    }

    fn verify_staging(
        collection: &NativeCollection,
        layout: CollectionLayout,
        (images, versions): (&[DocumentImage], &[Vec<u8>]),
        staging: &Path,
    ) -> Result<(), NativeStorageError> {
        if layout == CollectionLayout::Segments {
            let staged = SegmentSchemas::load(&SegmentPaths::of(staging), None)?;
            let same = staged.entries().len() == versions.len()
                && staged
                    .entries()
                    .iter()
                    .zip(versions)
                    .all(|(entry, version)| entry.version.as_bytes() == version.as_slice());
            if !same {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "layout migration copy does not hold the collection's schema registry",
                ));
            }
        }
        let copies = match layout {
            CollectionLayout::Segments => DocumentImage::read_segments(staging)?,
            CollectionLayout::Files => images
                .iter()
                .map(|image| {
                    let path =
                        staging.join(FileExport::relative_path(image, collection.shard_width()));
                    DocumentImage::read_file(&path, (&image.id, image.state))
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        DocumentImage::require_same(images, &copies)
    }

    fn write_marker(
        collection: &NativeCollection,
        to: CollectionLayout,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let marker = LayoutMigrationMarker {
            format: Self::MARKER_FORMAT.into(),
            to,
            generation,
        };
        write_json_durable(&Self::marker_path(collection)?, &marker)
    }

    /// Readers of the source layout see `writing` for the whole swap.
    fn publish_migration_writing(
        collection: &NativeCollection,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        if !collection.uses_segments() {
            let state = LogCheckpoint::state_path(collection)?;
            return write_json_durable(
                &state,
                &GenerationRecord::writing(generation + 1, Self::MIGRATION_TRANSACTION.into()),
            );
        }
        let stored = collection.stored_generation()?;
        let active = Self::open_active_segment(collection, &stored)?;
        let writing = GenerationRecord::log_writing(generation + 1, active.position());
        SegmentCheckpoint::publish_current(collection, &writing)
    }

    /// Every step of the swap checks what is already done, so a crash at any
    /// point is finished by running it again.
    fn finish_layout_migration(
        &self,
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        let marker_path = Self::marker_path(collection)?;
        let marker = self.swap_entries(collection)?;
        let staging = collection.path.join(Self::STAGING);
        let retired = collection.path.join(Self::RETIRED);
        self.finish_state(collection, &marker)?;
        TableCache::shared().forget(&SegmentPaths::of(&collection.path));
        remove_dir_durable(&retired)?;
        if crate::path_exists_no_follow(&staging)? {
            remove_dir_durable(&staging)?;
        }
        fylo_vfs::remove_file(&marker_path).map_err(NativeStorageError::io)?;
        Ok(())
    }

    /// Move the source layout's entries aside and the copy's into place.
    /// Returns the marker that names the migration.
    fn swap_entries(
        &self,
        collection: &NativeCollection,
    ) -> Result<LayoutMigrationMarker, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let marker: LayoutMigrationMarker =
            read_bounded_json(&Self::marker_path(collection)?, crate::MAX_DESCRIPTOR_BYTES)?;
        let staging = collection.path.join(Self::STAGING);
        let retired = collection.path.join(Self::RETIRED);
        fylo_vfs::create_dir_all(&retired).map_err(NativeStorageError::io)?;
        let (old_names, new_names) = match marker.to {
            CollectionLayout::Segments => (Self::FILE_NAMES, Self::SEGMENT_NAMES),
            CollectionLayout::Files => (Self::SEGMENT_NAMES, Self::FILE_NAMES),
        };
        for name in old_names {
            Self::retire(&collection.path, (&staging, &retired), name)?;
        }
        for name in new_names {
            Self::adopt(&collection.path, &staging, name)?;
        }
        sync_directory(&collection.path)?;
        Ok(marker)
    }

    /// Move the source layout's entry out of the way, unless the copy's
    /// entry of the same name is already in its place.
    fn retire(
        collection: &Path,
        (staging, retired): (&Path, &Path),
        name: &str,
    ) -> Result<(), NativeStorageError> {
        let current = collection.join(name);
        let replaced_already = !crate::path_exists_no_follow(&staging.join(name))?
            && Self::FILE_NAMES.contains(&name)
            && Self::SEGMENT_NAMES.contains(&name);
        if replaced_already || !crate::path_exists_no_follow(&current)? {
            return Ok(());
        }
        let target = retired.join(name);
        if crate::path_exists_no_follow(&target)? {
            return Ok(());
        }
        fylo_vfs::rename(&current, &target).map_err(NativeStorageError::io)
    }

    fn adopt(collection: &Path, staging: &Path, name: &str) -> Result<(), NativeStorageError> {
        let copied = staging.join(name);
        if !crate::path_exists_no_follow(&copied)? {
            return Ok(());
        }
        fylo_vfs::rename(&copied, collection.join(name)).map_err(NativeStorageError::io)
    }

    /// Point the catalog at the new layout and leave the state file the new
    /// layout expects: the fixed segment marker for segments, a stable v1 state
    /// for files.
    fn finish_state(
        &self,
        collection: &NativeCollection,
        marker: &LayoutMigrationMarker,
    ) -> Result<(), NativeStorageError> {
        let state = LogCheckpoint::state_path(collection)?;
        // A file layout collection with a registry keeps the fenced
        // generation format older releases refuse (ADR 0017 section 3).
        let fenced = self
            .root
            .collection(collection.name())?
            .schema_head()
            .is_some();
        if marker.to == CollectionLayout::Files {
            write_json_durable(
                &state,
                &GenerationRecord::writing(
                    marker.generation - 1,
                    Self::MIGRATION_TRANSACTION.into(),
                )
                .fenced(fenced),
            )?;
        }
        self.write_layout(collection, marker.to)?;
        match marker.to {
            CollectionLayout::Segments => SegmentLayout::write_marker(&state),
            CollectionLayout::Files => write_json_durable(
                &state,
                &GenerationRecord::stable(marker.generation).fenced(fenced),
            ),
        }
    }

    fn write_layout(
        &self,
        collection: &NativeCollection,
        layout: CollectionLayout,
    ) -> Result<(), NativeStorageError> {
        let path = self.descriptor_path(collection.name());
        let mut descriptor: Map<String, Value> = if crate::path_exists_no_follow(&path)? {
            read_bounded_json(&path, crate::MAX_DESCRIPTOR_BYTES)?
        } else {
            Self::default_descriptor(collection)
        };
        match layout {
            CollectionLayout::Segments => {
                descriptor.insert("layout".into(), Value::from(layout.as_str()));
            }
            CollectionLayout::Files => {
                descriptor.remove("layout");
            }
        }
        let mut encoded = serde_json::to_vec(&descriptor).map_err(|error| json_error(&error))?;
        encoded.push(b'\n');
        if let Some(parent) = path.parent() {
            fylo_vfs::create_dir_all(parent).map_err(NativeStorageError::io)?;
        }
        durable_replace(&path, &encoded)
    }

    fn default_descriptor(collection: &NativeCollection) -> Map<String, Value> {
        let mut descriptor = Map::new();
        descriptor.insert("version".into(), Value::from(1));
        descriptor.insert("kind".into(), Value::from("document"));
        descriptor.insert("shardWidth".into(), Value::from(collection.shard_width()));
        descriptor
    }

    fn descriptor_path(&self, name: &str) -> PathBuf {
        self.root
            .catalog_root()
            .join(".fylo-catalog")
            .join("collections")
            .join(format!("{name}.json"))
    }

    fn marker_path(collection: &NativeCollection) -> Result<PathBuf, NativeStorageError> {
        Ok(LogCheckpoint::state_path(collection)?.with_file_name(Self::MARKER_FILE))
    }

    #[cfg(unix)]
    fn directory_owner(path: &Path) -> Result<Option<(u32, u32)>, NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fylo_vfs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        Ok(Some((metadata.uid(), metadata.gid())))
    }

    // The signature mirrors the Unix implementation.
    #[allow(clippy::unnecessary_wraps)]
    #[cfg(not(unix))]
    fn directory_owner(_path: &Path) -> Result<Option<(u32, u32)>, NativeStorageError> {
        Ok(None)
    }

    fn unsupported(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::Unsupported, message)
    }
}

impl LayoutMigrationReport {
    fn of(name: &str, layout: CollectionLayout, images: &[DocumentImage]) -> Self {
        let deleted = images
            .iter()
            .filter(|image| image.state == LocationState::Deleted)
            .count();
        Self {
            collection: name.to_owned(),
            layout,
            documents: images.len() - deleted,
            deleted,
        }
    }
}
