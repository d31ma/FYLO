use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use rustix::fs as attributes;

use crate::attribute_names::AttributeNames;
use crate::sized_read::SizedRead;

/// A file's extended attributes by path, never following a symlink, on iOS:
/// rustix's `l*xattr` functions, with sizes read safely ([`SizedRead`]).
pub struct ExtendedAttributes;

impl ExtendedAttributes {
    /// The attribute's value, or `None` when the file has no such attribute.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn get(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
        let (path, name) = (path.as_ref(), name.as_ref());
        SizedRead::absent_is_none(SizedRead::read(
            || attributes::lgetxattr_query(path, name),
            |buffer| attributes::lgetxattr(path, name, buffer),
        ))
    }

    /// Set the attribute, replacing any value it had.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn set(path: impl AsRef<Path>, name: impl AsRef<OsStr>, value: &[u8]) -> io::Result<()> {
        attributes::lsetxattr(
            path.as_ref(),
            name.as_ref(),
            value,
            attributes::XattrFlags::empty(),
        )?;
        Ok(())
    }

    /// Remove the attribute.
    ///
    /// # Errors
    /// The system's error, also when the file has no such attribute.
    pub fn remove(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<()> {
        attributes::lremovexattr(path.as_ref(), name.as_ref())?;
        Ok(())
    }

    /// The names of the file's attributes.
    ///
    /// # Errors
    /// The system's error for the file.
    pub fn list(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = OsString>> {
        let path = path.as_ref();
        let names = SizedRead::read(
            || attributes::llistxattr_query(path),
            |buffer| attributes::llistxattr(path, buffer),
        )?;
        Ok(AttributeNames::new(&names))
    }
}
