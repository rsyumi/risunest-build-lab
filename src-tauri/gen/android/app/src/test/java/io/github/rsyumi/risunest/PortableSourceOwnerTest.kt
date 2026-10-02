package io.github.rsyumi.risunest

import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PortableSourceOwnerTest {
  @Test fun oldActivityDestructionCannotDiscardNewActivitySelectionOrClaimedJobs() {
    val oldActivity = PortableSourceOwner()
    val newActivity = PortableSourceOwner()
    val live = mutableSetOf("old-selected", "old-claimed", "new-selected")
    oldActivity.retain("old-selected") {}
    oldActivity.retain("old-claimed") {}
    newActivity.retain("new-selected") {}
    val discarded = mutableListOf<String>()
    oldActivity.retire { token ->
      discarded.add(token)
      if (token == "old-claimed") false else live.remove(token)
    }
    assertEquals(listOf("old-selected", "old-claimed"), discarded)
    assertEquals(setOf("old-claimed", "new-selected"), live)
    assertFalse(newActivity.isRetired())
    newActivity.retire { live.remove(it) }
    assertEquals(setOf("old-claimed"), live)
  }

  @Test fun retiredActivityCannotRegisterLatePickerCompletion() {
    val activity = PortableSourceOwner()
    activity.retire { error("No selected source") }
    var registered = false
    try {
      activity.retain("late-selection") { registered = true }
      error("Retired owner accepted a descriptor")
    } catch (expected: IOException) {
      assertEquals("source-reselect-required", expected.message)
    }
    assertFalse(registered)
  }

  @Test fun retirementWaitsForItsOwnInFlightRegistrationThenDiscardsOnlyThatSource() {
    val oldActivity = PortableSourceOwner()
    val newActivity = PortableSourceOwner()
    val registering = CountDownLatch(1)
    val proceed = CountDownLatch(1)
    val pool = Executors.newFixedThreadPool(2)
    try {
      val selection = pool.submit {
        oldActivity.retain("old-in-flight") {
          registering.countDown()
          assertTrue(proceed.await(5, TimeUnit.SECONDS))
        }
      }
      assertTrue(registering.await(5, TimeUnit.SECONDS))
      newActivity.retain("new-selected") {}
      val retired = pool.submit<List<String>> {
        val discarded = mutableListOf<String>()
        oldActivity.retire { discarded.add(it); true }
        discarded
      }
      proceed.countDown()
      selection.get(5, TimeUnit.SECONDS)
      assertEquals(listOf("old-in-flight"), retired.get(5, TimeUnit.SECONDS))
      assertFalse(newActivity.isRetired())
      val newTokens = mutableListOf<String>()
      newActivity.retire { newTokens.add(it); true }
      assertEquals(listOf("new-selected"), newTokens)
    } finally {
      proceed.countDown()
      pool.shutdownNow()
    }
  }
}
