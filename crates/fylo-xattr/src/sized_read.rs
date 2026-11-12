use std::io;

use rustix::io::Errno;

/// Reading a value whose size the system reports first (RUSTSEC-2026-0486).
///
/// The size comes from the `_query` call, never from a zero-length buffer.
/// A size of 0 answers empty bytes without a second call, so rustix never
/// gets a zero-length buffer. A value that grows between the two calls
/// (`ERANGE`) is read again, a bounded number of times.
pub(crate) struct SizedRead;

impl SizedRead {
    const ATTEMPTS: usize = 8;

    pub(crate) fn read(
        query: impl Fn() -> rustix::io::Result<usize>,
        read: impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
    ) -> io::Result<Vec<u8>> {
        for _ in 0..Self::ATTEMPTS {
            if let Some(bytes) = Self::attempt(&query, &read)? {
                return Ok(bytes);
            }
        }
        Err(io::Error::other(
            "an extended attribute kept changing size while it was read",
        ))
    }

    /// `None` for "no such attribute", which `get` reports as no value.
    pub(crate) fn absent_is_none(read: io::Result<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
        match read {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.raw_os_error() == Some(Errno::NOATTR.raw_os_error()) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn attempt(
        query: &impl Fn() -> rustix::io::Result<usize>,
        read: &impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
    ) -> io::Result<Option<Vec<u8>>> {
        let size = query()?;
        if size == 0 {
            return Ok(Some(Vec::new()));
        }
        Self::read_exactly(size, read)
    }

    fn read_exactly(
        size: usize,
        read: &impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut buffer = vec![0_u8; size];
        match read(&mut buffer) {
            Ok(length) => {
                buffer.truncate(length);
                Ok(Some(buffer))
            }
            Err(Errno::RANGE) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
