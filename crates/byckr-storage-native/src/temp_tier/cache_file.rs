use std::fs::DirEntry;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use super::cache_file_kind::CacheFileKind;

/// One file in a temp cache folder, as a sweep sees it.
#[derive(Clone, Debug)]
pub(crate) struct CacheFile {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) modified: SystemTime,
    pub(crate) kind: CacheFileKind,
}

impl CacheFile {
    const ENTRY_SUFFIX: &'static str = ".fyc";
    const PARTIAL_MARK: &'static str = ".partial-";

    /// The file `entry` lists, read without following a link.
    pub(crate) fn of(entry: &DirEntry) -> io::Result<Self> {
        let metadata = entry.metadata()?;
        Ok(Self {
            path: entry.path(),
            bytes: metadata.len(),
            modified: metadata.modified()?,
            kind: Self::kind_of(&entry.file_name().to_string_lossy()),
        })
    }

    fn kind_of(name: &str) -> CacheFileKind {
        match name {
            _ if name.contains(Self::PARTIAL_MARK) => CacheFileKind::Partial,
            _ if name.ends_with(Self::ENTRY_SUFFIX) => CacheFileKind::Entry,
            _ => CacheFileKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CacheFile;
    use crate::temp_tier::cache_file_kind::CacheFileKind;

    #[test]
    fn names_tell_entries_partial_files_and_others_apart() {
        assert_eq!(CacheFile::kind_of("ab12.fyc"), CacheFileKind::Entry);
        assert_eq!(
            CacheFile::kind_of("ab12.fyc.partial-42-9f"),
            CacheFileKind::Partial
        );
        assert_eq!(CacheFile::kind_of("sweep.stamp"), CacheFileKind::Other);
    }
}
