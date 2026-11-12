use serde_json::Value;

use crate::call_failure::CallFailure;
use crate::fylo_status::FyloStatus;

/// What one call produced, before it crosses the C boundary.
#[derive(Debug)]
pub(crate) enum CallOutcome {
    Done(Value),
    NotFound,
    Failed(CallFailure),
    Invalid(CallFailure),
    Closed,
    Panicked,
}

impl CallOutcome {
    const CLOSED: &'static str = "EROOTCLOSED";
    const PANICKED: &'static str = "EPANIC";

    pub(crate) fn from_result(result: Result<Value, CallFailure>) -> CallOutcome {
        match result {
            Ok(value) => CallOutcome::Done(value),
            Err(failure) => CallOutcome::failed(failure),
        }
    }

    /// A request FYLO could not read is the caller's mistake
    /// (`FYLO_INVALID_ARGUMENT`); anything else is FYLO's answer.
    pub(crate) fn failed(failure: CallFailure) -> CallOutcome {
        if failure.is_bad_request() {
            return CallOutcome::Invalid(failure);
        }
        CallOutcome::Failed(failure)
    }

    pub(crate) fn status(&self) -> FyloStatus {
        match self {
            CallOutcome::Done(_) => FyloStatus::Ok,
            CallOutcome::NotFound => FyloStatus::NotFound,
            CallOutcome::Failed(_) => FyloStatus::Error,
            CallOutcome::Invalid(_) => FyloStatus::InvalidArgument,
            CallOutcome::Closed => FyloStatus::Closed,
            CallOutcome::Panicked => FyloStatus::Panic,
        }
    }

    /// The output buffer's bytes: the result, the error, or nothing.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        match self {
            CallOutcome::Done(value) => value.to_string().into_bytes(),
            CallOutcome::NotFound => Vec::new(),
            CallOutcome::Failed(failure) | CallOutcome::Invalid(failure) => {
                failure.to_json().to_string().into_bytes()
            }
            CallOutcome::Closed => Self::message(Self::CLOSED, "the root handle is closed"),
            CallOutcome::Panicked => Self::message(
                Self::PANICKED,
                "FYLO panicked; the root handle is dead: close it and open the root again",
            ),
        }
    }

    fn message(code: &str, message: &str) -> Vec<u8> {
        CallFailure::new(code, message)
            .to_json()
            .to_string()
            .into_bytes()
    }
}
