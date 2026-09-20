package com.noralweb

import android.app.Activity
import android.os.Bundle
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient

class MainActivity : Activity() {
    lateinit var web: WebView

    override fun onCreate(s: Bundle?) {
        super.onCreate(s)
        // Rust çekirdeğine yazılabilir dizin (model + anahtarlar buraya).
        Core.init(filesDir.absolutePath + "/noral")

        web = WebView(this)
        setContentView(web)
        val st = web.settings
        st.javaScriptEnabled = true
        st.domStorageEnabled = true
        st.useWideViewPort = true
        st.loadWithOverviewMode = true
        st.builtInZoomControls = true
        st.displayZoomControls = false
        st.mediaPlaybackRequiresUserGesture = false

        web.addJavascriptInterface(Kopru(this), "ipc")
        web.webChromeClient = WebChromeClient()
        web.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(v: WebView, r: WebResourceRequest): Boolean {
                val u = r.url.toString()
                // Asset-içi kalır, dış link tarayıcıya gider.
                if (u.startsWith("file:///android_asset/")) return false
                try {
                    val i = android.content.Intent(
                        android.content.Intent.ACTION_VIEW,
                        android.net.Uri.parse(u)
                    )
                    startActivity(i)
                } catch (_: Exception) {
                }
                return true
            }

            override fun onPageFinished(v: WebView, url: String) {
                // Masaüstü sekme sistemi mobilde yok: tüm sonuç linkleri dış tarayıcıya.
                v.evaluateJavascript(
                    "window.__noralOpen=function(u){try{ipc.disAc(String(u))}catch(e){}};",
                    null
                )
            }
        }
        web.loadUrl("file:///android_asset/index.html")
    }

    override fun onBackPressed() {
        // Geri tuşu uygulamayı kapatmasın, arka plana alsın.
        moveTaskToBack(true)
    }

    override fun onDestroy() {
        try {
            web.destroy()
        } catch (_: Exception) {
        }
        super.onDestroy()
    }
}
