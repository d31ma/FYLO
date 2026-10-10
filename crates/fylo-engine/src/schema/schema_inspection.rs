use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use super::schema_manifest::SchemaManifest;
use super::schema_paths::SchemaPaths;
use super::schema_version_status::SchemaVersionStatus;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SchemaInspection {
    pub(super) collection: String,
    pub(super) schema_dir: PathBuf,
    pub(super) versioned: bool,
    pub(super) current: Option<String>,
    pub(super) manifest_path: PathBuf,
    pub(super) manifest: Option<SchemaManifest>,
    pub(super) versions: Vec<SchemaVersionStatus>,
}

impl SchemaInspection {
    pub(super) fn load(paths: &SchemaPaths<'_>) -> Result<Self, String> {
        let manifest = SchemaManifest::load(paths)?;
        let versions = Self::versions(paths, manifest.as_ref());
        Ok(Self {
            collection: paths.collection.to_owned(),
            schema_dir: paths.root.to_owned(),
            versioned: manifest.is_some(),
            current: manifest.as_ref().map(Self::current_version),
            manifest_path: paths.manifest(),
            manifest,
            versions,
        })
    }

    pub(super) fn into_value(self) -> Value {
        serde_json::to_value(self).expect("schema inspection is serializable")
    }

    fn current_version(manifest: &SchemaManifest) -> String {
        manifest.current.clone()
    }

    fn versions(
        paths: &SchemaPaths<'_>,
        manifest: Option<&SchemaManifest>,
    ) -> Vec<SchemaVersionStatus> {
        manifest
            .map(|manifest| Self::inspect_versions(paths, manifest))
            .unwrap_or_default()
    }

    fn inspect_versions(
        paths: &SchemaPaths<'_>,
        manifest: &SchemaManifest,
    ) -> Vec<SchemaVersionStatus> {
        let mut versions = Vec::new();
        for index in 0..manifest.versions.len() {
            versions.push(SchemaVersionStatus::new(paths, manifest, index));
        }
        versions
    }
}
