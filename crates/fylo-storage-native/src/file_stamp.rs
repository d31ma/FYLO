use fylo_vfs::File;
use std::io;

/// What an opened file is right now: which file, how long, and when it last
/// changed (ADR 0030 sections 3.1 and 3.2).
///
/// A file replaced by rename is another file, and a file changed in place
/// has a later change time, so two equal stamps name the same unchanged
/// file. It is read from an open handle, never by path: on NFS opening a
/// file makes the client ask the server, while a plain `stat` may answer
/// from the client's attribute cache.
///
/// On Unix it holds the device, inode, size, and modification and change
/// times in nanoseconds. On Windows it holds a hash of the volume serial
/// number and file index, the size, and the last-write and creation times:
/// Windows offers no change time without unsafe code, so a change to a
/// file's attributes alone is not seen there. The temp tier's `auto` mode
/// never runs on Windows (the EFS profile has no Windows form).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileStamp {
    device: u64,
    file: u64,
    size: u64,
    modified_nanoseconds: i128,
    changed_nanoseconds: i128,
}

impl FileStamp {
    #[cfg(unix)]
    const NANOSECONDS_PER_SECOND: i128 = 1_000_000_000;

    /// The stamp of an open file.
    ///
    /// # Errors
    ///
    /// Returns the error of reading the handle's metadata, or `Unsupported`
    /// on a target without file identities (WebAssembly).
    pub fn of_file(file: &File) -> io::Result<Self> {
        Self::read(file)
    }

    #[cfg(not(target_family = "wasm"))]
    /// Add every part of the stamp to `hasher`, in a fixed order, so equal
    /// stamps give equal digests in every process.
    pub(crate) fn feed(&self, hasher: &mut sha2::Sha256) {
        use sha2::Digest;

        hasher.update(self.device.to_le_bytes());
        hasher.update(self.file.to_le_bytes());
        hasher.update(self.size.to_le_bytes());
        hasher.update(self.modified_nanoseconds.to_le_bytes());
        hasher.update(self.changed_nanoseconds.to_le_bytes());
    }

    #[cfg(not(target_family = "wasm"))]
    /// A SHA-256 digest of this stamp followed by `manifest`'s, the same in
    /// every process: a record's payload and, in the EFS profile, its
    /// manifest file (ADR 0030 section 3.2).
    pub(crate) fn digest_with(&self, manifest: Option<&Self>) -> [u8; 32] {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        self.feed(&mut hasher);
        Self::feed_manifest(manifest, &mut hasher);
        hasher.finalize().into()
    }

    #[cfg(not(target_family = "wasm"))]
    fn feed_manifest(manifest: Option<&Self>, hasher: &mut sha2::Sha256) {
        let Some(manifest) = manifest else {
            return;
        };
        manifest.feed(hasher);
    }

    #[cfg(unix)]
    fn read(file: &File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            file: metadata.ino(),
            size: metadata.size(),
            modified_nanoseconds: Self::nanoseconds(metadata.mtime(), metadata.mtime_nsec()),
            changed_nanoseconds: Self::nanoseconds(metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    #[cfg(windows)]
    fn read(file: &File) -> io::Result<Self> {
        use std::os::windows::fs::MetadataExt;
        let metadata = file.metadata()?;
        let handle = same_file::Handle::from_file(file.try_clone()?)?;
        Ok(Self {
            device: 0,
            file: Self::identity_hash(&handle),
            size: metadata.file_size(),
            modified_nanoseconds: i128::from(metadata.last_write_time()),
            changed_nanoseconds: i128::from(metadata.creation_time()),
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn read(_file: &File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this target has no file identities",
        ))
    }

    #[cfg(unix)]
    fn nanoseconds(seconds: i64, nanoseconds: i64) -> i128 {
        i128::from(seconds) * Self::NANOSECONDS_PER_SECOND + i128::from(nanoseconds)
    }

    #[cfg(windows)]
    fn identity_hash(handle: &same_file::Handle) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(handle, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::FileStamp;
    use std::io::Write;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(crate::write::unique_name(name))
    }

    #[test]
    fn an_unchanged_file_keeps_its_stamp() {
        let path = scratch("file-stamp-same");
        std::fs::write(&path, b"one").unwrap();
        let first = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        let second = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_file_replaced_by_rename_has_another_stamp() {
        let path = scratch("file-stamp-renamed");
        let replacement = scratch("file-stamp-replacement");
        std::fs::write(&path, b"one").unwrap();
        let before = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::write(&replacement, b"one").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let after = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn a_file_grown_in_place_has_another_stamp() {
        let path = scratch("file-stamp-grown");
        std::fs::write(&path, b"one").unwrap();
        let before = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"two").unwrap();
        drop(file);
        let after = FileStamp::of_file(&std::fs::File::open(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_ne!(before, after);
    }
}
