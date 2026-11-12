//! FYLO's attribute calls, on every system that runs them: through the
//! `xattr` crate on the Mac and Linux, and through rustix on the iOS
//! simulator (`bun run test:mobile ios`).

use std::fs::{self, File};
use std::path::PathBuf;

use fylo_xattr::{ExtendedAttributes, FileExt};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let directory = std::env::temp_dir().join(format!(
            "fylo-xattr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        Scratch(directory)
    }

    fn file(&self) -> PathBuf {
        let path = self.0.join("record.json");
        fs::write(&path, b"{}").unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fylo_names(names: impl Iterator<Item = std::ffi::OsString>) -> Vec<String> {
    let mut names: Vec<String> = names
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("user.fylo."))
        .collect();
    names.sort();
    names
}

#[test]
fn a_value_is_set_read_listed_and_removed_by_path() {
    let scratch = Scratch::new("path");
    let path = scratch.file();

    ExtendedAttributes::set(&path, "user.fylo.key", b"/notes/a.json").unwrap();
    ExtendedAttributes::set(&path, "user.fylo.meta.tag", b"\"draft\"").unwrap();

    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.key").unwrap(),
        Some(b"/notes/a.json".to_vec())
    );
    assert_eq!(
        fylo_names(ExtendedAttributes::list(&path).unwrap()),
        ["user.fylo.key", "user.fylo.meta.tag"]
    );
    ExtendedAttributes::remove(&path, "user.fylo.meta.tag").unwrap();
    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.meta.tag").unwrap(),
        None
    );
}

#[test]
fn an_empty_value_reads_back_empty_and_a_missing_one_as_none() {
    let scratch = Scratch::new("empty");
    let path = scratch.file();

    ExtendedAttributes::set(&path, "user.fylo.empty", b"").unwrap();

    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.empty").unwrap(),
        Some(Vec::new())
    );
    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.absent").unwrap(),
        None
    );
}

#[test]
fn a_value_larger_than_four_kibibytes_reads_back_whole() {
    let scratch = Scratch::new("large");
    let path = scratch.file();
    let value = vec![b'x'; 8 * 1024];

    ExtendedAttributes::set(&path, "user.fylo.meta.note", &value).unwrap();

    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.meta.note").unwrap(),
        Some(value)
    );
}

#[test]
fn an_open_file_reads_and_writes_its_own_attributes() {
    let scratch = Scratch::new("file");
    let path = scratch.file();
    let file = File::options().read(true).write(true).open(&path).unwrap();

    file.set_xattr("user.fylo.checksum", b"abc").unwrap();
    file.set_xattr("user.fylo.empty", b"").unwrap();

    assert_eq!(
        file.get_xattr("user.fylo.checksum").unwrap(),
        Some(b"abc".to_vec())
    );
    assert_eq!(file.get_xattr("user.fylo.empty").unwrap(), Some(Vec::new()));
    assert_eq!(
        fylo_names(file.list_xattr().unwrap()),
        ["user.fylo.checksum", "user.fylo.empty"]
    );
    file.remove_xattr("user.fylo.checksum").unwrap();
    assert_eq!(file.get_xattr("user.fylo.checksum").unwrap(), None);
    assert_eq!(
        ExtendedAttributes::get(&path, "user.fylo.empty").unwrap(),
        Some(Vec::new())
    );
}

#[cfg(unix)]
#[test]
fn a_path_through_a_symlink_does_not_read_the_target() {
    let scratch = Scratch::new("link");
    let target = scratch.file();
    let link = scratch.0.join("link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    ExtendedAttributes::set(&target, "user.fylo.key", b"/notes/a.json").unwrap();

    assert_eq!(
        ExtendedAttributes::get(&link, "user.fylo.key").unwrap(),
        None
    );
}
