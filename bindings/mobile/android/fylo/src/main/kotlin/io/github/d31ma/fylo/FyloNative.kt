package io.github.d31ma.fylo

/**
 * The JNI functions of `libfylo_ffi.so` (fylo-ffi's `jni_calls.rs`).
 *
 * Every function takes UTF-8 bytes and answers one byte array: the first byte
 * is the [FyloStatus], the rest is JSON. Calls block and may write to disk, so
 * never call them on the main thread.
 */
internal object FyloNative {
    private const val LIBRARY = "fylo_ffi"

    init {
        System.loadLibrary(LIBRARY)
        install()
    }

    /** Hand FYLO the Java VM and [FyloKeystore], for `android-keystore:` keys. */
    @JvmStatic external fun install()

    @JvmStatic external fun abiVersion(): Int

    @JvmStatic external fun open(path: ByteArray, options: ByteArray, key: ByteArray): ByteArray

    @JvmStatic external fun close(root: Long): ByteArray

    @JvmStatic external fun createCollection(root: Long, name: ByteArray): ByteArray

    @JvmStatic external fun put(root: Long, collection: ByteArray, document: ByteArray): ByteArray

    @JvmStatic external fun get(root: Long, collection: ByteArray, id: ByteArray): ByteArray

    @JvmStatic external fun delete(root: Long, collection: ByteArray, id: ByteArray): ByteArray

    @JvmStatic external fun find(root: Long, collection: ByteArray, query: ByteArray): ByteArray

    @JvmStatic external fun findPage(root: Long, collection: ByteArray, query: ByteArray): ByteArray

    @JvmStatic external fun identityCreate(reference: ByteArray): ByteArray

    @JvmStatic external fun identityShow(reference: ByteArray): ByteArray
}
