/// Limits applied before a document can enter query or storage code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimits {
    /// Maximum encoded JSON bytes.
    pub max_bytes: usize,
    /// Maximum nested object/array depth.
    pub max_depth: usize,
    /// Maximum total values visited.
    pub max_nodes: usize,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_nodes: Self::DEFAULT_MAX_NODES,
        }
    }
}

impl DocumentLimits {
    const BYTES_PER_KILOBYTE: usize = 1024;

    pub const DEFAULT_MAX_BYTES: usize = 16 * Self::BYTES_PER_KILOBYTE * Self::BYTES_PER_KILOBYTE;
    pub const DEFAULT_MAX_DEPTH: usize = 64;
    pub const DEFAULT_MAX_NODES: usize = 1_000_000;
}
