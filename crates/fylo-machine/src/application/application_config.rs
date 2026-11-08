use std::path::{Path, PathBuf};

use fylo_identity::{Identity, IdentityId};
use fylo_storage_native::AccessTrust;
use fylo_storage_native::{DatabaseIdentity, DatabaseKey, RootOpenOptions};

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
    /// `fylo.identity`, or `FYLO_IDENTITY`: the key reference of the
    /// application identity a protected `.vdbx` database opens with
    /// (ADR 0021).
    pub identity_reference: Option<String>,
    /// `fylo.trustAnchor`, or `FYLO_TRUST_ANCHOR`: the identifier of the
    /// admin a protected database must have been created by.
    pub trust_anchor: Option<String>,
    /// Trust a protected database's creator, whoever it is, instead of a
    /// pinned anchor (`--trust-on-first-use`). Off by default.
    pub trust_on_first_use: bool,
    /// The identity [`Self::open_identity`] opened from the settings above.
    pub identity: Option<DatabaseIdentity>,
}

impl ApplicationConfig {
    const DEFAULT_FILE: &'static str = "application.json";

    const IDENTITY_VARIABLE: &'static str = "FYLO_IDENTITY";
    const TRUST_ANCHOR_VARIABLE: &'static str = "FYLO_TRUST_ANCHOR";

    /// `options` with this configuration's database key and identity, so
    /// an encrypted `.vdbx` root opens unlocked and a protected one opens
    /// as the identity. A directory root ignores both.
    #[must_use]
    pub fn with_database_access(&self, options: RootOpenOptions) -> RootOpenOptions {
        let options = self.with_key(options);
        self.with_identity(options)
    }

    /// Open the identity [`Self::identity_reference`] names, trusted as
    /// [`Self::trust_anchor`] or [`Self::trust_on_first_use`] say; no
    /// identity when none is named.
    ///
    /// # Errors
    /// Fails for a key reference that does not open, an anchor that is not
    /// an identifier, or both an anchor and trust on first use.
    pub fn open_identity(&mut self) -> Result<(), String> {
        self.identity = self
            .identity_reference
            .as_deref()
            .map(|reference| self.opened_identity(reference))
            .transpose()?;
        Ok(())
    }

    /// Take `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR` from the environment
    /// over the configuration file's settings.
    pub fn apply_identity_environment(&mut self) {
        if let Ok(reference) = std::env::var(Self::IDENTITY_VARIABLE) {
            self.identity_reference = Some(reference);
        }
        if let Ok(anchor) = std::env::var(Self::TRUST_ANCHOR_VARIABLE) {
            self.trust_anchor = Some(anchor);
        }
    }

    fn opened_identity(&self, reference: &str) -> Result<DatabaseIdentity, String> {
        let identity = Identity::from_reference(reference).map_err(|error| error.to_string())?;
        let trust = Self::trust(self.trust_anchor.as_deref(), self.trust_on_first_use)?;
        Ok(DatabaseIdentity::new(identity, trust))
    }

    fn trust(anchor: Option<&str>, first_use: bool) -> Result<AccessTrust, String> {
        match (anchor, first_use) {
            (Some(_), true) => {
                Err("a pinned trust anchor and --trust-on-first-use exclude each other".into())
            }
            (Some(anchor), false) => IdentityId::parse(anchor)
                .map(AccessTrust::Pinned)
                .map_err(|error| error.to_string()),
            (None, true) => Ok(AccessTrust::FirstUse),
            (None, false) => Ok(AccessTrust::Unpinned),
        }
    }

    fn with_key(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.database_key {
            Some(key) => options.with_database_key(key.clone()),
            None => options,
        }
    }

    fn with_identity(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.identity {
            Some(identity) => options.with_identity(identity.clone()),
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
        let file = Self::file(path)?;
        let source = ConfigFile::read(&file, path.is_none())?;
        let config = source
            .as_deref()
            .map(|source| Self::parse(source, &file))
            .transpose()?;
        let mut config = config.unwrap_or_default();
        config.apply_identity_environment();
        config.open_identity()?;
        Ok(config)
    }

    /// The database key the secret reference `reference` names, resolved
    /// as `fylo.encryption.key` is (ADR 0016 section 9): a complete
    /// `${NAME}` reference, never a literal secret, read from the
    /// environment or else from the `.env` file beside the configuration
    /// file `path` names (default `./application.json`), and at least 32
    /// UTF-8 bytes long. `option` names the setting in errors, which never
    /// show the secret. `fylo rekey --new-key` names the new key this way.
    ///
    /// # Errors
    /// Fails for a literal or malformed reference, a missing or invalid
    /// secret, and a secret shorter than 32 bytes.
    pub fn database_key_reference(
        path: Option<&Path>,
        reference: &str,
        option: &str,
    ) -> Result<DatabaseKey, String> {
        let file = Self::file(path)?;
        let directory = file
            .parent()
            .ok_or("configuration file has no parent directory")?;
        Settings::database_key(reference, option, directory)
    }

    /// The configuration file `path` names, or `./application.json`.
    fn file(path: Option<&Path>) -> Result<PathBuf, String> {
        let directory =
            std::env::current_dir().map_err(|_| "cannot resolve configuration directory")?;
        Ok(path.map_or_else(
            || directory.join(Self::DEFAULT_FILE),
            |path| directory.join(path),
        ))
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
