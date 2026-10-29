//! The files macOS writes beside FYLO's own, in FYLO's own directories.
//!
//! A volume with no extended attributes of its own — exFAT, FAT, many SMB and
//! NFS shares — gets them from macOS as an `AppleDouble` file named `._` plus
//! the owner's name, beside every file or directory that carries one; and
//! Finder leaves a `.DS_Store` in any directory it has shown. FYLO lists its
//! own directories and refuses what it did not write, so on such a volume
//! every record with an attribute read as corruption.
//!
//! No name FYLO writes in its own directories begins with `._` or is
//! `.DS_Store`: identifiers and shards are alphanumeric, collection names
//! begin with a letter or digit, no segment of a branch name may take either
//! form, and the segment, index, lock and log files all begin otherwise. Those
//! names alone therefore mark a companion, and skipping one cannot hide an
//! entry FYLO owns — a damaged record keeps its own name and is still refused.
//! Beside a database, whose name is the user's, FYLO finds its files by exact
//! name and needs no rule.

use fylo_vfs as fs;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// Whether a directory entry named `name` is a platform companion rather than
/// an entry FYLO wrote.
pub fn is_platform_companion(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    name.starts_with(b"._") || name == FINDER_STATE
}

/// Finder's view state for the directory that holds it.
const FINDER_STATE: &[u8] = b".DS_Store";

/// `read_dir` of a directory FYLO owns, without the platform companions.
pub(crate) fn read_dir_without_companions(
    path: impl AsRef<Path>,
) -> io::Result<impl Iterator<Item = io::Result<fs::DirEntry>>> {
    Ok(fs::read_dir(path)?.filter(|entry| {
        entry
            .as_ref()
            .map_or(true, |entry| !is_platform_companion(&entry.file_name()))
    }))
}

/// Remove the companion of `path`, which FYLO has just removed, if it has one.
///
/// macOS removes a companion with its owner on a volume it mounted itself, but
/// another host, or a share served by one, leaves it behind as an orphan, and
/// an orphan keeps its directory from being removed. This is best effort: an
/// orphan left behind is skipped like any other companion, so it is no reason
/// to fail a removal that has already happened.
pub(crate) fn remove_companion(path: &Path) {
    if let Some(companion) = companion_of(path) {
        let _ = fs::remove_file(companion);
    }
}

/// Remove a directory FYLO emptied, and its companion.
///
/// `remove_dir` refuses a directory still holding an orphaned companion, so on
/// a refusal the orphans — and Finder's state, whose owner is the directory
/// itself — are cleared and the removal tried once more. A companion whose
/// owner is still there is left alone, and the directory with it, which keeps
/// the refusal a caller relies on to leave a directory that still holds a
/// record.
pub(crate) fn remove_emptied_dir(path: &Path) -> io::Result<()> {
    if let Err(error) = fs::remove_dir(path) {
        if !remove_orphaned_companions(path).unwrap_or(false) {
            return Err(error);
        }
        fs::remove_dir(path)?;
    }
    remove_companion(path);
    Ok(())
}

/// Remove every companion in `directory` whose owner is gone, reporting
/// whether there was one.
fn remove_orphaned_companions(directory: &Path) -> io::Result<bool> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(false);
    };
    let mut removed = false;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !is_platform_companion(&name) || !entry.file_type()?.is_file() {
            continue;
        }
        // Every name FYLO writes is UTF-8, so a `._` name that is not owns
        // nothing of FYLO's and is left alone, as a bare `._` is.
        let owner = name.to_str().and_then(|name| name.strip_prefix("._"));
        if let Some(owner) = owner.filter(|owner| !owner.is_empty()) {
            match fs::symlink_metadata(directory.join(owner)) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        } else if name.as_encoded_bytes() != FINDER_STATE {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => removed = true,
        }
    }
    Ok(removed)
}

fn companion_of(path: &Path) -> Option<PathBuf> {
    let mut name = OsString::from("._");
    name.push(path.file_name()?);
    Some(path.with_file_name(name))
}

#[cfg(test)]
mod tests;
