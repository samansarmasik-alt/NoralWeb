//! NoralWeb Android çekirdeği — JNI üzerinden Kotlin'e JSON servis eder.
//!
//! Çekirdek (fetch/research/neural/nim) masaüstünden #[path] ile alınır:
//! kopya YOK, masaüstü gelişince APK otomatik güncellenir.
//! GUI bağımlılığı yok (ureq-only + jni) — `aarch64-linux-android` .so olur.
//!
//! Protokol masaüstü IPC ile aynıdır, sadece taşıma farklı:
//!   Kotlin `ipc.postMessage(JSON)` → burada bg thread → JSON döner →
//!   Kotlin `evaluateJavascript("window.__noralPush(<json>)")` ile sayfaya iter.
//! Gizli-tarayıcı hasadı YOK (ureq sonuçları + harvest-kapalı ajan).

#[path = "../../src/fetch.rs"]
mod fetch;
#[path = "../../src/research.rs"]
mod research;
#[path = "../../src/neural.rs"]
mod neural;
#[path = "../../src/nim.rs"]
mod nim;

use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;
use std::time::Instant;

// ---------- küçük JNI yardımcıları (panik yok: hepsi varsayılanlı) ----------

fn al(env: &mut JNIEnv, j: &JString) -> String {
    env.get_string(j).map(String::from).unwrap_or_default()
}

fn ver(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map(|o| o.into_raw()).unwrap_or(std::ptr::null_mut())
}

fn hata(neden: &str) -> String {
    serde_json::json!({"error": neden}).to_string()
}

/// Servis anahtar dosyası (masaüstü key_path ile aynı harita).
fn anahtar_yolu(servis: &str) -> std::path::PathBuf {
    if servis == "nim" {
        return nim::nim_key_path();
    }
    let dosya = match servis {
        "apinex" => "apinex-key.txt",
        "exa" => "exa-key.txt",
        "tavily" => "tavily-key.txt",
        "serper" => "serper-key.txt",
        "langsearch" => "langsearch-key.txt",
        _ => "brave-key.txt",
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join(dosya);
            if p.exists() {
                return p;
            }
        }
    }
    nim::appdata_dir().join(dosya)
}

fn anahtar_var(servis: &str) -> bool {
    std::fs::read_to_string(anahtar_yolu(servis))
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

fn anahtar_durumu(kaydedildi: bool) -> String {
    serde_json::json!({
        "saved": kaydedildi,
        "brave": anahtar_var("brave"),
        "apinex": anahtar_var("apinex"),
        "exa": anahtar_var("exa"),
        "tavily": anahtar_var("tavily"),
        "serper": anahtar_var("serper"),
        "langsearch": anahtar_var("langsearch"),
        "nim": nim::nim_key().is_some(),
    })
    .to_string()
}

/// Sekme URL izni (masaüstüyle aynı: http/https/about:blank).
fn url_izinli(u: &str) -> bool {
    let l = u.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l == "about:blank"
}

// ---------- iç akışlar (masaüstü main.rs ile aynı mantık, hasatsız) ----------

fn akis_arastir(sorgu: &str, derin: bool, apx: u8) -> String {
    let t0 = Instant::now();
    let (cands, sources, silent, cost) = fetch::live_search(sorgu, derin, apx);
    let cevrimdisi = cands.is_empty();
    let cands = if cevrimdisi { research::mock_candidates(sorgu) } else { cands };
    let srcs =
        if cevrimdisi { vec!["çevrimdışı-yedek".to_string()] } else { sources };
    let snap = neural::load_or_train();
    let results = research::rank(&research::NeuralRank, &snap.net, sorgu, cands);
    let rapor = research::Report {
        query: sorgu.to_string(),
        candidates: results.len(),
        sources: srcs,
        elapsed_ms: t0.elapsed().as_millis(),
        engine: "mlp-8-16-8-1 + bm25/cos".to_string(),
        net: snap.net.clone(),
        model: neural::ModelInfo {
            version: snap.version,
            clicks: snap.clicks,
            trained: true,
        },
        depth: if derin { 1 } else { 0 },
        silent,
        cost_usd: (cost * 100000.0).round() / 100000.0,
        cached: false,
        results,
    };
    serde_json::to_string(&rapor).unwrap_or_else(|_| hata("rapor kodlanamadı"))
}

fn akis_ajan(mesaj: &str, mod_: &str, gecmis_json: &str, apx: u8) -> String {
    let mode = if mod_ == "osint" { "osint" } else { "arastirma" };
    let key = match nim::nim_key() {
        Some(k) => k,
        None => {
            return serde_json::json!({
                "mode": mode,
                "model": nim::DEFAULT_MODEL,
                "answer": "NIM anahtarı yok. Ayarlardan anahtarını kaydet, sonra tekrar dene.",
                "steps": [],
                "need_key": true,
            })
            .to_string()
        }
    };
    // Hasat YOK: araç şeması harvest'siz, modele de söylenmez.
    let tools = nim::tools_schema(false);
    let mut messages = vec![nim::Msg {
        role: "system".into(),
        content: nim::sys_prompt(mode),
        tool_calls: None,
        tool_call_id: None,
    }];
    // Geçmiş: [{role, content}] (en fazla 10, 2000 kırpık).
    let gecmis: Vec<serde_json::Value> =
        serde_json::from_str(gecmis_json).unwrap_or_default();
    let mut gecmis = gecmis;
    while gecmis
        .last()
        .and_then(|h| h.get("content"))
        .and_then(|c| c.as_str())
        .map(|c| c.trim() == mesaj.trim() && !mesaj.trim().is_empty())
        .unwrap_or(false)
    {
        gecmis.pop();
    }
    for h in gecmis.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
        let rol = h.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        messages.push(nim::Msg {
            role: if rol == "assistant" || rol == "tool" { rol.into() } else { "user".into() },
            content: h
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .chars()
                .take(2000)
                .collect(),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.push(nim::Msg {
        role: "user".into(),
        content: mesaj.chars().take(2000).collect(),
        tool_calls: None,
        tool_call_id: None,
    });
    let basla = Instant::now();
    let mut steps: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
    let mut opened: Vec<String> = Vec::new();
    let mut searches = 0u32;
    let mut fetched = false;
    let snap_net = neural::load_or_train().net;
    let mut seen_q = std::collections::HashSet::new();
    let answer = loop {
        if basla.elapsed().as_secs() > 300 {
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
                }
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
                    if !seen_q.insert(qn) {
                        serde_json::json!({"error": "bu sorguyu zaten aradın (sonuçlar yukarıda). TEKRAR ARAMA YAPMA; fetch_page ile adayları oku ya da tamamen farklı bir açı dene."}).to_string()
                    } else {
                        searches += 1;
                        let (cands, _, _, _) = fetch::live_search(q, false, apx);
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
                // Sekme YOK: açılanlar listelenir, Kotlin dış tarayıcıda açar.
                "open_tab" => {
                    let u = tc.args.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if url_izinli(u) {
                        if opened.len() < 5 {
                            opened.push(u.to_string());
                        }
                        serde_json::json!({"opened": u, "note": "dış tarayıcıda açılacak"}).to_string()
                    } else {
                        serde_json::json!({"error": "geçersiz url"}).to_string()
                    }
                }
                "open_tabs" => {
                    let arr = tc.args.get("urls").and_then(|x| x.as_array());
                    if let Some(arr) = arr {
                        for it in arr.iter().take(5) {
                            if let Some(u) = it.as_str() {
                                if url_izinli(u) && opened.len() < 5 {
                                    opened.push(u.to_string());
                                }
                            }
                        }
                    }
                    serde_json::json!({"opened": opened, "count": opened.len()}).to_string()
                }
                "harvest" => {
                    serde_json::json!({"error": "deneysel hasat bu sürümde yok; fetch_page kullan"}).to_string()
                }
                other => serde_json::json!({"error": format!("bilinmeyen araç: {}", other)}).to_string(),
            };
            steps.push(serde_json::json!({"tool": tc.name, "args": arg_snip, "ok": !out.contains("error")}));
            messages.push(nim::Msg {
                role: "tool".into(),
                content: out.chars().take(4000).collect(),
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }
        if searches >= 4 && !fetched {
            fetched = true;
            messages.push(nim::Msg {
                role: "user".into(),
                content: "Yeterince adayın var. Yeni web_search YAPMA; en ilgili 2-3 sonucu fetch_page ile oku, sonra toparla.".into(),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    };
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
                    format!("Toparlama hatası: {}.", e)
                } else {
                    format!("Özet servisi yoruldu ({}). Ham bulgular:\n{}", e, evidence.join("\n"))
                }
            })
    } else {
        answer
    };
    let answer = if answer.trim().is_empty() {
        if evidence.is_empty() {
            "Bu konuda ücretsiz kaynaklarda bir şey bulunamadı.".to_string()
        } else {
            format!("Özet çıkarılamadı. Ham bulgular:\n{}", evidence.join("\n"))
        }
    } else {
        answer
    };
    serde_json::json!({
        "mode": mode,
        "model": nim::DEFAULT_MODEL,
        "answer": answer,
        "steps": steps,
        "evidence": evidence,
        "opened": opened,
        "cost_usd": (fetch::take_cost_usd() * 100000.0).round() / 100000.0,
    })
    .to_string()
}

// ---------- JNI dışa vurumları (com.noralweb.Core) ----------

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_init(
    mut env: JNIEnv,
    _cls: JClass,
    dizin: JString,
) {
    let d = al(&mut env, &dizin);
    if !d.is_empty() {
        std::env::set_var("NORAL_DATA", &d);
        let p = std::path::PathBuf::from(&d);
        let _ = std::fs::create_dir_all(&p);
        let _ = std::env::set_current_dir(&p);
        // Rust paniği prosesi öldürür; ölmeden sebebi dosyaya bırak (sonraki açılışta gösterilir).
        let yakala = d.clone();
        std::panic::set_hook(Box::new(move |bilgi| {
            let mut m = String::from("RUST PANIC: ");
            m.push_str(&bilgi.to_string());
            if let Some(konum) = bilgi.location() {
                m.push_str(&format!(" @ {}:{}", konum.file(), konum.line()));
            }
            let _ = std::fs::create_dir_all(&yakala);
            let _ = std::fs::write(std::path::PathBuf::from(&yakala).join("panic.log"), m);
        }));
    }
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_search(
    mut env: JNIEnv,
    _cls: JClass,
    sorgu: JString,
    derinlik: jint,
    apx: jint,
) -> jstring {
    let q = al(&mut env, &sorgu);
    if q.trim().is_empty() {
        return ver(&mut env, &hata("boş sorgu"));
    }
    let json = akis_arastir(q.trim(), derinlik != 0, apx.clamp(0, 2) as u8);
    ver(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_agent(
    mut env: JNIEnv,
    _cls: JClass,
    mesaj: JString,
    mod_: JString,
    gecmis: JString,
    apx: jint,
) -> jstring {
    let m = al(&mut env, &mesaj);
    if m.trim().is_empty() {
        return ver(&mut env, &hata("boş mesaj"));
    }
    let json = akis_ajan(&m, &al(&mut env, &mod_), &al(&mut env, &gecmis), apx.clamp(0, 2) as u8);
    ver(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_testmode(mut env: JNIEnv, _cls: JClass) -> jstring {
    let s = neural::load_or_train();
    let probes = neural::self_test(&s.net);
    let pass = probes.iter().filter(|p| p.pass).count();
    let json = serde_json::json!({
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
    })
    .to_string();
    ver(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_click(
    mut env: JNIEnv,
    _cls: JClass,
    feats: JString,
    atlanan: JString,
) {
    // Tıklamayla online öğrenme (masaüstü click IPC ile aynı).
    let tik: Vec<f32> = serde_json::from_str(&al(&mut env, &feats)).unwrap_or_default();
    let atl: Vec<Vec<f32>> = serde_json::from_str(&al(&mut env, &atlanan)).unwrap_or_default();
    if tik.len() != neural::N_IN || atl.is_empty() {
        return;
    }
    let mut tiklanan = [0f32; neural::N_IN];
    tiklanan.copy_from_slice(&tik);
    let mut gec: Vec<[f32; neural::N_IN]> = Vec::new();
    for s in atl {
        if s.len() == neural::N_IN {
            let mut a = [0f32; neural::N_IN];
            a.copy_from_slice(&s);
            gec.push(a);
        }
    }
    if gec.is_empty() {
        return;
    }
    let mut st = neural::load_or_train();
    neural::learn_click(&mut st.net, tiklanan, &gec);
    st.clicks += 1;
    st.version += 1;
    neural::save(&st);
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_saveKey(
    mut env: JNIEnv,
    _cls: JClass,
    servis: JString,
    anahtar: JString,
) -> jstring {
    let (service, key) = (al(&mut env, &servis), al(&mut env, &anahtar).trim().to_string());
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
        ok = std::fs::write(anahtar_yolu(&service), &key).is_ok();
    }
    ver(&mut env, &anahtar_durumu(ok))
}

#[no_mangle]
pub extern "system" fn Java_com_noralweb_Core_keyStatus(mut env: JNIEnv, _cls: JClass) -> jstring {
    ver(&mut env, &anahtar_durumu(false))
}
