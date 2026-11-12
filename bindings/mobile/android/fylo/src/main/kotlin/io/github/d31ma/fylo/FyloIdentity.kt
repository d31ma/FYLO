package io.github.d31ma.fylo

import org.json.JSONObject

/** Application identities (ADR 0021): keys named by a key reference. */
object FyloIdentity {
    /** Make the key [reference] names: `{"id", "publicKey"}`. */
    fun create(reference: String): JSONObject =
        FyloAnswer(FyloNative.identityCreate(reference.toByteArray(Charsets.UTF_8))).objectValue()

    /** Describe the identity [reference] names: `{"id", "publicKey"}`. */
    fun show(reference: String): JSONObject =
        FyloAnswer(FyloNative.identityShow(reference.toByteArray(Charsets.UTF_8))).objectValue()
}
