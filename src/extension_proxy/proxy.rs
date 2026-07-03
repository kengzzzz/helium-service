use axum::http::StatusCode;
use url::Url;

use crate::error::ServiceError;

use super::{ExtensionProxyService, bad_request, now_ms, signing, status_error};

const ONE_HOUR_MS: u64 = 60 * 60 * 1000;

pub(crate) fn wrap(service: &ExtensionProxyService, url: &str) -> Result<String, ServiceError> {
    parse_url_strict(url)?;

    let Some(base_url) = &service.config.proxy_base_url else {
        return Ok(url.to_string());
    };
    let Some(secret) = &service.config.hmac_secret else {
        return Ok(url.to_string());
    };

    let expiry = now_ms() + ONE_HOUR_MS;
    let mut proxy_url = base_url.clone();
    if !proxy_url.path().ends_with('/') {
        proxy_url.set_path(&format!("{}/", proxy_url.path()));
    }
    let path = format!("{}proxy", proxy_url.path());
    proxy_url.set_path(&path);
    proxy_url.query_pairs_mut().clear().append_pair("url", url);
    proxy_url
        .query_pairs_mut()
        .append_pair("sig", &signing::sign(secret, url, expiry)?)
        .append_pair("exp", &expiry.to_string());

    Ok(proxy_url.to_string())
}

pub(crate) fn unwrap(service: &ExtensionProxyService, url: &str) -> Result<String, ServiceError> {
    if !service.config.proxying_enabled() {
        return Err(status_error(
            StatusCode::NOT_FOUND,
            "content proxying is disabled",
        ));
    }

    let url = Url::parse(url).map_err(|_| bad_request("malformed url"))?;
    let original_url = url
        .query_pairs()
        .find_map(|(key, value)| (key == "url").then(|| value.into_owned()))
        .ok_or_else(|| bad_request("malformed url"))?;
    let signature = url
        .query_pairs()
        .find_map(|(key, value)| (key == "sig").then(|| value.into_owned()))
        .ok_or_else(|| bad_request("malformed url"))?;
    let expiry = url
        .query_pairs()
        .find_map(|(key, value)| (key == "exp").then(|| value.into_owned()))
        .ok_or_else(|| bad_request("malformed url"))?;
    let expiry = expiry
        .parse::<u64>()
        .map_err(|_| bad_request("malformed url"))?;

    parse_url_strict(&original_url)?;

    let secret = service.config.hmac_secret.as_deref().unwrap_or_default();
    if !signing::verify(secret, &original_url, expiry, &signature)? {
        return Err(bad_request("signature verification failed"));
    }

    if now_ms() > expiry {
        return Err(status_error(StatusCode::GONE, "URL expired"));
    }

    Ok(original_url)
}

pub(crate) fn parse_url_strict(value: &str) -> Result<Url, ServiceError> {
    let url = Url::parse(value).map_err(|_| bad_request("only http/https urls are supported"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad_request("only http/https urls are supported"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(bad_request(
            "usernames/passwords/ports in url are disallowed",
        ));
    }

    Ok(url)
}
