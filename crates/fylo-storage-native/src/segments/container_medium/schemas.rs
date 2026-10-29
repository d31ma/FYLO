//! A container collection's schemas snapshot (ADR 0017 section 3,
//! `docs/fxdb-segment-backend.md`, "Schema versions").
//!
//! The snapshot is a `schemas` run (extent kind 7) the checkpoint slot
//! names beside the location and index runs. The schema records are the
//! truth and the run is derived: a slot that names none means the
//! collection has no snapshot, as a slot written before schemas existed
//! does, and a run that cannot be read is rebuilt from the records.
//!
//! The empty registry is kept as no run at all, so a collection that never
//! held a schema version takes no extent for it. Replacing the run records
//! a new checkpoint slot that keeps the newest slot's position and its
//! location and index runs. Its bytes do not depend on the checkpoint that
//! names them in a plain file, so every later checkpoint names the same
//! run; an encrypted file seals each run for its checkpoint, so there a new
//! checkpoint writes a copy of every run it keeps.

use fylo_format::{ContainerCheckpointSlot, ContainerExtentKind, ContainerSnapshotRef};

use super::{ContainerMedium, INDEX, LOCATIONS};
use crate::segments::SnapshotStamp;
use crate::write::SegmentSchemas;
use crate::{ContainerError, ContainerFile, NativeStorageError};

pub(super) const SCHEMAS: ContainerExtentKind = ContainerExtentKind::Schemas;

impl ContainerMedium {
    /// The schemas snapshot the newest checkpoint names, or `None` when it
    /// names none. A run that cannot be read gives bytes that decode as no
    /// snapshot, so the registry is rebuilt from the records.
    pub(super) fn schemas_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        let file = self.shared.file();
        let Some(checkpoint) = file.read_checkpoint(block).map_err(Self::error)? else {
            return Ok(None);
        };
        let Some(run) = checkpoint.schemas else {
            return Ok(None);
        };
        match file.read_snapshot(id, SCHEMAS, checkpoint.checkpoint, &run, u64::MAX) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error @ (ContainerError::Io(_) | ContainerError::Locked)) => {
                Err(Self::error(error))
            }
            Err(_) => Ok(Some(Vec::new())),
        }
    }

    /// The length and SHA-256 the newest checkpoint records for its
    /// schemas run, which change whenever the run's bytes do.
    pub(super) fn schemas_stamp(&self) -> Result<Option<SnapshotStamp>, NativeStorageError> {
        Ok(self
            .newest_checkpoint()?
            .and_then(|checkpoint| checkpoint.schemas)
            .map(|run| SnapshotStamp::Run(run.length, run.sha256)))
    }

    /// Make `bytes` the schemas snapshot, `None` or the empty registry
    /// meaning none: write a new `schemas` run, checkpoint the map so it
    /// shows the run, record a checkpoint slot that names it and keeps the
    /// newest slot's position and its other runs, then release the run it
    /// replaces and checkpoint the map again. A stop between those steps
    /// leaves a run no slot names, which the next claim of the collection
    /// releases.
    ///
    /// A collection that never checkpointed gets a first slot at the start
    /// of its records that names no location or index run, which reads as
    /// the empty table and index, as no slot does.
    pub(super) fn replace_schemas(&self, bytes: Option<&[u8]>) -> Result<(), NativeStorageError> {
        let empty = SegmentSchemas::empty().encode()?;
        let bytes = bytes.filter(|bytes| *bytes != empty.as_slice());
        // A checkpoint of a collection without schema versions asks for the
        // empty registry each time; it finds nothing to change before taking
        // the allocation byte.
        if bytes.is_none() && self.schemas_stamp()?.is_none() {
            return Ok(());
        }
        let (id, block) = (self.shared.collection, self.shared.block);
        self.shared.writable()?.change_map(|file| {
            let previous = file.read_checkpoint(block).map_err(Self::error)?;
            let current = previous.and_then(|previous| previous.schemas);
            if bytes.is_none() && current.is_none() {
                return Ok(());
            }
            let checkpoint = previous.map_or(1, |previous| previous.checkpoint + 1);
            let schemas = match bytes {
                Some(bytes) => Some(
                    file.write_snapshot(id, SCHEMAS, checkpoint, bytes)
                        .map_err(Self::error)?,
                ),
                None => None,
            };
            let slot = match previous {
                Some(previous) => ContainerCheckpointSlot {
                    checkpoint,
                    locations: previous
                        .locations
                        .map(|run| {
                            Self::carried(
                                file,
                                (id, LOCATIONS),
                                (previous.checkpoint, run),
                                checkpoint,
                            )
                        })
                        .transpose()
                        .map_err(Self::error)?,
                    index: Self::carried_if_readable(
                        file,
                        (id, INDEX),
                        (previous.checkpoint, previous.index),
                        checkpoint,
                    )
                    .map_err(Self::error)?,
                    schemas,
                    ..previous
                },
                None => ContainerCheckpointSlot {
                    checkpoint,
                    lsn: 0,
                    extent: 0,
                    extent_sequence: 0,
                    offset: 0,
                    locations: None,
                    index: None,
                    schemas,
                    dictionary: None,
                },
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
            Ok(())
        })
    }

    /// The schemas run `previous` names, for checkpoint `checkpoint` to
    /// name too ([`Self::carried_if_readable`]).
    pub(super) fn carried_schemas(
        file: &mut ContainerFile,
        id: u32,
        previous: Option<&ContainerCheckpointSlot>,
        checkpoint: u64,
    ) -> Result<Option<ContainerSnapshotRef>, ContainerError> {
        let Some(previous) = previous else {
            return Ok(None);
        };
        Self::carried_if_readable(
            file,
            (id, SCHEMAS),
            (previous.checkpoint, previous.schemas),
            checkpoint,
        )
    }

    /// Run `run`, which checkpoint `number` names, as checkpoint
    /// `checkpoint` names it: the same run in a plain file, and a copy
    /// sealed for the new checkpoint in an encrypted one. The caller
    /// releases the run it replaces once the new slot is recorded.
    pub(super) fn carried(
        file: &mut ContainerFile,
        (id, kind): (u32, ContainerExtentKind),
        (number, run): (u64, ContainerSnapshotRef),
        checkpoint: u64,
    ) -> Result<ContainerSnapshotRef, ContainerError> {
        if file.cipher().is_none() {
            return Ok(run);
        }
        let bytes = file.read_snapshot(id, kind, number, &run, u64::MAX)?;
        file.write_snapshot(id, kind, checkpoint, &bytes)
    }

    /// [`Self::carried`] for a derived run that may be dropped: one that
    /// cannot be read is carried as none, to be rebuilt.
    pub(super) fn carried_if_readable(
        file: &mut ContainerFile,
        (id, kind): (u32, ContainerExtentKind),
        (number, run): (u64, Option<ContainerSnapshotRef>),
        checkpoint: u64,
    ) -> Result<Option<ContainerSnapshotRef>, ContainerError> {
        let Some(run) = run else {
            return Ok(None);
        };
        match Self::carried(file, (id, kind), (number, run), checkpoint) {
            Ok(run) => Ok(Some(run)),
            Err(error @ ContainerError::Io(_)) => Err(error),
            Err(_) => Ok(None),
        }
    }
}
