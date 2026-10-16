use std::str::Chars;

pub(super) struct SecretText<'a> {
    characters: Chars<'a>,
    quote: char,
    value: String,
    finished: bool,
}

impl<'a> SecretText<'a> {
    pub(super) fn parse(source: &'a str) -> Result<String, ()> {
        let quote = source
            .chars()
            .next()
            .filter(|character| Self::is_quote(*character));
        Self::read_value(source, quote)
    }

    fn is_quote(character: char) -> bool {
        matches!(character, '\'' | '"')
    }

    fn read_value(source: &'a str, quote: Option<char>) -> Result<String, ()> {
        match quote {
            None => Ok(source
                .split('#')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_owned()),
            Some(quote) => Self::read_quoted(source, quote),
        }
    }

    fn read_quoted(source: &'a str, quote: char) -> Result<String, ()> {
        let mut text = Self {
            characters: source[1..].chars(),
            quote,
            value: String::new(),
            finished: false,
        };
        text.read_characters()?;
        Ok(text.value)
    }

    fn read_characters(&mut self) -> Result<(), ()> {
        while !self.finished {
            self.read_character()?;
        }
        Ok(())
    }

    fn read_character(&mut self) -> Result<(), ()> {
        let character = self.characters.next().ok_or(())?;
        self.append_character(character)
    }

    fn append_character(&mut self, character: char) -> Result<(), ()> {
        match (character == self.quote, character, self.quote) {
            (true, _, _) => self.finish(),
            (false, '\\', '"') => self.append_escape(),
            _ => {
                self.value.push(character);
                Ok(())
            }
        }
    }

    fn finish(&mut self) -> Result<(), ()> {
        let tail = self.characters.as_str().trim();
        Self::require_comment_tail(tail)?;
        self.finished = true;
        Ok(())
    }

    fn require_comment_tail(tail: &str) -> Result<(), ()> {
        match tail.as_bytes().first() {
            None | Some(b'#') => Ok(()),
            _ => Err(()),
        }
    }

    fn append_escape(&mut self) -> Result<(), ()> {
        let character = self.characters.next().ok_or(())?;
        let (prefix, character) = Self::escape(character);
        self.value.extend(prefix);
        self.value.push(character);
        Ok(())
    }

    fn escape(character: char) -> (Option<char>, char) {
        match character {
            'n' => (None, '\n'),
            'r' => (None, '\r'),
            't' => (None, '\t'),
            '"' => (None, '"'),
            '\\' => (None, '\\'),
            other => (Some('\\'), other),
        }
    }
}
