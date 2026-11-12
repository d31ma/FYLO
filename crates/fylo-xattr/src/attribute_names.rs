use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// The names of a file's extended attributes, as the system lists them:
/// each followed by a zero byte.
pub struct AttributeNames {
    names: std::vec::IntoIter<OsString>,
}

impl AttributeNames {
    pub(crate) fn new(list: &[u8]) -> AttributeNames {
        let names: Vec<OsString> = list
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| OsStr::from_bytes(name).to_owned())
            .collect();
        AttributeNames {
            names: names.into_iter(),
        }
    }
}

impl Iterator for AttributeNames {
    type Item = OsString;

    fn next(&mut self) -> Option<OsString> {
        self.names.next()
    }
}
