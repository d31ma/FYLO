use std::path::{Path, PathBuf};

use crate::{NativeStorageError, NativeStorageErrorCode};

/// Why a root is refused with `ESYSTEMDISK` (ADR 0032 section 4), or with
/// `ENOTVOLUMEROOT` when it is not directly below a mount point (ADR 0033
/// section 5.2).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemDiskRefusal {
    /// The root's parent folder is not a mount point.
    NotVolumeRoot { parent: PathBuf, mount: PathBuf },
    /// The root is on the system disk.
    SystemDisk { disk: String },
    /// The root is on a memory filesystem (Linux `tmpfs`, `ramfs`).
    Memory { disk: String },
    /// The root is on a disk image whose file is on the system disk.
    ImageOnSystemDisk { disk: String, image: PathBuf },
    /// FYLO cannot tell which disk holds the root.
    CouldNotTell { reason: String },
    /// App mode: the root is outside the app's folders and refused for
    /// another reason too.
    OutsideApp {
        app: String,
        folders: String,
        inner: Box<Self>,
    },
    /// A setting is invalid, such as `FYLO_ALLOW_SYSTEM_DISK=yes`.
    Setting { reason: String },
}

impl SystemDiskRefusal {
    const ADVICE: &'static str = "A FYLO server keeps its data on another local disk: a disk under \
         /Volumes on macOS, a mounted disk such as /mnt/<disk> on Linux, or a drive other than \
         the Windows drive on Windows. To use the system disk anyway (tests, CI, a laptop), pass \
         --allow-system-disk, set FYLO_ALLOW_SYSTEM_DISK=1 or set fylo.allowSystemDisk to true. \
         An app that keeps its data in its own data folder passes --app-id <id> (FYLO_APP_ID, \
         fylo.appId).";

    /// The `ESYSTEMDISK` or `ENOTVOLUMEROOT` error for the root at `path`.
    pub(crate) fn error(&self, path: &Path) -> NativeStorageError {
        NativeStorageError::new(
            self.code(),
            format!(
                "FYLO root {} {} {}",
                path.display(),
                self.reason(),
                Self::ADVICE
            ),
        )
    }

    fn code(&self) -> NativeStorageErrorCode {
        match self {
            Self::NotVolumeRoot { .. } => NativeStorageErrorCode::NotVolumeRoot,
            Self::OutsideApp { inner, .. } => inner.code(),
            _ => NativeStorageErrorCode::SystemDisk,
        }
    }

    /// Whether the root is on the system disk, for the handshake; `None`
    /// when FYLO did not look or could not tell.
    pub(crate) fn system_disk(&self) -> Option<bool> {
        match self {
            Self::NotVolumeRoot { .. } => None,
            Self::SystemDisk { .. } => Some(true),
            Self::Memory { .. } | Self::ImageOnSystemDisk { .. } => Some(false),
            Self::CouldNotTell { .. } | Self::Setting { .. } => None,
            Self::OutsideApp { inner, .. } => inner.system_disk(),
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::NotVolumeRoot { parent, mount } => format!(
                "is not directly below a mount point: its parent {} is not a mount point, and \
                 the nearest mount point above it is {}. Put the root folder directly in the \
                 top folder of a drive that is not the boot disk.",
                parent.display(),
                mount.display()
            ),
            Self::SystemDisk { disk } => format!("is on the system disk ({disk})."),
            Self::Memory { disk } => format!("is on a memory file system ({disk})."),
            Self::ImageOnSystemDisk { disk, image } => format!(
                "is on a disk image ({disk}) whose file {} is on the system disk.",
                image.display()
            ),
            Self::CouldNotTell { reason } => {
                format!("is refused because FYLO could not tell which disk holds it ({reason}).")
            }
            Self::OutsideApp {
                app,
                folders,
                inner,
            } => format!(
                "is outside the folder of app {app} ({folders}), and {}",
                inner.reason().trim_start_matches("is ")
            ),
            Self::Setting { reason } => format!("cannot be checked: {reason}."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SystemDiskRefusal;
    use crate::NativeStorageErrorCode;
    use std::path::{Path, PathBuf};

    fn message(refusal: &SystemDiskRefusal) -> String {
        let error = refusal.error(Path::new("/Users/me/root"));
        assert_eq!(error.code(), NativeStorageErrorCode::SystemDisk);
        assert_eq!(error.code().as_str(), "ESYSTEMDISK");
        error.to_string()
    }

    fn names_the_opt_out(message: &str) {
        assert!(message.contains("--allow-system-disk"), "{message}");
        assert!(message.contains("FYLO_ALLOW_SYSTEM_DISK=1"), "{message}");
        assert!(message.contains("fylo.allowSystemDisk"), "{message}");
        assert!(message.contains("--app-id <id>"), "{message}");
    }

    #[test]
    fn the_system_disk_message_names_the_root_the_disk_and_the_opt_out() {
        let text = message(&SystemDiskRefusal::SystemDisk {
            disk: "disk3".to_owned(),
        });
        assert!(text.contains("FYLO root /Users/me/root is on the system disk (disk3)."));
        names_the_opt_out(&text);
    }

    #[test]
    fn every_variant_names_the_opt_out() {
        let image = SystemDiskRefusal::ImageOnSystemDisk {
            disk: "disk9".to_owned(),
            image: PathBuf::from("/Users/me/data.dmg"),
        };
        let unknown = SystemDiskRefusal::CouldNotTell {
            reason: "hdiutil timed out after 15 s".to_owned(),
        };
        let memory = SystemDiskRefusal::Memory {
            disk: "0:28 tmpfs on /tmp".to_owned(),
        };
        let app = SystemDiskRefusal::OutsideApp {
            app: "com.example.app".to_owned(),
            folders: "/home/me/.local/share/com.example.app".to_owned(),
            inner: Box::new(SystemDiskRefusal::SystemDisk {
                disk: "dev:8:2".to_owned(),
            }),
        };
        for refusal in [image, unknown, memory, app] {
            names_the_opt_out(&message(&refusal));
        }
    }

    #[test]
    fn a_root_below_a_deeper_folder_is_refused_with_its_own_code() {
        let refusal = SystemDiskRefusal::NotVolumeRoot {
            parent: PathBuf::from("/Volumes/HANGAR/roots"),
            mount: PathBuf::from("/Volumes/HANGAR"),
        };
        let error = refusal.error(Path::new("/Volumes/HANGAR/roots/mesail"));
        assert_eq!(error.code(), NativeStorageErrorCode::NotVolumeRoot);
        assert_eq!(error.code().as_str(), "ENOTVOLUMEROOT");
        let text = error.to_string();
        assert!(
            text.contains("its parent /Volumes/HANGAR/roots is not a mount point"),
            "{text}"
        );
        assert!(
            text.contains("the nearest mount point above it is /Volumes/HANGAR."),
            "{text}"
        );
        names_the_opt_out(&text);
        assert_eq!(refusal.system_disk(), None);
    }

    #[test]
    fn app_mode_keeps_the_code_of_the_inner_refusal() {
        let refusal = SystemDiskRefusal::OutsideApp {
            app: "com.example.app".to_owned(),
            folders: "/x/com.example.app".to_owned(),
            inner: Box::new(SystemDiskRefusal::NotVolumeRoot {
                parent: PathBuf::from("/Volumes/HANGAR/roots"),
                mount: PathBuf::from("/Volumes/HANGAR"),
            }),
        };
        let error = refusal.error(Path::new("/Volumes/HANGAR/roots/mesail"));
        assert_eq!(error.code(), NativeStorageErrorCode::NotVolumeRoot);
        assert!(
            error
                .to_string()
                .contains("and not directly below a mount point")
        );
    }

    #[test]
    fn the_variants_say_why() {
        let image = message(&SystemDiskRefusal::ImageOnSystemDisk {
            disk: "disk9".to_owned(),
            image: PathBuf::from("/Users/me/data.dmg"),
        });
        assert!(
            image
                .contains("disk image (disk9) whose file /Users/me/data.dmg is on the system disk")
        );
        let unknown = message(&SystemDiskRefusal::CouldNotTell {
            reason: "hdiutil timed out after 15 s".to_owned(),
        });
        assert!(
            unknown.contains("could not tell which disk holds it (hdiutil timed out after 15 s)")
        );
        let app = message(&SystemDiskRefusal::OutsideApp {
            app: "com.example.app".to_owned(),
            folders: "/x/com.example.app".to_owned(),
            inner: Box::new(SystemDiskRefusal::SystemDisk {
                disk: "disk3".to_owned(),
            }),
        });
        assert!(app.contains("is outside the folder of app com.example.app (/x/com.example.app), and on the system disk (disk3)."), "{app}");
    }
}
