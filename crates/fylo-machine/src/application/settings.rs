use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::RootConfig;
use fylo_storage_native::{CollectionLayout, DatabaseKey, Durability};

use super::application_config::ApplicationConfig;
use super::secret_reference::SecretReference;
use super::secrets::Secrets;

pub(super) struct Settings;

impl Settings {
    const SECTION: &'static str = "fylo";
    const ROOT_KEY: &'static str = "root";
    const STRICT_KEY: &'static str = "strict";
    const SCHEMA_KEY: &'static str = "schema";
    const SHARD_KEY: &'static str = "shard";
    const ENCRYPTION_KEY: &'static str = "encryption";
    const CIPHER_KEY: &'static str = "cipher";
    const CHEX_KEY: &'static str = "chex";
    const DURABILITY_KEY: &'static str = "durability";
    const PATH_KEY: &'static str = "path";
    const SECRET_KEY: &'static str = "key";
    const FILE_KEY: &'static str = "file";
    const AUTO_PUSH_KEY: &'static str = "autoPush";
    const LAYOUT_KEY: &'static str = "layout";
    const MINIMUM_SHARD_WIDTH: u64 = 1;
    const MAXIMUM_SHARD_WIDTH: u64 = 4;
    const MINIMUM_KEY_BYTES: usize = 32;
    const PATH_ERROR: &'static str = "must be a nonempty path without NUL";
    const KEYS: [&'static str; 9] = [
        Self::ROOT_KEY,
        Self::STRICT_KEY,
        Self::SCHEMA_KEY,
        Self::SHARD_KEY,
        Self::ENCRYPTION_KEY,
        Self::CIPHER_KEY,
        Self::CHEX_KEY,
        Self::DURABILITY_KEY,
        Self::LAYOUT_KEY,
    ];
    const CREDENTIAL_ERROR: &'static str = "fylo.encryption.key and fylo.cipher.salt require paired secret references; key must contain at least 32 UTF-8 bytes and salt must not be empty";

    pub(super) fn load(value: &Value, file: &Path) -> Result<ApplicationConfig, String> {
        let settings = Self::object(value, Self::SECTION)?;
        Self::known_keys(settings, &Self::KEYS, Self::SECTION)?;
        let directory = file
            .parent()
            .ok_or("configuration file has no parent directory")?;
        let mut secrets = Secrets::new(directory);
        let root = Self::path_setting(
            settings.get(Self::ROOT_KEY),
            "fylo.root",
            directory,
            &mut secrets,
        )?;
        let (schema, schema_auto_push) =
            Self::schema(settings.get(Self::SCHEMA_KEY), directory, &mut secrets)?;
        let strict_schema = Self::strict(settings.get(Self::STRICT_KEY))?;
        let durability = Self::durability(settings.get(Self::DURABILITY_KEY))?;
        let layout = Self::layout(settings.get(Self::LAYOUT_KEY))?;
        let shard_width = Self::member(settings, Self::SHARD_KEY, "width")?
            .map(Self::shard_width)
            .transpose()?;
        let (secret, encrypt_databases) =
            Self::encryption(settings.get(Self::ENCRYPTION_KEY), &mut secrets)?;
        let salt = Self::secret_setting(
            Self::member(settings, Self::CIPHER_KEY, "salt")?,
            "fylo.cipher.salt",
            &mut secrets,
        )?;
        let database_key = secret.as_deref().map(DatabaseKey::new);
        let credentials = match (secret, salt) {
            // A database key alone encrypts files, and no field.
            (Some(secret), None) if encrypt_databases => {
                Self::require_key_length(&secret)?;
                None
            }
            (secret, salt) => Self::credentials(secret, salt)?,
        };
        Self::reject_removed_chex(settings)?;
        Ok(ApplicationConfig {
            root,
            storage: RootConfig {
                strict_schema,
                shard_width,
                durability,
                schema_auto_push,
                layout,
            },
            schema,
            credentials,
            database_key,
            encrypt_databases,
            hot_set_bytes: None,
        })
    }

    /// `fylo.encryption`: `key`, a secret reference, and `file`, whether a
    /// `.fxdb` database created with this configuration is encrypted with
    /// it (ADR 0016 section 9). Encryption is opt-in: `file` is off unless
    /// set, and it needs the key.
    fn encryption(
        value: Option<&Value>,
        secrets: &mut Secrets,
    ) -> Result<(Option<String>, bool), String> {
        let Some(value) = value else {
            return Ok((None, false));
        };
        let name = "fylo.encryption";
        let encryption = Self::object(value, name)?;
        Self::known_keys(encryption, &[Self::SECRET_KEY, Self::FILE_KEY], name)?;
        let secret = Self::secret_setting(
            encryption.get(Self::SECRET_KEY),
            "fylo.encryption.key",
            secrets,
        )?;
        let file = match encryption.get(Self::FILE_KEY) {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("fylo.encryption.file must be a boolean")?,
        };
        if file && secret.is_none() {
            return Err(
                "fylo.encryption.file requires fylo.encryption.key, the secret a database's key is derived from".into(),
            );
        }
        Ok((secret, file))
    }

    /// `fylo.schema`: the folder path, or `{"path": ..., "autoPush": bool}`
    /// to also push the folder's new versions into the root's schema
    /// registry when a write engine opens (ADR 0017 section 3).
    fn schema(
        value: Option<&Value>,
        directory: &Path,
        secrets: &mut Secrets,
    ) -> Result<(Option<PathBuf>, bool), String> {
        let Some(Value::Object(schema)) = value else {
            let path = Self::path_setting(value, "fylo.schema", directory, secrets)?;
            return Ok((path, false));
        };
        Self::known_keys(
            schema,
            &[Self::PATH_KEY, Self::AUTO_PUSH_KEY],
            "fylo.schema",
        )?;
        let path = schema
            .get(Self::PATH_KEY)
            .ok_or("fylo.schema.path is required when fylo.schema is an object")?;
        let path = Self::resolve_path(path, "fylo.schema.path", directory, secrets)?;
        let auto_push = match schema.get(Self::AUTO_PUSH_KEY) {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("fylo.schema.autoPush must be a boolean")?,
        };
        Ok((Some(path), auto_push))
    }

    /// `fylo.durability`: `"process"` (the default) or `"power-loss"`.
    fn durability(value: Option<&Value>) -> Result<Durability, String> {
        let Some(value) = value else {
            return Ok(Durability::default());
        };
        value
            .as_str()
            .and_then(Durability::parse)
            .ok_or_else(|| "fylo.durability must be \"process\" or \"power-loss\"".into())
    }

    /// `fylo.layout`: `"files"` (the default) or `"segments"`, for document
    /// collections created from now on.
    fn layout(value: Option<&Value>) -> Result<CollectionLayout, String> {
        let Some(value) = value else {
            return Ok(CollectionLayout::default());
        };
        value
            .as_str()
            .and_then(|name| CollectionLayout::parse(name).ok())
            .ok_or_else(|| "fylo.layout must be \"files\" or \"segments\"".into())
    }

    /// `fylo.chex.binary` selected the external validator FYLO used to spawn.
    /// CHEX now runs in-process, so the setting has nothing left to choose;
    /// say so rather than reporting a generic unknown key.
    fn reject_removed_chex(settings: &Map<String, Value>) -> Result<(), String> {
        if settings.contains_key(Self::CHEX_KEY) {
            return Err(
                "fylo.chex is no longer supported; CHEX schema validation is built into FYLO, so remove the setting".into(),
            );
        }
        Ok(())
    }

    pub(super) fn object<'a>(
        value: &'a Value,
        name: &str,
    ) -> Result<&'a Map<String, Value>, String> {
        value
            .as_object()
            .ok_or_else(|| format!("{name} must be an object"))
    }

    fn known_keys(
        settings: &Map<String, Value>,
        allowed: &[&str],
        name: &str,
    ) -> Result<(), String> {
        let unknown = Self::contains_unknown_keys(settings, allowed);
        Self::require_known_keys(unknown, name)
    }

    fn contains_unknown_keys(settings: &Map<String, Value>, allowed: &[&str]) -> bool {
        settings.keys().any(|key| !allowed.contains(&key.as_str()))
    }

    fn require_known_keys(unknown: bool, name: &str) -> Result<(), String> {
        if unknown {
            return Err(format!(
                "unknown key under {name}; use documented nested configuration keys"
            ));
        }
        Ok(())
    }

    fn member<'a>(
        settings: &'a Map<String, Value>,
        parent: &str,
        key: &str,
    ) -> Result<Option<&'a Value>, String> {
        settings
            .get(parent)
            .map(|value| Self::nested_member(value, parent, key))
            .transpose()
            .map(Option::flatten)
    }

    fn nested_member<'a>(
        value: &'a Value,
        parent: &str,
        key: &str,
    ) -> Result<Option<&'a Value>, String> {
        let name = format!("fylo.{parent}");
        let nested = Self::object(value, &name)?;
        Self::known_keys(nested, &[key], &name)?;
        Ok(nested.get(key))
    }

    fn strict(value: Option<&Value>) -> Result<bool, String> {
        match value {
            None => Ok(false),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "fylo.strict must be a boolean".into()),
        }
    }

    fn shard_width(value: &Value) -> Result<u32, String> {
        let width = value
            .as_u64()
            .filter(|width| Self::valid_shard_width(*width))
            .ok_or("fylo.shard.width must be an integer from 1 to 4")?;
        u32::try_from(width).map_err(|_| "fylo.shard.width is out of range".into())
    }

    fn valid_shard_width(width: u64) -> bool {
        (Self::MINIMUM_SHARD_WIDTH..=Self::MAXIMUM_SHARD_WIDTH).contains(&width)
    }

    fn path_setting(
        value: Option<&Value>,
        name: &str,
        directory: &Path,
        secrets: &mut Secrets,
    ) -> Result<Option<PathBuf>, String> {
        value
            .map(|value| Self::resolve_path(value, name, directory, secrets))
            .transpose()
    }

    fn resolve_path(
        value: &Value,
        name: &str,
        directory: &Path,
        secrets: &mut Secrets,
    ) -> Result<PathBuf, String> {
        let source = value.as_str().ok_or_else(|| Self::string_error(name))?;
        let resolved = secrets.resolve(source)?;
        Self::require_path_content(&resolved, name)?;
        Self::require_path_without_null(&resolved, name)?;
        Ok(directory.join(resolved))
    }

    fn string_error(name: &str) -> String {
        if name == "fylo.schema" {
            return "fylo.schema must be a string or an object with a path".into();
        }
        format!("{name} must be a string")
    }

    fn require_path_content(value: &str, name: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err(Self::path_error(name));
        }
        Ok(())
    }

    fn require_path_without_null(value: &str, name: &str) -> Result<(), String> {
        if value.contains('\0') {
            return Err(Self::path_error(name));
        }
        Ok(())
    }

    fn path_error(name: &str) -> String {
        format!("{name} {}", Self::PATH_ERROR)
    }

    fn secret_setting(
        value: Option<&Value>,
        name: &str,
        secrets: &mut Secrets,
    ) -> Result<Option<String>, String> {
        value
            .map(|value| Self::resolve_secret(value, name, secrets))
            .transpose()
    }

    fn resolve_secret(value: &Value, name: &str, secrets: &mut Secrets) -> Result<String, String> {
        let source = value
            .as_str()
            .ok_or_else(|| format!("{name} must reference a secret"))?;
        let reference = SecretReference::parse(source)?;
        Self::require_reference(reference, name)?;
        secrets.resolve(source)
    }

    fn require_reference(reference: Option<&str>, name: &str) -> Result<(), String> {
        if reference.is_none() {
            return Err(format!(
                "{name} must use a ${{NAME}} secret reference, not a literal credential"
            ));
        }
        Ok(())
    }

    fn credentials(
        secret: Option<String>,
        salt: Option<String>,
    ) -> Result<Option<(String, String)>, String> {
        match (secret, salt) {
            (None, None) => Ok(None),
            (Some(secret), Some(salt)) => Self::checked_credentials(secret, salt).map(Some),
            _ => Err(Self::CREDENTIAL_ERROR.into()),
        }
    }

    fn checked_credentials(secret: String, salt: String) -> Result<(String, String), String> {
        Self::require_key_length(&secret)?;
        Self::require_salt_content(&salt)?;
        Ok((secret, salt))
    }

    fn require_key_length(secret: &str) -> Result<(), String> {
        if secret.len() < Self::MINIMUM_KEY_BYTES {
            return Err(Self::CREDENTIAL_ERROR.into());
        }
        Ok(())
    }

    fn require_salt_content(salt: &str) -> Result<(), String> {
        if salt.is_empty() {
            return Err(Self::CREDENTIAL_ERROR.into());
        }
        Ok(())
    }
}
