mod read_window;
mod temp_cache_mode;
mod temp_cache_settings;
mod tier_link;

#[cfg(not(target_family = "wasm"))]
mod document_entry;
#[cfg(not(target_family = "wasm"))]
mod entry_key;
#[cfg(not(target_family = "wasm"))]
mod entry_lengths;
#[cfg(not(target_family = "wasm"))]
mod entry_reader;
#[cfg(not(target_family = "wasm"))]
mod pending_entry;
#[cfg(not(target_family = "wasm"))]
mod temp_cache_tier;
#[cfg(not(target_family = "wasm"))]
mod tier_counters;
#[cfg(not(target_family = "wasm"))]
mod tier_failpoint;
#[cfg(not(target_family = "wasm"))]
mod tier_reading;

#[cfg(not(target_family = "wasm"))]
mod cache_budget;
#[cfg(not(target_family = "wasm"))]
mod cache_file;
#[cfg(not(target_family = "wasm"))]
mod cache_file_kind;
#[cfg(not(target_family = "wasm"))]
mod cache_listing;
#[cfg(not(target_family = "wasm"))]
mod cache_location;
#[cfg(not(target_family = "wasm"))]
mod cache_sweep;
#[cfg(not(target_family = "wasm"))]
mod cache_usage;
#[cfg(not(target_family = "wasm"))]
mod file_opening;
#[cfg(unix)]
mod folder_owner;
#[cfg(not(target_family = "wasm"))]
mod private_folder;
#[cfg(not(target_family = "wasm"))]
mod sweep_report;
#[cfg(not(target_family = "wasm"))]
mod temp_cache;
#[cfg(not(target_family = "wasm"))]
mod temp_cache_reason;
#[cfg(not(target_family = "wasm"))]
mod temp_cache_root;
#[cfg(not(target_family = "wasm"))]
mod temp_cache_state;
#[cfg(not(target_family = "wasm"))]
mod temp_cache_status;
#[cfg(not(target_family = "wasm"))]
mod volume_facts;
#[cfg(windows)]
mod windows_folder;

pub(crate) use read_window::ReadWindow;
pub use temp_cache_mode::TempCacheMode;
pub use temp_cache_settings::TempCacheSettings;
pub(crate) use tier_link::TierLink;

#[cfg(not(target_family = "wasm"))]
pub use cache_budget::CacheBudget;
#[cfg(not(target_family = "wasm"))]
pub use cache_sweep::CacheSweep;
#[cfg(not(target_family = "wasm"))]
pub use cache_usage::CacheUsage;
#[cfg(unix)]
pub use folder_owner::FolderOwner;
#[cfg(not(target_family = "wasm"))]
pub use private_folder::PrivateFolder;
#[cfg(not(target_family = "wasm"))]
pub use sweep_report::SweepReport;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache::TempCache;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache_reason::TempCacheReason;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache_root::TempCacheRoot;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache_state::TempCacheState;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache_status::TempCacheStatus;
#[cfg(not(target_family = "wasm"))]
pub use temp_cache_tier::TempCacheTier;
#[cfg(not(target_family = "wasm"))]
pub(crate) use {
    document_entry::DocumentEntry, entry_key::EntryKey, pending_entry::PendingEntry,
    tier_reading::TierReading,
};
