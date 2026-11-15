use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

use fylo_storage_native::{FilesystemProfile, RootOpenOptions};
use serde_json::Value;
#[cfg(not(target_family = "wasm"))]
use serde_json::json;

use crate::application::ApplicationConfig;

/// A session's temp-disk read tier (ADR 0030 sections 2 and 7): decided
/// once, when the session opens its default root, and handed to every
/// engine the session opens on that root.
#[derive(Clone, Debug)]
pub(crate) struct SessionTempCache {
    #[cfg(not(target_family = "wasm"))]
    state: fylo_storage_native::TempCacheState,
    #[cfg(not(target_family = "wasm"))]
    root: Option<PathBuf>,
}

#[cfg(not(target_family = "wasm"))]
impl SessionTempCache {
    /// The tier for a session whose default root is `root`, opened with
    /// `profile`, under the application's temp cache settings.
    pub(crate) fn decide(
        config: &ApplicationConfig,
        root: Option<&Path>,
        profile: FilesystemProfile,
    ) -> Self {
        let cache_root = fylo_storage_native::TempCacheRoot::new(root, profile);
        let state = fylo_storage_native::TempCache::start(
            &config.temp_cache,
            &cache_root,
            std::time::SystemTime::now(),
        );
        Self {
            state,
            root: root.map(Path::to_path_buf),
        }
    }

    /// A session with the tier off.
    #[cfg(test)]
    pub(crate) const fn off() -> Self {
        Self {
            state: fylo_storage_native::TempCacheState::Off,
            root: None,
        }
    }

    /// `capabilities.tempCache`: `false` under `--temp-cache off`, otherwise
    /// the tier's status with this process's counters.
    pub(crate) fn capability(&self) -> Value {
        self.state
            .status()
            .map_or(Value::Bool(false), |status| json!(status))
    }

    /// `options` for the root `repository_root`, reading through the tier
    /// when it is active and the root is the one it was decided for.
    pub(crate) fn attach(
        &self,
        options: RootOpenOptions,
        repository_root: &Path,
    ) -> RootOpenOptions {
        let tier = self
            .state
            .tier()
            .filter(|_| self.decided_for(repository_root));
        match tier {
            Some(tier) => options.with_temp_tier(tier),
            None => options,
        }
    }

    fn decided_for(&self, repository_root: &Path) -> bool {
        self.root.as_deref() == Some(repository_root)
    }
}

/// The browser engine never uses the temp tier: OPFS is its derived store
/// (ADR 0003).
#[cfg(target_family = "wasm")]
impl SessionTempCache {
    pub(crate) const fn decide(
        _config: &ApplicationConfig,
        _root: Option<&Path>,
        _profile: FilesystemProfile,
    ) -> Self {
        Self {}
    }

    #[cfg(test)]
    pub(crate) const fn off() -> Self {
        Self {}
    }

    #[expect(
        clippy::unused_self,
        reason = "the browser engine never has a temp tier"
    )]
    pub(crate) fn capability(&self) -> Value {
        Value::Bool(false)
    }

    #[expect(
        clippy::unused_self,
        reason = "the browser engine never has a temp tier"
    )]
    pub(crate) fn attach(
        &self,
        options: RootOpenOptions,
        _repository_root: &Path,
    ) -> RootOpenOptions {
        options
    }
}
