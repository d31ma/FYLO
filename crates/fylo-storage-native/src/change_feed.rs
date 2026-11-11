use crate::segments::{LocationState, LocationTable, SegmentLocation, SegmentStore};
use crate::{
    ChangePage, CollectionKind, DocumentChange, NativeCollection, NativeStorageError,
    NativeStorageErrorCode,
};

/// Reads a collection's change feed from its location table: every
/// document whose newest change follows a position, once, in the order of
/// those changes (ADR 0024).
///
/// Changes are ordered by a text key, the sequence number in
/// [`Self::LSN_DIGITS`] digits then the identifier, so the browser engine
/// reuses the text sort it already holds instead of carrying a new one.
pub(crate) struct ChangeFeed;

impl ChangeFeed {
    const LSN_DIGITS: usize = 20;

    pub(crate) fn page(
        collection: &NativeCollection,
        after: (u64, &str),
        limit: usize,
    ) -> Result<ChangePage, NativeStorageError> {
        Self::require_segments(collection)?;
        let medium = collection.segment_medium()?;
        let next_lsn = medium.require_slot()?.next_lsn;
        let store = SegmentStore::open(medium)?;
        let table = store.table();
        let mut keys = Self::keys_after(table.rows(), after);
        keys.sort_unstable();
        let more = keys.len() > limit;
        keys.truncate(limit);
        Ok(ChangePage {
            changes: Self::changes(table, &keys),
            more,
            next_lsn,
            lineage: collection.change_lineage()?,
        })
    }

    fn require_segments(collection: &NativeCollection) -> Result<(), NativeStorageError> {
        if collection.kind() == CollectionKind::Document && collection.uses_segments() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::LayoutUnsupported,
            format!(
                "the change feed reads collections in the segments layout or in a .vdbx \
                 database; run `fylo layout {} --to segments` first",
                collection.name()
            ),
        ))
    }

    fn keys_after(rows: Vec<&SegmentLocation>, after: (u64, &str)) -> Vec<String> {
        let mut keys = Vec::new();
        for row in rows {
            if (row.lsn, row.id.as_str()) > after {
                keys.push(format!(
                    "{:0width$}{}",
                    row.lsn,
                    row.id,
                    width = Self::LSN_DIGITS
                ));
            }
        }
        keys
    }

    fn changes(table: &LocationTable, keys: &[String]) -> Vec<DocumentChange> {
        let mut changes = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(row) = table.get(&key[Self::LSN_DIGITS..]) {
                changes.push(Self::change(row));
            }
        }
        changes
    }

    fn change(row: &SegmentLocation) -> DocumentChange {
        DocumentChange {
            id: row.id.clone(),
            deleted: row.state != LocationState::Live,
            lsn: row.lsn,
        }
    }
}
