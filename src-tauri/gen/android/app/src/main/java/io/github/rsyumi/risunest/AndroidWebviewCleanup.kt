package io.github.rsyumi.risunest

import android.webkit.WebStorage
import androidx.webkit.WebStorageCompat
import androidx.webkit.WebViewFeature

internal class AndroidWebviewCleanup(
  private val supportsDeletion: () -> Boolean = {
    WebViewFeature.isFeatureSupported(WebViewFeature.DELETE_BROWSING_DATA)
  },
  private val deleteData: (() -> Unit) -> Unit = { completed ->
    WebStorageCompat.deleteBrowsingData(WebStorage.getInstance()) { completed() }
  },
) {
  private var status = -1

  fun supported(): Int =
    if (supportsDeletion()) 1 else 0

  fun start(): Int {
    if (status == 0 || supported() != 1) return -1
    status = 0
    return try {
      deleteData { status = 1 }
      0
    } catch (_: Exception) {
      status = -1
      -1
    }
  }

  fun status(): Int = status
}
