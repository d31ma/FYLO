use super::{NameProblem, NameRule};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// What a checked name names, and the error its refusal carries.
pub(crate) struct NameSubject {
    code: NativeStorageErrorCode,
    label: &'static str,
    noun: &'static str,
}

impl NameSubject {
    /// An app name: the root's folder name, refused with `EAPPNAME`.
    pub(crate) const APP: Self = Self {
        code: NativeStorageErrorCode::AppName,
        label: "root's folder name",
        noun: "app",
    };
    /// A namespace name, refused with `ENAMESPACE`.
    pub(crate) const NAMESPACE: Self = Self {
        code: NativeStorageErrorCode::Namespace,
        label: "namespace name",
        noun: "namespace",
    };
    /// A collection name, refused with today's collection code.
    pub(crate) const COLLECTION: Self = Self {
        code: NativeStorageErrorCode::InvalidCollection,
        label: "collection name",
        noun: "collection",
    };
    const ELLIPSIS: &'static str = "...";

    /// Check `name` against the name rule.
    ///
    /// # Errors
    ///
    /// Returns this subject's error code when the name breaks the rule.
    pub(crate) fn check(&self, name: &str) -> Result<(), NativeStorageError> {
        NameRule::check(name).map_err(|problem| self.error(name, problem))
    }

    fn error(&self, name: &str, problem: NameProblem) -> NativeStorageError {
        let message = format!(
            "the {} `{}` is not a valid FYLO {} name: {}. {}",
            self.label,
            Self::shown(name),
            self.noun,
            problem.reason(),
            NameRule::ADVICE
        );
        NativeStorageError::new(self.code, message)
    }

    fn shown(name: &str) -> String {
        let start: String = name.chars().take(NameRule::MAX_BYTES).collect();
        format!("{}{}", start.escape_debug(), Self::ellipsis(name))
    }

    fn ellipsis(name: &str) -> &'static str {
        if name.chars().count() > NameRule::MAX_BYTES {
            return Self::ELLIPSIS;
        }
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::NameSubject;
    use crate::NativeStorageErrorCode;

    #[test]
    fn an_app_refusal_follows_the_adr_wording() {
        let error = NameSubject::APP.check("Mail").unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::AppName);
        assert_eq!(error.code().as_str(), "EAPPNAME");
        assert_eq!(
            error.message(),
            "the root's folder name `Mail` is not a valid FYLO app name: character 'M' not \
             allowed. Use 2-64 lowercase letters, digits or '-', starting and ending with a \
             letter or digit."
        );
    }

    #[test]
    fn a_namespace_refusal_uses_its_own_code() {
        let error = NameSubject::NAMESPACE.check("con").unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Namespace);
        assert_eq!(error.code().as_str(), "ENAMESPACE");
        assert!(
            error.message().starts_with(
                "the namespace name `con` is not a valid FYLO namespace name: reserved name."
            ),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_long_name_is_cut_in_the_message() {
        let error = NameSubject::NAMESPACE.check(&"a".repeat(500)).unwrap_err();
        let shown = format!("`{}...`", "a".repeat(64));
        assert!(error.message().contains(&shown), "{}", error.message());
    }

    #[test]
    fn control_characters_are_escaped_in_the_message() {
        let error = NameSubject::NAMESPACE.check("a\nb").unwrap_err();
        assert!(error.message().contains("`a\\nb`"), "{}", error.message());
    }

    #[test]
    fn a_valid_name_passes() {
        assert!(NameSubject::APP.check("chat").is_ok());
        assert!(NameSubject::NAMESPACE.check("slice-12").is_ok());
    }
}
