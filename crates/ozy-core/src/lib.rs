pub mod net;
pub mod tls;

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Destination is not permitted")]
    Forbidden,
    #[error("The upstream server could not be reached")]
    Upstream,
    #[error("The upstream response exceeded the size limit")]
    TooLarge,
    #[error("The operation exceeded its time limit")]
    Timeout,
    #[error("The site's robots policy disallows this request")]
    Robots,
    #[error("The upstream server rate limited this request")]
    RateLimited,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn bounded_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
