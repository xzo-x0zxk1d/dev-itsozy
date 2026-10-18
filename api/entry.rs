use axum::{Json, Router, http::StatusCode, response::Html, routing::get};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tower::ServiceBuilder;
use vercel_runtime::{Error, axum::VercelLayer};

async fn health() -> (StatusCode, Json<Value>) {
    let database = match std::env::var("POSTGRES_URL") {
        Ok(url) => match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(8))
            .connect(&url)
            .await
        {
            Ok(pool) => {
                let ok = sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&pool)
                    .await
                    .is_ok();
                pool.close().await;
                ok
            }
            Err(_) => false,
        },
        Err(_) => false,
    };
    (
        if database {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"service":"ozy-platform","phase":"infrastructure-bootstrap","database":database,"runtime":"rust"}),
        ),
    )
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let router = Router::new()
        .route("/", get(|| async { Html("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>OZY — Infrastructure verification</title><h1>OZY Developer Platform</h1><p>Infrastructure verification is in progress. Public APIs are not available yet.</p></html>") }))
        .route("/api/v1/health", get(health));
    vercel_runtime::run(
        ServiceBuilder::new()
            .layer(VercelLayer::new())
            .service(router),
    )
    .await
}
