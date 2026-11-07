use fylo_machine::{ApplicationConfig, RootConfig};
use serde_json::{Map, Value};

/// What a browser page tells the engine before its first batch (ADR 0021,
/// decision P5-5): the `webcrypto:` identity a protected `.vdbx` opens
/// with, and how its creator is trusted. A browser has no environment and no
/// `application.json`, so these arrive as one JSON object.
pub struct BrowserSettings;

impl BrowserSettings {
    const KEYS: [&'static str; 3] = ["identity", "trustAnchor", "trustOnFirstUse"];

    /// The configuration `bytes` describes, with its identity opened
    /// through the host.
    ///
    /// # Errors
    /// Text naming the first setting that is not valid, or why the identity
    /// did not open.
    pub fn parse(bytes: &[u8]) -> Result<ApplicationConfig, String> {
        let value: Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let settings = Self::object(&value)?;
        Self::require_known_keys(settings)?;
        let mut config = ApplicationConfig {
            storage: RootConfig::default(),
            identity_reference: Self::text(settings, "identity")?,
            trust_anchor: Self::text(settings, "trustAnchor")?,
            trust_on_first_use: Self::flag(settings, "trustOnFirstUse")?,
            ..ApplicationConfig::default()
        };
        config.open_identity()?;
        Ok(config)
    }

    /// The configuration a page that sent no settings runs with.
    pub fn plain() -> ApplicationConfig {
        ApplicationConfig {
            storage: RootConfig::default(),
            ..ApplicationConfig::default()
        }
    }

    fn object(value: &Value) -> Result<&Map<String, Value>, String> {
        value
            .as_object()
            .ok_or_else(|| "the engine settings must be a JSON object".to_owned())
    }

    fn require_known_keys(settings: &Map<String, Value>) -> Result<(), String> {
        match settings
            .keys()
            .find(|key| !Self::KEYS.contains(&key.as_str()))
        {
            Some(key) => Err(format!("unknown engine setting {key:?}")),
            None => Ok(()),
        }
    }

    fn text(settings: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
        match settings.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("the engine setting {key:?} must be a string")),
        }
    }

    fn flag(settings: &Map<String, Value>, key: &str) -> Result<bool, String> {
        match settings.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(format!("the engine setting {key:?} must be true or false")),
        }
    }
}
