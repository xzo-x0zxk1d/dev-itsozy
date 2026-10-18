# OzyScrape benchmarks

Measured on 2026-10-18. Raw data is in [`bench/results.json`](bench/results.json); every number below comes from
that file. Nothing here is estimated or extrapolated.

## Setup

| | |
|---|---|
| Machine | Windows 11 (10.0.26200), Intel 64 Family 6 Model 183 (16 logical cores) |
| Rust | rustc 1.99.1, `--release` (thin LTO, codegen-units 1) |
| Python | 3.12.10; Scrapling 0.5, lxml 6.1.3, Parsel 1.12.1, BeautifulSoup 4.15.0 |
| Inputs | Deterministic synthetic catalogues from `bench/make_fixtures.py`: 1,000 products (486 KiB) and 5,000 products (2,457 KiB), with nested markup, attributes, `<style>` and `<script>` |
| Task | Select `.product .price` and read every element's text (1,000 / 5,000 matches; every engine returned all of them). `tl` only supports simple selectors, so it used `.price` |
| Method | 3 warm-up runs, then 30 (1k) or 10 (5k) timed iterations per process; each benchmark ran in **3 separate processes** and the table shows the median of the three per-process medians |
| Memory | Peak working set of the benchmark process tree (psutil) while holding the parsed DOM, median of 3 processes, minus the same engine parsing a tiny document ("DOM memory") |

Reproduce: see the header of [`bench/run.py`](bench/run.py).

## HTML parsing and selector extraction

### 1,000 products (486 KiB)

| Engine | Parse (ms) | Extract on parsed DOM (ms) | Parse + extract (ms) | Relative to OzyScrape | DOM memory (MiB) | Process peak (MiB) |
|---|---:|---:|---:|---:|---:|---:|
| **OzyScrape** (Rust) | 10.43 | 0.27 | **12.71** | 1.00× | 4.3 | 9.4 |
| scraper crate (Rust) | 10.36 | 0.20 | 12.40 | 0.98× | 4.3 | 9.3 |
| tl crate (Rust)\* | 1.75 | n/a | 1.95 | 0.15× | 4.5 | 9.2 |
| Scrapling 0.5 | 18.54 | 16.44 | 36.26 | 2.85× slower | 8.4 | 36.7 |
| lxml | 18.51 | 17.01 | 37.10 | 2.92× slower | 7.7 | 27.1 |
| Parsel | 18.85 | 16.04 | 36.38 | 2.86× slower | 8.8 | 35.7 |
| BeautifulSoup (lxml) | 130.74 | 31.82 | 162.87 | 12.8× slower | 12.3 | 35.7 |
| BeautifulSoup (html.parser) | 282.49 | 31.64 | 288.64 | 22.7× slower | 12.1 | 35.4 |

### 5,000 products (2,457 KiB)

| Engine | Parse (ms) | Extract on parsed DOM (ms) | Parse + extract (ms) | Relative to OzyScrape | DOM memory (MiB) | Process peak (MiB) |
|---|---:|---:|---:|---:|---:|---:|
| **OzyScrape** (Rust) | 54.65 | 1.40 | **64.88** | 1.00× | 26.3 | 31.4 |
| scraper crate (Rust) | 54.74 | 1.05 | 63.37 | 0.98× | 26.3 | 31.4 |
| tl crate (Rust)\* | 10.84 | n/a | 11.99 | 0.18× | 24.8 | 29.6 |
| Scrapling 0.5 | 89.10 | 106.21 | 196.74 | 3.03× slower | 41.5 | 69.8 |
| lxml | 88.02 | 109.85 | 199.45 | 3.07× slower | 41.1 | 60.4 |
| Parsel | 89.55 | 105.32 | 196.02 | 3.02× slower | 43.9 | 70.8 |
| BeautifulSoup (lxml) | 1,119.54 | 161.79 | 1,099.09 | 16.9× slower | 57.8 | 81.2 |
| BeautifulSoup (html.parser) | 2,563.01 | 161.88 | 2,395.63 | 36.9× slower | 60.4 | 83.6 |

\* `tl` is a fast, non-spec-compliant tokenizer-style parser without descendant combinators or HTML5 tree construction,
so it is not a drop-in equivalent.

What this shows:

- Against Scrapling, lxml and Parsel, OzyScrape is about **2.9–3.1× faster end to end**. Selector extraction on an
  already-parsed DOM is about **60–80× faster** (0.27 ms vs 16–17 ms for 1,000 matches), and OzyScrape uses roughly
  **half the DOM memory**.
- OzyScrape is built on html5ever via the `scraper` crate, so raw parsing speed is the same as `scraper` (within noise).
  The engine's extra validation, text normalisation and script/style filtering cost under 0.4 ms per 1,000 matches.
- `tl` parses about 5–6× faster than any html5ever-based engine. OzyScrape keeps html5ever because its HTML5-spec tree
  construction matches what browsers build, and CSS selector support matters for user-supplied rules.
- BeautifulSoup's numbers fluctuate at 5k (its parse-only median sits slightly above parse + extract), which is within
  its run-to-run variance.
- **No 700× or 1000× claim is made.** The measured advantage over Scrapling is about 3× for full parse + extract.

## Adaptive selector recovery

The fingerprint is taken from product #500's price with `#p500 .price` on the original page. The "redesigned" page renames
every class (`product`→`tile`, `price`→`amount`, …), adds a wrapper element, swaps `section` for `ul`, and changes the
container hierarchy, so the original selector matches nothing. Each engine must find the same price without a new selector.

| Engine | Relocation time (median, ms) | Correct element | Reported confidence | Margin over runner-up |
|---|---:|---|---:|---:|
| **OzyScrape** | **6.66** | yes (`277.35`) | 0.836 | 0.093 |
| Scrapling 0.5 (`css(..., adaptive=True)`) | 1,082.54 | yes (`277.35`) | not reported | not reported |

Both engines recovered the correct element. OzyScrape did it about **160× faster** in this test and also reports a
confidence and the margin over the next candidate. It returns `uncertain` with no values when confidence is below 0.65
or the margin is below 0.07. This is a single scenario; real-world recovery rates depend on how much the page changes.

## Network: fetch latency, requests per second and concurrent crawling

These were **not** stress-benchmarked on purpose:

- OzyScrape enforces at least 1 second between requests to the same host (and honours robots.txt crawl-delay). Per-host
  throughput is therefore capped at about 1 request/s by policy, not by engine speed.
- Load-testing Scrapy/Scrapling/OzyScrape against third-party sites would be abusive, and SafeHttp refuses loopback
  targets (SSRF protection), so a local test server cannot be used without weakening production code.

Measured production behaviour instead (2026-10-18, `dev.itsozy.lol`, Vercel iad1, from server-side request logs during
the production smoke test; external sites were the toscrape.com scraping sandboxes):

| Call | Server-side duration |
|---|---:|
| Inline HTML scrape (no network) | 0–1 ms |
| PDF parse (1 page) | 1 ms |
| URL check (static heuristics) | 2 ms |
| Email validation (DNS) | 7 ms |
| HTTP performance check | 35 ms |
| Scrape of books.toscrape.com (robots.txt + fetch + extract) | 37–144 ms |
| Website intelligence (fetch + TLS handshake) | 70 ms |
| Link preview | 138 ms |
| Crawl, 3 pages at concurrency 2 on one host | 4,083 ms (dominated by the 1 s per-host politeness delay) |

Median server-side duration across the 14 metered calls was 36 ms (p95 144 ms). Client round trips from the
development PC (Saudi Arabia → us-east-1) added about 600–700 ms of network latency.

## Limitations of these results

- One machine, one OS, normal background load. Absolute numbers will differ elsewhere; ratios were stable across the
  three process repeats (see `process_medians_ms` in the raw data).
- Synthetic, well-formed markup. Malformed real-world pages may shift the parse results.
- The Python engines' text APIs normalise whitespace slightly differently; all engines returned the same match counts.
