package io.github.rsyumi.risunest

import android.content.Context

/**
 * Lets native HTTP and WebSocket connections verify servers through Android,
 * so they trust the CAs the network security config allows.
 */
internal object PlatformTls {
  init {
    System.loadLibrary("risunest_lib")
  }

  @JvmStatic external fun initialize(context: Context)
}
