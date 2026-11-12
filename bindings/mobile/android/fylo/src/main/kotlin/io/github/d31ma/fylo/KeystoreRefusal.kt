package io.github.d31ma.fylo

/** An expected Keystore refusal, which [FyloKeystore] answers as its [kind] byte. */
internal class KeystoreRefusal(val kind: Byte, message: String) : Exception(message)
