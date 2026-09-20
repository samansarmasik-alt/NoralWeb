package com.noralweb

// Rust çekirdeği (jni krati → libnoralcore.so). Tüm çağrılar bg thread'den yapılır.
object Core {
    init {
        System.loadLibrary("noralcore")
    }

    external fun init(dir: String)
    external fun search(query: String, depth: Int, apx: Int): String
    external fun agent(message: String, mode: String, history: String, apx: Int): String
    external fun testmode(): String
    external fun click(feats: String, skipped: String)
    external fun saveKey(service: String, key: String): String
    external fun keyStatus(): String
}
