use std::path::{Component, Path, PathBuf};

use super::app_id::AppId;

/// The OS values an app's data folders are built from (ADR 0032 section
/// 3.2). BYCKR reads them from the OS, never from the root's path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppFolderInputs {
    /// The user's home folder (on iOS and in a macOS sandbox, the app's
    /// container).
    pub(crate) home: Option<PathBuf>,
    /// `XDG_DATA_HOME`, Linux only.
    pub(crate) xdg_data_home: Option<PathBuf>,
    /// `FOLDERID_LocalAppData`, Windows only.
    pub(crate) local_app_data: Option<PathBuf>,
    /// `FOLDERID_RoamingAppData`, Windows only.
    pub(crate) roaming_app_data: Option<PathBuf>,
}

/// The OS whose folder rules apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppPlatform {
    MacOs,
    Windows,
    Linux,
    Ios,
    Android,
}

/// One rule for an app-owned folder.
#[derive(Clone, Debug, Eq, PartialEq)]
enum AppFolder {
    /// This folder and everything in it.
    Exact(PathBuf),
    /// `base`, then any one folder, then `tail`: a packaged Windows app's
    /// redirected folder, or Android's `/data/user/<n>/<id>`.
    AnyMiddle { base: PathBuf, tail: PathBuf },
    /// Anything below `base` that the app's own user owns: Android without
    /// an id.
    Owned(PathBuf),
}

/// The folders an app owns on one OS, and whether a root is inside them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppFolders {
    folders: Vec<AppFolder>,
}

impl AppFolders {
    const MAC_SUPPORT: &'static str = "Library/Application Support";
    const MAC_CONTAINERS: &'static str = "Library/Containers";
    const MAC_CONTAINER_DATA: &'static str = "Data";
    const LINUX_SHARE: &'static str = ".local/share";
    const WINDOWS_PACKAGES: &'static str = "Packages";
    const WINDOWS_LOCAL_CACHE: &'static str = "LocalCache";
    const WINDOWS_LOCAL: &'static str = "Local";
    const WINDOWS_ROAMING: &'static str = "Roaming";
    const ANDROID_USERS: &'static str = "/data/user";
    const ANDROID_DATA: &'static str = "/data/data";

    /// The folders `id` owns on `platform`; on iOS and Android `id` may be
    /// left out.
    pub(crate) fn of(platform: AppPlatform, id: Option<&AppId>, inputs: &AppFolderInputs) -> Self {
        let folders = match platform {
            AppPlatform::MacOs => Self::mac(id, inputs),
            AppPlatform::Windows => Self::windows(id, inputs),
            AppPlatform::Linux => Self::linux(id, inputs),
            AppPlatform::Ios => inputs.home.iter().cloned().map(AppFolder::Exact).collect(),
            AppPlatform::Android => Self::android(id),
        };
        Self { folders }
    }

    /// Whether the absolute `path` is inside one of the folders, after both
    /// are canonicalized: a symbolic link (or a Windows junction) that leads
    /// out does not count. `owned` says whether this process's user owns a
    /// path, for Android without an id.
    pub(crate) fn contains(&self, path: &Path, owned: impl Fn(&Path) -> bool) -> bool {
        let Some(path) = Self::canonical(path) else {
            return false;
        };
        self.folders
            .iter()
            .any(|folder| Self::folder_contains(folder, &path, &owned))
    }

    /// The folders, for a refusal message.
    pub(crate) fn describe(&self) -> String {
        let names: Vec<String> = self.folders.iter().map(Self::folder_name).collect();
        names.join(", ")
    }

    fn mac(id: Option<&AppId>, inputs: &AppFolderInputs) -> Vec<AppFolder> {
        let (Some(id), Some(home)) = (id, inputs.home.as_ref()) else {
            return Vec::new();
        };
        vec![
            AppFolder::Exact(home.join(Self::MAC_SUPPORT).join(id.as_str())),
            AppFolder::Exact(
                home.join(Self::MAC_CONTAINERS)
                    .join(id.as_str())
                    .join(Self::MAC_CONTAINER_DATA),
            ),
        ]
    }

    fn linux(id: Option<&AppId>, inputs: &AppFolderInputs) -> Vec<AppFolder> {
        let Some(id) = id else {
            return Vec::new();
        };
        let data = inputs
            .xdg_data_home
            .clone()
            .filter(|folder| folder.is_absolute())
            .or_else(|| {
                inputs
                    .home
                    .as_ref()
                    .map(|home| home.join(Self::LINUX_SHARE))
            });
        data.map(|data| AppFolder::Exact(data.join(id.as_str())))
            .into_iter()
            .collect()
    }

    fn windows(id: Option<&AppId>, inputs: &AppFolderInputs) -> Vec<AppFolder> {
        let Some(id) = id else {
            return Vec::new();
        };
        let mut folders: Vec<AppFolder> = [&inputs.local_app_data, &inputs.roaming_app_data]
            .into_iter()
            .flatten()
            .map(|folder| AppFolder::Exact(folder.join(id.as_str())))
            .collect();
        folders.extend(
            inputs
                .local_app_data
                .iter()
                .flat_map(|local| Self::packaged(local, id)),
        );
        folders
    }

    /// A packaged (MSIX) app's view of `%LOCALAPPDATA%\<id>` and
    /// `%APPDATA%\<id>` is redirected below its package folder.
    fn packaged(local: &Path, id: &AppId) -> [AppFolder; 2] {
        let base = local.join(Self::WINDOWS_PACKAGES);
        let cache = Path::new(Self::WINDOWS_LOCAL_CACHE);
        [Self::WINDOWS_LOCAL, Self::WINDOWS_ROAMING].map(|kind| AppFolder::AnyMiddle {
            base: base.clone(),
            tail: cache.join(kind).join(id.as_str()),
        })
    }

    fn android(id: Option<&AppId>) -> Vec<AppFolder> {
        let Some(id) = id else {
            return vec![
                AppFolder::Owned(PathBuf::from(Self::ANDROID_USERS)),
                AppFolder::Owned(PathBuf::from(Self::ANDROID_DATA)),
            ];
        };
        vec![
            AppFolder::AnyMiddle {
                base: PathBuf::from(Self::ANDROID_USERS),
                tail: PathBuf::from(id.as_str()),
            },
            AppFolder::Exact(Path::new(Self::ANDROID_DATA).join(id.as_str())),
        ]
    }

    fn folder_contains(folder: &AppFolder, path: &Path, owned: &impl Fn(&Path) -> bool) -> bool {
        match folder {
            AppFolder::Exact(folder) => {
                Self::canonical(folder).is_some_and(|folder| path.starts_with(folder))
            }
            AppFolder::AnyMiddle { base, tail } => {
                Self::canonical(base).is_some_and(|base| Self::middle_contains(&base, tail, path))
            }
            AppFolder::Owned(base) => {
                Self::canonical(base).is_some_and(|base| path.starts_with(base) && owned(path))
            }
        }
    }

    fn middle_contains(base: &Path, tail: &Path, path: &Path) -> bool {
        let Ok(rest) = path.strip_prefix(base) else {
            return false;
        };
        let mut components = rest.components();
        components.next().is_some() && components.as_path().starts_with(tail)
    }

    fn folder_name(folder: &AppFolder) -> String {
        match folder {
            AppFolder::Exact(folder) => folder.display().to_string(),
            AppFolder::AnyMiddle { base, tail } => base.join("*").join(tail).display().to_string(),
            AppFolder::Owned(base) => format!("{} (owned by this user)", base.display()),
        }
    }

    /// `path` with its existing part canonicalized and the rest added back;
    /// `None` when the rest steps up with `..`, which could leave the folder.
    pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
        let absolute = std::path::absolute(path).ok()?;
        let existing = absolute.ancestors().find(|ancestor| ancestor.exists())?;
        let rest = absolute.strip_prefix(existing).ok()?;
        if rest
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return None;
        }
        Some(std::fs::canonicalize(existing).ok()?.join(rest))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{AppFolderInputs, AppFolders, AppPlatform};
    use crate::system_disk::app_id::AppId;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    struct Home {
        _folder: crate::system_disk::scratch::Scratch,
        path: PathBuf,
    }

    impl Home {
        fn new() -> Self {
            let folder = crate::system_disk::scratch::Scratch::new("app-folders");
            let path = fs::canonicalize(folder.path()).unwrap();
            Self {
                _folder: folder,
                path,
            }
        }

        fn folder(&self, relative: &str) -> PathBuf {
            let folder = self.path.join(relative);
            fs::create_dir_all(&folder).unwrap();
            folder
        }
    }

    fn id() -> AppId {
        AppId::parse("com.example.app").unwrap()
    }

    fn nobody(_: &Path) -> bool {
        false
    }

    fn everyone(_: &Path) -> bool {
        true
    }

    /// One root inside (admitted), one beside the folder (refused) and one
    /// through a link that leads out (refused).
    fn check(folders: &AppFolders, app_folder: &Path, home: &Home) {
        assert!(
            folders.contains(&app_folder.join("db"), nobody),
            "{folders:?}"
        );
        let beside = home.folder("elsewhere");
        assert!(!folders.contains(&beside.join("db"), nobody));
        symlink(&beside, app_folder.join("link-out")).unwrap();
        assert!(!folders.contains(&app_folder.join("link-out/db"), nobody));
        assert!(!folders.contains(&app_folder.join("new/../../elsewhere/db"), nobody));
    }

    #[test]
    fn macos_application_support_is_owned_by_the_app() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            home: Some(home.path.clone()),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::MacOs, Some(&id()), &inputs);
        let app = home.folder("Library/Application Support/com.example.app");
        check(&folders, &app, &home);
    }

    #[test]
    fn a_macos_sandbox_container_is_owned_by_the_app() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            home: Some(home.path.clone()),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::MacOs, Some(&id()), &inputs);
        let app = home.folder("Library/Containers/com.example.app/Data");
        check(&folders, &app, &home);
    }

    #[test]
    fn windows_local_and_roaming_app_data_are_owned_by_the_app() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            local_app_data: Some(home.folder("Local")),
            roaming_app_data: Some(home.folder("Roaming")),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::Windows, Some(&id()), &inputs);
        check(&folders, &home.folder("Local/com.example.app"), &home);
        assert!(folders.contains(&home.folder("Roaming/com.example.app").join("db"), nobody));
    }

    #[test]
    fn a_packaged_windows_app_folder_is_owned_by_the_app() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            local_app_data: Some(home.folder("Local")),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::Windows, Some(&id()), &inputs);
        let app = home.folder("Local/Packages/Example_abc123/LocalCache/Local/com.example.app");
        check(&folders, &app, &home);
        let other = home.folder("Local/Packages/Example_abc123/LocalCache/Local/com.other");
        assert!(!folders.contains(&other.join("db"), nobody));
    }

    #[test]
    fn linux_xdg_data_home_is_owned_by_the_app() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            home: Some(home.path.clone()),
            xdg_data_home: Some(home.folder("xdg")),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::Linux, Some(&id()), &inputs);
        check(&folders, &home.folder("xdg/com.example.app"), &home);
        assert!(!folders.contains(
            &home.folder(".local/share/com.example.app").join("db"),
            nobody
        ));
    }

    #[test]
    fn linux_without_xdg_data_home_uses_local_share() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            home: Some(home.path.clone()),
            xdg_data_home: Some(PathBuf::from("relative/xdg")),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::Linux, Some(&id()), &inputs);
        check(
            &folders,
            &home.folder(".local/share/com.example.app"),
            &home,
        );
    }

    #[test]
    fn the_ios_container_is_owned_by_the_app_without_an_id() {
        let home = Home::new();
        let inputs = AppFolderInputs {
            home: Some(home.folder("container")),
            ..AppFolderInputs::default()
        };
        let folders = AppFolders::of(AppPlatform::Ios, None, &inputs);
        check(&folders, &home.folder("container/Library"), &home);
    }

    #[test]
    fn android_folders_follow_the_package_or_the_owner() {
        let with_id = AppFolders::of(
            AppPlatform::Android,
            Some(&id()),
            &AppFolderInputs::default(),
        );
        assert!(with_id.contains(Path::new("/data/user/0/com.example.app/files/db"), nobody));
        assert!(with_id.contains(Path::new("/data/data/com.example.app/files/db"), nobody));
        assert!(!with_id.contains(Path::new("/data/user/0/com.other/files/db"), nobody));
        let without = AppFolders::of(AppPlatform::Android, None, &AppFolderInputs::default());
        assert!(without.contains(Path::new("/data/user/0/com.any/files/db"), everyone));
        assert!(!without.contains(Path::new("/data/user/0/com.any/files/db"), nobody));
        assert!(!without.contains(Path::new("/sdcard/db"), everyone));
    }

    #[test]
    fn desktop_folders_need_an_app_id() {
        let inputs = AppFolderInputs {
            home: Some(PathBuf::from("/home/me")),
            ..AppFolderInputs::default()
        };
        assert_eq!(
            AppFolders::of(AppPlatform::Linux, None, &inputs).describe(),
            ""
        );
        assert_eq!(
            AppFolders::of(AppPlatform::MacOs, None, &inputs).describe(),
            ""
        );
    }
}
