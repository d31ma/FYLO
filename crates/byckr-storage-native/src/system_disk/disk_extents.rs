/// What a Windows volume's disk-extent query means for the system-disk rule
/// (ADR 0032 section 2), kept apart from the Windows call so it is tested on
/// every OS.
///
/// A complete list names the physical disks under the volume. A volume
/// with more extents than the buffer holds (a spanned or striped dynamic
/// volume) answers `ERROR_MORE_DATA` with a partial list: BYCKR never
/// compares a partial list, so BYCKR cannot tell. Any other failure (a
/// volume with no disk extents) leaves the volume alone to decide.
pub(crate) struct DiskExtents;

impl DiskExtents {
    /// `ERROR_MORE_DATA`.
    pub(crate) const MORE_DATA: u32 = 234;

    /// The disks of the volume: `Ok(Some)` for a complete list, `Ok(None)`
    /// when the volume alone decides.
    ///
    /// # Errors
    ///
    /// Returns "could not tell" when the list is partial.
    pub(crate) fn outcome(
        succeeded: bool,
        error: u32,
        count: u32,
        disks: &[u32],
    ) -> Result<Option<Vec<u32>>, String> {
        if succeeded {
            return Self::complete(count, disks);
        }
        if error == Self::MORE_DATA {
            return Err(Self::partial(disks.len()));
        }
        Ok(None)
    }

    fn complete(count: u32, disks: &[u32]) -> Result<Option<Vec<u32>>, String> {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        if count > disks.len() {
            return Err(Self::partial(disks.len()));
        }
        Ok(Some(disks[..count].to_vec()))
    }

    fn partial(room: usize) -> String {
        format!("the volume spans more than the {room} disk extents BYCKR reads")
    }
}

#[cfg(test)]
mod tests {
    use super::DiskExtents;

    #[test]
    fn a_complete_list_names_its_disks() {
        assert_eq!(
            DiskExtents::outcome(true, 0, 2, &[0, 3, 0, 0]),
            Ok(Some(vec![0, 3]))
        );
    }

    #[test]
    fn a_count_above_the_buffer_is_partial() {
        let error = DiskExtents::outcome(true, 0, 65, &[0; 64]).unwrap_err();
        assert!(error.contains("more than the 64 disk extents"), "{error}");
    }

    #[test]
    fn more_data_is_partial_and_cannot_be_told() {
        let error = DiskExtents::outcome(false, DiskExtents::MORE_DATA, 64, &[1; 64]).unwrap_err();
        assert!(error.contains("spans more than"), "{error}");
    }

    #[test]
    fn another_failure_leaves_the_volume_alone_to_decide() {
        assert_eq!(DiskExtents::outcome(false, 1, 0, &[0; 64]), Ok(None));
    }
}
