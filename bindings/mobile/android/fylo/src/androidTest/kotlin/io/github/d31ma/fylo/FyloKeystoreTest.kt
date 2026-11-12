package io.github.d31ma.fylo

import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.KeyStore
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * `android-keystore:` keys through FYLO (ADR 0026, section 8). The
 * emulator's Keystore is software, so this proves the code path, not the
 * device's hardware.
 */
@RunWith(AndroidJUnit4::class)
class FyloKeystoreTest {
    private lateinit var folder: File
    private val alias = "fylo.test.${System.nanoTime()}"
    private val reference = "android-keystore:$alias"

    @Before
    fun prepare() {
        assumeTrue(Build.VERSION.SDK_INT >= Build.VERSION_CODES.S)
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        folder = File(context.filesDir, "fylo-keystore-${System.nanoTime()}")
        folder.mkdirs()
    }

    @After
    fun removeKey() {
        val store = KeyStore.getInstance("AndroidKeyStore")
        store.load(null)
        store.deleteEntry(alias)
        folder.deleteRecursively()
    }

    @Test
    fun aKeystoreIdentityIsCreatedAndShownAgain() {
        val created = FyloIdentity.create(reference)
        assertEquals(created.getString("id"), FyloIdentity.show(reference).getString("id"))
    }

    @Test
    fun aKeystoreIdentityProtectsADatabaseAcrossReopening() {
        val id = FyloIdentity.create(reference).getString("id")
        val database = File(folder, "secure.vdbx").path
        val create = JSONObject().put(
            "create",
            JSONObject().put("mode", "protected").put("admin", reference),
        )
        val stored = Fylo.open(database, create.toString()).use { db ->
            db.createCollection("secrets")
            db.put("secrets", """{"pin": "1234"}""")
        }
        val reopen = JSONObject().put(
            "identity",
            JSONObject().put("reference", reference).put("trust", JSONObject().put("pinned", id)),
        )
        Fylo.open(database, reopen.toString()).use { db ->
            assertEquals("1234", db.get("secrets", stored)!!.getJSONObject(stored).getString("pin"))
        }
    }

    @Test
    fun aSecondKeyUnderTheSameAliasIsRefused() {
        FyloIdentity.create(reference)
        try {
            FyloIdentity.create(reference)
            throw AssertionError("expected a refusal")
        } catch (error: FyloException) {
            assertEquals(FyloStatus.ERROR, error.status)
        }
    }
}
