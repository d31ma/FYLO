use std::path::Path;

use crate::{NativeStorageError, NativeStorageErrorCode};

/// The error of one extended-attribute call on a native root.
///
/// A filesystem without extended attributes (a Docker Desktop `VirtioFS` bind
/// mount, a FAT volume) answers every call with `EOPNOTSUPP` or `ENOTSUP`.
/// That error alone names neither the call nor the file (#106), so it is
/// replaced by a message that names both and lists the remedies. The code
/// stays `ENATIVE_IO`. Every other error is returned unchanged.
pub(crate) struct ExtendedAttributeError;

impl ExtendedAttributeError {
    const UNSUPPORTED_ERROR_NUMBERS: [i32; 2] = [libc::ENOTSUP, libc::EOPNOTSUPP];

    /// What a caller can do about a filesystem without extended attributes.
    pub(crate) const REMEDY: &'static str = "use a local filesystem with extended attributes, the efs-sidecar-v1 filesystem profile, or a .vdbx database";

    /// The error of `operation` (such as `setxattr`) on attribute `name` of
    /// the file at `path`.
    pub(crate) fn for_attribute(
        error: std::io::Error,
        operation: &str,
        name: &str,
        path: &Path,
    ) -> NativeStorageError {
        Self::describe(error, &format!("{operation} {name}"), path)
    }

    /// The error of `operation` (such as `listxattr`) on the file at `path`.
    pub(crate) fn for_file(
        error: std::io::Error,
        operation: &str,
        path: &Path,
    ) -> NativeStorageError {
        Self::describe(error, operation, path)
    }

    fn describe(error: std::io::Error, action: &str, path: &Path) -> NativeStorageError {
        if !Self::is_unsupported(&error) {
            return NativeStorageError::io(error);
        }
        NativeStorageError::new(
            NativeStorageErrorCode::Io,
            format!(
                "{action} on {}: filesystem does not support extended attributes ({error}); {}",
                path.display(),
                Self::REMEDY
            ),
        )
    }

    fn is_unsupported(error: &std::io::Error) -> bool {
        error
            .raw_os_error()
            .is_some_and(|number| Self::UNSUPPORTED_ERROR_NUMBERS.contains(&number))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::ExtendedAttributeError;
    use crate::NativeStorageErrorCode;

    const RECORD: &str = "/data/root/.collections/notes/docs/O/4VRNF52JPCO.json";

    #[test]
    fn an_unsupported_attribute_call_names_the_call_the_attribute_and_the_file() {
        let error = ExtendedAttributeError::for_attribute(
            std::io::Error::from_raw_os_error(libc::EOPNOTSUPP),
            "setxattr",
            "user.fylo.key",
            Path::new(RECORD),
        );

        assert_eq!(error.code(), NativeStorageErrorCode::Io);
        assert_eq!(error.code().as_str(), "ENATIVE_IO");
        assert!(
            error.to_string().starts_with(&format!(
                "ENATIVE_IO: setxattr user.fylo.key on {RECORD}: filesystem does not support extended attributes ("
            )),
            "{error}"
        );
        assert!(
            error.to_string().ends_with(ExtendedAttributeError::REMEDY),
            "{error}"
        );
    }

    #[test]
    fn an_unsupported_list_call_names_the_call_and_the_file() {
        let error = ExtendedAttributeError::for_file(
            std::io::Error::from_raw_os_error(libc::ENOTSUP),
            "listxattr",
            Path::new(RECORD),
        );

        assert_eq!(error.code(), NativeStorageErrorCode::Io);
        assert!(
            error.to_string().contains(&format!(
                "listxattr on {RECORD}: filesystem does not support"
            )),
            "{error}"
        );
    }

    #[test]
    fn any_other_error_is_returned_unchanged() {
        let original = std::io::Error::from_raw_os_error(libc::ENOSPC);
        let expected = format!("ENATIVE_IO: {original}");

        let error = ExtendedAttributeError::for_attribute(
            original,
            "setxattr",
            "user.fylo.key",
            Path::new(RECORD),
        );

        assert_eq!(error.code(), NativeStorageErrorCode::Io);
        assert_eq!(error.to_string(), expected);
    }
}
