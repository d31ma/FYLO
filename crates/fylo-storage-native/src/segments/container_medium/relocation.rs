//! Moving what a collection keeps beside its segments nearer the front of a
//! `.fxdb` file, for `fylo compact --shrink` (ADR 0016 section 6,
//! "Shrinking"; `docs/fxdb-segment-backend.md`, "Compaction and shrinking").
//!
//! Extents are taken lowest first, so a copy lands in the first free extent
//! the map shows. A segment moves by compaction, which copies its live
//! records; the derived runs and a dictionary move here, by a copy the
//! checkpoint slot comes to name before the original is released, in one
//! change to the map. A stop before the slot is recorded leaves a copy no
//! slot names, which the next claim of the collection releases (a run) or
//! the next compaction releases (a dictionary).

use fylo_format::{ContainerCheckpointSlot, ContainerExtentKind, ContainerSnapshotRef};

use super::{ContainerMedium, INDEX, LOCATIONS, SCHEMAS};
use crate::segments::{SegmentMedium, TableCache};
use crate::{ContainerError, ContainerFile, NativeStorageError};

impl ContainerMedium {
    /// Copy the run of `kind` the newest checkpoint slot names into
    /// extents taken afresh, record a checkpoint slot that names the copy
    /// and carries the rest of the newest one, and release the run it
    /// replaces. Returns whether the collection had a checkpoint to move.
    /// An index or schemas run that cannot be read is named as none, to be
    /// rebuilt, as a checkpoint carries one.
    pub(crate) fn relocate_run(
        &self,
        kind: ContainerExtentKind,
    ) -> Result<bool, NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        let moved = self.shared.writable()?.change_map(|file| {
            let Some(previous) = file.read_checkpoint(block).map_err(Self::error)? else {
                return Ok(false);
            };
            let checkpoint = previous.checkpoint + 1;
            let from = previous.checkpoint;
            let locations = previous
                .locations
                .map(|run| {
                    if kind == LOCATIONS {
                        Self::copied(file, (id, LOCATIONS), (from, run), checkpoint)
                    } else {
                        Self::carried(file, (id, LOCATIONS), (from, run), checkpoint)
                    }
                })
                .transpose()
                .map_err(Self::error)?;
            let index = if kind == INDEX {
                Self::copied_if_readable(file, (id, INDEX), (from, previous.index), checkpoint)
            } else {
                Self::carried_if_readable(file, (id, INDEX), (from, previous.index), checkpoint)
            }
            .map_err(Self::error)?;
            let schemas = if kind == SCHEMAS {
                Self::copied_if_readable(file, (id, SCHEMAS), (from, previous.schemas), checkpoint)
            } else {
                Self::carried_schemas(file, id, Some(&previous), checkpoint)
            }
            .map_err(Self::error)?;
            let slot = ContainerCheckpointSlot {
                checkpoint,
                locations,
                index,
                schemas,
                ..previous
            };
            file.checkpoint()?;
            file.record_checkpoint(block, slot).map_err(Self::error)?;
            for (kind, kept) in [
                (LOCATIONS, slot.locations),
                (INDEX, slot.index),
                (SCHEMAS, slot.schemas),
            ] {
                Self::release_runs(file, (id, kind), kept.as_ref()).map_err(Self::error)?;
            }
            Ok(true)
        })?;
        TableCache::shared().forget(self);
        Self::forget_index(&self.identity());
        Ok(moved)
    }

    /// Copy the dictionary in `extent` into an extent taken afresh, under
    /// its own id, point the checkpoint slot at the copy when it named the
    /// original, and release the original. Records name a dictionary by
    /// id, so every record reads on with the copy.
    pub(crate) fn relocate_dictionary(&self, extent: u64) -> Result<(), NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        self.shared.writable()?.change_map(|file| {
            let dictionary = file.read_dictionary(id, extent).map_err(Self::error)?;
            let named = file
                .write_dictionary(id, &dictionary)
                .map_err(Self::error)?;
            let previous = file.read_checkpoint(block).map_err(Self::error)?;
            if let Some(previous) =
                previous.filter(|slot| slot.dictionary.is_some_and(|(at, _, _)| at == extent))
            {
                let checkpoint = previous.checkpoint + 1;
                let from = previous.checkpoint;
                let slot = ContainerCheckpointSlot {
                    checkpoint,
                    locations: previous
                        .locations
                        .map(|run| Self::carried(file, (id, LOCATIONS), (from, run), checkpoint))
                        .transpose()
                        .map_err(Self::error)?,
                    index: Self::carried_if_readable(
                        file,
                        (id, INDEX),
                        (from, previous.index),
                        checkpoint,
                    )
                    .map_err(Self::error)?,
                    schemas: Self::carried_schemas(file, id, Some(&previous), checkpoint)
                        .map_err(Self::error)?,
                    dictionary: Some(named),
                    ..previous
                };
                file.checkpoint()?;
                file.record_checkpoint(block, slot).map_err(Self::error)?;
                for (kind, kept) in [
                    (LOCATIONS, slot.locations),
                    (INDEX, slot.index),
                    (SCHEMAS, slot.schemas),
                ] {
                    Self::release_runs(file, (id, kind), kept.as_ref()).map_err(Self::error)?;
                }
            }
            file.release_dictionary(id, extent).map_err(Self::error)
        })?;
        Self::forget_index(&self.identity());
        Ok(())
    }

    /// Run `run`, which checkpoint `number` names, copied into a run taken
    /// afresh for checkpoint `checkpoint`.
    fn copied(
        file: &mut ContainerFile,
        (id, kind): (u32, ContainerExtentKind),
        (number, run): (u64, ContainerSnapshotRef),
        checkpoint: u64,
    ) -> Result<ContainerSnapshotRef, ContainerError> {
        let bytes = file.read_snapshot(id, kind, number, &run, u64::MAX)?;
        file.write_snapshot(id, kind, checkpoint, &bytes)
    }

    /// [`Self::copied`] for a derived run that may be dropped: one that
    /// cannot be read is named as none, to be rebuilt.
    fn copied_if_readable(
        file: &mut ContainerFile,
        (id, kind): (u32, ContainerExtentKind),
        (number, run): (u64, Option<ContainerSnapshotRef>),
        checkpoint: u64,
    ) -> Result<Option<ContainerSnapshotRef>, ContainerError> {
        let Some(run) = run else {
            return Ok(None);
        };
        match Self::copied(file, (id, kind), (number, run), checkpoint) {
            Ok(run) => Ok(Some(run)),
            Err(error @ ContainerError::Io(_)) => Err(error),
            Err(_) => Ok(None),
        }
    }
}
