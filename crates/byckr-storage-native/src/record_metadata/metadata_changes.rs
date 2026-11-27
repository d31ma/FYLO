use std::collections::{BTreeMap, BTreeSet};

/// The metadata values one write sets and removes on a record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MetadataChanges {
    set: BTreeMap<String, Vec<u8>>,
    remove: BTreeSet<String>,
}

impl MetadataChanges {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(mut self, name: &str, value: &[u8]) -> Self {
        self.remove.remove(name);
        self.set.insert(name.to_owned(), value.to_vec());
        self
    }

    pub(crate) fn remove(mut self, name: &str) -> Self {
        self.set.remove(name);
        self.remove.insert(name.to_owned());
        self
    }

    /// The values after these changes, starting from `values`.
    pub(crate) fn applied_to(
        &self,
        mut values: BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, Vec<u8>> {
        self.drop_removed(&mut values);
        values.extend(self.set.clone());
        values
    }

    fn drop_removed(&self, values: &mut BTreeMap<String, Vec<u8>>) {
        for name in &self.remove {
            values.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::MetadataChanges;

    fn values(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.to_vec()))
            .collect()
    }

    #[test]
    fn set_adds_and_replaces_values() {
        let changes = MetadataChanges::new().set("a", b"2").set("b", b"3");

        let after = changes.applied_to(values(&[("a", b"1")]));

        assert_eq!(after, values(&[("a", b"2"), ("b", b"3")]));
    }

    #[test]
    fn remove_drops_a_value() {
        let changes = MetadataChanges::new().remove("a");

        let after = changes.applied_to(values(&[("a", b"1"), ("b", b"2")]));

        assert_eq!(after, values(&[("b", b"2")]));
    }

    #[test]
    fn the_last_change_to_a_name_wins() {
        let changes = MetadataChanges::new().set("a", b"2").remove("a");

        let after = changes.applied_to(values(&[("a", b"1")]));

        assert_eq!(after, values(&[]));
    }

    #[test]
    fn a_set_after_a_remove_keeps_the_value() {
        let changes = MetadataChanges::new().remove("a").set("a", b"3");

        let after = changes.applied_to(values(&[("a", b"1")]));

        assert_eq!(after, values(&[("a", b"3")]));
    }
}
