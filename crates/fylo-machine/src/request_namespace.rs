use fylo_storage_native::layout::NamespaceName;
use serde_json::Value;

use crate::MachineError;

/// The namespace a machine request names in its optional `"namespace"`
/// field (ADR 0033 section 3), or the session's default: the `--namespace`
/// of `fylo exec`, else `public`.
pub(crate) struct RequestNamespace;

impl RequestNamespace {
    const FIELD: &'static str = "namespace";

    /// The request's namespace.
    ///
    /// # Errors
    ///
    /// Returns `EBADREQUEST` when the field is not a string, and `ENAMESPACE`
    /// when it breaks the name rule.
    pub(crate) fn resolve(
        request: &Value,
        default: Option<&NamespaceName>,
    ) -> Result<NamespaceName, MachineError> {
        match request.get(Self::FIELD) {
            None => Ok(default.cloned().unwrap_or_default()),
            Some(value) => Self::parse(value),
        }
    }

    fn parse(value: &Value) -> Result<NamespaceName, MachineError> {
        let name = value.as_str().ok_or_else(|| {
            MachineError::new(
                "EBADREQUEST",
                "machine request field \"namespace\" must be a string",
            )
        })?;
        NamespaceName::parse(name).map_err(|error| crate::storage_error(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::RequestNamespace;
    use fylo_storage_native::layout::NamespaceName;
    use serde_json::json;

    #[test]
    fn a_request_without_the_field_uses_public() {
        let namespace = RequestNamespace::resolve(&json!({"op": "getDoc"}), None).unwrap();
        assert_eq!(namespace, NamespaceName::public());
    }

    #[test]
    fn a_request_without_the_field_uses_the_session_default() {
        let default = NamespaceName::parse("slice-12").unwrap();
        let namespace =
            RequestNamespace::resolve(&json!({"op": "getDoc"}), Some(&default)).unwrap();
        assert_eq!(namespace, default);
    }

    #[test]
    fn the_field_wins_over_the_session_default() {
        let default = NamespaceName::parse("slice-12").unwrap();
        let request = json!({"op": "getDoc", "namespace": "billing"});
        let namespace = RequestNamespace::resolve(&request, Some(&default)).unwrap();
        assert_eq!(namespace.as_str(), "billing");
    }

    #[test]
    fn a_bad_name_is_refused_with_enamespace() {
        for name in ["", "Billing", "../x", "con", "a"] {
            let request = json!({"op": "getDoc", "namespace": name});
            let error = RequestNamespace::resolve(&request, None).unwrap_err();
            assert_eq!(error.code, "ENAMESPACE", "{name}");
        }
    }

    #[test]
    fn a_field_that_is_not_a_string_is_a_bad_request() {
        for value in [
            json!(null),
            json!(7),
            json!(["public"]),
            json!({"name": "public"}),
        ] {
            let request = json!({"op": "getDoc", "namespace": value});
            let error = RequestNamespace::resolve(&request, None).unwrap_err();
            assert_eq!(error.code, "EBADREQUEST");
        }
    }
}
