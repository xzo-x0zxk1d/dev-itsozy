//! Optional, separately deployed OZY worker.
//!
//! Serves the jobs Vercel functions cannot run: Tesseract OCR (feature `ocr`) and
//! headless Chromium screenshots, rendering and browser metrics (feature `browser`).
//! The API calls it with `Authorization: Bearer $OZY_WORKER_SECRET`.
//!
//!   OZY_WORKER_SECRET=... BIND=0.0.0.0:8080 cargo run -p ozy-worker --features ocr,browser
mod preprocess;
mod proxy;

#[cfg(feature = "browser")]
mod browser;
#[cfg(feature = "ocr")]
mod ocr;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Worker {
    secret: Arc<String>,
    jobs: Arc<Semaphore>,
    #[cfg_attr(not(feature = "browser"), allow(dead_code))]
    proxy: SocketAddr,
}

fn failure(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({"error":{"code":code,"message":message.into()}}))).into_response()
}

fn authorized(worker: &Worker, headers: &HeaderMap) -> bool {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    ozy_auth::constant_eq(presented, &worker.secret)
}

fn capabilities() -> Value {
    json!({"ocr": cfg!(feature = "ocr"), "browser": cfg!(feature = "browser")})
}

async fn health() -> Json<Value> {
    Json(json!({"service":"ozy-worker","version":env!("CARGO_PKG_VERSION"),"capabilities":capabilities()}))
}

async fn job(
    State(worker): State<Worker>,
    axum::extract::Path(kind): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    if !authorized(&worker, &headers) {
        return failure(StatusCode::UNAUTHORIZED, "unauthorized", "Invalid worker credentials");
    }
    let Ok(_permit) = worker.jobs.clone().try_acquire_owned() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "worker_busy",
            "The worker is at capacity; retry shortly",
        );
    };
    let result = match kind.as_str() {
        "ocr" => run_ocr(input).await,
        "screenshot" | "render" | "performance" => run_browser(&worker, &kind, input).await,
        _ => return failure(StatusCode::NOT_FOUND, "not_found", "Unknown job type"),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err((status, message)) => failure(status, "job_failed", message),
    }
}

type JobResult = Result<Value, (StatusCode, String)>;

async fn run_ocr(input: Value) -> JobResult {
    let options: preprocess::Options = serde_json::from_value(input["options"].clone()).unwrap_or_default();
    let images = input["images"]
        .as_array()
        .filter(|i| !i.is_empty() && i.len() <= 5)
        .ok_or((StatusCode::BAD_REQUEST, "1-5 images required".to_owned()))?;
    let mut decoded = Vec::new();
    for image in images {
        let bytes = B64
            .decode(image.as_str().unwrap_or_default())
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid base64 image".to_owned()))?;
        if bytes.len() > 1024 * 1024 {
            return Err((StatusCode::BAD_REQUEST, "Each image must be at most 1 MiB".to_owned()));
        }
        decoded.push(bytes);
    }
    let results = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, String> {
        decoded
            .iter()
            .map(|bytes| {
                let prepared = preprocess::prepare(bytes, &options)?;
                let (text, confidence) = recognize(&prepared, &options)?;
                Ok(
                    json!({"text":text,"confidence":confidence,"width":prepared.original.0,"height":prepared.original.1,
                    "preprocessing":{"grayscale":true,"threshold":prepared.threshold,"scale":options.scale,"psm":options.psm}}),
                )
            })
            .collect()
    })
    .await
    .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "OCR job crashed".to_owned()))?
    .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))?;
    Ok(json!({"results":results,"engine":"tesseract","language":"eng"}))
}

#[cfg(feature = "ocr")]
fn recognize(prepared: &preprocess::Prepared, options: &preprocess::Options) -> Result<(String, f32), String> {
    ocr::recognize(prepared, options)
}
#[cfg(not(feature = "ocr"))]
fn recognize(_: &preprocess::Prepared, _: &preprocess::Options) -> Result<(String, f32), String> {
    Err("This worker was built without the `ocr` feature".into())
}

#[cfg(feature = "browser")]
async fn run_browser(worker: &Worker, kind: &str, input: Value) -> JobResult {
    let url = input["url"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "url is required".to_owned()))?;
    // Validate up front for a clear error; the proxy enforces it again for every request.
    ozy_core::net::validate_url(url).map_err(|_| (StatusCode::BAD_REQUEST, "Destination is not permitted".to_owned()))?;
    let job = async {
        match kind {
            "screenshot" => browser::screenshot(worker.proxy, &input).await,
            "render" => browser::render(worker.proxy, &input).await,
            _ => browser::performance(worker.proxy, &input).await,
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(40), job)
        .await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "Browser job exceeded 40 seconds".to_owned()))?
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}
#[cfg(not(feature = "browser"))]
async fn run_browser(_: &Worker, _: &str, _: Value) -> JobResult {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        "This worker was built without the `browser` feature".into(),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let secret = std::env::var("OZY_WORKER_SECRET").map_err(|_| "OZY_WORKER_SECRET is required")?;
    if secret.len() < 32 {
        return Err("OZY_WORKER_SECRET must be at least 32 characters".into());
    }
    let concurrency = std::env::var("OZY_WORKER_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let worker = Worker {
        secret: Arc::new(secret),
        jobs: Arc::new(Semaphore::new(concurrency)),
        proxy: proxy::start().await?,
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/{kind}", post(job))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(worker);
    let bind = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, capabilities = %capabilities(), "ozy-worker listening");
    axum::serve(listener, app).await?;
    Ok(())
}
