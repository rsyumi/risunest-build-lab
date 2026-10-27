package io.github.rsyumi.risunest

import android.app.ActivityManager
import android.app.KeyguardManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Color
import android.graphics.Outline
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.Drawable
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.ViewOutlineProvider
import android.view.ViewTreeObserver
import android.webkit.WebView
import android.widget.FrameLayout
import android.widget.ImageView
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import org.json.JSONObject

private const val APP_LOCK_EVENT = "risunest-app-lock"
private const val TASK_VISIBILITY_POLL_MS = 200L

internal class AndroidAppLock(
  private val activity: MainActivity,
  private val reply: (Long, String) -> Unit,
) {
  private val handler = Handler(Looper.getMainLooper())
  private val lifecycle = AppLockLifecycle(SystemClock::elapsedRealtime)
  private var webView: WebView? = null
  private var cover: FrameLayout? = null
  private var prompt: BiometricPrompt? = null
  private var authRequest: Pair<Long, Long>? = null
  private var pendingAuthOutcome: String? = null
  private var pendingPromptReason: String? = null
  private var presentationRequest: Long? = null
  private val ownedResults = mutableSetOf<Int>()
  private var resumed = false
  private val focused get() = lifecycle.hasWindowFocus
  private val ownedPermissions = mutableSetOf<Int>()
  private val windowBackground = AppLockValueOverride<Drawable?>(
    { activity.window.decorView.background }, { activity.window.setBackgroundDrawable(it) },
  )
  private var webAccessibility: AppLockValueOverride<Int>? = null
  private var registered = false
  private var visualSequence = 0L
  private var closed = false
  private val authenticatorTypes = BiometricManager.Authenticators.BIOMETRIC_WEAK or
    BiometricManager.Authenticators.DEVICE_CREDENTIAL

  private val screenReceiver = object : BroadcastReceiver() {
    override fun onReceive(context: Context?, intent: Intent?) {
      if (intent?.action == Intent.ACTION_SCREEN_OFF) depart()
    }
  }

  private val taskVisibilityPoll = object : Runnable {
    override fun run() {
      if (closed || ownedResults.isEmpty() || Build.VERSION.SDK_INT < 32) return
      if (!isTaskVisible()) depart()
      else if (lifecycle.resume()) dispatch()
      handler.postDelayed(this, TASK_VISIBILITY_POLL_MS)
    }
  }

  fun attach(view: WebView) {
    invalidatePresentation()
    webAccessibility?.restore()
    webView = view
    webAccessibility = AppLockValueOverride({ view.importantForAccessibility }, { view.importantForAccessibility = it })
    if (!registered) {
      ContextCompat.registerReceiver(activity, screenReceiver,
        IntentFilter(Intent.ACTION_SCREEN_OFF), ContextCompat.RECEIVER_NOT_EXPORTED)
      registered = true
    }
    if (lifecycle.pendingPresentation || cover != null) showCover()
  }

  fun command(id: Long, operation: String, payload: String) {
    handler.post {
      if (closed) {
        reply(id, JSONObject().put("error", "app-lock-activity-closed").toString())
        return@post
      }
      try {
        when (operation) {
          "check" -> complete(id, capabilities())
          "authenticate" -> authenticate(id, JSONObject(payload).getString("reason"))
          "setPolicy" -> {
            val value = JSONObject(payload)
            val next = AppLockPolicy(
              enabled = value.getBoolean("enabled"), timing = value.getString("timing"),
              gateRequired = value.getBoolean("gateRequired"),
              hideRecentsPreview = value.getBoolean("hideRecentsPreview"),
              backgroundColor = value.getString("backgroundColor"),
            )
            if (next.gateRequired != lifecycle.policy.gateRequired) invalidatePresentation()
            lifecycle.setPolicy(next)
            if (Build.VERSION.SDK_INT >= 33) activity.setRecentsScreenshotEnabled(!lifecycle.policy.hideRecentsPreview)
            windowBackground.update(Build.VERSION.SDK_INT >= 33 && lifecycle.policy.hideRecentsPreview) {
              ColorDrawable(backgroundColor())
            }
            if (lifecycle.pendingPresentation) showCover()
            complete(id, snapshot())
          }
          "ready" -> {
            val value = JSONObject(payload)
            present(id, value.getLong("epoch"), value.getBoolean("locked"))
          }
          else -> error("Unknown app-lock command")
        }
      } catch (_: Exception) {
        reply(id, JSONObject().put("error", "app-lock-native-failed").toString())
      }
    }
  }

  private fun capabilities(): JSONObject {
    val enrolled = Build.VERSION.SDK_INT >= 29 &&
      (activity.getSystemService(KeyguardManager::class.java).isDeviceSecure ||
        BiometricManager.from(activity).canAuthenticate(authenticatorTypes) == BiometricManager.BIOMETRIC_SUCCESS)
    val status = appLockAuthStatus(Build.VERSION.SDK_INT, enrolled)
    return JSONObject().put("auth", JSONObject()
      .put("status", status)
      .apply { if (status == "available") put("method", "device") })
      .put("previewSupported", Build.VERSION.SDK_INT >= 33)
  }

  private fun authenticate(id: Long, reason: String) {
    if (authRequest != null) { complete(id, "cancelled"); return }
    if (!lifecycle.active) { complete(id, "cancelled"); return }
    if (capabilities().getJSONObject("auth").getString("status") != "available") {
      complete(id, "unavailable")
      return
    }
    authRequest = id to lifecycle.epoch
    lifecycle.beginAuthentication(Build.VERSION.SDK_INT)
    pendingPromptReason = reason
    startPendingPrompt()
  }

  private fun startPendingPrompt() {
    val reason = pendingPromptReason ?: return
    val request = authRequest ?: return
    if (!lifecycle.acceptsAuthentication(request.second)) { finishAuthentication("cancelled"); return }
    if (!resumed || !focused) return
    pendingPromptReason = null
    val callback = object : BiometricPrompt.AuthenticationCallback() {
      override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = finishAuthentication("success")
      override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
        val outcome = when (errorCode) {
          BiometricPrompt.ERROR_CANCELED, BiometricPrompt.ERROR_USER_CANCELED,
          BiometricPrompt.ERROR_NEGATIVE_BUTTON -> "cancelled"
          BiometricPrompt.ERROR_NO_BIOMETRICS, BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL,
          BiometricPrompt.ERROR_HW_NOT_PRESENT -> "unavailable"
          else -> "failed"
        }
        finishAuthentication(outcome)
      }
    }
    prompt = BiometricPrompt(activity, ContextCompat.getMainExecutor(activity), callback)
    try {
      prompt!!.authenticate(BiometricPrompt.PromptInfo.Builder()
        .setTitle(reason).setAllowedAuthenticators(authenticatorTypes).build())
    } catch (_: Exception) { finishAuthentication("failed") }
  }

  private fun finishAuthentication(outcome: String) {
    val request = authRequest ?: return
    if ((outcome == "success" || Build.VERSION.SDK_INT == 29) &&
      lifecycle.acceptsAuthentication(request.second) && (!resumed || !focused)) {
      pendingAuthOutcome = outcome
      prompt = null
      return
    }
    pendingAuthOutcome = null
    pendingPromptReason = null
    authRequest = null
    lifecycle.finishAuthentication()
    prompt = null
    complete(request.first, if (lifecycle.acceptsAuthentication(request.second)) outcome else "cancelled")
    if (resumed && focused) dispatch()
  }

  fun onPause() {
    resumed = false
    conceal()
    departUnlessTransient(activity.isChangingConfigurations)
  }

  fun onResume() {
    resumed = true
    lifecycle.resume()
    if (lifecycle.pendingPresentation) showCover()
    if (focused) pendingAuthOutcome?.let(::finishAuthentication)
    startPendingPrompt()
    dispatch()
  }

  fun onWindowFocusChanged(hasFocus: Boolean) {
    lifecycle.windowFocusChanged(hasFocus)
    if (!hasFocus) invalidatePresentation()
    else if (resumed) {
      lifecycle.resume()
      pendingAuthOutcome?.let(::finishAuthentication)
      startPendingPrompt()
      dispatch()
    }
  }

  fun onUserLeaveHint() {
    departUnlessTransient()
  }

  fun onStop() {
    departUnlessTransient(activity.isChangingConfigurations, stopped = true)
  }

  fun onTopResumedActivityChanged(isTop: Boolean) {
    if (!isTop) departUnlessTransient(activity.isChangingConfigurations)
    else if (resumed && focused) {
      lifecycle.resume()
      dispatch()
    }
  }

  fun beginOwnedActivity(requestCode: Int) {
    if (requestCode < 0) return
    ownedResults.add(requestCode)
    handler.removeCallbacks(taskVisibilityPoll)
    handler.post(taskVisibilityPoll)
  }

  fun finishOwnedActivity(requestCode: Int) {
    ownedResults.remove(requestCode)
    if (ownedResults.isEmpty()) handler.removeCallbacks(taskVisibilityPoll)
  }

  fun beginOwnedPermission(requestCode: Int) {
    ownedPermissions.add(requestCode)
  }

  fun finishOwnedPermission(requestCode: Int) {
    ownedPermissions.remove(requestCode)
  }

  fun backgroundForBack(): Boolean {
    if (!lifecycle.blocksBack) return false
    conceal()
    depart()
    activity.moveTaskToBack(true)
    return true
  }

  private fun isTaskVisible(): Boolean {
    if (Build.VERSION.SDK_INT < 32) return true
    if (!activity.getSystemService(PowerManager::class.java).isInteractive ||
      activity.getSystemService(KeyguardManager::class.java).isKeyguardLocked) return false
    val tasks = activity.getSystemService(ActivityManager::class.java).appTasks
    return tasks.any { it.taskInfo.taskId == activity.taskId && it.taskInfo.isVisible }
  }

  private fun departUnlessTransient(changingConfigurations: Boolean = false, stopped: Boolean = false) {
    if (lifecycle.departUnlessTransient(ownedResults.isNotEmpty(), ownedPermissions.isNotEmpty(), stopped, changingConfigurations)) {
      conceal()
      dispatch()
    }
  }

  private fun depart() {
    conceal()
    if (lifecycle.depart()) dispatch()
  }

  private fun conceal() {
    invalidatePresentation()
    if (lifecycle.concealTransition()) showCover()
  }

  private fun backgroundColor(): Int = runCatching { Color.parseColor(lifecycle.policy.backgroundColor) }
    .getOrDefault(Color.rgb(17, 24, 39))

  private fun showCover() {
    val root = activity.findViewById<ViewGroup>(android.R.id.content) ?: return
    webAccessibility?.update(true) { View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS }
    val existing = cover
    if (existing != null) {
      existing.setBackgroundColor(backgroundColor())
      existing.bringToFront()
      return
    }
    val density = activity.resources.displayMetrics.density
    val surface = FrameLayout(activity).apply {
      setBackgroundColor(backgroundColor())
      isClickable = true
      isFocusableInTouchMode = true
      importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
      elevation = 100_000f
    }
    val icon = ImageView(activity).apply {
      setImageResource(R.drawable.app_lock_logo)
      scaleType = ImageView.ScaleType.FIT_CENTER
      outlineProvider = object : ViewOutlineProvider() {
        override fun getOutline(view: View, outline: Outline) {
          outline.setRoundRect(0, 0, view.width, view.height, 14f * density)
        }
      }
      clipToOutline = true
    }
    surface.addView(icon, FrameLayout.LayoutParams((56 * density).toInt(), (56 * density).toInt(), Gravity.CENTER))
    root.addView(surface, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
    cover = surface
    surface.requestFocus()
  }

  private fun present(id: Long, epoch: Long, locked: Boolean) {
    val view = webView
    if (view == null || !lifecycle.acceptsPresentation(epoch, locked) || !resumed || !focused) {
      complete(id, false)
      return
    }
    invalidatePresentation()
    presentationRequest = id
    val sequence = ++visualSequence
    // Remove the sibling cover only when the WebView can draw the acknowledged frame.
    view.postVisualStateCallback(sequence, object : WebView.VisualStateCallback() {
      override fun onComplete(requestId: Long) {
        if (closed || webView !== view || sequence != visualSequence ||
          !lifecycle.acceptsPresentation(epoch, locked) || !resumed || !focused) {
          finishPresentation(id, false)
          return
        }
        val observer = view.viewTreeObserver
        observer.addOnPreDrawListener(object : ViewTreeObserver.OnPreDrawListener {
          override fun onPreDraw(): Boolean {
            if (view.viewTreeObserver.isAlive) view.viewTreeObserver.removeOnPreDrawListener(this)
            val accepted = !closed && webView === view && sequence == visualSequence &&
              resumed && focused && lifecycle.presented(epoch, locked)
            if (accepted) {
              cover?.let { (it.parent as? ViewGroup)?.removeView(it) }
              cover = null
              webAccessibility?.restore()
              view.requestFocus()
            }
            finishPresentation(id, accepted)
            return true
          }
        })
        view.invalidate()
      }
    })
  }

  private fun invalidatePresentation() {
    visualSequence += 1
    presentationRequest?.let { complete(it, false) }
    presentationRequest = null
  }

  private fun finishPresentation(id: Long, accepted: Boolean) {
    if (presentationRequest != id) return
    presentationRequest = null
    complete(id, accepted)
  }

  private fun snapshot(): JSONObject = lifecycle.snapshot().let {
    JSONObject().put("active", it.active).put("epoch", it.epoch).put("atMs", it.atMs)
      .put("departedAtMs", it.departedAtMs ?: JSONObject.NULL)
  }

  private fun dispatch() {
    val view = webView ?: return
    view.evaluateJavascript("window.dispatchEvent(new CustomEvent('$APP_LOCK_EVENT',{detail:${snapshot()}}))", null)
  }

  private fun complete(id: Long, value: Any) = reply(id, JSONObject().put("value", value).toString())

  fun close() {
    closed = true
    invalidatePresentation()
    handler.removeCallbacksAndMessages(null)
    if (registered) activity.unregisterReceiver(screenReceiver)
    registered = false
    authRequest?.let { complete(it.first, "cancelled") }
    authRequest = null
    pendingAuthOutcome = null
    pendingPromptReason = null
    prompt?.cancelAuthentication()
    prompt = null
    cover?.let { (it.parent as? ViewGroup)?.removeView(it) }
    cover = null
    webAccessibility?.restore()
    webAccessibility = null
    windowBackground.restore()
    webView = null
  }
}
