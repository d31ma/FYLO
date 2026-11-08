use std::collections::BTreeMap;

use super::secret_reference::SecretReference;
use super::secret_text::SecretText;

pub(super) struct SecretDeclarations {
    values: BTreeMap<String, String>,
}

impl SecretDeclarations {
    pub(super) fn parse(source: &str) -> Result<BTreeMap<String, String>, String> {
        let mut declarations = Self {
            values: BTreeMap::new(),
        };
        declarations.read_lines(source)?;
        Ok(declarations.values)
    }

    fn read_lines(&mut self, source: &str) -> Result<(), String> {
        for (index, line) in source.lines().enumerate() {
            self.read_line(index, line)?;
        }
        Ok(())
    }

    fn read_line(&mut self, index: usize, original: &str) -> Result<(), String> {
        let line = original.trim();
        let declaration = Self::declaration(line);
        declaration
            .map(|line| self.insert_line(index, line))
            .transpose()?;
        Ok(())
    }

    fn declaration(line: &str) -> Option<&str> {
        match line.as_bytes().first() {
            None | Some(b'#') => None,
            _ => Some(line),
        }
    }

    fn insert_line(&mut self, index: usize, line: &str) -> Result<(), String> {
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (name, source) = line.split_once('=').ok_or_else(|| Self::error(index))?;
        let name = name.trim();
        Self::require_name(name, index)?;
        self.require_unique(name, index)?;
        let value = SecretText::parse(source.trim()).map_err(|()| Self::error(index))?;
        self.values.insert(name.to_owned(), value);
        Ok(())
    }

    fn error(index: usize) -> String {
        format!("invalid .env secret declaration at line {}", index + 1)
    }

    fn require_name(name: &str, index: usize) -> Result<(), String> {
        let valid = SecretReference::valid_name(name);
        Self::require_valid_name(valid, index)
    }

    fn require_valid_name(valid: bool, index: usize) -> Result<(), String> {
        if !valid {
            return Err(Self::error(index));
        }
        Ok(())
    }

    fn require_unique(&self, name: &str, index: usize) -> Result<(), String> {
        if self.values.contains_key(name) {
            return Err(Self::error(index));
        }
        Ok(())
    }
}
