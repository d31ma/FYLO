use fylo_identity::SessionKey;
#[cfg(not(target_family = "wasm"))]
use fylo_identity::{Identity, IdentityError};

#[cfg(not(target_family = "wasm"))]
use crate::access_binding::AccessBinding;
#[cfg(not(target_family = "wasm"))]
use crate::session_certificate::SessionCertificate;

/// The session a writer signs records with (ADR 0021, phase 8): a fresh
/// in-memory key, and the certificate its identity signed for it once.
/// The browser never makes one (decision P5-5).
pub(crate) struct WriterSession {
    key: SessionKey,
    certificate: Vec<u8>,
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    not_after: u64,
}

impl WriterSession {
    /// How long a session signs, by the writer's clock: a day.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) const LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
    /// A session this close to its end is replaced before it signs again,
    /// so a record is never signed after `not_after`.
    #[cfg(not(target_family = "wasm"))]
    const RENEW_BEFORE_MS: u64 = 5 * 60 * 1000;

    /// A new session, certified by one signature of `identity`.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn issue(
        identity: &Identity,
        binding: AccessBinding,
        now: u64,
    ) -> Result<WriterSession, IdentityError> {
        let key = SessionKey::generate()?;
        let not_after = now.saturating_add(Self::LIFETIME_MS);
        let mut certificate =
            SessionCertificate::unsigned(identity.public().fingerprint(), &key, not_after);
        let signature = identity.try_sign(&certificate.message(binding))?;
        certificate.signature = signature.as_slice().try_into().map_err(|_| {
            IdentityError::Store(format!("{} gave a malformed signature", identity.id()))
        })?;
        Ok(WriterSession {
            key,
            certificate: certificate.to_bytes(),
            not_after,
        })
    }

    /// Whether this session may still sign at `now`.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn is_current(&self, now: u64) -> bool {
        now.saturating_add(Self::RENEW_BEFORE_MS) < self.not_after
    }

    /// The certificate, as a frame's trailer carries it.
    pub(crate) fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// The session key's signature over `message`.
    pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.key.sign(message)
    }
}

#[cfg(test)]
mod tests {
    use fylo_identity::{Identity, KeyReference};

    use super::WriterSession;
    use crate::access_binding::AccessBinding;
    use crate::session_certificate::SessionCertificate;

    struct Key(std::path::PathBuf);

    impl Key {
        fn new(name: &str) -> Key {
            let path = std::env::temp_dir()
                .join(crate::write::unique_name(&format!("session-{name}")))
                .with_extension("pem");
            Key(path)
        }

        fn identity(&self) -> Identity {
            Identity::create(&KeyReference::File(self.0.clone())).unwrap()
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const NOW: u64 = 1_700_000_000_000;

    #[test]
    fn a_session_signs_until_shortly_before_its_end() {
        let key = Key::new("current");
        let session =
            WriterSession::issue(&key.identity(), AccessBinding::new([3; 32]), NOW).unwrap();
        assert!(session.is_current(NOW));
        assert!(session.is_current(NOW + WriterSession::LIFETIME_MS / 2));
        assert!(!session.is_current(NOW + WriterSession::LIFETIME_MS - 1));
        assert!(!session.is_current(NOW + WriterSession::LIFETIME_MS));
    }

    #[test]
    fn its_certificate_is_the_identity_s_for_this_database() {
        let key = Key::new("certificate");
        let identity = key.identity();
        let binding = AccessBinding::new([3; 32]);
        let session = WriterSession::issue(&identity, binding, NOW).unwrap();
        let certificate = SessionCertificate::parse(session.certificate()).unwrap();
        assert_eq!(certificate.fingerprint, identity.public().fingerprint());
        assert_eq!(certificate.not_after, NOW + WriterSession::LIFETIME_MS);
        let public = identity.public();
        let session_key = certificate.verified_session(binding, &public).unwrap();
        session_key
            .verify(b"record", &session.sign(b"record"))
            .unwrap();
        assert!(
            certificate
                .verified_session(AccessBinding::new([4; 32]), &public)
                .is_none()
        );
    }
}
