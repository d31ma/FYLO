/// The browser build has no application identities (ADR 0021, phase 5):
/// no value of this type exists, so a protected `.vdbx` database always
/// opens locked there.
#[derive(Clone, Debug)]
pub enum DatabaseIdentity {}
