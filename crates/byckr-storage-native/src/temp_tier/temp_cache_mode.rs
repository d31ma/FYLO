/// `--temp-cache`: whether the temp-disk read tier is used (ADR 0030
/// section 4.1).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TempCacheMode {
    /// Nothing is read from or written to the temp folder.
    Off,
    /// BYCKR decides once, when a session opens its root.
    #[default]
    Auto,
    /// Any folder root uses the tier when its cache folder is safe.
    On,
}

impl TempCacheMode {
    const OFF: &'static str = "off";
    const AUTO: &'static str = "auto";
    const ON: &'static str = "on";

    /// The mode `text` names: `off`, `auto` or `on`.
    ///
    /// # Errors
    /// Names the accepted values for any other text.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::named(text).ok_or_else(|| Self::unknown(text))
    }

    /// The mode's name, as `--temp-cache` takes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => Self::OFF,
            Self::Auto => Self::AUTO,
            Self::On => Self::ON,
        }
    }

    fn named(text: &str) -> Option<Self> {
        match text {
            Self::OFF => Some(Self::Off),
            Self::AUTO => Some(Self::Auto),
            Self::ON => Some(Self::On),
            _ => None,
        }
    }

    fn unknown(text: &str) -> String {
        format!("temp cache mode must be off, auto or on, not {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::TempCacheMode;

    #[test]
    fn every_mode_reads_back_from_its_name() {
        for mode in [TempCacheMode::Off, TempCacheMode::Auto, TempCacheMode::On] {
            assert_eq!(TempCacheMode::parse(mode.as_str()), Ok(mode));
        }
    }

    #[test]
    fn the_default_mode_is_auto() {
        assert_eq!(TempCacheMode::default(), TempCacheMode::Auto);
    }

    #[test]
    fn an_unknown_mode_names_the_accepted_values() {
        let error = TempCacheMode::parse("always").unwrap_err();
        assert!(error.contains("off, auto or on"), "{error}");
    }
}
