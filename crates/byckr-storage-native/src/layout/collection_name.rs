use std::fmt;

use super::NameSubject;
use crate::NativeStorageError;

/// The name of a collection in the layout of ADR 0033: unique inside its
/// namespace, at most 64 bytes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CollectionName(String);

impl CollectionName {
    /// Check a name against the name rule.
    ///
    /// # Errors
    ///
    /// Returns today's collection error code, `ENATIVE_COLLECTION`, when the
    /// name breaks the rule.
    pub fn parse(name: &str) -> Result<Self, NativeStorageError> {
        NameSubject::COLLECTION.check(name)?;
        Ok(Self(name.to_owned()))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CollectionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::CollectionName;

    #[test]
    fn a_valid_collection_name_passes() {
        assert_eq!(
            CollectionName::parse("invoices").unwrap().as_str(),
            "invoices"
        );
    }

    #[test]
    fn a_bad_collection_name_keeps_the_collection_error_code() {
        for name in ["Invoices", "x", &"a".repeat(65), "engine", ".fylo"] {
            let error = CollectionName::parse(name).unwrap_err();
            assert_eq!(error.code().as_str(), "ENATIVE_COLLECTION", "{name}");
        }
    }
}
