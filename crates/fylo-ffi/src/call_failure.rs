use fylo::IdentityError;
use serde_json::{Value, json};

/// A failed call: a stable code from `api/errors/v1.json` and a message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallFailure {
    code: String,
    message: String,
}

impl CallFailure {
    const BAD_REQUEST: &'static str = "EBADREQUEST";

    pub(crate) fn new(code: &str, message: impl Into<String>) -> CallFailure {
        CallFailure {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> CallFailure {
        CallFailure::new(Self::BAD_REQUEST, message)
    }

    pub(crate) fn is_bad_request(&self) -> bool {
        self.code == Self::BAD_REQUEST
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

impl From<fylo::Error> for CallFailure {
    fn from(error: fylo::Error) -> CallFailure {
        CallFailure::new(error.code(), error.message())
    }
}

impl From<IdentityError> for CallFailure {
    fn from(error: IdentityError) -> CallFailure {
        CallFailure::new(error.code(), error.to_string())
    }
}

impl From<serde_json::Error> for CallFailure {
    fn from(error: serde_json::Error) -> CallFailure {
        CallFailure::bad_request(format!("the JSON is not valid here: {error}"))
    }
}
