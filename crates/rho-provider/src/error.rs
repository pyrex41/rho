use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SSE parse error: {0}")]
    SseParse(String),
    #[error("API error: status={status}, body={body}")]
    ApiError { status: u16, body: String },
    #[error("Stream error: {0}")]
    Stream(String),
}
