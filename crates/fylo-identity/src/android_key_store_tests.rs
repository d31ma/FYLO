//! `android-keystore:` against a software stand-in for the Keystore: the
//! store makes and opens keys, and hands back what the Keystore signs and
//! agrees in the forms FYLO uses (64-byte signatures, 32-byte secrets).

use std::collections::BTreeMap;
use std::sync::Mutex;

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::Generate as _;
use p256::elliptic_curve::sec1::ToSec1Point as _;
use p256::{PublicKey, SecretKey};
use zeroize::Zeroizing;

use super::AndroidKeyStore;
use crate::android_keystore::AndroidKeystore;
use crate::android_keystore_host::AndroidKeystoreHost;
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;

#[derive(Default)]
struct SoftwareKeystore {
    keys: Mutex<BTreeMap<String, SecretKey>>,
}

impl SoftwareKeystore {
    fn key(&self, alias: &str) -> Result<SecretKey, IdentityError> {
        self.keys
            .lock()
            .unwrap()
            .get(alias)
            .cloned()
            .ok_or_else(|| IdentityError::NotFound(alias.to_owned()))
    }
}

fn point(public_key: &PublicKey) -> Vec<u8> {
    public_key.to_sec1_point(false).as_bytes().to_vec()
}

fn shared_x(secret: &SecretKey, peer: &PublicKey) -> Vec<u8> {
    let shared = (peer.to_projective() * *secret.to_nonzero_scalar()).to_affine();
    point(&PublicKey::from_affine(shared).unwrap())[1..33].to_vec()
}

impl AndroidKeystore for SoftwareKeystore {
    fn create(&self, alias: &str) -> Result<Vec<u8>, IdentityError> {
        let mut keys = self.keys.lock().unwrap();
        if keys.contains_key(alias) {
            return Err(IdentityError::AlreadyExists(alias.to_owned()));
        }
        let secret = SecretKey::try_generate().unwrap();
        let public = point(&secret.public_key());
        keys.insert(alias.to_owned(), secret);
        Ok(public)
    }

    fn public_key(&self, alias: &str) -> Result<Vec<u8>, IdentityError> {
        Ok(point(&self.key(alias)?.public_key()))
    }

    fn sign(&self, alias: &str, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let signature: Signature = SigningKey::from(&self.key(alias)?).sign(message);
        Ok(signature.to_der().as_bytes().to_vec())
    }

    fn agree(&self, alias: &str, peer: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let peer = PublicKey::from_sec1_bytes(peer).unwrap();
        Ok(Zeroizing::new(shared_x(&self.key(alias)?, &peer)))
    }
}

fn store(alias: &str) -> AndroidKeyStore {
    AndroidKeystoreHost::install(Box::new(SoftwareKeystore::default()));
    AndroidKeyStore::new(alias.to_owned())
}

#[test]
fn a_created_key_is_opened_again_by_its_alias() {
    let created = store("device-open").create().unwrap();
    let opened = store("device-open").open().unwrap();
    assert_eq!(created.public_key(), opened.public_key());
}

#[test]
fn signatures_come_back_as_64_bytes_that_verify() {
    let key = store("device-sign").create().unwrap();
    let signature = key.sign(b"a record").unwrap();
    let verifying = VerifyingKey::from(&key.public_key());
    let signature = Signature::from_slice(&signature).unwrap();
    assert!(verifying.verify(b"a record", &signature).is_ok());
}

#[test]
fn agreement_matches_what_the_peer_computes() {
    let key = store("device-agree").create().unwrap();
    let peer = SecretKey::try_generate().unwrap();
    let shared = key.agree(&peer.public_key()).unwrap();
    assert_eq!(shared.to_vec(), shared_x(&peer, &key.public_key()));
}

#[test]
fn a_missing_alias_is_not_found_and_an_existing_one_is_not_replaced() {
    assert!(matches!(
        store("device-missing").open().err(),
        Some(IdentityError::NotFound(_))
    ));
    store("device-twice").create().unwrap();
    assert!(matches!(
        store("device-twice").create().err(),
        Some(IdentityError::AlreadyExists(_))
    ));
}
