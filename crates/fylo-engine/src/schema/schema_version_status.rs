use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::schema_manifest::SchemaManifest;
use super::schema_paths::SchemaPaths;
use super::schema_version::SchemaVersion;
use super::utils::schema_files::SchemaFiles;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SchemaVersionStatus {
    pub(super) version: String,
    pub(super) current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) added_at: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) sha256: Option<String>,
    pub(super) path: PathBuf,
    pub(super) exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) actual_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) sha256_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) next_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) migration_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) migration_exists: Option<bool>,
}

impl SchemaVersionStatus {
    pub(super) fn new(paths: &SchemaPaths<'_>, manifest: &SchemaManifest, index: usize) -> Self {
        let entry = &manifest.versions[index];
        let path = paths.version(&entry.version);
        let exists = path.is_file();
        let actual_sha256 = Self::file_hash(&path, exists);
        let next_version = manifest.versions.get(index + 1).map(Self::version_label);
        let migration_path = next_version
            .as_ref()
            .map(|next| paths.migration(&entry.version, next));
        let migration_exists = migration_path.as_deref().map(Self::file_exists);
        let sha256_ok = Self::hash_status(entry, actual_sha256.as_ref());
        Self {
            version: entry.version.clone(),
            current: entry.matches(&manifest.current),
            added_at: entry.added_at.clone(),
            sha256: entry.sha256.clone(),
            path,
            exists,
            actual_sha256,
            sha256_ok,
            next_version,
            migration_path,
            migration_exists,
        }
    }

    fn hash_status(entry: &SchemaVersion, actual: Option<&String>) -> Option<bool> {
        entry
            .sha256
            .as_ref()
            .map(|expected| Self::hash_matches(expected, actual))
    }

    fn hash_matches(expected: &String, actual: Option<&String>) -> bool {
        Some(expected) == actual
    }

    fn version_label(version: &SchemaVersion) -> String {
        version.version.clone()
    }

    fn file_exists(path: &Path) -> bool {
        path.is_file()
    }

    fn file_hash(path: &Path, exists: bool) -> Option<String> {
        if exists {
            return SchemaFiles::hash(path).ok();
        }
        None
    }
}
