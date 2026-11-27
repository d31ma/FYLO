use crate::{CollectionIncarnation, FileStamp, GenerationState, GenerationStatus};

/// What a collection is right now, as a cache must see it before it trusts
/// anything it kept (ADR 0030 section 3.1).
///
/// Two equal stamps mean the collection has not changed in any way between
/// them: no commit (the generation and write-ahead log end), no replaced
/// `state.json` (a restore, a branch checkout, a copied collection, or an
/// older release rewriting it, even with the same generation), and no new
/// life of the collection (the incarnation). A generation number alone
/// cannot promise that (`docs/QUERY_INDEX_REUSE.md`).
///
/// A stamp that is not [cacheable](Self::is_cacheable) must never be used
/// to keep or reuse anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionStamp {
    state: GenerationState,
    state_file: Option<FileStamp>,
    incarnation: CollectionIncarnation,
    files_layout: bool,
}

impl CollectionStamp {
    pub(crate) const fn new(
        state: GenerationState,
        state_file: Option<FileStamp>,
        incarnation: CollectionIncarnation,
        files_layout: bool,
    ) -> Self {
        Self {
            state,
            state_file,
            incarnation,
            files_layout,
        }
    }

    /// Whether a cache may keep or reuse what it read under this stamp.
    ///
    /// Only a files-layout collection of a folder root, at a stable
    /// generation, that can be told apart from another life of itself: by
    /// its `state.json` or by its incarnation. A collection with neither
    /// reads as generation zero, which a re-created collection also reads.
    #[must_use]
    pub fn is_cacheable(&self) -> bool {
        ![self.files_layout, self.is_stable(), self.is_identified()].contains(&false)
    }

    /// The generation state the stamp was read at.
    #[must_use]
    pub const fn state(&self) -> &GenerationState {
        &self.state
    }

    /// The incarnation the stamp was read at.
    #[must_use]
    pub const fn incarnation(&self) -> CollectionIncarnation {
        self.incarnation
    }

    #[cfg(not(target_family = "wasm"))]
    /// A SHA-256 digest of every part of the stamp, the same in every
    /// process: equal stamps give equal digests, so a temp entry keeps the
    /// digest of the stamp it was read under (ADR 0030 section 4.4).
    pub(crate) fn digest(&self) -> [u8; 32] {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        hasher.update(serde_json::to_vec(&self.state).unwrap_or_default());
        self.feed_state_file(&mut hasher);
        hasher.update(self.incarnation.encode());
        hasher.update([u8::from(self.files_layout)]);
        hasher.finalize().into()
    }

    #[cfg(not(target_family = "wasm"))]
    fn feed_state_file(&self, hasher: &mut sha2::Sha256) {
        use sha2::Digest;

        let Some(state_file) = &self.state_file else {
            hasher.update([0_u8]);
            return;
        };
        hasher.update([1_u8]);
        state_file.feed(hasher);
    }

    fn is_stable(&self) -> bool {
        self.state.state == GenerationStatus::Stable
    }

    fn is_identified(&self) -> bool {
        self.state_file.is_some() || self.incarnation.is_known()
    }
}
