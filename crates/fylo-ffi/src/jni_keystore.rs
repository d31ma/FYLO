//! `android-keystore:` keys (ADR 0026, section 8): FYLO's [`AndroidKeystore`]
//! answered by the Kotlin object `io.github.d31ma.fylo.FyloKeystore`, whose
//! Keystore API is Java only.
//!
//! `FyloNative.install()` saves the Java VM and a global reference to the
//! class, because a thread FYLO attaches later cannot find app classes by
//! name. Every call passes byte arrays and gets one back: a kind byte, then
//! the payload (or a UTF-8 message for a refusal).

use jni::errors::Error as JniError;
use jni::objects::{Global, JByteArray, JClass, JValue};
use jni::strings::JNIStr;
use jni::{Env, JavaVM, jni_sig, jni_str};
use zeroize::Zeroizing;

use fylo::{AndroidKeystore, AndroidKeystoreHost, IdentityError};

/// The Kotlin `FyloKeystore`, reached through the Java VM.
pub(crate) struct JniKeystore {
    vm: JavaVM,
    class: Global<JClass<'static>>,
}

impl JniKeystore {
    const OK: u8 = 0;
    const NOT_FOUND: u8 = 1;
    const ALREADY_EXISTS: u8 = 2;
    const UNSUPPORTED: u8 = 3;

    /// Find `FyloKeystore` on the caller's (an app) thread and install the
    /// Keystore for this process.
    pub(crate) fn install(env: &mut Env<'_>) -> jni::errors::Result<()> {
        let class = env.find_class(jni_str!("io/github/d31ma/fylo/FyloKeystore"))?;
        let class = env.new_global_ref(class)?;
        let vm = env.get_java_vm()?;
        AndroidKeystoreHost::install(Box::new(JniKeystore { vm, class }));
        Ok(())
    }

    /// Call one static `FyloKeystore` method that takes byte arrays and
    /// answers a byte array.
    fn call(
        &self,
        method: &JNIStr,
        arguments: &[&[u8]],
        signature: &jni::signature::MethodSignature<'_, '_>,
    ) -> Result<Vec<u8>, IdentityError> {
        let answer = self
            .vm
            .attach_current_thread(|env| -> Result<Vec<u8>, JniError> {
                self.call_attached(env, method, arguments, signature)
            })
            .map_err(|error| {
                IdentityError::Store(format!(
                    "android-keystore: the Keystore call failed: {error}"
                ))
            })?;
        Self::decode(&answer)
    }

    fn call_attached(
        &self,
        env: &mut Env<'_>,
        method: &JNIStr,
        arguments: &[&[u8]],
        signature: &jni::signature::MethodSignature<'_, '_>,
    ) -> Result<Vec<u8>, JniError> {
        let mut arrays = Vec::with_capacity(arguments.len());
        for argument in arguments {
            arrays.push(env.byte_array_from_slice(argument)?);
        }
        let values: Vec<JValue<'_>> = arrays.iter().map(JValue::from).collect();
        let answer = env
            .call_static_method(&self.class, method, signature, &values)?
            .l()?;
        let answer = JByteArray::cast_local(env, answer)?;
        env.convert_byte_array(&answer)
    }

    /// The payload of an answer, or the refusal it carries.
    fn decode(answer: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let Some((&kind, payload)) = answer.split_first() else {
            return Err(IdentityError::Store(
                "android-keystore: the Keystore gave an empty answer".to_owned(),
            ));
        };
        if kind == Self::OK {
            return Ok(payload.to_vec());
        }
        Err(Self::refusal(
            kind,
            String::from_utf8_lossy(payload).into_owned(),
        ))
    }

    fn refusal(kind: u8, message: String) -> IdentityError {
        match kind {
            Self::NOT_FOUND => IdentityError::NotFound(message),
            Self::ALREADY_EXISTS => IdentityError::AlreadyExists(message),
            Self::UNSUPPORTED => IdentityError::Unsupported(message),
            _ => IdentityError::Store(message),
        }
    }
}

impl AndroidKeystore for JniKeystore {
    fn create(&self, alias: &str) -> Result<Vec<u8>, IdentityError> {
        self.call(jni_str!("create"), &[alias.as_bytes()], &jni_sig!("([B)[B"))
    }

    fn public_key(&self, alias: &str) -> Result<Vec<u8>, IdentityError> {
        self.call(
            jni_str!("publicKey"),
            &[alias.as_bytes()],
            &jni_sig!("([B)[B"),
        )
    }

    fn sign(&self, alias: &str, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.call(
            jni_str!("sign"),
            &[alias.as_bytes(), message],
            &jni_sig!("([B[B)[B"),
        )
    }

    fn agree(&self, alias: &str, peer: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        self.call(
            jni_str!("agree"),
            &[alias.as_bytes(), peer],
            &jni_sig!("([B[B)[B"),
        )
        .map(Zeroizing::new)
    }
}
