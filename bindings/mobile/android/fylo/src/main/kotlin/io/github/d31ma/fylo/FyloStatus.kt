package io.github.d31ma.fylo

/** What a FYLO call did; the same numbers as `FyloStatus` in `fylo.h`. */
enum class FyloStatus(val number: Int) {
    OK(0),
    NOT_FOUND(1),
    ERROR(2),
    INVALID_ARGUMENT(3),
    CLOSED(4),
    PANIC(5);

    companion object {
        /** The status with [number]; an unknown number is [PANIC]. */
        fun of(number: Int): FyloStatus = entries.firstOrNull { it.number == number } ?: PANIC
    }
}
