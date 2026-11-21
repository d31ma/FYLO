/// One chunk of a large object, as its sealing binds it: its number and
/// how many chunks the object has (ADR 0034 section 4).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerObjectChunk {
    /// The chunk's number, from zero.
    pub index: u64,
    /// The object's chunk count.
    pub count: u64,
}

impl ContainerObjectChunk {
    /// Whether this is the object's last chunk.
    #[must_use]
    pub fn is_last(&self) -> bool {
        self.index + 1 == self.count
    }
}
