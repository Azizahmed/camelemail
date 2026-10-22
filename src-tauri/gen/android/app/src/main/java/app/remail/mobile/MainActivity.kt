package app.remail.mobile

import android.os.Bundle
import android.view.ActionMode
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
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
