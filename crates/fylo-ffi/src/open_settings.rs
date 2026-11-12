use std::path::Path;

use fylo::{AccessTrust, Identity, OpenOptions};
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
    pub(crate) fn prepare(&self, path: &Path) -> Result<OpenOptions, CallFailure> {
        let admin = self.create_missing(path)?;
        let options = OpenOptions::default().with_config(self.config()?);
        let identity = self.identity(admin)?;
        Ok(Self::with_identity(options, identity))
    }

    fn create_missing(&self, path: &Path) -> Result<Option<Identity>, CallFailure> {
        match &self.create {
            Some(create) => create.create_missing(path),
            None => Ok(None),
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
