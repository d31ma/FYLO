use super::{
    is_platform_companion, read_dir_without_companions, remove_companion, remove_emptied_dir,
};
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(crate::write::unique_name(label));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn names(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn only_a_leading_dot_underscore_or_finder_state_marks_a_companion() {
    for name in [
        "._4VRNF540000.json",
        "._items",
        "._",
        "._.DS_Store",
        ".DS_Store",
    ] {
        assert!(is_platform_companion(OsStr::new(name)), "{name}");
    }
    for name in [
        "4VRNF540000.json",
        ".collections",
        ".",
        "_.x",
        ".deleted",
        "a._b",
        ".ds_store",
        ".DS_Store.json",
        "_.DS_Store",
    ] {
        assert!(!is_platform_companion(OsStr::new(name)), "{name}");
    }
}

#[test]
fn a_listing_leaves_out_only_the_companions() {
    let scratch = Scratch::new("companion-listing");
    for name in ["A", "._A", ".deleted", "._.deleted", "_x", ".DS_Store"] {
        fs::write(scratch.0.join(name), b"\x00\x05\x16\x07").unwrap();
    }
    let mut listed: Vec<String> = read_dir_without_companions(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    listed.sort();
    assert_eq!(listed, [".deleted", "A", "_x"]);
}

#[test]
fn a_removed_owner_takes_its_companion_and_nothing_else() {
    let scratch = Scratch::new("companion-remove");
    fs::write(scratch.0.join("._A"), b"\x00\x05\x16\x07").unwrap();
    fs::write(scratch.0.join("._B"), b"\x00\x05\x16\x07").unwrap();
    remove_companion(&scratch.0.join("A"));
    // Nothing to remove is nothing to do.
    remove_companion(&scratch.0.join("A"));
    assert_eq!(names(&scratch.0), ["._B"]);
}

#[test]
fn an_emptied_directory_goes_despite_orphans_but_not_with_a_live_companion() {
    let scratch = Scratch::new("companion-rmdir");
    let shard = scratch.0.join("A");
    fs::create_dir(&shard).unwrap();
    fs::write(scratch.0.join("._A"), b"\x00\x05\x16\x07").unwrap();
    fs::write(shard.join("._4VRNF540000.json"), b"\x00\x05\x16\x07").unwrap();
    fs::write(shard.join(".DS_Store"), b"Bud1").unwrap();
    remove_emptied_dir(&shard).unwrap();
    assert_eq!(names(&scratch.0), Vec::<String>::new());

    let held = scratch.0.join("B");
    fs::create_dir(&held).unwrap();
    fs::write(held.join("4VRNF540000.json"), b"{}").unwrap();
    fs::write(held.join("._4VRNF540000.json"), b"\x00\x05\x16\x07").unwrap();
    fs::write(held.join("._4VRNF540001.json"), b"\x00\x05\x16\x07").unwrap();
    assert!(remove_emptied_dir(&held).is_err());
    // The orphan is cleared on the way; the record and its companion stay.
    assert_eq!(names(&held), ["._4VRNF540000.json", "4VRNF540000.json"]);

    assert!(remove_emptied_dir(&scratch.0.join("missing")).is_err());
}
