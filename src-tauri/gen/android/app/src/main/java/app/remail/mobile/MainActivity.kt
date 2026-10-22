package app.remail.mobile

import android.content.Intent
import android.os.Bundle
import android.view.ActionMode
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  companion object {
    /** The id of the Remail notification tapped last, until the app takes it
     *  (RemailPlugin). Kept here because a tap can arrive before any plugin
     *  is loaded: when it starts the app, or relaunches it after Android
     *  ended the process. */
    private var tapped: Int? = null

    /** The notification id of a tap intent from the notification plugin. */
    fun tapOf(intent: Intent?): Int? {
      if (intent == null || intent.getStringExtra("NotificationUserAction") != "tap") return null
      return intent.getIntExtra("NotificationId", Int.MIN_VALUE).takeIf { it != Int.MIN_VALUE }
    }

    @Synchronized
    fun takeTap(): Int? = tapped.also { tapped = null }

    @Synchronized
    private fun keep(id: Int?) {
      if (id != null) tapped = id
    }
  }

  override fun onNewIntent(intent: Intent) {
    keep(tapOf(intent))
    super.onNewIntent(intent)
  }

  /** The web view's text menu (Copy, Share…), so the page can close a stale one. */
  var textMenu: ActionMode? = null

  override fun onActionModeStarted(mode: ActionMode) {
    super.onActionModeStarted(mode)
    textMenu = mode
  }

  override fun onActionModeFinished(mode: ActionMode) {
    super.onActionModeFinished(mode)
    if (textMenu === mode) textMenu = null
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Started by a tap (not restored, which would repeat it).
    if (savedInstanceState == null) keep(tapOf(intent))
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Android draws apps edge to edge: keep the web view clear of the status
    // bar, the navigation bar, cutouts and the keyboard (so compose shrinks
    // above it). The window background shows behind the bars.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
      v.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
      WindowInsetsCompat.CONSUMED
    }
  }
}
