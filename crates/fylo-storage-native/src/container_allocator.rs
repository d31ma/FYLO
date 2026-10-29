//! Extent allocation for `.fxdb` containers (ADR 0016 section 6).
//!
//! This is the in-memory model of the extent map: which extents are free,
//! which hold what, and which were released since the last checkpoint. It does
//! no I/O. The container writes the extent headers, seal records and map pages
//! it decides on, and calls back here once they are durable.

use fylo_format::{ContainerExtentKind, ContainerExtentMapPage, ContainerExtentState};

/// One extent as the allocator sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    /// Extent 0: the superblock region, never allocated.
    Reserved,
    /// Reusable.
    Free,
    /// Holds data.
    InUse {
        kind: ContainerExtentKind,
        owner: u32,
        sequence: u64,
        sealed: bool,
        live_bytes: u64,
    },
    /// Released by a committed compaction, not yet reusable. A reader in
    /// another process may still hold a location into it until a checkpoint
    /// has written a map that shows it free.
    Released,
}

/// What [`ContainerExtentAllocator::allocate`] handed out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerExtentAllocation {
    /// The extent's index.
    pub index: u64,
    /// Its new, never-reused sequence number, for its header.
    pub sequence: u64,
    /// Whether the file must grow by one extent to hold it.
    pub grows_file: bool,
}

/// How to shrink a file by relocating its last extents (ADR 0016 section 6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerShrinkPlan {
    /// `(from, to)` extent moves, last extent first. Each is a compaction copy
    /// into a free extent nearer the front.
    pub moves: Vec<(u64, u64)>,
    /// Extent count once the moves are committed and checkpointed; the file is
    /// then truncated to this many extents.
    pub extent_count: u64,
}

/// The last extent in use, which a shrink moves nearer the front next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContainerTailExtent {
    /// Its index.
    pub(crate) index: u64,
    /// What it holds.
    pub(crate) kind: ContainerExtentKind,
    /// Its owning collection, or 0.
    pub(crate) owner: u32,
    /// Its sequence number, a segment's number.
    pub(crate) sequence: u64,
    /// Whether it is sealed.
    pub(crate) sealed: bool,
}

/// Extent counts by state and kind.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerExtentCensus {
    /// Reusable extents.
    pub free: u64,
    /// Extents replaced since the last checkpoint, reusable after the next.
    pub released: u64,
    /// Extents in use, by kind name.
    pub by_kind: std::collections::BTreeMap<&'static str, ContainerKindCensus>,
}

/// Extents of one kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerKindCensus {
    /// Extents of this kind.
    pub extents: u64,
    /// How many of them are sealed.
    pub sealed: u64,
    /// Bytes the sealed ones still hold live.
    pub live_bytes: u64,
}

/// Why an allocator operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerAllocatorError {
    /// The extent is not in use, so it cannot be sealed or released.
    NotInUse,
    /// The index is past the end of the file, or is the superblock region.
    OutOfRange,
    /// The map pages do not describe every extent exactly once, or describe
    /// extent 0 as anything but in use by the format.
    InconsistentMap,
}

/// The in-memory extent map of one `.fxdb` container.
#[derive(Clone, Debug)]
pub struct ContainerExtentAllocator {
    slots: Vec<Slot>,
    next_sequence: u64,
    /// Extents taken since the last durable map, which that map shows free.
    unmapped: std::collections::BTreeSet<u64>,
    /// Whether anything changed since the last durable map.
    changed: bool,
}

impl ContainerExtentAllocator {
    /// A new file: extent 0 is the superblock region and nothing else exists.
    #[must_use]
    pub fn new(next_sequence: u64) -> Self {
        Self {
            slots: vec![Slot::Reserved],
            next_sequence,
            unmapped: std::collections::BTreeSet::new(),
            changed: false,
        }
    }

    /// Rebuild the allocator from the extent-map pages of the last checkpoint.
    ///
    /// Recovery then replays the allocations, seals and releases recorded
    /// after that checkpoint.
    ///
    /// # Errors
    ///
    /// Returns `InconsistentMap` unless the pages cover extents 1 through
    /// `extent_count - 1` exactly once.
    pub fn from_map(
        extent_count: u64,
        next_sequence: u64,
        pages: &[ContainerExtentMapPage],
    ) -> Result<Self, ContainerAllocatorError> {
        let count =
            usize::try_from(extent_count).map_err(|_| ContainerAllocatorError::InconsistentMap)?;
        if count == 0 {
            return Err(ContainerAllocatorError::InconsistentMap);
        }
        let mut slots: Vec<Option<Slot>> = vec![None; count];
        slots[0] = Some(Slot::Reserved);
        for page in pages {
            for (offset, entry) in page.entries.iter().enumerate() {
                let index = page
                    .first_extent
                    .checked_add(offset as u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index >= 1 && *index < count)
                    .ok_or(ContainerAllocatorError::InconsistentMap)?;
                if slots[index].is_some() {
                    return Err(ContainerAllocatorError::InconsistentMap);
                }
                slots[index] = Some(match *entry {
                    ContainerExtentState::Free => Slot::Free,
                    ContainerExtentState::Owned {
                        kind,
                        owner,
                        sequence,
                    } => Slot::InUse {
                        kind,
                        owner,
                        sequence,
                        sealed: false,
                        live_bytes: 0,
                    },
                    ContainerExtentState::Sealed {
                        kind,
                        owner,
                        sequence,
                        live_bytes,
                    } => Slot::InUse {
                        kind,
                        owner,
                        sequence,
                        sealed: true,
                        live_bytes,
                    },
                });
            }
        }
        let slots = slots
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(ContainerAllocatorError::InconsistentMap)?;
        Ok(Self {
            slots,
            next_sequence,
            unmapped: std::collections::BTreeSet::new(),
            changed: false,
        })
    }

    /// Extent slots in the file, including the superblock region.
    #[must_use]
    pub fn extent_count(&self) -> u64 {
        self.slots.len() as u64
    }

    /// The next sequence number, for the superblock.
    #[must_use]
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Take the lowest-numbered free extent, or grow the file by one.
    ///
    /// Lowest first keeps data drifting toward the front of the file, so a
    /// later tail truncation has less to move.
    pub fn allocate(&mut self, kind: ContainerExtentKind, owner: u32) -> ContainerExtentAllocation {
        self.changed = true;
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let taken = Slot::InUse {
            kind,
            owner,
            sequence,
            sealed: false,
            live_bytes: 0,
        };
        if let Some(index) = self.slots.iter().position(|slot| *slot == Slot::Free) {
            self.slots[index] = taken;
            self.unmapped.insert(index as u64);
            return ContainerExtentAllocation {
                index: index as u64,
                sequence,
                grows_file: false,
            };
        }
        self.slots.push(taken);
        self.unmapped.insert((self.slots.len() - 1) as u64);
        ContainerExtentAllocation {
            index: (self.slots.len() - 1) as u64,
            sequence,
            grows_file: true,
        }
    }

    /// Take `count` consecutive extents: the lowest run of free ones, a free
    /// tail extended by growing the file, or new extents at the end. A map
    /// copy needs a run, because the superblock names only its first extent
    /// and its length.
    pub fn allocate_run(
        &mut self,
        kind: ContainerExtentKind,
        owner: u32,
        count: usize,
    ) -> Vec<ContainerExtentAllocation> {
        self.changed = true;
        let mut start = self.slots.len();
        let mut run = 0;
        for (index, slot) in self.slots.iter().enumerate() {
            if *slot == Slot::Free {
                if run == 0 {
                    start = index;
                }
                run += 1;
                if run == count {
                    break;
                }
            } else {
                run = 0;
                start = self.slots.len();
            }
        }
        (start..start + count)
            .map(|index| {
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                let taken = Slot::InUse {
                    kind,
                    owner,
                    sequence,
                    sealed: false,
                    live_bytes: 0,
                };
                let grows_file = index == self.slots.len();
                if grows_file {
                    self.slots.push(taken);
                } else {
                    self.slots[index] = taken;
                }
                self.unmapped.insert(index as u64);
                ContainerExtentAllocation {
                    index: index as u64,
                    sequence,
                    grows_file,
                }
            })
            .collect()
    }

    /// Mark an extent immutable, with the bytes the location table still
    /// references in it.
    ///
    /// # Errors
    ///
    /// Returns `OutOfRange` or `NotInUse`.
    pub fn seal(&mut self, index: u64, live: u64) -> Result<(), ContainerAllocatorError> {
        match self.slot_mut(index)? {
            Slot::InUse {
                sealed, live_bytes, ..
            } => {
                *sealed = true;
                *live_bytes = live;
                self.changed = true;
                Ok(())
            }
            _ => Err(ContainerAllocatorError::NotInUse),
        }
    }

    /// Release an extent that a committed compaction replaced. It becomes
    /// reusable only after [`Self::checkpoint_completed`].
    ///
    /// # Errors
    ///
    /// Returns `OutOfRange` or `NotInUse`.
    pub fn release(&mut self, index: u64) -> Result<(), ContainerAllocatorError> {
        let slot = self.slot_mut(index)?;
        if !matches!(slot, Slot::InUse { .. }) {
            return Err(ContainerAllocatorError::NotInUse);
        }
        *slot = Slot::Released;
        self.changed = true;
        Ok(())
    }

    /// Mark a free extent as holding allocation `sequence`, which a slot
    /// names although the durable map predates it, growing the map to
    /// reach it. The extent counts as unmapped until the next checkpoint,
    /// and the sequence counter moves past `sequence`.
    ///
    /// # Errors
    ///
    /// Returns `OutOfRange` for extent 0 and `InconsistentMap` when the
    /// extent is not free.
    pub fn reclaim(
        &mut self,
        index: u64,
        kind: ContainerExtentKind,
        owner: u32,
        sequence: u64,
    ) -> Result<(), ContainerAllocatorError> {
        let at = usize::try_from(index)
            .ok()
            .filter(|at| *at >= 1)
            .ok_or(ContainerAllocatorError::OutOfRange)?;
        if self.slots.len() <= at {
            self.slots.resize(at + 1, Slot::Free);
        }
        if self.slots[at] != Slot::Free {
            return Err(ContainerAllocatorError::InconsistentMap);
        }
        self.slots[at] = Slot::InUse {
            kind,
            owner,
            sequence,
            sealed: false,
            live_bytes: 0,
        };
        self.unmapped.insert(index);
        self.next_sequence = self.next_sequence.max(sequence + 1);
        self.changed = true;
        Ok(())
    }

    /// The map pages a checkpoint writes. Released extents are written as
    /// free: once these pages are durable, no reader can resolve a location
    /// into them any more.
    #[must_use]
    pub fn map_pages(&self) -> Vec<ContainerExtentMapPage> {
        let entries: Vec<ContainerExtentState> = self.slots[1..]
            .iter()
            .map(|slot| match *slot {
                Slot::InUse {
                    kind,
                    owner,
                    sequence,
                    sealed: false,
                    ..
                } => ContainerExtentState::Owned {
                    kind,
                    owner,
                    sequence,
                },
                Slot::InUse {
                    kind,
                    owner,
                    sequence,
                    sealed: true,
                    live_bytes,
                } => ContainerExtentState::Sealed {
                    kind,
                    owner,
                    sequence,
                    live_bytes,
                },
                Slot::Free | Slot::Released | Slot::Reserved => ContainerExtentState::Free,
            })
            .collect();
        entries
            .chunks(ContainerExtentMapPage::ENTRIES_PER_PAGE)
            .enumerate()
            .map(|(page, chunk)| ContainerExtentMapPage {
                first_extent: 1 + (page * ContainerExtentMapPage::ENTRIES_PER_PAGE) as u64,
                entries: chunk.to_vec(),
            })
            .collect()
    }

    /// The pages from [`Self::map_pages`] are durable: released extents are
    /// now free and may be reused.
    pub fn checkpoint_completed(&mut self) {
        for slot in &mut self.slots {
            if *slot == Slot::Released {
                *slot = Slot::Free;
            }
        }
        self.unmapped.clear();
        self.changed = false;
    }

    /// Whether the in-memory map differs from the last durable one: an
    /// extent taken, sealed or released since, which a checkpoint would
    /// record.
    #[must_use]
    pub fn has_unsaved_changes(&self) -> bool {
        self.changed
    }

    /// Whether the durable map already shows `index` in use. An extent taken
    /// since the last checkpoint is not, so recovery finds it only through
    /// a generation slot that names it.
    #[must_use]
    pub fn is_mapped(&self, index: u64) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|at| self.slots.get(at))
            .is_some_and(|slot| matches!(slot, Slot::InUse { .. }))
            && !self.unmapped.contains(&index)
    }

    /// The in-use extent holding allocation `sequence`, with its kind and
    /// owner: how a segment number, which is a sequence number, finds its
    /// extent.
    #[must_use]
    pub fn find_sequence(&self, sequence: u64) -> Option<(u64, ContainerExtentKind, u32)> {
        self.slots
            .iter()
            .enumerate()
            .find_map(|(index, slot)| match *slot {
                Slot::InUse {
                    kind,
                    owner,
                    sequence: held,
                    ..
                } if held == sequence => Some((index as u64, kind, owner)),
                _ => None,
            })
    }

    /// The in-use extents of one kind and owner, as `(index, sequence)` in
    /// sequence order: a collection's segments, oldest first.
    #[must_use]
    pub fn owned(&self, kind: ContainerExtentKind, owner: u32) -> Vec<(u64, u64)> {
        let mut extents: Vec<(u64, u64)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match *slot {
                Slot::InUse {
                    kind: held_kind,
                    owner: held_owner,
                    sequence,
                    ..
                } if held_kind == kind && held_owner == owner => Some((index as u64, sequence)),
                _ => None,
            })
            .collect();
        extents.sort_unstable_by_key(|(_, sequence)| *sequence);
        extents
    }

    /// Forget the free extents at the end of the file, so the next
    /// checkpoint's map no longer describes them and the file can be cut
    /// after them. Returns the new extent count. Released extents are kept:
    /// they become free only once a checkpoint is durable.
    pub fn truncate_free_tail(&mut self) -> u64 {
        while self.slots.len() > 1 && self.slots.last() == Some(&Slot::Free) {
            self.slots.pop();
            self.changed = true;
        }
        self.extent_count()
    }

    /// How many extents are in each state, for `fylo inspect`.
    #[must_use]
    pub fn census(&self) -> ContainerExtentCensus {
        let mut census = ContainerExtentCensus::default();
        for slot in &self.slots[1..] {
            match *slot {
                Slot::Free => census.free += 1,
                Slot::Released => census.released += 1,
                Slot::InUse {
                    kind,
                    sealed,
                    live_bytes,
                    ..
                } => {
                    let entry = census.by_kind.entry(kind.name()).or_default();
                    entry.extents += 1;
                    if sealed {
                        entry.sealed += 1;
                        entry.live_bytes += live_bytes;
                    }
                }
                Slot::Reserved => {}
            }
        }
        census
    }

    /// What one extent holds, for `fylo verify`: its kind, owner and
    /// sequence number when it is in use (owned or sealed), `None` when it
    /// is free, released, reserved or past the end of the map.
    pub(crate) fn in_use(&self, index: u64) -> Option<(ContainerExtentKind, u32, u64)> {
        match usize::try_from(index)
            .ok()
            .and_then(|at| self.slots.get(at))
        {
            Some(Slot::InUse {
                kind,
                owner,
                sequence,
                ..
            }) => Some((*kind, *owner, *sequence)),
            _ => None,
        }
    }

    /// The live bytes an extent was sealed with, or `None` while it is not
    /// sealed (active, free, released or past the end of the map).
    #[must_use]
    pub fn sealed_live_bytes(&self, index: u64) -> Option<u64> {
        match usize::try_from(index)
            .ok()
            .and_then(|at| self.slots.get(at))
        {
            Some(Slot::InUse {
                sealed: true,
                live_bytes,
                ..
            }) => Some(*live_bytes),
            _ => None,
        }
    }

    /// Whether the map shows an extent free and reusable.
    pub(crate) fn is_free(&self, index: u64) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|at| self.slots.get(at))
            .is_some_and(|slot| *slot == Slot::Free)
    }

    /// Free extents, whose disk space may be returned by punching a hole.
    #[must_use]
    pub fn free_extents(&self) -> Vec<u64> {
        self.indices_where(|slot| *slot == Slot::Free)
    }

    /// Plan a tail truncation: move sealed extents from the end into free
    /// extents nearer the front, last first. An unsealed (active) extent is
    /// never moved, and nothing moves past it.
    #[must_use]
    pub fn shrink_plan(&self) -> ContainerShrinkPlan {
        let mut free = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| **slot == Slot::Free)
            .map(|(index, _)| index);
        let mut moves = Vec::new();
        let mut kept = vec![true; self.slots.len()];
        let mut targets_taken = Vec::new();
        for index in (1..self.slots.len()).rev() {
            match self.slots[index] {
                Slot::Free => kept[index] = false,
                Slot::InUse { sealed: true, .. } => {
                    match free.next().filter(|target| *target < index) {
                        Some(target) => {
                            moves.push((index as u64, target as u64));
                            targets_taken.push(target);
                            kept[index] = false;
                        }
                        None => break,
                    }
                }
                Slot::InUse { sealed: false, .. } | Slot::Released | Slot::Reserved => break,
            }
        }
        for target in targets_taken {
            kept[target] = true;
        }
        let extent_count = kept
            .iter()
            .rposition(|keep| *keep)
            .map_or(1, |last| last as u64 + 1);
        ContainerShrinkPlan {
            moves,
            extent_count,
        }
    }

    /// The last extent in use, when a free extent lies before it, so that
    /// moving it lets the file end sooner. `None` when no extent before the
    /// last in use is free; a released one is free after the next
    /// checkpoint.
    pub(crate) fn movable_tail(&self) -> Option<ContainerTailExtent> {
        let last = self
            .slots
            .iter()
            .rposition(|slot| matches!(slot, Slot::InUse { .. }))?;
        if !self.slots[1..last].contains(&Slot::Free) {
            return None;
        }
        match self.slots[last] {
            Slot::InUse {
                kind,
                owner,
                sequence,
                sealed,
                ..
            } => Some(ContainerTailExtent {
                index: last as u64,
                kind,
                owner,
                sequence,
                sealed,
            }),
            _ => None,
        }
    }

    fn indices_where(&self, wanted: impl Fn(&Slot) -> bool) -> Vec<u64> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| wanted(slot))
            .map(|(index, _)| index as u64)
            .collect()
    }

    fn slot_mut(&mut self, index: u64) -> Result<&mut Slot, ContainerAllocatorError> {
        usize::try_from(index)
            .ok()
            .filter(|index| *index >= 1)
            .and_then(|index| self.slots.get_mut(index))
            .ok_or(ContainerAllocatorError::OutOfRange)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerAllocatorError, ContainerExtentAllocator, ContainerShrinkPlan};
    use fylo_format::{ContainerExtentKind, ContainerExtentMapPage};

    const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;

    #[test]
    fn grows_then_reuses_the_lowest_free_extent() {
        let mut allocator = ContainerExtentAllocator::new(1);
        let first = allocator.allocate(SEGMENT, 1);
        let second = allocator.allocate(SEGMENT, 1);
        assert_eq!(
            (first.index, first.sequence, first.grows_file),
            (1, 1, true)
        );
        assert_eq!(
            (second.index, second.sequence, second.grows_file),
            (2, 2, true)
        );
        allocator.seal(1, 0).unwrap();
        allocator.release(1).unwrap();
        allocator.checkpoint_completed();
        let reused = allocator.allocate(SEGMENT, 2);
        assert_eq!(
            (reused.index, reused.sequence, reused.grows_file),
            (1, 3, false)
        );
    }

    #[test]
    fn does_not_reuse_a_released_extent_before_the_checkpoint() {
        let mut allocator = ContainerExtentAllocator::new(1);
        allocator.allocate(SEGMENT, 1);
        allocator.release(1).unwrap();
        assert_eq!(allocator.allocate(SEGMENT, 1).index, 2);
        assert!(allocator.free_extents().is_empty());
        allocator.checkpoint_completed();
        assert_eq!(allocator.free_extents(), vec![1]);
    }

    #[test]
    fn allocates_runs_from_a_gap_a_free_tail_or_the_end() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..6 {
            allocator.allocate(SEGMENT, 1);
        }
        for index in [1, 3, 4, 6] {
            allocator.release(index).unwrap();
        }
        allocator.checkpoint_completed();
        let indices = |run: &[super::ContainerExtentAllocation]| {
            run.iter()
                .map(|allocation| (allocation.index, allocation.grows_file))
                .collect::<Vec<_>>()
        };
        let gap = allocator.allocate_run(ContainerExtentKind::Map, 0, 2);
        assert_eq!(indices(&gap), vec![(3, false), (4, false)]);
        let tail = allocator.allocate_run(ContainerExtentKind::Map, 0, 2);
        assert_eq!(indices(&tail), vec![(6, false), (7, true)]);
        let end = allocator.allocate_run(ContainerExtentKind::Map, 0, 2);
        assert_eq!(indices(&end), vec![(8, true), (9, true)]);
        assert_eq!(allocator.extent_count(), 10);
        assert_eq!(allocator.free_extents(), vec![1]);
    }

    #[test]
    fn truncates_only_free_extents_at_the_end() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..5 {
            allocator.allocate(SEGMENT, 1);
        }
        for index in [2, 4, 5] {
            allocator.release(index).unwrap();
        }
        // Released, not yet free: nothing goes.
        assert_eq!(allocator.truncate_free_tail(), 6);
        allocator.checkpoint_completed();
        assert_eq!(allocator.truncate_free_tail(), 4);
        assert_eq!(allocator.free_extents(), vec![2]);
        assert_eq!(allocator.allocate(SEGMENT, 1).index, 2);
        assert_eq!(allocator.allocate(SEGMENT, 1).index, 4);
    }

    #[test]
    fn tracks_changes_since_the_durable_map() {
        let mut allocator = ContainerExtentAllocator::new(1);
        assert!(!allocator.has_unsaved_changes());
        allocator.allocate(SEGMENT, 1);
        assert!(allocator.has_unsaved_changes());
        allocator.checkpoint_completed();
        assert!(!allocator.has_unsaved_changes());
        allocator.seal(1, 77).unwrap();
        assert!(allocator.has_unsaved_changes());
        assert_eq!(allocator.sealed_live_bytes(1), Some(77));
        let rebuilt = ContainerExtentAllocator::from_map(2, 2, &allocator.map_pages()).unwrap();
        assert!(!rebuilt.has_unsaved_changes());
        assert_eq!(rebuilt.sealed_live_bytes(1), Some(77));
        allocator.checkpoint_completed();
        allocator.release(1).unwrap();
        assert!(allocator.has_unsaved_changes());
        assert_eq!(allocator.sealed_live_bytes(1), None);
    }

    #[test]
    fn refuses_bad_indices() {
        let mut allocator = ContainerExtentAllocator::new(1);
        assert_eq!(
            allocator.release(0),
            Err(ContainerAllocatorError::OutOfRange)
        );
        assert_eq!(
            allocator.release(5),
            Err(ContainerAllocatorError::OutOfRange)
        );
        allocator.allocate(SEGMENT, 1);
        allocator.release(1).unwrap();
        assert_eq!(allocator.release(1), Err(ContainerAllocatorError::NotInUse));
        assert_eq!(allocator.seal(1, 0), Err(ContainerAllocatorError::NotInUse));
    }

    #[test]
    fn round_trips_through_map_pages() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..300 {
            allocator.allocate(SEGMENT, 1);
        }
        allocator.seal(4, 1234).unwrap();
        allocator.release(7).unwrap();
        let pages = allocator.map_pages();
        assert_eq!(pages.len(), 3);
        for page in &pages {
            let bytes = page.encode().unwrap();
            assert_eq!(ContainerExtentMapPage::decode(&bytes).as_ref(), Ok(page));
        }
        let rebuilt =
            ContainerExtentAllocator::from_map(allocator.extent_count(), 301, &pages).unwrap();
        assert_eq!(rebuilt.map_pages(), pages);
        // The released extent comes back free, as the checkpoint wrote it.
        assert_eq!(rebuilt.free_extents(), vec![7]);
    }

    #[test]
    fn rejects_a_map_with_gaps_or_overlaps() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..3 {
            allocator.allocate(SEGMENT, 1);
        }
        let pages = allocator.map_pages();
        assert_eq!(
            ContainerExtentAllocator::from_map(5, 4, &pages).map(|_| ()),
            Err(ContainerAllocatorError::InconsistentMap)
        );
        let doubled = [pages.clone(), pages].concat();
        assert_eq!(
            ContainerExtentAllocator::from_map(4, 4, &doubled).map(|_| ()),
            Err(ContainerAllocatorError::InconsistentMap)
        );
    }

    #[test]
    fn plans_a_tail_truncation() {
        // Extents 1..=6: 1 active, 2 free, 3 sealed, 4 free, 5 sealed, 6 sealed.
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..6 {
            allocator.allocate(SEGMENT, 1);
        }
        for index in [2, 3, 4, 5, 6] {
            allocator.seal(index, 10).unwrap();
        }
        allocator.release(2).unwrap();
        allocator.release(4).unwrap();
        allocator.checkpoint_completed();
        assert_eq!(
            allocator.shrink_plan(),
            ContainerShrinkPlan {
                moves: vec![(6, 2), (5, 4)],
                extent_count: 5,
            }
        );
    }

    #[test]
    fn never_moves_an_active_extent() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..3 {
            allocator.allocate(SEGMENT, 1);
        }
        allocator.seal(1, 0).unwrap();
        allocator.release(1).unwrap();
        allocator.checkpoint_completed();
        // Extent 3 is still active, so nothing behind it can move.
        let plan = allocator.shrink_plan();
        assert!(plan.moves.is_empty());
        assert_eq!(plan.extent_count, 4);
    }

    #[test]
    fn drops_trailing_free_extents_without_moving_anything() {
        let mut allocator = ContainerExtentAllocator::new(1);
        for _ in 0..4 {
            allocator.allocate(SEGMENT, 1);
        }
        for index in [3, 4] {
            allocator.seal(index, 0).unwrap();
            allocator.release(index).unwrap();
        }
        allocator.checkpoint_completed();
        assert_eq!(
            allocator.shrink_plan(),
            ContainerShrinkPlan {
                moves: Vec::new(),
                extent_count: 3,
            }
        );
    }
}
