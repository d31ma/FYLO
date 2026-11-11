/// Turns text into the normalized words of a word index (ADR 0023 section 2).
///
/// The JavaScript engine's `src/browser/core/word-text.js` implements the same
/// rules; `tests/fixtures/word-normalization-v1.json` keeps the two equal.
pub struct WordText;

impl WordText {
    /// Longest word kept, in UTF-8 bytes. Longer words keep their leading
    /// whole characters.
    pub const MAX_WORD_BYTES: usize = 64;
    /// Most terms one `$match` operand may hold.
    pub const MAX_TERMS: usize = 16;
    /// Folded forms of U+00C0 to U+017F. Digits stand for two letters (see
    /// [`Self::expansion`]); `-` marks the two code points that are not
    /// letters.
    const LATIN_FOLDS: &'static [u8; 192] = b"aaaaaa1ceeeeiiiidnooooo-ouuuuy23aaaaaa1ceeeeiiiidnooooo-ouuuuy2yaaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiii44jjkkkllllllllllnnnnnnnnnoooooo55rrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs";
    const FIRST_FOLDED: u32 = 0xC0;
    const FIRST_COMBINING_MARK: u32 = 0x300;
    const LAST_COMBINING_MARK: u32 = 0x36F;
    const FINAL_SIGMA: char = '\u{3C2}';
    const SIGMA: char = '\u{3C3}';

    /// The distinct words of `text`, in the order they first appear.
    #[must_use]
    pub fn words(text: &str) -> Vec<String> {
        let mut words: Vec<String> = Vec::new();
        let mut current = String::new();
        for character in text.chars() {
            Self::read_character(character, &mut current, &mut words);
        }
        Self::finish_word(&mut current, &mut words);
        words
    }

    /// Whether every term is the start of at least one word of `texts`.
    #[must_use]
    pub fn matches_terms<'a>(texts: impl Iterator<Item = &'a str>, terms: &[String]) -> bool {
        let words: Vec<String> = texts.flat_map(Self::words).collect();
        terms.iter().all(|term| Self::has_prefix(&words, term))
    }

    fn has_prefix(words: &[String], term: &str) -> bool {
        words.iter().any(|word| word.starts_with(term))
    }

    fn read_character(character: char, current: &mut String, words: &mut Vec<String>) {
        if Self::is_combining_mark(character) {
            return;
        }
        if !character.is_alphanumeric() {
            Self::finish_word(current, words);
            return;
        }
        for lower in character.to_lowercase() {
            Self::push_folded(lower, current);
        }
    }

    fn finish_word(current: &mut String, words: &mut Vec<String>) {
        let word = std::mem::take(current);
        if word.is_empty() || words.contains(&word) {
            return;
        }
        words.push(word);
    }

    fn push_folded(character: char, current: &mut String) {
        if Self::is_combining_mark(character) {
            return;
        }
        let folded = Self::fold(character);
        if current.len() + folded.len() > Self::MAX_WORD_BYTES {
            return;
        }
        current.push_str(&folded);
    }

    fn fold(character: char) -> String {
        if character == Self::FINAL_SIGMA {
            return Self::SIGMA.to_string();
        }
        let offset = u32::from(character).wrapping_sub(Self::FIRST_FOLDED);
        let Some(&folded) = usize::try_from(offset)
            .ok()
            .and_then(|offset| Self::LATIN_FOLDS.get(offset))
        else {
            return character.to_string();
        };
        Self::expansion(folded)
    }

    fn expansion(folded: u8) -> String {
        let pair = match folded {
            b'1' => "ae",
            b'2' => "th",
            b'3' => "ss",
            b'4' => "ij",
            b'5' => "oe",
            _ => return char::from(folded).to_string(),
        };
        pair.to_owned()
    }

    fn is_combining_mark(character: char) -> bool {
        (Self::FIRST_COMBINING_MARK..=Self::LAST_COMBINING_MARK).contains(&u32::from(character))
    }
}

#[cfg(test)]
mod tests {
    use super::WordText;

    #[test]
    fn splits_lowers_and_folds_latin_letters() {
        assert_eq!(
            WordText::words("Meeting with Tomás, café-crème; STRAẞE 2026!"),
            [
                "meeting", "with", "tomas", "cafe", "creme", "strasse", "2026"
            ]
        );
    }

    #[test]
    fn drops_combining_marks_inside_a_word() {
        assert_eq!(
            WordText::words("cafe\u{301} I\u{307}stanbul"),
            ["cafe", "istanbul"]
        );
    }

    #[test]
    fn keeps_each_word_once_in_first_order() {
        assert_eq!(WordText::words("b a B A"), ["b", "a"]);
    }

    #[test]
    fn maps_final_sigma_to_sigma() {
        assert_eq!(WordText::words("ΟΔΟΣ οδος"), ["οδοσ"]);
    }

    #[test]
    fn cuts_long_words_at_a_character_boundary() {
        let long = "é".repeat(70);
        let words = WordText::words(&long);
        assert_eq!(words, ["e".repeat(64)]);
        let wide = "日".repeat(30);
        assert_eq!(WordText::words(&wide)[0].len(), 63);
    }

    #[test]
    fn punctuation_only_text_has_no_words() {
        assert!(WordText::words(" -- !! ").is_empty());
    }

    #[test]
    fn every_term_must_start_some_word() {
        let terms = WordText::words("meet tom");
        assert!(WordText::matches_terms(
            ["Meeting with Tomás"].into_iter(),
            &terms
        ));
        assert!(!WordText::matches_terms(["Meeting"].into_iter(), &terms));
        assert!(WordText::matches_terms(
            ["Tomás", "meeting notes"].into_iter(),
            &terms
        ));
    }
}
