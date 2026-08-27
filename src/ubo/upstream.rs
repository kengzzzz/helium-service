use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::{
    error::ServiceError,
    upstream::{checked_response, read_limited},
};

const FILTER_LIST_LIMIT: usize = 32 * 1024 * 1024;

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
                let response = checked_response(client.get(url).send().await, "ubo_filter").await?;
                let body = read_limited(response, FILTER_LIST_LIMIT, "ubo_filter").await?;
                String::from_utf8(body.to_vec())
                    .map_err(|_| ServiceError::bad_gateway("ubo_filter", "invalid_text"))
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
