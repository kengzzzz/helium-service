use serde_json::{Value, json};

use crate::error::ServiceError;

use super::{
    App, ExtensionProxyService, MAX_EXTENSIONS_PER_REQUEST, OMAHA_JSON_PREFIX, ProtocolVersion,
    ServiceId, bad_request,
};

const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/json";

pub(crate) async fn request(
    service: &ExtensionProxyService,
    service_id: ServiceId,
    protocol: ProtocolVersion,
    apps: &[App],
    user_agent: &str,
) -> Result<Value, ServiceError> {
    if apps.len() > MAX_EXTENSIONS_PER_REQUEST {
        return Err(bad_request("too many apps in a single request"));
    }

    let app_ids = apps
        .iter()
        .map(|app| app.appid.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let browser_version = service
        .omaha
        .chromium_versions
        .random_version(&service.client)
        .await?;
    let body = craft_request(service, apps, protocol, &browser_version).await?;

    let response = service
        .client
        .post(UPDATE_SERVICE)
        .header("user-agent", user_agent)
        .header("content-type", "application/json")
        .header("priority", "u=4, i")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "no-cors")
        .header("sec-fetch-site", "none")
        .header("x-goog-update-appid", app_ids)
        .header(
            "x-goog-update-interactivity",
            if service_id == ServiceId::ChromeComponents {
                "fg"
            } else {
                "bg"
            },
        )
        .header("x-goog-update-updater", format!("chrome-{browser_version}"))
        .body(serde_json::to_string(&body).map_err(ServiceError::internal)?)
        .send()
        .await
        .map_err(ServiceError::internal)?;

    if !response.status().is_success() {
        return Err(ServiceError::internal("response is not ok"));
    }

    let text = response.text().await.map_err(ServiceError::internal)?;
    if !text.starts_with(OMAHA_JSON_PREFIX) {
        return Err(ServiceError::internal("invalid response"));
    }
    serde_json::from_str(&text[OMAHA_JSON_PREFIX.len()..]).map_err(ServiceError::internal)
}

async fn craft_request(
    service: &ExtensionProxyService,
    apps: &[App],
    protocol: ProtocolVersion,
    browser_version: &str,
) -> Result<Value, ServiceError> {
    let mut request = match protocol {
        ProtocolVersion::V3 => v3_template(browser_version),
        ProtocolVersion::V4 => v4_template(browser_version),
    };

    let physmemory = [4, 8, 16][fastrand::usize(..3)];
    request["request"]["hw"]["physmemory"] = json!(physmemory);
    request["request"]["requestid"] = json!(format!("{{{}}}", uuid::Uuid::new_v4()));
    request["request"]["sessionid"] = json!(format!("{{{}}}", uuid::Uuid::new_v4()));

    match protocol {
        ProtocolVersion::V3 => {
            request["request"]["app"] = Value::Array(
                apps.iter()
                    .map(|app| {
                        json!({
                            "appid": app.appid,
                            "version": app.version,
                            "enabled": true,
                            "installedby": "internal",
                            "installsource": "ondemand",
                            "lang": "",
                            "packages": { "package": [{ "fp": format!("2.{}", app.version) }] },
                            "ping": { "r": -1 },
                            "updatecheck": app.updatecheck.clone().unwrap_or_else(|| json!({})),
                        })
                    })
                    .collect(),
            );
        }
        ProtocolVersion::V4 => {
            request["request"]["apps"] = Value::Array(
                apps.iter()
                    .map(|app| {
                        json!({
                            "appid": app.appid,
                            "version": app.version,
                            "enabled": true,
                            "installsource": "ondemand",
                            "lang": "",
                            "ping": { "r": -2 },
                            "updatecheck": app.updatecheck.clone().unwrap_or_else(|| json!({})),
                        })
                    })
                    .collect(),
            );
        }
    }

    let _ = service;
    Ok(request)
}

fn v3_template(browser_version: &str) -> Value {
    json!({
        "request": {
            "@os": "win",
            "@updater": "chromecrx",
            "acceptformat": "crx3,puff",
            "app": [],
            "arch": "x86",
            "dedup": "cr",
            "domainjoined": false,
            "hw": {
                "avx": false,
                "physmemory": 4,
                "sse": false,
                "sse2": false,
                "sse3": false,
                "sse41": false,
                "sse42": false,
                "ssse3": false
            },
            "ismachine": false,
            "nacl_arch": "x86-64",
            "os": {
                "arch": "x86_64",
                "platform": "Windows",
                "version": "10.0.26100.3476"
            },
            "updater": {
                "name": "chromecrx",
                "ismachine": false,
                "autoupdatecheckenabled": true,
                "updatepolicy": 0,
                "version": browser_version
            },
            "prodversion": browser_version,
            "protocol": "3.1",
            "requestid": "",
            "sessionid": "",
            "updaterversion": browser_version,
            "wow64": true
        }
    })
}

fn v4_template(browser_version: &str) -> Value {
    json!({
        "request": {
            "@os": "win",
            "@updater": "chrome",
            "acceptformat": "crx3,download,puff,run,xz,zucc",
            "apps": [],
            "arch": "x86_64",
            "dedup": "cr",
            "domainjoined": false,
            "hw": {
                "avx": false,
                "physmemory": 16,
                "sse": false,
                "sse2": false,
                "sse3": false,
                "sse41": false,
                "sse42": false,
                "ssse3": false
            },
            "ismachine": false,
            "os": {
                "arch": "x86_64",
                "platform": "Windows",
                "version": "10.0.26100.3476"
            },
            "prodversion": browser_version,
            "protocol": "4.0",
            "requestid": "",
            "sessionid": "",
            "updaterversion": "0"
        }
    })
}
