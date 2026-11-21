use super::TempCache;
use crate::FilesystemProfile;
use crate::temp_tier::cache_budget::CacheBudget;
use crate::temp_tier::temp_cache_mode::TempCacheMode;
use crate::temp_tier::temp_cache_root::TempCacheRoot;
use crate::temp_tier::temp_cache_settings::TempCacheSettings;
use crate::temp_tier::temp_cache_status::TempCacheStatus;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

struct Place(PathBuf);

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-temp-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("root")).unwrap();
        fs::create_dir_all(path.join("base")).unwrap();
        Self(path)
    }

    fn settings(&self, mode: TempCacheMode) -> TempCacheSettings {
        TempCacheSettings {
            mode,
            bytes: None,
            dir: Some(self.base()),
        }
    }

    fn base(&self) -> PathBuf {
        self.0.join("base")
    }

    fn root(&self, profile: FilesystemProfile) -> TempCacheRoot {
        TempCacheRoot::new(Some(&self.0.join("root")), profile)
    }

    fn cache(&self) -> PathBuf {
        TempCache::folder(Some(&self.base()))
    }

    fn assess(&self, mode: TempCacheMode, root: &TempCacheRoot) -> Option<TempCacheStatus> {
        TempCache::assess(&self.settings(mode), root, SystemTime::now())
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reason(status: &TempCacheStatus) -> &'static str {
    assert!(!status.active);
    status.reason.unwrap()
}

#[test]
fn off_reports_nothing_and_creates_nothing() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::Native);
    assert_eq!(place.assess(TempCacheMode::Off, &root), None);
    assert_eq!(fs::read_dir(place.base()).unwrap().count(), 0);
}

#[test]
fn auto_reports_a_local_root_and_creates_nothing() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::Native);
    let status = place.assess(TempCacheMode::Auto, &root).unwrap();
    assert_eq!(reason(&status), "root is local");
    assert_eq!(status.mode, "auto");
    assert_eq!(status.dir, place.cache().display().to_string());
    assert_eq!(status.budget_bytes, CacheBudget::DEFAULT_BYTES);
    assert!(!place.cache().exists());
}

#[test]
fn a_session_without_a_root_reports_it() {
    let place = Place::new();
    let root = TempCacheRoot::new(None, FilesystemProfile::Native);
    let status = place.assess(TempCacheMode::On, &root).unwrap();
    assert_eq!(reason(&status), "no root");
}

#[test]
fn a_vdbx_root_never_uses_the_tier() {
    let place = Place::new();
    let root = TempCacheRoot::new(Some(&place.0.join("app.vdbx")), FilesystemProfile::Native);
    let status = place.assess(TempCacheMode::On, &root).unwrap();
    assert_eq!(reason(&status), "vdbx");
    assert!(!place.cache().exists());
}

#[test]
fn auto_refuses_a_cache_folder_on_the_roots_device() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::EfsSidecarV1);
    let status = place.assess(TempCacheMode::Auto, &root).unwrap();
    assert_eq!(reason(&status), "same device");
    assert!(!place.cache().exists());
}

#[test]
fn on_creates_a_private_folder_and_activates_a_native_root() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::Native);
    let status = place.assess(TempCacheMode::On, &root).unwrap();
    assert!(status.active, "{status:?}");
    assert_eq!(status.reason, None);
    assert_eq!(status.mode, "on");
    assert!(status.budget_bytes <= CacheBudget::DEFAULT_BYTES);
    assert!(status.budget_bytes > 0);
    assert!(place.cache().join("sweep.stamp").is_file());
}

#[test]
fn on_activates_an_efs_profile_root() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::EfsSidecarV1);
    let status = place.assess(TempCacheMode::On, &root).unwrap();
    assert!(status.active, "{status:?}");
    assert_eq!(status.reason, None);
    assert!(place.cache().join("sweep.stamp").is_file());
}

#[test]
fn start_hands_out_the_tier_only_when_it_is_active() {
    let place = Place::new();
    let native = place.root(FilesystemProfile::Native);
    let settings = place.settings(TempCacheMode::On);
    assert!(
        TempCache::start(&settings, &native, SystemTime::now())
            .tier()
            .is_some()
    );
    let efs = place.root(FilesystemProfile::EfsSidecarV1);
    assert!(
        TempCache::start(&settings, &efs, SystemTime::now())
            .tier()
            .is_some()
    );
    let auto = place.settings(TempCacheMode::Auto);
    assert!(
        TempCache::start(&auto, &native, SystemTime::now())
            .tier()
            .is_none()
    );
    let off = place.settings(TempCacheMode::Off);
    assert!(
        TempCache::start(&off, &native, SystemTime::now())
            .tier()
            .is_none()
    );
}

#[test]
fn a_requested_budget_is_reported() {
    let place = Place::new();
    let settings = TempCacheSettings {
        bytes: Some(4096),
        ..place.settings(TempCacheMode::On)
    };
    let root = place.root(FilesystemProfile::Native);
    let status = TempCache::assess(&settings, &root, SystemTime::now()).unwrap();
    assert_eq!(status.budget_bytes, 4096);
}

#[test]
fn the_sweep_totals_reach_the_status() {
    let place = Place::new();
    let root = place.root(FilesystemProfile::Native);
    place.assess(TempCacheMode::On, &root).unwrap();
    fs::write(place.cache().join("entry.fyc"), [0_u8; 100]).unwrap();
    fs::remove_file(place.cache().join("sweep.stamp")).unwrap();
    let status = place.assess(TempCacheMode::On, &root).unwrap();
    assert_eq!((status.used_bytes, status.entries), (100, 1));
}

#[test]
fn usage_counts_entries_and_partial_files() {
    let place = Place::new();
    assert!(!TempCache::usage(Some(&place.base())).unwrap().exists);
    place.assess(TempCacheMode::On, &place.root(FilesystemProfile::Native));
    fs::write(place.cache().join("a.fyc"), [0_u8; 30]).unwrap();
    fs::write(place.cache().join("b.fyc"), [0_u8; 20]).unwrap();
    fs::write(place.cache().join("c.fyc.partial-1-x"), [0_u8; 5]).unwrap();
    let usage = TempCache::usage(Some(&place.base())).unwrap();
    assert!(usage.exists);
    assert_eq!(
        (usage.used_bytes, usage.entries, usage.partial_files),
        (50, 2, 1)
    );
    assert!(usage.last_sweep.is_some());
}

#[test]
fn clear_removes_the_folder_and_reports_it() {
    let place = Place::new();
    assert_eq!(TempCache::clear(Some(&place.base())).unwrap(), None);
    place.assess(TempCacheMode::On, &place.root(FilesystemProfile::Native));
    fs::write(place.cache().join("a.fyc"), [0_u8; 30]).unwrap();
    assert_eq!(
        TempCache::clear(Some(&place.base())).unwrap(),
        Some(place.cache())
    );
    assert!(!place.cache().exists());
}

#[test]
fn a_missing_base_folder_is_an_unsafe_folder() {
    let place = Place::new();
    let settings = TempCacheSettings {
        dir: Some(place.0.join("missing")),
        ..place.settings(TempCacheMode::On)
    };
    let root = place.root(FilesystemProfile::Native);
    let status = TempCache::assess(&settings, &root, SystemTime::now()).unwrap();
    assert_eq!(reason(&status), "unsafe folder");
}

#[cfg(unix)]
mod unix {
    use super::{Place, reason};
    use crate::FilesystemProfile;
    use crate::temp_tier::temp_cache::TempCache;
    use crate::temp_tier::temp_cache_mode::TempCacheMode;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_shared_folder_turns_the_tier_off_and_is_left_alone() {
        let place = Place::new();
        fs::create_dir(place.cache()).unwrap();
        fs::set_permissions(place.cache(), fs::Permissions::from_mode(0o755)).unwrap();
        let root = place.root(FilesystemProfile::Native);
        let status = place.assess(TempCacheMode::On, &root).unwrap();
        assert_eq!(reason(&status), "unsafe folder");
        assert!(TempCache::clear(Some(&place.base())).is_err());
        assert!(place.cache().is_dir());
    }
}

#[test]
fn a_shared_temp_folder_passes_the_local_filesystem_rule() {
    let place = Place::new();
    let settings = TempCacheSettings {
        dir: None,
        ..place.settings(TempCacheMode::On)
    };
    let root = place.root(FilesystemProfile::Native);
    let status = TempCache::assess(&settings, &root, SystemTime::now()).unwrap();
    assert!(status.active, "{status:?}");
    assert_eq!(status.reason, None);
    assert_ne!(Path::new(&status.dir).parent(), None);
}
