use std::path::{Path, PathBuf};

use fylo_machine::ApplicationConfig;
use fylo_storage_native::{CollectionLayout, NativeWriteRoot};
use serde_json::{Value, json};

/// The segment layout's administrative commands (ADR 0015): `layout`,
/// `compact`, `export`, `verify-segments` and `salvage`.
///
/// They write outside the machine protocol on purpose: `export` and
/// `salvage` take an output directory, which a remote `fylo serve` client
/// must never be able to name.
pub struct SegmentCommand;

impl SegmentCommand {
    pub const COMMANDS: [&'static str; 5] =
        ["layout", "compact", "export", "verify-segments", "salvage"];

    /// Run one command. `collection` is its first positional argument.
    pub fn run(
        command: &str,
        (root, application): (&Path, &ApplicationConfig),
        collection: &str,
        options: &SegmentOptions,
    ) -> Result<String, String> {
        let writer = NativeWriteRoot::open(root)
            .map_err(|error| error.to_string())?
            .with_config(application.storage);
        let output = match command {
            "layout" => Self::layout(&writer, collection, options),
            "compact" => Self::compact(&writer, collection),
            "export" => Self::export(&writer, collection, options),
            "verify-segments" => Self::verify(&writer, collection),
            _ => Self::salvage(&writer, collection, options),
        }?;
        serde_json::to_string_pretty(&output).map_err(|error| error.to_string())
    }

    fn layout(
        writer: &NativeWriteRoot,
        collection: &str,
        options: &SegmentOptions,
    ) -> Result<Value, String> {
        let target = options
            .to
            .as_deref()
            .ok_or("layout needs --to files|segments")?;
        let layout = CollectionLayout::parse(target).map_err(|error| error.to_string())?;
        let report = writer
            .migrate_layout(collection, layout, options.allow_engine_only_access)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "collection": report.collection,
            "layout": report.layout.as_str(),
            "documents": report.documents,
            "deleted": report.deleted,
        }))
    }

    fn compact(writer: &NativeWriteRoot, collection: &str) -> Result<Value, String> {
        let report = writer
            .compact_collection(collection)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "collection": collection,
            "segmentsCompacted": report.segments_compacted,
            "documentsCopied": report.documents_copied,
            "bytesBefore": report.bytes_before,
            "bytesAfter": report.bytes_after,
        }))
    }

    fn export(
        writer: &NativeWriteRoot,
        collection: &str,
        options: &SegmentOptions,
    ) -> Result<Value, String> {
        let out = Self::output(options, "export")?;
        let report = writer
            .export_collection(collection, out)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "collection": report.collection,
            "out": out.display().to_string(),
            "documents": report.documents,
            "deleted": report.deleted,
        }))
    }

    fn verify(writer: &NativeWriteRoot, collection: &str) -> Result<Value, String> {
        let verification = writer
            .verify_segments(collection)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "collection": collection,
            "segments": verification.segments,
            "records": verification.records,
            "documents": verification.documents,
            "schemaVersions": verification.schema_versions,
            "defects": verification.defects,
            "ok": verification.defects.is_empty(),
        }))
    }

    fn salvage(
        writer: &NativeWriteRoot,
        collection: &str,
        options: &SegmentOptions,
    ) -> Result<Value, String> {
        let out = Self::output(options, "salvage")?;
        let report = writer
            .salvage_collection(collection, out)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "collection": collection,
            "out": out.display().to_string(),
            "recovered": report.recovered,
            "damaged": report.damaged,
            "lost": report.lost,
            "schemaVersions": report.schema_versions,
        }))
    }

    fn output<'a>(options: &'a SegmentOptions, command: &str) -> Result<&'a Path, String> {
        options
            .out
            .as_deref()
            .ok_or_else(|| format!("{command} needs --out <directory>"))
    }
}

/// The flags the segment commands read.
#[derive(Debug, Default)]
pub struct SegmentOptions {
    pub to: Option<String>,
    pub out: Option<PathBuf>,
    pub allow_engine_only_access: bool,
}
