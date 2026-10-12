package io.github.rsyumi.risunest

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID

internal fun copySelectedDocument(input: InputStream, output: OutputStream, checkActive: () -> Unit) {
  val buffer = ByteArray(256 * 1024)
  while (true) {
    checkActive()
    val count = input.read(buffer)
    if (count < 0) break
    output.write(buffer, 0, count)
  }
}

internal class NativeFileSelection(private val activity: ComponentActivity) {
  private val root get() = File(activity.cacheDir, "selected_documents")
  private var pending: CompletableDeferred<List<Uri>?>? = null
  private val launcher = activity.registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    val request = pending ?: return@registerForActivityResult
    when (result.resultCode) {
      Activity.RESULT_CANCELED -> request.complete(null)
      Activity.RESULT_OK -> {
        val data = result.data
        val clip = data?.clipData
        val uris = if (clip != null) (0 until clip.itemCount).map { clip.getItemAt(it).uri }
          else listOfNotNull(data?.data)
        if (uris.isEmpty()) request.completeExceptionally(IOException("The file picker returned no document"))
        else request.complete(uris)
      }
      else -> request.completeExceptionally(IOException("File picker failed (${result.resultCode})"))
    }
  }

  suspend fun pick(multiple: Boolean): JSONObject {
    check(pending == null) { "A file picker is already open" }
    val request = CompletableDeferred<List<Uri>?>()
    pending = request
    val staged = mutableListOf<File>()
    var delivered = false
    var failure: Throwable? = null
    try {
      launcher.launch(Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
        addCategory(Intent.CATEGORY_OPENABLE)
        type = "*/*"
        putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      })
      val uris = request.await() ?: return JSONObject().put("cancelled", true)
      val files = withContext(Dispatchers.IO) {
        val staleBefore = System.currentTimeMillis() - 24 * 60 * 60 * 1000L
        root.listFiles()?.filter { it.isDirectory && it.lastModified() < staleBefore }?.forEach {
          discard(File(it, "source").absolutePath)
        }
        val values = JSONArray()
        for (uri in if (multiple) uris else uris.take(1)) {
          currentCoroutineContext().ensureActive()
          require(uri.scheme == "content") { "The file picker returned an unsupported document URI" }
          val resolver = activity.contentResolver
          var name: String? = null
          resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) name = cursor.getString(0)
          }
          val displayName = name?.takeIf { it.isNotBlank() }
            ?: throw IOException("The selected document has no file name")
          val folder = File(root, UUID.randomUUID().toString())
          check(folder.mkdirs()) { "Could not create temporary document storage" }
          val file = File(folder, "source")
          staged.add(file)
          val context = currentCoroutineContext()
          resolver.openInputStream(uri)?.use { input ->
            file.outputStream().use { output ->
              copySelectedDocument(input, output) { context.ensureActive() }
            }
          } ?: throw IOException("Could not open the selected document")
          values.put(JSONObject().put("path", file.absolutePath).put("name", displayName)
            .put("bytes", file.length()).put("mimeType", resolver.getType(uri) ?: ""))
        }
        values
      }
      delivered = true
      return JSONObject().put("cancelled", false).put("files", files)
    } catch (error: Throwable) {
      failure = error
      throw error
    } finally {
      pending = null
      if (!delivered) withContext(NonCancellable + Dispatchers.IO) {
        val errors = staged.mapNotNull { runCatching { discard(it.absolutePath) }.exceptionOrNull() }
        if (errors.isNotEmpty()) throw IOException((listOfNotNull(failure) + errors).joinToString("\n") { it.toString() })
      }
    }
  }

  fun discard(path: String) {
    val file = File(path).canonicalFile
    require(file.name == "source" && file.parentFile?.parentFile == root.canonicalFile &&
      runCatching { UUID.fromString(file.parentFile!!.name) }.isSuccess) {
      "Invalid selected document path"
    }
    if (file.exists() && !file.delete()) throw IOException("Could not remove the temporary document")
    val folder = file.parentFile!!
    if (folder.exists() && !folder.delete()) throw IOException("Could not remove temporary document storage")
  }
}
