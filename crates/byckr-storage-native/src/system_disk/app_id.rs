/// The id of an app that keeps FYLO data in its own data folder (ADR 0032
/// section 3.2): a bundle id or package name in reverse-DNS style, such as
/// `com.example.app`. It becomes part of a folder path, so it is checked
/// before any path is built: ASCII letters, digits, `.`, `-` and `_` only,
/// 1 to 255 bytes, no leading or trailing `.` and no empty part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppId(String);

impl AppId {
    const MAX_BYTES: usize = 255;
    const EXAMPLE: &'static str = "com.example.app";

    /// Check `text` and keep it as an app id.
    ///
    /// # Errors
    ///
    /// Returns a message that says which rule `text` breaks.
    pub fn parse(text: &str) -> Result<Self, String> {
        match Self::broken_rule(text) {
            Some(rule) => Err(format!(
                "app id {text:?} {rule}; use a bundle id or package name such as {}",
                Self::EXAMPLE
            )),
            None => Ok(Self(text.to_owned())),
        }
    }

    /// The id as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn broken_rule(text: &str) -> Option<&'static str> {
        if text.is_empty() || text.len() > Self::MAX_BYTES {
            return Some("must be 1 to 255 bytes");
        }
        if !text.bytes().all(Self::allowed_byte) {
            return Some("may hold only ASCII letters, digits, '.', '-' and '_'");
        }
        if text.starts_with('.') || text.ends_with('.') || text.contains("..") {
            return Some("may not start or end with '.' or hold an empty part");
        }
        None
    }

    const fn allowed_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
    }
}

#[cfg(test)]
mod tests {
    use super::AppId;

    fn refused(text: &str) -> String {
        AppId::parse(text).unwrap_err()
    }

    #[test]
    fn reverse_dns_ids_are_accepted() {
        assert_eq!(
            AppId::parse("com.example.app").unwrap().as_str(),
            "com.example.app"
        );
        AppId::parse("com.example-app_2").unwrap();
        AppId::parse(&"a".repeat(255)).unwrap();
    }

    #[test]
    fn a_parent_folder_step_is_refused() {
        assert!(refused("../x").contains("only ASCII letters"));
        assert!(refused("a/../b").contains("only ASCII letters"));
        assert!(refused("..").contains("empty part"));
        assert!(refused("a..b").contains("empty part"));
    }

    #[test]
    fn folder_separators_are_refused() {
        assert!(refused("a/b").contains("only ASCII letters"));
        assert!(refused("a\\b").contains("only ASCII letters"));
    }

    #[test]
    fn leading_and_trailing_dots_are_refused() {
        assert!(refused(".hidden").contains("start or end"));
        assert!(refused("a.").contains("start or end"));
    }

    #[test]
    fn the_length_is_bounded() {
        assert!(refused("").contains("1 to 255 bytes"));
        assert!(refused(&"a".repeat(256)).contains("1 to 255 bytes"));
    }

    #[test]
    fn non_ascii_is_refused() {
        assert!(refused("caf\u{e9}").contains("only ASCII letters"));
        assert!(refused("a b").contains("only ASCII letters"));
    }
}
