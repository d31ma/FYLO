/// The whole disk of a macOS device (ADR 0032 section 2): the `diskN` part
/// of `/dev/diskNsM...`. For an APFS volume this is its APFS container,
/// the same answer `DiskArbitration`'s `DADiskCopyWholeDisk` gives.
pub(crate) struct BsdDiskName;

impl BsdDiskName {
    const DEVICE_FOLDER: &'static str = "/dev/";
    const DISK_PREFIX: &'static str = "disk";

    /// The whole disk of `device`, such as `disk3` for `/dev/disk3s1s1`.
    ///
    /// # Errors
    ///
    /// Returns a message when `device` is not a `/dev/disk...` device.
    pub(crate) fn whole_disk(device: &str) -> Result<String, String> {
        let name = device.strip_prefix(Self::DEVICE_FOLDER).unwrap_or(device);
        let number = name
            .strip_prefix(Self::DISK_PREFIX)
            .map(Self::leading_digits)
            .filter(|digits| !digits.is_empty())
            .ok_or_else(|| format!("{device} is not a disk device"))?;
        Ok(format!("{}{number}", Self::DISK_PREFIX))
    }

    fn leading_digits(text: &str) -> &str {
        let end = text
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(text.len());
        &text[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::BsdDiskName;

    #[test]
    fn the_boot_volume_and_the_data_volume_share_the_container() {
        assert_eq!(BsdDiskName::whole_disk("/dev/disk3s1s1").unwrap(), "disk3");
        assert_eq!(BsdDiskName::whole_disk("/dev/disk3s5").unwrap(), "disk3");
    }

    #[test]
    fn an_external_disk_is_another_disk() {
        assert_eq!(BsdDiskName::whole_disk("/dev/disk7s1").unwrap(), "disk7");
        assert_eq!(BsdDiskName::whole_disk("/dev/disk12s2").unwrap(), "disk12");
        assert_eq!(BsdDiskName::whole_disk("/dev/disk8").unwrap(), "disk8");
    }

    #[test]
    fn a_device_that_is_not_a_disk_cannot_be_told() {
        assert!(BsdDiskName::whole_disk("map auto_home").is_err());
        assert!(BsdDiskName::whole_disk("devfs").is_err());
        assert!(BsdDiskName::whole_disk("/dev/disk").is_err());
    }
}
