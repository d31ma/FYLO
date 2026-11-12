package io.github.d31ma.fylo

import java.io.Closeable
import org.json.JSONObject
import org.json.JSONTokener

/**
 * An open FYLO root or `.vdbx` database (ADR 0026).
 *
 * Every call blocks and may flush to disk: call FYLO from a background
 * thread, never the main thread. One [Fylo] may be used from many threads.
 * Results are the machine protocol's JSON, as `org.json` objects.
 *
 *     Fylo.open(File(filesDir, "app.vdbx").path, """{"create": {"mode": "plain"}}""").use { db ->
 *         db.createCollection("notes")
 *         val id = db.put("notes", """{"title": "Hello"}""")
 *         val note = db.get("notes", id)
 *     }
 */
class Fylo private constructor(private val root: Long) : Closeable {
    /** Create a document collection: `{"collection", "kind"}`. */
    fun createCollection(name: String): JSONObject =
        FyloAnswer(FyloNative.createCollection(root, name.bytes())).objectValue()

    /** Store a new document from its JSON; answers its id. */
    fun put(collection: String, documentJson: String): String =
        JSONTokener(FyloAnswer(FyloNative.put(root, collection.bytes(), documentJson.bytes())).text())
            .nextValue() as String

    /** One document as `{"<id>": {...}}`, or `null` when it does not exist. */
    fun get(collection: String, id: String): JSONObject? =
        FyloAnswer(FyloNative.get(root, collection.bytes(), id.bytes())).objectOrNull()

    /** Delete one document, keeping its tombstone. */
    fun delete(collection: String, id: String) {
        FyloAnswer(FyloNative.delete(root, collection.bytes(), id.bytes())).text()
    }

    /** Every document a structured query matches: `{"<id>": {...}, ...}`. */
    fun find(collection: String, queryJson: String): JSONObject =
        FyloAnswer(FyloNative.find(root, collection.bytes(), queryJson.bytes())).objectValue()

    /**
     * One page of a query with `$sort` (and `$limit`, `$after`):
     * `{"items": [{"id", "doc"}], "nextCursor"}`; `nextCursor` is null on
     * the last page.
     */
    fun findPage(collection: String, queryJson: String): JSONObject =
        FyloAnswer(FyloNative.findPage(root, collection.bytes(), queryJson.bytes())).objectValue()

    /** Close the root. The object cannot be used afterwards. */
    override fun close() {
        FyloAnswer(FyloNative.close(root)).text()
    }

    companion object {
        /** The C ABI version this library implements. */
        val abiVersion: Int
            get() = FyloNative.abiVersion()

        private const val ROOT = "root"
        private val NO_KEY = ByteArray(0)

        /**
         * Open the root or `.vdbx` database at [path]. [optionsJson] is
         * `{"config": {...}, "identity": {...}, "create": {"mode": ...}}`,
         * or empty; a missing database is made only when `create` says so.
         */
        fun open(path: String, optionsJson: String = ""): Fylo {
            val opened = FyloAnswer(FyloNative.open(path.bytes(), optionsJson.bytes(), NO_KEY))
            return Fylo(opened.objectValue().getLong(ROOT))
        }

        private fun String.bytes(): ByteArray = toByteArray(Charsets.UTF_8)
    }
}
