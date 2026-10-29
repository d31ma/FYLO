use std::path::{Path, PathBuf};

use fylo_vfs as fs;

use crate::NativeStorageError;

/// Where a segment collection keeps each of its files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SegmentPaths {
    directory: PathBuf,
}

impl SegmentPaths {
    const EXTENSION: &'static str = ".seg";
    const NUMBER_DIGITS: usize = 10;

    pub(crate) fn of(collection: &Path) -> Self {
        Self {
            directory: collection.join("segments"),
        }
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn segment(&self, number: u64) -> PathBuf {
        self.directory.join(format!(
            "{number:0width$}{}",
            Self::EXTENSION,
            width = Self::NUMBER_DIGITS
        ))
    }

    pub(crate) fn slots(&self) -> PathBuf {
        self.directory.join("state.slots")
    }

    pub(crate) fn locations_snapshot(&self) -> PathBuf {
        self.directory.join("locations.snapshot")
    }

    pub(crate) fn locations_wal(&self) -> PathBuf {
        self.directory.join("locations.wal")
    }

    /// The numbers of every segment file present, in ascending order.
    pub(crate) fn segment_numbers(&self) -> Result<Vec<u64>, NativeStorageError> {
        let mut numbers = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if let Some(number) = Self::number_of(&entry.file_name().to_string_lossy()) {
                numbers.push(number);
            }
        }
        numbers.sort_unstable();
        Ok(numbers)
    }

    fn number_of(file_name: &str) -> Option<u64> {
        let digits = file_name.strip_suffix(Self::EXTENSION)?;
        if digits.len() < Self::NUMBER_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
}
