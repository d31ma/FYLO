use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use fylo_format::LogRecord;

use super::{
    LocationState, LocationTable, SegmentDocument, SegmentLocation, SegmentMedium, SegmentReader,
    TableCache,
};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// A reader's view of one segment collection at one published slot.
///
/// A point read is one `pread`. A batch sorts its locations by segment and
/// offset and reads neighbouring records together, so a broad query reads a
/// segment in a few large requests instead of one per document.
#[derive(Debug)]
pub(crate) struct SegmentStore {
    medium: Arc<dyn SegmentMedium>,
    table: Arc<LocationTable>,
    reader: SegmentReader,
}

impl SegmentStore {
    const MAX_SPAN_BYTES: u64 = 1024 * 1024;
    const MAX_GAP_BYTES: u64 = 64 * 1024;

    /// The view a reader gets: the location log as far as the published
    /// slot says it reaches.
    pub(crate) fn open(medium: Arc<dyn SegmentMedium>) -> Result<Self, NativeStorageError> {
        let slot = medium.require_slot()?;
        let table =
            TableCache::shared().table(&*medium, slot.table_epoch, slot.locations_length)?;
        Ok(Self::with(medium, table))
    }

    /// The view a writer plans against under the collection lock: every
    /// location row already appended, including those of busy-period
    /// members that have applied but not yet published.
    pub(crate) fn open_for_writer(
        medium: Arc<dyn SegmentMedium>,
    ) -> Result<Self, NativeStorageError> {
        let slot = medium.require_slot()?;
        let wal_length = medium.location_log_length()?;
        let table = TableCache::shared().table(&*medium, slot.table_epoch, wal_length)?;
        Ok(Self::with(medium, table))
    }

    fn with(medium: Arc<dyn SegmentMedium>, table: Arc<LocationTable>) -> Self {
        Self {
            reader: SegmentReader::new(Arc::clone(&medium)),
            medium,
            table,
        }
    }

    pub(crate) fn table(&self) -> &LocationTable {
        &self.table
    }

    pub(crate) fn medium(&self) -> &dyn SegmentMedium {
        &*self.medium
    }

    pub(crate) fn ids(&self, state: LocationState) -> Vec<String> {
        self.table.ids(state)
    }

    /// The location of `id`, if its newest entry is in `state`.
    pub(crate) fn location(&self, id: &str, state: LocationState) -> Option<&SegmentLocation> {
        self.table
            .get(id)
            .filter(|location| location.state == state)
    }

    pub(crate) fn document(
        &self,
        id: &str,
        state: LocationState,
    ) -> Result<SegmentDocument, NativeStorageError> {
        let location = self.require_location(id, state)?;
        let record = self.reader.record(location)?;
        SegmentDocument::from_record(location, &record)
    }

    /// Read many documents, in the order asked, with coalesced reads.
    pub(crate) fn documents(
        &self,
        ids: &[String],
        state: LocationState,
    ) -> Result<Vec<SegmentDocument>, NativeStorageError> {
        let locations = ids
            .iter()
            .map(|id| self.require_location(id, state))
            .collect::<Result<Vec<_>, _>>()?;
        let mut order: Vec<usize> = (0..locations.len()).collect();
        order.sort_by_key(|index| (locations[*index].segment, locations[*index].offset));
        let mut documents: Vec<Option<SegmentDocument>> = vec![None; locations.len()];
        for group in Self::spans(&locations, &order) {
            self.read_group(&locations, &group, &mut documents)?;
        }
        Ok(documents.into_iter().flatten().collect())
    }

    fn require_location(
        &self,
        id: &str,
        state: LocationState,
    ) -> Result<&SegmentLocation, NativeStorageError> {
        self.location(id, state).ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                format!("document not found: {id}"),
            )
        })
    }

    /// Split locations, already in segment and offset order, into groups
    /// that one read can cover.
    fn spans(locations: &[&SegmentLocation], order: &[usize]) -> Vec<Vec<usize>> {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut current: Option<(u64, u64, u64)> = None;
        for index in order {
            let location = locations[*index];
            let end = location.offset + u64::from(location.length);
            match current {
                Some((segment, start, span_end))
                    if Self::joins(location, (segment, start, span_end), end) =>
                {
                    current = Some((segment, start, span_end.max(end)));
                    groups.last_mut().expect("a group is open").push(*index);
                }
                _ => {
                    current = Some((location.segment, location.offset, end));
                    groups.push(vec![*index]);
                }
            }
        }
        groups
    }

    fn joins(
        location: &SegmentLocation,
        (segment, start, span_end): (u64, u64, u64),
        end: u64,
    ) -> bool {
        location.segment == segment
            && location.offset <= span_end + Self::MAX_GAP_BYTES
            && end - start <= Self::MAX_SPAN_BYTES
    }

    fn read_group(
        &self,
        locations: &[&SegmentLocation],
        group: &[usize],
        documents: &mut [Option<SegmentDocument>],
    ) -> Result<(), NativeStorageError> {
        let first = locations[group[0]];
        let start = first.offset;
        let end = group
            .iter()
            .map(|index| locations[*index].offset + u64::from(locations[*index].length))
            .max()
            .unwrap_or(start);
        let span = self
            .reader
            .span(first.segment, start, Self::span_length(end - start)?)?;
        let mut records: BTreeMap<u64, LogRecord> = BTreeMap::new();
        for index in group {
            let location = locations[*index];
            if let Entry::Vacant(slot) = records.entry(location.offset) {
                let relative = Self::span_length(location.offset - start)?;
                let frame = &span[relative..relative + location.length as usize];
                slot.insert(SegmentReader::decode(frame)?);
            }
            let record = &records[&location.offset];
            documents[*index] = Some(SegmentDocument::from_record(location, record)?);
        }
        Ok(())
    }

    fn span_length(length: u64) -> Result<usize, NativeStorageError> {
        usize::try_from(length).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "segment read exceeds the address space",
            )
        })
    }
}
