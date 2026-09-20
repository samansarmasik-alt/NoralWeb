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

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

// ---------- renkli terminal sahne (sıfır bağımlılık, ANSI) ----------
const C_RST: &str = "\x1b[0m";
const C_B: &str = "\x1b[1m";
const C_CY: &str = "\x1b[36m";
const C_MG: &str = "\x1b[35m";
const C_YL: &str = "\x1b[33m";
const C_DM: &str = "\x1b[90m";
const C_GN: &str = "\x1b[32m";
const C_RD: &str = "\x1b[31m";

const SURUM: &str = "0.37.0";

/// Renk kararı (çalışma-anı): boru/NO_COLOR/dumb uçta ANSI kapat.
fn renk_acik() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        use std::io::IsTerminal;
        std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
            && std::io::stdout().is_terminal()
    })
}

struct Pal {
    b: &'static str,
    cy: &'static str,
    mg: &'static str,
    yl: &'static str,
    dm: &'static str,
    gn: &'static str,
    rd: &'static str,
    rst: &'static str,
}

fn pal() -> Pal {
    if renk_acik() {
        Pal { b: C_B, cy: C_CY, mg: C_MG, yl: C_YL, dm: C_DM, gn: C_GN, rd: C_RD, rst: C_RST }
    } else {
        Pal { b: "", cy: "", mg: "", yl: "", dm: "", gn: "", rd: "", rst: "" }
    }
}

/// Terminal genişliği (COLUMNS yoksa 80), kartlar buna göre dizilir.
fn term_w() -> usize {
    std::env::var("COLUMNS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(80).clamp(64, 110)
}

fn kar_say(s: &str) -> usize {
    s.chars().count()
}

/// Uzunsa … ile kırp (Türkçe-güvenli: karakter sayar).
fn kisalt(s: &str, n: usize) -> String {
    if kar_say(s) <= n {
        return s.to_string();
    }
    let mut k: String = s.chars().take(n.saturating_sub(1)).collect();
    k.push('…');
    k
}

/// Kelime-bazlı satırlara böl (uzun tek kelimeyi kırpar).
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut sat = Vec::new();
    let mut cur = String::new();
    let mut n = 0usize;
    for kel in s.split_whitespace() {
        let kl = kar_say(kel);
        if kl >= w {
            if !cur.is_empty() {
                sat.push(std::mem::take(&mut cur));
                n = 0;
            }
            sat.push(kisalt(kel, w));
            continue;
        }
        if n > 0 && n + 1 + kl > w {
            sat.push(std::mem::take(&mut cur));
            n = 0;
        }
        if n > 0 {
            cur.push(' ');
            n += 1;
        }
        cur.push_str(kel);
        n += kl;
    }
    if !cur.is_empty() {
        sat.push(cur);
    }
    sat
}

/// 10 hücreli skor çubuğu: ████████░░
fn skor_bar(s: f64) -> String {
    let dolu = (s.clamp(0.0, 1.0) * 10.0).round() as usize;
    "█".repeat(dolu) + &"░".repeat(10usize.saturating_sub(dolu))
}

fn skor_renk(p: &Pal, s: f64) -> &'static str {
    if s >= 0.7 {
        p.gn
    } else if s >= 0.4 {
        p.yl
    } else {
        p.rd
    }
}

/// Kutu gövde satırı: `│ içerik (sağa boşluklu) │`. `duz` renksiz uzunluktur.
fn kutu_satir(p: &Pal, ic: usize, duz: usize, renkli: &str) {
    let bos = " ".repeat(ic.saturating_sub(1 + duz));
    println!("{}│{} {}{}{}│{}", p.b, p.rst, renkli, bos, p.b, p.rst);
}

/// Başlıklı kutu: üstte `╭─ başlık ──╮`, altta `╰──╯`.
fn panel(p: &Pal, w: usize, baslik: &str, satirlar: &[String]) {
    let ic = w.saturating_sub(2);
    let b = kisalt(baslik, ic.saturating_sub(4));
    let duz = format!("╭─ {} ", b);
    let dolgu = "─".repeat(ic.saturating_sub(kar_say(&duz)));
    println!("{}╭─ {}{}{} {}{}╮{}", p.b, p.cy, b, p.b, dolgu, p.rst, p.rst);
    for s in satirlar {
        kutu_satir(p, ic, kar_say(s), s);
    }
    println!("{}╰{}╯{}", p.b, "─".repeat(ic), p.rst);
}

/// Virgüllü liste kutusuz yazılır (en_cok satırı aşarsa toplamı söyler).
fn liste(p: &Pal, w: usize, etiket: &str, items: &[String], en_cok: usize) {
    if items.is_empty() {
        return;
    }
    let girinti = " ".repeat(kar_say(etiket) + 2);
    let satirlar = wrap(&items.join(" · "), w.saturating_sub(kar_say(etiket) + 4));
    for (i, s) in satirlar.iter().take(en_cok).enumerate() {
        if i == 0 {
            println!("{}{}:{} {}", p.dm, etiket, p.rst, s);
        } else {
            println!("{}{}", girinti, s);
        }
    }
    if satirlar.len() > en_cok {
        println!("{}{}… (toplam {})", girinti, p.dm, items.len());
    }
}

fn banner() {
    let p = pal();
    println!("{mg}{b}  ███╗   ██╗{cy} ██████╗ ██████╗  █████╗ ██╗     {rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{mg}{b}  ████╗  ██║{cy}██╔═══██╗██╔══██╗██╔══██╗██║     {rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{mg}{b}  ██╔██╗ ██║{cy}██║   ██║██████╔╝███████║██║     {rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{mg}{b}  ██║╚██╗██║{cy}██║   ██║██╔══██╗██╔══██║██║     {rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{mg}{b}  ██║ ╚████║{cy}╚██████╔╝██║  ██║██║  ██║███████╗{rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{mg}{b}  ╚═╝  ╚═══╝{cy} ╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝╚══════╝{rst}", mg = p.mg, b = p.b, cy = p.cy, rst = p.rst);
    println!("{dm}  ── nöral araştırma motoru · 70+ canlı kaynak · MLP sıralayıcı · v{SURUM} ──{rst}", dm = p.dm, rst = p.rst);
}

/// Animasyonlu bekleme imleci (süre sayaçlı). Dönen handle + durdurma bayrağı verir.
fn spin(mesaj: &str) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let m = mesaj.to_string();
    let h = std::thread::spawn(move || {
        let p = pal();
        let t0 = std::time::Instant::now();
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut i = 0usize;
        while !s2.load(Ordering::Relaxed) {
            print!(
                "\r{cy}{frm}{rst} {msg} {dm}{sn}sn{rst}",
                cy = p.cy,
                frm = frames[i % frames.len()],
                rst = p.rst,
                msg = m,
                dm = p.dm,
                sn = t0.elapsed().as_secs()
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::thread::sleep(Duration::from_millis(80));
            i += 1;
        }
        print!("\r\x1b[2K");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    });
    (h, stop)
}

fn open_url(u: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", u]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(u).spawn();
    }
    #[cfg(target_os = "android")]
    {
        // Termux: termux-open yoksa sessizce xdg-open dene.
        let ok = std::process::Command::new("termux-open").arg(u).spawn().is_ok();
        if !ok {
            let _ = std::process::Command::new("xdg-open").arg(u).spawn();
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos"), not(target_os = "android")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(u).spawn();
    }
}

/// Tek sonuç kartı: başlıklı üst çizgi + başlık + url/kaynak + 3 satıra kadar özet.
fn kart(p: &Pal, no: usize, r: &research::Ranked, w: usize) {
    let ic = w.saturating_sub(2);
    let skor = format!("{:.2}", r.score);
    let bar = skor_bar(r.score);
    let rk = skor_renk(p, r.score);
    let duz = format!("╭─ {} ─ [{}] {} ", no, skor, bar);
    let dolgu = "─".repeat(ic.saturating_sub(kar_say(&duz)));
    println!(
        "{b}╭─ {yl}{b}{no}{rst}{b} ─ [{rk}{skor}{rst}{b}] {rk}{bar}{rst}{b} {dolgu}╮{rst}",
        b = p.b, yl = p.yl, no = no, rst = p.rst, rk = rk, skor = skor, bar = bar, dolgu = dolgu
    );
    let baslik = kisalt(&r.title, ic.saturating_sub(1));
    kutu_satir(p, ic, kar_say(&baslik), &format!("{}{}{}{}", p.b, p.cy, baslik, p.rst));
    let url_duz = kisalt(&r.url, ic.saturating_sub(kar_say(&r.source) + 6));
    let url_renkli = format!("{}{}{} · {}{}{}", p.dm, url_duz, p.rst, p.mg, r.source, p.rst);
    kutu_satir(p, ic, kar_say(&format!("{} · {}", url_duz, r.source)), &url_renkli);
    for s in wrap(&r.snippet, ic.saturating_sub(1)).into_iter().take(3) {
        kutu_satir(p, ic, kar_say(&s), &s);
    }
    println!("{}╰{}╯{}", p.b, "─".repeat(ic), p.rst);
}

/// Sonuç açma istemi: numara → tarayıcıda aç, Enter/q → geri.
fn open_prompt(results: &[research::Ranked]) {
    use std::io::{BufRead, IsTerminal};
    if results.is_empty() || !std::io::stdin().is_terminal() {
        return;
    }
    let p = pal();
    loop {
        print!("{yl}❯{rst} no ile aç · Enter ile kapat: ", yl = p.yl, rst = p.rst);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let t = line.trim();
        if t.is_empty() || t == "q" {
            return;
        }
        match t.parse::<usize>() {
            Ok(n) if n >= 1 && n <= results.len() => {
                open_url(&results[n - 1].url);
                println!("{gn}✓ açıldı:{rst} {}", results[n - 1].url, gn = p.gn, rst = p.rst);
            }
            _ => println!("{rd}1-{len} arası bir numara yaz{rst}", rd = p.rd, len = results.len(), rst = p.rst),
        }
    }
}

fn yardim() {
    let p = pal();
    println!("{mg}{b}noral-cli{rst} {dm}v{SURUM} — nöral araştırma motoru (terminal){rst}", mg = p.mg, b = p.b, rst = p.rst, dm = p.dm);
    panel(&p, 72, "kullanım", &[
        "noral \"sorgu\" [--fast] [--apx0|--apx1|--apx2] [--limit N] [--json]".to_string(),
        "noral --agent \"soru\" [--osint]".to_string(),
        "noral --testmode".to_string(),
    ]);
    panel(&p, 72, "notlar", &[
        "Araştırma: 70+ canlı kaynak → MLP sıralama → kartlar.".to_string(),
        "Ajan: NIM anahtarı gerekir (NIM_KEY env veya anahtar dosyası).".to_string(),
    ]);
}

fn arastir(query: &str, deep: bool, apx: u8, limit: usize, json: bool) {
    use std::time::Instant;
    let t0 = Instant::now();
    let (spin_h, spin_stop) = if json {
        // JSON pipedir — animasyon yok.
        let h = std::thread::spawn(|| {});
        (h, Arc::new(AtomicBool::new(true)))
    } else {
        spin("kaynaklar taranıyor…")
    };
    let snap = neural::load_or_train();
    let (cands, sources, silent, cost) = fetch::live_search(query, deep, apx);
    let results = research::rank(&research::NeuralRank, &snap.net, query, cands);
    spin_stop.store(true, Ordering::Relaxed);
    let _ = spin_h.join();
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
    let p = pal();
    let w = term_w();
    let gosteren = results.len().min(limit);
    let ms = t0.elapsed().as_millis();
    let sure = if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}sn", ms as f64 / 1000.0)
    };
    let ucret = if cost > 0.0 { format!("~${:.4}", cost) } else { "ücretsiz".to_string() };
    panel(&p, w, &format!("\"{}\"", kisalt(query, w / 2)), &[format!(
        "{} sonuç · {} aday · {} kaynak · {} · {}",
        results.len(),
        results.len(),
        sources.len(),
        sure,
        ucret
    )]);
    liste(&p, w, "kaynaklar", &sources, 2);
    if !silent.is_empty() {
        liste(&p, w, "suskun", &silent, 1);
    }
    println!();
    for (i, r) in results.iter().take(limit).enumerate() {
        kart(&p, i + 1, r, w);
    }
    if gosteren > 0 {
        println!(
            "{dm}gösteriliyor: {g}/{t}{rst}",
            dm = p.dm,
            g = gosteren,
            t = results.len(),
            rst = p.rst
        );
    }
    open_prompt(&results[..gosteren]);
}

fn testmodu() {
    let p = pal();
    let s = neural::load_or_train();
    let probes = neural::self_test(&s.net);
    let pass = probes.iter().filter(|pr| pr.pass).count();
    panel(&p, 64, "çekirdek testi", &[
        format!("{} · {} parametre · prob {}/{}", s.net.arch, s.net.params, pass, probes.len()),
        format!("eğitim: {} çift · model v{} · {} tıklama", neural::BASE_PAIRS.len(), s.version, s.clicks),
    ]);
    for pr in &probes {
        let isaret = if pr.pass {
            format!("{}✓{}", p.gn, p.rst)
        } else {
            format!("{}✗{}", p.rd, p.rst)
        };
        println!("  {} {} → {} ({})", isaret, pr.name, pr.output, pr.expect);
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
    let p = pal();
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
    let mut adim = 0usize;
    for _step in 0..6 {
        let (sh, ss) = spin("model düşünüyor…");
        let sonuc = nim::chat(&key, nim::DEFAULT_MODEL, &messages, &tools);
        ss.store(true, Ordering::Relaxed);
        let _ = sh.join();
        let reply = match sonuc {
            Ok(r) => r,
            Err(e) => {
                eprintln!("model hatası: {}", e);
                break;
            }
        };
        if reply.tool_calls.is_empty() {
            if reply.content.trim().is_empty() && !evidence.is_empty() {
                println!("{}{}── bulgular ──{}", p.b, p.cy, p.rst);
                println!("{}", evidence.join("\n"));
            } else {
                println!("{}", reply.content);
            }
            return;
        }
        adim += 1;
        eprintln!(
            "{}◆ adım {}:{} {}",
            p.cy,
            adim,
            p.rst,
            reply.tool_calls.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(" + ")
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
        println!("{}{}── bulgular ──{}", p.b, p.cy, p.rst);
        println!("{}", evidence.join("\n"));
    }
}

/// Etkileşimli menü: düz `noral` yazınca açılır (arama kutulu mini arayüz).
fn menu() {
    use std::io::{BufRead, IsTerminal};
    if !std::io::stdin().is_terminal() {
        yardim();
        return;
    }
    banner();
    let p = pal();
    panel(&p, 58, "noral", &[
        "[1] Araştır    70+ canlı kaynakta tara".to_string(),
        "[2] Ajan       NIM ile derin soru-cevap".to_string(),
        "[3] Ağ testi   model + çekirdek kontrolü".to_string(),
        "[q] Çık".to_string(),
    ]);
    loop {
        print!("{cy}noral{rst} ❯ ", cy = p.cy, rst = p.rst);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        match line.trim() {
            "1" => {
                print!("{cy}sorgu{rst} ❯ ", cy = p.cy, rst = p.rst);
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut q = String::new();
                if std::io::stdin().lock().read_line(&mut q).unwrap_or(0) == 0 {
                    return;
                }
                let q = q.trim();
                if !q.is_empty() {
                    arastir(q, true, 1, 10, false);
                }
            }
            "2" => {
                print!("{cy}soru{rst} ❯ ", cy = p.cy, rst = p.rst);
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut m = String::new();
                if std::io::stdin().lock().read_line(&mut m).unwrap_or(0) == 0 {
                    return;
                }
                let m = m.trim();
                if !m.is_empty() {
                    ajan(m, "arastirma");
                }
            }
            "3" => testmodu(),
            "q" | "Q" | "quit" | "exit" => return,
            "" => {}
            diger => {
                // Sayı değilse direkt sorgu say (hızlı yol).
                arastir(diger, true, 1, 10, false);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("noral-cli v{SURUM}");
        return;
    }
    if args.is_empty() {
        menu();
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
