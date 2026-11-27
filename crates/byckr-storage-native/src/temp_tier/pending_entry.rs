use super::entry_key::EntryKey;

/// An entry read during a stable-read attempt, waiting for the attempt's
/// closing check before it is written (ADR 0030 section 4.4): where it
/// goes, the digest of the collection stamp it was read under, and its
/// encoded bytes.
#[derive(Clone, Debug)]
pub(crate) struct PendingEntry {
    pub(crate) key: EntryKey,
    pub(crate) stamp: [u8; 32],
    pub(crate) bytes: Vec<u8>,
}
