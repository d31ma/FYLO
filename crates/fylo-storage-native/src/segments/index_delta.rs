/// The prefix-index changes a run of records makes, as index log lines,
/// for a medium that keeps no index log (`docs/vdbx-segment-backend.md`,
/// "Root integration").
///
/// A commit record lists the keys it removes and adds, so the index past a
/// snapshot is the snapshot with the lines of the records after it laid
/// over, as location rows are. Each line sets one key's presence, so laying
/// a line over a snapshot that already holds it changes nothing. The replay
/// itself is beside the location table's, in the write side's record
/// replay.
pub(crate) struct IndexDelta;
