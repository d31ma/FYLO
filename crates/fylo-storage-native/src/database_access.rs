use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use aes_gcm::Aes256Gcm;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fylo_format::ContainerAccessSlot;
use fylo_identity::{Identity, IdentityId, PublicIdentity};
use zeroize::Zeroizing;

use crate::access_binding::AccessBinding;
use crate::access_policy_chain::AccessPolicyChain;
use crate::collection_keys::CollectionKeys;
use crate::{
    AccessAction, AccessDocument, AccessGrantRequest, AccessGrants, AccessKeyWrap, AccessPolicy,
    AccessTrust, ContainerCipher, ContainerCipherError, DatabaseAccessError, DatabaseIdentity,
};

/// The access state of a protected `.vdbx` database as one identity opened
/// it (ADR 0021): the checked policy, what it grants this identity, and the
/// collection keys it unwrapped. Every handle of the file shares it through
/// the file's [`ContainerCipher`].
pub struct DatabaseAccess {
    identity: Arc<Identity>,
    binding: AccessBinding,
    catalog_key: Zeroizing<[u8; 32]>,
    anchor: IdentityId,
    trusted_on_first_use: bool,
    state: RwLock<AccessState>,
}

struct AccessState {
    generation: u64,
    document: AccessDocument,
    policy: AccessPolicy,
    policy_digest: String,
    names: BTreeMap<u32, String>,
    grants: AccessGrants,
    keys: BTreeMap<u32, CollectionKeys>,
}

/// A collection's id, an epoch, and that epoch's key.
type EpochKey = (u32, u32, Zeroizing<[u8; 32]>);

/// A change of the access document made in memory, to write to the access
/// slots before it takes effect ([`DatabaseAccess::commit`]): new keys,
/// a newly named collection, and a new policy version.
pub(crate) struct AccessChange {
    pub(crate) slot: ContainerAccessSlot,
    keys: Vec<EpochKey>,
    collection: Option<(u32, String)>,
    policy: Option<(AccessPolicy, String)>,
}

impl DatabaseAccess {
    const FIRST_EPOCH: u32 = 1;

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
        };
        Self::slot(1, &document)
    }

    /// Open a protected database's access slot as `identity`: unwrap the
    /// catalog key, check it against the superblock's key-check value,
    /// check the policy chain and its anchor, and unwrap the collection
    /// keys granted. Returns the file's cipher.
    pub(crate) fn open(
        slot: &ContainerAccessSlot,
        database: [u8; 32],
        key_check: &[u8; 16],
        identity: &DatabaseIdentity,
    ) -> Result<Arc<ContainerCipher>, DatabaseAccessError> {
        let binding = AccessBinding::new(database);
        let document = Self::parse(&slot.body)?;
        let catalog_key = Self::unwrap_catalog(&document, identity.identity(), binding)?;
        let catalog = ContainerCipher::from_key(&catalog_key);
        Self::require_catalog(&catalog, key_check)?;
        let chain = AccessPolicyChain::verify(&document.policies, &catalog, binding)?;
        let trusted_on_first_use = Self::require_trusted(chain.anchor(), identity.trust())?;
        let keys = Self::unwrap_collections(&document, identity.identity(), binding);
        let access = DatabaseAccess {
            identity: Arc::clone(identity.identity()),
            binding,
            catalog_key,
            anchor: chain.anchor().clone(),
            trusted_on_first_use,
            state: RwLock::new(AccessState {
                generation: slot.generation,
                policy_digest: chain.digest(),
                policy: chain.policy().clone(),
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

    /// The access slot generation this state was read from.
    pub(crate) fn generation(&self) -> u64 {
        self.read().generation
    }

    /// Learn the directory's collections, and what this identity may do
    /// with each. Until a collection is bound, nothing may be written to it.
    pub(crate) fn bind_directory(&self, collections: &[(u32, String)]) {
        let mut state = self.write();
        let names: Vec<&str> = collections.iter().map(|(_, name)| name.as_str()).collect();
        state.grants = AccessGrants::resolve(&state.policy, self.identity.id(), &names);
        state.names = collections.iter().cloned().collect();
    }

    /// Take in a newer access slot another process wrote: the policy that
    /// follows from it, and any collection key wrapped for this identity
    /// since.
    pub(crate) fn reload(&self, slot: &ContainerAccessSlot) -> Result<(), DatabaseAccessError> {
        let document = Self::parse(&slot.body)?;
        let catalog = ContainerCipher::from_key(&self.catalog_key);
        let chain = AccessPolicyChain::verify(&document.policies, &catalog, self.binding)?;
        Self::require_same_anchor(&self.anchor, chain.anchor())?;
        let keys = Self::unwrap_collections(&document, &self.identity, self.binding);
        let mut state = self.write();
        let names: Vec<String> = state.names.values().cloned().collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        state.grants = AccessGrants::resolve(chain.policy(), self.identity.id(), &names);
        state.generation = slot.generation;
        state.policy_digest = chain.digest();
        state.policy = chain.policy().clone();
        state.document = document;
        state.keys = keys;
        Ok(())
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
        let key = AccessBinding::random_key()?;
        let state = self.read();
        let info = self.binding.collection_info(id, Self::FIRST_EPOCH);
        let wraps = Self::wraps_for(
            &state.policy.collection_keys(name),
            &info,
            &key,
            Some((id, Self::FIRST_EPOCH)),
        )?;
        let mut document = state.document.clone();
        document.keyring.retain(|wrap| wrap.collection != Some(id));
        document.keyring.extend(wraps);
        Ok(AccessChange {
            slot: Self::slot(state.generation + 1, &document)?,
            keys: vec![(id, Self::FIRST_EPOCH, key)],
            collection: Some((id, name.to_owned())),
            policy: None,
        })
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
        self.prepare_policy(&state, policy, &[])
    }

    /// Prepare policy version n+1 without the rule `id`, signed by this
    /// admin. Every collection the rule covered starts a new key epoch, and
    /// the keyring is rebuilt, so an identity the rule granted gets no key
    /// for what is written from now on. What was written before stays
    /// sealed with the old epoch until `fylo rekey` rewrites it.
    pub(crate) fn prepare_revoke(&self, id: &str) -> Result<AccessChange, DatabaseAccessError> {
        self.require_admin()?;
        let state = self.read();
        let (policy, removed) = state.policy.without_rule(id, state.policy_digest.clone())?;
        let covered: Vec<u32> = state
            .names
            .iter()
            .filter(|(_, name)| removed.strongest_on_collection(name).is_some())
            .map(|(collection, _)| *collection)
            .collect();
        self.prepare_policy(&state, policy, &covered)
    }

    fn prepare_policy(
        &self,
        state: &AccessState,
        policy: AccessPolicy,
        new_epochs: &[u32],
    ) -> Result<AccessChange, DatabaseAccessError> {
        let catalog = ContainerCipher::from_key(&self.catalog_key);
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
        let keyring = self.rebuild_keyring(&policy, &state.names, &keys)?;
        let mut policies = state.document.policies.clone();
        policies.push(envelope);
        let document = AccessDocument {
            format: AccessDocument::FORMAT.to_owned(),
            policies,
            keyring,
        };
        Ok(AccessChange {
            slot: Self::slot(state.generation + 1, &document)?,
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
        names: &BTreeMap<u32, String>,
        keys: &BTreeMap<u32, CollectionKeys>,
    ) -> Result<Vec<AccessKeyWrap>, DatabaseAccessError> {
        let mut keyring = Self::wraps_for(
            &policy.granted_keys(),
            &self.binding.catalog_info(),
            &self.catalog_key,
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
        let document: AccessDocument = serde_json::from_slice(body).map_err(|error| {
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
