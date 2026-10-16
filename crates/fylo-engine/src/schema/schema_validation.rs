use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::chex::Chex;
use super::validation_request::ValidationRequest;

pub(super) struct SchemaValidation {
    binary: PathBuf,
    process: RefCell<Option<Chex>>,
}

impl SchemaValidation {
    pub(super) fn new() -> Self {
        Self {
            binary: PathBuf::from("chex"),
            process: RefCell::new(None),
        }
    }

    pub(super) fn set_binary(&mut self, binary: impl Into<PathBuf>) {
        self.binary = binary.into();
    }

    pub(super) fn validate(
        &self,
        schema: &str,
        data: &Map<String, Value>,
        directory: Option<&Path>,
    ) -> Result<Map<String, Value>, String> {
        let request = ValidationRequest::new(schema, data, directory);
        let response = self.request(&request)?;
        Self::require_success(&response)?;
        Self::result(&response)
    }

    fn request(&self, request: &ValidationRequest<'_>) -> Result<Value, String> {
        let mut process = self.process.borrow_mut();
        self.start_if_needed(&mut process)?;
        let outcome = process
            .as_mut()
            .ok_or_else(|| "CHEX process is unavailable".to_owned())?
            .request(request);
        Self::discard_failed_process(&mut process, outcome.is_err());
        outcome
    }

    fn start_if_needed(&self, process: &mut Option<Chex>) -> Result<(), String> {
        if process.is_none() {
            *process = Some(Chex::spawn(&self.binary)?);
        }
        Ok(())
    }

    fn discard_failed_process(process: &mut Option<Chex>, failed: bool) {
        if failed {
            *process = None;
        }
    }

    fn require_success(response: &Value) -> Result<(), String> {
        if response.get("ok") != Some(&Value::Bool(true)) {
            return Err(Self::error_message(response));
        }
        Ok(())
    }

    fn error_message(response: &Value) -> String {
        response
            .get("error")
            .and_then(Self::message)
            .and_then(Value::as_str)
            .unwrap_or("document does not match its schema")
            .to_owned()
    }

    fn message(error: &Value) -> Option<&Value> {
        error.get("message")
    }

    fn result(response: &Value) -> Result<Map<String, Value>, String> {
        response
            .get("result")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| "CHEX returned a non-object validation result".to_owned())
    }
}
