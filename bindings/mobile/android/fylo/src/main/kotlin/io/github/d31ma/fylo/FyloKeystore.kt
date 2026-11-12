package io.github.d31ma.fylo

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.math.BigInteger
import java.security.AlgorithmParameters
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.PublicKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import javax.crypto.KeyAgreement

/**
 * `android-keystore:<alias>` keys (ADR 0026, section 8), called by FYLO
 * through JNI. A key is P-256, made inside the Android Keystore (StrongBox
 * when the device has one, else the TEE) for signing and key agreement; it
 * never leaves the Keystore. User authentication is off, because FYLO signs
 * in the background.
 *
 * Every function answers one byte array: a kind byte (0 for a result), then
 * the result, or a UTF-8 message for a refusal. Points are 65-byte
 * uncompressed SEC1.
 */
internal object FyloKeystore {
    private const val PROVIDER = "AndroidKeyStore"
    private const val CURVE = "secp256r1"
    private const val SIGNATURE = "SHA256withECDSA"
    private const val AGREEMENT = "ECDH"
    private const val COORDINATE_BYTES = 32
    private const val UNCOMPRESSED: Byte = 4
    private const val OK: Byte = 0
    private const val NOT_FOUND: Byte = 1
    private const val ALREADY_EXISTS: Byte = 2
    private const val UNSUPPORTED: Byte = 3
    private const val FAILED: Byte = 4

    @JvmStatic
    fun create(alias: ByteArray): ByteArray = answer { createKey(String(alias, Charsets.UTF_8)) }

    @JvmStatic
    fun publicKey(alias: ByteArray): ByteArray = answer {
        point(certificateKey(String(alias, Charsets.UTF_8)))
    }

    @JvmStatic
    fun sign(alias: ByteArray, message: ByteArray): ByteArray = answer {
        val signer = Signature.getInstance(SIGNATURE)
        signer.initSign(privateKey(String(alias, Charsets.UTF_8)))
        signer.update(message)
        signer.sign()
    }

    @JvmStatic
    fun agree(alias: ByteArray, peer: ByteArray): ByteArray = answer {
        val agreement = KeyAgreement.getInstance(AGREEMENT, PROVIDER)
        agreement.init(privateKey(String(alias, Charsets.UTF_8)))
        agreement.doPhase(peerKey(peer), true)
        agreement.generateSecret()
    }

    private fun answer(call: () -> ByteArray): ByteArray =
        try {
            byteArrayOf(OK) + call()
        } catch (refusal: KeystoreRefusal) {
            byteArrayOf(refusal.kind) + message(refusal)
        } catch (failure: Exception) {
            byteArrayOf(FAILED) + message(failure)
        }

    private fun message(failure: Exception): ByteArray =
        (failure.message ?: failure.javaClass.name).toByteArray(Charsets.UTF_8)

    private fun createKey(alias: String): ByteArray {
        requireAgreement()
        refuseExisting(alias)
        return point(generate(alias))
    }

    private fun requireAgreement() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) return
        throw KeystoreRefusal(
            UNSUPPORTED,
            "android-keystore: needs Android 12 (API 31) or newer, whose Keystore does key agreement",
        )
    }

    private fun refuseExisting(alias: String) {
        if (!keyStore().containsAlias(alias)) return
        throw KeystoreRefusal(ALREADY_EXISTS, "android-keystore:$alias already holds a key")
    }

    private fun generate(alias: String): PublicKey =
        try {
            generate(alias, strongBox = true)
        } catch (missing: StrongBoxUnavailableException) {
            generate(alias, strongBox = false)
        }

    private fun generate(alias: String, strongBox: Boolean): PublicKey {
        val purposes = KeyProperties.PURPOSE_SIGN or KeyProperties.PURPOSE_AGREE_KEY
        val specification = KeyGenParameterSpec.Builder(alias, purposes)
            .setAlgorithmParameterSpec(ECGenParameterSpec(CURVE))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(false)
            .setIsStrongBoxBacked(strongBox)
            .build()
        val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, PROVIDER)
        generator.initialize(specification)
        return generator.generateKeyPair().public
    }

    private fun keyStore(): KeyStore {
        val store = KeyStore.getInstance(PROVIDER)
        store.load(null)
        return store
    }

    private fun certificateKey(alias: String): PublicKey {
        val certificate = keyStore().getCertificate(alias)
            ?: throw KeystoreRefusal(NOT_FOUND, "android-keystore:$alias holds no key")
        return certificate.publicKey
    }

    private fun privateKey(alias: String): PrivateKey =
        keyStore().getKey(alias, null) as? PrivateKey
            ?: throw KeystoreRefusal(NOT_FOUND, "android-keystore:$alias holds no key")

    private fun point(key: PublicKey): ByteArray {
        val point = (key as ECPublicKey).w
        return byteArrayOf(UNCOMPRESSED) + coordinate(point.affineX) + coordinate(point.affineY)
    }

    private fun coordinate(value: BigInteger): ByteArray {
        val bytes = value.toByteArray().takeLast(COORDINATE_BYTES).toByteArray()
        return ByteArray(COORDINATE_BYTES - bytes.size) + bytes
    }

    private fun peerKey(peer: ByteArray): PublicKey {
        val x = BigInteger(1, peer.copyOfRange(1, 1 + COORDINATE_BYTES))
        val y = BigInteger(1, peer.copyOfRange(1 + COORDINATE_BYTES, 1 + 2 * COORDINATE_BYTES))
        val specification = ECPublicKeySpec(ECPoint(x, y), curve())
        return KeyFactory.getInstance(KeyProperties.KEY_ALGORITHM_EC).generatePublic(specification)
    }

    private fun curve(): ECParameterSpec {
        val parameters = AlgorithmParameters.getInstance(KeyProperties.KEY_ALGORITHM_EC)
        parameters.init(ECGenParameterSpec(CURVE))
        return parameters.getParameterSpec(ECParameterSpec::class.java)
    }
}
