package io.github.rsyumi.risunest

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream

class NativeFileSelectionTest {
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
