use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{LocationState, SegmentLocation, SegmentMedium};
use crate::NativeStorageError;

/// Every document's newest location: the sorted snapshot with the location
/// log's later rows laid over it.
///
/// The log is applied in file order and the last row for an identifier
/// wins. Both files are derived and rebuild from segments. A row in the
/// `absent` state with sequence number zero only undoes a rolled-back
/// commit's row, and folding drops it.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocationTable {
    snapshot: Arc<Vec<SegmentLocation>>,
    overlay: BTreeMap<String, SegmentLocation>,
    wal_length: u64,
}

impl LocationTable {
    /// Load the snapshot and the first `wal_length` bytes of the log.
    pub(crate) fn load(
        medium: &dyn SegmentMedium,
        wal_length: u64,
    ) -> Result<Self, NativeStorageError> {
        let snapshot = SegmentLocation::decode_all(&medium.read_location_snapshot()?)?;
        let mut table = Self {
            snapshot: Arc::new(snapshot),
            overlay: BTreeMap::new(),
            wal_length: 0,
        };
        table.extend(medium, wal_length)?;
        Ok(table)
    }

    /// Apply the log's rows between the length already applied and
    /// `wal_length`.
    pub(crate) fn extend(
        &mut self,
        medium: &dyn SegmentMedium,
        wal_length: u64,
    ) -> Result<(), NativeStorageError> {
        if wal_length <= self.wal_length {
            return Ok(());
        }
        let tail = medium.read_location_log(self.wal_length, wal_length)?;
        for row in SegmentLocation::decode_all(&tail)? {
            self.apply(row);
        }
        self.wal_length = wal_length;
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

    /// Bytes of the location log this table has applied.
    pub(crate) fn wal_length(&self) -> u64 {
        self.wal_length
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
