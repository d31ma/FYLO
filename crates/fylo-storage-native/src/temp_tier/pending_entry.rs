use super::document_entry::DocumentEntry;
use super::entry_key::EntryKey;

/// An entry read during a stable-read attempt, waiting for the attempt's
/// closing check before it is written (ADR 0030 section 4.4).
#[derive(Clone, Debug)]
pub(crate) struct PendingEntry {
    pub(crate) key: EntryKey,
    pub(crate) entry: DocumentEntry,
}
