//! The rule that a server root lives on a local disk that is not the
//! system disk, with its opt-out and app mode (ADR 0032).

#[cfg(not(target_family = "wasm"))]
mod app_folders;
mod app_id;
#[cfg(target_os = "macos")]
mod apple_disks;
#[cfg(any(test, target_os = "macos"))]
mod bsd_disk_name;
#[cfg(not(target_family = "wasm"))]
mod disk_description;
#[cfg(any(test, target_os = "macos"))]
mod disk_image_table;
#[cfg(not(target_family = "wasm"))]
mod disk_reader;
#[cfg(not(target_family = "wasm"))]
mod disk_source;
#[cfg(any(test, target_os = "linux", target_os = "android"))]
mod linux_disks;
#[cfg(any(test, target_os = "linux", target_os = "android"))]
mod mount_table;
mod opt_out_source;
mod placement_kind;
mod placement_report;
mod placement_settings;
mod root_placement;
#[cfg(test)]
mod scratch;
#[cfg(not(target_family = "wasm"))]
mod system_disk_refusal;
#[cfg(not(target_family = "wasm"))]
mod system_disk_rule;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod tool_runner;
#[cfg(windows)]
mod windows_disks;

pub use app_id::AppId;
pub use opt_out_source::OptOutSource;
pub use placement_kind::PlacementKind;
pub use placement_report::PlacementReport;
pub use placement_settings::PlacementSettings;
pub use root_placement::RootPlacement;
#[cfg(not(target_family = "wasm"))]
pub(crate) use system_disk_rule::SystemDiskRule;
