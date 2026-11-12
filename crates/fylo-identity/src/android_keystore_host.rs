use std::sync::OnceLock;

use crate::android_keystore::AndroidKeystore;
use crate::identity_error::IdentityError;

/// Where FYLO finds the [`AndroidKeystore`] for `android-keystore:` keys:
/// installed once per process by FYLO's Android library.
pub struct AndroidKeystoreHost;

impl AndroidKeystoreHost {
    /// Install the Keystore. Only the first install counts: a process has
    /// one Keystore.
    pub fn install(keystore: Box<dyn AndroidKeystore>) {
        let _ = Self::slot().set(keystore);
    }

    pub(crate) fn installed() -> Result<&'static dyn AndroidKeystore, IdentityError> {
        Self::slot().get().map(Box::as_ref).ok_or_else(|| {
            IdentityError::Unsupported(
                "android-keystore: keys need Android 12 or newer and FYLO's Android library \
                 (io.github.d31ma:fylo-android); use file:<path> or the store of this system"
                    .to_owned(),
            )
        })
    }

    fn slot() -> &'static OnceLock<Box<dyn AndroidKeystore>> {
        static SLOT: OnceLock<Box<dyn AndroidKeystore>> = OnceLock::new();
        &SLOT
    }
}
