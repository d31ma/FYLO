pub(super) struct SecretReference;

impl SecretReference {
    const PREFIX: &'static str = "${";

    pub(super) fn parse(source: &str) -> Result<Option<&str>, String> {
        Self::parse_value(source)
    }

    pub(super) fn valid_name(name: &str) -> bool {
        Self::check_name(name).is_some()
    }

    fn parse_value(source: &str) -> Result<Option<&str>, String> {
        if !source.contains(Self::PREFIX) {
            return Ok(None);
        }
        Self::complete_reference(source).map(Some)
    }

    fn complete_reference(source: &str) -> Result<&str, String> {
        source
            .strip_prefix(Self::PREFIX)
            .and_then(|value| value.strip_suffix('}'))
            .filter(|name| Self::valid_name(name))
            .ok_or_else(|| "secret references must be a complete ${NAME} value".into())
    }

    fn check_name(name: &str) -> Option<()> {
        let mut bytes = name.bytes();
        let first = bytes.next()?;
        Self::first_byte(first).then_some(())?;
        bytes.all(Self::remaining_byte).then_some(())
    }

    fn first_byte(byte: u8) -> bool {
        match byte {
            b'_' => true,
            byte => byte.is_ascii_alphabetic(),
        }
    }

    fn remaining_byte(byte: u8) -> bool {
        match byte {
            b'_' => true,
            byte => byte.is_ascii_alphanumeric(),
        }
    }
}
