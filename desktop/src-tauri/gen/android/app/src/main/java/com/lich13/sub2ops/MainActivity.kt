package com.lich13.sub2ops

import android.os.Bundle
import android.webkit.WebView
import android.view.ViewGroup
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    // WebView padding does not reduce its CSS viewport. Inset the container so
    // 100dvh and fixed navigation are measured inside the usable screen area.
    webView.post {
      val container = webView.parent as? ViewGroup ?: return@post
      ViewCompat.setOnApplyWindowInsetsListener(container) { view, insets ->
        val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
        val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime())
        view.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, keyboard.bottom))
        WindowInsetsCompat.CONSUMED
      }
      ViewCompat.requestApplyInsets(container)
    }
  }
}
