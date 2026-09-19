//! Nöral Web — araştırma rank motoru.
//!
//! LLM değil, next-token yok. Üç gerçek sinyalin birleşimi:
//! 1) BM25-lite (terim frekansı, başlık 2x)
//! 2) Kosinüs benzerliği (TF vektörleri — anlamsal yakınlık sinyali)
//! 3) Kaynak otoritesi + MMR çeşitlilik cezası
//! Dışarıdan gelecek büyük araştırma projesi `RankEngine` trait'ine takılır.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::neural::{self, NetSpec, Trace};

/// Ham aday sonuç (fetch.rs canlı doldurur).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Kaynak etiketi: "ddg", "github", "wikipedia", "derin" ...
    pub source: String,
    /// 0 = arama motoru halkası, 1 = sayfa-içi derin halka
    #[serde(default)]
    pub depth: u8,
    /// Çekilen sayfa metni (derin modda dolar, en fazla ~1500 karakter)
    #[serde(default)]
    pub page: String,
}

/// Her sonucun skor dökümü — UI bunu şeffaf gösterir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreDetail {
    pub bm25: f64,
    pub cosine: f64,
    pub authority: f64,
    pub neural: f64,
    pub classic_norm: f64,
    pub mmr_penalty: f64,
}

/// Skorlanmış sonuç. `hidden` + `feats` sağ paneldeki ağ animasyonunu besler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ranked {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
    pub score: f64,
    pub detail: ScoreDetail,
    pub feats: [f32; neural::N_IN],
    pub hidden: [f32; neural::H2],
}

/// Çalışma raporu — UI'daki "motor" panelini besler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub query: String,
    pub candidates: usize,
    pub sources: Vec<String>,
    pub elapsed_ms: u128,
    pub engine: String,
    pub net: NetSpec,
    pub model: neural::ModelInfo,
    pub depth: u8,
    /// Sonuç vermeyen kaynaklar (UI'da gri).
    #[serde(default)]
    pub silent: Vec<String>,
    /// Bu aramanın Apinex maliyeti (USD). Önbellekte 0.
    #[serde(default)]
    pub cost_usd: f64,
    /// Önbellekten mi geldi?
    #[serde(default)]
    pub cached: bool,
    pub results: Vec<Ranked>,
}

/// Dış araştırma projesi buraya takılır.
pub trait RankEngine {
    fn score(&self, query_terms: &[String], c: &Candidate) -> (f64, f64);
}

/// Varsayılan motor: BM25-lite + kosinüs, otorite çarpanı ayrı uygulanır.
pub struct NeuralRank;

impl NeuralRank {
    pub fn authority(source: &str) -> f64 {
        match source {
            "gecmis" => 1.4,
            "wikipedia" => 1.25,
            "ddg-özet" => 1.2,
            "wikidata" => 1.2,
            "dbpedia" => 1.2,
            "github" => 1.15,
            "brave" => 1.15,
            "stack" => 1.15,
            "apinex" => 1.2,
            "apinex-derin" => 1.25,
            "exa" => 1.2,
            "tavily" => 1.2,
            "serper" => 1.2,
            "langsearch" => 1.2,
            "arxiv" => 1.1,
            "akademik" => 1.1,
            "hackernews" => 1.1,
            "ddg-web" => 1.05,
            "ddg" => 1.0,
            "bing" => 1.0,
            "npm" => 1.0,
            "crates" => 1.0,
            "mastodon" => 1.0,
            "bluesky" => 1.0,
            "twitter" => 1.05,
            "wiby" => 1.0,
            "cc" => 0.95,
            "derin" => 0.9,
            _ => 0.85,
        }
    }

    fn term_hits(query_terms: &[String], text: &str) -> f64 {
        let t = text.to_lowercase();
        query_terms
            .iter()
            .map(|q| {
                let hits = t.matches(q.as_str()).count() as f64;
                hits / (hits + 1.2)
            })
            .sum()
    }

    fn tf(text: &str) -> HashMap<String, f64> {        let mut m = HashMap::new();
        for tok in tokenize(text) {
            *m.entry(tok).or_insert(0.0) += 1.0;
        }
        m
    }

    pub fn cosine(query_terms: &[String], text: &str) -> f64 {
        let doc = Self::tf(text);
        if doc.is_empty() || query_terms.is_empty() {
            return 0.0;
        }
        let mut qtf: HashMap<&String, f64> = HashMap::new();
        for t in query_terms {
            *qtf.entry(t).or_insert(0.0) += 1.0;
        }
        let mut dot = 0.0;
        let mut qn = 0.0;
        let mut dn = 0.0;
        for v in qtf.values() {
            qn += v * v;
        }
        for v in doc.values() {
            dn += v * v;
        }
        for (t, qv) in &qtf {
            if let Some(dv) = doc.get(*t) {
                dot += qv * dv;
            }
        }
        if qn == 0.0 || dn == 0.0 {
            0.0
        } else {
            dot / (qn.sqrt() * dn.sqrt())
        }
    }
}

impl RankEngine for NeuralRank {
    fn score(&self, query_terms: &[String], c: &Candidate) -> (f64, f64) {
        let title = Self::term_hits(query_terms, &c.title) * 2.0;
        let body = Self::term_hits(query_terms, &c.snippet);
        let bm25 = title + body;
        let combined = format!("{} {}", c.title, c.snippet);
        let cos = Self::cosine(query_terms, &combined);
        (bm25, cos)
    }
}

/// Türkçe-uyumlu basit tokenizer: küçük harf (I→ı dahil), alfanümerik dışı ayraç.
pub fn tokenize(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|s| s.chars().count() > 2)
        .map(|s| s.to_string())
        .collect()
}

/// Normalize: küçük harf, ayraçlar tek boşluk ("Berkcan-Özbalci" → "berkcan özbalci").
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bitişik: sadece alfanümerik küçük ("Berkcan Özbalci" → "berkcanozbalci").
pub fn squished(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Skorla + nöral ağdan geçir + sırala + MMR çeşitlendir.
/// final = 0.5 * nöral + 0.5 * normalize-klasik (ikisi de 0..1).
pub fn rank<E: RankEngine>(
    engine: &E,
    net: &NetSpec,
    query: &str,
    cands: Vec<Candidate>,
) -> Vec<Ranked> {
    let terms = tokenize(query);

    // 1. tur: klasik ham skorlar (bm25 normalizasyonu için max gerekir)
    let mut raws: Vec<(Candidate, f64, f64, f64)> = Vec::with_capacity(cands.len());
    let mut max_bm25 = 0.0f64;
    for c in cands {
        let auth = NeuralRank::authority(&c.source);
        let (bm25, cos) = if terms.is_empty() {
            (0.0, 0.0)
        } else {
            engine.score(&terms, &c)
        };
        if bm25 > max_bm25 {
            max_bm25 = bm25;
        }
        raws.push((c, bm25, cos, auth));
    }

    // 2. tur: nöral iz + birleşim (12 öznitelik)
    let qnorm = normalize(query);
    let qsq = squished(query);
    let mut scored: Vec<Ranked> = raws
        .into_iter()
        .map(|(c, bm25, cos, auth)| {
            let (title_cov, snip_cov) = if terms.is_empty() {
                (0.0, 0.0)
            } else {
                let t = c.title.to_lowercase();
                let s = c.snippet.to_lowercase();
                (
                    terms.iter().filter(|q| t.contains(q.as_str())).count() as f64
                        / terms.len() as f64,
                    terms.iter().filter(|q| s.contains(q.as_str())).count() as f64
                        / terms.len() as f64,
                )
            };
            let total_len = (c.title.len() + c.snippet.len()) as f64;
            let concise = 1.0 - (total_len.min(2000.0) / 2000.0);
            // Wiki çöp filtresi: başlıkta sorgudan iz yoksa otorite kırpılır.
            let mut auth = auth;
            if title_cov == 0.0 && c.source == "wikipedia" {
                auth *= 0.6;
            }
            let page_cos = if c.page.is_empty() || terms.is_empty() {
                0.0
            } else {
                NeuralRank::cosine(&terms, &c.page)
            };
            // İSİM AYIRT EDİCİLER (Berkcan Özbalci vs Berkcan Ozan dersi):
            let title_norm = normalize(&c.title);
            let exact = if !qnorm.is_empty()
                && (title_norm.contains(&qnorm)
                    || squished(&c.title).contains(&qsq)
                    || squished(&c.url).contains(&qsq))
            {
                1.0
            } else {
                0.0
            };
            let surname = match terms.last() {
                Some(last) if title_norm.contains(last.as_str()) => 1.0,
                _ => 0.0,
            };
            let all_terms = if terms.is_empty() {
                0.0
            } else {
                let hay = format!("{} {}", c.title.to_lowercase(), c.snippet.to_lowercase());
                terms.iter().filter(|q| hay.contains(q.as_str())).count() as f64
                    / terms.len() as f64
            };
            let handle = if qsq.chars().count() >= 4
                && (squished(&c.title).contains(&qsq) || squished(&c.url).contains(&qsq))
            {
                1.0
            } else {
                0.0
            };
            let feats = [
                (if max_bm25 > 0.0 { bm25 / max_bm25 } else { 0.0 }) as f32,
                cos as f32,
                (auth / 1.4) as f32,
                title_cov as f32,
                concise as f32,
                snip_cov as f32,
                page_cos as f32,
                (if c.depth == 0 { 1.0 } else { 0.4 }) as f32,
                exact as f32,
                surname as f32,
                all_terms as f32,
                handle as f32,
            ];
            let trace: Trace = neural::forward(net, feats);
            let classic_raw = (bm25 * 0.75 + cos * 4.0) * auth;
            Ranked {
                title: c.title,
                url: c.url,
                snippet: c.snippet,
                source: c.source,
                score: 0.0, // 3. turda yazılır
                detail: ScoreDetail {
                    bm25: r2(bm25),
                    cosine: r2(cos),
                    authority: auth,
                    neural: r2(trace.output as f64),
                    classic_norm: r2(classic_raw),
                    mmr_penalty: 1.0,
                },
                feats: trace.feats,
                hidden: trace.hidden,
            }
        })
        .collect();

    // Klasik ham skoru 0..1'e çek (bu listedeki max'a göre), sonra nöral ile harmanla.
    let max_raw = scored
        .iter()
        .map(|r| r.detail.classic_norm)
        .fold(0.0f64, f64::max)
        .max(1e-9);
    for r in &mut scored {
        let cn = r.detail.classic_norm / max_raw;
        r.detail.classic_norm = r2(cn);
        r.score = r2(0.5 * r.detail.neural + 0.5 * cn);
    }

    // NaN/Inf girse çökmesin — yön aynı (azalan).
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<Ranked> = Vec::with_capacity(scored.len());
    for mut r in scored {
        let same_host = out
            .iter()
            .rev()
            .take(2)
            .filter(|p| host_of(&p.url) == host_of(&r.url))
            .count() as f64;
        if same_host > 0.0 {
            let pen = 0.75_f64.powf(same_host);
            r.detail.mmr_penalty = r2(pen);
            r.score = r2(r.score * pen);
        }
        out.push(r);
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn host_of(url: &str) -> &str {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.split('/').next().unwrap_or(u)
}

/// M0 demosu (offline yedek): canlı fetch boş dönerse kullanılır.
pub fn mock_candidates(query: &str) -> Vec<Candidate> {
    vec![Candidate {
        title: format!("{} — çevrimdışı önizleme", query),
        url: "https://example.com".into(),
        snippet: "Bağlantı kurulamadı; bu bir yer tutucu sonuçtur.".into(),
        source: "yerli".into(),
        depth: 0,
        page: String::new(),
    }]
}

#[cfg(test)]
pub fn cand(title: &str, url: &str, snippet: &str, source: &str) -> Candidate {
    Candidate {
        title: title.into(),
        url: url.into(),
        snippet: snippet.into(),
        source: source.into(),
        depth: 0,
        page: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baslik_eslesmesi_one_cikar() {
        let r = rank(
            &NeuralRank,
            &crate::neural::net(),
            "paslanmaz celik",
            vec![
                cand("alakasız yazı", "https://a.com/1", "bugün hava güzel", "bing"),
                cand(
                    "paslanmaz çelik rehberi",
                    "https://b.com/2",
                    "paslanmaz çelik hakkında detay",
                    "bing",
                ),
            ],
        );
        assert!(r[0].url.contains("b.com"));
        assert!(r[0].detail.bm25 > 0.0);
    }

    #[test]
    fn ayni_host_cezalandirilir() {
        let r = rank(
            &NeuralRank,
            &crate::neural::net(),
            "test",
            vec![
                cand("test bir", "https://a.com/1", "test", "bing"),
                cand("test iki", "https://a.com/2", "test", "bing"),
                cand("test üç", "https://b.com/3", "test", "bing"),
            ],
        );
        assert!(r[0].url.contains("b.com"));
        assert!(r[2].url.contains("a.com/2"));
        assert!(r[2].detail.mmr_penalty < 1.0);
    }

    #[test]
    fn kosinus_benzerligi_sifirdan_buyuk() {
        let (_, cos) = NeuralRank.score(
            &tokenize("kedi maması"),
            &cand("en iyi kedi maması", "https://x.com", "kedi maması seçimi", "ddg"),
        );
        assert!(cos > 0.3);
    }

    #[test]
    fn berkcan_ayrimi() {
        // "Berkcan Özbalci" sorgusunda tam isim, benzer ismi (Berkcan Ozan) yenmeli —
        // hem nöral hem klasik modda. Eğitilmiş ağla uçtan uca.
        let mut n = crate::neural::net();
        crate::neural::train_pairwise(
            &mut n,
            &crate::neural::BASE_PAIRS,
            crate::neural::BASE_EPOCHS,
            crate::neural::BASE_LR,
        );
        let r = rank(
            &NeuralRank,
            &n,
            "Berkcan Özbalci",
            vec![
                cand(
                    "Berkcan Ozan",
                    "https://x.com/berkcanozan",
                    "Berkcan Ozan profili",
                    "twitter",
                ),
                cand(
                    "Berkcan Özbalci",
                    "https://github.com/berkcanozbalci",
                    "Berkcan Özbalci projeleri",
                    "github",
                ),
            ],
        );
        assert!(
            r[0].url.contains("berkcanozbalci"),
            "yanlış birinci: {}",
            r[0].url
        );
        assert!(
            r[0].detail.neural > r[1].detail.neural,
            "nöral skor ters: {} vs {}",
            r[0].detail.neural,
            r[1].detail.neural
        );
    }
}
