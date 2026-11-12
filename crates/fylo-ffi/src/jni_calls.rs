//! The Android entry points (ADR 0026, section 4): the same calls as the C
//! ABI, reached from Kotlin's `io.github.d31ma.fylo.FyloNative` through JNI.
//!
//! Every input is a Java `byte[]`, copied into Rust for the call. Every
//! answer is one `byte[]`: its first byte is the `FyloStatus`, the rest is
//! the same JSON the C ABI returns. `open` answers `{"root": <handle>}`.
//! The `jni` crate attaches the thread, catches panics and turns a JNI
//! failure into a Java exception; a panic inside FYLO is caught first and
//! answers `FYLO_PANIC`, as in the C ABI. No `unsafe` code is needed here:
//! only the exported names are `#[unsafe(no_mangle)]`.

#![allow(
    unsafe_code,
    reason = "the JNI entry points must keep their exported names (no_mangle)"
)]
#![allow(
    non_snake_case,
    reason = "JNI names native methods Java_<class>_<method>"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "JNI passes each argument by value; the signatures are fixed"
)]

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass};
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned};
use serde_json::json;
use zeroize::Zeroizing;

use crate::c_abi::CAbi;
use crate::call_failure::CallFailure;
use crate::call_outcome::CallOutcome;
use crate::database_key_argument::DatabaseKeyArgument;
use crate::identity_calls::IdentityCalls;
use crate::jni_keystore::JniKeystore;
use crate::root_calls::RootCalls;

type Answer<'local> = jni::errors::Result<JByteArray<'local>>;

/// A root call that takes two texts after the handle.
type TwoTextCall = fn(u64, &str, &str) -> CallOutcome;

/// The JNI functions. Kotlin declares each as a static `external fun` of
/// `FyloNative`.
pub struct JniCalls;

impl JniCalls {
    /// `FyloNative.abiVersion()`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_abiVersion<'caller>(
        _env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
    ) -> jint {
        CAbi::ABI_VERSION.cast_signed()
    }

    /// `FyloNative.install()`: called once when the library loads, so
    /// `android-keystore:` keys can reach `FyloKeystore`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_install<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
    ) {
        env.with_env(|env| JniKeystore::install(env))
            .resolve::<ThrowRuntimeExAndDefault>();
    }

    /// `FyloNative.open(path, options, key)`: `{"root": <handle>}`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_open<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        path: JByteArray<'caller>,
        options: JByteArray<'caller>,
        key: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| -> Answer<'caller> {
            let path = env.convert_byte_array(&path)?;
            let options = env.convert_byte_array(&options)?;
            let key = Zeroizing::new(env.convert_byte_array(&key)?);
            let opened = CAbi::guarded(|| Self::open(&path, &options, &key));
            Self::answer(env, Self::opened(opened))
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.close(root)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_close<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
    ) -> JByteArray<'caller> {
        env.with_env(|env| -> Answer<'caller> {
            let handle = root.cast_unsigned();
            let outcome = CAbi::root_call(handle, || Ok(RootCalls::close(handle)));
            Self::answer(env, outcome)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.createCollection(root, name)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_createCollection<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        name: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| -> Answer<'caller> {
            let name = env.convert_byte_array(&name)?;
            let handle = root.cast_unsigned();
            let outcome = CAbi::root_call(handle, || {
                Ok(RootCalls::create_collection(handle, CAbi::utf8(&name)?))
            });
            Self::answer(env, outcome)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.put(root, collection, document)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_put<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        collection: JByteArray<'caller>,
        document: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::two_texts(env, root, &collection, &document, RootCalls::put))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.get(root, collection, id)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_get<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        collection: JByteArray<'caller>,
        id: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::two_texts(env, root, &collection, &id, RootCalls::get))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.delete(root, collection, id)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_delete<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        collection: JByteArray<'caller>,
        id: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::two_texts(env, root, &collection, &id, RootCalls::delete))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.find(root, collection, query)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_find<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        collection: JByteArray<'caller>,
        query: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::two_texts(env, root, &collection, &query, RootCalls::find))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.findPage(root, collection, query)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_findPage<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        root: jlong,
        collection: JByteArray<'caller>,
        query: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::two_texts(env, root, &collection, &query, RootCalls::find_page))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.identityCreate(reference)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_identityCreate<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        reference: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::identity(env, &reference, IdentityCalls::create))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    /// `FyloNative.identityShow(reference)`.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_io_github_d31ma_fylo_FyloNative_identityShow<'caller>(
        mut env: EnvUnowned<'caller>,
        _class: JClass<'caller>,
        reference: JByteArray<'caller>,
    ) -> JByteArray<'caller> {
        env.with_env(|env| Self::identity(env, &reference, IdentityCalls::show))
            .resolve::<ThrowRuntimeExAndDefault>()
    }

    fn open(path: &[u8], options: &[u8], key: &[u8]) -> Result<u64, CallFailure> {
        let key = DatabaseKeyArgument::parse(key)?;
        RootCalls::open(CAbi::utf8(path)?, CAbi::utf8(options)?, key)
    }

    fn opened(opened: Result<Result<u64, CallFailure>, ()>) -> CallOutcome {
        match opened {
            Ok(Ok(handle)) => CallOutcome::Done(json!({ "root": handle })),
            Ok(Err(failure)) => CallOutcome::failed(failure),
            Err(()) => CallOutcome::Panicked,
        }
    }

    fn two_texts<'local>(
        env: &mut Env<'local>,
        root: jlong,
        first: &JByteArray<'_>,
        second: &JByteArray<'_>,
        call: TwoTextCall,
    ) -> Answer<'local> {
        let first = env.convert_byte_array(first)?;
        let second = env.convert_byte_array(second)?;
        let handle = root.cast_unsigned();
        let outcome = CAbi::root_call(handle, || {
            Ok(call(handle, CAbi::utf8(&first)?, CAbi::utf8(&second)?))
        });
        Self::answer(env, outcome)
    }

    fn identity<'local>(
        env: &mut Env<'local>,
        reference: &JByteArray<'_>,
        call: fn(&str) -> CallOutcome,
    ) -> Answer<'local> {
        let reference = env.convert_byte_array(reference)?;
        let outcome = CAbi::guarded_outcome(|| Ok(call(CAbi::utf8(&reference)?)));
        Self::answer(env, outcome)
    }

    /// One `byte[]`: the status, then the outcome's JSON.
    fn answer<'local>(env: &mut Env<'local>, outcome: CallOutcome) -> Answer<'local> {
        let mut bytes = vec![outcome.status() as u8];
        bytes.extend(outcome.into_bytes());
        env.byte_array_from_slice(&bytes)
    }
}
