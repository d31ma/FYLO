use std::fmt;

use serde::{Deserialize, Serialize};

use super::NameSubject;
use crate::NativeStorageError;

/// The name of a namespace: a group of collections inside an app, like a
/// Postgres schema (ADR 0033 section 3).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct NamespaceName(String);

impl TryFrom<String> for NamespaceName {
    type Error = NativeStorageError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::parse(&name)
    }
}

impl From<NamespaceName> for String {
    fn from(name: NamespaceName) -> Self {
        name.0
    }
}

impl NamespaceName {
    /// The namespace a request uses when it names none.
    pub const DEFAULT: &'static str = "public";

    /// Check a name against the name rule.
    ///
    /// # Errors
    ///
    /// Returns `ENAMESPACE` when the name breaks the rule.
    pub fn parse(name: &str) -> Result<Self, NativeStorageError> {
        NameSubject::NAMESPACE.check(name)?;
        Ok(Self(name.to_owned()))
    }

    /// The default namespace, `public`.
    #[must_use]
    pub fn public() -> Self {
        Self(Self::DEFAULT.to_owned())
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NamespaceName {
    fn default() -> Self {
        Self::public()
    }
}

impl fmt::Display for NamespaceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::NamespaceName;

    #[test]
    fn the_default_namespace_is_public() {
        assert_eq!(NamespaceName::public().as_str(), "public");
        assert_eq!(NamespaceName::default(), NamespaceName::public());
        assert_eq!(
            NamespaceName::parse("public").unwrap(),
            NamespaceName::public()
        );
    }

    #[test]
    fn a_slice_namespace_is_valid() {
        let name = NamespaceName::parse("slice-12").unwrap();
        assert_eq!(name.to_string(), "slice-12");
    }

    #[test]
    fn a_bad_namespace_is_refused_with_enamespace() {
        for name in [
            "", "x", "Billing", "a.b", "../up", ".fylo", "-a", "lpt1", "sql",
        ] {
            let error = NamespaceName::parse(name).unwrap_err();
            assert_eq!(error.code().as_str(), "ENAMESPACE", "{name}");
        }
    }
}
