/// One retained value with its accounted size and its clock reference bit.
pub(crate) struct ResidentEntry<T> {
    value: T,
    bytes: usize,
    referenced: bool,
}

impl<T: Clone> ResidentEntry<T> {
    pub(crate) const fn new(value: T, bytes: usize) -> Self {
        Self {
            value,
            bytes,
            referenced: false,
        }
    }

    /// The value, marking the entry as recently used.
    pub(crate) fn touch(&mut self) -> T {
        self.referenced = true;
        self.value.clone()
    }

    pub(crate) const fn value(&self) -> &T {
        &self.value
    }

    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Clear the reference bit and say whether it was set: the clock's
    /// second chance.
    pub(crate) const fn take_reference(&mut self) -> bool {
        let referenced = self.referenced;
        self.referenced = false;
        referenced
    }
}
