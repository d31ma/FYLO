use fylo::Identity;
use serde_json::{Value, json};

use crate::call_failure::CallFailure;
use crate::call_outcome::CallOutcome;

/// `fylo_identity_create` and `fylo_identity_show`: phones have no CLI to
/// make an application identity (ADR 0021, ADR 0026).
pub(crate) struct IdentityCalls;

impl IdentityCalls {
    pub(crate) fn create(reference: &str) -> CallOutcome {
        CallOutcome::from_result(Self::created(reference))
    }

    pub(crate) fn show(reference: &str) -> CallOutcome {
        CallOutcome::from_result(Self::shown(reference))
    }

    fn created(reference: &str) -> Result<Value, CallFailure> {
        let reference = fylo::KeyReference::parse(reference)?;
        Ok(Self::describe(&Identity::create(&reference)?))
    }

    fn shown(reference: &str) -> Result<Value, CallFailure> {
        Ok(Self::describe(&Identity::from_reference(reference)?))
    }

    fn describe(identity: &Identity) -> Value {
        json!({
            "id": identity.id().to_string(),
            "publicKey": identity.public().to_string(),
        })
    }
}
