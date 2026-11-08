use serde::Serialize;

/// The records of one collection that carry the same POSTIX access
/// descriptor (`user.fylo.access`): one owner, group and mode.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordAccessGroup {
    /// The owner's user id.
    pub uid: u32,
    /// The group id.
    pub gid: u32,
    /// The permission bits, as an octal string such as `"0640"`.
    pub mode: String,
    /// How many records, live and deleted, carry this descriptor.
    pub records: usize,
}

impl RecordAccessGroup {
    /// The group of `records` records with owner `uid`, group `gid` and
    /// permission bits `mode`.
    #[must_use]
    pub fn new((uid, gid, mode): (u32, u32, u32), records: usize) -> Self {
        Self {
            uid,
            gid,
            mode: format!("{mode:04o}"),
            records,
        }
    }
}
