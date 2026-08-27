use std::{
    collections::HashMap,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use tokio::{sync::Mutex, time};

use crate::{error::ServiceError, ubo::tags::resource_tag};

#[derive(Clone, Default)]
pub(crate) struct Cache {
    entries: Arc<Mutex<HashMap<String, CacheEntry>>>,
    inflight: Arc<Mutex<HashMap<String, SharedCacheFuture>>>,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
}

type SharedCacheFuture = Shared<BoxFuture<'static, Result<CachedItem, ServiceError>>>;

#[derive(Clone)]
pub(crate) struct CacheOptions {
    pub(crate) content_type: String,
    pub(crate) expiry: Option<Duration>,
}

#[derive(Clone, Debug)]
pub(crate) struct CachedItem {
    pub(crate) body: Bytes,
    pub(crate) content_type: Arc<str>,
    pub(crate) etag: Arc<str>,
}

#[derive(Clone)]
enum CacheEntry {
    Positive {
        item: CachedItem,
        expiry: Option<Instant>,
    },
}

pub(crate) struct CacheStats {
    pub(crate) count: usize,
    pub(crate) size: usize,
}

impl Cache {
    pub(crate) async fn materialize<F, Fut>(
        &self,
        key: String,
        options: CacheOptions,
        source: F,
    ) -> Result<CachedItem, ServiceError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<String, ServiceError>> + Send + 'static,
    {
        if let Some(item) = self.cached(&key).await {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(item);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        let future = {
            let mut inflight = self.inflight.lock().await;
            if let Some(future) = inflight.get(&key) {
                future.clone()
            } else {
                let cache = self.clone();
                let future_key = key.clone();
                let future = async move {
                    let result = async {
                        let value = source().await?;
                        let item = build_cached_item(value, &options).await?;
                        cache
                            .store_positive(future_key.clone(), item.clone(), options.expiry)
                            .await;
                        Ok(item)
                    }
                    .await;
                    cache.inflight.lock().await.remove(&future_key);
                    result
                }
                .boxed()
                .shared();
                inflight.insert(key.clone(), future.clone());
                future
            }
        };

        future.await
    }

    pub(crate) fn counts(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn spawn_cleanup(&self) {
        let cache = self.clone();
        tokio::spawn(async move {
            loop {
                time::sleep(Duration::from_secs(60)).await;
                cache.cleanup_expired().await;
            }
        });
    }

    pub(crate) async fn stats(&self) -> CacheStats {
        let entries = self.entries.lock().await;
        let mut size = 0;
        for entry in entries.values() {
            let CacheEntry::Positive { item, .. } = entry;
            size += item.body.len();
        }

        CacheStats {
            count: entries.len(),
            size,
        }
    }

    async fn cached(&self, key: &str) -> Option<CachedItem> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        match entries.get(key).cloned() {
            Some(CacheEntry::Positive { item, expiry }) => {
                if expiry.is_none_or(|expiry| expiry > now) {
                    Some(item)
                } else {
                    entries.remove(key);
                    None
                }
            }
            None => None,
        }
    }

    async fn store_positive(&self, key: String, item: CachedItem, expiry: Option<Duration>) {
        self.entries.lock().await.insert(
            key,
            CacheEntry::Positive {
                item,
                expiry: expiry.map(|duration| Instant::now() + duration),
            },
        );
    }

    async fn cleanup_expired(&self) {
        let now = Instant::now();
        self.entries.lock().await.retain(|_, entry| {
            let CacheEntry::Positive { expiry, .. } = entry;
            expiry.is_none_or(|expiry| expiry > now)
        });
    }
}

async fn build_cached_item(
    value: String,
    options: &CacheOptions,
) -> Result<CachedItem, ServiceError> {
    let etag = resource_tag(&value);
    let body = tokio::task::spawn_blocking(move || brotli_compress_text(&value))
        .await
        .map_err(ServiceError::internal)??;

    Ok(CachedItem {
        body: Bytes::from(body),
        content_type: Arc::from(options.content_type.as_str()),
        etag: Arc::from(etag),
    })
}

fn brotli_compress_text(value: &str) -> Result<Vec<u8>, ServiceError> {
    let mut output = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 11, 22);
        writer
            .write_all(value.as_bytes())
            .map_err(ServiceError::internal)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    fn options() -> CacheOptions {
        CacheOptions {
            content_type: "text/plain".to_string(),
            expiry: Some(Duration::from_secs(60)),
        }
    }

    #[tokio::test]
    async fn concurrent_valid_misses_are_coalesced() {
        let cache = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..20 {
            let cache = cache.clone();
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                cache
                    .materialize("key".to_string(), options(), move || async move {
                        calls.fetch_add(1, Ordering::Relaxed);
                        time::sleep(Duration::from_millis(20)).await;
                        Ok("value".to_string())
                    })
                    .await
            }));
        }

        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn source_errors_are_never_cached() {
        let cache = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let result = cache
                .materialize("key".to_string(), options(), move || async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Err(ServiceError::Upstream {
                        status: axum::http::StatusCode::BAD_GATEWAY,
                    })
                })
                .await;
            let Err(error) = result else {
                panic!("source error was unexpectedly cached as a success");
            };
            assert_eq!(error.status(), axum::http::StatusCode::BAD_GATEWAY);
        }

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.stats().await.count, 0);
    }
}
