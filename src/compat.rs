use axum::{
    Router,
    body::Body,
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    },
    response::Response,
    routing::get,
};

use crate::error::ServiceError;

const HELIUM_HOME: &str = "https://helium.computer";
const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";

pub(crate) fn app() -> Router {
    Router::new()
        .route("/", get(root).head(root))
        .route("/robots.txt", get(robots).head(robots_head))
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
