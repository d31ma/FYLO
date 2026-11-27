use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

/// A file's extended attributes by path, never following a symlink. Windows
/// has no extended-attribute backend in FYLO yet (ADR 0026), so every call is
/// a no-op: reads report no attribute and writes succeed without storing.
pub struct ExtendedAttributes;

impl ExtendedAttributes {
    /// The attribute's value, or `None` when the file has no such attribute.
    ///
    /// # Errors
    /// Never: this no-op always reports no attribute.
    pub fn get(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Set the attribute, replacing any value it had.
    ///
    /// # Errors
    /// Never: this no-op always succeeds.
    pub fn set(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>, _value: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Remove the attribute.
    ///
    /// # Errors
    /// Never: this no-op always succeeds.
    pub fn remove(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>) -> io::Result<()> {
        Ok(())
    }

    /// The names of the file's attributes.
    ///
    /// # Errors
    /// Never: this no-op always reports no attributes.
    pub fn list(_path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = OsString>> {
        Ok(std::iter::empty::<OsString>())
    }
}
