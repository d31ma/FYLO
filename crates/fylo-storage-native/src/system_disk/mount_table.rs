use std::path::{Path, PathBuf};

/// A Linux mount table, `/proc/self/mountinfo` (ADR 0032 section 2).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MountTable {
    mounts: Vec<Mount>,
}

/// One mount: its device number (`major:minor`, the filesystem's
/// superblock), where it is mounted, its filesystem type and its source.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Mount {
    pub(crate) device: String,
    pub(crate) location: PathBuf,
    pub(crate) filesystem: String,
    pub(crate) source: String,
}

impl MountTable {
    const SEPARATOR: &'static str = " - ";
    const DEVICE_FIELD: usize = 2;
    const MOUNT_POINT_FIELD: usize = 4;
    const OCTAL_DIGITS: usize = 3;
    const OCTAL_RADIX: u32 = 8;

    /// Read a mountinfo table; lines it cannot read are left out.
    pub(crate) fn parse(text: &str) -> Self {
        Self {
            mounts: text.lines().filter_map(Self::mount).collect(),
        }
    }

    /// The mount that holds the absolute, canonical `path`: the longest
    /// mount point that is a parent of it, and the last one listed when a
    /// mount point is mounted over (`max_by_key` keeps the last of equal
    /// keys).
    ///
    /// # Errors
    ///
    /// Returns a message when no mount holds `path`.
    pub(crate) fn holding(&self, path: &Path) -> Result<&Mount, String> {
        self.mounts
            .iter()
            .filter(|mount| path.starts_with(&mount.location))
            .max_by_key(|mount| mount.location.components().count())
            .ok_or_else(|| format!("no mount holds {}", path.display()))
    }

    fn mount(line: &str) -> Option<Mount> {
        let (fields, filesystem) = line.split_once(Self::SEPARATOR)?;
        let fields: Vec<&str> = fields.split(' ').collect();
        let mut filesystem = filesystem.split(' ');
        Some(Mount {
            device: (*fields.get(Self::DEVICE_FIELD)?).to_owned(),
            location: PathBuf::from(Self::unescape(fields.get(Self::MOUNT_POINT_FIELD)?)),
            filesystem: filesystem.next()?.to_owned(),
            source: filesystem.next()?.to_owned(),
        })
    }

    /// mountinfo writes a space, tab, newline and backslash as `\ooo`.
    fn unescape(field: &str) -> String {
        let mut text = String::new();
        let mut rest = field;
        while let Some(at) = rest.find('\\') {
            text.push_str(&rest[..at]);
            let (character, used) = Self::octal(&rest[at + 1..]);
            text.push(character);
            rest = &rest[at + 1 + used..];
        }
        text.push_str(rest);
        text
    }

    fn octal(rest: &str) -> (char, usize) {
        rest.get(..Self::OCTAL_DIGITS)
            .and_then(|digits| u32::from_str_radix(digits, Self::OCTAL_RADIX).ok())
            .and_then(char::from_u32)
            .map_or(('\\', 0), |character| (character, Self::OCTAL_DIGITS))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.mounts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::MountTable;
    use std::path::Path;

    const TABLE: &str = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
23 22 0:5 / /proc rw - proc proc rw
30 22 0:28 / /tmp rw,nosuid shared:5 - tmpfs tmpfs rw
40 22 8:17 / /mnt/data rw,relatime - ext4 /dev/sdb1 rw
41 22 8:2 /srv /mnt/bound rw,relatime - ext4 /dev/sda2 rw
42 22 8:17 / /mnt/my\\040disk rw - ext4 /dev/sdb1 rw
43 40 0:29 / /mnt/data rw - tmpfs tmpfs rw
";

    fn holding<'a>(table: &'a MountTable, path: &str) -> &'a super::Mount {
        table.holding(Path::new(path)).unwrap()
    }

    #[test]
    fn the_longest_mount_point_holds_a_path() {
        let table = MountTable::parse(TABLE);
        assert_eq!(table.len(), 7);
        assert_eq!(holding(&table, "/home/me").device, "8:2");
        assert_eq!(holding(&table, "/tmp/x").filesystem, "tmpfs");
        assert_eq!(holding(&table, "/mnt/bound/a").device, "8:2");
        assert_eq!(holding(&table, "/").source, "/dev/sda2");
    }

    #[test]
    fn a_mount_point_mounted_over_is_hidden() {
        let table = MountTable::parse(TABLE);
        assert_eq!(holding(&table, "/mnt/data/root").filesystem, "tmpfs");
    }

    #[test]
    fn escaped_locations_are_read() {
        let table = MountTable::parse(TABLE);
        assert_eq!(holding(&table, "/mnt/my disk/root").device, "8:17");
    }

    #[test]
    fn a_path_outside_every_mount_has_no_mount() {
        assert!(MountTable::parse("").holding(Path::new("/x")).is_err());
    }
}
