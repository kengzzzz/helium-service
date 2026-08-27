use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use reqwest::Client;
use tokio::time;

use crate::{
    allowlist::Allowlist,
    cache::{Cache, CachedItem},
    config::Config,
};

pub(crate) mod assets;
mod filterlist;
pub(crate) mod tags;
mod upstream;
pub(crate) mod urls;

#[derive(Clone)]
pub struct UboService {
    pub(crate) client: Client,
    pub(crate) config: Arc<Config>,
    pub(crate) allowlist: Allowlist,
    cache: Cache,
    ready: Arc<AtomicBool>,
}

impl UboService {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            client: crate::upstream::metadata_client()
                .expect("metadata HTTP client configuration must be valid"),
            config,
            allowlist: Allowlist::default(),
            cache: Cache::default(),
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn preload_assets(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            for attempt in 1..=3 {
                if service.handle_assets().await.is_ok() {
                    return;
                }
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "event": "readiness",
                        "component": "ubo",
                        "state": "retrying",
                        "attempt": attempt,
                    })
                );
                if attempt < 3 {
                    time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    pub fn spawn_cache_cleanup(&self) {
        self.cache.spawn_cleanup();
    }

    pub fn spawn_stats_logger(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut previous = 0;
            loop {
                time::sleep(Duration::from_secs(60 * 60)).await;
                let (hits, misses) = service.cache.counts();
                let sum = hits + misses;
                if sum == previous {
                    continue;
                }

                previous = sum;
                let stats = service.cache.stats().await;
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "event": "service_stats",
                        "cache_entries": stats.count,
                        "cache_bytes": stats.size,
                        "cache_hits": hits,
                        "cache_misses": misses,
                        "upstream_failures": crate::observability::upstream_failure_count(),
                        "ubo_ready": service.is_ready(),
                    })
                );
            }
        });
    }

    pub(crate) async fn handle_assets(&self) -> Result<CachedItem, crate::error::ServiceError> {
        let item = assets::handle_assets(self).await?;
        self.ready.store(true, Ordering::Release);
        Ok(item)
    }

    pub(crate) async fn handle_filterlist(
        &self,
        path: String,
    ) -> Result<CachedItem, crate::error::ServiceError> {
        filterlist::handle_filterlist(self, path).await
    }

    #[cfg(test)]
    pub(crate) async fn cache_stats(&self) -> crate::cache::CacheStats {
        self.cache.stats().await
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}
