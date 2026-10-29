use std::path::{Path, PathBuf};

use fylo_storage_native::{DatabaseKey, RootOpenOptions};

use crate::{RootConfig, StrictValue};

use super::config_file::ConfigFile;
use super::settings::Settings;

#[derive(Clone, Default)]
pub struct ApplicationConfig {
    pub root: Option<PathBuf>,
    pub storage: RootConfig,
    pub schema: Option<PathBuf>,
    pub credentials: Option<(String, String)>,
    /// The secret `fylo.encryption.key` names, which opens an encrypted
    /// `.vdbx` database (ADR 0016 section 9).
    pub database_key: Option<DatabaseKey>,
    /// `fylo.encryption.file`: a `.vdbx` database this configuration
    /// creates is encrypted with [`Self::database_key`]. Off by default.
    pub encrypt_databases: bool,
    /// Memory a session holding the root's exclusive lease keeps for warm
    /// reads (ADR 0014). `None` is the default budget; zero turns it off.
    pub hot_set_bytes: Option<usize>,
}

impl ApplicationConfig {
    const DEFAULT_FILE: &'static str = "application.json";

    /// `options` with this configuration's database key, so an encrypted
    /// `.vdbx` root opens unlocked. A directory root ignores it.
    #[must_use]
    pub fn with_database_key(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.database_key {
            Some(key) => options.with_database_key(key.clone()),
            None => options,
        }
    }

    /// The key a `.vdbx` database this configuration creates is encrypted
    /// with: `Some` only under `fylo.encryption.file`.
    #[must_use]
    pub fn creation_key(&self) -> Option<&DatabaseKey> {
        self.database_key
            .as_ref()
            .filter(|_| self.encrypt_databases)
    }

    /// # Errors
    /// Fails for unreadable files, invalid settings or secret references, and exceeded limits.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let directory =
            std::env::current_dir().map_err(|_| "cannot resolve configuration directory")?;
        let file = path.map_or_else(
            || directory.join(Self::DEFAULT_FILE),
            |path| directory.join(path),
        );
        let source = ConfigFile::read(&file, path.is_none())?;
        let config = source
            .as_deref()
            .map(|source| Self::parse(source, &file))
            .transpose()?;
        Ok(config.unwrap_or_default())
    }

    fn parse(source: &str, file: &Path) -> Result<Self, String> {
        let value = serde_json::from_str::<StrictValue>(source)
            .map_err(|error| {
                format!(
                    "invalid application.json at line {}, column {}",
                    error.line(),
                    error.column()
                )
            })?
            .into_inner();
        let document = Settings::object(&value, Self::DEFAULT_FILE)?;
        let config = document
            .get("fylo")
            .map(|value| Settings::load(value, file))
            .transpose()?;
        Ok(config.unwrap_or_default())
    }
}
