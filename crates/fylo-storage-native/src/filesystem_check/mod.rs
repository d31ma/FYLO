//! The file-system probe that runs once when an app folder is created, and
//! the small check that runs at every open (ADR 0033 section 6).

#[cfg(not(target_family = "wasm"))]
mod attribute_test;
#[cfg(not(target_family = "wasm"))]
mod filesystem_probe;
mod filesystem_probe_record;
mod metadata_storage;
#[cfg(not(target_family = "wasm"))]
mod probe_failure;
#[cfg(not(target_family = "wasm"))]
mod probe_files;

#[cfg(not(target_family = "wasm"))]
pub use filesystem_probe::FilesystemProbe;
pub use filesystem_probe_record::FilesystemProbeRecord;
pub use metadata_storage::MetadataStorage;
