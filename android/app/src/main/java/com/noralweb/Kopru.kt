package com.noralweb

import android.content.Intent
import android.net.Uri
import android.webkit.JavascriptInterface
import org.json.JSONArray
import org.json.JSONObject

// Sayfanın `window.ipc.postMessage(JSON)` çağrılarını karşılar (masaüstü wry IPC ile aynı
// komutlar). Ağır iş bg thread'de koşar, sonuç UI thread'de `window.__noral*` ile itilir.
class Kopru(private val act: MainActivity) {

    private fun it(js: String) {
        act.runOnUiThread {
            try {
                act.web.evaluateJavascript(js, null)
            } catch (_: Exception) {
            }
        }
    }

    private fun disariAc(u: String) {
        try {
            val s = u.trim()
            if (s.startsWith("http://") || s.startsWith("https://")) {
                val i = Intent(Intent.ACTION_VIEW, Uri.parse(s))
                i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                act.startActivity(i)
            }
        } catch (_: Exception) {
        }
    }

    @JavascriptInterface
    fun postMessage(msg: String) {
        Thread {
            try {
                val o = JSONObject(msg)
                when (o.optString("cmd")) {
                    "research" -> {
                        val j = Core.search(
                            o.optString("query"),
                            o.optInt("depth", 1),
                            o.optInt("apx", 1)
                        )
                        it("window.__noralPush($j)")
                    }
                    "click" -> {
                        val f = o.optJSONArray("feats")?.toString() ?: "[]"
                        val s = o.optJSONArray("skipped")?.toString() ?: "[]"
                        Core.click(f, s)
                    }
                    "testmode" -> {
                        val j = Core.testmode()
                        it("window.__noralTest($j)")
                    }
                    "agent" -> {
                        val j = Core.agent(
                            o.optString("message"),
                            o.optString("mode"),
                            o.optJSONArray("history")?.toString() ?: "[]",
                            o.optInt("apx", 1)
                        )
                        // Ajanın açtıkları dış tarayıcıda açılır (sekme yok).
                        try {
                            val arr: JSONArray? = JSONObject(j).optJSONArray("opened")
                            if (arr != null) {
                                for (i in 0 until arr.length()) {
                                    disariAc(arr.optString(i))
                                }
                            }
                        } catch (_: Exception) {
                        }
                        it("window.__noralAgentResult($j)")
                    }
                    "savekey" -> {
                        val j = Core.saveKey(o.optString("service"), o.optString("key"))
                        it("window.__noralKeys($j)")
                    }
                    "keystatus" -> {
                        val j = Core.keyStatus()
                        it("window.__noralKeys($j)")
                    }
                }
            } catch (_: Exception) {
            }
        }.start()
    }

    // Sayfanın kendi `__noralOpen`u yerine geçer (uygulama-içi sekme yok, dış tarayıcı var).
    @JavascriptInterface
    fun disAc(u: String) {
        disariAc(u)
    }
}
