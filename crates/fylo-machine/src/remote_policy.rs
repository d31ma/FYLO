//! What a network client may ask a served root to do.
//!
//! A local session runs as the caller, so it may name any root, schema
//! directory, file or URL the caller could reach anyway. A network client runs
//! as the server, so those fields would let it read or write the server's disk
//! and network. It gets the served root and inline file bytes only.

use std::path::Path;

use serde_json::Value;

use crate::MachineError;

/// Admission check for frames from a network client.
pub(crate) struct RemotePolicy;

impl RemotePolicy {
    const BAD_REQUEST: &'static str = "EBADREQUEST";
    const UNSUPPORTED: &'static str = "EUNSUPPORTEDOP";

    /// Refuse a frame that names another root or anything on the server host.
    pub(crate) fn check(request: &Value, served_root: Option<&Path>) -> Result<(), MachineError> {
        Self::check_root(request, served_root)?;
        Self::check_schema_directory(request)?;
        Self::check_operation(request)?;
        Self::check_file_output(request)?;
        Self::check_file_input(request)
    }

    fn check_root(request: &Value, served_root: Option<&Path>) -> Result<(), MachineError> {
        let Some(root) = request.get("root") else {
            return Ok(());
        };
        if root.as_str().map(Path::new) == served_root {
            return Ok(());
        }
        Err(MachineError::new(
            Self::BAD_REQUEST,
            "a served session cannot choose another FYLO root",
        ))
    }

    fn check_schema_directory(request: &Value) -> Result<(), MachineError> {
        if request.get("schemaDir").is_none() {
            return Ok(());
        }
        Err(MachineError::new(
            Self::BAD_REQUEST,
            "a served session cannot override the server's schema directory",
        ))
    }

    fn check_operation(request: &Value) -> Result<(), MachineError> {
        if Self::operation(request) != Some("importBulkData") {
            return Ok(());
        }
        Err(MachineError::new(
            Self::UNSUPPORTED,
            "importBulkData makes the server fetch a URL, so a served session cannot use it",
        ))
    }

    fn check_file_output(request: &Value) -> Result<(), MachineError> {
        let writes_host_file =
            Self::operation(request) == Some("getFileData") && request.get("path").is_some();
        if !writes_host_file {
            return Ok(());
        }
        Err(MachineError::new(
            Self::UNSUPPORTED,
            "a served session cannot write a file on the server; omit \"path\" to get base64",
        ))
    }

    fn check_file_input(request: &Value) -> Result<(), MachineError> {
        if request.get("file").is_none() {
            return Ok(());
        }
        Err(MachineError::new(
            Self::UNSUPPORTED,
            "a served session cannot read a server path or URL as file input",
        ))
    }

    fn operation(request: &Value) -> Option<&str> {
        request.get("op").and_then(Value::as_str)
    }
}
