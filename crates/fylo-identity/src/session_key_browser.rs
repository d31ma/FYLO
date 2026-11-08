use crate::identity_error::IdentityError;

/// The browser build has no session keys: its `webcrypto:` identity signs
/// each record through the host, and P-256 stays out of the module (ADR
/// 0021, decision P5-5). It still verifies records native session keys
/// signed.
pub struct SessionKey {
    point: [u8; 65],
}

impl SessionKey {
    /// Always refused in the browser.
    ///
    /// # Errors
    /// [`IdentityError::Unsupported`].
    pub fn generate() -> Result<SessionKey, IdentityError> {
        Err(IdentityError::Unsupported(
            "session keys are native only; the browser signs each record with its webcrypto: identity".into(),
        ))
    }

    /// The public key, a 65-byte SEC1 uncompressed point.
    #[must_use]
    pub fn public_point(&self) -> [u8; 65] {
        self.point
    }

    /// No signature: a browser session key cannot exist.
    #[must_use]
    pub fn sign(&self, _message: &[u8]) -> Vec<u8> {
        Vec::new()
    }
}
