use std::collections::BTreeMap;
use std::env::VarError;
use std::path::{Path, PathBuf};

use super::config_file::ConfigFile;
use super::secret_declarations::SecretDeclarations;
use super::secret_reference::SecretReference;

pub(super) struct Secrets {
    directory: PathBuf,
    values: Option<BTreeMap<String, String>>,
}

impl Secrets {
    pub(super) fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            values: None,
        }
    }

    pub(super) fn resolve(&mut self, source: &str) -> Result<String, String> {
        let name = SecretReference::parse(source)?;
        self.resolve_value(source, name)
    }

    fn resolve_value(&mut self, source: &str, name: Option<&str>) -> Result<String, String> {
        match name {
            None => Ok(source.to_owned()),
            Some(name) => self.referenced_value(name),
        }
    }

    fn referenced_value(&mut self, name: &str) -> Result<String, String> {
        let value = self.lookup(name)?;
        Self::require_content(&value, name)?;
        Self::require_without_null(&value, name)?;
        Self::require_size(&value, name)?;
        Ok(value)
    }

    fn lookup(&mut self, name: &str) -> Result<String, String> {
        match std::env::var(name) {
            Ok(value) => Ok(value),
            Err(VarError::NotUnicode(_)) => Err("referenced secret is not valid UTF-8".into()),
            Err(VarError::NotPresent) => self.file_value(name),
        }
    }

    fn file_value(&mut self, name: &str) -> Result<String, String> {
        self.load_values()?;
        self.values
            .as_ref()
            .and_then(|values| values.get(name))
            .cloned()
            .ok_or_else(|| format!("missing referenced secret {name}"))
    }

    fn load_values(&mut self) -> Result<(), String> {
        if self.values.is_none() {
            let source = ConfigFile::read(&self.directory.join(".env"), true)?.unwrap_or_default();
            self.values = Some(SecretDeclarations::parse(&source)?);
        }
        Ok(())
    }

    fn invalid_value(name: &str) -> String {
        format!("referenced secret {name} is empty, oversized, or contains NUL")
    }

    fn require_content(value: &str, name: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err(Self::invalid_value(name));
        }
        Ok(())
    }

    fn require_without_null(value: &str, name: &str) -> Result<(), String> {
        if value.contains('\0') {
            return Err(Self::invalid_value(name));
        }
        Ok(())
    }

    fn require_size(value: &str, name: &str) -> Result<(), String> {
        if value.len() as u64 > ConfigFile::MAX_BYTES {
            return Err(Self::invalid_value(name));
        }
        Ok(())
    }
}
