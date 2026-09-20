# NoralWeb — Neural Research Browser

A tiny (~2.3 MB) single-file web browser that **researches instead of just browsing**.
Type anything that isn't a URL and it fans out to **40+ live sources**, ranks every
candidate with a **trainable neural ranker (MLP, not an LLM)**, and shows the receipts:
per-result score breakdowns, source labels, and a live visualization of the network.

No Electron. No Chromium bundle. One Rust binary + your system webview
(WebView2 on Windows, WebKitGTK on Linux).

![Rust](https://img.shields.io/badge/Rust-stable-blue)
![Windows](https://img.shields.io/badge/Windows-exe-0078D6)
![Linux](https://img.shields.io/badge/Linux-AppImage-orange)
![License](https://img.shields.io/badge/license-MIT-green)

---

## Why it hooks you

- **Ask anything, get a ranked report** — one query hits Wikipedia, Bing, Brave,
  Yahoo, Yandex, YouTube, Google News, Reddit, Semantic Scholar, Crossref, ORCID,
  GitHub, Stack Overflow, Hacker News, arXiv, npm, crates.io, and 25+ more. Free
  sources, no API keys needed for the core set.
- **It learns what *you* consider good** — every click trains the ranker
  (pairwise updates, saved to `noral-model.json`). Your browser gets smarter the
  more you use it. Watch it happen live in the **N-NET panel**.
- **It sees what scrapers can't** — the experimental **HARVEST** engine renders
  pages in a hidden real browser and reads the screen, beating bot walls that
  stop plain HTTP clients (Google-H, Yandex-H fallbacks included).
- **Agentic mode (AJAN)** — NVIDIA NIM-powered assistant with tools:
  `web_search`, `fetch_page`, `harvest`, `open_tab`. OSINT mode included.
- **Deep ring** — top seeds are followed in-page, their links fetched and ranked
  as second-ring candidates.
- **Bookmarks + recent searches**, vertical tabs, split view, find-fast shortcuts.
- **Bilingual UI** — Turkish / English, auto-detected from the system language.
- **Private by design** — no account, no telemetry, no cloud. API keys (optional,
  for paid lanes like Apinex/Brave/Exa) live in plain text files *next to the
  exe*, never in code, never in git.

## How it ranks (the honest version)

```
query → 40+ parallel fetchers → candidates
      → BM25-lite + TF-cosine + authority + title/concise/page/deep/exact/surname/handles (12 features)
      → MLP 12-48-28-1 + skip connection (2037 params) ─┐
      → classic score ───────────────────────────────────┤ 50/50 blend
      → MMR diversity penalty → ranked report
```

- No LLM in the ranking path. Small, deterministic, testable
  (`cargo test`: 43 tests incl. gradient checks, probe suite, parser fixtures).
- Click feedback = pairwise logistic updates with projection SGD.
- Silent sources are shown grey **with reasons** (`Google-H(duvar)`,
  `SearXNG(429)`…), so you always know what didn't answer and why.

## Project layout

```
src/
  main.rs      — window, tabs, IPC, agent loop, harvest bridge, Google/Yandex-H fallback
  fetch.rs     — 40+ source scrapers/API clients (ureq only, zero extra deps), silent tracking
  research.rs  — Candidate/Ranked/Report, BM25+cosine+MMR, rank()
  neural.rs    — MLP 12-48-28-1, pairwise training, click learning, self-test probes
  nim.rs       — NVIDIA NIM OpenAI-compatible client, tool schema, retry/backoff
ui/
  index.html   — whole UI (Zen-style dark theme, TR/EN i18n, bookmarks, N-NET svg)
```

## Build

### Windows (single .exe, ~2.3 MB)

Requirements: [Rust stable (GNU toolchain)](https://rustup.rs) + MSYS2 UCRT64
(`pacman -S mingw-w64-ucrt-x86_64-gcc`).

```powershell
$env:PATH = "C:\msys64\ucrt64\bin;" + $env:PATH
cargo build --release
# → target\release\noral-web.exe
```

> The project folder contains a Turkish character (`nöral web`) which breaks the
> GNU linker — point the target dir elsewhere if you build from such a path:
> `$env:CARGO_TARGET_DIR = "C:\Temp\noral-target"`.

### Linux (ELF + AppImage)

Requirements: Rust stable + `libgtk-3-dev libwebkit2gtk-4.1-dev
libayatana-appindicator3-dev` (Ubuntu 24.04+).

```bash
cargo build --release
# → target/release/noral-web
```

For a portable build, package with
[linuxdeploy](https://github.com/linuxdeploy/linuxdeploy) (`--plugin gtk`).
Note: distro WebKit builds hardcode the helper path
`/usr/lib/.../webkit2gtk-4.1` — after linuxdeploy, rewrite it inside the AppDir
(see `tauri-bundler`'s approach):

```bash
find "$APPDIR"/usr/lib* -name 'libwebkit*' -exec sed -i -e 's|/usr|././|g' '{}' \;
```

## API keys (all optional)

Drop any of these next to the binary (or `%APPDATA%\NoralWeb` on Windows,
`~/.config/NoralWeb` on Linux) — the app picks them up automatically:

| file | lane |
|---|---|
| `apinex-key.txt` | Apinex web/research/twitter (paid) |
| `langsearch-key.txt` | LangSearch |
| `brave-key.txt` / `exa-key.txt` / `tavily-key.txt` / `serper-key.txt` | paid search APIs |
| NIM key (`nim-key.txt`, set from the AJAN gate) | agent mode |

Without keys you still get the full 40+ free-source pipeline.

## Terminal (no exe double-click)

`noral` behaves like any modern CLI — install once, run from anywhere:

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/samansarmasik-alt/NoralWeb/main/install.ps1 | iex
```

```bash
# Linux
curl -fsSL https://raw.githubusercontent.com/samansarmasik-alt/NoralWeb/main/install.sh | sh
```

Then:

```bash
noral "kuantum bilgisayar" --fast --limit 5   # ranked report
noral "sorgu" --json                          # machine-readable
noral --agent "bulguları topla" --osint        # agentic (needs NIM_KEY)
noral --testmode                               # probe suite
```

Source lives in [`cli/`](cli) and shares the desktop core by reference
(`#[path]` — zero copy drift). Linux binary needs nothing but `chmod +x`.

## Usage

- **Address bar**: URL → opens site. Anything else → neural research.
- **Cards**: title or **Open** → opens in a tab (and trains the ranker).
  **★** bookmarks. **Copy** copies title+URL+snippet.
- **N-NET button**: live neural-net visualization (panel opens *only* here).
- **Ctrl+K / Ctrl+L** address · **Ctrl+T/W** tabs · **Alt+1..8** jump ·
  **Ctrl+\\** split.
- **HASAT** toggle: hidden-browser harvesting for bot-walled pages.
- **TR/EN** button or automatic from system language.

## Caveats (read before judging a silent source)

- Some providers rate-limit/ban datacenter IPs or non-browser TLS fingerprints
  (Google-ureq serves a JS shell, SearXNG pools 429, Yandex captchas). The app
  fails **silent-with-reason** and falls back (lite endpoints, page-2, harvest).
- An obscure name with no web footprint returns thin results *everywhere* —
  that's the web telling the truth, not a bug. Compare with a head query
  (`kuantum bilgisayar`) to see the full firehose.
- Scraping third-party HTML is inherently brittle and may violate a
  provider's ToS — use fair query rates; paid API lanes exist for heavy use.

## Tests

```powershell
cargo test            # 43 pass (offline; network probes are #[ignore])
cargo test -- --ignored   # live probes (need internet; some fail behind banned IPs)
```

## License

MIT — see [LICENSE](LICENSE).
