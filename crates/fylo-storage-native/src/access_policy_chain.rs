use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fylo_identity::{Identity, IdentityId, PublicIdentity};
use sha2::{Digest, Sha256};

use crate::access_binding::AccessBinding;
use crate::{
    AccessPolicy, AccessPolicyEnvelope, AccessSignature, ContainerCipher, ContainerSealContext,
    DatabaseAccessError,
};

/// The policy versions of a protected database, checked as a chain:
/// version 1 is signed by an admin it names, its signer is the anchor, and
/// each later version names the one before by hash and is signed by an
/// admin of it.
pub(crate) struct AccessPolicyChain {
    policy: AccessPolicy,
    bytes: Vec<u8>,
    anchor: IdentityId,
    versions: Vec<AccessPolicy>,
}

impl AccessPolicyChain {
    /// Open and check every version in `envelopes` with the catalog key.
    pub(crate) fn verify(
        envelopes: &[AccessPolicyEnvelope],
        catalog: &ContainerCipher,
        binding: AccessBinding,
    ) -> Result<AccessPolicyChain, DatabaseAccessError> {
        let first = envelopes
            .first()
            .ok_or_else(|| DatabaseAccessError::Policy("the database has no policy".into()))?;
        let mut chain = Self::verify_first(first, catalog, binding)?;
        for envelope in &envelopes[1..] {
            chain = chain.verify_next(envelope, catalog, binding)?;
        }
        Ok(chain)
    }

    /// Seal `policy` with the catalog key and sign it as `signer`.
    pub(crate) fn seal(
        policy: &AccessPolicy,
        catalog: &ContainerCipher,
        binding: AccessBinding,
        signer: &Identity,
    ) -> Result<AccessPolicyEnvelope, DatabaseAccessError> {
        let bytes = serde_json::to_vec(policy).map_err(|error| Self::damaged(&error))?;
        let sealed = catalog
            .seal(
                ContainerSealContext::Policy {
                    version: policy.version,
                },
                &bytes,
            )
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        let signature = signer.sign(&binding.policy_message(&bytes));
        Ok(AccessPolicyEnvelope {
            sealed: STANDARD.encode(sealed),
            signatures: vec![AccessSignature {
                key: signer.id().to_string(),
                signature: STANDARD.encode(signature),
            }],
        })
    }

    pub(crate) fn policy(&self) -> &AccessPolicy {
        &self.policy
    }

    /// Every version, oldest first.
    pub(crate) fn versions(&self) -> &[AccessPolicy] {
        &self.versions
    }

    pub(crate) fn anchor(&self) -> &IdentityId {
        &self.anchor
    }

    /// `sha256:<hex>` of the newest version's bytes, which the next names.
    pub(crate) fn digest(&self) -> String {
        Self::digest_of(&self.bytes)
    }

    fn verify_first(
        envelope: &AccessPolicyEnvelope,
        catalog: &ContainerCipher,
        binding: AccessBinding,
    ) -> Result<AccessPolicyChain, DatabaseAccessError> {
        let (policy, bytes) = Self::open_version(envelope, catalog, 1)?;
        Self::require_previous(&policy, None)?;
        let anchor = Self::signer(
            envelope,
            &policy.admin_keys(),
            &binding.policy_message(&bytes),
        )?;
        Ok(AccessPolicyChain {
            versions: vec![policy.clone()],
            policy,
            bytes,
            anchor,
        })
    }

    fn verify_next(
        self,
        envelope: &AccessPolicyEnvelope,
        catalog: &ContainerCipher,
        binding: AccessBinding,
    ) -> Result<AccessPolicyChain, DatabaseAccessError> {
        let (policy, bytes) = Self::open_version(envelope, catalog, self.policy.version + 1)?;
        Self::require_previous(&policy, Some(&self.digest()))?;
        Self::signer(
            envelope,
            &self.policy.admin_keys(),
            &binding.policy_message(&bytes),
        )?;
        let mut versions = self.versions;
        versions.push(policy.clone());
        Ok(AccessPolicyChain {
            policy,
            bytes,
            anchor: self.anchor,
            versions,
        })
    }

    fn open_version(
        envelope: &AccessPolicyEnvelope,
        catalog: &ContainerCipher,
        version: u64,
    ) -> Result<(AccessPolicy, Vec<u8>), DatabaseAccessError> {
        let sealed = STANDARD
            .decode(&envelope.sealed)
            .map_err(|error| Self::damaged(&error))?;
        let bytes = catalog
            .open(ContainerSealContext::Policy { version }, &sealed)
            .map_err(|_| {
                Self::refused(&format!(
                    "policy version {version} does not open with the catalog key"
                ))
            })?;
        let policy: AccessPolicy =
            serde_json::from_slice(&bytes).map_err(|error| Self::refused(&error.to_string()))?;
        Self::require_header(&policy, version)?;
        Ok((policy, bytes))
    }

    fn require_header(policy: &AccessPolicy, version: u64) -> Result<(), DatabaseAccessError> {
        Some(policy)
            .filter(|policy| policy.format == AccessPolicy::FORMAT && policy.version == version)
            .map(|_| ())
            .ok_or_else(|| {
                Self::refused(&format!(
                    "policy version {version} is not {}",
                    AccessPolicy::FORMAT
                ))
            })
    }

    fn require_previous(
        policy: &AccessPolicy,
        previous: Option<&String>,
    ) -> Result<(), DatabaseAccessError> {
        Some(policy)
            .filter(|policy| policy.previous.as_ref() == previous)
            .map(|_| ())
            .ok_or_else(|| {
                Self::refused(&format!(
                    "policy version {} does not follow the version before it",
                    policy.version
                ))
            })
    }

    fn signer(
        envelope: &AccessPolicyEnvelope,
        admins: &[PublicIdentity],
        message: &[u8],
    ) -> Result<IdentityId, DatabaseAccessError> {
        envelope
            .signatures
            .iter()
            .find_map(|signature| Self::verified_signer(signature, admins, message))
            .ok_or_else(|| Self::refused("no admin of the policy signed this version"))
    }

    fn verified_signer(
        signature: &AccessSignature,
        admins: &[PublicIdentity],
        message: &[u8],
    ) -> Option<IdentityId> {
        let bytes = STANDARD.decode(&signature.signature).ok()?;
        admins
            .iter()
            .filter(|admin| admin.id().as_str() == signature.key)
            .find(|admin| admin.verify(message, &bytes).is_ok())
            .map(|admin| admin.id().clone())
    }

    pub(crate) fn digest_of(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let hex = digest.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        });
        format!("sha256:{hex}")
    }

    fn refused(message: &str) -> DatabaseAccessError {
        DatabaseAccessError::Policy(format!("the database policy is refused: {message}"))
    }

    fn damaged(error: &dyn std::fmt::Display) -> DatabaseAccessError {
        DatabaseAccessError::Damaged(format!("the database policy is damaged: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use fylo_identity::{Identity, KeyReference};

    use super::AccessPolicyChain;
    use crate::access_binding::AccessBinding;
    use crate::{AccessAction, AccessGrantRequest, AccessPolicy, AccessRule, ContainerCipher};

    struct Keys(std::path::PathBuf);

    impl Keys {
        fn new(label: &str) -> Self {
            let directory = std::env::temp_dir().join(crate::write::unique_name(label));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn identity(&self, name: &str) -> Identity {
            Identity::create(&KeyReference::File(self.0.join(format!("{name}.pem")))).unwrap()
        }
    }

    impl Drop for Keys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn admin_grant(identity: &Identity) -> AccessGrantRequest {
        AccessGrantRequest {
            keys: vec![identity.public()],
            label: None,
            rule: AccessRule {
                id: "forged".into(),
                identity: "forger".into(),
                actions: vec![AccessAction::Admin],
                resources: vec!["database".into()],
            },
        }
    }

    #[test]
    fn a_version_signed_by_a_non_admin_is_refused() {
        let keys = Keys::new("policy-chain-forged");
        let admin = keys.identity("admin");
        let reader = keys.identity("reader");
        let catalog = ContainerCipher::from_key(&[3; 32]);
        let binding = AccessBinding::new([5; 32]);
        let first = AccessPolicy::first(&admin.public());
        let digest = AccessPolicyChain::digest_of(&serde_json::to_vec(&first).unwrap());
        let forged = first.with_grant(&admin_grant(&reader), digest).unwrap();
        let envelopes = vec![
            AccessPolicyChain::seal(&first, &catalog, binding, &admin).unwrap(),
            AccessPolicyChain::seal(&forged, &catalog, binding, &reader).unwrap(),
        ];
        let error = AccessPolicyChain::verify(&envelopes, &catalog, binding)
            .err()
            .unwrap();
        assert_eq!(error.code(), "EPOLICY");
    }

    #[test]
    fn a_chain_of_admin_signed_versions_verifies() {
        let keys = Keys::new("policy-chain-valid");
        let admin = keys.identity("admin");
        let reader = keys.identity("reader");
        let catalog = ContainerCipher::from_key(&[3; 32]);
        let binding = AccessBinding::new([5; 32]);
        let first = AccessPolicy::first(&admin.public());
        let digest = AccessPolicyChain::digest_of(&serde_json::to_vec(&first).unwrap());
        let second = first.with_grant(&admin_grant(&reader), digest).unwrap();
        let envelopes = vec![
            AccessPolicyChain::seal(&first, &catalog, binding, &admin).unwrap(),
            AccessPolicyChain::seal(&second, &catalog, binding, &admin).unwrap(),
        ];
        let chain = AccessPolicyChain::verify(&envelopes, &catalog, binding).unwrap();
        assert_eq!(chain.policy().version, 2);
        assert_eq!(chain.anchor(), admin.id());
    }

    #[test]
    fn a_version_from_another_database_is_refused() {
        let keys = Keys::new("policy-chain-other-database");
        let admin = keys.identity("admin");
        let catalog = ContainerCipher::from_key(&[3; 32]);
        let first = AccessPolicy::first(&admin.public());
        let envelopes = vec![
            AccessPolicyChain::seal(&first, &catalog, AccessBinding::new([5; 32]), &admin).unwrap(),
        ];
        let error = AccessPolicyChain::verify(&envelopes, &catalog, AccessBinding::new([6; 32]))
            .err()
            .unwrap();
        assert_eq!(error.code(), "EPOLICY");
    }
}
