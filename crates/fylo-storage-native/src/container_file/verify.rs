//! `fylo verify` for a `.fxdb` container (ADR 0016 sections 3, 6, 9 and 11).
//!
//! Verification reads the file and changes nothing. It needs no key: the
//! superblocks, extent headers, map and slots are in the clear, a sealed
//! collection block carries a CRC over its ciphertext, and a snapshot's
//! SHA-256 is over the bytes it stores. So it checks a locked encrypted file
//! as fully as a plain one, except that a locked file cannot say which
//! collection ids exist.
//!
//! The extent map in force is the allocator's view: the checkpointed map,
//! plus what this handle allocated, sealed and released since. An extent is
//! reused only after a checkpoint has written a map that shows it free, so
//! every extent the map shows in use still carries the header written when
//! it was taken. An extent the map shows free may carry a valid header too:
//! one taken after the last checkpoint, which recovery finds by its seal
//! record, or one released with its old header left in place. Those are
//! counted, not reported.

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentHeader, ContainerExtentKind,
    ContainerFormatError, ContainerGenerationSlot, ContainerSnapshotRef, ContainerSuperblock,
};
use sha2::{Digest, Sha256};

use super::{BLOCK_BYTES, ContainerError, ContainerFile, SUPERBLOCK_BYTES, read_exact_at};

/// The largest read verification makes at once while hashing a snapshot.
const HASH_CHUNK_BYTES: u64 = 1 << 20;

/// What `fylo verify` found in a container.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerVerifyReport {
    /// Whether no problem is an error. Warnings leave a file `ok`.
    pub ok: bool,
    /// Extents the map shows in use whose headers were checked.
    pub checked_extents: u64,
    /// Extents the map shows free, or that lie past its end, that still hold
    /// a valid header: taken after the last checkpoint, or released with
    /// their old header in place. Not a problem.
    pub unmapped_headers: u64,
    /// What is wrong, errors and warnings, in the order found.
    pub problems: Vec<ContainerProblem>,
}

/// How serious a [`ContainerProblem`] is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContainerProblemSeverity {
    /// The file is damaged.
    Error,
    /// The file is sound, but an interrupted write or a derived structure
    /// left something worth knowing.
    Warning,
}

/// One thing `fylo verify` found.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerProblem {
    /// Error or warning.
    pub severity: ContainerProblemSeverity,
    /// A stable camelCase code: `superblockDamaged`,
    /// `superblockCopyDamaged`, `fileTooShort`, `extentHeaderMismatch`,
    /// `unknownOwner`, `unexpectedOwner`, `directoryBlockDamaged`,
    /// `tornSlot`, `slotDamaged`, `snapshotOutOfRange`,
    /// `snapshotExtentMismatch`, `snapshotNotInMap` or
    /// `snapshotHashMismatch`.
    pub code: &'static str,
    /// The extent concerned, if one is.
    pub extent: Option<u64>,
    /// What is wrong, for a person.
    pub message: String,
}

/// The problems found so far.
#[derive(Default)]
struct Findings {
    checked_extents: u64,
    unmapped_headers: u64,
    problems: Vec<ContainerProblem>,
}

impl Findings {
    fn error(&mut self, code: &'static str, extent: Option<u64>, message: String) {
        self.push(ContainerProblemSeverity::Error, code, extent, message);
    }

    fn warning(&mut self, code: &'static str, extent: Option<u64>, message: String) {
        self.push(ContainerProblemSeverity::Warning, code, extent, message);
    }

    fn push(
        &mut self,
        severity: ContainerProblemSeverity,
        code: &'static str,
        extent: Option<u64>,
        message: String,
    ) {
        self.problems.push(ContainerProblem {
            severity,
            code,
            extent,
            message,
        });
    }

    fn report(self) -> ContainerVerifyReport {
        ContainerVerifyReport {
            ok: !self
                .problems
                .iter()
                .any(|problem| problem.severity == ContainerProblemSeverity::Error),
            checked_extents: self.checked_extents,
            unmapped_headers: self.unmapped_headers,
            problems: self.problems,
        }
    }
}

impl ContainerFile {
    /// Check the container's structures against each other without changing
    /// anything and without the key, for `fylo verify`: both superblock
    /// copies, the file's length against the map, the header of every
    /// extent the map shows in use, every collection block and slot pair in
    /// the directory, and each collection's newest location and index
    /// snapshots against their SHA-256. Works on readers and on a locked
    /// encrypted file; an unlocked or plain file also has every extent
    /// owner checked against the collections that exist.
    ///
    /// Damage is reported in the result, not as an error.
    ///
    /// # Errors
    ///
    /// Returns the failure to read the file's length or a read failure
    /// other than the file ending early.
    pub fn verify(&self) -> Result<ContainerVerifyReport, ContainerError> {
        let mut findings = Findings::default();
        let file_bytes = self.file().metadata()?.len();
        if file_bytes < 2 * SUPERBLOCK_BYTES {
            findings.error(
                "fileTooShort",
                None,
                format!("the file is {file_bytes} bytes, too short to hold both superblocks"),
            );
            return Ok(findings.report());
        }
        self.verify_superblocks(&mut findings)?;
        self.verify_length(file_bytes, &mut findings);
        self.verify_extents(file_bytes, &mut findings)?;
        self.verify_directory(file_bytes, &mut findings)?;
        Ok(findings.report())
    }

    fn verify_superblocks(&self, findings: &mut Findings) -> Result<(), ContainerError> {
        let mut copies = vec![0_u8; 2 * ContainerSuperblock::ENCODED_BYTES];
        read_exact_at(self.file(), &mut copies, 0)?;
        let (copy_a, copy_b) = copies.split_at(ContainerSuperblock::ENCODED_BYTES);
        let (in_force, other, names) = if self.superblock_in_a {
            (copy_a, copy_b, ("A", "B"))
        } else {
            (copy_b, copy_a, ("B", "A"))
        };
        if let Err(error) = ContainerSuperblock::decode(in_force) {
            findings.error(
                "superblockDamaged",
                None,
                format!("superblock copy {} in force is damaged: {error}", names.0),
            );
        }
        // A copy no checkpoint has written yet is all zero.
        if !all_zero(other)
            && let Err(error) = ContainerSuperblock::decode(other)
        {
            findings.warning(
                "superblockCopyDamaged",
                None,
                format!(
                    "superblock copy {} is damaged ({error}); a checkpoint was interrupted, \
                     and copy {} is in force",
                    names.1, names.0
                ),
            );
        }
        Ok(())
    }

    fn verify_length(&self, file_bytes: u64, findings: &mut Findings) {
        let extent_bytes = self.extent_bytes();
        let required = self.allocator.extent_count() * extent_bytes;
        if file_bytes >= required {
            return;
        }
        // Extents that end past the end of the file.
        let first_missing = (file_bytes / extent_bytes).max(1);
        let only_free = (first_missing..self.allocator.extent_count())
            .all(|extent| self.allocator.is_free(extent));
        let message = format!(
            "the file is {file_bytes} bytes but the map describes {required}; extents from \
             {first_missing} on are cut short"
        );
        if only_free {
            findings.warning("fileTooShort", Some(first_missing), message);
        } else {
            findings.error("fileTooShort", Some(first_missing), message);
        }
    }

    fn verify_extents(
        &self,
        file_bytes: u64,
        findings: &mut Findings,
    ) -> Result<(), ContainerError> {
        let extent_bytes = self.extent_bytes();
        let collection_ids: Option<Vec<u32>> = (!self.is_locked()).then(|| {
            self.collections
                .iter()
                .map(|collection| collection.header.id)
                .collect()
        });
        // Whole extents the file holds past the map's end were taken after
        // the last checkpoint grew it.
        let last = self.allocator.extent_count().max(file_bytes / extent_bytes);
        for extent in 1..last {
            let Some(header) = self.read_extent_header(extent, file_bytes)? else {
                continue;
            };
            let Some((kind, owner, sequence)) = self.allocator.in_use(extent) else {
                if header.is_ok_and(|header| header.index == extent)
                    && (extent >= self.allocator.extent_count() || self.allocator.is_free(extent))
                {
                    findings.unmapped_headers += 1;
                }
                continue;
            };
            findings.checked_extents += 1;
            match header {
                Err(error) => findings.error(
                    "extentHeaderMismatch",
                    Some(extent),
                    format!(
                        "the map shows extent {extent} in use as {} but its header does not \
                         decode: {error}",
                        kind.name()
                    ),
                ),
                Ok(header)
                    if (header.index, header.kind, header.owner, header.sequence)
                        != (extent, kind, owner, sequence) =>
                {
                    findings.error(
                        "extentHeaderMismatch",
                        Some(extent),
                        format!(
                            "extent {extent}'s header names {} of owner {} with sequence {} at \
                             index {}; the map expects {} of owner {owner} with sequence \
                             {sequence}",
                            header.kind.name(),
                            header.owner,
                            header.sequence,
                            header.index,
                            kind.name()
                        ),
                    );
                }
                Ok(_) => {}
            }
            verify_owner(extent, kind, owner, collection_ids.as_deref(), findings);
        }
        Ok(())
    }

    /// An extent's header as decoded, or `None` when the file ends before
    /// it.
    fn read_extent_header(
        &self,
        extent: u64,
        file_bytes: u64,
    ) -> Result<Option<Result<ContainerExtentHeader, ContainerFormatError>>, ContainerError> {
        let at = extent * self.extent_bytes();
        if at + ContainerExtentHeader::ENCODED_BYTES as u64 > file_bytes {
            return Ok(None);
        }
        let mut bytes = [0_u8; ContainerExtentHeader::ENCODED_BYTES];
        read_exact_at(self.file(), &mut bytes, at)?;
        Ok(Some(ContainerExtentHeader::decode(&bytes)))
    }

    fn verify_directory(
        &self,
        file_bytes: u64,
        findings: &mut Findings,
    ) -> Result<(), ContainerError> {
        let directory = self.superblock.directory_extent;
        let start = directory * self.extent_bytes() + ContainerExtentHeader::PAYLOAD_OFFSET;
        let mut bytes = vec![0_u8; ContainerCollectionHeader::BLOCK_BYTES];
        for block in 0..self.directory_blocks() {
            let at = start + block * BLOCK_BYTES;
            if at + BLOCK_BYTES > file_bytes {
                break;
            }
            read_exact_at(self.file(), &mut bytes, at)?;
            if all_zero(&bytes) {
                break;
            }
            let block = u32::try_from(block).map_err(|_| ContainerFormatError::Oversized)?;
            let decoded = if self.is_encrypted() {
                ContainerCollectionHeader::decode_sealed(&bytes).map(|_| None)
            } else {
                ContainerCollectionHeader::decode(&bytes).map(|header| Some(header.id))
            };
            let collection = match decoded {
                Ok(id) => id.or_else(|| {
                    self.collections
                        .iter()
                        .find(|collection| collection.block == block)
                        .map(|collection| collection.header.id)
                }),
                Err(error) => {
                    findings.error(
                        "directoryBlockDamaged",
                        Some(directory),
                        format!("collection block {block} is damaged: {error}"),
                    );
                    None
                }
            };
            let checkpoints = &bytes[ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET
                ..ContainerCollectionHeader::GENERATION_SLOTS_OFFSET];
            let generations = &bytes[ContainerCollectionHeader::GENERATION_SLOTS_OFFSET..];
            verify_slot_pair(
                block,
                "checkpoint",
                checkpoints,
                ContainerCheckpointSlot::ENCODED_BYTES,
                |slot| ContainerCheckpointSlot::decode(slot).map(|_| ()),
                findings,
            );
            verify_slot_pair(
                block,
                "generation",
                generations,
                ContainerGenerationSlot::ENCODED_BYTES,
                |slot| ContainerGenerationSlot::decode(slot).map(|_| ()),
                findings,
            );
            if let Some((slot, _)) = ContainerCheckpointSlot::newest(checkpoints) {
                for (kind, snapshot) in [
                    (ContainerExtentKind::Locations, slot.locations),
                    (ContainerExtentKind::Index, slot.index),
                ] {
                    if let Some(snapshot) = snapshot {
                        self.verify_snapshot(
                            (block, collection),
                            kind,
                            &snapshot,
                            file_bytes,
                            findings,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Check a snapshot run a checkpoint slot names: inside the file, every
    /// extent of the right kind and one owner (the collection's, when it is
    /// known), and its stored bytes hashing to the recorded SHA-256. The
    /// bytes are hashed as stored, sealed in an encrypted file, so no key is
    /// needed.
    fn verify_snapshot(
        &self,
        (block, collection): (u32, Option<u32>),
        kind: ContainerExtentKind,
        snapshot: &ContainerSnapshotRef,
        file_bytes: u64,
        findings: &mut Findings,
    ) -> Result<(), ContainerError> {
        let first = snapshot.first_extent;
        let extents = u64::from(snapshot.extent_count);
        let capacity = self.payload_capacity();
        let name = kind.name();
        let inside = first
            .checked_add(extents)
            .and_then(|end| end.checked_mul(self.extent_bytes()))
            .is_some_and(|end| end <= file_bytes);
        if first == 0 || !inside {
            findings.error(
                "snapshotOutOfRange",
                Some(first),
                format!(
                    "collection block {block}'s {name} snapshot names {extents} extents from \
                     {first}, past the end of the file"
                ),
            );
            return Ok(());
        }
        if extents
            .checked_mul(capacity)
            .is_none_or(|holds| snapshot.length > holds)
        {
            findings.error(
                "snapshotOutOfRange",
                Some(first),
                format!(
                    "collection block {block}'s {name} snapshot is {} bytes, more than its {} \
                     extents hold",
                    snapshot.length, extents
                ),
            );
            return Ok(());
        }
        let mut owner = collection;
        let mut unmapped = false;
        for extent in first..first + extents {
            let found = if let Some((found_kind, found_owner, _)) = self.allocator.in_use(extent) {
                Some((found_kind, found_owner))
            } else {
                // Written after the last checkpoint: only the header says
                // what the extent holds.
                unmapped = true;
                self.read_extent_header(extent, file_bytes)?
                    .and_then(Result::ok)
                    .filter(|header| header.index == extent)
                    .map(|header| (header.kind, header.owner))
            };
            match found {
                Some((found_kind, found_owner))
                    if found_kind == kind && owner.is_none_or(|owner| owner == found_owner) =>
                {
                    owner = Some(found_owner);
                }
                _ => {
                    findings.error(
                        "snapshotExtentMismatch",
                        Some(extent),
                        format!(
                            "extent {extent} of collection block {block}'s {name} snapshot does \
                             not hold that snapshot"
                        ),
                    );
                    return Ok(());
                }
            }
        }
        if unmapped {
            findings.warning(
                "snapshotNotInMap",
                Some(first),
                format!(
                    "collection block {block}'s {name} snapshot was written after the last map \
                     checkpoint"
                ),
            );
        }
        if self.hash_run(first, snapshot.length)? != snapshot.sha256 {
            findings.error(
                "snapshotHashMismatch",
                Some(first),
                format!(
                    "collection block {block}'s {name} snapshot does not match its SHA-256; it \
                     will be rebuilt from the segments"
                ),
            );
        }
        Ok(())
    }

    /// SHA-256 of `length` payload bytes laid across consecutive extents
    /// from `first`, read in bounded chunks. The caller has checked the run
    /// lies inside the file; it may reach past the map's end.
    fn hash_run(&self, first: u64, length: u64) -> Result<[u8; 32], ContainerError> {
        let capacity = self.payload_capacity();
        let chunk = usize::try_from(HASH_CHUNK_BYTES.min(capacity).min(length.max(1)))
            .map_err(|_| ContainerFormatError::Oversized)?;
        let mut buffer = vec![0_u8; chunk];
        let mut hasher = Sha256::new();
        let mut remaining = length;
        let mut extent = first;
        while remaining > 0 {
            let in_extent = remaining.min(capacity);
            let mut offset = 0;
            while offset < in_extent {
                let take = usize::try_from((in_extent - offset).min(chunk as u64))
                    .map_err(|_| ContainerFormatError::Oversized)?;
                let at =
                    extent * self.extent_bytes() + ContainerExtentHeader::PAYLOAD_OFFSET + offset;
                read_exact_at(self.file(), &mut buffer[..take], at)?;
                hasher.update(&buffer[..take]);
                offset += take as u64;
            }
            remaining -= in_extent;
            extent += 1;
        }
        Ok(hasher.finalize().into())
    }
}

/// Database-level extents belong to no collection; every other extent
/// belongs to a collection in the directory, when the directory is readable.
fn verify_owner(
    extent: u64,
    kind: ContainerExtentKind,
    owner: u32,
    collection_ids: Option<&[u32]>,
    findings: &mut Findings,
) {
    let database_level = matches!(
        kind,
        ContainerExtentKind::Directory | ContainerExtentKind::Map
    );
    if database_level {
        if owner != 0 {
            findings.error(
                "unexpectedOwner",
                Some(extent),
                format!(
                    "{} extent {extent} names owner {owner}; database extents have none",
                    kind.name()
                ),
            );
        }
    } else if owner == 0 || collection_ids.is_some_and(|ids| !ids.contains(&owner)) {
        findings.error(
            "unknownOwner",
            Some(extent),
            format!(
                "{} extent {extent} names collection {owner}, which the directory does not hold",
                kind.name()
            ),
        );
    }
}

/// A slot pair is written one slot at a time: one valid slot beside a
/// damaged one is a write that was cut short, and the valid slot stands.
/// Neither valid, with something written, loses the pair.
fn verify_slot_pair(
    block: u32,
    name: &str,
    pair: &[u8],
    width: usize,
    decode: impl Fn(&[u8]) -> Result<(), ContainerFormatError>,
    findings: &mut Findings,
) {
    let (a, b) = pair.split_at(width.min(pair.len()));
    let state = |slot: &[u8]| (decode(slot).is_ok(), all_zero(slot));
    match (state(a), state(b)) {
        ((true, _), (false, false)) | ((false, false), (true, _)) => findings.warning(
            "tornSlot",
            None,
            format!(
                "collection block {block} has one damaged {name} slot; a write was interrupted \
                 and the other slot stands"
            ),
        ),
        ((false, a_zero), (false, b_zero)) if !(a_zero && b_zero) => findings.error(
            "slotDamaged",
            None,
            format!("collection block {block} has no valid {name} slot"),
        ),
        _ => {}
    }
}

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

#[cfg(test)]
mod tests;
