#[cfg(not(target_family = "wasm"))]
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(not(target_family = "wasm"))]
use super::pending_entry::PendingEntry;
#[cfg(not(target_family = "wasm"))]
use crate::CollectionIncarnation;

/// What one collection handle saw of its collection for the temp tier
/// (ADR 0030 sections 3.1 and 4.4).
///
/// Every read of the collection's generation records the digest of the
/// collection stamp it read, or none when the collection is not cacheable
/// or the read failed. Document reads compare entries with that digest.
/// Entries read from the root wait here; the next generation read writes
/// those read under a stamp equal to the one it sees, and drops the rest,
/// so an entry reaches the disk only after the read passed its closing
/// check. A clone of a handle starts with an empty window.
#[derive(Debug, Default)]
pub(crate) struct ReadWindow {
    #[cfg(not(target_family = "wasm"))]
    state: Mutex<WindowState>,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Default)]
struct WindowState {
    stamp: Option<[u8; 32]>,
    incarnation: Option<CollectionIncarnation>,
    pending: Vec<PendingEntry>,
    pending_bytes: usize,
}

impl Clone for ReadWindow {
    fn clone(&self) -> Self {
        Self::default()
    }
}

#[cfg(not(target_family = "wasm"))]
impl ReadWindow {
    /// Record that the collection now has the stamp `digest` names, and
    /// return the waiting entries read under that same stamp. The others
    /// are dropped.
    pub(crate) fn observe(&self, digest: Option<[u8; 32]>) -> Vec<PendingEntry> {
        let mut state = self.lock();
        state.stamp = digest;
        state.pending_bytes = 0;
        let pending = std::mem::take(&mut state.pending);
        drop(state);
        Self::read_under(pending, digest)
    }

    /// The digest of the stamp the last generation read saw.
    pub(crate) fn stamp(&self) -> Option<[u8; 32]> {
        self.lock().stamp
    }

    /// The collection's incarnation, read once per handle with `read`.
    pub(crate) fn incarnation(
        &self,
        read: impl FnOnce() -> CollectionIncarnation,
    ) -> CollectionIncarnation {
        *self.lock().incarnation.get_or_insert_with(read)
    }

    /// Keep `pending` until the next generation read, unless the waiting
    /// entries would then hold more than `limit` payload bytes.
    pub(crate) fn defer(&self, pending: PendingEntry, limit: usize) {
        let mut state = self.lock();
        let bytes = state.pending_bytes + pending.entry.payload.len();
        if bytes > limit {
            return;
        }
        state.pending_bytes = bytes;
        state.pending.push(pending);
    }

    fn read_under(pending: Vec<PendingEntry>, digest: Option<[u8; 32]>) -> Vec<PendingEntry> {
        let Some(digest) = digest else {
            return Vec::new();
        };
        pending
            .into_iter()
            .filter(|pending| pending.entry.collection_stamp == digest)
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, WindowState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
