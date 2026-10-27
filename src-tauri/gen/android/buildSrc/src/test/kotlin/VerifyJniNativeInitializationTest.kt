import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VerifyJniNativeInitializationTest {
    private fun nativeInitializer(owner: String, method: String = "initialize", signature: String = "()V", access: String = "0119") = """
    #1              : (in Lio/github/rsyumi/risunest/$owner;)
      name          : '$method'
      type          : '$signature'
      access        : 0x$access (PUBLIC STATIC FINAL NATIVE)
""".trimStart('\n')

    @Test fun nativeInitializersRemainVerifiedWhenR8OmitsTheirMappingLines() {
        val mapping = """
io.github.rsyumi.risunest.DeviceSecrets -> io.github.rsyumi.risunest.DeviceSecrets:
    11:20:byte[] open(java.lang.String,byte[]):83:83 -> open
""".trimIndent()
        assertFalse(mapping.contains("initialize"))
        val dump = nativeInitializer("DeviceSecrets")
        assertEquals(setOf("DeviceSecrets"), retainedJniNativeInitializers(dump))
        assertEquals(retainedJniNativeInitializers(dump), retainedJniNativeInitializers(dump.replace("\n", "\r\n")))
    }

    @Test fun nativeInitializerRequiresExactOwnerNameDescriptorAndFlags() {
        for (dump in listOf(
            "",
            nativeInitializer("OtherSecrets"),
            nativeInitializer("ExternalStorageSecrets"),
            nativeInitializer("ServerSyncSecrets"),
            nativeInitializer("DeviceSecrets", method = "renamed"),
            nativeInitializer("DeviceSecrets", signature = "([B)V"),
            nativeInitializer("DeviceSecrets", signature = "()I"),
            nativeInitializer("DeviceSecrets", access = "0111"),
            nativeInitializer("DeviceSecrets", access = "0019"),
        )) {
            assertTrue(retainedJniNativeInitializers(dump).isEmpty())
        }
        val retained = retainedJniNativeInitializers(nativeInitializer("DeviceSecrets"))
        assertEquals(setOf("DeviceSecrets"), retained)
        assertFalse("ServerSyncSecrets" in retained)
    }

    @Test fun platformTlsInitializerTakesTheApplicationContext() {
        val context = "(Landroid/content/Context;)V"
        assertEquals(setOf("PlatformTls"), retainedJniNativeInitializers(nativeInitializer("PlatformTls", signature = context)))
        for (dump in listOf(
            nativeInitializer("PlatformTls"),
            nativeInitializer("PlatformTls", signature = context, access = "0111"),
            nativeInitializer("DeviceSecrets", signature = context),
        )) {
            assertTrue(retainedJniNativeInitializers(dump).isEmpty())
        }
    }
}
