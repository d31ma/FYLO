use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::ApplicationConfig;
#[cfg(not(target_family = "wasm"))]
use fylo_storage_native::{CacheUsage, TempCache};
#[cfg(not(target_family = "wasm"))]
use serde_json::json;
use serde_json::{Map, Value};

/// `fylo cache status` and `fylo cache clear` (ADR 0030 section 4.5): the
/// current OS user's temp cache folder, below `--temp-cache-dir`,
/// `fylo.tempCache.dir` or the OS temp folder.
///
/// `status` prints the folder, whether it exists, the bytes and number of
/// its entries, its partial files and the time of its last sweep. `clear`
/// deletes the folder. Both refuse a folder that is not private (a link,
/// another owner, or one other users may reach) and leave it alone. A
/// WebAssembly build has no temp cache tier, so both fail there.
pub struct CacheCommand;

impl CacheCommand {
    const STATUS: &'static str = "status";
    const CLEAR: &'static str = "clear";
    const USAGE: &'static str =
        "usage: fylo cache <status|clear> [--temp-cache-dir <path>] [--config <path>] [--json]";
    #[cfg(target_family = "wasm")]
    const NATIVE_ONLY: &'static str =
        "fylo cache is not available in a WebAssembly build: the temp cache tier is native only";

    /// Run `action` on the cache folder `config` names, as text or, with
    /// `json`, as one JSON object.
    ///
    /// # Errors
    /// The usage text for a missing or unknown action; the reason a folder
    /// is refused or cannot be read or removed.
    pub fn run(
        action: Option<&str>,
        config: &ApplicationConfig,
        json: bool,
    ) -> Result<String, String> {
        let base = config.temp_cache.dir.as_deref();
        let report = Self::report(action, base)?;
        Ok(Self::render(&report, json))
    }

    fn report(action: Option<&str>, base: Option<&Path>) -> Result<Value, String> {
        match action {
            Some(Self::STATUS) => Self::status(base),
            Some(Self::CLEAR) => Self::clear(base),
            _ => Err(Self::USAGE.to_owned()),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn status(base: Option<&Path>) -> Result<Value, String> {
        let usage = TempCache::usage(base).map_err(|error| error.to_string())?;
        Ok(Self::usage_report(&usage))
    }

    #[cfg(not(target_family = "wasm"))]
    fn usage_report(usage: &CacheUsage) -> Value {
        json!({
            "dir": usage.dir.display().to_string(),
            "exists": usage.exists,
            "usedBytes": usage.used_bytes,
            "entries": usage.entries,
            "partialFiles": usage.partial_files,
            "lastSweepMs": usage.last_sweep.map(Self::unix_milliseconds),
        })
    }

    #[cfg(not(target_family = "wasm"))]
    fn clear(base: Option<&Path>) -> Result<Value, String> {
        let folder = TempCache::folder(base);
        let removed = TempCache::clear(base).map_err(|error| error.to_string())?;
        Ok(json!({
            "dir": folder.display().to_string(),
            "removed": removed.is_some(),
        }))
    }

    #[cfg(target_family = "wasm")]
    fn status(_base: Option<&Path>) -> Result<Value, String> {
        Err(Self::NATIVE_ONLY.to_owned())
    }

    #[cfg(target_family = "wasm")]
    fn clear(_base: Option<&Path>) -> Result<Value, String> {
        Err(Self::NATIVE_ONLY.to_owned())
    }

    #[cfg(not(target_family = "wasm"))]
    fn unix_milliseconds(time: SystemTime) -> u64 {
        let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    }

    fn render(report: &Value, json: bool) -> String {
        if json {
            return report.to_string();
        }
        Self::text(report.as_object().cloned().unwrap_or_default())
    }

    fn text(report: Map<String, Value>) -> String {
        report
            .into_iter()
            .map(|(name, value)| format!("{name}: {}", Self::plain(value)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn plain(value: Value) -> String {
        match value {
            Value::String(text) => text,
            other => other.to_string(),
        }
    }
}
