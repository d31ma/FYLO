use std::path::Path;

use fylo::{AccessTrust, DatabaseKey, Identity, OpenOptions};
use serde::Deserialize;

use crate::call_failure::CallFailure;
use crate::create_settings::CreateSettings;
use crate::engine_settings::EngineSettings;
use crate::identity_settings::IdentitySettings;

/// The JSON options of `fylo_open`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenSettings {
    config: Option<EngineSettings>,
    identity: Option<IdentitySettings>,
    create: Option<CreateSettings>,
}

impl OpenSettings {
    /// Parse the options; empty text means none.
    pub(crate) fn parse(text: &str) -> Result<OpenSettings, CallFailure> {
        if text.trim().is_empty() {
            return Ok(OpenSettings::default());
        }
        Ok(serde_json::from_str(text)?)
    }

    /// Make the database when `create` asks for it, then build the options
    /// `Root::open` takes.
    pub(crate) fn prepare(
        &self,
        path: &Path,
        key: Option<DatabaseKey>,
    ) -> Result<OpenOptions, CallFailure> {
        self.refuse_key_with_identity(key.as_ref())?;
        let admin = self.create_missing(path, key.as_ref())?;
        let options = OpenOptions::default().with_config(self.config()?);
        let identity = self.identity(admin)?;
        let options = Self::with_identity(options, identity);
        Ok(Self::with_key(options, key))
    }

    /// A protected database has its own keys (ADR 0025): a key given beside
    /// an identity is a mistake, not a second lock.
    fn refuse_key_with_identity(&self, key: Option<&DatabaseKey>) -> Result<(), CallFailure> {
        if key.is_some() && self.uses_identity() {
            return Err(CallFailure::bad_request(
                "an encryption key and an identity cannot be used together",
            ));
        }
        Ok(())
    }

    fn uses_identity(&self) -> bool {
        self.identity.is_some()
            || self
                .create
                .as_ref()
                .is_some_and(CreateSettings::is_protected)
    }

    fn create_missing(
        &self,
        path: &Path,
        key: Option<&DatabaseKey>,
    ) -> Result<Option<Identity>, CallFailure> {
        match &self.create {
            Some(create) => create.create_missing(path, key),
            None => Ok(None),
        }
    }

    fn with_key(options: OpenOptions, key: Option<DatabaseKey>) -> OpenOptions {
        match key {
            Some(key) => options.with_database_key(key),
            None => options,
        }
    }

    fn config(&self) -> Result<fylo::EngineConfig, CallFailure> {
        self.config.clone().unwrap_or_default().to_config()
    }

    /// The identity named in the options, or else the admin of a database
    /// this call just made protected, pinned to itself.
    fn identity(
        &self,
        admin: Option<Identity>,
    ) -> Result<Option<(Identity, AccessTrust)>, CallFailure> {
        if let Some(settings) = &self.identity {
            return settings.open().map(Some);
        }
        Ok(admin.map(|identity| {
            let anchor = identity.id().clone();
            (identity, AccessTrust::Pinned(anchor))
        }))
    }

    fn with_identity(
        options: OpenOptions,
        identity: Option<(Identity, AccessTrust)>,
    ) -> OpenOptions {
        match identity {
            Some((identity, trust)) => options.with_identity(identity, trust),
            None => options,
        }
    }
}
