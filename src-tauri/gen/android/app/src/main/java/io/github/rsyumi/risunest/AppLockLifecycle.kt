package io.github.rsyumi.risunest

internal data class AppLockPolicy(
  val enabled: Boolean = false,
  val timing: String = "launch",
  val gateRequired: Boolean = false,
  val hideRecentsPreview: Boolean = false,
  val backgroundColor: String = "#111827",
) {
  val coversReturn get() = enabled && timing != "launch"
}

internal data class AppLockSnapshot(
  val active: Boolean,
  val epoch: Long,
  val atMs: Long,
  val departedAtMs: Long?,
)

internal fun appLockAuthStatus(apiLevel: Int, enrolled: Boolean): String = when {
  apiLevel < 29 -> "unsupported"
  enrolled -> "available"
  else -> "notEnrolled"
}

internal class AppLockLifecycle(private val now: () -> Long) {
  var policy = AppLockPolicy()
    private set
  var active = true
    private set
  var epoch = 0L
    private set
  var departedAtMs: Long? = null
    private set
  var pendingPresentation = false
    private set

  private var authenticationKeepsActive = false

  fun beginAuthentication(apiLevel: Int) {
    authenticationKeepsActive = apiLevel == 29
  }

  fun finishAuthentication() {
    authenticationKeepsActive = false
  }

  var hasWindowFocus = false
    private set

  fun windowFocusChanged(hasFocus: Boolean) {
    hasWindowFocus = hasFocus
  }

  val blocksBack get() = policy.gateRequired || pendingPresentation

  fun setPolicy(next: AppLockPolicy) {
    if (next.gateRequired && !policy.gateRequired) pendingPresentation = true
    policy = next
  }

  fun concealTransition(): Boolean {
    if (policy.coversReturn || policy.gateRequired) pendingPresentation = true
    return pendingPresentation
  }

  fun departUnlessTransient(
    ownedActivity: Boolean,
    permissionPrompt: Boolean = false,
    stopped: Boolean = false,
    changingConfigurations: Boolean = false,
  ): Boolean {
    if (ownedActivity || (permissionPrompt && !stopped) || changingConfigurations) return false
    return depart()
  }

  fun depart(): Boolean {
    if (authenticationKeepsActive) return false
    concealTransition()
    if (!active) return false
    active = false
    epoch += 1
    departedAtMs = now()
    return true
  }

  fun resume(): Boolean {
    if (active) return false
    active = true
    epoch += 1
    return true
  }

  fun acceptsPresentation(expectedEpoch: Long, locked: Boolean): Boolean =
    active && epoch == expectedEpoch && (!policy.gateRequired || locked)

  fun presented(expectedEpoch: Long, locked: Boolean): Boolean {
    if (!acceptsPresentation(expectedEpoch, locked)) return false
    policy = policy.copy(gateRequired = locked)
    pendingPresentation = false
    return true
  }

  fun acceptsAuthentication(expectedEpoch: Long): Boolean = active && epoch == expectedEpoch

  fun snapshot() = AppLockSnapshot(active, epoch, now(), departedAtMs)
}

internal class AppLockValueOverride<T>(private val read: () -> T, private val write: (T) -> Unit) {
  private var restoreValue: (() -> Unit)? = null

  fun update(enabled: Boolean, value: () -> T) {
    if (!enabled) { restore(); return }
    if (restoreValue == null) {
      val previous = read()
      restoreValue = { write(previous) }
    }
    write(value())
  }

  fun restore() {
    restoreValue?.invoke()
    restoreValue = null
  }
}
