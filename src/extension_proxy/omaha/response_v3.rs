use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header::LOCATION},
    response::Response,
};
use serde_json::{Value, json};
use url::Url;

use crate::error::ServiceError;

use super::{
    ExtensionProxyService, ResponseType, bad_request, handlers, json_response, response_headers,
    text_escape,
};

pub(crate) async fn create_response(
    service: &ExtensionProxyService,
    response_type: ResponseType,
    mut data: Value,
) -> Result<Response, ServiceError> {
    let filtered = filter_response(service, &data).await?;
    data["response"] = filtered;

    match response_type {
        ResponseType::Redirect => handle_redirect(&data),
        ResponseType::Json => json_response(data),
        ResponseType::Xml => handle_xml(&data),
    }
}

async fn filter_response(
    service: &ExtensionProxyService,
    data: &Value,
) -> Result<Value, ServiceError> {
    let response = data
        .get("response")
        .ok_or_else(|| bad_request("invalid response"))?;
    let protocol = response.get("protocol").and_then(Value::as_str);
    if !matches!(protocol, Some("3.0" | "3.1")) {
        return Err(bad_request(
            "trying to pass a non-v3 response through v3 filter",
        ));
    }

    let apps = match response.get("app").and_then(Value::as_array) {
        Some(apps) => {
            let mut filtered = Vec::with_capacity(apps.len());
            for app in apps {
                let mut updatecheck = app.get("updatecheck").cloned();
                if updatecheck
                    .as_ref()
                    .and_then(|uc| uc.get("status"))
                    .and_then(Value::as_str)
                    == Some("ok")
                {
                    let mut url = best_url(updatecheck.as_ref().unwrap())?;
                    if let Some(file_name) = updatecheck
                        .as_ref()
                        .and_then(|uc| uc.pointer("/manifest/packages/package/0/name"))
                        .and_then(Value::as_str)
                    {
                        if !url.path().ends_with('/') {
                            url.set_path(&format!("{}/", url.path()));
                        }
                        let path = format!("{}{}", url.path(), file_name);
                        url.set_path(&path);
                    }

                    let wrapped = service.wrap_url(url.as_str()).await?;
                    if let Some(uc) = updatecheck.as_mut() {
                        uc["urls"]["url"] = json!([{ "codebase": wrapped }]);
                    }
                }

                filtered.push(json!({
                    "appid": app.get("appid").cloned().unwrap_or(Value::Null),
                    "status": app.get("status").cloned().unwrap_or(Value::Null),
                    "cohort": "",
                    "cohortname": "",
                    "cohorthint": "",
                    "ping": app.get("ping").cloned().unwrap_or_else(|| json!({})),
                    "updatecheck": updatecheck,
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
        "app": apps,
    }))
}

fn best_url(updatecheck: &Value) -> Result<Url, ServiceError> {
    let urls = updatecheck
        .pointer("/urls/url")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("could not get any viable URL for download"))?;
    let mut backup = None;
    for url_obj in urls {
        let Some(codebase) = url_obj.get("codebase").and_then(Value::as_str) else {
            continue;
        };
        let url = Url::parse(codebase).map_err(ServiceError::internal)?;
        if url.scheme() == "https" {
            return Ok(url);
        }
        backup = Some(url);
    }
    backup.ok_or_else(|| bad_request("could not get any viable URL for download"))
}

fn handle_redirect(data: &Value) -> Result<Response, ServiceError> {
    if let Some(codebase) = data
        .pointer("/response/app/0/updatecheck/urls/url/0/codebase")
        .and_then(Value::as_str)
    {
        let mut response = Response::new(Body::from("Found"));
        *response.status_mut() = StatusCode::FOUND;
        response.headers_mut().insert(
            LOCATION,
            HeaderValue::from_str(codebase).map_err(ServiceError::internal)?,
        );
        return Ok(response);
    }

    Ok(handlers::text_response(
        StatusCode::NOT_FOUND,
        "Not Found",
        "text/plain",
    ))
}

fn handle_xml(data: &Value) -> Result<Response, ServiceError> {
    let response = data
        .get("response")
        .ok_or_else(|| bad_request("invalid response"))?;
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><gupdate xmlns="http://www.google.com/update2/response" protocol="2.0""#,
    );
    if let Some(server) = response.get("server").and_then(Value::as_str) {
        xml.push_str(&format!(r#" server="{}""#, text_escape(server)));
    }
    xml.push('>');

    let elapsed_days = response
        .pointer("/daystart/elapsed_days")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let elapsed_seconds = response
        .pointer("/daystart/elapsed_seconds")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    xml.push_str(&format!(
        r#"<daystart elapsed_days="{elapsed_days}" elapsed_seconds="{elapsed_seconds}"/>"#
    ));

    if let Some(apps) = response.get("app").and_then(Value::as_array) {
        for app in apps {
            let appid = app.get("appid").and_then(Value::as_str).unwrap_or_default();
            let status = app
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            xml.push_str(&format!(
                r#"<app appid="{}" status="{}""#,
                text_escape(appid),
                text_escape(status)
            ));
            if app
                .get("cohort")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
            {
                xml.push_str(r#" cohort="" cohortname="""#);
            }
            xml.push('>');

            if let Some(updatecheck) = app.get("updatecheck") {
                append_updatecheck_xml(&mut xml, updatecheck)?;
            }
            xml.push_str("</app>");
        }
    }

    xml.push_str("</gupdate>");

    let mut http_response =
        handlers::text_response(StatusCode::OK, xml, "application/xml; charset=utf-8");
    response_headers(&mut http_response, response)?;
    Ok(http_response)
}

fn append_updatecheck_xml(xml: &mut String, updatecheck: &Value) -> Result<(), ServiceError> {
    if updatecheck.get("status").and_then(Value::as_str) == Some("ok") {
        let codebase = updatecheck
            .pointer("/urls/url/0/codebase")
            .and_then(Value::as_str)
            .ok_or_else(|| bad_request("differential updates are not supported here"))?;
        let package = updatecheck
            .pointer("/manifest/packages/package/0")
            .ok_or_else(|| bad_request("differential updates are not supported here"))?;
        let version = updatecheck
            .pointer("/manifest/version")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let hash = package
            .get("hash_sha256")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let size = package
            .get("size")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        xml.push_str(&format!(
            r#"<updatecheck status="ok" fp="" hash_sha256="{}" size="{size}" protected="0" version="{}" codebase="{}"/>"#,
            text_escape(hash),
            text_escape(version),
            text_escape(codebase)
        ));
    } else if let Some(object) = updatecheck.as_object() {
        xml.push_str("<updatecheck");
        for (key, value) in object {
            let value = value
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| value.to_string());
            xml.push_str(&format!(r#" {}="{}""#, key, text_escape(&value)));
        }
        xml.push_str("/>");
    }
    Ok(())
}
