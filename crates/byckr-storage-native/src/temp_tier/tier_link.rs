use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

#[cfg(not(target_family = "wasm"))]
use super::entry_key::EntryKey;
#[cfg(not(target_family = "wasm"))]
use super::temp_cache_tier::TempCacheTier;
#[cfg(not(target_family = "wasm"))]
use super::volume_facts::VolumeFacts;

/// The temp tier a root reads through, if any, with the key that names the
/// root's entries (ADR 0030 sections 2 and 4.3).
///
/// A host passes a tier with [`crate::RootOpenOptions::with_temp_tier`];
/// opening the root binds it to the root's canonical path and device. The
/// browser engine never has one.
#[derive(Clone, Debug, Default)]
pub(crate) struct TierLink {
    #[cfg(not(target_family = "wasm"))]
    tier: Option<Arc<TempCacheTier>>,
    #[cfg(not(target_family = "wasm"))]
    root_key: Option<Arc<str>>,
}

#[cfg(not(target_family = "wasm"))]
impl TierLink {
    /// A link to `tier`, not yet bound to a root.
    pub(crate) fn to(tier: Arc<TempCacheTier>) -> Self {
        Self {
            tier: Some(tier),
            root_key: None,
        }
    }

    /// This link bound to the root at `canonical`. A root whose device
    /// cannot be read reads without the tier.
    pub(crate) fn bind(self, canonical: &Path) -> Self {
        let device = VolumeFacts::device(canonical).ok();
        let root_key = device.map(|device| Arc::from(EntryKey::root_key(canonical, device)));
        Self {
            tier: self.tier.filter(|_| root_key.is_some()),
            root_key,
        }
    }

    /// The tier and the root's key, when the root reads through one.
    pub(crate) fn tier(&self) -> Option<(&TempCacheTier, &str)> {
        Some((self.tier.as_deref()?, self.root_key.as_deref()?))
    }
}

#[cfg(target_family = "wasm")]
impl TierLink {
    /// The browser engine never has a temp tier.
    pub(crate) fn bind(self, _canonical: &Path) -> Self {
        self
    }
}
