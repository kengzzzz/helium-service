use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::{
    cache::{CacheOptions, CachedItem},
    error::ServiceError,
    ubo::{
        UboService,
        tags::{sha256_hex, sha256_u32_words},
        urls::{filename_for_source, is_valid_http_url, join_url, posix_dirname},
    },
};

pub(super) async fn handle_assets(service: &UboService) -> Result<CachedItem, ServiceError> {
    let service = service.clone();
    let cache = service.cache.clone();
    cache
        .materialize(
            "assets.json".to_string(),
            CacheOptions {
                content_type: "application/json; charset=utf-8".to_string(),
                expiry: None,
            },
            move || async move { service.prepare_asset_string().await },
        )
        .await
}

impl UboService {
    async fn prepare_asset_string(&self) -> Result<String, ServiceError> {
        let asset_list = self
            .client
            .get(self.config.assets_url()?)
            .send()
            .await
            .map_err(ServiceError::internal)?
            .text()
            .await
            .map_err(ServiceError::internal)?;

        let checksum = sha256_hex(asset_list.as_bytes());
        if checksum != self.config.file_checksum()? {
            eprintln!("[!] assets.json checksum does not match");
            return Err(ServiceError::bad_request(format!(
                "checksum does not match: {checksum}"
            )));
        }

        let mut manifest: Map<String, Value> =
            serde_json::from_str(&asset_list).map_err(ServiceError::internal)?;
        let mut asset_urls: HashMap<String, Vec<String>> = HashMap::new();

        for (id, asset) in manifest.iter_mut() {
            let object = asset
                .as_object_mut()
                .ok_or_else(|| ServiceError::bad_request(format!("invalid asset: {id}")))?;

            let mut all_urls = Vec::new();
            if let Some(content_url) = object.get("contentURL") {
                append_string_values(content_url, &mut all_urls);
            }
            if let Some(cdn_urls) = object.get("cdnURLs") {
                append_string_values(cdn_urls, &mut all_urls);
            }

            object.remove("cdnURLs");

            if id == "assets.json" {
                object.insert(
                    "contentURL".to_string(),
                    Value::String(join_url(&self.config.base_url, "assets.json")?),
                );
                continue;
            }

            let source_urls: Vec<String> = all_urls
                .iter()
                .filter(|url| is_valid_http_url(url))
                .cloned()
                .collect();
            let locals: Vec<String> = all_urls
                .iter()
                .filter(|url| url.starts_with("assets/"))
                .cloned()
                .collect();

            if source_urls.is_empty() {
                let title = object
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(id.as_str());
                return Err(ServiceError::bad_request(format!("no source for {title}")));
            }

            let filename = filename_for_source(&source_urls[0])?;
            let repr_hash = sha256_u32_words(&source_urls[0]);
            let key = format!("{}/{:x}/{:x}/{}", id, repr_hash[0], repr_hash[1], filename);
            let proxy_url = join_url(&self.config.base_url, &key)?;

            if locals.is_empty() {
                object.insert("contentURL".to_string(), Value::String(proxy_url));
            } else {
                let mut urls = vec![Value::String(proxy_url)];
                urls.extend(locals.into_iter().map(Value::String));
                object.insert("contentURL".to_string(), Value::Array(urls));
            }

            if object.contains_key("patchURLs") {
                object.insert(
                    "patchURLs".to_string(),
                    Value::Array(vec![Value::String(join_url(
                        &self.config.base_url,
                        &posix_dirname(&key),
                    )?)]),
                );
            }

            asset_urls.insert(key, source_urls);
        }

        self.allowlist.add_entries("assets.json", asset_urls).await;
        to_pretty_json_4(&Value::Object(manifest)).map_err(ServiceError::internal)
    }
}

fn append_string_values(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                append_string_values(value, output);
            }
        }
        _ => {}
    }
}

pub(crate) fn to_pretty_json_4(value: &Value) -> Result<String, serde_json::Error> {
    let mut output = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
    value.serialize(&mut serializer)?;
    String::from_utf8(output).map_err(|err| serde_json::Error::io(std::io::Error::other(err)))
}
