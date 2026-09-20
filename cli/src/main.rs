//! NoralWeb CLI — çekirdeğin terminal cephesi (Windows + Linux).
//!
//! Çekirdek (fetch/research/neural/nim) masaüstünden #[path] ile alınır:
//! kopya YOK, masaüstü gelişince CLI otomatik güncellenir.
//! GUI bağımlılığı yok (ureq-only) — her yerde derlenir.
//!
//! Kullanım:
//!   noral-cli "sorgu" [--fast] [--apx0|--apx1|--apx2] [--limit N] [--json]
//!   noral-cli --agent "soru" [--osint]
//!   noral-cli --testmode
//!   noral-cli --help

#[path = "../../src/fetch.rs"]
mod fetch;
#[path = "../../src/research.rs"]
mod research;
#[path = "../../src/neural.rs"]
mod neural;
#[path = "../../src/nim.rs"]
mod nim;

fn yardim() {
    println!("noral-cli v0.37.0 — nöral araştırma motoru (terminal)");
    println!();
    println!("  noral \"sorgu\" [--fast] [--apx0|--apx1|--apx2] [--limit N] [--json]");
    println!("  noral --agent \"soru\" [--osint]");
    println!("  noral --testmode");
    println!();
    println!("Araştırma: 70+ canlı kaynak → MLP sıralama → rapor.");
    println!("Ajan: NIM anahtarı gerekir (NIM_KEY env veya anahtar dosyası).");
}

fn arastir(query: &str, deep: bool, apx: u8, limit: usize, json: bool) {
    use std::time::Instant;
    let t0 = Instant::now();
    let snap = neural::load_or_train();
    let (cands, sources, silent, cost) = fetch::live_search(query, deep, apx);
    let results = research::rank(&research::NeuralRank, &snap.net, query, cands);
    if json {
        let rep = serde_json::json!({
            "query": query,
            "candidates": results.len(),
            "sources": sources,
            "silent": silent,
            "elapsed_ms": t0.elapsed().as_millis(),
            "cost_usd": cost,
            "results": results.iter().take(limit).map(|r| serde_json::json!({
                "title": r.title, "url": r.url, "snippet": r.snippet,
                "source": r.source, "score": r.score,
                "neural": r.detail.neural, "classic": r.detail.classic_norm,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&rep).unwrap_or_default());
        return;
    }
    println!("{} sonuç · {} aday · {} ms{}", results.len(), results.len(), t0.elapsed().as_millis(), if cost > 0.0 { format!(" · ~${}", cost) } else { " · ücretsiz".to_string() });
    println!("kaynaklar: {}", sources.join(" | "));
    if !silent.is_empty() {
        println!("suskun: {}", silent.join(" | "));
    }
    println!();
    for (i, r) in results.iter().take(limit).enumerate() {
        println!("{}. [{:.2}] {}", i + 1, r.score, r.title);
        println!("   {} · {}", r.url, r.source);
        if !r.snippet.is_empty() {
            let s: String = r.snippet.chars().take(220).collect();
            println!("   {}", s);
        }
    }
}

fn testmodu() {
    let s = neural::load_or_train();
    let probes = neural::self_test(&s.net);
    let pass = probes.iter().filter(|p| p.pass).count();
    println!("{} · {} parametre · prob {}/{}", s.net.arch, s.net.params, pass, probes.len());
    println!("eğitim: {} çift · model v{} · {} tıklama", neural::BASE_PAIRS.len(), s.version, s.clicks);
    for p in &probes {
        println!("[{}] {} → {} ({})", if p.pass { "PASS" } else { "FAIL" }, p.name, p.output, p.expect);
    }
}

fn ajan(message: &str, mode: &str) {
    let key = match nim::nim_key() {
        Some(k) => k,
        None => {
            eprintln!("NIM anahtarı yok: NIM_KEY env ver ya da masaüstünden kaydet.");
            std::process::exit(2);
        }
    };
    let snap = neural::load_or_train();
    let mut sys = nim::sys_prompt(mode);
    sys.push_str("\nCLI NOTU: harvest ve sekme araçları YOK; open_tab yerine URL'leri yanıtta KANIT LİNKLERİ olarak listele, fetch_page ile oku.");
    let mut messages = vec![nim::Msg {
        role: "system".into(),
        content: sys,
        tool_calls: None,
        tool_call_id: None,
    }];
    messages.push(nim::Msg {
        role: "user".into(),
        content: message.chars().take(2000).collect(),
        tool_calls: None,
        tool_call_id: None,
    });
    let tools = nim::tools_schema(false);
    let mut evidence: Vec<String> = Vec::new();
    let mut seen_q = std::collections::HashSet::new();
    for _step in 0..6 {
        let reply = match nim::chat(&key, nim::DEFAULT_MODEL, &messages, &tools) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("model hatası: {}", e);
                break;
            }
        };
        if reply.tool_calls.is_empty() {
            if reply.content.trim().is_empty() && !evidence.is_empty() {
                println!("bulgular:\n{}", evidence.join("\n"));
            } else {
                println!("{}", reply.content);
            }
            return;
        }
        eprintln!(
            "[adım] {}",
            reply.tool_calls.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(", ")
        );
        messages.push(nim::Msg {
            role: "assistant".into(),
            content: reply.content.clone(),
            tool_calls: Some(reply.tool_calls.iter().map(|t| t.to_out()).collect()),
            tool_call_id: None,
        });
        for tc in &reply.tool_calls {
            let out = match tc.name.as_str() {
                "web_search" => {
                    let q = tc.args.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    let qn = q.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
                    if !seen_q.insert(qn) {
                        serde_json::json!({"error": "bu sorguyu zaten aradın."}).to_string()
                    } else {
                        let (cands, _, _, _) = fetch::live_search(q, false, 1);
                        let ranked = research::rank(&research::NeuralRank, &snap.net, q, cands);
                        for c in ranked.iter().take(5) {
                            if evidence.len() < 10 {
                                evidence.push(format!(
                                    "- {} ({})",
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
                                })
                            })
                            .collect();
                        serde_json::json!({ "results": top }).to_string()
                    }
                }
                "fetch_page" => {
                    let u = tc.args.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    match fetch::fetch_page(u) {
                        Some(p) => {
                            if evidence.len() < 10 {
                                evidence.push(format!(
                                    "- SAYFA {}: {}",
                                    p.title.chars().take(80).collect::<String>(),
                                    p.text.chars().take(300).collect::<String>()
                                ));
                            }
                            serde_json::json!({
                                "title": p.title.chars().take(200).collect::<String>(),
                                "text": p.text.chars().take(2500).collect::<String>(),
                            })
                            .to_string()
                        }
                        None => serde_json::json!({"error": "sayfa çekilemedi"}).to_string(),
                    }
                }
                _ => serde_json::json!({"note": "CLI'de sekme/harvest yok; URL'leri yanıtta listele."}).to_string(),
            };
            messages.push(nim::Msg {
                role: "tool".into(),
                content: out,
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
            });
        }
    }
    if !evidence.is_empty() {
        println!("bulgular:\n{}", evidence.join("\n"));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("noral-cli v0.37.0");
        return;
    }
    if args.is_empty() {
        yardim();
        // Çift tıklamayla açanlar okuyabilsin (terminalde Enter'a basılır, zararsız).
        #[cfg(windows)]
        {
            use std::io::BufRead;
            let _ = std::io::stdin().lock().lines().next();
        }
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        yardim();
        return;
    }
    if args.iter().any(|a| a == "--testmode") {
        testmodu();
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--agent") {
        let msg = args.get(i + 1).cloned().unwrap_or_default();
        if msg.trim().is_empty() || msg.starts_with("--") {
            eprintln!("kullanım: noral-cli --agent \"soru\" [--osint]");
            std::process::exit(2);
        }
        let mode = if args.iter().any(|a| a == "--osint") { "osint" } else { "arastirma" };
        ajan(&msg, mode);
        return;
    }
    // Araştırma: ilk bayrak-olmayan argüman sorgudur.
    let query = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_default();
    if query.trim().is_empty() {
        yardim();
        std::process::exit(2);
    }
    let deep = !args.iter().any(|a| a == "--fast");
    let apx = if args.iter().any(|a| a == "--apx0") {
        0
    } else if args.iter().any(|a| a == "--apx2") {
        2
    } else {
        1
    };
    let limit = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 50);
    let json = args.iter().any(|a| a == "--json");
    arastir(&query, deep, apx, limit, json);
}
