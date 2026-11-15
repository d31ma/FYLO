use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::cache_file::CacheFile;

/// Every file and folder below a temp cache folder, found without following
/// a link: a listed link is a file, never a folder to enter.
///
/// The layout is at most three folders deep (root, collection, id prefix,
/// ADR 0030 section 4.3); anything deeper is not FYLO's and is not listed.
#[derive(Debug, Default)]
pub(crate) struct CacheListing {
    pub(crate) files: Vec<CacheFile>,
    pub(crate) folders: Vec<PathBuf>,
}

impl CacheListing {
    const MAXIMUM_DEPTH: usize = 3;

    /// List `folder`. A file or folder that disappears while it is listed,
    /// because another process swept it, is skipped.
    pub(crate) fn of(folder: &Path) -> io::Result<Self> {
        let mut listing = Self::default();
        let mut pending = vec![(folder.to_path_buf(), 0)];
        while let Some((path, depth)) = pending.pop() {
            listing.read_folder(&path, depth, &mut pending)?;
        }
        Ok(listing)
    }

    /// Remove each listed folder that is empty, deepest first. A folder
    /// another process filled again stays.
    pub(crate) fn remove_empty_folders(&self) {
        let mut folders = self.folders.clone();
        folders.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for folder in folders {
            let _ = fs::remove_dir(folder);
        }
    }

    fn read_folder(
        &mut self,
        path: &Path,
        depth: usize,
        pending: &mut Vec<(PathBuf, usize)>,
    ) -> io::Result<()> {
        for entry in Self::gone_is_empty(fs::read_dir(path))? {
            self.add(&entry?, depth, pending)?;
        }
        Ok(())
    }

    fn gone_is_empty(
        listing: io::Result<fs::ReadDir>,
    ) -> io::Result<Vec<io::Result<fs::DirEntry>>> {
        match listing {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            other => other.map(Iterator::collect),
        }
    }

    fn add(
        &mut self,
        entry: &fs::DirEntry,
        depth: usize,
        pending: &mut Vec<(PathBuf, usize)>,
    ) -> io::Result<()> {
        if entry.file_type()?.is_dir() {
            self.add_folder(entry.path(), depth + 1, pending);
            return Ok(());
        }
        self.add_file(entry)
    }

    fn add_folder(&mut self, path: PathBuf, depth: usize, pending: &mut Vec<(PathBuf, usize)>) {
        if depth > Self::MAXIMUM_DEPTH {
            return;
        }
        self.folders.push(path.clone());
        pending.push((path, depth));
    }

    fn add_file(&mut self, entry: &fs::DirEntry) -> io::Result<()> {
        match CacheFile::of(entry) {
            Ok(file) => self.files.push(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }
}
