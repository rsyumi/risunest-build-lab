package io.github.rsyumi.risunest

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import java.util.Locale

internal fun androidExportMimeType(filename: String): String = when (
  filename.substringAfterLast('.', "").lowercase(Locale.ROOT)
) {
  "png" -> "image/png"
  "jpg", "jpeg" -> "image/jpeg"
  "webp" -> "image/webp"
  "zip" -> "application/zip"
  "json" -> "application/json"
  "txt" -> "text/plain"
  "html", "htm" -> "text/html"
  "risum", "risup", "charx", "risunest", "risudat" -> "application/x-risunest"
  else -> "application/octet-stream"
}

/** Copies may start immediately; only delivery waits for the actual app document. */
internal class AndroidFrontendReady {
  private val ready = CompletableDeferred<Unit>()
  @Volatile private var cancelled = false
  val isReady: Boolean get() = !cancelled && ready.isCompleted && !ready.isCancelled

  fun markReady(): Boolean = !cancelled && ready.complete(Unit)
  fun cancel() {
    cancelled = true
    ready.cancel()
  }
  suspend fun await() {
    ready.await()
    if (cancelled) throw CancellationException("Android frontend retired")
  }
}
