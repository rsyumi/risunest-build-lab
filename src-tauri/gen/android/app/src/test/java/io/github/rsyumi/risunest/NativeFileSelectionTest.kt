package io.github.rsyumi.risunest

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.UUID

class NativeFileSelectionTest {
  @get:Rule val temporary = TemporaryFolder()

  @Test fun staleCleanupReportsFailureAndContinuesWithOtherDocuments() {
    val stale = temporary.newFolder("stale").apply { setLastModified(1_000) }
    val other = temporary.newFolder("other").apply { setLastModified(1_000) }
    temporary.newFolder("fresh").apply { setLastModified(3_000) }
    val visited = mutableListOf<String>()
    val errors = mutableListOf<Exception>()
    cleanupStaleSelectedDocuments(temporary.root, 2_000, { path ->
      visited.add(path)
      if (path == File(stale, "source").absolutePath) throw IOException("synthetic cleanup denied")
    }, errors::add)
    assertEquals(setOf(File(stale, "source").absolutePath, File(other, "source").absolutePath), visited.toSet())
    assertEquals(listOf("synthetic cleanup denied"), errors.map { it.message })
  }

  @Test fun copiesExactBinaryBytesAcrossBoundedReads() {
    val data = ByteArray(600_001) { (it % 256).toByte() }
    val output = ByteArrayOutputStream()
    copySelectedDocument(ByteArrayInputStream(data), output) {}
    assertArrayEquals(data, output.toByteArray())
  }

  @Test fun stopsCopyingBetweenChunksWhenCancelled() {
    var checks = 0
    val output = ByteArrayOutputStream()
    assertThrows(InterruptedException::class.java) {
      copySelectedDocument(ByteArrayInputStream(ByteArray(600_001)), output) {
        if (++checks == 2) throw InterruptedException("synthetic cancellation")
      }
    }
    assertEquals(256 * 1024, output.size())
  }

  @Test fun discardRemovesAStagedSourceAndItsFolder() {
    val root = temporary.newFolder("selected_documents")
    val folder = File(root, UUID.randomUUID().toString()).apply { mkdirs() }
    val source = File(folder, "source").apply { writeText("synthetic") }
    discardSelectedDocument(root, source.absolutePath)
    assertFalse(source.exists())
    assertFalse(folder.exists())
  }

  @Test fun discardRefusesASourceOutsideTheStagingRoot() {
    val root = temporary.newFolder("selected_documents")
    val outside = UUID.randomUUID().toString()
    val source = File(temporary.newFolder("elsewhere", outside), "source").apply { writeText("synthetic") }
    val escaping = File(root, "${UUID.randomUUID()}/../../elsewhere/$outside/source")
    assertThrows(IllegalArgumentException::class.java) { discardSelectedDocument(root, escaping.path) }
    assertTrue(source.exists())
  }

  @Test fun discardRefusesAStagedFileNotNamedSource() {
    val root = temporary.newFolder("selected_documents")
    val folder = File(root, UUID.randomUUID().toString()).apply { mkdirs() }
    val other = File(folder, "other").apply { writeText("synthetic") }
    assertThrows(IllegalArgumentException::class.java) { discardSelectedDocument(root, other.absolutePath) }
    assertTrue(other.exists())
  }
}
