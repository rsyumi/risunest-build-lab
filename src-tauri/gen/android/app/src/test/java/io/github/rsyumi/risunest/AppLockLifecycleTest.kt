package io.github.rsyumi.risunest

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockLifecycleTest {
  @Test
  fun `republishing a presented gate does not replace it with a native cover`() {
    val state = AppLockLifecycle { 100L }
    val policy = AppLockPolicy(enabled = true, timing = "immediate", gateRequired = true)
    state.setPolicy(policy)
    assertTrue(state.pendingPresentation)
    assertTrue(state.presented(state.epoch, locked = true))
    state.setPolicy(policy)
    assertFalse(state.pendingPresentation)
    assertTrue(state.blocksBack)
    state.setPolicy(policy.copy(backgroundColor = "#ffffff"))
    assertFalse(state.pendingPresentation)
    state.concealTransition()
    state.setPolicy(policy)
    assertTrue(state.pendingPresentation)
  }

  @Test
  fun `return cannot expose content until current presentation completes`() {
    var time = 1_000L
    val state = AppLockLifecycle { time }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "immediate"))
    val staleEpoch = state.epoch
    assertTrue(state.depart())
    assertTrue(state.blocksBack)
    time = 90_000L
    assertTrue(state.resume())
    assertEquals(1_000L, state.snapshot().departedAtMs)
    assertEquals(90_000L, state.snapshot().atMs)
    assertFalse(state.presented(staleEpoch, locked = false))
    state.setPolicy(state.policy.copy(gateRequired = true))
    assertFalse(state.presented(state.epoch, locked = false))
    assertTrue(state.presented(state.epoch, locked = true))
    assertTrue(state.blocksBack)
    state.setPolicy(state.policy.copy(gateRequired = false))
    assertFalse(state.blocksBack)
  }

  @Test
  fun `a later departure invalidates both presentation and native unlock results`() {
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "after-1m"))
    val authEpoch = state.epoch
    state.depart()
    state.resume()
    val returningEpoch = state.epoch
    state.depart()
    assertFalse(state.acceptsAuthentication(authEpoch))
    assertFalse(state.presented(returningEpoch, locked = true))
    assertTrue(state.blocksBack)
    state.resume()
    assertTrue(state.presented(state.epoch, locked = false))
    assertFalse(state.blocksBack)
  }

  @Test
  fun `launch-only mode leaves an unlocked app uncovered on later departures`() {
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true))
    state.depart()
    state.resume()
    assertFalse(state.pendingPresentation)
    assertFalse(state.blocksBack)
  }

  @Test
  fun `app-owned transient focus changes conceal without starting a departure`() {
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "immediate"))
    val epoch = state.epoch
    assertTrue(state.concealTransition())
    assertTrue(state.active)
    assertEquals(epoch, state.epoch)
    assertEquals(null, state.departedAtMs)
    assertTrue(state.acceptsAuthentication(epoch))
    assertTrue(state.presented(epoch, locked = false))
    assertFalse(state.blocksBack)
  }

  @Test
  fun `repeated paused signals retain the original away time`() {
    var time = 100L
    val state = AppLockLifecycle { time }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "after-5m"))
    state.depart()
    val departedEpoch = state.epoch
    time = 5_000L
    assertFalse(state.depart())
    assertEquals(100L, state.departedAtMs)
    assertEquals(departedEpoch, state.epoch)
  }
}
