use axum::{http::StatusCode, response::Response};
use serde_json::{Value, json};
use url::Url;

use crate::error::ServiceError;

use super::{ExtensionProxyService, ResponseType, bad_request, json_response};

pub(crate) async fn create_response(
    service: &ExtensionProxyService,
    response_type: ResponseType,
    mut data: Value,
) -> Result<Response, ServiceError> {
    data["response"] = filter_response(service, &data).await?;

    if response_type == ResponseType::Json {
        return json_response(data);
    }

    Err(bad_request(format!(
        "unsupported response type for omaha v4: {response_type:?}"
    )))
}

async fn filter_response(
    service: &ExtensionProxyService,
    data: &Value,
) -> Result<Value, ServiceError> {
    let response = data
        .get("response")
        .ok_or_else(|| bad_request("invalid response"))?;
    if response.get("protocol").and_then(Value::as_str) != Some("4.0") {
        return Err(bad_request(
            "trying to pass a non-v4 response through a v4 filter",
        ));
    }

    let apps = match response.get("apps").and_then(Value::as_array) {
        Some(apps) => {
            let mut filtered = Vec::with_capacity(apps.len());
            for app in apps {
                filtered.push(json!({
                    "appid": app.get("appid").cloned().unwrap_or(Value::Null),
                    "status": app.get("status").cloned().unwrap_or(Value::Null),
                    "cohort": "",
                    "cohortname": "",
                    "ping": {
                        "status": app.pointer("/ping/status").cloned().unwrap_or(Value::Null),
                    },
                    "updatecheck": filter_updatecheck(
                        service,
                        app.get("updatecheck").ok_or_else(|| bad_request("invalid response"))?,
                    ).await?,
                }));
            }
            Value::Array(filtered)
        }
        None => Value::Null,
    };

    Ok(json!({
        "server": response.get("server").cloned().unwrap_or(Value::Null),
        "protocol": response.get("protocol").cloned().unwrap_or(Value::Null),
        "daystart": response.get("daystart").cloned().unwrap_or_else(|| json!({})),
        "apps": apps,
    }))
}

async fn filter_updatecheck(
    service: &ExtensionProxyService,
    updatecheck: &Value,
) -> Result<Value, ServiceError> {
    if updatecheck.get("status").and_then(Value::as_str) != Some("ok") {
        return Ok(json!({ "status": "noupdate" }));
    }

    let pipelines = updatecheck
        .get("pipelines")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("updatecheck(v4): too many pipelines"))?;
    if pipelines.len() > 1 {
        return Err(bad_request("updatecheck(v4): too many pipelines"));
    }

    let pipeline = pipelines
        .first()
        .ok_or_else(|| bad_request("updatecheck(v4): too many pipelines"))?;
    let operations = pipeline
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("updatecheck(v4): unsupported ops"))?;
    if operations.iter().any(|op| {
        !matches!(
            op.get("type").and_then(Value::as_str),
            Some("download" | "crx3")
        )
    }) {
        return Err(bad_request("updatecheck(v4): unsupported ops"));
    }

    let mut filtered_ops = Vec::new();
    for op in operations
        .iter()
        .filter(|op| op.get("type").and_then(Value::as_str) == Some("download"))
    {
        if let Some(url) = best_url(updatecheck)? {
            let wrapped = service.wrap_url(url.as_str()).await?;
            filtered_ops.push(json!({
                "type": "download",
                "size": op.get("size").cloned().unwrap_or(Value::Null),
                "out": {
                    "sha256": op.pointer("/out/sha256").cloned().unwrap_or(Value::Null),
                },
                "urls": [{ "url": wrapped }],
            }));
        }
    }

    for op in operations
        .iter()
        .filter(|op| op.get("type").and_then(Value::as_str) == Some("crx3"))
    {
        filtered_ops.push(json!({
            "type": "crx3",
            "in": {
                "sha256": op.pointer("/in/sha256").cloned().unwrap_or(Value::Null),
            },
        }));
    }

    Ok(json!({
        "status": "ok",
        "nextversion": updatecheck.get("nextversion").cloned().unwrap_or(Value::Null),
        "pipelines": [{
            "pipeline_id": pipeline.get("pipeline_id").cloned().unwrap_or(Value::Null),
            "operations": filtered_ops,
        }],
    }))
}

fn best_url(updatecheck: &Value) -> Result<Option<Url>, ServiceError> {
    let Some(pipeline) = updatecheck
        .get("pipelines")
        .and_then(Value::as_array)
        .and_then(|pipelines| pipelines.first())
    else {
        return Ok(None);
    };
    let Some(download_op) = pipeline
        .get("operations")
        .and_then(Value::as_array)
        .and_then(|ops| {
            ops.iter()
                .find(|op| op.get("type").and_then(Value::as_str) == Some("download"))
        })
    else {
        return Ok(None);
    };

    let mut backup = None;
    if let Some(urls) = download_op.get("urls").and_then(Value::as_array) {
        for url_obj in urls {
            let Some(url_str) = url_obj.get("url").and_then(Value::as_str) else {
                continue;
            };
            let url = Url::parse(url_str).map_err(ServiceError::internal)?;
            if url.scheme() == "https" {
                return Ok(Some(url));
            }
            backup = Some(url);
        }
    }

    Ok(backup)
}

#[allow(dead_code)]
fn _status_code(_: StatusCode) {}
