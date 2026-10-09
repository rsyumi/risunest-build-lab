package io.github.rsyumi.risunest

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Native-only OS protection for external-storage secrets, repository keys and
 * the account token and Sync credentials. Each purpose holds its own Keystore key.
 */
internal object DeviceSecrets {
  private const val PROVIDER_PURPOSE = "external-storage-secrets"
  private const val ROOT_KEY_PURPOSE = "external-storage-root-keys"
  private const val ACCOUNT_PURPOSE = "account-credentials"
  private const val PROVIDER_ALIAS = "risunest.external-storage.secrets"
  private const val ROOT_KEY_ALIAS = "risunest.external-storage.root-key"
  private const val ACCOUNT_ALIAS = "risunest.account.credential"
  private const val SYNC_PURPOSE = "server-sync"
  private const val SYNC_ALIAS = "risunest.server-sync.device-token"
  private fun maxEnvelopeBytes(purpose: String): Int {
    alias(purpose)
    return if (purpose == SYNC_PURPOSE) 16_384 else 65_536
  }

  @JvmStatic external fun initialize()

  internal fun alias(purpose: String): String = when (purpose) {
    PROVIDER_PURPOSE -> PROVIDER_ALIAS
    ROOT_KEY_PURPOSE -> ROOT_KEY_ALIAS
    ACCOUNT_PURPOSE -> ACCOUNT_ALIAS
    SYNC_PURPOSE -> SYNC_ALIAS
    else -> throw IllegalArgumentException("unsupported device secret purpose")
  }

  @JvmStatic
  @Synchronized
  fun removeKeys() {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    removeOwnedKeys { alias -> store.deleteEntry(alias) }
  }

  internal fun removeOwnedKeys(remove: (String) -> Unit) {
    for (alias in listOf(PROVIDER_ALIAS, ROOT_KEY_ALIAS, ACCOUNT_ALIAS, SYNC_ALIAS)) remove(alias)
  }

  @Synchronized
  private fun key(purpose: String): SecretKey {
    val alias = alias(purpose)
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(alias, null) as? SecretKey)?.let { return it }
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
      init(
        KeyGenParameterSpec.Builder(
          alias,
          KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
          .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
          .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
          .setKeySize(256)
          .build(),
      )
    }.generateKey()
  }

  internal fun validatePlaintextSize(purpose: String, size: Int) {
    val minimum = if (purpose == PROVIDER_PURPOSE) 0 else 1
    require(size in minimum..(maxEnvelopeBytes(purpose) - 28))
  }

  internal fun validateEnvelopeSize(purpose: String, size: Int) {
    val minimum = if (purpose == PROVIDER_PURPOSE) 28 else 29
    require(size in minimum..maxEnvelopeBytes(purpose))
  }

  @JvmStatic
  fun seal(purpose: String, input: ByteArray): ByteArray {
    validatePlaintextSize(purpose, input.size)
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, key(purpose))
    check(cipher.iv.size == 12)
    cipher.updateAAD(purpose.toByteArray(Charsets.UTF_8))
    return cipher.iv + cipher.doFinal(input)
  }

  @JvmStatic
  fun open(purpose: String, input: ByteArray): ByteArray {
    validateEnvelopeSize(purpose, input.size)
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.DECRYPT_MODE, key(purpose), GCMParameterSpec(128, input.copyOfRange(0, 12)))
    cipher.updateAAD(purpose.toByteArray(Charsets.UTF_8))
    return cipher.doFinal(input, 12, input.size - 12)
  }
}
