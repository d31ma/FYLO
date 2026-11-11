//! A file key a host derived itself (ADR 0025): the HKDF-SHA256 output a
//! browser's `deriveBits` gives for a file's salt opens the file a secret
//! made, a file made with it opens with the secret, and a key derived for
//! another salt is refused. Every key is generated here.

use std::fs;
use std::path::{Path, PathBuf};

use fylo_storage_native::{
    CollectionKind, DatabaseKey, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, RootOpenOptions,
};
use hkdf::Hkdf;
use sha2::Sha256;

const FILE_KEY_INFO: &[u8] = b"fylo.vdbx.file-key.v1";
const CHECK_INFO: &[u8] = b"fylo.vdbx.key-check.v1";
const SALT_RANGE: std::ops::Range<usize> = 80..112;
const DOCUMENT: &str = "4VRNF580001";
const TEXT: &str = "{\"note\":\"derived\"}";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-derived-key-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn database(&self) -> PathBuf {
        self.0.join("db.vdbx")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn secret() -> Vec<u8> {
    format!(
        "generated-derived-key-test-{}-secret-bytes",
        std::process::id()
    )
    .into_bytes()
}

fn derived(secret: &[u8], salt: [u8; 32]) -> DatabaseKey {
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), secret);
    let mut file_key = [0_u8; 32];
    let mut check = [0_u8; 16];
    hkdf.expand(FILE_KEY_INFO, &mut file_key).unwrap();
    hkdf.expand(CHECK_INFO, &mut check).unwrap();
    DatabaseKey::derived(salt, file_key, check)
}

fn salt_of(path: &Path) -> [u8; 32] {
    let bytes = fs::read(path).unwrap();
    bytes[SALT_RANGE].try_into().unwrap()
}

fn options(path: &Path, key: &DatabaseKey) -> RootOpenOptions {
    RootOpenOptions::new(path).with_database_key(key.clone())
}

fn write(path: &Path, key: &DatabaseKey) {
    let writer = NativeWriteRoot::open_profiled(options(path, key)).unwrap();
    writer
        .create_collection("notes", CollectionKind::Document, None)
        .unwrap();
    writer
        .put_document(
            "notes",
            DOCUMENT,
            TEXT.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn read(path: &Path, key: &DatabaseKey) -> Vec<u8> {
    NativeRoot::close_database(path).unwrap();
    let root = NativeRoot::open_profiled(options(path, key)).unwrap();
    root.collection("notes")
        .unwrap()
        .read_document(DOCUMENT)
        .unwrap()
        .bytes
}

#[test]
fn a_derived_key_opens_a_file_its_secret_made() {
    let scratch = Scratch::new("open");
    let path = scratch.database();
    let secret = DatabaseKey::new(secret());
    NativeRoot::create_encrypted_database(&path, &secret).unwrap();
    let key = derived(&self::secret(), salt_of(&path));
    write(&path, &key);
    assert_eq!(read(&path, &secret), TEXT.as_bytes());
}

#[test]
fn a_file_made_with_a_derived_key_opens_with_its_secret() {
    let scratch = Scratch::new("create");
    let path = scratch.database();
    let key = derived(&secret(), [7; 32]);
    NativeRoot::create_encrypted_database(&path, &key).unwrap();
    assert_eq!(salt_of(&path), [7; 32]);
    write(&path, &key);
    assert_eq!(read(&path, &DatabaseKey::new(secret())), TEXT.as_bytes());
}

#[test]
fn a_key_derived_for_another_salt_is_refused() {
    let scratch = Scratch::new("salt");
    let path = scratch.database();
    NativeRoot::create_encrypted_database(&path, &DatabaseKey::new(secret())).unwrap();
    let mut other = salt_of(&path);
    other[0] ^= 1;
    NativeRoot::close_database(&path).unwrap();
    let refused =
        NativeRoot::open_profiled(options(&path, &derived(&secret(), other))).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
}
