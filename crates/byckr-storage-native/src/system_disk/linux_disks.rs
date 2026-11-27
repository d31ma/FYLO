use std::fs;
use std::path::{Path, PathBuf};

use super::disk_description::DiskDescription;
use super::mount_table::{Mount, MountTable};

/// Describes the disk that holds a path on Linux (ADR 0032 section 2), from
/// a mount table and a sysfs folder, so tests can give both.
///
/// Keys: the mount's device number (`dev:8:2`), which catches bind mounts
/// and btrfs subvolumes of one filesystem; the whole disks below a block
/// device (`disk:sda`), found through partitions and the `slaves` of
/// device-mapper and md devices; and a ZFS dataset's pool (`zpool:tank`).
/// A loop device adds its backing file to `images`.
pub(crate) struct LinuxDisks<'a> {
    table: MountTable,
    sys: &'a Path,
}

impl<'a> LinuxDisks<'a> {
    const MEMORY_FILESYSTEMS: [&'static str; 2] = ["tmpfs", "ramfs"];
    const ZFS: &'static str = "zfs";
    const ANONYMOUS_MAJOR: &'static str = "0";
    const LOOP_PREFIX: &'static str = "loop";
    const DELETED_SUFFIX: &'static str = "(deleted)";
    const MAX_DEPTH: usize = 8;

    pub(crate) fn new(mountinfo: &str, sys: &'a Path) -> Self {
        Self {
            table: MountTable::parse(mountinfo),
            sys,
        }
    }

    /// The disk that holds the absolute, canonical `path`.
    ///
    /// # Errors
    ///
    /// Returns a message when no mount holds it, or a loop device's backing
    /// file is gone.
    pub(crate) fn describe(&self, path: &Path) -> Result<DiskDescription, String> {
        let mount = self.table.holding(path)?;
        let mut description = DiskDescription::named(&format!("dev:{}", mount.device));
        description.label = Self::label(mount);
        description.memory = Self::MEMORY_FILESYSTEMS.contains(&mount.filesystem.as_str());
        description.keys.extend(Self::pool(mount));
        let disks = self.whole_disks(mount);
        self.add_disks(&mut description, &disks)?;
        Ok(description)
    }

    fn label(mount: &Mount) -> String {
        format!(
            "{} {} on {}",
            mount.device,
            mount.source,
            mount.location.display()
        )
    }

    fn pool(mount: &Mount) -> Option<String> {
        (mount.filesystem == Self::ZFS)
            .then(|| mount.source.split('/').next().unwrap_or_default())
            .map(|pool| format!("zpool:{pool}"))
    }

    fn add_disks(&self, description: &mut DiskDescription, disks: &[String]) -> Result<(), String> {
        for disk in disks {
            description.keys.push(format!("disk:{disk}"));
            if let Some(image) = self.loop_backing(disk)? {
                description.images.push(image);
            }
        }
        Ok(())
    }

    /// The whole disks below the mount's block device; none when it has no
    /// block device (overlay, tmpfs, NFS) or sysfs does not show it.
    fn whole_disks(&self, mount: &Mount) -> Vec<String> {
        let mut disks = Vec::new();
        if let Some(folder) = self.block_folder(mount) {
            self.collect_disks(&folder, 0, &mut disks);
        }
        disks
    }

    /// `/sys/dev/block/<major:minor>`, for a block filesystem. A filesystem
    /// with an anonymous device (btrfs) is found through its source device.
    fn block_folder(&self, mount: &Mount) -> Option<PathBuf> {
        let device = Self::block_device(mount)?;
        fs::canonicalize(self.sys.join("dev/block").join(device)).ok()
    }

    fn block_device(mount: &Mount) -> Option<String> {
        let (major, _) = mount.device.split_once(':')?;
        if major != Self::ANONYMOUS_MAJOR {
            return Some(mount.device.clone());
        }
        Self::source_device(&mount.source)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn source_device(source: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let source = source.starts_with("/dev/").then_some(source)?;
        let device = fs::metadata(source).ok()?.rdev();
        Some(format!(
            "{}:{}",
            rustix::fs::major(device),
            rustix::fs::minor(device)
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn source_device(_source: &str) -> Option<String> {
        None
    }

    /// A partition's disk is its parent folder; a device-mapper or md
    /// device's disks are its `slaves`, followed down; anything else is a
    /// whole disk.
    fn collect_disks(&self, folder: &Path, depth: usize, disks: &mut Vec<String>) {
        if depth > Self::MAX_DEPTH {
            return;
        }
        if folder.join("partition").exists() {
            disks.extend(folder.parent().and_then(Self::name));
            return;
        }
        let slaves = Self::slaves(folder);
        if slaves.is_empty() {
            disks.extend(Self::name(folder));
            return;
        }
        for slave in slaves {
            self.collect_slave(&slave, depth, disks);
        }
    }

    fn collect_slave(&self, slave: &str, depth: usize, disks: &mut Vec<String>) {
        if let Ok(folder) = fs::canonicalize(self.sys.join("class/block").join(slave)) {
            self.collect_disks(&folder, depth + 1, disks);
        }
    }

    fn slaves(folder: &Path) -> Vec<String> {
        fs::read_dir(folder.join("slaves"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn name(folder: &Path) -> Option<String> {
        folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }

    /// A loop device's backing file, from `/sys/block/loopN/loop/backing_file`.
    fn loop_backing(&self, disk: &str) -> Result<Option<PathBuf>, String> {
        if !disk.starts_with(Self::LOOP_PREFIX) {
            return Ok(None);
        }
        let file = self.sys.join("block").join(disk).join("loop/backing_file");
        let text = fs::read_to_string(&file)
            .map_err(|error| format!("the file behind {disk} cannot be read ({error})"))?;
        Self::backing_path(disk, text.trim_end())
    }

    fn backing_path(disk: &str, text: &str) -> Result<Option<PathBuf>, String> {
        if text.is_empty() || text.ends_with(Self::DELETED_SUFFIX) {
            return Err(format!("the file behind {disk} is gone"));
        }
        Ok(Some(PathBuf::from(text)))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::LinuxDisks;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    /// A small sysfs: `sda` with partitions, `sdb` whole, `dm-0` over
    /// `sdc1` (LVM over dm-crypt in one step), and `loop0` over a file.
    fn sysfs(root: &Path) {
        let devices = root.join("devices");
        let disk = |name: &str| devices.join(name);
        for folder in [
            disk("sda/sda1"),
            disk("sda/sda2"),
            disk("sdb"),
            disk("sdc/sdc1"),
            disk("dm-0/slaves"),
            disk("loop0/loop"),
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(disk("sda/sda1/partition"), "1").unwrap();
        fs::write(disk("sda/sda2/partition"), "2").unwrap();
        fs::write(disk("sdc/sdc1/partition"), "1").unwrap();
        fs::write(disk("loop0/loop/backing_file"), "/var/images/data.img\n").unwrap();
        let block = root.join("dev/block");
        let class = root.join("class/block");
        fs::create_dir_all(&block).unwrap();
        fs::create_dir_all(&class).unwrap();
        fs::create_dir_all(root.join("block")).unwrap();
        for (device, path) in [
            ("8:1", "sda/sda1"),
            ("8:2", "sda/sda2"),
            ("8:16", "sdb"),
            ("8:33", "sdc/sdc1"),
            ("253:0", "dm-0"),
            ("7:0", "loop0"),
        ] {
            symlink(disk(path), block.join(device)).unwrap();
        }
        symlink(disk("sdc/sdc1"), class.join("sdc1")).unwrap();
        symlink(disk("sdc/sdc1"), disk("dm-0/slaves/sdc1")).unwrap();
        symlink(disk("loop0"), root.join("block/loop0")).unwrap();
    }

    const TABLE: &str = "\
22 1 8:2 / / rw - ext4 /dev/sda2 rw
24 22 8:1 / /boot rw - vfat /dev/sda1 rw
30 22 0:28 / /tmp rw - tmpfs tmpfs rw
40 22 8:16 / /mnt/data rw - ext4 /dev/sdb rw
41 22 8:2 /srv /mnt/bound rw - ext4 /dev/sda2 rw
42 22 253:0 / /mnt/crypt rw - ext4 /dev/mapper/vault rw
43 22 7:0 / /mnt/image rw - ext4 /dev/loop0 rw
44 22 0:50 / /mnt/home rw - btrfs /dev/sda2 rw,subvol=/home
45 22 0:50 / /mnt/snap rw - btrfs /dev/sda2 rw,subvol=/snap
46 22 0:60 / /tank/a rw - zfs tank/a rw
47 22 0:61 / /tank/b rw - zfs tank/b rw
";

    struct Fixture {
        _folder: crate::system_disk::scratch::Scratch,
        sys: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let folder = crate::system_disk::scratch::Scratch::new("linux-disks");
            let sys = folder.path().join("sys");
            sysfs(&sys);
            Self {
                _folder: folder,
                sys,
            }
        }

        fn disks(&self) -> LinuxDisks<'_> {
            LinuxDisks::new(TABLE, &self.sys)
        }

        fn same_disk(&self, left: &str, right: &str) -> bool {
            let disks = self.disks();
            let left = disks.describe(Path::new(left)).unwrap();
            let right = disks.describe(Path::new(right)).unwrap();
            left.shares_disk_with(&right)
        }
    }

    #[test]
    fn a_folder_on_the_root_filesystem_is_on_the_system_disk() {
        assert!(Fixture::new().same_disk("/home/me/root", "/"));
    }

    #[test]
    fn a_bind_mount_of_the_system_disk_is_on_the_system_disk() {
        assert!(Fixture::new().same_disk("/mnt/bound/root", "/"));
    }

    #[test]
    fn another_partition_of_the_system_disk_is_on_the_system_disk() {
        assert!(Fixture::new().same_disk("/boot/root", "/"));
    }

    #[test]
    fn a_second_disk_is_a_data_disk() {
        assert!(!Fixture::new().same_disk("/mnt/data/root", "/"));
    }

    #[test]
    fn device_mapper_is_followed_to_its_disks() {
        let fixture = Fixture::new();
        let crypt = fixture
            .disks()
            .describe(Path::new("/mnt/crypt/root"))
            .unwrap();
        assert!(crypt.keys.contains(&"disk:sdc".to_owned()), "{crypt:?}");
        assert!(!fixture.same_disk("/mnt/crypt/root", "/"));
    }

    #[test]
    fn btrfs_subvolumes_of_one_filesystem_share_a_disk() {
        assert!(Fixture::new().same_disk("/mnt/home/root", "/mnt/snap/root"));
    }

    #[test]
    fn datasets_of_one_zfs_pool_share_a_disk() {
        assert!(Fixture::new().same_disk("/tank/a/root", "/tank/b/root"));
    }

    #[test]
    fn tmpfs_is_a_memory_filesystem() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .disks()
                .describe(Path::new("/tmp/root"))
                .unwrap()
                .memory
        );
        assert!(
            !fixture
                .disks()
                .describe(Path::new("/mnt/data"))
                .unwrap()
                .memory
        );
    }

    #[test]
    fn a_loop_device_names_its_backing_file() {
        let fixture = Fixture::new();
        let image = fixture
            .disks()
            .describe(Path::new("/mnt/image/root"))
            .unwrap();
        assert_eq!(image.images, vec![PathBuf::from("/var/images/data.img")]);
    }

    #[test]
    fn a_deleted_backing_file_cannot_be_told() {
        let fixture = Fixture::new();
        fs::write(
            fixture.sys.join("devices/loop0/loop/backing_file"),
            "/var/images/data.img (deleted)\n",
        )
        .unwrap();
        assert!(
            fixture
                .disks()
                .describe(Path::new("/mnt/image/root"))
                .is_err()
        );
    }

    #[test]
    fn an_overlay_root_shares_only_its_own_device() {
        let table = "\
1 0 0:40 / / rw - overlay overlay rw,lowerdir=/l
2 1 254:17 / /home/ci rw - ext4 /dev/vdb1 rw
";
        let folder = crate::system_disk::scratch::Scratch::new("linux-disks");
        let disks = LinuxDisks::new(table, folder.path());
        let system = disks.describe(Path::new("/")).unwrap();
        let tmp = disks.describe(Path::new("/tmp/root")).unwrap();
        let volume = disks.describe(Path::new("/home/ci/root")).unwrap();
        assert!(tmp.shares_disk_with(&system));
        assert!(!volume.shares_disk_with(&system));
    }
}
