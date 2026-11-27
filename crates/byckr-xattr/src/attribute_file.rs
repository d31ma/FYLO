use std::ffi::OsStr;
use std::fs::File;
use std::io;

use rustix::fs as attributes;

use crate::attribute_names::AttributeNames;
use crate::sized_read::SizedRead;

/// Extended attributes of an open file on iOS: the same calls as the
/// `xattr` crate's `FileExt`, through rustix's `f*xattr` functions.
pub trait FileExt {
    /// The attribute's value, or `None` when the file has no such attribute.
    ///
    /// # Errors
    /// The system's error for the file.
    fn get_xattr<N: AsRef<OsStr>>(&self, name: N) -> io::Result<Option<Vec<u8>>>;

    /// Set the attribute, replacing any value it had.
    ///
    /// # Errors
    /// The system's error for the file.
    fn set_xattr<N: AsRef<OsStr>>(&self, name: N, value: &[u8]) -> io::Result<()>;

    /// Remove the attribute.
    ///
    /// # Errors
    /// The system's error, also when the file has no such attribute.
    fn remove_xattr<N: AsRef<OsStr>>(&self, name: N) -> io::Result<()>;

    /// The names of the file's attributes.
    ///
    /// # Errors
    /// The system's error for the file.
    fn list_xattr(&self) -> io::Result<AttributeNames>;
}

impl FileExt for File {
    fn get_xattr<N: AsRef<OsStr>>(&self, name: N) -> io::Result<Option<Vec<u8>>> {
        let name = name.as_ref();
        SizedRead::absent_is_none(SizedRead::read(
            || attributes::fgetxattr_query(self, name),
            |buffer| attributes::fgetxattr(self, name, buffer),
        ))
    }

    fn set_xattr<N: AsRef<OsStr>>(&self, name: N, value: &[u8]) -> io::Result<()> {
        attributes::fsetxattr(self, name.as_ref(), value, attributes::XattrFlags::empty())?;
        Ok(())
    }

    fn remove_xattr<N: AsRef<OsStr>>(&self, name: N) -> io::Result<()> {
        attributes::fremovexattr(self, name.as_ref())?;
        Ok(())
    }

    fn list_xattr(&self) -> io::Result<AttributeNames> {
        let names = SizedRead::read(
            || attributes::flistxattr_query(self),
            |buffer| attributes::flistxattr(self, buffer),
        )?;
        Ok(AttributeNames::new(&names))
    }
}
