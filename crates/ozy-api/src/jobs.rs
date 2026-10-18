//! Stateful APIs backed by PostgreSQL: monitors, saved extractions, crawls,
//! the webhook relay, and the scheduler tick that advances all of them.
use crate::services::{ScrapeInput, extract, validate_rules};
use crate::*;
use axum::{
    body::Bytes,
    extract::{Extension, Path, Query},
};
use futures_util::{StreamExt, stream};
use ozyscrape::{Document, Rule, crawl};
use serde::Deserialize;

const MAX_MONITORS: i64 = 10;
const MAX_EXTRACTIONS: i64 = 10;
const MAX_WEBHOOKS: i64 = 10;
const MAX_ACTIVE_CRAWLS: i64 = 2;
const WEBHOOK_MAX_ATTEMPTS: i32 = 6;

/// Per-user resource cap, serialized per user with a transaction-scoped advisory lock.
async fn within_limit(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user: Uuid, sql: &str, max: i64, what: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(user.to_string())
        .execute(&mut **tx)
        .await?;
    let count: i64 = sqlx::query_scalar(sql).bind(user).fetch_one(&mut **tx).await?;
    if count >= max {
        return Err(Failure(
            StatusCode::CONFLICT,
            "limit_reached",
            format!("Free tier allows at most {max} {what}"),
        ));
    }
    Ok(())
}

fn webhook_destination(raw: &str) -> Result<String> {
    let url = ozy_core::net::validate_url(raw)?;
    if url.scheme() != "https" {
        return Err(invalid("Webhook destinations must use https"));
    }
    Ok(url.to_string())
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Signs and posts one JSON event. Never follows redirects; destination is SSRF-validated per call.
async fn send_signed(
    app: &App,
    destination: &str,
    secret: &str,
    event_id: &str,
    event_type: &str,
    body: Vec<u8>,
) -> std::result::Result<u16, String> {
    let timestamp = unix_now().to_string();
    let signature = ozy_auth::sign(secret, &timestamp, &body);
    let headers = [
        ("x-ozy-signature", signature.as_str()),
        ("x-ozy-timestamp", timestamp.as_str()),
        ("x-ozy-event-id", event_id),
        ("x-ozy-event-type", event_type),
    ];
    match app.http.post_signed(destination, body, &headers).await {
        Ok(response) => Ok(response.status),
        Err(error) => Err(error.to_string()),
    }
}

async fn notify(app: &App, user: Uuid, kind: &str, message: &str) {
    let _ = sqlx::query("INSERT INTO ozy.notifications(id,user_id,kind,message) VALUES($1,$2,$3,$4)")
        .bind(Uuid::new_v4())
        .bind(user)
        .bind(kind)
        .bind(ozy_core::bounded_text(message, 500))
        .execute(&app.db)
        .await;
}

// ───────────────────────────── API 2: Website Monitor ─────────────────────────────

#[derive(Deserialize)]
pub struct MonitorInput {
    url: String,
    interval_seconds: Option<i32>,
    webhook_url: Option<String>,
    /// Optional CSS selector; content-change detection hashes only its visible text.
    selector: Option<String>,
}

async fn monitor_view(app: &App, user: Uuid, id: Uuid) -> Result<Value> {
    let monitor: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'url',url,'interval_seconds',interval_seconds,'webhook_url',webhook_url,
         'selector',selector,'enabled',enabled,'next_check',next_check,'last_status',last_status,
         'last_available',last_available,'last_checked_at',last_checked_at,'last_error',last_error,'created_at',created_at)
         FROM ozy.monitors WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(missing)?;
    let stats: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object(
          'checks',count(*),
          'uptime_24h',round(100.0*avg(available::int) FILTER (WHERE checked_at>now()-interval '24 hours'),2),
          'uptime_7d',round(100.0*avg(available::int) FILTER (WHERE checked_at>now()-interval '7 days'),2),
          'uptime_30d',round(100.0*avg(available::int),2),
          'average_response_ms_24h',round(avg(duration_ms) FILTER (WHERE checked_at>now()-interval '24 hours')),
          'content_changes_30d',count(*) FILTER (WHERE changed))
         FROM ozy.monitor_checks WHERE monitor_id=$1 AND checked_at>now()-interval '30 days'",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await?;
    let history: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(x ORDER BY x.checked_at DESC),'[]') FROM (SELECT status,duration_ms,available,changed,error,checked_at
         FROM ozy.monitor_checks WHERE monitor_id=$1 ORDER BY checked_at DESC LIMIT 100) x",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await?;
    Ok(json!({"monitor":monitor,"stats":stats,"history":history}))
}

pub async fn create_monitor(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<MonitorInput>,
) -> Result<Json<Value>> {
    let url = ozy_core::net::validate_url(&input.url)?.to_string();
    let interval = input.interval_seconds.unwrap_or(300);
    if !(300..=86_400).contains(&interval) {
        return Err(invalid("interval_seconds must be between 300 and 86400"));
    }
    if let Some(selector) = &input.selector {
        Document::parse("")?.select(selector)?;
    }
    let destination = input.webhook_url.as_deref().map(webhook_destination).transpose()?;
    let secret = destination.as_ref().map(|_| ozy_auth::token("whsec_"));
    let ciphertext = secret
        .as_deref()
        .map(|s| ozy_auth::encrypt(&app.encryption_key, s))
        .transpose()
        .map_err(|_| invalid("Could not secure webhook secret"))?;
    let monitor_id = Uuid::new_v4();
    let mut tx = app.db.begin().await?;
    within_limit(
        &mut tx,
        id.user_id,
        "SELECT count(*) FROM ozy.monitors WHERE user_id=$1",
        MAX_MONITORS,
        "monitors",
    )
    .await?;
    sqlx::query("INSERT INTO ozy.monitors(id,user_id,url,interval_seconds,webhook_url,webhook_secret_ciphertext,selector) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(monitor_id)
        .bind(id.user_id)
        .bind(&url)
        .bind(interval)
        .bind(&destination)
        .bind(ciphertext)
        .bind(&input.selector)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    check_monitor(&app, monitor_id).await?;
    let mut view = monitor_view(&app, id.user_id, monitor_id).await?;
    if let Some(secret) = secret {
        view["webhook_secret"] = json!(secret);
        view["notice"] =
            json!("Copy the webhook secret now; it is shown once. Verify x-ozy-signature = HMAC-SHA256(secret, timestamp + '.' + body).");
    }
    Ok(Json(view))
}

/// Runs one availability/content check and emits down/up/changed events.
pub async fn check_monitor(app: &App, monitor_id: Uuid) -> Result<Value> {
    let row = sqlx::query("SELECT user_id,url,interval_seconds,webhook_url,webhook_secret_ciphertext,selector,content_hash,last_available FROM ozy.monitors WHERE id=$1")
        .bind(monitor_id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(missing)?;
    let user: Uuid = row.get("user_id");
    let url: String = row.get("url");
    let selector: Option<String> = row.get("selector");
    let previous_hash: Option<String> = row.get("content_hash");
    let previous_available: Option<bool> = row.get("last_available");
    let started = Instant::now();
    let outcome = app.http.fetch(&url, false).await;
    let duration = started.elapsed().as_millis() as i64;
    let (status, available, hash, error) = match outcome {
        Ok(response) => {
            let text = Document::parse(&String::from_utf8_lossy(&response.body)).and_then(|d| d.text_of(selector.as_deref()));
            let hash = text.ok().map(|t| ozy_core::digest(t.as_bytes()));
            (Some(i32::from(response.status)), response.status < 400, hash, None)
        }
        Err(error) => (None, false, None, Some(error.to_string())),
    };
    let changed = matches!((&previous_hash, &hash), (Some(a), Some(b)) if a != b);
    sqlx::query("INSERT INTO ozy.monitor_checks(monitor_id,status,duration_ms,available,changed,error) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(monitor_id)
        .bind(status)
        .bind(duration)
        .bind(available)
        .bind(changed)
        .bind(&error)
        .execute(&app.db)
        .await?;
    sqlx::query(
        "UPDATE ozy.monitors SET content_hash=COALESCE($2,content_hash),last_status=$3,last_available=$4,last_checked_at=now(),
         last_error=$5,next_check=now()+make_interval(secs=>interval_seconds),lease_until=NULL WHERE id=$1",
    )
    .bind(monitor_id)
    .bind(&hash)
    .bind(status)
    .bind(available)
    .bind(&error)
    .execute(&app.db)
    .await?;
    let mut events = Vec::new();
    match (previous_available, available) {
        (Some(true), false) => events.push("monitor.down"),
        (Some(false), true) => events.push("monitor.up"),
        _ => {}
    }
    if changed {
        events.push("monitor.content_changed");
    }
    let destination: Option<String> = row.get("webhook_url");
    let secret = row
        .get::<Option<String>, _>("webhook_secret_ciphertext")
        .and_then(|c| ozy_auth::decrypt(&app.encryption_key, &c).ok());
    for event in &events {
        notify(app, user, event, &format!("{url}: {event} (status {status:?})")).await;
        if let (Some(destination), Some(secret)) = (&destination, &secret) {
            let event_id = Uuid::new_v4().to_string();
            let body = json!({"id":event_id,"type":event,"monitor_id":monitor_id,"url":url,"status":status,
                "available":available,"response_ms":duration,"error":error,"checked_at":chrono::Utc::now()});
            if let Err(failure) = send_signed(app, destination, secret, &event_id, event, body.to_string().into_bytes()).await {
                notify(app, user, "monitor.webhook_failed", &format!("Webhook for {url} failed: {failure}")).await;
            }
        }
    }
    Ok(json!({"status":status,"available":available,"changed":changed,"events":events}))
}

pub async fn list_monitors(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let items: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT id,url,interval_seconds,enabled,last_status,
         last_available,last_checked_at,next_check,created_at FROM ozy.monitors WHERE user_id=$1) x",
    )
    .bind(id.user_id)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(json!({"monitors":items,"limit":MAX_MONITORS})))
}

pub async fn get_monitor(State(app): State<App>, Extension(id): Extension<Identity>, Path(monitor): Path<Uuid>) -> Result<Json<Value>> {
    Ok(Json(monitor_view(&app, id.user_id, monitor).await?))
}

pub async fn delete_monitor(State(app): State<App>, Extension(id): Extension<Identity>, Path(monitor): Path<Uuid>) -> Result<Json<Value>> {
    let deleted = sqlx::query("DELETE FROM ozy.monitors WHERE id=$1 AND user_id=$2")
        .bind(monitor)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(missing());
    }
    Ok(Json(json!({"deleted":true})))
}

// ───────────────────────────── API 11: Website to API ─────────────────────────────

#[derive(Deserialize)]
pub struct ExtractionInput {
    name: String,
    url: String,
    rules: Vec<Rule>,
    refresh_seconds: Option<i32>,
    #[serde(default)]
    markdown: bool,
}

async fn extraction_view(app: &App, user: Uuid, id: Uuid) -> Result<Value> {
    let view: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'name',name,'url',url,'rules',rules,'refresh_seconds',refresh_seconds,'markdown',markdown,
         'next_refresh',next_refresh,'result',last_result,'error',last_error,'created_at',created_at,
         'usage',jsonb_build_object('fetch_count',fetch_count,'refresh_count',refresh_count,
           'last_refreshed_at',last_refreshed_at,'last_accessed_at',last_accessed_at))
         FROM ozy.extractions WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(missing)?;
    Ok(view)
}

pub async fn refresh_extraction(app: &App, id: Uuid) -> Result<()> {
    let row = sqlx::query("SELECT user_id,url,rules,markdown FROM ozy.extractions WHERE id=$1")
        .bind(id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(missing)?;
    let rules: Vec<Rule> = serde_json::from_value(row.get("rules")).map_err(|_| invalid("Stored rules are invalid"))?;
    let input = ScrapeInput {
        url: Some(row.get("url")),
        html: None,
        rules,
        markdown: row.get("markdown"),
        cache_seconds: 0,
        render: false,
    };
    let (result, error) = match extract(app, row.get("user_id"), &input).await {
        Ok(result) => (Some(result), None),
        Err(Failure(_, code, message)) => (None, Some(format!("{code}: {message}"))),
    };
    sqlx::query(
        "UPDATE ozy.extractions SET last_result=COALESCE($2,last_result),last_error=$3,last_refreshed_at=now(),
         refresh_count=refresh_count+1,next_refresh=now()+make_interval(secs=>refresh_seconds),lease_until=NULL WHERE id=$1",
    )
    .bind(id)
    .bind(result)
    .bind(error)
    .execute(&app.db)
    .await?;
    Ok(())
}

pub async fn create_extraction(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<ExtractionInput>,
) -> Result<Json<Value>> {
    let name = input.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(invalid("name must be 1-80 bytes"));
    }
    if input.rules.is_empty() {
        return Err(invalid("At least one rule is required"));
    }
    validate_rules(&input.rules)?;
    let url = ozy_core::net::validate_url(&input.url)?.to_string();
    let refresh = input.refresh_seconds.unwrap_or(3600);
    if !(300..=86_400).contains(&refresh) {
        return Err(invalid("refresh_seconds must be between 300 and 86400"));
    }
    let extraction_id = Uuid::new_v4();
    let mut tx = app.db.begin().await?;
    within_limit(
        &mut tx,
        id.user_id,
        "SELECT count(*) FROM ozy.extractions WHERE user_id=$1",
        MAX_EXTRACTIONS,
        "saved extractions",
    )
    .await?;
    sqlx::query("INSERT INTO ozy.extractions(id,user_id,name,url,rules,refresh_seconds,markdown) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(extraction_id)
        .bind(id.user_id)
        .bind(name)
        .bind(&url)
        .bind(json!(input.rules))
        .bind(refresh)
        .bind(input.markdown)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    refresh_extraction(&app, extraction_id).await?;
    let mut view = extraction_view(&app, id.user_id, extraction_id).await?;
    view["endpoint"] = json!(format!("{}/api/v1/extract/{extraction_id}", app.origin));
    Ok(Json(view))
}

#[derive(Deserialize)]
pub struct RefreshQuery {
    #[serde(default)]
    refresh: bool,
}

pub async fn get_extraction(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Path(extraction): Path<Uuid>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<Value>> {
    let stale: Option<bool> = sqlx::query_scalar(
        "SELECT COALESCE(last_refreshed_at<now()-interval '60 seconds',true) FROM ozy.extractions WHERE id=$1 AND user_id=$2",
    )
    .bind(extraction)
    .bind(id.user_id)
    .fetch_optional(&app.db)
    .await?;
    let stale = stale.ok_or_else(missing)?;
    if query.refresh && stale {
        refresh_extraction(&app, extraction).await?;
    }
    sqlx::query("UPDATE ozy.extractions SET fetch_count=fetch_count+1,last_accessed_at=now() WHERE id=$1")
        .bind(extraction)
        .execute(&app.db)
        .await?;
    let mut view = extraction_view(&app, id.user_id, extraction).await?;
    if query.refresh && !stale {
        view["refresh_skipped"] = json!("Refreshed less than 60 seconds ago");
    }
    Ok(Json(view))
}

pub async fn list_extractions(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let items: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT id,name,url,refresh_seconds,next_refresh,
         last_refreshed_at,last_error,fetch_count,created_at FROM ozy.extractions WHERE user_id=$1) x",
    )
    .bind(id.user_id)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(json!({"extractions":items,"limit":MAX_EXTRACTIONS})))
}

pub async fn delete_extraction(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Path(extraction): Path<Uuid>,
) -> Result<Json<Value>> {
    let deleted = sqlx::query("DELETE FROM ozy.extractions WHERE id=$1 AND user_id=$2")
        .bind(extraction)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(missing());
    }
    Ok(Json(json!({"deleted":true})))
}

// ───────────────────────── Crawls (OzyScrape concurrent crawling) ─────────────────────────

#[derive(Deserialize, serde::Serialize, Clone)]
pub struct CrawlConfig {
    url: String,
    #[serde(default)]
    rules: Vec<Rule>,
    #[serde(default = "one")]
    depth: usize,
    #[serde(default = "ten")]
    max_pages: usize,
    #[serde(default = "two")]
    concurrency: usize,
}
fn one() -> usize {
    1
}
fn ten() -> usize {
    10
}
fn two() -> usize {
    2
}

async fn crawl_view(app: &App, user: Uuid, id: Uuid) -> Result<Value> {
    let view: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('id',id,'state',state,'config',config,'error',error,'created_at',created_at,'updated_at',updated_at,
         'pages',checkpoint->'pages','visited',jsonb_array_length(checkpoint->'visited'),'pending',jsonb_array_length(checkpoint->'pending'))
         FROM ozy.crawls WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(missing)?;
    Ok(view)
}

/// Advances a crawl until it completes or `budget` elapses; progress is checkpointed after every batch.
pub async fn run_crawl(app: &App, id: Uuid, budget: Duration) -> Result<()> {
    let deadline = Instant::now() + budget;
    let Some(row) = sqlx::query(
        "UPDATE ozy.crawls SET state='running',lease_until=now()+interval '90 seconds',updated_at=now()
         WHERE id=$1 AND state='queued' AND (lease_until IS NULL OR lease_until<now()) RETURNING config,checkpoint",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?
    else {
        return Ok(());
    };
    let config: CrawlConfig = serde_json::from_value(row.get("config")).map_err(|_| invalid("Stored crawl config is invalid"))?;
    let mut checkpoint: crawl::Checkpoint =
        serde_json::from_value(row.get("checkpoint")).map_err(|_| invalid("Stored checkpoint is invalid"))?;
    let done = |c: &crawl::Checkpoint| c.pending.is_empty() || c.visited.len() >= config.max_pages;
    while !done(&checkpoint) && Instant::now() < deadline {
        match crawl::batch(
            &app.http,
            checkpoint.clone(),
            &config.rules,
            config.depth,
            config.max_pages,
            config.concurrency,
        )
        .await
        {
            Ok(next) => checkpoint = next,
            Err(error) => {
                sqlx::query(
                    "UPDATE ozy.crawls SET state='failed',error=$2,lease_until=NULL,updated_at=now() WHERE id=$1 AND state='running'",
                )
                .bind(id)
                .bind(error.to_string())
                .execute(&app.db)
                .await?;
                return Ok(());
            }
        }
        let still_running =
            sqlx::query("UPDATE ozy.crawls SET checkpoint=$2,updated_at=now() WHERE id=$1 AND state='running' RETURNING id")
                .bind(id)
                .bind(json!(checkpoint))
                .fetch_optional(&app.db)
                .await?;
        if still_running.is_none() {
            return Ok(()); // Cancelled while the batch ran.
        }
    }
    let state = if done(&checkpoint) { "completed" } else { "queued" };
    sqlx::query("UPDATE ozy.crawls SET state=$2,checkpoint=$3,lease_until=NULL,updated_at=now() WHERE id=$1 AND state='running'")
        .bind(id)
        .bind(state)
        .bind(json!(checkpoint))
        .execute(&app.db)
        .await?;
    Ok(())
}

pub async fn create_crawl(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(config): Json<CrawlConfig>,
) -> Result<Json<Value>> {
    let start = ozy_core::net::validate_url(&config.url)?.to_string();
    validate_rules(&config.rules)?;
    if config.depth > 3 || !(1..=20).contains(&config.max_pages) || !(1..=4).contains(&config.concurrency) {
        return Err(invalid("Crawl limits: depth 0-3, max_pages 1-20, concurrency 1-4"));
    }
    let mut checkpoint = crawl::Checkpoint::default();
    checkpoint.pending.push_back((start, 0));
    let crawl_id = Uuid::new_v4();
    let mut tx = app.db.begin().await?;
    within_limit(
        &mut tx,
        id.user_id,
        "SELECT count(*) FROM ozy.crawls WHERE user_id=$1 AND state IN ('queued','running')",
        MAX_ACTIVE_CRAWLS,
        "active crawls",
    )
    .await?;
    sqlx::query("INSERT INTO ozy.crawls(id,user_id,config,checkpoint,state) VALUES($1,$2,$3,$4,'queued')")
        .bind(crawl_id)
        .bind(id.user_id)
        .bind(json!(config))
        .bind(json!(checkpoint))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    run_crawl(&app, crawl_id, Duration::from_secs(20)).await?;
    Ok(Json(crawl_view(&app, id.user_id, crawl_id).await?))
}

pub async fn get_crawl(State(app): State<App>, Extension(id): Extension<Identity>, Path(crawl_id): Path<Uuid>) -> Result<Json<Value>> {
    Ok(Json(crawl_view(&app, id.user_id, crawl_id).await?))
}

pub async fn cancel_crawl(State(app): State<App>, Extension(id): Extension<Identity>, Path(crawl_id): Path<Uuid>) -> Result<Json<Value>> {
    sqlx::query("UPDATE ozy.crawls SET state='cancelled',lease_until=NULL,updated_at=now() WHERE id=$1 AND user_id=$2 AND state IN ('queued','running','failed')")
        .bind(crawl_id)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    Ok(Json(crawl_view(&app, id.user_id, crawl_id).await?))
}

pub async fn resume_crawl(State(app): State<App>, Extension(id): Extension<Identity>, Path(crawl_id): Path<Uuid>) -> Result<Json<Value>> {
    let resumed = sqlx::query("UPDATE ozy.crawls SET state='queued',error=NULL,updated_at=now() WHERE id=$1 AND user_id=$2 AND state IN ('queued','failed','cancelled') RETURNING id")
        .bind(crawl_id)
        .bind(id.user_id)
        .fetch_optional(&app.db)
        .await?;
    if resumed.is_none() {
        crawl_view(&app, id.user_id, crawl_id).await?;
        return Err(Failure(
            StatusCode::CONFLICT,
            "not_resumable",
            "Only queued, failed or cancelled crawls can be resumed".into(),
        ));
    }
    run_crawl(&app, crawl_id, Duration::from_secs(20)).await?;
    Ok(Json(crawl_view(&app, id.user_id, crawl_id).await?))
}

// ───────────────────────────── API 12: Webhook Relay ─────────────────────────────

#[derive(Deserialize)]
pub struct WebhookInput {
    destination: String,
    event_types: Option<Vec<String>>,
}

pub fn valid_event_pattern(pattern: &str) -> bool {
    !pattern.is_empty() && pattern.len() <= 64 && pattern.chars().all(|c| c.is_ascii_alphanumeric() || "._-*:".contains(c))
}

/// `*` matches everything; `order.*` matches `order.created`; otherwise exact.
pub fn event_matches(patterns: &[String], event: &str) -> bool {
    patterns
        .iter()
        .any(|p| p == "*" || p == event || p.strip_suffix('*').is_some_and(|prefix| event.starts_with(prefix)))
}

pub async fn create_webhook(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<WebhookInput>,
) -> Result<Json<Value>> {
    let destination = webhook_destination(&input.destination)?;
    let events = input.event_types.unwrap_or_else(|| vec!["*".into()]);
    if events.is_empty() || events.len() > 20 || !events.iter().all(|e| valid_event_pattern(e)) {
        return Err(invalid("event_types must contain 1-20 patterns of [A-Za-z0-9._-:*]"));
    }
    let secret = ozy_auth::token("whsec_");
    let ciphertext = ozy_auth::encrypt(&app.encryption_key, &secret).map_err(|_| invalid("Could not secure webhook secret"))?;
    let webhook_id = Uuid::new_v4();
    let mut tx = app.db.begin().await?;
    within_limit(
        &mut tx,
        id.user_id,
        "SELECT count(*) FROM ozy.webhooks WHERE user_id=$1",
        MAX_WEBHOOKS,
        "webhook relays",
    )
    .await?;
    sqlx::query("INSERT INTO ozy.webhooks(id,user_id,destination,secret_ciphertext,event_types) VALUES($1,$2,$3,$4,$5)")
        .bind(webhook_id)
        .bind(id.user_id)
        .bind(&destination)
        .bind(ciphertext)
        .bind(&events)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id":webhook_id,"receive_url":format!("{}/api/v1/hooks/{webhook_id}",app.origin),
        "destination":destination,"event_types":events,"secret":secret,
        "notice":"Copy the secret now; it is shown once. Senders must sign requests; OZY re-signs forwarded events with the same secret.",
        "inbound_signature":["x-ozy-timestamp + x-ozy-signature: hex HMAC-SHA256(secret, timestamp + '.' + body), timestamp within 300 s",
            "or x-hub-signature-256: sha256=<hex HMAC-SHA256(secret, body)> (GitHub compatible)"]}),
    ))
}

pub async fn list_webhooks(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let items: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT w.id,w.destination,w.event_types,w.enabled,w.created_at,
         (SELECT count(*) FROM ozy.webhook_events e WHERE e.webhook_id=w.id) AS events,
         (SELECT count(*) FROM ozy.webhook_events e WHERE e.webhook_id=w.id AND e.state='failed') AS failed
         FROM ozy.webhooks w WHERE w.user_id=$1) x",
    )
    .bind(id.user_id)
    .fetch_one(&app.db)
    .await?;
    let items = match items {
        Value::Array(list) => Value::Array(
            list.into_iter()
                .map(|mut w| {
                    w["receive_url"] = json!(format!("{}/api/v1/hooks/{}", app.origin, w["id"].as_str().unwrap_or("")));
                    w
                })
                .collect(),
        ),
        other => other,
    };
    Ok(Json(json!({"webhooks":items,"limit":MAX_WEBHOOKS})))
}

pub async fn delete_webhook(State(app): State<App>, Extension(id): Extension<Identity>, Path(webhook): Path<Uuid>) -> Result<Json<Value>> {
    let deleted = sqlx::query("DELETE FROM ozy.webhooks WHERE id=$1 AND user_id=$2")
        .bind(webhook)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(missing());
    }
    Ok(Json(json!({"deleted":true})))
}

pub async fn deliveries(State(app): State<App>, Extension(id): Extension<Identity>, Path(webhook): Path<Uuid>) -> Result<Json<Value>> {
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM ozy.webhooks WHERE id=$1 AND user_id=$2")
        .bind(webhook)
        .bind(id.user_id)
        .fetch_optional(&app.db)
        .await?;
    owned.ok_or_else(missing)?;
    let events: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT e.id,e.external_id,e.event_type,e.state,e.attempts,
         e.next_attempt,e.last_status,e.last_error,e.created_at,
         (SELECT COALESCE(jsonb_agg(a ORDER BY a.attempted_at),'[]') FROM (SELECT attempt,status,error,duration_ms,attempted_at
           FROM ozy.webhook_attempts WHERE event_id=e.id) a) AS attempts_log
         FROM ozy.webhook_events e WHERE e.webhook_id=$1 ORDER BY e.created_at DESC LIMIT 100) x",
    )
    .bind(webhook)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(
        json!({"deliveries":events,"retry_policy":{"max_attempts":WEBHOOK_MAX_ATTEMPTS,"backoff":"30s doubling"}}),
    ))
}

/// Inbound side of the relay. Only correctly signed requests are accepted, which
/// prevents the endpoint from being used as an open relay.
pub async fn receive_webhook(State(app): State<App>, Path(webhook): Path<Uuid>, headers: HeaderMap, body: Bytes) -> Result<Response> {
    if body.len() > 256 * 1024 {
        return Err(Failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Webhook payloads must be at most 256 KiB".into(),
        ));
    }
    let row = sqlx::query("SELECT user_id,secret_ciphertext,event_types FROM ozy.webhooks WHERE id=$1 AND enabled")
        .bind(webhook)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(missing)?;
    let user: Uuid = row.get("user_id");
    for (key, window, limit) in [
        (format!("hook:{webhook}"), 60, 60),
        (format!("hooks:day:{user}"), 86_400, 500),
        ("platform:day".into(), 86_400, app.limits.platform_per_day),
    ] {
        if !ozy_db::take_budget(&app.db, &key, window, limit, 1).await? {
            return Err(Failure(
                StatusCode::TOO_MANY_REQUESTS,
                "quota_exceeded",
                "Webhook relay quota reached".into(),
            ));
        }
    }
    let secret = ozy_auth::decrypt(&app.encryption_key, &row.get::<String, _>("secret_ciphertext")).map_err(|_| {
        Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "secret_unavailable",
            "Webhook secret could not be read".into(),
        )
    })?;
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let verified = match (header("x-ozy-timestamp"), header("x-ozy-signature"), header("x-hub-signature-256")) {
        (Some(timestamp), Some(signature), _) => {
            let fresh = timestamp.parse::<i64>().is_ok_and(|t| (unix_now() - t).abs() <= 300);
            fresh && ozy_auth::verify_signature(&secret, timestamp, &body, signature)
        }
        (_, _, Some(signature)) => ozy_auth::verify_body_signature(&secret, &body, signature),
        _ => false,
    };
    if !verified {
        return Err(Failure(
            StatusCode::UNAUTHORIZED,
            "invalid_signature",
            "Missing, stale or invalid webhook signature".into(),
        ));
    }
    let payload: Value = serde_json::from_slice(&body).map_err(|_| invalid("Webhook payload must be JSON"))?;
    let event_type = header("x-ozy-event-type")
        .or_else(|| header("x-github-event"))
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("type")
                .or_else(|| payload.get("event"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".into());
    let event_type = ozy_core::bounded_text(&event_type, 64);
    let external_id = header("x-ozy-event-id")
        .or_else(|| header("x-github-delivery"))
        .map(|v| ozy_core::bounded_text(v, 128))
        .unwrap_or_else(|| ozy_core::digest(&body));
    let patterns: Vec<String> = row.get("event_types");
    let accepted = event_matches(&patterns, &event_type);
    let event_id = Uuid::new_v4();
    let inserted = sqlx::query(
        "INSERT INTO ozy.webhook_events(id,webhook_id,external_id,event_type,payload,state) VALUES($1,$2,$3,$4,$5,$6)
         ON CONFLICT(webhook_id,external_id) DO NOTHING RETURNING id",
    )
    .bind(event_id)
    .bind(webhook)
    .bind(&external_id)
    .bind(&event_type)
    .bind(&payload)
    .bind(if accepted { "pending" } else { "filtered" })
    .fetch_optional(&app.db)
    .await?;
    if inserted.is_none() {
        return Ok((StatusCode::OK, Json(json!({"duplicate":true,"external_id":external_id}))).into_response());
    }
    let state = if accepted {
        deliver_event(&app, event_id).await?
    } else {
        "filtered".into()
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"event_id":event_id,"event_type":event_type,"state":state})),
    )
        .into_response())
}

/// One delivery attempt with exponential backoff bookkeeping.
pub async fn deliver_event(app: &App, event_id: Uuid) -> Result<String> {
    let Some(row) = sqlx::query(
        "UPDATE ozy.webhook_events e SET lease_until=now()+interval '60 seconds' FROM ozy.webhooks w
         WHERE e.id=$1 AND e.webhook_id=w.id AND e.state='pending' AND (e.lease_until IS NULL OR e.lease_until<now())
         RETURNING e.event_type,e.payload,e.attempts,w.destination,w.secret_ciphertext,w.user_id",
    )
    .bind(event_id)
    .fetch_optional(&app.db)
    .await?
    else {
        return Ok("in_progress".into());
    };
    let attempts: i32 = row.get::<i32, _>("attempts") + 1;
    let user: Uuid = row.get("user_id");
    let destination: String = row.get("destination");
    let event_type: String = row.get("event_type");
    let payload: Value = row.get("payload");
    let started = Instant::now();
    let outcome = match ozy_auth::decrypt(&app.encryption_key, &row.get::<String, _>("secret_ciphertext")) {
        Ok(secret) => {
            send_signed(
                app,
                &destination,
                &secret,
                &event_id.to_string(),
                &event_type,
                payload.to_string().into_bytes(),
            )
            .await
        }
        Err(_) => Err("Webhook secret could not be read".into()),
    };
    let (status, error) = match &outcome {
        Ok(code) if (200..300).contains(code) => (Some(i32::from(*code)), None),
        Ok(code) => (Some(i32::from(*code)), Some(format!("Destination returned HTTP {code}"))),
        Err(message) => (None, Some(message.clone())),
    };
    sqlx::query("INSERT INTO ozy.webhook_attempts(event_id,attempt,status,error,duration_ms) VALUES($1,$2,$3,$4,$5)")
        .bind(event_id)
        .bind(attempts)
        .bind(status)
        .bind(&error)
        .bind(started.elapsed().as_millis() as i64)
        .execute(&app.db)
        .await?;
    let state = if error.is_none() {
        "delivered"
    } else if attempts >= WEBHOOK_MAX_ATTEMPTS {
        "failed"
    } else {
        "pending"
    };
    sqlx::query(
        "UPDATE ozy.webhook_events SET state=$2,attempts=$3,last_status=$4,last_error=$5,lease_until=NULL,
         next_attempt=now()+make_interval(secs=>30*power(2,$3-1)) WHERE id=$1",
    )
    .bind(event_id)
    .bind(state)
    .bind(attempts)
    .bind(status)
    .bind(&error)
    .execute(&app.db)
    .await?;
    if state == "failed" {
        notify(
            app,
            user,
            "webhook.delivery_failed",
            &format!("Event {event_id} to {destination} failed after {attempts} attempts"),
        )
        .await;
    }
    Ok(state.into())
}

// ───────────────────────────── Scheduler ─────────────────────────────

async fn lease_due(app: &App, sql: &str) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(sql).fetch_all(&app.db).await?)
}

/// Called by Supabase pg_cron (every minute) and the Vercel daily cron fallback.
/// Authenticated with `Authorization: Bearer $CRON_SECRET`.
pub async fn tick(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if app.cron_secret.len() < 32 || !ozy_auth::constant_eq(presented, &app.cron_secret) {
        return Err(unauthorized());
    }
    let started = Instant::now();
    let monitors = lease_due(
        &app,
        "UPDATE ozy.monitors SET lease_until=now()+interval '2 minutes' WHERE id IN (SELECT id FROM ozy.monitors
         WHERE enabled AND next_check<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY next_check LIMIT 10 FOR UPDATE SKIP LOCKED) RETURNING id",
    )
    .await?;
    let events = lease_due(
        &app,
        "SELECT id FROM ozy.webhook_events WHERE state='pending' AND next_attempt<=now() AND (lease_until IS NULL OR lease_until<now())
         ORDER BY next_attempt LIMIT 20",
    )
    .await?;
    let extractions = lease_due(
        &app,
        "UPDATE ozy.extractions SET lease_until=now()+interval '2 minutes' WHERE id IN (SELECT id FROM ozy.extractions
         WHERE next_refresh<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY next_refresh LIMIT 5 FOR UPDATE SKIP LOCKED) RETURNING id",
    )
    .await?;
    let crawls = lease_due(
        &app,
        "SELECT id FROM ozy.crawls WHERE state='queued' AND (lease_until IS NULL OR lease_until<now()) ORDER BY updated_at LIMIT 2",
    )
    .await?;
    let monitor_results = stream::iter(monitors.clone().into_iter().map(|id| check_monitor(&app, id)))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let event_results = stream::iter(events.clone().into_iter().map(|id| deliver_event(&app, id)))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let extraction_results = stream::iter(extractions.clone().into_iter().map(|id| refresh_extraction(&app, id)))
        .buffer_unordered(2)
        .collect::<Vec<_>>()
        .await;
    let mut crawl_errors = 0;
    for id in &crawls {
        if started.elapsed() > Duration::from_secs(30) {
            break;
        }
        if run_crawl(&app, *id, Duration::from_secs(10)).await.is_err() {
            crawl_errors += 1;
        }
    }
    for sql in [
        "DELETE FROM ozy.sessions WHERE expires_at<now()",
        "DELETE FROM ozy.cache WHERE expires_at<now()",
        "DELETE FROM ozy.budgets WHERE created_at<now()-interval '2 days'",
        "DELETE FROM ozy.domain_leases WHERE next_request<now()-interval '1 hour'",
        "DELETE FROM ozy.request_logs WHERE created_at<now()-interval '30 days'",
        "DELETE FROM ozy.monitor_checks WHERE checked_at<now()-interval '30 days'",
        "DELETE FROM ozy.webhook_events WHERE created_at<now()-interval '30 days' AND state<>'pending'",
    ] {
        sqlx::query(sql).execute(&app.db).await?;
    }
    let summary = json!({"monitors_checked":monitors.len(),"monitor_errors":monitor_results.iter().filter(|r| r.is_err()).count(),
        "webhook_attempts":events.len(),"webhook_errors":event_results.iter().filter(|r| r.is_err()).count(),
        "extractions_refreshed":extractions.len(),"extraction_errors":extraction_results.iter().filter(|r| r.is_err()).count(),
        "crawls_advanced":crawls.len(),"crawl_errors":crawl_errors,"duration_ms":started.elapsed().as_millis() as u64});
    sqlx::query("INSERT INTO ozy.system_state(key,value,updated_at) VALUES('last_tick',$1,now()) ON CONFLICT(key) DO UPDATE SET value=$1,updated_at=now()")
        .bind(&summary)
        .execute(&app.db)
        .await?;
    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_filters() {
        let patterns = vec!["order.*".to_owned(), "push".to_owned()];
        assert!(event_matches(&patterns, "order.created"));
        assert!(event_matches(&patterns, "push"));
        assert!(!event_matches(&patterns, "pull_request"));
        assert!(event_matches(&["*".to_owned()], "anything"));
        assert!(valid_event_pattern("invoice.paid"));
        assert!(!valid_event_pattern("bad pattern"));
        assert!(!valid_event_pattern(""));
    }
    #[test]
    fn webhook_destinations_must_be_public_https() {
        assert!(webhook_destination("https://hooks.example.com/x").is_ok());
        assert!(webhook_destination("http://hooks.example.com/x").is_err());
        assert!(webhook_destination("https://127.0.0.1/x").is_err());
        assert!(webhook_destination("https://metadata.google.internal/").is_err());
    }
}
