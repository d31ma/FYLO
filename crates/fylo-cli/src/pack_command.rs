use std::path::{Path, PathBuf};

use fylo_machine::ApplicationConfig;
use fylo_storage_native::{DatabasePackReport, NativeWriteRoot};
use serde_json::{Value, json};

/// `fylo pack <root> --out <db.fxdb>` and `fylo unpack <db.fxdb> --out
/// <directory>` (ADR 0016 section 11).
///
/// Like `export` and `salvage`, they write outside the machine protocol on
/// purpose: they take an output path, which a machine protocol client must
/// never be able to name.
pub struct PackCommand;

impl PackCommand {
    pub const COMMANDS: [&'static str; 2] = ["pack", "unpack"];

    /// Run one command on `source`: the directory root to pack, or the
    /// database to unpack.
    pub fn run(
        command: &str,
        (source, application): (&Path, &ApplicationConfig),
        options: &PackOptions,
    ) -> Result<String, String> {
        let out = options.out.as_deref().ok_or_else(|| match command {
            "pack" => "pack needs --out <db.fxdb>".to_owned(),
            _ => "unpack needs --out <directory>".to_owned(),
        })?;
        if options.sealed {
            return Err(
                "ENATIVE_UNSUPPORTED: fylo pack --sealed is not supported yet; a plain pack is a consistent copy too"
                    .into(),
            );
        }
        let writer = NativeWriteRoot::open(source)
            .map_err(|error| error.to_string())?
            .with_config(application.storage);
        let report = match command {
            "pack" => writer.pack_database(out, options.allow_engine_only_access),
            _ => writer.unpack_database(out),
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&Self::render(source, out, &report))
            .map_err(|error| error.to_string())
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
        json!({
            "source": source.display().to_string(),
            "out": out.display().to_string(),
            "collections": collections,
        })
    }
}

/// The flags `pack` and `unpack` read.
#[derive(Debug, Default)]
pub struct PackOptions {
    pub out: Option<PathBuf>,
    pub sealed: bool,
    pub allow_engine_only_access: bool,
}
