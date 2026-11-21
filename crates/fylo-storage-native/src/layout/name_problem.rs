/// Why a name breaks the name rule of app, namespace and collection names
/// (ADR 0033 section 2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameProblem {
    /// Shorter than 2 bytes.
    TooShort,
    /// Longer than 64 bytes.
    TooLong,
    /// Holds a character other than `a-z`, `0-9` or `-`.
    CharacterNotAllowed(char),
    /// Starts or ends with `-`.
    BadFirstOrLastCharacter(char),
    /// A Windows device name or a FYLO reserved word.
    ReservedName,
}

impl NameProblem {
    /// The reason, as the error message says it.
    #[must_use]
    pub fn reason(self) -> String {
        match self {
            Self::TooShort => "too short".to_owned(),
            Self::TooLong => "too long".to_owned(),
            Self::CharacterNotAllowed(character) => {
                format!("character {character:?} not allowed")
            }
            Self::BadFirstOrLastCharacter(character) => {
                format!("bad first or last character {character:?}")
            }
            Self::ReservedName => "reserved name".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NameProblem;

    #[test]
    fn each_problem_has_a_plain_reason() {
        assert_eq!(NameProblem::TooShort.reason(), "too short");
        assert_eq!(NameProblem::TooLong.reason(), "too long");
        assert_eq!(
            NameProblem::CharacterNotAllowed('M').reason(),
            "character 'M' not allowed"
        );
        assert_eq!(
            NameProblem::BadFirstOrLastCharacter('-').reason(),
            "bad first or last character '-'"
        );
        assert_eq!(NameProblem::ReservedName.reason(), "reserved name");
    }
}
