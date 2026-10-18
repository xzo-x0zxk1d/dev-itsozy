pub mod account;
pub mod catalog;
pub mod jobs;
pub mod services;
pub mod web;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

#[derive(Clone)]
pub struct App {
    pub db: PgPool,
    pub http: ozy_core::net::SafeHttp,
    pub crypto_slots: Arc<Semaphore>,
    pub origin: String,
    pub encryption_key: String,
    pub cron_secret: String,
    pub worker_url: Option<String>,
    pub worker_secret: Option<String>,
    pub limits: Limits,
}
/// Free-tier quotas in units. Overridable with OZY_UNITS_PER_MINUTE, OZY_UNITS_PER_DAY, OZY_PLATFORM_UNITS_PER_DAY.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub per_minute: i32,
    pub per_day: i32,
    pub platform_per_day: i32,
}
impl Limits {
    pub fn from_env() -> Self {
        let read = |name: &str, default: i32| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &i32| *v > 0)
                .unwrap_or(default)
        };
        Self {
            per_minute: read("OZY_UNITS_PER_MINUTE", 20),
            per_day: read("OZY_UNITS_PER_DAY", 100),
            platform_per_day: read("OZY_PLATFORM_UNITS_PER_DAY", 2000),
        }
    }
}
#[derive(Clone)]
pub struct Identity {
    pub user_id: Uuid,
    pub key_id: Option<Uuid>,
    pub session: bool,
}
#[derive(Debug)]
pub struct Failure(pub StatusCode, pub &'static str, pub String);
pub type Result<T> = std::result::Result<T, Failure>;
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":{"code":self.1,"message":self.2}}))).into_response()
    }
}
impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
            "Database operation could not be completed".into(),
        )
    }
}
impl From<ozy_core::Error> for Failure {
    fn from(error: ozy_core::Error) -> Self {
        use ozy_core::Error;
        let (status, code) = match &error {
            Error::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_input"),
            Error::Forbidden => (StatusCode::FORBIDDEN, "destination_blocked"),
            Error::Robots => (StatusCode::FORBIDDEN, "robots_denied"),
            Error::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "response_too_large"),
            Error::Timeout => (StatusCode::GATEWAY_TIMEOUT, "upstream_timeout"),
            Error::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "upstream_rate_limited"),
            Error::Upstream => (StatusCode::BAD_GATEWAY, "upstream_failed"),
        };
        Self(status, code, error.to_string())
    }
}
pub fn invalid(message: &str) -> Failure {
    Failure(StatusCode::BAD_REQUEST, "invalid_input", message.into())
}
pub fn unauthorized() -> Failure {
    Failure(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "A valid API key or session is required".into(),
    )
}
pub fn missing() -> Failure {
    Failure(StatusCode::NOT_FOUND, "not_found", "Resource not found".into())
}

pub async fn from_env() -> std::result::Result<App, Box<dyn std::error::Error + Send + Sync>> {
    let database_url = std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .map_err(|_| "POSTGRES_URL is not set")?;
    let encryption_key = std::env::var("OZY_ENCRYPTION_KEY").map_err(|_| "OZY_ENCRYPTION_KEY is not set")?;
    if encryption_key.len() != 64 || hex_invalid(&encryption_key) {
        return Err("OZY_ENCRYPTION_KEY must be 32 random bytes encoded as hex".into());
    }
    let cron_secret = std::env::var("CRON_SECRET").map_err(|_| "CRON_SECRET is not set")?;
    if cron_secret.len() < 32 {
        return Err("CRON_SECRET must be at least 32 characters".into());
    }
    Ok(App {
        db: ozy_db::connect(&database_url)?,
        http: Default::default(),
        crypto_slots: Arc::new(Semaphore::new(2)),
        // Preview deployments have no PUBLIC_ORIGIN; Vercel exposes their host as VERCEL_URL.
        origin: std::env::var("PUBLIC_ORIGIN")
            .or_else(|_| std::env::var("VERCEL_URL").map(|host| format!("https://{host}")))
            .unwrap_or_else(|_| "https://dev.itsozy.lol".into()),
        encryption_key,
        cron_secret,
        worker_url: std::env::var("OZY_WORKER_URL").ok().filter(|v| !v.is_empty()),
        worker_secret: std::env::var("OZY_WORKER_SECRET").ok().filter(|v| !v.is_empty()),
        limits: Limits::from_env(),
    })
}
fn hex_invalid(value: &str) -> bool {
    !value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Served when required configuration is missing: pages still render, APIs answer 503.
pub fn unconfigured_router(reason: String) -> Router {
    let reason = Arc::new(reason);
    Router::new()
        .route(
            "/api/v1/health",
            get({
                let r = reason.clone();
                move || async move {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"service":"ozy-platform","configured":false,"reason":*r})),
                    )
                }
            }),
        )
        .route("/assets/app.js", get(web::javascript))
        .route("/assets/style.css", get(web::stylesheet))
        .route(
            "/{*path}",
            axum::routing::any(|| async {
                Failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "not_configured",
                    "The platform is missing required configuration".into(),
                )
            }),
        )
        .layer(middleware::from_fn(security_headers))
}

pub fn router(app: App) -> Router {
    let protected = Router::new()
        .route("/api/v1/scrape", post(services::scrape))
        .route("/api/v1/preview", get(services::preview))
        .route("/api/v1/intelligence", get(services::intelligence))
        .route("/api/v1/performance", post(services::performance))
        .route("/api/v1/url/check", post(services::url_check))
        .route("/api/v1/email/validate", post(services::email_validate))
        .route("/api/v1/ocr/captcha", post(services::ocr))
        .route("/api/v1/screenshot", post(services::screenshot))
        .route("/api/v1/pdf/parse", post(services::pdf))
        .route("/api/v1/monitor", post(jobs::create_monitor).get(jobs::list_monitors))
        .route("/api/v1/monitor/{id}", get(jobs::get_monitor).delete(jobs::delete_monitor))
        .route("/api/v1/extract", get(jobs::list_extractions))
        .route("/api/v1/extract/create", post(jobs::create_extraction))
        .route("/api/v1/extract/{id}", get(jobs::get_extraction).delete(jobs::delete_extraction))
        .route("/api/v1/crawls", post(jobs::create_crawl))
        .route("/api/v1/crawls/{id}", get(jobs::get_crawl).delete(jobs::cancel_crawl))
        .route("/api/v1/crawls/{id}/resume", post(jobs::resume_crawl))
        .route("/api/v1/webhooks", post(jobs::create_webhook).get(jobs::list_webhooks))
        .route("/api/v1/webhooks/{id}", delete(jobs::delete_webhook))
        .route("/api/v1/webhooks/{id}/deliveries", get(jobs::deliveries))
        .route("/api/v1/account", get(account::me).patch(account::update_account))
        .route("/api/v1/account/password", post(account::change_password))
        .route("/api/v1/keys", get(account::keys).post(account::create_key))
        .route("/api/v1/keys/{id}", delete(account::revoke_key))
        .route("/api/v1/usage", get(account::usage))
        .route("/api/v1/logs", get(account::logs))
        .route("/api/v1/notifications", get(account::notifications))
        .route("/api/v1/auth/logout", post(account::logout))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .merge(protected)
        .route("/api/v1/auth/register", post(account::register))
        .route("/api/v1/auth/login", post(account::login))
        .route("/api/v1/hooks/{id}", post(jobs::receive_webhook))
        .route("/api/v1/internal/tick", post(jobs::tick).get(jobs::tick))
        .route("/api/v1/health", get(health))
        .route("/api/v1/status", get(web::status))
        .route("/api/v1/catalog", get(web::catalog_json))
        .route("/openapi.json", get(web::openapi))
        .route("/assets/app.js", get(web::javascript))
        .route("/assets/style.css", get(web::stylesheet))
        .route("/", get(web::page))
        .route("/{*path}", get(web::page))
        .route("/api/{*rest}", axum::routing::any(|| async { missing() }))
        .fallback(|| async { missing() })
        .layer(DefaultBodyLimit::max(3 * 1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}
async fn health(State(app): State<App>) -> impl IntoResponse {
    let database = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&app.db).await.is_ok();
    (
        if database {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"service":"ozy-platform","runtime":"rust","database":database,"worker_configured":app.worker_url.is_some(),"version":env!("CARGO_PKG_VERSION")}),
        ),
    )
}
pub async fn security_headers(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let mut response = match tokio::time::timeout(Duration::from_secs(55), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Failure(StatusCode::GATEWAY_TIMEOUT, "request_timeout", "Request exceeded 55 seconds".into()).into_response(),
    };
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
        ("cache-control", "no-store"),
    ] {
        headers.insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    if let Ok(value) = request_id.parse() {
        headers.insert("x-request-id", value);
    }
    response
}
pub fn origin_check(app: &App, headers: &HeaderMap, required: bool) -> Result<()> {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(value) if value == app.origin => Ok(()),
        None if !required => Ok(()),
        _ => Err(Failure(
            StatusCode::FORBIDDEN,
            "csrf_rejected",
            "Request origin is not permitted".into(),
        )),
    }
}
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|s| cookie::Cookie::parse(s.trim()).ok())
        .find(|c| matches!(c.name(), "__Host-ozy_session" | "ozy_session"))
        .map(|c| c.value().to_owned())
}
async fn authenticate(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let result = identify(&app, request.headers().clone(), &path, &method).await;
    let identity = match result {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let management = is_management(&path);
    let cost = if management {
        0
    } else if path.contains("ocr/") || path.contains("screenshot") || path.contains("pdf/") {
        10
    } else {
        1
    };
    if cost > 0 {
        for (key, window, limit) in [
            (format!("minute:{}", identity.user_id), 60, app.limits.per_minute),
            (format!("day:{}", identity.user_id), 86400, app.limits.per_day),
            ("platform:day".into(), 86400, app.limits.platform_per_day),
        ] {
            match ozy_db::take_budget(&app.db, &key, window, limit, cost).await {
                Ok(true) => {}
                Ok(false) => {
                    return Failure(
                        StatusCode::TOO_MANY_REQUESTS,
                        "quota_exceeded",
                        "Free-tier request quota reached".into(),
                    )
                    .into_response();
                }
                Err(e) => return Failure::from(e).into_response(),
            }
        }
    }
    request.extensions_mut().insert(identity.clone());
    let started = Instant::now();
    let response = next.run(request).await;
    if cost > 0 {
        let _ =
            sqlx::query("INSERT INTO ozy.request_logs(id,user_id,key_id,endpoint,status,duration_ms,units) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(Uuid::new_v4())
                .bind(identity.user_id)
                .bind(identity.key_id)
                .bind(path)
                .bind(i32::from(response.status().as_u16()))
                .bind(started.elapsed().as_millis() as i64)
                .bind(cost)
                .execute(&app.db)
                .await;
    }
    response
}
fn is_management(path: &str) -> bool {
    [
        "/api/v1/account",
        "/api/v1/keys",
        "/api/v1/usage",
        "/api/v1/logs",
        "/api/v1/notifications",
        "/api/v1/auth/",
    ]
    .iter()
    .any(|p| path.starts_with(p))
}
async fn identify(app: &App, headers: HeaderMap, path: &str, method: &axum::http::Method) -> Result<Identity> {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        if !bearer.starts_with("ozy_") || bearer.len() > 100 {
            return Err(unauthorized());
        }
        let row = sqlx::query("SELECT id,user_id,permissions FROM ozy.api_keys WHERE token_hash=$1 AND revoked_at IS NULL")
            .bind(ozy_auth::token_hash(bearer))
            .fetch_optional(&app.db)
            .await?
            .ok_or_else(unauthorized)?;
        if is_management(path) {
            return Err(Failure(
                StatusCode::FORBIDDEN,
                "session_required",
                "Sign in to manage your account".into(),
            ));
        }
        let permission = path.trim_start_matches("/api/v1/").split('/').next().unwrap_or("");
        let permissions: Vec<String> = row.get("permissions");
        if !permissions.iter().any(|p| p == "*" || p == permission) {
            return Err(Failure(
                StatusCode::FORBIDDEN,
                "permission_denied",
                "This key does not allow this API".into(),
            ));
        }
        let id: Uuid = row.get("id");
        sqlx::query("UPDATE ozy.api_keys SET last_used_at=now() WHERE id=$1")
            .bind(id)
            .execute(&app.db)
            .await?;
        return Ok(Identity {
            user_id: row.get("user_id"),
            key_id: Some(id),
            session: false,
        });
    }
    let token = session_token(&headers).ok_or_else(unauthorized)?;
    if token.len() > 100 {
        return Err(unauthorized());
    }
    let user_id = sqlx::query_scalar("SELECT user_id FROM ozy.sessions WHERE token_hash=$1 AND expires_at>now()")
        .bind(ozy_auth::token_hash(&token))
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(unauthorized)?;
    if !matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
        origin_check(app, &headers, true)?;
    }
    Ok(Identity {
        user_id,
        key_id: None,
        session: true,
    })
}
pub async fn domain_budget(app: &App, raw: &str) -> Result<()> {
    let url = ozy_core::net::validate_url(raw)?;
    let host = url.host_str().ok_or_else(|| invalid("Missing host"))?;
    let granted=sqlx::query_scalar::<_,String>("INSERT INTO ozy.domain_leases(host,next_request) VALUES($1,now()+interval '1 second') ON CONFLICT(host) DO UPDATE SET next_request=now()+interval '1 second' WHERE ozy.domain_leases.next_request<=now() RETURNING host")
        .bind(host).fetch_optional(&app.db).await?;
    if granted.is_none() {
        return Err(Failure(
            StatusCode::TOO_MANY_REQUESTS,
            "domain_rate_limited",
            "Please wait before requesting this domain again".into(),
        ));
    }
    Ok(())
}
