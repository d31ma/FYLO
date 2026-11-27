/// The direction of one `orderBy` entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Direction {
    /// Smaller values first.
    #[default]
    Ascending,
    /// Larger values first.
    Descending,
}

impl Direction {
    /// The wire name: `asc` or `desc`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ascending => "asc",
            Self::Descending => "desc",
        }
    }
}
