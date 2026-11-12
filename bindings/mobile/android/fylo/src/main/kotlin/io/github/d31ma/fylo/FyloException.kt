package io.github.d31ma.fylo

/**
 * A FYLO call that did not work. [code] is a code of FYLO's error list
 * (`api/errors/v1.json`), such as `EROOTCLOSED` or `EBADREQUEST`.
 */
class FyloException(
    val status: FyloStatus,
    val code: String,
    message: String,
) : RuntimeException("$code: $message")
