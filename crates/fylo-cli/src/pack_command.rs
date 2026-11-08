use std::path::{Path, PathBuf};

use fylo_engine::WriteEngine;
use fylo_identity::Identity;
use fylo_machine::ApplicationConfig;
use fylo_storage_native::{
    AccessGrantRequest, AccessTrust, ContainerFile, DatabaseIdentity, DatabaseKey,
    DatabasePackReport, DatabaseRekey, NativeRoot, NativeWriteRoot, RecordAccessMigration,
    RootOpenOptions,
};

use crate::grants_file::GrantsFile;
use serde_json::{Value, json};

/// `fylo pack <root> --out <db.vdbx>`, `fylo unpack <db.vdbx> --out
/// <directory>` and `fylo create <db.vdbx>` (ADR 0016 section 11), and
/// `fylo rekey <db.vdbx>` (ADR 0016 section 9).
///
/// `create --admin <reference>` makes a protected database (ADR 0021): its
/// keys are wrapped for application identities under a signed policy, and
/// the identity the reference names is its admin and trust anchor. A new
/// database is protected by default, so `create` without `--admin` needs
/// `--unprotected`.
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
/// `pack --protect --admin <reference>` packs a directory root, or a plain
/// or shared-key database, into a new protected database (ADR 0021, phase
/// 6). Its records' POSTIX access descriptors are refused unless
/// `--drop-record-access` or `--keep-record-access-as-metadata` says what
/// to do with them, and `--grants <file.json>` adds grants in the same
/// pack.
///
/// `rekey --new-key '${NAME}'` rewrites an encrypted database, opened with
/// `fylo.encryption.key`, under the secret the reference names, resolved
/// as `fylo.encryption.key` is: from the environment or the `.env` file
/// beside the configuration, never from a literal on the command line.
/// `--encrypt` with `--new-key` encrypts a plain database, and `--decrypt`
/// writes an encrypted one plain.
///
/// Like `export` and `salvage`, they write outside the machine protocol on
/// purpose: they take an output path, which a machine protocol client must
/// never be able to name, or replace the database file whole.
pub struct PackCommand;

impl PackCommand {
    pub const COMMANDS: [&'static str; 4] = ["pack", "unpack", "create", "rekey"];

    /// Run one command on `source`: the directory root to pack, or the
    /// database to unpack or create.
    pub fn run(
        command: &str,
        (source, application): (&Path, &ApplicationConfig),
        options: &PackOptions,
    ) -> Result<String, String> {
        if options.protect {
            return Self::protect(command, (source, application), options);
        }
        Self::require_no_record_access(options)?;
        if command == "rekey" {
            return Self::rekey(source, application, options);
        }
        if options.rekey_to.is_some() {
            return Err("--new-key and --decrypt are options of rekey".into());
        }
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
            application.with_database_access(RootOpenOptions::new(source)),
        )
        .map_err(|error| error.to_string())?
        .with_config(application.storage);
        if options.sealed && key.is_none() && Self::encrypted_database(source) {
            key = application.database_key.as_ref();
        }
        let report = match (command, key) {
            ("pack", key) if options.sealed => writer.pack_sealed_database(out, key),
            ("pack", Some(key)) => writer.pack_encrypted_database(out, key),
            ("pack", None) => writer.pack_database(out),
            _ => writer.unpack_database(out),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&Self::render(source, out, &report))
            .map_err(|error| error.to_string())
    }

    /// `pack --protect`: copy `source` into a new protected database whose
    /// admin and trust anchor is the identity `--admin` names, with the
    /// grants `--grants` lists (ADR 0021, phase 6).
    fn protect(
        command: &str,
        (source, application): (&Path, &ApplicationConfig),
        options: &PackOptions,
    ) -> Result<String, String> {
        Self::require_protect_options(command, options)?;
        let out = options
            .out
            .as_deref()
            .ok_or_else(|| "pack --protect needs --out <db.vdbx>".to_owned())?;
        let admin = Self::admin_identity(options.admin.as_deref())?;
        let grants = Self::grants(options.grants.as_deref())?;
        let writer = NativeWriteRoot::open_profiled(
            application.with_database_access(RootOpenOptions::new(source)),
        )
        .map_err(|error| error.to_string())?
        .with_config(application.storage);
        let report = writer
            .pack_protected_database(out, &admin, (options.record_access, &grants))
            .map_err(|error| error.to_string())?;
        let mut rendered = Self::render(source, out, &report);
        rendered["protected"] = json!(true);
        rendered["trustAnchor"] = json!(admin.identity().id().as_str());
        rendered["recordAccess"] = json!(Self::record_access_name(options.record_access));
        rendered["grants"] = json!(grants.len());
        serde_json::to_string_pretty(&rendered).map_err(|error| error.to_string())
    }

    fn require_protect_options(command: &str, options: &PackOptions) -> Result<(), String> {
        let refused = command != "pack"
            || options.sealed
            || options.encrypt
            || options.unprotected
            || options.rekey_to.is_some();
        if refused {
            return Err(
                "--protect is an option of pack, and takes none of --sealed, --encrypt, \
                 --unprotected, --new-key and --decrypt"
                    .into(),
            );
        }
        Ok(())
    }

    /// `--drop-record-access` and `--keep-record-access-as-metadata` belong
    /// to `pack --protect`.
    fn require_no_record_access(options: &PackOptions) -> Result<(), String> {
        if options.record_access == RecordAccessMigration::Refuse && options.grants.is_none() {
            return Ok(());
        }
        Err(
            "--drop-record-access, --keep-record-access-as-metadata and --grants are options of \
             pack --protect"
                .into(),
        )
    }

    /// The identity `--admin` names, pinned as the new database's trust
    /// anchor.
    fn admin_identity(reference: Option<&str>) -> Result<DatabaseIdentity, String> {
        let reference = reference.ok_or_else(|| {
            "pack --protect needs --admin <key reference>, the new database's admin".to_owned()
        })?;
        let admin = Identity::from_reference(reference).map_err(|error| error.to_string())?;
        let anchor = AccessTrust::Pinned(admin.id().clone());
        Ok(DatabaseIdentity::new(admin, anchor))
    }

    fn grants(path: Option<&Path>) -> Result<Vec<AccessGrantRequest>, String> {
        path.map_or_else(|| Ok(Vec::new()), GrantsFile::read)
    }

    fn record_access_name(record_access: RecordAccessMigration) -> &'static str {
        match record_access {
            RecordAccessMigration::Refuse => "none",
            RecordAccessMigration::Drop => "dropped",
            RecordAccessMigration::KeepAsMetadata => "keptAsMetadata",
        }
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
        if let Some(admin) = &options.admin {
            return Self::create_protected(path, admin, options);
        }
        Self::require_unprotected(options)?;
        let key = Self::encryption_key(application, options)?;
        match key {
            Some(key) => NativeRoot::create_encrypted_database(path, key),
            None => NativeRoot::create_database(path),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&json!({
            "database": path.display().to_string(),
            "encrypted": key.is_some(),
            "protected": false,
        }))
        .map_err(|error| error.to_string())
    }

    /// `create --admin <reference>`: a protected database whose admin and
    /// trust anchor is the identity `admin` names.
    fn create_protected(path: &Path, admin: &str, options: &PackOptions) -> Result<String, String> {
        if options.unprotected || options.encrypt {
            return Err(
                "create --admin makes a protected database and takes neither --unprotected nor --encrypt"
                    .into(),
            );
        }
        let admin = Identity::from_reference(admin).map_err(|error| error.to_string())?;
        NativeRoot::create_protected_database(path, &admin).map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&json!({
            "database": path.display().to_string(),
            "encrypted": true,
            "protected": true,
            "trustAnchor": admin.id().as_str(),
        }))
        .map_err(|error| error.to_string())
    }

    /// A new database is protected unless `--unprotected` says otherwise
    /// (ADR 0021, Q2).
    fn require_unprotected(options: &PackOptions) -> Result<(), String> {
        if options.unprotected {
            return Ok(());
        }
        Err("EPOLICY: a new .vdbx database is protected by default (ADR 0021): name its admin with --admin <key reference>, or pass --unprotected for a database any process that reads the file can open".into())
    }

    /// Rewrite the database at `path` under `--new-key`, or encrypt or
    /// decrypt it.
    fn rekey(
        path: &Path,
        application: &ApplicationConfig,
        options: &PackOptions,
    ) -> Result<String, String> {
        if options.out.is_some() || options.sealed {
            return Err(
                "rekey rewrites the database in place and takes no --out or --sealed".into(),
            );
        }
        let writer = NativeWriteRoot::open_profiled(
            application.with_database_access(RootOpenOptions::new(path)),
        )
        .map_err(|error| error.to_string())?
        .with_config(application.storage);
        if writer.native_root().database_access().is_some() {
            return Self::rekey_protected(path, (writer, application), options);
        }
        if options.collection.is_some() {
            return Err(
                "rekey --collection rewrites one collection of a protected database".into(),
            );
        }
        let rekey = match (&options.rekey_to, options.encrypt) {
            (Some(RekeyTo::Plain), true) => {
                return Err("rekey takes --encrypt or --decrypt, not both".into());
            }
            (None, true) => {
                return Err(
                    "rekey --encrypt needs --new-key '${NAME}', the secret to encrypt with".into(),
                );
            }
            (None, false) => {
                return Err(
                    "rekey needs --new-key '${NAME}', the secret the database is encrypted with \
                     from now on, or --decrypt"
                        .into(),
                );
            }
            (Some(RekeyTo::Key(key)), true) => DatabaseRekey::Encrypt(key),
            (Some(RekeyTo::Key(key)), false) => DatabaseRekey::Rotate(key),
            (Some(RekeyTo::Plain), false) => DatabaseRekey::Decrypt,
        };
        let report = writer
            .rekey_database(rekey)
            .map_err(|error| error.to_string())?;
        let mut rendered = json!({
            "database": path.display().to_string(),
            "encrypted": !matches!(rekey, DatabaseRekey::Decrypt),
            "collections": Self::collections(&report),
        });
        if let Some(sealed) = &report.sealed {
            rendered["sealed"] = json!(sealed);
        }
        serde_json::to_string_pretty(&rendered).map_err(|error| error.to_string())
    }

    /// `rekey` of a protected database, opened with an admin identity
    /// (ADR 0021 section 7): every key new, or with `--collection` that
    /// collection's. The engine writes `$encrypted` values again under the
    /// new field keys, reading them with the old ones and, when it is
    /// configured, `fylo.encryption.key`.
    fn rekey_protected(
        path: &Path,
        (writer, application): (NativeWriteRoot, &ApplicationConfig),
        options: &PackOptions,
    ) -> Result<String, String> {
        if options.rekey_to.is_some() || options.encrypt {
            return Err(
                "a protected database is rekeyed with the admin's identity; it takes no \
                        --new-key, --encrypt or --decrypt"
                    .into(),
            );
        }
        let mut engine = WriteEngine::from_writer(writer.clone());
        if let Some((secret, salt)) = &application.credentials {
            engine = engine
                .with_encryption_credentials(secret, salt)
                .map_err(|error| error.to_string())?;
        }
        let report = writer
            .rekey_protected_database(options.collection.as_deref(), &engine.field_rekey())
            .map_err(|error| error.to_string())?;
        let rendered = json!({
            "database": path.display().to_string(),
            "protected": true,
            "rekeyed": options.collection.as_deref().map_or("database", |_| "collection"),
            "collection": options.collection,
            "collections": Self::collections(&report),
        });
        serde_json::to_string_pretty(&rendered).map_err(|error| error.to_string())
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
        let mut rendered = json!({
            "source": source.display().to_string(),
            "out": out.display().to_string(),
            "collections": Self::collections(report),
        });
        if let Some(sealed) = &report.sealed {
            rendered["sealed"] = json!(sealed);
        }
        rendered
    }

    fn collections(report: &DatabasePackReport) -> Vec<Value> {
        report
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
            .collect()
    }
}

/// The flags `pack`, `unpack`, `create` and `rekey` read.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct PackOptions {
    pub out: Option<PathBuf>,
    pub sealed: bool,
    pub encrypt: bool,
    /// `create --admin <reference>`: the admin of a protected database.
    pub admin: Option<String>,
    /// `create --unprotected`: a database with no identities.
    pub unprotected: bool,
    /// `rekey --new-key` or `rekey --decrypt`.
    pub rekey_to: Option<RekeyTo>,
    /// `pack --protect`: pack into a protected database (ADR 0021).
    pub protect: bool,
    /// `--drop-record-access` or `--keep-record-access-as-metadata`.
    pub record_access: RecordAccessMigration,
    /// `--grants <file.json>`: the grants `pack --protect` adds.
    pub grants: Option<PathBuf>,
    /// `rekey --collection <name>`: rekey one collection of a protected
    /// database.
    pub collection: Option<String>,
}

/// What `rekey` rewrites a database with.
#[derive(Debug)]
pub enum RekeyTo {
    /// `--new-key`: the key its secret reference names.
    Key(DatabaseKey),
    /// `--decrypt`: no key.
    Plain,
}
