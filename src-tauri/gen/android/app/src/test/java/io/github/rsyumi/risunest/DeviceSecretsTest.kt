package io.github.rsyumi.risunest

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.lang.reflect.Modifier

class DeviceSecretsTest {
  @Test
  fun cleanupDeletesOnlyOwnedAliasesAndPropagatesFailures() {
    val expected = listOf("risunest.external-storage.secrets", "risunest.external-storage.root-key", "risunest.account.credential", "risunest.server-sync.device-token")
    val removed = mutableListOf<String>()
    DeviceSecrets.removeOwnedKeys { removed.add(it) }
    assertEquals(expected, removed)
    assertThrows(IllegalStateException::class.java) {
      DeviceSecrets.removeOwnedKeys { throw IllegalStateException("locked synthetic store") }
    }
    removed.clear()
    DeviceSecrets.removeOwnedKeys { removed.add(it) }
    assertEquals(expected, removed)
    val method = DeviceSecrets::class.java.getDeclaredMethod("removeKeys")
    assertTrue(Modifier.isPublic(method.modifiers))
    assertTrue(Modifier.isStatic(method.modifiers))
    assertEquals(Void.TYPE, method.returnType)
  }

  @Test
  fun nativeEntryPointsMatchTheJniSignatures() {
    val type = DeviceSecrets::class.java
    for (name in listOf("seal", "open")) {
      val method = type.getDeclaredMethod(name, String::class.java, ByteArray::class.java)
      assertTrue(Modifier.isPublic(method.modifiers))
      assertTrue(Modifier.isStatic(method.modifiers))
      assertEquals(ByteArray::class.java, method.returnType)
    }
    val initialize = type.getDeclaredMethod("initialize")
    assertTrue(Modifier.isStatic(initialize.modifiers))
    assertTrue(Modifier.isNative(initialize.modifiers))
    assertEquals(Void.TYPE, initialize.returnType)
  }

  @Test
  fun everyNativePurposeMapsToItsOwnKeystoreAlias() {
    val aliases = listOf(
      "external-storage-secrets",
      "external-storage-root-keys",
      "account-credentials",
      "server-sync",
    ).map(DeviceSecrets::alias)

    assertEquals(aliases.size, aliases.toSet().size)
    assertThrows(IllegalArgumentException::class.java) {
      DeviceSecrets.alias("account-credentials-2")
    }
  }

  @Test
  fun plaintextAndEnvelopeBoundsAllowEmptyProviderPasswordsOnly() {
    DeviceSecrets.validatePlaintextSize("external-storage-secrets", 0)
    DeviceSecrets.validatePlaintextSize("external-storage-secrets", 1)
    DeviceSecrets.validatePlaintextSize("external-storage-secrets", 65_508)
    assertThrows(IllegalArgumentException::class.java) {
      DeviceSecrets.validatePlaintextSize("external-storage-secrets", -1)
    }
    assertThrows(IllegalArgumentException::class.java) {
      DeviceSecrets.validatePlaintextSize("external-storage-secrets", 65_509)
    }

    DeviceSecrets.validateEnvelopeSize("external-storage-secrets", 28)
    DeviceSecrets.validateEnvelopeSize("external-storage-secrets", 65_536)
    assertThrows(IllegalArgumentException::class.java) {
      DeviceSecrets.validateEnvelopeSize("external-storage-secrets", 27)
    }
    assertThrows(IllegalArgumentException::class.java) {
      DeviceSecrets.validateEnvelopeSize("external-storage-secrets", 65_537)
    }
    for (purpose in listOf("external-storage-root-keys", "account-credentials", "server-sync")) {
      assertThrows(IllegalArgumentException::class.java) {
        DeviceSecrets.validatePlaintextSize(purpose, 0)
      }
      assertThrows(IllegalArgumentException::class.java) {
        DeviceSecrets.validateEnvelopeSize(purpose, 28)
      }
    }
  }
  @Test fun acceptsStructuredCredentialsBeyondTheOldTokenLength() {
    DeviceSecrets.validatePlaintextSize("server-sync", 2048)
    DeviceSecrets.validateEnvelopeSize("server-sync", 2076)
    DeviceSecrets.validatePlaintextSize("server-sync", 16356)
    DeviceSecrets.validateEnvelopeSize("server-sync", 16384)
  }

  @Test fun rejectsEmptyTruncatedAndOversizedCredentials() {
    for (size in listOf(0, 16357)) {
      assertThrows(IllegalArgumentException::class.java) {
        DeviceSecrets.validatePlaintextSize("server-sync", size)
      }
    }
    for (size in listOf(0, 28, 16385)) {
      assertThrows(IllegalArgumentException::class.java) {
        DeviceSecrets.validateEnvelopeSize("server-sync", size)
      }
    }
  }
}
