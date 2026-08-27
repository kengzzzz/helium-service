use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, Response};

use crate::error::ServiceError;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const METADATA_READ_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn metadata_client() -> Result<Client, String> {
    client(CONNECT_TIMEOUT, METADATA_READ_TIMEOUT)
}

pub(crate) fn download_client() -> Result<Client, String> {
    client(CONNECT_TIMEOUT, DOWNLOAD_READ_TIMEOUT)
}

pub(crate) fn client(connect_timeout: Duration, read_timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .build()
        .map_err(|err| err.to_string())
}

pub(crate) async fn checked_response(
    response: Result<Response, reqwest::Error>,
    category: &'static str,
) -> Result<Response, ServiceError> {
    let response = response.map_err(|err| ServiceError::upstream(category, &err))?;
    if !response.status().is_success() {
        return Err(ServiceError::bad_gateway(category, "status"));
    }
    Ok(response)
}

pub(crate) async fn read_limited(
    response: Response,
    limit: usize,
    category: &'static str,
) -> Result<Bytes, ServiceError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ServiceError::bad_gateway(category, "oversized"));
    }

    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| ServiceError::upstream(category, &err))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ServiceError::bad_gateway(category, "oversized"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use std::convert::Infallible;

    use axum::{Router, body::Body, response::Response, routing::get};
    use futures_util::stream;
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn stalled_response_hits_idle_read_timeout() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let client = client(Duration::from_secs(1), Duration::from_millis(30)).unwrap();
        let error = checked_response(client.get(format!("http://{address}")).send().await, "test")
            .await
            .unwrap_err();

        assert_eq!(error.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn oversized_stream_without_content_length_is_rejected() {
        let route = Router::new().route(
            "/",
            get(|| async { Response::new(Body::from(vec![b'x'; 33])) }),
        );
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });

        let response = metadata_client()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        let error = read_limited(response, 32, "test").await.unwrap_err();

        assert_eq!(error.status(), axum::http::StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn slow_continuously_streaming_download_stays_active() {
        let route = Router::new().route(
            "/",
            get(|| async {
                let chunks = stream::unfold(0, |index| async move {
                    if index == 5 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    Some((Ok::<_, Infallible>(Bytes::from_static(b"chunk")), index + 1))
                });
                Response::new(Body::from_stream(chunks))
            }),
        );
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, route).await.unwrap();
        });

        let client = client(Duration::from_secs(1), Duration::from_millis(40)).unwrap();
        let response = client
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        let body = response.bytes().await.unwrap();

        assert_eq!(body.len(), 25);
    }
}
