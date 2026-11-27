use std::path::Path;

use serde::Serialize;

/// What `fylo access report` and `fylo verify` say of a catalog-key
/// rotation a revocation left pending (ADR 0021 section 7): until it is
/// settled, directory blocks under the old catalog key stay readable by the
/// identity the revocation removed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogRotationNotice {
    /// The policy version the rotation started at.
    pub pending_since: u64,
    /// What is pending, for a person.
    pub message: String,
    /// The command that finishes it.
    pub command: String,
}

impl CatalogRotationNotice {
    const UNKNOWN_VERSION: &'static str = "a catalog rotation is pending (open the database with an identity to see since which \
         policy version)";

    /// The notice for the database at `path`, pending since `version`.
    #[must_use]
    pub fn of(path: &Path, version: u64) -> CatalogRotationNotice {
        CatalogRotationNotice {
            pending_since: version,
            message: Self::message(Some(version)),
            command: Self::command(path),
        }
    }

    /// The sentence for a rotation pending since `version`, or since a
    /// version this reader cannot see.
    #[must_use]
    pub fn message(version: Option<u64>) -> String {
        let pending = version.map_or_else(
            || Self::UNKNOWN_VERSION.to_owned(),
            |version| format!("catalog rotation pending since policy version {version}"),
        );
        format!(
            "{pending}: directory blocks under the old catalog key stay readable by the \
             revoked identity until an admin runs fylo access settle"
        )
    }

    /// `fylo access settle <path>`.
    #[must_use]
    pub fn command(path: &Path) -> String {
        format!("fylo access settle {}", path.display())
    }
}
