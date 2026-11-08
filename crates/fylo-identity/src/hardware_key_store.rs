use crate::hardware_key::HardwareKey;
use crate::identity_error::IdentityError;

/// Where a key reference keeps a hardware key (ADR 0021, phase 8). The
/// hardware makes the key; FYLO only names it.
pub(crate) trait HardwareKeyStore {
    /// Make a new key under this name. Fails with
    /// [`IdentityError::AlreadyExists`] rather than replace one.
    fn create(&self) -> Result<Box<dyn HardwareKey>, IdentityError>;

    /// Open the key stored under this name.
    fn open(&self) -> Result<Box<dyn HardwareKey>, IdentityError>;
}
