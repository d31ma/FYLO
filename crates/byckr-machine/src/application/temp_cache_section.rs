use std::path::Path;

use serde_json::Value;

use byckr_storage_native::{TempCacheMode, TempCacheSettings};

use super::secrets::Secrets;
use super::settings::Settings;

/// `fylo.tempCache` in `application.json`: `mode` (`"off"`, `"auto"` or
/// `"on"`), `bytes` (the budget) and `dir` (the base folder), all optional
/// (ADR 0030 section 7).
pub(super) struct TempCacheSection;

impl TempCacheSection {
    pub(super) const KEY: &'static str = "tempCache";
    const NAME: &'static str = "fylo.tempCache";
    const MODE_KEY: &'static str = "mode";
    const BYTES_KEY: &'static str = "bytes";
    const DIR_KEY: &'static str = "dir";
    const DIR_NAME: &'static str = "fylo.tempCache.dir";
    const MODE_ERROR: &'static str = "fylo.tempCache.mode must be \"off\", \"auto\" or \"on\"";
    const BYTES_ERROR: &'static str = "fylo.tempCache.bytes must be a whole number of bytes";

    pub(super) fn load(
        value: Option<&Value>,
        directory: &Path,
        secrets: &mut Secrets,
    ) -> Result<TempCacheSettings, String> {
        let section = value
            .map(|value| Self::read(value, directory, secrets))
            .transpose()?;
        Ok(section.unwrap_or_default())
    }

    fn read(
        value: &Value,
        directory: &Path,
        secrets: &mut Secrets,
    ) -> Result<TempCacheSettings, String> {
        let section = Settings::object(value, Self::NAME)?;
        Settings::known_keys(
            section,
            &[Self::MODE_KEY, Self::BYTES_KEY, Self::DIR_KEY],
            Self::NAME,
        )?;
        Ok(TempCacheSettings {
            mode: Self::mode(section.get(Self::MODE_KEY))?,
            bytes: Self::bytes(section.get(Self::BYTES_KEY))?,
            dir: Settings::path_setting(
                section.get(Self::DIR_KEY),
                Self::DIR_NAME,
                directory,
                secrets,
            )?,
        })
    }

    fn mode(value: Option<&Value>) -> Result<TempCacheMode, String> {
        let mode = value.map(Self::mode_of).transpose()?;
        Ok(mode.unwrap_or_default())
    }

    fn mode_of(value: &Value) -> Result<TempCacheMode, String> {
        let text = value.as_str().ok_or(Self::MODE_ERROR)?;
        TempCacheMode::parse(text).map_err(|_| Self::MODE_ERROR.to_owned())
    }

    fn bytes(value: Option<&Value>) -> Result<Option<u64>, String> {
        value
            .map(|value| value.as_u64().ok_or_else(|| Self::BYTES_ERROR.to_owned()))
            .transpose()
    }
}
