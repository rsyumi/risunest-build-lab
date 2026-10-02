package io.github.rsyumi.risunest

import android.content.ContentResolver
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.system.Os
import android.system.OsConstants
import java.io.InputStream
import java.io.IOException
import java.io.ByteArrayInputStream
import java.io.SequenceInputStream
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean

internal data class PortableSelected(val ready: SafSpoolReady, val sourceType: String)

internal class PortableSourceOwner {
  private val lock = Any()
  private var retired = false
  private val tokens = mutableSetOf<String>()

  fun isRetired(): Boolean = synchronized(lock) { retired }

  fun retain(token: String, register: () -> Unit) = synchronized(lock) {
    if (retired) throw IOException("source-reselect-required")
    register()
    tokens.add(token)
    Unit
  }

  fun released(token: String) { synchronized(lock) { tokens.remove(token) } }

  fun retire(discard: (String) -> Boolean) {
    val owned = synchronized(lock) { retired = true; tokens.toList() }
    owned.forEach { token -> if (runCatching { discard(token) }.getOrDefault(false)) released(token) }
  }
}

@androidx.annotation.Keep
internal object PortableSourceNative {
  @JvmStatic external fun register(token: String, fd: Int): Boolean
  @JvmStatic external fun discard(token: String): Boolean
  @JvmStatic external fun duplicateForCopy(token: String): Int
  @JvmStatic external fun verifyCopy(token: String): Boolean
  private data class Owned(
    val descriptor: ParcelFileDescriptor,
    val owner: PortableSourceOwner,
    val name: String,
    val bytes: Long,
    val cancelled: AtomicBoolean = AtomicBoolean(false),
    val copying: AtomicBoolean = AtomicBoolean(false),
    val copied: CountDownLatch = CountDownLatch(1),
  )
  private val selected = ConcurrentHashMap<String, Owned>()

  fun select(resolver: ContentResolver, uri: Uri, name: String, store: SafSpoolStore, owner: PortableSourceOwner, cancelled: () -> Boolean): PortableSelected {
    if (owner.isRetired()) throw IOException("source-reselect-required")
    val descriptor = resolver.openFileDescriptor(uri, "r")
      ?: throw IOException("source-unavailable")
    try {
      val info = Os.fstat(descriptor.fileDescriptor)
      val seekable = OsConstants.S_ISREG(info.st_mode) && info.st_size > 0 &&
        runCatching { Os.lseek(descriptor.fileDescriptor, 0, OsConstants.SEEK_SET) == 0L }.getOrDefault(false)
      if (!seekable) {
        val input = ParcelFileDescriptor.AutoCloseInputStream(descriptor)
        input.use {
          val prefix = ByteArray(11)
          var count = 0
          while (count < prefix.size) {
            val read = input.read(prefix, count, prefix.size - count)
            if (read < 0) break
            count += read
          }
          // ZIP sources require random access. Never spool their payload to obtain it.
          if (count >= 2 && prefix[0] == 0x50.toByte() && prefix[1] == 0x4b.toByte()) throw IOException("source-not-seekable")
          val wrapped = object : SafInputSource {
            override val displayName = name
            override val totalBytes: Long? = null
            override fun open(): InputStream = SequenceInputStream(ByteArrayInputStream(prefix, 0, count), input)
          }
          val copied = store.spool(listOf(wrapped), isCancelled = { cancelled() || owner.isRetired() })
          if (copied.failures.isNotEmpty() || copied.ready.size != 1) throw IOException(copied.failures.firstOrNull()?.code ?: "source-copy-failed")
          return PortableSelected(copied.ready.single(), "androidSpool")
        }
      }
      if (selected.size >= 16) throw IOException("source-busy")
      val token = UUID.randomUUID().toString()
      owner.retain(token) {
        selected[token] = Owned(descriptor, owner, name, info.st_size)
        if (!register(token, descriptor.fd)) {
          selected.remove(token)
          throw IOException("source-unavailable")
        }
      }
      return PortableSelected(SafSpoolReady(token, name, info.st_size, info.st_size), "androidSeekable")
    } catch (error: Exception) { descriptor.close(); throw error }
  }

  @JvmStatic fun releaseDescriptor(token: String): Boolean {
    val source = selected[token] ?: return true
    source.cancelled.set(true)
    if (source.copying.get()) source.copied.await()
    source.descriptor.close()
    selected.remove(token, source)
    source.owner.released(token)
    return true
  }

  fun retireUnclaimed(owner: PortableSourceOwner) { owner.retire { discard(it) } }

  fun materialize(token: String, store: SafSpoolStore): SafSpoolBatch {
    val source = selected[token] ?: throw IOException("source-reselect-required")
    if (!source.copying.compareAndSet(false, true)) throw IOException("source-busy")
    var batch: SafSpoolBatch? = null
    try {
      val fd = duplicateForCopy(token)
      if (fd < 0) throw IOException("source-format-not-confirmed")
      val input = ParcelFileDescriptor.AutoCloseInputStream(ParcelFileDescriptor.adoptFd(fd))
      val wrapped = object : SafInputSource {
        override val displayName = source.name
        override val totalBytes = source.bytes
        override fun open(): InputStream = input
      }
      input.use { batch = store.spool(listOf(wrapped), isCancelled = { source.cancelled.get() }) }
      if (!verifyCopy(token)) {
        batch!!.ready.forEach { store.discardReady(it.token) }
        throw IOException("source-changed")
      }
      return batch!!
    } finally {
      source.copied.countDown()
      if (!discard(token)) batch?.ready?.forEach { store.discardReady(it.token) }
    }
  }
}
