use std::sync::OnceLock;

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG,
            IF_NONE_MATCH,
        },
    },
    response::Response,
};

use crate::{error::ServiceError, ubo::tags::resource_tag};

const BANGS_JSON: &str = include_str!("../assets/bangs.json");
const CACHE_CONTROL_VALUE: &str = "public, max-age=86400, stale-if-error=604800";
const CONTENT_TYPE_VALUE: &str = "application/json; charset=utf-8";

static ETAG_VALUE: OnceLock<String> = OnceLock::new();

pub(crate) async fn get(headers: HeaderMap) -> Result<Response, ServiceError> {
    response(headers, true)
}

pub(crate) async fn head(headers: HeaderMap) -> Result<Response, ServiceError> {
    response(headers, false)
}

fn response(request_headers: HeaderMap, include_body: bool) -> Result<Response, ServiceError> {
    let etag = etag();
    let cached_on_client = request_headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(", ").take(8).any(|candidate| candidate == etag))
        .unwrap_or(false);

    let body = if include_body && !cached_on_client {
        Body::from(BANGS_JSON)
    } else {
        Body::empty()
    };
    let mut response = Response::new(body);
    *response.status_mut() = if cached_on_client {
        StatusCode::NOT_MODIFIED
    } else {
        StatusCode::OK
    };

    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE_VALUE));
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&BANGS_JSON.len().to_string()).map_err(ServiceError::internal)?,
    );
    headers.insert(
        ETAG,
        HeaderValue::from_str(etag).map_err(ServiceError::internal)?,
    );
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));

    Ok(response)
}

fn etag() -> &'static str {
    ETAG_VALUE.get_or_init(|| resource_tag(BANGS_JSON))
}
