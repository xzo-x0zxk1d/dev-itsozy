//! Original Rust extraction engine. Scrapling is a capability reference, not a dependency.
use ozy_core::{Error, bounded_text};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub mod crawl;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub selector: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub attribute: Option<String>,
    #[serde(default)]
    pub adaptive: bool,
}
fn default_kind() -> String {
    "text".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fingerprint {
    pub version: u8,
    pub tag: String,
    pub text: String,
    pub attributes: BTreeMap<String, String>,
    pub classes: BTreeSet<String>,
    pub ancestors: Vec<String>,
    pub neighbors: Vec<String>,
    pub sibling_pattern: Vec<String>,
    pub sibling_position: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extraction {
    pub values: Vec<String>,
    pub match_type: String,
    pub confidence: f64,
    pub candidate_margin: f64,
    pub fingerprint: Option<Fingerprint>,
}
pub struct Document {
    dom: Html,
}

impl Document {
    pub fn parse(source: &str) -> Result<Self, Error> {
        if source.len() > ozy_core::net::MAX_RESPONSE {
            return Err(Error::TooLarge);
        }
        Ok(Self {
            dom: Html::parse_document(source),
        })
    }
    pub fn select(&self, selector: &str) -> Result<Vec<ElementRef<'_>>, Error> {
        if selector.len() > 512 {
            return Err(Error::Invalid("Selector exceeds 512 bytes".into()));
        }
        let css = xpath_to_css(selector)?;
        let parsed = Selector::parse(&css).map_err(|_| Error::Invalid("Invalid or unsupported selector".into()))?;
        Ok(self.dom.select(&parsed).take(100).collect())
    }
    pub fn extract(&self, rule: &Rule, saved: Option<&Fingerprint>) -> Result<Extraction, Error> {
        if rule.name.is_empty() || rule.name.len() > 64 || !matches!(rule.kind.as_str(), "text" | "html" | "attribute") {
            return Err(Error::Invalid("Rule requires a name and text, html or attribute kind".into()));
        }
        if rule.kind == "attribute" && rule.attribute.as_ref().is_none_or(|a| a.is_empty() || a.len() > 64) {
            return Err(Error::Invalid("Attribute extraction requires an attribute name".into()));
        }
        let exact = self.select(&rule.selector)?;
        if !exact.is_empty() {
            let fp = fingerprint(exact[0]);
            return Ok(Extraction {
                values: exact.into_iter().map(|e| value(e, rule)).collect(),
                match_type: "exact".into(),
                confidence: 1.0,
                candidate_margin: 1.0,
                fingerprint: Some(fp),
            });
        }
        if rule.adaptive
            && let Some(saved) = saved
        {
            if saved.version != 1 {
                return Err(Error::Invalid("Unsupported fingerprint version".into()));
            }
            let all = Selector::parse("*").map_err(|_| Error::Upstream)?;
            let mut candidates: Vec<_> = self
                .dom
                .select(&all)
                .take(20_000)
                .filter(|e| e.value().name() == saved.tag)
                .map(|e| (e, similarity(saved, &fingerprint(e))))
                .collect();
            candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
            if let Some((element, confidence)) = candidates.first() {
                let margin = confidence - candidates.get(1).map(|c| c.1).unwrap_or(0.0);
                let accepted = *confidence >= 0.65 && margin >= 0.07;
                return Ok(Extraction {
                    values: if accepted { vec![value(*element, rule)] } else { vec![] },
                    match_type: if accepted { "recovered" } else { "uncertain" }.into(),
                    confidence: *confidence,
                    candidate_margin: margin,
                    fingerprint: None,
                });
            }
        }
        Ok(Extraction {
            values: vec![],
            match_type: "missing".into(),
            confidence: 0.0,
            candidate_margin: 0.0,
            fingerprint: None,
        })
    }
    pub fn metadata(&self, base: &str) -> serde_json::Value {
        let text = |selector: &str| {
            self.select(selector)
                .ok()
                .and_then(|v| v.first().map(|e| visible_text(*e)))
                .unwrap_or_default()
        };
        let attr = |selector: &str, key: &str| {
            self.select(selector)
                .ok()
                .and_then(|v| v.first().and_then(|e| e.value().attr(key).map(|v| bounded_text(v, 2048))))
                .unwrap_or_default()
        };
        let absolute = |value: String| {
            if value.is_empty() {
                None
            } else {
                url::Url::parse(base)
                    .ok()
                    .and_then(|u| u.join(&value).ok())
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                    .map(|u| u.to_string())
            }
        };
        let description = attr("meta[name='description']", "content");
        let og_description = attr("meta[property='og:description']", "content");
        serde_json::json!({
            "title": text("title"),
            "description": if og_description.is_empty() { description } else { og_description },
            "image": absolute(attr("meta[property='og:image']", "content")),
            "favicon": absolute(self.select("link[rel~='icon']").ok().and_then(|v| v.first().and_then(|e| e.value().attr("href").map(str::to_owned))).unwrap_or_else(|| "/favicon.ico".into())),
            "canonical_url": absolute(attr("link[rel='canonical']", "href")),
            "site_name": attr("meta[property='og:site_name']", "content"),
            "language": attr("html", "lang"), "generator": attr("meta[name='generator']", "content")
        })
    }
    pub fn markdown(&self) -> String {
        let html = self
            .select("body")
            .ok()
            .and_then(|nodes| nodes.first().map(|e| e.html()))
            .unwrap_or_default();
        bounded_text(&html2md::parse_html(&html), 100_000)
    }
    pub fn links(&self, base: &str) -> Vec<String> {
        let Ok(base) = url::Url::parse(base) else { return vec![] };
        self.select("a[href]")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let mut url = base.join(e.value().attr("href")?).ok()?;
                if url.origin() != base.origin() {
                    return None;
                }
                url.set_fragment(None);
                Some(url.to_string())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    /// Open Graph, Twitter card and article metadata (first 50 tags).
    pub fn meta_tags(&self) -> BTreeMap<String, String> {
        self.select("meta[property],meta[name]")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let key = e.value().attr("property").or_else(|| e.value().attr("name"))?.to_ascii_lowercase();
                let interesting = [
                    "og:",
                    "twitter:",
                    "article:",
                    "author",
                    "keywords",
                    "theme-color",
                    "robots",
                    "application-name",
                ];
                if !interesting.iter().any(|p| key.starts_with(p)) {
                    return None;
                }
                Some((bounded_text(&key, 64), bounded_text(e.value().attr("content")?, 1024)))
            })
            .take(50)
            .collect()
    }
    /// Visible text of the first match, or of the whole body when `selector` is None.
    pub fn text_of(&self, selector: Option<&str>) -> Result<String, Error> {
        let nodes = self.select(selector.unwrap_or("body"))?;
        Ok(nodes.first().map(|e| visible_text_bounded(*e, 200_000)).unwrap_or_default())
    }
    /// Values of `attribute` for every element matching `selector` (max 200).
    pub fn attributes(&self, selector: &str, attribute: &str) -> Vec<String> {
        self.select(selector)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e.value().attr(attribute).map(|v| bounded_text(v, 512)))
            .take(200)
            .collect()
    }
    pub fn resource_count(&self) -> usize {
        let selector =
            Selector::parse("script[src],img[src],link[rel='stylesheet'],iframe[src],video[src],audio[src]").expect("constant selector");
        self.dom.select(&selector).count()
    }
}
fn value(element: ElementRef<'_>, rule: &Rule) -> String {
    let value = match rule.kind.as_str() {
        "html" => element.html(),
        "attribute" => element
            .value()
            .attr(rule.attribute.as_deref().unwrap_or(""))
            .unwrap_or("")
            .to_owned(),
        _ => visible_text(element),
    };
    bounded_text(&value, 8192)
}
fn visible_text(element: ElementRef<'_>) -> String {
    visible_text_bounded(element, 1024)
}
fn visible_text_bounded(element: ElementRef<'_>, max_words: usize) -> String {
    element
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            if node
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|e| matches!(e.value().name(), "script" | "style" | "noscript" | "template"))
            {
                return None;
            }
            Some(text.text.as_ref())
        })
        .flat_map(str::split_whitespace)
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn fingerprint(element: ElementRef<'_>) -> Fingerprint {
    let siblings: Vec<_> = element
        .parent()
        .map(|p| p.children().filter_map(ElementRef::wrap).take(256).collect())
        .unwrap_or_default();
    let position = siblings.iter().position(|e| e.id() == element.id()).unwrap_or(0);
    let neighbors = [position.checked_sub(1), Some(position + 1)]
        .into_iter()
        .flatten()
        .filter_map(|i| siblings.get(i))
        .map(|e| format!("{}:{}", e.value().name(), bounded_text(&visible_text(*e), 64)))
        .collect();
    Fingerprint {
        version: 1,
        tag: element.value().name().into(),
        text: bounded_text(&visible_text(element), 256),
        attributes: element
            .value()
            .attrs()
            .filter(|(k, _)| !matches!(*k, "style" | "class"))
            .take(32)
            .map(|(k, v)| (k.into(), bounded_text(v, 256)))
            .collect(),
        classes: element.value().classes().take(32).map(str::to_owned).collect(),
        ancestors: element
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take(6)
            .map(|e| e.value().name().into())
            .collect(),
        neighbors,
        sibling_pattern: siblings.iter().map(|e| e.value().name().into()).collect(),
        sibling_position: position,
    }
}
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        a.intersection(b).count() as f64 / union as f64
    }
}
pub fn similarity(a: &Fingerprint, b: &Fingerprint) -> f64 {
    let mut weighted = 0.0;
    let mut total = 0.0;
    let mut add = |weight: f64, score: f64, present: bool| {
        if present {
            weighted += weight * score;
            total += weight;
        }
    };
    add(0.10, f64::from(a.tag == b.tag), true);
    add(0.28, strsim::normalized_levenshtein(&a.text, &b.text), !a.text.is_empty());
    let attrs = |fp: &Fingerprint| fp.attributes.iter().map(|(k, v)| format!("{k}={v}")).collect();
    add(0.18, jaccard(&attrs(a), &attrs(b)), !a.attributes.is_empty());
    add(0.10, jaccard(&a.classes, &b.classes), !a.classes.is_empty());
    add(
        0.12,
        strsim::normalized_levenshtein(&a.ancestors.join("/"), &b.ancestors.join("/")),
        !a.ancestors.is_empty(),
    );
    add(
        0.10,
        strsim::normalized_levenshtein(&a.neighbors.join("|"), &b.neighbors.join("|")),
        !a.neighbors.is_empty(),
    );
    add(
        0.06,
        strsim::normalized_levenshtein(&a.sibling_pattern.join("/"), &b.sibling_pattern.join("/")),
        !a.sibling_pattern.is_empty(),
    );
    add(0.06, 1.0 / (1.0 + a.sibling_position.abs_diff(b.sibling_position) as f64), true);
    if total == 0.0 { 0.0 } else { weighted / total }
}
/// A small XPath subset: //tag, //tag[@attr='value'], / child paths.
/// Unsupported expressions fail explicitly instead of being treated as CSS.
fn xpath_to_css(input: &str) -> Result<String, Error> {
    if !input.starts_with('/') {
        return Ok(input.into());
    }
    let step = regex::Regex::new(r#"^([A-Za-z][A-Za-z0-9_-]*|\*)(?:\[@([A-Za-z_][A-Za-z0-9_-]*)=['"]([^'"\]\\]*)['"]\])?$"#)
        .expect("constant regex");
    if !input.starts_with("//") {
        return Err(Error::Invalid("XPath supports descendant paths beginning with //".into()));
    }
    let mut css = String::new();
    let mut remaining = &input[2..];
    loop {
        let split = remaining.find('/').unwrap_or(remaining.len());
        let captures = step
            .captures(&remaining[..split])
            .ok_or_else(|| Error::Invalid("Unsupported XPath expression; use CSS selectors".into()))?;
        css.push_str(&captures[1]);
        if let (Some(key), Some(value)) = (captures.get(2), captures.get(3)) {
            css.push_str(&format!("[{}=\"{}\"]", key.as_str(), value.as_str()));
        }
        remaining = &remaining[split..];
        if remaining.is_empty() {
            break;
        }
        if remaining.starts_with("//") {
            css.push(' ');
            remaining = &remaining[2..];
        } else {
            css.push_str(" > ");
            remaining = &remaining[1..];
        }
    }
    Ok(css)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rule(selector: &str) -> Rule {
        Rule {
            name: "price".into(),
            selector: selector.into(),
            kind: "text".into(),
            attribute: None,
            adaptive: true,
        }
    }
    #[test]
    fn extraction_and_xpath() {
        let doc = Document::parse("<title>Shop</title><article><b data-id='a'> USD12 </b><script>bad()</script></article>").unwrap();
        assert_eq!(doc.extract(&rule("//b[@data-id='a']"), None).unwrap().values, ["USD12"]);
        assert_eq!(doc.extract(&rule("article"), None).unwrap().values, ["USD12"]);
        assert!(doc.select("//b[contains(text(),'a')]").is_err());
    }
    #[test]
    fn metadata_and_helpers() {
        let doc = Document::parse("<html lang='en'><head><title>T</title><meta property='og:image' content='/i.png'><meta name='twitter:card' content='summary'><link rel='icon' href='/f.ico'></head><body><p>Hello <script>x()</script>world</p><a href='/a#x'>a</a><a href='https://other.example/'>o</a></body></html>").unwrap();
        let meta = doc.metadata("https://example.com/page");
        assert_eq!(meta["title"], "T");
        assert_eq!(meta["image"], "https://example.com/i.png");
        assert_eq!(meta["favicon"], "https://example.com/f.ico");
        assert_eq!(doc.meta_tags()["twitter:card"], "summary");
        assert_eq!(doc.text_of(Some("p")).unwrap(), "Hello world");
        assert_eq!(doc.links("https://example.com/page"), ["https://example.com/a"]);
        assert!(doc.markdown().contains("Hello"));
    }
    #[test]
    fn attribute_rule_and_validation() {
        let doc = Document::parse("<a href='/x' class='l'>x</a>").unwrap();
        let mut r = rule("a.l");
        r.kind = "attribute".into();
        r.attribute = Some("href".into());
        assert_eq!(doc.extract(&r, None).unwrap().values, ["/x"]);
        r.attribute = None;
        assert!(doc.extract(&r, None).is_err());
        assert!(doc.select("a[").is_err());
        assert_eq!(doc.extract(&rule(".none"), None).unwrap().match_type, "missing");
    }
    #[test]
    fn recovers_renamed_class_without_claiming_exact() {
        let original = Document::parse("<article><h2>Camera</h2><span class='price' data-currency='USD'>120</span></article>").unwrap();
        let saved = original.extract(&rule(".price"), None).unwrap().fingerprint.unwrap();
        let changed =
            Document::parse("<section><article><h2>Camera</h2><span class='cost' data-currency='USD'>120</span></article></section>")
                .unwrap();
        let result = changed.extract(&rule(".price"), Some(&saved)).unwrap();
        assert_eq!(result.match_type, "recovered");
        assert_eq!(result.values, ["120"]);
        assert!(result.confidence >= 0.65);
    }
    #[test]
    fn ambiguous_recovery_returns_no_values() {
        let doc = Document::parse("<p><span class='p'>same</span></p>").unwrap();
        let saved = doc.extract(&rule(".p"), None).unwrap().fingerprint.unwrap();
        let changed = Document::parse("<p><span class='x'>same</span></p><p><span class='x'>same</span></p>").unwrap();
        let result = changed.extract(&rule(".p"), Some(&saved)).unwrap();
        assert_eq!(result.match_type, "uncertain");
        assert!(result.values.is_empty());
    }
}
