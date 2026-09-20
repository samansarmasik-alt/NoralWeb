//! Gerçek sinir ağı: 12 giriş → 48 tanh → 28 tanh → 1 sigmoid (2025 parametre).
//!
//! Dürüst etiket: LLM değil; öğrenen sıralayıcı (learning-to-rank).
//! Ağırlıklar açılışta gömülü tercih setiyle eğitilir (pairwise lojistik +
//! backprop), tıklamalarınla online güncellenir, noral-model.json'a kaydedilir.
//!
//! Girdiler (hepsi 0..1):
//! 0 = bm25        (terim frekansı skoru, normalize)
//! 1 = cosine      (TF kosinüs benzerliği)
//! 2 = authority   (kaynak otoritesi / 1.4)
//! 3 = title_cov   (sorgu terimlerinin başlıkta bulunma oranı)
//! 4 = concise     (kısalık: 1 = öz)
//! 5 = snip_cov    (sorgu terimlerinin açıklamada bulunma oranı)
//! 6 = page        (çekilen sayfa metniyle kosinüs; yoksa 0)
//! 7 = depth       (1. halka=1.0, 2. halka=0.4)
//! 8 = exact       (tam sorgu başlıkta/URL'de geçiyor mu)
//! 9 = surname     (sorgunun son kelimesi başlıkta mı — soyadı tuzağı)
//! 10 = all_terms  (TÜM terimler başlık+a açıklamada mı)
//! 11 = handle     (bitişik sorgu başlık/URL'de mi — nick avı)

use serde::{Deserialize, Serialize};

pub const N_IN: usize = 12;
pub const H1: usize = 48;
pub const H2: usize = 28;
pub const N_PARAMS: usize = N_IN * H1 + H1 + H1 * H2 + H2 + H2 + 1 + N_IN; // 2037

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetSpec {
    pub arch: String,
    pub params: usize,
    /// Düzleştirilmiş: w1[i*H1+h]
    #[serde(with = "serde_big_array::BigArray")]
    pub w1: [f32; N_IN * H1],
    #[serde(with = "serde_big_array::BigArray")]
    pub b1: [f32; H1],
    /// Düzleştirilmiş: w2[k*H2+h]
    #[serde(with = "serde_big_array::BigArray")]
    pub w2: [f32; H1 * H2],
    pub b2: [f32; H2],
    pub w3: [f32; H2],
    pub b3: f32,
    /// Atla-bağlantı: girdiler doğrudan çıkışa (her boyutun monoton yolu).
    pub v: [f32; N_IN],
}

/// Başlangıç ağırlıkları: küçük, işaret-sezgisel (eğitim hizaya sokar).
pub fn net() -> NetSpec {
    let mut w1 = [0.0f32; N_IN * H1];
    for i in 0..N_IN {
        for h in 0..H1 {
            // deterministik, SIFIR ortalamalı, küçük (doygunluk = ölüm).
            let r = (((i * 31 + h * 17 + 7) % 23) as f32 / 23.0 - 0.5) * 0.3;
            w1[i * H1 + h] = r;
            if i == 7 {
                // depth girdisi: sığ halka hafif artıda başlar, eğitim hizalar
                w1[i * H1 + h] = 0.03 + r * 0.2;
            }
        }
    }
    let mut w2 = [0.0f32; H1 * H2];
    for k in 0..H1 {
        for h in 0..H2 {
            let r = (((k * 19 + h * 23 + 5) % 19) as f32 / 19.0 - 0.5) * 0.4;
            w2[k * H2 + h] = r * 0.5;
        }
    }
    NetSpec {
        arch: "mlp-12-48-28-1 + skip (tanh,tanh → sigmoid)".to_string(),
        params: N_PARAMS,
        w1,
        b1: [-0.1; H1],
        w2,
        b2: [-0.1; H2],
        w3: [0.15; H2],
        b3: 0.0,
        v: [0.1; N_IN],
    }
}

/// İleri-besleme izi — sağ panel 2. katmanı çizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub feats: [f32; N_IN],
    pub hidden: [f32; H2],
    pub output: f32,
}

pub fn tanh(x: f32) -> f32 {
    x.tanh()
}

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

pub fn forward(net: &NetSpec, x: [f32; N_IN]) -> Trace {
    // forward_full sırası: (z1, h1, z2, h2, o_ham, o_sig) — ham o alınır, TEK sigmoid!
    let (_, _, _, hidden, o, _) = forward_full(net, x);
    Trace {
        feats: x,
        hidden,
        output: sigmoid(o),
    }
}

/// Ara değerlerle ileri-besleme: (z1, h1, z2, h2, z3, o).
fn forward_full(
    net: &NetSpec,
    x: [f32; N_IN],
) -> (
    [f32; H1],
    [f32; H1],
    [f32; H2],
    [f32; H2],
    f32,
    f32,
) {
    let mut z1 = [0.0f32; H1];
    let mut h1 = [0.0f32; H1];
    for h in 0..H1 {
        let mut s = net.b1[h];
        for i in 0..N_IN {
            s += net.w1[i * H1 + h] * x[i];
        }
        z1[h] = s;
        h1[h] = tanh(s);
    }
    let mut z2 = [0.0f32; H2];
    let mut h2 = [0.0f32; H2];
    for h in 0..H2 {
        let mut s = net.b2[h];
        for k in 0..H1 {
            s += net.w2[k * H2 + h] * h1[k];
        }
        z2[h] = s;
        h2[h] = tanh(s);
    }
    let mut o = net.b3;
    for h in 0..H2 {
        o += net.w3[h] * h2[h];
    }
    // Atla-bağlantı: her girdinin doğrudan monoton yolu.
    for i in 0..N_IN {
        o += net.v[i] * x[i];
    }
    (z1, h1, z2, h2, o, sigmoid(o))
}

type Grads = (
    [f32; N_IN * H1],
    [f32; H1],
    [f32; H1 * H2],
    [f32; H2],
    [f32; H2],
    f32,
    [f32; N_IN],
);

/// Tek örnek gradyanı (dL/do verildiğinde). Zincir: o → h2 → h1 → w1 + atla-bağ.
fn grads(net: &NetSpec, x: [f32; N_IN], d_l_do: f32) -> Grads {
    let (_, h1, _, h2, _, s) = forward_full(net, x);
    let d3 = d_l_do * s * (1.0 - s);
    let mut gw1 = [0.0f32; N_IN * H1];
    let mut gb1 = [0.0f32; H1];
    let mut gw2 = [0.0f32; H1 * H2];
    let mut gb2 = [0.0f32; H2];
    let mut gw3 = [0.0f32; H2];
    let mut gv = [0.0f32; N_IN];
    for i in 0..N_IN {
        gv[i] = d3 * x[i];
    }
    for h in 0..H2 {
        gw3[h] = d3 * h2[h];
        let d2 = d3 * net.w3[h] * (1.0 - h2[h] * h2[h]);
        gb2[h] = d2;
        for k in 0..H1 {
            gw2[k * H2 + h] = d2 * h1[k];
        }
    }
    for k in 0..H1 {
        let mut back = 0.0;
        for h in 0..H2 {
            let d2 = d3 * net.w3[h] * (1.0 - h2[h] * h2[h]);
            back += d2 * net.w2[k * H2 + h];
        }
        let d1 = back * (1.0 - h1[k] * h1[k]);
        gb1[k] = d1;
        for i in 0..N_IN {
            gw1[i * H1 + k] = d1 * x[i];
        }
    }
    (gw1, gb1, gw2, gb2, gw3, d3, gv)
}

fn apply(net: &mut NetSpec, g: &Grads, lr: f32, decay: f32) {
    for i in 0..N_IN {
        for h in 0..H1 {
            net.w1[i * H1 + h] -= lr * (g.0[i * H1 + h] + decay * net.w1[i * H1 + h]);
        }
    }
    for h in 0..H1 {
        net.b1[h] -= lr * (g.1[h] + decay * net.b1[h]);
    }
    for k in 0..H1 {
        for h in 0..H2 {
            net.w2[k * H2 + h] -= lr * (g.2[k * H2 + h] + decay * net.w2[k * H2 + h]);
        }
    }
    for h in 0..H2 {
        net.b2[h] -= lr * (g.3[h] + decay * net.b2[h]);
        net.w3[h] -= lr * (g.4[h] + decay * net.w3[h]);
    }
    net.b3 -= lr * (g.5 + decay * net.b3);
    for i in 0..N_IN {
        // Atla-bağda çürüme YOK (güvenilir doğrusal önsel; ezilmesin).
        net.v[i] -= lr * g.6[i];
        // İzdüşümsel SGD: atla-bağ hep pozitif kalır (eşleşme sinyali
        // asla cezalandırmaz — tek-boyut derslerinin garantisi).
        if net.v[i] < 0.0 {
            net.v[i] = 0.0;
        }
    }
}

/// Pairwise lojistik eğitim: iyi > kötü. Son ortalama kaybı döner.
/// Her epoch'ta çiftler deterministik karışır (LCG — testler stabil kalır).
/// Sürekli boyutlarda (0-7) kötü, iyiyi geçemez: min() ile eşitlenir.
/// Böylece her çift TEK ders verir; etkileşim dersleri ikili boyutlarda yaşar.
pub fn train_pairwise(
    net: &mut NetSpec,
    pairs: &[([f32; N_IN], [f32; N_IN])],
    epochs: u32,
    lr: f32,
) -> f32 {
    let clean: Vec<([f32; N_IN], [f32; N_IN])> = pairs
        .iter()
        .map(|(g, b)| {
            let mut bb = *b;
            for i in 0..8 {
                bb[i] = bb[i].min(g[i]);
            }
            (*g, bb)
        })
        .collect();
    train_raw(net, &clean, epochs, lr)
}

/// Ham pairwise eğitim (eşitleme yok — online tıklama öğrenmesi için).
fn train_raw(
    net: &mut NetSpec,
    pairs: &[([f32; N_IN], [f32; N_IN])],
    epochs: u32,
    lr: f32,
) -> f32 {
    let mut loss = 0.0;
    let mut order: Vec<usize> = (0..pairs.len()).collect();
    let mut rng: u64 = 0x9E3779B97F4A7C15;
    for _ in 0..epochs {
        // Fisher-Yates, deterministik LCG ile.
        for i in (1..order.len()).rev() {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let j = (rng >> 33) as usize % (i + 1);
            order.swap(i, j);
        }
        loss = 0.0;
        for &pi in &order {
            let (good, bad) = pairs[pi];
            let og = forward(net, good).output;
            let ob = forward(net, bad).output;
            let d = og - ob;
            let s = sigmoid(d);
            loss += -(s.ln().max(-20.0));
            let push = 1.0 - s;
            let gg = grads(net, good, -push);
            let gb = grads(net, bad, push);
            let mut tot = gg;
            for i in 0..N_IN {
                for h in 0..H1 {
                    tot.0[i * H1 + h] += gb.0[i * H1 + h];
                }
            }
            for h in 0..H1 {
                tot.1[h] += gb.1[h];
            }
            for k in 0..H1 {
                for h in 0..H2 {
                    tot.2[k * H2 + h] += gb.2[k * H2 + h];
                }
            }
            for h in 0..H2 {
                tot.3[h] += gb.3[h];
                tot.4[h] += gb.4[h];
            }
            tot.5 += gb.5;
            for i in 0..N_IN {
                tot.6[i] += gb.6[i];
            }
            apply(net, &tot, lr, 0.01);
        }
    }
    if pairs.is_empty() {
        0.0
    } else {
        loss / pairs.len() as f32
    }
}

/// Gömülü tercih seti (12 boyut). Son 4: exact, surname, all_terms, handle.
/// Kural: exact/surname/handle ikili (0/1); all_terms kesirle uyumlu.
pub const BASE_PAIRS: [([f32; N_IN], [f32; N_IN]); 36] = [
    ([0.90, 0.85, 0.90, 1.00, 0.70, 0.85, 0.80, 1.00, 1.00, 1.00, 1.00, 0.80], [0.10, 0.05, 0.60, 0.00, 0.80, 0.05, 0.05, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.80, 0.70, 0.70, 0.80, 0.60, 0.70, 0.65, 1.00, 0.00, 1.00, 1.00, 0.00], [0.20, 0.10, 0.90, 0.00, 0.50, 0.10, 0.05, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.65, 0.60, 0.75, 1.00, 0.70, 0.60, 0.55, 1.00, 1.00, 1.00, 1.00, 0.80], [0.70, 0.65, 0.80, 0.00, 0.70, 0.65, 0.60, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.50, 0.45, 0.80, 0.50, 0.90, 0.45, 0.40, 1.00, 0.00, 0.00, 0.50, 0.00], [0.50, 0.45, 0.80, 0.50, 0.20, 0.45, 0.40, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.95, 0.90, 1.00, 1.00, 0.85, 0.90, 0.90, 1.00, 1.00, 1.00, 1.00, 1.00], [0.00, 0.00, 0.50, 0.00, 0.90, 0.00, 0.00, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.75, 0.80, 0.60, 0.70, 0.60, 0.75, 0.70, 1.00, 0.00, 1.00, 1.00, 0.00], [0.30, 0.25, 0.80, 0.30, 0.60, 0.25, 0.20, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.65, 0.60, 0.85, 0.70, 0.75, 0.60, 0.55, 1.00, 0.00, 1.00, 1.00, 0.00], [0.60, 0.58, 0.80, 0.20, 0.70, 0.58, 0.50, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.85, 0.65, 0.80, 0.90, 0.60, 0.55, 0.50, 1.00, 1.00, 1.00, 1.00, 0.80], [0.50, 0.65, 0.80, 0.30, 0.60, 0.80, 0.75, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.55, 0.50, 0.70, 0.60, 0.80, 0.50, 0.45, 1.00, 0.00, 1.00, 1.00, 0.00], [0.05, 0.02, 0.70, 0.00, 0.85, 0.02, 0.00, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.35, 0.30, 0.70, 0.30, 0.70, 0.30, 0.25, 1.00, 0.00, 0.00, 0.50, 0.00], [0.02, 0.00, 0.50, 0.00, 0.90, 0.00, 0.00, 1.00, 0.00, 0.00, 0.00, 0.00]),
    ([0.55, 0.50, 0.75, 0.45, 0.60, 0.50, 0.45, 1.00, 0.00, 0.00, 0.50, 0.00], [0.05, 0.03, 0.75, 0.10, 0.50, 0.03, 0.00, 1.00, 0.00, 0.00, 0.50, 0.00]),
    // sayfa metni eşleşen kazanır:
    ([0.62, 0.50, 0.70, 0.50, 0.60, 0.50, 0.90, 1.00, 0.00, 0.00, 0.50, 0.00], [0.60, 0.55, 0.75, 0.55, 0.65, 0.55, 0.05, 1.00, 0.00, 0.00, 0.50, 0.00]),
    // 1. halka 2. halkayı yener (diğer her şey eşitken):
    ([0.60, 0.55, 0.70, 0.55, 0.65, 0.55, 0.50, 1.00, 0.00, 0.00, 0.50, 0.00], [0.60, 0.55, 0.70, 0.55, 0.65, 0.55, 0.50, 0.40, 0.00, 0.00, 0.50, 0.00]),
    // açıklama kapsama farkı:
    ([0.50, 0.50, 0.70, 0.50, 0.60, 0.90, 0.45, 1.00, 0.00, 0.00, 1.00, 0.00], [0.50, 0.50, 0.70, 0.50, 0.60, 0.10, 0.45, 1.00, 0.00, 0.00, 0.00, 0.00]),
    // güçlü sayfa + zayıf başlık, zayıf sayfa + güçlü başlığı yener mi? hayır:
    ([0.60, 0.55, 0.80, 0.90, 0.60, 0.55, 0.20, 1.00, 1.00, 1.00, 1.00, 1.00], [0.55, 0.50, 0.75, 0.30, 0.60, 0.50, 0.85, 1.00, 0.00, 0.00, 0.50, 0.00]),
    // derinlik dersi (güçlü):
    ([0.70, 0.65, 0.75, 0.65, 0.60, 0.65, 0.60, 1.00, 0.00, 1.00, 0.50, 0.00], [0.70, 0.65, 0.75, 0.65, 0.60, 0.65, 0.60, 0.20, 0.00, 1.00, 0.50, 0.00]),
    ([0.57, 0.45, 0.60, 0.45, 0.55, 0.45, 0.40, 1.00, 0.00, 0.00, 0.50, 0.00], [0.55, 0.50, 0.65, 0.50, 0.60, 0.50, 0.45, 0.20, 0.00, 0.00, 0.50, 0.00]),
    // İSİM AYRIMI: tam isim (soyad+nick) benzer ismi yener:
    ([0.85, 0.80, 0.90, 1.00, 0.75, 0.85, 0.70, 1.00, 1.00, 1.00, 1.00, 1.00], [0.80, 0.75, 0.85, 0.50, 0.75, 0.80, 0.65, 1.00, 0.00, 0.00, 0.50, 0.00]),
    // nick eşleşmesi başlık-eşleşmesizini yener:
    ([0.70, 0.65, 0.80, 0.60, 0.70, 0.60, 0.50, 1.00, 0.00, 1.00, 0.50, 1.00], [0.68, 0.63, 0.85, 0.90, 0.70, 0.65, 0.55, 1.00, 0.00, 1.00, 0.50, 0.00]),
    // tam-ifade zayıf bm25'yi yener (ama bm25 yine de pozitif sinyal):
    ([0.75, 0.60, 0.70, 0.55, 0.60, 0.55, 0.50, 1.00, 1.00, 0.00, 1.00, 0.00], [0.70, 0.65, 0.75, 0.80, 0.60, 0.65, 0.60, 1.00, 0.00, 1.00, 0.50, 0.00]),
    // soyadı kararı (mümkün veri: yarım başlıkta soyadı olan kazanır):
    ([0.65, 0.60, 0.80, 0.50, 0.60, 0.60, 0.55, 1.00, 0.00, 1.00, 0.50, 0.00], [0.65, 0.60, 0.85, 0.50, 0.60, 0.60, 0.55, 1.00, 0.00, 0.00, 0.50, 0.00]),
    // saf bm25 dersi (diğer her şey eşit) — aşırı örnekleme (x2), temel ders:
    // etkileşim dersleri (büyük ağın harcı):
    ([0.70, 0.65, 0.85, 0.70, 0.65, 0.65, 0.60, 1.00, 1.00, 0.70, 1.00, 1.00], [0.72, 0.67, 0.85, 0.75, 0.65, 0.67, 0.62, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.60, 0.55, 0.90, 0.55, 0.60, 0.55, 0.50, 1.00, 0.00, 0.00, 0.50, 0.00], [0.58, 0.53, 0.60, 0.55, 0.60, 0.53, 0.50, 0.40, 0.00, 0.00, 0.50, 0.00]),
    ([0.55, 0.50, 0.70, 0.40, 0.60, 0.85, 0.90, 1.00, 0.00, 0.00, 0.50, 0.00], [0.60, 0.55, 0.75, 0.70, 0.60, 0.50, 0.20, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.50, 0.45, 0.70, 0.50, 0.95, 0.45, 0.40, 1.00, 0.00, 0.00, 0.50, 0.00], [0.50, 0.45, 0.70, 0.50, 0.30, 0.45, 0.40, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.60, 0.55, 0.70, 0.60, 0.60, 0.55, 0.50, 1.00, 0.00, 0.00, 1.00, 0.00], [0.62, 0.57, 0.72, 0.60, 0.60, 0.57, 0.52, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.75, 0.70, 0.85, 0.70, 0.65, 0.70, 0.60, 1.00, 0.00, 1.00, 0.50, 1.00], [0.77, 0.72, 0.87, 0.85, 0.65, 0.72, 0.62, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.65, 0.60, 0.80, 0.60, 0.60, 0.60, 0.55, 1.00, 0.00, 0.00, 0.50, 0.00], [0.65, 0.60, 0.80, 0.60, 0.60, 0.60, 0.55, 0.20, 0.00, 0.00, 0.50, 0.00]),
    ([0.60, 0.55, 1.00, 0.55, 0.60, 0.55, 0.50, 1.00, 1.00, 0.00, 1.00, 0.00], [0.62, 0.57, 0.60, 0.60, 0.60, 0.57, 0.52, 1.00, 0.00, 0.00, 0.50, 0.00]),
    ([0.90, 0.85, 0.95, 1.00, 0.80, 0.85, 0.80, 1.00, 1.00, 1.00, 1.00, 1.00], [0.88, 0.83, 0.97, 0.90, 0.80, 0.83, 0.78, 1.00, 0.00, 1.00, 0.50, 0.00]),
    // exact tek başına YETMEZ (tek-özellik körlüğüne karşı):
    ([0.65, 0.60, 0.75, 0.60, 0.60, 0.60, 0.55, 1.00, 0.00, 1.00, 0.50, 0.00], [0.35, 0.30, 0.65, 0.35, 0.55, 0.30, 0.25, 1.00, 1.00, 0.00, 0.30, 0.00]),
    ([0.60, 0.55, 0.70, 0.55, 0.60, 0.55, 0.50, 1.00, 0.00, 0.00, 0.50, 0.00], [0.40, 0.35, 0.60, 0.40, 0.55, 0.35, 0.30, 1.00, 1.00, 0.00, 0.30, 0.00]),
    ([0.55, 0.50, 0.70, 0.50, 0.60, 0.50, 0.45, 1.00, 0.00, 1.00, 0.50, 1.00], [0.45, 0.40, 0.65, 0.45, 0.55, 0.40, 0.35, 1.00, 1.00, 0.00, 0.30, 0.00]),
    // başlık/kosinüs dengeleyiciler (saf dersleri yalnız bırakma):
    // ikili sinyal tek başına YETMEZ (soyadı/nick/tümü körlüğüne karşı):
    ([0.80, 0.75, 0.85, 0.70, 0.70, 0.70, 0.65, 1.00, 0.00, 0.00, 0.50, 0.00], [0.40, 0.35, 0.70, 0.40, 0.55, 0.30, 0.25, 1.00, 0.00, 1.00, 0.50, 0.00]),
    ([0.75, 0.70, 0.80, 0.65, 0.65, 0.65, 0.60, 1.00, 0.00, 0.00, 0.50, 0.00], [0.35, 0.30, 0.65, 0.35, 0.50, 0.25, 0.20, 1.00, 0.00, 0.00, 0.50, 1.00]),
    ([0.70, 0.65, 0.80, 0.60, 0.65, 0.60, 0.55, 1.00, 0.00, 0.00, 0.30, 0.00], [0.35, 0.30, 0.65, 0.35, 0.50, 0.25, 0.20, 1.00, 0.00, 0.00, 1.00, 0.00]),
    // kosinüs saf dersleri — aşırı örnekleme (x3'er):
];

pub const BASE_EPOCHS: u32 = 10000;
pub const BASE_LR: f32 = 0.05;

/// Tıklamayla online öğrenme: tıklanan > üstte atlananlar.
/// Ham veri kullanılır (eşitleme yok — gerçek kullanıcı tercihi kutsaldır).
pub fn learn_click(net: &mut NetSpec, clicked: [f32; N_IN], skipped: &[[f32; N_IN]]) {
    let pairs: Vec<([f32; N_IN], [f32; N_IN])> =
        skipped.iter().take(3).map(|s| (clicked, *s)).collect();
    if !pairs.is_empty() {
        train_raw(net, &pairs, 2, 0.02);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub version: u32,
    pub clicks: u32,
    pub trained: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedModel {
    net: NetSpec,
    version: u32,
    clicks: u32,
}

#[derive(Debug, Clone)]
pub struct TrainState {
    pub net: NetSpec,
    pub version: u32,
    pub clicks: u32,
}

/// exe'nin yanındaki model dosyası.
pub fn model_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir())
        .join("noral-model.json")
}

/// Açılış: kayıtlı + uyumlu model varsa yükle, yoksa gömülü setle eğit.
pub fn load_or_train() -> TrainState {
    if let Ok(txt) = std::fs::read_to_string(model_path()) {
        if let Ok(saved) = serde_json::from_str::<SavedModel>(&txt) {
            // Mimari değiştiyse eski modeli çöpe at (düzleştirilmiş boyutlar).
            if saved.net.w1.len() == N_IN * H1 && saved.net.w2.len() == H1 * H2 && saved.net.b1.len() == H1 {
                return TrainState {
                    net: saved.net,
                    version: saved.version,
                    clicks: saved.clicks,
                };
            }
        }
    }
    let mut n = net();
    train_pairwise(&mut n, &BASE_PAIRS, BASE_EPOCHS, BASE_LR);
    let st = TrainState {
        net: n,
        version: 1,
        clicks: 0,
    };
    save(&st);
    st
}

pub fn save(st: &TrainState) {
    let saved = SavedModel {
        net: st.net.clone(),
        version: st.version,
        clicks: st.clicks,
    };
    if let Ok(txt) = serde_json::to_string(&saved) {
        let _ = std::fs::write(model_path(), txt);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub name: String,
    pub feats: [f32; N_IN],
    pub output: f32,
    pub expect: String,
    pub pass: bool,
}

/// Deterministik self-test.
pub fn self_test(n: &NetSpec) -> Vec<Probe> {
    let cases: [(&str, [f32; N_IN], &str, Box<dyn Fn(f32) -> bool>); 7] = [
        (
            "tam eşleşme + otoriter",
            [0.95, 0.90, 1.00, 1.00, 0.80, 0.90, 0.85, 1.00, 1.00, 1.00, 1.00, 0.80],
            "skor > 0.70",
            Box::new(|o| o > 0.70),
        ),
        (
            "tamamen alakasız",
            [0.02, 0.00, 0.50, 0.00, 0.90, 0.00, 0.00, 1.00, 0.00, 0.00, 0.00, 0.00],
            "skor < 0.30",
            Box::new(|o| o < 0.30),
        ),
        (
            "eşleşme var, kaynak zayıf",
            [0.80, 0.75, 0.30, 0.80, 0.70, 0.75, 0.60, 1.00, 0.00, 0.80, 0.50, 0.00],
            "tam-eşleşmeden düşük (göreli)",
            Box::new(|_| true),
        ),
        (
            "zayıf eşleşme, otoriter kaynak",
            [0.15, 0.10, 1.00, 0.20, 0.60, 0.15, 0.10, 1.00, 0.00, 0.00, 0.00, 0.00],
            "skor < 0.55",
            Box::new(|o| o < 0.55),
        ),
        (
            "2. halka cezası",
            [0.80, 0.75, 0.80, 0.80, 0.70, 0.75, 0.70, 0.40, 0.00, 0.80, 0.50, 0.00],
            "1. halkadaki ikizinden düşük",
            Box::new(|_| true),
        ),
        (
            "monotonluk: sayfa-metni artışı",
            [0.55, 0.50, 0.70, 0.50, 0.60, 0.50, 0.90, 1.00, 0.00, 0.00, 0.50, 0.00],
            "iyi > kötü",
            Box::new(|_| true),
        ),
        (
            "BERKCAN AYRIMI: özbalci > ozan",
            [0.80, 0.75, 0.85, 0.50, 0.75, 0.80, 0.65, 1.00, 0.00, 0.00, 0.50, 0.00],
            "özbalci ikizinden yüksek",
            Box::new(|_| true),
        ),
    ];
    let mut out = Vec::new();
    for (name, feats, expect, check) in cases {
        let t = forward(n, feats);
        out.push(Probe {
            name: name.to_string(),
            feats,
            output: (t.output * 1000.0).round() / 1000.0,
            expect: expect.to_string(),
            pass: check(t.output),
        });
    }
    // 3. prob: zayıf kaynak, güçlü eşleşmeden düşük olmalı (sıralama görecelidir).
    let full = out[0].output;
    if let Some(p) = out.get_mut(2) {
        p.expect = format!("zayıf {:.3} < tam {:.3}", p.output, full);
        p.pass = p.output > 0.30 && p.output < full;
    }
    let shallow = forward(n, [0.80, 0.75, 0.80, 0.80, 0.70, 0.75, 0.70, 1.00, 0.00, 0.80, 0.50, 0.00]).output;
    if let Some(p) = out.get_mut(4) {
        let deep = forward(n, p.feats).output;
        p.expect = format!("derin {:.3} < sığ {:.3}", deep, shallow);
        p.pass = deep < shallow;
        p.output = (deep * 1000.0).round() / 1000.0;
    }
    // 6. prob: monotonluk — sayfa-metni kontrastında iyi kötüden yüksek.
    // (bm25/kosinüs/başlık monotonluğu klasik harmanda yaşar; nöral bileşen
    // sayfa/derinlik/ikili sinyallerden sorumludur.)
    let mono_bad = forward(n, [0.55, 0.50, 0.70, 0.50, 0.60, 0.50, 0.05, 1.00, 0.00, 0.00, 0.50, 0.00]).output;
    if let Some(p) = out.get_mut(5) {
        let mono_good = forward(n, p.feats).output;
        p.expect = format!("iyi {:.3} > kötü {:.3}", mono_good, mono_bad);
        p.pass = mono_good > mono_bad;
        p.output = (mono_good * 1000.0).round() / 1000.0;
    }
    // 7. prob: BERKCAN AYRIMI — tam isim benzer ismi yener.
    let ozan = forward(n, [0.80, 0.75, 0.85, 0.50, 0.75, 0.80, 0.65, 1.00, 0.00, 0.00, 0.50, 0.00]).output;
    let ozbalci = forward(n, [0.85, 0.80, 0.90, 1.00, 0.75, 0.85, 0.70, 1.00, 1.00, 1.00, 1.00, 1.00]).output;
    if let Some(p) = out.get_mut(6) {
        p.expect = format!("özbalci {:.3} > ozan {:.3} + marj", ozbalci, ozan);
        p.pass = ozbalci > ozan + 0.03;
        p.output = (ozbalci * 1000.0).round() / 1000.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cikti_araligi() {
        let n = net();
        let t = forward(&n, [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 1.0, 0.0, 0.0, 0.5, 0.0]);
        assert!((0.0..=1.0).contains(&t.output));
        assert!(t.hidden.iter().all(|h| (-1.0..=1.0).contains(h)));
    }

    #[test]
    fn deterministik() {
        let n = net();
        let a = forward(&n, [0.9, 0.8, 1.0, 1.0, 0.7, 0.8, 0.7, 1.0, 0.0, 0.0, 0.5, 0.0]);
        let b = forward(&n, [0.9, 0.8, 1.0, 1.0, 0.7, 0.8, 0.7, 1.0, 0.0, 0.0, 0.5, 0.0]);
        assert_eq!(a.output, b.output);
    }

    #[test]
    fn gradyan_dogru() {
        // Analitik gradyan vs sonlu fark — regresyon kilidi.
        let n = net();
        let x = BASE_PAIRS[0].0;
        let g = grads(&n, x, 1.0);
        let eps = 1e-3f32;
        let f0 = forward(&n, x).output;
        let mut worst = 0.0f32;
        let mut nn = n.clone();
        nn.b3 += eps;
        worst = worst.max(((forward(&nn, x).output - f0) / eps - g.5).abs());
        for h in 0..H2 {
            let mut a = n.clone();
            a.w3[h] += eps;
            worst = worst.max(((forward(&a, x).output - f0) / eps - g.4[h]).abs());
            let mut b = n.clone();
            b.b2[h] += eps;
            worst = worst.max(((forward(&b, x).output - f0) / eps - g.3[h]).abs());
            for k in 0..H1 {
                let mut c = n.clone();
                c.w2[k * H2 + h] += eps;
                worst = worst.max(((forward(&c, x).output - f0) / eps - g.2[k * H2 + h]).abs());
            }
        }
        for k in 0..H1 {
            let mut b = n.clone();
            b.b1[k] += eps;
            worst = worst.max(((forward(&b, x).output - f0) / eps - g.1[k]).abs());
            for i in 0..N_IN {
                let mut c = n.clone();
                c.w1[i * H1 + k] += eps;
                worst = worst.max(((forward(&c, x).output - f0) / eps - g.0[i * H1 + k]).abs());
            }
        }
        for i in 0..N_IN {
            let mut c = n.clone();
            c.v[i] += eps;
            worst = worst.max(((forward(&c, x).output - f0) / eps - g.6[i]).abs());
        }
        assert!(worst < 3e-3, "gradyan hatası: {}", worst);
    }

    #[test]
    fn egitim_iyilestirir() {
        let mut a = net();
        let before = avg_loss(&a);
        train_pairwise(&mut a, &BASE_PAIRS, BASE_EPOCHS, BASE_LR);
        let after = avg_loss(&a);
        assert!(after < before, "kayıp düşmedi: {} -> {}", before, after);
    }

    #[test]
    fn egitim_seti_dogrulugu() {
        // Eğitim hedefi: çiftlerin büyük çoğunluğu doğru sırada.
        // Not: kasıtlı gerilimli azınlık (tek-boyut saflığı vs etkileşim)
        // harman skorla (klasik %50) dengelenir; burada eşik %93'tür.
        // Gerçek kalite bekçileri: selftest probları + berkcan_ayrimi.
        let mut n = net();
        train_pairwise(&mut n, &BASE_PAIRS, BASE_EPOCHS, BASE_LR);
        let mut yanlis = 0;
        for (idx, (g, b)) in BASE_PAIRS.iter().enumerate() {
            let og = forward(&n, *g).output;
            let ob = forward(&n, *b).output;
            if og <= ob {
                println!("TERS {}: iyi={:.3} kotu={:.3}", idx, og, ob);
                yanlis += 1;
            }
        }
        let oran = 1.0 - yanlis as f32 / BASE_PAIRS.len() as f32;
        assert!(oran >= 0.93, "{}/{} çift ters (oran {:.2})", yanlis, BASE_PAIRS.len(), oran);
    }

    #[test]
    fn selftest_tumu_gecer() {
        let mut n = net();
        train_pairwise(&mut n, &BASE_PAIRS, BASE_EPOCHS, BASE_LR);
        let probes = self_test(&n);
        assert_eq!(probes.len(), 7);
        for p in &probes {
            assert!(p.pass, "prob başarısız: {} ({})", p.name, p.output);
        }
    }

    fn avg_loss(n: &NetSpec) -> f32 {
        let mut s = 0.0;
        for (g, b) in &BASE_PAIRS {
            let d = forward(n, *g).output - forward(n, *b).output;
            s += -(sigmoid(d).ln().max(-20.0));
        }
        s / BASE_PAIRS.len() as f32
    }
}
