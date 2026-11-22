use std::path::Path;

use super::attribute_plan::AttributePlan;
use super::sidecar_plan::SidecarPlan;
use super::{CapturedAttribute, MetadataPlanInput, SidecarImage};
use crate::{FilesystemProfile, NativeRoot, NativeStorageError};

/// The final metadata of one record: the complete extended attribute set
/// (`None` leaves the attributes as they are) and the sidecar file's final
/// state. Planning writes nothing, so a write-ahead log can record the plan
/// and replay it.
#[derive(Clone, Debug)]
pub(crate) struct MetadataPlan {
    xattrs: Option<Vec<CapturedAttribute>>,
    sidecar: SidecarImage,
}

impl MetadataPlan {
    /// Plan the metadata of the record at `path` after `input`'s changes.
    pub(crate) fn for_record(
        root: &NativeRoot,
        path: &Path,
        input: &MetadataPlanInput<'_>,
    ) -> Result<Self, NativeStorageError> {
        Self::plan_for(root.filesystem_profile(), root, path, input)
    }

    /// The complete extended attribute set to write, or `None` to leave the
    /// attributes untouched.
    pub(crate) fn xattrs(&self) -> Option<&[CapturedAttribute]> {
        self.xattrs.as_deref()
    }

    /// The sidecar file's final state.
    pub(crate) const fn sidecar(&self) -> &SidecarImage {
        &self.sidecar
    }

    pub(super) const fn new(xattrs: Option<Vec<CapturedAttribute>>, sidecar: SidecarImage) -> Self {
        Self { xattrs, sidecar }
    }

    fn plan_for(
        profile: FilesystemProfile,
        root: &NativeRoot,
        path: &Path,
        input: &MetadataPlanInput<'_>,
    ) -> Result<Self, NativeStorageError> {
        match profile {
            FilesystemProfile::Native => AttributePlan::post_image(path, input.changes()),
            FilesystemProfile::EfsSidecarV1 => SidecarPlan::post_image(root, path, input),
        }
    }
}
