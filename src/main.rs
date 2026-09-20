//! Nöral Web v0.5 — Rust + WebView2 tek exe, derin araştırma kabuğu.

mod fetch;
mod neural;
mod nim;
mod research;

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

const SHELL_HTML: &str = include_str!("../ui/index.html");

#[derive(Debug, Deserialize)]
struct IpcReq {
    cmd: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    feats: Option<[f32; neural::N_IN]>,
    #[serde(default)]
    skipped: Vec<[f32; neural::N_IN]>,
    /// 0 = hızlı (tek halka), 1 = derin (iki halka). Yoksa derin.
    #[serde(default)]
    depth: Option<u8>,
    /// apinex/agent: 0=kapalı, 1=zayıfken, 2=her zaman. Yoksa zayıfken.
    #[serde(default)]
    apx: Option<u8>,
    /// deneysel hasatçı (gizli tarayıcı) açık mı?
    #[serde(default)]
    harvest: bool,
    /// savekey için: servis + anahtar
    #[serde(default)]
    service: String,
    #[serde(default)]
    key: String,
    /// agent için: kullanıcı mesajı + mod + geçmiş
    #[serde(default)]
    message: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    history: Vec<HistMsg>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistMsg {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
}

/// Servis anahtar dosyası. NIM dış depodadır (%APPDATA%/NoralWeb).
fn key_path(service: &str) -> std::path::PathBuf {
    if service == "nim" {
        return nim::nim_key_path();
    }
    let file = match service {
        "apinex" => "apinex-key.txt",
        "exa" => "exa-key.txt",
        "tavily" => "tavily-key.txt",
        "serper" => "serper-key.txt",
        "langsearch" => "langsearch-key.txt",
        _ => "brave-key.txt",
    };
    // Önce exe yanı, yoksa %APPDATA%/NoralWeb (OneDrive senkron derdine çare).
    let exe_yani = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir())
        .join(file);
    if exe_yani.exists() {
        exe_yani
    } else {
        nim::appdata_dir().join(file)
    }
}

fn has_key(service: &str) -> bool {
    key_path(service).exists()
        && std::fs::read_to_string(key_path(service))
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
}

/// Sekme URL izni: yalnızca http/https/about:blank açılır (javascript:/data:/vbscript: engelli).
fn url_izinli(u: &str) -> bool {
    let l = u.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l == "about:blank"
}

#[derive(Debug)]
enum UserEvent {
    ResearchResult(String),
    TestResult(String),
    KeyStatus(String),
    AgentProgress(String),
    AgentResult(String),
    OpenTab(String),
    /// Deneysel hasatçı isteği: gizli tarayıcıda sayfayı aç, ekran verisini çek.
    HarvestReq {
        id: u64,
        url: String,
        tx: std::sync::mpsc::Sender<String>,
    },
    HarvestDone {
        id: u64,
        json: String,
    },
    HarvestTimeout {
        id: u64,
    },
}

/// Gizli hasat yuvası: pencere + görünüm düşürülünce kapanır.
struct HarvestSlot {
    _win: tao::window::Window,
    _view: wry::WebView,
    tx: std::sync::mpsc::Sender<String>,
}

/// Sayfa yüklenince ekran verisini toplayıp Rust'a postalar.
/// Gerçek Edge motoru = gerçek TLS parmak izi + JS + çerez = bot duvarları aşılır.
const HARVEST_JS: &str = r#"(function(){
  function collect(){
    try{
      var t=document.title||'';
      var tx='';
      try{ tx=(document.body?document.body.innerText:'').slice(0,8000); }catch(e){}
      if(!tx){ try{ tx=(document.body?document.body.textContent:'').slice(0,8000); }catch(e){} }
      var md='',og='';
      try{ var m=document.querySelector('meta[name="description"]'); if(m)md=m.content||'';
        var o=document.querySelector('meta[property="og:image"]'); if(o)og=o.content||''; }catch(e){}
      var ls=[];
      try{ var as=document.links; for(var i=0;i<as.length&&ls.length<40;i++){ var a=as[i];
        if(a.href&&a.href.indexOf('http')===0) ls.push({t:(a.innerText||'').slice(0,120),u:a.href}); } }catch(e){}
      window.ipc.postMessage(JSON.stringify({title:t,text:tx,meta:md,og:og,links:ls}));
    }catch(e){}
  }
  window.addEventListener('load',function(){setTimeout(collect,3000);});
  document.addEventListener('DOMContentLoaded',function(){setTimeout(collect,2000);});
  setTimeout(collect,12000);
})();"#;

/// Google çerez aşısı: taze WebView profiline rıza çerezi eker (duvarı yumuşatır).
/// HARVEST_JS'ten ÖNCE çalışır; google dışı URL'lerde kullanılmaz.
const GOOGLE_INIT: &str = r#"(function(){
  try{ document.cookie="CONSENT=YES+cb.20230626-15-p0.en+FX+700;domain=.google.com;path=/;max-age=31536000"; }catch(e){}
})();"#;

/// Agentic döngü: NIM + araçlar (web_search / fetch_page / harvest / open_tab).
/// Araçlar varsayılan zayıfken-modundadır (UI'daki $ ayarı geçer).
static NEXT_HARVEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Hasat süzgeci için kötü uzantılar (fetch.rs BAD_EXT ile aynı — kopya, bağımlılık yok).
const HARVEST_BAD_EXT: [&str; 14] = [
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".css", ".js", ".pdf", ".zip",
    ".mp4", ".mp3", ".ico", ".woff",
];

/// Minik percent-decode (fetch::dec private olduğu için burada — harvest /url?q= için).
fn harvest_dec(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let mut o = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                o.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            o.push(' ');
        } else {
            o.push(b[i] as char);
        }
        i += 1;
    }
    o
}

/// HARVEST_JS çıktısından güvenli link listesi çıkarır.
/// Şema: {title,text,meta,links:[{t,u}]} — duvar/consent sayfası boş döner.
fn parse_harvest_links(json: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    // Hata JSON'u (hasat yoğunluğu vb.) sessiz geçilir.
    if v.get("error").is_some() {
        return Vec::new();
    }
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
    // Duvar/rıza sayfası: başlıkta Sorry/Consent geçerse boş dön.
    let tl = title.to_lowercase();
    if tl.contains("sorry") || tl.contains("consent") {
        return Vec::new();
    }
    let Some(arr) = v.get("links").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for it in arr.iter() {
        let (mut t, mut u) = (
            it.get("t").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
            it.get("u").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
        );
        if u.is_empty() {
            continue;
        }
        // Google yönlendirmesini çöz (/url?q=GERÇEK&sa=...).
        if let Some(p) = u.find("/url?q=") {
            let art = &u[p + 7..];
            let son = art.find('&').unwrap_or(art.len());
            u = harvest_dec(&art[..son]);
        }
        // Yandex yönlendirmesini çöz (/r?...&u=GERÇEK&... — sadece yandex hostunda).
        if u.to_lowercase().contains("yandex.") {
            if let Some(p) = u.find("u=http") {
                let art = &u[p + 2..];
                let son = art.find('&').unwrap_or(art.len());
                u = harvest_dec(&art[..son]);
            }
        }
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            continue;
        }
        let dl = u.to_lowercase();
        // İç sayfalar elenir.
        if dl.contains("google.com") || dl.contains("gstatic") || dl.contains("consent") {
            continue;
        }
        if HARVEST_BAD_EXT.iter().any(|e| dl.contains(e)) {
            continue;
        }
        // Başlık boşsa URL başlık olur.
        if t.is_empty() {
            t = u.clone();
        }
        out.push((t, u));
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Hasat boşluğunun nedeni — research yedeğinde silent izi için (BEKLENEN'e
/// dokunmaz, silent free-form): "Google-H(duvar)" / "Google-H(zaman-aşımı)" /
/// "Google-H(yoğun)". parse_harvest_links'e dokunmaz, sadece sınıflar.
fn harvest_bos_neden(json: Option<&str>, etiket: &str) -> String {
    let k = match json {
        // Alıcı hiç gövde görmedi (recv zaman aşımı).
        None => "zaman-aşımı",
        Some(j) => {
            if j.contains("yoğun") {
                "yoğun"
            } else if j.contains("zaman") {
                "zaman-aşımı"
            } else {
                // Duvar/consent sayfası ya da linkler elenip boş kaldı.
                "duvar"
            }
        }
    };
    format!("{etiket}({k})")
}

/// Bloklu sayfa hasadı: gizli WebView ile render edip link toplar.
/// 14sn bekler — hata/timeout sessiz boş + neden döner.
fn harvest_page_blocking(
    proxy: &EventLoopProxy<UserEvent>,
    url: String,
    etiket: &str,
) -> (Vec<(String, String)>, Option<String>) {
    let id = NEXT_HARVEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = proxy.send_event(UserEvent::HarvestReq { id, url, tx });
    // Yedek temizlik: sayfa asılı kalırsa yuvayı düşür (arama akışı 14sn bekler).
    let tp = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(16));
        let _ = tp.send_event(UserEvent::HarvestTimeout { id });
    });
    match rx.recv_timeout(std::time::Duration::from_secs(14)) {
        Ok(json) => {
            let links = parse_harvest_links(&json);
            let neden = if links.is_empty() {
                Some(harvest_bos_neden(Some(&json), etiket))
            } else {
                None
            };
            (links, neden)
        }
        Err(_) => (Vec::new(), Some(harvest_bos_neden(None, etiket))),
    }
}

/// Bloklu Google harvest (ajan yolu sarmalayıcı — imza sabit, davranış aynı).
fn harvest_google_blocking(proxy: &EventLoopProxy<UserEvent>, query: &str) -> (Vec<(String, String)>, Option<String>) {
    let url = format!(
        "https://www.google.com/search?q={}&num=20&hl=tr",
        fetch::enc(query)
    );
    harvest_page_blocking(proxy, url, "Google-H")
}

fn run_agent(
    proxy: EventLoopProxy<UserEvent>,
    key: String,
    mode: String,
    message: String,
    history: Vec<HistMsg>,
    apx: u8,
    harvest_on: bool,
) {
    let mode = if mode == "osint" { "osint" } else { "arastirma" };
    let mut sys = nim::sys_prompt(mode);
    if harvest_on {
        sys.push_str("\nDeneysel HASAT açık: bot duvarlı/JS-ağır sayfalar için harvest aracını kullan (gerçek tarayıcı ekran verisini çeker).");
    }
    let mut messages = vec![nim::Msg {
        role: "system".into(),
        content: sys,
        tool_calls: None,
        tool_call_id: None,
    }];
    let start = std::time::Instant::now();
    // Aynı mesaj hem geçmişte hem güncelde gelirse tekille (ardışık kopya 500 yapıyor).
    let mut history = history;
    while history
        .last()
        .map(|h| h.content.trim() == message.trim() && !message.trim().is_empty())
        .unwrap_or(false)
    {
        history.pop();
    }
    for h in history.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
        let role = if h.role == "assistant" || h.role == "tool" {
            h.role.clone()
        } else {
            "user".into()
        };
        messages.push(nim::Msg {
            role,
            content: h.content.chars().take(2000).collect(),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.push(nim::Msg {
        role: "user".into(),
        content: message.chars().take(2000).collect(),
        tool_calls: None,
        tool_call_id: None,
    });
    let tools = nim::tools_schema(harvest_on);
    let mut steps: Vec<serde_json::Value> = Vec::new();
    // Kanıt havuzu: özet turu patlarsa ham bulgularla düşülür.
    let mut evidence: Vec<String> = Vec::new();
    let mut searches = 0u32;
    let mut fetched = false;
    // Ajan aramaları nöral ağla sıralanır (eğitimli ağırlık dosyadan okunur).
    let snap_net = neural::load_or_train().net;
    // Tekrar-sorgu freni: aynı sorgu bir kez aranır.
    let mut seen_q: std::collections::HashSet<String> = std::collections::HashSet::new();
    let answer = loop {
        if start.elapsed().as_secs() > 300 {
            break "Süre doldu (5 dk); bulduklarımla toparlıyorum.".to_string();
        }
        if steps.len() >= nim::MAX_STEPS {
            break String::new();
        }
        let reply = match nim::chat(&key, nim::DEFAULT_MODEL, &messages, &tools) {
            Ok(r) => r,
            Err(e) => {
                break if evidence.is_empty() {
                    format!("Model hatası: {}", e)
                } else {
                    format!("Model hatası ({}). Toplanan bulgular:\n{}", e, evidence.join("\n"))
                };
            }
        };
        if reply.tool_calls.is_empty() {
            break reply.content;
        }
        messages.push(nim::Msg {
            role: "assistant".into(),
            content: reply.content.clone(),
            tool_calls: Some(reply.tool_calls.iter().map(|t| t.to_out()).collect()),
            tool_call_id: None,
        });
        for tc in &reply.tool_calls {
            let arg_snip = tc.args.to_string().chars().take(120).collect::<String>();
            let out = match tc.name.as_str() {
                "web_search" => {
                    let q = tc.args.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    let qn = q.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
                    if !seen_q.insert(qn.clone()) {
                        // AYNI sorgu tekrarı → çalıştırmadan geri çevir, modele yol göster.
                        serde_json::json!({"error": "bu sorguyu zaten aradın (sonuçlar yukarıda). TEKRAR ARAMA YAPMA; fetch_page ile adayları oku ya da tamamen farklı bir açı dene."}).to_string()
                    } else {
                        searches += 1;
                        // UI'daki $ ayarına saygı (zayıfken: kuruyunca Apinex girer).
                        let (mut cands, mut sources, _, _) = fetch::live_search(q, false, apx);
                        // Google duvar yedeği: ureq-Google boşsa gizli hasat dene (rank öncesi).
                        if !sources.iter().any(|s| s.starts_with("Google(")) {
                            let (h, _) = harvest_google_blocking(&proxy, q);
                            if !h.is_empty() {
                                let mut n = 0;
                                for (t, u) in h {
                                    cands.push(research::Candidate {
                                        title: t,
                                        url: u,
                                        snippet: String::new(),
                                        source: "harvest-google".into(),
                                        depth: 0,
                                        page: String::new(),
                                    });
                                    n += 1;
                                }
                                sources.push(format!("Google-H({})", n));
                            }
                        }
                        // Nöral sıralama: en iyiler üste.
                        let ranked = research::rank(&research::NeuralRank, &snap_net, q, cands);
                        for c in ranked.iter().take(5) {
                            if evidence.len() < 10 {
                                evidence.push(format!(
                                    "• {} ({})",
                                    c.title.chars().take(90).collect::<String>(),
                                    c.url
                                ));
                            }
                        }
                        let top: Vec<serde_json::Value> = ranked
                            .into_iter()
                            .take(12)
                            .map(|c| {
                                serde_json::json!({
                                    "title": c.title,
                                    "url": c.url,
                                    "snippet": c.snippet.chars().take(220).collect::<String>(),
                                    "source": c.source,
                                    "neural": c.detail.neural,
                                })
                            })
                            .collect();
                        serde_json::json!({ "results": top, "count": top.len() }).to_string()
                    }
                }
                "fetch_page" => {
                    fetched = true;
                    let u = tc.args.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    match fetch::fetch_page(u) {
                        Some(p) => {
                            if evidence.len() < 10 {
                                evidence.push(format!(
                                    "• SAYFA {}: {}",
                                    p.title.chars().take(80).collect::<String>(),
                                    p.text.chars().take(300).collect::<String>()
                                ));
                            }
                            serde_json::json!({
                            "title": p.title.chars().take(200).collect::<String>(),
                            "text": p.text.chars().take(2500).collect::<String>(),
                            "meta_desc": p.meta_desc,
                            "og_image": p.og_image,
                            "links": p.links.into_iter().take(14).map(|(a, u)| {
                                serde_json::json!({"anchor": a.chars().take(120).collect::<String>(), "url": u})
                            }).collect::<Vec<_>>(),
                        })
                        .to_string()
                        }
                        // Direkt çekim öldüyse Apinex contents yedeği (bot duvarları için).
                        None => match fetch::apinex_contents(u) {
                            Some((t, md)) => {
                                if evidence.len() < 10 {
                                    evidence.push(format!(
                                        "• SAYFA(yedek) {}: {}",
                                        t.chars().take(80).collect::<String>(),
                                        md.chars().take(300).collect::<String>()
                                    ));
                                }
                                serde_json::json!({
                                    "title": t.chars().take(200).collect::<String>(),
                                    "text": md,
                                    "via": "apinex-contents",
                                    "links": [],
                                })
                                .to_string()
                            }
                            None => serde_json::json!({"error": "sayfa çekilemedi"}).to_string(),
                        },
                    }
                }
                "open_tabs" => {
                    let arr = tc.args.get("urls").and_then(|x| x.as_array());
                    let mut opened = Vec::new();
                    if let Some(arr) = arr {
                        for it in arr.iter().take(5) {
                            if let Some(u) = it.as_str() {
                                // Yalnızca http/https/about:blank açılır, gerisi elenir.
                                if url_izinli(u) {
                                    let _ = proxy.send_event(UserEvent::OpenTab(u.to_string()));
                                    opened.push(u.to_string());
                                }
                            }
                        }
                    }
                    serde_json::json!({"opened": opened, "count": opened.len()}).to_string()
                }
                // Deneysel hasat: gizli gerçek-tarayıcıda ekran verisini çeker.
                "harvest" => {
                    if !harvest_on {
                        serde_json::json!({"error": "deneysel hasat kapalı — ayarlardan aç"}).to_string()
                    } else {
                        let u = tc.args.get("url").and_then(|x| x.as_str()).unwrap_or("");
                        if !(u.starts_with("http://") || u.starts_with("https://")) {
                            serde_json::json!({"error": "geçersiz url"}).to_string()
                        } else {
                            let id = NEXT_HARVEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let (tx, rx) = std::sync::mpsc::channel();
                            let _ = proxy.send_event(UserEvent::HarvestReq {
                                id,
                                url: u.to_string(),
                                tx,
                            });
                            // 15 sn yedek zamanlayıcı (ana döngü yuvayı düşürür, 8 adım bütçesi erimesin).
                            let tp = proxy.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_secs(15));
                                let _ = tp.send_event(UserEvent::HarvestTimeout { id });
                            });
                            match rx.recv_timeout(std::time::Duration::from_secs(20)) {
                                Ok(json) => {
                                    if evidence.len() < 10 {
                                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                                            let ti = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                                            let tx2 = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
                                            evidence.push(format!(
                                                "• HASAT {}: {}",
                                                ti.chars().take(80).collect::<String>(),
                                                tx2.chars().take(300).collect::<String>()
                                            ));
                                        }
                                    }
                                    json
                                }
                                Err(_) => serde_json::json!({"error": "hasat zaman aşımı (15sn)"}).to_string(),
                            }
                        }
                    }
                }
                "open_tab" => {
                    let u = tc.args.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if url_izinli(u) {
                        let _ = proxy.send_event(UserEvent::OpenTab(u.to_string()));
                        serde_json::json!({"opened": u}).to_string()
                    } else {
                        serde_json::json!({"error": "geçersiz url"}).to_string()
                    }
                }
                other => serde_json::json!({"error": format!("bilinmeyen araç: {}", other)}).to_string(),
            };
            steps.push(serde_json::json!({"tool": tc.name, "args": arg_snip, "ok": !out.contains("error")}));
            let prog = serde_json::json!({"step": steps.len(), "tool": tc.name, "args": arg_snip});
            if let Ok(j) = serde_json::to_string(&prog) {
                let _ = proxy.send_event(UserEvent::AgentProgress(j));
            }
            messages.push(nim::Msg {
                role: "tool".into(),
                content: out.chars().take(4000).collect(),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }
        // Arama freni: 4 aramadan sonra hâlâ sayfa okunmadıysa oku.
        if searches >= 4 && !fetched {
            fetched = true; // bir kez uyar
            messages.push(nim::Msg {
                role: "user".into(),
                content: "Yeterince adayın var. Yeni web_search YAPMA; en ilgili 2-3 sonucu fetch_page ile oku, sonra toparla.".into(),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    };
    // Adım limitiyle biterse son toparlama turu.
    let answer = if answer.trim().is_empty() {
        messages.push(nim::Msg {
            role: "user".into(),
            content: "Araç çıktılarına dayanarak Türkçe toparla, kaynak URL'lerini ver. Hiçbir şey bulunamadıysa bunu açıkça söyle.".into(),
            tool_calls: None,
            tool_call_id: None,
        });
        nim::chat(&key, nim::DEFAULT_MODEL, &messages, &serde_json::json!([]))
            .map(|r| r.content)
            .unwrap_or_else(|e| {
                if evidence.is_empty() {
                    format!("Toparlama hatası: {}. İpucu: ücretsiz kaynaklar kuruduysa üst bardan $Herzaman modunu aç.", e)
                } else {
                    format!(
                        "Özet servisi yoruldu ({}). Ham bulgular:\n{}",
                        e,
                        evidence.join("\n")
                    )
                }
            })
    } else {
        answer
    };
    // Boş cevap + boş kanıt = dürüst "bulunamadı" (model gevelediyse).
    let answer = if answer.trim().is_empty() {
        if evidence.is_empty() {
            "Bu konuda ücretsiz kaynaklarda bir şey bulunamadı. İpucu: üst bardan $Herzaman modunu açıp tekrar dene (Apinex web indeksi girer).".to_string()
        } else {
            format!("Özet çıkarılamadı. Ham bulgular:\n{}", evidence.join("\n"))
        }
    } else {
        answer
    };
    let payload = serde_json::json!({
        "mode": mode,
        "model": nim::DEFAULT_MODEL,
        "answer": answer,
        "steps": steps,
        "evidence": evidence,
        "cost_usd": (fetch::take_cost_usd() * 100000.0).round() / 100000.0,
    });
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = proxy.send_event(UserEvent::AgentResult(json));
    }
}

fn main() {
    // Açılışta model: kayıtlı varsa yüklenir, yoksa gömülü setle eğitilir.
    let state = Arc::new(Mutex::new(neural::load_or_train()));
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();
    // Deneysel hasat yuvaları (ana döngüde yaşar, iş bitince düşer).
    let mut harvest_slots: HashMap<u64, HarvestSlot> = HashMap::new();

    let window = WindowBuilder::new()
        .with_title("Nöral Web")
        .with_inner_size(LogicalSize::new(1360.0, 880.0))
        .build(&event_loop)
        .expect("pencere açılamadı");

    // Ana döngü için yedek tutamaç (IPC gövdesi proxy'yi taşır).
    let proxy_loop = proxy.clone();

    let webview = WebViewBuilder::new()
        .with_html(SHELL_HTML)
        .with_devtools(cfg!(debug_assertions))
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            let r: IpcReq = match serde_json::from_str(req.body()) {
                Ok(p) => p,
                Err(_) => return,
            };
            if r.cmd == "click" {
                // Tıklamayla online öğrenme: tıklanan > üstte atlananlar.
                if let Some(clicked) = r.feats {
                    if !r.skipped.is_empty() {
                        let st = state.clone();
                        std::thread::spawn(move || {
                            if let Ok(mut s) = st.lock() {
                                neural::learn_click(&mut s.net, clicked, &r.skipped);
                                s.clicks += 1;
                                s.version += 1;
                                neural::save(&s);
                            }
                        });
                    }
                }
                return;
            }
            if r.cmd == "savekey" {
                // Servis API anahtarını exe yanına kaydeder (brave / apinex).
                let proxy = proxy.clone();
                let (service, key) = (r.service.clone(), r.key.trim().to_string());
                std::thread::spawn(move || {
                    let mut ok = false;
                    if (service == "brave"
                        || service == "apinex"
                        || service == "exa"
                        || service == "tavily"
                        || service == "serper"
                        || service == "langsearch"
                        || service == "nim")
                        && !key.is_empty()
                        && key.len() < 300
                    {
                        if service == "nim" {
                            let _ = std::fs::create_dir_all(nim::appdata_dir());
                        }
                        ok = std::fs::write(key_path(&service), &key).is_ok();
                    }
                    let payload = serde_json::json!({
                        "saved": ok,
                        "brave": has_key("brave"),
                        "apinex": has_key("apinex"),
                        "exa": has_key("exa"),
                        "tavily": has_key("tavily"),
                        "serper": has_key("serper"),
                        "langsearch": has_key("langsearch"),
                        "nim": nim::nim_key().is_some(),
                    });
                    if let Ok(json) = serde_json::to_string(&payload) {
                        let _ = proxy.send_event(UserEvent::KeyStatus(json));
                    }
                });
                return;
            }
            if r.cmd == "keystatus" {
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    let payload = serde_json::json!({
                        "saved": false,
                        "brave": has_key("brave"),
                        "apinex": has_key("apinex"),
                        "exa": has_key("exa"),
                        "tavily": has_key("tavily"),
                        "serper": has_key("serper"),
                        "langsearch": has_key("langsearch"),
                        "nim": nim::nim_key().is_some(),
                    });
                    if let Ok(json) = serde_json::to_string(&payload) {
                        let _ = proxy.send_event(UserEvent::KeyStatus(json));
                    }
                });
                return;
            }
            if r.cmd == "agent" && !r.message.trim().is_empty() {
                // Agentic sohbet: NIM anahtarı dış depodan okunur.
                let proxy = proxy.clone();
                let (mode, message, history) = (r.mode.clone(), r.message.clone(), r.history.clone());
                let apx = r.apx.unwrap_or(1).min(2);
                let harvest_on = r.harvest;
                std::thread::spawn(move || match nim::nim_key() {
                    Some(key) => run_agent(proxy, key, mode, message, history, apx, harvest_on),
                    None => {
                        let payload = serde_json::json!({
                            "mode": if mode == "osint" { "osint" } else { "arastirma" },
                            "model": nim::DEFAULT_MODEL,
                            "answer": "NIM anahtarı yok. Soldaki panelden anahtarını kaydet, sonra tekrar dene.",
                            "steps": [],
                            "need_key": true,
                        });
                        if let Ok(json) = serde_json::to_string(&payload) {
                            let _ = proxy.send_event(UserEvent::AgentResult(json));
                        }
                    }
                });
                return;
            }
            if r.cmd == "testmode" {
                let st = state.clone();
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    let s = st.lock().map(|g| g.clone()).unwrap_or_else(|_| neural::load_or_train());
                    let probes = neural::self_test(&s.net);
                    let pass = probes.iter().filter(|p| p.pass).count();
                    let payload = serde_json::json!({
                        "arch": s.net.arch,
                        "params": s.net.params,
                        "net": s.net,
                        "probes": probes,
                        "training": {
                            "base_pairs": neural::BASE_PAIRS.len(),
                            "base_epochs": neural::BASE_EPOCHS,
                            "version": s.version,
                            "clicks": s.clicks,
                            "passed": pass,
                        },
                    });
                    if let Ok(json) = serde_json::to_string(&payload) {
                        let _ = proxy.send_event(UserEvent::TestResult(json));
                    }
                });
                return;
            }
            if r.cmd == "research" && !r.query.trim().is_empty() {
                let proxy = proxy.clone();
                let st = state.clone();
                let query = r.query.clone();
                let deep = r.depth.unwrap_or(1) == 1;
                let apx = r.apx.unwrap_or(1).min(2);
                // Ağı bloklamamak için arka planda çalışır, bitince UI'a iter.
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    // Önbellek YOK: her arama canlı (kötü veri kalıcı olmasın diye).
                    let (mut cands, mut sources, mut silent, cost) = fetch::live_search(&query, deep, apx);
                    // Duvar yedekleri: ureq boşsa gizli hasat dene (rank öncesi, paralel).
                    let need_g = !sources.iter().any(|s| s.starts_with("Google("));
                    let need_y = !sources.iter().any(|s| s.starts_with("Yandex("));
                    let need_e = !sources.iter().any(|s| s.starts_with("Ecosia("));
                    let need_q = !sources.iter().any(|s| s.starts_with("Qwant("));
                    let need_s = !sources.iter().any(|s| s.starts_with("Startpage("));
                    let need_m = !sources.iter().any(|s| s.starts_with("Mojeek("));
                    let (pg, py, pe) = (proxy.clone(), proxy.clone(), proxy.clone());
                    let (pq, ps, pm) = (proxy.clone(), proxy.clone(), proxy.clone());
                    let (qg, qy, qe) = (query.clone(), query.clone(), query.clone());
                    let (qq, qs, qm) = (query.clone(), query.clone(), query.clone());
                    let hg = std::thread::spawn(move || {
                        if need_g {
                            let url = format!(
                                "https://www.google.com/search?q={}&num=20&hl=tr",
                                fetch::enc(&qg)
                            );
                            harvest_page_blocking(&pg, url, "Google-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let hy = std::thread::spawn(move || {
                        if need_y {
                            let url = format!(
                                "https://yandex.com.tr/search/?text={}",
                                fetch::enc(&qy)
                            );
                            harvest_page_blocking(&py, url, "Yandex-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let he = std::thread::spawn(move || {
                        if need_e {
                            let url = format!(
                                "https://www.ecosia.org/search?q={}",
                                fetch::enc(&qe)
                            );
                            harvest_page_blocking(&pe, url, "Ecosia-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let hq = std::thread::spawn(move || {
                        if need_q {
                            let url = format!("https://www.qwant.com/?q={}", fetch::enc(&qq));
                            harvest_page_blocking(&pq, url, "Qwant-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let hs = std::thread::spawn(move || {
                        if need_s {
                            let url = format!(
                                "https://www.startpage.com/sp/search?query={}",
                                fetch::enc(&qs)
                            );
                            harvest_page_blocking(&ps, url, "Startpage-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let hm = std::thread::spawn(move || {
                        if need_m {
                            let url = format!("https://www.mojeek.com/search?q={}", fetch::enc(&qm));
                            harvest_page_blocking(&pm, url, "Mojeek-H")
                        } else {
                            (Vec::new(), None)
                        }
                    });
                    let (h_g, n_g) = hg.join().unwrap_or_default();
                    let (h_y, n_y) = hy.join().unwrap_or_default();
                    let (h_e, n_e) = he.join().unwrap_or_default();
                    let (h_q, n_q) = hq.join().unwrap_or_default();
                    let (h_s, n_s) = hs.join().unwrap_or_default();
                    let (h_m, n_m) = hm.join().unwrap_or_default();
                    let mut merge = |h: Vec<(String, String)>,
                                     neden: Option<String>,
                                     etiket: &str,
                                     kaynak: &str| {
                        if !h.is_empty() {
                            let mut n = 0;
                            for (t, u) in h {
                                cands.push(research::Candidate {
                                    title: t,
                                    url: u,
                                    snippet: String::new(),
                                    source: kaynak.into(),
                                    depth: 0,
                                    page: String::new(),
                                });
                                n += 1;
                            }
                            sources.push(format!("{etiket}({n})"));
                            silent.retain(|s| s != etiket);
                        } else if let Some(n) = neden {
                            // Başarısız hasat rapora iz bırakır (BEKLENEN'e dokunmaz, free-form).
                            silent.push(n);
                        }
                    };
                    merge(h_g, n_g, "Google-H", "harvest-google");
                    merge(h_y, n_y, "Yandex-H", "harvest-yandex");
                    merge(h_e, n_e, "Ecosia-H", "harvest-ecosia");
                    merge(h_q, n_q, "Qwant-H", "harvest-qwant");
                    merge(h_s, n_s, "Startpage-H", "harvest-startpage");
                    merge(h_m, n_m, "Mojeek-H", "harvest-mojeek");
                    let was_cached = false;
                    let offline = cands.is_empty();
                    let cands = if offline {
                        research::mock_candidates(&query)
                    } else {
                        cands
                    };
                    let mut srcs = sources;
                    if offline {
                        srcs = vec!["çevrimdışı-yedek".to_string()];
                    }
                    let (snap_net, model) = match st.lock() {
                        Ok(s) => (
                            s.net.clone(),
                            neural::ModelInfo {
                                version: s.version,
                                clicks: s.clicks,
                                trained: true,
                            },
                        ),
                        Err(_) => (
                            neural::net(),
                            neural::ModelInfo {
                                version: 0,
                                clicks: 0,
                                trained: false,
                            },
                        ),
                    };
                    let results = research::rank(&research::NeuralRank, &snap_net, &query, cands);
                    let report = research::Report {
                        candidates: results.len(),
                        sources: srcs,
                        elapsed_ms: t0.elapsed().as_millis(),
                        engine: "mlp-8-16-8-1 + bm25/cos".to_string(),
                        net: snap_net,
                        model,
                        depth: if deep { 1 } else { 0 },
                        silent,
                        cost_usd: (cost * 100000.0).round() / 100000.0,
                        cached: was_cached,
                        results,
                        query,
                    };
                    if let Ok(json) = serde_json::to_string(&report) {
                        let _ = proxy.send_event(UserEvent::ResearchResult(json));
                    }
                });
            }
        })
        .build(&window)
        .expect("webview açılamadı");

    event_loop.run(move |event, event_loop, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => (),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::ResearchResult(json)) => {
                let js = format!("window.__noralPush({})", json);
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::TestResult(json)) => {
                let js = format!("window.__noralTest({})", json);
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::KeyStatus(json)) => {
                let js = format!("window.__noralKeys({})", json);
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::AgentProgress(json)) => {
                let js = format!("window.__noralAgentProgress({})", json);
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::AgentResult(json)) => {
                let js = format!("window.__noralAgentResult({})", json);
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(UserEvent::OpenTab(url)) => {
                // Son kapı: güvensiz şema açılmaz, durum satırında uyarılır.
                if !url_izinli(&url) {
                    let _ = webview.evaluate_script(
                        "document.getElementById('stxt').textContent='engellendi: güvensiz şema'",
                    );
                    return;
                }
                let esc = url.replace('\\', "\\\\").replace('"', "\\\"");
                let js = format!("window.__noralOpen(\"{}\")", esc);
                let _ = webview.evaluate_script(&js);
            }
            // Deneysel hasatçı: gizli pencerede gerçek tarayıcıyla aç.
            Event::UserEvent(UserEvent::HarvestReq { id, url, tx }) => {
                if harvest_slots.len() >= 3 {
                    let _ = tx.send("{\"error\":\"hasat yoğunluğu\"}".to_string());
                    return;
                }
                let win = match WindowBuilder::new()
                    .with_title("noral-hasat")
                    .with_visible(false)
                    .with_inner_size(LogicalSize::new(1280.0, 900.0))
                    .build(event_loop)
                {
                    Ok(w) => w,
                    Err(_) => {
                        let _ = tx.send("{\"error\":\"pencere açılamadı\"}".to_string());
                        return;
                    }
                };
                let px = proxy_loop.clone();
                // Google URL'sinde çerez aşısı + hasat birlikte koşar.
                let init = if url.contains("google.") {
                    format!("{GOOGLE_INIT}{HARVEST_JS}")
                } else {
                    HARVEST_JS.to_string()
                };
                let view = match WebViewBuilder::new()
                    .with_url(&url)
                    .with_initialization_script(&init)
                    .with_ipc_handler(move |req: wry::http::Request<String>| {
                        let body = req.body().clone();
                        if body.contains("\"title\"") || body.contains("\"error\"") {
                            let _ = px.send_event(UserEvent::HarvestDone { id, json: body });
                        }
                    })
                    .build(&win)
                {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = tx.send("{\"error\":\"görünüm açılamadı\"}".to_string());
                        return;
                    }
                };
                harvest_slots.insert(id, HarvestSlot { _win: win, _view: view, tx });
            }
            Event::UserEvent(UserEvent::HarvestDone { id, json }) => {
                if let Some(slot) = harvest_slots.remove(&id) {
                    let _ = slot.tx.send(json);
                }
            }
            Event::UserEvent(UserEvent::HarvestTimeout { id }) => {
                // Yuvayı düşürmeden önce bekleyen alıcıya haber ver (kör beklemesin).
                // Süresiz sayı yok — ileti tüm harvest-search beklemeleriyle tutarlı.
                if let Some(slot) = harvest_slots.remove(&id) {
                    let _ = slot.tx.send("{\"error\":\"hasat zaman aşımı\"}".to_string());
                }
            }
            _ => (),
        }
    });
}

#[cfg(test)]
mod harvest_tests {
    use super::*;

    #[test]
    fn hasat_link_suzgeci() {
        // Karışık hasat çıktısı: 2 geçerli + elenenler.
        let json = r#"{"title":"test arama","text":"x","meta":"","links":[
            {"t":"Birinci Başlık","u":"https://example.com/sayfa1"},
            {"t":"","u":"https://ornek.com/bos-baslik"},
            {"t":"Google iç","u":"https://www.google.com/url?q=https://example.com/cozulmus%3Fid%3D5&sa=U"},
            {"t":"Reklam","u":"https://www.google.com/aclk?sa=L"},
            {"t":"Boş","u":""},
            {"t":"JS","u":"javascript:void(0)"},
            {"t":"Resim","u":"https://example.com/kedi.jpg"}
        ]}"#;
        let r = parse_harvest_links(json);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], ("Birinci Başlık".to_string(), "https://example.com/sayfa1".to_string()));
        // Başlık boşsa URL başlık olur.
        assert_eq!(r[1].0, "https://ornek.com/bos-baslik");
        // /url?q= çözülür.
        assert_eq!(r[2].1, "https://example.com/cozulmus?id=5");
        // Hata JSON'u sessiz boş döner.
        assert!(parse_harvest_links(r#"{"error":"hasat yoğunluğu"}"#).is_empty());
        // Bozuk JSON sessiz boş döner.
        assert!(parse_harvest_links("bu json değil").is_empty());
    }

    #[test]
    fn hasat_consent_bos_doner() {
        // Rıza/duvar sayfası: başlıkta Consent geçerse boş.
        let consent = r#"{"title":"Consent","text":"...","links":[
            {"t":"x","u":"https://consent.google.com/x"}
        ]}"#;
        assert!(parse_harvest_links(consent).is_empty());
        // Sorry sayfası da boş döner.
        let sorry = r#"{"title":"Sorry...","text":"captcha","links":[
            {"t":"a","u":"https://www.google.com/sorry/x"}
        ]}"#;
        assert!(parse_harvest_links(sorry).is_empty());
        // Hep google.com linkleri elenince boş kalır.
        let hep_google = r#"{"title":"test","text":"x","links":[
            {"t":"a","u":"https://www.google.com/search?q=test"},
            {"t":"b","u":"https://consent.google.com/m"}
        ]}"#;
        assert!(parse_harvest_links(hep_google).is_empty());
    }

    #[test]
    fn hasat_bos_nedeni() {
        // Yoğunluk hatası → yoğun izi.
        assert_eq!(
            harvest_bos_neden(Some(r#"{"error":"hasat yoğunluğu"}"#), "Google-H"),
            "Google-H(yoğun)"
        );
        // Timeout bildirimi (HarvestTimeout yuvası) → zaman-aşımı izi.
        assert_eq!(
            harvest_bos_neden(Some(r#"{"error":"hasat zaman aşımı"}"#), "Yandex-H"),
            "Yandex-H(zaman-aşımı)"
        );
        // Alıcı hiç gövde görmedi (recv zaman aşımı) → zaman-aşımı izi.
        assert_eq!(harvest_bos_neden(None, "Google-H"), "Google-H(zaman-aşımı)");
        // Duvar/consent ya da elenip boş kalan → duvar izi.
        assert_eq!(
            harvest_bos_neden(Some(r#"{"title":"Consent","text":"...","links":[]}"#), "Google-H"),
            "Google-H(duvar)"
        );
        assert_eq!(harvest_bos_neden(Some("bu json değil"), "Google-H"), "Google-H(duvar)");
        // Dolu hasatta neden üretilmez (çağıran is_empty ile kapılar).
        assert!(!parse_harvest_links(
            r#"{"title":"t","links":[{"t":"A","u":"https://example.com/a"}]}"#
        ).is_empty());
        // Yandex yönlendirmesi çözülür.
        let y = parse_harvest_links(
            r#"{"title":"yandex","links":[{"t":"T","u":"https://yandex.com.tr/r?text=q&u=https%3A%2F%2Fornek.com%2F1&l=x"}]}"#,
        );
        assert_eq!(y.len(), 1);
        assert_eq!(y[0].1, "https://ornek.com/1");
        // Çerez aşısı sabiti CONSENT içerir.
        assert!(GOOGLE_INIT.contains("CONSENT"));
    }
}
