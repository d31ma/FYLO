use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use byckr_vfs::File;

use super::{CapturedAttribute, MetadataChanges, MetadataPlan, SidecarImage};
use crate::NativeStorageError;

/// Plans the metadata of a record kept in extended attributes.
pub(super) struct AttributePlan;

impl AttributePlan {
    pub(super) fn post_image(
        path: &Path,
        changes: &MetadataChanges,
    ) -> Result<MetadataPlan, NativeStorageError> {
        let current = Self::current_values(path)?;
        let next = changes.applied_to(current);
        Ok(MetadataPlan::new(
            Some(Self::captured(&next)),
            SidecarImage::Keep,
        ))
    }

    fn current_values(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        match File::open(path) {
            Ok(file) => crate::read_byckr_attributes(&file, path),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn captured(values: &BTreeMap<String, Vec<u8>>) -> Vec<CapturedAttribute> {
        let mut attributes = Vec::new();
        for (name, value) in values {
            attributes.push(CapturedAttribute {
                name: name.clone(),
                value: BASE64.encode(value),
            });
        }
        attributes
    }
}

#[cfg(all(test, unix))]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use byckr_xattr::FileExt;

    use super::AttributePlan;
    use crate::record_metadata::{MetadataChanges, SidecarImage};

    fn record_file(name: &str) -> std::path::PathBuf {
        let folder =
            std::env::temp_dir().join(format!("fylo-attribute-plan-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("record.bin");
        std::fs::write(&path, b"payload").unwrap();
        path
    }

    #[test]
    fn the_plan_is_the_complete_attribute_set_after_the_changes() {
        let path = record_file("complete");
        let file = std::fs::File::open(&path).unwrap();
        file.set_xattr("user.fylo.meta.old", b"1").unwrap();
        file.set_xattr("user.fylo.meta.kept", b"2").unwrap();
        let changes = MetadataChanges::new()
            .remove("user.fylo.meta.old")
            .set("user.fylo.meta.new", b"3");

        let plan = AttributePlan::post_image(&path, &changes).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();

        let names: Vec<(&str, Vec<u8>)> = plan
            .xattrs()
            .unwrap()
            .iter()
            .map(|attribute| {
                (
                    attribute.name.as_str(),
                    BASE64.decode(&attribute.value).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("user.fylo.meta.kept", b"2".to_vec()),
                ("user.fylo.meta.new", b"3".to_vec())
            ]
        );
        assert_eq!(plan.sidecar(), &SidecarImage::Keep);
    }

    #[test]
    fn a_record_not_written_yet_starts_with_no_attributes() {
        let path = record_file("missing").with_file_name("not-written.bin");
        let changes = MetadataChanges::new().set("user.fylo.meta.a", b"1");

        let plan = AttributePlan::post_image(&path, &changes).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();

        assert_eq!(plan.xattrs().unwrap().len(), 1);
        assert_eq!(plan.xattrs().unwrap()[0].name, "user.fylo.meta.a");
    }
}
