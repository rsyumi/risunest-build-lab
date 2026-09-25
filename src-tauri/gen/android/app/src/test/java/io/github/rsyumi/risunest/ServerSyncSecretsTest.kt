package io.github.rsyumi.risunest

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import java.lang.reflect.Modifier
import org.junit.Assert.assertThrows
import org.junit.Test

class ServerSyncSecretsTest {
  @Test
  fun cleanupDeletesOnlyOwnedAliasesAndPropagatesFailures() {
    val expected = listOf("risunest.server-sync.device-token")
    val removed = mutableListOf<String>()
    ServerSyncSecrets.removeOwnedKeys { removed.add(it) }
    assertEquals(expected, removed)
    assertThrows(IllegalStateException::class.java) {
      ServerSyncSecrets.removeOwnedKeys { throw IllegalStateException("locked synthetic store") }
    }
    removed.clear()
    ServerSyncSecrets.removeOwnedKeys { removed.add(it) }
    assertEquals(expected, removed)
    val method = ServerSyncSecrets::class.java.getDeclaredMethod("removeKeys")
    assertTrue(Modifier.isPublic(method.modifiers))
    assertTrue(Modifier.isStatic(method.modifiers))
    assertEquals(Void.TYPE, method.returnType)
  }

  @Test fun acceptsStructuredCredentialsBeyondTheOldTokenLength() {
    ServerSyncSecrets.validatePlaintextSize(2048)
    ServerSyncSecrets.validateEnvelopeSize(2076)
    ServerSyncSecrets.validatePlaintextSize(16356)
    ServerSyncSecrets.validateEnvelopeSize(16384)
  }

  @Test fun rejectsEmptyTruncatedAndOversizedCredentials() {
    for (size in listOf(0, 16357)) {
      assertThrows(IllegalArgumentException::class.java) {
        ServerSyncSecrets.validatePlaintextSize(size)
      }
    }
    for (size in listOf(0, 28, 16385)) {
      assertThrows(IllegalArgumentException::class.java) {
        ServerSyncSecrets.validateEnvelopeSize(size)
      }
    }
  }
}
