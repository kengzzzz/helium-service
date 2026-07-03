use axum::{
    Router,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
        header::{
            ACCEPT_ENCODING, ALLOW, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
            ETAG, IF_NONE_MATCH, VARY,
        },
    },
    response::Response,
};

use crate::{error::ServiceError, ubo::UboService};

pub fn app(service: UboService) -> Router {
    Router::new().fallback(handle_request).with_state(service)
}

async fn handle_request(
    State(service): State<UboService>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ServiceError> {
    if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Err(ServiceError::with_status(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        ));
    }

    if !accepts_brotli(&headers) {
        return Err(ServiceError::with_status(
            StatusCode::NOT_ACCEPTABLE,
            "this service can only respond with brotli-encodedresponses",
        ));
    }

    let data = if uri.path() == "/assets.json" {
        service.handle_assets().await?
    } else {
        service.handle_filterlist(uri.path().to_string()).await?
    };

    let cached_on_client = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(", ")
                .take(8)
                .any(|candidate| candidate == data.etag.as_ref())
        })
        .unwrap_or(false);

    let status = if method == Method::OPTIONS {
        StatusCode::NO_CONTENT
    } else if cached_on_client {
        StatusCode::NOT_MODIFIED
    } else {
        StatusCode::OK
    };

    let include_body = method == Method::GET && !cached_on_client;
    let mut response = if include_body {
        Response::new(Body::from(data.body.clone()))
    } else {
        Response::new(Body::empty())
    };
    *response.status_mut() = status;

    let headers = response.headers_mut();
    if method == Method::OPTIONS {
        headers.insert(ALLOW, HeaderValue::from_static("OPTIONS, GET, HEAD"));
    }
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&data.content_type).map_err(ServiceError::internal)?,
    );
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&data.body.len().to_string()).map_err(ServiceError::internal)?,
    );
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
    headers.insert(
        ETAG,
        HeaderValue::from_str(&data.etag).map_err(ServiceError::internal)?,
    );
    headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));

    Ok(response)
}

fn accepts_brotli(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(", ")
                .take(8)
                .any(|enc| enc == "*" || enc == "br")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::ACCEPT_ENCODING};

    use super::*;

    #[test]
    fn accepts_brotli_matches_upstream_split_behavior() {
        let mut headers = HeaderMap::new();
        assert!(!accepts_brotli(&headers));

        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip,br"));
        assert!(!accepts_brotli(&headers));

        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, br"));
        assert!(accepts_brotli(&headers));

        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("*"));
        assert!(accepts_brotli(&headers));
    }
}
