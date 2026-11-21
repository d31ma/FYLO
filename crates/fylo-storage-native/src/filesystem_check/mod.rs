//! The file-system probe that runs once when an app folder is created, and
//! the small check that runs at every writing open (ADR 0033 section 6).

mod attribute_test;
mod filesystem_probe;
mod probe_failure;
mod probe_files;

pub use filesystem_probe::FilesystemProbe;
