use std::path::Path;
use std::sync::Once;

use crate::NativeStorageError;

use super::app_folders::AppFolders;
use super::app_id::AppId;
use super::disk_description::DiskDescription;
use super::disk_reader::DiskReader;
use super::disk_source::DiskSource;
use super::opt_out_source::OptOutSource;
use super::placement_kind::PlacementKind;
use super::placement_report::PlacementReport;
use super::root_placement::RootPlacement;
use super::root_role::RootRole;
use super::system_disk_refusal::SystemDiskRefusal;
use super::volume_root::VolumeRoot;
use crate::volume_root_rule::VolumeRootRule;

/// The rule that a server root lives directly below the mount point of a
/// drive that is not the system disk (ADR 0032, ADR 0033 section 5). It
/// runs after ADR 0029's network check, on a root that is local, or on a
/// network root once the network switch admits it.
pub(crate) struct SystemDiskRule;

impl SystemDiskRule {
    const IMAGE_DEPTH: usize = 4;

    /// Admit the root at `path` under `placement`, or refuse it with
    /// `ENOTVOLUMEROOT` or `ESYSTEMDISK`.
    ///
    /// # Errors
    ///
    /// Returns `ENOTVOLUMEROOT` for a root whose parent is not a mount
    /// point, and `ESYSTEMDISK` for a root on the system disk, on a memory
    /// filesystem, on a disk image whose file is on the system disk, or on a
    /// disk BYCKR cannot tell.
    pub(crate) fn admit(
        path: &Path,
        placement: &RootPlacement,
        role: RootRole,
    ) -> Result<PlacementReport, NativeStorageError> {
        Self::verdict(path, placement, role).map_err(|refusal| refusal.error(path))
    }

    /// What the handshake reports for the primary root at `path`; it never
    /// refuses.
    pub(crate) fn report(path: &Path, placement: &RootPlacement) -> PlacementReport {
        Self::verdict(path, placement, RootRole::Primary)
            .unwrap_or_else(|refusal| PlacementReport::refused(refusal.system_disk()))
    }

    /// The refusal for a placement setting that cannot be read.
    pub(crate) fn setting_error(path: &Path, reason: String) -> NativeStorageError {
        SystemDiskRefusal::Setting { reason }.error(path)
    }

    fn verdict(
        path: &Path,
        placement: &RootPlacement,
        role: RootRole,
    ) -> Result<PlacementReport, SystemDiskRefusal> {
        match placement {
            RootPlacement::AllowSystemDisk(source) => Ok(Self::allowed(*source)),
            RootPlacement::AppOwned(app) if DiskReader::MOBILE => Self::mobile(path, Some(app)),
            _ if DiskReader::MOBILE => Self::mobile(path, None),
            RootPlacement::AppOwned(app) => Self::app_owned(path, app, role),
            RootPlacement::DataDisk => {
                Self::data_disk(&DiskReader, path, role, VolumeRootRule::ENFORCED)
            }
        }
    }

    /// The opt-out: warn once per process, and look nothing up.
    fn allowed(source: OptOutSource) -> PlacementReport {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "BYCKR warning: system-disk check disabled by {} (ADR 0032); a root on the system disk is allowed.",
                source.describe()
            );
        });
        PlacementReport::admitted(None, PlacementKind::SystemDiskAllowed)
    }

    /// iOS and Android have no other disk: app mode is automatic, and the
    /// root must be inside the app's own data.
    fn mobile(path: &Path, app: Option<&AppId>) -> Result<PlacementReport, SystemDiskRefusal> {
        let folders = AppFolders::of(
            DiskReader::platform(),
            app,
            &DiskReader::app_folder_inputs(),
        );
        if folders.contains(path, DiskReader::owned_by_process) {
            return Ok(PlacementReport::admitted(None, PlacementKind::AppOwned));
        }
        Err(SystemDiskRefusal::OutsideApp {
            app: app.map_or("this app", AppId::as_str).to_owned(),
            folders: folders.describe(),
            inner: Box::new(SystemDiskRefusal::SystemDisk {
                disk: "the device's only disk".to_owned(),
            }),
        })
    }

    /// App mode: the app's own folder, or a data disk like any server.
    fn app_owned(
        path: &Path,
        app: &AppId,
        role: RootRole,
    ) -> Result<PlacementReport, SystemDiskRefusal> {
        let folders = AppFolders::of(
            DiskReader::platform(),
            Some(app),
            &DiskReader::app_folder_inputs(),
        );
        if folders.contains(path, DiskReader::owned_by_process) {
            return Ok(PlacementReport::admitted(None, PlacementKind::AppOwned));
        }
        Self::data_disk(&DiskReader, path, role, VolumeRootRule::ENFORCED).map_err(|inner| {
            SystemDiskRefusal::OutsideApp {
                app: app.as_str().to_owned(),
                folders: folders.describe(),
                inner: Box::new(inner),
            }
        })
    }

    /// The drive rule, in the order of ADR 0033 section 5.2: the mount
    /// point first, then the disk. `volume_rule` is
    /// [`VolumeRootRule::ENFORCED`]; tests pass `true` to check the
    /// mount-point rule before it is switched on.
    fn data_disk(
        disks: &dyn DiskSource,
        path: &Path,
        role: RootRole,
        volume_rule: bool,
    ) -> Result<PlacementReport, SystemDiskRefusal> {
        Self::volume(disks, path, role, volume_rule)?;
        Self::drive(disks, path)
    }

    /// A worktree's repository root carries the mount-point test, and the
    /// test runs only while the rule is switched on.
    fn volume(
        disks: &dyn DiskSource,
        path: &Path,
        role: RootRole,
        volume_rule: bool,
    ) -> Result<(), SystemDiskRefusal> {
        match (role, volume_rule) {
            (RootRole::Primary, true) => VolumeRoot::check(disks, path).map(drop),
            _ => Ok(()),
        }
    }

    /// A network share has no disk, so it is never the system disk.
    fn drive(disks: &dyn DiskSource, path: &Path) -> Result<PlacementReport, SystemDiskRefusal> {
        if disks.remote(path).map_err(Self::could_not_tell)? {
            return Ok(PlacementReport::admitted(None, PlacementKind::DataDisk));
        }
        Self::local_disk(disks, path)
    }

    fn local_disk(
        disks: &dyn DiskSource,
        path: &Path,
    ) -> Result<PlacementReport, SystemDiskRefusal> {
        let system = disks.system().map_err(Self::could_not_tell)?;
        let root = disks.describe(path).map_err(Self::could_not_tell)?;
        Self::judge(disks, &root, &system, 0)?;
        Ok(PlacementReport::admitted(
            Some(false),
            PlacementKind::DataDisk,
        ))
    }

    /// A disk is refused when it is a memory filesystem, shares a disk with
    /// the system, or is made from an image whose file is on the system
    /// disk.
    fn judge(
        disks: &dyn DiskSource,
        disk: &DiskDescription,
        system: &DiskDescription,
        depth: usize,
    ) -> Result<(), SystemDiskRefusal> {
        if disk.memory {
            return Err(SystemDiskRefusal::Memory {
                disk: disk.label.clone(),
            });
        }
        if disk.shares_disk_with(system) {
            return Err(SystemDiskRefusal::SystemDisk {
                disk: disk.label.clone(),
            });
        }
        let images = disks
            .images(disk)
            .map_err(|reason| Self::unknown_image(disk, None, &reason))?;
        images
            .iter()
            .try_for_each(|image| Self::judge_image(disks, disk, image, system, depth))
    }

    /// Follow a disk image to its file. A file on a network share or on
    /// another local disk is admitted; a file on the system disk (or a
    /// memory filesystem) is not.
    fn judge_image(
        disks: &dyn DiskSource,
        disk: &DiskDescription,
        image: &Path,
        system: &DiskDescription,
        depth: usize,
    ) -> Result<(), SystemDiskRefusal> {
        if depth >= Self::IMAGE_DEPTH {
            let reason = format!(
                "disk images are nested more than {} deep",
                Self::IMAGE_DEPTH
            );
            return Err(Self::unknown_image(disk, Some(image), &reason));
        }
        let remote = disks
            .remote(image)
            .map_err(|reason| Self::unknown_image(disk, Some(image), &reason))?;
        if remote {
            return Ok(());
        }
        let backing = disks
            .describe(image)
            .map_err(|reason| Self::unknown_image(disk, Some(image), &reason))?;
        Self::judge(disks, &backing, system, depth + 1)
            .map_err(|refusal| Self::through_image(disk, image, refusal))
    }

    fn through_image(
        disk: &DiskDescription,
        image: &Path,
        refusal: SystemDiskRefusal,
    ) -> SystemDiskRefusal {
        match refusal {
            SystemDiskRefusal::SystemDisk { .. } | SystemDiskRefusal::Memory { .. } => {
                SystemDiskRefusal::ImageOnSystemDisk {
                    disk: disk.label.clone(),
                    image: image.to_owned(),
                }
            }
            other => other,
        }
    }

    /// "Could not tell", naming the disk and, when known, the image file.
    fn unknown_image(
        disk: &DiskDescription,
        image: Option<&Path>,
        reason: &str,
    ) -> SystemDiskRefusal {
        let image = image
            .map(|image| format!(", image {}", image.display()))
            .unwrap_or_default();
        Self::could_not_tell(format!("disk {}{image}: {reason}", disk.label))
    }

    fn could_not_tell(reason: String) -> SystemDiskRefusal {
        SystemDiskRefusal::CouldNotTell { reason }
    }
}

#[cfg(test)]
mod tests {
    use super::{DiskDescription, DiskSource, RootRole, SystemDiskRefusal, SystemDiskRule};
    use crate::system_disk::placement_kind::PlacementKind;
    use std::path::{Path, PathBuf};

    /// Disks by path prefix: `/system` is the system disk, `/data` a second
    /// local disk, `/share` a network share, `/tmp` a memory filesystem.
    /// `/data/image` and `/system/image` are disk images whose files are
    /// named by `image_files`. Each prefix is also its mount point.
    struct FakeDisks {
        image_files: Vec<(&'static str, &'static str)>,
        broken_images: bool,
    }

    impl FakeDisks {
        fn new(image_files: &[(&'static str, &'static str)]) -> Self {
            Self {
                image_files: image_files.to_vec(),
                broken_images: false,
            }
        }

        fn disk_of(path: &Path) -> &'static str {
            [
                "/data/image-system",
                "/data/image-local",
                "/data/image-share",
                "/data/image-nested",
                "/system",
                "/data",
                "/tmp",
                "/other",
                "/share",
            ]
            .into_iter()
            .find(|prefix| path.starts_with(prefix))
            .unwrap_or("/unknown")
        }
    }

    impl DiskSource for FakeDisks {
        fn system(&self) -> Result<DiskDescription, String> {
            Ok(DiskDescription::named("system"))
        }

        fn describe(&self, path: &Path) -> Result<DiskDescription, String> {
            let disk = Self::disk_of(path);
            if disk == "/unknown" {
                return Err("no mount holds it".to_owned());
            }
            let mut description = DiskDescription::named(disk.trim_start_matches('/'));
            description.memory = disk == "/tmp";
            Ok(description)
        }

        fn images(&self, disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
            if self.broken_images && disk.label.starts_with("data/image") {
                return Err("hdiutil timed out after 15 s".to_owned());
            }
            Ok(self
                .image_files
                .iter()
                .filter(|(label, _)| *label == disk.label)
                .map(|(_, file)| PathBuf::from(file))
                .collect())
        }

        fn remote(&self, path: &Path) -> Result<bool, String> {
            Ok(path.starts_with("/share"))
        }

        fn mount_point(&self, path: &Path) -> Result<PathBuf, String> {
            match Self::disk_of(path) {
                "/unknown" => Err("no mount holds it".to_owned()),
                disk => Ok(PathBuf::from(disk)),
            }
        }
    }

    const IMAGES: &[(&str, &str)] = &[
        ("data/image-system", "/system/Users/me/data.dmg"),
        ("data/image-local", "/other/images/data.dmg"),
        (
            "data/image-share",
            "/share/Mesail/mesail-fylo-test.sparsebundle",
        ),
        ("data/image-nested", "/data/image-nested/inner.dmg"),
    ];

    fn verdict(disks: &FakeDisks, path: &str) -> Result<PlacementKind, SystemDiskRefusal> {
        SystemDiskRule::data_disk(disks, Path::new(path), RootRole::Primary, true)
            .map(|report| report.placement.unwrap())
    }

    #[test]
    fn a_root_on_the_system_disk_is_refused() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(
            verdict(&disks, "/system/root"),
            Err(SystemDiskRefusal::SystemDisk {
                disk: "system".to_owned()
            })
        );
    }

    #[test]
    fn a_root_below_a_folder_of_a_data_disk_is_refused_before_the_disk_is_read() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(
            verdict(&disks, "/data/roots/job/root"),
            Err(SystemDiskRefusal::NotVolumeRoot {
                parent: PathBuf::from("/data/roots/job"),
                mount: PathBuf::from("/data"),
            })
        );
    }

    #[test]
    fn a_worktree_inside_its_repository_is_judged_by_its_disk_only() {
        let disks = FakeDisks::new(IMAGES);
        let worktree = Path::new("/data/root/.fylo-vcs/worktrees/topic");
        let report = SystemDiskRule::data_disk(&disks, worktree, RootRole::Worktree, true).unwrap();
        assert_eq!(report.placement, Some(PlacementKind::DataDisk));
        let system = Path::new("/system/root/.fylo-vcs/worktrees/topic");
        assert!(matches!(
            SystemDiskRule::data_disk(&disks, system, RootRole::Worktree, true),
            Err(SystemDiskRefusal::SystemDisk { .. })
        ));
    }

    #[test]
    fn while_the_rule_is_off_a_nested_root_is_judged_by_its_disk_only() {
        let disks = FakeDisks::new(IMAGES);
        let nested = Path::new("/data/roots/job/root");
        let report = SystemDiskRule::data_disk(&disks, nested, RootRole::Primary, false).unwrap();
        assert_eq!(report.placement, Some(PlacementKind::DataDisk));
        assert!(matches!(
            SystemDiskRule::data_disk(
                &disks,
                Path::new("/system/Users/me/root"),
                RootRole::Primary,
                false
            ),
            Err(SystemDiskRefusal::SystemDisk { .. })
        ));
    }

    #[test]
    fn a_home_folder_root_is_refused_for_its_folder_first() {
        let disks = FakeDisks::new(IMAGES);
        assert!(matches!(
            verdict(&disks, "/system/Users/me/root"),
            Err(SystemDiskRefusal::NotVolumeRoot { .. })
        ));
    }

    #[test]
    fn a_root_directly_below_a_network_share_is_admitted_without_a_disk() {
        let disks = FakeDisks::new(IMAGES);
        let report =
            SystemDiskRule::data_disk(&disks, Path::new("/share/root"), RootRole::Primary, true)
                .unwrap();
        assert_eq!(report.placement, Some(PlacementKind::DataDisk));
        assert_eq!(report.system_disk, None);
    }

    #[test]
    fn a_root_below_a_folder_of_a_network_share_is_refused() {
        let disks = FakeDisks::new(IMAGES);
        assert!(matches!(
            verdict(&disks, "/share/Mesail/root"),
            Err(SystemDiskRefusal::NotVolumeRoot { .. })
        ));
    }

    #[test]
    fn a_root_on_a_data_disk_is_admitted() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(verdict(&disks, "/data/root"), Ok(PlacementKind::DataDisk));
    }

    #[test]
    fn a_root_on_a_memory_filesystem_is_refused() {
        let disks = FakeDisks::new(IMAGES);
        assert!(matches!(
            verdict(&disks, "/tmp/root"),
            Err(SystemDiskRefusal::Memory { .. })
        ));
    }

    #[test]
    fn an_image_whose_file_is_on_the_system_disk_is_refused() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(
            verdict(&disks, "/data/image-system/root"),
            Err(SystemDiskRefusal::ImageOnSystemDisk {
                disk: "data/image-system".to_owned(),
                image: PathBuf::from("/system/Users/me/data.dmg"),
            })
        );
    }

    #[test]
    fn an_image_whose_file_is_on_another_local_disk_is_admitted() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(
            verdict(&disks, "/data/image-local/root"),
            Ok(PlacementKind::DataDisk)
        );
    }

    #[test]
    fn an_image_whose_file_is_on_a_network_share_is_admitted() {
        let disks = FakeDisks::new(IMAGES);
        assert_eq!(
            verdict(&disks, "/data/image-share/root"),
            Ok(PlacementKind::DataDisk)
        );
    }

    #[test]
    fn an_image_that_cannot_be_read_is_refused_and_named() {
        let mut disks = FakeDisks::new(IMAGES);
        disks.broken_images = true;
        let refusal = verdict(&disks, "/data/image-share/root").unwrap_err();
        let SystemDiskRefusal::CouldNotTell { reason } = refusal else {
            panic!("{refusal:?}");
        };
        assert!(reason.contains("disk data/image-share"), "{reason}");
        assert!(reason.contains("hdiutil timed out"), "{reason}");
    }

    #[test]
    fn images_nested_too_deep_cannot_be_told() {
        let disks = FakeDisks::new(IMAGES);
        assert!(matches!(
            verdict(&disks, "/data/image-nested/root"),
            Err(SystemDiskRefusal::CouldNotTell { .. })
        ));
    }

    #[test]
    fn a_disk_that_cannot_be_described_is_refused() {
        let disks = FakeDisks::new(IMAGES);
        assert!(matches!(
            verdict(&disks, "/unknown/root"),
            Err(SystemDiskRefusal::CouldNotTell { .. })
        ));
    }
}
