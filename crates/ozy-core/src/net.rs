//! All untrusted outbound HTTP goes through this module.
//! Each DNS answer is checked and pinned into the actual connection. Redirects
//! are disabled in reqwest and independently validated here.
use crate::Error;
use futures_util::StreamExt;
use reqwest::{Client, Method, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

pub const MAX_RESPONSE: usize = 2 * 1024 * 1024;
/// Response headers retained for callers. Cookies and auth-related headers are never kept.
pub const SAFE_HEADERS: [&str; 24] = [
    "content-type",
    "content-length",
    "content-encoding",
    "location",
    "server",
    "x-powered-by",
    "via",
    "cf-ray",
    "x-vercel-id",
    "x-nf-request-id",
    "x-amz-cf-id",
    "x-served-by",
    "x-cache",
    "x-generator",
    "x-drupal-cache",
    "x-shopify-stage",
    "x-wix-request-id",
    "x-github-request-id",
    "fly-request-id",
    "etag",
    "last-modified",
    "strict-transport-security",
    "content-security-policy",
    "alt-svc",
];
pub const USER_AGENT: &str = "OzyScrape/0.1 (+https://dev.itsozy.lol/docs/responsible-use)";

/// When a robots.txt body was fetched, and the body (None = the site has none).
type RobotsEntry = (Instant, Option<String>);

#[derive(Clone)]
pub struct SafeHttp {
    slots: Arc<Semaphore>,
    domains: Arc<Mutex<HashMap<String, Instant>>>,
    clients: Arc<Mutex<HashMap<String, (Instant, Client)>>>,
    /// robots.txt bodies per origin for ten minutes (None = no robots file).
    robots: Arc<Mutex<HashMap<String, RobotsEntry>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    pub elapsed_ms: u128,
    pub redirects: Vec<String>,
    /// Time until response headers arrived for the final hop.
    pub first_byte_ms: u128,
    pub headers: HashMap<String, String>,
    #[serde(skip)]
    pub body: Vec<u8>,
}

pub fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

pub fn validate_url(raw: &str) -> Result<Url, Error> {
    if raw.len() > 2048 {
        return Err(Error::Invalid("URL exceeds 2048 bytes".into()));
    }
    let mut url = Url::parse(raw).map_err(|_| Error::Invalid("Invalid absolute URL".into()))?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err(Error::Forbidden);
    }
    let host = url.host_str().ok_or(Error::Forbidden)?.trim_end_matches('.');
    if !host.contains('.')
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".test")
        || host.ends_with(".invalid")
        || host == "metadata.google.internal"
    {
        return Err(Error::Forbidden);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) if public_ipv4(v4) => {}
            _ => return Err(Error::Forbidden),
        }
    }
    url.set_fragment(None);
    Ok(url)
}

/// IPv4-only egress is intentional. IPv6 transition mechanisms are rejected
/// rather than guessed at. Every returned IPv4 must be public.
pub async fn resolve_public(url: &Url) -> Result<Vec<SocketAddr>, Error> {
    let host = url.host_str().ok_or(Error::Forbidden)?;
    let port = url.port_or_known_default().ok_or(Error::Forbidden)?;
    let answers = tokio::time::timeout(Duration::from_secs(4), tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Upstream)?;
    let mut addresses = Vec::new();
    for addr in answers {
        if let IpAddr::V4(ip) = addr.ip() {
            if !public_ipv4(ip) {
                return Err(Error::Forbidden);
            }
            addresses.push(addr);
        }
    }
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(Error::Forbidden);
    }
    Ok(addresses)
}

impl Default for SafeHttp {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(8)),
            domains: Arc::new(Mutex::new(HashMap::new())),
            clients: Arc::new(Mutex::new(HashMap::new())),
            robots: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl SafeHttp {
    async fn pace(&self, host: &str) {
        let wait = {
            let mut domains = self.domains.lock().await;
            domains.retain(|_, t| *t > Instant::now() - Duration::from_secs(60));
            let now = Instant::now();
            let next = domains.entry(host.to_owned()).or_insert(now);
            let wait = next.saturating_duration_since(now);
            *next = std::cmp::max(*next, now) + Duration::from_secs(1);
            wait
        };
        tokio::time::sleep(wait).await;
    }

    async fn client(&self, url: &Url) -> Result<Client, Error> {
        let addrs = resolve_public(url).await?;
        let host = url.host_str().ok_or(Error::Forbidden)?;
        let key = format!("{host}:{addrs:?}");
        let mut clients = self.clients.lock().await;
        clients.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        if let Some((_, client)) = clients.get(&key) {
            return Ok(client.clone());
        }
        if clients.len() >= 128 {
            clients.clear();
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .resolve_to_addrs(host, &addrs)
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|_| Error::Upstream)?;
        clients.insert(key, (Instant::now(), client.clone()));
        Ok(client)
    }

    pub async fn fetch(&self, raw: &str, robots: bool) -> Result<FetchResult, Error> {
        tokio::time::timeout(Duration::from_secs(35), self.fetch_inner(raw, robots))
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn fetch_inner(&self, raw: &str, robots: bool) -> Result<FetchResult, Error> {
        let mut url = validate_url(raw)?;
        let _permit = self.slots.acquire().await.map_err(|_| Error::Upstream)?;
        let started = Instant::now();
        let mut redirects = Vec::new();
        for hop in 0..=5 {
            if robots {
                self.check_robots(&url).await?;
            }
            let mut fetched = self.request(&url, Method::GET, None, &[]).await?;
            if matches!(fetched.status, 301 | 302 | 303 | 307 | 308) {
                if hop == 5 {
                    return Err(Error::Invalid("Too many redirects".into()));
                }
                let location = fetched.headers.get("location").ok_or(Error::Upstream)?;
                url = validate_url(url.join(location).map_err(|_| Error::Upstream)?.as_str())?;
                redirects.push(url.to_string());
                continue;
            }
            fetched.elapsed_ms = started.elapsed().as_millis();
            fetched.redirects = redirects;
            return Ok(fetched);
        }
        Err(Error::Upstream)
    }

    async fn check_robots(&self, target: &Url) -> Result<(), Error> {
        let origin = target.origin().ascii_serialization();
        let cached = {
            let mut cache = self.robots.lock().await;
            cache.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(600));
            cache.get(&origin).map(|(_, body)| body.clone())
        };
        let body = match cached {
            Some(body) => body,
            None => {
                let body = self.fetch_robots(target).await?;
                let mut cache = self.robots.lock().await;
                if cache.len() >= 256 {
                    cache.clear();
                }
                cache.insert(origin, (Instant::now(), body.clone()));
                body
            }
        };
        let Some(body) = body else { return Ok(()) };
        let mut matcher = robotstxt::DefaultMatcher::default();
        if !matcher.one_agent_allowed_by_robots(&body, "OzyScrape", target.as_str()) {
            return Err(Error::Robots);
        }
        // Honor declared crawl delays conservatively, including wildcard groups.
        let delay = body
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                if key.trim().eq_ignore_ascii_case("crawl-delay") {
                    value.trim().parse::<f64>().ok()
                } else {
                    None
                }
            })
            .filter(|v| v.is_finite() && *v > 0.0)
            .fold(0.0, f64::max);
        if delay > 10.0 {
            return Err(Error::RateLimited);
        }
        if delay > 1.0 {
            tokio::time::sleep(Duration::from_secs_f64(delay - 1.0)).await;
        }
        Ok(())
    }

    /// Ok(None) when the site has no robots.txt (404/410). Other failures deny.
    async fn fetch_robots(&self, target: &Url) -> Result<Option<String>, Error> {
        let mut robots = target.clone();
        robots.set_path("/robots.txt");
        robots.set_query(None);
        for _ in 0..=3 {
            let response = self.request_unpaced(&robots).await?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let destination = response.headers.get("location").ok_or(Error::Robots)?;
                robots = validate_url(robots.join(destination).map_err(|_| Error::Robots)?.as_str())?;
                continue;
            }
            if response.status == 404 || response.status == 410 {
                return Ok(None);
            }
            if response.status != 200 {
                return Err(Error::Robots);
            }
            return Ok(Some(crate::bounded_text(&String::from_utf8_lossy(&response.body), 500_000)));
        }
        Err(Error::Robots)
    }

    /// Webhook delivery never follows redirects. Only `x-ozy-*` headers may be added.
    pub async fn post_signed(&self, raw: &str, body: Vec<u8>, headers: &[(&str, &str)]) -> Result<FetchResult, Error> {
        let url = validate_url(raw)?;
        if url.scheme() != "https" {
            return Err(Error::Forbidden);
        }
        if headers.iter().any(|(k, _)| !k.starts_with("x-ozy-")) {
            return Err(Error::Forbidden);
        }
        let _permit = self.slots.acquire().await.map_err(|_| Error::Upstream)?;
        tokio::time::timeout(Duration::from_secs(15), self.request(&url, Method::POST, Some(body), headers))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// robots.txt is fetched once per origin per ten minutes, so it skips per-host pacing.
    async fn request_unpaced(&self, url: &Url) -> Result<FetchResult, Error> {
        self.send(url, Method::GET, None, &[], false).await
    }

    async fn request(&self, url: &Url, method: Method, body: Option<Vec<u8>>, headers: &[(&str, &str)]) -> Result<FetchResult, Error> {
        self.send(url, method, body, headers, true).await
    }

    async fn send(
        &self,
        url: &Url,
        method: Method,
        body: Option<Vec<u8>>,
        headers: &[(&str, &str)],
        paced: bool,
    ) -> Result<FetchResult, Error> {
        let started = Instant::now();
        for attempt in 0..3 {
            if paced {
                self.pace(url.host_str().ok_or(Error::Forbidden)?).await;
            }
            let client = self.client(url).await?;
            let mut request = client.request(method.clone(), url.clone());
            if let Some(bytes) = &body {
                request = request.body(bytes.clone()).header("content-type", "application/json");
            }
            for (key, value) in headers {
                request = request.header(*key, *value);
            }
            let sent = Instant::now();
            let response = request.send().await.map_err(|_| Error::Upstream)?;
            if matches!(response.status().as_u16(), 429 | 503) {
                if method != Method::GET || attempt == 2 {
                    return Err(Error::RateLimited);
                }
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1 << attempt);
                if delay > 5 {
                    return Err(Error::RateLimited);
                }
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            if response.content_length().is_some_and(|n| n > MAX_RESPONSE as u64) {
                return Err(Error::TooLarge);
            }
            let first_byte_ms = sent.elapsed().as_millis();
            let status = response.status().as_u16();
            let headers = SAFE_HEADERS
                .into_iter()
                .filter_map(|key| {
                    response
                        .headers()
                        .get(key)
                        .and_then(|v| v.to_str().ok())
                        .map(|v| (key.to_owned(), crate::bounded_text(v, 2048)))
                })
                .collect();
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| Error::Upstream)?;
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                    return Err(Error::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(FetchResult {
                url: url.to_string(),
                status,
                elapsed_ms: started.elapsed().as_millis(),
                redirects: vec![],
                first_byte_ms,
                headers,
                body: bytes,
            });
        }
        Err(Error::Upstream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_ssrf_encodings_and_non_http() {
        for raw in [
            "http://127.0.0.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.1",
            "http://[::1]",
            "http://[::ffff:127.0.0.1]",
            "file:///etc/passwd",
            "http://user:pass@example.com",
            "https://example.com:8080",
            "http://metadata.google.internal",
        ] {
            assert!(validate_url(raw).is_err(), "{raw}");
        }
        assert!(validate_url("https://example.com/path?q=1").is_ok());
    }
    #[test]
    fn rejects_reserved_ranges() {
        for ip in [
            "100.64.0.1",
            "198.19.0.1",
            "192.0.2.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public_ipv4(ip.parse().unwrap()));
        }
        assert!(public_ipv4("93.184.216.34".parse().unwrap()));
    }
}
