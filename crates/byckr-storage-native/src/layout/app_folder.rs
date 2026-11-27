use std::io::ErrorKind;
use std::path::Path;

use byckr_vfs as fs;

use super::{FormatMarker, GuardFiles, ProbeRecord, RootLayout};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// Makes a new app folder, or opens one FYLO made (ADR 0033 sections 1 and
/// 5.4). FYLO never writes into a folder it did not make: a folder that is
/// not empty and has no `.fylo/format.json` is refused with `ENOTFYLOROOT`.
///
/// The steps are safe to repeat after a crash: the folder, its `.fylo`
/// folder, `format.json`, the guard files and `.fylo/trash`, in that order.
pub struct AppFolder;

impl AppFolder {
    /// Make the app folder of `layout`, or open it when FYLO already made it.
    /// The drive (the app folder's parent) must exist.
    ///
    /// For a new folder, `probe` tests the file system in `<app>/.fylo` (slice
    /// L1-4) and its result is recorded in `format.json` with `created_by`,
    /// the FYLO version. An existing folder keeps its recorded marker, and
    /// `probe` is not called.
    ///
    /// Returns the marker the folder holds.
    ///
    /// # Errors
    ///
    /// Returns `ENOTFYLOROOT` for a folder FYLO did not make,
    /// `ENATIVE_WRONG_TYPE` when a path FYLO needs is not of the expected
    /// type, `ENATIVE_METADATA` for a damaged `format.json` or a bad
    /// `created_by`, the probe's error when it fails, and `ENATIVE_IO` when a
    /// file cannot be made.
    pub fn prepare(
        layout: &RootLayout,
        created_by: &str,
        probe: impl FnOnce(&Path) -> Result<ProbeRecord, NativeStorageError>,
    ) -> Result<FormatMarker, NativeStorageError> {
        Self::make_folder(layout.app_folder())?;
        let recorded = FormatMarker::read(&layout.format_marker_path())?;
        Self::refuse_foreign_folder(layout, recorded.as_ref())?;
        Self::make_folder(&layout.app_state_folder())?;
        let recorded = Self::record_marker(layout, recorded, (created_by, probe))?;
        GuardFiles::write(layout.app_folder())?;
        Self::make_folder(&layout.trash_folder())?;
        Ok(recorded)
    }

    fn make_folder(path: &Path) -> Result<(), NativeStorageError> {
        match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Self::require_folder(path),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn require_folder(path: &Path) -> Result<(), NativeStorageError> {
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        if !metadata.is_dir() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::WrongType,
                format!("{} is not a folder", path.display()),
            ));
        }
        Ok(())
    }

    fn refuse_foreign_folder(
        layout: &RootLayout,
        recorded: Option<&FormatMarker>,
    ) -> Result<(), NativeStorageError> {
        if recorded.is_none() && Self::holds_foreign_entries(layout.app_folder())? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::NotFyloRoot,
                format!(
                    "{} is not a FYLO app folder: it is not empty and has no .fylo/format.json. \
                     FYLO does not write into a folder it did not make; use a new or empty \
                     folder. An old-layout root is copied with `fylo upgrade-root`.",
                    layout.app_folder().display()
                ),
            ));
        }
        Ok(())
    }

    fn holds_foreign_entries(folder: &Path) -> Result<bool, NativeStorageError> {
        for entry in fs::read_dir(folder).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            if !Self::is_own_entry(&entry.file_name().to_string_lossy(), &entry.path())? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_own_entry(name: &str, path: &Path) -> Result<bool, NativeStorageError> {
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        let own_folder = name == RootLayout::STATE_FOLDER && metadata.is_dir();
        let own_guard = GuardFiles::is_guard_name(name) && metadata.is_file();
        Ok(own_folder || own_guard)
    }

    fn record_marker(
        layout: &RootLayout,
        recorded: Option<FormatMarker>,
        (created_by, probe): (
            &str,
            impl FnOnce(&Path) -> Result<ProbeRecord, NativeStorageError>,
        ),
    ) -> Result<FormatMarker, NativeStorageError> {
        match recorded {
            Some(recorded) => Ok(recorded),
            None => Self::write_new_marker(layout, created_by, probe),
        }
    }

    fn write_new_marker(
        layout: &RootLayout,
        created_by: &str,
        probe: impl FnOnce(&Path) -> Result<ProbeRecord, NativeStorageError>,
    ) -> Result<FormatMarker, NativeStorageError> {
        let probe = probe(&layout.app_state_folder())?;
        let marker = FormatMarker::new(created_by, probe)?;
        marker.write(&layout.format_marker_path())?;
        Ok(marker)
    }
}
