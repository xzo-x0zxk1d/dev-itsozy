//! Vercel Function entry point (official `vercel_runtime` with Axum).
//! Every path is rewritten here by vercel.json; routing happens in ozy-api.
use tower::ServiceBuilder;
use vercel_runtime::{Error, axum::VercelLayer};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let router = match ozy_api::from_env().await {
        Ok(app) => ozy_api::router(app),
        Err(error) => {
            tracing::error!(%error, "configuration incomplete; serving 503 for APIs");
            ozy_api::unconfigured_router(error.to_string())
        }
    };
    vercel_runtime::run(ServiceBuilder::new().layer(VercelLayer::new()).service(router)).await
}
