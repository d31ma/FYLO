use std::fs::File;
use std::path::Path;

use super::attribute_test::AttributeTest;
use super::probe_files::ProbeFiles;

/// Measures the largest single extended attribute value a new file can
/// hold, up to [`Self::ROOM`], by a binary search of about seventeen writes.
/// Plain ext4 holds about 4 KiB (one block for all of a file's attributes);
/// APFS, XFS, ext4 with `ea_inode` and NTFS streams hold all of it.
pub(crate) struct AttributeLimit;

impl AttributeLimit {
    const NAMES_AND_OTHER_ATTRIBUTES: u32 = 4 * 1024;
    /// The room one record's attributes need: FYLO's largest metadata value
    /// plus its name and the record's other attributes (key, checksum,
    /// access). A file system with less keeps a whole app folder's metadata
    /// in sidecar files (ADR 0033 section 6, FYLO-L1-4b).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_META_VALUE_BYTES is 60 KiB"
    )]
    pub(crate) const ROOM: u32 =
        crate::MAX_META_VALUE_BYTES as u32 + Self::NAMES_AND_OTHER_ATTRIBUTES;
    const CEILING: u32 = Self::ROOM;

    pub(crate) fn measure(files: &mut ProbeFiles) -> std::io::Result<u32> {
        let (path, file) = files.create_new()?;
        Self::search(&file, &path)
    }

    /// Whether a new file still holds [`Self::ROOM`] bytes in one attribute:
    /// one write, for the check at every writing open.
    pub(crate) fn holds_room(files: &mut ProbeFiles) -> std::io::Result<bool> {
        let (path, file) = files.create_new()?;
        Self::stores(&file, &path, Self::ROOM)
    }

    fn search(file: &File, path: &Path) -> std::io::Result<u32> {
        let mut fits = 0;
        let mut fails = Self::CEILING + 1;
        while fails - fits > 1 {
            (fits, fails) = Self::narrow(file, path, fits, fails)?;
        }
        Ok(fits)
    }

    fn narrow(file: &File, path: &Path, fits: u32, fails: u32) -> std::io::Result<(u32, u32)> {
        let middle = fits + (fails - fits) / 2;
        let stored = Self::stores(file, path, middle)?;
        Ok(Self::next_range(stored, fits, fails, middle))
    }

    const fn next_range(stored: bool, fits: u32, fails: u32, middle: u32) -> (u32, u32) {
        if stored {
            return (middle, fails);
        }
        (fits, middle)
    }

    fn stores(file: &File, path: &Path, size: u32) -> std::io::Result<bool> {
        let value = vec![b'x'; size as usize];
        match AttributeTest::set(file, path, &value) {
            Ok(()) => Ok(true),
            Err(error) if Self::too_large(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn too_large(error: &std::io::Error) -> bool {
        byckr_xattr::ValueTooLarge::matches(error)
    }

    #[cfg(not(unix))]
    fn too_large(error: &std::io::Error) -> bool {
        error.kind() == std::io::ErrorKind::StorageFull
    }
}

#[cfg(test)]
mod tests {
    use super::AttributeLimit;

    #[test]
    fn a_stored_middle_raises_the_lower_bound() {
        assert_eq!(AttributeLimit::next_range(true, 0, 100, 50), (50, 100));
    }

    #[test]
    fn a_refused_middle_lowers_the_upper_bound() {
        assert_eq!(AttributeLimit::next_range(false, 0, 100, 50), (0, 50));
    }
}
