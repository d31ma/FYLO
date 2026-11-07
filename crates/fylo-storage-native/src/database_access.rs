use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use aes_gcm::Aes256Gcm;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fylo_format::{ContainerAccessSlot, LogFormatError};
use fylo_identity::{Identity, IdentityId, PublicIdentity};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::access_binding::AccessBinding;
use crate::access_policy_chain::AccessPolicyChain;
use crate::catalog_keys::CatalogKeys;
use crate::collection_keys::CollectionKeys;
use crate::{
    AccessAction, AccessDocument, AccessGrantRequest, AccessGrants, AccessKeyWrap, AccessPolicy,
    AccessPolicyChange, AccessPolicyEnvelope, AccessTrust, ContainerCipher, ContainerCipherError,
    DatabaseAccessError, DatabaseIdentity,
};

/// The access state of a protected `.vdbx` database as one identity opened
/// it (ADR 0021): the checked policy, what it grants this identity, and the
/// collection keys it unwrapped. Every handle of the file shares it through
/// the file's [`ContainerCipher`].
pub struct DatabaseAccess {
    identity: Arc<Identity>,
    binding: AccessBinding,
    anchor: IdentityId,
    trusted_on_first_use: bool,
    fingerprint: [u8; 32],
    state: RwLock<AccessState>,
    verified: Mutex<HashSet<[u8; 32]>>,
    /// The keys a rekey's new file gives its collections as it creates
    /// them, by name ([`Self::inherit`]); empty otherwise.
    inherited: Mutex<BTreeMap<String, CollectionKeys>>,
}

struct AccessState {
    generation: u64,
    catalog: CatalogKeys,
    document: AccessDocument,
    policy: AccessPolicy,
    policy_digest: String,
    versions: Vec<AccessPolicy>,
    /// The admin whose signature each version carries, oldest first.
    version_signers: Vec<IdentityId>,
    signers: BTreeMap<[u8; 32], PublicIdentity>,
    names: BTreeMap<u32, String>,
    grants: AccessGrants,
    keys: BTreeMap<u32, CollectionKeys>,
}

/// A collection's id, an epoch, and that epoch's key.
type EpochKey = (u32, u32, Zeroizing<[u8; 32]>);

/// Which keys a copy of a protected database renews (ADR 0021 section 7).
#[derive(Clone, Copy, Debug)]
pub(crate) enum RekeyScope<'a> {
    /// None: `fylo pack --sealed` copies it with every key and the policy
    /// chain as they are.
    Nothing,
    /// One collection's key: `fylo rekey --collection`.
    Collection(&'a str),
    /// The catalog key and every collection key: `fylo rekey`.
    Everything,
}

/// What a protected `fylo rekey` writes its new file with (ADR 0021
/// section 7): the same database id, a catalog key, the access slot of the
/// new file (every policy version sealed under that key, then a `rekey`
/// version signed by the admin, and the catalog key wrapped for everyone the
/// policy names), and the keys each collection takes in it, by name.
pub(crate) struct RekeyPlan {
    pub(crate) database: [u8; 32],
    pub(crate) catalog_key: Zeroizing<[u8; 32]>,
    pub(crate) slot: ContainerAccessSlot,
    pub(crate) keys: BTreeMap<String, CollectionKeys>,
    /// The collections whose key is new, whose records' `$encrypted`
    /// values must be written again under it.
    pub(crate) rotated: Vec<String>,
}

/// A change of the access document made in memory, to write to the access
/// slots before it takes effect ([`DatabaseAccess::commit`]): new keys,
/// a newly named collection, and a new policy version.
pub(crate) struct AccessChange {
    pub(crate) slot: ContainerAccessSlot,
    catalog: Option<CatalogKeys>,
    keys: Vec<EpochKey>,
    collection: Option<(u32, String)>,
    policy: Option<(AccessPolicy, String)>,
}

impl DatabaseAccess {
    const FIRST_EPOCH: u32 = 1;
    /// The signer kind of a frame signed with an identity key. A later
    /// kind (a session certificate for hardware keys, phase 8) is refused
    /// by this build.
    const IDENTITY_SIGNER: u8 = 1;
    /// Kind, fingerprint and signature.
    pub(crate) const TRAILER_BYTES: usize = 1 + 32 + 64;
    /// Frames whose signature this process remembers having checked.
    const VERIFIED_CAPACITY: usize = 1 << 20;

    /// The access slot of a new protected database: policy version 1 makes
    /// `admin` its admin and anchor, and the catalog key is wrapped for it.
    pub(crate) fn create(
        admin: &Identity,
        database: [u8; 32],
        catalog_key: &[u8; 32],
    ) -> Result<ContainerAccessSlot, DatabaseAccessError> {
        let binding = AccessBinding::new(database);
        let catalog = ContainerCipher::from_key(catalog_key);
        let policy = AccessPolicy::first(&admin.public());
        let envelope = AccessPolicyChain::seal(&policy, &catalog, binding, admin)?;
        let wrap = Self::wrap_for(&admin.public(), &binding.catalog_info(), catalog_key, None)?;
        let document = AccessDocument {
            format: AccessDocument::FORMAT.to_owned(),
            policies: vec![envelope],
            keyring: vec![wrap],
            previous: None,
            original: None,
        };
        Self::slot(1, &document)
    }

    /// Open a protected database's access slot as `identity`: unwrap the
    /// catalog key, check it against the superblock's key-check value,
    /// check the policy chain and its anchor, and unwrap the collection
    /// keys granted. Returns the file's cipher.
    #[inline(never)]
    pub(crate) fn open(
        slot: &ContainerAccessSlot,
        database: [u8; 32],
        key_check: &[u8; 16],
        identity: &DatabaseIdentity,
    ) -> Result<Arc<ContainerCipher>, DatabaseAccessError> {
        let binding = AccessBinding::new(database);
        let document = Self::parse(&slot.body)?;
        let current = Self::unwrap_catalog(&document, identity.identity(), binding)?;
        let catalog_keys = CatalogKeys::from_document(current, &document)?;
        let catalog = ContainerCipher::from_key(catalog_keys.original());
        Self::require_catalog(&catalog, key_check)?;
        let chain = AccessPolicyChain::verify(
            &document.policies,
            &ContainerCipher::from_key(catalog_keys.current()),
            binding,
        )?;
        let trusted_on_first_use = Self::require_trusted(chain.anchor(), identity.trust())?;
        let keys = Self::unwrap_collections(&document, identity.identity(), binding);
        let access = DatabaseAccess {
            identity: Arc::clone(identity.identity()),
            binding,
            anchor: chain.anchor().clone(),
            trusted_on_first_use,
            fingerprint: identity.identity().public().fingerprint(),
            verified: Mutex::new(HashSet::new()),
            inherited: Mutex::new(BTreeMap::new()),
            state: RwLock::new(AccessState {
                generation: slot.generation,
                catalog: catalog_keys,
                policy_digest: chain.digest(),
                policy: chain.policy().clone(),
                versions: chain.versions().to_vec(),
                version_signers: chain.signers().to_vec(),
                signers: Self::signers(chain.versions()),
                document,
                names: BTreeMap::new(),
                grants: AccessGrants::default(),
                keys,
            }),
        };
        Ok(Arc::new(catalog.with_access(Arc::new(access))))
    }

    /// The identity this access was opened with.
    #[must_use]
    pub fn identity(&self) -> &IdentityId {
        self.identity.id()
    }

    /// The admin identity that created the database and signed its first
    /// policy.
    #[must_use]
    pub fn anchor(&self) -> &IdentityId {
        &self.anchor
    }

    /// This identity, with this database's anchor pinned: how a file that
    /// carries this database's policy chain is opened as it.
    pub(crate) fn pinned_identity(&self) -> DatabaseIdentity {
        DatabaseIdentity::shared(
            Arc::clone(&self.identity),
            AccessTrust::Pinned(self.anchor.clone()),
        )
    }

    /// Whether the anchor was trusted without being pinned
    /// ([`AccessTrust::FirstUse`]); the caller should warn and pin it.
    #[must_use]
    pub fn trusted_on_first_use(&self) -> bool {
        self.trusted_on_first_use
    }

    /// The newest policy version.
    #[must_use]
    pub fn policy(&self) -> AccessPolicy {
        self.read().policy.clone()
    }

    /// What the policy grants this identity on the collections the
    /// directory holds.
    #[must_use]
    pub fn grants(&self) -> AccessGrants {
        self.read().grants.clone()
    }

    /// Every policy version, oldest first, as `fylo access audit` lists it:
    /// its signer, its change and the rules it added and removed.
    #[must_use]
    pub fn audit(&self) -> Vec<crate::AccessAuditEntry> {
        let state = self.read();
        let mut entries = Vec::with_capacity(state.versions.len());
        let mut before: Option<&AccessPolicy> = None;
        for (policy, signer) in state.versions.iter().zip(&state.version_signers) {
            entries.push(crate::AccessAuditEntry::of(
                policy,
                signer.to_string(),
                before,
            ));
            before = Some(policy);
        }
        entries
    }

    /// The access slot generation this state was read from.
    pub(crate) fn generation(&self) -> u64 {
        self.read().generation
    }

    /// Learn the directory's collections, and what this identity may do
    /// with each. Until a collection is bound, nothing may be written to it.
    #[inline(never)]
    pub(crate) fn bind_directory(&self, collections: &[(u32, String)]) {
        let mut state = self.write();
        let names: Vec<&str> = collections.iter().map(|(_, name)| name.as_str()).collect();
        state.grants = AccessGrants::resolve(&state.policy, self.identity.id(), &names);
        state.names = Self::names_by_id(collections);
    }

    /// Take in a newer access slot another process wrote: the policy that
    /// follows from it, and any collection key wrapped for this identity
    /// since.
    #[inline(never)]
    pub(crate) fn reload(&self, slot: &ContainerAccessSlot) -> Result<(), DatabaseAccessError> {
        let document = Self::parse(&slot.body)?;
        let current = Self::unwrap_catalog(&document, &self.identity, self.binding)?;
        let catalog = CatalogKeys::from_document(current, &document)?;
        Self::require_same_original(&self.read().catalog, &catalog)?;
        let chain = AccessPolicyChain::verify(
            &document.policies,
            &ContainerCipher::from_key(catalog.current()),
            self.binding,
        )?;
        Self::require_same_anchor(&self.anchor, chain.anchor())?;
        let keys = Self::unwrap_collections(&document, &self.identity, self.binding);
        let mut state = self.write();
        let names: Vec<String> = state.names.values().cloned().collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        state.grants = AccessGrants::resolve(chain.policy(), self.identity.id(), &names);
        state.generation = slot.generation;
        state.catalog = catalog;
        state.policy_digest = chain.digest();
        state.policy = chain.policy().clone();
        state.versions = chain.versions().to_vec();
        state.version_signers = chain.signers().to_vec();
        state.signers = Self::signers(chain.versions());
        state.document = document;
        state.keys = keys;
        Ok(())
    }

    /// Sign a protected file's record frame, `stored` holding its stored
    /// bytes: set the signed flag in its header byte, and append the
    /// trailer (signer kind, the signer's fingerprint, an ECDSA P-256
    /// signature over the database, the collection, the LSN and the SHA-256
    /// of the stored bytes).
    pub(crate) fn sign_frame(&self, collection: u32, lsn: u64, mut stored: Vec<u8>) -> Vec<u8> {
        if let Some(header) = stored.first_mut() {
            *header |= crate::container_record::SIGNED;
        }
        let digest: [u8; 32] = Sha256::digest(&stored).into();
        let message = self.binding.frame_message(collection, lsn, &digest);
        let signature = self.identity.sign(&message);
        self.remember_verified(Sha256::digest(&message).into());
        stored.push(Self::IDENTITY_SIGNER);
        stored.extend_from_slice(&self.fingerprint);
        stored.extend_from_slice(&signature);
        stored
    }

    /// Check a signed record frame of `collection`: its trailer names a key
    /// of the policy, the signature verifies, and that key could write the
    /// collection at the record's LSN. Returns the stored bytes before the
    /// trailer. A frame checked once in this process is not checked again.
    #[inline(never)]
    pub(crate) fn verify_frame<'a>(
        &self,
        collection: u32,
        stored: &'a [u8],
    ) -> Result<&'a [u8], LogFormatError> {
        let (prefix, trailer) = Self::split_signed(stored)?;
        let lsn = crate::ContainerStoredBody::parse(prefix)
            .map_err(|_| LogFormatError::BadSignature)?
            .lsn;
        let digest: [u8; 32] = Sha256::digest(prefix).into();
        let message = self.binding.frame_message(collection, lsn, &digest);
        let seen: [u8; 32] = Sha256::digest(&message).into();
        if self.was_verified(&seen) {
            return Ok(prefix);
        }
        self.check_trailer(collection, lsn, &message, trailer)?;
        self.remember_verified(seen);
        Ok(prefix)
    }

    fn split_signed(stored: &[u8]) -> Result<(&[u8], &[u8]), LogFormatError> {
        stored
            .len()
            .checked_sub(Self::TRAILER_BYTES)
            .map(|at| stored.split_at(at))
            .filter(|(prefix, _)| {
                prefix
                    .first()
                    .is_some_and(|header| header & crate::container_record::SIGNED != 0)
            })
            .ok_or(LogFormatError::BadSignature)
    }

    fn check_trailer(
        &self,
        collection: u32,
        lsn: u64,
        message: &[u8],
        trailer: &[u8],
    ) -> Result<(), LogFormatError> {
        let (kind, rest) = trailer.split_first().ok_or(LogFormatError::BadSignature)?;
        let (fingerprint, signature) = rest
            .split_first_chunk::<32>()
            .filter(|_| *kind == Self::IDENTITY_SIGNER)
            .ok_or(LogFormatError::BadSignature)?;
        let state = self.read();
        let signer = state
            .signers
            .get(fingerprint)
            .ok_or(LogFormatError::BadSignature)?;
        signer
            .verify(message, signature)
            .map_err(|_| LogFormatError::BadSignature)?;
        Some(Self::could_write(&state, collection, lsn, signer.id()))
            .filter(|allowed| *allowed)
            .map(|_| ())
            .ok_or(LogFormatError::BadSignature)
    }

    /// Whether the key `id` could write the collection at `lsn`: under the
    /// newest version whose revocation boundary for it is at or before
    /// `lsn`, or a later version.
    fn could_write(state: &AccessState, collection: u32, lsn: u64, id: &IdentityId) -> bool {
        let Some(name) = state.names.get(&collection) else {
            return false;
        };
        let boundary = state
            .versions
            .iter()
            .rposition(|policy| {
                policy
                    .boundaries
                    .get(&collection)
                    .is_some_and(|boundary| *boundary <= lsn)
            })
            .unwrap_or(0);
        let start = boundary.max(Self::newest_rewrite(&state.versions));
        state.versions[start..]
            .iter()
            .any(|policy| policy.may_write(id, name))
    }

    /// The newest version a rekey wrote. A rekey signs every record of the
    /// file it writes again, so what versions before it allowed no longer
    /// counts, and their boundaries name the old file's collection ids.
    fn newest_rewrite(versions: &[AccessPolicy]) -> usize {
        versions
            .iter()
            .rposition(|policy| {
                policy
                    .change
                    .as_ref()
                    .is_some_and(crate::AccessPolicyChange::rewrote_records)
            })
            .unwrap_or(0)
    }

    fn was_verified(&self, seen: &[u8; 32]) -> bool {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(seen)
    }

    fn remember_verified(&self, seen: [u8; 32]) {
        let mut verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        if verified.len() >= Self::VERIFIED_CAPACITY {
            verified.clear();
        }
        verified.insert(seen);
    }

    fn signers(versions: &[AccessPolicy]) -> BTreeMap<[u8; 32], PublicIdentity> {
        let mut signers = BTreeMap::new();
        let keys = versions
            .iter()
            .flat_map(|policy| policy.identities.values())
            .flat_map(|identity| identity.keys.iter())
            .filter_map(|key| PublicIdentity::parse(key).ok());
        for key in keys {
            signers.insert(key.fingerprint(), key);
        }
        signers
    }

    fn boundaries(covered: &[u32], next_lsns: &BTreeMap<u32, u64>) -> BTreeMap<u32, u64> {
        let mut boundaries = BTreeMap::new();
        for collection in covered {
            let next = next_lsns.get(collection).copied().unwrap_or(0);
            boundaries.insert(*collection, next);
        }
        boundaries
    }

    fn names_by_id(collections: &[(u32, String)]) -> BTreeMap<u32, String> {
        let mut names = BTreeMap::new();
        for (id, name) in collections {
            names.insert(*id, name.clone());
        }
        names
    }

    /// The `$encrypted` field keys of the collection named `name`, newest
    /// epoch first: one per collection key epoch this identity holds
    /// (ADR 0021 section 7).
    ///
    /// # Errors
    ///
    /// `EACCES` when this identity holds no key of that collection, or the
    /// directory has no collection by that name.
    pub fn field_keys(&self, name: &str) -> Result<crate::FieldKeys, DatabaseAccessError> {
        let state = self.read();
        let (collection, keys) = state
            .names
            .iter()
            .find(|(_, held)| held.as_str() == name)
            .and_then(|(collection, _)| state.keys.get(collection).map(|keys| (*collection, keys)))
            .ok_or_else(|| self.denied("holds no key of that collection"))?;
        let derived = keys
            .keys()
            .iter()
            .map(|(epoch, key)| self.binding.field_key(collection, *epoch, key))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::FieldKeys::new(derived))
    }

    /// Whether a catalog-key rotation is still resealing the directory
    /// (ADR 0021 section 7).
    pub(crate) fn rotation_pending(&self) -> bool {
        self.read().catalog.rotation_pending()
    }

    /// The key that seals a directory block: the current catalog key.
    pub(crate) fn catalog_sealing_key(&self) -> Arc<Aes256Gcm> {
        self.read().catalog.sealing()
    }

    /// The keys that may open a directory block: the current catalog key,
    /// then the previous one while a rotation is pending.
    pub(crate) fn catalog_opening_keys(&self) -> Vec<Arc<Aes256Gcm>> {
        self.read().catalog.opening()
    }

    /// The policy version a pending catalog-key rotation started at: the
    /// newest revocation, or the newest version when none is recorded.
    /// `None` when no rotation is pending (ADR 0021 section 7).
    #[must_use]
    pub fn catalog_rotation_pending(&self) -> Option<u64> {
        let state = self.read();
        state
            .catalog
            .rotation_pending()
            .then(|| Self::rotation_version(&state.versions))
    }

    fn rotation_version(versions: &[AccessPolicy]) -> u64 {
        versions
            .iter()
            .rev()
            .find(|policy| Self::is_revocation(policy))
            .or(versions.last())
            .map_or(0, |policy| policy.version)
    }

    fn is_revocation(policy: &AccessPolicy) -> bool {
        policy
            .change
            .as_ref()
            .is_some_and(|change| change.kind == AccessPolicyChange::REVOKE)
    }

    /// Prepare the access document without the previous catalog key, once
    /// every directory block is sealed under the current one. With
    /// `record`, which only an admin passes, the document also gets a
    /// `settle` policy version signed by this identity, so `fylo access
    /// audit` lists who finished the rotation; when that version would not
    /// fit in the access slot, the rotation is finished without it, since
    /// dropping the previous key matters more than its record.
    pub(crate) fn prepare_settled(
        &self,
        record: bool,
    ) -> Result<AccessChange, DatabaseAccessError> {
        let state = self.read();
        let catalog = state.catalog.settled();
        let recorded = self.recorded_settle(&state, &catalog, record)?;
        recorded.map_or_else(|| Self::unrecorded_settle(&state, catalog), Ok)
    }

    fn unrecorded_settle(
        state: &AccessState,
        catalog: CatalogKeys,
    ) -> Result<AccessChange, DatabaseAccessError> {
        let (previous, original) = catalog.document_fields()?;
        let document = AccessDocument {
            previous,
            original,
            ..state.document.clone()
        };
        Ok(AccessChange {
            slot: Self::slot(state.generation + 1, &document)?,
            catalog: Some(catalog),
            keys: Vec::new(),
            collection: None,
            policy: None,
        })
    }

    /// The settled document with a signed `settle` version, or `None`
    /// without `record`, for an identity the newest policy no longer makes
    /// an admin (its signature would not verify), or when it does not fit
    /// in the access slot.
    fn recorded_settle(
        &self,
        state: &AccessState,
        catalog: &CatalogKeys,
        record: bool,
    ) -> Result<Option<AccessChange>, DatabaseAccessError> {
        if !(record && state.grants.is_admin()) {
            return Ok(None);
        }
        let policy = state.policy.recording(
            state.policy_digest.clone(),
            AccessPolicyChange::now(AccessPolicyChange::SETTLE),
        );
        let sealing = ContainerCipher::from_key(catalog.current());
        let envelope = AccessPolicyChain::seal(&policy, &sealing, self.binding, &self.identity)?;
        let bytes = serde_json::to_vec(&policy)
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        let mut policies = state.document.policies.clone();
        policies.push(envelope);
        let (previous, original) = catalog.document_fields()?;
        let document = AccessDocument {
            policies,
            previous,
            original,
            ..state.document.clone()
        };
        let slot = Self::fitting_slot(state.generation + 1, &document)?;
        Ok(slot.map(|slot| AccessChange {
            slot,
            catalog: Some(catalog.clone()),
            keys: Vec::new(),
            collection: None,
            policy: Some((policy, AccessPolicyChain::digest_of(&bytes))),
        }))
    }

    /// The slot holding `document`, or `None` when it is too large for one.
    fn fitting_slot(
        generation: u64,
        document: &AccessDocument,
    ) -> Result<Option<ContainerAccessSlot>, DatabaseAccessError> {
        let body = serde_json::to_vec(document)
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        Ok((body.len() <= ContainerAccessSlot::MAX_BODY_BYTES)
            .then_some(ContainerAccessSlot { generation, body }))
    }

    /// Whether this identity is an admin of the database.
    pub(crate) fn is_admin(&self) -> bool {
        self.read().grants.is_admin()
    }

    fn require_same_original(
        held: &CatalogKeys,
        found: &CatalogKeys,
    ) -> Result<(), DatabaseAccessError> {
        Some(found)
            .filter(|found| found.original() == held.original())
            .map(|_| ())
            .ok_or_else(|| {
                DatabaseAccessError::Policy(
                    "the catalog key in the access document does not match this database".into(),
                )
            })
    }

    /// Refuse unless this identity is an admin of the database.
    pub(crate) fn require_admin(&self) -> Result<(), DatabaseAccessError> {
        Some(self.read().grants.is_admin())
            .filter(|admin| *admin)
            .map(|_| ())
            .ok_or_else(|| self.denied("is not an admin of this database"))
    }

    /// Refuse unless this identity may write the collection `collection`.
    pub(crate) fn require_write(&self, collection: u32) -> Result<(), DatabaseAccessError> {
        Some(self.may_write(collection))
            .filter(|allowed| *allowed)
            .map(|_| ())
            .ok_or_else(|| self.denied("may not write that collection"))
    }

    /// The key that seals a new unit of the collection: its newest epoch.
    pub(crate) fn sealing_key(
        &self,
        collection: u32,
    ) -> Result<Arc<Aes256Gcm>, ContainerCipherError> {
        let state = self.read();
        Some(collection)
            .filter(|collection| Self::grants_write(&state, *collection))
            .and_then(|collection| state.keys.get(&collection))
            .and_then(CollectionKeys::newest)
            .ok_or(ContainerCipherError::NoGrant)
    }

    /// Every epoch's key of the collection, newest first, to open a unit.
    pub(crate) fn opening_keys(
        &self,
        collection: u32,
    ) -> Result<Vec<Arc<Aes256Gcm>>, ContainerCipherError> {
        self.read()
            .keys
            .get(&collection)
            .map(CollectionKeys::all)
            .ok_or(ContainerCipherError::NoGrant)
    }

    /// Prepare the keys of a new collection `id` named `name`: a fresh key,
    /// wrapped for every identity a rule covering it names, in place of any
    /// wraps a stopped creation left for that id.
    pub(crate) fn prepare_collection(
        &self,
        id: u32,
        name: &str,
    ) -> Result<AccessChange, DatabaseAccessError> {
        self.require_admin()?;
        let keys = self.keys_of_new_collection(name)?;
        let state = self.read();
        let recipients = state.policy.collection_keys(name);
        let mut wraps = Vec::new();
        for (epoch, key) in keys.keys() {
            let info = self.binding.collection_info(id, epoch);
            wraps.extend(Self::wraps_for(
                &recipients,
                &info,
                &key,
                Some((id, epoch)),
            )?);
        }
        let mut document = state.document.clone();
        document.keyring.retain(|wrap| wrap.collection != Some(id));
        document.keyring.extend(wraps);
        Ok(AccessChange {
            slot: Self::slot(state.generation + 1, &document)?,
            keys: keys
                .keys()
                .into_iter()
                .map(|(epoch, key)| (id, epoch, key))
                .collect(),
            catalog: None,
            collection: Some((id, name.to_owned())),
            policy: None,
        })
    }

    /// The keys a new collection named `name` takes: those a rekey handed
    /// down for it ([`Self::inherit`]), or a fresh key at the first epoch.
    fn keys_of_new_collection(&self, name: &str) -> Result<CollectionKeys, DatabaseAccessError> {
        let inherited = self
            .inherited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        if let Some(keys) = inherited {
            return Ok(keys);
        }
        let mut keys = CollectionKeys::new();
        keys.add(Self::FIRST_EPOCH, &*AccessBinding::random_key()?);
        Ok(keys)
    }

    /// Give each collection named in `keys` those keys when this file
    /// creates it: the new file of a protected `fylo rekey`.
    pub(crate) fn inherit(&self, keys: BTreeMap<String, CollectionKeys>) {
        *self
            .inherited
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = keys;
    }

    /// Plan a protected `fylo rekey` (ADR 0021 section 7). With
    /// `collection`, only that collection's key is new, at the epoch after
    /// its newest, and the catalog key and every other collection's epochs
    /// carry over; without, the catalog key and every collection key are
    /// new. Either way the policy chain carries over, sealed under the
    /// catalog key, and a `rekey` version signed by this admin follows it.
    pub(crate) fn plan_rekey(
        &self,
        scope: RekeyScope<'_>,
    ) -> Result<RekeyPlan, DatabaseAccessError> {
        self.require_admin()?;
        let state = self.read();
        if let RekeyScope::Collection(name) = scope {
            Self::require_collection(&state, name)?;
        }
        let catalog_key = match scope {
            RekeyScope::Everything => AccessBinding::random_key()?,
            RekeyScope::Nothing | RekeyScope::Collection(_) => state.catalog.current().clone(),
        };
        let catalog = ContainerCipher::from_key(&catalog_key);
        let mut policies = Self::resealed_policies(&state, &catalog)?;
        let policy = match Self::rekey_change(scope) {
            Some(change) => {
                let policy = state.policy.recording(state.policy_digest.clone(), change);
                policies.push(AccessPolicyChain::seal(
                    &policy,
                    &catalog,
                    self.binding,
                    &self.identity,
                )?);
                policy
            }
            None => state.policy.clone(),
        };
        let keyring = Self::wraps_for(
            &policy.granted_keys(),
            &self.binding.catalog_info(),
            &catalog_key,
            None,
        )?;
        let (keys, rotated) = self.rekeyed_collections(&state, scope)?;
        let document = AccessDocument {
            format: AccessDocument::FORMAT.to_owned(),
            policies,
            keyring,
            previous: None,
            original: None,
        };
        Ok(RekeyPlan {
            database: self.binding.database(),
            catalog_key,
            slot: Self::slot(1, &document)?,
            keys,
            rotated,
        })
    }

    fn rekey_change(scope: RekeyScope<'_>) -> Option<AccessPolicyChange> {
        match scope {
            RekeyScope::Nothing => None,
            RekeyScope::Collection(name) => Some(
                AccessPolicyChange::now(AccessPolicyChange::REKEY_COLLECTION).with_collection(name),
            ),
            RekeyScope::Everything => Some(AccessPolicyChange::now(AccessPolicyChange::REKEY)),
        }
    }

    fn require_collection(state: &AccessState, name: &str) -> Result<(), DatabaseAccessError> {
        Some(name)
            .filter(|name| state.names.values().any(|held| held == name))
            .map(|_| ())
            .ok_or_else(|| {
                DatabaseAccessError::Policy(format!("the database has no collection {name:?}"))
            })
    }

    /// Every policy version, sealed under `catalog`.
    fn resealed_policies(
        state: &AccessState,
        catalog: &ContainerCipher,
    ) -> Result<Vec<AccessPolicyEnvelope>, DatabaseAccessError> {
        let current = ContainerCipher::from_key(state.catalog.current());
        let mut policies = Vec::with_capacity(state.document.policies.len() + 1);
        for (version, envelope) in (1_u64..).zip(&state.document.policies) {
            policies.push(AccessPolicyChain::reseal(
                envelope,
                version,
                (&current, catalog),
            )?);
        }
        Ok(policies)
    }

    /// The keys each live collection takes in a rekey's new file: a fresh
    /// key, at the epoch after its newest, for each rotated one, which
    /// `collection` names or which are all when it is `None`; the epochs it
    /// has for the others. Also the names rotated.
    fn rekeyed_collections(
        &self,
        state: &AccessState,
        scope: RekeyScope<'_>,
    ) -> Result<(BTreeMap<String, CollectionKeys>, Vec<String>), DatabaseAccessError> {
        let mut keys = BTreeMap::new();
        let mut rotated = Vec::new();
        for (id, name) in &state.names {
            let held = state
                .keys
                .get(id)
                .ok_or_else(|| self.denied(&format!("holds no key of collection {name:?}")))?;
            if !Self::renews(scope, name) {
                keys.insert(name.clone(), held.clone());
                continue;
            }
            let mut fresh = CollectionKeys::new();
            fresh.add(held.next_epoch(), &*AccessBinding::random_key()?);
            keys.insert(name.clone(), fresh);
            rotated.push(name.clone());
        }
        Ok((keys, rotated))
    }

    fn renews(scope: RekeyScope<'_>, name: &str) -> bool {
        match scope {
            RekeyScope::Nothing => false,
            RekeyScope::Collection(only) => only == name,
            RekeyScope::Everything => true,
        }
    }

    /// Prepare policy version n+1 with `request`'s rule added, signed by
    /// this admin, and the keyring rebuilt for it: the new identity's keys
    /// get the catalog key and every key of each collection the rule covers.
    pub(crate) fn prepare_grant(
        &self,
        request: &AccessGrantRequest,
    ) -> Result<AccessChange, DatabaseAccessError> {
        self.require_admin()?;
        let state = self.read();
        let policy = state
            .policy
            .with_grant(request, state.policy_digest.clone())?;
        self.prepare_policy(&state, policy, (&[], false))
    }

    /// Prepare policy version n+1 without the rule `id`, signed by this
    /// admin. Every collection the rule covered starts a new key epoch, and
    /// the keyring is rebuilt, so an identity the rule granted gets no key
    /// for what is written from now on. What was written before stays
    /// sealed with the old epoch until `fylo rekey` rewrites it. The version
    /// records each covered collection's next LSN, from `next_lsns`, as the
    /// boundary past which a record must be signed by an identity that can
    /// still write it.
    pub(crate) fn prepare_revoke(
        &self,
        id: &str,
        next_lsns: &BTreeMap<u32, u64>,
    ) -> Result<AccessChange, DatabaseAccessError> {
        self.require_admin()?;
        let state = self.read();
        let (mut policy, removed) = state.policy.without_rule(id, state.policy_digest.clone())?;
        let covered: Vec<u32> = state
            .names
            .iter()
            .filter(|(_, name)| removed.strongest_on_collection(name).is_some())
            .map(|(collection, _)| *collection)
            .collect();
        policy.boundaries = Self::boundaries(&covered, next_lsns);
        self.prepare_policy(&state, policy, (&covered, true))
    }

    fn prepare_policy(
        &self,
        state: &AccessState,
        policy: AccessPolicy,
        (new_epochs, rotate): (&[u32], bool),
    ) -> Result<AccessChange, DatabaseAccessError> {
        let catalog_keys = if rotate {
            state.catalog.rotated(CatalogKeys::fresh()?)
        } else {
            state.catalog.clone()
        };
        let catalog = ContainerCipher::from_key(catalog_keys.current());
        let envelope = AccessPolicyChain::seal(&policy, &catalog, self.binding, &self.identity)?;
        let bytes = serde_json::to_vec(&policy)
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        let fresh = Self::fresh_epochs(&state.keys, new_epochs)?;
        let mut keys = state.keys.clone();
        for (collection, epoch, key) in &fresh {
            keys.entry(*collection)
                .or_insert_with(CollectionKeys::new)
                .add(*epoch, key);
        }
        let keyring =
            self.rebuild_keyring(&policy, (&state.names, &keys), catalog_keys.current())?;
        let mut policies = Self::resealed_policies(state, &catalog)?;
        policies.push(envelope);
        let (previous, original) = catalog_keys.document_fields()?;
        let document = AccessDocument {
            format: AccessDocument::FORMAT.to_owned(),
            policies,
            keyring,
            previous,
            original,
        };
        Ok(AccessChange {
            slot: Self::slot(state.generation + 1, &document)?,
            catalog: rotate.then_some(catalog_keys),
            keys: fresh,
            collection: None,
            policy: Some((policy, AccessPolicyChain::digest_of(&bytes))),
        })
    }

    fn fresh_epochs(
        keys: &BTreeMap<u32, CollectionKeys>,
        collections: &[u32],
    ) -> Result<Vec<EpochKey>, DatabaseAccessError> {
        collections
            .iter()
            .map(|collection| {
                let epoch = keys
                    .get(collection)
                    .map_or(Self::FIRST_EPOCH, CollectionKeys::next_epoch);
                AccessBinding::random_key().map(|key| (*collection, epoch, key))
            })
            .collect()
    }

    /// Every wrap `policy` calls for: the catalog key for each identity a
    /// rule names, and each epoch of each live collection's key for each
    /// identity a rule covering it names.
    fn rebuild_keyring(
        &self,
        policy: &AccessPolicy,
        (names, keys): (&BTreeMap<u32, String>, &BTreeMap<u32, CollectionKeys>),
        catalog_key: &[u8; 32],
    ) -> Result<Vec<AccessKeyWrap>, DatabaseAccessError> {
        let mut keyring = Self::wraps_for(
            &policy.granted_keys(),
            &self.binding.catalog_info(),
            catalog_key,
            None,
        )?;
        for (collection, name) in names {
            let recipients = policy.collection_keys(name);
            let held = keys
                .get(collection)
                .map(CollectionKeys::keys)
                .unwrap_or_default();
            for (epoch, key) in held {
                let info = self.binding.collection_info(*collection, epoch);
                keyring.extend(Self::wraps_for(
                    &recipients,
                    &info,
                    &key,
                    Some((*collection, epoch)),
                )?);
            }
        }
        Ok(keyring)
    }

    /// Take a change in once its slot is written and flushed.
    pub(crate) fn commit(&self, change: AccessChange) -> Result<(), DatabaseAccessError> {
        let document = Self::parse(&change.slot.body)?;
        let mut state = self.write();
        state.generation = change.slot.generation;
        state.document = document;
        if let Some(catalog) = change.catalog {
            state.catalog = catalog;
        }
        for (collection, epoch, key) in &change.keys {
            state
                .keys
                .entry(*collection)
                .or_insert_with(CollectionKeys::new)
                .add(*epoch, key);
        }
        if let Some((id, name)) = change.collection {
            state.names.insert(id, name);
        }
        if let Some((policy, digest)) = change.policy {
            state.versions.push(policy.clone());
            state.version_signers.push(self.identity.id().clone());
            state.signers = Self::signers(&state.versions);
            state.policy = policy;
            state.policy_digest = digest;
        }
        let names: Vec<String> = state.names.values().cloned().collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        state.grants = AccessGrants::resolve(&state.policy, self.identity.id(), &names);
        Ok(())
    }

    fn may_write(&self, collection: u32) -> bool {
        Self::grants_write(&self.read(), collection)
    }

    fn grants_write(state: &AccessState, collection: u32) -> bool {
        state
            .names
            .get(&collection)
            .and_then(|name| state.grants.on_collection(name))
            .is_some_and(|action| action >= AccessAction::Write)
    }

    fn parse(body: &[u8]) -> Result<AccessDocument, DatabaseAccessError> {
        let document = AccessDocument::from_json(body).map_err(|error| {
            DatabaseAccessError::Damaged(format!("the access document is damaged: {error}"))
        })?;
        Some(document)
            .filter(|document| document.format == AccessDocument::FORMAT)
            .ok_or_else(|| {
                DatabaseAccessError::Damaged(format!(
                    "the access document is not {}",
                    AccessDocument::FORMAT
                ))
            })
    }

    fn slot(
        generation: u64,
        document: &AccessDocument,
    ) -> Result<ContainerAccessSlot, DatabaseAccessError> {
        let body = serde_json::to_vec(document)
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        Some(body.len())
            .filter(|length| *length <= ContainerAccessSlot::MAX_BODY_BYTES)
            .ok_or_else(|| {
                DatabaseAccessError::Policy(format!(
                    "the access document would take {} bytes, past the {} a protected database holds",
                    body.len(),
                    ContainerAccessSlot::MAX_BODY_BYTES
                ))
            })?;
        Ok(ContainerAccessSlot { generation, body })
    }

    fn unwrap_catalog(
        document: &AccessDocument,
        identity: &Identity,
        binding: AccessBinding,
    ) -> Result<Zeroizing<[u8; 32]>, DatabaseAccessError> {
        let me = identity.id().to_string();
        let wrap = document
            .wraps_for(&me)
            .find(|wrap| wrap.collection.is_none())
            .ok_or_else(|| {
                DatabaseAccessError::Denied(format!("{me} has no grant on this database"))
            })?;
        let bytes = Self::unwrap(identity, &binding.catalog_info(), wrap)?;
        AccessBinding::key_bytes(&bytes)
    }

    fn unwrap_collections(
        document: &AccessDocument,
        identity: &Identity,
        binding: AccessBinding,
    ) -> BTreeMap<u32, CollectionKeys> {
        let me = identity.id().to_string();
        let mut keys: BTreeMap<u32, CollectionKeys> = BTreeMap::new();
        for (collection, epoch, key) in document
            .wraps_for(&me)
            .filter_map(|wrap| Self::unwrap_collection(identity, binding, wrap))
        {
            keys.entry(collection)
                .or_insert_with(CollectionKeys::new)
                .add(epoch, &key);
        }
        keys
    }

    fn unwrap_collection(
        identity: &Identity,
        binding: AccessBinding,
        wrap: &AccessKeyWrap,
    ) -> Option<(u32, u32, Zeroizing<[u8; 32]>)> {
        let (collection, epoch) = wrap.collection.zip(wrap.epoch)?;
        let bytes =
            Self::unwrap(identity, &binding.collection_info(collection, epoch), wrap).ok()?;
        AccessBinding::key_bytes(&bytes)
            .ok()
            .map(|key| (collection, epoch, key))
    }

    fn unwrap(
        identity: &Identity,
        info: &[u8],
        wrap: &AccessKeyWrap,
    ) -> Result<Zeroizing<Vec<u8>>, DatabaseAccessError> {
        let wrapped = STANDARD.decode(&wrap.wrap).map_err(|error| {
            DatabaseAccessError::Damaged(format!("a key wrap is damaged: {error}"))
        })?;
        identity.unwrap_key(info, &wrapped).map_err(|_| {
            DatabaseAccessError::Denied(format!(
                "a key wrapped for {} does not open with it",
                identity.id()
            ))
        })
    }

    fn wraps_for(
        recipients: &[PublicIdentity],
        info: &[u8],
        key: &[u8; 32],
        collection: Option<(u32, u32)>,
    ) -> Result<Vec<AccessKeyWrap>, DatabaseAccessError> {
        recipients
            .iter()
            .map(|recipient| Self::wrap_for(recipient, info, key, collection))
            .collect()
    }

    fn wrap_for(
        recipient: &PublicIdentity,
        info: &[u8],
        key: &[u8; 32],
        collection: Option<(u32, u32)>,
    ) -> Result<AccessKeyWrap, DatabaseAccessError> {
        let wrapped = recipient.wrap(info, key)?;
        Ok(AccessKeyWrap {
            recipient: recipient.id().to_string(),
            collection: collection.map(|(id, _)| id),
            epoch: collection.map(|(_, epoch)| epoch),
            wrap: STANDARD.encode(wrapped),
        })
    }

    fn require_catalog(
        catalog: &ContainerCipher,
        key_check: &[u8; 16],
    ) -> Result<(), DatabaseAccessError> {
        catalog.check(key_check).map_err(|_| {
            DatabaseAccessError::Policy(
                "the catalog key in the access document does not match this database".into(),
            )
        })
    }

    fn require_trusted(
        anchor: &IdentityId,
        trust: &AccessTrust,
    ) -> Result<bool, DatabaseAccessError> {
        match trust {
            AccessTrust::Pinned(pinned) if pinned == anchor => Ok(false),
            AccessTrust::FirstUse => Ok(true),
            AccessTrust::Pinned(pinned) => Err(DatabaseAccessError::Policy(format!(
                "the database was created by {anchor}, not by the pinned trust anchor {pinned}"
            ))),
            AccessTrust::Unpinned => Err(DatabaseAccessError::Policy(format!(
                "the database's trust anchor is {anchor}; pin it (--trust-anchor, fylo.trustAnchor or FYLO_TRUST_ANCHOR), or trust it on first use (--trust-on-first-use)"
            ))),
        }
    }

    fn require_same_anchor(
        held: &IdentityId,
        found: &IdentityId,
    ) -> Result<(), DatabaseAccessError> {
        Some(found)
            .filter(|found| *found == held)
            .map(|_| ())
            .ok_or_else(|| {
                DatabaseAccessError::Policy(format!(
                    "the database's policy now starts at {found}, not {held}"
                ))
            })
    }

    fn denied(&self, what: &str) -> DatabaseAccessError {
        DatabaseAccessError::Denied(format!("{} {what}", self.identity.id()))
    }

    fn read(&self) -> RwLockReadGuard<'_, AccessState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, AccessState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for DatabaseAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseAccess")
            .field("identity", self.identity.id())
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fylo_identity::{Identity, KeyReference};

    use super::{AccessState, DatabaseAccess};
    use crate::{
        AccessAction, AccessDocument, AccessGrantRequest, AccessGrants, AccessPolicy, AccessRule,
    };

    const LEDGER: u32 = 7;

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

    fn writer_rule(writer: &Identity) -> AccessGrantRequest {
        AccessGrantRequest {
            keys: vec![writer.public()],
            label: None,
            rule: AccessRule {
                id: "writer".into(),
                identity: "writer".into(),
                actions: vec![AccessAction::Write],
                resources: vec!["collection:ledger".into()],
            },
        }
    }

    fn state(versions: Vec<AccessPolicy>) -> AccessState {
        AccessState {
            generation: 1,
            catalog: crate::catalog_keys::CatalogKeys::new(zeroize::Zeroizing::new([3; 32])),
            document: AccessDocument {
                format: AccessDocument::FORMAT.into(),
                policies: Vec::new(),
                keyring: Vec::new(),
                previous: None,
                original: None,
            },
            policy: versions.last().unwrap().clone(),
            policy_digest: String::new(),
            version_signers: Vec::new(),
            signers: DatabaseAccess::signers(&versions),
            versions,
            names: BTreeMap::from([(LEDGER, "ledger".to_owned())]),
            grants: AccessGrants::default(),
            keys: BTreeMap::new(),
        }
    }

    /// Version 1 makes `admin` admin, version 2 grants `writer`, and
    /// version 3 revokes it at LSN 10.
    fn revoked(admin: &Identity, writer: &Identity) -> Vec<AccessPolicy> {
        let first = AccessPolicy::first(&admin.public());
        let granted = first.with_grant(&writer_rule(writer), "v1".into()).unwrap();
        let (mut revoked, _) = granted.without_rule("writer", "v2".into()).unwrap();
        revoked.boundaries = BTreeMap::from([(LEDGER, 10)]);
        vec![first, granted, revoked]
    }

    #[test]
    fn a_revoked_writer_signs_validly_only_before_the_boundary() {
        let keys = Keys::new("access-could-write");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let state = state(revoked(&admin, &writer));
        assert!(DatabaseAccess::could_write(&state, LEDGER, 9, writer.id()));
        assert!(!DatabaseAccess::could_write(
            &state,
            LEDGER,
            10,
            writer.id()
        ));
        assert!(!DatabaseAccess::could_write(
            &state,
            LEDGER,
            11,
            writer.id()
        ));
    }

    #[test]
    fn a_writer_a_rekey_does_not_grant_signs_validly_nowhere_after_it() {
        let keys = Keys::new("access-could-write-rekey");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let mut versions = revoked(&admin, &writer);
        let rekeyed = versions[2].recording(
            "v3".into(),
            crate::AccessPolicyChange::now(crate::AccessPolicyChange::REKEY),
        );
        versions.push(rekeyed);
        let state = state(versions);
        assert!(!DatabaseAccess::could_write(&state, LEDGER, 5, writer.id()));
        assert!(DatabaseAccess::could_write(&state, LEDGER, 5, admin.id()));
    }

    #[test]
    fn an_admin_signs_validly_past_the_boundary() {
        let keys = Keys::new("access-could-write-admin");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let state = state(revoked(&admin, &writer));
        assert!(DatabaseAccess::could_write(&state, LEDGER, 11, admin.id()));
    }

    #[test]
    fn a_writer_granted_again_signs_validly_past_the_boundary() {
        let keys = Keys::new("access-could-write-again");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let mut versions = revoked(&admin, &writer);
        let again = versions[2]
            .with_grant(&writer_rule(&writer), "v3".into())
            .unwrap();
        versions.push(again);
        assert!(DatabaseAccess::could_write(
            &state(versions),
            LEDGER,
            11,
            writer.id()
        ));
    }

    #[test]
    fn an_identity_the_policy_never_granted_signs_validly_nowhere() {
        let keys = Keys::new("access-could-write-stranger");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let stranger = keys.identity("stranger");
        let state = state(revoked(&admin, &writer));
        assert!(!DatabaseAccess::could_write(
            &state,
            LEDGER,
            0,
            stranger.id()
        ));
    }

    #[test]
    fn a_collection_the_directory_does_not_hold_is_refused() {
        let keys = Keys::new("access-could-write-unknown");
        let (admin, writer) = (keys.identity("admin"), keys.identity("writer"));
        let state = state(revoked(&admin, &writer));
        assert!(!DatabaseAccess::could_write(
            &state,
            LEDGER + 1,
            0,
            admin.id()
        ));
    }
}
