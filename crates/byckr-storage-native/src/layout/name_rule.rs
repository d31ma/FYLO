use super::NameProblem;

/// The one name rule for app, namespace and collection names (ADR 0033
/// section 2): lowercase `a-z`, digits and `-`, 2 to 64 bytes, a letter or
/// digit first and last, and no Windows device name or FYLO reserved word.
pub struct NameRule;

impl NameRule {
    /// Fewest bytes a name may have.
    pub const MIN_BYTES: usize = 2;
    /// Most bytes a name may have.
    pub const MAX_BYTES: usize = 64;
    /// What a valid name looks like, for error messages.
    pub const ADVICE: &'static str =
        "Use 2-64 lowercase letters, digits or '-', starting and ending with a letter or digit.";
    const DASH: char = '-';
    const WINDOWS_DEVICE_NAMES: [&'static str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];

    /// Check a name against the rule.
    ///
    /// # Errors
    ///
    /// Returns the first problem found, in this order: too short, too long,
    /// a character that is not allowed, a bad first or last character, a
    /// reserved name.
    pub fn check(name: &str) -> Result<(), NameProblem> {
        Self::check_not_too_short(name)?;
        Self::check_not_too_long(name)?;
        Self::check_characters(name)?;
        Self::check_ends(name)?;
        Self::check_not_reserved(name)
    }

    fn check_not_too_short(name: &str) -> Result<(), NameProblem> {
        if name.len() < Self::MIN_BYTES {
            return Err(NameProblem::TooShort);
        }
        Ok(())
    }

    fn check_not_too_long(name: &str) -> Result<(), NameProblem> {
        if name.len() > Self::MAX_BYTES {
            return Err(NameProblem::TooLong);
        }
        Ok(())
    }

    fn check_characters(name: &str) -> Result<(), NameProblem> {
        let first_bad = name.chars().find(|character| !Self::is_allowed(*character));
        first_bad.map_or(Ok(()), |character| {
            Err(NameProblem::CharacterNotAllowed(character))
        })
    }

    fn is_allowed(character: char) -> bool {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == Self::DASH
    }

    fn check_ends(name: &str) -> Result<(), NameProblem> {
        let ends = [name.chars().next(), name.chars().last()];
        let bad_end = ends.into_iter().flatten().find(|end| *end == Self::DASH);
        bad_end.map_or(Ok(()), |character| {
            Err(NameProblem::BadFirstOrLastCharacter(character))
        })
    }

    fn check_not_reserved(name: &str) -> Result<(), NameProblem> {
        if Self::is_reserved(name) {
            return Err(NameProblem::ReservedName);
        }
        Ok(())
    }

    fn is_reserved(name: &str) -> bool {
        Self::WINDOWS_DEVICE_NAMES.contains(&name) || crate::RESERVED_COLLECTIONS.contains(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::NameRule;
    use crate::layout::NameProblem;

    #[test]
    fn valid_names_pass() {
        for name in [
            "ab",
            "chat",
            "mail-in",
            "slice-12",
            "a1",
            "public",
            &"a".repeat(64),
        ] {
            assert_eq!(NameRule::check(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn short_names_fail_first() {
        assert_eq!(NameRule::check(""), Err(NameProblem::TooShort));
        assert_eq!(NameRule::check("a"), Err(NameProblem::TooShort));
        assert_eq!(NameRule::check("-"), Err(NameProblem::TooShort));
    }

    #[test]
    fn names_over_64_bytes_fail() {
        assert_eq!(NameRule::check(&"a".repeat(65)), Err(NameProblem::TooLong));
    }

    #[test]
    fn the_length_counts_bytes_not_characters() {
        let thirty_three_two_byte_characters = "\u{e9}".repeat(33);
        assert_eq!(
            NameRule::check(&thirty_three_two_byte_characters),
            Err(NameProblem::TooLong)
        );
    }

    #[test]
    fn the_first_bad_character_is_named() {
        assert_eq!(
            NameRule::check("Mail"),
            Err(NameProblem::CharacterNotAllowed('M'))
        );
        assert_eq!(
            NameRule::check("mail_in"),
            Err(NameProblem::CharacterNotAllowed('_'))
        );
        assert_eq!(
            NameRule::check(".tmpAbC123"),
            Err(NameProblem::CharacterNotAllowed('.'))
        );
        assert_eq!(
            NameRule::check("a b"),
            Err(NameProblem::CharacterNotAllowed(' '))
        );
        assert_eq!(
            NameRule::check("caf\u{e9}"),
            Err(NameProblem::CharacterNotAllowed('\u{e9}'))
        );
        assert_eq!(
            NameRule::check("a/b"),
            Err(NameProblem::CharacterNotAllowed('/'))
        );
    }

    #[test]
    fn a_dash_may_not_start_or_end_a_name() {
        assert_eq!(
            NameRule::check("-chat"),
            Err(NameProblem::BadFirstOrLastCharacter('-'))
        );
        assert_eq!(
            NameRule::check("chat-"),
            Err(NameProblem::BadFirstOrLastCharacter('-'))
        );
    }

    #[test]
    fn windows_device_names_are_reserved() {
        for name in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
            assert_eq!(
                NameRule::check(name),
                Err(NameProblem::ReservedName),
                "{name}"
            );
        }
    }

    #[test]
    fn similar_names_that_are_not_devices_pass() {
        for name in ["com0", "com10", "lpt0", "console", "null"] {
            assert_eq!(NameRule::check(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn byckr_reserved_words_are_reserved() {
        for name in [
            "sql", "as", "then", "db", "engine", "cache", "queue", "join",
        ] {
            assert_eq!(
                NameRule::check(name),
                Err(NameProblem::ReservedName),
                "{name}"
            );
        }
    }
}
