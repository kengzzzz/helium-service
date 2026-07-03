use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::ServiceError;

const UPDATE_INFO_URL: &str = "https://chromiumdash.appspot.com/fetch_releases?channel=Stable&platform=Windows&num=5&offset=0";

#[derive(Clone, Default)]
pub(crate) struct ChromiumVersionCache {
    inner: Arc<Mutex<CacheData>>,
}

#[derive(Default)]
struct CacheData {
    versions: Vec<String>,
    cached_at: Option<Instant>,
}

impl ChromiumVersionCache {
    pub(crate) async fn random_version(
        &self,
        client: &reqwest::Client,
    ) -> Result<String, ServiceError> {
        let should_refresh = {
            let inner = self.inner.lock().await;
            inner
                .cached_at
                .is_none_or(|cached_at| cached_at + Duration::from_secs(60 * 60) < Instant::now())
        };

        if should_refresh {
            match fetch_versions(client).await {
                Ok(versions) => {
                    let mut inner = self.inner.lock().await;
                    inner.cached_at = Some(Instant::now());
                    inner.versions = versions;
                }
                Err(err) => eprintln!("Error occurred fetching Chromium versions: {err:?}"),
            }
        }

        let inner = self.inner.lock().await;
        if inner.versions.is_empty() {
            return Err(crate::extension_proxy::bad_request(
                "could not get random chrome version",
            ));
        }
        Ok(inner.versions[fastrand::usize(..inner.versions.len())].clone())
    }
}

async fn fetch_versions(client: &reqwest::Client) -> Result<Vec<String>, ServiceError> {
    let response = client
        .get(UPDATE_INFO_URL)
        .send()
        .await
        .map_err(ServiceError::internal)?;
    if !response.status().is_success() {
        return Err(ServiceError::internal("response is not ok"));
    }
    let text = response.text().await.map_err(ServiceError::internal)?;
    let data: Value = serde_json::from_str(&text).map_err(ServiceError::internal)?;
    let array = data
        .as_array()
        .ok_or_else(|| ServiceError::internal("invalid response"))?;
    let mut versions = Vec::with_capacity(array.len());
    for item in array {
        let version = item
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| ServiceError::internal("missing/invalid version in response"))?;
        if !is_valid_version(version) {
            return Err(ServiceError::internal(
                "missing/invalid version in response",
            ));
        }
        versions.push(version.to_string());
    }
    Ok(versions)
}

fn is_valid_version(version: &str) -> bool {
    let mut parts = version.split('.').peekable();
    if parts.peek().is_none() {
        return false;
    }
    parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}
