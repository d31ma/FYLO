package io.github.d31ma.fylo

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/** The Kotlin wrapper on a device or emulator, through JNI (ADR 0026). */
@RunWith(AndroidJUnit4::class)
class FyloTest {
    private lateinit var folder: File

    @Before
    fun makeFolder() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        folder = File(context.filesDir, "fylo-test-${System.nanoTime()}")
        folder.mkdirs()
    }

    @After
    fun removeFolder() {
        folder.deleteRecursively()
    }

    private fun database(): String = File(folder, "app.vdbx").path

    private fun openPlain(): Fylo = Fylo.open(database(), """{"create": {"mode": "plain"}}""")

    private fun expectStatus(expected: FyloStatus, call: () -> Unit) {
        try {
            call()
            fail("expected $expected")
        } catch (error: FyloException) {
            assertEquals(expected, error.status)
        }
    }

    @Test
    fun theLibraryReportsAbiVersion1() {
        assertEquals(1, Fylo.abiVersion)
    }

    @Test
    fun aPlainDatabaseIsCreatedWrittenReadFoundAndDeleted() {
        openPlain().use { db ->
            assertEquals("notes", db.createCollection("notes").getString("collection"))
            val id = db.put("notes", """{"title": "Hello", "rank": 1}""")
            assertEquals("Hello", db.get("notes", id)!!.getJSONObject(id).getString("title"))
            val found = db.find("notes", """{"${'$'}ops": [{"title": {"${'$'}eq": "Hello"}}]}""")
            assertTrue(found.has(id))
            db.delete("notes", id)
            assertNull(db.get("notes", id))
        }
    }

    @Test
    fun sortedPagesFollowTheCursorToTheLastPage() {
        openPlain().use { db ->
            db.createCollection("messages")
            for (sent in 1..3) db.put("messages", """{"sentAt": $sent}""")
            val first = db.findPage("messages", """{"${'$'}sort": {"sentAt": "desc"}, "${'$'}limit": 2}""")
            assertEquals(2, first.getJSONArray("items").length())
            val after = first.getString("nextCursor")
            val query = JSONObject()
                .put("${'$'}sort", JSONObject().put("sentAt", "desc"))
                .put("${'$'}limit", 2)
                .put("${'$'}after", after)
            val second = db.findPage("messages", query.toString())
            assertEquals(1, second.getJSONArray("items").length())
            assertTrue(second.isNull("nextCursor"))
        }
    }

    @Test
    fun aClosedDatabaseAnswersClosed() {
        val db = openPlain()
        db.close()
        expectStatus(FyloStatus.CLOSED) { db.createCollection("late") }
    }

    @Test
    fun aMissingDatabaseWithoutCreateIsNotMade() {
        expectStatus(FyloStatus.ERROR) { Fylo.open(database()) }
        assertTrue(!File(database()).exists())
    }

    @Test
    fun badOptionsAreAnInvalidArgument() {
        expectStatus(FyloStatus.INVALID_ARGUMENT) { Fylo.open(database(), """{"unknown": true}""") }
    }

    @Test
    fun anIdentityIsCreatedAndProtectsADatabase() {
        val reference = "file:" + File(folder, "device.pem").path
        val created = FyloIdentity.create(reference)
        assertEquals(created.getString("id"), FyloIdentity.show(reference).getString("id"))
        val options = JSONObject().put(
            "create",
            JSONObject().put("mode", "protected").put("admin", reference),
        )
        Fylo.open(database(), options.toString()).use { db ->
            db.createCollection("secrets")
            val id = db.put("secrets", """{"pin": "1234"}""")
            assertEquals("1234", db.get("secrets", id)!!.getJSONObject(id).getString("pin"))
        }
    }
}
