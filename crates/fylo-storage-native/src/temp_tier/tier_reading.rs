use super::entry_key::EntryKey;
use super::temp_cache_tier::TempCacheTier;

/// One document read's view of the temp tier: the tier, the digest of the
/// collection stamp the read runs under, and where the entry lives.
pub(crate) struct TierReading<'a> {
    pub(crate) tier: &'a TempCacheTier,
    pub(crate) stamp: [u8; 32],
    pub(crate) collection: String,
    pub(crate) key: EntryKey,
}
