use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    http::{HeaderValue, Method, Request, StatusCode},
    response::Response,
};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::ServiceError;

use super::{ExtensionProxyService, bad_request, handlers, is_valid_app_id, status_error};

mod chromium_version;
mod request;
mod response_v3;
mod response_v4;

const MAX_EXTENSIONS_PER_REQUEST: usize = 100;
const CHROME_COMPONENTS_CRXSET: &str = "hfnkpimlhhgieaddgfemjhofmfblmnib";
pub(crate) const OMAHA_JSON_PREFIX: &str = ")]}'";

#[derive(Clone, Default)]
pub(crate) struct OmahaState {
    chromium_versions: chromium_version::ChromiumVersionCache,
    mixins: Arc<Mutex<HashMap<ServiceId, MixinPool>>>,
}

#[derive(Clone, Default)]
struct MixinPool {
    apps: Vec<App>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct App {
    appid: String,
    version: String,
    updatecheck: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ServiceId {
    ChromeWebstore,
    ChromeComponents,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolVersion {
    V3,
    V4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseType {
    Json,
    Xml,
    Redirect,
}

struct RequestData {
    apps: Vec<App>,
    protocol: ProtocolVersion,
    response_type: ResponseType,
}

pub(crate) async fn handle_omaha_query(
    service: ExtensionProxyService,
    request: Request<Body>,
) -> Result<Response, ServiceError> {
    let service_id = get_service_id(request.uri().path());
    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let data = get_data(request).await?;
    let filtered_apps = check_and_filter_apps(service_id, data.apps)?;
    if filtered_apps.is_empty() {
        return Err(bad_request("no allowed extension IDs left to fetch"));
    }

    let mut apps_with_mixins = add_random_extensions(&service, service_id, &filtered_apps).await;
    fastrand::shuffle(&mut apps_with_mixins);

    let mut omaha_response = request::request(
        &service,
        service_id,
        data.protocol,
        &apps_with_mixins,
        &user_agent,
    )
    .await?;

    add_to_pool_from_response(&service, service_id, &omaha_response).await;
    unmix_response(&filtered_apps, &mut omaha_response)?;

    match data.protocol {
        ProtocolVersion::V3 => {
            response_v3::create_response(&service, data.response_type, omaha_response).await
        }
        ProtocolVersion::V4 => {
            response_v4::create_response(&service, data.response_type, omaha_response).await
        }
    }
}

fn get_service_id(path: &str) -> ServiceId {
    if path == "/com" {
        ServiceId::ChromeComponents
    } else {
        ServiceId::ChromeWebstore
    }
}

async fn get_data(request: Request<Body>) -> Result<RequestData, ServiceError> {
    match *request.method() {
        Method::GET => handle_get(request.uri()),
        Method::POST => handle_post(request).await,
        _ => Err(status_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        )),
    }
}

fn handle_get(uri: &axum::http::Uri) -> Result<RequestData, ServiceError> {
    let query = uri.query().unwrap_or_default();
    let response_type = if url::form_urlencoded::parse(query.as_bytes())
        .any(|(key, value)| key == "response" && value == "redirect")
    {
        ResponseType::Redirect
    } else {
        ResponseType::Xml
    };

    let x_params = url::form_urlencoded::parse(query.as_bytes())
        .filter_map(|(key, value)| (key == "x").then(|| value.into_owned()))
        .collect::<Vec<_>>();
    if x_params.is_empty() || (x_params.len() > 1 && response_type == ResponseType::Redirect) {
        return Err(bad_request("malformed request"));
    }

    Ok(RequestData {
        response_type,
        protocol: ProtocolVersion::V3,
        apps: get_apps_from_query(&x_params)?,
    })
}

fn get_apps_from_query(params: &[String]) -> Result<Vec<App>, ServiceError> {
    params
        .iter()
        .map(|param| {
            let pairs = url::form_urlencoded::parse(param.as_bytes()).collect::<Vec<_>>();
            let has_uc = pairs.iter().any(|(key, _)| key == "uc");
            let appid = pairs
                .iter()
                .find_map(|(key, value)| (key == "id").then(|| value.to_string()))
                .ok_or_else(|| bad_request("invalid x string"))?;
            if !has_uc {
                return Err(bad_request("invalid x string"));
            }
            let version = pairs
                .iter()
                .find_map(|(key, value)| (key == "v").then(|| value.to_string()))
                .unwrap_or_else(|| "0.0.0.0".to_string());

            Ok(App {
                appid,
                version,
                updatecheck: None,
            })
        })
        .collect()
}

async fn handle_post(request: Request<Body>) -> Result<RequestData, ServiceError> {
    if request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        != Some("application/json")
    {
        return Err(status_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid content-type",
        ));
    }

    let body = handlers::read_body(request.into_body()).await?;
    let body: Value = serde_json::from_slice(&body).map_err(|_| bad_request("invalid body"))?;
    let request = body
        .get("request")
        .ok_or_else(|| bad_request("invalid body"))?;
    let protocol_str = request
        .get("protocol")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("unknown omaha protocol version: \"\""))?;
    let protocol = if protocol_str.starts_with("3.") {
        ProtocolVersion::V3
    } else if protocol_str.starts_with("4.") {
        ProtocolVersion::V4
    } else {
        return Err(bad_request(format!(
            "unknown omaha protocol version: \"{protocol_str}\""
        )));
    };

    Ok(RequestData {
        response_type: ResponseType::Json,
        protocol,
        apps: get_apps_for_protocol(request, protocol)?,
    })
}

fn get_apps_for_protocol(
    request: &Value,
    protocol: ProtocolVersion,
) -> Result<Vec<App>, ServiceError> {
    let key = match protocol {
        ProtocolVersion::V3 => "app",
        ProtocolVersion::V4 => "apps",
    };
    let apps = request
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("malformed request"))?;

    apps.iter()
        .map(|app| {
            let appid = app
                .get("appid")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_request("malformed request"))?;
            let version = app
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_request("malformed request"))?;
            Ok(App {
                appid: appid.to_string(),
                version: version.to_string(),
                updatecheck: app.get("updatecheck").cloned(),
            })
        })
        .collect()
}

fn check_and_filter_apps(service_id: ServiceId, apps: Vec<App>) -> Result<Vec<App>, ServiceError> {
    let mut ids = std::collections::HashSet::new();
    let mut filtered = Vec::new();
    for app in apps {
        if !ids.insert(app.appid.clone()) {
            return Err(bad_request(format!(
                "duplicates not allowed -- {}",
                app.appid
            )));
        }
        if !is_valid_app_id(&app.appid) {
            return Err(bad_request(format!("invalid app id -- {}", app.appid)));
        }
        if app.version.is_empty() || app.version.len() > 16 {
            return Err(bad_request(format!("invalid version -- {}", app.version)));
        }

        if service_id == ServiceId::ChromeWebstore || app.appid == CHROME_COMPONENTS_CRXSET {
            filtered.push(app);
        }
    }
    Ok(filtered)
}

async fn add_random_extensions(
    service: &ExtensionProxyService,
    service_id: ServiceId,
    apps: &[App],
) -> Vec<App> {
    let n_apps_to_mixin = ((apps.len() as f64).log2() + 1.0).ceil() as usize * 2;
    let id_set = apps
        .iter()
        .map(|app| app.appid.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut mixins = {
        let pools = service.omaha.mixins.lock().await;
        pools
            .get(&service_id)
            .map(|pool| {
                pool.apps
                    .iter()
                    .filter(|app| !id_set.contains(app.appid.as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    fastrand::shuffle(&mut mixins);
    mixins.truncate(n_apps_to_mixin);

    apps.iter().cloned().chain(mixins).collect()
}

async fn add_to_pool(service: &ExtensionProxyService, service_id: ServiceId, app: App) {
    const MAX_EXTENSIONS_IN_POOL: usize = 1024;

    let mut pools = service.omaha.mixins.lock().await;
    let pool = pools.entry(service_id).or_default();
    if !pool
        .apps
        .iter()
        .any(|entry| entry.appid == app.appid && entry.version == app.version)
    {
        pool.apps.insert(0, app);
    }
    if pool.apps.len() > MAX_EXTENSIONS_IN_POOL {
        pool.apps.truncate(MAX_EXTENSIONS_IN_POOL);
    }
}

async fn add_to_pool_from_response(
    service: &ExtensionProxyService,
    service_id: ServiceId,
    response: &Value,
) {
    let Some(inner) = response.get("response") else {
        return;
    };
    if inner.get("protocol").and_then(Value::as_str) == Some("4.0") {
        if let Some(apps) = inner.get("apps").and_then(Value::as_array) {
            for app in apps {
                let Some(updatecheck) = app.get("updatecheck") else {
                    continue;
                };
                if updatecheck.get("status").and_then(Value::as_str) == Some("ok")
                    && let (Some(appid), Some(version)) = (
                        app.get("appid").and_then(Value::as_str),
                        updatecheck.get("nextversion").and_then(Value::as_str),
                    )
                {
                    add_to_pool(
                        service,
                        service_id,
                        App {
                            appid: appid.to_string(),
                            version: version.to_string(),
                            updatecheck: None,
                        },
                    )
                    .await;
                }
            }
        }
    } else if let Some(apps) = inner.get("app").and_then(Value::as_array) {
        for app in apps {
            let version = app
                .pointer("/updatecheck/manifest/version")
                .and_then(Value::as_str);
            if app.pointer("/updatecheck/status").and_then(Value::as_str) == Some("ok")
                && let (Some(appid), Some(version)) =
                    (app.get("appid").and_then(Value::as_str), version)
            {
                add_to_pool(
                    service,
                    service_id,
                    App {
                        appid: appid.to_string(),
                        version: version.to_string(),
                        updatecheck: None,
                    },
                )
                .await;
            }
        }
    }
}

fn unmix_response(expected_apps: &[App], response: &mut Value) -> Result<(), ServiceError> {
    let expected = expected_apps
        .iter()
        .map(|app| app.appid.as_str())
        .collect::<std::collections::HashSet<_>>();
    let inner = response
        .get_mut("response")
        .ok_or_else(|| bad_request("invalid response"))?;
    if inner.get("protocol").and_then(Value::as_str) == Some("4.0") {
        if let Some(apps) = inner.get_mut("apps").and_then(Value::as_array_mut) {
            apps.retain(|app| {
                app.get("appid")
                    .and_then(Value::as_str)
                    .is_some_and(|appid| expected.contains(appid))
            });
        }
    } else if let Some(apps) = inner.get_mut("app").and_then(Value::as_array_mut) {
        apps.retain(|app| {
            app.get("appid")
                .and_then(Value::as_str)
                .is_some_and(|appid| expected.contains(appid))
        });
    } else {
        return Err(bad_request("unknown protocol to unmix"));
    }
    Ok(())
}

fn response_headers(response: &mut Response, inner: &Value) -> Result<(), ServiceError> {
    let elapsed_days = inner
        .pointer("/daystart/elapsed_days")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .to_string();
    let elapsed_seconds = inner
        .pointer("/daystart/elapsed_seconds")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .to_string();
    let headers = response.headers_mut();
    headers.insert(
        "x-daynum",
        HeaderValue::from_str(&elapsed_days).map_err(ServiceError::internal)?,
    );
    headers.insert(
        "x-daystart",
        HeaderValue::from_str(&elapsed_seconds).map_err(ServiceError::internal)?,
    );
    headers.insert(
        "cache-control",
        HeaderValue::from_static("no-cache, no-store, max-age=0, must-revalidate"),
    );
    headers.insert("accept-ranges", HeaderValue::from_static("none"));
    headers.insert("pragma", HeaderValue::from_static("no-cache"));
    Ok(())
}

fn json_response(mut data: Value) -> Result<Response, ServiceError> {
    let body = format!(
        "{}{}",
        OMAHA_JSON_PREFIX,
        serde_json::to_string(&data).map_err(ServiceError::internal)?
    );
    let inner = data
        .get_mut("response")
        .ok_or_else(|| bad_request("invalid response"))?;
    let mut response =
        handlers::text_response(StatusCode::OK, body, "application/json; charset=utf-8");
    response_headers(&mut response, inner)?;
    Ok(response)
}

fn text_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_apps_from_query_defaults_missing_version() {
        let apps =
            get_apps_from_query(&["id=abcdefghijklmnopabcdefghijklmnop&uc".to_string()]).unwrap();
        assert_eq!(apps[0].version, "0.0.0.0");
    }

    #[test]
    fn components_filter_allows_only_crlset() {
        let apps = vec![
            App {
                appid: CHROME_COMPONENTS_CRXSET.to_string(),
                version: "1".to_string(),
                updatecheck: None,
            },
            App {
                appid: "abcdefghijklmnopabcdefghijklmnop".to_string(),
                version: "1".to_string(),
                updatecheck: None,
            },
        ];

        let filtered = check_and_filter_apps(ServiceId::ChromeComponents, apps).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].appid, CHROME_COMPONENTS_CRXSET);
    }
}
