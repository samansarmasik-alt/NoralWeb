//! NVIDIA NIM istemcisi — agentic araştırma + OSINT motoru.
//!
//! Varsayılan model: nvidia/nemotron-3-super-120b-a12b (OpenAI-uyumlu uç).
//! Anahtar %APPDATA%/NoralWeb/nim-key.txt içindedir (proje klasörü DIŞINDA,
//! koda gömülmez, sadece api.nvidia.com'a gönderilir).

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const NIM_BASE: &str = "https://integrate.api.nvidia.com/v1";
pub const DEFAULT_MODEL: &str = "nvidia/nemotron-3-super-120b-a12b";
pub const MAX_STEPS: usize = 8;

/// Platform veri dizini: Windows'ta %APPDATA%/NoralWeb, Linux'ta $HOME/.config/NoralWeb.
pub fn appdata_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("NoralWeb")
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Linux'ta APPDATA yoktur; XDG karşılığı $HOME/.config/NoralWeb (yoksa oluşturmayı dene, hata yut).
        let taban = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let dizin = taban.join(".config").join("NoralWeb");
        let _ = std::fs::create_dir_all(&dizin);
        dizin
    }
}

pub fn nim_key_path() -> std::path::PathBuf {
    appdata_dir().join("nim-key.txt")
}

/// Anahtarı dış depodan okur.
pub fn nim_key() -> Option<String> {
    let k = std::fs::read_to_string(nim_key_path()).ok()?.trim().to_string();
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OutCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Giden format: OpenAI şeması (type + function + arguments STRING).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OutFunc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutFunc {
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    pub fn to_out(&self) -> OutCall {
        OutCall {
            id: self.id.clone(),
            kind: "function".into(),
            function: OutFunc {
                name: self.name.clone(),
                arguments: serde_json::to_string(&self.args).unwrap_or("{}".into()),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NimReply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Ajan araç şemaları (OpenAI function-calling formatı).
/// harvest=true ise deneysel ekran-hasadı da listelenir.
pub fn tools_schema(harvest: bool) -> serde_json::Value {
    let mut tools = match serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Yerel ücretsiz motorda web araması. Kişi, site, konu, kullanıcı adı bulur. Son 3-4 kelimelik odaklı sorgular kullan.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Arama sorgusu"}
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "fetch_page",
                "description": "Bir sayfanın metnini, meta/OG bilgisini ve dış linklerini çeker. Profil bio'larındaki linkleri takip etmek, siteler arası bağlantı kurmak için kullan. Linktree/about.me gibi hub sayfaları otomatik açılır.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Tam URL (http...)"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "open_tabs",
                "description": "Birden fazla önemli URL'yi tek seferde yeni sekmelerde açar (en fazla 5). Profil + haber + resmi site gibi toplu açışlarda open_tab yerine bunu kullan.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "urls": {"type": "array", "items": {"type": "string"}, "description": "Tam URL listesi"}
                    },
                    "required": ["urls"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "open_tab",
                "description": "URL'yi kullanıcının tarayıcısında yeni sekmede açar. Önemli bulguları (profil, kaynak) açmak için kullan.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Tam URL (http...)"}
                    },
                    "required": ["url"]
                }
            }
        }
    ]) {
        serde_json::Value::Array(a) => a,
        _ => Vec::new(),
    };
    if harvest {
        tools.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": "harvest",
                "description": "DENEYSEL: URL'yi gizli gerçek-tarayıcıda açıp EKRAN verisini çeker (JS ile oluşan içerik dahil). Bot duvarlı sitelerde fetch_page ölürse bunu dene. Yavaştır (10-30sn), idareli kullan.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "Tam URL (http...)"}
                    },
                    "required": ["url"]
                }
            }
        }));
    }
    serde_json::Value::Array(tools)
}

/// Tek chat turu (araç çağrısı dönebilir).
pub fn chat(
    key: &str,
    model: &str,
    messages: &[Msg],
    tools: &serde_json::Value,
) -> Result<NimReply, String> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": 2048,
        "temperature": 0.3,
    });
    // Boş tools + tool_choice bazı uçlarda 400 verir; sadece doluyken ekle.
    if tools.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        body["tools"] = tools.clone();
        body["tool_choice"] = serde_json::json!("auto");
    }
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let resp = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(90))
            .build()
            .post(&format!("{}/chat/completions", NIM_BASE))
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string());
        match resp {
            Ok(r) => {
                let txt = r.into_string().map_err(|e| format!("nim gövde: {}", e))?;
                return parse_reply(&txt);
            }
            // Aşırı yük / kota / geçici arıza → bekleyip tekrar dene (6 deneme).
            Err(ureq::Error::Status(_code @ (429 | 500 | 502 | 503 | 504), r)) if attempt <= 6 => {
                let _ = r.into_string();
                let wait = 2u64.pow(attempt).min(20);
                std::thread::sleep(Duration::from_secs(wait));
                continue;
            }
            Err(ureq::Error::Status(code, r)) => {
                // Gerçek sebep gövdede yazar — mesaja ve dosyaya dök.
                let detail = r.into_string().unwrap_or_default();
                let dbg = serde_json::json!({
                    "request": body,
                    "status": code,
                    "response": detail,
                });
                if let Ok(txt) = serde_json::to_string(&dbg) {
                    let _ = std::fs::write(std::env::temp_dir().join("noral-nim-hata.json"), txt);
                }
                return Err(format!(
                    "nim {}: {}",
                    code,
                    detail.chars().take(500).collect::<String>()
                ));
            }
            // Taşıma hatası (zaman aşımı vb.) → 2 kez daha dene.
            Err(_e) if attempt <= 2 => {
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
            Err(e) => return Err(format!("nim http: {}", e)),
        }
    }
}

/// Yanıt ayrıştırma (test edilebilir).
pub fn parse_reply(txt: &str) -> Result<NimReply, String> {
    let v: serde_json::Value =
        serde_json::from_str(txt).map_err(|e| format!("nim json: {}", e))?;
    if let Some(err) = v.get("error") {
        return Err(format!("nim api: {}", err));
    }
    let msg = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .ok_or_else(|| "nim: choices.message yok".to_string())?;
    let content = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let mut calls = Vec::new();
    if let Some(arr) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in arr {
            let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let f = tc.get("function");
            let name = f
                .and_then(|x| x.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let args_s = f
                .and_then(|x| x.get("arguments"))
                .and_then(|x| x.as_str())
                .unwrap_or("{}");
            let args: serde_json::Value = serde_json::from_str(args_s).unwrap_or(serde_json::json!({}));
            if name.is_empty() {
                continue;
            }
            calls.push(ToolCall { id, name, args });
        }
    }
    Ok(NimReply {
        content,
        tool_calls: calls,
    })
}

pub fn sys_prompt(mode: &str) -> String {
    if mode == "osint" {
        "Sen Nöral Web'in OSINT ajanısın. Türkçe yanıt verirsin. Görevin: kişi, kullanıcı adı ve sosyal profil korelasyonu. \
        Kurallar: (1) Önce web_search ile geniş tara — sorguları spesifik yaz (tam ad + platform/ipucu ekle, örn. '\"Ad Soyad\" instagram'). Sonra fetch_page ile profil bio'larındaki linkleri takip et, siteler arası bağlantı kur (github↔mastodon↔bluesky↔X↔kişisel site). \
        (2) Tahmin üretme; her iddiayı kaynak URL ile destekle. (3) Emin olmadıklarını 'olasılık' diye etiketle. \
        (4) Önemli bulguları open_tabs ile toplu aç (tek tek open_tab yerine). (5) En fazla 8 araç adımı; sonra toparla: kim, nerede aktif, kanıt linkleri.".to_string()
    } else {
        "Sen Nöral Web'in araştırma ajanısın. Türkçe yanıt verirsin. Görevin: soruyu derinlemesine araştırıp kaynaklı yanıt vermek. \
        Kurallar: (1) web_search ile farklı açılardan 2-4 spesifik sorgu yap, (2) kritik sayfaları fetch_page ile oku, \
        (3) yanıtında kaynak URL'lerini belirt, (4) önemli sayfaları open_tabs ile toplu aç, (5) en fazla 8 araç adımı.".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yanit_ayristir() {
        let txt = r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"Merhaba","tool_calls":[{"id":"c1","type":"function","function":{"name":"web_search","arguments":"{\"query\":\"test\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let r = parse_reply(txt).unwrap();
        assert_eq!(r.content, "Merhaba");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "web_search");
        assert_eq!(r.tool_calls[0].args["query"], "test");
    }

    #[test]
    fn aracsiz_yanit() {
        let txt = r#"{"choices":[{"message":{"role":"assistant","content":"42","tool_calls":[]}}]}"#;
        let r = parse_reply(txt).unwrap();
        assert_eq!(r.content, "42");
        assert!(r.tool_calls.is_empty());
    }

    #[test]
    fn hata_yaniti() {
        let txt = r#"{"error":{"message":"kota bitti"}}"#;
        assert!(parse_reply(txt).is_err());
    }
}
