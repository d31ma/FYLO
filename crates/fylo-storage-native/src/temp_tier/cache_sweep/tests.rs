use super::CacheSweep;
use crate::temp_tier::private_folder::PrivateFolder;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

struct Place {
    parent: PathBuf,
    folder: PrivateFolder,
    now: SystemTime,
}

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = fylo_test_support::TestFolder::path().join(format!(
            "fylo-cache-sweep-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&parent).unwrap();
        let folder = PrivateFolder::open_or_create(&parent.join("fylo-cache-v1")).unwrap();
        Self {
            parent,
            folder,
            now: SystemTime::now(),
        }
    }

    fn write(&self, relative: &str, bytes: usize, age: Duration) {
        let relative = Path::new(relative);
        if let Some(parent) = relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            self.folder.create_folders(parent).unwrap();
        }
        let file = self.folder.create_file(relative).unwrap();
        file.set_len(u64::try_from(bytes).unwrap()).unwrap();
        file.set_modified(self.now - age).unwrap();
    }

    fn exists(&self, relative: &str) -> bool {
        self.folder.path().join(relative).exists()
    }

    fn sweep(&self, budget: u64) -> super::SweepReport {
        CacheSweep::new(budget, self.now).run(&self.folder).unwrap()
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

#[test]
fn the_sweep_keeps_the_newest_entries_under_ninety_percent() {
    let place = Place::new();
    for index in 0..10_u64 {
        place.write(
            &format!("root/collection/0{index}/entry{index}.fyc"),
            100,
            Duration::from_secs(100 - index),
        );
    }
    let report = place.sweep(500);
    assert_eq!(report.entries, 4);
    assert_eq!(report.used_bytes, 400);
    assert_eq!(report.evicted, 6);
    for index in 0..6 {
        assert!(!place.exists(&format!("root/collection/0{index}/entry{index}.fyc")));
    }
    for index in 6..10 {
        assert!(place.exists(&format!("root/collection/0{index}/entry{index}.fyc")));
    }
}

#[test]
fn a_folder_under_its_target_loses_nothing() {
    let place = Place::new();
    place.write("a.fyc", 100, Duration::from_secs(5));
    place.write("b.fyc", 100, Duration::from_secs(4));
    let report = place.sweep(1000);
    assert_eq!(
        (report.entries, report.used_bytes, report.evicted),
        (2, 200, 0)
    );
}

#[test]
fn old_partial_files_go_and_new_ones_stay() {
    let place = Place::new();
    place.write(
        "old.fyc.partial-1-a",
        10,
        CacheSweep::PARTIAL_AGE + Duration::from_secs(1),
    );
    place.write("new.fyc.partial-2-b", 10, Duration::from_mins(1));
    let report = place.sweep(1000);
    assert_eq!(report.partials_removed, 1);
    assert!(!place.exists("old.fyc.partial-1-a"));
    assert!(place.exists("new.fyc.partial-2-b"));
}

#[test]
fn files_that_are_not_entries_are_left_alone() {
    let place = Place::new();
    place.write("notes.txt", 5000, Duration::from_secs(5000));
    let report = place.sweep(10);
    assert_eq!(report.entries, 0);
    assert!(place.exists("notes.txt"));
}

#[test]
fn folders_left_empty_are_removed() {
    let place = Place::new();
    place.write("root/collection/ab/entry.fyc", 100, Duration::from_secs(5));
    place.sweep(10);
    assert!(!place.exists("root"));
}

#[test]
fn a_new_folder_is_due_and_a_swept_one_waits_thirty_seconds() {
    let place = Place::new();
    assert!(CacheSweep::new(10, place.now).is_due(&place.folder));
    place.sweep(10);
    let swept = fs::metadata(place.folder.path().join("sweep.stamp"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(!CacheSweep::new(10, swept).is_due(&place.folder));
    let later = swept + CacheSweep::INTERVAL;
    assert!(CacheSweep::new(10, later).is_due(&place.folder));
}

#[test]
fn the_stamp_keeps_the_last_totals() {
    let place = Place::new();
    assert_eq!(CacheSweep::last(&place.folder), None);
    place.write("a.fyc", 100, Duration::from_secs(5));
    let report = place.sweep(1000);
    assert_eq!(CacheSweep::last(&place.folder), Some(report));
}

#[test]
fn a_wiped_folder_sweeps_as_empty() {
    let place = Place::new();
    fs::remove_dir_all(place.folder.path()).unwrap();
    assert!(CacheSweep::new(10, place.now).run(&place.folder).is_err());
    let again = PrivateFolder::open_or_create(place.folder.path()).unwrap();
    let report = CacheSweep::new(10, place.now).run(&again).unwrap();
    assert_eq!(report.entries, 0);
}
