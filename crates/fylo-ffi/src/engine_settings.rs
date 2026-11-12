use fylo::{Durability, EngineConfig};
use serde::Deserialize;

use crate::call_failure::CallFailure;

/// `config` in the open options: the engine settings phones may change.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineSettings {
    durability: Option<String>,
    strict: Option<bool>,
}

impl EngineSettings {
    pub(crate) fn to_config(&self) -> Result<EngineConfig, CallFailure> {
        Ok(EngineConfig {
            durability: self.durability()?,
            strict_schema: self.strict.unwrap_or(false),
            ..EngineConfig::default()
        })
    }

    fn durability(&self) -> Result<Durability, CallFailure> {
        let Some(name) = self.durability.as_deref() else {
            return Ok(Durability::default());
        };
        Durability::parse(name).ok_or_else(|| {
            CallFailure::bad_request("config.durability must be \"process\" or \"power-loss\"")
        })
    }
}
