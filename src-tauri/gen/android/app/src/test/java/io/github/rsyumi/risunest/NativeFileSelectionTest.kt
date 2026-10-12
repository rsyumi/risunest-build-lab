package io.github.rsyumi.risunest

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream

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
    var checks = 0
    copySelectedDocument(ByteArrayInputStream(data), output) { checks++ }
    assertArrayEquals(data, output.toByteArray())
    assertEquals(4, checks)
  }

  @Test fun preservesProviderReadFailure() {
    val source = object : InputStream() {
      override fun read(): Int = throw IOException("synthetic provider denied access")
    }
    val error = assertThrows(IOException::class.java) {
      copySelectedDocument(source, ByteArrayOutputStream()) {}
    }
    assertEquals("synthetic provider denied access", error.message)
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
}
