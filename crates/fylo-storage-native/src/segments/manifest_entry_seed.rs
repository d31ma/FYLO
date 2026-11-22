use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, Visitor};

use super::{EntryListSeed, SegmentEntry};

/// Reads a commit manifest keeping only its format and the entry at one
/// index, so a point read builds one entry, not every entry of its record.
/// The other fields are read and checked as the full decode checks them.
pub(crate) struct ManifestEntrySeed {
    index: usize,
}

impl ManifestEntrySeed {
    const FIELDS: [&'static str; 5] = [
        "format",
        "operation",
        "entries",
        "indexRemoved",
        "indexAdded",
    ];

    pub(crate) fn new(index: usize) -> Self {
        Self { index }
    }

    fn read_field<'de, A: MapAccess<'de>>(
        &self,
        map: &mut A,
        key: &str,
        fields: &mut (Option<String>, bool, bool, Option<SegmentEntry>),
    ) -> Result<(), A::Error> {
        match key {
            "format" => fields.0 = Some(map.next_value::<String>()?),
            "operation" => {
                map.next_value::<String>()?;
                fields.1 = true;
            }
            "entries" => {
                fields.3 = map.next_value_seed(EntryListSeed::new(self.index))?;
                fields.2 = true;
            }
            "indexRemoved" | "indexAdded" => {
                map.next_value::<Vec<String>>()?;
            }
            other => return Err(A::Error::unknown_field(other, &Self::FIELDS)),
        }
        Ok(())
    }

    fn require<T, E: Error>(value: Option<T>, field: &'static str) -> Result<T, E> {
        value.ok_or_else(|| E::missing_field(field))
    }
}

impl<'de> DeserializeSeed<'de> for ManifestEntrySeed {
    type Value = (String, Option<SegmentEntry>);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for ManifestEntrySeed {
    type Value = (String, Option<SegmentEntry>);

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a segment commit manifest")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = (None, false, false, None);
        while let Some(key) = map.next_key::<String>()? {
            self.read_field(&mut map, &key, &mut fields)?;
        }
        let (format, operation, entries, entry) = fields;
        Self::require(operation.then_some(()), "operation")?;
        Self::require(entries.then_some(()), "entries")?;
        Ok((Self::require(format, "format")?, entry))
    }
}
