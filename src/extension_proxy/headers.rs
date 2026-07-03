use axum::http::{HeaderMap, HeaderValue};

pub(crate) const SAFE_REQUEST_HEADERS: &[&str] = &[
    "user-agent",
    "accept-encoding",
    "range",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-range",
];

pub(crate) const SAFE_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-length",
    "vary",
    "content-range",
];

pub(crate) fn copy_allowed_request_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for name in SAFE_REQUEST_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|value| value.to_str().ok()) {
            builder = builder.header(*name, value);
        }
    }
    builder
}

pub(crate) fn copy_allowed_response_headers(
    target: &mut HeaderMap,
    source: &reqwest::header::HeaderMap,
) -> Result<(), crate::error::ServiceError> {
    for name in SAFE_RESPONSE_HEADERS {
        if let Some(value) = source.get(*name) {
            target.insert(
                axum::http::HeaderName::from_static(name),
                HeaderValue::from_bytes(value.as_bytes())
                    .map_err(crate::error::ServiceError::internal)?,
            );
        }
    }
    Ok(())
}
