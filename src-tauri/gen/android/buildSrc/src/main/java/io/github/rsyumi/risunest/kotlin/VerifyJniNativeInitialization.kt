private val nativeInitializerRecord = Regex(
    """(?m)^    #\d+\s+: \(in Lio/github/rsyumi/risunest/(ExternalStorageSecrets|ServerSyncSecrets);\)\r?\n      name\s+: 'initialize'\r?\n      type\s+: '\(\)V'\r?\n      access\s+: 0x([0-9a-fA-F]+)\b""",
)

fun retainedJniNativeInitializers(dexDump: String): Set<String> =
    nativeInitializerRecord.findAll(dexDump).mapNotNull { record ->
        val access = record.groupValues[2].toInt(16)
        if ((access and 0x0108) == 0x0108) record.groupValues[1] else null
    }.toSet()
