use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::error::ServiceError;

pub(super) async fn shotgun_fetch(
    client: &reqwest::Client,
    urls: &[String],
) -> Result<String, ServiceError> {
    let mut requests = urls
        .iter()
        .cloned()
        .map(|url| {
            let client = client.clone();
            async move {
                let response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(ServiceError::internal)?;
                if response.status() == reqwest::StatusCode::OK {
                    response.text().await.map_err(ServiceError::internal)
                } else {
                    Err(ServiceError::internal(format!(
                        "unexpected status: {}",
                        response.status()
                    )))
                }
            }
        })
        .collect::<FuturesUnordered<_>>();

    let mut last_error = None;
    while let Some(result) = requests.next().await {
        match result {
            Ok(text) => return Ok(text),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| ServiceError::internal("no source urls")))
}
