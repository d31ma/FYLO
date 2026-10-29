use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{LocationState, RecordPosition, SegmentLocation, SegmentMedium, TableReach};
use crate::NativeStorageError;

/// Every document's newest location: the sorted snapshot with later rows
/// laid over it.
///
/// The later rows come from the location log, applied in file order, or,
/// for a medium without one, from replaying the records after the
/// snapshot's position. The last row for an identifier wins. Both files
/// are derived and rebuild from segments. A row in the `absent` state with
/// sequence number zero only undoes a rolled-back commit's row, and folding
/// drops it.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocationTable {
    snapshot: Arc<Vec<SegmentLocation>>,
    overlay: BTreeMap<String, SegmentLocation>,
    reach: TableReach,
    /// Where a salvaging replay met a record it could not read and
    /// stopped; the table holds nothing the records from there derive.
    damage: Option<RecordPosition>,
    /// The segment and LSN of the last record replayed, so the next
    /// replay from where this one stopped checks that its first record
    /// follows it.
    replayed: Option<(u64, u64)>,
}

impl LocationTable {
    /// Load the snapshot and lay over it the rows up to `reach`: the log's
    /// first bytes, or the records from the snapshot's position.
    pub(crate) fn load(
        medium: &dyn SegmentMedium,
        reach: TableReach,
    ) -> Result<Self, NativeStorageError> {
        let (bytes, start) = match reach {
            TableReach::Log(_) => (medium.read_location_snapshot()?, TableReach::Log(0)),
            TableReach::Records(_) => {
                let (bytes, position) = medium.read_location_checkpoint()?;
                (bytes, TableReach::Records(position))
            }
        };
        let mut table = Self {
            snapshot: Arc::new(SegmentLocation::decode_all(&bytes)?),
            overlay: BTreeMap::new(),
            reach: start,
            damage: None,
            replayed: None,
        };
        table.extend(medium, reach)?;
        Ok(table)
    }

    /// The table as far as the records can be read, for `fylo verify` and
    /// `fylo salvage`. It is [`Self::load`], except that for a medium
    /// without a location log a record after the snapshot that cannot be
    /// read ends the replay there instead of failing it, as a salvage
    /// steps past what it cannot read; [`Self::damage`] then says where.
    /// Such a table is degraded and is never cached or read from: the rows
    /// past the damage are missing from it.
    pub(crate) fn load_salvaging(
        medium: &dyn SegmentMedium,
        reach: TableReach,
    ) -> Result<Self, NativeStorageError> {
        let TableReach::Records(to) = reach else {
            return Self::load(medium, reach);
        };
        let (bytes, from) = medium.read_location_checkpoint()?;
        let mut table = Self {
            snapshot: Arc::new(SegmentLocation::decode_all(&bytes)?),
            overlay: BTreeMap::new(),
            reach,
            damage: None,
            replayed: None,
        };
        table.replay_salvaging(medium, (from, to))?;
        Ok(table)
    }

    /// Where a salvaging load stopped at a record it could not read.
    pub(crate) fn damage(&self) -> Option<RecordPosition> {
        self.damage
    }

    /// The segment and LSN of the last record a replay applied, which the
    /// next replay's first record in that segment must follow.
    pub(crate) fn replayed(&self) -> Option<(u64, u64)> {
        self.replayed
    }

    /// Note the last record a replay applied.
    pub(crate) fn note_replayed(&mut self, segment: u64, lsn: u64) {
        self.replayed = Some((segment, lsn));
    }

    /// Record that the replay stopped at `at`, the first record it could
    /// not read.
    pub(crate) fn mark_damaged(&mut self, at: RecordPosition) {
        self.damage.get_or_insert(at);
    }

    /// Apply the rows between the reach already applied and `reach`: the
    /// log's rows up to that length, or the rows the records up to that
    /// position derive. A reach not past the applied one changes nothing.
    pub(crate) fn extend(
        &mut self,
        medium: &dyn SegmentMedium,
        reach: TableReach,
    ) -> Result<(), NativeStorageError> {
        match (self.reach, reach) {
            (TableReach::Log(applied), TableReach::Log(length)) if applied < length => {
                let tail = medium.read_location_log(applied, length)?;
                for row in SegmentLocation::decode_all(&tail)? {
                    self.apply(row);
                }
            }
            (TableReach::Records(applied), TableReach::Records(position)) if applied < position => {
                self.replay(medium, (applied, position))?;
            }
            _ => return Ok(()),
        }
        self.reach = reach;
        Ok(())
    }

    /// A table built from rows already known, such as a rebuild's.
    pub(crate) fn from_rows(rows: impl IntoIterator<Item = SegmentLocation>) -> Self {
        let mut table = Self::default();
        for row in rows {
            table.apply(row);
        }
        table
    }

    /// How far past its snapshot this table reaches.
    pub(crate) fn reach(&self) -> TableReach {
        self.reach
    }

    pub(crate) fn apply(&mut self, row: SegmentLocation) {
        self.overlay.insert(row.id.clone(), row);
    }

    pub(crate) fn get(&self, id: &str) -> Option<&SegmentLocation> {
        let row = match self.overlay.get(id) {
            Some(row) => Some(row),
            None => self
                .snapshot
                .binary_search_by(|row| row.id.as_str().cmp(id))
                .ok()
                .map(|index| &self.snapshot[index]),
        };
        row.filter(|row| !Self::is_undo(row))
    }

    /// Identifiers whose newest entry is in `state`, in ascending order.
    pub(crate) fn ids(&self, state: LocationState) -> Vec<String> {
        self.rows()
            .into_iter()
            .filter(|row| row.state == state)
            .map(|row| row.id.clone())
            .collect()
    }

    /// Every row, one per identifier, in ascending identifier order,
    /// without undo rows.
    pub(crate) fn rows(&self) -> Vec<&SegmentLocation> {
        let mut merged = Vec::with_capacity(self.snapshot.len() + self.overlay.len());
        let mut snapshot = self.snapshot.iter().peekable();
        for row in self.overlay.values() {
            while let Some(earlier) = snapshot.next_if(|earlier| earlier.id < row.id) {
                merged.push(earlier);
            }
            snapshot.next_if(|same| same.id.cmp(&row.id) == Ordering::Equal);
            merged.push(row);
        }
        merged.extend(snapshot);
        merged.retain(|row| !Self::is_undo(row));
        merged
    }

    /// Bytes of segment `number`'s records that some row still refers to:
    /// each referenced record once, however many entries of it are live.
    pub(crate) fn live_bytes(&self, number: u64) -> u64 {
        let records: BTreeMap<u64, u32> = self
            .rows()
            .into_iter()
            .filter(|row| row.segment == number)
            .map(|row| (row.offset, row.length))
            .collect();
        records.values().map(|length| u64::from(*length)).sum()
    }

    /// The table as snapshot bytes: every row, sorted.
    pub(crate) fn encode_snapshot(&self) -> Result<Vec<u8>, NativeStorageError> {
        let rows = self.rows();
        let mut bytes = Vec::with_capacity(rows.len() * SegmentLocation::ENCODED_BYTES);
        for row in rows {
            bytes.extend_from_slice(&row.encode()?);
        }
        Ok(bytes)
    }

    fn is_undo(row: &SegmentLocation) -> bool {
        row.state == LocationState::Absent && row.lsn == 0
    }
}

#[cfg(test)]
mod tests {
    use super::{LocationState, LocationTable, SegmentLocation};

    fn row(id: &str, state: LocationState, lsn: u64) -> SegmentLocation {
        SegmentLocation {
            id: id.into(),
            segment: 1,
            offset: lsn * 100,
            length: 90,
            entry: 0,
            state,
            lsn,
        }
    }

    #[test]
    fn the_last_row_for_an_identifier_wins() {
        let table = LocationTable::from_rows([
            row("A", LocationState::Live, 1),
            row("B", LocationState::Live, 2),
            row("A", LocationState::Deleted, 3),
        ]);
        assert_eq!(table.ids(LocationState::Live), vec!["B".to_owned()]);
        assert_eq!(table.ids(LocationState::Deleted), vec!["A".to_owned()]);
    }

    #[test]
    fn an_undo_row_hides_its_identifier() {
        let table = LocationTable::from_rows([
            row("A", LocationState::Live, 1),
            row("A", LocationState::Absent, 0),
        ]);
        assert!(table.get("A").is_none());
        assert!(table.rows().is_empty());
    }

    #[test]
    fn rows_merge_the_snapshot_and_the_overlay_in_order() {
        let snapshot = LocationTable::from_rows([
            row("A", LocationState::Live, 1),
            row("C", LocationState::Live, 2),
            row("E", LocationState::Live, 3),
        ]);
        let bytes = snapshot.encode_snapshot().unwrap();
        let mut table = LocationTable {
            snapshot: std::sync::Arc::new(SegmentLocation::decode_all(&bytes).unwrap()),
            ..LocationTable::default()
        };
        table.apply(row("B", LocationState::Live, 4));
        table.apply(row("C", LocationState::Deleted, 5));
        let ids: Vec<(&str, LocationState)> = table
            .rows()
            .into_iter()
            .map(|row| (row.id.as_str(), row.state))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("A", LocationState::Live),
                ("B", LocationState::Live),
                ("C", LocationState::Deleted),
                ("E", LocationState::Live),
            ]
        );
        assert_eq!(table.get("C").unwrap().lsn, 5);
        assert_eq!(table.get("E").unwrap().lsn, 3);
    }
}
