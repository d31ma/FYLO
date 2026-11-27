use std::path::PathBuf;

use super::temp_cache_mode::TempCacheMode;

/// The temp tier's settings for one process: `--temp-cache`,
/// `--temp-cache-bytes` and `--temp-cache-dir`, or `yckr.tempCache` in
/// `application.json` (ADR 0030 section 7). A request never changes them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TempCacheSettings {
    /// Whether the tier is used.
    pub mode: TempCacheMode,
    /// The byte budget; `None` is the default, 256 MiB. Either way it is
    /// capped at half the free space of the cache folder's filesystem.
    pub bytes: Option<u64>,
    /// The base folder that replaces the OS temp folder.
    pub dir: Option<PathBuf>,
}
