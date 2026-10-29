//! The medium of one collection over a shared open: the one collection of
//! a hybrid collection directory's file, or one of the many of a `.vdbx`
//! database root (`docs/vdbx-segment-backend.md`, "Root integration").

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use super::dictionaries::FileDictionaries;
use super::{ContainerMedium, ContainerOpen, MediumState, Shared};
use crate::ContainerCollection;
use crate::segments::{ContainerFrames, MediumIdentity};

/// Media open in this process, by file and directory block.
type Media = BTreeMap<(PathBuf, u32), Weak<Shared>>;

impl ContainerMedium {
    /// The medium of `collection` in the file `file` has open. Every medium
    /// of one collection open at once is one; the registry keeps none
    /// alive, so a medium closes, and gives up its claim on the collection,
    /// when the last operation holding it ends.
    pub(crate) fn attach(file: &Arc<ContainerOpen>, collection: &ContainerCollection) -> Self {
        let key = (file.path().to_owned(), collection.block);
        let (capacity, cipher) = {
            let open = file.lock();
            (open.payload_capacity(), open.shared_cipher())
        };
        let mut media = Self::media();
        media.retain(|_, shared| shared.strong_count() > 0);
        if let Some(shared) = media.get(&key).and_then(Weak::upgrade)
            && Arc::ptr_eq(&shared.file, file)
            && shared.collection == collection.header.id
        {
            return Self { shared };
        }
        let shared = Arc::new(Shared {
            file: Arc::clone(file),
            identity: Self::identity_of(file.path(), collection),
            collection: collection.header.id,
            block: collection.block,
            capacity,
            frames: Arc::new(
                ContainerFrames::with_source(
                    collection.header.id,
                    Arc::new(FileDictionaries {
                        file: Arc::clone(file),
                        collection: collection.header.id,
                    }),
                )
                .sealed_with(cipher),
            ),
            state: Mutex::new(MediumState::default()),
            claim: Mutex::new(None),
            writes: AtomicUsize::new(0),
        });
        media.insert(key, Arc::downgrade(&shared));
        Self { shared }
    }

    /// The identity of `collection` in the file at `path`: the path, the
    /// collection's id and a digest of its descriptor, which names when it
    /// was created.
    pub(crate) fn identity_of(path: &Path, collection: &ContainerCollection) -> MediumIdentity {
        let mut identity = OsString::from(path.as_os_str());
        identity.push(format!(
            "#{}#{}",
            collection.header.id,
            &crate::sha256_hex(&collection.header.descriptor)[..16]
        ));
        MediumIdentity::new(identity)
    }

    /// Forget every medium of the file at `path`, as [`ContainerOpen::forget`]
    /// does its open.
    pub(super) fn forget_media(path: &Path) {
        Self::media().retain(|(file, _), _| file != path);
    }

    fn media() -> MutexGuard<'static, Media> {
        static MEDIA: OnceLock<Mutex<Media>> = OnceLock::new();
        MEDIA
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
