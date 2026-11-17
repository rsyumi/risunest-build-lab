package io.github.rsyumi.risunest

import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.atomic.AtomicBoolean

private const val DESTINATION_STATE_VERSION = 3
private const val MAX_DESTINATION_STATE_BYTES = 8_192L
private const val MAX_DESTINATION_URI_CHARS = 2_048
internal const val DESTINATION_PICKER_STALE_MILLIS = 60 * 60 * 1_000L
private val SAFE_CODE = Regex("[a-z0-9-]{1,64}")

internal enum class SafDestinationPhase(val wireName: String) {
  PICKING("picking"),
  COPYING("copying"),
  CANCELLING("cancelling"),
  SUCCEEDED("succeeded"),
  FAILED("failed"),
  CANCELLED("cancelled");

  companion object {
    fun fromWireName(value: String): SafDestinationPhase? = entries.find { it.wireName == value }
  }
}

internal enum class SafDestinationSourceKind(val wireName: String) {
  RISU_SAVE("risuSave"),
  LEGACY_BACKUP("legacyBackup"),
  SCREENSHOT("screenshot"),
  DOWNLOAD("download");

  companion object {
    fun fromWireName(value: String): SafDestinationSourceKind? =
      entries.find { it.wireName == value }
  }
}

internal enum class SafDestinationRecoveryAction {
  WAIT_FOR_PICKER,
  CLEAN_PARTIAL,
  FAIL_INTERRUPTED,
  REPLAY_TERMINAL,
}

internal class SafDestinationSlot {
  private val held = AtomicBoolean(false)

  fun tryAcquire(): Boolean = held.compareAndSet(false, true)

  fun acquireRestored() {
    held.set(true)
  }

  fun release() {
    held.set(false)
  }
}

internal fun isPendingSafDestinationPicker(record: SafDestinationRecord): Boolean =
  record.destinationUri == null &&
    record.phase in setOf(SafDestinationPhase.PICKING, SafDestinationPhase.CANCELLING)

internal fun restoredSafDestinationCancellation(record: SafDestinationRecord): Boolean =
  record.reservedDestination || record.phase == SafDestinationPhase.CANCELLING

internal fun selectedSafDestinationState(
  record: SafDestinationRecord,
  requestId: String,
  destinationUri: String?,
  cancellationRequested: Boolean,
  nowMillis: Long,
): SafDestinationRecord? {
  if (record.requestId != requestId || !isPendingSafDestinationPicker(record)) return null
  if (destinationUri == null) {
    return record.copy(
      phase = SafDestinationPhase.CANCELLED,
      destinationUri = null,
      bytes = null,
      code = "cancelled",
      warningCodes = emptyList(),
      updatedAtMillis = nowMillis,
      publicationPrerequisitesComplete = record.reservedDestination && !record.copyStarted,
    )
  }
  return record.copy(
    phase = if (record.phase == SafDestinationPhase.CANCELLING || cancellationRequested) {
      SafDestinationPhase.CANCELLING
    } else {
      SafDestinationPhase.COPYING
    },
    destinationUri = destinationUri,
    bytes = null,
    code = null,
    warningCodes = listOf("android-saf-provider-not-atomic"),
    updatedAtMillis = nowMillis,
  )
}

internal fun expiredSafDestinationState(
  record: SafDestinationRecord,
  requestId: String,
  nowMillis: Long,
): SafDestinationRecord? {
  if (
    record.requestId != requestId ||
    !isPendingSafDestinationPicker(record) ||
    record.reservedDestination ||
    nowMillis - record.updatedAtMillis < DESTINATION_PICKER_STALE_MILLIS
  ) return null
  val wasCancelling = record.phase == SafDestinationPhase.CANCELLING
  return record.copy(
    phase = if (wasCancelling) SafDestinationPhase.CANCELLED else SafDestinationPhase.FAILED,
    destinationUri = null,
    bytes = null,
    code = if (wasCancelling) "cancelled" else "destination-interrupted",
    warningCodes = emptyList(),
    updatedAtMillis = nowMillis,
    publicationPrerequisitesComplete = record.reservedDestination && !record.copyStarted,
  )
}

internal fun decideSafDestinationRecovery(
  record: SafDestinationRecord,
  hasRestoredActivityState: Boolean,
  nowMillis: Long = System.currentTimeMillis(),
): SafDestinationRecoveryAction = when (record.phase) {
  SafDestinationPhase.PICKING -> if (
    hasRestoredActivityState &&
    (record.reservedDestination || nowMillis - record.updatedAtMillis < DESTINATION_PICKER_STALE_MILLIS)
  ) {
    SafDestinationRecoveryAction.WAIT_FOR_PICKER
  } else {
    SafDestinationRecoveryAction.FAIL_INTERRUPTED
  }
  SafDestinationPhase.COPYING -> SafDestinationRecoveryAction.CLEAN_PARTIAL
  SafDestinationPhase.CANCELLING -> if (record.destinationUri == null) {
    if (
      hasRestoredActivityState &&
      (record.reservedDestination || nowMillis - record.updatedAtMillis < DESTINATION_PICKER_STALE_MILLIS)
    ) {
      SafDestinationRecoveryAction.WAIT_FOR_PICKER
    } else {
      SafDestinationRecoveryAction.FAIL_INTERRUPTED
    }
  } else {
    SafDestinationRecoveryAction.CLEAN_PARTIAL
  }
  SafDestinationPhase.SUCCEEDED,
  SafDestinationPhase.FAILED,
  SafDestinationPhase.CANCELLED,
  -> SafDestinationRecoveryAction.REPLAY_TERMINAL
}

internal data class SafDestinationRecord(
  val requestId: String,
  val exportId: String,
  val phase: SafDestinationPhase,
  val destinationUri: String?,
  val bytes: Long?,
  val code: String?,
  val warningCodes: List<String>,
  val updatedAtMillis: Long,
  val sourceKind: SafDestinationSourceKind = SafDestinationSourceKind.RISU_SAVE,
  val publicationPrerequisitesComplete: Boolean = false,
  val reservedDestination: Boolean = false,
  val exportBound: Boolean = true,
  val copyStarted: Boolean = true,
  val jobId: String? = null,
) {
  fun isTerminal() = phase in setOf(
    SafDestinationPhase.SUCCEEDED,
    SafDestinationPhase.FAILED,
    SafDestinationPhase.CANCELLED,
  )
}

internal class SafDestinationStateStore(
  private val stateFile: File,
  private val atomicPublisher: SafAtomicPublisher,
) {
  fun load(): SafDestinationRecord? {
    if (!stateFile.isFile || stateFile.length() > MAX_DESTINATION_STATE_BYTES) return null
    val json = runCatching { stateFile.readText(Charsets.UTF_8) }.getOrNull() ?: return null
    val version = numberField(json, "version") ?: return null
    if (version != DESTINATION_STATE_VERSION.toLong()) return null
    val requestId = stringField(json, "requestId") ?: return null
    val exportId = stringField(json, "exportId") ?: return null
    val phase = stringField(json, "phase")?.let(SafDestinationPhase::fromWireName) ?: return null
    val destinationUri = nullableStringField(json, "destinationUri") ?: return null
    val bytes = nullableLongField(json, "bytes") ?: return null
    val code = nullableStringField(json, "code") ?: return null
    val warningCodes = warningCodesField(json) ?: return null
    val updatedAtMillis = numberField(json, "updatedAtMillis") ?: return null
    val sourceKind = stringField(json, "sourceKind")
      ?.let(SafDestinationSourceKind::fromWireName)
      ?: return null
    val publicationPrerequisitesComplete = booleanField(
      json,
      "publicationPrerequisitesComplete",
    ) ?: return null
    val record = SafDestinationRecord(
      requestId = requestId,
      exportId = exportId,
      phase = phase,
      destinationUri = destinationUri.value,
      bytes = bytes.value,
      code = code.value,
      warningCodes = warningCodes,
      updatedAtMillis = updatedAtMillis,
      sourceKind = sourceKind,
      publicationPrerequisitesComplete = publicationPrerequisitesComplete,
      reservedDestination = optionalBooleanField(json, "reservedDestination", false) ?: return null,
      exportBound = optionalBooleanField(json, "exportBound", true) ?: return null,
      copyStarted = optionalBooleanField(json, "copyStarted", true) ?: return null,
      jobId = if (json.contains("\"jobId\"")) (nullableStringField(json, "jobId") ?: return null).value else null,
    )
    return record.takeIf(::isValidRecord)
  }

  fun save(record: SafDestinationRecord) {
    require(isValidRecord(record)) { "invalid Android SAF destination state" }
    val parent = stateFile.parentFile ?: error("destination state requires a parent")
    check(parent.mkdirs() || parent.isDirectory) { "destination state root cannot be created" }
    val temporary = parent.resolve("${stateFile.name}.tmp")
    parent.resolve("${stateFile.name}.cleared").delete()
    val warnings = record.warningCodes.joinToString(",") { "\"$it\"" }
    val json = "{" +
      "\"version\":$DESTINATION_STATE_VERSION," +
      "\"requestId\":\"${record.requestId}\"," +
      "\"exportId\":\"${record.exportId}\"," +
      "\"sourceKind\":\"${record.sourceKind.wireName}\"," +
      "\"phase\":\"${record.phase.wireName}\"," +
      "\"destinationUri\":${record.destinationUri?.let { "\"$it\"" } ?: "null"}," +
      "\"bytes\":${record.bytes ?: "null"}," +
      "\"code\":${record.code?.let { "\"$it\"" } ?: "null"}," +
      "\"warningCodes\":[$warnings]," +
      "\"publicationPrerequisitesComplete\":${record.publicationPrerequisitesComplete}," +
      "\"reservedDestination\":${record.reservedDestination}," +
      "\"exportBound\":${record.exportBound}," +
      "\"copyStarted\":${record.copyStarted}," +
      "\"jobId\":${record.jobId?.let { "\"$it\"" } ?: "null"}," +
      "\"updatedAtMillis\":${record.updatedAtMillis}" +
      "}"
    try {
      FileOutputStream(temporary, false).use { output ->
        output.write(json.toByteArray(Charsets.UTF_8))
        output.flush()
        output.fd.sync()
      }
      atomicPublisher.publish(temporary, stateFile)
    } catch (error: Exception) {
      temporary.delete()
      throw error
    }
  }

  fun clear(requestId: String): Boolean {
    val record = load() ?: return false
    if (record.requestId != requestId || !record.isTerminal()) return false
    val parent = stateFile.parentFile ?: return false
    parent.resolve("${stateFile.name}.tmp").delete()
    val cleared = parent.resolve("${stateFile.name}.cleared")
    cleared.delete()
    return try {
      atomicPublisher.publish(stateFile, cleared)
      cleared.delete()
      true
    } catch (error: Exception) {
      !stateFile.exists()
    }
  }
}

// Recovered receipts that a newer export moved out of the journal, one file per request until acknowledged.
internal class SafDestinationReceiptStore(
  private val directory: File,
  private val atomicPublisher: SafAtomicPublisher,
) {
  private fun store(requestId: String) =
    SafDestinationStateStore(directory.resolve("$requestId.json"), atomicPublisher)

  fun save(record: SafDestinationRecord) = store(record.requestId).save(record)

  fun loadAll(): List<SafDestinationRecord> = directory.listFiles().orEmpty()
    .map { it.name.removeSuffix(".json") }
    .filter(::isCanonicalUuidV4)
    .mapNotNull { requestId -> store(requestId).load()?.takeIf { it.requestId == requestId } }
    .sortedBy { it.updatedAtMillis }

  fun clear(requestId: String): Boolean =
    !directory.resolve("$requestId.json").exists() || store(requestId).clear(requestId)
}

/** Moves a recovered receipt still waiting for its owner out of the journal before another export takes it. */
internal fun keepRecoveredSafReceipt(
  current: SafDestinationRecord?,
  nextRequestId: String,
  session: SafDestinationRendererSession,
  receipts: SafDestinationReceiptStore,
) {
  if (current != null && current.requestId != nextRequestId && session.hasRecovered(current.requestId)) {
    receipts.save(current)
  }
}

internal fun completedSafPublicationPrerequisites(
  record: SafDestinationRecord,
  requestId: String,
  nowMillis: Long,
): SafDestinationRecord? {
  if (
    record.requestId != requestId ||
    record.sourceKind != SafDestinationSourceKind.RISU_SAVE ||
    !record.isTerminal()
  ) return null
  return record.copy(
    publicationPrerequisitesComplete = true,
    updatedAtMillis = nowMillis,
  )
}

internal fun reconciledSafDestination(
  record: SafDestinationRecord,
  ownedByRenderer: Boolean,
  nowMillis: Long,
  requiresPublicationProof: Boolean,
): SafDestinationRecord? {
  if (ownedByRenderer || record.sourceKind != SafDestinationSourceKind.RISU_SAVE || !record.isTerminal()) return null
  if (!record.exportBound || !record.copyStarted || record.publicationPrerequisitesComplete) return record
  if (!requiresPublicationProof) return record.copy(
    publicationPrerequisitesComplete = true,
    updatedAtMillis = nowMillis,
  )
  return record.copy(
    phase = SafDestinationPhase.FAILED,
    code = "destination-interrupted",
    warningCodes = (record.warningCodes.filterNot { it in interruptedSafDestinationWarnings() }.take(14) +
      interruptedSafDestinationWarnings()).distinct(),
    publicationPrerequisitesComplete = true,
    updatedAtMillis = nowMillis,
  )
}

// Session ownership is deliberately transient; persisted records belong to the previous renderer.
internal class SafDestinationRendererSession {
  var generation: Long = 0
    private set
  private var ownedRequestId: String? = null
  private val recovered = linkedMapOf<String, SafDestinationRecord>()

  fun own(requestId: String, expectedGeneration: Long = generation) {
    if (expectedGeneration == generation) ownedRequestId = requestId
  }
  fun retire() { ownedRequestId = null; generation++ }
  fun owns(requestId: String): Boolean = ownedRequestId == requestId
  fun pending(): List<SafDestinationRecord> = recovered.values.toList()
  fun hasRecovered(requestId: String): Boolean = recovered.containsKey(requestId)
  fun status(current: SafDestinationRecord?): SafDestinationRecord? =
    if (current == null) recovered.values.firstOrNull() else current.takeIf(SafDestinationRecord::isTerminal)

  fun reconcile(
    record: SafDestinationRecord,
    nowMillis: Long,
    save: (SafDestinationRecord) -> Unit,
    prepare: (SafDestinationRecord) -> Boolean,
    requiresPublicationProof: (SafDestinationRecord) -> Boolean,
  ): SafDestinationRecord? {
    // The journal keeps a recovered receipt until its owner acknowledges it.
    if (recovered.containsKey(record.requestId)) return null
    val terminal = reconciledSafDestination(record, owns(record.requestId), nowMillis, requiresPublicationProof(record)) ?: return null
    save(terminal)
    if (!prepare(terminal)) return null
    recovered[terminal.requestId] = terminal
    return terminal
  }

  fun acknowledge(requestId: String, current: SafDestinationRecord?, clear: (String) -> Boolean): Boolean {
    if (!recovered.containsKey(requestId)) return false
    if (current?.requestId == requestId && !clear(requestId)) return false
    recovered.remove(requestId)
    return true
  }
}

internal fun acknowledgeSafDestinationExport(
  requestId: String,
  load: () -> SafDestinationRecord?,
  requiresPublicationProof: (SafDestinationRecord) -> Boolean,
  prepare: (SafDestinationRecord) -> Boolean,
  clear: (String) -> Boolean,
): Boolean {
  if (!isCanonicalUuidV4(requestId)) return false
  val record = load()
    ?.takeIf { it.requestId == requestId && it.isTerminal() }
    ?: return false
  if (
    record.exportBound && record.copyStarted && requiresPublicationProof(record) &&
    !record.publicationPrerequisitesComplete
  ) return false
  if (record.exportBound && record.copyStarted && !prepare(record)) return false
  return clear(requestId)
}

internal fun interruptedSafDestinationWarnings(): List<String> =
  listOf("android-saf-provider-not-atomic", "partial-destination-may-remain")

internal fun bindReservedSafDestination(
  record: SafDestinationRecord,
  requestId: String,
  exportId: String,
  nowMillis: Long,
): SafDestinationRecord? {
  if (!isCanonicalUuidV4(exportId) || record.requestId != requestId ||
    !record.reservedDestination || record.copyStarted ||
    record.phase != SafDestinationPhase.COPYING || record.destinationUri == null ||
    (record.exportBound && record.jobId != exportId)) return null
  return record.copy(jobId = exportId, exportId = exportId, exportBound = true, updatedAtMillis = nowMillis)
}

internal fun beginReservedSafCopy(
  record: SafDestinationRecord,
  requestId: String,
  jobId: String,
  exportId: String,
  sourceKind: SafDestinationSourceKind,
  nowMillis: Long,
): SafDestinationRecord? {
  if (!record.exportBound || record.sourceKind != sourceKind || !isCanonicalUuidV4(exportId)) return null
  return bindReservedSafDestination(record, requestId, jobId, nowMillis)
    ?.copy(copyStarted = true, exportId = exportId)
}

// A picker reservation proves access, not ownership of the returned document.
internal fun interruptedSafDestinationWarnings(record: SafDestinationRecord): List<String> =
  if (record.destinationUri == null) emptyList() else interruptedSafDestinationWarnings()

private data class NullableString(val value: String?)
private data class NullableLong(val value: Long?)

private fun optionalBooleanField(json: String, name: String, default: Boolean): Boolean? =
  if (json.contains("\"$name\"")) booleanField(json, name) else default

private fun isValidRecord(record: SafDestinationRecord): Boolean {
  if (!isCanonicalUuidV4(record.requestId) || !isCanonicalUuidV4(record.exportId)) return false
  if (record.jobId?.let { !isCanonicalUuidV4(it) } == true) return false
  if (record.reservedDestination && record.exportBound && record.jobId == null) return false
  if (!record.reservedDestination && (!record.exportBound || !record.copyStarted)) return false
  if (!record.exportBound && (record.exportId != record.requestId || record.copyStarted)) return false
  if (record.updatedAtMillis < 0 || record.bytes?.let { it < 0 } == true) return false
  if (record.code?.let { !SAFE_CODE.matches(it) } == true) return false
  if (record.warningCodes.size > 16 || record.warningCodes.any { !SAFE_CODE.matches(it) }) return false
  val uri = record.destinationUri
  if (
    uri != null &&
    (
      uri.length > MAX_DESTINATION_URI_CHARS ||
        !uri.startsWith("content://") ||
        uri.any { it == '"' || it == '\\' || it < ' ' }
      )
  ) return false
  if (record.phase == SafDestinationPhase.PICKING && uri != null) return false
  if (
    record.phase == SafDestinationPhase.COPYING &&
    uri == null
  ) return false
  return true
}

// Shared with SafFileBridge.kt's spool manifest parsing.
internal fun stringField(json: String, name: String): String? {
  val literal = Regex("\\\"$name\\\":\\\"((?:[^\\\"\\\\]|\\\\.)*)\\\"")
    .find(json)?.groupValues?.get(1) ?: return null
  val result = StringBuilder()
  var index = 0
  while (index < literal.length) {
    val char = literal[index++]
    if (char != '\\') { result.append(char); continue }
    if (index == literal.length) return null
    when (val escape = literal[index++]) {
      '"', '\\', '/' -> result.append(escape)
      'b' -> result.append('\b')
      'f' -> result.append('\u000c')
      'n' -> result.append('\n')
      'r' -> result.append('\r')
      't' -> result.append('\t')
      'u' -> {
        if (index + 4 > literal.length) return null
        val code = literal.substring(index, index + 4).toIntOrNull(16) ?: return null
        result.append(code.toChar())
        index += 4
      }
      else -> return null
    }
  }
  return result.toString()
}

private fun nullableStringField(json: String, name: String): NullableString? {
  val match = Regex("\\\"$name\\\":(null|\\\"([^\\\"]*)\\\")").find(json) ?: return null
  return NullableString(if (match.groupValues[1] == "null") null else match.groupValues[2])
}

internal fun numberField(json: String, name: String): Long? =
  Regex("\\\"$name\\\":([0-9]+)").find(json)?.groupValues?.get(1)?.toLongOrNull()

private fun booleanField(json: String, name: String): Boolean? =
  Regex("\\\"$name\\\":(true|false)").find(json)?.groupValues?.get(1)?.toBooleanStrictOrNull()

private fun nullableLongField(json: String, name: String): NullableLong? {
  val match = Regex("\\\"$name\\\":(null|[0-9]+)").find(json) ?: return null
  if (match.groupValues[1] == "null") return NullableLong(null)
  return match.groupValues[1].toLongOrNull()?.let(::NullableLong)
}

private fun warningCodesField(json: String): List<String>? {
  val body = Regex("\\\"warningCodes\\\":\\[([^]]*)]").find(json)?.groupValues?.get(1)
    ?: return null
  if (body.isEmpty()) return emptyList()
  return body.split(',').map { encoded ->
    Regex("\\\"([a-z0-9-]{1,64})\\\"").matchEntire(encoded)?.groupValues?.get(1)
      ?: return null
  }
}
