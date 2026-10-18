//! Benchmark runner used by bench/run.py. One engine and task per process so the
//! harness can also record peak memory.
//!
//!   cargo run --release -p ozyscrape --example compare -- <engine> <task> <file> <iterations> [changed-file]
//!   engines: ozyscrape | scraper | tl      tasks: parse | extract | parse_extract | memory | adaptive
use std::time::Instant;

const SELECTOR: &str = ".product .price";
/// tl supports only simple selectors, so it gets the flat equivalent.
const TL_SELECTOR: &str = ".price";

fn stats(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    let pick = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    serde_json::json!({"median_ms": pick(0.5), "p95_ms": pick(0.95), "min_ms": samples[0], "runs": samples.len()})
}

fn time<T>(iterations: usize, mut f: impl FnMut() -> T) -> (serde_json::Value, T) {
    for _ in 0..3 {
        std::hint::black_box(f());
    }
    let mut samples = Vec::with_capacity(iterations);
    let mut last = None;
    for _ in 0..iterations {
        let start = Instant::now();
        let value = std::hint::black_box(f());
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        last = Some(value);
    }
    (stats(samples), last.expect("at least one iteration"))
}

fn ozy_extract(doc: &ozyscrape::Document) -> usize {
    doc.texts(SELECTOR, usize::MAX).expect("valid selector").len()
}

fn scraper_extract(html: &scraper::Html, selector: &scraper::Selector) -> usize {
    html.select(selector)
        .map(|e| e.text().collect::<String>())
        .filter(|t| !t.is_empty())
        .count()
}

fn tl_extract(html: &str) -> usize {
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("parse");
    let parser = dom.parser();
    dom.query_selector(TL_SELECTOR)
        .map(|nodes| {
            nodes
                .filter_map(|n| n.get(parser))
                .map(|n| n.inner_text(parser).into_owned())
                // Explicit fold: every text is materialised, exactly as `count()` would drive it.
                .fold(0, |total, _text| total + 1)
        })
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (engine, task, file) = (args[1].as_str(), args[2].as_str(), &args[3]);
    let iterations: usize = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(20);
    let html = std::fs::read_to_string(file).expect("fixture");
    let selector = scraper::Selector::parse(SELECTOR).expect("selector");
    let result = match (engine, task) {
        ("ozyscrape", "parse") => time(iterations, || ozyscrape::Document::parse_unbounded(&html)).0,
        ("ozyscrape", "extract") => {
            let doc = ozyscrape::Document::parse_unbounded(&html);
            let (stats, count) = time(iterations, || ozy_extract(&doc));
            serde_json::json!({"timing": stats, "matches": count})
        }
        ("ozyscrape", "parse_extract") => {
            let (stats, count) = time(iterations, || ozy_extract(&ozyscrape::Document::parse_unbounded(&html)));
            serde_json::json!({"timing": stats, "matches": count})
        }
        ("ozyscrape", "memory") => {
            let doc = ozyscrape::Document::parse_unbounded(&html);
            let count = ozy_extract(&doc);
            std::thread::sleep(std::time::Duration::from_millis(400));
            serde_json::json!({"matches": count})
        }
        ("ozyscrape", "adaptive") => {
            let changed = std::fs::read_to_string(&args[5]).expect("changed fixture");
            let original = ozyscrape::Document::parse_unbounded(&html);
            let target = original.select("#p500 .price").expect("selector")[0];
            let expected = target.text().collect::<String>();
            let fingerprint = ozyscrape::fingerprint(target);
            let moved = ozyscrape::Document::parse_unbounded(&changed);
            let (stats, found) = time(iterations, || moved.relocate(&fingerprint));
            let (value, confidence, margin) = found.unwrap_or_default();
            serde_json::json!({"timing": stats, "expected": expected, "found": value, "correct": value == expected,
                "confidence": confidence, "margin": margin})
        }
        ("scraper", "parse") => time(iterations, || scraper::Html::parse_document(&html)).0,
        ("scraper", "extract") => {
            let doc = scraper::Html::parse_document(&html);
            let (stats, count) = time(iterations, || scraper_extract(&doc, &selector));
            serde_json::json!({"timing": stats, "matches": count})
        }
        ("scraper", "parse_extract") => {
            let (stats, count) = time(iterations, || scraper_extract(&scraper::Html::parse_document(&html), &selector));
            serde_json::json!({"timing": stats, "matches": count})
        }
        ("scraper", "memory") => {
            let doc = scraper::Html::parse_document(&html);
            let count = scraper_extract(&doc, &selector);
            std::thread::sleep(std::time::Duration::from_millis(400));
            serde_json::json!({"matches": count})
        }
        ("tl", "parse") => {
            time(iterations, || {
                tl::parse(&html, tl::ParserOptions::default()).map(|d| d.nodes().len()).unwrap_or(0)
            })
            .0
        }
        ("tl", "parse_extract") => {
            let (stats, count) = time(iterations, || tl_extract(&html));
            serde_json::json!({"timing": stats, "matches": count})
        }
        ("tl", "memory") => {
            let dom = tl::parse(&html, tl::ParserOptions::default()).expect("parse");
            let count = dom.query_selector(TL_SELECTOR).map(|n| n.count()).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(400));
            serde_json::json!({"matches": count})
        }
        _ => serde_json::json!({"skipped": "not supported"}),
    };
    println!("{}", serde_json::json!({"engine": engine, "task": task, "result": result}));
}
