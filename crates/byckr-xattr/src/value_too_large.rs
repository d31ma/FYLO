use std::io::{self, ErrorKind};

/// Recognizes the errors a file system gives when an attribute value, or a
/// file's attributes together, do not fit: `E2BIG`, `ENOSPC` and `ERANGE`.
/// Plain ext4 keeps all of a file's attributes in one 4 KiB block, so a
/// larger value fails there. BYCKR then keeps the value in the record's
/// sidecar file (ADR 0033 section 6).
pub struct ValueTooLarge;

impl ValueTooLarge {
    const OUT_OF_RANGE: i32 = 34;

    /// Whether `error` means the value did not fit.
    pub fn matches(error: &io::Error) -> bool {
        Self::kind_or_code_matches(error.kind(), error.raw_os_error())
    }

    fn kind_or_code_matches(kind: ErrorKind, code: Option<i32>) -> bool {
        matches!(
            (kind, code),
            (ErrorKind::ArgumentListTooLong | ErrorKind::StorageFull, _)
                | (_, Some(Self::OUT_OF_RANGE))
        )
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::ValueTooLarge;

    #[test]
    fn an_argument_list_too_long_error_matches() {
        assert!(ValueTooLarge::matches(&io::Error::from_raw_os_error(7)));
    }

    #[test]
    fn a_no_space_error_matches() {
        assert!(ValueTooLarge::matches(&io::Error::from_raw_os_error(28)));
    }

    #[test]
    fn an_out_of_range_error_matches() {
        assert!(ValueTooLarge::matches(&io::Error::from_raw_os_error(34)));
    }

    #[test]
    fn a_permission_error_does_not_match() {
        assert!(!ValueTooLarge::matches(&io::Error::from_raw_os_error(13)));
    }

    #[test]
    fn a_not_supported_error_does_not_match() {
        assert!(!ValueTooLarge::matches(&io::Error::from(
            io::ErrorKind::Unsupported
        )));
    }
}
