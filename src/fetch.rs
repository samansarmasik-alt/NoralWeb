//! Canlı aday toplayıcı — API anahtarsız, resmi uçlar + derin tarama.
//!
//! 1. halka (arama): DDG Instant Answer, Wikipedia (opensearch + fulltext),
//!   GitHub repo arama, Brave (exe yanında brave-key.txt varsa), DDG HTML (yedek).
//! 2. halka (derin): ilk halkanın sayfaları çekilir, sayfa metni adaya işlenir,
//!   sayfadaki dış linkler yeni aday olur (kaynak: "derin").
//!
//! Not: DDG/Ecosia/Mojeek HTML taraması bot engeline takılıyor; o yüzden
//! resmi JSON API'ler birincil kaynaktır.

use super::research::Candidate;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Apinex harcaması (mikro-dolar, arama başına sıfırlanır).
static API_COST_MICRO: AtomicU64 = AtomicU64::new(0);

fn add_cost_usd(v: &serde_json::Value, fallback: f64) {
    let usd = v
        .get("usage")
        .and_then(|u| u.get("cost_usd"))
        .and_then(|c| c.as_f64())
        .unwrap_or(fallback);
    API_COST_MICRO.fetch_add((usd * 1_000_000.0) as u64, Ordering::Relaxed);
}

/// Arama sonunda çağrılır: biriken maliyeti alır ve sıfırlar.
pub fn take_cost_usd() -> f64 {
    API_COST_MICRO.swap(0, Ordering::SeqCst) as f64 / 1_000_000.0
}

/// Dönen kullanıcı ajanları — bot imzasını dağıtmak için (deneysel hasat desteği).
const UAS: [&str; 3] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Edg/125.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
];

static UA_IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn ua_rot() -> &'static str {
    let i = UA_IDX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    UAS[i % UAS.len()]
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(6))
        .build()
}

fn agent_fast() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()
}

/// Uzun kuyruk: ilk atışta 6sn'yi aşan yavaş uçlar (Marginalia + OpenLib) için.
/// SADECE bu iki src fns kullanır, diğerlerine dokunma.
const AGENT_LONG_SECS: u64 = 12;
fn agent_long() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(AGENT_LONG_SECS))
        .build()
}

/// 429/503/5xx + taşıma hatalarında beklemeli tekrar (ajan şikayeti #4).
fn with_retry<F>(mut f: F) -> Result<ureq::Response, ureq::Error>
where
    F: FnMut() -> Result<ureq::Response, ureq::Error>,
{
    let mut wait = 1u64;
    for attempt in 1..=3u32 {
        match f() {
            Ok(r) => return Ok(r),
            Err(ureq::Error::Status(code @ (429 | 500 | 502 | 503), _)) if attempt < 3 => {
                let _ = code;
                std::thread::sleep(Duration::from_secs(wait));
                wait *= 2;
            }
            Err(ureq::Error::Transport(_)) if attempt < 2 => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("retry döngüsü ya döner ya biter")
}

fn get_text(url: &str) -> Option<String> {
    with_retry(|| {
        agent()
            .get(url)
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json, text/html")
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()?
    .into_string()
    .ok()
}

/// En küçük percent-encode (sorgu için yeterli).
pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else if b == b' ' {
            o.push('+');
        } else {
            o.push_str(&format!("%{:02X}", b));
        }
    }
    o
}

fn dec(s: &str) -> String {
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

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `<tag ...>...</tag>` bloklarını at — büyük/küçük harf duyarsız, byte-güvenli.
/// (to_lowercase ile indekslemek Türkçe karakterde patlar, o yüzden elle tara.)
/// Bayt-düzeyinde etiket eşleşmesi — dilimleme yok, asla panik yapmaz.
fn tag_at(bytes: &[u8], pos: usize, tag: &str) -> bool {
    let t = tag.as_bytes();
    pos + t.len() <= bytes.len() && bytes[pos..pos + t.len()].eq_ignore_ascii_case(t)
}

fn cut_blocks(html: &str, tag: &str) -> String {
    let bytes = html.as_bytes();
    let n = bytes.len();
    let tl = tag.len();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < n {
        let is_open = i + 1 + tl <= n
            && bytes[i] == b'<'
            && bytes[i + 1] != b'/'
            && tag_at(bytes, i + 1, tag)
            && (i + 1 + tl >= n || !bytes[i + 1 + tl].is_ascii_alphanumeric());
        if is_open {
            // kapanışı ara
            let mut j = i + 1 + tl;
            let mut end = n;
            while j + 3 + tl <= n {
                if bytes[j] == b'<'
                    && bytes[j + 1] == b'/'
                    && tag_at(bytes, j + 2, tag)
                    && (j + 2 + tl >= n || !bytes[j + 2 + tl].is_ascii_alphanumeric())
                {
                    let mut k = j + 2 + tl;
                    while k < n && bytes[k] != b'>' {
                        k += 1;
                    }
                    end = (k + 1).min(n);
                    break;
                }
                j += 1;
            }
            i = end;
            out.push(' ');
        } else {
            let ch = html[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8().max(1);
        }
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                o.push(' ');
            }
            _ if !in_tag => o.push(ch),
            _ => {}
        }
    }
    let mut o: String = o.split_whitespace().collect::<Vec<_>>().join(" ");
    for (a, b) in [
        ("&amp;", "&"),
        ("&quot;", "\""),
        ("&#x27;", "'"),
        ("&#39;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
    ] {
        o = o.replace(a, b);
    }
    o.trim().to_string()
}

fn host_of(url: &str) -> String {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.split('/').next().unwrap_or(u).to_lowercase()
}

fn norm_url(url: &str) -> String {
    let mut u = url.trim().to_lowercase();
    while u.ends_with('/') {
        u.pop();
    }
    u
}

/// 1) DuckDuckGo Instant Answer (resmi API).
fn src_ddg_ia(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&lang=tr-tr",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let abs = v.get("AbstractText").and_then(|x| x.as_str()).unwrap_or("");
    let abs_url = v.get("AbstractURL").and_then(|x| x.as_str()).unwrap_or("");
    let mut n = 0;
    if !abs.is_empty() && !abs_url.is_empty() {
        out.push(Candidate {
            title: v.get("Heading").and_then(|x| x.as_str()).unwrap_or(query).to_string(),
            url: abs_url.to_string(),
            snippet: abs.to_string(),
            source: "ddg-özet".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if let Some(topics) = v.get("RelatedTopics").and_then(|x| x.as_array()) {
        for t in topics.iter().take(6) {
            let (text, url) = if t.get("Topics").is_some() {
                match t.get("Topics").and_then(|x| x.as_array()).and_then(|a| a.first()) {
                    Some(f) => (
                        f.get("Text").and_then(|x| x.as_str()).unwrap_or(""),
                        f.get("FirstURL").and_then(|x| x.as_str()).unwrap_or(""),
                    ),
                    None => continue,
                }
            } else {
                (
                    t.get("Text").and_then(|x| x.as_str()).unwrap_or(""),
                    t.get("FirstURL").and_then(|x| x.as_str()).unwrap_or(""),
                )
            };
            if text.is_empty() || url.is_empty() {
                continue;
            }
            n += 1;
            let mut parts = text.splitn(2, " - ");
            out.push(Candidate {
                title: parts.next().unwrap_or(text).to_string(),
                url: url.to_string(),
                snippet: parts.next().unwrap_or("").to_string(),
                source: "ddg".into(),
                depth: 0,
                page: String::new(),
            });
        }
    }
    if n > 0 {
        sources.push(format!("DuckDuckGo({})", n));
    }
}

/// 2) Wikipedia opensearch (TR + EN).
fn src_wiki_os(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    for lang in ["tr", "en"] {
        let Some(body) = get_text(&format!(
            "https://{}.wikipedia.org/w/api.php?action=opensearch&search={}&limit=6&namespace=0&format=json",
            lang,
            enc(query)
        )) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let (Some(t), Some(d), Some(u)) = (
            v.get(1).and_then(|x| x.as_array()),
            v.get(2).and_then(|x| x.as_array()),
            v.get(3).and_then(|x| x.as_array()),
        ) else {
            continue;
        };
        // Uzak diziler farklı boyda olabilir — en kısa boya kırp, OOB panic imkansız olsun.
        let n = t.len().min(d.len()).min(u.len()).min(6);
        for i in 0..n {
            let (ti, de, ur) = (
                t[i].as_str().unwrap_or(""),
                d[i].as_str().unwrap_or(""),
                u[i].as_str().unwrap_or(""),
            );
            if ti.is_empty() || ur.is_empty() {
                continue;
            }
            out.push(Candidate {
                title: ti.to_string(),
                url: ur.to_string(),
                snippet: if de.is_empty() {
                    "Wikipedia maddesi".to_string()
                } else {
                    de.to_string()
                },
                source: "wikipedia".into(),
                depth: 0,
                page: String::new(),
            });
        }
        if n > 0 {
            sources.push(format!("Wikipedia-{}({})", lang, n));
        }
        if lang == "tr" && n > 0 {
            break;
        }
    }
}

/// 3) Wikipedia tam-metin arama (opensearch'in kaçırdıklarını yakalar).
fn src_wiki_full(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    for lang in ["tr", "en"] {
        let Some(body) = get_text(&format!(
            "https://{}.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=8&srnamespace=0&format=json",
            lang,
            enc(query)
        )) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let Some(arr) = v
            .get("query")
            .and_then(|q| q.get("search"))
            .and_then(|s| s.as_array())
        else {
            continue;
        };
        let mut n = 0;
        for it in arr.iter().take(8) {
            let (ti, sn) = (
                it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                it.get("snippet").and_then(|x| x.as_str()).unwrap_or(""),
            );
            if ti.is_empty() {
                continue;
            }
            out.push(Candidate {
                title: ti.to_string(),
                url: format!(
                    "https://{}.wikipedia.org/wiki/{}",
                    lang,
                    enc(&ti.replace(' ', "_"))
                ),
                snippet: strip_tags(sn),
                source: "wikipedia".into(),
                depth: 0,
                page: String::new(),
            });
            n += 1;
        }
        if n > 0 && !sources.iter().any(|s| s.starts_with(&format!("WikiTam-{}", lang))) {
            sources.push(format!("WikiTam-{}({})", lang, n));
        }
        if lang == "tr" && n >= 3 {
            break;
        }
    }
}

/// 4) GitHub repo + kullanıcı arama (anahtarsız, dakikada 10 istek).
/// Hiçbir şey bulunamazsa bitişik yazımı da dener (HasanHaydarHizli gibi).
fn src_github(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    fn gh_get(url: &str) -> Option<serde_json::Value> {
        with_retry(|| {
            agent()
                .get(url)
                .set("User-Agent", ua_rot())
                .set("Accept", "application/vnd.github+json")
                .set("Accept-Charset", "utf-8")
                .call()
        })
        .ok()
        .and_then(|r| r.into_string().ok())
        .and_then(|b| serde_json::from_str(&b).ok())
    }
    fn harvest_repos(v: &serde_json::Value, out: &mut Vec<Candidate>, cap: usize) -> usize {
        let mut n = 0;
        if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
            for it in arr.iter().take(cap) {
                let (name, url, desc, stars) = (
                    it.get("full_name").and_then(|x| x.as_str()).unwrap_or(""),
                    it.get("html_url").and_then(|x| x.as_str()).unwrap_or(""),
                    it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
                    it.get("stargazers_count").and_then(|x| x.as_u64()).unwrap_or(0),
                );
                if name.is_empty() || url.is_empty() {
                    continue;
                }
                out.push(Candidate {
                    title: name.to_string(),
                    url: url.to_string(),
                    snippet: if desc.is_empty() {
                        format!("GitHub reposu · ★ {}", stars)
                    } else {
                        format!("{} · ★ {}", desc, stars)
                    },
                    source: "github".into(),
                    depth: 0,
                    page: String::new(),
                });
                n += 1;
            }
        }
        n
    }
    fn harvest_users(v: &serde_json::Value, out: &mut Vec<Candidate>, cap: usize) -> usize {
        let mut n = 0;
        if let Some(arr) = v.get("items").and_then(|x| x.as_array()) {
            for it in arr.iter().take(cap) {
                let (login, url) = (
                    it.get("login").and_then(|x| x.as_str()).unwrap_or(""),
                    it.get("html_url").and_then(|x| x.as_str()).unwrap_or(""),
                );
                if login.is_empty() || url.is_empty() {
                    continue;
                }
                out.push(Candidate {
                    title: format!("{} (GitHub profili)", login),
                    url: url.to_string(),
                    snippet: "GitHub kullanıcı profili".to_string(),
                    source: "github".into(),
                    depth: 0,
                    page: String::new(),
                });
                n += 1;
            }
        }
        n
    }
    let mut n = 0;
    let q1 = enc(query);
    if let Some(v) = gh_get(&format!(
        "https://api.github.com/search/repositories?q={}&per_page=5",
        q1
    )) {
        n += harvest_repos(&v, out, 5);
    }
    if let Some(v) = gh_get(&format!(
        "https://api.github.com/search/users?q={}&per_page=5",
        q1
    )) {
        n += harvest_users(&v, out, 5);
    }
    // Sıfır çekildiyse bitişik yazımı dene (kullanıcı adları için).
    if n == 0 {
        let squished: String = query.chars().filter(|c| c.is_alphanumeric()).collect();
        if squished.chars().count() >= 3 {
            let q2 = enc(&squished);
            if let Some(v) = gh_get(&format!(
                "https://api.github.com/search/users?q={}&per_page=3",
                q2
            )) {
                n += harvest_users(&v, out, 3);
            }
        }
    }
    if n > 0 {
        sources.push(format!("GitHub({})", n));
    }
}

/// 5) Brave Search — exe yanı öncelikli, yedek appdata (ücretsiz anahtar).
fn src_brave(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Ok(key) = std::fs::read_to_string(anahtar_yolu("brave-key.txt")) else {
        return;
    };
    let key = key.trim().to_string();
    if key.is_empty() {
        return;
    }
    let Some(body) = with_retry(|| {
        agent()
            .get(&format!(
                "https://api.search.brave.com/res/v1/web/search?q={}&count=10&text_decorations=false",
                enc(query)
            ))
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json")
            .set("Accept-Charset", "utf-8")
            .set("X-Subscription-Token", &key)
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
    else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(10) {
        let (ti, ur, de) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: ti.to_string(),
            url: ur.to_string(),
            snippet: de.to_string(),
            source: "brave".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Brave({})", n));
    }
}

/// 7) Wikidata varlık arama (TR + EN) — kavram/kişi tanımlarında güçlü.
fn src_wikidata(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let mut n = 0;
    for lang in ["tr", "en"] {
        let Some(body) = get_text(&format!(
            "https://www.wikidata.org/w/api.php?action=wbsearchentities&search={}&language={}&limit=5&format=json",
            enc(query),
            lang
        )) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let Some(arr) = v.get("search").and_then(|x| x.as_array()) else {
            continue;
        };
        for it in arr.iter().take(5) {
            let (id, label, desc) = (
                it.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                it.get("label").and_then(|x| x.as_str()).unwrap_or(""),
                it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
            );
            if id.is_empty() || label.is_empty() {
                continue;
            }
            out.push(Candidate {
                title: label.to_string(),
                url: format!("https://www.wikidata.org/wiki/{}", id),
                snippet: if desc.is_empty() {
                    "Wikidata kaydı".to_string()
                } else {
                    desc.to_string()
                },
                source: "wikidata".into(),
                depth: 0,
                page: String::new(),
            });
            n += 1;
        }
        if n >= 4 {
            break;
        }
    }
    if n > 0 {
        sources.push(format!("Wikidata({})", n));
    }
}

/// 8) StackOverflow — teknik sorularda altın değerinde.
fn src_stack(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://api.stackexchange.com/2.3/search/advanced?order=desc&sort=relevance&q={}&site=stackoverflow&pagesize=6",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(6) {
        let (ti, ur, sc, an) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("link").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("score").and_then(|x| x.as_i64()).unwrap_or(0),
            it.get("answer_count").and_then(|x| x.as_i64()).unwrap_or(0),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: strip_tags(ti),
            url: ur.to_string(),
            snippet: format!("StackOverflow · ▲{} · {} yanıt", sc, an),
            source: "stack".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Stack({})", n));
    }
}

/// 9) Hacker News (Algolia) — teknoloji gündemi.
fn src_hn(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://hn.algolia.com/api/v1/search?query={}&tags=story",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("hits").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(6) {
        let (ti, pts, com, oid) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("points").and_then(|x| x.as_i64()).unwrap_or(0),
            it.get("num_comments").and_then(|x| x.as_i64()).unwrap_or(0),
            it.get("objectID").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() {
            continue;
        }
        let url = it
            .get("url")
            .and_then(|x| x.as_str())
            .filter(|u| !u.is_empty())
            .map(|u| u.to_string())
            .unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", oid));
        out.push(Candidate {
            title: ti.to_string(),
            url,
            snippet: format!("Hacker News · ▲{} · {} yorum", pts, com),
            source: "hackernews".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("HN({})", n));
    }
}

/// 10) OpenAlex — akademik makaleler (anahtarsız).
fn src_openalex(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: OpenAlex 5→10
        "https://api.openalex.org/works?search={}&per-page=10&mailto=noral@example.com",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    // limit artışı: OpenAlex 5→10
    for it in arr.iter().take(10) {
        let (ti, doi, year, cited) = (
            it.get("display_name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("doi").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("publication_year").and_then(|x| x.as_i64()).unwrap_or(0),
            it.get("cited_by_count").and_then(|x| x.as_i64()).unwrap_or(0),
        );
        if ti.is_empty() || ti == "Unknown" {
            continue;
        }
        let url = if doi.is_empty() {
            it.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string()
        } else {
            format!("https://doi.org/{}", doi.trim_start_matches("https://doi.org/"))
        };
        if url.is_empty() {
            continue;
        }
        let authors: Vec<String> = it
            .get("authorships")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .take(3)
                    .filter_map(|au| {
                        au.get("author")
                            .and_then(|x| x.get("display_name"))
                            .and_then(|x| x.as_str())
                    })
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        out.push(Candidate {
            title: ti.to_string(),
            url,
            snippet: format!(
                "{} · {} · {} atıf",
                if authors.is_empty() {
                    "makale".to_string()
                } else {
                    authors.join(", ")
                },
                if year > 0 { year.to_string() } else { "?".into() },
                cited
            ),
            source: "akademik".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Akademik({})", n));
    }
}

/// 11) npm paketleri.
fn src_npm(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: npm 5→8
        "https://registry.npmjs.org/-/v1/search?text={}&size=8",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("objects").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    // limit artışı: npm 5→8
    for it in arr.iter().take(8) {
        let Some(p) = it.get("package") else { continue };
        let (name, ver, desc) = (
            p.get("name").and_then(|x| x.as_str()).unwrap_or(""),
            p.get("version").and_then(|x| x.as_str()).unwrap_or(""),
            p.get("description").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if name.is_empty() {
            continue;
        }
        let url = p
            .get("links")
            .and_then(|l| l.get("npm"))
            .and_then(|x| x.as_str())
            .map(|u| u.to_string())
            .unwrap_or_else(|| format!("https://www.npmjs.com/package/{}", name));
        out.push(Candidate {
            title: format!("{} {}", name, ver),
            url,
            snippet: if desc.is_empty() {
                "npm paketi".to_string()
            } else {
                desc.to_string()
            },
            source: "npm".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("npm({})", n));
    }
}

/// 12) crates.io (Rust paketleri).
fn src_crates(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: crates 5→8
        "https://crates.io/api/v1/crates?q={}&per_page=8",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("crates").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    // limit artışı: crates 5→8
    for it in arr.iter().take(8) {
        let (name, desc, dl) = (
            it.get("name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
        );
        if name.is_empty() {
            continue;
        }
        let url = it
            .get("repository")
            .and_then(|x| x.as_str())
            .filter(|u| u.starts_with("http"))
            .map(|u| u.to_string())
            .unwrap_or_else(|| format!("https://crates.io/crates/{}", name));
        out.push(Candidate {
            title: format!("{} (crate)", name),
            url,
            snippet: format!(
                "{} · {} indirme",
                if desc.is_empty() { "Rust paketi" } else { desc },
                dl
            ),
            source: "crates".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("crates({})", n));
    }
}

/// 13) arXiv (XML tarama).
fn src_arxiv(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: arXiv 4→10
        "https://export.arxiv.org/api/query?search_query=all:{}&max_results=10&sortBy=relevance",
        enc(query)
    )) else {
        return;
    };
    if !body.contains("<entry>") {
        return;
    }
    let mut n = 0;
    let mut pos = 0;
    while n < 10 {
        let s = match body[pos..].find("<entry>") {
            Some(i) => pos + i + 7,
            None => break,
        };
        let e = body[s..].find("</entry>").map(|i| s + i).unwrap_or(body.len());
        let block = &body[s..e];
        pos = e + 8;
        let field = |tag: &str| {
            let o = format!("<{}>", tag);
            let c = format!("</{}>", tag);
            block
                .find(&o)
                .and_then(|a| block[a + o.len()..].find(&c).map(|z| (a + o.len(), a + o.len() + z)))
                .map(|(a, b)| block[a..b].split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default()
        };
        let title = field("title");
        // <id> yazarlarda da geçer — entry bloğunun ilk link/id'si makalenindir
        let id = field("id");
        let summary = field("summary");
        if title.is_empty() || !id.starts_with("http") {
            continue;
        }
        out.push(Candidate {
            title,
            url: id,
            snippet: summary.chars().take(300).collect::<String>(),
            source: "arxiv".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("arXiv({})", n));
    }
}

/// 14) Mastodon kişi arama (anahtarsız, resmi API).
fn src_mastodon(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: Mastodon 5→10
        "https://mastodon.social/api/v2/search?q={}&resolve=false&limit=10",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("accounts").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    // limit artışı: Mastodon 5→10
    for it in arr.iter().take(10) {
        let (user, disp, url, note, fol) = (
            it.get("acct").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("display_name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("note").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("followers_count").and_then(|x| x.as_u64()).unwrap_or(0),
        );
        if user.is_empty() || url.is_empty() {
            continue;
        }
        let title = if disp.is_empty() {
            format!("@{} (Mastodon)", user)
        } else {
            format!("{} · @{} (Mastodon)", disp, user)
        };
        let bio = strip_tags(note);
        out.push(Candidate {
            title,
            url: url.to_string(),
            snippet: format!(
                "{} · {} takipçi",
                bio.chars().take(160).collect::<String>(),
                fol
            ),
            source: "mastodon".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Mastodon({})", n));
    }
}

/// 15) Bluesky kişi arama (anahtarsız, resmi API).
fn src_bsky(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: Bluesky 5→10
        "https://public.api.bsky.app/xrpc/app.bsky.actor.searchActors?q={}&limit=10",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("actors").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    // limit artışı: Bluesky 5→10
    for it in arr.iter().take(10) {
        let (handle, disp, desc) = (
            it.get("handle").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("displayName").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if handle.is_empty() {
            continue;
        }
        let title = if disp.is_empty() {
            format!("@{} (Bluesky)", handle)
        } else {
            format!("{} · @{} (Bluesky)", disp, handle)
        };
        out.push(Candidate {
            title,
            url: format!("https://bsky.app/profile/{}", handle),
            snippet: if desc.is_empty() {
                "Bluesky profili".to_string()
            } else {
                desc.chars().take(200).collect()
            },
            source: "bluesky".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Bluesky({})", n));
    }
}

/// 16) StackOverflow kullanıcı arama.
fn src_so_users(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://api.stackexchange.com/2.3/users?inname={}&site=stackoverflow&pagesize=5",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(5) {
        let (name, link, rep) = (
            it.get("display_name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("link").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("reputation").and_then(|x| x.as_i64()).unwrap_or(0),
        );
        if name.is_empty() || link.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: format!("{} (StackOverflow)", name),
            url: link.to_string(),
            snippet: format!("StackOverflow profili · {} repütasyon", rep),
            source: "stack".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("SO-kullanıcı({})", n));
    }
}

/// Anahtar yolu: önce exe yanı, yoksa %APPDATA%/NoralWeb (OneDrive senkron derdine çare).
fn anahtar_yolu(dosya: &str) -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join(dosya);
            if p.exists() {
                return p;
            }
        }
    }
    crate::nim::appdata_dir().join(dosya)
}

/// Apinex anahtarı (exe yanı öncelikli, yedek appdata).
fn apinex_key() -> Option<String> {
    let k = std::fs::read_to_string(anahtar_yolu("apinex-key.txt")).ok()?;
    let k = k.trim().to_string();
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

fn apinex_post(path: &str, body: &serde_json::Value, fallback_usd: f64) -> Option<serde_json::Value> {
    let key = apinex_key()?;
    let body_s = serde_json::to_string(body).ok()?;
    let url = format!("https://apinex.bond{}", path);
    let txt = with_retry(|| {
        agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", "application/json")
            .set("Accept-Charset", "utf-8")
            .send_string(&body_s)
    })
    .ok()?
    .into_string()
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    add_cost_usd(&v, fallback_usd);
    Some(v)
}

fn apinex_get(path: &str, fallback_usd: f64) -> Option<serde_json::Value> {
    let key = apinex_key()?;
    let url = format!("https://apinex.bond{}", path);
    let txt = with_retry(|| {
        agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", key))
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()?
    .into_string()
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    add_cost_usd(&v, fallback_usd);
    Some(v)
}

/// 17) Apinex web arama — gerçek web indeksi (anahtarlı).
fn src_apinex(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(v) = apinex_post(
        "/v1/tools/web/search",
        &serde_json::json!({"query": query, "count": 10}),
        0.0002,
    ) else {
        return;
    };
    let Some(arr) = v
        .get("results")
        .and_then(|r| r.get("web"))
        .and_then(|w| w.as_array())
    else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(10) {
        let (url, title, desc) = (
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if url.is_empty() || title.is_empty() {
            continue;
        }
        let mut snip = desc.to_string();
        if let Some(ss) = it.get("snippets").and_then(|x| x.as_array()) {
            for s in ss.iter().take(2) {
                if let Some(t) = s.as_str() {
                    if !t.is_empty() {
                        if !snip.is_empty() {
                            snip.push(' ');
                        }
                        snip.push_str(t);
                    }
                }
            }
        }
        out.push(Candidate {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snip.chars().take(400).collect(),
            source: "apinex".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Apinex({})", n));
    }
}

/// 18) Apinex derin araştırma — sadece Derin modda (kaynakçalı).
fn src_apinex_research(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(v) = apinex_post(
        "/v1/tools/web/research",
        &serde_json::json!({"query": query, "count": 5}),
        0.005,
    ) else {
        return;
    };
    let Some(o) = v.get("output") else { return };
    let brief = o.get("content").and_then(|x| x.as_str()).unwrap_or("");
    let Some(arr) = o.get("sources").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    let mut first_url = String::new();
    for it in arr.iter().take(8) {
        let (url, title) = (
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if url.is_empty() || title.is_empty() {
            continue;
        }
        if first_url.is_empty() {
            first_url = url.to_string();
        }
        let mut snip = String::new();
        if let Some(ss) = it.get("snippets").and_then(|x| x.as_array()) {
            for s in ss.iter().take(2) {
                if let Some(t) = s.as_str() {
                    if !t.is_empty() {
                        if !snip.is_empty() {
                            snip.push(' ');
                        }
                        snip.push_str(t);
                    }
                }
            }
        }
        out.push(Candidate {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snip.chars().take(400).collect(),
            source: "apinex-derin".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    // AI özeti de aday olur (ilk kaynağa bağlı).
    if !brief.is_empty() && !first_url.is_empty() {
        out.push(Candidate {
            title: format!("{} — araştırma özeti", query),
            url: first_url,
            snippet: brief.chars().take(500).collect(),
            source: "apinex-derin".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("ApinexDerin({})", n));
    }
}

/// 19) Apinex Twitter/X kişi arama — kısa sorgularda (isim avı).
fn src_apinex_twitter(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // Uzun cümlelerde kişi aramanın anlamı yok + para yazar.
    if query.split_whitespace().count() > 5 {
        return;
    }
    let Some(v) = apinex_get(
        &format!("/v1/tools/twitter/user/search?query={}", enc(query)),
        0.0008,
    ) else {
        return;
    };
    let Some(arr) = v.get("users").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(6) {
        let (handle, name, desc, fol) = (
            it.get("screen_name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("followers_count").and_then(|x| x.as_u64()).unwrap_or(0),
        );
        let handle = if handle.is_empty() {
            it.get("username").and_then(|x| x.as_str()).unwrap_or("")
        } else {
            handle
        };
        if handle.is_empty() {
            continue;
        }
        let title = if name.is_empty() {
            format!("@{} (X)", handle)
        } else {
            format!("{} · @{} (X)", name, handle)
        };
        out.push(Candidate {
            title,
            url: format!("https://x.com/{}", handle),
            snippet: format!(
                "{} · {} takipçi",
                desc.chars().take(180).collect::<String>(),
                fol
            ),
            source: "twitter".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Twitter({})", n));
    }
}

/// 20) DBpedia varlık arama (anahtarsız) — kavram/kişi tanımlarında güçlü.
fn src_dbpedia(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://lookup.dbpedia.org/api/search?query={}&format=json&maxResults=5",
        enc(query)
    )) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = v.get("docs").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(5) {
        let (label, comment, uri) = (
            it.get("label").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("comment").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("uri").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if label.is_empty() || uri.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: strip_tags(label),
            url: uri.to_string(),
            snippet: strip_tags(comment).chars().take(300).collect(),
            source: "dbpedia".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("DBpedia({})", n));
    }
}

/// 21) Wiby — bağımsız küçük web indeksi (anahtarsız JSON).
fn src_wiby(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!("https://wiby.me/json/?q={}", enc(query))) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let arr = if let Some(a) = v.as_array() {
        a
    } else if let Some(a) = v.get("results").and_then(|x| x.as_array()) {
        a
    } else {
        return;
    };
    let mut n = 0;
    // limit artışı: Wiby 8→15
    for it in arr.iter().take(15) {
        let (url, title, snip) = (
            it.get("URL").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("Title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("Snippet").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if url.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: if title.is_empty() { url.to_string() } else { title.to_string() },
            url: url.to_string(),
            snippet: snip.chars().take(300).collect(),
            source: "wiby".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Wiby({})", n));
    }
}

/// 21b) SearXNG havuzu — sırayla dene, ilk dolu sonuçta dur (anahtarsız).
const SEARXNG_INSTANCES: [&str; 6] = [
    "https://searxng.eshnetwork.space",
    "https://opnxng.com",
    "https://priv.au",
    "https://search.inetol.net",
    "https://search.rhscz.eu",
    "https://baresearch.org",
];

/// SearXNG JSON gövdesinden (title, url, content) çıkarır — katı şema yok, ekstra alanlar olabilir.
fn parse_searxng(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for it in arr.iter() {
        let (ti, ur, sn) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("content").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push((ti.to_string(), ur.to_string(), sn.to_string()));
    }
    out
}

/// Tek sayfayı havuzda gez — instance başına tek deneme (havuz zaten retry görevi görür).
fn src_searxng_page(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>, pageno: u8) {
    for inst in SEARXNG_INSTANCES {
        let url = format!(
            "{}/search?q={}&format=json&categories=general&language=all&pageno={}&safesearch=0",
            inst,
            enc(query),
            pageno
        );
        // with_retry yok — ölü instance'ta takılma, sonrakine geç.
        let Ok(resp) = agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json")
            .set("Accept-Charset", "utf-8")
            .call()
        else {
            continue;
        };
        let ct = resp.header("content-type").unwrap_or("").to_lowercase();
        if !ct.contains("json") {
            continue;
        }
        let Ok(body) = resp.into_string() else {
            continue;
        };
        let rows = parse_searxng(&body);
        if rows.is_empty() {
            continue;
        }
        let mut n = 0;
        // limit artışı: SearXNG-p1 10→20
        for (ti, ur, sn) in rows.into_iter().take(20) {
            out.push(Candidate {
                title: ti,
                url: ur,
                snippet: sn.chars().take(400).collect(),
                source: "searxng".into(),
                depth: 0,
                page: String::new(),
            });
            n += 1;
        }
        if n > 0 {
            sources.push(format!("SearXNG({})", n));
        }
        return;
    }
}

/// SearXNG 1. sayfa (her aramada).
fn src_searxng(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    src_searxng_page(query, out, sources, 1);
}

/// SearXNG 2. sayfa (yalnızca derin modda).
fn src_searxng_p2(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    src_searxng_page(query, out, sources, 2);
}

/// 22) Common Crawl URL avcısı — kullanıcı adı/handle keşfi (anahtarsız, sınırsız).
/// En son taramada URL'sinde sorgu geçen sayfaları listeler.
const CC_INDEXES: [&str; 2] = ["CC-MAIN-2026-34", "CC-MAIN-2026-30"];
fn src_cc(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // En az 4 harfli alfanümerik çekirdek gerekli (yoksa her şeyi döndürür).
    let core: String = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() >= 4)
        .collect::<Vec<_>>()
        .join("");
    if core.chars().count() < 4 || core.chars().count() > 40 {
        return;
    }
    // Birincil + yedek index: 404/boş ("No Captures") dönerse diğerini dene.
    // (collinfo.json'daki en yeni 2 index, 2026-08'de doğrulandı.)
    let mut body = String::new();
    for idx in CC_INDEXES {
        let got = agent()
            .get(&format!(
                "https://index.commoncrawl.org/{}-index?url=*{}*&output=json&limit=25",
                idx, core
            ))
            .set("User-Agent", ua_rot())
            .call()
            .ok()
            .and_then(|r| r.into_string().ok())
            .unwrap_or_default();
        if got.is_empty() || got.contains("No Captures") {
            continue;
        }
        body = got;
        break;
    }
    if body.is_empty() || body.contains("No Captures") {
        return;
    }
    let mut n = 0;
    let mut seen_here = std::collections::HashSet::new();
    for line in body.lines().take(25) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (url, mime, status) = (
            v.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            v.get("mime").and_then(|x| x.as_str()).unwrap_or(""),
            v.get("status").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if url.is_empty() || status != "200" || !mime.contains("html") {
            continue;
        }
        if !seen_here.insert(norm_url(url)) {
            continue;
        }
        // Başlık yok — URL'den okunabilir başlık üret (terim eşleşsin diye).
        let h = host_of(url);
        let path_tail = url
            .split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or("");
        let pretty = format!(
            "{} {}",
            h.replace('.', " ").replace('-', " "),
            path_tail.replace(['-', '_', '+'], " ")
        );
        let ts = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("");
        out.push(Candidate {
            title: pretty.chars().take(120).collect(),
            url: url.to_string(),
            snippet: format!("Common Crawl arşivi · {}", &ts[..ts.len().min(8)]),
            source: "cc".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
        // limit artışı: CC 10→15
        if n >= 15 {
            break;
        }
    }
    if n > 0 {
        sources.push(format!("CC({})", n));
    }
}

/// Anahtarlı sağlayıcı şablonu: exa / tavily / serper / langsearch (anahtar dosyası varsa).
fn provider_key(service: &str) -> Option<String> {
    let file = match service {
        "exa" => "exa-key.txt",
        "tavily" => "tavily-key.txt",
        "serper" => "serper-key.txt",
        "langsearch" => "langsearch-key.txt",
        _ => return None,
    };
    let p = anahtar_yolu(file);
    let k = std::fs::read_to_string(p).ok()?.trim().to_string();
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}

/// 23) Exa arama (anahtarlı).
fn src_exa(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(key) = provider_key("exa") else {
        return;
    };
    let body_s = serde_json::json!({"query": query, "numResults": 10}).to_string();
    let Some(txt) = with_retry(|| {
        agent()
            .post("https://api.exa.ai/search")
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", "application/json")
            .set("Accept-Charset", "utf-8")
            .send_string(&body_s)
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return;
    };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(10) {
        let (ti, ur, tx) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("text").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: ti.to_string(),
            url: ur.to_string(),
            snippet: tx.chars().take(350).collect(),
            source: "exa".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Exa({})", n));
    }
}

/// 24) Tavily arama (anahtarlı).
fn src_tavily(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(key) = provider_key("tavily") else {
        return;
    };
    let body_s = serde_json::json!({"query": query, "max_results": 10, "include_answer": false}).to_string();
    let Some(txt) = with_retry(|| {
        agent()
            .post("https://api.tavily.com/search")
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", "application/json")
            .set("Accept-Charset", "utf-8")
            .send_string(&body_s)
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return;
    };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(10) {
        let (ti, ur, tx) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("content").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: ti.to_string(),
            url: ur.to_string(),
            snippet: tx.chars().take(350).collect(),
            source: "tavily".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Tavily({})", n));
    }
}

/// 25) Serper (Google) arama (anahtarlı).
fn src_serper(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(key) = provider_key("serper") else {
        return;
    };
    let body_s = serde_json::json!({"q": query, "num": 10}).to_string();
    let Some(txt) = with_retry(|| {
        agent()
            .post("https://google.serper.dev/search")
            .set("X-API-KEY", &key)
            .set("Content-Type", "application/json")
            .set("Accept-Charset", "utf-8")
            .send_string(&body_s)
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return;
    };
    let Some(arr) = v.get("organic").and_then(|x| x.as_array()) else {
        return;
    };
    let mut n = 0;
    for it in arr.iter().take(10) {
        let (ti, ur, tx) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("link").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("snippet").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: ti.to_string(),
            url: ur.to_string(),
            snippet: tx.chars().take(350).collect(),
            source: "serper".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Serper({})", n));
    }
}

/// 26) LangSearch web arama (anahtarlı).
fn src_langsearch(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(key) = provider_key("langsearch") else {
        return;
    };
    // limit artışı: LangSearch 10→20
    let body_s = serde_json::json!({"query": query, "count": 20}).to_string();
    let Some(txt) = with_retry(|| {
        agent()
            .post("https://api.langsearch.com/v1/web-search")
            .set("Authorization", &format!("Bearer {}", key))
            .set("Content-Type", "application/json")
            .set("Accept-Charset", "utf-8")
            .send_string(&body_s)
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return;
    };
    let Some(arr) = v
        .get("data")
        .and_then(|d| d.get("webPages"))
        .and_then(|w| w.get("value"))
        .and_then(|x| x.as_array())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: LangSearch 10→20
    for it in arr.iter().take(20) {
        let (ti, ur, tx) = (
            it.get("name").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("snippet").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ti.is_empty() || ur.is_empty() {
            continue;
        }
        out.push(Candidate {
            title: ti.to_string(),
            url: ur.to_string(),
            snippet: tx.chars().take(350).collect(),
            source: "langsearch".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("LangSearch({})", n));
    }
}

/// DDG HTML yedek — engel yenirse sessizce boş döner.
/// DDG-lite gövdesinden (başlık, url, snippet) çıkarır — html ucu challenge yiyince yedek.
/// lite örüntüsü: <a ... href='...' class='result-link'>Başlık</a> (+ result-snippet hücresi).
fn parse_ddg_lite(body: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while out.len() < 20 {
        let a = match body[pos..].find("result-link") {
            Some(i) => pos + i,
            None => break,
        };
        // Enclosing <a ...>: geriye doğru tara.
        let tag_s = match body[..a].rfind("<a ") {
            Some(i) => i,
            None => {
                pos = a + 11;
                continue;
            }
        };
        // href= sonrası tırnak tipini (tek/çift) otomatik al.
        let h = match body[tag_s..a].find("href=") {
            Some(i) => tag_s + i + 5,
            None => {
                pos = a + 11;
                continue;
            }
        };
        let q = body[h..].chars().next().unwrap_or('"');
        if q != '"' && q != '\'' {
            pos = a + 11;
            continue;
        }
        let hs = h + 1;
        let he = match body[hs..].find(q) {
            Some(i) => hs + i,
            None => {
                pos = a + 11;
                continue;
            }
        };
        let mut href = body[hs..he].replace("&amp;", "&");
        if let Some(u) = href.find("uddg=").map(|i| href[i + 5..].to_string()) {
            let end = u.find('&').unwrap_or(u.len());
            href = dec(&u[..end]);
        }
        if href.starts_with("//") {
            href = format!("https:{}", href);
        }
        let title_s = match body[he..].find('>') {
            Some(i) => he + i + 1,
            None => {
                pos = he;
                continue;
            }
        };
        let title_e = match body[title_s..].find("</a>") {
            Some(i) => title_s + i,
            None => {
                pos = title_s;
                continue;
            }
        };
        let title = strip_tags(&body[title_s..title_e]);
        // Snippet: sonraki result-snippet hücresi (yoksa boş).
        let mut snippet = String::new();
        if let Some(s) = body[title_e..title_e + 4000.min(body.len() - title_e)].find("result-snippet") {
            let sp = title_e + s;
            if let (Some(gt), Some(e)) = (body[sp..].find('>'), body[sp..].find("</td>")) {
                if gt < e {
                    snippet = strip_tags(&body[sp + gt + 1..sp + e]).chars().take(400).collect();
                }
            }
        }
        pos = title_e + 4;
        if title.is_empty() || !(href.starts_with("http://") || href.starts_with("https://")) {
            continue;
        }
        if href.contains("duckduckgo.com") {
            continue;
        }
        out.push((title, href, snippet));
    }
    out
}

fn src_ddg_html(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://html.duckduckgo.com/html/?q={}",
        enc(query)
    )) else {
        return;
    };
    // Bot sayfası (anasayfa) ise bırak.
    if !body.contains("result__a") {
        return;
    }
    let mut n = 0;
    let mut pos = 0;
    while n < 20 {
        // limit artışı: DDG-Web 10→20
        let a = match body[pos..].find("result__a") {
            Some(i) => pos + i,
            None => break,
        };
        let href_s = match body[a..].find("href=\"") {
            Some(i) => a + i + 6,
            None => {
                pos = a + 9;
                continue;
            }
        };
        let href_e = match body[href_s..].find('"') {
            Some(i) => href_s + i,
            None => {
                pos = href_s;
                continue;
            }
        };
        let mut href = body[href_s..href_e].replace("&amp;", "&");
        if let Some(u) = href.find("uddg=").map(|i| href[i + 5..].to_string()) {
            let end = u.find('&').unwrap_or(u.len());
            href = dec(&u[..end]);
        }
        // Protokolsüz DDG iç linkini gerçek http'ye çevirmeyi dene
        if href.starts_with("//") {
            href = format!("https:{}", href);
        }
        let title_s = match body[href_e..].find('>') {
            Some(i) => href_e + i + 1,
            None => {
                pos = href_e;
                continue;
            }
        };
        let title_e = match body[title_s..].find("</a>") {
            Some(i) => title_s + i,
            None => {
                pos = title_s;
                continue;
            }
        };
        let title = strip_tags(&body[title_s..title_e]);
        let block_end = body[title_e..]
            .find("result__a")
            .map(|i| title_e + i)
            .unwrap_or(body.len());
        let snippet = if let Some(s) = body[title_e..block_end].find("result__snippet") {
            let sp = title_e + s;
            if let Some(gt) = body[sp..].find('>') {
                let ts = sp + gt + 1;
                // sınır-güvenli: bul, sonra karakterle kısalt
                if let Some(e) = body[ts..].find("</a>") {
                    let raw = strip_tags(&body[ts..ts + e]);
                    raw.chars().take(500).collect()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        pos = title_e + 4;
        if title.is_empty() || !(href.starts_with("http://") || href.starts_with("https://")) {
            continue;
        }
        if href.contains("duckduckgo.com") {
            continue;
        }
        out.push(Candidate {
            title,
            url: href,
            snippet,
            source: "ddg-web".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    // html ucu challenge yediyse (n==0) lite yedeğine düş — POST form ister.
    if n == 0 {
        let lite = with_retry(|| {
            agent()
                .post("https://lite.duckduckgo.com/lite/")
                .set("User-Agent", ua_rot())
                .set("Content-Type", "application/x-www-form-urlencoded")
                .send_string(&format!("q={}", enc(query)))
        })
        .ok()
        .and_then(|r| r.into_string().ok());
        if let Some(lb) = lite {
            for (title, href, snippet) in parse_ddg_lite(&lb) {
                out.push(Candidate {
                    title,
                    url: href,
                    snippet,
                    source: "ddg-web".into(),
                    depth: 0,
                    page: String::new(),
                });
                n += 1;
            }
        }
    }
    if n > 0 {
        sources.push(format!("DDG-Web({})", n));
    }
}

const BAD_EXT: [&str; 14] = [
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".css", ".js", ".pdf", ".zip",
    ".mp4", ".mp3", ".ico", ".woff",
];

/// base64url harf değeri (Bing /ck/a çözümü için).
fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'-' | b'+' => Some(62),
        b'_' | b'/' => Some(63),
        _ => None,
    }
}

/// Minik base64url çözücü (crate yok) — padding'i kendisi tamamlar.
/// Çözüm http(s) değilse ya da alfabe bozuksa None döner.
fn b64url_decode(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4096 {
        return None;
    }
    let mut clean = s.to_string();
    let rem = clean.len() % 4;
    if rem == 1 {
        return None;
    }
    for _ in 0..((4 - rem) % 4) {
        clean.push('=');
    }
    let b = clean.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() / 4 * 3);
    let mut i = 0;
    while i < b.len() {
        let mut vals = [0u8; 4];
        let mut pad = 0;
        for k in 0..4 {
            if b[i + k] == b'=' {
                pad += 1;
            } else {
                vals[k] = b64_val(b[i + k])?;
            }
        }
        if pad > 2 {
            return None;
        }
        let n = ((vals[0] as u32) << 18) | ((vals[1] as u32) << 12) | ((vals[2] as u32) << 6) | (vals[3] as u32);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad == 0 {
            out.push(n as u8);
        }
        i += 4;
    }
    let s = String::from_utf8(out).ok()?;
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        return None;
    }
    Some(s)
}

/// Bing gizli linkini çöz — direkt URL ise aynen, /ck/a ise a1 sonrası çözülür.
/// Çözülemezse None (çağıran atlar).
fn bing_href_coz(href: &str) -> Option<String> {
    let h = href.replace("&amp;", "&");
    if h.contains("/ck/a") {
        let p = h.find("u=a1")?;
        let art = &h[p + 4..];
        let son = art.find('&').unwrap_or(art.len());
        let kod = &art[..son];
        if kod.is_empty() {
            return None;
        }
        return b64url_decode(kod);
    }
    Some(h)
}

/// Bing gövdesinden (başlık, url, açıklama) çıkarır — b_algo yoksa boş döner.
fn parse_bing_html(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("b_algo") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    // limit artışı: Bing 10→20
    while out.len() < 20 {
        let a = match body[pos..].find("b_algo") {
            Some(i) => pos + i,
            None => break,
        };
        let h2 = match body[a..].find("<h2") {
            Some(i) => a + i,
            None => {
                pos = a + 6;
                continue;
            }
        };
        // bu sonucun sonu = sonraki b_algo (taşmayı önler)
        let blok_son = body[h2..].find("b_algo").map(|i| h2 + i).unwrap_or(body.len());
        let href_i = match body[h2..blok_son].find("href=\"") {
            Some(i) => h2 + i + 6,
            None => {
                pos = h2 + 3;
                continue;
            }
        };
        let href_e = match body[href_i..].find('"') {
            Some(i) => href_i + i,
            None => {
                pos = href_i;
                continue;
            }
        };
        let ham = body[href_i..href_e].replace("&amp;", "&");
        let Some(url) = bing_href_coz(&ham) else {
            pos = href_e + 1;
            continue;
        };
        let gt = match body[href_e..].find('>') {
            Some(i) => href_e + i + 1,
            None => {
                pos = href_e + 1;
                continue;
            }
        };
        let baslik_son = match body[gt..blok_son].find("</a>") {
            Some(i) => gt + i,
            None => {
                pos = gt;
                continue;
            }
        };
        let baslik = strip_tags(&body[gt..baslik_son]);
        if baslik.is_empty() {
            pos = baslik_son + 4;
            continue;
        }
        // açıklama: sonraki ilk <p> (yoksa boş)
        let mut ozet = String::new();
        if let Some(p_rel) = body[baslik_son..blok_son].find("<p") {
            let p_abs = baslik_son + p_rel;
            if let Some(gt2) = body[p_abs..blok_son].find('>') {
                let ic_bas = p_abs + gt2 + 1;
                if let Some(p_son) = body[ic_bas..blok_son].find("</p>") {
                    ozet = strip_tags(&body[ic_bas..ic_bas + p_son]);
                }
            }
        }
        pos = baslik_son + 4;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dusuk = url.to_lowercase();
        if dusuk.contains("bing.com") || dusuk.contains("microsoft.com") {
            continue;
        }
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        out.push((baslik, url, ozet));
    }
    out
}

/// 27) Bing web arama (anahtarsız HTML, sayfalama yok).
fn src_bing(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!(
        "https://www.bing.com/search?q={}&adlt=off&mkt=tr-TR&setlang=tr",
        enc(query)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    if !body.contains("b_algo") {
        return;
    }
    let mut n = 0;
    // limit artışı: Bing 10→20
    for (ti, ur, sn) in parse_bing_html(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(400).collect(),
            source: "bing".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Bing({})", n));
    }
}

/// Bing 2. sayfa (derin mod): first=11&FORM=PERE — canlı doğrulandı (b_algo var).
fn src_bing_p2(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!(
        "https://www.bing.com/search?q={}&adlt=off&mkt=tr-TR&setlang=tr&first=11&FORM=PERE",
        enc(query)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    if !body.contains("b_algo") {
        return;
    }
    let mut n = 0;
    for (ti, ur, sn) in parse_bing_html(&body).into_iter().take(15) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(400).collect(),
            source: "bing".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Bing({})", n));
    }
}

/// Google gövdesinden (başlık, url, açıklama) çıkarır — duvar/duvarsız kontrol dahil.
fn parse_google_html(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("/url?q=") {
        return Vec::new();
    }
    let dusuk = body.to_lowercase();
    if dusuk.contains("sorry") || dusuk.contains("captcha") || dusuk.contains("consent.google") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    // limit artışı: Google-ureq 10→20
    while out.len() < 20 {
        let a = match body[pos..].find("/url?q=") {
            Some(i) => pos + i + 7,
            None => break,
        };
        let son = body[a..]
            .find('&')
            .map(|i| a + i)
            .unwrap_or_else(|| body[a..].find('"').map(|i| a + i).unwrap_or(body.len()));
        if son <= a {
            pos = a + 1;
            continue;
        }
        let url = dec(&body[a..son]);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            pos = son + 1;
            continue;
        }
        let dl = url.to_lowercase();
        if dl.contains("google.com") {
            pos = son + 1;
            continue;
        }
        if BAD_EXT.iter().any(|e| dl.contains(e)) {
            pos = son + 1;
            continue;
        }
        // başlık: sonraki <h3> (800 karakter içindeyse)
        let h3_rel = match body[son..].find("<h3") {
            Some(i) => i,
            None => {
                pos = son + 1;
                continue;
            }
        };
        if h3_rel > 800 {
            pos = son + 1;
            continue;
        }
        let h3 = son + h3_rel;
        let ic_bas = match body[h3..].find('>') {
            Some(i) => h3 + i + 1,
            None => {
                pos = h3 + 3;
                continue;
            }
        };
        let h3_son = match body[ic_bas..].find("</h3>") {
            Some(i) => ic_bas + i,
            None => {
                pos = ic_bas;
                continue;
            }
        };
        let baslik = strip_tags(&body[ic_bas..h3_son]);
        if baslik.is_empty() {
            pos = h3_son + 5;
            continue;
        }
        // açıklama: sonraki ilk <div> metni (yoksa boş)
        let mut ozet = String::new();
        if let Some(d_rel) = body[h3_son..].find("<div") {
            let d_abs = h3_son + d_rel;
            if let Some(gt) = body[d_abs..].find('>') {
                let div_bas = d_abs + gt + 1;
                if let Some(d_son) = body[div_bas..].find("</div>") {
                    ozet = strip_tags(&body[div_bas..div_bas + d_son]).chars().take(400).collect();
                }
            }
        }
        pos = h3_son + 5;
        out.push((baslik, url, ozet));
    }
    out
}

/// 28) Google web arama (best-effort; duvarlı IP'de sessiz döner).
fn src_google_page(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>, start: u32) {
    let url = format!(
        // limit artışı: num 10→20
        "https://www.google.com/search?q={}&num=20&hl=tr&start={}",
        enc(query),
        start
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .set("Cookie", "CONSENT=YES+")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    if !body.contains("/url?q=") {
        return;
    }
    let dl = body.to_lowercase();
    if dl.contains("sorry") || dl.contains("captcha") || dl.contains("consent.google") {
        return;
    }
    let mut n = 0;
    // limit artışı: Google-ureq 10→20
    for (ti, ur, sn) in parse_google_html(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "google".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Google({})", n));
    }
}

/// Google 1. sayfa (her aramada).
fn src_google(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    src_google_page(query, out, sources, 0);
}

/// Google 2. sayfa (yalnızca derin modda).
fn src_google_p2(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // limit artışı: num=20 olunca 2. sayfa 20'den başlar
    src_google_page(query, out, sources, 20);
}

/// Marginalia JSON gövdesinden (başlık, url, açıklama) çıkarır.
fn parse_marginalia(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: Marginalia 10→15
    for it in arr.iter().take(15) {
        let (ur, ti, de) = (
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if ur.is_empty() || ti.is_empty() {
            continue;
        }
        out.push((ti.to_string(), ur.to_string(), de.to_string()));
    }
    out
}

/// 29) Marginalia bağımsız indeks (anahtarsız JSON, tek atış).
fn src_marginalia(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // limit artışı: Marginalia 10→15
    let url = format!("https://api.marginalia.nu/public/search/{}?count=15", enc(query));
    let Some(body) = with_retry(|| {
        agent_long()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Marginalia 10→15
    for (ti, ur, sn) in parse_marginalia(&body).into_iter().take(15) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(400).collect(),
            source: "marginalia".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Marginalia({})", n));
    }
}

/// GNews RSS gövdesinden (başlık, url, kaynak-adı) çıkarır — <item> yoksa boş döner.
fn parse_gnews(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("<item") {
        return Vec::new();
    }
    // CDATA sarmalını çöz (strip_tags öncesi, yoksa başlık yutulur).
    let coz = |ham: &str| ham.replace("<![CDATA[", "").replace("]]>", "");
    // Blok içi `<etiket ...>...</etiket>` metni (nitelikli açılışa dayanıklı).
    let alan = |blok: &str, etiket: &str| -> String {
        let acilis = format!("<{}", etiket);
        let kapanis = format!("</{}>", etiket);
        let a = match blok.find(&acilis) {
            Some(i) => i,
            None => return String::new(),
        };
        let gt = match blok[a..].find('>') {
            Some(i) => a + i + 1,
            None => return String::new(),
        };
        let son = match blok[gt..].find(&kapanis) {
            Some(i) => gt + i,
            None => return String::new(),
        };
        strip_tags(&coz(&blok[gt..son])).trim().to_string()
    };
    let mut out = Vec::new();
    let mut pos = 0;
    // limit artışı: GNews 10→20
    while out.len() < 20 {
        let a = match body[pos..].find("<item") {
            Some(i) => pos + i,
            None => break,
        };
        let gt = match body[a..].find('>') {
            Some(i) => a + i + 1,
            None => break,
        };
        let son = match body[gt..].find("</item>") {
            Some(i) => gt + i,
            None => break,
        };
        let blok = &body[gt..son];
        pos = son + 7;
        let baslik = alan(blok, "title");
        // Link şifreli redirect'tir — OLDUĞU GİBİ alınır, çözülmez.
        let baglanti = alan(blok, "link");
        if baslik.is_empty() || baglanti.is_empty() {
            continue;
        }
        if !(baglanti.starts_with("http://") || baglanti.starts_with("https://")) {
            continue;
        }
        let dusuk = baglanti.to_lowercase();
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        let kaynak = alan(blok, "source");
        out.push((baslik, baglanti, kaynak));
    }
    out
}

/// 30) GNews (Google News RSS) — haber araması (anahtarsız RSS).
fn src_gnews(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://news.google.com/rss/search?q={}&hl=tr&gl=TR&ceid=TR:tr",
        enc(query)
    )) else {
        return;
    };
    let mut n = 0;
    // limit artışı: GNews 10→20
    for (ti, ur, kaynak) in parse_gnews(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: kaynak,
            source: "gnews".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("GNews({})", n));
    }
}

/// ytInitialData JSON bloğunu dengeli-süslü taramayla çıkarır (dize-duyarlı).
fn yt_json_al(body: &str) -> Option<serde_json::Value> {
    let anahtar = body.find("ytInitialData")?;
    let bas = body[anahtar..].find('{').map(|i| anahtar + i)?;
    let b = body.as_bytes();
    let mut derinlik: i32 = 0;
    let mut dize = false;
    let mut kacis = false;
    let mut i = bas;
    while i < b.len() {
        let c = b[i];
        if kacis {
            kacis = false;
            i += 1;
            continue;
        }
        if c == b'\\' && dize {
            kacis = true;
            i += 1;
            continue;
        }
        if c == b'"' {
            dize = !dize;
            i += 1;
            continue;
        }
        if dize {
            i += 1;
            continue;
        }
        if c == b'{' {
            derinlik += 1;
        } else if c == b'}' {
            derinlik -= 1;
            if derinlik == 0 {
                let dilim = body.get(bas..=i)?;
                return serde_json::from_str(dilim).ok();
            }
        }
        i += 1;
    }
    None
}

/// videoRenderer düğümlerini özyineli tara (başlık, url, kanal).
fn yt_gezin(v: &serde_json::Value, out: &mut Vec<(String, String, String)>) {
    // limit artışı: YouTube 8→16
    if out.len() >= 16 {
        return;
    }
    match v {
        serde_json::Value::Array(a) => {
            for x in a {
                if out.len() >= 16 {
                    break;
                }
                yt_gezin(x, out);
            }
        }
        serde_json::Value::Object(m) => {
            if let Some(vr) = m.get("videoRenderer") {
                if let Some(o) = vr.as_object() {
                    let kimlik = o.get("videoId").and_then(|x| x.as_str()).unwrap_or("");
                    let baslik = o
                        .get("title")
                        .and_then(|t| {
                            t.get("runs")
                                .and_then(|r| r.as_array())
                                .and_then(|a| a.first())
                                .and_then(|r| r.get("text"))
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    t.get("simpleText").and_then(|x| x.as_str()).map(|s| s.to_string())
                                })
                                .or_else(|| {
                                    t.get("accessibility")
                                        .and_then(|a| a.get("accessibilityData"))
                                        .and_then(|d| d.get("label"))
                                        .and_then(|x| x.as_str())
                                        .map(|s| s.to_string())
                                })
                        })
                        .unwrap_or_default();
                    if !kimlik.is_empty() && !baslik.trim().is_empty() {
                        let kanal = o
                            .get("ownerText")
                            .and_then(|t| t.get("runs"))
                            .and_then(|r| r.as_array())
                            .and_then(|a| a.first())
                            .and_then(|r| r.get("text"))
                            .and_then(|x| x.as_str())
                            .or_else(|| {
                                o.get("longBylineText")
                                    .and_then(|t| t.get("runs"))
                                    .and_then(|r| r.as_array())
                                    .and_then(|a| a.first())
                                    .and_then(|r| r.get("text"))
                                    .and_then(|x| x.as_str())
                            })
                            .unwrap_or("YouTube videosu")
                            .to_string();
                        out.push((
                            baslik.trim().to_string(),
                            format!("https://www.youtube.com/watch?v={}", kimlik),
                            kanal,
                        ));
                    }
                }
            }
            // limit artışı: YouTube 8→16
            if out.len() >= 16 {
                return;
            }
            for (_, x) in m.iter() {
                if out.len() >= 16 {
                    break;
                }
                yt_gezin(x, out);
            }
        }
        _ => {}
    }
}

/// YouTube gövdesinden video adayları — ytInitialData yoksa boş döner.
fn parse_youtube(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("ytInitialData") {
        return Vec::new();
    }
    let Some(kok) = yt_json_al(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    yt_gezin(&kok, &mut out);
    out
}

/// 31) YouTube arama (anahtarsız HTML, ytInitialData taraması).
fn src_youtube(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!("https://www.youtube.com/results?search_query={}", enc(query));
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    if !body.contains("ytInitialData") {
        return;
    }
    let mut n = 0;
    // limit artışı: YouTube 8→16
    for (ti, ur, sn) in parse_youtube(&body).into_iter().take(16) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(200).collect(),
            source: "youtube".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("YouTube({})", n));
    }
}

/// SemScholar gövdesinden (başlık, url, özet) çıkarır — data yoksa boş döner.
fn parse_semscholar(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("data").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: SemScholar 10→15
    for it in arr.iter().take(15) {
        let baslik = it.get("title").and_then(|x| x.as_str()).unwrap_or("");
        if baslik.trim().is_empty() {
            continue;
        }
        let mut baglanti = it.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if baglanti.is_empty() {
            // URL yoksa makale kimliğinden kur (resmi şema: paperId).
            if let Some(kimlik) = it.get("paperId").and_then(|x| x.as_str()) {
                if !kimlik.is_empty() {
                    baglanti = format!("https://www.semanticscholar.org/paper/{}", kimlik);
                }
            }
        }
        if !(baglanti.starts_with("http://") || baglanti.starts_with("https://")) {
            continue;
        }
        let dusuk = baglanti.to_lowercase();
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        let ozet = it.get("abstract").and_then(|x| x.as_str()).unwrap_or("");
        let yil = it.get("year").and_then(|x| x.as_i64()).unwrap_or(0);
        let parca: String = if ozet.is_empty() {
            if yil > 0 {
                format!("Semantic Scholar makalesi · {}", yil)
            } else {
                "Semantic Scholar makalesi".to_string()
            }
        } else {
            ozet.chars().take(300).collect()
        };
        out.push((baslik.to_string(), baglanti, parca));
    }
    out
}

/// 32) Semantic Scholar — akademik makale araması (anahtarsız).
fn src_semscholar(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: SemScholar 10→15
        "https://api.semanticscholar.org/graph/v1/paper/search?query={}&limit=15&fields=title,url,abstract,year,authors",
        enc(query)
    )) else {
        return;
    };
    let mut n = 0;
    for (ti, ur, sn) in parse_semscholar(&body).into_iter().take(15) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "semscholar".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("SemScholar({})", n));
    }
}

/// Crossref gövdesinden (başlık, url, yazar+yıl) çıkarır — items yoksa boş döner.
fn parse_crossref(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v
        .get("message")
        .and_then(|m| m.get("items"))
        .and_then(|x| x.as_array())
    else {
        return Vec::new();
    };
    // Yayın yılını basılı/çevrimiçi/karma tarih alanından çeker.
    let yil_al = |it: &serde_json::Value| -> String {
        for anahtar in ["published", "published-print", "published-online", "created"] {
            if let Some(yil) = it
                .get(anahtar)
                .and_then(|p| p.get("date-parts"))
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .and_then(|i| i.as_array())
                .and_then(|a| a.first())
                .and_then(|y| y.as_i64())
            {
                if yil > 0 {
                    return yil.to_string();
                }
            }
        }
        String::new()
    };
    let mut out = Vec::new();
    // limit artışı: Crossref 8→15
    for it in arr.iter().take(15) {
        let baslik = it
            .get("title")
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if baslik.trim().is_empty() {
            continue;
        }
        let mut baglanti = it.get("URL").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if baglanti.is_empty() {
            // URL yoksa DOI'dan kur (resmi şema).
            if let Some(doi) = it.get("DOI").and_then(|x| x.as_str()) {
                if !doi.is_empty() {
                    baglanti = format!("https://doi.org/{}", doi);
                }
            }
        }
        if !(baglanti.starts_with("http://") || baglanti.starts_with("https://")) {
            continue;
        }
        let yazarlar: Vec<String> = it
            .get("author")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .take(3)
                    .filter_map(|y| {
                        let ad = y.get("given").and_then(|x| x.as_str()).unwrap_or("");
                        let soyad = y.get("family").and_then(|x| x.as_str()).unwrap_or("");
                        let tam = format!("{} {}", ad, soyad).trim().to_string();
                        if tam.is_empty() { None } else { Some(tam) }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let yil = yil_al(it);
        let parca = match (yazarlar.is_empty(), yil.is_empty()) {
            (true, true) => "Crossref kaydı".to_string(),
            (true, false) => yil,
            (false, true) => yazarlar.join(", "),
            (false, false) => format!("{} · {}", yazarlar.join(", "), yil),
        };
        out.push((baslik.to_string(), baglanti, parca));
    }
    out
}

/// 33) Crossref — akademik meta-veri araması (anahtarsız, kibar mailto).
fn src_crossref(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: Crossref 8→15
        "https://api.crossref.org/works?query={}&rows=15&select=DOI,title,URL,published,author&mailto=noralweb@example.com",
        enc(query)
    )) else {
        return;
    };
    let mut n = 0;
    for (ti, ur, sn) in parse_crossref(&body).into_iter().take(15) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "crossref".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Crossref({})", n));
    }
}

/// ORCID gövdesinden (ad-soyad, profil-url, profil-notu) çıkarır.
fn parse_orcid(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("expanded-result").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: ORCID 5→8
    for it in arr.iter().take(8) {
        let kimlik = it.get("orcid-id").and_then(|x| x.as_str()).unwrap_or("");
        if kimlik.is_empty() {
            continue;
        }
        let ad = it.get("given-names").and_then(|x| x.as_str()).unwrap_or("");
        let soyad = it.get("family-names").and_then(|x| x.as_str()).unwrap_or("");
        let tam = format!("{} {}", ad, soyad).trim().to_string();
        if tam.is_empty() {
            continue;
        }
        out.push((
            tam,
            format!("https://orcid.org/{}", kimlik),
            "ORCID araştırmacı profili".to_string(),
        ));
    }
    out
}

/// 34) ORCID kişi araması (Accept: application/json ŞART).
fn src_orcid(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // limit artışı: ORCID 5→8
    let url = format!("https://pub.orcid.org/v3.0/expanded-search/?q={}&rows=8", enc(query));
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json")
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: ORCID 5→8
    for (ti, ur, sn) in parse_orcid(&body).into_iter().take(8) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "orcid".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("ORCID({})", n));
    }
}

/// GDELT gövdesinden (başlık, url, domain+tarih) çıkarır — articles yoksa boş döner.
fn parse_gdelt(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("articles").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: GDELT 10→20
    for it in arr.iter().take(20) {
        let (baslik, baglanti) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if baslik.trim().is_empty() || baglanti.is_empty() {
            continue;
        }
        if !(baglanti.starts_with("http://") || baglanti.starts_with("https://")) {
            continue;
        }
        let dusuk = baglanti.to_lowercase();
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        let (alan_adi, tarih) = (
            it.get("domain").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("seendate").and_then(|x| x.as_str()).unwrap_or(""),
        );
        let parca = match (alan_adi.is_empty(), tarih.is_empty()) {
            (true, true) => "GDELT haberi".to_string(),
            (true, false) => tarih.to_string(),
            (false, true) => alan_adi.to_string(),
            (false, false) => format!("{} · {}", alan_adi, tarih),
        };
        out.push((baslik.to_string(), baglanti.to_string(), parca));
    }
    out
}

/// 35) GDELT haber araması (anahtarsız DOC API).
fn src_gdelt(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        "https://api.gdeltproject.org/api/v2/doc/doc?query={}&mode=artlist&format=json&maxrecords=50",
        enc(query)
    )) else {
        return;
    };
    let mut n = 0;
    // limit artışı: GDELT 10→20
    for (ti, ur, sn) in parse_gdelt(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "gdelt".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("GDELT({})", n));
    }
}

/// Open Library gövdesinden (kitap-başlığı, eser-url, yazar+yıl) çıkarır.
fn parse_openlib(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("docs").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: OpenLib 8→15
    for it in arr.iter().take(15) {
        let (anahtar, baslik) = (
            it.get("key").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if anahtar.is_empty() || baslik.trim().is_empty() {
            continue;
        }
        let yazarlar: Vec<String> = it
            .get("author_name")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .take(3)
                    .filter_map(|y| y.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let yil = it.get("first_publish_year").and_then(|x| x.as_i64()).unwrap_or(0);
        let parca = match (yazarlar.is_empty(), yil > 0) {
            (true, false) => "Open Library kitabı".to_string(),
            (true, true) => format!("ilk baskı {}", yil),
            (false, false) => yazarlar.join(", "),
            (false, true) => format!("{} · ilk baskı {}", yazarlar.join(", "), yil),
        };
        out.push((
            baslik.to_string(),
            format!("https://openlibrary.org{}", anahtar),
            parca,
        ));
    }
    out
}

/// 36) Open Library kitap araması (anahtarsız).
fn src_openlib(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    // Yavaş uç: 6sn yerine uzun ajan (12sn) — get_text'e dokunulmaz, o diğerlerinindir.
    let Some(body) = with_retry(|| {
        agent_long()
            .get(&format!(
                // limit artışı: OpenLib 8→15
                "https://openlibrary.org/search.json?q={}&fields=key,title,author_name,first_publish_year&limit=15",
                enc(query)
            ))
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json, text/html")
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    for (ti, ur, sn) in parse_openlib(&body).into_iter().take(15) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "openlib".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("OpenLib({})", n));
    }
}

/// 37) Bing-Sosyal — aynı parse_bing_html yeniden kullanılır (sosyal sorgu).
fn src_bing_sosyal(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let sosyal = format!(
        "{} site:linkedin.com OR site:instagram.com OR site:facebook.com OR site:youtube.com OR site:x.com",
        query
    );
    let url = format!(
        "https://www.bing.com/search?q={}&adlt=off&mkt=tr-TR&setlang=tr",
        enc(&sosyal)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    if !body.contains("b_algo") {
        return;
    }
    let mut n = 0;
    // BAD_EXT/host filtresi parse_bing_html içindedir — kopyala-yapıştır yok.
    // limit artışı: Bing-Sosyal 6→10
    for (ti, ur, sn) in parse_bing_html(&body).into_iter().take(10) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(400).collect(),
            source: "bing-sosyal".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Bing-Sosyal({})", n));
    }
}

/// Yahoo gövdesinden (başlık, url, açıklama) çıkarır — algo-sr yoksa boş döner.
/// RU= şifreli link dec() ile çözülür, /RK veya /RS öncesi kesilir.
fn parse_yahoo(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("algo-sr") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    // limit artışı: Yahoo 10→20
    while out.len() < 20 {
        let a = match body[pos..].find("algo-sr") {
            Some(i) => pos + i,
            None => break,
        };
        let blok_son = body[a + 7..]
            .find("algo-sr")
            .map(|i| a + 7 + i)
            .unwrap_or(body.len());
        let blok = &body[a..blok_son];
        pos = a + 7;
        // compTitle içindeki a href
        let ct = match blok.find("compTitle") {
            Some(i) => i,
            None => continue,
        };
        let href_i = match blok[ct..].find("href=\"") {
            Some(i) => ct + i + 6,
            None => continue,
        };
        let href_e = match blok[href_i..].find('"') {
            Some(i) => href_i + i,
            None => continue,
        };
        let ham = blok[href_i..href_e].replace("&amp;", "&");
        let url: String;
        if let Some(ru) = ham.find("RU=") {
            let art = &ham[ru + 3..];
            let mut son = art.len();
            for isaret in ["/RK=", "/RS="] {
                if let Some(k) = art.find(isaret) {
                    if k < son {
                        son = k;
                    }
                }
            }
            url = dec(&art[..son]);
        } else if ham.starts_with("http://") || ham.starts_with("https://") {
            url = ham;
        } else {
            continue;
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dusuk = url.to_lowercase();
        if dusuk.contains("yahoo.com") {
            continue;
        }
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        // başlık: önce aria-label, yoksa a/h3 metni
        let mut baslik = String::new();
        if let Some(al) = blok[ct..].find("aria-label=\"") {
            let s = ct + al + 12;
            if let Some(e) = blok[s..].find('"') {
                baslik = strip_tags(&blok[s..s + e]);
            }
        }
        if baslik.is_empty() {
            let gt = match blok[href_e..].find('>') {
                Some(i) => href_e + i + 1,
                None => continue,
            };
            let kapan = match blok[gt..].find("</a>") {
                Some(i) => gt + i,
                None => continue,
            };
            if kapan > blok.len() {
                continue;
            }
            baslik = strip_tags(&blok[gt..kapan]);
        }
        if baslik.is_empty() {
            continue;
        }
        // açıklama: compText div metni
        let mut ozet = String::new();
        if let Some(c) = blok.find("compText") {
            if let Some(gt) = blok[c..].find('>') {
                let bas = c + gt + 1;
                if let Some(son) = blok[bas..].find("</div>") {
                    ozet = strip_tags(&blok[bas..bas + son]).chars().take(400).collect();
                }
            }
        }
        out.push((baslik, url, ozet));
    }
    out
}

/// 38) Yahoo arama (anahtarsız HTML, tarayıcı UA).
fn src_yahoo(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = with_retry(|| {
        agent()
            .get(&format!(
                "https://search.yahoo.com/search?p={}&vc=tr&guccounter=1",
                enc(query)
            ))
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Yahoo 10→20
    for (ti, ur, sn) in parse_yahoo(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn.chars().take(400).collect(),
            source: "yahoo".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Yahoo({})", n));
    }
}

/// Ecosia gövdesinden (başlık, url, açıklama) çıkarır — class yok, kırılgan yapı.
/// href="http ile başlayanlar alınır, ecosia iç + /images + reklam elenir.
fn parse_ecosia(body: &str) -> Vec<(String, String, String)> {
    if !body.contains("href=\"http") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut gorulen = std::collections::HashSet::new();
    let mut pos = 0;
    // limit artışı: Ecosia 8→20
    while out.len() < 20 {
        let a = match body[pos..].find("href=\"http") {
            Some(i) => pos + i + 6,
            None => break,
        };
        let son = match body[a..].find('"') {
            Some(i) => a + i,
            None => {
                pos = a + 1;
                continue;
            }
        };
        let url = body[a..son].replace("&amp;", "&");
        pos = son + 1;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dusuk = url.to_lowercase();
        // iç arama + görsel + reklam elenir
        if dusuk.contains("ecosia.org/search") {
            continue;
        }
        if dusuk.contains("ecosia.org") {
            continue;
        }
        if dusuk.contains("/images") {
            continue;
        }
        if dusuk.contains("googleadservices") || dusuk.contains("sponsored") {
            continue;
        }
        if BAD_EXT.iter().any(|e| dusuk.contains(e)) {
            continue;
        }
        // en yakın h2/p metni (sonraki href'e taşmadan)
        let ileri_son = body.floor_char_boundary((son + 2000).min(body.len()));
        let ileri = &body[son..ileri_son];
        let pencere = ileri
            .find("href=\"http")
            .map(|i| &ileri[..i])
            .unwrap_or(ileri);
        let mut baslik = String::new();
        if let Some(h2) = pencere.find("<h2") {
            if let Some(gt) = pencere[h2..].find('>') {
                let bas = h2 + gt + 1;
                if let Some(kapan) = pencere[bas..].find("</h2>") {
                    baslik = strip_tags(&pencere[bas..bas + kapan]);
                }
            }
        }
        if baslik.is_empty() {
            // çapa metni yedeği
            if let Some(gt) = pencere.find('>') {
                if let Some(kapan) = pencere[gt + 1..].find("</a>") {
                    let ham = strip_tags(&pencere[gt + 1..gt + 1 + kapan]);
                    if ham.chars().count() >= 3 {
                        baslik = ham;
                    }
                }
            }
        }
        if baslik.is_empty() {
            continue;
        }
        let mut ozet = String::new();
        if let Some(p) = pencere.find("<p") {
            if let Some(gt) = pencere[p..].find('>') {
                let bas = p + gt + 1;
                if let Some(kapan) = pencere[bas..].find("</p>") {
                    ozet = strip_tags(&pencere[bas..bas + kapan]).chars().take(400).collect();
                }
            }
        }
        if !gorulen.insert(norm_url(&url)) {
            continue;
        }
        out.push((baslik, url, ozet));
    }
    out
}

/// 39) Ecosia arama (anahtarsız HTML, tutmazsa sessiz).
fn src_ecosia(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = with_retry(|| {
        agent()
            .get(&format!("https://www.ecosia.org/search?q={}", enc(query)))
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Ecosia 8→20
    for (ti, ur, sn) in parse_ecosia(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "ecosia".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Ecosia({})", n));
    }
}

/// SvelteKit gömülü JSON içinde brave sonuç dizisini bulur (özyineli).
fn brave_sonuclar(v: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(web) = m.get("web") {
                if let Some(arr) = web.get("results").and_then(|x| x.as_array()) {
                    return Some(arr);
                }
                if let Some(r) = brave_sonuclar(web) {
                    return Some(r);
                }
            }
            if let Some(resp) = m.get("response") {
                if let Some(r) = brave_sonuclar(resp) {
                    return Some(r);
                }
            }
            if let Some(kok) = m.get("body") {
                if let Some(r) = brave_sonuclar(kok) {
                    return Some(r);
                }
            }
            if let Some(arr) = m.get("results").and_then(|x| x.as_array()) {
                if arr.first().and_then(|e| e.get("url")).is_some() {
                    return Some(arr);
                }
            }
            for (_, x) in m.iter() {
                if let Some(r) = brave_sonuclar(x) {
                    return Some(r);
                }
            }
            None
        }
        serde_json::Value::Array(a) => {
            for x in a {
                if let Some(r) = brave_sonuclar(x) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

/// Brave gövdesinden (başlık, url, açıklama) çıkarır — önce gömülü JSON blob,
/// yoksa data-type="web" yedeği. Düz regex yetmez, gerçek JSON parse şart.
fn parse_braveweb(body: &str) -> Vec<(String, String, String)> {
    // 1) gömülü SvelteKit JSON blobu
    let mut pos = 0;
    while pos < body.len() {
        let rel = match body[pos..].find("\"response\"") {
            Some(i) => pos + i,
            None => break,
        };
        // geriye en yakın '{' (en fazla 600 bayt geri)
        let mut bas = rel;
        let geri = rel.saturating_sub(600);
        while bas > geri && body.get(bas..bas + 1) != Some("{") {
            bas -= 1;
            while bas > geri && !body.is_char_boundary(bas) {
                bas -= 1;
            }
        }
        if body.get(bas..bas + 1) != Some("{") {
            pos = rel + 10;
            continue;
        }
        // dengeli süslü tara (dize-duyarlı)
        let b = body.as_bytes();
        let mut derinlik: i32 = 0;
        let mut dize = false;
        let mut kacis = false;
        let mut i = bas;
        let mut son: Option<usize> = None;
        while i < b.len() {
            let c = b[i];
            if kacis {
                kacis = false;
                i += 1;
                continue;
            }
            if c == b'\\' && dize {
                kacis = true;
                i += 1;
                continue;
            }
            if c == b'"' {
                dize = !dize;
                i += 1;
                continue;
            }
            if dize {
                i += 1;
                continue;
            }
            if c == b'{' {
                derinlik += 1;
            } else if c == b'}' {
                derinlik -= 1;
                if derinlik == 0 {
                    son = Some(i);
                    break;
                }
            }
            i += 1;
            if i - bas > 200_000 {
                break;
            }
        }
        let Some(son) = son else {
            pos = rel + 10;
            continue;
        };
        let dilim = body.get(bas..=son).unwrap_or("");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(dilim) {
            if let Some(arr) = brave_sonuclar(&v) {
                let mut out = Vec::new();
                // limit artışı: BraveWeb 10→20
                for it in arr.iter().take(20) {
                    let (ti, ur, de) = (
                        it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                        it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                        it.get("description").and_then(|x| x.as_str()).unwrap_or(""),
                    );
                    if ti.is_empty() || ur.is_empty() {
                        continue;
                    }
                    if !(ur.starts_with("http://") || ur.starts_with("https://")) {
                        continue;
                    }
                    let dl = ur.to_lowercase();
                    if dl.contains("brave.com") {
                        continue;
                    }
                    if BAD_EXT.iter().any(|e| dl.contains(e)) {
                        continue;
                    }
                    out.push((ti.to_string(), ur.to_string(), de.chars().take(400).collect()));
                    // limit artışı: BraveWeb 10→20
                    if out.len() >= 20 {
                        break;
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
        pos = rel + 10;
    }
    // 2) yedek: data-type="web" bloklarındaki a href
    if !body.contains("data-type") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut p = 0;
    // limit artışı: BraveWeb 10→20
    while out.len() < 20 {
        let a = match body[p..].find("data-type") {
            Some(i) => p + i,
            None => break,
        };
        let pencere_son = body.floor_char_boundary((a + 4000).min(body.len()));
        let pencere = &body[a..pencere_son];
        p = a + 9;
        // web bloğu değilse atla
        if !pencere.contains("web") {
            continue;
        }
        let href_i = match pencere.find("href=\"") {
            Some(i) => i + 6,
            None => continue,
        };
        let href_e = match pencere[href_i..].find('"') {
            Some(i) => href_i + i,
            None => continue,
        };
        let url = pencere[href_i..href_e].replace("&amp;", "&");
        let gt = match pencere[href_e..].find('>') {
            Some(i) => href_e + i + 1,
            None => continue,
        };
        let kapan = match pencere[gt..].find("</a>") {
            Some(i) => gt + i,
            None => continue,
        };
        let baslik = strip_tags(&pencere[gt..kapan]);
        if baslik.is_empty() {
            continue;
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dl = url.to_lowercase();
        if dl.contains("brave.com") {
            continue;
        }
        if BAD_EXT.iter().any(|e| dl.contains(e)) {
            continue;
        }
        let mut ozet = String::new();
        if let Some(pr) = pencere[kapan..].find("<p") {
            let pa = kapan + pr;
            if let Some(g2) = pencere[pa..].find('>') {
                let ic = pa + g2 + 1;
                if let Some(ps) = pencere[ic..].find("</p>") {
                    ozet = strip_tags(&pencere[ic..ic + ps]).chars().take(400).collect();
                }
            }
        }
        out.push((baslik, url, ozet));
    }
    out
}

/// 40) Brave web taraması (anahtarsız HTML + gömülü JSON, API anahtarı istemez).
fn src_braveweb(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = with_retry(|| {
        agent()
            .get(&format!(
                "https://search.brave.com/search?q={}&source=web",
                enc(query)
            ))
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: BraveWeb 10→20
    for (ti, ur, sn) in parse_braveweb(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "braveweb".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("BraveWeb({})", n));
    }
}

/// Yandex gövdesinden (başlık, url, açıklama) çıkarır — serp-item yoksa boş.
/// data-type=ads elenir, /r?u= yönlendirmesi dec() ile çözülür, captcha sessiz.
fn parse_yandex(body: &str) -> Vec<(String, String, String)> {
    let dus = body.to_lowercase();
    if dus.contains("captcha") || dus.contains("showcaptcha") {
        return Vec::new();
    }
    if !body.contains("serp-item") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    // limit artışı: Yandex 8→20
    while out.len() < 20 {
        let a = match body[pos..].find("serp-item") {
            Some(i) => pos + i,
            None => break,
        };
        let blok_son = body[a + 9..]
            .find("serp-item")
            .map(|i| a + 9 + i)
            .unwrap_or(body.len());
        let blok = &body[a..blok_son];
        pos = a + 9;
        // reklam bloğu ele
        if blok.contains("data-type=\"ads\"") || blok.contains("data-type='ads'") {
            continue;
        }
        let bag = match blok.find("OrganicTitle-Link") {
            Some(i) => i,
            None => continue,
        };
        let href_i = match blok[bag..].find("href=\"") {
            Some(i) => bag + i + 6,
            None => continue,
        };
        let href_e = match blok[href_i..].find('"') {
            Some(i) => href_i + i,
            None => continue,
        };
        let ham = blok[href_i..href_e].replace("&amp;", "&");
        let url: String;
        if let Some(u) = ham.find("u=") {
            let art = &ham[u + 2..];
            let son = art.find('&').unwrap_or(art.len());
            url = dec(&art[..son]);
        } else if ham.starts_with("http://") || ham.starts_with("https://") {
            url = ham;
        } else {
            continue;
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dl = url.to_lowercase();
        if dl.contains("yandex.") {
            continue;
        }
        if BAD_EXT.iter().any(|e| dl.contains(e)) {
            continue;
        }
        let gt = match blok[href_e..].find('>') {
            Some(i) => href_e + i + 1,
            None => continue,
        };
        let kapan = match blok[gt..].find("</a>") {
            Some(i) => gt + i,
            None => continue,
        };
        if kapan > blok.len() {
            continue;
        }
        let baslik = strip_tags(&blok[gt..kapan]);
        if baslik.is_empty() {
            continue;
        }
        let mut ozet = String::new();
        if let Some(d) = blok[kapan..].find("<div") {
            let da = kapan + d;
            if let Some(g2) = blok[da..].find('>') {
                let ic = da + g2 + 1;
                if let Some(ds) = blok[ic..].find("</div>") {
                    ozet = strip_tags(&blok[ic..ic + ds]).chars().take(400).collect();
                }
            }
        }
        out.push((baslik, url, ozet));
    }
    out
}

/// 41) Yandex arama (anahtarsız HTML, captcha/boşsa sessiz).
fn src_yandex(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = with_retry(|| {
        agent()
            .get(&format!("https://yandex.com.tr/search/?text={}", enc(query)))
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Language", "tr-TR,tr;q=0.9")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Yandex 8→20
    for (ti, ur, sn) in parse_yandex(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "yandex".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Yandex({})", n));
    }
}

/// Qwant gövdesinden (başlık, url, açıklama) çıkarır — mainline içinde type==web.
/// error_*/captcha/403 durumunda sessiz boş döner (alan adı desc!).
fn parse_qwant(body: &str) -> Vec<(String, String, String)> {
    if body.to_lowercase().contains("captcha") {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    if v.get("status").and_then(|x| x.as_str()) == Some("error") {
        return Vec::new();
    }
    if let Some(kod) = v.get("error_code").and_then(|x| x.as_i64()) {
        if kod != 0 {
            return Vec::new();
        }
    }
    let Some(hat) = v
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.get("items"))
        .and_then(|i| i.get("mainline"))
        .and_then(|x| x.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for grup in hat {
        if grup.get("type").and_then(|x| x.as_str()) != Some("web") {
            continue;
        }
        let Some(arr) = grup.get("items").and_then(|x| x.as_array()) else {
            continue;
        };
        for it in arr.iter() {
            // limit artışı: Qwant 8→20
            if out.len() >= 20 {
                break;
            }
            let (ti, ur, de) = (
                it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                it.get("desc").and_then(|x| x.as_str()).unwrap_or(""),
            );
            if ti.is_empty() || ur.is_empty() {
                continue;
            }
            if !(ur.starts_with("http://") || ur.starts_with("https://")) {
                continue;
            }
            let dl = ur.to_lowercase();
            if BAD_EXT.iter().any(|e| dl.contains(e)) {
                continue;
            }
            out.push((ti.to_string(), ur.to_string(), de.chars().take(400).collect()));
        }
        // limit artışı: Qwant 8→20
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// 43) Qwant arama (anahtarsız JSON, qwant header şart).
fn src_qwant(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!(
        // limit artışı: Qwant 8→20
        "https://api.qwant.com/v3/search/web?q={}&count=20&locale=tr_TR&offset=0&device=desktop&safesearch=1",
        enc(query)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", ua_rot())
            .set("Accept", "application/json")
            .set("Referer", "https://www.qwant.com/")
            .set("Origin", "https://www.qwant.com/")
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Qwant 8→20
    for (ti, ur, sn) in parse_qwant(&body).into_iter().take(20) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "qwant".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Qwant({})", n));
    }
}

/// Reddit gövdesinden (başlık, url, alt-bilgi) çıkarır — children yoksa boş.
/// self-post ise permalinkten kurulur, yoksa url aynen alınır.
fn parse_reddit(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v
        .get("data")
        .and_then(|d| d.get("children"))
        .and_then(|x| x.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: Reddit 8→12
    for cocuk in arr.iter().take(12) {
        let it = cocuk.get("data").unwrap_or(cocuk);
        let (baslik, oz, bag, alt) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("selftext").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("url").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("subreddit").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if baslik.trim().is_empty() {
            continue;
        }
        let kalici = it.get("permalink").and_then(|x| x.as_str()).unwrap_or("");
        let kendine = it.get("is_self").and_then(|x| x.as_bool()).unwrap_or(false);
        let mut url = bag.to_string();
        if kendine || url.is_empty() {
            if kalici.is_empty() {
                continue;
            }
            url = format!("https://www.reddit.com{}", kalici);
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let dl = url.to_lowercase();
        if BAD_EXT.iter().any(|e| dl.contains(e)) {
            continue;
        }
        let parca = if oz.trim().is_empty() {
            if alt.is_empty() {
                "Reddit gönderisi".to_string()
            } else {
                format!("r/{} · Reddit", alt)
            }
        } else {
            let kisalt: String = strip_tags(oz).chars().take(300).collect();
            if alt.is_empty() {
                kisalt
            } else {
                format!("{} · r/{}", kisalt, alt)
            }
        };
        out.push((strip_tags(baslik), url, parca));
        // limit artışı: Reddit 8→12
        if out.len() >= 12 {
            break;
        }
    }
    out
}

/// 44) Reddit arama (anahtarsız JSON, özel UA şart — rotasyon banlanır).
fn src_reddit(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!(
        // limit artışı: Reddit 8→12
        "https://www.reddit.com/search.json?q={}&limit=12&sort=relevance&raw_json=1",
        enc(query)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", "NoralWeb/1.0 (by /u/noral)")
            .set("Accept", "application/json")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Reddit 8→12
    for (ti, ur, sn) in parse_reddit(&body).into_iter().take(12) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "reddit".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Reddit({})", n));
    }
}

/// WikiAra gövdesinden (başlık, curid-url, açıklama) çıkarır — snipetteki vurgu temizlenir.
fn parse_wikiara(body: &str, dil: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v
        .get("query")
        .and_then(|q| q.get("search"))
        .and_then(|x| x.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: WikiAra 6→10
    for it in arr.iter().take(10) {
        let baslik = it.get("title").and_then(|x| x.as_str()).unwrap_or("");
        if baslik.trim().is_empty() {
            continue;
        }
        let kimlik = it.get("pageid").and_then(|x| x.as_u64()).unwrap_or(0);
        if kimlik == 0 {
            continue;
        }
        let ham = it.get("snippet").and_then(|x| x.as_str()).unwrap_or("");
        let ozet: String = strip_tags(ham).chars().take(400).collect();
        out.push((
            baslik.to_string(),
            format!("https://{}.wikipedia.org/?curid={}", dil, kimlik),
            ozet,
        ));
        // limit artışı: WikiAra 6→10
        if out.len() >= 10 {
            break;
        }
    }
    out
}

/// 45) WikiAra tam-metin (TR + EN) — ?curid= bağlantılı, iki etiketli.
fn src_wikiara(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    for dil in ["tr", "en"] {
        let Some(body) = get_text(&format!(
            // limit artışı: WikiAra 6→10
            "https://{}.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=10&format=json&utf8=",
            dil,
            enc(query)
        )) else {
            continue;
        };
        let mut n = 0;
        for (ti, ur, sn) in parse_wikiara(&body, dil).into_iter().take(10) {
            out.push(Candidate {
                title: ti,
                url: ur,
                snippet: sn,
                source: "wikibul".into(),
                depth: 0,
                page: String::new(),
            });
            n += 1;
        }
        if n > 0 {
            sources.push(format!("WikiAra-{}({})", dil, n));
        }
    }
}

/// Deezer gövdesinden (şarkı-başlığı, link, tür+sanatçı) çıkarır — data yoksa boş.
fn parse_deezer(body: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.get("data").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: Deezer 5→8
    for it in arr.iter().take(8) {
        let (baslik, bag, tur) = (
            it.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("link").and_then(|x| x.as_str()).unwrap_or(""),
            it.get("type").and_then(|x| x.as_str()).unwrap_or(""),
        );
        if baslik.is_empty() || bag.is_empty() {
            continue;
        }
        if !(bag.starts_with("http://") || bag.starts_with("https://")) {
            continue;
        }
        let sanatci = it
            .get("artist")
            .and_then(|a| a.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let tam = if sanatci.is_empty() {
            baslik.to_string()
        } else {
            format!("{} – {}", baslik, sanatci)
        };
        let parca = if sanatci.is_empty() && tur.is_empty() {
            "Deezer kaydı".to_string()
        } else if tur.is_empty() {
            sanatci.to_string()
        } else if sanatci.is_empty() {
            format!("Deezer {}", tur)
        } else {
            format!("Deezer {} · {}", tur, sanatci)
        };
        out.push((tam, bag.to_string(), parca));
        // limit artışı: Deezer 5→8
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// 46) Deezer müzik araması (anahtarsız JSON).
fn src_deezer(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let Some(body) = get_text(&format!(
        // limit artışı: Deezer 5→8
        "https://api.deezer.com/search?q={}&limit=8",
        enc(query)
    )) else {
        return;
    };
    let mut n = 0;
    for (ti, ur, sn) in parse_deezer(&body).into_iter().take(8) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "deezer".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Deezer({})", n));
    }
}

/// Nominatim gövdesinden (yer-adı, osm-url, enlem+boylam) çıkarır — dizi yoksa boş.
fn parse_nominatim(body: &str, sorgu: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // limit artışı: Nominatim 5→8
    for it in arr.iter().take(8) {
        let ad = it.get("display_name").and_then(|x| x.as_str()).unwrap_or("");
        if ad.trim().is_empty() {
            continue;
        }
        let enlem = it
            .get("lat")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| it.get("lat").and_then(|x| x.as_f64()).map(|n| n.to_string()))
            .unwrap_or_default();
        let boylam = it
            .get("lon")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| it.get("lon").and_then(|x| x.as_f64()).map(|n| n.to_string()))
            .unwrap_or_default();
        if enlem.is_empty() || boylam.is_empty() {
            continue;
        }
        out.push((
            ad.to_string(),
            format!("https://www.openstreetmap.org/search?query={}", enc(sorgu)),
            format!("{}, {}", enlem, boylam),
        ));
        // limit artışı: Nominatim 5→8
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// 47) Nominatim yer araması (anahtarsız JSON, özel UA + tek istek).
fn src_nominatim(query: &str, out: &mut Vec<Candidate>, sources: &mut Vec<String>) {
    let url = format!(
        // limit artışı: Nominatim 5→8
        "https://nominatim.openstreetmap.org/search?q={}&format=jsonv2&limit=8&accept-language=tr",
        enc(query)
    );
    let Some(body) = with_retry(|| {
        agent()
            .get(&url)
            .set("User-Agent", "NoralWeb/1.0 (noralweb@example.com)")
            .set("Accept", "application/json")
            .call()
    })
    .ok()
    .and_then(|r| r.into_string().ok())
    else {
        return;
    };
    let mut n = 0;
    // limit artışı: Nominatim 5→8
    for (ti, ur, sn) in parse_nominatim(&body, query).into_iter().take(8) {
        out.push(Candidate {
            title: ti,
            url: ur,
            snippet: sn,
            source: "nominatim".into(),
            depth: 0,
            page: String::new(),
        });
        n += 1;
    }
    if n > 0 {
        sources.push(format!("Nominatim({})", n));
    }
}

/// Sayfa çek: başlık + metin + dış linkler + meta. Yoksa None.
pub struct PageData {
    pub title: String,
    pub text: String,
    pub links: Vec<(String, String)>, // (anchor, url)
    pub meta_desc: String,
    pub og_image: String,
}

/// Yönlendirme-hub domainleri (ajan şikayeti #3): bir katman daha takip edilir.
const HUBS: [&str; 10] = [
    "linktr.ee", "linktree", "about.me", "beacons.ai", "carrd.co", "bio.link", "lnk.bio",
    "allmylinks.", "solo.to", "taplink.",
];

/// windows-1254 (Türkçe) bayt → char. Bozuk kodlamalı TR sayfalar için.
fn win1254_byte(b: u8) -> char {
    match b {
        0x80 => '€', 0x82 => '‚', 0x83 => 'ƒ', 0x84 => '„', 0x85 => '…',
        0x86 => '†', 0x87 => '‡', 0x88 => 'ˆ', 0x89 => '‰', 0x8A => 'Š',
        0x8B => '‹', 0x8C => 'Œ', 0x8E => 'Ž', 0x91 => '\'', 0x92 => '\'',
        0x93 => '"', 0x94 => '"', 0x95 => '•', 0x96 => '–', 0x97 => '—',
        0x98 => '˜', 0x99 => '™', 0x9A => 'š', 0x9B => '›', 0x9C => 'œ',
        0x9E => 'ž', 0x9F => 'Ÿ', 0xD0 => 'Ğ', 0xDD => 'İ', 0xDE => 'Ş',
        0xF0 => 'ğ', 0xFD => 'ı', 0xFE => 'ş',
        0x00..=0x7F | 0xA0..=0xCF | 0xD1..=0xDC | 0xDF..=0xEF | 0xF1..=0xFC | 0xFF => {
            b as char
        }
        _ => '�',
    }
}

/// Ham bayt → metin: önce UTF-8, olmazsa windows-1254 (ajan şikayeti #6).
fn decode_body(raw: Vec<u8>) -> String {
    match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| win1254_byte(b)).collect(),
    }
}

fn html_unescape(s: &str) -> String {
    let mut o = s.to_string();
    for (a, b) in [
        ("&amp;", "&"),
        ("&quot;", "\""),
        ("&#x27;", "'"),
        ("&#39;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
    ] {
        o = o.replace(a, b);
    }
    o
}

/// <meta attr="val" content="..."> değerini al (sıra bağımsız, bayt-güvenli).
fn meta_content(body: &str, attr: &str, val: &str) -> String {
    let mut pos = 0;
    while pos + 6 < body.len() {
        let s = match body.get(pos..).and_then(|r| r.find("<meta")) {
            Some(i) => pos + i,
            None => break,
        };
        // Sabit bayt ofseti Türkçe sınırını kesmesin — güvenli sınıra indir.
        let ham = body[s..].find('>').map(|k| s + k).unwrap_or((s + 600).min(body.len()));
        let e = body.floor_char_boundary(ham);
        let son = body.floor_char_boundary((s + 800).min(body.len()).min(e));
        let tag = body.get(s..son).unwrap_or("");
        let low = tag.to_lowercase();
        if low.contains(attr) && low.contains(val) {
            for q in ["content=\"", "content='"] {
                if let Some(c) = tag.find(q) {
                    let a = c + q.len();
                    let qc = if q.ends_with('"') { '"' } else { '\'' };
                    if let Some(z) = tag[a..].find(qc) {
                        return html_unescape(tag[a..a + z].trim());
                    }
                }
            }
        }
        pos = s + 5;
    }
    String::new()
}

/// Metin-içi çıplak URL'leri topla (ajan şikayeti #3).
fn bare_urls(body: &str, base_host: &str) -> Vec<String> {
    let b = body.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 < n && out.len() < 10 {
        let rest = &body[i..];
        let adv = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            0
        };
        if adv == 0 {
            let ch = rest.chars().next().unwrap_or(' ');
            i += ch.len_utf8().max(1);
            continue;
        }
        let mut j = i + adv;
        while j < n && !b" \t\n\r\"'<>".contains(&b[j]) {
            j += 1;
        }
        let url = body[i..j].to_string();
        i = j;
        if url.len() > 14 && url.contains('.') && host_of(&url) != base_host {
            let low = url.to_lowercase();
            if !BAD_EXT.iter().any(|e| low.contains(e)) && !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

pub fn fetch_page(url: &str) -> Option<PageData> {
    fetch_page_inner(url, true)
}

fn fetch_page_inner(url: &str, follow_hubs: bool) -> Option<PageData> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let low = url.to_lowercase();
    if BAD_EXT.iter().any(|e| low.contains(e)) {
        return None;
    }
    let resp = with_retry(|| {
        agent_fast()
            .get(url)
            .set("User-Agent", ua_rot())
            .set("Accept", "text/html")
            .set("Accept-Charset", "utf-8")
            .call()
    })
    .ok()?;
    let ct = resp.header("content-type").unwrap_or("").to_lowercase();
    if !ct.is_empty() && !ct.contains("html") && !ct.contains("text") {
        return None;
    }
    if let Some(len) = resp.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if len > 800_000 {
            return None;
        }
    }
    let mut raw: Vec<u8> = Vec::new();
    use std::io::Read;
    resp.into_reader().take(800_001).read_to_end(&mut raw).ok()?;
    if raw.len() > 800_000 || raw.len() < 200 {
        return None;
    }
    let body = decode_body(raw);
    if body.len() < 200 {
        return None;
    }
    // <title> + meta/og (ajan şikayeti #7)
    let title = body
        .find("<title>")
        .and_then(|s| {
            body[s + 7..]
                .find("</title>")
                .map(|e| strip_tags(&body[s + 7..s + 7 + e]))
        })
        .map(|t| t.chars().take(200).collect::<String>())
        .unwrap_or_default();
    let meta_desc = {
        let d = meta_content(&body, "name", "description");
        if d.is_empty() {
            meta_content(&body, "property", "og:description")
        } else {
            d
        }
        .chars()
        .take(400)
        .collect::<String>()
    };
    let og_title = meta_content(&body, "property", "og:title");
    let og_image = meta_content(&body, "property", "og:image");
    let title = if title.is_empty() && !og_title.is_empty() {
        og_title.chars().take(200).collect()
    } else {
        title
    };
    // linkler
    let base_host = host_of(url);
    let base_proto = if url.starts_with("https") { "https" } else { "http" };
    let mut links: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    while links.len() < 12 {
        let a = match body[pos..].find("<a ") {
            Some(i) => pos + i,
            None => break,
        };
        let tag_e = match body[a..].find('>') {
            Some(i) => a + i,
            None => {
                pos = a + 3;
                continue;
            }
        };
        let tag = &body[a..tag_e];
        let href = if let Some(h) = tag.find("href=\"") {
            let s = h + 6;
            tag[s..].find('"').map(|e| tag[s..s + e].to_string())
        } else if let Some(h) = tag.find("href='") {
            let s = h + 6;
            tag[s..].find('\'').map(|e| tag[s..s + e].to_string())
        } else {
            None
        };
        let close = body[tag_e..].find("</a>").map(|i| tag_e + i).unwrap_or(tag_e);
        // kapanış yoksa veya etiketin önündeyse: güvenli atla
        if close <= tag_e + 1 {
            pos = tag_e + 1;
            continue;
        }
        // anchor: önce sınır-güvenli ham dilim, uzunluk kısaltması karakterle
        let anchor_raw = &body[tag_e + 1..close];
        let anchor_full = strip_tags(anchor_raw);
        let anchor: String = anchor_full.chars().take(140).collect();
        pos = close + 4;
        let Some(mut href) = href else { continue };
        href = href.replace("&amp;", "&");
        if href.starts_with("//") {
            href = format!("{}:{}", base_proto, href);
        } else if href.starts_with('/') {
            href = format!("{}://{}{}", base_proto, base_host, href);
        } else if !(href.starts_with("http://") || href.starts_with("https://")) {
            continue; // göreli karmaşık linkleri atla
        }
        let low = href.to_lowercase();
        if BAD_EXT.iter().any(|e| low.contains(e)) {
            continue;
        }
        if host_of(&href) == base_host {
            continue; // site-içi linkleri atla, dışarı açıl
        }
        if anchor.chars().count() < 3 || anchor.chars().count() > 140 {
            continue;
        }
        links.push((anchor, href));
    }
    // metin-içi çıplak URL'ler
    for u in bare_urls(&body, &base_host) {
        if links.len() >= 16 {
            break;
        }
        if !links.iter().any(|(_, x)| x == &u) {
            links.push(("(metin içi)".to_string(), u));
        }
    }
    // hub takibi: linktree/about.me vb. bir katman daha açılır
    if follow_hubs {
        let hubs: Vec<String> = links
            .iter()
            .filter(|(_, u)| HUBS.iter().any(|h| host_of(u).contains(h)))
            .take(3)
            .map(|(_, u)| u.clone())
            .collect();
        for h in hubs {
            if let Some(sub) = fetch_page_inner(&h, false) {
                for (a, u) in sub.links.into_iter().take(6) {
                    if links.len() >= 22 {
                        break;
                    }
                    if !links.iter().any(|(_, x)| x == &u) {
                        links.push((format!("hub: {}", a.chars().take(100).collect::<String>()), u));
                    }
                }
            }
            if links.len() >= 22 {
                break;
            }
        }
    }
    // sayfa metni: script/style/nav/footer/header bloklarını bayt-güvenli at
    let mut clean = body.clone();
    for tag in ["script", "style", "nav", "footer", "header"] {
        clean = cut_blocks(&clean, tag);
        if clean.len() < 200 {
            break;
        }
    }
    let mut text = strip_tags(&clean);
    if text.chars().count() < 50 && !meta_desc.is_empty() {
        text = meta_desc.clone(); // metinsiz sayfada meta açıklama
    }
    let text: String = text.chars().take(1500).collect();
    Some(PageData { title, text, links, meta_desc, og_image })
}

/// Apinex ile sayfa içeriği (bot duvarlı siteler için yedek — ajan şikayeti #1).
/// Maliyet $0.00005/sayfa, sayaça işlenir.
pub fn apinex_contents(url: &str) -> Option<(String, String)> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let v = apinex_post(
        "/v1/tools/web/contents",
        &serde_json::json!({"urls": [url]}),
        0.00005,
    )?;
    let r = v.get("results")?.as_array()?.first()?;
    let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let md = r.get("markdown").and_then(|x| x.as_str()).unwrap_or("");
    if md.is_empty() {
        return None;
    }
    Some((title, md.chars().take(3000).collect()))
}

/// Ana giriş: deep=true ise 2. halka taraması da yapılır.
/// 1. halka kaynakları paralel çalışır (toplam süre ~en yavaş kaynak).
pub fn live_search(query: &str, deep: bool, apx: u8) -> (Vec<Candidate>, Vec<String>, Vec<String>, f64) {
    let q = query.to_string();
    let mk = |f: fn(&str, &mut Vec<Candidate>, &mut Vec<String>)| {
        let qq = q.clone();
        std::thread::spawn(move || {
            let mut o = Vec::new();
            let mut s = Vec::new();
            f(&qq, &mut o, &mut s);
            (o, s)
        })
    };
    // 1. faz: ücretsiz kaynaklar (hep paralel).
    let mut free_jobs = vec![
        mk(src_ddg_ia),
        mk(src_wiki_os),
        mk(src_wiki_full),
        mk(src_wikidata),
        mk(src_dbpedia),
        mk(src_github),
        mk(src_stack),
        mk(src_so_users),
        mk(src_hn),
        mk(src_openalex),
        mk(src_npm),
        mk(src_crates),
        mk(src_arxiv),
        mk(src_mastodon),
        mk(src_bsky),
        mk(src_brave),
        mk(src_wiby),
        mk(src_searxng),
        mk(src_cc),
        mk(src_exa),
        mk(src_tavily),
        mk(src_serper),
        mk(src_langsearch),
        mk(src_ddg_html),
        mk(src_bing),
        mk(src_google),
        mk(src_marginalia),
        mk(src_gnews),
        mk(src_youtube),
        mk(src_semscholar),
        mk(src_crossref),
        mk(src_orcid),
        mk(src_gdelt),
        mk(src_openlib),
        mk(src_bing_sosyal),
        mk(src_yahoo),
        mk(src_ecosia),
        mk(src_braveweb),
        mk(src_yandex),
        mk(src_qwant),
        mk(src_reddit),
        mk(src_wikiara),
        mk(src_deezer),
        mk(src_nominatim),
    ];
    // Derin modda SearXNG 2. sayfa da paralel koşar.
    if deep {
        free_jobs.push(mk(src_searxng_p2));
        free_jobs.push(mk(src_google_p2));
        free_jobs.push(mk(src_bing_p2));
    }
    let mut out: Vec<Candidate> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for j in free_jobs {
        if let Ok((mut o, mut s)) = j.join() {
            out.append(&mut o);
            sources.append(&mut s);
        }
    }
    // 2. faz: Apinex — apx: 0=kapalı, 1=zayıfken (<10 aday), 2=her zaman.
    let use_apx = apinex_key().is_some() && (apx == 2 || (apx == 1 && out.len() < 10));
    if use_apx {
        let mut paid = vec![mk(src_apinex), mk(src_apinex_twitter)];
        if deep {
            paid.push(mk(src_apinex_research));
        }
        for j in paid {
            if let Ok((mut o, mut s)) = j.join() {
                out.append(&mut o);
                sources.append(&mut s);
            }
        }
    }

    // Suskun kaynaklar (UI'da gri görünür).
    // Google-H: gizli hasat yedeği (main.rs rank öncesi ekler).
    const BEKLENEN: [&str; 45] = [
        "DuckDuckGo", "Wikipedia-tr", "Wikipedia-en", "WikiTam-tr", "WikiTam-en",
        "Wikidata", "DBpedia", "GitHub", "Stack", "SO-kullanıcı", "HN", "Akademik", "npm",
        "crates", "arXiv", "DDG-Web", "Wiby", "SearXNG", "CC", "Exa", "Tavily", "LangSearch",
        "Bing", "Google", "Google-H", "Marginalia", "GNews", "YouTube", "SemScholar", "Crossref",
        "ORCID", "GDELT", "OpenLib", "Bing-Sosyal",
        "Yahoo", "Ecosia", "BraveWeb", "Yandex", "Qwant", "Reddit",
        "WikiAra-tr", "WikiAra-en", "Deezer", "Nominatim", "Yandex-H",
    ];
    let mut silent: Vec<String> = Vec::new();
    for b in BEKLENEN {
        if !sources.iter().any(|s| s.starts_with(b)) {
            silent.push(b.to_string());
        }
    }
    // Brave anahtarsızsa suskun sayılır (API etiketi "Brave(" — BraveWeb'e dokunmaz).
    if !sources.iter().any(|s| s.starts_with("Brave(")) {
        silent.push("Brave(anahtar yok)".to_string());
    }
    // Mastodon/Bluesky/Twitter/Apinex/Serper suskunları da ekle.
    for b in ["Mastodon", "Bluesky", "Twitter", "Apinex", "ApinexDerin", "Serper"] {
        if !sources.iter().any(|s| s.starts_with(b)) {
            silent.push(b.to_string());
        }
    }

    // Tekille: aynı URL bir kez.
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(norm_url(&c.url)));

    // 2. halka: akıllı tohum — önce yüksek sinyalli kaynaklar, alaka kapılı.
    if deep && !out.is_empty() {
        fn seed_rank(c: &Candidate) -> u8 {
            match c.source.as_str() {
                "github" => 0,
                "wikidata" => 1,
                "stack" => 2,
                "brave" => 3,
                "ddg-özet" => 4,
                "ddg-web" => 5,
                "hackernews" => 6,
                "mastodon" => 7,
                "bluesky" => 7,
                "wikipedia" => 8,
                "ddg" => 9,
                _ => 10,
            }
        }
        let mut idx: Vec<usize> = (0..out.len()).collect();
        idx.sort_by_key(|&i| seed_rank(&out[i]));
        let seeds: Vec<String> = idx
            .into_iter()
            .filter(|&i| overlap(query, &out[i].title, &out[i].snippet) >= 0.34)
            .take(8)
            .map(|i| out[i].url.clone())
            .collect();
        let mut handles = Vec::new();
        for url in seeds {
            handles.push(std::thread::spawn(move || (url.clone(), fetch_page(&url))));
        }
        let mut ring2: Vec<Candidate> = Vec::new();
        for h in handles {
            let Ok((url, page)) = h.join() else { continue };
            let Some(p) = page else { continue };
            // sayfa metnini tohum adaya işle
            if let Some(seed) = out.iter_mut().find(|c| c.url == url) {
                if !p.title.is_empty() && seed.title.len() < 8 {
                    seed.title = p.title.clone();
                }
                seed.page = p.text.clone();
            }
            for (anchor, link) in p.links.into_iter().take(8) {
                let nu = norm_url(&link);
                if seen.contains(&nu) {
                    continue;
                }
                seen.insert(nu);
                ring2.push(Candidate {
                    title: anchor.clone(),
                    url: link,
                    snippet: format!("Bağlantı metni: {}", anchor),
                    source: "derin".into(),
                    depth: 1,
                    page: String::new(),
                });
                // derin halka cap artışı: 24→32 (tohum sayısı değişmedi)
                if ring2.len() >= 32 {
                    break;
                }
            }
            // derin halka cap artışı: 24→32
            if ring2.len() >= 32 {
                break;
            }
        }
        if !ring2.is_empty() {
            sources.push(format!("DerinHalka(+{})", ring2.len()));
            out.extend(ring2);
        }
    }

    (out, sources, silent, take_cost_usd())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win1254_turkce() {
        // ğ ı ş Ğ İ Ş ç ç → windows-1254 baytları (ş=FE, Ş=DE)
        let raw = vec![0xF0u8, 0xFD, 0xFE, 0xD0, 0xDD, 0xDE, 0xE7, 0xE7];
        assert_eq!(decode_body(raw), "ğışĞİŞçç");
    }

    #[test]
    fn utf8_bozulmaz() {
        assert_eq!(decode_body("Berkcan Özbalci".as_bytes().to_vec()), "Berkcan Özbalci");
    }

    #[test]
    fn meta_og() {
        let html = r#"<html><head><meta property="og:title" content="Başlık X"><meta name="description" content="Açıklama Y"><meta property="og:image" content="https://x.com/i.png"></head></html>"#;
        assert_eq!(meta_content(html, "property", "og:title"), "Başlık X");
        assert_eq!(meta_content(html, "name", "description"), "Açıklama Y");
        assert_eq!(meta_content(html, "property", "og:image"), "https://x.com/i.png");
        assert_eq!(meta_content(html, "property", "og:video"), "");
    }

    #[test]
    fn ciplak_url() {
        let body = "bak şuraya https://ornek.com/sayfa ve devam";
        let u = bare_urls(body, "baska.com");
        assert_eq!(u, vec!["https://ornek.com/sayfa".to_string()]);
        // kendi hostu elenir
        let u2 = bare_urls("https://baska.com/a", "baska.com");
        assert!(u2.is_empty());
    }

    #[test]
    fn searxng_parse() {
        // ekstra alanlar + eksik alanlar bir arada
        let body = r#"{"query":"test","results":[
            {"url":"https://a.com/1","title":"Başlık 1","content":"Açıklama 1","engine":"g1","extra":42},
            {"url":"https://b.com/2","title":"Başlık 2"},
            {"url":"","title":"Boş URL"},
            {"url":"https://c.com/3","title":""},
            {"url":"https://d.com/4","title":"Başlık 4","content":"","engine":"g2"}
        ],"suggestions":[],"unresponsive_engines":[]}"#;
        let r = parse_searxng(body);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], ("Başlık 1".to_string(), "https://a.com/1".to_string(), "Açıklama 1".to_string()));
        assert_eq!(r[1].2, "");
        assert_eq!(r[2].0, "Başlık 4".to_string());
        // boş results + bozuk gövde
        assert!(parse_searxng(r#"{"query":"x","results":[]}"#).is_empty());
        assert!(parse_searxng("bu json değil").is_empty());
        assert!(parse_searxng(r#"{"query":"x"}"#).is_empty());
    }

    #[test]
    fn bing_parse() {
        // 1 direkt + 1 ck/a linkli b_algo bloğu
        let html = r#"<html><body><ol>
<li class="b_algo"><h2><a href="https://ornek.com/birinci">Birinci Başlık</a></h2><p>Birinci açıklama metni.</p></li>
<li class="b_algo"><h2><a href="/ck/a?!&&p=abc&u=a1aHR0cHM6Ly9leGFtcGxlLmNvbQ&m=xyz">İkinci Başlık</a></h2><p>İkinci açıklama.</p></li>
</ol></body></html>"#;
        let r = parse_bing_html(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Başlık");
        assert_eq!(r[0].1, "https://ornek.com/birinci");
        assert_eq!(r[0].2, "Birinci açıklama metni.");
        assert_eq!(r[1].0, "İkinci Başlık");
        assert_eq!(r[1].1, "https://example.com");
        assert_eq!(r[1].2, "İkinci açıklama.");
        // b_algo yoksa sessiz boş
        assert!(parse_bing_html("<html><body>sonuç yok</body></html>").is_empty());
        // çözülemez ck/a atlanır
        let kotu = r#"<li class="b_algo"><h2><a href="/ck/a?!&&p=x&u=a1!!!bozuk!!!&m=y">Kötü</a></h2><p>X</p></li>"#;
        assert!(parse_bing_html(kotu).is_empty());
    }

    #[test]
    #[ignore]
    fn ddg_yedek_canli() {
        // html ucu challenge'lı sorguda lite yedeği devreye girmeli.
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_ddg_html("yenal tanoren", &mut o, &mut s);
        assert!(!o.is_empty(), "DDG lite yedeği sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn bing_p2_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_bing_p2("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "Bing 2. sayfa sonuç dönmedi");
    }

    #[test]
    fn ddg_lite_parse() {
        // lite örüntüsü: tek tırnaklı result-link + result-snippet hücresi.
        let html = "<html><body><table><tr><td><a rel=\"nofollow\" href=\"https://ornek.com/1\" class='result-link'>Birinci Başlık</a></td></tr><tr><td class='result-snippet'>Birinci açıklama.</td></tr><tr><td><a rel=\"nofollow\" href=\"//yenal.com/2\" class='result-link'>İkinci</a></td></tr></table></body></html>";
        let r = parse_ddg_lite(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Başlık");
        assert_eq!(r[0].1, "https://ornek.com/1");
        assert_eq!(r[0].2, "Birinci açıklama.");
        assert_eq!(r[1].1, "https://yenal.com/2");
        assert!(parse_ddg_lite("<html><body>sonuç yok</body></html>").is_empty());
    }

    #[test]
    fn google_parse() {
        // 2 /url?q= bloğu
        let html = r#"<html><body>
<div><a href="/url?q=https://ornek.com/sayfa1&sa=U&ved=1"><h3>Sayfa Bir Başlık</h3></a><div>Birinci snippet metni burada.</div></div>
<div><a href="/url?q=https://example.com/page2%3Fid%3D5&sa=U"><h3>Second Title</h3></a><div>Second snippet text.</div></div>
</body></html>"#;
        let r = parse_google_html(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Sayfa Bir Başlık");
        assert_eq!(r[0].1, "https://ornek.com/sayfa1");
        assert!(r[0].2.contains("Birinci snippet"));
        assert_eq!(r[1].0, "Second Title");
        assert_eq!(r[1].1, "https://example.com/page2?id=5");
        // /url?q= yoksa boş
        assert!(parse_google_html("<html>boş</html>").is_empty());
        // duvar sayfası sessiz döner
        assert!(parse_google_html(r#"<html><div id="sorry">sorry google</div><a href="/url?q=https://a.com&sa=U"><h3>T</h3></a></html>"#).is_empty());
        assert!(parse_google_html(r#"<html><a href="/url?q=https://a.com&sa=U"><h3>T</h3></a>captcha here</html>"#).is_empty());
        // JS-shell (2026-08 curl kanıtı, ~92KB): /url?q= ve <h3> yok, sorry/captcha
        // da yok — duvar DEĞİL yeni markup değil, JS challenge. Parser'a dokunulmaz,
        // hasat tek yoldur; boş dönmesi beklenir.
        let kabuk = r#"<html lang="tr"><head><title>Google Search</title></head><body><noscript><meta content="0;url=/httpservice/retry/enablejs?sei=xyz" http-equiv="refresh"><div>Birkaç saniye içinde yönlendirilmezseniz <a href="/httpservice/retry/enablejs?sei=xyz">burayı</a> tıklayın.</div></noscript><div id="yvlrue" style="display:none">Google Arama'ya erişme konusunda sorun yaşıyorsanız <a href="/search?q=rust&amp;hl=tr&amp;emsg=SG_REL">burayı tıklayın</a> veya <a href="https://support.google.com/websearch">geri bildirim</a> gönderin.</div></body></html>"#;
        assert!(parse_google_html(kabuk).is_empty());
    }

    #[test]
    fn cc_index_yedegi() {
        // collinfo.json'daki en yeni 2 index: birincil + yedek (2026-08 doğrulandı).
        assert_eq!(CC_INDEXES.len(), 2);
        assert_eq!(CC_INDEXES[0], "CC-MAIN-2026-34");
        assert_eq!(CC_INDEXES[1], "CC-MAIN-2026-30");
    }

    #[test]
    fn uzun_ajan_suresi() {
        // Yavaş ikili (Marginalia + OpenLib) ilk atışta budanmasın: 6sn yerine 12sn.
        assert_eq!(AGENT_LONG_SECS, 12);
        assert!(AGENT_LONG_SECS > 6);
    }

    #[test]
    fn marginalia_parse() {
        // gerçek şema
        let body = r#"{"query":"rust","results":[
            {"url":"https://www.rust-lang.org/","title":"Rust","description":"Sistem dili","quality":3.8},
            {"url":"","title":"Boş URL","description":"x","quality":1.0},
            {"url":"https://example.com","title":"","description":"y","quality":1.0}
        ]}"#;
        let r = parse_marginalia(body);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], ("Rust".to_string(), "https://www.rust-lang.org/".to_string(), "Sistem dili".to_string()));
        // boş + bozuk durumlar
        assert!(parse_marginalia(r#"{"query":"x","results":[]}"#).is_empty());
        assert!(parse_marginalia("bu json değil").is_empty());
        assert!(parse_marginalia(r#"{"query":"x"}"#).is_empty());
    }

    #[test]
    fn gnews_parse() {
        // Gerçek RSS şeması: 2 item (CDATA + kaynak adı).
        let body = r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><title>test</title>
<item><title><![CDATA[Birinci Haber Başlığı]]></title><link>https://news.google.com/rss/articles/CBMiTest1?oc=5</link><source url="https://ornek.com">Örnek Gazete</source><pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate></item>
<item><title>İkinci Haber &amp; Gelişme</title><link>https://news.google.com/rss/articles/CBMiTest2?oc=5</link><source url="https://misal.com">Misal Haber</source></item>
</channel></rss>"#;
        let r = parse_gnews(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Haber Başlığı");
        assert_eq!(r[0].1, "https://news.google.com/rss/articles/CBMiTest1?oc=5");
        assert_eq!(r[0].2, "Örnek Gazete");
        assert_eq!(r[1].0, "İkinci Haber & Gelişme");
        assert_eq!(r[1].2, "Misal Haber");
        // Boş / bozuk durumlar sessiz döner.
        assert!(parse_gnews("<rss><channel></channel></rss>").is_empty());
        assert!(parse_gnews("bu rss değil").is_empty());
        assert!(parse_gnews("<item><title>Bağlantısız</title></item>").is_empty());
    }

    #[test]
    fn youtube_parse() {
        // ytInitialData içinde 1 videoRenderer (runs başlığı + kanal).
        let body = r#"<html><body><script>var ytInitialData = {"contents":{"twoColumnSearchResultsRenderer":{"primaryContents":{"sectionListRenderer":{"contents":[{"itemSectionRenderer":{"contents":[{"videoRenderer":{"videoId":"dQw4w9WgXcQ","title":{"runs":[{"text":"Test Videosu Başlığı"}]},"ownerText":{"runs":[{"text":"Test Kanalı"}]}}}]}}]}}}}}};</script></body></html>"#;
        let r = parse_youtube(body);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "Test Videosu Başlığı");
        assert_eq!(r[0].1, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(r[0].2, "Test Kanalı");
        // simpleText yedeği.
        let sade = r#"<html><script>var ytInitialData = {"contents":[{"videoRenderer":{"videoId":"abc123XYZ-_","title":{"simpleText":"Sade Başlık"}}}]}</script></html>"#;
        let r2 = parse_youtube(sade);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].0, "Sade Başlık");
        assert_eq!(r2[0].1, "https://www.youtube.com/watch?v=abc123XYZ-_");
        // ytInitialData yoksa / bozuksa sessiz dön.
        assert!(parse_youtube("<html><body>sonuç yok</body></html>").is_empty());
        assert!(parse_youtube("var ytInitialData = {bozuk json").is_empty());
    }

    #[test]
    fn semscholar_parse() {
        // Gerçek şema: 2 kayıt (biri URL'siz → paperId yedeği).
        let body = r#"{"total":2,"offset":0,"data":[
            {"paperId":"abc123","title":"Derin Öğrenme ile Test","url":"https://arxiv.org/abs/1234.5678","abstract":"Bu çalışma test eder.","year":2023,"authors":[{"name":"Ali Veli"}]},
            {"paperId":"def456","title":"İkinci Makale","url":null,"abstract":"","year":2021,"authors":[]}
        ]}"#;
        let r = parse_semscholar(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Derin Öğrenme ile Test");
        assert_eq!(r[0].1, "https://arxiv.org/abs/1234.5678");
        assert!(r[0].2.contains("Bu çalışma test eder."));
        assert_eq!(r[1].1, "https://www.semanticscholar.org/paper/def456");
        // Boş / bozuk durumlar.
        assert!(parse_semscholar(r#"{"total":0,"data":[]}"#).is_empty());
        assert!(parse_semscholar("bu json değil").is_empty());
        assert!(parse_semscholar(r#"{"total":0}"#).is_empty());
    }

    #[test]
    fn crossref_parse() {
        // Gerçek şema: 2 kayıt (biri URL'siz → DOI yedeği).
        let body = r#"{"status":"ok","message":{"items":[
            {"DOI":"10.1000/test1","title":["Birinci Çalışma Başlığı"],"URL":"https://example.com/makale1","author":[{"given":"Ayşe","family":"Yılmaz"},{"given":"Mehmet","family":"Demir"}],"published":{"date-parts":[[2022,5,1]]}},
            {"DOI":"10.1000/test2","title":["İkinci Çalışma"],"URL":"","author":[],"published-print":{"date-parts":[[2020]]}}
        ]}}"#;
        let r = parse_crossref(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Çalışma Başlığı");
        assert_eq!(r[0].1, "https://example.com/makale1");
        assert!(r[0].2.contains("Ayşe Yılmaz"));
        assert!(r[0].2.contains("2022"));
        assert_eq!(r[1].1, "https://doi.org/10.1000/test2");
        assert!(r[1].2.contains("2020"));
        // Boş / bozuk durumlar.
        assert!(parse_crossref(r#"{"status":"ok","message":{"items":[]}}"#).is_empty());
        assert!(parse_crossref("bu json değil").is_empty());
        assert!(parse_crossref(r#"{"status":"ok"}"#).is_empty());
    }

    #[test]
    fn orcid_parse() {
        // Gerçek şema: 2 araştırmacı kaydı.
        let body = r#"{"num-found":2,"expanded-result":[
            {"orcid-id":"0000-0001-2345-6789","given-names":"Ayşe","family-names":"Yılmaz"},
            {"orcid-id":"0000-0002-1825-0097","given-names":"Mehmet","family-names":"Demir"}
        ]}"#;
        let r = parse_orcid(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Ayşe Yılmaz");
        assert_eq!(r[0].1, "https://orcid.org/0000-0001-2345-6789");
        assert_eq!(r[1].0, "Mehmet Demir");
        // Boş / bozuk durumlar.
        assert!(parse_orcid(r#"{"expanded-result":[]}"#).is_empty());
        assert!(parse_orcid("bu json değil").is_empty());
        assert!(parse_orcid(r#"{"num-found":0}"#).is_empty());
    }

    #[test]
    fn gdelt_parse() {
        // Gerçek şema: 2 haber kaydı (domain + tarih).
        let body = r#"{"status":"ok","articles":[
            {"title":"Birinci Haber","url":"https://ornek.com/haber1","seendate":"20240101T120000Z","domain":"ornek.com"},
            {"title":"İkinci Haber","url":"https://misal.com/haber2","seendate":"20240202T080000Z","domain":"misal.com"}
        ]}"#;
        let r = parse_gdelt(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Haber");
        assert!(r[0].2.contains("ornek.com"));
        assert!(r[0].2.contains("20240101T120000Z"));
        // articles yoksa / bozuksa sessiz dön.
        assert!(parse_gdelt(r#"{"status":"ok"}"#).is_empty());
        assert!(parse_gdelt("bu json değil").is_empty());
        assert!(parse_gdelt(r#"{"articles":[]}"#).is_empty());
    }

    #[test]
    fn openlib_parse() {
        // Gerçek şema: 2 kitap kaydı.
        let body = r#"{"numFound":2,"docs":[
            {"key":"/works/OL123W","title":"Birinci Kitap","author_name":["Orhan Pamuk"],"first_publish_year":2002},
            {"key":"/works/OL456W","title":"İkinci Kitap","author_name":["Yaşar Kemal","Sabahattin Ali"],"first_publish_year":1970}
        ]}"#;
        let r = parse_openlib(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Kitap");
        assert_eq!(r[0].1, "https://openlibrary.org/works/OL123W");
        assert!(r[0].2.contains("Orhan Pamuk"));
        assert!(r[1].2.contains("Yaşar Kemal"));
        // Boş / bozuk durumlar.
        assert!(parse_openlib(r#"{"numFound":0,"docs":[]}"#).is_empty());
        assert!(parse_openlib("bu json değil").is_empty());
        assert!(parse_openlib(r#"{"numFound":0}"#).is_empty());
    }

    #[test]
    #[ignore]
    fn s2_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_semscholar("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "SemScholar sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn crossref_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_crossref("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "Crossref sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn gnews_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_gnews("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "GNews sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn bing_canli() {
        // havuzdan sonuç gelirse geçsin
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_bing("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "Bing sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn marginalia_canli() {
        // havuzdan sonuç gelirse geçsin
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_marginalia("rust", &mut o, &mut s);
        assert!(!o.is_empty(), "Marginalia sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn searxng_canli_prob() {
        // Havuzdan en az biri dolu sonuç dönmeli.
        let mut ok = 0;
        for inst in SEARXNG_INSTANCES {
            let url = format!(
                "{}/search?q={}&format=json&categories=general&language=all&pageno=1&safesearch=0",
                inst,
                enc("rust")
            );
            let Ok(resp) = agent()
                .get(&url)
                .set("User-Agent", ua_rot())
                .set("Accept", "application/json")
                .call()
            else {
                continue;
            };
            if !resp.header("content-type").unwrap_or("").to_lowercase().contains("json") {
                continue;
            }
            let Ok(body) = resp.into_string() else { continue };
            if !parse_searxng(&body).is_empty() {
                ok += 1;
                break;
            }
        }
        assert!(ok >= 1, "hiçbir SearXNG instance'ı sonuç dönmedi");
    }

    #[test]
    fn yahoo_parse() {
        // RU= şifreli 2 blok (RK + RS kesimi), aria-label + h3 yedeği.
        let html = r#"<html><body>
<div class="algo-sr"><div class="compTitle"><a href="https://r.search.yahoo.com/_ylt=abc/RU=https%3a%2f%2fexample.com%2fsayfa1/RK=2/RS=xyz"><h3 aria-label="Birinci Başlık">Birinci <b>Başlık</b></h3></a></div><div class="compText">Birinci açıklama metni.</div></div>
<div class="algo-sr"><div class="compTitle"><a href="https://r.search.yahoo.com/_ylt=def/RU=https%3a%2f%2fornek.com%2fikinci%3Fid%3D5/RS=abc"><h3>İkinci Başlık</h3></a></div><div class="compText">İkinci açıklama.</div></div>
</body></html>"#;
        let r = parse_yahoo(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Başlık");
        assert_eq!(r[0].1, "https://example.com/sayfa1");
        assert!(r[0].2.contains("Birinci açıklama"));
        assert_eq!(r[1].0, "İkinci Başlık");
        assert_eq!(r[1].1, "https://ornek.com/ikinci?id=5");
        // Boş / bozuk haller sessiz döner.
        assert!(parse_yahoo("<html><body>sonuç yok</body></html>").is_empty());
        assert!(parse_yahoo(r#"<div class="algo-sr"><div class="compTitle"><a href="/search?p=test">Kötü</a></div></div>"#).is_empty());
    }

    #[test]
    fn ecosia_parse() {
        // 2 geçerli + iç arama ve /images elenenler.
        let html = r#"<html><body>
<div><a href="https://example.com/sayfa1"><h2>Birinci Başlık</h2></a><p>Birinci açıklama.</p></div>
<div><a href="https://www.ecosia.org/search?q=test">İç arama</a></div>
<div><a href="https://example.com/images/kedi"><h2>Görsel</h2></a></div>
<div><a href="https://ornek.com/ikinci"><h2>İkinci Başlık</h2></a><p>İkinci açıklama metni.</p></div>
</body></html>"#;
        let r = parse_ecosia(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Başlık");
        assert_eq!(r[0].1, "https://example.com/sayfa1");
        assert!(r[0].2.contains("Birinci açıklama"));
        assert_eq!(r[1].0, "İkinci Başlık");
        // Boş / bozuk haller.
        assert!(parse_ecosia("<html>boş</html>").is_empty());
        assert!(parse_ecosia("bu html değil ama href yok").is_empty());
    }

    #[test]
    fn braveweb_parse() {
        // Mini SvelteKit JSON blobu (gerçek JSON parse şart).
        let blob = r#"<html><body><script type="application/json">{"body":{"response":{"web":{"results":[{"url":"https://example.com/1","title":"Başlık 1","description":"Açıklama 1"},{"url":"https://ornek.com/2","title":"Başlık 2","description":"Açıklama 2"}]}}}}</script></body></html>"#;
        let r = parse_braveweb(blob);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Başlık 1");
        assert_eq!(r[0].1, "https://example.com/1");
        assert!(r[0].2.contains("Açıklama 1"));
        // Yedek: data-type="web" bloğu.
        let yedek = r#"<html><body><div data-type="web"><a href="https://example.com/yedek">Yedek Başlık</a><p>Yedek açıklama.</p></div></body></html>"#;
        let r2 = parse_braveweb(yedek);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].0, "Yedek Başlık");
        // Gerçek parça (2026-08, 332KB gövdeden): response JSON'u yok, data-type
        // bloklarında iç içe favicon div'leri var — yedek yine link çıkarmalı.
        let gercek = r#"<div data-type="web" data-keynav="true"><div class="result-body svelte-1rq4ngz"><div class="result-content svelte-1rq4ngz"><a href="https://rust-lang.org/en-US/" target="_self" class="svelte-14r20fy l1"><div class="site-name-wrapper svelte-on1hvy"><img src="https://imgs.search.brave.com/x" alt="" loading="lazy"/></div><div class="title">Rust Programming Language</div></a><p>Rust systems language.</p></div></div></div>"#;
        let r3 = parse_braveweb(gercek);
        assert!(!r3.is_empty());
        assert_eq!(r3[0].1, "https://rust-lang.org/en-US/");
        assert!(r3[0].0.contains("Rust"));
        // Boş haller.
        assert!(parse_braveweb("<html><body>sonuç yok</body></html>").is_empty());
        assert!(parse_braveweb("bozuk { json").is_empty());
    }

    #[test]
    fn yandex_parse() {
        // Direkt + reklam elenen + /r?u= yönlendirmeli serp-item.
        let html = r#"<html><body><ul>
<li class="serp-item"><a class="OrganicTitle-Link" href="https://example.com/sayfa"><span>Birinci Başlık</span></a><div>Birinci açıklama metni.</div></li>
<li class="serp-item" data-type="ads"><a class="OrganicTitle-Link" href="https://reklam.com/x"><span>Reklam</span></a></li>
<li class="serp-item"><a class="OrganicTitle-Link" href="/r?u=https%3A%2F%2Fornek.com%2Fyonlendirme&x=1"><span>İkinci Başlık</span></a><div>İkinci açıklama.</div></li>
</ul></body></html>"#;
        let r = parse_yandex(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Birinci Başlık");
        assert_eq!(r[0].1, "https://example.com/sayfa");
        assert_eq!(r[1].0, "İkinci Başlık");
        assert_eq!(r[1].1, "https://ornek.com/yonlendirme");
        // Captcha / boş sessiz döner.
        assert!(parse_yandex("<html><body>sonuç yok</body></html>").is_empty());
        assert!(parse_yandex("<html><body>showcaptcha lazım</body></html>").is_empty());
    }

    #[test]
    fn qwant_parse() {
        // mainline içinde web grubu alınır, images atlanır (alan adı desc!).
        let body = r#"{"status":"success","data":{"result":{"items":{"mainline":[
            {"type":"web","items":[{"title":"Başlık 1","url":"https://example.com/1","desc":"Açıklama 1"},{"title":"Başlık 2","url":"https://ornek.com/2","desc":"Açıklama 2"}]},
            {"type":"images","items":[{"title":"Görsel","url":"https://example.com/g.jpg","desc":"g"}]}
        ]}}}}"#;
        let r = parse_qwant(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Başlık 1");
        assert_eq!(r[0].1, "https://example.com/1");
        assert_eq!(r[0].2, "Açıklama 1");
        // Hata / captcha sessiz döner.
        assert!(parse_qwant(r#"{"status":"error","error_code":123}"#).is_empty());
        assert!(parse_qwant("captcha burada").is_empty());
        assert!(parse_qwant("bu json değil").is_empty());
    }

    #[test]
    fn reddit_parse() {
        // Self-post permalinkten, dış link aynen alınır.
        let body = r#"{"kind":"Listing","data":{"children":[
            {"kind":"t3","data":{"title":"Self Gönderi","selftext":"Kendi metnim burada.","permalink":"/r/test/comments/abc/self/","url":"https://www.reddit.com/r/test/comments/abc/self/","subreddit":"test","is_self":true}},
            {"kind":"t3","data":{"title":"Dış Bağlantı","selftext":"","permalink":"/r/test/comments/def/dis/","url":"https://example.com/dis-sayfa","subreddit":"test","is_self":false}}
        ]}}"#;
        let r = parse_reddit(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Self Gönderi");
        assert_eq!(r[0].1, "https://www.reddit.com/r/test/comments/abc/self/");
        assert!(r[0].2.contains("Kendi metnim"));
        assert_eq!(r[1].1, "https://example.com/dis-sayfa");
        // Boş / bozuk haller.
        assert!(parse_reddit(r#"{"kind":"Listing","data":{"children":[]}}"#).is_empty());
        assert!(parse_reddit("bu json değil").is_empty());
        assert!(parse_reddit(r#"{"kind":"Listing"}"#).is_empty());
    }

    #[test]
    fn wikiara_parse() {
        // Fulltext snippet vurgulu HTML içerir, curid bağlantısı kurulur.
        let body = r#"{"query":{"search":[
            {"title":"Ankara","pageid":123,"snippet":"<span class=\"searchmatch\">Ankara</span> Türkiye'nin başkentidir."},
            {"title":"İstanbul","pageid":456,"snippet":"Tarihi <span class=\"searchmatch\">şehir</span> metni."}
        ]}}"#;
        let r = parse_wikiara(body, "tr");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Ankara");
        assert_eq!(r[0].1, "https://tr.wikipedia.org/?curid=123");
        assert!(r[0].2.contains("Ankara"));
        assert!(!r[0].2.contains("<span"));
        assert_eq!(r[1].1, "https://tr.wikipedia.org/?curid=456");
        // Boş / bozuk haller.
        assert!(parse_wikiara(r#"{"query":{"search":[]}}"#, "tr").is_empty());
        assert!(parse_wikiara("bu json değil", "tr").is_empty());
        assert!(parse_wikiara(r#"{"query":{}}"#, "en").is_empty());
    }

    #[test]
    fn deezer_parse() {
        // Başlık sanatçıyla birleşir.
        let body = r#"{"data":[
            {"title":"Şarkı Bir","link":"https://www.deezer.com/track/111","type":"track","artist":{"name":"Sanatçı A"}},
            {"title":"Şarkı İki","link":"https://www.deezer.com/track/222","type":"track","artist":{"name":"Sanatçı B"}}
        ],"total":2}"#;
        let r = parse_deezer(body);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Şarkı Bir – Sanatçı A");
        assert_eq!(r[0].1, "https://www.deezer.com/track/111");
        assert!(r[0].2.contains("Sanatçı A"));
        // Boş / bozuk haller.
        assert!(parse_deezer(r#"{"data":[]}"#).is_empty());
        assert!(parse_deezer("bu json değil").is_empty());
        assert!(parse_deezer(r#"{}"#).is_empty());
    }

    #[test]
    fn nominatim_parse() {
        // Yer adı + enlem/boylam, url OSM aramasına kurulur.
        let body = r#"[
            {"display_name":"Ankara, Türkiye","lat":"39.9334","lon":"32.8597"},
            {"display_name":"İstanbul, Türkiye","lat":"41.0082","lon":"28.9784"}
        ]"#;
        let r = parse_nominatim(body, "Ankara");
        assert_eq!(r.len(), 2);
        assert!(r[0].0.contains("Ankara"));
        assert!(r[0].1.contains("openstreetmap.org/search?query="));
        assert_eq!(r[0].2, "39.9334, 32.8597");
        // Boş / bozuk haller.
        assert!(parse_nominatim("[]", "Ankara").is_empty());
        assert!(parse_nominatim("bu json değil", "Ankara").is_empty());
        assert!(parse_nominatim(r#"{}"#, "Ankara").is_empty());
    }

    #[test]
    #[ignore]
    fn deezer_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_deezer("madonna", &mut o, &mut s);
        assert!(!o.is_empty(), "Deezer sonuç dönmedi");
    }

    #[test]
    #[ignore]
    fn wikifull_canli() {
        let mut o = Vec::new();
        let mut s = Vec::new();
        src_wikiara("Ankara", &mut o, &mut s);
        assert!(!o.is_empty(), "WikiAra sonuç dönmedi");
    }
}

/// Sorgu terimlerinin başlık+a açıklamada bulunma oranı (derin tohum kapısı).
fn overlap(query: &str, title: &str, snippet: &str) -> f64 {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() > 2)
        .map(|s| s.to_string())
        .collect();
    if terms.is_empty() {
        return 0.0;
    }
    let hay = format!("{} {}", title.to_lowercase(), snippet.to_lowercase());
    // tam sorgu geçiyorsa direkt 1.0
    if hay.contains(&query.to_lowercase()) {
        return 1.0;
    }
    terms.iter().filter(|t| hay.contains(t.as_str())).count() as f64 / terms.len() as f64
}
