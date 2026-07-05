use axum::{
    Router,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
        header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    },
    response::Response,
    routing::get,
};
use url::Url;

use crate::error::ServiceError;

const HELIUM_HOME: &str = "https://helium.computer";
const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";
const MAC_UPDATES_BASE: &str = "https://updates.helium.computer/mac";
const SAFE_REQUEST_HEADERS: &[&str] = &[
    "user-agent",
    "accept-encoding",
    "range",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-range",
];
const SAFE_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-length",
    "vary",
    "content-range",
];

#[derive(Clone)]
pub(crate) struct CompatibilityService {
    client: reqwest::Client,
    mac_updates_base: Url,
}

impl CompatibilityService {
    fn new(mac_updates_base: Url) -> Self {
        Self {
            client: reqwest::Client::new(),
            mac_updates_base,
        }
    }

    fn default() -> Result<Self, ServiceError> {
        Ok(Self::new(
            Url::parse(MAC_UPDATES_BASE).map_err(ServiceError::internal)?,
        ))
    }

    fn mac_updates_url(&self, uri: &Uri) -> String {
        let suffix = uri.path().strip_prefix("/updates/mac").unwrap_or_default();
        let mut url = self.mac_updates_base.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}{suffix}"));
        url.set_query(uri.query());
        url.to_string()
    }
}

pub(crate) fn app() -> Result<Router, ServiceError> {
    app_with_service(CompatibilityService::default()?)
}

#[cfg(test)]
pub(crate) fn app_with_mac_updates_base(mac_updates_base: Url) -> Result<Router, ServiceError> {
    app_with_service(CompatibilityService::new(mac_updates_base))
}

fn app_with_service(service: CompatibilityService) -> Result<Router, ServiceError> {
    Ok(Router::new()
        .route("/", get(root).head(root))
        .route("/robots.txt", get(robots).head(robots_head))
        .route("/updates/mac", get(mac_updates).head(mac_updates))
        .route("/updates/mac/", get(mac_updates).head(mac_updates))
        .route("/updates/mac/{*path}", get(mac_updates).head(mac_updates))
        .with_state(service))
}

async fn root() -> Result<Response, ServiceError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_static(HELIUM_HOME));
    Ok(response)
}

async fn robots() -> Result<Response, ServiceError> {
    robots_response(true)
}

async fn robots_head() -> Result<Response, ServiceError> {
    robots_response(false)
}

fn robots_response(include_body: bool) -> Result<Response, ServiceError> {
    let body = if include_body {
        Body::from(ROBOTS_TXT)
    } else {
        Body::empty()
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&ROBOTS_TXT.len().to_string()).map_err(ServiceError::internal)?,
    );
    Ok(response)
}

async fn mac_updates(
    State(service): State<CompatibilityService>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ServiceError> {
    let url = service.mac_updates_url(&uri);
    let builder = match method {
        Method::GET => service.client.get(url),
        Method::HEAD => service.client.head(url),
        _ => {
            return Err(ServiceError::with_status(
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed",
            ));
        }
    };

    let response = copy_allowed_request_headers(builder, &headers)
        .send()
        .await
        .map_err(ServiceError::internal)?;
    proxy_response(response, method == Method::GET).await
}

async fn proxy_response(
    response: reqwest::Response,
    include_body: bool,
) -> Result<Response, ServiceError> {
    let status =
        StatusCode::from_u16(response.status().as_u16()).map_err(ServiceError::internal)?;
    let source_headers = response.headers().clone();
    let body = if include_body {
        Body::from(response.bytes().await.map_err(ServiceError::internal)?)
    } else {
        Body::empty()
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    copy_allowed_response_headers(response.headers_mut(), &source_headers)?;
    Ok(response)
}

fn copy_allowed_request_headers(
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

fn copy_allowed_response_headers(
    target: &mut HeaderMap,
    source: &reqwest::header::HeaderMap,
) -> Result<(), ServiceError> {
    for name in SAFE_RESPONSE_HEADERS {
        if let Some(value) = source.get(*name) {
            target.insert(
                axum::http::HeaderName::from_static(name),
                HeaderValue::from_bytes(value.as_bytes()).map_err(ServiceError::internal)?,
            );
        }
    }
    Ok(())
}
