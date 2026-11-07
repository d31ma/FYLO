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
fn a_sealed_pack_of_a_protected_database_takes_no_shared_key() {
    let database = Protected::create("vdbx-protected-sealed");
    let out = database.directory.join("copy.vdbx");
    let key = DatabaseKey::new(vec![b'k'; 32]);
    let error = database
        .writer()
        .pack_sealed_database(&out, Some(&key))
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
    assert_eq!(policy.boundaries.len(), 1);
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

/// Rewrite every record frame the admin signed in the file: `mutate`
/// changes its stored bytes in place, and the frame's checksum is written
/// again, so the frame reads as whole and only its signature is wrong.
/// Returns how many frames it rewrote.
fn rewrite_signed_frames(database: &Protected, mutate: impl Fn(&mut [u8])) -> usize {
    rewrite_frames(database, &[], mutate)
}

/// As [`rewrite_signed_frames`], for the signed frames whose last bytes
/// differ from `before`, the file as it was: the frames written since.
fn rewrite_frames(database: &Protected, before: &[u8], mutate: impl Fn(&mut [u8])) -> usize {
    database.reopen();
    let fingerprint = database.admin.public().fingerprint();
    let mut bytes = std::fs::read(database.path()).unwrap();
    let ends: Vec<usize> = bytes
        .windows(fingerprint.len())
        .enumerate()
        .filter(|(_, window)| *window == fingerprint)
        .map(|(at, _)| at + fingerprint.len() + 64)
        .filter(|end| before.get(end - 64..*end) != bytes.get(end - 64..*end))
        .collect();
    let mut rewritten = 0;
    for end in ends {
        let Some(start) = (end.saturating_sub(1 << 20)..end.saturating_sub(8))
            .rev()
            .find(|start| frame_ends_at(&bytes, *start, end))
        else {
            continue;
        };
        mutate(&mut bytes[start + 8..end]);
        let checksum = fylo_format::Crc32c::checksum(&bytes[start + 8..end]);
        bytes[start + 4..start + 8].copy_from_slice(&checksum.to_le_bytes());
        rewritten += 1;
    }
    std::fs::write(database.path(), bytes).unwrap();
    rewritten
}

fn frame_ends_at(bytes: &[u8], start: usize, end: usize) -> bool {
    let length = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
    let checksum = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
    usize::try_from(length).is_ok_and(|length| start + 8 + length == end)
        && fylo_format::Crc32c::checksum(&bytes[start + 8..end]) == checksum
}

fn read_code(database: &Protected) -> NativeStorageErrorCode {
    database.reopen();
    let root = database.root();
    root.collection(LEDGER)
        .and_then(|ledger| ledger.read_document(DOCUMENT).map(|_| ()))
        .unwrap_err()
        .code()
}

#[test]
fn every_record_of_a_protected_database_is_signed() {
    let database = Protected::create("vdbx-protected-signed");
    write_ledger(&database);
    assert!(rewrite_signed_frames(&database, |_| ()) >= 2);
    database.reopen();
    assert_eq!(read(&database.root()), order());
}

#[test]
fn a_record_whose_signature_was_changed_is_refused() {
    let database = Protected::create("vdbx-protected-forged");
    write_ledger(&database);
    let last = |stored: &mut [u8]| {
        let at = stored.len() - 1;
        stored[at] ^= 0x01;
    };
    assert!(rewrite_signed_frames(&database, last) >= 2);
    assert_eq!(read_code(&database), NativeStorageErrorCode::Signature);
}

#[test]
fn a_record_stripped_of_its_signed_flag_is_refused() {
    let database = Protected::create("vdbx-protected-unsigned");
    write_ledger(&database);
    let unsigned = |stored: &mut [u8]| stored[0] &= !crate::container_record::SIGNED;
    assert!(rewrite_signed_frames(&database, unsigned) >= 2);
    assert_eq!(read_code(&database), NativeStorageErrorCode::Signature);
}

#[test]
fn verify_with_the_identity_reports_a_bad_signature() {
    let database = Protected::create("vdbx-protected-verify");
    write_ledger(&database);
    let last = |stored: &mut [u8]| {
        let at = stored.len() - 1;
        stored[at] ^= 0x01;
    };
    rewrite_signed_frames(&database, last);
    let mut file = crate::ContainerFile::open_reader(&database.path()).unwrap();
    let identity = DatabaseIdentity::new(
        Identity::open(database.admin.reference()).unwrap(),
        AccessTrust::Pinned(database.admin.id().clone()),
    );
    file.unlock_with_identity(&identity).unwrap();
    let report = file.verify().unwrap();
    assert!(!report.ok);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.code == "badSignature")
    );
}

#[test]
fn verify_with_the_identity_passes_a_signed_database() {
    let database = Protected::create("vdbx-protected-verify-ok");
    write_ledger(&database);
    database.reopen();
    let mut file = crate::ContainerFile::open_reader(&database.path()).unwrap();
    let identity = DatabaseIdentity::new(
        Identity::open(database.admin.reference()).unwrap(),
        AccessTrust::Pinned(database.admin.id().clone()),
    );
    file.unlock_with_identity(&identity).unwrap();
    let report = file.verify().unwrap();
    assert!(report.ok, "{:?}", report.problems);
}

#[test]
fn compaction_signs_the_records_it_rewrites() {
    let database = Protected::create("vdbx-protected-compaction");
    write_ledger(&database);
    let writer = database.writer();
    put(&writer, LEDGER, SECOND);
    writer.delete_document(LEDGER, SECOND).unwrap();
    writer.compact_collection(LEDGER).unwrap();
    drop(writer);
    database.reopen();
    assert_eq!(read(&database.root()), order());
    let mut file = crate::ContainerFile::open_reader(&database.path()).unwrap();
    let identity = DatabaseIdentity::new(
        Identity::open(database.admin.reference()).unwrap(),
        AccessTrust::Pinned(database.admin.id().clone()),
    );
    file.unlock_with_identity(&identity).unwrap();
    assert!(file.verify().unwrap().ok);
}

#[test]
fn records_before_a_bad_signature_read_and_writes_past_it_are_refused() {
    let database = Protected::create("vdbx-protected-fallback");
    write_ledger(&database);
    database.reopen();
    let before = std::fs::read(database.path()).unwrap();
    put(&database.writer(), LEDGER, SECOND);
    let flip = |stored: &mut [u8]| {
        let at = stored.len() - 1;
        stored[at] ^= 0x01;
    };
    assert!(rewrite_frames(&database, &before, flip) >= 1);
    database.reopen();
    let root = database.root();
    assert_eq!(read(&root), order());
    let ids = root.collection(LEDGER).unwrap().document_ids().unwrap();
    assert_eq!(ids, vec![DOCUMENT.to_owned()]);
    drop(root);
    database.reopen();
    let writer = NativeWriteRoot::open_profiled(database.as_admin()).unwrap();
    let third = order().to_string();
    let refused = writer
        .put_document(
            LEDGER,
            "4VRNF560003",
            third.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::Signature);
}

/// Per-commit and open costs of a protected database against a plain and a
/// key-encrypted one (ADR 0021, phase 4). Run in release with
/// `--run-ignored only --no-capture`; it prints, it asserts nothing about
/// speed.
#[test]
#[ignore = "a measurement, run by hand in release"]
fn measure_protected_costs() {
    const PUTS: usize = 400;
    const OPENS: usize = 40;
    let identity = Protected::create("vdbx-cost-sign").identity("signer");
    let message = [7_u8; 120];
    let signed = time_each(1000, || {
        std::hint::black_box(identity.sign(&message));
    });
    let signature = identity.sign(&message);
    let public = identity.public();
    let verified = time_each(1000, || public.verify(&message, &signature).unwrap());
    println!("ecdsa p256 sign: {}", summary(&signed));
    println!("ecdsa p256 verify: {}", summary(&verified));
    for mode in ["plain", "encrypted", "protected"] {
        let database = Protected::create(&format!("vdbx-cost-{mode}"));
        let options = cost_options(&database, mode);
        let writer = NativeWriteRoot::open_profiled(options.clone()).unwrap();
        writer
            .create_collection(LEDGER, CollectionKind::Document, None)
            .unwrap();
        let mut number = 0;
        let puts = time_each(PUTS, || {
            number += 1;
            let id = format!("4VRNF5{:05}", 10_000 + number);
            put(&writer, LEDGER, &id);
        });
        drop(writer);
        let opens = time_each(OPENS, || {
            database.reopen();
            let root = NativeRoot::open_profiled(options.clone()).unwrap();
            root.collection(LEDGER).unwrap().document_ids().unwrap();
        });
        println!("{mode} durable put: {}", summary(&puts));
        println!("{mode} open and list {PUTS} documents: {}", summary(&opens));
    }
}

fn cost_options(database: &Protected, mode: &str) -> RootOpenOptions {
    let path = database.directory.join(format!("{mode}.vdbx"));
    let key = DatabaseKey::new(vec![b'k'; 32]);
    match mode {
        "plain" => NativeRoot::create_database(&path).unwrap(),
        "encrypted" => NativeRoot::create_encrypted_database(&path, &key).unwrap(),
        _ => NativeRoot::create_protected_database(&path, &database.admin).unwrap(),
    }
    let options = RootOpenOptions::new(&path);
    match mode {
        "plain" => options,
        "encrypted" => options.with_database_key(key),
        _ => options.with_identity(DatabaseIdentity::new(
            Identity::open(database.admin.reference()).unwrap(),
            AccessTrust::Pinned(database.admin.id().clone()),
        )),
    }
}

fn time_each(count: usize, mut work: impl FnMut()) -> Vec<std::time::Duration> {
    (0..count)
        .map(|_| {
            let started = std::time::Instant::now();
            work();
            started.elapsed()
        })
        .collect()
}

fn summary(times: &[std::time::Duration]) -> String {
    let mut sorted = times.to_vec();
    sorted.sort();
    let at = |percent: usize| sorted[(sorted.len() - 1) * percent / 100];
    format!("p50 {:?}, p95 {:?}, n {}", at(50), at(95), sorted.len())
}

/// The bytes of both access slots of `bytes`, a protected file, and where
/// they start.
fn access_slots(bytes: &[u8]) -> (usize, Vec<u8>) {
    let magic = b"VDXACCS\0";
    let at = bytes
        .windows(magic.len())
        .position(|window| window == magic)
        .unwrap();
    (at, bytes[at..at + 512 * 1024].to_vec())
}

#[test]
fn a_revocation_rotates_the_catalog_key_so_old_slots_open_no_directory() {
    let database = Protected::create("vdbx-protected-catalog");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let kept = std::fs::read(database.path()).unwrap();
    database.writer().revoke_access("reader-rule").unwrap();
    database.reopen();
    let root = database.root();
    assert!(!root.database_access().flatten().unwrap().rotation_pending());
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec![LEDGER.to_owned()]
    );
    assert_eq!(read(&root), order());
    drop(root);
    // The reader puts back the slots it kept, whose wrap of the old catalog
    // key it can open: the directory is sealed under the new key now.
    database.reopen();
    let mut spliced = std::fs::read(database.path()).unwrap();
    let (at, slots) = access_slots(&kept);
    assert_eq!(access_slots(&spliced).0, at);
    spliced[at..at + slots.len()].copy_from_slice(&slots);
    std::fs::write(database.path(), &spliced).unwrap();
    let opened = NativeRoot::open_profiled(database.as_identity(&reader));
    let names = opened
        .as_ref()
        .ok()
        .and_then(NativeRoot::database_collection_names)
        .unwrap_or_default();
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn a_rotation_stopped_before_the_reseal_reads_and_the_next_change_finishes_it() {
    let database = Protected::create("vdbx-protected-catalog-stopped");
    write_ledger(&database);
    let reader = database.identity("reader");
    database.grant("reader", &reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let root = database.root();
    root.database()
        .unwrap()
        .revoke_unsettled("reader-rule")
        .unwrap();
    drop(root);
    database.reopen();
    let root = database.root();
    assert!(root.database_access().flatten().unwrap().rotation_pending());
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec![LEDGER.to_owned()]
    );
    assert_eq!(read(&root), order());
    drop(root);
    assert_eq!(
        database.open_code(database.as_identity(&reader)),
        NativeStorageErrorCode::PermissionDenied
    );
    let other = database.identity("other");
    database.grant("other", &other, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let root = database.root();
    assert!(!root.database_access().flatten().unwrap().rotation_pending());
    assert_eq!(read(&root), order());
    drop(root);
    let root = NativeRoot::open_profiled(database.as_identity(&other)).unwrap();
    assert_eq!(read(&root), order());
}

/// Grant `reader` read on every collection, revoke it with the rotation
/// stopped after its commit point, and return the file as it was before
/// the revocation, which the reader may have kept.
fn revoke_reader_unsettled(database: &Protected, reader: &Identity) -> Vec<u8> {
    database.grant("reader", reader, &[AccessAction::Read], &["collection:*"]);
    database.reopen();
    let kept = std::fs::read(database.path()).unwrap();
    let root = database.root();
    root.database()
        .unwrap()
        .revoke_unsettled("reader-rule")
        .unwrap();
    drop(root);
    database.reopen();
    kept
}

/// Write three collections, so a settle reseals three directory blocks.
fn write_three_collections(database: &Protected) {
    write_ledger(database);
    let writer = database.writer();
    for name in ["wombat-orders", "wombat-notes"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
}

/// The policy version a revocation after create and one grant writes.
const REVOCATION_VERSION: u64 = 3;

fn rotation_pending(database: &Protected) -> Option<u64> {
    database.reopen();
    database
        .root()
        .database_access()
        .flatten()
        .unwrap()
        .catalog_rotation_pending()
}

fn last_audit_kind(database: &Protected) -> String {
    database.reopen();
    let audit = database.root().database_access().flatten().unwrap().audit();
    audit.last().unwrap().kind.clone()
}

/// The collection names `identity` reads after putting back the access
/// slots of `kept` into a copy of the file as it is now.
fn names_with_kept_slots(database: &Protected, kept: &[u8], identity: &Identity) -> Vec<String> {
    database.reopen();
    let now = std::fs::read(database.path()).unwrap();
    let mut spliced = now.clone();
    let (at, slots) = access_slots(kept);
    spliced[at..at + slots.len()].copy_from_slice(&slots);
    std::fs::write(database.path(), &spliced).unwrap();
    let opened = NativeRoot::open_profiled(database.as_identity(identity));
    let names = opened
        .as_ref()
        .ok()
        .and_then(NativeRoot::database_collection_names)
        .unwrap_or_default();
    drop(opened);
    database.reopen();
    std::fs::write(database.path(), &now).unwrap();
    names
}

/// Make the settle meet `fault` once `passes` of its writes have passed,
/// as a crash there leaves the file, then check the rotation is still
/// pending, the admin reads everything, and a settle finishes it.
fn settle_stopped_at(fault: crate::container_file::fault::ContainerFault, passes: usize) {
    let database = Protected::create("vdbx-protected-settle-stopped");
    write_three_collections(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    let writer = database.writer();
    crate::container_file::fault::arm_after(fault, passes);
    assert!(writer.settle_access().is_err());
    assert!(!crate::container_file::fault::pending());
    drop(writer);
    assert_eq!(rotation_pending(&database), Some(REVOCATION_VERSION));
    database.reopen();
    let root = database.root();
    assert_eq!(root.database_collection_names().unwrap().len(), 3);
    assert_eq!(read(&root), order());
    drop(root);
    database.reopen();
    assert_eq!(
        database.writer().settle_access().unwrap(),
        Some(REVOCATION_VERSION)
    );
    assert_eq!(rotation_pending(&database), None);
    assert_eq!(read(&database.root()), order());
}

#[test]
fn a_settle_stopped_before_the_first_directory_block_finishes_on_the_next_settle() {
    settle_stopped_at(
        crate::container_file::fault::ContainerFault::DirectoryBlock,
        0,
    );
}

#[test]
fn a_settle_stopped_between_directory_blocks_finishes_on_the_next_settle() {
    settle_stopped_at(
        crate::container_file::fault::ContainerFault::DirectoryBlock,
        1,
    );
}

#[test]
fn a_settle_stopped_before_the_last_directory_block_finishes_on_the_next_settle() {
    settle_stopped_at(
        crate::container_file::fault::ContainerFault::DirectoryBlock,
        2,
    );
}

#[test]
fn a_settle_stopped_before_the_access_slot_finishes_on_the_next_settle() {
    settle_stopped_at(crate::container_file::fault::ContainerFault::AccessSlot, 0);
}

#[test]
fn settle_finishes_a_stopped_rotation_and_records_a_signed_version() {
    let database = Protected::create("vdbx-protected-settle");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    assert_eq!(rotation_pending(&database), Some(REVOCATION_VERSION));
    database.reopen();
    assert_eq!(
        database.writer().settle_access().unwrap(),
        Some(REVOCATION_VERSION)
    );
    assert_eq!(rotation_pending(&database), None);
    assert_eq!(last_audit_kind(&database), "settle");
    database.reopen();
    let audit = database.root().database_access().flatten().unwrap().audit();
    assert_eq!(audit.last().unwrap().signer, database.admin.id().as_str());
    assert_eq!(audit.last().unwrap().version, REVOCATION_VERSION + 1);
}

#[test]
fn a_second_settle_finds_nothing_pending_and_writes_nothing() {
    let database = Protected::create("vdbx-protected-settle-again");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    database.writer().settle_access().unwrap();
    database.reopen();
    let before = std::fs::read(database.path()).unwrap();
    assert_eq!(database.writer().settle_access().unwrap(), None);
    database.reopen();
    assert_eq!(std::fs::read(database.path()).unwrap(), before);
}

#[test]
fn only_an_admin_settles() {
    let database = Protected::create("vdbx-protected-settle-admin");
    write_ledger(&database);
    let writer_identity = database.identity("writer");
    database.grant(
        "writer",
        &writer_identity,
        &[AccessAction::Write],
        &["collection:*"],
    );
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&writer_identity)).unwrap();
    assert_eq!(
        writer.settle_access().unwrap_err().code(),
        NativeStorageErrorCode::PermissionDenied
    );
    drop(writer);
    assert_eq!(rotation_pending(&database), Some(REVOCATION_VERSION + 1));
}

#[test]
fn a_revoked_reader_reads_old_key_blocks_until_the_rotation_is_settled() {
    let database = Protected::create("vdbx-protected-settle-revoked");
    write_ledger(&database);
    let reader = database.identity("reader");
    let kept = revoke_reader_unsettled(&database, &reader);
    assert_eq!(
        names_with_kept_slots(&database, &kept, &reader),
        vec![LEDGER.to_owned()]
    );
    database.reopen();
    database.writer().settle_access().unwrap();
    assert!(names_with_kept_slots(&database, &kept, &reader).is_empty());
}

#[test]
fn an_admin_write_settles_a_pending_rotation() {
    let database = Protected::create("vdbx-protected-settle-write");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    put(&database.writer(), LEDGER, SECOND);
    assert_eq!(rotation_pending(&database), None);
    assert_eq!(last_audit_kind(&database), "settle");
}

#[test]
fn another_identity_s_write_leaves_a_pending_rotation() {
    let database = Protected::create("vdbx-protected-settle-writer");
    write_ledger(&database);
    let writer_identity = database.identity("writer");
    database.grant(
        "writer",
        &writer_identity,
        &[AccessAction::Write],
        &["collection:*"],
    );
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    let writer = NativeWriteRoot::open_profiled(database.as_identity(&writer_identity)).unwrap();
    put(&writer, LEDGER, SECOND);
    drop(writer);
    assert_eq!(rotation_pending(&database), Some(REVOCATION_VERSION + 1));
}

fn locked_file(database: &Protected) -> crate::ContainerFile {
    database.reopen();
    crate::ContainerFile::open_reader(&database.path()).unwrap()
}

fn unlocked_file(database: &Protected) -> crate::ContainerFile {
    let mut file = locked_file(database);
    let identity = DatabaseIdentity::new(
        Identity::open(database.admin.reference()).unwrap(),
        AccessTrust::Pinned(database.admin.id().clone()),
    );
    file.unlock_with_identity(&identity).unwrap();
    file
}

/// The codes of what verify finds, which must leave the file `ok`.
fn problem_codes(file: &crate::ContainerFile) -> Vec<&'static str> {
    let report = file.verify().unwrap();
    assert!(report.ok, "{:?}", report.problems);
    report.problems.iter().map(|problem| problem.code).collect()
}

#[test]
fn verify_warns_of_a_pending_rotation_with_the_identity() {
    let database = Protected::create("vdbx-protected-settle-verify");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    assert_eq!(
        problem_codes(&unlocked_file(&database)),
        vec!["catalogRotationPending"]
    );
}

#[test]
fn verify_warns_of_a_pending_rotation_without_an_identity() {
    let database = Protected::create("vdbx-protected-settle-verify-locked");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    assert_eq!(
        problem_codes(&locked_file(&database)),
        vec!["catalogRotationPending"]
    );
}

#[test]
fn verify_is_quiet_once_the_rotation_is_settled() {
    let database = Protected::create("vdbx-protected-settle-verify-done");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    database.writer().settle_access().unwrap();
    assert!(problem_codes(&unlocked_file(&database)).is_empty());
}

#[test]
fn the_access_report_names_a_pending_rotation_and_the_command() {
    let database = Protected::create("vdbx-protected-settle-report");
    write_ledger(&database);
    let reader = database.identity("reader");
    revoke_reader_unsettled(&database, &reader);
    let report = crate::RootAccessReport::of(&database.root()).unwrap();
    let notice = report.catalog_rotation.unwrap();
    assert_eq!(notice.pending_since, REVOCATION_VERSION);
    assert!(notice.message.contains("since policy version 3"));
    assert_eq!(
        notice.command,
        format!("fylo access settle {}", database.path().display())
    );
}
