use std::{
    collections::HashMap,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::http::StatusCode;
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

#[derive(Clone)]
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
    Negative {
        expiry: Instant,
    },
}

pub(crate) struct CacheStats {
    pub(crate) count: usize,
    pub(crate) negative: usize,
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
        if let Some(item) = self.cached(&key).await? {
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
                    let result = match source().await {
                        Ok(value) => {
                            let item = build_cached_item(value, &options).await?;
                            cache
                                .store_positive(future_key.clone(), item.clone(), options.expiry)
                                .await;
                            Ok(item)
                        }
                        Err(err) => {
                            cache.store_negative(future_key.clone()).await;
                            Err(err)
                        }
                    };
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
        let mut negative = 0;
        let mut size = 0;
        for entry in entries.values() {
            match entry {
                CacheEntry::Positive { item, .. } => size += item.body.len(),
                CacheEntry::Negative { .. } => negative += 1,
            }
        }

        CacheStats {
            count: entries.len(),
            negative,
            size,
        }
    }

    async fn cached(&self, key: &str) -> Result<Option<CachedItem>, ServiceError> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        match entries.get(key).cloned() {
            Some(CacheEntry::Positive { item, expiry }) => {
                if expiry.is_none_or(|expiry| expiry > now) {
                    Ok(Some(item))
                } else {
                    entries.remove(key);
                    Ok(None)
                }
            }
            Some(CacheEntry::Negative { expiry }) => {
                if expiry > now {
                    Err(ServiceError::with_status(
                        StatusCode::NOT_FOUND,
                        "Not Found",
                    ))
                } else {
                    entries.remove(key);
                    Ok(None)
                }
            }
            None => Ok(None),
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

    async fn store_negative(&self, key: String) {
        self.entries.lock().await.insert(
            key,
            CacheEntry::Negative {
                expiry: Instant::now() + Duration::from_secs(30),
            },
        );
    }

    async fn cleanup_expired(&self) {
        let now = Instant::now();
        self.entries.lock().await.retain(|_, entry| match entry {
            CacheEntry::Positive { expiry, .. } => expiry.is_none_or(|expiry| expiry > now),
            CacheEntry::Negative { expiry } => *expiry > now,
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
