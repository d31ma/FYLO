use fylo::DatabaseKey;

use crate::call_failure::CallFailure;

/// The `key` argument of `fylo_open` (ADR 0026 section 9, the rules of ADR
/// 0025): empty for none, else the secret's bytes, at least 32 of them. The
/// bytes go straight into a [`DatabaseKey`], which wipes them when the last
/// handle that uses it closes. Error text names the rule, never the key or
/// its length.
pub(crate) struct DatabaseKeyArgument;

impl DatabaseKeyArgument {
    const MINIMUM_BYTES: usize = 32;
    const KEY_ERROR: &'static str = "EKEY";

    /// `None` for an empty key.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Option<DatabaseKey>, CallFailure> {
        if bytes.is_empty() {
            return Ok(None);
        }
        Self::checked(bytes).map(Some)
    }

    /// The failure for a database that needs a key and was given none.
    pub(crate) fn missing(rule: &str) -> CallFailure {
        CallFailure::new(Self::KEY_ERROR, rule)
    }

    fn checked(bytes: &[u8]) -> Result<DatabaseKey, CallFailure> {
        if bytes.len() < Self::MINIMUM_BYTES {
            return Err(CallFailure::new(
                Self::KEY_ERROR,
                "the encryption key must be at least 32 bytes",
            ));
        }
        Ok(DatabaseKey::new(bytes.to_vec()))
    }
}
