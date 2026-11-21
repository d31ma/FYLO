use super::{CollectionName, NamespaceName, RecordKind};
use crate::NativeStorageError;

/// Where one record is inside an app: its namespace, collection, id and kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordAddress {
    namespace: NamespaceName,
    collection: CollectionName,
    id: String,
    kind: RecordKind,
}

impl RecordAddress {
    /// The address of record `id`.
    ///
    /// # Errors
    ///
    /// Returns `EINVALIDDOCID` when `id` is not a FYLO record id (a TTID).
    pub fn new(
        namespace: NamespaceName,
        collection: CollectionName,
        id: &str,
        kind: RecordKind,
    ) -> Result<Self, NativeStorageError> {
        crate::validate_ttid_shape(id)?;
        Ok(Self {
            namespace,
            collection,
            id: id.to_owned(),
            kind,
        })
    }

    /// The record's namespace.
    #[must_use]
    pub fn namespace(&self) -> &NamespaceName {
        &self.namespace
    }

    /// The record's collection.
    #[must_use]
    pub fn collection(&self) -> &CollectionName {
        &self.collection
    }

    /// The record's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the record is a document or a file.
    #[must_use]
    pub fn kind(&self) -> &RecordKind {
        &self.kind
    }

    /// The record's file name, `<id>.<extension>`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.id, self.kind.extension())
    }
}

#[cfg(test)]
mod tests {
    use super::RecordAddress;
    use crate::layout::{CollectionName, NamespaceName, RecordKind};

    const ID: &str = "4UUB32VGUDW";

    fn address(id: &str, kind: RecordKind) -> Result<RecordAddress, crate::NativeStorageError> {
        RecordAddress::new(
            NamespaceName::public(),
            CollectionName::parse("users").unwrap(),
            id,
            kind,
        )
    }

    #[test]
    fn a_document_file_name_ends_in_json() {
        let address = address(ID, RecordKind::Document).unwrap();
        assert_eq!(address.file_name(), "4UUB32VGUDW.json");
        assert_eq!(address.id(), ID);
        assert_eq!(address.namespace().as_str(), "public");
        assert_eq!(address.collection().as_str(), "users");
        assert_eq!(address.kind(), &RecordKind::Document);
    }

    #[test]
    fn a_file_name_ends_in_its_extension() {
        let address = address(ID, RecordKind::file("png").unwrap()).unwrap();
        assert_eq!(address.file_name(), "4UUB32VGUDW.png");
    }

    #[test]
    fn an_id_that_is_not_a_ttid_is_refused() {
        for id in ["", "../x", "a/b", "not a ttid", &"A".repeat(40)] {
            let error = address(id, RecordKind::Document).unwrap_err();
            assert_eq!(error.code().as_str(), "EINVALIDDOCID", "{id}");
        }
    }
}
