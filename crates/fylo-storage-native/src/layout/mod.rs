//! The layout of ADR 0033: `<drive>/<app>/<namespace>/<collection>/<TTID>.json`.
//! Names, paths, the format marker and the guard files. The engine does not
//! use it yet; slice L1-5 switches the engine over.

mod app_folder;
mod app_name;
mod collection_name;
mod format_marker;
mod guard_files;
mod marker_file;
mod metadata_store;
mod name_problem;
mod name_rule;
mod name_subject;
mod namespace_marker;
mod namespace_name;
mod probe_record;
mod record_address;
mod record_kind;
mod root_layout;
#[cfg(all(test, not(target_family = "wasm")))]
mod tests;

pub use app_folder::AppFolder;
pub use app_name::AppName;
pub use collection_name::CollectionName;
pub use format_marker::FormatMarker;
pub use guard_files::GuardFiles;
use marker_file::MarkerFile;
pub use metadata_store::MetadataStore;
pub use name_problem::NameProblem;
pub use name_rule::NameRule;
use name_subject::NameSubject;
pub use namespace_marker::NamespaceMarker;
pub use namespace_name::NamespaceName;
pub use probe_record::ProbeRecord;
pub use record_address::RecordAddress;
pub use record_kind::RecordKind;
pub use root_layout::RootLayout;
