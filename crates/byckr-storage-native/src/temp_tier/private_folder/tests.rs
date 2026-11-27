use super::PrivateFolder;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

struct Place(PathBuf);

impl Place {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = byckr_test_support::TestFolder::path().join(format!(
            "fylo-private-folder-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn cache(&self) -> PathBuf {
        self.0.join("fylo-cache-v1")
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn refused(result: std::io::Result<PrivateFolder>) -> String {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{error}");
    error.to_string()
}

#[test]
fn a_missing_folder_is_created_and_opened() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    assert_eq!(folder.path(), place.cache());
    assert!(place.cache().is_dir());
}

#[test]
fn open_existing_creates_nothing() {
    let place = Place::new();
    assert!(
        PrivateFolder::open_existing(&place.cache())
            .unwrap()
            .is_none()
    );
    assert!(!place.cache().exists());
}

#[test]
fn a_wiped_folder_is_created_again() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    folder.create_folders(Path::new("root/collection")).unwrap();
    drop(folder);
    fs::remove_dir_all(place.cache()).unwrap();
    let again = PrivateFolder::open_or_create(&place.cache()).unwrap();
    again.create_folders(Path::new("root/collection")).unwrap();
    assert!(place.cache().join("root/collection").is_dir());
}

#[test]
fn a_file_in_place_of_the_folder_is_refused() {
    let place = Place::new();
    fs::write(place.cache(), b"not a folder").unwrap();
    let message = refused(PrivateFolder::open_or_create(&place.cache()));
    assert!(message.contains("is not a folder"), "{message}");
}

#[test]
fn files_written_below_the_folder_read_back() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    folder.create_folders(Path::new("a/b")).unwrap();
    folder
        .create_file(Path::new("a/b/entry.fyc"))
        .unwrap()
        .write_all(b"bytes")
        .unwrap();
    let mut text = String::new();
    folder
        .open_file(Path::new("a/b/entry.fyc"))
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "bytes");
}

#[test]
fn create_file_refuses_an_existing_name() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    folder.create_file(Path::new("entry.fyc")).unwrap();
    let error = folder.create_file(Path::new("entry.fyc")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
}

#[test]
fn replace_file_empties_an_existing_file() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    folder
        .replace_file(Path::new("sweep.stamp"))
        .unwrap()
        .write_all(b"long old text")
        .unwrap();
    folder
        .replace_file(Path::new("sweep.stamp"))
        .unwrap()
        .write_all(b"new")
        .unwrap();
    assert_eq!(fs::read(place.cache().join("sweep.stamp")).unwrap(), b"new");
}

#[test]
fn a_path_that_leaves_the_folder_is_refused() {
    let place = Place::new();
    let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
    for relative in ["../escape", "", "a/../../escape"] {
        let error = folder.open_file(Path::new(relative)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{relative}");
    }
}

#[cfg(unix)]
mod unix {
    use super::{Place, PrivateFolder, refused};
    use crate::temp_tier::folder_owner::FolderOwner;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::Path;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_new_folder_is_private_to_its_user() {
        let place = Place::new();
        PrivateFolder::open_or_create(&place.cache()).unwrap();
        assert_eq!(mode(&place.cache()), 0o700);
        let owner = fs::metadata(place.cache()).unwrap().uid();
        assert_eq!(owner, FolderOwner::current().id());
    }

    #[test]
    fn new_files_and_folders_below_it_are_private() {
        let place = Place::new();
        let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
        folder.create_folders(Path::new("root")).unwrap();
        folder.create_file(Path::new("root/entry.fyc")).unwrap();
        assert_eq!(mode(&place.cache().join("root")), 0o700);
        assert_eq!(mode(&place.cache().join("root/entry.fyc")), 0o600);
    }

    #[test]
    fn a_folder_that_is_a_symbolic_link_is_refused() {
        let place = Place::new();
        let target = place.0.join("elsewhere");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&target, place.cache()).unwrap();
        let message = refused(PrivateFolder::open_or_create(&place.cache()));
        assert!(message.contains("symbolic link"), "{message}");
    }

    #[test]
    fn a_folder_with_group_bits_is_refused_and_left_unchanged() {
        let place = Place::new();
        fs::create_dir(place.cache()).unwrap();
        fs::set_permissions(place.cache(), fs::Permissions::from_mode(0o750)).unwrap();
        let message = refused(PrivateFolder::open_or_create(&place.cache()));
        assert!(message.contains("other users"), "{message}");
        assert_eq!(mode(&place.cache()), 0o750);
    }

    #[test]
    fn a_folder_with_other_bits_is_refused() {
        let place = Place::new();
        fs::create_dir(place.cache()).unwrap();
        fs::set_permissions(place.cache(), fs::Permissions::from_mode(0o701)).unwrap();
        refused(
            PrivateFolder::open_existing(&place.cache())
                .transpose()
                .unwrap(),
        );
    }

    #[test]
    fn a_folder_of_another_owner_is_refused() {
        let place = Place::new();
        let stranger = FolderOwner::with_id(FolderOwner::current().id().wrapping_add(1));
        let message = refused(PrivateFolder::open_or_create_for(&place.cache(), stranger));
        assert!(message.contains("another user"), "{message}");
    }

    #[test]
    fn missing_owner_bits_of_its_own_folder_are_restored() {
        let place = Place::new();
        fs::create_dir(place.cache()).unwrap();
        fs::set_permissions(place.cache(), fs::Permissions::from_mode(0o500)).unwrap();
        PrivateFolder::open_or_create(&place.cache()).unwrap();
        assert_eq!(mode(&place.cache()), 0o700);
    }

    #[test]
    fn a_linked_file_below_the_folder_is_not_followed() {
        let place = Place::new();
        let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
        let secret = place.0.join("secret");
        fs::write(&secret, b"secret").unwrap();
        symlink(&secret, place.cache().join("entry.fyc")).unwrap();
        assert!(folder.open_file(Path::new("entry.fyc")).is_err());
        assert!(folder.replace_file(Path::new("entry.fyc")).is_err());
        assert_eq!(fs::read(&secret).unwrap(), b"secret");
    }

    #[test]
    fn a_linked_folder_below_the_folder_is_not_entered() {
        let place = Place::new();
        let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
        let outside = place.0.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, place.cache().join("root")).unwrap();
        assert!(folder.create_file(Path::new("root/entry.fyc")).is_err());
        assert!(folder.create_folders(Path::new("root/collection")).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }
}

#[cfg(windows)]
mod windows {
    use super::{Place, PrivateFolder, refused};
    use crate::temp_tier::windows_folder::WindowsFolder;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn a_new_folder_grants_only_the_current_user() {
        let place = Place::new();
        PrivateFolder::open_or_create(&place.cache()).unwrap();
        assert!(WindowsFolder::is_private(&place.cache()).unwrap());
    }

    #[test]
    fn a_folder_is_private_as_it_is_created() {
        let place = Place::new();
        WindowsFolder::create_private(&place.cache()).unwrap();
        assert!(WindowsFolder::grants_only_current_user(&place.cache()).unwrap());
        assert!(WindowsFolder::is_private(&place.cache()).unwrap());
        let error = WindowsFolder::create_private(&place.cache()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn folders_below_it_inherit_its_private_access() {
        let place = Place::new();
        let folder = PrivateFolder::open_or_create(&place.cache()).unwrap();
        folder.create_folders(Path::new("root/collection")).unwrap();
        folder.create_file(Path::new("root/entry.fyc")).unwrap();
        assert!(
            WindowsFolder::grants_only_current_user(&place.cache().join("root/collection"))
                .unwrap()
        );
        assert!(
            WindowsFolder::grants_only_current_user(&place.cache().join("root/entry.fyc")).unwrap()
        );
    }

    #[test]
    fn a_folder_another_account_owns_is_not_the_users() {
        let place = Place::new();
        PrivateFolder::open_or_create(&place.cache()).unwrap();
        assert!(WindowsFolder::owned_by_current_user(&place.cache()).unwrap());
        let system = std::env::var_os("SystemRoot").unwrap();
        assert!(!WindowsFolder::owned_by_current_user(Path::new(&system)).unwrap());
        assert!(!WindowsFolder::is_private(Path::new(&system)).unwrap());
    }

    #[test]
    fn a_folder_that_inherits_shared_access_is_refused() {
        let place = Place::new();
        fs::create_dir(place.cache()).unwrap();
        let message = refused(PrivateFolder::open_or_create(&place.cache()));
        assert!(message.contains("other accounts"), "{message}");
    }

    #[test]
    fn a_junction_is_refused() {
        let place = Place::new();
        let target = place.0.join("elsewhere");
        PrivateFolder::open_or_create(&target).unwrap();
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(place.cache())
            .arg(&target)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        let message = refused(PrivateFolder::open_or_create(&place.cache()));
        assert!(message.contains("reparse point"), "{message}");
    }
}
