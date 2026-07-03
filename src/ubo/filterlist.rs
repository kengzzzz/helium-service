use std::{collections::HashMap, time::Duration};

use axum::http::StatusCode;
use regex::Regex;
use url::Url;

use crate::{
    cache::{CacheOptions, CachedItem},
    error::ServiceError,
    ubo::{
        UboService,
        upstream::shotgun_fetch,
        urls::{join_source_relative, posix_dirname, posix_join},
    },
};

pub(super) async fn handle_filterlist(
    service: &UboService,
    mut path: String,
) -> Result<CachedItem, ServiceError> {
    if let Some(stripped) = path.strip_prefix('/') {
        path = stripped.to_string();
    }

    let service = service.clone();
    let cache = service.cache.clone();
    let source_path = path.clone();
    cache
        .materialize(
            path,
            CacheOptions {
                content_type: "text/plain; charset=utf-8".to_string(),
                expiry: Some(Duration::from_secs(3600)),
            },
            move || async move { service.prepare_filterlist(source_path).await },
        )
        .await
}

impl UboService {
    async fn prepare_filterlist(&self, path: String) -> Result<String, ServiceError> {
        let urls = self
            .allowlist
            .get_urls_for_path(&path)
            .await
            .ok_or_else(|| ServiceError::with_status(StatusCode::NOT_FOUND, "Not Found"))?;

        let text = shotgun_fetch(&self.client, &urls).await?;
        let parent_id = path.split('/').next().unwrap_or_default().to_string();
        let mut to_allowlist = HashMap::new();
        let include_regex =
            Regex::new(r"^!#include +(\S+)[^\n\r]*(?:[\n\r]+|$)").expect("valid include regex");

        for line in text.split('\n') {
            if line.starts_with("!#include") {
                if let Some(include_match) = include_regex.captures(line)
                    && let Some(include_path) = include_match.get(1).map(|m| m.as_str())
                {
                    self.add_relative_allowlist_entry(
                        &path,
                        &parent_id,
                        include_path,
                        &urls,
                        &mut to_allowlist,
                        "include",
                    );
                    continue;
                }
                eprintln!("WARN: erroneous include in  {path} {line}");
            } else if line.starts_with("! Diff-Path")
                && let Some((_, diff_path)) = line.split_once("! Diff-Path:")
            {
                let diff_path = diff_path.trim();
                self.add_relative_allowlist_entry(
                    &path,
                    &parent_id,
                    diff_path,
                    &urls,
                    &mut to_allowlist,
                    "diff",
                );
            }
        }

        self.allowlist.add_entries(&path, to_allowlist).await;
        Ok(text)
    }

    fn add_relative_allowlist_entry(
        &self,
        path: &str,
        parent_id: &str,
        relative_path: &str,
        source_urls: &[String],
        to_allowlist: &mut HashMap<String, Vec<String>>,
        kind: &str,
    ) {
        let mut absolute_path = posix_join(&posix_dirname(path), relative_path);
        if kind == "diff"
            && let Some((before_fragment, _)) = absolute_path.split_once('#')
        {
            absolute_path = before_fragment.to_string();
        }

        let is_absolute_url = if kind == "include" {
            Url::parse(relative_path).is_ok()
        } else {
            Url::parse(&absolute_path).is_ok()
        };

        if is_absolute_url || absolute_path.split('/').next().unwrap_or_default() != parent_id {
            eprintln!("WARN: unsupported {kind} in  {path}");
            return;
        }

        let urls = source_urls
            .iter()
            .filter_map(|base| join_source_relative(base, relative_path))
            .collect::<Vec<_>>();
        to_allowlist.entry(absolute_path).or_insert(urls);
    }
}
