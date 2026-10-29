use crate::ContainerFormatError;

/// What an allocated extent of a `.fxdb` container holds (ADR 0016 section 3).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContainerExtentKind {
    /// Collection blocks.
    Directory,
    /// ADR 0015 records of one collection.
    Segment,
    /// A compression dictionary of one collection.
    Dictionary,
    /// Part of a collection's location snapshot (derived).
    Locations,
    /// Part of a collection's prefix-index snapshot (derived).
    Index,
    /// Part of an extent-map copy (derived).
    Map,
}

impl ContainerExtentKind {
    /// The kind's one-byte code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Directory => 1,
            Self::Segment => 2,
            Self::Dictionary => 3,
            Self::Locations => 4,
            Self::Index => 5,
            Self::Map => 6,
        }
    }

    /// Decode a kind code.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for a code the format does not define.
    pub const fn from_code(code: u8) -> Result<Self, ContainerFormatError> {
        match code {
            1 => Ok(Self::Directory),
            2 => Ok(Self::Segment),
            3 => Ok(Self::Dictionary),
            4 => Ok(Self::Locations),
            5 => Ok(Self::Index),
            6 => Ok(Self::Map),
            _ => Err(ContainerFormatError::InvalidField),
        }
    }

    /// Whether extents of this kind are a source of truth rather than derived
    /// from segments. A source-of-truth extent is never freed while it is
    /// referenced.
    #[must_use]
    pub const fn is_source_of_truth(self) -> bool {
        matches!(self, Self::Directory | Self::Segment | Self::Dictionary)
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerExtentKind;

    #[test]
    fn codes_round_trip() {
        for kind in [
            ContainerExtentKind::Directory,
            ContainerExtentKind::Segment,
            ContainerExtentKind::Dictionary,
            ContainerExtentKind::Locations,
            ContainerExtentKind::Index,
            ContainerExtentKind::Map,
        ] {
            assert_eq!(ContainerExtentKind::from_code(kind.code()), Ok(kind));
        }
        assert!(ContainerExtentKind::from_code(0).is_err());
        assert!(ContainerExtentKind::from_code(7).is_err());
    }
}
