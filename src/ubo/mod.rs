use std::{sync::Arc, time::Duration};

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
}

impl UboService {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            client: Client::new(),
            config,
            allowlist: Allowlist::default(),
            cache: Cache::default(),
        }
    }

    pub fn preload_assets(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let _ = service.handle_assets().await;
        });
    }

    pub fn spawn_cache_cleanup(&self) {
        self.cache.spawn_cleanup();
    }

    pub fn spawn_stats_logger(&self) {
        let cache = self.cache.clone();
        tokio::spawn(async move {
            let mut previous = 0;
            loop {
                time::sleep(Duration::from_secs(60 * 60)).await;
                let (hits, misses) = cache.counts();
                let sum = hits + misses;
                if sum == previous {
                    continue;
                }

                previous = sum;
                let hit_rate = if sum == 0 {
                    0.0
                } else {
                    (hits as f64 / sum as f64) * 100.0
                };
                println!("Cache hit rate: {hits} / {misses} ({hit_rate:.2}% hit rate)");

                let stats = cache.stats().await;
                println!(
                    "Cache size: {} (+ negative {}) items in cache taking up {} KB",
                    stats.count,
                    stats.negative,
                    stats.size / 1024
                );
            }
        });
    }

    pub(crate) async fn handle_assets(&self) -> Result<CachedItem, crate::error::ServiceError> {
        assets::handle_assets(self).await
    }

    pub(crate) async fn handle_filterlist(
        &self,
        path: String,
    ) -> Result<CachedItem, crate::error::ServiceError> {
        filterlist::handle_filterlist(self, path).await
    }
}
