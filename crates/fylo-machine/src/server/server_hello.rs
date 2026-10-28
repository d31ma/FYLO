use serde_json::{Value, json};

use crate::{MachineError, ServeToken, StrictValue};

/// The first frame on a connection: `{"fyloServe":1,"token":"..."}`.
///
/// It belongs to the transport, not to the machine protocol, so the machine
/// frames that follow are exactly the ones a local session reads. A hello
/// with `"shutdown":true` asks the server to stop gracefully.
pub(crate) struct ServerHello {
    shutdown: bool,
}

impl ServerHello {
    pub(crate) const TRANSPORT_VERSION: u64 = 1;
    pub(crate) const MAX_BYTES: usize = 4096;
    const AUTH_FAILED: &'static str = "EAUTH";
    const BAD_HELLO: &'static str = "EBADHELLO";

    /// Parse the hello frame and check its token.
    pub(crate) fn admit(frame: &[u8], token: &ServeToken) -> Result<Self, MachineError> {
        let hello = Self::parse(frame)?;
        Self::require_version(&hello)?;
        Self::require_token(&hello, token)?;
        Ok(Self {
            shutdown: hello.get("shutdown") == Some(&Value::Bool(true)),
        })
    }

    pub(crate) const fn wants_shutdown(&self) -> bool {
        self.shutdown
    }

    pub(crate) fn accepted() -> Vec<u8> {
        Self::line(&json!({ "fyloServe": Self::TRANSPORT_VERSION, "ok": true }))
    }

    pub(crate) fn refused(error: &MachineError) -> Vec<u8> {
        Self::line(&json!({
            "fyloServe": Self::TRANSPORT_VERSION,
            "ok": false,
            "error": error,
        }))
    }

    fn parse(frame: &[u8]) -> Result<Value, MachineError> {
        serde_json::from_slice::<StrictValue>(frame)
            .map(StrictValue::into_inner)
            .map_err(|_| MachineError::new(Self::BAD_HELLO, "the hello frame is not JSON"))
    }

    fn require_version(hello: &Value) -> Result<(), MachineError> {
        if hello.get("fyloServe").and_then(Value::as_u64) == Some(Self::TRANSPORT_VERSION) {
            return Ok(());
        }
        Err(MachineError::new(
            Self::BAD_HELLO,
            "the hello frame must carry \"fyloServe\":1",
        ))
    }

    fn require_token(hello: &Value, token: &ServeToken) -> Result<(), MachineError> {
        let candidate = hello.get("token").and_then(Value::as_str).unwrap_or("");
        if token.matches(candidate.as_bytes()) {
            return Ok(());
        }
        Err(MachineError::new(
            Self::AUTH_FAILED,
            "the serve token was missing or wrong",
        ))
    }

    fn line(value: &Value) -> Vec<u8> {
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}
