/// Which part of a collection slot one clock entry stands for.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ResidentPart {
    /// The collection's kind, generation and encrypted fields.
    Header,
    /// The parsed index snapshot.
    Snapshot,
    /// Every live document identifier, for scans.
    Identifiers,
    /// One parsed document.
    Document(String),
}
