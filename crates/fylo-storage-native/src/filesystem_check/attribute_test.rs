use std::fs::File;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use super::probe_files::ProbeFiles;

/// Sets an extended attribute on a new probe file and reads it back. It
/// passes only when the file system keeps the attribute itself: macOS
/// pretends to keep attributes on NFS and SMB shares by writing a `._<name>`
/// `AppleDouble` file, which other machines do not read, so that counts as a
/// failure.
pub(crate) struct AttributeTest;

impl AttributeTest {
    #[cfg(unix)]
    const ATTRIBUTE: &'static str = "user.fylo.probe";
    #[cfg(not(unix))]
    const STREAM: &'static str = ":fylo.probe";
    const VALUE: &'static [u8] = b"fylo";

    pub(crate) fn run(files: &mut ProbeFiles) -> std::io::Result<()> {
        let (path, file) = files.create_new()?;
        Self::set(&file, &path, Self::VALUE)?;
        let value = Self::get(&file, &path)?;
        Self::require_same(value.as_deref())?;
        Self::require_no_apple_double(&path)
    }

    #[cfg(unix)]
    pub(super) fn set(file: &File, _path: &Path, value: &[u8]) -> std::io::Result<()> {
        use fylo_xattr::FileExt;

        file.set_xattr(Self::ATTRIBUTE, value)
    }

    #[cfg(unix)]
    fn get(file: &File, _path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        use fylo_xattr::FileExt;

        file.get_xattr(Self::ATTRIBUTE)
    }

    #[cfg(not(unix))]
    pub(super) fn set(_file: &File, path: &Path, value: &[u8]) -> std::io::Result<()> {
        std::fs::write(Self::stream_of(path), value)
    }

    #[cfg(not(unix))]
    fn get(_file: &File, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        std::fs::read(Self::stream_of(path)).map(Some)
    }

    #[cfg(not(unix))]
    fn stream_of(path: &Path) -> PathBuf {
        let mut stream = std::ffi::OsString::from(path.as_os_str());
        stream.push(Self::STREAM);
        PathBuf::from(stream)
    }

    fn require_same(value: Option<&[u8]>) -> std::io::Result<()> {
        if value == Some(Self::VALUE) {
            return Ok(());
        }
        Err(Error::other(
            "the attribute read back differs from the value written",
        ))
    }

    fn require_no_apple_double(path: &Path) -> std::io::Result<()> {
        let apple_double: PathBuf = ProbeFiles::apple_double_of(path);
        match std::fs::symlink_metadata(&apple_double) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(Error::other(format!(
                "the attribute was stored in the AppleDouble file {}, not by the file system",
                apple_double.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AttributeTest;
    use crate::filesystem_check::probe_files::ProbeFiles;

    #[test]
    fn an_apple_double_file_beside_the_probe_file_means_no_native_attributes() {
        let directory =
            std::env::temp_dir().join(format!("fylo-apple-double-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("probe.tmp");
        std::fs::write(ProbeFiles::apple_double_of(&path), b"emulated").unwrap();

        let result = AttributeTest::require_no_apple_double(&path);
        std::fs::remove_dir_all(&directory).unwrap();

        assert!(result.unwrap_err().to_string().contains("AppleDouble"));
    }

    #[test]
    fn no_apple_double_file_means_native_attributes() {
        let path = std::env::temp_dir().join("fylo-apple-double-missing-probe.tmp");

        AttributeTest::require_no_apple_double(&path).unwrap();
    }

    #[test]
    fn a_different_value_read_back_fails() {
        let result = AttributeTest::require_same(Some(b"other".as_slice()));

        assert!(result.is_err());
    }

    #[test]
    fn a_missing_value_read_back_fails() {
        assert!(AttributeTest::require_same(None).is_err());
    }
}
