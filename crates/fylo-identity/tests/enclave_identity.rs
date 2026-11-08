//! `enclave:` keys on this Mac's Secure Enclave (ADR 0021, phase 8). Each
//! test keeps its keys' token blobs in its own throwaway keychain, named by
//! `FYLO_KEYCHAIN` and deleted afterwards; the tests take turns, since the
//! variable is the process's.
#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use fylo_identity::{Identity, KeyReference};

const SECURITY: &str = "/usr/bin/security";

struct Keychain {
    directory: PathBuf,
    path: PathBuf,
    _turn: MutexGuard<'static, ()>,
}

impl Keychain {
    fn new(name: &str) -> Keychain {
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let directory =
            std::env::temp_dir().join(format!("fylo-enclave-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("test.keychain-db");
        let password = format!("fylo-test-{}", std::process::id());
        security(&["create-keychain", "-p", &password, path.to_str().unwrap()]);
        security(&["set-keychain-settings", path.to_str().unwrap()]);
        security(&["unlock-keychain", "-p", &password, path.to_str().unwrap()]);
        // SAFETY: the tests take turns under `TURN`, so no other thread of
        // this binary reads or writes the environment meanwhile.
        unsafe { std::env::set_var("FYLO_KEYCHAIN", &path) };
        Keychain {
            directory,
            path,
            _turn: turn,
        }
    }

    fn reference(&self, name: &str) -> KeyReference {
        assert!(self.path.exists());
        KeyReference::Enclave(format!("com.delma.fylo-test.{name}.{}", std::process::id()))
    }
}

impl Drop for Keychain {
    fn drop(&mut self) {
        let _ = Command::new(SECURITY)
            .args(["delete-keychain", self.path.to_str().unwrap()])
            .output();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn security(arguments: &[&str]) {
    let output = Command::new(SECURITY).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "security {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_enclave_key_signs_and_its_public_half_verifies() {
    let keychain = Keychain::new("sign");
    let identity = Identity::create(&keychain.reference("sign")).unwrap();
    assert!(identity.is_hardware());
    let signature = identity.try_sign(b"policy version 1").unwrap();
    assert_eq!(signature.len(), 64);
    identity
        .public()
        .verify(b"policy version 1", &signature)
        .unwrap();
}

#[test]
fn an_enclave_key_unwraps_a_key_wrapped_for_it() {
    let keychain = Keychain::new("wrap");
    let identity = Identity::create(&keychain.reference("wrap")).unwrap();
    let wrapped = identity.public().wrap(b"catalog", &[7; 32]).unwrap();
    let opened = identity.unwrap_key(b"catalog", &wrapped).unwrap();
    assert_eq!(opened.as_slice(), &[7; 32]);
}

#[test]
fn an_enclave_key_refuses_a_wrap_for_another_purpose() {
    let keychain = Keychain::new("wrap-purpose");
    let identity = Identity::create(&keychain.reference("wrap-purpose")).unwrap();
    let wrapped = identity.public().wrap(b"catalog", &[7; 32]).unwrap();
    let error = identity.unwrap_key(b"collection", &wrapped).unwrap_err();
    assert_eq!(error.code(), "EIDENTITY_UNWRAP");
}

#[test]
fn opening_it_again_gives_the_same_key() {
    let keychain = Keychain::new("reopen");
    let reference = keychain.reference("reopen");
    let created = Identity::create(&reference).unwrap();
    let opened = Identity::open(&reference).unwrap();
    assert_eq!(opened.id(), created.id());
    let signature = opened.try_sign(b"record").unwrap();
    created.public().verify(b"record", &signature).unwrap();
}

#[test]
fn create_never_replaces_an_enclave_key() {
    let keychain = Keychain::new("exists");
    let reference = keychain.reference("exists");
    let created = Identity::create(&reference).unwrap();
    let error = Identity::create(&reference).unwrap_err();
    assert_eq!(error.code(), "EIDENTITY_EXISTS");
    assert_eq!(Identity::open(&reference).unwrap().id(), created.id());
}

#[test]
fn a_missing_enclave_key_is_not_found() {
    let keychain = Keychain::new("missing");
    let error = Identity::open(&keychain.reference("missing")).unwrap_err();
    assert_eq!(error.code(), "EIDENTITY_NOT_FOUND");
}

#[test]
fn the_blob_lives_in_the_throwaway_keychain_under_its_own_account() {
    let keychain = Keychain::new("account");
    let reference = keychain.reference("account");
    Identity::create(&reference).unwrap();
    let KeyReference::Enclave(label) = &reference else {
        unreachable!()
    };
    let output = Command::new(SECURITY)
        .args([
            "find-generic-password",
            "-s",
            label,
            "-a",
            "fylo-enclave-identity",
            keychain.path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
}
