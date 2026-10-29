use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::{Component, Path, PathBuf};

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, KeyInit},
};
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac_array;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::EngineError;
use crate::schema::SchemaRegistry;

const MAX_SCHEMA_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SCHEMA_BYTES: u64 = 16 * 1024 * 1024;
const PBKDF2_ITERATIONS: u32 = 100_000;
type SecretKey = Zeroizing<[u8; 32]>;

/// Field encryption, with each collection's `$encrypted` lists read from its
/// schema registry when it has one, else from the `fylo.schema` folder.
///
/// Every engine has one, because a registry collection needs its lists even
/// where no `fylo.schema` is configured. With neither a registry nor a folder
/// a collection declares nothing and stored ciphertext fails closed.
#[derive(Clone)]
pub(crate) struct EncryptionReader {
    schema_root: Option<PathBuf>,
    registry: SchemaRegistry,
    key: Option<SecretKey>,
    hmac_key: Option<SecretKey>,
}

pub(crate) fn reject_undeclared_ciphertext(
    collection: &str,
    fields: &Map<String, Value>,
) -> Result<(), String> {
    if contains_ciphertext(&Value::Object(fields.clone())) {
        Err(format!(
            "cannot decrypt collection {collection}: ciphertext exists but no schema root was configured"
        ))
    } else {
        Ok(())
    }
}

impl EncryptionReader {
    /// No folder and no keys: only registry collections declare fields.
    pub(crate) fn unconfigured(registry: SchemaRegistry) -> Self {
        Self {
            schema_root: None,
            registry,
            key: None,
            hmac_key: None,
        }
    }

    pub(crate) fn open(
        schema_root: impl AsRef<Path>,
        credentials: Option<(&str, &str)>,
        registry: SchemaRegistry,
    ) -> Result<Self, String> {
        let schema_root = fs::canonicalize(schema_root.as_ref())
            .map_err(|error| format!("cannot open FYLO schema root: {error}"))?;
        if !fs::metadata(&schema_root)
            .map_err(|error| format!("cannot inspect FYLO schema root: {error}"))?
            .is_dir()
        {
            return Err("FYLO schema root is not a directory".into());
        }
        Self::unconfigured(registry).with_credentials(credentials, Some(schema_root))
    }

    /// This reader with `credentials`' keys, keeping its folder unless
    /// `schema_root` names one.
    pub(crate) fn with_credentials(
        mut self,
        credentials: Option<(&str, &str)>,
        schema_root: Option<PathBuf>,
    ) -> Result<Self, String> {
        let keys = credentials
            .map(|(secret, salt)| derive_keys(secret, salt))
            .transpose()?;
        let (key, hmac_key) =
            keys.map_or((None, None), |(key, hmac_key)| (Some(key), Some(hmac_key)));
        self.key = key;
        self.hmac_key = hmac_key;
        if schema_root.is_some() {
            self.schema_root = schema_root;
        }
        Ok(self)
    }

    /// Decrypt with the `$encrypted` list of the version the document was
    /// written at, its `_v`, so that a later version that renames or drops an
    /// encrypted field does not leave old documents unreadable. A document
    /// without `_v`, or at a version the manifest does not list, is decrypted
    /// with the head's list, as it was written outside strict mode.
    pub(crate) fn decode_document(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let version = fields.get("_v").and_then(Value::as_str).map(str::to_owned);
        self.decode_document_at(collection, fields, version.as_deref())
    }

    /// [`Self::decode_document`] for a document taken to be at `version`.
    pub(crate) fn decode_document_at(
        &self,
        collection: &str,
        mut fields: Map<String, Value>,
        version: Option<&str>,
    ) -> Result<Map<String, Value>, String> {
        let Some(encrypted) = self.declared_fields_at(collection, version)? else {
            reject_undeclared_ciphertext(collection, &fields)?;
            return Ok(fields);
        };
        if encrypted.is_empty() {
            if fields.values().any(contains_ciphertext) {
                return Err(format!(
                    "cannot decrypt collection {collection}: ciphertext exists but its schema does not declare encrypted fields"
                ));
            }
            return Ok(fields);
        }
        let key = self.key.as_ref().ok_or_else(|| {
            format!(
                "cannot decrypt collection {collection}: fylo.encryption.key and fylo.cipher.salt are required"
            )
        })?;
        for (field, value) in &mut fields {
            decode_value(value, field, &encrypted, key).map_err(|reason| {
                format!("cannot decrypt encrypted field in collection {collection}: {reason}")
            })?;
        }
        Ok(fields)
    }

    /// Encrypt every schema-declared field, matching the JavaScript
    /// `encodeEncrypted` traversal and `v2.` envelope byte for byte.
    pub(crate) fn encode_document(
        &self,
        collection: &str,
        mut fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let Some(encrypted) = self.declared_fields_at(collection, None)? else {
            reject_undeclared_ciphertext(collection, &fields)?;
            return Ok(fields);
        };
        if encrypted.is_empty() {
            return Ok(fields);
        }
        let key = self.key.as_ref().ok_or_else(|| {
            format!(
                "cannot encrypt collection {collection}: fylo.encryption.key and fylo.cipher.salt are required"
            )
        })?;
        for (field, value) in &mut fields {
            let path = field.clone();
            encode_value(value, &path, &encrypted, key).map_err(|reason| {
                format!("cannot encrypt declared field in collection {collection}: {reason}")
            })?;
        }
        Ok(fields)
    }

    /// The oldest version in the manifest, which a document without `_v` is
    /// taken to be at by schema materialization.
    pub(crate) fn oldest_version(&self, collection: &str) -> Result<Option<String>, String> {
        if let Some(first) = self.registry.versions(collection)?.first() {
            return Ok(Some(first.label().to_owned()));
        }
        Ok(self
            .manifest(collection)?
            .and_then(|manifest| manifest.versions.into_iter().next())
            .map(|entry| entry.v))
    }

    fn manifest(&self, collection: &str) -> Result<Option<SchemaManifest>, String> {
        let Some(root) = self.schema_root.as_deref() else {
            return Ok(None);
        };
        let manifest_path = PathBuf::from(collection).join("manifest.json");
        if !Self::path_exists(root, &manifest_path)? {
            return Ok(None);
        }
        let manifest: SchemaManifest = serde_json::from_slice(&Self::read_file(
            root,
            &manifest_path,
            MAX_SCHEMA_MANIFEST_BYTES,
        )?)
        .map_err(|error| format!("schema manifest is corrupt: {error}"))?;
        validate_version(&manifest.current)?;
        Ok(Some(manifest))
    }

    fn version_schema(root: &Path, collection: &str, version: &str) -> Result<Value, String> {
        validate_version(version)?;
        let schema_path = PathBuf::from(collection)
            .join("history")
            .join(format!("{version}.schema.json"));
        serde_json::from_slice(&Self::read_file(root, &schema_path, MAX_SCHEMA_BYTES)?)
            .map_err(|error| format!("schema {version} is corrupt: {error}"))
    }

    pub(crate) fn encrypted_fields(&self, collection: &str) -> Result<Vec<String>, String> {
        Ok(self
            .declared_fields_at(collection, None)?
            .unwrap_or_default())
    }

    /// The `$encrypted` list of `version`, or of the head when `version` is
    /// `None` or not a version the collection has. `None` when the collection
    /// has neither a registry, a configured folder, nor an encrypted-fields
    /// record to declare one.
    ///
    /// A collection without a registry also honours its encrypted-fields
    /// record, which keyed writers with the folder keep (see
    /// [`Self::record_folder_declaration`]): a writer without the folder
    /// encrypts what the record lists, or is refused without the key, rather
    /// than store a declared field in plaintext. The head's list is the
    /// folder's and the record's together; a document at a version the
    /// folder lists decodes with that version's list, as before.
    fn declared_fields_at(
        &self,
        collection: &str,
        version: Option<&str>,
    ) -> Result<Option<Vec<String>>, String> {
        if let Some(head) = self.registry.head(collection)? {
            let at = match version {
                Some(label) => self.registry.at_label(collection, label)?,
                None => None,
            };
            return Ok(Some(at.unwrap_or(head).encrypted.clone()));
        }
        let recorded = self.registry.encrypted_record(collection)?;
        let Some(root) = self.schema_root.as_deref() else {
            return Ok(recorded);
        };
        let Some(manifest) = self.manifest(collection)? else {
            return Ok(Some(recorded.unwrap_or_default()));
        };
        let listed = version.filter(|version| {
            *version != manifest.current
                && manifest.versions.iter().any(|entry| entry.v == *version)
        });
        let schema = Self::version_schema(root, collection, listed.unwrap_or(&manifest.current))?;
        let mut fields = encrypted_list(collection, &schema)?;
        if listed.is_none() {
            for field in recorded.unwrap_or_default() {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
        }
        Ok(Some(fields))
    }

    /// Keep `collection`'s encrypted-fields record equal to the head
    /// `$encrypted` list its `fylo.schema` folder declares, so writers
    /// without the folder honour it. Only a keyed writer whose folder
    /// declares the collection records anything: one without the key could
    /// not have written the fields it lists, and a collection with a schema
    /// registry keeps its lists in the database already.
    pub(crate) fn record_folder_declaration(&self, collection: &str) -> Result<(), EngineError> {
        if self.key.is_none()
            || self
                .registry
                .head(collection)
                .map_err(EngineError::schema)?
                .is_some()
        {
            return Ok(());
        }
        let Some(root) = self.schema_root.as_deref() else {
            return Ok(());
        };
        let Some(manifest) = self.manifest(collection).map_err(EngineError::schema)? else {
            return Ok(());
        };
        let fields = Self::version_schema(root, collection, &manifest.current)
            .and_then(|schema| encrypted_list(collection, &schema))
            .map_err(EngineError::schema)?;
        self.registry
            .write_encrypted_record(collection, &fields)
            .map(|_| ())
            .map_err(EngineError::storage)
    }

    pub(crate) fn blind_index(&self, value: &str) -> Result<String, String> {
        let key = self.hmac_key.as_ref().ok_or_else(|| {
            "fylo.encryption.key and fylo.cipher.salt are required for blind indexes".to_owned()
        })?;
        let mut hmac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key.as_slice())
            .map_err(|_| "invalid HMAC-SHA256 key".to_owned())?;
        hmac.update(value.as_bytes());
        Ok(format!(
            "idx1.{}",
            encode_base64_url(&hmac.finalize().into_bytes())
        ))
    }

    fn path_exists(root: &Path, relative: &Path) -> Result<bool, String> {
        let path = root.join(relative);
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("cannot inspect schema path: {error}")),
        }
    }

    fn read_file(root: &Path, relative: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
        let mut current = root.to_owned();
        for component in relative.components() {
            let Component::Normal(segment) = component else {
                return Err("schema path contains an unsafe component".into());
            };
            current.push(segment);
            let metadata = fs::symlink_metadata(&current)
                .map_err(|error| format!("cannot inspect schema path: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err("schema path contains a symbolic link or reparse point".into());
            }
        }
        let canonical = fs::canonicalize(&current)
            .map_err(|error| format!("cannot canonicalize schema path: {error}"))?;
        if !canonical.starts_with(root) {
            return Err("schema path escapes the canonical schema root".into());
        }
        let file =
            File::open(&canonical).map_err(|error| format!("cannot open schema file: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect schema file: {error}"))?;
        if !metadata.is_file() {
            return Err("schema path is not a regular file".into());
        }
        if metadata.len() > max_bytes {
            return Err(format!("schema file exceeds {max_bytes} bytes"));
        }
        read_bounded(file.take(max_bytes.saturating_add(1)), max_bytes)
    }
}

/// A schema's `$encrypted` list, each entry a checked slash-separated path.
pub(crate) fn encrypted_list(collection: &str, schema: &Value) -> Result<Vec<String>, String> {
    let Some(encrypted) = schema.get("$encrypted") else {
        return Ok(Vec::new());
    };
    let encrypted = encrypted.as_array().ok_or_else(|| {
        format!("schema $encrypted for {collection} must be an array of field names")
    })?;
    encrypted
        .iter()
        .map(|field| {
            let field = field.as_str().ok_or_else(|| {
                format!("schema $encrypted for {collection} must contain only strings")
            })?;
            validate_field_path(field)?;
            Ok(field.to_owned())
        })
        .collect()
}

#[derive(Deserialize)]
struct SchemaManifest {
    current: String,
    #[serde(default)]
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct ManifestVersion {
    v: String,
}

/// Keys derived so far in this process, by a SHA-256 of the secret and salt.
///
/// PBKDF2 at 100,000 iterations costs about 28 ms in a release build, and
/// every machine request that touches an encrypting collection opens a fresh
/// engine. The derived keys are the same every time, so a process derives
/// them once. The secret itself is not kept.
static DERIVED_KEYS: std::sync::Mutex<Vec<([u8; 32], SecretKey, SecretKey)>> =
    std::sync::Mutex::new(Vec::new());

fn derive_keys(secret: &str, salt: &str) -> Result<(SecretKey, SecretKey), String> {
    let fingerprint: [u8; 32] = {
        use sha2::Digest as _;
        let mut digest = Sha256::new();
        digest.update((secret.len() as u64).to_le_bytes());
        digest.update(secret.as_bytes());
        digest.update(salt.as_bytes());
        digest.finalize().into()
    };
    let mut cache = DERIVED_KEYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, key, hmac_key)) = cache.iter().find(|(seen, _, _)| *seen == fingerprint) {
        return Ok((key.clone(), hmac_key.clone()));
    }
    let (key, hmac_key) = derive_keys_uncached(secret, salt)?;
    cache.push((fingerprint, key.clone(), hmac_key.clone()));
    Ok((key, hmac_key))
}

fn derive_keys_uncached(secret: &str, salt: &str) -> Result<(SecretKey, SecretKey), String> {
    if secret.len() < 32 {
        return Err("fylo.encryption.key must be at least 32 UTF-8 bytes long".into());
    }
    if salt.is_empty() {
        return Err("fylo.cipher.salt must not be empty".into());
    }
    let material = Zeroizing::new(pbkdf2_hmac_array::<Sha256, 64>(
        secret.as_bytes(),
        salt.as_bytes(),
        PBKDF2_ITERATIONS,
    ));
    let mut key = Zeroizing::new([0_u8; 32]);
    key.copy_from_slice(&material[..32]);
    let mut hmac_key = Zeroizing::new([0_u8; 32]);
    hmac_key.copy_from_slice(&material[32..]);
    Ok((key, hmac_key))
}

fn decode_value(
    value: &mut Value,
    field: &str,
    encrypted: &[String],
    key: &[u8; 32],
) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            for (child, value) in fields {
                decode_value(value, &format!("{field}/{child}"), encrypted, key)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                decode_value(value, field, encrypted, key)?;
            }
        }
        Value::String(ciphertext) if is_encrypted_field(field, encrypted) => {
            *value = decode_stored_value(&decrypt(ciphertext, key)?);
        }
        _ => {}
    }
    Ok(())
}

/// JavaScript drops the parent path when it recurses into an array element, so
/// an object nested inside an array is never encrypted. The Rust writer must
/// reproduce that exactly or the JavaScript reader would see ciphertext where
/// its own writer produced plaintext.
fn encode_value(
    value: &mut Value,
    field: &str,
    encrypted: &[String],
    key: &[u8; 32],
) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            for (child, value) in fields {
                let path = child_path(field, child);
                encode_value(value, &path, encrypted, key)?;
            }
        }
        Value::Array(values) => {
            for item in values {
                if item.is_object() || item.is_array() {
                    encode_value(item, "", encrypted, key)?;
                } else if is_encrypted_field(field, encrypted) {
                    *item = Value::String(encrypt(&stringify_stored_value(item), key)?);
                }
            }
        }
        _ if is_encrypted_field(field, encrypted) => {
            *value = Value::String(encrypt(&stringify_stored_value(value), key)?);
        }
        _ => {}
    }
    Ok(())
}

fn child_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn stringify_stored_value(value: &Value) -> String {
    let rendered = match value {
        // A string whose text is also JSON (`"999"`, `"true"`, `"null"`) is
        // stored quoted, or `decode_stored_value` would parse it back as a
        // number, boolean or null. Every other string keeps the JavaScript
        // engine's unquoted form, and both readers parse a quoted one back to
        // the same string.
        Value::String(text) if serde_json::from_str::<Value>(text).is_ok() => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    };
    rendered.replace('/', "%2F")
}

fn encrypt(plaintext: &str, key: &[u8; 32]) -> Result<String, String> {
    let mut nonce = Zeroizing::new([0_u8; 12]);
    getrandom::fill(nonce.as_mut_slice())
        .map_err(|error| format!("cannot generate a ciphertext nonce: {error}"))?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "invalid AES-256 key".to_owned())?;
    let sealed: &aes_gcm::aead::Nonce<Aes256Gcm> = nonce
        .as_slice()
        .try_into()
        .map_err(|_| "ciphertext nonce has an invalid length".to_owned())?;
    let ciphertext = cipher
        .encrypt(sealed, plaintext.as_bytes())
        .map_err(|_| "configured key could not seal the value".to_owned())?;
    let mut combined = Vec::with_capacity(nonce.len() + ciphertext.len());
    combined.extend_from_slice(nonce.as_slice());
    combined.extend_from_slice(&ciphertext);
    Ok(format!("v2.{}", encode_base64_url(&combined)))
}

pub(crate) fn is_encrypted_field(field: &str, encrypted: &[String]) -> bool {
    encrypted
        .iter()
        .any(|pattern| field == pattern || field.starts_with(&format!("{pattern}/")))
}

pub(crate) fn encode_base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(ALPHABET[usize::from(first >> 2)]));
        output.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            output.push(char::from(
                ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))],
            ));
        }
        if chunk.len() > 2 {
            output.push(char::from(ALPHABET[usize::from(third & 0x3f)]));
        }
    }
    output
}

fn decrypt(encoded: &str, key: &[u8; 32]) -> Result<String, String> {
    let payload = encoded
        .strip_prefix("v2.")
        .ok_or_else(|| "unsupported ciphertext format; expected v2".to_owned())?;
    let combined = decode_base64_url(payload)?;
    if combined.len() < 12 + 16 {
        return Err("ciphertext envelope is truncated".into());
    }
    let (nonce, ciphertext) = combined.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "invalid AES-256 key".to_owned())?;
    let nonce: &aes_gcm::aead::Nonce<Aes256Gcm> = nonce
        .try_into()
        .map_err(|_| "ciphertext nonce has an invalid length".to_owned())?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "configured key could not authenticate ciphertext".to_owned())?;
    String::from_utf8(plaintext).map_err(|_| "decrypted value is not valid UTF-8".into())
}

fn decode_stored_value(plaintext: &str) -> Value {
    let unescaped = plaintext.replace("%2F", "/");
    match serde_json::from_str(&unescaped) {
        Ok(value) => value,
        Err(_) => Value::String(unescaped),
    }
}

fn contains_ciphertext(value: &Value) -> bool {
    match value {
        Value::String(value) => value.starts_with("v2."),
        Value::Array(values) => values.iter().any(contains_ciphertext),
        Value::Object(fields) => fields.values().any(contains_ciphertext),
        _ => false,
    }
}

pub(crate) fn decode_base64_url(encoded: &str) -> Result<Vec<u8>, String> {
    let mut source = encoded.as_bytes().to_vec();
    for byte in &mut source {
        match *byte {
            b'-' => *byte = b'+',
            b'_' => *byte = b'/',
            _ => {}
        }
    }
    let padding = (4 - source.len() % 4) % 4;
    source.extend(std::iter::repeat_n(b'=', padding));
    decode_base64(&source)
}

fn decode_base64(encoded: &[u8]) -> Result<Vec<u8>, String> {
    if !encoded.len().is_multiple_of(4) {
        return Err("ciphertext contains invalid base64url".into());
    }
    let quartets = encoded.chunks_exact(4);
    let count = quartets.len();
    let mut output = Vec::with_capacity(count * 3);
    for (index, quartet) in quartets.enumerate() {
        let last = index + 1 == count;
        if (!last && quartet.contains(&b'='))
            || quartet[0] == b'='
            || quartet[1] == b'='
            || (quartet[2] == b'=' && quartet[3] != b'=')
        {
            return Err("ciphertext contains invalid base64url padding".into());
        }
        let a = base64_value(quartet[0])?;
        let b = base64_value(quartet[1])?;
        let c = if quartet[2] == b'=' {
            0
        } else {
            base64_value(quartet[2])?
        };
        let d = if quartet[3] == b'=' {
            0
        } else {
            base64_value(quartet[3])?
        };
        output.push((a << 2) | (b >> 4));
        if quartet[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if quartet[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err("ciphertext contains invalid base64url".into()),
    }
}

pub(crate) fn validate_version(version: &str) -> Result<(), String> {
    let valid = !version.is_empty()
        && version.len() <= 128
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    if valid {
        Ok(())
    } else {
        Err("schema manifest current version is invalid".into())
    }
}

fn validate_field_path(field: &str) -> Result<(), String> {
    let valid = !field.is_empty()
        && field.len() <= 1024
        && field
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if valid {
        Ok(())
    } else {
        Err("schema contains an invalid encrypted field path".into())
    }
}

fn read_bounded<R: Read>(mut reader: Take<R>, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read schema file: {error}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("schema file exceeds {max_bytes} bytes"));
    }
    Ok(bytes)
}
