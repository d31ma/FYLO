use std::fs::File;
use std::path::Path;

use super::attribute_test::AttributeTest;
use super::probe_files::ProbeFiles;

/// Measures the largest single extended attribute value a new file can
/// hold, up to 64 KiB, by a binary search of about seventeen writes. Plain
/// ext4 holds about 4 KiB (one block for all of a file's attributes); APFS,
/// XFS and NTFS streams hold the whole 64 KiB.
pub(crate) struct AttributeLimit;

impl AttributeLimit {
    const CEILING: u32 = 64 * 1024;

    pub(crate) fn measure(files: &mut ProbeFiles) -> std::io::Result<u32> {
        let (path, file) = files.create_new()?;
        Self::search(&file, &path)
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
        fylo_xattr::ValueTooLarge::matches(error)
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
