use criterion::{Criterion, criterion_group, criterion_main};
use ozyscrape::{Document, Rule};
use std::hint::black_box;
fn bench(c: &mut Criterion) {
    let html = format!(
        "<html><body>{}</body></html>",
        (0..1000)
            .map(|i| format!("<article class='product'><h2>Item {i}</h2><span class='price'>USD {i}</span></article>"))
            .collect::<String>()
    );
    c.bench_function("ozy_parse_1000_products", |b| b.iter(|| Document::parse(black_box(&html)).unwrap()));
    let doc = Document::parse(&html).unwrap();
    let rule = Rule {
        name: "price".into(),
        selector: ".price".into(),
        kind: "text".into(),
        attribute: None,
        adaptive: false,
    };
    c.bench_function("ozy_extract_first_100_prices", |b| {
        b.iter(|| doc.extract(black_box(&rule), None).unwrap())
    });
    c.bench_function("scraper_parse_1000_products", |b| {
        b.iter(|| scraper::Html::parse_document(black_box(&html)))
    });
}
criterion_group!(benches, bench);
criterion_main!(benches);
