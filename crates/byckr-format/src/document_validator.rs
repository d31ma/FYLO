use serde_json::{Map, Value};

use crate::{DocumentLimits, FormatError, FormatErrorCode};

pub(crate) struct DocumentValidator {
    limits: DocumentLimits,
    visited: usize,
}

impl DocumentValidator {
    pub(crate) const fn new(limits: DocumentLimits) -> Self {
        Self { limits, visited: 1 }
    }

    pub(crate) fn validate(&mut self, map: &Map<String, Value>) -> Result<(), FormatError> {
        self.validate_object(map, 1, None)
    }

    fn validate_object(
        &mut self,
        map: &Map<String, Value>,
        depth: usize,
        parent: Option<&str>,
    ) -> Result<(), FormatError> {
        self.check_depth(depth)?;
        for (field, value) in map {
            self.visit_node()?;
            let path = Self::field_path(parent, field);
            self.validate_value(value, depth + 1, &path)?;
        }
        Ok(())
    }

    fn field_path(parent: Option<&str>, field: &str) -> String {
        match parent {
            Some(prefix) => format!("{prefix}/{field}"),
            None => field.to_owned(),
        }
    }

    fn validate_value(
        &mut self,
        value: &Value,
        depth: usize,
        path: &str,
    ) -> Result<(), FormatError> {
        match value {
            Value::Object(map) => self.validate_object(map, depth, Some(path)),
            Value::Array(items) => self.validate_array(items, depth, path),
            _ => Ok(()),
        }
    }

    fn validate_array(
        &mut self,
        items: &[Value],
        depth: usize,
        path: &str,
    ) -> Result<(), FormatError> {
        self.check_depth(depth)?;
        for item in items {
            self.visit_node()?;
            Self::check_array_item(item, path)?;
        }
        Ok(())
    }

    fn check_array_item(item: &Value, path: &str) -> Result<(), FormatError> {
        match item {
            Value::Object(_) | Value::Array(_) => Err(FormatError::new(
                FormatErrorCode::ArrayOfObjects,
                format!(
                    "cannot index an array of objects at \"{path}\": store objects in their own collection and reference them by key"
                ),
            )),
            _ => Ok(()),
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), FormatError> {
        if depth > self.limits.max_depth {
            return Err(FormatError::new(
                FormatErrorCode::DocumentTooDeep,
                format!(
                    "document nesting depth exceeds limit of {}",
                    self.limits.max_depth
                ),
            ));
        }
        Ok(())
    }

    fn visit_node(&mut self) -> Result<(), FormatError> {
        self.visited = self.visited.saturating_add(1);
        if self.visited > self.limits.max_nodes {
            return Err(FormatError::new(
                FormatErrorCode::TooManyNodes,
                format!(
                    "document value count exceeds limit of {}",
                    self.limits.max_nodes
                ),
            ));
        }
        Ok(())
    }
}
