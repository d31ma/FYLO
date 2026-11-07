use serde::Serialize;

use super::super::NativeWriteRoot;
use super::collection_access_report::CollectionAccessReport;
use super::pack_blockers::PackBlockers;
use crate::{NativeRoot, NativeStorageError};

/// What a directory root or a `.vdbx` database uses today, and what moving
/// it into a protected database with `fylo pack --protect` changes: the
/// output of `fylo access report` (ADR 0021 phase 6). Building it reads
/// every record and writes nothing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootAccessReport {
    /// The root's path.
    pub root: String,
    /// `directory` or `database`.
    pub kind: String,
    /// `none`, `shared-key` (`fylo.encryption.key`) or `protected`.
    pub encryption: String,
    /// Whether the database was opened without the key or identity it
    /// needs, so its collections are not listed.
    pub locked: bool,
    /// Records, live and deleted, that carry a POSTIX access descriptor.
    pub records_with_access: usize,
    /// Every collection, with what it holds.
    pub collections: Vec<CollectionAccessReport>,
    /// What `fylo pack` refuses in this root until it is removed.
    pub blockers: Vec<String>,
    /// What `fylo pack --protect` changes, one sentence each.
    pub changes: Vec<String>,
    /// The command that moves this root into a protected database, or
    /// `None` when it is one already.
    pub command: Option<String>,
    /// In a protected database opened with an identity, a catalog-key
    /// rotation a revocation left pending, with the command that finishes
    /// it (ADR 0021 section 7); left out when none is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_rotation: Option<crate::CatalogRotationNotice>,
}

impl RootAccessReport {
    const DIRECTORY: &'static str = "directory";
    const DATABASE: &'static str = "database";
    const PLAIN: &'static str = "none";
    const SHARED_KEY: &'static str = "shared-key";
    const PROTECTED: &'static str = "protected";

    /// Report on `root`, opened with whatever key or identity its caller
    /// has.
    ///
    /// # Errors
    ///
    /// Returns an error when a collection or record cannot be read.
    pub fn of(root: &NativeRoot) -> Result<Self, NativeStorageError> {
        let collections = Self::collections(root)?;
        let records_with_access = collections
            .iter()
            .map(|collection| collection.records_with_access)
            .sum();
        let encryption = Self::encryption(root);
        let blockers = Self::blockers(root)?;
        Ok(Self {
            root: root.path().display().to_string(),
            kind: Self::kind(root).to_owned(),
            encryption: encryption.to_owned(),
            locked: Self::locked(root),
            records_with_access,
            changes: Self::changes(root, (encryption, records_with_access), &collections),
            command: Self::command(root, (encryption, records_with_access)),
            catalog_rotation: Self::catalog_rotation(root),
            collections,
            blockers,
        })
    }

    fn catalog_rotation(root: &NativeRoot) -> Option<crate::CatalogRotationNotice> {
        root.database_access()
            .flatten()
            .and_then(|access| access.catalog_rotation_pending())
            .map(|version| crate::CatalogRotationNotice::of(root.path(), version))
    }

    fn kind(root: &NativeRoot) -> &'static str {
        if root.is_database() {
            return Self::DATABASE;
        }
        Self::DIRECTORY
    }

    fn encryption(root: &NativeRoot) -> &'static str {
        if root.database_access().is_some() {
            return Self::PROTECTED;
        }
        Self::unprotected_encryption(root)
    }

    fn unprotected_encryption(root: &NativeRoot) -> &'static str {
        match root.database_encryption() {
            Some((true, _)) => Self::SHARED_KEY,
            _ => Self::PLAIN,
        }
    }

    fn locked(root: &NativeRoot) -> bool {
        let without_key = root.database_encryption().is_some_and(|(_, locked)| locked);
        let without_identity = matches!(root.database_access(), Some(None));
        without_key || without_identity
    }

    fn blockers(root: &NativeRoot) -> Result<Vec<String>, NativeStorageError> {
        if root.is_database() {
            return Ok(Vec::new());
        }
        PackBlockers::of(root)
    }

    fn collection_names(root: &NativeRoot) -> Result<Vec<String>, NativeStorageError> {
        match root.database_collection_names() {
            Some(names) => Ok(names),
            None => NativeWriteRoot::packed_collection_names(root),
        }
    }

    fn collections(root: &NativeRoot) -> Result<Vec<CollectionAccessReport>, NativeStorageError> {
        let mut collections = Vec::new();
        for name in Self::collection_names(root)? {
            let collection = root.collection(&name)?;
            collections.push(CollectionAccessReport::of(root, &collection)?);
        }
        Ok(collections)
    }

    fn changes(
        root: &NativeRoot,
        (encryption, records_with_access): (&str, usize),
        collections: &[CollectionAccessReport],
    ) -> Vec<String> {
        let mut changes = Vec::new();
        changes.extend(Self::protected_change(encryption));
        changes.extend(Self::locked_change(root));
        changes.extend(Self::record_access_change(encryption, records_with_access));
        changes.extend(Self::encryption_change(encryption));
        changes.extend(Self::layout_change(root));
        changes.extend(Self::field_changes(encryption, collections));
        changes
    }

    fn protected_change(encryption: &str) -> Option<String> {
        if encryption != Self::PROTECTED {
            return None;
        }
        Some("it is a protected database already: there is nothing to migrate".into())
    }

    fn locked_change(root: &NativeRoot) -> Option<String> {
        if !Self::locked(root) {
            return None;
        }
        Some(
            "it was opened without its key (fylo.encryption.key) or identity, so its records are \
             not counted; run the report with them"
                .into(),
        )
    }

    fn record_access_change(encryption: &str, records_with_access: usize) -> Option<String> {
        if records_with_access == 0 || encryption == Self::PROTECTED {
            return None;
        }
        Some(format!(
            "{records_with_access} records carry POSTIX access descriptors (uid, gid, mode), which \
             this release no longer enforces: pack --protect drops them with \
             --drop-record-access, or keeps each as the metadata field legacyAccess with \
             --keep-record-access-as-metadata, and refuses without one of the two"
        ))
    }

    fn encryption_change(encryption: &str) -> Option<String> {
        match encryption {
            Self::SHARED_KEY => Some(
                "it is encrypted with the shared key fylo.encryption.key, which opens every \
                 collection; a protected database seals each collection under its own key, \
                 wrapped only for the identities its policy names"
                    .into(),
            ),
            Self::PLAIN => Some(
                "its records are stored in plain text; a protected database encrypts them, \
                 its directory and its indexes"
                    .into(),
            ),
            _ => None,
        }
    }

    fn layout_change(root: &NativeRoot) -> Option<String> {
        if root.is_database() {
            return None;
        }
        Some(
            "the directory root becomes one .vdbx file: its documents, deleted documents and \
             schema versions are copied, and its indexes are rebuilt inside the file, sealed"
                .into(),
        )
    }

    fn field_changes(encryption: &str, collections: &[CollectionAccessReport]) -> Vec<String> {
        if encryption == Self::PROTECTED {
            return Vec::new();
        }
        collections.iter().filter_map(Self::field_change).collect()
    }

    fn field_change(collection: &CollectionAccessReport) -> Option<String> {
        if collection.encrypted_fields.is_empty() {
            return None;
        }
        Some(format!(
            "collection {} keeps its $encrypted fields ({}) sealed with fylo.encryption.key, \
             which their readers still need",
            collection.collection,
            collection.encrypted_fields.join(", ")
        ))
    }

    fn command(
        root: &NativeRoot,
        (encryption, records_with_access): (&str, usize),
    ) -> Option<String> {
        if encryption == Self::PROTECTED {
            return None;
        }
        Some(format!(
            "fylo pack {} --out <db.vdbx> --protect --admin <key reference>{}",
            root.path().display(),
            Self::record_access_flag(records_with_access)
        ))
    }

    fn record_access_flag(records_with_access: usize) -> &'static str {
        if records_with_access == 0 {
            return "";
        }
        " (--drop-record-access | --keep-record-access-as-metadata)"
    }
}
