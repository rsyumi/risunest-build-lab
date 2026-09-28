package io.github.rsyumi.risunest

import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager
import java.util.UUID
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

internal const val GENERATION_FOREGROUND_START_MODE = Service.START_NOT_STICKY
private const val GENERATION_FOREGROUND_CHANNEL = "risunest-generation"
private const val GENERATION_FOREGROUND_NOTIFICATION_ID = 0x52474e31
private const val GENERATION_FOREGROUND_BEGIN = "io.github.rsyumi.risunest.GENERATION_FOREGROUND_BEGIN"
private const val GENERATION_FOREGROUND_TOKEN = "io.github.rsyumi.risunest.GENERATION_FOREGROUND_TOKEN"
private const val INVALID_GENERATION_FOREGROUND_TOKEN = -1L

internal class GenerationForegroundLifecycle {
  private var count = 0
  private var activeToken = INVALID_GENERATION_FOREGROUND_TOKEN
  private var activeStartId: Int? = null
  private var nextToken = 0L

  @Synchronized
  fun begin(dispatchStart: (Long) -> Boolean): Boolean {
    if (count > 0) {
      count += 1
      return true
    }

    nextToken += 1
    val token = nextToken
    if (!dispatchStart(token)) return false
    activeToken = token
    count = 1
    return true
  }

  @Synchronized
  fun end(stopService: () -> Boolean): Boolean {
    if (count == 0) return false
    count -= 1
    if (count > 0) return true

    activeToken = INVALID_GENERATION_FOREGROUND_TOKEN
    activeStartId = null
    return stopService()
  }

  @Synchronized
  fun stopAll(stopService: () -> Boolean): Boolean {
    if (count == 0) return false
    count = 0
    activeToken = INVALID_GENERATION_FOREGROUND_TOKEN
    activeStartId = null
    return stopService()
  }

  @Synchronized
  fun timeout(
    token: Long,
    startId: Int,
    stopTimedOut: () -> Unit,
    stopStaleTimeout: () -> Unit,
  ): Boolean {
    if (token != activeToken || startId != activeStartId) {
      stopStaleTimeout()
      return false
    }
    count = 0
    activeToken = INVALID_GENERATION_FOREGROUND_TOKEN
    activeStartId = null
    stopTimedOut()
    return true
  }

  @Synchronized
  fun serviceDestroyed(token: Long, startId: Int): Boolean {
    if (token != activeToken || startId != activeStartId) return false
    count = 0
    activeToken = INVALID_GENERATION_FOREGROUND_TOKEN
    activeStartId = null
    return true
  }

  @Synchronized
  fun activate(
    token: Long,
    startId: Int,
    startForeground: () -> Unit,
    stopStaleStart: () -> Unit,
  ): Boolean {
    if (count > 0 && token == activeToken) {
      activeStartId = startId
      startForeground()
      return true
    }
    stopStaleStart()
    return false
  }
}

internal data class BackgroundTaskStatus(val kind: String, val percent: Int = -1)

internal class BackgroundTaskRegistry {
  private val tasks = linkedMapOf<String, BackgroundTaskStatus>()
  private val expired = mutableMapOf<String, (String) -> Unit>()

  fun begin(kind: String, start: () -> Boolean, onExpired: (String) -> Unit): String? {
    if (kind !in setOf("backup", "restore", "sync", "import", "export", "maintenance")) return null
    if (!start()) return null
    val id = UUID.randomUUID().toString()
    tasks[id] = BackgroundTaskStatus(kind)
    expired[id] = onExpired
    return id
  }

  fun progress(id: String, percent: Int): Boolean {
    val previous = tasks[id] ?: return false
    if (percent !in -1..100) return false
    tasks[id] = previous.copy(percent = percent)
    return true
  }

  fun end(id: String, stop: () -> Boolean): Boolean {
    if (tasks.remove(id) == null) return false
    expired.remove(id)
    stop()
    return true
  }

  fun snapshot(): List<BackgroundTaskStatus> = tasks.values.toList()

  fun clear(notify: Boolean = false) {
    val callbacks = expired.toMap()
    tasks.clear()
    expired.clear()
    if (notify) callbacks.forEach { (id, callback) -> callback(id) }
  }
}

class GenerationForegroundService : Service() {
  private var wakeLock: PowerManager.WakeLock? = null
  private var activatedToken = INVALID_GENERATION_FOREGROUND_TOKEN
  private var activatedStartId: Int? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val token = if (intent?.action == GENERATION_FOREGROUND_BEGIN) {
      intent.getLongExtra(GENERATION_FOREGROUND_TOKEN, INVALID_GENERATION_FOREGROUND_TOKEN)
    } else {
      INVALID_GENERATION_FOREGROUND_TOKEN
    }
    lifecycle.activate(
      token = token,
      startId = startId,
      startForeground = {
        activatedToken = token
        activatedStartId = startId
        startInForeground()
      },
      stopStaleStart = {
        stopSelfResult(startId)
      },
    )
    return GENERATION_FOREGROUND_START_MODE
  }

  override fun onDestroy() {
    val destroyed = activatedStartId?.let { startId ->
      lifecycle.serviceDestroyed(activatedToken, startId)
    } == true
    if (instance === this) instance = null
    if (destroyed) background.clear(notify = true)
    releaseWakeLock()
    super.onDestroy()
  }

  override fun onTimeout(startId: Int, fgsType: Int) {
    lifecycle.timeout(
      token = activatedToken,
      startId = startId,
      stopTimedOut = {
        background.clear(notify = true)
        releaseWakeLock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelfResult(startId)
      },
      stopStaleTimeout = { stopSelfResult(startId) },
    )
  }

  private fun startInForeground() {
    instance = this
    createNotificationChannel()
    val openAppIntent = PendingIntent.getActivity(
      this,
      GENERATION_FOREGROUND_NOTIFICATION_ID,
      Intent(this, MainActivity::class.java).addFlags(
        Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP,
      ),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    val tasks = background.snapshot()
    if (tasks.isNotEmpty() && wakeLock == null) {
      wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "RisuNest:background-work").apply {
          setReferenceCounted(false)
          acquire(6 * 60 * 60 * 1000L)
        }
    } else if (tasks.isEmpty()) releaseWakeLock()
    val builder = androidx.core.app.NotificationCompat.Builder(this, GENERATION_FOREGROUND_CHANNEL)
        .setSmallIcon(android.R.drawable.stat_sys_download)
        .setContentTitle(getString(R.string.generation_notification_title))
        .setContentText(getString(R.string.generation_notification_text))
        .setContentIntent(openAppIntent)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
    if (tasks.isNotEmpty()) {
      val lines = tasks.map { task ->
        val title = getString(when (task.kind) {
          "backup" -> R.string.background_backup
          "restore" -> R.string.background_restore
          "sync" -> R.string.background_sync
          "import" -> R.string.background_import
          "export" -> R.string.background_export
          else -> R.string.background_maintenance
        })
        if (task.percent >= 0) "$title ${task.percent}%" else title
      }
      val percent = tasks.singleOrNull()?.percent ?: -1
      builder.setContentTitle(getString(R.string.background_notification_title))
        .setContentText(lines.joinToString(" · "))
        .setStyle(androidx.core.app.NotificationCompat.InboxStyle().also { style -> lines.forEach { style.addLine(it) } })
        .setProgress(100, maxOf(0, percent), percent < 0)
    }
    startForeground(GENERATION_FOREGROUND_NOTIFICATION_ID, builder.build())
  }

  private fun releaseWakeLock() {
    wakeLock?.let { if (it.isHeld) it.release() }
    wakeLock = null
  }

  private fun createNotificationChannel() {
    NotificationManagerCompat.from(this).createNotificationChannel(
      NotificationChannelCompat.Builder(
        GENERATION_FOREGROUND_CHANNEL,
        NotificationManagerCompat.IMPORTANCE_LOW,
      )
        .setName(getString(R.string.generation_notification_channel))
        .build(),
    )
  }

  companion object {
    private val lifecycle = GenerationForegroundLifecycle()
    private val background = BackgroundTaskRegistry()
    private var instance: GenerationForegroundService? = null

    internal fun beginTask(context: Context, kind: String, expired: (String) -> Unit): String? {
      val id = background.begin(kind, { start(context) }, expired)
      instance?.startInForeground()
      return id
    }

    internal fun taskProgress(id: String, percent: Int): Boolean {
      val updated = background.progress(id, percent)
      if (updated) instance?.startInForeground()
      return updated
    }

    internal fun endTask(context: Context, id: String): Boolean {
      val ended = background.end(id) { stop(context) }
      if (ended && background.snapshot().isNotEmpty()) instance?.startInForeground()
      else if (ended) instance?.releaseWakeLock()
      return ended
    }

    internal fun start(context: Context): Boolean = lifecycle.begin { token ->
      runCatching {
        ContextCompat.startForegroundService(
          context,
          Intent(context, GenerationForegroundService::class.java)
            .setAction(GENERATION_FOREGROUND_BEGIN)
            .putExtra(GENERATION_FOREGROUND_TOKEN, token),
        )
        true
      }.getOrDefault(false)
    }

    internal fun stop(context: Context): Boolean {
      var stopping = false
      val result = lifecycle.end {
        stopping = true
        runCatching { context.stopService(Intent(context, GenerationForegroundService::class.java)) }.getOrDefault(false)
      }
      if (!stopping) instance?.startInForeground()
      return result
    }

    internal fun stopAll(context: Context): Boolean {
      background.clear()
      instance?.releaseWakeLock()
      return lifecycle.stopAll {
        runCatching { context.stopService(Intent(context, GenerationForegroundService::class.java)) }.getOrDefault(false)
      }
    }

    internal fun notificationsEnabled(context: Context): Boolean {
      val manager = NotificationManagerCompat.from(context)
      if (!manager.areNotificationsEnabled()) return false
      val channel = manager.getNotificationChannelCompat(GENERATION_FOREGROUND_CHANNEL)
      return channel == null || channel.importance != NotificationManagerCompat.IMPORTANCE_NONE
    }
  }
}
