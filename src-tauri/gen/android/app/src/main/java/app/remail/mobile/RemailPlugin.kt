package app.remail.mobile

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.FileProvider
import androidx.core.view.WindowCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class FileArgs {
  lateinit var path: String
  lateinit var name: String
  lateinit var mime: String
}

@InvokeArg
class BarsArgs {
  var dark: Boolean = false
  lateinit var background: String
}

/** Native pieces for the Rust side (src-tauri/src/mobile.rs) and the UI. */
@TauriPlugin
class RemailPlugin(private val activity: Activity) : Plugin(activity) {
  private companion object {
    const val KEY_ALIAS = "remail-credentials-wrap"
    const val PREFS = "remail-keystore"
    const val WRAPPED = "credentials-key"
  }

  /** The Keystore key that wraps the credentials key. It never leaves the Keystore. */
  private fun wrapKey(): SecretKey {
    val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (ks.getEntry(KEY_ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
    val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    gen.init(
      KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .build()
    )
    return gen.generateKey()
  }

  private fun newCredentialsKey(): ByteArray {
    val key = ByteArray(32).also { SecureRandom().nextBytes(it) }
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, wrapKey())
    val wrapped = cipher.iv + cipher.doFinal(key)
    activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
      .putString(WRAPPED, Base64.encodeToString(wrapped, Base64.NO_WRAP)).commit()
    return key
  }

  /** The 32-byte key sealing Remail's credentials file, created on first use. */
  @Command
  fun credentialsKey(invoke: Invoke) {
    try {
      val stored = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(WRAPPED, null)
      val key = stored?.let {
        try {
          val blob = Base64.decode(it, Base64.NO_WRAP)
          val cipher = Cipher.getInstance("AES/GCM/NoPadding")
          cipher.init(Cipher.DECRYPT_MODE, wrapKey(), GCMParameterSpec(128, blob, 0, 12))
          cipher.doFinal(blob, 12, blob.size - 12)
        } catch (e: Exception) {
          null // The Keystore entry was reset: the old credentials are gone.
        }
      } ?: newCredentialsKey()
      invoke.resolve(JSObject().put("key", Base64.encodeToString(key, Base64.NO_WRAP)))
    } catch (e: Exception) {
      invoke.reject("Keystore unavailable: ${e.message}")
    }
  }

  /** Copy a file to the public Downloads folder. */
  @Command
  fun saveToDownloads(invoke: Invoke) {
    val args = invoke.parseArgs(FileArgs::class.java)
    try {
      val src = File(args.path)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        val values = ContentValues().apply {
          put(MediaStore.MediaColumns.DISPLAY_NAME, args.name)
          put(MediaStore.MediaColumns.MIME_TYPE, args.mime)
          put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
        }
        val resolver = activity.contentResolver
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
          ?: throw IllegalStateException("Downloads is unavailable")
        resolver.openOutputStream(uri).use { out -> src.inputStream().use { it.copyTo(out!!) } }
      } else {
        // Android 8–9: the app's own Downloads folder needs no storage permission.
        val dir = activity.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: throw IllegalStateException("No storage")
        src.copyTo(File(dir, args.name), overwrite = true)
      }
      invoke.resolve(JSObject().put("location", "Downloads/${args.name}"))
    } catch (e: Exception) {
      invoke.reject("Couldn't save ${args.name}: ${e.message}")
    }
  }

  /** Open a file from the app's cache in whichever app handles its type. */
  @Command
  fun openFile(invoke: Invoke) {
    val args = invoke.parseArgs(FileArgs::class.java)
    try {
      val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", File(args.path))
      val intent = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(uri, args.mime.ifBlank { "application/octet-stream" })
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      }
      activity.startActivity(Intent.createChooser(intent, args.name).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
      invoke.resolve()
    } catch (e: ActivityNotFoundException) {
      invoke.reject("No app on this phone opens ${args.name}")
    } catch (e: Exception) {
      invoke.reject("Couldn't open ${args.name}: ${e.message}")
    }
  }

  /** Colour the area behind the status and navigation bars to match the app's theme. */
  @Command
  fun setSystemBars(invoke: Invoke) {
    val args = invoke.parseArgs(BarsArgs::class.java)
    activity.runOnUiThread {
      try {
        activity.window.decorView.setBackgroundColor(Color.parseColor(args.background))
        WindowCompat.getInsetsController(activity.window, activity.window.decorView).apply {
          isAppearanceLightStatusBars = !args.dark
          isAppearanceLightNavigationBars = !args.dark
        }
      } catch (_: Exception) {
      }
    }
    invoke.resolve()
  }

  /** Close the text menu (Copy, Share…) the web view can leave up with nothing
   *  selected, e.g. after another app or a system dialog had the screen. */
  @Command
  fun dismissTextMenu(invoke: Invoke) {
    activity.runOnUiThread { (activity as? MainActivity)?.textMenu?.finish() }
    invoke.resolve()
  }

  // Taps on Remail's notifications (src-tauri/src/notify.rs): MainActivity
  // keeps the tapped notification's id; this tells the Rust side and hands
  // it over.

  override fun onNewIntent(intent: Intent) {
    if (MainActivity.tapOf(intent) != null) trigger("notificationTap", JSObject())
  }

  /** `{ id }` of the notification tapped last (once), or `{}`. */
  @Command
  fun takeNotificationTap(invoke: Invoke) {
    val reply = JSObject()
    MainActivity.takeTap()?.let { reply.put("id", it) }
    invoke.resolve(reply)
  }
}
