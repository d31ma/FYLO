//! A protected `.vdbx` database (ADR 0021): created with an admin identity,
//! opened only with an identity the signed policy grants and a trusted
//! anchor, locked without an identity, and sealed so its documents and
//! collection names never appear in the file. Every key is generated here,
//! in key files under the test's temporary directory.

use std::path::PathBuf;

use fylo_identity::{Identity, KeyReference};
use serde_json::{Value, json};

use crate::{
    AccessAction, AccessGrantRequest, AccessPolicy, AccessRule, AccessTrust, CollectionKind,
    DatabaseIdentity, DatabaseKey, DatabaseRekey, NativeRoot, NativeStorageErrorCode,
    NativeWriteRoot, PutDocumentOptions, RootOpenOptions,
};

/// Text in every document, which must never appear in a protected file.
const CANARY: &str = "WOMBAT-CANARY";
/// A collection name, which the sealed directory must hide too.
const LEDGER: &str = "wombat-ledger";
const DOCUMENT: &str = "4VRNF560001";

/// A directory holding `db.vdbx`, protected with a generated admin
/// identity, and the key files of the identities a test makes.
struct Protected {
    directory: PathBuf,
    admin: Identity,
}

impl Protected {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let admin = identity_in(&directory, "admin");
        NativeRoot::create_protected_database(directory.join("db.vdbx"), &admin).unwrap();
        Self { directory, admin }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("db.vdbx")
    }

    fn identity(&self, name: &str) -> Identity {
        identity_in(&self.directory, name)
    }

    fn as_admin(&self) -> RootOpenOptions {
        self.options(&self.admin, AccessTrust::Pinned(self.admin.id().clone()))
    }

    fn options(&self, identity: &Identity, trust: AccessTrust) -> RootOpenOptions {
        let identity = Identity::open(identity.reference()).unwrap();
        RootOpenOptions::new(self.path()).with_identity(DatabaseIdentity::new(identity, trust))
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.as_admin()).unwrap()
    }

    /// Open as `identity`, trusting the admin as the anchor, after this
    /// process forgets the database: one process opens it as one identity.
    fn as_identity(&self, identity: &Identity) -> RootOpenOptions {
        self.reopen();
        self.options(identity, AccessTrust::Pinned(self.admin.id().clone()))
    }

    /// Grant `identity`, named `name`, `actions` on `resources` as the
    /// admin.
    fn grant(
        &self,
        name: &str,
        identity: &Identity,
        actions: &[AccessAction],
        resources: &[&str],
    ) -> AccessPolicy {
        self.reopen();
        self.writer()
            .grant_access(&request(name, identity, actions, resources))
            .unwrap()
    }

    fn root(&self) -> NativeRoot {
        NativeRoot::open_profiled(self.as_admin()).unwrap()
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    fn contains(&self, text: &str) -> bool {
        std::fs::read(self.path())
            .unwrap()
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    fn open_code(&self, options: RootOpenOptions) -> NativeStorageErrorCode {
        self.reopen();
        NativeRoot::open_profiled(options).unwrap_err().code()
    }
}

impl Drop for Protected {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn identity_in(directory: &std::path::Path, name: &str) -> Identity {
    Identity::create(&KeyReference::File(directory.join(format!("{name}.pem")))).unwrap()
}

fn request(
    name: &str,
    identity: &Identity,
    actions: &[AccessAction],
    resources: &[&str],
) -> AccessGrantRequest {
    AccessGrantRequest {
        keys: vec![identity.public()],
        label: None,
        rule: AccessRule {
            id: format!("{name}-rule"),
            identity: name.to_owned(),
            actions: actions.to_vec(),
            resources: resources
                .iter()
                .map(|resource| (*resource).to_owned())
                .collect(),
        },
    }
}

fn put(writer: &NativeWriteRoot, collection: &str, id: &str) {
    writer
        .put_document(
            collection,
            id,
            order().to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn order() -> Value {
    json!({ "customer": format!("customer {CANARY}"), "city": "Halifax" })
}

fn write_ledger(database: &Protected) {
    let writer = database.writer();
    writer
        .create_collection(LEDGER, CollectionKind::Document, None)
        .unwrap();
    writer
        .put_document(
            LEDGER,
            DOCUMENT,
            order().to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
    writer.rebuild_collection(LEDGER).unwrap();
}

fn read(root: &NativeRoot) -> Value {
    let bytes = root
        .collection(LEDGER)
        .unwrap()
        .read_document(DOCUMENT)
        .unwrap()
        .bytes;
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn the_admin_writes_and_reads_a_protected_database() {
    let database = Protected::create("vdbx-protected-round-trip");
    write_ledger(&database);
    assert_eq!(read(&database.root()), order());
    database.reopen();
    let root = database.root();
    assert_eq!(read(&root), order());
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec![LEDGER.to_owned()]
    );
}

#[test]
fn a_protected_database_hides_its_documents_and_collection_names() {
    let database = Protected::create("vdbx-protected-hidden");
    write_ledger(&database);
    database.reopen();
    assert!(!database.contains(CANARY));
    assert!(!database.contains(LEDGER));
}

#[test]
fn the_creator_is_the_anchor_and_an_admin() {
    let database = Protected::create("vdbx-protected-anchor");
    let access = database.root().database_access().unwrap().unwrap();
    assert_eq!(access.anchor(), database.admin.id());
    assert!(access.grants().is_admin());
    assert!(!access.trusted_on_first_use());
}

#[test]
fn without_an_identity_the_database_is_locked() {
    let database = Protected::create("vdbx-protected-locked");
    write_ledger(&database);
    database.reopen();
    let root = NativeRoot::open_profiled(RootOpenOptions::new(database.path())).unwrap();
    assert_eq!(
        root.database_collection_names().unwrap(),
        Vec::<String>::new()
    );
    assert!(matches!(root.database_access(), Some(None)));
    let error = root.collection(LEDGER).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::NoIdentity);
}

#[test]
fn without_an_identity_nothing_is_written() {
    let database = Protected::create("vdbx-protected-locked-write");
    let writer = NativeWriteRoot::open_profiled(RootOpenOptions::new(database.path())).unwrap();
    let error = writer
        .create_collection(LEDGER, CollectionKind::Document, None)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::NoIdentity);
}

#[test]
fn an_identity_without_a_grant_is_refused() {
    let database = Protected::create("vdbx-protected-stranger");
    let stranger = database.identity("stranger");
    let options = database.options(&stranger, AccessTrust::Pinned(database.admin.id().clone()));
    assert_eq!(
        database.open_code(options),
        NativeStorageErrorCode::PermissionDenied
    );
}

#[test]
fn another_pinned_anchor_is_refused() {
    let database = Protected::create("vdbx-protected-other-anchor");
    let other = database.identity("other");
    let options = database.options(&database.admin, AccessTrust::Pinned(other.id().clone()));
    assert_eq!(database.open_code(options), NativeStorageErrorCode::Policy);
}

#[test]
fn an_unpinned_open_is_refused() {
    let database = Protected::create("vdbx-protected-unpinned");
    let options = database.options(&database.admin, AccessTrust::Unpinned);
    assert_eq!(database.open_code(options), NativeStorageErrorCode::Policy);
}

#[test]
fn trust_on_first_use_opens_and_says_so() {
    let database = Protected::create("vdbx-protected-first-use");
    write_ledger(&database);
    database.reopen();
    let root = NativeRoot::open_profiled(database.options(&database.admin, AccessTrust::FirstUse))
        .unwrap();
    let access = root.database_access().unwrap().unwrap();
    assert!(access.trusted_on_first_use());
    assert_eq!(access.anchor(), database.admin.id());
    assert_eq!(read(&root), order());
}

#[test]
fn a_database_another_admin_made_in_its_place_is_refused() {
    let database = Protected::create("vdbx-protected-replaced");
    let intruder = database.identity("intruder");
    std::fs::remove_file(database.path()).unwrap();
    NativeRoot::create_protected_database(database.path(), &intruder).unwrap();
    let options = database.options(&intruder, AccessTrust::Pinned(database.admin.id().clone()));
    assert_eq!(database.open_code(options), NativeStorageErrorCode::Policy);
}

#[test]
fn a_second_identity_in_one_process_is_refused() {
    let database = Protected::create("vdbx-protected-second-identity");
    let _root = database.root();
    let other = database.identity("other");
    let options = database.options(&other, AccessTrust::Pinned(database.admin.id().clone()));
    let error = NativeRoot::open_profiled(options).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::PermissionDenied);
}

#[test]
fn damaged_access_slots_refuse_the_open() {
    let database = Protected::create("vdbx-protected-damaged");
    database.reopen();
    let mut bytes = std::fs::read(database.path()).unwrap();
    let magic = b"VDXACCS\0";
    let at = bytes
        .windows(magic.len())
        .position(|window| window == magic)
        .unwrap();
    bytes[at + 40] ^= 0xFF;
    std::fs::write(database.path(), bytes).unwrap();
    assert_eq!(
        database.open_code(database.as_admin()),
        NativeStorageErrorCode::CorruptMetadata
    );
}

#[test]
fn rekey_refuses_a_protected_database() {
    let database = Protected::create("vdbx-protected-rekey");
    let key = DatabaseKey::new(vec![b'k'; 32]);
    let error = database
        .writer()
        .rekey_database(DatabaseRekey::Rotate(&key))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
}

#[test]
fn a_sealed_pack_refuses_a_protected_database() {
    let database = Protected::create("vdbx-protected-sealed");
    let out = database.directory.join("copy.vdbx");
    let error = database
        .writer()
        .pack_sealed_database(&out, false, None)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    assert!(!out.exists());
}

const SECRETS: &str = "secrets";
const SECOND: &str = "4VRNF560002";

fn write_ledger_and_secrets(database: &Protected) {
    write_ledger(database);
    let writer = database.writer();
    writer
        .create_collection(SECRETS, CollectionKind::Document, None)
        .unwrap();
    put(&writer, SECRETS, DOCUMENT);
}

#[test]
fn a_reader_reads_the_collection_it_is_granted_and_nothing_else() {
    let database = Protected::create("vdbx-protected-reader");
    write_ledger_and_secrets(&database);
    let reader = database.identity("reader");
    let policy = database.grant(
        "reader",
        &reader,
        &[AccessAction::Read],
        &["collection:wombat-ledger"],
    );
    assert_eq!(policy.version, 2);
    let root = NativeRoot::open_profiled(database.as_identity(&reader)).unwrap();
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec![LEDGER.to_owned()]
    );
    assert_eq!(read(&root), order());
    let secrets = root
        .collection(SECRETS)
        .map(|collection| collection.read_document(DOCUMENT));
    assert!(!matches!(secrets, Ok(Ok(_))));
}

#[test]
fn a_reader_may_not_write() {
    let database = Protected::create("vdbx-protected-reader-write");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant(
        "reader",
        &reader,
        &[AccessAction::Read],
        &["collection:wombat-ledger"],
    );
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&reader)).unwrap();
    let error = writer
        .put_document(
            LEDGER,
            SECOND,
            order().to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::PermissionDenied);
}

#[test]
fn a_writer_writes_the_collection_it_is_granted() {
    let database = Protected::create("vdbx-protected-writer");
    write_ledger_and_secrets(&database);
    let writer_identity = database.identity("writer");
    database.grant(
        "writer",
        &writer_identity,
        &[AccessAction::Write],
        &["collection:wombat-ledger"],
    );
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&writer_identity)).unwrap();
    put(&writer, LEDGER, SECOND);
    drop(writer);
    let root = NativeRoot::open_profiled(database.as_identity(&database.admin)).unwrap();
    let ids = root.collection(LEDGER).unwrap().document_ids().unwrap();
    assert_eq!(ids.len(), 2);
}

#[test]
fn a_grant_on_every_collection_covers_one_created_later() {
    let database = Protected::create("vdbx-protected-every");
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    write_ledger(&database);
    let root = NativeRoot::open_profiled(database.as_identity(&reader)).unwrap();
    assert_eq!(read(&root), order());
}

#[test]
fn only_an_admin_grants() {
    let database = Protected::create("vdbx-protected-not-admin");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    let other = database.identity("other");
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&reader)).unwrap();
    let error = writer
        .grant_access(&request(
            "other",
            &other,
            &[AccessAction::Read],
            &["database"],
        ))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::PermissionDenied);
}

#[test]
fn a_second_admin_grants_and_its_version_is_checked() {
    let database = Protected::create("vdbx-protected-second-admin");
    write_ledger(&database);
    let second = database.identity("second");
    database.grant("second", &second, &[AccessAction::Admin], &["database"]);
    let reader = database.identity("reader");
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&second)).unwrap();
    let policy = writer
        .grant_access(&request(
            "reader",
            &reader,
            &[AccessAction::Read],
            &["collection:*"],
        ))
        .unwrap();
    assert_eq!(policy.version, 3);
    drop(writer);
    let root = NativeRoot::open_profiled(database.as_identity(&reader)).unwrap();
    assert_eq!(read(&root), order());
}

#[test]
fn a_revoked_identity_is_refused() {
    let database = Protected::create("vdbx-protected-revoked");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let policy = database.writer().revoke_access("reader-rule").unwrap();
    assert_eq!(policy.version, 3);
    assert!(!policy.identities.contains_key("reader"));
    let options = database.as_identity(&reader);
    assert_eq!(
        database.open_code(options),
        NativeStorageErrorCode::PermissionDenied
    );
}

#[test]
fn after_a_revocation_old_and_new_documents_both_read() {
    let database = Protected::create("vdbx-protected-epochs");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant(
        "reader",
        &reader,
        &[AccessAction::Read],
        &["collection:wombat-ledger"],
    );
    database.reopen();
    let writer = database.writer();
    writer.revoke_access("reader-rule").unwrap();
    put(&writer, LEDGER, SECOND);
    drop(writer);
    database.reopen();
    let root = database.root();
    let ledger = root.collection(LEDGER).unwrap();
    assert_eq!(ledger.document_ids().unwrap().len(), 2);
    assert_eq!(read(&root), order());
    let second = ledger.read_document(SECOND).unwrap().bytes;
    assert_eq!(serde_json::from_slice::<Value>(&second).unwrap(), order());
}

#[test]
fn revoking_the_last_admin_is_refused() {
    let database = Protected::create("vdbx-protected-last-admin");
    let error = database
        .writer()
        .revoke_access(AccessPolicy::CREATOR_RULE)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Policy);
}

#[test]
fn a_rule_id_in_use_is_refused() {
    let database = Protected::create("vdbx-protected-rule-taken");
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let error = database
        .writer()
        .grant_access(&request(
            "reader",
            &reader,
            &[AccessAction::Write],
            &["collection:*"],
        ))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Policy);
}

#[test]
fn an_unknown_resource_is_refused() {
    let database = Protected::create("vdbx-protected-bad-resource");
    let reader = database.identity("reader");
    let error = database
        .writer()
        .grant_access(&request(
            "reader",
            &reader,
            &[AccessAction::Read],
            &["table:orders"],
        ))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Policy);
}
