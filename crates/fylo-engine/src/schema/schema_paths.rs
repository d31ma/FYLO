use std::path::{Path, PathBuf};

pub(super) struct SchemaPaths<'a> {
    pub(super) root: &'a Path,
    pub(super) collection: &'a str,
}

impl<'a> SchemaPaths<'a> {
    pub(super) fn new(root: &'a Path, collection: &'a str) -> Self {
        Self { root, collection }
    }

    pub(super) fn manifest(&self) -> PathBuf {
        self.root.join(self.collection).join("manifest.json")
    }

    pub(super) fn version(&self, version: &str) -> PathBuf {
        self.root
            .join(self.collection)
            .join("history")
            .join(format!("{version}.schema.json"))
    }

    pub(super) fn upgrader(&self, from: &str, to: &str) -> PathBuf {
        self.root
            .join(self.collection)
            .join("upgraders")
            .join(format!("{from}-to-{to}.js"))
    }

    pub(super) fn flat(&self) -> PathBuf {
        self.root.join(format!("{}.schema.json", self.collection))
    }
}
