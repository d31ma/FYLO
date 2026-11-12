/// A byte buffer FYLO gives the caller. The caller reads `len` bytes at `ptr`
/// and gives the buffer back to `fylo_bytes_free` exactly once.
#[repr(C)]
#[derive(Debug)]
pub struct FyloBytes {
    /// The first byte, or null for an empty buffer.
    pub ptr: *mut u8,
    /// How many bytes the buffer holds.
    pub len: usize,
}

impl FyloBytes {
    /// A buffer with nothing in it, which needs no freeing.
    #[must_use]
    pub fn empty() -> FyloBytes {
        FyloBytes {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// Hand `bytes` to the caller. `fylo_bytes_free` rebuilds the boxed
    /// slice from the same length, so no capacity is ever guessed.
    #[must_use]
    pub fn from_vec(bytes: Vec<u8>) -> FyloBytes {
        let len = bytes.len();
        let ptr = Box::into_raw(bytes.into_boxed_slice()).cast::<u8>();
        FyloBytes { ptr, len }
    }
}
