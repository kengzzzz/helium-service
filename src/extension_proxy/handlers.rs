use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header::CONTENT_TYPE},
    response::Response,
};
use bytes::Bytes;

use crate::error::ServiceError;

use super::{ExtensionProxyService, bad_request, headers, is_valid_app_id, omaha, status_error};

const BODY_LIMIT: usize = 1024 * 1024;
const CHROME_WEBSTORE_SNIPPET: &str =
    "https://chromewebstore.googleapis.com/v2/items/{}:fetchItemSnippet";

pub(crate) async fn handle(
    State(service): State<ExtensionProxyService>,
    request: Request<Body>,
) -> Result<Response, ServiceError> {
    let path = request.uri().path().to_string();
    match path.as_str() {
        "/proxy" => handle_payload_proxy(service, request).await,
        "/cws_snippet" => handle_snippet_proxy(service, request).await,
        "/com" | "/" => omaha::handle_omaha_query(service, request).await,
        _ => Err(status_error(StatusCode::NOT_FOUND, "Not Found")),
    }
}

async fn handle_payload_proxy(
    service: ExtensionProxyService,
    request: Request<Body>,
) -> Result<Response, ServiceError> {
    if request.method() != Method::GET {
        return Err(status_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        ));
    }

    let url = absolute_request_url(request.uri())?;
    let original_url = service.unwrap_url(&url).await?;
    proxy_get(service, &original_url, request.headers()).await
}

async fn handle_snippet_proxy(
    service: ExtensionProxyService,
    request: Request<Body>,
) -> Result<Response, ServiceError> {
    if !matches!(*request.method(), Method::GET | Method::POST) {
        return Err(status_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        ));
    }

    let extension_id = request
        .uri()
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find_map(|(key, value)| (key == "id").then(|| value.into_owned()))
        })
        .ok_or_else(|| bad_request("missing or invalid extension id"))?;
    if !is_valid_app_id(&extension_id) {
        return Err(bad_request("missing or invalid extension id"));
    }

    let url = CHROME_WEBSTORE_SNIPPET.replace("{}", &extension_id);
    let response = service
        .client
        .post(url)
        .header("accept", "application/x-protobuf")
        .header("content-type", "application/x-protobuf")
        .header("x-http-method-override", "GET")
        .send()
        .await
        .map_err(ServiceError::internal)?;

    proxy_response(response).await
}

pub(crate) async fn proxy_get(
    service: ExtensionProxyService,
    url: &str,
    request_headers: &HeaderMap,
) -> Result<Response, ServiceError> {
    let builder = service.client.get(url);
    let response = headers::copy_allowed_request_headers(builder, request_headers)
        .send()
        .await
        .map_err(ServiceError::internal)?;
    proxy_response(response).await
}

pub(crate) async fn proxy_response(response: reqwest::Response) -> Result<Response, ServiceError> {
    let status =
        StatusCode::from_u16(response.status().as_u16()).map_err(ServiceError::internal)?;
    let source_headers = response.headers().clone();
    let body = response.bytes().await.map_err(ServiceError::internal)?;

    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    headers::copy_allowed_response_headers(response.headers_mut(), &source_headers)?;
    Ok(response)
}

pub(crate) async fn read_body(body: Body) -> Result<Bytes, ServiceError> {
    to_bytes(body, BODY_LIMIT)
        .await
        .map_err(ServiceError::internal)
}

pub(crate) fn text_response(
    status: StatusCode,
    body: impl Into<Body>,
    content_type: &'static str,
) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn absolute_request_url(uri: &Uri) -> Result<String, ServiceError> {
    let path = uri
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or_else(|| uri.path());
    Ok(format!("http://localhost{path}"))
}
