use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use super::schema_inspection::SchemaInspection;
use super::schema_paths::SchemaPaths;
use super::schema_version_status::SchemaVersionStatus;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SchemaDiagnosis {
    collection: String,
    schema_dir: PathBuf,
    ok: bool,
    issues: Vec<String>,
    warnings: Vec<String>,
    inspect: Option<SchemaInspection>,
}

impl SchemaDiagnosis {
    pub(super) fn load(paths: &SchemaPaths<'_>) -> Self {
        let mut issues = Vec::new();
        let inspect = Self::read_inspection(paths, &mut issues);
        Self::check_inspection(inspect.as_ref(), &mut issues);
        Self {
            collection: paths.collection.to_owned(),
            schema_dir: paths.root.to_owned(),
            ok: issues.is_empty(),
            issues,
            warnings: Vec::new(),
            inspect,
        }
    }

    pub(super) fn into_value(self) -> Value {
        serde_json::to_value(self).expect("schema diagnosis is serializable")
    }

    fn read_inspection(
        paths: &SchemaPaths<'_>,
        issues: &mut Vec<String>,
    ) -> Option<SchemaInspection> {
        match SchemaInspection::load(paths) {
            Ok(inspection) => Some(inspection),
            Err(error) => {
                issues.push(error);
                None
            }
        }
    }

    fn check_inspection(inspection: Option<&SchemaInspection>, issues: &mut Vec<String>) {
        if let Some(inspection) = inspection {
            Self::check_report(inspection, issues);
        }
    }

    fn check_report(inspection: &SchemaInspection, issues: &mut Vec<String>) {
        Self::check_manifest(inspection, issues);
        let mut seen = Vec::new();
        Self::check_versions(&inspection.versions, &mut seen, issues);
        let current = inspection.current.as_deref().unwrap_or_default();
        let missing = Self::current_is_missing(current, &seen);
        Self::check_current(current, missing, issues);
    }

    fn check_manifest(inspection: &SchemaInspection, issues: &mut Vec<String>) {
        if !inspection.versioned {
            issues.push(format!(
                "Missing manifest: {}",
                Self::path_value(&inspection.manifest_path)
            ));
        }
    }

    fn check_versions(
        versions: &[SchemaVersionStatus],
        seen: &mut Vec<String>,
        issues: &mut Vec<String>,
    ) {
        for version in versions {
            Self::check_version(version, seen, issues);
        }
    }

    fn check_version(
        version: &SchemaVersionStatus,
        seen: &mut Vec<String>,
        issues: &mut Vec<String>,
    ) {
        Self::check_duplicate(&version.version, seen, issues);
        seen.push(version.version.clone());
        Self::check_file(version, issues);
        Self::check_hash(version, issues);
        let missing_migration = Self::migration_is_missing(version);
        Self::check_migration(version, missing_migration, issues);
    }

    fn check_duplicate(label: &str, seen: &[String], issues: &mut Vec<String>) {
        if seen.contains(&label.to_owned()) {
            issues.push(format!("Duplicate version label: {label}"));
        }
    }

    fn check_file(version: &SchemaVersionStatus, issues: &mut Vec<String>) {
        if !version.exists {
            issues.push(format!(
                "Missing schema version file: {}",
                Self::path_value(&version.path)
            ));
        }
    }

    fn check_hash(version: &SchemaVersionStatus, issues: &mut Vec<String>) {
        if version.sha256_ok == Some(false) {
            issues.push(format!(
                "SHA-256 mismatch for {}: {}",
                version.version,
                Self::path_value(&version.path)
            ));
        }
    }

    fn migration_is_missing(version: &SchemaVersionStatus) -> bool {
        version.next_version.is_some() && version.migration_exists != Some(true)
    }

    fn check_migration(version: &SchemaVersionStatus, missing: bool, issues: &mut Vec<String>) {
        if missing {
            issues.push(format!(
                "Missing migration {}->{}: {}",
                version.version,
                serde_json::to_value(&version.next_version).expect("version label is serializable"),
                serde_json::to_value(&version.migration_path).expect("schema path is serializable")
            ));
        }
    }

    fn current_is_missing(current: &str, seen: &[String]) -> bool {
        !current.is_empty() && !seen.contains(&current.to_owned())
    }

    fn check_current(current: &str, missing: bool, issues: &mut Vec<String>) {
        if missing {
            issues.push(format!(
                "Current version is not declared in manifest.versions: {current}"
            ));
        }
    }

    fn path_value(path: &std::path::Path) -> Value {
        serde_json::to_value(path).expect("schema path is serializable")
    }
}
