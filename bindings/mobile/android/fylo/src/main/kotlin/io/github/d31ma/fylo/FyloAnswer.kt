package io.github.d31ma.fylo

import org.json.JSONObject

/** One answer of [FyloNative]: its status byte, then its JSON. */
internal class FyloAnswer(bytes: ByteArray) {
    val status: FyloStatus = FyloStatus.of(bytes.first().toInt())
    private val json: String = String(bytes, 1, bytes.size - 1, Charsets.UTF_8)

    /** The JSON text of a call that worked. */
    fun text(): String {
        requireOk()
        return json
    }

    /** The JSON object of a call that worked. */
    fun objectValue(): JSONObject = JSONObject(text())

    /** The JSON object, or `null` when FYLO answered [FyloStatus.NOT_FOUND]. */
    fun objectOrNull(): JSONObject? {
        if (status == FyloStatus.NOT_FOUND) return null
        return objectValue()
    }

    private fun requireOk() {
        if (status != FyloStatus.OK) throw failure()
    }

    private fun failure(): FyloException {
        val error = JSONObject(json)
        return FyloException(status, error.optString(CODE), error.optString(MESSAGE))
    }

    private companion object {
        const val CODE = "code"
        const val MESSAGE = "message"
    }
}
