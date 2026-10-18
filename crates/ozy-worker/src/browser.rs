//! Headless Chromium over the DevTools Protocol (feature `browser`).
//! Each job gets a fresh browser process and profile, and all traffic goes through
//! the validating loopback proxy.
use chromiumoxide::{
    Browser, BrowserConfig, Page,
    cdp::browser_protocol::{emulation::SetDeviceMetricsOverrideParams, page::CaptureScreenshotFormat},
    page::ScreenshotParams,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};

const MOBILE_UA: &str =
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36 OzyScrape/0.1";

pub struct Session {
    browser: Browser,
    handler: tokio::task::JoinHandle<()>,
    profile: std::path::PathBuf,
}

impl Session {
    pub async fn launch(proxy: SocketAddr, width: u32, height: u32) -> Result<Self, String> {
        let profile = std::env::temp_dir().join(format!("ozy-chrome-{}", uuid_like()));
        let mut config = BrowserConfig::builder()
            .new_headless_mode()
            .window_size(width, height)
            .user_data_dir(&profile)
            .request_timeout(Duration::from_secs(30))
            .args([
                format!("--proxy-server=http://{proxy}"),
                "--proxy-bypass-list=<-loopback>".into(),
                // Hostnames must reach the proxy; Chromium never resolves them itself.
                "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1".into(),
                "--disable-quic".into(),
                "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
                "--disable-background-networking".into(),
                "--disable-extensions".into(),
                "--disable-sync".into(),
                "--no-first-run".into(),
                "--mute-audio".into(),
                "--hide-scrollbars".into(),
            ]);
        if let Ok(path) = std::env::var("CHROME_PATH") {
            config = config.chrome_executable(path);
        }
        // Only inside a container, which is then the isolation boundary.
        if std::env::var("OZY_CHROME_NO_SANDBOX").is_ok_and(|v| v == "1") {
            config = config.no_sandbox();
        }
        let (browser, mut events) = Browser::launch(config.build()?)
            .await
            .map_err(|e| format!("Chromium failed to start: {e}"))?;
        let handler = tokio::spawn(async move { while events.next().await.is_some() {} });
        Ok(Self { browser, handler, profile })
    }

    pub async fn open(&self, url: &str, width: u32, height: u32, scale: f64, mobile: bool) -> Result<Page, String> {
        let page = self.browser.new_page("about:blank").await.map_err(|_| "Could not open a tab")?;
        page.execute(SetDeviceMetricsOverrideParams::new(width, height, scale, mobile))
            .await
            .map_err(|_| "Viewport rejected")?;
        if mobile {
            page.set_user_agent(MOBILE_UA).await.map_err(|_| "User agent rejected")?;
        }
        tokio::time::timeout(Duration::from_secs(25), page.goto(url))
            .await
            .map_err(|_| "Navigation timed out")?
            .map_err(|e| format!("Navigation failed: {e}"))?;
        // Let late layout and fonts settle without waiting for every analytics beacon.
        tokio::time::sleep(Duration::from_millis(800)).await;
        Ok(page)
    }

    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.browser.wait()).await;
        self.handler.abort();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    )
}

async fn navigation_status(page: &Page) -> Value {
    page.evaluate("(() => { const n = performance.getEntriesByType('navigation')[0]; return n ? n.responseStatus || null : null })()")
        .await
        .ok()
        .and_then(|r| r.value().cloned())
        .unwrap_or(Value::Null)
}

pub async fn screenshot(proxy: SocketAddr, request: &Value) -> Result<Value, String> {
    let url = request["url"].as_str().ok_or("url is required")?;
    let width = request["width"].as_u64().unwrap_or(1440).clamp(320, 2560) as u32;
    let height = request["height"].as_u64().unwrap_or(900).clamp(240, 2560) as u32;
    let scale = request["device_scale_factor"].as_f64().unwrap_or(1.0).clamp(1.0, 3.0);
    let mobile = request["mobile"].as_bool().unwrap_or(false);
    let format = match request["format"].as_str().unwrap_or("png") {
        "jpeg" => CaptureScreenshotFormat::Jpeg,
        "webp" => CaptureScreenshotFormat::Webp,
        _ => CaptureScreenshotFormat::Png,
    };
    let session = Session::launch(proxy, width, height).await?;
    let result = async {
        let page = session.open(url, width, height, scale, mobile).await?;
        let mut params = ScreenshotParams::builder()
            .format(format.clone())
            .full_page(request["full_page"].as_bool().unwrap_or(false));
        if format != CaptureScreenshotFormat::Png {
            params = params.quality(request["quality"].as_i64().unwrap_or(80).clamp(1, 100));
        }
        let bytes = page.screenshot(params.build()).await.map_err(|e| format!("Capture failed: {e}"))?;
        if bytes.len() > 3 * 1024 * 1024 {
            return Err("Screenshot exceeds 3 MiB; use jpeg/webp or a smaller viewport".to_owned());
        }
        let (w, h) = image::load_from_memory(&bytes).map(|i| (i.width(), i.height())).unwrap_or((0, 0));
        use base64::Engine;
        Ok(json!({"image_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
            "format": request["format"].as_str().unwrap_or("png"), "width": w, "height": h, "bytes": bytes.len(),
            "final_url": page.url().await.ok().flatten(), "status": navigation_status(&page).await}))
    }
    .await;
    session.close().await;
    result
}

pub async fn render(proxy: SocketAddr, request: &Value) -> Result<Value, String> {
    let url = request["url"].as_str().ok_or("url is required")?;
    let session = Session::launch(proxy, 1440, 900).await?;
    let result = async {
        let page = session.open(url, 1440, 900, 1.0, false).await?;
        let html = page.content().await.map_err(|_| "Could not read rendered HTML")?;
        if html.len() > 2 * 1024 * 1024 {
            return Err("Rendered HTML exceeds 2 MiB".to_owned());
        }
        Ok(json!({"html": html, "final_url": page.url().await.ok().flatten(), "status": navigation_status(&page).await}))
    }
    .await;
    session.close().await;
    result
}

pub async fn performance(proxy: SocketAddr, request: &Value) -> Result<Value, String> {
    let url = request["url"].as_str().ok_or("url is required")?;
    let session = Session::launch(proxy, 1440, 900).await?;
    let result = async {
        let page = session.open(url, 1440, 900, 1.0, false).await?;
        let script = "(() => { const n = performance.getEntriesByType('navigation')[0] || {}; const r = performance.getEntriesByType('resource');
            const paint = Object.fromEntries(performance.getEntriesByType('paint').map(p => [p.name, Math.round(p.startTime)]));
            return { status: n.responseStatus || null, ttfb_ms: Math.round(n.responseStart - n.requestStart), dom_content_loaded_ms: Math.round(n.domContentLoadedEventEnd),
              load_ms: Math.round(n.loadEventEnd), first_paint_ms: paint['first-paint'] ?? null, first_contentful_paint_ms: paint['first-contentful-paint'] ?? null,
              resource_count: r.length, transfer_bytes: r.reduce((a, e) => a + (e.transferSize || 0), n.transferSize || 0), dom_nodes: document.getElementsByTagName('*').length }; })()";
        let metrics = page.evaluate(script).await.map_err(|_| "Could not read performance metrics")?;
        Ok(json!({"url": page.url().await.ok().flatten(), "browser_metrics": metrics.value().cloned(),
            "notes": "Measured by headless Chromium on the worker; network path differs from real users."}))
    }
    .await;
    session.close().await;
    result
}
