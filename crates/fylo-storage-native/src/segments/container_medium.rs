//! A segment collection kept in a `.fxdb` file
//! (`docs/fxdb-segment-backend.md`, step 3 and "Root integration").
//!
//! A collection's segments as `segment` extents, its generation slots, and
//! its location, prefix-index and schemas snapshots as `locations`, `index`
//! and `schemas` runs named by its checkpoint slot. There is no location or
//! index log; a location table and the index replay the records after the
//! snapshots' position.
//!
//! The file is either `segments.fxdb` in a collection directory, a hybrid
//! stepping stone toward ADR 0016 that holds that one collection while the
//! collection lock files and the catalog descriptor stay in the directory,
//! or a `.fxdb` database root that holds every collection. Both are one
//! [`ContainerOpen`] per file in a process, shared by every medium over it
//! ([`open`]), and one medium per collection ([`ContainerMedium::attach`]).
//!
//! A medium reads without a lock. Its first write opens the file to write
//! and claims the collection: where writers share the file under
//! byte-range locks, the collection's writer byte is taken and held while
//! the medium is open, so one process at a time writes a collection, and
//! under the lease this process is the only writer. Either way the claim
//! then settles what a writer that stopped left in the collection.
//!
//! Records are the plain frames a directory collection writes: no codecs and
//! no encryption yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fylo_format::{
    ContainerCheckpointSlot, ContainerExtentKind, ContainerGenerationSlot, ContainerSuperblock,
    LogScan, LogSegmentHeader,
};

use super::{
    MediumIdentity, RecordPosition, SegmentHandle, SegmentMedium, SlotRecord, SnapshotStamp,
};
use crate::{
    ContainerError, ContainerFile, Durability, NativeRoot, NativeStorageError,
    NativeStorageErrorCode,
};
pub(crate) use open::ContainerOpen;
use open::Open;
use schemas::SCHEMAS;

const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;
const LOCATIONS: ContainerExtentKind = ContainerExtentKind::Locations;
const INDEX: ContainerExtentKind = ContainerExtentKind::Index;

/// A segment collection whose segments, slots and snapshots live in a
/// `.fxdb` file.
///
/// A segment's number is the sequence number of the extent that holds it,
/// which is never reused: a location into an extent that was released and
/// taken again fails the header check, and the reader retries.
#[derive(Clone, Debug)]
pub(crate) struct ContainerMedium {
    shared: Arc<Shared>,
}

/// What every handle on one collection shares.
#[derive(Debug)]
struct Shared {
    file: Arc<ContainerOpen>,
    identity: MediumIdentity,
    collection: u32,
    block: u32,
    capacity: u64,
    state: Mutex<MediumState>,
    /// Set once this medium may write the collection.
    claim: Mutex<Option<Claim>>,
}

/// This process's right to write one collection.
#[derive(Debug)]
struct Claim {
    /// The writer of the open it was made under ([`Open::writer_epoch`]).
    epoch: u64,
    /// The collection's writer byte, where writers share the file under
    /// byte-range locks.
    #[cfg(not(target_family = "wasm"))]
    _writer: Option<crate::ByteLockGuard>,
}

/// What this process knows about the collection's segments beyond the file.
#[derive(Debug, Default)]
struct MediumState {
    /// Bytes written to each segment this process opened for writing, which
    /// its slot may not publish yet.
    written: BTreeMap<u64, u64>,
    /// Valid lengths of segments found by a scan. A segment no writer holds
    /// never changes, and its number is never reused.
    scanned: BTreeMap<u64, u64>,
    /// This process's compacted copy, not yet adopted, as (extent, number).
    compaction: Option<(u64, u64)>,
}

impl ContainerMedium {
    /// The file in a collection directory that makes it a container
    /// collection.
    pub(crate) const FILE_NAME: &'static str = "segments.fxdb";

    /// The medium of the collection in `directory`, or `None` when the
    /// directory holds no container.
    ///
    /// Every medium of one collection open in the process at once is one,
    /// over the process's one open of the file, and both close when the
    /// last of them drops: the registries keep none alive. A file dropped
    /// and created again at the same path is then opened afresh, and a
    /// writer's claim on the collection lasts as long as some operation
    /// holds the medium.
    pub(crate) fn of(directory: &Path) -> Result<Option<Self>, NativeStorageError> {
        let path = directory.join(Self::FILE_NAME);
        let file = match ContainerOpen::registered(&path) {
            Some(file) => file,
            None if !crate::path_exists_no_follow(&path)? => return Ok(None),
            None => ContainerOpen::open(&path).map_err(Self::error)?,
        };
        let live = |open: &Open| {
            open.collections()
                .iter()
                .find(|collection| !collection.header.dropped)
                .cloned()
        };
        let collection = {
            let mut open = file.lock();
            match live(&open) {
                Some(collection) => Some(collection),
                // Another open in this process may have added it since.
                None => open.refresh()?.then(|| live(&open)).flatten(),
            }
        };
        let collection = collection.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!("{} holds no segment collection", path.display()),
            )
        })?;
        Ok(Some(Self::attach(&file, &collection)))
    }

    /// Create the container of a new collection named `name` in
    /// `directory`, which exists, with extents of `1 << extent_shift` bytes.
    /// A container a crash left without its collection gets it now.
    pub(crate) fn create(
        directory: &Path,
        name: &str,
        extent_shift: u8,
    ) -> Result<(), NativeStorageError> {
        let path = directory.join(Self::FILE_NAME);
        if !crate::path_exists_no_follow(&path)? {
            ContainerFile::create(&path, extent_shift).map_err(Self::error)?;
            crate::write::sync_directory(directory)?;
        }
        let file = ContainerOpen::open(&path).map_err(Self::error)?;
        file.change_map(|open| {
            if open
                .collections()
                .iter()
                .all(|collection| collection.header.dropped)
            {
                open.add_collection(name, &Self::descriptor(serde_json::Map::new()))
                    .map_err(Self::error)?;
                open.checkpoint()?;
            }
            Ok(())
        })
    }

    /// A collection's descriptor with `fields`, and a `created` field that
    /// names this creation: a collection created again at the same path or
    /// in a file created again there must not share a cached location table
    /// or index with the one it replaces ([`SegmentMedium::identity`]).
    pub(crate) fn descriptor(mut fields: serde_json::Map<String, serde_json::Value>) -> Vec<u8> {
        fields.insert(
            "created".into(),
            crate::write::unique_name("fxdb-collection").into(),
        );
        serde_json::Value::Object(fields).to_string().into_bytes()
    }

    /// The extent size a test container gets: the smallest, so a segment
    /// fills after a megabyte.
    pub(crate) const TEST_EXTENT_SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;

    /// Forget the open container of the collection in `directory`, as a
    /// process that stopped would, so the next access reads the file afresh
    /// even while a medium of the old open is still held.
    #[cfg(test)]
    pub(crate) fn forget(directory: &Path) {
        ContainerOpen::forget(&directory.join(Self::FILE_NAME));
    }

    /// Open containers to write with `options` from now on, in this whole
    /// process.
    #[cfg(all(test, not(target_family = "wasm")))]
    pub(crate) fn set_write_options(options: crate::ContainerWriteOptions) {
        ContainerOpen::set_write_options(options);
    }

    /// A container failure as a storage error. Other writers keeping the
    /// file (`EBUSY`), a lost lease and the other failures that are no
    /// damage keep their container code in the message.
    pub(crate) fn error(error: ContainerError) -> NativeStorageError {
        let error = match error {
            ContainerError::Io(error) => return NativeStorageError::io(error),
            other => other,
        };
        let code = match &error {
            ContainerError::Busy => NativeStorageErrorCode::RootLocked,
            #[cfg(not(target_family = "wasm"))]
            ContainerError::Lease(_) => NativeStorageErrorCode::RootLeaseLost,
            ContainerError::CollectionExists => NativeStorageErrorCode::InvalidCollection,
            ContainerError::ReadOnly | ContainerError::Locked => {
                NativeStorageErrorCode::Unsupported
            }
            _ => NativeStorageErrorCode::CorruptMetadata,
        };
        if code == NativeStorageErrorCode::CorruptMetadata {
            NativeStorageError::new(code, format!("segment container: {error}"))
        } else {
            NativeStorageError::new(
                code,
                format!("segment container: {error} ({})", error.code()),
            )
        }
    }

    /// A segment that was released or reused, or that ends before a
    /// location, means the generation moved under the reader, which readers
    /// retry.
    fn moved() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::ConcurrentWrite,
            "a segment changed while it was read",
        )
    }

    fn handle(&self, (extent, number): (u64, u64)) -> Arc<dyn SegmentHandle> {
        Arc::new(ContainerSegment {
            shared: Arc::clone(&self.shared),
            extent,
            number,
        })
    }

    /// A generation slot as the segment code reads it.
    fn slot_record(slot: &ContainerGenerationSlot) -> SlotRecord {
        SlotRecord {
            generation: slot.generation,
            writing: slot.writing,
            segment: slot.active_sequence,
            length: slot.committed_length,
            next_lsn: slot.next_lsn,
            records: slot.record_count,
            locations_length: 0,
            table_epoch: slot.table_epoch,
        }
    }

    /// Claim the collection for this process's writes, if this medium has
    /// not yet ([`Shared::writable`]).
    pub(crate) fn claim(&self) -> Result<(), NativeStorageError> {
        self.shared.writable().map(drop)
    }
}

impl Shared {
    fn file(&self) -> MutexGuard<'_, Open> {
        self.file.lock()
    }

    fn state(&self) -> MutexGuard<'_, MediumState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The file, open to write and with this process's right to write it
    /// and the collection checked. The first call claims the collection
    /// ([`Self::claim`]).
    ///
    /// A claim holds while the open stays the writer it was made under.
    /// Once it is another, as after a lost lease, the collection is claimed
    /// again and what this medium knew of unpublished bytes is dropped,
    /// since another process may have written the collection between.
    fn writable(&self) -> Result<MutexGuard<'_, Open>, NativeStorageError> {
        let mut claim = self.claim.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let open = self.file.writable()?;
            if claim
                .as_ref()
                .is_some_and(|claim| claim.epoch == open.writer_epoch())
            {
                return Ok(open);
            }
            drop(open);
            *claim = None;
            self.state().written.clear();
            *claim = Some(self.claim()?);
        }
    }

    /// Claim the collection: where writers share the file, take its writer
    /// byte, waiting for the process that holds it, then settle what a
    /// writer that stopped left. The open is not held while waiting, so the
    /// process's other collections in the file carry on.
    fn claim(&self) -> Result<Claim, NativeStorageError> {
        #[cfg(not(target_family = "wasm"))]
        let writer = {
            let open = self.file.writable()?;
            match open.access.as_ref() {
                Some(right) if right.shares_the_file() => {
                    let (handle, wait) = (
                        open.lock_handle().map_err(ContainerMedium::error)?,
                        right.busy_timeout,
                    );
                    drop(open);
                    Some(
                        crate::container_file::lock_byte(
                            &handle,
                            crate::LockByte::collection_writer(self.block),
                            wait,
                        )
                        .map_err(ContainerMedium::error)?,
                    )
                }
                _ => None,
            }
        };
        let epoch = self.file.change_map(|open| {
            self.settle(open)?;
            Ok(open.writer_epoch())
        })?;
        Ok(Claim {
            epoch,
            #[cfg(not(target_family = "wasm"))]
            _writer: writer,
        })
    }

    /// Settle what a writer that stopped left in the collection, with the
    /// map read afresh: the active segment extent its slot names but the
    /// map does not show is taken back, and snapshot runs the newest
    /// checkpoint slot does not name are released. A collection dropped
    /// since this medium opened is not found.
    fn settle(&self, open: &mut Open) -> Result<(), NativeStorageError> {
        let (id, block) = (self.collection, self.block);
        if !open.collections().iter().any(|collection| {
            collection.block == block && collection.header.id == id && !collection.header.dropped
        }) {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                "the collection was dropped from its .fxdb file",
            ));
        }
        if let Some(slot) = open
            .read_generation(block)
            .map_err(ContainerMedium::error)?
            && open.allocator().in_use(slot.active_extent).is_none()
        {
            open.reclaim(slot.active_extent, SEGMENT, id, slot.active_sequence)
                .map_err(ContainerMedium::error)?;
            open.table(id)
                .insert(slot.active_sequence, slot.active_extent);
        }
        let checkpoint = open
            .read_checkpoint(block)
            .map_err(ContainerMedium::error)?;
        let named = checkpoint
            .and_then(|checkpoint| checkpoint.locations)
            .map_or(0..0, |run| {
                run.first_extent..run.first_extent + u64::from(run.extent_count)
            });
        for (extent, _) in open.allocator().owned(LOCATIONS, id) {
            if !named.contains(&extent) {
                open.release(extent).map_err(ContainerMedium::error)?;
            }
        }
        for (kind, kept) in [
            (INDEX, checkpoint.and_then(|checkpoint| checkpoint.index)),
            (
                SCHEMAS,
                checkpoint.and_then(|checkpoint| checkpoint.schemas),
            ),
        ] {
            ContainerMedium::release_runs(open, (id, kind), kept.as_ref())
                .map_err(ContainerMedium::error)?;
        }
        Ok(())
    }

    /// The extent holding segment `number`, while it is one of this
    /// collection's segments.
    ///
    /// The segment table answers. When it does not know the number, the
    /// open may be older than another writer's map: it reads the map again
    /// if a checkpoint moved it, and else takes back, in memory, the extent
    /// the generation slot names, which no map on disk may show yet.
    fn extent(&self, open: &mut Open, number: u64) -> Result<Option<u64>, NativeStorageError> {
        if let Some(extent) = open.table(self.collection).get(&number) {
            return Ok(Some(*extent));
        }
        if open.refresh()?
            && let Some(extent) = open.table(self.collection).get(&number)
        {
            return Ok(Some(*extent));
        }
        let Some(slot) = open
            .read_generation(self.block)
            .map_err(ContainerMedium::error)?
            .filter(|slot| slot.active_sequence == number)
        else {
            return Ok(None);
        };
        if open.allocator().in_use(slot.active_extent).is_some()
            || open
                .reclaim(slot.active_extent, SEGMENT, self.collection, number)
                .is_err()
        {
            return Ok(None);
        }
        open.table(self.collection)
            .insert(number, slot.active_extent);
        Ok(Some(slot.active_extent))
    }

    /// Bring the segment table up to the file: reread the map if a
    /// checkpoint moved it, and add the active segment its slot names.
    fn follow(&self, open: &mut Open) -> Result<(), NativeStorageError> {
        open.refresh()?;
        if let Some(slot) = open
            .read_generation(self.block)
            .map_err(ContainerMedium::error)?
        {
            self.extent(open, slot.active_sequence)?;
        }
        Ok(())
    }

    /// Read payload bytes of segment `number` in `extent` after checking
    /// the extent still holds it, in the map and in its header.
    fn read_checked(
        &self,
        file: &ContainerFile,
        (extent, number): (u64, u64),
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), NativeStorageError> {
        let held = file.allocator().in_use(extent) == Some((SEGMENT, self.collection, number));
        if !held
            || file
                .expect_extent(extent, SEGMENT, self.collection, number)
                .is_err()
        {
            return Err(ContainerMedium::moved());
        }
        file.read_payload(extent, offset, buffer)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::UnexpectedEof => {
                    ContainerMedium::moved()
                }
                _ => NativeStorageError::io(error),
            })
    }

    /// Every payload byte of segment `number`, or `None` when the
    /// collection holds no such segment.
    fn payload(&self, number: u64) -> Result<Option<Vec<u8>>, NativeStorageError> {
        let mut file = self.file();
        let Some(extent) = self.extent(&mut file, number)? else {
            return Ok(None);
        };
        let size = usize::try_from(self.capacity).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "segment extent exceeds the address space",
            )
        })?;
        let mut bytes = vec![0_u8; size];
        self.read_checked(&file, (extent, number), 0, &mut bytes)?;
        Ok(Some(bytes))
    }

    /// Bytes segment `number` holds: what this process wrote to it, the
    /// committed length the slot publishes for the active segment, or else
    /// its last valid frame's end, found once by a scan.
    fn length(&self, number: u64) -> Result<u64, NativeStorageError> {
        if let Some(length) = self.state().written.get(&number) {
            return Ok(*length);
        }
        let slot = self
            .file()
            .read_generation(self.block)
            .map_err(ContainerMedium::error)?;
        if let Some(slot) = slot.filter(|slot| slot.active_sequence == number) {
            return Ok(slot.committed_length);
        }
        if let Some(length) = self.state().scanned.get(&number) {
            return Ok(*length);
        }
        let Some(payload) = self.payload(number)? else {
            return Ok(0);
        };
        let length = LogScan::segment(&payload).map_or(0, |scan| scan.valid_length);
        self.state().scanned.insert(number, length);
        Ok(length)
    }

    fn generation_slot(
        &self,
        open: &mut Open,
        slot: &SlotRecord,
    ) -> Result<ContainerGenerationSlot, NativeStorageError> {
        let extent = self.extent(open, slot.segment)?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!(
                    "slot names segment {}, which the container does not hold",
                    slot.segment
                ),
            )
        })?;
        Ok(ContainerGenerationSlot {
            generation: slot.generation,
            writing: slot.writing,
            active_extent: extent,
            active_sequence: slot.segment,
            committed_length: slot.length,
            next_lsn: slot.next_lsn,
            record_count: slot.records,
            table_epoch: slot.table_epoch,
        })
    }
}

impl SegmentMedium for ContainerMedium {
    /// The file's path, the collection's id and a digest of its
    /// descriptor, which names when it was created: a collection dropped
    /// and created again at the same path is another collection.
    fn identity(&self) -> MediumIdentity {
        self.shared.identity.clone()
    }

    /// An extent's payload.
    fn max_segment_bytes(&self) -> u64 {
        self.shared.capacity
    }

    /// The collection's segment extents by sequence number, without the
    /// compacted copy this process has not adopted yet.
    fn segment_numbers(&self) -> Result<Vec<u64>, NativeStorageError> {
        let pending = self.shared.state().compaction.map(|(_, number)| number);
        let mut open = self.shared.file();
        self.shared.follow(&mut open)?;
        Ok(open
            .table(self.shared.collection)
            .keys()
            .copied()
            .filter(|number| Some(*number) != pending)
            .collect())
    }

    fn segment_length(&self, number: u64) -> Result<u64, NativeStorageError> {
        self.shared.length(number)
    }

    /// The payload up to the last valid frame or the last byte written,
    /// whichever is further, so a recovery sees a torn tail as a directory
    /// collection's longer file shows it. A reused extent was cleared before
    /// its header was written, so nothing past that is a former owner's.
    fn read_segment(&self, number: u64) -> Result<Option<Vec<u8>>, NativeStorageError> {
        let Some(mut payload) = self.shared.payload(number)? else {
            return Ok(None);
        };
        let valid = LogScan::segment(&payload).map_or(0, |scan| scan.valid_length);
        let written = self
            .shared
            .state()
            .written
            .get(&number)
            .copied()
            .unwrap_or(0);
        let touched = payload
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |at| at as u64 + 1);
        let end = valid.max(written).max(touched);
        payload.truncate(usize::try_from(end).unwrap_or(payload.len()));
        Ok(Some(payload))
    }

    fn read_handle(&self, number: u64) -> Result<Arc<dyn SegmentHandle>, NativeStorageError> {
        let extent = self
            .shared
            .extent(&mut self.shared.file(), number)?
            .ok_or_else(Self::moved)?;
        Ok(self.handle((extent, number)))
    }

    fn write_handle(
        &self,
        number: u64,
    ) -> Result<Option<Arc<dyn SegmentHandle>>, NativeStorageError> {
        let Some(extent) = self.shared.extent(&mut *self.shared.writable()?, number)? else {
            return Ok(None);
        };
        let length = self.shared.length(number)?;
        self.shared.state().written.entry(number).or_insert(length);
        Ok(Some(self.handle((extent, number))))
    }

    /// The next number is the sequence number of a newly taken extent.
    /// The map is checkpointed once the extent holds its opening bytes,
    /// before any slot names it, so every segment a slot can name is in the
    /// durable map, and so is the seal of the segment just finished
    /// (`docs/fxdb-segment-backend.md`, "Taking a new extent"). A process
    /// that stops before the checkpoint leaves an extent no slot names,
    /// which the durable map shows free.
    fn create_segment(
        &self,
        opening: &dyn Fn(u64) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(u64, Arc<dyn SegmentHandle>), NativeStorageError> {
        let id = self.shared.collection;
        let (allocation, length) = self.shared.writable()?.change_map(|open| {
            let allocation = open.allocate(SEGMENT, id).map_err(Self::error)?;
            let written = opening(allocation.sequence).and_then(|bytes| {
                if bytes.len() as u64 > self.shared.capacity {
                    return Err(ContainerSegment::too_large());
                }
                open.write_payload(allocation.index, 0, &bytes)
                    .map_err(NativeStorageError::io)?;
                Ok(bytes.len() as u64)
            });
            match written {
                Ok(length) => {
                    open.table(id).insert(allocation.sequence, allocation.index);
                    Ok((allocation, length))
                }
                Err(error) => {
                    let _ = open.release(allocation.index);
                    Err(error)
                }
            }
        })?;
        self.shared
            .state()
            .written
            .insert(allocation.sequence, length);
        Ok((
            allocation.sequence,
            self.handle((allocation.index, allocation.sequence)),
        ))
    }

    /// Seal the extent in the map. The map checkpoint that taking the next
    /// segment's extent runs makes it durable.
    fn seal_segment(&self, number: u64, live_bytes: u64) -> Result<(), NativeStorageError> {
        let mut open = self.shared.writable()?;
        let Some(extent) = self.shared.extent(&mut open, number)? else {
            return Ok(());
        };
        open.seal((extent, self.shared.collection, number), live_bytes)
    }

    /// Release the extent and checkpoint the map, so the release outlives
    /// this open: the medium closes once no operation holds it. The extent
    /// may be reused from then on, and a reader still holding a location
    /// into it fails the header check and retries.
    fn remove_segment(&self, number: u64) -> Result<(), NativeStorageError> {
        let mut open = self.shared.writable()?;
        let Some(extent) = self.shared.extent(&mut open, number)? else {
            return Ok(());
        };
        open.change_map(|open| {
            open.release(extent).map_err(Self::error)?;
            open.table(self.shared.collection).remove(&number);
            Ok(())
        })?;
        drop(open);
        let mut state = self.shared.state();
        state.written.remove(&number);
        state.scanned.remove(&number);
        if state
            .compaction
            .is_some_and(|(_, pending)| pending == number)
        {
            state.compaction = None;
        }
        Ok(())
    }

    /// The container file.
    fn segment_path(&self, _number: u64) -> PathBuf {
        self.shared.file.path().to_owned()
    }

    /// Take a segment extent for the copy, checkpointing the map. Its
    /// sequence number is the number the copy is adopted under.
    fn create_compaction(&self) -> Result<Arc<dyn SegmentHandle>, NativeStorageError> {
        self.remove_compactions()?;
        let id = self.shared.collection;
        let allocation = self.shared.writable()?.change_map(|open| {
            let allocation = open.allocate(SEGMENT, id).map_err(Self::error)?;
            open.table(id).insert(allocation.sequence, allocation.index);
            Ok(allocation)
        })?;
        let mut state = self.shared.state();
        state.compaction = Some((allocation.index, allocation.sequence));
        state.written.insert(allocation.sequence, 0);
        drop(state);
        Ok(self.handle((allocation.index, allocation.sequence)))
    }

    fn read_compaction(&self) -> Result<Vec<u8>, NativeStorageError> {
        let (extent, number) = self.pending_compaction()?;
        let length = self.shared.length(number)?;
        let mut bytes = vec![0_u8; usize::try_from(length).unwrap_or(usize::MAX)];
        let file = self.shared.file();
        self.shared
            .read_checked(&file, (extent, number), 0, &mut bytes)?;
        Ok(bytes)
    }

    /// Seal the copy, which is finished and holds only live entries, and
    /// checkpoint the map, so the copy is in it before the compaction
    /// record that adopts it.
    fn adopt_compaction(&self) -> Result<u64, NativeStorageError> {
        let (extent, number) = self.pending_compaction()?;
        let live = self
            .shared
            .length(number)?
            .saturating_sub(LogSegmentHeader::ENCODED_BYTES as u64);
        self.shared
            .writable()?
            .change_map(|open| open.file.seal(extent, live).map_err(Self::error))?;
        self.shared.state().compaction = None;
        Ok(number)
    }

    /// Release this process's unadopted copy. A copy a stopped process
    /// left is a segment no location refers to, which the next recovery or
    /// compaction removes.
    fn remove_compactions(&self) -> Result<(), NativeStorageError> {
        let pending = self.shared.state().compaction.take();
        if let Some((extent, number)) = pending {
            self.shared.writable()?.change_map(|open| {
                open.release(extent).map_err(Self::error)?;
                open.table(self.shared.collection).remove(&number);
                Ok(())
            })?;
            self.shared.state().written.remove(&number);
        }
        Ok(())
    }

    /// The collection's generation slot pair in its directory block.
    fn read_slot(&self) -> Result<Option<SlotRecord>, NativeStorageError> {
        let slot = self
            .shared
            .file()
            .read_generation(self.shared.block)
            .map_err(Self::error)?;
        Ok(slot.as_ref().map(Self::slot_record))
    }

    /// Every publication first checks this process may still write.
    fn publish_slot(&self, slot: &SlotRecord) -> Result<(), NativeStorageError> {
        let mut open = self.shared.writable()?;
        let slot = self.shared.generation_slot(&mut open, slot)?;
        open.publish_generation(self.shared.block, slot)
            .map_err(Self::error)
    }

    fn create_slots(&self, slot: &SlotRecord) -> Result<(), NativeStorageError> {
        self.publish_slot(slot)?;
        self.sync_slots()
    }

    fn sync_slots(&self) -> Result<(), NativeStorageError> {
        self.shared.file().sync().map_err(NativeStorageError::io)
    }

    /// A valid generation slot exists.
    fn has_slots(&self) -> Result<bool, NativeStorageError> {
        Ok(self.read_slot()?.is_some())
    }

    /// The container already exists.
    fn create_storage(&self, _root: &NativeRoot) -> Result<(), NativeStorageError> {
        Ok(())
    }

    fn read_location_snapshot(&self) -> Result<Vec<u8>, NativeStorageError> {
        Ok(self.read_location_checkpoint()?.0)
    }

    /// Write the snapshot into a new `locations` run, checkpoint the map
    /// so it shows the run, record a checkpoint slot naming it and the end
    /// of the records the newest generation slot publishes, release the
    /// run the previous checkpoint named and checkpoint the map again. A
    /// stop between those steps leaves a run no slot names, which the next
    /// claim of the collection releases.
    ///
    /// The prefix index folds with it into an `index` run the same slot
    /// names, and every other index run of the collection is released. The
    /// slot keeps the schemas run the previous one named.
    fn replace_location_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        let open = self.shared.writable()?;
        let slot = open
            .read_generation(block)
            .map_err(Self::error)?
            .ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "segment collection has no valid generation slot",
                )
            })?;
        let previous = open.read_checkpoint(block).map_err(Self::error)?;
        // The index folds with the locations; its replay takes the open.
        drop(open);
        let index = self.folded_index(
            previous.as_ref(),
            RecordPosition::published(&Self::slot_record(&slot)),
        )?;
        self.shared.writable()?.change_map(|open| {
            let checkpoint = previous.map_or(1, |previous| previous.checkpoint + 1);
            let locations = open
                .write_snapshot(id, LOCATIONS, checkpoint, bytes)
                .map_err(Self::error)?;
            let index = match index {
                Some(index) => Some(
                    open.write_snapshot(id, INDEX, checkpoint, &index)
                        .map_err(Self::error)?,
                ),
                None => None,
            };
            let schemas = Self::carried_schemas(open, id, previous.as_ref(), checkpoint)
                .map_err(Self::error)?;
            open.checkpoint()?;
            open.record_checkpoint(
                block,
                ContainerCheckpointSlot {
                    checkpoint,
                    lsn: slot.next_lsn,
                    extent: slot.active_extent,
                    extent_sequence: slot.active_sequence,
                    offset: slot.committed_length,
                    locations: Some(locations),
                    index,
                    schemas,
                    dictionary: None,
                },
            )
            .map_err(Self::error)?;
            if let Some(old) = previous.and_then(|previous| previous.locations) {
                open.release_snapshot(&old).map_err(Self::error)?;
            }
            Self::release_runs(open, (id, INDEX), index.as_ref()).map_err(Self::error)?;
            Self::release_runs(open, (id, SCHEMAS), schemas.as_ref()).map_err(Self::error)
        })
    }

    /// The `schemas` run the newest checkpoint slot names, or `None` when
    /// it names none: the collection has no schema version, or was written
    /// before schemas runs existed, and the registry is then rebuilt from
    /// the records ([`SegmentSchemas::load`]).
    fn read_schemas_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        self.schemas_snapshot()
    }

    /// The run's length and SHA-256 as the slot records them.
    fn schemas_snapshot_stamp(&self) -> Result<Option<SnapshotStamp>, NativeStorageError> {
        self.schemas_stamp()
    }

    /// Write a new `schemas` run and a checkpoint slot naming it. The empty
    /// registry is kept as no run.
    fn replace_schemas_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        self.replace_schemas(Some(bytes))
    }

    /// The same steps as a replace, which pass no failpoint.
    fn restore_schemas_snapshot(&self, previous: Option<&[u8]>) -> Result<(), NativeStorageError> {
        if self.read_schemas_snapshot()?.as_deref() == previous {
            return Ok(());
        }
        self.replace_schemas(previous)
    }

    /// Set `READ_ONLY_COMPAT_SCHEMAS` in the superblock, durably, unless
    /// the file has it.
    fn mark_schemas(&self) -> Result<(), NativeStorageError> {
        let feature = ContainerSuperblock::READ_ONLY_COMPAT_SCHEMAS;
        let mut open = self.shared.writable()?;
        if open.superblock().read_only_compat & feature != 0 {
            return Ok(());
        }
        open.change_map(|open| open.add_read_only_feature(feature))
    }

    fn has_location_log(&self) -> bool {
        false
    }

    /// The run the newest checkpoint slot names and the position it
    /// records, read under one lock so they belong together. A collection
    /// that never checkpointed has an empty snapshot reaching back to the
    /// start.
    fn read_location_checkpoint(&self) -> Result<(Vec<u8>, RecordPosition), NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        let file = self.shared.file();
        let Some(checkpoint) = file.read_checkpoint(block).map_err(Self::error)? else {
            return Ok((Vec::new(), RecordPosition::START));
        };
        let bytes = match checkpoint.locations {
            Some(run) => file
                .read_snapshot(id, LOCATIONS, checkpoint.checkpoint, &run, u64::MAX)
                .map_err(|error| {
                    NativeStorageError::new(
                        NativeStorageErrorCode::CorruptMetadata,
                        format!("segment location snapshot is damaged: {error}"),
                    )
                })?,
            None => Vec::new(),
        };
        let position = RecordPosition {
            segment: checkpoint.extent_sequence,
            offset: checkpoint.offset,
        };
        Ok((bytes, position))
    }

    fn location_log_length(&self) -> Result<u64, NativeStorageError> {
        Ok(0)
    }

    fn read_location_log(&self, _start: u64, _end: u64) -> Result<Vec<u8>, NativeStorageError> {
        Ok(Vec::new())
    }

    fn append_location_log(&self, _bytes: &[u8]) -> Result<(), NativeStorageError> {
        Ok(())
    }

    fn truncate_location_log(&self, _length: u64) -> Result<(), NativeStorageError> {
        Ok(())
    }

    fn reset_location_log(&self) -> Result<(), NativeStorageError> {
        Ok(())
    }

    fn sync_location_log(&self) -> Result<(), NativeStorageError> {
        Ok(())
    }

    /// The container file's owner.
    #[cfg(unix)]
    fn owner(&self) -> Result<(u32, u32), NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let metadata =
            fylo_vfs::symlink_metadata(self.shared.file.path()).map_err(NativeStorageError::io)?;
        Ok((metadata.uid(), metadata.gid()))
    }

    /// The index snapshot is a run the checkpoint slot names, and the keys
    /// past it are replayed from the records.
    fn keeps_index(&self) -> bool {
        true
    }

    fn read_index(&self, to: RecordPosition) -> Result<(Vec<u8>, Vec<u8>), NativeStorageError> {
        self.index_at(to)
    }

    fn read_index_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        self.index_snapshot()
    }

    fn replace_index_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        self.replace_index(bytes)
    }
}

impl ContainerMedium {
    fn pending_compaction(&self) -> Result<(u64, u64), NativeStorageError> {
        self.shared.state().compaction.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                "no compacted copy is in progress",
            )
        })
    }
}

/// One segment extent, open to read or write.
#[derive(Debug)]
struct ContainerSegment {
    shared: Arc<Shared>,
    extent: u64,
    number: u64,
}

impl ContainerSegment {
    const CLEAR_BYTES: usize = 64 * 1024;

    fn too_large() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::FileTooLarge,
            "segment exceeds its container extent",
        )
    }
}

impl SegmentHandle for ContainerSegment {
    fn length(&self) -> Result<u64, NativeStorageError> {
        self.shared.length(self.number)
    }

    /// Every read checks the extent's header first.
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> Result<(), NativeStorageError> {
        let file = self.shared.file();
        self.shared
            .read_checked(&file, (self.extent, self.number), offset, buffer)
    }

    fn write_at(&self, bytes: &[u8], offset: u64) -> Result<(), NativeStorageError> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= self.shared.capacity)
            .ok_or_else(Self::too_large)?;
        self.shared
            .file()
            .write_payload(self.extent, offset, bytes)
            .map_err(NativeStorageError::io)?;
        let mut state = self.shared.state();
        let written = state.written.entry(self.number).or_insert(0);
        *written = (*written).max(end);
        Ok(())
    }

    /// Nothing is cut: the bytes past `length` are zeroed, so a later scan
    /// of the extent ends at `length`.
    fn truncate(&self, length: u64) -> Result<(), NativeStorageError> {
        let file = self.shared.file();
        let zeros = vec![0_u8; Self::CLEAR_BYTES];
        let mut chunk = vec![0_u8; Self::CLEAR_BYTES];
        let mut at = length.min(self.shared.capacity);
        while at < self.shared.capacity {
            let size = usize::try_from(self.shared.capacity - at)
                .map_or(Self::CLEAR_BYTES, |left| left.min(Self::CLEAR_BYTES));
            self.shared
                .read_checked(&file, (self.extent, self.number), at, &mut chunk[..size])?;
            if chunk[..size].iter().any(|byte| *byte != 0) {
                file.write_payload(self.extent, at, &zeros[..size])
                    .map_err(NativeStorageError::io)?;
            }
            at += size as u64;
        }
        drop(file);
        let mut state = self.shared.state();
        state.written.insert(self.number, length);
        state.scanned.remove(&self.number);
        Ok(())
    }

    /// A flush covers the whole file.
    fn sync(&self, _durability: Durability) -> Result<(), NativeStorageError> {
        self.shared.file().sync().map_err(NativeStorageError::io)
    }

    /// The extent is named by the generation slot or the map, which
    /// [`Self::sync`] flushes with it.
    fn sync_created(&self) -> Result<(), NativeStorageError> {
        Ok(())
    }
}

mod database;
mod open;
mod prefix_index;
mod schemas;
#[cfg(test)]
mod tests;
