use std::io::Write;
use std::path::{Path, PathBuf};

use fylo_vfs::{self as fs, OpenOptions};

use super::super::{restore_attributes, safe_relative_path, sibling_scratch};
use super::commit_step::CommitStep;
use super::move_step::MoveStep;
use super::record_image::RecordImage;
use super::update_step::UpdateStep;
use crate::platform_companion::remove_companion;
use crate::{ExpectedType, NativeRoot, NativeStorageError, NativeStorageErrorCode};
use crate::{path_exists_no_follow, write::failpoint};

/// Brings record files up to the post-images of committed steps, without
/// flushing: the log already holds every byte, so a crash is repaired by
/// replaying it.
pub(crate) struct StepApplier<'a> {
    root: &'a NativeRoot,
    collection: &'a Path,
    replay: bool,
}

impl<'a> StepApplier<'a> {
    /// An applier for a commit this process just made durable. Its paths were
    /// verified when the plan was built under the same lock.
    pub(crate) fn live(root: &'a NativeRoot, collection: &'a Path) -> Self {
        Self {
            root,
            collection,
            replay: false,
        }
    }

    /// An applier for records read back from the log, whose paths are
    /// verified again and whose blobs are checked against their digests.
    pub(crate) fn replay(root: &'a NativeRoot, collection: &'a Path) -> Self {
        Self {
            root,
            collection,
            replay: true,
        }
    }

    pub(crate) fn apply(&self, step: &CommitStep, blobs: &[u8]) -> Result<(), NativeStorageError> {
        match step {
            CommitStep::Update(update) => self.apply_update(update, blobs),
            CommitStep::Move(rename) => self.apply_move(rename),
        }?;
        failpoint("after-log-apply-step")
    }

    /// Undo a live commit's steps in reverse from the plan's pre-images.
    pub(crate) fn undo(
        &self,
        steps: &[CommitStep],
        pre_images: &[(PathBuf, Option<RecordImage>)],
    ) -> Result<(), NativeStorageError> {
        for step in steps.iter().rev() {
            if let CommitStep::Move(rename) = step {
                self.undo_move(rename)?;
            }
        }
        for (path, image) in pre_images.iter().rev() {
            self.restore_image(path, image.as_ref())?;
        }
        Ok(())
    }

    fn apply_update(&self, step: &UpdateStep, blobs: &[u8]) -> Result<(), NativeStorageError> {
        let path = self.resolve(&step.path)?;
        let bytes = step
            .blob
            .as_ref()
            .map(|blob| self.blob(blob, blobs))
            .transpose()?;
        if step.create {
            return self.create(&path, step, bytes.unwrap_or_default());
        }
        if !path_exists_no_follow(&path)? {
            return self.missing_update_target();
        }
        if self.replay {
            self.root.verify_path(&path, ExpectedType::File)?;
        }
        Self::rewrite_in_place(&path, step, bytes)
    }

    fn create(
        &self,
        path: &Path,
        step: &UpdateStep,
        bytes: &[u8],
    ) -> Result<(), NativeStorageError> {
        let parent = path.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "log path has no parent")
        })?;
        self.create_directories(parent)?;
        let scratch = sibling_scratch(path);
        let outcome = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&scratch)
                .map_err(NativeStorageError::io)?;
            file.write_all(bytes).map_err(NativeStorageError::io)?;
            drop(file);
            Self::finish_file(&scratch, step)?;
            fs::rename(&scratch, path).map_err(NativeStorageError::io)
        })();
        if outcome.is_err() && path_exists_no_follow(&scratch)? {
            fs::remove_file(&scratch).map_err(NativeStorageError::io)?;
        }
        outcome
    }

    fn rewrite_in_place(
        path: &Path,
        step: &UpdateStep,
        bytes: Option<&[u8]>,
    ) -> Result<(), NativeStorageError> {
        if let Some(bytes) = bytes {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .map_err(NativeStorageError::io)?;
            file.write_all(bytes).map_err(NativeStorageError::io)?;
        }
        Self::finish_file(path, step)
    }

    fn finish_file(path: &Path, step: &UpdateStep) -> Result<(), NativeStorageError> {
        if let Some(attributes) = &step.attributes {
            restore_attributes(path, attributes)?;
        }
        Self::set_modified(path, step.modified_ms)?;
        Self::set_ownership(path, step)
    }

    fn apply_move(&self, rename: &MoveStep) -> Result<(), NativeStorageError> {
        let from = self.resolve(&rename.from)?;
        let to = self.resolve(&rename.to)?;
        if path_exists_no_follow(&to)? {
            return Ok(());
        }
        if !path_exists_no_follow(&from)? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "a logged move has neither its source nor its destination",
            ));
        }
        if self.replay {
            self.root.verify_path(&from, ExpectedType::File)?;
        }
        let parent = to.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "log path has no parent")
        })?;
        self.create_directories(parent)?;
        fs::rename(&from, &to).map_err(NativeStorageError::io)
    }

    fn undo_move(&self, rename: &MoveStep) -> Result<(), NativeStorageError> {
        let from = self.resolve(&rename.from)?;
        let to = self.resolve(&rename.to)?;
        if path_exists_no_follow(&to)? && !path_exists_no_follow(&from)? {
            fs::rename(&to, &from).map_err(NativeStorageError::io)?;
        }
        Ok(())
    }

    fn restore_image(
        &self,
        path: &Path,
        image: Option<&RecordImage>,
    ) -> Result<(), NativeStorageError> {
        let Some(image) = image else {
            return Self::remove_if_present(path);
        };
        let step = UpdateStep {
            path: String::new(),
            create: false,
            blob: None,
            attributes: image.attributes.clone(),
            mode: image.mode,
            uid: None,
            gid: None,
            modified_ms: image.modified_ms,
        };
        if path_exists_no_follow(path)? {
            return Self::rewrite_in_place(path, &step, Some(&image.bytes));
        }
        self.create(path, &step, &image.bytes)
    }

    fn remove_if_present(path: &Path) -> Result<(), NativeStorageError> {
        match fs::remove_file(path) {
            Ok(()) => {
                remove_companion(path);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn missing_update_target(&self) -> Result<(), NativeStorageError> {
        if self.replay {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::NotFound,
            "record disappeared while its commit was applied",
        ))
    }

    fn resolve(&self, relative: &str) -> Result<PathBuf, NativeStorageError> {
        Ok(self.collection.join(safe_relative_path(relative)?))
    }

    fn blob<'b>(
        &self,
        blob: &super::blob_reference::BlobReference,
        blobs: &'b [u8],
    ) -> Result<&'b [u8], NativeStorageError> {
        let start = usize::try_from(blob.offset).ok();
        let end = start.and_then(|start| start.checked_add(usize::try_from(blob.length).ok()?));
        let bytes = start
            .zip(end)
            .and_then(|(start, end)| blobs.get(start..end))
            .ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "log blob lies outside its record",
                )
            })?;
        if self.replay && crate::sha256_hex(bytes) != blob.sha256 {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "log blob does not match its digest",
            ));
        }
        Ok(bytes)
    }

    fn create_directories(&self, directory: &Path) -> Result<(), NativeStorageError> {
        if path_exists_no_follow(directory)? {
            return self.verify_directory(directory);
        }
        let relative = directory.strip_prefix(self.collection).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "log directory escapes its collection",
            )
        })?;
        let mut current = self.collection.to_owned();
        for component in relative.components() {
            current.push(component);
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(NativeStorageError::io(error)),
            }
        }
        self.verify_directory(directory)
    }

    fn verify_directory(&self, directory: &Path) -> Result<(), NativeStorageError> {
        if self.replay {
            self.root.verify_path(directory, ExpectedType::Directory)?;
        }
        Ok(())
    }

    fn set_modified(path: &Path, modified_ms: u64) -> Result<(), NativeStorageError> {
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_millis(modified_ms);
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.set_times(
            fs::FileTimes::new()
                .set_accessed(modified)
                .set_modified(modified),
        )
        .map_err(NativeStorageError::io)
    }

    #[cfg(unix)]
    fn set_ownership(path: &Path, step: &UpdateStep) -> Result<(), NativeStorageError> {
        use std::os::unix::fs::PermissionsExt;
        if step.uid.is_some() || step.gid.is_some() {
            std::os::unix::fs::chown(path, step.uid, step.gid).map_err(NativeStorageError::io)?;
        }
        if let Some(mode) = step.mode {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .map_err(NativeStorageError::io)?;
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn set_ownership(_path: &Path, step: &UpdateStep) -> Result<(), NativeStorageError> {
        if step.uid.is_some() || step.gid.is_some() || step.mode.is_some() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "UID/GID/mode access control is available only on POSIX platforms",
            ));
        }
        Ok(())
    }
}
