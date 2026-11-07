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
        Ok(AccessPolicyChain {
            policy,
            bytes,
            anchor: self.anchor,
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

    fn digest_of(bytes: &[u8]) -> String {
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
