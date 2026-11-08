/// A half-open run of encoded prefix-index keys: `start <= key < end`.
///
/// An empty `end` has no upper bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexKeyRange {
    start: Vec<u8>,
    end: Vec<u8>,
}

impl IndexKeyRange {
    /// Every key that starts with `prefix`.
    #[must_use]
    pub fn prefix(prefix: &str) -> Self {
        let start = prefix.as_bytes().to_vec();
        let end = Self::first_key_after_prefix(&start);
        Self { start, end }
    }

    /// Every key from `start` up to, but not including, `end`.
    #[must_use]
    pub fn new(start: Vec<u8>, end: Vec<u8>) -> Self {
        Self { start, end }
    }

    /// The first key in the range.
    #[must_use]
    pub fn start(&self) -> &[u8] {
        &self.start
    }

    /// The first key after the range, or empty when it has no end.
    #[must_use]
    pub fn end(&self) -> &[u8] {
        &self.end
    }

    /// Whether `key` lies in the range.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let after_start = key >= self.start.as_slice();
        let before_end = self.is_before_end(key);
        after_start & before_end
    }

    /// Whether `key` sorts before the end of the range.
    #[must_use]
    pub fn is_before_end(&self, key: &[u8]) -> bool {
        if self.end.is_empty() {
            return true;
        }
        key < self.end.as_slice()
    }

    fn first_key_after_prefix(prefix: &[u8]) -> Vec<u8> {
        let mut end = prefix.to_vec();
        Self::increment_last_byte(&mut end);
        end
    }

    fn increment_last_byte(bytes: &mut [u8]) {
        if let Some(last) = bytes.last_mut() {
            *last += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IndexKeyRange;

    #[test]
    fn a_prefix_range_holds_exactly_the_keys_with_that_prefix() {
        let range = IndexKeyRange::prefix("age/n/");
        assert!(range.contains(b"age/n/"));
        assert!(range.contains(b"age/n/c05/4V0001"));
        assert!(!range.contains(b"age/n"));
        assert!(!range.contains(b"age/nr/c05/4V0001"));
        assert!(!range.contains(b"age/o"));
    }

    #[test]
    fn an_empty_prefix_holds_every_key() {
        let range = IndexKeyRange::prefix("");
        assert!(range.contains(b""));
        assert!(range.contains(&[0xFF, 0xFF]));
    }

    #[test]
    fn a_range_includes_its_start_and_excludes_its_end() {
        let range = IndexKeyRange::new(b"b".to_vec(), b"d".to_vec());
        assert!(range.contains(b"b"));
        assert!(range.contains(b"c/zzz"));
        assert!(!range.contains(b"d"));
        assert!(!range.contains(b"a/zzz"));
    }
}
