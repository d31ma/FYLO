use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, MutexGuard};

use fylo_format::{LogRecord, LogRecordKind};
use fylo_vfs::{self as fs, OpenOptions};

use super::super::schema_counts::SchemaCountDelta;
use super::super::{
    CollectionWriteLock, GenerationRecord, NativeWriteRoot, ensure_directory, json_error,
    sibling_scratch,
};
use super::abort_manifest::AbortManifest;
use super::checkpoint_manifest::CheckpointManifest;
use super::commit_plan::CommitPlan;
use super::commit_queue::{BusyPeriod, CommitQueue, QueueState, SchemaFence};
use super::log_checkpoint::LogCheckpoint;
use super::log_segment::LogSegment;
use super::logged_plan::LoggedPlan;
use super::placed_record::PlacedRecord;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_recovery::SegmentRecovery;
use crate::{
    CollectionKind, FilesystemProfile, GenerationState, GenerationStatus, LogPosition,
    NativeCollection, NativeStorageError, NativeStorageErrorCode, path_exists_no_follow,
};

/// What a plan closure hands back: the plan to commit, if any, and the value
/// the caller returns once it is committed.
pub(crate) type Planned<T, P = CommitPlan> = (Option<P>, T);

impl NativeWriteRoot {
    const LOG_TRANSACTION_PREFIX: &'static str = "log-";
    const CHECKPOINT_RECORDS: u64 = 4_096;
    const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;

    /// Whether this collection's ordinary mutations go through the log.
    pub(crate) fn uses_log(&self, collection: &NativeCollection) -> bool {
        self.filesystem_profile() == FilesystemProfile::Native
            && collection.kind == CollectionKind::Document
            && cfg!(any(unix, windows))
    }

    /// Commit one mutation through the collection's write-ahead log.
    ///
    /// `plan` runs under the collection lock and may run more than once when
    /// another writer in this process holds a commit on the same files.
    pub(crate) fn commit_logged<T, P: LoggedPlan>(
        &self,
        collection: &NativeCollection,
        mut plan: impl FnMut() -> Result<Planned<T, P>, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        let queue = CommitQueue::shared(&collection.path);
        let mut state = queue.lock();
        let (placed, plan, value) = loop {
            while state.exclusive.is_some()
                || state
                    .busy
                    .as_ref()
                    .is_some_and(|busy| busy.failure.is_some())
            {
                state = queue.wait(state);
            }
            if state.busy.is_none() {
                state.origin = Some((self.path().to_owned(), collection.name().to_owned()));
                state.busy = Some(self.begin_busy(collection)?);
            }
            let (planned, value) = match plan() {
                Ok(planned) => planned,
                Err(error) => {
                    Self::end_idle(&queue, &mut state, collection);
                    return Err(error);
                }
            };
            let Some(planned) = planned else {
                Self::end_idle(&queue, &mut state, collection);
                return Ok(value);
            };
            let busy = state.busy.as_mut().expect("busy period started above");
            if planned
                .touched()
                .iter()
                .any(|path| busy.pending.contains(path))
            {
                state = queue.wait(state);
                continue;
            }
            match self.append_commit(collection, busy, &planned) {
                Ok(placed) => break (placed, planned, value),
                Err(error) => {
                    Self::end_idle(&queue, &mut state, collection);
                    return Err(error);
                }
            }
        };
        let flushed = self.flush_until(&queue, state, placed.lsn);
        let outcome = match flushed {
            Ok(()) => self.apply_or_abort(collection, &queue, &placed, &plan),
            Err(error) => Err(error),
        };
        // The plan's files are this member's until it finishes, so what it
        // applied is still there to read.
        let delta = (outcome.is_ok() && collection.tracks_schema_versions())
            .then(|| plan.schema_count_delta(collection));
        let finished = Self::finish_member(collection, &queue, &plan, delta);
        outcome.and(finished).map(|()| value)
    }

    /// Recover the log, if a crash left work in it, before a journal
    /// operation or an explicit recovery, then checkpoint it away.
    pub(crate) fn settle_log(
        &self,
        collection: &NativeCollection,
    ) -> Result<bool, NativeStorageError> {
        let recovered = self.recover_log_if_needed(collection)?;
        let stored = collection.stored_generation()?;
        if stored.state != GenerationStatus::Stable {
            return Ok(recovered);
        }
        if collection.uses_segments() {
            let segment = Self::open_active_segment(collection, &stored)?;
            SegmentCheckpoint::run(collection, segment, stored.generation)?;
            return Ok(recovered);
        }
        LogCheckpoint::run(collection, stored.generation)?;
        Ok(recovered)
    }

    /// Checkpoint every collection this process logged to, so a closing
    /// process leaves the v1 layout that older releases can open.
    ///
    /// A root or collection that was removed since this process wrote to it
    /// has nothing left to checkpoint and is skipped.
    ///
    /// # Errors
    ///
    /// Returns the first recovery or checkpoint failure.
    pub fn close_logs() -> Result<(), NativeStorageError> {
        Self::close_logs_matching(|_| true)
    }

    /// Checkpoint every collection of the root at `root` that this process
    /// logged to, leaving other roots' logs alone.
    ///
    /// # Errors
    ///
    /// Returns the first recovery or checkpoint failure.
    pub fn close_logs_under(root: impl AsRef<Path>) -> Result<(), NativeStorageError> {
        let root = root.as_ref();
        Self::close_logs_matching(|origin| origin == root)
    }

    fn close_logs_matching(selected: impl Fn(&Path) -> bool) -> Result<(), NativeStorageError> {
        for queue in CommitQueue::all() {
            let origin = queue.lock().origin.clone();
            let Some((root, name)) = origin.filter(|(root, _)| selected(root)) else {
                continue;
            };
            match Self::close_log(&root, &name) {
                Err(error) if root.exists() => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }

    fn close_log(root: &Path, name: &str) -> Result<(), NativeStorageError> {
        let writer = Self::open(root)?;
        let Some(collection) = writer.collection_if_present(name)? else {
            return Ok(());
        };
        if !path_exists_no_follow(&collection.log_directory())? {
            return Ok(());
        }
        let _guard = writer.lock_collection(&collection.path)?;
        writer.settle_log(&collection)?;
        Ok(())
    }

    fn begin_busy(&self, collection: &NativeCollection) -> Result<BusyPeriod, NativeStorageError> {
        let lock = CollectionWriteLock::acquire_profiled(
            &self.root,
            &collection.path.join("locks"),
            "collection.lock",
        )?;
        self.require_no_pending_migration(collection)?;
        let mut stored = collection.stored_generation()?;
        if Self::log_needs_recovery(collection, &stored)? {
            self.run_log_recovery(collection, &stored)?;
            stored = collection.stored_generation()?;
        }
        if stored.state == GenerationStatus::Writing {
            self.recover_journal_locked(collection)?;
            stored = collection.stored_generation()?;
        }
        let segment = match &stored.log {
            Some(position) => {
                LogSegment::open_matching(&collection.logged_segment_path(position), position)?
            }
            None => None,
        };
        let segment = Self::rotate_if_full(collection, segment, &stored)?;
        if collection.uses_segments() {
            stored = collection.stored_generation()?;
        }
        let durable = segment.as_ref().map_or((0, 0, 0), |segment| {
            let position = segment.position();
            (position.length, position.next_lsn - 1, position.records)
        });
        Ok(BusyPeriod {
            _lock: Some(lock),
            segment,
            generation: stored.generation,
            published_writing: false,
            members: 0,
            pending: BTreeSet::new(),
            appended_lsn: 0,
            durable_lsn: durable.1,
            durable_length: durable.0,
            durable_records: durable.2,
            flushing: false,
            failure: None,
            stranded: false,
            fence: SchemaFence::new(collection.generation_fenced(&stored)),
            schema_counts: Self::schema_counts_before_commit(collection, stored.generation),
            schema_delta: super::super::schema_counts::SchemaCountDelta::default(),
        })
    }

    /// A segment collection seals a full active segment before the next
    /// busy period appends to it.
    fn rotate_if_full(
        collection: &NativeCollection,
        segment: Option<LogSegment>,
        stored: &GenerationState,
    ) -> Result<Option<LogSegment>, NativeStorageError> {
        if !collection.uses_segments() {
            return Ok(segment);
        }
        let segment = segment.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment collection's active segment does not match its slot",
            )
        })?;
        if !SegmentCheckpoint::wanted(collection, &segment)? {
            return Ok(Some(segment));
        }
        SegmentCheckpoint::run(collection, segment, stored.generation).map(Some)
    }

    pub(crate) fn open_active_segment(
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<LogSegment, NativeStorageError> {
        let position = stored.log.as_ref().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment collection has no segment position",
            )
        })?;
        LogSegment::open_matching(&collection.logged_segment_path(position), position)?.ok_or_else(
            || {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "segment collection's active segment does not match its slot",
                )
            },
        )
    }

    fn run_log_recovery(
        &self,
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return SegmentRecovery::run(self, collection, stored);
        }
        super::log_recovery::LogRecovery::run(self, collection, stored)
    }

    fn append_commit<P: LoggedPlan>(
        &self,
        collection: &NativeCollection,
        busy: &mut BusyPeriod,
        plan: &P,
    ) -> Result<PlacedRecord, NativeStorageError> {
        if busy.segment.is_none() {
            busy.segment = Some(Self::create_segment(
                &self.root,
                collection,
                busy.generation,
            )?);
            busy.durable_length = 0;
            busy.durable_lsn = 0;
            busy.durable_records = 0;
        }
        let segment = busy.segment.as_mut().expect("segment created above");
        if !busy.published_writing {
            let writing = GenerationRecord::log_writing(busy.generation + 1, segment.position())
                .fenced(busy.fence.is_present());
            Self::publish_quietly(collection, &writing)?;
            busy.published_writing = true;
        }
        let lsn = segment.next_lsn();
        let record = LogRecord {
            lsn,
            kind: LogRecordKind::Commit,
            generation: busy.generation + 2,
            manifest: plan.manifest()?,
            blobs: plan.blobs().to_vec(),
        };
        let encoded = segment.encode(&record)?;
        let placed = PlacedRecord {
            segment: segment.sequence(),
            offset: segment.length(),
            length: u32::try_from(encoded.len()).map_err(|_| {
                NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge,
                    "commit record exceeds the segment record limit",
                )
            })?,
            lsn,
        };
        segment.append(&encoded, 1)?;
        busy.members += 1;
        busy.pending.extend(plan.touched().iter().cloned());
        busy.appended_lsn = lsn;
        Ok(placed)
    }

    fn create_segment(
        root: &crate::NativeRoot,
        collection: &NativeCollection,
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        let state_directory = LogCheckpoint::state_path(collection)?
            .parent()
            .map(Path::to_owned)
            .ok_or_else(|| {
                NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "state has no parent")
            })?;
        ensure_directory(root, &state_directory)?;
        let index = collection.path.join("index");
        let checkpoint = CheckpointManifest {
            format: CheckpointManifest::FORMAT.into(),
            keys_wal_length: LogCheckpoint::length(&index.join("keys.wal"))?,
            snapshot_sha256: Self::snapshot_digest(collection)?,
        };
        LogSegment::create(&collection.log_segment_path(), &checkpoint, generation)
    }

    fn snapshot_digest(collection: &NativeCollection) -> Result<String, NativeStorageError> {
        let snapshot = collection.path.join("index").join("keys.snapshot");
        match fs::read(&snapshot) {
            Ok(bytes) => Ok(crate::sha256_hex(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::sha256_hex(b""))
            }
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Wait until `lsn` is durable, flushing for every waiting writer when no
    /// flush is already running.
    fn flush_until<'a>(
        &self,
        queue: &'a Arc<CommitQueue>,
        mut state: MutexGuard<'a, QueueState>,
        lsn: u64,
    ) -> Result<(), NativeStorageError> {
        loop {
            let busy = state
                .busy
                .as_mut()
                .expect("member keeps the busy period open");
            if busy.durable_lsn >= lsn {
                return Ok(());
            }
            if let Some(failure) = &busy.failure {
                return Err(NativeStorageError::new(
                    failure.0,
                    format!(
                        "write-ahead log flush failed; the write was not committed: {}",
                        failure.1
                    ),
                ));
            }
            if busy.flushing {
                state = queue.wait(state);
                continue;
            }
            busy.flushing = true;
            let target = busy.appended_lsn;
            let segment = busy
                .segment
                .as_ref()
                .expect("an appended record has a segment");
            let length = segment.length();
            let records = segment.position().records;
            let flush = segment.flush_handle();
            drop(state);
            let outcome = flush.run(self.config.durability);
            state = queue.lock();
            let busy = state
                .busy
                .as_mut()
                .expect("member keeps the busy period open");
            busy.flushing = false;
            match outcome {
                Ok(()) => {
                    busy.durable_lsn = target;
                    busy.durable_length = length;
                    busy.durable_records = records;
                    if let Some(segment) = busy.segment.as_mut() {
                        segment.mark_flushed();
                    }
                }
                Err(error) => {
                    busy.failure = Some((error.code(), error.to_string()));
                    Self::rewind_after_failed_flush(busy);
                    queue.notify();
                    return Err(error);
                }
            }
            queue.notify();
        }
    }

    /// Drop everything a failed flush covered. A segment with nothing durable
    /// in it is removed outright, so the collection is left as it was.
    fn rewind_after_failed_flush(busy: &mut BusyPeriod) {
        let rewind = (
            busy.durable_length,
            busy.durable_lsn + 1,
            busy.durable_records,
        );
        let Some(segment) = busy.segment.as_mut() else {
            return;
        };
        if rewind.0 > 0 {
            busy.stranded |= segment.rewind(rewind).is_err();
            return;
        }
        let path = segment.path().to_owned();
        busy.segment = None;
        let removed =
            fs::remove_file(&path).and_then(|()| path.parent().map_or(Ok(()), fs::remove_dir));
        busy.stranded |= removed.is_err();
    }

    fn apply_or_abort<P: LoggedPlan>(
        &self,
        collection: &NativeCollection,
        queue: &Arc<CommitQueue>,
        placed: &PlacedRecord,
        plan: &P,
    ) -> Result<(), NativeStorageError> {
        let (error, index_applied) = match plan.apply(&self.root, collection, placed) {
            Ok(()) => return Ok(()),
            Err(failure) => failure,
        };
        match self.roll_back(collection, queue, (placed.lsn, plan), index_applied) {
            Ok(()) => Err(error),
            Err(rollback) => {
                let mut state = queue.lock();
                if let Some(busy) = state.busy.as_mut() {
                    busy.stranded = true;
                }
                Err(NativeStorageError::new(
                    NativeStorageErrorCode::Io,
                    format!(
                        "write failed ({error}) and rollback failed ({rollback}); recovery will complete the committed write"
                    ),
                ))
            }
        }
    }

    fn roll_back<P: LoggedPlan>(
        &self,
        collection: &NativeCollection,
        queue: &Arc<CommitQueue>,
        (lsn, plan): (u64, &P),
        index_applied: bool,
    ) -> Result<(), NativeStorageError> {
        plan.undo(&self.root, collection, index_applied)?;
        let mut state = queue.lock();
        let busy = state
            .busy
            .as_mut()
            .expect("member keeps the busy period open");
        let segment = busy
            .segment
            .as_mut()
            .expect("a committed record has a segment");
        let abort_lsn = segment.next_lsn();
        let manifest =
            serde_json::to_vec(&AbortManifest::new(lsn)).map_err(|error| json_error(&error))?;
        let record = LogRecord {
            lsn: abort_lsn,
            kind: LogRecordKind::Abort,
            generation: busy.generation + 2,
            manifest,
            blobs: Vec::new(),
        };
        let encoded = segment.encode(&record)?;
        segment.append(&encoded, 1)?;
        busy.appended_lsn = abort_lsn;
        self.flush_until(queue, state, abort_lsn)
    }

    fn finish_member<P: LoggedPlan>(
        collection: &NativeCollection,
        queue: &Arc<CommitQueue>,
        plan: &P,
        delta: Option<Result<SchemaCountDelta, NativeStorageError>>,
    ) -> Result<(), NativeStorageError> {
        let mut state = queue.lock();
        let busy = state
            .busy
            .as_mut()
            .expect("member keeps the busy period open");
        match delta {
            Some(Ok(delta)) => busy.schema_delta.merge(&delta),
            Some(Err(_)) => busy.schema_counts = None,
            None => {}
        }
        for path in plan.touched() {
            busy.pending.remove(path);
        }
        busy.members -= 1;
        let outcome = if busy.members == 0 {
            let busy = state.busy.take().expect("checked above");
            Self::end_busy(collection, busy)
        } else {
            Ok(())
        };
        queue.notify();
        outcome
    }

    /// Close a busy period nobody has appended to yet.
    fn end_idle(
        queue: &Arc<CommitQueue>,
        state: &mut MutexGuard<'_, QueueState>,
        collection: &NativeCollection,
    ) {
        if state.busy.as_ref().is_some_and(|busy| busy.members == 0) {
            if let Some(busy) = state.busy.take() {
                let _ = Self::publish_end(collection, &busy);
            }
            queue.notify();
        }
    }

    /// The busy period is consumed first, so its segment handle is closed
    /// before a checkpoint removes the log: Windows cannot remove a directory
    /// that holds an open file.
    fn end_busy(
        collection: &NativeCollection,
        mut busy: BusyPeriod,
    ) -> Result<(), NativeStorageError> {
        let published = Self::publish_end(collection, &busy)?;
        let generation = busy.generation + 2;
        if collection.uses_segments() {
            return Self::end_segment_busy(collection, &mut busy, published.is_some());
        }
        // Still under the collection lock, which `busy` holds.
        if busy.published_writing
            && !busy.stranded
            && let Some(counts) = busy.schema_counts.take()
        {
            Self::schema_counts_after_commit(collection, counts, &busy.schema_delta, generation);
        }
        drop(busy);
        let Some(position) = published else {
            return Ok(());
        };
        if Self::wants_checkpoint(collection, &position)? {
            LogCheckpoint::run(collection, generation)?;
        }
        Ok(())
    }

    /// A segment collection checkpoints while the busy period still holds
    /// the collection lock, because the checkpoint appends to the active
    /// segment and may rewrite the location snapshot.
    fn end_segment_busy(
        collection: &NativeCollection,
        busy: &mut BusyPeriod,
        published: bool,
    ) -> Result<(), NativeStorageError> {
        let Some(segment) = busy.segment.take() else {
            return Ok(());
        };
        if !published || !SegmentCheckpoint::wanted(collection, &segment)? {
            return Ok(());
        }
        SegmentCheckpoint::run(collection, segment, busy.generation + 2)?;
        Ok(())
    }

    /// Publish the stable generation that closes a busy period, unless a
    /// failed rollback left it for recovery.
    fn publish_end(
        collection: &NativeCollection,
        busy: &BusyPeriod,
    ) -> Result<Option<LogPosition>, NativeStorageError> {
        if !busy.published_writing || busy.stranded {
            return Ok(None);
        }
        let generation = busy.generation + 2;
        let Some(segment) = busy.segment.as_ref() else {
            Self::publish_quietly(
                collection,
                &GenerationRecord::stable(generation).fenced(busy.fence.is_present()),
            )?;
            return Ok(None);
        };
        let position = segment.position();
        Self::publish_quietly(
            collection,
            &GenerationRecord::log_stable(generation, position.clone())
                .fenced(busy.fence.is_present()),
        )?;
        Ok(Some(position))
    }

    fn wants_checkpoint(
        collection: &NativeCollection,
        position: &LogPosition,
    ) -> Result<bool, NativeStorageError> {
        Ok(position.records >= Self::CHECKPOINT_RECORDS
            || position.length >= Self::CHECKPOINT_BYTES
            || LogCheckpoint::index_wants_compaction(collection)?)
    }

    pub(crate) fn index_lines(removed: &BTreeSet<String>, added: &BTreeSet<String>) -> Vec<u8> {
        let mut lines = Vec::new();
        for (sign, key) in removed
            .iter()
            .map(|key| (b'-', key))
            .chain(added.iter().map(|key| (b'+', key)))
        {
            lines.push(sign);
            lines.push(b'\t');
            lines.extend_from_slice(key.as_bytes());
            lines.push(b'\n');
        }
        lines
    }

    pub(crate) fn append_index(
        collection: &NativeCollection,
        lines: &[u8],
    ) -> Result<(), NativeStorageError> {
        if lines.is_empty() {
            return Ok(());
        }
        let index = collection.path.join("index");
        if !path_exists_no_follow(&index)? {
            fs::create_dir(&index).map_err(NativeStorageError::io)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(index.join("keys.wal"))
            .map_err(NativeStorageError::io)?;
        file.write_all(lines).map_err(NativeStorageError::io)
    }

    /// Replace `state.json` without flushing it: the log, not this file,
    /// carries durability, and recovery rewrites it from the log.
    pub(crate) fn publish_quietly(
        collection: &NativeCollection,
        record: &GenerationRecord,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return SegmentCheckpoint::publish_current(collection, record);
        }
        let path = LogCheckpoint::state_path(collection)?;
        let mut bytes = serde_json::to_vec(record).map_err(|error| json_error(&error))?;
        bytes.push(b'\n');
        let scratch = sibling_scratch(&path);
        let outcome = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&scratch)
                .map_err(NativeStorageError::io)?;
            file.write_all(&bytes).map_err(NativeStorageError::io)?;
            fs::rename(&scratch, &path).map_err(NativeStorageError::io)
        })();
        if outcome.is_err() && path_exists_no_follow(&scratch)? {
            fs::remove_file(&scratch).map_err(NativeStorageError::io)?;
        }
        outcome
    }

    /// Recover the collection when its state or its log shows a write that
    /// a crash interrupted. Returns whether anything was recovered.
    pub(crate) fn recover_log_if_needed(
        &self,
        collection: &NativeCollection,
    ) -> Result<bool, NativeStorageError> {
        let stored = collection.stored_generation()?;
        if !Self::log_needs_recovery(collection, &stored)? {
            return Ok(false);
        }
        self.run_log_recovery(collection, &stored)?;
        Ok(true)
    }

    fn log_needs_recovery(
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<bool, NativeStorageError> {
        if stored.state == GenerationStatus::Writing {
            return Ok(stored
                .transaction_id
                .as_deref()
                .is_some_and(|identifier| identifier.starts_with(Self::LOG_TRANSACTION_PREFIX)));
        }
        match &stored.log {
            Some(position) => Ok(LogSegment::open_matching(
                &collection.logged_segment_path(position),
                position,
            )?
            .is_none()),
            None => Self::leftover_log_needs_recovery(collection, stored.generation),
        }
    }

    /// A v1 state beside a log directory is one of two things. A checkpoint
    /// that published v1 and then died before removing the log left a segment
    /// opened at an older generation than v1 records; it is finished and only
    /// the directory is removed. Otherwise a commit's publication was lost and
    /// the log holds work to replay.
    fn leftover_log_needs_recovery(
        collection: &NativeCollection,
        generation: u64,
    ) -> Result<bool, NativeStorageError> {
        let directory = collection.log_directory();
        if !path_exists_no_follow(&directory)? {
            return Ok(false);
        }
        let opened = LogSegment::scan(&collection.log_segment_path())?
            .and_then(|scan| scan.records.first().map(|(_, record)| record.generation));
        if opened.is_some_and(|opened| opened >= generation) {
            return Ok(true);
        }
        super::super::remove_dir_durable(&directory)?;
        Ok(false)
    }
}

impl GenerationRecord {
    pub(crate) fn log_writing(generation: u64, position: LogPosition) -> Self {
        Self {
            format: GenerationState::FORMAT_V2.into(),
            generation,
            state: "writing".into(),
            transaction_id: Some(format!(
                "{}{}",
                NativeWriteRoot::LOG_TRANSACTION_PREFIX,
                position.nonce
            )),
            log: Some(position),
        }
    }

    pub(crate) fn log_stable(generation: u64, position: LogPosition) -> Self {
        Self {
            format: GenerationState::FORMAT_V2.into(),
            generation,
            state: "stable".into(),
            transaction_id: None,
            log: Some(position),
        }
    }
}
