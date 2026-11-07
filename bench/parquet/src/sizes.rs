use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde_json::{Value, json};

/// What a store occupies: files, the bytes they hold, and the bytes the
/// filesystem allocated for them.
#[derive(Default)]
pub struct Footprint {
    pub files: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

impl Footprint {
    pub fn measure(path: &Path) -> Self {
        let mut footprint = Self::default();
        footprint.visit(path);
        footprint
    }

    fn visit(&mut self, path: &Path) {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if metadata.is_dir() {
            self.directories += 1;
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                self.visit(&entry.path());
            }
            return;
        }
        self.files += 1;
        self.logical_bytes += metadata.len();
        self.allocated_bytes += metadata.blocks() * 512;
    }

    pub fn to_json(&self) -> Value {
        json!({
            "files": self.files,
            "directories": self.directories,
            "logical_bytes": self.logical_bytes,
            "allocated_bytes": self.allocated_bytes,
        })
    }
}
