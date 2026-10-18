//! End-to-end tests of the real router against a real PostgreSQL database and the
//! public internet. Runs only when a database is configured:
//!   repo-root `.env` (OZY_ENCRYPTION_KEY, CRON_SECRET) + `.env.preview.local` (POSTGRES_URL),
//! or the same variables exported. Otherwise each test prints a skip notice and passes.
//! Apply migrations first: `cargo run --bin ozy-ops -- --env-file .env.preview.local migrate`.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:3000";

async fn setup(per_minute: i32) -> Option<(Router, ozy_api::App)> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("ozy_api=info")
        .with_test_writer()
        .try_init();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let _ = dotenvy::from_path(root.join(".env"));
    let _ = dotenvy::from_path(root.join(".env.preview.local"));
    if std::env::var("POSTGRES_URL").is_err() || std::env::var("OZY_ENCRYPTION_KEY").is_err() {
        eprintln!("SKIPPED: no test database configured");
        return None;
    }
    // SAFETY: tests set process env before any app threads read it.
    unsafe {
        std::env::set_var("PUBLIC_ORIGIN", ORIGIN);
        std::env::remove_var("OZY_WORKER_URL");
    }
    let mut app = ozy_api::from_env().await.expect("app config");
    app.limits = ozy_api::Limits {
        per_minute,
        per_day: 1000,
        platform_per_day: 100_000,
    };
    Some((ozy_api::router(app.clone()), app))
}

struct Client {
    router: Router,
    cookie: Option<String>,
    key: Option<String>,
    ip: String,
}

struct Reply {
    status: StatusCode,
    json: Value,
    text: String,
    headers: axum::http::HeaderMap,
}

impl Client {
    async fn send(&mut self, method: &str, path: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Reply {
        let mut request = Request::builder().method(method).uri(path).header("x-real-ip", &self.ip);
        if let Some(cookie) = &self.cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if let Some(key) = &self.key {
            request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
        }
        for (k, v) in extra {
            request = request.header(*k, *v);
        }
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        if let Some(cookie) = headers.get(header::SET_COOKIE).and_then(|v| v.to_str().ok()) {
            let pair = cookie.split(';').next().unwrap().to_owned();
            self.cookie = if pair.ends_with('=') { None } else { Some(pair) };
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Reply {
            status,
            json: serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
            headers,
        }
    }
    async fn get(&mut self, path: &str) -> Reply {
        self.send("GET", path, None, &[]).await
    }
    async fn post(&mut self, path: &str, body: Value) -> Reply {
        self.send("POST", path, Some(body), &[("origin", ORIGIN)]).await
    }
}

fn unique() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// One-page PDF with real text, built with lopdf.
fn sample_pdf(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"});
    let resources_id = doc.add_object(dictionary! {"Font" => dictionary! {"F1" => font_id}});
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(dictionary! {"Type" => "Page", "Parent" => pages_id, "Contents" => content_id});
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {"Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()]}),
    );
    let info_id = doc.add_object(dictionary! {"Title" => Object::string_literal("OZY test document")});
    let catalog_id = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

async fn pace() {
    // Respect the platform's own one-request-per-second-per-host rule.
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

#[tokio::test]
async fn full_platform_flow() {
    let Some((router, app)) = setup(1000).await else { return };
    let ip = format!("198.51.100.{}", rand_octet());
    let mut c = Client {
        router,
        cookie: None,
        key: None,
        ip,
    };
    let email = format!("it-{}@example.com", unique());
    let clock = std::time::Instant::now();
    let password = "integration-test-password";

    // Public pages, assets and docs
    eprintln!("[{:>6.1?}] Public pages, assets and docs", clock.elapsed());
    let home = c.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.text.contains("OZY"));
    assert!(home.headers.get("content-security-policy").is_some());
    assert!(home.headers.get("x-request-id").is_some());
    for path in [
        "/apis",
        "/apis/scrape",
        "/playground",
        "/docs",
        "/status",
        "/changelog",
        "/login",
        "/register",
        "/dashboard/keys",
        "/assets/app.js",
        "/assets/style.css",
    ] {
        assert_eq!(c.get(path).await.status, StatusCode::OK, "{path}");
    }
    assert_eq!(c.get("/does-not-exist").await.status, StatusCode::NOT_FOUND);
    let api_missing = c.get("/api/v1/does-not-exist").await;
    assert_eq!(api_missing.status, StatusCode::NOT_FOUND);
    assert_eq!(api_missing.json["error"]["code"], "not_found");
    let openapi = c.get("/openapi.json").await;
    assert_eq!(openapi.json["paths"].as_object().unwrap().len(), 12);
    let health = c.get("/api/v1/health").await;
    assert_eq!(health.json["database"], true, "{}", health.text);
    let status = c.get("/api/v1/status").await;
    assert_eq!(status.json["apis"].as_array().unwrap().len(), 12);
    assert_eq!(status.json["apis"][0]["availability"], "requires_worker");

    // Authentication
    eprintln!("[{:>6.1?}] Authentication", clock.elapsed());
    assert_eq!(c.get("/api/v1/scrape").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        c.post("/api/v1/scrape", json!({"html":"<p>x</p>"})).await.status,
        StatusCode::UNAUTHORIZED
    );
    let weak = c.post("/api/v1/auth/register", json!({"email":email,"password":"short"})).await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    let registered = c
        .post(
            "/api/v1/auth/register",
            json!({"email":email,"password":password,"name":"Integration"}),
        )
        .await;
    assert_eq!(registered.status, StatusCode::OK, "{}", registered.text);
    assert!(c.cookie.is_some());
    let duplicate = c.post("/api/v1/auth/register", json!({"email":email,"password":password})).await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    c.cookie = None;
    let wrong = c
        .post("/api/v1/auth/login", json!({"email":email,"password":"definitely-wrong-password"}))
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let login = c.post("/api/v1/auth/login", json!({"email":email,"password":password})).await;
    assert_eq!(login.status, StatusCode::OK);
    let me = c.get("/api/v1/account").await;
    assert_eq!(me.json["user"]["email"], email.as_str());

    // CSRF: session writes need our Origin
    eprintln!("[{:>6.1?}] CSRF: session writes need our Origin", clock.elapsed());
    let no_origin = c.send("POST", "/api/v1/keys", Some(json!({"name":"x"})), &[]).await;
    assert_eq!(no_origin.status, StatusCode::FORBIDDEN);
    assert_eq!(no_origin.json["error"]["code"], "csrf_rejected");

    // API keys
    eprintln!("[{:>6.1?}] API keys", clock.elapsed());
    let key = c.post("/api/v1/keys", json!({"name":"full"})).await;
    assert_eq!(key.status, StatusCode::OK, "{}", key.text);
    let full_key = key.json["key"].as_str().unwrap().to_owned();
    assert!(full_key.starts_with("ozy_"));
    let limited = c
        .post("/api/v1/keys", json!({"name":"preview only","permissions":["preview"]}))
        .await;
    let limited_key = limited.json["key"].as_str().unwrap().to_owned();
    let listed = c.get("/api/v1/keys").await;
    assert_eq!(listed.json["keys"].as_array().unwrap().len(), 2);
    assert!(!listed.text.contains(&full_key), "plaintext key must never be listed");
    let session_cookie = c.cookie.take();

    c.key = Some(limited_key.clone());
    let denied = c.post("/api/v1/email/validate", json!({"email":"a@example.com"})).await;
    assert_eq!(denied.json["error"]["code"], "permission_denied");
    assert_eq!(c.get("/api/v1/keys").await.json["error"]["code"], "session_required");
    c.key = Some("ozy_not-a-real-key".into());
    assert_eq!(
        c.post("/api/v1/scrape", json!({"html":"<p>x</p>"})).await.status,
        StatusCode::UNAUTHORIZED
    );
    c.key = Some(full_key.clone());

    // API 3: scraper with adaptive recovery (inline HTML, no network)
    eprintln!("[{:>6.1?}] API 3: scraper with adaptive recovery (i", clock.elapsed());
    let rule = json!([{"name":"price","selector":".price","adaptive":true},{"name":"link","selector":"//a[@id='buy']","kind":"attribute","attribute":"href"}]);
    let first = c.post("/api/v1/scrape", json!({"html":"<article><h2>Camera</h2><span class='price' data-currency='USD'>120</span><a id='buy' href='/b'>Buy</a></article>","rules":rule,"markdown":true})).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json["fields"]["price"]["match_type"], "exact");
    assert_eq!(first.json["fields"]["link"]["values"][0], "/b");
    assert!(first.json["markdown"].as_str().unwrap().contains("Camera"));
    let changed = c.post("/api/v1/scrape", json!({"html":"<main><article><h2>Camera</h2><span class='cost' data-currency='USD'>120</span></article></main>","rules":[{"name":"price","selector":".price","adaptive":true}]})).await;
    assert_eq!(changed.json["fields"]["price"]["match_type"], "recovered", "{}", changed.text);
    assert_eq!(changed.json["fields"]["price"]["values"][0], "120");
    let cached1 = c.post("/api/v1/scrape", json!({"html":"<p>c</p>","cache_seconds":60})).await;
    let cached2 = c.post("/api/v1/scrape", json!({"html":"<p>c</p>","cache_seconds":60})).await;
    assert_eq!(cached1.json["cached"], false);
    assert_eq!(cached2.json["cached"], true);
    assert_eq!(
        c.post(
            "/api/v1/scrape",
            json!({"html":"<p>x</p>","rules":[{"name":"bad","selector":"//p[contains(.,'x')]"}]})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );

    // SSRF protections
    eprintln!("[{:>6.1?}] SSRF protections", clock.elapsed());
    for target in [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1/",
        "http://localhost.localdomain.example.invalid/",
        "http://[::1]/",
        "http://2130706433/",
    ] {
        let blocked = c.post("/api/v1/scrape", json!({"url":target})).await;
        assert!(
            blocked.status == StatusCode::FORBIDDEN || blocked.status == StatusCode::BAD_REQUEST,
            "{target}: {}",
            blocked.text
        );
    }

    // API 10: email (DNS)
    eprintln!("[{:>6.1?}] API 10: email (DNS)", clock.elapsed());
    let t = std::time::Instant::now();
    let email_check = c.post("/api/v1/email/validate", json!({"email":"someone@gmial.com"})).await;
    assert_eq!(email_check.json["did_you_mean"], "someone@gmail.com");
    let gmail = c.post("/api/v1/email/validate", json!({"email":"someone@gmail.com"})).await;
    assert_eq!(gmail.json["deliverability"], "domain_accepts_mail", "{}", gmail.text);
    assert_eq!(
        c.post("/api/v1/email/validate", json!({"email":"x@mailinator.com"})).await.json["disposable"],
        true
    );
    assert_eq!(
        c.post("/api/v1/email/validate", json!({"email":"not-an-email"})).await.json["syntax_valid"],
        false
    );

    eprintln!("email {:?}", t.elapsed());
    // API 9: URL safety
    eprintln!("[{:>6.1?}] API 9: URL safety", clock.elapsed());
    let risky = c
        .post(
            "/api/v1/url/check",
            json!({"url":"http://user@10.0.0.1/login/verify","follow_redirects":true}),
        )
        .await;
    assert_eq!(risky.json["redirect_inspection"]["fetched"], false);
    assert!(risky.json["risk_score"].as_u64().unwrap() >= 50, "{}", risky.text);

    // Network-backed APIs against the toscrape.com scraping sandboxes
    eprintln!("[{:>6.1?}] Network-backed APIs against the toscrape", clock.elapsed());
    // (example.com asks not to be used for testing or monitoring).
    let t = std::time::Instant::now();
    let preview = c.send("GET", "/api/v1/preview?url=https://quotes.toscrape.com/", None, &[]).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.text);
    assert_eq!(preview.json["metadata"]["title"], "Quotes to Scrape");
    eprintln!("preview {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let intel = c
        .send("GET", "/api/v1/intelligence?url=https://books.toscrape.com/", None, &[])
        .await;
    eprintln!("intelligence {:?}", t.elapsed());
    assert_eq!(intel.status, StatusCode::OK, "{}", intel.text);
    assert!(
        intel.json["tls"]["certificate"]["subject_alt_names"]
            .as_array()
            .is_some_and(|s| !s.is_empty()),
        "{}",
        intel.json["tls"]
    );
    pace().await;
    let perf = c.post("/api/v1/performance", json!({"url":"https://quotes.toscrape.com/"})).await;
    assert_eq!(perf.json["measurement"], "http_fetch", "{}", perf.text);
    assert!(perf.json["html_bytes"].as_u64().unwrap() > 100);
    pace().await;
    let scraped = c.post("/api/v1/scrape", json!({"url":"https://books.toscrape.com/","rules":[{"name":"h1","selector":"h1"},{"name":"titles","selector":"article.product_pod h3 a","kind":"attribute","attribute":"title"}]})).await;
    assert_eq!(scraped.json["fields"]["h1"]["values"][0], "All products", "{}", scraped.text);
    assert_eq!(scraped.json["fields"]["titles"]["values"].as_array().unwrap().len(), 20);
    assert!(ozy_api::domain_budget(&app, "https://rate-limit-check.example.org/").await.is_ok());
    let limited = ozy_api::domain_budget(&app, "https://rate-limit-check.example.org/other").await;
    assert_eq!(limited.unwrap_err().1, "domain_rate_limited");

    // API 6: PDF (pure Rust)
    eprintln!("[{:>6.1?}] API 6: PDF (pure Rust)", clock.elapsed());
    use base64::Engine;
    let pdf = base64::engine::general_purpose::STANDARD.encode(sample_pdf("Hello OZY PDF"));
    let parsed = c.post("/api/v1/pdf/parse", json!({"data":pdf,"format":"markdown"})).await;
    assert_eq!(parsed.status, StatusCode::OK, "{}", parsed.text);
    assert_eq!(parsed.json["page_count"], 1);
    assert_eq!(parsed.json["metadata"]["title"], "OZY test document");
    assert!(
        parsed.json["markdown"].as_str().unwrap().contains("Hello OZY PDF"),
        "{}",
        parsed.text
    );
    let not_pdf = c.post("/api/v1/pdf/parse", json!({"data":"aGVsbG8="})).await;
    assert_eq!(not_pdf.status, StatusCode::BAD_REQUEST);
    let corrupt = base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4\n garbage without structure");
    assert_eq!(
        c.post("/api/v1/pdf/parse", json!({"data":corrupt})).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // Worker-backed APIs report the missing worker honestly
    eprintln!("[{:>6.1?}] Worker-backed APIs report the missing wo", clock.elapsed());
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let ocr = c
        .post("/api/v1/ocr/captcha", json!({"authorized_testing":true,"images":[png]}))
        .await;
    assert_eq!(ocr.json["error"]["code"], "worker_unavailable", "{}", ocr.text);
    assert_eq!(
        c.post("/api/v1/ocr/captcha", json!({"images":[png]})).await.status,
        StatusCode::BAD_REQUEST
    );
    let shot = c.post("/api/v1/screenshot", json!({"url":"https://example.org/"})).await;
    assert_eq!(shot.json["error"]["code"], "worker_unavailable", "{}", shot.text);

    // API 2: monitor
    eprintln!("[{:>6.1?}] API 2: monitor", clock.elapsed());
    pace().await;
    let monitor = c
        .post(
            "/api/v1/monitor",
            json!({"url":"https://books.toscrape.com/","interval_seconds":300,"selector":"h1"}),
        )
        .await;
    assert_eq!(monitor.status, StatusCode::OK, "{}", monitor.text);
    assert_eq!(monitor.json["monitor"]["last_available"], true);
    assert_eq!(monitor.json["history"].as_array().unwrap().len(), 1);
    let monitor_id = monitor.json["monitor"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.get(&format!("/api/v1/monitor/{monitor_id}")).await.json["stats"]["uptime_24h"],
        100.0
    );
    assert_eq!(
        c.post(
            "/api/v1/monitor",
            json!({"url":"https://books.toscrape.com/","interval_seconds":10})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );

    // API 11: website to API
    eprintln!("[{:>6.1?}] API 11: website to API", clock.elapsed());
    pace().await;
    let saved = c
        .post(
            "/api/v1/extract/create",
            json!({"name":"Books","url":"https://books.toscrape.com/","rules":[{"name":"title","selector":"h1"}],"refresh_seconds":600}),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(
        saved.json["result"]["fields"]["title"]["values"][0], "All products",
        "{}",
        saved.text
    );
    let saved_id = saved.json["id"].as_str().unwrap().to_owned();
    let read = c.get(&format!("/api/v1/extract/{saved_id}")).await;
    assert_eq!(read.json["usage"]["fetch_count"], 1);
    assert_eq!(c.get("/api/v1/extract").await.json["extractions"].as_array().unwrap().len(), 1);

    // Crawl with checkpoint
    eprintln!("[{:>6.1?}] Crawl with checkpoint", clock.elapsed());
    pace().await;
    let crawl = c.post("/api/v1/crawls", json!({"url":"https://quotes.toscrape.com/","depth":1,"max_pages":3,"concurrency":2,"rules":[{"name":"quotes","selector":".quote .text"}]})).await;
    assert_eq!(crawl.status, StatusCode::OK, "{}", crawl.text);
    assert_eq!(crawl.json["state"], "completed", "{}", crawl.text);
    let pages = crawl.json["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3, "{}", crawl.text);
    assert!(
        pages
            .iter()
            .any(|p| p["fields"]["quotes"]["values"].as_array().is_some_and(|v| v.len() == 10)),
        "{}",
        crawl.text
    );

    // API 12: webhook relay
    eprintln!("[{:>6.1?}] API 12: webhook relay", clock.elapsed());
    let relay = c
        .post(
            "/api/v1/webhooks",
            json!({"destination":"https://dev.itsozy.lol/api/v1/health","event_types":["order.*"]}),
        )
        .await;
    assert_eq!(relay.status, StatusCode::OK, "{}", relay.text);
    let relay_id = relay.json["id"].as_str().unwrap().to_owned();
    let secret = relay.json["secret"].as_str().unwrap().to_owned();
    assert_eq!(
        c.post("/api/v1/webhooks", json!({"destination":"http://example.com/"}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let hook_path = format!("/api/v1/hooks/{relay_id}");
    let anonymous_key = c.key.take();
    let body = json!({"type":"order.created","id":"evt_1"}).to_string();
    let unsigned = c.send("POST", &hook_path, Some(serde_json::from_str(&body).unwrap()), &[]).await;
    assert_eq!(unsigned.status, StatusCode::UNAUTHORIZED);
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let signature = ozy_auth::sign(&secret, &timestamp, body.as_bytes());
    let signed_headers = [
        ("x-ozy-timestamp", timestamp.as_str()),
        ("x-ozy-signature", signature.as_str()),
        ("x-ozy-event-id", "evt_1"),
    ];
    let accepted = c
        .send("POST", &hook_path, Some(serde_json::from_str(&body).unwrap()), &signed_headers)
        .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{}", accepted.text);
    assert!(
        matches!(accepted.json["state"].as_str(), Some("pending" | "delivered")),
        "{}",
        accepted.text
    );
    let replay = c
        .send("POST", &hook_path, Some(serde_json::from_str(&body).unwrap()), &signed_headers)
        .await;
    assert_eq!(replay.json["duplicate"], true);
    let other = json!({"type":"user.created"}).to_string();
    let other_sig = ozy_auth::sign(&secret, &timestamp, other.as_bytes());
    let filtered = c
        .send(
            "POST",
            &hook_path,
            Some(serde_json::from_str(&other).unwrap()),
            &[("x-ozy-timestamp", timestamp.as_str()), ("x-ozy-signature", other_sig.as_str())],
        )
        .await;
    assert_eq!(filtered.json["state"], "filtered", "{}", filtered.text);
    let stale = (chrono::Utc::now().timestamp() - 3600).to_string();
    let stale_sig = ozy_auth::sign(&secret, &stale, body.as_bytes());
    assert_eq!(
        c.send(
            "POST",
            &hook_path,
            Some(serde_json::from_str(&body).unwrap()),
            &[("x-ozy-timestamp", stale.as_str()), ("x-ozy-signature", stale_sig.as_str())]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    c.key = anonymous_key;
    let log = c.get(&format!("/api/v1/webhooks/{relay_id}/deliveries")).await;
    assert_eq!(log.json["deliveries"].as_array().unwrap().len(), 2, "{}", log.text);
    assert!(log.text.contains("attempts_log"));

    // Scheduler
    eprintln!("[{:>6.1?}] Scheduler", clock.elapsed());
    c.key = None;
    assert_eq!(
        c.send("POST", "/api/v1/internal/tick", None, &[("authorization", "Bearer wrong")])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let tick = c
        .send(
            "POST",
            "/api/v1/internal/tick",
            None,
            &[("authorization", &format!("Bearer {}", app.cron_secret))],
        )
        .await;
    assert_eq!(tick.status, StatusCode::OK, "{}", tick.text);

    // Dashboard data, then cleanup through the API
    eprintln!("[{:>6.1?}] Dashboard data, then cleanup through the", clock.elapsed());
    c.key = None;
    c.cookie = session_cookie;
    let logs = c.get("/api/v1/logs").await;
    assert!(logs.json["logs"].as_array().unwrap().len() > 10, "{}", logs.text);
    assert!(!c.get("/api/v1/usage").await.json["daily"].as_array().unwrap().is_empty());
    assert!(c.get("/api/v1/notifications").await.status.is_success());
    let key_id = c.get("/api/v1/keys").await.json["keys"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.send("DELETE", &format!("/api/v1/keys/{key_id}"), None, &[("origin", ORIGIN)])
            .await
            .json["revoked"],
        true
    );
    for path in [
        format!("/api/v1/monitor/{monitor_id}"),
        format!("/api/v1/extract/{saved_id}"),
        format!("/api/v1/webhooks/{relay_id}"),
    ] {
        assert_eq!(
            c.send("DELETE", &path, None, &[("origin", ORIGIN)]).await.json["deleted"],
            true,
            "{path}"
        );
    }
    let changed_pw = c
        .post(
            "/api/v1/account/password",
            json!({"current_password":password,"new_password":"another-long-password"}),
        )
        .await;
    assert_eq!(changed_pw.json["sign_in_required"], true);
    assert_eq!(
        c.get("/api/v1/account").await.status,
        StatusCode::UNAUTHORIZED,
        "password change must revoke sessions"
    );
    sqlx::query("DELETE FROM ozy.users WHERE email=$1")
        .bind(&email)
        .execute(&app.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn quotas_are_enforced() {
    let Some((router, app)) = setup(3).await else { return };
    let mut c = Client {
        router,
        cookie: None,
        key: None,
        ip: format!("203.0.113.{}", rand_octet()),
    };
    let email = format!("quota-{}@example.com", unique());
    assert_eq!(
        c.post("/api/v1/auth/register", json!({"email":email,"password":"quota-test-password"}))
            .await
            .status,
        StatusCode::OK
    );
    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(c.post("/api/v1/scrape", json!({"html":"<p>q</p>"})).await.status);
    }
    assert_eq!(&statuses[..3], &[StatusCode::OK; 3]);
    assert_eq!(statuses[3], StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("DELETE FROM ozy.users WHERE email=$1")
        .bind(&email)
        .execute(&app.db)
        .await
        .unwrap();
}

fn rand_octet() -> u8 {
    (uuid::Uuid::new_v4().as_bytes()[0] % 250) + 1
}
