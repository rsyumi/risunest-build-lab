// RisuNest: a custom-protocol response body read from native memory one piece
// at a time, so a large body never becomes one Java array.

@file:Suppress("unused")

package {{package}}

import java.io.IOException
import java.io.InputStream

class RustResponseStream(private var handle: Long, private val length: Long) : InputStream() {
    private var position = 0L

    @Synchronized
    override fun read(): Int {
        val one = ByteArray(1)
        return if (read(one, 0, 1) == 1) one[0].toInt() and 0xff else -1
    }

    @Synchronized
    override fun read(buffer: ByteArray, offset: Int, count: Int): Int {
        if (offset < 0 || count < 0 || count > buffer.size - offset) {
            throw IndexOutOfBoundsException()
        }
        if (handle == 0L) throw IOException("Stream closed")
        if (count == 0) return 0
        val read = Rust.responseStreamRead(handle, buffer, offset, count)
        if (read > 0) position += read
        return read
    }

    @Synchronized
    override fun available(): Int {
        if (handle == 0L) return 0
        return minOf(length - position, Int.MAX_VALUE.toLong()).toInt()
    }

    override fun close() {
        release()
    }

    // The WebView normally closes the stream; this frees a stream it drops.
    protected fun finalize() {
        release()
    }

    @Synchronized
    private fun release() {
        val current = handle
        if (current == 0L) return
        handle = 0L
        Rust.responseStreamRelease(current)
    }
}
