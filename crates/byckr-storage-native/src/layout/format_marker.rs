use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{MarkerFile, ProbeRecord};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// `<app>/.fylo/format.json`: names the layout, the BYCKR version that made
/// the app folder and the probe result (ADR 0033 section 1, storage contract
/// v2). A folder without it is not a BYCKR app folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FormatMarker {
    format: String,
    created_by: String,
    probe: ProbeRecord,
}

impl FormatMarker {
    /// The layout this marker names.
    pub const FORMAT: &'static str = "fylo.layout.v2";
    const MAX_CREATED_BY_BYTES: usize = 64;

    /// A marker for an app folder made by BYCKR version `created_by`.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_METADATA` when `created_by` is empty or longer than
    /// 64 bytes.
    pub fn new(created_by: &str, probe: ProbeRecord) -> Result<Self, NativeStorageError> {
        Self::check_created_by(created_by)?;
        Ok(Self {
            format: Self::FORMAT.to_owned(),
            created_by: created_by.to_owned(),
            probe,
        })
    }

    /// The marker at `path`, or `None` when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_METADATA` when the file is not a valid marker of this
    /// layout, and `ENATIVE_IO` when it cannot be read.
    pub fn read(path: &Path) -> Result<Option<Self>, NativeStorageError> {
        MarkerFile::read_if_present::<Self>(path)?
            .map(Self::validated)
            .transpose()
    }

    /// The BYCKR version that made the app folder.
    #[must_use]
    pub fn created_by(&self) -> &str {
        &self.created_by
    }

    /// The recorded probe result.
    #[must_use]
    pub fn probe(&self) -> ProbeRecord {
        self.probe
    }

    pub(crate) fn write(&self, path: &Path) -> Result<(), NativeStorageError> {
        crate::write::write_json_durable(path, self)
    }

    fn validated(self) -> Result<Self, NativeStorageError> {
        self.check_format()?;
        Self::check_created_by(&self.created_by)?;
        Ok(self)
    }

    fn check_format(&self) -> Result<(), NativeStorageError> {
        if self.format != Self::FORMAT {
            return Err(Self::corrupt(&format!(
                "names the layout `{}`, not `{}`",
                self.format.escape_debug(),
                Self::FORMAT
            )));
        }
        Ok(())
    }

    fn check_created_by(created_by: &str) -> Result<(), NativeStorageError> {
        if !(1..=Self::MAX_CREATED_BY_BYTES).contains(&created_by.len()) {
            return Err(Self::corrupt(
                "has a createdBy that is empty or over 64 bytes",
            ));
        }
        Ok(())
    }

    fn corrupt(reason: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("the app folder's format.json {reason}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::FormatMarker;
    use crate::layout::{MetadataStore, ProbeRecord};

    fn probe() -> ProbeRecord {
        ProbeRecord::new(MetadataStore::ExtendedAttributes, true)
    }

    #[test]
    fn the_json_follows_the_storage_contract() {
        let marker = FormatMarker::new("26.50.02", probe()).unwrap();
        assert_eq!(
            serde_json::to_string(&marker).unwrap(),
            r#"{"format":"fylo.layout.v2","createdBy":"26.50.02","probe":{"metadata":"xattr","hardLinks":true}}"#
        );
        assert_eq!(marker.created_by(), "26.50.02");
        assert_eq!(marker.probe(), probe());
    }

    #[test]
    fn a_bad_created_by_is_refused() {
        for created_by in ["", &"9".repeat(65)] {
            let error = FormatMarker::new(created_by, probe()).unwrap_err();
            assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
        }
    }

    #[test]
    fn a_marker_of_another_layout_is_refused() {
        let marker: FormatMarker = serde_json::from_str(
            r#"{"format":"fylo.layout.v9","createdBy":"x","probe":{"metadata":"xattr","hardLinks":true}}"#,
        )
        .unwrap();
        let error = marker.validated().unwrap_err();
        assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
        assert!(
            error.message().contains("fylo.layout.v9"),
            "{}",
            error.message()
        );
    }
}
