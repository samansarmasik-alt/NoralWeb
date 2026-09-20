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
        val dizin = java.io.File(filesDir, "noral")
        // Önceki çökme izi varsa önce onu göster (sebebi bulmak için).
        val iz = cokmeIzi(dizin)
        if (iz != null) {
            hataGoster("Önceki açılışta çöktü:\n\n" + iz, dizin)
            return
        }
        // Yakalanmayan her hata dosyaya yazılır, sonraki açılışta ekrana gelir.
        val eski = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { t, e ->
            try {
                dizin.mkdirs()
                val sw = java.io.StringWriter()
                e.printStackTrace(java.io.PrintWriter(sw))
                java.io.File(dizin, "crash.log").appendText("\n--- " + e.toString() + "\n" + sw.toString().take(3000))
            } catch (_: Exception) {
            }
            eski?.uncaughtException(t, e)
        }
        try {
            // Rust çekirdeğine yazılabilir dizin (model + anahtarlar buraya).
            Core.init(dizin.absolutePath)
        } catch (e: Throwable) {
            // Kütüphane yüklenemezse sessiz kapanma yerine sebebi göster.
            hataGoster("Çekirdek yüklenemedi:\n" + (e.message ?: e.toString()), null)
            return
        }

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

    // crash.log (Java) + panic.log (Rust) varsa birleştirip döndürür.
    private fun cokmeIzi(dizin: java.io.File): String? {
        val out = StringBuilder()
        try {
            val c = java.io.File(dizin, "crash.log")
            if (c.exists() && c.length() > 0) out.append(c.readText().take(2500))
        } catch (_: Exception) {
        }
        try {
            val p = java.io.File(dizin, "panic.log")
            if (p.exists() && p.length() > 0) {
                if (out.isNotEmpty()) out.append("\n\n")
                out.append(p.readText().take(1500))
            }
        } catch (_: Exception) {
        }
        return if (out.isEmpty()) null else out.toString()
    }

    private fun hataGoster(mesaj: String, dizin: java.io.File?) {
        val kaydir = android.widget.ScrollView(this)
        val kutu = android.widget.LinearLayout(this)
        kutu.orientation = android.widget.LinearLayout.VERTICAL
        kutu.setPadding(48, 96, 48, 48)
        val t = android.widget.TextView(this)
        t.text = "NoralWeb açılamadı\n\n" + mesaj.take(4000) + "\n\nBu yazının ekran görüntüsünü gönder."
        t.textSize = 15f
        kutu.addView(t)
        if (dizin != null) {
            val b = android.widget.Button(this)
            b.text = "İzleri temizle ve yeniden başlat"
            b.setOnClickListener {
                try {
                    java.io.File(dizin, "crash.log").delete()
                    java.io.File(dizin, "panic.log").delete()
                } catch (_: Exception) {
                }
                recreate()
            }
            kutu.addView(b)
        }
        kaydir.addView(kutu)
        setContentView(kaydir)
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
