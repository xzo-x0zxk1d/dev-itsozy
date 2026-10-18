//! Local development server: `cargo run --bin ozy-local`.
//! Loads `.env` (local secrets) and then `OZY_ENV_FILE` or `.env.preview.local`
//! (the preview database), so local work never touches production data by default.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename(std::env::var("OZY_ENV_FILE").unwrap_or_else(|_| ".env.preview.local".into()));
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let app = ozy_api::from_env().await?;
    let port = std::env::var("PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(3917);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!("OZY listening on http://127.0.0.1:{port}");
    axum::serve(listener, ozy_api::router(app)).await?;
    Ok(())
}
