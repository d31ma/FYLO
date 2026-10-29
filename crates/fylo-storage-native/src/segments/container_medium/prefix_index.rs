//! A container collection's prefix index (`docs/fxdb-segment-backend.md`,
//! "Root integration").
//!
//! The index snapshot is an `index` run the checkpoint slot names beside
//! the location run, and both reach the slot's position. The keys past it
//! are not written twice: they are the index deltas of the commit records
//! after that position, replayed as location rows are. Every location fold
//! folds the index with it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use fylo_format::{ContainerCheckpointSlot, ContainerExtentKind, ContainerSnapshotRef};

use super::schemas::SCHEMAS;
use super::{ContainerMedium, INDEX, LOCATIONS};
use crate::index_cache::IndexInputs;
use crate::segments::{IndexDelta, MediumIdentity, RecordPosition, SegmentMedium};
use crate::{ContainerError, ContainerFile, NativeStorageError, NativeStorageErrorCode};

/// A reader's index of one collection: the snapshot of one checkpoint, and
/// the lines replayed past it up to a published position.
struct CachedIndex {
    checkpoint: u64,
    snapshot: Arc<Vec<u8>>,
    reach: RecordPosition,
    lines: Arc<Vec<u8>>,
}

impl ContainerMedium {
    /// The index snapshot and the lines of the records after it, up to
    /// `to`. The replayed lines are kept per collection and checkpoint and
    /// extended, so a reader replays each record once; `to` is a published
    /// position, which never separates a commit from the abort that
    /// cancels it.
    pub(super) fn index_at(
        &self,
        to: RecordPosition,
    ) -> Result<(Vec<u8>, Vec<u8>), NativeStorageError> {
        let identity = self.identity();
        let checkpoint = self.newest_checkpoint()?;
        let number = checkpoint.as_ref().map_or(0, |slot| slot.checkpoint);
        let from = Self::position_of(checkpoint.as_ref());
        let cached = Self::cache()
            .get(&identity)
            .filter(|cached| cached.checkpoint == number)
            .map(|cached| {
                (
                    Arc::clone(&cached.snapshot),
                    cached.reach,
                    Arc::clone(&cached.lines),
                )
            });
        let (snapshot, reach, mut lines) = if let Some(cached) = cached {
            cached
        } else {
            let snapshot = self
                .snapshot_of(checkpoint.as_ref())
                .ok_or_else(Self::needs_rebuild)?;
            (Arc::new(snapshot), from, Arc::new(Vec::new()))
        };
        if reach < to {
            let mut extended = Vec::clone(&lines);
            extended.extend(IndexDelta::replay(self, (reach, to))?);
            lines = Arc::new(extended);
        } else if to < reach {
            let replayed = IndexDelta::replay(self, (from, to))?;
            return Ok((Vec::clone(&snapshot), replayed));
        }
        let answer = (Vec::clone(&snapshot), Vec::clone(&lines));
        Self::cache().insert(
            identity,
            CachedIndex {
                checkpoint: number,
                snapshot,
                reach: to,
                lines,
            },
        );
        Ok(answer)
    }

    /// The index snapshot the newest checkpoint names: empty before the
    /// first checkpoint, and `None` when it names none or cannot be read.
    pub(super) fn index_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        Ok(self.snapshot_of(self.newest_checkpoint()?.as_ref()))
    }

    /// The index for a checkpoint at `to`: the snapshot `previous` names
    /// with the lines of the records since laid over. `None` when that
    /// snapshot cannot be read; the checkpoint then names no index, and the
    /// collection's index must be rebuilt.
    pub(super) fn folded_index(
        &self,
        previous: Option<&ContainerCheckpointSlot>,
        to: RecordPosition,
    ) -> Result<Option<Vec<u8>>, NativeStorageError> {
        let Some(snapshot) = self.snapshot_of(previous) else {
            return Ok(None);
        };
        let from = Self::position_of(previous);
        let lines = IndexDelta::replay(self, (from, to))?;
        Ok(IndexInputs {
            snapshot,
            log: Some(lines),
        }
        .merge()
        .ok()
        .map(|merged| merged.as_bytes().to_vec()))
    }

    /// Record `bytes` as the index snapshot in a new checkpoint at the
    /// newest checkpoint's position, rewriting its location snapshot under
    /// the new checkpoint number, which an encrypted file seals it for, and
    /// keeping its schemas snapshot.
    pub(super) fn replace_index(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        self.shared.writable()?.change_map(|file| {
            let previous = file
                .read_checkpoint(block)
                .map_err(Self::error)?
                .ok_or_else(|| {
                    NativeStorageError::new(
                        NativeStorageErrorCode::CorruptMetadata,
                        "a container collection replaces its index before any checkpoint",
                    )
                })?;
            let checkpoint = previous.checkpoint + 1;
            let locations = match previous.locations {
                Some(run) => {
                    let rows = file
                        .read_snapshot(id, LOCATIONS, previous.checkpoint, &run, u64::MAX)
                        .map_err(Self::error)?;
                    Some(
                        file.write_snapshot(id, LOCATIONS, checkpoint, &rows)
                            .map_err(Self::error)?,
                    )
                }
                None => None,
            };
            let index = file
                .write_snapshot(id, INDEX, checkpoint, bytes)
                .map_err(Self::error)?;
            let schemas = Self::carried_schemas(file, id, Some(&previous), checkpoint)
                .map_err(Self::error)?;
            file.checkpoint()?;
            file.record_checkpoint(
                block,
                ContainerCheckpointSlot {
                    checkpoint,
                    locations,
                    index: Some(index),
                    schemas,
                    ..previous
                },
            )
            .map_err(Self::error)?;
            if let Some(old) = previous.locations {
                file.release_snapshot(&old).map_err(Self::error)?;
            }
            Self::release_runs(file, (id, INDEX), Some(&index)).map_err(Self::error)?;
            Self::release_runs(file, (id, SCHEMAS), schemas.as_ref()).map_err(Self::error)
        })?;
        Self::cache().remove(&self.identity());
        Ok(())
    }

    /// Release every run of `kind` of collection `id` but `kept`: the one
    /// an earlier checkpoint named, and any a stopped writer left unnamed.
    /// It runs while this process holds its claim on the collection.
    pub(super) fn release_runs(
        file: &mut ContainerFile,
        (id, kind): (u32, ContainerExtentKind),
        kept: Option<&ContainerSnapshotRef>,
    ) -> Result<(), ContainerError> {
        let kept = kept.map_or(0..0, |run| {
            run.first_extent..run.first_extent + u64::from(run.extent_count)
        });
        for (extent, _) in file.allocator().owned(kind, id) {
            if !kept.contains(&extent) {
                file.release(extent)?;
            }
        }
        Ok(())
    }

    /// Forget what this process replayed of the index of the collection
    /// `identity` names.
    pub(crate) fn forget_index(identity: &MediumIdentity) {
        Self::cache().remove(identity);
    }

    pub(super) fn newest_checkpoint(
        &self,
    ) -> Result<Option<ContainerCheckpointSlot>, NativeStorageError> {
        self.shared
            .file()
            .read_checkpoint(self.shared.block)
            .map_err(Self::error)
    }

    /// The index snapshot `checkpoint` names, empty for no checkpoint and
    /// for one that names no location run either: a slot that has folded
    /// nothing yet, as the first schemas snapshot records it.
    fn snapshot_of(&self, checkpoint: Option<&ContainerCheckpointSlot>) -> Option<Vec<u8>> {
        let Some(checkpoint) = checkpoint else {
            return Some(Vec::new());
        };
        let Some(run) = checkpoint.index else {
            return checkpoint.locations.is_none().then(Vec::new);
        };
        self.shared
            .file()
            .read_snapshot(
                self.shared.collection,
                INDEX,
                checkpoint.checkpoint,
                &run,
                u64::MAX,
            )
            .ok()
    }

    fn position_of(checkpoint: Option<&ContainerCheckpointSlot>) -> RecordPosition {
        checkpoint.map_or(RecordPosition::START, |checkpoint| RecordPosition {
            segment: checkpoint.extent_sequence,
            offset: checkpoint.offset,
        })
    }

    fn needs_rebuild() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptIndex,
            "the collection's prefix index snapshot is missing or damaged; run fylo rebuild",
        )
    }

    fn cache() -> MutexGuard<'static, BTreeMap<MediumIdentity, CachedIndex>> {
        static CACHE: OnceLock<Mutex<BTreeMap<MediumIdentity, CachedIndex>>> = OnceLock::new();
        CACHE
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
