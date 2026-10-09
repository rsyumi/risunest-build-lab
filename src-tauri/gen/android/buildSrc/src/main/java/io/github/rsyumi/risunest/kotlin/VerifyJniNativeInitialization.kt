private val nativeInitializerDescriptors = mapOf(
    "DeviceSecrets" to "()V",
    "PlatformTls" to "(Landroid/content/Context;)V",
)

private val nativeInitializerRecord = Regex(
    """(?m)^    #\d+\s+: \(in Lio/github/rsyumi/risunest/(\w+);\)\r?\n      name\s+: 'initialize'\r?\n      type\s+: '([^']*)'\r?\n      access\s+: 0x([0-9a-fA-F]+)\b""",
)

fun retainedJniNativeInitializers(dexDump: String): Set<String> =
    nativeInitializerRecord.findAll(dexDump).mapNotNull { record ->
        val (owner, descriptor, access) = record.destructured
        val flags = access.toInt(16)
        if (nativeInitializerDescriptors[owner] == descriptor && (flags and 0x0108) == 0x0108) owner else null
    }.toSet()
