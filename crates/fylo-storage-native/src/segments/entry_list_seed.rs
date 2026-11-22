use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, SeqAccess, Visitor};

use super::SegmentEntry;

/// Reads a manifest's `entries` list, building only the entry at `index`
/// and skipping the others without building them.
pub(crate) struct EntryListSeed {
    index: usize,
}

impl EntryListSeed {
    pub(crate) fn new(index: usize) -> Self {
        Self { index }
    }

    fn read_one<'de, A: SeqAccess<'de>>(
        &self,
        list: &mut A,
        position: usize,
        found: &mut Option<SegmentEntry>,
    ) -> Result<bool, A::Error> {
        if position == self.index {
            *found = list.next_element::<SegmentEntry>()?;
            return Ok(found.is_some());
        }
        Ok(list.next_element::<IgnoredAny>()?.is_some())
    }
}

impl<'de> DeserializeSeed<'de> for EntryListSeed {
    type Value = Option<SegmentEntry>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for EntryListSeed {
    type Value = Option<SegmentEntry>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of segment entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut list: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        let mut position = 0;
        while self.read_one(&mut list, position, &mut found)? {
            position += 1;
        }
        Ok(found)
    }
}
