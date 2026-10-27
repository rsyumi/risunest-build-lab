package io.github.rsyumi.risunest

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppLockLifecycleTest {
  @Test
  fun `native unlock is unavailable through Android 9 even with an enrolled credential`() {
    for (apiLevel in 24..28) {
      assertEquals("unsupported", appLockAuthStatus(apiLevel, enrolled = false))
      assertEquals("unsupported", appLockAuthStatus(apiLevel, enrolled = true))
    }
    assertEquals("notEnrolled", appLockAuthStatus(29, enrolled = false))
    assertEquals("available", appLockAuthStatus(29, enrolled = true))
    assertEquals("available", appLockAuthStatus(36, enrolled = true))
  }

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
    assertFalse(state.presented(staleEpoch, locked = true))
    assertFalse(state.policy.gateRequired)
    assertTrue(state.presented(state.epoch, locked = true))
    assertTrue(state.blocksBack)
    assertFalse(state.presented(state.epoch, locked = false))
    state.setPolicy(state.policy.copy(gateRequired = false))
    assertTrue(state.presented(state.epoch, locked = false))
    assertFalse(state.blocksBack)
  }

  @Test
  fun `a later departure invalidates both presentation and native unlock results`() {
    for (apiLevel in listOf(29, 30, 36)) {
      var now = 100L
      val pending = AppLockLifecycle { now }
      pending.setPolicy(AppLockPolicy(enabled = true, timing = "immediate", gateRequired = true))
      val requestEpoch = pending.epoch
      pending.beginAuthentication(apiLevel)
      assertEquals(apiLevel != 29, pending.departUnlessTransient(ownedActivity = false, stopped = true))
      assertEquals(apiLevel == 29, pending.acceptsAuthentication(requestEpoch))
      if (apiLevel == 29) {
        now = 90_000L
        assertFalse(pending.depart())
        assertFalse(pending.resume())
        assertEquals(requestEpoch, pending.epoch)
        assertEquals(null, pending.departedAtMs)
        assertTrue(pending.acceptsAuthentication(requestEpoch))
        assertTrue(pending.blocksBack)
      } else {
        assertTrue(pending.resume())
        assertFalse(pending.acceptsAuthentication(requestEpoch))
      }
      pending.finishAuthentication()
      assertTrue(pending.depart())
      assertEquals(now, pending.departedAtMs)
      assertFalse(pending.acceptsAuthentication(requestEpoch))
    }
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "after-1m"))
    val authEpoch = state.epoch
    state.depart()
    assertFalse(state.acceptsAuthentication(authEpoch))
    state.resume()
    assertFalse(state.acceptsAuthentication(authEpoch))
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
  fun `owned picker time is excluded but an observed task departure still invalidates results`() {
    var time = 100L
    val state = AppLockLifecycle { time }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "after-1m"))
    val epoch = state.epoch
    assertTrue(state.concealTransition())
    assertFalse(state.departUnlessTransient(ownedActivity = true, stopped = true))
    time = 90_000L
    assertFalse(state.departUnlessTransient(ownedActivity = true))
    assertFalse(state.resume())
    assertTrue(state.active)
    assertEquals(epoch, state.epoch)
    assertEquals(null, state.snapshot().departedAtMs)
    assertEquals(time, state.snapshot().atMs)
    assertTrue(state.acceptsAuthentication(epoch))
    assertTrue(state.presented(epoch, locked = false))
    assertFalse(state.blocksBack)
    assertTrue(state.depart())
    assertEquals(time, state.departedAtMs)
    assertFalse(state.acceptsAuthentication(epoch))
    assertFalse(state.presented(epoch, locked = false))
    state.resume()
    assertFalse(state.acceptsAuthentication(epoch))
    assertFalse(state.presented(epoch, locked = false))
  }

  @Test
  fun `foreground focus loss leaves the current frame visible without an away transition`() {
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "immediate"))
    state.windowFocusChanged(true)
    val epoch = state.epoch
    state.windowFocusChanged(false)
    assertFalse(state.hasWindowFocus)
    assertTrue(state.active)
    assertEquals(epoch, state.epoch)
    assertEquals(null, state.departedAtMs)
    assertFalse(state.pendingPresentation)
    assertFalse(state.blocksBack)
    state.windowFocusChanged(true)
    assertTrue(state.hasWindowFocus)
    assertTrue(state.acceptsAuthentication(epoch))
    state.depart()
    state.windowFocusChanged(false)
    assertTrue(state.pendingPresentation)
    assertFalse(state.acceptsAuthentication(epoch))
  }

  @Test
  fun `owned permission continuation does not lock but a real departure invalidates its old epoch`() {
    val state = AppLockLifecycle { 100L }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "immediate"))
    val epoch = state.epoch
    state.concealTransition()
    assertFalse(state.departUnlessTransient(ownedActivity = false, permissionPrompt = true))
    assertFalse(state.resume())
    assertEquals(null, state.departedAtMs)
    assertTrue(state.presented(epoch, locked = false))
    assertFalse(state.pendingPresentation)
    assertTrue(state.departUnlessTransient(ownedActivity = false, permissionPrompt = true, stopped = true))
    assertFalse(state.acceptsAuthentication(epoch))
    state.resume()
    assertFalse(state.acceptsAuthentication(epoch))
    assertFalse(state.presented(epoch, locked = false))
  }

  @Test
  fun `native surface overrides are scoped and restore the exact prior value`() {
    val original = Any()
    var surface: Any? = original
    val override = AppLockValueOverride({ surface }, { surface = it })
    override.update(false) { error("Disabled app lock must not apply a surface value") }
    assertTrue(surface === original)
    val first = Any()
    override.update(true) { first }
    assertTrue(surface === first)
    val changed = Any()
    override.update(true) { changed }
    assertTrue(surface === changed)
    override.update(false) { error("Disabling must restore, not replace") }
    assertTrue(surface === original)
    surface = null
    override.update(true) { first }
    override.restore()
    assertEquals(null, surface)
    override.restore()
    assertEquals(null, surface)
  }

  @Test
  fun `repeated paused signals retain the original away time`() {
    var time = 100L
    val state = AppLockLifecycle { time }
    state.setPolicy(AppLockPolicy(enabled = true, timing = "after-5m"))
    assertTrue(state.departUnlessTransient(ownedActivity = false))
    val departedEpoch = state.epoch
    time = 5_000L
    assertFalse(state.departUnlessTransient(ownedActivity = false))
    assertEquals(100L, state.departedAtMs)
    assertEquals(departedEpoch, state.epoch)
  }
}
