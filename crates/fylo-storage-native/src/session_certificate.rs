use fylo_identity::PublicIdentity;
#[cfg(not(target_family = "wasm"))]
use fylo_identity::SessionKey;

use crate::access_binding::AccessBinding;

/// A session certificate (ADR 0021, phase 8): an identity's statement that
/// a session key may sign this database's records for it. 169 bytes in a
/// session-signed frame's trailer: the identity's fingerprint (32), the
/// session's public key as a SEC1 uncompressed point (65), the time after
/// which the writer signs nothing more with it, in Unix milliseconds by the
/// writer's clock (8, little-endian), and the identity's ECDSA P-256
/// signature over [`AccessBinding::session_message`] (64).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionCertificate {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) session_key: [u8; 65],
    pub(crate) not_after: u64,
    pub(crate) signature: [u8; 64],
}

impl SessionCertificate {
    pub(crate) const BYTES: usize = 32 + 65 + 8 + 64;

    /// The certificate `session` gets, before the identity signs it.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn unsigned(
        fingerprint: [u8; 32],
        session: &SessionKey,
        not_after: u64,
    ) -> SessionCertificate {
        SessionCertificate {
            fingerprint,
            session_key: session.public_point(),
            not_after,
            signature: [0; 64],
        }
    }

    /// What the identity signs.
    pub(crate) fn message(&self, binding: AccessBinding) -> Vec<u8> {
        binding.session_message(&self.fingerprint, &self.session_key, self.not_after)
    }

    /// The certificate as it is stored.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        [
            &self.fingerprint[..],
            &self.session_key,
            &self.not_after.to_le_bytes(),
            &self.signature,
        ]
        .concat()
    }

    /// Read a stored certificate; `None` unless `bytes` is exactly one.
    pub(crate) fn parse(bytes: &[u8]) -> Option<SessionCertificate> {
        let (fingerprint, rest) = bytes.split_first_chunk::<32>()?;
        let (session_key, rest) = rest.split_first_chunk::<65>()?;
        let (not_after, rest) = rest.split_first_chunk::<8>()?;
        let signature: &[u8; 64] = rest.try_into().ok()?;
        Some(SessionCertificate {
            fingerprint: *fingerprint,
            session_key: *session_key,
            not_after: u64::from_le_bytes(*not_after),
            signature: *signature,
        })
    }

    /// Whether `signer`, the identity the fingerprint names, signed this
    /// certificate for this database; then the session's public key.
    pub(crate) fn verified_session(
        &self,
        binding: AccessBinding,
        signer: &PublicIdentity,
    ) -> Option<PublicIdentity> {
        signer
            .verify(&self.message(binding), &self.signature)
            .ok()?;
        PublicIdentity::from_sec1(&self.session_key).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::SessionCertificate;

    fn certificate() -> SessionCertificate {
        SessionCertificate {
            fingerprint: [1; 32],
            session_key: [4; 65],
            not_after: 1_700_000_000_000,
            signature: [9; 64],
        }
    }

    #[test]
    fn reads_what_it_writes() {
        let bytes = certificate().to_bytes();
        assert_eq!(bytes.len(), SessionCertificate::BYTES);
        assert_eq!(SessionCertificate::parse(&bytes), Some(certificate()));
    }

    #[test]
    fn refuses_a_short_certificate() {
        let bytes = certificate().to_bytes();
        assert_eq!(SessionCertificate::parse(&bytes[..100]), None);
    }

    #[test]
    fn refuses_a_long_certificate() {
        let mut bytes = certificate().to_bytes();
        bytes.push(0);
        assert_eq!(SessionCertificate::parse(&bytes), None);
    }
}
