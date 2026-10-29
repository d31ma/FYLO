use super::{RecordPosition, SegmentMedium, SlotRecord};
use crate::NativeStorageError;

/// How far a location table reaches past its snapshot: a length of the
/// location log, or, for a medium without one, the record position up to
/// which it has replayed the records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TableReach {
    Log(u64),
    Records(RecordPosition),
}

impl Default for TableReach {
    fn default() -> Self {
        Self::Log(0)
    }
}

impl TableReach {
    /// What a reader at `slot` sees: the location log as long as the slot
    /// recorded, or the records up to the slot's end.
    pub(crate) fn published(medium: &dyn SegmentMedium, slot: &SlotRecord) -> Self {
        if medium.has_location_log() {
            Self::Log(slot.locations_length)
        } else {
            Self::Records(RecordPosition::published(slot))
        }
    }

    /// What the writer sees under the collection lock: every row appended
    /// to the location log, or every record written to the active segment,
    /// including those of busy-period members that have not published yet.
    pub(crate) fn written(medium: &dyn SegmentMedium) -> Result<Self, NativeStorageError> {
        if medium.has_location_log() {
            return Ok(Self::Log(medium.location_log_length()?));
        }
        let slot = medium.require_slot()?;
        Ok(Self::Records(RecordPosition {
            segment: slot.segment,
            offset: medium.segment_length(slot.segment)?,
        }))
    }

    /// The table as the location snapshot holds it: the snapshot with the
    /// first `wal_length` bytes of the log a checkpoint recorded, or the
    /// snapshot alone.
    pub(crate) fn checkpoint(medium: &dyn SegmentMedium, wal_length: u64) -> Self {
        if medium.has_location_log() {
            Self::Log(wal_length)
        } else {
            Self::Records(RecordPosition::START)
        }
    }

    /// Whether a table reaching `self` can be extended to `later`.
    pub(crate) fn precedes(self, later: Self) -> bool {
        match (self, later) {
            (Self::Log(applied), Self::Log(length)) => applied < length,
            (Self::Records(applied), Self::Records(position)) => applied < position,
            _ => false,
        }
    }
}
