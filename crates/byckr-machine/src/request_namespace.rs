use byckr_storage_native::layout::NamespaceName;
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
    /// Returns `EBADREQUEST` when the field is not a string, `ENAMESPACE`
    /// when it breaks the name rule, and `ENAMESPACEUNSUPPORTED` for any
    /// namespace but `public` until namespace separation is on (slice L1-5).
    pub(crate) fn resolve(
        request: &Value,
        default: Option<&NamespaceName>,
    ) -> Result<NamespaceName, MachineError> {
        let namespace = match request.get(Self::FIELD) {
            None => default.cloned().unwrap_or_default(),
            Some(value) => Self::parse(value)?,
        };
        crate::NamespaceSeparation::require(&namespace)
            .map_err(|error| crate::storage_error(&error))?;
        Ok(namespace)
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
    use byckr_storage_native::layout::NamespaceName;
    use serde_json::json;
    use std::sync::Mutex;

    /// The separation switch is one process-wide flag, so the tests that
    /// toggle it must run one at a time.
    static SEPARATION: Mutex<()> = Mutex::new(());

    #[test]
    fn a_request_without_the_field_uses_public() {
        let namespace = RequestNamespace::resolve(&json!({"op": "getDoc"}), None).unwrap();
        assert_eq!(namespace, NamespaceName::public());
    }

    #[test]
    fn a_request_without_the_field_uses_the_session_default() {
        let _guard = SEPARATION.lock().unwrap();
        crate::NamespaceSeparation::enable_for_tests();
        let default = NamespaceName::parse("slice-12").unwrap();
        let namespace =
            RequestNamespace::resolve(&json!({"op": "getDoc"}), Some(&default)).unwrap();
        assert_eq!(namespace, default);
    }

    #[test]
    fn the_field_wins_over_the_session_default() {
        let _guard = SEPARATION.lock().unwrap();
        crate::NamespaceSeparation::enable_for_tests();
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

    #[test]
    fn another_namespace_is_refused_while_separation_is_off() {
        let _guard = SEPARATION.lock().unwrap();
        crate::NamespaceSeparation::disable_for_tests();
        let refused =
            RequestNamespace::resolve(&json!({"namespace": "tenant-b"}), None).unwrap_err();
        assert_eq!(refused.code, "ENAMESPACEUNSUPPORTED");
        assert!(refused.message.contains("not yet supported in this build"));
    }
}
