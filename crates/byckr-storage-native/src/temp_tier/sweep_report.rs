/// What one sweep of a temp cache folder found and removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    /// Bytes of the entries left.
    pub used_bytes: u64,
    /// Entries left.
    pub entries: u64,
    /// Entries removed to bring the folder under its target.
    pub evicted: u64,
    /// Partial files removed because they were old.
    pub partials_removed: u64,
}

impl SweepReport {
    const MAGIC: &'static str = "fylo-sweep-v1";
    const MAXIMUM_TEXT_BYTES: usize = 128;

    /// The line `sweep.stamp` keeps, so a later process reads the last
    /// sweep's totals without listing the folder.
    pub(crate) fn to_line(self) -> String {
        format!(
            "{} {} {} {} {}\n",
            Self::MAGIC,
            self.used_bytes,
            self.entries,
            self.evicted,
            self.partials_removed
        )
    }

    /// The report a stamp line holds, or `None` for text that is not one.
    pub(crate) fn from_line(text: &str) -> Option<Self> {
        let fields = Self::fields(text)?;
        Some(Self {
            used_bytes: fields[0],
            entries: fields[1],
            evicted: fields[2],
            partials_removed: fields[3],
        })
    }

    pub(crate) const fn maximum_text_bytes() -> usize {
        Self::MAXIMUM_TEXT_BYTES
    }

    fn fields(text: &str) -> Option<[u64; 4]> {
        let mut words = text.split_ascii_whitespace();
        (words.next() == Some(Self::MAGIC)).then_some(())?;
        let numbers = words
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        numbers.try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::SweepReport;

    #[test]
    fn a_report_reads_back_from_its_stamp_line() {
        let report = SweepReport {
            used_bytes: 4096,
            entries: 3,
            evicted: 2,
            partials_removed: 1,
        };
        assert_eq!(SweepReport::from_line(&report.to_line()), Some(report));
    }

    #[test]
    fn other_text_is_no_report() {
        for text in [
            "",
            "fylo-sweep-v1 1 2",
            "other 1 2 3 4",
            "fylo-sweep-v1 1 2 3 x",
        ] {
            assert_eq!(SweepReport::from_line(text), None, "{text}");
        }
    }
}
