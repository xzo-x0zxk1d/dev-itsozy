//! Stateless request/response APIs: scrape, preview, intelligence, performance,
//! URL safety, email validation, OCR, screenshots and PDF parsing.
use crate::*;
use axum::extract::{Extension, FromRequest, Multipart, Query};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::StreamExt;
use ozyscrape::{Document, Fingerprint, Rule};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Clone)]
pub struct ScrapeInput {
    pub url: Option<String>,
    pub html: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub markdown: bool,
    #[serde(default)]
    pub cache_seconds: u32,
    /// Render JavaScript through the optional browser worker before extraction.
    #[serde(default)]
    pub render: bool,
}

pub fn validate_rules(rules: &[Rule]) -> Result<()> {
    if rules.len() > 20 {
        return Err(invalid("At most 20 rules are supported"));
    }
    let doc = Document::parse("")?;
    let mut names = std::collections::HashSet::new();
    for rule in rules {
        if !names.insert(&rule.name) {
            return Err(invalid("Rule names must be unique"));
        }
        doc.extract(rule, None)?;
    }
    Ok(())
}

pub async fn scrape(State(app): State<App>, Extension(id): Extension<Identity>, Json(input): Json<ScrapeInput>) -> Result<Json<Value>> {
    Ok(Json(extract(&app, id.user_id, &input).await?))
}

pub async fn extract(app: &App, owner: Uuid, input: &ScrapeInput) -> Result<Value> {
    validate_rules(&input.rules)?;
    if input.url.is_some() == input.html.is_some() {
        return Err(invalid("Provide exactly one of url or html"));
    }
    if input.html.as_ref().is_some_and(|h| h.len() > ozy_core::net::MAX_RESPONSE) {
        return Err(invalid("Inline html must be at most 2 MiB"));
    }
    if input.render && input.html.is_some() {
        return Err(invalid("render applies only to url requests"));
    }
    if input.cache_seconds > 3600 {
        return Err(invalid("Cache lifetime must be at most 3600 seconds"));
    }
    let cache_key = ozy_core::digest(&serde_json::to_vec(input).map_err(|_| invalid("Invalid extraction"))?);
    if input.cache_seconds > 0
        && let Some(mut cached) =
            sqlx::query_scalar::<_, Value>("SELECT value FROM ozy.cache WHERE user_id=$1 AND key=$2 AND expires_at>now()")
                .bind(owner)
                .bind(&cache_key)
                .fetch_optional(&app.db)
                .await?
    {
        cached["cached"] = json!(true);
        return Ok(cached);
    }
    let (source, origin, status, rendered) = if let Some(url) = &input.url {
        domain_budget(app, url).await?;
        if input.render {
            let page = worker(app, "render", json!({"url": url})).await?;
            let html = page["html"].as_str().ok_or_else(|| invalid("Worker returned no HTML"))?;
            if html.len() > ozy_core::net::MAX_RESPONSE {
                return Err(ozy_core::Error::TooLarge.into());
            }
            (
                html.to_owned(),
                page["final_url"].as_str().unwrap_or(url).to_owned(),
                page["status"].as_u64().unwrap_or(0) as u16,
                true,
            )
        } else {
            let response = app.http.fetch(url, true).await?;
            (
                String::from_utf8_lossy(&response.body).into_owned(),
                response.url,
                response.status,
                false,
            )
        }
    } else {
        (input.html.clone().unwrap_or_default(), "inline".into(), 200, false)
    };
    let mut saved = BTreeMap::<String, Fingerprint>::new();
    for rule in input.rules.iter().filter(|r| r.adaptive) {
        if let Some(value) =
            sqlx::query_scalar::<_, Value>("SELECT fingerprint FROM ozy.fingerprints WHERE user_id=$1 AND origin=$2 AND identifier=$3")
                .bind(owner)
                .bind(&origin)
                .bind(&rule.name)
                .fetch_optional(&app.db)
                .await?
            && let Ok(fp) = serde_json::from_value(value)
        {
            saved.insert(rule.name.clone(), fp);
        }
    }
    // DOM values are deliberately dropped before the next await.
    let (mut result, fingerprints) = {
        let doc = Document::parse(&source)?;
        let mut fields = BTreeMap::new();
        let mut fingerprints = Vec::new();
        for rule in &input.rules {
            let mut extracted = doc.extract(rule, saved.get(&rule.name))?;
            if rule.adaptive {
                if let Some(fp) = extracted.fingerprint.take() {
                    fingerprints.push((rule.name.clone(), json!(fp)));
                }
            } else {
                extracted.fingerprint = None;
            }
            fields.insert(rule.name.clone(), extracted);
        }
        (
            json!({"url":origin,"status":status,"rendered":rendered,"fields":fields,"metadata":doc.metadata(&origin),
            "markdown":if input.markdown {Some(doc.markdown())} else {None},"cached":false}),
            fingerprints,
        )
    };
    for (name, fp) in fingerprints {
        sqlx::query("INSERT INTO ozy.fingerprints(user_id,origin,identifier,fingerprint) VALUES($1,$2,$3,$4) ON CONFLICT(user_id,origin,identifier) DO UPDATE SET fingerprint=$4,updated_at=now()")
            .bind(owner)
            .bind(&origin)
            .bind(name)
            .bind(fp)
            .execute(&app.db)
            .await?;
    }
    if input.cache_seconds > 0 {
        sqlx::query("INSERT INTO ozy.cache(user_id,key,value,expires_at) VALUES($1,$2,$3,now()+make_interval(secs=>$4)) ON CONFLICT(user_id,key) DO UPDATE SET value=$3,expires_at=now()+make_interval(secs=>$4)")
            .bind(owner)
            .bind(cache_key)
            .bind(&result)
            .bind(f64::from(input.cache_seconds))
            .execute(&app.db)
            .await?;
    }
    result["limits"] = json!({"matches_per_rule":100,"text_characters_per_match":8192});
    Ok(result)
}

#[derive(Deserialize)]
pub struct UrlInput {
    pub url: String,
}

pub async fn preview(State(app): State<App>, Query(input): Query<UrlInput>) -> Result<Json<Value>> {
    domain_budget(&app, &input.url).await?;
    let response = app.http.fetch(&input.url, true).await?;
    let doc = Document::parse(&String::from_utf8_lossy(&response.body))?;
    Ok(Json(
        json!({"url":response.url,"status":response.status,"redirects":response.redirects,
        "metadata":doc.metadata(&response.url),"public_metadata":doc.meta_tags()}),
    ))
}

/// (name, category, signals). A signal is (source, key, needle, confidence);
/// an empty needle means "header present".
type Signal = (&'static str, &'static str, &'static str, f64);
const TECHNOLOGIES: &[(&str, &str, &[Signal])] = &[
    (
        "WordPress",
        "CMS",
        &[
            ("html", "wp-content/", "", 0.8),
            ("html", "wp-includes/", "", 0.8),
            ("generator", "wordpress", "", 0.95),
        ],
    ),
    (
        "Drupal",
        "CMS",
        &[
            ("html", "drupal-settings-json", "", 0.9),
            ("header", "x-drupal-cache", "", 0.9),
            ("generator", "drupal", "", 0.95),
        ],
    ),
    (
        "Joomla",
        "CMS",
        &[("generator", "joomla", "", 0.95), ("html", "/media/jui/", "", 0.6)],
    ),
    ("Ghost", "CMS", &[("generator", "ghost", "", 0.95)]),
    ("Hugo", "Static site generator", &[("generator", "hugo", "", 0.95)]),
    ("Jekyll", "Static site generator", &[("generator", "jekyll", "", 0.95)]),
    (
        "Gatsby",
        "Frontend framework",
        &[("html", "___gatsby", "", 0.9), ("generator", "gatsby", "", 0.95)],
    ),
    (
        "Astro",
        "Frontend framework",
        &[("html", "astro-island", "", 0.85), ("generator", "astro", "", 0.95)],
    ),
    (
        "Next.js",
        "Frontend framework",
        &[
            ("html", "/_next/", "", 0.9),
            ("html", "__next_data__", "", 0.95),
            ("header", "x-powered-by", "next.js", 0.95),
        ],
    ),
    (
        "Nuxt",
        "Frontend framework",
        &[("html", "/_nuxt/", "", 0.9), ("html", "window.__nuxt", "", 0.85)],
    ),
    ("SvelteKit", "Frontend framework", &[("html", "_app/immutable/", "", 0.85)]),
    ("Remix", "Frontend framework", &[("html", "__remixcontext", "", 0.9)]),
    ("Angular", "Frontend framework", &[("html", "ng-version=", "", 0.95)]),
    (
        "React",
        "JavaScript library",
        &[
            ("html", "data-reactroot", "", 0.8),
            ("html", "__next_data__", "", 0.6),
            ("script", "react", "", 0.5),
        ],
    ),
    (
        "Vue.js",
        "JavaScript library",
        &[
            ("html", "data-v-", "", 0.6),
            ("script", "vue", "", 0.5),
            ("html", "/_nuxt/", "", 0.6),
        ],
    ),
    ("Ember.js", "JavaScript library", &[("html", "ember-application", "", 0.85)]),
    ("jQuery", "JavaScript library", &[("script", "jquery", "", 0.8)]),
    (
        "Alpine.js",
        "JavaScript library",
        &[("script", "alpine", "", 0.7), ("html", " x-data=", "", 0.5)],
    ),
    (
        "htmx",
        "JavaScript library",
        &[("script", "htmx", "", 0.8), ("html", " hx-get=", "", 0.7)],
    ),
    (
        "Bootstrap",
        "UI framework",
        &[("html", "bootstrap.min.css", "", 0.8), ("script", "bootstrap", "", 0.7)],
    ),
    (
        "Font Awesome",
        "Font library",
        &[("html", "font-awesome", "", 0.7), ("html", "fontawesome", "", 0.7)],
    ),
    (
        "Shopify",
        "E-commerce",
        &[("html", "cdn.shopify.com", "", 0.9), ("header", "x-shopify-stage", "", 0.95)],
    ),
    ("WooCommerce", "E-commerce", &[("html", "woocommerce", "", 0.8)]),
    ("Magento", "E-commerce", &[("html", "mage/cookies", "", 0.8)]),
    ("BigCommerce", "E-commerce", &[("html", "cdn11.bigcommerce.com", "", 0.9)]),
    (
        "Wix",
        "Website builder",
        &[("header", "x-wix-request-id", "", 0.95), ("html", "static.wixstatic.com", "", 0.85)],
    ),
    (
        "Squarespace",
        "Website builder",
        &[("html", "static1.squarespace.com", "", 0.9), ("generator", "squarespace", "", 0.95)],
    ),
    (
        "Webflow",
        "Website builder",
        &[("html", "data-wf-site", "", 0.9), ("generator", "webflow", "", 0.95)],
    ),
    (
        "Google Analytics",
        "Analytics",
        &[
            ("html", "googletagmanager.com/gtag/js", "", 0.9),
            ("html", "google-analytics.com", "", 0.8),
        ],
    ),
    (
        "Google Tag Manager",
        "Tag manager",
        &[("html", "googletagmanager.com/gtm.js", "", 0.9)],
    ),
    ("Plausible", "Analytics", &[("script", "plausible.io", "", 0.95)]),
    (
        "Cloudflare",
        "CDN",
        &[("header", "cf-ray", "", 0.95), ("header", "server", "cloudflare", 0.95)],
    ),
    (
        "Vercel",
        "Hosting",
        &[("header", "x-vercel-id", "", 0.95), ("header", "server", "vercel", 0.95)],
    ),
    (
        "Netlify",
        "Hosting",
        &[("header", "x-nf-request-id", "", 0.95), ("header", "server", "netlify", 0.95)],
    ),
    (
        "Amazon CloudFront",
        "CDN",
        &[("header", "x-amz-cf-id", "", 0.95), ("header", "via", "cloudfront", 0.9)],
    ),
    ("Fastly", "CDN", &[("header", "x-served-by", "cache-", 0.8)]),
    (
        "GitHub Pages",
        "Hosting",
        &[("header", "server", "github.com", 0.95), ("header", "x-github-request-id", "", 0.8)],
    ),
    ("Fly.io", "Hosting", &[("header", "fly-request-id", "", 0.95)]),
    ("Nginx", "Web server", &[("header", "server", "nginx", 0.95)]),
    ("Apache HTTP Server", "Web server", &[("header", "server", "apache", 0.95)]),
    ("LiteSpeed", "Web server", &[("header", "server", "litespeed", 0.95)]),
    ("Microsoft IIS", "Web server", &[("header", "server", "microsoft-iis", 0.95)]),
    ("Caddy", "Web server", &[("header", "server", "caddy", 0.95)]),
    ("PHP", "Programming language", &[("header", "x-powered-by", "php", 0.95)]),
    ("ASP.NET", "Web framework", &[("header", "x-powered-by", "asp.net", 0.95)]),
    ("Express", "Web framework", &[("header", "x-powered-by", "express", 0.95)]),
    ("HSTS", "Security", &[("header", "strict-transport-security", "", 1.0)]),
    ("HTTP/3", "Protocol", &[("header", "alt-svc", "h3", 0.9)]),
];

/// Deterministic technology detection. Confidence combines independent signals as 1-Π(1-c).
pub fn detect_technologies(html: &str, headers: &std::collections::HashMap<String, String>) -> Vec<Value> {
    let doc = Document::parse(html).ok();
    let lower = html.to_lowercase();
    let generator = doc
        .as_ref()
        .map(|d| d.attributes("meta[name='generator']", "content").join(" ").to_lowercase())
        .unwrap_or_default();
    let scripts = doc
        .as_ref()
        .map(|d| d.attributes("script[src]", "src").join(" ").to_lowercase())
        .unwrap_or_default();
    let mut found = Vec::new();
    for (name, category, signals) in TECHNOLOGIES {
        let mut miss = 1.0;
        let mut evidence = Vec::new();
        for (source, key, needle, confidence) in *signals {
            let matched = match *source {
                "html" => lower.contains(key),
                "generator" => generator.contains(key),
                "script" => scripts.contains(key),
                _ => headers
                    .get(*key)
                    .is_some_and(|v| needle.is_empty() || v.to_lowercase().contains(needle)),
            };
            if matched {
                miss *= 1.0 - confidence;
                evidence.push(json!({"source":source,"key":key,"match":if needle.is_empty() {None} else {Some(needle)}}));
            }
        }
        if !evidence.is_empty() {
            let confidence = ((1.0 - miss) * 100.0_f64).round() / 100.0;
            found.push(json!({"name":name,"category":category,"confidence":confidence,"evidence":evidence}));
        }
    }
    found
}

pub async fn intelligence(State(app): State<App>, Query(input): Query<UrlInput>) -> Result<Json<Value>> {
    domain_budget(&app, &input.url).await?;
    let response = app.http.fetch(&input.url, true).await?;
    let source = String::from_utf8_lossy(&response.body).into_owned();
    let technologies = detect_technologies(&source, &response.headers);
    let metadata = Document::parse(&source)?.metadata(&response.url);
    let final_url = url::Url::parse(&response.url).map_err(|_| invalid("Invalid final URL"))?;
    let tls = if final_url.scheme() == "https" {
        match ozy_core::tls::inspect(&final_url).await {
            Ok(certificate) => json!({"https":true,"certificate":certificate}),
            Err(error) => json!({"https":true,"certificate":null,"error":error.to_string()}),
        }
    } else {
        json!({"https":false,"certificate":null})
    };
    Ok(Json(
        json!({"url":response.url,"status":response.status,"technologies":technologies,"headers":response.headers,
        "metadata":metadata,"tls":tls,"method":"deterministic fingerprints v1",
        "limitations":["Indicators can be hidden or spoofed; absence is not proof","Only the first HTML response is analysed; scripts are not executed"]}),
    ))
}

pub async fn performance(State(app): State<App>, Json(input): Json<Value>) -> Result<Json<Value>> {
    let url = input.get("url").and_then(Value::as_str).ok_or_else(|| invalid("url is required"))?;
    ozy_core::net::validate_url(url)?;
    if input.get("browser").and_then(Value::as_bool) == Some(true) {
        domain_budget(&app, url).await?;
        let Json(mut measured) = worker(&app, "performance", json!({"url":url})).await?;
        measured["measurement"] = json!("browser");
        return Ok(Json(measured));
    }
    domain_budget(&app, url).await?;
    let response = app.http.fetch(url, false).await?;
    let count = Document::parse(&String::from_utf8_lossy(&response.body))?.resource_count();
    Ok(Json(json!({"url":response.url,"status":response.status,"measurement":"http_fetch",
        "elapsed_ms":response.elapsed_ms,"first_byte_ms":response.first_byte_ms,"html_bytes":response.body.len(),
        "redirect_count":response.redirects.len(),"redirects":response.redirects,
        "declared_resource_count":count,"browser_metrics":null,
        "timing_includes":"DNS, connection, transfer, redirects, retries and protective pacing; sub-resources are not downloaded",
        "note":"Set browser=true for real browser rendering metrics (requires the optional worker)"})))
}

const SHORTENERS: &[&str] = &[
    "bit.ly",
    "tinyurl.com",
    "t.co",
    "goo.gl",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "cutt.ly",
    "rebrand.ly",
    "shorturl.at",
    "rb.gy",
    "t.ly",
    "s.id",
];
const RISKY_TLDS: &[&str] = &[
    "zip", "mov", "top", "xyz", "click", "country", "gq", "tk", "ml", "cf", "ga", "work", "support", "loan", "rest", "buzz",
];
const LURE_WORDS: &[&str] = &[
    "login", "signin", "verify", "account", "update", "secure", "wallet", "password", "banking", "confirm", "invoice", "unlock", "recover",
];

/// Static URL heuristics. Every point added carries a human-readable reason.
pub fn url_heuristics(url: &url::Url) -> (u32, Vec<String>) {
    let host = url.host_str().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    let mut score = 0;
    let mut reasons = Vec::new();
    let mut add = |points: u32, reason: &str| {
        score += points;
        reasons.push(format!("+{points} {reason}"));
    };
    if url.scheme() == "http" {
        add(15, "unencrypted http scheme");
    }
    if !url.username().is_empty() || url.password().is_some() {
        add(35, "credentials or '@' in the authority can disguise the real host");
    }
    if url.host().is_some_and(|h| !matches!(h, url::Host::Domain(_))) {
        add(20, "IP address used instead of a domain name");
    }
    if host.split('.').any(|label| label.starts_with("xn--")) {
        add(20, "internationalized (punycode) domain may imitate another name");
    }
    if host.split('.').count() > 4 {
        add(10, "deeply nested subdomains");
    }
    if host.matches('-').count() >= 3 {
        add(10, "many hyphens in the host name");
    }
    if let Some(tld) = psl::suffix_str(&host)
        && RISKY_TLDS.contains(&tld)
    {
        add(10, "top-level domain frequently seen in abuse reports");
    }
    if SHORTENERS.contains(&host.as_str()) {
        add(15, "URL shortener hides the destination");
    }
    let lowered = url.as_str().to_ascii_lowercase();
    let lures = LURE_WORDS.iter().filter(|w| lowered.contains(*w)).count();
    if lures >= 2 {
        add(15, "multiple credential-lure keywords");
    } else if lures == 1 {
        add(5, "credential-lure keyword");
    }
    if url.as_str().len() > 200 {
        add(10, "unusually long URL");
    }
    if lowered.matches('%').count() > 10 {
        add(10, "heavy percent-encoding");
    }
    if url.port().is_some() {
        add(10, "non-default port");
    }
    (score.min(100), reasons)
}

#[derive(Deserialize)]
pub struct UrlCheckInput {
    pub url: String,
    #[serde(default = "yes")]
    pub follow_redirects: bool,
}
fn yes() -> bool {
    true
}

pub async fn url_check(State(app): State<App>, Json(input): Json<UrlCheckInput>) -> Result<Json<Value>> {
    if input.url.len() > 2048 {
        return Err(invalid("URL exceeds 2048 bytes"));
    }
    let Ok(url) = url::Url::parse(input.url.trim()) else {
        return Ok(Json(json!({"valid":false,"reason":"Not an absolute URL"})));
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(Json(
            json!({"valid":false,"reason":"Only http and https URLs are analysed","scheme":url.scheme()}),
        ));
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let (mut score, mut reasons) = url_heuristics(&url);
    let domain = json!({"host":host,"registrable_domain":psl::domain_str(&host),"public_suffix":psl::suffix_str(&host)});
    let mut inspection = json!({"fetched":false});
    if input.follow_redirects {
        match ozy_core::net::validate_url(url.as_str()) {
            Ok(safe) => {
                domain_budget(&app, safe.as_str()).await?;
                match app.http.fetch(safe.as_str(), false).await {
                    Ok(response) => {
                        let downgrade = url.scheme() == "https" && response.redirects.iter().any(|r| r.starts_with("http:"));
                        if downgrade {
                            score += 30;
                            reasons.push("+30 HTTPS downgraded to HTTP during redirects".into());
                        }
                        if response.redirects.len() > 2 {
                            score += 10;
                            reasons.push("+10 more than two redirects".into());
                        }
                        let final_host = url::Url::parse(&response.url)
                            .ok()
                            .and_then(|u| u.host_str().map(str::to_owned))
                            .unwrap_or_default();
                        if psl::domain_str(&final_host) != psl::domain_str(&host) {
                            score += 10;
                            reasons.push("+10 redirects to a different registrable domain".into());
                        }
                        inspection = json!({"fetched":true,"status":response.status,"final_url":response.url,"redirects":response.redirects,"elapsed_ms":response.elapsed_ms});
                    }
                    Err(error) => inspection = json!({"fetched":false,"error":error.to_string()}),
                }
            }
            Err(error) => {
                inspection = json!({"fetched":false,"error":error.to_string(),"note":"Destination is not fetched by policy (private, non-standard port or credentials)"})
            }
        }
    }
    let score = score.min(100);
    Ok(Json(json!({"valid":true,"url":url,"domain":domain,"risk_score":score,
        "risk_level":match score {0..=19=>"low",20..=49=>"medium",_=>"high"},
        "reasons":reasons,"redirect_inspection":inspection,"method":"transparent heuristics v2","malware_detection":false,
        "disclaimer":"Heuristic only. A low score does not establish safety; no reputation or malware database is queried."})))
}

const DISPOSABLE: &str = include_str!("../data/disposable_domains.txt");
const COMMON_PROVIDERS: &[&str] = &[
    "gmail.com",
    "yahoo.com",
    "hotmail.com",
    "outlook.com",
    "icloud.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "live.com",
    "msn.com",
    "gmx.com",
    "yandex.com",
    "mail.com",
    "zoho.com",
];
const ROLE_ACCOUNTS: &[&str] = &[
    "admin",
    "administrator",
    "info",
    "support",
    "sales",
    "contact",
    "noreply",
    "no-reply",
    "postmaster",
    "webmaster",
    "hostmaster",
    "abuse",
    "billing",
    "help",
    "office",
    "team",
    "hello",
    "marketing",
    "security",
];

pub fn is_disposable(domain: &str) -> bool {
    DISPOSABLE
        .lines()
        .map(str::trim)
        .any(|d| !d.is_empty() && !d.starts_with('#') && (domain == d || domain.ends_with(&format!(".{d}"))))
}

pub fn typo_suggestion(domain: &str) -> Option<&'static str> {
    if COMMON_PROVIDERS.contains(&domain) {
        return None;
    }
    COMMON_PROVIDERS
        .iter()
        .copied()
        .filter(|p| strsim::damerau_levenshtein(domain, p) <= 2)
        .min_by_key(|p| strsim::damerau_levenshtein(domain, p))
}

#[derive(Deserialize)]
pub struct EmailInput {
    pub email: String,
}

pub async fn email_validate(Json(input): Json<EmailInput>) -> Result<Json<Value>> {
    let email = input.email.trim().to_lowercase();
    let valid = email.len() <= 254 && email.is_ascii() && email_address::EmailAddress::is_valid(&email);
    if !valid {
        return Ok(Json(json!({"email":email,"syntax_valid":false,"deliverability":"invalid_syntax"})));
    }
    let (local, domain) = email.rsplit_once('@').ok_or_else(|| invalid("Invalid email"))?;
    let domain = domain.trim_end_matches('.').to_owned();
    use hickory_resolver::{TokioAsyncResolver, config::*};
    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_secs(2);
    opts.attempts = 1;
    let resolver = match hickory_resolver::system_conf::read_system_conf() {
        Ok((config, _)) => TokioAsyncResolver::tokio(config, opts),
        Err(_) => TokioAsyncResolver::tokio(ResolverConfig::cloudflare(), opts),
    };
    let timeout = Duration::from_secs(4);
    let (mx, a, spf, dmarc) = tokio::join!(
        tokio::time::timeout(timeout, resolver.mx_lookup(domain.as_str())),
        tokio::time::timeout(timeout, resolver.lookup_ip(domain.as_str())),
        tokio::time::timeout(timeout, resolver.txt_lookup(domain.as_str())),
        tokio::time::timeout(timeout, resolver.txt_lookup(format!("_dmarc.{domain}"))),
    );
    let mut dns_status = "resolved";
    let records: Vec<Value> = match mx {
        Ok(Ok(mx)) => mx
            .iter()
            .take(20)
            .map(|r| json!({"priority":r.preference(),"exchange":r.exchange().to_utf8()}))
            .collect(),
        Ok(Err(e)) if matches!(e.kind(), hickory_resolver::error::ResolveErrorKind::NoRecordsFound { .. }) => vec![],
        _ => {
            dns_status = "dns_error";
            vec![]
        }
    };
    let has_address = matches!(a, Ok(Ok(ref ips)) if ips.iter().next().is_some());
    let txt = |result: std::result::Result<std::result::Result<hickory_resolver::lookup::TxtLookup, _>, _>, prefix: &str| -> bool {
        matches!(result, Ok(Ok(ref txt)) if txt.iter().any(|r| r.to_string().to_ascii_lowercase().starts_with(prefix)))
    };
    let spf = txt(spf, "v=spf1");
    let dmarc = txt(dmarc, "v=dmarc1");
    let null_mx = records.iter().any(|r| r["exchange"] == ".");
    let deliverability = if null_mx {
        "domain_rejects_mail"
    } else if !records.is_empty() {
        "domain_accepts_mail"
    } else if has_address {
        "implicit_mx_only"
    } else if dns_status == "dns_error" {
        "unknown"
    } else {
        "domain_has_no_mail_records"
    };
    Ok(Json(json!({"email":email,"syntax_valid":true,"local_part":local,"domain":domain,
        "mx_records":records,"dns_status":dns_status,"has_address_records":has_address,
        "spf_record":spf,"dmarc_record":dmarc,
        "disposable":is_disposable(&domain),"disposable_list":"built-in list; not exhaustive",
        "role_account":ROLE_ACCOUNTS.contains(&local),
        "did_you_mean":typo_suggestion(&domain).map(|d| format!("{local}@{d}")),
        "deliverability":deliverability,"mailbox_verified":false,
        "limitations":["No individual mailbox is probed (no SMTP RCPT checks)","Domain-level indicators only"]})))
}

/// Accepts JSON or multipart/form-data. Multipart file parts become `files`;
/// an `options` text part (JSON) becomes `fields`.
pub struct Upload {
    pub files: Vec<Vec<u8>>,
    pub fields: Value,
}

pub async fn read_upload(request: Request, max_files: usize) -> Result<Upload> {
    let multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    if !multipart {
        let bytes = axum::body::to_bytes(request.into_body(), 3 * 1024 * 1024)
            .await
            .map_err(|_| invalid("Request body exceeds 3 MiB"))?;
        let fields = serde_json::from_slice(&bytes).map_err(|_| invalid("Body must be JSON or multipart/form-data"))?;
        return Ok(Upload { files: vec![], fields });
    }
    let mut form = Multipart::from_request(request, &())
        .await
        .map_err(|_| invalid("Invalid multipart body"))?;
    let mut upload = Upload {
        files: vec![],
        fields: json!({}),
    };
    while let Some(field) = form.next_field().await.map_err(|_| invalid("Invalid multipart body"))? {
        let is_file = field.file_name().is_some();
        let name = field.name().unwrap_or("").to_owned();
        let bytes = field.bytes().await.map_err(|_| invalid("Upload exceeds the size limit"))?;
        if is_file {
            if upload.files.len() >= max_files {
                return Err(invalid("Too many files"));
            }
            upload.files.push(bytes.to_vec());
        } else if name == "options" {
            upload.fields = serde_json::from_slice(&bytes).map_err(|_| invalid("options must be JSON"))?;
        }
    }
    Ok(upload)
}

pub fn image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// Validated, normalised OCR options forwarded to the worker.
pub fn ocr_options(raw: &Value) -> Result<Value> {
    let threshold = match raw.get("threshold") {
        None | Some(Value::Null) => json!(null),
        Some(Value::String(s)) if s == "otsu" => json!("otsu"),
        Some(Value::Number(n)) if n.as_u64().is_some_and(|v| v <= 255) => json!(n),
        _ => return Err(invalid("threshold must be 0-255 or \"otsu\"")),
    };
    let scale = raw.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
    if !(0.25..=4.0).contains(&scale) {
        return Err(invalid("scale must be between 0.25 and 4"));
    }
    let psm = raw.get("psm").and_then(Value::as_u64).unwrap_or(7);
    if ![3, 6, 7, 8, 11, 13].contains(&psm) {
        return Err(invalid("psm must be one of 3, 6, 7, 8, 11, 13"));
    }
    let whitelist = raw.get("whitelist").and_then(Value::as_str).unwrap_or("");
    if whitelist.len() > 128 || !whitelist.chars().all(|c| c.is_ascii_graphic()) {
        return Err(invalid("whitelist must be up to 128 printable ASCII characters"));
    }
    Ok(json!({"grayscale":raw.get("grayscale").and_then(Value::as_bool).unwrap_or(true),
        "threshold":threshold,"scale":scale,"psm":psm,"whitelist":whitelist,"lang":"eng"}))
}

pub async fn ocr(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    let upload = read_upload(request, 5).await?;
    if upload.fields.get("authorized_testing").and_then(Value::as_bool) != Some(true) {
        return Err(invalid(
            "authorized_testing=true is required: only user-owned test images and authorized test environments",
        ));
    }
    let mut images = upload.files;
    if let Some(list) = upload.fields.get("images").and_then(Value::as_array) {
        for item in list {
            let data = item
                .as_str()
                .or_else(|| item.get("data").and_then(Value::as_str))
                .ok_or_else(|| invalid("images[] must be base64 strings"))?;
            images.push(B64.decode(data.trim()).map_err(|_| invalid("Invalid base64 image"))?);
        }
    }
    if images.is_empty() || images.len() > 5 {
        return Err(invalid("Provide 1-5 images"));
    }
    let mut formats = Vec::new();
    for image in &images {
        if image.len() > 1024 * 1024 {
            return Err(invalid("Each image must be at most 1 MiB"));
        }
        formats.push(image_format(image).ok_or_else(|| invalid("Images must be PNG, JPEG or WebP"))?);
    }
    let options = ocr_options(upload.fields.get("options").unwrap_or(&upload.fields))?;
    let encoded: Vec<String> = images.iter().map(|i| B64.encode(i)).collect();
    let Json(mut result) = worker(&app, "ocr", json!({"images":encoded,"options":options})).await?;
    result["formats"] = json!(formats);
    result["notice"] = json!("For authorized testing only. No third-party CAPTCHA compatibility is claimed.");
    Ok(Json(result))
}

pub fn screenshot_request(input: &Value) -> Result<Value> {
    let url = input.get("url").and_then(Value::as_str).ok_or_else(|| invalid("url is required"))?;
    let url = ozy_core::net::validate_url(url)?;
    let device = input.get("device").and_then(Value::as_str).unwrap_or("desktop");
    let (mut width, mut height, mobile, scale) = match device {
        "desktop" => (1440, 900, false, 1.0),
        "mobile" => (390, 844, true, 2.0),
        _ => return Err(invalid("device must be desktop or mobile")),
    };
    if let Some(w) = input.get("width").and_then(Value::as_u64) {
        width = w;
    }
    if let Some(h) = input.get("height").and_then(Value::as_u64) {
        height = h;
    }
    if !(320..=2560).contains(&width) || !(240..=2560).contains(&height) {
        return Err(invalid("width must be 320-2560 and height 240-2560"));
    }
    let format = input.get("format").and_then(Value::as_str).unwrap_or("png");
    if !matches!(format, "png" | "jpeg" | "webp") {
        return Err(invalid("format must be png, jpeg or webp"));
    }
    let quality = input.get("quality").and_then(Value::as_u64).unwrap_or(80);
    if !(1..=100).contains(&quality) {
        return Err(invalid("quality must be 1-100"));
    }
    Ok(
        json!({"url":url,"width":width,"height":height,"mobile":mobile,"device_scale_factor":scale,
        "full_page":input.get("full_page").and_then(Value::as_bool).unwrap_or(false),"format":format,"quality":quality}),
    )
}

pub async fn screenshot(State(app): State<App>, Json(input): Json<Value>) -> Result<Json<Value>> {
    let request = screenshot_request(&input)?;
    domain_budget(&app, request["url"].as_str().unwrap_or_default()).await?;
    worker(&app, "screenshot", request).await
}

#[derive(Default, Serialize)]
pub struct ParsedPdf {
    pub version: String,
    pub page_count: usize,
    pub metadata: BTreeMap<String, String>,
    pub pages: Vec<Value>,
    #[serde(skip)]
    pub ocr_images: Vec<(usize, Vec<u8>)>,
}

fn pdf_string(object: &lopdf::Object) -> Option<String> {
    let lopdf::Object::String(bytes, _) = object else { return None };
    let text = if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    };
    Some(ozy_core::bounded_text(text.trim(), 1024))
}

/// Pure-Rust parsing; runs on a blocking thread with a timeout and panic isolation.
pub fn parse_pdf(bytes: &[u8], max_pages: usize) -> std::result::Result<ParsedPdf, String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|_| "The file is not a readable PDF".to_owned())?;
    if doc.is_encrypted() {
        return Err("Encrypted PDFs are not supported".into());
    }
    let mut parsed = ParsedPdf {
        version: doc.version.clone(),
        ..Default::default()
    };
    if let Ok(info) = doc
        .trailer
        .get(b"Info")
        .and_then(|o| o.as_reference())
        .and_then(|id| doc.get_dictionary(id))
    {
        for key in [
            "Title",
            "Author",
            "Subject",
            "Keywords",
            "Creator",
            "Producer",
            "CreationDate",
            "ModDate",
        ] {
            if let Some(value) = info.get(key.as_bytes()).ok().and_then(pdf_string).filter(|v| !v.is_empty()) {
                parsed.metadata.insert(key.to_ascii_lowercase(), value);
            }
        }
    }
    let pages = doc.get_pages();
    parsed.page_count = pages.len();
    let texts = pdf_extract::extract_text_from_mem_by_pages(bytes).map_err(|_| "Text extraction failed for this PDF".to_owned())?;
    for (index, ((number, page_id), text)) in pages.iter().zip(texts).take(max_pages).enumerate() {
        let text = ozy_core::bounded_text(text.trim(), 50_000);
        let needs_ocr = text.chars().filter(|c| !c.is_whitespace()).count() < 16;
        if needs_ocr && parsed.ocr_images.len() < 5 {
            for image in doc.get_page_images(*page_id).unwrap_or_default() {
                let jpeg = image.filters.as_ref().is_some_and(|f| f.iter().any(|f| f == "DCTDecode"));
                if jpeg && image.content.len() <= 1024 * 1024 && parsed.ocr_images.len() < 5 {
                    parsed.ocr_images.push((index, image.content.to_vec()));
                }
            }
        }
        parsed
            .pages
            .push(json!({"page":number,"text":text,"characters":text.chars().count(),"needs_ocr":needs_ocr}));
    }
    Ok(parsed)
}

pub fn pdf_markdown(parsed: &ParsedPdf) -> String {
    let mut out = String::new();
    if let Some(title) = parsed.metadata.get("title") {
        out.push_str(&format!("# {title}\n\n"));
    }
    for page in &parsed.pages {
        out.push_str(&format!("## Page {}\n\n{}\n\n", page["page"], page["text"].as_str().unwrap_or("")));
    }
    out
}

pub async fn pdf(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    let upload = read_upload(request, 1).await?;
    let options = upload.fields;
    let bytes = if let Some(file) = upload.files.into_iter().next() {
        file
    } else if let Some(data) = options.get("data").and_then(Value::as_str) {
        B64.decode(data.trim()).map_err(|_| invalid("data must be base64"))?
    } else if let Some(url) = options.get("url").and_then(Value::as_str) {
        domain_budget(&app, url).await?;
        let response = app.http.fetch(url, true).await?;
        if response.status != 200 {
            return Err(Failure(
                StatusCode::BAD_GATEWAY,
                "upstream_status",
                format!("PDF URL returned HTTP {}", response.status),
            ));
        }
        response.body
    } else {
        return Err(invalid("Provide a PDF as a multipart file, base64 data, or url"));
    };
    if !bytes.starts_with(b"%PDF-") {
        return Err(invalid("The document is not a PDF"));
    }
    if bytes.len() > 3 * 1024 * 1024 {
        return Err(invalid("PDF must be at most 3 MiB"));
    }
    let max_pages = options.get("max_pages").and_then(Value::as_u64).unwrap_or(50).clamp(1, 200) as usize;
    let format = options.get("format").and_then(Value::as_str).unwrap_or("json");
    if !matches!(format, "json" | "markdown") {
        return Err(invalid("format must be json or markdown"));
    }
    let job = tokio::task::spawn_blocking(move || parse_pdf(&bytes, max_pages));
    let mut parsed = match tokio::time::timeout(Duration::from_secs(25), job).await {
        Err(_) => {
            return Err(Failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "pdf_timeout",
                "PDF processing exceeded 25 seconds".into(),
            ));
        }
        Ok(Err(_)) => {
            return Err(Failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "pdf_unprocessable",
                "The PDF could not be processed safely".into(),
            ));
        }
        Ok(Ok(Err(message))) => return Err(Failure(StatusCode::UNPROCESSABLE_ENTITY, "pdf_unprocessable", message)),
        Ok(Ok(Ok(parsed))) => parsed,
    };
    let scanned = parsed.pages.iter().filter(|p| p["needs_ocr"] == true).count();
    let ocr_status = if options.get("ocr").and_then(Value::as_bool) != Some(true) {
        if scanned > 0 { "not_requested" } else { "not_needed" }
    } else if parsed.ocr_images.is_empty() {
        if scanned > 0 { "no_supported_images" } else { "not_needed" }
    } else {
        let images: Vec<String> = parsed.ocr_images.iter().map(|(_, b)| B64.encode(b)).collect();
        match worker(&app, "ocr", json!({"images":images,"options":ocr_options(&json!({"psm":3}))?})).await {
            Ok(Json(result)) => {
                for ((page, _), recognised) in parsed.ocr_images.iter().zip(result["results"].as_array().into_iter().flatten()) {
                    if let Some(entry) = parsed.pages.get_mut(*page) {
                        entry["ocr_text"] = recognised["text"].clone();
                        entry["ocr_confidence"] = recognised["confidence"].clone();
                    }
                }
                "completed"
            }
            Err(Failure(_, "worker_unavailable", _)) => "worker_unavailable",
            Err(_) => "worker_failed",
        }
    };
    let markdown = (format == "markdown").then(|| pdf_markdown(&parsed));
    Ok(Json(
        json!({"version":parsed.version,"page_count":parsed.page_count,"pages_returned":parsed.pages.len(),
        "metadata":parsed.metadata,"pages":if format == "json" {json!(parsed.pages)} else {json!(null)},
        "markdown":markdown,"scanned_pages":scanned,"ocr_status":ocr_status}),
    ))
}

pub fn worker_unavailable(message: &str) -> Failure {
    Failure(StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable", message.into())
}

pub async fn worker(app: &App, kind: &str, input: Value) -> Result<Json<Value>> {
    let url = app.worker_url.as_deref().ok_or_else(|| {
        worker_unavailable("This feature requires the optional self-hosted OZY worker, which is not configured on this deployment.")
    })?;
    let secret = app
        .worker_secret
        .as_deref()
        .ok_or_else(|| worker_unavailable("Worker authentication is not configured"))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|_| worker_unavailable("Worker client unavailable"))?;
    let response = client
        .post(format!("{}/v1/{kind}", url.trim_end_matches('/')))
        .bearer_auth(secret)
        .json(&input)
        .send()
        .await
        .map_err(|_| Failure(StatusCode::BAD_GATEWAY, "worker_failed", "Worker did not respond".into()))?;
    let status = response.status();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Failure(StatusCode::BAD_GATEWAY, "worker_failed", "Worker response interrupted".into()))?;
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err(Failure(
                StatusCode::BAD_GATEWAY,
                "worker_failed",
                "Worker response exceeds 4 MiB".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let parsed: Value =
        serde_json::from_slice(&body).map_err(|_| Failure(StatusCode::BAD_GATEWAY, "worker_failed", "Invalid worker response".into()))?;
    if !status.is_success() {
        // Worker errors use the same envelope; surface its code but never internal details.
        let message = parsed["error"]["message"]
            .as_str()
            .unwrap_or("Worker rejected the request")
            .to_owned();
        let code = if status.is_client_error() {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        return Err(Failure(code, "worker_failed", message));
    }
    Ok(Json(parsed))
}

pub async fn worker_health(app: &App) -> Value {
    let Some(url) = app.worker_url.as_deref() else {
        return json!({"configured":false,"reachable":false});
    };
    let reachable = async {
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().ok()?;
        let response = client.get(format!("{}/health", url.trim_end_matches('/'))).send().await.ok()?;
        response.json::<Value>().await.ok()
    }
    .await;
    json!({"configured":true,"reachable":reachable.is_some(),"capabilities":reachable.map(|v| v["capabilities"].clone())})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_heuristics_explain_every_point() {
        let (score, reasons) =
            url_heuristics(&url::Url::parse("http://user@xn--pple-43d.com.login-verify-account.secure.example.zip/").unwrap());
        assert!(score >= 70, "{score}");
        assert!(reasons.iter().all(|r| r.starts_with('+')));
        let (score, _) = url_heuristics(&url::Url::parse("https://www.rust-lang.org/").unwrap());
        assert_eq!(score, 0);
    }
    #[test]
    fn disposable_and_typos() {
        assert!(is_disposable("mailinator.com"));
        assert!(is_disposable("sub.yopmail.com"));
        assert!(!is_disposable("gmail.com"));
        assert_eq!(typo_suggestion("gmial.com"), Some("gmail.com"));
        assert_eq!(typo_suggestion("gmail.com"), None);
        assert_eq!(typo_suggestion("company-example.org"), None);
    }
    #[test]
    fn technology_detection_reports_evidence() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("server".to_owned(), "nginx/1.25".to_owned());
        headers.insert("x-vercel-id".to_owned(), "iad1::abc".to_owned());
        let found = detect_technologies(
            "<meta name='generator' content='WordPress 6.6'><script src='/wp-includes/js/jquery/jquery.min.js'></script>",
            &headers,
        );
        let names: Vec<_> = found.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in ["WordPress", "jQuery", "Vercel", "Nginx"] {
            assert!(names.contains(&expected), "{expected} missing from {names:?}");
        }
        let wordpress = found.iter().find(|t| t["name"] == "WordPress").unwrap();
        assert!(wordpress["confidence"].as_f64().unwrap() > 0.95);
    }
    #[test]
    fn image_sniffing_and_ocr_options() {
        assert_eq!(image_format(b"\x89PNG\r\n\x1a\nxxxx"), Some("png"));
        assert_eq!(image_format(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpeg"));
        assert_eq!(image_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_format(b"GIF89a"), None);
        assert!(ocr_options(&json!({"threshold":"otsu","scale":2})).is_ok());
        assert!(ocr_options(&json!({"threshold":300})).is_err());
        assert!(ocr_options(&json!({"psm":99})).is_err());
    }
    #[test]
    fn screenshot_validation() {
        let ok = screenshot_request(&json!({"url":"https://example.com","device":"mobile","format":"webp"})).unwrap();
        assert_eq!(ok["width"], 390);
        assert_eq!(ok["mobile"], true);
        assert!(screenshot_request(&json!({"url":"http://169.254.169.254/"})).is_err());
        assert!(screenshot_request(&json!({"url":"https://example.com","width":10000})).is_err());
        assert!(screenshot_request(&json!({"url":"https://example.com","format":"gif"})).is_err());
    }
}
