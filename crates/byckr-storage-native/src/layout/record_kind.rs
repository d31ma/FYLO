use crate::{NativeStorageError, NativeStorageErrorCode};

/// What a record in a collection folder is: a JSON document, or a file kept
/// under its own extension beside the documents (ADR 0033 sections 1 and 7).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordKind {
    /// A document, `<TTID>.json`.
    Document,
    /// A file, `<TTID>.<extension>`.
    File {
        /// Lowercase letters and digits, 1 to 16 of them, never `json`.
        extension: String,
    },
}

impl RecordKind {
    /// The extension documents own.
    pub const DOCUMENT_EXTENSION: &'static str = "json";
    const MAX_EXTENSION_BYTES: usize = 16;

    /// A file record with this extension.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSAFE_PATH` when the extension is empty, longer than
    /// 16 bytes, holds anything but lowercase letters and digits, or is
    /// `json`, which documents own.
    pub fn file(extension: &str) -> Result<Self, NativeStorageError> {
        Self::check_extension_shape(extension)?;
        Self::check_not_document_extension(extension)?;
        Ok(Self::File {
            extension: extension.to_owned(),
        })
    }

    /// The extension of the record's file name.
    #[must_use]
    pub fn extension(&self) -> &str {
        match self {
            Self::Document => Self::DOCUMENT_EXTENSION,
            Self::File { extension } => extension,
        }
    }

    fn check_extension_shape(extension: &str) -> Result<(), NativeStorageError> {
        if !Self::has_extension_shape(extension) {
            return Err(Self::refusal(
                extension,
                "is not 1-16 lowercase letters or digits",
            ));
        }
        Ok(())
    }

    fn has_extension_shape(extension: &str) -> bool {
        (1..=Self::MAX_EXTENSION_BYTES).contains(&extension.len())
            && extension
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }

    fn check_not_document_extension(extension: &str) -> Result<(), NativeStorageError> {
        if extension == Self::DOCUMENT_EXTENSION {
            return Err(Self::refusal(extension, "belongs to documents"));
        }
        Ok(())
    }

    fn refusal(extension: &str, reason: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::UnsafePath,
            format!("file extension `{}` {reason}", extension.escape_debug()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::RecordKind;

    #[test]
    fn a_document_uses_json() {
        assert_eq!(RecordKind::Document.extension(), "json");
    }

    #[test]
    fn a_file_keeps_its_extension() {
        assert_eq!(RecordKind::file("png").unwrap().extension(), "png");
        assert_eq!(RecordKind::file("mp4").unwrap().extension(), "mp4");
        assert_eq!(RecordKind::file("bin").unwrap().extension(), "bin");
    }

    #[test]
    fn a_file_may_not_use_json() {
        let error = RecordKind::file("json").unwrap_err();
        assert_eq!(error.code().as_str(), "ENATIVE_UNSAFE_PATH");
        assert!(error.message().contains("belongs to documents"));
    }

    #[test]
    fn unsafe_extensions_are_refused() {
        for extension in [
            "",
            "PNG",
            "tar.gz",
            "../x",
            "a/b",
            &"a".repeat(17),
            "p\u{e9}g",
        ] {
            let error = RecordKind::file(extension).unwrap_err();
            assert_eq!(error.code().as_str(), "ENATIVE_UNSAFE_PATH", "{extension}");
        }
    }
}
