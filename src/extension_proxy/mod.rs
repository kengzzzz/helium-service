use std::sync::Arc;

use axum::{Router, routing::any};
use reqwest::Client;

use crate::error::ServiceError;

mod config;
mod handlers;
mod headers;
mod omaha;
mod proxy;
mod signing;

pub use config::ExtensionProxyConfig;

#[derive(Clone)]
pub struct ExtensionProxyService {
    client: Client,
    config: Arc<ExtensionProxyConfig>,
    omaha: omaha::OmahaState,
}

impl ExtensionProxyService {
    pub fn new(config: Arc<ExtensionProxyConfig>) -> Self {
        Self {
            client: Client::new(),
            config,
            omaha: omaha::OmahaState::default(),
        }
    }

    async fn wrap_url(&self, url: &str) -> Result<String, ServiceError> {
        proxy::wrap(self, url)
    }

    async fn unwrap_url(&self, url: &str) -> Result<String, ServiceError> {
        proxy::unwrap(self, url)
    }
}

pub fn app(service: ExtensionProxyService) -> Router {
    Router::new()
        .route("/proxy", any(handlers::handle))
        .route("/cws_snippet", any(handlers::handle))
        .route("/com", any(handlers::handle))
        .route("/", any(handlers::handle))
        .with_state(service)
}

fn status_error(status: axum::http::StatusCode, text: impl Into<String>) -> ServiceError {
    let text = text.into();
    ServiceError::with_status(status, format!("error {}: {text}", status.as_u16()))
}

fn bad_request(text: impl Into<String>) -> ServiceError {
    status_error(axum::http::StatusCode::BAD_REQUEST, text)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn is_valid_app_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| matches!(byte, b'a'..=b'p'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_validation_matches_chrome_extension_shape() {
        assert!(is_valid_app_id("abcdefghijklmnopabcdefghijklmnop"));
        assert!(!is_valid_app_id("abcdefghijklmnopabcdefghijklmnox"));
        assert!(!is_valid_app_id("short"));
    }
}
