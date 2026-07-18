package dev.onyx.app

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  /**
   * Wry's default back handling replays webview session history. The shell
   * is a SPA with its own navigation stack, and Chromium's history
   * manipulation intervention marks programmatic pushState entries as
   * skippable — the history dance eventually runs dry and a back press
   * falls out of the app while overlays are still open. We own back
   * instead and ask the SPA first (see __onyxHandleBack in MobileApp).
   */
  override val handleBackNavigation: Boolean
    get() = false

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    intent = rewriteShareIntent(intent)
    super.onCreate(savedInstanceState)

    onBackPressedDispatcher.addCallback(
      this,
      object : OnBackPressedCallback(true) {
        override fun handleOnBackPressed() {
          val view = webView ?: return fallthrough(this)
          view.evaluateJavascript(
            "window.__onyxHandleBack ? window.__onyxHandleBack() : false"
          ) { consumed ->
            if (consumed != "true") fallthrough(this)
          }
        }
      },
    )
  }

  /** Re-dispatch back with our callback disabled: default minimize/finish. */
  private fun fallthrough(callback: OnBackPressedCallback) {
    callback.isEnabled = false
    onBackPressedDispatcher.onBackPressed()
    callback.isEnabled = true
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
