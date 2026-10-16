use std::path::Path;

use serde::{Deserialize, Serialize};

use super::schema_paths::SchemaPaths;
use super::schema_version::SchemaVersion;
use super::utils::schema_files::SchemaFiles;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct SchemaManifest {
    pub(super) current: String,
    #[serde(default)]
    pub(super) versions: Vec<SchemaVersion>,
}

impl SchemaManifest {
    pub(super) fn load(paths: &SchemaPaths<'_>) -> Result<Option<Self>, String> {
        Self::load_path(&paths.manifest())
    }

    pub(super) fn current(self) -> String {
        self.current
    }

    fn load_path(path: &Path) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        Self::read(path).map(Some)
    }

    fn read(path: &Path) -> Result<Self, String> {
        let bytes = SchemaFiles::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("schema manifest is corrupt: {error}"))
    }
}
