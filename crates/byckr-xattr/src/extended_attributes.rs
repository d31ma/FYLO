use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

/// A file's extended attributes by path, never following a symlink. On this
/// system they are the `xattr` crate's calls.
pub struct ExtendedAttributes;

impl ExtendedAttributes {
    /// The attribute's value, or `None` when the file has no such attribute.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn get(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
        xattr::get(path, name)
    }

    /// Set the attribute, replacing any value it had.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn set(path: impl AsRef<Path>, name: impl AsRef<OsStr>, value: &[u8]) -> io::Result<()> {
        xattr::set(path, name, value)
    }

    /// Remove the attribute.
    ///
    /// # Errors
    /// The system's error, also when the file has no such attribute.
    pub fn remove(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<()> {
        xattr::remove(path, name)
    }

    /// The names of the file's attributes.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn list(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = OsString>> {
        xattr::list(path)
    }
}
