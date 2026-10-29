use std::path::{Path, PathBuf};

use fylo_machine::ApplicationConfig;
use fylo_storage_native::{
    ContainerFile, DatabaseKey, DatabasePackReport, NativeRoot, NativeWriteRoot, RootOpenOptions,
};
use serde_json::{Value, json};

/// `fylo pack <root> --out <db.vdbx>`, `fylo unpack <db.vdbx> --out
/// <directory>` and `fylo create <db.vdbx>` (ADR 0016 section 11).
///
/// `--encrypt`, or `fylo.encryption.file` in the configuration, makes the
/// database `create` or `pack` writes an encrypted one, sealed with
/// `fylo.encryption.key` (ADR 0016 section 9). Without either it is plain.
///
/// `pack --sealed` writes a sealed database: read-only, with no free
/// extents, recompressed in 64 KiB blocks (ADR 0016 sections 8 and 11). It
/// also takes a `.vdbx` database as its source, a consistent copy of a
/// live one for a backup; an encrypted source packs encrypted, with the
/// same key.
///
/// Like `export` and `salvage`, they write outside the machine protocol on
/// purpose: they take an output path, which a machine protocol client must
/// never be able to name.
pub struct PackCommand;

impl PackCommand {
    pub const COMMANDS: [&'static str; 3] = ["pack", "unpack", "create"];

    /// Run one command on `source`: the directory root to pack, or the
    /// database to unpack or create.
    pub fn run(
        command: &str,
        (source, application): (&Path, &ApplicationConfig),
        options: &PackOptions,
    ) -> Result<String, String> {
        if command == "create" {
            return Self::create(source, application, options);
        }
        let out = options.out.as_deref().ok_or_else(|| match command {
            "pack" => "pack needs --out <db.vdbx>".to_owned(),
            _ => "unpack needs --out <directory>".to_owned(),
        })?;
        if options.sealed && command != "pack" {
            return Err("--sealed is an option of pack".into());
        }
        let mut key = Self::encryption_key(application, options)?;
        let writer = NativeWriteRoot::open_profiled(
            application.with_database_key(RootOpenOptions::new(source)),
        )
        .map_err(|error| error.to_string())?
        .with_config(application.storage);
        if options.sealed && key.is_none() && Self::encrypted_database(source) {
            key = application.database_key.as_ref();
        }
        let allow = options.allow_engine_only_access;
        let report = match (command, key) {
            ("pack", key) if options.sealed => writer.pack_sealed_database(out, allow, key),
            ("pack", Some(key)) => writer.pack_encrypted_database(out, allow, key),
            ("pack", None) => writer.pack_database(out, allow),
            _ => writer.unpack_database(out),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&Self::render(source, out, &report))
            .map_err(|error| error.to_string())
    }

    /// Create an empty database at `path`, which must not exist.
    fn create(
        path: &Path,
        application: &ApplicationConfig,
        options: &PackOptions,
    ) -> Result<String, String> {
        if !NativeRoot::names_database(path) {
            return Err("create needs a path that ends in .vdbx".into());
        }
        let key = Self::encryption_key(application, options)?;
        match key {
            Some(key) => NativeRoot::create_encrypted_database(path, key),
            None => NativeRoot::create_database(path),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&json!({
            "database": path.display().to_string(),
            "encrypted": key.is_some(),
        }))
        .map_err(|error| error.to_string())
    }

    /// Whether `source` is an encrypted `.vdbx` database, whose sealed pack
    /// is encrypted with its key.
    fn encrypted_database(source: &Path) -> bool {
        source.is_file() && ContainerFile::open_reader(source).is_ok_and(|file| file.is_encrypted())
    }

    /// The key a new database is sealed with: `Some` under `--encrypt` or
    /// `fylo.encryption.file`, which both need `fylo.encryption.key`.
    fn encryption_key<'a>(
        application: &'a ApplicationConfig,
        options: &PackOptions,
    ) -> Result<Option<&'a DatabaseKey>, String> {
        if !options.encrypt {
            return Ok(application.creation_key());
        }
        application.database_key.as_ref().map(Some).ok_or_else(|| {
            "EKEY: --encrypt needs the database key in fylo.encryption.key".to_owned()
        })
    }

    fn render(source: &Path, out: &Path, report: &DatabasePackReport) -> Value {
        let collections: Vec<Value> = report
            .collections
            .iter()
            .map(|packed| {
                json!({
                    "collection": packed.collection,
                    "documents": packed.documents,
                    "deleted": packed.deleted,
                    "schemaVersions": packed.schema_versions,
                })
            })
            .collect();
        let mut rendered = json!({
            "source": source.display().to_string(),
            "out": out.display().to_string(),
            "collections": collections,
        });
        if let Some(sealed) = &report.sealed {
            rendered["sealed"] = json!(sealed);
        }
        rendered
    }
}

/// The flags `pack` and `unpack` read.
#[derive(Debug, Default)]
pub struct PackOptions {
    pub out: Option<PathBuf>,
    pub sealed: bool,
    pub encrypt: bool,
    pub allow_engine_only_access: bool,
}
