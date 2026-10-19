package io.github.rsyumi.risunest

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidControlBridgeTest {
  /** The same golden messages drive the JavaScript control client test. */
  private fun progressFixture(): String {
    var current: File? = File(System.getProperty("user.dir")).absoluteFile
    repeat(8) {
      val candidate = current?.resolve("src/ts/storage/tests/fixtures/androidBackupCopyProgressV1Golden.json")
      if (candidate != null && candidate.isFile) return candidate.readText(Charsets.UTF_8)
      current = current?.parentFile
    }
    error("androidBackupCopyProgressV1Golden.json was not found above ${System.getProperty("user.dir")}")
  }

  @Test fun backupCopyProgressMatchesTheSharedWireFormat() {
    val fixture = progressFixture()
    val method = Regex("\"method\"\\s*:\\s*\"([^\"]+)\"").find(fixture)!!.groupValues[1]
    val id = Regex("\"id\"\\s*:\\s*\"([^\"]+)\"").find(fixture)!!.groupValues[1]
    val messages = Regex(
      "\\{\\s*\"copiedBytes\"\\s*:\\s*(\\d+)\\s*,\\s*\"totalBytes\"\\s*:\\s*(\\d+|null)\\s*," +
        "\\s*\"wire\"\\s*:\\s*\"((?:[^\"\\\\]|\\\\.)*)\"\\s*\\}",
    ).findAll(fixture).toList()
    assertEquals(3, messages.size)
    for (message in messages) {
      val (copied, total, wire) = message.destructured
      val progress = SafSpoolProgress("spool-token", copied.toLong(), total.toLongOrNull())
      assertEquals(wire.replace("\\\"", "\""), androidControlProgressMessage(id, method, progress))
    }
    val progress = SafSpoolProgress("spool-token", 1, 2)
    for (other in listOf("saf.discardPortableSource", "saf.copyExport", "files.pick", "background.progress")) {
      assertNull(other, androidControlProgressMessage(id, other, progress))
    }
  }

  @Test fun copyReportsAdvancingBytesWithoutFloodingTheWebView() {
    var now = 0L
    val delivered = mutableListOf<SafSpoolProgress>()
    val progress = AndroidControlCopyProgress(nowMillis = { now }, emit = { delivered.add(it) })
    val total = 8L * 1024 * 1024 * 1024
    progress.report(SafSpoolProgress("copy", 1, total))
    for (bytes in 2L..100L) progress.report(SafSpoolProgress("copy", bytes, total))
    now = 250
    progress.report(SafSpoolProgress("copy", total / 2, total))
    now = 500
    progress.report(SafSpoolProgress("copy", total / 2, total))
    progress.report(SafSpoolProgress("copy", 1, total))
    now = 501
    progress.report(SafSpoolProgress("copy", total - 1, total))
    progress.report(SafSpoolProgress("copy", total, total))
    assertEquals(listOf(1L, total / 2, total - 1, total), delivered.map { it.copiedBytes })
    assertTrue(delivered.all { it.totalBytes == total })
  }

  @Test fun acceptsOnlyTheExactAppOriginInTheMainFrame() {
    for (origin in listOf("http://tauri.localhost", "https://tauri.localhost")) {
      assertTrue(acceptsAndroidControl(origin, true))
      assertFalse(acceptsAndroidControl(origin, false))
    }
  }

  @Test fun rejectsOpaquePluginAndLookalikeOrigins() {
    for (origin in listOf(
      "null", "about:blank", "file://", "https://example.invalid",
      "https://tauri.localhost.example.invalid", "https://tauri.localhost:5174",
      "https://example.invalid@tauri.localhost", "http://localhost:5174",
    )) {
      assertFalse(origin, acceptsAndroidControl(origin, true))
      assertFalse(origin, acceptsAndroidControl(origin, false))
    }
  }

  @Test fun developmentOriginsAreExactAndNeverAllowedInRelease() {
    val development = "http://192.0.2.1:5174"
    assertTrue(acceptsAndroidControl(development, true, androidControlOrigins(true, development)))
    assertFalse(acceptsAndroidControl(development, false, androidControlOrigins(true, development)))
    assertFalse(acceptsAndroidControl(development, true, androidControlOrigins(false, development)))
    assertFalse(acceptsAndroidControl("http://192.0.2.1:80", true, androidControlOrigins(true, development)))
    for (invalid in listOf("*", "null", "http://*.example.invalid", "http://localhost:5174/path", "http://user@localhost:5174")) {
      assertEquals(CONTROL_ORIGINS, androidControlOrigins(true, invalid))
    }
  }

  @Test fun onlyEventBasedOperationsCanOmitAReply() {
    assertTrue(androidControlNotification("lifecycle.onFlushHold"))
    assertTrue(androidControlNotification("saf.copyExport"))
    assertFalse(androidControlNotification("saf.acknowledgeExport"))
    assertFalse(androidControlNotification("background.defer"))
  }

  @Test fun dispatchSurfaceIsExplicitAndKeepsExistingArgumentShapes() {
    assertEquals(0, androidControlArgumentCount("generation.notificationsEnabled"))
    assertEquals(2, androidControlArgumentCount("background.begin"))
    assertEquals(2, androidControlArgumentCount("background.defer"))
    assertEquals(1, androidControlArgumentCount("lifecycle.onFlushHold"))
    assertEquals(1, androidControlArgumentCount("saf.acknowledgeExport"))
    assertEquals(1, androidControlArgumentCount("saf.pickPortableBackupSource"))
    assertEquals(1, androidControlArgumentCount("saf.discardPortableSource"))
    assertEquals(1, androidControlArgumentCount("saf.materializeBackupSource"))
    assertTrue(androidControlNotification("saf.pickPortableBackupSource"))
    assertFalse(androidControlNotification("saf.materializeBackupSource"))
    assertNull(androidControlArgumentCount("saf.portableSourceDescriptor"))
    assertEquals(2, androidControlArgumentCount("saf.pickContentSource"))
    assertEquals(3, androidControlArgumentCount("saf.copyExport"))
    assertEquals(3, androidControlArgumentCount("saf.reserveExport"))
    assertEquals(3, androidControlArgumentCount("saf.copyReservedExport"))
    assertEquals(2, androidControlArgumentCount("saf.bindExport"))
    assertEquals(1, androidControlArgumentCount("saf.releaseExport"))
    assertFalse(androidControlNotification("saf.bindExport"))
    assertFalse(androidControlNotification("saf.releaseExport"))
    assertNull(androidControlArgumentCount("saf.deleteFile"))
    assertNull(androidControlArgumentCount("lifecycle.finishAndRemoveTask"))
    assertNull(androidControlArgumentCount("generation.*"))
  }
}
