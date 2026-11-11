use crate::segments::{LocationState, SegmentLocation, SegmentStore};
use crate::{
    ChangePage, CollectionKind, DocumentChange, NativeCollection, NativeStorageError,
    NativeStorageErrorCode,
};

/// Reads a collection's change feed from its location table: every
/// document whose newest change follows a position, once, in the order of
/// those changes (ADR 0024).
pub(crate) struct ChangeFeed;

impl ChangeFeed {
    pub(crate) fn page(
        collection: &NativeCollection,
        after: (u64, &str),
        limit: usize,
    ) -> Result<ChangePage, NativeStorageError> {
        Self::require_segments(collection)?;
        let medium = collection.segment_medium()?;
        let next_lsn = medium.require_slot()?.next_lsn;
        let store = SegmentStore::open(medium)?;
        let mut rows = Self::rows_after(store.table().rows(), after);
        rows.sort_unstable_by(|left, right| (left.lsn, &left.id).cmp(&(right.lsn, &right.id)));
        let more = rows.len() > limit;
        rows.truncate(limit);
        Ok(ChangePage {
            changes: rows.into_iter().map(Self::change).collect(),
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

    fn rows_after<'a>(
        rows: Vec<&'a SegmentLocation>,
        after: (u64, &str),
    ) -> Vec<&'a SegmentLocation> {
        let mut later = Vec::new();
        for row in rows {
            if (row.lsn, row.id.as_str()) > after {
                later.push(row);
            }
        }
        later
    }

    fn change(row: &SegmentLocation) -> DocumentChange {
        DocumentChange {
            id: row.id.clone(),
            deleted: row.state != LocationState::Live,
            lsn: row.lsn,
        }
    }
}
