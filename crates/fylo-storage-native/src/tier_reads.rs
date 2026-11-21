use std::path::{Component, Path};

use crate::sidecar_attributes::SidecarAttributes;
use crate::temp_tier::{
    DocumentEntry, EntryKey, PendingEntry, RecordCheck, TempCacheTier, TierReading,
};
use crate::{
    CollectionIncarnation, CollectionKind, CollectionStamp, FileStamp, FilesystemProfile,
    GenerationState, NativeCollection, NativeStorageError, SpellingCache, StoredBytes,
};

/// How a files-layout collection reads its documents through the temp tier
/// (ADR 0030 sections 3 and 4).
///
/// Every generation read takes the collection stamp from the `state.json`
/// handle it already opens, and records its digest in the handle's read
/// window. A document read then looks for an entry: one read under the
/// same stamp is used with no file call on the root; one read under
/// another stamp is used only when the document file still has the stamp
/// the entry recorded. A document read from the root becomes an entry only
/// after the next generation read sees the same stamp.
impl NativeCollection {
    const LIVE_FOLDER: &'static str = "docs";
    const DOCUMENT_SUFFIX: &'static str = ".json";

    /// Whether this collection's document reads go through a temp tier:
    /// a files-layout document collection of a native-profile folder root
    /// that was opened with one.
    pub(crate) fn reads_through_tier(&self) -> bool {
        self.tier().is_some()
    }

    /// The generation, as [`Self::generation`] reads it, with the stamp
    /// recorded for the temp tier and the entries read under it written.
    ///
    /// A stamped read that fails (for example a `state.json` replaced while
    /// it was read) clears the window and answers as a handle without a
    /// tier would, so the tier never adds an error.
    pub(crate) fn observed_generation(&self) -> Result<GenerationState, NativeStorageError> {
        let observed = self.stamped_state().ok();
        let stamp = observed.as_ref().and_then(|(_, stamp)| stamp.as_ref());
        self.settle_window(stamp);
        observed.map_or_else(|| self.plain_generation(), |(state, _)| Ok(state))
    }

    /// One live document, from the temp tier when it holds a current entry,
    /// else from the root.
    pub(crate) fn read_live_document(
        &self,
        identifier: &str,
    ) -> Result<StoredBytes, NativeStorageError> {
        let live_folder = self.path.join(Self::LIVE_FOLDER);
        let Some(reading) = self.tier_reading(identifier) else {
            return self.read_document_at(&live_folder, identifier);
        };
        let cached = self.cached_document(&reading, identifier);
        cached.map_or_else(|| self.read_and_keep(&reading, identifier), Ok)
    }

    pub(crate) fn tier(&self) -> Option<(&TempCacheTier, &str)> {
        let usable = [
            self.kind == CollectionKind::Document,
            !self.uses_segments(),
            !self.root.is_database(),
        ];
        self.root.tier.tier().filter(|_| !usable.contains(&false))
    }

    fn stamped_state(
        &self,
    ) -> Result<(GenerationState, Option<CollectionStamp>), NativeStorageError> {
        self.root.assert_profile_owned()?;
        let (state, state_file) = self.stamped_generation_file()?;
        SpellingCache::shared().observe_generation(&self.path, state.generation);
        let state = self.unapplied_log_check(state)?;
        let incarnation = self.window.incarnation(|| self.read_incarnation());
        let stamp = CollectionStamp::new(state.clone(), state_file, incarnation, true);
        Ok((state, Some(stamp).filter(CollectionStamp::is_cacheable)))
    }

    fn read_incarnation(&self) -> CollectionIncarnation {
        CollectionIncarnation::read(&self.incarnation_path())
    }

    fn settle_window(&self, stamp: Option<&CollectionStamp>) {
        let ready = self.window.observe(stamp.map(CollectionStamp::digest));
        let Some((tier, _)) = self.tier() else {
            return;
        };
        for pending in ready {
            tier.write(&pending.key, &pending.bytes);
        }
    }

    /// The name the temp tier keys this collection's entries by. Every
    /// entry key and every entry's collection field come from here, so a
    /// later change to what names a collection (ADR 0030, layout work)
    /// touches only this helper.
    pub(crate) fn cache_name(&self) -> String {
        format!("{}/{}", self.kind_folder, self.name)
    }

    fn tier_reading(&self, identifier: &str) -> Option<TierReading<'_>> {
        let (tier, root_key) = self.tier()?;
        let stamp = self.window.stamp()?;
        let collection = self.cache_name();
        let key = EntryKey::document(root_key, &collection, identifier);
        Some(TierReading {
            tier,
            stamp,
            collection,
            key,
        })
    }

    fn cached_document(&self, reading: &TierReading<'_>, identifier: &str) -> Option<StoredBytes> {
        let entry = reading
            .tier
            .read(&reading.key, &reading.collection, identifier);
        let Some(entry) = entry else {
            reading.tier.count_miss();
            return None;
        };
        let Some(stored) = self.sound_entry(&entry, identifier) else {
            Self::refuse(reading);
            return None;
        };
        if entry.collection_stamp == reading.stamp {
            reading.tier.count_hit();
            return Some(stored);
        }
        self.revalidated(reading, entry, stored, identifier)
    }

    /// The stored bytes of `entry`, when it names this document's path and,
    /// as a root read checks, its manifest binds its payload to this record.
    fn sound_entry(&self, entry: &DocumentEntry, identifier: &str) -> Option<StoredBytes> {
        let stored = self.stored_from(entry, identifier)?;
        self.manifest_binds(entry, identifier).then_some(stored)
    }

    /// The EFS profile's record-identity check, from the entry alone: its
    /// manifest must bind its payload as this collection's `identifier`. A
    /// native-profile entry has no manifest.
    fn manifest_binds(&self, entry: &DocumentEntry, identifier: &str) -> bool {
        if self.root.profile == FilesystemProfile::Native {
            return entry.manifest.is_empty();
        }
        let record = [self.kind_folder.as_str(), self.name.as_str(), identifier];
        SidecarAttributes::decode(&entry.manifest)
            .is_ok_and(|values| SidecarAttributes::binds(&values, record, &entry.payload))
    }

    fn stored_from(&self, entry: &DocumentEntry, identifier: &str) -> Option<StoredBytes> {
        let relative = Path::new(&entry.relative_path);
        Self::is_document_path(relative, identifier).then(|| StoredBytes {
            bytes: entry.payload.clone(),
            modified_millis: entry.modified_millis,
            path: self.path.join(Self::LIVE_FOLDER).join(relative),
        })
    }

    fn is_document_path(relative: &Path, identifier: &str) -> bool {
        let plain = relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        let name = relative.file_name().and_then(|name| name.to_str());
        plain && name == Some(&format!("{identifier}{}", Self::DOCUMENT_SUFFIX))
    }

    /// An entry read under an older stamp: the collection changed since.
    /// It is used only when the document file on the root is still the file
    /// it was read from, and then kept again under the current stamp.
    fn revalidated(
        &self,
        reading: &TierReading<'_>,
        entry: DocumentEntry,
        stored: StoredBytes,
        identifier: &str,
    ) -> Option<StoredBytes> {
        reading.tier.count_revalidation();
        let current = self.document_stamp(identifier, &stored.path);
        if current != Some(entry.document_stamp) {
            Self::refuse(reading);
            return None;
        }
        reading.tier.count_hit();
        let refreshed = DocumentEntry {
            collection_stamp: reading.stamp,
            ..entry
        };
        self.keep(reading, &refreshed);
        Some(stored)
    }

    /// The digest of the document's file stamps on the root now: its
    /// payload and, in the EFS profile, its manifest.
    fn document_stamp(&self, identifier: &str, path: &Path) -> Option<[u8; 32]> {
        let live_folder = self.path.join(Self::LIVE_FOLDER);
        let (opened, (file, _)) = self.open_document(&live_folder, identifier).ok()?;
        let payload = FileStamp::of_file(&file).ok()?;
        (opened == path).then_some(())?;
        self.record_digest(payload, &opened)
    }

    /// `payload`'s stamp digested with the stamp of its manifest file in
    /// the EFS profile; `None` when the manifest cannot be opened.
    fn record_digest(&self, payload: FileStamp, path: &Path) -> Option<[u8; 32]> {
        if self.root.profile == FilesystemProfile::Native {
            return Some(payload.digest_with(None));
        }
        let manifest = crate::sidecar_path(path);
        let (file, _) = self
            .root
            .open_file(&manifest, crate::MAX_DOCUMENT_BYTES)
            .ok()?;
        let manifest = FileStamp::of_file(&file).ok()?;
        Some(payload.digest_with(Some(&manifest)))
    }

    fn refuse(reading: &TierReading<'_>) {
        reading.tier.reject(&reading.key);
        reading.tier.count_miss();
    }

    fn read_and_keep(
        &self,
        reading: &TierReading<'_>,
        identifier: &str,
    ) -> Result<StoredBytes, NativeStorageError> {
        let live_folder = self.path.join(Self::LIVE_FOLDER);
        let opened = |file: &fylo_vfs::File| FileStamp::of_file(file).ok();
        let (stored, opened, check) =
            self.read_document_file(&live_folder, identifier, opened, RecordCheck::of)?;
        let entry = self.entry_for(reading, &stored, (opened, check), identifier);
        if let Some(entry) = entry {
            self.keep(reading, &entry);
        }
        Ok(stored)
    }

    fn entry_for(
        &self,
        reading: &TierReading<'_>,
        stored: &StoredBytes,
        (opened, check): (Option<FileStamp>, RecordCheck),
        identifier: &str,
    ) -> Option<DocumentEntry> {
        let relative = stored
            .path
            .strip_prefix(self.path.join(Self::LIVE_FOLDER))
            .ok()?;
        let document_stamp = check.unchanged_digest(opened)?;
        let entry = DocumentEntry {
            collection: reading.collection.clone(),
            identifier: identifier.to_owned(),
            collection_stamp: reading.stamp,
            document_stamp,
            modified_millis: stored.modified_millis,
            relative_path: relative.to_str()?.to_owned(),
            payload: stored.bytes.clone(),
            manifest: check.manifest_bytes()?,
        };
        let admitted = entry.fits() && reading.tier.admits(entry.stored_bytes());
        admitted.then_some(entry)
    }

    fn keep(&self, reading: &TierReading<'_>, entry: &DocumentEntry) {
        let pending = PendingEntry {
            key: reading.key.clone(),
            stamp: entry.collection_stamp,
            bytes: entry.encode(),
        };
        self.window.defer(pending, reading.tier.pending_limit());
    }
}
