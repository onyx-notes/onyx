package dev.onyx.app

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    intent = rewriteShareIntent(intent)
    super.onCreate(savedInstanceState)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(rewriteShareIntent(intent) ?: intent)
  }

  /**
   * Share target: fold ACTION_SEND text into the onyx://capture deep link
   * so a single frontend route handles shares, quick actions, and links.
   * Non-share intents pass through untouched.
   */
  private fun rewriteShareIntent(intent: Intent?): Intent? {
    if (intent?.action != Intent.ACTION_SEND || intent.type != "text/plain") {
      return intent
    }
    val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return intent
    val title = intent.getStringExtra(Intent.EXTRA_SUBJECT)
    val uri = Uri.Builder()
      .scheme("onyx")
      .authority("capture")
      .appendQueryParameter("text", text)
      .apply { title?.let { appendQueryParameter("title", it) } }
      .build()
    return Intent(Intent.ACTION_VIEW, uri).apply {
      // Keep the component so the deep-link plugin sees a normal VIEW.
      setPackage(packageName)
    }
  }
}
