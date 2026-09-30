package io.github.rsyumi.risunest

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

internal object CompletionNotifications {
  private const val CHANNEL = "risunest-completion"
  private const val ID = 0x52474e32

  fun enabled(context: Context): Boolean {
    val manager = NotificationManagerCompat.from(context)
    if (!manager.areNotificationsEnabled()) return false
    val channel = manager.getNotificationChannelCompat(CHANNEL)
    return channel == null || channel.importance != NotificationManagerCompat.IMPORTANCE_NONE
  }

  fun post(context: Context, body: String): Boolean {
    if (!enabled(context)) return false
    val manager = NotificationManagerCompat.from(context)
    manager.createNotificationChannel(
      NotificationChannelCompat.Builder(CHANNEL, NotificationManagerCompat.IMPORTANCE_DEFAULT)
        .setName(context.getString(R.string.completion_notification_channel)).build(),
    )
    val open = PendingIntent.getActivity(context, ID,
      Intent(context, MainActivity::class.java).addFlags(
        Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP,
      ), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    manager.notify(ID, NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(android.R.drawable.stat_notify_chat)
      .setContentTitle(context.getString(R.string.app_name))
      .setContentText(body.take(4096))
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(open)
      .setAutoCancel(true)
      .build())
    return true
  }
}
