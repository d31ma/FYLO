use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use super::cache_budget::CacheBudget;
use super::cache_location::CacheLocation;
use super::cache_sweep::CacheSweep;
use super::cache_usage::CacheUsage;
use super::private_folder::PrivateFolder;
use super::sweep_report::SweepReport;
use super::temp_cache_mode::TempCacheMode;
use super::temp_cache_reason::TempCacheReason;
use super::temp_cache_root::TempCacheRoot;
use super::temp_cache_settings::TempCacheSettings;
use super::temp_cache_state::TempCacheState;
use super::temp_cache_status::TempCacheStatus;
use super::temp_cache_tier::TempCacheTier;
use super::volume_facts::VolumeFacts;
use crate::RootFilesystem;

/// The temp-disk read tier's folder manager (ADR 0030 section 4).
///
/// A session asks it once, when it opens its root, whether the tier may be
/// active. In `auto` mode it is only for a root opened with the EFS profile
/// whose cache folder is private, on a local filesystem, on another device
/// and with at least 64 MiB free; `on` drops the profile and device rules.
pub struct TempCache;

impl TempCache {
    const UNKNOWN_FILESYSTEM: &'static str = "unknown";

    /// The temp tier's status for a session that opened `root`, or `None`
    /// when `settings` turn it off. When the rules pass, the cache folder is
    /// created and, at most once per 30 seconds between processes, swept.
    #[must_use]
    pub fn assess(
        settings: &TempCacheSettings,
        root: &TempCacheRoot,
        now: SystemTime,
    ) -> Option<TempCacheStatus> {
        Self::start(settings, root, now).status()
    }

    /// The temp tier of a session that opened `root`: off, inactive with a
    /// reason, or active. When the rules pass, the cache folder is created
    /// and, at most once per 30 seconds between processes, swept.
    #[must_use]
    pub fn start(
        settings: &TempCacheSettings,
        root: &TempCacheRoot,
        now: SystemTime,
    ) -> TempCacheState {
        if settings.mode == TempCacheMode::Off {
            return TempCacheState::Off;
        }
        Self::start_on(settings, root, now)
    }

    /// The current user's cache folder below `base`, or below the OS temp
    /// folder.
    #[must_use]
    pub fn folder(base: Option<&Path>) -> PathBuf {
        CacheLocation::folder(base)
    }

    /// What the current user's cache folder holds. Nothing is created.
    ///
    /// # Errors
    /// `PermissionDenied` for a folder that is not private (see
    /// [`PrivateFolder`]); the OS error of listing it.
    pub fn usage(base: Option<&Path>) -> io::Result<CacheUsage> {
        let path = CacheLocation::folder(base);
        let folder = PrivateFolder::open_existing(&path)?;
        folder.map_or_else(
            || Ok(CacheUsage::missing(&path)),
            |folder| CacheUsage::of(&folder),
        )
    }

    /// Delete the current user's cache folder, and return it; `None` when
    /// there was none. A folder that is not private is left alone.
    ///
    /// # Errors
    /// `PermissionDenied` for a folder that is not private; the OS error of
    /// removing it.
    pub fn clear(base: Option<&Path>) -> io::Result<Option<PathBuf>> {
        let path = CacheLocation::folder(base);
        let folder = PrivateFolder::open_existing(&path)?;
        folder.map(Self::remove).transpose()
    }

    fn remove(folder: PrivateFolder) -> io::Result<PathBuf> {
        let path = folder.path().to_path_buf();
        drop(folder);
        fs::remove_dir_all(&path)?;
        Ok(path)
    }

    fn start_on(
        settings: &TempCacheSettings,
        root: &TempCacheRoot,
        now: SystemTime,
    ) -> TempCacheState {
        let folder = CacheLocation::folder(settings.dir.as_deref());
        Self::evaluate(settings, root, &folder, now).unwrap_or_else(|reason| {
            TempCacheState::Inactive(TempCacheStatus::inactive(
                settings.mode,
                folder.display().to_string(),
                CacheBudget::wanted(settings.bytes),
                reason,
            ))
        })
    }

    fn evaluate(
        settings: &TempCacheSettings,
        root: &TempCacheRoot,
        folder: &Path,
        now: SystemTime,
    ) -> Result<TempCacheState, TempCacheReason> {
        let root_path = Self::admit_root(settings.mode, root)?;
        let base = CacheLocation::base(settings.dir.as_deref());
        Self::require_local(&base)?;
        let free = Self::require_space(&base)?;
        Self::require_other_device(settings.mode, &base, root_path)?;
        let private =
            PrivateFolder::open_or_create(folder).map_err(|_| TempCacheReason::UnsafeFolder)?;
        let budget = CacheBudget::cap(settings.bytes, free);
        let sweep = Self::sweep(&private, budget, now);
        let tier = TempCacheTier::new(private, settings.mode, budget, sweep);
        Ok(TempCacheState::Active(Arc::new(tier)))
    }

    fn admit_root(mode: TempCacheMode, root: &TempCacheRoot) -> Result<&Path, TempCacheReason> {
        let path = root.path().ok_or(TempCacheReason::NoRoot)?;
        Self::refuse_database(root)?;
        Self::refuse_local_root(mode, root)?;
        Ok(path)
    }

    fn refuse_database(root: &TempCacheRoot) -> Result<(), TempCacheReason> {
        if root.is_database() {
            return Err(TempCacheReason::Vdbx);
        }
        Ok(())
    }

    fn refuse_local_root(mode: TempCacheMode, root: &TempCacheRoot) -> Result<(), TempCacheReason> {
        if mode == TempCacheMode::Auto && !root.is_remote_or_sidecar() {
            return Err(TempCacheReason::RootIsLocal);
        }
        Ok(())
    }

    fn require_local(base: &Path) -> Result<(), TempCacheReason> {
        let filesystem =
            RootFilesystem::of(base).map_err(|_| TempCacheReason::CacheFolderRemote)?;
        if filesystem.remote() || filesystem.name() == Self::UNKNOWN_FILESYSTEM {
            return Err(TempCacheReason::CacheFolderRemote);
        }
        Ok(())
    }

    fn require_space(base: &Path) -> Result<u64, TempCacheReason> {
        let free = VolumeFacts::free_bytes(base).map_err(|_| TempCacheReason::UnsafeFolder)?;
        if CacheBudget::is_low(free) {
            return Err(TempCacheReason::LowSpace);
        }
        Ok(free)
    }

    fn require_other_device(
        mode: TempCacheMode,
        base: &Path,
        root: &Path,
    ) -> Result<(), TempCacheReason> {
        if mode == TempCacheMode::On {
            return Ok(());
        }
        Self::refuse_same_device(base, root)
    }

    fn refuse_same_device(base: &Path, root: &Path) -> Result<(), TempCacheReason> {
        let cache = VolumeFacts::device(base).map_err(|_| TempCacheReason::UnsafeFolder)?;
        let root = VolumeFacts::device(root).map_err(|_| TempCacheReason::SameDevice)?;
        if cache == root {
            return Err(TempCacheReason::SameDevice);
        }
        Ok(())
    }

    fn sweep(folder: &PrivateFolder, budget: u64, now: SystemTime) -> SweepReport {
        let sweep = CacheSweep::new(budget, now);
        if sweep.is_due(folder) {
            return sweep.run(folder).unwrap_or_default();
        }
        CacheSweep::last(folder).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
