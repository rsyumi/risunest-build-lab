package io.github.rsyumi.risunest

import org.junit.Assert.assertEquals
import org.junit.Test

class AndroidWebviewCleanupTest {
  @Test
  fun deletionRequiresCompletionAndRejectsConcurrentRequests() {
    var completion: (() -> Unit)? = null
    var calls = 0
    val cleanup = AndroidWebviewCleanup({ true }) { callback ->
      calls++
      completion = callback
    }
    assertEquals(0, cleanup.start())
    assertEquals(0, cleanup.status())
    assertEquals(-1, cleanup.start())
    assertEquals(1, calls)
    completion!!()
    assertEquals(1, cleanup.status())
  }

  @Test
  fun unsupportedWebviewDoesNotDeleteAnything() {
    var calls = 0
    val cleanup = AndroidWebviewCleanup({ false }) { calls++ }
    assertEquals(0, cleanup.supported())
    assertEquals(-1, cleanup.start())
    assertEquals(0, calls)
  }

  @Test
  fun deletionFailureIsNotSuccess() {
    val cleanup = AndroidWebviewCleanup({ true }) { throw IllegalStateException() }
    assertEquals(-1, cleanup.start())
    assertEquals(-1, cleanup.status())
  }
}
