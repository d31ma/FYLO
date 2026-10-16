use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::SchemaTools;

pub(super) struct SchemaFixture(pub(super) PathBuf);

impl SchemaFixture {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fylo-schema-rules-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(directory.join("people/history")).unwrap();
        fs::create_dir_all(directory.join("people/upgraders")).unwrap();
        Self(directory)
    }

    pub(super) fn tools(&self) -> SchemaTools {
        SchemaTools::new(&self.0)
    }

    pub(super) fn manifest(&self, value: &Value) {
        fs::write(
            self.0.join("people/manifest.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    pub(super) fn versions(&self) {
        self.manifest(&json!({"current":"v2","versions":[{"v":"v1"},{"v":"v2"}]}));
    }

    pub(super) fn upgrader(&self, source: &[u8]) {
        fs::write(self.0.join("people/upgraders/v1-to-v2.js"), source).unwrap();
    }

    #[cfg(unix)]
    pub(super) fn validator(&self, response: &str) -> SchemaTools {
        self.validator_script(&format!(
            "while IFS= read -r request; do\nprintf '%s\\n' '{response}'\ndone\n"
        ))
    }

    #[cfg(unix)]
    pub(super) fn validator_script(&self, source: &str) -> SchemaTools {
        use std::os::unix::fs::PermissionsExt;

        let path = self.0.join("validator.sh");
        fs::write(&path, format!("#!/bin/sh\n{source}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        self.tools().with_chex_binary(path)
    }
}

impl Drop for SchemaFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
