#[derive(Clone, Copy, Debug)]
pub(super) struct ScanBudget {
    pub(super) remaining: u64,
}

impl ScanBudget {
    pub(super) const fn new(bytes: u64) -> Self {
        Self { remaining: bytes }
    }
}
